# Embedding krkr-rs

Status: **guide**. It describes the crates as they are today and links to the
pages that hold the contracts.

This page is for a Rust developer who wants to put **this project's engine**
into their own application: a scene graph, a projection, a renderer, a
storage namespace, a mixer, timers, text layout, and the platform services
underneath them — the window, the event loop, the clock, the input plumbing,
the texture upload, the file system and the media decoders — without writing
that platform boilerplate again.

It is a how-to. Two other pages hold the references it points at:

| page | what it holds |
|---|---|
| [`l2-api.md`](l2-api.md) | the `engine` crate's public surface: the types, the operations, the error enums and the stability contract |
| [`docs/code/engine/embedding.md`](../code/engine/embedding.md) | the engine-only guide: the scene, the storage namespace, the mixer, the input state, the presentation, in detail |

Where this page states a behaviour that the engine reproduces from the
KiriKiri2 engine, the pointer is to **our** file — the crate and the module
the code lives in. The C++ tree is the oracle for the compatibility layer
only (§7), and it is cited there and nowhere else.

---

## 1. The scope

### 1.1 What you get

Seven crates, in two layers. Nothing below L2 is optional, and nothing above
L2 is included.

| crate | layer | what it gives the embedder |
|---|---|---|
| `engine` | L2 | the scene (windows, layers, bitmaps, fonts), the projection to a draw tree, the presentation mapping, the storage namespace, the mixer, the timer registry, the input state, the text engine, the video overlay, the transition machine, and the frame that owns the clock |
| `api` | L1 | the shared value types (`BlendMode`, `RectF`, `InputEvent`, `AudioSpec`, `FontMetrics`) and the typed error enums (`FsError`, `MediaError`, `RenderError`, `AudioError`, `GlyphError`) |
| `fs` | L1 | the stateless file system — a host-directory backend and an in-memory backend — and the XP3 container parse |
| `media` | L1 | the stateless codecs: image decode/encode, audio decode, video decode, TLG, YUV |
| `render` | L1 | the render pipeline (a null backend and a Vulkan backend), the surface, the render state, the texture, the draw tree, the font system |
| `platform` | L1 | the window and its event loop, the ticker, the audio output device, the system font directories, the process metrics |
| `psd` | L1 | the PSB container and the PSD image decoder, as plain functions over bytes |

The dependency direction inside that set is fixed: `engine` depends on
`api`, `fs` and `render` for its types and for the stateless services it
acts on; `platform` and `media` sit beside it and are used by the
composition root. The layering is described in
[`overall.md`](overall.md) §1 and in [`l1.md`](l1.md).

### 1.2 What you do not get

| not included | what it is | how to add it |
|---|---|---|
| the script surface | the `compat` crate: the `Layer`, `Window`, `Bitmap`, `Storages`, `Trans`, `Font`, `WaveSoundBuffer`, `VideoOverlay` classes and every other script-visible name | add `crates/compat` and `crates/vm`; §7 |
| the script VM | the `vm` crate over the vendored C++ TJS2 VM in `crates/tjs` | the same |
| the game's loading conventions | the archive-mounting policy, the project-path choice, the startup-script order, the plugin autoload, the CLI options | these live in `crates/app`; copy the conventions you want, or call `app::run` directly |
| the Android launcher | the `GameActivity`, the JNI wiring, `krkr.log`, `krkr_state.json` | `crates/android`; it is a separate composition root over the same `app` crate |

Two of these are worth stating more plainly.

**The archive-mounting policy is the application's, not the engine's.** The
engine's storage namespace (`crates/engine/src/storage.rs`) mounts what it is
given, in order, later mounts winning, and resolves names against that list.
*Which* archives to mount, whether a directory is scanned for `*.xp3`, and
which mount is the "project" are decisions the composition root makes
(`crates/app/src/game.rs`). An embedder that wants the reference's
conventions takes them from there.

**The startup-script names are the engine's, the order is the
application's.** `engine::storage::Storage::startup_plan`
(`crates/engine/src/storage.rs`) resolves `patch.tjs`, `startup.tjs`,
`system/Initialize.tjs` and `AfterStartup.tjs`; it only looks them up. The
order they are executed in, and what a failure of each means, is the
composition root's (`crates/app/src/game.rs`, `run_startup`).

### 1.3 The layering rule for your own code

Your code sits **above** L2, in the position `crates/app` occupies. That
gives you three permissions and one prohibition:

* you may use L1 freely — the render pipeline, the file system, the codecs,
  the window, the audio device — because you are the composition root and
  the wiring is your job;
* you may use L2 through its public API (`engine::Engine`, `engine::Scene`,
  `engine::storage::Storage`, `engine::sound::Mixer`, …);
* you must own the frame's clock source and the frame hook, and pass them in;
* you must **not** push platform knowledge down into the engine. No window
  handle, no `winit` event, no `#[cfg(target_os)]`, no device pixel, no
  codec handle and no `std::fs` call belongs in `crates/engine`. If the
  engine needs something the platform can give it, the engine asks for it
  through a generic value it owns.

