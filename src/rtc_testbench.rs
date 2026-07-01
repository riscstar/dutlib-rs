use std::{
    collections::BTreeMap,
    fmt,
    fs::File,
    io::{self, BufWriter},
    path::Path,
};

use expectrl::Error;
use serde::{Deserialize, Serialize};

type YamlValue = serde_yaml::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    #[serde(rename = "Application")]
    pub application: Application,
    #[serde(rename = "RTC")]
    pub rtc: YamlValue,
    #[serde(rename = "RTA")]
    pub rta: YamlValue,
    #[serde(rename = "DCP")]
    pub dcp: YamlValue,
    #[serde(rename = "LLDP")]
    pub lldp: YamlValue,
    #[serde(rename = "UDPHigh")]
    pub udp_high: YamlValue,
    #[serde(rename = "UDPLow")]
    pub udp_low: YamlValue,
    #[serde(rename = "Log")]
    pub log: Log,
    #[serde(rename = "Debug")]
    pub debug: Debug,
}

impl Config {
    pub fn to_file(&self, path: impl AsRef<Path>) -> Result<(), Error> {
        let file = File::create(path)?;
        let writer = BufWriter::new(file);
        serde_yaml::to_writer(writer, self)
            .map_err(|_| io::Error::other("Failed to write YAML file"))?;

        Ok(())
    }
}

