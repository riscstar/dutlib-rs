use std::{
    io::{self, ErrorKind},
    process,
};

use clap::{Parser, Subcommand};
use expectrl::{Error, repl::ReplSession, session::OsSession};

use dutlib::{
    CommandExecutor, Config,
    dut::DeviceUnderTest,
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

    /// Name of the device (as it appears in `ip addr`)
    #[arg(short, long)]
    adapter: Option<String>,

    /// Name of the network driver module to be loaded
    #[arg(short, long)]
    module: Option<String>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Reboot the target (using the least aggressive means necessary)
    Reboot(RebootCli),

    /// Run a simple smoke test
    SmokeTest(RunTestCli),

    /// Boot cycling test (subset of smoke test) to estimate boot reliability
    BootCycle(BootCycleCli),

    /// Functional testing (inc. data integrity)
    FunctionalTest(RunTestCli),

    /// Bandwidth testing
    BandwidthTest(RunTestCli),

    /// Latency testing (using ping)
    LatencyTest(RunTestCli),

    /// PHY Auto-Negotiation testing
    PhyAnTest(RunTestCli),

    /// PHY Auto-Negotiation testing covering only local advertisements
    PhyQuickTest(RunTestCli),

    /// System integration testing (including suspend/resume tests)
    SystemTest(RunTestCli),

    /// Run tests that require the partner to be configures (requires sudo prep)
    PartnerTest(RunTestCli),

    /// Run all tests that do not require the board to be rebooted.
    AllTests(AllTestsCli),
}

#[derive(Debug, Parser)]
pub struct RebootCli {
    /// Wait for the reboot to complete before exiting
    #[arg(short, long)]
    wait: bool,
}

fn reboot(config: Config, args: RebootCli) -> Result<(), Error> {
    let mut board = DeviceUnderTest::new(&config.console, &config.power_cycle);
    let mut console = board.console()?;
    let _ = board.reboot(console);
    if args.wait {
        console = board.console()?;
        tests::uname(&mut console)?;
    }

    Ok(())
}

#[derive(Debug, Parser)]
pub struct BootCycleCli {
    /// Number of boot cycles to perform
    #[arg(short, long, default_value_t = 100)]
    cycles: u32,

