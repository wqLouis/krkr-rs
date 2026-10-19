#!/usr/bin/env python3
"""Lock the VM dispatch surface.

The surface the TJS VM dispatches against — which classes exist and which
members they expose — is the contract that must not change silently (D29).
Everything above it (L3 internals) may be refactored freely, because a
refactor that cannot change this dump cannot change how a call reaches a
native.

The dump is ground truth: `crates/compat/tests/runtime_surface.rs` asks the
*live* VM what `CompatHost::install` actually registered, rather than parsing
Rust source. This script compares it against the committed baseline
`docs/surface/runtime_surface.json` and fails on any difference:

  * members that appear  -> invented surface (the hallucinated-API case);
  * members that vanish  -> dispatch silently broken.

Usage:
    uv run --project scripts krkr-gate surface-lock             # regenerate + check
    uv run --project scripts krkr-gate surface-lock --no-regen   # check the existing dump
    uv run --project scripts krkr-gate surface-lock --update     # accept the current surface

An intentional surface change therefore has to be committed *with* a new
baseline in `docs/surface/`, where it is reviewable as a diff — that is the
hard constraint. A bare `--update` with an unexplained diff is a rejected
change, not a passing one.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import subprocess
import sys

from ..common import REPO_ROOT

ROOT = REPO_ROOT
BASELINE = ROOT / "docs" / "surface" / "runtime_surface.json"
DUMP = ROOT / "target" / "runtime_surface.json"


def load(path: pathlib.Path) -> dict[str, list[str]]:
    data = json.loads(path.read_text(encoding="utf-8"))
    classes = data.get("classes")
    if not isinstance(classes, dict):
        raise SystemExit(f"{path}: not a runtime surface dump (no 'classes')")
    out: dict[str, list[str]] = {}
    for name, members in sorted(classes.items()):
        if not isinstance(members, list):
            raise SystemExit(f"{path}: class {name!r} has a non-list member set")
        # The dump is a plain list of names today; tolerate {"name": ...}
        # entries so the lock keeps working if the dump ever carries kinds.
        names = sorted(
            {
                (m["name"] if isinstance(m, dict) else m)
                for m in members
            }
        )
        out[name] = names
    return out


def regenerate() -> None:
    print("surface_lock: regenerating the dump "
          "(cargo test -p compat --test runtime_surface)", file=sys.stderr)
    result = subprocess.run(
        ["cargo", "test", "-q", "-p", "compat", "--test", "runtime_surface"],
        cwd=ROOT,
    )
    if result.returncode != 0:
        raise SystemExit(
            f"surface_lock: runtime_surface test failed ({result.returncode}) — "
            "the surface could not be read, so it cannot be checked"
        )
    if not DUMP.exists():
        raise SystemExit(f"surface_lock: no dump written at {DUMP}")


def diff(
    old: dict[str, list[str]], new: dict[str, list[str]]
) -> tuple[dict[str, list[str]], dict[str, list[str]]]:
    added: dict[str, list[str]] = {}
    removed: dict[str, list[str]] = {}
    for cls in sorted(set(old) | set(new)):
        was, now = set(old.get(cls, [])), set(new.get(cls, []))
        if cls not in old:
            added[cls] = sorted(now)
        elif cls not in new:
            removed[cls] = sorted(was)
        else:
            if now - was:
                added[cls] = sorted(now - was)
            if was - now:
                removed[cls] = sorted(was - now)
    return added, removed


def members_total(surface: dict[str, list[str]]) -> int:
    return sum(len(v) for v in surface.values())


def self_test() -> int:
    """Check the class/member diff logic on a synthetic surface pair."""
    baseline = {"A": ["x"], "B": ["p", "q"]}
    current = {"A": ["x", "y"], "C": ["z"]}
    added, removed = diff(baseline, current)
    problems: list[str] = []
    if added != {"A": ["y"], "C": ["z"]}:
        problems.append(f"added {added} != {{'A': ['y'], 'C': ['z']}}")
    if removed != {"B": ["p", "q"]}:
        problems.append(f"removed {removed} != {{'B': ['p', 'q']}}")
    if problems:
        print("surface_lock: self-test FAILED:")
        for problem in problems:
            print(f"  {problem}")
        return 1
    print("surface_lock: self-test ok (class/member diff)")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--update",
        action="store_true",
        help="write the current dump as the new committed baseline",
    )
    parser.add_argument(
        "--no-regen",
        action="store_true",
        help="check the existing target/runtime_surface.json without "
        "re-running the dump test",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="run the class/member diff self-test and exit",
    )
    args = parser.parse_args(argv)

    if args.self_test:
        return self_test()

    if not args.no_regen:
        regenerate()
    if not DUMP.exists():
        raise SystemExit(
            f"surface_lock: {DUMP} does not exist — run without --no-regen first"
        )
    current = load(DUMP)

    if args.update:
        BASELINE.parent.mkdir(parents=True, exist_ok=True)
        BASELINE.write_text(
            json.dumps({"classes": current}, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        print(
            f"surface_lock: baseline updated — {len(current)} classes, "
            f"{members_total(current)} members -> {BASELINE.relative_to(ROOT)}"
        )
        return 0

    if not BASELINE.exists():
        raise SystemExit(
            "surface_lock: no baseline at "
            f"{BASELINE.relative_to(ROOT)} — create it once with --update"
        )
    baseline = load(BASELINE)
    added, removed = diff(baseline, current)

    if not added and not removed:
        print(
            f"surface_lock: OK — {len(current)} classes, "
            f"{members_total(current)} members, unchanged from baseline"
        )
        return 0

    print("surface_lock: FAILED — the VM dispatch surface changed.")
    if added:
        print("\n  surface ADDED (invented / unapproved API):")
        for cls, members in added.items():
            tag = "" if cls in baseline else "   [new class]"
            print(f"    {cls}:{tag}  {', '.join(members)}")
    if removed:
        print("\n  surface REMOVED (dispatch would silently break):")
        for cls, members in removed.items():
            print(f"    {cls}  {', '.join(members)}")
    print(
        "\n  L3 may be refactored freely, but this dump is the contract the VM\n"
        "  dispatches against (D29). If the change is intended and\n"
        "  reference-backed (D28), run\n"
        "      uv run --project scripts krkr-gate surface-lock --update\n"
        "  and commit docs/surface/runtime_surface.json in the same change,\n"
        "  with the reasoning in the commit message."
    )
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
