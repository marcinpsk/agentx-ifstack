"""Check the release gate and the service restart policy."""

import ast
import configparser
import contextlib
import functools
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
SHELL_PART = re.compile(r"""'[^']*'|"(?:\\.|[^"\\])*"|\\.|\#[^\n]*|[;&|()\n]+""")
REAL_NAMESPACE_JQ = """
  [
    .[]
    | select(
        .reason == "compiler-artifact"
        and .target.kind == ["test"]
        and .target.name == "real_namespace"
        and .executable != null
      )
    | .executable
  ]
  | if length == 1 then .[0] else error("expected one real_namespace test artifact") end
"""
REAL_NAMESPACE_IMAGE = [
    "docker",
    "build",
    "--tag",
    "agentx-ifstack-real-namespace",
    "--file",
    ".github/real-namespace.Dockerfile",
    ".github",
]
REAL_NAMESPACE_DOCKER = [
    "docker",
    "run",
    "--rm",
    "--network",
    "none",
    "--read-only",
    "--tmpfs",
    "/tmp:rw,nosuid,nodev,exec",
    "--cap-drop",
    "ALL",
    "--cap-add",
    "SYS_ADMIN",
    "--cap-add",
    "NET_ADMIN",
    "--cap-add",
    "SETUID",
    "--cap-add",
    "SETGID",
    "--cap-add",
    "KILL",
    "--security-opt",
    "no-new-privileges",
    "--entrypoint",
    "$TEST_BINARY",
    "--volume",
    "$PWD:$PWD:ro",
    "--workdir",
    "$PWD",
    "agentx-ifstack-real-namespace",
    "--ignored",
]
REAL_NAMESPACE_COMMANDS = [
    ["ARTIFACTS=$RUNNER_TEMP/real-namespace-artifacts.json"],
    ["BINARY_PATH=$RUNNER_TEMP/real-namespace-binary"],
    [
        "cargo",
        "test",
        "--locked",
        "--test",
        "real_namespace",
        "--no-run",
        "--message-format=json",
        ">",
        "$ARTIFACTS",
    ],
    [
        "jq",
        "--slurp",
        "--raw-output",
        "--exit-status",
        REAL_NAMESPACE_JQ,
        "$ARTIFACTS",
        ">",
        "$BINARY_PATH",
    ],
    ["IFS=", "read", "-r", "TEST_BINARY", "<", "$BINARY_PATH"],
    ["test", "-x", "$TEST_BINARY"],
    REAL_NAMESPACE_IMAGE,
    REAL_NAMESPACE_DOCKER,
]


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


INLINE_CODE = re.compile(r"(`+)(?!`)(.+?)(?<!`)\1(?!`)", re.DOTALL)


def markdown_code_spans(path):
    """Extract inline code and fenced blocks in document order."""
    spans = []
    prose = []
    fenced = []
    marker = None

    def inline_spans():
        spans.extend(match[2] for match in INLINE_CODE.finditer("".join(prose)))
        prose.clear()

    for line in path.read_text().splitlines(keepends=True):
        fence = re.match(r"^ {0,3}(`{3,}|~{3,})(.*)$", line.rstrip("\n"))
        if marker is None:
            if fence:
                inline_spans()
                marker = fence[1]
            else:
                prose.append(line)
        elif (fence and fence[1][0] == marker[0]
              and len(fence[1]) >= len(marker) and not fence[2].strip()):
            spans.append("".join(fenced))
            fenced.clear()
            marker = None
        else:
            fenced.append(line)
    assert marker is None, f"{path}: unclosed code fence"
    inline_spans()
    return spans


def shell_commands(text):
    """Yield shell token lists while splitting shell operators."""
    text = text.replace("\\\n", "")
    start = 0
    for part in SHELL_PART.finditer(text):
        if part[0][0] in ";&|()\n":
            words = shlex.split(text[start:part.start()], comments=True)
            if words:
                yield words
            start = part.end()
    words = shlex.split(text[start:], comments=True)
    if words:
        yield words


def documented_shell_commands(path):
    """Yield shell token lists from Markdown code spans."""
    for span in markdown_code_spans(path):
        yield from shell_commands(span)


def runs_real_namespace_suite(step):
    """Return whether a workflow step runs the suite with a compatible iproute2."""
    return list(shell_commands(str(step.get("run", "")))) == REAL_NAMESPACE_COMMANDS


def iproute_requirements(requirements):
    """Return forbidden runtime requirement names."""
    return [name for name in requirements if "iproute" in name]


