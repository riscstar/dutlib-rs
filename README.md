# RB3gen2 semi-automatic tests

## Quickstart

The DUT is partnered with another machine that can be used to test against.
It is assumed the DUT and it's partner are connected with a point-to-point
link.

Partner setup should include:

1. Configuring NetworkManager such that the IPv4 Configuration is "Shared"
   (meaning the partner will share it's connection with the DUT and provide
   it an address via DHCP if asked). The IPv4 address should be
   `192.168.10.2/24` (and check "Never us this network for default route").
2. Install `iperf3` and run the server by default.
3. Create a user called `test` and use `ssh-keygen` and `ssh-copy-id` to
   ensure the DUT can `ssh 192.168.10.2` without needing a password.

Next, edit the source code and replace every reference to `walnut` and
`iot-power-cycle` with something suitable for your environment (feel free to
extract that UART attach and power cycle command into TOML configuration file
if you like).

Note that `iot-power-cycle` is optional. The tests will still work without
it but it will be impossible to automatically recover the board if any
commands does not run to completion.

Finally, try:

    cargo run -- --help

Zealous adopters could also choose to extract `192.168.10.2` into a TOML
configuration file, giving greater flexibility for step #1 above.
