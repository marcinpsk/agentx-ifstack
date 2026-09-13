# Monitor topology independently of AgentX sessions

Replace the ip subprocess with background, event-driven netlink acquisition.
The topology monitor runs for the process lifetime and continues across AgentX
disconnects, so master restarts do not interrupt topology monitoring.
SNMP requests read the published table and never trigger acquisition.
Link events drive normal updates. Periodic background reconciliation covers
silently missed events, including when no later notification arrives.
The reconciliation interval defaults to one hour, measured from the last
successful complete inventory. The configurable `reconcile` key specifies
this interval in seconds and replaces `refresh`. Existing `refresh` entries
fail configuration validation and require an explicit update.

Link-change notifications trigger a complete inventory. Coalesce notification
bursts into a pending acquisition. Startup, periodic reconciliation, and
recovery after detected event loss use the same complete-inventory acquisition
path.
Failed acquisitions retry automatically after one second, doubling the delay
to a 30-second cap, even when no further notifications arrive. New events
cannot bypass this failure delay. A successful complete inventory ends the
retry cycle and resets the failure backoff. Failure backoff is separate from
event backoff.
Publish a valid completed inventory even when notifications arrived during
acquisition, and retain the pending acquisition for those notifications.
Repeated event-triggered acquisitions use capped exponential backoff to bound
the acquisition rate during link flapping. The delay starts at one second,
doubles to a 30-second cap, and resets after 60 seconds without link events.
Coalesce events into one pending acquisition and allow only one acquisition
at a time. Later events do not postpone an already scheduled acquisition.
Successful acquisition alone does not reset the event backoff while events
continue.
Reject interrupted or invalid inventories. Each SNMP response uses one
published table version.

Before the first successful inventory, or after a detected loss of
synchronization, reads return an AgentX processing error until acquisition
succeeds. During an ordinary update, reads use the previous complete table
until its replacement is ready. An ordinary acquisition failure preserves
that availability while retries continue. A reported synchronization loss
keeps the table unavailable until a complete inventory succeeds.

## Topology and table ownership

Topology contains real interfaces and direct relationships with named higher
and lower indices. It does not retain relationship kinds. The MIB derives
zero-index boundary rows for each missing side of the emitted stack.

## Design review

Revision: r1. Scope: the selected behavioral contract, before implementation
design. The operator selected event-triggered full inventories with periodic
reconciliation. Incremental event updates and demand-driven acquisition were
not selected, so this review uses the selected-plan route.

The monitor owns acquisition scheduling and availability. Its publication
interface separates acquisition from SNMP handling. Validation must prevent
partial inventories from becoming published tables and must demonstrate that
SNMP requests cannot trigger acquisition.

Acceptance conditions:

- Topology monitoring continues across AgentX disconnects.
- Each response uses one complete published table.
- Events arriving during acquisition remain pending without starving
  publication or creating parallel acquisitions.
- Sustained events respect event backoff. Failures respect failure backoff.
- Initial unavailability and detected loss return errors until recovery.
- Ordinary acquisition failures preserve an available table while retrying.
- Reconciliation repairs silent loss without demand-driven acquisition.
- Configuration uses `reconcile` with a 3600-second default.
- The MIB derives boundary rows from interfaces and direct relationships.

Evidence: `src/session.rs` currently owns a session-local, demand-driven
cache; `src/link.rs` currently mixes relationships with boundary rows;
`tests/session.rs` currently injects topology through an ip executable.
These paths establish the behavior and test infrastructure being replaced.
Runtime choice, exact module interfaces, and execution of the replacement
remain implementation-design work and are outside this review's scope.

### Section 0: refuted findings

None.

### Review rounds

Round 1: RATIFY r1, behavioral-contract scope. The independent reviewer found
the acceptance conditions coherent, with no unresolved operator-policy
blocker. It checked this record against `src/main.rs`, `src/session.rs`,
`src/link.rs`, `src/config.rs`, README, and CONTEXT. No replacement tests ran.

The review confirmed two obligations derived from the recovery and scheduling
requirements:

- Work already in progress when loss is detected cannot restore availability
  merely by completing. Recovery must establish a valid post-loss inventory.
- All acquisition triggers share one in-flight slot and respect the failure
  backoff deadline, including periodic reconciliation.

The verdict covers contract coherence. It does not verify replacement code,
library behavior, runtime interfaces, or live sandbox compatibility.

### Specification review

The operator confirmed the testing interfaces: actual-binary AgentX tests
with real isolated interfaces, plus one injectable acquisition interface for
deterministic fault and timing tests.

Specification review found that tests injecting loss above the production
adapter would not prove that its subscription handler reports receive errors
or termination. The corrected testing plan explicitly covers production loss
reporting, failed resubscription, autonomous retry, and restoration only after
monitoring and a valid post-loss inventory are established.

Round 2: RATIFY specification r2, synthesis and test-strategy scope. The
finding is closed. This verdict does not validate replacement implementation
or live netlink behavior.

## Operator confirmation

The operator confirmed this behavioral contract. The design interview is
complete.

## Next action

The confirmed implementation spec is published as GitHub issue #6 with the
`ready-for-agent` label. The published body matches the reviewed local draft.

Implement #7 first, then #8. Both tickets carry `ready-for-agent`. GitHub
records a native blocking dependency from #8 to #7. The parent spec remains
open and unchanged. Each child carries a reference to #6.

1. **#7: Exercise AgentX replies against real isolated interfaces.** Establish
   a permanent namespace and actual-binary test harness against current
   production behavior, with required CI coverage. It has no blockers.
2. **#8: Replace subprocess topology reads with the complete netlink monitor.**
   Build the model, monitor, scheduling, recovery, configuration, MIB rendering,
   and package cleanup together. Retain actual-binary protocol coverage and
   remove obsolete subprocess scaffolding in the same change. This ticket is
   blocked by #7 because its required real-interface validation uses that
   harness.

No runtime implementation has started. Read the published ticket body and
comments before starting work on another host. Runtime choice and exact
module interfaces remain implementation choices constrained by this contract.

## Ticket decomposition review

The operator approved the two-ticket breakdown. The review required complete
spec coverage, independently verifiable results, genuine blocking edges, and
no temporary production source or incomplete replacement.

Two designers worked from the confirmed spec and current-code evidence. The
independent designer used a fresh context without the local breakdown or
draft ticket bodies. Both used the session-inherited model and reasoning
effort without overrides. Both candidates were complete before comparison.

| Decision | Local candidate | Independent candidate | Disposition and evidence |
| --- | --- | --- | --- |
| Real-interface harness | Separate first ticket | Separate first ticket | Keep it. Existing wire tests inject ip scripts; real-interface setup is useful before and after replacement. |
| Model, monitor, recovery, configuration, and removal | One replacement ticket | One replacement ticket | Keep them together. The confirmed replacement contract requires all these behaviors to work together. |
| Installed-artifact expansion | Separate third ticket | Required package checks in replacement | Drop the optional separate project. All required package, resource, and release checks remain in the replacement ticket. |

RATIFY merged r1, ticket-decomposition scope. The independent reviewer checked
both draft bodies against all 38 parent user stories. It confirmed the useful
standalone prefactor, complete replacement, and single genuine dependency.
There were no blocking or refuted findings. This verdict does not establish
runtime suitability or implementation execution results.

Coverage ownership: #8 owns all new production behavior, model, configuration,
recovery, and packaging requirements. #7 establishes the real-interface and
protocol baseline; #8 preserves and extends it for netlink and the new monitor
lifecycle. Deterministic loss and timing tests belong to #8.
