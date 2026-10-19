"""House-style guard (AGENTS §5/§6).

Scans ``crates/**`` for bean accessors that only move a field, ``&String`` /
``&Vec<T>`` / ``&Box<T>`` / ``&PathBuf`` parameters, decision numbers in
comments, flat glob re-exports, internal jargon in log/print literals,
non-test comments that cite the external reference/oracle or narrate history,
and ``clippy`` allowances anywhere in Rust code.  Reviewed false positives are
listed in ``scripts/allowlists/house_style_allow.txt``; legacy comments that
predate the comment rules are listed separately in
``scripts/allowlists/house_style_baseline.txt`` (tracked in ``TODO`` §21), so
the gate catches new citations while the old ones burn down in batches.  The
``clippy-allow`` rule has no exemptions in either file.
"""

import json
import re
import sys
from collections import Counter

from ..common import ALLOWLISTS_DIR, FIXTURES_DIR, REPO_ROOT
from ..framework import (
    AllowEntry,
    FixtureRules,
    Finding,
    IDENT_RE,
    anchor_in,
    dedupe_findings,
    filter_findings,
    fixture_problems,
    group_findings,
    in_ranges,
    line_number,
    line_offsets,
    load_path_line_allowlist,
    mask,
    print_all_findings,
    print_stale,
    report_self_test_failure,
    scan_spans,
    stale_keys,
    test_ranges,
    tokenize,
)

ROOT = REPO_ROOT
ALLOW = ALLOWLISTS_DIR / "house_style_allow.txt"
BASELINE = ALLOWLISTS_DIR / "house_style_baseline.txt"

CRATES = ROOT / "crates"
FIXTURES = FIXTURES_DIR / "house_style"
BAD_FIXTURE = "scripts/fixtures/house_style/bad_style.rs"
GOOD_FIXTURE = "scripts/fixtures/house_style/good_style.rs"


def find_fns(tokens):
    """Yield (name, name_pos, params, body) for every `fn` in token order.

    `body` is the brace-balanced token slice of the function body, or `None`
    for a body-less declaration.  Generics and the return type are skipped, so
    a `where` clause or an `impl Trait` return does not confuse the parse.
    """
    n = len(tokens)
    i = 0
    while i < n:
        if tokens[i][0] != "fn":
            i += 1
            continue
        if i + 1 >= n or not IDENT_RE.match(tokens[i + 1][0]):
            i += 1
            continue
        name, name_pos = tokens[i + 1]
        j = i + 2
        if j < n and tokens[j][0] == "<":
            depth = 0
            while j < n:
                tk = tokens[j][0]
                if tk == "<":
                    depth += 1
                elif tk == ">":
                    depth -= 1
                    if depth == 0:
                        j += 1
                        break
                elif tk == ">>":
                    depth -= 2
                    if depth <= 0:
                        j += 1
                        break
                j += 1
        while j < n and tokens[j][0] != "(":
            if tokens[j][0] in (";", "{", "}"):
                break
            j += 1
        if j >= n or tokens[j][0] != "(":
            i += 1
            continue
        depth = 0
        k = j
        while k < n:
            if tokens[k][0] == "(":
                depth += 1
            elif tokens[k][0] == ")":
                depth -= 1
                if depth == 0:
                    break
            k += 1
        params = tokens[j + 1 : k]
        j = k + 1
        while j < n and tokens[j][0] not in ("{", ";", "}"):
            j += 1
        body = None
        if j < n and tokens[j][0] == "{":
            depth = 0
            bstart = j + 1
            k = j
            while k < n:
                if tokens[k][0] == "{":
                    depth += 1
                elif tokens[k][0] == "}":
                    depth -= 1
                    if depth == 0:
                        break
                k += 1
            body = tokens[bstart:k]
            j = k + 1
        yield name, name_pos, params, body
        i = max(j, i + 1)


def strip_trailing(tokens):
    while tokens and tokens[-1][0] in (";", ","):
        tokens = tokens[:-1]
    return tokens


