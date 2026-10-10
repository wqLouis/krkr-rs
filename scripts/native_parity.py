#!/usr/bin/env python3
"""Native-surface parity checker for krkr-rs.

This is a *development* tool. It machine-diffs the script-visible native class
surface of the reference C++ KiriKiri/TVP engine against the classes the Rust
engine actually registers, so member gaps (``Layer.colorRect``,
``Bitmap.loadAsync``, ...) are found systematically instead of by luck.

How it works (short version)
----------------------------
* Reference surface: every ``TJS_BEGIN_NATIVE_METHOD_DECL(...)`` /
  ``TJS_BEGIN_NATIVE_PROP_DECL(...)`` (and the constructor registration) in
  ``reference/cpp/core/**/*.cpp`` is attributed to the class it is registered
  on.  Class attribution uses the enclosing ``tTJSNativeClass(TJS_W("X"))``
  constructor, the ``TJS_BEGIN_NATIVE_MEMBERS(X)`` macro argument, or the
  ``TVPCreateNativeClass_X()`` function that performs ``_OUTER``
  registrations.  Preprocessor regions that are disabled in a normal build
  (``#if 0``, ``#ifdef USE_OBSOLETE_FUNCTIONS``) are ignored.
* Rust surface: for the files listed in ``RUST_CLASS_MAP`` the builder whose
  ``name: "X"`` matches is scanned for ``name: "member"`` entries,
  ``method("member", ...)`` / ``property("member", ...)`` shorthands,
  referenced stub arrays (``noop_stubs``) and referenced helper functions
  (``system_properties()``).
* Missing = reference - rust, extra = rust - reference.  Coverage is
  ``matched / reference``.

Ratchet
-------
``scripts/native_parity_allow.txt`` lists the currently accepted missing
members (one ``Class.member`` per line).  The checker fails when a *new*
missing member appears.  When an allowlisted member becomes implemented it is
reported as stale so the list can shrink.

Usage
-----
    python3 scripts/native_parity.py                 # check + regenerate report
    python3 scripts/native_parity.py --update-allow  # refresh the allowlist
    python3 scripts/native_parity.py --no-doc        # skip docs/native_parity.md
    python3 scripts/native_parity.py --quiet         # only print failures
    python3 scripts/native_parity.py --runtime target/runtime_surface.json \
        --no-doc                                     # check the live VM surface

Do not hand-edit ``docs/native_parity.md``: it is generated.
"""

from __future__ import annotations

import argparse
import datetime
import glob
import json
import os
import re
import sys
from collections import OrderedDict
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
REFERENCE_GLOB = "reference/cpp/core/**/*.cpp"
ALLOWLIST_PATH = REPO_ROOT / "scripts" / "native_parity_allow.txt"
DOC_PATH = REPO_ROOT / "docs" / "native_parity.md"

# Classes that live in tjs2 itself (provided by the VM, not the TVP port).
TJS2_BUILTINS = {
    "Array",
    "Date",
    "Dictionary",
    "Exception",
    "Math",
    "RandomGenerator",
    "RegExp",
}

# Nested helper classes of Debug that are not game-script surface.
UNTRACKED_REFERENCE_CLASSES = {"Controller", "Console"}

# Reference classes compared to Rust.  The value is a list of
# (relative_rust_path, builder_name_or_None).  ``None`` means "union of all
# builders in that file"; a builder name restricts parsing to the matching
# registration block (needed when one file registers several classes).
RUST_CLASS_MAP: "OrderedDict[str, list[tuple[str, str | None]]]" = OrderedDict(
    [
        (
            "Layer",
            [
                ("crates/compat/src/natives/visual/layer/mod.rs", None),
                ("crates/compat/src/natives/visual/layer/pixels.rs", None),
                ("crates/compat/src/natives/visual/layer/events.rs", None),
                ("crates/compat/src/natives/visual/layer/focus.rs", None),
                ("crates/compat/src/natives/visual/layer/order.rs", None),
                ("crates/compat/src/natives/visual/layer/transitions.rs", None),
                ("crates/compat/src/natives/visual/layer/util.rs", None),
            ],
        ),
        ("Bitmap", [("crates/compat/src/natives/visual/bitmap.rs", None)]),
        ("Window", [("crates/compat/src/natives/visual/window.rs", None)]),
        ("Font", [("crates/compat/src/natives/visual/font.rs", None)]),
        ("Timer", [("crates/compat/src/natives/visual/timer.rs", None)]),
        ("System", [("crates/compat/src/natives/system.rs", None)]),
        ("Debug", [("crates/compat/src/natives/debug.rs", "Debug")]),
        ("Plugins", [("crates/compat/src/plugins_native.rs", None)]),
        ("VideoOverlay", [("crates/compat/src/natives/video.rs", None)]),
        ("AsyncTrigger", [("crates/compat/src/natives/async_trigger.rs", None)]),
        ("MenuItem", [("crates/compat/src/natives/menu_item.rs", None)]),
        ("Storages", [("crates/compat/src/natives/storages.rs", None)]),
        ("Scripts", [("crates/compat/src/natives/scripts.rs", None)]),
        # `kag` may be a single file or a directory module.
        (
            "KAGParser",
            [
                ("crates/compat/src/natives/kag.rs", None),
                ("crates/compat/src/natives/kag/mod.rs", None),
            ],
        ),
        ("WaveSoundBuffer", [("crates/compat/src/natives/sound.rs", None)]),
        # Classes tracked so the metric reflects the *whole* reference surface;
        # a not-yet-created Rust file contributes 0 (port progress).
        ("Pad", [("crates/compat/src/natives/input.rs", "Pad")]),
        ("Rect", [("crates/compat/src/natives/rect.rs", None)]),
        ("Clipboard", [("crates/compat/src/natives/clipboard.rs", None)]),
        ("ImageFunction", [("crates/compat/src/natives/image_function.rs", None)]),
        (
            "BitmapLayerTreeOwner",
            [("crates/compat/src/natives/tree_owner.rs", None)],
        ),
        (
            "CDDASoundBuffer",
            [("crates/compat/src/natives/cdda_midi.rs", "CDDASoundBuffer")],
        ),
        (
            "MIDISoundBuffer",
            [("crates/compat/src/natives/cdda_midi.rs", "MIDISoundBuffer")],
        ),
    ]
)

