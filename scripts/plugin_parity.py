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
  ``NCB_REGISTER_FUNCTION`` attach free functions to a class.  Class and
  global members installed after construction with
  ``PropSet(TJS_MEMBERENSURE, ...)`` are read too: a namespace attaching a
  class (``motion->PropSet("Player")`` -> ``Motion.Player``), the global
  object registering a free function (``getDirList``), and a TJS global
  function published from a ``TVPExecuteScript`` string (``drawFFTGraph``).
  A ``PropSet`` whose name is built at runtime is reported as unrecoverable
  rather than dropped.  Preprocessor
  regions disabled in a normal build (``#if 0``) are ignored, as are
  ``#define`` bodies (so the macros themselves are not mistaken for uses).
* Rust surface: every ``.rs`` file under ``crates/compat/src/natives/plugins``
  is parsed with ``scripts/native_parity.py`` helpers.  A class is declared by
  ``ctx.register_instance(&NativeInstanceBuilder { name: "X", .. })`` and
  attached members by ``ctx.register_static_members(&NativeStaticMembers {
  class_name: "X", .. })`` / ``ctx.register_instance_members(&
  NativeInstanceMembers { class_name: "X", .. })``.  Data members written
  directly onto a class object (``set_member(<class>.raw_id(), "name", value)``
  on a variable bound from ``eval_retained("<Class>")``) are recognised too:
  the reference registers them with ``Variant(_, TJS_STATICMEMBER)`` rather
  than an accessor property (the ``Scripts.pf*`` flags, the ``GdiPlus``
  constants).
* Missing = reference - rust.  A *plugin extra* is a member registered in a
  plugin file with no reference counterpart; core-native members and members
  inherited from a shared registration table (``EmotePlayer`` gets ``Player``'s
  table) are not extras.  Coverage is ``matched / reference``.

Namespace structure
-------------------
``NCB_REGISTER_CLASS(Ns)`` publishes ``global.Ns``; the ``NCB_SUBCLASS``
family inside that block attaches the mapped class as a static member named
``Ns.name``, while ``NCB_REGISTER_SUBCLASS`` alone publishes nothing.  The
checker models that relation:

* a missing namespace class object (``GdiPlus``) or attached class
  (``GdiPlus.Image``) is an allowlistable ratchet gap, keyed ``Class`` /
  ``Namespace.Name`` like a member;
* hard, non-allowlistable failures are the wrong-level cases — a class the
  reference only nests exposed as a top-level global (a Rust port that aliases
  the class into the namespace and ``delete_member``s the global is accepted),
  and a top-level class a plugin file registers that the reference never
  publishes at all (``LayerExMovie``, ``LayerExPerspective``).  Namespaces,
  nested classes, core natives and TJS-script classes (``WIN32Dialog``) are
  excluded from the latter.

Ratchet
-------
``scripts/plugin_parity_allow.txt`` lists the currently accepted missing
surface: members (``Class.member``), nested classes (``GdiPlus.Image``) and
namespace objects (``GdiPlus``).  The checker fails when a *new* missing entry
appears.  When an allowlisted entry becomes implemented it is reported as
stale so the list can shrink.

Usage
-----
    python3 scripts/plugin_parity.py                 # check + regenerate report
    python3 scripts/plugin_parity.py --update-allow  # refresh the allowlist
    python3 scripts/plugin_parity.py --no-doc        # skip docs/plugin_parity.md
    python3 scripts/plugin_parity.py --quiet         # only print failures
    python3 scripts/plugin_parity.py --self-test     # extractor fixture self-test
    python3 scripts/plugin_parity.py --runtime target/runtime_surface.json \
        --no-doc                                     # check the live VM surface

Do not hand-edit ``docs/plugin_parity.md``: it is generated.
"""

from __future__ import annotations

import argparse
import datetime
import glob
import json
import re
import sys
from collections import OrderedDict
from dataclasses import dataclass, field
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
FIXTURES_DIR = REPO_ROOT / "scripts" / "parity_fixtures"

REFERENCE_GLOBS = ("**/*.cpp", "**/*.hpp", "**/*.h")

# Reference classes that are GDI+ rendering internals rather than game-script
# surface.  ``GdiPlus`` is *not* excluded: it is a real script namespace
# (``NCB_REGISTER_CLASS(GdiPlus)`` with ``NCB_SUBCLASS`` attachments) and the
# namespace-object guard below relies on modelling it.  The set is kept so a
# future genuine exclusion has a home.
EXCLUDED_REFERENCE_CLASSES: set[str] = set()

# ``Debug.console`` is a dotted target: the Rust port attaches the members to
# the native ``Console`` class, so map the reference spelling onto it.
CLASS_ALIASES = {"Debug.console": "Console"}

# Rust classes that are the port of a reference class/extension under a
# different name.  ``Motion.Rect`` is registered as ``MotionRect`` in Rust
# because the core VM already owns a ``Rect`` class.
RUST_CLASS_ALIASES = {"MotionRect": "Rect"}

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


@dataclass
class ReferenceModel:
    """The reference plugin surface split by how it becomes script-visible.

    ``classes`` is the script-visible surface: top-level classes plus
    namespace-attached classes under their dotted spelling (``Motion.Point``,
    ``GdiPlus.Appearance``).  ``nested_by_ns`` records the ``NCB_SUBCLASS``
    attachment relation (namespace -> public name -> internal class) and
    ``namespace_objects`` the namespaces that carry one; ``published_top_level``
    is the set of names the reference publishes as a global class, which the
    port-level class guard compares against.
    """

    classes: dict[str, set[str]]
    impl_classes: dict[str, set[str]]
    nested_by_ns: dict[str, dict[str, str]] = field(default_factory=dict)
    namespace_objects: set[str] = field(default_factory=set)
    published_top_level: set[str] = field(default_factory=set)
    unrecoverable: list[str] = field(default_factory=list)
    propset_members: dict[str, set[str]] = field(default_factory=dict)


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

# Class-level registration performed after a class/object is built.
# ``_PLUGIN_PROPSET_RE`` finds the receiver; ``_split_call_args`` reads the
# flag/name arguments.  A namespace object is a variable bound (usually via
# ``global->PropGet(..., TJS_W("Motion"), ...)``) to a namespace dispatch.
_PLUGIN_PROPSET_RE = re.compile(
    r"([A-Za-z_]\w*)\s*(?:->|\.)\s*(PropSet|PropSetByNum)\s*\("
)
_PLUGIN_PROPGET_RE = re.compile(r"\bPropGet\s*\(")
_PLUGIN_AS_OBJECT_RE = re.compile(
    r"([A-Za-z_]\w*)\s*=\s*([A-Za-z_]\w*)\s*\.\s*AsObjectNoAddRef\s*\("
)
_SCRIPT_GLOBAL_FUNCTION_RE = re.compile(r"\bfunction\s+([A-Za-z_]\w*)\s*\(")


def _plugin_namespace_bindings(src: str) -> dict[str, str]:
    """Namespace-object variable -> TJS namespace name for one plugin file.

    The reference reaches a namespace object by reading a global
    (``global->PropGet(0, TJS_W("Motion"), ..., &motionVar, global)`` then
    ``motion = motionVar.AsObjectNoAddRef()``).  ``global`` itself always maps
    to the synthetic global class.
    """
    bindings: dict[str, str] = {"global": GLOBAL_CLASS}
    propvars: dict[str, set[str]] = {}
    for m in _PLUGIN_PROPGET_RE.finditer(src):
        args = np._split_cpp_args(src, m.end() - 1)
        name = None
        out_var = None
        for arg in args:
            literal = np._cpp_string_literal(arg)
            if literal is not None:
                name = literal
            om = re.fullmatch(r"&\s*([A-Za-z_]\w*)", arg)
            if om:
                out_var = om.group(1)
        if name and out_var:
            propvars.setdefault(out_var, set()).add(name)
    for m in _PLUGIN_AS_OBJECT_RE.finditer(src):
        var, propvar = m.group(1), m.group(2)
        # A scratch variant (``tmp``) is often reused by several ``PropGet``
        # calls, so only a single-valued binding is trusted.
        names = propvars.get(propvar)
        if names is not None and len(names) == 1:
            bindings[var] = next(iter(names))
    return bindings


def _script_globals(classes: dict[str, set[str]], src: str) -> set[str]:
    """Global TJS functions defined in ``TVPExecuteScript`` strings.

    ``fftgraph.cpp`` publishes ``drawFFTGraph`` from a script string, the same
    way ``_script_classes`` reads script classes.  Functions declared inside a
    class body the reader already harvested are excluded.
    """
    class_members = {m for members in classes.values() for m in members}
    out: set[str] = set()
    for m in re.finditer(r"\bTVPExecuteScript\s*\(", src):
        args, _ = _split_call_args(src, m.end() - 1)
        script = _script_text(" ".join(args))
        for fm in _SCRIPT_GLOBAL_FUNCTION_RE.finditer(script):
            name = fm.group(1)
            if name not in class_members:
                out.add(name)
    return out


def _plugin_propset_sites(src: str) -> list[tuple[str, str, str, str]]:
    """``(receiver, method, flag_text, name_expr)`` for each class-level call."""
    out: list[tuple[str, str, str, str]] = []
    for m in _PLUGIN_PROPSET_RE.finditer(src):
        args = np._split_cpp_args(src, m.end() - 1)
        if len(args) < 2:
            continue
        out.append((m.group(1), m.group(2), args[0], args[1]))
    return out


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


# Subclass attachments inside a namespace block.  ``NCB_SUBCLASS_NAME`` and
# ``NCB_GDIP_SUBCLASS`` are file-local spellings of ``NCB_SUBCLASS``
# (``#define``d in ``layerex_draw``); ``SubClass`` is the raw container call.
_SUBCLASS_CALL_RE = re.compile(
    r"\bNCB_SUBCLASS(?:_NAME)?\s*\("
    r"|\bNCB_GDIP_SUBCLASS\s*\("
    r"|\bSubClass\s*\("
)

_SCRIPT_CLASS_RE = re.compile(r"\bclass\s+([A-Za-z_]\w*)")
_SCRIPT_FUNCTION_RE = re.compile(r"\bfunction\s+([A-Za-z_]\w*)\s*\(")
_CPP_STRING_RE = re.compile(r'"((?:[^"\\]|\\.)*)"')

# ``tTJSNativeClass *cls = TVPCreateNativeClass_Storages();`` and the
# ``REGISTER_OBJECT(Storages, cls)`` macro both name the class a variable owns.
_TVPCREATE_RE = re.compile(r"\bTVPCreateNativeClass_([A-Za-z_]\w*)\s*\(")
_REGISTER_OBJECT_RE = re.compile(
    r"\bREGISTER_OBJECT\s*\(\s*([A-Za-z_]\w*)\s*,\s*([A-Za-z_]\w*)\s*\)"
)
_REGISTER_NCM_RE = re.compile(r"\bTJSNativeClassRegisterNCM\s*\(")


def _cpp_name_literal(text: str) -> "str | None":
    """The TJS name in ``TJS_W("X")`` / ``X`` / ``"X"``."""
    m = re.search(r'TJS_W\s*\(\s*"([^"]+)"\s*\)', text)
    if m:
        return m.group(1)
    m = re.fullmatch(r'"?([A-Za-z_]\w*)"?', text.strip())
    return m.group(1) if m else None


def _subclass_attachments(body: str) -> list[tuple[str, str]]:
    """``(public_name, internal_class)`` attachments in a class body.

    ``NCB_SUBCLASS(name, cls)`` publishes ``Ns.name`` backed by ``cls``;
    ``NCB_SUBCLASS_NAME(name)`` is ``NCB_SUBCLASS(name, name)`` and
    ``NCB_GDIP_SUBCLASS(Class)`` is ``NCB_SUBCLASS(Class, GdipWrapper<Class>)``.
    ``SubClass(TJS_W("name"), TypeWrap<T>())`` is the raw container call.
    """
    out: list[tuple[str, str]] = []
    for m in _SUBCLASS_CALL_RE.finditer(body):
        macro = m.group(0).strip().rstrip("(").strip()
        args, _ = _split_call_args(body, m.end() - 1)
        if not args:
            continue
        public = _cpp_name_literal(args[0])
        if not public:
            continue
        if macro == "NCB_GDIP_SUBCLASS":
            internal = f"GdipWrapper<{public}>"
        elif macro == "NCB_SUBCLASS_NAME":
            internal = public
        else:
            internal = _cpp_name_literal(args[1]) if len(args) >= 2 else public
        out.append((public, internal or public))
    return out


def _script_text(text: str) -> str:
    """Concatenate the C++ string literals of a ``TVPExecuteScript`` argument."""
    return "".join(_CPP_STRING_RE.findall(text))


def _script_classes(src: str) -> dict[str, set[str]]:
    """TJS classes (and their ``function`` members) in ``TVPExecuteScript``.

    ``win32dialog.cpp`` publishes ``WIN32Dialog`` from a script string rather
    than an ncbind macro, so it is not reachable through the class spans; its
    ``function messageBox(...)`` is a real member and is harvested from the
    enclosing ``class`` body.
    """
    out: dict[str, set[str]] = {}
    for m in re.finditer(r"\bTVPExecuteScript\s*\(", src):
        args, _ = _split_call_args(src, m.end() - 1)
        script = _script_text(" ".join(args))
        for cm in _SCRIPT_CLASS_RE.finditer(script):
            name = cm.group(1)
            members = out.setdefault(name, set())
            brace = script.find("{", cm.end())
            if brace == -1:
                continue
            body = script[brace : np._match_brace(script, brace)]
            members.update(_SCRIPT_FUNCTION_RE.findall(body))
    return out


def _tvp_native_class_bindings(src: str) -> dict[str, str]:
    """Variables holding a class object named by ``TVPCreateNativeClass_*``.

    Handles the declaration ``tTJSNativeClass *cls =
    TVPCreateNativeClass_Storages();`` and the ``REGISTER_OBJECT(Storages,
    cls)`` macro invocation.
    """
    bindings: dict[str, str] = {}
    for m in _TVPCREATE_RE.finditer(src):
        prefix = src[max(0, m.start() - 96) : m.start()]
        am = re.search(r"([A-Za-z_]\w*)\s*=\s*$", prefix)
        if am:
            bindings[am.group(1)] = m.group(1)
    for m in _REGISTER_OBJECT_RE.finditer(src):
        bindings[m.group(2)] = m.group(1)
    return bindings


def _ncm_members(src: str, bindings: dict[str, str]) -> dict[str, set[str]]:
    """Members registered with ``TJSNativeClassRegisterNCM``.

    The second argument is the script name; the fourth is a runtime expression
    (``cls->getClassName().c_str()``), so the class comes from the receiver's
    ``TVPCreateNativeClass_*`` / ``REGISTER_OBJECT`` binding instead.
    """
    out: dict[str, set[str]] = {}
    for m in _REGISTER_NCM_RE.finditer(src):
        args, _ = _split_call_args(src, m.end() - 1)
        if len(args) < 2:
            continue
        var = args[0].strip().lstrip("&").strip()
        cls = bindings.get(var)
        if not cls:
            continue
        name = _cpp_name_literal(args[1])
        if name:
            out.setdefault(cls, set()).add(name)
    return out


def _collect_class_spans(
    src: str,
) -> list[tuple[int, int, str, "str | None", str]]:
    """Return ``(start, end, class_name, impl_name, kind)`` for every block.

    ``impl_name`` is the implementation class of an ``NCB_ATTACH_CLASS*`` block
    (the first argument).  ``kind`` is ``"top"`` for a class the reference
    publishes globally, ``"nested"`` for an ``NCB_REGISTER_SUBCLASS*`` class
    (script-visible only when a namespace block attaches it) and ``"attach"``
    for ``NCB_ATTACH_CLASS*`` (members attach to an existing class).  A Rust
    port may be named after the implementation rather than the TJS target, so
    the implementation name is tracked separately.
    """
    spans: list[tuple[int, int, str, "str | None", str]] = []
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
            kind = "attach"
        else:
            cls = args[0] if args else None
            kind = "top" if macro.startswith("NCB_REGISTER_CLASS") else "nested"
        if cls:
            spans.append((m.start(), end, _normalize_class(cls), impl, kind))

    for m in _TJS_MEMBERS_RE.finditer(src):
        end_m = _TJS_END_MEMBERS_RE.search(src, m.end())
        end = end_m.start() if end_m else len(src)
        spans.append((m.start(), end, _normalize_class(m.group(1)), None, "top"))
    return spans


def _innermost(
    spans: list[tuple[int, int, str, "str | None", str]], pos: int
) -> "tuple[int, int, str, str | None, str] | None":
    containing = [s for s in spans if s[0] < pos < s[1]]
    if not containing:
        return None
    return min(containing, key=lambda s: s[1] - s[0])


def _visible_span_class(
    span: tuple[int, int, str, "str | None", str],
    nested_visible: dict[str, str],
) -> str:
    """The script-visible class name for a member's innermost span."""
    _, _, cls, _, kind = span
    if kind == "nested":
        return nested_visible.get(cls, cls)
    return cls


def _canon_class(name: str) -> str:
    """Canonicalise a C++ implementation name to the Rust class spelling."""
    return name[:1].upper() + name[1:] if name else name


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


def parse_reference(sources: "list[Path] | None" = None) -> ReferenceModel:
    """Return the reference plugin surface as a :class:`ReferenceModel`.

    ``sources`` overrides the reference plugin directory; the self-test passes
    a single fixture file.  Class blocks are classified top-level vs. ``NCB_REGISTER_SUBCLASS``
    (nested).  The ``NCB_SUBCLASS``/``SubClass`` attachments inside a
    top-level block map their internal classes onto the dotted
    ``Namespace.name`` surface, so a class is compared at the place the
    reference actually exposes it.  ``impl_classes`` maps an
    ``NCB_ATTACH_CLASS*`` implementation class (or the file stem of a manual
    ``addMethod`` registration) to its members, so a Rust class named after the
    implementation is matched to the plugin surface.
    """
    classes: dict[str, set[str]] = {}
    impl_classes: dict[str, set[str]] = {}
    nested_by_ns: dict[str, dict[str, str]] = {}
    namespace_objects: set[str] = set()
    published_top_level: set[str] = set()
    unrecoverable: list[str] = []
    global_members: set[str] = set()
    propset_members: dict[str, set[str]] = {}

    files: list[tuple[Path, str, list[tuple[int, int, str, "str | None", str]]]] = []
    for fp in (sources if sources is not None else _iter_reference_files()):
        raw = fp.read_text(encoding="utf-8", errors="replace")
        src = _blank_directives(np._strip_cpp_comments(np._preprocess_cpp(raw)))
        files.append((fp, src, _collect_class_spans(src)))

    # Namespace nesting first: a namespace may attach a nested class defined
    # earlier in the same translation unit.
    for fp, src, spans in files:
        for start, end, cls, _impl, kind in spans:
            if kind == "top":
                published_top_level.add(cls)
                subs = _subclass_attachments(src[start:end])
                if subs:
                    namespace_objects.add(cls)
                    nested_by_ns.setdefault(cls, {}).update(dict(subs))
            elif kind == "attach" and cls:
                published_top_level.add(cls)
        for script_cls, script_members in _script_classes(src).items():
            classes.setdefault(script_cls, set()).update(script_members)
            published_top_level.add(script_cls)

        # Class-level members installed after construction.  A namespace
        # object (``Motion``) can attach a class with ``PropSet``; the global
        # object registers a free function (``getDirList``); a class object
        # with ``TJS_STATICMEMBER`` whose receiver is a lambda parameter cannot
        # be attributed, and a name that is not a literal is unrecoverable.
        bindings = _plugin_namespace_bindings(src)
        for var, method, flag, name_expr in _plugin_propset_sites(src):
            target = bindings.get(var)
            name = np._cpp_string_literal(name_expr)
            rel = str(fp.relative_to(REPO_ROOT))
            if target == GLOBAL_CLASS:
                if name:
                    global_members.add(name)
                    propset_members.setdefault(GLOBAL_CLASS, set()).add(name)
                else:
                    unrecoverable.append(f"{rel}: global.{method}: {name_expr}")
                continue
            if target is not None:
                if name:
                    nested_by_ns.setdefault(target, {})[name] = name
                    namespace_objects.add(target)
                    propset_members.setdefault(target, set()).add(name)
                else:
                    unrecoverable.append(
                        f"{rel}: {target}.{var}.{method}: {name_expr}"
                    )
                continue
            if "TJS_STATICMEMBER" in flag:
                # A class object passed to a helper (``playerClass``); the
                # literal name is visible but the class is not statically
                # bound here.
                detail = name if name is not None else name_expr
                unrecoverable.append(
                    f"{rel}: unresolved static member {var}.{method}: {detail}"
                )

    nested_visible: dict[str, str] = {}
    for ns, mapping in nested_by_ns.items():
        for public, internal in mapping.items():
            nested_visible[internal] = f"{ns}.{public}"

    for fp, src, spans in files:
        for regex in _MEMBER_RES:
            for m in regex.finditer(src):
                span = _innermost(spans, m.start())
                if span is None:
                    continue
                visible = _visible_span_class(span, nested_visible)
                if visible in EXCLUDED_REFERENCE_CLASSES:
                    continue
                classes.setdefault(visible, set()).add(m.group(1))
                if span[3]:
                    impl_classes.setdefault(span[3], set()).add(m.group(1))

        for m in _CONSTRUCTOR_RE.finditer(src):
            span = _innermost(spans, m.start())
            if span is None:
                continue
            visible = _visible_span_class(span, nested_visible)
            if visible in EXCLUDED_REFERENCE_CLASSES:
                continue
            classes.setdefault(visible, set()).add(visible)

        for m in _ATTACH_FUNCTION_RE.finditer(src):
            cls = _normalize_class(m.group(2))
            if cls not in EXCLUDED_REFERENCE_CLASSES:
                classes.setdefault(cls, set()).add(m.group(1))
                published_top_level.add(cls)
        for m in _ATTACH_FUNCTION_TAG_RE.finditer(src):
            cls = _normalize_class(m.group(3))
            if cls not in EXCLUDED_REFERENCE_CLASSES:
                classes.setdefault(cls, set()).add(m.group(1))
                published_top_level.add(cls)
        for m in _REGISTER_FUNCTION_RE.finditer(src):
            classes.setdefault(GLOBAL_CLASS, set()).add(m.group(1))
        for m in _TJS_CREATE_CLASS_RE.finditer(src):
            cls = _normalize_class(m.group(1))
            if cls not in EXCLUDED_REFERENCE_CLASSES:
                classes.setdefault(cls, set())
                published_top_level.add(cls)

        # Global functions published from a script string, and the
        # ``global->PropSet`` free functions collected in the first pass.
        classes.setdefault(GLOBAL_CLASS, set()).update(_script_globals(classes, src))

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
                    published_top_level.add(cls)
            impl_classes.setdefault(fp.stem, set()).add(name)

        # Older native-class registrations: ``cls`` is the class object built
        # by ``TVPCreateNativeClass_<Name>()`` (or named by
        # ``REGISTER_OBJECT(<Name>, cls)``), and ``TJSNativeClassRegisterNCM``
        # adds a static member to it.
        ncm_bindings = _tvp_native_class_bindings(src)
        if ncm_bindings:
            for cls, names in _ncm_members(src, ncm_bindings).items():
                if cls not in EXCLUDED_REFERENCE_CLASSES:
                    classes.setdefault(cls, set()).update(names)
                    published_top_level.add(cls)

    classes.setdefault(GLOBAL_CLASS, set()).update(global_members)

    return ReferenceModel(
        classes=classes,
        impl_classes=impl_classes,
        nested_by_ns=nested_by_ns,
        namespace_objects=namespace_objects,
        published_top_level=published_top_level,
        unrecoverable=unrecoverable,
        propset_members=propset_members,
    )


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

    # ``register_builtin_static_members(engine, "Class", &[..], &[..])``.
    # The reference attaches a class's members with a per-member static flag on
    # one ``NCB_ATTACH_CLASS`` call, so those members carry the objthis
    # signatures and are passed as a slice rather than a ``Native*Members``
    # block. The second argument names the class; the member ``name:``
    # literals sit in the slice.
    for m in re.finditer(r"\bregister_builtin_static_members\s*\(", code):
        arg_start = m.end()
        arg_end = _scan_paren(code, arg_start)
        args = _split_top_level(raw[arg_start:arg_end])
        if len(args) < 4:
            continue
        # A leading comment lands in the first split, so take the first
        # argument that opens with a string literal rather than assuming a
        # position.
        nm = None
        for arg in args:
            nm = re.match(r'\s*"([^"]+)"', arg)
            if nm:
                break
        if not nm:
            continue
        spans.append((arg_start, arg_end, nm.group(1), None))
    return spans


# ``for (public, internal, keep) in [ ("a", "b", bool), ... ]``: the port
# registers each class globally and then aliases it into a namespace object.
_ALIAS_LOOP_RE = re.compile(r"\bfor\s*\([^)]*\)\s+in\s*\[", re.S)
_ALIAS_TUPLE_RE = re.compile(
    r'\(\s*"([^"]+)"\s*,\s*"([^"]+)"\s*,\s*(true|false)\s*\)'
)


def _rust_aliases(code: str, raw: str) -> list[tuple[str, str, bool]]:
    """``(public, internal, keeps_global)`` namespace-alias tables in one file.

    A namespace alias table is a ``for (...) in [ ("a", "b", bool), ... ]``
    loop whose body also deletes the registration global
    (``delete_member(global, internal)``); each tuple maps the namespace's
    public member name to the class's global registration name.
    """
    out: list[tuple[str, str, bool]] = []
    for m in _ALIAS_LOOP_RE.finditer(code):
        bracket = m.end() - 1
        arr_end = np._match_bracket(code, bracket)
        brace = code.find("{", arr_end)
        if brace == -1:
            continue
        body_end = np._match_brace(code, brace)
        if "delete_member" not in code[brace:body_end]:
            continue
        for tm in _ALIAS_TUPLE_RE.finditer(raw[bracket + 1 : arr_end]):
            out.append((tm.group(1), tm.group(2), tm.group(3) == "true"))
    return out


def _class_registration_names(
    code: str, raw: str, strings: dict[int, tuple[int, str, int]]
) -> set[str]:
    """Class names registered by a ``Native*Builder { name: ... }`` block.

    ``NativeStaticMembers``/``NativeInstanceMembers`` use ``class_name:`` and
    only *attach* members to a class that already exists, so they are not a
    class registration.
    """
    names: set[str] = set()
    consts = np._const_strings(code, strings)
    for m in re.finditer(r"Native(?:Instance|Class)Builder\s*\{", code):
        open_pos = code.index("{", m.start())
        end = np._match_brace(code, open_pos)
        seg = raw[open_pos:end]
        nm = re.search(r'\bname\s*:\s*"([^"]+)"', seg)
        if nm:
            names.add(nm.group(1))
            continue
        nm = re.search(r"\bname\s*:\s*([A-Za-z_]\w*)", seg)
        if nm and nm.group(1) in consts:
            names.add(consts[nm.group(1)])
    return names


def _class_object_bindings(code: str, raw: str) -> dict[str, set[str]]:
    """Variables holding a class object returned by ``eval_retained("X")``.

    Recognises ``let x = match engine.eval_retained("X", ..) { ..
    RetainedValue::Object(x) .. }`` and ``let Ok(RetainedValue::Object(x)) =
    engine.eval_retained("X", ..)``.  Only string-literal class names are
    resolved: ``eval_retained(internal, ..)`` reads a variable and is left to
    the namespace-alias logic.
    """
    bindings: dict[str, set[str]] = {}
    for m in re.finditer(r'eval_retained\s*\(\s*"([^"]+)"', raw):
        cls = m.group(1)
        # A class name is an identifier (possibly dotted); ``global`` and
        # literals such as ``%[]`` name an object, not a class.
        if cls == "global" or not re.fullmatch(r"[A-Za-z_][\w.]*", cls):
            continue
        p = m.start()
        # Backward: a `let` binding in the initialiser pattern (not the match
        # arm, which sits after the call).
        let_pos = code.rfind("let", 0, p)
        if let_pos != -1:
            eq = code.find("=", let_pos)
            if let_pos < eq < p:
                for bm in re.finditer(
                    r"RetainedValue::Object\s*\(\s*([A-Za-z_]\w*)\s*\)",
                    code[let_pos:eq],
                ):
                    bindings.setdefault(bm.group(1), set()).add(cls)
        # Forward: the match-arm binding `Ok(RetainedValue::Object(x)) => x`.
        paren = code.find("(", p)
        close = _scan_paren(code, paren + 1)
        stmt_end = _scan_to_semicolon(code, close + 1)
        for bm in re.finditer(
            r"RetainedValue::Object\s*\(\s*([A-Za-z_]\w*)\s*\)",
            code[close:stmt_end],
        ):
            bindings.setdefault(bm.group(1), set()).add(cls)
    return bindings


def _loop_var_strings(code: str, raw: str, var: str) -> set[str]:
    """String-literal members a ``for`` loop binds to ``var``.

    Handles a tuple array such as ``const PF: &[(&str, i64)] = &[("a", 1), ..]``
    iterated by ``for &(name, value) in PF`` (taking the element position the
    binding occupies) and a plain ``&[&str]`` array.
    """
    out: set[str] = set()
    for m in re.finditer(r"\bfor\s+([^;{]+?)\s+in\s+([A-Za-z_]\w*)", code):
        names = re.findall(r"[A-Za-z_]\w*", m.group(1))
        if var not in names:
            continue
        idx = names.index(var)
        arr = m.group(2)
        init = re.search(
            r"\b(?:const|static|let)\s+(?:mut\s+)?" + re.escape(arr) + r"\b[^=;]*?=",
            code,
        )
        if not init:
            continue
        bracket = code.find("[", init.end())
        if bracket == -1:
            continue
        end = np._match_bracket(code, bracket)
        for element in _split_top_level(raw[bracket + 1 : end]):
            if element.startswith("(") and element.endswith(")"):
                fields = _split_top_level(element[1:-1])
                chunk = fields[idx] if idx < len(fields) else ""
            else:
                chunk = element
            lit = re.search(r'"([^"]*)"', chunk)
            if lit:
                out.add(lit.group(1))
    return out


def _rust_data_members(
    code: str, raw: str, bindings: dict[str, set[str]]
) -> dict[str, set[str]]:
    """Members written with ``set_member(<class>.raw_id(), "name", value)``.

    The reference registers these as data members (``Variant(_,
    TJS_STATICMEMBER)``), not accessor properties, so they have no builder
    definition.  Only calls whose receiver is a variable bound from
    ``eval_retained("<Class>")`` are attributed; a ``Retained`` value is
    namespace-alias wiring and stays with the namespace logic.
    """
    out: dict[str, set[str]] = {}
    for m in re.finditer(r"\bset_member\s*\(", code):
        args, _ = _split_call_args(raw, m.end() - 1)
        if len(args) < 3:
            continue
        rm = re.search(r"([A-Za-z_]\w*)\s*\.\s*raw_id\s*\(", args[0])
        if not rm:
            continue
        var = rm.group(1)
        if var not in bindings:
            continue
        if "Retained" in args[2]:
            continue
        name_arg = args[1].strip()
        lm = re.fullmatch(r'"([^"]*)"', name_arg)
        if lm:
            names = {lm.group(1)}
        else:
            nm = re.fullmatch(r"&?\s*([A-Za-z_]\w*)", name_arg)
            if not nm:
                continue
            names = _loop_var_strings(code, raw, nm.group(1)) or _string_locals(
                code, raw, nm.group(1)
            )
        if not names:
            continue
        for cls in bindings[var]:
            out.setdefault(cls, set()).update(names)
    return out


def _string_locals(code: str, raw: str, var: str) -> set[str]:
    """String literals a local binding ``let var = "..."`` holds."""
    out: set[str] = set()
    for m in re.finditer(
        r"\blet\s+(?:mut\s+)?" + re.escape(var) + r"\b[^=;]*=\s*", code
    ):
        lit = re.match(r'"([^"]*)"', raw[m.end() : m.end() + 200])
        if lit:
            out.add(lit.group(1))
    return out


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

    # Data members written directly onto a class object with ``set_member``
    # (reference ``Variant(_, TJS_STATICMEMBER)``): the ``Scripts.pf*`` flags
    # and the ``GdiPlus`` enum constants.  Added last so the loop/array
    # handling above sees only the builder classes.
    bindings = _class_object_bindings(code, raw)
    if bindings:
        for cls, names in _rust_data_members(code, raw, bindings).items():
            result.setdefault(cls, set()).update(names)

    # TJS functions installed from a script string.  They are not attributed
    # to a class; ``parse_rust`` keeps only the names the reference publishes
    # as globals.
    script_globals: set[str] = set()
    for _off, (_end, content, _quote) in strings.items():
        for m in _SCRIPT_GLOBAL_FUNCTION_RE.finditer(content):
            script_globals.add(m.group(1))
    if script_globals:
        result.setdefault("__script__", set()).update(script_globals)

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


def parse_rust(
    global_names: "set[str] | None" = None,
) -> tuple[
    dict[str, set[str]],
    dict[str, list[str]],
    dict[str, set[str]],
    dict[str, set[str]],
    list[tuple[set[str], set[str], "str | None"]],
    list[tuple[str, str, bool]],
    set[str],
    set[str],
]:
    """Return ``(members, sources, plugin_only, core_members, dynamic_groups,
    aliases, plugin_classes, core_classes)``.

    ``global_names`` is the reference global-function surface; a script-defined
    function is kept only when it is one of those names, so an internal helper
    installed with ``exec_script`` is not mistaken for script surface.

    ``plugin_only`` holds the members registered by a plugin file that no core
    file also registers; those are the only candidates for a *plugin* extra.
    ``dynamic_groups`` describes each shared registration table as
    ``(classes, base_members, primary_class)`` so inherited members can be
    identified (``EmotePlayer`` inherits ``Player``'s table).  ``aliases`` is the
    union of the ``set_member``/``delete_member`` namespace tables;
    ``plugin_classes``/``core_classes`` are the class names actually registered
    by a plugin / core file (member attachments excluded).
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
    aliases: list[tuple[str, str, bool]] = []
    plugin_classes: set[str] = set()
    core_classes: set[str] = set()
    dynamic: list[tuple[str, int, "int | None", set[str], set[str], Path]] = []
    for path, raw, code, strings, local in files:
        res, dyn = _parse_rust_file(path, raw, code, strings, local, global_defs)
        script_members = res.pop("__script__", set())
        if global_names is not None:
            res.setdefault(GLOBAL_CLASS, set()).update(script_members & global_names)
        rel = str(path.relative_to(REPO_ROOT))
        is_plugin = _is_plugin_file(path)
        target = plugin_members if is_plugin else core_members
        registered = _class_registration_names(code, raw, strings)
        (plugin_classes if is_plugin else core_classes).update(registered)
        aliases.extend(_rust_aliases(code, raw))
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
    return (
        members,
        sources,
        plugin_only,
        core_members,
        groups,
        aliases,
        plugin_classes,
        core_classes,
    )


# ---------------------------------------------------------------------------
# Allowlist + report
# ---------------------------------------------------------------------------


def read_allowlist(path: Path) -> set[str]:
    return set(read_allowlist_reasons(path))


def read_allowlist_reasons(path: Path) -> "OrderedDict[str, str]":
    """Allowlist entries as ``{key: reason}``.

    An entry is ``Class.member`` (optionally ``Namespace.Name`` or a namespace
    object) followed by a reason after an em dash, ``#`` or `` -- ``.  A
    reason starting with ``debt:`` marks a deliberately-baselined known gap
    (mirrors ``scripts/layering_allow.txt``); a bare entry has an empty reason.
    """
    entries: "OrderedDict[str, str]" = OrderedDict()
    if not path.exists():
        return entries
    for line in path.read_text(encoding="utf-8").splitlines():
        text = line.strip()
        if not text or text.startswith("#"):
            continue
        for sep in ("\u2014", " -- ", " # "):
            if sep in text:
                key, _, reason = text.partition(sep)
                entries[key.strip()] = reason.strip()
                break
        else:
            entries[text] = ""
    return entries


def write_allowlist(
    path: Path, entries: set[str], reasons: "dict[str, str] | None" = None
) -> None:
    header = (
        "# Plugin parity allowlist: reference surface that is known-missing.\n"
        "# One entry per line: Class.member, a class (`GdiPlus.Image`) or a\n"
        "# namespace object (`GdiPlus`), optionally followed by '— reason'.\n"
        "# A reason beginning with 'debt:' records a known gap that is accepted\n"
        "# but still visible; see docs/audits/propset-parity.md.\n"
        "# Regenerate after intentional additions with:\n"
        "#   python3 scripts/plugin_parity.py --update-allow\n"
        "# The checker fails when a missing entry is not listed here.\n"
        "\n"
    )
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
    rust_sources: dict[str, list[str]],
    missing_by_class: dict[str, set[str]],
    extra_by_class: dict[str, set[str]],
    core_by_class: dict[str, set[str]],
    tracked: list[str],
    structure_missing: "set[str] | None" = None,
    wrong_level_failures: "list[str] | None" = None,
    port_only_failures: "list[str] | None" = None,
    propset_members: "dict[str, set[str]] | None" = None,
    unrecoverable: "list[str] | None" = None,
) -> str:
    structure_missing = structure_missing or set()
    wrong_level_failures = wrong_level_failures or []
    port_only_failures = port_only_failures or []
    propset_members = propset_members or {}
    unrecoverable = unrecoverable or []
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
    lines.append("python3 scripts/plugin_parity.py --self-test     # extractor fixture self-test")
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

    lines.append("## Members installed by `PropSet`")
    lines.append("")
    lines.append(
        "Class and global members the reference installs after construction "
        "with ``PropSet(TJS_MEMBERENSURE, ...)``: a namespace attaching a class "
        "(``Motion.Player``), the global object registering a free function "
        "(``getDirList``), or a class-object member. ``kind`` is unknown "
        "because ``PropSet`` does not reveal method versus property."
    )
    lines.append("")
    if propset_members:
        lines.append("| class | member | kind | static | mechanism |")
        lines.append("| --- | --- | --- | --- | --- |")
        for cls in sorted(propset_members):
            for member in sorted(propset_members[cls]):
                lines.append(
                    f"| `{cls}` | `{member}` | unknown | true | `propset` |"
                )
    else:
        lines.append("_None._")
    lines.append("")

    lines.append("## Registration sites with a runtime-built name")
    lines.append("")
    if unrecoverable:
        lines.append(
            f"{len(unrecoverable)} site(s) install a member whose name is not a "
            "string literal, or whose receiver is a helper parameter, so the "
            "source reader cannot recover it. These are not counted as "
            "members: the coverage figures exclude them, which is a remaining "
            "blind spot, not a claim they do not exist."
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
        cov = f"{100.0 * matched / len(r):.1f}%" if r else "n/a"
        lines.append(
            f"| `{cls}` | {len(r)} | {len(u)} | {matched} | "
            f"{len(missing_by_class.get(cls, ()))} | "
            f"{len(extra_by_class.get(cls, ()))} | {cov} |"
        )
    lines.append("")

    lines.append("## Namespace structure")
    lines.append("")
    lines.append(
        "The reference publishes a class object with ``NCB_REGISTER_CLASS`` and "
        "attaches classes inside it with ``NCB_SUBCLASS`` (script name "
        "``Namespace.name``); ``NCB_REGISTER_SUBCLASS`` alone never publishes a "
        "global. A missing namespace object or nested class is an allowlistable "
        "ratchet gap (like a missing member). A nested class exposed as a bare "
        "global, or a top-level class the reference never publishes, is a hard "
        "failure: that is the registration bug class."
    )
    lines.append("")
    lines.append("### Missing namespace / nested classes (allowlistable)")
    lines.append("")
    if not structure_missing:
        lines.append("_None._")
    else:
        lines.append(", ".join(f"`{e}`" for e in sorted(structure_missing)))
    lines.append("")
    lines.append("### Wrong-level classes (hard failures)")
    lines.append("")
    if not wrong_level_failures and not port_only_failures:
        lines.append("_None._")
    else:
        for line in wrong_level_failures + port_only_failures:
            lines.append(f"- {line}")
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
        "- `layerExBase` is not modelled as a class: it is an internal C++ base "
        "whose names (`imageWidth`, `clipLeft`, `update`, ...) are read from the "
        "`Layer` object (`reference/cpp/plugins/layerExBase.hpp:84-91`) and are "
        "part of the core `Layer` surface, which `native_parity.py` covers. "
        "`LayerExMovie`/`LayerExPerspective` are `NCB_ATTACH_CLASS` "
        "implementations, not published classes."
    )
    lines.append(
        "- `NCB_SUBCLASS`/`NCB_SUBCLASS_NAME`/`NCB_GDIP_SUBCLASS`/`SubClass` "
        "inside a `NCB_REGISTER_CLASS` block are modelled as `Namespace.name` "
        "classes. A Rust port that registers the class globally and then "
        "`set_member`s it into the namespace and `delete_member`s the global "
        "(`motion_player`) is recognised as the nested class."
    )
    lines.append(
        "- A plugin file that registers a class the reference never publishes "
        "at all (`LayerExMovie`, `LayerExPerspective`, a bare `LayerExBase`) is "
        "a hard failure, as is a nested class leaking to the top level. A "
        "missing namespace object or nested class is an allowlistable ratchet gap."
    )
    lines.append(
        "- Class and global members installed after construction with "
        "`PropSet(TJS_MEMBERENSURE, ...)` are read too: a namespace attaching a "
        "class (`motion->PropSet(\"Player\")` -> `Motion.Player`), the global "
        "object registering a free function (`getDirList`), and a TJS global "
        "function published from a `TVPExecuteScript` string (`drawFFTGraph`)."
    )
    lines.append(
        "- Remaining blind spots: a `PropSet` name built at runtime "
        "(`idstr.c_str()`) or a static member on an unresolved helper parameter "
        "(`playerClass->PropSet(...)`) is recorded as unrecoverable, not counted; "
        "the coverage figures therefore exclude it rather than claiming it absent."
    )
    lines.append(
        "- A `PropSet` on an instance object (a window, a dictionary, a method "
        "parameter) is not a class member and is not counted. A member installed "
        "by a plugin DLL that has no `reference/cpp` source is outside this "
        "report until its surface is extracted (D38)."
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
    if EXCLUDED_REFERENCE_CLASSES:
        lines.append(
            "Reference classes in `"
            + "`, `".join(sorted(EXCLUDED_REFERENCE_CLASSES))
            + "` are skipped entirely. `Debug.console` targets are reported as "
            "the Rust class `Console`. The `finalize` lifecycle hook is "
            "ignored on both sides."
        )
    else:
        lines.append(
            "No reference classes are excluded: `GdiPlus` and its ``ENUM`` "
            "constants are part of the reference surface and are checked like "
            "any other namespace. `Debug.console` targets are reported as the "
            "Rust class `Console`. The `finalize` lifecycle hook is ignored on "
            "both sides."
        )
    lines.append("")
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def _missing_structure_entries(
    reference: ReferenceModel, present: set[str]
) -> set[str]:
    """Allowlistable keys for a missing namespace or nested class.

    A reference ``NCB_REGISTER_CLASS`` namespace and every class attached to it
    with ``NCB_SUBCLASS`` are part of the surface, but an unimplemented one is
    a ratchet gap like a missing member, so it is keyed ``Class`` /
    ``Namespace.Name`` and handled by ``--update-allow``.  The wrong-level
    checks below stay hard.
    """
    missing: set[str] = set()
    for ns in reference.namespace_objects:
        if ns not in present:
            missing.add(ns)
    for ns, mapping in reference.nested_by_ns.items():
        for public in mapping:
            dotted = f"{ns}.{public}"
            if dotted not in present:
                missing.add(dotted)
    return missing


def _wrong_level_failures(
    reference: ReferenceModel,
    top_level: set[str],
    aliased: set[str],
) -> list[str]:
    """Hard failures for a class exposed at the wrong level.

    A class the reference only nests (``NCB_SUBCLASS``) must not be a bare
    global; a port that registered it globally and aliased it into the
    namespace (``aliased``) is correct.  This is the registration bug class
    the guard exists for, so it is never allowlistable.
    """
    failures: list[str] = []
    for ns in sorted(reference.nested_by_ns):
        for public, internal in sorted(reference.nested_by_ns[ns].items()):
            candidates = {
                public,
                internal,
                RUST_CLASS_ALIASES.get(internal, internal),
            }
            if any(c in aliased for c in candidates):
                continue
            bare = sorted(c for c in candidates if c in top_level)
            if bare:
                failures.append(
                    f"extra top-level class: `{bare[0]}` is reference-nested as "
                    f"`{ns}.{public}` but the port registers it as a global "
                    f"class"
                )
    return failures


def _port_only_class_failures(
    reference: ReferenceModel,
    plugin_classes: set[str],
    core_classes: set[str],
    aliased: set[str],
) -> list[str]:
    """Port-only top-level classes the reference never publishes.

    A class registered by a plugin file is a violation when the reference never
    publishes it: not through ``NCB_REGISTER_CLASS``/``NCB_ATTACH_*``, not as a
    namespace (``Motion``), not nested under one (``Motion.Point``), and not a
    TJS-script class (``WIN32Dialog``).  Core natives and classes the port
    correctly aliases into a namespace are excluded.
    """
    published = reference.published_top_level | reference.namespace_objects
    nested_names = {p for m in reference.nested_by_ns.values() for p in m}
    nested_internal = {i for m in reference.nested_by_ns.values() for i in m.values()}
    failures: list[str] = []
    for cls in sorted(plugin_classes):
        if cls in core_classes or cls in published:
            continue
        if cls in nested_names or cls in nested_internal:
            continue
        if cls in aliased or cls == GLOBAL_CLASS:
            continue
        failures.append(
            f"port-only top-level class: the reference never publishes `{cls}` "
            f"as a TJS class"
        )
    return failures


def self_test(verbose: bool = True) -> int:
    """Assert the reference extractor still sees each registration mechanism.

    ``scripts/parity_fixtures/plugin_surface.cpp`` exercises the ncbind macro
    blocks, class/namespace/global ``PropSet`` registrations and a script
    global, plus a runtime-built name that must be reported as unrecoverable.
    """
    fixture = FIXTURES_DIR / "plugin_surface.cpp"
    expected_path = FIXTURES_DIR / "plugin_surface.expected.json"
    for path in (fixture, expected_path):
        if not path.is_file():
            print(f"plugin_parity: self-test fixture missing: {path}")
            return 1
    expected = json.loads(expected_path.read_text(encoding="utf-8"))
    model = parse_reference([fixture])

    problems: list[str] = []

    def diff_map(label: str, got: dict, want: dict) -> None:
        for key in sorted(set(got) | set(want)):
            g = set(got.get(key, []))
            w = set(want.get(key, []))
            if g - w:
                problems.append(f"{label}[{key}]: unexpected {sorted(g - w)}")
            if w - g:
                problems.append(f"{label}[{key}]: missing {sorted(w - g)}")

    diff_map("classes", model.classes, expected["classes"])
    diff_map(
        "nested",
        {ns: set(mapping) for ns, mapping in model.nested_by_ns.items()},
        expected.get("nested", {}),
    )
    diff_map("propset", model.propset_members, expected.get("propset", {}))

    want_unrecoverable = expected.get("unrecoverable", [])
    if len(model.unrecoverable) != len(want_unrecoverable):
        problems.append(
            "unrecoverable sites: expected "
            f"{len(want_unrecoverable)}, got {len(model.unrecoverable)}: "
            f"{model.unrecoverable}"
        )
    for needle in want_unrecoverable:
        if not any(needle in site for site in model.unrecoverable):
            problems.append(f"unrecoverable sites: no site matching {needle!r}")

    if problems:
        print("plugin_parity: extractor self-test FAILED:")
        for problem in problems:
            print(f"  {problem}")
        print("  Fix the extractor; do not silence the self-test.")
        return 1
    total = sum(len(v) for v in model.classes.values())
    if verbose:
        print(
            "plugin_parity: extractor self-test ok "
            f"({total} members, {len(model.unrecoverable)} unrecoverable site(s))"
        )
    return 0


def _check_runtime(args: argparse.Namespace) -> int:
    """Compare the parsed reference plugin surface against a runtime dump.

    Report only: the dump is the ground truth for the whole reference surface,
    so missing members are printed and the process exits 0.  The namespace
    structure is checked too so a missing ``Namespace.name`` is visible.
    """
    runtime = np.load_runtime_surface(args.runtime)
    reference = parse_reference()
    ref = reference.classes
    # Reference members attached to an implementation class
    # (``layerExMovie`` -> ``LayerExMovie``): match them to the runtime class
    # with the same canonicalised name.
    for impl, impl_members in reference.impl_classes.items():
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

    top_level = {name for name in runtime if "." not in name}
    # The dump holds the live VM surface, so a core native such as `Rect` or
    # `Font` is a bare global even though the reference only reaches it as
    # `Motion.Rect`/`GdiPlus.Font`.  Restrict the top-level check to the
    # classes the port registered from plugin files, matching the source run.
    (
        _members,
        _sources,
        _plugin_only,
        _core_members,
        _groups,
        source_aliases,
        plugin_classes,
        core_classes,
    ) = parse_rust(reference.classes.get(GLOBAL_CLASS, set()))
    alias_visible = {
        f"{ns}.{public}"
        for public, _internal, _keeps in source_aliases
        for ns in reference.nested_by_ns
        if public in reference.nested_by_ns[ns]
    }
    structure_missing = _missing_structure_entries(
        reference, set(runtime) | alias_visible
    )
    if structure_missing:
        print(
            f"plugin_parity(runtime): {len(structure_missing)} namespace/nested "
            "class(es) missing (allowlistable):"
        )
        for entry in sorted(structure_missing):
            print(f"  - {entry}")
    wrong_level = _wrong_level_failures(
        reference, plugin_classes & top_level, alias_visible
    )
    wrong_level += _port_only_class_failures(
        reference,
        plugin_classes & set(runtime),
        core_classes,
        alias_visible,
    )
    if wrong_level:
        print(
            f"plugin_parity(runtime): {len(wrong_level)} wrong-level "
            "divergence(s):"
        )
        for line in wrong_level:
            print(f"  - {line}")
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
        "--self-test",
        action="store_true",
        help="run the extractor self-test against scripts/parity_fixtures and exit",
    )
    parser.add_argument(
        "--runtime",
        metavar="PATH",
        help="compare the parsed reference plugin surface against a "
        "runtime_surface.json dump instead of the Rust source (report only; "
        "does not touch the allowlist)",
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

    reference = parse_reference()
    ref = reference.classes
    impl_classes = reference.impl_classes
    (
        rust,
        rust_sources,
        plugin_only,
        core_members,
        dynamic_groups,
        aliases,
        plugin_classes,
        core_classes,
    ) = parse_rust(reference.classes.get(GLOBAL_CLASS, set()))

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

    # The reference attaches a subclass inside a namespace with ``NCB_SUBCLASS``;
    # it is script-visible only as ``Namespace.name``.  The port emulates that by
    # registering the class globally and then aliasing it into the namespace
    # object and deleting the global.  Move those members onto the dotted class
    # so ``Motion.Point`` matches and the bare global is not an extra.
    alias_visible: dict[str, str] = {}
    for public, internal, keeps_global in aliases:
        ns = next(
            (n for n, mapping in reference.nested_by_ns.items() if public in mapping),
            None,
        )
        if ns is None:
            continue
        dotted = f"{ns}.{public}"
        if keeps_global:
            # The reference attaches the class into the namespace with
            # ``PropSet`` *and* leaves the global in place; the port keeps the
            # global, so the dotted name must still count as present.
            alias_visible[public] = dotted
            continue
        internal_key = RUST_CLASS_ALIASES.get(internal, internal)
        moved = set(plugin_only.get(internal_key, ())) | set(
            plugin_only.get(public, ())
        )
        rust.setdefault(dotted, set()).update(moved)
        alias_visible[public] = dotted
        alias_visible[internal] = dotted
        alias_visible[internal_key] = dotted
        plugin_only.pop(internal_key, None)
        plugin_only.pop(public, None)
        if internal_key not in core_members:
            rust.pop(internal_key, None)
        if public not in core_members:
            rust.pop(public, None)

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

    # A missing namespace object or nested class is a ratchet gap, not a
    # wrong-level registration; key it like a member so ``--update-allow``
    # accepts it (``GdiPlus`` / ``GdiPlus.Image``).
    structure_missing = _missing_structure_entries(
        reference, set(rust) | set(alias_visible.values())
    )
    all_missing = {
        f"{cls}.{member}"
        for cls, missing in missing_by_class.items()
        for member in missing
    } | structure_missing

    allow = read_allowlist(ALLOWLIST_PATH)
    if args.update_allow:
        write_allowlist(
            ALLOWLIST_PATH, all_missing, read_allowlist_reasons(ALLOWLIST_PATH)
        )
        print(
            f"plugin_parity: wrote {len(all_missing)} known-missing entries to "
            f"{ALLOWLIST_PATH.relative_to(REPO_ROOT)}"
        )
        allow = set(all_missing)

    new_missing = sorted(all_missing - allow)
    stale = sorted(allow - all_missing)

    # Wrong-level guards stay hard and are not allowlistable: a nested class
    # may not be a bare global, and a plugin may not register a top-level class
    # the reference never publishes.
    top_level_plugin = set(plugin_classes) | set(plugin_only)
    wrong_level_failures = _wrong_level_failures(
        reference, top_level_plugin, set(alias_visible)
    )
    port_only_failures = _port_only_class_failures(
        reference, set(plugin_classes), set(core_classes), set(alias_visible)
    )

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
                structure_missing,
                wrong_level_failures,
                port_only_failures,
                reference.propset_members,
                reference.unrecoverable,
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

    if wrong_level_failures:
        print(
            f"plugin_parity: {len(wrong_level_failures)} wrong-level class "
            "failure(s) — these are the registration bug class:",
            file=sys.stderr,
        )
        for line in wrong_level_failures:
            print(f"  - {line}", file=sys.stderr)

    if port_only_failures:
        print(
            f"plugin_parity: {len(port_only_failures)} port-only top-level "
            "class(es) the reference never publishes:",
            file=sys.stderr,
        )
        for line in port_only_failures:
            print(f"  - {line}", file=sys.stderr)

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
    if wrong_level_failures or port_only_failures:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
