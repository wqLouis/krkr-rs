# 03 — Middle-layer API (stateless / functional)

Status: design sketch. Signatures are illustrative Rust; no code is written
on this branch.

This is the contract between the middle layer (L2) and the bottom core (L1).
It is deliberately small and TVP-free, and it is **stateless**: L1 computes,
L2 remembers. See
[`02-target-architecture.md`](02-target-architecture.md) §2.1 for the rule
and §5 for ownership.

---

## 0. Principle 0 — stateless and functional (D9)

L1 is a pure library over explicit inputs. All persistent state lives in L2.

### 0.1 What "stateless" forbids in L1

* **No globals.** No mutable `static`/`thread_local`, no `LazyLock`,
  `OnceLock`, `OnceCell`, `lazy_static`, no process-wide registries or
  singletons, no `Mutex<HashMap<...>>` caches.
* **No hidden state between calls.** A function's output depends only on its
  arguments and the explicit resource values passed to it.
* **No `Id`-addressed resource tables.** L1 does not keep
  `HashMap<u64, Texture>` and let L2 pass a raw id. If L2 owns a resource, it
  owns the RAII value (`Texture`, `FileHandle`, `AudioDecoder`, …), not an
  integer that L1 resolves.
* **No policy.** L1 does not decide to cache, reuse, prefetch, coalesce,
  defer, or throttle. Those are L2 decisions.
* **No engine knowledge.** No layers, no TVP types, no TJS, no scene.

### 0.2 What L1 is allowed to hold

Platform state that cannot be avoided is encapsulated in an **RAII value the
caller owns**:

* `Gpu` — instance/device/queue/allocator; created once by L2 and passed by
  reference to every operation;
* `Surface` — the window's swapchain; owned by L2 (the host);
* `Texture`/`Target` — GPU images; owned by L2;
* `FileHandle` — an open file/asset; owned by L2;
* `GlyphFace` — a parsed font; owned by L2;
* `AudioDecoder`/`VideoDecoder` — codec contexts; owned by L2;
* `AudioDevice` — the OS output stream; owned by L2.

The distinction is: these are *resources*, not *engine state*. L1 does not
remember anything across calls beyond what the value itself contains, and
the value's lifetime is managed by L2.

### 0.3 Shape rules

1. **Pure transforms are free functions.** `decode_image(bytes)`,
   `probe_audio(bytes)`, `yuv420_to_rgba(...)`, `rasterize_glyph(...)`,
   resampling, blend math. Inputs → output, no `self`.
2. **Impure operations take the resource explicitly.** `submit(&mut gpu,
   &mut cache, &list)`, `read_at(&file, offset, buf)`,
   `read_frames(&mut decoder, out)`.
3. **Caches and atlases are parameters, not globals.** `submit(gpu,
   &mut pipelines, &list)`; `compose_text(atlas, glyphs)`.
4. **All DTOs are plain owned/`Copy` data.** No `Arc<RwLock<…>>` crossing
   the boundary, no callbacks into L2 from L1.
5. **Errors are values.** Typed errors per domain; no panics for
   unsupported input.

### 0.4 Why this is worth the discipline

* **Mockable.** `NullGpu`, an in-memory `FsRoot`, a stub media backend make L2 testable
  with no window, GPU, codec or filesystem.
* **Parallel-safe.** Stateless L1 functions can run on an L2-owned thread
  pool with no locking and no hidden races.
* **Backend-consistent.** The Bevy adapter, the Vulkan core, FFmpeg and
  MediaCodec all implement the same pure operations, so semantics cannot
  drift into a cache.
* **The current pain disappears.** Today the "middle layer" is a pile of
  `static ENGINE`/`SCENE`/`GLOBAL_MIXER` registries
  (`01-investigation.md §9`). L1 must not reproduce that pattern.

---

## 1. File system

Stateless functions over an explicit root; the open handle is an RAII value.

