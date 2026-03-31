use expectrl::Error;

use crate::{CommandExecutor, Config, tests};

type TestFunc<T> = fn(&Config, &mut T) -> Result<u32, Error>;

struct TestCase<T: CommandExecutor> {
    name: &'static str,
    code: TestFunc<T>,
}

impl<T: CommandExecutor> TestCase<T> {
    pub fn new(name: &'static str, code: TestFunc<T>) -> Self {
        Self { name, code }
    }

    pub fn run(&self, config: &Config, shell: &mut T) -> Result<u32, Error> {
        let name = self.name;
        match (self.code)(config, shell) {
            Ok(0) => {
                log::info!("PASSED: {name}");
                Ok(0)
            }
            Ok(failures) => {
                log::error!("FAILED: {name} (reported {failures} failures)");
                Ok(failures)
            }
            Err(e) => {
                log::error!("ABORTED: {name}: {e}");
                Err(e)
            }
        }
    }
}

enum TestSet<T: CommandExecutor> {
    TestCase(TestCase<T>),
    TestPlan(TestPlan<T>),
}

pub struct TestPlan<T: CommandExecutor> {
    name: &'static str,
    plan: Vec<TestSet<T>>,
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

    pub fn run(&self, config: &Config, shell: &mut T) -> Result<u32, Error> {
        let name = self.name;
        log::info!("Running {name} plan");
        let mut failures = 0;

        for t in self.plan.iter() {
            failures += match t {
                TestSet::TestCase(test_case) => test_case.run(config, shell)?,
                TestSet::TestPlan(test_plan) => test_plan.run(config, shell)?,
            }
        }

        match failures {
            0 => log::info!("Completed {name} plan successfully"),
            n => log::error!("Completed {name} plan but reported {n} failures"),
        }
        Ok(failures)
    }
}

pub fn smoke_test_new<T: CommandExecutor>() -> TestPlan<T> {
    let mut plan = TestPlan::new("smoke-test");

    plan.test_case("ping", tests::ping);
    plan.test_case("iperf3_bidir", tests::iperf3_bidir);
    plan.test_case("iperf3_tx", tests::iperf3_tx);
    plan.test_case("iperf3_rx", tests::iperf3_rx);
    plan.test_case("ethtool_selftest", tests::ethtool_selftest);
    plan.test_case("verify_log_messages", tests::verify_log_messages);

    plan
}

pub fn smoke_test(config: &Config, shell: &mut impl CommandExecutor) -> Result<u32, Error> {
    tests::wait_for_ipv4(config, shell)?;
    smoke_test_new().run(config, shell)
}

pub fn functional_test_new<T: CommandExecutor>() -> TestPlan<T> {
    let mut plan = TestPlan::new("functional-test");

    plan.test_case("scp_bidir", tests::scp_bidir);
    plan.test_case("scp_tx", tests::scp_tx);
    plan.test_case("scp_rx", tests::scp_rx);
    plan.test_case("verify_log_messages", tests::verify_log_messages);

    plan
}

pub fn functional_test(config: &Config, shell: &mut impl CommandExecutor) -> Result<u32, Error> {
    tests::wait_for_ipv4(config, shell)?;
    functional_test_new().run(config, shell)
}

pub fn bandwidth_test_new<T: CommandExecutor>() -> TestPlan<T> {
    let mut plan = TestPlan::new("bandwidth-test");

    plan.test_case("iperf3_intervals_bidir", tests::iperf3_intervals_bidir);
    plan.test_case("iperf3_intervals_tx", tests::iperf3_intervals_tx);
    plan.test_case("iperf3_intervals_rx", tests::iperf3_intervals_rx);
    plan.test_case("iperf3_udp_bidir", tests::iperf3_udp_bidir);
    plan.test_case("iperf3_udp_bidir", tests::iperf3_udp_tx);
    plan.test_case("iperf3_udp_bidir", tests::iperf3_udp_rx);
    plan.test_case("iperf3_x16_bidir", tests::iperf3_x16_bidir);
    plan.test_case("iperf3_x16_bidir", tests::iperf3_x16_tx);
    plan.test_case("iperf3_x16_bidir", tests::iperf3_x16_rx);
    plan.test_case("verify_log_messages", tests::verify_log_messages);

    plan
}
pub fn bandwidth_test(config: &Config, shell: &mut impl CommandExecutor) -> Result<u32, Error> {
    tests::wait_for_ipv4(config, shell)?;
    bandwidth_test_new().run(config, shell)
}

