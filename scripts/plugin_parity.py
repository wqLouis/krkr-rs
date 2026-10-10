#!/usr/bin/env python3
"""Plugin-surface parity checker for krkr-rs.

This is the plugin analogue of ``scripts/native_parity.py``.  It machine-diffs
the script-visible surface of the reference KiriKiri plugins
(``reference/cpp/plugins/**``) against the Rust plugin ports
(``crates/compat/src/natives/plugins/**``) and reports per-class coverage.

How it works (short version)
----------------------------
* Reference surface: the ncbind registration macros in
  ``reference/cpp/plugins/**/*.{cpp,hpp,h}`` are scanned.  Class bodies
  (``NCB_REGISTER_CLASS``, ``NCB_REGISTER_SUBCLASS*``, ``NCB_ATTACH_CLASS*``)
  and the older ``TJS_BEGIN_NATIVE_MEMBERS(X)`` blocks attribute their members
  to the TJS target class; ``NCB_ATTACH_FUNCTION`` / ``_WITHTAG`` and
  ``NCB_REGISTER_FUNCTION`` attach free functions to a class.  Preprocessor
  regions disabled in a normal build (``#if 0``) are ignored, as are
  ``#define`` bodies (so the macros themselves are not mistaken for uses).
* Rust surface: every ``.rs`` file under ``crates/compat/src/natives/plugins``
  is parsed with ``scripts/native_parity.py`` helpers.  A class is declared by
  ``ctx.register_instance(&NativeInstanceBuilder { name: "X", .. })`` and
  attached members by ``ctx.register_static_members(&NativeStaticMembers {
  class_name: "X", .. })`` / ``ctx.register_instance_members(&
  NativeInstanceMembers { class_name: "X", .. })``.
* Missing = reference - rust.  A *plugin extra* is a member registered in a
  plugin file with no reference counterpart; core-native members and members
  inherited from a shared registration table (``EmotePlayer`` gets ``Player``'s
  table) are not extras.  Coverage is ``matched / reference``.

Ratchet
-------
``scripts/plugin_parity_allow.txt`` lists the currently accepted missing
members (one ``Class.member`` per line).  The checker fails when a *new*
missing member appears.  When an allowlisted member becomes implemented it is
reported as stale so the list can shrink.

Usage
-----
    python3 scripts/plugin_parity.py                 # check + regenerate report
    python3 scripts/plugin_parity.py --update-allow  # refresh the allowlist
    python3 scripts/plugin_parity.py --no-doc        # skip docs/plugin_parity.md
    python3 scripts/plugin_parity.py --quiet         # only print failures
    python3 scripts/plugin_parity.py --runtime target/runtime_surface.json \
        --no-doc                                     # check the live VM surface

Do not hand-edit ``docs/plugin_parity.md``: it is generated.
"""

from __future__ import annotations

import argparse
import datetime
import glob
import re
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent
sys.path.insert(0, str(SCRIPT_DIR))

import native_parity as np  # noqa: E402  (shares the Rust lexer/helpers)

REFERENCE_DIR = REPO_ROOT / "reference" / "cpp" / "plugins"
RUST_PLUGINS_DIR = REPO_ROOT / "crates" / "compat" / "src" / "natives" / "plugins"
# Core native classes (``Storages``, ``System``, ``Layer`` ...) live outside the
# plugin tree; reference plugins attach members to them, so the whole natives
# tree is globbed, not just ``natives/plugins/**``.
RUST_NATIVES_DIR = REPO_ROOT / "crates" / "compat" / "src" / "natives"
ALLOWLIST_PATH = SCRIPT_DIR / "plugin_parity_allow.txt"
DOC_PATH = REPO_ROOT / "docs" / "plugin_parity.md"

REFERENCE_GLOBS = ("**/*.cpp", "**/*.hpp", "**/*.h")

# Reference classes that are GDI+ rendering internals rather than game-script
# surface.  ``GdiPlus`` carries ~150 ``ENUM(...)`` drawing constants plus the
# ``GdipWrapper<...>`` bridge classes; those are intentionally not part of the
# parity metric.
EXCLUDED_REFERENCE_CLASSES = {"GdiPlus"}

# ``Debug.console`` is a dotted target: the Rust port attaches the members to
# the native ``Console`` class, so map the reference spelling onto it.
CLASS_ALIASES = {"Debug.console": "Console"}

# Rust classes that are the port of a reference class/extension under a
# different name.  ``Motion.Rect`` is registered as ``MotionRect`` in Rust
# because the core VM already owns a ``Rect`` class.
RUST_CLASS_ALIASES = {"MotionRect": "Rect"}

# Reference C++ helper structs that back a Rust class but are never registered
# through an ncbind macro (the plugin registers the members on ``Layer`` or not
# at all).  Map Rust class -> (file relative to the plugin dir, struct name).
HELPER_STRUCT_CLASSES = {
    "LayerExBase": ("layerExBase.hpp", "layerExBase"),
}

# Synthetic class for ``NCB_REGISTER_FUNCTION`` (truly global functions).
GLOBAL_CLASS = "__global__"

# Lifecycle hooks that are not script-visible members.  Applied to both sides
# so a port that ships an explicit ``finalize`` does not look like an extra.
IGNORED_MEMBERS = {"finalize"}

# Macros that are deliberately not script surface.  Kept here so the report can
# document the exclusions (mirrors the task's list).
EXCLUDED_REFERENCE_MACROS = [
    "NCB_MODULE_NAME",
    "NCB_PRE_REGIST_CALLBACK",
    "NCB_POST_REGIST_CALLBACK",
    "NCB_PRE_UNREGIST_CALLBACK",
    "NCB_POST_UNREGIST_CALLBACK",
    "NCB_SET_CONVERTOR / _SRC / _DST / _BOTH",
    "NCB_TYPECONV_* (boxing/convertor wiring)",
    "NCB_GDIP_* (GDI+ rendering internals)",
]


# ---------------------------------------------------------------------------
# C++ reference parsing
# ---------------------------------------------------------------------------

# Class-body macros.  The second tuple item is the index of the TJS class name
# argument; ``NCB_REGISTER_*`` use arg 0 while ``NCB_ATTACH_CLASS*`` use the
# second argument (the target class).
_BLOCK_RE = re.compile(
    r"\b(NCB_REGISTER_CLASS_DIFFER|NCB_REGISTER_CLASS_DELAY|NCB_REGISTER_CLASS"
    r"|NCB_REGISTER_SUBCLASS_DELAY|NCB_REGISTER_SUBCLASS"
    r"|NCB_ATTACH_CLASS_WITH_HOOK|NCB_ATTACH_CLASS)\s*\("
)
_ATTACH_MACROS = {"NCB_ATTACH_CLASS", "NCB_ATTACH_CLASS_WITH_HOOK"}