```rust
/// Explicit backend/root configuration. Cheap to clone; carries no open
/// files and no indexes (an index would be L2 state).
#[derive(Clone)]
pub struct FsRoot { /* backend kind + base */ }

pub fn open(root: &FsRoot, path: &FsPath, mode: OpenMode) -> Result<FileHandle, FsError>;
pub fn stat(root: &FsRoot, path: &FsPath) -> Result<FileStat, FsError>;
pub fn exists(root: &FsRoot, path: &FsPath) -> bool;
pub fn list(root: &FsRoot, path: &FsPath) -> Result<Vec<DirEntry>, FsError>;
pub fn create_dir_all(root: &FsRoot, path: &FsPath) -> Result<(), FsError>;
pub fn remove_file(root: &FsRoot, path: &FsPath) -> Result<(), FsError>;
pub fn copy(root: &FsRoot, from: &FsPath, to: &FsPath) -> Result<(), FsError>;
pub fn rename(root: &FsRoot, from: &FsPath, to: &FsPath) -> Result<(), FsError>;

/// Open file/asset. Positional by design: the caller (L2) owns all cursor
/// state, so the handle need not be sequential and reads can be parallel.
pub struct FileHandle { /* fd / asset handle */ }

impl FileHandle {
    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, FsError>;
    pub fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<usize, FsError>;
    pub fn size(&self) -> Result<u64, FsError>;
    pub fn flush(&mut self) -> Result<(), FsError>;
}

pub enum FsPath {
    Host(PathBuf),   // Linux, Android app-private dirs
    Uri(String),     // Android SAF / APK asset id
}
```

* Cursor/stream state is **L2's**: a `tvp-streams` sequential stream is a
  thin L2 wrapper that remembers a `u64` offset and calls `read_at`
  (`06-file-system.md §3`).
* The storage namespace, `arc.xp3>path` addressing, case-insensitive walk
  and the mount index all stay in L2.
* Test backends: an in-memory `FsRoot` kind and mocks; no global
  registry.

> `tvp-streams::binary::BinaryStream` is the existing trait that fits here;
> the plan is to align it with `FileHandle` rather than keep two stream
> abstractions.

## 2. Renderer

`Gpu` is the explicit platform value (device/queue/allocator). Everything
else — pipelines, descriptor caches, atlases, texture tables — is L2 state
passed in or owned as an RAII handle.

```rust
/// Created once by L2; never a global.
pub struct Gpu { /* instance, physical/logical device, queue, allocator */ }

impl Gpu {
    pub fn new(config: GpuConfig) -> Result<Self, RenderError>;
    pub fn capabilities(&self) -> GpuCapabilities;
}

/// Swapchain; owned by the host (L2/L4).
pub struct Surface { /* vk surface, swapchain, images */ }

pub fn create_surface(
    gpu: &mut Gpu,
    window: &RawWindowHandle,
    size: (u32, u32),
) -> Result<Surface, RenderError>;

pub fn resize_surface(
    gpu: &mut Gpu,
    surface: &mut Surface,
    size: (u32, u32),
) -> Result<SurfaceEvent, RenderError>;

/// GPU images; owned by L2, one per live bitmap / effect target.
pub struct Texture { /* vk image + view + memory */ }
pub struct Target { /* vk image usable as sampled + color attachment */ }

pub fn create_texture(gpu: &mut Gpu, desc: TextureDesc) -> Result<Texture, RenderError>;
pub fn drop_texture(gpu: &mut Gpu, tex: Texture);
pub fn upload_texture(
    gpu: &mut Gpu,
    tex: &Texture,
    region: PixelRegion,
    pixels: &[u8],
) -> Result<(), RenderError>;

pub fn create_target(gpu: &mut Gpu, desc: TextureDesc) -> Result<Target, RenderError>;
pub fn drop_target(gpu: &mut Gpu, target: Target);

/// Pipelines live in an L2-owned cache; `submit` fills it lazily.
#[derive(Default)]
pub struct PipelineCache { /* owned by L2 */ }

pub fn begin_frame(
    gpu: &mut Gpu,
    surface: &mut Surface,
) -> Result<Frame, RenderError>;

/// Pure over `list`: the same list draws the same thing. Any persistent
/// cache is the caller's `PipelineCache`.
pub fn submit(
    gpu: &mut Gpu,
    frame: &mut Frame,
    pipelines: &mut PipelineCache,
    list: &RenderList,
) -> Result<(), RenderError>;

pub fn end_frame(
    gpu: &mut Gpu,
    surface: &mut Surface,
    frame: Frame,
) -> Result<Presentation, RenderError>;
```

### 2.1 The draw list (L2 output)

