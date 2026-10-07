#!/usr/bin/env bash
# D9 guard: the bottom core must be stateless.
#
# Checks the new bottom crates for process-global mutable state, hidden
# caches and singletons, and checks the L1 service objects for stateful
# (`&mut self`) methods. See docs/migration/02-target-architecture.md §9
# (acceptance criterion 3) and docs/migration/03-middle-layer-api.md §9.
#
# Usage: scripts/check-stateless.sh
# Exits non-zero (and prints the offending lines) if a forbidden construct
# is found. Intended for CI and for local pre-commit checks.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT" || exit 1

# Bottom-core crates. The middle layer (krkr-engine) is allowed to own state,
# so it is intentionally excluded. krkr-platform is L1 and must be stateless
# too: only its *values* (the frame pacer, the clock) carry state, never a
# process-global.
CRATES=(
  "crates/krkr-api"
  "crates/krkr-fs"
  "crates/krkr-media"
  "crates/krkr-render"
  "crates/krkr-platform"
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

# L1 service objects must be immutable after construction: a public method
# that takes `&mut self` means the object remembers state (03 §9). Stateful
# *value* types (RenderScene, the platform `Ticker`, `InputState`, …) are
# deliberately not services and are not listed here.
SERVICES=(
  "crates/krkr-render:RenderPipeline"
  "crates/krkr-fs:FileSystem"
  "crates/krkr-media:Media"
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

# Stateful-service check: no `pub fn ...(&mut self)` inside the service type's
# `impl` blocks. The awk tracks the current `impl` header so unrelated
# stateful types in the same crate do not trip the guard.
for service in "${SERVICES[@]}"; do
  crate="${service%%:*}"
  type="${service##*:}"
  [ -d "$crate/src" ] || continue
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    echo "D9 violation [$crate]: stateful service method on $type: $line"
    fail=1
  done < <(
    find "$crate/src" -name '*.rs' -print0 2>/dev/null |
      xargs -0 awk -v type="$type" '
        /^[[:space:]]*impl[[:space:]]/ {
          in_impl = (index($0, type) > 0)
          next
        }
        in_impl && /pub fn[[:space:]]+[A-Za-z_0-9]+[[:space:]]*\(&mut self/ {
          print FILENAME ":" FNR ":" $0
        }
        in_impl && /^}/ { in_impl = 0 }
      ' 2>/dev/null || true
  )
done

if [ "$fail" -ne 0 ]; then
  echo ""
  echo "ERROR: forbidden process-global state in the bottom core (D9)."
  echo "Move the state into an explicit value owned by the middle layer."
  exit 1
fi

echo "D9 guard: clean (no globals/caches, no stateful services in the bottom core)"
