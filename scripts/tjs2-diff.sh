#!/usr/bin/env bash
# Print what the C++ TJS2 tree under crates/tjs/cpp/tjs2/ has changed
# against the reference checkout.
#
# The tree is maintained in place: the VM is edited there directly and is
# never regenerated from the reference, so this script only reads. It needs
# the reference checkout (AGENTS.md 1.3) and prints an empty diff when the
# tree matches it.
#
# Usage: tjs2-diff.sh [REFERENCE_ROOT]
#   REFERENCE_ROOT is the root of the reference checkout, the directory
#   that holds cpp/core/tjs2. It defaults to ./reference, searching one
#   level down as well.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$ROOT/crates/tjs/cpp/tjs2"
REF_ROOT="${1:-$ROOT/reference}"

REF=""
for candidate in "$REF_ROOT/cpp/core/tjs2" "$REF_ROOT"/*/cpp/core/tjs2; do
    if [[ -d "$candidate" ]]; then
        REF="$candidate"
        break
    fi
done

if [[ -z "$REF" ]]; then
    echo "error: no reference TJS2 tree found under $REF_ROOT" >&2
    echo "hint:  git clone --depth 1 https://github.com/2468785842/krkr2.git reference" >&2
    exit 1
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cp -r "$REF" "$tmp/a"
cp -r "$DEST" "$tmp/b"
rm -f "$tmp/a/CMakeLists.txt" "$tmp/b/CMakeLists.txt" "$tmp/b/LICENSE.krkr2"

# diff exits 1 when the trees differ, which is the normal case here.
diff -ruN "$tmp/a" "$tmp/b" || true