```rust
pub struct RenderList {
    pub target: TargetRef,          // Swapchain | Target(&Target)
    pub clear: Option<[f32; 4]>,
    pub quads: Vec<DrawQuad>,
}

pub struct DrawQuad {
    pub source: QuadSource,         // Texture(&Texture) | Solid
    pub src: PixelRegion,           // texel sub-rect
    pub dst: RectF,                 // framebuffer pixels
    pub transform: Option<Affine2>, // rotation/scale
    pub opacity: f32,
    pub tint: [f32; 4],
    pub blend: BlendMode,
    pub clip: Option<PixelRegion>,
    pub z: f32,
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

* Flattening is L2's job. The TVP `LT_*` numbering never reaches L1; L2 maps it.
* `BlendMode` is a closed enum, not an integer, so both backends (Bevy
  adapter and Vulkan core) implement the same set.
* Destination-dependent blends need an offscreen `Target` and a
  destination-sampling pipeline variant; that is an L1 detail triggered by
  the enum, not a policy cache.

## 3. Glyph engine

Rasterization is a pure function over a font handle. The **atlas is an L2
cache**, because caching is a decision.

```rust
/// Parsed face; RAII value owned by L2.
pub struct GlyphFace { /* ttf-parser/ab_glyph/swash face */ }

pub fn load_face(bytes: &[u8], index: u32) -> Result<GlyphFace, GlyphError>;

/// Pure: no atlas, no cache, no state beyond the face.
pub fn rasterize_glyph(
    face: &mut GlyphFace,
    glyph: char,
    size_px: f32,
    style: GlyphStyle,
) -> Result<GlyphBitmap, GlyphError>;

pub fn glyph_advance(
    face: &GlyphFace,
    glyph: char,
    size_px: f32,
    style: GlyphStyle,
) -> f32;

pub fn font_metrics(face: &GlyphFace, size_px: f32) -> FontMetrics;

pub struct GlyphBitmap {
    pub width: u32,
    pub height: u32,
    pub coverage: Vec<u8>,   // 8-bit coverage, row-major
    pub bearing_x: f32,
    pub bearing_y: f32,
    pub advance: f32,
}
```

L2 owns the `GlyphAtlas` (today's `tvp-text/src/atlas.rs`), the layout
engine, and the font-config resolution (`fonts.json`). `paint_layout`
composites coverage into the layer's `BitmapState`, so `getPixel`/`piledCopy`
keep working. An optional later phase uploads atlas cells as textures and
emits text quads through the same `RenderList`; the rasterization API does
not change.

## 4. Media

### 4.1 Pure decode functions

Image and whole-file audio decode need no context and are free functions:

```rust
pub fn probe_image(bytes: &[u8], hint: Option<&str>) -> Result<ImageInfo, MediaError>;
pub fn decode_image(bytes: &[u8], hint: Option<&str>) -> Result<DecodedImage, MediaError>;

pub fn probe_audio(bytes: &[u8], hint: Option<&str>) -> Result<AudioSpec, MediaError>;
pub fn decode_audio(bytes: &[u8], hint: Option<&str>) -> Result<DecodedAudio, MediaError>;

pub fn yuv420_to_rgba(/* layout */) -> Vec<u8>;
```

### 4.2 Streaming/video: RAII decoder values

Streaming needs a codec context; that context is an RAII value owned by L2,
and every operation takes it explicitly. There is no L1-side stream registry.

```rust
pub struct AudioDecoder { /* symphonia/ffmpeg/libopus context */ }

pub fn open_audio(
    bytes: Arc<[u8]>,
    hint: Option<&str>,
) -> Result<AudioDecoder, MediaError>;

impl AudioDecoder {
    pub fn spec(&self) -> AudioSpec;
    /// Advance the decoder; `out` receives interleaved f32. Progress lives
    /// in the decoder value, which L2 owns — not in an L1 table.
    pub fn read_frames(&mut self, out: &mut [f32]) -> Result<usize, MediaError>;
    pub fn seek(&mut self, frame: u64) -> Result<(), MediaError>;
}

pub struct VideoDecoder { /* ffmpeg / mediacodec context */ }

pub fn open_video(
    bytes: Arc<[u8]>,
    hint: Option<&str>,
) -> Result<VideoDecoder, MediaError>;

