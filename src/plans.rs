use expectrl::Error;

use crate::{CommandExecutor, Config, tests};

pub fn smoke_test(config: &Config, shell: &mut impl CommandExecutor) -> Result<u32, Error> {
    let adapter = &config.adapter;
    let ipaddr = &config.ipaddr;

    tests::wait_for_ipv4(shell, adapter)?;

    let mut failures = 0;

    failures += tests::ping(shell, ipaddr)?;
    failures += tests::iperf3_bidir(shell, adapter, ipaddr)?;
    failures += tests::iperf3_tx(shell, adapter, ipaddr)?;
    failures += tests::iperf3_rx(shell, adapter, ipaddr)?;
    failures += tests::ethtool_selftest(shell, adapter)?;
    failures += tests::verify_log_messages(shell)?;

    Ok(failures)
}

pub fn functional_test(config: &Config, shell: &mut impl CommandExecutor) -> Result<u32, Error> {
    let adapter = &config.adapter;
    let ipaddr = &config.ipaddr;

    tests::wait_for_ipv4(shell, adapter)?;

    let mut failures = 0;

    failures += tests::scp_bidir(shell, ipaddr)?;
    failures += tests::scp_tx(shell, ipaddr)?;
    failures += tests::scp_rx(shell, ipaddr)?;
    failures += tests::verify_log_messages(shell)?;

    Ok(failures)
}

pub fn bandwidth_test(config: &Config, shell: &mut impl CommandExecutor) -> Result<u32, Error> {
    let adapter = &config.adapter;
    let ipaddr = &config.ipaddr;

    tests::wait_for_ipv4(shell, adapter)?;

    let mut failures = 0;

    failures += tests::iperf3_intervals_bidir(shell, adapter, ipaddr)?;
    failures += tests::iperf3_intervals_tx(shell, adapter, ipaddr)?;
    failures += tests::iperf3_intervals_rx(shell, adapter, ipaddr)?;
    failures += tests::iperf3_udp_bidir(shell, adapter, ipaddr)?;
    failures += tests::iperf3_udp_tx(shell, adapter, ipaddr)?;
    failures += tests::iperf3_udp_rx(shell, adapter, ipaddr)?;
    failures += tests::iperf3_x16_bidir(shell, adapter, ipaddr)?;
    failures += tests::iperf3_x16_tx(shell, adapter, ipaddr)?;
    failures += tests::iperf3_x16_rx(shell, adapter, ipaddr)?;
    failures += tests::verify_log_messages(shell)?;

    Ok(failures)
}

pub fn latency_test(config: &Config, shell: &mut impl CommandExecutor) -> Result<u32, Error> {
    let adapter = &config.adapter;
    let ipaddr = &config.ipaddr;

    tests::wait_for_ipv4(shell, adapter)?;

    let mut failures = 0;

    failures += tests::ping_1s(shell, ipaddr)?;
    failures += tests::ping_100ms(shell, ipaddr)?;
    failures += tests::ping_10ms(shell, ipaddr)?;
    failures += tests::ping_flood(shell, ipaddr)?;
    failures += tests::verify_log_messages(shell)?;

    Ok(failures)
}

pub fn quick_test(
    shell: &mut impl CommandExecutor,
    adapter: &str,
    ipaddr: &str,
) -> Result<u32, Error> {
    tests::wait_for_ipv4(shell, adapter)?;

    let mut failures = 0;

    failures += tests::ping(shell, ipaddr)?;
    failures += tests::iperf3_bidir(shell, adapter, ipaddr)?;
    failures += tests::verify_log_messages(shell)?;

    Ok(failures)
}

pub fn phy_an_test(config: &Config, shell: &mut impl CommandExecutor) -> Result<u32, Error> {
    let adapter = &config.adapter;
    let ipaddr = &config.ipaddr;
    let partner_adapter = &config.partner_adapter;

    tests::wait_for_ipv4(shell, adapter)?;

    let mut failures = 0;

    failures +=
        tests::link_mode_and_partner_advertise_all(shell, adapter, ipaddr, partner_adapter)?;
    failures +=
        tests::link_partner_advertise_1000baset_full(shell, adapter, ipaddr, partner_adapter)?;
    failures +=
        tests::link_partner_advertise_100baset_full(shell, adapter, ipaddr, partner_adapter)?;
    failures +=
        tests::link_partner_advertise_10baset_full(shell, adapter, ipaddr, partner_adapter)?;
    failures += tests::link_mode_advertise_1000baset_full(shell, adapter, ipaddr)?;
    failures += tests::link_mode_advertise_100baset_full(shell, adapter, ipaddr)?;
    failures += tests::link_mode_advertise_10baset_full(shell, adapter, ipaddr)?;
    failures += tests::verify_log_messages(shell)?;

    // Check that the previous tests didn't damage anything when returning to default
    failures +=
        tests::link_mode_and_partner_advertise_all(shell, adapter, ipaddr, partner_adapter)?;

    Ok(failures)
}

pub fn phy_quick_test(config: &Config, shell: &mut impl CommandExecutor) -> Result<u32, Error> {
    let adapter = &config.adapter;
    let ipaddr = &config.ipaddr;

    tests::wait_for_ipv4(shell, adapter)?;

    let mut failures = 0;

    failures += tests::link_mode_advertise_1000baset_full(shell, adapter, ipaddr)?;
    failures += tests::link_mode_advertise_100baset_full(shell, adapter, ipaddr)?;
    failures += tests::link_mode_advertise_all(shell, adapter, ipaddr)?;
    failures += tests::verify_log_messages(shell)?;

    Ok(failures)
}

pub fn system_test(config: &Config, shell: &mut impl CommandExecutor) -> Result<u32, Error> {
    let adapter = &config.adapter;
    let ipaddr = &config.ipaddr;

    tests::wait_for_ipv4(shell, &config.adapter)?;

    let mut failures = 0;

    failures += tests::suspend_resume(shell, adapter, ipaddr)?;
    failures += tests::suspend_resume(shell, adapter, ipaddr)?;
    failures += tests::disable_checksum_offload(shell, adapter, ipaddr)?;
    failures += tests::disable_tso(shell, adapter, ipaddr)?;
    failures += tests::eee(shell, adapter, ipaddr)?;
    failures += tests::verify_log_messages(shell)?;

    Ok(failures)
}

pub fn partner_test(config: &Config, shell: &mut impl CommandExecutor) -> Result<u32, Error> {
    tests::wait_for_ipv4(shell, &config.adapter)?;

    let mut failures = 0;

    failures += tests::mtu(config, shell)?;
    failures += tests::vlan_smoke_test(config, shell)?;
    failures += tests::ptp_receiver(config, shell)?;
    failures += tests::ptp_transmitter(config, shell)?;

    Ok(failures)
}
