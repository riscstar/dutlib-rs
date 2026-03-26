use std::{io, process::Command, sync::LazyLock, thread, time::Duration};

use errno::Errno;
use expectrl::Error;
use regex::Regex;
use serde::Deserialize;

use crate::{CommandExecutor, plans};

/// Show what drivers have bound to the adapter
pub fn driver_info(shell: &mut impl CommandExecutor, adapter: &str) -> Result<(), Error> {
    let mut eth_driver = "unknown";
    let mut bus_info = "unknown";

    // Use ethtool to check for driver information
    let reply = shell.cmd(&format!("ethtool --driver {adapter}"))?;
    for ln in reply.lines() {
        if ln.starts_with("driver: ") {
            eth_driver = &ln["driver: ".len()..];
        }
        if ln.starts_with("bus-info: ") {
            bus_info = &ln["bus-info: ".len()..];
        }
    }

    // Check the bus driver uses using sysfs
    let reply = shell.cmd(&format!("find /sys/bus/*/drivers/ -name \"{bus_info}\""))?;
    let bus_driver = match reply.lines().next() {
        Some(first_line) => first_line.split('/').skip(5).next().unwrap_or("unknown"),
        None => "unknown",
    };

    log::info!("driver_info: {bus_info} is bound to {eth_driver}/{bus_driver}");
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
pub struct AdapterInfo {
    pub speed: Option<u32>,
}

/// Gather information about the adapter
pub fn adapter_info(shell: &mut impl CommandExecutor, adapter: &str) -> Result<AdapterInfo, Error> {
    let reply = shell.cmd(&format!("ethtool --json {adapter}"))?;
    let json = serde_json::from_str::<serde_json::Value>(&reply).map_err(|e| {
        log::error!("{e}");
        io::Error::other("Cannot parse JSON from ethtool")
    })?;

    let speed = json[0]["speed"].as_i64().map(|s| s as u32);

    Ok(AdapterInfo { speed })
}

/// Get the adapter speed (or 2500.0 is the speed cannot be read for any reason)
pub fn adapter_speed(shell: &mut impl CommandExecutor, adapter: &str) -> f64 {
    match adapter_info(shell, adapter) {
        Ok(AdapterInfo { speed: Some(speed) }) => speed as f64,
        _ => 2500.0,
    }
}

/// Wait for the adapter to detect a link and report the link speed
pub fn wait_for_adapter_info(
    shell: &mut impl CommandExecutor,
    adapter: &str,
) -> Result<Option<AdapterInfo>, Error> {
    for _ in 0..10 {
        let info = adapter_info(shell, adapter)?;
        if info.speed.is_some() {
            return Ok(Some(info));
        }

        thread::sleep(Duration::from_secs(1));
    }

    Ok(None)
}

/// Helper function to send an ethtool command that provokes phy renegotiation
/// and wait for link up.
pub fn ethtool_and_wait_for_adapter_info(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    cmd: &str,
) -> Result<Option<AdapterInfo>, Error> {
    shell.cmd(format!("ethtool {cmd}").replace("<ADAPTER>", adapter))?;

    // We assume the above command will cause the PHY to renegotiate. Let's
    // leave a moment for that process to *start* (with a little margin to
    // avoid spamming the logs)
    thread::sleep(Duration::from_secs(3));

    wait_for_adapter_info(shell, adapter)
}

/// Wait for the specified IP address to be assigned to the board
pub fn wait_for_ipv4(shell: &mut impl CommandExecutor, adapter: &str) -> Result<(), Error> {
    for _ in 0..6 {
        let reply = shell.cmd(&format!("ip -4 addr show {adapter}"))?;

        if reply.contains("inet") {
            driver_info(shell, adapter)?;
            return Ok(());
        }

        thread::sleep(Duration::from_secs(5));
    }

    Err(io::Error::other("Timed out waiting for IP address").into())
}

/// Capture basic kernel version management data
pub fn uname(shell: &mut impl CommandExecutor) -> Result<String, Error> {
    let reply = shell.cmd(&format!("uname -a"))?;
    log::info!("uname: {reply}");
    Ok(reply)
}

#[derive(Debug)]
struct PingStats {
    //min: f64,
    avg: f64,
    max: f64,
    //mdev: f64,
}

fn ping_helper_with_stats(
    shell: &mut impl CommandExecutor,
    ipaddr: &str,
    args: &str,
) -> Result<Option<PingStats>, Error> {
    let reply = shell.with_timeout_secs(15, |sh| sh.cmd(&format!("ping {ipaddr} {args}")))?;

    if !reply.contains(" 0% packet loss") {
        return Ok(None);
    }

    for ln in reply.lines() {
        if ln.starts_with("rtt") {
            // rtt min/avg/max/mdev = 0.766/1.146/1.437/0.205 ms
            let stats = ln.split_whitespace().skip(3).next();
            if let Some(stats) = stats {
                let values = stats
                    .split('/')
                    .filter_map(|s| s.parse().ok())
                    .collect::<Vec<f64>>();
                if values.len() == 4 {
                    return Ok(Some(PingStats {
                        //min: values[0],
                        avg: values[1],
                        max: values[2],
                        //mdev: values[3],
                    }));
                }
            }
        }
    }

    Ok(None)
}

pub fn ping_helper(
    name: &str,
    shell: &mut impl CommandExecutor,
    ipaddr: &str,
    args: &str,
) -> Result<u32, Error> {
    let stats = ping_helper_with_stats(shell, ipaddr, args)?;
    if let Some(stats) = stats.as_ref() {
        log::info!("{name}: {stats:?}");
    }

    // The max value is fairly relaxed because neither DUT nor the ping target
    // are tuned for minimal latency.
    Ok(match stats {
        Some(stats) if stats.avg < 2.0 && stats.max < 10.0 => 0,
        Some(stats) => {
            log::warn!("ping: Failed QoS checks ({stats:?})");
            1
        }
        None => {
            log::warn!("ping: Could not ping {ipaddr}");
            1
        }
    })
}

/// Issue 10 pings at 0.5s intervals, check for packet loss and confirm RTT summary exceeds threshold
pub fn ping(shell: &mut impl CommandExecutor, ipaddr: &str) -> Result<u32, Error> {
    ping_helper("ping", shell, ipaddr, "-c 10 -i 0.5")
}

/// Issue 10 pings at 1s intervals, check for packet loss and confirm RTT summary exceeds threshold
pub fn ping_1s(shell: &mut impl CommandExecutor, ipaddr: &str) -> Result<u32, Error> {
    ping_helper("ping_1s", shell, ipaddr, "-c 10")
}

/// Issue 100 pings at 100ms intervals, check for packet loss and confirm RTT summary exceeds threshold
pub fn ping_100ms(shell: &mut impl CommandExecutor, ipaddr: &str) -> Result<u32, Error> {
    ping_helper("ping_100ms", shell, ipaddr, "-c 100 -i 0.1")
}

/// Issue 1000 pings at 10ms intervals, check for packet loss and confirm RTT summary exceeds threshold
pub fn ping_10ms(shell: &mut impl CommandExecutor, ipaddr: &str) -> Result<u32, Error> {
    ping_helper("ping_10ms", shell, ipaddr, "-c 1000 -i 0.01")
}

/// Issue 5000 pings at maximum rate, check for packet loss and confirm RTT summary exceeds threshold
pub fn ping_flood(shell: &mut impl CommandExecutor, ipaddr: &str) -> Result<u32, Error> {
    ping_helper("ping_flood", shell, ipaddr, "-c 5000 -f")
}

fn iperf3_helper(
    shell: &mut impl CommandExecutor,
    ipaddr: &str,
    args: &str,
) -> Result<([f64; 2], [f64; 2]), Error> {
    let reply = shell.cmd(&format!("iperf3 -c {ipaddr} -i 5 -t 5 --json {args}"))?;

    // During iperf3 execution there could be AER error reports on the console.
    // We need to skip these to ensure the JSON parses correctly.
    let offset = reply.find("{").unwrap_or(0);

    let json = serde_json::from_str::<serde_json::Value>(&reply[offset..]).map_err(|e| {
        log::error!("{e}");
        io::Error::other("Cannot parse JSON from iperf3")
    })?;

    Ok((
        [
            json["end"]["sum_sent"]["bits_per_second"]
                .as_f64()
                .unwrap_or(0.0)
                / 1000000.0,
            json["end"]["sum_received"]["bits_per_second"]
                .as_f64()
                .unwrap_or(0.0)
                / 1000000.0,
        ],
        [
            json["end"]["sum_sent_bidir_reverse"]["bits_per_second"]
                .as_f64()
                .unwrap_or(0.0)
                / 1000000.0,
            json["end"]["sum_received_bidir_reverse"]["bits_per_second"]
                .as_f64()
                .unwrap_or(0.0)
                / 1000000.0,
        ],
    ))
}

pub fn iperf3_bidir_tuneable(
    shell: &mut impl CommandExecutor,
    ipaddr: &str,
    tx_threshold: f64,
    rx_threshold: f64,
) -> Result<u32, Error> {
    let (tx, rx) = iperf3_helper(shell, ipaddr, "--bidir")?;
    log::info!("iperf3_bidir: TX is {tx:?}, RX is {rx:?}");

    Ok(
        if tx[0] < tx_threshold
            || tx[1] < tx_threshold
            || rx[0] < rx_threshold
            || rx[1] < rx_threshold
        {
            log::warn!("iperf3_bidir: Network performance is too slow: tx {tx:?} rx {rx:?}");
            1
        } else {
            0
        },
    )
}

pub fn iperf3_bidir(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    let speed = adapter_speed(shell, adapter);
    let (tthresh, rthresh) = (
        speed * 0.7,
        if speed >= 2500.0 {
            // TODO: At 2.5Gb/s we don't achieve peak RX bandwidth
            speed * 0.5
        } else {
            speed * 0.7
        },
    );
    iperf3_bidir_tuneable(shell, ipaddr, tthresh, rthresh)
}

pub fn iperf3_rx(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    let bench = iperf3_helper(shell, ipaddr, "-R")?.0;
    log::info!("iperf3_rx: RX is {bench:?}");
    let threshold = adapter_speed(shell, adapter) * 0.8;

    Ok(if bench[0] < threshold || bench[1] < threshold {
        log::warn!("iperf3_rx: Network performance is too slow {bench:?}\n");
        1
    } else {
        0
    })
}

pub fn iperf3_tx(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    let bench = iperf3_helper(shell, ipaddr, "")?.0;
    log::info!("iperf3_tx: TX performance is {bench:?}");
    let threshold = adapter_speed(shell, adapter) * 0.8;

    Ok(if bench[0] < threshold || bench[1] < threshold {
        log::warn!("iperf3_tx: Network performance is too slow {bench:?}\n");
        1
    } else {
        0
    })
}

pub fn ethtool_selftest(shell: &mut impl CommandExecutor, adapter: &str) -> Result<u32, Error> {
    let mut failures = 0;

    let reply = shell.cmd(format!("ethtool -t {adapter}"))?;
    if reply.contains("Cannot test: Operation not supported") {
        log::warn!(
            "ethtool_selftest: Kernel has been compiled without CONFIG_STMMAC_SELFTESTS, skipping test"
        );
        return Ok(0);
    }

    // Wait for the PHY to renegotiate the link
    thread::sleep(Duration::from_secs(5));

    for ln in reply.lines() {
        //  1. MAC Loopback               \t 0
        // 32. TBS (ETF Scheduler)        \t -95
        let Some((left, right)) = ln.split_once('\t') else {
            continue;
        };
        let name = &left[4..].trim();
        let Ok(result) = right.trim().parse::<i32>().map(|e| Errno(e * -1)) else {
            continue;
        };

        // EOPNOTSUPP (95): Operation no supported
        if result.0 != 0 && result.0 != 95 {
            if name.contains("VLAN") {
                // Currently all VLAN selftests timeout (but VLAN works) so
                // we'll treat it to a warning for now...
                log::warn!("ethtool_selftest: {name:-30} {result}");
            } else {
                log::error!("ethtool_selftest: {name:-30} {result}");
                failures += 1;
            }
        } else {
            log::debug!("ethtool_selftest: {name:-30} {result}");
        }
    }

    Ok(failures)
}

/// Verify the log messages
pub fn verify_log_messages(shell: &mut impl CommandExecutor) -> Result<u32, Error> {
    let mut failures = 0;

    // dwmac-tc956x 0001:05:00.1 enP1p5s0f1: PHY [stmmac-501:1c] driver [Qualcomm QCA8081] (irq=279)
    static PHY_DRIVER_IRQ: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r#": PHY \[(?P<address>[^\]]+)\] driver \[(?P<driver>[^\]]+)\] \(irq=(?P<irq>[^)]+)\)"#,
        )
        .unwrap()
    });

    let dmesg = shell.cmd("dmesg --color=never --read-clear")?;

    for ln in dmesg.lines() {
        // Check that the PHY is not in polling mode
        if let Some(caps) = PHY_DRIVER_IRQ.captures(ln) {
            log::info!(
                "PHY found on bus {}: driver {}, irq {}",
                &caps["address"],
                &caps["driver"],
                &caps["irq"]
            );

            if caps["irq"].to_lowercase().contains("poll") {
                failures += 1;
            }
        }

        let lower = ln.to_lowercase();
        if lower.contains("warn") || lower.contains("error") || lower.contains("bug") {
            // In general warn/error/bug cause the test to fail... but
            // there are a few things on the allowlist...

            // Bugs are bad, debugging is (usually) good
            if !lower.contains("warn")
                && !lower.contains("error")
                && !lower.replace("debug", "").contains("bug")
            {
                continue;
            }

            // [   76.783004] platform sound: deferred probe pending: snd-sc8280xp: WSA Playback: error getting cpu dai name
            if ln.contains("platform sound: deferred probe pending: snd-sc8280xp: WSA Playback: error getting cpu dai name") {
                continue;
            }

            // [  749.349882] pcieport 0001:00:00.0: AER: Correctable error message received from 0001:01:00.0
            // [  749.360186] pcieport 0001:01:00.0: PCIe Bus Error: severity=Correctable, type=Data Link Layer, (Transmitter ID)
            // [  749.372161] pcieport 0001:01:00.0:   device [1179:0623] error status/mask=00001000/00006000
            // [  749.381840] pcieport 0001:01:00.0:    [12] Timeouts
            if !lower.contains("warn")
                && !lower.contains("bug")
                && lower.contains("pcieport")
                && (lower.contains("correctable") || lower.contains("error status/mask="))
            {
                continue;
            }

            // Allow "error -ENODEV: MDIO bus (id: 1280) registration failed"
            if ln.contains("error -ENODEV: MDIO bus") && ln.contains("registration failed") {
                continue;
            }

            // The following are "normal" when an RB3gen2 boots:
            // [    1.785316] geni_i2c 980000.i2c: Direct firmware load for qcom/qcs6490/qupv3fw.elf failed with error -2
            // [    1.836510] remoteproc remoteproc1: Direct firmware load for qcom/qcs6490/adsp.mbn failed with error -2
            // [    1.839334] remoteproc remoteproc3: Direct firmware load for qcom/qcs6490/cdsp.mbn failed with error -2
            if ln.contains("Direct firmware load for qcom/qcs6490/qupv3fw.elf failed with error -2")
                || ln
                    .contains("Direct firmware load for qcom/qcs6490/adsp.mbn failed with error -2")
                || ln
                    .contains("Direct firmware load for qcom/qcs6490/cdsp.mbn failed with error -2")
            {
                continue;
            }

            // Vendor driver can emit any of the following:
            // [   52.803491] tc956x_pci-eth 0001:05:00.0: Error: Module parameter tc956x_eth_ports_bdf not provided or
            // 			value provided in module param not matching with the device BDF.
            // 			Use the device number as 14 and set other associated module parameter values to default
            // [   52.803607] tc956x_pci-eth 0001:05:00.0: tc956xmac_pci_probe: ERROR Invalid macX_interface parameter passed.
            //          Restoring to default interface 1 for the device index: 14
            if ln.contains("Error: Module parameter tc956x_eth_ports_bdf not provided")
                || ln.contains("ERROR Invalid macX_interface parameter passed")
            {
                continue;
            }

            // QLI1.7 emits the following errors during boot:
            // [    4.632429] [     T68] cpufreq-dt: probe of cpufreq-dt failed with error -17
            // [    5.635194] [    T340] mcp251xfd spi3.0 can0: MCP2517FD rev0.0 (-RX_INT -PLL +MAB_NO_WARN +CRC_REG +CRC_RX +CRC_TX +ECC -HD o:0.00MHz c:40.00MHz m:10.00MHz rs:10.00MHz es:0.00MHz rf:10.00MHz ef:0.00MHz) successfully initialized.
            if ln.contains("cpufreq-dt: probe of cpufreq-dt failed with error -17")
                || ln.contains("+MAB_NO_WARN")
            {
                continue;
            }

            log::error!("Message triggered failures: {ln}");
            failures += 1;
        }
    }

    if failures >= 1 {
        log::info!("Kernel log review failed\n{dmesg}");
    }

    Ok(failures)
}

