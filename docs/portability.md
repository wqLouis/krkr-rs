# krkr-rs portability assessment: Android and WebAssembly (wasm32)

Scope: can the current `krkr-rs` workspace (Rust + Bevy 0.19 + a C++ TJS2 VM)
be built and run on Android and in a browser? This document records the
evidence (file:line), cheap cross-compile checks, the hard blockers, and a
prioritized plan for each target.

Method: manifest + source inspection, `cargo tree`, and `cargo check` of the
pure-Rust leaf crates for `wasm32-unknown-unknown` and
`aarch64-linux-android`. No release builds, no toolchain/target installs.
The full game path (`render`/`engine`/`tjs2-sys`) was **not** cross-checked
because `crates/tjs2-sys/build.rs` compiles the C++ VM for the *host*
regardless of `--target` (see §2), which would make a `cargo check` result
misleading; a real cross build is not cheap. That limitation is called out
again in §11.

---

## 0. Effort scale

| Effort | Meaning |
|---|---|
| S | < ~1 day, mechanical/feature-flag work |
| M | a few days, new backend or build wiring |
| L | a week+, structural change or a new subsystem |

---

## 1. Summary table

| Area | Android (`aarch64-linux-android`) | wasm32 (`wasm32-unknown-unknown`) | Effort |
|---|---|---|---|
| Cargo workspace / Bevy features | Builds with Bevy defaults; needs Android entrypoint + packaging | Builds with Bevy defaults; needs wasm entrypoint + web packaging | M |
| C++ TJS2 VM (`tjs2-sys`) | Cross-compile with NDK (zig or NDK clang); build.rs not target-aware yet | **Hard blocker**: no C++ runtime for `wasm32-unknown-unknown`; needs Emscripten/WASI side module | L |
| oniguruma C lib | Cross-compile with NDK; `deps/build.zig` already target-aware | Same C++ toolchain problem | L |
| libopus (`opusic-sys` bundled) | `opusic-sys` has explicit NDK support (`ANDROID_NDK_HOME` + cmake toolchain) | **Hard blocker**: cmake/emscripten not wired; no wasm libopus | M / L |
| Filesystem / storage | `std::fs` works only inside app-private dirs; APK assets need `AssetManager`; scoped storage | **Hard blocker**: no synchronous fs; needs fetch/OPFS/IndexedDB + a storage trait | L |
| Threading / concurrency | Fine; app already multi-threaded; Bevy defaults work | Works single-threaded; Bevy `multi_threaded` degrades; no SharedArrayBuffer needed unless threads/WebAudio worklet | M |
| Audio output (rodio/cpal) | cpal AAudio backend present (`ndk`); Opus cmake is the work | cpal WebAudio backend is available *via Bevy* feature unification; rodio's sync API + Opus remain issues | M / L |
| Rendering / GPU (Bevy/wgpu) | Vulkan/GLES both in wgpu defaults; should work | WebGL2 (Bevy default) suffices for the current shader; no compute used | S |
| Entrypoints / tooling | Needs `android_main`/GameActivity, Gradle project, cargo-ndk | Needs `#[wasm_bindgen(start)]`, `index.html`, trunk/wasm-bindgen-cli | L |
| **Overall** | Feasible but a real port (VM cross-compile + storage + audio + packaging) | Very hard today because of the C++ VM + sync filesystem | L / L |

Areas with **no blocker found**: pure-Rust logic crates, xp3 parsing, TLG/image
decode, text layout, the WGSL blend material (WebGL2-compatible, §9).

---

## 2. Workspace layout and dependencies

Workspace `Cargo.toml`:
- Members: 19 crates (`Cargo.toml:3`); `default-members = ["crates/render", "crates/app"]`
  (`Cargo.toml:4`).
- Release profile uses thin LTO + `codegen-units = 1` (`Cargo.toml:12-15`) —
  irrelevant to cross-compilation, but note `lto` interacts with `opusic-sys`'s
  cmake LTO handling (see §6).

Bevy:
- `crates/render/Cargo.toml:16` → `bevy = "0.19"` with **default features**
  (no `default-features = false`).
- Bevy 0.19.1 defaults are `default = ["2d", "3d", "ui", "audio"]`
  (`bevy 0.19.1` manifest, `[features]`). `default_platform` includes
  `multi_threaded`, `bevy_winit`, `bevy_gilrs`, `webgl2`, `x11`, `wayland`,
  `sysinfo_plugin`, `default_font` (same manifest). So the current app pulls
  Winit, the render stack, and Bevy audio/rodio.
- `crates/render/Cargo.toml` also declares `crates/render/src/main.rs` as the
  `krkr-rs` bin and exports a `krkr_render` lib (`crates/render/src/lib.rs`).