The rules the tree enforces, and that your code inherits by living above it,
are in [`../rules/arch_rules.md`](../rules/arch_rules.md) and
[`../rules/api_rules.md`](../rules/api_rules.md).

---

## 2. The dependencies

The crates are workspace members, not published packages: an embedder
references them by path and vendors or forks the tree. The workspace
version and license are in the root `Cargo.toml` (`1.0.0`,
`GPL-3.0-only`).

```toml
[dependencies]
# L2 — the engine.
engine = { path = "krkr-rs/crates/engine" }

# L1 — the type crates and the services.
api    = { path = "krkr-rs/crates/platform/api" }
fs     = { path = "krkr-rs/crates/platform/fs" }
media  = { path = "krkr-rs/crates/platform/media" }
render = { path = "krkr-rs/crates/platform/render" }

# L1 — the window, the event loop, the clock, the audio device.
# Exactly one platform feature must be active; the crate has no default.
platform = { path = "krkr-rs/crates/platform/platform", features = ["platform-linux"] }

# The window handle type the surface is created for.
raw-window-handle = "0.6"
```

Two feature selections are the embedder's to make:

* `platform` has **no default feature**; `platform-linux` and
  `platform-android` are mutually exclusive and one of them is required.
  Enabling neither, or both, is a compile error
  (`crates/platform/platform/src/lib.rs`).
* `media` selects the video backend: `video-ffmpeg` (the default, the
  desktop path) or `video-mediacodec` (Android). A member cannot turn the
  workspace default off, so an Android consumer names the path directly:
  `media = { path = "…", default-features = false, features =
  ["video-mediacodec"] }`.

`psd` is a separate workspace member inside the `media` directory
(`crates/platform/media/psd`); add it only if you want the PSB/PSD
decoder directly. The compatibility layer depends on it; the L1+L2 path does
not.

The `engine` crate itself pulls in `api`, `fs`, `render`, `cosmic-text`,
`thiserror`, `log`, `ahash` and `hashbrown`, plus the optional
`fearless_simd` behind the `simd-experiment` feature. It is
`#![forbid(unsafe_code)]` and `#![warn(missing_docs)]`.

---

## 3. The seams you implement

The engine and the services are not abstract over your platform: they are
written against a small number of seams, and each one is either a trait you
implement or a value you construct. These are all of them.

### 3.1 The frame clock — a trait you implement

```rust
// crates/engine/src/clock.rs
pub trait FrameTimeSource {
    fn elapsed_ms(&self) -> f64;
}
```

The engine owns the clock it advances; you own where the elapsed time comes
from. One source per run: every timeline the frame derives — the timers, the
script tick count, the mixer — comes from this one value.

`engine::InjectedFrameTime` is the built-in implementation: a public
`elapsed_ms: f64` field you write before every frame. The windowed loop in
this repository writes the time the platform measured; a test writes whatever
it needs.

### 3.2 The frame hook — a trait you implement

```rust
// crates/engine/src/engine.rs
pub trait GameFrame {
    fn before_render(&mut self, now_ms: u64, dt_ms: f64) -> Result<(), String>;
    fn after_project(&mut self, _drawn_layers: &[u32]) -> Result<(), String> { Ok(()) }
}
```

This is the single upward edge of the frame. The engine calls
`before_render` after the due timers have fired and before the scene is
projected, so whatever your hook leaves in the scene is what the frame draws.
`after_project` runs after the projection has reported the layers it
emitted; a hook that changes the scene there is followed by a second upload
and projection, so its change is in the frame being drawn.

A failure is a message: it reaches the caller as `EngineError::Frame` and
the frame is abandoned without drawing.

### 3.3 The render surface — values you construct

```rust
// crates/platform/render/src/lib.rs
pub fn create_render_pipeline(config: PipelineConfig) -> Result<RenderPipeline, RenderError>;
pub fn begin_render_state(
    p: &RenderPipeline,
    window: &RawWindowHandle,
) -> Result<(Surface, RenderState), RenderError>;
pub fn begin_render_state_with_display(
    p: &RenderPipeline,
    display: &RawDisplayHandle,
    window: &RawWindowHandle,
) -> Result<(Surface, RenderState), RenderError>;
```

`PipelineConfig` names the backend (`BackendKind::Null` or
`BackendKind::Vulkan`), the application name, the preferred format
(`TextureFormat::Rgba8Srgb`), the validation flag and the frame interval
that bounds the presentation wait.

The engine takes the pipeline, the surface and the render state over in one
`engine::EngineParts` and keeps no copy. The surface size it is given is the
**device** size in physical pixels; it can be changed later with
`Engine::resize`.

There is no plug point here: `RenderPipeline` holds a private backend
discriminant, so a third-party render backend cannot be registered from
outside the crate. An embedder uses the null backend or the Vulkan backend,
or renders the draw tree itself — the tree is a plain value
(`render::RenderScene`) and the engine's projection is public.

### 3.4 The file system — a value you construct

```rust
// crates/platform/fs/src/lib.rs
impl FileSystem {
    pub fn host(base: impl Into<PathBuf>) -> Self;
    pub fn memory() -> Self;
}
```

