#!/usr/bin/env python3
"""Native void getter guard (AGENTS §4).

A native property getter must hand the script the value the reference returns.
When the backing state is a ``StoredValue`` field whose constructor default is
``StoredValue::Void`` and the getter returns that field unchanged, a fresh
object surfaces a *void* value where the reference returns an array, a
dictionary or a native object. Reading a member from a void value raises
``Member ... does not exist``, so the first script that touches the property
fails (``Motion.Player.variableKeys`` was the observed case).

A finding is a raw void passthrough:

* a field declared ``pub(crate) <name>: StoredValue`` somewhere in
  ``crates/compat/src``, and
* that field is initialised as ``StoredValue::Void`` (the constructor default),
  and
* a native body returns it directly (``p.<name>.to_retained(ctx)``,
  ``inst.<name>.to_retained(&ctx)``, in a property getter or any other body),
  and
* the enclosing function or macro invocation contains no
  ``StoredValue::Void`` guard that maps the void value to the reference's
  non-void result.

The guarded form (``player_get_variable_keys``: it matches
``StoredValue::Void`` and returns a fresh empty array) is not a finding. A
property whose reference getter really can read back void is recorded, with a
reason, in ``scripts/allowlists/void_getter_allow.txt`` under
``path:field``. An entry whose reason starts with ``debt:`` tracks a known
defect that is not fixed yet; it does not silence the finding. The rule is
deliberately narrow: a missed passthrough is acceptable, a flagged getter
that is correct is not.

Usage:
    uv run --project scripts krkr-gate void-getter
    uv run --project scripts krkr-gate void-getter --self-test
    uv run --project scripts krkr-gate void-getter --print-all
    uv run --project scripts krkr-gate void-getter --quiet
"""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass

from ..common import ALLOWLISTS_DIR, FIXTURES_DIR, REPO_ROOT
from ..framework import (
    FixtureRules,
    Finding,
    blank_cfg_test,
    dedupe_findings,
    filter_findings,
    fixture_problems,
    mask,
    print_all_findings,
    print_stale,
    read_allowlist_entries,
    stale_keys,
    tokenize,
)

ROOT = REPO_ROOT
COMPAT_SRC = ROOT / "crates" / "compat" / "src"
FIXTURES = FIXTURES_DIR / "void_getter"
ALLOW = ALLOWLISTS_DIR / "void_getter_allow.txt"
RULE = "void-getter"
DEBT = "debt:"

BAD_FIXTURE = "scripts/fixtures/void_getter/bad/raw_getter.rs"
GOOD_GUARDED_FIXTURE = "scripts/fixtures/void_getter/good/guarded_getter.rs"
GOOD_CLEAN_FIXTURE = "scripts/fixtures/void_getter/good/clean_getter.rs"

# A stored-value field a getter may hand to the script.
FIELD_DECL = re.compile(r"\bpub\s*\(\s*crate\s*\)\s+(\w+)\s*:\s*StoredValue\b")

# The void default of a `StoredValue` field in a constructor/Default impl.
VOID_DEFAULT = re.compile(r"\b(\w+)\s*:\s*StoredValue\s*::\s*Void\b")

# `p.<field>.to_retained(ctx)` / `inst.<field>.to_retained(&ctx)`.
PASSTHROUGH = re.compile(
    r"\b([A-Za-z_]\w*)\s*\.\s*(\w+)\s*\.\s*to_retained\s*\("
)

# A remap of the void default to the reference's non-void result.
VOID_GUARD = re.compile(r"\bStoredValue\s*::\s*Void\b")

_DELIMITER_PAIRS = {")": "(", "]": "[", "}": "{"}


@dataclass(frozen=True)
class VoidEntry:
    """One ``path:field — reason`` allowlist entry."""

    path: str
    field: str
    reason: str

    @property
    def key(self) -> str:
        """The allowlist key, anchored on the field name."""
        return f"{self.path}:{self.field}"

    @property
    def debt(self) -> bool:
        """Whether the entry tracks a known, unfixed defect."""
        return self.reason.startswith(DEBT)


def _rust_files(root):
    """The ``.rs`` files under ``root``, skipping ``tests/`` directories."""
    if root.is_file():
        files = [root]
    else:
        files = sorted(root.rglob("*.rs"))
    for path in files:
        try:
            parts = path.relative_to(ROOT).parts
        except ValueError:
            parts = path.parts
        if "tests" in parts:
            continue
        yield path


def _relative(path) -> str:
    """The repo-relative POSIX path of ``path`` (absolute when outside)."""
    try:
        return path.relative_to(ROOT).as_posix()
    except ValueError:
        return path.as_posix()


def _item_ranges(masked: str) -> list[tuple[int, int]]:
    """Character ranges of the top-level items of a masked Rust file.

    An item ends at a ``;`` or at the closing ``}`` that returns every
    delimiter to the top level; a macro invocation whose body carries a
    closure is one item, so its body is the guard scope the rule inspects.
    """
    ranges: list[tuple[int, int]] = []
    stack: list[str] = []
    start = 0
    for token, offset in tokenize(masked):
        if token in ("(", "[", "{"):
            stack.append(token)
        elif token in _DELIMITER_PAIRS:
            if stack and stack[-1] == _DELIMITER_PAIRS[token]:
                stack.pop()
            if not stack:
                ranges.append((start, offset + 1))
                start = offset + 1
        elif token == ";" and not stack:
            ranges.append((start, offset + 1))
            start = offset + 1
    if masked[start:].strip():
        ranges.append((start, len(masked)))
    return ranges


def _enclosing_item(ranges, offset):
    """The ``(start, end)`` item that contains ``offset``, if any."""
    for start, end in ranges:
        if start <= offset < end:
            return start, end
    return None


