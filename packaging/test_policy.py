"""Check the release gate and the service restart policy."""

import configparser
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parent.parent


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


if __name__ == "__main__":
    unittest.main()
