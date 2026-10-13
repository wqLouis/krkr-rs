#!/usr/bin/env python3
"""Fail when a host-fact seam has no production installer.

``CompatHost`` carries provider-installation seams: ``set_*`` methods (and
constructor-style ``with_*`` methods) declared in an ``impl CompatHost`` block.
Each one installs a host capability the compat natives cannot derive on their
own. A seam that no production code calls is dead weight: the matching native
raises "the host fact is not installed" at run time even though the feature
looked wired.

This check collects every ``pub fn set_*`` / ``pub fn with_*`` declared in an
``impl CompatHost`` block under ``crates/compat/src/``, then looks for a call
site in a production source file (``crates/*/src/**/*.rs``). A call in an
integration test (``crates/*/tests/**``) or inside a ``#[cfg(test)]`` block
does not count.

The composition root (``crates/app/src/``) is the expected installer. A seam
wired by another production crate is accepted only when it is genuinely the
installer, recorded in ``scripts/allowlists/host_facts_allow.txt`` (one
``CompatHost::<seam>`` per line, optional ``-- reason``). A new unwired seam
fails the check; an allowlisted seam that is now wired is reported stale so the
list shrinks.

Usage:
    uv run --project scripts krkr-gate host-facts               # check + fixture self-test
    uv run --project scripts krkr-gate host-facts --self-test   # only the fixture self-test
    uv run --project scripts krkr-gate host-facts --update-allow
    uv run --project scripts krkr-gate host-facts --quiet
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import sys

from ..common import ALLOWLISTS_DIR, FIXTURES_DIR, REPO_ROOT

ROOT = REPO_ROOT
ALLOWLIST = ALLOWLISTS_DIR / "host_facts_allow.txt"
FIXTURES = FIXTURES_DIR / "host_facts"

# `pub fn set_foo` / `pub fn with_foo`, the provider-installation shape.
_SEAM_RE = re.compile(r"\bpub\s+fn\s+(set_\w+|with_\w+)")
_CFG_TEST_RE = re.compile(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]")


def _blank_chars(chars: list[str], start: int, end: int) -> None:
    n = len(chars)
    for i in range(start, min(end, n)):
        if chars[i] != "\n":
            chars[i] = " "


def strip_rust(text: str) -> str:
    """Blank comments and string/char literals, preserving offsets and lines.

    The result has the same length as ``text`` so match positions map straight
    back to the original line numbers, while braces that live inside a comment
    or a literal cannot confuse the block walker.
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
            _blank_chars(out, i, j)
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
            _blank_chars(out, i, j)
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
            _blank_chars(out, i, j)
            i = j
        elif c == "r" or (c == "b" and i + 1 < n and text[i + 1] in "#r"):
            raw = re.match(r'(?:br|rb|r)(#*)"', text[i:])
            if raw is not None:
                hashes = raw.group(1)
                term = '"' + hashes
                end = text.find(term, i + raw.end())
                end = n if end < 0 else end + len(term)
                _blank_chars(out, i, end)
                i = end
            else:
                i += 1
        elif c == "'":
            k = text.find("'", i + 1)
            if text[i + 1 : i + 2] == "\\":
                k = text.find("'", i + 2)
            if 0 < k and k - i <= 4:
                _blank_chars(out, i, k + 1)
                i = k + 1
            else:
                i += 1
        else:
            i += 1
    return "".join(out)


