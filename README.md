# RB3gen2 semi-automatic tests

## Overview

The _rb3gen2_ test suite performs a set of tests to validate Ethernet
functionality implemented on the two interfaces supported on the
Qualcomm RB3gen2 robotics platform.  They involve a single RB3gen2
system and a separate "link partner" system.  The two systems are
connected directly using a Cat6 Ethernet cable.

The test suite runs in two modes: "local" and "remote".  Both modes
_use_ the Ethernet connection, and involve processing on both systems.
In "local" mode, tests are initiated on the device under test (the _DUT_,
which is the RB3gen2).  In local mode the DUT runs the `rb3gen2-test`
executable.  Tests on the DUT are run as the superuser.  Local mode
tests are a subset of the "full suite."

The full set of tests are run in "remote" mode.  For remote mode,
running the test suite is initiated on the link partner system, where
`rb3gen2-remote` is executed.  Tests on the link partner system are
run under another user, which we'll call the _remote user_.  (This can
be your own account, for example.)

When running `rb3gen2-remote`, certain programs require superuser access
and authentication.  We configure the `sudoers` file on the link partner
system to allow passwordless access to these specific commands.

Finally, a `test` user account (which we will create) on the link
partner system is used for `scp`-based file transfer tests.

The instructions that follow assume that both the DUT and the link partner are
running Debian or one of it's derivatives (e.g. Ubuntu).

## Setting up the DUT

Setting up the DUT involves ensuring certain commands are installed,
configuring the DUT to automatically log as root after reboot,
installing the test program, and installing the configuration file.

Commands that must be installed on the DUT for this test suite
include: dmesg, ethtool, ip, iperf3, linuxptp, ping, and scp.
Most of these should be already present, but just in case:

~~~sh
apt update
apt install \
  util-linux ethtool iproute2 iperf3 linuxptp iputils-ping \
  openssh-client libmosquitto1 libxdp1
~~~

### Setting up `systemd` to automatically log in as root after reboot

To run remote mode tests, the test program assumes that the console
terminal is in a logged-in state.  To ensure this following a reboot
(which can be forced in the event of a test timeout), we configure
`systemd` to automatically log in to root on the console terminal
(`/dev/ttyMSM0`).  This involves editing three files.

**First**, on the DUT, run this command root:

~~~sh
systemctl edit serial-getty@ttyMSM0.service
~~~

Near the top of the file, add the following lines.  The first `ExecStart`
clears the previous value, and the second simply adds `--autologin root`
to what was the previous value.  (The previous value is found, commented
out, later in the file.)

~~~text
[Service]
ExecStart=
ExecStart=-/sbin/agetty -o '-- \\u' --noreset --noclear --autologin root --keep-baud 115200,57600,38400,9600 - ${TERM}
~~~

Save and quit.


**Next**, we must indicate the console terminal is secure.  This involves
including `ttyMSM0` in `/etc/securetty`.  This command does it (but be
careful).

~~~sh
echo ttyMSM0 >> /etc/securetty
~~~

**Third**, we must allow passwordless root login on the DUT.
Edit `/etc/pam.d/common-auth` and add this near the top, then save and quit:

~~~text
# Allow passwordless login for root on a securetty
auth sufficient pam_listfile.so item=tty sense=allow file=/etc/securetty onerr=fail apply=root
~~~

### Installing the test program

It is not strictly necessary to build the `rb3gen2-test` executable
for the DUT.  A pre-built binary can be downloaded this way:

1. Visit https://github.com/riscstar/dutlib-rs/actions?query=is%3Asuccess+branch%3Amain
2.  Follow the first link in the workflow run results list, and download
the **dutlib** file in the Artifacts section.
3.  Extract `dutlib.zip`, which contains a compressed tar file
4.  Extract the tar archive with:
    ~~~sh
    tar -x --zstd -f dutlib-v*.tar.zst
    ~~~
5.  Within the extracted directory there is a `bin` directory that
contains both executables (compiled for Arm architecture)

On the DUT, only `rb3gen2-test` is needed, and that should be installed
in `/usr/local/bin`.

### Installing the configuration file

Finally, a configuration file must be installed on the DUT.  This goes
in a new `.dutlib` directory in the root account's home directory.  A
template for its contents is provided below, but the IP address and
other values used should be adjusted to suit your environment.
(More on this at the end.)

## Setting up the link partner

The link partner is connected to the DUT using an Ethernet cable
connected to an interface on both machines.  These two interfaces
will share a private IP network.  The link partner interface will
be configured to be _shared_, which causes it to provide a DHCP
address for the DUT interface on the other end of the cable.

To successfully run the test suite on the link partner certain
commands must be installed.  Again, many of these are probably
already in place, but just in case:

