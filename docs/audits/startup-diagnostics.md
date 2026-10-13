# Startup script failure diagnostics

Oracle: `reference/cpp/**`. Scope: what a failing storage-backed script logs,
so a device failure can be diagnosed from the log alone.

Motivation: a real startup failure prints

```
ERROR cannot open the game: startup: system/Initialize.tjs: TJS error: Initialize.tjs(579): script exception : Config.tjs が見つかりません。
```

The line names the file and the source line but not the code, so diagnosing it
required inferring the script's behaviour from storage probes, archive listings
and plugin links. This diagnostic prints the source around the failure at the
moment it fails, in the same spirit as the `script read` line that found the
bytecode bug: a failure must not hide its own evidence.

## What is logged and when

For `Scripts.execStorage` / `Scripts.evalStorage`, and the startup path through
`compat::exec_storage` (which all route through `execute_storage`), when the
decoded script fails:

* **Text script** — the engine returns the script error after the bytes
  decoded as text. The diagnostic reports the error and a numbered source
  excerpt.
* **Precompiled bytecode** — the loader recognized the bytes and rejected them
  or their execution raised. There is no text source, so the diagnostic names
  the bytecode path instead of decoding binary data as text.

Both fire only at `debug` level. Source is never printed for a script that
succeeds. Nothing is emitted when debug logging is off, and no work is added
on the success path.

## Format

One `debug` record per failure. The first line repeats the error; the header
names the line range; each source line is prefixed with its 1-based line
number.

Text script (error names line 579 of 900; the cap binds):

```
script failure 'system/Initialize.tjs': TJS error: Initialize.tjs(579): script exception : Config.tjs が見つかりません。
source lines 519-639 of 900:
   519| // ...
   520| // ...
   ...
   579| throw new Exception("Config.tjs が見つかりません。");
   ...
   639| // ...
... excerpt truncated at 8192 bytes
... 779 more line(s) not shown
```

When the error names no source line (for example a storage or decode failure
after a line-less error), the head of the script is printed instead:

```
script failure 'data/foo.tjs': TJS error: ...
source lines 1-40 of 120:
     1| // ...
   ...
    40| // ...
... 80 more line(s) not shown
```

Precompiled bytecode failure:

```
script failure 'startup.tjs': precompiled bytecode, no source to show: TJS error: startup.tjs(1): ...
```

### The reported line is always present

`error_source_line` reads the `<block>(<line>): <message>` prefix the VM's
error conversion produces (`crates/tjs/cpp/tjs2_abi.cpp`, `make_error_message`).
When a line is present, the excerpt **always contains that line**: it starts at
the reported line and grows outward one line at a time on each side until the
byte cap is reached. Lines are never taken from the start of a window that
could cut the failure off, so the reported line is always present, even when
the excerpt is truncated — that is the guarantee this diagnostic exists to
make. If the reported line alone exceeds the cap, it is shown truncated but
its line number still appears.

When it names no line, or the named line is outside the script, the excerpt is
the first 40 lines (`FAILURE_HEAD_LINES`).

## Bound

The excerpt text is capped at `FAILURE_SOURCE_BYTES` = 8192 bytes. When the cap
binds, the message states `excerpt truncated at 8192 bytes`; the number of
lines never printed is appended as `... N more line(s) not shown`. The header
and the error string are bounded separately (the VM caps its own trace text),
so one failure cannot flood the log. Because the reported line is placed
first, a single oversized line can only truncate itself, never the line the
error names.

## Why failure-only and `debug`

What a failing script was doing is diagnostic evidence, not a trace of normal
operation. Printing source for every `execStorage` would flood a device log
during normal startup, so it is gated behind `debug` — the level the user turns
on deliberately when diagnosing. The existing `script read` line stays the
`debug` identifier of the raw bytes; the new line is its failure companion.

## What this deliberately does not do

* **No source on success.** A script that runs cleanly emits no source; the
  only line is the existing `script read` diagnostic.
* **No full-script dump.** The excerpt grows from the reported line only as
  far as the byte cap allows; a head of at most 40 lines covers a line-less
  error.
* **No change to the `script read` line.** Its name, field order, hex head,
  encoding and NUL counts are untouched; the device workflow greps it.
* **No re-read of storage.** The decoded text is already in hand at the point
  the engine returns the error, so the diagnostic reuses it.
* **No new failure.** Building the diagnostic cannot panic and cannot change
  the returned error; a script failure stays the same failure.
* **No bytes-as-text for bytecode.** A failing precompiled script logs that it
  is bytecode, not a decoded excerpt.

## Evidence and scope

`crates/compat/tests/scripts.rs` drives the real `Scripts.execStorage` surface
over the L2 storage and captures the `debug` log:

| Test | Asserts |
|---|---|
| `failing_storage_script_logs_its_source_window` | a failing text script logs the error and a numbered window around the reported line; cites `reference/cpp/core/base/ScriptMgnIntf.cpp:713-811` |
| `successful_storage_script_logs_no_source` | a succeeding script logs no `script failure` / source, and the `script read` fields are unchanged; cites `ScriptMgnIntf.cpp:713-811` |
| `failure_source_excerpt_keeps_the_failing_line_when_capped` | a script whose lines exceed the cap keeps the reported line (line 60) while the earliest lines are cut, and states `excerpt truncated at 8192 bytes` and the omitted count |
| `failing_bytecode_script_logs_no_source_excerpt` | a failing precompiled script logs `precompiled bytecode, no source to show` and no `source lines`; cites `ScriptMgnIntf.cpp:713-727` |

This is host-level evidence about the diagnostic's formatting and gating. It is
not evidence that the two failing titles now start, nor that every VM error
carries a parseable `<block>(<line>)` prefix — a line-less error falls back to
the head, which the format covers.

The pure formatting helpers also have unit tests in
`crates/compat/src/natives/scripts.rs`: `error_source_line_reads_the_reported_line`
checks the `<block>(<line>):` parse, and
`failure_message_uses_the_head_without_a_reported_line` checks the head
fallback and the omitted-line note.
