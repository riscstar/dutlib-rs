use expectrl::Error;

use crate::{CommandExecutor, Config, ethtool, native::SudoExecutor};

/// Automatically tune the system to improve performance (esp. latency).
///
/// For QPS615 to QPS615 testing then failures are expected without this tuning.
///
/// This is *not* a test case and there is no logic to "undo" the tuning after
/// is has been applied.
pub fn autotune(config: &Config, shell: &mut impl CommandExecutor) -> Result<(), Error> {
    if !config.no_autotune {
        shell.cmd("cpupower idle-set --disable-by-latency 100")?;

        if !config.partner_adapter.is_empty() {
            let mut partner = SudoExecutor::new();

            partner.cmd("cpupower idle-set --disable-by-latency 100")?;

            // The EEE tests (later) assume EEE is enabled on both adapters by
            // default. stmmac *is* on by default but some adapters require
            // this to be enabled explicitly, so let's do that just in case.
            ethtool::cmd_and_wait_link_up(
                &mut partner,
                &config.partner_adapter,
                "--set-eee <ADAPTER> eee on",
            )?;
        }
    }

    Ok(())
}
