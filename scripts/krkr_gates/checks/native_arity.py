#!/usr/bin/env python3
"""Native zero-argument arity guard.

A reference native method may define a zero-argument call as a real form, not a
bad-parameter error: ``Layer.setClip()`` resets the clip rectangle
(``reference/cpp/core/visual/LayerIntf.cpp:8641``), so with no arguments the
method succeeds while one through three arguments raise
``TJS_E_BADPARAMCOUNT``.  The port routes the call through a Rust callback; a
callback that rejects ``argc == 0`` with an arity guard and has no
zero-argument branch makes the valid call raise.

The concrete instance was ``Layer.setClip``: the reference reset the clip for
``numparams == 0`` and otherwise required four arguments
(``reference/cpp/core/visual/LayerIntf.cpp:8635-8658``); the port's
``layer_set_clip`` rejected every ``argc < 4``, so a script's ``setClip()``
raised ``"Layer.setClip requires 4 arguments"`` and aborted whatever called it.

The check

* scans the reference native methods for an explicit zero-argument path
  (``numparams == 0`` that is not itself a ``TJS_E_BADPARAMCOUNT`` return);
* maps each method to the Rust callback registered for it through the same
  class map the parity checker uses;
* fails when the callback rejects ``argc == 0`` with an arity guard and has no
  zero-argument branch;
* records reviewed divergences in
  ``scripts/allowlists/native_arity_allow.txt``.

Usage:
    uv run --project scripts krkr-gate native-arity
    uv run --project scripts krkr-gate native-arity --self-test
    uv run --project scripts krkr-gate native-arity --print-all
    uv run --project scripts krkr-gate native-arity --quiet
"""

from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass
from pathlib import Path

from ..common import (
    ALLOWLISTS_DIR,
    FIXTURES_DIR,
    REPO_ROOT,
    reference_core_sources,
    reference_source_label,
)
from ..framework import match_delimiter, read_allowlist_entries
from . import native_parity as parity

ROOT = REPO_ROOT
NATIVES = ROOT / "crates" / "compat" / "src" / "natives"
FIXTURES = FIXTURES_DIR / "native_arity"
ALLOW = ALLOWLISTS_DIR / "native_arity_allow.txt"
RULE = "native-arity"

BAD_FIXTURE = "bad_rust.rs"
GOOD_FIXTURE = "good_rust.rs"
REFERENCE_FIXTURE = "reference.cpp"

# A reference method body that accepts a zero-argument call.
_ZERO_REFERENCE_RE = re.compile(r"numparams\s*==\s*0")
# The same test spelled as a bad-parameter return is a rejection, not a path.
_ZERO_REJECT_REFERENCE_RE = re.compile(
    r"numparams\s*<\s*1\b[^;]*TJS_E_BADPARAMCOUNT", re.DOTALL
)

# A full method registration literal.
_METHOD_DEF_RE = re.compile(r"\bNative(?:Instance|ObjThis|Static)?MethodDef\s*\{")
# The layer submodules register methods through a `d("name", f)` shorthand.
_SHORTHAND_DEF_RE = re.compile(r"\bd\s*\(")

# An arity guard that rejects zero: a positive lower bound on the argument
# count, or the explicit `<= 0` spelling of "empty".
_REJECT_GUARD_RE = re.compile(
    r"\bargc\s*<\s*([1-9]\d*)"
    r"|\bargc\s*<=\s*0"
    r"|\b[A-Za-z_]\w*\.len\(\)\s*<\s*([1-9]\d*)"
    r"|require_args\s*\(\s*[^,]+,\s*([1-9]\d*)"
)
# A branch that tests for the zero-argument case.
_ZERO_TEST_RE = re.compile(
    r"if\s+(argc\s*==\s*0|argc\s*<=\s*0|[A-Za-z_]\w*\.is_empty\(\)"
    r"|[A-Za-z_]\w*\.len\(\)\s*==\s*0)\s*\{"
)
# A body that reports a bad-parameter error rather than handling the call.
_ERROR_RETURN_RE = re.compile(
    r"report_error\s*\(|return\s+Err\b|Err\s*\(|TJS_E_BADPARAMCOUNT"
)

HANDLED = "handled"
REJECTED = "rejected"
UNMAPPED = "unmapped"


@dataclass(frozen=True)
class ReferenceMethod:
    """A reference native method with an explicit zero-argument path."""

    cls: str
    member: str
    file: str
    line: int


@dataclass(frozen=True)
class Finding:
    """A reference zero-argument path whose Rust callback rejects zero."""

    reference: ReferenceMethod
    callback_file: str
    callback_line: int
    callback: str


