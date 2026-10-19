# Runtime configuration

Status: implemented. Related: [`migration/09-game-loading.md`](migration/09-game-loading.md)
(launch arguments), [`android.md`](android.md) (the Android frontend).

The engine is configured with one explicit value,
`api::config::RuntimeConfig`, resolved with the precedence

```
launch argument  >  the launcher's configuration store  >  built-in default
```

**The launcher owns configuration loading.** It parses its own store, merges
the launch arguments over it, and passes the finished `RuntimeConfig` to
`app::run` as a function argument. The emulator core never reads a
configuration file, an environment variable or a platform settings store, so
the same entry point serves every frontend:

```rust
// crates/app/src/main.rs — the desktop CLI
let file = load_desktop_config()?;                 // KRKR_CONFIG, `key=value`
let runtime = resolve_runtime_config(&cli, file);  // arguments over file over defaults
app::run(Launch {
    runtime,
    backend: cli.backend,
    game: resolve_game(&cli)?,
    mode: if cli.headless { RunMode::Headless { frames: cli.frames } } else { RunMode::Windowed },
})?;
```

```rust
// crates/android — the Android launcher, the same entry point
// The player's stored choices (frame rate, archive preload) are read from the
// launcher's own settings file, resolved over the Android defaults, and the
// finished value is handed to the shared host. An unparseable or
// out-of-range value is a ConfigError naming the option and its range; it is
// never defaulted away.
let log_level = config::resolve_log_level(stored_log_level)?; // selects the logger filter
let runtime = config::resolve_runtime_config(
    stored_frame_rate,
    stored_preload_archives,
    stored_debug_overlay,
)?;
let handler = host::AndroidHost::new(
    BackendKind::Vulkan,
    game_dir,
    runtime.preload_archives,
    runtime.debug_overlay,
);
host::run_loop(app, handler, Duration::from_secs_f64(runtime.frame_rate_limit.period_secs()));
```

The reason is that **where configuration comes from is a platform concern**,
not an engine one. A desktop build reads a file; an Android build reads its
own storage; a build with no persistent configuration just uses the defaults.
Keeping that choice in each launcher means the core contains no
`#[cfg(target_os = ...)]`, no configuration path and no loading trait — it
only acts on the value it is handed. A launcher still needs the shared value
types and the `key=value` parser, so `api::config` provides `RuntimeConfig`,
`FrameRateLimit`, `parse_overrides`, the partial
`RuntimeConfigOverrides` shape and the typed `ConfigError`.

Every field is validated. An unparseable or out-of-range value is a typed
`ConfigError` naming the option and the accepted range, never a clamped or
defaulted value. Nothing takes effect until the whole configuration —
arguments *and* the launcher's store — validates.

## 1. The desktop file

The desktop launcher reads the `key=value` file named by the `KRKR_CONFIG`
environment variable (unset → no file → defaults). This loader lives in
`crates/app/src/main.rs`, next to the argument parser that shares its
precedence; the emulator core does not know it exists. `crates/app` is allowed
to read files (only the compatibility layer is barred from `std::fs`).

A file that is named but unreadable is a typed error, and so is an unknown key
or a bad value; nothing is skipped and nothing is applied until the whole file
parses. The recognized keys are `frame_rate`, `preload_archives`, `debug_overlay` and
`graphic_cache_limit`, matching the argument names.

## 2. Implemented options

