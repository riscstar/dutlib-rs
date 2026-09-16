use std::{
    collections::BTreeMap,
    io::{self, Write},
    thread::{self},
    time::{Duration, Instant},
};

use statrs::statistics::Statistics as StatrsStatistics;

use crate::{
    CommandExecutor, Config, UnrecoverableError,
    ethtool::{self, Statistics},
    ip,
    native::SudoExecutor,
    rtc_testbench::{self, TrafficClass, TrafficContext, TrafficStats},
    tests::DriverInfo,
    tracker::UndoTracker,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum QdiscUseCase {
    Taprio,
    Mqprio,
    MqprioAndCbs,
    MqprioAndEtf,
    MqprioAndCbsWith5Queues,
    MqprioAndEtfWith5Queues,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtpMode {
    Enabled,
    Disabled,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XdpMode {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum PtpRole {
    Master,
    Slave,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CbsConfig {
    pub idle_slope: i64, // Bandwidth allocation in kbit/s
    pub send_slope: i64, // Credit depletion rate in kbit/s (always negative)
    pub hi_credit: i32,  // Upper credit boundary in bytes
    pub lo_credit: i32,  // Lower credit boundary in bytes (always negative)
}

#[derive(Debug, PartialEq, Eq)]
pub enum CbsError {
    ZeroLinkSpeed,
    ReservedBwExceedsCapacity,
}

impl CbsConfig {
    pub fn new(idle_slope: i64, send_slope: i64, hi_credit: i32, lo_credit: i32) -> Self {
        Self {
            idle_slope,
            send_slope,
            hi_credit,
            lo_credit,
        }
    }

    /// Calculates IEEE 802.1Qav CBS parameters
    ///
    /// # Arguments
    /// * `link_speed_kbps` - Total physical link rate in kbit/s (e.g., 1_000_000 for 1 Gbps)
    /// * `reserved_bw_kbps` - CBS reserved bandwidth in kbit/s (e.g., 100_000 for 100 Mbps)
    /// * `cbs_mtu_bytes` - MTU payload size for the target CBS traffic class (e.g., 1500)
    /// * `int_mtu_bytes` - MTU payload size for interfering traffic (e.g., 1500)
    pub fn new_with_bandwidth_and_mtu(
        link_speed_kbps: u64,
        reserved_bw_kbps: u64,
        cbs_mtu_bytes: u32,
        int_mtu_bytes: u32,
    ) -> Option<CbsConfig> {
        const ETHERNET_FRAME_OVERHEAD_BYTES: u32 = 42;

        if link_speed_kbps == 0 || reserved_bw_kbps >= link_speed_kbps {
            return None;
        }

        // Account for physical framing overheads
        let s_cbs = (cbs_mtu_bytes + ETHERNET_FRAME_OVERHEAD_BYTES) as f64;
        let s_int = (int_mtu_bytes + ETHERNET_FRAME_OVERHEAD_BYTES) as f64;

        let c = link_speed_kbps as f64;
        let r = reserved_bw_kbps as f64;

        let idle_slope = reserved_bw_kbps as i64;
        let send_slope = idle_slope - (link_speed_kbps as i64);

        // hi_credit = S_int * (idle_slope / C) — rounded up to prevent latency starvation
        let hi_credit = (s_int * (r / c)).ceil() as i32;

        // lo_credit = S_cbs * (send_slope / C) — rounded down (more negative) for safe allocation
        let lo_credit = (s_cbs * (send_slope as f64 / c)).floor() as i32;

        Some(CbsConfig::new(idle_slope, send_slope, hi_credit, lo_credit))
    }

    pub fn new_with_bandwidth(link_speed_kbps: u64, reserved_bw_kbps: u64) -> Option<CbsConfig> {
        Self::new_with_bandwidth_and_mtu(link_speed_kbps, reserved_bw_kbps, 1500, 1500)
    }
}

fn traffic_context(
    config: &Config,
    shell: &mut impl CommandExecutor,
    partner: &mut impl CommandExecutor,
) -> Result<TrafficContext, UnrecoverableError> {
    let adapter = &config.adapter;
    let mac_addr = ip::mac_address(shell, adapter)?;
    let ip_addr = ip::ipv4_address(shell, adapter)?;

    let partner_adapter = &config.partner_adapter;
    let partner_mac_addr = ip::mac_address(partner, partner_adapter)?;

    let context = TrafficContext {
        reference_interface: config.adapter.clone(),
        reference_ip: ip_addr,
        reference_mac: mac_addr,
        mirror_interface: config.partner_adapter.clone(),
        mirror_ip: config.ipaddr.clone(),
        mirror_mac: partner_mac_addr,
    };

    log::debug!("{:?}", context);

    Ok(context)
}

struct BenchmarkResult {
    rtc_stats_shell: Option<BTreeMap<String, TrafficStats>>,
    eth_stats_shell: Statistics,
    rtc_stats_partner: Option<BTreeMap<String, TrafficStats>>,
    eth_stats_partner: Statistics,
}

impl BenchmarkResult {
    pub fn log(&self) {
        rtc_testbench::log_traffic_stats(&self.rtc_stats_shell);
        rtc_testbench::log_traffic_stats(&self.rtc_stats_partner);
        log::info!("DUT packet counts: {:?}", self.eth_stats_shell);
        log::info!("LP packet counts: {:?}", self.eth_stats_partner);
    }
}

fn run_rtc_benchmark(
    config: &Config,
    shell: &mut impl CommandExecutor,
    partner: &mut SudoExecutor,
    competitive_egress: bool,
) -> Result<BenchmarkResult, UnrecoverableError> {
    const DURATION_SECS: u64 = 60;

    let startup_delay = 30;
    let mirror_duration = DURATION_SECS + startup_delay + 5;
    let reference_duration = DURATION_SECS + startup_delay;
    let timeout = reference_duration + 30;

    log::info!("Running RTC-Testbench (this will take {mirror_duration} seconds)");

    // Make sure we don't read results from stale log files!
    shell.cmd("rm reference.log")?;
    partner.cmd("rm mirror.log")?;

    if competitive_egress {
        shell.cmd(format!(
            "iperf3 -c {} -u -b 2G -i 0 -t {} & sleep 1",
            config.ipaddr,
            reference_duration - 1
        ))?;
    }

    let before_partner = ethtool::statistics(partner, &config.partner_adapter)?;
    let before_shell = ethtool::statistics(shell, &config.adapter)?;

    let (reference_result, mirror_result) = thread::scope(|s| {
        let mirror = s.spawn(|| {
            partner.with_timeout_secs(timeout, |sh| {
                sh.cmd(format!("timeout {mirror_duration} mirror -c mirror.yaml"))
            })
        });

        let reference = shell.with_timeout_secs(timeout, |sh| {
            sh.cmd(format!(
                "timeout {reference_duration} reference -c reference.yaml"
            ))
        });

        (reference, mirror.join().unwrap())
    });

    reference_result?;
    mirror_result?;

    let eth_stats_shell = ethtool::statistics(shell, &config.adapter)? - before_shell;
    let eth_stats_partner = ethtool::statistics(partner, &config.partner_adapter)? - before_partner;
    let rtc_stats_shell = rtc_testbench::parse_log_line(shell.cmd("tail -1 reference.log")?);
    let rtc_stats_partner = rtc_testbench::parse_log_line(partner.cmd("tail -1 mirror.log")?);

    Ok(BenchmarkResult {
        rtc_stats_shell,
        eth_stats_shell,
        rtc_stats_partner,
        eth_stats_partner,
    })
}

fn load_kernel_modules(shell: &mut impl CommandExecutor) -> Result<(), UnrecoverableError> {
    shell.cmd("modprobe sch_taprio")?;
    shell.cmd("modprobe sch_mqprio")?;
    shell.cmd("modprobe sch_etf")?;

    Ok(())
}

fn setup_threaded_napi(
    shell: &mut impl CommandExecutor,
    interface: impl AsRef<str>,
    tracker: &mut UndoTracker,
) -> Result<(), UnrecoverableError> {
    let interface = interface.as_ref();

    tracker.sysfs(shell, format!("/sys/class/net/{interface}/threaded"), "1")?;

    let pid_list = shell.cmd(format!(
        "ps aux | grep napi | grep {interface} | awk '{{ print $2; }}'"
    ))?;

    for pid in pid_list.split_whitespace() {
        shell.cmd(format!("chrt -p -f 85 {pid}"))?;
        // No undo needed (assuming NAPI is not threaded *before* we are called
        // then the NAPI threads are will be destroyed when threading disabled)
    }

    Ok(())
}

fn napi_defer_hard_irqs(
    shell: &mut impl CommandExecutor,
    interface: impl AsRef<str>,
    cycle_time_ns: u64,
    tracker: &mut UndoTracker,
) -> Result<(), UnrecoverableError> {
    let interface = interface.as_ref();

    tracker.sysfs(
        shell,
        format!("/sys/class/net/{interface}/napi_defer_hard_irqs"),
        "10",
    )?;
    tracker.sysfs(
        shell,
        format!("/sys/class/net/{interface}/gro_flush_timeout"),
        format!("{}", cycle_time_ns / 10),
    )?;

    Ok(())
}

fn boost_irq_threads(
    shell: &mut impl CommandExecutor,
    interface: impl AsRef<str>,
    tracker: &mut UndoTracker,
) -> Result<(), UnrecoverableError> {
    let interface = interface.as_ref();

    // let the updated threads fire up
    thread::sleep(Duration::from_secs(3));

    let pid_list = shell.cmd(format!(
        "ps aux | grep irq | grep {interface} | awk '{{ print $2; }}'"
    ))?;

    for pid in pid_list.split_whitespace() {
        shell.cmd(format!("chrt -p -f 85 {pid}"))?;
        tracker.add(format!("chrt -p -f 50 {pid}"));
    }

    Ok(())
}

#[rustfmt::skip]
fn igc_setup(
    shell: &mut impl CommandExecutor,
    interface: impl AsRef<str>,
    cycle_time_ns: u64,
    use_case: QdiscUseCase,
) -> Result<UndoTracker, UnrecoverableError> {
    let interface = interface.as_ref();
    let mut tracker = UndoTracker::new();

    load_kernel_modules(shell)?;
    setup_threaded_napi(shell, interface, &mut tracker)?;

    shell.cmd(format!("ethtool -K {interface} rx-vlan-offload off"))?;
    tracker.add(format!("ethtool -K {interface} rx-vlan-offload on"));

    shell.cmd(format!(
        "ethtool -s {interface} speed 1000 autoneg on duplex full",
    ))?;
    tracker.add(format!("ethtool -s {interface} autoneg on"));

    // RTC-Testbench scripts set `ethtool -L {interface} combined 4` here but
    // that's already the default so we don't bother!

    // In principle setting base-time to zero will be projected forward in
    // time and give the same base-time across everything sharing the same
    // PTP clock.
    let base_time = 0;

    // TODO:igc currently uses hard coded cycle times and only supports taprio
    if cycle_time_ns != 1000000 {
        return Err(UnrecoverableError::message("unsupported cycle time"));
    }
    if use_case != QdiscUseCase::Taprio {
        return Err(UnrecoverableError::message("unsupported use case"));
    }

    // Tx Assignment with Qbv and full hardware offload: 20% RT, 80% non-RT.
    //
    // TX Q 0 - RTC
    // TX Q 1 - RTA
    // TX Q 2 - DCP, LLDP, UDP High
    // TX Q 3 - Everything else
    shell.cmd(format!(
        concat!(
            "tc qdisc replace dev {} handle 100 parent root taprio num_tc 4 ",
            //    0  1  2  3  4  5  6  7  8  9 10 11 12 13 14 15
            "map  3  3  3  3  3  2  1  0  3  3  3  3  3  3  3  3 ",
            "queues 1@0 1@1 1@2 1@3 ",
            "base-time {} ",
            "sched-entry S 0x01 100000 ",
            "sched-entry S 0x02 100000 ",
            "sched-entry S 0x04 400000 ",
            "sched-entry S 0x08 400000 ",
            "flags 0x02"
        ),
        interface, base_time
    ))?;
    tracker.add(format!("tc qdisc del dev {interface} root"));

    // Rx Assignment is the same as Tx Assignment
    let rx_queues = [2, 2, 2, 0, 1, 2, 2, 3, 2, 2];

    shell.cmd(format!("ethtool -K {interface} ntuple on"))?;
    tracker.add(format!("ethtool -K {interface} ntuple off"));

    // Steer based in VLAN priority
    let reply = shell.cmd(format!("ethtool -N {interface} flow-type ether vlan 0xe000 m 0x1fff action {} ", rx_queues[0]))?;
    tracker.add(format!( "ethtool -N {interface} delete {}", reply.get(19..).unwrap_or("") ));
    let reply = shell.cmd(format!("ethtool -N {interface} flow-type ether vlan 0xc000 m 0x1fff action {} ", rx_queues[1]))?;
    tracker.add(format!( "ethtool -N {interface} delete {}", reply.get(19..).unwrap_or("") ));
    let reply = shell.cmd(format!("ethtool -N {interface} flow-type ether vlan 0xa000 m 0x1fff action {} ", rx_queues[2]))?;
    tracker.add(format!( "ethtool -N {interface} delete {}", reply.get(19..).unwrap_or("") ));
    let reply = shell.cmd(format!("ethtool -N {interface} flow-type ether vlan 0x8000 m 0x1fff action {} ", rx_queues[3]))?;
    tracker.add(format!( "ethtool -N {interface} delete {}", reply.get(19..).unwrap_or("") ));
    let reply = shell.cmd(format!("ethtool -N {interface} flow-type ether vlan 0x6000 m 0x1fff action {} ", rx_queues[4]))?;
    tracker.add(format!( "ethtool -N {interface} delete {}", reply.get(19..).unwrap_or("") ));
    let reply = shell.cmd(format!("ethtool -N {interface} flow-type ether vlan 0x4000 m 0x1fff action {} ", rx_queues[5]))?;
    tracker.add(format!( "ethtool -N {interface} delete {}", reply.get(19..).unwrap_or("") ));
    let reply = shell.cmd(format!("ethtool -N {interface} flow-type ether vlan 0x2000 m 0x1fff action {} ", rx_queues[6]))?;
    tracker.add(format!( "ethtool -N {interface} delete {}", reply.get(19..).unwrap_or("") ));
    let reply = shell.cmd(format!("ethtool -N {interface} flow-type ether vlan 0xe000 m 0x1fff action {} ", rx_queues[7]))?;
    tracker.add(format!( "ethtool -N {interface} delete {}", reply.get(19..).unwrap_or("") ));

    // Steer PTP and LLDP by EtherType
    let reply = shell.cmd(format!("ethtool -N {interface} flow-type ether proto 0x88f7 action {} ", rx_queues[8]))?;
    tracker.add(format!( "ethtool -N {interface} delete {}", reply.get(19..).unwrap_or("") ));
    let reply = shell.cmd(format!("ethtool -N {interface} flow-type ether proto 0x88f7 action {} ", rx_queues[9]))?;
    tracker.add(format!( "ethtool -N {interface} delete {}", reply.get(19..).unwrap_or("") ));

    // Increase ring parameters
    shell.cmd(format!("ethtool -G {interface} tx 4096 rx 4096"))?;
    tracker.add(format!("ethtool -G {interface} tx 256 rx 256"));

    boost_irq_threads(shell, interface, &mut tracker)?;

    Ok(tracker)
}

/// Tx Assignment with Qbv and full hardware offload: 20% RT, 80% non-RT.
///
/// TX Q 0 - Everything else
/// TX Q 1 - DCP, LLDP, UDP High
/// TX Q 2 - RTA
/// TX Q 3 - RTC
fn stmmac_setup_egress_taprio(
    shell: &mut impl CommandExecutor,
    interface: &str,
    cycle_time_ns: u64,
    tracker: &mut UndoTracker,
) -> Result<(), UnrecoverableError> {
    // In principle setting base-time to zero will be projected forward in
    // time and give the same base-time across everything sharing the same
    // PTP clock.
    let base_time = 0;

    let qdisc = format!(
        concat!(
            "tc qdisc replace dev {} handle 100 parent root stab overhead 28 taprio num_tc 4 ",
            //    0  1  2  3  4  5  6  7  8  9 10 11 12 13 14 15
            "map  0  0  0  0  0  1  2  3  0  0  0  0  0  0  0  0 ",
            "queues 1@0 1@1 1@3 1@2 ",
            "base-time {} ",
            "sched-entry S 0x08 {} ",
            "sched-entry S 0x04 {} ",
            "sched-entry S 0x02 {} ",
            "sched-entry S 0x01 {} ",
            "flags 0x02"
        ),
        interface,
        base_time,
        (cycle_time_ns * 10) / 100,
        (cycle_time_ns * 10) / 100,
        (cycle_time_ns * 40) / 100,
        (cycle_time_ns * 40) / 100
    );

    shell.cmd(qdisc)?;
    tracker.add(format!("tc qdisc del dev {interface} root"));

    Ok(())
}

/// Profinet-RT style configuration using simple static priority (mqprio).
///
/// Uses four TX queues assigned as follows:
///
///  * TX Q 0 - Everything else
///  * TX Q 1 - DCP, LLDP, UDP High
///  * TX Q 2 - RTA
///  * TX Q 3 - RTC
fn stmmac_setup_4q_egress_mqprio(
    shell: &mut impl CommandExecutor,
    interface: &str,
    tracker: &mut UndoTracker,
) -> Result<(), UnrecoverableError> {
    let qdisc = format!(
        concat!(
            "tc qdisc replace dev {} handle 100 parent root mqprio ",
            "num_tc 4 ",
            //    0  1  2  3  4  5  6  7  8  9 10 11 12 13 14 15
            "map  0  0  0  0  0  1  2  3  0  0  0  0  0  0  0  0 ",
            "queues 1@0 1@1 1@2 1@3 ",
            "hw 1"
        ),
        interface,
    );

    shell.cmd(qdisc)?;
    tracker.add(format!("tc qdisc del dev {interface} root"));

    Ok(())
}

/// Profinet-RT style configuration using simple static priority (mqprio).
///
/// Uses five TX queues assigned as follows:
///
///  * TX Q 0 - Everything else
///  * TX Q 1 - UDP Low
///  * TX Q 1 - DCP, LLDP, UDP High
///  * TX Q 2 - RTA
///  * TX Q 3 - RTC
fn stmmac_setup_5q_egress_mqprio(
    shell: &mut impl CommandExecutor,
    interface: &str,
    tracker: &mut UndoTracker,
) -> Result<(), UnrecoverableError> {
    let qdisc = format!(
        concat!(
            "tc qdisc replace dev {} handle 100 parent root mqprio ",
            "num_tc 5 ",
            //    0  1  2  3  4  5  6  7  8  9 10 11 12 13 14 15
            "map  0  0  0  0  1  2  3  4  0  0  0  0  0  0  0  0 ",
            "queues 1@0 1@1 1@2 1@3 1@4 ",
            "hw 1"
        ),
        interface,
    );

    shell.cmd(qdisc)?;
    tracker.add(format!("tc qdisc del dev {interface} root"));

    Ok(())
}

/// Extend the mqprio configuration by applying bandwidth limiter.
fn stmmac_setup_4q_egress_mqprio_cbs(
    shell: &mut impl CommandExecutor,
    interface: &str,
    tracker: &mut UndoTracker,
) -> Result<(), UnrecoverableError> {
    // Queue #1 does not support CBS in hardware (hence "offload 0")
    let cbs = CbsConfig::new_with_bandwidth(2500000, 800000).unwrap();
    shell.cmd(format!(
        concat!(
            "tc qdisc replace dev {} parent 100:2 handle 102: cbs ",
            "idleslope {} ",
            "sendslope {} ",
            "hicredit {} ",
            "locredit {} ",
            "offload 0"
        ),
        interface, cbs.idle_slope, cbs.send_slope, cbs.hi_credit, cbs.lo_credit
    ))?;
    tracker.add(format!("tc qdisc del dev {interface} parent 100:2"));

    // Queue #2 (and #3) supports full hardware offload of CBS
    let cbs = CbsConfig::new_with_bandwidth(2500000, 400000).unwrap();
    shell.cmd(format!(
        concat!(
            "tc qdisc replace dev {} parent 100:3 handle 103: cbs ",
            "idleslope {} ",
            "sendslope {} ",
            "hicredit {} ",
            "locredit {} ",
            "offload 1"
        ),
        interface, cbs.idle_slope, cbs.send_slope, cbs.hi_credit, cbs.lo_credit
    ))?;
    tracker.add(format!("tc qdisc del dev {interface} parent 100:3"));

    let cbs = CbsConfig::new_with_bandwidth(2500000, 200000).unwrap();
    shell.cmd(format!(
        concat!(
            "tc qdisc replace dev {} parent 100:4 handle 104: cbs ",
            "idleslope {} ",
            "sendslope {} ",
            "hicredit {} ",
            "locredit {} ",
            "offload 1"
        ),
        interface, cbs.idle_slope, cbs.send_slope, cbs.hi_credit, cbs.lo_credit
    ))?;
    tracker.add(format!("tc qdisc del dev {interface} parent 100:4"));

    Ok(())
}

/// Extend the mqprio configuration by applying bandwidth limiter.
fn stmmac_setup_5q_egress_mqprio_cbs(
    shell: &mut impl CommandExecutor,
    interface: &str,
    tracker: &mut UndoTracker,
) -> Result<(), UnrecoverableError> {
    let cbs = CbsConfig::new_with_bandwidth(2500000, 800000).unwrap();
    shell.cmd(format!(
        concat!(
            "tc qdisc replace dev {} parent 100:3 handle 103: cbs ",
            "idleslope {} ",
            "sendslope {} ",
            "hicredit {} ",
            "locredit {} ",
            "offload 1"
        ),
        interface, cbs.idle_slope, cbs.send_slope, cbs.hi_credit, cbs.lo_credit
    ))?;
    tracker.add(format!("tc qdisc del dev {interface} parent 100:3"));

    let cbs = CbsConfig::new_with_bandwidth(2500000, 400000).unwrap();
    shell.cmd(format!(
        concat!(
            "tc qdisc replace dev {} parent 100:4 handle 104: cbs ",
            "idleslope {} ",
            "sendslope {} ",
            "hicredit {} ",
            "locredit {} ",
            "offload 1"
        ),
        interface, cbs.idle_slope, cbs.send_slope, cbs.hi_credit, cbs.lo_credit
    ))?;
    tracker.add(format!("tc qdisc del dev {interface} parent 100:4"));

    let cbs = CbsConfig::new_with_bandwidth(2500000, 200000).unwrap();
    shell.cmd(format!(
        concat!(
            "tc qdisc replace dev {} parent 100:5 handle 105: cbs ",
            "idleslope {} ",
            "sendslope {} ",
            "hicredit {} ",
            "locredit {} ",
            "offload 1"
        ),
        interface, cbs.idle_slope, cbs.send_slope, cbs.hi_credit, cbs.lo_credit
    ))?;
    tracker.add(format!("tc qdisc del dev {interface} parent 100:5"));

    Ok(())
}

/// Extend the mqprio configuration by applying ETF scheduling to the highest
/// priority queue.
///
/// For a TC956x with 4 queues it is not possible to offload to hardware.
fn stmmac_setup_4q_egress_mqprio_tbs(
    shell: &mut impl CommandExecutor,
    interface: &str,
    tracker: &mut UndoTracker,
) -> Result<(), UnrecoverableError> {
    shell.cmd(format!(
        concat!(
            "tc qdisc replace dev {} parent 100:4 handle 104: etf ",
            "clockid CLOCK_TAI ",
            "delta 100000"
        ),
        interface
    ))?;
    tracker.add(format!("tc qdisc del dev {interface} parent 100:4"));

    Ok(())
}

/// Extend the mqprio configuration by applying ETF scheduling to the highest
/// priority queue.
///
/// For a TC956x with 5 queues we can offload queue #4 to hardware
fn stmmac_setup_5q_egress_mqprio_tbs(
    shell: &mut impl CommandExecutor,
    interface: &str,
    tracker: &mut UndoTracker,
) -> Result<(), UnrecoverableError> {
    shell.cmd(format!(
        concat!(
            "tc qdisc replace dev {} parent 100:5 handle 105: etf ",
            "clockid CLOCK_TAI ",
            "delta 100000 ",
            "offload"
        ),
        interface
    ))?;
    tracker.add(format!("tc qdisc del dev {interface} parent 100:5"));

    Ok(())
}
/// Rx Assignment for any 4 queue use-case
///
/// Queue assignment is the same as taprio assignment:
///
///  * RX Q 0 - Everything else
///  * RX Q 1 - DCP, LLDP, UDP High
///  * RX Q 2 - RTA
///  * RX Q 3 - RTC
#[rustfmt::skip]
fn stmmac_setup_4q_ingress_queue(
    shell: &mut impl CommandExecutor,
    interface: &str,
    tracker: &mut UndoTracker,
) -> Result<(), UnrecoverableError> {
    // Rx Assignment is the same as Tx Assignment
    //               7  6  5  4  3  2  1  0 PTP LLDP
    let rx_queues = [1, 1, 1, 3, 2, 1, 1, 0, 1, 1];

    shell.cmd(format!("tc qdisc add dev {interface} ingress"))?;
    tracker.add(format!("tc qdisc del dev {interface} ingress"));

    // Steer based in VLAN priority
    shell.cmd(format!("tc filter add dev {interface} parent ffff: protocol 802.1Q flower vlan_prio 7 hw_tc {}", rx_queues[0]))?;
    shell.cmd(format!("tc filter add dev {interface} parent ffff: protocol 802.1Q flower vlan_prio 6 hw_tc {}", rx_queues[1]))?;
    shell.cmd(format!("tc filter add dev {interface} parent ffff: protocol 802.1Q flower vlan_prio 5 hw_tc {}", rx_queues[2]))?;
    shell.cmd(format!("tc filter add dev {interface} parent ffff: protocol 802.1Q flower vlan_prio 4 hw_tc {}", rx_queues[3]))?;
    shell.cmd(format!("tc filter add dev {interface} parent ffff: protocol 802.1Q flower vlan_prio 3 hw_tc {}", rx_queues[4]))?;
    shell.cmd(format!("tc filter add dev {interface} parent ffff: protocol 802.1Q flower vlan_prio 2 hw_tc {}", rx_queues[5]))?;
    shell.cmd(format!("tc filter add dev {interface} parent ffff: protocol 802.1Q flower vlan_prio 1 hw_tc {}", rx_queues[6]))?;
    shell.cmd(format!("tc filter add dev {interface} parent ffff: protocol 802.1Q flower vlan_prio 0 hw_tc {}", rx_queues[7]))?;

    // Steer PTP and LLDP by EtherType
    shell.cmd(format!("tc filter add dev {interface} parent ffff: protocol 0x88f7 flower hw_tc {}", rx_queues[8]))?;
    shell.cmd(format!("tc filter add dev {interface} parent ffff: protocol 0x88cc flower hw_tc {}", rx_queues[9]))?;

    Ok(())
}

/// The current hardware channel settings reported by
/// `ethtool --show-channels {interface}`.
///
/// Channels reported as `n/a` by ethtool are represented as `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChannelSettings {
    rx: Option<u32>,
    tx: Option<u32>,
    other: Option<u32>,
    combined: Option<u32>,
}

/// Get the current hardware queue counts for the given interface
fn stmmac_get_queue_count(
    shell: &mut impl CommandExecutor,
    interface: &str,
) -> Result<ChannelSettings, UnrecoverableError> {
    let reply = shell.cmd(format!("ethtool --show-channels {interface}"))?;
    parse_channel_settings(&reply)
}

/// Parse the "Current hardware settings" section of
/// `ethtool --show-channels` output.
///
/// The "Pre-set maximums" section is ignored. It uses the same keys as the
/// current settings so parsing must not begin until the current settings
/// header has been seen.
fn parse_channel_settings(output: &str) -> Result<ChannelSettings, UnrecoverableError> {
    let mut settings = ChannelSettings {
        rx: None,
        tx: None,
        other: None,
        combined: None,
    };

    let mut current_settings = false;
    for line in output.lines() {
        let line = line.trim();

        if line == "Current hardware settings:" {
            current_settings = true;
            continue;
        }

        if !current_settings {
            continue;
        }

        let Some((key, value)) = line.split_once(':') else {
            continue;
        };

        // Only the four channel keys are of interest. Anything else (e.g.
        // shell prompt remnants) is ignored so that it cannot be mistaken
        // for a channel setting.
        if !matches!(key.trim(), "RX" | "TX" | "Other" | "Combined") {
            continue;
        }

        let value = value.trim();
        if value == "n/a" {
            continue;
        }

        let value = value.parse::<u32>().map_err(|_| {
            log::error!("Cannot parse ethtool --show-channels value: {line:?}");
            UnrecoverableError::message("Cannot parse ethtool --show-channels output")
        })?;

        match key.trim() {
            "RX" => settings.rx = Some(value),
            "TX" => settings.tx = Some(value),
            "Other" => settings.other = Some(value),
            "Combined" => settings.combined = Some(value),
            _ => {}
        }
    }

    if !current_settings {
        log::error!("ethtool --show-channels output has no current settings: {output:?}");
        return Err(UnrecoverableError::message(
            "ethtool --show-channels output has no current settings",
        ));
    }

    Ok(settings)
}

fn stmmac_setup(
    shell: &mut impl CommandExecutor,
    interface: impl AsRef<str>,
    cycle_time_ns: u64,
    use_case: QdiscUseCase,
) -> Result<UndoTracker, UnrecoverableError> {
    let interface = interface.as_ref();
    let mut tracker = UndoTracker::new();

    load_kernel_modules(shell)?;
    napi_defer_hard_irqs(shell, interface, cycle_time_ns, &mut tracker)?;

    tracker.add(format!("sleep 2"));

    // TODO: There appear to be problems cleaning up after hardware offload in
    //       stmmac. In particular the ingress filtering remains in effect after
    //       hardware offload is disabled. We can work around this by disabling
    //       and re-enabling the interface as part of the cleanup.
    tracker.add(format!("ip link set {interface} up"));
    tracker.add(format!("ip link set {interface} down"));

    shell.cmd(format!("ethtool -K {interface} rx-vlan-offload off"))?;
    tracker.add(format!("ethtool -K {interface} rx-vlan-offload on"));

    let channel_settings = stmmac_get_queue_count(shell, interface)?;
    let tx = channel_settings.tx.unwrap_or(1);

    // tsn_setup() already aborts 5 queue tests if they are not supported
    // allowing us to fallback to 4 queue setups (in case the link partner
    // has fewer queues available)
    match use_case {
        QdiscUseCase::Taprio => {
            stmmac_setup_egress_taprio(shell, interface, cycle_time_ns, &mut tracker)?;
            stmmac_setup_4q_ingress_queue(shell, interface, &mut tracker)?;
        }
        QdiscUseCase::Mqprio => {
            stmmac_setup_4q_egress_mqprio(shell, interface, &mut tracker)?;
            stmmac_setup_4q_ingress_queue(shell, interface, &mut tracker)?;
        }
        QdiscUseCase::MqprioAndCbsWith5Queues if tx >= 5 => {
            stmmac_setup_5q_egress_mqprio(shell, interface, &mut tracker)?;
            stmmac_setup_5q_egress_mqprio_cbs(shell, interface, &mut tracker)?;
            stmmac_setup_4q_ingress_queue(shell, interface, &mut tracker)?;
        }
        QdiscUseCase::MqprioAndCbs | QdiscUseCase::MqprioAndCbsWith5Queues => {
            stmmac_setup_4q_egress_mqprio(shell, interface, &mut tracker)?;
            stmmac_setup_4q_egress_mqprio_cbs(shell, interface, &mut tracker)?;
            stmmac_setup_4q_ingress_queue(shell, interface, &mut tracker)?;
        }
        QdiscUseCase::MqprioAndEtfWith5Queues if tx >= 5 => {
            stmmac_setup_5q_egress_mqprio(shell, interface, &mut tracker)?;
            stmmac_setup_5q_egress_mqprio_tbs(shell, interface, &mut tracker)?;
            stmmac_setup_4q_ingress_queue(shell, interface, &mut tracker)?;
        }
        QdiscUseCase::MqprioAndEtf | QdiscUseCase::MqprioAndEtfWith5Queues => {
            stmmac_setup_4q_egress_mqprio(shell, interface, &mut tracker)?;
            stmmac_setup_4q_egress_mqprio_tbs(shell, interface, &mut tracker)?;
            stmmac_setup_4q_ingress_queue(shell, interface, &mut tracker)?;
        }
    }

    boost_irq_threads(shell, interface, &mut tracker)?;

    Ok(tracker)
}

pub fn mac_setup(
    shell: &mut impl CommandExecutor,
    interface: impl AsRef<str>,
    cycle_time_ns: u64,
    use_case: QdiscUseCase,
) -> Result<UndoTracker, UnrecoverableError> {
    let interface = interface.as_ref();
    let connection = shell.connection();
    log::debug!("Setting up {interface} via {connection} connection");

    let DriverInfo { eth_driver, .. } = DriverInfo::new(shell, interface)?;
    if eth_driver == "igc" {
        igc_setup(shell, interface, cycle_time_ns, use_case)
    } else {
        stmmac_setup(shell, interface, cycle_time_ns, use_case)
    }
}

/// Setup PTP on the given interface
pub fn ptp_setup(
    shell: &mut impl CommandExecutor,
    interface: impl AsRef<str>,
    role: PtpRole,
) -> Result<UndoTracker, UnrecoverableError> {
    let interface = interface.as_ref();
    let connection = shell.connection();
    log::debug!("Setting up PTP for the {connection} connection");

    let mut tracker = UndoTracker::new();

    // Kill already running daemons (just in case)
    shell.cmd("killall ptp4l")?;
    shell.cmd("killall phc2sys")?;

    if role == PtpRole::Master {
        // Copy NTP time to PHC
        shell.cmd(format!("phc_ctl {interface} set"))?;
    } else {
        // Stop NTP service on the slave (on the master need to leave it
        // running so it can provide us the UTC/TAI offset)
        shell.cmd("systemctl stop systemd-timesyncd")?;
        tracker.add(format!("sleep 2"));
        tracker.add(format!("systemctl start systemd-timesyncd"));
    }

    thread::sleep(Duration::from_secs(1));

    // Start PTP as a background process
    let extra_flags = if role == PtpRole::Slave { "-s" } else { "" };
    shell.cmd(format!("setsid -f -- ptp4l -2 -H -i {interface} --socket_priority=4 --tx_timestamp_timeout=40 --hwts_filter=full --step_threshold=1 --transportSpecific=1 {extra_flags}"))?;
    tracker.add(format!("killall ptp4l"));

    if role == PtpRole::Master {
        thread::sleep(Duration::from_secs(1));

        // Configure the grandmaster's clock setup:
        shell.cmd(concat!(
            "pmc -u -b 0 -t 1 'SET GRANDMASTER_SETTINGS_NP ",
            "clockClass 248 ",                 // Default/Uncordinated
            "clockAccuracy 0x2b ",             // Within 10ms
            "offsetScaledLogVariance 0xffff ", // Unspecified
            "currentUtcOffset 37 ",            // UTC to TAI offset
            "leap61 0 ",                       // No leap second
            "leap59 0 ",                       // No leap second
            "currentUtcOffsetValid 1 ",        // UTC offset is known good
            "ptpTimescale 1 ",                 // Network clocks are TAI
            "timeTraceable 1 ",                // Clock is traceable
            "frequencyTraceable 0 ",           // Frequency is not traceable
            "timeSource 0x50 ",                // Time is anchored using NTP
            "'"
        ))?;
    }

    thread::sleep(Duration::from_secs(10));

    let phc2sys_flags = if role == PtpRole::Master {
        // For the master we need to configure phc2sys manually. Automatic setup
        // should work by gathering the settings from ptp4l (e.g. those set
        // above) but this results in the PTP clock being synced to
        // CLOCK_REALTIME rather than CLOCK_TAI).
        //
        // In that basis it is rather odd that to get the right relationship
        // when syncing CLOCK_REALTIME to eth0 that I must set the offset to
        // zero!
        format!("-s CLOCK_REALTIME -c {interface} -O 0")
    } else {
        // When syncing from PHC to then all the automatic discovery across the
        //  network works perfectly!
        format!("-a -r")
    };
    shell.cmd(format!(
        "setsid -f -- phc2sys {phc2sys_flags} --step_threshold=1 --transportSpecific=1"
    ))?;
    tracker.add(format!("killall phc2sys"));

    Ok(tracker)
}

/// Poll `pmc` on the DUT until PTP has settled.
///
/// PTP is considered settled once we have seen five consecutive one-second
/// samples where `offsetFromMaster` is within ±100 ns.
fn wait_for_ptp_settled(shell: &mut impl CommandExecutor) -> Result<(), UnrecoverableError> {
    const OFFSET_LIMIT_NS: f64 = 100.0;
    const REQUIRED_CONSECUTIVE_SAMPLES: u32 = 5;
    const TIMEOUT: Duration = Duration::from_secs(300);

    log::debug!(
        "Waiting for offsetFromMaster to stay within ±{OFFSET_LIMIT_NS} ns for \
         {REQUIRED_CONSECUTIVE_SAMPLES} consecutive seconds"
    );

    let mut consecutive = 0;
    let start = Instant::now();
    loop {
        let elapsed = start.elapsed();
        if elapsed >= TIMEOUT {
            return Err(UnrecoverableError::message(
                "timed out waiting for PTP to settle",
            ));
        }

        let total_secs = elapsed.as_secs();
        let minutes = total_secs / 60;
        let seconds = total_secs % 60;

        if let Some(offset) = ptp_offset_from_master(shell)? {
            print!("[{minutes:02}:{seconds:02}] offsetFromMaster: {offset:6.0} ns\r");
            let _ = io::stdout().flush();

            if offset.abs() <= OFFSET_LIMIT_NS {
                consecutive += 1;
            } else {
                consecutive = 0;
            }
        } else {
            log::info!("Failed to read offsetFromMaster, resetting streak");
            consecutive = 0;
        }

        if consecutive >= REQUIRED_CONSECUTIVE_SAMPLES {
            log::info!("PTP has settled");
            return Ok(());
        }

        thread::sleep(Duration::from_secs(1));
    }
}

/// Query the DUT's PTP client for `offsetFromMaster`, in nanoseconds.
fn ptp_offset_from_master(
    shell: &mut impl CommandExecutor,
) -> Result<Option<f64>, UnrecoverableError> {
    let output = shell.cmd("sudo pmc -u -b 0 -t 1 'GET CURRENT_DATA_SET'")?;
    Ok(parse_ptp_offset_from_master(&output))
}

fn parse_ptp_offset_from_master(output: &str) -> Option<f64> {
    output.lines().find_map(|line| {
        line.trim()
            .strip_prefix("offsetFromMaster")
            .and_then(|rest| rest.trim().parse::<f64>().ok())
    })
}

/// Setup both the DUT and the link partner for TSN testing.
///
/// This involves setting up packet ingress, packet egress and PTP.
pub fn tsn_setup(
    config: &Config,
    shell: &mut impl CommandExecutor,
    partner: &mut impl CommandExecutor,
    cycle_time_ns: u64,
    use_case: QdiscUseCase,
    ptp: PtpMode,
) -> Result<(TrafficContext, UndoTracker, UndoTracker), UnrecoverableError> {
    let channel_settings = stmmac_get_queue_count(shell, &config.adapter)?;
    let tx = channel_settings.tx.unwrap_or(1);
    if tx < 5
        && (use_case == QdiscUseCase::MqprioAndCbsWith5Queues
            || use_case == QdiscUseCase::MqprioAndEtfWith5Queues)
    {
        return Err(UnrecoverableError::message(
            "adapter must have at least 5 TX queues",
        ));
    }

    let context = traffic_context(config, shell, partner)?;

    let mut partner_teardown = UndoTracker::new();
    partner_teardown.append(mac_setup(
        partner,
        &context.mirror_interface,
        cycle_time_ns,
        use_case,
    )?);
    if ptp == PtpMode::Enabled {
        partner_teardown.append(ptp_setup(
            partner,
            &context.mirror_interface,
            PtpRole::Master,
        )?);
    }

    let mut shell_teardown = UndoTracker::new();
    shell_teardown.append(mac_setup(
        shell,
        &context.reference_interface,
        cycle_time_ns,
        use_case,
    )?);
    if ptp == PtpMode::Enabled {
        shell_teardown.append(ptp_setup(
            shell,
            &context.reference_interface,
            PtpRole::Slave,
        )?);

        wait_for_ptp_settled(shell)?;
    }

    Ok((context, shell_teardown, partner_teardown))
}

fn config_cyclic_only(
    context: &TrafficContext,
    shell: &mut impl CommandExecutor,
    partner: &mut impl CommandExecutor,
    cycle_time_ns: u64,
) -> Result<(), UnrecoverableError> {
    let rtc = TrafficClass::rtc()
        //.set_xdp(false, true, true)
        .set_vid(100)
        .set_frame_count_and_length(1, 128)
        .set_txrx_queue(1)
        .set_socket_priority(7)
        .set_thread_cpu_and_priority(1, 98);

    let rta = TrafficClass::rta().disable();
    let dcp = TrafficClass::dcp().disable();
    let lldp = TrafficClass::lldp().disable();
    let udp_high = TrafficClass::udp_high().disable();
    let udp_low = TrafficClass::udp_low().disable();

    let reference = rtc_testbench::Config {
        application: rtc_testbench::Application {
            application_clock_id: "CLOCK_TAI".to_string(),
            application_base_cycle_time_ns: cycle_time_ns.to_string(),
            application_tx_base_offset_ns: ((cycle_time_ns * 85) / 100).to_string(),
            application_rx_base_offset_ns: ((cycle_time_ns * 20) / 100).to_string(),
            application_xdp_program: "xdp_kern_profinet_vid100.o".to_string(),
        },

        rtc: rtc.clone().with_reference(&context).to_value(),
        rta: rta.clone().with_reference(&context).to_value(),
        dcp: dcp.clone().with_reference(&context).to_value(),
        lldp: lldp.clone().with_reference(&context).to_value(),
        udp_high: udp_high.clone().with_reference(&context).to_value(),
        udp_low: udp_low.clone().with_reference(&context).to_value(),

        log: rtc_testbench::Log {
            log_thread_priority: 1,
            log_thread_cpu: 0,
            log_file: "reference.log".to_string(),
            log_level: "Info".to_string(),
        },
        debug: rtc_testbench::Debug {
            debug_stop_trace_on_outlier: false,
            debug_stop_trace_on_error: false,
            debug_monitor_mode: false,
            debug_monitor_destination: "44:44:44:44:44:44".to_string(),
        },
    };

    shell.cmd(format!(
        "cat > reference.yaml <<\"EOF\"\n{}\nEOF",
        reference.to_string()
    ))?;

    let mut mirror = reference.clone();
    mirror.rtc = rtc.with_mirror(&context).to_value();
    mirror.rta = rta.with_mirror(&context).to_value();
    mirror.dcp = dcp.with_mirror(&context).to_value();
    mirror.lldp = lldp.with_mirror(&context).to_value();
    mirror.udp_high = udp_high.with_mirror(&context).to_value();
    mirror.udp_low = udp_low.with_mirror(&context).to_value();
    mirror.log.log_file = "mirror.log".to_string();

    partner.cmd(format!(
        "cat > mirror.yaml <<\"EOF\"\n{}\nEOF",
        mirror.to_string()
    ))?;

    Ok(())
}

pub fn profinet_cyclic_only(
    config: &Config,
    shell: &mut impl CommandExecutor,
) -> Result<u32, UnrecoverableError> {
    let cycle_time_ns = 1_000_000;
    let mut failures = 0;

    let mut executor = SudoExecutor::new();
    let partner = &mut executor;

    let (context, mut shell_teardown, mut partner_teardown) = tsn_setup(
        config,
        shell,
        partner,
        cycle_time_ns,
        QdiscUseCase::Taprio,
        PtpMode::Enabled,
    )?;

    config_cyclic_only(&context, shell, partner, cycle_time_ns)?;

    let benchmark = run_rtc_benchmark(config, shell, partner, false)?;

    partner_teardown.restore(partner)?;
    shell_teardown.restore(shell)?;

    benchmark.log();

    match &benchmark.rtc_stats_shell {
        Some(stats) => {
            for tag in ["Rtc"] {
                if stats.get(tag).map(|s| s.received).unwrap_or(0) == 0 {
                    log::error!("{tag}Received is zero");
                    failures += 1;
                }
            }
        }
        None => {
            log::error!("Failed to parse reference stats");
            failures += 1;
        }
    }

    Ok(failures)
}

fn config_profinet_rt(
    context: &TrafficContext,
    shell: &mut impl CommandExecutor,
    partner: &mut impl CommandExecutor,
    cycle_time_ns: u64,
    xdp_mode: XdpMode,
) -> Result<(), UnrecoverableError> {
    let mut rtc = TrafficClass::rtc()
        .set_vid(100)
        .set_frame_count_and_length(32, 128)
        .set_txrx_queue(3)
        .set_socket_priority(7)
        .set_thread_cpu_and_priority(3, 98);
    if xdp_mode == XdpMode::Enabled {
        rtc = rtc.set_xdp(false, true, true);
    }

    let mut rta = TrafficClass::rta()
        .set_vid(100)
        .set_burst_period_ns("200ms")
        .set_frame_count_and_length(32, 128)
        .set_txrx_queue(2)
        .set_socket_priority(6)
        .set_thread_cpu_and_priority(2, 97);
    if xdp_mode == XdpMode::Enabled {
        rta = rta.set_xdp(false, true, true);
    }

    let dcp = TrafficClass::dcp()
        .set_vid(100)
        .set_burst_period_ns("2s")
        .set_frame_count_and_length(1, 200)
        .set_txrx_queue(1)
        .set_socket_priority(5)
        .set_thread_cpu_and_priority(1, 53)
        .set_destination("01:0e:cf:00:00:00");

    let lldp = TrafficClass::lldp()
        .set_burst_period_ns("5s")
        .set_frame_count_and_length(1, 200)
        .set_txrx_queue(1)
        .set_socket_priority(5)
        .set_thread_cpu_and_priority(1, 52)
        .set_destination("01:80:c2:00:00:0e");

    let udp_high = TrafficClass::udp_high()
        .set_burst_period_ns("1s")
        .set_frame_count_and_length(1, 1400)
        .set_txrx_queue(1)
        .set_socket_priority(5)
        .set_thread_cpu_and_priority(1, 51)
        .set_port(6666);

    let udp_low = TrafficClass::udp_low()
        .set_burst_period_ns("1s")
        .set_frame_count_and_length(1, 1400)
        .set_txrx_queue(0)
        .set_socket_priority(4)
        .set_thread_cpu_and_priority(0, 51)
        .set_port(6667);

    let reference = rtc_testbench::Config {
        application: rtc_testbench::Application {
            application_clock_id: "CLOCK_TAI".to_string(),
            application_base_cycle_time_ns: cycle_time_ns.to_string(),
            application_tx_base_offset_ns: ((cycle_time_ns * 85) / 100).to_string(),
            application_rx_base_offset_ns: ((cycle_time_ns * 20) / 100).to_string(),
            application_xdp_program: "xdp_kern_profinet_vid100.o".to_string(),
        },

        rtc: rtc.clone().with_reference(&context).to_value(),
        rta: rta.clone().with_reference(&context).to_value(),
        dcp: dcp.clone().with_reference(&context).to_value(),
        lldp: lldp.clone().with_reference(&context).to_value(),
        udp_high: udp_high.clone().with_reference(&context).to_value(),
        udp_low: udp_low.clone().with_reference(&context).to_value(),

        log: rtc_testbench::Log {
            log_thread_priority: 1,
            log_thread_cpu: 0,
            log_file: "reference.log".to_string(),
            log_level: "Info".to_string(),
        },
        debug: rtc_testbench::Debug {
            debug_stop_trace_on_outlier: false,
            debug_stop_trace_on_error: false,
            debug_monitor_mode: false,
            debug_monitor_destination: "44:44:44:44:44:44".to_string(),
        },
    };

    shell.cmd(format!(
        "cat > reference.yaml <<\"EOF\"\n{}\nEOF",
        reference.to_string()
    ))?;

    let mut mirror = reference.clone();
    mirror.rtc = rtc.with_mirror(&context).to_value();
    mirror.rta = rta.with_mirror(&context).to_value();
    mirror.dcp = dcp.with_mirror(&context).to_value();
    mirror.lldp = lldp.with_mirror(&context).to_value();
    mirror.udp_high = udp_high.with_mirror(&context).to_value();
    mirror.udp_low = udp_low.with_mirror(&context).to_value();
    mirror.log.log_file = "mirror.log".to_string();

    partner.cmd(format!(
        "cat > mirror.yaml <<\"EOF\"\n{}\nEOF",
        mirror.to_string()
    ))?;

    Ok(())
}

pub fn profinet_rt_nop(
    config: &Config,
    shell: &mut impl CommandExecutor,
) -> Result<u32, UnrecoverableError> {
    let cycle_time_ns = 1_000_000;

    let mut executor = SudoExecutor::new();
    let partner = &mut executor;

    let context = traffic_context(config, shell, partner)?;

    config_profinet_rt(&context, shell, partner, cycle_time_ns, XdpMode::Disabled)?;

    let benchmark = run_rtc_benchmark(config, shell, partner, true)?;

    // This test does not have pass/fail criteria, to get a sense of TSN
    // performance we must eyeball the logs
    benchmark.log();

    Ok(0)
}

pub fn profinet_rt_mqprio_xdp(
    config: &Config,
    shell: &mut impl CommandExecutor,
) -> Result<u32, UnrecoverableError> {
    let cycle_time_ns = 1_000_000;

    let mut executor = SudoExecutor::new();
    let partner = &mut executor;

    let (context, mut shell_teardown, mut partner_teardown) = tsn_setup(
        config,
        shell,
        partner,
        cycle_time_ns,
        QdiscUseCase::Mqprio,
        PtpMode::Enabled,
    )?;

    config_profinet_rt(&context, shell, partner, cycle_time_ns, XdpMode::Enabled)?;

    let benchmark = run_rtc_benchmark(config, shell, partner, true)?;

    partner_teardown.restore(partner)?;
    shell_teardown.restore(shell)?;

    // This test does not have pass/fail criteria, to get a sense of TSN
    // performance we must eyeball the logs
    benchmark.log();

    Ok(0)
}

pub fn profinet_rt_mqprio_noxdp(
    config: &Config,
    shell: &mut impl CommandExecutor,
) -> Result<u32, UnrecoverableError> {
    let cycle_time_ns = 1_000_000;

    let mut executor = SudoExecutor::new();
    let partner = &mut executor;

    let (context, mut shell_teardown, mut partner_teardown) = tsn_setup(
        config,
        shell,
        partner,
        cycle_time_ns,
        QdiscUseCase::Mqprio,
        PtpMode::Enabled,
    )?;

    config_profinet_rt(&context, shell, partner, cycle_time_ns, XdpMode::Disabled)?;

    let benchmark = run_rtc_benchmark(config, shell, partner, true)?;
    partner_teardown.restore(partner)?;
    shell_teardown.restore(shell)?;

    // This test does not have pass/fail criteria, to get a sense of TSN
    // performance we must eyeball the logs
    benchmark.log();

    Ok(0)
}

pub fn profinet_rt_mqprio_and_cbs(
    config: &Config,
    shell: &mut impl CommandExecutor,
) -> Result<u32, UnrecoverableError> {
    let cycle_time_ns = 1_000_000;

    let mut executor = SudoExecutor::new();
    let partner = &mut executor;

    let (context, mut shell_teardown, mut partner_teardown) = tsn_setup(
        config,
        shell,
        partner,
        cycle_time_ns,
        QdiscUseCase::MqprioAndCbs,
        PtpMode::Enabled,
    )?;

    config_profinet_rt(&context, shell, partner, cycle_time_ns, XdpMode::Disabled)?;

    let benchmark = run_rtc_benchmark(config, shell, partner, true)?;
    partner_teardown.restore(partner)?;
    shell_teardown.restore(shell)?;

    // This test does not have pass/fail criteria, to get a sense of TSN
    // performance we must eyeball the logs
    benchmark.log();

    Ok(0)
}

pub fn profinet_rt_taprio(
    config: &Config,
    shell: &mut impl CommandExecutor,
) -> Result<u32, UnrecoverableError> {
    let cycle_time_ns = 1_000_000;

    let mut executor = SudoExecutor::new();
    let partner = &mut executor;

    let (context, mut shell_teardown, mut partner_teardown) = tsn_setup(
        config,
        shell,
        partner,
        cycle_time_ns,
        QdiscUseCase::Taprio,
        PtpMode::Enabled,
    )?;

    config_profinet_rt(&context, shell, partner, cycle_time_ns, XdpMode::Enabled)?;

    let benchmark = run_rtc_benchmark(config, shell, partner, false)?;

    partner_teardown.restore(partner)?;
    shell_teardown.restore(shell)?;

    // This test does not have pass/fail criteria, to get a sense of TSN
    // performance we must eyeball the logs
    benchmark.log();

    Ok(0)
}

/// A single result line from a `netperf -t UDP_STREAM` run.
///
/// The five fields are the "Socket Size", "Message Size", "Elapsed Time",
/// "Messages" (Okay) and "Throughput" columns of the netperf report. The
/// "Message Size" column is optional: it is absent from the send-side
/// result lines.
#[derive(Debug, PartialEq)]
struct NetperfStatus {
    socket_size: u64,
    message_size: Option<u64>,
    elapsed: f64,
    okay: u64,
    /// Throughput in 10^6 bits/sec.
    throughput: f64,
}

/// Parses the result lines from the output of one or more concurrent
/// `netperf -t UDP_STREAM` runs, returning an iterator over the parsed
/// `NetperfStatus` values.
///
/// The interesting result lines have one of two shapes. Receive-side lines
/// include both the "Message Size" and "Errors" columns:
///
/// ```text
/// 212992   65507   5.00         6949      0     728.14
/// ```
///
/// Send-side lines have four fields; the "Message Size" and "Errors"
/// columns are missing:
///
/// ```text
/// 212992           5.00         2761            289.31
/// ```
///
/// In both cases field 3 is the elapsed time and the final field is the
/// throughput in 10^6 bits/sec. Lines that do not look like result lines
/// (banners, headers, blank lines) are skipped.
fn parse_netperf_throughput(output: &str) -> impl Iterator<Item = NetperfStatus> + '_ {
    output.lines().filter_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();

        let (socket_size, message_size, elapsed, okay, throughput) = match fields.as_slice() {
            // Receive-side line: message size present, errors ignored.
            [socket, message, elapsed, okay, _errors, throughput] => (
                socket.parse().ok()?,
                Some(message.parse().ok()?),
                elapsed.parse().ok()?,
                okay.parse().ok()?,
                throughput.parse().ok()?,
            ),
            // Send-side line: message size (and errors) absent.
            [socket, elapsed, okay, throughput] => (
                socket.parse().ok()?,
                None,
                elapsed.parse().ok()?,
                okay.parse().ok()?,
                throughput.parse().ok()?,
            ),
            _ => return None,
        };

        Some(NetperfStatus {
            socket_size,
            message_size,
            elapsed,
            okay,
            throughput,
        })
    })
}

/// Run netperf with the given use case and return the low and high priority
/// throughput.
fn netperf_pair(
    config: &Config,
    shell: &mut impl CommandExecutor,
    use_case: Option<QdiscUseCase>,
) -> Result<(f64, f64), UnrecoverableError> {
    const DURATION_UDP_HIGH: u64 = 10;
    const DURATION_RTC: u64 = DURATION_UDP_HIGH + 2;

    let mut executor = SudoExecutor::new();
    let partner = &mut executor;

    let (mut shell_teardown, mut partner_teardown) = if let Some(use_case) = use_case {
        let (_, shell_teardown, partner_teardown) = tsn_setup(
            config,
            shell,
            partner,
            1_000_000,
            use_case,
            PtpMode::Disabled,
        )?;

        (shell_teardown, partner_teardown)
    } else {
        (UndoTracker::new(), UndoTracker::new())
    };

    partner.cmd("netserver")?;
    partner_teardown.add("killall netserver".to_string());

    let reply = shell.with_timeout_secs(DURATION_UDP_HIGH + 30, |sh| {
        sh.cmd(format!(
            concat!(
                "netperf -H {} -l {} -y 7 -t UDP_STREAM -- -m 1472 & ",
                "sleep 1; ",
                "netperf -H {} -l {} -y 5 -t UDP_STREAM -- -m 1472; ",
                "wait"
            ),
            config.ipaddr, DURATION_RTC, config.ipaddr, DURATION_UDP_HIGH
        ))
    })?;

    partner_teardown.restore(partner)?;
    shell_teardown.restore(shell)?;

    let mut rtc = 0.0;
    let mut udp_high = 0.0;

    for status in parse_netperf_throughput(&reply) {
        if status.message_size.is_none() {
            match status.elapsed as u64 {
                DURATION_UDP_HIGH => udp_high = status.throughput,
                DURATION_RTC => rtc = status.throughput,
                _ => {}
            }
        }
    }

    let priorities = (rtc, udp_high);
    log::info!("high to low priority throughput: {priorities:?}");

    Ok(priorities)
}

/// Run two competing UDP streams on a default-configured network device.
///
/// We expect both streams to achieve similar bandwidth but with the "high"
/// priority stream achieving more throughput because it has no competition
/// for two seconds of its runtime.
pub fn netperf_pair_nop(
    config: &Config,
    shell: &mut impl CommandExecutor,
) -> Result<u32, UnrecoverableError> {
    let (hi, lo) = netperf_pair(config, shell, None)?;

    let mut failures = 0;

    if hi < 750.0 || hi > 1500.0 {
        log::error!("netperf_pair_nop: netperf high priority throughput is out of range: {hi}");
        failures += 1;
    }

    if lo < 750.0 || lo > 1500.0 {
        log::error!("netperf_pair_nop: netperf low priority throughput is out of range: {lo}");
        failures += 1;
    }

    if lo > hi {
        log::error!(
            "netperf_pair_nop: netperf low priority throughput is greater than high: {lo} vs {hi}"
        );
        failures += 1;
    }

    Ok(failures)
}

/// Run two UDP streams on a taprio configured network device.
///
/// The two streams are not competing because they are time gated and don't
/// share a gate. Based in the taprio configuration (above) the high
/// priority stream should receive approximately 10% of the bandwidth,
/// while the low priority stream should get 40%.
pub fn netperf_pair_taprio(
    config: &Config,
    shell: &mut impl CommandExecutor,
) -> Result<u32, UnrecoverableError> {
    let (hi, lo) = netperf_pair(config, shell, Some(QdiscUseCase::Taprio))?;

    let mut failures = 0;

    if hi < 125.0 || hi > 250.0 {
        log::error!("netperf_pair_taprio: netperf high priority throughput is out of range: {hi}");
        failures += 1;
    }

    if lo < 600.0 || lo > 1000.0 {
        log::error!("netperf_pair_taprio: netperf low priority throughput is out of range: {lo}");
        failures += 1;
    } else if lo < 750.0 {
        log::warn!(
            "netperf_pair_taprio: low priority throughput is low (but within variability): {lo}"
        );
    }

    Ok(failures)
}

/// Run two competing UDP streams the on a mqprio configured network device.
///
/// The two streams are competing head on and the higher priority stream should
/// get massively more bandwidth than the other.
pub fn netperf_pair_mqprio(
    config: &Config,
    shell: &mut impl CommandExecutor,
) -> Result<u32, UnrecoverableError> {
    let (hi, lo) = netperf_pair(config, shell, Some(QdiscUseCase::Mqprio))?;

    let mut failures = 0;

    if hi < 1000.0 {
        log::error!("netperf_pair_mqprio: netperf high priority throughput is out of range: {hi}");
        failures += 1;
    }

    if lo > 300.0 {
        log::error!("netperf_pair_mqprio: netperf low priority throughput is out of range: {lo}");
        failures += 1;
    } else if lo > 150.0 {
        log::warn!(
            "netperf_pair_mqprio: netperf low priority throughput is high (but within variability): {lo}"
        );
    }

    Ok(failures)
}

/// Run two competing UDP streams the on a mqprio configured network device that
/// uses CBS (Credit-Based Shaping) to limit the bandwidth of each stream.
///
/// The two streams are competing but both have CBS shaping that should dictate
/// the throughput of each.
///
/// Note that on TC956x devices, only the high priority stream will be using
/// hardware offloading for CBS. The low priority stream is on a queue that does
/// not support hardware offload and therefore has a relaxed lower bandwidth
/// limit (since software CBS performance can be unpredictable).
pub fn netperf_pair_mqprio_and_cbs(
    config: &Config,
    shell: &mut impl CommandExecutor,
) -> Result<u32, UnrecoverableError> {
    let (hi, lo) = netperf_pair(config, shell, Some(QdiscUseCase::MqprioAndCbs))?;

    let mut failures = 0;

    if hi < 150.0 || hi > 200.0 {
        log::error!(
            "netperf_pair_mqprio_and_cbs: netperf high priority throughput is out of range: {hi}"
        );
        failures += 1;
    }

    if lo < 500.0 || lo > 800.0 {
        log::error!(
            "netperf_pair_mqprio_and_cbs: netperf low priority throughput is out of range: {lo}"
        );
        failures += 1;
    }

    Ok(failures)
}

/// As `netperf_pair_mqprio_and_cbs` but with 5 queues instead of 4.
///
/// This allows queue 1 to 3 (where queue 1 has no hardware CBS support) to be
/// replaced with queue 2 to 4 (all of which have hardware CBS support).
pub fn netperf_pair_mqprio_and_cbs_with_5_queues(
    config: &Config,
    shell: &mut impl CommandExecutor,
) -> Result<u32, UnrecoverableError> {
    let (hi, lo) = netperf_pair(config, shell, Some(QdiscUseCase::MqprioAndCbsWith5Queues))?;

    let mut failures = 0;

    if hi < 150.0 || hi > 200.0 {
        log::error!(
            "netperf_pair_mqprio_and_cbs: netperf high priority throughput is out of range: {hi}"
        );
        failures += 1;
    }

    if lo < 700.0 || lo > 800.0 {
        log::error!(
            "netperf_pair_mqprio_and_cbs: netperf low priority throughput is out of range: {lo}"
        );
        failures += 1;
    }

    Ok(failures)
}

/// Similar to `netperf_pair` (above) but runs four UDP streams rather than just two.
fn netperf_quad(
    config: &Config,
    shell: &mut impl CommandExecutor,
    use_case: Option<QdiscUseCase>,
) -> Result<(f64, f64, f64, f64), UnrecoverableError> {
    const DURATION_UDP_LOW: u64 = 10;
    const DURATION_UDP_HIGH: u64 = DURATION_UDP_LOW + 2;
    const DURATION_RTA: u64 = DURATION_UDP_HIGH + 2;
    const DURATION_RTC: u64 = DURATION_RTA + 2;

    let mut executor = SudoExecutor::new();
    let partner = &mut executor;

    let (mut shell_teardown, mut partner_teardown) = if let Some(use_case) = use_case {
        let (_, shell_teardown, partner_teardown) = tsn_setup(
            config,
            shell,
            partner,
            1_000_000,
            use_case,
            PtpMode::Disabled,
        )?;

        (shell_teardown, partner_teardown)
    } else {
        (UndoTracker::new(), UndoTracker::new())
    };

    partner.cmd("netserver")?;
    partner_teardown.add("killall netserver".to_string());

    let reply = shell.with_timeout_secs(DURATION_UDP_LOW + 30, |sh| {
        sh.cmd(format!(
            concat!(
                "netperf -H {} -l {} -y 7 -t UDP_STREAM -- -m 1472 & ",
                "sleep 1; ",
                "netperf -H {} -l {} -y 6 -t UDP_STREAM -- -m 1472 & ",
                "sleep 1; ",
                "netperf -H {} -l {} -y 5 -t UDP_STREAM -- -m 1472 & ",
                "sleep 1; ",
                "netperf -H {} -l {} -y 4 -t UDP_STREAM -- -m 1472; ",
                "wait"
            ),
            config.ipaddr,
            DURATION_RTC,
            config.ipaddr,
            DURATION_RTA,
            config.ipaddr,
            DURATION_UDP_HIGH,
            config.ipaddr,
            DURATION_UDP_LOW
        ))
    })?;

    partner_teardown.restore(partner)?;
    shell_teardown.restore(shell)?;

    let mut rtc = 0.0;
    let mut rta = 0.0;
    let mut udp_high = 0.0;
    let mut udp_low = 0.0;

    for status in parse_netperf_throughput(&reply) {
        if status.message_size.is_some() {
            match status.elapsed as u64 {
                DURATION_UDP_LOW => udp_low = status.throughput,
                DURATION_UDP_HIGH => udp_high = status.throughput,
                DURATION_RTA => rta = status.throughput,
                DURATION_RTC => rtc = status.throughput,
                _ => {}
            }
        }
    }

    let priorities = (rtc, rta, udp_high, udp_low);
    log::info!("high to low priority throughput: {priorities:?}");

    Ok(priorities)
}

pub fn netperf_quad_nop(
    config: &Config,
    shell: &mut impl CommandExecutor,
) -> Result<u32, UnrecoverableError> {
    const DURATION_SECS: f64 = 300.0;

    const T: f64 = 2500.0 * 1.2;
    const T_UDP_LOW: f64 = T / 4.0;
    const T_UDP_HIGH: f64 = (2.0 * (T / 3.0) + DURATION_SECS * T_UDP_LOW) / (DURATION_SECS + 2.0);
    const T_RTA: f64 =
        (2.0 * (T / 2.0) + (DURATION_SECS + 2.0) * T_UDP_HIGH) / (DURATION_SECS + 4.0);
    const T_RTC: f64 = (2.0 * T + (DURATION_SECS + 4.0) * T_RTA) / (DURATION_SECS + 6.0);

    let (rtc, rta, udp_high, udp_low) = netperf_quad(config, shell, None)?;

    let mut failures = 0;

    if rtc > T_RTC {
        log::error!("netperf_quad_nop: rtc throughput is too high: {rtc}");
        failures += 1;
    }

    if rta > T_RTA {
        log::error!("netperf_quad_nop: rta throughput is too high: {rta}");
        failures += 1;
    }

    if udp_high > T_UDP_HIGH {
        log::error!("netperf_quad_nop: udp_high throughput is too high: {udp_high}");
        failures += 1;
    }

    if udp_low > T_UDP_LOW {
        log::error!("netperf_quad_nop: udp_low throughput is too high: {udp_low}");
        failures += 1;
    }

    Ok(failures)
}
pub fn netperf_quad_taprio(
    config: &Config,
    shell: &mut impl CommandExecutor,
) -> Result<u32, UnrecoverableError> {
    let (rtc, rta, udp_high, udp_low) = netperf_quad(config, shell, Some(QdiscUseCase::Taprio))?;

    let mut failures = 0;

    if rtc > 250.0 {
        log::error!("netperf_quad_taprio: rtc throughput is too high: {rtc}");
        failures += 1;
    }

    if rta > 250.0 {
        log::error!("netperf_quad_taprio: rta throughput is too high: {rta}");
        failures += 1;
    }

    if udp_high > 1000.0 {
        log::error!("netperf_quad_taprio: udp-high throughput is too high: {udp_high}");
        failures += 1;
    }

    if udp_low > 1000.0 {
        log::error!("netperf_quad_taprio: udp-low throughput is too high: {udp_low}");
        failures += 1;
    }

    Ok(failures)
}

pub fn netperf_quad_mqprio(
    config: &Config,
    shell: &mut impl CommandExecutor,
) -> Result<u32, UnrecoverableError> {
    let (rtc, rta, udp_high, udp_low) = netperf_quad(config, shell, Some(QdiscUseCase::Mqprio))?;

    let mut failures = 0;

    if rtc < 1000.0 {
        log::error!("netperf_quad_mqprio: rtc throughput is too low: {rtc}");
        failures += 1;
    }

    if rta > 150.0 {
        log::error!("netperf_quad_mqprio: rta throughput is too high: {rta}");
        failures += 1;
    }

    if udp_high > 150.0 {
        log::error!("netperf_quad_mqprio: udp-high throughput is too high: {udp_high}");
        failures += 1;
    }

    if udp_low > 150.0 {
        log::error!("netperf_quad_mqprio: udp-low throughput is too high: {udp_low}");
        failures += 1;
    }

    Ok(failures)
}

pub fn netperf_quad_mqprio_and_cbs(
    config: &Config,
    shell: &mut impl CommandExecutor,
) -> Result<u32, UnrecoverableError> {
    let (rtc, rta, udp_high, _) = netperf_quad(config, shell, Some(QdiscUseCase::MqprioAndCbs))?;

    let mut failures = 0;

    if rtc > 200.0 {
        log::error!("netperf_quad_mqprio_and_cbs: rtc throughput is too high: {rtc}");
        failures += 1;
    }

    if rta > 400.0 {
        log::error!("netperf_quad_mqprio_and_cbs: rta throughput is too high: {rta}");
        failures += 1;
    }

    if udp_high > 600.0 {
        log::error!("netperf_quad_mqprio_and_cbs: udp-high throughput is too high: {udp_high}");
        failures += 1;
    }

    Ok(failures)
}

pub fn netperf_quad_mqprio_and_cbs_with_5_queues(
    config: &Config,
    shell: &mut impl CommandExecutor,
) -> Result<u32, UnrecoverableError> {
    let (rtc, rta, udp_high, _) =
        netperf_quad(config, shell, Some(QdiscUseCase::MqprioAndCbsWith5Queues))?;

    let mut failures = 0;

    if rtc > 200.0 {
        log::error!("netperf_quad_mqprio_and_cbs: rtc throughput is too high: {rtc}");
        failures += 1;
    }

    if rta > 400.0 {
        log::error!("netperf_quad_mqprio_and_cbs: rta throughput is too high: {rta}");
        failures += 1;
    }

    if udp_high > 600.0 {
        log::error!("netperf_quad_mqprio_and_cbs: udp-high throughput is too high: {udp_high}");
        failures += 1;
    }

    Ok(failures)
}

fn txrx_tsn(
    config: &Config,
    shell: &mut impl CommandExecutor,
    use_case: QdiscUseCase,
) -> Result<u32, UnrecoverableError> {
    let mut executor = SudoExecutor::new();
    let partner = &mut executor;

    let partner_mac_addr = ip::mac_address(partner, &config.partner_adapter)?;

    let (_, mut shell_teardown, mut partner_teardown) = tsn_setup(
        config,
        shell,
        partner,
        1_000_000,
        use_case,
        PtpMode::Enabled,
    )?;

    partner.cmd("rm -f packet.log")?;
    partner.cmd(format!(
        "setsid -f chrt -f 35 txrx-tsn --interface {} --afpkt --receive --frames-to-send=1200 --hw-timestamps > packet.log",
        config.partner_adapter
    ))?;
    partner_teardown.add(format!("killall txrx-tsn"));

    thread::sleep(Duration::from_secs(1));

    shell.with_timeout_secs(30, |sh| {
        sh.cmd(format!(
            concat!(
                "chrt -f 35 ",
                "strace -o txrx-tsn.log -- ",
                "txrx-tsn --interface {} --afpkt --transmit --hw-timestamps ",
                "--dst-mac-addr={} ",
                "--frames-to-send=1500 --cycle-time=10000000 ",
                "--vlan-prio=7 ",
                "--launchtime --early-offset=5000000"
            ),
            config.partner_adapter, partner_mac_addr
        ))
    })?;

    // We are deliberately tearing down the partner second. We are allowing
    // time for the receiver to finish processing the packets.
    shell_teardown.restore(shell)?;
    partner_teardown.restore(partner)?;

    let log = partner.cmd("cat packet.log")?;
    let deltas = log
        .lines()
        .map(|ln| {
            let mut fields = ln.split_whitespace();
            let tx_time = fields.nth(4).unwrap_or("").parse::<u64>().unwrap_or(0);
            let rx_time = fields.next().unwrap_or("").parse::<u64>().unwrap_or(0);

            rx_time as f64 - tx_time as f64
        })
        .collect::<Vec<_>>();

    let min = deltas.clone().min();
    let max = deltas.clone().max();
    let mean = deltas.clone().mean();
    let stddev = deltas.clone().std_dev();
    let len = deltas.len();

    log::info!(
        "txrx_tsn_mqprio_and_etf: delta stats: N={len} min={min} mean={mean} max={max} stddev={stddev}",
    );

    let mut failures = 0;

    // If packets go missing that's usually because they were still valid (before
    // deadline) when issued by userspace but were dropped by the kernel for
    // being late as they were issued to the DMA queue.
    if len < 1200 {
        log::error!("txrx_tsn_mqprio_and_etf: Expected >1200 samples, got {len}",);
        failures += 1;
    }

    if mean < 0.0 {
        log::error!("txrx_tsn_mqprio_and_etf: Most packets were not ready in time: {mean} ns");
        failures += 1;
    }

    Ok(failures)
}

pub fn txrx_tsn_mqprio_and_etf(
    config: &Config,
    shell: &mut impl CommandExecutor,
) -> Result<u32, UnrecoverableError> {
    txrx_tsn(config, shell, QdiscUseCase::MqprioAndEtf)
}

pub fn txrx_tsn_mqprio_and_etf_with_5_queues(
    config: &Config,
    shell: &mut impl CommandExecutor,
) -> Result<u32, UnrecoverableError> {
    txrx_tsn(config, shell, QdiscUseCase::MqprioAndEtfWith5Queues)
}

#[cfg(test)]
mod tests {
    use super::{
        CbsConfig, ChannelSettings, NetperfStatus, parse_channel_settings,
        parse_netperf_throughput, parse_ptp_offset_from_master,
    };

    const NETPERF_OUTPUT: &str = "\
MIGRATED UDP STREAM TEST from 0.0.0.0 (0.0.0.0) port 0 AF_INET to 192.168.10.2 () port 0 AF_INET : demo
MIGRATED UDP STREAM TEST from 0.0.0.0 (0.0.0.0) port 0 AF_INET to 192.168.10.2 () port 0 AF_INET : demo
Socket  Message  Elapsed      Messages
Size    Size     Time         Okay Errors   Throughput
bytes   bytes    secs            #      #   10^6bits/sec

212992   65507   5.00         6949      0     728.14
212992           5.00         2761            289.31

Socket  Message  Elapsed      Messages
Size    Size     Time         Okay Errors   Throughput
bytes   bytes    secs            #      #   10^6bits/sec

212992    1472   7.00        61528      0     103.50
212992           7.00        61483            103.43";

    const PMC_OUTPUT: &str = "sending: GET CURRENT_DATA_SET\
        \n\t025efb.fffe.641bcb-0 seq 0 RESPONSE MANAGEMENT CURRENT_DATA_SET\
        \n\t\tstepsRemoved     1\
        \n\t\toffsetFromMaster 23.0\
        \n\t\tmeanPathDelay    3989.0";

    #[test]
    fn parses_offset_from_master() {
        assert_eq!(parse_ptp_offset_from_master(PMC_OUTPUT), Some(23.0));
    }

    #[test]
    fn parses_negative_offset() {
        let output = "\t\toffsetFromMaster -87.0";
        assert_eq!(parse_ptp_offset_from_master(output), Some(-87.0));
    }

    #[test]
    fn missing_offset_is_none() {
        assert_eq!(parse_ptp_offset_from_master("no data here"), None);
    }

    #[test]
    fn parses_netperf_throughput() {
        let statuses: Vec<NetperfStatus> = parse_netperf_throughput(NETPERF_OUTPUT).collect();

        assert_eq!(
            statuses,
            [
                NetperfStatus {
                    socket_size: 212992,
                    message_size: Some(65507),
                    elapsed: 5.00,
                    okay: 6949,
                    throughput: 728.14,
                },
                NetperfStatus {
                    socket_size: 212992,
                    message_size: None,
                    elapsed: 5.00,
                    okay: 2761,
                    throughput: 289.31,
                },
                NetperfStatus {
                    socket_size: 212992,
                    message_size: Some(1472),
                    elapsed: 7.00,
                    okay: 61528,
                    throughput: 103.50,
                },
                NetperfStatus {
                    socket_size: 212992,
                    message_size: None,
                    elapsed: 7.00,
                    okay: 61483,
                    throughput: 103.43,
                },
            ]
        );
    }

    #[test]
    fn netperf_missing_lines_are_skipped() {
        let statuses: Vec<NetperfStatus> =
            parse_netperf_throughput("no netperf output here").collect();

        assert!(statuses.is_empty());
    }

    const ETHTOOL_CHANNELS_OUTPUT: &str = "\
Channel parameters for eth0:
Pre-set maximums:
RX:\t\t8
TX:\t\t8
Other:\t\tn/a
Combined:\tn/a
Current hardware settings:
RX:\t\t5
TX:\t\t5
Other:\t\tn/a
Combined:\tn/a";

    #[test]
    fn parses_channel_settings() {
        let settings = parse_channel_settings(ETHTOOL_CHANNELS_OUTPUT).unwrap();

        // The pre-set maximums (8) must not be confused with the current
        // settings (5)
        assert_eq!(
            settings,
            ChannelSettings {
                rx: Some(5),
                tx: Some(5),
                other: None,
                combined: None,
            }
        );
    }

    #[test]
    fn parses_combined_channel_settings() {
        let output = "\
Channel parameters for eth0:
Pre-set maximums:
RX:\t\tn/a
TX:\t\tn/a
Other:\t\t1
Combined:\t8
Current hardware settings:
RX:\t\tn/a
TX:\t\tn/a
Other:\t\t1
Combined:\t4";

        let settings = parse_channel_settings(output).unwrap();

        assert_eq!(
            settings,
            ChannelSettings {
                rx: None,
                tx: None,
                other: Some(1),
                combined: Some(4),
            }
        );
    }

    #[test]
    fn channel_settings_tolerates_prompt_noise() {
        let output = format!("{ETHTOOL_CHANNELS_OUTPUT}\nroot@rb3gen2:~# ");

        let settings = parse_channel_settings(&output).unwrap();

        assert_eq!(settings.rx, Some(5));
        assert_eq!(settings.tx, Some(5));
    }

    #[test]
    fn channel_settings_without_current_section_is_an_error() {
        let output = "\
Channel parameters for eth0:
Pre-set maximums:
RX:\t\t8
TX:\t\t8";

        assert!(parse_channel_settings(output).is_err());
    }

    #[test]
    fn channel_settings_with_unparseable_value_is_an_error() {
        let output = "\
Current hardware settings:
RX:\t\tmany
TX:\t\t5";

        assert!(parse_channel_settings(output).is_err());
    }

    #[test]
    fn cbs_config_new_with_bandwidth() {
        let config = CbsConfig::new_with_bandwidth(1_000_000, 100_000).unwrap();

        assert_eq!(config.idle_slope, 100_000);
        assert_eq!(config.send_slope, -900_000);
        // hi_credit = ceil(1542 * 100_000 / 1_000_000) = ceil(154.2) = 155
        assert_eq!(config.hi_credit, 155);
        // lo_credit = floor(1542 * -900_000 / 1_000_000) = floor(-1387.8) = -1388
        assert_eq!(config.lo_credit, -1388);
    }
}
