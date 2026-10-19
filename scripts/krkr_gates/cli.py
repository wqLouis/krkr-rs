"""Command-line entry point for the krkr-rs gate checks.

Usage::

    uv run --project scripts krkr-gate <check> [check options]

``<check>`` is one of ``stateless``, ``error-handling``, ``house-style``,
``layering``, ``host-facts``, ``android-target``, ``surface-lock``,
``native-parity``, ``plugin-parity``, ``callback-objthis``, ``native-arity``,
or the ``extract-xp3`` helper. ``gate`` runs the full ordered sequence; its
``cargo`` steps are for the main agent.
"""

from __future__ import annotations

import contextlib
import io
import subprocess
import sys
from collections.abc import Callable

from .checks import (
    android_target,
    callback_objthis,
    error_handling,
    extract_xp3,
    host_facts,
    house_style,
    layering,
    lifecycle_order,
    native_arity,
    native_construction,
    native_parity,
    plugin_parity,
    stateless,
    storage_boundary,
    storage_media,
    surface_lock,
)
from .common import REPO_ROOT

# Check subcommands that run without a compiler. ``android-target`` shells out
# to ``cargo tree`` (not a build); the rest are pure file scans.
CHECKS: dict[str, Callable[[list[str]], int]] = {
    "stateless": stateless.main,
    "error-handling": error_handling.main,
    "house-style": house_style.main,
    "layering": layering.main,
    "storage-boundary": storage_boundary.main,
    "native-construction": native_construction.main,
    "lifecycle-order": lifecycle_order.main,
    "storage-media": storage_media.main,
    "callback-objthis": callback_objthis.main,
    "native-arity": native_arity.main,
    "host-facts": host_facts.main,
    "android-target": android_target.main,
    "surface-lock": surface_lock.main,
    "native-parity": native_parity.main,
    "plugin-parity": plugin_parity.main,
}

