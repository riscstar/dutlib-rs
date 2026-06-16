use std::{io, thread, time::Duration};

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
    shell.cmd(format!("ip -color=never {command}").replace("<ADAPTER>", adapter))
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

/// Helper function to lookup our MAC address
pub fn mac_address(shell: &mut impl CommandExecutor, adapter: &str) -> Result<String, Error> {
    let reply = cmd(shell, adapter, "link show <ADAPTER>")?;

    let mut trigger = false;
    for word in reply.split_whitespace() {
        if trigger {
            return Ok(word.to_string());
        }
        if word == "link/ether" {
            // We found "link/ether" so MAC address will be the next word
            trigger = true;
        }
    }

    Err(io::Error::other("Unexpected output from `ip link`").into())
}

/// Helper function to lookup our MAC address
pub fn ipv4_address(shell: &mut impl CommandExecutor, adapter: &str) -> Result<String, Error> {
    let reply = cmd(shell, adapter, "-4 addr show <ADAPTER>")?;

    let mut trigger = false;
    for word in reply.split_whitespace() {
        if trigger {
            if let Some((addr, _)) = word.split_once("/") {
                return Ok(addr.to_string());
            }
            break;
        }
        if word == "inet" {
            // We found "inet" so IP address will be the next word
            trigger = true;
        }
    }

    Err(io::Error::other("Unexpected output from `ip addr`").into())
}
