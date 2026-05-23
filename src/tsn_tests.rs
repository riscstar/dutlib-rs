use std::{thread, time::Duration};

use expectrl::Error;

use crate::{
    CommandExecutor, Config, ip,
    native::SudoExecutor,
    rtc_testbench::{self, TrafficClass, TrafficContext},
    tracker::UndoTracker,
};

pub fn traffic_context(
    config: &Config,
    shell: &mut impl CommandExecutor,
    partner: &mut impl CommandExecutor,
) -> Result<TrafficContext, Error> {
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

pub fn config_profinet_rt(
    context: &TrafficContext,
    shell: &mut impl CommandExecutor,
    partner: &mut impl CommandExecutor,
) -> Result<(), Error> {
    let rtc = TrafficClass::rtc()
        .set_xdp(false, true, true)
        .set_vid(100)
        .set_frame_count_and_length(32, 128)
        .set_txrx_queue(1)
        .set_socket_priority(7)
        .set_thread_cpu_and_priority(1, 98);

    let rta = TrafficClass::rta()
        .set_xdp(false, true, true)
        .set_vid(100)
        .set_burst_period_ns("200ms")
        .set_frame_count_and_length(32, 128)
        .set_txrx_queue(2)
        .set_socket_priority(6)
        .set_thread_cpu_and_priority(2, 97);

    let dcp = TrafficClass::dcp()
        .set_vid(100)
        .set_burst_period_ns("2s")
        .set_frame_count_and_length(1, 200)
        .set_txrx_queue(3)
        .set_socket_priority(5)
        .set_thread_cpu_and_priority(3, 53)
        .set_destination("01:0e:cf:00:00:00");

    let lldp = TrafficClass::lldp()
        .set_burst_period_ns("5s")
        .set_frame_count_and_length(1, 200)
        .set_txrx_queue(3)
        .set_socket_priority(5)
        .set_thread_cpu_and_priority(4, 52)
        .set_destination("01:80:c2:00:00:0e");

    let udp_high = TrafficClass::udp_high()
        .set_burst_period_ns("1s")
        .set_frame_count_and_length(1, 1400)
        .set_txrx_queue(3)
        .set_socket_priority(5)
        .set_thread_cpu_and_priority(5, 51)
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
            application_base_cycle_time_ns: "8ms".to_string(),
            application_tx_base_offset_ns: "6800us".to_string(),
            application_rx_base_offset_ns: "800us".to_string(),
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

pub fn run_rtc_benchmark(
    shell: &mut impl CommandExecutor,
    partner: &mut SudoExecutor,
    duration_secs: u64,
) -> Result<(), Error> {
    let startup_delay = 30;
    let mirror_duration = duration_secs + startup_delay + 5;
    let reference_duration = duration_secs + startup_delay;
    let timeout = reference_duration + 30;

    // Make sure we don't read results from stale log files!
    shell.cmd("rm reference.log")?;
    partner.cmd("rm reference.log")?;

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

    Ok(())
}

pub fn load_kernel_modules(shell: &mut impl CommandExecutor) -> Result<(), Error> {
    shell.cmd("modprobe sch_taprio")?;
    shell.cmd("modprobe sch_mqprio")?;
    shell.cmd("modprobe sch_etf")?;

    Ok(())
}

pub fn napi_defer_hard_irqs(
    shell: &mut impl CommandExecutor,
    interface: impl AsRef<str>,
    cycle_time_ns: u64,
    tracker: &mut UndoTracker,
) -> Result<(), Error> {
    let interface = interface.as_ref();

    tracker.sysfs(
        shell,
        format!("/sys/class/net/{interface}/napi_defer_hard_irqs"),
        "10",
    )?;
    tracker.sysfs(
        shell,
        format!("/sys/class/net/{interface}/gro_flush_timeout"),
        format!("{}", cycle_time_ns * 2),
    )?;

    Ok(())
}

pub fn boost_irq_threads(
    shell: &mut impl CommandExecutor,
    interface: impl AsRef<str>,
    tracker: &mut UndoTracker,
) -> Result<(), Error> {
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
pub fn stmmac_setup(
    shell: &mut impl CommandExecutor,
    interface: impl AsRef<str>,
    cycle_time_ns: u64,
) -> Result<UndoTracker, Error> {
    let interface = interface.as_ref();
    let mut tracker = UndoTracker::new();

    load_kernel_modules(shell)?;
    napi_defer_hard_irqs(shell, interface, cycle_time_ns, &mut tracker)?;

    shell.cmd(format!("ethtool -K {interface} rx-vlan-offload off"))?;
    tracker.add(format!("ethtool -K {interface} rx-vlan-offload on"));

    // Tx Assignment with Qbv and full hardware offload: 20% RT, 80% non-RT.
    //
    // TX Q 0 - Everything else
    // TX Q 1 - RTC
    // TX Q 2 - RTA
    // TX Q 3 - DCP, LLDP, UDP High
    //
    // In principle setting base-time to zero will be projected forward in
    // time and give the same base-time across everything sharing the same
    // PTP clock.
    shell.cmd(format!(
        concat!(
            "tc qdisc replace dev {} handle 100 parent root taprio num_tc 4 ",
            "map 0 0 0 0 0 3 2 1 0 0 0 0 0 0 0 0 ",
            "queues 1@0 1@1 1@2 1@3 ",
            "base-time {} ",
            "sched-entry S 0x02 100000 ",
            "sched-entry S 0x04 100000 ",
            "sched-entry S 0x08 400000 ",
            "sched-entry S 0x01 400000 ",
            "flags 0x02"
        ),
        interface, 0
    ))?;
    tracker.add(format!("tc qdisc del dev {interface} root"));

    // Rx Assignment is the same as Tx Assignment
    let rx_queues = [ 3, 3, 3, 1, 2, 3, 3, 0, 3, 3 ];

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

    boost_irq_threads(shell, interface, &mut tracker)?;

    Ok(tracker)
}

pub fn profinet_rt(config: &Config, shell: &mut impl CommandExecutor) -> Result<u32, Error> {
    let mut failures = 0;

    let mut executor = SudoExecutor::new();
    let partner = &mut executor;

    let context = traffic_context(config, shell, partner)?;

    let mut shell_teardown = stmmac_setup(shell, &context.reference_interface, 1000000)?;
    let mut partner_teardown = stmmac_setup(partner, &context.mirror_interface, 1000000)?;

    config_profinet_rt(&context, shell, partner)?;
    run_rtc_benchmark(shell, partner, 60)?;

    let reference = shell.cmd("tail -1 reference.log")?;
    let mirror = partner.cmd("tail -1 mirror.log")?;
    let stats = (
        rtc_testbench::parse_log_line(reference),
        rtc_testbench::parse_log_line(mirror),
    );

    partner_teardown.restore(partner)?;
    shell_teardown.restore(shell)?;

    match &stats.0 {
        Some(stats) => {
            for tag in ["Rtc", "Rta", "Dcp", "Lldp", "UdpHigh", "UdpLow"] {
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

    rtc_testbench::log_traffic_stats(&stats.0);
    rtc_testbench::log_traffic_stats(&stats.1);

    Ok(failures)
}
