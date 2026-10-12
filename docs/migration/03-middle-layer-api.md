# 03 — Middle-layer API (stateless objects + explicit state)

Status: design sketch. Signatures are illustrative Rust; no code is written
on this branch.

This is the contract between the middle layer (L2) and the bottom core (L1).
The bottom layer creates the **abstraction objects**; the middle layer stores
every **state value** and performs actions on those objects. For rendering,
L2 only maintains the **scene/layer object tree** and hands it to the L1
**render pipeline**, which does all traversal, composition and drawing. See
[`02-target-architecture.md`](02-target-architecture.md) §2.1 and §3.

---

## 0. Model — stateless objects, explicit state

### 0.1 Division of labour

```
L1  creates and owns  ·  the abstraction objects (services)
                        RenderPipeline · FileSystem · Media · FontSystem
                        AudioDevice · Clock · Texture
    handles             ·  all mechanism: Vulkan, FFmpeg/MediaCodec, font
                        rasterization, path IO, scene-tree traversal
    stores              ·  NO state

L2  stores              ·  every state value:
                        the layer system / RenderScene tree, RenderState,
                        SurfaceState, FileState, DecoderState, GlyphAtlas,
                        Mixer, caches, indexes, positions
    performs            ·  actions on the L1 objects, passing the state:
                        render_pipeline.render(&mut state, &scene)
                        media.read_audio(&mut dec_state, out)
                        fs.read_at(&mut file_state, off, buf)
```

An L1 object is a **facade over a platform capability** with no mutable
state of its own. Its methods are `&self` and take evolving state as an
explicit `&mut` parameter. The state is a bottom-defined value
(`RenderState`, `SurfaceState`, `DecoderState`, …) that L2 owns and stores.

### 0.2 What L1 must not do

* **No globals.** No mutable `static`/`thread_local`, `LazyLock`,
  `OnceLock`, `OnceCell`, `lazy_static`, no process-wide registries, no
  `Mutex<HashMap<...>>` caches.
* **No mutable fields on objects.** No `&mut self` that remembers anything
  for a later call. If something evolves, it is a state value, not an
  object field.
* **No hidden state between calls.** Output depends only on the object's
  immutable configuration, the arguments, and the passed-in state.
* **No `Id`-addressed tables.** L1 never resolves an integer id through a
  private map; L2 holds the state value directly.
* **No policy.** L1 does not cache, reuse, prefetch, coalesce, defer or
  throttle. Those are L2 decisions expressed in L2 state.
* **No TVP/TJS knowledge.** No script objects, no focus/hit-test, no
  storage naming. The render pipeline knows drawable structure and blend
  semantics, not what a KAG layer means.

### 0.3 Shape rules

1. **Bottom-created objects are stateless services.** `RenderPipeline`,
   `FileSystem`, `Media`, `FontSystem`, `AudioDevice`.
2. **All evolving state is an explicit value** (`RenderScene`, `RenderState`,
   `SurfaceState`, `FileState`, `DecoderState`, …), defined by L1, owned and
   stored by L2.
3. **Actions take state as a parameter.** `render(&self, state: &mut
   RenderState, scene: &RenderScene, …)`.
4. **The render pipeline consumes the scene tree.** L2 does not flatten or
   batch; it maintains the tree and passes it. Traversal, z-order,
   composition, batching, uploads and draw are L1.
5. **Pure transforms are free functions.** `decode_image(bytes)`,
   `yuv420_to_rgba(...)`, `rasterize_glyph(font, ...)`, resampling math.
6. **Caches are L2 state, passed in.** Glyph atlas, decoded assets,
   compiled pipelines, resolved faces live in L2.
7. **No callbacks into L2 from L1.** L1 returns owned data; L2 polls.
8. **Errors are values.** Typed per domain; no panics on bad input.

### 0.4 Why offload the render pipeline to L1

* L2 no longer owns a `Scene → RenderScene` projection, blend mapping, sprite
  positioning, clipping, batching or upload scheduling — those move to L1
  and are shared by every backend.
* The Bevy adapter and the Vulkan core both consume the same `RenderScene`
  tree, so the two renderers cannot drift in composition semantics.
* L2 stays a **state store + dispatcher**: it mutates the layer tree from
  script calls and calls `render`.
* "Could two engines run in one process?" stays **yes**: objects are
  immutable services, all state is local to the caller.

The current codebase does the opposite: it hides engine state in
process-global statics (`history/01-investigation.md §9`) and re-derives the tree
into Bevy entities every frame.

---

## 1. File system