pub fn latency_test_new<T: CommandExecutor>() -> TestPlan<T> {
    let mut plan = TestPlan::new("bandwidth-test");

    plan.test_case("ping_1s", tests::ping_1s);
    plan.test_case("ping_100ms", tests::ping_100ms);
    plan.test_case("ping_10ms", tests::ping_10ms);
    plan.test_case("ping_flood", tests::ping_flood);
    plan.test_case("verify_log_messages", tests::verify_log_messages);

    plan
}

pub fn latency_test(config: &Config, shell: &mut impl CommandExecutor) -> Result<u32, Error> {
    tests::wait_for_ipv4(config, shell)?;
    latency_test_new().run(config, shell)
}

pub fn quick_test_new<T: CommandExecutor>() -> TestPlan<T> {
    let mut plan = TestPlan::new("quick-test");

    plan.test_case("ping", tests::ping);
    plan.test_case("iperf3_bidir", tests::iperf3_bidir);
    plan.test_case("verify_log_messages", tests::verify_log_messages);

    plan
}

pub fn quick_test(config: &Config, shell: &mut impl CommandExecutor) -> Result<u32, Error> {
    tests::wait_for_ipv4(config, shell)?;
    quick_test_new().run(config, shell)
}

pub fn phy_an_test_new<T: CommandExecutor>() -> TestPlan<T> {
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

pub fn phy_an_test(config: &Config, shell: &mut impl CommandExecutor) -> Result<u32, Error> {
    tests::wait_for_ipv4(config, shell)?;
    phy_an_test_new().run(config, shell)
}

pub fn phy_quick_test_new<T: CommandExecutor>() -> TestPlan<T> {
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

pub fn phy_quick_test(config: &Config, shell: &mut impl CommandExecutor) -> Result<u32, Error> {
    tests::wait_for_ipv4(config, shell)?;
    phy_quick_test_new().run(config, shell)
}

pub fn system_test_new<T: CommandExecutor>() -> TestPlan<T> {
    let mut plan = TestPlan::new("system-test");

    plan.test_case("suspend_resume", tests::suspend_resume);
    plan.test_case("suspend_resume", tests::suspend_resume);
    plan.test_case("disable_checksum_offload", tests::disable_checksum_offload);
    plan.test_case("disable_tso", tests::disable_tso);
    plan.test_case("eee", tests::eee);
    plan.test_case("verify_log_messages", tests::verify_log_messages);

    plan
}

pub fn system_test(config: &Config, shell: &mut impl CommandExecutor) -> Result<u32, Error> {
    tests::wait_for_ipv4(config, shell)?;
    system_test_new().run(config, shell)
}

pub fn partner_test_new<T: CommandExecutor>() -> TestPlan<T> {
    let mut plan = TestPlan::new("partner-test");

    plan.test_case("mtu", tests::mtu);
    plan.test_case("vlan_smoke_test", tests::vlan_smoke_test);
    plan.test_case("ptp_receiver", tests::ptp_receiver);
    plan.test_case("ptp_transmitter", tests::ptp_transmitter);
    plan.test_case("verify_log_messages", tests::verify_log_messages);

    plan
}

pub fn partner_test(config: &Config, shell: &mut impl CommandExecutor) -> Result<u32, Error> {
    tests::wait_for_ipv4(config, shell)?;
    partner_test_new().run(config, shell)
}
