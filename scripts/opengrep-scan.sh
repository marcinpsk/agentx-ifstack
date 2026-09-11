#!/usr/bin/env bash
# Run the custom opengrep ruleset over the source tree. CodeRabbit keeps running its own
# opengrep packs, because the ruleset is deliberately not named so CodeRabbit adopts it
# as its config. See .opengrep/README.md.
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

# Scan explicit targets if given, otherwise the crate source. The fixtures under
# .opengrep/tests/ violate the rules on purpose and are not part of the crate.
targets=("$@")
if [[ ${#targets[@]} -eq 0 ]]; then
  targets=("$repo_root/src")
fi

exec "$opengrep_bin" scan \
  --config "$repo_root/.opengrep/agentx-ifstack-rules.yaml" \
  --error \
  "${targets[@]}"