# Registered Rust classes with no native C++ counterpart in this tree.  These
# are reported for completeness (their members are all "extra" by definition).
RUST_ONLY_CLASS_MAP: "OrderedDict[str, list[tuple[str, str | None]]]" = OrderedDict(
    [
        (
            "SoundBuffer",
            [("crates/compat/src/natives/sound.rs", "SoundBuffer")],
        ),
        (
            "SoundChannel",
            [("crates/compat/src/natives/sound.rs", "SoundChannel")],
        ),
    ]
)

# Macros considered *defined* while walking the C++ preprocessor.  Deliberately
# empty: members gated behind feature flags such as USE_OBSOLETE_FUNCTIONS are
# not part of a normal build.
CPP_ASSUMED_DEFINED: set[str] = set()

# Lifecycle hooks that are not part of the script-visible surface games call.
IGNORED_REFERENCE_MEMBERS = {"finalize"}


# ---------------------------------------------------------------------------
# C++ reference parsing
# ---------------------------------------------------------------------------

_CPP_DIRECTIVE_RE = re.compile(r"^\s*#\s*(if|ifdef|ifndef|else|elif|endif)\b(.*)$")
_CPP_BLOCK_RE = re.compile(r"TJS_BEGIN_NATIVE_MEMBERS\s*\(\s*([A-Za-z_]\w*)\s*\)")
_CPP_CTOR_RE = re.compile(
    r"tTJSNativeClass\s*\(\s*TJS_W\s*\(\s*\"([^\"]+)\"\s*\)\s*\)"
)
_CPP_FUNC_RE = re.compile(r"TVPCreateNativeClass_(\w+)\s*\([^)]*\)\s*\{")
_CPP_MEMBER_RE = re.compile(
    r"TJS_BEGIN_NATIVE_(?:STATIC_)?(?:METHOD|PROP)_DECL\s*\(\s*([A-Za-z_]\w*)\s*\)"
)
_CPP_CTOR_END_RE = re.compile(
    r"TJS_END_NATIVE_(?:STATIC_)?CONSTRUCTOR_DECL\s*\(\s*([A-Za-z_]\w*)\s*\)"
)
_CPP_END_MEMBERS_RE = re.compile(r"TJS_END_NATIVE_MEMBERS")


def _preprocess_cpp(src: str) -> str:
    """Blank out lines that a normal build would not compile.

    Only the handful of flags that actually guard native member declarations
    matter here, but the walk handles nesting, ``#else`` and ``#elif``.
    """
    lines = src.split("\n")
    active = True
    # stack entries: [parent_active, this_branch_active, any_branch_taken]
    stack: list[list[bool]] = []
    out: list[str] = []
    for line in lines:
        m = _CPP_DIRECTIVE_RE.match(line)
        if m:
            kw, rest = m.group(1), m.group(2).strip()
            if kw in ("if", "ifdef", "ifndef"):
                parent = active
                if kw == "if":
                    cond = rest != "0"
                elif kw == "ifdef":
                    cond = rest in CPP_ASSUMED_DEFINED
                else:  # ifndef
                    cond = rest not in CPP_ASSUMED_DEFINED
                this = parent and cond
                stack.append([parent, this, this])
                active = this
            elif kw == "else" and stack:
                parent, _this, taken = stack[-1]
                newthis = parent and not taken
                stack[-1] = [parent, newthis, taken or newthis]
                active = newthis
            elif kw == "elif" and stack:
                parent, _this, taken = stack[-1]
                cond = rest != "0"
                newthis = parent and not taken and cond
                stack[-1] = [parent, newthis, taken or newthis]
                active = newthis
            elif kw == "endif":
                if stack:
                    stack.pop()
                active = stack[-1][1] if stack else True
            out.append("")
            continue
        out.append(line if active else "")
    return "\n".join(out)


