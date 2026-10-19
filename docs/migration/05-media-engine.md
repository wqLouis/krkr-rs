# 05 — Media engine

Status: implemented. One bottom-layer media crate decodes images, audio and
video, with two platform backends — **system FFmpeg on Linux** and **Android
MediaCodec** — behind the TVP-free stateless [`media`](03-middle-layer-api.md#4-media)
API. The mixer, sound object model, `.sli` loops, video-overlay events and
all script semantics are L2/L3.

---

## 1. Inventory

| Concern | Owner |
|---------|-------|
| Image decode (TLG5/6 + `image` formats) | `media` (`image_decode`, `tlg`) |
| Image encode (PNG/BMP/JPEG) | `media` (`image_encode`) |
| Audio decode (symphonia + Opus adapter) | `media` (`audio`) |
| Audio mixer/model | L2 `engine::sound` |
| Audio output device | L1 `platform::audio` |
| Video decode | `media::video` (FFmpeg | MediaCodec) |
| Video overlay events | L2 `engine::video` + L3 `compat` |
| YUV conversion | `media::yuv` |

`media` links no graphics or windowing dependency, and the FFmpeg backend is
behind the `video-ffmpeg` feature.

## 2. Engine shape

```
media
  ├─ image_decode.rs        format sniffing + the image crate
  ├─ tlg.rs                 TLG5/TLG6
  ├─ image_encode.rs        PNG/BMP/JPEG encode
  ├─ audio.rs               probe, decode-all, streaming decoder
  ├─ video/ffmpeg.rs        Linux: ffmpeg-next
  ├─ video/mediacodec.rs    Android: AMediaExtractor + AMediaCodec
  └─ yuv.rs                 YUV → RGBA8
```

The API is stateless (D9): pure free functions for image/whole-file audio
decode, and L1-defined `AudioDecoderState`/`VideoDecoderState` values owned
by L2 for streaming. There is no engine-side registry, no global buffer and no hidden
cache. L2 owns the worker pool; decode never runs on the host/VM thread
except for trivially small probes.

## 3. Image decode

* `tlg.rs` and the `decode_image` dispatch live in `media`; TLG is a pure
  format with no platform dependency.
* The order is: extension hint → magic sniff (`image::guess_format`) →
  decode. TLG by extension and by `TLG` magic.
* `probe_image` returns dimensions/alpha/format without full decode (used by
  `loadHeader`).
* Async decode path: the script-facing `AsyncTrigger`/poll machinery stays
  in L2/L3 and owns the completion table; L1 is the pure `decode_image` call
  plus, where needed, an L2-scheduled worker. Image decoding is currently
  synchronous on the VM thread; the async/decode-worker path is tracked in
  `TODO.md` §1.
* Image **save** (TLG5/TLG6, PNG) lives in `media` too, because it is a
  format concern. `Bitmap.saveLayerImage`/`save` go through it.
* Supported formats stay the current `image` 0.25 feature list (png, jpeg,
  webp, bmp, gif, tiff, tga, dds, pnm, ico, qoi, hdr, exr, ff); AVIF/pvr/jxr
  remain absent unless a title needs them.

## 4. Audio decode

### 4.1 Decoder

* symphonia setup, codec registry, Opus adapter (`audio.rs`);
* probe/whole-file/streaming shapes (`AudioMetadata`, `DecodedAudio`,
  `AudioDecoderState`);
* the compressed-buffer sharing (`Arc<[u8]>`) and ring-buffer decode-ahead
  policy.

### 4.2 What stays L2

* `Mixer`/`Channel` math, volume/volume2/rate/pan/3D/fades;
* `.sli` loop/label handling (`sli.rs`, `waveflags.rs`);
* `WaveSoundBuffer`/`SoundBuffer`/`SoundChannel` state machines and
  `onStatusChanged`/`onFadeCompleted` delivery (`sound_poll`);
* the choice between whole-file and streaming (a policy decision), though
  the engine may expose the byte-size hint that informs it.

### 4.3 Backend split

Audio decoding is symphonia on both targets (wav/pcm, ogg/vorbis, flac, mp3)
plus the Opus adapter; there is no separate FFmpeg audio path. Video uses
FFmpeg on Linux and MediaCodec on Android. The same free functions and decoder
values are used either way, so the mixer never learns which backend ran.

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

* `ffmpeg-next` with the in-memory custom `StreamIo` (no temp file); L2 reads
  the bytes through `fs` and passes them in.
* `present_video`, `next_video_frame`, `convert_video` (swscale→RGBA) and
  `drain_audio` (swresample→f32) are implemented.
* **Known inefficiency:** the audio track is decoded fully at `open`
  (`decode_audio_track`); streaming the audio through the audio decode API is
  open work (`TODO.md` §7 `M5`).

### 5.3 MediaCodec backend (Android)

* Keep `AMediaExtractor` + `AMediaCodec` over a `memfd` (API-21 constraint
  documented in `android.rs`), byte-buffer output with
  `COLOR_FormatYUV420Flexible`, and the pure-Rust `yuv.rs` conversion.
* Keep the deliberate deviations: fps/total frames from the sample-table
  walk; cropped frame dimensions.
* This backend is compile-verified and host-tests its pure parts but is
  **runtime-untested**; it stays that way until run on a device
  (`docs/android.md §6`).

### 5.4 Feeding L2

The video frame sink exists: `present_video` returns an `RgbaFrame` and L2
blits it into the target layer's `BitmapState` (CPU), so video is visible
through the normal layer system; `blit_frame_to_layer_opacity` composites
it into `layer1`/`layer2` and the mixing layer. The `video_audio` PCM sink
returns interleaved `f32`; installing it as a mixer source remains open:
the FFmpeg backend still decodes the whole audio track eagerly (§5.2).

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

L2 drains completions at a fixed point each frame: the host runs the ordered
`compat::run_frame_body` sequence (`timer_poll → sound_poll → video_poll →
async_trigger_poll → continuous_handler_poll → project_and_render`), before
the render list is built.

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

Cargo features: the `media` crate's `video-ffmpeg` feature is the default for
desktop and is turned off on Android, where `video-mediacodec` is selected
instead (`crates/platform/media/Cargo.toml`, `crates/app/Cargo.toml`,
`crates/android/Cargo.toml`).

## 9. Implementation

* `media` exposes the stateless `Media` object, the pure image/audio
  functions, and the caller-owned `AudioDecoderState`/`VideoDecoderState`
  values (`03-middle-layer-api.md §4`).
* TLG5/TLG6 and the `image`-crate dispatch are in `media`; the image encoder
  is in `media::image_encode`.
* The FFmpeg backend wraps the video demux/decode, and the MediaCodec backend
  covers Android; exactly one is selected by feature.
* L2 owns the mixer, the streaming decode-ahead worker and the completion
  table; the video frame/PCM sinks feed the layer bitmap and the mixer.