~~~sh
sudo apt update
sudo apt install \
  util-linux ethtool iproute2 iperf3 linuxptp iputils-ping \
  openssh-client openssh-server libmosquitto1 libxdp1
~~~

When installing `iperf3`, it should be set up to start the `systemd`
service automatically.  And openssh-server should be configured to
allow inbound ssh connections.

### Setting up passwordless access to privileged commands

The test suite needs to make changes to the networking configuration
on both the DUT and the link partner.  On the DUT we run as the
superuser, which has the privilege needed to do this.

On the link partner will use `sudo` to perform these changes, and
to avoid interrupting testing with authentication prompts, we configure
`sudo` to allow the needed commands to operate without a password.
Adding lines to the `sudoers` file on the DUT accomplishes this.

- Edit the `sudoers` file on the link partner system by running the `visudo`
command as superuser (via `sudo`).
- Near the end of the file, you'll see this line:

~~~text
%sudo   ALL=(ALL:ALL) ALL
~~~

- Under that, *add* these lines:

~~~text
# Allow members of the sudo group to execute specific commands without a
# password.
%sudo	ALL=(ALL:ALL) NOPASSWD: /usr/sbin/ethtool
%sudo	ALL=(ALL:ALL) NOPASSWD: /usr/sbin/ip
%sudo	ALL=(ALL:ALL) NOPASSWD: /usr/bin/timeout ^[0-9][0-9]* ptp4l .*$
~~~

- Save and quit.

### Configuring the link partner Ethernet port

The NetworkManager GUI does not expose all the capabilities needed for
configuring the link partner interface in shared mode, so we'll
use the TUI instead.

On the link partner system, as superuser (via `sudo`), run the
`nmcli` program.

