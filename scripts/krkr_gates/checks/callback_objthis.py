#!/usr/bin/env python3
"""Native callback receiver guard (AGENTS §0).

A native member that invokes a script callback must receive the caller's
``objthis``.  When the callback is an anonymous function (one whose closure
carries no receiver of its own) the reference falls back to the *caller's*
receiver, so a member registered through the plain static dispatch — whose
callback signature has no ``objthis`` — silently runs the callback with the
function object as ``this``.

The concrete instance was ``Scripts.foreach``: the reference forwards the
caller's receiver to the callback (``funcClosure.ObjThis == 0`` →
``functhis = objthis``, ``reference/cpp/plugins/scriptsEx.cpp:690-693``), but
the port registered it as a plain static and the callback resolved its members
against the function object.  A KAG title's ``KAGEnvironment.entryUpdateAll``
then failed with ``Member "entryUpdate" does not exist``.

This check scans ``crates/compat/src/natives/**`` for member implementations
that invoke a caller-supplied script callback and requires them to be
registered through an objthis/instance dispatch:

* a callback invocation is a call to ``call_detached`` /
  ``call_detached_with_objthis`` / ``call_value`` whose target is not an
  internally-evaluated TJS helper (a helper obtained from ``cached_helper`` /
  ``eval_retained`` / ``duplicate_retained`` is the port's own closure, not the
  caller's callback);
* a member "invokes" that callback when the invocation is reachable from the
  member's body through same-file helper functions;
* ``NativeObjThisMethodDef`` / ``NativeInstanceMethodDef`` registrations carry
  ``objthis``; a plain ``NativeMethodDef`` does not, so a callback-invoking
  member registered as one is a finding.

A callback-invoking member that legitimately does not need the caller's
receiver (it passes an explicit receiver, or invokes a value that carries its
own) is recorded, with a reason, in
``scripts/allowlists/callback_objthis_allow.txt``.

Usage:
    uv run --project scripts krkr-gate callback-objthis
    uv run --project scripts krkr-gate callback-objthis --self-test
    uv run --project scripts krkr-gate callback-objthis --print-all
    uv run --project scripts krkr-gate callback-objthis --quiet
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

from ..common import ALLOWLISTS_DIR, FIXTURES_DIR, REPO_ROOT
from ..framework import (
    filter_findings,
    find_block,
    mask,
    match_delimiter,
    print_all_findings,
    print_stale,
    read_allowlist_entries,
    stale_keys,
)

ROOT = REPO_ROOT
NATIVES = ROOT / "crates" / "compat" / "src" / "natives"
FIXTURES = FIXTURES_DIR / "callback_objthis"
ALLOW = ALLOWLISTS_DIR / "callback_objthis_allow.txt"
RULE = "callback-objthis"

BAD_FIXTURE = "bad_plain_static.rs"
GOOD_OBJTHIS_FIXTURE = "good_objthis_static.rs"
GOOD_PLAIN_FIXTURE = "good_non_callback_static.rs"

# A function definition. The body brace is located afterwards, so a generic
# `<...>` parameter list is handled.
_FN = re.compile(r"\bfn\s+(\w+)\s*[<(]")

# A script-callback invocation. `call_member` is deliberately excluded: it
# names an explicit receiver, so it never relies on the caller's `objthis`.
_CALLBACK = re.compile(r"\b(call_detached(?:_with_objthis)?|call_value)\s*\(")

# The value a callback invocation is applied to (the first argument).
_CALL_TARGET = re.compile(
    r"\b(?:call_detached(?:_with_objthis)?|call_value)\s*\(\s*&?\s*"
    r"([A-Za-z_][A-Za-z0-9_.]*)"
)

# A local binding whose initialiser provably builds the port's own TJS helper.
_INTERNAL_INIT = re.compile(r"\b(cached_helper|eval_retained|duplicate_retained)\s*\(")

# A member registration block. The definition type decides the dispatch:
# `NativeMethodDef` is the plain static callback (no `objthis`), the other two
# receive the receiver.
_DEF = re.compile(r"\bNative(MethodDef|ObjThisMethodDef|InstanceMethodDef)\s*\{")

# Identifiers called from a body; only locally-defined functions are edges.
_IDENT_CALL = re.compile(r"\b([a-z_][A-Za-z0-9_]*)\s*\(")

SAFE_KINDS = {"ObjThisMethodDef", "InstanceMethodDef"}


def _signature_end(text: str, fn_match: re.Match) -> int | None:
    """Return the offset of a function body's ``{``, or ``None``."""
    open_paren = text.find("(", fn_match.end() - 1)
    if open_paren == -1:
        return None
    close = match_delimiter(text, open_paren, "(", ")")
    if close < 0:
        return None
    brace = text.find("{", close)
    return brace if brace != -1 else None


