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
# `CXX` in the form `crates/tjs/build.rs` reads (it sets the target-scoped
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
# We therefore link with 16 KB alignment and *verify* it before the package is
# handed to Gradle — see the "16 KB page-size alignment" and "verify the
# package" sections.
#
# ## The C++ runtime
#
# The engine links the C++ runtime statically. The shared runtime
# (`libc++_shared.so`) is not part of the Android platform: a library that
# needs it must package it, and one that does not is loaded by the device
# without it. See "the C++ runtime" below.
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

# Only arm64-v8a is supported, by decision (docs.old/android.md §1): there is no
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

CRATE="android"
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

# --- the C++ runtime --------------------------------------------------------
#
# The engine links the C++ runtime **statically**. `cc` — the build-script
# helper the C++ crates use — defaults to `c++_shared` on Android, and that
# puts `libc++_shared.so` into the engine's `DT_NEEDED`: the library then has
# to be packaged into the APK as well, or the loader fails on the device with
# "failed to dlopen libc++_shared.so". `CXXSTDLIB` is the variable `cc` reads
# for that choice; it is set target-scoped, so the host build is untouched.
#
# The static runtime is the right one here: the APK ships a single C++
# library, so there is no second copy of the runtime to disagree with, and
# the runtime ends up inside this library's own 16 KB-aligned image instead
# of in a separate file the loader would have to map.
export CXXSTDLIB_aarch64_linux_android="c++_static"

# --- the link search order ---------------------------------------------------
#
# The NDK keeps two kinds of library in the sysroot:
#
#   * the per-API directory (`usr/lib/<triple>/<api>/`) holds the *shared*
#     platform libraries the device actually provides — `libc.so`, `libm.so`,
#     `libdl.so`, `libz.so`, and the versioned NDK stubs;
#   * the sysroot root (`usr/lib/<triple>/`) holds the static archives the
#     build needs to find: `libc++_static.a`, `libc++abi.a`, and also the
#     *static* platform libraries `libc.a`, `libm.a`, `libdl.a`, `libz.a`.
#
# The static C++ runtime lives only in the root, so a search path for it is
# required. It must come **after** the per-API directory: with the root first,
# `-lc` resolves the root's `libc.a` instead of the per-API `libc.so`, and the
# engine links a private copy of bionic. A shared object cannot carry its own
# libc — the dynamic linker never initialises that copy — so the library
# crashes before `android_main` and the launcher reports a launch that never
# reached the engine.
#
# `check-jni-libs.sh` rejects such a package; this ordering is what prevents it.
SYSROOT_LIB_DIR="$TOOLCHAIN/sysroot/usr/lib/aarch64-linux-android"
API_LIB_DIR="$SYSROOT_LIB_DIR/$API_LEVEL"
if [[ ! -f "$API_LIB_DIR/libc.so" ]]; then
    echo "error: no per-API platform stubs at $API_LIB_DIR" >&2
    echo "  NDK r27c ships them for API levels 21..35;" >&2
    echo "  linking without this directory resolves -lc to the sysroot's libc.a." >&2
    exit 1
fi

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
#
# The sysroot root is last and the per-API directory is first, even when a
# caller supplied its own target-scoped flags: an existing `-L` for the root
# must not be able to shadow the platform's shared libraries.
ALIGN_RUSTFLAGS="-C link-arg=-Wl,-z,max-page-size=16384 -C link-arg=-Wl,-z,common-page-size=16384 -L native=$SYSROOT_LIB_DIR"
EXISTING_RUSTFLAGS="${CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS:-${RUSTFLAGS:-}}"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS="-L native=$API_LIB_DIR ${EXISTING_RUSTFLAGS} ${ALIGN_RUSTFLAGS}"

for tool in "$CC" "$CXX" "$AR"; do
    if [[ ! -x "$tool" ]]; then
        echo "error: missing toolchain binary: $tool" >&2
        exit 1
    fi
done

# The package is verified with `readelf` (the dependencies and the page
# alignment of every staged library); without it nothing can be verified, so
# the build stops before it packages anything.
if ! command -v readelf >/dev/null 2>&1; then
    echo "error: readelf (binutils) is required to verify the staged libraries" >&2
    echo "  install binutils; refusing to package unverifiable libraries" >&2
    exit 1
fi

echo "krkr-rs android: abi=$ABI target=$RUST_TARGET api=$API_LEVEL profile=$PROFILE"
echo "krkr-rs android: ndk=$NDK"

# --- build ------------------------------------------------------------------
#
# The crate graph is selected by `-p android`; the video backend is chosen by
# `crates/app`, which this crate depends on. Android has no system FFmpeg, so
# that choice must resolve to `media`'s MediaCodec backend (docs.old/android.md
# §6) and the desktop FFmpeg feature must not reach this target.
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

# --- the C++ runtime in the package ----------------------------------------
#
# The engine links the C++ runtime statically (see CXXSTDLIB above), so the
# package normally carries no C++ runtime at all. If a dependency pulls the
# shared runtime in anyway, the NDK's copy has to be packaged next to the
# engine, or the loader fails with "failed to dlopen libc++_shared.so".
#
# The NDK's prebuilt runtime is only 16 KB aligned from NDK r28 on, and the
# package is mapped straight out of the APK (`extractNativeLibs=false`), so a
# runtime that the device cannot map is refused by the packaging check below
# rather than shipped.
CXX_RUNTIME="libc++_shared.so"
NEEDED="$(readelf -dW "$JNI_LIBS/$LIB_NAME" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p')"
if [[ " $NEEDED " == *" $CXX_RUNTIME "* ]]; then
    # The shared runtime is per ABI, but the NDK also keeps it per API level.
    NDK_RUNTIME=""
    for candidate in \
        "$TOOLCHAIN/sysroot/usr/lib/aarch64-linux-android/$CXX_RUNTIME" \
        "$TOOLCHAIN/sysroot/usr/lib/aarch64-linux-android/$API_LEVEL/$CXX_RUNTIME"; do
        if [[ -f "$candidate" ]]; then
            NDK_RUNTIME="$candidate"
            break
        fi
    done
    if [[ -z "$NDK_RUNTIME" ]]; then
        echo "error: $LIB_NAME needs $CXX_RUNTIME, but the NDK does not provide it:" >&2
        echo "  $TOOLCHAIN/sysroot/usr/lib/aarch64-linux-android/$CXX_RUNTIME" >&2
        echo "  link the C++ runtime statically (CXXSTDLIB=c++_static) instead." >&2
        exit 1
    fi
    cp "$NDK_RUNTIME" "$JNI_LIBS/$CXX_RUNTIME"
    echo "krkr-rs android: staged $CXX_RUNTIME from the NDK"
else
    # A runtime left over from an earlier build would be packaged for
    # nothing; the engine does not need it.
    rm -f "$JNI_LIBS/$CXX_RUNTIME"
fi

# --- verify the package ------------------------------------------------------
#
# The staged directory is what Gradle puts into the APK, so it is checked as
# a package: every dependency of every library must be resolvable on the
# device (the platform or another packaged library), and every library must
# be loadable from the APK as it is — `extractNativeLibs=false` maps the file
# straight out of the APK, so a PT_LOAD whose p_align is not a multiple of
# 0x4000 (16 KB) crashes on a 16 KB-page device.
bash "$ROOT/crates/android/check-jni-libs.sh" "$JNI_LIBS"

echo "krkr-rs android: staged $JNI_LIBS/$LIB_NAME ($(du -h "$JNI_LIBS/$LIB_NAME" | cut -f1))"
echo "krkr-rs android: next, from android/: gradle assembleDebug"