`FileSystem` is a stateless root object; `FileState` is the open handle plus
the cursor, stored by L2.

```rust
#[derive(Clone)]
pub struct FileSystem { /* host root | in-memory root | URI root */ }

pub fn create_file_system(config: FsConfig) -> FileSystem;

pub fn open(fs: &FileSystem, path: &FsPath, mode: OpenMode) -> Result<FileState, FsError>;
pub fn stat(fs: &FileSystem, path: &FsPath) -> Result<FileStat, FsError>;
pub fn exists(fs: &FileSystem, path: &FsPath) -> bool;
pub fn list(fs: &FileSystem, path: &FsPath) -> Result<Vec<DirEntry>, FsError>;
pub fn create_dir_all(fs: &FileSystem, path: &FsPath) -> Result<(), FsError>;
pub fn remove_file(fs: &FileSystem, path: &FsPath) -> Result<(), FsError>;
pub fn copy(fs: &FileSystem, from: &FsPath, to: &FsPath) -> Result<(), FsError>;
pub fn rename(fs: &FileSystem, from: &FsPath, to: &FsPath) -> Result<(), FsError>;

pub struct FileState { /* fd/asset handle + logical cursor */ }

pub fn read_at(fs: &FileSystem, file: &FileState, offset: u64, buf: &mut [u8])
    -> Result<usize, FsError>;
pub fn write_at(fs: &FileSystem, file: &FileState, offset: u64, buf: &[u8])
    -> Result<usize, FsError>;
pub fn file_size(fs: &FileSystem, file: &FileState) -> Result<u64, FsError>;
pub fn flush(fs: &FileSystem, file: &FileState) -> Result<(), FsError>;
pub fn read_seq(fs: &FileSystem, file: &mut FileState, buf: &mut [u8])
    -> Result<usize, FsError>;

pub enum FsPath {
    Host(PathBuf),   // Linux, Android app-private dirs
    Uri(String),     // Android SAF / APK asset id
}
```

* The cursor is inside `FileState`, which L2 stores.
* The storage namespace, `arc.xp3>path` addressing, case-insensitive walk
  and mount index are L2 state; L1 has no namespace.
* `tvp-streams::binary::BinaryStream` becomes an L2 value wrapping a
  `FileState` + offset.

## 2. Render pipeline

### 2.1 The abstraction

L1 provides one stateless `RenderPipeline` object. It owns the entire render
mechanism: scene-tree traversal, parent transforms, z-order, opacity
composition, clipping, blend-state selection, batching, texture upload
scheduling, offscreen compositing and presentation.

L2 provides a `RenderScene` tree (its layer system, flattened into drawable
nodes) plus a `RenderState` value (GPU resource bookkeeping). It mutates the
tree from script calls and, once per frame, calls `render`. It never computes
a draw list.

```rust
/// Stateless render service. Created once, shared by reference.
pub struct RenderPipeline { /* device, swapchain mechanism, shader lib */ }

pub fn create_render_pipeline(config: PipelineConfig)
    -> Result<RenderPipeline, RenderError>;
pub fn capabilities(p: &RenderPipeline) -> GpuCapabilities;

/// L1-owned render state value, stored/reused by L2.
pub struct RenderState {
    /* swapchain, frame-in-flight, staging, compiled pipelines, texture
       uploads, offscreen targets, glyph atlas uploads */
}

pub fn begin_render_state(p: &RenderPipeline, window: &RawWindowHandle)
    -> Result<(Surface, RenderState), RenderError>;
pub fn resize(
    p: &RenderPipeline,
    surface: &Surface,
    state: &mut RenderState,
    size: (u32, u32),
) -> Result<SurfaceEvent, RenderError>;
```

### 2.2 The abstract scene/layer object tree

The tree is defined by L1 (so the pipeline can consume it) and owned by L2
(so the middle layer keeps all state). It carries drawable structure and
blend semantics only — no TVP properties.