Crates that are hard to cross-compile (contain or depend on native code):
- `crates/tjs2-sys` — C++ TJS2 VM + libonig (§3).
- `crates/tvp-sound` — `symphonia-adapter-libopus` → `opusic-sys` bundled
  (C libopus via cmake), and `rodio` → `cpal` (`crates/tvp-sound/Cargo.toml:18-20`).
- `crates/tvp-archive` — `zip`/`sevenz-rust`/`tar`
  (`crates/tvp-archive/Cargo.toml:9-11`) pull `zstd-sys`, `liblzma-sys`,
  `bzip2` C libs and `getrandom`. **This crate is not referenced by any other
  workspace crate** (grep for `tvp_archive`/`tvp-archive` outside its own
  directory returns nothing) and is not a default member, so it does not affect
  the current game binary — but it will affect any "full workspace" build.
- `crates/tvp-visual` depends on `tempfile` as a **normal** dependency
  (`crates/tvp-visual/Cargo.toml:15`) even though it is only used from
  `#[cfg(test)]` (`crates/tvp-visual/src/natives/mod.rs:275,279`); move it to
  `[dev-dependencies]` to avoid dragging fs/rand code into non-test wasm builds.

Pure-Rust crates (fine to cross-compile in principle): `xp3`, `kag`,
`tvp-util`, `tvp-streams`, `tvp-config`, `tvp-text`, `engine` (once `tjs2-sys`
links), `tvp-input`, `tvp-natives`, `tvp-scripts`, `tvp-storages`,
`tvp-kagparser`.

---

## 3. Native / C++ code — the central blocker

### 3.1 `tjs2-sys` build

`crates/tjs2-sys/build.rs` does **not** use `cmake` or the `cc` crate. It:
1. runs `zig build` in `deps/` to fetch fmt/spdlog/boost headers and build
   `libonig.a` (`build.rs:122-128`; `deps/build.zig` builds ~55 oniguruma C
   sources into a static lib);
2. generates bison parsers (`build.rs:173-176`, `BISON_GRAMMARS` at `:72`) and
   a Python date map (`build.rs:184-190`);
3. compiles 35 tjs2 `.cpp` files + `cpp/tjs2_abi.cpp` + `cpp/streams.cpp`
   (`build.rs:37-69`, `:237-244`) by spawning `zig c++` **without any target
   flag** (`build.rs:149-153`, `:254-278`, flags `-fPIC -pthread` at
   `:200-201`);
4. archives into `libtjs2_core.a` with host `ar` (`build.rs:297-299`) and emits
   `cargo:rustc-link-lib=static=tjs2_core` + `onig` (`build.rs:302-308`);
5. only for `env::consts::OS == "linux"` links `c++`/`c++abi` and `-pthread`
   (`build.rs:309-313`).

There is **no `TARGET`/`--target`/`--sysroot` handling anywhere in build.rs**
(grep for `TARGET`/`--target` finds none). Consequently a `cargo check
--target <anything>` still compiles an *x86_64-linux* C++ archive; only the
final Rust link would reveal the mismatch. Under `cargo check` (no link) it can
even appear to "succeed". This is why the full game path is untested here.

The vendored C++ uses `std::mutex`, `std::thread::id`, `std::this_thread`
(`crates/tjs2-sys/cpp/tjs2/tjsUtils.cpp:14-38`,
`cpp/tjs2/tjsInterCodeExec.cpp:476-498`) and `<cstdio>` file I/O in the save
path (`crates/tjs2-sys/cpp/streams.cpp:21,63,152`). It needs a full C++
runtime (libc++/libc++abi), RTTI, exceptions, and thread support.

### 3.2 oniguruma

`deps/build.zig` uses `b.standardTargetOptions` and computes target-aware
`config.h` sizes, so it *can* be told to cross-compile (`-Dtarget=...`), but
`build.rs` never passes a target (`build.rs:126`). Oniguruma also needs a libc
for the target (Android NDK sysroot, or WASI/Emscripten for wasm).

### 3.3 libopus

`crates/tvp-sound` depends on `symphonia-adapter-libopus = "0.3"`
(`crates/tvp-sound/Cargo.toml:19`), whose default feature is
`bundled = ["opusic-sys/bundled"]` (crate manifest). `opusic-sys 0.7.5`'s
`build.rs` uses the `cmake` crate to build the vendored Opus tree
(`~/.cargo/registry/.../opusic-sys-0.7.5/build.rs`, `cmake::Config::new("opus")`).
It has explicit Android handling: if `ANDROID_NDK_HOME` is set it points cmake
at `build/cmake/android.toolchain.cmake` and maps the Rust target to an
`ANDROID_ABI` (`aarch64-linux-android` → `arm64-v8a`). There is **no
emscripten/wasm path** in that build script.

---

## 4. Platform assumptions, grouped by crate

> "std::fs" below means synchronous host filesystem calls; there is no
> abstraction trait in front of them.

