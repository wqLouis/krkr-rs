# L1 — the platform core (`crates/platform/**`)

Source: `crates/platform/` · crates `api`, `fs`, `media`, `psd`, `render`,
`platform` · layer **L1**

L1 is **mechanism**: the Vulkan device, the FFmpeg/MediaCodec codecs, the raw
byte IO, the XP3 container parse, the window, the event loop, the clock, the
audio output and the process metrics. It knows nothing of layers, of TVP, of
TJS, of storage namespaces or of KAG.

The layer model is in [`overall.md`](../../arch/overall.md) and
[`arch_rules.md`](../../rules/arch_rules.md); the crate-by-crate view is in
[`l1.md`](../../arch/l1.md). This directory is the **code map**: what each
crate is, what it exposes, what it owns and how it fails.

---

## 1. The object/state split, in one paragraph

L1 owns the *service objects* (`FileSystem`, `RenderPipeline`, `Media`,
`AudioDevice`, `FontSystem`, `PlatformWindow`) — configuration only, their
actions take `&self`. L2 owns every *state value* (`RenderScene`,
`RenderState`, `Surface`, `FileState`, `AudioDecoderState`,
`VideoDecoderState`, the glyph atlas, the mixer) — each one defined by L1 and
handed back as an explicit `&mut` argument. The rule, its rationale and the
gate that enforces it are in [`arch_rules.md`](../../rules/arch_rules.md) §3.1
and [`l1.md`](../../arch/l1.md) §1; what follows is only which crate holds
what.

| crate | service objects (L1 owns) | state values (L2 owns) |
|---|---|---|
| `api` | — | — (DTOs and error values only) |
| `fs` | `FileSystem` | `FileState` |
| `media` | `Media` | `AudioDecoderState`, `VideoDecoderState` |
| `psd` | — (pure functions over bytes) | — |
| `render` | `RenderPipeline`, `FontSystem`, `Font` | `RenderState`, `Surface`, `RenderScene` (the tree L2 keeps current) |
| `platform` | `AudioDevice`, `PlatformWindow` | `AudioOutputState`, `Clock`, `Ticker` |

---

## 2. The crates

One document per crate. Each states the crate's responsibility, its layer and
dependencies, its public API, the state it owns, the platform gating, its
failure modes, its tests and its links.

| crate | source | one-line role | document |
|---|---|---|---|
| `api` | `crates/platform/api/src/` | the shared DTO crate: plain types and the typed `*Error` enums, no implementations | [api.md](api.md) |
| `fs` | `crates/platform/fs/src/` | the stateless file-system service, the `FileState` value and the XP3 container parse | [fs.md](fs.md) |
| `media` | `crates/platform/media/src/` | the stateless media service: image/audio/video decode and encode | [media.md](media.md) |
| `psd` | `crates/platform/media/psd/src/` | the stateless PSB/PSD container reader and image decoder | [media/psd.md](media/psd.md) |
| `render` | `crates/platform/render/src/` | the stateless render pipeline: the draw tree, the null/CPU backend and the Vulkan backend | [render.md](render.md) |
| `platform` | `crates/platform/platform/src/` | the host: window, event loop, input, clock, audio output, fonts, metrics | [platform.md](platform.md) |

`psd` is a **separate workspace member** inside the `media` directory; the
`media` package excludes it from its own file list (`exclude = ["psd"]`), so
the two never merge into one crate.

---

## 3. Dependencies

```
api  ←  fs, media, render, platform
psd  ←  (no api dependency; it owns its own error enum)
```

* `api` — `thiserror` only.
* `fs` — `api`, `thiserror`, `flate2` (XP3 zlib), `ahash`.
* `media` — `api`, `symphonia` (+ the Opus adapter), `image`, `ffmpeg-next`
  (optional), `log`; `ndk` is Android-only and target-gated.
* `psd` — `thiserror`, `log`, `flate2`, `forkunion`, `fearless_simd`.
* `render` — `api`, `thiserror`, `log`, `ash`, `ash-window`,
  `raw-window-handle`, `ab_glyph`, `ttf-parser`.
* `platform` — `api`, `raw-window-handle`, `thiserror`, `log`, `winit`
  (Linux, optional), `cpal` (Linux, optional); `ndk` is Android-only and
  target-gated.

No L1 crate depends on `engine` (L2), `compat` (L3), `vm`/`tjs` (L4) or a
composition root.

---

## 4. Platform gating

| crate | gating | effect |
|---|---|---|
| `api`, `fs`, `psd` | none | identical on every target |
| `media` | the `video-ffmpeg` (default) / `video-mediacodec` features | exactly one video backend; neither or both is a `compile_error!` |
| `media` | `cfg(target_os = "android")` for the `ndk` dependency and the extractor shim | the Android target links no FFmpeg; a non-Android host compiles a typed stub |
| `render` | none | the Vulkan backend builds on every target; the null backend needs no GPU |
| `platform` | the `platform-linux` / `platform-android` features (no default) | exactly one; both or neither is a `compile_error!` |
| `platform` | `cfg(target_os = "android")` for the `ndk` dependency | the AAudio backend is Android-only; elsewhere a typed stub keeps the API identical |

`uv run --project scripts krkr-gate android-target` resolves the dependency
graph per target without compiling and fails when a desktop-only backend
feature leaks into the Android target or when two mutually exclusive backends
are enabled together. The rule it protects is in
[`arch_rules.md`](../../rules/arch_rules.md) §12.

---

## 5. Coordinates