def _strip_cpp_comments(src: str) -> str:
    """Replace C/C++ comments with spaces, preserving offsets/newlines."""
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


def _match_brace(src: str, open_pos: int) -> int:
    """Return the index just past the ``}`` matching ``{`` at ``open_pos``."""
    depth = 0
    i = open_pos
    n = len(src)
    while i < n:
        c = src[i]
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return n


def parse_reference() -> dict[str, set[str]]:
    """Return ``{reference_class: {member, ...}}``."""
    classes: dict[str, set[str]] = {}
    files = sorted(glob.glob(str(REPO_ROOT / REFERENCE_GLOB), recursive=True))
    for fp in files:
        raw = Path(fp).read_text(encoding="utf-8", errors="replace")
        src = _strip_cpp_comments(_preprocess_cpp(raw))

        spans: list[tuple[int, int, str]] = []

        # TJS_BEGIN_NATIVE_MEMBERS(X) ... TJS_END_NATIVE_MEMBERS
        for m in _CPP_BLOCK_RE.finditer(src):
            cls = m.group(1)
            # Prefer the enclosing tTJSNativeClass(TJS_W("X")) argument: the
            # macro argument is sometimes a copy/paste leftover (DebugImpl.cpp
            # uses TJS_BEGIN_NATIVE_MEMBERS(Debug) inside Controller/Console).
            for cm in _CPP_CTOR_RE.finditer(src[: m.start()]):
                cls = cm.group(1)
            end_m = _CPP_END_MEMBERS_RE.search(src[m.end() :])
            end = m.end() + end_m.start() if end_m else len(src)
            spans.append((m.start(), end, cls))

        # TVPCreateNativeClass_X() { ... }  (hosts the _OUTER registrations)
        for m in _CPP_FUNC_RE.finditer(src):
            open_pos = src.index("{", m.start())
            spans.append((m.start(), _match_brace(src, open_pos), m.group(1)))

        # Attribute every member declaration to the innermost containing span.
        for m in _CPP_MEMBER_RE.finditer(src):
            name = m.group(1)
            if name in IGNORED_REFERENCE_MEMBERS:
                continue
            pos = m.start()
            containing = [s for s in spans if s[0] < pos < s[1]]
            if not containing:
                continue
            cls = min(containing, key=lambda s: s[1] - s[0])[2]
            classes.setdefault(cls, set()).add(name)

        # Constructor registrations expose a member named after the class.
        for m in _CPP_CTOR_END_RE.finditer(src):
            name = m.group(1)
            pos = m.start()
            containing = [s for s in spans if s[0] < pos < s[1]]
            if not containing:
                continue
            cls = min(containing, key=lambda s: s[1] - s[0])[2]
            classes.setdefault(cls, set()).add(name)

    return classes


# ---------------------------------------------------------------------------
# Runtime surface comparison
# ---------------------------------------------------------------------------

def load_runtime_surface(path: "str | Path") -> dict[str, set[str]]:
    """Load a ``runtime_surface.json`` dump produced by the compat test.

    Accepts either the wrapped form ``{"classes": {...}}`` or a bare
    ``{class: [members]}`` mapping.  Returns ``{class: set(members)}``.
    """
    data = json.loads(Path(path).read_text(encoding="utf-8"))
    classes = data.get("classes", data) if isinstance(data, dict) else data
    if not isinstance(classes, dict):
        raise SystemExit(f"native_parity: invalid runtime surface: {path}")
    return {str(name): {str(m) for m in members} for name, members in classes.items()}


def runtime_candidates(ref_class: str, runtime: dict[str, set[str]]) -> list[str]:
    """Runtime class keys that can carry ``ref_class``.

    Exact match first, then any dotted key whose last segment is the class
    name (``Motion.Rect`` for ``Rect``, ``Math.RandomGenerator`` for
    ``RandomGenerator``).
    """
    candidates = [ref_class]
    suffix = "." + ref_class
    candidates += [name for name in runtime if name.endswith(suffix)]
    return [name for name in candidates if name in runtime]


