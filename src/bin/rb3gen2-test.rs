use std::{
    io::{self, ErrorKind},
    process::{self, Command},
};

use clap::{Parser, Subcommand, ValueEnum};
use expectrl::Error;

use dutlib::{
    CommandExecutor, Config,
    native::NativeExecutor,
    plans::{self, TestPlan},
    read_config, tests, tsn_tests,
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
    SmokeTest(RunTestCli),

    /// Cycling test to estimate test reliability
    Cycle(CycleCli),

    /// Functional testing (inc. data integrity)
    FunctionalTest(RunTestCli),

    /// Bandwidth testing
    BandwidthTest(RunTestCli),

    /// Latency testing (using ping)
    LatencyTest(RunTestCli),

    /// PHY Auto-Negotiation testing covering only local advertisements
    PhyQuickTest(RunTestCli),

    /// System integration testing (including suspend/resume tests)
    SystemTest(RunTestCli),

    /// Run all tests that do not require the board to be rebooted.
    AllTests(RunTestCli),

    // Configure interactive TSN scenarios
    Tsn(TsnCli),
}

#[derive(Debug, Parser)]
pub struct CycleCli {
    /// Number of test cycles to perform
    #[arg(short, long, default_value_t = 100)]
    cycles: u32,

    /// Halt on first error
    #[arg(short, long)]
    halt: bool,

    /// Choose which test plan to cycle through
    #[arg(short, long, default_value = "smoke-test")]
    plan: String,

    /// Skip all tests that do not match the selection
    #[arg(short, long)]
    select: Option<String>,
}

fn all_test_plans() -> TestPlan<NativeExecutor> {
    let mut plan = TestPlan::new("all-tests");
    plan.test_plan(plans::smoke_test());
    plan.test_plan(plans::functional_test());
    plan.test_plan(plans::bandwidth_test());
    plan.test_plan(plans::latency_test());
    plan.test_plan(plans::phy_quick_test());
    plan.test_plan(plans::system_test());

    plan
}

fn cycle(config: Config, args: CycleCli) -> Result<(), Error> {
    let all_plans = all_test_plans();
    let mut plan = None;
    for candidate in all_plans.into_iter() {
        if args.plan == candidate.name() {
            plan = Some(candidate);
            break;
        }
    }
    let Some(plan) = plan else {
        log::error!("Unknown test plan: {}", args.plan);
        return Ok(());
    };

    // Filter if needed
    let plan = if let Some(select) = &args.select {
        plan.filter(|f| f.contains(select.as_str()))
    } else {
        plan
    };

    let mut good = 0;
    let mut bad = 0;

    let mut shell = NativeExecutor::new();
    tests::uname(&mut shell)?;
    shell.load_module(&config.module)?;
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

#[derive(Debug, Parser)]
pub struct RunTestCli {
    /// Skip all tests that do not match the selection
    #[arg(short, long)]
    select: Option<String>,
}

fn run_test(
    config: Config,
    args: RunTestCli,
    test_plan: TestPlan<NativeExecutor>,
) -> Result<(), Error> {
    let mut shell = NativeExecutor::new();
    tests::uname(&mut shell)?;
    shell.load_module(&config.module)?;
    tests::wait_for_ipv4(&config, &mut shell)?;

    // Filter if needed
    let test_plan = if let Some(select) = &args.select {
        test_plan.filter(|f| f.contains(select.as_str()))
    } else {
        test_plan
    };

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

#[derive(Clone, Debug, ValueEnum)]
enum TsnScenario {
    /// Configure as profinet_rt
    ProfinetRt,
}

#[derive(Debug, Parser)]
pub struct TsnCli {
    /// Skip all tests that do not match the selection
    scenario: TsnScenario,

    /// Configure the link partner device (based on config)
    #[arg(short, long)]
    mirror: bool,
}

fn tsn_scenario(config: Config, args: TsnCli) -> Result<(), Error> {
    let mut shell = NativeExecutor::new();
    tests::uname(&mut shell)?;
    shell.load_module(&config.module)?;
    tests::wait_for_ipv4(&config, &mut shell)?;

    let interface = if args.mirror {
        &config.partner_adapter
    } else {
        &config.adapter
    };

    let mut undo = tsn_tests::stmmac_setup(&mut shell, interface, 1000000)?;
    println!("\nScenario is configured. Press RETURN to return to defaults.");
    std::io::stdin().read_line(&mut String::new())?;
    undo.restore(&mut shell)?;

    Ok(())
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
        Commands::SmokeTest(args) => run_test(config, args, plans::smoke_test()),
        Commands::Cycle(args) => cycle(config, args),
        Commands::FunctionalTest(args) => run_test(config, args, plans::functional_test()),
        Commands::BandwidthTest(args) => run_test(config, args, plans::bandwidth_test()),
        Commands::LatencyTest(args) => run_test(config, args, plans::latency_test()),
        Commands::PhyQuickTest(args) => run_test(config, args, plans::phy_quick_test()),
        Commands::SystemTest(args) => run_test(config, args, plans::system_test()),
        Commands::AllTests(args) => run_test(config, args, all_test_plans()),
        Commands::Tsn(args) => tsn_scenario(config, args),
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
