use std::{io, process};

use clap::{Parser, Subcommand};
use expectrl::{Error, repl::ReplSession, session::OsSession};
use log::error;

use rb3gen2_test::{Rb3Gen2, ReplSessionExt, cmd};

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Only report warnings and errors
    #[arg(short, long)]
    quiet: bool,

    /// Increase verbosity
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Reboot the target (using the least aggressive means necessary)
    Reboot(RebootCli),

    /// Run a simple smoke test
    SmokeTest(SmokeTestCli),

    /// Boot cycling test (subset of smoke test) to estimate boot reliability
    BootCycle(BootCycleCli),
}

#[derive(Debug, Parser)]
pub struct RebootCli {}

fn reboot(_args: RebootCli) -> Result<(), Error> {
    let mut board = Rb3Gen2::new();
    let mut console = board.console()?;
    console.cmd("reboot")?;

    Ok(())
}

#[derive(Debug, Parser)]
pub struct SmokeTestCli {
    /// Name of the network driver module to be loaded
    #[arg(short, long, default_value = "dwmac_tc956x")]
    module: String,

    /// Name of the device (as it appears in `ip addr`)
    #[arg(short, long, default_value = "enP1p5s0f1")]
    name: String,

    /// IP address (or name) of a machine running `iperf3 -s`
    #[arg(short, long, default_value = "192.168.10.2")]
    ipaddr: String,
}

fn run_smoke_test(
    console: &mut ReplSession<OsSession>,
    name: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    cmd::wait_for_ipv4(console, name)?;

    let mut failures = 0;

    failures += cmd::ping(console, ipaddr)?;
    failures += cmd::iperf3_bidir(console, ipaddr)?;
    failures += cmd::iperf3_tx(console, ipaddr)?;
    failures += cmd::iperf3_rx(console, ipaddr)?;

    Ok(failures)
}
fn smoke_test(args: SmokeTestCli) -> Result<(), Error> {
    let mut board = Rb3Gen2::new();
    let mut console = board.console_with_module(&args.module)?;
    cmd::uname(&mut console)?;

    match run_smoke_test(&mut console, &args.name, &args.ipaddr) {
        Ok(0) => Ok(()),
        Ok(n) => Err(io::Error::other(format!("{n} failures reported")).into()),
        Err(e) => Err(e),
    }
}

#[derive(Debug, Parser)]
pub struct BootCycleCli {
    /// Name of the network driver module to be loaded
    #[arg(short, long, default_value = "dwmac_tc956x")]
    module: String,

    /// Name of the device (as it appears in `ip addr`)
    #[arg(short, long, default_value = "enP1p5s0f1")]
    name: String,

    /// IP address (or name) of a machine running `iperf3 -s`
    #[arg(short, long, default_value = "192.168.10.2")]
    ipaddr: String,

    /// Number of boot cycles to perform
    #[arg(short, long, default_value_t = 100)]
    cycles: u32,
}

fn boot_cycle(args: BootCycleCli) -> Result<(), Error> {
    let mut board = Rb3Gen2::new();

    let mut good = 0;
    let mut bad = 0;

    let mut console = board.console()?;

    for cycle in 0..args.cycles {
        let _ = board.reboot(console);
        console = board.console_with_module(&args.module)?;
        let _ = cmd::uname(&mut console).inspect_err(|e| log::error!("{e}"));

        match run_smoke_test(&mut console, &args.name, &args.ipaddr) {
            Ok(0) => good += 1,
            Ok(n) => {
                bad += 1;
                log::error!("{n} tested failed");
            }
            Err(err) => {
                bad += 1;
                log::error!("{err}");
            }
        };

        log::info!(
            "Boot success is {:4.1}% after {} iterations",
            (100 * good) as f64 / (good + bad) as f64,
            cycle + 1
        );
    }

    Ok(())
}

fn app() -> Result<(), Error> {
    let cli = Cli::parse();

    let default_level = if cli.verbose {
        "debug"
    } else if cli.quiet {
        "warn"
    } else {
        "info"
    };
    let env = env_logger::Env::default().default_filter_or(default_level);
    env_logger::Builder::from_env(env).init();

    match cli.command {
        Commands::Reboot(args) => reboot(args),
        Commands::SmokeTest(args) => smoke_test(args),
        Commands::BootCycle(args) => boot_cycle(args),
    }
}

fn main() {
    if let Err(e) = app() {
        error!("{e}");
        process::exit(1);
    }
}
