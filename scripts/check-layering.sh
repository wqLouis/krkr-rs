#!/usr/bin/env bash
# Layering and unsafe guard (AGENTS §2: the four layers and the one allowed
# dependency direction; AGENTS §5: idiomatic Rust / the `unsafe` policy).
#
# A compiler-free guard for the layer boundaries and the `unsafe` rules.  It
# mirrors scripts/check-error-handling.sh: it scans the tree, ignores comments
# and string/char literals, skips `#[cfg(test)]` items and test files where the
# rule says so, prints every offending file:line with its rule, and exits
# non-zero unless the site has a reviewed entry in scripts/layering_allow.txt.
#
# Checked rules
# -------------
#   l1-vocabulary      `crates/platform/{api,fs,media,render,platform}/src/**`
#                      must not name TVP/TJS/KAG vocabulary in code.  A token
#                      that is also a legitimate L1 concept is not checked; see
#                      DROPPED TOKENS below.
#   l2-vocabulary      `crates/engine/**` must not name TVP/TJS vocabulary in
#                      code (the same tokens, plus the TVP `0..100000` volume
#                      scale literal).
#   l2-storage-name    `crates/engine/**` must not carry the TVP media/domain
#                      storage-name prefix.  A string literal whose content
#                      *begins* `file:` (which covers `file://`, `file://.`,
#                      and the single-slash `file:/…` form) names a TVP
#                      storage and belongs to L3.  The rule inspects string
#                      literals because the prefix only ever lives inside one;
#                      anchoring at the literal start keeps an ordinary
#                      `file:` inside a log message from matching, and a
#                      comment is never a literal.  `arc.xp3>path` addressing
#                      stays L2 and is deliberately not matched.
#   l3-platform        `crates/compat/src/**` must not contain platform code
#                      (`#[cfg(target_os`, `winit`, `android-activity`,
#                      `std::fs`, `jni`, `ndk`).  `crates/compat/tests/**` is
#                      deliberately excluded: tests build fixtures with
#                      `std::fs`, which is not the compat source's platform
#                      leak.  A test file is scanned by no rule that bans
#                      `std::fs`.
#   unsafe-no-safety-note  every `unsafe { … }` block or `unsafe fn` outside
#                      tests needs a `// SAFETY:` note adjacent to the block (or
#                      a `# Safety` doc on the fn).  The window is tight: the
#                      note must be on the same line, on the first line inside
#                      the block, or in the run of `//`/`#[…]` lines
#                      immediately above — never a note belonging to a
#                      neighbouring block.  `unsafe impl`/`unsafe extern`
#                      declarations are not checked (their contract is a
#                      `// SAFETY:` on the impl, which this rule cannot tell
#                      apart from the surrounding code).
#   unsafe-allow-outside-ffi  a file-level `#![allow(unsafe_code)]` is allowed
#                      only in the reviewed FFI/Vulkan locations listed in
#                      `FFI_UNSAFE_LOCATIONS` below (each with its reason);
#                      a file-level allow anywhere else is a finding.
#   missing-docs-lint  every `crates/**/src/lib.rs` enables
#                      `#![warn(missing_docs)]` (AGENTS §5).
#
# What the scan cannot decide
# ---------------------------
# A token can be TVP vocabulary or an unrelated identifier that merely matches
# the spelling.  The rule drops every token whose meaning a syntax scan cannot
# settle:
#   * `Layer`  — Vulkan calls its validation layers "layer" (`VK_LAYER_…`), so
#                the word is not TVP-specific in `render`.
#   * `VK_`    — Vulkan's own `VK_*` constants share the prefix with the TVP
#                virtual-key codes; it is only checked outside `render` (and in
#                L2), and only when it is a code identifier (the Vulkan uses
#                live in `CStr`/`format!` literals, which the mask blanks).
#   * `.sli` / `.tjs`  — the extensions only ever occur inside string literals,
#                and a literal is a near miss by design; the mask blanks it.
#   * KAG tag names — the tag words (`image`, `wait`, `eval`, …) are ordinary
#                English identifiers; deciding one is a KAG tag needs the
#                surrounding script, which the scan does not have.
#   * `Storage` — L1 and L2 have their own storage types; the token is not TVP.
#   * bare `tjs` — a local named `tjs` (a test variable, a file list) is
#                indistinguishable from the language name; only `TJS` and the
#                `.tjs` extension are unambiguous, and the extension is a string.
#   * `arc.xp3>path` — the VFS addressing is L2 (`06-file-system.md` §5), not
#                TVP storage naming; the storage-name rule matches only the
#                `file:` media prefix.
# kag/KAG is checked because it has no L1/L2 meaning at all.
#
# Allowlist: scripts/layering_allow.txt
#   one entry per line:  path:line:pattern — reason
#   `path` may contain shell globs (`*` matches `/` too), so a whole reviewed
#   location is one entry.  `line` is an integer, or `*`/`0` for any line.
#   `pattern` is either a rule name above (matched together with `path` and
#   `line`) or a code anchor, a substring of the offending source line (matched
#   with `path` only, so the entry survives line churn).  `#` starts a comment.
#   An entry that no longer matches any finding is reported as stale.
#
# Usage: scripts/check-layering.sh [--print-all] [--self-test]
#   --print-all   list every finding without allowlist filtering (seeding aid)
#   --self-test   run only the guard's own fixture self-test and exit
#
# The scan runs in python3 because a correct guard must ignore comments, string
# and char literals and `#[cfg(test)]` items, and must measure the note window
# per line.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT" || exit 1