_TJS_MEMBERS_RE = re.compile(
    r"TJS_BEGIN_NATIVE_MEMBERS\s*\(\s*([A-Za-z_]\w*)\s*\)"
)
_TJS_END_MEMBERS_RE = re.compile(r"TJS_END_NATIVE_MEMBERS")

# Member declarations inside a class body.  ``NCB_METHOD*``/``NCB_PROPERTY*``
# cover every variant (``_DETAIL``, ``_PROXY``, ``_RAW_CALLBACK`` ...).  The
# remaining entries are the raw container calls the macros expand to, so the
# plugins that call ``RawCallback`` / ``Property`` / ``Method`` directly are
# still covered, plus a handful of file-local helper macros.
_MEMBER_RES = [
    re.compile(r"\bNCB_(?:METHOD|PROPERTY)[A-Z_]*\s*\(\s*([A-Za-z_]\w*)"),
    re.compile(r"\bNCB_MEMBER_PROPERTY\s*\(\s*([A-Za-z_]\w*)"),
    re.compile(r"\bNCB_ARG_PROPERTY_RO\s*\(\s*([A-Za-z_]\w*)"),
    re.compile(
        r"\b(?:RawCallback|Property|Method|Variant)\s*\(\s*"
        r"(?:TJS_W\s*\(\s*)?\"([^\"]+)\""
    ),
    re.compile(r"\bproperty_delegate\s*\(\s*([A-Za-z_]\w*)"),
    re.compile(r"\bINTPROP\s*\(\s*([A-Za-z_]\w*)"),
    re.compile(r"\bENUM\s*\(\s*([A-Za-z_]\w*)"),
    re.compile(r"TJS_BEGIN_NATIVE_(?:STATIC_)?(?:METHOD|PROP)_DECL\s*\(\s*([A-Za-z_]\w*)"),
]

# Constructor registration: the class object is added as a member named after
# the class and dropped from both sides of the diff later (it is not a
# script-callable member).
_CONSTRUCTOR_RE = re.compile(r"\b(?:NCB_CONSTRUCTOR|Constructor)\s*\(")

# Attached free functions.
_ATTACH_FUNCTION_RE = re.compile(
    r"\bNCB_ATTACH_FUNCTION\s*\(\s*([A-Za-z_]\w*)\s*,\s*([A-Za-z_]\w*)\s*,"
)
# Manual ``addMethod(dispatch, TJS_W("name"), ..)`` registration (used by
# ``layerExPerspective.cpp`` instead of an ncbind macro block).
_ADDMETHOD_RE = re.compile(
    r"\baddMethod\s*\(\s*([A-Za-z_]\w*)\s*,\s*TJS_W\s*\(\s*\"([^\"]+)\""
)
_ATTACH_FUNCTION_TAG_RE = re.compile(
    r"\bNCB_ATTACH_FUNCTION_WITHTAG\s*\(\s*([A-Za-z_]\w*)\s*,\s*"
    r"([A-Za-z_]\w*)\s*,\s*([A-Za-z_][\w.]*)\s*,"
)
_REGISTER_FUNCTION_RE = re.compile(
    r"\bNCB_REGISTER_FUNCTION\s*\(\s*([A-Za-z_]\w*)\s*,"
)
# ``TJSCreateNativeClassForPlugin(TJS_W("X"), ..)`` names the TJS class for
# the older ``TJS_BEGIN_NATIVE_MEMBERS`` style.  The members come from the
# member block; this only guarantees the class is tracked even if empty.
_TJS_CREATE_CLASS_RE = re.compile(
    r"TJSCreateNativeClassForPlugin\s*\(\s*TJS_W\s*\(\s*\"([^\"]+)\""
)


def _blank_directives(src: str) -> str:
    """Blank ``#...`` lines (including line continuations).

    ``native_parity._preprocess_cpp`` already removes ``#if``/``#else``
    regions.  What remains are ``#define`` bodies, which contain macro
    *definitions* such as ``#define NCB_GDIP_METHOD(name) Method(...)`` that
    must not be mistaken for real registrations.
    """
    out: list[str] = []
    in_directive = False
    for line in src.split("\n"):
        if in_directive:
            out.append(" " * len(line))
            if not line.rstrip().endswith("\\"):
                in_directive = False
            continue
        if line.lstrip().startswith("#"):
            out.append(" " * len(line))
            if line.rstrip().endswith("\\"):
                in_directive = True
            continue
        out.append(line)
    return "\n".join(out)


def _normalize_class(name: str) -> str:
    name = name.strip()
    return CLASS_ALIASES.get(name, name)


def _split_call_args(src: str, open_paren: int) -> tuple[list[str], int]:
    """Split the argument list of a call whose ``(`` is at ``open_paren``.

    Returns ``(args, index_after_closing_paren)``.  Only parens/brackets/braces
    are tracked, which is enough for the ncbind macros (a template comma inside
    an argument is not used by any registration macro in this tree).
    """
    depth = 0
    args: list[str] = []
    cur: list[str] = []
    i = open_paren + 1
    n = len(src)
    while i < n:
        c = src[i]
        if c in "([{":
            depth += 1
            cur.append(c)
        elif c in ")]}":
            if c == ")" and depth == 0:
                args.append("".join(cur).strip())
                return args, i + 1
            depth -= 1
            cur.append(c)
        elif c == "," and depth == 0:
            args.append("".join(cur).strip())
            cur = []
        else:
            cur.append(c)
        i += 1
    args.append("".join(cur).strip())
    return args, n


def _collect_class_spans(src: str) -> list[tuple[int, int, str, "str | None"]]:
    """Return ``(start, end, class_name, impl_name)`` for every class block.

    ``impl_name`` is the implementation class of an ``NCB_ATTACH_CLASS*`` block
    (the first argument).  A Rust port may be named after the implementation
    rather than the TJS target, so it is tracked separately.
    """
    spans: list[tuple[int, int, str, "str | None"]] = []
    for m in _BLOCK_RE.finditer(src):
        macro = m.group(1)
        args, close = _split_call_args(src, m.end() - 1)
        j = close
        while j < len(src) and src[j] in " \t\r\n":
            j += 1
        if j >= len(src) or src[j] != "{":
            continue
        end = np._match_brace(src, j)
        impl = None
        if macro in _ATTACH_MACROS:
            impl = args[0] if args else None
            cls = args[1] if len(args) >= 2 else None
        else:
            cls = args[0] if args else None
        if cls:
            spans.append((m.start(), end, _normalize_class(cls), impl))

    for m in _TJS_MEMBERS_RE.finditer(src):
        end_m = _TJS_END_MEMBERS_RE.search(src, m.end())
        end = end_m.start() if end_m else len(src)
        spans.append((m.start(), end, _normalize_class(m.group(1)), None))
    return spans


def _innermost(
    spans: list[tuple[int, int, str, "str | None"]], pos: int
) -> "tuple[int, int, str, str | None] | None":
    containing = [s for s in spans if s[0] < pos < s[1]]
    if not containing:
        return None
    return min(containing, key=lambda s: s[1] - s[0])


