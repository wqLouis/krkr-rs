# 03 — Middle-layer API

Status: design sketch. Signatures are illustrative Rust; no code is written
on this branch.

This is the contract between the middle layer (L2) and the bottom core (L1).
It is deliberately small and TVP-free. See
[`02-target-architecture.md`](02-target-architecture.md) §2 for the
ownership rules.

Design principles:

1. **Actions, not objects.** The API exposes verbs ("decode", "draw",
   "open"), not the KiriKiri object model.
2. **Plain data.** DTOs are `Copy`/`Clone`, no `Arc<RwLock<…>>`, no trait
   objects crossing thread boundaries, no TJS values.
3. **No callbacks into L2 from L1 threads.** L1 returns data/completions;
   L2 polls them on the host thread.
4. **Fallible and explicit.** Every OS/platform operation returns a typed
   error; missing capabilities are feature-detected, not panicked.
5. **One implementation per platform target.** Backends are selected at
   compile time (`cfg(target_os = "android")`) where possible.

---

## 1. File system

Raw byte IO only. Path namespace, archive addressing and case rules are L2.

```rust
pub trait FileSystem: Send + Sync {
    fn open(&self, path: &FsPath, mode: OpenMode) -> Result<Box<dyn Stream>, FsError>;
    fn stat(&self, path: &FsPath) -> Result<FileStat, FsError>;
    fn exists(&self, path: &FsPath) -> bool;
    fn list(&self, path: &FsPath) -> Result<Vec<DirEntry>, FsError>;
    fn create_dir_all(&self, path: &FsPath) -> Result<(), FsError>;
    fn remove_file(&self, path: &FsPath) -> Result<(), FsError>;
    fn copy(&self, from: &FsPath, to: &FsPath) -> Result<(), FsError>;
    fn rename(&self, from: &FsPath, to: &FsPath) -> Result<(), FsError>;
}

pub trait Stream: Send {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, FsError>;
    fn write(&mut self, buf: &[u8]) -> Result<usize, FsError>;
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, FsError>;
    fn size(&self) -> Result<u64, FsError>;
    fn flush(&mut self) -> Result<(), FsError>;
}

pub enum FsPath {
    /// A real host path (Linux, Android app-private dirs).
    Host(PathBuf),
    /// An opaque content URI / asset id (Android SAF, APK assets).
    Uri(String),
}
```

Notes:

* `Stream` is the same shape as the existing
  `tvp_streams::binary::BinaryStream` (`binary.rs:57`); the plan is to make
  that trait the shared definition rather than invent a second one.
* `list`/`stat` are enough for `Storages.getFileList` and case-insensitive
  discovery; L2 keeps doing the recursive walk if the backend cannot.
* A `MemoryFileSystem` and a `MockFileSystem` are first-class test
  implementations and are what L2 unit tests use.

Conversion of `FsError` to TJS exceptions stays in L3.

## 2. Renderer

### 2.1 Resources

```rust
pub trait Renderer: Send {
    type Texture: Copy + Eq + Hash + Send + Sync;

    fn create_texture(&mut self, desc: TextureDesc) -> Result<Self::Texture, RenderError>;
    fn update_texture(&mut self, tex: Self::Texture, region: PixelRegion, pixels: &[u8])
        -> Result<(), RenderError>;
    fn drop_texture(&mut self, tex: Self::Texture);

    fn begin_frame(&mut self, frame: FrameInfo) -> Result<(), RenderError>;
    fn submit(&mut self, list: &RenderList) -> Result<(), RenderError>;
    fn end_frame(&mut self) -> Result<Presentation, RenderError>;

    fn resize(&mut self, surface: SurfaceDesc) -> Result<(), RenderError>;
    fn create_target(&mut self, desc: TextureDesc) -> Result<Self::Texture, RenderError>;
}
```

* `TextureDesc`: width, height, format (`Rgba8UnormSrgb` to start),
  `sampled`, `render_target`.
* `RenderList`: ordered `DrawQuad`s (see below) + an optional clear color.
* `Presentation`: whether a swapchain image was acquired/presented, plus
  events (resize requested, surface lost) that L4 must handle.
