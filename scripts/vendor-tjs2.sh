#!/usr/bin/env bash
# Re-vendor the C++ TJS2 core from the reference checkout into
# crates/tjs2-sys/cpp/tjs2, then re-apply every krkr-rs patch.
#
# The patches live in `scripts/patches/tjs2-krkr-rs.patch` (a unified diff
# against the reference tree) rather than as ad-hoc edits here, so they are
# reviewable in git and cannot be silently lost when upstream is re-vendored.
# **Any new change to `crates/tjs2-sys/cpp/tjs2/**` must be added to that
# patch file**, otherwise this script will re-copy the pristine upstream and
# the change will disappear.
#
# To regenerate the patch after editing the vendored sources:
#   rm -rf /tmp/vend && mkdir -p /tmp/vend
#   cp -r reference/cpp/core/tjs2 /tmp/vend/a
#   cp -r crates/tjs2-sys/cpp/tjs2 /tmp/vend/b
#   rm -f /tmp/vend/{a,b}/CMakeLists.txt /tmp/vend/b/LICENSE.krkr2
#   (cd /tmp/vend && diff -ruN a b) \
#     | sed -E 's/^(---|\+\+\+) ([ab]\/[^\t]+)\t.*/\1 \2/' \
#     > scripts/patches/tjs2-krkr-rs.patch
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REF="$ROOT/reference/cpp/core/tjs2"
DEST="$ROOT/crates/tjs2-sys/cpp/tjs2"
PATCH="$ROOT/scripts/patches/tjs2-krkr-rs.patch"

if [[ ! -d "$REF" ]]; then
    echo "error: reference checkout not found at $REF" >&2
    echo "hint:  git clone --depth 1 https://github.com/2468785842/krkr2.git reference" >&2
    exit 1
fi
if [[ ! -f "$PATCH" ]]; then
    echo "error: patch file not found at $PATCH" >&2
    exit 1
fi

rm -rf "$DEST"
cp -r "$REF" "$DEST"
cp "$ROOT/reference/LICENSE" "$DEST/LICENSE.krkr2"
rm -f "$DEST/CMakeLists.txt"

# `-p1` strips the `a/`/`b/` prefixes; `--forward` makes an already-applied
# hunk a warning rather than a failure, `--fuzz=0` keeps it strict.
patch -p1 --forward --fuzz=0 -d "$DEST" < "$PATCH"

# Fail loudly if a patch marker is missing: a silent re-vendor that drops a
# krkr-rs fix is exactly the failure this script exists to prevent.
missing=0
for marker in \
    "krkr-rs patch" \
    "TJSVariantStringChars" \
    "TJSVariantStringLength"
do
    if ! grep -rqF "$marker" "$DEST"; then
        echo "error: patch marker '$marker' missing after applying $PATCH" >&2
        missing=1
    fi
done
if [[ "$missing" != 0 ]]; then
    exit 1
fi

echo "vendored tjs2 -> $DEST (patched with $(basename "$PATCH"))"
