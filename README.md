# agentx-ifstack

An AgentX (RFC 2741) subagent that serves IF-MIB `ifStackTable`
(`1.3.6.1.2.1.31.1.2`) for Linux hosts, so SNMP monitoring systems can discover interface relationships.

The subagent serves GET, GETNEXT, and GETBULK requests. One background netlink
monitor keeps the topology current independently of AgentX sessions. The
subagent reconnects and registers again when the master closes the session or
the socket fails.

## Install and run

Packages support amd64 Linux hosts. Both formats contain a stripped musl static
binary, so the executable does not require a particular glibc version.
Download the package for your distribution from the project's GitHub Releases.

On Debian 12 or 13:

```bash
sudo apt install ./agentx-ifstack_*_amd64.deb
```

On an RPM distribution with DNF:

```bash
sudo dnf install ./agentx-ifstack-*.x86_64.rpm
```

The packages install the service without enabling or starting it. Add this
line to `/etc/snmp/snmpd.conf`:

```text
master agentx
```

Edit `/etc/agentx-ifstack.toml` if needed, then start the services:

```bash
sudo systemctl restart snmpd.service
sudo systemctl enable --now agentx-ifstack.service
systemctl status agentx-ifstack.service
journalctl -u agentx-ifstack.service
agentx-ifstack --version
man agentx-ifstack
```

The service runs as root because `/var/agentx` is normally root-owned with mode
0700. It has no capabilities and writes no files. Its systemd sandbox permits
Unix and netlink sockets in the host network namespace.
The unit orders itself after `snmpd.service` without pulling that service in.
A missing master causes connection retries, not startup failure.

On Debian, `sudo apt remove agentx-ifstack` preserves the configuration;
`sudo apt purge agentx-ifstack` removes it. On RPM distributions,
`sudo dnf remove agentx-ifstack` removes the package. RPM saves a modified
configuration as `/etc/agentx-ifstack.toml.rpmsave` on removal.
Both package formats preserve local configuration edits during upgrades.
An upgrade restarts the service only if it is already running.

Before upgrading from a release that used `refresh`, replace that key in the
preserved configuration. The old key is intentionally invalid. For example,
replace `refresh = 5` with `reconcile = 3600`.

## Configuration

The default file is `/etc/agentx-ifstack.toml`. It accepts exactly four keys:

```toml
socket = "/var/agentx/master"
reconcile = 3600
priority = 127
log_level = "info"
```

- `socket` is a nonempty AgentX Unix socket path.
- `reconcile` is an integer background reconciliation interval in seconds, at
  least 1. Its default is 3600.
- `priority` is an integer from 1 to 255. Lower registration priorities win.
- `log_level` is `error`, `warn`, `info`, `debug`, or `trace`.

Omitted keys use built-in defaults. An absent default file is allowed.
`--config PATH` selects another file and requires that file to exist, even if
PATH names the default location. Invalid TOML, unknown keys, invalid values,
and unreadable files cause an error before any connection attempt.

File values override built-in defaults. `--socket PATH` overrides the file's
socket value. `RUST_LOG` overrides the log filter. Fatal startup errors go directly
to stderr even when `RUST_LOG=off`. Logs go to stderr without
timestamps; journald supplies timestamps for the service. A healthy idle daemon
is quiet at `info`. Restart the process after configuration changes.

```bash
sudo agentx-ifstack --config /etc/agentx-ifstack.toml --socket /run/agentx/master
agentx-ifstack --help
```

The process uses one topology-monitor thread and the AgentX supervisor thread.
The monitor subscribes to route-netlink link notifications before its first
complete inventory. AgentX reads use only the last published table and never
start topology acquisition.

Link events schedule complete inventories after 1, 2, 4, 8, 16, and at most
30 seconds during sustained change. A 60-second quiet period resets that
delay. Failed acquisition retries independently after 1, 2, 4, 8, 16, and at
most 30 seconds. Successful acquisition resets only the failure delay. The
periodic `reconcile` inventory is measured from the last successful inventory.

Before the first complete inventory, or after the monitor detects lost
notification continuity, reads return an AgentX processing error. An ordinary
inventory failure keeps the previous complete table available. Recovery from
loss requires a new subscription and a complete post-loss inventory.

AgentX reconnect delays start at one second and double to a maximum of 30
seconds. A session that lasts at least 30 seconds resets that delay.

## Build from source

Install Rust using the toolchain pinned in `rust-toolchain.toml`, then:

```bash
cargo build --release
sudo ./target/release/agentx-ifstack
```

For both packages, install `musl-tools`, `binutils`, and `gzip` on a Debian
build host, then:

```bash
rustup target add x86_64-unknown-linux-musl
cargo install --locked --version 3.8.0 cargo-deb
cargo install --locked --version 0.21.0 cargo-generate-rpm
sh packaging/build.sh
```