/// Transfer 1GiB of random data concurrently between partner and DUT and verify
/// sha256sums match
///
/// This test will timeout if run over a link slower than 1g (scp cannot copy
/// a gigabyte in that timeframe)
pub fn scp_bidir(shell: &mut impl CommandExecutor, ipaddr: &str) -> Result<u32, Error> {
    // The vendor driver has very limited RX bandwidth so we need a depressingly
    // long timeout here.
    //shell.with_timeout_secs(30, |sh| {
    shell.with_timeout_secs(90, |sh| {
        // Generate and checksum the TX data
        sh.cmd(
            "dd if=/dev/urandom of=urandom_tx.dat bs=1024 count=$((1024*1024)) status=progress",
        )?;
        let my_sha256sum_tx = sh.cmd("sha256sum urandom_tx.dat")?;

        // Generate and checksum the RX data
        sh
        .cmd(format!("ssh test@{ipaddr} dd if=/dev/urandom of=urandom_rx.dat bs=1024 count=$((1024*1024)) status=progress"))?;
        let their_sha256sum_rx =
            sh.cmd(format!("ssh test@{ipaddr} sha256sum urandom_rx.dat"))?;

        // Run the transfer
        sh.cmd(format!(
            "scp urandom_tx.dat test@{ipaddr}: & scp test@{ipaddr}:urandom_rx.dat .; wait"
        ))?;

        // Collect the remaining checksums
        let my_sha256sum_rx = sh.cmd("sha256sum urandom_rx.dat")?;
        let their_sha256sum_tx =
            sh.cmd(format!("ssh test@{ipaddr} sha256sum urandom_tx.dat"))?;

        let mut failures = 0;
        if my_sha256sum_tx != their_sha256sum_tx {
            log::warn!(
                "scp_bidir: TX checksum mismatch: {my_sha256sum_tx} vs {their_sha256sum_tx}"
            );
            failures += 1;
        }
        if my_sha256sum_rx != their_sha256sum_rx {
            log::warn!(
                "scp_bidir: RX checksum mismatch: {my_sha256sum_rx} vs {their_sha256sum_rx}"
            );
            failures += 1;
        }

        Ok(failures)
    })
}

