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

    /// Name of the network driver module to be loaded
    #[arg(short, long)]
    module: Option<String>,

    /// Name of the device (as it appears in `ip addr`)
    #[arg(short, long)]
    adapter: Option<String>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Reboot the target (using the least aggressive means necessary)
    Reboot,

    /// Run a simple smoke test
    SmokeTest,

    /// Boot cycling test (subset of smoke test) to estimate boot reliability
    BootCycle(BootCycleCli),

    /// Functional testing (inc. data integrity)
    FunctionalTest,

    /// Bandwidth testing
    BandwidthTest,

    /// Latency testing (using ping)
    LatencyTest,

    /// PHY Auto-Negotiation testing
    PhyAnTest,

    /// PHY Auto-Negotiation testing covering only local advertisements
    PhyQuickTest,

    /// System integration testing (including suspend/resume tests)
    SystemTest,

    /// Run tests that require the partner to be configures (requires sudo prep)
    PartnerTest,

    /// Run all tests that do not require the board to be rebooted.
    AllTests,
}

fn reboot(config: Config) -> Result<(), Error> {
    let mut board = DeviceUnderTest::new(&config.console, &config.power_cycle);
    let mut console = board.console()?;
    console.cmd("reboot")?;

    Ok(())
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

    /// Halt on first error
    #[arg(short, long)]
    halt: bool,

    /// Choose which test plan to cycle through
    #[arg(short, long, default_value = "smoke-test")]
    plan: String,
}

fn all_test_plans() -> TestPlan<ReplSession<OsSession>> {
    let mut plan = TestPlan::new("all-tests");
    plan.test_plan(plans::smoke_test_new());
    plan.test_plan(plans::functional_test_new());
    plan.test_plan(plans::bandwidth_test_new());
    plan.test_plan(plans::latency_test_new());
    plan.test_plan(plans::phy_an_test_new());
    plan.test_plan(plans::phy_quick_test_new());
    plan.test_plan(plans::system_test_new());
    plan.test_plan(plans::partner_test_new());

    plan
}

fn boot_cycle(config: Config, args: BootCycleCli) -> Result<(), Error> {
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

    let mut board = DeviceUnderTest::new(&config.console, &config.power_cycle);

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

fn run_test(config: Config, test_plan: TestPlan<ReplSession<OsSession>>) -> Result<(), Error> {
    let mut board = DeviceUnderTest::new(&config.console, &config.power_cycle);
    let mut console = board.console_with_module(&config.module)?;
    tests::uname(&mut console)?;
    tests::wait_for_ipv4(&config, &mut console)?;

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

fn all_tests(config: Config) -> Result<(), Error> {
    let plan = all_test_plans();
    let mut board = DeviceUnderTest::new(&config.console, &config.power_cycle);
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
        0 => log::info!("All tests completed successfully"),
        n => log::error!("All tests completed but reported {n} failures"),
    }

    Ok(())
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
        Commands::Reboot => reboot(config),
        Commands::SmokeTest => run_test(config, plans::smoke_test_new()),
        Commands::BootCycle(args) => boot_cycle(config, args),
        Commands::FunctionalTest => run_test(config, plans::functional_test_new()),
        Commands::BandwidthTest => run_test(config, plans::bandwidth_test_new()),
        Commands::LatencyTest => run_test(config, plans::latency_test_new()),
        Commands::PhyAnTest => run_test(config, plans::phy_an_test_new()),
        Commands::PhyQuickTest => run_test(config, plans::phy_quick_test_new()),
        Commands::SystemTest => run_test(config, plans::system_test_new()),
        Commands::PartnerTest => run_test(config, plans::partner_test_new()),
        Commands::AllTests => all_tests(config),
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