impl ToString for Config {
    fn to_string(&self) -> String {
        serde_yaml::to_string(self).unwrap_or("# Failed to serialize YAML file".to_string())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Application {
    pub application_clock_id: String,
    #[serde(rename = "ApplicationBaseCycleTimeNS")]
    pub application_base_cycle_time_ns: String,
    #[serde(rename = "ApplicationTxBaseOffsetNS")]
    pub application_tx_base_offset_ns: String,
    #[serde(rename = "ApplicationRxBaseOffsetNS")]
    pub application_rx_base_offset_ns: String,
    pub application_xdp_program: String,
}

fn is_zero(num: &u16) -> bool {
    *num == 0
}

#[derive(Clone, Debug)]
pub struct TrafficContext {
    pub reference_interface: String,
    pub reference_ip: String,
    pub reference_mac: String,

    pub mirror_interface: String,
    pub mirror_ip: String,
    pub mirror_mac: String,
}

#[derive(Clone, Debug)]
pub enum TrafficClass {
    Rtc(TrafficConfig),
    Rta(TrafficConfig),
    Dcp(TrafficConfig),
    Lldp(TrafficConfig),
    UdpHigh(TrafficConfig),
    UdpLow(TrafficConfig),
}

impl TrafficClass {
    pub fn rtc() -> Self {
        TrafficClass::Rtc(TrafficConfig {
            tx_timestamp_enabled: Some(false),
            payload_pattern: "RtcPayloadPattern\n".to_string(),
            ..TrafficConfig::new()
        })
    }

    pub fn rta() -> Self {
        TrafficClass::Rta(TrafficConfig {
            tx_timestamp_enabled: Some(false),
            payload_pattern: "RtaPayloadPattern\n".to_string(),
            ..TrafficConfig::new()
        })
    }

    pub fn dcp() -> Self {
        TrafficClass::Dcp(TrafficConfig {
            payload_pattern: "DcpPayloadPattern\n".to_string(),
            ..TrafficConfig::new()
        })
    }

    pub fn lldp() -> Self {
        TrafficClass::Lldp(TrafficConfig {
            payload_pattern: "LldpPayloadPattern\n".to_string(),
            ..TrafficConfig::new()
        })
    }

    pub fn udp_high() -> Self {
        TrafficClass::UdpHigh(TrafficConfig {
            payload_pattern: "UdpHighPayloadPattern\n".to_string(),
            ..TrafficConfig::new()
        })
    }

    pub fn udp_low() -> Self {
        TrafficClass::UdpLow(TrafficConfig {
            payload_pattern: "UdpLowPayloadPattern\n".to_string(),
            ..TrafficConfig::new()
        })
    }

    pub fn inner_mut(&mut self) -> &mut TrafficConfig {
        match self {
            TrafficClass::Rtc(config)
            | TrafficClass::Rta(config)
            | TrafficClass::Dcp(config)
            | TrafficClass::Lldp(config)
            | TrafficClass::UdpHigh(config)
            | TrafficClass::UdpLow(config) => config,
        }
    }

    pub fn disable(mut self) -> Self {
        let config = self.inner_mut();
        config.enabled = false;
        self
    }

    pub fn set_xdp(mut self, skb_mode: bool, zc_mode: bool, wakeup_mode: bool) -> Self {
        let config = self.inner_mut();
        config.xdp_enabled = Some(true);
        config.xdp_skb_mode = Some(skb_mode);
        config.xdp_zc_mode = Some(zc_mode);
        config.xdp_wakeup_mode = Some(wakeup_mode);
        self
    }

    #[allow(unused)]
    pub fn set_tx_timestamp(mut self) -> Self {
        let config = self.inner_mut();
        config.tx_timestamp_enabled = Some(true);
        self
    }

    pub fn set_vid(mut self, vid: u16) -> Self {
        let config = self.inner_mut();
        config.vid = Some(vid);
        self
    }

    pub fn set_burst_period_ns(mut self, burst_period_ns: impl ToString) -> Self {
        let config = self.inner_mut();
        config.burst_period_ns = Some(burst_period_ns.to_string());
        self
    }

    pub fn set_frame_count_and_length(
        mut self,
        num_frames_per_cycle: u32,
        frame_length: u32,
    ) -> Self {
        let config = self.inner_mut();
        config.num_frames_per_cycle = num_frames_per_cycle;
        config.frame_length = frame_length;
        self
    }

    pub fn set_txrx_queue(mut self, queue: u32) -> Self {
        let config = self.inner_mut();
        config.rx_queue = queue;
        config.tx_queue = queue;
        self
    }

    pub fn set_socket_priority(mut self, socket_priority: u32) -> Self {
        let config = self.inner_mut();
        config.socket_priority = socket_priority;
        self
    }

    pub fn set_thread_cpu_and_priority(mut self, cpu: u32, priority: u32) -> Self {
        let config = self.inner_mut();
        config.tx_thread_cpu = cpu;
        config.rx_thread_cpu = cpu;
        config.tx_thread_priority = priority;
        config.rx_thread_priority = priority;
        self
    }

    pub fn set_port(mut self, port: u16) -> Self {
        let config = self.inner_mut();
        config.port = port;
        self
    }

    pub fn set_destination(mut self, destination: impl ToString) -> Self {
        let config = self.inner_mut();
        config.destination = destination.to_string();
        self
    }

    pub fn to_value(self) -> serde_yaml::Value {
        match self {
            TrafficClass::Rtc(config) => config.to_value("Rtc"),
            TrafficClass::Rta(config) => config.to_value("Rta"),
            TrafficClass::Dcp(config) => config.to_value("Dcp"),
            TrafficClass::Lldp(config) => config.to_value("Lldp"),
            TrafficClass::UdpHigh(config) => config.to_value("UdpHigh"),
            TrafficClass::UdpLow(config) => config.to_value("UdpLow"),
        }
    }

    pub fn with_reference(self, context: &TrafficContext) -> Self {
        match self {
            TrafficClass::Rtc(mut config) => {
                config.interface = context.reference_interface.clone();
                config.destination = context.mirror_mac.clone();
                TrafficClass::Rtc(config)
            }
            TrafficClass::Rta(mut config) => {
                config.interface = context.reference_interface.clone();
                config.destination = context.mirror_mac.clone();
                TrafficClass::Rta(config)
            }
            TrafficClass::Dcp(mut config) => {
                config.interface = context.reference_interface.clone();
                TrafficClass::Dcp(config)
            }
            TrafficClass::Lldp(mut config) => {
                config.interface = context.reference_interface.clone();
                TrafficClass::Lldp(config)
            }
            TrafficClass::UdpHigh(mut config) => {
                config.interface = context.reference_interface.clone();
                config.destination = context.mirror_ip.clone();
                config.source = context.reference_ip.clone();
                TrafficClass::UdpHigh(config)
            }
            TrafficClass::UdpLow(mut config) => {
                config.interface = context.reference_interface.clone();
                config.destination = context.mirror_ip.clone();
                config.source = context.reference_ip.clone();
                TrafficClass::UdpLow(config)
            }
        }
    }

    pub fn with_mirror(self, context: &TrafficContext) -> Self {
        match self {
            TrafficClass::Rtc(mut config) => {
                config.interface = context.mirror_interface.clone();
                config.destination = context.reference_mac.clone();
                TrafficClass::Rtc(config)
            }
            TrafficClass::Rta(mut config) => {
                config.interface = context.mirror_interface.clone();
                config.destination = context.reference_mac.clone();
                TrafficClass::Rta(config)
            }
            TrafficClass::Dcp(mut config) => {
                config.interface = context.mirror_interface.clone();
                config.destination = context.reference_mac.clone();
                TrafficClass::Dcp(config)
            }
            TrafficClass::Lldp(mut config) => {
                config.interface = context.mirror_interface.clone();
                TrafficClass::Lldp(config)
            }
            TrafficClass::UdpHigh(mut config) => {
                config.interface = context.mirror_interface.clone();
                config.destination = context.reference_ip.clone();
                config.source = context.mirror_ip.clone();
                TrafficClass::UdpHigh(config)
            }
            TrafficClass::UdpLow(mut config) => {
                config.interface = context.mirror_interface.clone();
                config.destination = context.reference_ip.clone();
                config.source = context.mirror_ip.clone();
                TrafficClass::UdpLow(config)
            }
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct TrafficConfig {
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    xdp_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    xdp_skb_mode: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    xdp_zc_mode: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    xdp_wakeup_mode: Option<bool>,
    #[serde(rename = "TxTimeStampEnabled", skip_serializing_if = "Option::is_none")]
    tx_timestamp_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    vid: Option<u16>,
    #[serde(rename = "BurstPeriodNS", skip_serializing_if = "Option::is_none")]
    burst_period_ns: Option<String>,
    num_frames_per_cycle: u32,
    payload_pattern: String,
    frame_length: u32,
    rx_queue: u32,
    tx_queue: u32,
    socket_priority: u32,
    tx_thread_priority: u32,
    rx_thread_priority: u32,
    tx_thread_cpu: u32,
    rx_thread_cpu: u32,
    interface: String,
    #[serde(skip_serializing_if = "is_zero")]
    port: u16,
    destination: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    source: String,
}

impl TrafficConfig {
    fn new() -> Self {
        Self {
            enabled: true,
            ..TrafficConfig::default()
        }
    }

    pub fn to_value(self, prefix: &str) -> serde_yaml::Value {
        if !self.enabled {
            return serde_yaml::from_str(&format!("{prefix}Enabled: false")).unwrap();
        }

        match serde_yaml::to_value(self).expect("TrafficClass is not YAML compatible") {
            serde_yaml::Value::Mapping(map) => {
                // Transform the mapping using an iterator pipeline
                let new_map: serde_yaml::Mapping = map
                    .into_iter()
                    .map(|(key, value)| {
                        let new_key = match key {
                            serde_yaml::Value::String(key_str) => {
                                serde_yaml::Value::String(format!("{}{}", prefix, key_str))
                            }
                            other => other,
                        };
                        // Return the new (key, value) tuple for collection
                        (new_key, value)
                    })
                    .collect();

                serde_yaml::Value::Mapping(new_map)
            }
            // If it's not a mapping, return it unmodified
            other => other,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Log {
    pub log_thread_priority: u32,
    pub log_thread_cpu: u32,
    pub log_file: String,
    pub log_level: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Debug {
    pub debug_stop_trace_on_outlier: bool,
    pub debug_stop_trace_on_error: bool,
    pub debug_monitor_mode: bool,
    pub debug_monitor_destination: String,
}

#[derive(Clone, Debug, Default)]
pub struct TrafficStats {
    pub sent: u64,
    pub received: u64,
    pub rtt_min_us: u64,
    pub rtt_max_us: u64,
    pub rtt_avg_us: f64,
    pub one_way_min_us: u64,
    pub one_way_max_us: u64,
    pub one_way_avg_us: f64,
    pub rtt_outliers: u64,
    pub one_way_outliers: u64,
}

impl fmt::Display for TrafficStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Limit threshold
        const LIMIT: u64 = 99999;

        // Helper to format u64 microsecond values
        let fmt_us_u64 = |val: u64| -> String {
            if val > LIMIT {
                "#####".to_string()
            } else {
                val.to_string()
            }
        };

        // Helper to format f64 microsecond values
        let fmt_us_f64 = |val: f64| -> String {
            if val > LIMIT as f64 {
                "#####.#".to_string()
            } else {
                // Formats to 2 decimal places for clean viewing
                format!("{:.1}", val)
            }
        };

        // Write the formatted output
        write!(
            f,
            "TxRx {:7}/{:<7} RTT {:>5}/{:>7}/{:>5} OneWay {:>5}/{:>7}/{:>5} Out {:7}/{:<7}",
            self.sent,
            self.received,
            fmt_us_u64(self.rtt_min_us),
            fmt_us_f64(self.rtt_avg_us),
            fmt_us_u64(self.rtt_max_us),
            fmt_us_u64(self.one_way_min_us),
            fmt_us_f64(self.one_way_avg_us),
            fmt_us_u64(self.one_way_max_us),
            self.rtt_outliers,
            self.one_way_outliers
        )
    }
}

pub fn parse_log_line(line: impl AsRef<str>) -> Option<BTreeMap<String, TrafficStats>> {
    let (_, info) = line.as_ref().split_once(':')?;
    let (_, fields) = info.split_once(':')?;

    let mut map = BTreeMap::new();

    for field in fields.split('|') {
        let field = field.trim();
        let field = if field.ends_with(" [us]") {
            &field[0..field.len() - 5]
        } else {
            field
        };

        if field.len() == 0 {
            continue;
        }

        let (name, value) = field.split_once('=')?;
        if let Some((tag, _)) = name.split_once("Sent") {
            // Sent is always first so we can create a new structure
            let stats = TrafficStats {
                sent: value.parse().ok()?,
                ..TrafficStats::default()
            };
            if map.insert(tag.into(), stats).is_some() {
                return None;
            }
        } else if let Some((tag, _)) = name.split_once("Received") {
            map.get_mut(tag)?.received = value.parse().ok()?;
        } else if let Some((tag, _)) = name.split_once("RttMin") {
            map.get_mut(tag)?.rtt_min_us = value.parse().ok()?;
        } else if let Some((tag, _)) = name.split_once("RttMax") {
            map.get_mut(tag)?.rtt_max_us = value.parse().ok()?;
        } else if let Some((tag, _)) = name.split_once("RttAvg") {
            map.get_mut(tag)?.rtt_avg_us = value.parse().ok()?;
        } else if let Some((tag, _)) = name.split_once("OnewayMin") {
            map.get_mut(tag)?.one_way_min_us = value.parse().ok()?;
        } else if let Some((tag, _)) = name.split_once("OnewayMax") {
            map.get_mut(tag)?.one_way_max_us = value.parse().ok()?;
        } else if let Some((tag, _)) = name.split_once("OnewayAvg") {
            map.get_mut(tag)?.one_way_avg_us = value.parse().ok()?;
        } else if let Some((tag, _)) = name.split_once("RttOutliers") {
            map.get_mut(tag)?.rtt_outliers = value.parse().ok()?;
        } else if let Some((tag, _)) = name.split_once("OnewayOutliers") {
            map.get_mut(tag)?.one_way_outliers = value.parse().ok()?;
        }
    }

    Some(map)
}

pub fn log_traffic_stats(stats: &Option<BTreeMap<String, TrafficStats>>) {
    if let Some(stats) = stats {
        let keys = ["Rtc", "Rta", "Dcp", "Lldp", "UdpHigh", "UdpLow"];
        for k in keys {
            if let Some(v) = stats.get(k) {
                log::info!("{k:7}  {v}");
            }
        }

        for (k, v) in stats.iter() {
            if !keys.contains(&k.as_str()) {
                log::info!("{k:7}  {v}");
            }
        }
    } else {
        log::warn!("Failed to parse traffic stats");
    }
}

#[cfg(test)]
mod tests {
    // Note this useful idiom: importing names from outer (for mod tests) scope.
    use super::*;

    #[test]
    fn test_parse_log_line() {
        let example = "[1779189233.471340708]: [INFO]: RtcSent=928736 | RtcReceived=1 | RtcRttMin=18446744073709551615 [us] | RtcRttMax=0 [us] | RtcRttAvg=0.000000 [us] | RtcOnewayMin=18446744073709551615 [us] | RtcOnewayMax=0 [us] | RtcOnewayAvg=0.000000 [us] | RtcRttOutliers=0 | RtcOnewayOutliers=0 | RtaSent=4640 | RtaReceived=0 | RtaRttMin=18446744073709551615 [us] | RtaRttMax=0 [us] | RtaRttAvg=0.000000 [us] | RtaOnewayMin=18446744073709551615 [us] | RtaOnewayMax=0 [us] | RtaOnewayAvg=0.000000 [us] | DcpSent=14 | DcpReceived=14 | DcpRttMin=2071 [us] | DcpRttMax=2997 [us] | DcpRttAvg=2204.285714 [us] | DcpOnewayMin=18446744073252868 [us] | DcpOnewayMax=18446744073252934 [us] | DcpOnewayAvg=18446744073252888.000000 [us] | LldpSent=5 | LldpReceived=5 | LldpRttMin=2122 [us] | LldpRttMax=3109 [us] | LldpRttAvg=2699.200000 [us] | LldpOnewayMin=18446744073252895 [us] | LldpOnewayMax=18446744073252930 [us] | LldpOnewayAvg=18446744073252908.000000 [us] | UdpHighSent=29 | UdpHighReceived=22 | UdpHighRttMin=2840 [us] | UdpHighRttMax=1003180 [us] | UdpHighRttAvg=321154.045455 [us] | UdpHighOnewayMin=18446744073252705 [us] | UdpHighOnewayMax=18446744073252882 [us] | UdpHighOnewayAvg=18446744073252812.000000 [us] | UdpLowSent=29 | UdpLowReceived=29 | UdpLowRttMin=1720 [us] | UdpLowRttMax=2115 [us] | UdpLowRttAvg=1937.551724 [us] | UdpLowOnewayMin=18446744073252355 [us] | UdpLowOnewayMax=18446744073252870 [us] | UdpLowOnewayAvg=18446744073252656.000000 [us] |";

        let stats = parse_log_line(example).unwrap();
        dbg!(&stats);

        assert_eq!(stats["Rtc"].sent, 928736);
        assert_eq!(stats["Rtc"].received, 1);
        assert_eq!(stats["UdpLow"].one_way_avg_us, 18446744073252656.000000);
    }
}