@functools.cache
def gh_json_fields(group, subcommand):
    """Field names gh accepts for `gh <group> <sub> --json`, read from gh itself."""
    with tempfile.TemporaryDirectory() as config:
        probe = subprocess.run(
            ["gh", group, subcommand, "--json"],
            capture_output=True, text=True, timeout=30, check=False,
            env={**os.environ, "GH_TOKEN": "", "GH_CONFIG_DIR": config},
        )
    output = probe.stdout + probe.stderr
    if "comma-separated fields" not in output:
        raise AssertionError(
            f"gh {group} {subcommand} --json did not list its fields: {output}"
        )
    return frozenset(
        line.strip() for line in output.splitlines() if line.startswith("  ")
    )


def documented_gh_json_commands(path):
    """Yield (group, subcommand, fields) for each documented `gh ... --json` command."""
    yield from _gh_json_commands(documented_shell_commands(path))


def documented_gh_json_commands_in(text):
    """The same, for one Markdown fragment rather than a whole document."""
    yield from _gh_json_commands(
        shlex.split(span) for span in INLINE_CODE.findall(text) for span in [span[1]]
    )


def _gh_json_commands(commands):
    for words in commands:
        if "gh" not in words:
            continue
        arguments = words[words.index("gh") + 1:]
        if len(arguments) < 2:
            continue
        group, subcommand = arguments[:2]
        _, options = shell_options(arguments[2:], {"--json"})
        for option, value in options:
            if option == "--json" and value:
                yield group, subcommand, value.split(",")