def _canon_class(name: str) -> str:
    """Canonicalise a C++ implementation name to the Rust class spelling."""
    return name[:1].upper() + name[1:] if name else name


def _helper_struct_members(path: Path, struct_name: str) -> set[str]:
    """Members of a C++ helper struct that no ncbind macro ever registers.

    ``LayerExBase`` is the only such case: the reference exposes its members on
    ``Layer`` through ``ObjectCache``, while the Rust port models it as its own
    class.  Public method declarations and the ``TJS_W("...")`` property
    names in the constructor initialiser list form the surface.
    """
    if not path.is_file():
        return set()
    raw = path.read_text(encoding="utf-8", errors="replace")
    src = _blank_directives(np._strip_cpp_comments(np._preprocess_cpp(raw)))
    m = re.search(r"\bstruct\s+" + re.escape(struct_name) + r"\b", src)
    if not m:
        return set()
    brace = src.find("{", m.end())
    if brace == -1:
        return set()
    body = src[brace : np._match_brace(src, brace)]
    members = set(re.findall(r'TJS_W\("([^"]+)"\)', body))
    for mm in re.finditer(
        r"\b(?:virtual\s+)?(?:void|bool|int|tjs_int|tjs_real|"
        r"GeometryT|BufferT|PitchT|DispatchT|StringT)\s+([a-z_]\w*)\s*\(",
        body,
    ):
        members.add(mm.group(1))
    return members


def _iter_reference_files() -> list[Path]:
    if not REFERENCE_DIR.is_dir():
        raise SystemExit(
            "plugin_parity: reference plugins directory not found: "
            f"{REFERENCE_DIR.relative_to(REPO_ROOT)}"
        )
    files: list[Path] = []
    seen: set[str] = set()
    for pat in REFERENCE_GLOBS:
        for fp in sorted(glob.glob(str(REFERENCE_DIR / pat), recursive=True)):
            if fp not in seen:
                seen.add(fp)
                files.append(Path(fp))
    if not files:
        raise SystemExit(
            f"plugin_parity: no reference plugin sources under "
            f"{REFERENCE_DIR.relative_to(REPO_ROOT)} — update REFERENCE_GLOBS."
        )
    return files


def parse_reference() -> tuple[dict[str, set[str]], dict[str, set[str]]]:
    """Return ``(classes, impl_classes)``.

    ``impl_classes`` maps an ``NCB_ATTACH_CLASS*`` implementation class (or the
    file stem of a manual ``addMethod`` registration) to its members, so a Rust
    class named after the implementation is matched to the plugin surface.
    """
    classes: dict[str, set[str]] = {}
    impl_classes: dict[str, set[str]] = {}
    for fp in _iter_reference_files():
        raw = fp.read_text(encoding="utf-8", errors="replace")
        src = _blank_directives(np._strip_cpp_comments(np._preprocess_cpp(raw)))
        spans = _collect_class_spans(src)

        for regex in _MEMBER_RES:
            for m in regex.finditer(src):
                span = _innermost(spans, m.start())
                if span is None or span[2] in EXCLUDED_REFERENCE_CLASSES:
                    continue
                classes.setdefault(span[2], set()).add(m.group(1))
                if span[3]:
                    impl_classes.setdefault(span[3], set()).add(m.group(1))

        for m in _CONSTRUCTOR_RE.finditer(src):
            span = _innermost(spans, m.start())
            if span is None or span[2] in EXCLUDED_REFERENCE_CLASSES:
                continue
            classes.setdefault(span[2], set()).add(span[2])

        for m in _ATTACH_FUNCTION_RE.finditer(src):
            cls = _normalize_class(m.group(2))
            if cls not in EXCLUDED_REFERENCE_CLASSES:
                classes.setdefault(cls, set()).add(m.group(1))
        for m in _ATTACH_FUNCTION_TAG_RE.finditer(src):
            cls = _normalize_class(m.group(3))
            if cls not in EXCLUDED_REFERENCE_CLASSES:
                classes.setdefault(cls, set()).add(m.group(1))
        for m in _REGISTER_FUNCTION_RE.finditer(src):
            classes.setdefault(GLOBAL_CLASS, set()).add(m.group(1))
        for m in _TJS_CREATE_CLASS_RE.finditer(src):
            cls = _normalize_class(m.group(1))
            if cls not in EXCLUDED_REFERENCE_CLASSES:
                classes.setdefault(cls, set())

        # Manual ``addMethod(dispatch, TJS_W("name"), ..)`` registrations: the
        # target class is the class named in the nearest previous ``PropGet``.
        # Record the member under the file stem too, so a Rust class named after
        # the plugin is matched (``layerExPerspective`` -> ``LayerExPerspective``).
        for m in _ADDMETHOD_RE.finditer(src):
            name = m.group(2)
            target = None
            for pm in re.finditer(
                r'PropGet\s*\([^;]*?TJS_W\s*\(\s*"([^"]+)"', src[: m.start()]
            ):
                target = pm.group(1)
            if target:
                cls = _normalize_class(target)
                if cls not in EXCLUDED_REFERENCE_CLASSES:
                    classes.setdefault(cls, set()).add(name)
            impl_classes.setdefault(fp.stem, set()).add(name)

    for rust_cls, (rel, struct_name) in HELPER_STRUCT_CLASSES.items():
        members = _helper_struct_members(REFERENCE_DIR / rel, struct_name)
        if members:
            classes.setdefault(rust_cls, set()).update(members)

    return classes, impl_classes


# ---------------------------------------------------------------------------
# Rust registration parsing
# ---------------------------------------------------------------------------

# A helper call inside a builder whose first string argument is the member
# name, e.g. ``static_int("nchtError", getter)``.
_HELPER_CALL_RE = re.compile(r'\b([A-Za-z_]\w*)\s*\(\s*"([^"]+)"')
_FOR_IN_RE = re.compile(r"\bfor\s+([A-Za-z_]\w*)\s+in\s+([A-Za-z_]\w*)")

# Local member-table macros: the first string literal is the member name.
# ``method!`` / ``prop!`` are declared inside a registration function;
# ``property!`` / ``m!`` / ``p!`` are the other spellings used in the tree.
_MEMBER_MACRO_NAMES = {"method", "property", "prop", "meth", "m", "p"}
_MACRO_MEMBER_RE = re.compile(r'\b([A-Za-z_]\w*)!\s*\(\s*"([^"]+)"')

# A function returning a vector of member definitions.  The broad ``Native[A-Za-z]*``
# tail covers ``NativeObjThisMethodDef``/``NativeInstancePropertyDef`` too.
_DEF_FN_RE = re.compile(
    r"fn\s+([A-Za-z_]\w*)\s*\([^)]*\)\s*->\s*[^{;]*"
    r"Native[A-Za-z]*(?:Method|Property)Def"
)
_BARE_CALL_RE = re.compile(r"([A-Za-z_]\w*(?:::[A-Za-z_]\w*)*)\s*\(\s*\)")


