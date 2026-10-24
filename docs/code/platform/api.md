# `api` — the shared DTO crate

Source: `crates/platform/api/src/` · crate `api` · layer **L1**

`api` holds the types that more than one L1 crate and L2 must agree on, and
nothing else: **no implementations**. It is DTOs, plain enums and the typed
`*Error` values.

```rust
#![forbid(unsafe_code)]
#![warn(missing_docs)]
```

---

## 1. Responsibility

* the shared value types (geometry, blend, media, glyph, input, host, config);
* the typed error enums the L1 crates return.

`api` owns no state and starts no work. It is the vocabulary the other L1
crates speak.

---

## 2. Layer and dependencies

| | |
|---|---|
| Layer | L1 (bottom core) |
| Depends on | `thiserror` only |
| Must not depend on | every other crate, including `fs`, `media`, `render`, `platform` |
| Forbidden | `unsafe` (`#![forbid(unsafe_code)]`), process-global state, `#[allow(dead_code)]` |
| Lint | `#![warn(missing_docs)]` — every public item is documented |

The crate-level rules (no state in L1, one typed error enum per domain) are
in [`arch_rules.md`](../../rules/arch_rules.md) §3.1 and §6.

---

## 3. Modules

| module | contents |
|---|---|
| `api::blend` | `BlendMode`, the destination-dependent `DstBlend` family |
| `api::config` | the runtime configuration values: `CacheLimit`, `FrameRateLimit`, `RuntimeConfig`, `RuntimeConfigOverrides`, `parse_bool`, `parse_overrides`, `ConfigError` |
| `api::error` | the typed error enums: `FsError`, `MediaError`, `RenderError`, `GlyphError`, `AudioError` |
| `api::fs` | `FsPath`, `OpenMode`, `SeekFrom`, `FileStat`, `FileMetadata`, `DirEntry` |
| `api::geometry` | `RectF`, `PixelRegion`, `Affine2`, `Transform2D` |
| `api::glyph` | `GlyphBackend`, `GlyphStyle`, `GlyphBitmap`, `FontMetrics` |
| `api::host` | `TouchPhase`, `TouchPoint`, `SurfaceEvent`, `PresentSkip`, `Presentation` |
| `api::input` | `InputEvent`, `MouseButton`, `PhysicalKey` |
| `api::media` | `ImageInfo`, `DecodedImage`, `AudioSpec`, `DecodedAudio`, `RgbaFrame`, `VideoSpec`, `MediaCapabilities` |
| `api::shader` | the custom-shader **data** half: `ShaderStage`, `ShaderCode`, `ShaderModule`, `VertexLayout`, `SamplerKind`, `BindingKind`, `BindingDesc`, `EffectUniforms`, `EffectPipelineDesc`, `ShaderDataError` and the `MAX_EFFECT_*` budgets |

---

## 4. Public API

The crate is a flat list of modules; there is no crate-root re-export block.
A consumer names the module: `api::geometry::RectF`, `api::error::FsError`.

### `api::geometry`

```rust
pub struct RectF { pub x: f32, pub y: f32, pub width: f32, pub height: f32 }
pub struct PixelRegion { pub x: u32, pub y: u32, pub width: u32, pub height: u32 }
pub struct Affine2 { pub m11: f32, pub m12: f32, pub m21: f32, pub m22: f32, pub tx: f32, pub ty: f32 }
pub struct Transform2D { pub translate: (f32, f32), pub scale: (f32, f32), pub rotation: f32, pub pivot: (f32, f32) }
```

`RectF` is a float rectangle in a coordinate space; `PixelRegion` is an
integer pixel rectangle (a texture sub-rectangle, a lock region).
`Affine2`/`Transform2D` carry the node transform; `Transform2D::IDENTITY` and
`Affine2::IDENTITY` are the identity values.

### `api::blend`

```rust
pub enum BlendMode { SourceOver, Replace, Additive, Subtractive, AddAlpha, Multiply, Screen, Dst(DstBlend) }
pub enum DstBlend { /* ColorDodge, ColorDodge5, ColorBurn, Darken, Lighten, Difference, Difference5, Exclusion, Overlay, HardLight, SoftLight */ }
```

The eleven `DstBlend` modes read the destination colour, which fixed-function
blend state cannot express, so they are **CPU-only** today. The Vulkan
backend reports `RenderError::Unsupported` for them rather than substituting
source-over — see [render.md](render.md) §6.

### `api::fs`

```rust
pub enum FsPath { Host(PathBuf), Uri(String) }
pub enum OpenMode { Read, Write, Append, ReadWrite, Create, Truncate, CreateNew }
pub enum SeekFrom { Start(u64), Current(i64), End(i64) }
pub struct FileStat { … }
pub struct FileMetadata { … }
pub struct DirEntry { … }
```

