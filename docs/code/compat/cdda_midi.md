# `compat::natives::cdda_midi` — the `CDDASoundBuffer` and `MIDISoundBuffer` classes

Source: `crates/compat/src/natives/cdda_midi.rs` · layer **L3** · classes
`CDDASoundBuffer`, `MIDISoundBuffer`

These two classes exist because the reference declares them. Their playback
surface sits behind a feature macro — `ENABLE_CDDA` for the CD-DA class and
`TVP_ENABLE_MIDI` for the MIDI class — that is **defined nowhere in the
reference tree**, so the reference itself ships them disabled.

The port reproduces that **disabled build** exactly: the playback methods
perform their argument-count check and then do nothing, and the properties
report the disabled build's values.

---

## 1. Registered surface

Both classes register the same playback surface.

**Methods (8):** the constructor (`CDDASoundBuffer` / `MIDISoundBuffer`),
`open`, `play`, `stop`, `fade`, `stopFade`, `onStatusChanged`,
`onFadeCompleted`.

**Properties (7):**

* read/write (6): `position`, `paused`, `totalTime`, `looping`, `volume`,
  `volume2`;
* read-only (1): `status`.

`MIDISoundBuffer` adds one more member, `midiOut(data)`. It is registered as
an **objthis-only** member, so it is callable on the class object as well as
on an instance.

---

## 2. What the disabled build reports

| member | value |
|---|---|
| `position` | **void** |
| `paused` | **void** |
| `totalTime` | **void** |
| `looping` | **void** |
| `volume` | CD-DA: **void**; MIDI: `0` |
| `volume2` | `0` |
| `status` | `"unload"` |

The CD-DA / MIDI asymmetry on `volume` is **reference-exact**: the CD-DA
getter's whole body is compiled out, while the MIDI getter is not guarded and
calls the disabled `GetVolume()`, which returns `0`.

Every **setter** is inert: the value is accepted and discarded.

`status` is read-only — a write raises the VM's access-denied error.

---

## 3. The two live members

`onStatusChanged(status)` and `onFadeCompleted()` sit **outside** the feature
guards, so they work: they build the reference event dictionary and invoke
the constructor's action owner as `actionOwner.action(event)`, discarding the
result.

| member | event dictionary |
|---|---|
| `onStatusChanged(status)` | `{type:"onStatusChanged", target, status}` |
| `onFadeCompleted()` | `{type:"onFadeCompleted", target}` |

`type` is the **member name**, not a status string. The argument count is
checked only when an action owner exists: `onStatusChanged` needs one
argument, `onFadeCompleted` needs none. With no owner they are silent void
no-ops.

`status` never leaves `"unload"`, so the disabled build never posts a status
event on its own.

---

## 4. State ownership

| state | owner |
|---|---|
| the class it was created for and the retained action owner | the **per-object payload** (`DiscSoundInst`) |
| everything else | nothing |

No object owns a mixer channel, a decoder or a playback device. The module
names **no** L2 item at all: there is no mixer channel, no storage read and
no media decode behind these classes. The TVP volume scale is therefore
irrelevant here — the classes never produce a volume.

The owner is taken out of the payload across the VM call and restored
afterwards, so a re-entrant native sees consistent state.

---

## 5. Errors

The disc-sound family uses the **`disc sound.`** prefix rather than the class
name:

| shape | example |
|---|---|
| `"disc sound: compat context is not bound"` | the engine binding is missing |
| `"disc sound: missing instance"` | no instance payload |
| `"disc sound.<member>: missing instance"` | a member on an unconstructed object |
| `"disc sound.<member>: compat context is not bound"` | the engine binding is missing |
| `"disc sound.<member>: needs N argument(s)"` | `disc sound.onStatusChanged: needs 1 argument(s)` |
| `"disc sound.<member>: {error}"` | the action dispatch failed |

The two shared arity messages are **bare** (no prefix at all):
`open: a storage name or CD track is required` and
`fade(to, timeMs[, delayMs])`.

`MIDISoundBuffer.midiOut` raises `MIDISoundBuffer.midiOut(data)` with no
argument.

---

## 6. Tests

`crates/compat/tests/cdda_midi.rs` pins the disabled build's defaults per
class (including the `volume` asymmetry), the inert setters, the read-only
`status`, the no-op methods, the two arity messages, the `midiOut` no-op, the
event members forwarding to the construction action owner with the exact
`type` and `status` members, and the argument check being present only when
an owner is set.

---

## 7. Known gaps

1. **There is no CD-DA device and no MIDI output device.** The whole
   playback surface is the reference's *disabled build*. Nothing is ever
   decoded, played or emitted. This is a faithful emulation of the reference
   tree, not a port of a working feature.
2. **Every property but `volume2` and `status` reads void.**
3. **Every setter is inert.**
4. **The two classes share one instance payload type**; the payload is
   created by the matching create function, so a mismatch is impossible.

---

## 8. Related documents

| topic | document |
|---|---|
| the wave sound buffer family | [`sound.md`](sound.md) |
| the L3 layer | [`../../arch/l3.md`](../../arch/l3.md) |
| the error contract | [`../../rules/arch_rules.md`](../../rules/arch_rules.md) §6 |