def _split_top_level(text: str) -> list[str]:
    """Split a comma-separated list, ignoring separators inside brackets."""
    parts: list[str] = []
    cur: list[str] = []
    depth = 0
    for c in text:
        if c in "([{<":
            depth += 1
        elif c in ")]}>" and depth > 0:
            depth -= 1
        if c == "," and depth == 0:
            parts.append("".join(cur))
            cur = []
        else:
            cur.append(c)
    parts.append("".join(cur))
    return [p.strip() for p in parts if p.strip()]


def _scan_to_semicolon(code: str, start: int) -> int:
    """Index of the top-level ``;`` (or the end of the file)."""
    depth = 0
    i = start
    while i < len(code):
        c = code[i]
        if c in "([{":
            depth += 1
        elif c in ")]}":
            if depth == 0:
                return i
            depth -= 1
        elif c == ";" and depth == 0:
            return i
        i += 1
    return len(code)


def _scan_to_field_end(code: str, start: int, limit: int) -> int:
    """Index of the top-level ``,`` / ``}`` ending a struct field value."""
    depth = 0
    i = start
    while i < limit:
        c = code[i]
        if c in "([{":
            depth += 1
        elif c in ")]}":
            if depth == 0:
                return i
            depth -= 1
        elif c == "," and depth == 0:
            return i
        i += 1
    return limit


def _scan_paren(code: str, start: int) -> int:
    """Index of the ``)`` matching the ``(`` just before ``start``."""
    depth = 1
    i = start
    while i < len(code):
        c = code[i]
        if c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
            if depth == 0:
                return i
        i += 1
    return len(code)


def _rust_functions(code: str) -> list[tuple[str, list[str], int, int]]:
    """Return ``(name, params, body_start, body_end)`` for each ``fn``."""
    out: list[tuple[str, list[str], int, int]] = []
    for m in re.finditer(r"\bfn\s+([A-Za-z_]\w*)\s*(?:<[^>{};]*>)?\s*\(", code):
        open_paren = m.end() - 1
        close = _scan_paren(code, open_paren + 1)
        brace = code.find("{", close)
        if brace == -1:
            continue
        end = np._match_brace(code, brace)
        params = []
        for p in _split_top_level(code[open_paren + 1 : close]):
            pm = re.match(r"\s*(?:mut\s+)?([A-Za-z_]\w*)", p)
            if pm:
                params.append(pm.group(1))
        out.append((m.group(1), params, brace, end))
    return out


def _local_defs(
    code: str, strings: dict[int, tuple[int, str, int]], raw: str
) -> dict[str, set[str]]:
    """``{fn_name: members}`` for def-returning functions in one file."""
    out: dict[str, set[str]] = {}
    for m in _DEF_FN_RE.finditer(code):
        name = m.group(1)
        brace = code.find("{", m.end())
        if brace == -1:
            continue
        end = np._match_brace(code, brace)
        mem = np._extract_members(code, strings, brace, end)
        for lit in re.finditer(r'\b[A-Za-z_]\w*\s*\(\s*"([^"]+)"', raw[brace:end]):
            mem.add(lit.group(1))
        # Some tables use a plain `("name", getter)` tuple array (the
        # ``motion_properties`` map) rather than a ``name:`` field; every
        # literal in a def-returning body is a member name.
        if not mem:
            mem.update(re.findall(r'"([^"]+)"', raw[brace:end]))
        out[name] = mem
    return out


def _lookup_fn(
    callee: str,
    stem: str,
    global_defs: dict[tuple[str, str], set[str]],
    local_defs: dict[str, set[str]],
) -> set[str]:
    """Resolve ``helper()`` / ``module::helper()`` / ``super::module::helper()``."""
    parts = callee.split("::")
    name = parts[-1]
    if len(parts) == 1:
        if name in local_defs:
            return set(local_defs[name])
        return set(global_defs.get((stem, name), ()))
    mod = parts[-2]
    if (mod, name) in global_defs:
        return set(global_defs[(mod, name)])
    hits = [v for (s, n), v in global_defs.items() if n == name]
    if len(hits) == 1:
        return set(hits[0])
    return set()


def _resolve_region(
    code: str,
    strings: dict[int, tuple[int, str, int]],
    start: int,
    end: int,
    stem: str,
    global_defs: dict[tuple[str, str], set[str]],
    local_defs: dict[str, set[str]],
    variables: dict[str, set[str]],
) -> set[str]:
    """Members denoted by the expression in ``code[start:end]``."""
    text = code[start:end].strip()
    if text.startswith("vec!"):
        mem = np._extract_members(code, strings, start, end)
        return mem or np._extract_members(code, strings, start, end, bare=True)
    m = _BARE_CALL_RE.fullmatch(text)
    if m:
        return _lookup_fn(m.group(1), stem, global_defs, local_defs)
    m = re.fullmatch(r"([A-Za-z_]\w*)", text)
    if m:
        return set(variables.get(m.group(1), ()))
    return np._extract_members(code, strings, start, end)


def _stringify_macro_members(
    code: str, strings: dict[int, tuple[int, str, int]]
) -> set[str]:
    """Members from ``macro! { name = value; ... }`` name-table invocations.

    ``psd_constant_properties!`` generates one property per invocation entry via
    ``stringify!($name)``; the generated table has no literal ``name:`` field for
    the parser to find, so the invocation block itself is the source of truth.
    """
    out: set[str] = set()
    for m in re.finditer(r"macro_rules!\s+([A-Za-z_]\w*)", code):
        name = m.group(1)
        brace = code.find("{", m.end())
        if brace == -1:
            continue
        body = code[brace : np._match_brace(code, brace)]
        if "stringify!" not in body and "name: $name" not in body:
            continue
        for inv in re.finditer(r"\b" + re.escape(name) + r"!\s*\{", code):
            ibrace = code.index("{", inv.end() - 1)
            iend = np._match_brace(code, ibrace)
            block = code[ibrace:iend]
            for em in re.finditer(r"(?m)^\s*([A-Za-z_]\w*)\s*=", block):
                out.add(em.group(1))
    return out


