use std::path::Path;

use expectrl::Error;

use crate::CommandExecutor;

pub struct UndoTracker {
    commands: Vec<String>,
}

impl UndoTracker {
    pub fn new() -> Self {
        Self {
            commands: Vec::new(),
        }
    }

    pub fn add(&mut self, action: String) {
        self.commands.push(action);
    }

    pub fn sysfs(
        &mut self,
        shell: &mut impl CommandExecutor,
        path: impl AsRef<Path>,
        value: impl AsRef<str>,
    ) -> Result<(), Error> {
        let path = path.as_ref().to_string_lossy();
        let value = value.as_ref();

        let old_value = shell.cmd(format!("cat {path}"))?;
        let _ = shell.cmd(format!("echo {value} > {path}"))?;
        self.add(format!("echo {old_value} > {path}"));
        dbg!(&self.commands);

        Ok(())
    }

    pub fn restore(&mut self, shell: &mut impl CommandExecutor) -> Result<(), Error> {
        while let Some(action) = self.commands.pop() {
            let _ = shell.cmd(action)?;
        }

        Ok(())
    }
}

impl Drop for UndoTracker {
    fn drop(&mut self) {
        if !self.commands.is_empty() {
            log::error!(
                "UndoTracker contains pending state changes: {:?}",
                self.commands
            );
        }
    }
}