ALLOW="scripts/layering_allow.txt"

exec python3 - "$ALLOW" "$@" <<'PY'
import fnmatch
import re
import sys
import time
from pathlib import Path

ROOT = Path.cwd()
ALLOW = Path(sys.argv[1])
ARGS = set(sys.argv[2:])
PRINT_ALL = "--print-all" in ARGS

CRATES = ROOT / "crates"
FIXTURES = ROOT / "scripts" / "layering_fixtures"

# L1 crates whose *source* must not name layer/TVP/TJS/KAG vocabulary.
L1_PREFIXES = (
    "crates/platform/api/src/",
    "crates/platform/fs/src/",
    "crates/platform/media/src/",
    "crates/platform/render/src/",
    "crates/platform/platform/src/",
)
# `VK_` is a TVP virtual-key code in L1, but Vulkan's own prefix in `render`;
# the render crate is the one place the token is not TVP-specific.
VK_EXEMPT_PREFIX = "crates/platform/render/src/"

# File-level `#![allow(unsafe_code)]` is legitimate only in these reviewed
# locations.  A new file-level allow anywhere else is a finding.
FFI_UNSAFE_LOCATIONS = (
    ("crates/compat/src/", "the native ABI seam: compat marshals the C ABI"),
    ("crates/platform/render/src/vulkan/", "the ash/Vulkan FFI boundary"),
    ("crates/vm/src/host.rs", "the tjs2 C ABI host (log callback, attach)"),
    ("crates/app/src/video.rs", "the video-decoder seam holds a storage pointer"),
)

# Code vocabulary.  Each entry is a stable label (the allowlist anchor reads
# the offending line, not the label) and the extended regex.
L1_TOKENS = (
    (re.compile(r"\bTVP_[A-Za-z0-9_]")),
    (re.compile(r"\bLT_[A-Za-z0-9_]")),
    (re.compile(r"\bTJS\b")),
    (re.compile(r"\bkag\b|\bKAG\b")),
)
L1_VK_TOKEN = re.compile(r"\bVK_[A-Za-z0-9_]")
L2_TOKENS = L1_TOKENS + (L1_VK_TOKEN, re.compile(r"\b100000\b"))
# The TVP media/domain storage-name prefix (`file://.`, `file://`, `file:`, and
# the single-slash `file:/…`).  The token scan runs over masked code, so a
# prefix that only ever lives inside a string literal is invisible to it; the
# l2-storage-name rule matches this against the *content* of a literal span.
# Anchoring at `^` keeps an ordinary `file:` inside a log sentence from
# matching, and a comment is never a literal span.
L2_STORAGE_NAME = re.compile(r"^file:")
L3_TOKENS = (
    (re.compile(r"#\[cfg\(\s*target_os")),
    (re.compile(r"\bwinit\b")),
    (re.compile(r"android-activity")),
    (re.compile(r"\bstd::fs\b")),
    (re.compile(r"\bjni\b")),
    (re.compile(r"\bndk\b")),
)