def shell_options(arguments, value_options):
    """Return positional tokens and ordered option/value pairs for a CLI schema."""
    positional = []
    options = []
    words = iter(arguments)
    for word in words:
        if word == "--":
            positional.extend(words)
            break
        if not word.startswith("-") or word == "-":
            positional.append(word)
            continue
        option, equals, value = word.partition("=")
        if not equals:
            value = next(words, None) if option in value_options else None
        options.append((option, value))
    return positional, options


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

    def test_the_unit_reports_active_only_once_the_table_is_registered(self):
        """A subagent that never registers serves nothing, and must not look healthy."""
        unit = configparser.ConfigParser(interpolation=None)
        unit.read(ROOT / "packaging/agentx-ifstack.service")
        self.assertEqual(unit["Service"]["Type"], "notify")
        self.assertEqual(unit["Service"]["NotifyAccess"], "main")
        self.assertEqual(unit["Service"]["TimeoutStartSec"], "60s")

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

    def test_ci_runs_the_real_namespace_agentx_suite(self):
        """The namespace suite must not disappear behind Cargo's ignored-test default."""
        executions = [
            (job, step)
            for job in workflow("checks.yml")["jobs"].values()
            for step in job.get("steps", [])
            if runs_real_namespace_suite(step)
        ]
        self.assertTrue(executions, "CI does not run the real namespace AgentX suite")
        self.assertTrue(
            any(
                "if" not in job
                and "if" not in step
                and job.get("continue-on-error", "false") == "false"
                and step.get("continue-on-error", "false") == "false"
                for job, step in executions
            ),
            "the real namespace AgentX suite must run and propagate failures",
        )

    def test_documented_real_namespace_command_resolves_cargo_before_sudo(self):
        """sudo can replace PATH, so invoke the Cargo binary resolved by the caller."""
        documented = list(documented_shell_commands(ROOT / "README.md"))
        self.assertIn(["CARGO=$(command -v cargo)"], documented)
        self.assertIn(
            [
                "sudo",
                "env",
                "HOME=$HOME",
                "$CARGO",
                "test",
                "--locked",
                "--test",
                "real_namespace",
                "--",
                "--ignored",
            ],
            documented,
        )

    def test_real_namespace_ci_uses_a_compatible_iproute_userspace(self):
        """Use one current iproute2 to create every real-interface fixture."""
        step = next(
            step
            for job in workflow("checks.yml")["jobs"].values()
            for step in job.get("steps", [])
            if step.get("name") == "Test AgentX against real network namespaces"
        )
        run = str(step.get("run", ""))
        commands = list(shell_commands(run))
        image_build = next(
            (
                words
                for words in commands
                if words[:3] == ["docker", "build", "--tag"]
            ),
            None,
        )
        self.assertEqual(
            image_build,
            [
                "docker",
                "build",
                "--tag",
                "agentx-ifstack-real-namespace",
                "--file",
                ".github/real-namespace.Dockerfile",
                ".github",
            ],
        )
        dockerfile = (ROOT / ".github/real-namespace.Dockerfile").read_text()
        self.assertIn("FROM debian:13", dockerfile)
        self.assertIn("apt-get install --yes --no-install-recommends iproute2", dockerfile)

    def test_real_namespace_ci_uses_cargos_exact_test_artifact(self):
        """A leftover executable must not replace the test artifact Cargo built."""
        step = next(
            step
            for job in workflow("checks.yml")["jobs"].values()
            for step in job.get("steps", [])
            if step.get("name") == "Test AgentX against real network namespaces"
        )
        run = str(step.get("run", ""))
        self.assertIn("--message-format=json", run)
        self.assertIn('target.name == "real_namespace"', run)
        self.assertNotIn("find target/debug/deps", run)

    def test_real_namespace_ci_limits_container_authority(self):
        """The test may administer only its isolated container network namespace."""
        step = next(
            step
            for job in workflow("checks.yml")["jobs"].values()
            for step in job.get("steps", [])
            if step.get("name") == "Test AgentX against real network namespaces"
        )
        run = str(step.get("run", ""))
        self.assertNotIn("--privileged", run)
        for option in (
            "--network none",
            "--read-only",
            "--tmpfs /tmp:rw,nosuid,nodev,exec",
            "--cap-drop ALL",
            "--cap-add SYS_ADMIN",
            "--cap-add NET_ADMIN",
            "--cap-add SETUID",
            "--cap-add SETGID",
            "--cap-add KILL",
            "--security-opt no-new-privileges",
        ):
            self.assertIn(option, run)

    def test_the_documented_zizmor_command_matches_ci(self):
        """A narrower local command passes while CI fails.

        The documented command audited `.github/workflows/` while the action defaults to
        the repo root, so `dependabot.yml` was never audited locally and its three
        cooldown findings only appeared in CI.
        """
        step = next(
            step
            for job in workflow("checks.yml")["jobs"].values()
            for step in job.get("steps", [])
            if str(step.get("uses", "")).startswith("zizmorcore/zizmor-action@")
        )
        options = step.get("with") or {}
        # Defaults from the action's own action.yml at the pinned SHA.
        expected_inputs = sorted(str(options.get("inputs", ".")).split())
        expected_persona = str(options.get("persona", "regular"))

        invocations = []
        for name in ("CLAUDE.md", "README.md"):
            for words in documented_shell_commands(ROOT / name):
                index = next(
                    (
                        position
                        for position, word in enumerate(words)
                        if word == "zizmor" or word.startswith("zizmor@")
                    ),
                    None,
                )
                if index is None or len(words) == 1:
                    continue  # prose naming the tool, not a command
                invocations.append((name, words, index))
        self.assertTrue(invocations, "no documented zizmor command to check")

        for name, words, index in invocations:
            arguments = words[index + 1 :]
            paths, parsed = shell_options(arguments, {"--persona", "--collect"})
            paths.sort()
            self.assertEqual(
                paths,
                expected_inputs,
                f"{name}: the documented zizmor command audits {paths}, "
                f"but the CI job audits {expected_inputs}",
            )
            persona = "regular"
            collection = []
            for option, value in parsed:
                if option == "--persona":
                    persona = value
                elif option in ("--pedantic", "-p"):
                    persona = "pedantic"
                elif option == "--collect":
                    self.assertIsNotNone(value, "--collect requires a value")
                    collection.extend(value.split(","))
            self.assertEqual(
                collection or ["default"], ["default"],
                f"{name}: documented collection must match CI's default collection",
            )
            self.assertEqual(
                persona,
                expected_persona,
                f"{name}: the documented zizmor command uses persona {persona}, "
                f"but the CI job uses {expected_persona}",
            )

    def test_agent_guidance_describes_the_netlink_topology_contract(self):
        """Future changes must start from the implemented monitor boundaries."""
        guidance = (ROOT / "CLAUDE.md").read_text()
        configuration = guidance.split("## Configuration and packages", 1)[1]
        configuration = configuration.split("## Build and test", 1)[0]
        for requirement in (
            "docs/adr/0001-monitor-topology-independently-of-agentx.md",
            "docs/adr/0002-implement-the-netlink-monitor-as-a-process-actor.md",
            "socket, reconcile, priority, and log_level",
        ):
            self.assertIn(requirement, configuration)

        data_source = guidance.split("## Data source", 1)[1]
        data_source = data_source.split("## AgentX constraints", 1)[0]
        self.assertIn("typed route-netlink messages", data_source)
        self.assertIn("`NLMSG_DONE`", data_source)

    def test_runtime_package_and_service_do_not_depend_on_iproute(self):
        """The test harness can use ip, but the installed daemon does not."""
        manifest = tomllib.loads((ROOT / "Cargo.toml").read_text())
        deb = manifest["package"]["metadata"]["deb"]
        rpm = manifest["package"]["metadata"]["generate-rpm"]
        self.assertNotIn("iproute", deb.get("depends", ""))
        self.assertEqual(iproute_requirements(rpm.get("requires", {})), [])
        unit = (ROOT / "packaging/agentx-ifstack.service").read_text()
        self.assertNotIn("Environment=PATH=", unit)

        shipped = tomllib.loads((ROOT / "packaging/agentx-ifstack.toml").read_text())
        self.assertEqual(shipped["reconcile"], 3600)
        self.assertNotIn("refresh", shipped)

    def test_every_distribution_package_test_runs_the_non_root_scenario(self):
        """A distribution package test must prove access through agentXPerms."""
        expected = ["sh", "/work/packaging/non-root-agentx.sh"]
        scripts = sorted((ROOT / "packaging").glob("test-*.sh"))
        self.assertTrue(scripts, "no distribution package test scripts found")
        missing = [
            path.name
            for path in scripts
            if expected not in shell_commands(path.read_text())
        ]
        self.assertEqual(
            missing,
            [],
            "distribution package tests missing the non-root AgentX scenario",
        )

    def test_non_root_documentation_matches_the_net_snmp_permission_model(self):
        """The recipe must separate directory traversal from socket access."""
        directive = "agentXPerms 0660 0755 root agentx-ifstack"
        for name in ("README.md", "packaging/agentx-ifstack.8"):
            text = (ROOT / name).read_text()
            if name == "README.md":
                section = text.split("To run the subagent without root", 1)[1]
                section = section.split("On Debian,", 1)[0]
            else:
                section = text.split(".SH NON-ROOT OPERATION", 1)[1]
                section = section.split(".SH FILES", 1)[0]
            self.assertIn(directive, section, name)
            self.assertNotIn("agentXPerms 0660 0770", section, name)
            self.assertIn("existing", section, name)
            self.assertIn("traversable", section, name)
            self.assertIn("setpriv", section, name)
            self.assertIn("do not exercise", section, name)

    def test_non_root_scenario_handles_agentx_permissions_safely(self):
        """The scenario must wait for final metadata and preserve prior state."""
        source = (ROOT / "packaging/non-root-agentx.sh").read_text()
        self.assertIn("agentXPerms 0660 0755 root $subagent_group", source)
        self.assertLess(
            source.index('if [ -L "$agentx_dir" ]'),
            source.index("agentx_dir_existed=yes"),
        )
        self.assertRegex(
            source,
            r'(?s)created_user.*getent passwd "\$subagent_user".*userdel '
            r'"\$subagent_user"',
        )
        self.assertRegex(
            source,
            r'(?s)created_group.*getent group "\$subagent_group".*groupdel '
            r'"\$subagent_group"',
        )
        socket_wait = source.split("master_pid=$!", 1)[1].split("done", 1)[0]
        self.assertIn("660:0:$subagent_gid", socket_wait)
        self.assertIn('stat -c \'%g\' "$agentx_dir")" = 0', source)
        self.assertIn('test -x "$agentx_dir"', source)

    def test_non_root_scenario_attributes_rows_to_the_non_root_child(self):
        """Published rows must come from the child and match kernel interfaces."""
        source = (ROOT / "packaging/non-root-agentx.sh").read_text()
        launch = source.index("/usr/bin/agentx-ifstack")
        self.assertLess(source.index('"$work/pre-subagent.walk"'), launch)
        self.assertIn('"/proc/$subagent_pid/status"', source)
        self.assertIn("/sys/class/net/*/ifindex", source)
        self.assertIn('-v expected_indexes="$work/interface-indexes"', source)
        self.assertIn(
            "agentx-ifstack did not register and publish rows after 30 attempts",
            source,
        )
        self.assertNotIn(
            "agentx-ifstack did not register and publish rows within 30 seconds",
            source,
        )

    def test_non_root_scenario_treats_the_manual_as_diagnostic_output(self):
        """Manual rendering must not decide whether the runtime scenario passes."""
        source = (ROOT / "packaging/non-root-agentx.sh").read_text()
        manual = source.split("MANWIDTH=", 1)[1].split(
            'cat > "$work/snmpd.conf"', 1
        )[0]
        self.assertIn("Installed snmpd.conf agentXPerms section:", manual)
        self.assertIn("The installed snmpd.conf manual page is missing.", manual)
        self.assertNotIn("manual_signature=", manual)
        self.assertNotIn("expected_signature=", manual)
        self.assertNotIn("fail ", manual)

    def run_walk_check(self, interfaces, rows):
        """Run the shipped walk validator over one synthetic ifStackTable walk."""
        prefix = ".1.3.6.1.2.1.31.1.2.1.3."
        with tempfile.TemporaryDirectory() as work:
            indexes = Path(work) / "interface-indexes"
            indexes.write_text("".join(f"{index}\n" for index in interfaces))
            walk = Path(work) / "ifstack.walk"
            walk.write_text(
                "".join(f"{prefix}{row} = INTEGER: 1\n" for row in rows)
            )
            return subprocess.run(
                [
                    "awk",
                    "-v", f"prefix={prefix}",
                    "-v", f"expected_indexes={indexes}",
                    "-f", str(ROOT / "packaging/ifstack-walk-check.awk"),
                    str(walk),
                ],
                capture_output=True, text=True, timeout=30, check=False,
            )

    def assert_walk_accepted(self, interfaces, rows):
        result = self.run_walk_check(interfaces, rows)
        self.assertEqual(result.returncode, 0, result.stderr)

    def assert_walk_rejected(self, interfaces, rows, reason):
        result = self.run_walk_check(interfaces, rows)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(reason, result.stderr)

    def test_non_root_scenario_runs_the_shipped_walk_validator(self):
        """The scenario must validate through the file these tests exercise."""
        source = (ROOT / "packaging/non-root-agentx.sh").read_text()
        self.assertIn("ifstack-walk-check.awk", source)
        self.assertTrue((ROOT / "packaging/ifstack-walk-check.awk").is_file())
        self.assertNotIn("zero_higher[", source)

    def test_walk_check_accepts_a_standalone_topology(self):
        """Interfaces that stack on nothing carry both boundary rows."""
        self.assert_walk_accepted([1, 2], ["0.1", "0.2", "1.0", "2.0"])

    def test_walk_check_accepts_a_bond_over_one_member(self):
        """A relationship row is valid, and it removes one boundary row per side."""
        self.assert_walk_accepted([3, 9], ["0.9", "3.0", "9.3"])

    def test_walk_check_accepts_a_vlan_over_a_bond_over_a_member(self):
        """A bond between a VLAN and a member carries no boundary row at all."""
        self.assert_walk_accepted([3, 9, 10], ["0.10", "3.0", "9.3", "10.9"])

    def test_walk_check_rejects_a_boundary_row_the_relationships_exclude(self):
        """A zero-higher row is wrong when another interface runs over it."""
        self.assert_walk_rejected(
            [3, 9, 10],
            ["0.9", "0.10", "3.0", "9.3", "10.9"],
            "unexpected zero-higher boundary row for interface 9",
        )

    def test_walk_check_rejects_an_interface_that_stacks_on_itself(self):
        """A relationship row may not name one interface on both sides."""
        self.assert_walk_rejected([1, 2], ["0.2", "1.1", "2.0"], "on both sides")

    def test_walk_check_rejects_a_missing_boundary_row(self):
        """Every side of the stack that no relationship covers needs its row."""
        self.assert_walk_rejected(
            [1, 2], ["0.1", "0.2", "1.0"], "missing zero-lower boundary row"
        )

    def test_walk_check_rejects_an_unknown_interface(self):
        """A row must name an interface the container actually has."""
        self.assert_walk_rejected(
            [1, 2],
            ["0.1", "0.2", "1.0", "2.0", "7.0"],
            "unknown interface",
        )

    def test_walk_check_rejects_rows_outside_rfc_index_order(self):
        """GETNEXT walks depend on the index order."""
        self.assert_walk_rejected(
            [1, 2], ["0.2", "0.1", "1.0", "2.0"], "not in RFC index order"
        )

    def test_walk_check_rejects_an_empty_walk(self):
        """An empty walk proves nothing."""
        self.assert_walk_rejected([1], [], "returned no rows")

    def test_runtime_package_dependency_check_matches_variants(self):
        """A distribution-specific package suffix must not bypass the guard."""
        self.assertEqual(
            iproute_requirements({"iproute2": "*", "systemd": "*"}),
            ["iproute2"],
        )

    def test_topology_change_checks_wait_for_publication(self):
        """Asynchronous topology assertions must use the bounded wait helper."""
        source = (ROOT / "tests/real_namespace.rs").read_text()
        test = source.split(
            "fn topology_changes_eventually_reach_get_and_walk()", 1
        )[1].split("\n#[test]", 1)[0]
        self.assertNotIn("std::thread::sleep", test)

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
            for words in documented_shell_commands(path):
                if "gh" not in words:
                    continue
                arguments = words[words.index("gh") + 1:]
                if len(arguments) < 2:
                    continue
                group, subcommand = arguments[:2]
                _, options = shell_options(arguments[2:], {"--json"})
                for option, value in options:
                    if option == "--json" and value:
                        documented.append(
                            (path.name, group, subcommand, value.split(","))
                        )
        self.assertTrue(documented, "expected documented gh --json commands")
        for name, group, subcommand, fields in documented:
            for field in fields:
                self.assertIn(
                    field,
                    gh_json_fields(group, subcommand),
                    f"{name}: gh {group} {subcommand} has no --json field {field!r}",
                )

    def test_documented_procedures_fetch_the_fields_they_read(self):
        """A procedure that names a field in prose must request it in its own command.

        The frontier query listed `state,blockedBy,assignees` and then told the reader to
        add `body`, so the documented command could not detect a `Blocked by:` fallback.
        """
        incomplete = []
        for path in sorted((ROOT / "docs" / "agents").glob("*.md")):
            for item in re.split(r"^(?=- )", path.read_text(), flags=re.MULTILINE):
                requested = set()
                accepted = set()
                for group, subcommand, fields in documented_gh_json_commands_in(item):
                    requested.update(fields)
                    accepted |= gh_json_fields(group, subcommand)
                mentioned = {
                    match[2] for match in INLINE_CODE.finditer(item)
                } & (accepted - requested)
                incomplete.extend(
                    f"{path.name}: {field!r} is read in prose but no --json list "
                    "in the same step requests it"
                    for field in sorted(mentioned)
                )
        self.assertEqual(incomplete, [])