* `Self::Texture` is an opaque handle owned by the renderer. L2 stores it in
  its bitmap records; L1 never sees `BitmapState`.

### 2.2 The draw list

The flattening of the layer tree is an L2 responsibility. L1 receives a flat,
back-to-front list:

```rust
pub struct RenderList {
    pub target: Target,
    pub clear: Option<[f32; 4]>,
    pub quads: Vec<DrawQuad>,
}

pub struct DrawQuad {
    /// Source texture, or `Solid` for a flat color.
    pub source: QuadSource,
    /// Source sub-rect in texels (sprite-sheet frames, clipping).
    pub src: PixelRegion,
    /// Destination rect in logical/framebuffer pixels.
    pub dst: RectF,
    /// Optional affine transform applied to `dst` (rotation/scale).
    pub transform: Option<Affine2>,
    /// Global alpha multiplied into the color.
    pub opacity: f32,
    /// Straight-alpha tint (white = no tint).
    pub tint: [f32; 4],
    pub blend: BlendMode,
    /// Scissor/clip in framebuffer pixels.
    pub clip: Option<PixelRegion>,
    /// Stable z (already sorted by L2; L1 preserves order).
    pub z: f32,
}

pub enum QuadSource {
    Texture(u64),        // opaque, renderer-resolved
    Solid,               // use `tint` as the fill color
}

pub enum BlendMode {
    /// Pure fixed-function, directly expressible in a Vulkan pipeline.
    SourceOver, Replace, Additive, Subtractive, AddAlpha,
    Multiply, Screen,
    /// Requires reading the destination: L1 renders to an offscreen target
    /// and ping-pongs. Listed explicitly so L1 can decide the strategy.
    DestinationDependent(DstBlend),
}

pub enum DstBlend {
    ColorDodge, ColorDodge5, ColorBurn, Darken, Lighten,
    Difference, Difference5, Exclusion, Overlay, HardLight, SoftLight,
}
```

`BlendMode` is a **closed** enum, deliberately not the TVP `LT_*` integer
space. The L2 mapping (`blend_mode_for` in today's
`render/src/blend.rs`) converts `Layer.type` → `BlendMode`, keeping the TVP
numbering out of L1.

### 2.3 Offscreen composition

For `DestinationDependent` modes and, later, GPU transitions, L1 needs
render targets:

```
// conceptual, not a trait method per se
let layer_rt = renderer.create_target(TextureDesc { w, h, sampled: true, .. });
renderer.submit(&RenderList { target: Target::Texture(layer_rt), quads: layer_quads, .. });
renderer.submit(&RenderList {
    target: Target::Swapchain,
    quads: vec![DrawQuad { source: QuadSource::Texture(layer_rt),
                           blend: BlendMode::DestinationDependent(..), .. }],
    .. });
```

This is the escape hatch that finally implements the 12
destination-dependent blend modes that today silently degrade
(`render/src/blend.rs`).

## 3. Glyph engine

Kept in L1 because rasterization is a platform/font-library concern, but it
is **pixel-output**, not GPU:

```rust
pub trait GlyphEngine: Send {
    fn load_face(&mut self, face: &FaceSpec) -> Result<FaceId, GlyphError>;
    fn rasterize(&mut self, face: FaceId, glyph: char, size_px: f32, style: GlyphStyle)
        -> Result<GlyphBitmap, GlyphError>;
    fn advance(&self, face: FaceId, glyph: char, size_px: f32, style: GlyphStyle)
        -> f32;
    fn metrics(&self, face: FaceId, size_px: f32) -> FontMetrics;
}

pub struct GlyphBitmap {
    pub width: u32,
    pub height: u32,
    /// 8-bit coverage, row-major.
    pub coverage: Vec<u8>,
    pub bearing_x: f32,
    pub bearing_y: f32,
    pub advance: f32,
}
```

L2's `layout` + `paint_layout` then composites `coverage` into the layer's
`BitmapState` exactly as today, preserving `getPixel`/`piledCopy`. The
`FaceSpec` resolution (`fonts.json`, `FaceRequest`) stays in L2 because it is
configuration, not platform.