def blank_cfg_test(text: str) -> str:
    """Blank the item a ``#[cfg(test)]`` attribute guards.

    A test-only caller must not count as a production installer, whether it is
    an inline ``mod tests { ... }`` block or a single ``#[cfg(test)] fn``.
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
        _blank_chars(out, match.start(), k)
    return "".join(out)


def _compat_host_impls(text: str) -> list[tuple[int, int]]:
    """Return ``(body_start, body_end)`` for each ``impl ... CompatHost`` block."""
    blocks: list[tuple[int, int]] = []
    for match in re.finditer(r"\bimpl\b", text):
        brace = text.find("{", match.end())
        if brace < 0:
            continue
        header = text[match.end() : brace]
        if "CompatHost" not in header or re.search(r"\bfor\b", header):
            continue
        depth = 0
        i = brace
        while i < len(text):
            if text[i] == "{":
                depth += 1
            elif text[i] == "}":
                depth -= 1
                if depth == 0:
                    break
            i += 1
        blocks.append((brace + 1, i))
    return blocks


def _rust_sources(path: pathlib.Path):
    if not path.is_dir():
        return
    for source in sorted(path.rglob("*.rs")):
        yield source


def _line_of(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


def collect_seams(root: pathlib.Path) -> dict[str, list[str]]:
    """Map each ``CompatHost`` seam name to its declaration sites."""
    seams: dict[str, list[str]] = {}
    src = root / "crates" / "compat" / "src"
    for path in _rust_sources(src):
        rel = path.relative_to(root).as_posix()
        raw = path.read_text(encoding="utf-8")
        scrubbed = blank_cfg_test(strip_rust(raw))
        for start, end in _compat_host_impls(scrubbed):
            body = scrubbed[start:end]
            for match in _SEAM_RE.finditer(body):
                name = match.group(1)
                line = _line_of(raw, start + match.start())
                seams.setdefault(name, []).append(f"{rel}:{line}")
    return seams


def _caller_patterns(name: str) -> list[re.Pattern[str]]:
    esc = re.escape(name)
    # A method call (`host.set_foo(`) or, for a constructor-style seam, an
    # associated call (`CompatHost::with_foo(`).
    patterns = [re.compile(rf"\.\s*{esc}\s*\(")]
    if name.startswith("with_"):
        patterns.append(re.compile(rf"::\s*{esc}\s*\("))
    # A free installer taking the host first: `set_foo(host, ...)`.
    patterns.append(re.compile(rf"(?<![\w:]){esc}\s*\(\s*host\b"))
    return patterns


def production_corpus(root: pathlib.Path) -> list[tuple[str, str]]:
    """Return ``(relative path, scrubbed text)`` once per production source.

    Every source file is read and scrubbed a single time; the per-seam walk in
    [`scan`] then only runs cheap regexes over the cached text.
    """
    files: list[tuple[str, str]] = []
    crates = root / "crates"
    if not crates.is_dir():
        return files
    for crate in sorted(crates.iterdir()):
        if not crate.is_dir() or "tests" in crate.relative_to(root).parts:
            continue
        for path in _rust_sources(crate / "src"):
            rel_parts = path.relative_to(root).parts
            if "tests" in rel_parts:
                continue
            raw = path.read_text(encoding="utf-8")
            if "set_" not in raw and "with_" not in raw:
                continue
            files.append(
                (path.relative_to(root).as_posix(), blank_cfg_test(strip_rust(raw)))
            )
    return files


def scan(
    root: pathlib.Path,
) -> tuple[dict[str, list[str]], dict[str, list[str]], dict[str, list[str]]]:
    """Return ``(seams, unwired, callers)`` for ``root``."""
    seams = collect_seams(root)
    corpus = production_corpus(root)
    unwired: dict[str, list[str]] = {}
    callers: dict[str, list[str]] = {}
    for name, decls in seams.items():
        patterns = _caller_patterns(name)
        hits = sorted(
            {
                f"{rel}:{_line_of(text, match.start())}"
                for rel, text in corpus
                for pattern in patterns
                for match in pattern.finditer(text)
            }
        )
        callers[name] = hits
        if not hits:
            unwired[name] = decls
    return seams, unwired, callers


def seam_key(name: str) -> str:
    return f"CompatHost::{name}"


def read_allowlist(path: pathlib.Path) -> dict[str, str]:
    entries: dict[str, str] = {}
    if not path.is_file():
        return entries
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        reason = ""
        if "--" in line:
            line, reason = line.split("--", 1)
            reason = reason.strip()
        entries[line.strip()] = reason
    return entries


def write_allowlist(path: pathlib.Path, seams: list[str], reasons: dict[str, str]) -> None:
    lines = [
        "# Host-fact seam allowlist: provider-installation seams on CompatHost",
        "# that are deliberately not installed by the composition root.",
        "# One entry per line: CompatHost::<seam> -- reason",
        "# A seam wired by another production crate is allowed only when that",
        "# crate is genuinely the installer; state which in the reason.",
        "# Regenerate after review with:",
        "#   uv run --project scripts krkr-gate host-facts --update-allow",
        "# The checker fails when an unwired seam is not listed here.",
        "",
    ]
    for name in sorted(seams):
        key = seam_key(name)
        reason = reasons.get(key, "")
        lines.append(f"{key} -- {reason}" if reason else key)
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def self_test(verbose: bool = True) -> int:
    """Fixture check: an unwired seam is flagged, a wired one is not."""
    tree = FIXTURES / "tree"
    expected_path = FIXTURES / "expected.json"
    for path in (tree, expected_path):
        if not path.exists():
            print(f"check-host-facts: self-test fixture missing: {path}")
            return 1
    expected = json.loads(expected_path.read_text(encoding="utf-8"))
    seams, unwired, _callers = scan(tree)
    want_seams = set(expected["seams"])
    want_unwired = set(expected["unwired"])
    got_seams = set(seams)
    got_unwired = set(unwired)
    problems: list[str] = []
    if got_seams != want_seams:
        problems.append(f"seams: got {sorted(got_seams)} != want {sorted(want_seams)}")
    if got_unwired != want_unwired:
        problems.append(
            f"unwired: got {sorted(got_unwired)} != want {sorted(want_unwired)}"
        )
    if problems:
        print("check-host-facts: self-test FAILED:")
        for problem in problems:
            print(f"  {problem}")
        print("  Fix the extractor; do not silence the self-test.")
        return 1
    if verbose:
        print(
            "check-host-facts: self-test ok "
            f"({len(got_seams)} seam(s), {len(got_unwired)} unwired in the fixture)"
        )
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--self-test", action="store_true", help="run the fixture self-test and exit"
    )
    parser.add_argument(
        "--update-allow",
        action="store_true",
        help="rewrite scripts/allowlists/host_facts_allow.txt with the unwired seams",
    )
    parser.add_argument("--quiet", action="store_true", help="only print failures")
    args = parser.parse_args(argv)

    # The self-test runs on every invocation, like the other guards.
    selftest = self_test(verbose=not args.quiet)
    if args.self_test:
        return selftest
    if selftest != 0:
        return 1

    if args.update_allow:
        seams, unwired, _callers = scan(ROOT)
        write_allowlist(ALLOWLIST, sorted(unwired), read_allowlist(ALLOWLIST))
        print(
            f"check-host-facts: allowlist updated — {len(unwired)} seam(s) not "
            f"installed by a production caller -> {ALLOWLIST.relative_to(ROOT)}"
        )
        return 0

    seams, unwired, _callers = scan(ROOT)
    allowed = read_allowlist(ALLOWLIST)
    new_unwired = {n: d for n, d in unwired.items() if seam_key(n) not in allowed}
    stale = [
        key
        for key in allowed
        if key not in {seam_key(n) for n in unwired}
    ]

    if new_unwired or stale:
        if new_unwired:
            print(
                f"check-host-facts: {len(new_unwired)} seam(s) declared on "
                "CompatHost have no production installer:"
            )
            for name in sorted(new_unwired):
                for decl in new_unwired[name]:
                    print(f"  {decl}  {seam_key(name)}")
            print(
                "\n  The composition root (crates/app/src/) is the expected "
                "installer. Wire the seam there, or record a reviewed exception "
                "in scripts/allowlists/host_facts_allow.txt."
            )
        if stale:
            print(
                f"\ncheck-host-facts: {len(stale)} allowlisted seam(s) are now "
                "installed — shrink scripts/allowlists/host_facts_allow.txt:"
            )
            for key in sorted(stale):
                print(f"  {key}")
        return 1

    if not args.quiet:
        print(
            f"check-host-facts: clean ({len(seams)} seams, all installed by a "
            f"production caller; {len(allowed)} allowlisted)"
        )
    return 0


def run(argv=None):
    """Run the check. ``argv`` carries the check-specific flags."""
    return main(argv)


if __name__ == "__main__":
    sys.exit(run(sys.argv[1:]))
