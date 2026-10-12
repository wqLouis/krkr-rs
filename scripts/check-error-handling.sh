#!/usr/bin/env bash
# Error-handling guard (AGENTS §4: errors, fallbacks and "slop").
#
# A compiler-free guard for the silent-fallback classes AGENTS §4 bans.  It
# mirrors scripts/check-stateless.sh: it scans the tree, prints every offending
# file:line with its pattern and rule, and exits non-zero unless the site has a
# reviewed entry in scripts/error_handling_allow.txt.
#
# Checked rules
# -------------
#   parse-unwrap     `.parse(..)` immediately feeding `.unwrap_or*` — the TJS
#                    string->number coercion bug; the reference parses a
#                    prefix, Rust `parse` rejects the whole string.
#   result-fallback  `.unwrap_or` / `.unwrap_or_default` / `.unwrap_or_else`
#                    whose receiver chain calls a known-fallible operation.
#   let-underscore   `let _ = <fallible call>;` with no `?` (the error is
#                    dropped, not propagated).
#   ok-discard       `.ok()` whose receiver calls a known-fallible operation.
#   iflet-ok         `if let Ok(..)` over a known-fallible operation.
#   debug-assert     `debug_assert*!` outside `#[cfg(test)]` — a data-integrity
#                    condition must be a typed error, not a release no-op.
#   non-exhaustive   `#[non_exhaustive]` under crates/compat — compat matches
#                    its enums variant-by-variant, so a new variant must be a
#                    compile error.
#   allow-dead-code  `#[allow(dead_code)]` outside the one documented
#                    exception (crates/compat/tests/common/mod.rs).
#
# A grep cannot decide whether a receiver is a `Result` or an `Option`, so the
# guard over-reports on the known-fallible method names and every accepted site
# is justified in the allowlist.  A missed silent fallback is worse than a
# reviewed exception.
#
# Allowlist: scripts/error_handling_allow.txt
#   one entry per line:  path:line:pattern — reason
#   `pattern` is either a rule name above (matched together with `path:line`)
#   or a code anchor, a substring of the offending source line (matched
#   together with `path` only, so the entry survives line churn).  `#` starts
#   a comment line.  An entry that no longer matches any finding is reported
#   as stale.
#
# The scan runs in python3 (already used by the repo) because a correct guard
# must ignore comments, string/char literals and `#[cfg(test)]` items, and must
# follow a method chain across lines (`.lock()\n.unwrap_or_else(..)`).
#
# Usage: scripts/check-error-handling.sh [--print-all]
#   --print-all   list every finding without allowlist filtering (seeding aid)

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT" || exit 1

ALLOW="scripts/error_handling_allow.txt"

exec python3 - "$ALLOW" "$@" <<'PY'
import re
import sys
from pathlib import Path

ROOT = Path.cwd()
ALLOW = Path(sys.argv[1])
ARGS = set(sys.argv[2:])
PRINT_ALL = "--print-all" in ARGS

CRATES = ROOT / "crates"
# The one documented `#[allow(dead_code)]` exception (testing helper).
DEAD_CODE_EXCEPTION = "crates/compat/tests/common/mod.rs"

# Known-fallible calls.  These names return `Result` (or a `Result`-like
# error path); a fallback on them is a candidate §4 violation.  `format.i32`
# (MediaFormat) and `str::strip_prefix` are `Option` and are handled by the
# allowlist rather than removed from the list, so nothing is silently ignored.
FALLIBLE = re.compile(
    r"(metadata\(|read_dir\(|CString::new\(|\.status\(|\.send_eof\("
    r"|\.poll\(|decode|inflate|\.lock\(|\.wait\(|\.strip_prefix\(|\.absolute\("
    r"|\.i32\(|\.i16\(|\.u8\(|\.u16\(|\.u32\(|\.u64\(|\.unicode_string\("
    r"|\.key4\(|\btry_from\(|\btry_into\(|\.integer\(|\.take\("
    r"|\.read\(|\.write\(|\.parse\()"
)
# `.ok()?` on a `try_from`/`try_into` is the out-of-range => `None` contract
# (AGENTS §4 list calls this legitimate), so those two are not in the `.ok()`
# detector.  Everything else matches FALLIBLE.
FALLIBLE_OK = re.compile(
    r"(metadata\(|read_dir\(|CString::new\(|\.status\(|\.send_eof\("
    r"|\.poll\(|decode|inflate|\.lock\(|\.wait\(|\.strip_prefix\(|\.absolute\("
    r"|\.i32\(|\.i16\(|\.u8\(|\.u16\(|\.u32\(|\.u64\(|\.unicode_string\("
    r"|\.key4\(|\.integer\(|\.take\(|\.read\(|\.write\(|\.parse\()"
)
# Fallible calls whose discarded result is worth flagging under `let _ =`.
FALLIBLE_LET = re.compile(
    r"(set_member|set_call_missing|set_default_member|resolve_face"
    r"|update_layer|\.poll\(|\.stop\(|delete_member|send_eof|call_member)"
)

UNWRAP = re.compile(r"\.unwrap_or")
PARSE = re.compile(r"\.parse(::<[^>]*>)?\(\)")
OK_CALL = re.compile(r"\.ok\(\)")
IFLET_OK = re.compile(r"if let Ok\(")
LET_UND = re.compile(r"let _ = ")
DEBUG_ASSERT = re.compile(r"debug_assert(_eq|_ne)?!")
NON_EXHAUSTIVE = re.compile(r"#\[non_exhaustive\]")
DEAD_CODE = re.compile(r"#!?\[allow\(dead_code\)\]")


