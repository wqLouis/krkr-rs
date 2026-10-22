# `app::audio` — the decoder seam and the output device

Source: `crates/app/src/audio.rs`

Two things cross the L3 boundary here, and this module wires both:

* **the decoder** — the L1 `media` backend is installed on the
  `add_audio_decoder` chain, so a `WaveSoundBuffer` (and the legacy
  `CDDASoundBuffer` / `MIDISoundBuffer` classes) can decode and play;
* **the device** — the L1 output device is opened at game start and drained
  once per frame with a `dt`-derived chunk of the shared mixer.

The split is strict: **L1 owns the device, L2 owns the mixing, L3 renders on
request, and the root only shuttles buffers between them.** `compat` never
links a codec and never sees a device.

---

## 1. The decoder seam

```rust
pub fn install_audio_decoder(host: &CompatHost, media: &Media);
```

The installation marks the mixer as render-driven and adds one decoder to
the chain. The decoder owns a clone of the stateless `Media` backend and its
own explicit decoder state.

A track is decoded by a **chain** of backends, and the chain stops at the
first backend that decodes it:

| build | first | fallback |
|---|---|---|
| desktop (Linux) | the FFmpeg backend | the built-in decoder |
| Android | the built-in decoder | the platform's AAC decoder (MediaCodec) |

On the desktop the FFmpeg backend comes first: it reconstructs the SBR band
of an AAC track with the SBR extension, so such a track decodes at the rate
the container declares — the built-in decoder reports that track instead of
playing it at the wrong speed. The FFmpeg path **materializes the whole
track**, so playback seeks by frame without decoding again; the spec, the
loop information and the frame seek stay intact.

The built-in decoder stays in the chain because it **streams**: the container
is probed when the sound is opened (sample rate, channels, total frames) and
the samples are decoded as playback reads them. So a track the FFmpeg backend
cannot read still plays without holding the whole track in memory. It is the
Android build's first choice and the desktop build's fallback.

The **spec** is the container's whichever backend decodes the samples. The
desktop path probes the bytes with the built-in probe before it decodes them
with FFmpeg, and the rate, the channel count and the frame count the
container states are the ones the script reads (`Frequency`, `Channels`,
`TotalTime`) — the same values the built-in path reported. The samples are
FFmpeg's. A probe that fails, or a rate or a channel count that disagrees
with the decode, leaves the decoder's own spec in place: an HE-AAC track
declares the extension's rate while the built-in probe reports the core's,
and a container the built-in probe cannot read has no probe to compare with.

When every backend in the chain fails, the error names each of them and the
reason it gave, so neither is lost.

`MediaAudioStream` is the L2 `SoundSource` view of a decoded track: it
reports its spec, reads samples into the mixer's buffer and seeks by frame.
The decoder reports the end of the stream as an error, which the stream
translates into the zero-sample end-of-window signal.

---

## 2. The output device

```rust
pub(crate) struct AudioOutput { /* device, state, spec, buffer, carry, cap */ }
```

`AudioOutput::open(headless)` runs before anything else in the game host, so
a windowed host fails fast when there is no audio device.

| host | device | format |
|---|---|---|
| windowed | `AudioDevice::open(AudioDevice::default_spec()?)` | the device's own preferred format; the mixer resamples every source to it |
| headless | `AudioDevice::null(HEADLESS_SPEC)` | 48 kHz stereo, chosen rather than inherited |

The headless choice exercises the same call path — render plus a
frame-aligned write — and discards the samples. It is the one legitimate
no-op: it never panics and never blocks. The format only fixes the
render/buffer arithmetic, which is rate-independent.

A device that cannot be queried or opened is a typed `PlatformError`
surfaced through `GameError::Audio` — never silence.

---

## 3. The buffer and `dt` policy

The mixer is **render-driven**: the render call advances it by the frames it
produces, so the playback cursor is always the end of the audio rendered so
far. Each frame renders one chunk of `carry + dt_seconds × sample_rate`
frames and keeps the sub-sample remainder in `carry` for the next frame.
The carry makes the mapping sample-accurate over time: no rounding drift
accumulates, and consecutive chunks tile gaplessly whatever the frame
jitter, so no audio is dropped or repeated and the queue depth stays
constant.