/// Transfer 1GiB of random data from partner to DUT and verify sha256sum
/// matches.
///
/// This test will timeout if run over a link slower than 1g (scp cannot copy
/// a gigabyte in that timeframe)
pub fn scp_rx(shell: &mut impl CommandExecutor, ipaddr: &str) -> Result<u32, Error> {
    // The vendor driver has very limited RX bandwidth so we need a depressingly
    // long timeout here.
    //shell.with_timeout_secs(30, |sh| {
    shell.with_timeout_secs(90, |sh| {
        sh
        .cmd(format!("ssh test@{ipaddr} dd if=/dev/urandom of=urandom_rx.dat bs=1024 count=$((1024*1024)) status=progress"))?;
        let their_sha256sum = sh.cmd(format!("ssh test@{ipaddr} sha256sum urandom_rx.dat"))?;
            sh.cmd(format!("scp test@{ipaddr}:urandom_rx.dat ."))?;
        let my_sha256sum = sh.cmd("sha256sum urandom_rx.dat")?;

        Ok(if my_sha256sum != their_sha256sum {
            log::warn!("scp_rx: Checksum mismatch: {my_sha256sum} vs {their_sha256sum}");
            1
        } else {
            0
        })
    })
}

/// Transfer 1GiB of random data from DUT to partner and verify sha256sum
/// matches.
///
/// This test will timeout if run over a link slower than 1g (scp cannot copy
/// a gigabyte in that timeframe)
pub fn scp_tx(shell: &mut impl CommandExecutor, ipaddr: &str) -> Result<u32, Error> {
    shell.with_timeout_secs(30, |sh| {
        sh.cmd(
            "dd if=/dev/urandom of=urandom_tx.dat bs=1024 count=$((1024*1024)) status=progress",
        )?;
        let my_sha256sum = sh.cmd("sha256sum urandom_tx.dat")?;
        sh.cmd(format!("scp urandom_tx.dat test@{ipaddr}:"))?;
        let their_sha256sum = sh.cmd(format!("ssh test@{ipaddr} sha256sum urandom_tx.dat"))?;

        Ok(if my_sha256sum != their_sha256sum {
            log::warn!("scp_tx: Checksum mismatch: {my_sha256sum} vs {their_sha256sum}");
            1
        } else {
            0
        })
    })
}

