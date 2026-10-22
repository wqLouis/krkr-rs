# `engine::sound` — the sound model and the software mixer

Source: `crates/engine/src/sound.rs`

This is the **state** half of sound: the playable sources, the playback
voices, the script-object half and the mixer that renders them.

* `SoundSource` — a playable PCM source: either caller-supplied decoded
  samples or an `AudioStream` that a decode-ahead worker fills a bounded
  window from on its own thread while the frame thread reads it; a read
  outside the window reseeks the worker. The stream's own `AudioSpec` and
  the optional generic loop information travel with it.
* `SoundChannel` — one playback voice: play/stop/pause, loop, pan,
  volume/volume2, playback rate, 3D position and linear fades.
* `SoundBuffer` — the script-object half: the loaded source, its `LoopFlags`,
  and label/status event tracking.
* `Mixer` — a clock-driven collection of channels plus the `Mixer::mix`
  action that renders the caller's output buffer.

---

## 1. Responsibility

* Own the decoded PCM (or the decoder that produces it) and the loop metadata.
* Own the per-channel playback state.
* Mix the channels into an output buffer.
* Report the events a script observes (status changes, fade completion, label
  crossings).

---

## 2. State it owns

**No device, no globals.** Nothing here opens an audio device and there is no
process-global state: the update loop calls `Mixer::advance` (or
`advance_to`) each frame, and the L1 `AudioDevice` pulls `Mixer::mix` into
its own callback. Two independent `Mixer` values in one process never
interact. A streaming `SoundSource` owns its decode-ahead worker thread and
its window as explicit per-source state.

| value | state |
|---|---|
| `SoundSource` | the PCM buffer, or the streaming window (`StreamBuffer`) plus the worker thread |
| `SoundChannel` | the source, the play state, the volume pair, the rate, the pan, the 3D position, the loop flags, the position, the output latency and the active fade |
| `SoundBuffer` | the loaded source, the `LoopFlags`, the labels and the status |
| `Mixer` | the channels, the output clock, the output latency, the global volume, the focus mode and the app focus/minimize flags |
| `LoopInfo` / `LoopLink` / `LoopCondition` / `SoundLabel` | the generic loop metadata |
| `LoopFlags` | the 16 flag values a loop condition reads |
| `Fade` | a linear volume ramp with a delay |

---

## 3. Public API

### Constants

```rust
pub const LOOP_FLAG_COUNT: usize = 16;
pub const MAX_LOOP_FLAG_VALUE: i32 = 9999;
pub const MAX_VOLUME: f32 = 1.0;
pub const MAX_PAN: f32 = 1.0;
```

### Sources

```rust
pub trait AudioStream: Send + Sync {
    fn spec(&self) -> AudioSpec;
    fn read(&mut self, out: &mut [f32]) -> Result<usize, SoundError>;
    fn seek(&mut self, frame: u64) -> Result<(), SoundError>;
}

impl SoundSource {
    pub fn new(audio: Arc<DecodedAudio>) -> Result<Self, SoundError>;
    pub fn with_loops(audio: Arc<DecodedAudio>, loops: LoopInfo) -> Result<Self, SoundError>;
    pub fn streaming(spec: AudioSpec, stream: Box<dyn AudioStream>,
                     info: Option<LoopInfo>) -> Result<Self, SoundError>;
    pub fn audio(&self) -> Result<&DecodedAudio, SoundError>;
    pub fn decode_entire(&self) -> Result<DecodedAudio, SoundError>;
    pub fn is_streaming(&self) -> bool;
    pub fn start_decoding(&self);
    pub fn spec(&self) -> AudioSpec;
    pub fn reported_frames(&self) -> Option<u64>;
    pub fn sample_rate(&self) -> u32;
    pub fn channels(&self) -> u32;
    pub fn frames(&self) -> u64;
    pub fn is_empty(&self) -> bool;
    pub fn duration_seconds(&self) -> f64;
    pub fn loops(&self) -> Option<&LoopInfo>;
    pub fn loop_link(&self) -> Option<&LoopLink>;
    pub fn loop_link_with_flags(&self, flags: &[i32]) -> Option<&LoopLink>;
    pub fn labels(&self) -> &[SoundLabel];
    pub fn frame(&self, frame: u64) -> Result<Option<(f32, f32)>, SoundError>;
    pub fn read_into(&self, out: &mut [f32], start_frame: u64, frames: usize)
        -> Result<usize, SoundError>;
    pub fn has_ended_at(&self, seconds: f64) -> bool;
    pub fn playback_ended(&self, seconds: f64) -> bool;
}
```

A streaming source takes the container's `AudioSpec` (so the duration is
known without decoding) and the decoder itself; the two must agree on the
sample rate and the channel count, or the source is rejected.

