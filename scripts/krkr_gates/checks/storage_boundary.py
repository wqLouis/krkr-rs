#!/usr/bin/env python3
"""Fail when a script-supplied storage name reaches L2 or the host unbared.

L3 owns the TVP storage-name grammar. A script-visible name may carry the local
``file`` media prefix (``file://.``, ``file://`` or ``file:``); the L2
``engine::Storage`` namespace takes a bare name within a mount. Every L3 loader
that hands a script name across that boundary must first route it through
``crate::storage::strip_file_media``.

A name that carries **another** scheme (``var://…``, ``psb://…``) crosses the
boundary unchanged: the namespace dispatches a registered scheme to the
provider that serves it, and the provider owns the scheme's own path grammar.
Such a name needs no stripping, so a binding that is known to carry a scheme
is not a finding.

This check scans ``crates/compat/src/**`` for calls that carry a script name to
the namespace, the host filesystem, or the decoded-video seam:

* a storage-handle method — ``read`` / ``read_prefix`` / ``find`` / ``exists``
  / ``stat`` / ``relative_disk_name`` / ``local_name`` — on a ``Storage``
  handle;
* a storage write seam — a ``save_image`` / ``save`` method call whose callee
  does not itself route the name through ``strip_file_media``;
* a host write — ``fs::write`` / ``fs::open`` / ``fs::create_dir_all`` and the
  other path-taking ``fs`` helpers;
* ``game_dir().join(...)``;
* the ``VideoSource`` seam — ``spec`` / ``audio`` / ``frame_at`` and the
  ``decoded_audio`` helper — which ``crates/app`` later reads through
  ``Storage::read``;
* ``FsPath::host(...)``, the host-filesystem boundary for read/write loaders.

A call is flagged only when its name argument is a **bare identifier** that is
not already known to be stripped. The check knows a name is stripped when it is

* bound in the same function from ``strip_file_media(...)``, or transitively
  from an expression that mentions a stripped name;

* bound from a call to a function whose body routes through
  ``strip_file_media`` (directly or transitively), so a helper such as a
  ``local_path`` / ``output_path`` wrapper is recognized — across files, so a
  shared helper such as the image ``save_image`` seam counts at its callers;

* bound from a name that carries a media scheme (``"var://…"``,
  ``"psb://…"``): such a name crosses the boundary unchanged and the
  namespace routes it to the provider that registered the scheme;

* the callee of a save seam (``save_image`` / ``save``) when its own body
  routes through ``strip_file_media``; a seam that strips is safe at every
  call site, and a seam that does not is a finding.

* a module-level ``const`` / ``static`` (a literal constant, not script data);

* a string literal (the scrubber blanks it), or an expression that is not a
  bare identifier (``self.storage``, ``Path::new(...)``, ``format!(...)``, a
  ``strip_file_media`` call, …).

Anything left is a script name crossing the boundary unstripped. The allowlist
(``scripts/allowlists/storage_boundary_allow.txt``) records the reviewed
exceptions — a name that has already been stripped by a resolver method, a
closure input, and the like — one entry per line with a reason. A new unstripped
call fails the check; an allowlisted call that no longer appears is reported
stale so the list shrinks.

Usage:
    uv run --project scripts krkr-gate storage-boundary
    uv run --project scripts krkr-gate storage-boundary --self-test
    uv run --project scripts krkr-gate storage-boundary --update-allow
    uv run --project scripts krkr-gate storage-boundary --quiet
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import sys

from ..common import ALLOWLISTS_DIR, FIXTURES_DIR, REPO_ROOT
from ..framework import (
    blank_cfg_test,
    load_keyed_allowlist,
    match_delimiter,
    strip_rust,
    write_keyed_allowlist,
)

ROOT = REPO_ROOT
ALLOWLIST = ALLOWLISTS_DIR / "storage_boundary_allow.txt"
FIXTURES = FIXTURES_DIR / "storage_boundary"
ALLOWLIST_HEADER = (
    "# Storage-boundary allowlist: script names that reach L2 or the host\n"
    "# without a direct strip_file_media route, reviewed as safe.\n"
    "# One entry per line: <file> :: <function> :: <call> -- reason\n"
    "# Regenerate after review with:\n"
    "#   uv run --project scripts krkr-gate storage-boundary --update-allow\n"
    "# The checker fails when an unstripped call is not listed here.\n"
    "\n"
)

# A call is a boundary site when this identifier is the receiver.
_STORAGE_RECEIVERS = {"storage", "store", "pointer", "ptr"}
_SEAM_RECEIVERS = {"source"}

# Ordered so a longer method name wins the alternation.
_STORAGE_CALL = re.compile(
    r"(?P<recv>[A-Za-z_][A-Za-z0-9_]*)\s*\.\s*"
    r"(?P<method>read_prefix|relative_disk_name|local_name|read|find|exists|stat)\s*\("
)
_JOIN_CALL = re.compile(r"game_dir\s*\(\s*\)\s*\.\s*join\s*\(")
_SEAM_CALL = re.compile(
    r"(?P<recv>[A-Za-z_][A-Za-z0-9_]*)\s*\.\s*(?P<method>frame_at|spec|audio)\s*\("
)
_DECODED_AUDIO = re.compile(r"\bdecoded_audio\s*\(")
# A string literal that carries a media scheme ("var://…", "psb://…"): such a
# name crosses the boundary unchanged and the namespace routes it to the
# provider that registered the scheme.
_MEDIA_QUALIFIED = re.compile(r'"[A-Za-z][A-Za-z0-9+.\-]*://')
_SAVE_CALL = re.compile(r"\.\s*(?P<method>save_image_on|save_image|save)\s*\(")
_HOST_WRITE = re.compile(
    r"\bfs\s*::\s*(?P<method>write|open|create_dir_all|remove_file|remove_dir|remove_dir_all)\s*\("
)
_FSPATH_HOST = re.compile(r"\bFsPath::host\s*\(")
_PATH_NEW = re.compile(r"\bPath::new\s*\(")
_PATHBUF_FROM = re.compile(r"\bPathBuf::from\s*\(")
_FN = re.compile(r"\bfn\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)")
_CONST = re.compile(r"\b(?:const|static)\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)\s*:")
_LET = re.compile(r"\blet\s+(?:mut\s+)?(?P<name>[A-Za-z_][A-Za-z0-9_]*)")
_IDENT = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")


def _rust_sources(path: pathlib.Path):
    if not path.is_dir():
        return
    for source in sorted(path.rglob("*.rs")):
        yield source


def _line_of(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


def _split_top_level(text: str) -> list[str]:
    parts: list[str] = []
    start = 0
    depth = 0
    for i, c in enumerate(text):
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
        elif c == "," and depth == 0:
            parts.append(text[start:i])
            start = i + 1
    parts.append(text[start:])
    return parts


def _parse_functions(scrubbed: str):
    """Return ``(name, params_text, body_start, body_end)`` for each ``fn``.

    ``params_text`` and the body offsets index the scrubbed source (and the raw
    source, since the scrubber preserves offsets).
    """
    functions = []
    for match in _FN.finditer(scrubbed):
        name = match.group("name")
        popen = scrubbed.find("(", match.end())
        if popen < 0:
            continue
        pclose = match_delimiter(scrubbed, popen, "(", ")")
        if pclose < 0:
            continue
        j = pclose + 1
        brace = -1
        while j < len(scrubbed):
            if scrubbed[j] == "{":
                brace = j
                break
            if scrubbed[j] == ";":
                break
            j += 1
        if brace < 0:
            continue
        bclose = match_delimiter(scrubbed, brace, "{", "}")
        if bclose < 0:
            continue
        functions.append((name, scrubbed[popen + 1 : pclose], brace + 1, bclose))
    return functions


def _param_idents(params_text: str) -> set[str]:
    names: set[str] = set()
    for part in _split_top_level(params_text):
        part = part.strip()
        if not part or part in {"&self", "self", "&mut self"}:
            continue
        left = part.split(":", 1)[0]
        left = re.sub(r"&(?:mut\s+)?", "", left)
        tokens = re.findall(r"[A-Za-z_][A-Za-z0-9_]*", left)
        if tokens and tokens[-1] not in {"mut", "ref"}:
            names.add(tokens[-1])
    return names


def _path_param_idents(params_text: str) -> set[str]:
    """Parameters typed as a ``Path``/``PathBuf`` — an already-resolved name.

    A script name is a string at the storage boundary; a helper that takes a
    ``&Path`` receives a path that was resolved before the call, so its own
    ``FsPath::host(path)`` is not a boundary crossing. A raw-to-path
    conversion is caught at its own ``Path::new``/``PathBuf::from`` site.
    """
    names: set[str] = set()
    for part in _split_top_level(params_text):
        part = part.strip()
        if not part or ":" not in part:
            continue
        left, right = part.split(":", 1)
        if "Path" not in right:
            continue
        left = re.sub(r"&(?:mut\s+)?", "", left)
        tokens = re.findall(r"[A-Za-z_][A-Za-z0-9_]*", left)
        if tokens and tokens[-1] not in {"mut", "ref"}:
            names.add(tokens[-1])
    return names


def _local_bindings(raw_body: str, scrubbed_body: str):
    """Return ``(name, raw_rhs)`` for each ``let`` bound in the body."""
    bindings = []
    for match in _LET.finditer(scrubbed_body):
        name = match.group("name")
        eq = -1
        depth = 0
        k = match.end()
        while k < len(scrubbed_body):
            c = scrubbed_body[k]
            if c in "([{":
                depth += 1
            elif c in ")]}":
                depth -= 1
            elif c == "=" and depth == 0:
                eq = k
                break
            elif c == ";" and depth == 0:
                break
            k += 1
        if eq < 0:
            continue
        semi = len(scrubbed_body)
        depth = 0
        k = eq + 1
        while k < len(scrubbed_body):
            c = scrubbed_body[k]
            if c in "([{":
                depth += 1
            elif c in ")]}":
                depth -= 1
            elif c == ";" and depth == 0:
                semi = k
                break
            k += 1
        bindings.append((name, raw_body[eq + 1 : semi]))
    return bindings


def _stripping_functions(functions, raw: str) -> set[str]:
    """Function names whose body routes a name through ``strip_file_media``.

    Transitive: a helper that merely calls a stripping helper counts too, so a
    ``local_path`` / ``output_path`` wrapper is recognized at its call sites.
    """
    bodies = {name: raw[start:end] for name, _params, start, end in functions}
    stripping = {name for name, body in bodies.items() if "strip_file_media" in body}
    changed = True
    while changed:
        changed = False
        for name, body in bodies.items():
            if name in stripping:
                continue
            if any(re.search(rf"\b{re.escape(fn)}\s*\(", body) for fn in stripping):
                stripping.add(name)
                changed = True
    return stripping


def _rhs_is_stripped(rhs: str, stripped: set[str], stripping_fns: set[str]) -> bool:
    if "strip_file_media" in rhs:
        return True
    if _MEDIA_QUALIFIED.search(rhs):
        return True
    if any(re.search(rf"\b{re.escape(fn)}\s*\(", rhs) for fn in stripping_fns):
        return True
    return any(re.search(rf"\b{re.escape(name)}\b", rhs) for name in stripped)


def _stripped_in_function(bindings, stripping_fns: set[str]) -> set[str]:
    stripped: set[str] = set()
    changed = True
    while changed:
        changed = False
        for name, rhs in bindings:
            if name in stripped:
                continue
            if _rhs_is_stripped(rhs, stripped, stripping_fns):
                stripped.add(name)
                changed = True
    return stripped


def _bare_ident(argument: str) -> str | None:
    argument = argument.strip()
    argument = re.sub(r"^&\s*(?:mut\s+)?", "", argument).strip()
    return argument if _IDENT.match(argument) else None


def _is_definition(scrubbed: str, match: re.Match[str]) -> bool:
    return bool(re.search(r"\bfn\s+$", scrubbed[: match.start()]))


def scan(root: pathlib.Path) -> list[dict[str, str]]:
    """Return one record per unstripped boundary call under ``root``."""
    src = root / "crates" / "compat" / "src"
    parsed = []
    stripping_fns: set[str] = set()
    for path in _rust_sources(src):
        rel = path.relative_to(root).as_posix()
        raw = path.read_text(encoding="utf-8")
        scrubbed = blank_cfg_test(strip_rust(raw))
        functions = _parse_functions(scrubbed)
        parsed.append((rel, raw, scrubbed, functions))
        # The stripping set spans files: a shared helper defined in one module
        # (the image `save_image` seam) is recognized at its callers.
        stripping_fns |= _stripping_functions(functions, raw)

    findings: list[dict[str, str]] = []
    for rel, raw, scrubbed, functions in parsed:
        consts = {m.group("name") for m in _CONST.finditer(scrubbed)}
        scoped = []
        for name, params_text, start, end in functions:
            bindings = _local_bindings(raw[start:end], scrubbed[start:end])
            scoped.append(
                (
                    name,
                    start,
                    end,
                    _param_idents(params_text),
                    _path_param_idents(params_text),
                    {b[0] for b in bindings},
                    _stripped_in_function(bindings, stripping_fns),
                )
            )

        def enclosing(offset: int):
            best = None
            for info in scoped:
                _name, start, end, _params, _paths, _locals, _stripped = info
                if start <= offset < end and (best is None or end - start < best[2] - best[1]):
                    best = info
            return best

        def consider(kind: str, label: str, argument: str, offset: int) -> None:
            ident = _bare_ident(argument)
            if ident is None or ident in consts:
                return
            info = enclosing(offset)
            function = info[0] if info is not None else "<module>"
            if info is not None:
                _name, _start, _end, params, path_params, locals_, stripped = info
                if ident in stripped or ident in path_params:
                    return
                if ident not in params and ident not in locals_:
                    # An identifier that is neither a parameter nor a local is
                    # an outer const/static/associated item; not script data.
                    return
            findings.append(
                {
                    "file": rel,
                    "line": str(_line_of(raw, offset)),
                    "function": function,
                    "kind": kind,
                    "label": label,
                    "key": f"{rel} :: {function} :: {label}",
                }
            )

        def argument_at(open_idx: int, index: int = 0) -> str:
            close = match_delimiter(scrubbed, open_idx, "(", ")")
            if close < 0:
                return ""
            parts = _split_top_level(raw[open_idx + 1 : close])
            return parts[index] if index < len(parts) else ""

        def shown(argument: str) -> str:
            return _bare_ident(argument) or argument.strip()

        for match in _STORAGE_CALL.finditer(scrubbed):
            if match.group("recv") not in _STORAGE_RECEIVERS:
                continue
            method = match.group("method")
            arg = argument_at(match.end() - 1)
            consider("storage", f"{method}({shown(arg)})", arg, match.start())

        for match in _JOIN_CALL.finditer(scrubbed):
            arg = argument_at(match.end() - 1)
            consider("join", f"game_dir().join({shown(arg)})", arg, match.start())

        for match in _SEAM_CALL.finditer(scrubbed):
            if match.group("recv") not in _SEAM_RECEIVERS:
                continue
            method = match.group("method")
            arg = argument_at(match.end() - 1)
            consider("seam", f"source.{method}({shown(arg)})", arg, match.start())

        for match in _DECODED_AUDIO.finditer(scrubbed):
            if _is_definition(scrubbed, match):
                continue
            arg = argument_at(match.end() - 1, 1)
            consider("seam", f"decoded_audio(_, {shown(arg)})", arg, match.start())

        for match in _FSPATH_HOST.finditer(scrubbed):
            arg = argument_at(match.end() - 1)
            consider("host", f"FsPath::host({shown(arg)})", arg, match.start())

        for regex, name in ((_PATH_NEW, "Path::new"), (_PATHBUF_FROM, "PathBuf::from")):
            for match in regex.finditer(scrubbed):
                arg = argument_at(match.end() - 1)
                consider("path", f"{name}({shown(arg)})", arg, match.start())

        for match in _SAVE_CALL.finditer(scrubbed):
            method = match.group("method")
            # A write seam whose own body strips the media prefix is safe at
            # every call site; one that does not is the finding.
            if method in stripping_fns:
                continue
            arg = argument_at(match.end() - 1)
            consider("save", f"{method}({shown(arg)})", arg, match.start())

        for match in _HOST_WRITE.finditer(scrubbed):
            method = match.group("method")
            arg = argument_at(match.end() - 1, 1)
            consider("write", f"fs::{method}(_, {shown(arg)})", arg, match.start())

    return findings


def self_test(verbose: bool = True) -> int:
    tree = FIXTURES / "tree"
    expected_path = FIXTURES / "expected.json"
    for path in (tree, expected_path):
        if not path.exists():
            print(f"check-storage-boundary: self-test fixture missing: {path}")
            return 1
    expected = json.loads(expected_path.read_text(encoding="utf-8"))
    found = scan(tree)
    got = {finding["key"] for finding in found}
    want = set(expected["flagged"])
    if got != want:
        print("check-storage-boundary: self-test FAILED:")
        print(f"  flagged: got {sorted(got)}")
        print(f"           want {sorted(want)}")
        print("  Fix the extractor; do not silence the self-test.")
        return 1
    if verbose:
        print(
            "check-storage-boundary: self-test ok "
            f"({len(got)} flagged in the fixture, {len(want)} expected)"
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
        help="rewrite scripts/allowlists/storage_boundary_allow.txt with the findings",
    )
    parser.add_argument("--quiet", action="store_true", help="only print failures")
    args = parser.parse_args(argv)

    selftest = self_test(verbose=not args.quiet)
    if args.self_test:
        return selftest
    if selftest != 0:
        return 1

    if args.update_allow:
        findings = scan(ROOT)
        write_keyed_allowlist(
            ALLOWLIST,
            ALLOWLIST_HEADER,
            [f["key"] for f in findings],
            load_keyed_allowlist(ALLOWLIST, ("--",)),
        )
        print(
            f"check-storage-boundary: allowlist updated — {len(findings)} finding(s) "
            f"-> {ALLOWLIST.relative_to(ROOT)}"
        )
        return 0

    findings = scan(ROOT)
    allowed = load_keyed_allowlist(ALLOWLIST, ("--",))
    new = [f for f in findings if f["key"] not in allowed]
    stale = [key for key in allowed if key not in {f["key"] for f in findings}]

    if new or stale:
        if new:
            print(
                f"check-storage-boundary: {len(new)} script name(s) reach L2 or the "
                "host without strip_file_media:"
            )
            for finding in new:
                print(
                    f"  {finding['file']}:{finding['line']}  "
                    f"{finding['function']}  {finding['label']}"
                )
            print(
                "\n  Route the name through crate::storage::strip_file_media at the "
                "call site (a name that already carries a media scheme needs no "
                "stripping), or record a reviewed exception in "
                "scripts/allowlists/storage_boundary_allow.txt."
            )
        if stale:
            print(
                f"\ncheck-storage-boundary: {len(stale)} allowlisted call(s) are gone "
                "— shrink scripts/allowlists/storage_boundary_allow.txt:"
            )
            for key in sorted(stale):
                print(f"  {key}")
        return 1

    if not args.quiet:
        print(
            f"check-storage-boundary: clean ({len(findings)} finding(s), all "
            f"allowlisted; {len(allowed)} allowlisted)"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