impl VideoDecoder {
    pub fn spec(&self) -> VideoSpec;
    pub fn present_at(&mut self, at_ms: u64) -> Result<RgbaFrame, MediaError>;
    pub fn audio(&mut self) -> Result<DecodedAudio, MediaError>;
}
```

### 4.3 Formats and backends

* Image: TLG5/6 + `image` formats; one implementation shared by backends.
* Audio: symphonia + libopus; FFmpeg/MediaCodec only where symphonia lacks
  a codec.
* Video: `ffmpeg` on Linux, `mediacodec` on Android.
* Backend selection is by `cfg`, and the selected backend must not change
  the API or introduce caching. `MediaError::Unsupported` replaces the
  scattered `#[cfg]` capability branches.

### 4.4 What stays in L2

* the mixer and `WaveSoundBuffer`/`SoundBuffer`/`SoundChannel` state;
* `.sli` loop/label handling and fades;
* the whole-file-vs-stream decision and the `Arc<[u8]>` byte sharing;
* the video overlay state machine, timing anchors and event delivery;
* the decoded-image/audio caches.

## 5. Audio device

The device is an RAII value; the mixer is L2. L1 just moves samples.

```rust
pub struct AudioDevice { /* cpal/AAudio stream + guard */ }

impl AudioDevice {
    pub fn open(spec: AudioSpec) -> Result<Self, AudioError>;
}

/// `pull` is L2-owned state; L1 calls it from the audio callback and writes
/// nothing of its own.
pub fn start(
    device: &mut AudioDevice,
    mut pull: impl FnMut(&mut [f32]) + Send + 'static,
) -> Result<(), AudioError>;

pub fn stop(device: &mut AudioDevice);
pub fn is_running(device: &AudioDevice) -> bool;
```

* `open`/`start`/`stop` run on the host thread because the stream may be
  `!Send` (today's `MainThreadOutputGuard`).
* Headless mode has no `AudioDevice`; L2 advances the mixer from the frame
  clock (`advance(dt)`), exactly as today.

## 6. Input and host window

Host-produced, passed to L2 as plain snapshots.

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

/// Injected at startup instead of read from process globals.
pub struct ProcessEnv {
    pub project_dir: PathBuf,
    pub app_data_dir: PathBuf,
    pub state_dir: Option<PathBuf>,
    pub screen_size: (u32, u32),
    pub touch_device: bool,
}
```

## 8. The engine (L2) owns everything

L1 has no engine object; L2 does. This is the same as
`02-target-architecture.md`, restated around the stateless API:

```rust
pub struct Engine {
    // L2 state — all of it.
    scene: Scene,
    mixer: Mixer,
    glyph_atlas: GlyphAtlas,
    pipelines: PipelineCache,
    bitmaps: HashMap<BitmapId, Texture>,   // L2 owns the handles
    decoders: HashMap<SoundId, AudioDecoder>,
    storage: Storage,                       // namespace over FsRoot
    // platform values owned by L2, not globally reachable
    gpu: Gpu,
    surface: Surface,
    audio: Option<AudioDevice>,
    pool: WorkerPool,                       // L2-owned
}

impl Engine {
    pub fn tick(&mut self, dt_ms: f32);
    pub fn render(&mut self);               // builds RenderList, calls submit
    pub fn install_natives(&self, vm: &Tjs2Engine);
}
```

Tests construct `Engine` with `NullGpu`, an in-memory `FsRoot`, pure stubs and
`MockClock`. No `static` is involved.

## 9. Enforcing D9

* A CI check greps `krkr-core` for forbidden constructs
  (`static mut`, `thread_local!`, `LazyLock`, `OnceLock`, `lazy_static!`,
  `OnceCell`) and for `pub fn` returning an integer id that indexes a
  module-level table.
* Every L1 function is unit-testable with plain inputs; an integration test
  constructs two independent `Gpu`/backend values in one process (if L1 had
  globals, this would fail or alias).
* Review rule: "could two engines run in one process?" For L1 the answer
  must be **yes**.

## 10. What the API intentionally leaves out

* Storage namespaces / `.xp3` / auto-path search (L2).
* TVP `Layer.type` numbering (L2 maps to `BlendMode`).
* `.sli` loop metadata, fade scheduling, 3D attenuation (L2 mixer).
* Glyph atlas, decoded-asset caches, pipeline caches (L2).
* Video-overlay event model (`onFrameUpdate`/`onPeriod`) (L2).
* KAG, TJS values, exceptions, object registries (L3).
* Window menus and UI (L4 or L2-drawn, see `04-renderer-vulkan.md §7`).
