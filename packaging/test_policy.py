"""Check the release gate and the service restart policy."""

import configparser
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

import tomllib
import yaml

ROOT = Path(__file__).resolve().parent.parent


PINNED_ACTION = re.compile(r"^[\w.-]+/[\w./-]+@[0-9a-f]{40}$")
MKTEMP = re.compile(r"\bmktemp\b")
PREDICTABLE_TEMP = re.compile(r"(?:/tmp(?:/|\b)|\$\{?TMPDIR\}?/)")


def manifest_version():
    return tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]


def locked_version():
    """The version Cargo.lock records for this package's own entry."""
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text())
    entries = [p for p in lock["package"] if p["name"] == "agentx-ifstack"]
    assert len(entries) == 1, f"Cargo.lock holds {len(entries)} entries"
    return entries[0]["version"]


def changelog_version():
    """The version of the newest Debian changelog stanza."""
    head = (ROOT / "packaging/changelog").read_text().splitlines()[0]
    match = re.match(r"^agentx-ifstack \(([^)-]+)-\d+\)", head)
    assert match, f"unparsable changelog header: {head}"
    return match.group(1)


def workflow_paths():
    directory = ROOT / ".github/workflows"
    return [p for p in directory.iterdir() if p.suffix in (".yml", ".yaml")]


def uses_references(document):
    """Yield every `uses:` value in a workflow, whatever the YAML shape."""
    for job in (document.get("jobs") or {}).items():
        definition = job[1] or {}
        if "uses" in definition:
            yield definition["uses"]
        for step in definition.get("steps") or []:
            if isinstance(step, dict) and "uses" in step:
                yield step["uses"]


def workflow(name):
    return yaml.load(
        (ROOT / ".github/workflows" / name).read_text(), Loader=yaml.BaseLoader
    )


