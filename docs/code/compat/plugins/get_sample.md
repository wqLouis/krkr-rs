# getSample (`getSample.dll`)

**DLL name:** `getSample.dll` — linked with `Plugins.link("getSample.dll")`
**Rust module:** `crates/compat/src/natives/plugins/get_sample/`
**Reference:** `reference/cpp/plugins/getSample.cpp`
**Registers:** members on the core `WaveSoundBuffer` class — the plugin
publishes no class and no global of its own

## Surface

The reference attaches two groups of members to the `WaveSoundBuffer` class
the core already registers:

* `NCB_ATTACH_FUNCTION` puts `getSample`
  (`reference/cpp/plugins/getSample.cpp:45`), `setDefaultCounts` and
  `setDefaultAheads` (`reference/cpp/plugins/getSample.cpp:178-181`) on the
  class object;
* `NCB_ATTACH_CLASS_WITH_HOOK` puts the `sampleValue`, `sampleCount` and
  `sampleAhead` properties on the same class object
  (`reference/cpp/plugins/getSample.cpp:171-177`). The helper class behind
  that macro, `WaveSoundBufferAdd`, is a C++ class with a native instance
  payload — it is never published as a TJS class, so the parity report
  counts its three members under `WaveSoundBuffer`
  (`reference/cpp/core/plugin/ncbind.hpp:2235-2237`).

The port registers all of them as members of the native class
`WaveSoundBuffer`, so a script reaches them through the class object
(`WaveSoundBuffer.setDefaultCounts(7)`) and through an instance
(`s.sampleCount`) alike.

| member | kind | behaviour |
|---|---|---|
| `getSample` | method | the mean of the current playback samples; with no argument it reads `100` samples, a non-positive count leaves the result `void` |
| `sampleValue` | property (read-only) | the squared peak amplitude of the current samples, in `0.0..=1.0` |
| `sampleCount` | property (read/write) | how many samples `sampleValue` reads |
| `sampleAhead` | property (read/write) | the `ahead` offset handed to `getVisBuffer` |
| `setDefaultCounts` | method (class member) | the default `sampleCount` for the objects the plugin touches afterwards |
| `setDefaultAheads` | method (class member) | the default `sampleAhead` for the objects the plugin touches afterwards |
| `finalize` | method (lifecycle hook) | drops the per-object state; the VM calls it while the object is finalized |

`finalize` is the port's teardown hook. The reference `WaveSoundBuffer`
carries an **empty** `finalize` from the core
(`reference/cpp/core/sound/WaveIntf.cpp:1096`,
`reference/cpp/core/tjs2/tjsNative.h:374-376`), and the plugin's `finalize`
takes that slot: it releases the per-object parameters. The parity report
counts six reference members on `WaveSoundBuffer` and matches all six; the
`finalize` hook is ignored on both sides
([`docs/plugin_parity.md`](../../../plugin_parity.md)).

### When the members appear

The members are published by the **first** `Plugins.link("getSample.dll")`,
not by the engine startup: `getSample.dll` is not in the startup list
(`natives::plugins::STARTUP_PLUGINS` holds only `xp3filter.dll`), so before
the link a probe of a member reports it absent. The timing is
script-observable, and the shipped `system/voice.tjs` depends on it: its
compiled top level publishes `WaveSoundBuffer.enableGetSample` only after it
has linked the module.

```tjs
if (typeof WaveSoundBuffer.setDefaultAheads == "undefined") {
    WaveSoundBuffer.enableGetSample = 0;
    if (CanLoadPlugin("getSample.dll")) {
        try {
            Plugins.link("getSample.dll");
            WaveSoundBuffer.setDefaultAheads(0);
            WaveSoundBuffer.enableGetSample = 1;
        } catch (e) {
            System.inform("getSample.dll load failed: " + e.message);
        }
    }
}
```

Its `onFlip` timer action reads that flag before it reads the plugin's
`sampleValue`, so a port that registered the surface at startup would leave
the flag unset and the action would raise
`Member "enableGetSample" does not exist` on every fire. The link is
idempotent, and `Plugins.unlink` only forgets the link: the ABI cannot
remove a member, so the surface stays on the class
([`../plugin.md`](../plugin.md)).