`host` roots the relative paths at a directory on disk; `memory` gives a
private in-memory store, which is what a test or an embedded resource bundle
uses. The engine's storage namespace is built over this object
(`Storage::mount_archives(fs, base_dir, archives)`,
`crates/engine/src/storage.rs`).

Like the render pipeline, the backend is a private discriminant: a
third-party file system cannot be plugged in from outside the crate.

### 3.5 The media backend — free functions you call

```rust
// crates/platform/media/src/lib.rs — the re-export surface; the codecs live
// in image_decode.rs, image_encode.rs, audio.rs and video/mod.rs.
pub struct Media;
impl Media {
    pub const fn new() -> Self;
}
pub fn decode_image(
    m: &Media,
    bytes: &[u8],
    hint: Option<&str>,
) -> Result<DecodedImage, MediaError>;
pub fn probe_image(m: &Media, bytes: &[u8], hint: Option<&str>) -> Result<ImageInfo, MediaError>;
pub fn encode_image(
    m: &Media,
    width: u32,
    height: u32,
    rgba: &[u8],
    hint: &str,
) -> Result<Vec<u8>, MediaError>;
pub fn open_audio(
    m: &Media,
    bytes: Arc<[u8]>,
    hint: Option<&str>,
) -> Result<(AudioSpec, AudioDecoderState), MediaError>;
pub fn read_audio(
    m: &Media,
    state: &mut AudioDecoderState,
    out: &mut [f32],
) -> Result<usize, MediaError>;
pub fn open_video(
    m: &Media,
    bytes: Arc<[u8]>,
    hint: Option<&str>,
) -> Result<(VideoSpec, VideoDecoderState), MediaError>;
pub fn present_video(
    m: &Media,
    state: &mut VideoDecoderState,
    at_ms: u64,
) -> Result<Arc<RgbaFrame>, MediaError>;
pub fn video_audio(m: &Media, state: &VideoDecoderState) -> Result<Arc<DecodedAudio>, MediaError>;
```

`Media` is a facade over the codecs the build links; it holds no state. The
free functions are the API. The image and audio codecs are always linked;
only the **video** backend is selected at compile time, by the `media` crate's
features (§2). There is no plug point here either: a third-party codec
cannot be registered from outside the crate.

The engine's own plug point for image decoding is a trait:

```rust
// crates/engine/src/imagedecode.rs
pub trait ImageDecoder: Send + Sync {
    fn probe(&self, bytes: &[u8], hint: Option<&str>) -> Result<ImageInfo, ImageDecodeError>;
    fn decode(&self, bytes: &[u8], hint: Option<&str>) -> Result<DecodedImage, ImageDecodeError>;
}
```

You install it with `Engine::install_decoder(decoder, config)`, which starts
the engine's deferred decode pool. `crates/app/src/image_codec.rs` shows
the wiring: the installed decoder delegates straight to `media::decode_image`
and `media::probe_image`.

### 3.6 The storage reader — a trait you implement

```rust
// crates/engine/src/storage.rs
pub trait StorageReader: Send + Sync {
    fn read(&self, job: &StorageJob) -> Result<Vec<u8>, StorageReadError>;
}
```

A read is scheduled, not cached: the reader runs on a worker and its failure
is the storage's own typed error — there is no substitute value and no empty
result. `engine::storage::FileSystemReader` is the default, and it reads
through the file system above. You install your own with
`Engine::install_storage_reader(reader, config)`, which hands the pool back
so the storage and the engine share it:

```rust
let pool = engine.install_storage_reader(
    std::sync::Arc::new(engine::storage::FileSystemReader),
    engine::storage::StoragePoolConfig::default(),
)?;
storage.read_pool = Some(pool);
```

Without a pool the storage reads on the calling thread.

### 3.7 The audio sink — a device you open, or your own

```rust
// crates/platform/platform/src/audio.rs
impl AudioDevice {
    pub fn default_spec() -> Result<AudioSpec, PlatformError>;
    pub fn open(spec: AudioSpec) -> Result<(Self, AudioOutputState), PlatformError>;
    pub fn null(spec: AudioSpec) -> Result<(Self, AudioOutputState), PlatformError>;
    pub fn write(&self, state: &mut AudioOutputState, pcm: &[f32]) -> Result<(), PlatformError>;
}
```

`open` opens the real output device at the spec the device prefers;
`null` opens a silent mock with the same queue and pre-fill policy, which
is what a headless host uses. `AudioDevice` holds only the spec and the
backend; the stream and its queue live in the caller-owned
`AudioOutputState`.

The engine has **no** audio device. The mixer is a value you own
(`engine::sound::Mixer`), and it renders into your buffer:

```rust
// crates/engine/src/sound.rs
impl Mixer {
pub fn mix(&self, out: &mut [f32], out_spec: AudioSpec) -> Result<(), SoundError>;
pub fn mix_advancing(&mut self, out: &mut [f32], out_spec: AudioSpec) -> Result<(), SoundError>;
pub fn advance(&mut self, dt: f64);
pub fn advance_to(&mut self, now: f64);
}
```