If we later want GPU text, the same coverage data is uploaded to a glyph
atlas by L2 and emitted as `DrawQuad`s; the trait does not change.

> Open question for review: whether `GlyphEngine` should live in `krkr-core`
> or `krkr-render`. The plan places it in `krkr-render` (it needs the font
> library and benefits from being near the atlas), but it has no GPU
> dependency and could be a standalone `krkr-glyph` crate. See
> `07-roadmap.md §7`.

## 4. Media engine

One trait for image/audio/video decode. Backends differ by target.

```rust
pub trait MediaEngine: Send + Sync {
    // Images -----------------------------------------------------------------
    fn probe_image(&self, bytes: &[u8], hint: Option<&str>) -> Result<ImageInfo, MediaError>;
    fn decode_image(&self, bytes: &[u8], hint: Option<&str>) -> Result<DecodedImage, MediaError>;

    // Audio ------------------------------------------------------------------
    fn probe_audio(&self, bytes: &[u8], hint: Option<&str>) -> Result<AudioSpec, MediaError>;
    fn decode_audio(&self, bytes: &[u8], hint: Option<&str>) -> Result<DecodedAudio, MediaError>;
    fn open_audio(&self, bytes: Arc<[u8]>, hint: Option<&str>)
        -> Result<AudioStreamId, MediaError>;
    fn read_audio(&self, id: AudioStreamId, out: &mut [f32]) -> Result<usize, MediaError>;
    fn seek_audio(&self, id: AudioStreamId, frame: u64) -> Result<(), MediaError>;
    fn close_audio(&self, id: AudioStreamId);

    // Video ------------------------------------------------------------------
    fn open_video(&self, bytes: Arc<[u8]>, hint: Option<&str>)
        -> Result<VideoStreamId, MediaError>;
    fn video_info(&self, id: VideoStreamId) -> VideoSpec;
    fn present_video(&self, id: VideoStreamId, at_ms: u64) -> Result<RgbaFrame, MediaError>;
    fn video_audio(&self, id: VideoStreamId) -> Result<DecodedAudio, MediaError>;
    fn close_video(&self, id: VideoStreamId);
}
```

### 4.1 Decoded formats

```rust
pub struct DecodedImage { pub width: u32, pub height: u32, pub rgba: Vec<u8> }

pub struct AudioSpec {
    pub sample_rate: u32, pub channels: u16, pub total_frames: Option<u64>,
}

pub struct DecodedAudio { pub spec: AudioSpec, pub interleaved: Vec<f32> }

pub struct RgbaFrame { pub width: u32, pub height: u32, pub pts_ms: u64, pub rgba: Vec<u8> }

pub struct VideoSpec {
    pub width: u32, pub height: u32,
    pub fps: f32, pub total_frames: Option<u64>,
    pub sample_rate: u32, pub channels: u16,
}
```

### 4.2 Backends

```rust
#[cfg(target_os = "linux")]
pub type PlatformMedia = ffmpeg::FfmpegMedia;
#[cfg(target_os = "android")]
pub type PlatformMedia = mediacodec::MediaCodecMedia;
#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub type PlatformMedia = unsupported::UnsupportedMedia;
```

* `FfmpegMedia`: system FFmpeg (`ffmpeg-next` on Linux) for audio+video,
  `symphonia`/`image` for audio/image where already implemented.
* `MediaCodecMedia`: NDK `AMediaExtractor`/`AMediaCodec` video+audio, plus
  `symphonia`/`image` for the still-image and non-video audio formats.
* The image path is platform-independent; it can be a single
  `ImageDecoder` shared by both backends. TLG (`tvp-visual/tlg.rs`) moves
  into `krkr-media`.

### 4.3 Threading and the TJS poll contract

The current code already establishes the pattern to keep: synchronous
natives return promptly and script-visible events are delivered by polls
(`sound_poll`, `video_overlay_poll`, async bitmap completion). The media
engine therefore:

