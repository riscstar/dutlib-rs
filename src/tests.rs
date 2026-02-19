use std::{io, thread, time::Duration};

use expectrl::{Error, repl::ReplSession, session::OsSession};

use crate::dut::ReplSessionExt;

/// Convenience function to set the a timeout
fn set_timeout_secs(shell: &mut ReplSession<OsSession>, duration: u64) {
    shell
        .get_session_mut()
        .set_expect_timeout(Some(Duration::from_secs(duration)));
}

fn set_timeout_default(shell: &mut ReplSession<OsSession>) {
    shell
        .get_session_mut()
        .set_expect_timeout(Some(Duration::from_secs(10)));
}

/// Wait for the specified IP address to be assigned to the board
pub fn wait_for_ipv4(shell: &mut ReplSession<OsSession>, adapter: &str) -> Result<(), Error> {
    for _ in 0..6 {
        let reply = shell.cmd(&format!("ip -4 addr show {adapter}"))?;

        if reply.contains("inet") {
            return Ok(());
        }

        thread::sleep(Duration::from_secs(5));
    }

    Err(io::Error::other("Timed out waiting for IP address").into())
}

/// Capture basic kernel version management data
pub fn uname(shell: &mut ReplSession<OsSession>) -> Result<String, Error> {
    let reply = shell.cmd(&format!("uname -a"))?;
    log::info!("uname: {reply}");
    Ok(reply)
}

/// Ping an IP address and check that we get a suitable reply
pub fn ping(shell: &mut ReplSession<OsSession>, ipaddr: &str) -> Result<u32, Error> {
    let reply = shell.cmd(&format!("ping -c 4 {ipaddr}"))?;

    Ok(
        if reply.contains("4 received") && reply.contains(" 0% packet loss") {
            0
        } else {
            log::warn!("ping: Could not ping {ipaddr}");
            1
        },
    )
}

fn iperf3_helper(
    shell: &mut ReplSession<OsSession>,
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

pub fn iperf3_bidir(shell: &mut ReplSession<OsSession>, ipaddr: &str) -> Result<u32, Error> {
    let (tx, rx) = iperf3_helper(shell, ipaddr, "--bidir")?;
    log::info!("iperf3_bidir: TX is {tx:?}, RX is {rx:?}");

    Ok(
        //if tx[0] < 47.5 || tx[1] < 47.5 || rx[0] < 220.0 || rx[1] < 220.0 {
        if tx[0] < 2000.0 || tx[1] < 2000.0 || rx[0] < 1400.0 || rx[1] < 1400.0 {
            log::warn!("iperf3_bidir: Network performance is too slow: tx {tx:?} rx {rx:?}");
            1
        } else {
            0
        },
    )
}

pub fn iperf3_rx(shell: &mut ReplSession<OsSession>, ipaddr: &str) -> Result<u32, Error> {
    let bench = iperf3_helper(shell, ipaddr, "-R")?.0;
    log::info!("iperf3_rx: RX is {bench:?}");

    Ok(if bench[0] < 2000.0 || bench[1] < 2000.0 {
        log::warn!("iperf3_rx: Network performance is too slow {bench:?}\n");
        1
    } else {
        0
    })
}

pub fn iperf3_tx(shell: &mut ReplSession<OsSession>, ipaddr: &str) -> Result<u32, Error> {
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
pub fn scp_bidir(shell: &mut ReplSession<OsSession>, ipaddr: &str) -> Result<u32, Error> {
    set_timeout_secs(shell, 30);

    // Generate and checksum the TX data
    shell
        .cmd("dd if=/dev/urandom of=urandom_tx.dat bs=1024 count=$((1024*1024)) status=progress")?;
    let my_sha256sum_tx = shell.cmd("sha256sum urandom_tx.dat")?;

    // Generate and checksum the RX data
    shell
        .cmd(format!("ssh test@{ipaddr} dd if=/dev/urandom of=urandom_rx.dat bs=1024 count=$((1024*1024)) status=progress"))?;
    let their_sha256sum_rx = shell.cmd(format!("ssh test@{ipaddr} sha256sum urandom_rx.dat"))?;

    // Run the transfer
    shell.cmd(format!(
        "scp urandom_tx.dat test@{ipaddr}: & scp test@{ipaddr}:urandom_rx.dat . "
    ))?;
    shell.cmd("fg")?;

    // Collect the remaining checksums
    let my_sha256sum_rx = shell.cmd("sha256sum urandom_rx.dat")?;
    let their_sha256sum_tx = shell.cmd(format!("ssh test@{ipaddr} sha256sum urandom_tx.dat"))?;

    set_timeout_default(shell);

    let mut failures = 0;
    if my_sha256sum_tx != their_sha256sum_tx {
        log::warn!("scp_bidir: TX checksum mismatch: {my_sha256sum_tx} vs {their_sha256sum_tx}");
        failures += 1;
    }
    if my_sha256sum_rx != their_sha256sum_rx {
        log::warn!("scp_bidir: RX checksum mismatch: {my_sha256sum_rx} vs {their_sha256sum_rx}");
        failures += 1;
    }

    Ok(failures)
}

/// Transfer 1GiB of random data from partner to DUT and verify sha256sum
/// matches.
///
/// This test will timeout if run over a link slower than 1g (scp cannot copy
/// a gigabyte in that timeframe)
pub fn scp_rx(shell: &mut ReplSession<OsSession>, ipaddr: &str) -> Result<u32, Error> {
    set_timeout_secs(shell, 30);

    shell
        .cmd(format!("ssh test@{ipaddr} dd if=/dev/urandom of=urandom_rx.dat bs=1024 count=$((1024*1024)) status=progress"))?;
    let their_sha256sum = shell.cmd(format!("ssh test@{ipaddr} sha256sum urandom_rx.dat"))?;
    shell.cmd(format!("scp test@{ipaddr}:urandom_rx.dat ."))?;
    let my_sha256sum = shell.cmd("sha256sum urandom_rx.dat")?;

    set_timeout_default(shell);

    Ok(if my_sha256sum != their_sha256sum {
        log::warn!("scp_rx: Checksum mismatch: {my_sha256sum} vs {their_sha256sum}");
        1
    } else {
        0
    })
}

/// Transfer 1GiB of random data from DUT to partner and verify sha256sum
/// matches.
///
/// This test will timeout if run over a link slower than 1g (scp cannot copy
/// a gigabyte in that timeframe)
pub fn scp_tx(shell: &mut ReplSession<OsSession>, ipaddr: &str) -> Result<u32, Error> {
    set_timeout_secs(shell, 30);

    shell
        .cmd("dd if=/dev/urandom of=urandom_tx.dat bs=1024 count=$((1024*1024)) status=progress")?;
    let my_sha256sum = shell.cmd("sha256sum urandom_tx.dat")?;
    shell.cmd(format!("scp urandom_tx.dat test@{ipaddr}:"))?;
    let their_sha256sum = shell.cmd(format!("ssh test@{ipaddr} sha256sum urandom_tx.dat"))?;

    set_timeout_default(shell);

    Ok(if my_sha256sum != their_sha256sum {
        log::warn!("scp_tx: Checksum mismatch: {my_sha256sum} vs {their_sha256sum}");
        1
    } else {
        0
    })
}