`FsPath::Uri` exists so an Android SAF / APK-asset backend can be added
without an API change. **No such backend is wired yet** — see
[fs.md](fs.md) §5.

### `api::media`

```rust
pub struct ImageInfo { … }        // dimensions, has-alpha, format name
pub struct DecodedImage { … }     // pixels + spec
pub struct AudioSpec { pub sample_rate: u32, pub channels: u32, pub total_frames: Option<u64> }
pub struct DecodedAudio { … }     // spec + interleaved f32 samples
pub struct RgbaFrame { … }        // tightly packed RGBA8 + dimensions
pub struct VideoSpec { … }        // dimensions, fps, duration, audio-track params
pub struct MediaCapabilities { … }// the formats this build can decode
```

`DecodedAudio`'s samples are **interleaved per channel**
(`interleaved[frame * channels + channel]`).

### `api::input` and `api::host`

```rust
pub enum PhysicalKey { KeyA, …, Escape, Native(u32), … }
pub enum MouseButton { Left, Right, Middle, Back, Forward }
pub enum InputEvent { Key { code: PhysicalKey, pressed: bool },
                      MouseButton { button: MouseButton, pressed: bool },
                      PointerMoved { x: f32, y: f32 },
                      Touch { … } }
```

`PhysicalKey::Native(u32)` keeps an unmapped platform key's raw identity
instead of dropping or guessing at it; the compatibility layer maps a
physical key onto the reference virtual-key table, and only the L3 side knows
that table. The high bit that separates a raw native code from a named key
is crate-private: `PhysicalKey::index` is the only way to read it.

`api::host` holds the shapes the engine stores — `TouchPhase`, `TouchPoint`,
`SurfaceEvent` (`Lost`, `OutOfDate`, …), `PresentSkip` and `Presentation`.

### `api::host::Presentation` and `PresentSkip`

```rust
pub struct Presentation {
    pub presented: bool,
    pub surface_event: Option<SurfaceEvent>,
    pub skip: Option<PresentSkip>,
}

pub enum PresentSkip {
    AcquireTimeout,
}
```

A backend reduces its presentation to this shape. `presented == false` with a
`surface_event` names a surface fault the caller must react to; `skip` names
the case where the backend **abandoned the frame without a fault** — the
display did not release a swapchain image within the frame's presentation
budget. A skip is not an error: no work was submitted, the previous surface
contents stay on screen, and the caller counts the frame as skipped and
reports it. `skip` outranks `surface_event` when both are present, because a
backend that abandoned the frame before any GPU work carries no surface fault.

See [render.md](render.md) §4 for the budget and
[../engine/engine.md](../engine/engine.md) for the caller's accounting.

### `api::shader`

The **data half** of the custom-shader contract: the stage code, the
descriptor a pipeline is built from, the sampler and binding vocabulary, and
the uniform block a draw carries. It is plain data with no policy and no
backend knowledge; the policy half — the program ids, the pipeline cache, the
lifetime — is the middle layer's (`engine::shader`, see
[../engine/shader.md](../engine/shader.md)), and the backend that executes a
pipeline is `render`.

The code is SPIR-V, and `ShaderCode` has exactly one variant, so a caller
cannot believe a format exists that the backend cannot consume. The budgets
(`MAX_EFFECT_TEXTURES`, `MAX_EFFECT_UNIFORMS`,
`EFFECT_PUSH_CONSTANT_BYTES`) are **API** budgets, not driver limits: a
descriptor that exceeds one is refused with a `ShaderDataError`, never
truncated. The SPIR-V header words (`SPIRV_MAGIC`, `SPIRV_VERSION_MAX`) and the
reserved binding index (`RESERVED_BINDING`) are crate-private: the validation
that uses them is the contract, not the numbers.

### `api::config`

The runtime configuration values, shared by the desktop CLI, the desktop
config file and the Android launcher:

```rust
pub enum CacheLimit { Disabled, Bytes(u64) }
pub struct FrameRateLimit(u32);           // 1..=1000, default 60
pub struct RuntimeConfig { pub frame_rate_limit: FrameRateLimit,
                           pub preload_archives: bool,
                           pub debug_overlay: bool,
                           pub graphic_cache_limit: Option<CacheLimit> }
pub struct RuntimeConfigOverrides { … }
pub fn parse_bool(option: &'static str, value: &str) -> Result<bool, ConfigError>;
pub fn parse_overrides(text: &str) -> Result<RuntimeConfigOverrides, ConfigError>;
```