def is_field_access(tokens):
    """True for a body that only reads `self.field` (optionally by reference)."""
    tokens = strip_trailing(tokens)
    if len(tokens) >= 4 and tokens[0][0] == "&":
        if tokens[1][0] == "mut":
            return (
                len(tokens) == 5
                and tokens[2][0] == "self"
                and tokens[3][0] == "."
                and IDENT_RE.match(tokens[4][0])
            )
        return (
            len(tokens) == 4
            and tokens[1][0] == "self"
            and tokens[2][0] == "."
            and IDENT_RE.match(tokens[3][0])
        )
    return (
        len(tokens) == 3
        and tokens[0][0] == "self"
        and tokens[1][0] == "."
        and IDENT_RE.match(tokens[2][0])
    )


def is_ok_field(tokens):
    """True for a body that only returns `Ok(self.field)`."""
    tokens = strip_trailing(tokens)
    if len(tokens) >= 5 and tokens[0][0] == "Ok" and tokens[1][0] == "(" and tokens[-1][0] == ")":
        return is_field_access(tokens[2:-1])
    return False


def is_trivial_setter(tokens):
    """True for a body that only moves an argument into `self.field`."""
    tokens = strip_trailing(tokens)
    return (
        len(tokens) == 5
        and tokens[0][0] == "self"
        and tokens[1][0] == "."
        and IDENT_RE.match(tokens[2][0])
        and tokens[3][0] == "="
        and IDENT_RE.match(tokens[4][0])
    )


def ref_collection_hits(params):
    """Positions of `&String` / `&Vec<…>` / `&Box<…>` / `&PathBuf` types."""
    hits = []
    for idx, (tok, pos) in enumerate(params):
        if idx == 0 or params[idx - 1][0] != "&":
            continue
        if tok in ("String", "PathBuf"):
            hits.append(pos)
        elif tok in ("Vec", "Box") and idx + 1 < len(params) and params[idx + 1][0] == "<":
            hits.append(pos)
    return hits


# A whole-crate glob re-export: `pub use crate::…::*` or an external crate.
# `self`/`super` module globs and non-`pub` globs are not matched.
GLOB_RE = re.compile(
    r"\bpub\s+use\s+"
    r"(?:crate|(?!self\b|super\b)[A-Za-z_][A-Za-z0-9_]*)"
    r"::(?:[A-Za-z_][A-Za-z0-9_]*::)*\*;"
)
# A decision number: D1–D999, but a `U+D800` scalar (the `+` lookbehind) and a
# `0xD800` byte (no word boundary) are not decision numbers.
DEC_RE = re.compile(r"(?<!\+)\bD[0-9]{1,3}\b")
LOG_RE = re.compile(
    r"\b(?:log::(?:trace|debug|info|warn|error)|println|eprintln|print|eprint)!\s*"
)
JARGON_RE = re.compile(r"(?<!\+)\bL[1-4]\b|(?<!\+)\bD[0-9]{1,3}\b")

# A production comment describes the code as it is: it does not cite the
# external engine or the port history.  The term list is deliberately
# overinclusive; the allowlist carries only reviewed false positives, never a
# true citation.
REFERENCE_TERMS_RE = re.compile(
    r"reference/cpp"
    r"|\breference\b"
    r"|\boracle\b"
    r"|\bC\+\+"
    r"|\bported\b"
    r"|\bport of\b"
    r"|\bupstream\b"
    r"|\boriginal implementation\b"
    r"|\bTVP\b"
    r"|kirikiri",
    re.IGNORECASE,
)
# A C++ source file with a line (or line-range) suffix: `LayerIntf.cpp:42`.
CPP_CITATION_RE = re.compile(
    r"[\w./\\-]+\.(?:cpp|hpp|h|cc|cxx|inl):\d+(?:-\d+)?", re.IGNORECASE
)
# A citation-shaped parenthetical, with or without a line:
# `(psbfile/main.cpp + PSBFile.cpp)`.
CPP_PAREN_RE = re.compile(
    r"\([^()\n]*[\w./\\-]+\.(?:cpp|hpp|h|cc|cxx|inl)\b[^()\n]*\)", re.IGNORECASE
)
# Comments that narrate history instead of describing the code.
HISTORY_RE = re.compile(
    r"\bused to\b|\bpreviously\b|\bbefore this\b|\breplaced by\b", re.IGNORECASE
)
# `#[allow(clippy::…)]`, `#![allow(clippy::…)]` or `#[expect(clippy::…)]`.
CLIPPY_ALLOW_RE = re.compile(r"#!?\[\s*(?:allow|expect)\s*\([^)]*clippy\s*::")


