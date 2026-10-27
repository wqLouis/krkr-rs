# `media` — the stateless media service

Source: `crates/platform/media/src/` · crate `media` · layer **L1**

`media` decodes images, audio and video, and encodes images. The mixer, the
sound object model, the `.sli` loops, the video-overlay events and every
script semantic are L2/L3 — see [`../engine/`](../engine/README.md).

The design and the backend matrix are in
[`05-media-engine.md`](../../../docs.old/migration/05-media-engine.md) (evidence trail:
[`docs.old/migration/05-media-engine.md`](../../../docs.old/migration/05-media-engine.md)).

---

## 1. Responsibility

| concern | file |
|---|---|
| image decode (format sniffing + the `image` crate) | `image_decode.rs` |
| the native TLG5/TLG6 decoder | `tlg.rs` |
| image encode (PNG/BMP/JPEG) | `image_encode.rs` |
| audio probe / whole-file decode / streaming decode | `audio.rs` |
| the desktop platform audio fallback | `audio_ffmpeg.rs` |
| video decode | `video/ffmpeg.rs` (Linux), `video/mediacodec.rs` (Android) |
| YUV → RGBA8 | `yuv.rs` |

The crate links no graphics or windowing dependency.

---

## 2. Layer and dependencies

| | |
|---|---|
| Layer | L1 (bottom core) |
| Depends on | `api` (L1 DTOs + error enums), `symphonia` (+ `symphonia-adapter-libopus`), `image`, `log`; `ffmpeg-next` optional (the `video-ffmpeg` feature); `ndk` Android-only and target-gated |
| Must not depend on | `engine` (L2), `compat` (L3), `vm`/`tjs` (L4), a composition root, a windowing or graphics crate |
| Forbidden | process-global state, `#[allow(dead_code)]`, a silent substitute for a failed decode |
| Lint | `#![cfg_attr(not(target_os = "android"), forbid(unsafe_code))]`, `#![cfg_attr(target_os = "android", deny(unsafe_code))]`, `#![warn(missing_docs)]` |

`unsafe_code` is `forbid` off Android and `deny` on Android, because the
MediaCodec extractor shim is raw `extern "C"` FFI (`AMediaExtractor` is not
covered by the `ndk` crate) and rustc rejects a local `allow` under a
crate-wide `forbid`. The shim module carries the single scoped
`#[allow(unsafe_code)]`.

---

## 3. The service object and the state values

```rust
pub struct Media;
impl Media {
    pub const fn new() -> Self;
}
pub fn capabilities(m: &Media) -> MediaCapabilities;
```

`Media` holds no state and has **no `&mut self` method**. It is the facade the
architecture mandates: the decode actions take `&Media` so the call reads like
a service call, and the object selects nothing. There is no configuration to
pass — a decode that cannot be performed returns a typed `MediaError` rather
than a substituted value — so there is no configuration type and no second
factory spelling. Every evolving value is a separate value created by this
crate and owned by the caller:

| value | what it holds | owner |
|---|---|---|
| `AudioDecoderState` | the demuxer/codec context, the decode position, the retained compressed bytes, the pending samples | L2 |
| `VideoDecoderState` | the demuxer, the codec, the frame and audio state | L2 |

There is no global codec registry and no lazy cache: the symphonia
`CodecRegistry` is built inside `open_audio` on each open and the decoder is
moved into the state. Dropping the state releases every decoder/format
context (RAII).

`capabilities` is a **free function**, not a method, and its result depends
only on how the crate was compiled — so L2 chooses a decode path without a
backend-specific `#[cfg]` branch.

---

## 4. Public API

### Images

```rust
pub fn probe_image(m: &Media, bytes: &[u8], hint: Option<&str>) -> Result<ImageInfo, MediaError>;
pub fn decode_image(m: &Media, bytes: &[u8], hint: Option<&str>) -> Result<DecodedImage, MediaError>;
pub fn encode_image(m: &Media, width: u32, height: u32, rgba: &[u8], hint: &str) -> Result<Vec<u8>, MediaError>;
```

