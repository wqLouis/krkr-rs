#!/usr/bin/env bash
# House-style guard (AGENTS §5 idiomatic Rust, §6 comments/naming).
#
# A compiler-free guard for the house rules that no existing tool checks:
# `cargo fmt` and `clippy -D warnings` do not see bean accessors, the
# `&String`/`&Vec<T>` parameter shape, a decision number quoted in a comment,
# or a flat glob re-export.  It mirrors scripts/check-error-handling.sh: it
# scans the tree, prints every offending file:line with its rule, and exits
# non-zero unless the site has a reviewed entry in scripts/house_style_allow.txt.
#
# Checked rules
# -------------
#   bean-accessor        a `get_*`/`set_*`/`is_*` method whose body only moves a
#                        field: `self.field = arg;`, `self.field`,
#                        `&self.field`, `&mut self.field`, or `Ok(self.field)`.
#                        AGENTS §5: such a setter must be a public field and
#                        its getter deleted.  Only `get_`/`set_`/`is_` prefixed
#                        methods are considered; a body that does real work is
#                        not flagged.
#   ref-collection-param an immutable `&String`, `&Vec<T>`, `&Box<T>` or
#                        `&PathBuf` in a function parameter list.  AGENTS §5:
#                        use `&str`, `&[T]`, `&T`, `&Path`.  `&mut Vec<T>` /
#                        `&mut String` are not flagged: a growable mutable
#                        buffer cannot always be a `&mut [T]` / `&mut str`.
#   decision-number      a decision number (D1–D37) in a code or doc comment.
#                        AGENTS §6 / D31 ban them; history is read with git.
#                        Test files and `#[cfg(test)]` items are exempt.
#   glob-reexport        `pub use crate::…::*;` or `pub use <crate>::…::*;`.
#                        AGENTS §5: a flat glob re-export of the whole crate is
#                        not the default; freeze only the intended contract.
#                        `self::` / `super::` module globs and non-`pub` globs
#                        are not flagged.
#   jargon-in-log        `L1`–`L4` or a decision number inside a string literal
#                        passed to a `log::*!` / `println!` / `print!` family
#                        macro.  AGENTS §6 bans internal jargon in printed or
#                        logged text.
#
# A syntax-level scan cannot decide whether an accessor "does real work",
# whether a re-export is deliberate, or whether a private-field getter is the
# intended read API, so the guard over-reports on those shapes and every
# accepted site is justified in the allowlist.  A missed house-style regression
# is worse than a reviewed exception.
#
# Allowlist: scripts/house_style_allow.txt
#   one entry per line:  path:line:pattern — reason
#   `pattern` is either a rule name above (matched together with `path:line`)
#   or a code anchor, a substring of the offending source line (matched
#   together with `path` only, so the entry survives line churn).  `#` starts
#   a comment line.  An entry that no longer matches any finding is reported
#   as stale.
#
# Usage: scripts/check-house-style.sh [--print-all] [--self-test]
#   --print-all   list every finding without allowlist filtering (seeding aid)
#   --self-test   run only the guard's own fixture self-test and exit

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT" || exit 1

ALLOW="scripts/house_style_allow.txt"

exec python3 - "$ALLOW" "$@" <<'PY'
import bisect
import re
import sys
from pathlib import Path

ROOT = Path.cwd()
ALLOW = Path(sys.argv[1])
ARGS = set(sys.argv[2:])
PRINT_ALL = "--print-all" in ARGS

CRATES = ROOT / "crates"
FIXTURES = ROOT / "scripts" / "house_style_fixtures"
BAD_FIXTURE = "scripts/house_style_fixtures/bad_style.rs"
GOOD_FIXTURE = "scripts/house_style_fixtures/good_style.rs"


