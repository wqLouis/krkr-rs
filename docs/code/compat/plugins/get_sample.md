# getSample (`getSample.dll`)

**DLL name:** `getSample.dll` — linked with `Plugins.link("getSample.dll")`
**Rust module:** `crates/compat/src/natives/plugins/get_sample/`
**Reference:** `reference/cpp/plugins/getSample.cpp`
**Registers:** members on the core `WaveSoundBuffer` class
**Registered:** on the first `Plugins.link("getSample.dll")`, not at startup

## Surface

The reference attaches two groups of members to the already-registered
`WaveSoundBuffer` class:

* the legacy `getSample(n)` function (`NCB_ATTACH_FUNCTION`);
* the new-style `WaveSoundBufferAdd` class (`NCB_ATTACH_CLASS_WITH_HOOK`),
  which contributes the `sampleValue` read-only property and the
  `sampleCount` / `sampleAhead` read-write properties, plus the
  `setDefaultCounts` / `setDefaultAheads` functions.

The compat VM has no subclassing, so the `WaveSoundBufferAdd` members are
attached to `WaveSoundBuffer` itself:

| member | kind | behaviour |
|---|---|---|
| `getSample([n])` | method | the mean of the current playback samples; `n` defaults to `100` |
| `sampleValue` | property (read-only) | the squared peak amplitude of the current samples, in `0.0..=1.0` |
| `sampleCount` | property (read/write) | how many samples `sampleValue` reads |
| `sampleAhead` | property (read/write) | the `ahead` argument passed to `getVisBuffer` |
| `setDefaultCounts(count)` | method | the default `sampleCount` for objects touched afterwards |
| `setDefaultAheads(ahead)` | method | the default `sampleAhead` for objects touched afterwards |
| `finalize()` | method | drops the per-object state |

## When the members appear

The members are published when a script calls `Plugins.link("getSample.dll")`,
**not** when the engine starts. `getSample.dll` is not in the startup surface
(`natives::plugins::STARTUP_PLUGINS` holds only `xp3filter.dll`), so before
the link the members do not exist and a probe of one reports it absent.

The timing is **script-observable**, and the shipped `system/voice.tjs`
depends on it. Its compiled top level publishes the
`WaveSoundBuffer.enableGetSample` flag **only after** it has linked the
module:

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

Its `onFlip` timer action then reads the flag before it reads the plugin's
`sampleValue`. Registering the surface at startup would make the probe
`typeof WaveSoundBuffer.setDefaultAheads` return a type instead of
`"undefined"`, so the script would skip the whole block and leave the flag
unset — the reference does not have that failure, and neither must the port:
the `onFlip` action would raise `Member "enableGetSample" does not exist` on
every fire.

Two tests pin this:

* `crates/compat/tests/get_sample.rs` — the members do **not** exist before
  the link, and the harness links the module before it exercises them.
* `crates/compat/tests/timer_behavior.rs` — the shipped `voice.tjs` path:
  the flag is published only after the link, and the timer's action reads it
  without raising.

## Behaviour

### `getSample([n])`

The legacy function allocates a buffer of `n` 16-bit samples, calls the
object's own `getVisBuffer` into it, and returns the **mean of the
non-negative samples**. A negative sample means "no sample" and is skipped.
With no argument it uses `100`. A non-positive `n` yields `void`.

The reference reaches the sample buffer through
`tTJSVariant buffer = (tTVInteger)buf` — the buffer address as an integer.
The port does the same: it registers the caller-owned destination with the
sound module's `VisBufferRegistration` for the duration of the call, so
`getVisBuffer` accepts the address and clamps its write to the buffer length.

### `sampleValue`

`sampleValue` zeroes a `sampleCount`-sample buffer, calls
`getVisBuffer(buf, counts, 1, aheads)` and returns the **squared peak
amplitude** in `0.0..=1.0`, i.e. `(max|sample| / 32768)²`.

The reported sample count is clamped the way the reference does:
`cnt = (cnt > counts || cnt < 0) ? counts : cnt`. A `getVisBuffer` failure is
logged and the computation continues with the zeroed buffer, matching the
reference.

### `sampleCount` / `sampleAhead`

`sampleCount` is how many samples `sampleValue` reads; `sampleAhead` is the
`ahead` argument handed to `getVisBuffer`. Both are per object.

`setDefaultCounts` / `setDefaultAheads` change the defaults applied to objects
**created afterwards** — in practice, to objects that have not been touched by
any of these members yet.

### The `useVisBuffer` side effect

The reference's constructor writes `objthis.useVisBuffer = 1`. The port
performs the same write the first time one of the members touches an object,
and logs a warning and continues when the write fails (an object that is not
a live `WaveSoundBuffer`, for example the class object itself, simply has no
visualization buffer to enable).

## State and lifetime

* The reference keeps the per-object `counts` / `aheads` (and the reusable
  sample buffer) in a native instance attached to the `WaveSoundBuffer`
  object by `NCB_GET_INSTANCE_HOOK`. The compat ABI's
  `register_instance_members` callbacks receive only `objthis` (no native
  payload), so the equivalent per-object state lives in a per-engine map
  keyed by the raw `objthis` pointer — the same object-identity keying
  `crates/compat/src/natives/sound.rs` uses for its `SoundInst` /
  `SoundObject` state.
* The entry is created when a member first uses the object and removed by
  `finalize` (the reference's destructor), so a destroyed object leaves no
  entry behind.
* The per-engine defaults (`default_counts` = `100`, `default_aheads` = `0`)
  live in the same dispatch state.

`getVisBuffer` itself is the L2 playback state's own method: the plugin never
reaches into the `sound` module, it invokes the object's script-visible
method exactly like the reference's `objthis->FuncCall(0, "getVisBuffer", …)`.

## Errors

| condition | outcome |
|---|---|
| no argument to `setDefaultCounts` / `setDefaultAheads` | typed error — the reference's `doInvoke` wrapper raises `TJS_E_BADPARAMCOUNT` for a short argument list (`reference/cpp/core/plugin/ncbind.hpp:1178`) |
| `getVisBuffer` raises during `getSample` | the error is reported as `WaveSoundBuffer.getSample: <error>` |
| `getVisBuffer` raises during `sampleValue` | the failure is logged and the value is computed from the zeroed buffer, as in the reference |
| the compat context is not bound | typed error |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/get_sample.rs` | the `getSample` mean over the non-negative samples, the `sampleValue` squared peak, the `sampleCount` / `sampleAhead` round trip, the default setters, the `useVisBuffer` write, and the `finalize` cleanup |
| `crates/compat/tests/sound.rs` | the core `WaveSoundBuffer` behaviour the plugin builds on (`getVisBuffer`, the visualization buffer) |
| `crates/compat/tests/timer_behavior.rs` | the link-time publication: the members are absent before `Plugins.link`, and the shipped game's `voice.tjs` publishes `WaveSoundBuffer.enableGetSample` only after the link |

## Known gaps

* The reference's `WaveSoundBufferAdd` is a separate native class with its
  own instance payload; the port attaches its members to `WaveSoundBuffer`
  because the compat ABI has no subclassing. A script sees the same members
  on the same object.
* The reference's reusable sample buffer is a single allocation reused
  across calls; the port allocates a fresh buffer per call. The observable
  values are identical; only the allocation pattern differs.