    /// Set the number of times to run the test plan per boot
    #[arg(long, default_value_t = 1)]
    cycles_per_boot: u32,

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

fn all_test_plans() -> TestPlan<ReplSession<OsSession>> {
    let mut plan = TestPlan::new("all-tests");
    plan.test_plan(plans::smoke_test());
    plan.test_plan(plans::functional_test());
    plan.test_plan(plans::bandwidth_test());
    plan.test_plan(plans::latency_test());
    plan.test_plan(plans::phy_an_test());
    plan.test_plan(plans::phy_quick_test());
    plan.test_plan(plans::system_test());
    plan.test_plan(plans::partner_test());

    plan
}

fn boot_cycle(config: Config, args: BootCycleCli) -> Result<(), Error> {
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

    let mut board = DeviceUnderTest::new(&config.console, &config.power_cycle);

    let mut good = 0;
    let mut bad = 0;

    let mut remaining_this_boot = 0;
    let mut console = board.console()?;

    for cycle in 0..args.cycles {
        if remaining_this_boot <= 1 {
            let _ = board.reboot(console);
            console = board.console_with_module(&config.module)?;
            let _ = tests::uname(&mut console).inspect_err(|e| log::error!("{e}"));
            remaining_this_boot = args.cycles_per_boot;
        } else {
            remaining_this_boot -= 1;
        }

        tests::wait_for_ipv4(&config, &mut console)?;
        match plan.run(&config, &mut console) {
            Ok(0) => good += 1,
            Ok(_) => {
                bad += 1;
            }
            Err(_) => {
                bad += 1;
                log::info!("{:?}", console.try_read_to_string());
            }
        };

        if !args.halt {
            log::info!(
                "Boot success is {:4.1}% after {} iterations",
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

    /// Reboot before running any tests
    #[arg(short, long)]
    reboot: bool,
}

fn run_test(
    config: Config,
    args: RunTestCli,
    test_plan: TestPlan<ReplSession<OsSession>>,
) -> Result<(), Error> {
    let mut board = DeviceUnderTest::new(&config.console, &config.power_cycle);
    if args.reboot {
        let console = board.console()?;
        let _ = board.reboot(console);
    }
    let mut console = board.console_with_module(&config.module)?;
    tests::uname(&mut console)?;
    tests::wait_for_ipv4(&config, &mut console)?;

    // Filter if needed
    let test_plan = if let Some(select) = &args.select {
        test_plan.filter(|f| f.contains(select.as_str()))
    } else {
        test_plan
    };

    match test_plan.run(&config, &mut console) {
        Ok(0) => Ok(()),
        Ok(n) => {
            Err(io::Error::other(format!("{} reported {n} failures", test_plan.name())).into())
        }
        Err(e) => {
            log::info!("{:?}", console.try_read_to_string());
            Err(e)
        }
    }
}

#[derive(Debug, Parser)]
pub struct AllTestsCli {
    /// Reboot before running any tests
    #[arg(short, long)]
    reboot: bool,
}
fn all_tests(config: Config, args: AllTestsCli) -> Result<(), Error> {
    let plan = all_test_plans();
    let mut board = DeviceUnderTest::new(&config.console, &config.power_cycle);
    if args.reboot {
        let console = board.console()?;
        let _ = board.reboot(console);
    }
    let mut console = board.console_with_module(&config.module)?;
    tests::uname(&mut console)?;
    tests::wait_for_ipv4(&config, &mut console)?;

    let mut failures = 0;

    // phy-quick-test is a subset of phy-an-test so no need to run that one
    for sub_plan in plan.iter().filter(|p| p.name() != "phy-quick-test") {
        let result = sub_plan.run(&config, &mut console);
        match result {
            Ok(n) => {
                failures += n;
            }
            Err(_) => {
                log::info!("{:?}", console.try_read_to_string());
                failures += 1;

                // Now let's try to recover the system so we can run the next
                // set of tests
                board.crashed(console.into_session());
                console = board.console_with_module(&config.module)?;
            }
        }
    }

    match failures {
        0 => {
            log::info!("All tests completed successfully");
            Ok(())
        }
        n => Err(io::Error::other(format!("All tests completed but reported {n} failures")).into()),
    }
}

fn app() -> Result<(), Error> {
    let mut cli = Cli::parse();

    let levels = [
        "error",
        "warn",
        "info,dutlib=warn",
        "info,dutlib::dut=warn,dutlib::native=warn",
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

    let mut config = read_config()?;
    if let Some(adapter) = cli.adapter.take() {
        config.adapter = adapter;
    }
    if let Some(module) = cli.module.take() {
        config.module = module;
    }

    match cli.command {
        Commands::Reboot(args) => reboot(config, args),
        Commands::SmokeTest(args) => run_test(config, args, plans::smoke_test()),
        Commands::BootCycle(args) => boot_cycle(config, args),
        Commands::FunctionalTest(args) => run_test(config, args, plans::functional_test()),
        Commands::BandwidthTest(args) => run_test(config, args, plans::bandwidth_test()),
        Commands::LatencyTest(args) => run_test(config, args, plans::latency_test()),
        Commands::PhyAnTest(args) => run_test(config, args, plans::phy_an_test()),
        Commands::PhyQuickTest(args) => run_test(config, args, plans::phy_quick_test()),
        Commands::SystemTest(args) => run_test(config, args, plans::system_test()),
        Commands::PartnerTest(args) => run_test(config, args, plans::partner_test()),
        Commands::AllTests(args) => all_tests(config, args),
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
