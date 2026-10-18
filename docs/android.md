# krkr-rs on Android — design and milestones

Status: **experiment**. Target: a native Android app (Kotlin + Jetpack Compose,
Material 3) that manages a library of KiriKiri games and launches the real
engine — the same Rust engine + C++ TJS2 stack the desktop binary runs — with
touch input and MediaCodec video.

This document is the plan. [`portability.md`](portability.md) covers the
per-target backend features; §2 below is the Android-specific state.

---

## 1. Decisions

| Decision | Choice | Why |
|---|---|---|
| Scope | Experimental; correctness first, no schedule | User directive |
| Platform level | **`minSdk = 35` (Android 15+), `compileSdk`/`targetSdk` = 37 (latest)** | User directive: Android 15+ is acceptable, newest platform otherwise. Native code is built at API 35 because that is the highest level NDK r27c provides a sysroot for — raising `minSdk` past 35 means raising the NDK with it. |
| ABIs | **`arm64-v8a` only** | User directive. No `armeabi-v7a`/`x86`/`x86_64`. Emulator testing is out of scope until an x86_64 ABI is added back. |
| UI | Kotlin + **Jetpack Compose + Material 3** | User directive: "native M3 design app ui" |
| App shape | **Frontend-first**: a launcher app that picks a game folder, then hands off to the engine | User directive |
| Activity backend | `android-activity`'s **`GameActivity`** | `GameActivity` derives from `AndroidAppCompat` (`winit-0.30.13/src/platform/android.rs:49`), i.e. it *is* a normal `AppCompatActivity` — so it can host a Compose UI, unlike `NativeActivity` |
| Video | **NDK MediaCodec** (`libmediandk`) on Android; FFmpeg on desktop | `ffmpeg-next` links via pkg-config and cannot be assumed on Android; MediaCodec is what Kirikiroid2 uses |
| Folder picking | Android **SAF** (`ACTION_OPEN_DOCUMENT_TREE`) + persisted permission | User directive ("use android's system file picker") |
| Storage access | **SAF for picking, a resolved filesystem path for reading, `MANAGE_EXTERNAL_STORAGE`** — option A in §4 | The app is **sideloaded**, so the Play-policy restriction is moot (user directive). The SAF-backed storage backend (option B) is **not** planned. |
| Game selection | **Explicit folders only** — no auto-indexing of a default directory | User directive: mirror `krkr-rs run <game directory>` |
| Orientation | **Locked to landscape** | User directive. The games are 1280×720; portrait would letterbox badly. |
| Launcher state | **JSON config file** (`games.json`) in the app's private files dir — **Android app only** | User directive: keep the library in a config file so folders never have to be found again. JSON is the project's configuration convention (`api::config` replaced the reference's `GlobalPreference.xml`/`Kirikiroid2Preference.xml` with a `key=value` store). |
| Engine and CLI | **Unchanged on every platform.** No library, no config file, no game list in the Rust workspace | User directive: the Linux build is "pure simple cli" — `krkr-rs run <game directory>` and nothing more. The Android app is what adds convenience, not the engine. |

---

## 2. What is already verified (measured, not assumed)

> **The development host has no NDK or Android SDK, and the Android target is
> never built or tested here** (AGENTS §0, `TODO.md` §11). The cross-compiles
> and the packaged build below were run on a machine that has the toolchain;
> Android-only code that has not been compiled is documented as unverified,
> never as working. The host bar is `cargo check --workspace` staying
> green, the `platform-android` host-feature steps and
> `scripts/check-android-target.py`.

- **The C++ TJS2 VM cross-compiles for Android.** Zig cannot do it (it refuses
  to provide Android libc: `unable to provide libc for target
  aarch64-linux.5.10...android.29`), but `crates/tjs/build.rs` already
  honours a `CXX` override, so pointing it at the NDK's
  `aarch64-linux-android21-clang++` and running
  `cargo check -p tjs --target aarch64-linux-android` **succeeds in ~19 s**
  (NDK r27c, bison + python present). This was the single largest unknown and
  it is now a known-good path.
- **`tjs/build.rs` is target-aware**: it reads `CARGO_CFG_TARGET_OS` and the
  Cargo target, emits the desktop C++ runtime flags (`-lc++ -lc++abi -pthread`)
  only when the *target* is desktop Linux, and derives oniguruma's `config.h`
  sizes from the target rather than the build host. The Android cross-build
  supplies the NDK `CC`/`CXX` and does not need the desktop runtime flags.
- **Zig can target Android for C, but not for C++** (zig 0.16.0, measured):
  `zig cc -target aarch64-linux-android` works once the sysroot's include dirs
  are given explicitly with `-isystem` (`--sysroot` is **not** honoured by the
  `cc`/`c++` wrapper). Every `zig c++ … -std=c++17` attempt, with or without the
  NDK's `libc++` headers and `-nostdinc++`, fails with `CacheCheckFailed`,
  because zig cannot supply Android's C++ runtime. The TJS2 VM is C++, so the
  Android cross-build uses the NDK clang; zig remains the toolchain for the
  Linux/host build.
- **`cargo ndk` must not be used for this build.** It does not set `CXX` in the
  form `crates/tjs/build.rs` reads (it sets the target-scoped
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

The full workspace is cross-checked for `aarch64-linux-android` on a machine
with the NDK; it is not cross-checked on the development host.

### 2.1 Platform backend: window, input and scale (current state)

`crates/platform/platform/src/android.rs` is the L1 Android backend. Three
parts are real and compile for `aarch64-linux-android` on a machine with the
NDK (the host-feature step `cargo check -p platform --features platform-android`
checks the target-independent helpers and the typed stub):

* **The window.** On Android, `PlatformWindow` owns the `ANativeWindow` the
  activity acquired and projects it through `raw-window-handle`
  (`RawWindowHandle::AndroidNdk`), so the renderer can bind and rebind its
  Vulkan surface to the real window. `size()` is the `ANativeWindow` buffer
  size in physical pixels; `scale_factor()` is the display's
  `Configuration.densityDpi / 160`. The non-Android host keeps a typed stub,
  because the `ndk` crate is target-gated.
* **Input translation.** `PlatformWindow::physical_key_from_android`,
  `touch_phase_from_android` and `mouse_button_from_android` translate the
  Android `AKEYCODE_*` / `AMOTION_EVENT_*` values into `platform`'s
  `PhysicalKey` / `TouchPhase` / `MouseButton`. They name no Android type, so
  they are unit-tested on the host. `AKEYCODE_BACK` becomes
  `PhysicalKey::Escape` (`MainScene.cpp:820-829`) rather than a new key, and the
  reference `VK_*` codes stay the compatibility layer's job. The composition
  root (`crates/android/src/input.rs`) consumes contact phases through the
  touch→mouse synthesizer (`crates/android/src/gesture.rs`, §5) instead of
  forwarding raw touch.
* **Audio output** (AAudio) is real.

The **event loop** is driven by the Android composition root, not by
`platform::run`. `platform::run` is the desktop (winit) driver and still
returns `PlatformError::NotImplemented` on Android; the Android root
(`crates/android`) owns the `android-activity` `GameActivity` glue and turns
its lifecycle and input callbacks into the same `PlatformEvent`s, calling the
same `EventHandler` the winit driver calls. The `android-activity` dependency
lives in `crates/android` (target-scoped) so the desktop workspace is
unaffected:

```toml
[target.'cfg(target_os = "android")'.dependencies]
android-activity = { version = "0.6", default-features = false, features = ["game-activity"] }
```

(`android-activity` 0.6.1 uses the same `ndk` 0.9 as `platform`, so the
`NativeWindow` type unifies. Its build script compiles a small C++ glue file,
so an Android target check needs the NDK `cc`/`cxx` that the `platform` check
above does not.)

### 2.2 Presentation: the game is always scaled to fit (Fit with letterbox)

On a phone the game must never be presented at its raw logical size, and the
same is now true on the desktop: the engine presents every game uniformly
scaled to the largest size that fits the display, centred, with the remainder
letterboxed (`docs/migration/11`, D39). There is no per-platform default and no
mode to choose — Fit is the single rule, on every backend.

The reference's Android build stretches instead: it sets the design resolution
to the full frame size with cocos2d's `EXACT_FIT`
(`reference/cpp/core/environ/cocos2d/AppDelegate.cpp:60-70`) and derives the
design size from the screen aspect
(`reference/cpp/core/environ/cocos2d/MainScene.cpp:436-438`). Keeping the
aspect (no distortion) is a deliberate divergence, chosen for phones; it is a
presentation-internal choice, so nothing a script observes changes — a script
still sees its own declared logical size.

The platform side reports the physical surface size (`PlatformWindow::size()`)
and the display density (`PlatformWindow::scale_factor()`). The mapping between
the declared logical size and the surface is L2's pixel-translation layer
(`docs/migration/11` §6), never L1 and never the launcher.

---

## 3. Architecture

```
android/                          Kotlin + Compose M3 app (Gradle)
  app/src/main/kotlin/…/
    MainActivity.kt               library: game list, "Add game" (SAF), settings
    KrkrGameActivity.kt           hosts the engine surface; publishes the game
                                  dir + runtime settings over JNI
    GameLibrary.kt                persisted picked-folder URIs + metadata
    RuntimeSettings.kt            persisted runtime options (frame rate,
                                  archive preload, debug overlay, log level)
    ui/                           Compose screens, M3 theme
  app/src/main/jniLibs/arm64-v8a/
    libkrkr_android.so            ← the Rust engine (cargo-ndk output)

crates/android/                   Rust cdylib (package `krkr-android`)
  src/lib.rs                      android_main: reads the game dir and the
                                  runtime settings from the Activity over JNI,
                                  builds the shared `app::Host`
  src/jni_fields.rs               the static-field JNI reads
  src/config.rs                   validates the settings into a `RuntimeConfig`

crates/app/                       shared app assembly (`app::Host`/`app::GameHost`,
                                  driven by the desktop `krkr` binary and by the
                                  Android cdylib alike)

crates/platform/media/src/video/
  ffmpeg.rs                       desktop decoder   (`video-ffmpeg`)
  mediacodec.rs                   MediaCodec decoder (`video-mediacodec`)

crates/tjs/                       C++ TJS2 VM (target-aware build)
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
convention (`api::config` replaced the reference engine's
`GlobalPreference.xml`/`Kirikiroid2Preference.xml` with a `key=value` store).
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

### 3.2 Runtime settings — `settings.json` and the JNI path

The launcher also persists the player's runtime options and carries them into
the engine. A **Settings** screen reachable from the launcher's top bar exposes
the options the desktop launcher calls `--fps`, `--preload-archives` and
`--debug-overlay`, with the same accepted values. The decoded-image cache
limit is currently a desktop CLI option only; Android leaves the engine's
reference default (disabled).
Nothing is offered for a feature the engine has no knob for
— a control that silently did nothing is the bug class this project forbids.
Presentation is not among the options: Fit-with-letterbox is the engine's single
rule (`docs/migration/11`, D39), so there is no scaling control to show.

| option | values / range | default | basis |
|---|---|---|---|
| Frame-rate limit | decimal integer `1..=1000` fps | `60` | reference `fps_limit` default 60 (`reference/cpp/core/environ/ui/PreferenceConfig.h:304-309`); the same range `--fps` accepts |
| Archive preload | `true`, `false` | `false` | whether every mounted archive is read into memory when it mounts; the cost is that archive's size in resident memory, the gain is that entry reads stop touching the backing filesystem |
| Debug overlay | `true`, `false` | `false` | draw FPS and 1% low, CPU, disk I/O, memory and object counts in the top-left corner; a render-only panel, never a script-visible layer or bitmap |
| Graphic cache limit | desktop CLI only | engine default (disabled) | `--graphic-cache-limit none\|0\|BYTES`; not exposed by the Android settings screen yet |
| Engine log level | `off`, `error`, `warn`, `info`, `debug`, `trace` | `info` | how much the engine writes to `krkr.log`; `debug`/`trace` are diagnostic and produce large logs (see §7) |

The debug overlay is off by default and persisted under the `debug_overlay`
key; `KrkrGameActivity` publishes it as `pendingDebugOverlay` and
`crates/android` passes it to `app::HostConfig::with_debug_overlay`. It is a
diagnostic panel only — a render-only node, never a script-visible layer or
bitmap.

The archive-preload switch is the option to weigh on a phone: it is not a debug
mode, it reads the archive into the same storage as before so every entry
returns the same bytes, and the resident-memory cost is the archive's size — a
real and often large number — traded for no longer reading it back from
storage.

The choices are stored as JSON in the app's private files directory —
`<filesDir>/settings.json` — next to `games.json`, following the same
inspectable, atomically-written convention (§3.1):

```json
{ "version": 1, "frame_rate": "60",
  "preload_archives": "false", "debug_overlay": "false", "log_level": "info" }
```

`RuntimeSettings.kt` keeps each value as the exact string the user chose; it
does not validate the value itself. Validation belongs to the read side, exactly
as it does for the desktop `key=value` file.

The configuration path mirrors the desktop launcher's, with the settings file
playing the role of the `KRKR_CONFIG` file:

1. **The settings UI** validates the frame rate (`1..=1000`), the archive
   preload (`true`/`false`) and the debug overlay (`true`/`false`) and writes
   `settings.json` atomically. A value loaded from a hand-edited file that is
   not one of the accepted values is shown and must be corrected before it can
   be saved; the hand-edited value is preserved, not rewritten.
2. **`KrkrGameActivity`** reads `settings.json` in `onCreate`/`onNewIntent`
   *before* `super.onCreate` and copies the four values onto the static fields
   `pendingFrameRate`, `pendingPreloadArchives`,
   `pendingDebugOverlay` and `pendingLogLevel` — the same JNI channel
   `pendingGameDir`/`pendingGameName`/`pendingStateDir` already use.
3. **`crates/android`** reads those fields (`jni_fields.rs`) and resolves them
   (`config.rs`) into a `RuntimeConfig`, applying `false` as the preload and
   debug-overlay defaults. The log level is resolved first,
   before the logger is installed, because it selects the filter; its default
   is `info`. An unparseable or out-of-range value is a typed `ConfigError`
   naming the option and the accepted values; it is written to `krkr_state.json`
   as a failure and the loop is never started — it is never replaced by the
   default.
4. **The shared emulator host** receives the finished value: the
   `crates/android` loop passes the archive-preload choice into
   `app::GameHost::open_with_preload`, the
   debug-overlay choice into `app::HostConfig::with_debug_overlay`, and the
   frame period into its ticker, exactly as the desktop `app::run` does; the
   log level becomes the process logger's filter. The emulator core performs no
   configuration I/O, exactly as on the desktop (`docs/runtime-config.md`).

Because the settings are read at launch, a change applies to the **next** run,
which the screen states.

Two options that have been suggested are deliberately **not** exposed, because
the engine exposes no knob for either: texture resolution and audio buffering.
Adding a control for them would be a stub. They are engine work, not launcher
work — see `docs/runtime-config.md` §3.

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
**touch → mouse**, not a new input model. The reference's Android environment
turns one finger into a virtual mouse
(`reference/cpp/core/environ/cocos2d/MainScene.cpp:1017-1146`); this port does
the same, at the composition root: the state machine is
`crates/android/src/gesture.rs` (pure, host-tested) and the Android motion
events are `crates/android/src/input.rs`. The synthesised events travel the
same host path and the same aspect-correct window→game transform as a real
mouse, so engine-side layer hit-testing, `onMouseDown`/`onMouseUp`,
`onClick`/`onDoubleClick` and the game's own cursor logic keep working
unchanged. A tap a KAG menu would otherwise miss is the symptom this fixes.

The mapping, as implemented:

- **Tap → left click.** A single finger that travels no more than 16 px and is
  released before 1000 ms presses and releases the left button at the touch
  position. The L2 input state turns the press/release pair into
  `onMouseDown`/`onMouseUp` and then `onClick`/`onDoubleClick`.
- **Drag → mouse move with the button held.** Once the finger passes the slop,
  the left button presses and every move is a pointer move until the release.
- **Long press → right click.** A finger held still past 1000 ms (the
  reference's virtual-mouse timeout, `MainScene.cpp:1097-1101`) presses the
  right button, released when the finger lifts. KAG opens its menu on the right
  button and games read it through `System.getMouseButtonState(1)`.
- **Two-finger tap / swipe-down → `Escape`.** Two fingers that both lift
  within 300 ms, or that drag down past 64 px, emit an Escape press/release
  pair — the key the Android back button already maps to (`AKEYCODE_BACK` →
  `PhysicalKey::Escape`, §2.1).
- **Multi-touch must not double-fire.** The raw touch is consumed by the
  synthesizer and is **not** also forwarded as `onTouchDown`/`onTouchMove`/
  `onTouchUp`: a KAG menu reads the mouse family, and sending both would fire
  twice. A second finger that arrives after a drag or a long press has begun is
  ignored, so it cannot turn an active drag into a back gesture.

The 16 px slop is a fixed physical-pixel value. The reference scales the same
threshold by the display DPI (`MainScene.cpp:494-496`); that is a small
deliberate divergence which keeps the synthesizer free of a display service.

**Implemented and host-tested:** the whole state machine and every mapping
above (`crates/android/src/gesture.rs` unit tests). **Not verified on the
host:** that the Android motion actions and timestamps actually reach it in the
assumed order, that the synthesised events hit a real KAG menu, and the
DPI-based slop.

Not done yet:

- **On-screen controls overlay** for keys touch cannot express: at minimum a
  Ctrl (skip) button, a right-click button and a menu/back button. Compose is
  the intended home (M3 styling, no engine changes) — it can sit in the same
  `GameActivity` hierarchy above the surface. There is no control for it today.
- **The Activity-level back gesture.** `android-activity` 0.6 does not surface
  `onBackPressed`, so the only back path is the mapped `AKEYCODE_BACK` key; the
  two-finger gestures above are the in-game substitute. “First press opens the
  menu, a second press exits” is not implemented.

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

**Verified end to end on a machine with the NDK and SDK** (the development
host has neither and does not build the Android target — §2). From a clean
tree:

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
engine writes two files into `<filesDir>` and the launcher reads them:

* `<filesDir>/krkr_state.json` — atomically written
  `starting`/`running`/`failed`/`stopped` plus the real error message and pid;
  the launcher turns it into a dialog with the reason. A crash is recognised by
  a stale `starting`/`running` whose pid is no longer the current process.
* `<filesDir>/krkr.log` — the engine log, and what the launcher's **Logs**
  screen and the dialogs' tail read. `env_logger` writes every record to
  stderr and the same record is appended to this file. Lines are UTF-8, oldest first,
  `"<rfc3339> <LEVEL> <message>"`; the launcher reads the last 400 lines, up
  to 256 KiB, dropping a leading partial line (`LauncherLog.tail`).

The engine's log level is a **launcher setting** (`settings.json` →
`pendingLogLevel` → `crates/android`), not an environment variable: `RUST_LOG`
cannot be set on a device. `RUST_LOG` keeps its precedence on the desktop and
for anyone who can set it; when it is unset the launcher's level (default
`info`) selects the filter. `info` omits the TJS VM's exception detail: the
VM's per-exception disassembly, register dump and console output reach our
logger as `debug` records (`crates/tjs/cpp/tjs2_abi.cpp`
`ConsoleOutputAdapter`; `tjsInterCodeExec.cpp`
`DisplayExceptionGeneratedCode`), so **`debug` is the level that surfaces a
script exception in `krkr.log`**. `debug` (and `trace`) also record per-frame
detail, so the log grows and rotates much sooner; use it to diagnose a
problem, then return to `info`. The vendored VM also has an independent
spdlog logger that writes to stderr (logcat) at a fixed `debug` level and does
not honour the launcher setting; the exception disassembly we need goes
through the `log` callback and does honour it.

The log is capped at **1 MiB per file** and rotated through `krkr.log.1` …
`krkr.log.6`: each rotation shifts the existing files up one slot and drops
the oldest, so the sink holds seven files and about **7 MiB worst case** (a
record larger than the whole cap is still written whole rather than truncated
mid-record, so one oversized record can add its own size once). Six rotations
are kept deliberately. The failure observed on a device repeated a VM exception
dump until 87% of one session's output was the same text; the old
single-rotation pair (2 MiB) could therefore lose the *start* of the very
session that failed, including the original exception. Six rotations hold a few
launches' startup plus the failure cascade at a bounded cost. The file is
opened before the global logger is installed; a log the app cannot open is
returned as a startup error and written to the state file as `failed`, so the
Logs screen is never silently empty with no explanation. The current file
name and the directory are the Kotlin contract
(`EngineDiagnostics.engineLogFile` is `File(filesDir, "krkr.log")`, and
`KrkrGameActivity.pendingStateDir` is `filesDir.absolutePath`); the rotations
are discovered from the directory, not fixed by Kotlin.

An app-private directory cannot be browsed on a device, so the Logs screen
offers two ways off it: the **tail** (copy/share of the last 400 lines plus the
state file, which the crash dialogs also use), and the **full log** — *Share
all* stages the `krkr_state.json` file, **every** engine log the retention
policy currently has on disk, and the launcher's current and rotated log into
`cacheDir` and sends them as `ACTION_SEND_MULTIPLE` attachments with a read
grant on the `FileProvider` URIs, while *Export* writes one combined file
through the system document picker (`ACTION_CREATE_DOCUMENT`) to a location a
PC can reach over USB. The export **enumerates** the engine logs from the files
directory rather than naming a fixed pair, so it cannot drift from the Rust
sink's rotation count. The screen lists each file with its real path and size,
or `not created yet`, shows the `krkr_state.json` content in its own section
(its `message` is the single most useful line for a failed title), and reports
the combined size before the export. The export does not truncate anything: it
writes every file whole, and honesty about a several-MiB export is enough
because it goes to the user's own Downloads or mail, not a server. An absent or
empty file is named when the export runs and never fails it.

The state file's `message` explains a **reported** failure only. A native crash
or a `SIGKILL` leaves the last state the process wrote — `starting` or
`running`, with `message: null` — so the export then tells you the engine never
reported an exit, not why it died; the `krkr.log` history is what covers that
case.

**Host-tested:** the record format, the byte accounting, the multi-step
rotation (the slot order, the dropped oldest, a previous process's file rotated
rather than appended, and the cap holding for every file), reopening a previous
file and the missing-directory error (`crates/android/src/log_file.rs` unit
tests). **Not verified on the host:** that `<filesDir>` is writable by the app
process, that the records appear in `adb logcat` under the `env_logger` format,
that the Logs screen and crash dialog read them back, and the Kotlin export
enumeration (there is no Gradle/SDK on the host — see §7's build note).

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
| M2 | Rust `cdylib` + `android_main` entry point; the engine builds the shared `app::GameHost` | **verified** — links and exports `android_main` |
| M3 | Compose M3 launcher + SAF picker + persisted library; hands the folder to `GameActivity` | **compiles and packages** (never run on a device) |
| M4 | Touch → mouse bridge | **implemented and host-tested** (`crates/android/src/gesture.rs`); on-screen controls and the Activity-level back gesture still open |
| M5 | MediaCodec video backend (replacing FFmpeg on Android) | **compile-verified and symbol-checked**; never run on a device |
| M6 | CI APK build + artifact upload | workflow written, never triggered |
| M7 | Engine writes `<filesDir>/krkr.log` (capped, rotated through `krkr.log.1`–`.6`) for the Logs screen; the full-log export also carries `krkr_state.json` | **host-tested**; never run on a device |

The whole chain now produces an installable APK from a clean tree — see §7 —
but **nothing has been run on hardware**. What that leaves genuinely unproven is
narrow and worth stating plainly: whether the engine starts, decodes and renders
on a real phone; whether the system font directories (`/system/fonts`) are
populated and a requested face resolves — the host test checks the directory
list, not a device's contents; whether MediaCodec's demux/decode loop works;
whether `memfd_create` is permitted by a device's seccomp policy; and whether
touch delivery and coordinate mapping behave as the host tests assume. A
release needs at least one real KiriKiri title run on a real device with its
log captured (`<filesDir>/krkr.log`, §7).

---

## 9. Open questions

The storage, orientation and game-discovery questions are now **decided**
(§1): sideloaded storage via option A, landscape lock, and explicit folders
only — no auto-indexing of a default location.

1. **Resolution/scaling policy** on phones — **decided: always Fit with
   letterbox, no override.** The target is to scale the game uniformly to fit
   the screen and letterbox the remainder, so it keeps its aspect; this is the
   engine's single presentation rule (`docs/migration/11`, D39), not an Android
   default, and the settings screen (§3.2) no longer offers a mode. The engine
   fits the script's declared size into the display-sized surface; the script
   still sees its declared size. The reference's Android build stretches
   instead (`EXACT_FIT`,
   `reference/cpp/core/environ/cocos2d/AppDelegate.cpp:60-70`,
   `MainScene.cpp:436-438`); we choose no distortion.
2. **Audio focus/interruption** (phone call, notification) and lifecycle
   (backgrounding must pause the VM + mixer) — not yet scoped.
3. **Emulator support**: arm64-only means no x86_64 emulator; verification will
   need a physical device (or a temporary x86_64 ABI).