def scan_spans(src):
    """Yield (kind, start, end) for comment and string/char literal spans.

    `kind` is "comment", "str" or "char".  The spans are byte offsets into
    `src`; a span keeps its newlines so positions stay valid.  A regex alone
    would treat `// pub use …` or a `"&String"` string as code.
    """
    n = len(src)
    i = 0
    while i < n:
        c = src[i]
        if c == "/" and i + 1 < n and src[i + 1] == "/":
            j = i + 2
            while j < n and src[j] != "\n":
                j += 1
            yield ("comment", i, j)
            i = j
        elif c == "/" and i + 1 < n and src[i + 1] == "*":
            depth = 1
            j = i + 2
            while j < n and depth:
                if src[j] == "/" and j + 1 < n and src[j + 1] == "*":
                    depth += 1
                    j += 2
                elif src[j] == "*" and j + 1 < n and src[j + 1] == "/":
                    depth -= 1
                    j += 2
                else:
                    j += 1
            yield ("comment", i, j)
            i = j
        elif c == "r" and i + 1 < n and (src[i + 1] == '"' or src[i + 1] == "#"):
            m = re.match(r'r(#*)"', src[i:])
            if m:
                hashes = m.group(1)
                start = i + len(m.group(0))
                end = src.find('"' + hashes, start)
                if end == -1:
                    yield ("str", i, n)
                    i = n
                else:
                    yield ("str", i, end + len(hashes) + 1)
                    i = end + len(hashes) + 1
            else:
                i += 1
        elif c == '"':
            j = i + 1
            while j < n:
                if src[j] == "\\":
                    j += 2
                elif src[j] == '"':
                    j += 1
                    break
                else:
                    j += 1
            yield ("str", i, j)
            i = j
        elif c == "'":
            m = re.match(
                r"'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F_]+\}|.)|[^'\\])'", src[i:]
            )
            if m:
                yield ("char", i, i + len(m.group(0)))
                i += len(m.group(0))
            else:
                i += 1
        else:
            i += 1


def mask(src):
    """Blank comment and literal contents, keeping length and newlines.

    The result has the same length as `src`, so byte offsets and line numbers
    stay valid, and braces inside literals can no longer desynchronise the
    `#[cfg(test)]` brace matcher.
    """
    out = list(src)
    for _kind, a, b in scan_spans(src):
        for k in range(a, b):
            if out[k] != "\n":
                out[k] = " "
    return "".join(out)


def test_ranges(masked):
    """Character ranges of every `#[cfg(test)]` item."""
    ranges = []
    n = len(masked)
    for m in re.finditer(r"#\[cfg\(test\)\]", masked):
        j = m.end()
        while j < n and masked[j] not in "{;":
            j += 1
        if j >= n:
            continue
        if masked[j] == ";":
            ranges.append((m.start(), j))
            continue
        depth = 0
        k = j
        while k < n:
            if masked[k] == "{":
                depth += 1
            elif masked[k] == "}":
                depth -= 1
                if depth == 0:
                    break
            k += 1
        ranges.append((m.start(), k + 1))
    return ranges


def in_ranges(pos, ranges):
    return any(a <= pos < b for a, b in ranges)


TOKEN_RE = re.compile(
    r"[A-Za-z_][A-Za-z0-9_]*"
    r"|0[xX][0-9a-fA-F_]+"
    r"|\d[\d_]*(?:\.\d+)?(?:[eE][+-]?\d+)?"
    r"|::|->|=>|\.\.=?|\|\||&&|==|!=|<=|>=|<<=|>>=|\+=|-=|\*=|/=|%=|&=|\|=|\^=|<<|>>"
    r"|[+\-*/%&|^!~<>=?.,;:()\[\]{}@#']"
)
IDENT_RE = re.compile(r"^[A-Za-z_]\w*$")


def tokenize(text):
    return [(m.group(0), m.start()) for m in TOKEN_RE.finditer(text)]


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