| option | unit / range | default | basis |
|---|---|---|---|
| `--fps N` (`frame_rate`) | frames per second, `1..=1000` | `60` | reference `fps_limit` default `60` (`reference/cpp/core/environ/ui/PreferenceConfig.h:304-309`), applied as the animation interval `1/fps` (`reference/cpp/core/environ/cocos2d/MainScene.cpp:684-685`); the main loop sleeps the remaining interval (`reference/cpp/core/environ/RenderUtils.h`, `TVPDrawSceneOnce`) |
| `--preload-archives` (`preload_archives`) | `true`, `false` | `false` | read every mounted archive into memory when it mounts; the cost is that archive's size in resident memory, the gain is that entry reads stop touching the backing filesystem |
| `--debug-overlay` (`debug_overlay`) | `true`, `false` | `false` | draw FPS and its rolling 1% low, process CPU, storage read/write rates, resident memory and live object counts in the top-left corner; a render-only node, never a script-visible layer or bitmap, so it cannot change what a script observes. CPU comes from `/proc/self/task/*/schedstat` and is a fraction of one core, so a single saturated core reads `100%` and N saturated cores read `N*100%`; storage from `/proc/self/io`, RSS from `/proc/self/status`; an unavailable value is shown as `?`, never fabricated |
| `--graphic-cache-limit LIMIT` (`graphic_cache_limit`) | `none`, `0`/`unlimited`, or a non-negative byte count | unset (reference default: disabled) | the decoded-image cache policy. `none` clears and disables it; `0` or `unlimited` keeps every decoded image; a positive integer is a byte cap with LRU eviction. Unset leaves the engine's reference default (disabled), so a startup script's `System.graphicCacheLimit` write still works and can override the launcher value. This is a launcher extension: it does **not** change the script property's reference encoding, where `0` means disabled and `-1` selects the 512 MiB system cap |

The frame-rate limit paces both loops: the windowed loop through
`PlatformConfig::frame_interval` and the headless step through
`FrameRateLimit::period_ms`. `1000` is the upper bound because that is where
the period reaches the platform ticker's 1 ms minimum; a higher limit would be
silently clamped, so it is rejected instead.

Preloading is a memory-for-filesystem trade and is worth setting when the game
lives on a slow mount. It is not a special mode: it reads the archive into the
same storage as before, so every entry returns the same bytes, and the only
difference is that the bytes no longer come from the backing filesystem.

Presentation is **not** configurable. There is no `--scaling` option or
`scaling` key: the engine always fits the scene into the framebuffer with the
single Fit rule (`docs/migration/11` D39), uniformly scaled and letterboxed.

The reference splits the two sizes. `Window.innerWidth`/`Window.innerHeight`
are the window client, which an OS resize and `Window.setInnerSize` move;
`System.screenWidth`/`System.screenHeight` are the logical screen, the primary
layer, and presentation fits that screen into the window. The mapping lives in
L2 and is invisible to scripts; there is no size setting to expose.

## 3. Not implemented meaningfully

Both of these have no knob to turn today; inventing one would be a stub, so
they are reported rather than implemented.

* **Texture resolution.** The renderer exposes no downsample factor. It
  advertises `GpuCapabilities::max_texture_dimension` — a hardware limit
  (`crates/platform/render/src/lib.rs`, `NULL_MAX_TEXTURE_DIMENSION` / the
  Vulkan device limit), not a setting — and `TextureDesc` is always the decoded
  image's native size. A "texture resolution" option would need a scale factor
  threaded through decode/upload while keeping the script-visible
  `Bitmap.width`/`height` unchanged; that is a substantive change to
  `media`/`render`/`app`, not a new DTO field.
* **Audio buffering.** The pre-fill (`PREFILL_MS = 150`) and cap
  (`MAX_BUFFER_MS = 2000`) are `pub(crate)` constants in
  `crates/platform/platform/src/audio_queue.rs`, shared by the cpal and AAudio
  backends and fixed in the reference's `FillL2Buffer`/`FillBuffer` pre-fill
  (`reference/cpp/core/sound/win32/WaveImpl.cpp:2911-2921`). Exposing them
  would mean adding a buffer parameter to `AudioDevice::open` and threading it
  into both backends (including `android.rs`); there is no existing seam.

## 4. Tests

`crates/app/src/main.rs` covers the desktop loader, the precedence
(argument beats file beats default, field by field), every validation failure,
and `--help` completeness. `crates/app/src/runner.rs` drives `app::run` with a
configuration passed directly — no file and no environment variable — and
checks the run used its frame period. `crates/platform/api/src/config.rs`
covers the value validation and the `key=value` parser. The overlay itself is
covered by `crates/app/src/overlay.rs` (formatting, the built-in font and the
missing-glyph error), `crates/app/tests/debug_overlay.rs` (the node is present
when enabled and absent when disabled, and its texture is uploaded before the
frame) and `crates/platform/platform/src/memory.rs` (the `VmRSS` parse).
