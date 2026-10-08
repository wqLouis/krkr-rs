# Performance opportunities (new layered architecture)

Status: **forward-looking backlog.** **Deferred by D22** until a real game runs
end-to-end — correctness first, then invest. This complements the historical audits
[`../optimization.md`](../optimization.md) and [`../performance.md`](../performance.md),
which measured the *old* Bevy/tvp-* tree and are kept only as a record. Numbers
here are not measured yet; they are the places we know we can win, ordered by
expected impact for real KiriKiri titles (text-heavy, many TLG layers, long BGM).

The rule for every item below: **the CPU path is the correctness reference**
(D8). An optimisation may change *how* pixels are produced, never *what* a
script can read back through `getPixel`/readback.

## L1 `render`

| Opportunity | Why it matters | Notes |
|---|---|---|
| **GPU glyph runs** (`RenderContent::Glyphs`) | Today L2 emits **one `RenderContent::Texture` node per glyph**; a KAG screen with 2 000 characters = 2 000 nodes, transforms and draw submissions. This is the single biggest text cost. | Expand a run into **one instanced draw** sampling the L2 atlas. Vulkan backend currently returns `Unsupported` for `Glyphs` (`vulkan/mod.rs:30`); null counts it (`null.rs:546`). |
| **Programmable/advanced blends** (`VK_EXT_blend_operation_advanced`) | Dest-dependent `DstBlend` modes (Multiply/Screen/Overlay/ColorDodge/…) are CPU-only today; GPU scenes fall back or return `Unsupported`. | Exposes most modes as hardware blend ops; keep the CPU path for `ColorDodge5`/`Difference5` and GPUs lacking the feature. |
| **MSDF atlas** | Crisp text at arbitrary zoom/rotation without re-rasterising per size (KAG effects scale layers). | Generate once (CPU `msdfgen`/Rust port or compute pass); shader derives coverage. |
| **Async readback** | `read_render_target` waits on a fence; a script that samples every frame stalls the CPU↔GPU sync. | Ring-buffer of readback targets + deferred map; return the previous frame's pixels. |
| **Batching / pipeline cache** | Fewer pipeline switches and vertex uploads per frame. | Batch by `(texture, blend, clip)`; reuse the staging buffer; cache `VkPipeline` by state. |

## L1 `media`

| Opportunity | Why it matters | Notes |
|---|---|---|
| **Hardware video decode** | Software FFmpeg decode of 1080p is the dominant startup/scrub cost. | VAAPI/NVDEC on Linux, MediaCodec on Android; zero-copy frames into the render path. |
| **Streaming `video_audio`** | The current `video_audio` decodes the **whole** track eagerly (the scaffold has no range parameter). | Add a range/streaming API and decode on demand. |
| **SIMD image/TLG decode** | TLG is the most common KiriKiri image; startup decodes dozens. | SIMD predictor + colour conversion; parallel decode with a bounded pool. |
| **Faster inflate** | XP3 entries and `FE FE` script containers use zlib. | `libdeflate`/`zlib-ng` instead of the default backend. |

## L1 `fs`

| Opportunity | Why it matters | Notes |
|---|---|---|
| **Index lookup** | `find`/`stat` run per storage open; a title can open thousands per boot. | Hash the XP3 index once; avoid repeated linear scans; cache decoded paths/offsets. |
| **Decompress cache** | Re-opening the same entry re-inflates it. | Small LRU of decoded entries keyed by `(archive, offset)`; never mutate the mounted set. |
| **Memory-map archives** | Large direct-from-disk reads. | mmap the container on desktop; `pread` on Android. |

## L2 `engine`

| Opportunity | Why it matters | Notes |
|---|---|---|
| **Glyph cache by coverage, not colour** | The atlas key includes the **colour** today, so every text-colour change re-rasterises and re-uploads. KAG text recolours constantly. | Cache straight-alpha coverage only and tint per node (the Vulkan shader already takes a push-constant tint — `vulkan/draw.rs:34`); the per-glyph tint becomes the text colour. **Highest text-cache win.** |
| **Layout cache** | `cosmic-text` shaping runs every frame for unchanged labels. | Key by `(face, text, size, style, wrap-width)`, invalidate on change. |
| **Multi-page atlas + eviction** | One fixed atlas hits `AtlasFull` and then fails. | Grow to several pages; LRU-evict cold glyphs. |
| **Run batching** | Feed `RenderContent::Glyphs` once the L1 path exists. | Removes the per-glyph node overhead. |
| **Scene projection dirty flags** | `project` rebuilds the whole `RenderScene` each frame. | Track per-layer dirty bits; reuse unchanged subtrees. |
| **Mixer SIMD + quality** | Software mixing of several BGM/voice channels. | SIMD the gain/pan/resample inner loops; make resampler quality configurable. |

## L3 `compat` / L4 `vm`

| Opportunity | Why it matters | Notes |
|---|---|---|
| **Native dispatch** | Every property read/write is a TJS→C thunk. | Cache resolved members; avoid allocating dictionaries per call. |
| **Script compile cache** | Re-parsing scripts on reload. | Cache compiled bytecode by storage path + mtime. |

## How to measure
Add `#[ignore]` perf probes next to the code (the old tree used this pattern) and
a scene benchmark that rebuilds a title-like `Scene` (1 window, ~23 layers,
~34 bitmaps) so the text/layer counts match the real test title.
