"""Shared mechanics for the gate checks.

Every check owns its policy — the rules it looks for, the allowlist it reads,
the wording it prints — and this module owns the mechanics they otherwise
re-implement: the Rust lexical scanners, the allowlist file format, finding
filtering, stale-entry reporting and the fixture self-test helpers.
"""

from __future__ import annotations

import bisect
import re
from collections.abc import Callable, Iterable, Iterator, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path

__all__ = [
    "AllowEntry",
    "FixtureRules",
    "Finding",
    "IDENT_RE",
    "TOKEN_RE",
    "anchor_in",
    "blank_cfg_test",
    "blank_ranges",
    "clean_fixture_problems",
    "dedupe_findings",
    "filter_findings",
    "find_block",
    "fixture_problems",
    "group_findings",
    "in_ranges",
    "line_offsets",
    "line_number",
    "literal_content",
    "load_keyed_allowlist",
    "load_path_line_allowlist",
    "mask",
    "match_delimiter",
    "normalize",
    "print_all_findings",
    "print_stale",
    "read_allowlist_entries",
    "report_self_test_failure",
    "scan_spans",
    "stale_keys",
    "strip_cpp_comments",
    "strip_rust",
    "test_ranges",
    "tokenize",
    "write_keyed_allowlist",
]


# ---------------------------------------------------------------------------
# Findings
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Finding:
    """One check finding.

    ``path`` is repo-relative, ``line`` is 1-based, ``rule`` is the check's
    stable rule name, ``anchor`` is the offending source text the allowlist
    anchor compares against, and ``message`` carries any extra detail a check
    wants to report.
    """

    path: str
    line: int
    rule: str
    anchor: str = ""
    message: str = ""

    @property
    def location(self) -> str:
        """``path:line``, the site the finding was raised at."""
        return f"{self.path}:{self.line}"


def dedupe_findings(findings: Iterable[Finding]) -> list[Finding]:
    """Keep the first finding for every ``(path, line, rule)``."""
    seen: set[tuple[str, int, str]] = set()
    unique: list[Finding] = []
    for finding in findings:
        key = (finding.path, finding.line, finding.rule)
        if key in seen:
            continue
        seen.add(key)
        unique.append(finding)
    return unique


# ---------------------------------------------------------------------------
# Rust lexical spans
# ---------------------------------------------------------------------------


def scan_spans(src: str) -> Iterator[tuple[str, int, int]]:
    """Yield ``(kind, start, end)`` for comment and string/char literal spans.

    ``kind`` is ``"comment"``, ``"str"`` or ``"char"``.  The spans are byte
    offsets into ``src`` and keep their newlines, so positions stay valid.  A
    regex alone would treat ``// pub use …`` or a ``"&String"`` string as code.
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


def mask(src: str) -> str:
    """Blank comment and literal contents, keeping length and newlines.

    The result has the same length as ``src``, so byte offsets and line numbers
    stay valid, and braces inside literals can no longer desynchronise the
    ``#[cfg(test)]`` brace matcher.
    """
    out = list(src)
    for _kind, a, b in scan_spans(src):
        for k in range(a, b):
            if out[k] != "\n":
                out[k] = " "
    return "".join(out)


def literal_content(span: str) -> str | None:
    """The content of a string-literal span, or ``None`` for another kind.

    ``span`` is the literal including its ``r``/``r#`` prefix and its quotes; a
    byte or C string's leading ``b``/``c`` sits outside the span.
    """
    m = re.match(r'^r(#*)"', span)
    if m:
        return span[m.end() :]
    m = re.match(r'^"', span)
    if m:
        return span[m.end() :]
    return None


def test_ranges(masked: str) -> list[tuple[int, int]]:
    """Character ranges of every ``#[cfg(test)]`` item."""
    ranges: list[tuple[int, int]] = []
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


def in_ranges(pos: int, ranges: Sequence[tuple[int, int]]) -> bool:
    """Whether ``pos`` falls inside one of the ``(start, end)`` ranges."""
    return any(a <= pos < b for a, b in ranges)


def blank_ranges(masked: str, ranges: Iterable[tuple[int, int]]) -> str:
    """Blank ``#[cfg(test)]`` items but keep newlines, so braces stay balanced."""
    chars = list(masked)
    for a, b in ranges:
        for k in range(a, min(b, len(chars))):
            if chars[k] != "\n":
                chars[k] = " "
    return "".join(chars)