def runtime_missing_by_class(
    ref: dict[str, set[str]],
    runtime: dict[str, set[str]],
    aliases: "dict[str, str] | None" = None,
) -> dict[str, set[str]]:
    """Reference members absent from the runtime dump, keyed by reference class.

    A reference class matches its exact runtime key and any dotted runtime key
    ending in ``.<class>``; the runtime members of all matches are unioned
    before the diff, so a class reachable both top-level and under a namespace
    is covered.
    """
    aliases = aliases or {}
    missing: dict[str, set[str]] = {}
    for cls, members in ref.items():
        target = aliases.get(cls, cls)
        available: set[str] = set()
        for key in runtime_candidates(target, runtime):
            available |= runtime[key]
        missing[cls] = {m for m in members if m not in available}
    return missing


# ---------------------------------------------------------------------------
# Rust registration parsing
# ---------------------------------------------------------------------------

# A minimal lexer so braces inside strings/comments never confuse the parser.
_RAW_LITERAL_RE = re.compile(r"(?:b|br|rb)?r(#*)\"")


def _try_rust_literal(src: str, i: int):
    """If a string/char literal starts at ``i`` return (end, content, quote).

    ``content`` is ``None`` for char literals (we only need strings).  Returns
    ``(None, None, None)`` when ``i`` is not the start of a literal.
    """
    n = len(src)
    j = i
    raw = False
    if src.startswith("br", j) or src.startswith("rb", j):
        j += 2
        raw = True
    elif j < n and src[j] == "b":
        j += 1
    if j < n and src[j] == "r":
        j += 1
        raw = True

    if raw:
        hashes = 0
        k = j
        while k < n and src[k] == "#":
            hashes += 1
            k += 1
        if k < n and src[k] == '"':
            close = '"' + "#" * hashes
            end = src.find(close, k + 1)
            if end == -1:
                end = n
                content = src[k + 1 : n]
            else:
                content = src[k + 1 : end]
                end += len(close)
            return end, content, k
        return None, None, None

    if j < n and src[j] == '"':
        k = j + 1
        while k < n:
            if src[k] == "\\":
                k += 2
                continue
            if src[k] == '"':
                break
            k += 1
        end = min(k + 1, n)
        return end, src[j + 1 : k], j

    if j < n and src[j] == "'":
        # Distinguish a char literal from a lifetime ('a).
        k = j + 1
        if k < n and src[k] == "\\":
            k += 2
        else:
            k += 1
        if k < n and src[k] == "'":
            return k + 1, None, j
        return None, None, None

    return None, None, None


def _lex_rust(src: str) -> tuple[str, dict[int, tuple[int, str, int]]]:
    """Blank comments and literal bodies; record string literals.

    Returns ``(code, strings)`` where ``code`` has the same length as ``src``
    but with comments and string/char bodies replaced by spaces, and
    ``strings`` maps the literal start offset to
    ``(end_offset, content, opening_quote_offset)``.
    """
    code = list(src)
    strings: dict[int, tuple[int, str, int]] = {}
    i = 0
    n = len(src)
    while i < n:
        c = src[i]
        if c == "/" and i + 1 < n and src[i + 1] == "/":
            j = src.find("\n", i)
            if j == -1:
                j = n
            for k in range(i, j):
                code[k] = " "
            i = j
            continue
        if c == "/" and i + 1 < n and src[i + 1] == "*":
            depth = 1
            j = i + 2
            while j < n and depth > 0:
                if src[j] == "/" and j + 1 < n and src[j + 1] == "*":
                    depth += 1
                    j += 2
                elif src[j] == "*" and j + 1 < n and src[j + 1] == "/":
                    depth -= 1
                    j += 2
                else:
                    j += 1
            for k in range(i, j):
                if src[k] != "\n":
                    code[k] = " "
            i = j
            continue
        end, content, quote = _try_rust_literal(src, i)
        if end is not None:
            if content is not None:
                strings[i] = (end, content, quote)
            for k in range(i, end):
                if src[k] != "\n":
                    code[k] = " "
            i = end
            continue
        i += 1
    return "".join(code), strings


_NAME_CONTEXT_RE = re.compile(r"\bname\s*:\s*$")
_CALL_CONTEXT_RE = re.compile(r"\b(?:method|property|getter)\s*\(\s*$")


def _extract_members(
    code: str,
    strings: dict[int, tuple[int, str, int]],
    start: int,
    end: int,
    *,
    bare: bool = False,
    exclude: "set[int] | None" = None,
) -> set[str]:
    """Collect member names from string literals inside ``[start, end)``.

    With ``bare=False`` only literals whose preceding code reads ``name:`` or
    ``method(``/``property(``/``getter(`` are considered.  With ``bare=True``
    every literal in the range is taken as a member (used for stub arrays).
    Literal offsets in ``exclude`` (the builder's own ``name:`` field) are
    skipped so a class builder's name is not miscounted as a member.
    """
    members: set[str] = set()
    for start_off, (end_off, content, quote) in strings.items():
        if not (start <= start_off < end):
            continue
        if exclude and start_off in exclude:
            continue
        if bare:
            members.add(content)
            continue
        prefix = code[max(0, quote - 64) : quote]
        if _NAME_CONTEXT_RE.search(prefix) or _CALL_CONTEXT_RE.search(prefix):
            members.add(content)
    return members


