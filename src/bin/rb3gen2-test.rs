use std::{
    io::{self, ErrorKind},
    process::{self, Command},
};

use clap::{Parser, Subcommand, ValueEnum};
use expectrl::Error;

use dutlib::{CommandExecutor, Config, native::NativeExecutor, plans, read_config, tests};

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
    /// Run a simple smoke test
    SmokeTest(TestCli),

    /// Cycling test to estimate test reliability
    Cycle(CycleCli),

    /// Functional testing (inc. data integrity)
    FunctionalTest(TestCli),

    /// Bandwidth testing
    BandwidthTest(TestCli),

    /// Latency testing (using ping)
    LatencyTest(TestCli),

    /// PHY Auto-Negotiation testing covering only local advertisements
    PhyQuickTest(TestCli),

    /// System integration testing (including suspend/resume tests)
    SystemTest(TestCli),

    /// Run all tests that do not require the board to be rebooted.
    AllTests(TestCli),
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
    BandwidthTest,
    LatencyTest,
    PhyQuickTest,
    SystemTest,
}

type TestPlanRunner = fn(&Config, &mut NativeExecutor) -> Result<u32, Error>;

impl TestPlan {
    fn name(&self) -> &'static str {
        match self {
            Self::SmokeTest => "Smoke tests",
            Self::FunctionalTest => "Functional tests",
            Self::BandwidthTest => "Bandwidth tests",
            Self::LatencyTest => "Latency tests",
            Self::PhyQuickTest => "PHY quick auto-negotiation tests",
            Self::SystemTest => "System integration tests",
        }
    }

    fn runner(&self) -> TestPlanRunner {
        match self {
            Self::SmokeTest => plans::smoke_test,
            Self::FunctionalTest => plans::functional_test,
            Self::BandwidthTest => plans::bandwidth_test,
            Self::LatencyTest => plans::latency_test,
            Self::PhyQuickTest => plans::phy_quick_test,
            Self::SystemTest => plans::system_test,
        }
    }
}

#[derive(Debug, Parser)]
pub struct CycleCli {
    /// Name of the network driver module to be loaded
    #[arg(short, long, default_value = "dwmac_tc956x")]
    module: String,

    /// Name of the device (as it appears in `ip addr`)
    #[arg(short, long, default_value = "enP1p5s0f1")]
    name: String,

    /// IP address (or name) of a machine running `iperf3 -s`
    #[arg(short, long, default_value = "192.168.10.2")]
    ipaddr: String,

    /// Number of test cycles to perform
    #[arg(short, long, default_value_t = 100)]
    cycles: u32,

    /// Halt on first error
    #[arg(short, long)]
    halt: bool,

    /// Choose which test plan to cycle through
    #[arg(short, long, default_value = "smoke-test")]
    plan: TestPlan,
}

fn cycle(config: Config, args: CycleCli) -> Result<(), Error> {
    let mut good = 0;
    let mut bad = 0;

    let mut shell = NativeExecutor::new();
    tests::uname(&mut shell)?;
    shell.load_module(&args.module)?;

    for cycle in 0..args.cycles {
        match args.plan.runner()(&config, &mut shell)? {
            0 => good += 1,
            n => {
                bad += 1;
                log::error!("{n} tests failed");
            }
        };

        if !args.halt {
            log::info!(
                "Success rate is {:4.1}% after {} iterations",
                (100 * good) as f64 / (good + bad) as f64,
                cycle + 1
            );
        } else if bad == 0 {
            log::info!("Successfully completed {good} iterations");
        } else {
            return Err(io::Error::other(format!("FAILED after {good} iterations")).into());
        }
    }

    Ok(())
}

fn run_test(config: Config, args: TestCli, test_plan: TestPlan) -> Result<(), Error> {
    let mut shell = NativeExecutor::new();
    tests::uname(&mut shell)?;
    shell.load_module(&args.module)?;

    let name = test_plan.name();
    match test_plan.runner()(&config, &mut shell) {
        Ok(0) => {
            log::info!("{name} completed successfully");
            Ok(())
        }
        Ok(n) => Err(io::Error::other(format!("{name} reported {n} failures")).into()),
        Err(e) => {
            log::error!("{name} did not complete");
            log::info!("Debug info: {:?}", shell.try_read_to_string());
            Err(e)
        }
    }
}

fn all_tests(config: Config, args: TestCli) -> Result<(), Error> {
    let tests = [
        TestPlan::SmokeTest,
        TestPlan::FunctionalTest,
        TestPlan::BandwidthTest,
        TestPlan::LatencyTest,
        TestPlan::PhyQuickTest,
        TestPlan::SystemTest,
    ];

    let mut shell = NativeExecutor::new();
    tests::uname(&mut shell)?;
    shell.load_module(&args.module)?;

    let mut failures = 0;

    for (plan, name) in tests.iter().map(|p| (p.runner(), p.name())) {
        let result = plan(&config, &mut shell)?;
        match result {
            0 => {
                log::info!("{name} completed successfully");
            }
            n => {
                log::info!("{name} reported {n} failures");
                failures += n;
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
        "info,dutlib::native=warn",
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

    let printk = if cli.quiet > 0 {
        let settings = Command::new("sh")
            .arg("-c")
            .arg("cat /proc/sys/kernel/printk")
            .output()
            .ok();
        Command::new("sh")
            .arg("-c")
            .arg("echo 1 > /proc/sys/kernel/printk")
            .output()?;
        settings
    } else {
        None
    };

    let config = read_config()?;

    let result = match cli.command {
        Commands::SmokeTest(args) => run_test(config, args, TestPlan::SmokeTest),
        Commands::Cycle(args) => cycle(config, args),
        Commands::FunctionalTest(args) => run_test(config, args, TestPlan::FunctionalTest),
        Commands::BandwidthTest(args) => run_test(config, args, TestPlan::BandwidthTest),
        Commands::LatencyTest(args) => run_test(config, args, TestPlan::LatencyTest),
        Commands::PhyQuickTest(args) => run_test(config, args, TestPlan::PhyQuickTest),
        Commands::SystemTest(args) => run_test(config, args, TestPlan::SystemTest),
        Commands::AllTests(args) => all_tests(config, args),
    };

    if let Some(settings) = printk {
        let original = String::from_utf8_lossy(&settings.stdout);
        let _ = Command::new("sh")
            .arg("-c")
            .arg(format!("echo '{original}' > /proc/sys/kernel/printk"))
            .spawn();
    }

    result
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
