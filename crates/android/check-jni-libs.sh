#!/usr/bin/env bash
# Verify that the native libraries of the Android package can actually be
# loaded on a device.
#
# ## Why this exists
#
# The APK ships native libraries as files; the loader resolves their
# `DT_NEEDED` entries when a library is `dlopen`ed. A library whose
# dependency is neither part of the platform nor present in the package fails
# at *runtime*, with an error that names the missing file and nothing else —
# `failed to dlopen libc++_shared.so` is what the C++ runtime of the engine
# used to produce, because the engine linked it dynamically while nothing
# ever packaged it.
#
# A build that produces such a library is green everywhere and broken on the
# device, so the packaging is checked here instead: every `DT_NEEDED` entry
# must be either an Android platform library or another library of the same
# package, and every packaged library must be loadable from the APK as it is
# (the manifest sets `android:extractNativeLibs="false"`, so the loader maps
# the file straight out of the APK and its segments must be aligned to the
# device's page size — 16 KB on Android 15+).
#
# ## Usage
#
#   ./crates/android/check-jni-libs.sh <path>
#   ./crates/android/check-jni-libs.sh --self-test
#
# `<path>` is either a directory of `.so` files (the staged
# `android/app/src/main/jniLibs/<abi>/` directory) or an `.apk` file. An APK
# is unpacked with `unzip`, or with `python3 -m zipfile` when `unzip` is not
# installed.
set -euo pipefail

# The platform libraries an Android application may depend on. They are
# provided by the device, not by the package, so a `DT_NEEDED` entry naming
# one of them is satisfied without shipping anything.
#
# `libc++_shared.so` is deliberately **not** in this list: the C++ runtime of
# the NDK is not part of the platform, it is a library the package has to
# carry itself (see `build-android.sh`, which links the C++ runtime
# statically so that it does not have to).
PLATFORM_LIBS=" \
libaaudio.so libandroid.so libbinder.so libc.so libcamera2ndk.so libdl.so \
libEGL.so libGLESv2.so libGLESv3.so libjnigraphics.so liblog.so libm.so \
libmediandk.so libnativewindow.so libOpenSLES.so libutils.so libvulkan.so \
libz.so"

# The page size a 16 KB-page device (Android 15+) requires of every library
# that is mapped straight out of the APK.
PAGE_ALIGN=16384

die() {
    echo "check-jni-libs: $*" >&2
    exit 1
}

usage() {
    sed -n '2,31p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
}

# The directory a temporary unpacking or fixture tree lives in; the EXIT
# trap removes it. It is a global because the trap runs after the function
# that created it returned.
CLEANUP_DIR=""

cleanup() {
    [[ -n "$CLEANUP_DIR" ]] && rm -rf "$CLEANUP_DIR"
    return 0
}

# The libraries a shared object depends on, one per line.
needed_libs() {
    readelf -dW "$1" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p'
}

# Unpack the `lib/` tree of an APK into a directory and print the directory
# that holds the libraries.
unpack_apk() {
    local apk="$1" out="$2"
    mkdir -p "$out"
    if command -v unzip >/dev/null 2>&1; then
        if ! unzip -l "$apk" 'lib/*' 2>/dev/null | grep -q 'lib/'; then
            die "$apk contains no native libraries"
        fi
        unzip -q -o "$apk" 'lib/*' -d "$out"
    elif command -v python3 >/dev/null 2>&1; then
        python3 - "$apk" "$out" <<'PY'
import sys, zipfile

with zipfile.ZipFile(sys.argv[1]) as apk:
    names = [n for n in apk.namelist() if n.startswith("lib/")]
    if not names:
        sys.exit("the archive contains no native libraries")
    apk.extractall(sys.argv[2], members=names)
PY
    else
        die "neither unzip nor python3 is available to unpack $apk"
    fi
    echo "$out/lib"
}

# Check one shared object: its dependencies must be satisfiable, and its
# segments must be aligned to the device's page size.
check_lib() {
    local lib="$1" name dir
    name="$(basename "$lib")"
    # A dependency is packaged next to the library that needs it — the loader
    # looks in the directory of the library, which for an APK is the
    # directory of its ABI.
    dir="$(dirname "$lib")"

    if ! readelf -dW "$lib" >/dev/null 2>&1; then
        die "$name: not an ELF shared object"
    fi
    local needed
    needed="$(needed_libs "$lib")"

    local dep
    while read -r dep; do
        [[ -n "$dep" ]] || continue
        if [[ -f "$dir/$dep" ]]; then
            continue
        fi
        if [[ " $PLATFORM_LIBS " == *" $dep "* ]]; then
            continue
        fi
        die "$name depends on $dep, which is neither part of the platform nor
  packaged next to it. The loader would fail with \"failed to dlopen $dep\".
  Link the dependency into the package, or add it to PLATFORM_LIBS in
  $(basename "$0") if it really is a platform library."
    done <<< "$needed"

    # The Android C library is part of the platform. A link search path that
    # reaches the NDK sysroot's `libc.a` before the per-API `libc.so` stub
    # links bionic into the library: `DT_NEEDED` then has no `libc.so`, and the
    # private copy is never initialised by the dynamic linker, so the library
    # crashes before its entry point runs. Bionic's static sources leave their
    # build paths in `.rodata` and a weak `__loader_add_thread_local_dtor` in
    # the symbol table; either is proof the static archive was linked, whether
    # or not `libc.so` also appears in `DT_NEEDED`.
    if grep -aq 'bionic/libc/bionic/' "$lib" ||
        grep -aq '__loader_add_thread_local_dtor' "$lib"; then
        die "$name carries bionic's static C library (libc.a). A shared object
  cannot carry a private libc — the dynamic linker never initialises that copy,
  so the library crashes before its entry point. Order every link search path so
  the NDK's per-API directory (.../sysroot/usr/lib/<triple>/<api>) comes before
  the sysroot root, where libc.a lives; then -lc resolves libc.so instead."
    fi

    local aligns
    aligns="$(readelf -lW "$lib" | awk '$1 == "LOAD" { print $NF }')"
    if [[ -z "$aligns" ]]; then
        die "$name: no PT_LOAD segments found (unexpected readelf output)"
    fi
    local align
    while read -r align; do
        if (( align == 0 || align % PAGE_ALIGN != 0 )); then
            die "$name is not 16 KB page aligned: PT_LOAD p_align=$align
  every p_align must be a non-zero multiple of $PAGE_ALIGN; Android 15+
  16 KB-page devices crash loading it."
        fi
    done <<< "$aligns"
}

