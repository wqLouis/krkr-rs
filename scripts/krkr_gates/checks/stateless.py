"""Static guard for process-global mutable state in the bottom-core crates.

Checks the bottom crates for process-global mutable state, hidden caches and
singletons, and checks the service objects for stateful (``&mut self``)
methods. The rules are the same literal patterns and service list as the
original shell guard.
"""

from __future__ import annotations

import re
from collections.abc import Iterable
from pathlib import Path

from ..common import REPO_ROOT

ROOT = REPO_ROOT

# Bottom-core crates. The middle layer (engine) owns state and is excluded.
CRATES = (
    "crates/platform/api",
    "crates/platform/fs",
    "crates/platform/media",
    "crates/platform/render",
    "crates/platform/platform",
)

# Forbidden patterns, matched against .rs sources.
PATTERNS = (
    re.compile(r"static\s+mut\s"),
    re.compile(r"thread_local!"),
    re.compile(r"lazy_static!"),
    re.compile(r"LazyLock"),
    re.compile(r"OnceLock"),
    re.compile(r"OnceCell"),
)

# Service objects that must be immutable after construction.
SERVICES = (
    ("crates/platform/render", "RenderPipeline"),
    ("crates/platform/fs", "FileSystem"),
    ("crates/platform/media", "Media"),
)

_IMPL = re.compile(r"^[ \t]*impl[ \t]")
_PUB_MUT_SELF = re.compile(r"pub fn[ \t]+[A-Za-z_0-9]+[ \t]*\(&mut self")
_CLOSE = re.compile(r"^}")


def _rust_sources(crate: str) -> Iterable[Path]:
    src = ROOT / crate / "src"
    if not src.is_dir():
        return ()
    return sorted(src.rglob("*.rs"))


def scan_globals() -> list[str]:
    findings: list[str] = []
    for crate in CRATES:
        for pattern in PATTERNS:
            for path in _rust_sources(crate):
                rel = path.relative_to(ROOT).as_posix()
                text = path.read_text(encoding="utf-8", errors="replace")
                for lineno, line in enumerate(text.splitlines(), 1):
                    if pattern.search(line):
                        findings.append(
                            f"D9 violation [{crate}]: {rel}:{lineno}:{line}"
                        )
    return findings


def scan_services() -> list[str]:
    findings: list[str] = []
    for crate, type_name in SERVICES:
        for path in _rust_sources(crate):
            rel = path.relative_to(ROOT).as_posix()
            in_impl = False
            for lineno, line in enumerate(
                path.read_text(encoding="utf-8", errors="replace").splitlines(), 1
            ):
                if _IMPL.match(line):
                    in_impl = type_name in line
                    continue
                if in_impl and _PUB_MUT_SELF.search(line):
                    findings.append(
                        f"D9 violation [{crate}]: stateful service method on "
                        f"{type_name}: {rel}:{lineno}:{line}"
                    )
                if in_impl and _CLOSE.match(line):
                    in_impl = False
    return findings


def main(argv: list[str] | None = None) -> int:
    if "--self-test" in (argv or []):
        print(
            "stateless guard: no fixture self-test "
            "(the rules are literal patterns matched against the tree)"
        )
        return 0
    findings = scan_globals() + scan_services()
    for finding in findings:
        print(finding)
    if findings:
        print("")
        print("ERROR: forbidden process-global state in the bottom core (D9).")
        print("Move the state into an explicit value owned by the middle layer.")
        return 1
    print(
        "D9 guard: clean "
        "(no globals/caches, no stateful services in the bottom core)"
    )
    return 0


def run(argv: list[str] | None = None) -> int:
    """Run the check. ``argv`` carries the check-specific flags."""
    return main(argv)