def _parameter_names(text: str, fn_match: re.Match) -> set[str]:
    """Return the parameter names of the function whose header starts at ``fn_match``."""
    open_paren = text.find("(", fn_match.end() - 1)
    if open_paren == -1:
        return set()
    close = match_delimiter(text, open_paren, "(", ")")
    if close < 0:
        return set()
    names: set[str] = set()
    for part in text[open_paren + 1 : close].split(","):
        part = re.sub(r"^\s*mut\s+", "", part.strip())
        name = re.match(r"([A-Za-z_][A-Za-z0-9_]*)", part)
        if name:
            names.add(name.group(1))
    return names


def _definitions(masked: str):
    """Map function name -> list of ``(body_start, body_end, params)``."""
    definitions: dict[str, list[tuple[int, int, set[str]]]] = {}
    for fn_match in _FN.finditer(masked):
        brace = _signature_end(masked, fn_match)
        if brace is None:
            continue
        span = find_block(masked, brace)
        if span is None:
            continue
        start, end = span
        definitions.setdefault(fn_match.group(1), []).append(
            (start, end, _parameter_names(masked, fn_match))
        )
    return definitions


def _target_is_internal(body: str, target: str, params: set[str]) -> bool:
    """Whether a callback target is the port's own TJS helper, not a caller's.

    A field access (``handler.value``), a parameter, and a plain local are all
    potentially caller-supplied.  A local initialised from ``cached_helper`` /
    ``eval_retained`` / ``duplicate_retained`` is the port's own closure.
    """
    if "." in target or target in params:
        return False
    init = re.search(
        r"\blet\s+(?:mut\s+)?" + re.escape(target) + r"\s*=\s*([^;]*);", body
    )
    return bool(init and _INTERNAL_INIT.search(init.group(1)))


def _direct_callback_functions(definitions, masked: str) -> set[str]:
    """Functions whose body invokes a caller-supplied script callback."""
    direct: set[str] = set()
    for name, spans in definitions.items():
        for start, end, params in spans:
            body = masked[start:end]
            if not _CALLBACK.search(body):
                continue
            for target_match in _CALL_TARGET.finditer(body):
                if not _target_is_internal(body, target_match.group(1), params):
                    direct.add(name)
                    break
    return direct


def _reachable_callbacks(definitions, masked: str, direct: set[str]) -> set[str]:
    """Names of functions that reach a caller-callback invocation."""
    edges: dict[str, set[str]] = {}
    for name, spans in definitions.items():
        callees: set[str] = set()
        for start, end, _params in spans:
            for call in _IDENT_CALL.finditer(masked[start:end]):
                if call.group(1) in definitions:
                    callees.add(call.group(1))
        edges[name] = callees
    reach = set(direct)
    changed = True
    while changed:
        changed = False
        for name in definitions:
            if name in reach:
                continue
            if edges[name] & reach:
                reach.add(name)
                changed = True
    return reach


def _scan_source(path: Path, rel: str, reach: set[str]) -> list[dict[str, object]]:
    """Find plain-static members in one file that invoke a callback."""
    raw = path.read_text(encoding="utf-8", errors="replace")
    masked = mask(raw)
    findings: list[dict[str, object]] = []
    for def_match in _DEF.finditer(masked):
        span = find_block(masked, def_match.end() - 1)
        if span is None:
            continue
        start, end = span
        member = re.search(r'\bname\s*:\s*"([^"]+)"', raw[start:end])
        function = re.search(r"\bf\s*:\s*(\w+)", masked[start:end])
        if member is None or function is None:
            continue
        if function.group(1) not in reach:
            continue
        if def_match.group(1) in SAFE_KINDS:
            continue
        findings.append(
            {
                "file": rel,
                "line": masked.count("\n", 0, def_match.start()) + 1,
                "kind": def_match.group(1),
                "member": member.group(1),
                "func": function.group(1),
                "rule": RULE,
            }
        )
    return findings


