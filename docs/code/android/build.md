# `build` — the cross-compile, the staging and the packaging

Source: `crates/android/build-android.sh`, `crates/android/check-jni-libs.sh`,
`android/` (the Gradle project), `.github/workflows/android.yml`

Everything Android-specific about the *build* lives in the script and in the
Gradle project, so the engine crates stay platform-neutral: they only need a
toolchain in the standard environment variables.

---

## 1. What is built

| artifact | produced by |
|---|---|
| `libkrkr_android.so` | `crates/android/build-android.sh` — the Rust cdylib (`crates/android`, `[lib] name = "krkr_android"`) |
| `app-release.apk` | `gradle :app:assembleRelease` in `android/` |

The APK's `native-code` is `arm64-v8a` only, by decision: there is no
emulator (x86_64) build. The game's own files never ship in the APK; the user
picks the folder at runtime.

The package is checked by `crates/android/check-jni-libs.sh`, which runs on
the staged directory before Gradle sees it and on the finished APK in CI —
see §6.

---

## 2. The cross-compile

```bash
ANDROID_NDK_HOME=/path/to/ndk ./crates/android/build-android.sh [--check] [--debug]
```

`ANDROID_NDK_HOME` is honoured if set; otherwise pass `--ndk`. `--check` runs
`cargo check` instead of a full build (much faster, no linking). `--debug`
builds the debug profile, which exists only for on-device debugging with a
debugger attached.

The script sets the toolchain itself:

```bash
export CC="$BIN/aarch64-linux-android${API_LEVEL}-clang"
export CXX="$BIN/aarch64-linux-android${API_LEVEL}-clang++"
export AR="$BIN/llvm-ar"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$CC"
export ANDROID_NDK_HOME="$NDK"
export CXXSTDLIB_aarch64_linux_android="c++_static"
```

and then runs a plain `cargo build --target aarch64-linux-android -p android`.
The toolchain must be addressed by its **triple-prefixed** driver name, which
is what tells clang to emit Android code — `crates/tjs/build.rs` passes no
`--target` of its own.

`ANDROID_NDK_HOME` is exported because `opusic-sys`'s build script switches to
the NDK's cmake toolchain when it sees it; without it the Opus build fails
with "Failed to find tool".

### The C++ runtime

The C++ runtime is linked **statically**, and that is what `CXXSTDLIB` is for.
`cc` — the build-script helper the C++ crates use — defaults to `c++_shared`
on Android, which puts `libc++_shared.so` into the engine's `DT_NEEDED`. That
library is **not** part of the Android platform: it has to be packaged into
the APK next to the engine, and when it is not, the app dies on the device
with `failed to dlopen libc++_shared.so` — long after the build went green.
`forkunion` (the C++ core of the `psd` crate) is the crate that pulls it in.

`CXXSTDLIB` is the variable `cc` reads for that choice; the script sets it
**target-scoped** (`CXXSTDLIB_aarch64_linux_android`), so the desktop build
is untouched. `crates/tjs/build.rs` links the runtime itself as well
(`static=c++_static`, `static=c++abi`) right after the VM's archive, so the
C++ code of the VM is resolved by the runtime whatever order the linker sees
the crates in.

The static runtime is also the right choice for the page size: the shared
runtime of the NDK is only 16 KB aligned from NDK r28 on, and the package is
mapped straight out of the APK (§3), so a separate runtime file would have to
be aligned as well.

### Why not `cargo ndk`

`cargo-ndk` wires up `CC`/`AR`/the Rust linker, but it does **not** provide
`CXX` in the form `crates/tjs/build.rs` reads (it sets the target-scoped
`CXX_<triple>`, while the build script reads plain `CXX`). The result is
worse than a failure: `build.rs` falls back to its default `zig c++`, which
compiles for the **host**, so an Android build silently contains x86-64
objects. `cargo check` does not link, so it looks like it worked.

The script therefore guards the trap: it checks the produced library's
architecture with `file` and **refuses to stage** anything that is not
arm64 — before and after the strip step.

### The release profile

The build is **release** by default. The workspace's release profile
(`lto = "thin"`, `strip = "symbols"`, `codegen-units = 1`) is what makes the
library packable: a debug build of this stack was measured at 1.6 GB
unstripped (414 MB stripped), release at 71 MB. The script strips the staged
copy with the NDK's `llvm-strip` and leaves the `target/` artifact untouched
as the incremental-build output.

---

## 3. The 16 KB page size

Android 15+ devices may use 16 KB memory pages (mandatory for new devices),
and the manifest sets `android:extractNativeLibs="false"`, so the loader maps
the `.so` straight out of the APK. NDK r27 and older link with 4 KB page
alignment (`PT_LOAD p_align = 0x1000`) by default; per Google's "Support 16 KB
page sizes", such a library "crashes at runtime with a segmentation fault".