# The gate sequence in the AGENTS §0 order. Each step is
# ``(label, kind, name, argv)`` where ``kind`` is ``"cargo"`` or ``"check"``.
GATE_SEQUENCE: list[tuple[str, str, str, list[str]]] = [
    ("cargo fmt", "cargo", "cargo", ["cargo", "fmt", "--all", "--", "--check"]),
    ("stateless", "check", "stateless", []),
    ("error-handling", "check", "error-handling", []),
    ("house-style", "check", "house-style", []),
    ("layering", "check", "layering", []),
    ("storage-boundary", "check", "storage-boundary", []),
    ("native-construction", "check", "native-construction", []),
    ("lifecycle-order", "check", "lifecycle-order", []),
    ("storage-media", "check", "storage-media", []),
    ("callback-objthis", "check", "callback-objthis", []),
    ("native-arity", "check", "native-arity", []),
    (
        "cargo clippy",
        "cargo",
        "cargo",
        ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"],
    ),
    (
        "cargo clippy platform-android",
        "cargo",
        "cargo",
        [
            "cargo",
            "clippy",
            "-p",
            "platform",
            "--features",
            "platform-android",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
    ),
    ("android-target", "check", "android-target", []),
    ("surface-lock --update", "check", "surface-lock", ["--update"]),
    ("surface-lock", "check", "surface-lock", []),
    ("native-parity", "check", "native-parity", []),
    ("plugin-parity", "check", "plugin-parity", ["--no-doc"]),
    ("host-facts", "check", "host-facts", []),
]

# The workspace and per-target test suites. These are deliberately **not** part
# of ``GATE_SEQUENCE``: the guard script is a fast static check, and the test
# suites are run separately with ``krkr-gate test``.
TEST_SEQUENCE: list[tuple[str, list[str]]] = [
    ("cargo test", ["cargo", "test", "--workspace", "--no-fail-fast"]),
    (
        "cargo test platform-linux",
        ["cargo", "test", "-p", "platform", "--features", "platform-linux"],
    ),
    (
        "cargo test platform-android",
        ["cargo", "test", "-p", "platform", "--features", "platform-android"],
    ),
]

USAGE = """krkr-gate <check> [options]

Checks:
  stateless        process-global state in the bottom-core crates
  error-handling   silent fallbacks and dropped errors (AGENTS §4)
  house-style      bean accessors, parameter shapes, comments (AGENTS §5/§6)
  layering         layer vocabulary and unsafe placement (AGENTS §2/§5)
  storage-boundary script storage names that reach L2 or the host unstripped
  native-construction native instances left unconstructed at create time
  lifecycle-order  Scene removal without child-finalizer ordering
  storage-media    reference storage-media domains neither served nor documented
  callback-objthis  callback-invoking native members registered without objthis
  native-arity     reference zero-argument native methods rejected by a Rust guard
  host-facts       host seams with no production installer
  android-target   per-target backend-feature resolution
  surface-lock     VM dispatch surface vs the committed baseline
  native-parity    native class surface vs the reference
  plugin-parity    plugin surface vs the reference
  extract-xp3      extract a file from an XP3 archive
  gate             the ordered static gate (fmt, guards, clippy)
                   --no-cargo   run only the compiler-free checks
                   --keep-going do not stop at the first failing step
  test             the workspace and platform test suites

Global options:
  --quiet          only print failing checks
  --verbose        print all check output (default)
"""


def _run_callable(
    func: Callable[[list[str]], int], argv: list[str]
) -> tuple[int, str]:
    """Run an in-process check, capturing its stdout and stderr."""
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        try:
            code = func(list(argv))
        except SystemExit as exc:
            code = exc.code if isinstance(exc.code, int) else 1
    return code, out.getvalue() + err.getvalue()


def _emit(output: str) -> None:
    if output:
        sys.stdout.write(output)
        if not output.endswith("\n"):
            sys.stdout.write("\n")


def run_one(name: str, argv: list[str], quiet: bool) -> int:
    """Run one check subcommand and print its status line."""
    code, output = _run_callable(CHECKS[name], argv)
    if code != 0:
        _emit(output)
        print(f"krkr-gate: FAIL {name}")
        return code
    if not quiet:
        _emit(output)
        print(f"krkr-gate: PASS {name}")
    return 0


def _run_cargo(argv: list[str], quiet: bool) -> int:
    if quiet:
        result = subprocess.run(argv, cwd=REPO_ROOT, capture_output=True, text=True)
        if result.returncode != 0:
            sys.stdout.write(result.stdout)
            sys.stderr.write(result.stderr)
        return result.returncode
    return subprocess.run(argv, cwd=REPO_ROOT).returncode


def run_gate(
    argv: list[str], quiet: bool, verbose: bool
) -> int:
    no_cargo = "--no-cargo" in argv
    keep_going = "--keep-going" in argv
    failures: list[str] = []
    for label, kind, name, step_argv in GATE_SEQUENCE:
        if no_cargo and kind == "cargo":
            continue
        if no_cargo and name == "surface-lock" and "--update" in step_argv:
            continue
        effective = list(step_argv)
        if no_cargo and name == "surface-lock":
            effective = ["--no-regen"]
        if kind == "cargo":
            code = _run_cargo(effective, quiet)
        else:
            code, output = _run_callable(CHECKS[name], effective)
            if code != 0 or not quiet:
                _emit(output)
        if code != 0:
            failures.append(label)
            print(f"krkr-gate: FAIL {label}")
            if not keep_going:
                break
        elif not quiet:
            print(f"krkr-gate: PASS {label}")
    if failures:
        print(f"krkr-gate: gate FAILED ({', '.join(failures)})")
        return 1
    if not quiet:
        print("krkr-gate: gate clean")
    return 0


def run_tests(quiet: bool) -> int:
    failures: list[str] = []
    for label, step_argv in TEST_SEQUENCE:
        code = _run_cargo(step_argv, quiet)
        if code != 0:
            failures.append(label)
            print(f"krkr-gate: FAIL {label}")
        elif not quiet:
            print(f"krkr-gate: PASS {label}")
    if failures:
        print(f"krkr-gate: tests FAILED ({', '.join(failures)})")
        return 1
    if not quiet:
        print("krkr-gate: tests clean")
    return 0


def main(argv: list[str] | None = None) -> int:
    args = list(argv if argv is not None else sys.argv[1:])
    quiet = "--quiet" in args
    verbose = "--verbose" in args
    args = [a for a in args if a not in ("--quiet", "--verbose")]

    if not args or args[0] in ("-h", "--help", "help"):
        print(USAGE)
        return 0
    if args[0] in ("-V", "--version"):
        from . import __version__

        print(f"krkr-gate {__version__}")
        return 0
    if args[0] == "list":
        for name in CHECKS:
            print(name)
        return 0

    command, rest = args[0], args[1:]
    if command == "gate":
        return run_gate(rest, quiet=quiet, verbose=verbose)
    if command == "test":
        return run_tests(quiet=quiet)
    if command == "extract-xp3":
        return extract_xp3.main(rest)
    if command in CHECKS:
        return run_one(command, rest, quiet=quiet)

    print(f"krkr-gate: unknown check {command!r}", file=sys.stderr)
    print(USAGE, file=sys.stderr)
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
