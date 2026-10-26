# `compat::natives::sound` — the `WaveSoundBuffer` family

Source: `crates/compat/src/natives/sound.rs`, `sound/phase_vocoder.rs`,
`sound/wave_flags.rs` · layer **L3** · classes `WaveSoundBuffer`,
`WaveFlags`, `__TvpPhaseVocoder`

The sound family is the script's audio surface. `WaveSoundBuffer` plays a
decoded wave file with the reference's loop/label machinery; `WaveFlags` is the
conditional-loop flag proxy it exposes; `__TvpPhaseVocoder` is the
time/pitch filter a script can push onto a buffer's `filters` array.

The playback model, the mixer and the loop model are L2's; this page covers
the L3 adapter. See [`../../code/engine/sound.md`](../../code/engine/sound.md)
for the L2 side.

The disc-sound classes (`CDDASoundBuffer`, `MIDISoundBuffer`) are documented
separately in [`cdda_midi.md`](cdda_midi.md).

---

## 1. Registered surface

### `WaveSoundBuffer` — 33 members

**Instance methods (11):** `WaveSoundBuffer` (the constructor), `open`,
`play`, `stop`, `fade`, `stopFade`, `setPos`, `getVisBuffer`,
`onStatusChanged`, `onFadeCompleted`, `onLabel`.

**Instance properties (19):**

* read/write (13): `position`, `samplePosition`, `paused`, `totalTime`,
  `looping`, `volume`, `volume2`, `pan`, `posX`, `posY`, `posZ`, `frequency`,
  `useVisBuffer`;
* read-only (6): `status`, `bits`, `channels`, `flags`, `labels`, `filters`.

**Static members (3):** the method `freeDirectSound`, and the read/write
properties `globalVolume` and `globalFocusMode`.

Six more members are attached to the same class object by the `getSample`
plugin: the method `getSample`, the read-only property `sampleValue`, the
read/write properties `sampleCount` and `sampleAhead`, and the methods
`setDefaultCounts` and `setDefaultAheads`.

### `WaveFlags` — 21 members

The constructor `WaveFlags`, the method `reset`, the read-only properties
`count` and `N`, and the sixteen numeric properties `"0"` … `"15"`.

### `__TvpPhaseVocoder` — 6 members

The constructor, the read-only property `interface`, and the read/write
properties `window`, `overlap`, `pitch` and `time`.

The class is registered under a hidden global name and aliased onto the
`WaveSoundBuffer` class object by a script that runs at registration time
(and only if `WaveSoundBuffer.PhaseVocoder` is not already defined), so
scripts spell it `WaveSoundBuffer.PhaseVocoder`.

---

## 2. The volume scale

The script scale is the reference's: volume `0..=100000`, pan
`-100000..=100000`. The L2 mixer works in normalized gain and balance.

The conversion happens **in L3, at the script boundary**, in four helpers on
the per-engine sound state:

```rust
fn script_volume_to_gain(value: i64) -> f32;   // value / 100000, clamped to 0..1
fn script_pan_to_balance(value: i64) -> f32;    // value / 100000, clamped to -1..1
fn to_script_volume(gain: f32) -> i64;          // round(gain * 100000)
fn to_script_pan(balance: f32) -> i64;
```

The clamp is what reproduces the reference's setter behaviour: `200000`
becomes `100000`, `-5` becomes `0`.

`VideoOverlay` reuses the same two helpers for the movie audio.

---

## 3. `open`

`open(name)`:

1. clears the buffer (stop, rewind to 0, drop the loop manager, `paused`
   false, status `unload`);
2. reads the storage entry;
3. reads and parses the `<name>.sli` side-car;
4. runs the decoder chain;
5. stores the format and the frequency;
6. loads the source onto the channel and sets the status to `stop`.

The decoder chain is a **host seam**: L3 links no codec. The host installs
one with

```rust
pub type AudioDecoder =
    Box<dyn Fn(&[u8], Option<LoopInfo>, Option<&str>) -> Result<Arc<SoundSource>, String>>;
pub fn add_audio_decoder(services: &CompatServices, decoder: AudioDecoder);
```

Every installed decoder is tried in order and the first success wins. When
none matches, **every** decoder's error is reported, joined with `"; "`.

The `.sli` side-car is looked up as `<bare name>.sli` and parsed by the
module's own parser, which supports the two reference formats:

* the old format — `LoopStart=<n>` / `LoopLength=<n>`;
* the `#2.00` format — `Link { … }` and `Label { … }` blocks with
  `key=value;` bodies.

`Link` keys: `from`, `to`, `smooth`, `condition`, `refvalue`, `condvar`.
`Label` keys: `position`, `name`. The condition spellings are `no`, `eq`,
`ne`, `gt`, `ge`, `lt`, `le`.