/// Run a ethtool command on the link partner.
///
/// This can be used to restrict the advertised link modes.
pub fn link_partner_ethtool(args: &str) -> Result<(), Error> {
    let output = Command::new("sudo")
        .arg("ethtool")
        .args(
            args.replace("<ADAPTER>", "enxccbabda84b23")
                .split_whitespace(),
        )
        .output()?;

    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("ethtool failed: {output:?}")).into())
    }
}

/// Run smoke tests at the default (maximum) link speed.
pub fn link_mode_and_partner_advertise_all(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    let mut failures = 0;

    // advertise everything
    link_partner_ethtool("-s <ADAPTER> advertise 0xffffffffffffffff")?;
    let _ = shell.cmd(&format!(
        "ethtool -s {adapter} advertise 0xffffffffffffffff"
    ))?;
    thread::sleep(Duration::from_secs(2));
    let adapter_info = wait_for_adapter_info(shell, adapter)?;

    if let Some(AdapterInfo { speed: Some(speed) }) = adapter_info {
        log::info!("Link negotiated at {speed}Mb/s");
    } else {
        log::error!("Unexpected adapter info: {adapter_info:?}");
        failures += 1;
    }

    failures += plans::quick_test(shell, adapter, ipaddr)?;

    Ok(failures)
}

fn link_partner_advertise_helper(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    ipaddr: &str,
    advertisement: &str,
    expected_speed: u32,
) -> Result<u32, Error> {
    let mut failures = 0;

    // get the initial adapter info
    let Some(initial_info) = wait_for_adapter_info(shell, adapter)? else {
        log::error!("Failed to read adapter info");
        return Ok(1);
    };

    // skip the test if the partner device is not faster than we are
    if let Some(speed) = initial_info.speed
        && speed == expected_speed
    {
        log::warn!("Cannot test {speed}base-T because partner link speed is too slow");
        return Ok(0);
    }

    // change the link partner's advertisement and give time for the link to stop
    link_partner_ethtool(&format!("-s <ADAPTER> advertise {advertisement}"))?;
    thread::sleep(Duration::from_secs(2));

    // wait for the new adapter info
    let test_info_result = wait_for_adapter_info(shell, adapter);
    if let Ok(Some(AdapterInfo { speed: Some(speed) })) = test_info_result {
        log::info!("Link negotiated at {speed}Mb/s");
    }

    let smoke_test_result = plans::quick_test(shell, adapter, ipaddr);

    // restore the link partner's advertisement and make sure we get the adapter back
    link_partner_ethtool("-s <ADAPTER> advertise 0xffffffffffffffff")?;
    thread::sleep(Duration::from_secs(2));
    let restored_info = wait_for_adapter_info(shell, adapter)?;

    // Make sure "something" works after restoring the defaults
    failures += ping(shell, ipaddr)?;

    // deferred error handling (to ensure the advertisement was restored)
    let test_info = test_info_result?;
    failures += smoke_test_result?;

    // check the link achieved the expect speed
    match test_info {
        Some(AdapterInfo { speed: Some(speed) }) => {
            if expected_speed != speed {
                log::error!("Bad link speed {speed}");
                failures += 1;
            }
        }
        info => {
            log::error!("Unexpected adapter info: {info:?}");
            failures += 1;
        }
    }

    // check the link restored OK
    match restored_info {
        None => {
            log::error!("Link did not restore correctly (no link)");
            failures += 1;
        }
        Some(restored_info) => {
            if initial_info != restored_info {
                log::error!("Bad restored link info: {initial_info:?} versus {restored_info:?}");
                failures += 1;
            }
        }
    }

    Ok(failures)
}

/// Provoke the link partner to advertise 1000baseT/Full and verify correct
/// negotiation.
pub fn link_partner_advertise_1000baset_full(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    link_partner_advertise_helper(shell, adapter, ipaddr, "0x0020", 1000)
}

/// Provoke the link partner to advertise 100baseT/Full and verify correct
/// negotiation.
pub fn link_partner_advertise_100baset_full(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    link_partner_advertise_helper(shell, adapter, ipaddr, "0x0008", 100)
}

/// Provoke the link partner to advertise 10baseT/Full and verify correct
/// negotiation.
pub fn link_partner_advertise_10baset_full(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    link_partner_advertise_helper(shell, adapter, ipaddr, "0x0002", 10)
}

