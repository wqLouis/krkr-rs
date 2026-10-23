#!/usr/bin/env python3
"""Validate the release CI contract without running GitHub Actions.

The release workflow is the one place where a mistake is expensive: a wrong tag
filter can start a paid build for every push, a missing dependency fails after
the gate, and two jobs that both create a GitHub Release race each other. This
check reads the workflow as text (no YAML dependency) and asserts the pieces of
the contract the repository relies on:

* the tag trigger is 'v*' and nothing else, with 'workflow_dispatch' kept for
  manual runs;
* the static gate is a job, and both platform jobs wait for it;
* the Linux job builds 'target/release/krkr' and packages it with the license
  and a run note;
* the Android job invokes 'build-android.sh' and 'check-jni-libs.sh';
* exactly one final job publishes the release and attaches both artifacts.

The check deliberately validates names and commands, not a full YAML schema; a
real workflow parser would be a dependency and would still not know which
command is the gate. The fixture under 'scripts/fixtures/ci/' proves each rule
can fail.

Usage:
    uv run --project scripts krkr-gate ci               # check the real workflow
    uv run --project scripts krkr-gate ci --self-test   # only the fixture self-test
    uv run --project scripts krkr-gate ci --quiet       # only print failures
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

from ..common import FIXTURES_DIR, REPO_ROOT

FIXTURES = FIXTURES_DIR / "ci"
FIXTURE_WORKFLOW = FIXTURES / "good.yml"
FIXTURE_MUTATIONS = FIXTURES / "mutations.json"

GATE_JOB = "gate"
LINUX_JOB = "linux"
ANDROID_JOB = "apk"
RELEASE_JOB = "release"

GATE_COMMAND = "uv run --project scripts krkr-gate gate"
LINUX_ARTIFACT = "krkr-rs-linux-x86_64"
ANDROID_ARTIFACT = "krkr-rs-release-arm64-v8a"
LINUX_TARBALL = "krkr-rs-linux-x86_64.tar.gz"

Finding = tuple[str, str]


def _top_level_block(text: str, key: str) -> str | None:
    """Return the indented body of a column-0 'key:' block."""
    lines = text.splitlines()
    start = None
    for index, line in enumerate(lines):
        if re.fullmatch(rf"{re.escape(key)}:\s*(?:#.*)?", line):
            start = index
            break
    if start is None:
        return None
    end = len(lines)
    for index in range(start + 1, len(lines)):
        line = lines[index]
        if line and not line[0].isspace() and not line.startswith("#"):
            end = index
            break
    return "\n".join(lines[start + 1 : end])


def _job_blocks(text: str) -> dict[str, str]:
    """Return '{job name: body}' for the two-space keys under 'jobs:'."""
    lines = text.splitlines()
    jobs_at = None
    for index, line in enumerate(lines):
        if re.fullmatch(r"jobs:\s*(?:#.*)?", line):
            jobs_at = index
            break
    if jobs_at is None:
        return {}
    jobs: dict[str, list[str]] = {}
    current: str | None = None
    for line in lines[jobs_at + 1 :]:
        if line and not line[0].isspace() and not line.startswith("#"):
            break
        match = re.fullmatch(r"  ([A-Za-z0-9_-]+):\s*(?:#.*)?", line)
        if match:
            current = match.group(1)
            jobs[current] = []
            continue
        if current is not None:
            jobs[current].append(line)
    return {name: "\n".join(body) for name, body in jobs.items()}


def _needs(body: str) -> set[str]:
    """Return the job names in a job's 'needs:' line (scalar or list)."""
    match = re.search(r"^\s+needs:\s*(.+?)\s*$", body, re.MULTILINE)
    if not match:
        return set()
    raw = match.group(1).split("#", 1)[0].strip()
    raw = raw.strip("[]")
    return {
        token.strip().strip("'\"")
        for token in re.split(r"[,\s]+", raw)
        if token.strip()
    }


def _tag_patterns(on_block: str) -> list[str]:
    """Return the 'push.tags' patterns declared under 'on:'."""
    patterns: list[str] = []
    in_tags = False
    for line in on_block.splitlines():
        if re.fullmatch(r"\s+tags:\s*", line):
            in_tags = True
            continue
        if not in_tags:
            continue
        match = re.fullmatch(r"\s*-\s*(.+?)\s*", line)
        if match:
            patterns.append(match.group(1).strip().strip("'\""))
            continue
        if line.strip() and not line.lstrip().startswith("#"):
            in_tags = False
    return patterns


def _gh_lines(body: str, command: str) -> list[str]:
    """Return the lines of 'body' that run 'gh <command>'."""
    return [
        line
        for line in body.splitlines()
        if re.search(rf"\bgh\s+release\s+{re.escape(command)}\b", line)
    ]


