"""Check the release gate and the service restart policy."""

import ast
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
        # CI runs on pull requests only, so it has no push trigger to carry a tag.
        self.assertNotIn("push", workflow("ci.yml")["on"])

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

    def test_the_release_uploads_both_package_formats(self):
        """`version` builds dist/ and creates the release but uploads nothing from it.

        Only `publish` uploads dist_glob_patterns, so a release that runs `version`
        alone ships a tag with no packages attached.
        """
        config = tomllib.loads((ROOT / "pyproject.toml").read_text())
        publish = config["tool"]["semantic_release"]["publish"]
        self.assertIs(publish["upload_to_vcs_release"], True)
        globs = publish["dist_glob_patterns"]
        for suffix in (".deb", ".rpm"):
            self.assertTrue(
                any(glob.endswith(suffix) for glob in globs),
                f"no dist glob matches {suffix}: {globs}",
            )
        workflow = yaml.safe_load((ROOT / ".github/workflows/release.yml").read_text())
        steps = workflow["jobs"]["semantic-release"]["steps"]
        commands = "\n".join(str(step.get("run", "")) for step in steps)
        self.assertIn(
            "semantic-release version", commands, "the release must compute a version"
        )
        self.assertIn(
            "semantic-release publish",
            commands,
            "run semantic-release publish, or the packages never reach the release",
        )
        # publish defaults to the latest release, which would attach this run's packages
        # to the previous tag when nothing was bumped. Pin the whole command: a stale
        # literal or an unrelated variable would satisfy a bare "--tag" check.
        self.assertIn(
            'semantic-release publish --tag "$after"',
            commands,
            "publish must upload to the tag this run created",
        )

    def test_the_build_command_refuses_an_incomplete_package_set(self):
        """build_command runs before the tag, so a missing format must stop the release.

        Checking after `semantic-release version` is too late: it has already committed,
        tagged, pushed and created the release by then. Run the real script against a
        stubbed build so the guard itself is exercised.
        """
        def run(files, directories=()):
            with tempfile.TemporaryDirectory() as directory:
                work = Path(directory)
                (work / "packaging").mkdir()
                shutil.copy(
                    ROOT / "packaging/release-build.sh", work / "packaging/release-build.sh"
                )
                (work / "packaging/sync-version.sh").write_text("#!/bin/sh\n")
                (work / "packaging/build.sh").write_text(
                    "#!/bin/sh\nset -eu\nmkdir -p dist\n"
                    + "".join(f"touch dist/pkg{suffix}\n" for suffix in files)
                    + "".join(f"mkdir -p dist/pkg{suffix}\n" for suffix in directories)
                )
                return subprocess.run(
                    ["sh", "packaging/release-build.sh"], cwd=work,
                    capture_output=True, text=True, check=False,
                )

        self.assertEqual(run([".deb", ".rpm"]).returncode, 0, "a complete set must build")
        for files, missing in (([".deb"], ".rpm"), ([".rpm"], ".deb"), ([], "both")):
            self.assertNotEqual(
                run(files).returncode, 0,
                f"the build command accepted a package set missing {missing}",
            )
        # A directory carrying the suffix is not a package.
        for files, directories, shape in (
            ([".rpm"], [".deb"], "a directory named *.deb"),
            ([".deb"], [".rpm"], "a directory named *.rpm"),
        ):
            self.assertNotEqual(
                run(files, directories).returncode, 0,
                f"the build command accepted {shape}",
            )

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

    def test_the_generated_changelog_trailer_parses(self):
        """Debian trailers need a numeric timezone offset, and lintian fails on a warning.

        Parse the generated file with dpkg itself rather than a regex: the hand written
        stanza was valid, so nothing caught the generator until the first real release.
        """
        parser = shutil.which("dpkg-parsechangelog")
        if parser is None:
            self.skipTest("dpkg-parsechangelog is not installed")
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
                manifest.replace(f'version = "{manifest_version()}"', 'version = "9.9.9"', 1)
            )
            subprocess.run(
                ["sh", "packaging/sync-version.sh"], cwd=work, check=True,
                capture_output=True, text=True,
            )
            parsed = subprocess.run(
                [parser, "-l", str(work / "packaging/changelog")],
                capture_output=True, text=True, check=True,
            )
            self.assertEqual(
                parsed.stderr.strip(), "", "dpkg rejected the generated changelog"
            )
            self.assertIn("Version: 9.9.9-1", parsed.stdout)

    def test_the_sync_script_finishes_a_half_applied_run(self):
        """A retry after a crash between the two writes must still fix Cargo.lock."""
        bumped = "9.9.9"
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            for name in ("Cargo.toml", "Cargo.lock"):
                shutil.copy(ROOT / name, work / name)
            (work / "packaging").mkdir()
            shutil.copy(ROOT / "packaging/sync-version.sh", work / "packaging/sync-version.sh")
            manifest = (work / "Cargo.toml").read_text()
            (work / "Cargo.toml").write_text(
                manifest.replace(f'version = "{manifest_version()}"', f'version = "{bumped}"', 1)
            )
            # The state a crash between the changelog write and the lock write leaves:
            # the changelog already names the new version, Cargo.lock still does not.
            (work / "packaging/changelog").write_text(
                f"agentx-ifstack ({bumped}-1) unstable; urgency=medium\n"
                f"\n  * Release {bumped}.\n\n -- A B <a@b.invalid>  "
                f"Thu, 11 Sep 2026 00:00:00 +0000\n"
            )

            subprocess.run(
                ["sh", "packaging/sync-version.sh"], cwd=work, check=True,
                capture_output=True, text=True,
            )

            lock = tomllib.loads((work / "Cargo.lock").read_text())
            locked = [p for p in lock["package"] if p["name"] == "agentx-ifstack"]
            self.assertEqual(
                [p["version"] for p in locked], [bumped],
                "a retry left Cargo.lock stale, so cargo build --locked would fail",
            )
            # The changelog must not gain a second stanza for the same version.
            body = (work / "packaging/changelog").read_text()
            self.assertEqual(body.count(f"agentx-ifstack ({bumped}-1)"), 1)

    def test_a_synchronised_retry_does_not_rewrite_anything(self):
        """Rewriting an already-correct file opens a truncation window for no gain."""
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            for name in ("Cargo.toml", "Cargo.lock"):
                shutil.copy(ROOT / name, work / name)
            (work / "packaging").mkdir()
            for name in ("changelog", "sync-version.sh"):
                shutil.copy(ROOT / "packaging" / name, work / "packaging" / name)

            def run():
                subprocess.run(
                    ["sh", "packaging/sync-version.sh"], cwd=work, check=True,
                    capture_output=True, text=True,
                )

            run()
            watched = [work / "Cargo.lock", work / "packaging/changelog"]
            before = [(p.stat().st_ino, p.stat().st_mtime_ns) for p in watched]
            run()
            after = [(p.stat().st_ino, p.stat().st_mtime_ns) for p in watched]
            self.assertEqual(before, after, "a no-op retry rewrote a file")

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

    def test_no_workflow_runs_twice_for_one_push(self):
        """push on every branch plus pull_request runs every job twice on a PR branch.

        pull_request builds the merge commit, which is the result that matters for a
        pull request, so push stays on main for post-merge validation.
        """
        for path in sorted(workflow_paths()):
            workflow = yaml.safe_load(path.read_text())
            # PyYAML follows YAML 1.1, where a bare `on:` key parses as the boolean True.
            triggers = workflow.get("on", workflow.get(True))
            if not isinstance(triggers, dict) or "pull_request" not in triggers:
                continue
            push = triggers.get("push")
            if push is None:
                continue
            self.assertEqual(
                push.get("branches"),
                ["main"],
                f"{path.name}: push and pull_request both fire on a PR branch, "
                "so every job runs twice; limit push to main",
            )

    def test_no_workflow_checkout_persists_its_credential(self):
        """actions/checkout leaves the token in .git/config, where any later step reads it."""
        persisting = []
        for path in sorted(workflow_paths()):
            document = yaml.load(path.read_text(), Loader=yaml.BaseLoader)
            for job_name, job in (document.get("jobs") or {}).items():
                for step in (job or {}).get("steps") or []:
                    if not isinstance(step, dict):
                        continue
                    if not str(step.get("uses", "")).startswith("actions/checkout@"):
                        continue
                    if (step.get("with") or {}).get("persist-credentials") != "false":
                        persisting.append(f"{path.name}:{job_name}")
        self.assertEqual(persisting, [], "checkout must not persist credentials")

    def test_the_custom_ruleset_adds_to_coderabbit_instead_of_replacing_it(self):
        """CodeRabbit runs a detected opengrep config INSTEAD OF its default packs.

        A file named opengrep.yml or semgrep.yml would silently replace that coverage,
        so the ruleset carries a name CodeRabbit does not adopt and is passed with
        --config instead.
        """
        for name in (
            ".semgrep.yaml", ".semgrep.yml", "semgrep.yaml", "semgrep.yml",
            ".opengrep.yaml", ".opengrep.yml", "opengrep.yaml", "opengrep.yml",
        ):
            self.assertFalse(
                (ROOT / name).exists(),
                f"{name} would replace CodeRabbit's own opengrep packs",
            )
        ruleset = ROOT / ".opengrep/agentx-ifstack-rules.yaml"
        self.assertTrue(ruleset.exists(), "the custom ruleset is missing")

        # Every rule needs a fixture, or it can silently stop matching.
        rules = yaml.safe_load(ruleset.read_text())["rules"]
        self.assertTrue(rules, "the ruleset declares no rules")
        for rule in rules:
            fixture = ROOT / ".opengrep/tests" / f"{rule['id']}.rs"
            self.assertTrue(
                fixture.exists(), f"rule {rule['id']} has no rule-test fixture"
            )

        steps = workflow("checks.yml")["jobs"]["rules"]["steps"]
        commands = "\n".join(str(step.get("run", "")) for step in steps)
        self.assertIn("opengrep-test.sh", commands, "CI must run the rule-tests")
        self.assertIn("opengrep-scan.sh", commands, "CI must run the ruleset")
        # The binary is fetched over the network, so pin it by digest.
        self.assertIn("sha256sum -c -", commands, "pin the opengrep binary by checksum")

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

    def test_the_sync_script_makes_each_replacement_durable(self):
        """A file fsync leaves the new directory entry unflushed, so a crash can undo it.

        Ordering is the property that matters, so read the syntax tree rather than the
        text. The no-op path must sync too, or a retry after a failed sync never repairs it.
        """
        script = (ROOT / "packaging/sync-version.sh").read_text()
        embedded = re.search(r"python3 - <<'PY'\n(.*?)\nPY\n", script, re.DOTALL)
        self.assertIsNotNone(embedded, "the sync script must embed a python3 heredoc")
        tree = ast.parse(embedded.group(1))
        functions = {
            node.name: node
            for node in ast.walk(tree)
            if isinstance(node, ast.FunctionDef)
        }
        self.assertIn("sync_parent", functions, "expected a sync_parent helper")

        # The helper must fsync a descriptor opened on the parent directory.
        opened = [
            node
            for node in ast.walk(functions["sync_parent"])
            if isinstance(node, ast.Assign)
            and isinstance(node.value, ast.Call)
            and isinstance(node.value.func, ast.Attribute)
            and node.value.func.attr == "open"
            and node.value.args
            and isinstance(node.value.args[0], ast.Attribute)
            and node.value.args[0].attr == "parent"
        ]
        self.assertEqual(len(opened), 1, "sync_parent must open the parent directory")
        descriptor = opened[0].targets[0].id
        self.assertTrue(
            any(
                isinstance(node, ast.Call)
                and isinstance(node.func, ast.Attribute)
                and node.func.attr == "fsync"
                and node.args
                and isinstance(node.args[0], ast.Name)
                and node.args[0].id == descriptor
                for node in ast.walk(functions["sync_parent"])
            ),
            f"sync_parent must fsync {descriptor}",
        )

        writer = functions["write_if_changed"]
        calls = [
            node.lineno
            for node in ast.walk(writer)
            if isinstance(node, ast.Call)
            and isinstance(node.func, ast.Name)
            and node.func.id == "sync_parent"
        ]
        replace = [
            node.lineno
            for node in ast.walk(writer)
            if isinstance(node, ast.Call)
            and isinstance(node.func, ast.Attribute)
            and node.func.attr == "replace"
        ]
        self.assertEqual(len(replace), 1, "expected exactly one os.replace")
        self.assertTrue(
            any(line > replace[0] for line in calls),
            "call sync_parent after os.replace, or a crash can revert the rename",
        )
        returns = [
            node.lineno for node in ast.walk(writer) if isinstance(node, ast.Return)
        ]
        self.assertTrue(returns, "expected the unchanged-content early return")
        self.assertTrue(
            any(line < min(returns) for line in calls),
            "call sync_parent before the early return, so a retry repairs a failed sync",
        )

if __name__ == "__main__":
    unittest.main()
