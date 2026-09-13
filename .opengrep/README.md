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

CodeRabbit steps aside from opengrep in two separate ways, and this setup avoids both.

It auto-detects an opengrep config only when it is named `opengrep.yml` or `semgrep.yml`
(and a few variants), and when it finds one it runs *that* **instead of** its default
packs. This ruleset deliberately avoids those names.

It also skips its own opengrep pass when it sees opengrep running in the workflows. These
rules therefore run from a **local pre-commit hook only**, never in CI. Running them in CI
would trade CodeRabbit's broad packs for this repo's narrow rule, which is a straight
loss. A policy test asserts no workflow mentions opengrep.

Install the hook with `pre-commit install --install-hooks`. Both hooks carry a `files`
filter, because the scripts scan the whole crate and would otherwise run on every commit:
the scan runs when `src/` or the rules change, the rule-tests only when the rules change. A
commit touching neither costs nothing.

## Layout

| Path | Purpose |
| --- | --- |
| `.opengrep/agentx-ifstack-rules.yaml` | The ruleset, and the single source of truth. Named so CodeRabbit does not adopt it. |
| `.opengrep/tests/*.rs` | Rule-test fixtures. `// ruleid:` must match, `// ok:` must not. They violate the rules on purpose and are not part of the crate. |
| `scripts/opengrep-scan.sh` | Scan `src/`. Exits non-zero on any finding. Wired to pre-commit by `.pre-commit-config.yaml`. |
| `scripts/opengrep-test.sh` | Run the rule-tests against the ruleset. Runs when the rules change. |

## Rules

| Rule | Invariant |
| --- | --- |
| `agentx-unwrap-outside-tests` | `unwrap` panics, and a panic aborts the daemon while systemd counts the restart. |

Suppress a deliberate exception on the line with `// nosemgrep: <rule-id>`.
