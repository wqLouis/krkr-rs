#!/usr/bin/env python3
"""Rank Rust functions by size and flag native callbacks that look like stubs.

Motivation: a member can be present in the L3 surface lock while its callback
always raises (`report_error("... not exposed ...")`) or does nothing. The lock
counts *presence*, so those stubs pass the gate. This script lists functions
smallest-first so the lowest ones can be inspected one by one, and can emit a
self-contained Markdown report that embeds each candidate's source chunk,
location and registration line — so an agent reads one file instead of N.

Usage:
    python3 scripts/find_stubs.py [ROOT ...]        # default crates/compat/src
    python3 scripts/find_stubs.py --all             # every crates/**/src
    python3 scripts/find_stubs.py --limit 120
    python3 scripts/find_stubs.py --flagged         # only STUB / STUB? / EMPTY?
    python3 scripts/find_stubs.py --md /tmp/stub_report.md

Output columns: LOC  classification  file:line  fn
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

FN = re.compile(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)")
STUB_MARKERS = ("unimplemented!", "todo!")
# Phrases that mark a deliberate no-op / deferral, whether raised or logged.
STUB_PHRASES = (
    "not implemented",
    "not exposed",
    "not supported",
    "no font enumeration",
    "no host",
    "is not wired",
    "is not available",
    "cannot be exposed",
)
DOC_PREFIXES = ("///", "//!", "#[", "//")
DOC_CONTEXT = 6


def _match_brace(text: str, brace: int) -> int:
    depth = 0
    k = brace
    while k < len(text):
        c = text[k]
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return k
        k += 1
    return -1


def functions(path: Path):
    """Yield (name, loc, start, end, first, is_extern, body, source) per `fn`."""
    text = path.read_text(encoding="utf-8", errors="replace")
    lines = text.splitlines()
    for m in FN.finditer(text):
        brace = text.find("{", m.end())
        if brace == -1:
            continue
        end = _match_brace(text, brace)
        if end == -1:
            continue
        start = text[: m.start()].count("\n") + 1
        end_line = text[:end].count("\n") + 1
        is_extern = 'extern "C"' in text[max(0, m.start() - 32) : m.start()]
        body = text[brace : end + 1]
        # Prepend leading doc-attribute lines for context.
        first = start
        while first > 1 and lines[first - 2].lstrip().startswith(DOC_PREFIXES):
            first -= 1
            if start - first >= DOC_CONTEXT:
                break
        source = "\n".join(lines[first - 1 : end_line])
        yield m.group(1), end_line - start + 1, start, end_line, first, is_extern, body, source


def classify(body: str, loc: int) -> str:
    inner = body.strip()[1:-1].strip() if body.strip().startswith("{") else body
    stmts = [
        line.strip()
        for line in inner.splitlines()
        if line.strip() and not line.strip().startswith("//")
    ]
    lowered = body.lower()
    if any(marker in body for marker in STUB_MARKERS):
        return "STUB"
    # A callback that mostly raises is a deferral candidate.
    if "report_error(" in body and len(stmts) <= 10:
        return "STUB?"
    # A no-op that logs (or raises) a deferral phrase instead of raising.
    if any(phrase in lowered for phrase in STUB_PHRASES) and len(stmts) <= 12:
        return "STUB?"
    # A callback that only warns and returns a fixed value.
    if "log::warn!" in body and len(stmts) <= 6:
        return "STUB?"
    if not stmts or all(line.rstrip(";") in ("0", "()", "None", "false", "true") for line in stmts):
        return "EMPTY?"
    if loc <= 4:
        return "tiny"
    return ""


def registration_line(path: Path, name: str) -> str | None:
    """The first line that mentions `name` but is not its definition."""
    for n, line in enumerate(path.read_text(encoding="utf-8", errors="replace").splitlines(), 1):
        if name in line and not re.search(rf"\bfn\s+{re.escape(name)}\b", line):
            return f"{n}: {line.strip()}"
    return None


def member_of(path: Path, name: str) -> str | None:
    """Best-effort TJS member name: a `"name", <fn>` registration pair."""
    text = path.read_text(encoding="utf-8", errors="replace")
    m = re.search(rf'"([^"\n]+)"\s*,\s*{re.escape(name)}\b', text)
    return m.group(1) if m else None


def number(source: str, start: int) -> str:
    return "\n".join(
        f"{start + i:5} | {line}" for i, line in enumerate(source.splitlines())
    )


def main() -> int:
    args = sys.argv[1:]
    all_crates = "--all" in args
    flagged_only = "--flagged" in args
    extern_only = "--extern" in args
    md_path: Path | None = None
    if "--md" in args:
        i = args.index("--md")
        md_path = Path(args[i + 1]) if i + 1 < len(args) and not args[i + 1].startswith("-") else Path("/tmp/stub_report.md")
        del args[i : i + (2 if i + 1 < len(args) and not args[i + 1].startswith("-") else 1)]
    limit = 0
    if "--limit" in args:
        i = args.index("--limit")
        limit = int(args[i + 1])
        del args[i : i + 2]
    args = [a for a in args if not a.startswith("--")]
    roots = [Path(a) for a in args]
    if not roots:
        roots = [Path("crates")] if all_crates else [Path("crates/compat/src")]

    rows = []
    for root in roots:
        paths = [root] if root.is_file() else sorted(root.rglob("*.rs"))
        for path in paths:
            for name, loc, start, end, first, is_extern, body, source in functions(path):
                if extern_only and not is_extern:
                    continue
                cls = classify(body, loc)
                if flagged_only and cls not in ("STUB", "STUB?", "EMPTY?"):
                    continue
                rows.append((loc, cls, path, start, is_extern, name, first, source))
    rows.sort(key=lambda r: (r[0], str(r[2])))
    if limit:
        rows = rows[:limit]

    if md_path is None:
        for loc, cls, path, start, is_extern, name, _first, _src in rows:
            tag = " [extern]" if is_extern else ""
            print(f"{loc:4}  {cls:7} {path}:{start}  {name}{tag}")
        return 0

    out = [
        "# Stub-hunt report",
        "",
        f"Generated by `scripts/find_stubs.py` from {', '.join(str(r) for r in roots)}.",
        f"{len(rows)} candidate(s), smallest first. Classification is a heuristic;",
        "confirm each against `reference/cpp/**`.",
        "",
    ]
    for loc, cls, path, start, is_extern, name, first, source in rows:
        reg = registration_line(path, name)
        member = member_of(path, name)
        tag = " [extern]" if is_extern else ""
        out.append(f"## `{name}` — `{path}:{start}` — LOC {loc} — `{cls}`{tag}")
        out.append("")
        if member:
            out.append(f"Member: `{member}`")
            out.append("")
        if reg:
            out.append(f"Registration: `{reg}`")
            out.append("")
        out.append("```rust")
        out.append(number(source, first))
        out.append("```")
        out.append("")
    md_path.write_text("\n".join(out), encoding="utf-8")
    print(f"wrote {md_path} ({len(rows)} candidates)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
