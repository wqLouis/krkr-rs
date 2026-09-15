# krkr-rs on Android — design and milestones

Status: **experiment**. Target: a native Android app (Kotlin + Jetpack Compose,
Material 3) that manages a library of KiriKiri games and launches the real
engine — the same Rust + Bevy + C++ TJS2 stack the desktop binary runs — with
touch input and MediaCodec video.

This document is the plan. `docs/portability.md` is the older, broader build
assessment (Android + wasm); §1 there ("the full game path was not
cross-checked") is now superseded for Android — see §2 below.

---

## 1. Decisions

| Decision | Choice | Why |
|---|---|---|
| Scope | Experimental; correctness first, no schedule | User directive |
| Platform level | **`minSdk = 35` (Android 15+), `compileSdk`/`targetSdk` = 37 (latest)** | User directive: Android 15+ is acceptable, newest platform otherwise. Native code is built at API 35 because that is the highest level NDK r27c provides a sysroot for — raising `minSdk` past 35 means raising the NDK with it. |
| ABIs | **`arm64-v8a` only** | User directive. No `armeabi-v7a`/`x86`/`x86_64`. Emulator testing is out of scope until an x86_64 ABI is added back. |
| UI | Kotlin + **Jetpack Compose + Material 3** | User directive: "native M3 design app ui" |
| App shape | **Frontend-first**: a launcher app that picks a game folder, then hands off to the engine | User directive |
| Activity backend | `android-activity`'s **`GameActivity`** (`winit/android-game-activity`, `bevy/android-game-activity`) | `GameActivity` derives from `AndroidAppCompat` (`winit-0.30.13/src/platform/android.rs:49`), i.e. it *is* a normal `AppCompatActivity` — so it can host a Compose UI, unlike `NativeActivity` |
| Video | **NDK MediaCodec** (`libmediandk`) on Android; FFmpeg on desktop | `ffmpeg-next` links via pkg-config and cannot be assumed on Android; MediaCodec is what Kirikiroid2 uses |
| Folder picking | Android **SAF** (`ACTION_OPEN_DOCUMENT_TREE`) + persisted permission | User directive ("use android's system file picker") |
| Storage access | **SAF for picking, a resolved filesystem path for reading, `MANAGE_EXTERNAL_STORAGE`** — option A in §4 | The app is **sideloaded**, so the Play-policy restriction is moot (user directive). The SAF-backed storage backend (option B) is **not** planned. |
| Game selection | **Explicit folders only** — no auto-indexing of a default directory | User directive: mirror `krkr-rs run <game directory>` |
| Orientation | **Locked to landscape** | User directive. The games are 1280×720; portrait would letterbox badly. |
| Launcher state | **JSON config file** (`games.json`) in the app's private files dir — **Android app only** | User directive: keep the library in a config file so folders never have to be found again. JSON, matching `crates/tvp-config`, which deliberately replaced the reference's XML preferences with JSON. |
| Engine and CLI | **Unchanged on every platform.** No library, no config file, no game list in the Rust workspace | User directive: the Linux build is "pure simple cli" — `krkr-rs run <game directory>` and nothing more. The Android app is what adds convenience, not the engine. |

---

## 2. What is already verified (measured, not assumed)

- **The C++ TJS2 VM cross-compiles for Android.** Zig cannot do it (it refuses
  to provide Android libc: `unable to provide libc for target
  aarch64-linux.5.10...android.29`), but `crates/tjs2-sys/build.rs` already
  honours a `CXX` override, so pointing it at the NDK's
  `aarch64-linux-android21-clang++` and running
  `cargo check -p tjs2-sys --target aarch64-linux-android` **succeeds in ~19 s**
  (NDK r27c, bison + python present). This was the single largest unknown and
  it is now a known-good path. `docs/portability.md` §2/§11's statement that
  the full path "was not cross-checked because build.rs compiles for the host
  regardless of `--target`" is therefore outdated.
- **Bevy needs no Android logging work**: `bevy_log` already installs an
  `android_log_sys` backend on `target_os = "android"`
  (`bevy_log-0.19.1/src/lib.rs:21`, `src/android_tracing.rs`).
- **Bevy 0.19 exposes the needed backends**: `bevy/android-game-activity`
  (→ `bevy_internal/android-game-activity` → `bevy_winit` →
  `winit/android-game-activity`) and the `android-native-activity`
  alternative (`bevy_winit-0.19.1/Cargo.toml:43-44`).
- **`tjs2-sys/build.rs` is not target-aware** (`:332`): it gates the C++
  runtime link flags on `env::consts::OS`, which in a build script is the
  **host** OS. A Linux→Android build therefore emits desktop-only
  `-lc++ -lc++abi` instead of letting the NDK clang link `libc++_static`. Must
  switch to `CARGO_CFG_TARGET_OS == "android"` (drop the explicit C++ runtime
  flags for Android). *Not yet fixed.*
- **Zig can target Android for C, but not for C++** (zig 0.16.0, measured):
  `zig cc -target aarch64-linux-android` works once the sysroot's include dirs
  are given explicitly with `-isystem` (`--sysroot` is **not** honoured by the
  `cc`/`c++` wrapper). Every `zig c++ … -std=c++17` attempt, with or without the
  NDK's `libc++` headers and `-nostdinc++`, fails with `CacheCheckFailed`,
  because zig cannot supply Android's C++ runtime. The TJS2 VM is C++, so the
  Android cross-build uses the NDK clang; zig remains the toolchain for the
  Linux/host build.
- **`cargo ndk` must not be used for this build.** It does not set `CXX` in the
  form `crates/tjs2-sys/build.rs` reads (it sets the target-scoped
  `CXX_<triple>`), so the build script falls back to its default `zig c++` —
  which targets the **host**. `cargo ndk -t arm64-v8a build` was measured
  producing `out/obj/tjsInterCodeExec.o: ELF 64-bit … x86-64` inside an Android
  build, and `cargo check` never links, so it appears to succeed. The working
  invocation addresses the toolchain by its *triple-prefixed* driver name with
  plain `cargo build --target`; `crates/android/build-android.sh` does that and
  verifies the staged `.so` is aarch64.
- **NDK r27c's sysroot covers API 21–35** (`usr/lib/aarch64-linux-android/`),
  which is why the native API level is 35 and `minSdk` cannot go above it
  without a newer NDK. The app's `compileSdk`/`targetSdk` (37) are independent
  of that.

Not yet verified (still to be cross-checked once the FFmpeg feature split
lands): the full workspace for `aarch64-linux-android`, including `blake3`
(C + asm via `cc-rs` — needs `cargo-ndk`'s toolchain wiring, seen failing with
`failed to find tool "aarch64-linux-android-clang"`), `rodio`/`cpal` (AAudio),
and `opusic-sys`.

---

## 3. Architecture

```
android/                          Kotlin + Compose M3 app (Gradle)
  app/src/main/kotlin/…/
    MainActivity.kt               library: game list, "Add game" (SAF), settings
    GameActivity.kt               hosts the engine surface (GameActivity)
    GameLibrary.kt                persisted picked-folder URIs + metadata
    ui/                           Compose screens, M3 theme
  app/src/main/jniLibs/arm64-v8a/
    libkrkr_android.so            ← the Rust engine (cargo-ndk output)

crates/android/                   Rust cdylib (package `krkr-android`)
  src/lib.rs                      #[bevy_main] android_main; reads the game
                                  dir from the Activity (JNI/intent extras),
                                  builds the Bevy app from `render`
crates/render/                    shared app assembly (refactored so the
                                  desktop bin and the Android cdylib build the
                                  same App)
crates/tvp-natives/src/video/
  ffmpeg.rs                       desktop decoder   (default)
  android.rs                      MediaCodec decoder (target_os = "android")
```

Two Activities, one engine:

1. **`MainActivity`** (Compose M3) — the library. On first run it shows an empty
   state with a prominent "Add game folder" action. That launches
   `ACTION_OPEN_DOCUMENT_TREE`; the returned tree URI is persisted with
   `takePersistableUriPermission` so it survives reboots. Entries are stored in
   `GameLibrary` (DataStore). Tapping a game starts **`GameActivity`** with the
   folder as an Intent extra.
2. **`GameActivity`** — extends the AGDK `GameActivity`, hosts the surface and
   runs the Rust `android_main`. It is where the engine renders, and where the
   touch overlay lives.

The engine core is unchanged: mount storage → run `startup.tjs` → drive the VM
each frame → composite the layer tree. Only the platform edges differ (entry
point, storage access, video codec, input).

### 3.1 Launcher state — `games.json` (Android app only)

**This is an app-level concern, not an engine one.** The engine accepts a game
directory and nothing else, on every platform — the Android app is exactly
`krkr-rs run <game directory>`, with a UI in front of it. No library, config
file or game list is added to the Rust workspace: the desktop build stays a
plain CLI (`krkr-rs run <dir>`), and `games.json` is never read by the engine or
shared with it, so there is no schema coupling in either direction.

The library is a plain JSON file in the app's private files directory
(`<filesDir>/games.json`), not `SharedPreferences`: it is inspectable,
portable and user-editable, and JSON is already the project's configuration
convention (`crates/tvp-config` replaced the reference engine's
`GlobalPreference.xml`/`Kirikiroid2Preference.xml` with `config.json`).
Losing this file means re-picking every game folder, which is precisely what it
exists to prevent, so it is written atomically (temp file + rename) and read
tolerantly (a corrupt file is reported and treated as empty rather than
crashing the launcher).

```json
{
  "version": 1,
  "games": [
    {
      "name": "不可视之药",
      "treeUri": "content://com.android.externalstorage.documents/tree/primary%3AGames",
      "path": "/storage/emulated/0/Games"
    }
  ]
}
```

`treeUri` is the durable identity and the handle the persisted URI permission
is granted against; `path` is the resolved filesystem path the engine is
handed (§4), cached because resolving it costs a `ContentResolver` query. `path`
is absent when the folder is not addressable as a path (a cloud provider); such
an entry is still listed, and launching it explains why it cannot run rather
than starting the engine against a path that will not resolve.

---

## 4. Storage: SAF is a URI, the engine wants paths

This is the one genuinely awkward problem, and it is worth being explicit
rather than discovering it late. The engine's storage layer is path-based:
`Storage::mount(game_dir)` walks a directory and `Xp3Archive::open(path)` opens
files. A SAF tree URI (`content://com.android.externalstorage.documents/tree/primary%3AGames`)
is **not** a path, and Android 11+ restricts direct path reads of shared
storage. Three options:

**A. SAF for picking, path for reading (recommended for the experiment).**
Resolve the tree URI to a real path via `DocumentsContract.getTreeDocumentId`
(`primary:Games/MyGame` → `/storage/emulated/0/Games/MyGame`; SD volumes use
their UUID as the volume id). Request `MANAGE_EXTERNAL_STORAGE` ("All files
access") so path reads are actually permitted. Simple, no engine changes, fast
I/O — at the cost of a permission that Google Play restricts (fine for a
sideloaded/experimental build, which is the current scope).

**B. A real SAF-backed filesystem (the production-correct path).** Introduce a
`trait` over the storage backend and implement it via JNI `ContentResolver` /
`ParcelFileDescriptor` streaming, so no permissions are needed and Play policy
is satisfied. This is the right long-term answer but a substantial change:
every storage read crosses JNI, and the storage layer currently assumes
`std::fs`. Worth doing once the experiment proves the rest of the stack.

**C. Import/copy into app-private storage on selection.** Robust and
permission-free, but duplicates gigabytes per game — acceptable only as a
fallback for small games.

Plan: **option A is the decision.** The app is sideloaded, so
`MANAGE_EXTERNAL_STORAGE`'s Play-policy restriction does not apply (user
directive), and the engine's path-based storage layer is left exactly as it is.
Option **B** is consequently not pursued; the only concession to portability is
that all URI→path logic lives in one place (`SafPaths`), so it would be
contained if that ever changes. Option **C** is not used.

---

## 5. Touch input

KiriKiri games are mouse-and-keyboard driven, so the bridge is
touch → mouse/keyboard, not a new input model:

- **Tap → left click** at the touch position, through the same
  aspect-correct window→game transform the desktop bridge already uses, so
  engine-side layer hit-testing, `onMouseDown`/`onMouseUp` and the game's own
  cursor logic all keep working unchanged.
- **Long press → right click** (KAG uses right-click for the menu; games also
  call `System.getMouseButtonState(1)`).
- **Drag → mouse move with the button held** (some games implement drag).
- **Two-finger tap / swipe-down → `Escape`/right-click equivalent**, since
  there is no back button in the game and Android's back gesture must be
  handled (`onBackPressed` → engine "open menu" event, second press → exit).
- **On-screen controls overlay** for keys the games need and touch cannot
  express: at minimum a Ctrl (skip) button, a right-click button and a
  menu/back button, drawn either as a Compose overlay over the surface or
  in-engine. Compose is preferable (M3 styling, no engine changes) — it can
  sit in the same `GameActivity` hierarchy above the surface.
- Multi-touch must not double-fire: the bridge should consume `TouchInput`
  events on Android rather than also synthesising mouse events.

---

## 6. Video: MediaCodec

Replace FFmpeg on Android behind the **same** `video::MovieDecoder` API, so
`VideoOverlay` and the compositor are untouched:

- Demux/decode with the NDK C API (`AMediaExtractor` + `AMediaCodec`, API 21+,
  `libmediandk`) — no JNI needed for decoding.
- Video: decode to a `ByteBuffer` output (not a Surface) with
  `COLOR_FormatYUV420Flexible`, then convert YUV420 → tightly packed RGBA8 to
  match `RgbaFrame`. The conversion is platform-independent pure Rust and can
  be unit-tested on the host.
- Audio: `AMediaCodec` AAC → PCM, converted to the same interleaved `f32`
  `DecodedAudioPcm` the mixer already consumes.
- Seeking must reproduce `present_at`'s "seek when moving backwards" contract.
- Link `-lmediandk` only for Android (target-aware `build.rs`).

Honest limitation: this can be **compile-verified** in CI and its
platform-independent parts unit-tested on the host, but running it needs a
device. I will not claim runtime correctness for MediaCodec until it has been
run on hardware.

---

## 7. Packaging and CI

**Verified end to end on this machine.** From a clean tree:

```bash
ANDROID_NDK_HOME=/path/to/ndk ./crates/android/build-android.sh   # → 71 MB .so
cd android && gradle :app:assembleRelease                        # → 94 MB APK
```

That produces `android/app/build/outputs/apk/release/app-release.apk`: 447
entries, `native-code: 'arm64-v8a'`, `minSdk 35`, `targetSdk 37`, signed (v2
scheme), carrying a 71 MB engine `.so` and a 13 MB `classes.dex`. The adaptive
icon is present and correct (`aapt2 dump xmltree` shows the background colour and
foreground the sources declare).

**Release, not debug.** The workspace's release profile (`lto = "thin"`,
`strip = "symbols"`, `codegen-units = 1`) is what makes the library packable: the
debug build was 1.6 GB unstripped (414 MB stripped), release is 71 MB. The build
script therefore builds release by default; `--debug` exists only for on-device
debugging.

