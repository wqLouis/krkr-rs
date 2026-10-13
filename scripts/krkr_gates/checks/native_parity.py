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
  registrations.  Class-object members installed after construction with
  ``PropSet(TJS_MEMBERENSURE, ...)`` on a variable bound by
  ``TVPCreateNativeClass_*`` / ``registerObject`` are read too (``Window.menu``,
  ``Window.BasicDrawDevice``).  A ``PropSet`` whose name is not a literal is
  reported as unrecoverable rather than dropped.  A preprocessor region is
  blanked only when a normal build would not compile it: ``#if 0``, or a flag
  the reference source never defines.  A flag a shipped header does define
  (``#ifdef USE_OBSOLETE_FUNCTIONS``) is part of the surface.
* Rust surface: for the files listed in ``RUST_CLASS_MAP`` the builder whose
  ``name: "X"`` matches is scanned for ``name: "member"`` entries,
  ``method("member", ...)`` / ``property("member", ...)`` shorthands,
  referenced stub arrays (``noop_stubs``) and referenced helper functions
  (``system_properties()``).  Every ``Native*PropertyDef`` literal also
  records its ``get``/``set`` presence.
* Missing = reference - rust, extra = rust - reference.  Coverage is
  ``matched / reference``.
* Accessor parity: a reference property declared with
  ``TJS_BEGIN_NATIVE_PROP_DECL`` must expose the same accessors in Rust.  A
  reference-readable property whose Rust def has no ``get``, or a
  reference-writable one whose def has no ``set``, fails the check (the
  ``Layer.imageWidth`` shape); a Rust accessor the reference denies is
  reported as *extra* and fails the same way.

Ratchet
-------
``scripts/allowlists/native_parity_allow.txt`` lists the currently accepted missing
members (one ``Class.member`` per line);
``scripts/allowlists/native_parity_props_allow.txt`` lists the currently accepted
accessor mismatches (one ``Class.property`` per line).  The checker fails when
a *new* one appears, and reports an allowlisted entry that no longer applies
as stale so the list can shrink.

Usage
-----
    uv run --project scripts krkr-gate native-parity                 # check + regenerate report
    uv run --project scripts krkr-gate native-parity --update-allow  # refresh the member allowlist
    uv run --project scripts krkr-gate native-parity --update-props-allow  # refresh the accessor allowlist
    uv run --project scripts krkr-gate native-parity --no-doc        # skip docs/native_parity.md
    uv run --project scripts krkr-gate native-parity --quiet         # only print failures
    uv run --project scripts krkr-gate native-parity --self-test     # extractor fixture self-test
    uv run --project scripts krkr-gate native-parity --runtime target/runtime_surface.json \
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
from dataclasses import dataclass
from pathlib import Path

from ..common import ALLOWLISTS_DIR, REPO_ROOT

REFERENCE_GLOB = "reference/cpp/core/**/*.cpp"
ALLOWLIST_PATH = ALLOWLISTS_DIR / "native_parity_allow.txt"
PROPS_ALLOWLIST_PATH = ALLOWLISTS_DIR / "native_parity_props_allow.txt"
DOC_PATH = REPO_ROOT / "docs" / "native_parity.md"
FIXTURES_DIR = REPO_ROOT / "scripts" / "fixtures" / "parity"

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
        # Nested class objects the reference declares in their own
        # ``TJS_BEGIN_NATIVE_MEMBERS`` block and attaches to a host class
        # (``Window.BasicDrawDevice``, ``WaveSoundBuffer.PhaseVocoder``).
        # Without an entry here their members were never compared.
        (
            "BasicDrawDevice",
            [("crates/compat/src/natives/visual/draw_device.rs", "BasicDrawDevice")],
        ),
        (
            "PassThroughDrawDevice",
            [
                (
                    "crates/compat/src/natives/visual/draw_device.rs",
                    "PassThroughDrawDevice",
                )
            ],
        ),
        (
            "PhaseVocoder",
            [("crates/compat/src/natives/sound/phase_vocoder.rs", "__TvpPhaseVocoder")],
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

def _reference_defined_macros() -> set[str]:
    """Flags the reference source both gates code on and defines itself.

    A member behind ``#ifdef FLAG`` is part of the shipped surface when the
    reference defines ``FLAG`` — ``USE_OBSOLETE_FUNCTIONS`` is defined in
    ``reference/cpp/core/visual/WindowIntf.h:23`` — while a flag it never
    defines is a build option and its members are not part of the surface.
    Collect the identifiers used in preprocessor conditions and keep the ones
    some ``#define`` names, so an unrelated internal macro is ignored.
    """
    define_re = re.compile(r"^\s*#\s*define\s+([A-Za-z_]\w*)")
    cond_re = re.compile(r"^\s*#\s*(?:if|ifdef|ifndef|elif)\b(.*)$")
    ident_re = re.compile(r"[A-Za-z_]\w*")
    defined_any: set[str] = set()
    used_in_condition: set[str] = set()
    patterns = ("reference/cpp/**/*.h", "reference/cpp/**/*.hpp", "reference/cpp/**/*.cpp")
    paths: list[str] = []
    for pattern in patterns:
        paths.extend(glob.glob(str(REPO_ROOT / pattern), recursive=True))
    for path in sorted(set(paths)):
        for line in Path(path).read_text(encoding="utf-8", errors="replace").splitlines():
            match = define_re.match(line)
            if match:
                defined_any.add(match.group(1))
            condition = cond_re.match(line)
            if condition:
                used_in_condition.update(ident_re.findall(condition.group(1)))
    return defined_any & used_in_condition


# Macros considered *defined* while walking the C++ preprocessor: the flags the
# reference source both gates code on and defines itself.  A flag it never
# defines is a build option, and members behind it are not part of the surface.
CPP_ASSUMED_DEFINED: set[str] = _reference_defined_macros()

# Lifecycle hooks that are not part of the script-visible surface games call.
IGNORED_REFERENCE_MEMBERS = {"finalize"}


@dataclass(frozen=True)
class MemberInfo:
    """How one script-visible reference member is installed.

    ``kind`` is ``"method"`` / ``"property"`` when the declaring macro says
    so, ``"constructor"`` for the class-name entry, and ``"unknown"`` when the
    installation mechanism does not reveal it (``PropSet`` installs a value or
    a property object, and the two are not distinguishable from the call site).
    ``static`` is true for a class-object member; ``mechanism`` names the source
    construct so the report can show where a member came from.

    ``get``/``set`` record the accessor halves of a ``TJS_BEGIN_NATIVE_PROP_DECL``
    property: whether its block contains ``TJS_BEGIN_NATIVE_PROP_GETTER`` /
    ``TJS_BEGIN_NATIVE_PROP_SETTER``.  They stay ``None`` for methods and for
    mechanisms (``PropSet``, constructor) whose accessors the source does not
    express.
    """

    kind: str = "unknown"
    static: bool = False
    mechanism: str = "unknown"
    get: "bool | None" = None
    set: "bool | None" = None


# Registration mechanisms, in descending order of how much they reveal.  A
# member reached by more than one mechanism keeps the first (strongest) entry.
_MECHANISM_RANK = {
    "native-macro": 0,
    "constructor": 1,
    "ncm": 2,
    "propset": 3,
    "propset-bynum": 4,
    "unknown": 5,
}


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
    r"TJS_BEGIN_NATIVE_(STATIC_)?(METHOD|PROP)_DECL\s*\(\s*([A-Za-z_]\w*)\s*\)"
)
_CPP_PROP_BEGIN_RE = re.compile(
    r"TJS_BEGIN_NATIVE_(STATIC_)?PROP_DECL\s*\(\s*([A-Za-z_]\w*)\s*\)"
)
# A property block closes with one of four DECL spellings; the getter/setter
# blocks inside it close with ``TJS_END_NATIVE_PROP_GETTER`` / ``_SETTER`` and
# must not end the property.
_CPP_PROP_END_RE = re.compile(r"TJS_END_NATIVE_(?:STATIC_)?PROP_DECL(?:_OUTER)?")
_CPP_PROP_GETTER_RE = re.compile(r"TJS_BEGIN_NATIVE_PROP_GETTER")
_CPP_PROP_SETTER_RE = re.compile(r"TJS_BEGIN_NATIVE_PROP_SETTER")
_CPP_CTOR_END_RE = re.compile(
    r"TJS_END_NATIVE_(?:STATIC_)?CONSTRUCTOR_DECL\s*\(\s*([A-Za-z_]\w*)\s*\)"
)
_CPP_END_MEMBERS_RE = re.compile(r"TJS_END_NATIVE_MEMBERS")
_CPP_DEFINE_RE = re.compile(r"^\s*#\s*define\s+([A-Za-z_]\w*)")
_CPP_UNDEF_RE = re.compile(r"^\s*#\s*undef\s+([A-Za-z_]\w*)")


def _preprocess_cpp(src: str) -> str:
    """Blank out lines that a normal build would not compile.

    Only the handful of flags that actually guard native member declarations
    matter here, but the walk handles nesting, ``#else`` and ``#elif``.
    """
    lines = src.split("\n")
    active = True
    # Flags the source itself defines (in addition to the reference-wide set),
    # so a fixture or a same-file ``#define`` is honoured.
    defined = set(CPP_ASSUMED_DEFINED)
    # stack entries: [parent_active, this_branch_active, any_branch_taken]
    stack: list[list[bool]] = []
    out: list[str] = []
    # A multi-line ``#define`` body is blanked so a macro parameter (e.g.
    # ``TVP_REGISTER_PTDD_ENUM(name)``) is not read as a member literally
    # named ``name``.
    in_define_body = False
    for line in lines:
        if in_define_body:
            out.append("")
            if not line.rstrip().endswith("\\"):
                in_define_body = False
            continue
        m = _CPP_DIRECTIVE_RE.match(line)
        if m:
            kw, rest = m.group(1), m.group(2).strip()
            if kw in ("if", "ifdef", "ifndef"):
                parent = active
                if kw == "if":
                    cond = rest != "0"
                elif kw == "ifdef":
                    cond = rest in defined
                else:  # ifndef
                    cond = rest not in defined
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
        if active:
            dm = _CPP_DEFINE_RE.match(line)
            if dm:
                defined.add(dm.group(1))
                out.append("")
                if line.rstrip().endswith("\\"):
                    in_define_body = True
                continue
            um = _CPP_UNDEF_RE.match(line)
            if um:
                defined.discard(um.group(1))
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


def _split_cpp_args(src: str, open_paren: int) -> list[str]:
    """Split a C++ call argument list whose ``(`` is at ``open_paren``.

    Returns the argument strings; only parens/brackets/braces are tracked,
    which is enough for the registration calls in this tree.
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
                return args
            depth -= 1
            cur.append(c)
        elif c == "," and depth == 0:
            args.append("".join(cur).strip())
            cur = []
        else:
            cur.append(c)
        i += 1
    args.append("".join(cur).strip())
    return args


def _cpp_string_literal(expr: str) -> "str | None":
    """The TJS name in ``TJS_W("x")`` / ``"x"``; ``None`` for an expression."""
    m = re.search(r'TJS_W\s*\(\s*"([^"]+)"\s*\)', expr)
    if m:
        return m.group(1)
    m = re.fullmatch(r'\s*"([^"]+)"\s*', expr)
    return m.group(1) if m else None


def _core_class_bindings(src: str) -> dict[str, str]:
    """Class object variable -> TJS class name for one core file.

    Two shapes bind a class object to a local: an explicit
    ``var = TVPCreateNativeClass_X()`` and the ``registerObject`` call
    ``registerObject(TJS_W("X"), (var = TVPCreateNativeClass_X()))``.  The
    ``registerObject`` spelling wins because it names the TJS class directly.
    """
    bindings: dict[str, str] = {}
    for m in re.finditer(
        r"([A-Za-z_]\w*)\s*=\s*TVPCreateNativeClass_(\w+)\s*\(", src
    ):
        bindings[m.group(1)] = m.group(2)
    for m in re.finditer(
        r'registerObject\s*\(\s*TJS_W\s*\(\s*"([^"]+)"\s*\)\s*,\s*'
        r"\(?\s*([A-Za-z_]\w*)\s*=\s*TVPCreateNativeClass_(\w+)\s*\(",
        src,
    ):
        bindings[m.group(2)] = m.group(1)
    return bindings


def collect_propset_members(
    src: str, bindings: dict[str, str]
) -> tuple[dict[str, dict[str, MemberInfo]], list[str]]:
    """Class-level members installed with ``PropSet`` / ``PropSetByNum``.

    ``bindings`` maps a class-object variable to its TJS class name.  A
    ``var->PropSet(TJS_MEMBERENSURE, TJS_W("name"), ...)`` call on a bound
    variable installs a class member; ``TJS_STATICMEMBER`` makes the static
    kind explicit, but the member is class-level either way.  A name that is
    not a string literal (``something.c_str()``, a loop variable, ...) cannot
    be recovered and is returned in the second list so it is counted as an
    unsupported site rather than silently dropped.
    """
    members: dict[str, dict[str, MemberInfo]] = {}
    unrecoverable: list[str] = []
    for m in re.finditer(
        r"([A-Za-z_]\w*)\s*(?:->|\.)\s*(PropSet|PropSetByNum)\s*\(", src
    ):
        var, method = m.group(1), m.group(2)
        cls = bindings.get(var)
        if cls is None:
            continue
        open_paren = src.index("(", m.start())
        args = _split_cpp_args(src, open_paren)
        if len(args) < 2:
            continue
        name = _cpp_string_literal(args[1])
        if name is None:
            unrecoverable.append(f"{cls}.{var}.{method}: {args[1]}")
            continue
        info = MemberInfo(
            kind="unknown",
            static=True,
            mechanism="propset" if method == "PropSet" else "propset-bynum",
        )
        members.setdefault(cls, {})[name] = info
    return members, unrecoverable


def _set_member(
    surface: dict[str, dict[str, MemberInfo]],
    cls: str,
    name: str,
    info: MemberInfo,
) -> None:
    """Add a member, keeping the strongest (earliest) mechanism seen."""
    existing = surface.setdefault(cls, {})
    old = existing.get(name)
    if old is None or _MECHANISM_RANK.get(info.mechanism, 9) < _MECHANISM_RANK.get(
        old.mechanism, 9
    ):
        existing[name] = info


def collect_prop_accessors(src: str) -> dict[int, tuple[bool, bool]]:
    """Accessor presence for each ``TJS_BEGIN_NATIVE_PROP_DECL`` in ``src``.

    Keyed by the offset of the ``TJS_BEGIN_NATIVE_PROP_DECL`` so the member
    loop can attach the pair to the property it just recorded.  A read-only
    property ends its setter with ``TJS_DENY_NATIVE_PROP_SETTER`` (which is not
    a ``TJS_BEGIN_NATIVE_PROP_SETTER``), so its ``set`` half is false.
    """
    out: dict[int, tuple[bool, bool]] = {}
    for m in _CPP_PROP_BEGIN_RE.finditer(src):
        end = _CPP_PROP_END_RE.search(src, m.end())
        body = src[m.end() : end.start()] if end else src[m.end() :]
        out[m.start()] = (
            bool(_CPP_PROP_GETTER_RE.search(body)),
            bool(_CPP_PROP_SETTER_RE.search(body)),
        )
    return out


def parse_cpp_source_surface(
    src: str,
) -> tuple[dict[str, dict[str, MemberInfo]], list[str]]:
    """Reference members declared in one already-masked C++ source.

    Covers the native-member macro blocks and constructors, plus class-level
    ``PropSet``/``PropSetByNum`` registrations on a class object bound from
    ``TVPCreateNativeClass_*`` / ``registerObject``.  The second return value
    lists registration sites whose member name is built at runtime.
    """
    surface: dict[str, dict[str, MemberInfo]] = {}
    unrecoverable: list[str] = []

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

    def enclosing(pos: int) -> "str | None":
        containing = [s for s in spans if s[0] < pos < s[1]]
        if not containing:
            return None
        return min(containing, key=lambda s: s[1] - s[0])[2]

    # Accessor halves per property declaration, keyed by declaration offset.
    prop_accessors = collect_prop_accessors(src)

    # Attribute every member declaration to the innermost containing span.
    for m in _CPP_MEMBER_RE.finditer(src):
        name = m.group(3)
        if name in IGNORED_REFERENCE_MEMBERS:
            continue
        cls = enclosing(m.start())
        if cls is None:
            continue
        kind = "method" if m.group(2) == "METHOD" else "property"
        get: "bool | None" = None
        set_: "bool | None" = None
        if kind == "property":
            pair = prop_accessors.get(m.start())
            if pair is not None:
                get, set_ = pair
        _set_member(
            surface,
            cls,
            name,
            MemberInfo(kind, m.group(1) is not None, "native-macro", get, set_),
        )

    # Constructor registrations expose a member named after the class.
    for m in _CPP_CTOR_END_RE.finditer(src):
        name = m.group(1)
        cls = enclosing(m.start())
        if cls is None:
            continue
        _set_member(surface, cls, name, MemberInfo("constructor", False, "constructor"))

    # Class-object registrations performed after the class is built.
    ps_members, ps_unrecoverable = collect_propset_members(
        src, _core_class_bindings(src)
    )
    for cls, entries in ps_members.items():
        for name, info in entries.items():
            _set_member(surface, cls, name, info)
    unrecoverable.extend(ps_unrecoverable)
    return surface, unrecoverable


def parse_reference_surface() -> tuple[dict[str, dict[str, MemberInfo]], list[str]]:
    """Return ``({class: {member: MemberInfo}}, unrecoverable_sites)``."""
    surface: dict[str, dict[str, MemberInfo]] = {}
    unrecoverable: list[str] = []
    files = sorted(glob.glob(str(REPO_ROOT / REFERENCE_GLOB), recursive=True))
    for fp in files:
        raw = Path(fp).read_text(encoding="utf-8", errors="replace")
        src = _strip_cpp_comments(_preprocess_cpp(raw))
        file_surface, file_unrecoverable = parse_cpp_source_surface(src)
        for cls, entries in file_surface.items():
            for name, info in entries.items():
                _set_member(surface, cls, name, info)
        unrecoverable.extend(
            f"{Path(fp).relative_to(REPO_ROOT)}: {site}" for site in file_unrecoverable
        )
    return surface, unrecoverable


def parse_reference() -> dict[str, set[str]]:
    """Return ``{reference_class: {member, ...}}``."""
    surface, _unrecoverable = parse_reference_surface()
    return {cls: set(entries) for cls, entries in surface.items()}


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


_RUST_PROP_DEF_RE = re.compile(r"\bNative(?:Instance|ObjThis)?PropertyDef\s*\{")
_RUST_PROP_NAME_RE = re.compile(r"\bname\s*:")
_RUST_PROP_GET_RE = re.compile(r"\bget\s*:\s*(Some\b|None\b)")
_RUST_PROP_SET_RE = re.compile(r"\bset\s*:\s*(Some\b|None\b)")


def _extract_prop_accessors(
    code: str,
    strings: dict[int, tuple[int, str, int]],
    start: int,
    end: int,
    skip: "list[tuple[int, int]] | None" = None,
    unrecoverable: "list[str] | None" = None,
) -> "dict[str, tuple[bool | None, bool | None]]":
    """The ``get``/``set`` presence of every property def literal in the range.

    A literal ``Native*PropertyDef { name: "x", get: Some(..), set: None }``
    yields ``{"x": (True, False)}``.  A def whose ``name`` is not a string
    literal, or whose ``get``/``set`` field is not a static ``Some``/``None``,
    is recorded as unrecoverable (when a list is supplied) rather than guessed.
    ``skip`` ranges are ``macro_rules!`` bodies, whose defs are templates with
    ``$``-names, not registrations.
    """
    out: "dict[str, tuple[bool | None, bool | None]]" = {}
    skip = skip or []
    for m in _RUST_PROP_DEF_RE.finditer(code, start, end):
        if any(s <= m.start() < e for s, e in skip):
            continue
        open_pos = code.index("{", m.end() - 1)
        close = _match_brace(code, open_pos)
        if close > end:
            close = end
        body = code[open_pos:close]
        nm = _RUST_PROP_NAME_RE.search(body)
        name = None
        if nm:
            after = open_pos + nm.end()
            candidates = [off for off in strings if after <= off < close]
            if candidates:
                name = strings[min(candidates)][1]
        if name is None:
            if unrecoverable is not None:
                unrecoverable.append(
                    f"property def at offset {m.start()} has no literal name"
                )
            continue
        gm = _RUST_PROP_GET_RE.search(body)
        sm = _RUST_PROP_SET_RE.search(body)
        get = (gm.group(1) == "Some") if gm else None
        set_ = (sm.group(1) == "Some") if sm else None
        if (get is None or set_ is None) and unrecoverable is not None:
            unrecoverable.append(
                f"property {name!r} has no static get/set "
                f"(get={gm.group(1) if gm else '?'}, "
                f"set={sm.group(1) if sm else '?'})"
            )
        out[name] = (get, set_)
    return out


def _macro_rule_ranges(code: str) -> list[tuple[int, int]]:
    """``macro_rules! NAME { ... }`` spans.

    Their ``Native*PropertyDef`` are templates with ``$``-names, not
    registrations, so accessor extraction skips them.
    """
    ranges: list[tuple[int, int]] = []
    for m in re.finditer(r"macro_rules!\s*[A-Za-z_]\w*\s*\{", code):
        open_pos = code.index("{", m.start())
        ranges.append((m.start(), _match_brace(code, open_pos)))
    return ranges


def _variable_range(code: str, var: str) -> "tuple[int, int] | None":
    """The ``[ ... ]`` / ``vec![ ... ]`` range of ``let [mut] var = ...``."""
    m = re.search(r"\blet\s+(?:mut\s+)?" + re.escape(var) + r"\b[^=;]*=", code)
    if not m:
        return None
    rhs = m.end()
    bracket = code.find("[", rhs)
    if bracket == -1:
        return None
    # Ignore a '[' that belongs to a generic/attribute before the value.
    return bracket, _match_bracket(code, bracket)


def _function_range(code: str, fn: str) -> "tuple[int, int] | None":
    """The ``{ ... }`` body range of ``fn name( ... )``."""
    m = re.search(r"\bfn\s+" + re.escape(fn) + r"\s*\(", code)
    if not m:
        return None
    open_pos = code.find("{", m.end())
    if open_pos == -1:
        return None
    return open_pos, _match_brace(code, open_pos)


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
    rng = _variable_range(code, var)
    if rng is None:
        return set()
    return _resolve_expr(code, strings, rng[0], rng[1])


def _resolve_variable_accessors(
    code: str,
    strings: dict[int, tuple[int, str, int]],
    var: str,
    skip: "list[tuple[int, int]]",
    unrecoverable: "list[str]",
) -> "dict[str, tuple[bool | None, bool | None]]":
    """Accessors of the def literals in a ``let [mut] var = [...]`` value."""
    rng = _variable_range(code, var)
    if rng is None:
        return {}
    return _extract_prop_accessors(code, strings, rng[0], rng[1], skip, unrecoverable)


def _resolve_function(
    code: str, strings: dict[int, tuple[int, str, int]], fn: str
) -> set[str]:
    """Resolve a helper returning a member vector (e.g. system_properties())."""
    rng = _function_range(code, fn)
    if rng is None:
        return set()
    return _extract_members(code, strings, rng[0], rng[1])


def _resolve_function_accessors(
    code: str,
    strings: dict[int, tuple[int, str, int]],
    fn: str,
    skip: "list[tuple[int, int]]",
    unrecoverable: "list[str]",
) -> "dict[str, tuple[bool | None, bool | None]]":
    """Accessors of the def literals in a helper ``fn`` returning a def vector."""
    rng = _function_range(code, fn)
    if rng is None:
        return {}
    return _extract_prop_accessors(code, strings, rng[0], rng[1], skip, unrecoverable)


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


def _def_function_ranges(code: str) -> list[tuple[int, int]]:
    """Body ranges of ``fn ...() -> Vec<Native*MethodDef/PropertyDef>``."""
    ranges: list[tuple[int, int]] = []
    for m in re.finditer(
        r"fn\s+[A-Za-z_]\w*\s*\([^)]*\)\s*->\s*[^{;]*"
        r"Native(?:Instance|Static)?(?:Method|Property)Def",
        code,
    ):
        open_pos = code.find("{", m.end())
        if open_pos == -1:
            continue
        ranges.append((open_pos, _match_brace(code, open_pos)))
    return ranges


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
    for open_pos, end in _def_function_ranges(code):
        members |= _extract_members(code, strings, open_pos, end)
        for lit in re.finditer(r'\b[A-Za-z_]\w*\s*\(\s*"([^"]+)"', raw[open_pos:end]):
            members.add(lit.group(1))
    return members


def _def_function_accessors(
    code: str,
    strings: dict[int, tuple[int, str, int]],
    skip: "list[tuple[int, int]]",
    unrecoverable: "list[str]",
) -> "dict[str, tuple[bool | None, bool | None]]":
    """Accessors declared by ``fn ...() -> Vec<Native*PropertyDef>`` helpers."""
    accessors: "dict[str, tuple[bool | None, bool | None]]" = {}
    for open_pos, end in _def_function_ranges(code):
        accessors.update(
            _extract_prop_accessors(code, strings, open_pos, end, skip, unrecoverable)
        )
    return accessors


def parse_rust_file(
    path: Path,
) -> tuple[
    dict[str, set[str]],
    dict[str, dict[str, tuple[bool | None, bool | None]]],
    list[str],
]:
    """Parse one Rust file into members, property accessors and unrecoverable sites.

    Returns ``(members, accessors, unrecoverable)`` where ``members`` is
    ``{builder_class_name: {member, ...}}`` and ``accessors`` is
    ``{builder_class_name: {property: (get, set)}}`` for the
    ``Native*PropertyDef`` literals.  A file that does not exist yet (a native
    family not started) contributes no members rather than failing: the
    coverage report then shows it as unimplemented, which is the point.
    """
    if not path.is_file():
        return {}, {}, []
    raw = path.read_text(encoding="utf-8", errors="replace")
    code, strings = _lex_rust(raw)
    macro_spans = _macro_rule_ranges(code)
    unrecoverable: list[str] = []

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
    accessors: dict[str, dict[str, tuple[bool | None, bool | None]]] = {}
    for start, end, cls, name_lit in _builder_spans(code, strings, raw):
        members = _extract_members(
            code,
            strings,
            start,
            end,
            exclude={name_lit} if name_lit is not None else set(),
        )
        props = _extract_prop_accessors(
            code, strings, start, end, macro_spans, unrecoverable
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
                props.update(
                    _resolve_function_accessors(
                        code, strings, ident, macro_spans, unrecoverable
                    )
                )
            else:
                members |= _resolve_variable(code, strings, ident)
                props.update(
                    _resolve_variable_accessors(
                        code, strings, ident, macro_spans, unrecoverable
                    )
                )

        result.setdefault(cls, set()).update(members)
        accessors.setdefault(cls, {}).update(props)

    # Helper functions returning a def vector (`fn method_defs() -> Vec<..>`),
    # collected under ``__defs__`` so a class mapped with ``builder=None``
    # unions them (used when a class is split across files).
    result.setdefault("__defs__", set()).update(
        _def_function_members(code, strings, raw)
    )
    accessors.setdefault("__defs__", {}).update(
        _def_function_accessors(code, strings, macro_spans, unrecoverable)
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
        bprops = _extract_prop_accessors(
            code, strings, open_pos, end, macro_spans, unrecoverable
        )
        for call in re.finditer(
            r"\b" + re.escape(fname) + r"\s*\(\s*\"([^\"]+)\"", raw
        ):
            result.setdefault(call.group(1), set()).update(bmembers)
            accessors.setdefault(call.group(1), {}).update(bprops)

    return result, accessors, unrecoverable


_RUNTIME_ATTACH_RE = re.compile(
    r"attach_class\s*\(\s*\w+\s*,\s*\"([^\"]+)\"\s*,\s*\"([^\"]+)\"\s*\)"
)
_SCRIPT_MEMBER_RE = re.compile(r"([A-Za-z_]\w*)\.([A-Za-z_]\w*)\s*=")


def parse_runtime_class_attachments() -> dict[str, set[str]]:
    """Class members the port installs after the class object is built.

    Two shapes the builder reader cannot see are recognised:
    ``attach_class(ctx, "Target", "Member")`` (the draw devices attached to
    ``Window``) and a class-member assignment inside an ``exec_script`` string
    (``WaveSoundBuffer.PhaseVocoder = ...``).  Only targets that are tracked
    classes are kept, so ordinary script assignments (``t.fontSize = ...``)
    are ignored.
    """
    tracked = set(RUST_CLASS_MAP) | set(RUST_ONLY_CLASS_MAP)
    out: dict[str, set[str]] = {}
    root = REPO_ROOT / "crates" / "compat" / "src" / "natives"
    if not root.is_dir():
        return out
    for path in sorted(root.rglob("*.rs")):
        raw = path.read_text(encoding="utf-8", errors="replace")
        _code, strings = _lex_rust(raw)
        code_no_comment = _strip_cpp_comments(raw)
        for m in _RUNTIME_ATTACH_RE.finditer(code_no_comment):
            target, member = m.group(1), m.group(2)
            if target in tracked:
                out.setdefault(target, set()).add(member)
        for _off, (_end, content, _quote) in strings.items():
            for m in _SCRIPT_MEMBER_RE.finditer(content):
                target, member = m.group(1), m.group(2)
                if target in tracked:
                    out.setdefault(target, set()).add(member)
    return out


def parse_rust() -> tuple[
    dict[str, set[str]],
    dict[str, dict[str, tuple[bool | None, bool | None]]],
    dict[str, list[str]],
    list[str],
]:
    """Parse all mapped Rust files.

    Returns ``(class_members, class_accessors, class_sources, unrecoverable)``.
    """
    cache: dict[
        str,
        tuple[
            dict[str, set[str]],
            dict[str, dict[str, tuple[bool | None, bool | None]]],
            list[str],
        ],
    ] = {}
    class_members: dict[str, set[str]] = {}
    class_accessors: dict[str, dict[str, tuple[bool | None, bool | None]]] = {}
    class_sources: dict[str, list[str]] = {}
    rust_unrecoverable: list[str] = []

    def get_file(
        rel: str,
    ) -> tuple[
        dict[str, set[str]],
        dict[str, dict[str, tuple[bool | None, bool | None]]],
        list[str],
    ]:
        if rel not in cache:
            cache[rel] = parse_rust_file(REPO_ROOT / rel)
        return cache[rel]

    for cls, specs in {**RUST_CLASS_MAP, **RUST_ONLY_CLASS_MAP}.items():
        members: set[str] = set()
        accessors: dict[str, tuple[bool | None, bool | None]] = {}
        sources: list[str] = []
        existing = [rel for rel, _ in specs if (REPO_ROOT / rel).is_file()]
        if not existing:
            raise SystemExit(
                "native_parity: none of the mapped path(s) for "
                f"{cls} exist: {[rel for rel, _ in specs]} — the file was "
                "renamed/moved; update RUST_CLASS_MAP."
            )
        for rel, builder in specs:
            parsed, parsed_accessors, parsed_unrecoverable = get_file(rel)
            rust_unrecoverable.extend(parsed_unrecoverable)
            sources.append(rel + (f"#{builder}" if builder else ""))
            if builder is None:
                for _, mset in parsed.items():
                    members |= mset
                for _, props in parsed_accessors.items():
                    accessors.update(props)
            else:
                members |= parsed.get(builder, set())
                accessors.update(parsed_accessors.get(builder, {}))
        class_members[cls] = members
        class_accessors[cls] = accessors
        class_sources[cls] = sources

    for cls, members in parse_runtime_class_attachments().items():
        if cls in class_members:
            class_members[cls] |= members
            note = "runtime class attachment"
            class_sources.setdefault(cls, [])
            if note not in class_sources[cls]:
                class_sources[cls].append(note)
    return class_members, class_accessors, class_sources, rust_unrecoverable


def _yesno(value: "bool | None") -> str:
    return "yes" if value else "no"


def accessor_mismatches(
    ref_surface: "dict[str, dict[str, MemberInfo]]",
    ref_members: "dict[str, set[str]]",
    rust_members: "dict[str, set[str]]",
    rust_accessors: "dict[str, dict[str, tuple[bool | None, bool | None]]]",
    classes: "list[str] | None" = None,
) -> tuple["OrderedDict[str, str]", "OrderedDict[str, str]"]:
    """Compare property accessors, returning ``(missing, extra)``.

    ``missing`` is the failing set: a reference-readable property whose Rust
    def has no ``get``, or a reference-writable property whose Rust def has no
    ``set`` (the ``Layer.imageWidth`` shape).  ``extra`` is the reverse: Rust
    exposes an accessor the reference denies.  Both are keyed
    ``Class.property`` with a ``reference get=.. set=..; rust get=.. set=..``
    description.

    A property the reference installs through a mechanism that does not
    express accessors (``PropSet``), or a Rust def whose accessors are not
    statically readable, is skipped: the member diff and the unrecoverable
    report cover those.
    """
    missing: "OrderedDict[str, str]" = OrderedDict()
    extra: "OrderedDict[str, str]" = OrderedDict()
    for cls in (classes if classes is not None else list(RUST_CLASS_MAP)):
        names = rust_members.get(cls, set())
        for name, info in ref_surface.get(cls, {}).items():
            if info.kind != "property" or info.get is None or info.set is None:
                continue
            if name not in names:
                continue
            rust = rust_accessors.get(cls, {}).get(name)
            if rust is None or rust[0] is None or rust[1] is None:
                continue
            ref_get, ref_set = bool(info.get), bool(info.set)
            rust_get, rust_set = rust
            if ref_get == rust_get and ref_set == rust_set:
                continue
            detail = (
                f"reference get={_yesno(ref_get)} set={_yesno(ref_set)}; "
                f"rust get={_yesno(rust_get)} set={_yesno(rust_set)}"
            )
            key = f"{cls}.{name}"
            if (ref_get and not rust_get) or (ref_set and not rust_set):
                missing[key] = detail
            else:
                extra[key] = detail
    return missing, extra


# ---------------------------------------------------------------------------
# Allowlist + report
# ---------------------------------------------------------------------------


def read_allowlist(path: Path) -> set[str]:
    return set(read_allowlist_reasons(path))


def read_allowlist_reasons(path: Path) -> "OrderedDict[str, str]":
    """Allowlist entries as ``{key: reason}``.

    An entry is ``Class.member`` optionally followed by a reason after an
    em dash, a ``#`` comment marker or `` -- ``.  A reason starting with
    ``debt:`` marks a deliberately-baselined known gap (mirrors
    ``scripts/allowlists/layering_allow.txt``); a bare entry has an empty reason.
    """
    entries: "OrderedDict[str, str]" = OrderedDict()
    if not path.exists():
        return entries
    for line in path.read_text(encoding="utf-8").splitlines():
        text = line.strip()
        if not text or text.startswith("#"):
            continue
        reason = ""
        for sep in ("\u2014", " -- ", " # "):
            if sep in text:
                key, _, reason = text.partition(sep)
                entries[key.strip()] = reason.strip()
                break
        else:
            entries[text] = ""
    return entries


MEMBER_ALLOW_HEADER = (
    "# Native parity allowlist: reference members that are known-missing.\n"
    "# One entry per line: Class.member [— reason]\n"
    "# A reason beginning with 'debt:' records a known gap that is accepted\n"
    "# but still visible; see docs/audits/propset-parity.md.\n"
    "# Regenerate after intentional additions with:\n"
    "#   uv run --project scripts krkr-gate native-parity --update-allow\n"
    "# The checker fails when a missing member is not listed here.\n"
    "\n"
)

PROPS_ALLOW_HEADER = (
    "# Native parity allowlist: accepted property getter/setter mismatches.\n"
    "# One entry per line: Class.property [— reason]\n"
    "# An entry records a reference property whose Rust accessors differ.\n"
    "# A reason beginning with 'debt:' records a known divergence that is\n"
    "# accepted but still visible.\n"
    "# Regenerate after intentional additions with:\n"
    "#   uv run --project scripts krkr-gate native-parity --update-props-allow\n"
    "# The checker fails when a mismatch is not listed here.\n"
    "\n"
)


def write_allowlist(
    path: Path,
    entries: set[str],
    reasons: "dict[str, str] | None" = None,
    header: str = MEMBER_ALLOW_HEADER,
) -> None:
    reasons = reasons or {}
    lines = []
    for key in sorted(entries):
        reason = (reasons.get(key) or "").strip()
        lines.append(f"{key} \u2014 {reason}" if reason else key)
    body = "\n".join(lines)
    path.write_text(header + body + ("\n" if body else ""), encoding="utf-8")


def build_report(
    ref: dict[str, set[str]],
    rust: dict[str, set[str]],
    rust_only: dict[str, set[str]],
    rust_sources: dict[str, list[str]],
    missing_by_class: dict[str, set[str]],
    extra_by_class: dict[str, set[str]],
    ref_surface: "dict[str, dict[str, MemberInfo]] | None" = None,
    unrecoverable: "list[str] | None" = None,
    props_missing: "dict[str, str] | None" = None,
    props_extra: "dict[str, str] | None" = None,
) -> str:
    ref_surface = ref_surface or {}
    unrecoverable = unrecoverable or []
    props_missing = props_missing or {}
    props_extra = props_extra or {}
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
        "> **Generated** by `krkr-gate native-parity` on "
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
    lines.append("uv run --project scripts krkr-gate native-parity                 # check + regenerate this report")
    lines.append("uv run --project scripts krkr-gate native-parity --no-doc        # check only")
    lines.append("uv run --project scripts krkr-gate native-parity --update-allow  # accept current gaps after review")
    lines.append(
        "uv run --project scripts krkr-gate native-parity --update-props-allow  "
        "# accept accessor mismatches"
    )
    lines.append("uv run --project scripts krkr-gate native-parity --self-test     # extractor fixture self-test")
    lines.append("```")
    lines.append("")
    lines.append(
        "The checker exits non-zero when a missing member is **not** listed in "
        "`scripts/allowlists/native_parity_allow.txt` (a regression). When an allowlisted "
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

    # Registration mechanisms (how each reference member becomes visible).
    mechanism_counts: "OrderedDict[str, int]" = OrderedDict()
    for entries in ref_surface.values():
        for info in entries.values():
            mechanism_counts[info.mechanism] = (
                mechanism_counts.get(info.mechanism, 0) + 1
            )
    if mechanism_counts:
        lines.append("## Reference registration mechanisms")
        lines.append("")
        lines.append(
            "How each reference member becomes script-visible. `native-macro` is "
            "`TJS_BEGIN_NATIVE_*_DECL`, `constructor` the class-name entry, and "
            "`propset` a class-object member installed after the class is built "
            "with `PropSet(TJS_MEMBERENSURE, ...)`. A member reached by more than "
            "one mechanism is counted once, under the strongest."
        )
        lines.append("")
        lines.append("| mechanism | members |")
        lines.append("| --- | ---: |")
        for mech, count in sorted(mechanism_counts.items(), key=lambda kv: (-kv[1], kv[0])):
            lines.append(f"| `{mech}` | {count} |")
        lines.append("")

        propset = [
            (cls, name, info)
            for cls, entries in ref_surface.items()
            for name, info in entries.items()
            if info.mechanism in ("propset", "propset-bynum")
        ]
        lines.append("## Class-level members installed by `PropSet`")
        lines.append("")
        lines.append(
            "Members and class objects attached to an already-built class object. "
            "`kind` is `unknown` when `PropSet` does not reveal method versus "
            "property."
        )
        lines.append("")
        if propset:
            lines.append("| class | member | kind | static | mechanism |")
            lines.append("| --- | --- | --- | --- | --- |")
            for cls, name, info in sorted(propset):
                lines.append(
                    f"| `{cls}` | `{name}` | {info.kind} | "
                    f"{str(info.static).lower()} | `{info.mechanism}` |"
                )
        else:
            lines.append("_None._")
        lines.append("")

    lines.append("## Registration sites with a runtime-built name")
    lines.append("")
    if unrecoverable:
        lines.append(
            f"{len(unrecoverable)} site(s) install a member whose name is not a "
            "string literal, so the source reader cannot recover it. These are "
            "not counted as members: the coverage figures exclude them, which is a "
            "remaining blind spot, not a claim they do not exist."
        )
        lines.append("")
        for site in unrecoverable:
            lines.append(f"- `{site}`")
    else:
        lines.append("_None._")
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

    lines.append("## Extraction notes and remaining blind spots")
    lines.append("")
    lines.append(
        "- Reference members come from `TJS_BEGIN_NATIVE_(STATIC_)METHOD/PROP_DECL` "
        "blocks, `TJS_END_NATIVE_CONSTRUCTOR_DECL`, and class-object members "
        "installed with `PropSet`/`PropSetByNum` on a variable bound by "
        "`TVPCreateNativeClass_*`/`registerObject`."
    )
    lines.append(
        "- The Rust side is read from the builders in `RUST_CLASS_MAP`, plus "
        "`attach_class(ctx, \"Target\", \"Member\")` and a class member assigned "
        "inside an `exec_script` string, so a class attached after construction "
        "is not a false missing."
    )
    lines.append(
        "- A `PropSet` name that is not a string literal is not counted (see "
        "the runtime-built-name section): a name built at runtime cannot be "
        "recovered from source, and a coverage figure that silently dropped it "
        "would read as evidence it does not exist."
    )
    lines.append(
        "- A `PropSet` on an *instance* object (`this`, a dictionary, a method "
        "parameter) is not a class member and is deliberately not counted; "
        "only class-object receivers are members of the class surface."
    )
    lines.append(
        "- `global->PropSet(...)` registers a global (a class object or a free "
        "function such as `getDirList`), not a class member; the plugin reader "
        "covers that surface, this report does not."
    )
    lines.append(
        "- A member installed by a plugin at load time rather than in the core "
        "is outside this report; `krkr-gate plugin-parity` covers the plugin "
        "surface."
    )
    lines.append("")

    lines.append("## Property getter/setter parity")
    lines.append("")
    lines.append(
        "A reference property declared with `TJS_BEGIN_NATIVE_PROP_DECL` must "
        "expose the same accessors in Rust: a reference-readable property "
        "needs a Rust `get`, and a reference-writable one a Rust `set`. The "
        "reverse (a Rust accessor the reference denies) is listed as *extra* "
        "and fails identically. `scripts/allowlists/native_parity_props_allow.txt` "
        "records accepted exceptions."
    )
    lines.append("")
    lines.append(f"- missing accessors: {len(props_missing)}")
    lines.append(f"- extra accessors: {len(props_extra)}")
    lines.append("")
    if props_missing or props_extra:
        lines.append("| property | direction | reference | rust |")
        lines.append("| --- | --- | --- | --- |")
        for key, detail in props_missing.items():
            ref_part, _, rust_part = detail.partition("; ")
            lines.append(
                f"| `{key}` | missing | {ref_part.removeprefix('reference ')} "
                f"| {rust_part.removeprefix('rust ')} |"
            )
        for key, detail in props_extra.items():
            ref_part, _, rust_part = detail.partition("; ")
            lines.append(
                f"| `{key}` | extra | {ref_part.removeprefix('reference ')} "
                f"| {rust_part.removeprefix('rust ')} |"
            )
    else:
        lines.append("_None._")
    lines.append("")
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def _accessor_self_test() -> list[str]:
    """Fixture checks for the getter/setter rule (a match, a miss, an extra).

    ``native_props.cpp`` declares ``readWrite`` (get+set) and ``readOnly``
    (get only).  ``native_props_good.rs`` matches both,
    ``native_props_bad.rs`` drops ``readWrite``'s setter, and
    ``native_props_extra.rs`` adds a setter the reference denies for
    ``readOnly``.  The rule must return nothing for the good fixture and
    exactly the expected miss/extra for the other two.
    """
    ref_path = FIXTURES_DIR / "native_props.cpp"
    good_path = FIXTURES_DIR / "native_props_good.rs"
    bad_path = FIXTURES_DIR / "native_props_bad.rs"
    extra_path = FIXTURES_DIR / "native_props_extra.rs"
    expected_path = FIXTURES_DIR / "native_props.expected.json"
    problems: list[str] = []
    for path in (ref_path, good_path, bad_path, extra_path, expected_path):
        if not path.is_file():
            problems.append(f"accessor fixture missing: {path}")
    if problems:
        return problems
    expected = json.loads(expected_path.read_text(encoding="utf-8"))
    raw = ref_path.read_text(encoding="utf-8")
    src = _strip_cpp_comments(_preprocess_cpp(raw))
    ref_surface, _unrecoverable = parse_cpp_source_surface(src)
    for key, want in expected["reference"].items():
        cls, name = key.split(".", 1)
        info = ref_surface.get(cls, {}).get(name)
        got = None if info is None else [bool(info.get), bool(info.set)]
        if got != list(want):
            problems.append(f"{key}: reference accessors {got} != {want}")
    ref_members = {cls: set(entries) for cls, entries in ref_surface.items()}
    for variant, path in (
        ("good", good_path),
        ("bad", bad_path),
        ("extra", extra_path),
    ):
        members, accessors, _unrecoverable = parse_rust_file(path)
        missing, extra = accessor_mismatches(
            ref_surface, ref_members, members, accessors, classes=["PropWidget"]
        )
        want_missing = expected[f"{variant}_missing"]
        want_extra = expected[f"{variant}_extra"]
        if dict(missing) != want_missing:
            problems.append(f"{variant}: missing {dict(missing)} != {want_missing}")
        if dict(extra) != want_extra:
            problems.append(f"{variant}: extra {dict(extra)} != {want_extra}")
    return problems


def self_test(verbose: bool = True) -> int:
    """Assert the reference extractor still sees each registration mechanism.

    ``scripts/fixtures/parity/native_core.cpp`` exercises every core mechanism
    the extractor claims, plus a runtime-built name that must be reported as
    unrecoverable.  A regex that silently stops matching fails here rather than
    quietly shrinking the covered surface.
    """
    fixture = FIXTURES_DIR / "native_core.cpp"
    expected_path = FIXTURES_DIR / "native_core.expected.json"
    for path in (fixture, expected_path):
        if not path.is_file():
            print(f"native_parity: self-test fixture missing: {path}")
            return 1
    expected = json.loads(expected_path.read_text(encoding="utf-8"))
    raw = fixture.read_text(encoding="utf-8")
    src = _strip_cpp_comments(_preprocess_cpp(raw))
    surface, unrecoverable = parse_cpp_source_surface(src)
    got = {cls: sorted(entries) for cls, entries in surface.items()}
    want = expected["members"]

    problems: list[str] = []
    if "USE_OBSOLETE_FUNCTIONS" not in CPP_ASSUMED_DEFINED:
        problems.append(
            "the reference defines USE_OBSOLETE_FUNCTIONS "
            "(reference/cpp/core/visual/WindowIntf.h:23); the preprocessor "
            "walk must treat it as active"
        )
    for cls in sorted(set(got) | set(want)):
        got_members = set(got.get(cls, []))
        want_members = set(want.get(cls, []))
        if got_members - want_members:
            problems.append(f"{cls}: unexpected member(s) {sorted(got_members - want_members)}")
        if want_members - got_members:
            problems.append(f"{cls}: missing member(s) {sorted(want_members - got_members)}")

    want_unrecoverable = expected.get("unrecoverable", [])
    if len(unrecoverable) != len(want_unrecoverable):
        problems.append(
            "unrecoverable sites: expected "
            f"{len(want_unrecoverable)}, got {len(unrecoverable)}: {unrecoverable}"
        )
    for needle in want_unrecoverable:
        if not any(needle in site for site in unrecoverable):
            problems.append(f"unrecoverable sites: no site matching {needle!r}")
    if any("runtime" in m for members in got.values() for m in members):
        problems.append("runtime-built name leaked into the member set")

    problems.extend(_accessor_self_test())

    if problems:
        print("native_parity: extractor self-test FAILED:")
        for problem in problems:
            print(f"  {problem}")
        print("  Fix the extractor; do not silence the self-test.")
        return 1
    if verbose:
        print(
            "native_parity: extractor self-test ok "
            f"({sum(len(v) for v in got.values())} members, "
            f"{len(unrecoverable)} unrecoverable site(s))"
        )
    return 0


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
        help="rewrite scripts/allowlists/native_parity_allow.txt with the current missing set",
    )
    parser.add_argument(
        "--update-props-allow",
        action="store_true",
        help=(
            "rewrite scripts/allowlists/native_parity_props_allow.txt with the current "
            "getter/setter mismatches"
        ),
    )
    parser.add_argument(
        "--no-doc", action="store_true", help="do not regenerate docs/native_parity.md"
    )
    parser.add_argument(
        "--quiet", action="store_true", help="only print failures/stale warnings"
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="run the extractor self-test against scripts/fixtures/parity and exit",
    )
    parser.add_argument(
        "--runtime",
        metavar="PATH",
        help="compare the parsed reference surface against a runtime_surface.json "
        "dump instead of the Rust source (report only; does not touch the allowlist)",
    )
    args = parser.parse_args(argv)

    # A regex that silently stops matching is the failure mode; run the
    # fixture self-test on every invocation (the guard scripts do the same).
    selftest = self_test(verbose=not args.quiet)
    if selftest != 0:
        return selftest
    if args.self_test:
        return 0

    if args.runtime:
        return _check_runtime(args)

    ref_surface, unrecoverable = parse_reference_surface()
    ref = {cls: set(entries) for cls, entries in ref_surface.items()}
    rust, rust_accessors, rust_sources, rust_unrecoverable = parse_rust()
    unrecoverable.extend(rust_unrecoverable)

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
    # Existing reasons (including `debt:` markers) are preserved.
    allow = read_allowlist(ALLOWLIST_PATH)
    if args.update_allow:
        write_allowlist(
            ALLOWLIST_PATH, all_missing, read_allowlist_reasons(ALLOWLIST_PATH)
        )
        print(
            f"native_parity: wrote {len(all_missing)} known-missing entries to "
            f"{ALLOWLIST_PATH.relative_to(REPO_ROOT)}"
        )
        allow = set(all_missing)

    new_missing = sorted(all_missing - allow)
    # Entries that are no longer missing: implemented (or the class was
    # removed).  These are only a warning so the allowlist can shrink.
    stale = sorted(allow - all_missing)

    # Getter/setter parity: a reference-readable property must have a Rust
    # `get`, and a reference-writable one a Rust `set` (the `imageWidth`
    # shape).  `props_extra` is the reverse direction: a Rust accessor the
    # reference denies, which lets a script write a property the reference
    # raises on.  Both directions are gated by the same allowlist.
    props_missing, props_extra = accessor_mismatches(
        ref_surface, ref, rust, rust_accessors
    )
    props_allow = read_allowlist(PROPS_ALLOWLIST_PATH)
    if args.update_props_allow:
        write_allowlist(
            PROPS_ALLOWLIST_PATH,
            set(props_missing) | set(props_extra),
            read_allowlist_reasons(PROPS_ALLOWLIST_PATH),
            PROPS_ALLOW_HEADER,
        )
        print(
            f"native_parity: wrote {len(set(props_missing) | set(props_extra))} "
            f"getter/setter entries to "
            f"{PROPS_ALLOWLIST_PATH.relative_to(REPO_ROOT)}"
        )
        props_allow = set(props_missing) | set(props_extra)
    new_props_missing = sorted(set(props_missing) - props_allow)
    new_props_extra = sorted(set(props_extra) - props_allow)
    stale_props = sorted(props_allow - set(props_missing) - set(props_extra))

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
                ref_surface,
                unrecoverable,
                props_missing,
                props_extra,
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

    if not args.quiet and (props_missing or props_extra):
        print(
            f"native_parity: {len(props_missing)} getter/setter "
            f"mismatch(es), {len(props_extra)} extra accessor(s):"
        )
        for key, detail in props_missing.items():
            print(f"  - {key}: {detail}")
        for key, detail in props_extra.items():
            print(f"  ~ {key}: {detail} (extra)")

    if stale:
        print(
            f"native_parity: {len(stale)} allowlisted member(s) are now "
            "implemented — shrink scripts/allowlists/native_parity_allow.txt:"
        )
        for entry in stale:
            print(f"  - {entry}")

    if stale_props:
        print(
            f"native_parity: {len(stale_props)} allowlisted getter/setter "
            "mismatch(es) are now resolved — shrink "
            "scripts/allowlists/native_parity_props_allow.txt:"
        )
        for entry in stale_props:
            print(f"  - {entry}")

    if new_props_missing:
        print(
            f"native_parity: {len(new_props_missing)} new getter/setter "
            f"mismatch(es) not in "
            f"{PROPS_ALLOWLIST_PATH.relative_to(REPO_ROOT)}:",
            file=sys.stderr,
        )
        for entry in new_props_missing:
            print(f"  - {entry}: {props_missing[entry]}", file=sys.stderr)
        print(
            "native_parity: run with --update-props-allow only after reviewing "
            "these.",
            file=sys.stderr,
        )
        return 1

    if new_props_extra:
        print(
            f"native_parity: {len(new_props_extra)} new extra-accessor "
            f"mismatch(es) not in "
            f"{PROPS_ALLOWLIST_PATH.relative_to(REPO_ROOT)}:",
            file=sys.stderr,
        )
        for entry in new_props_extra:
            print(f"  - {entry}: {props_extra[entry]} (extra)", file=sys.stderr)
        print(
            "native_parity: run with --update-props-allow only after reviewing "
            "these.",
            file=sys.stderr,
        )
        return 1

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


def run(argv=None):
    """Run the check. ``argv`` carries the check-specific flags."""
    return main(argv)


if __name__ == "__main__":
    raise SystemExit(run(sys.argv[1:]))
