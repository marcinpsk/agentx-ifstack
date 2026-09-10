# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

An AgentX (RFC 2741) subagent that serves IF-MIB `ifStackTable` (`1.3.6.1.2.1.31.1.2`)
on Linux hosts. net-snmp implements neither `ifStackTable` nor `IEEE8023-LAG-MIB`, so a
Linux host exposes no stack rows for its bonds, bridges or VLANs. This subagent is the
only missing hop between the kernel's view of those relationships and an SNMP poller.

For the LAG half, the plugin identifies the aggregate by ifType `ieee8023adLag` or a
configured name pattern. Linux bonds report `ethernetCsmacd`, so a `PortStackLagPattern`
regex like `^bond\d+$` must be configured for `linux` and `proxmox`. Sub-interfaces need
no configuration; they are paired by the `.N` name suffix.

Read `README.md` first for runtime behavior, test commands, and the source-verified
research. `link.rs` parses topology, `mib.rs` serves ordered rows, `session.rs` handles
AgentX and refresh I/O, and `main.rs` supervises reconnection.

## Build and test

```bash
cargo build
cargo test
cargo test <test_name>          # single test
cargo test -- --nocapture       # keep test stdout
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

`rust-toolchain.toml` pins the toolchain, but a `RUSTUP_TOOLCHAIN` environment variable
overrides it. Check that variable before blaming a build failure on the code.

## Wire format: higher sub-layer ifIndex first

```
ifStackStatus.<higher_ifindex>.<lower_ifindex> = 1   # active(1)
```

The higher sub-layer is the **first** index component. The lower sub-layer is the second.
[RFC 2863, ifStackTable DESCRIPTION](https://www.rfc-editor.org/rfc/rfc2863.html#section-6)
defines the rule:

> when the sub-layer with ifIndex value x runs over the sub-layer with
> ifIndex value y, then this table contains: ifStackStatus.x.y=active

A bond (9) runs over its member (3), so its row is `9.3`. A VLAN (10)
runs over that bond (9), so its row is `10.9`. Use `higher` and `lower`
for row components in code and tests.

Some consumers label these columns the other way round. RFC 2863 is the authority,
not a consumer's column names: higher sub-layer first.

Rows must be emitted in RFC index order, because GETNEXT walks depend on it.
Keep zero-index boundary rows required by RFC 2863. Count and assert non-zero
relationships separately from boundary rows.

## Data source

`ip -details -json link show` supplies every relationship needed. Prefer parsing that
JSON over `/sys/class/net` traversal.

| field | meaning |
|---|---|
| `ifindex` | the interface's own ifIndex |
| `master` | bond or bridge membership, names the higher sub-layer |
| `linkinfo.info_kind` | interface kind, distinguishes stack layers from peers |
| `link` | lower interface name for `vlan`, `macvlan`, `ipvlan`, `macvtap`, and `vxlan` |
| `link_index` | lower interface index when emitted in numeric form |

Reject lower-interface references with `link_netnsid`: remote ifIndexes are
not local interface identifiers. Exclude veth peer links from stack rows.

## AgentX constraints that shape the design

- Validate request OIDs at the session boundary before processing them. Limit
  normalized OIDs to 128 sub-identifiers. Check both SearchRange ends, TestSet
  names, and OID-valued TestSet values. Return `parseError` on violations.
  The crate decoder can construct oversized OIDs that panic on serialization.
- The `agentx` crate implements RFC 2741 PDUs and encodings but has **no connection or
  session handling**. `session.rs` provides that layer. `drbd-reactor` is the reference
  implementation for the session loop.
- The master must reconnect cleanly and re-register without supervision. snmpd is
  restarted by configuration management, so `AgentX master disconnected us` is a normal
  event, not a fatal one.
- Multiple subagents can register at once. net-snmp resolves overlapping regions by
  registration priority (default 127, lower wins) and rejects an exact duplicate region
  at equal priority with `duplicateRegistration`. net-snmp does not own
  `1.3.6.1.2.1.31.1.2`, so there is no conflict with the running master or with lldpd.
- `/var/agentx` is `drwx------ root root`. An unprivileged subagent cannot traverse into
  it, so either run as root or set `agentXPerms` in `snmpd.conf`.

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

## Prior art worth reading before writing the session loop

- `drbd-reactor`, complete subagent on the `agentx` crate, the session-loop reference.
- net-snmp `local/snmp-bridge-mib`, Perl AgentX subagent reading `/sys/class/net`,
  BRIDGE-MIB only.
- `snmp_rust_agent`, `sunt`.