def _match_bracket(src: str, open_pos: int) -> int:
    """Return the index just past the ``]`` matching ``[`` at ``open_pos``."""
    depth = 0
    i = open_pos
    n = len(src)
    while i < n:
        c = src[i]
        if c == "[":
            depth += 1
        elif c == "]":
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return n


def _resolve_expr(
    code: str,
    strings: dict[int, tuple[int, str, int]],
    expr_start: int,
    expr_end: int,
) -> set[str]:
    """Members from a ``vec![...]`` of defs or a plain ``[...]`` of names."""
    context = _extract_members(code, strings, expr_start, expr_end)
    if context:
        return context
    return _extract_members(code, strings, expr_start, expr_end, bare=True)


def _resolve_variable(
    code: str, strings: dict[int, tuple[int, str, int]], var: str
) -> set[str]:
    """Resolve ``let [mut] var ... = [...]`` / ``= vec![...]``."""
    m = re.search(
        r"\blet\s+(?:mut\s+)?" + re.escape(var) + r"\b[^=;]*=", code
    )
    if not m:
        return set()
    rhs = m.end()
    bracket = code.find("[", rhs)
    if bracket == -1:
        return set()
    # Ignore a '[' that belongs to a generic/attribute before the value.
    end = _match_bracket(code, bracket)
    return _resolve_expr(code, strings, bracket, end)


def _resolve_function(
    code: str, strings: dict[int, tuple[int, str, int]], fn: str
) -> set[str]:
    """Resolve a helper returning a member vector (e.g. system_properties())."""
    m = re.search(r"\bfn\s+" + re.escape(fn) + r"\s*\(", code)
    if not m:
        return set()
    open_pos = code.find("{", m.end())
    if open_pos == -1:
        return set()
    end = _match_brace(code, open_pos)
    return _extract_members(code, strings, open_pos, end)


def _const_strings(code: str, strings: dict[int, tuple[int, str, int]]) -> dict[str, str]:
    """Resolve simple ``const NAME: &str = "value";`` declarations."""
    out: dict[str, str] = {}
    for m in re.finditer(r"\bconst\s+([A-Za-z_]\w*)\s*:\s*&str\s*=", code):
        name = m.group(1)
        candidates = [off for off in strings if off >= m.end()]
        if not candidates:
            continue
        out[name] = strings[min(candidates)][1]
    return out


def _builder_spans(
    code: str,
    strings: dict[int, tuple[int, str, int]],
    raw: str,
) -> list[tuple[int, int, str, "int | None"]]:
    """Return ``(start, end, class_name, name_literal_off)`` per builder.

    The builder's own ``name:`` literal offset is returned so it can be
    excluded from member extraction: it is the class object (the constructor),
    not a script-callable member.  ``main`` drops the class-name entry from
    both the reference and Rust sets for the same reason.
    """
    spans: list[tuple[int, int, str, "int | None"]] = []
    consts = _const_strings(code, strings)
    for m in re.finditer(
        r"Native(?:Instance|Class)Builder\s*\{"
        r"|Native(?:Static|Instance)Members\s*\{",
        code,
    ):
        open_pos = code.index("{", m.start())
        end = _match_brace(code, open_pos)
        # Read the builder's own `name:` field from the *original* text (the
        # literal contents are blanked in `code`); fall back to a const
        # identifier (gdiplus uses APPEARANCE_CLASS).
        name_text = raw[open_pos:end]
        first = re.search(r"\b(?:class_)?name\s*:\s*", name_text)
        cls = None
        name_lit = None
        if first:
            rest = name_text[first.end() :]
            m_str = re.match(r'"([^"]+)"', rest)
            m_id = re.match(r"([A-Za-z_]\w*)", rest)
            if m_str:
                cls = m_str.group(1)
                name_lit = open_pos + first.end() + m_str.start(0)
            elif m_id:
                cls = consts.get(m_id.group(1))
        if cls:
            spans.append((open_pos, end, cls, name_lit))
    return spans


def _def_function_members(
    code: str,
    strings: dict[int, tuple[int, str, int]],
    raw: str,
) -> set[str]:
    """Members declared by ``fn ...() -> Vec<Native*MethodDef/PropertyDef>``.

    Handles both ``NativeInstanceMethodDef { name: "x", .. }`` and the
    shorthand ``d("x", f)`` / ``m("x", ..)`` helper calls, so a class split
    into submodules (``visual/layer/pixels.rs`` etc.) is still attributed.
    """
    members: set[str] = set()
    for m in re.finditer(
        r"fn\s+[A-Za-z_]\w*\s*\([^)]*\)\s*->\s*[^{;]*"
        r"Native(?:Instance|Static)?(?:Method|Property)Def",
        code,
    ):
        open_pos = code.find("{", m.end())
        if open_pos == -1:
            continue
        end = _match_brace(code, open_pos)
        members |= _extract_members(code, strings, open_pos, end)
        for lit in re.finditer(r'\b[A-Za-z_]\w*\s*\(\s*"([^"]+)"', raw[open_pos:end]):
            members.add(lit.group(1))
    return members


