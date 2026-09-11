#!/bin/sh
# Runs as semantic-release's build_command, after it writes the version into
# Cargo.toml and before it stages `assets`.
set -eu

sh packaging/sync-version.sh
sh packaging/build.sh