A materialized source takes the decoded buffer and the spec the decoder
reported: a spec that states a frame count keeps it — the container's own
length, so the track's duration does not depend on which decoder produced
the samples — and a spec without one falls back to the buffer's length.

### Channels, buffers and the mixer

```rust
impl SoundChannel {
    pub fn load(&mut self, source: Arc<SoundSource>);
    pub fn play(&mut self, source: Arc<SoundSource>);
    pub fn stop(&mut self); pub fn pause(&mut self); pub fn resume(&mut self);
    pub fn position_seconds(&self) -> f64;
    pub fn played_position_seconds(&self) -> f64;
    pub fn set_position(&mut self, seconds: f64);
    pub fn volume(&self) -> f32; pub fn set_volume(&mut self, volume: f32);
    pub fn volume2(&self) -> f32; pub fn set_volume2(&mut self, volume: f32);
    pub fn rate(&self) -> f64; pub fn set_rate(&mut self, rate: f64);
    pub fn pan(&self) -> f32; pub fn set_pan(&mut self, pan: f32);
    pub fn pos(&self) -> [f32; 3]; pub fn set_pos(&mut self, x: f32, y: f32, z: f32);
    pub fn flags(&self) -> &LoopFlags; pub fn flags_mut(&mut self) -> &mut LoopFlags;
    pub fn effective_volume(&self) -> f32;
    pub fn output_gain(&self) -> f32;
    pub fn spatial_attenuation(&self) -> f32;
    pub fn fade(&mut self, to: f32, duration: f64, delay: f64);
    pub fn active_fade(&self) -> Option<&Fade>;
    pub fn fade_finished(&self) -> bool;
    pub fn duration_seconds(&self) -> f64;
    pub fn advance(&mut self, dt: f64) -> bool;
    /* plus id, source, is_playing, is_paused, is_done, status, output_latency */
}

impl SoundBuffer {
    pub fn new() -> Self;
    pub fn open(&mut self, source: Arc<SoundSource>);
    pub fn close(&mut self);
    pub fn is_loaded(&self) -> bool;
    pub fn source(&self) -> Option<&Arc<SoundSource>>;
    pub fn flags(&self) -> &LoopFlags; pub fn flags_mut(&mut self) -> &mut LoopFlags;
    pub fn labels(&self) -> &[SoundLabel];
    pub fn status(&self) -> Status;
    pub fn apply_flags_to(&self, channel: &mut SoundChannel);
    pub fn load_on(&self, channel: &mut SoundChannel) -> Result<(), SoundError>;
    pub fn play_on(&self, channel: &mut SoundChannel) -> Result<(), SoundError>;
    pub fn poll(&self, channel: &mut SoundChannel) -> Vec<SoundEvent>;
}

impl Mixer {
    pub fn new() -> Self;
    pub fn channels(&self) -> &[SoundChannel];
    pub fn spawn_channel(&mut self) -> u64;
    pub fn remove_channel(&mut self, id: u64);
    pub fn channel(&mut self, id: u64) -> Option<&mut SoundChannel>;
    pub fn channel_ref(&self, id: u64) -> Option<&SoundChannel>;
    pub fn advance(&mut self, dt: f64);
    pub fn advance_to(&mut self, now: f64);
    pub fn clock(&self) -> f64;
    pub fn output_latency(&self) -> f64;
    pub fn set_output_latency(&mut self, seconds: f64);
    pub fn global_volume(&self) -> f32; pub fn set_global_volume(&mut self, volume: f32);
    pub fn global_focus_mode(&self) -> i32; pub fn set_global_focus_mode(&mut self, mode: i32);
    pub fn focus_gain(&self) -> f32;
    pub fn mix(&self, out: &mut [f32], out_spec: AudioSpec) -> Result<(), SoundError>;
    pub fn mix_advancing(&mut self, out: &mut [f32], out_spec: AudioSpec)
        -> Result<(), SoundError>;
}

pub fn mix(channels: &[SoundChannel], settings: MixSettings, out: &mut [f32],
           out_spec: AudioSpec) -> Result<(), SoundError>;
pub fn mix_source(source: &SoundSource, position_seconds: f64, rate: f64, gain: f32,
                  pan: f32, out: &mut [f32], out_spec: AudioSpec) -> Result<(), SoundError>;
```

`mix_advancing` is the **device-driven clock**: the L1 `AudioDevice` callback
calls it, and each rendered chunk moves the play positions so the next
callback does not replay it. A headless application uses `advance`/
`advance_to` instead; the two clocks must not drive the same mixer at once.

### Types

