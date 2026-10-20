# `engine::video` — the video-overlay playback machine

Source: `crates/engine/src/video.rs`

The module models the TVP `VideoOverlay` playback machine (open/play/stop/
pause/seek, position/duration, visible, loop, segment loops, overlay
binding) as explicit L2 state. It consumes frames and positions supplied by
the caller — **L1 `media` owns decoding** — and never opens a codec itself.

---

## 1. Responsibility

* Own the overlay's status, clock, position, play rate, looping, period
  events, the current decoded frame, the binding and the pending event
  stream.
* Convert the decoded frame into a layer's main image.
* Report the script-visible events.

---

## 2. State it owns

`VideoOverlay` is a **plain, caller-owned value**: there is no process global,
no active-instance registry and no clock. The update loop calls
`VideoOverlay::advance` with the elapsed milliseconds each frame; the caller
asks L1 `media` for the frame at `VideoOverlay::position_ms` and hands it back
with `VideoOverlay::present_frame`.

| field | meaning |
|---|---|
| `spec: Option<VideoSpec>` | the open stream's description |
| `status: PlayStatus` | `Unload` / `Stop` / `Play` / `Pause` |
| `playing: bool` | whether the clock runs |
| `position_exact_ms: f64` | the position with sub-millisecond resolution |
| `play_rate`, `looping` | the playback rate multiplier and whole-stream looping |
| `segment_start`, `segment_end` | the configured segment loop |
| `period_event_frame`, `period_past` | the configured period event |
| `preparing` | whether the machine waits for the next delivered frame |
| `visible`, `bounds` | the overlay's visibility and rectangle |
| `binding` | the bound layer/window and the mixing opacity |
| `frame: Option<Arc<RgbaFrame>>` | the picture on screen (shared, not copied) |
| `last_frame_update` | the picture index the last frame update was reported for |
| `restarted_at_loop` | whether the last advance put the clock back to a loop boundary |
| `pending: Vec<VideoEvent>` | the queued events |

---

## 3. Public API

```rust
impl VideoOverlay {
    pub fn new() -> Self;
    pub fn open(&mut self, spec: VideoSpec) -> Result<(), VideoError>;
    pub fn close(&mut self);
    pub fn is_open(&self) -> bool;
    pub fn spec(&self) -> Option<&VideoSpec>;
    pub fn status(&self) -> PlayStatus;
    pub fn is_playing(&self) -> bool;

    pub fn play(&mut self) -> Result<(), VideoError>;
    pub fn pause(&mut self) -> Result<(), VideoError>;
    pub fn stop(&mut self) -> Result<(), VideoError>;
    pub fn rewind(&mut self);
    pub fn prepare(&mut self) -> Result<(), VideoError>;

    pub fn position_ms(&self) -> i64;
    pub fn position_exact_ms(&self) -> f64;
    pub fn frame(&self) -> i64;
    pub fn sync_clock(&mut self, milliseconds: f64);
    pub fn restarted_at_loop(&self) -> bool;
    pub fn duration_ms(&self) -> i64;
    pub fn total_frames(&self) -> i64;
    pub fn fps(&self) -> f64;
    pub fn seek_ms(&mut self, milliseconds: i64);
    pub fn seek_frame(&mut self, frame: i64);

    pub fn segment_loop(&self) -> Option<(i64, i64)>;
    pub fn set_segment_loop(&mut self, start: i64, end: i64);
    pub fn cancel_segment_loop(&mut self);
    pub fn period_event(&self) -> Option<i64>;
    pub fn set_period_event(&mut self, frame: i64);
    pub fn cancel_period_event(&mut self);

    pub fn visible(&self) -> bool;
    pub fn set_visible(&mut self, visible: bool);
    pub fn set_position(&mut self, left: i32, top: i32);
    pub fn set_size(&mut self, width: u32, height: u32);
    pub fn binding(&self) -> OverlayBinding;
    pub fn bind_layer(&mut self, layer: u32);
    pub fn bind_window(&mut self, window: u32);
    pub fn unbind(&mut self);
    pub fn set_binding_alpha(&mut self, alpha: f32);

    pub fn present_frame(&mut self, frame: Arc<RgbaFrame>) -> Result<(), VideoError>;
    pub fn complete_prepare(&mut self);
    pub fn current_frame(&self) -> Option<&RgbaFrame>;
    pub fn clear_frame(&mut self);
    pub fn current_frame_pts_ms(&self) -> Option<u64>;
    pub fn pending_events(&self) -> &[VideoEvent];
    pub fn poll(&mut self) -> Vec<VideoEvent>;
    pub fn take_status_events(&mut self) -> Vec<PlayStatus>;
    pub fn advance(&mut self, dt_ms: f64);
}

pub fn blit_frame_to_layer(scene: &mut Scene, layer_id: u32, frame: &RgbaFrame,
                            source_over: bool) -> bool;
pub fn blit_frame_to_layer_opacity(scene: &mut Scene, layer_id: u32, frame: &RgbaFrame,
                                   source_over: bool, opacity: u8) -> bool;
```

