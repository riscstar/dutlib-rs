use std::{io, thread, time::Duration};

use expectrl::{Error, repl::ReplSession, session::OsSession};

use crate::ReplSessionExt;

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

    Ok(
        if tx[0] < 47.5 || tx[1] < 47.5 || rx[0] < 220.0 || rx[1] < 220.0 {
            log::warn!("iperf3_bidir: Network performance is too slow: tx {tx:?} rx {rx:?}");
            1
        } else {
            0
        },
    )
}

pub fn iperf3_rx(shell: &mut ReplSession<OsSession>, ipaddr: &str) -> Result<u32, Error> {
    let bench = iperf3_helper(shell, ipaddr, "-R")?.0;

    Ok(if bench[0] < 250.0 || bench[1] < 250.0 {
        log::warn!("iperf3_rx: Network performance is too slow {bench:?}\n");
        1
    } else {
        0
    })
}

pub fn iperf3_tx(shell: &mut ReplSession<OsSession>, ipaddr: &str) -> Result<u32, Error> {
    let bench = iperf3_helper(shell, ipaddr, "")?.0;

    Ok(if bench[0] < 140.0 || bench[1] < 140.0 {
        log::warn!("iperf3_tx: Network performance is too slow {bench:?}\n");
        1
    } else {
        0
    })
}
