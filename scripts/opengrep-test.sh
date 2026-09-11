#!/usr/bin/env bash
# Run opengrep rule-tests for .opengrep/agentx-ifstack-rules.yaml against the annotated
# fixtures in .opengrep/tests/. Each fixture carries `// ruleid:` and `// ok:` markers
# asserting which lines must and must not match.
#
# `opengrep test` pairs a <stem>.yaml rule file with a same-stem <stem>.rs fixture in one
# directory. To keep one source of truth, stage a temporary directory pairing a copy of
# the ruleset with each fixture, then run the test there.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

opengrep_bin="${OPENGREP_BIN:-}"
if [[ -z "$opengrep_bin" ]]; then
  if command -v opengrep >/dev/null 2>&1; then
    opengrep_bin="$(command -v opengrep)"
  else
    echo "error: opengrep not found. Install it from https://github.com/opengrep/opengrep" >&2
    echo "       (or set OPENGREP_BIN=/path/to/opengrep)." >&2
    exit 1
  fi
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

for fixture in "$repo_root"/.opengrep/tests/*.rs; do
  stem="$(basename "$fixture" .rs)"
  cp "$repo_root/.opengrep/agentx-ifstack-rules.yaml" "$tmp/$stem.yaml"
  cp "$fixture" "$tmp/$stem.rs"
done

exec "$opengrep_bin" test "$tmp"
