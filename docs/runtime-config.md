# Runtime configuration

Status: implemented. Related: [`migration/09-game-loading.md`](migration/09-game-loading.md)
(launch arguments), [`android.md`](android.md) (the Android frontend).

The binary resolves one explicit value, `api::config::RuntimeConfig`, with the
precedence

```
launch argument  >  platform-provided configuration  >  built-in default
```

`crates/app/src/main.rs` parses and validates the arguments, `platform` supplies
a `api::config::ConfigSource`, and the resolver merges the two over the
defaults. Every field is validated; an unparseable or out-of-range value is a
typed `ConfigError` naming the option and its accepted range, never a clamped
or defaulted value (D11). Nothing takes effect until the whole configuration —
arguments *and* the platform source — validates.

## 1. The platform seam

`RuntimeConfig` lives in `api` (the shared DTO home). The *source* of the
platform half is `api::config::ConfigSource`, implemented and returned by
`platform::config_source()`. The composition root calls that function and never
chooses a path, a format or an OS:

```rust
let source = platform::config_source();
let runtime = resolve_runtime_config(&cli, &source)?;
```

A `#[cfg(target_os = ...)]` in the app would be wrong: the app is the
composition root and must not branch on the OS, and the platform is where the
window, IO and lifecycle already live. Each platform build therefore holds its
configuration its own way behind the same trait:

* desktop reads the `key=value` file named by the `KRKR_CONFIG` environment
  variable (absent → `NoConfigSource` → defaults);
* a future Android build can return a settings-store-backed source instead;
* `NoConfigSource` is the valid "no configuration" implementation.

A source that is configured but unreadable or malformed is a typed error, not
an empty result ("no silent fallbacks"). Configuration is deliberately **not**
read from a fixed `.config` path baked into the app, so no platform is forced
into the desktop's storage model.

## 2. Implemented options

| option | unit / range | default | basis |
|---|---|---|---|
| `--fps N` (`frame_rate_limit`) | frames per second, `1..=1000` | `60` | reference `fps_limit` default `60` (`reference/cpp/core/environ/ui/PreferenceConfig.h:304-309`), applied as the animation interval `1/fps` (`reference/cpp/core/environ/cocos2d/MainScene.cpp:684-685`); the main loop sleeps the remaining interval (`reference/cpp/core/environ/RenderUtils.h`, `TVPDrawSceneOnce`) |
| `--scaling MODE` (`scaling`) | `game`, `fit`, `fill`, `expand` | `game` | `game` is the reference Win32 one-to-one presentation; the other three are the existing engine projection modes |

The frame-rate limit paces both loops: the windowed loop through
`PlatformConfig::frame_interval` and the headless step through
`FrameRateLimit::period_ms`. `1000` is the upper bound because that is where
the period reaches the platform ticker's 1 ms minimum; a higher limit would be
silently clamped, so it is rejected instead.

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

`crates/app/src/main.rs` covers the precedence (argument beats platform beats
default, field by field), every validation failure, and `--help` completeness;
`crates/platform/api/src/config.rs` covers the value validation and the
`key=value` parser.