def _all_builder_spans(
    code: str, strings: dict[int, tuple[int, str, int]], raw: str
) -> list[tuple[int, int, "str | None", "int | None"]]:
    """Like ``native_parity._builder_spans`` but keeps dynamic builders.

    ``_builder_spans`` drops a builder whose ``name:`` is a parameter (a
    literal or ``const``-resolvable name is required).  Those builders are
    exactly the ones whose members must be attributed at the call site, so they
    are re-added here with ``class_name=None``.
    """
    spans = list(np._builder_spans(code, strings, raw))
    covered = {s[0] for s in spans}
    consts = np._const_strings(code, strings)
    for m in re.finditer(
        r"Native(?:Instance|Class)Builder\s*\{"
        r"|Native(?:Static|Instance)Members\s*\{",
        code,
    ):
        open_pos = code.index("{", m.start())
        if open_pos in covered:
            continue
        end = np._match_brace(code, open_pos)
        name_text = raw[open_pos:end]
        first = re.search(r"\b(?:class_)?name\s*:\s*", name_text)
        if not first:
            continue
        rest = name_text[first.end() :]
        m_str = re.match(r'"([^"]+)"', rest)
        m_id = re.match(r"([A-Za-z_]\w*)", rest)
        cls = None
        name_lit = None
        if m_str:
            cls = m_str.group(1)
            name_lit = open_pos + first.end() + m_str.start(0)
        elif m_id:
            cls = consts.get(m_id.group(1))
        spans.append((open_pos, end, cls, name_lit))
    return spans


def _iter_rust_files() -> list[Path]:
    if not RUST_NATIVES_DIR.is_dir():
        raise SystemExit(
            "plugin_parity: Rust natives directory not found: "
            f"{RUST_NATIVES_DIR.relative_to(REPO_ROOT)}"
        )
    # Core native files (``natives/*.rs``) plus the plugin tree
    # (``natives/plugins/**``): reference plugins attach members to core classes
    # such as ``Storages`` / ``System`` / ``Scripts``.
    files = sorted(RUST_NATIVES_DIR.glob("*.rs"))
    files += sorted(RUST_PLUGINS_DIR.rglob("*.rs"))
    if not files:
        raise SystemExit(
            "plugin_parity: no Rust native sources under "
            f"{RUST_NATIVES_DIR.relative_to(REPO_ROOT)} — natives were moved."
        )
    return files


def _parse_rust_file(
    path: Path,
    raw: str,
    code: str,
    strings: dict[int, tuple[int, str, int]],
    local_defs: dict[str, set[str]],
    global_defs: dict[tuple[str, str], set[str]],
) -> tuple[dict[str, set[str]], list[tuple[str, int, "int | None", set[str], set[str]]]]:
    """Parse one Rust file.

    Returns ``(class_members, dynamic_builders)`` where each dynamic builder is
    ``(fn_name, class_param_index, emote_param_index, base_members,
    emote_members)``: the class name is supplied by the call site rather than a
    literal in the builder.
    """
    stem = path.stem
    result: dict[str, set[str]] = {}
    dynamic: list[tuple[str, int, "int | None", set[str], set[str]]] = []

    # Precompute each function's local variables (``methods`` / ``properties``
    # vectors built with ``let``, ``.extend`` and ``.push``) and the ``if emote``
    # ranges used to split conditional members.
    func_infos: list[tuple[str, list[str], int, int, dict[str, set[str]], list[tuple[int, int]]]] = []
    for fname, params, bstart, bend in _rust_functions(code):
        body = code[bstart:bend]
        variables: dict[str, set[str]] = {}
        for m in re.finditer(r"\blet\s+(?:mut\s+)?([A-Za-z_]\w*)\s*(?::[^=;]*)?=", body):
            var = m.group(1)
            s = bstart + m.end()
            e = _scan_to_semicolon(code, s)
            variables.setdefault(var, set()).update(
                _resolve_region(code, strings, s, e, stem, global_defs, local_defs, variables)
            )
        for m in re.finditer(r"\b([A-Za-z_]\w*)\s*\.extend\(\s*", body):
            var = m.group(1)
            s = bstart + m.end()
            e = _scan_paren(code, s)
            variables.setdefault(var, set()).update(
                _resolve_region(code, strings, s, e, stem, global_defs, local_defs, variables)
            )
        for m in re.finditer(r"\b([A-Za-z_]\w*)\s*\.push\(\s*", body):
            var = m.group(1)
            s = bstart + m.end()
            e = _scan_paren(code, s)
            variables.setdefault(var, set()).update(
                np._extract_members(code, strings, s, e, bare=True)
            )
        emote_ranges: list[tuple[int, int]] = []
        for em in re.finditer(r"\bif\s+emote\s*\{", body):
            es = bstart + em.start()
            ebrace = code.find("{", es)
            emote_ranges.append((es, np._match_brace(code, ebrace)))
        func_infos.append((fname, params, bstart, bend, variables, emote_ranges))

    for open_pos, end, cls, name_lit in _all_builder_spans(code, strings, raw):
        enclosing = None
        for info in func_infos:
            if info[2] <= open_pos < info[3]:
                enclosing = info
                break

        members = np._extract_members(
            code,
            strings,
            open_pos,
            end,
            exclude={name_lit} if name_lit is not None else set(),
        )
        for m in _HELPER_CALL_RE.finditer(raw[open_pos:end]):
            if name_lit is not None and open_pos + m.start(2) == name_lit:
                continue
            members.add(m.group(2))
        for m in _MACRO_MEMBER_RE.finditer(raw[open_pos:end]):
            if m.group(1) in _MEMBER_MACRO_NAMES:
                members.add(m.group(2))

        if enclosing is not None:
            _fname, _params, _bstart, _bend, variables, _ranges = enclosing
            for kind in ("methods", "properties"):
                for m in re.finditer(r"\b" + kind + r"\s*:\s*", code[open_pos:end]):
                    s = open_pos + m.end()
                    e = _scan_to_field_end(code, s, end)
                    members |= _resolve_region(
                        code, strings, s, e, stem, global_defs, local_defs, variables
                    )
                if re.search(r"\b" + kind + r"\s*[,}]", code[open_pos:end]):
                    members |= set(variables.get(kind, ()))

        if cls is not None:
            result.setdefault(cls, set()).update(members)
            continue

        # Dynamic builder: ``name: <parameter>``.  The class name is the
        # literal at the call site (``register_named(ctx, "Player", ..)``).
        if enclosing is None:
            continue
        fname, params, bstart, bend, variables, emote_ranges = enclosing
        name_m = re.search(r"\bname\s*:\s*([A-Za-z_]\w*)", code[open_pos:end])
        if not name_m or name_m.group(1) not in params:
            continue
        class_idx = params.index(name_m.group(1))
        emote_idx = params.index("emote") if "emote" in params else None
        base_macros: set[str] = set()
        emote_macros: set[str] = set()
        for m in _MACRO_MEMBER_RE.finditer(raw[bstart:bend]):
            if m.group(1) not in _MEMBER_MACRO_NAMES:
                continue
            pos = bstart + m.start()
            if any(a <= pos < b for a, b in emote_ranges):
                emote_macros.add(m.group(2))
            else:
                base_macros.add(m.group(2))
        dynamic.append(
            (fname, class_idx, emote_idx, members | base_macros, emote_macros)
        )

    # ``for x in NAMES`` loops append a string array to a member vector.
    classes = list(result)
    if classes:
        for fm in _FOR_IN_RE.finditer(code):
            vals = _resolve_str_array(code, strings, fm.group(2))
            if not vals:
                continue
            for cls in classes:
                result[cls] |= vals

    # Macro-generated name tables (``psd_constant_properties!``).
    macro_members = _stringify_macro_members(code, strings)
    if macro_members:
        for cls in result:
            result[cls] |= macro_members

    return result, dynamic


