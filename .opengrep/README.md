# opengrep ruleset

Custom [opengrep](https://github.com/opengrep/opengrep) rules that encode this project's
`CLAUDE.md` correctness invariants as machine-checked gates, so the same classes of bug
stop coming back review after review.

## Why opengrep, and not just clippy or CodeQL

- **clippy** is type aware and covers idiomatic Rust far better than a syntactic matcher,
  but it cannot express "this method is only allowed inside this function".
- **CodeQL** (`CodeQL/Analyze (rust)`) covers broad dataflow SAST.
- **opengrep** fills the gap: cheap, readable patterns for *our* invariants, and it is the
  same engine CodeRabbit runs.

## Relationship to CodeRabbit

CodeRabbit auto-detects an opengrep config only when it is named `opengrep.yml` or
`semgrep.yml` (and a few variants), and when it finds one it runs *that* **instead of** its
default packs. This ruleset deliberately avoids those names, so CodeRabbit keeps running
its own packs while these rules are enforced separately by `scripts/opengrep-scan.sh` and
the CI job. Both rulesets apply.

## Layout

| Path | Purpose |
| --- | --- |
| `.opengrep/agentx-ifstack-rules.yaml` | The ruleset, and the single source of truth. Named so CodeRabbit does not adopt it. |
| `.opengrep/tests/*.rs` | Rule-test fixtures. `// ruleid:` must match, `// ok:` must not. They violate the rules on purpose and are not part of the crate. |
| `scripts/opengrep-scan.sh` | Scan `src/`. Exits non-zero on any finding. |
| `scripts/opengrep-test.sh` | Run the rule-tests against the ruleset. |

## Rules

| Rule | Invariant |
| --- | --- |
| `agentx-try-wait-outside-finish` | `try_wait` reaps the child and frees its pid, and that pid is the process group id, so reaping before the group kill lets `kill(-pgid)` reach an unrelated group. Reap only in `IpCommand::finish`. |
| `agentx-unwrap-outside-tests` | `unwrap` panics, and a panic aborts the daemon while systemd counts the restart. |

Suppress a deliberate exception on the line with `// nosemgrep: <rule-id>`.
