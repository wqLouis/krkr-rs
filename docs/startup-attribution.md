# Startup attribution

Startup on a real device is dominated by the entry scripts, not by mounting
or decoding: a measured 692 MB mount took 39 ms and 36 images decoded in
170 ms, while the single `startup.tjs` execution took 20.8 s. Two pieces of
instrumentation make that time attributable.

## Per-script timing

`Scripts.exec` / `eval` / `execStorage` / `evalStorage` / `compileStorage`
and the composition root's `compat::exec_storage` log one `debug` line per
call:

```
DEBUG startup: script <name> in <ms> ms
```

The entry script runs at depth 0 and every nested `Scripts.exec*` / `eval*`
call runs one deeper; the depth is shown as two spaces of indentation, so a
nested script's line is visibly inside its caller's.

## Per-native aggregate

While the startup window is open, the instrumented natives accumulate their
dispatch time in `natives::NativeTiming` (host dispatch state, never a
process global). `CompatHost::end_native_timing` emits one line, the top ten
members by total milliseconds with call counts:

```
DEBUG|INFO startup: native time (top 10): <name>=<ms>ms/<calls>; ...
```

The line is `info` when any single member reached 50 ms and `debug`
otherwise. Formatting and allocation happen only when the line is emitted.
Because a script-executing native brackets the scripts it runs, a nested
script is counted in every enclosing native as well, so the totals overlap;
the per-script lines give the exclusive cost.

## Coverage and extension

Only the `Scripts` family is instrumented today. Every other native adds one
line at the top of its callback:

```rust
let _timing = crate::natives::NativeTimer::start(services, "Font.mapPrerenderedFont");
```

The native needs `services: &CompatServices`; a native that resolves the
binding through `bound(engine)` already has it.

## Cost

Outside the window `NativeTimer::start` pays one dispatch-state lookup and
one `Cell` read; the frame hot path is not instrumented. Inside the window an
instrumented call pays one `Instant` pair and one linear scan over the
recorded members. Per-script timing is gated on `debug` logging being
enabled, so a release build pays nothing for it.