check_dir() {
    local dir="$1" lib count=0
    while IFS= read -r lib; do
        [[ -n "$lib" ]] || continue
        check_lib "$lib"
        count=$((count + 1))
    done < <(find "$dir" -name '*.so' -type f | sort)
    if (( count == 0 )); then
        die "no shared object found in $dir"
    fi
    echo "check-jni-libs: $count shared object(s) in $dir: dependencies satisfied, 16 KB aligned"
}

# Build fixtures with the host compiler and assert that the rules above fire.
self_test() {
    local cc
    if ! cc="$(command -v cc || command -v gcc)"; then
        echo "check-jni-libs: self-test skipped: no host C compiler"
        return 0
    fi
    local tmp
    tmp="$(mktemp -d)"
    CLEANUP_DIR="$tmp"
    trap cleanup EXIT
    mkdir -p "$tmp/runtime" "$tmp/alone" "$tmp/complete" "$tmp/misaligned"

    # A stand-in for the NDK's shared C++ runtime, and a library that needs
    # it — the shape of the bug this check exists for.
    printf 'int krkr_fake_cxx(void) { return 0; }\n' > "$tmp/fake.c"
    "$cc" -shared -fPIC -nostdlib -o "$tmp/runtime/libc++_shared.so" "$tmp/fake.c" \
        -Wl,-z,max-page-size=16384
    printf 'extern int krkr_fake_cxx(void);\nint krkr_use(void) { return krkr_fake_cxx(); }\n' \
        > "$tmp/use.c"
    "$cc" -shared -fPIC -nostdlib -o "$tmp/alone/libneeds.so" "$tmp/use.c" \
        -Wl,--no-as-needed -L"$tmp/runtime" -lc++_shared -Wl,-z,max-page-size=16384

    local out
    if out="$(check_dir "$tmp/alone" 2>&1)"; then
        die "self-test: a library with an unresolvable dependency was accepted"
    fi
    if [[ "$out" != *"libc++_shared.so"* ]]; then
        die "self-test: the failure does not name libc++_shared.so: $out"
    fi
    echo "check-jni-libs: self-test: an unresolvable dependency is rejected"

    # The same library with the runtime packaged next to it is complete.
    cp "$tmp/alone/libneeds.so" "$tmp/runtime/libc++_shared.so" "$tmp/complete/"
    check_dir "$tmp/complete" >/dev/null
    echo "check-jni-libs: self-test: a packaged dependency is accepted"

    # A shared object that carries bionic's static libc has no `libc.so` in
    # DT_NEEDED and keeps bionic's source paths in `.rodata`. That is the shape
    # of a link whose search path reached the NDK sysroot's `libc.a` first;
    # the check must refuse to ship it, because the loader cannot initialise a
    # private libc inside a shared object.
    mkdir -p "$tmp/static-libc"
    cat > "$tmp/static_libc.c" <<'C'
__attribute__((used)) static const char bionic_libc_marker[] =
    "bionic/libc/bionic/bionic_elf_tls.cpp";
int krkr_static_libc_marker(void) { return bionic_libc_marker[0]; }
C
    "$cc" -shared -fPIC -nostdlib -o "$tmp/static-libc/libstaticlibc.so" \
        "$tmp/static_libc.c" -Wl,-z,max-page-size=16384
    if out="$(check_dir "$tmp/static-libc" 2>&1)"; then
        die "self-test: a library with a private static libc was accepted"
    fi
    if [[ "$out" != *"carries bionic's static C library"* ]]; then
        die "self-test: the failure does not name the static C library: $out"
    fi
    echo "check-jni-libs: self-test: a private static libc is rejected"

    # A library that is not 16 KB aligned is rejected.
    "$cc" -shared -fPIC -nostdlib -o "$tmp/misaligned/libsmall.so" "$tmp/fake.c"
    if out="$(check_dir "$tmp/misaligned" 2>&1)"; then
        die "self-test: a library that is not 16 KB aligned was accepted"
    fi
    if [[ "$out" != *"page aligned"* ]]; then
        die "self-test: the failure does not report the page alignment: $out"
    fi
    echo "check-jni-libs: self-test: a misaligned library is rejected"
    echo "check-jni-libs: self-test ok"
}

case "${1:-}" in
    -h|--help) usage ;;
    --self-test) self_test; exit 0 ;;
esac
[[ $# -eq 1 ]] || usage

target="$1"
if [[ -d "$target" ]]; then
    check_dir "$target"
elif [[ -f "$target" ]]; then
    tmp="$(mktemp -d)"
    CLEANUP_DIR="$tmp"
    trap cleanup EXIT
    libdir="$(unpack_apk "$target" "$tmp")"
    [[ -d "$libdir" ]] || die "$target contains no native libraries"
    check_dir "$libdir"
else
    die "$target is neither a directory nor a file"
fi