The script therefore links with 16 KB alignment and **verifies** it before
staging:

```bash
ALIGN_RUSTFLAGS="-C link-arg=-Wl,-z,max-page-size=16384 -C link-arg=-Wl,-z,common-page-size=16384"
```

Both flags are needed: 16384 is the page size, and it must apply to
`max-page-size` (segment alignment) and `common-page-size`. The flags go into
the **target-scoped** rustflags variable, not the global `RUSTFLAGS` — which
would also reach the host build scripts, where macOS's linker rejects
`-Wl,-z,…`. An existing target-scoped value (or a global `RUSTFLAGS`) is
preserved, because cargo treats the target-scoped variable as a *replacement*
for `RUSTFLAGS` and dropping it would silently discard the caller's flags.

The verification reads the `PT_LOAD` alignments with `readelf` and refuses to
package a library whose alignment is not a non-zero multiple of `0x4000`. A
missing `readelf` is an error too: the script refuses to package an
*unverifiable* library. The rule lives in `check-jni-libs.sh` (§6), which
checks every library of the package, not only the engine's.

---

## 4. The API level

| value | |
|---|---|
| the native API level | `35` (Android 15) |
| the Rust target | `aarch64-linux-android` |
| the ABI | `arm64-v8a` |

The native API level is baked into the C/C++ objects, so changing it changes
the build. It must be a level the NDK ships a sysroot for: NDK r27c's
`usr/lib/aarch64-linux-android/` tops out at 35, which is also the app's
`minSdk`. Targeting a newer level therefore means moving to a newer NDK as
well — **they must be raised together**.

The Gradle side is independent: `compileSdk = 37`, `targetSdk = 37`,
`minSdk = 35`, `ndkVersion = "27.2.12479018"`, `abiFilters = ["arm64-v8a"]`.

Adding an ABI means adding it in three places: the script, `abiFilters` in
`android/app/build.gradle.kts`, and the `jniLibs` directory the Gradle build
picks up. Otherwise the APK ships without a `.so` for it and crashes at
launch.

---

## 5. The Gradle project

```
android/
  build.gradle.kts              the plugin versions (AGP 9.4.0, Kotlin 2.4.20)
  settings.gradle.kts
  gradle.properties
  app/build.gradle.kts          the module: compileSdk, minSdk, signing, packaging
  app/src/main/AndroidManifest.xml
  app/src/main/kotlin/dev/krkr/rs/
      MainActivity.kt           the Compose M3 launcher: the game library and the SAF picker
      KrkrGameActivity.kt       the engine Activity; publishes the settings over JNI
      GameLibrary.kt            the persisted picked folders
      RuntimeSettings.kt        the persisted runtime options
      EngineDiagnostics.kt      the state/log file contract
      LauncherLog.kt            the log tail reader
      SafPaths.kt               the URI→path resolution
      GamePreflight.kt, DurableWrite.kt, ui/Theme.kt
  app/src/main/jniLibs/arm64-v8a/libkrkr_android.so   ← staged by the script
```

Two details that are easy to get wrong and are pinned in the build files:

* **No `org.jetbrains.kotlin.android` plugin.** Since AGP 9.0 the Android
  plugin has Kotlin support built in, and applying it is a hard error. For the
  same reason `kotlinOptions { jvmTarget }` no longer resolves; the Kotlin
  `jvmTarget` is derived from `compileOptions`.
* **`androidx.appcompat` must be on the compile classpath**, even though the
  code never references it directly: the AGDK `GameActivity` derives from
  `AppCompatActivity`, and without appcompat the Kotlin compiler fails with
  "Cannot access 'androidx.appcompat.app.AppCompatActivity' which is a
  supertype of 'dev.krkr.rs.KrkrGameActivity'". For the same reason the
  engine Activity's theme must derive from an **AppCompat** theme — an
  `AppCompat`-derived theme is what `GameActivity.onCreateSurfaceView`
  requires, and the failure happens inside `super.onCreate`, *before* the
  native library loads, so it is invisible to the engine and shows up as a
  black screen.

`gradle assembleDebug` rather than `./gradlew`: the repository has no Gradle
wrapper (its jar is a binary), so CI installs a pinned Gradle instead.

### Signing

The release signing key comes from the **environment**, so CI can sign with a
real key that never enters the repository — a keystore is the app's permanent
identity, and committing it would let anyone publish as this package. The
four variables are `KRKR_KEYSTORE_PATH`, `KRKR_KEYSTORE_PASSWORD`,
`KRKR_KEY_ALIAS` and `KRKR_KEY_PASSWORD`.