def _resolve_str_array(
    code: str, strings: dict[int, tuple[int, str, int]], var: str
) -> set[str]:
    """Resolve ``const NAME: &[&str] = &[..]`` / ``let mut NAME = vec![..]``."""
    m = re.search(
        r"\b(?:const|static|let)\s+(?:mut\s+)?"
        + re.escape(var)
        + r"\b[^=]*?=",
        code,
    )
    if not m:
        return set()
    rhs = m.end()
    bracket = code.find("[", rhs)
    if bracket == -1:
        return set()
    between = code[rhs:bracket]
    if not re.fullmatch(r"[\s&]*(?:vec\s*!\s*)?", between):
        return set()
    end = np._match_bracket(code, bracket)
    return np._extract_members(code, strings, bracket, end, bare=True)


def _is_plugin_file(path: Path) -> bool:
    """True when ``path`` lives under ``crates/compat/src/natives/plugins``."""
    try:
        path.relative_to(RUST_PLUGINS_DIR)
        return True
    except ValueError:
        return False


def parse_rust() -> tuple[
    dict[str, set[str]],
    dict[str, list[str]],
    dict[str, set[str]],
    dict[str, set[str]],
    list[tuple[set[str], set[str], "str | None"]],
]:
    """Return ``(members, sources, plugin_only, core_members, dynamic_groups)``.

    ``plugin_only`` holds the members registered by a plugin file that no core
    file also registers; those are the only candidates for a *plugin* extra.
    ``dynamic_groups`` describes each shared registration table as
    ``(classes, base_members, primary_class)`` so inherited members can be
    identified (``EmotePlayer`` inherits ``Player``'s table).
    """
    files: list[tuple[Path, str, str, dict[int, tuple[int, str, int]], dict[str, set[str]]]] = []
    global_defs: dict[tuple[str, str], set[str]] = {}
    for path in _iter_rust_files():
        raw = path.read_text(encoding="utf-8", errors="replace")
        code, strings = np._lex_rust(raw)
        local = _local_defs(code, strings, raw)
        for name, mem in local.items():
            global_defs[(path.stem, name)] = mem
        files.append((path, raw, code, strings, local))

    members: dict[str, set[str]] = {}
    sources: dict[str, list[str]] = {}
    plugin_members: dict[str, set[str]] = {}
    core_members: dict[str, set[str]] = {}
    dynamic: list[tuple[str, int, "int | None", set[str], set[str], Path]] = []
    for path, raw, code, strings, local in files:
        res, dyn = _parse_rust_file(path, raw, code, strings, local, global_defs)
        rel = str(path.relative_to(REPO_ROOT))
        target = plugin_members if _is_plugin_file(path) else core_members
        for cls, mset in res.items():
            members.setdefault(cls, set()).update(mset)
            sources.setdefault(cls, []).append(rel)
            target.setdefault(cls, set()).update(mset)
        dynamic.extend((*d, path) for d in dyn)

    # Resolve dynamic builders: attribute their members to the literal class
    # name supplied at each call site (``register_named(ctx, "Player", ..)``).
    groups: list[tuple[set[str], set[str], "str | None"]] = []
    for fname, class_idx, emote_idx, base, emote, origin in dynamic:
        target = plugin_members if _is_plugin_file(origin) else core_members
        origin_rel = str(origin.relative_to(REPO_ROOT))
        group_classes: set[str] = set()
        primary = None
        for path, raw, code, _strings, _local in files:
            for m in re.finditer(r"\b" + re.escape(fname) + r"\s*\(", code):
                arg_start = m.end()
                arg_end = _scan_paren(code, arg_start)
                args = _split_top_level(raw[arg_start:arg_end])
                if class_idx >= len(args):
                    continue
                lm = re.match(r'\s*"([^"]+)"', args[class_idx])
                if not lm:
                    continue
                cls = lm.group(1)
                group_classes.add(cls)
                is_emote = (
                    emote_idx is not None
                    and emote_idx < len(args)
                    and bool(re.search(r"\btrue\b", args[emote_idx]))
                )
                if emote_idx is not None and not is_emote:
                    primary = cls
                members.setdefault(cls, set()).update(base)
                target.setdefault(cls, set()).update(base)
                if origin_rel not in sources.setdefault(cls, []):
                    sources[cls].append(origin_rel)
                if is_emote:
                    members[cls] |= emote
                    target[cls] |= emote
        if group_classes:
            if primary is None:
                primary = sorted(group_classes)[0]
            groups.append((group_classes, base, primary))

    plugin_only = {
        cls: set(mset) - core_members.get(cls, set())
        for cls, mset in plugin_members.items()
    }
    return members, sources, plugin_only, core_members, groups


# ---------------------------------------------------------------------------
# Allowlist + report
# ---------------------------------------------------------------------------


def read_allowlist(path: Path) -> set[str]:
    if not path.exists():
        return set()
    entries: set[str] = set()
    for line in path.read_text(encoding="utf-8").splitlines():
        item = line.strip()
        if not item or item.startswith("#"):
            continue
        entries.add(item)
    return entries


def write_allowlist(path: Path, entries: set[str]) -> None:
    header = (
        "# Plugin parity allowlist: reference members that are known-missing.\n"
        "# One entry per line: Class.member\n"
        "# Regenerate after intentional additions with:\n"
        "#   python3 scripts/plugin_parity.py --update-allow\n"
        "# The checker fails when a missing member is not listed here.\n"
        "\n"
    )
    body = "\n".join(sorted(entries))
    path.write_text(header + body + ("\n" if body else ""), encoding="utf-8")


