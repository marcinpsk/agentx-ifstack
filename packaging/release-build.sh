#!/bin/sh
# Runs as semantic-release's build_command, after it writes the version into
# Cargo.toml and before it stages `assets`.
set -eu

sh packaging/sync-version.sh
sh packaging/build.sh

# semantic-release runs this before it commits, tags and pushes, so failing here stops
# the release. A release that is already published cannot be un-published.
for suffix in deb rpm; do
    [ -n "$(find dist -type f -name "*.${suffix}" -print -quit)" ] || {
        echo "dist holds no .${suffix}, refusing to release an incomplete package set" >&2
        exit 1
    }
done
