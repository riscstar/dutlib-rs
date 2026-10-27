use std::path::Path;

use crate::{CommandExecutor, CommandExecutorConnection, UnrecoverableError};

pub struct UndoTracker {
    connection: CommandExecutorConnection,
    commands: Vec<String>,
}

impl UndoTracker {
    pub fn new(shell: &impl CommandExecutor) -> Self {
        Self {
            connection: shell.connection(),
            commands: Vec::new(),
        }
    }

    pub fn add(&mut self, action: String) {
        self.commands.push(action);
    }

    pub fn append(&mut self, mut other: UndoTracker) {
        self.commands.append(&mut other.commands);
    }

    pub fn sysfs(
        &mut self,
        shell: &mut impl CommandExecutor,
        path: impl AsRef<Path>,
        value: impl AsRef<str>,
    ) -> Result<(), UnrecoverableError> {
        let path = path.as_ref().to_string_lossy();
        let value = value.as_ref();

        let old_value = shell.cmd(format!("cat {path}"))?;
        let _ = shell.cmd(format!("echo {value} > {path}"))?;
        self.add(format!("echo {old_value} > {path}"));

        Ok(())
    }

    pub fn restore(&mut self, shell: &mut impl CommandExecutor) -> Result<(), UnrecoverableError> {
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
                "UndoTracker for {} contains pending state changes: {}",
                self.connection,
                self.commands
                    .iter()
                    .rev()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("; ")
            );
        }
    }
}
