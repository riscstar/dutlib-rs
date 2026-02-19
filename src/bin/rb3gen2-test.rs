use std::{io, process};

use clap::{Parser, Subcommand};
use expectrl::{Error, repl::ReplSession, session::OsSession};
use log::error;

use dutlib::{dut::DeviceUnderTest, dut::ReplSessionExt, tests};

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
    SmokeTest(TestCli),

    /// Boot cycling test (subset of smoke test) to estimate boot reliability
    BootCycle(BootCycleCli),

    /// Functional testing (inc. data integrity)
    FunctionalTest(TestCli),

    /// Run all tests that do not require the board to be rebooted.
    AllTests(TestCli),
}

#[derive(Debug, Parser)]
pub struct RebootCli {}

fn reboot(_args: RebootCli) -> Result<(), Error> {
    let mut board = DeviceUnderTest::new();
    let mut console = board.console()?;
    console.cmd("reboot")?;

    Ok(())
}

#[derive(Debug, Parser)]
pub struct TestCli {
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

fn smoke_test(
    console: &mut ReplSession<OsSession>,
    name: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    tests::wait_for_ipv4(console, name)?;

    let mut failures = 0;

    failures += tests::ping(console, ipaddr)?;
    failures += tests::iperf3_bidir(console, ipaddr)?;
    failures += tests::iperf3_tx(console, ipaddr)?;
    failures += tests::iperf3_rx(console, ipaddr)?;

    Ok(failures)
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
    let mut board = DeviceUnderTest::new();

    let mut good = 0;
    let mut bad = 0;

    let mut console = board.console()?;

    for cycle in 0..args.cycles {
        let _ = board.reboot(console);
        console = board.console_with_module(&args.module)?;
        let _ = tests::uname(&mut console).inspect_err(|e| log::error!("{e}"));

        match smoke_test(&mut console, &args.name, &args.ipaddr) {
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

fn functional_test(
    console: &mut ReplSession<OsSession>,
    name: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    tests::wait_for_ipv4(console, name)?;

    let mut failures = 0;

    failures += tests::scp_bidir(console, ipaddr)?;
    failures += tests::scp_tx(console, ipaddr)?;
    failures += tests::scp_rx(console, ipaddr)?;

    Ok(failures)
}

fn run_test<T>(args: TestCli, test: T) -> Result<(), Error>
where
    T: FnOnce(&mut ReplSession<OsSession>, &str, &str) -> Result<u32, Error>,
{
    let mut board = DeviceUnderTest::new();
    let mut console = board.console_with_module(&args.module)?;
    tests::uname(&mut console)?;

    match test(&mut console, &args.name, &args.ipaddr) {
        Ok(0) => Ok(()),
        Ok(n) => Err(io::Error::other(format!("{n} failures reported")).into()),
        Err(e) => {
            log::error!("Test plan failed to complete due to internal error");
            log::info!("{:?}", console.try_read_to_string());
            Err(e)
        }
    }
}

fn all_tests(args: TestCli) -> Result<(), Error> {
    let plans = [smoke_test, functional_test];
    let names = ["Smoke tests", "Functional tests"];

    let mut board = DeviceUnderTest::new();
    let mut console = board.console_with_module(&args.module)?;
    tests::uname(&mut console)?;

    let mut failures = 0;

    for (plan, name) in plans.iter().zip(names) {
        let result = plan(&mut console, &args.name, &args.ipaddr);
        match result {
            Ok(0) => {
                log::info!("{name} completed successfully");
            }
            Ok(n) => {
                log::info!("{name} reported {n} failures");
                failures += n;
            }
            Err(err) => {
                log::error!("{name} failed to complete due to an internal error");
                log::info!("{err}");
                log::info!("{:?}", console.try_read_to_string());
                failures += 1;

                // Now let's try to recover the system so we can run the next
                // set of tests
                board.crashed(console.into_session());
                console = board.console_with_module(&args.module)?;
            }
        }
    }

    match failures {
        0 => log::info!("All tests completed successfully"),
        n => log::info!("All tests completed but reported {n} failures"),
    }

    Ok(())
}

fn app() -> Result<(), Error> {
    let cli = Cli::parse();

    let default_level = if cli.verbose {
        "debug"
    } else if cli.quiet {
        "info,dutlib::dut=warn"
    } else {
        "info"
    };
    let env = env_logger::Env::default().default_filter_or(default_level);
    env_logger::Builder::from_env(env).init();

    match cli.command {
        Commands::Reboot(args) => reboot(args),
        Commands::SmokeTest(args) => run_test(args, smoke_test),
        Commands::BootCycle(args) => boot_cycle(args),
        Commands::FunctionalTest(args) => run_test(args, functional_test),
        Commands::AllTests(args) => all_tests(args),
    }
}

fn main() {
    if let Err(e) = app() {
        error!("{e}");
        process::exit(1);
    }
}