TOKEN_RE = re.compile(
    r"[A-Za-z_][A-Za-z0-9_]*"
    r"|0[xX][0-9a-fA-F_]+"
    r"|\d[\d_]*(?:\.\d+)?(?:[eE][+-]?\d+)?"
    r"|::|->|=>|\.\.=?|\|\||&&|==|!=|<=|>=|<<=|>>=|\+=|-=|\*=|/=|%=|&=|\|=|\^=|<<|>>"
    r"|[+\-*/%&|^!~<>=?.,;:()\[\]{}@#']"
)
IDENT_RE = re.compile(r"^[A-Za-z_]\w*$")


def tokenize(text: str) -> list[tuple[str, int]]:
    """Return ``(token, offset)`` pairs for a masked Rust source."""
    return [(m.group(0), m.start()) for m in TOKEN_RE.finditer(text)]


def match_delimiter(text: str, open_idx: int, opener: str, closer: str) -> int:
    """Index of the ``closer`` matching the ``opener`` at ``open_idx``, or ``-1``."""
    depth = 0
    i = open_idx
    n = len(text)
    while i < n:
        c = text[i]
        if c == opener:
            depth += 1
        elif c == closer:
            depth -= 1
            if depth == 0:
                return i
        i += 1
    return -1


def find_block(text: str, open_pos: int) -> tuple[int, int] | None:
    """Body offsets of the brace block whose opening ``{`` is at ``open_pos``.

    The returned ``(start, end)`` exclude the braces; an unterminated block
    returns ``None``.
    """
    close = match_delimiter(text, open_pos, "{", "}")
    if close < 0:
        return None
    return open_pos + 1, close


def line_offsets(text: str) -> list[int]:
    """Byte offset of every line in ``text`` (the inverse of a line lookup)."""
    offsets: list[int] = []
    acc = 0
    for line in text.splitlines():
        offsets.append(acc)
        acc += len(line) + 1
    return offsets


def line_number(offsets: Sequence[int], pos: int) -> int:
    """1-based line number for a byte offset, from a per-line offset table."""
    return max(1, bisect.bisect_right(offsets, pos))


def normalize(text: str) -> str:
    """Collapse every whitespace run to one space."""
    return " ".join(text.split())


def anchor_in(pattern: str, text: str) -> bool:
    """Whether a normalized allowlist anchor appears in a normalized line."""
    return normalize(pattern) in normalize(text)


def strip_rust(text: str) -> str:
    """Blank comments and string/char literals, preserving offsets and lines.

    This is the byte-string-aware variant used by the host-facts and
    storage-boundary scans: it blanks raw/byte strings and never looks inside a
    literal for code.  The result has the same length as ``text`` so match
    positions map straight back to the original line numbers.
    """
    out = list(text)
    n = len(text)
    i = 0
    while i < n:
        c = text[i]
        if c == "/" and i + 1 < n and text[i + 1] == "/":
            j = text.find("\n", i)
            if j < 0:
                j = n
            _blank(out, i, j)
            i = j
        elif c == "/" and i + 1 < n and text[i + 1] == "*":
            depth = 1
            j = i + 2
            while j < n and depth:
                if text[j] == "/" and j + 1 < n and text[j + 1] == "*":
                    depth += 1
                    j += 2
                elif text[j] == "*" and j + 1 < n and text[j + 1] == "/":
                    depth -= 1
                    j += 2
                else:
                    j += 1
            _blank(out, i, j)
            i = j
        elif c == '"':
            j = i + 1
            while j < n:
                if text[j] == "\\":
                    j += 2
                elif text[j] == '"':
                    j += 1
                    break
                else:
                    j += 1
            _blank(out, i, j)
            i = j
        elif c == "r" or (c == "b" and i + 1 < n and text[i + 1] in "#r"):
            raw = re.match(r'(?:br|rb|r)(#*)"', text[i:])
            if raw is not None:
                hashes = raw.group(1)
                term = '"' + hashes
                end = text.find(term, i + raw.end())
                end = n if end < 0 else end + len(term)
                _blank(out, i, end)
                i = end
            else:
                i += 1
        elif c == "'":
            k = text.find("'", i + 1)
            if text[i + 1 : i + 2] == "\\":
                k = text.find("'", i + 2)
            if 0 < k and k - i <= 4:
                _blank(out, i, k + 1)
                i = k + 1
            else:
                i += 1
        else:
            i += 1
    return "".join(out)


