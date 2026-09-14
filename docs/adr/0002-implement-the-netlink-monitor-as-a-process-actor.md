# Implement the netlink monitor as a process actor

Status: accepted

## Context

Issue #8 implements the behavioral contract in ADR 0001. The current daemon
acquires topology from an `ip` subprocess inside each AgentX session. The new
implementation must subscribe before its first inventory, preserve netlink
dump completion and loss metadata, schedule all acquisition outside AgentX
requests, and publish only complete table versions.

The replacement also needs one controlled acquisition interface for exact
timing and failure tests. Production code must place strict time and size
bounds around each inventory. It must distinguish an ordinary dump failure
from loss of the notification stream.

The selected modules use the domain language from `CONTEXT.md`:

- A topology owns the interface set and direct stack relationships.
- A relationship has named `higher` and `lower` indices.
- The MIB owns boundary-row derivation.

## Decision

Run one synchronous monitor actor on a dedicated process-lifetime thread.
Keep the existing blocking AgentX supervisor on the main thread. The monitor
continues while the AgentX master is absent and across every AgentX session.

Use these module interfaces:

- `link` converts a complete inventory of observed links into a validated
  `Topology`. `Topology` contains a set of interface indices and a set of
  direct `StackRelationship { higher, lower }` values. It contains no netlink
  types, relationship kinds, boundary rows, or acquisition state.
- `mib` builds one immutable `Mib` from a `Topology`. It derives the RFC 2863
  boundary rows and keeps the existing ordered AgentX lookup behavior.
- `netlink` implements the production acquisition adapter with
  `netlink-sys`, `netlink-packet-core`, and `netlink-packet-route`. These are
  maintained rust-netlink libraries. No netlink type crosses this module.
- `monitor` owns subscription, acquisition scheduling, continuity state,
  retry state, and publication. Its only production input is the acquisition
  interface. A controlled adapter supplies the same interface to tests.
- `session` owns only AgentX connection, registration, validation, request
  parsing, and response serialization.

The publication interface has separate read and write capabilities. The
monitor owns the publisher. AgentX sessions receive only a clonable reader.
The reader can obtain `Option<Arc<Mib>>`, and it has no acquisition method.
Each request takes one snapshot before it evaluates any search range. `None`
maps to AgentX `processingError`.

### Production acquisition adapter

The adapter owns two route-netlink sockets:

1. One process-lifetime link-notification socket. It subscribes to the link
   multicast group before the first inventory.
2. One bounded dump socket per complete inventory. A new socket prevents a
   timed-out request from contaminating a later inventory with stale sequence
   data.

The adapter sends `RTM_GETLINK` with `NLM_F_REQUEST | NLM_F_DUMP`. During the
dump, it polls both sockets. It consumes and records notifications without
letting them replace the complete inventory. It uses a fixed datagram buffer,
rejects truncation, caps total dump bytes, and requires completion before a
fixed three-second deadline. This deadline is deliberately shorter than the
60-second event-backoff reset interval. The first and last timestamps therefore
preserve every reset-relevant gap in a notification batch received during one
inventory.

The adapter accepts only matching `RTM_NEWLINK` messages followed by an
explicit `NLMSG_DONE`. It inspects `NLM_F_DUMP_INTR` on every matching message,
including the final `NLMSG_DONE`. It rejects malformed messages, unexpected
responses, errors, overrun, a nonzero completion code, missing completion,
and partial inventories.

On the notification socket, `RTM_NEWLINK` and `RTM_DELLINK` record a link
change. `NLMSG_OVERRUN`, `ENOBUFS`, malformed input, receive failure, and
termination report lost continuity. Loss discards the notification socket.
Recovery creates and subscribes a new socket before it requests a new
inventory. The adapter never enables `NETLINK_NO_ENOBUFS`.