**What a local build needs** (none of which is in the repo):

- the SDK via `cmdline-tools` → `platforms;android-37.2` + `build-tools;37.0.0`.
  Note the platform package carries a **minor version** — there is no plain
  `platforms;android-37`.
- the NDK **reachable under `<sdk>/ndk/<version>`**, because AGP resolves
  `ndkVersion` there (to strip the prebuilt `.so`). Locally that is a symlink;
  `sdkmanager --install "ndk;27.2.12479018"` does it properly in CI.
- Gradle (9.7.1 was used) and a JDK. There is still no Gradle wrapper in the
  repo — its jar is a binary — so CI installs a pinned Gradle instead.

**16 KB page size.** Android 15+ devices may use 16 KB pages (mandatory for
new devices), and a library built with NDK r27 or older must be linked with
`-Wl,-z,max-page-size=16384` or the loader rejects it. Measured symptom of
getting this wrong: every `PT_LOAD` in the staged `.so` had `p_align = 0x1000`
while AndroidX's own packaged library is `0x4000` — and because the manifest
sets `extractNativeLibs=false`, the `.so` is mapped straight out of the APK,
which is exactly the affected path. `build-android.sh` now passes the flag *and
verifies* the staged library with `readelf`, failing loudly otherwise, so a
non-compliant build cannot be shipped.

