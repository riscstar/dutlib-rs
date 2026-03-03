use std::{
    io::{self, ErrorKind},
    process,
};

use clap::{Parser, Subcommand, ValueEnum};
use expectrl::{Error, repl::ReplSession, session::OsSession};

use dutlib::{CommandExecutor, dut::DeviceUnderTest, plans, tests};

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Reduce the verbosity
    #[arg(short, long, action = clap::ArgAction::Count)]
    quiet: u8,

    /// Increase verbosity
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
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

    /// Latency testing (using ping)
    LatencyTest(TestCli),

    /// PHY Auto-Negotiation testing
    PhyAnTest(TestCli),

    /// PHY Auto-Negotiation testing covering only 2.5Gb/s and 1Gb/s
    PhyQuickTest(TestCli),

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

#[derive(Clone, Debug, ValueEnum)]
pub enum TestPlan {
    SmokeTest,
    FunctionalTest,
    LatencyTest,
    PhyAnTest,
    PhyQuickTest,
}

type TestPlanRunner = fn(&mut ReplSession<OsSession>, &str, &str) -> Result<u32, Error>;

impl TestPlan {
    fn name(&self) -> &'static str {
        match self {
            Self::SmokeTest => "Smoke tests",
            Self::FunctionalTest => "Functional tests",
            Self::LatencyTest => "Latency tests",
            Self::PhyAnTest => "PHY auto-negotiation tests",
            Self::PhyQuickTest => "PHY quick auto-negotiation tests",
        }
    }

    fn runner(&self) -> TestPlanRunner {
        match self {
            Self::SmokeTest => plans::smoke_test,
            Self::FunctionalTest => plans::functional_test,
            Self::LatencyTest => plans::latency_test,
            Self::PhyAnTest => plans::phy_an_test,
            Self::PhyQuickTest => plans::phy_quick_test,
        }
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

    /// Set the number of times to run the test plan per boot
    #[arg(long, default_value_t = 1)]
    cycles_per_boot: u32,

    /// Choose which test plan to cycle through
    #[arg(short, long, default_value = "smoke-test")]
    plan: TestPlan,
}

fn boot_cycle(args: BootCycleCli) -> Result<(), Error> {
    let mut board = DeviceUnderTest::new();

    let mut good = 0;
    let mut bad = 0;

    let mut remaining_this_boot = 0;
    let mut console = board.console()?;

    for cycle in 0..args.cycles {
        if remaining_this_boot <= 1 {
            let _ = board.reboot(console);
            console = board.console_with_module(&args.module)?;
            let _ = tests::uname(&mut console).inspect_err(|e| log::error!("{e}"));
            remaining_this_boot = args.cycles_per_boot;
        } else {
            remaining_this_boot -= 1;
        }

        match args.plan.runner()(&mut console, &args.name, &args.ipaddr) {
            Ok(0) => good += 1,
            Ok(n) => {
                bad += 1;
                log::error!("{n} tested failed");
            }
            Err(err) => {
                bad += 1;
                log::error!("{err}");
                log::info!("{:?}", console.try_read_to_string());
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

fn run_test(args: TestCli, test_plan: TestPlan) -> Result<(), Error> {
    let mut board = DeviceUnderTest::new();
    let mut console = board.console_with_module(&args.module)?;
    tests::uname(&mut console)?;

    let name = test_plan.name();
    match test_plan.runner()(&mut console, &args.name, &args.ipaddr) {
        Ok(0) => {
            log::info!("{name} completed successfully");
            Ok(())
        }
        Ok(n) => Err(io::Error::other(format!("{name} reported {n} failures")).into()),
        Err(e) => {
            log::error!("Test plan failed to complete due to internal error");
            log::info!("{:?}", console.try_read_to_string());
            Err(e)
        }
    }
}

fn all_tests(args: TestCli) -> Result<(), Error> {
    let tests = [
        TestPlan::SmokeTest,
        TestPlan::FunctionalTest,
        TestPlan::LatencyTest,
        TestPlan::PhyAnTest,
    ];

    let mut board = DeviceUnderTest::new();
    let mut console = board.console_with_module(&args.module)?;
    tests::uname(&mut console)?;

    let mut failures = 0;

    for (plan, name) in tests.iter().map(|p| (p.runner(), p.name())) {
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
                log::error!("{name} failed to complete due to an internal error ({err})");
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

    let levels = [
        "error",
        "warn",
        "info,dutlib=warn",
        "info,dutlib::dut=warn",
        "info", // default
        "debug",
        "trace",
    ];
    const DEFAULT_LEVEL: i16 = 4;
    let env = env_logger::Env::default().default_filter_or(
        levels[(DEFAULT_LEVEL + cli.verbose as i16 - cli.quiet as i16)
            .max(0)
            .min(5) as usize],
    );
    env_logger::Builder::from_env(env).init();

    match cli.command {
        Commands::Reboot(args) => reboot(args),
        Commands::SmokeTest(args) => run_test(args, TestPlan::SmokeTest),
        Commands::BootCycle(args) => boot_cycle(args),
        Commands::FunctionalTest(args) => run_test(args, TestPlan::FunctionalTest),
        Commands::LatencyTest(args) => run_test(args, TestPlan::LatencyTest),
        Commands::PhyAnTest(args) => run_test(args, TestPlan::PhyAnTest),
        Commands::PhyQuickTest(args) => run_test(args, TestPlan::PhyQuickTest),
        Commands::AllTests(args) => all_tests(args),
    }
}

fn main() {
    match app() {
        Ok(()) => {}
        Err(Error::IO(e)) if e.kind() == ErrorKind::Other => {
            log::error!("{e}");
            process::exit(1);
        }
        Err(Error::IO(e)) => {
            log::error!("IO error: {e}");
            process::exit(2);
        }
        Err(e) => {
            log::error!("{e}");
            process::exit(3);
        }
    }
}
