use expectrl::{
    Error, Expect, Session,
    repl::ReplSession,
    session::{OsProcess, OsSession, OsStream},
    stream::log::LogStream,
};
use std::{
    io::{self, Stdout},
    process::Command,
    thread,
    time::Duration,
};

use crate::{CommandExecutor, SessionExt};

impl CommandExecutor for ReplSession<OsSession> {
    fn cmd<C: AsRef<str>>(&mut self, cmd: C) -> Result<String, Error> {
        log::info!(">>> {}", cmd.as_ref());
        let reply = if cmd.as_ref().contains('\n') {
            // Newlines play havoc with the echo suppression. Let's just
            // disable it and leave the echo for the caller to handle
            self.set_echo(false);
            let reply = self.execute(cmd);
            self.set_echo(true);
            reply
        } else {
            self.execute(cmd)
        };
        reply.map(|raw| {
            let mut s = String::new();

            if raw.len() > 21 {
                // TODO: We need more substantial filtering here. In particular
                //       we need to get rid of escape sequences. Currently we
                //       just blindly remove characters at the beginning and end
                //       to make the logs look good.
                let mut first_line = true;
                for ln in String::from_utf8_lossy(&raw[11..raw.len() - 10]).split("\r\n") {
                    log::debug!("  < {}", ln);
                    if first_line {
                        first_line = false;
                    } else {
                        s.push('\n');
                    }
                    s.push_str(ln);
                }
            }

            log::trace!("{s:?}");

            s
        })
    }

    fn try_read_to_string(&mut self) -> Option<String> {
        self.get_session_mut().try_read_to_string()
    }

    /// Convenience function to set the a timeout
    fn with_timeout_secs<F, R>(&mut self, duration: u64, work: F) -> R
    where
        F: FnOnce(&mut Self) -> R,
    {
        self.get_session_mut()
            .set_expect_timeout(Some(Duration::from_secs(duration)));

        let result = work(self);

        self.get_session_mut()
            .set_expect_timeout(Some(Duration::from_secs(10)));

        result
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum MachineState {
    Booted,
    Crashed,
    Rebooting,
}

pub type OsLogSession = Session<OsProcess, LogStream<OsStream, Stdout>>;

pub struct DeviceUnderTest {
    state: MachineState,
    console: String,
    power_cycle: String,
}

impl DeviceUnderTest {
    pub fn new(console: &str, power_cycle: &str) -> Self {
        Self {
            state: MachineState::Booted,
            console: console.to_string(),
            power_cycle: power_cycle.to_string(),
        }
    }

    pub fn console(&mut self) -> Result<ReplSession<OsSession>, Error> {
        log::info!("Connecting to console ({:?})", self.state);
        if self.state == MachineState::Crashed {
            let mut iter = self.power_cycle.split_ascii_whitespace();
            if let Some(first) = iter.next() {
                let mut cmd = Command::new(first);
                cmd.args(iter);
                let output = cmd.output()?;
                log::debug!("Power-cycle: {output:?}");
            } else {
                return Err(
                    io::Error::other("TOML configuration error: power_cycle is not set").into(),
                );
            };
        }

        let mut iter = self.console.split_ascii_whitespace();
        let mut uart = if let Some(first) = iter.next() {
            let mut cmd = Command::new(first);
            cmd.args(iter);
            Session::spawn(cmd)?
        } else {
            return Err(io::Error::other("TOML configuration error: console is not set").into());
        };

        if self.console.contains("picocom") {
            // Error handling will try to reboot the target if possible but also
            // implements direct exit for permanent failures.
            if let Err(err) = uart.expect("Terminal ready") {
                let remaining = uart.try_read_to_string();
                if remaining
                    .as_deref()
                    .unwrap_or("")
                    .contains("Resource temporarily unavailable")
                {
                    log::debug!("{:?}", remaining);
                    log::error!("Terminal emulator cannot start (port busy?)");
                    return Err(err);
                }

                if remaining.as_deref().unwrap_or("").contains("password: ") {
                    log::debug!("{:?}", remaining);
                    log::warn!("Received password prompt (missing ssh credentials?");
                    return Err(err);
                }

                log::debug!("{:?}", remaining);
                log::warn!("Cannot connect to target");
                return Err(err);
            }
        }

        match self.state {
            MachineState::Booted => {
                // nothing to do
            }
            MachineState::Rebooting => {
                log::debug!("Waiting for getty to start");
                thread::sleep(Duration::from_secs(40));
            }
            MachineState::Crashed => {
                // we already waited for the getty to start are part of the
                // iot-power-cycle command
            }
        }

        // Probe to see if a shell is present
        uart.send_line("")?;
        uart.send_line("echo sync ch''eck")?;
        match uart.expect("sync check") {
            Ok(_) => {
                self.state = MachineState::Booted;
            }
            Err(Error::ExpectTimeout) => {
                log::debug!("{:?}", uart.try_read_to_string());
                log::warn!("Timed out during synchronization check");
                return if self.state == MachineState::Crashed {
                    Err(Error::ExpectTimeout)
                } else {
                    self.crashed(uart);
                    self.console()
                };
            }
            Err(err) => {
                log::debug!("{:?}", uart.try_read_to_string());
                return Err(err);
            }
        }

        // Make sure we are in the user's home directory
        uart.send_line("cd")?;

        // Kernel messages can make the REPL management unreliable (since they
        // can interfere with the prompt). Let's turn them off.
        uart.send_line("echo 1 > /proc/sys/kernel/printk")?;

        // We need a TERM value that doesn't inject junk into the echoed
        // characters
        uart.send_line("TERM=vt102; export TERM")?;

        // Let's set our own prompt
        uart.send_line("PS1=\"REPLSESSION# \"")?;

        uart.send_line("echo resync ch''eck")?;
        uart.expect("resync check")?;

        let mut shell = ReplSession::new(uart, "REPLSESSION# ");
        shell.set_echo(self.console.contains("picocom") || !self.console.contains("ssh"));
        shell.set_quit_command("exit");
        shell.expect_prompt()?;

        Ok(shell)
    }

    pub fn console_with_module(
        &mut self,
        module_name: &str,
    ) -> Result<ReplSession<OsSession>, Error> {
        let mut shell = self.console()?;

        let lsmod = shell.cmd("lsmod")?;

        if !lsmod.contains(module_name) {
            // Check for incompatible module state
            if module_name.contains("tc956x") && lsmod.contains("tc956x") {
                self.reboot(shell)?;
                return self.console_with_module(module_name);
            }

            shell.cmd(format!("modprobe {module_name}"))?;
        }

        Ok(shell)
    }

    /// Send a reboot command to the target device.
    ///
    /// This function deliberately drops `shell` to force a clean shutdown.
    pub fn reboot(&mut self, mut shell: ReplSession<OsSession>) -> Result<(), Error> {
        // The `reboot` is a fire-and-forget command (rather than waiting for the
        // prompt) because sometimes the close down messages interfere with the
        // display of the prompt.
        shell.send_line("reboot")?;

        // update the board state
        self.state = MachineState::Rebooting;

        Ok(())
    }

    /// Record that a machine has crashed.
    ///
    /// Marking the board as crashed changes how we connect to it in the future.
    ///
    /// This function deliberately drops `_session` to force a clean shutdown.
    pub fn crashed(&mut self, mut _session: OsSession) {
        self.state = MachineState::Crashed;
    }
}
