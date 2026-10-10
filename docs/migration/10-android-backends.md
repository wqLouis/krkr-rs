# Android backends (AAudio + MediaCodec) — design & references

Status: **design + reference doc for untestable-on-this-host backends.** **Android
is a first-class host target (D18); Windows is emulated, not a host.** We
cannot build or run Android here, so this file pins the behaviour to the
official Android documentation and defines exactly how the L1 backends map onto
our existing, already-tested API surface. Both backends are **compile-time
selected** (D13) and implemented against the NDK; each is only compiled on an
Android target (host builds use a stub), so they are compile-checked there and
must be validated on a device.

## Official references

**Audio output (AAudio / NDK)**
- AAudio guide: <https://developer.android.com/ndk/guides/audio/aaudio/aaudio>
- AAudio API reference: <https://developer.android.com/ndk/reference/group/aaudio>
- Header/lib: `<aaudio/AAudio.h>`, `libaaudio.so` (Android 8.0 / API 26+)
- Recommended by Google: **Oboe** (C++ wrapper over AAudio, OpenSL ES fallback)
  <https://github.com/google/oboe>

**Video decode (MediaCodec / NDK)**
- NDK media reference: <https://developer.android.com/ndk/reference/group/media>
- Java `MediaCodec`: <https://developer.android.com/reference/android/media/MediaCodec>
- Headers/libs: `<media/NdkMediaCodec.h>`, `<media/NdkMediaExtractor.h>`,
  `<media/NdkMediaFormat.h>`, `libmediandk.so`

## AAudio → `platform::AudioDevice`

| Our API | AAudio |
|---|---|
| `AudioDevice::open(spec)` | `AAudio_createStreamBuilder` → set direction/sample-rate/channel-count/format/sharing-mode/performance-mode/buffer-capacity → `AAudioStreamBuilder_openStream` → `AAudioStreamBuilder_delete`. Store the `AAudioStream` in `AudioOutputState`. |
| `AudioOutputState` queue | An `AudioQueue` held by the state — the same buffer policy as the Linux backend (150 ms pre-fill gate before playback starts, 2 s cap dropping oldest-first, a ~5 ms fade to/from silence on underrun) — and the `AAudioStream_dataCallback` drains it into `audioData`. Use `AAUDIO_PERFORMANCE_MODE_LOW_LATENCY`. |
| `AudioDevice::write(state, pcm)` | Push to the queue (preferred, callback-driven) or `AAudioStream_write` for a blocking/non-blocking push. |
| `AudioDevice::stop` | `AAudioStream_requestStop` → `AAudioStream_close`. |
| `AudioDevice::is_running` | Track in the state; AAudio state transitions are async and use `AAudioStream_waitForStateChange`. |

