# `compat::natives::debug` — the `Debug`, `Console` and `Controller` classes

Source: `crates/compat/src/natives/debug.rs` · layer **L3** · classes `Debug`,
`Console`, `Controller`

`Debug` is the script's logging and diagnostics surface. `Console` and
`Controller` are the two singleton objects it exposes.

---

## 1. Registered surface

### `Debug` — 7 methods and 5 properties, all class-level

**Methods:** `message`, `notice`, `startLogToFile`, `logAsError`,
`addLoggingHandler`, `removeLoggingHandler`, `getLastLog`.

**Properties:** `logLocation` (read/write), `logToFileOnError` (read/write),
`clearLogFileOnError` (read/write), `controller` (read-only), `console`
(read-only).

### `Console` and `Controller`

Each is an **instance** class with a constructor and one read/write property,
`visible`. They are registered as **global classes**, so a script may write
`new Console()` even though the reference exposes them only through
`Debug.console` and `Debug.controller`.

Seven further members on `Console` — `restoreMaximize`, `maximize`,
`getRect`, `setPos`, `getPlacement`, `setPlacement`, `bringAfter` — come from
the **windowEx plugin**, not from this module, and their bodies are inert.

---

## 2. The log

`message(arg, …)` joins its arguments with `", "` into one line and then:

* pushes it into the ring buffer;
* appends it to the log file when one is open;
* dispatches it to every registered logging handler;
* writes it to the process log at `info`.

`notice(arg, …)` does the same, and additionally accumulates the line into
the "important log", which `startLogToFile` flushes into the file. It writes
to the process log at `debug`.

The ring buffer holds `TVP_LOG_MAX_LINES = 2048` entries, and the read
window is `TVPLogMaxLines + 100 = 2148`. When the buffer reaches the window
it drains the oldest 100 entries in one go, so it holds between 2048 and 2147
entries.

`getLastLog([lines])` renders the entries as `"{HH:MM:SS} {line}\r\n"`,
oldest first, including a trailing `\r\n` after the last entry. Without an
argument the window is 2148; with one it is truncated to a `tjs_uint`. A
count of zero yields the empty string.

The timestamp is **UTC**; the reference uses local time. See §7.

---

## 3. The log file

`startLogToFile([clear])` writes to **`{logLocation}/krkr.console.log`** —
there is no filename argument. An optional boolean `clear` (default false)
truncates instead of appending. The call is **ignored while logging is
already on**, and the reference's early return happens *before* the clear,
so a second call does not truncate either.

After opening, the file receives a separator, a header line
(`"Logging to <file> started on <asctime>"`), the accumulated important log,
a dashed separator and the last 100 buffered lines.

`logAsError()` starts the error log file only if `logToFileOnError` is set,
using `clearLogFileOnError` as the clear flag.

The file is written through the **L1 `fs` crate** to the host filesystem — it
deliberately bypasses the game's storage namespace.

`logLocation` is normalized on assignment (lowercased, `\` folded to `/`, the
in-archive part normalized), and **any name containing `>` clears the
location**: an in-archive name has no local form. The setter then reopens the
stream and honours the `-forcelog` command-line option (`yes` starts logging,
`clear` truncates first).

---

## 4. The logging handlers

`addLoggingHandler(fn)` requires an object. A TJS `null` is accepted
**silently** and stored but never invoked — the reference's null is an empty
closure. Any other non-object raises. A **duplicate registration of the same
`(object handle, objthis)` pair is ignored**.

`removeLoggingHandler(fn)` removes exactly the handler with that identity.

A handler receives `" " + line` — with the reference's leading space — as
its single argument.

---

## 5. The singletons

`Debug.console` and `Debug.controller` return a **lazily created, cached**
object: the same object on every read, and the two are different objects.

`visible` defaults to false. The flag lives in the shared per-engine state,
so `Debug.console.visible` and a script-created `new Console().visible`
observe the **same** value.

The reference registers `visible` as a *static* property on a class object
and its getter returns `0` unconditionally; the port registers an instance
class and stores a real flag, so `Debug.controller.visible` can become true.

---

## 6. State ownership

| state | owner |
|---|---|
| the log file handle, the logging flag, the failed-open flag, the ring buffer, the important log, the location, the two error flags, the handlers, the two singletons, the two visibility flags | the **per-engine dispatch state** (`CompatServices::state::<DebugState>()`) |
| a placeholder payload | the **per-object payload** (`DebugWindowState`) |
| the log file itself | the **L1 `fs` crate**, on the host filesystem |

The `Console`/`Controller` payloads carry nothing: all state is in the
per-engine dispatch state, which is why the singletons and a script-created
instance share their flags.

There is no L2 seam.

---

## 7. Errors

| shape | example |
|---|---|
| `"Debug.<member> requires 1 argument"` | `Debug.message requires 1 argument` |
| `"Debug.<member>: compat context is not bound"` | the engine binding is missing |
| `"Debug.<member>: argument is not an object"` | a non-object logging handler |
| `"Debug.<member>: cannot retain the handler: {error}"` | the handler could not be retained |
| `"Debug.<member>: missing value"` | a setter without a value slot |
| `"Debug.console: unexpected scalar result"` / `"Debug.controller: unexpected scalar result"` | the singleton construction returned a scalar |
| `"Debug.console: {error}"` / `"Debug.controller: {error}"` | the singleton construction failed |
| `"Console.visible: compat context is not bound"` / `": missing value"` | the visibility property |
| `"Controller.visible: compat context is not bound"` / `": missing value"` | the same |

A failed log-file open is **logged**, not raised:
`Debug: failed to open "<file>": <error>`. The failure is remembered, so the
file is not retried until the location changes.

---

## 8. Tests

`crates/compat/tests/debug.rs` covers the ring buffer and the `getLastLog`
window and eviction, the multi-argument joining, the logging-handler
contract (the leading space, the removal, the ignored duplicate), the
non-object-raises / null-accepted rule, the property round trips, the
`logLocation` archive-name clearing and the `-forcelog` option, the
`startLogToFile` append semantics, the no-location case, and the
`Console`/`Controller` singletons and their visibility flags.

---

## 9. Known gaps

1. **The timestamps are UTC**; the reference uses local time. The test
   asserts the format with a ±2 s UTC window.
2. **The reference's UTF-16 BOM is omitted** — the log stream is UTF-8.
3. **`Console.visible` and `Controller.visible` are no-ops** (the
   reference's UI flag is inert too).
4. **`Console` and `Controller` are registered as global classes**, so
   `new Console()` works where the reference only exposes them through
   `Debug.console` / `Debug.controller`.
5. **The "locally accessible" test is approximated** by "the name contains
   `>`", standing in for the reference's `TVPGetLocallyAccessibleName`.
6. **`Console` carries seven extra members from the windowEx plugin**; they
   are not part of this module and their bodies are inert.
7. **The log file bypasses the game storage namespace.**
8. **`message` and `notice` also write to the process log** — a side effect
   the reference does not have, since it has a separate console window.

---

## 10. Related documents

| topic | document |
|---|---|
| the L3 layer | [`../../arch/l3.md`](../../arch/l3.md) |
| the error contract and the host environment | [`../../rules/arch_rules.md`](../../rules/arch_rules.md) §6, §7 |
| the plugin model and an unported plugin | [`../../arch/l3.md`](../../arch/l3.md) §5 |