L1 speaks **device pixels** (the framebuffer / swapchain). L2 and L3 speak
**VM pixels**. The mapping is L2's (`engine::translate::PixelTranslation`); L1
never sees a VM coordinate and L3 never sees a device one. The contract is in
[`11-presentation-and-pixel-translation.md`](../../../docs.old/migration/11-presentation-and-pixel-translation.md)
§6; the space table is in [`overall.md`](../../arch/overall.md) §3.

---

## 6. Tests

| crate | tests |
|---|---|
| `api` | `crates/platform/api/tests/{blend,error,fs,geometry,glyph,host,media}.rs` |
| `fs` | `crates/platform/fs/tests/{host,memory,xp3}.rs` + `tests/common/mod.rs` |
| `media` | `crates/platform/media/tests/{audio,bmp_encode,image,media,tlg,video,yuv}.rs` |
| `psd` | `crates/platform/media/psd/tests/{decode,malformed}.rs` |
| `render` | `crates/platform/render/tests/{glyph,null,pipeline,scene,target,texture}.rs` |
| `platform` | `crates/platform/platform/tests/{audio,capabilities,clock,events}.rs` |

Unit tests live next to the code in `#[cfg(test)] mod tests` blocks. The
behaviour assertions in the L1 tests cite `reference/cpp/...:line`; the
crate's rules are in [`code-conduct.md`](../../../docs.old/code-conduct.md) and
[`AGENTS.md`](../../../AGENTS.md) §8.

The **main agent** runs the build, clippy and test gate; the compiler-free
checks (`stateless`, `error-handling`, `house-style`, `layering`,
`storage-boundary`, `storage-media`, `native-construction`, `lifecycle-order`,
`native-arity`, `void-getter`, `host-facts`, `android-target`) run without a
build and can be run from a worker.

---

## 7. Open findings that touch L1

The full list is in [`l1.md`](../../arch/l1.md) §8. The ones that shape the
crates documented here:

* **Device-pixel readback is not wired.** The backing is the
  device-resolution framebuffer; a VM-pixel read should sample the device
  region `[x·s, (x+1)·s) × [y·s, (y+1)·s)`. The transports are designed
  (Android: an `AHardwareBuffer` allocated first and imported into Vulkan,
  read with `AHardwareBuffer_lock` over the region; desktop and fallback: a
  host-visible staging copy) but neither is implemented. Today the script
  reads the CPU layer bitmap, which is already VM-exact.
* **No GPU glyph path.** `RenderContent::Glyphs` is rejected by both backends
  and `supports_glyph_atlas` is `false` everywhere, so text is soft at
  `scale > 1` — a rasterization approximation, not a script-observable
  difference.
* **No destination-dependent blend on Vulkan.** The eleven `DstBlend` modes
  are CPU-only; the Vulkan backend reports `RenderError::Unsupported` rather
  than substituting source-over.
* **`create_render_target` / `read_render_target` are plumbed by neither
  `engine` nor `compat`**, so a capture-style API has no path to a rendered
  texture.
* **The Vulkan descriptor pool is a fixed cap.** It exhausts past roughly
  1024 live textures; the pool should be sized from the live texture count.
* **No SAF or `AssetManager` backend** in `fs`; `FsPath::Uri` is reserved.
* **The C++ `fopen` bypass.** `tjs/cpp/streams.cpp` implements TJS
  `Array.save`/`load`/`saveStruct` with `std::fopen` directly, outside the
  stateless fs surface — the one IO path L1 cannot reach, and a known
  Android/scoped-storage hazard.
* **Android is compile-checked, never run.** No NDK is installed on the
  development host and none may be installed. The AAudio, MediaCodec and
  `AHardwareBuffer` paths are reviewed here and run on a machine that has an
  NDK; everything Android-only is documented as unverified.

---

## 8. Related documents

| topic | document |
|---|---|
| the layer model, the crate map, the coordinate spaces | [`overall.md`](../../arch/overall.md) |
| L1 as an architecture | [`l1.md`](../../arch/l1.md) |
| the layer rules, the state ownership rules, the error contract | [`arch_rules.md`](../../rules/arch_rules.md) |
| the Rust-level checklist | [`code_styles.md`](../../rules/code_styles.md) |
| the L1↔L2 protocol | [`14-l3-l2-protocol.md`](../../../docs.old/migration/14-l3-l2-protocol.md) |
| the render pipeline (deep evidence trail) | [`04-renderer-vulkan.md`](../../../docs.old/migration/04-renderer-vulkan.md) · snapshot [`docs.old/migration/04-renderer-vulkan.md`](../../../docs.old/migration/04-renderer-vulkan.md) |
| the media engine (deep evidence trail) | [`05-media-engine.md`](../../../docs.old/migration/05-media-engine.md) · snapshot [`docs.old/migration/05-media-engine.md`](../../../docs.old/migration/05-media-engine.md) |
| the file system (deep evidence trail) | [`06-file-system.md`](../../../docs.old/migration/06-file-system.md) · snapshot [`docs.old/migration/06-file-system.md`](../../../docs.old/migration/06-file-system.md) |
| the Android backends (deep evidence trail) | [`10-android-backends.md`](../../../docs.old/migration/10-android-backends.md) · snapshot [`docs.old/migration/10-android-backends.md`](../../../docs.old/migration/10-android-backends.md) |
| the Android host | [`../android/README.md`](../android/README.md) |
| the decisions D9–D41 | [`migration/README.md`](../../../docs.old/migration/README.md) §3 |
| Rust review rules | [`code-conduct.md`](../../../docs.old/code-conduct.md) |
| working rules for agents and humans | [`AGENTS.md`](../../../AGENTS.md) |