def mask(src):
    """Blank comments and string/char literal *contents*, keeping newlines.

    The result has the same length as `src`, so byte offsets and line numbers
    stay valid.  Braces inside literals can no longer desynchronise the
    `#[cfg(test)]` brace matcher.
    """
    out = list(src)
    n = len(src)
    i = 0

    def blank(a, b):
        for k in range(a, b):
            if out[k] != "\n":
                out[k] = " "

    while i < n:
        c = src[i]
        if c == "/" and i + 1 < n and src[i + 1] == "/":
            j = i + 2
            while j < n and src[j] != "\n":
                j += 1
            blank(i, j)
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
            blank(i, j)
            i = j
        elif c == "r" and i + 1 < n and (src[i + 1] == '"' or src[i + 1] == "#"):
            m = re.match(r'r(#*)"', src[i:])
            if m:
                hashes = m.group(1)
                start = i + len(m.group(0))
                end = src.find('"' + hashes, start)
                if end == -1:
                    blank(i, n)
                    i = n
                else:
                    blank(i, end + len(hashes) + 1)
                    i = end + len(hashes) + 1
                continue
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
            blank(i, j)
            i = j
        elif c == "'":
            m = re.match(
                r"'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F_]+\}|.)|[^'\\])'", src[i:]
            )
            if m:
                blank(i, i + len(m.group(0)))
                i += len(m.group(0))
            else:
                i += 1
        else:
            i += 1
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


def stmt_prefix(text, pos):
    """The expression text from the last statement boundary before `pos`.

    A statement boundary is `;`, `{` or `}`, which lets a receiver chain that
    spans lines (`.lock()\n.unwrap_or_else(..)`) still be inspected.
    """
    lo = 0
    for ch in ";{}":
        k = text.rfind(ch, 0, pos)
        if k > lo:
            lo = k
    return text[lo + 1 : pos]


def scan():
    findings = []
    for path in sorted(CRATES.rglob("*.rs")):
        rel = str(path.relative_to(ROOT))
        if "/tjs/cpp/" in rel:
            continue
        try:
            src = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        masked = mask(src)
        orig = src.splitlines()

        # `#[allow(dead_code)]` is banned everywhere outside the one
        # documented exception, including test and build files (D32).
        for idx, line in enumerate(masked.splitlines()):
            if DEAD_CODE.search(line) and rel != DEAD_CODE_EXCEPTION:
                code = orig[idx].strip() if idx < len(orig) else ""
                findings.append((rel, idx + 1, "allow-dead-code", code))

        if "/tests/" in rel or rel.endswith("build.rs"):
            continue
        ranges = test_ranges(masked)
        lines = masked.splitlines()
        offsets = []
        acc = 0
        for line in lines:
            offsets.append(acc)
            acc += len(line) + 1

        def add(line_no, rule, _masked_code):
            # Report the real source line; detection used the masked copy.
            code = orig[line_no - 1].strip() if line_no - 1 < len(orig) else ""
            findings.append((rel, line_no, rule, code))

        for idx, line in enumerate(lines):
            if not line.strip():
                continue
            base = offsets[idx]
            if in_ranges(base, ranges):
                continue
            stripped = line.strip()

            for m in UNWRAP.finditer(line):
                if in_ranges(base + m.start(), ranges):
                    continue
                pre = stmt_prefix(masked, base + m.start())
                if PARSE.search(pre):
                    add(idx + 1, "parse-unwrap", stripped)
                elif FALLIBLE.search(pre):
                    add(idx + 1, "result-fallback", stripped)

            if LET_UND.search(line) and "?" not in line and FALLIBLE_LET.search(line):
                add(idx + 1, "let-underscore", stripped)

            for m in OK_CALL.finditer(line):
                pre = stmt_prefix(masked, base + m.start())
                if FALLIBLE_OK.search(pre):
                    add(idx + 1, "ok-discard", stripped)

            if IFLET_OK.search(line):
                window = "\n".join(lines[idx : idx + 4])
                if FALLIBLE.search(window):
                    add(idx + 1, "iflet-ok", stripped)

            if DEBUG_ASSERT.search(line):
                add(idx + 1, "debug-assert", stripped)

            if NON_EXHAUSTIVE.search(line) and rel.startswith("crates/compat/"):
                add(idx + 1, "non-exhaustive", stripped)

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
    "parse-unwrap",
    "result-fallback",
    "let-underscore",
    "ok-discard",
    "iflet-ok",
    "debug-assert",
    "non-exhaustive",
    "allow-dead-code",
}


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


def main():
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
        print(f"ERROR: fallback/absence violation [{rule}] {rel}:{line}: {code}")

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
            f"ERROR: {len(unallowed)} unallowlisted fallback(s)/absence(s) "
            f"(AGENTS §4); {len(findings)} site(s) checked, {len(allow)} "
            "allowlist entry/entries."
        )
        print("  Either fix the fallback or add a reviewed entry to")
        print(f"  {ALLOW}")
        print("  Format: path:line:pattern — reason  (pattern is a rule name")
        print("  or a source-line anchor)")
        return 1

    print(
        f"error-handling guard: clean ({len(findings)} site(s) checked, "
        f"{len(allow)} allowlisted) — {ALLOW}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
PY
