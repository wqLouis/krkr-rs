# `psd` — the PSB/PSD container reader and image decoder

Source: `crates/platform/media/psd/src/` · crate `psd` · layer **L1**

`psd` is a **separate workspace member** inside the `media` directory. The
`media` package excludes it from its own file list (`exclude = ["psd"]`), so
the two crates never merge.

It parses the Emote **PSB/MDF** container and the Photoshop **PSD** document,
and decodes a PSD image into straight-alpha RGBA. It backs the PSB/PSD plugin
natives and the motion player.

---

## 1. Responsibility

| module | role |
|---|---|
| `container` | the PSB/MDF container: the header, the name and string tables, the typed value tree, the raw resource chunks, and the `decompress` helper (plain, zlib-wrapped `MDF`, and the Emote LZ4 frame) |
| `parse` | the PSD document: the header, the colour-mode data, the image resources, the layer/mask section and the merged composite, plus the `descriptor` helper |
| `decode` | the pixel work: expanding each channel (raw, PackBits or zip) and merging the colour planes into straight-alpha RGBA |
| `error` | the two typed error enums, `PsbError` and `PsdError` |
| `parallel`, `simd` | the fork-join schedule for the 8-bit channel expansion and the runtime-dispatched SIMD passes |

---

## 2. Layer and dependencies

| | |
|---|---|
| Layer | L1 (bottom core) |
| Depends on | `thiserror`, `log`, `flate2`, `forkunion`, `fearless_simd` |
| Must not depend on | `api`, `engine` (L2), `compat` (L3), a composition root |
| Forbidden | `unsafe` (`#![forbid(unsafe_code)]`), process-global state, a cached pool |
| Lint | `#![warn(missing_docs)]` |

`psd` owns its own error enums rather than reusing `api::error`: the crate is
self-contained by design, so `api` is not on its dependency list.

---

## 3. Public API

Every entry point is a free function that takes the bytes and returns the
parsed model or a typed error. Nothing is cached, nothing is registered and
no value outlives the call, so a caller can decode any number of documents
concurrently.

### The PSB container

```rust
pub fn parse(mut data: Vec<u8>, seed: i64) -> PsbResult<PsbDocument>;
pub fn parse_stream(raw: &[u8], seed: i64) -> PsbResult<PsbDocument>;

pub struct PsbDocument { … }
pub enum PsbValue { … }        // the typed value tree
pub struct ResourceRef { … }   // a reference into the raw resource chunks
```

`parse` takes the decompressed container bytes; `parse_stream` takes the raw
stream and handles the `MDF`/`MFL` archive wrapper itself. `seed` is the
Emote decryption seed; an encrypted file with no seed is
`PsbError::Encrypted`.

The document is parsed eagerly into the `PsbValue` tree, and the raw
decompressed bytes stay available so the chunk reads and the string lookups
are answered from the same document.

### The PSD document

```rust
pub fn parse(bytes: &[u8]) -> PsdResult<PsdFile>;

pub struct PsdFile { … }
pub struct Header { … }
pub struct ChannelInfo { … }
pub struct LayerInfo { … }
pub struct LayerMask { … }
pub struct AdditionalLayer { … }
pub struct LayerExtra { … }
pub struct LayerCompInfo { … }
pub struct ImageResource { … }
pub struct SliceItem { … }
pub struct SliceResource { … }
pub struct GuideItem { … }
pub struct GridGuideResource { … }
pub struct ColorTable { … }
pub struct LayerComp { … }
pub enum LayerType { … }
pub enum BlendMode { … }
pub enum ImageMode { … }
```

The colour-mode constants are public: `COLOR_MODE_BITMAP = 0`,
`COLOR_MODE_GRAYSCALE = 1`, `COLOR_MODE_INDEXED = 2`, `COLOR_MODE_RGB = 3`,
`COLOR_MODE_CMYK = 4`, `COLOR_MODE_MULTICHANNEL = 7`,
`COLOR_MODE_DUOTONE = 8`, `COLOR_MODE_LAB = 9`.