fn link_mode_advertise_helper(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    ipaddr: &str,
    advertisement: &str,
    expected_speed: u32,
) -> Result<u32, Error> {
    let mut failures = 0;

    // get the initial adapter info
    let Some(initial_info) = wait_for_adapter_info(shell, adapter)? else {
        log::error!("Failed to read adapter info");
        return Ok(1);
    };

    // skip the test if the partner device is not faster than we are
    if let Some(speed) = initial_info.speed
        && speed == expected_speed
    {
        log::warn!("Cannot test {speed}base-T because partner link speed is too slow");
        return Ok(0);
    }

    // change our own advertisement and give time for the link to stop
    let _ = shell.cmd(&format!("ethtool -s {adapter} advertise {advertisement}"))?;
    thread::sleep(Duration::from_secs(2));

    // wait for the new adapter info
    let test_info_result = wait_for_adapter_info(shell, adapter);
    if let Ok(Some(AdapterInfo { speed: Some(speed) })) = test_info_result {
        log::info!("Link negotiated at {speed}Mb/s");
    }

    let smoke_test_result = plans::quick_test(shell, adapter, ipaddr);

    // restore the link partner's advertisement and make sure we get the adapter back
    let _ = shell.cmd(&format!(
        "ethtool -s {adapter} advertise 0xffffffffffffffff"
    ))?;
    thread::sleep(Duration::from_secs(2));
    let restored_info = wait_for_adapter_info(shell, adapter)?;

    // Make sure "something" works after restoring the defaults
    failures += ping(shell, ipaddr)?;

    // deferred error handling (to ensure the advertisement was restored)
    let test_info = test_info_result?;
    failures += smoke_test_result?;

    // check the link achieved the expect speed
    match test_info {
        Some(AdapterInfo { speed: Some(speed) }) => {
            if expected_speed != speed {
                log::error!("Bad link speed {speed}");
                failures += 1;
            }
        }
        info => {
            log::error!("Unexpected adapter info: {info:?}");
            failures += 1;
        }
    }

    // check the link restored OK
    match restored_info {
        None => {
            log::error!("Link did not restore correctly (no link)");
            failures += 1;
        }
        Some(restored_info) => {
            if initial_info != restored_info {
                log::error!("Bad restored link info: {initial_info:?} versus {restored_info:?}");
                failures += 1;
            }
        }
    }

    Ok(failures)
}

/// Run smoke tests at the default (maximum) link speed.
pub fn link_mode_advertise_all(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    let mut failures = 0;

    // advertise everything
    let _ = shell.cmd(&format!(
        "ethtool -s {adapter} advertise 0xffffffffffffffff"
    ))?;
    thread::sleep(Duration::from_secs(2));
    let adapter_info = wait_for_adapter_info(shell, adapter)?;

    if let Some(AdapterInfo { speed: Some(speed) }) = adapter_info {
        log::info!("Link negotiated at {speed}Mb/s");
    } else {
        log::error!("Unexpected adapter info: {adapter_info:?}");
        failures += 1;
    }

    failures += plans::quick_test(shell, adapter, ipaddr)?;

    Ok(failures)
}

/// Advertise (up to) 1000baseT/Full and verify correct negotiation.
pub fn link_mode_advertise_1000baset_full(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    link_mode_advertise_helper(shell, adapter, ipaddr, "0x002f", 1000)
}

/// Advertise (up to) 100baseT/Full and verify correct negotiation.
pub fn link_mode_advertise_100baset_full(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    link_mode_advertise_helper(shell, adapter, ipaddr, "0x000f", 100)
}

/// Advertise (up to) 10baseT/Full and verify correct negotiation.
pub fn link_mode_advertise_10baset_full(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    link_mode_advertise_helper(shell, adapter, ipaddr, "0x0003", 10)
}

//
// Bandwidth tests
//

#[derive(Debug, Deserialize)]
pub struct IperfResult {
    pub intervals: Vec<Interval>,
    pub end: End,
}

#[derive(Debug, Deserialize)]
pub struct Interval {
    pub streams: Vec<StreamStats>,
    pub sum: Option<StreamStats>,
    pub sum_bidir_reverse: Option<StreamStats>,
}

#[derive(Debug, Deserialize)]
pub struct End {
    pub streams: Vec<EndStreams>,
    pub sum: Option<StreamStats>,
    pub sum_sent: Option<StreamStats>,
    pub sum_received: Option<StreamStats>,
    pub sum_bidir_reverse: Option<StreamStats>,
    pub sum_sent_bidir_reverse: Option<StreamStats>,
    pub sum_received_bidir_reverse: Option<StreamStats>,
}

#[derive(Debug, Deserialize)]
pub struct EndStreams {
    pub sender: Option<StreamStats>,
    pub receiver: Option<StreamStats>,
    pub udp: Option<StreamStats>,
}

#[derive(Copy, Clone, Debug, Deserialize, Default)]
pub struct StreamStats {
    pub bits_per_second: f64,
    pub lost_percent: Option<f64>,
}

fn get_mbps(stats: Option<StreamStats>) -> f64 {
    stats.unwrap_or(StreamStats::default()).bits_per_second / 1000000.0
}

fn get_lost_percent(stats: Option<StreamStats>) -> f64 {
    stats
        .unwrap_or(StreamStats::default())
        .lost_percent
        .unwrap_or(100.0)
}

fn iperf3_new_helper(
    shell: &mut impl CommandExecutor,
    ipaddr: &str,
    args: &str,
) -> Result<IperfResult, Error> {
    let reply = shell.with_timeout_secs(45, |sh| {
        sh.cmd(&format!("iperf3 -c {ipaddr} -t 30 --json {args}"))
    })?;

    // During iperf3 execution there could be AER error reports on the console.
    // We need to skip these to ensure the JSON parses correctly.
    let offset = reply.find("{").unwrap_or(0);

    let json = serde_json::from_str(&reply[offset..]).map_err(|e| {
        log::error!("{e}");
        io::Error::other("Cannot parse JSON from iperf3")
    })?;

    Ok(json)
}

