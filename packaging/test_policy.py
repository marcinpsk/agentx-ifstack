"""Check the release gate and the service restart policy."""

import ast
import configparser
import contextlib
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

import tomllib
import yaml
from pre_commit.clientlib import load_config
from pre_commit.commands.run import Classifier
from pre_commit.hook import Hook
from pre_commit.prefix import Prefix
from pre_commit.repository import _hook

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


def effective_permissions(owner, inherited):
    if "permissions" not in owner:
        return inherited
    declared = owner["permissions"]
    if isinstance(declared, str):
        return {"*": declared.removesuffix("-all")}
    return dict(declared or {})


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

    def test_a_merge_commit_subject_can_bump_the_version(self):
        """Pull requests merge with merge_commit_title = PR_TITLE, so the conventional
        subject lives on the merge commit. python-semantic-release ignores merge commits
        by default, which silently drops that subject and bumps from the branch commits.

        Measured on a scratch repository tagged v0.1.0, with a `fix:` on the branch and a
        `feat:` merge subject: the default yields 0.1.1, this setting yields 0.2.0.
        """
        config = tomllib.loads((ROOT / "pyproject.toml").read_text())
        parser = config["tool"]["semantic_release"]["commit_parser_options"]
        self.assertIs(
            parser.get("ignore_merge_commits"),
            False,
            "merge commits carry the conventional subject here, so they must be parsed",
        )

    def test_the_release_uploads_both_package_formats(self):
        """`version` builds dist/ and creates the release but uploads nothing from it.

        Only `publish` uploads dist_glob_patterns, so a release that runs `version`
        alone ships a tag with no packages attached.
        """
        config = tomllib.loads((ROOT / "pyproject.toml").read_text())
        publish = config["tool"]["semantic_release"]["publish"]
        self.assertIs(publish["upload_to_vcs_release"], True)
        globs = publish["dist_glob_patterns"]
        # build.sh writes into dist/, so a glob elsewhere uploads nothing.
        for suffix in (".deb", ".rpm"):
            self.assertTrue(
                any(
                    glob.startswith("dist/") and glob.endswith(suffix)
                    for glob in globs
                ),
                f"no dist/ glob matches {suffix}: {globs}",
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
                # Both local forms are versioned with this repo, so neither takes a SHA.
                # "$/" is GitHub's self-repository syntax, which is itself a pinning form.
                if reference.startswith(("./", "$/")):
                    continue
                if not PINNED_ACTION.match(reference):
                    unpinned.append(f"{path.name} {reference}")
        self.assertEqual(unpinned, [], "third-party actions must be pinned to a SHA")

    def test_ci_leaves_opengrep_to_coderabbit_and_the_local_hook(self):
        """CodeRabbit skips its own opengrep pass when it sees opengrep in the workflows.

        Running it in CI therefore replaces CodeRabbit's broad coverage with this repo's
        two narrow rules, which is a straight loss. The ruleset runs from a local
        pre-commit hook instead, so both the custom rules and CodeRabbit's packs apply.
        """
        for path in sorted(workflow_paths()):
            text = path.read_text()
            self.assertNotIn(
                "opengrep",
                text.lower(),
                f"{path.name}: opengrep in a workflow suppresses CodeRabbit's own pass; "
                "run it from the local pre-commit hook instead",
            )

        config = ROOT / ".pre-commit-config.yaml"
        self.assertTrue(config.exists(), "the local hook config is missing")
        config_data = yaml.safe_load(config.read_text())
        resolved_config = load_config(str(config))
        self.assertIn("pre-commit", resolved_config["default_install_hook_types"])
        resolved_hooks = {
            hook["id"]: Hook.create(
                repo["repo"], Prefix(str(ROOT)), _hook(hook, root_config=resolved_config)
            )
            for repo in resolved_config["repos"]
            for hook in repo["hooks"]
        }
        hooks = [
            hook for repo in config_data["repos"] for hook in repo.get("hooks", [])
        ]
        by_id = {h.get("id"): h for h in hooks}
        # Read the paths from disk rather than sampling, so a filter narrowed to one file
        # cannot pass while real files silently lose coverage.
        sources = sorted(str(f.relative_to(ROOT)) for f in (ROOT / "src").rglob("*.rs"))
        ruleset = sorted(
            str(f.relative_to(ROOT)) for f in (ROOT / ".opengrep").rglob("*") if f.is_file()
        )
        self.assertTrue(sources and ruleset, "expected source and ruleset files on disk")

        for hook_id, entry, must_match, must_skip in (
            ("opengrep", "scripts/opengrep-scan.sh", sources + ruleset, []),
            ("opengrep-rule-tests", "scripts/opengrep-test.sh", ruleset, sources),
        ):
            hook = by_id.get(hook_id)
            self.assertIsNotNone(hook, f"the {hook_id} hook is missing")
            # A hook can be neutered without touching its files filter.
            self.assertEqual(hook.get("entry"), entry, f"{hook_id} runs the wrong command")
            self.assertEqual(hook.get("language"), "script", f"{hook_id} must run the script")
            self.assertIsNone(hook.get("exclude"), f"{hook_id} must not exclude its inputs")
            resolved = resolved_hooks[hook_id]
            self.assertFalse(resolved.pass_filenames, f"{hook_id} must scan all inputs")
            self.assertIn(
                "pre-commit",
                resolved.stages,
                f"{hook_id} must run at the pre-commit stage",
            )
            self.assertTrue((ROOT / entry).is_file(), f"{entry} does not exist")
            self.assertTrue(os.access(ROOT / entry, os.X_OK), f"{entry} is not executable")
            pattern = hook.get("files")
            self.assertTrue(pattern, f"hook {hook_id} needs a files filter")
            for candidate in must_match:
                self.assertRegex(candidate, pattern, f"{hook_id} would skip {candidate}")
            for candidate in must_skip:
                self.assertNotRegex(candidate, pattern, f"{hook_id} runs for {candidate}")
            with contextlib.chdir(ROOT):
                selected = set(
                    Classifier.from_config(
                        sources + ruleset,
                        resolved_config["files"],
                        resolved_config["exclude"],
                    ).filenames_for_hook(resolved)
                )
            self.assertTrue(
                set(must_match) <= selected,
                f"{hook_id} skips {set(must_match) - selected}",
            )
            self.assertFalse(set(must_skip) & selected, f"{hook_id} selects excluded inputs")

    def test_a_called_workflow_never_outranks_its_callers(self):
        """A reusable workflow's job cannot exceed the permissions of the job calling it.

        GitHub refuses the run rather than trimming the request. Resolve effective
        permissions on both sides, because a job inherits the workflow's top-level block,
        and compare access levels: a callee may ask for less than the caller grants, but
        never more.
        """
        levels = {"none": 0, "read": 1, "write": 2}

        def level_of(perms, scope):
            return levels[perms.get(scope, perms.get("*", "none"))]

        callers = {}
        for path in sorted(workflow_paths()):
            data = workflow(path.name)
            top = effective_permissions(data, {})
            for job in data.get("jobs", {}).values():
                target = str(job.get("uses", ""))
                if not target.startswith(("./", "$/")):
                    continue
                callers.setdefault(target.rsplit("/", 1)[-1], []).append(
                    (path.name, effective_permissions(job, top))
                )

        for name, grants in sorted(callers.items()):
            callee = workflow(name)
            self.assertIn("permissions", callee, f"{name} declares no permissions")
            callee_top = effective_permissions(callee, {})
            for job_name, job in callee.get("jobs", {}).items():
                wanted = effective_permissions(job, callee_top)
                scopes = set(wanted).union(*(g for _, g in grants))
                for scope in sorted(scopes):
                    need = level_of(wanted, scope)
                    if need == 0:
                        continue
                    for caller_name, granted in grants:
                        self.assertGreaterEqual(
                            level_of(granted, scope),
                            need,
                            f"{name} job {job_name} requests {scope} level {need}, but "
                            f"{caller_name} grants level {level_of(granted, scope)}",
                        )

    def test_the_workflows_are_audited_by_zizmor(self):
        """Workflow permissions and injection risks are not covered by the other gates."""
        steps = [
            step
            for job in workflow("checks.yml")["jobs"].values()
            for step in job.get("steps", [])
        ]
        uses = " ".join(str(step.get("uses", "")) for step in steps)
        self.assertIn("zizmor", uses, "no job audits the workflows with zizmor")
        audits = [
            (job, step)
            for job in workflow("checks.yml")["jobs"].values()
            for step in job.get("steps", [])
            if str(step.get("uses", "")).startswith("zizmorcore/zizmor-action@")
        ]
        self.assertTrue(
            any(
                "if" not in job
                and "if" not in step
                and job.get("continue-on-error", "false") == "false"
                and step.get("continue-on-error", "false") == "false"
                for job, step in audits
            ),
            "zizmor must run without conditions and propagate failures",
        )

    def test_no_workflow_runs_twice_for_one_push(self):
        """push on every branch plus pull_request runs every job twice on a PR branch.

        pull_request builds the merge commit, which is the result that matters for a
        pull request, so push stays on main for post-merge validation.
        """
        for path in sorted(workflow_paths()):
            workflow = yaml.safe_load(path.read_text())
            # PyYAML follows YAML 1.1, where a bare `on:` key parses as the boolean True.
            triggers = workflow.get("on", workflow.get(True))
            # GitHub Actions also accepts `on: [push, pull_request]`.
            if isinstance(triggers, list):
                self.assertFalse(
                    "push" in triggers and "pull_request" in triggers,
                    f"{path.name}: push and pull_request both fire on a PR branch, "
                    "so every job runs twice; limit push to main",
                )
                continue
            if not isinstance(triggers, dict) or "pull_request" not in triggers:
                continue
            if "push" not in triggers:
                continue
            push = triggers["push"]
            # GitHub Actions allows an event with no configuration, which PyYAML loads
            # as None. A bare `push:` fires on every branch.
            self.assertIsNotNone(
                push,
                f"{path.name}: a bare push trigger fires on every branch, so every job "
                "runs twice on a PR branch; limit push to main",
            )
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

        # Where the ruleset actually runs is asserted by
        # test_ci_leaves_opengrep_to_coderabbit_and_the_local_hook: the pre-commit hook,
        # never CI, because CodeRabbit skips its own pass when it sees opengrep there.

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

    def test_documented_gh_json_fields_exist(self):
        """gh rejects an unknown --json field, so a wrong one makes the skill unusable.

        `gh pr list --json authorAssociation` shipped here once and fails with
        "Unknown JSON field". gh validates the names locally, before auth and before
        any request, and an empty --json prints the accepted set for a subcommand.
        """
        documented = []
        for path in sorted((ROOT / "docs" / "agents").glob("*.md")):
            for span in re.findall(r"`([^`]*--json[^`]*)`", path.read_text()):
                command = re.search(r"\bgh\s+(\w[\w-]*)\s+(\w[\w-]*)", span)
                fields = re.search(r"--json\s+([A-Za-z][A-Za-z,]*)", span)
                if command and fields:
                    documented.append(
                        (path.name, command[1], command[2], fields[1].split(","))
                    )
        self.assertTrue(documented, "expected documented gh --json commands")

        accepted = {}
        with tempfile.TemporaryDirectory() as config:
            for _, group, subcommand, _fields in documented:
                if (group, subcommand) in accepted:
                    continue
                probe = subprocess.run(
                    ["gh", group, subcommand, "--json"],
                    capture_output=True,
                    text=True,
                    timeout=30,
                    check=False,
                    env={**os.environ, "GH_TOKEN": "", "GH_CONFIG_DIR": config},
                )
                output = probe.stdout + probe.stderr
                self.assertIn(
                    "comma-separated fields",
                    output,
                    f"gh {group} {subcommand} --json did not list its fields: {output}",
                )
                accepted[(group, subcommand)] = {
                    line.strip() for line in output.splitlines() if line.startswith("  ")
                }

        for name, group, subcommand, fields in documented:
            for field in fields:
                self.assertIn(
                    field,
                    accepted[(group, subcommand)],
                    f"{name}: gh {group} {subcommand} has no --json field {field!r}",
                )


class GuardRegressionTests(unittest.TestCase):
    def setUp(self):
        global ROOT
        original_root = ROOT
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        for name in (".github", ".opengrep", "src", "scripts", "docs"):
            shutil.copytree(ROOT / name, self.root / name)
        shutil.copy2(ROOT / ".pre-commit-config.yaml", self.root)
        self.addCleanup(setattr, sys.modules[__name__], "ROOT", original_root)
        ROOT = self.root
        self.addCleanup(os.chdir, Path.cwd())
        os.chdir(ROOT)
        self.policy = PackagingPolicyTests()

    def mutate(self, name, change):
        path = ROOT / name
        data = yaml.safe_load(path.read_text())
        change(data)
        path.write_text(yaml.safe_dump(data))

    def reject_hook(self, change):
        self.mutate(".pre-commit-config.yaml", change)
        with self.assertRaises(AssertionError):
            self.policy.test_ci_leaves_opengrep_to_coderabbit_and_the_local_hook()

    def test_hook_rejects_effective_file_filters(self):
        path = ROOT / ".pre-commit-config.yaml"
        baseline = path.read_text()
        for owner, field, value in (
            ("hook", "types", ["python"]),
            ("hook", "types_or", ["python"]),
            ("hook", "exclude_types", ["rust"]),
            ("global", "exclude", ".*"),
            ("global", "files", r"\.py$"),
        ):
            with self.subTest(owner=owner, field=field):
                path.write_text(baseline)

                def change(config, owner=owner, field=field, value=value):
                    target = config if owner == "global" else config["repos"][0]["hooks"][1]
                    target[field] = value

                self.reject_hook(change)

    def test_hook_rejects_passed_filenames(self):
        self.reject_hook(
            lambda config: config["repos"][0]["hooks"][1].update(pass_filenames=True)
        )

    def test_hook_rejects_missing_install_stage(self):
        self.reject_hook(
            lambda config: config.update(default_install_hook_types=["pre-push"])
        )

    def test_hook_accepts_effective_defaults(self):
        def change(config):
            config.pop("default_install_hook_types")
            config.pop("default_stages")
            config["repos"][0]["hooks"][1]["types_or"] = ["rust", "yaml", "markdown"]

        self.mutate(".pre-commit-config.yaml", change)
        self.policy.test_ci_leaves_opengrep_to_coderabbit_and_the_local_hook()

    def test_permissions_reject_wildcard_increases(self):
        for caller, callee in (
            ("read-all", "write-all"),
            ({}, "read-all"),
            ({"contents": "write"}, "read-all"),
        ):
            with self.subTest(caller=caller, callee=callee):
                for name, permissions in (
                    ("ci.yml", caller), ("release.yml", caller), ("checks.yml", callee)
                ):
                    self.mutate(
                        f".github/workflows/{name}",
                        lambda data, permissions=permissions: data.update(
                            permissions=permissions
                        ),
                    )
                with self.assertRaisesRegex(AssertionError, "checks.yml"):
                    self.policy.test_a_called_workflow_never_outranks_its_callers()

    def test_permissions_accept_wildcard_reductions(self):
        for name in ("ci.yml", "release.yml"):
            self.mutate(
                f".github/workflows/{name}",
                lambda data: data.update(permissions="write-all"),
            )
        self.mutate(
            ".github/workflows/checks.yml",
            lambda data: data.update(permissions="read-all"),
        )
        self.policy.test_a_called_workflow_never_outranks_its_callers()

    def test_audit_rejects_disabled_or_nonblocking_execution(self):
        path = ROOT / ".github/workflows/checks.yml"
        baseline = path.read_text()
        for owner, field, value in (
            ("job", "if", "false"),
            ("job", "if", "${{ github.event_name == 'push' }}"),
            ("step", "if", "${{ false }}"),
            ("job", "continue-on-error", True),
            ("step", "continue-on-error", True),
        ):
            with self.subTest(owner=owner, field=field, value=value):
                path.write_text(baseline)

                def change(data, owner=owner, field=field, value=value):
                    job = data["jobs"]["zizmor"]
                    target = job if owner == "job" else job["steps"][-1]
                    target[field] = value

                self.mutate(".github/workflows/checks.yml", change)
                with self.assertRaises(AssertionError):
                    self.policy.test_the_workflows_are_audited_by_zizmor()

    def test_external_pr_query_reads_later_pages(self):
        document = (ROOT / "docs/agents/issue-tracker.md").read_text()
        command = shlex.split(re.search(
            r'`(gh api "repos/<owner>/<repo>/pulls\?state=open"[^`]+)`', document
        )[1])
        pages = []

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                page = int(parse_qs(urlsplit(self.path).query).get("page", ["1"])[0])
                pages.append(page)
                records = [
                    {
                        "number": number,
                        "title": "Example",
                        "user": {"login": "contributor"},
                        "author_association": "MEMBER" if page == 1 else "CONTRIBUTOR",
                    }
                    for number in (range(1, 31) if page == 1 else [31])
                ]
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                if page == 1:
                    self.send_header("Link", f'<{endpoint}&page=2>; rel="next"')
                self.end_headers()
                self.wfile.write(json.dumps(records).encode())

            def log_message(self, *_args):
                pass

        with ThreadingHTTPServer(("localhost", 0), Handler) as server:
            endpoint = (
                f"http://localhost:{server.server_port}"
                "/repos/example/project/pulls?state=open"
            )
            command[2] = endpoint
            thread = threading.Thread(target=server.serve_forever)
            thread.start()
            try:
                result = subprocess.run(
                    command, capture_output=True, text=True, timeout=15, check=False,
                    env={
                        **os.environ,
                        "GH_TOKEN": "placeholder",
                        "GH_ENTERPRISE_TOKEN": "placeholder",
                        "GH_CONFIG_DIR": str(ROOT / "gh-config"),
                        "GH_DEBUG": "",
                    },
                )
            finally:
                server.shutdown()
                thread.join()
        self.assertEqual(result.returncode, 0, result.stderr)
        records = []
        output = result.stdout.strip()
        while output:
            page, end = json.JSONDecoder().raw_decode(output)
            records.extend(page)
            output = output[end:].strip()
        self.assertEqual([record["number"] for record in records], [31], result.stdout)
        self.assertEqual(pages, [1, 2])


if __name__ == "__main__":
    unittest.main()