def scan_paths(paths) -> list[dict[str, object]]:
    """Return findings for every ``.rs`` file under ``paths``."""
    findings: list[dict[str, object]] = []
    for path in paths:
        if path.is_file():
            files = [path]
        else:
            files = sorted(path.rglob("*.rs"))
        for file in files:
            masked = mask(file.read_text(encoding="utf-8", errors="replace"))
            definitions = _definitions(masked)
            direct = _direct_callback_functions(definitions, masked)
            reach = _reachable_callbacks(definitions, masked, direct)
            try:
                rel = file.relative_to(ROOT).as_posix()
            except ValueError:
                rel = file.as_posix()
            findings.extend(_scan_source(file, rel, reach))
    return findings


def scan(roots=None) -> list[dict[str, object]]:
    """Find every callback-invoking native member registered without ``objthis``."""
    if roots is None:
        roots = [NATIVES]
    return scan_paths(roots)


def load_allowlist() -> list[tuple[str, str, str]]:
    """Return ``(path, member, reason)`` allowlist entries."""
    entries: list[tuple[str, str, str]] = []
    for key, reason in read_allowlist_entries(ALLOW):
        path, _, member = key.rpartition(":")
        entries.append((path.strip(), member.strip(), reason))
    return entries


def entry_matches(entry, finding) -> bool:
    path, member, _reason = entry
    return path == finding["file"] and member == finding["member"]


def self_test() -> int:
    """The bad fixture must trip the rule; the near-misses must trip nothing."""
    if not FIXTURES.is_dir():
        print(f"ERROR: callback-objthis self-test fixtures missing at {FIXTURES}")
        return 1
    findings = scan([FIXTURES])
    bad = [f for f in findings if f["file"].endswith(BAD_FIXTURE)]
    if not bad:
        print(
            f"ERROR: callback-objthis self-test: {BAD_FIXTURE} matched no "
            f"`{RULE}` finding. Fix the rule; do not silence it."
        )
        return 1
    for fixture in (GOOD_OBJTHIS_FIXTURE, GOOD_PLAIN_FIXTURE):
        unexpected = [f for f in findings if f["file"].endswith(fixture)]
        if unexpected:
            print(
                f"ERROR: callback-objthis self-test: {fixture} unexpectedly "
                f"matched {unexpected[0]['kind']} {unexpected[0]['member']}; "
                "it must not be flagged."
            )
            return 1
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--self-test", action="store_true", help="run the fixture self-test and exit"
    )
    parser.add_argument(
        "--print-all",
        action="store_true",
        help="print every callback-invoking member, allowlist not applied",
    )
    parser.add_argument("--quiet", action="store_true", help="only print failures")
    args = parser.parse_args(argv)

    selftest = self_test()
    if args.self_test:
        return selftest
    if selftest != 0:
        return 1

    findings = scan()
    if args.print_all:
        print_all_findings(
            findings,
            lambda finding: (
                f"{finding['file']}:{finding['line']}:{RULE}: "
                f"{finding['kind']} {finding['member']} -> {finding['func']}"
            ),
        )
        return 0

    allow = load_allowlist()
    unallowed, used = filter_findings(
        findings, allow, entry_matches, first_match=True
    )

    for finding in unallowed:
        print(
            f"ERROR: native callback without a receiver [{RULE}] "
            f"{finding['file']}:{finding['line']}: "
            f"{finding['kind']} {finding['member']} -> {finding['func']}"
        )
    stale = stale_keys(allow, used, lambda entry: f"{entry[0]}:{entry[1]}")
    print_stale(stale)

    if unallowed:
        print("")
        print(
            f"ERROR: {len(unallowed)} callback-invoking native member(s) registered "
            f"without `objthis` ({len(findings)} site(s) checked, {len(allow)} allowlisted)."
        )
        print("  Register the member as a `NativeObjThisMethodDef` (an objthis")
        print("  member) or a `NativeInstanceMethodDef` (an instance member), or")
        print(f"  add a reviewed entry to {ALLOW.relative_to(ROOT)}")
        return 1

    if not args.quiet:
        print(
            f"callback-objthis guard: clean ({len(findings)} site(s) checked, "
            f"{len(allow)} allowlisted) — {ALLOW.relative_to(ROOT)}"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