`mix` renders without moving the clock; `mix_advancing` renders and
advances by what it rendered. `crates/app/src/audio.rs` renders on the frame
thread and writes to the device; a host with its own audio stack pulls from
the device callback instead. The mixer never allocates an output device.

### 3.8 The input source — events you produce

The raw event is `api::input::InputEvent` (re-exported as
`platform::InputEvent`): `PointerMoved`, `PointerLeft`, `MouseButton`,
`Wheel`, `Key`, `Text`, `Touch`. The generic per-frame state is
`engine::input::InputState`, reached through `Engine::input_mut()`, and
the frame protocol is three calls:

```rust
// crates/engine/src/input.rs
impl InputState {
pub fn begin_frame(&mut self);
pub fn apply(&mut self, event: &InputEvent);
pub fn end_frame(&mut self);
}
```

`begin_frame` clears the release flags, the wheel and the last-pressed list;
`apply` folds in one event; `end_frame` advances the held counters. Call
them exactly once per frame, around the `apply` calls.

The state carries no TVP or VK semantics — no key codes, no click-sequence
constants, no shift flags. Those belong to the layer above.

### 3.9 The window and the event loop — a trait you implement

```rust
// crates/platform/platform/src/lib.rs
pub fn run<H: EventHandler>(config: PlatformConfig, handler: H) -> Result<(), PlatformError>;

pub trait EventHandler {
    fn resumed(&mut self, window: &PlatformWindow) -> Flow;
    fn event(&mut self, event: PlatformEvent) -> Flow;
    fn frame(&mut self, dt_ms: f32, window: &PlatformWindow, pacing: FramePacing) -> Flow;
    fn about_to_wait(&mut self) -> Flow { Flow::Continue }
}
```

The platform owns the window and the ticker; your handler owns the engine.
`resumed` is called once the window and its display connection exist, and
may be called again after a resume on a mobile platform — make it idempotent.
`frame` is called once per due frame with the time since the previous frame.
`PlatformConfig` is `#[non_exhaustive]`: build it with `Default` and
assign the fields you need.

`PlatformWindow` gives you the handles the surface needs
(`raw_window_handle()`, `raw_display_handle()`), the physical size
(`size()`), the applied size of a requested change (`set_size()`) and
`request_redraw()`.

---

## 4. A worked example

The smallest host that owns a window and drives the frame. It mirrors
`crates/app/src/runner.rs` (the windowed loop) and `crates/app/src/lib.rs`
(`Host::new`, the engine assembly), with the game, the storage and the VM
left out.

What comes from this repository: the window, the event loop, the ticker, the
render pipeline, the surface, the engine, the scene, the projection and the
draw. What is yours: the handler, the frame hook, and the decisions about
what to draw.