def validate(text: str) -> list[Finding]:
    """Return every contract violation in one workflow file."""
    findings: list[Finding] = []

    if "android-*" in text:
        findings.append(
            (
                "no-android-tag",
                "the release workflow still names an 'android-*' tag; releases are v* only",
            )
        )

    on_block = _top_level_block(text, "on")
    if on_block is None:
        findings.append(("trigger-v", "the workflow has no top-level 'on:' trigger block"))
    else:
        tags = _tag_patterns(on_block)
        if "v*" not in tags:
            findings.append(
                (
                    "trigger-v",
                    f"the release workflow must trigger on 'v*' tags (found: {tags or 'none'})",
                )
            )
        extra = [tag for tag in tags if tag != "v*"]
        if extra:
            findings.append(
                (
                    "trigger-v",
                    f"the release workflow must trigger on 'v*' only (also found: {extra})",
                )
            )
        if not re.search(r"^\s+workflow_dispatch:\s*$", on_block, re.MULTILINE):
            findings.append(
                ("dispatch", "the release workflow must keep 'workflow_dispatch:' for manual runs")
            )
        if re.search(r"^\s+branches:\s*", on_block, re.MULTILINE):
            findings.append(
                (
                    "trigger-branches",
                    "the release workflow must not trigger on branch pushes",
                )
            )

    jobs = _job_blocks(text)
    for job, rule in (
        (GATE_JOB, "job-gate"),
        (LINUX_JOB, "job-linux"),
        (ANDROID_JOB, "job-android"),
        (RELEASE_JOB, "job-release"),
    ):
        if job not in jobs:
            findings.append((rule, f"the release workflow has no '{job}:' job"))

    gate = jobs.get(GATE_JOB)
    if gate is not None:
        if GATE_COMMAND not in gate:
            findings.append(
                (
                    "gate-command",
                    f"the '{GATE_JOB}' job must run '{GATE_COMMAND}'",
                )
            )
        if "astral-sh/setup-uv" not in gate:
            findings.append(
                ("gate-uv", f"the '{GATE_JOB}' job must install uv (astral-sh/setup-uv)")
            )
        if "dtolnay/rust-toolchain" not in gate:
            findings.append(
                ("gate-rust", f"the '{GATE_JOB}' job must install the Rust toolchain")
            )
        for package in ("libavcodec-dev", "pkg-config"):
            if package not in gate:
                findings.append(
                    (
                        "gate-deps",
                        f"the '{GATE_JOB}' job must install the Linux build dependency '{package}'",
                    )
                )

    linux = jobs.get(LINUX_JOB)
    if linux is not None:
        if GATE_JOB not in _needs(linux):
            findings.append(
                ("linux-needs-gate", f"the '{LINUX_JOB}' job must 'needs: {GATE_JOB}'")
            )
        if "cargo build --release -p app" not in linux:
            findings.append(
                (
                    "linux-build",
                    f"the '{LINUX_JOB}' job must run 'cargo build --release -p app'",
                )
            )
        package_lines = [
            line
            for line in linux.splitlines()
            if "tar " in line and "-c" in line
        ]
        packages_tarball = any(
            LINUX_TARBALL in line or "$tarball" in line for line in package_lines
        )
        if LINUX_TARBALL not in linux or not packages_tarball:
            findings.append(
                (
                    "linux-package",
                    f"the '{LINUX_JOB}' job must package '{LINUX_TARBALL}'",
                )
            )
        for item in ("target/release/krkr", "LICENSE", "README"):
            if item not in linux:
                findings.append(
                    (
                        "linux-contents",
                        f"the Linux archive must contain '{item}'",
                    )
                )
        if not (
            re.search(r"uses:\s*actions/upload-artifact", linux)
            and re.search(rf"name:\s*{re.escape(LINUX_ARTIFACT)}\b", linux)
        ):
            findings.append(
                (
                    "linux-artifact",
                    f"the '{LINUX_JOB}' job must upload the '{LINUX_ARTIFACT}' artifact",
                )
            )

    android = jobs.get(ANDROID_JOB)
    if android is not None:
        if GATE_JOB not in _needs(android):
            findings.append(
                ("android-needs-gate", f"the '{ANDROID_JOB}' job must 'needs: {GATE_JOB}'")
            )
        if "crates/android/build-android.sh" not in android:
            findings.append(
                (
                    "android-build",
                    f"the '{ANDROID_JOB}' job must invoke 'crates/android/build-android.sh'",
                )
            )
        if "crates/android/check-jni-libs.sh" not in android:
            findings.append(
                (
                    "android-jni-check",
                    f"the '{ANDROID_JOB}' job must invoke 'crates/android/check-jni-libs.sh'",
                )
            )
        if not (
            re.search(r"uses:\s*actions/upload-artifact", android)
            and re.search(rf"name:\s*{re.escape(ANDROID_ARTIFACT)}\b", android)
        ):
            findings.append(
                (
                    "android-artifact",
                    f"the '{ANDROID_JOB}' job must upload the '{ANDROID_ARTIFACT}' artifact",
                )
            )

    release = jobs.get(RELEASE_JOB)
    if release is not None:
        needed = _needs(release)
        if not {LINUX_JOB, ANDROID_JOB} <= needed:
            findings.append(
                (
                    "release-needs",
                    f"the '{RELEASE_JOB}' job must need both '{LINUX_JOB}' and '{ANDROID_JOB}'",
                )
            )
        for artifact in (LINUX_ARTIFACT, ANDROID_ARTIFACT):
            if not re.search(rf"name:\s*{re.escape(artifact)}\b", release):
                findings.append(
                    (
                        "release-assets",
                        f"the '{RELEASE_JOB}' job must download the '{artifact}' artifact",
                    )
                )
        creates = _gh_lines(release, "create")
        if not creates:
            findings.append(
                ("release-create", f"the '{RELEASE_JOB}' job must run 'gh release create'")
            )
        for line in creates:
            for asset in ("$linux", "$apk"):
                if asset not in line:
                    findings.append(
                        (
                            "release-assets",
                            f"'gh release create' must attach both artifacts ('{asset}' missing)",
                        )
                    )
        if not _gh_lines(release, "upload"):
            findings.append(
                (
                    "release-upload",
                    f"the '{RELEASE_JOB}' job must run 'gh release upload' for an existing release",
                )
            )

    publishers = [
        name
        for name, body in jobs.items()
        if _gh_lines(body, "create") or _gh_lines(body, "upload")
    ]
    if publishers != [RELEASE_JOB]:
        findings.append(
            (
                "release-single",
                f"exactly one job may publish a GitHub Release; found {publishers or 'none'}",
            )
        )

    return findings


