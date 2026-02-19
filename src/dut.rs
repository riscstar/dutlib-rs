use expectrl::{
    Error, Expect, Session,
    process::NonBlocking,
    repl::ReplSession,
    session::{OsProcess, OsSession, OsStream},
    stream::log::LogStream,
};
use std::{
    io::{Read, Stdout},
    process::Command,
    thread,
    time::Duration,
};

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

        log::info!("{:?}", data);
        if data.len() > 0 {
            Some(String::from_utf8_lossy(&data).to_string())
        } else {
            return None;
        }
    }
}

pub trait ReplSessionExt {
    /// Convenience method to run a comment.
    ///
    /// This method is similar to `execute()` but has some additional
    /// conveniences.
    ///
    /// 1. Output can be automatically logged (if log level is set
    ///    appropriately).
    /// 2. Output is cleaned up to remove some escape sequences and to replace
    ///    \r\n with plain \n
    /// 3. Output if returned as a `String`
    fn cmd<C: AsRef<str>>(&mut self, cmd: C) -> Result<String, Error>;

    /// Read all available data and convert to a string.
    fn try_read_to_string(&mut self) -> Option<String>;
}

impl<S: Expect + SessionExt> ReplSessionExt for ReplSession<S> {
    fn cmd<C: AsRef<str>>(&mut self, cmd: C) -> Result<String, Error> {
        log::info!(">>> {}", cmd.as_ref());
        let reply = self.execute(cmd);
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

            log::trace!("{raw:?}");
            log::trace!("{s:?}");

            s
        })
    }

    fn try_read_to_string(&mut self) -> Option<String> {
        self.get_session_mut().try_read_to_string()
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
}

impl DeviceUnderTest {
    pub fn new() -> Self {
        Self {
            state: MachineState::Booted,
        }
    }

    pub fn console(&mut self) -> Result<ReplSession<OsSession>, Error> {
        log::info!("Connecting to console ({:?})", self.state);
        if self.state == MachineState::Crashed {
            let mut cmd = Command::new("iot-power-cycle");
            cmd.arg("rb3gen2");
            let output = cmd.output()?;
            log::debug!("Power-cycle: {output:?}");
        }

        let mut cmd = Command::new("ssh");
        cmd.args("-t walnut picocom -b 115200 /dev/serial/by-id/usb-Prolific_Technology_Inc._Prolific_PL2303GD_USB_Serial_COM_Port_DAAOb119D16-if00".split_whitespace());
        let mut uart = Session::spawn(cmd)?;

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

            log::debug!("{:?}", remaining);
            log::warn!("Cannot connect to target");
            return Err(err);
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

        // Kernel messages can make the REPL management unreliable (since they
        // can interfere with the prompt). Let's turn them off.
        uart.send_line("echo 1 > /proc/sys/kernel/printk")?;

        // We need a TERM value that doesn't inject junk into the echoed
        // characters
        uart.send_line("TERM=vt102; export TERM")?;

        // Let's set our own prompt (one without any escape codes in it)
        uart.send_line("PS1=\"REPLSESSION# \"")?;

        uart.send_line("echo resync ch''eck")?;
        uart.expect("resync check")?;

        let mut shell = ReplSession::new(uart, "REPLSESSION# ");
        shell.set_echo(true);
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

        // Check for incompatible module state
        if module_name.contains("tc956x")
            && lsmod.contains("tc956x")
            && !lsmod.contains(module_name)
        {
            self.reboot(shell)?;
            return self.console_with_module(module_name);
        }

        shell.cmd(format!("modprobe {module_name}"))?;
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
