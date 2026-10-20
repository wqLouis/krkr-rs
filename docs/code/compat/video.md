# `compat::natives::video` — the `VideoOverlay` class

Source: `crates/compat/src/natives/video.rs` · layer **L3** · class
`VideoOverlay`

`VideoOverlay` plays a video file and presents it: as an overlay, into a
bound `Layer`, or through a mixing layer. The playback machine, the clock, the
frame index and the event queue are L2's; the class is the L3 adapter that
marshals the script surface onto them.

See [`../../code/engine/video.md`](../../code/engine/video.md) for the L2
side.

---

## 1. Registered surface

70 members in total: 22 methods (the constructor included) and 48
properties, plus the inherited `finalize`.

**Methods (22):** `VideoOverlay` (the constructor), `open`, `close`, `play`,
`pause`, `stop`, `rewind`, `prepare`, `setPos`, `setSize`, `setBounds`,
`setSegmentLoop`, `cancelSegmentLoop`, `setPeriodEvent`, `cancelPeriodEvent`,
`setMixingLayer`, `resetMixingLayer`, `selectAudioStream`,
`onStatusChanged`, `onCallbackCommand`, `onPeriod`, `onFrameUpdate`.

**Properties (48):**

* geometry: `left`, `top`, `width`, `height`, `visible`;
* transport: `position`, `frame`, `loop`, `mode`, `playRate`;
* period and segment loop: `periodEventFrame`, `segmentLoopStartFrame`,
  `segmentLoopEndFrame`;
* audio: `audioVolume`, `audioBalance`, `enabledAudioStream`,
  `enabledVideoStream`, `numberOfAudioStream`, `numberOfVideoStream`;
* source: `originalWidth`, `originalHeight`, `numberOfFrame`, `fps`,
  `totalTime`;
* colour adjustment: `contrast`, `brightness`, `hue`, `saturation` and each
  one's `…RangeMin`, `…RangeMax`, `…DefaultValue`, `…StepSize` (20 members);
* mixing: `mixingMovieAlpha`, `mixingMovieBGColor`;
* layers: `layer1`, `layer2`.

There is **no `selectVideoStream` method**: the video-stream selector is only
reachable through the `enabledVideoStream` setter, which routes to the same
single-stream check.

---

## 2. Modes

| mode | meaning |
|---|---|
| 0 | `vomOverlay` — the reference's OS overlay window |
| 1 | `vomLayer` — the frame is composited into the bound layers |
| 2 | `vomMixer` — the frame is composited through the mixing layer |

`mode` is **frozen while a stream is open** — a write is ignored. The
reference default is `vomOverlay`.

In `vomLayer` mode the frame is blitted into `layer1` (and `layer2` when it
is set), honouring the layer's opacity. In `vomMixer` mode it goes through
the mixing layer instead. `vomOverlay` presents the frame but has **no
script-visible destination** here.

A fresh overlay has the reference's default **320×240** rectangle.

---

## 3. The host seam

L3 links **no** decoder and **no** FFmpeg. The host supplies decoded data
through the `VideoSource` trait:

```rust
pub trait VideoSource {
    fn spec(&self, path: &str) -> Option<VideoSpec>;
    fn frame_at(&mut self, path: &str, at_ms: u64) -> Option<Arc<RgbaFrame>>;
    fn audio(&mut self, path: &str) -> Result<Option<Arc<DecodedAudio>>, MediaError>;
}
```

The host installs one with `CompatHost::set_video_source` or
`compat::set_video_source`.

Without a source, `open` raises
`VideoOverlay.open: no decoded source for "<path>"` rather than fabricating
metadata.

The frame step is `compat::video_advance(ctx, dt_ms)`, the frame body's
`video_poll` step. It advances each open overlay by the **caller's**
per-frame `dt` — never by wall time — presents the host frame once, composites
it, completes a pending prepare, syncs the audio and dispatches the queued
events. The composite precedes the prepare pause/rewind so the prepared picture
is visible when `perPrepare` fires.

---

## 4. Events

| member | argument | when |
|---|---|---|
| `onStatusChanged(status)` | `"unload"`, `"stop"`, `"play"` or `"pause"` | **synchronously** inside the calling command |
| `onPeriod(reason)` | an integer reason code | at the next `video_advance` |
| `onFrameUpdate(frame)` | the integer frame index | once per advance that reaches a new frame |
| `onCallbackCommand(...)` | — | **never invoked** by the engine |

The `onPeriod` reason codes are `0` (whole-stream loop), `1` (the configured
period frame), `2` (`prepare`) and `3` (a segment-loop wrap).

The status sequence for a full transport run is exactly
`"stop,play,pause,stop,unload,"`: `open` → `"stop"`, `play` → `"play"`,
`pause` → `"pause"`, `stop` → `"stop"`, `close` → `"unload"`.

Dispatch goes through the object's class chain, so a script override wins
over the native hook. Handler failures are **logged, never raised**.

