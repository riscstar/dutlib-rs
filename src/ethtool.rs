use std::{io, thread, time::Duration};

use expectrl::Error;

use crate::CommandExecutor;

#[derive(Debug, Eq, PartialEq)]
pub struct AdapterInfo {
    pub speed: Option<u32>,
}

/// Gather information about the adapter
pub fn info(shell: &mut impl CommandExecutor, adapter: &str) -> Result<AdapterInfo, Error> {
    let reply = shell.cmd(&format!("ethtool --json {adapter}"))?;
    let json = serde_json::from_str::<serde_json::Value>(&reply).map_err(|e| {
        log::error!("{e}");
        io::Error::other("Cannot parse JSON from ethtool")
    })?;

    let speed = json[0]["speed"].as_i64().map(|s| s as u32);

    Ok(AdapterInfo { speed })
}

/// Get the adapter speed (or 2500.0 is the speed cannot be read for any reason)
pub fn get_speed(shell: &mut impl CommandExecutor, adapter: &str) -> f64 {
    match info(shell, adapter) {
        Ok(AdapterInfo { speed: Some(speed) }) => speed as f64,
        _ => 2500.0,
    }
}

/// Wait for the adapter to detect a link and report the link speed
pub fn wait_link_up(
    shell: &mut impl CommandExecutor,
    adapter: &str,
) -> Result<Option<AdapterInfo>, Error> {
    for _ in 0..10 {
        let info = info(shell, adapter)?;
        if info.speed.is_some() {
            return Ok(Some(info));
        }

        thread::sleep(Duration::from_secs(1));
    }

    Ok(None)
}

/// Helper function to send an ethtool command that provokes phy renegotiation
/// and wait for link up.
pub fn cmd_and_wait_link_up(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    cmd: &str,
) -> Result<Option<AdapterInfo>, Error> {
    shell.cmd(format!("ethtool {cmd}").replace("<ADAPTER>", adapter))?;

    // We assume the above command will cause the PHY to renegotiate. Let's
    // leave a moment for that process to *start* (with a little margin to
    // avoid spamming the logs)
    thread::sleep(Duration::from_secs(3));

    wait_link_up(shell, adapter)
}

/// Helper function to change the advertised modes
pub fn advertise(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    advertisement: u64,
) -> Result<Option<AdapterInfo>, Error> {
    cmd_and_wait_link_up(
        shell,
        adapter,
        &format!("-s <ADAPTER> advertise 0x{advertisement:x}"),
    )
}