### The pixel decode

```rust
pub struct LayerImage {
    pub left: i32, pub top: i32,
    pub width: u32, pub height: u32,
    pub opacity: u8,
    pub blend: BlendMode,
    pub pixels: Vec<u8>,          // straight-alpha RGBA, row by row
}

pub fn layer_image(psd: &PsdFile, layer: &LayerInfo) -> PsdResult<LayerImage>;
pub fn merged_image(psd: &PsdFile) -> PsdResult<Option<(u32, u32, Vec<u8>)>>;
```

`merged_image` returns `None` when the document carries no merged composite.

---

## 4. State ownership

The crate holds no state. The metadata walk is a single serial cursor pass;
the pixel work is the only part worth spreading:

* `decode` expands a document's 8-bit channels on a **short-lived fork-join
  pool** when the decode is large enough to pay for it
  (`PARALLEL_DECODE_BYTES = 256 KiB`);
* the 8-bit planar-to-RGBA and the mask-to-alpha passes run through
  runtime-dispatched SIMD (`fearless_simd`);
* the pool is created **per decode call** and never cached.

Both paths produce the same bytes as the scalar path — the thresholds only
choose a schedule, never the output.

---

## 5. Platform gating

None. `psd` is pure Rust and compiles identically on every target.

**One caveat:** `forkunion` has a C++ core, so the host build compiles but the
aarch64 Android build and run are **unverified** — the same limitation the
whole Android target carries (see
[`../../platform/README.md`](../../platform/README.md) §7 and
[`../../android/build.md`](../../android/build.md) §6).

---

## 6. Failure modes

Two typed enums, both `#[non_exhaustive]`.

`PsbError`:

| variant | meaning |
|---|---|
| `BadSignature([u8; 4])` | the four-byte magic was not `PSB\0` |
| `ArchiveNotPsb` | the stream starts with the `MDF`/`MFL` archive magic |
| `UnsupportedVersion(u16)` | the format version is outside the supported `1..=3` range |
| `Encrypted` | the header advertises an encrypted file and no seed was supplied |
| `Truncated(&'static str)` | the stream ended before the requested field could be read |
| `Format(String)` | the stream is structurally invalid (bad type byte, bad index, …) |
| `Lz4(String)` | the embedded Emote LZ4 frame or block was invalid |
| `Decompress(String)` | the MDF zlib payload could not be inflated |

`PsdError`:

| variant | meaning |
|---|---|
| `Eof` | the input ended before a required field could be read |
| `Invalid(String)` | the file is structurally invalid |
| `Decompress(String)` | a compressed channel could not be expanded |

The reader **never returns a partially parsed document**: every failure is
reported, and a damaged channel or descriptor is an error, never a
zero-filled buffer.

---

## 7. Tests

* `crates/platform/media/psd/tests/decode.rs` — a hand-built one-layer PSD:
  the RGB planes merging into RGBA, the grayscale plane repeated across the
  colour channels, the masked layer folding `alpha * mask / 255` over the
  overlap of the layer and mask rectangles, and the parallel expansion path
  producing the same pixels as the scalar one.
* `crates/platform/media/psd/tests/malformed.rs` — the wrong PSD signature, the
  short header, the truncated layer/mask section, the bad image-resource
  signature, the truncated merged image, the short PSB stream, the unknown PSB
  magic and the MDF archive.

---

## 8. Related documents

| topic | document |
|---|---|
| the L1 overview | [README.md](../README.md) |
| L1 as an architecture | [`arch/l1.md`](../../../arch/l1.md) §5 |
| the sibling media crate | [media.md](../media.md) |
| the plugin natives this crate backs | [`../../compat/plugins/psb_file.md`](../../compat/plugins/psb_file.md) · [`../../compat/plugins/psd_file.md`](../../compat/plugins/psd_file.md) |
| the Rust-level checklist | [`code_styles.md`](../../../rules/code_styles.md) |