class PackagingPolicyTests(unittest.TestCase):
    def test_releases_come_from_main_and_not_from_a_pushed_tag(self):
        """semantic-release creates the tag, so a tag trigger would double-fire."""
        release_push = workflow("release.yml")["on"]["push"]
        self.assertEqual(release_push.get("branches"), ["main"])
        self.assertNotIn("tags", release_push)
        ci_push = workflow("ci.yml")["on"]["push"]
        self.assertEqual(ci_push.get("branches"), ["**"])
        self.assertNotIn("tags", ci_push)

    def test_the_release_job_waits_for_the_shared_checks(self):
        """A red test gate must stop the release before it tags and publishes."""
        jobs = workflow("release.yml")["jobs"]
        shared = workflow("ci.yml")["jobs"]["check"]["uses"]
        release = jobs["semantic-release"]
        needs = release["needs"]
        needs = [needs] if isinstance(needs, str) else needs
        self.assertTrue(
            any(jobs[name].get("uses") == shared for name in needs),
            "the release job must depend on the same checks as CI",
        )

    def test_the_release_commit_does_not_retrigger_itself(self):
        """The release push would otherwise start another release, forever."""
        release = workflow("release.yml")["jobs"]["semantic-release"]
        self.assertIn("chore(release):", release["if"])
        self.assertEqual(release["concurrency"]["group"], "release")

    def test_every_recorded_version_agrees(self):
        """A version source semantic-release does not rewrite drifts silently."""
        self.assertEqual(locked_version(), manifest_version())
        self.assertEqual(changelog_version(), manifest_version())

    def test_semantic_release_writes_the_manifest_and_syncs_the_rest(self):
        """Cargo.toml is the only file semantic-release writes by itself."""
        config = tomllib.loads((ROOT / "pyproject.toml").read_text())
        release = config["tool"]["semantic_release"]
        self.assertEqual(release["version_toml"], ["Cargo.toml:package.version"])
        self.assertEqual(release["branch"], "main")
        # Everything else must be carried by the build command and staged with it.
        build = release["build_command"]
        self.assertIn("release-build.sh", build)
        chain = (ROOT / "packaging/release-build.sh").read_text()
        self.assertIn("sync-version.sh", chain, "the build command must sync versions")
        self.assertIn("build.sh", chain, "the build command must build the packages")
        self.assertEqual(sorted(release["assets"]), ["Cargo.lock", "packaging/changelog"])
        # Squash merges put the real commit subjects in the body. Without this a squashed
        # feat: is invisible and the release silently under-bumps to a patch.
        parser = release["commit_parser_options"]
        self.assertIs(parser["parse_squash_commits"], True)
        # A feat: on a 0.x version must not jump to 1.0.0 while the wire format settles.
        self.assertIs(release["major_on_zero"], False)
        self.assertIs(release["allow_zero_version"], True)

    def test_the_sync_script_carries_a_bump_into_every_version_source(self):
        """Run the real script on a real copy: a stub would not catch cargo drift."""
        bumped = "9.9.9"
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            for name in ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml"):
                shutil.copy(ROOT / name, work / name)
            shutil.copytree(ROOT / "src", work / "src")
            (work / "packaging").mkdir()
            for name in ("changelog", "sync-version.sh"):
                shutil.copy(ROOT / "packaging" / name, work / "packaging" / name)
            manifest = (work / "Cargo.toml").read_text()
            (work / "Cargo.toml").write_text(
                manifest.replace(f'version = "{manifest_version()}"', f'version = "{bumped}"', 1)
            )

            subprocess.run(
                ["sh", "packaging/sync-version.sh"], cwd=work, check=True,
                capture_output=True, text=True,
            )

            lock = tomllib.loads((work / "Cargo.lock").read_text())
            locked = [p for p in lock["package"] if p["name"] == "agentx-ifstack"]
            self.assertEqual([p["version"] for p in locked], [bumped])
            head = (work / "packaging/changelog").read_text().splitlines()[0]
            self.assertTrue(
                head.startswith(f"agentx-ifstack ({bumped}-1) "), f"changelog head: {head}"
            )
            # The previous stanza must survive so the package keeps its history.
            self.assertIn(f"agentx-ifstack ({manifest_version()}-1) ",
                          (work / "packaging/changelog").read_text())

    def test_permanent_errors_do_not_restart_and_crashes_are_limited(self):
        unit = configparser.ConfigParser(interpolation=None)
        unit.read(ROOT / "packaging/agentx-ifstack.service")
        self.assertEqual(unit["Service"]["Restart"], "on-failure")
        self.assertEqual(unit["Service"]["RestartPreventExitStatus"], "1")
        self.assertEqual(unit["Service"]["RestartSec"], "5s")
        self.assertEqual(unit["Unit"]["StartLimitIntervalSec"], "60s")
        self.assertEqual(unit["Unit"].getint("StartLimitBurst"), 3)

    def test_third_party_actions_are_pinned_to_full_commit_shas(self):
        """A movable tag lets a compromised action change what CI and releases run."""
        unpinned = []
        for path in sorted(workflow_paths()):
            for reference in uses_references(
                yaml.load(path.read_text(), Loader=yaml.BaseLoader)
            ):
                if reference.startswith("./"):
                    continue  # a local reusable workflow is versioned with this repo
                if not PINNED_ACTION.match(reference):
                    unpinned.append(f"{path.name} {reference}")
        self.assertEqual(unpinned, [], "third-party actions must be pinned to a SHA")

    def test_package_scripts_use_private_temporary_files(self):
        """A predictable temporary path lets a local user redirect a root-run write."""
        offenders = []
        for path in sorted((ROOT / "packaging").glob("*.sh")):
            text = path.read_text()
            lines = text.splitlines()
            for number, line in enumerate(lines, 1):
                code = line.split("#", 1)[0]
                if MKTEMP.search(code):
                    continue  # a mktemp template is the safe construction
                if PREDICTABLE_TEMP.search(code):
                    offenders.append(f"{path.name}:{number} {line.strip()}")
            if any(MKTEMP.search(line.split("#", 1)[0]) for line in lines):
                self.assertIn(
                    "trap", text, f"{path.name} must clean up its temporary directory"
                )
        self.assertEqual(offenders, [], "use mktemp -d instead of predictable paths")


if __name__ == "__main__":
    unittest.main()