def parse_rust_file(path: Path) -> dict[str, set[str]]:
    """Return ``{builder_class_name: {member, ...}}`` for one Rust file.

    A file that does not exist yet (a native family not started) contributes
    no members rather than failing: the coverage report then shows it as
    unimplemented, which is the point.
    """
    if not path.is_file():
        return {}
    raw = path.read_text(encoding="utf-8", errors="replace")
    code, strings = _lex_rust(raw)

    # Stub arrays appended to a list, e.g. `methods.extend(noop_stubs.into_iter()`
    # in layer.rs.  Keyed by the receiver so the members only attach to the
    # builder that actually uses that list.
    extends: dict[str, set[str]] = {}
    for m in re.finditer(
        r"([A-Za-z_]\w*)\s*\.extend\(\s*([A-Za-z_]\w*)\s*"
        r"(?:\.into_iter\s*)?\(",
        code,
    ):
        extends.setdefault(m.group(1), set()).add(m.group(2))

    result: dict[str, set[str]] = {}
    for start, end, cls, name_lit in _builder_spans(code, strings, raw):
        members = _extract_members(
            code,
            strings,
            start,
            end,
            exclude={name_lit} if name_lit is not None else set(),
        )
        builder = code[start:end]

        # External method/property lists: `methods: system_properties()`,
        # `methods,` (shorthand), `properties: helper()`.
        refs: list[tuple[str, str]] = []  # (kind, identifier)
        for kind in ("methods", "properties"):
            for m in re.finditer(
                r"\b" + kind + r"\s*:\s*([A-Za-z_]\w*)\s*\(", builder
            ):
                refs.append(("fn", m.group(1)))
            for m in re.finditer(
                r"\b" + kind + r"\s*:\s*([A-Za-z_]\w*)\s*[,}]", builder
            ):
                refs.append(("var", m.group(1)))
            for m in re.finditer(r"\b" + kind + r"\s*[,}]", builder):
                refs.append(("var", kind))
        for receiver, idents in extends.items():
            if re.search(r"\b" + re.escape(receiver) + r"\b", builder):
                refs.extend(("var", ident) for ident in idents)
                # `.extend(helper())` where ``helper`` is a ``fn`` returning a
                # def vector (e.g. ``methods.extend(method_defs())``).
                refs.extend(("fn", ident) for ident in idents)

        for kind, ident in refs:
            if kind == "fn":
                members |= _resolve_function(code, strings, ident)
            else:
                members |= _resolve_variable(code, strings, ident)

        result.setdefault(cls, set()).update(members)

    # Helper functions returning a def vector (`fn method_defs() -> Vec<..>`),
    # collected under ``__defs__`` so a class mapped with ``builder=None``
    # unions them (used when a class is split across files).
    result.setdefault("__defs__", set()).update(
        _def_function_members(code, strings, raw)
    )

    # Builder factory whose class name is a parameter, e.g.
    # `fn instance_builder(name, ..) -> NativeInstanceBuilder { .. }` called as
    # `instance_builder("CDDASoundBuffer", ..)`: attribute the builder's
    # members to each literal class name at the call site.
    for m in re.finditer(
        r"fn\s+([A-Za-z_]\w*)\s*\([^)]*\)\s*->\s*Native(?:Instance|Class)Builder",
        code,
    ):
        fname = m.group(1)
        open_pos = code.find("{", m.end())
        if open_pos == -1:
            continue
        end = _match_brace(code, open_pos)
        bmembers = _extract_members(code, strings, open_pos, end)
        for call in re.finditer(
            r"\b" + re.escape(fname) + r"\s*\(\s*\"([^\"]+)\"", raw
        ):
            result.setdefault(call.group(1), set()).update(bmembers)

    return result