The build script writes `.deb` and `.rpm` files to `dist/`. It compresses the
handwritten man page and checks that the binary is static, stripped, and free
of build-directory paths. The Cargo metadata defines the installed assets.
The systemd unit installs under `/usr/lib/systemd/system`; `/lib/systemd/system`
resolves to the same location on the supported Debian releases.
The Debian maintainer contact is a placeholder until a public contact is set.

## Package validation

CI runs formatting, strict Clippy, tests, and a musl package build on branch pushes
and pull requests. It installs the packages in Debian 12, Debian 13, and Fedora
containers. Checks cover the executable, unit, man page, licenses, configuration
registration, local edits across reinstall, and removal. Debian also checks
configuration preservation on remove and deletion on purge. Tag releases require
the shared formatting, Clippy, test, and package workflows before publishing.
Only the release workflow handles tag pushes, so each tag builds packages once.

To run the container checks after building packages:

```bash
docker run --rm -v "$PWD:/work:ro" debian:12 sh -c 'sh /work/packaging/test-deb.sh /work/dist/*.deb'
docker run --rm -v "$PWD:/work:ro" debian:13 sh -c 'sh /work/packaging/test-deb.sh /work/dist/*.deb'
docker run --rm -v "$PWD:/work:ro" fedora:latest sh -c 'sh /work/packaging/test-rpm.sh /work/dist/*.rpm'
```

Lintian runs with errors and warnings treated as failures. The package documents
two exceptions: `initial-upload-closes-no-bugs`, because this upstream package
has no Debian intent-to-package bug, and `shared-library-lacks-prerequisites`,
because the musl static PIE requires no shared libraries. The Apache license
reference uses Debian's common-license path; both full licenses also ship.
RPM linting permits `statically-linked-binary` for the musl executable and
`no-signature` for unsigned GitHub Release artifacts. The spelling filter accepts
`subagent`, the term used by RFC 2741. The `no-buildhost-tag` filter permits
reproducible builds to omit the build hostname. The `no-changelogname-tag` filter
permits this upstream binary package to use Cargo metadata; its packaging
changelog ships as the Debian changelog. Other RPM findings remain
visible in CI. RPM contents and installation are checked by the Fedora job.
The container checks use offline systemd inspection and do not exercise a live
systemd service manager.

## Table behaviour

Register the region `1.3.6.1.2.1.31.1.2` at the configured priority (default 127) in the default
context. Serve `ifStackStatus` as INTEGER 1. TestSet returns
`notWritable(17)` at the first binding. CleanupSet requires no response.

Emit rows in **RFC index order**:

```
ifStackStatus.<higher_ifindex>.<lower_ifindex> = 1   # active
```