class GuardRegressionTests(unittest.TestCase):
    def setUp(self):
        global ROOT
        original_root = ROOT
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        for name in (".github", ".opengrep", "src", "scripts", "docs"):
            shutil.copytree(ROOT / name, self.root / name)
        for name in (".pre-commit-config.yaml", "CLAUDE.md", "README.md"):
            shutil.copy2(ROOT / name, self.root)
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

    def test_real_namespace_guard_rejects_disabled_or_nonblocking_execution(self):
        path = ROOT / ".github/workflows/checks.yml"
        baseline = path.read_text()
        for owner, field, value in (
            ("job", "if", "false"),
            ("step", "if", "${{ false }}"),
            ("job", "continue-on-error", True),
            ("step", "continue-on-error", True),
        ):
            with self.subTest(owner=owner, field=field, value=value):
                path.write_text(baseline)

                def change(data, owner=owner, field=field, value=value):
                    job = data["jobs"]["check"]
                    step = next(
                        step for step in job["steps"] if runs_real_namespace_suite(step)
                    )
                    target = job if owner == "job" else step
                    target[field] = value

                self.mutate(".github/workflows/checks.yml", change)
                with self.assertRaises(AssertionError):
                    self.policy.test_ci_runs_the_real_namespace_agentx_suite()

    def test_real_namespace_guard_rejects_an_echo_decoy(self):
        required = "cargo test --locked --test real_namespace -- --ignored"

        def change(data):
            job = data["jobs"]["check"]
            step = next(
                step for step in job["steps"] if runs_real_namespace_suite(step)
            )
            step["if"] = "${{ false }}"
            job["steps"].append({"run": f"echo {required}"})

        self.mutate(".github/workflows/checks.yml", change)
        with self.assertRaises(AssertionError):
            self.policy.test_ci_runs_the_real_namespace_agentx_suite()

    def test_real_namespace_guard_rejects_an_artifact_decoy(self):
        path = ROOT / ".github/workflows/checks.yml"

        def change(data):
            job = data["jobs"]["check"]
            step = next(
                step for step in job["steps"] if runs_real_namespace_suite(step)
            )
            run = str(step["run"])
            step["run"] = (
                "echo --message-format=json\n"
                "echo 'target.name == \"real_namespace\"'\n"
                "TEST_BINARY=/bin/true\n"
                f"{run[run.index('docker build'):]}"
            )

        self.mutate(path.relative_to(ROOT), change)
        with self.assertRaises(AssertionError):
            self.policy.test_ci_runs_the_real_namespace_agentx_suite()

    def test_real_namespace_guard_requires_the_test_binary_entrypoint(self):
        def change(data):
            job = data["jobs"]["check"]
            step = next(
                step for step in job["steps"] if runs_real_namespace_suite(step)
            )
            step["run"] = str(step["run"]).replace(
                '  --entrypoint "$TEST_BINARY" \\\n', ""
            )

        self.mutate(".github/workflows/checks.yml", change)
        with self.assertRaises(AssertionError):
            self.policy.test_ci_runs_the_real_namespace_agentx_suite()

    def test_gh_json_guard_rejects_a_field_the_cli_lacks(self):
        document = ROOT / "docs/agents/issue-tracker.md"
        document.write_text(
            document.read_text().replace(
                "--json subIssues", "--json subIssues,authorAssociation"
            )
        )
        with self.assertRaisesRegex(AssertionError, "authorAssociation"):
            self.policy.test_documented_gh_json_fields_exist()

    def test_zizmor_guard_rejects_a_documented_command_narrower_than_ci(self):
        baseline = (ROOT / "CLAUDE.md").read_text()
        for replacement, expected in (
            ("uvx --native-tls zizmor .github/workflows/", "audits"),
            ("uvx --native-tls zizmor --persona=pedantic .", "persona"),
        ):
            with self.subTest(replacement=replacement):
                (ROOT / "CLAUDE.md").write_text(
                    baseline.replace("uvx --native-tls zizmor .", replacement)
                )
                with self.assertRaisesRegex(AssertionError, expected):
                    self.policy.test_the_documented_zizmor_command_matches_ci()
        (ROOT / "CLAUDE.md").write_text(baseline)

        # The guard follows the job, so widening CI alone must also fail.
        self.mutate(
            ".github/workflows/checks.yml",
            lambda data: data["jobs"]["zizmor"]["steps"][-1].update(
                {"with": {"advanced-security": False, "inputs": ".github .opengrep"}}
            ),
        )
        with self.assertRaisesRegex(AssertionError, "audits"):
            self.policy.test_the_documented_zizmor_command_matches_ci()

    def test_ticket_fetch_resolves_pr_before_issue(self):
        document = (ROOT / "docs/agents/issue-tracker.md").read_text()
        instruction = document.split('## When a skill says "fetch the relevant ticket"')[1]
        instruction = instruction.split("##", 1)[0]
        self.assertIn(
            "gh pr view <number> --comments` and fall back to "
            "`gh issue view <number> --comments", instruction,
        )

    def test_field_completeness_guard_checks_prose_against_the_command(self):
        path = ROOT / "docs/agents/issue-tracker.md"
        baseline = path.read_text()
        for old, new, expected in (
            # A field dropped from the command while the prose still reads it.
            ("--json state,blockedBy,assignees,body", "--json state,blockedBy,assignees", "body"),
            # A field named in prose that no command in the step requests.
            ("has an open entry in `blockedBy`", "has an open entry in `blockedBy` or `milestone`",
             "milestone"),
        ):
            with self.subTest(expected=expected):
                self.assertIn(old, baseline)
                path.write_text(baseline.replace(old, new))
                with self.assertRaisesRegex(AssertionError, expected):
                    self.policy.test_documented_procedures_fetch_the_fields_they_read()

        # A mention satisfied by a different command in the same step is not a defect,
        # and a mention that is not a real field is not one either: the external-PR step
        # names authorAssociation precisely to say gh does not have it.
        path.write_text(baseline.replace(
            "The first eligible child in map order wins.",
            "The first eligible child in map order wins, read from `subIssues`.",
        ))
        self.policy.test_documented_procedures_fetch_the_fields_they_read()
        path.write_text(baseline)
        self.policy.test_documented_procedures_fetch_the_fields_they_read()

    def test_gh_json_guard_checks_complete_fields_in_every_command(self):
        path = ROOT / "docs/agents/issue-tracker.md"
        baseline = path.read_text()
        for addition, field in (
            ('`gh issue view 1 --json=authorAssociation`', "authorAssociation"),
            ('`gh issue view 1 --json "authorAssociation"`', "authorAssociation"),
            ('`gh issue view 1 --json title_typo`', "title_typo"),
            ('`gh issue view 1 --json \\\n authorAssociation`', "authorAssociation"),
            ('```sh\ngh issue view 1 --json title\ngh issue view 1 --json authorAssociation\n```', "authorAssociation"),
            ('~~~sh\ngh issue view 1 --json authorAssociation\n~~~', "authorAssociation"),
            *(
                (f'`gh issue view 1 --json title {operator} gh issue view 1 --json title_typo`', "title_typo")
                for operator in (";", "&&", "||", "|", "&")
            ),
        ):
            with self.subTest(addition=addition):
                path.write_text(baseline + "\n" + addition + "\n")
                with self.assertRaisesRegex(AssertionError, field):
                    self.policy.test_documented_gh_json_fields_exist()

    def test_zizmor_guard_checks_collection_and_persona_options(self):
        path = ROOT / "CLAUDE.md"
        baseline = path.read_text()
        for option, expected in (
            ("--collect=workflows", "collect"),
            ('--collect "workflows"', "collect"),
            ("--pedantic", "persona"),
            ("-p", "persona"),
        ):
            with self.subTest(option=option):
                path.write_text(baseline.replace(
                    "uvx --native-tls zizmor .", f"uvx --native-tls zizmor {option} ."
                ))
                with self.assertRaisesRegex(AssertionError, expected):
                    self.policy.test_the_documented_zizmor_command_matches_ci()

    def test_zizmor_guard_checks_tilde_fences_in_another_document(self):
        path = ROOT / "README.md"
        path.write_text(path.read_text() +
                        "\n~~~sh\nuvx --native-tls zizmor .github/workflows/\n~~~\n")
        with self.assertRaisesRegex(AssertionError, "audits"):
            self.policy.test_the_documented_zizmor_command_matches_ci()

    def test_external_pr_query_reads_later_pages(self):
        document = (ROOT / "docs/agents/issue-tracker.md").read_text()
        command = re.search(
            r'`(gh api "repos/<owner>/<repo>/pulls\?state=open"[^`]+)`', document
        )[1]
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
                        "author_association": "MEMBER" if number < 30 else "CONTRIBUTOR",
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
            command = command.replace(
                '"repos/<owner>/<repo>/pulls?state=open"', shlex.quote(endpoint)
            )
            thread = threading.Thread(target=server.serve_forever)
            thread.start()
            try:
                result = subprocess.run(
                    ["bash", "-o", "pipefail", "-c", command],
                    capture_output=True, text=True, timeout=15, check=False,
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
        records = json.loads(result.stdout)
        self.assertEqual([record["number"] for record in records], [30, 31], result.stdout)
        self.assertEqual(pages, [1, 2])

    def test_external_pr_query_requires_pagination_and_one_json_value(self):
        path = ROOT / "docs/agents/issue-tracker.md"
        baseline = path.read_text()
        for old, new in (
            ("--paginate --slurp", "--slurp"),
            ("--paginate --slurp | jq '[.[][]", "--paginate --jq '[.[]"),
        ):
            with self.subTest(replacement=new):
                self.assertIn(old, baseline)
                path.write_text(baseline.replace(old, new))
                with self.assertRaises((AssertionError, json.JSONDecodeError)):
                    self.test_external_pr_query_reads_later_pages()

    def test_documented_commands_preserve_valid_quotes_and_continuations(self):
        path = ROOT / "docs/agents/issue-tracker.md"
        path.write_text(
            path.read_text() + '\n`gh issue view 1 --json="title,body"`\n'
            '`gh issue view 1 --json "title,body"`\n'
            '~~~sh\ngh issue view 1 --json \\\n title,body\n~~~\n'
            '```sh\ngh issue view 1 --json title # fields\n'
            'gh issue view 1 --json body\n```\n'
        )
        self.policy.test_documented_gh_json_fields_exist()
        path = ROOT / "README.md"
        path.write_text(path.read_text() +
                        '\n~~~sh\nuvx --native-tls zizmor \\\n'
                        ' --persona "regular" --collect=default .\n~~~\n')
        self.policy.test_the_documented_zizmor_command_matches_ci()

    def test_shell_commands_keep_quoted_operators_as_arguments(self):
        path = ROOT / "commands.md"
        path.write_text('````sh\ngh issue view 1 --json="title,body" '
                        '--jq ".[] | .title"; printf "%s" "|"\n````\n')
        commands = list(documented_shell_commands(path))
        self.assertEqual(commands, [
            ["gh", "issue", "view", "1", "--json=title,body", "--jq", ".[] | .title"],
            ["printf", "%s", "|"],
        ])
        self.assertEqual(shell_options(commands[0][3:], {"--json", "--jq"}),
                         (["1"], [("--json", "title,body"), ("--jq", ".[] | .title")]))


if __name__ == "__main__":
    unittest.main()
