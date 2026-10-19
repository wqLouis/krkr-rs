#!/usr/bin/env python3
"""Reference storage-media domain coverage guard.

The reference engine dispatches the ``scheme://…`` prefix of every storage
name to an ``iTVPStorageMedia`` registered through ``TVPRegisterStorageMedia``
(``reference/cpp/core/base/StorageIntf.cpp:514``; the manager registers the
built-in file media at ``:190-196``).  The media that name a domain via
``GetName`` are

* ``tTVPFileMedia`` → ``file`` (``StorageImpl.cpp:60,77``);
* ``PSBMedia`` → ``psb`` (``psbfile/PSBMedia.h:10,25``);
* ``PSDStorage`` → ``psd`` (``psdfile/main.cpp:15,70``, via ``BASENAME``);
* ``VarStorage`` → ``var`` (``varfile.cpp:198,221``, via ``BASENAME``).

A port that parses a container but never registers the media makes those names
miss silently.  That is the ``psb://`` bug: ``PSBFile.load`` parsed the
document but did not register the resources, so
``Layer.loadImages("psb://quickmenu.pimg/<id>.tlg")`` raised and aborted
``DialogLayer.addButton`` before it populated ``names``.

This check derives the reference domains from the ``iTVPStorageMedia``
subclasses' ``GetName`` bodies (resolving the ``#define BASENAME`` indirection)
and requires each one to be either

* **served** — a scheme-keyed ``register_media("scheme://…")`` site in a crate
  ``src`` tree (``engine::Storage::register_media`` and its compat callers), or
  the core ``file://`` handling in ``crates/compat/src/storage.rs``; or
* **documented** — a plugin module under
  ``crates/compat/src/natives/plugins`` that names both the scheme and the
  missing L2 media-provider seam (an explicit no-op rather than a silent
  miss).

A domain that is neither is a finding.  A reference domain the port genuinely
cannot recover is recorded, with a reason, in
``scripts/allowlists/storage_media_allow.txt``.

Usage:
    uv run --project scripts krkr-gate storage-media
    uv run --project scripts krkr-gate storage-media --self-test
    uv run --project scripts krkr-gate storage-media --update-allow
    uv run --project scripts krkr-gate storage-media --quiet
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

from ..common import ALLOWLISTS_DIR, FIXTURES_DIR, REPO_ROOT
from ..framework import (
    find_block,
    load_keyed_allowlist,
    strip_cpp_comments,
    write_keyed_allowlist,
)

ROOT = REPO_ROOT
ALLOWLIST = ALLOWLISTS_DIR / "storage_media_allow.txt"
FIXTURES = FIXTURES_DIR / "storage_media"
ALLOWLIST_HEADER = (
    "# Storage-media allowlist: reference domains the port does not serve\n"
    "# and cannot document as a recoverable missing seam.\n"
    "# One entry per line: <scheme> -- reason citing reference/cpp/**\n"
    "# Regenerate after review with:\n"
    "#   uv run --project scripts krkr-gate storage-media --update-allow\n"
    "\n"
)

# A storage-media domain name; the reference spellings are plain lowercase
# words, but the grammar allows the usual scheme characters.
SCHEME = r"[A-Za-z][A-Za-z0-9+.\-]*"

REFERENCE_SUFFIXES = {".cpp", ".cc", ".cxx", ".h", ".hpp"}

# ``class Name : public bases… {`` where ``iTVPStorageMedia`` is among the
# bases.  The body is matched afterwards, so the base list never crosses ``{``.
_CLASS = re.compile(
    r"\bclass\s+(?P<name>\w+)\s*:\s*public\s+(?P<bases>[^{;]*?iTVPStorageMedia[^{;]*?)\{",
    re.S,
)

# ``void GetName(ttstr &name) … {`` — the one ``iTVPStorageMedia`` signature.
# The other ``GetName`` overloads return a ``ttstr``/``const char *`` and take
# no ``ttstr &`` parameter, so they never match.
_GETNAME = re.compile(r"\bGetName\s*\(\s*ttstr\s*&\s*(?P<param>\w+)\s*\)[^{;]*\{")

# ``name = TJS_W("psb")`` (PSBMedia.h) — a literal in the body.
_TJS_W = re.compile(r'TJS_W\(\s*"(?P<domain>[^"]*)"\s*\)')

# ``#define BASENAME TJS_W("var")`` (varfile.cpp) — the macro the body assigns.
_DEFINE_BASENAME = re.compile(
    r'#define\s+(?P<name>\w+)\s+TJS_W\(\s*"(?P<domain>[^"]*)"\s*\)'
)

# ``name = BASENAME;`` — an assignment to the ``GetName`` out-parameter.
_ASSIGN = re.compile(r"\b(?P<lhs>\w+)\s*=\s*(?P<rhs>\w+)\s*;")

# ``register_media("psb://…")`` / ``register_media(format!("psb://…"))``.
_REGISTER_MEDIA = re.compile(
    r"\bregister_media\s*\(\s*(?:format!\s*\(\s*)?" r'"(?P<name>[^"]*)"'
)

# A string literal that opens a ``scheme://`` name (the core ``file`` media).
_SCHEME_LITERAL = re.compile(rf'"({SCHEME})://')

# The plugin module documents the decision rather than serving the scheme.
# The documentation must be about the storage-media provider seam specifically,
# not any incidental "seam" or "no-op" elsewhere in the file.
_MEDIA_CONTEXT = re.compile(
    r"storage[\s-]+media|media[\s-]+provider|iTVPStorageMedia"
    r"|register_media|TVPRegisterStorageMedia",
    re.I,
)
_NOT_SERVED = re.compile(r"\bseam\b|\bnot ported\b|\bno-op\b", re.I)


def _block(text: str, open_brace: int) -> str:
    """Return the text of the block whose ``{`` is at ``open_brace``."""
    span = find_block(text, open_brace)
    if span is not None:
        return text[span[0] : span[1]]
    return text[open_brace + 1 : len(text) - 1]


def _reference_sources(root: Path):
    base = root / "reference" / "cpp"
    if not base.is_dir():
        return
    for path in sorted(base.rglob("*")):
        if path.suffix in REFERENCE_SUFFIXES:
            yield path


def _domain_from_getname(body: str, param: str, macros: dict[str, str]):
    literal = _TJS_W.search(body)
    if literal:
        return literal.group("domain")
    for match in _ASSIGN.finditer(body):
        if match.group("lhs") == param:
            return macros.get(match.group("rhs"))
    return None


def reference_domains(root: Path) -> dict[str, list[tuple[str, int]]]:
    """Return ``scheme -> [(reference_path, line)]`` for every named media."""
    found: dict[str, list[tuple[str, int]]] = {}
    for path in _reference_sources(root):
        raw = path.read_text(encoding="utf-8", errors="replace")
        macros = {
            match.group("name"): match.group("domain")
            for match in _DEFINE_BASENAME.finditer(raw)
        }
        masked = strip_cpp_comments(raw)
        try:
            rel = path.relative_to(root).as_posix()
        except ValueError:
            rel = path.as_posix()
        for cls in _CLASS.finditer(masked):
            body = _block(masked, cls.end() - 1)
            getname = _GETNAME.search(body)
            if getname is None:
                continue
            method = _block(body, getname.end() - 1)
            domain = _domain_from_getname(method, getname.group("param"), macros)
            if not domain:
                continue
            line = masked.count("\n", 0, cls.start()) + 1
            found.setdefault(domain, []).append((rel, line))
    return found


def served_domains(root: Path) -> set[str]:
    """Return the schemes the port registers through the media seam."""
    served: set[str] = set()
    crates = root / "crates"
    if crates.is_dir():
        for path in sorted(crates.rglob("*.rs")):
            rel = path.relative_to(crates).as_posix()
            if "/src/" not in rel:
                continue
            text = path.read_text(encoding="utf-8", errors="replace")
            for match in _REGISTER_MEDIA.finditer(text):
                scheme = re.match(rf"({SCHEME})://", match.group("name"))
                if scheme:
                    served.add(scheme.group(1).lower())
    core = root / "crates" / "compat" / "src" / "storage.rs"
    if core.is_file():
        text = core.read_text(encoding="utf-8", errors="replace")
        served.update(
            match.group(1).lower() for match in _SCHEME_LITERAL.finditer(text)
        )
    return served


def documented_domains(root: Path) -> set[str]:
    """Return the schemes a plugin module documents as not served."""
    documented: set[str] = set()
    plugins = root / "crates" / "compat" / "src" / "natives" / "plugins"
    if not plugins.is_dir():
        return documented
    for path in sorted(plugins.rglob("*.rs")):
        text = path.read_text(encoding="utf-8", errors="replace")
        schemes = {m.group(1).lower() for m in re.finditer(rf"({SCHEME})://", text)}
        if not schemes:
            continue
        if not (_MEDIA_CONTEXT.search(text) and _NOT_SERVED.search(text)):
            continue
        documented |= schemes
    return documented


def scan(root: Path) -> list[dict[str, object]]:
    """Return one finding per reference domain that is neither served nor documented."""
    domains = reference_domains(root)
    covered = served_domains(root) | documented_domains(root)
    findings: list[dict[str, object]] = []
    for scheme in sorted(domains):
        if scheme.lower() in covered:
            continue
        for rel, line in domains[scheme]:
            findings.append({"scheme": scheme, "file": rel, "line": line})
    return findings


def self_test(verbose: bool = True) -> int:
    tree = FIXTURES / "tree"
    expected_path = FIXTURES / "expected.json"
    for path in (tree, expected_path):
        if not path.exists():
            print(f"check-storage-media: self-test fixture missing: {path}")
            return 1
    expected = json.loads(expected_path.read_text(encoding="utf-8"))
    findings = scan(tree)
    got = {finding["scheme"] for finding in findings}
    want = set(expected["findings"])
    if got != want:
        print("check-storage-media: self-test FAILED:")
        print(f"  findings: got {sorted(got)}")
        print(f"            want {sorted(want)}")
        print("  Fix the extractor; do not silence the self-test.")
        return 1
    served = served_domains(tree)
    documented = documented_domains(tree)
    if served != set(expected["served"]):
        print("check-storage-media: self-test FAILED:")
        print(f"  served: got {sorted(served)} want {sorted(expected['served'])}")
        return 1
    if documented != set(expected["documented"]):
        print("check-storage-media: self-test FAILED:")
        print(
            f"  documented: got {sorted(documented)} "
            f"want {sorted(expected['documented'])}"
        )
        return 1
    if verbose:
        print(
            "check-storage-media: self-test ok "
            f"({len(want)} finding(s), {len(served)} served, {len(documented)} documented)"
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
        help="rewrite scripts/allowlists/storage_media_allow.txt with the findings",
    )
    parser.add_argument("--quiet", action="store_true", help="only print failures")
    args = parser.parse_args(argv)

    selftest = self_test(verbose=not args.quiet)
    if args.self_test:
        return selftest
    if selftest != 0:
        return 1

    domains = reference_domains(ROOT)
    served = served_domains(ROOT)
    documented = documented_domains(ROOT)
    findings = scan(ROOT)
    allowed = load_keyed_allowlist(ALLOWLIST)

    if args.update_allow:
        write_keyed_allowlist(
            ALLOWLIST,
            ALLOWLIST_HEADER,
            sorted({f["scheme"] for f in findings}),  # type: ignore[arg-type]
            allowed,
        )
        print(
            f"check-storage-media: allowlist updated — {len(findings)} finding(s) "
            f"-> {ALLOWLIST.relative_to(ROOT)}"
        )
        return 0

    new = [f for f in findings if f["scheme"] not in allowed]
    stale = [scheme for scheme in allowed if scheme not in {f["scheme"] for f in findings}]

    if new or stale:
        if new:
            print(
                "check-storage-media: reference storage media with no port serving "
                "path and no documented no-op:"
            )
            for finding in new:
                print(
                    f"  {finding['scheme']}://  {finding['file']}:{finding['line']}"
                )
            print(
                "\n  Serve the domain through engine::Storage::register_media "
                "(a scheme-keyed registration), or document the missing L2 "
                "media-provider seam in the plugin module, or record a reviewed "
                "exception in scripts/allowlists/storage_media_allow.txt."
            )
        if stale:
            print(
                f"\ncheck-storage-media: {len(stale)} allowlisted scheme(s) are no "
                "longer findings — shrink "
                "scripts/allowlists/storage_media_allow.txt:"
            )
            for scheme in sorted(stale):
                print(f"  {scheme}")
        return 1

    if not args.quiet:
        print(
            f"check-storage-media: clean ({len(domains)} reference domain(s): "
            f"{len(served)} served, {len(documented)} documented, "
            f"{len(allowed)} allowlisted)"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
