"""Check the release gate and the service restart policy."""

import configparser
import re
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parent.parent


PINNED_ACTION = re.compile(r"^[\w.-]+/[\w./-]+@[0-9a-f]{40}$")
MKTEMP = re.compile(r"\bmktemp\b")
PREDICTABLE_TEMP = re.compile(r"(?:/tmp(?:/|\b)|\$\{?TMPDIR\}?/)")


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
    def test_tag_pushes_only_use_the_release_workflow(self):
        ci_push = workflow("ci.yml")["on"]["push"]
        self.assertIsInstance(ci_push, dict)
        self.assertEqual(ci_push.get("branches"), ["**"])
        self.assertNotIn("tags", ci_push)
        self.assertEqual(workflow("release.yml")["on"]["push"]["tags"], ["v*"])

    def test_release_requires_the_ci_checks(self):
        ci_check = workflow("ci.yml")["jobs"]["check"]
        self.assertIn("uses", ci_check, "CI checks must use a shared workflow")
        jobs = workflow("release.yml")["jobs"]
        pending = ["publish"]
        prerequisites = set()
        while pending:
            name = pending.pop()
            if name in prerequisites:
                continue
            prerequisites.add(name)
            needs = jobs[name].get("needs", [])
            pending.extend([needs] if isinstance(needs, str) else needs)
        self.assertTrue(
            any(jobs[name].get("uses") == ci_check["uses"] for name in prerequisites),
            "Publishing must depend on the same checks as CI",
        )
        self.assertTrue(
            any(
                jobs[name].get("uses") == "./.github/workflows/packages.yml"
                for name in prerequisites
            ),
            "Publishing must also depend on package validation",
        )

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