The decode order is the reference router's:

1. an explicit TLG spelling (`.tlg`/`.tlg5`/`.tlg6`) → the native TLG
   decoder; a corrupt *real* TLG reports its decode error, a *mislabeled*
   non-TLG file falls through;
2. the format implied by the extension hint (this is what disambiguates the
   weak-magic formats such as TGA);
3. content detection: the TLG magic first (the `image` crate cannot sniff
   it), then `image::guess_format`'s magic table — so a wrong or missing
   extension still decodes.

`hint` only steers format selection and is never trusted over the content.

`encode_image` accepts `png`, `bmp`, `bmp32`, `bmp24`, `bmp8`, `jpg`/`jpeg`
(a leading dot or a storage name is accepted). BMP is written by hand in the
reference's `TVPSaveTextureAsBMP` layout: a 14-byte file header, a 40-byte
info header, an optional 1024-byte palette for 8 bpp, then the bottom-up
scanlines with their 4-byte pitch padding. PNG and JPEG delegate to the
`image` crate.

Supported decode formats are the `image` crate's enabled feature list: png,
jpeg, webp, bmp, gif, tiff, tga, dds, pnm, ico, qoi, hdr, exr, ff — plus
tlg5/tlg6. AVIF/pvr/jxr are deliberately absent.

### Audio

```rust
pub fn probe_audio(m: &Media, bytes: &[u8], hint: Option<&str>) -> Result<AudioSpec, MediaError>;
pub fn decode_audio(m: &Media, bytes: &[u8], hint: Option<&str>) -> Result<DecodedAudio, MediaError>;
pub fn open_audio(m: &Media, bytes: Arc<[u8]>, hint: Option<&str>) -> Result<(AudioSpec, AudioDecoderState), MediaError>;
pub fn read_audio(m: &Media, state: &mut AudioDecoderState, out: &mut [f32]) -> Result<usize, MediaError>;
pub fn seek_audio(m: &Media, state: &mut AudioDecoderState, frame: u64) -> Result<(), MediaError>;
```

* `probe_audio` reads only the container header (sample rate, channels,
  frame count) so L2 can answer a duration query promptly.
* `decode_audio` materializes a whole file into interleaved `f32`.
* `open_audio` + `read_audio` + `seek_audio` stream from a
  caller-owned `AudioDecoderState`. `read_audio` returns the number of
  samples written, always a whole number of frames; the tail of `out` that
  cannot hold a complete frame is left untouched.
* `seek_audio` reopens the reader from the retained compressed bytes and
  discards decoded frames until the requested frame.

The codecs are symphonia's (`wav`/`pcm`, `ogg`/`vorbis`, `flac`, `mp3`,
`mp4`/`aac`) plus the separately shipped Opus adapter.

A **corrupt frame is skipped** and decoding continues; a stream that cannot
be probed or decoded returns a typed `MediaError` — silence is never
substituted.

### Video

```rust
pub fn open_video(m: &Media, bytes: Arc<[u8]>, hint: Option<&str>) -> Result<(VideoSpec, VideoDecoderState), MediaError>;
pub fn present_video(m: &Media, state: &mut VideoDecoderState, at_ms: u64) -> Result<Arc<RgbaFrame>, MediaError>;
pub fn video_audio(m: &Media, state: &VideoDecoderState) -> Result<Arc<DecodedAudio>, MediaError>;
```

`present_video` returns the picture whose timestamp the clock has reached:
the latest picture at or before the requested time. The stream is read forward
until a picture after the requested time turns up, and the picture before it
is the one presented. A request behind the picture on screen is served from
that picture when the difference is smaller than `SEEK_TOLERANCE_MS` (500 ms);
a request further behind it reads the stream from the requested time again,
and a stream that cannot be read from an earlier time - a raw elementary stream
has no index to seek in - keeps the picture on screen and is read forward only.
The picture is shared with the decoder, so handing it on copies no pixels.