def parse_rust() -> tuple[dict[str, set[str]], dict[str, list[str]]]:
    """Parse all mapped Rust files.

    Returns ``(class_members, class_sources)``.
    """
    cache: dict[str, dict[str, set[str]]] = {}
    class_members: dict[str, set[str]] = {}
    class_sources: dict[str, list[str]] = {}

    def get_file(rel: str) -> dict[str, set[str]]:
        if rel not in cache:
            cache[rel] = parse_rust_file(REPO_ROOT / rel)
        return cache[rel]

    for cls, specs in {**RUST_CLASS_MAP, **RUST_ONLY_CLASS_MAP}.items():
        members: set[str] = set()
        sources: list[str] = []
        existing = [rel for rel, _ in specs if (REPO_ROOT / rel).is_file()]
        if not existing:
            raise SystemExit(
                "native_parity: none of the mapped path(s) for "
                f"{cls} exist: {[rel for rel, _ in specs]} — the file was "
                "renamed/moved; update RUST_CLASS_MAP."
            )
        for rel, builder in specs:
            parsed = get_file(rel)
            sources.append(rel + (f"#{builder}" if builder else ""))
            if builder is None:
                for _, mset in parsed.items():
                    members |= mset
            else:
                members |= parsed.get(builder, set())
        class_members[cls] = members
        class_sources[cls] = sources
    return class_members, class_sources


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
        "# Native parity allowlist: reference members that are known-missing.\n"
        "# One entry per line: Class.member\n"
        "# Regenerate after intentional additions with:\n"
        "#   python3 scripts/native_parity.py --update-allow\n"
        "# The checker fails when a missing member is not listed here.\n"
        "\n"
    )
    body = "\n".join(sorted(entries))
    path.write_text(header + body + ("\n" if body else ""), encoding="utf-8")


