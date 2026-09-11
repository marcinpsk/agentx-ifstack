#!/bin/sh
# Propagates the version in Cargo.toml into every other file that records it.
# semantic-release writes Cargo.toml only, so anything it does not know about
# drifts from the released tag unless it is updated here.
set -eu

python3 - <<'PY'
"""Carry the version semantic-release just wrote into the Debian changelog."""
import datetime
import email.utils
import os
import pathlib
import re
import tempfile
import time
import tomllib


def sync_parent(path):
    """Flush the directory entry: a file fsync does not make the rename durable."""
    directory = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


def write_if_changed(path, content):
    """Replace path atomically, and not at all when it already matches.

    A plain write truncates first, so an interruption leaves the file empty and every
    later retry fails on a file it destroyed itself.
    """
    if path.exists() and path.read_text() == content:
        # A previous run may have replaced the file and then failed to sync the parent.
        sync_parent(path)
        return
    handle, temporary = tempfile.mkstemp(dir=path.parent, prefix=path.name, suffix=".tmp")
    try:
        with os.fdopen(handle, "w") as new:
            new.write(content)
            new.flush()
            os.fsync(new.fileno())
        os.replace(temporary, path)
        sync_parent(path)
    except BaseException:
        pathlib.Path(temporary).unlink(missing_ok=True)
        raise

manifest = tomllib.loads(pathlib.Path("Cargo.toml").read_text())
version = manifest["package"]["version"]
maintainer = manifest["package"]["metadata"]["deb"]["maintainer"]

changelog = pathlib.Path("packaging/changelog")
existing = changelog.read_text()
# Skip only the insertion when this version is already the newest stanza. Exiting here
# would leave a run that died between the two writes with a stale Cargo.lock, and the
# retry would not repair it.
if not re.match(rf"^agentx-ifstack \({re.escape(version)}-\d+\) ", existing):
    # Honour SOURCE_DATE_EPOCH so a rebuild of the same release is reproducible.
    stamp = int(os.environ.get("SOURCE_DATE_EPOCH", time.time()))
    # A Debian trailer needs a numeric offset. usegmt writes "GMT", which dpkg and
    # lintian both reject as a badly formatted trailer line.
    released = email.utils.format_datetime(
        datetime.datetime.fromtimestamp(stamp, datetime.timezone.utc)
    )
    entry = (
        f"agentx-ifstack ({version}-1) unstable; urgency=medium\n"
        f"\n"
        f"  * Release {version}. See CHANGELOG.md for the change list.\n"
        f"\n"
        f" -- {maintainer}  {released}\n"
    )
    write_if_changed(changelog, f"{entry}\n{existing}" if existing.strip() else entry)

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
write_if_changed(lock, updated)
PY