The accepted frame-rate range is 1..=1000 with a built-in default of 60,
and the accepted-values text the errors name is
`"an integer in 1..=1000 frames per second"`. Those numbers are
crate-private: `FrameRateLimit::new` and `FrameRateLimit::parse` validate
against them and the errors quote them, so a caller reads the range from the
error rather than from a constant. The configuration surface as a whole is
in [`runtime-config.md`](../../../docs.old/runtime-config.md).

---

## 5. State ownership

`api` owns nothing. Every type in it is a value: a DTO copied into a struct, a
state value L2 stores and hands back, or an error returned by value.

The crate declares no traits: a contract with a single implementor is an
inherent method, and a contract with no implementor is not a contract.

---

## 6. Platform gating

None. `api` is identical on every target and every feature combination: no
`#[cfg(target_os)]`, no feature gate.

---

## 7. Failure modes

The five error enums are the crate's whole error vocabulary. Every enum is
`#[non_exhaustive]`, so a new failure mode is a new variant and the call sites
that match exhaustively become compile errors until they handle it.

| enum | variants |
|---|---|
| `FsError` | `Io`, `InvalidState`, `NotFound`, `InvalidPath`, `Unsupported`, `PermissionDenied`, `AlreadyExists`, `NotADirectory`, `IsADirectory` |
| `MediaError` | `Io`, `NotFound`, `Decode`, `Encode`, `Unsupported`, `InvalidData`, `InvalidState`, `EndOfStream` |
| `RenderError` | `Unsupported`, `DeviceUnavailable`, `DeviceLost`, `SurfaceLost`, `SurfaceOutOfDate`, `OutOfMemory`, `InvalidArgument`, `Shader`, `Upload`, `Framebuffer` |
| `GlyphError` | `Decode`, `Unsupported`, `InvalidFont`, `MissingGlyph`, `Rasterize` |
| `AudioError` | `Unsupported`, `DeviceUnavailable`, `Open`, `Stream`, `InvalidSpec`, `AlreadyRunning`, `NotRunning` |

Two distinctions carry weight downstream:

* `RenderError::Unsupported` reports a *capability* this build or backend does
  not provide; `RenderError::DeviceUnavailable` reports a *runtime* failure
  while bringing the device up.
* `FsError::Io` is a device failure; `FsError::InvalidState` is a bad call
  (reading from a write-only handle, using a handle that belongs to a
  different backend).

`ConfigError` (in `api::config`) reports an unparseable or out-of-range
configuration value and names the option and its accepted values. The
absent-versus-invalid distinction the launcher relies on is in
[`../android/config.md`](../android/config.md) §3.

---

## 8. Tests

* `crates/platform/api/tests/blend.rs` — the default blend mode and the
  `DstBlend` wrapping.
* `crates/platform/api/tests/error.rs` — the display text of the error enums,
  the `MissingGlyph` code-point format, that `RenderError::DeviceUnavailable`
  is distinct from `Unsupported`, and that the errors are cloneable values.
* `crates/platform/api/tests/fs.rs` — the `FsPath` accessors and conversions,
  the `OpenMode` capabilities and the stat shape.
* `crates/platform/api/tests/geometry.rs` — `RectF` edges/center/min-max, the
  half-open `contains`, intersection and union; the `PixelRegion` helpers and
  its saturating intersection; the `Affine2` translation, scale, quarter
  turn and inverse round trip; the `Transform2D` pivot.
* `crates/platform/api/tests/glyph.rs` — the `GlyphBitmap` consistency, the
  `FontMetrics` line height and the default `GlyphStyle`.
* `crates/platform/api/tests/host.rs` — the default `Presentation`.
* `crates/platform/api/tests/media.rs` — the `DecodedImage`/`DecodedAudio`/
  `RgbaFrame` consistency, the frame count, the zero-channel safety, the
  `VideoSpec` audio detection and the default `MediaCapabilities`.

---

## 9. Related documents

| topic | document |
|---|---|
| the L1 overview | [README.md](README.md) |
| L1 as an architecture | [`arch/l1.md`](../../arch/l1.md) §1 |
| the state-ownership and error rules | [`arch_rules.md`](../../rules/arch_rules.md) §3.1, §6 |
| the Rust-level checklist | [`code_styles.md`](../../rules/code_styles.md) |
| the runtime configuration surface | [`runtime-config.md`](../../../docs.old/runtime-config.md) |
| the file system that consumes `api::fs` | [fs.md](fs.md) |
| the media crate that consumes `api::media` | [media.md](media.md) |
| the render crate that consumes `api::blend`/`api::geometry` | [render.md](render.md) |
| the host that consumes `api::input`/`api::host` | [platform.md](platform.md) |
| the Android launcher that consumes `api::config` | [`../android/config.md`](../android/config.md) |