A `WaveSoundBuffer` constructed before the link does not carry the members:
a native class copies its members onto every instance when the instance is
constructed (`reference/cpp/core/tjs2/tjsNative.cpp:305-378`).

## Behaviour

### `getSample([n])`

The legacy function allocates a buffer of `n` 16-bit samples, calls the
object's own `getVisBuffer` into it and returns the **mean of the
non-negative samples**; a negative sample counts as "no sample" and is
skipped (`reference/cpp/plugins/getSample.cpp:15-43`). With no argument it
uses `100` (`reference/cpp/plugins/getSample.cpp:18`), and a non-positive
`n` leaves the result `void` because the reference fills the result only
when `n > 0` (`reference/cpp/plugins/getSample.cpp:19`).

The reference hands the buffer to `getVisBuffer` as an integer address
(`tTJSVariant buffer = (tTVInteger)buf`,
`reference/cpp/plugins/getSample.cpp:22`). The port does the same: it
registers the caller-owned destination with the sound module's
`VisBufferRegistration` for the duration of the call, so `getVisBuffer`
accepts the address and clamps its write to the buffer length.

### `sampleValue`

`sampleValue` zeroes a `sampleCount`-sample buffer, calls
`getVisBuffer(buf, counts, 1, aheads)` and returns the **squared peak
amplitude** in `0.0..=1.0`, i.e. `(max|sample| / 32768)²`
(`reference/cpp/plugins/getSample.cpp:103-125`). The reported sample count
is clamped the way the reference does — `cnt = (cnt > counts || cnt < 0) ?
counts : cnt` (`reference/cpp/plugins/getSample.cpp:112-114`) — so a
malformed result cannot read past the end of the buffer.

### `sampleCount` and `sampleAhead`

`sampleCount` is how many samples `sampleValue` reads; `sampleAhead` is the
`ahead` argument handed to `getVisBuffer`. Both are per object, and the
reference's `setSampleCount` reallocates the object's sample buffer
(`reference/cpp/plugins/getSample.cpp:132-138`).

`setDefaultCounts` and `setDefaultAheads` change the static defaults
(`reference/cpp/plugins/getSample.cpp:147-148,151-152`). An object reads
them when the plugin first touches it — the reference's instance getter
creates the `WaveSoundBufferAdd` with the defaults current at that moment
(`reference/cpp/plugins/getSample.cpp:74,155-165`) — so an object the
plugin has already touched keeps the values it was seeded with, and only an
object touched afterwards sees the new defaults. The two functions are
class members, so an instance reaches them too.

Both setters coerce their argument through TJS's integer conversion, which
truncates a real toward zero
(`reference/cpp/core/tjs2/tjsVariant.h:933`).

### The `useVisBuffer` side effect

The reference's `WaveSoundBufferAdd` constructor writes
`objthis.useVisBuffer = 1` (`reference/cpp/plugins/getSample.cpp:76-82`),
and the instance is created on the object's first member access
(`reference/cpp/plugins/getSample.cpp:155-165`). The port performs the same
write the first time one of the members touches an object, and logs a
warning and continues when the write fails — an object that is not a live
`WaveSoundBuffer` (the class object itself, for instance) simply has no
visualization buffer to enable.

The legacy `getSample` does not take part in this: it has no per-object
state and writes nothing.

### Reaching `getVisBuffer` through the object

Both sample paths call `getVisBuffer` **through the object**
(`objthis->FuncCall(0, "getVisBuffer", …)`,
`reference/cpp/plugins/getSample.cpp:26` and
`reference/cpp/plugins/getSample.cpp:106`), so a script member installed on
the instance wins over the class's own method and its return value feeds
the reductions. The legacy path passes three arguments (buffer, count,
channel `1`), the new-style path four (the fourth is `sampleAhead`).

## State and lifetime

* The reference keeps the per-object `counts` / `aheads` (and the reusable
  sample buffer) in a native instance attached to the `WaveSoundBuffer`
  object by `NCB_GET_INSTANCE_HOOK`
  (`reference/cpp/plugins/getSample.cpp:155-167`). The compat ABI's
  `register_instance_members` callbacks receive only `objthis` (no native
  payload), so the equivalent per-object state lives in a per-engine map
  keyed by the raw `objthis` pointer — the same object-identity keying
  `crates/compat/src/natives/sound.rs` uses for its `SoundInst` /
  `SoundObject` state.