pub fn iperf3_intervals_bidir(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    let stats = iperf3_new_helper(shell, ipaddr, "-i 5 --bidir")?;

    let speed_mbps = adapter_speed(shell, adapter);

    let mut failures = 0;

    // Vendor driver performance (running on 7.0-rc3, worst-of-ten at 2500Mb/s):
    //                  Mb/s        BW      BW * 0.95
    // TX per interval  2339.5      94%     89%
    // RX per interval  1099.5      44%     42%
    // TX overall       2341.2      94%     89%
    // RX overall       1561.6      62%     59%
    let tx_threshold = speed_mbps * 0.89;
    let rx_threshold = speed_mbps * 0.6;

    for (i, (tx, rx)) in stats
        .intervals
        .iter()
        .map(|interval| (get_mbps(interval.sum), get_mbps(interval.sum_bidir_reverse)))
        .enumerate()
    {
        if tx < tx_threshold || rx < rx_threshold {
            log::warn!(
                "iperf3_intervals_bidir: interval #{i}: Network performance is too slow: TX {tx:0.1}, RX {rx:0.1}"
            );
            failures += 1;
        } else {
            log::info!("iperf3_intervals_bidir: interval #{i}: TX {tx:0.1}, RX {rx:0.1}");
        }
    }

    let tx_us = get_mbps(stats.end.sum_sent);
    let rx_them = get_mbps(stats.end.sum_received);
    let tx_them = get_mbps(stats.end.sum_sent_bidir_reverse);
    let rx_us = get_mbps(stats.end.sum_received_bidir_reverse);

    if tx_us < tx_threshold
        || rx_them < tx_threshold
        || rx_us < rx_threshold
        || tx_them < rx_threshold
    {
        log::warn!(
            "iperf3_intervals_bidir: Overall: Network performance is too slow: TX {tx_us:0.1} ({rx_them:0.1}), RX {rx_us:0.1} ({tx_them:0.1})"
        );
        failures += 1;
    } else {
        log::info!(
            "iperf3_intervals_bidir: Overall: TX {tx_us:0.1} ({rx_them:0.1}), RX {rx_us:0.1} ({tx_them:0.1})"
        );
    }

    Ok(failures)
}

pub fn iperf3_intervals_tx(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    let stats = iperf3_new_helper(shell, ipaddr, "-i 5")?;

    let speed_mbps = adapter_speed(shell, adapter);

    let mut failures = 0;

    // Vendor driver performance (running on 7.0-rc3, worst-of-ten at 2500Mb/s):
    //                  Mb/s        BW      BW * 0.95
    // TX per interval  2350.5      94%     89%
    // TX overall       2348.4      94%     89%
    let tx_threshold = speed_mbps * 0.89;

    for (i, tx) in stats
        .intervals
        .iter()
        .map(|interval| get_mbps(interval.sum))
        .enumerate()
    {
        if tx < tx_threshold {
            log::warn!(
                "iperf3_intervals_tx: interval #{i}: Network performance is too slow: TX {tx:0.1}"
            );
            failures += 1;
        } else {
            log::info!("iperf3_intervals_tx: interval #{i}: TX {tx:0.1}");
        }
    }

    let tx_us = get_mbps(stats.end.sum_sent);
    let rx_them = get_mbps(stats.end.sum_received);

    if tx_us < tx_threshold || rx_them < tx_threshold {
        log::warn!(
            "iperf3_intervals_tx: Overall: Network performance is too slow: TX {tx_us:0.1} ({rx_them:0.1})"
        );
        failures += 1;
    } else {
        log::info!("iperf3_intervals_tx: Overall: TX {tx_us:0.1} ({rx_them:0.1})");
    }

    Ok(failures)
}

pub fn iperf3_intervals_rx(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    let stats = iperf3_new_helper(shell, ipaddr, "-i 5 -R")?;

    let speed_mbps = adapter_speed(shell, adapter);

    let mut failures = 0;

    // Vendor driver performance (running on 7.0-rc3, worst-of-ten at 2500Mb/s):
    //                  Mb/s        BW      BW * 0.95
    // RX per interval  126.1       5%      5%
    // RX overall       136.3       5%      5%
    let rx_threshold = speed_mbps * 0.8;

    for (i, rx) in stats
        .intervals
        .iter()
        .map(|interval| get_mbps(interval.sum))
        .enumerate()
    {
        if rx < rx_threshold {
            log::warn!(
                "iperf3_intervals_rx: interval #{i}: Network performance is too slow: RX {rx:0.1}"
            );
            failures += 1;
        } else {
            log::info!("iperf3_intervals_rx: interval #{i}: RX {rx:0.1}");
        }
    }

    let tx_them = get_mbps(stats.end.sum_sent);
    let rx_us = get_mbps(stats.end.sum_received);

    if rx_us < rx_threshold || tx_them < rx_threshold {
        log::warn!(
            "iperf3_intervals_rx: Overall: Network performance is too slow: RX {rx_us:0.1} ({tx_them:0.1})"
        );
        failures += 1;
    } else {
        log::info!("iperf3_intervals_rx: Overall: RX {rx_us:0.1} ({tx_them:0.1})");
    }

    Ok(failures)
}

pub fn iperf3_udp_bidir(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    let speed_mbps = adapter_speed(shell, adapter);
    let bitrate = (speed_mbps * 0.8) as u32;

    let stats = iperf3_new_helper(
        shell,
        ipaddr,
        &format!("-i 30 --udp --bitrate {bitrate}M --bidir"),
    )?;

    if stats.end.streams.len() != 2 {
        log::error!("Unexpected reply from iperf3");
        return Ok(1);
    }

    let mut failures = 0;

    // Vendor driver performance (running on 7.0-rc3, worst-of-ten at 2500Mb/s):
    //                  %       % * 1.05
    // TX lost packets  5.8%    6.1%
    // RX lost packets  4.2%    4.4%
    let tx_threshold = 5.0;
    let rx_threshold = 4.4;

    let rx = get_lost_percent(stats.end.streams[0].udp);
    let tx = get_lost_percent(stats.end.streams[1].udp);

    if rx > rx_threshold || tx > tx_threshold {
        log::warn!("iperf3_udp_bidir: Packet loss too high: TX {tx:0.1}, RX {rx:0.1}");
        failures += 1;
    } else {
        log::info!("iperf3_udp_bidir: Packet loss OK: TX {tx:0.1}, RX {rx:0.1}");
    }

    Ok(failures)
}

pub fn iperf3_udp_tx(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    let speed_mbps = adapter_speed(shell, adapter);
    let bitrate = (speed_mbps * 0.8) as u32;

    let stats = iperf3_new_helper(shell, ipaddr, &format!("-i 30 --udp --bitrate {bitrate}M"))?;

    if stats.end.streams.len() != 1 {
        log::error!("Unexpected reply from iperf3");
        return Ok(1);
    }

    let mut failures = 0;

    // Vendor driver performance (running on 7.0-rc3, worst-of-ten at 2500Mb/s):
    //                  %       % * 1.05
    // TX lost packets  4.2%    4.4%
    //let threshold = 4.4;
    let threshold = 5.0;

    let tx = get_lost_percent(stats.end.streams[0].udp);

    if tx > threshold {
        log::warn!("iperf3_udp_tx: Packet loss too high: TX {tx:0.1}");
        failures += 1;
    } else {
        log::info!("iperf3_udp_tx: Packet loss OK: TX {tx:0.1}");
    }

    Ok(failures)
}