def scan_file(rel, src):
    findings = []
    orig = src.splitlines()
    masked = mask(src)
    ranges = test_ranges(masked)
    spans = list(scan_spans(src))
    lines = masked.splitlines()
    offsets = line_offsets(masked)

    def add(pos, rule):
        ln = min(line_number(offsets, pos), len(orig))
        code = orig[ln - 1].strip() if ln - 1 < len(orig) else ""
        findings.append(Finding(rel, ln, rule, code))

    # A clippy allowance is a finding in every Rust file, tests included, and
    # it is never allowlisted; run it before the test-code early return.
    for m in CLIPPY_ALLOW_RE.finditer(masked):
        add(m.start(), "clippy-allow")

    # Test code is exempt from the accessor, parameter, jargon and decision
    # rules; a test-local glob is not part of any crate's contract either, and
    # a test comment is where a `reference/cpp/...:line` citation belongs.
    if "/tests/" in rel or rel.endswith("build.rs"):
        return findings

    tokens = tokenize(masked)
    for name, name_pos, params, body in find_fns(tokens):
        if in_ranges(name_pos, ranges):
            continue
        if body is not None:
            if name.startswith("get_") and (is_field_access(body) or is_ok_field(body)):
                add(name_pos, "bean-accessor")
            elif name.startswith("set_") and is_trivial_setter(body):
                add(name_pos, "bean-accessor")
            elif name.startswith("is_") and is_field_access(body):
                add(name_pos, "bean-accessor")
        for pos in ref_collection_hits(params):
            add(pos, "ref-collection-param")

    for m in GLOB_RE.finditer(masked):
        if not in_ranges(m.start(), ranges):
            add(m.start(), "glob-reexport")

    for kind, a, b in spans:
        if kind != "comment" or in_ranges(a, ranges):
            continue
        text = src[a:b]
        for m in DEC_RE.finditer(text):
            add(a + m.start(), "decision-number")
        for m in REFERENCE_TERMS_RE.finditer(text):
            add(a + m.start(), "reference-citation")
        for m in CPP_CITATION_RE.finditer(text):
            add(a + m.start(), "reference-citation")
        for m in CPP_PAREN_RE.finditer(text):
            add(a + m.start(), "reference-citation")
        for m in HISTORY_RE.finditer(text):
            add(a + m.start(), "history-comment")

    for m in LOG_RE.finditer(masked):
        if in_ranges(m.start(), ranges):
            continue
        j = m.end()
        if j >= len(masked) or masked[j] not in "([{":
            continue
        opener = masked[j]
        closer = {"(": ")", "[": "]", "{": "}"}[opener]
        depth = 0
        k = j
        while k < len(masked):
            if masked[k] == opener:
                depth += 1
            elif masked[k] == closer:
                depth -= 1
                if depth == 0:
                    break
            k += 1
        for skind, sa, sb in spans:
            if skind != "str" or sa < j or sa >= k:
                continue
            if JARGON_RE.search(src[sa:sb]):
                add(sa, "jargon-in-log")

    return findings


