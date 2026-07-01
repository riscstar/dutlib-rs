use expectrl::Error;

use crate::{CommandExecutor, Config, tests, tsn_tests, tuning};

type TestFunc<T> = fn(&Config, &mut T) -> Result<u32, Error>;

#[derive(Debug)]
struct TestCase<T: CommandExecutor> {
    name: &'static str,
    code: TestFunc<T>,
}

impl<T: CommandExecutor> Clone for TestCase<T> {
    fn clone(&self) -> Self {
        Self {
            name: self.name,
            code: self.code,
        }
    }
}

impl<T: CommandExecutor> TestCase<T> {
    pub fn new(name: &'static str, code: TestFunc<T>) -> Self {
        Self { name, code }
    }

    pub fn run(&self, config: &Config, shell: &mut T, path: &str) -> Result<u32, Error> {
        let name = self.name;
        log::info!("RUNNING: {path}/{name}");
        match (self.code)(config, shell) {
            Ok(0) => {
                log::info!("PASSED: {path}/{name}");
                Ok(0)
            }
            Ok(failures) => {
                log::error!("FAILED: {path}/{name} (reported {failures} failures)");
                Ok(failures)
            }
            Err(e) => {
                log::error!("ABORTED: {path}/{name}: {e}");
                Err(e)
            }
        }
    }
}

#[derive(Debug)]
enum TestSet<T: CommandExecutor> {
    TestCase(TestCase<T>),
    TestPlan(TestPlan<T>),
}

impl<T: CommandExecutor> Clone for TestSet<T> {
    fn clone(&self) -> Self {
        match self {
            TestSet::TestCase(test_case) => TestSet::TestCase(test_case.clone()),
            TestSet::TestPlan(test_plan) => TestSet::TestPlan(test_plan.clone()),
        }
    }
}

impl<T: CommandExecutor> TestSet<T> {
    fn name(&self) -> &'static str {
        match self {
            TestSet::TestCase(test_case) => test_case.name,
            TestSet::TestPlan(test_plan) => test_plan.name,
        }
    }
}

#[derive(Debug)]
pub struct TestPlan<T: CommandExecutor> {
    name: &'static str,
    plan: Vec<TestSet<T>>,
}

impl<T: CommandExecutor> Clone for TestPlan<T> {
    fn clone(&self) -> Self {
        Self {
            name: self.name,
            plan: self.plan.clone(),
        }
    }
}

impl<T: CommandExecutor> TestPlan<T> {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            plan: Vec::new(),
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn test_case(&mut self, name: &'static str, code: TestFunc<T>) {
        self.plan.push(TestSet::TestCase(TestCase::new(name, code)));
    }

    pub fn test_plan(&mut self, plan: TestPlan<T>) {
        self.plan.push(TestSet::TestPlan(plan));
    }

    pub fn iter(&self) -> impl Iterator<Item = &TestPlan<T>> {
        self.plan.iter().filter_map(|s| match s {
            TestSet::TestCase(_) => None,
            TestSet::TestPlan(plan) => Some(plan),
        })
    }

    /// IntoIterator work-a-like.
    ///
    /// This is implemented as a regular method rather than using IntoIterator
    /// because I don't know how to spell the return type without using
    /// `impl` and when implementing IntoIterator the return type would become
    /// an associated type (see https://github.com/rust-lang/rust/issues/63063).
    pub fn into_iter(self) -> impl Iterator<Item = TestPlan<T>> {
        self.plan.into_iter().filter_map(|s| match s {
            TestSet::TestCase(_) => None,
            TestSet::TestPlan(plan) => Some(plan),
        })
    }

    pub fn filter(&self, predicate: impl Fn(&str) -> bool) -> TestPlan<T> {
        Self {
            name: self.name,
            plan: self
                .plan
                .iter()
                .filter(|ts| predicate(ts.name()))
                .map(|ts| ts.clone())
                .collect(),
        }
    }

    pub fn run_with_path(&self, config: &Config, shell: &mut T, path: &str) -> Result<u32, Error> {
        let name = if path.is_empty() || path.ends_with("/") {
            format!("{}{}", path, self.name)
        } else {
            format!("{}/{}", path, self.name)
        };
        let mut failures = 0;

        for t in self.plan.iter() {
            failures += match t {
                TestSet::TestCase(test_case) => test_case.run(config, shell, &name)?,
                TestSet::TestPlan(test_plan) => test_plan.run_with_path(config, shell, &name)?,
            }
        }

        match failures {
            0 => log::info!("SUMMARY: Completed {name} plan successfully"),
            n => log::error!("SUMMARY: Completed {name} plan but reported {n} failures"),
        }
        Ok(failures)
    }

    pub fn run(&self, config: &Config, shell: &mut T) -> Result<u32, Error> {
        log::info!("TUNING: Automatically applying system tuning");
        tuning::autotune(config, shell)?;

        self.run_with_path(config, shell, "")
    }
}

pub fn smoke_test<T: CommandExecutor>() -> TestPlan<T> {
    let mut plan = TestPlan::new("smoke-test");

    plan.test_case("ping", tests::ping);
    plan.test_case("iperf3_bidir", tests::iperf3_bidir);
    plan.test_case("iperf3_tx", tests::iperf3_tx);
    plan.test_case("iperf3_rx", tests::iperf3_rx);
    plan.test_case("ethtool_selftest", tests::ethtool_selftest);
    plan.test_case("verify_log_messages", tests::verify_log_messages);

    plan
}