def _match_pair(src: str, open_pos: int, open_ch: str, close_ch: str) -> int:
    """Return the index just past the ``close_ch`` matching ``open_ch``."""
    close = match_delimiter(src, open_pos, open_ch, close_ch)
    return len(src) if close < 0 else close + 1


def _cpp_class_spans(src: str) -> list[tuple[int, int, str]]:
    """``(start, end, class)`` spans for reference member declarations.

    The same attribution the parity checker uses: the enclosing
    ``TJS_BEGIN_NATIVE_MEMBERS(X)`` block (preferring its
    ``tTJSNativeClass(TJS_W("X"))`` constructor) and the
    ``TVPCreateNativeClass_X()`` function.
    """
    spans: list[tuple[int, int, str]] = []
    for m in parity._CPP_BLOCK_RE.finditer(src):
        cls = m.group(1)
        for cm in parity._CPP_CTOR_RE.finditer(src[: m.start()]):
            cls = cm.group(1)
        end_m = parity._CPP_END_MEMBERS_RE.search(src[m.end() :])
        end = m.end() + end_m.start() if end_m else len(src)
        spans.append((m.start(), end, cls))
    for m in parity._CPP_FUNC_RE.finditer(src):
        open_pos = src.index("{", m.start())
        spans.append((m.start(), parity._match_brace(src, open_pos), m.group(1)))
    return spans


def _enclosing_class(spans: list[tuple[int, int, str]], pos: int) -> "str | None":
    """The innermost class span containing ``pos``."""
    containing = [s for s in spans if s[0] < pos < s[1]]
    if not containing:
        return None
    return min(containing, key=lambda s: s[1] - s[0])[2]


def reference_methods_in(src: str, rel: str) -> list[ReferenceMethod]:
    """Reference native methods in one masked C++ source with a zero-arg path."""
    spans = _cpp_class_spans(src)
    out: list[ReferenceMethod] = []
    for m in parity._CPP_MEMBER_RE.finditer(src):
        if m.group(2) != "METHOD":
            continue
        cls = _enclosing_class(spans, m.start())
        if cls is None:
            continue
        end = parity._CPP_METHOD_END_RE.search(src, m.end())
        body = src[m.end() : end.start()] if end else src[m.end() :]
        if not _ZERO_REFERENCE_RE.search(body):
            continue
        if _ZERO_REJECT_REFERENCE_RE.search(body):
            continue
        zero = _ZERO_REFERENCE_RE.search(body)
        assert zero is not None
        line = src.count("\n", 0, m.end() + zero.start()) + 1
        out.append(ReferenceMethod(cls, m.group(3), rel, line))
    return out


def parse_reference_methods(
    files: "list[Path] | None" = None,
) -> list[ReferenceMethod]:
    """Reference zero-argument methods from the core sources (or ``files``)."""
    if files is None:
        files = reference_core_sources()
    methods: list[ReferenceMethod] = []
    for path in files:
        raw = path.read_text(encoding="utf-8", errors="replace")
        src = parity._mask_cpp_source(raw)
        methods.extend(reference_methods_in(src, reference_source_label(path)))
    return methods


def _name_in(
    code: str, strings: dict[int, tuple[int, str, int]], start: int, end: int
) -> "str | None":
    """The first ``name: "literal"`` value inside ``[start, end)``."""
    for off, (_e, content, _q) in strings.items():
        if start <= off < end and re.search(r"\bname\s*:\s*$", code[max(0, off - 48) : off]):
            return content
    return None


def method_defs_in(
    code: str,
    strings: dict[int, tuple[int, str, int]],
    start: int,
    end: int,
) -> list[tuple[str, str]]:
    """``(member, callback)`` for method registrations in ``[start, end)``.

    Handles both the struct literal ``Native*MethodDef { name: "x", f: cb }``
    and the ``d("x", cb)`` shorthand the layer submodules use.  Property
    registrations are not methods and are ignored.
    """
    defs: list[tuple[str, str]] = []
    for m in _METHOD_DEF_RE.finditer(code):
        if not (start <= m.start() < end):
            continue
        open_pos = code.index("{", m.end() - 1)
        close = parity._match_brace(code, open_pos)
        body = code[open_pos:close]
        member = _name_in(code, strings, open_pos, close)
        if member is None:
            continue
        cb = re.search(r"\bf\s*:\s*([A-Za-z_]\w*)", body)
        if cb is not None:
            defs.append((member, cb.group(1)))
    for m in _SHORTHAND_DEF_RE.finditer(code):
        if not (start <= m.start() < end):
            continue
        open_pos = m.end() - 1
        close = _match_pair(code, open_pos, "(", ")")
        candidates = [off for off in strings if open_pos < off < close]
        if not candidates:
            continue
        off = min(candidates)
        member = strings[off][1]
        after = strings[off][0]
        cb = re.match(r"\s*,\s*([A-Za-z_]\w*)", code[after:close])
        if cb is not None:
            defs.append((member, cb.group(1)))
    return defs


