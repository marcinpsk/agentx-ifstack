# agentx-ifstack

An AgentX (RFC 2741) subagent that serves IF-MIB `ifStackTable`
(`1.3.6.1.2.1.31.1.2`) for Linux hosts, so SNMP monitoring systems can discover interface relationships.

The subagent serves GET, GETNEXT, and GETBULK requests. It refreshes the
topology on demand after a five-second cache window. It reconnects and
registers again when the master closes the session or the socket fails.

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

The packages depend on `iproute2` on Debian and `iproute` on RPM distributions.
They install the service without enabling or starting it. Add this line to
`/etc/snmp/snmpd.conf`:

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
Unix and netlink sockets and execution of `ip` in the host network namespace.
The unit orders itself after `snmpd.service` without pulling that service in.
A missing master causes connection retries, not startup failure.

On Debian, `sudo apt remove agentx-ifstack` preserves the configuration;
`sudo apt purge agentx-ifstack` removes it. On RPM distributions,
`sudo dnf remove agentx-ifstack` removes the package. RPM saves a modified
configuration as `/etc/agentx-ifstack.toml.rpmsave` on removal.
Both package formats preserve local configuration edits during upgrades.
An upgrade restarts the service only if it is already running.

## Configuration

The default file is `/etc/agentx-ifstack.toml`. It accepts exactly four keys:

```toml
socket = "/var/agentx/master"
refresh = 5
priority = 127
log_level = "info"
```

- `socket` is a nonempty AgentX Unix socket path.
- `refresh` is an integer cache interval in seconds, at least 1.
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

The process uses one thread. It runs `ip -details -json link show` in the
current network namespace. Refresh is demand driven: the first read after the
cache expires loads topology again. Failed commands and invalid topology data
return an AgentX processing error. The next read retries the refresh.
Reconnect delays start at one second and double to a maximum of 30 seconds.
A session that lasts at least 30 seconds resets the delay.

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
missing side of the emitted stack. A plain interface has both `(0, index)`
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

`ip -details -json link show` supplies everything needed:

| field | meaning |
|---|---|
| `ifindex` | the interface's own ifIndex |
| `master` | bond or bridge membership, as an interface **name** |
| `linkinfo.info_kind` | interface kind, used to distinguish stack layers from peers |
| `link` | lower interface **name** for `vlan`, `macvlan`, `ipvlan`, and `macvtap` |
| `link_index` | lower interface index when emitted in numeric form |
| `linkinfo.info_data.link` | lower interface **name** for `vxlan` |

Verified against real Proxmox hosts: `master` and `link` are
**names, not indices**, and `link_index` is absent entirely. Build an
ifname -> ifindex map from the same output and resolve through it. Do not
depend on `link_index` being present.

The parser resolves lower interfaces for `vlan`, `macvlan`, `ipvlan`, and
`macvtap` through either `link` or `link_index`. A vxlan carries its underlay in
`linkinfo.info_data` instead, always as a name. `ip` writes the literal `if<index>`
there when no interface resolves, so an underlay that matches no interface in the same
output leaves the vxlan standalone, as does a vxlan with no underlay. It rejects
missing lower interfaces, conflicting references, duplicate names or indices,
and self-links. It does not treat veth peer links or VRF membership as stack
relationships. A lower interface in another namespace is an error because
its ifIndex cannot identify a local interface.

Real topologies to handle, present in the collected captures:

- a bond with physical members (`bond0` <- `nic2`, `nic3`)
- VLANs on a bond (`bond0.110`, `bond0.111`, each `link: bond0`)
- a bond enslaved to a bridge (`bond1` has `master: vmbr1`), so an interface is
  a higher sub-layer for its members and a lower sub-layer for the bridge
- a VLAN on a bridge (`vmbr1.120`, `link: vmbr1`)
- Proxmox firewall bridges (`fwbr<vmid>i0`) joined to `vmbr0` by a veth pair
  (`fwpr<vmid>p0` / `fwln<vmid>i0`), plus `tap<vmid>i<n>` guest interfaces

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
sudo --preserve-env=PATH env HOME="$HOME" cargo test --locked --test real_namespace -- --ignored
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
write rejection, refresh, Close, reconnect after socket loss, configuration,
and the request OID limit. It asserts direct relationships separately from
zero-index boundary rows. `tests/session.rs` keeps fixture executables only for
the current `ip` subprocess lifecycle, timeout, cleanup, and output-limit
behavior. Pure tests cover parser edge cases and MIB boundaries.

`tests/fixtures/proxmox.json` preserves the relationship fields from the
collected Proxmox capture. Interface names are replaced with `port<index>`.
Addresses and unrelated configuration fields are removed. Add sanitized
captures as JSON fixtures and assert their expected direct rows in `link.rs`.

## Sources

The table decisions are grounded in these, not in convention:

- [RFC 2863](https://www.rfc-editor.org/rfc/rfc2863.html), sections 3.1.1 and 6, for the
  ifStackTable model, the "runs over" index rule, and the zero-index boundary rows.
- [RFC 2741](https://www.rfc-editor.org/rfc/rfc2741.html), sections 5.1, 5.4 and 7.2.2
  through 7.2.4, for PDU handling, the 128 sub-identifier OID limit, and GETNEXT
  semantics.
- The `agentx` 0.1.1 source, for the encoder and decoder behaviour this depends on.

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
Every interface also yields the two zero-index boundary rows RFC 2863 requires.
