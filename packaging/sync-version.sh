#!/bin/sh
# Propagates the version in Cargo.toml into every other file that records it.
# semantic-release writes Cargo.toml only, so anything it does not know about
# drifts from the released tag unless it is updated here.
set -eu

python3 - <<'PY'
"""Carry the version semantic-release just wrote into the Debian changelog."""
import email.utils
import os
import pathlib
import re
import time
import tomllib

manifest = tomllib.loads(pathlib.Path("Cargo.toml").read_text())
version = manifest["package"]["version"]
maintainer = manifest["package"]["metadata"]["deb"]["maintainer"]

changelog = pathlib.Path("packaging/changelog")
existing = changelog.read_text()
if re.match(rf"^agentx-ifstack \({re.escape(version)}-\d+\) ", existing):
    raise SystemExit(0)  # already recorded, keep the build command idempotent

# Honour SOURCE_DATE_EPOCH so a rebuild of the same release is reproducible.
stamp = int(os.environ.get("SOURCE_DATE_EPOCH", time.time()))
released = email.utils.formatdate(stamp, usegmt=True)
entry = (
    f"agentx-ifstack ({version}-1) unstable; urgency=medium\n"
    f"\n"
    f"  * Release {version}. See CHANGELOG.md for the change list.\n"
    f"\n"
    f" -- {maintainer}  {released}\n"
)
changelog.write_text(f"{entry}\n{existing}" if existing.strip() else entry)

# Cargo.lock records this package's own version. Rewrite just that line rather than
# shelling out to cargo, which would need a populated registry cache to run offline.
# A wrong edit cannot slip through: packaging/build.sh then runs cargo build --locked.
lock = pathlib.Path("Cargo.lock")
text = lock.read_text()
pattern = re.compile(
    r'(\[\[package\]\]\nname = "agentx-ifstack"\nversion = ")[^"]+(")'
)
updated, count = pattern.subn(rf"\g<1>{version}\g<2>", text)
if count != 1:
    raise SystemExit(f"Cargo.lock holds {count} agentx-ifstack entries, expected 1")
lock.write_text(updated)
PY