### Types

`VideoOverlay`, `PlayStatus`, `PeriodReason`, `VideoEvent`, `VideoBounds`,
`OverlayBinding`, `VideoError`.

Two of those types carry the script-facing value:

| method | value |
|---|---|
| `PlayStatus::as_str()` | the reference status string: `"unload"`, `"stop"`, `"play"`, `"pause"` |
| `PeriodReason::code()` | the integer the script's `onPeriod(reason)` receives: `0` (whole-stream loop), `1` (the configured period frame), `2` (prepare), `3` (a segment-loop wrap) |

L3 turns them into the `onStatusChanged` and `onPeriod` arguments.

---

## 4. The clock

The reference stores an absolute play anchor and derives the position from
the engine tick. To stay free of a global clock, this model advances a
**relative** position by `dt * play_rate`; the two are equivalent for a
monotonically increasing clock, and a relative step is directly testable.

The position is kept with **sub-millisecond resolution**
(`position_exact_ms`) and never rounded per step: a step of `1000/60` ms is
`16.666…` ms, and rounding it to `17` ms would make the clock run two
percent fast and skip a frame's worth of audio on every step.
`position_ms` rounds the accumulated value for the script surface.

---

## 5. Invariants

* **`Pause` is a distinct status here** (unlike the audio model), matching
  the reference's `ssPlay`/`ssStop`/`ssPause`/`ssUnload`.
* **The status event fires only on a real change.**
* **The frame update belongs to a picture**: it fires once per decoded
  picture, so a picture that is already on screen fires nothing, and a
  picture that arrives while the overlay is not playing fires nothing either.
  A frame update is reported **before** the period events of the same update.
* **A transport jump re-arms the frame update**, because the stream decodes
  a new picture.
* **Prepare mode freezes the clock** until the next delivered frame; the
  caller presents and composites that picture, then calls
  [`VideoOverlay::complete_prepare`], which queues `perPrepare` and only then
  pauses and rewinds. This keeps the prepared picture visible on the prepare
  frame.
* **A segment loop wraps before the whole-stream end**, whether or not
  whole-stream looping is enabled.
* **A non-looping stream that crosses its end stops and rewinds to frame 0**,
  queuing `TransitionComplete` — the reference's completion handler calls
  `Stop()`, which rewinds before reporting the status.
* **A configured period event fires once per arming.**
* **The bounds, the frame size and the blit rects are VM pixels.**
* **The current frame is shared, not copied**: the decoder hands the same
  buffer to every holder.

---

## 6. Data flow

```
L1 media (FFmpeg / MediaCodec)
   │  decodes a picture
   ▼
VideoOverlay::present_frame(frame)      → the picture on screen + a frame update
   │
   ▼
blit_frame_to_layer_opacity(scene, layer, frame, source_over, opacity)
   │        → the layer's MainImage
   │
   ▼
VideoOverlay::complete_prepare()       → perPrepare + pause + rewind
   │
   ▼
bitmap marked dirty → engine::Engine::sync_dirty_bitmaps → upload_textures

clock:  frame loop ──► VideoOverlay::advance(dt_ms)
events: VideoOverlay::poll() ──► VideoEvent list ──► L3 dispatches on* members
```

---

## 7. Failure modes

| failure | behaviour |
|---|---|
| an unusable `VideoSpec` (non-finite or non-positive `fps`, a missing or zero total frame count) | `VideoError::InvalidSpec` |
| a playback command on a closed overlay | `VideoError::NotOpen` |
| a decoded frame whose length disagrees with its dimensions | `VideoError::InconsistentFrame` |
| a frame with no pixels or a layer with no main image | `blit_frame_to_layer*` returns `false` |

Script-visible values are not validated here: the reference stores whatever
it is given.

---

## 8. Current limits

* **The video pump still runs inside the L3 hook**, not inside L2.

---

## 9. Tests

* `crates/engine/tests/video.rs` — the playback machine, the clock, the
  looping, the period events, the binding and the frame copy.
* `crates/engine/src/video.rs` `mod tests` — the unit-level invariants.

---

## 10. See also

| topic | document |
|---|---|
| the L3 ↔ L2 protocol for this module | [`rules/api_rules.md`](../../rules/api_rules.md) §4 |
| the video model in the architecture | [`arch/l2.md`](../../arch/l2.md) §7 |
| the L1 media engine and the decoders | [`arch/l1.md`](../../arch/l1.md) §4 |
| the pixel operations the frame copy uses | [layer_ops.md](layer_ops.md) |
| the frame that advances the overlay (still in the hook) | [engine.md](engine.md) |
| the archived protocol trail | [`docs.old/migration/14-l3-l2-protocol.md`](../../../docs.old/migration/14-l3-l2-protocol.md) §6.7 |
