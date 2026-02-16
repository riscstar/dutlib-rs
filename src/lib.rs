use expectrl::{
    Error, Expect, Session,
    repl::ReplSession,
    session::{OsProcess, OsSession, OsStream},
    stream::log::LogStream,
};
use std::{io::Stdout, process::Command};

pub mod cmd;

pub trait ReplSessionExt {
    fn cmd<C: AsRef<str>>(&mut self, cmd: C) -> Result<String, Error>;
}

impl<S: Expect> ReplSessionExt for ReplSession<S> {
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
                for ln in String::from_utf8_lossy(&raw[11..raw.len() - 10]).split("\r\n") {
                    log::debug!("  < {}", ln);
                    s.push('\n');
                    s.push_str(ln);
                }
            }

            s
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum MachineState {
    Booted,
    Crashed,
    Rebooting,
}

pub type OsLogSession = Session<OsProcess, LogStream<OsStream, Stdout>>;

pub struct Rb3Gen2 {
    state: MachineState,
}

impl Rb3Gen2 {
    pub fn new() -> Self {
        Self {
            state: MachineState::Booted,
        }
    }

    pub fn console(&mut self) -> Result<ReplSession<OsSession>, Error> {
        log::info!("Connecting to console ({:?})", self.state);
        match self.state {
            MachineState::Booted => {}
            MachineState::Rebooting => {
                // wait to the prompt to appear
                std::thread::sleep(std::time::Duration::from_secs(40));
                self.state = MachineState::Booted;
            }
            MachineState::Crashed => {
                let mut cmd = Command::new("iot-power-cycle");
                cmd.arg("rb3gen2-usbc");
                let _ = cmd.output()?;
            }
        }

        let mut cmd = Command::new("ssh");
        cmd.args("-t walnut picocom -b 115200 /dev/serial/by-id/usb-Prolific_Technology_Inc._Prolific_PL2303GD_USB_Serial_COM_Port_DAAOb119D16-if00".split_whitespace());
        let mut uart = Session::spawn(cmd)?;

        if let Err(err) = uart.expect("Terminal ready") {
            log::error!("Terminal emulator did not report ready (port busy?)");
            return Err(err);
        }
        uart.send_line("")?;
        uart.send_line("echo sync ch''eck")?;
        match uart.expect("sync check") {
            Ok(_) => {}
            Err(Error::ExpectTimeout) => {
                log::warn!("Timed out during synchonization check");
                self.state = MachineState::Crashed;
                return self.console();
            }
            Err(err) => {
                return Err(err);
            }
        }

        // Kernel messages can make the REPL management unreliable (since they
        // can interfere with the prompt). Let's turn them off.
        uart.send_line("echo 1 > /proc/sys/kernel/printk")?;
        uart.send_line("echo resync ch''eck")?;
        uart.expect("resync check")?;

        let mut shell = ReplSession::new(uart, "root@rb3gen2:~# ");
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
            shell.cmd("reboot")?;
            self.state = MachineState::Rebooting;
            std::mem::drop(shell);
            return self.console_with_module(module_name);
        }

        shell.cmd(format!("modprobe {module_name}"))?;
        Ok(shell)
    }

    pub fn reboot(&mut self, mut shell: ReplSession<OsSession>) -> Result<(), Error> {
        shell.cmd("reboot")?;
        self.state = MachineState::Rebooting;
        Ok(())
    }
}