def build_report(
    ref: dict[str, set[str]],
    rust: dict[str, set[str]],
    rust_only: dict[str, set[str]],
    rust_sources: dict[str, list[str]],
    missing_by_class: dict[str, set[str]],
    extra_by_class: dict[str, set[str]],
) -> str:
    tracked = [c for c in RUST_CLASS_MAP if c in ref]
    total_ref = sum(len(ref.get(c, ())) for c in tracked)
    total_matched = sum(len(ref.get(c, set()) & rust.get(c, set())) for c in tracked)
    total_missing = sum(len(missing_by_class.get(c, ())) for c in tracked)
    total_extra = sum(len(extra_by_class.get(c, ())) for c in tracked)
    coverage = (100.0 * total_matched / total_ref) if total_ref else 0.0

    lines: list[str] = []
    lines.append("# Native surface parity")
    lines.append("")
    lines.append(
        "> **Generated** by `scripts/native_parity.py` on "
        f"{datetime.date.today().isoformat()} — do not edit by hand."
    )
    lines.append("")
    lines.append(
        "Machine-diff of the reference C++ native class surface "
        "(`reference/cpp/core/**/*.cpp`) against the native classes the Rust "
        "engine registers. A *missing* member exists in the reference but is "
        "not registered by Rust; an *extra* member is registered by Rust but "
        "has no reference counterpart (engine-internal helpers such as `id` "
        "and `nativeId` are expected here)."
    )
    lines.append("")
    lines.append("## Usage")
    lines.append("")
    lines.append("```sh")
    lines.append("python3 scripts/native_parity.py                 # check + regenerate this report")
    lines.append("python3 scripts/native_parity.py --no-doc        # check only")
    lines.append("python3 scripts/native_parity.py --update-allow  # accept current gaps after review")
    lines.append("```")
    lines.append("")
    lines.append(
        "The checker exits non-zero when a missing member is **not** listed in "
        "`scripts/native_parity_allow.txt` (a regression). When an allowlisted "
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
    lines.append(f"| extra (Rust only) | {total_extra} |")
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
        cov = (100.0 * matched / len(r)) if r else 0.0
        lines.append(
            f"| `{cls}` | {len(r)} | {len(u)} | {matched} | "
            f"{len(missing_by_class.get(cls, ()))} | "
            f"{len(extra_by_class.get(cls, ()))} | {cov:.1f}% |"
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
        "Rust registrations with no reference counterpart; usually intentional "
        "engine internals."
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

    # Reference classes with no Rust implementation at all.
    untracked = sorted(
        c
        for c in ref
        if c not in RUST_CLASS_MAP
        and c not in TJS2_BUILTINS
        and c not in UNTRACKED_REFERENCE_CLASSES
    )
    lines.append("## Reference classes not registered in Rust")
    lines.append("")
    if untracked:
        lines.append("| class | reference members |")
        lines.append("| --- | ---: |")
        for cls in untracked:
            lines.append(f"| `{cls}` | {len(ref[cls])} |")
    else:
        lines.append("_None._")
    lines.append("")

    # Rust-only registered classes.
    lines.append("## Rust-only registered classes")
    lines.append("")
    lines.append(
        "These builders have no native C++ counterpart in `reference/cpp/core` "
        "(TJS-level or plugin classes)."
    )
    lines.append("")
    if rust_only:
        lines.append("| class | Rust members | source |")
        lines.append("| --- | ---: | --- |")
        for cls in rust_only:
            src = ", ".join(f"`{s}`" for s in rust_sources.get(cls, []))
            lines.append(f"| `{cls}` | {len(rust_only[cls])} | {src} |")
    else:
        lines.append("_None._")
    lines.append("")
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def _check_runtime(args: argparse.Namespace) -> int:
    """Compare the parsed reference surface against a runtime surface dump.

    This is a report, not a gate: missing members are printed and the process
    exits 0, because the dump is the ground truth for the whole reference
    surface (including classes the source mode does not track).
    """
    runtime = load_runtime_surface(args.runtime)
    ref = parse_reference()
    # The class-name constructor is the class object, not a callable member.
    for cls, members in ref.items():
        members.discard(cls)
        members -= IGNORED_REFERENCE_MEMBERS
    missing_by_class = runtime_missing_by_class(ref, runtime)
    total_ref = sum(len(m) for m in ref.values())
    total_missing = sum(len(m) for m in missing_by_class.values())
    print(
        f"native_parity(runtime): {len(ref)} reference classes, "
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
        help="rewrite scripts/native_parity_allow.txt with the current missing set",
    )
    parser.add_argument(
        "--no-doc", action="store_true", help="do not regenerate docs/native_parity.md"
    )
    parser.add_argument(
        "--quiet", action="store_true", help="only print failures/stale warnings"
    )
    parser.add_argument(
        "--runtime",
        metavar="PATH",
        help="compare the parsed reference surface against a runtime_surface.json "
        "dump instead of the Rust source (report only; does not touch the allowlist)",
    )
    args = parser.parse_args(argv)

    if args.runtime:
        return _check_runtime(args)

    ref = parse_reference()
    rust, rust_sources = parse_rust()

    # The constructor (reference `TJS_END_NATIVE_CONSTRUCTOR_DECL(X)`, Rust
    # `NativeInstanceBuilder { name: "X" }` / class builder name) is the class
    # object, not a script-callable member, and some classes register it while
    # others do not.  Drop the class-name entry from both sides so the diff is
    # about real members only.
    for cls in set(RUST_CLASS_MAP) | set(RUST_ONLY_CLASS_MAP):
        if cls in ref:
            ref[cls].discard(cls)
        if cls in rust:
            rust[cls].discard(cls)

    missing_by_class: dict[str, set[str]] = {}
    extra_by_class: dict[str, set[str]] = {}
    for cls in RUST_CLASS_MAP:
        r = ref.get(cls, set())
        u = rust.get(cls, set())
        missing_by_class[cls] = r - u
        extra_by_class[cls] = u - r

    all_missing = {
        f"{cls}.{member}"
        for cls, missing in missing_by_class.items()
        for member in missing
    }

    # `--update-allow` accepts the current gap set wholesale, so the check
    # below is bypassed and the allowlist is rewritten deterministically.
    allow = read_allowlist(ALLOWLIST_PATH)
    if args.update_allow:
        write_allowlist(ALLOWLIST_PATH, all_missing)
        print(
            f"native_parity: wrote {len(all_missing)} known-missing entries to "
            f"{ALLOWLIST_PATH.relative_to(REPO_ROOT)}"
        )
        allow = set(all_missing)

    new_missing = sorted(all_missing - allow)
    # Entries that are no longer missing: implemented (or the class was
    # removed).  These are only a warning so the allowlist can shrink.
    stale = sorted(allow - all_missing)

    if not args.no_doc:
        DOC_PATH.parent.mkdir(parents=True, exist_ok=True)
        DOC_PATH.write_text(
            build_report(
                ref,
                rust,
                {c: rust.get(c, set()) for c in RUST_ONLY_CLASS_MAP},
                rust_sources,
                missing_by_class,
                extra_by_class,
            ),
            encoding="utf-8",
        )
        if not args.quiet:
            print(f"native_parity: wrote {DOC_PATH.relative_to(REPO_ROOT)}")

    total_ref = sum(len(ref.get(c, ())) for c in RUST_CLASS_MAP)
    total_missing = sum(len(m) for m in missing_by_class.values())
    total_extra = sum(len(e) for e in extra_by_class.values())
    if not args.quiet:
        matched = sum(
            len(ref.get(c, set()) & rust.get(c, set())) for c in RUST_CLASS_MAP
        )
        cov = (100.0 * matched / total_ref) if total_ref else 0.0
        print(
            f"native_parity: {len(RUST_CLASS_MAP)} classes, "
            f"{total_ref} reference members, {total_missing} missing, "
            f"{total_extra} extra, coverage {cov:.1f}%"
        )

    if stale:
        print(
            f"native_parity: {len(stale)} allowlisted member(s) are now "
            "implemented — shrink scripts/native_parity_allow.txt:"
        )
        for entry in stale:
            print(f"  - {entry}")

    if new_missing:
        print(
            f"native_parity: {len(new_missing)} new missing member(s) not in "
            f"{ALLOWLIST_PATH.relative_to(REPO_ROOT)}:",
            file=sys.stderr,
        )
        for entry in new_missing:
            print(f"  - {entry}", file=sys.stderr)
        print(
            "native_parity: run with --update-allow only after reviewing these.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