```rust
pub struct RenderScene {
    pub root: RenderNodeId,
    pub nodes: Vec<RenderNode>,          // arena; L2 owns it
    pub background: Option<[f32; 4]>,
}

pub struct RenderNodeId(pub u32);

pub struct RenderNode {
    pub parent: Option<RenderNodeId>,
    pub children: Vec<RenderNodeId>,
    pub content: RenderContent,
    /// Node-local transform (translate/scale/rotate/pivot).
    pub transform: Transform2D,
    /// Destination rect in the parent's coordinate space.
    pub dst: RectF,
    /// Source sub-rect for textures (sprite-sheet frames, clipping).
    pub src: PixelRegion,
    pub opacity: f32,
    pub visible: bool,
    pub z: i32,
    pub blend: BlendMode,
    pub clip: Option<Clip>,
    /// `holdAlpha` / main-vs-province selection hook (TVP face).
    pub hold_alpha: bool,
}

pub enum RenderContent {
    None,                       // container only (e.g. ltBinder)
    Solid([f32; 4]),            // flat fill
    Texture(Texture),           // L1-created texture object
    Glyphs(GlyphRun),           // optional GPU text path
}

pub struct Transform2D {
    pub translate: (f32, f32),
    pub scale: (f32, f32),
    pub rotation: f32,          // radians
    pub pivot: (f32, f32),
}

pub enum Clip {
    Rect(RectF),
    Mask(Texture),              // alpha mask
}

pub enum BlendMode {
    SourceOver, Replace, Additive, Subtractive, AddAlpha, Multiply, Screen,
    DestinationDependent(DstBlend),
}

pub enum DstBlend {
    ColorDodge, ColorDodge5, ColorBurn, Darken, Lighten,
    Difference, Difference5, Exclusion, Overlay, HardLight, SoftLight,
}
```

### 2.3 Textures

Textures are L1 objects referenced by `RenderContent::Texture`; L2 stores the
handle and streams pixel updates into `RenderState`.

```rust
pub struct Texture { /* immutable image + view */ }

pub fn create_texture(p: &RenderPipeline, desc: TextureDesc) -> Result<Texture, RenderError>;
pub fn drop_texture(p: &RenderPipeline, state: &mut RenderState, tex: Texture);
pub fn upload_texture(
    p: &RenderPipeline,
    state: &mut RenderState,
    tex: &Texture,
    region: PixelRegion,
    pixels: &[u8],
) -> Result<(), RenderError>;
```

### 2.4 The render action

```rust
pub fn render(
    p: &RenderPipeline,
    surface: &Surface,
    state: &mut RenderState,
    scene: &RenderScene,
) -> Result<Presentation, RenderError>;
```

This single call replaces `sync_scene` + `LayerBlendMaterial` + `Sprite`
placement + `window_layer_order` + `visible_image_region` + every per-frame
entity/material operation. L2's only render responsibility is to keep the
tree current.

### 2.5 What L2 still owns semantically

The TVP layer system is an L2 structure **on top of** the render tree:

* every `LayerState`/`WindowState` property (masks, province, focus,
  hit-test, callbacks, `ImageLeft/Top/Width/Height`, `mainImage`,
  `provinceImage`, `face`, `holdAlpha`, `type`, …);
* CPU pixel operations (`layer_ops.rs`, `raster.rs`, TLG decode, text
  composition) and the `MainImage` screen-buffer read-back;
* the projection from the L2 layer tree into `RenderScene` nodes (one node
  per drawn layer, parent links, composed opacity/z/visible, resolved
  texture handle). This is a field copy, not a draw-list computation.

`BlendMode` is a closed enum: L2 maps its `LT_*` numbering to it (today's
`blend_mode_for`), and one L1 shader set implements it for every backend.

## 3. Glyph / font

`FontSystem` is the stateless abstraction; `Font` is immutable parsed data;
the atlas is L2 state.

```rust
pub struct FontSystem { /* font library / rasterizer backend */ }
pub fn create_font_system() -> FontSystem;

pub struct Font { /* parsed face, index */ }
pub fn load_font(fs: &FontSystem, bytes: &[u8], index: u32) -> Result<Font, GlyphError>;

pub fn rasterize_glyph(
    fs: &FontSystem,
    font: &Font,
    glyph: char,
    size_px: f32,
    style: GlyphStyle,
) -> Result<GlyphBitmap, GlyphError>;

pub fn glyph_advance(fs: &FontSystem, font: &Font, glyph: char, size_px: f32,
    style: GlyphStyle) -> f32;
pub fn font_metrics(fs: &FontSystem, font: &Font, size_px: f32) -> FontMetrics;

pub struct GlyphBitmap {
    pub width: u32, pub height: u32,
    pub coverage: Vec<u8>,   // 8-bit coverage, row-major
    pub bearing_x: f32, pub bearing_y: f32, pub advance: f32,
}
```

L2 stores the `GlyphAtlas`, `Font` values, layout engine and `fonts.json`
resolution. `paint_layout` composites coverage into the layer's
`BitmapState` and the result is uploaded as a normal texture, so
`getPixel`/`piledCopy` keep working. A later GPU text path emits
`RenderContent::Glyphs`; the API does not change.