`video_audio` hands out the shared decoded track, so a caller that plays it
does not copy the whole track.

The `video` module itself is private. The three actions above and
`VideoDecoderState` are published at the crate root, and the backend's own
`open`/`present`/`audio` stay inside the crate: `crate::video` is their
only caller. The MediaCodec build also publishes `decode_audio_mediacodec`
(the standalone audio path) at the crate root.

### YUV

The YUV vocabulary lives in the `media::yuv` module; it is not re-exported at
the crate root.

```rust
pub fn yuv420_to_rgba(yuv: &[u8], layout: &Yuv420Layout) -> Vec<u8>;
pub fn micros_to_millis(micros: i64) -> i64;
pub enum YuvSampling { Planar, SemiPlanar }
pub struct Yuv420Layout { … }   // width, height, stride, slice height, sampling, crop
pub struct CropRect { … }
```

The pure YUV conversion, used by the MediaCodec backend's
`COLOR_FormatYUV420Flexible` output.

---

## 5. The audio decode chain

The built-in decoder (symphonia) has **no SBR synthesis**. An AAC track with
the SBR extension (HE-AAC) declares the extension's rate (48 kHz) while the
decoder produces the core's (24 kHz), so the track would play at the wrong
speed.

The built-in decoder **reports** that mismatch instead of approximating it:
`open_audio` decodes the first packet, so the report happens at the open, as
`MediaError::Unsupported`. The platform's AAC decoder reconstructs the SBR
band instead:

| entry point | backend |
|---|---|
| `media::decode_audio_ffmpeg` (the `video-ffmpeg` feature, Linux) | FFmpeg's AAC decoder, which reconstructs the SBR band |
| `media::decode_audio_mediacodec` (the `video-mediacodec` feature, Android) | the NDK `audio/mp4a-latm` decoder |

**The host owns the order**, not L1:

| build | first | fallback |
|---|---|---|
| desktop (Linux) | the FFmpeg backend | the built-in decoder |
| Android | the built-in decoder | the platform's AAC decoder |

The desktop build decodes with FFmpeg first: it reconstructs the SBR band,
so the tracks the built-in decoder reports are decoded at the rate the
container declares. The built-in decoder stays in the chain because it
**streams** — the container is probed at the open and the samples are decoded
as playback reads them — so a track the FFmpeg backend cannot read still
plays without holding the whole track in memory.

The platform step materializes the **whole track** into interleaved `f32`;
the caller plays the returned buffer. The Android path reuses the MediaCodec
backend's extractor, codec and audio state, so there is one implementation of
the demux/decode loop; a decoder that cannot be created, configured or started
is logged with the mime and the reason and reported as a typed error.

The chain lives in `crates/app/src/audio.rs` (`decode_attempts` and
`decode_with_attempts`), and the order is selected per target with
`#[cfg(target_os = …)]`. When every backend in the chain fails, the error
names each of them and the reason it gave, so neither is lost.

The materialized step costs the memory a whole-file decode costs, and the
desktop build pays it for every track it opens.

### The container's priming and channel count

An AAC track is encoded with a delay: the first frames hold the encoder's
warm-up, not the recording, and the container says how many samples they are
— the track's `initial_padding`, which the demuxer hands to the decoder as
the skip-samples side data of the first packet — and how long the track is
presented, so the samples the encoder padded it with are not part of it. The
track the platform step returns therefore starts at the recording and ends at
the recording's end; without that a voice would start a frame late and run
long. The committed MP4/AAC fixture pins it: the 0.5 s encode is 22,050
samples, and the backend returns that many (the 23 AAC frames it decodes are
23,552 samples, of which the 1,024-sample priming and the encoder's padding
are removed).

The **built-in** decoder honours the same timing: it reads the track's edit
list out of the container's own bytes and skips the priming and the padding,
so both paths return the same 22,050 samples for the same file. The edit
list is the container's statement of the timing — the media time the
presentation starts at and the length of the presentation — and the samples
are counted in the decoder's own rate, so a container whose timescales differ
from the sample rate is converted exactly.

