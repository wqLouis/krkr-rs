#!/usr/bin/env python3
"""Scene-record ownership guard.

A Scene record belongs to the script object that owns it. A layer's native
state lives as long as the layer's **script object** — not as long as its
parent layer's and not as long as its window's. A native teardown must
therefore neither remove nor invalidate a record that belongs to a *different*
script object.

The reference:

* `tTJSNI_BaseLayer::Invalidate`
  (`reference/cpp/core/visual/LayerIntf.cpp:515-579`) severs its direct
  children with `child->Part()` (`:543-549`); `Part()` (`:624-631`) is a
  pure detach, so a child keeps its native layer, its script object and its
  validity until that child is finalized on its own. The layer's teardown
  never invalidates a child script object.
* `tTJSNI_BaseWindow::Invalidate`
  (`reference/cpp/core/visual/WindowIntf.cpp:175-241`) invalidates only the
  objects the script registered through `Window.add` (the `ObjectVector`,
  `:208-222`); the primary-layer detach is commented out (`:186-187`), so
  the window's teardown never reaches into the layers.
* The VM runs a script object's `finalize` before its native `Invalidate`
  (`reference/cpp/core/tjs2/tjsObject.cpp:389-405`), so a layer's finalizer
  always runs while its own record is alive. A teardown that removed a
  foreign record first would make that finalizer raise
  `layer no longer exists`.

This check enforces four rules:

* **R1** `remove_window` (`crates/engine/src/scene.rs`) must not call
  `destroy_layer` or `remove_layer`: dropping a window record must not
  touch a layer record, because that record belongs to the layer's own script
  object.
* **R2** `invalidate_layer` (`crates/engine/src/scene.rs`) must not call
  `destroy_layer`: invalidating a layer detaches it (`remove_layer`, its
  own record) and severs its children, whose records stay alive.
* **R3** `layer_invalidate`
  (`crates/compat/src/natives/visual/layer/mod.rs`) must not call
  `invalidate_retained`: a layer's teardown must not invalidate another
  script object.
* **R4** the window module
  (`crates/compat/src/natives/visual/window.rs`) must not reference the
  layer registry `objects.layers`: the window's teardown must not reach into
  the layers at all.

The fixture self-test proves that every rule can fail: each rule has a
`bad_*` fixture that must be reported and a `good_*` fixture that must stay
clean. The scan is deliberately lexical: it follows the named functions in the
scanned files and does not resolve helper calls, types or aliases.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

from ..common import FIXTURES_DIR, REPO_ROOT
from ..framework import (
    FixtureRules,
    Finding,
    dedupe_findings,
    find_block,
    fixture_problems,
    mask,
)

ROOT = REPO_ROOT
VISUAL = ROOT / "crates" / "compat" / "src" / "natives" / "visual"
SCENE = ROOT / "crates" / "engine" / "src" / "scene.rs"
FIXTURES = FIXTURES_DIR / "lifecycle_order"

ENGINE_SCENE = "crates/engine/src/scene.rs"
LAYER_MODULE_DIR = "crates/compat/src/natives/visual/layer"
WINDOW_MODULE = "crates/compat/src/natives/visual/window.rs"
FIXTURE_PREFIX = "scripts/fixtures/lifecycle_order/"

RULE = "scene-record-ownership"
RULE_WINDOW_RECORDS = "window-removes-no-layer-record"
RULE_LAYER_SUBTREE = "layer-invalidate-destroys-no-subtree"
RULE_LAYER_TEARDOWN = "layer-teardown-invalidates-no-child"
RULE_WINDOW_TEARDOWN = "window-teardown-reaches-no-layer"

BAD_REMOVE_WINDOW = "scripts/fixtures/lifecycle_order/bad_remove_window.rs"
GOOD_REMOVE_WINDOW = "scripts/fixtures/lifecycle_order/good_remove_window.rs"
BAD_INVALIDATE_LAYER = "scripts/fixtures/lifecycle_order/bad_invalidate_layer.rs"
GOOD_INVALIDATE_LAYER = "scripts/fixtures/lifecycle_order/good_invalidate_layer.rs"
BAD_LAYER_INVALIDATE = "scripts/fixtures/lifecycle_order/bad_layer_invalidate.rs"
GOOD_LAYER_INVALIDATE = "scripts/fixtures/lifecycle_order/good_layer_invalidate.rs"
BAD_WINDOW_TEARDOWN = "scripts/fixtures/lifecycle_order/bad_window_teardown.rs"
GOOD_WINDOW_TEARDOWN = "scripts/fixtures/lifecycle_order/good_window_teardown.rs"

# A Rust function definition: the parameter list of the functions under
# inspection holds no nested parentheses, and the return type (when there is
# one) contains no brace.
FN = re.compile(r"\bfn\s+(\w+)\s*\([^)]*\)[^{]*\{")

# A call the rules forbid, per function.
FORBIDDEN_CALLS = {
    "remove_window": ("destroy_layer", "remove_layer"),
    "invalidate_layer": ("destroy_layer",),
    "layer_invalidate": ("invalidate_retained",),
}

# The layer registry the window module must not reach into.
LAYER_REGISTRY = re.compile(r"\bobjects\s*\.\s*layers\b")


def _line(masked: str, pos: int) -> int:
    return masked[:pos].count("\n") + 1


def _functions(masked: str) -> list[tuple[str, int, int]]:
    """(name, body start, body end) for every function definition."""
    bodies: list[tuple[str, int, int]] = []
    for match in FN.finditer(masked):
        span = find_block(masked, match.end() - 1)
        if span is None:
            continue
        bodies.append((match.group(1), span[0], span[1]))
    return bodies


def _calls(body: str, name: str) -> list[int]:
    """Offsets of every `name(...)` call inside a function body."""
    return [match.start() for match in re.finditer(rf"\b{re.escape(name)}\s*\(", body)]


def _is_fixture(rel: str) -> bool:
    return rel.startswith(FIXTURE_PREFIX)


def _in_layer_module(rel: str) -> bool:
    return rel.startswith(LAYER_MODULE_DIR + "/")


def scan_text(rel: str, source: str) -> list[Finding]:
    """Return every scene-record ownership violation in one Rust source."""
    masked = mask(source)
    fixture = _is_fixture(rel)
    findings: list[Finding] = []

    for name, start, end in _functions(masked):
        forbidden = FORBIDDEN_CALLS.get(name)
        if forbidden is None:
            continue
        # R1 and R2 belong to the engine scene, R3 to the compat layer
        # module. Inside the fixture directory every rule applies to every
        # file, so the self-test can prove that each of them can fail.
        if not fixture:
            if name in ("remove_window", "invalidate_layer"):
                if rel != ENGINE_SCENE:
                    continue
            elif not _in_layer_module(rel):
                continue
        body = masked[start:end]
        for call in forbidden:
            for offset in _calls(body, call):
                pos = start + offset
                rule = {
                    "remove_window": RULE_WINDOW_RECORDS,
                    "invalidate_layer": RULE_LAYER_SUBTREE,
                    "layer_invalidate": RULE_LAYER_TEARDOWN,
                }[name]
                findings.append(
                    Finding(
                        rel,
                        _line(masked, pos),
                        rule,
                        anchor=call,
                        message=(
                            f"{name} must not call {call}: a Scene record "
                            "belongs to the script object that owns it"
                        ),
                    )
                )

    # R4: the window module must not reach into the layer registry.
    if fixture or rel == WINDOW_MODULE:
        for match in LAYER_REGISTRY.finditer(masked):
            findings.append(
                Finding(
                    rel,
                    _line(masked, match.start()),
                    RULE_WINDOW_TEARDOWN,
                    anchor=match.group(0),
                    message=(
                        "the window module must not reach into the layer "
                        "registry: the window's teardown invalidates only the "
                        "Window.add registry"
                    ),
                )
            )

    return dedupe_findings(findings)


def scan(roots=None) -> list[Finding]:
    """Scan the engine scene and the compat visual layer/window modules."""
    if roots is None:
        roots = [SCENE, VISUAL]
    findings: list[Finding] = []
    for root in roots:
        paths = [root] if root.is_file() else sorted(root.rglob("*.rs"))
        for path in paths:
            try:
                rel = path.relative_to(ROOT).as_posix()
            except ValueError:
                rel = path.as_posix()
            source = path.read_text(encoding="utf-8")
            findings.extend(scan_text(rel, source))
    return dedupe_findings(findings)


def self_test() -> int:
    if not FIXTURES.is_dir():
        print(f"ERROR: lifecycle-order self-test fixtures missing at {FIXTURES}")
        return 1
    findings = scan([FIXTURES])
    problems = fixture_problems(
        findings,
        [
            FixtureRules(
                BAD_REMOVE_WINDOW,
                (RULE_WINDOW_RECORDS,),
                (GOOD_REMOVE_WINDOW,),
            ),
            FixtureRules(
                BAD_INVALIDATE_LAYER,
                (RULE_LAYER_SUBTREE,),
                (GOOD_INVALIDATE_LAYER,),
            ),
            FixtureRules(
                BAD_LAYER_INVALIDATE,
                (RULE_LAYER_TEARDOWN,),
                (GOOD_LAYER_INVALIDATE,),
            ),
            FixtureRules(
                BAD_WINDOW_TEARDOWN,
                (RULE_WINDOW_TEARDOWN,),
                (GOOD_WINDOW_TEARDOWN,),
            ),
        ],
    )
    if problems:
        print("ERROR: lifecycle-order self-test:")
        for problem in problems:
            print(f"  {problem}")
        print("  Fix the rule that stopped matching; do not silence the self-test.")
        return 1
    return 0


def main(argv=None) -> int:
    del argv
    code = self_test()
    if code:
        return code
    findings = scan()
    for finding in findings:
        print(
            f"ERROR: scene record ownership violation [{finding.rule}] "
            f"{finding.path}:{finding.line}: {finding.message}"
        )
    if findings:
        print("")
        print(f"ERROR: {len(findings)} scene-record ownership violation(s).")
        print("  A Scene record belongs to the script object that owns it: a")
        print("  native teardown must not remove or invalidate the record of a")
        print("  different script object.")
        return 1
    print("lifecycle-order guard: clean")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