When they are absent — a local build, or a CI run without the secrets — the
release falls back to AGP's debug key, which still produces an installable
APK. That is deliberate for a sideloaded experiment: no configuration should
produce a *broken* build, only one signed with a throwaway key. A **partial**
configuration is reported with a warning, because an APK signed by the debug
key cannot be upgraded to one signed by the release key without uninstalling
first.

---

## 6. The packaging check

`crates/android/check-jni-libs.sh` verifies that the native libraries of the
package can be loaded on a device. It takes either a directory of `.so` files
or an `.apk`, and for every library it checks that

* every `DT_NEEDED` entry is either an Android platform library or another
  library of the same package — a dependency that is neither is what the
  device reports as `failed to dlopen <library>`; `libc++_shared.so` is
  deliberately **not** a platform library, so an engine that needs it has to
  package it;
* every `PT_LOAD` segment is aligned to 16 KB (§3).

`build-android.sh` runs it on the staged directory, before Gradle packages
it, and the workflow runs it on the finished APK. The script has a
`--self-test` that builds fixtures with the host compiler and asserts that
the rules fire — a check that cannot fail is not a check.

---

## 7. CI

`.github/workflows/android.yml` triggers on **`android-*` tags** and on
`workflow_dispatch`. A branch push triggers nothing, so ordinary engine work
cannot start an Android build; a `v*` tag (an engine release on `main`) does
not match either.

The workflow installs the NDK, the SDK platform and the build tools, JDK 21,
Rust with the `aarch64-linux-android` target, Zig (the C++ dependency fetch
needs it) and `bison` (the TJS2 parser generator), runs the same
`build-android.sh`, runs `gradle :app:assembleRelease`, checks the packaged
native libraries (§6), and publishes the APK as an artifact — and, for a tag
run, as a GitHub Release asset.

The pinned versions are the ones the port was verified against: NDK r27c
compiles the vendored C++ TJS2 VM, so a version bump changes the compiler
that builds the VM and must be re-verified.

---

## 8. What is not verified here

**The Android target is never built or tested on the development host.** There
is no NDK and none may be installed. The host-side equivalents are:

* `cargo check -p platform --features platform-android` — the
  platform-independent helpers and the typed stub;
* `uv run --project scripts krkr-gate android-target` — the per-target
  dependency graph, resolved **without compiling**; it fails when a
  desktop-only backend feature leaks into the Android target or when two
  mutually exclusive backends are enabled together.

Everything Android-only is therefore documented as unverified. The device-only
checks that are still open:

| area | what a device must show |
|---|---|
| AAudio | the open and the start, the callback drain without underruns, the `PCM_I16` conversion, a real disconnect reap, whether `LOW_LATENCY` is granted |
| MediaCodec | the demux/decode loop, the `memfd` and `setDataSourceFd`, the seek and `flush`, the colour-format negotiation (`COLOR_FormatYUV420Flexible` and the documented planar fallback), the stride/slice-height/crop geometry, the format-change timing, the PCM encoding and the EOS/drain termination |
| the platform AAC fallback | the HE-AAC track decoding at the rate the container declares |
| the window | that the `ANativeWindow` handed over is the one the renderer binds to, that the surface is rebuilt after the window is destroyed, that the size is the rotated landscape size, and that the touch coordinates are in the same physical space as `size()` |
| the fonts | that `/system/fonts` is populated and a requested face resolves (the host test checks the directory list, not a device's contents) |
| the diagnostics files | that `<filesDir>` is writable by the app process, that the records appear in logcat, and that the Logs screen and the crash dialog read them back |
| `memfd_create` | that a device's seccomp policy permits it |
| `forkunion` (the PSB/PSD crate) | that the C++ core builds and runs for aarch64 |

The `AHardwareBuffer` readback path is not implemented at all; the
staging-copy transport is the current default.

---

## 9. Related documents

| topic | document |
|---|---|
| the Android composition root overview | [README.md](README.md) |
| the Android design, the milestones, the storage decision | [`android.md`](../../../docs.old/android.md) · snapshot [`docs.old/android.md`](../../../docs.old/android.md) |
| the Android backends (AAudio, MediaCodec, `AHardwareBuffer`) | [`10-android-backends.md`](../../../docs.old/migration/10-android-backends.md) · snapshot [`docs.old/migration/10-android-backends.md`](../../../docs.old/migration/10-android-backends.md) |
| the Kotlin frontend's own README | [`android/README.md`](../../../android/README.md) |
| the per-target dependency-graph check | [`../../rules/arch_rules.md`](../../rules/arch_rules.md) §12 |
| the agent gate | [`../../rules/arch_rules.md`](../../rules/arch_rules.md) §14 |