UNSAFE_BLOCK = re.compile(r"\bunsafe\s*\{")
UNSAFE_FN = re.compile(r"\bunsafe\s+(?:extern\s+\"[^\"]*\"\s+)?fn\b")
UNSAFE_ALLOW = re.compile(r"#!\[allow\(unsafe_code\)\]")
MISSING_DOCS = re.compile(r"#!\[warn\(missing_docs\)\]")

RULES = {
    "l1-vocabulary",
    "l2-vocabulary",
    "l2-storage-name",
    "l3-platform",
    "unsafe-no-safety-note",
    "unsafe-allow-outside-ffi",
    "missing-docs-lint",
}


def scan_spans(src):
    """Yield `(kind, start, end)` for every comment and string/char literal.

    `kind` is `"comment"`, `"str"` or `"char"`; the spans are byte offsets
    into `src` and keep their newlines, so positions stay valid.  `mask`
    blanks the spans and the storage-name rule needs the string spans
    themselves, so one tokenizer feeds both.
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
    """Blank comment and string/char literal contents, keeping newlines.

    The result has the same length as `src`, so byte offsets and line numbers
    stay valid.  Braces inside literals can no longer desynchronise the
    `#[cfg(test)]` brace matcher.
    """
    out = list(src)
    for _kind, a, b in scan_spans(src):
        for k in range(a, b):
            if out[k] != "\n":
                out[k] = " "
    return "".join(out)


def literal_content(span):
    """The content of a string-literal span, or `None` for another kind.

    `span` is the literal including its `r`/`r#` prefix and its quotes; a byte
    or C string's leading `b`/`c` sits outside the span.
    """
    m = re.match(r'^r(#*)"', span)
    if m:
        return span[m.end() :]
    m = re.match(r'^"', span)
    if m:
        return span[m.end() :]
    return None


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


def logical(rel):
    """The path from the crate root, so fixture trees reuse the real scopes.

    Both `crates/foo/src/x.rs` and
    `scripts/layering_fixtures/crates/foo/src/x.rs` reduce to
    `crates/foo/src/x.rs`.
    """
    i = rel.rfind("crates/")
    return rel[i:] if i >= 0 else rel


def line_of(offsets, pos):
    """1-based line number for a byte offset, from a per-line offset table."""
    lo, hi = 0, len(offsets)
    while lo < hi:
        mid = (lo + hi) // 2
        if offsets[mid] <= pos:
            lo = mid + 1
        else:
            hi = mid
    return max(1, lo)


def has_safety_note(orig, ln):
    """True when the `unsafe` on line `ln` (1-based) carries a SAFETY note.

    The note is accepted on the same line, on the first line inside the block,
    or in the contiguous run of comment/attribute lines immediately above.  The
    run stops at the first line that is not a `//` comment or an attribute, so
    a note belonging to a neighbouring block is never matched.
    """
    if ln - 1 < len(orig) and "SAFETY:" in orig[ln - 1]:
        return True
    if ln < len(orig):
        following = orig[ln].strip()
        if (following.startswith("//") or following.startswith("/*")) and (
            "SAFETY:" in following
        ):
            return True
    i = ln - 2
    while i >= 0:
        s = orig[i].strip()
        if s.startswith("//") or s.startswith("#["):
            if "SAFETY:" in s:
                return True
            i -= 1
            continue
        break
    return False


def has_fn_safety(orig, ln):
    """`unsafe fn` is documented either by a `# Safety` doc or a note."""
    if has_safety_note(orig, ln):
        return True
    i = ln - 2
    while i >= 0:
        s = orig[i].strip()
        if s.startswith("///") or s.startswith("//!") or s.startswith("#["):
            if "# Safety" in s:
                return True
            i -= 1
            continue
        break
    return False


def is_ffi_unsafe_location(log):
    return any(log.startswith(prefix) for prefix, _reason in FFI_UNSAFE_LOCATIONS)


def scan_text(rel, src):
    """All findings in one file, before allowlist filtering."""
    findings = []
    log = logical(rel)
    orig = src.splitlines()
    spans = list(scan_spans(src))
    masked = mask(src)
    ranges = test_ranges(masked)
    lines = masked.splitlines()
    offsets = []
    acc = 0
    for line in lines:
        offsets.append(acc)
        acc += len(line) + 1

    def add(line_no, rule):
        code = orig[line_no - 1].strip() if 0 <= line_no - 1 < len(orig) else ""
        findings.append((rel, line_no, rule, code))

    def tokens_scan(tokens, rule):
        for rx in tokens:
            for m in rx.finditer(masked):
                if in_ranges(m.start(), ranges):
                    continue
                add(line_of(offsets, m.start()), rule)

    is_test_path = "/tests/" in log
    if not is_test_path:
        if any(log.startswith(p) for p in L1_PREFIXES):
            tokens = L1_TOKENS
            if not log.startswith(VK_EXEMPT_PREFIX):
                tokens = tokens + (L1_VK_TOKEN,)
            tokens_scan(tokens, "l1-vocabulary")
        if log.startswith("crates/engine/"):
            tokens_scan(L2_TOKENS, "l2-vocabulary")
            for kind, a, b in spans:
                if kind != "str" or in_ranges(a, ranges):
                    continue
                body = literal_content(src[a:b])
                if body is not None and L2_STORAGE_NAME.match(body):
                    add(line_of(offsets, a), "l2-storage-name")
        if log.startswith("crates/compat/src/"):
            tokens_scan(L3_TOKENS, "l3-platform")

        for rx, check in ((UNSAFE_BLOCK, has_safety_note), (UNSAFE_FN, has_fn_safety)):
            for m in rx.finditer(masked):
                if in_ranges(m.start(), ranges):
                    continue
                ln = line_of(offsets, m.start())
                if not check(orig, ln):
                    add(ln, "unsafe-no-safety-note")

    # A file-level allow is a crate-wide FFI statement, so it is checked in
    # tests too; the reviewed locations are all non-test source files.
    for m in UNSAFE_ALLOW.finditer(masked):
        if in_ranges(m.start(), ranges):
            continue
        if not is_ffi_unsafe_location(log):
            add(line_of(offsets, m.start()), "unsafe-allow-outside-ffi")

    if log.endswith("/src/lib.rs") or log == "crates/lib.rs":
        if not MISSING_DOCS.search(masked):
            add(1, "missing-docs-lint")

    return findings


def scan(roots=None):
    findings = []
    root_dirs = [CRATES] if roots is None else list(roots)
    for root in root_dirs:
        if not root.is_dir():
            continue
        for path in sorted(root.rglob("*.rs")):
            rel = str(path.relative_to(ROOT))
            if "/tjs/cpp/" in rel or rel.endswith("build.rs"):
                continue
            try:
                src = path.read_text(encoding="utf-8", errors="replace")
            except OSError:
                continue
            findings.extend(scan_text(rel, src))

    seen = set()
    unique = []
    for rel, line, rule, code in findings:
        key = (rel, line, rule)
        if key in seen:
            continue
        seen.add(key)
        unique.append((rel, line, rule, code))
    return unique


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
            line_no = 0
        entries.append((path, line_no, pattern, reason.strip()))
    return entries


def entry_matches(entry, rel, line, rule, code):
    path, line_no, pattern, _reason = entry
    log = logical(rel)
    if not (fnmatch.fnmatch(log, path) or fnmatch.fnmatch(rel, path)):
        return False
    if pattern in RULES:
        if pattern != rule:
            return False
        return line_no == 0 or line_no == line
    # Code anchor: survives line shifts, fails closed when the code changes.
    return normalize(pattern) in normalize(code)


FIXTURES_CRATES = FIXTURES / "crates"
BAD_FIXTURES = {
    "crates/platform/api/src/bad_l1_vocabulary.rs": "l1-vocabulary",
    "crates/engine/src/bad_l2_vocabulary.rs": "l2-vocabulary",
    "crates/engine/src/bad_l2_storage_name.rs": "l2-storage-name",
    "crates/compat/src/bad_l3_platform.rs": "l3-platform",
    "crates/compat/src/bad_unsafe_note.rs": "unsafe-no-safety-note",
    "crates/engine/src/bad_unsafe_allow.rs": "unsafe-allow-outside-ffi",
    "crates/bad_crate/src/lib.rs": "missing-docs-lint",
}
GOOD_FIXTURES = (
    "scripts/layering_fixtures/crates/platform/api/src/good_l1.rs",
    "scripts/layering_fixtures/crates/engine/src/good_l2.rs",
    "scripts/layering_fixtures/crates/engine/src/good_l2_storage_name.rs",
    "scripts/layering_fixtures/crates/compat/src/good_l3.rs",
    "scripts/layering_fixtures/crates/compat/src/good_unsafe_note.rs",
    "scripts/layering_fixtures/crates/compat/src/good_allow.rs",
    "scripts/layering_fixtures/crates/compat/tests/good_std_fs.rs",
    "scripts/layering_fixtures/crates/good_crate/src/lib.rs",
)


def self_test():
    """Assert each rule still matches its bad fixture and no good fixture.

    A regex that silently stops matching is worse than no guard, so the guard
    runs this on every invocation and reports its runtime.
    """
    start = time.perf_counter()
    if not FIXTURES_CRATES.is_dir():
        print(f"ERROR: layering guard self-test fixtures missing at {FIXTURES_CRATES}")
        return 1
    findings = scan([FIXTURES_CRATES])
    by_file = {}
    for rel, line, rule, code in findings:
        by_file.setdefault(rel, []).append((line, rule, code))
    problems = []
    for fixture, rule in BAD_FIXTURES.items():
        rel = str((FIXTURES / fixture).relative_to(ROOT))
        rules = {r for _line, r, _code in by_file.get(rel, [])}
        if rule not in rules:
            problems.append(f"{rel}: expected a `{rule}` finding, matched none")
    for rel in GOOD_FIXTURES:
        for line, rule, code in by_file.get(rel, []):
            problems.append(f"{rel}:{line}: unexpected `{rule}` finding: {code}")
    elapsed = (time.perf_counter() - start) * 1000.0
    if problems:
        print("ERROR: layering guard self-test failed:")
        for problem in problems:
            print(f"  {problem}")
        print("  Fix the rule that stopped matching; do not silence the self-test.")
        return 1
    print(
        "layering guard: self-test ok "
        f"({len(BAD_FIXTURES)} rules flagged their bad fixture, "
        f"{len(GOOD_FIXTURES)} near-miss file(s) clean, {elapsed:.1f} ms)"
    )
    return 0


def main():
    selftest = self_test()
    if selftest != 0:
        return selftest
    if "--self-test" in ARGS:
        return 0

    start = time.perf_counter()
    findings = scan()
    elapsed = time.perf_counter() - start

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
        print(f"ERROR: layer/unsafe violation [{rule}] {rel}:{line}: {code}")

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
            f"ERROR: {len(unallowed)} unallowlisted layer/unsafe violation(s) "
            f"(AGENTS §2/§5); {len(findings)} site(s) checked, {len(allow)} "
            "allowlist entry/entries."
        )
        print("  Either fix the violation or add a reviewed entry to")
        print(f"  {ALLOW}")
        print("  Format: path:line:pattern — reason  (pattern is a rule name")
        print("  or a source-line anchor; line `*` matches any line)")
        return 1

    print(
        f"layering guard: clean ({len(findings)} site(s) checked, "
        f"{len(allow)} allowlisted, {elapsed * 1000.0:.1f} ms) — {ALLOW}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
PY