A failed `.sli` parse raises and leaves the buffer unloaded, but the format
that was assigned **before** the parse survives — the reference assigns the
input format before it reads the side-car too.

---

## 4. The loop flags

`WaveFlags` is a proxy over the L2 `LoopFlags` (a `[i32; 16]`).

* `count` and `N` are always **16**.
* The flag properties read `0` and drop writes while no source is open,
  because the reference's loop manager does not exist before the first
  `open`.
* A written value is clamped to `0..=9999`.
* `reset()` clears every flag and re-applies them to the channel.

The proxy object is cached on the owning buffer, so `s.flags === s.flags`
holds.

---

## 5. Events

Three event members exist, and each has a matching native member that a
script may override. The poll dispatches **through the object's class
chain**, so a script override wins over the native member; the native member
then re-invokes the constructor's action owner as `actionOwner.action(event)`.

| member | event dictionary | when |
|---|---|---|
| `onStatusChanged(status)` | `{type:"onStatusChanged", target, status}` | **synchronously** inside `open` / `play` / `pause` / `stop`; **asynchronously** at the next `sound_poll` when the track ends on its own |
| `onLabel(name)` | `{type:"onLabel", target, name}` | at the next `sound_poll` |
| `onFadeCompleted()` | `{type:"onFadeCompleted", target}` | see below |

The status strings are exactly `"unload"`, `"stop"` and `"play"`. A **paused**
buffer still reports `"play"` — pause is not a status.

`onStatusChanged` is **not** raised when `stop` is called while the status is
`unload`. A status change **cancels** the events still queued for that
object, so a label crossed in the same drain is lost.

`onFadeCompleted` fires:

* synchronously inside `fade` when a running fade is interrupted;
* synchronously inside `fade` when the fade time is below one 60 ms beat and
  there is no delay — the volume jumps to the target;
* synchronously inside `stopFade` when a fade was running;
* asynchronously at the next `sound_poll` for a natural completion.

A `stopFade` with no fade running is a strict no-op: no event, no volume
write.

The argument count of the three event members is checked **only when an
action owner exists**; with no owner they are silent void no-ops.

Handler failures are **logged, never raised**.

---

## 6. `fade`

`fade(to, timeMs[, delayMs])` implements the reference's fade state machine
(target volume, delta, count, blank-left).