def scan_file(rel, src):
    findings = []
    orig = src.splitlines()
    masked = mask(src)
    ranges = test_ranges(masked)
    spans = list(scan_spans(src))
    lines = masked.splitlines()
    offsets = []
    acc = 0
    for line in lines:
        offsets.append(acc)
        acc += len(line) + 1

    def add(pos, rule):
        ln = bisect.bisect_right(offsets, pos)
        ln = max(1, min(ln, len(orig)))
        code = orig[ln - 1].strip() if ln - 1 < len(orig) else ""
        findings.append((rel, ln, rule, code))

    # Test code is exempt from the accessor, parameter, jargon and decision
    # rules; a test-local glob is not part of any crate's contract either.
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
        for m in DEC_RE.finditer(src[a:b]):
            add(a + m.start(), "decision-number")

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
    findings = []
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

    seen = set()
    unique = []
    for rel, line, rule, code in findings:
        key = (rel, line, rule)
        if key in seen:
            continue
        seen.add(key)
        unique.append((rel, line, rule, code))
    return unique


RULES = {
    "bean-accessor",
    "ref-collection-param",
    "decision-number",
    "glob-reexport",
    "jargon-in-log",
}
SELF_TEST_BAD_RULES = tuple(sorted(RULES))


def normalize(text):
    return " ".join(text.split())


def load_allowlist():
    """Return a list of (path, line, pattern, reason) allowlist entries."""
    entries = []
    if not ALLOW.exists():
        return entries
    for raw in ALLOW.read_text(encoding="utf-8").splitlines():
        text = raw.strip()
        if not text or text.startswith("#"):
            continue
        key, sep, reason = text.partition("\u2014")  # em dash
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


def entry_matches(entry, rel, line, rule, code):
    path, line_no, pattern, _reason = entry
    if path != rel:
        return False
    if pattern in RULES:
        return pattern == rule and line_no == line
    # Code anchor: survives line shifts, fails closed when the code changes.
    return normalize(pattern) in normalize(code)


def self_test():
    """Assert every rule matches the bad fixture and none matches the good one.

    A regex that silently stops matching is worse than no guard, so the guard
    runs this on every invocation.
    """
    if not FIXTURES.is_dir():
        print(f"ERROR: guard self-test fixtures missing at {FIXTURES}")
        return 1
    findings = scan([FIXTURES])
    by_file = {}
    for rel, line, rule, code in findings:
        by_file.setdefault(rel, []).append((line, rule, code))
    bad_rules = {rule for _line, rule, _code in by_file.get(BAD_FIXTURE, [])}
    good = by_file.get(GOOD_FIXTURE, [])
    problems = []
    for rule in SELF_TEST_BAD_RULES:
        if rule not in bad_rules:
            problems.append(f"{BAD_FIXTURE}: expected a `{rule}` finding, matched none")
    for line, rule, code in good:
        problems.append(f"{GOOD_FIXTURE}:{line}: unexpected `{rule}` finding: {code}")
    if problems:
        print("ERROR: guard self-test failed:")
        for problem in problems:
            print(f"  {problem}")
        print("  Fix the rule that stopped matching; do not silence the self-test.")
        return 1
    print(
        "house-style guard: self-test ok "
        f"({len(SELF_TEST_BAD_RULES)} rules flagged the bad fixture, "
        f"{len(by_file.get(GOOD_FIXTURE, []))} finding(s) in the good fixture)"
    )
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
    unallowed = []
    used = [False] * len(allow)
    for rel, line, rule, code in findings:
        hit = False
        for i, entry in enumerate(allow):
            if entry_matches(entry, rel, line, rule, code):
                used[i] = True
                hit = True
        if not hit:
            unallowed.append((rel, line, rule, code))

    for rel, line, rule, code in unallowed:
        print(f"ERROR: house-style violation [{rule}] {rel}:{line}: {code}")

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
            f"ERROR: {len(unallowed)} unallowlisted house-style violation(s) "
            f"(AGENTS §5/§6); {len(findings)} site(s) checked, {len(allow)} "
            "allowlist entry/entries."
        )
        print("  Either fix the site or add a reviewed entry to")
        print(f"  {ALLOW}")
        print("  Format: path:line:pattern — reason  (pattern is a rule name")
        print("  or a source-line anchor)")
        return 1

    print(
        f"house-style guard: clean ({len(findings)} site(s) checked, "
        f"{len(allow)} allowlisted) — {ALLOW}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
PY