Format handling: request `AAUDIO_FORMAT_PCM_FLOAT`; if the device returns
`AAUDIO_FORMAT_PCM_I16` convert `f32 → i16` (as cpal's `FromSample` does). Handle
`AAUDIO_STREAM_STATE_DISCONNECTED` via the error callback; always stop+close a
disconnected stream. The AAudio API is **not fully thread-safe**: do not close a
stream while another thread reads/writes it (serialise through the queue).

**Implemented** on `ndk::audio` (`AudioStreamBuilder` / `AudioStream`). Two
deliberate choices: (1) `AAudioStream_write` is **never** called — AAudio forbids
`write` on a callback-owned stream, so `write` only pushes into the queue the
data callback drains; (2) the format is read with `AudioStream::format()` (the
ABI's `AAudioStream_getFormat` — the guide's `AAudioStream_getDataFormat`
snippet is a **doc typo**; that symbol is not in `AAudio.h`). Sharing mode is
`SHARED`; performance mode `LOW_LATENCY`.

## MediaCodec → `media` video backend

| Our API | NDK |
|---|---|
| `open_video` | `AMediaExtractor_new` + `setDataSource` (fd/memory) → `getTrackCount`/`getTrackFormat` per track; pick video + audio. `AMediaCodec_createDecoderByType` → `AMediaCodec_configure` (with the track `AMediaFormat`) → `AMediaCodec_start`. |
| `present_video(at_ms)` | Feed samples: `AMediaExtractor_readSampleData` → `AMediaCodec_dequeueInputBuffer`/`getInputBuffer`/`queueInputBuffer`; drain with `AMediaCodec_dequeueOutputBuffer`/`getOutputBuffer`/`releaseOutputBuffer`. Seek via `AMediaExtractor_seekTo` + `AMediaCodec_flush`. Output is typically YUV; convert to packed RGBA8 (we already have `media/src/yuv.rs`). |
| `video_audio` | The selected audio track decoded through a second `AMediaCodec`, or the whole track decoded at open (same eager behaviour the FFmpeg backend currently has). |
| release | `AMediaCodec_stop`/`delete`, `AMediaExtractor_delete`, `AMediaFormat_delete` on `Drop`. |

`AMediaFormat` accessors (`getInteger`-style helpers are `AMediaFormat_getInt32`
etc. — see the reference) read width/height/frame-rate/duration; `AMediaCodec`
reports a **format change** on the output buffer (`AMediaCodec_getOutputFormat`)
that must be re-read.

**Implemented** with `ndk::media::media_codec` + `ndk::media::media_format` for
the codec and format work; only `AMediaExtractor_*` (not exposed by `ndk`) stays
as a small raw shim, including `memfd_create` (per-ABI syscall numbers) to feed
the in-memory `Arc<[u8]>` to `AMediaExtractor_setDataSourceFd`.

## Rust bindings: use the maintained `ndk` crate

Third-party AAudio/MediaCodec crates exist but most are stale: `aaudio`
(v0.1.1, ~6 years old), `mediacodec` (~4 years old), and `oboe`/`oboe-sys`
(11 M downloads, but wraps Google's **Oboe** *C++* library with an OpenSL ES
fallback — use only if we need pre-API-26 fallback).

**Decision:** use the maintained **`ndk` crate** (`rust-mobile/ndk`, 59 M
downloads), which ships *safe* Rust bindings for exactly what we need:
- `ndk::audio` (feature `audio`) — `AAudioStream` / `AAudioStreamBuilder`,
  data/error callbacks, `AudioFormat`, performance/sharing modes;
- `ndk::media::media_codec` + `ndk::media::media_format` (feature `media`) —
  MediaCodec + `AMediaFormat` (`AMediaExtractor` is **not** covered; use a small
  raw `extern "C"` shim for the extractor only).

`ndk` only compiles for an Android target, so add it **target-gated** to keep
host checks working:

```toml
[target.'cfg(target_os = "android")'.dependencies]
ndk = { version = "0.9", features = ["audio"] }   # or ["media"]
```

and implement the real path under `#[cfg(target_os = "android")]` with a
`#[cfg(not(target_os = "android"))]` stub alongside. Then a host
`cargo check --features platform-android` compiles the stub, while a real Android
build (NDK target) compiles the `ndk` path.

Lint: `ndk` is safe, but two operations still need `unsafe` — the audio callback
hands the buffer over as a raw `*mut c_void` (no safe slice wrapper), and the
extractor shim is raw FFI. Both crates therefore use
`#![cfg_attr(not(target_os = "android"), forbid(unsafe_code))]` +
`#![cfg_attr(target_os = "android", deny(unsafe_code))]`, with `#[allow(unsafe_code)]`
scoped to the one module each (`media`'s extractor shim; `platform`'s buffer
conversion). Rustc rejects a local `allow` under a crate-wide `forbid`, which is
why Android uses `deny`.

## What we cannot verify here
- No device/emulator in this environment, so the backends **cannot be run**. They
  *are* compile-checked: `cargo check --target aarch64-linux-android -p platform
  --features platform-android` and the equivalent `media` check compile the real
  `ndk` code (no link), so the API usage is type-checked, not just the host stub.
  Add an **Android CI job** (NDK target) so this stays checked, and validate on a
  device before calling it done. Everything testable stays covered by the
  Linux/null backends.
- On-device validation still needed: AAudio open/start + callback drain without
  underruns, `PCM_I16` correctness, real disconnect reap, whether `LOW_LATENCY`
  is granted; MediaCodec demux/decode loop, `memfd`+`setDataSourceFd`, seek +
  `flush`, colour-format negotiation (`COLOR_FormatYUV420Flexible` and the
  documented planar fallback), stride/slice-height/crop geometry, format-change
  timing, PCM encoding, and EOS/drain termination.