def check_tree(root: Path) -> list[Finding]:
    """Validate the real workflows under 'root'."""
    workflow = root / ".github" / "workflows" / "release.yml"
    if not workflow.is_file():
        return [
            (
                "workflow-file",
                f"the release workflow is missing at {workflow.relative_to(root).as_posix()}",
            )
        ]

    findings: list[Finding] = list(validate(workflow.read_text(encoding="utf-8")))

    legacy = root / ".github" / "workflows" / "android.yml"
    if legacy.exists():
        findings.append(
            (
                "legacy-workflow",
                "the legacy Android-only workflow still exists; the v* release workflow replaces it",
            )
        )

    workflows = root / ".github" / "workflows"
    if workflows.is_dir():
        for path in sorted(workflows.glob("*.y*ml")):
            if path == workflow:
                continue
            text = path.read_text(encoding="utf-8")
            if "android-*" in text:
                findings.append(
                    (
                        "no-android-tag",
                        f"{path.name} still triggers on an 'android-*' tag",
                    )
                )
            if _gh_lines(text, "create") or _gh_lines(text, "upload"):
                findings.append(
                    (
                        "release-single",
                        f"{path.name} also publishes a GitHub Release; only {workflow.name} may",
                    )
                )

    # Preserve order while removing duplicates from the whole-tree scan.
    seen: set[Finding] = set()
    unique: list[Finding] = []
    for finding in findings:
        if finding not in seen:
            seen.add(finding)
            unique.append(finding)
    return unique


def self_test(verbose: bool = True) -> int:
    """Prove every rule fires on a mutated copy of the fixture workflow."""
    for path in (FIXTURE_WORKFLOW, FIXTURE_MUTATIONS):
        if not path.exists():
            print(f"check-ci: self-test fixture missing: {path}")
            return 1

    good = FIXTURE_WORKFLOW.read_text(encoding="utf-8")
    problems = validate(good)
    if problems:
        print("check-ci: self-test FAILED: the good fixture does not pass")
        for rule, message in problems:
            print(f"  {rule}: {message}")
        return 1

    mutations = json.loads(FIXTURE_MUTATIONS.read_text(encoding="utf-8"))
    if not isinstance(mutations, list) or not mutations:
        print("check-ci: self-test FAILED: mutations.json must be a non-empty list")
        return 1

    checked = 0
    for mutation in mutations:
        name = mutation.get("name", "<unnamed>")
        old = mutation.get("old")
        new = mutation.get("new")
        rule = mutation.get("rule")
        if not all(isinstance(value, str) for value in (name, old, new, rule)):
            print(f"check-ci: self-test FAILED: malformed mutation {mutation!r}")
            return 1
        if old not in good:
            print(f"check-ci: self-test FAILED: mutation {name!r} no longer matches the fixture")
            return 1
        broken = good.replace(old, new, 1)
        fired = {finding[0] for finding in validate(broken)}
        if rule not in fired:
            print(f"check-ci: self-test FAILED: mutation {name!r} did not fire {rule!r}")
            print(f"  fired: {sorted(fired)}")
            return 1
        checked += 1

    if verbose:
        print(f"check-ci: self-test ok ({checked} mutations)")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--self-test", action="store_true", help="run the fixture self-test and exit"
    )
    parser.add_argument("--quiet", action="store_true", help="only print failures")
    args = parser.parse_args(argv)

    selftest = self_test(verbose=not args.quiet)
    if args.self_test:
        return selftest
    if selftest != 0:
        return 1

    findings = check_tree(REPO_ROOT)
    if findings:
        for rule, message in findings:
            print(f"check-ci: {rule}: {message}", file=sys.stderr)
        print(
            "check-ci: the release workflow no longer matches the CI contract",
            file=sys.stderr,
        )
        return 1
    if not args.quiet:
        print(
            "check-ci: v* -> static gate -> Linux + Android -> one release publisher OK"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
