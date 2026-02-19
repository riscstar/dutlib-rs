# RB3gen2 semi-automatic tests

## Quickstart

The DUT is partnered with another machine that can be used to test against.
It is assumed the DUT and it's partner are connected with a point-to-point
link.

Partner setup should include:

1. Configuring NetworkManager such that the IPv4 Configuration is "Shared"
   (meaning the partner will share it's connection with the DUT and provide
   it an address via DHCP if asked). The IPv4 address should be
   192.168.10.2/24 (and check "Never us this network for default route").
2. Install `iperf3` and run the server by default.
3. Create a user called `test` and ensure the DUT can SSH to test without
   needing a password.