pub fn functional_test<T: CommandExecutor>() -> TestPlan<T> {
    let mut plan = TestPlan::new("functional-test");

    plan.test_case("scp_bidir", tests::scp_bidir);
    plan.test_case("scp_tx", tests::scp_tx);
    plan.test_case("scp_rx", tests::scp_rx);
    plan.test_case("verify_log_messages", tests::verify_log_messages);

    plan
}

pub fn bandwidth_test<T: CommandExecutor>() -> TestPlan<T> {
    let mut plan = TestPlan::new("bandwidth-test");

    plan.test_case("iperf3_intervals_bidir", tests::iperf3_intervals_bidir);
    plan.test_case("iperf3_intervals_tx", tests::iperf3_intervals_tx);
    plan.test_case("iperf3_intervals_rx", tests::iperf3_intervals_rx);
    plan.test_case("iperf3_udp_bidir", tests::iperf3_udp_bidir);
    plan.test_case("iperf3_udp_tx", tests::iperf3_udp_tx);
    plan.test_case("iperf3_udp_tx", tests::iperf3_udp_rx);
    plan.test_case("iperf3_x16_bidir", tests::iperf3_x16_bidir);
    plan.test_case("iperf3_x16_tx", tests::iperf3_x16_tx);
    plan.test_case("iperf3_x16_rx", tests::iperf3_x16_rx);
    plan.test_case("verify_log_messages", tests::verify_log_messages);

    plan
}

pub fn latency_test<T: CommandExecutor>() -> TestPlan<T> {
    let mut plan = TestPlan::new("latency-test");

    plan.test_case("ping_1s", tests::ping_1s);
    plan.test_case("ping_100ms", tests::ping_100ms);
    plan.test_case("ping_10ms", tests::ping_10ms);
    plan.test_case("ping_flood", tests::ping_flood);
    plan.test_case("verify_log_messages", tests::verify_log_messages);

    plan
}

pub fn quick_test<T: CommandExecutor>() -> TestPlan<T> {
    let mut plan = TestPlan::new("quick-test");

    plan.test_case("ping", tests::ping);
    plan.test_case("iperf3_bidir", tests::iperf3_bidir);
    plan.test_case("verify_log_messages", tests::verify_log_messages);

    plan
}

pub fn phy_an_test<T: CommandExecutor>() -> TestPlan<T> {
    let mut plan = TestPlan::new("phy-an-test");

    plan.test_case(
        "link_mode_and_partner_advertise_all",
        tests::link_mode_and_partner_advertise_all,
    );
    plan.test_case(
        "link_partner_advertise_1000baset_full",
        tests::link_partner_advertise_1000baset_full,
    );
    plan.test_case(
        "link_partner_advertise_100baset_full",
        tests::link_partner_advertise_100baset_full,
    );
    plan.test_case(
        "link_partner_advertise_10baset_full",
        tests::link_partner_advertise_10baset_full,
    );
    plan.test_case(
        "link_mode_advertise_1000baset_full",
        tests::link_mode_advertise_1000baset_full,
    );
    plan.test_case(
        "link_mode_advertise_100baset_full",
        tests::link_mode_advertise_100baset_full,
    );
    plan.test_case(
        "link_mode_advertise_10baset_full",
        tests::link_mode_advertise_10baset_full,
    );
    // Check that the previous tests didn't damage anything when returning to default
    plan.test_case(
        "link_mode_and_partner_advertise_all",
        tests::link_mode_and_partner_advertise_all,
    );
    plan.test_case("verify_log_messages", tests::verify_log_messages);

    plan
}

pub fn phy_quick_test<T: CommandExecutor>() -> TestPlan<T> {
    let mut plan = TestPlan::new("phy-quick-test");

    plan.test_case(
        "link_mode_advertise_1000baset_full",
        tests::link_mode_advertise_1000baset_full,
    );
    plan.test_case(
        "link_mode_advertise_100baset_full",
        tests::link_mode_advertise_100baset_full,
    );
    plan.test_case("link_mode_advertise_all", tests::link_mode_advertise_all);
    plan.test_case("verify_log_messages", tests::verify_log_messages);

    plan
}

pub fn system_test<T: CommandExecutor>() -> TestPlan<T> {
    let mut plan = TestPlan::new("system-test");

    plan.test_case("suspend_resume", tests::suspend_resume);
    plan.test_case("suspend_resume", tests::suspend_resume);
    plan.test_case("disable_checksum_offload", tests::disable_checksum_offload);
    plan.test_case("disable_tso", tests::disable_tso);
    plan.test_case("eee", tests::eee);
    plan.test_case("verify_log_messages", tests::verify_log_messages);

    plan
}

pub fn partner_test<T: CommandExecutor>() -> TestPlan<T> {
    let mut plan = TestPlan::new("partner-test");

    plan.test_case("mtu", tests::mtu);
    plan.test_case("vlan_smoke_test", tests::vlan_smoke_test);
    plan.test_case("ptp_receiver", tests::ptp_receiver);
    plan.test_case("ptp_transmitter", tests::ptp_transmitter);
    plan.test_case("verify_log_messages", tests::verify_log_messages);

    plan
}

pub fn tsn_test<T: CommandExecutor>() -> TestPlan<T> {
    let mut plan = TestPlan::new("tsn-test");

    plan.test_case("profinet_cyclic_only", tsn_tests::profinet_cyclic_only);
    plan.test_case("profinet_rt", tsn_tests::profinet_rt);

    plan
}
