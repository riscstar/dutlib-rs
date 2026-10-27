use std::{fs, io::Read, path::Path};

use expectrl::{Session, process::NonBlocking};
use serde::{Deserialize, Serialize};

pub mod dut;
pub mod error;
pub mod ethtool;
pub mod ip;
pub mod native;
pub mod plans;
pub mod proc;
pub mod rtc_testbench;
pub mod tests;
pub mod tracker;
pub mod tsn_tests;
pub mod tuning;

pub use error::{RunTestError, UnrecoverableError, UnrecoverableErrorKind};

/// Version string reported by `--version`. It contains the output of
/// `git describe --dirty` (with a leading `v` stripped) followed by the date
/// and time of the described commit. It is generated at build time by
/// `build.rs`.
pub const GIT_VERSION: &str = env!("DUTLIB_GIT_VERSION");

/// The type of connection used by a [`CommandExecutor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandExecutorConnection {
    /// Connection to the device under test.
    Dut,
    /// Connection to the local machine.
    Native,
    /// Connection to the local machine with elevated (sudo) privileges.
    Sudo,
}

impl std::fmt::Display for CommandExecutorConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommandExecutorConnection::Dut => write!(f, "dut"),
            CommandExecutorConnection::Native => write!(f, "native"),
            CommandExecutorConnection::Sudo => write!(f, "sudo"),
        }
    }
}

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
    fn cmd<C: AsRef<str>>(&mut self, cmd: C) -> Result<String, UnrecoverableError>;

    /// Return the type of the session (e.g. "dut", "native", "sudo").
    fn connection(&self) -> CommandExecutorConnection;

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

    /// Flag use to suppress automatic system tuning during test execution.
    #[serde(default)]
    pub no_autotune: bool,
}

pub fn read_config(board: Option<&str>) -> Result<Config, UnrecoverableError> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| UnrecoverableError::message("No HOME in environment"))?;
    let board = board.unwrap_or("rb3gen2");

    let config_file = Path::new(&home)
        .join(".dutlib")
        .join(format!("{board}.toml"));
    let config = fs::read_to_string(config_file)?;
    let config =
        toml::from_str(&config).map_err(|e| UnrecoverableError::message(format!("{e}")))?;

    Ok(config)
}

/// Splits the input string into `N` whitespace-separated fields, with the
/// remainder collected into the `N`-th slot.
///
/// Returns `None` if the input does not contain `N` whitespace-separated fields.
pub fn split_whitespace_remainder<const N: usize>(input: &str) -> Option<[&str; N]> {
    if N == 0 {
        return Some([""; N]);
    }

    let mut rest = input.trim_start();
    let mut array = [""; N];

    // Collect first N - 1 fields
    for slot in &mut array[..N - 1] {
        let word = rest.split_whitespace().next()?;
        *slot = word;
        rest = rest[word.len()..].trim_start();
    }

    // Collect remainder into the N-th slot
    let remainder = rest.trim_end();
    if remainder.is_empty() {
        return None;
    }
    array[N - 1] = remainder;

    Some(array)
}