def scan(roots=None):
    findings: list[Finding] = []
    root_dirs = [CRATES] if roots is None else list(roots)
    for root in root_dirs:
        if not root.is_dir():
            continue
        for path in sorted(root.rglob("*.rs")):
            rel = str(path.relative_to(ROOT))
            if "/tjs/cpp/" in rel:
                continue
            try:
                src = path.read_text(encoding="utf-8", errors="replace")
            except OSError:
                continue
            findings.extend(scan_file(rel, src))

    return dedupe_findings(findings)


RULES = {
    "bean-accessor",
    "ref-collection-param",
    "decision-number",
    "glob-reexport",
    "jargon-in-log",
    "reference-citation",
    "history-comment",
    "clippy-allow",
}
SELF_TEST_BAD_RULES = tuple(sorted(RULES))


def entry_matches_raw(entry: AllowEntry, finding: Finding) -> bool:
    if entry.path != finding.path:
        return False
    if entry.pattern in RULES:
        return entry.pattern == finding.rule and entry.line == finding.line
    # Code anchor: survives line shifts, fails closed when the code changes.
    return anchor_in(entry.pattern, finding.anchor)


def load_baseline():
    """Return ``(path, rule, anchor, reason)`` legacy-comment baseline entries.

    The baseline is JSON Lines: one object per line with ``path``, ``rule``,
    ``anchor`` (the normalized offending comment line) and ``reason``.  It is
    keyed to the comment text, not a line number, so a shift does not make it
    stale; a rewritten comment does.
    """
    entries = []
    if not BASELINE.exists():
        return entries
    for raw in BASELINE.read_text(encoding="utf-8").splitlines():
        text = raw.strip()
        if not text or text.startswith("#"):
            continue
        try:
            obj = json.loads(text)
        except json.JSONDecodeError:
            continue
        path = str(obj.get("path", ""))
        rule = str(obj.get("rule", ""))
        anchor = str(obj.get("anchor", ""))
        reason = str(obj.get("reason", ""))
        if path and rule and anchor:
            entries.append((path, rule, anchor, reason))
    return entries


def baseline_matches(entry, finding: Finding) -> bool:
    """Whether a legacy-comment baseline entry suppresses a finding.

    A baseline entry names one rule and one normalized comment line, so it can
    only suppress the legacy comment it was recorded for.  The
    ``clippy-allow`` rule is never baselined.
    """
    path, entry_rule, anchor, _reason = entry
    if finding.rule == "clippy-allow":
        return False
    return (
        entry_rule == finding.rule
        and path == finding.path
        and anchor_in(anchor, finding.anchor)
    )


def entry_matches(entry: AllowEntry, finding: Finding) -> bool:
    """Whether an allowlist entry suppresses a finding.

    The ``clippy-allow`` rule has no exemptions: a clippy lint is fixed or the
    code is written differently, never grandfathered.
    """
    if finding.rule == "clippy-allow":
        return False
    return entry_matches_raw(entry, finding)


def crate_of(rel):
    """The crate a repo-relative path belongs to (``platform/fs``, ``compat``)."""
    parts = rel.split("/")
    if len(parts) > 1 and parts[0] == "crates":
        if parts[1] == "platform" and len(parts) > 2:
            return f"platform/{parts[2]}"
        return parts[1]
    return parts[0]


def print_summary(findings):
    """Print the per-rule and per-crate counts of a finding list."""
    rules = Counter(finding.rule for finding in findings)
    crates = Counter(crate_of(finding.path) for finding in findings)
    files = {}
    for finding in findings:
        files.setdefault(crate_of(finding.path), set()).add(finding.path)
    print("")
    print("summary by rule:")
    for rule, count in sorted(rules.items()):
        print(f"  {rule:22s} {count}")
    print("summary by crate (findings in files):")
    for crate, count in sorted(crates.items()):
        print(f"  {crate:22s} {count} finding(s) in {len(files[crate])} file(s)")


