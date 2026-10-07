#!/usr/bin/env bash
# D9 guard: the bottom core must be stateless.
#
# Checks the new bottom crates for process-global mutable state, hidden
# caches and singletons. See docs/migration/02-target-architecture.md §9
# (acceptance criterion 3) and docs/migration/03-middle-layer-api.md §0.
#
# Usage: scripts/check-stateless.sh
# Exits non-zero (and prints the offending lines) if a forbidden construct
# is found. Intended for CI and for local pre-commit checks.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT" || exit 1

# Bottom-core crates. The middle layer (krkr-engine) is allowed to own state,
# so it is intentionally excluded.
CRATES=(
  "crates/krkr-api"
  "crates/krkr-fs"
  "crates/krkr-media"
  "crates/krkr-render"
)

# Forbidden patterns (extended regex, matched against .rs sources).
PATTERNS=(
  'static[[:space:]]+mut[[:space:]]'
  'thread_local!'
  'lazy_static!'
  'LazyLock'
  'OnceLock'
  'OnceCell'
)

fail=0

for crate in "${CRATES[@]}"; do
  [ -d "$crate/src" ] || continue
  for pat in "${PATTERNS[@]}"; do
    # -n line numbers, -E extended regex, skip nothing.
    while IFS= read -r line; do
      echo "D9 violation [$crate]: $line"
      fail=1
    done < <(grep -rnE "$pat" "$crate/src" --include='*.rs' 2>/dev/null || true)
  done
done

if [ "$fail" -ne 0 ]; then
  echo ""
  echo "ERROR: forbidden process-global state in the bottom core (D9)."
  echo "Move the state into an explicit value owned by the middle layer."
  exit 1
fi

echo "D9 guard: clean (no globals/caches in krkr-api/krkr-fs/krkr-media/krkr-render)"
