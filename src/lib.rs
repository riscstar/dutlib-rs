use std::io::Read;

use expectrl::{Error, Session, process::NonBlocking};

pub mod dut;
pub mod native;
pub mod plans;
pub mod tests;

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