def self_test():
    """Assert every rule matches the bad fixture and none matches the good one.

    A regex that silently stops matching is worse than no guard, so the guard
    runs this on every invocation.
    """
    if not FIXTURES.is_dir():
        print(f"ERROR: guard self-test fixtures missing at {FIXTURES}")
        return 1
    findings = scan([FIXTURES])
    by_path = group_findings(findings)
    problems = fixture_problems(
        findings,
        (FixtureRules(BAD_FIXTURE, SELF_TEST_BAD_RULES, (GOOD_FIXTURE,)),),
    )
    if problems:
        report_self_test_failure("ERROR: guard self-test failed", problems)
        return 1
    print(
        "house-style guard: self-test ok "
        f"({len(SELF_TEST_BAD_RULES)} rules flagged the bad fixture, "
        f"{len(by_path.get(GOOD_FIXTURE, []))} finding(s) in the good fixture)"
    )
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
        print_summary(findings)
        return 0

    allow = load_path_line_allowlist(ALLOW)
    baseline = load_baseline()

    # A clippy allowance is a hard finding: neither the reviewed allowlist nor
    # the legacy baseline may suppress it, whether an entry names the rule or
    # anchors the offending line.
    clippy_sites = [f for f in findings if f.rule == "clippy-allow"]
    clippy_entries = [
        f"{entry.path}:{entry.line}:{entry.pattern}"
        for entry in allow
        if entry.pattern == "clippy-allow"
        or any(entry_matches_raw(entry, site) for site in clippy_sites)
    ]
    clippy_baseline = [
        f"{entry[0]}:{entry[1]}:{entry[2]}"
        for entry in baseline
        if entry[1] == "clippy-allow"
    ]
    if clippy_entries or clippy_baseline:
        for key in sorted(clippy_entries + clippy_baseline):
            print(f"ERROR: a clippy allowance cannot be allowlisted: {key}")
        print(
            f"ERROR: clippy-allow entries in {ALLOW.relative_to(ROOT)} and "
            f"{BASELINE.relative_to(ROOT)} must be zero."
        )
        return 1

    unallowed, used = filter_findings(findings, allow, entry_matches)

    # Legacy comments are baselined separately from the reviewed allowlist: a
    # baselined finding is not a failure, and a stale baseline entry is a note.
    remaining, used_baseline = filter_findings(
        unallowed, baseline, baseline_matches
    )
    baselined = len(unallowed) - len(remaining)
    unallowed = remaining

    for finding in unallowed:
        print(
            f"ERROR: house-style violation [{finding.rule}] "
            f"{finding.path}:{finding.line}: {finding.anchor}"
        )

    if unallowed:
        print_summary(unallowed)

    stale = stale_keys(
        allow, used, lambda entry: f"{entry.path}:{entry.line}:{entry.pattern}"
    )
    print_stale(stale)

    stale_baseline = stale_keys(
        baseline, used_baseline, lambda entry: f"{entry[0]}:{entry[1]}"
    )
    print_stale(
        stale_baseline,
        note=(
            f"note: {len(stale_baseline)} baseline entr(y/ies) no longer match "
            "(legacy comment rewritten; remove from "
            f"{BASELINE.relative_to(ROOT)}):"
        ),
    )

    if unallowed:
        print("")
        print(
            f"ERROR: {len(unallowed)} unallowlisted house-style violation(s) "
            f"(AGENTS §5/§6); {len(findings)} site(s) checked, "
            f"{len(allow)} reviewed entr(y/ies), {len(baseline)} baseline "
            f"entr(y/ies) ({baselined} applied)."
        )
        print("  Either fix the site or add a reviewed entry to")
        print(f"  {ALLOW.relative_to(ROOT)}")
        print("  Format: path:line:pattern — reason  (pattern is a rule name")
        print("  or a source-line anchor)")
        return 1

    print(
        f"house-style guard: clean ({len(findings)} site(s) checked, "
        f"{len(allow)} reviewed, {baselined} baselined) — "
        f"{ALLOW.relative_to(ROOT)}, {BASELINE.relative_to(ROOT)}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
