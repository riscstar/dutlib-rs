# RB3gen2 semi-automatic tests

## Building and installing

1. Install a recent Rust toolchain (https://rustup.rs/ ).
2. `git checkout git@github.com:riscstar/dutlib-rs`
3. `cd dutlib-rs`
4. `cargo build --release`
5. `cp target/release/rb3gen2-remote $HOME/bin`

The full test suite must be run from a "link partner" which is typically
an x86 machine with a suitable 2.5Gb/s network card available. However
a subset of the test suite is available to be run from the board itself,
it includes all tests that don't required changes to the link partner's
configuration (for example, the partner tests).

When running from the board itself you will need cross-compiled binaries
(unless you use an arm64 laptop like Daniel does). To simplify this process
cross-compiled binaries are made available by the CI system. Visit
https://github.com/riscstar/dutlib-rs/actions?query=is%3Asuccess+branch%3Amain
follow the link at the top of the list and download the dutlib artifact.
You will need to copy `rb3gen2-test` to `/usr/local/bin` on the target
device.

Note: Both binaries are built by default but only one is required to run tests.
Use `rb3gen2-remote` to run tests from the link partner, use `rb3gen2-local`
to run tests from the target.

## Configuration

Both tools read their configurations from a TOML file as they boot. They will
report an error if this file is missing.

Start with this template, which should be copied to `$HOME/.dutlib/rb2gen2.toml`:

~~~toml
## module is the name of the kernel module that must be loaded before testing
## commences. Translate - to _ if possible (this is required for the logic that
## detect when the wrong module is loaded to function correctly)
#module = "dwmac_tc956x"
module = "tc9564_pci"

## adapter is the name of the adapter to be tested
#adapter = "enP1p5s0f1"
adapter = "eth0"

## ipaddr is the IP address for the link partner. It must be running an iperf3
## and sshd server.
ipaddr = "192.168.10.2"

## [OPTIONAL] console is used by rb3gen2-remote to gain access to the serial
## port in order to console the target device.
console = "ssh -t 192.168.0.4 picocom -b 115200 /dev/serial/by-id/usb-Prolific_Technology_Inc._Prolific_PL2303GD_USB_Serial_COM_Port_DAAOb119D16-if00"

## [OPTIONAL] power_cycle is an optional extension to assist with boot cycle
## testing. This command is run if the device crashes in order to recover it.
## It is not set then boot cycle testing will still work, but will not be able
## to automatically recovery and continue testing.
power_cycle = "iot-power-cycle rb3gen2"

## [OPTIONAL] partner_adapter is the name of the link partner's adapter and is
## used by partner tests to alter the link partners setting.
partner_adapter = "enP1p3s0"
~~~

## Link partner setup

The DUT is partnered with another machine that can be used to test against.
It is assumed the DUT and it's partner are connected with a point-to-point
link (although only the partner tests run by `rb3gen2-remote` require a
point-to-point link).

Link partner setup should include:

1. Configuring NetworkManager such that the IPv4 Configuration is "Shared"
   (meaning the partner will share it's connection with the DUT and provide
   it an address via DHCP if asked). The IPv4 address should match whatever it
   provided in `rb3gen2.toml` and the recommended value is
   `192.168.10.2/24` (and check "Never us this network for default route").
2. Install `iperf3` and run the server by default.
3. Create a user called `test` and use `ssh-keygen` and `ssh-copy-id` to
   ensure the DUT can `ssh 192.168.10.2` without needing a password.

## Additional configuration for remote runners

The test suite needs to make local changes to your networking configuration
and will use `sudo` to do so. The simplest way to enable this is to configure
sudo to allow it to operate without a password:

~~~
%sudo ALL=(ALL) NOPASSWD: ALL
~~~

The test suite requires only three commands: `ip`, `ethtool` and `timeout` and
`sudo` could be configured only to allow these three tools (rather then `ALL`)
however since `timeout` essentially grants unrestricted shell access anyway
there is little point in doing to.

## Running tests

Both tools provide comprehensive built-in help describing their command line
options. Be aware that both tools adopt a `git-like` mechanism of sub-commands
and argument position matters.

Global parameters should be to the left of the sub-command (if there is one):

`rb3gen2-test --help`
`rb3gen2-test --verbose smoke-test`

Sub-command parameters go on the right:

`rb3gen2-test all-tests --help`

## Common test plans

Note: To list other test plans use `--help`

### Smoke test

The smoke test is very fast to run and consists of a single ping, three iperf3
invocations and, if CONFIG_STMMAC_SELFTESTS is enabled, the stmmac self tests.

Choose one of the following, as appropriate:

~~~sh
rb3gen2-test -q smoke-test
rb3gen2-remote -q smoke-test
~~~

### All tests

Full testing is also useful. Be aware that `rb3gen2-test` cannot run all
available tests (in particular it cannot run MTU, VLAN or PTP tests).

Choose one of the following, as appropriate:

~~~sh
rb3gen2-test -q all-tests
rb3gen2-remote -q all-tests
~~~

### Boot cycle testing

Boot cycle testing allows us to reboot the board and run the smoke test in a
loop. The tooling with initial attempt to reboot the board by issuing a
`reboot` command but if the board crashes it will attempt to `power_cycle`
(if it is set) to recover a crashed board and continue gathering statistics.

~~~sh
rb3gen2-remote -q power-cycle --cycles 25 --plan smoke-test
~~~

Note that, because the board will be rebooted, is important to configure
systemd-boot to automatically load the correct kernel!