**engine (`src/storage.rs`)**
- `use std::fs` and `fs::read_dir` to enumerate `*.xp3` (`storage.rs:4,116`).
- `Storage` is a concrete struct holding `PathBuf` + `Vec<(PathBuf, Xp3Archive)>`
  (`storage.rs:66-74`); `read()` does `fs::read(path)` for disk entries
  (`storage.rs:210`); `stat()` uses `fs::metadata` (`storage.rs:240`).
- case-insensitive disk walk uses `fs::read_dir` recursively (`storage.rs:47-70`).
- No `#[cfg(target_os)]` anywhere in `engine`.

**xp3 (`src/archive.rs`)**
- `Xp3Archive` stores a live `std::fs::File` and reads/`seek`s it
  (`archive.rs:4,38,63,99+`). No in-memory/stream backend.

**render (`src/main.rs`, `src/sync.rs`, `src/input_bridge.rs`)**
- `std::env::args` for CLI parsing (`main.rs:44`), `std::process::exit` on error
  and headless success (`main.rs:57,63,211,245,264,389`), `std::env::temp_dir()`
  for `app_data_dir` (`main.rs:233`), `std::fs::create_dir_all(.../savedata)`
  (`main.rs:217`), `std::thread::current().id()` to bind the audio stream to
  the main thread (`main.rs:273`), a process-wide `static VM_RUN_LOCK`
  (`main.rs:319-330`), and `std::thread::sleep` in an ignored test
  (`main.rs:889`).
- Bevy input only: `ButtonInput<MouseButton/KeyCode>`, `CursorMoved`,
  `MouseWheel`, `Window` (`input_bridge.rs:32-37`). No raw-window-handle /
  winit API usage in our crates (grep for `winit`/`raw_window_handle` outside
  Cargo.lock returns nothing).

**app (`src/main.rs`)**
- `std::env::args` (`app/src/main.rs:13`), `std::fs::create_dir_all(.../savedata)`
  (`:113`), `std::env::temp_dir()` (`:116`), and `process::exit` (`:37`).

**tvp-natives**
- `System.shellExecute` uses `std::process::Command` (Linux `xdg-open`,
  Windows `cmd`, macOS `open`; `system.rs:63,191-206`).
- `SystemContext::default` uses `std::env::current_dir` (`system.rs:709`);
  `app_data_dir` is supplied by the host (`render/src/main.rs:233`,
  `app/src/main.rs:116`).
- `Debug.startLogToFile` opens a host file with `OpenOptions` (`debug.rs:33,164`).

**tvp-storages**
- `Storages.deleteFile` → `std::fs::remove_file` (`storages/src/lib.rs:1014`),
  `Storages.copyFile` → `std::fs::copy` (`:1076`), directory listing →
  `std::fs::read_dir` recursion (`:182`).

**tvp-streams**
- A genuine `BinaryStream` trait exists (`binary.rs:57`) with impls for
  `File` (`:291`) and `MemoryStream` (`:396`) — but `engine::Storage` and
  `Xp3Archive` do **not** use it.

**tvp-text**
- `FontFace::from_path` → `std::fs::read` (`font.rs:108`); a fixed list of
  Linux Noto paths (`font.rs:82-84`) and `fontdb` with `fs` feature
  (`Cargo.toml:10`). `tvp-visual` also reads `KRKR_RS_SYSTEM_FONT`
  (`natives/layer.rs:1224`).

**No occurrences** of `dirs`, `home`, `getrandom` (direct), `instant`,
`web-sys`, `wasm-bindgen`, `js-sys`, `#[cfg(unix)]`, or `#[cfg(target_os = ...)]`
(other than the `cfg!(target_os=...)` string checks in `system.rs:191-196`)
anywhere in `crates/`. There are **no** Android or wasm entrypoints:
grep for `wasm_bindgen`, `#[wasm`, `android_main`, `#[no_main]`, `JNI_OnLoad`
returns nothing.

Absolute paths appear only in tests/ignored tests and docs (e.g.
`engine/src/storage.rs:314`, `render/src/main.rs:792,867`,
`tvp-text/src/font.rs:82-84`). No absolute desktop path is used on the normal
runtime path except the Noto font fallback.

---

## 5. Storage / IO abstraction assessment

Current design is **not** abstracted:
- `engine::Storage::mount` scans a real directory with `fs::read_dir`
  (`storage.rs:116`), and `find()` joins `game_dir` with names and calls
  `Path::is_file()` (`storage.rs:157-201`).
- `Storage::read` returns bytes from `fs::read` or `Xp3Archive::read`
  (`storage.rs:207-218`).
- `Xp3Archive` keeps an open `std::fs::File` (`xp3/src/archive.rs:38,63`) and
  seeks/reads it for each segment.