* The entry is created when a member first uses the object and removed by
  `finalize`, so a finalized object leaves no entry behind. The VM calls a
  `finalize` member while it invalidates an object, before the native
  instances go away (`reference/cpp/core/tjs2/tjsObject.cpp:389-401`).
* The per-engine defaults (`sampleCount` = `100`, `sampleAhead` = `0`,
  `reference/cpp/plugins/getSample.cpp:151-152`) live in the same dispatch
  state, next to the per-object map.
* `getVisBuffer` itself is the L2 playback state's own method
  ([`../sound.md`](../sound.md)): the plugin never reaches into the sound
  module, it invokes the object's script-visible method exactly like the
  reference does.

## Errors

| condition | outcome |
|---|---|
| `setDefaultCounts` / `setDefaultAheads` without an argument | typed error — the reference's `NCB_ATTACH_FUNCTION` wraps the C++ function in ncbind's `doInvoke`, which rejects a short argument list with `TJS_E_BADPARAMCOUNT` (`reference/cpp/core/plugin/ncbind.hpp:1178`, rendered as "Invalid argument count", `reference/cpp/core/tjs2/tjsError.h:268`) |
| `getVisBuffer` raises during `getSample` | the error is reported as `WaveSoundBuffer.getSample: <error>`; the reference propagates it too (`reference/cpp/plugins/getSample.cpp:26-28`) |
| `getVisBuffer` raises during `sampleValue` | the failure is logged and the value is computed from the zeroed buffer, as in the reference (`reference/cpp/plugins/getSample.cpp:109-110`) |
| the `useVisBuffer = 1` write fails | a warning is logged and the call continues (`reference/cpp/plugins/getSample.cpp:81-82`) |
| the compat context is not bound | a typed error naming the member |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/get_sample.rs` | the `sampleCount` / `sampleAhead` defaults, their per-object round trip, the default setters reaching only later objects, `sampleValue` as a real plus the implicit `useVisBuffer` write, the legacy `getSample` mean and its `void` result for a non-positive count, the argument truncation, the missing-argument error, the `finalize` re-seed, the class-object error, and the `getVisBuffer`-through-the-object dispatch |
| `crates/compat/tests/sound.rs` | the core `getVisBuffer` the plugin drives — its argument count, the `0` on an idle buffer, and the live peak while playing, where `sampleValue` reads the fixture tone's `(16384/32768)²` |
| `crates/compat/tests/timer_behavior.rs` | the link-time publication: the members are absent before `Plugins.link("getSample.dll")`, the shipped `voice.tjs` publishes `WaveSoundBuffer.enableGetSample` after the link, and its `onFlip` action reads the flag and the plugin's `sampleValue` without raising |
| `crates/compat/tests/dispatch_fmt.rs` | the receiver-sensitive dispatch of the objthis members: `with(target) { .sampleCount }` reads the target's per-object state |
| `crates/compat/tests/plugin.rs` | that `getSample.dll` resolves to the built-in table and not to the null plugin |
| `crates/compat/src/natives/plugins/get_sample/mod.rs` (unit tests) | the two reference reductions over synthetic buffers: the mean that skips negative samples, and the squared normalized peak |

## Known gaps

* The reference's `WaveSoundBufferAdd` is a separate C++ class with its own
  native instance payload; the port keeps the equivalent state in a
  per-engine map keyed by the raw `objthis` pointer. The script-visible
  members are the same, on the same object.
* The reference reuses one sample buffer per object and reallocates it in
  `setSampleCount` (`reference/cpp/plugins/getSample.cpp:132-138`); the
  port allocates a fresh buffer per call. The observable values are
  identical; only the allocation pattern differs.
* A negative `sampleCount` is accepted by the port, and the reduction then
  yields `0.0`. The reference reallocates its buffer with
  `new short[counts]`, which raises a C++ allocation error for a negative
  count.
* The legacy `getSample` allocates its buffer with `malloc` and leaves the
  result `void` when the allocation fails
  (`reference/cpp/plugins/getSample.cpp:20-21`); the port's allocation
  aborts the process on an allocation failure instead.