## 4. Media

`Media` is the stateless backend object. Pure decodes are free functions;
streaming state is bottom-defined and L2-owned.

```rust
pub struct Media { /* backend configuration */ }
pub fn create_media(config: MediaConfig) -> Media;

// --- pure image / whole-file audio ---------------------------------------
pub fn probe_image(m: &Media, bytes: &[u8], hint: Option<&str>) -> Result<ImageInfo, MediaError>;
pub fn decode_image(m: &Media, bytes: &[u8], hint: Option<&str>) -> Result<DecodedImage, MediaError>;

pub fn probe_audio(m: &Media, bytes: &[u8], hint: Option<&str>) -> Result<AudioSpec, MediaError>;
pub fn decode_audio(m: &Media, bytes: &[u8], hint: Option<&str>) -> Result<DecodedAudio, MediaError>;

pub fn yuv420_to_rgba(/* layout */) -> Vec<u8>;

// --- streaming audio ------------------------------------------------------
pub struct AudioDecoderState { /* codec context + decode position */ }

pub fn open_audio(m: &Media, bytes: Arc<[u8]>, hint: Option<&str>)
    -> Result<(AudioSpec, AudioDecoderState), MediaError>;
pub fn read_audio(m: &Media, state: &mut AudioDecoderState, out: &mut [f32])
    -> Result<usize, MediaError>;
pub fn seek_audio(m: &Media, state: &mut AudioDecoderState, frame: u64)
    -> Result<(), MediaError>;

// --- video ----------------------------------------------------------------
pub struct VideoDecoderState { /* demuxer/codec context, current frame, audio */ }

pub fn open_video(m: &Media, bytes: Arc<[u8]>, hint: Option<&str>)
    -> Result<(VideoSpec, VideoDecoderState), MediaError>;
pub fn present_video(m: &Media, state: &mut VideoDecoderState, at_ms: u64)
    -> Result<RgbaFrame, MediaError>;
pub fn video_audio(m: &Media, state: &VideoDecoderState) -> Result<DecodedAudio, MediaError>;
```

* The codec context, decode position and current frame live in
  `AudioDecoderState`/`VideoDecoderState` — stored by L2, never by `Media`.
* Backends: FFmpeg on Linux, MediaCodec on Android; selection is by `cfg`
  and the object/state API is identical.
* Backend capability query is a stateless free function
  (`capabilities(m: &Media)`).

### 4.1 What stays in L2

* mixer and `WaveSoundBuffer`/`SoundBuffer`/`SoundChannel` state;
* `.sli` loop/label handling and fades;
* the whole-file-vs-stream decision and `Arc<[u8]>` byte sharing;
* the video-overlay state machine, timing anchors, event delivery;
* decoded-asset and image caches.

## 5. Audio device

`AudioDevice` is the stateless abstraction; `AudioOutputState` is the OS
stream state stored by L2.

```rust
pub struct AudioDevice { /* immutable device config */ }
pub struct AudioOutputState { /* stream, callback slot, running flag */ }

pub fn open_audio_device(spec: AudioSpec)
    -> Result<(AudioDevice, AudioOutputState), AudioError>;

pub fn start(
    dev: &AudioDevice,
    state: &mut AudioOutputState,
    pull: impl FnMut(&mut [f32]) + Send + 'static,
) -> Result<(), AudioError>;

pub fn stop(dev: &AudioDevice, state: &mut AudioOutputState);
pub fn is_running(state: &AudioOutputState) -> bool;
```

* `open`/`start`/`stop` run on the host thread (the stream may be `!Send`).
* Headless: no `AudioDevice`; L2 advances the mixer from the frame clock.

## 6. Input and host window

Host-produced; passed to L2 as plain snapshots. The host owns window state,
L2 owns the input/TVP state.

```rust
pub struct InputState {
    pub mouse_pos: (f32, f32),
    pub mouse_buttons: [bool; 3],
    pub keys: KeyState,
    pub wheel: f32,
    pub touch: Vec<TouchPoint>,
    pub text_input: String,
}

pub trait InputSource {           // implemented by L4 host glue
    fn state(&self) -> InputState;
}

pub trait HostWindow {
    fn logical_size(&self) -> (u32, u32);
    fn set_logical_size(&mut self, size: (u32, u32));
    fn set_title(&mut self, title: &str);
    fn request_redraw(&mut self);
    fn set_fullscreen(&mut self, on: bool);
}
```

TVP hit-testing, focus and gesture synthesis stay in L2; only the raw event
source is host glue.

## 7. Clock, logging, process