What a port needs:
- A `StorageBackend` trait with sync-ish operations (`read`, `stat`, `exists`,
  `list`, and a read/write stream for save data). The existing `BinaryStream`
  trait (`tvp-streams/src/binary.rs:57`) is the natural base; `Xp3Archive`
  should be reworked to parse from a `Read + Seek` source (or a fully buffered
  `Vec<u8>`) instead of `std::fs::File`.
- **wasm**: no synchronous filesystem in a browser. Practical approach:
  1. `fetch()` the game's `data.xp3` (+ patch) into `Vec<u8>` at startup and
     mount an in-memory archive (this also removes the `File` dependency from
     `Xp3Archive`); 2. implement save-data writes over OPFS or IndexedDB. All
     of this needs an async bootstrap before `engine::loader::prepare`, or a
     JS-provided in-memory image.
- **Android**: game data typically lives outside the APK if the user copies it
  to app storage (then `std::fs` works for app-private dirs), but data bundled
  in the APK is reachable only through `AAssetManager` (`ndk` crate) and is not
  a real path. Save data should go to the app's internal/external files dir,
  not `env::temp_dir()`.

---

## 6. Threading / concurrency

- The VM is single-threaded by design: `tjs2-sys` documents this and only makes
  the handle `Send`/`Sync` via `unsafe impl` (`crates/tjs2-sys/src/lib.rs:311-314,
  339-340`), with per-crate `VM_LOCK` test mutexes.
- The Bevy app serializes the whole VM step with `static VM_RUN_LOCK`
  (`render/src/main.rs:319-330`) because Bevy's scheduler may run systems on
  worker threads.
- Shared state is `Arc<RwLock<Scene>>` / `Arc<Mutex<Storage>>`
  (`render/src/sync.rs:85`, `render/src/main.rs:286-287`), plus process-global
  `Mutex`/`OnceLock` registries in the native crates
  (`tvp-input/src/lib.rs:405`, `engine/src/storage.rs:15`,
  `tvp-kagparser/src/lib.rs:130`).
- Audio output is created/dropped on the main thread and wrapped in a type with
  `unsafe impl Send/Sync` (`tvp-sound/src/player.rs:200-210`); the mixer itself
  is pull-based (no app-owned thread) — grep for `thread` in
  `tvp-sound/src/mixer.rs` finds none.
- `tjs2-sys/build.rs` uses `std::thread::spawn` during the **build**
  (`build.rs:254`), which is host-side only.

Impact:
- **wasm single-threaded (default, no SharedArrayBuffer)**: `Mutex`/`RwLock`
  work (they just never contend), `Arc` works, and `VM_RUN_LOCK` is a no-op in
  practice. Bevy's `multi_threaded` default is expected to fall back to a
  single-threaded task pool, but this should be verified. No `wasm-threads`
  requirement for the core loop.
- **Android**: threading is already correct for a multi-threaded OS; nothing to
  change beyond making sure the VM step stays serialized (already done).
- **wasm + WebAudio/audioworklet**: cpal's `audioworklet` backend requires
  `target_feature = "atomics"` and cross-origin isolation (COOP/COEP). The
  default cpal wasm path uses the regular `AudioContext` (no COEP needed), so
  the simple path is fine. Only pick the worklet path if low latency is
  required.

---

## 7. Audio

Decode path:
- `symphonia 0.6` direct (`tvp-sound/Cargo.toml:18`), with a global registry
  that registers `symphonia_adapter_libopus::OpusDecoder`
  (`tvp-sound/src/decode.rs:85-86`). Opus is the voice format.
- If Opus decode fails there is already a silent fallback that returns ~60 ms
  of silence so sequencing still works (`decode.rs:92-113`).

Output path:
- `rodio 0.22` with features `playback, symphonia-wav/vorbis/flac/mp3`
  (`tvp-sound/Cargo.toml:20`). `start_output` calls
  `DeviceSinkBuilder::open_default_sink()` (`player.rs:153-154`); the sink is
  `!Send`/`!Sync` and is wrapped for Bevy (`player.rs:200-210`).
- cpal's own Android backend is present in the dependency graph: cpal 0.17.3
  declares `[target.'cfg(target_os = "android")'] ndk` with features
  `audio, api-level-26` (crate manifest), i.e. AAudio.
- cpal's wasm backend is **feature-gated**: `host/mod.rs` only compiles
  `webaudio` under `all(target_arch = "wasm32", feature = "wasm-bindgen")`
  (cpal crate manifest); without it cpal compiles the `null` backend.
  `tvp-sound` does **not** enable `rodio/wasm-bindgen`, **but Bevy does**:
  `bevy_audio 0.19.1` has a `cfg(target_arch = "wasm32")` rodio dependency with
  features `["wasm-bindgen", "playback"]` (crate manifest). Because Bevy is a
  default dependency of `render`, Cargo feature unification enables
  `cpal/wasm-bindgen` for the whole graph. So WebAudio should be reachable
  without touching `tvp-sound/Cargo.toml` — but this is fragile (it depends on
  Bevy audio staying enabled) and should be made explicit.

