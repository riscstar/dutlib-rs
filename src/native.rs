use std::{ffi::OsString, io, process::Command, time::Duration};

use expectrl::{Eof, Error, Expect, Session};

use crate::{CommandExecutor, SessionExt};

pub struct NativeExecutor {
    pub timeout: Option<Duration>,
    available: Option<String>,
}

impl NativeExecutor {
    pub fn new() -> Self {
        Self {
            timeout: None,
            available: None,
        }
    }

    pub fn load_module(&mut self, module_name: &str) -> Result<(), Error> {
        let lsmod = self.cmd("lsmod")?;

        if !lsmod.contains(module_name) {
            // Check for incompatible module state
            if module_name.contains("tc956x") && lsmod.contains("tc956x") {
                return Err(io::Error::other("Incorrect module is already loaded").into());
            }

            self.cmd(format!("modprobe {module_name}"))?;
        }

        Ok(())
    }
}

impl CommandExecutor for NativeExecutor {
    fn cmd<C: AsRef<str>>(&mut self, cmd: C) -> Result<String, Error> {
        log::info!(">>> {}", cmd.as_ref());
        let mut sh = Command::new("bash");
        sh.arg("-c");
        sh.arg(OsString::from(String::from(cmd.as_ref())));

        let mut p = Session::spawn(sh)?;
        if self.timeout.is_some() {
            p.set_expect_timeout(self.timeout);
        }

        let result = p.expect(Eof);
        match result {
            Ok(captures) => {
                let raw = match captures.as_bytes() {
                    bytes if bytes.ends_with(&['\r' as u8, '\n' as u8]) => {
                        &bytes[0..bytes.len() - 2]
                    }
                    bytes => bytes,
                };
                let mut s = String::new();
                for ln in String::from_utf8_lossy(raw).split("\r\n") {
                    log::debug!("  < {}", ln);
                    s.push_str(ln);
                    s.push('\n');
                }
                s.pop(); // remove the trailing '\n' added by the loop

                log::trace!("{s:?}");

                Ok(s)
            }
            Err(e) => {
                self.available = p.try_read_to_string();
                Err(e)
            }
        }
    }

    fn try_read_to_string(&mut self) -> Option<String> {
        self.available.take()
    }

    fn with_timeout_secs<F, R>(&mut self, duration: u64, work: F) -> R
    where
        F: FnOnce(&mut Self) -> R,
    {
        self.timeout = Some(Duration::from_secs(duration));
        let result = work(self);
        self.timeout = None;
        result
    }
}

pub struct SudoExecutor {
    executor: NativeExecutor,
}

impl SudoExecutor {
    pub fn new() -> Self {
        Self {
            executor: NativeExecutor::new(),
        }
    }
}

impl CommandExecutor for SudoExecutor {
    fn cmd<C: AsRef<str>>(&mut self, cmd: C) -> Result<String, Error> {
        let cmd = cmd.as_ref();
        if cmd.starts_with("echo ")
            && let Some((left, right)) = cmd.split_once(" > ")
        {
            // Special-case `echo this > that` into a form that works with sudo
            self.executor
                .cmd(format!("{left} | sudo tee {right} > /dev/null"))
        } else {
            self.executor.cmd(format!("sudo {cmd}"))
        }
    }

    fn try_read_to_string(&mut self) -> Option<String> {
        self.executor.try_read_to_string()
    }

    fn with_timeout_secs<F, R>(&mut self, duration: u64, work: F) -> R
    where
        F: FnOnce(&mut Self) -> R,
    {
        self.executor.timeout = Some(Duration::from_secs(duration));
        let result = work(self);
        self.executor.timeout = None;
        result
    }
}
