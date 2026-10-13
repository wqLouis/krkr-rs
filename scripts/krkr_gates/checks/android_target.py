#!/usr/bin/env python3
"""Resolve each composition root per target and reject a backend-feature leak.

Guards: a platform/video backend selected unconditionally is invisible until
something builds the other way — the workspace still resolves, but the Android
build links FFmpeg and the Linux window/audio backend while the desktop build
pulls the NDK. This resolves the dependency graph of each root package for the
host and for `aarch64-linux-android` and asserts the feature set each target
actually gets. It compiles nothing and needs no NDK, so it runs in the gate on
the development host.

Why this drives `cargo tree` and not `cargo metadata --filter-platform`:
``cargo metadata`` computes each package's features as a union over *all*
workspace members and does not filter that union by target, so it reports both
``platform-linux`` and ``platform-android`` for both targets even when the
manifests are correct; its ``packages`` list also names optional dependencies
(such as ``cpal``) that no active feature enables. ``cargo tree -p <root>
--target <triple>`` resolves exactly the build graph the compiler would see,
including per-target features, which is what this guard must check.

Usage:
    uv run --project scripts krkr-gate android-target [--quiet]

Exit status is non-zero on any leak.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

from ..common import REPO_ROOT
ANDROID_TRIPLE = "aarch64-linux-android"

# Roots whose resolved graph is checked against the Android target: the Android
# composition root (`android`), the desktop root it wraps (`app`), and `compat`,
# whose integration tests link `media` as a dev-dependency.
ANDROID_ROOTS = ["android", "app", "compat"]
# Roots checked against the host target. `android` is excluded: on the desktop
# its Android-only dependencies are absent by construction, so it has no
# desktop backend to assert.
HOST_ROOTS = ["app", "compat"]

# Backend features that must never be active together for one target.
EXCLUSIVE = (
    ("platform", "platform-linux", "platform-android"),
    ("media", "video-ffmpeg", "video-mediacodec"),
)

# (package, feature) pairs that must / must not resolve per target.
ANDROID_PRESENT = (("platform", "platform-android"), ("media", "video-mediacodec"))
ANDROID_ABSENT = (("platform", "platform-linux"), ("media", "video-ffmpeg"))
HOST_PRESENT = (("platform", "platform-linux"), ("media", "video-ffmpeg"))
HOST_ABSENT = (("platform", "platform-android"), ("media", "video-mediacodec"))


def _cargo(cmd: list[str]) -> str:
    """Run a cargo subcommand in the repo, failing loudly on any error."""
    result = subprocess.run(cmd, cwd=REPO_ROOT, capture_output=True, text=True)
    if result.returncode != 0:
        sys.stderr.write(
            f"check-android-target: `{' '.join(cmd)}` failed:\n{result.stderr.strip()}\n"
        )
        raise SystemExit(2)
    return result.stdout


def host_triple() -> str:
    """Return the host target triple (querying cargo, never building)."""
    for cmd in (["cargo", "-vV"], ["rustc", "-vV"]):
        try:
            out = subprocess.run(cmd, capture_output=True, text=True).stdout
        except FileNotFoundError:
            continue
        match = re.search(r"^host:\s*(\S+)", out, re.MULTILINE)
        if match:
            return match.group(1)
    sys.stderr.write("check-android-target: cannot determine the host target triple\n")
    raise SystemExit(2)


def _parse_tree_output(out: str) -> dict[str, set[str]]:
    """Union the ``pkg|features`` lines printed by ``cargo tree``."""
    features: dict[str, set[str]] = {}
    for line in out.splitlines():
        spec, sep, raw = line.partition("|")
        if not sep:
            continue
        name = spec.split()[0]
        if not name:
            continue
        raw = raw.removesuffix("(*)").strip()
        enabled = {f for f in raw.split(",") if f}
        features.setdefault(name, set()).update(enabled)
    return features


def resolved_features(root: str, triple: str) -> dict[str, set[str]]:
    """Return ``{package: {features}}`` for ``root``'s graph on ``triple``.

    ``cargo tree --format '{p}|{f}'`` prints one line per resolved package
    (repeated when a package carries different features along different
    paths), so the sets are unioned. ``-e normal,dev`` keeps every normal
    dependency plus the selected root's dev-dependencies — the graph
    ``cargo test -p <root>`` builds — while dropping build edges and the
    dev-dependencies of transitive normal dependencies, which a test of the
    root never compiles. Dropping dev edges entirely would hide a
    dev-dependency that pulls the wrong codec default.
    """
    out = _cargo(
        [
            "cargo",
            "tree",
            "-p",
            root,
            "--target",
            triple,
            "-e",
            "normal,dev",
            "--prefix",
            "none",
            "--format",
            "{p}|{f}",
        ]
    )
    return _parse_tree_output(out)


def check(
    label: str,
    root: str,
    triple: str,
    present: tuple[tuple[str, str], ...],
    absent: tuple[tuple[str, str], ...],
    quiet: bool,
) -> list[str]:
    """Check one (root, target) pair; return a list of failure strings."""
    features = resolved_features(root, triple)
    failures: list[str] = []
    for package, feature in present:
        if feature not in features.get(package, set()):
            failures.append(
                f"{package} does not resolve with `{feature}` for {label} ({triple})"
            )
    for package, feature in absent:
        if feature in features.get(package, set()):
            failures.append(
                f"{package} resolves with `{feature}` for {label} ({triple})"
            )
    for package, first, second in EXCLUSIVE:
        enabled = features.get(package, set())
        if first in enabled and second in enabled:
            failures.append(
                f"{package} enables mutually exclusive `{first}` and `{second}` "
                f"for {label} ({triple})"
            )
    if not quiet:
        print(
            f"check-android-target: {label} ({triple}) "
            f"platform={{{','.join(sorted(features.get('platform', set())))}}} "
            f"media={{{','.join(sorted(features.get('media', set())))}}}"
        )
    return failures


def self_test() -> int:
    """Check the ``cargo tree`` line parser on a synthetic graph."""
    sample = (
        "platform v0.1.0 (path+file:///x)|default,platform-android\n"
        "media v0.1.0 (path+file:///y)|default,video-mediacodec,video-mediacodec\n"
        "ignored line without a separator\n"
    )
    got = _parse_tree_output(sample)
    want = {
        "platform": {"default", "platform-android"},
        "media": {"default", "video-mediacodec"},
    }
    if got != want:
        print("check-android-target: self-test FAILED:")
        print(f"  parsed {got} != {want}")
        return 1
    print("check-android-target: self-test ok (cargo tree parser)")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--quiet", action="store_true", help="only print failures"
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="run the cargo-tree parser self-test and exit",
    )
    args = parser.parse_args(argv)

    if args.self_test:
        return self_test()

    host = host_triple()
    failures: list[str] = []
    for root in ANDROID_ROOTS:
        failures += check(
            root, root, ANDROID_TRIPLE, ANDROID_PRESENT, ANDROID_ABSENT, args.quiet
        )
    for root in HOST_ROOTS:
        failures += check(
            f"desktop {root}", root, host, HOST_PRESENT, HOST_ABSENT, args.quiet
        )

    if failures:
        for failure in failures:
            print(f"check-android-target: {failure}", file=sys.stderr)
        print(
            "check-android-target: a backend feature leaked across targets; "
            "scope it with [target.'cfg(target_os = \"...\")'.dependencies].",
            file=sys.stderr,
        )
        return 1
    if not args.quiet:
        print("check-android-target: per-target backend features OK")
    return 0


def run(argv=None):
    """Run the check. ``argv`` carries the check-specific flags."""
    return main(argv)


if __name__ == "__main__":
    raise SystemExit(run(sys.argv[1:]))