An internal datagram-I/O seam sits below the production adapter's receive and
classification logic. It exposes the real syscall result, sender, truncation,
and bytes. Adapter tests script this seam to exercise `ENOBUFS`, zero-length
termination, truncation, malformed data, and resubscription failure through the
same code that owns production classification. It is not a second acquisition
interface and it does not expose a way to inject already-classified loss.

The packet decoder maps the actual route message types into an internal
observed-link value:

- the link header supplies the interface index;
- `IfName` supplies the required nonempty interface identity;
- `Controller` supplies bond and bridge membership;
- `Link` supplies the lower index for VLAN, macvlan, ipvlan, and macvtap;
- `LinkNetNsId` marks a non-local lower reference;
- `LinkInfo::Kind` classifies relationships during construction only; and
- `InfoData::Vxlan` with `InfoVxlan::Link` supplies the VXLAN underlay.

The decoder ignores the generic `Link` attribute for VXLAN. A missing or
non-member VXLAN underlay leaves that interface standalone. Veth lower links
and controllers whose target is not a bond or bridge create no relationship.
Malformed supported link references fail the complete inventory.

### Monitor scheduling

The acquisition interface exposes four operations: monotonic time,
subscription, one complete inventory, and waiting for one notification or a
deadline. Inventory results include the first and last notification times
observed while the dump was active. Errors explicitly distinguish ordinary
inventory failure from lost notification continuity.

The actor keeps one in-flight slot and these independent values:

- a pending event deadline;
- event delay and last-event time;
- a failure not-before deadline and failure delay;
- last successful inventory time;
- subscription and recovery state; and
- publication availability.

Startup requests an immediate inventory after subscription. The first link
event schedules an inventory one second later. Later events do not postpone
that deadline. An event-triggered attempt doubles the next event delay through
1, 2, 4, 8, 16, and 30 seconds. A gap of at least 60 seconds between events
resets it to one second. Successful acquisition alone does not reset it.
Events received during an attempt remain pending, but a valid result still
publishes.

An acquisition or subscription failure schedules a retry through 1, 2, 4, 8,
16, and 30 seconds. This deadline is a hard floor for event, reconciliation,
startup, and recovery work. A successful complete inventory resets the
failure delay. Reconciliation is due from the last successful inventory, so a
failure does not move its deadline. Pending triggers coalesce into one attempt.

An ordinary failure preserves an existing publication. Lost continuity makes
publication unavailable immediately, discards the subscription, and requires
a new subscription plus a complete post-loss inventory. Because one actor
serializes notification handling and inventory completion, an attempt that
observes loss cannot publish its result. The controlled adapter also tags each
attempt by continuity generation to prove that a stale pre-loss completion
cannot restore availability.

The actor logs failures and continues. Reading a publication never starts,
reschedules, or accelerates acquisition.

## Blind design comparison

Two designs were completed independently before comparison.

| Decision | Local candidate | Blind candidate | Merged disposition |
| --- | --- | --- | --- |
| Domain and MIB boundary | `Topology` owns interfaces and direct relationships. `Mib` derives boundary rows. | The same boundary, types, and ownership. | Keep. It gives topology one domain meaning and keeps table-only rows local to the MIB. |
| Publication | Split reader and publisher around immutable `Arc<Mib>` snapshots. | The same split and snapshot contract. | Keep. AgentX cannot acquire or publish. |
| Process shape | Dedicated synchronous monitor thread beside the blocking AgentX loop. | Tokio current-thread monitor plus a dedicated blocking AgentX thread. | Use the synchronous monitor thread. It preserves the existing AgentX supervisor on the main thread and needs no async runtime for two pollable file descriptors and one timer. |
| Netlink library level | Use `netlink-sys` and typed packet crates directly. | Use `rtnetlink` and `netlink-proto`, then bypass the normal link-get stream to recover raw completion messages. | Use the direct packet and socket crates. They are maintained by the same rust-netlink project and expose dump flags and overrun without relying on non-default forwarding behavior. |
| Dump and notification socket | Separate process-lifetime notification socket and per-attempt dump socket. | One multicast connection multiplexes replies and notifications; a timeout restarts it and becomes continuity loss. | Use separate sockets. A dump timeout remains an ordinary inventory failure because the notification socket can continue to prove continuity. |
| Test clock | The controlled acquisition interface supplies monotonic time and advances it while waiting. | Tokio paused time. | Use the controlled interface. It keeps production and deterministic tests on the same actor without adding a second runtime abstraction. |
| Stale completion | The synchronous adapter returns loss instead of completion when it observes notification loss. Tests also track a continuity generation. | Ordered async events carry attempt and continuity identifiers. | Keep both protections at the actor boundary. They make the stale-result invariant explicit even though production serialization already enforces it. |

