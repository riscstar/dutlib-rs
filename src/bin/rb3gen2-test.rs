use std::process::{self, Command};

use clap::{Parser, Subcommand, ValueEnum};

use dutlib::{
    CommandExecutor, Config, RunTestError, UnrecoverableError,
    native::{NativeExecutor, SudoExecutor},
    plans::{self, Selection, TestPlan},
    read_config, tests,
    tsn_tests::{self, PtpRole},
    tuning,
};

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Reduce the verbosity
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    quiet: u8,

    /// Increase verbosity
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Name of the device (as it appears in `ip addr`)
    #[arg(short, long)]
    adapter: Option<String>,

    /// Name of the board (rb3gen2, pc, etc)
    #[arg(short, long)]
    board: Option<String>,

    /// Name of the network driver module to be loaded
    #[arg(short, long)]
    module: Option<String>,
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
    SetupTsn(TsnCli),
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

    #[command(flatten)]
    select: Selection,
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

fn cycle(config: Config, args: CycleCli) -> Result<(), UnrecoverableError> {
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
    let plan = args.select.filter_plan(plan);

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
            return Err(UnrecoverableError::message(format!(
                "FAILED after {good} iterations"
            )));
        }
    }

    Ok(())
}

#[derive(Debug, Parser)]
pub struct RunTestCli {
    #[command(flatten)]
    select: Selection,
}

fn run_test(
    config: Config,
    args: RunTestCli,
    test_plan: TestPlan<NativeExecutor>,
) -> Result<(), RunTestError> {
    let mut shell = NativeExecutor::new();
    tests::uname(&mut shell)?;
    shell.load_module(&config.module)?;
    tests::wait_for_ipv4(&config, &mut shell)?;

    // Filter if needed
    let test_plan = args.select.filter_plan(test_plan);

    match test_plan.run(&config, &mut shell) {
        Ok(0) => Ok(()),
        Ok(n) => Err(RunTestError::CompleteWithFailures(n)),
        Err(e) => {
            log::info!("Debug info: {:?}", shell.try_read_to_string());
            Err(e.into())
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

    /// Adopt the specified qdisc use case (default: taprio)
    #[arg(short, long, default_value = "taprio")]
    qdisc: tsn_tests::QdiscUseCase,

    /// Skip automatic platform tuning when setting up the use-case
    #[arg(short, long)]
    no_autotune: bool,
}

fn setup_tsn(config: Config, args: TsnCli) -> Result<(), UnrecoverableError> {
    let mut shell = SudoExecutor::new();
    tests::uname(&mut shell)?;
    shell.load_module(&config.module)?;
    tests::wait_for_ipv4(&config, &mut shell)?;

    let interface = if args.mirror {
        &config.partner_adapter
    } else {
        &config.adapter
    };
    let role = if args.mirror {
        PtpRole::Master
    } else {
        PtpRole::Slave
    };

    if !args.no_autotune {
        tuning::autotune(&config, &mut shell)?;
    }

    let mut undo = tsn_tests::mac_setup(&mut shell, interface, 1000000, args.qdisc)?;
    undo.append(tsn_tests::ptp_setup(&mut shell, interface, role)?);

    println!("\nScenario is configured. Press RETURN to return to defaults.");
    std::io::stdin().read_line(&mut String::new())?;

    undo.restore(&mut shell)?;

    Ok(())
}

fn app() -> Result<(), RunTestError> {
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

    let mut config = read_config(cli.board.as_deref())?;
    if let Some(adapter) = cli.adapter.take() {
        config.adapter = adapter;
    }
    if let Some(module) = cli.module.take() {
        config.module = module;
    }

    let result = match cli.command {
        Commands::SmokeTest(args) => run_test(config, args, plans::smoke_test()),
        Commands::Cycle(args) => cycle(config, args).map_err(RunTestError::from),
        Commands::FunctionalTest(args) => run_test(config, args, plans::functional_test()),
        Commands::BandwidthTest(args) => run_test(config, args, plans::bandwidth_test()),
        Commands::LatencyTest(args) => run_test(config, args, plans::latency_test()),
        Commands::PhyQuickTest(args) => run_test(config, args, plans::phy_quick_test()),
        Commands::SystemTest(args) => run_test(config, args, plans::system_test()),
        Commands::AllTests(args) => run_test(config, args, all_test_plans()),
        Commands::SetupTsn(args) => setup_tsn(config, args).map_err(RunTestError::from),
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
    let Err(e) = app() else {
        return;
    };

    match &e {
        RunTestError::Unrecoverable(_) => log::error!("{e:?}"),
        _ => log::error!("{e}"),
    }

    process::exit(match e {
        RunTestError::CompleteWithFailures(_) => 1,
        RunTestError::Unrecoverable(_) => 255,
    });
}
