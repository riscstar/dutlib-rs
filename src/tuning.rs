use expectrl::Error;

use crate::{CommandExecutor, Config, ethtool, native::SudoExecutor};

fn apply_tuning(shell: &mut impl CommandExecutor) -> Result<(), Error> {
    // Disable high latency cpuidle states
    shell.cmd("cpupower idle-set --disable-by-latency 100")?;

    // Interrupt latency is strongly influenced by little cluster minimum
    // performance
    shell.cmd("cpupower -c 0 frequency-set --min 1000000")?;

    // Prevent ASPM adopting L0 or L1 states
    shell.cmd("echo performance > /sys/module/pcie_aspm/parameters/policy")?;

    Ok(())
}

/// Automatically tune the system to improve performance (esp. latency).
///
/// For QPS615 to QPS615 testing then failures are expected without this tuning.
///
/// This is *not* a test case and there is no logic to "undo" the tuning after
/// is has been applied.
pub fn autotune(config: &Config, shell: &mut impl CommandExecutor) -> Result<(), Error> {
    if !config.no_autotune {
        apply_tuning(shell)?;

        if !config.partner_adapter.is_empty() {
            let mut partner = SudoExecutor::new();

            apply_tuning(&mut partner)?;

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