1.  Select "Edit a connection"
2.  Select the connection that will be used for testing (e.g. "Wired
Connection 1"), and select the <Edit...> option.
3.  Next to IPv4 CONFIGURATION, change <Manual> to be <Shared>
4.  In the IPv4 CONFIGURATION section, set the IP address to use for
the interface in the "Addresses" field.  A good option is `192.168.10.2/24`.
5.  Set the value of the "Gateway" field to the same IP address used in the
previous step (e.g., `192.168.10.2`).
6.  Select the "Never use this network for default route" option
7.  Select <OK> at the end to save this configuration.
8.  Press ESC and then select "Quit".

Now connect a Cat6 Ethernet cable directly between this interface on
the link partner system and the Ethernet interface to be tested on
the DUT.  The connected DUT interface should automatically be issued
an IP address in the selected subnet (`192.168.10.0/24`)

### Setting up the `test` account on the link partner

We'll use a `test` account on the link partner system to provide a
place to create large files used in transfer tests.  After this
account is set up we will configure the DUT so it has passwordless
ssh access to it.

If you already have a suitable account set up for this, you can skip
this section.

To set up a test user:

1.  As superuser on the link parter system, run `adduser test`
2.  Supply a password (twice)
3.  Supply a full name, along with any other appropriate metadata (room number,
    work phone, home phone, etc.). All of these can be skipped by pressing return.
4.  Confirm the information is correct.

### Installing (and building) the test program

Next we need to install the `rb3gen2-remote` program.  If your link
partner system is a 64-bit Arm system, you can use the `rb3gen2-remote`
binary extracted earlier, and you can skip the over the build
instructions that follow.

To build `rb3gen2-remote`, do the following on the link partner system.
(This could be done on a separate development system instead, as long
as it builds executables appropriate for the link partner system.)
You need to install a recent Rust toolchain.  The first command below
does this, using the instructions at `https://rustup.rs/`.

~~~sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
git clone git@github.com:riscstar/dutlib-rs
cd dutlib-rs
cargo build --release
~~~

Once built, the `rb3gen2-remote` executable is found under `target/release`.

This file should be installed in the `bin` directory for the
account used to initiate testing (i.e., _your_ account).  (This
assumes the `bin` directory is in your path.)

~~~sh
mkdir -p ~/bin
cp target/release/rb3gen2-remote ~/bin
~~~

### Installing the configuration file

The same configuration file used on the DUT must be installed on
the link partner system, in the new `.dutlib` directory in the
home directory of the user to be used to initiate testing (i.e.,
_your_ account).  (More on this next.)

## Installing the configuration file(s)

A configuration file must be installed on both the DUT and the
link partner.   This configuration file is interpreted by the
test executable (`rb3gen2-local` on the DUT and `rb3gen2-remote`
on the link partner).  The files are identical, and contain
information about both the DUT and the link partner systems.

Start with the following template file, and edit it as described
below.

~~~toml
## module is the name of the kernel module that must be loaded before testing
## commences. Translate - to _ if possible (this is required for the logic that
## detect when the wrong module is loaded to function correctly)
#module = "dwmac_tc956x"
module = "tc956x_pci"

## adapter is the name of the Ethernet adapter on the DUT to be tested.
#adapter = "enP1p5s0f1"
adapter = "eth0"

## ipaddr is the IP address for the link partner. It must be running an iperf3
## and sshd server.
ipaddr = "192.168.10.2"

## [REMOTE-ONLY] console is used by rb3gen2-remote to gain access to a serial
## port that connects to the console on the DUT.
console = "ssh -t 192.168.0.4 picocom -b 115200 /dev/serial/by-id/usb-Prolific_Technology_Inc._Prolific_PL2303GD_USB_Serial_COM_Port_DAAOb119D16-if00"

## [REMOTE-ONLY] partner_adapter is the name of the link partner's Ethernet
## adapter and is used by remote tests to alter the link partner's settings.
partner_adapter = "enp3s0"

## [REMOTE-ONLY, OPTIONAL] power_cycle is an optional command to assist with
## boot cycle testing. This command is run if a test times out, to recover
## access to the DUT.  If it is not set, boot cycle testing will still work,
## but will not be able to automatically recover and continue testing.
power_cycle = "iot-power-cycle rb3gen2"
~~~

As stated earlier, the configuration file is placed in a new `.dutlib`
directory.  For the DUT, it's in the root user's home directory; on
the link partner, it's in the remote user's home directory.

The template file should be edited to match your environment.  The
following settings must be defined for both local and remote mode testing:
- The `module` setting is name of the module loaded on the DUT and
should be fine as-is.
- The `adapter` setting is the name of the **DUT** Ethernet
adapter being tested and should also be fine as-is.
- The `ipaddr` setting is the IP address assigned to the **link
partner** Ethernet interface used for testing.

If remote testing is used (running `rb3gen2-remote` on the link
partner), additional settings must be provided.
- The `console` setting is a command executed on the link partner that
provides access to the DUT serial console port
- The `partner_adapter` setting is the name of the link partner
Ethernet interface used for testing.
- The `power_cycle` setting is an optional command run on the link partner
that power-cycles the DUT in the event of a timeout running a test.

### Setting up passwordless ssh access to the `test` account

In order to run certain tests, the root account on the DUT must be
configured to have passwordless access via ssh to the test account
on the link partner system.  To do this, we must set up a key that
is used in this process.  We will use the IP address assigned
to the link partner Ethernet port (i.e., `192.168.10.2`).

On the DUT, as the root user, run these commands.  The `ssh-keygen` command
will prompt for several things; pressing return at these prompts is
sufficient.

~~~sh
cd /root
ssh-keygen -t ed25519
# press return three times
~~~

This will create a new directory `~/.ssh` containing two files,
`id_ed25519` and `id_ed25519.pub`  The former should be kept private.

Next we use `ssh-copy-id` to record that we have sufficient privilege
to log in to the test user on the link partner system without providing
a password.  Running this command might ask "are you sure?" and will
prompt for a password.  The password requested is the password for the
`test` account on the link partner system.  Once this step completes
successfully, future `ssh` commands from root on the DUT to test on
the link partner will will not prompt.

~~~
ssh-copy-id test@192.168.10.2
~~~

## Running tests

To run the local test suite, run `rb3gen2-local` on the DUT.  This
currently takes about 8 minutes to complete.

To run the full test suite, run `rb3gen2-remote` on the link partner
system.  This takes about 17 minutes to complete.

Both commands provide brief help if run without arguments.  A set of
test suites to run are available.  Here are two example commands:

To run the full test suite, in remote mode:
~~~
# Run as the remote user on the link partner system
rb3gen2-remote all-tests
~~~

To run a simple "smoke test" in local mode:
~~~
# Run as the root user on the DUT
rb3gen2-local smoke-test
~~~

## Additional test information

Both tools provide comprehensive built-in help describing their command line
options. Be aware that both tools adopt a `git-like` mechanism of sub-commands
and argument position matters.

Global parameters should be to the left of the sub-command (if there is one):

~~~sh
rb3gen2-test --help
~~~

~~~sh
rb3gen2-test --verbose smoke-test
~~~

Sub-command parameters go on the right:

~~~sh
rb3gen2-test all-tests --help
~~~

## Common test plans

Note: To list other test plans use `--help`

### Smoke test

The smoke test is very fast to run and consists of a single ping, three iperf3
invocations and, if `CONFIG_STMMAC_SELFTESTS` is enabled, the stmmac self tests.

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