pub fn iperf3_udp_rx(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    let speed_mbps = adapter_speed(shell, adapter);
    let bitrate = (speed_mbps * 0.8) as u32;

    let stats = iperf3_new_helper(
        shell,
        ipaddr,
        &format!("-i 30 --udp --bitrate {bitrate}M -R"),
    )?;

    if stats.end.streams.len() != 1 {
        log::error!("Unexpected reply from iperf3");
        return Ok(1);
    }

    let mut failures = 0;

    // Vendor driver performance (running on 7.0-rc3, worst-of-ten at 2500Mb/s):
    //                  %       % * 1.05
    // RX lost packets  0.4%    0.42%
    //let threshold = 0.42;
    let threshold = 5.0;

    let rx = get_lost_percent(stats.end.streams[0].udp);

    if rx > threshold {
        log::warn!("iperf3_udp_rx: Packet loss too high: RX {rx:0.1}");
        failures += 1;
    } else {
        log::info!("iperf3_udp_rx: Packet loss OK: RX {rx:0.1}");
    }

    Ok(failures)
}

pub fn iperf3_x16_bidir(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    let stats = iperf3_new_helper(shell, ipaddr, &format!("-i 30 --parallel 8 --bidir"))?;

    let speed_mbps = adapter_speed(shell, adapter);

    let mut failures = 0;

    // Vendor driver performance (running on 7.0-rc3, worst-of-ten at 2500Mb/s):
    //                  Mb/s        BW      BW * 0.95
    // Per interval     176.3       56%     54%
    // TX overall       2342.3      94%     89%
    // RX overall       1541.8      62%     59%
    let stream_threshold = (speed_mbps * 0.54) / 8.0;
    let tx_threshold = speed_mbps * 0.89;
    let rx_threshold = speed_mbps * 0.59;

    for (i, (sender, receiver)) in stats
        .end
        .streams
        .iter()
        .map(|stream| (get_mbps(stream.sender), get_mbps(stream.receiver)))
        .enumerate()
    {
        // The "us" and "them" is mixed in the streams array making it
        // difficult to figure out which sockets are used to TX and which for
        // RX (which whether it is us or them that is the sender). We therefore
        // compare both values against a common minimum threshold.
        if sender < stream_threshold || receiver < stream_threshold {
            log::warn!(
                "iperf3_x16_bidir: stream #{i}: Network bandwidth is not fairly allocated: Send {sender:0.1}, Recv {receiver:0.1}"
            );
            failures += 1;
        } else {
            log::info!("iperf3_x16_bidir: stream #{i}: Send {sender:0.1}, Recv {receiver:0.1}");
        }
    }

    let tx_us = get_mbps(stats.end.sum_sent);
    let rx_them = get_mbps(stats.end.sum_received);
    let tx_them = get_mbps(stats.end.sum_sent_bidir_reverse);
    let rx_us = get_mbps(stats.end.sum_received_bidir_reverse);

    if tx_us < tx_threshold
        || rx_them < tx_threshold
        || rx_us < rx_threshold
        || tx_them < rx_threshold
    {
        log::warn!(
            "iperf3_x16_bidir: Overall: Network performance is too slow: TX {tx_us:0.1} ({rx_them:0.1}), RX {rx_us:0.1} ({tx_them:0.1})"
        );
        failures += 1;
    } else {
        log::info!(
            "iperf3_x16_bidir: Overall: TX {tx_us:0.1} ({rx_them:0.1}), RX {rx_us:0.1} ({tx_them:0.1})"
        );
    }

    Ok(failures)
}

pub fn iperf3_x16_tx(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    let stats = iperf3_new_helper(shell, ipaddr, &format!("-i 30 --parallel 16"))?;

    let speed_mbps = adapter_speed(shell, adapter);

    let mut failures = 0;

    // Vendor driver performance (running on 7.0-rc3, worst-of-ten at 2500Mb/s):
    //                  Mb/s        BW      BW * 0.95
    // Per interval     142.2       91%     86%
    // TX overall       2351.0      94%     89%
    let stream_threshold = (speed_mbps * 0.86) / 16.0;
    let tx_threshold = speed_mbps * 0.89;

    for (i, (sender, receiver)) in stats
        .end
        .streams
        .iter()
        .map(|stream| (get_mbps(stream.sender), get_mbps(stream.receiver)))
        .enumerate()
    {
        if sender < stream_threshold || receiver < stream_threshold {
            log::warn!(
                "iperf3_x16_tx: stream #{i}: Network bandwidth is not fairly allocated: Send {sender:0.1}, Recv {receiver:0.1}"
            );
            failures += 1;
        } else {
            log::info!("iperf3_x16_tx: stream #{i}: Send {sender:0.1}, Recv {receiver:0.1}");
        }
    }

    let tx_us = get_mbps(stats.end.sum_sent);
    let rx_them = get_mbps(stats.end.sum_received);

    if tx_us < tx_threshold || rx_them < tx_threshold {
        log::warn!(
            "iperf3_x16_tx: Overall: Network performance is too slow: TX {tx_us:0.1} ({rx_them:0.1})"
        );
        failures += 1;
    } else {
        log::info!("iperf3_x16_tx: Overall: TX {tx_us:0.1} ({rx_them:0.1})");
    }

    Ok(failures)
}

pub fn iperf3_x16_rx(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    let stats = iperf3_new_helper(shell, ipaddr, &format!("-i 30 --parallel 16 -R"))?;

    let speed_mbps = adapter_speed(shell, adapter);

    let mut failures = 0;

    // Vendor driver performance (running on 7.0-rc3, worst-of-ten at 2500Mb/s):
    //                  Mb/s        BW      BW * 0.95
    // Per interval     62.1        40%     38%
    // RX overall       1167.6      47%     44%
    let stream_threshold = (speed_mbps * 0.6) / 16.0;
    let rx_threshold = speed_mbps * 0.6;

    for (i, (sender, receiver)) in stats
        .end
        .streams
        .iter()
        .map(|stream| (get_mbps(stream.sender), get_mbps(stream.receiver)))
        .enumerate()
    {
        if sender < stream_threshold || receiver < stream_threshold {
            log::warn!(
                "iperf3_x16_rx: stream #{i}: Network bandwidth is not fairly allocated: Send {sender:0.1}, Recv {receiver:0.1}"
            );
            failures += 1;
        } else {
            log::info!("iperf3_x16_rx: stream #{i}: Send {sender:0.1}, Recv {receiver:0.1}");
        }
    }

    let rx_us = get_mbps(stats.end.sum_received);
    let tx_them = get_mbps(stats.end.sum_sent);

    if rx_us < rx_threshold || tx_them < rx_threshold {
        log::warn!(
            "iperf3_x16_rx: Overall: Network performance is too slow: RX {rx_us:0.1} ({tx_them:0.1})"
        );
        failures += 1;
    } else {
        log::info!("iperf3_x16_rx: Overall: RX {rx_us:0.1} ({tx_them:0.1})");
    }

    Ok(failures)
}