def _blank(chars: list[str], start: int, end: int) -> None:
    for i in range(start, min(end, len(chars))):
        if chars[i] != "\n":
            chars[i] = " "


_CFG_TEST_RE = re.compile(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]")


def blank_cfg_test(text: str) -> str:
    """Blank the item a ``#[cfg(test)]`` attribute guards.

    A test-only caller must not count as production use, whether it is an
    inline ``mod tests { ... }`` block or a single ``#[cfg(test)] fn``.
    """
    out = list(text)
    for match in _CFG_TEST_RE.finditer(text):
        j = match.end()
        while j < len(text) and text[j] in " \t\r\n":
            j += 1
        depth = 0
        started = False
        k = j
        while k < len(text):
            c = text[k]
            if c == "{":
                started = True
                depth += 1
            elif c == "}":
                depth -= 1
                if started and depth == 0:
                    k += 1
                    break
            elif c == ";" and not started:
                k += 1
                break
            k += 1
        _blank(out, match.start(), k)
    return "".join(out)


def strip_cpp_comments(src: str) -> str:
    """Replace C/C++ comments with spaces, preserving offsets and newlines."""
    out: list[str] = []
    i = 0
    n = len(src)
    while i < n:
        c = src[i]
        if c == "/" and i + 1 < n and src[i + 1] == "/":
            j = src.find("\n", i)
            if j == -1:
                j = n
            out.append(" " * (j - i))
            i = j
        elif c == "/" and i + 1 < n and src[i + 1] == "*":
            j = src.find("*/", i + 2)
            j = n if j == -1 else j + 2
            out.append(re.sub(r"[^\n]", " ", src[i:j]))
            i = j
        else:
            out.append(c)
            i += 1
    return "".join(out)


# ---------------------------------------------------------------------------
# Allowlists
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class AllowEntry:
    """One ``path:line:pattern — reason`` allowlist entry."""

    path: str
    line: int
    pattern: str
    reason: str


def read_allowlist_entries(
    path: Path, separators: Sequence[str] = ("\u2014", " -- ")
) -> Iterator[tuple[str, str]]:
    """Yield ``(key, reason)`` for every non-comment allowlist line.

    A reason follows the first of ``separators``; a bare key gets an empty
    reason.  The separator choice is the check's format policy.
    """
    if not path.exists():
        return
    for raw in path.read_text(encoding="utf-8").splitlines():
        text = raw.strip()
        if not text or text.startswith("#"):
            continue
        for sep in separators:
            if sep in text:
                key, _, reason = text.partition(sep)
                yield key.strip(), reason.strip()
                break
        else:
            yield text, ""


def load_path_line_allowlist(path: Path, line_default: int = -1) -> list[AllowEntry]:
    """Load a ``path:line:pattern — reason`` allowlist.

    A malformed line number becomes ``line_default``; the check decides whether
    an unknown line means "any line" (``0``) or "no match" (``-1``).
    """
    entries: list[AllowEntry] = []
    for key, reason in read_allowlist_entries(path):
        parts = key.split(":", 2)
        if len(parts) != 3:
            continue
        entry_path, line_text, pattern = (
            parts[0].strip(),
            parts[1].strip(),
            parts[2].strip(),
        )
        try:
            line = int(line_text)
        except ValueError:
            line = line_default
        entries.append(AllowEntry(entry_path, line, pattern, reason))
    return entries


def load_keyed_allowlist(
    path: Path, separators: Sequence[str] = ("\u2014", " -- ")
) -> dict[str, str]:
    """Load a ``key — reason`` allowlist as an ordered ``{key: reason}`` map."""
    return dict(read_allowlist_entries(path, separators))


def write_keyed_allowlist(
    path: Path,
    header: str,
    keys: Iterable[str],
    reasons: Mapping[str, str],
    separator: str = " -- ",
) -> None:
    """Write a ``key <separator> reason`` allowlist with a check-owned header."""
    lines: list[str] = []
    for key in sorted(keys):
        reason = (reasons.get(key) or "").strip()
        lines.append(f"{key}{separator}{reason}" if reason else key)
    body = "\n".join(lines)
    path.write_text(header + body + ("\n" if body else ""), encoding="utf-8")