Cross-compile needs:
- **Android / Opus**: `opusic-sys` supports it via `ANDROID_NDK_HOME` (its
  `build.rs` maps targets to ABIs). Requires the NDK and cmake; `cargo-ndk`
  supplies the env. Effort M.
- **wasm / Opus**: no wasm branch in `opusic-sys/build.rs`; cmake would build
  for the host or fail. Options: (a) build libopus for wasm with Emscripten/WASI
  and link it (`default-features = false` + `OPUS_LIB_DIR`/`OPUS_LIB_STATIC`,
  per that build script); (b) ship without Opus and rely on the silent fallback
  (`decode.rs`) — voices won't play but sequencing won't stall. There is no
  pure-Rust Opus decoder in the current tree. Effort L for full audio.
- **wasm / rodio sync API**: `DeviceSinkBuilder::open_default_sink()` is called
  from the app on startup; on wasm that must run inside a user-gesture-driven
  `AudioContext.resume()` flow. The current code opens it directly from Bevy
  startup (`render/src/main.rs:273`), which browsers may reject until a user
  interacts. Needs an explicit "click to start audio" glue. Unknown how much
  rodio 0.22's wasm path already handles.

---

## 8. Entrypoints and build tooling

Current entrypoints:
- Native binary: `crates/render/src/main.rs` (`fn main`, windowed `App` via
  `DefaultPlugins` at `:129-176`, or `MinimalPlugins` headless at
  `:178-198`). CLI-only headless tool: `crates/app/src/main.rs`
  (`krkr-cli`); the graphical runner's `krkr-rs run <game> --headless` is the
  diagnostic path.
- No `#[wasm_bindgen(start)]` lib entry and no `android_main`/GameActivity
  entry exist (grep, §4).
- `krkr_render` is already a lib (`render/src/lib.rs`) that exposes
  `sync_scene`, `SharedScene`, `LayerBlendPlugin`, etc., so a wasm/Android
  entry can reuse it.

Tooling present on this machine (verified):
- `zig 0.16.0`, `cmake 4.4.3`, rustup targets `wasm32-unknown-unknown`,
  `wasm32-wasip2`, `aarch64-linux-android`, `armv7-linux-androideabi`,
  `i686-linux-android`, `x86_64-linux-android`.
- **Missing**: `cargo-ndk`, `gradle`, `wasm-bindgen`, `wasm-pack`, `trunk`,
  `emcc`/`emcmake`. There is no `Trunk.toml`, no `index.html`, no
  `build.gradle`, no `.cargo/config.toml`, no CI YAML — only
  `scripts/vendor-tjs2.sh` and `scripts/extract_xp3.py`.

What each target additionally needs:
- **Android**: `android_main` (`#[cfg(target_os = "android")]`) or Bevy's
  GameActivity integration; a Gradle project with `AndroidManifest.xml`,
  `cargo-ndk` (or a custom linker config), the NDK sysroot wired into
  `tjs2-sys`/onig/opus, and asset packaging for the game.
- **wasm**: `wasm-bindgen-cli` (matching the `wasm-bindgen` crate version),
  a `#[wasm_bindgen(start)]` entry, `index.html` + a bundler (`trunk` or
  `wasm-pack`), async storage bootstrap, and a user-gesture audio start.

---

## 9. Rendering / GPU

- No custom `RenderPlugin`/backend selection exists; the app uses Bevy defaults
  (`render/src/main.rs`), so backends come from wgpu's defaults
  (`dx12, metal, gles, vulkan, wgsl, webgpu` per wgpu 29 manifest).
  - Android: Vulkan and GLES are both available.
  - wasm: Bevy's `webgl2` feature (in `default_platform`) selects the WebGL2
    path; enabling Bevy's `webgpu` feature would switch to WebGPU.
- The scene sync creates `Image`s as `Rgba8UnormSrgb` 2D textures
  (`render/src/sync.rs:130-143`) and `Camera2d` + `Sprite`/`Mesh2d`
  (`sync.rs:68,74,272,343`). This is WebGL2-compatible.
- Custom material: `LayerBlendMaterial` (`render/src/blend.rs:181-244`) with an
  embedded fragment shader `layer_blend_material.wgsl` that only samples a
  `texture_2d` + `sampler` and returns `color * sample`
  (`layer_blend_material.wgsl`). Compositing is done with fixed-function
  `BlendState` selected per variant (`blend.rs:94-135,246-263`).
  - `SourceOver`/`Replace`/`Additive` use `FUNC_ADD`; `Subtractive` uses
    `BlendOperation::ReverseSubtract` (`blend.rs:125-135`), which maps to
    `GL_FUNC_REVERSE_SUBTRACT` — available in WebGL2. So the shader/blend set
    should work on WebGL2.