* `time <= 0` or `delay < 0` raises `WaveSoundBuffer.fade: Invalid argument`
  (the reference's `TVPInvalidParam`).
* A fade **never starts playback**.
* A fade below one 60 ms beat completes inside the call.
* A fade with a delay below one beat never completes: the volume stays
  frozen and the fade stays "running".

---

## 7. `getVisBuffer`

`getVisBuffer(dest, numsamples, channels[, ahead])` copies decoded samples
around the channel's play position into `dest` as interleaved signed 16-bit
PCM and returns the number of frames written.

It returns `0` when `useVisBuffer` is off, when the buffer is not playing,
when the requested layout is neither mono nor the source's own layout, or
when the count is not positive.

`dest` must be a buffer **registered** through the vis-buffer registration
seam; an unregistered address raises
`WaveSoundBuffer.getVisBuffer: getVisBuffer destination is not a registered sample buffer`.

The `getSample` plugin's `sampleValue` reads through this member: it is the
squared peak amplitude over the first `sampleCount` samples, normalized to
`0.0..=1.0` as `(max|sample| / 32768)²`.

---

## 8. `PhaseVocoder`

A phase vocoder is a **time/pitch filter**. Pushed onto a buffer's `filters`
array, it stretches or compresses the playback while the mixer plays the
*filtered* source.

| member | default | accepted |
|---|---|---|
| `window` | 4096 | the powers of two 64 … 32768 |
| `overlap` | 0 (auto) | 0, 2, 4, 8, 16, 32 |
| `pitch` | 1.0 | any real |
| `time` | 1.0 | any real |

A rejected `window` or `overlap` **leaves the old value in place** — the
reference throws before the assignment. The messages are the reference's
exact strings: `Invalid window size must be in 64 to 32768` and
`Invalid overlap count must be in 2 to 32`.

`interface` is the opaque filter token (the instance address as an integer);
it is read-only.

The DSP is a port of the reference pipeline: a Vorbis-I window, an
unnormalized Ooura-style radix-2 FFT, per-bin phase propagation, optional
frequency-axis resampling with bilinear interpolation, phase accumulation at
the output hop and overlap-add synthesis. The chain is **streaming**: each
filter pulls only the hop it needs.

The `.sli` loop links and labels are scaled by the chain's cumulative time
scale, so the loop points keep their musical position.

---

## 9. State ownership

| state | owner |
|---|---|
| the mixer, the live buffers, the next id, the last poll tick, the render-driven flag, the installed decoders, the vocoder configs, the registered vis buffers, the focus mode | the **per-engine dispatch state** (`CompatServices::state::<SoundState>()`) |
| the L2 `SoundBuffer`, the channel id, the source, the last decoded format, the raw owner handle, the retained action owner, the retained `filters` array, the cached `flags` / `labels` objects, the flags (`use_vis_buffer`, `frequency`, `paused`), the fade state, the pending event queue | the **per-object payload** |
| the playback state, the loop flags, the label cursor, the status | the **L2 sound model** |
| the decoding | the **L1 media crate**, behind the host-installed decoder |

The mixer's clock has two sources: the frame poll advances it, or the render
path advances it by the frames it produced. The host selects with
`set_sound_render_driven`.

The frame poll's own step comes from the **engine's clock**: the sound state
keeps no clock of its own, and the poll differences the engine's frame clock
against the previous poll. The fade beat follows the same step.

---

## 10. Errors

Every instance member runs through a shared prelude:

| shape | example |
|---|---|
| `"WaveSoundBuffer.<member>: compat context is not bound"` | the engine binding is missing |
| `"WaveSoundBuffer.<member>: missing instance"` | no instance payload |
| `"WaveSoundBuffer.<member>: not constructed"` | the constructor has not run |
| `"WaveSoundBuffer.<member>: {error}"` | an inner failure |

The arity messages are the reference's own shapes:
`WaveSoundBuffer.open: a storage name is required`,
`WaveSoundBuffer.fade(to, timeMs[, delayMs])`,
`WaveSoundBuffer.setPos(x, y, z)`,
`WaveSoundBuffer.getVisBuffer(dest, numsamples, channels[, ahead])`.

---

## 11. Tests

`crates/compat/tests/sound.rs` covers the constructor defaults, the
argument shapes, the arity errors, the read-only members, the return types,
the volume and pan clamps, the global volume and the focus mode, the
frequency's verbatim storage and its lack of playback effect, the position
and sample-position round trips, pausing, the status events delivered inside
the call, a script override winning over the native event member, the native
event members forwarding to the action owner, a label event dropped by a
status change, the labels dictionary, the flags' lifetime and reset, the
`WaveFlags` and `PhaseVocoder` surfaces, the vis buffer, the fade machine
(including the instant completion and the interrupted fade), a failed open
keeping the format, the teardown, the render-driven clock and the streaming
source.

`crates/compat/tests/dispatch_sound.rs` pins the receiver routing for the
family (`with` targets, subclass methods, static members), and
`crates/compat/tests/audio_render.rs` pins the L3↔L2↔L1 render seam.

Unit tests inside the module cover the foreign voice, the `.sli` parser (both
formats and the malformed cases), the float-to-i16 and downmix rounding, the
vis-buffer layout, and the phase vocoder's time stretch and pitch shift.

---

## 12. Known gaps

1. **`bits` reports `32`**, not the reference's 16: the L1 audio spec
   carries no bit depth, so the reference's 16-bit WAV value cannot be
   reproduced truthfully.
2. **`freeDirectSound` is a no-op** — there is no DirectSound device.
3. **The constructor tolerates a missing argument** where the reference
   raises a bad-parameter-count. The owner is then the reference's empty
   closure, so the event forwarders stay silent.
4. **The constructor returns an integer object id** in the result slot where
   the reference returns void. It is observable only through
   `super.WaveSoundBuffer(c)` inside a subclass.
5. **`totalTime` on an unloaded buffer returns `0`**; the reference divides
   by zero there.
6. **Only `PhaseVocoder` is supported as a filter.** A `filters` element
   without a resolvable `interface` is skipped; the reference's other
   filter implementations are absent.
7. **The phase vocoder's output gain is an approximation**, and the
   `overlap == 0` (auto) thresholds are empirical.
8. **`getVisBuffer` requires a registered destination.**
9. **`open` does not decode the whole file** — the decoder may stream, which
   matches the reference's decode thread.
10. **The `.sli` parser supports only the two documented formats**; a
    negative loop value is unrepresentable in the model and becomes a typed
    error.

---

## 13. Related documents

| topic | document |
|---|---|
| the L2 mixer and the sound model | [`../../code/engine/sound.md`](../../code/engine/sound.md) |
| the L3 layer and the frame body | [`../../arch/l3.md`](../../arch/l3.md) §6 |
| the disc-sound classes | [`cdda_midi.md`](cdda_midi.md) |
| the video overlay (which reuses the volume helpers) | [`video.md`](video.md) |
| the L3 ↔ L2 protocol | [`../../../docs.old/migration/14-l3-l2-protocol.md`](../../../docs.old/migration/14-l3-l2-protocol.md) |
