#!/usr/bin/env bash
# Build the krkr-rs engine for Android.
#
# Everything Android-specific lives here (or in this crate) on purpose: the
# engine crates stay platform-neutral, and the only thing they need from us is
# a toolchain in the standard environment variables.
#
# ## Why not `cargo ndk`?
#
# `cargo-ndk` wires up `CC`/`AR`/the Rust linker, but it does **not** provide
# `CXX` in the form `crates/tjs2-sys/build.rs` reads (it sets the target-scoped
# `CXX_<triple>`, while the build script reads plain `CXX`). The result is worse
# than a failure: `build.rs` falls back to its default `zig c++`, which compiles
# for the **host**, so an Android build silently contains x86-64 objects. That
# was verified — `cargo ndk -t arm64-v8a build` produced
# `out/obj/tjsInterCodeExec.o: ELF 64-bit … x86-64`. `cargo check` does not
# link, so it looks like it worked.
#
# So we set the variables explicitly and use plain `cargo build --target`. The
# toolchain must be addressed by its *triple-prefixed* driver name, which is
# what tells clang to produce Android code — `build.rs` passes no `--target`.
#
# ## Usage
#
#   ./crates/android/build-android.sh [--ndk <path>] [--check]
#
# `ANDROID_NDK_HOME` is honoured if set; otherwise pass `--ndk`. `--check` runs
# `cargo check` instead of a full build (much faster, no linking).
set -euo pipefail

# --- configuration ----------------------------------------------------------

# Only arm64-v8a is supported, by decision (docs/android.md §1): there is no
# x86_64 build for emulators. Adding an ABI here means adding it in three
# places — here, `abiFilters` in android/app/build.gradle.kts, and the jniLibs
# directory the Gradle build picks up.
ABI="arm64-v8a"
RUST_TARGET="aarch64-linux-android"
# Android API level for the native code. Must be a level the NDK ships a sysroot
# for, and it is baked into the C/C++ objects, so changing it changes the build.
#
# 35 = Android 15, which is the app's `minSdk` (android/app/build.gradle.kts) and
# the highest level NDK r27c provides a sysroot for (its `usr/lib/
# aarch64-linux-android/` tops out at 35). Targeting a newer level therefore
# means moving to a newer NDK as well — they must be raised together.
API_LEVEL=35

CRATE="krkr-android"
# The cdylib name is fixed by `android.app.lib_name` in the manifest.
LIB_NAME="libkrkr_android.so"

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
JNI_LIBS="$ROOT/android/app/src/main/jniLibs/$ABI"

NDK="${ANDROID_NDK_HOME:-}"
MODE="build"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --ndk) NDK="$2"; shift 2 ;;
        --ndk=*) NDK="${1#--ndk=}"; shift ;;
        --check) MODE="check"; shift ;;
        -h|--help) sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "build-android.sh: unknown argument '$1' (try --help)" >&2; exit 2 ;;
    esac
done

if [[ -z "$NDK" ]]; then
    echo "error: no NDK given." >&2
    echo "  set ANDROID_NDK_HOME, or pass --ndk <path>" >&2
    echo "  (NDK r27c was the verified revision: https://dl.google.com/android/repository/android-ndk-r27c-linux.zip)" >&2
    exit 1
fi

HOST_TAG="linux-x86_64"
if [[ "$(uname -s)" == "Darwin" ]]; then HOST_TAG="darwin-x86_64"; fi
TOOLCHAIN="$NDK/toolchains/llvm/prebuilt/$HOST_TAG"
if [[ ! -d "$TOOLCHAIN" ]]; then
    echo "error: no toolchain at $TOOLCHAIN — is --ndk pointing at an NDK root?" >&2
    exit 1
fi
BIN="$TOOLCHAIN/bin"

# --- toolchain --------------------------------------------------------------
#
# CC/CXX/AR are what the C/C++ build scripts read; the linker variable is how
# rustc links a target that has no system linker of its own.
export CC="$BIN/aarch64-linux-android${API_LEVEL}-clang"
export CXX="$BIN/aarch64-linux-android${API_LEVEL}-clang++"
export AR="$BIN/llvm-ar"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$CC"

# opusic-sys's build script switches to the NDK's cmake toolchain when it sees
# this; without it the Opus build fails with "Failed to find tool".
export ANDROID_NDK_HOME="$NDK"

for tool in "$CC" "$CXX" "$AR"; do
    if [[ ! -x "$tool" ]]; then
        echo "error: missing toolchain binary: $tool" >&2
        exit 1
    fi
done

echo "krkr-rs android: abi=$ABI target=$RUST_TARGET api=$API_LEVEL"
echo "krkr-rs android: ndk=$NDK"

# --- build ------------------------------------------------------------------
#
# No FFmpeg: Android has no system FFmpeg, and this crate depends on `render`
# with `default-features = false` (Cargo.toml), which drops the `ffmpeg`
# feature transitively. The engine's MediaCodec backend is the replacement
# (docs/android.md §6); until it lands, video returns a clear "no MPEG decoder
# in this build" error rather than failing to compile.
cd "$ROOT"
cargo "$MODE" --target "$RUST_TARGET" -p "$CRATE"

if [[ "$MODE" == "check" ]]; then
    echo "krkr-rs android: check complete (no .so produced)"
    exit 0
fi

# --- verify and stage the shared library ------------------------------------

SO="$ROOT/target/$RUST_TARGET/debug/$LIB_NAME"
if [[ ! -f "$SO" ]]; then
    echo "error: expected the engine at $SO but it is not there" >&2
    exit 1
fi

# Guard against the silent-miscompile trap described at the top of this file:
# a host-architecture .so would install fine and crash on device.
ARCH="$(file -b "$SO")"
case "$ARCH" in
    *"ARM aarch64"*|*"aarch64"*) ;;
    *)
        echo "error: $LIB_NAME is not arm64 — the toolchain did not target Android:" >&2
        echo "  $ARCH" >&2
        exit 1
        ;;
esac

mkdir -p "$JNI_LIBS"
# A debug Bevy build carries full debug info: ~1.6 GB with it, ~400 MB without.
# The device does not need the symbols, and Gradle would copy the larger file
# into the APK staging area on every build, so strip what gets staged. The
# artifact in `target/` is left untouched (it is the incremental-build output).
STRIP="$BIN/llvm-strip"
if [[ "$MODE" == "build" && "${KEEP_DEBUG_SYMBOLS:-0}" != "1" && -x "$STRIP" ]]; then
    cp "$SO" "$JNI_LIBS/$LIB_NAME"
    "$STRIP" --strip-debug "$JNI_LIBS/$LIB_NAME"
else
    cp "$SO" "$JNI_LIBS/$LIB_NAME"
fi

# Re-check after stripping: the strip step must not have broken the library,
# and this is the file the APK will actually contain.
ARCH="$(file -b "$JNI_LIBS/$LIB_NAME")"
case "$ARCH" in
    *"ARM aarch64"*|*"aarch64"*) ;;
    *)
        echo "error: the staged $LIB_NAME is not arm64:" >&2
        echo "  $ARCH" >&2
        exit 1
        ;;
esac

echo "krkr-rs android: staged $JNI_LIBS/$LIB_NAME ($(du -h "$JNI_LIBS/$LIB_NAME" | cut -f1))"
echo "krkr-rs android: next, from android/: gradle assembleDebug"