The L1 device pre-fills its queue with roughly 150 ms before it starts and
fades the last per-channel sample to silence over a few milliseconds on a
real underrun. The device's latency is read back after each write and
published to the mixer, which is how a picture that follows the sound is
placed correctly.

### The playback cap

One frame may advance the playback cursor by at most a configured maximum
(`DEFAULT_MAX_CHUNK_MS`, 250 ms by default). The frame is clamped to the
cap **before** the carry is added, so a long frame advances at most the cap
and the excess is reported instead of being rendered or carried:

* the excess is logged at `warn` — the frame exceeded the cap and playback
  falls that far behind the wall clock;
* playback does **not** catch up, and no audio is dropped or repeated, so a
  single write cannot jump the queue past its bound;
* the cap is not a substitute for the L1 pre-fill: a small cap keeps the
  post-hitch latency low but rebuilds only a small reservoir, while a large
  cap covers more of the hitch in one write at the cost of more latency.

The cap is overridden by the `KRKR_RS_AUDIO_MAX_CHUNK_MS` environment
variable, which must be a positive, finite number of milliseconds. An
invalid value **fails the device open with a typed error** rather than being
ignored.

A `dt ≤ 0` renders nothing (the carry alone is under one frame). A
non-finite `dt` is warned about and skipped rather than being turned into a
giant allocation.

The movie picture advances by the same bound, so a long frame leaves the
picture and the sound equally far behind.

---

## 4. The per-frame pump

```rust
pub(crate) fn pump(&mut self, vm: &VmHost, dt_ms: f64) -> Result<(), GameError>
```

1. A non-finite `dt` is warned about and the frame is skipped.
2. The chunk is planned from the `dt`, the carry and the cap.
3. A frame over the cap is reported at `warn`.
4. A chunk of zero frames renders nothing.
5. Otherwise the buffer is resized, `compat::sound_render` renders into it,
   the device writes it, and the device's queued latency is published back
   to the mixer.

A failure to reach the bound compat context, a malformed output spec, or a
device that rejects the write is a typed error — never a silent frame.

---

## 5. Tests

| test | pins |
|---|---|
| `the_default_cap_is_250_ms` | the documented default |
| `a_frame_shorter_than_one_frame_renders_nothing` | a sub-frame `dt` renders nothing |
| `a_frame_at_the_cap_renders_the_whole_chunk` | the cap is not clamped from below |
| `a_frame_five_times_the_cap_advances_only_the_cap` | the excess is reported, not rendered |
| `the_carry_preserves_the_sub_frame_remainder` | the carry keeps the mapping sample-accurate |
| `a_long_frame_does_not_carry_the_excess` | the excess is behind, not carried |
| `a_different_cap_changes_the_advance` | the cap is the only bound |
| `a_negative_frame_renders_only_the_carry` | a negative `dt` renders nothing |
| `a_track_the_builtin_decoder_cannot_read_is_decoded_by_the_ffmpeg_backend` | the FFmpeg backend reads a container the built-in decoder has no reader for |
| `the_desktop_chain_decodes_an_ogg_track_with_the_ffmpeg_backend` | the desktop chain decodes with FFmpeg first, at the container's rate and channel count |
| `the_ffmpeg_path_reports_the_container_s_frame_count` | the frame count the script reads is the container's, not the decoder's |
| `the_fallback_decodes_a_wav_with_the_builtin_decoder` | a backend that cannot decode hands the track to the built-in decoder |
| `a_track_neither_backend_can_decode_names_both_reasons` | neither reason is lost |

---

## 6. Related documents

| topic | document |
|---|---|
| the crate overview and the wiring | [README.md](README.md) |
| the game host and the per-frame drain | [game.md](game.md) |
| the L2 sound model and the mixer | [`../engine/sound.md`](../engine/sound.md) |
| the L3 sound natives the decoder feeds | [`../compat/sound.md`](../compat/sound.md) |
| the L1 audio device and its queue | [`../arch/l1.md`](../../arch/l1.md) §4 |
| the media backends | [`../../migration/05-media-engine.md`](../../../docs.old/migration/05-media-engine.md) |
| the Android AAudio backend | [`../../android.md`](../../../docs.old/android.md) |