def registrations_in_file(
    path: Path, builder: "str | None" = None
) -> list[tuple[str, str]]:
    """Method ``(member, callback)`` registrations in one Rust file.

    ``builder`` restricts the scan to a single ``Native*Builder`` /
    ``Native*Members`` block, for a file that registers more than one class.
    """
    raw = path.read_text(encoding="utf-8", errors="replace")
    code, strings = parity._lex_rust(raw)
    if builder is None:
        return method_defs_in(code, strings, 0, len(code))
    defs: list[tuple[str, str]] = []
    for start, end, cls, _lit, _static in parity._builder_spans_with_kind(
        code, strings, raw
    ):
        if cls == builder:
            defs.extend(method_defs_in(code, strings, start, end))
    return defs


def callback_index() -> dict[str, list[tuple[str, int, str]]]:
    """Map callback function name -> ``(file, line, body)`` for every native fn."""
    index: dict[str, list[tuple[str, int, str]]] = {}
    if not NATIVES.is_dir():
        return index
    for path in sorted(NATIVES.rglob("*.rs")):
        raw = path.read_text(encoding="utf-8", errors="replace")
        code, _strings = parity._lex_rust(raw)
        try:
            rel = path.relative_to(ROOT).as_posix()
        except ValueError:
            rel = path.as_posix()
        for m in re.finditer(r"\bfn\s+([A-Za-z_]\w*)\s*[<(]", code):
            name = m.group(1)
            rng = parity._function_range(code, name)
            if rng is None:
                continue
            body = code[rng[0] : rng[1]]
            line = raw.count("\n", 0, m.start()) + 1
            index.setdefault(name, []).append((rel, line, body))
    return index


def callback_disposition(body: str) -> str:
    """``handled``, ``rejected`` or ``none`` for one callback body."""
    handles = False
    rejects = False
    for zero in _ZERO_TEST_RE.finditer(body):
        block = body[zero.end() - 1 :]
        block = block[: _match_pair(block, 0, "{", "}")]
        if _ERROR_RETURN_RE.search(block):
            rejects = True
        else:
            handles = True
    if _REJECT_GUARD_RE.search(body):
        rejects = True
    if handles:
        return HANDLED
    if rejects:
        return REJECTED
    return "none"


def rust_callback_for(
    cls: str, member: str
) -> "tuple[str, str] | None":
    """``(callback, file)`` registered for ``cls.member``, or ``None``."""
    for rel, builder in parity.RUST_CLASS_MAP.get(cls, []):
        path = ROOT / rel
        if not path.is_file():
            continue
        for name, callback in registrations_in_file(path, builder):
            if name == member:
                return callback, rel
    return None


def evaluate() -> tuple[list[Finding], list[tuple[ReferenceMethod, str]]]:
    """Findings plus the ``(method, disposition)`` of every reference method."""
    index = callback_index()
    findings: list[Finding] = []
    evaluated: list[tuple[ReferenceMethod, str]] = []
    for method in parse_reference_methods():
        if method.cls not in parity.RUST_CLASS_MAP:
            evaluated.append((method, UNMAPPED))
            continue
        mapped = rust_callback_for(method.cls, method.member)
        if mapped is None:
            evaluated.append((method, UNMAPPED))
            continue
        callback, _rel = mapped
        bodies = index.get(callback, [])
        dispositions = [callback_disposition(body) for _f, _l, body in bodies]
        if HANDLED in dispositions or not bodies:
            evaluated.append((method, HANDLED if bodies else UNMAPPED))
            continue
        if REJECTED in dispositions:
            file, line, _body = bodies[0]
            findings.append(Finding(method, file, line, callback))
            evaluated.append((method, REJECTED))
        else:
            evaluated.append((method, "none"))
    return findings, evaluated


def load_allowlist() -> list[tuple[str, str, str]]:
    """Return ``(class, member, reason)`` allowlist entries."""
    entries: list[tuple[str, str, str]] = []
    for key, reason in read_allowlist_entries(ALLOW):
        cls, _, member = key.rpartition(".")
        entries.append((cls.strip(), member.strip(), reason))
    return entries


