# 05 — Media engine

Status: design sketch. Extracts decoding from `tvp-sound`, `tvp-visual` and
`tvp-natives/video` into the bottom core.

Goal: one bottom-layer media engine that decodes images, audio and video,
with two platform backends — **system FFmpeg on Linux** and **Android
MediaCodec** — behind the TVP-free [`MediaEngine`](03-middle-layer-api.md#4-media-engine)
trait. The mixer, sound object model, `.sli` loops, video-overlay events and
all script semantics stay in the middle layer.

---

## 1. Inventory being replaced

| Concern | Today | Target |
|---------|-------|--------|
| Image decode | `tvp-visual/src/bitmap.rs::decode_image` + `tlg.rs` + `image` crate | `krkr-media` (shared `ImageDecoder`) |
| Audio decode | `tvp-sound/src/decode.rs` (symphonia + libopus), `source.rs` policy | `krkr-media` (`FfmpegMedia` audio / `MediaCodecMedia` audio) |
| Audio mixer/model | `tvp-sound/src/mixer.rs`, `wavesound.rs`, `natives.rs`, `sli.rs` | stays L2 `krkr-engine` |
| Audio output | `tvp-sound/src/player.rs` (rodio/cpal) | `krkr-core` `AudioOutput` |
| Video decode | `tvp-natives/src/video/{ffmpeg,android,yuv}.rs` | `krkr-media` (two backends) |
| Video overlay events | `tvp-natives/src/video_overlay.rs` | stays L2 (polls the engine) |
| Format registry | ad hoc per path | declared per backend |

The current split is already close: `tvp-sound` and `tvp-natives/video` are
Bevy-free, and the `MovieDecoder` interface already abstracts FFmpeg vs
MediaCodec. This is the least disruptive of the three bottom components.

## 2. Engine shape

```
krkr-media
  ├─ image/                 format sniffing, TLG5/6, image crate
  ├─ audio/                 probe, decode-all, streaming decoder
  ├─ video/                 probe, open, present_at, audio track
  ├─ backend/ffmpeg.rs      Linux: ffmpeg-next (+ symphonia fallback)
  └─ backend/mediacodec.rs  Android: AMediaExtractor + AMediaCodec
```

The trait is stateful by handle (`AudioStreamId`, `VideoStreamId`) so the
middle layer can hold TVP objects that map 1:1 to engine streams. The engine
owns a worker pool; decode never runs on the host/VM thread except for
trivially small probes.

## 3. Image decode

* Move `tvp-visual/src/tlg.rs` and the `decode_image` dispatch into
  `krkr-media`. TLG is a pure format and has no platform dependency.
* Keep the current order: extension hint → magic sniff (`image::guess_format`)
  → decode. TLG by extension and by `TLG` magic.
* `probe_image` returns dimensions/alpha/format without full decode (used by
  `loadHeader`).
* Async decode path: the existing `AsyncTrigger`/poll machinery
  (`tvp-visual/src/natives/bitmap.rs::loadAsync`) stays in L2 and calls the
  engine; the engine returns a completion id.
* Image **save** (TLG5/TLG6, PNG) lives in `krkr-media` too, because it is a
  format concern. `Bitmap.saveLayerImage`/`save` go through it.
* Supported formats stay the current `image` 0.25 feature list (png, jpeg,
  webp, bmp, gif, tiff, tga, dds, pnm, ico, qoi, hdr, exr, ff); AVIF/pvr/jxr
  remain absent unless a title needs them.

## 4. Audio decode

### 4.1 What moves

* symphonia setup, codec registry, Opus adapter (`decode.rs`);
* probe/whole-file/streaming shapes (`AudioMetadata`, `DecodedAudio`,
  `StreamDecoder::next_chunk`);
* the compressed-buffer sharing (`Arc<[u8]>`) and ring-buffer decode-ahead
  policy (`source.rs`).

### 4.2 What stays L2

* `Mixer`/`Channel` math, volume/volume2/rate/pan/3D/fades;
* `.sli` loop/label handling (`sli.rs`, `waveflags.rs`);
* `WaveSoundBuffer`/`SoundBuffer`/`SoundChannel` state machines and
  `onStatusChanged`/`onFadeCompleted` delivery (`sound_poll`);
* the choice between whole-file and streaming (a policy decision), though
  the engine may expose the byte-size hint that informs it.

### 4.3 Backend split

On Linux the current symphonia path is already the decode engine; the plan
is to keep it and add FFmpeg audio only where symphonia lacks a codec. On
Android the plan is `AMediaCodec` for AAC/video audio with a symphonia
fallback for wav/ogg/mp3. The trait exposes the same `DecodedAudio`/stream
API either way, so the mixer never learns which backend ran.

### 4.4 Long tracks

The measured pain point is a whole-track decode (a 5.37 MB BGM → 8.5 s and
~125 MB, `docs/optimization.md`). The engine keeps the streaming decoder as
the default for long tracks and exposes the frame count so L2 can decide.
The engine should support `seek`/loop-wrap without a full re-decode where
the backend allows.

## 5. Video decode

### 5.1 Interface

Keep the current `MovieDecoder` semantics, generalized to handles:

* `open_video(bytes) -> VideoStreamId` + `VideoSpec`;
* `present_video(id, at_ms) -> RgbaFrame` with the **backward-seek contract**
  (today: seek if moving backwards more than `SEEK_TOLERANCE_MS`, otherwise
  decode forward; `ffmpeg.rs:279`);
* `video_audio(id) -> DecodedAudio` (or a stream variant);
* `close_video`.

### 5.2 FFmpeg backend (Linux)

* `ffmpeg-next` with the existing in-memory custom `StreamIo` (no temp
  file), so the engine reads bytes from `FileSystem` via L2.
* Reuse `present_at`, `next_video_frame`, `convert_video` (swscale→RGBA),
  `drain_audio` (swresample→f32) verbatim.
* Fix the known inefficiency: audio is decoded fully at `open`
  (`ffmpeg.rs:234`); the target streams it through the audio decode API.

### 5.3 MediaCodec backend (Android)

* Keep `AMediaExtractor` + `AMediaCodec` over a `memfd` (API-21 constraint
  documented in `android.rs`), byte-buffer output with
  `COLOR_FormatYUV420Flexible`, and the pure-Rust `yuv.rs` conversion.
* Keep the deliberate deviations: fps/total frames from the sample-table
  walk; cropped frame dimensions.
* This backend is compile-verified and host-tests its pure parts but is
  **runtime-untested**; it stays that way until run on a device
  (`docs/android.md §6`).
* No JNI is needed for decode (`libmediandk`); JNI remains only in the
  Android host crate.

### 5.4 Feeding L2

The current `VideoOverlay` does **not** feed the mixer or the renderer
(`video_overlay.rs`); it exposes `frameBytes` and composites over a mixing
layer's `MainImage`. The migration adds the two missing sinks without
changing the public native API:

1. **Frame sink**: `present_video` returns `RgbaFrame`; L2 blits it into the
   target layer's `BitmapState` (CPU) exactly like `compose_mixing` does
   today, or emits a texture update. This is what makes video visible
   through the normal layer system.
2. **PCM sink**: `video_audio` returns interleaved `f32`; L2 installs it as
   a mixer source so video audio actually plays.

Both are L2 changes; the engine API already produces the required data.

## 6. Threading and the poll contract

The engine owns its workers. Rules:

* L1 never calls TJS or the scene.
* Every long-running action is either synchronous with a small budget
  (probes) or returns a completion handle polled by L2 on the host thread.
* Results are plain owned buffers (`Vec<u8>`/`Vec<f32>`), never borrowed
  from engine state across the boundary.
* Cancellation: dropping the last handle stops the worker
  (today's `AudioTrack` `Drop` cancels its thread).
* `MediaEngine` is `Send + Sync`; individual streams are `Send`.

L2 drains completions at a fixed point each frame (today: the top of
`run_vm`, with `sound_poll`/`video_overlay_poll`/`async_poll`), before the
render list is built.

## 7. Errors and capability detection

* Missing codec/backend → a typed `MediaError::Unsupported`, surfaced to
  scripts as the current friendly TJS error ("no MPEG decoder in this
  build").
* `MediaEngine::capabilities()` can report which formats/backends exist so
  L2 can choose. This replaces the `#[cfg]` branches scattered in
  `video/mod.rs`.
* Keep the Opus-failure silence fallback (it preserves sequencing).

## 8. Feature/target matrix

| Feature | Linux | Android |
|---------|-------|---------|
| Images (TLG/png/…) | yes | yes |
| Audio wav/ogg-vorbis/flac/mp3 | symphonia | symphonia |
| Audio opus | libopus adapter | libopus adapter (or MediaCodec AAC only) |
| Video H.264/AAC | FFmpeg | MediaCodec |
| FFmpeg dependency | system pkg-config | **none** |
| JNI for decode | n/a | none (libmediandk) |

Cargo features mirror the current `ffmpeg` pass-through
(`crates/render/Cargo.toml`, `crates/app/Cargo.toml`,
`crates/tvp-natives/Cargo.toml`): the media engine's `ffmpeg` feature is
enabled for desktop and off on Android, where `mediacodec` is enabled
instead.

## 9. Migration steps specific to media

1. Define `MediaEngine` + DTOs in `krkr-api`.
2. Move `tlg.rs`/`decode_image` into `krkr-media`; keep a thin re-export in
   `tvp-visual` so natives compile unchanged.
3. Implement `FfmpegMedia` by wrapping the existing `video/ffmpeg.rs` and
   `decode.rs`; implement `UnsupportedMedia`.
4. Switch `tvp-sound::source` and `tvp-visual` bitmap loading to the engine
   handle API.
5. Add the video frame/PCM sinks in L2.
6. Implement `MediaCodecMedia` by moving `video/android.rs` + `yuv.rs`.
7. Delete the old decode modules; the natives keep calling the engine.

Each step leaves the game runnable because the wrappers preserve the current
public functions until the callers move.