`SoundError`, `Status`, `SoundEvent`, `LoopCondition`, `LoopLink`,
`LoopInfo`, `LoopFlags`, `SoundLabel`, `Fade`, `MixSettings`.

---

## 4. Invariants

* **The mixer owns its state.** There is no process-global mixer, no shared
  cursor and no global channel registry; the L3 `SoundChannel` native
  instances are thin handles onto these.
* **Channel ids start at 1**; `0` is the "no channel" sentinel the script
  surface uses.
* **The output clock is monotonic.** `advance_to` advances by 0 for a
  timestamp that moves backwards.
* **The output latency is the distance between the rendered position and the
  audible one.** `played_position_seconds` is the rendered position minus
  that latency.
* **A fade is a linear ramp** from `from` to `to` over `duration` seconds,
  after an optional `delay`; its volume is clamped to `0..=1`.
* **The status is `Unload`, `Stop` or `Play`.** A paused channel still
  reports `Play`: pause is not a status in the audio model (it *is* one in
  the video model).
* **The loop flags are 16 values**, each `0..=9999`; a loop link's condition
  compares one of them against a reference value.
* **A streaming source decodes ahead on its own thread** into a bounded
  window; a read outside the window asks the worker to reseek. The first
  decoder failure is reported to every reader.
* **`mix` writes into the caller's buffer** with the caller's output spec; it
  never allocates an output device.

### The resampling kernel

A source is converted to the output rate by convolving the source frames
around the interpolated one with a 65-tap windowed-sinc kernel:

* the window is the Blackman window, symmetric about the kernel's centre and
  vanishing at both of its ends;
* the sinc's argument is scaled by the ratio of the output rate to the
  source's rate (times the playback rate), capped at one, so the passband
  follows the **output's** Nyquist frequency: the content above it is
  rejected instead of being folded back into the band;
* the weights are normalised over the frames the kernel actually holds, so a
  track that ends inside the kernel keeps its gain.

An upsampling conversion keeps the source's own band. The scale does not
change the pitch: the conversion reads the source at the output's time, so a
tone stays at its frequency.

---

## 5. Data flow

```
L1 decoder (FFmpeg / MediaCodec)
   │  installed as an AudioStream by the host
   ▼
SoundSource::streaming ──► decode-ahead worker ──► bounded frame window
   │
   ▼
SoundBuffer (the script object) ──apply_flags_to──► SoundChannel
   │                                                 │
   │ advance(dt) ◄── Mixer::advance(dt) ◄── frame   │
   ▼                                                 │
Mixer::mix(out, out_spec) ◄── L1 AudioDevice callback
   │        (or Mixer::mix_advancing, which also advances the clock)
   ▼
the device's output buffer
```

Events flow the other way: `SoundBuffer::poll(channel)` returns the
`SoundEvent`s observed since the previous poll (`StatusChanged`,
`FadeCompleted`, `Label`).

---

## 6. Failure modes

| failure | behaviour |
|---|---|
| an unusable audio specification | `SoundError::InvalidSpec` |
| a source with a channel count the mixer cannot render | `SoundError::UnsupportedChannels` |
| a decoded buffer whose length disagrees with its spec | `SoundError::InconsistentBuffer` |
| a buffer with no loaded source | `SoundError::NoSource` |
| an incremental decoder fails while reading or seeking | `SoundError::Decode` |
| the decode-ahead worker cannot be started | `SoundError::DecodeThread` |

Invalid PCM and spec values are rejected when the source is **built**, not
when it is played, and there are no hardcoded chain, size or loop caps.

---

## 7. Current limits

* **The mixer pump still runs inside the L3 hook**, not inside L2: the
  composition root's frame hook advances the mixer and drains the device.
* **`SoundState.clock`** (L3) has not been retired for an L2-owned delta.

---

## 8. Tests

* `crates/engine/tests/sound.rs` — the mixer, the channels, the fades, the
  loop conditions, the labels, the streaming source and the errors.
* `crates/engine/src/sound.rs` `mod tests` — the unit-level invariants: the
  resampling window's symmetry, the kernel's passband and the band it
  rejects, and the kernel's scale.

---

## 9. See also

| topic | document |
|---|---|
| the L3 ↔ L2 protocol for this module | [`rules/api_rules.md`](../../rules/api_rules.md) §4 |
| the sound model in the architecture | [`arch/l2.md`](../../arch/l2.md) §6 |
| the L1 media engine and the audio device | [`arch/l1.md`](../../arch/l1.md) §4 |
| the frame that drives the mixer (still in the hook) | [engine.md](engine.md) |
| the archived protocol trail | [`docs.old/migration/14-l3-l2-protocol.md`](../../../docs.old/migration/14-l3-l2-protocol.md) §6.6 |
