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
// The player's stored choices (frame rate, scaling) are read from the
// launcher's own settings file, resolved over the Android defaults, and the
// finished value is handed in. An unparseable or out-of-range value is a
// ConfigError naming the option and its range; it is never defaulted away.
let runtime = config::resolve(stored_frame_rate, stored_scaling)?;
app::run(Launch {
    runtime,
    backend: BackendKind::Vulkan,
    game: Some(GameInput { base_dir, archives }),
    mode: RunMode::Windowed,
})?;
```

The reason is that **where configuration comes from is a platform concern**,
not an engine one. A desktop build reads a file; an Android build reads its
own storage; a build with no persistent configuration just uses the defaults.
Keeping that choice in each launcher means the core contains no
`#[cfg(target_os = ...)]`, no configuration path and no loading trait — it
only acts on the value it is handed. A launcher still needs the shared value
types and the `key=value` parser, so `api::config` provides `RuntimeConfig`,
`FrameRateLimit`, `PresentationScaling`, `parse_overrides`, the partial
`RuntimeConfigOverrides` shape and the typed `ConfigError`.

Every field is validated. An unparseable or out-of-range value is a typed
`ConfigError` naming the option and the accepted range, never a clamped or
defaulted value (D11). Nothing takes effect until the whole configuration —
arguments *and* the launcher's store — validates.

## 1. The desktop file

The desktop launcher reads the `key=value` file named by the `KRKR_CONFIG`
environment variable (unset → no file → defaults). This loader lives in
`crates/app/src/main.rs`, next to the argument parser that shares its
precedence; the emulator core does not know it exists. `crates/app` is allowed
to read files (only the compatibility layer is barred from `std::fs`).

A file that is named but unreadable is a typed error, and so is an unknown key
or a bad value; nothing is skipped and nothing is applied until the whole file
parses. The recognized keys are `frame_rate`, `scaling` and
`preload_archives`, matching the argument names.

## 2. Implemented options

| option | unit / range | default | basis |
|---|---|---|---|
| `--fps N` (`frame_rate`) | frames per second, `1..=1000` | `60` | reference `fps_limit` default `60` (`reference/cpp/core/environ/ui/PreferenceConfig.h:304-309`), applied as the animation interval `1/fps` (`reference/cpp/core/environ/cocos2d/MainScene.cpp:684-685`); the main loop sleeps the remaining interval (`reference/cpp/core/environ/RenderUtils.h`, `TVPDrawSceneOnce`) |
| `--scaling MODE` (`scaling`) | `game`, `fit`, `fill`, `expand` | `game` (Android launchers default to `fit`) | `game` is the reference Win32 one-to-one presentation; the other three are the existing engine projection modes |
| `--preload-archives` (`preload_archives`) | `true`, `false` | `false` | read every mounted archive into memory when it mounts; the cost is that archive's size in resident memory, the gain is that entry reads stop touching the backing filesystem |

The frame-rate limit paces both loops: the windowed loop through
`PlatformConfig::frame_interval` and the headless step through
`FrameRateLimit::period_ms`. `1000` is the upper bound because that is where
the period reaches the platform ticker's 1 ms minimum; a higher limit would be
silently clamped, so it is rejected instead.

Preloading is a memory-for-filesystem trade and is worth setting when the game
lives on a slow mount. It is not a special mode: it reads the archive into the
same storage as before, so every entry returns the same bytes, and the only
difference is that the bytes no longer come from the backing filesystem.

## 3. Not implemented meaningfully

Both of these were requested, but there is no knob to turn today; inventing one
would be a stub, so they are reported rather than implemented.

* **Texture resolution.** The renderer exposes no downsample factor. It
  advertises `GpuCapabilities::max_texture_dimension` — a hardware limit
  (`crates/platform/render/src/lib.rs`, `NULL_MAX_TEXTURE_DIMENSION` / the
  Vulkan device limit), not a setting — and `TextureDesc` is always the decoded
  image's native size. A "texture resolution" option would need a scale factor
  threaded through decode/upload while keeping the script-visible
  `Bitmap.width`/`height` unchanged (D34); that is a substantive change to
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
covers the value validation and the `key=value` parser.
