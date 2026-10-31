"""Shared paths for the gate checks.

The package lives at ``scripts/krkr_gates``; the project root is ``scripts``
and the repository root is discovered by walking up to the directory that
holds ``Cargo.toml`` and ``crates``.

The reference checkout is a separate clone, not part of this repository.
:func:`reference_root` resolves it — ``KRKR_REFERENCE`` (which ``krkr-gate
--reference PATH`` sets), then ``<repo>/reference`` — and stops with the paths
it tried when neither holds a ``cpp`` directory, because a scan that quietly
finds no reference source shrinks the covered surface instead of failing.
"""

from __future__ import annotations

import os
import re
from functools import lru_cache
from pathlib import Path

PACKAGE_DIR = Path(__file__).resolve().parent
SCRIPTS_DIR = PACKAGE_DIR.parent
FIXTURES_DIR = SCRIPTS_DIR / "fixtures"
ALLOWLISTS_DIR = SCRIPTS_DIR / "allowlists"


def find_repo_root(start: Path) -> Path:
    """Return the repository root that contains ``Cargo.toml`` and ``crates``."""
    for candidate in (start, *start.parents):
        if (candidate / "Cargo.toml").is_file() and (candidate / "crates").is_dir():
            return candidate
    raise SystemExit(
        "krkr-gate: cannot locate the repository root (no Cargo.toml/crates)"
    )


REPO_ROOT = find_repo_root(PACKAGE_DIR)

#: The option and the environment variable that name the reference checkout.
REFERENCE_FLAG = "--reference"
REFERENCE_ENV = "KRKR_REFERENCE"
#: Where the reference lives unless it is named explicitly.
DEFAULT_REFERENCE_DIR = REPO_ROOT / "reference"
#: Suffixes the reference-source scans read.
REFERENCE_SUFFIXES = (".h", ".hpp", ".cpp")


def reference_root() -> Path:
    """Return the reference checkout root: the clone that holds ``cpp/``."""
    named = os.environ.get(REFERENCE_ENV)
    tried: list[str] = []
    candidates: list[Path] = []
    if named:
        candidates.append(Path(named).expanduser())
        tried.append(f"{REFERENCE_ENV}={named}")
    candidates.append(DEFAULT_REFERENCE_DIR)
    tried.append(f"the default location {DEFAULT_REFERENCE_DIR}")
    for root in candidates:
        if (root / "cpp").is_dir():
            return root.resolve()
    raise SystemExit(
        "krkr-gate: no reference checkout found (a clone holding a cpp/ "
        f"directory); tried {', '.join(tried)}. "
        f"Pass {REFERENCE_FLAG} PATH or set {REFERENCE_ENV}."
    )


def reference_citation(path: Path, root: Path) -> str:
    """Render a reference file as ``reference/cpp/...`` wherever the clone sits.

    The generated reports and the design record cite the default layout, so a
    citation must not depend on where the checkout actually lives.
    """
    try:
        return f"reference/{path.resolve().relative_to(root).as_posix()}"
    except ValueError:
        return str(path)


@lru_cache(maxsize=None)
def reference_defined_macros(root: Path) -> frozenset[str]:
    """The preprocessor flags the reference both defines and gates code on.

    A member behind ``#ifdef FLAG`` is part of the shipped surface when the
    reference defines ``FLAG`` — its own window interface header defines
    ``USE_OBSOLETE_FUNCTIONS` — while a flag it never defines is a build option
    and its members are not part of the surface.  Collect the identifiers used
    in preprocessor conditions and keep the ones some ``#define`` names, so an
    unrelated internal macro is ignored.
    """
    define_re = re.compile(r"^\s*#\s*define\s+([A-Za-z_]\w*)")
    cond_re = re.compile(r"^\s*#\s*(?:if|ifdef|ifndef|elif)\b(.*)$")
    ident_re = re.compile(r"[A-Za-z_]\w*")
    defined_any: set[str] = set()
    used_in_condition: set[str] = set()
    for path in (root / "cpp").rglob("*"):
        if path.suffix not in REFERENCE_SUFFIXES or not path.is_file():
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        for line in text.splitlines():
            match = define_re.match(line)
            if match:
                defined_any.add(match.group(1))
            condition = cond_re.match(line)
            if condition:
                used_in_condition.update(ident_re.findall(condition.group(1)))
    return frozenset(defined_any & used_in_condition)

def reference_core_sources(root: Path | None = None) -> list[Path]:
    """The reference's core C++ sources, in path order."""
    root = root if root is not None else reference_root()
    files = sorted((root / "cpp" / "core").rglob("*.cpp"))
    if not files:
        raise SystemExit(
            f"krkr-gate: no reference core sources under {root / 'cpp' / 'core'} — "
            "the checkout is incomplete; point --reference at the clone root."
        )
    return files


def reference_source_label(path: Path, root: Path | None = None) -> str:
    """The name a report cites a scanned source by.

    A reference file is cited as ``reference/cpp/...`` wherever its checkout
    lives; anything else is cited from the repository root.
    """
    citation = reference_citation(path, root if root is not None else reference_root())
    if citation.startswith("reference/"):
        return citation
    try:
        return path.relative_to(REPO_ROOT).as_posix()
    except ValueError:
        return str(path)
