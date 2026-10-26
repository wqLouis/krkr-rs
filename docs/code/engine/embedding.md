# Embedding the engine

This page is for a project that wants the **engine** — the layer tree, the
projection, the storage namespace, the mixer, the timer registry and the
pixel translation — as a library, and drives it with its own application code
rather than with the KiriKiri compatibility surface.

It is not a tutorial for porting a KiriKiri game. The script-visible names
(`Layer`, `Window`, `Bitmap`, `Storages`, `Trans`, the `Layer.type`
numbering, the `arc.xp3>` storage grammar, the KAG tags) live in
`crates/compat`, and this page does not describe them. What it describes is
the layer below that one: a generic, TVP-free engine that a host can own.

Every name on this page exists in the tree. The worked example in
[§4](#4-a-minimal-composition-root) is written against the real signatures in
`crates/engine/src/engine.rs` and `crates/app/src/game.rs`.

---

## 1. What the engine is, and what it is not

The engine crate (`crates/engine`) is the middle layer of this repository. It
is a **generic game engine**: a scene graph of windows, layers, bitmaps and
fonts; the projection that turns that tree into a draw tree; a storage
namespace over mounted archives and directories; a software mixer; a timer
registry; an input state; a text engine; a video-overlay playback machine; a
transition state machine; and the frame that ties them together.

What it gives you:

| subsystem | type | what it holds |
|---|---|---|
| the frame | `engine::Engine` | the scene, the projected tree, the texture table, the timer registry, the input state, the clock, the presentation mapping |
| the logical tree | `engine::Scene` | windows, layers, bitmaps, fonts, focus domains, the decoded-image cache |
| the projection | `engine::projection` | the pure function from a scene to an L1 `RenderScene` |
| the presentation | `engine::PixelTranslation` | the VM-pixel ↔ device-pixel mapping for one pair of sizes |
| the namespace | `engine::storage::Storage` | the mounts, the auto paths, the resolution cache, the read pool |
| the mixer | `engine::sound::Mixer` | the channels, the voices, the fades, the loop state |
| the timers | `engine::timer::TimerState` | the repeating timer registry |
| the input state | `engine::input::InputState` | pointer, buttons, wheel, keys, touches, the text buffer |
| the text | `engine::text::TextEngine` | the faces, the shaping, the coverage cache, the glyph atlas |
| the video | `engine::video::VideoOverlay` | the playback machine of one overlay |
| the transitions | `engine::trans::Trans` | one transition's kind, duration, phase and rule |

### What it is not

* **It is not the compatibility surface.** `crates/compat` registers the
  script-visible classes and owns every name a game sees. The engine speaks
  generic concepts — `BlendMode`, `Rect`, `BitmapState`, `LayerState` —
  and the compatibility layer maps the TVP-shaped vocabulary onto them. A host
  that does not want that layer never has to depend on it.
* **It is not a platform layer.** The engine has no window, no event loop, no
  audio device, no image codec and no file-system calls of its own. It drives
  the stateless platform services below it and owns the state above them.
  [§3](#3-what-you-supply-the-platform-side) says what a host must provide.
* **It is not a game runtime.** There is no script, no plugin, no save file
  and no archive format beyond what the storage namespace resolves.
* **It is not the whole engine state.** Some of the generic subsystems are
  standalone values the **host** owns and drives, not fields of `Engine`:
  `Mixer`, `TextEngine`, `VideoOverlay` and `Trans` are caller-owned
  values. `Engine` owns the scene, the timers, the input state, the clock,
  the decode pool, the storage read pool, the shader library and the
  presentation mapping.

### Boundaries worth knowing before you start

* **The archive format is XP3.** The storage namespace resolves plain
  directories and XP3 archives (`fs::xp3`), and its explicit-archive
  addressing uses the `archive.xp3>entry` spelling. The namespace itself is
  generic; the container is not.
* **The startup discovery is script-shaped.**
  `engine::startup::discover_startup` looks for `patch.tjs`,
  `startup.tjs`, `AfterStartup.tjs` and the `system/Initialize.tjs`
  fallback. It is a pure lookup, and a host that does not want those names
  does not call it.
* **The window/primary-layer pair is the screen.** The logical screen is the
  first window's primary layer; the framebuffer is the device surface. The
  presentation fits the first into the second.
* **The crates are workspace path dependencies, not published packages.**
  `engine`, `render`, `fs` and `api` carry the workspace version and the
  workspace license (`GPL-3.0-only`). A host vendors or forks the tree.

---

## 2. The dependencies

A host that uses the engine adds these crates:

```toml
[dependencies]
engine = { path = "path/to/krkr-rs/crates/engine" }
render = { path = "path/to/krkr-rs/crates/platform/render" }
api   = { path = "path/to/krkr-rs/crates/platform/api" }
fs    = { path = "path/to/krkr-rs/crates/platform/fs" }
raw-window-handle = "0.6"
```

| crate | why the host needs it |
|---|---|
| `engine` | the engine itself |
| `render` | the pipeline, the surface, the render state, the texture and the `RenderScene` types the engine's API names |
| `api` | the shared DTOs and the typed error enums (`InputEvent`, `BlendMode`, `RenderError`, `FsError`, `AudioSpec`) |
| `fs` | the file-system object the storage namespace is built over |
| `raw-window-handle` | the window handle the surface is created for, when the host has its own windowing |

### What `engine` pulls in

`crates/engine/Cargo.toml` names exactly these dependencies:

```
api, fs, render, cosmic-text, thiserror, log, ahash, hashbrown
```

and one optional dependency, `fearless_simd`, behind the `simd-experiment`
feature.

* `api`, `fs` and `render` are the **L1 type crates**: shared value types
  and stateless services. The engine names their types in its own public API
  (`RenderScene`, `RenderState`, `Surface`, `Texture`, `BlendMode`,
  `InputEvent`, `AudioSpec`), so a host that uses the engine's API uses
  them too.
* `cosmic-text` does the shaping, the bidi and the line breaking. The
  coverage rasterization itself is the L1 `render::FontSystem`.
* Through `render`, the engine transitively links the Vulkan loader
  (`ash`) and the font parsers (`ab_glyph`, `ttf-parser`) even when the
  null backend is the one in use. The null backend is a separate code path in
  the same crate, so the dependency is there either way.

### What the engine does **not** depend on

| not a dependency | consequence for the host |
|---|---|
| `compat` (the compatibility surface) | the script-visible names are not reachable through the engine; a host that wants them adds `crates/compat` and `crates/vm` |
| `vm`, `tjs` (the script VM) | the engine has no script engine and never evaluates a script |
| `platform` (window, event loop, audio device, clock) | the host supplies the surface, the events, the elapsed time and the audio device |
| `media` (image, audio and video decoding) | the host supplies an `ImageDecoder` and the decoded PCM it wants mixed |
| `app`, `android` (the composition roots) | the wiring is the host's own |
| `winit`, `cpal`, FFmpeg, MediaCodec | no windowing, audio or video backend is selected by the engine |

The engine crate itself declares `#![forbid(unsafe_code)]` and
`#![warn(missing_docs)]`.

---

## 3. What you supply: the platform side

The engine is a stateful middle layer over stateless services. A host
provides the bottom objects and the per-frame inputs; the engine provides
everything between them.

### The bottom render objects

The engine draws through the L1 render pipeline, so the host creates the
pipeline, the surface and the render state and hands them over:

```rust
let pipeline = render::create_render_pipeline(render::PipelineConfig::null())?;
let (surface, render_state) = render::begin_render_state(&pipeline, &handle)?;
```

* `create_render_pipeline` takes a `render::PipelineConfig` and returns a
  `RenderPipeline`. The `null` backend needs neither a GPU nor a display;
  the `vulkan` backend needs a window handle and, on X11/XCB/Wayland, a
  display handle as well — `render::begin_render_state_with_display` takes
  both.
* `begin_render_state` returns the `(Surface, RenderState)` pair. The
  engine takes both over and stores them; the host keeps no copy.
* The surface size is the **device** size in physical pixels. It is set on
  `EngineParts::surface_size` and can be changed later with
  `Engine::resize`.

### The per-frame inputs

| what | where it goes |
|---|---|
| the frame's elapsed time | the `FrameTimeSource` the host passes to `Engine::run` |
| input events | `engine::InputState::apply`, one event at a time |
| the decoded pixels of a loaded image | an `Arc<dyn ImageDecoder>` the host installs with `Engine::install_decoder` |
| the bytes of a storage read | an `Arc<dyn StorageReader>` the host installs with `Engine::install_storage_reader` |
| the audio device | outside the engine: the host renders the mixer into a buffer and writes it to its own device |
| a render-only debug overlay | `EngineParts::overlay`, an `engine::EngineOverlay` |

### The clock

The engine owns the clock it advances; the host owns where the elapsed time
comes from. The contract is one trait:

```rust
pub trait FrameTimeSource {
    fn elapsed_ms(&self) -> f64;
}
```

`engine::InjectedFrameTime` is the built-in implementation: a public
`elapsed_ms` field the host writes before every frame. The windowed loop in
this repository writes the real time the platform measured; the headless
runner writes the frame period it simulates.

### What this repository's own platform crate does

`crates/platform/platform` is the L1 platform layer: the window and its
event loop (`platform::run`, `EventHandler`, `PlatformEvent`), the audio
device (`AudioDevice`, `AudioOutputState`), the font directories and the
thread helpers. A host that has its own windowing and audio stack does not
need it. A host that wants this repository's desktop or Android front end
uses `crates/app` or `crates/android` as the composition root instead of
writing one.

---

## 4. A minimal composition root

The whole example: create the bottom objects, create the engine, put one
layer on the screen, drive one frame with an injected clock, tear down.

```rust
use engine::{Engine, EngineParts, GameFrame, InjectedFrameTime, Rect};
use raw_window_handle::{RawWindowHandle, WebWindowHandle};
use render::{PipelineConfig, begin_render_state, create_render_pipeline};

/// The upward hook: the one place the host's own per-frame work runs.
struct HostFrame;

impl GameFrame for HostFrame {
    fn before_render(&mut self, _now_ms: u64, _dt_ms: f64) -> Result<(), String> {
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. The bottom render objects. `PipelineConfig::null()` needs neither a
    //    GPU nor a window; a windowed host builds its own `PipelineConfig`
    //    and passes its window handle here.
    let pipeline = create_render_pipeline(PipelineConfig::null())?;
    let handle = RawWindowHandle::Web(WebWindowHandle::new(0));
    let (surface, render_state) = begin_render_state(&pipeline, &handle)?;

    // 2. The engine. It takes the surface and the render state over.
    let mut engine = Engine::new(EngineParts {
        pipeline,
        surface,
        render_state,
        surface_size: (640, 480),
        headless: true,
        overlay: None,
    })?;

    // 3. The scene: one window, one layer on it, and the layer's image.
    {
        let scene = engine.scene_mut();
        let window = scene.add_window("demo", (640, 480));
        let layer = scene.add_layer(window, None);
        if let Some(state) = scene.layer_mut(layer) {
            state.rect = Rect::new(0, 0, 640, 480);
        }
        assert!(scene.set_layer_image_size(layer, 640, 480));
    }

    // 4. One frame. The clock is injected: every timeline the frame derives
    //    comes from this one value.
    let clock = InjectedFrameTime::new(16.0);
    let mut hooks = HostFrame;
    let report = engine.run(&clock, &mut hooks)?;
    println!("frame {} presented {}", report.frame_index, report.presented);

    // 5. Teardown. Dropping the engine drops the surface and the render state.
    drop(engine);
    Ok(())
}
```

Every call in it, against the source:

| call | signature | source |
|---|---|---|
| `create_render_pipeline(PipelineConfig::null())` | `fn create_render_pipeline(config: PipelineConfig) -> Result<RenderPipeline, RenderError>` | `crates/platform/render/src/lib.rs` |
| `begin_render_state(&pipeline, &handle)` | `fn begin_render_state(p: &RenderPipeline, window: &RawWindowHandle) -> Result<(Surface, RenderState), RenderError>` | `crates/platform/render/src/lib.rs` |
| `Engine::new(EngineParts { .. })` | `fn new(parts: EngineParts) -> Result<Self, EngineError>` | `crates/engine/src/engine.rs` |
| `engine.scene_mut()` | `fn scene_mut(&mut self) -> &mut Scene` | `crates/engine/src/engine.rs` |
| `scene.add_window(title, size)` | `fn add_window(&mut self, title: impl Into<String>, inner_size: (u32, u32)) -> u32` | `crates/engine/src/scene.rs` |
| `scene.add_layer(window, parent)` | `fn add_layer(&mut self, window: u32, parent: Option<u32>) -> u32` | `crates/engine/src/scene.rs` |
| `scene.layer_mut(id)` | `fn layer_mut(&mut self, id: u32) -> Option<&mut LayerState>` | `crates/engine/src/scene.rs` |
| `Rect::new(x, y, w, h)` | `fn new(x: i32, y: i32, w: u32, h: u32) -> Self` | `crates/engine/src/scene.rs` |
| `scene.set_layer_image_size(id, w, h)` | `fn set_layer_image_size(&mut self, layer_id: u32, width: u32, height: u32) -> bool` | `crates/engine/src/scene.rs` |
| `engine.run(&clock, &mut hooks)` | `fn run(&mut self, clock: &dyn FrameTimeSource, hooks: &mut dyn GameFrame) -> Result<FrameReport, EngineError>` | `crates/engine/src/engine.rs` |
| `report.frame_index`, `report.presented` | `pub frame_index: u64`, `pub presented: bool` | `crates/engine/src/engine.rs` |

Two notes on the example:

* The layer's image is allocated by `set_layer_image_size`, which creates the
  bitmap when the layer has none. A layer with no image draws nothing.
* The window's client size and the framebuffer size are independent. Here they
  are both `640x480`, so the presentation is the identity; a host whose
  framebuffer differs gets the Fit mapping described in
  [§10](#10-presentation-the-fit-mapping).

---

## 5. The frame loop

The engine owns the frame. A host drives it with one of three entry points,
all of which take the upward hook:

```rust
pub fn run(&mut self, clock: &dyn FrameTimeSource, hooks: &mut dyn GameFrame)
    -> Result<FrameReport, EngineError>;
pub fn step(&mut self, dt_ms: f32, hooks: &mut dyn GameFrame)
    -> Result<FrameReport, EngineError>;
pub fn frame(&mut self, now_ms: u64, hooks: &mut dyn GameFrame)
    -> Result<FrameReport, EngineError>;
```

| entry point | the clock | use it for |
|---|---|---|
| `run` | advanced by the elapsed time the `FrameTimeSource` reports | the running loop: the host injects the real elapsed time |
| `step` | advanced by `dt_ms`, fractional milliseconds included | a loop that knows its own period |
| `frame` | **set** to `now_ms` exactly | a test that jumps between frames deterministically |

The frame runs in this order:

1. set the clock; fit the presentation;
2. deliver the due timers;
3. drain the storage read pool — the reads the previous frame scheduled are
   published before the hook runs;
4. run `hooks.before_render(now_ms, dt_ms)`;
5. wait for the pending image decodes — the image barrier;
6. fit the presentation again, because the hook may have resized a window's
   primary layer;
7. refresh the render-only overlay;
8. upload the dirty bitmaps and prune the stale textures;
9. project the scene into the VM-space `RenderScene`;
10. run `hooks.after_project(drawn_layers)`;
11. upload and project again if the hook changed the scene;
12. one `render::render` call.

### The upward hook

```rust
pub trait GameFrame {
    fn before_render(&mut self, now_ms: u64, dt_ms: f64) -> Result<(), String>;
    fn after_project(&mut self, _drawn_layers: &[u32]) -> Result<(), String> { Ok(()) }
}
```

This is the only place where behaviour from above the engine enters. The hook
runs **inside** the frame, after the timers and before the projection, so the
state it leaves in the scene is the state the frame draws. `after_project`
runs after the projection has reported which layers it emitted, so a hook that
changes the scene is followed by a second upload and projection and its
changes are in the frame being drawn.

A hook failure is returned to the caller as `EngineError::Frame`, and the
frame is abandoned without drawing.

### What the report says

`engine::FrameReport` is a plain value:

| field | meaning |
|---|---|
| `frame_index` | frames drawn so far, counting this one |
| `elapsed_ms` | the frame clock, with its fractional part |
| `presented` | whether the frame reached the surface |
| `stats` | the null backend's counters, or `None` on a GPU backend |
| `sync_ms`, `upload_ms`, `prune_ms` | the bitmap-to-texture costs |
| `project_ms` | the projection cost |
| `render_ms` | the pipeline's render cost |

A frame with `presented == false` updated the scene, but the display kept
the previous frame's contents. The engine records the run of such frames and
reports it; the host does not have to retry anything.

### What a host does between frames

The order the windowed loop in `crates/app/src/runner.rs` uses:

1. **Apply the pending resize.** The device framebuffer size changed, so
   `Engine::resize` rebinds the surface. The scene's window and layer sizes
   do not change: an OS resize moves no value a script observes.
2. **Fold the input events into the input state.**
   `InputState::begin_frame`, one `apply` per event, `end_frame`. See
   [§9](#9-input-the-state-and-the-frame-protocol).
3. **Drive the frame.** `engine.run(&clock, &mut hooks)`.
4. **Apply a script-side window resize.** The engine surfaces one through
   `Engine::take_window_resize`; the host resizes its own window and reports
   the new size back through `Engine::resize`.
5. **Mix and write the audio.** The mixer is advanced and rendered into the
   host's buffer, outside the engine frame. See
   [§8](#8-audio-the-mixer-and-the-device-callback).

The full contract — who owns the clock, what the hook may do, what the report
means, what a replacement engine must satisfy — is in
[`docs/code/engine/implementing.md`](implementing.md).

---

## 6. Driving the state

The scene is `engine::Scene`, reached through `Engine::scene()` and
`Engine::scene_mut()`. It holds four public vectors — `windows`,
`layers`, `bitmaps`, `fonts` — and a set of accessors that maintain the
id lookups, the sibling ordering, the focus domains and the two revision
counters.

### The safe order of operations

1. **Create the window.** `Scene::add_window(title, inner_size)` returns
   the window id. A window starts visible and opaque, with no primary layer.
2. **Create the layers.** `Scene::add_layer(window, parent)` attaches a
   layer to a window; `parent: None` attaches it directly to the window.
   `Scene::add_detached_layer` plus `Scene::attach_layer` is the two-step
   form, for a layer whose parent is not known yet. The first layer of a
   window becomes its primary layer.
3. **Set the geometry and the properties the projection reads.** `rect`,
   `visible`, `opacity`, `blend`, `image_left`, `image_top`,
   `image_width`, `image_height`, `hold_alpha`.
4. **Give the layer an image.** `Scene::set_layer_image_size(layer, w, h)`
   allocates the bitmap when the layer has none and resizes it when it has
   one; `Scene::ensure_layer_image(layer, min_w, min_h)` grows it to cover
   the layer. `Scene::replace_bitmap_image(id, w, h, rgba)` swaps the pixels.
5. **Order the siblings.** `Scene::set_order_index(id, order)`,
   `Scene::assign_front_order(id)`, `Scene::renumber_siblings(id)`.
6. **Move the focus.** `Scene::set_focus(id)` focuses in the window's input
   domain, `Scene::set_domain_focus(window, domain, target)` in a named
   one, `Scene::clear_focus(window)` blurs it.
7. **Remove.** `Scene::remove_layer(id)`, `Scene::remove_window(id)`,
   `Scene::remove_bitmap(id)`.

### Write through the accessors, not through the vectors

`Scene::layers`, `Scene::windows`, `Scene::bitmaps` and `Scene::fonts`
are public, and writing through them does **not** bump the scene's structure
revision. The projection compares that revision across frames and reuses the
projected tree when it did not move, so a change written straight into the
vector is not drawn until something else bumps it.

The accessors do bump it:

| accessor | what it bumps |
|---|---|
| `Scene::layer_mut(id)` | the structure revision, unconditionally |
| `Scene::update_layer(id, f)` | the structure revision, when `f` reports a change |
| `Scene::bitmap_mut(id)` | the content revision and the bitmap's content generation |
| `Scene::replace_bitmap_image(..)` | the content revision always; the structure revision when the size changed |
| `Scene::add_layer`, `add_window`, `add_bitmap`, `add_font`, `remove_*` | the structure revision |

`Scene::update_layer(id, f)` is the accessor to prefer: `f` returns
`true` when it actually changed the layer, and the revision moves only then,
so a setter that writes the value a layer already has costs nothing.

### The focus domains

A focus domain is one parentless root layer's manager. Every parentless layer
founds its own domain; a child inherits its parent's. `LayerState::domain`
is fixed at creation. The window's **input domain** is the first-registered
root's domain — `Scene::window_input_root(window)` returns it, and it is
the domain a window-level focus read and write and the input events bind to.

### The pixel operations and the rasterizer

The CPU pixel work is a set of free functions over a caller's
`&mut BitmapState`:

* `engine::layer_ops` — the fills, the blits, the colour conversions, the
  blur, the gamma, the 9-slice scaling, the subtree compositing.
* `engine::raster` — the polygon, polyline, hatch, arc and bézier
  rasterizer, and the shared `blend_pixel` compositing primitive.

Both are free functions: no globals, no caches, no locks. The code map is
[`layer_ops.md`](layer_ops.md) and [`raster.md`](raster.md).

---

## 7. Storage

The namespace is `engine::storage::Storage`, a plain value the caller owns.

### Mounting

```rust
let fs = fs::FileSystem::host(base_dir);
let storage = Storage::mount_archives(fs, base_dir, archives)?;
```

| function | behaviour |
|---|---|
| `Storage::mount_archives(fs, base_dir, archives)` | mounts each archive in order; **later archives win**; an archive that cannot be opened is a typed `MountError` |
| `Storage::mount_discovered_archives(fs, base_dir, archives)` | the same, but an archive that cannot be read or parsed is skipped and reported through `warn!` |
| `Storage::mount_archives_with(..)` / `mount_discovered_archives_with(..)` | the same, with a `MountOptions { preload_archives }` that reads each archive's whole image into memory in the background |
| `Storage::mount_archive(path)` / `unmount_archive(path)` | mount or unmount one archive after the fact |
| `Storage::set_project(path)` | names the project storage a relative auto path resolves against |

The mount list is **explicit**: the caller names the archives. The opt-in
exception is `engine::storage::discover_archives(file_system, base_dir)`, the
recursive scan for `*.xp3` under a directory, ordered by file name and then
shallower-first, with the shallower path mounting last and winning.

### Resolution order

`Storage::find(name)` returns `Result<Option<Location>, StorageReadError>` —
"not found" and a genuine failure stay distinguishable — and resolves in this
order:

1. **Explicit archive addressing** — `foo.xp3>data/script.ks`.
2. **The project storage** — the project archive for an archive project, the
   app directory for a folder project.
3. **The auto paths** — last added first.
4. **The remaining mounted archives** — later mounts first, so the archive
   mounted last wins.
5. **The app directory as a loose-file fallback** — archive projects only.

A hit is cached; a miss is not. A `..` component that escapes the mount is
`StorageReadError::InvalidPathName`, not a miss.

### Reading

```rust
pub fn read(&self, name: &str) -> Result<Vec<u8>, StorageReadError>;
pub fn read_prefix(&self, name: &str, max_len: usize) -> Result<Vec<u8>, StorageReadError>;
pub fn stat(&self, name: &str) -> Result<Option<StorageMetadata>, StorageReadError>;
pub fn exists(&self, name: &str) -> Result<bool, StorageReadError>;
pub fn list(&self, dir: &str) -> Result<Vec<String>, StorageReadError>;
pub fn file_list(&self, mask: &str, attr: i64) -> Result<Vec<String>, StorageReadError>;
```

The bytes are never served from a cache: a read the frame published is taken
once, so a later read of the same name reads the entry again and sees an
edited file.

### The read pool

A read is **scheduled, not cached**. `Storage::lookup(name)` resolves the
name, answers with the entry's existence and size, and schedules the raw read
and the archive decompression on a worker. `read` and `read_prefix` wait for
that read.

The pool is engine state:

```rust
let pool = engine.install_storage_reader(
    std::sync::Arc::new(engine::storage::FileSystemReader),
    engine::storage::StoragePoolConfig::default(),
)?;
storage.read_pool = Some(pool);
```

The engine drains the pool every frame, so a read the previous frame
scheduled is published before the next frame's work runs. Without a pool the
storage reads on the calling thread. The code map is
[`storage.md`](storage.md).

### The startup discovery

`engine::startup::discover_startup(&storage)` resolves the bootstrap
scripts without executing them, and returns a typed `StartupError` when
neither the searched script nor the fallback exists. A host with its own
bootstrap does not call it.

---

## 8. Audio: the mixer and the device callback

The engine has **no audio device**. `engine::sound::Mixer` is a value the
host owns; it renders the mix into the host's own buffer.

### The pieces

| type | role |
|---|---|
| `SoundSource` | a playable PCM source: decoded samples, or an `AudioStream` a decode-ahead worker fills |
| `SoundChannel` | one playback voice: play, stop, pause, loop, pan, volume, rate, position, fades |
| `SoundBuffer` | the loaded source, its loop flags and its label/status events |
| `Mixer` | the channels, the clock, the global volume and the focus gain |

### The model

The mixer is **clock-driven**. The host advances it:

```rust
mixer.advance(dt_seconds);            // or mixer.advance_to(now_seconds)
```

and renders it:

```rust
mixer.mix(&mut buffer, output_spec)?;         // samples without advancing
mixer.mix_advancing(&mut buffer, output_spec)?; // samples and advances by what it rendered
```

`mix` takes the output spec (`api::media::AudioSpec` — sample rate,
channel count) and fills the caller's interleaved `f32` buffer. A spec with
no sample rate, or a channel count other than one or two, is a typed
`SoundError::InvalidSpec` and leaves the buffer untouched.

### Where the device callback fits

Two shapes are in use, and the choice is the host's:

* **Render on the frame thread.** The host renders a chunk of the mix during
  the frame and writes it to the device. The mixer advances by the frames it
  rendered, so the mix and the wall clock stay together. This is what
  `crates/app/src/audio.rs` does.
* **Pull from the device callback.** The device's callback calls
  `Mixer::mix` into the buffer the callback owns. The callback must not
  block and must not take a lock the frame thread holds; the mixer is
  documented as being pulled from the device callback.

The device's queue depth is worth handing back to the mixer:
`Mixer::set_output_latency(seconds)` tells the mixer how far ahead of the
listener its rendered position runs, so a voice's played position can be
reported accurately.

### Events

`SoundBuffer::poll(&mut channel)` returns the `SoundEvent` values the host
observes — status changes, fade completion, label crossings. A channel's
status is `engine::sound::Status`. The code map is
[`sound.md`](sound.md).

---

## 9. Input: the state and the frame protocol

`engine::input::InputState` is the generic per-frame input state: pointer
position, buttons, wheel, key edges, touch contacts and the text buffer. It
carries **no** TVP or VK semantics — no key codes, no click-sequence
constants, no shift flags. Those belong to the layer above.

### The frame protocol

```text
begin_frame            // clear the release flags, the wheel, the last-pressed list
  apply(event) ...     // fold in every raw event
end_frame              // bump the held counters, advance the frame index
```

`InputState::apply(&mut self, event: &InputEvent)` takes one
`api::input::InputEvent`:

| variant | effect |
|---|---|
| `PointerMoved { x, y }` | sets the pointer position |
| `PointerLeft` | marks the pointer as outside |
| `MouseButton { button, pressed }` | press or release, with the click sequence |
| `Wheel { delta }` | accumulates the wheel |
| `Key { code, pressed }` | press or release of a platform-neutral key code |
| `Text(text)` | appends to the text buffer |
| `Touch { id, phase, x, y }` | updates a contact |

The per-frame edges are cleared by `begin_frame` and the held counters are
advanced by `end_frame`, so a host calls them exactly once per frame, around
the `apply` calls.

### Reading it

`pointer()`, `pointer_inside()`, `is_button_down(button)`,
`is_button_released(button)`, `button_hold_frames(button)`,
`button_click_count(button)`, `wheel()`, `is_key_down(code)`,
`is_key_released(code)`, `key_hold_frames(code)`, `last_pressed()`,
`held_keys()`, `released_keys()`, `text_input()`,
`touch_point_count()`, `touch_contact(id)`, `touch_velocity(id)`,
`touch_speed(id)`.

The engine reaches the state through `Engine::input()` and
`Engine::input_mut()`. The code map is [`input.md`](input.md).

---

## 10. Presentation: the Fit mapping

The scene is described in **VM pixels** — the size the game declared for its
logical screen, which is the first window's primary layer's rect. The
framebuffer is in **device pixels**. The host's own window client is a third
space.

### The rule

One rule, and it is the only one:

* scale the VM scene **uniformly** by `s = min(device_w / vm_w, device_h / vm_h)`;
* place it **centred**;
* clear the margin.

There is no mode enum and no configuration key. A size pair with a zero
dimension has no valid mapping: `PixelTranslation::fit` returns
`TranslateError::EmptyDimension` and the engine skips the frame rather than
substituting a scale.

### The value

```rust
pub struct PixelTranslation {
    pub scale: f32,          // device pixels per VM pixel
    pub offset: (f32, f32),  // device-space position of the VM origin
    pub vm_size: (u32, u32),
    pub device_size: (u32, u32),
}
```

`engine::PixelTranslation` is `Copy`, holds no clock and no cache, and is
recomputed whenever either size changes. It offers `to_device_point`,
`to_device_rect`, `to_vm_point`, `raster_scale`, `vm_pixel_region`,
`root_transform` and `inverse_root_transform`. The engine's current mapping
is `Engine::translation()`.

### What the host maps on the way in

The projected tree keeps **VM** coordinates and carries the Fit mapping on the
render scene's root transform, so the render pipeline composes it during
traversal without ever interpreting the VM semantics.

**Input travels the other way.** A pointer or touch position arrives from the
host in device pixels; the host maps it back into VM pixels before the input
state sees it, using the mapping the engine published:

```rust
if let Some(translation) = engine.translation() {
    let (x, y) = translation.to_vm_point((device_x, device_y));
    input.set_pointer(x, y);
}
```

The host's own event loop is where this happens. `crates/app/src/game.rs`
does it in the frame hook, mapping only the position-carrying events (pointer
move and touch) and passing every other event through unchanged.

**A device resize moves nothing in the scene.** `Engine::resize(size)`
rebinds the surface to a new device size; the window's client size and the
primary layer's rect are untouched, and the presentation refits the screen
into the new framebuffer. The other direction is a script-side window size:
the engine surfaces one through `Engine::take_window_resize()`, and the host
applies it to its own window.

The code map is [`translate.md`](translate.md); the architecture is
[`docs/arch/l2.md`](../../arch/l2.md) §4.

---

## 11. Errors

Every domain owns one error enum, and every enum is a **value**: there is
no panic on malformed input, no silent fallback, and no substituted default.

| enum | domain | variants |
|---|---|---|
| `engine::EngineError` | the frame | `Render`, `Overlay`, `Frame` |
| `engine::TranslateError` | the presentation | `EmptyDimension` |
| `engine::storage::MountError` | the archive mount | `NotADirectory`, `Fs`, `Xp3`, `ArchiveTooLarge` |
| `engine::storage::PreloadError` | a preloaded archive's image | `Truncated`, `Allocation`, `Io` |
| `engine::storage::StorageReadError` | the namespace reads | `NotFound`, `InvalidPathName`, `Fs`, `Xp3`, `Preload`, `NoResult` |
| `engine::storage::StoragePoolError` | the read pool | `Worker`, `Config`, `QueueFull`, `MemoryLimit` |
| `engine::startup::StartupError` | the startup discovery | `NoStartupScript`, `Fs`, `Storage` |
| `engine::timer::TimerError` | the timer registry | `UnknownTimer`, `InvalidInterval` |
| `engine::sound::SoundError` | the mixer and the sources | `InvalidSpec`, `UnsupportedChannels`, `InconsistentBuffer`, `NoSource`, `Decode`, `DecodeThread` |
| `engine::text::TextError` | the text engine | `UnknownFont`, `InvalidSize`, `InvalidFont`, `FontNotFound`, `AtlasFull`, `InvalidCoverage`, `MaskTooLarge`, `Glyph`, `Render` |
| `engine::video::VideoError` | the video overlay | `InvalidSpec`, `NotOpen`, `InconsistentFrame` |
| `engine::trans::TransError` | the transition machine | `InvalidDuration`, `InvalidVague`, `MissingRule`, `UnexpectedRule`, `InconsistentRule`, `SizeMismatch`, `InvalidOption` |
| `engine::imagedecode::DecodePoolError` | the decode pool | `Worker`, `Config`, `QueueFull`, `MemoryLimit`, `SizeOverflow`, `IdExhausted` |
| `api::error::RenderError` | the render service | `Unsupported`, `DeviceUnavailable`, `DeviceLost`, `SurfaceLost`, `SurfaceOutOfDate`, `OutOfMemory`, `InvalidArgument`, `Shader`, `Upload`, `Framebuffer` |
| `api::error::FsError` | the file system | `Io`, `InvalidState`, `NotFound`, `InvalidPath`, `Unsupported`, `PermissionDenied`, `AlreadyExists`, `NotADirectory`, `IsADirectory` |
| `api::error::MediaError` | the codecs | `Io`, `NotFound`, `Decode`, `Encode`, `Unsupported`, `InvalidData`, `InvalidState`, `EndOfStream` |
| `api::error::AudioError` | the audio device | `Unsupported`, `DeviceUnavailable`, `Open`, `Stream`, `InvalidSpec`, `AlreadyRunning`, `NotRunning` |
| `api::error::GlyphError` | the font system | `Decode`, `Unsupported`, `InvalidFont`, `MissingGlyph`, `Rasterize` |

All of these are `#[non_exhaustive]` except `DecodePoolError` and
`StoragePoolError`, which name every variant they have. A host that matches
on one of the non-exhaustive enums needs a wildcard arm:

```rust
match engine.run(&clock, &mut hooks) {
    Ok(report) => { /* the frame is done */ }
    Err(engine::EngineError::Render(error)) => return Err(error.into()),
    Err(other) => return Err(other.to_string().into()),
}
```

Two consequences worth stating plainly:

* **"Not found" and "failed" are different.** `Storage::find` returns
  `Result<Option<Location>, StorageReadError>`; a miss is `Ok(None)` and a
  broken file system is `Err(..)`.
* **A failed read is the storage's own error.** The read pool publishes the
  failure the worker saw; there is no substitute value and no empty result.

---

## 12. Testing your host

### The determinism contract

The frame's timeline comes from the caller; the only wall-clock reads inside
the frame are the `Instant` measurements behind the report's cost fields. The
elapsed time comes from the caller, so a test drives exactly the same code
path as the running game:

* `Engine::frame(now_ms, hooks)` **sets** the clock to an exact value, so a
  test can jump between frames. The delta handed to the hook is the distance
  from the previous frame's clock, and zero on the first frame.
* `Engine::step(dt_ms, hooks)` accumulates, keeping the fractional
  millisecond: sixty `step(16.6)` calls advance the clock by 996 ms.
* `Engine::run(clock, hooks)` takes the elapsed time from the source, and
  `InjectedFrameTime::new(ms)` is the source a test writes.

A test therefore needs no sleep and no wall-clock dependency: it names the
frame times it wants and asserts on what the frame produced. The timer
registry is deterministic for the same reason — time enters it only through
`TimerState::now_ms` and the frame's delivery step.

### The engine's own suites

`crates/engine/tests/` holds the integration suites, one file per module:
`engine.rs` (the frame), `clock.rs`, `scene.rs`, `bitmap.rs`, `dirty.rs`,
`oop_semantics.rs`, `pending_safety.rs`, `projection.rs`, `translate.rs`,
`layer_ops.rs`, `raster.rs`, `storage.rs`, `storage_async.rs`, `startup.rs`,
`sound.rs`, `video.rs`, `text.rs`, `text_render.rs`, `timer.rs`, `input.rs`,
`imagedecode.rs`, `shader_library.rs`, `shader_assets.rs`, `trans.rs`,
`trans_blend.rs`, `trans_effect.rs`, `trans_effect_gpu.rs` and the
`extrans_*.rs` provider suites. They run on the null backend with a headless
window handle, so they need neither a display server nor a GPU.

A host that only *uses* the engine does not have to run them. A host that
*replaces* a part of it does: the suites are the contract that part has to
keep. The per-module code map lists which suite covers which module.

### The headless runner as a smoke test

This repository's own binary runs an empty scene offscreen and prints the
null backend's counters:

```text
krkr --headless --frames 120 --backend null
```

It needs neither a display nor a GPU, and it exercises the whole frame path:
the pipeline, the surface, the presentation, the projection and the draw. A
host that wants the same smoke test builds the `app` crate and runs that
binary, or drives `Engine::run` over the null backend the same way.

The CLI options are `--headless`, `--frames N`, `--fps N`,
`--backend null|vulkan`, `--preload-archives`, `--graphic-cache-limit`,
`--archive XP3` (repeatable), `--base-dir DIR` and `--debug-overlay`. The
full list is in `crates/app/src/main.rs`.

---

## 13. A checklist for a new host

**Dependencies**

- [ ] The host depends on `engine`, `render`, `api`, `fs` and
      `raw-window-handle`, and on nothing from the compatibility layer.
- [ ] The host's own codec and device code lives outside the engine.

**Bottom objects**

- [ ] The pipeline, the surface and the render state are created by the host
      and handed to `Engine::new` in one `EngineParts`.
- [ ] The surface size is the device size in physical pixels.
- [ ] A window handle is available for the surface, and a display handle too
      when the backend needs one.

**The frame**

- [ ] The elapsed time comes from one `FrameTimeSource`, and every timeline
      the frame derives comes from it.
- [ ] The host implements `GameFrame` and does its per-frame work in
      `before_render`, after the timers and before the projection.
- [ ] The host reads `FrameReport::presented` and treats a frame that was
      not presented as a frame that changed nothing on screen.
- [ ] Input is folded in with `begin_frame` / `apply` / `end_frame`,
      exactly once per frame.
- [ ] Pointer and touch positions are mapped from device pixels into VM
      pixels through `PixelTranslation::to_vm_point` before the input state
      sees them.
- [ ] A device resize goes through `Engine::resize` and changes nothing in
      the scene; a script-side window size is taken from
      `Engine::take_window_resize` and applied to the host's own window.

**The scene**

- [ ] Every write goes through the scene's accessors, so the structure
      revision moves and the projection sees the change.
- [ ] A layer that should draw has an image: `set_layer_image_size` or
      `ensure_layer_image`.
- [ ] The audio is mixed into a buffer the host owns and written to the
      host's own device.

**Errors**

- [ ] Every fallible call is matched on its typed error; no wildcard arm
      swallows a variant that means something.
- [ ] "Not found" and "failed" are treated as different outcomes.

**Testing**

- [ ] The host's tests drive the frame with an injected clock and no sleep.
- [ ] The host's smoke test runs the frame on the null backend.

---

## Where to read next

| topic | document |
|---|---|
| the module map, one page per module | [README.md](README.md) |
| the frame contract and the obligations a replacement engine meets | [implementing.md](implementing.md) |
| the engine's architecture | [docs/arch/l2.md](../../arch/l2.md) |
| the layers and the crate map | [docs/arch/overall.md](../../arch/overall.md) |
| the public surface and the error enums | [docs/rules/api_rules.md](../../rules/api_rules.md) |
| the state ownership and the coordinate spaces | [docs/rules/arch_rules.md](../../rules/arch_rules.md) |
| the composition root in this repository | `crates/app/src/game.rs`, `crates/app/src/runner.rs` |

