"""Shared paths for the gate checks.

The package lives at ``scripts/krkr_gates``; the project root is ``scripts``
and the repository root is discovered by walking up to the directory that
holds ``Cargo.toml`` and ``crates``.
"""

from __future__ import annotations

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