def build_report(
    ref: dict[str, set[str]],
    rust: dict[str, set[str]],
    rust_sources: dict[str, list[str]],
    missing_by_class: dict[str, set[str]],
    extra_by_class: dict[str, set[str]],
    core_by_class: dict[str, set[str]],
    tracked: list[str],
) -> str:
    total_ref = sum(len(ref.get(c, ())) for c in tracked)
    total_matched = sum(len(ref.get(c, set()) & rust.get(c, set())) for c in tracked)
    total_missing = sum(len(missing_by_class.get(c, ())) for c in tracked)
    total_extra = sum(len(extra_by_class.get(c, ())) for c in tracked)
    total_core = sum(len(core_by_class.get(c, ())) for c in tracked)
    coverage = (100.0 * total_matched / total_ref) if total_ref else 0.0

    lines: list[str] = []
    lines.append("# Plugin surface parity")
    lines.append("")
    lines.append(
        "> **Generated** by `scripts/plugin_parity.py` on "
        f"{datetime.date.today().isoformat()} — do not edit by hand."
    )
    lines.append("")
    lines.append(
        "Machine-diff of the reference KiriKiri plugin surface "
        "(`reference/cpp/plugins/**`) against the plugin classes the Rust "
        "engine registers (`crates/compat/src/natives/plugins/**`). A "
        "*missing* member exists in the reference but is not registered by "
        "Rust. An *extra* is a genuine plugin-surface addition: a member "
        "registered by a plugin file that the reference class does not have. "
        "Core natives (`natives/*.rs`) and members inherited from a shared "
        "registration table are counted separately."
    )
    lines.append("")
    lines.append("## Usage")
    lines.append("")
    lines.append("```sh")
    lines.append("python3 scripts/plugin_parity.py                 # check + regenerate this report")
    lines.append("python3 scripts/plugin_parity.py --no-doc        # check only")
    lines.append("python3 scripts/plugin_parity.py --update-allow  # accept current gaps after review")
    lines.append("```")
    lines.append("")
    lines.append(
        "The checker exits non-zero when a missing member is **not** listed in "
        "`scripts/plugin_parity_allow.txt` (a regression). When an allowlisted "
        "member is implemented it prints a stale-entry warning so the allowlist "
        "can shrink. Coverage can therefore only move in one direction."
    )
    lines.append("")
    lines.append("## Coverage summary")
    lines.append("")
    lines.append("| metric | value |")
    lines.append("| --- | ---: |")
    lines.append(f"| tracked classes | {len(tracked)} |")
    lines.append(f"| reference members | {total_ref} |")
    lines.append(f"| matched | {total_matched} |")
    lines.append(f"| **missing** | **{total_missing}** |")
    lines.append(f"| extra (genuine plugin additions) | {total_extra} |")
    lines.append(f"| core members (not plugin extras) | {total_core} |")
    lines.append(f"| coverage | **{coverage:.1f}%** |")
    lines.append("")

    lines.append("## Per-class coverage")
    lines.append("")
    lines.append("| class | ref | rust | matched | missing | extra | coverage |")
    lines.append("| --- | ---: | ---: | ---: | ---: | ---: | ---: |")
    for cls in tracked:
        r = ref.get(cls, set())
        u = rust.get(cls, set())
        matched = len(r & u)
        cov = f"{100.0 * matched / len(r):.1f}%" if r else "n/a"
        lines.append(
            f"| `{cls}` | {len(r)} | {len(u)} | {matched} | "
            f"{len(missing_by_class.get(cls, ()))} | "
            f"{len(extra_by_class.get(cls, ()))} | {cov} |"
        )
    lines.append("")

    lines.append("## Missing members by class")
    lines.append("")
    if not total_missing:
        lines.append("_None for tracked classes._")
    for cls in tracked:
        miss = sorted(missing_by_class.get(cls, ()))
        if not miss:
            continue
        lines.append(f"### `{cls}` — {len(miss)} missing")
        lines.append("")
        lines.append(", ".join(f"`{m}`" for m in miss))
        lines.append("")

    lines.append("## Extra members by class")
    lines.append("")
    lines.append(
        "Genuine plugin-surface additions: members registered in a plugin file "
        "that the reference class does not have. Core natives and members "
        "inherited from a shared registration table (``EmotePlayer`` gets "
        "``Player``'s table) are excluded."
    )
    lines.append("")
    if not total_extra:
        lines.append("_None for tracked classes._")
    for cls in tracked:
        extra = sorted(extra_by_class.get(cls, ()))
        if not extra:
            continue
        lines.append(f"### `{cls}` — {len(extra)} extra")
        lines.append("")
        lines.append(", ".join(f"`{m}`" for m in extra))
        lines.append("")

    lines.append("## Core members by class (excluded from extras)")
    lines.append("")
    lines.append(
        "Members registered by a core file (``crates/compat/src/natives/*.rs``) "
        "with no reference-plugin counterpart. These are not plugin-surface "
        "additions."
    )
    lines.append("")
    if not total_core:
        lines.append("_None._")
    else:
        lines.append("| class | core members |")
        lines.append("| --- | ---: |")
        for cls in tracked:
            n = len(core_by_class.get(cls, ()))
            if n:
                lines.append(f"| `{cls}` | {n} |")
    lines.append("")

    lines.append("## Rust-only registered classes")
    lines.append("")
    lines.append(
        "Plugin classes Rust registers that have no reference counterpart "
        "reachable through the ncbind surface (TJS aliases, GDI+ internals, or "
        "plugins whose reference file uses manual TJS registration)."
    )
    lines.append("")
    rust_only = sorted(c for c in rust if c not in ref)
    if rust_only:
        lines.append("| class | Rust members | source |")
        lines.append("| --- | ---: | --- |")
        for cls in rust_only:
            src = ", ".join(f"`{s}`" for s in sorted(rust_sources.get(cls, [])))
            lines.append(f"| `{cls}` | {len(rust[cls])} | {src} |")
    else:
        lines.append("_None._")
    lines.append("")

    lines.append("## Extraction notes")
    lines.append("")
    lines.append(
        "- Core native classes (`Storages`, `System`, `Scripts`, ...) are read "
        "from `crates/compat/src/natives/*.rs` as well as the plugin tree, so "
        "plugin registrations that attach to a core class are matched."
    )
    lines.append(
        "- `NCB_ATTACH_CLASS*` members are also recorded under the C++ "
        "implementation class and matched to a Rust class with the "
        "canonicalised name (`layerExMovie` -> `LayerExMovie`). Manual "
        "`addMethod(dispatch, TJS_W(\"name\"), ...)` registrations are read the "
        "same way."
    )
    lines.append(
        "- `MotionRect` is compared against the reference `Rect` (the Rust port "
        "renames it to avoid colliding with the core `Rect` class)."
    )
    lines.append(
        "- `LayerExBase` has no registered TJS class in the reference; its "
        "surface comes from the public methods and `TJS_W(\"...\")` property "
        "names of the `layerExBase` helper struct."
    )
    lines.append("")
    lines.append("## Documented exclusions")
    lines.append("")
    lines.append(
        "The following macros are not script surface and are intentionally not "
        "parsed as members:"
    )
    lines.append("")
    for macro in EXCLUDED_REFERENCE_MACROS:
        lines.append(f"- `{macro}`")
    lines.append("")
    lines.append(
        "Reference classes in `"
        + "`, `".join(sorted(EXCLUDED_REFERENCE_CLASSES))
        + "` are skipped entirely (GDI+ rendering internals). `Debug.console` "
        "targets are reported as the Rust class `Console`. The `finalize` "
        "lifecycle hook is ignored on both sides."
    )
    lines.append("")
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def _check_runtime(args: argparse.Namespace) -> int:
    """Compare the parsed reference plugin surface against a runtime dump.

    Report only: the dump is the ground truth for the whole reference surface,
    so missing members are printed and the process exits 0.
    """
    runtime = np.load_runtime_surface(args.runtime)
    ref, impl_classes = parse_reference()
    # Reference members attached to an implementation class
    # (``layerExMovie`` -> ``LayerExMovie``): match them to the runtime class
    # with the same canonicalised name.
    for impl, impl_members in impl_classes.items():
        canon = _canon_class(impl)
        for runtime_cls in runtime:
            if _canon_class(runtime_cls) == canon:
                ref.setdefault(runtime_cls, set()).update(impl_members)
    # Drop lifecycle hooks and the class-name constructor entry.
    for cls, members in ref.items():
        members -= IGNORED_MEMBERS
        members.discard(cls)
    missing_by_class = np.runtime_missing_by_class(ref, runtime, CLASS_ALIASES)
    total_ref = sum(len(m) for m in ref.values())
    total_missing = sum(len(m) for m in missing_by_class.values())
    print(
        f"plugin_parity(runtime): {len(ref)} reference classes, "
        f"{total_ref} reference members, {total_missing} missing at runtime"
    )
    if not args.quiet:
        for cls in sorted(missing_by_class):
            miss = missing_by_class[cls]
            if not miss:
                continue
            print(f"  {cls}: {len(miss)} missing")
            for member in sorted(miss):
                print(f"    - {cls}.{member}")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--update-allow",
        action="store_true",
        help="rewrite scripts/plugin_parity_allow.txt with the current missing set",
    )
    parser.add_argument(
        "--no-doc", action="store_true", help="do not regenerate docs/plugin_parity.md"
    )
    parser.add_argument(
        "--quiet", action="store_true", help="only print failures/stale warnings"
    )
    parser.add_argument(
        "--runtime",
        metavar="PATH",
        help="compare the parsed reference plugin surface against a "
        "runtime_surface.json dump instead of the Rust source (report only; "
        "does not touch the allowlist)",
    )
    args = parser.parse_args(argv)

    if args.runtime:
        return _check_runtime(args)

    ref, impl_classes = parse_reference()
    rust, rust_sources, plugin_only, core_members, dynamic_groups = parse_rust()

    # Rust classes that are the port of a reference class under another name
    # (``Motion.Rect`` -> ``MotionRect``).  Drop the alias's own name from the
    # merged table: it is a constructor, not a member.
    for src_name, dst in RUST_CLASS_ALIASES.items():
        if src_name in rust:
            rust.setdefault(dst, set()).update(rust.pop(src_name))
            rust[dst].discard(src_name)
        if src_name in plugin_only:
            plugin_only.setdefault(dst, set()).update(plugin_only.pop(src_name))
            plugin_only[dst].discard(src_name)
        if src_name in rust_sources:
            rust_sources.setdefault(dst, []).extend(rust_sources.pop(src_name))

    # Reference members attached to an implementation class (``layerExMovie``)
    # or a manual ``addMethod`` registration; match them to the Rust class that
    # carries the canonicalised implementation name (``LayerExMovie``).
    for impl, impl_members in impl_classes.items():
        canon = _canon_class(impl)
        for rust_cls in rust:
            if _canon_class(rust_cls) == canon:
                ref.setdefault(rust_cls, set()).update(impl_members)

    # Drop lifecycle hooks from every set and the class-name constructor entry
    # (the class object is not a script-callable member).
    for members in (ref, rust, plugin_only, core_members):
        for cls, mset in members.items():
            mset -= IGNORED_MEMBERS
            mset.discard(cls)

    # Tracked classes: every reference class, plus Rust-only classes so
    # implementation drift on a not-yet-registered reference class is visible.
    tracked = sorted(set(ref) | set(rust))

    # A member a non-primary class receives from a shared registration table is
    # inherited (``EmotePlayer`` gets ``Player``'s whole table), not a plugin
    # addition of its own.
    inherited: dict[str, set[str]] = {}
    for group_classes, base, primary in dynamic_groups:
        for cls in group_classes:
            if cls != primary:
                inherited.setdefault(cls, set()).update(base)

    missing_by_class: dict[str, set[str]] = {}
    extra_by_class: dict[str, set[str]] = {}
    core_by_class: dict[str, set[str]] = {}
    for cls in tracked:
        r = ref.get(cls, set())
        u = rust.get(cls, set())
        missing_by_class[cls] = r - u
        # Only plugin-registered members with no reference counterpart count as
        # extras; core natives and inherited members do not.
        extra_by_class[cls] = (
            plugin_only.get(cls, set()) - r - inherited.get(cls, set())
        )
        core_by_class[cls] = core_members.get(cls, set()) - r

    all_missing = {
        f"{cls}.{member}"
        for cls, missing in missing_by_class.items()
        for member in missing
    }

    allow = read_allowlist(ALLOWLIST_PATH)
    if args.update_allow:
        write_allowlist(ALLOWLIST_PATH, all_missing)
        print(
            f"plugin_parity: wrote {len(all_missing)} known-missing entries to "
            f"{ALLOWLIST_PATH.relative_to(REPO_ROOT)}"
        )
        allow = set(all_missing)

    new_missing = sorted(all_missing - allow)
    stale = sorted(allow - all_missing)

    if not args.no_doc:
        DOC_PATH.parent.mkdir(parents=True, exist_ok=True)
        DOC_PATH.write_text(
            build_report(
                ref,
                rust,
                rust_sources,
                missing_by_class,
                extra_by_class,
                core_by_class,
                tracked,
            ),
            encoding="utf-8",
        )
        if not args.quiet:
            print(f"plugin_parity: wrote {DOC_PATH.relative_to(REPO_ROOT)}")

    total_ref = sum(len(ref.get(c, ())) for c in tracked)
    total_missing = sum(len(m) for m in missing_by_class.values())
    total_extra = sum(len(e) for e in extra_by_class.values())
    total_core = sum(len(c) for c in core_by_class.values())
    if not args.quiet:
        matched = sum(
            len(ref.get(c, set()) & rust.get(c, set())) for c in tracked
        )
        cov = (100.0 * matched / total_ref) if total_ref else 0.0
        print(
            f"plugin_parity: {len(tracked)} classes, "
            f"{total_ref} reference members, {total_missing} missing, "
            f"{total_extra} plugin extras ({total_core} core members excluded), "
            f"coverage {cov:.1f}%"
        )

    if stale:
        print(
            f"plugin_parity: {len(stale)} allowlisted member(s) are now "
            "implemented — shrink scripts/plugin_parity_allow.txt:"
        )
        for entry in stale:
            print(f"  - {entry}")

    if new_missing:
        print(
            f"plugin_parity: {len(new_missing)} new missing member(s) not in "
            f"{ALLOWLIST_PATH.relative_to(REPO_ROOT)}:",
            file=sys.stderr,
        )
        for entry in new_missing:
            print(f"  - {entry}", file=sys.stderr)
        print(
            "plugin_parity: run with --update-allow only after reviewing these.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
