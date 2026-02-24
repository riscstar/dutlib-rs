use std::{io, thread, time::Duration};

use expectrl::Error;

use crate::CommandExecutor;

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

/// Issue 4 pings at 1s intervals, check for packet loss and confirm RTT summary exceeds threshold
pub fn ping(shell: &mut impl CommandExecutor, ipaddr: &str) -> Result<u32, Error> {
    ping_helper("ping", shell, ipaddr, "-c 4")
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

pub fn iperf3_bidir(shell: &mut impl CommandExecutor, ipaddr: &str) -> Result<u32, Error> {
    let (tx, rx) = iperf3_helper(shell, ipaddr, "--bidir")?;
    log::info!("iperf3_bidir: TX is {tx:?}, RX is {rx:?}");

    Ok(
        //if tx[0] < 47.5 || tx[1] < 47.5 || rx[0] < 220.0 || rx[1] < 220.0 {
        if tx[0] < 2000.0 || tx[1] < 2000.0 || rx[0] < 1250.0 || rx[1] < 1250.0 {
            log::warn!("iperf3_bidir: Network performance is too slow: tx {tx:?} rx {rx:?}");
            1
        } else {
            0
        },
    )
}

pub fn iperf3_rx(shell: &mut impl CommandExecutor, ipaddr: &str) -> Result<u32, Error> {
    let bench = iperf3_helper(shell, ipaddr, "-R")?.0;
    log::info!("iperf3_rx: RX is {bench:?}");

    Ok(if bench[0] < 2000.0 || bench[1] < 2000.0 {
        log::warn!("iperf3_rx: Network performance is too slow {bench:?}\n");
        1
    } else {
        0
    })
}

pub fn iperf3_tx(shell: &mut impl CommandExecutor, ipaddr: &str) -> Result<u32, Error> {
    let bench = iperf3_helper(shell, ipaddr, "")?.0;
    log::info!("iperf3_tx: TX performance is {bench:?}");

    Ok(if bench[0] < 2000.0 || bench[1] < 2000.0 {
        log::warn!("iperf3_tx: Network performance is too slow {bench:?}\n");
        1
    } else {
        0
    })
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
            "scp urandom_tx.dat test@{ipaddr}: & scp test@{ipaddr}:urandom_rx.dat . "
        ))?;
        sh.cmd("fg")?;

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
