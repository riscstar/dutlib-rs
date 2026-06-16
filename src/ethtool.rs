use std::{io, thread, time::Duration};

use expectrl::Error;

use crate::{CommandExecutor, tracker::UndoTracker};

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

    wait_link_up(shell, adapter)?;

    // This is a workaround to link "ping/pong" seen with I226 adapters
    thread::sleep(Duration::from_secs(2));
    wait_link_up(shell, adapter)
}

/// Helper function to change the advertised modes
pub fn advertise(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    advertisement: u64,
    undo: Option<&mut UndoTracker>,
) -> Result<Option<AdapterInfo>, Error> {
    let info = cmd_and_wait_link_up(
        shell,
        adapter,
        &format!("-s {adapter} advertise 0x{advertisement:x}"),
    )?;

    if let Some(undo) = undo {
        undo.add(format!(
            "ethtool -s {adapter} advertise 0x{:x} && sleep 3",
            u64::MAX
        ));
    }

    Ok(info)
}

/// Helper to check for feature enable/disable
pub fn show_feature(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    feature: &str,
) -> Result<bool, Error> {
    let reply = shell.cmd(&format!("ethtool --json --show-features {adapter}"))?;
    let json = serde_json::from_str::<serde_json::Value>(&reply).map_err(|e| {
        log::error!("{e}");
        io::Error::other("Cannot parse JSON from ethtool")
    })?;

    let Some(active) = json[0][feature]["active"].as_bool() else {
        return Err(io::Error::other(format!("Cannot lookup ethtool {feature} feature")).into());
    };

    Ok(active)
}

/// Helper to turn features on and off
pub fn feature(
    shell: &mut impl CommandExecutor,
    undo: &mut UndoTracker,
    adapter: &str,
    feature: &str,
    state: bool,
) -> Result<(), Error> {
    let active = show_feature(shell, adapter, feature)?;

    let _ = shell.cmd(&format!(
        "ethtool --features {adapter} {feature} {}",
        if state { "on" } else { "off" }
    ))?;

    undo.add(format!(
        "ethtool --features {adapter} {feature} {}",
        if active { "on" } else { "off" }
    ));

    Ok(())
}

#[derive(Debug, Clone)]
pub struct Statistics {
    pub rx_packet_count: [u64; 8],
    pub tx_packet_count: [u64; 8],
}

impl Statistics {
    pub fn new() -> Self {
        Self {
            rx_packet_count: [0; 8],
            tx_packet_count: [0; 8],
        }
    }

    pub fn normalize(&self) -> Self {
        let rx_total = self.rx_packet_count.iter().sum::<u64>();
        let tx_total = self.tx_packet_count.iter().sum::<u64>();

        let mut v = self.clone();

        if rx_total != 0 {
            v.rx_packet_count
                .iter_mut()
                .for_each(|c| *c = *c * 1000000 / rx_total);
        }
        if tx_total != 0 {
            v.tx_packet_count
                .iter_mut()
                .for_each(|c| *c = *c * 1000000 / tx_total);
        }

        v
    }
}

impl std::ops::Add for Statistics {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        let mut rx = [0; 8];
        for i in 0..8 {
            rx[i] = self.rx_packet_count[i].saturating_add(rhs.rx_packet_count[i]);
        }
        let mut tx = [0; 8];
        for i in 0..8 {
            tx[i] = self.tx_packet_count[i].saturating_add(rhs.tx_packet_count[i]);
        }
        Self {
            rx_packet_count: rx,
            tx_packet_count: tx,
        }
    }
}

impl std::ops::Sub for Statistics {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        let mut rx = [0; 8];
        for i in 0..8 {
            rx[i] = self.rx_packet_count[i].saturating_sub(rhs.rx_packet_count[i]);
        }
        let mut tx = [0; 8];
        for i in 0..8 {
            tx[i] = self.tx_packet_count[i].saturating_sub(rhs.tx_packet_count[i]);
        }
        Self {
            rx_packet_count: rx,
            tx_packet_count: tx,
        }
    }
}

pub fn statistics(shell: &mut impl CommandExecutor, adapter: &str) -> Result<Statistics, Error> {
    let mut stats = Statistics::new();

    // No --json for ethtool statistics
    let reply = shell.cmd(&format!("ethtool --statistics {adapter}"))?;
    let lines = reply.lines();
    for line in lines {
        let Some((key, value)) = line.trim().split_once(':') else {
            continue;
        };
        let value = value.trim().parse::<u64>();
        if let Ok(value) = value {
            match key {
                // stmmac statistics
                "q0_tx_pkt_n" => stats.tx_packet_count[0] = value,
                "q1_tx_pkt_n" => stats.tx_packet_count[1] = value,
                "q2_tx_pkt_n" => stats.tx_packet_count[2] = value,
                "q3_tx_pkt_n" => stats.tx_packet_count[3] = value,
                "q4_tx_pkt_n" => stats.tx_packet_count[4] = value,
                "q5_tx_pkt_n" => stats.tx_packet_count[5] = value,
                "q6_tx_pkt_n" => stats.tx_packet_count[6] = value,
                "q7_tx_pkt_n" => stats.tx_packet_count[7] = value,
                "q0_rx_pkt_n" => stats.rx_packet_count[0] = value,
                "q1_rx_pkt_n" => stats.rx_packet_count[1] = value,
                "q2_rx_pkt_n" => stats.rx_packet_count[2] = value,
                "q3_rx_pkt_n" => stats.rx_packet_count[3] = value,
                "q4_rx_pkt_n" => stats.rx_packet_count[4] = value,
                "q5_rx_pkt_n" => stats.rx_packet_count[5] = value,
                "q6_rx_pkt_n" => stats.rx_packet_count[6] = value,
                "q7_rx_pkt_n" => stats.rx_packet_count[7] = value,
                _ => {}
            }
        }
    }

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_statistics_add() {
        let mut s1 = Statistics::new();
        let mut s2 = Statistics::new();

        s1.rx_packet_count[0] = 100;
        s2.rx_packet_count[0] = 80;

        let sum = s1 + s2;

        assert_eq!(sum.rx_packet_count[0], 180);
        assert_eq!(sum.rx_packet_count[1], 0);
    }

    #[test]
    fn test_statistics_sub() {
        let mut s1 = Statistics::new();
        let mut s2 = Statistics::new();

        s1.rx_packet_count[0] = 100;
        s2.rx_packet_count[0] = 80;

        s1.tx_packet_count[1] = 50;
        s2.tx_packet_count[1] = 60; // Larger than s1, test saturation

        let diff = s1 - s2;

        assert_eq!(diff.rx_packet_count[0], 20);
        assert_eq!(diff.tx_packet_count[1], 0); // Should saturate to 0
        assert_eq!(diff.rx_packet_count[1], 0);
        assert_eq!(diff.tx_packet_count[0], 0);
    }

    #[test]
    fn test_statistics_normalize() {
        let mut s = Statistics::new();
        s.rx_packet_count[0] = 50;
        s.rx_packet_count[1] = 50;
        s.tx_packet_count[0] = 100;

        let n = s.normalize();

        assert_eq!(n.rx_packet_count[0], 500000);
        assert_eq!(n.rx_packet_count[1], 500000);
        assert_eq!(n.tx_packet_count[0], 1000000);
    }
}