The alternative fully async daemon was rejected because it would rewrite the
AgentX transport without improving the required seams. A high-level rtnetlink
adapter was rejected because the normal link-get path does not expose the
exact final completion metadata. The selected direct adapter does not
reimplement route attribute decoding. The packet crate remains the single
decoder.

The module deletion test supports these boundaries. Deleting `monitor` would
spread scheduling, retry, continuity, and availability across `main`,
`session`, and `netlink`. Deleting `netlink` would leak kernel wire types and
socket behavior into the domain model. Deleting `link` would mix transient
kernel classification with persistent MIB rows. Each module therefore earns
its interface.

## Verification seams

- Domain tests construct observed links and cover every supported relation,
  endpoint validation, duplicate and self-link rejection, remote namespaces,
  unresolved VXLAN underlays, veth exclusion, and direct-only output.
- MIB tests prove boundary derivation, higher-first order, GETNEXT order, and
  bounded GETBULK.
- Production adapter tests serialize actual netlink packet types through the
  production decoder. They cover final-DONE interruption, malformed input,
  overrun and receive-loss classification, notification termination,
  resubscription failure, and post-loss recovery. The internal datagram-I/O
  seam supplies raw syscall outcomes, not preclassified adapter events.
- Controlled-adapter tests run the real monitor, topology builder, MIB, and
  publication with deterministic time. They prove the exact event and failure
  delay sequences, reset conditions, non-postponement, one slot, coalescing,
  overlap behavior, availability rules, stale-result rejection, and the fact
  that reads cannot request work.
- Actual-binary tests use real isolated interfaces. They prove initial
  inventory, asynchronous change without a triggering read, current topology
  after AgentX reconnect, and availability while an update is pending. They
  retain the protocol and topology coverage established by issue #7.
- Repeated controlled failures, loss, bursts, and reconnects check bounded
  thread and descriptor growth.

## Consequences

The daemon gains one long-lived thread and one short publication lock. The
session hot path clones one `Arc`. It performs no topology I/O.

The replacement deletes the `ip` command path, session cache, JSON topology
parser, process supervision, obsolete subprocess tests, and related lint
fixtures. Runtime packages no longer depend on iproute2. The systemd service
no longer needs a PATH solely for `ip`. Test setup can retain iproute2 to build
real interfaces.

Configuration accepts `reconcile` with a default of 3600 seconds and rejects
the removed `refresh` key. Package documentation tells operators to replace
`refresh` explicitly before upgrade. No compatibility alias or migration is
provided.

Release validation must confirm the selected crate versions on the pinned
Rust toolchain and supported static targets. Live namespace tests remain the
authority for sandbox compatibility.

## Design review

Revision: r1. Scope: issue #8 runtime interfaces, netlink adapter, monitor
scheduling, publication, and verification seams.

The merged design above is ready for independent adversarial ratification.

Round 1: RATIFY r1. The reviewer checked the merged design against issue #8,
ADR 0001, the current code, and the selected rust-netlink sources. It first
identified two gaps: the event-batch timestamps needed a bound below the quiet
reset interval, and packet serialization alone could not prove production
receive-error classification. Revision r1 now fixes the inventory deadline at
three seconds and adds the raw datagram-I/O seam described above. The reviewer
then ratified the revised design with no remaining blocker.
