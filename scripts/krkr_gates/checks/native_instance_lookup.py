#!/usr/bin/env python3
"""Native-instance class lookup vs dispatch guard.

NativeInstanceSupport(TJS_NIS_GETINSTANCE, classid) resolves an
invalidated-but-alive native instance by class: Invalidate releases the
instance's resources, but the class id stays registered until Destruct. An
argument lookup such as Layer.assignImages must therefore still recognize the
object, while a direct method/property/constructor dispatch must keep
rejecting a finalized instance.

crates/tjs/cpp/tjs2_abi.cpp has two accessors for exactly that split:
GetNativePtrForLookup() is the valid-only class lookup, and GetNativePtr() is
the invalidated-guarded dispatch accessor. This check enforces:

* tjs2_get_native_instance calls GetNativePtrForLookup(), never GetNativePtr();
* GetNativePtrForLookup() is the valid-only lookup (it must not grow the
  valid && !invalidated dispatch guard);
* GetNativePtr() keeps the invalidation guard;
* the four instance dispatch bodies (method, property get, property set and
  constructor) still call GetNativePtr() and never the lookup accessor.

The fixture self-test applies four mutations to one good fixture and requires
the matching rule to fire. The scan is deliberately lexical: it follows the
named functions in one file and does not preprocess C++ or follow an alias or
helper call.
"""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass
from pathlib import Path

from ..common import FIXTURES_DIR, REPO_ROOT
from ..framework import Finding, dedupe_findings, find_block, mask, normalize

ROOT = REPO_ROOT
TARGET = ROOT / "crates" / "tjs" / "cpp" / "tjs2_abi.cpp"
FIXTURES = FIXTURES_DIR / "native_instance_lookup"
GOOD_FIXTURE = "scripts/fixtures/native_instance_lookup/good_abi.cpp"

RULE_LOOKUP = "instance-lookup"
RULE_LOOKUP_GUARD = "instance-lookup-guard"
RULE_DISPATCH = "instance-dispatch"
RULE_DISPATCH_GUARD = "instance-dispatch-guard"

LOOKUP_FN = "tjs2_get_native_instance"
LOOKUP_ACCESSOR = "GetNativePtrForLookup"
DISPATCH_ACCESSOR = "GetNativePtr"
DISPATCH_FNS = (
    "tjs2_dispatch_native_instance_method",
    "tjs2_dispatch_native_instance_property_get",
    "tjs2_dispatch_native_instance_property_set",
    "tjs2_native_instance_constructor_dispatch::FuncCall",
)

# A function definition whose return type is void * or tjs_error. Parameter
# lists in this file contain no nested parentheses.
FUNCTION = re.compile(
    r"(?:void\s*\*|tjs_error)\s*"
    r"(?P<name>(?:\w+::)?\w+)\s*\([^;{}]*\)\s*(?:const\s*)?\{"
)
LOOKUP_CALL = re.compile(r"\bGetNativePtrForLookup\s*\(")
DISPATCH_CALL = re.compile(r"\bGetNativePtr\s*\(")


@dataclass(frozen=True)
class Body:
    """One function definition and the body offsets of its brace block."""

    name: str
    header_start: int
    header_end: int
    start: int
    end: int


def _line(masked: str, pos: int) -> int:
    return masked[:pos].count("\n") + 1


def _bodies(masked: str) -> list[Body]:
    """Return the definitions of the accessors and lookup/dispatch functions."""
    bodies: list[Body] = []
    for match in FUNCTION.finditer(masked):
        span = find_block(masked, match.end() - 1)
        if span is None:
            continue
        bodies.append(
            Body(
                match.group("name"),
                match.start(),
                match.end(),
                span[0],
                span[1],
            )
        )
    return bodies


def _first(bodies: list[Body], name: str) -> Body | None:
    return next((body for body in bodies if body.name == name), None)


def _valid_only(body: str) -> bool:
    """Whether an accessor body is the valid-only lookup.

    The body may be a single ternary or an early return, but it must not test
    the invalidation flag and must mention both the validity flag and the
    payload.
    """
    text = normalize(body)
    if "invalidated" in text or "&&" in text or "||" in text:
        return False
    return (
        re.search(r"\bvalid\b", text) is not None
        and "native_ptr" in text
        and "nullptr" in text
    )


