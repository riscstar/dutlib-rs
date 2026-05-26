use std::{
    fs,
    io::{self, Read},
    path::Path,
};

use expectrl::{Error, Session, process::NonBlocking};
use serde::{Deserialize, Serialize};

pub mod dut;
pub mod ethtool;
pub mod ip;
pub mod native;
pub mod plans;
pub mod rtc_testbench;
pub mod tests;
pub mod tracker;
pub mod tsn_tests;

pub trait CommandExecutor {
    /// Convenience method to run a comment.
    ///
    /// This method is similar to `ReplSession::execute()` but has some
    /// additional conveniences.
    ///
    /// 1. Output can be automatically logged (if log level is set
    ///    appropriately).
    /// 2. Output is cleaned up to remove some escape sequences and to replace
    ///    \r\n with plain \n
    /// 3. Output if returned as a `String`
    fn cmd<C: AsRef<str>>(&mut self, cmd: C) -> Result<String, Error>;

    /// Read all available data and convert to a string.
    fn try_read_to_string(&mut self) -> Option<String>;

    /// Run the supplied closure with a non-standard timeout
    fn with_timeout_secs<F, R>(&mut self, duration: u64, work: F) -> R
    where
        F: FnOnce(&mut Self) -> R;
}

pub trait SessionExt {
    /// Read all available data and convert to a string.
    ///
    /// This is largely useful for debugging timeouts. The concept comes from
    /// https://github.com/zhiburt/expectrl/issues/75#issuecomment-2638198930
    fn try_read_to_string(&mut self) -> Option<String>;
}

impl<P, S: Read + NonBlocking> SessionExt for Session<P, S> {
    fn try_read_to_string(&mut self) -> Option<String> {
        let mut buf = [0; 1024];
        let mut data = Vec::new();
        match self.try_read(&mut buf) {
            Ok(n) if n > 0 => data.extend(&buf[..n]),
            _ => {}
        }

        if data.len() > 0 {
            Some(String::from_utf8_lossy(&data).to_string())
        } else {
            return None;
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    /// IP address to launch tests against. This IP address must respond to ping
    /// and be running both sshd and iperf3.
    pub ipaddr: String,

    /// Name of the network adapter under test.
    pub adapter: String,

    /// Name of the kernel module that must be loaded.
    pub module: String,

    /// Command to connect to the serial port of the device-under-test.
    /// This is mandatory for remote tests and optional (unused) for local tests.
    #[serde(default)]
    pub console: String,

    /// Command to reset the device-under-test if the console becomes
    /// unresponsive. Optional and only required if automatic reboot during
    /// boot cycle testing is needed.
    #[serde(default)]
    pub power_cycle: String,

    /// Name of the network adapter of the link partner. Only used for remote
    /// tests.
    #[serde(default)]
    pub partner_adapter: String,
}

pub fn read_config(board: Option<&str>) -> Result<Config, io::Error> {
    let home =
        std::env::var_os("HOME").ok_or_else(|| io::Error::other("No HOME in environment"))?;
    let board = board.unwrap_or("rb3gen2");

    let config_file = Path::new(&home)
        .join(".dutlib")
        .join(format!("{board}.toml"));
    let config = fs::read_to_string(config_file)?;
    let config = toml::from_str(&config).map_err(|e| io::Error::other(format!("{e}")))?;

    Ok(config)
}
