use std::{thread, time::Duration};

use expectrl::Error;

use crate::{
    CommandExecutor,
    ethtool::{self, AdapterInfo},
};

/// Helper function to run an `ip` command. This is pretty trivial but aligns
/// nicely with `cmd_and_wait_link_up`.
pub fn cmd(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    command: &str,
) -> Result<String, Error> {
    shell.cmd(format!("ip {command}").replace("<ADAPTER>", adapter))
}

/// Helper function to send an ethtool command that provokes phy renegotiation
/// and wait for link up.
pub fn cmd_and_wait_link_up(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    command: &str,
) -> Result<Option<AdapterInfo>, Error> {
    cmd(shell, adapter, command)?;

    // We assume the above command will cause the PHY to renegotiate. Let's
    // leave a moment for that process to *start* (with a little margin to
    // avoid spamming the logs)
    thread::sleep(Duration::from_secs(3));

    ethtool::wait_link_up(shell, adapter)
}