def _dispatch_guarded(body: str) -> bool:
    """Whether the dispatch accessor still rejects a finalized instance."""
    text = normalize(body)
    return (
        re.search(r"\bvalid\b", text) is not None
        and "invalidated" in text
        and "&&" in text
        and "native_ptr" in text
    )


def scan_text(rel: str, source: str) -> list[Finding]:
    """Return every native-instance lookup violation in one C++ source."""
    masked = mask(source)
    bodies = _bodies(masked)
    findings: list[Finding] = []

    lookup = _first(bodies, LOOKUP_FN)
    accessor = _first(bodies, LOOKUP_ACCESSOR)
    guarded = _first(bodies, DISPATCH_ACCESSOR)

    if lookup is None:
        findings.append(
            Finding(
                rel,
                1,
                RULE_LOOKUP,
                anchor=LOOKUP_FN,
                message=f"{LOOKUP_FN} is missing",
            )
        )
    else:
        body = masked[lookup.start : lookup.end]
        if LOOKUP_CALL.search(body) is None:
            findings.append(
                Finding(
                    rel,
                    _line(masked, lookup.start),
                    RULE_LOOKUP,
                    anchor=LOOKUP_FN,
                    message=f"{LOOKUP_FN} must resolve through {LOOKUP_ACCESSOR}()",
                )
            )
        if DISPATCH_CALL.search(body) is not None:
            findings.append(
                Finding(
                    rel,
                    _line(masked, lookup.start),
                    RULE_LOOKUP,
                    anchor=LOOKUP_FN,
                    message=(
                        f"{LOOKUP_FN} must not use the invalidation-guarded "
                        f"{DISPATCH_ACCESSOR}()"
                    ),
                )
            )

    if accessor is None:
        findings.append(
            Finding(
                rel,
                1,
                RULE_LOOKUP,
                anchor=LOOKUP_ACCESSOR,
                message=f"{LOOKUP_ACCESSOR}() is missing",
            )
        )
    elif not _valid_only(masked[accessor.start : accessor.end]):
        findings.append(
            Finding(
                rel,
                _line(masked, accessor.start),
                RULE_LOOKUP_GUARD,
                anchor=LOOKUP_ACCESSOR,
                message=(
                    f"{LOOKUP_ACCESSOR}() must be the valid-only lookup; it "
                    "must not grow the valid && !invalidated dispatch guard"
                ),
            )
        )

    if guarded is None:
        findings.append(
            Finding(
                rel,
                1,
                RULE_DISPATCH_GUARD,
                anchor=DISPATCH_ACCESSOR,
                message=f"{DISPATCH_ACCESSOR}() is missing",
            )
        )
    elif not _dispatch_guarded(masked[guarded.start : guarded.end]):
        findings.append(
            Finding(
                rel,
                _line(masked, guarded.start),
                RULE_DISPATCH_GUARD,
                anchor=DISPATCH_ACCESSOR,
                message=(
                    f"{DISPATCH_ACCESSOR}() must keep the "
                    "valid && !invalidated dispatch guard"
                ),
            )
        )

    for name in DISPATCH_FNS:
        body = _first(bodies, name)
        if body is None:
            findings.append(
                Finding(
                    rel,
                    1,
                    RULE_DISPATCH,
                    anchor=name,
                    message=f"dispatch function {name} is missing",
                )
            )
            continue
        text = masked[body.start : body.end]
        if DISPATCH_CALL.search(text) is None:
            findings.append(
                Finding(
                    rel,
                    _line(masked, body.start),
                    RULE_DISPATCH,
                    anchor=name,
                    message=(
                        f"{name} must resolve the payload through "
                        f"{DISPATCH_ACCESSOR}()"
                    ),
                )
            )
        if LOOKUP_ACCESSOR in text:
            findings.append(
                Finding(
                    rel,
                    _line(masked, body.start),
                    RULE_DISPATCH,
                    anchor=name,
                    message=(
                        f"{name} must use the invalidation-guarded "
                        f"{DISPATCH_ACCESSOR}(), not {LOOKUP_ACCESSOR}()"
                    ),
                )
            )

    # A dispatch accessor call outside the four dispatch bodies (the class
    # lookup itself is checked above) is the same opposite regression.
    for call in DISPATCH_CALL.finditer(masked):
        pos = call.start()
        if any(body.header_start <= pos < body.header_end for body in bodies):
            continue
        enclosing = next(
            (body for body in bodies if body.start <= pos < body.end),
            None,
        )
        if enclosing is not None and enclosing.name == LOOKUP_FN:
            continue
        if enclosing is not None and enclosing.name in DISPATCH_FNS:
            continue
        findings.append(
            Finding(
                rel,
                _line(masked, pos),
                RULE_DISPATCH,
                anchor=DISPATCH_ACCESSOR,
                message=(
                    f"{DISPATCH_ACCESSOR}() used outside the dispatch path; "
                    f"the class lookup must use {LOOKUP_ACCESSOR}()"
                ),
            )
        )

    return dedupe_findings(findings)