**AGP 9 gotchas, all found by actually building** (see the commits that fixed
them): the Kotlin plugin `org.jetbrains.kotlin.android` is a *hard error* now
that AGP has built-in Kotlin support; `kotlinOptions { jvmTarget }` no longer
resolves for the same reason; `androidx.appcompat` must be an explicit
dependency; and — the one that actually crashed on a device —
`Theme.Krkr.Fullscreen` must derive from an **AppCompat** theme, because the
AGDK `GameActivity` extends `AppCompatActivity`:

```
java.lang.IllegalStateException: You need to use a Theme.AppCompat theme
(or descendant) with this activity.
  at com.google.androidgamesdk.GameActivity.onCreateSurfaceView(GameActivity.java:285)
```

That throw happens inside `super.onCreate`, *before* the native library loads,
so it is invisible to the engine and the app showed only a black screen.

**CI.** `.github/workflows/android.yml` runs **only on tags** (`android-*`) and
manual `workflow_dispatch` — never on the default branch, so ordinary engine
work cannot trigger an Android build. It installs JDK 17, the SDK/NDK, Zig
(the C++ dependency fetch needs it) and `bison`, calls the same script, runs
`gradle assembleRelease`, and publishes the APK to the GitHub Release for the
tag. It has run repeatedly; the `android-v1.0.0` release carries the built APK.

