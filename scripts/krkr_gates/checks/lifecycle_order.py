"""Lifecycle-order guard for native Scene ownership.

A native object that owns Scene records must finalize its child script
objects *before* it removes those records. A child finalizer may write a
native property (``cursor`` is the usual one); once its record is gone that
write raises ``layer no longer exists`` and aborts the parent's teardown.

The guard checks the two ownership sites in ``crates/compat/src/natives``:

* a ``scene.remove_window``/``scene.invalidate_window`` call must be preceded
  in the same function by ``invalidate_window_layers`` (the helper finalizes
  every layer script object while the records are live);
* a ``scene.invalidate_layer`` call must be preceded by
  ``invalidate_retained`` (the child-finalization pass).

``layer_destroy`` and the ``layer_create`` failure cleanup are exempt: the
former is the native destructor after ``_Finalize`` already invalidated the
children, and the latter has no joined children yet.
"""

from __future__ import annotations

import re
import sys

from ..common import FIXTURES_DIR, REPO_ROOT
from .error_handling import mask

ROOT = REPO_ROOT
NATIVES = ROOT / "crates" / "compat" / "src" / "natives"
FIXTURES = FIXTURES_DIR / "lifecycle_order"
BAD_WINDOW = "scripts/fixtures/lifecycle_order/bad_window.rs"
GOOD_WINDOW = "scripts/fixtures/lifecycle_order/good_window.rs"
BAD_LAYER = "scripts/fixtures/lifecycle_order/bad_layer.rs"
GOOD_LAYER = "scripts/fixtures/lifecycle_order/good_layer.rs"
RULE = "scene-lifecycle-order"

WINDOW_CALL = re.compile(r"\bscene\.(remove_window|invalidate_window)\s*\(")
LAYER_CALL = re.compile(r"\bscene\.(?:invalidate_layer|remove_layer)\s*\(")
FN = re.compile(r"\bfn\s+(\w+)\s*\([^)]*\)[^{]*\{")

# These are safe by construction; see the module doc.
SAFE_LAYER_FUNCTIONS = {"layer_create", "layer_ctor", "layer_destroy"}


def _block(masked: str, open_brace: int) -> tuple[int, int]:
    """Return the span (body_start, body_end) of the block at ``open_brace``."""
    depth = 1
    i = open_brace + 1
    while i < len(masked) and depth:
        if masked[i] == "{":
            depth += 1
        elif masked[i] == "}":
            depth -= 1
        i += 1
    return open_brace + 1, i - 1


def _enclosing_function(masked: str, offset: int) -> tuple[str, int, int] | None:
    """Find the innermost ``fn`` whose body contains ``offset``."""
    best: tuple[str, int, int] | None = None
    for match in FN.finditer(masked):
        body_start, body_end = _block(masked, match.end() - 1)
        if body_start <= offset < body_end:
            best = (match.group(1), body_start, body_end)
    return best


def scan(roots=None):
    """Return ``(rel_path, line, rule, detail)`` findings."""
    if roots is None:
        roots = [NATIVES]
    findings = []
    for root in roots:
        paths = [root] if root.is_file() else sorted(root.rglob("*.rs"))
        for path in paths:
            try:
                rel = path.relative_to(ROOT).as_posix()
            except ValueError:
                rel = path.as_posix()
            source = path.read_text(encoding="utf-8")
            masked = mask(source)
            for pattern, window in ((WINDOW_CALL, True), (LAYER_CALL, False)):
                for call in pattern.finditer(masked):
                    enclosing = _enclosing_function(masked, call.start())
                    if enclosing is None:
                        continue
                    name, start, _end = enclosing
                    if not window and name in SAFE_LAYER_FUNCTIONS:
                        continue
                    before = masked[start:call.start()]
                    required = (
                        "invalidate_window_layers"
                        if window
                        else "invalidate_retained"
                    )
                    if required not in before:
                        line = masked[: call.start()].count("\n") + 1
                        findings.append(
                            (rel, line, RULE, f"{name}: missing {required}")
                        )
    return findings


def self_test() -> int:
    if not FIXTURES.is_dir():
        print(f"ERROR: lifecycle-order self-test fixtures missing at {FIXTURES}")
        return 1
    findings = scan([FIXTURES])
    by_path = {}
    for finding in findings:
        by_path.setdefault(finding[0], []).append(finding)
    missing = [
        path
        for path in (BAD_WINDOW, BAD_LAYER)
        if not by_path.get(path)
    ]
    unexpected = [
        path
        for path in (GOOD_WINDOW, GOOD_LAYER)
        if by_path.get(path)
    ]
    if missing:
        print(
            "ERROR: lifecycle-order self-test: bad fixture(s) matched nothing: "
            + ", ".join(missing)
        )
        return 1
    if unexpected:
        print(
            "ERROR: lifecycle-order self-test: good fixture(s) matched: "
            + ", ".join(unexpected)
        )
        return 1
    return 0


def main() -> int:
    code = self_test()
    if code:
        return code
    findings = scan()
    for rel, line, rule, detail in findings:
        print(f"ERROR: scene lifecycle violation [{rule}] {rel}:{line}: {detail}")
    if findings:
        print("")
        print(
            f"ERROR: {len(findings)} scene-removal site(s) run before child "
            "finalization."
        )
        print("  Finalize the child script objects first (invalidate_window_layers")
        print("  for a window, invalidate_retained for a layer) and only then")
        print("  remove their Scene records.")
        return 1
    print("lifecycle-order guard: clean")
    return 0


def run(argv=None) -> int:
    del argv
    return main()


if __name__ == "__main__":
    sys.exit(run(sys.argv[1:]))