```rust
use std::time::Duration;

use engine::{Engine, EngineParts, GameFrame, InjectedFrameTime};
use platform::{
    EventHandler, Flow, FramePacing, PlatformConfig, PlatformError, PlatformEvent, PlatformWindow,
};
use render::{
    BackendKind, PipelineConfig, RenderState, Surface, TextureFormat,
    begin_render_state_with_display, create_render_pipeline,
};

/// Your per-frame work.
///
/// This is the single upward edge of the frame: the engine calls it after
/// the due timers have fired and before the scene is projected, so whatever
/// it leaves in the scene is what the frame draws.
struct HostFrame;

impl GameFrame for HostFrame {
    fn before_render(&mut self, _now_ms: u64, _dt_ms: f64) -> Result<(), String> {
        Ok(())
    }
}

/// Your window, and the frame loop around it.
struct MyApp {
    /// The frame's elapsed time, written from the platform's measurement.
    frame_time: InjectedFrameTime,
    /// The engine, created once the window handle exists.
    engine: Option<Engine>,
    /// The newest window size an OS resize reported since the last frame.
    pending_surface_size: Option<(u32, u32)>,
}

impl EventHandler for MyApp {
    fn resumed(&mut self, window: &PlatformWindow) -> Flow {
        // A missing handle is fatal; a synthetic one is never substituted.
        let window_handle = match window.raw_window_handle() {
            Ok(handle) => handle,
            Err(error) => {
                eprintln!("my-app: cannot obtain the window handle: {error}");
                return Flow::Exit;
            }
        };
        let display_handle = match window.raw_display_handle() {
            Ok(handle) => handle,
            Err(error) => {
                eprintln!("my-app: cannot obtain the display handle: {error}");
                return Flow::Exit;
            }
        };

        // The bottom render objects. The null backend needs neither a GPU nor
        // a display; the Vulkan backend needs the two handles above.
        let pipeline = match create_render_pipeline(PipelineConfig {
            backend: BackendKind::Vulkan,
            application_name: "my-app".to_owned(),
            preferred_format: TextureFormat::Rgba8Srgb,
            validation: false,
            frame_interval: Duration::from_millis(16),
        }) {
            Ok(pipeline) => pipeline,
            Err(error) => {
                eprintln!("my-app: cannot create the render pipeline: {error}");
                return Flow::Exit;
            }
        };
        let (surface, render_state): (Surface, RenderState) =
            match begin_render_state_with_display(&pipeline, &display_handle, &window_handle) {
                Ok(pair) => pair,
                Err(error) => {
                    eprintln!("my-app: cannot create the surface: {error}");
                    return Flow::Exit;
                }
            };

        // The engine takes the pipeline, the surface and the render state
        // over. The surface size is the device size in physical pixels.
        match Engine::new(EngineParts {
            pipeline,
            surface,
            render_state,
            surface_size: window.size(),
            headless: false,
            overlay: None,
        }) {
            Ok(engine) => {
                self.engine = Some(engine);
                Flow::Continue
            }
            Err(error) => {
                eprintln!("my-app: cannot create the engine: {error}");
                Flow::Exit
            }
        }
    }

    fn event(&mut self, event: PlatformEvent) -> Flow {
        match event {
            PlatformEvent::Resized(width, height) => {
                // The surface follows the newest size at the next frame
                // boundary; the scene's own sizes do not change.
                self.pending_surface_size = Some((width, height));
                Flow::Continue
            }
            PlatformEvent::CloseRequested => Flow::Exit,
            _ => Flow::Continue,
        }
    }

    fn frame(&mut self, dt_ms: f32, _window: &PlatformWindow, _pacing: FramePacing) -> Flow {
        // The platform measured the gap since the previous frame; that
        // measurement is the elapsed time the engine's clock advances by.
        self.frame_time.elapsed_ms = f64::from(dt_ms);
        let Some(engine) = self.engine.as_mut() else {
            return Flow::Continue;
        };
        // An OS resize is applied once, with the newest size the event loop
        // reported, before the frame body and the draw see it.
        if let Some(size) = self.pending_surface_size.take() {
            if let Err(error) = engine.resize(size) {
                eprintln!("my-app: resize failed: {error}");
                return Flow::Exit;
            }
        }
        let mut hooks = HostFrame;
        match engine.run(&self.frame_time, &mut hooks) {
            Ok(report) => {
                // `presented` is false when the scene had nothing new to
                // draw, and when the display did not accept the frame. The
                // engine counts the frames the display skipped.
                if report.frame_index % 60 == 0 {
                    eprintln!("my-app: frame {}", report.frame_index);
                }
            }
            Err(error) => {
                eprintln!("my-app: frame failed: {error}");
                return Flow::Exit;
            }
        }
        Flow::Continue
    }
}

fn main() -> Result<(), PlatformError> {
    // `PlatformConfig` is `#[non_exhaustive]`: build it from `Default` and
    // assign the fields you need.
    let mut config = PlatformConfig::default();
    config.title = "my-app".to_owned();
    config.frame_interval = Duration::from_millis(16);

    platform::run(
        config,
        MyApp {
            frame_time: InjectedFrameTime::default(),
            engine: None,
            pending_surface_size: None,
        },
    )
}
```

The scene is empty in this example, so the frame draws nothing. To put
something on the screen, build it through `Engine::scene_mut()`:

```rust
let scene = engine.scene_mut();
let window = scene.add_window("demo", (640, 480));
let layer = scene.add_layer(window, None);
if let Some(state) = scene.layer_mut(layer) {
    state.rect = engine::Rect::new(0, 0, 640, 480);
}
assert!(scene.set_layer_image_size(layer, 640, 480));
```

A layer with no image draws nothing, and `set_layer_image_size` is what
allocates one. Every write goes through the scene's own accessors, so the
structure revision moves and the projection sees the change; a field written
through a raw `&mut LayerState` would not be drawn until something else
bumped the revision.

### 4.1 The calls, against the source

| call | signature | source |
|---|---|---|
| `platform::run(config, handler)` | `fn run<H: EventHandler>(config: PlatformConfig, handler: H) -> Result<(), PlatformError>` | `crates/platform/platform/src/lib.rs` |
| `create_render_pipeline(config)` | `fn create_render_pipeline(config: PipelineConfig) -> Result<RenderPipeline, RenderError>` | `crates/platform/render/src/lib.rs` |
| `begin_render_state_with_display(p, display, window)` | `fn begin_render_state_with_display(p: &RenderPipeline, display: &RawDisplayHandle, window: &RawWindowHandle) -> Result<(Surface, RenderState), RenderError>` | `crates/platform/render/src/lib.rs` |
| `Engine::new(parts)` | `fn new(parts: EngineParts) -> Result<Self, EngineError>` | `crates/engine/src/engine.rs` |
| `engine.run(clock, hooks)` | `fn run(&mut self, clock: &dyn FrameTimeSource, hooks: &mut dyn GameFrame) -> Result<FrameReport, EngineError>` | `crates/engine/src/engine.rs` |
| `engine.resize(size)` | `fn resize(&mut self, size: (u32, u32)) -> Result<SurfaceEvent, EngineError>` | `crates/engine/src/engine.rs` |
| `scene.add_window(title, size)` | `fn add_window(&mut self, title: impl Into<String>, inner_size: (u32, u32)) -> u32` | `crates/engine/src/scene.rs` |
| `scene.add_layer(window, parent)` | `fn add_layer(&mut self, window: u32, parent: Option<u32>) -> u32` | `crates/engine/src/scene.rs` |
| `scene.set_layer_image_size(id, w, h)` | `fn set_layer_image_size(&mut self, layer_id: u32, width: u32, height: u32) -> bool` | `crates/engine/src/scene.rs` |

### 4.2 What this repository's own roots add

`crates/app` is the worked example above plus three things:

* the game host (`crates/app/src/game.rs`): the storage mount, the L1 codec
  seams, the plugin autoload and the startup sequence;
* the CLI (`crates/app/src/main.rs`): the argument parsing, the configuration
  file and the game path;
* the debug overlay (`crates/app/src/overlay.rs`): a render-only node drawn
  above the scene, which the engine carries as `EngineParts::overlay` — an
  `engine::EngineOverlay`.

`crates/android` is the same emulator behind an `android-activity`
`GameActivity`. Both drive the same `platform::EventHandler`; the
difference is the launcher and the event source, never the engine.

---

## 5. The frame contract

The engine owns the frame. Nothing above L2 owns a `dt`.

### 5.1 Who owns the clock

The engine owns the clock it advances (`engine::clock::FrameClock`); you
own where the elapsed time comes from. One `FrameTimeSource` per run, and
every timeline the frame derives comes from that one value.

There are three entry points, and they differ only in how the clock moves:

| call | the clock | when to use it |
|---|---|---|
| `Engine::run(clock, hooks)` | **advanced** by the elapsed time the source reports, fractional milliseconds included | the running loop |
| `Engine::step(dt_ms, hooks)` | **advanced** by `dt_ms` (`f32`) | a loop that knows its own period |
| `Engine::frame(now_ms, hooks)` | **set** to `now_ms` exactly | a test that jumps between frames deterministically |

All three return `Result<FrameReport, EngineError>`. Sixty
`step(16.6)` calls advance the clock by 996 ms, not 960; a negative delta
counts as zero.

### 5.2 The order of one frame

`Engine::run`, `step` and `frame` all end in the same sequence
(`crates/engine/src/engine.rs`, `run_frame`):

1. record the frame's clock;
2. fit the presentation — refit the VM↔device mapping and observe the window
   client size;
3. fire the due timers (`engine::timer::deliver_due`);
4. drain the storage read pool — the reads the previous frame scheduled are
   published before your hook runs;
5. run your hook, `GameFrame::before_render(now_ms, dt_ms)`;
6. wait for the pending image decodes — the image barrier;
7. fit the presentation again, because the hook may have resized a window's
   primary layer;
8. refresh the render-only overlay, if you installed one;
9. upload the dirty bitmap regions and prune the textures whose bitmap is
   gone;
10. project the scene when its structure revision changed;
11. run `GameFrame::after_project(drawn_layers)`;
12. fit the presentation again, upload again, and project again if the hook
    changed the scene's structure;
13. **one** render call — and only when the scene changed: a frame with
    nothing to show does not draw at all, and the previous frame's contents
    stay on screen;
14. recover a lost surface, record the frame in the presentation stall, bump
   the frame counter and return the report.

A size pair that cannot be fitted skips everything that draws and returns a
report with no statistics.

### 5.3 The report

`engine::FrameReport` is a plain value:

| field | meaning |
|---|---|
| `frame_index` | the frames drawn so far, counting this one |
| `elapsed_ms` | the frame clock this frame ran at |
| `presented` | whether the frame reached the display |
| `stats` | the null backend's per-frame counters, `None` on a GPU backend |
| `sync_ms`, `upload_ms`, `prune_ms` | the bitmap-to-texture costs |
| `project_ms` | the projection cost |
| `render_ms` | the pipeline's render cost |

A frame with `presented == false` either had nothing new to draw (the
scene did not change) or reached a display that did not accept it; in the
second case the previous surface contents stay on screen. The engine counts
the run of skipped frames and reports it; you do not have to retry
anything.

### 5.4 Which errors surface

`engine::EngineError` is `#[non_exhaustive]` and has three variants:

