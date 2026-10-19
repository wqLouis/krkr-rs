"""Native-instance construction guard (AGENTS §4/D11).

A native class instance is created by the VM's ``CreateNativeInstance`` (the
Rust ``*_create`` callback) *before* the script's constructor runs, and a
script subclass may touch a member from its class prelude or from its
constructor before it calls ``super.<Class>(...)``. The payload therefore has
to carry the state every member helper needs the moment it exists; a
``*_create`` that returns a bare ``Default`` payload while the same module's
helpers refuse an unconstructed payload is the
``Layer: this layer is not constructed`` class of bug (the real game
千恋万花's ``MessageLayer`` writes ``left`` before ``super.AnimKAGLayer``).

For every module under ``crates/compat/src/natives`` that raises a
"not constructed" error, a ``*_create`` whose whole body is
``Box::into_raw(Box::new(<T>::default()))`` is a finding (``native-preconstruct``).
Sites that are intentionally lazy are accepted in
``scripts/allowlists/native_construction_allow.txt``.
"""

import re
import sys

from ..common import ALLOWLISTS_DIR, FIXTURES_DIR, REPO_ROOT
from ..framework import (
    AllowEntry,
    Finding,
    anchor_in,
    filter_findings,
    find_block,
    load_path_line_allowlist,
    mask,
    print_all_findings,
    print_stale,
    stale_keys,
)

ROOT = REPO_ROOT
ALLOW = ALLOWLISTS_DIR / "native_construction_allow.txt"

NATIVES = ROOT / "crates" / "compat" / "src" / "natives"
FIXTURES = FIXTURES_DIR / "native_construction"
BAD_FIXTURE = "scripts/fixtures/native_construction/bad_lazy_instance.rs"
GOOD_FIXTURE = "scripts/fixtures/native_construction/good_eager_instance.rs"
RULE = "native-preconstruct"

# A member helper that refuses a payload before the constructor has joined it.
GUARD = re.compile(
    r"is not constructed|not properly constructed|ObjectIsNotProperlyConstructed"
)

# An ``extern "C" fn <name>_create(...) -> *mut c_void {`` header. The
# `"C"` literal is blanked by `mask`, so accept its absence.
CREATE = re.compile(
    r'extern\s+(?:"C"\s+)?fn\s+(\w+_create)\s*\([^)]*\)\s*->\s*\*mut\s+c_void\s*\{'
)
# The entire function body is a bare
# ``Box::into_raw(Box::new(<T>::default())) as *mut c_void``.
BARE_DEFAULT = re.compile(
    r"^\s*Box::into_raw\(\s*Box::(?:new\(\s*(?P<t1>\w+)\s*::default\(\)\s*\)"
    r"|<\s*(?P<t2>\w+)\s*>::default\(\))\s*\)\s*as\s*\*mut\s+c_void\s*;?\s*$",
    re.S,
)


def _function_body(masked, open_brace):
    """Return the text of the block whose ``{`` is at ``open_brace``."""
    span = find_block(masked, open_brace)
    if span is not None:
        return masked[span[0] : span[1]]
    return masked[open_brace + 1 : len(masked) - 1]


def has_guard(source):
    """Whether the source raises a real pre-construction error.

    The guard lives inside an error string. A ``log::trace!`` that merely says
    the instance is unconstructed is not a rejection, so those lines are
    ignored.
    """
    for line in source.splitlines():
        if GUARD.search(line) and "log::" not in line:
            return True
    return False


def scan(roots=None) -> list[Finding]:
    """Return one finding per bare-default create callback."""
    if roots is None:
        roots = [NATIVES]
    findings: list[Finding] = []
    for root in roots:
        paths = [root] if root.is_file() else sorted(root.rglob("*.rs"))
        for path in paths:
            try:
                rel = path.relative_to(ROOT).as_posix()
            except ValueError:
                rel = path.as_posix()
            source = path.read_text(encoding="utf-8")
            # The guard message lives inside a string literal (or an `#[error]`
            # attribute), which `mask` blanks, so look at the raw source for
            # it and at the masked source for the code shape.
            if not has_guard(source):
                continue
            masked = mask(source)
            for match in CREATE.finditer(masked):
                body = _function_body(masked, match.end() - 1)
                if BARE_DEFAULT.match(body):
                    line = masked[: match.start()].count("\n") + 1
                    findings.append(Finding(rel, line, RULE, match.group(1)))
    return findings


def load_allowlist() -> list[AllowEntry]:
    """Return the reviewed native-preconstruction exceptions."""
    return load_path_line_allowlist(ALLOW)


def entry_matches(entry: AllowEntry, finding: Finding) -> bool:
    if entry.path != finding.path:
        return False
    if entry.pattern == RULE:
        return entry.line == finding.line
    # Code anchor: the create function name, stable across line churn.
    return anchor_in(entry.pattern, finding.anchor)


def self_test():
    """The bad fixture must trip the rule; the near-miss must trip nothing."""
    if not FIXTURES.is_dir():
        print(f"ERROR: native-construction self-test fixtures missing at {FIXTURES}")
        return 1
    findings = scan([FIXTURES])
    bad = [f for f in findings if f.path == BAD_FIXTURE]
    good = [f for f in findings if f.path == GOOD_FIXTURE]
    if not bad:
        print(
            f"ERROR: native-construction self-test: {BAD_FIXTURE} "
            f"matched no `{RULE}` finding. Fix the rule; do not silence it."
        )
        return 1
    if good:
        print(
            f"ERROR: native-construction self-test: {GOOD_FIXTURE} "
            f"unexpectedly matched {good[0].anchor}; the eager create callback "
            "must not be flagged."
        )
        return 1
    return 0


def main(argv=None):
    args = set(argv or [])
    selftest = self_test()
    if selftest != 0:
        return selftest
    if "--self-test" in args:
        return 0

    findings = scan()
    if "--print-all" in args:
        print_all_findings(findings)
        return 0

    allow = load_allowlist()
    unallowed, used = filter_findings(
        findings, allow, entry_matches, first_match=True
    )

    for finding in unallowed:
        print(
            f"ERROR: native-instance construction violation [{finding.rule}] "
            f"{finding.path}:{finding.line}: {finding.anchor}"
        )
    stale = stale_keys(
        allow, used, lambda entry: f"{entry.path}:{entry.line}:{entry.pattern}"
    )
    print_stale(stale)

    if unallowed:
        print("")
        print(
            f"ERROR: {len(unallowed)} native instance(s) left unconstructed "
            f"({len(findings)} site(s) checked, {len(allow)} allowlisted)."
        )
        print("  Construct the payload in the create callback (the reference's")
        print("  CreateNativeInstance already hands out a usable instance), or")
        print(f"  add a reviewed entry to {ALLOW.relative_to(ROOT)}")
        return 1

    print(
        f"native-construction guard: clean ({len(findings)} site(s) checked, "
        f"{len(allow)} allowlisted) — {ALLOW.relative_to(ROOT)}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
