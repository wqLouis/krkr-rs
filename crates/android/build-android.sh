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
# ## 16 KB page sizes
#
# Android 15+ devices may use 16 KB memory pages (mandatory for new devices),
# and the manifest sets `android:extractNativeLibs="false"`, so the loader maps
# this `.so` straight out of the APK. NDK r27 and older link with 4 KB page
# alignment (PT_LOAD p_align = 0x1000) by default; per Google's "Support 16 KB
# page sizes", such a library "crashes at runtime with a segmentation fault".
# We therefore link with 16 KB alignment and *verify* it before staging — see
# the "16 KB page-size alignment" and "verify 16 KB page alignment" sections.
#
# ## Usage
#
#   ./crates/android/build-android.sh [--ndk <path>] [--check] [--debug]
#
# `ANDROID_NDK_HOME` is honoured if set; otherwise pass `--ndk`. `--check` runs
# `cargo check` instead of a full build (much faster, no linking).
#
# The build is **release** by default. A debug build of this stack is unusable
# for packaging — full debug info and no optimisation made the `.so` 1.6 GB —
# whereas the workspace's release profile (`lto = "thin"`,
# `strip = "symbols"`, `codegen-units = 1`) produces a library in the tens of
# megabytes. `--debug` exists only for on-device debugging with a debugger
# attached.
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
# Release unless `--debug` is given; see the usage note above for why.
PROFILE="release"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --ndk) NDK="$2"; shift 2 ;;
        --ndk=*) NDK="${1#--ndk=}"; shift ;;
        --check) MODE="check"; shift ;;
        --debug) PROFILE="debug"; shift ;;
        -h|--help) sed -n '2,50p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "build-android.sh: unknown argument '$1' (try --help)" >&2; exit 2 ;;
    esac
done

# cargo's profile flag and the output directory it implies.
CARGO_PROFILE_ARGS=()
if [[ "$PROFILE" == "release" ]]; then
    CARGO_PROFILE_ARGS+=(--release)
fi

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

# --- 16 KB page-size alignment ----------------------------------------------
#
# Pass the alignment to the Rust link via the *target-scoped* rustflags variable
# (not the global RUSTFLAGS, which would also reach host build scripts: macOS's
# linker rejects `-Wl,-z,...`). Both page-size flags are needed; 16384 is the
# 16 KB page size, and it must apply to max-page-size (segment alignment) and
# common-page-size. The value of an existing target-scoped variable — or, if
# that is unset, a global RUSTFLAGS — is preserved: cargo treats the
# target-scoped variable as a replacement for RUSTFLAGS, so dropping it would
# silently discard the caller's flags.
ALIGN_RUSTFLAGS="-C link-arg=-Wl,-z,max-page-size=16384 -C link-arg=-Wl,-z,common-page-size=16384"
EXISTING_RUSTFLAGS="${CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS:-${RUSTFLAGS:-}}"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS="${EXISTING_RUSTFLAGS} ${ALIGN_RUSTFLAGS}"

for tool in "$CC" "$CXX" "$AR"; do
    if [[ ! -x "$tool" ]]; then
        echo "error: missing toolchain binary: $tool" >&2
        exit 1
    fi
done

echo "krkr-rs android: abi=$ABI target=$RUST_TARGET api=$API_LEVEL profile=$PROFILE"
echo "krkr-rs android: ndk=$NDK"

# --- build ------------------------------------------------------------------
#
# No FFmpeg: Android has no system FFmpeg, and this crate depends on `render`
# with `default-features = false` (Cargo.toml), which drops the `ffmpeg`
# feature transitively. The engine's MediaCodec backend is the replacement
# (docs/android.md §6); until it lands, video returns a clear "no MPEG decoder
# in this build" error rather than failing to compile.
cd "$ROOT"
cargo "$MODE" "${CARGO_PROFILE_ARGS[@]}" --target "$RUST_TARGET" -p "$CRATE"

if [[ "$MODE" == "check" ]]; then
    echo "krkr-rs android: check complete (no .so produced)"
    exit 0
fi

# --- verify and stage the shared library ------------------------------------

SO="$ROOT/target/$RUST_TARGET/$PROFILE/$LIB_NAME"
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
# The release profile already strips symbols (`strip = "symbols"`), so this is
# a no-op there; it matters for `--debug`, where full debug info made the
# library 1.6 GB. Gradle copies whatever is staged into the APK, so stage the
# small one. The `target/` artifact is left untouched (it is the
# incremental-build output).
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

# --- verify 16 KB page alignment --------------------------------------------
#
# The flags above are the fix; this is the proof. `extractNativeLibs=false`
# maps the `.so` directly out of the APK, so a PT_LOAD whose p_align is not a
# multiple of 0x4000 (16 KB) crashes on a 16 KB-page device. Refuse to stage a
# library that is not compliant, and refuse to guess when the tool needed to
# check it is missing.
if ! command -v readelf >/dev/null 2>&1; then
    echo "error: readelf (binutils) is required to verify 16 KB page alignment" >&2
    echo "  install binutils; refusing to stage an unverifiable $LIB_NAME" >&2
    exit 1
fi

LOAD_ALIGNS="$(readelf -lW "$JNI_LIBS/$LIB_NAME" | awk '$1 == "LOAD" { print $NF }')"
if [[ -z "$LOAD_ALIGNS" ]]; then
    echo "error: no PT_LOAD segments found in $JNI_LIBS/$LIB_NAME (unexpected readelf output)" >&2
    exit 1
fi
while read -r align; do
    if (( align == 0 || align % 0x4000 != 0 )); then
        echo "error: $LIB_NAME is not 16 KB page aligned: PT_LOAD p_align=$align" >&2
        echo "  every p_align must be a non-zero multiple of 0x4000 (16384);" >&2
        echo "  Android 15+ 16 KB-page devices will crash loading it." >&2
        exit 1
    fi
done <<< "$LOAD_ALIGNS"
echo "krkr-rs android: all PT_LOAD segments are 16 KB aligned"

echo "krkr-rs android: staged $JNI_LIBS/$LIB_NAME ($(du -h "$JNI_LIBS/$LIB_NAME" | cut -f1))"
echo "krkr-rs android: next, from android/: gradle assembleDebug"
