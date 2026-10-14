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
from .error_handling import mask

ROOT = REPO_ROOT
ALLOW = ALLOWLISTS_DIR / "native_construction_allow.txt"
ARGS: set[str] = set()
PRINT_ALL = False

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
    depth = 1
    i = open_brace + 1
    while i < len(masked) and depth:
        if masked[i] == "{":
            depth += 1
        elif masked[i] == "}":
            depth -= 1
        i += 1
    return masked[open_brace + 1 : i - 1]


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


def scan(roots=None):
    """Return ``(rel_path, line, rule, code)`` findings for the given roots."""
    if roots is None:
        roots = [NATIVES]
    findings = []
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
                    findings.append((rel, line, RULE, match.group(1)))
    return findings


def load_allowlist():
    """Return a list of ``(path, line, pattern, reason)`` allowlist entries."""
    entries = []
    if not ALLOW.exists():
        return entries
    for raw in ALLOW.read_text(encoding="utf-8").splitlines():
        text = raw.strip()
        if not text or text.startswith("#"):
            continue
        key, sep, reason = text.partition("—")
        if not sep:
            key, sep, reason = text.partition(" -- ")
        parts = key.split(":", 2)
        if len(parts) != 3:
            continue
        path, line, pattern = parts[0].strip(), parts[1].strip(), parts[2].strip()
        try:
            line_no = int(line)
        except ValueError:
            line_no = -1
        entries.append((path, line_no, pattern, reason.strip()))
    return entries


def normalize(text):
    return " ".join(text.split())


def entry_matches(entry, rel, line, code):
    path, line_no, pattern, _reason = entry
    if path != rel:
        return False
    if pattern == RULE:
        return line_no == line
    # Code anchor: the create function name, stable across line churn.
    return normalize(pattern) in normalize(code)


def self_test():
    """The bad fixture must trip the rule; the near-miss must trip nothing."""
    if not FIXTURES.is_dir():
        print(f"ERROR: native-construction self-test fixtures missing at {FIXTURES}")
        return 1
    findings = scan([FIXTURES])
    bad = [f for f in findings if f[0] == BAD_FIXTURE]
    good = [f for f in findings if f[0] == GOOD_FIXTURE]
    if not bad:
        print(
            f"ERROR: native-construction self-test: {BAD_FIXTURE} "
            f"matched no `{RULE}` finding. Fix the rule; do not silence it."
        )
        return 1
    if good:
        print(
            f"ERROR: native-construction self-test: {GOOD_FIXTURE} "
            f"unexpectedly matched {good[0]}; the eager create callback "
            "must not be flagged."
        )
        return 1
    return 0


def main():
    selftest = self_test()
    if selftest != 0:
        return selftest
    if "--self-test" in ARGS:
        return 0

    findings = scan()
    if PRINT_ALL:
        for rel, line, rule, code in findings:
            print(f"{rel}:{line}:{rule}: {code}")
        print(f"\n{len(findings)} finding(s) total (allowlist not applied)")
        return 0

    allow = load_allowlist()
    used = [False] * len(allow)
    unallowed = []
    for rel, line, rule, code in findings:
        matched = False
        for i, entry in enumerate(allow):
            if entry_matches(entry, rel, line, code):
                used[i] = True
                matched = True
                break
        if not matched:
            unallowed.append((rel, line, rule, code))

    for rel, line, rule, code in unallowed:
        print(f"ERROR: native-instance construction violation [{rule}] {rel}:{line}: {code}")
    stale = [
        f"{allow[i][0]}:{allow[i][1]}:{allow[i][2]}"
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


def run(argv=None):
    """Run the check. ``argv`` carries the check-specific flags."""
    global ARGS, PRINT_ALL
    ARGS = set(argv or [])
    PRINT_ALL = "--print-all" in ARGS
    return main()


if __name__ == "__main__":
    sys.exit(run(sys.argv[1:]))