def entry_matches(entry: tuple[str, str, str], finding: Finding) -> bool:
    cls, member, _reason = entry
    return cls == finding.reference.cls and member == finding.reference.member


def self_test() -> int:
    """The bad fixture must trip the rule; the good one must not."""
    if not FIXTURES.is_dir():
        print(f"ERROR: native-arity self-test fixtures missing at {FIXTURES}")
        return 1
    reference = parse_reference_methods([FIXTURES / REFERENCE_FIXTURE])
    if not any(m.cls == "Layer" and m.member == "setClip" for m in reference):
        print(
            f"ERROR: native-arity self-test: {REFERENCE_FIXTURE} must describe "
            "`Layer.setClip` with a zero-argument path"
        )
        return 1
    if any(m.member == "setSize" for m in reference):
        print(
            f"ERROR: native-arity self-test: {REFERENCE_FIXTURE} must not treat "
            "a `numparams < 2` bad-parameter return as a zero-argument path"
        )
        return 1

    def disposition(fixture: str) -> str:
        path = FIXTURES / fixture
        raw = path.read_text(encoding="utf-8")
        code, _strings = parity._lex_rust(raw)
        bodies: list[str] = []
        for _member, callback in registrations_in_file(path):
            rng = parity._function_range(code, callback)
            if rng is not None:
                bodies.append(code[rng[0] : rng[1]])
        if any(callback_disposition(b) == REJECTED for b in bodies):
            return REJECTED
        if any(callback_disposition(b) == HANDLED for b in bodies):
            return HANDLED
        return "none"

    if disposition(BAD_FIXTURE) != REJECTED:
        print(
            f"ERROR: native-arity self-test: {BAD_FIXTURE} must reject `argc 0` "
            f"and be flagged. Fix the rule; do not silence it."
        )
        return 1
    if disposition(GOOD_FIXTURE) != HANDLED:
        print(
            f"ERROR: native-arity self-test: {GOOD_FIXTURE} must handle `argc 0` "
            "and not be flagged."
        )
        return 1
    return 0


def main(argv: "list[str] | None" = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--self-test", action="store_true", help="run the fixture self-test and exit"
    )
    parser.add_argument(
        "--print-all",
        action="store_true",
        help="print every reference zero-argument method and its disposition",
    )
    parser.add_argument("--quiet", action="store_true", help="only print failures")
    args = parser.parse_args(argv)

    selftest = self_test()
    if args.self_test:
        return selftest
    if selftest != 0:
        return 1

    findings, evaluated = evaluate()
    if args.print_all:
        for method, disposition in evaluated:
            location = f"{method.file}:{method.line}" if method.file else "?"
            print(
                f"{RULE}: {method.cls}.{method.member} "
                f"[{disposition}] {location}"
            )
        print(f"\n{len(findings)} finding(s) total (allowlist not applied)")
        return 0

    allow = load_allowlist()
    used = [False] * len(allow)
    unallowed: list[Finding] = []
    for finding in findings:
        matched = False
        for i, entry in enumerate(allow):
            if entry_matches(entry, finding):
                used[i] = True
                matched = True
                break
        if not matched:
            unallowed.append(finding)

    for finding in unallowed:
        reference = finding.reference
        print(
            f"ERROR: native arity [{RULE}] {reference.cls}.{reference.member}: "
            f"the reference accepts zero arguments ({reference.file}:"
            f"{reference.line}) but the Rust callback {finding.callback} rejects "
            f"`argc == 0` ({finding.callback_file}:{finding.callback_line})"
        )
    stale = [
        f"{allow[i][0]}.{allow[i][1]}"
        for i in range(len(allow))
        if not used[i]
    ]
    if stale:
        print("")
        print(f"note: {len(stale)} allowlist entr(y/ies) no longer match (stale):")
        for key in sorted(stale):
            print(f"  {key}")

    if unallowed:
        print("")
        print(
            f"ERROR: {len(unallowed)} reference zero-argument method(s) rejected by "
            f"their Rust callback ({len(findings)} finding(s), {len(allow)} allowlisted)."
        )
        print(
            "  Handle `argc == 0` in the callback (return the reference's "
            "zero-argument form) before the arity guard, or add a reviewed entry "
            f"to {ALLOW.relative_to(ROOT)}"
        )
        return 1

    if not args.quiet:
        print(
            f"native-arity guard: clean ({len(findings)} finding(s), "
            f"{len(allow)} allowlisted) — {ALLOW.relative_to(ROOT)}"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