- There is **no compute shader / storage texture / GPU-driven** feature in the
  render crate (grep for compute finds only comments). No obvious WebGL2
  limitation.
- Risk to verify on device/browser: Bevy's default stack also includes `3d`,
  `ui`, and `bevy_gilrs` (gamepad); those are unnecessary and could pull
  platform-specific code. Consider trimming Bevy features to the 2D subset
  (`bevy = { version = "0.19", default-features = false, features = [...] }`).

---

## 10. Cheap cross-compile checks (executed today)

Toolchain: `rustc 1.97.1`, `cargo 1.97.1`, `zig 0.16.0`.

### 10.1 `wasm32-unknown-unknown`

Passed (pure Rust, full dependency graphs for these crates):

| Crate | Result |
|---|---|
| `tvp-util` | `Finished` |
| `xp3` | `Finished` |
| `kag` | `Finished` |
| `tvp-streams` | `Finished` |
| `tvp-config` | `Finished` |
| `tvp-text` | `Finished` (ab_glyph/fontdb/ttf-parser compile) |

Failed:

- `cargo check -p tvp-archive --target wasm32-unknown-unknown` →
  ```
  error: The wasm32-unknown-unknown targets are not supported by default; you may
  need to enable the "wasm_js" configuration flag. Note that enabling the
  `wasm_js` feature flag alone is insufficient. For more information see:
  https://docs.rs/getrandom/0.3.4/#webassembly-support
     --> .../getrandom-0.3.4/src/backends.rs:194:17
  error: could not compile `getrandom` (lib) due to 1 previous error
  ```
  Reverse dependency: `getrandom v0.3.4 ← zip v4.6.1 ← tvp-archive`
  (`cargo tree -p tvp-archive --target wasm32-unknown-unknown -i getrandom@0.3.4`).
  Note `tvp-archive` is unused by the game crates today (§2), so this does not
  block the current binary.

### 10.2 `aarch64-linux-android`

Passed: `tvp-util`, `xp3`, `kag`, `tvp-streams`, `tvp-config`, `tvp-text`.

Failed:

- `cargo check -p tvp-archive --target aarch64-linux-android` →
  ```
  error occurred in cc-rs: failed to find tool "aarch64-linux-android-clang":
  No such file or directory (os error 2)
  error: failed to run custom build command for `zstd-sys v2.0.16+zstd.1.5.7`
  error: failed to run custom build command for `liblzma-sys v0.4.8`
  ```
  (`zstd-sys` and `liblzma-sys` are transitive through `tvp-archive`.)
  Expected fix: point cargo at the NDK clang (e.g. via `cargo-ndk`), which is
  the same mechanism the whole port needs.

### 10.3 Not tested (and why)

- `render` / `engine` / `tvp-sound` / `tvp-visual` cross-checks were **not**
  run: each pulls `tjs2-sys`, whose `build.rs` compiles the C++ VM for the host
  regardless of `--target` (§3.1), so `cargo check` (no link) is not
  authoritative, and a linking build is not a "cheap" check. Treat the C++ and
  libopus cross-compilation as **unverified** until the build wiring is fixed.

---

## 11. Prioritized task list — Android ("runs the game on Android")

Ordered so that each step is independently testable. Files/crates to touch are
named, with why.

1. **Trim Bevy features** (`crates/render/Cargo.toml:16`). Pick an explicit 2D
   feature set (`default-features = false`, keep `bevy_winit`, `bevy_render`,
   `bevy_sprite`, `bevy_asset`, `bevy_audio`/audio format features, `png`, and
   the Android activity feature). Why: drop `3d`, `ui`, `bevy_gilrs`,
   `sysinfo_plugin`, desktop-only `x11`/`wayland`, and reduce the dependency
   surface before touching the VM/audio. Effort S.
