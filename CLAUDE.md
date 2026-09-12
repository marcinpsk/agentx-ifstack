# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

An AgentX (RFC 2741) subagent that serves IF-MIB `ifStackTable`
(`1.3.6.1.2.1.31.1.2`) on Linux hosts. Read `README.md` first for runtime
behavior, package commands, and the table model.

`config.rs` validates CLI and file settings before supervision starts.
`link.rs` parses topology, `mib.rs` serves ordered rows, `session.rs` handles
AgentX and refresh I/O, and `main.rs` supervises reconnection.

## Configuration and packages

Keep the configuration limited to socket, refresh, priority, and log_level.
An absent default file is allowed. An explicit missing file or invalid file
must fail before the supervise loop. Tests use real temporary files and the
actual binary with an AgentX UnixListener.

When changing packaging, run `sh packaging/build.sh` and the container commands
in README.md. The reusable packages workflow gates releases on installation,
reinstall, and removal checks in Debian 12, Debian 13, and Fedora.
The build script remaps compiler source paths to keep build-user information
out of release binaries. Keep the artifact checks in that script.

Use cargo-deb's systemd integration for Debian maintainer scripts. RPM scriptlets
must also work without a running systemd manager. Initial installation leaves
the service disabled and stopped. Upgrades restart only an active service.
Preserve local configuration edits in both formats.

The unit runs as root to traverse /var/agentx, with an empty capability set.
Keep the host network namespace and allow AF_UNIX, AF_NETLINK, and execution
of ip. Offline unit analysis does not prove live sandbox compatibility.
See README.md for the documented lint exceptions. Keep package contents and
examples free of private project references and build-machine identifiers.

## Build and test

```bash
cargo build
cargo test
cargo test <test_name>          # single test
cargo test -- --nocapture       # keep test stdout
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo deny check                # advisories, licences, banned and duplicate crates
```

Lint levels live in `Cargo.toml` under `[lints]`, so a plain `cargo clippy` fails on the
same code CI rejects. `cargo deny` needs `cargo install --locked cargo-deny`; CI runs it
as a separate job in `checks.yml`.

Project invariants that clippy cannot express live in `.opengrep/agentx-ifstack-rules.yaml`.
Run `scripts/opengrep-scan.sh` to check the source and `scripts/opengrep-test.sh` to check
the rules themselves. Every rule needs a fixture in `.opengrep/tests/`, which the packaging
policy tests enforce.

These run from a local pre-commit hook, never in CI: CodeRabbit skips its own opengrep pass
when it sees opengrep in the workflows. Install with `pre-commit install --install-hooks`.
The hooks are filtered so a commit touching neither `src/` nor the rules costs nothing. See
`.opengrep/README.md` for that and for why the filename matters.

`zizmor` audits the workflows themselves (permissions, injection, unpinned actions) in the
`Workflow audit` CI job. Run it locally with `uvx --native-tls zizmor .github/workflows/`.

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
| `link` | lower interface name for `vlan`, `macvlan`, `ipvlan`, and `macvtap` |
| `link_index` | lower interface index when emitted in numeric form |
| `linkinfo.info_data.link` | lower interface name for `vxlan` |

A vxlan does not use `link` or `link_index`. Its underlay is `IFLA_VXLAN_LINK`,
which `ip` prints inside `linkinfo.info_data`. It always prints a name, through
`ll_index_to_name`, which falls back to the literal `if<index>` when no interface
resolves. An underlay that names no interface in the same output is therefore not a
local relationship, so the vxlan is standalone. A vxlan with no underlay at all is
standalone too. Neither case is an error: one odd interface must not fail the table.

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

## Agent skills

### Issue tracker

Issues live as GitHub issues in this repo, driven through the `gh` CLI. External pull
requests are treated as a request surface and triaged alongside issues. See
`docs/agents/issue-tracker.md`.

### Triage labels

The five canonical roles, each label string equal to its name. See
`docs/agents/triage-labels.md`.

### Domain docs

Single-context: `CONTEXT.md` and `docs/adr/` at the repo root. Neither exists yet, and the
skills proceed silently rather than scaffolding them. See `docs/agents/domain.md`.