```rust
pub trait Clock { fn now_ms(&self) -> u64; }
pub struct SystemClock;
pub fn now_ms(clock: &impl Clock) -> u64;

pub trait LogSink { fn log(&self, record: &LogRecord); }

pub struct ProcessEnv {
    pub project_dir: PathBuf,
    pub app_data_dir: PathBuf,
    pub state_dir: Option<PathBuf>,
    pub screen_size: (u32, u32),
    pub touch_device: bool,
}
```

## 8. L2 is the state store and dispatcher

```rust
pub struct Engine {
    // ---- L1 stateless objects (shared services) ----
    pipeline: RenderPipeline,
    media: Media,
    fs: FileSystem,
    fonts: FontSystem,
    audio: Option<AudioDevice>,

    // ---- L2 state (all of it) ----
    scene: Scene,                // TVP layer system (all properties)
    render_scene: RenderScene,   // L1 tree, projected from `scene`
    render_state: RenderState,   // L1 GPU state, owned by L2
    surface: Surface,
    mixer: Mixer,
    glyph_atlas: GlyphAtlas,
    decoders: HashMap<SoundId, AudioDecoderState>,
    video: HashMap<VideoId, VideoDecoderState>,
    open_files: HashMap<FileId, FileState>,
    storage_index: StorageIndex, // namespace, arc.xp3>path
    pool: WorkerPool,            // L2-owned threads + completions
}

impl Engine {
    /// The whole frame (D26): advance timers — invoking their stored
    /// callbacks — mixer and video from `now`, run `hooks.before_render()`,
    /// project, render. Public and *deterministic*: tests and headless runs
    /// drive it directly with a virtual clock, so nothing sleeps.
    pub fn frame(&mut self, now_ms: u64, hooks: &mut dyn GameFrame);
    /// Sugar over `frame`: platform event pump + `SystemClock`.
    pub fn run(&mut self, clock: impl Clock, hooks: &mut dyn GameFrame);
    pub fn render(&mut self);             // project tree, call pipeline.render
    pub fn install_natives(&self, vm: &Tjs2Engine);
}

/// Declared in L2, implemented by L3/L4, wired by the composition root.
/// This is L2's only upward edge: it never names L3 or L4.
pub trait GameFrame {
    /// L4 `vm.tick` plus L3's KiriKiri event dispatch (async triggers,
    /// continuous handlers).
    fn before_render(&mut self);
}
```

The timer is the template for what L2 keeps (D27): no config struct, no mode
enum, no fire list to hand upward —

```rust
create_interval(interval_ms, callback) -> TimerId
set_interval(id, interval_ms)
set_enabled(id, enabled)
remove(id)
```

all state stays private to `Engine`, and the pump (`advance(now)`) is internal
to `frame`.

`Engine::render` is: sync dirty `scene` fields into `render_scene`, upload
dirty bitmaps into `render_state`, then a single `render(pipeline, surface,
render_state, render_scene)`. No draw-list construction in L2.

Tests construct `Engine` with a `NullRenderPipeline`, an in-memory
`FileSystem`, a stub `Media`, `SystemClock` and no window/codec.

## 9. Enforcing the model

* A CI check greps `krkr-core` for forbidden constructs (`static mut`,
  `thread_local!`, `LazyLock`, `OnceLock`, `lazy_static!`, `OnceCell`) and
  for `&mut self` methods on the L1 service objects.
* An integration test constructs **two independent `RenderPipeline`/`Media`/
  `FileSystem` objects and two `Engine`s** in one process; if L1 had globals
  or object state, this would alias or fail.
* Review rule: "could two engines run in one process?" Must be **yes**.
* Review rule: "does this object remember anything?" If yes, move it to a
  state value.
* Review rule for rendering: "does L2 compute a draw list or traverse the
  tree?" If yes, the pipeline is not doing its job.

## 10. What the API intentionally leaves out

* Storage namespaces / `.xp3` / auto-path search (L2).
* TVP `Layer.type` numbering (L2 maps to `BlendMode`).
* TVP layer properties, masks, province, focus/hit-test, script callbacks
  (L2, on top of `RenderScene`).
* `.sli` loop metadata, fade scheduling, 3D attenuation (L2 mixer).
* Glyph atlas, decoded-asset caches, compiled pipelines (L2 state).
* Video-overlay event model (`onFrameUpdate`/`onPeriod`) (L2).
* KAG, TJS values, exceptions, object registries (L3).
* Window menus and UI (L4 or L2-drawn, see `04-renderer-vulkan.md §7`).