The higher sub-layer is the **first** index component. The lower sub-layer
is the second. [RFC 2863, ifStackTable DESCRIPTION](https://www.rfc-editor.org/rfc/rfc2863.html#section-6)
defines the rule:

> when the sub-layer with ifIndex value x runs over the sub-layer with
> ifIndex value y, then this table contains: ifStackStatus.x.y=active

A bond (9) runs over its member (3), so its row is `9.3`. A VLAN (10)
runs over that bond (9), so its row is `10.9`.

Emit only direct relationships. A VLAN on a bond adds a `(VLAN, bond)` row.
It does not add VLAN-to-member rows. Include zero-index rows for each
missing side of the emitted stack. A standalone interface has both `(0, index)`
and `(index, 0)`. Consumers wanting only real relationships filter those rows.
Counts of non-zero relationships exclude boundary rows. Tests assert the two sets separately.

GETNEXT honors inclusive starts and exclusive ends. A null end has no
upper bound. Exhausted ranges return `endOfMibView` with the requested OID.
GETBULK returns repeaters in iteration order and stops when all repeaters
are exhausted or the response reaches 4096 bindings. Input PDUs are limited
to 1 MiB of payload. Request OIDs are limited to 128 sub-identifiers after
prefix expansion, per [RFC 2741 section 5.1](https://www.rfc-editor.org/rfc/rfc2741.html#section-5.1).
Validate both ends of every SearchRange, every TestSet name, and every
OID-valued TestSet value before processing the request. Oversized OIDs
return `parseError`, and the session continues to serve requests.
Handshakes and partial reads have five-second socket timeouts. An idle
registered connection has no read timeout.

## Data source

The process reads typed `RTM_GETLINK` inventories and subscribes to the
route-netlink link multicast group. The link header supplies the interface
index. `IFLA_MASTER` supplies bond and bridge membership. `IFLA_LINK` supplies
the lower interface for VLAN, macvlan, ipvlan, and macvtap. VXLAN uses only
`IFLA_VXLAN_LINK` from its link-info data.

The monitor publishes only a completed, validated inventory. It rejects dump
interruption, malformed or partial messages, unsupported indices, missing
local endpoints, duplicate interfaces, and self-links. A missing or unresolved
VXLAN underlay leaves the VXLAN standalone. Veth peer links and unsupported
controller kinds are not stack relationships. A remote-namespace lower link
never becomes a local relationship.

## Tests

```bash
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

The normal `cargo test` command compiles but does not run the privileged
real-interface suite. On Linux, install `iproute2` and run that suite with:

```bash
CARGO="$(command -v cargo)"
sudo env HOME="$HOME" "$CARGO" test --locked --test real_namespace -- --ignored
```

Each test creates a network namespace, temporary configuration, AgentX Unix
socket, and subagent process. The tests discover the kernel-assigned interface
indices. They fail if `iproute2`, root access, or network namespace permission
is missing. Sandboxes that deny `unshare(CLONE_NEWNET)` cannot run this suite.
CI invokes it explicitly and treats missing prerequisites as a failure.

`python3 packaging/test_policy.py` checks the release gate, push triggers, and
service restart policy. It requires PyYAML 6.0.3, pre-commit 4.5.1, `gh`, `jq`, and Bash.
The shared checks workflow runs it.

`tests/real_namespace.rs` runs the actual binary against a UnixListener and
real isolated Linux interfaces. It covers both byte orders, reads, bulk walks,
write rejection, asynchronous topology updates, Close, reconnect after socket
loss, configuration, and the request OID limit. It asserts direct
relationships separately from zero-index boundary rows.

Pure tests run the real monitor against one controlled acquisition adapter.
They cover exact event and failure backoff, coalescing, reconciliation,
availability, continuity loss, stale completion, and read independence. The
production adapter tests use actual netlink packet types and raw receive
outcomes. They cover dump completion, interruption, malformed input, overrun,
receive failure, termination, and resubscription.

## Sources

The table decisions are grounded in these, not in convention:

- [RFC 2863](https://www.rfc-editor.org/rfc/rfc2863.html), sections 3.1.1 and 6, for the
  ifStackTable model, the "runs over" index rule, and the zero-index boundary rows.
- [RFC 2741](https://www.rfc-editor.org/rfc/rfc2741.html), sections 5.1, 5.4 and 7.2.2
  through 7.2.4, for PDU handling, the 128 sub-identifier OID limit, and GETNEXT
  semantics.
- The `agentx` 0.1.1 source, for the encoder and decoder behaviour this depends on.
- The rust-netlink `netlink-sys`, `netlink-packet-core`, and
  `netlink-packet-route` sources, for route socket and message decoding.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

## Prior art

- No existing subagent serves `ifStackTable`. The closest is net-snmp's own
  `local/snmp-bridge-mib`: Perl, AgentX, reads `/sys/class/net`, but BRIDGE-MIB
  only.
- The Rust `agentx` crate implements RFC 2741 PDUs and encodings but has **no
  connection or session handling**. `src/session.rs` provides that layer.
- `drbd-reactor` is a complete subagent built on that crate and is the reference
  implementation to crib the session loop from.
- Also worth reading: `snmp_rust_agent`, `sunt`.

## Deployment constraints

- AgentX supports multiple simultaneous subagents. Source-verified in net-snmp's
  `agent/mibgroup/agentx/master.c` (`master_sessions` linked list, per-session
  `sessid`). Overlapping regions resolve by registration priority (default 127,
  lower wins); an exact duplicate region at equal priority is rejected with
  `duplicateRegistration`. net-snmp does not own `1.3.6.1.2.1.31.1.2`, so there
  is no conflict with the running master or with lldpd.
- `/var/agentx` is `drwx------ root root` and the socket is `srwxr-xr-x root
  root`. lldpd connects because its monitor process runs as root. An
  unprivileged subagent cannot traverse into `/var/agentx`, so either run as
  root or set `agentXPerms` in `snmpd.conf`.
- The master must reconnect cleanly. snmpd is restarted by configuration
  management, so the session loop needs to survive `AgentX master disconnected
  us` and re-register without supervision.

## Verifying

With `master agentx` in `snmpd.conf` and the service running, walk the table
locally:

```bash
snmpwalk -v2c -c public localhost 1.3.6.1.2.1.31.1.2
```

Rows appear as `IF-MIB::ifStackStatus.<higher>.<lower> = INTEGER: active(1)`.
Compare them against the host's own topology:

```bash
ip -details -json link show
```

A bond or bridge yields one row per member, with the master's ifIndex first. A
VLAN yields one row with the VLAN's ifIndex first and its base interface second.
An interface has a zero higher sub-layer row only if no interface runs over it.
It has a zero lower sub-layer row only if it runs over no other interface.
