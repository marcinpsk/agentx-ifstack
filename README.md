# agentx-ifstack

An AgentX (RFC 2741) subagent that serves IF-MIB `ifStackTable`
(`1.3.6.1.2.1.31.1.2`) for Linux hosts, so SNMP monitoring systems can discover
interface relationships.

The subagent serves GET, GETNEXT, and GETBULK requests. It refreshes the
topology on demand after a five-second cache window. It reconnects and
registers again when the master closes the session or the socket fails.

## Run

Install `iproute2` and enable `master agentx` in `snmpd.conf`. Build and run:

```bash
cargo build --release
sudo ./target/release/agentx-ifstack
```

The default socket is `/var/agentx/master`. Use `--socket PATH` to select
another Unix socket. Logs go to stderr. Reconnect delays start at one second
and double to a maximum of 30 seconds. A session that lasts at least 30
seconds resets the delay.

The process uses one thread. It runs `ip -details -json link show` in the
current network namespace. Failed commands and invalid topology data return
an AgentX processing error. The next request retries the refresh.

## Why this exists

net-snmp implements neither `ifStackTable` nor `IEEE8023-LAG-MIB`. This is
source-checked, not assumed: `agent/mibgroup/if-mib/` holds only `data_access`,
`ifTable` and `ifXTable`, and nothing under `agent/mibgroup` matches
ifStack/lag/dot3ad. Live walks agree, both return `No Such Object`. There is no
option to enable; the code does not exist.

A Linux host therefore exposes no stack rows for its bonds, bridges or VLANs.
Only this first hop is missing.

## Table behaviour

Register the region `1.3.6.1.2.1.31.1.2` at priority 127 in the default
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
| `link` | lower interface **name** for `vlan`, `macvlan`, `ipvlan`, `macvtap`, and `vxlan` |
| `link_index` | lower interface index when emitted in numeric form |

Verified against real Proxmox hosts: `master` and `link` are
**names, not indices**, and `link_index` is absent entirely. Build an
ifname -> ifindex map from the same output and resolve through it. Do not
depend on `link_index` being present.

The parser resolves lower interfaces for `vlan`, `macvlan`, `ipvlan`,
`macvtap`, and `vxlan` through either `link` or `link_index`. It rejects
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

`tests/session.rs` runs the actual binary against a UnixListener. The master
uses real AgentX PDUs. A fixture executable supplies `ip` output without
changing host interfaces. Tests cover both byte orders, reads, bulk walks,
write rejection, cache refresh, errors, Close, and reconnect after socket
loss. Raw wire tests cover oversized OIDs at both SearchRange ends and in
TestSet names and OID values. They check `parseError`, process survival, and
a normal GET on the same connection. Pure tests cover topology fixtures and
OID boundaries.

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