**Deliberate divergence:** the reference engine's wave decoder
(`reference/cpp/core/sound/FFWaveDecoder.cpp`) has no priming handling of its
own and reports the container's media length, so the track it plays starts
with the encoder's delay. This engine removes it, as the modern FFmpeg
decode path does, so that a voice starts when the script says it does.

The track's channel count is the one the container declares. A track whose
`AudioSpecificConfig` declares a mono core stays mono even when its
bitstream carries the parametric-stereo extension: the extension is only
reconstructed for a track the container declares as stereo. The reference
takes the channel count from the decoder's context as well, so a mono track
is mono in both.

**Verified vs unverified:** the desktop path is exercised by the committed
MP4/AAC fixture (`crates/platform/media/tests/assets/sine_440hz_0.5s.m4a`).
An HE-AAC track's SBR band has no committed fixture — the test game's HE-AAC
voice track is not committed — so the band is unverified here. The Android
path is compile-checked only — the `AMediaExtractor` shim, the `memfd` trick,
the platform AAC decoder and the SBR handling have not run on a device here.

---

## 6. The two video backends

Exactly one backend is compiled; selecting neither or both is a
`compile_error!` in `src/video/mod.rs`.

| feature | target | backend | notes |
|---|---|---|---|
| `video-ffmpeg` (the default) | Linux / desktop | `ffmpeg-next`, linked dynamically through pkg-config | demuxes from an in-memory `StreamIo` (no temp file), converts frames to tightly packed RGBA8 with `swscale`, decodes the audio track with `swresample` |
| `video-mediacodec` | Android | the NDK `AMediaExtractor`/`AMediaCodec` | links no FFmpeg and needs no JNI; the bytes are written to an anonymous `memfd` and the descriptor is passed to `AMediaExtractor_setDataSourceFd` |

The FFmpeg backend decodes the whole audio track eagerly at `open`; the
decoded track is handed out shared, so the eager decode is paid once and not
copied per caller.

The MediaCodec backend derives the frame rate and the total frame count from
a walk of the sample table and reports the cropped frame dimensions — the
deliberate deviations from the reference are recorded in
[`05-media-engine.md`](../../../docs.old/migration/05-media-engine.md) §5.3.

**Known approximation:** a track with more than two channels is downmixed to
stereo, because the L2 mixer renders mono and stereo only. The reference
keeps the file's channel count, so a surround track loses its rear channels
here.

---

## 7. State ownership

| value | owner |
|---|---|
| `Media` (the immutable configuration) | L1 |
| `AudioDecoderState`, `VideoDecoderState` | L2 |
| the mixer, the channels, the loops, the fades | L2 (`engine::sound`) |
| the video-overlay playback machine | L2 (`engine::video`) |
| the decoded-image cache | L2 |

L1 never calls TJS, the scene or a callback into L2: it returns owned data
(`Vec<u8>` / `Vec<f32>`) and L2 polls. Cancellation is L2's — it drops the
future/handle and the RAII decoder value it owns; L1 holds no cancellation
state.

---

## 8. Platform gating

| gate | effect |
|---|---|
| `video-ffmpeg` / `video-mediacodec` | exactly one video backend; neither or both is a `compile_error!` |
| `cfg(target_os = "android")` | the `ndk` dependency and the extractor shim exist only on Android; a non-Android host compiles a typed stub whose every entry point returns `MediaError::Unsupported` |
| `cfg(feature = "video-ffmpeg")` | `audio_ffmpeg` and the FFmpeg video backend exist only in the desktop build |

FFmpeg is a **required system dependency** on Linux and is not bundled.

---

## 9. Failure modes

Every failure is a typed `MediaError` (the variants are listed in
[api.md](api.md) §7). The cases that matter here:

