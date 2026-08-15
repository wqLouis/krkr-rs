#!/usr/bin/env bash
# Re-vendor the C++ TJS2 core from the reference checkout into
# crates/tjs2-sys/cpp/tjs2, then re-apply the krkr-rs patches.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REF="$ROOT/reference/cpp/core/tjs2"
DEST="$ROOT/crates/tjs2-sys/cpp/tjs2"

if [[ ! -d "$REF" ]]; then
    echo "error: reference checkout not found at $REF" >&2
    echo "hint:  git clone --depth 1 https://github.com/2468785842/krkr2.git reference" >&2
    exit 1
fi

rm -rf "$DEST"
cp -r "$REF" "$DEST"
cp "$ROOT/reference/LICENSE" "$DEST/LICENSE.krkr2"
rm -f "$DEST/CMakeLists.txt"

# --- krkr-rs patches ---------------------------------------------------------
# 1. parser::error: throw a TJS error instead of only logging + error
#    recovery (which leaves half-parsed blocks that crash at execution).
python3 - "$DEST/tjsInterCodeGen.cpp" << 'PYEOF'
import sys
path = sys.argv[1]
src = open(path, encoding="utf-8").read()
old = """    void parser::error(const std::string &msg) {
        spdlog::get("tjs2")->critical(msg);
    }"""
new = """    void parser::error(const std::string &msg) {
        spdlog::get("tjs2")->critical(msg);
        // krkr-rs patch: the upstream code only logs and relies on the
        // grammar's `error ";"` recovery, which can leave the script block
        // half-parsed and crash later at execution. Abort the parse with a
        // proper TJS error instead.
        TJS_eTJSScriptError(ttstr(msg.c_str()).c_str(), ptr, -1);
    }"""
assert old in src, "parser::error patch anchor not found (upstream changed?)"
open(path, "w", encoding="utf-8").write(src.replace(old, new))
print("patched:", path)
PYEOF

# 2. zero the VM register area on function entry: the exception-display
#    register dump reads ALL allocated slots, including ones the VM never
#    wrote; the stack allocator reuses memory, so those slots are
#    uninitialized and reading them is UB (crashes under clang -O2, which
#    eliminates tTJSVariantString's `if(!this)` guard).
python3 - "$DEST/tjsInterCodeExec.cpp" << 'PYEOF'
import sys
path = sys.argv[1]
src = open(path, encoding="utf-8").read()
old = """            tTJSVariant *regs = TJSVariantArrayStack->Allocate(num_alloc);
            tTJSVariant *ra =
                regs + MaxVariableCount + VariableReserveCount; // register area"""
new = """            tTJSVariant *regs = TJSVariantArrayStack->Allocate(num_alloc);
            // krkr-rs patch: the exception-display register dump reads ALL
            // num_alloc slots, including ones the VM never wrote; the
            // allocator reuses memory, so those slots are uninitialized and
            // reading them is UB (crashes under clang -O2, which eliminates
            // tTJSVariantString's `if(!this)` guard). Zero them so the dump
            // always sees tvtVoid slots.
            std::memset(regs, 0, sizeof(tTJSVariant) * num_alloc);
            tTJSVariant *ra =
                regs + MaxVariableCount + VariableReserveCount; // register area"""
assert old in src, "register-zero patch anchor not found (upstream changed?)"
open(path, "w", encoding="utf-8").write(src.replace(old, new))
print("patched:", path)
PYEOF

echo "vendored tjs2 -> $DEST"