def scan(paths=None) -> list[Finding]:
    """Scan the explicit paths, or the ABI file, for lookup violations."""
    if paths is None:
        paths = [TARGET]
    findings: list[Finding] = []
    for path in paths:
        path = path if isinstance(path, Path) else Path(path)
        files = sorted(path.glob("*.cpp")) if path.is_dir() else [path]
        for file in files:
            try:
                rel = file.relative_to(ROOT).as_posix()
            except ValueError:
                rel = file.as_posix()
            findings.extend(scan_text(rel, file.read_text(encoding="utf-8")))
    return dedupe_findings(findings)


def _mutation(source: str, old: str, new: str) -> str | None:
    """Replace exactly one intended occurrence, or None for a bad test."""
    if source.count(old) < 1:
        return None
    return source.replace(old, new, 1)


def self_test() -> tuple[int, int]:
    """Run the good fixture and one mutation per rule; return (code, count)."""
    if not FIXTURES.is_dir():
        print(f"ERROR: native-instance-lookup self-test fixtures missing at {FIXTURES}")
        return 1, 0
    try:
        good = (ROOT / GOOD_FIXTURE).read_text(encoding="utf-8")
    except OSError as error:
        print(f"ERROR: native-instance-lookup self-test: {error}")
        return 1, 0

    good_findings = scan_text(GOOD_FIXTURE, good)
    if good_findings:
        first = good_findings[0]
        print(
            "ERROR: native-instance-lookup self-test: the good fixture "
            f"unexpectedly matched [{first.rule}] {first.location}: "
            f"{first.message}"
        )
        return 1, 0

    mutations = (
        (
            "lookup uses the dispatch accessor",
            "->GetNativePtrForLookup();",
            "->GetNativePtr();",
            RULE_LOOKUP,
        ),
        (
            "lookup accessor grows the dispatch guard",
            "return valid ? native_ptr : nullptr;",
            "return (valid && !invalidated) ? native_ptr : nullptr;",
            RULE_LOOKUP_GUARD,
        ),
        (
            "dispatch drops the guarded accessor",
            "->GetNativePtr();",
            "->GetNativePtrForLookup();",
            RULE_DISPATCH,
        ),
        (
            "dispatch accessor loses the invalidation guard",
            "return (valid && !invalidated) ? native_ptr : nullptr;",
            "return valid ? native_ptr : nullptr;",
            RULE_DISPATCH_GUARD,
        ),
    )
    for label, old, new, rule in mutations:
        mutated = _mutation(good, old, new)
        if mutated is None:
            print(
                "ERROR: native-instance-lookup self-test: mutation "
                f"fixture text not found ({label})"
            )
            return 1, 0
        findings = scan_text(GOOD_FIXTURE, mutated)
        if not any(finding.rule == rule for finding in findings):
            print(
                "ERROR: native-instance-lookup self-test: mutation "
                f"{label!r} did not raise {rule}"
            )
            return 1, 0
    return 0, len(mutations)


def main(argv=None) -> int:
    args = set(argv or [])
    code, mutations = self_test()
    if code:
        return code
    if "--self-test" in args:
        print(
            "native-instance-lookup self-test: "
            f"{mutations} mutation(s) flagged, good fixture clean"
        )
        return 0

    findings = scan()
    for finding in findings:
        print(
            f"ERROR: native-instance lookup violation [{finding.rule}] "
            f"{finding.location}: {finding.message}"
        )
    if findings:
        print("")
        print(f"ERROR: {len(findings)} native-instance lookup violation(s).")
        print("  tjs2_get_native_instance must call GetNativePtrForLookup();")
        print("  that accessor must return native_ptr only when valid, while")
        print("  method/property/constructor dispatch keeps the")
        print("  valid && !invalidated GetNativePtr() guard.")
        return 1

    print(
        "native-instance-lookup guard: clean "
        f"({len(DISPATCH_FNS)} dispatch site(s), 1 class lookup)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