| failure | behaviour |
|---|---|
| the container cannot be probed | `MediaError::Decode` — the hinted attempt's message and the unhinted attempt's message are both in the text |
| the stream has no decodable track | `MediaError::Decode` naming the container |
| the decoder produces a different rate than the container declares (the SBR case) | `MediaError::Unsupported` naming both rates — the track is **not** played at the wrong speed |
| the codec is not in this build | `MediaError::Unsupported` |
| the bytes are garbage | `MediaError::Decode` — never a panic, never silence |
| the stream ended | `MediaError::EndOfStream` |
| an unknown encode target | `MediaError::Unsupported`; a wrong buffer length | `MediaError::InvalidData`; a codec failure | `MediaError::Encode` |
| the MediaCodec decoder cannot be created, configured or started | `MediaError::Unsupported`, with the mime and the reason logged |

There is **no size cap**: a decode that cannot be performed (malformed,
overflowing, unsupported) is an error, not a truncated result.

---

## 10. Tests

* `crates/platform/media/tests/audio.rs` — the WAV header probe and frame
  count, the streaming read matching the whole-file decode, the seek landing
  on the requested frame, the Ogg Vorbis and MP4/AAC fixtures (probe, decode
  to audible samples, streaming match), the truncated Ogg returning a typed
  error rather than silence, the undecodable Opus, the hint handling, the
  garbage input, and the FFmpeg backend decoding the Ogg Vorbis and the
  MP4/AAC fixtures into the format the container declares.
* `crates/platform/media/tests/bmp_encode.rs` — the BMP header and total
  length against the reference, the `bmp`/`bmp32` mode equivalence, the
  bottom-up BGRA body, the 24 bpp row padding, the 8 bpp palette and pitch,
  the palette-index rows and the default mode.
* `crates/platform/media/tests/image.rs` — the reference extension spellings,
  the encode round trip for the supported formats, the rejected encode
  target and buffer, the TLG hint routing, the magic-based decode with a
  missing or wrong extension, the mislabeled TLG, the garbage input and the
  large image with no configured size cap.
* `crates/platform/media/tests/media.rs` — the advertised capabilities, the
  preserved config and the stateless `Media`, and the rejected video open.
* `crates/platform/media/tests/tlg.rs` — the TLG6 header probe, the `sds`
  wrapper equivalence, the TLG6/TLG5 round trips over every filter, the
  narrow width, the multi-band partial round trip, the block height larger
  than the image, and the garbage inputs.
* `crates/platform/media/tests/video.rs` — the generated clip: the reported
  dimensions and audio track, the tightly packed RGBA with non-empty pixels,
  the backward seek past the tolerance and the forward decode, the
  interleaved PCM, and the stereo track keeping both channels. The tests
  **skip** when no usable `ffmpeg` encoder is available.
* `crates/platform/media/tests/yuv.rs` — the solid black/white planes, a
  colour conversion, the odd-width chroma rounding, the stride padding, the
  crop rectangle, the semi-planar NV12 interleave, the short buffer and the
  µs→ms rescale.

---

## 11. Related documents

| topic | document |
|---|---|
| the L1 overview | [README.md](README.md) |
| L1 as an architecture | [`arch/l1.md`](../../arch/l1.md) §4 |
| the media engine design, the backend matrix, the threading contract | [`05-media-engine.md`](../../../docs.old/migration/05-media-engine.md) · snapshot [`docs.old/migration/05-media-engine.md`](../../../docs.old/migration/05-media-engine.md) |
| the mixer and the sound model (L2) | [`../engine/README.md`](../engine/README.md) |
| the PSB/PSD reader, a separate crate | [media/psd.md](media/psd.md) |
| the platform audio output device | [platform.md](platform.md) §6 |
| the Android host and the MediaCodec backend | [`../android/README.md`](../android/README.md) |
| the typed-error rule | [`arch_rules.md`](../../rules/arch_rules.md) §6 |
| the Rust-level checklist | [`code_styles.md`](../../rules/code_styles.md) |
