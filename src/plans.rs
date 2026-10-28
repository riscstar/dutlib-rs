use crate::{
    CommandExecutor, Config, UnrecoverableError, doc_lookup, tests,
    tsn_tests::{self},
    tuning,
};

type TestFunc<T> = fn(&Config, &mut T) -> Result<u32, UnrecoverableError>;

/// Derive a test label from the path of the function implementing it.
///
/// The label is the final segment of the path, so `tests::iperf3_bidir`
/// becomes `"iperf3_bidir"`. Tying the label to the function path means a
/// global rename of the function updates the label too, rather than leaving a
/// stale string literal behind.
macro_rules! test_name {
    ($prefix:ident :: $($rest:ident)::+) => {
        test_name!($($rest)::+)
    };
    ($name:ident) => {
        stringify!($name)
    };
}

/// Add a test case to a plan, deriving the label from the function's path.
///
/// `test_case!(plan, tests::iperf3_bidir)` is equivalent to
/// `plan.test_case("iperf3_bidir", tests::iperf3_bidir)`.
macro_rules! test_case {
    ($plan:ident, $($path:ident)::+) => {
        $plan.test_case(test_name!($($path)::+), $($path)::+);
    };
}

/// The common `--select` command line arguments shared by the test
/// executables.
///
/// More than one selection may be given (either as repeated arguments or as
/// several values after a single `--select`); a test is retained if its name
/// contains any of the selections.
#[derive(Clone, Debug, clap::Args)]
pub struct Selection {
    /// Skip all tests that do not match one of the selections
    #[arg(short, long, num_args = 1..)]
    select: Vec<String>,
}

impl Selection {
    /// Does the named test match one of the selections?
    pub fn matches(&self, name: &str) -> bool {
        self.select.iter().any(|s| name.contains(s.as_str()))
    }

    /// Filter `plan` using the selections (a no-op if nothing was selected).
    pub fn filter_plan<T: CommandExecutor>(&self, plan: TestPlan<T>) -> TestPlan<T> {
        if self.select.is_empty() {
            plan
        } else {
            plan.filter(|f| self.matches(f))
        }
    }
}

/// Join a plan `path` and a child `name` using `/` as the separator.
fn join_path(path: &str, name: &str) -> String {
    if path.is_empty() || path.ends_with('/') {
        format!("{path}{name}")
    } else {
        format!("{path}/{name}")
    }
}

/// Render an ATX markdown heading prefix of the given level. Markdown only
/// supports six heading levels so deeper levels are clamped.
fn heading(level: usize) -> String {
    "#".repeat(level.clamp(1, 6))
}

