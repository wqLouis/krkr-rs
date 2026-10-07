# 05 — Media engine

Status: design sketch. Extracts decoding from `tvp-sound`, `tvp-visual` and
`tvp-natives/video` into the bottom core.

Goal: one bottom-layer media engine that decodes images, audio and video,
with two platform backends — **system FFmpeg on Linux** and **Android
MediaCodec** — behind the TVP-free stateless [`media`](03-middle-layer-api.md#4-media)
API. The mixer, sound object model, `.sli` loops, video-overlay events and
all script semantics stay in the middle layer.

---

## 1. Inventory being replaced

| Concern | Today | Target |
|---------|-------|--------|
| Image decode | `tvp-visual/src/bitmap.rs::decode_image` + `tlg.rs` + `image` crate | `media` (shared `ImageDecoder`) |
| Audio decode | `tvp-sound/src/decode.rs` (symphonia + libopus), `source.rs` policy | `media` (FFmpeg audio / MediaCodec audio) |
| Audio mixer/model | `tvp-sound/src/mixer.rs`, `wavesound.rs`, `natives.rs`, `sli.rs` | stays L2 `engine` |
| Audio output | `tvp-sound/src/player.rs` (rodio/cpal) | `krkr-core` `AudioDevice` |
| Video decode | `tvp-natives/src/video/{ffmpeg,android,yuv}.rs` | `media` (two backends) |
| Video overlay events | `tvp-natives/src/video_overlay.rs` | stays L2 (polls the engine) |
| Format registry | ad hoc per path | declared per backend |

The current split is already close: `tvp-sound` and `tvp-natives/video` are
Bevy-free, and the `MovieDecoder` interface already abstracts FFmpeg vs
MediaCodec. This is the least disruptive of the three bottom components.

## 2. Engine shape

```
media
  ├─ image/                 format sniffing, TLG5/6, image crate
  ├─ audio/                 probe, decode-all, streaming decoder
  ├─ video/                 probe, open, present_at, audio track
  ├─ backend/ffmpeg.rs      Linux: ffmpeg-next (+ symphonia for codecs it lacks)
  └─ backend/mediacodec.rs  Android: AMediaExtractor + AMediaCodec
```

The API is stateless (D9): pure free functions for image/whole-file audio
decode, and L1-defined `AudioDecoderState`/`VideoDecoderState` values owned
by L2 for streaming. There is no engine-side registry, no global buffer and no hidden
cache. L2 owns the worker pool; decode never runs on the host/VM thread
except for trivially small probes.

## 3. Image decode

* Move `tvp-visual/src/tlg.rs` and the `decode_image` dispatch into
  `media`. TLG is a pure format and has no platform dependency.
* Keep the current order: extension hint → magic sniff (`image::guess_format`)
  → decode. TLG by extension and by `TLG` magic.
* `probe_image` returns dimensions/alpha/format without full decode (used by
  `loadHeader`).
* Async decode path: the existing `AsyncTrigger`/poll machinery
  (`tvp-visual/src/natives/bitmap.rs::loadAsync`) stays in L2 and owns the
  completion table; L1 is just the pure `decode_image` call plus, if
  needed, an L2-scheduled worker.
* Image **save** (TLG5/TLG6, PNG) lives in `media` too, because it is a
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
Android the plan is `AMediaCodec` for AAC/video audio with symphonia for
wav/ogg/mp3. The same free functions and decoder values are
used either way, so the mixer never learns which backend ran.

### 4.4 Long tracks

The measured pain point is a whole-track decode (a 5.37 MB BGM → 8.5 s and
~125 MB, `docs/optimization.md`). The engine provides a streaming decoder
and exposes the frame count; **L2 chooses** whole-file vs streaming. Seeking
and loop-wrap should not require a full re-decode where the backend allows.

## 5. Video decode

### 5.1 Interface

Keep the current `MovieDecoder` semantics as an L1-defined
`VideoDecoderState` value stored by L2, not an id-addressed table:

* `open_video(bytes) -> (VideoSpec, VideoDecoderState)`;
* `present_video(state, at_ms) -> RgbaFrame` with the **backward-seek
  contract** (today: seek if moving backwards more than `SEEK_TOLERANCE_MS`,
  otherwise decode forward; `ffmpeg.rs:279`);
* `video_audio(state) -> DecodedAudio` (or a streaming variant);
* dropping the state releases the codec.

### 5.2 FFmpeg backend (Linux)

* `ffmpeg-next` with the existing in-memory custom `StreamIo` (no temp
  file); L2 reads the bytes through `fs` and passes them in.
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

The **L2 worker pool** owns its threads; L1 is stateless functions. Rules:

* L1 never calls TJS, the scene or a callback into L2.
* Every long-running action is either synchronous with a small budget
  (probes) or a pure function the L2 pool runs; **L2 owns the completion
  table** and drains it on the host thread.
* Results are plain owned buffers (`Vec<u8>`/`Vec<f32>`), never borrowed
  across the boundary.
* Cancellation: L2 drops the future/handle and the RAII decoder value it
  owns; L1 holds no cancellation state.
* Pure L1 functions are freely `Send`; RAII decoder values are owned by L2
  and need not be `Sync`.

L2 drains completions at a fixed point each frame (today: the top of
`run_vm`, with `sound_poll`/`video_overlay_poll`/`async_poll`), before the
render list is built.

## 7. Errors and capability detection

* Missing codec/backend → a typed `MediaError::Unsupported`, surfaced to
  scripts as the current friendly TJS error ("no MPEG decoder in this
  build").
* A stateless `capabilities()` free function per backend reports which
  formats exist so L2 can choose. This replaces the `#[cfg]` branches
  scattered in `video/mod.rs`.
* No silence fallback (D11): a failed Opus decode returns `MediaError`; it is
  never replaced by silence.

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

1. Define the stateless media signatures + DTOs in `api`
   (`03-middle-layer-api.md §4`).
2. Move `tlg.rs`/`decode_image` into `media`; keep a thin re-export in
   `tvp-visual` so natives compile unchanged.
3. Implement the FFmpeg backend by wrapping the existing `video/ffmpeg.rs`
   and `decode.rs` as free functions + `AudioDecoderState`/
   `VideoDecoderState` values; implement the unsupported backend as pure
   error-returning stubs.
4. Switch `tvp-sound::source` and `tvp-visual` bitmap loading to the
   stateless API; **L2 gains the thread pool and completion table**.
5. Add the video frame/PCM sinks in L2.
6. Implement the MediaCodec backend by moving `video/android.rs` + `yuv.rs`.
7. Delete the old decode modules; the natives keep calling the engine.

Each step leaves the game runnable because the wrappers preserve the current
public functions until the callers move.