2. **Add an Android entrypoint** (new `crates/render/src/main.rs` branch or a
   new `src/bin/android.rs`/`src/android.rs`). Use `#[cfg(target_os =
   "android")]` + a `android_main` (or Bevy's GameActivity plugin) that builds
   the same `App` as `game_app` (`render/src/main.rs:129-176`). Replace
   `std::env::args`/`std::process::exit` for this path. Effort M.
3. **Cross-compile the C++ VM** (`crates/tjs2-sys/build.rs`). Teach `build.rs`
   to read `TARGET` and pass the right compiler/target to `zig c++` (or set
   `CXX` to the NDK `clang++` and use `-target`/`--sysroot`), compile
   `deps/build.zig` with `-Dtarget=<TARGET>` (add the missing flag at
   `build.rs:122-128`), and link the NDK C++ runtime (`c++_shared` /
   `c++_static` instead of the Linux-only `-lc++ -lc++abi` branch at
   `build.rs:309-313`). Keep bison/python/bison host-side. Why: this is the
   make-or-break for Android. Effort L.
4. **Cross-compile oniguruma** (same `build.rs` target wiring; `deps/build.zig`
   already uses `standardTargetOptions` but the sysroot must be supplied).
   Effort M (mostly falls out of step 3).
5. **Cross-compile libopus** (`crates/tvp-sound`, `opusic-sys`). Set
   `ANDROID_NDK_HOME` and the NDK platform, build through `cargo-ndk`, or
   switch `symphonia-adapter-libopus` to `default-features = false` and link a
   prebuilt NDK libopus via `OPUS_LIB_DIR`/`OPUS_LIB_STATIC`. Effort M.
6. **Storage backend for Android** (`engine/src/storage.rs`,
   `xp3/src/archive.rs`, `tvp-storages/src/lib.rs`). Introduce a
   `StorageBackend` trait and an `Xp3Archive` that reads from a `Read + Seek`
   source; provide an `std::fs` backend (works for user-copied games in
   app-private dirs) and an `AssetManager` backend for APK-bundled assets. Route
   save data to the app files dir instead of `env::temp_dir()`
   (`render/src/main.rs:217,233`, `app/src/main.rs:113,116`). Effort L.
7. **Audio/open the output after the platform is ready** (`render/src/main.rs:273`,
   `tvp-sound/src/player.rs:153`): keep creating/dropping on the Android main
   thread, and surface failures as non-fatal (already the case). Effort S.
8. **Packaging/tooling**: add a Gradle project + `AndroidManifest.xml`, wire
   `cargo-ndk`, decide how the game ships (external storage vs APK assets), and
   handle `android.permission`/scoped storage. Effort L.
9. **Device test**: verify Vulkan/GLES selection, window/input, timers, and
   that `std::process::exit` call sites are replaced by `AppExit`
   (`render/src/main.rs:57,63,211,245,264,389`). Effort M.

## 12. Prioritized task list — wasm32 ("runs in a browser")

1. **Scaffold the web target** (`crates/render`): add a `#[wasm_bindgen(start)]`
   lib entry that builds the same `App` as `game_app`; add `index.html` and a
   bundler config (Trunk or wasm-pack); install `wasm-bindgen-cli` matching the
   crate version. Keep the native `main.rs` behind `#[cfg(not(target_arch =
   "wasm32"))]`. Why: you cannot even load the app without this. Effort M.
2. **Replace process/CLI assumptions on wasm**: `std::process::exit`
   (`render/src/main.rs` and `crates/app`), `std::env::args` (no args in a
   browser), and `std::env::temp_dir`/`savedata` creation. Drive game selection
   from a JS-provided path/URL or a bundled game. Effort M.
3. **Fix the unconditionally-built crates**: move `tempfile` to
   `[dev-dependencies]` in `crates/tvp-visual/Cargo.toml:15`; if the full
   workspace is built for wasm, gate or fix `tvp-archive`'s `getrandom`
   (`zip`'s dependency) with the `wasm_js` cfg/feature or exclude it. Effort S.
4. **Web audio path**: make `cpal/wasm-bindgen` explicit rather than relying on
   Bevy feature unification (`crates/tvp-sound/Cargo.toml:20`), and gate audio
   start behind a user gesture (browsers block autoplay); verify rodio 0.22's
   wasm behavior for `DeviceSinkBuilder::open_default_sink()`. Effort M.
5. **Opus decision**: either build libopus for wasm and link it
   (`symphonia-adapter-libopus` `default-features = false` + `OPUS_LIB_DIR`) or
   accept the existing silent fallback (`tvp-sound/src/decode.rs:92-113`) for a
   first browser milestone. Effort L for full audio, S for the fallback.
6. **Storage backend for wasm** (`engine/src/storage.rs`,
   `xp3/src/archive.rs`): add a `fetch`-based in-memory backend that downloads
   `.xp3` into `Vec<u8>` before `engine::loader::prepare`, and an OPFS/IndexedDB
   save backend. This requires making the loader/bootstrap async or
   JS-driven. Effort L.
7. **The TJS2 VM** (the hard blocker — see §13). Without a solution here, none
   of the above produces a runnable game. Effort L.
8. **Verify Bevy/wgpu on WebGL2**: confirm the app boots under WebGL2 (Bevy
   `webgl2` default) with the custom material, or enable Bevy's `webgpu`
   feature. The shader itself is simple (§9), so this is expected to be low
   risk. Effort S.

## 13. Hard blockers and viable strategies

### B1. C++ TJS2 VM (`tjs2-sys`) — the biggest blocker

**Android**: solvable with NDK cross-compilation. Make `build.rs` target-aware
(today it never reads `TARGET`: `build.rs:122-153,254-278`), cross-compile
oniguruma via `deps/build.zig`'s `-Dtarget`, and link the NDK C++ runtime. The
C++ sources use `std::mutex`/`std::thread::id`/`<cstdio>`
(`cpp/tjs2/tjsUtils.cpp`, `cpp/streams.cpp`), which the NDK provides. Risk:
zig's bundled libc++ vs NDK libc++/sysroot mismatches; using the NDK clang++
directly (`CXX`) is the lower-risk route. Effort L, but well-trodden.

**wasm**: genuinely hard. `wasm32-unknown-unknown` has **no libc/C++ runtime**,
and `build.rs` targets the host. Options, best to worst:
1. **Emscripten side module**: compile tjs2 + onig + libc++ into a separate
   Emscripten wasm module and have the Rust `wasm-bindgen` app call it through
   JS glue. This preserves the existing VM but requires re-plumbing the C ABI
   across the JS boundary (the current ABI passes raw pointers and callbacks,
   `cpp/tjs2_abi.h`). Large but realistic. Requires `emcc`/`emcmake` (not
   installed).
2. **WASI static library + shims**: compile tjs2/onig with a WASI toolchain
   (`wasm32-wasip1`, which ships libc++), then link into a `wasm32-wasip1`
   Rust target. This conflicts with Bevy/winit/wgpu, which expect
   `wasm32-unknown-unknown`; mixing WASI imports into the browser build needs
   shims. High risk.
3. **Port TJS2 to Rust** or precompile scripts to a pure-Rust bytecode VM:
   the most invasive, essentially a rewrite. Not recommended as a first step.
4. **Defer the browser target** until a decision on 1 vs 2 is made.

This assessment does not claim any of these is proven; they are the only
plausible routes given the current architecture.

### B2. libopus (`opusic-sys` bundled cmake)

- **Android**: supported by the crate's own build script via `ANDROID_NDK_HOME`
  + cmake NDK toolchain (see §3.3); wire it with `cargo-ndk`. M.
- **wasm**: no wasm branch. Build libopus for wasm (Emscripten/WASI) and link
  via `OPUS_LIB_DIR`/`OPUS_LIB_STATIC` with `default-features = false`, or ship
  without Opus using the existing silent fallback (`decode.rs:92-113`). L / S.

### B3. Filesystem

`engine::Storage` and `Xp3Archive` hardcode `std::fs` and a live `File`
(`engine/src/storage.rs:4,116,210`; `xp3/src/archive.rs:4,38,63`).
- Introduce a `StorageBackend` trait (the `tvp-streams` `BinaryStream` trait at
  `binary.rs:57` is a ready-made starting point) and make `Xp3Archive` read from
  `Read + Seek` / a `Vec<u8>`.
- **wasm**: fetch `.xp3` into memory; OPFS/IndexedDB for saves; async bootstrap.
- **Android**: `std::fs` for user-copied app-private games; `AssetManager` for
  APK-bundled assets; app files dir for saves.
Effort L on both.

### B4. Threading

- No fundamental blocker: the locks are already no-ops in a single-threaded
  environment, and the current entrypoints spawn no application threads (only
  the C++ build script runs host-side).
- **wasm**: keep Bevy single-threaded (or verify its fallback); only enable
  `wasm-threads`/SharedArrayBuffer if the audioworklet path is needed.
- **Android**: already multi-threaded; nothing structural.
Effort M (verification + config).

### B5. Process/CLI assumptions

`std::process::exit` and `std::env::args` are used on the normal game path
(`render/src/main.rs:44,57,63,211,245,264,389`). They must become `AppExit` and
a platform-supplied game handle for wasm/Android. Small but pervasive. S/M.

---

## 14. Unknowns / explicitly not verified

- Whether `render`/`engine`/`tvp-sound` actually cross-compile after the build
  wiring is fixed — not tested (see §10.3).
- Whether zig `c++` can target Android with the installed zig 0.16.0 without
  `ZIG_LIB_DIR`/NDK sysroot tweaks; not tested.
- Whether Bevy's `multi_threaded` default cleanly falls back on wasm without
  `wasm-threads`; not tested.
- Whether rodio 0.22 `DeviceSinkBuilder::open_default_sink()` works under
  `wasm32-unknown-unknown` + cpal `webaudio` without changes; not tested.
- Whether Bevy's `bevy_gilrs`/`sysinfo_plugin`/`3d` default features compile
  for wasm/Android as-is (untested; trimming them is recommended regardless).
- Android `std::fs` behavior under scoped storage for the intended game
  layout; depends on where the game is shipped.
- `tjs2-sys/cpp/streams.cpp` save/load uses `fopen` directly
  (`streams.cpp:63,152`); it would need a backend hook for both targets beyond
  the Rust `Storage` layer.