`prepare()` in `vomLayer` mode ends **paused at frame 0** and fires
`onPeriod(2)` on the first decoded frame. The frame is composited first; then
`complete_prepare()` queues `onFrameUpdate`, `onPeriod(2)`, the pause and the
rewind. The next idle pump leaves the rewound picture on the layer.

`onCallbackCommand` is registered but never invoked: the reference fires it
from host decoder callbacks, and this port has no equivalent seam.

---

## 5. The movie audio

The decoded audio track plays through the **sound family's mixer** as a
*foreign voice* — it is not a script `SoundBuffer`.

`audioVolume` uses the reference's `0..=100000` gain and `audioBalance` its
`-100000..=100000` pan; the conversion is the same helper the sound family
uses, so the scale is identical.

The voice keeps the mixer's own clock. It is put back on the picture's
position only on an explicit transport command, or when the drift exceeds
0.25 s.

---

## 6. State ownership

| state | owner |
|---|---|
| the slots, the next id, the installed `VideoSource` | the **per-engine dispatch state** (`CompatServices::state::<VideoState>()`) |
| the L2 overlay, the opened path, the raw owner handle, the layer references, the mixing layer, the mode, the audio balance and volume, the stream selectors and counts, the decoded track and its mixer voice | the **per-object payload** |
| the spec, the status, the position clock, the frame index, the segment loop, the period event, the visibility, the bounds, the binding, the play rate, the loop flag, the event queue, the presented frame | the **L2 video model** |

---

## 7. Errors

| shape | example |
|---|---|
| `"VideoOverlay.<member>: compat context is not bound"` | the engine binding is missing |
| `"VideoOverlay.<member>: object is not constructed"` | the constructor has not run |
| `"VideoOverlay: specify layer"` | a non-`Layer` object in a layer slot |
| `"VideoOverlay: specify layer: {error}"` | the layer lookup failed |
| `"VideoOverlay.open: no decoded source for \"<path>\""` | no host source installed |
| `"VideoOverlay.selectAudioStream: this decoder decodes a single audio stream"` | a second stream was requested |
| `"VideoOverlay.selectVideoStream: this decoder decodes a single video stream"` | the same for video |

The arity messages use the reference's shapes:
`VideoOverlay.selectAudioStream requires 1 argument`, and so on.

---

## 8. Tests

`crates/compat/tests/video.rs` covers the read-only media members and their
reset on `close`, the "no source" and "invalid spec" errors, the bounds and
visibility round trip, the transport commands, the inert behaviour on a
closed overlay, the default 320×240 rectangle, the loop and period members,
the segment loop and the play rate (any value is accepted), the layer
references and their validation, the mixing-layer selection, the passthrough
members and their closed/open sentinels, the stubbed colour adjustment (all
20 members read `-1.0`), the reachability of every member, the three
compositing modes, the advance using the frame body's `dt`, the synchronous
status events, `prepare` acting only in layer mode, the frozen `mode`, the
layer-mode geometry, the constructor's window requirement, the audio members
before `open`, the stream counts and the default gain, the typed error for a
second audio stream, and the movie audio reaching the mixer with its volume
and pan.

Unit tests inside the module cover the event→script-member mapping and the
frame selection of the buffered source.

---

## 9. Known gaps

1. **`onCallbackCommand` is never fired** — there is no host decoder-callback
   seam.
2. **The transition-complete event is dropped**: the reference's
   `setTransitionCompleteCall` is not part of the surface.
3. **The colour adjustment is entirely stubbed.** `contrast`, `brightness`,
   `hue`, `saturation` and their sixteen range/default/step members all read
   `-1.0`, and their setters are inert.
4. **`mixingMovieAlpha` and `mixingMovieBGColor` are backend stubs**:
   `1.0` / `0xFF000000` while open, `0.0` / `0` while closed, with inert
   setters.
5. **Only one audio and one video stream can be selected**; a different
   in-range index raises a typed error instead of switching.
6. **`vomOverlay` has no script-visible destination** — the frame is
   presented but not composited into any layer, because the reference draws
   through an OS overlay window.
7. **`setSize` / `setBounds` and the `width` / `height` setters are no-ops
   in `vomLayer` mode**, and the getters keep reading the wrapper's own
   rectangle.
8. **`mode` is frozen while a stream is open.**
9. **There is no `vomOverlay` OS window** and no video filters of any kind.
10. **The movie audio is a foreign mixer voice**, not a script
    `SoundBuffer`.
11. **`setPeriodEvent` with two or more arguments is ignored**, matching the
    reference.

---

## 10. Related documents

| topic | document |
|---|---|
| the L2 video overlay machine | [`../../code/engine/video.md`](../../code/engine/video.md) |
| the sound family and the volume scale | [`sound.md`](sound.md) |
| the frame body and its poll order | [`../../arch/l3.md`](../../arch/l3.md) §6 |
| the L3 ↔ L2 protocol | [`../../../docs.old/migration/14-l3-l2-protocol.md`](../../../docs.old/migration/14-l3-l2-protocol.md) |