**Diagnostics on a device.** Because a failure here is otherwise invisible (the
engine's stdout/stderr is discarded, and `MANAGE_EXTERNAL_STORAGE` needs the
user to visit a Settings page that no `requestPermissions` call can reach), the
engine writes `<filesDir>/krkr.log` and an atomically-written
`<filesDir>/krkr_state.json` (`starting`/`running`/`failed`/`stopped` plus the
real error message and pid), and the launcher turns that into a dialog with the
reason and a **Logs** screen that can copy or share it. A crash is recognised
by a stale `starting`/`running` whose pid is no longer the current process.

Two deliberate choices worth restating:

- `cargo-ndk` is **not** used (§2 explains why it silently produces host
  objects), so there is nothing to install for it.
- The game's own assets are **not** in the APK: the user picks the folder at
  runtime (§4), which is also the only workable model for copyrighted game data.

---

## 8. Milestones

| # | Milestone | State |
|---|---|---|
| M0 | C++ TJS2 VM cross-compiles for `arm64-v8a` | **verified** (19 s, NDK r27c) |
| M1 | Whole workspace cross-checks with the FFmpeg feature off; `build.rs` made target-aware | **verified** (`build-android.sh --check`) |
| M2 | Rust `cdylib` + `#[bevy_main]` entry point; engine builds an `App` from `render` | **verified** — links and exports `android_main` |
| M3 | Compose M3 launcher + SAF picker + persisted library; hands the folder to `GameActivity` | **compiles and packages** (never run on a device) |
| M4 | Touch → mouse bridge | **implemented and host-tested** (on-screen controls and the back gesture still open) |
| M5 | MediaCodec video backend (replacing FFmpeg on Android) | **compile-verified and symbol-checked**; never run on a device |
| M6 | CI APK build + artifact upload | workflow written, never triggered |

The whole chain now produces an installable APK from a clean tree — see §7 —
but **nothing has been run on hardware**. What that leaves genuinely unproven is
narrow and worth stating plainly: whether the engine starts, decodes and renders
on a real phone; whether MediaCodec's demux/decode loop works; whether
`memfd_create` is permitted by a device's seccomp policy; and whether touch
delivery and coordinate mapping behave as the host tests assume.

---

## 9. Open questions

The storage, orientation and game-discovery questions are now **decided**
(§1): sideloaded storage via option A, landscape lock, and explicit folders
only — no auto-indexing of a default location.

1. **Resolution/scaling policy** on phones: the games are 1280×720. Letterbox
   (current `AutoMin` behaviour) is correct, but a wide phone leaves side bars —
   is a `ScalingMode` that fills more aggressively ever wanted?
2. **Audio focus/interruption** (phone call, notification) and lifecycle
   (backgrounding must pause the VM + mixer) — not yet scoped.
3. **Emulator support**: arm64-only means no x86_64 emulator; verification will
   need a physical device (or a temporary x86_64 ABI).
