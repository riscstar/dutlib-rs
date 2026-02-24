use expectrl::Error;

use crate::{CommandExecutor, tests};

pub fn smoke_test(
    console: &mut impl CommandExecutor,
    name: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    tests::wait_for_ipv4(console, name)?;

    let mut failures = 0;

    failures += tests::ping(console, ipaddr)?;
    failures += tests::iperf3_bidir(console, ipaddr)?;
    failures += tests::iperf3_tx(console, ipaddr)?;
    failures += tests::iperf3_rx(console, ipaddr)?;

    Ok(failures)
}

pub fn functional_test(
    console: &mut impl CommandExecutor,
    name: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    tests::wait_for_ipv4(console, name)?;

    let mut failures = 0;

    failures += tests::scp_bidir(console, ipaddr)?;
    failures += tests::scp_tx(console, ipaddr)?;
    failures += tests::scp_rx(console, ipaddr)?;

    Ok(failures)
}

pub fn latency_test(
    console: &mut impl CommandExecutor,
    name: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    tests::wait_for_ipv4(console, name)?;

    let mut failures = 0;

    failures += tests::ping_1s(console, ipaddr)?;
    failures += tests::ping_100ms(console, ipaddr)?;
    failures += tests::ping_10ms(console, ipaddr)?;
    failures += tests::ping_flood(console, ipaddr)?;

    Ok(failures)
}