| variant | meaning |
|---|---|
| `EngineError::Render(RenderError)` | a bottom-layer render action failed; the L1 error is carried unchanged |
| `EngineError::Overlay(String)` | the overlay could not be refreshed; the frame is abandoned without projecting or drawing |
| `EngineError::Frame(String)` | your hook reported a failure; the frame is abandoned without projecting or drawing |

A failure that does not stop the frame — a zero-dimension bitmap, a texture
the backend refuses, a surface that was lost — is reported and counted, never
turned into a substitute value.

The other domains have their own enums: `engine::TranslateError`,
`engine::timer::TimerError`, `engine::text::TextError`,
`engine::sound::SoundError`, `engine::video::VideoError`,
`engine::trans::TransError`, `engine::shader::ShaderError`,
`engine::storage::MountError`, `engine::storage::StorageReadError`,
`engine::storage::StoragePoolError`, `engine::imagedecode::DecodePoolError`,
`engine::startup::StartupError`, and in L1 `api::error::RenderError`,
`api::error::FsError`, `api::error::MediaError`, `api::error::AudioError`,
`api::error::GlyphError`. The full list with their variants is in
[`l2-api.md`](l2-api.md) §7.

---

## 6. The coordinate spaces

Three pixel spaces, and they are never the same:

| space | unit | who speaks it |
|---|---|---|
| **VM pixel** | one pixel of the logical screen (the first window's primary layer) | the scene, the scripts, the input state |
| **window pixel** | one pixel of the OS window's client area | the window size the platform reports |
| **device pixel** | one pixel of the framebuffer | the render pipeline |

Only L2 names more than one of them.
`engine::translate::PixelTranslation` owns the VM↔device mapping: the
scene is scaled uniformly by `min(device / vm)`, centred, and the margin is
cleared. There is no mode enum and no configuration key.

**Input travels the other way.** A pointer or touch position arrives from the
platform in device pixels; you map it back into VM pixels before the input
state sees it:

```rust
if let Some(translation) = engine.translation() {
    let (x, y) = translation.to_vm_point((device_x, device_y));
    input.set_pointer(x, y);
}
```

`crates/app/src/game.rs` does this in the frame hook, mapping only the
position-carrying events (the pointer move and the touch) and passing every
other event through unchanged.

**A device resize moves nothing in the scene.** `Engine::resize(size)`
rebinds the surface to a new device size; the window's client size and the
primary layer's rect are untouched, and the presentation refits the screen
into the new framebuffer. The other direction — a script-side window size —
travels through `Engine::take_window_resize()`, and you apply it to your
own window.

---

## 7. Adding the script layer

This section is the one place where the KiriKiri2 engine is the oracle. The
compatibility layer exists to make unmodified games and scripts behave exactly
as they do there, so the reference is the right place to look for what a
behaviour is.

### 7.1 What the two crates add

| crate | what it adds |
|---|---|
| `crates/compat` (L3) | the script-visible classes and their members: `Layer`, `Window`, `Bitmap`, `Storages`, `Font`, `Trans`, `WaveSoundBuffer`, `VideoOverlay`, the plugins, the KAG runtime, the event adapter — and the seam module `crates/compat/src/engine_api.rs`, through which L3 names L2 |
| `crates/vm` (L4) | the TJS2 VM host over the vendored C++ VM in `crates/tjs`: the script lifecycle, the bytecode cache, the frame body |

The vendored VM is a separate workspace member and the only crate in the tree
that contains C++.

### 7.2 What you then have to supply

Adding them is not a matter of calling one constructor. The compatibility
layer needs the L2 state attached, and the composition root is what owns
that wiring:

1. **The storage mount.** The script reads through `Storages`, so a
   `engine::storage::Storage` must be mounted over the game's files first
   (`crates/app/src/game.rs`). The mount policy — which archives, in what
   order, which one is the project — is the application's choice.
2. **The plugin autoload.** The `.tpm` plugins in the executable's and the
   system's plugin folders are loaded before any script runs, which is the
   reference's order
   (`reference/cpp/core/plugin/PluginImpl.cpp:148-177`, cited in
   `crates/app/src/game.rs`). The composition root enumerates the files
   with the L1 file system and the compatibility layer links them.
3. **The startup script.** `Storage::startup_plan()`
   (`crates/engine/src/storage.rs`) resolves the four names; executing them
   in order is the composition root's job. The reference runs `patch.tjs`,
   then `startup.tjs` (with the `system/Initialize.tjs` fallback), then
   `AfterStartup.tjs` — the order `crates/app/src/game.rs` implements
   (the reference's `TVPExecuteStartupScript`, named in that file).
4. **The service handles.** `VmHost::with_default_services`
   (`crates/vm/src/host.rs`) attaches raw pointers to the scene, the
   storage, the timer registry, the input state, the frame clock and the
   decode pool. Each pointer must stay valid for as long as the binding is
   installed, so the values they point at are boxed and outlive the host.
   This is the one place the tree uses `unsafe`, and it is confined to the
   composition root.
5. **The L1 codec seams.** The compatibility layer never links a codec. The
   image codec, the audio decoder and the video source are installed on the
   `CompatHost` by the composition root
   (`crates/app/src/image_codec.rs`, `crates/app/src/audio.rs`,
   `crates/app/src/video.rs`).
6. **The frame body.** The script's per-frame work — the sound, video,
   async-trigger, continuous-handler and transition polls — is a
   `compat::FrameBody`, and the composition root runs it inside the engine's
   `GameFrame::before_render`, right after `VmHost::tick`
   (`crates/app/src/game.rs`).

### 7.3 What it costs

* **A build with a C++ toolchain.** `crates/tjs` compiles the vendored VM;
  the workspace's build script fetches its dependencies with Zig.
* **A process-global binding.** The VM keeps process-global state, so two
  VM hosts cannot run at once in one process. The engine has no such limit:
  two engines can run side by side.
* **A surface lock.** What `CompatHost::install` registers is the committed
  dump `docs/surface/runtime_surface.json`, and the `surface-lock` gate
  check fails on any added or removed member. If you add a script-visible
  member, the baseline change is a reviewed one.
* **The reference's semantics.** Every script-observable value — a member
  getter, a return value, a raised error, a `typeof`, a string conversion
  — matches the reference exactly. That is the whole point of the layer, and
  it is why the reference, not this repository, is its oracle.

### 7.4 The short version

If you want a KiriKiri game to run unmodified, do not wire `compat` and
`vm` yourself: call `app::run`
(`crates/app/src/runner.rs`) with a `Launch`, or use `app::GameHost`
(`crates/app/src/game.rs`) directly. It already does all six things above.
Reach for the manual wiring only when you need the script surface inside an
application that has its own composition root — and then follow
`crates/app/src/game.rs`, because that is the only worked example of the
wiring in the tree.

---

## 8. The rules your code must obey

### 8.1 The layering

Your code is above L2. The engine stays below L1, L1 stays below the OS, and
the compatibility layer stays above the engine. The dependency direction is
in [`overall.md`](overall.md) §1 and the rules in
[`../rules/arch_rules.md`](../rules/arch_rules.md).

Three obligations follow for an embedder:

* **the engine stays generic.** No TVP vocabulary in L2: no blend numbering,
  no virtual-key codes, no archive-path spelling, no script tags, no volume
  scale. If you need one of those in your host, it belongs in your host or
  in L3.
* **L1 stays stateless.** The L1 services are facades: their methods take
  `&self` and the evolving value as an explicit `&mut` argument
  (`RenderState`, `FileState`, `AudioOutputState`, the decoder states).
  All engine state is owned by L2, and by you.
* **the compatibility layer names the engine only through the seam.** If you
  add script-visible behaviour, it goes through
  `crates/compat/src/engine_api.rs`.

### 8.2 The errors

Every domain owns one `#[non_exhaustive]` error enum, and every enum is a
value. There is no panic on malformed input, no silent fallback, no
substituted default, and no `unwrap_or` on a fallible result. A failure
that cannot be performed is a typed error.

Two consequences an embedder hits immediately:

* **"not found" and "failed" are different.** `Storage::find` returns
  `Result<Option<Location>, StorageReadError>`; a miss is `Ok(None)` and a
  broken file system is `Err(..)`.
* **a match on a non-exhaustive enum needs a catch-all arm**, and that arm
  must not swallow the variant silently.

The rules are in [`../rules/api_rules.md`](../rules/api_rules.md) §5.

### 8.3 The stability contract

The `engine` crate's public surface is a **reviewed contract**, not a
snapshot. An additive change — a new function, a new method, a new variant —
is fine. A removal, a changed signature or a changed meaning needs a
reviewed change. An L1 type may flow into an `engine` signature, and such a
change is accepted rather than forbidden.

The full statement is in [`l2-api.md`](l2-api.md) §9. In short, as a caller
you may rely on: the crate root's curated re-export list and the module paths
behind it; the per-subsystem invariants; and the error enums' existing
variants.

### 8.4 The surface lock

If you add script-visible members, the lock applies:
`crates/compat/tests/runtime_surface.rs` dumps what `CompatHost::install`
actually registered, and the `surface-lock` gate check fails on any added or
removed member. Adding a member is a reviewed baseline change, never a silent
edit. The lock is described in [`../rules/api_rules.md`](../rules/api_rules.md)
§2.

### 8.5 The gate

If you change this repository rather than only embedding it, the ordered
static gate and the test suites must be green:

```bash
uv run --project scripts krkr-gate gate
uv run --project scripts krkr-gate test
```

The checks and what each one protects are in
[`../rules/gates.md`](../rules/gates.md).

---

## 9. Where to read next

| you want | read |
|---|---|
| the engine crate's public surface: the types, the operations, the error enums, the stability contract | [`l2-api.md`](l2-api.md) |
| the engine in detail: the scene, the storage namespace, the mixer, the input state, the presentation | [`../code/engine/embedding.md`](../code/engine/embedding.md) |
| the layers, the crate map, the coordinate spaces, the frame in one page | [`overall.md`](overall.md) |
| the engine's architecture: the layer tree, the projection, the presentation, the frame | [`l2.md`](l2.md) |
| the stateless platform services and the state values | [`l1.md`](l1.md) |
| the module map, one page per module | [`../code/engine/README.md`](../code/engine/README.md), [`../code/platform/README.md`](../code/platform/README.md) |
| the layering, the state ownership, the frame contract, the error contract | [`../rules/arch_rules.md`](../rules/arch_rules.md) |
| the public surfaces, the L3↔L2 protocol, the ownership rules, the error enums | [`../rules/api_rules.md`](../rules/api_rules.md) |
| the Rust-level review checklist | [`../rules/code_styles.md`](../rules/code_styles.md) |
| the compatibility surface and the VM host | [`l3.md`](l3.md), [`l4.md`](l4.md) |
| the composition roots in this repository | [`../code/app/README.md`](../code/app/README.md) |