//
// System integration tests
//

pub fn suspend_resume(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    let mut failures = 0;

    if !shell.cmd("ls /sys/power")?.contains("pm_test") {
        log::warn!(
            "suspend_resume: Kernel has been compiled without CONFIG_PM_DEBUG, skipping test"
        );
        return Ok(0);
    }

    shell.cmd("echo core > /sys/power/pm_test")?;
    shell.cmd("systemctl suspend")?;
    // The shell will be unresponsive for around 8 seconds during suspend
    thread::sleep(Duration::from_secs(10));
    shell.cmd("echo none > /sys/power/pm_test")?;
    // Wait for the PHY to come back up. This can be fairly short. We need to
    // wait for the link to come up but there is only a brief interruption to
    // the link so there should be no need for a new DHCP lease.
    thread::sleep(Duration::from_secs(5));
    failures += plans::quick_test(shell, adapter, ipaddr)?;

    Ok(failures)
}

pub fn disable_checksum_offload(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    let mut failures = 0;

    let reply = shell.cmd("ethtool -k enP1p5s0f1 | grep '^[tr]x-checksumming'")?;
    if !reply.contains("tx-checksumming: on") || !reply.contains("rx-checksumming: on") {
        log::error!("disable_checksum_offload: Checksum offloading is not enabled by default");
        failures += 1;
    }

    shell.cmd("ethtool -K enP1p5s0f1 tx off rx off")?;

    // This is like plans::quick_test() but has a very relaxed pass criteria
    // since there's little point in performance tuning with offload disabled
    failures += ping(shell, ipaddr)?;
    let speed = adapter_speed(shell, adapter);
    let (tx_thresh, rx_thresh) = (speed * 0.2, speed * 0.2);
    failures += iperf3_bidir_tuneable(shell, ipaddr, tx_thresh, rx_thresh)?;

    shell.cmd("ethtool -K enP1p5s0f1 tx on rx on")?;
    failures += plans::quick_test(shell, adapter, ipaddr)?;

    Ok(failures)
}

pub fn disable_tso(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    let mut failures = 0;

    let reply = shell.cmd("ethtool -k enP1p5s0f1 | grep '^tcp-segmentation-offload'")?;
    if !reply.contains("tcp-segmentation-offload: on") {
        log::error!("disable_checksum_offload: TSO is not enabled by default");
        failures += 1;
    }

    shell.cmd("ethtool -K enP1p5s0f1 tso off")?;

    // This is like plans::quick_test() but has a very relaxed pass criteria
    // since there's little point in performance tuning with offload disabled
    failures += ping(shell, ipaddr)?;
    let speed = adapter_speed(shell, adapter);
    let (tx_thresh, rx_thresh) = (speed * 0.2, speed * 0.2);
    failures += iperf3_bidir_tuneable(shell, ipaddr, tx_thresh, rx_thresh)?;

    shell.cmd("ethtool -K enP1p5s0f1 tso on")?;
    failures += plans::quick_test(shell, adapter, ipaddr)?;

    Ok(failures)
}

pub fn ethtool_show_eee(shell: &mut impl CommandExecutor, adapter: &str) -> Result<String, Error> {
    let reply = shell.cmd(format!("ethtool --json --show-eee {adapter}"))?;
    let json = serde_json::from_str::<serde_json::Value>(&reply).map_err(|e| {
        log::error!("{e}");
        io::Error::other("Cannot parse JSON from ethtool")
    })?;

    let status = json[0]["status"].as_str().unwrap_or("invalid");
    Ok(status.to_string())
}

pub fn eee(shell: &mut impl CommandExecutor, adapter: &str, ipaddr: &str) -> Result<u32, Error> {
    let mut failures = 0;

    let og_status = ethtool_show_eee(shell, adapter)?;
    if og_status != "active" && og_status != "inactive" {
        log::error!("check_eee: EEE at default speed is {og_status}");
        return Ok(1);
    }

    // If EEE in inactive then let's try check 1000baseT/Full instead
    if og_status == "inactive" {
        ethtool_and_wait_for_adapter_info(shell, adapter, "-s <ADAPTER> advertise 0x20")?;
        let status = ethtool_show_eee(shell, adapter)?;
        if status != "active" {
            log::error!("check_eee: EEE at 1000baseT/Full is {status}");
            ethtool_and_wait_for_adapter_info(
                shell,
                adapter,
                "-s <ADAPTER> advertise 0xffffffffffffffff",
            )?;
            return Ok(1);
        }

        // We *don't* test everything still works here because 1000baseT/Full
        // is tested as part of the PHY auto-negotiation tests so, providing
        // EEE is on by default, we've already done smoke testing in that mode
    }

    // Disable EEE and check is has been acted on.
    //
    // Note that we sometimes get an additional link down/up as our
    // partner reacts to the renegotiation so we end up waiting for the adapter
    // twice.
    ethtool_and_wait_for_adapter_info(shell, adapter, "--set-eee <ADAPTER> eee off")?;
    wait_for_adapter_info(shell, adapter)?;
    let status = ethtool_show_eee(shell, adapter)?;
    if status != "disabled" {
        log::error!("check_eee: EEE is {status} (expected disabled)");
        failures += 1;
    }

    // Check everything still works
    failures += plans::quick_test(shell, adapter, ipaddr)?;

    // Re-enable EEE
    ethtool_and_wait_for_adapter_info(shell, adapter, "--set-eee <ADAPTER> eee on")?;
    let status = ethtool_show_eee(shell, adapter)?;
    if status != "active" {
        log::error!("check_eee: EEE is {status} (expected active)");
        failures += 1;
    }

    // Restore the link speed (if needed)
    if og_status == "inactive" {
        ethtool_and_wait_for_adapter_info(
            shell,
            adapter,
            "-s <ADAPTER> advertise 0xffffffffffffffff",
        )?;
    }

    Ok(failures)
}