* accepts work synchronously or spawns workers internally;
* publishes completions tagged by a caller-supplied id;
* never touches TJS.

`AudioStreamId`/`VideoStreamId` are opaque engine handles (u64). The middle
layer maps them to TVP object ids.

## 5. Audio output

The mixer is L2 (it implements TVP channel semantics); the OS device is L1.

```rust
pub trait AudioOutput: Send {
    fn start(&mut self, spec: AudioSpec, pull: Box<dyn FnMut(&mut [f32]) + Send>)
        -> Result<(), AudioError>;
    fn stop(&mut self);
    fn is_running(&self) -> bool;
}
```

* L2 owns the `Mixer`; the `pull` closure writes interleaved `f32`.
* `start`/`stop` run on the host thread because the stream may be `!Send`
  (today's `MainThreadOutputGuard`, `tvp-sound/src/player.rs:16-27`).
* Headless mode uses no `AudioOutput`; L2 advances the mixer from the frame
  clock (`lib.rs advance(dt)`), exactly as today.
* Android AAudio comes through the same cpal/AAudio path; the backend type
  is chosen by target.

## 6. Input and host window

These are host-produced; the traits exist so L2/TJS can read state without
importing winit.

```rust
pub trait InputSource {
    /// Snapshot for the current frame; called on the host thread.
    fn state(&self) -> InputState;
}

pub struct InputState {
    pub mouse_pos: (f32, f32),
    pub mouse_buttons: [bool; 3],
    pub keys: KeyState,          // bitmap or press/release edges
    pub wheel: f32,
    pub touch: Vec<TouchPoint>,
    pub text_input: String,
}

pub trait HostWindow {
    fn logical_size(&self) -> (u32, u32);
    fn set_logical_size(&mut self, size: (u32, u32));
    fn set_title(&mut self, title: &str);
    fn request_redraw(&mut self);
    fn set_fullscreen(&mut self, on: bool);
}
```

The TVP hit-testing and gesture synthesis (`layer_at`, `touch_gesture`,
`input_bridge`'s button-delay logic) stay in L2; only the raw event source
moves to the host.

## 7. Clock, logging, process

```rust
pub trait Clock { fn now_ms(&self) -> u64; }

pub trait LogSink { fn log(&self, record: &LogRecord); }
```

Process concerns (app-data dir, exit request, state file) remain a small L2
or L4 service; the current `engine::state`/`runner::app_data_dir` split is
kept but moved behind a `ProcessEnv` struct injected at startup rather than
read from globals.

## 8. Dependency-injection shape

The engine is constructed once, explicitly, instead of through process
globals:

```rust
pub struct Engine {
    scene: Scene,             // L2 layer system
    mixer: Mixer,             // L2
    renderer: Box<dyn Renderer>,
    glyphs: Box<dyn GlyphEngine>,
    media: Arc<dyn MediaEngine>,
    fs: Arc<dyn FileSystem>,
    window: Box<dyn HostWindow>,
    input: Box<dyn InputSource>,
    clock: Box<dyn Clock>,
    audio: Option<Box<dyn AudioOutput>>,
}

impl Engine {
    /// The only frame entry point L4 calls.
    pub fn tick(&mut self, dt_ms: f32);
    /// Flatten the scene into a list and submit it.
    pub fn render(&mut self);
    /// Register L3 natives with the VM.
    pub fn install_natives(&self, vm: &Tjs2Engine);
}
```

Tests build an `Engine` with `MemoryFileSystem`, a `NullRenderer`, a stub
media engine and a `MockClock`. That is the mechanism that makes the
migration verifiable phase by phase.

## 9. What the API intentionally leaves out

* Storage namespaces / `.xp3` / auto-path search (L2).
* TVP `Layer.type` numbering (L2 maps to `BlendMode`).
* `.sli` loop metadata, fade scheduling, 3D attenuation (L2 mixer).
* Video overlay's `onFrameUpdate`/`onPeriod` event model (L2).
* KAG, TJS values, exceptions, object registries (L3).
* Window menus and UI (L4 or L2-drawn, see `04-renderer-vulkan.md §7`).
