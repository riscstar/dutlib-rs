use std::{
    io::{self, ErrorKind},
    process::{self, Command},
};

use clap::{Parser, Subcommand};
use expectrl::Error;

use dutlib::{
    CommandExecutor, Config,
    native::NativeExecutor,
    plans::{self, TestPlan},
    read_config, tests,
};

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

    /// Name of the network driver module to be loaded
    #[arg(short, long)]
    module: Option<String>,

    /// Name of the device (as it appears in `ip addr`)
    #[arg(short, long)]
    adapter: Option<String>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Run a simple smoke test
    SmokeTest,

    /// Cycling test to estimate test reliability
    Cycle(CycleCli),

    /// Functional testing (inc. data integrity)
    FunctionalTest,

    /// Bandwidth testing
    BandwidthTest,

    /// Latency testing (using ping)
    LatencyTest,

    /// PHY Auto-Negotiation testing covering only local advertisements
    PhyQuickTest,

    /// System integration testing (including suspend/resume tests)
    SystemTest,

    /// Run all tests that do not require the board to be rebooted.
    AllTests,
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
    plan: String,
}

fn all_test_plans() -> TestPlan<NativeExecutor> {
    let mut plan = TestPlan::new("all-tests");
    plan.test_plan(plans::smoke_test_new());
    plan.test_plan(plans::functional_test_new());
    plan.test_plan(plans::bandwidth_test_new());
    plan.test_plan(plans::latency_test_new());
    plan.test_plan(plans::phy_quick_test_new());
    plan.test_plan(plans::system_test_new());

    plan
}

fn cycle(config: Config, args: CycleCli) -> Result<(), Error> {
    let all_plans = all_test_plans();
    let mut plan = None;
    for candidate in all_plans.iter() {
        if args.plan == candidate.name() {
            plan = Some(candidate);
            break;
        }
    }
    let Some(plan) = plan else {
        log::error!("Unknown test plan: {}", args.plan);
        return Ok(());
    };

    let mut good = 0;
    let mut bad = 0;

    let mut shell = NativeExecutor::new();
    tests::uname(&mut shell)?;
    shell.load_module(&args.module)?;
    tests::wait_for_ipv4(&config, &mut shell)?;

    for cycle in 0..args.cycles {
        match plan.run(&config, &mut shell)? {
            0 => good += 1,
            _n => {
                bad += 1;
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

fn run_test(config: Config, test_plan: TestPlan<NativeExecutor>) -> Result<(), Error> {
    let mut shell = NativeExecutor::new();
    tests::uname(&mut shell)?;
    shell.load_module(&config.module)?;
    tests::wait_for_ipv4(&config, &mut shell)?;

    match test_plan.run(&config, &mut shell) {
        Ok(0) => Ok(()),
        Ok(n) => {
            Err(io::Error::other(format!("{} reported {n} failures", test_plan.name())).into())
        }
        Err(e) => {
            log::info!("Debug info: {:?}", shell.try_read_to_string());
            Err(e)
        }
    }
}

fn app() -> Result<(), Error> {
    let mut cli = Cli::parse();

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

    let mut config = read_config()?;
    if let Some(adapter) = cli.adapter.take() {
        config.adapter = adapter;
    }
    if let Some(module) = cli.module.take() {
        config.module = module;
    }

    let result = match cli.command {
        Commands::SmokeTest => run_test(config, plans::smoke_test_new()),
        Commands::Cycle(args) => cycle(config, args),
        Commands::FunctionalTest => run_test(config, plans::functional_test_new()),
        Commands::BandwidthTest => run_test(config, plans::bandwidth_test_new()),
        Commands::LatencyTest => run_test(config, plans::latency_test_new()),
        Commands::PhyQuickTest => run_test(config, plans::phy_quick_test_new()),
        Commands::SystemTest => run_test(config, plans::system_test_new()),
        Commands::AllTests => run_test(config, all_test_plans()),
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