/// Render an italic "Link to source" markdown line for a test function.
fn source_link(entry: &doc_lookup::DocEntry) -> String {
    format!(
        "*Link to source: [{}]({})*",
        entry.path,
        doc_lookup::source_url(entry)
    )
}

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

    pub fn run(
        &self,
        config: &Config,
        shell: &mut T,
        path: &str,
    ) -> Result<u32, UnrecoverableError> {
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

impl<T: CommandExecutor> TestSet<T> {}

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

    /// Filter the plan, keeping only the test cases whose name matches the
    /// predicate.
    ///
    /// Nested plans are filtered recursively and are dropped when none of the
    /// test cases they contain match.
    pub fn filter(&self, predicate: impl Fn(&str) -> bool) -> TestPlan<T> {
        self.filter_by(&predicate)
    }

    /// The implementation of [`TestPlan::filter`]. The predicate is passed by
    /// reference (rather than by value) so that the recursive calls do not
    /// monomorphise a new closure type at every level.
    fn filter_by(&self, predicate: &dyn Fn(&str) -> bool) -> TestPlan<T> {
        Self {
            name: self.name,
            plan: self
                .plan
                .iter()
                .filter_map(|ts| match ts {
                    TestSet::TestCase(test_case) => {
                        if predicate(test_case.name) {
                            Some(TestSet::TestCase(test_case.clone()))
                        } else {
                            None
                        }
                    }
                    TestSet::TestPlan(test_plan) => {
                        let filtered = test_plan.filter_by(predicate);
                        if filtered.plan.is_empty() {
                            None
                        } else {
                            Some(TestSet::TestPlan(filtered))
                        }
                    }
                })
                .collect(),
        }
    }

    pub fn run_with_path(
        &self,
        config: &Config,
        shell: &mut T,
        path: &str,
    ) -> Result<u32, UnrecoverableError> {
        let name = join_path(path, self.name);
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

    pub fn run(&self, config: &Config, shell: &mut T) -> Result<u32, UnrecoverableError> {
        log::info!("TUNING: Automatically applying system tuning");
        tuning::autotune(config, shell)?;

        self.run_with_path(config, shell, "")
    }

    /// Print the name of every test case in the plan.
    ///
    /// Nested plans are expanded and each test is printed on its own line using
    /// the same path that would be logged when the test is run (for example
    /// `smoke-test/ping`).
    pub fn list(&self) {
        self.list_with_path("");
    }

    fn list_with_path(&self, path: &str) {
        let name = join_path(path, self.name);
        for t in self.plan.iter() {
            match t {
                TestSet::TestCase(test_case) => println!("{name}/{}", test_case.name),
                TestSet::TestPlan(test_plan) => test_plan.list_with_path(&name),
            }
        }
    }

    /// Print the plan as markdown.
    ///
    /// Every plan and test case is rendered as a heading using its local
    /// name (for example `all-tests`, `smoke-test`, `ping`). Heading levels
    /// follow the nesting depth of the plan, clamped to the six levels markdown
    /// supports. Each test is followed by an italic "Link to source" line
    /// pointing at the function's definition, then by its rustdoc comment
    /// (collected at build time by `build.rs`) passed through unmodified.
    /// Tests without a rustdoc comment are listed with the link only.
    /// Blocks are separated by blank lines.
    pub fn describe(&self) {
        let mut blocks: Vec<String> = Vec::new();
        self.collect_markdown(1, &mut blocks);

        if !blocks.is_empty() {
            println!("{}", blocks.join("\n\n"));
        }
    }

    /// Collect the plan as markdown blocks (headings and rustdoc paragraphs).
    ///
    /// `level` is the markdown heading level of this plan; its test cases use
    /// `level + 1`. Empty plans contribute nothing so that no headings are
    /// emitted without content beneath them.
    fn collect_markdown(&self, level: usize, blocks: &mut Vec<String>) {
        if self.plan.is_empty() {
            return;
        }

        blocks.push(format!("{} {}", heading(level), self.name));

        for t in self.plan.iter() {
            match t {
                TestSet::TestCase(test_case) => {
                    blocks.push(format!("{} {}", heading(level + 1), test_case.name));
                    if let Some(entry) = doc_lookup::lookup_entry(test_case.name) {
                        blocks.push(source_link(entry));
                        if let Some(doc) = entry.doc {
                            blocks.push(doc.to_owned());
                        }
                    }
                }
                TestSet::TestPlan(test_plan) => test_plan.collect_markdown(level + 1, blocks),
            }
        }
    }
}

#[cfg(test)]
mod plan_tests {
    use super::*;
    use crate::native::NativeExecutor;

    fn dummy_test(_: &Config, _: &mut NativeExecutor) -> Result<u32, UnrecoverableError> {
        Ok(0)
    }

    #[test]
    fn describe_renders_markdown() {
        let mut plan = TestPlan::<NativeExecutor>::new("outer");
        plan.test_case("ping", tests::ping);
        plan.test_case("definitely_not_a_test_function", dummy_test);

        let mut blocks: Vec<String> = Vec::new();
        plan.collect_markdown(1, &mut blocks);

        // Headings use local names, each test links back to its source
        // (italic, before the doc), the rustdoc is passed through unmodified
        // and unknown tests get a heading with no content.
        let entry = doc_lookup::lookup_entry("ping").unwrap();
        assert_eq!(
            blocks.join("\n\n"),
            format!(
                "# outer\n\n## ping\n\n{}\n\n{}\n\n## definitely_not_a_test_function",
                source_link(entry),
                entry.doc.unwrap()
            )
        );
    }

    #[test]
    fn filter_recurses_into_nested_plans() {
        let mut inner = TestPlan::<NativeExecutor>::new("inner");
        inner.test_case("ping", tests::ping);
        inner.test_case("ping_1s", tests::ping_1s);
        inner.test_case("verify_log_messages", tests::verify_log_messages);

        let mut outer = TestPlan::<NativeExecutor>::new("outer");
        outer.test_plan(inner);
        outer.test_case("verify_log_messages", tests::verify_log_messages);

        let filtered = outer.filter(|name| name.contains("ping"));

        // The nested plan is retained with only its matching tests and the
        // top-level verify_log_messages test case is dropped.
        assert_eq!(filtered.plan.len(), 1);
        match &filtered.plan[0] {
            TestSet::TestPlan(sub_plan) => {
                assert_eq!(sub_plan.name, "inner");
                let names: Vec<&str> = sub_plan
                    .plan
                    .iter()
                    .map(|ts| match ts {
                        TestSet::TestCase(test_case) => test_case.name,
                        _ => panic!("expected only test cases"),
                    })
                    .collect();
                assert_eq!(names, ["ping", "ping_1s"]);
            }
            _ => panic!("expected the filtered sub-plan to be retained"),
        }
    }

    #[test]
    fn filter_drops_empty_nested_plans() {
        let mut inner = TestPlan::<NativeExecutor>::new("inner");
        inner.test_case("verify_log_messages", tests::verify_log_messages);

        let mut outer = TestPlan::<NativeExecutor>::new("outer");
        outer.test_plan(inner);

        let filtered = outer.filter(|name| name.contains("ping"));
        assert!(filtered.plan.is_empty());
    }

    #[test]
    fn describe_renders_nested_plans_with_deeper_headings() {
        let mut inner = TestPlan::<NativeExecutor>::new("inner");
        inner.test_case("ping", tests::ping);

        let mut outer = TestPlan::<NativeExecutor>::new("outer");
        outer.test_plan(inner);

        let mut blocks: Vec<String> = Vec::new();
        outer.collect_markdown(1, &mut blocks);

        let entry = doc_lookup::lookup_entry("ping").unwrap();
        assert_eq!(
            blocks,
            [
                "# outer".to_string(),
                "## inner".to_string(),
                "### ping".to_string(),
                source_link(entry),
                entry.doc.unwrap().to_string(),
            ]
        );
    }

    #[test]
    fn describe_links_undocumented_tests() {
        let mut plan = TestPlan::<NativeExecutor>::new("quick-test");
        plan.test_case("iperf3_bidir", tests::iperf3_bidir);

        let mut blocks: Vec<String> = Vec::new();
        plan.collect_markdown(1, &mut blocks);

        // iperf3_bidir has no rustdoc comment but is still captured, so it
        // gets a heading and a source link with no documentation.
        let entry = doc_lookup::lookup_entry("iperf3_bidir").unwrap();
        assert!(entry.doc.is_none());
        assert_eq!(
            blocks,
            [
                "# quick-test".to_string(),
                "## iperf3_bidir".to_string(),
                source_link(entry),
            ]
        );
    }

    #[test]
    fn describe_of_an_empty_plan_produces_nothing() {
        let plan = TestPlan::<NativeExecutor>::new("empty");

        let mut blocks: Vec<String> = Vec::new();
        plan.collect_markdown(1, &mut blocks);

        assert!(blocks.is_empty());
    }

    #[test]
    fn heading_levels_are_clamped() {
        assert_eq!(heading(1), "#");
        assert_eq!(heading(6), "######");
        assert_eq!(heading(7), "######");
    }
}

pub fn smoke_test<T: CommandExecutor>() -> TestPlan<T> {
    let mut plan = TestPlan::new("smoke-test");

    test_case!(plan, tests::ping);
    test_case!(plan, tests::iperf3_bidir);
    test_case!(plan, tests::iperf3_tx);
    test_case!(plan, tests::iperf3_rx);
    test_case!(plan, tests::ethtool_selftest);
    test_case!(plan, tests::verify_log_messages);

    plan
}

pub fn functional_test<T: CommandExecutor>() -> TestPlan<T> {
    let mut plan = TestPlan::new("functional-test");

    test_case!(plan, tests::scp_bidir);
    test_case!(plan, tests::scp_tx);
    test_case!(plan, tests::scp_rx);
    test_case!(plan, tests::verify_log_messages);

    plan
}

pub fn bandwidth_test<T: CommandExecutor>() -> TestPlan<T> {
    let mut plan = TestPlan::new("bandwidth-test");

    test_case!(plan, tests::iperf3_intervals_bidir);
    test_case!(plan, tests::iperf3_intervals_tx);
    test_case!(plan, tests::iperf3_intervals_rx);
    test_case!(plan, tests::iperf3_udp_bidir);
    test_case!(plan, tests::iperf3_udp_tx);
    test_case!(plan, tests::iperf3_udp_rx);
    test_case!(plan, tests::iperf3_x16_bidir);
    test_case!(plan, tests::iperf3_x16_tx);
    test_case!(plan, tests::iperf3_x16_rx);
    test_case!(plan, tests::verify_log_messages);

    plan
}

pub fn latency_test<T: CommandExecutor>() -> TestPlan<T> {
    let mut plan = TestPlan::new("latency-test");

    test_case!(plan, tests::ping_1s);
    test_case!(plan, tests::ping_100ms);
    test_case!(plan, tests::ping_10ms);
    test_case!(plan, tests::ping_flood);
    test_case!(plan, tests::verify_log_messages);

    plan
}

pub fn quick_test<T: CommandExecutor>() -> TestPlan<T> {
    let mut plan = TestPlan::new("quick-test");

    test_case!(plan, tests::ping);
    test_case!(plan, tests::iperf3_bidir);
    test_case!(plan, tests::verify_log_messages);

    plan
}

pub fn phy_an_test<T: CommandExecutor>() -> TestPlan<T> {
    let mut plan = TestPlan::new("phy-an-test");

    test_case!(plan, tests::link_mode_and_partner_advertise_all);
    test_case!(plan, tests::link_partner_advertise_1000baset_full);
    test_case!(plan, tests::link_partner_advertise_100baset_full);
    test_case!(plan, tests::link_partner_advertise_10baset_full);
    test_case!(plan, tests::link_mode_advertise_1000baset_full);
    test_case!(plan, tests::link_mode_advertise_100baset_full);
    test_case!(plan, tests::link_mode_advertise_10baset_full);
    // Check that the previous tests didn't damage anything when returning to default
    test_case!(plan, tests::link_mode_and_partner_advertise_all);
    test_case!(plan, tests::verify_log_messages);

    plan
}

pub fn phy_quick_test<T: CommandExecutor>() -> TestPlan<T> {
    let mut plan = TestPlan::new("phy-quick-test");

    test_case!(plan, tests::link_mode_advertise_1000baset_full);
    test_case!(plan, tests::link_mode_advertise_100baset_full);
    test_case!(plan, tests::link_mode_advertise_all);
    test_case!(plan, tests::verify_log_messages);

    plan
}

pub fn system_test<T: CommandExecutor>() -> TestPlan<T> {
    let mut plan = TestPlan::new("system-test");

    test_case!(plan, tests::iperf3_udp_tx_all_queues);
    test_case!(plan, tests::suspend_resume);
    test_case!(plan, tests::suspend_resume);
    test_case!(plan, tests::disable_checksum_offload);
    test_case!(plan, tests::disable_tso);
    test_case!(plan, tests::eee);
    test_case!(plan, tests::verify_log_messages);

    plan
}

pub fn partner_test<T: CommandExecutor>() -> TestPlan<T> {
    let mut plan = TestPlan::new("partner-test");

    test_case!(plan, tests::mtu);
    test_case!(plan, tests::vlan_smoke_test);
    test_case!(plan, tests::ptp_receiver);
    test_case!(plan, tests::ptp_transmitter);
    test_case!(plan, tests::verify_log_messages);

    plan
}

pub fn tsn_test<T: CommandExecutor>() -> TestPlan<T> {
    let mut plan = TestPlan::new("tsn-test");

    test_case!(plan, tsn_tests::netperf_pair_nop);
    test_case!(plan, tsn_tests::netperf_pair_4q_taprio);
    test_case!(plan, tsn_tests::netperf_pair_4q_mqprio);
    test_case!(plan, tsn_tests::netperf_pair_4q_cbs);
    test_case!(plan, tsn_tests::netperf_pair_5q_cbs);

    test_case!(plan, tsn_tests::netperf_quad_nop);
    test_case!(plan, tsn_tests::netperf_quad_4q_taprio);
    test_case!(plan, tsn_tests::netperf_quad_4q_mqprio);
    test_case!(plan, tsn_tests::netperf_quad_5q_mqprio);
    test_case!(plan, tsn_tests::netperf_quad_4q_cbs);
    test_case!(plan, tsn_tests::netperf_quad_5q_cbs);

    test_case!(plan, tsn_tests::profinet_rt_nop);
    test_case!(plan, tsn_tests::profinet_rt_4q_taprio);
    test_case!(plan, tsn_tests::profinet_cyclic_4q_taprio);
    test_case!(plan, tsn_tests::profinet_rt_4q_mqprio_xdp);
    test_case!(plan, tsn_tests::profinet_rt_4q_mqprio_noxdp);
    test_case!(plan, tsn_tests::profinet_rt_4q_cbs);
    test_case!(plan, tsn_tests::profinet_rt_5q_cbs);

    test_case!(plan, tsn_tests::txrx_tsn_4q_etf_network_idle);
    test_case!(plan, tsn_tests::txrx_tsn_4q_etf_network_busy);
    test_case!(plan, tsn_tests::txrx_tsn_5q_etf_network_idle);
    test_case!(plan, tsn_tests::txrx_tsn_5q_etf_network_busy);
    test_case!(plan, tsn_tests::txrx_tsn_5q_tbs_network_idle);
    test_case!(plan, tsn_tests::txrx_tsn_5q_tbs_network_busy);
    test_case!(plan, tsn_tests::txrx_tsn_5q_tbs_fpe_network_busy);

    plan
}