def _read(path) -> str:
    """Read and lexically blank one Rust file (comments, literals, tests)."""
    return blank_cfg_test(mask(path.read_text(encoding="utf-8", errors="replace")))


def void_fields(masked_by_path) -> set[str]:
    """Field names declared ``pub(crate) <name>: StoredValue`` and default-void.

    The declaration is collected from the whole scanned tree, so a payload
    declared in one module and constructed in another is still recognised.
    """
    declared: set[str] = set()
    defaulted: set[str] = set()
    for masked in masked_by_path.values():
        declared.update(m.group(1) for m in FIELD_DECL.finditer(masked))
        defaulted.update(m.group(1) for m in VOID_DEFAULT.finditer(masked))
    return declared & defaulted


def scan(roots=None) -> list[Finding]:
    """Return one finding per raw void passthrough of a default-void field."""
    if roots is None:
        roots = [COMPAT_SRC]
    files = [path for root in roots for path in _rust_files(root)]
    masked_by_path = {path: _read(path) for path in files}
    fields = void_fields(masked_by_path)
    findings: list[Finding] = []
    for path, masked in masked_by_path.items():
        rel = _relative(path)
        ranges = _item_ranges(masked)
        for match in PASSTHROUGH.finditer(masked):
            receiver, field = match.group(1), match.group(2)
            if field not in fields:
                continue
            item = _enclosing_item(ranges, match.start())
            if item is not None and VOID_GUARD.search(masked[item[0] : item[1]]):
                continue
            line = masked.count("\n", 0, match.start()) + 1
            findings.append(
                Finding(
                    rel,
                    line,
                    RULE,
                    anchor=field,
                    message=f"{receiver}.{field}",
                )
            )
    return dedupe_findings(findings)


def load_allowlist() -> list[VoidEntry]:
    """Return the reviewed ``path:field`` exceptions."""
    entries: list[VoidEntry] = []
    for key, reason in read_allowlist_entries(ALLOW):
        path, _, field = key.rpartition(":")
        if not path or not field:
            continue
        entries.append(VoidEntry(path.strip(), field.strip(), reason))
    return entries


def entry_matches(entry: VoidEntry, finding: Finding) -> bool:
    return entry.path == finding.path and entry.field == finding.anchor


def self_test() -> int:
    """The raw fixture must trip the rule once; the near-misses stay clean."""
    if not FIXTURES.is_dir():
        print(f"ERROR: void-getter self-test fixtures missing at {FIXTURES}")
        return 1
    findings = scan([FIXTURES])
    problems = fixture_problems(
        findings,
        [FixtureRules(BAD_FIXTURE, (RULE,), (GOOD_GUARDED_FIXTURE, GOOD_CLEAN_FIXTURE))],
    )
    bad = [finding for finding in findings if finding.path == BAD_FIXTURE]
    if len(bad) != 1:
        problems.append(
            f"{BAD_FIXTURE}: expected exactly 1 `{RULE}` finding, matched {len(bad)}"
        )
    if problems:
        print("ERROR: void-getter self-test:")
        for problem in problems:
            print(f"  {problem}")
        print("  Fix the rule that stopped matching; do not silence the self-test.")
        return 1
    return 0


def main(argv=None) -> int:
    args = set(argv or ())
    selftest = self_test()
    if selftest != 0:
        return selftest
    if "--self-test" in args:
        return 0

    findings = scan()
    if "--print-all" in args:
        print_all_findings(
            findings,
            lambda finding: (
                f"{finding.path}:{finding.line}:{finding.rule}: "
                f"{finding.message} (default-void field `{finding.anchor}`)"
            ),
        )
        return 0

    allow = load_allowlist()
    reviewed = [entry for entry in allow if not entry.debt]
    debt = [entry for entry in allow if entry.debt]

    raw, used_raw = filter_findings(findings, reviewed, entry_matches, first_match=True)
    unallowed, used_debt = filter_findings(raw, debt, entry_matches, first_match=True)
    tracked = [finding for finding in raw if finding not in unallowed]

    for finding in unallowed:
        print(
            f"ERROR: raw void passthrough [{RULE}] {finding.location}: "
            f"{finding.message} is returned straight from a `StoredValue::Void`"
            f"-defaulted field"
        )
    for finding in tracked:
        reason = next(
            (entry.reason for entry in debt if entry_matches(entry, finding)), DEBT
        )
        print(
            f"ERROR: raw void passthrough [{RULE}] [debt] {finding.location}: "
            f"{finding.message} — {reason.removeprefix(DEBT).strip()}"
        )

    print_stale(
        stale_keys(reviewed, used_raw, lambda entry: entry.key)
        + stale_keys(debt, used_debt, lambda entry: f"{entry.key} (debt)")
    )

    if unallowed or tracked:
        print("")
        print(
            f"ERROR: {len(unallowed) + len(tracked)} raw void passthrough "
            f"site(s) reach the script ({len(findings)} site(s) checked, "
            f"{len(reviewed)} allowlisted, {len(tracked)} tracked as debt)."
        )
        print(
            "  Map the void value to the reference's non-void result (an array,"
            " a dictionary"
        )
        print(
            "  or the class's native object) before it reaches the script. When"
            " the reference"
        )
        print(
            f"  property is genuinely void-able, add a reviewed `path:field —"
            f" reason` line to {ALLOW.relative_to(ROOT)}."
        )
        print(
            "  A `debt:` entry records the fix that is still owed; it does not"
            " silence the finding."
        )
        return 1

    if "--quiet" not in args:
        print(
            f"void-getter guard: clean ({len(findings)} site(s) checked, "
            f"{len(reviewed)} allowlisted) — {ALLOW.relative_to(ROOT)}"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