def filter_findings(
    findings: Iterable[Finding],
    entries: Sequence[object],
    matches: Callable[[object, Finding], bool],
    *,
    first_match: bool = False,
) -> tuple[list[Finding], list[bool]]:
    """Split findings into unallowed ones plus the used-entry mask.

    ``matches`` is the check's match policy.  By default every matching entry
    is marked used, so a duplicate entry is not reported stale; ``first_match``
    stops at the first match and is for the checks whose loops did.
    """
    used = [False] * len(entries)
    unallowed: list[Finding] = []
    for finding in findings:
        hit = False
        for i, entry in enumerate(entries):
            if matches(entry, finding):
                used[i] = True
                hit = True
                if first_match:
                    break
        if not hit:
            unallowed.append(finding)
    return unallowed, used


def stale_keys(
    entries: Sequence[object], used: Sequence[bool], key: Callable[[object], str]
) -> list[str]:
    """The keys of the entries that matched no finding."""
    return [key(entry) for i, entry in enumerate(entries) if not used[i]]


def print_stale(keys: Sequence[str], note: str | None = None) -> None:
    """Print the shared stale-entry note, sorted, or nothing when empty."""
    if not keys:
        return
    if note is None:
        note = f"note: {len(keys)} allowlist entr(y/ies) no longer match (stale):"
    print("")
    print(note)
    for key in sorted(keys):
        print(f"  {key}")


# ---------------------------------------------------------------------------
# Fixture self-tests
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class FixtureRules:
    """A bad fixture that must trip every rule, and clean near-misses."""

    bad: str
    rules: tuple[str, ...]
    good: tuple[str, ...] = ()


def group_findings(findings: Iterable[Finding]) -> dict[str, list[Finding]]:
    """Group findings by their path."""
    by_path: dict[str, list[Finding]] = {}
    for finding in findings:
        by_path.setdefault(finding.path, []).append(finding)
    return by_path


def fixture_problems(
    findings: Iterable[Finding], expectations: Iterable[FixtureRules]
) -> list[str]:
    """Standard problems for a bad-fixture/good-fixture expectation set.

    Each rule must appear on its bad fixture and no finding may appear on a
    near-miss fixture; the messages match the guards' existing self-tests.
    """
    by_path = group_findings(findings)
    problems: list[str] = []
    for expectation in expectations:
        bad_rules = {finding.rule for finding in by_path.get(expectation.bad, ())}
        for rule in expectation.rules:
            if rule not in bad_rules:
                problems.append(
                    f"{expectation.bad}: expected a `{rule}` finding, matched none"
                )
        problems.extend(clean_fixture_problems(findings, expectation.good))
    return problems


def clean_fixture_problems(
    findings: Iterable[Finding], fixtures: Iterable[str]
) -> list[str]:
    """Problems for each finding raised on a fixture that must stay clean."""
    by_path = group_findings(findings)
    problems: list[str] = []
    for rel in fixtures:
        for finding in by_path.get(rel, ()):
            problems.append(
                f"{rel}:{finding.line}: unexpected `{finding.rule}` finding: "
                f"{finding.anchor}"
            )
    return problems


def report_self_test_failure(
    header: str,
    problems: Sequence[str],
    fix: str = "  Fix the rule that stopped matching; do not silence the self-test.",
) -> None:
    """Print the shared self-test failure block."""
    print(f"{header}:")
    for problem in problems:
        print(f"  {problem}")
    print(fix)


# ---------------------------------------------------------------------------
# Reporting
# ---------------------------------------------------------------------------


def print_all_findings(
    findings: Sequence[Finding],
    format_line: Callable[[Finding], str] | None = None,
) -> None:
    """Print every finding, then the shared ``--print-all`` footer."""
    if format_line is None:

        def format_line(finding: Finding) -> str:
            return f"{finding.path}:{finding.line}:{finding.rule}: {finding.anchor}"

    for finding in findings:
        print(format_line(finding))
    print(f"\n{len(findings)} finding(s) total (allowlist not applied)")
