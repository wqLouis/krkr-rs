# `compat::natives::system` — the `System` class

Source: `crates/compat/src/natives/system.rs` · layer **L3** · class `System`

`System` is the script's window onto the host: paths, the environment, the
command line, the clock, the keyboard, the window-system facts and the
lifetime of the process. Almost every member is a **host fact** — a value the
engine has no other way to know.

---

## 1. Registered surface

### Methods (23, all static)

| member | behaviour |
|---|---|
| `inform(text[, caption[, buttons]])` | logs the message; returns void without a button count, `0` for one or two buttons, `-1` otherwise |
| `getTickCount()` | milliseconds since the first call |
| `getArgument(name)` | the `name=value` argument, or void |
| `setArgument(name, value)` | rewrites or inserts an argument |
| `createAppLock(name)` | always true — the single-process emulator always wins the lock |
| `exit([code])` | records an exit request for the host |
| `terminate([code])` | the same |
| `dumpHeap()` | a no-op (as in the reference) |
| `nullpo()` | logs an error instead of crashing the process |
| `showVersion()` | logs the version |
| `inputString(caption, prompt, initial)` | logs the prompt and returns the initial value |
| `toActualColor(color)` | maps a system colour to `0xRRGGBB` |
| `createUUID()` | an RFC-4122 v4 string |
| `assignMessage(id, message)` | overrides a TJS message; returns whether one existed |
| `doCompact([level])` | garbage-collects and/or clears the graphic cache |
| `readRegValue(key)` | reads a key out of `RegisterData.tjs`; void on a miss |
| `touchImages(storages[, limit[, timeout]])` | validates its arguments; the pre-caching itself is not done |
| `addContinuousHandler(fn)` | registers a per-frame callback |
| `removeContinuousHandler(fn)` | removes it |
| `getKeyState(code[, getcurrent])` | the state of a virtual key |
| `shellExecute(target[, param])` | returns true without launching anything |
| `system(target)` | returns 0 without running anything |
| `clearGraphicCache()` | drops the decoded-image cache and the unreferenced bitmaps |

### Properties

Read/write: `title`, `eventDisabled`, `graphicCacheLimit`,
`exitOnWindowClose`, `exitOnNoWindowStartup`, `drawThreadNum`.

Read-only: `exePath`, `dataPath`, `personalPath`, `savedGamesPath`, `exeName`,
`screenWidth`, `screenHeight`, `versionString`, `versionInformation`,
`platformName`, `osName`, `processorNum`, `exeBits`, `osBits`, `appDataPath`,
`desktopLeft`, `desktopTop`, `desktopWidth`, `desktopHeight`, `touchDevice`.

Three further members are **default data members** on the class object rather
than native properties: `System.exceptionHandler`, `System.onActivate` and
`System.onDeactivate` each read `null` until a script assigns one. They exist
so a `MEMBERMUSTEXIST` lookup succeeds.

Fifteen more members are attached to the same class object by **plugins**:
the windowEx plugin adds `getDisplayMonitors`, `getMonitorInfo`,
`getCursorPos`, `setCursorPos`, `getSystemMetrics`, `readEnvValue`,
`expandEnvString`, `setApplicationIcon`, `setIconicPreview`,
`getDoubleClickTime`, `breathe`, `isBreathing`, `getAboutString` and
`getCPUType`; the addFont plugin adds `addFont`.

---

## 2. Behaviour notes

### The paths

| member | value |
|---|---|
| `exePath` | the normalized app path with a trailing `/` |
| `appDataPath` | **the same value** — the reference's `TVPGetAppDataPath()` returns `TVPGetAppPath()` unconditionally |
| `personalPath` | the app path |
| `savedGamesPath` | the app path |
| `dataPath` | the app path + `savedata/` |
| `exeName` | the normalized app path |

`exePath`, `appDataPath`, `dataPath`, `personalPath` and `savedGamesPath`
need the L2 storage; without it they raise
`System.<member>: storage is not attached`. `exeName` does not: it falls back
to the normalized current working directory, and to the constant `"krkr-rs/"`
as a last resort.

### The window-system facts

`desktopLeft`, `desktopTop`, `desktopWidth` and `desktopHeight` are genuine
host facts with no L2 counterpart. They are served through the closure seam
`CompatHost::set_desktop_rect`; when the fact is **not** installed the member
raises `System.<member>: the host fact is not installed` rather than
returning a plausible default.

`screenWidth` / `screenHeight` read the first window's primary-layer rect
from the L2 scene, falling back to the window's inner size and to `(0, 0)`
when there is no window.

`touchDevice` is the constant `0x300` (`tdMouse | tdMouseWheel`) — the
reference returns it unconditionally.

`platformName` is `"Win32"`, `"MacOS"`, `"Android"`, `"iPhone"` or
`"Linux"`; `osName` returns the same value.

### The command line

`getArgument` / `setArgument` mirror the reference's `TVPProgramArguments`: an
ordered vector of `name=value` strings matched **by prefix**, where the
character after the prefix must be `=` or the end of the string. An entry
without a value reads back as `"yes"`. A miss returns **void**.

`setArgument` rewrites the first matching entry, or inserts a new one at the
**front** of the vector.

The vector lives in the per-engine dispatch state, not in a process global.

### The clock

`getTickCount` returns the milliseconds since the **first** call — the epoch
is created lazily and kept for the life of the engine.

### The periodic object rehash

The reference system watch timer calls `TJSDoRehash()` when the rough tick is
more than 1500 ms past the last rehash
(`reference/cpp/core/environ/impl/SystemControl.cpp:181-185`). The frame body
asks for the same rehash through `rehash_poll` before its script polls
(`crates/compat/src/frame.rs`); the call only bumps the rebuild magic, and each
live object rebuilds its member table on its next property access
(`reference/cpp/core/tjs2/tjsObject.cpp:1280-1282`). Without the tick, an
object whose member count grew past its initial bucket count keeps walking a
long collision chain for the rest of the run.

### The keyboard

`getKeyState(code[, getcurrent])` reads the attached L2 input state through
the same virtual-key mapping the `Key` class uses. `getcurrent` defaults to
true (currently held); false asks for "pressed since the previous query",
and the edge is consumed by the query.

### Continuous handlers

`addContinuousHandler` retains the closure **and its identity** (the
`(Object, ObjThis)` pair), so a duplicate registration of the same closure is
ignored. A TJS `null` is accepted silently and stored as an empty closure.

`continuous_handler_poll` delivers the handlers once per frame with the
current tick count. A handler that raises is **removed** (as in the
reference). A handler may register or remove handlers; a newly registered
handler is delivered in the same pass. The state borrow is released across
each callback, so a re-entrant `System` call cannot panic.

### The graphic cache

`clearGraphicCache` clears the L2 scene's decoded-image cache and releases
every bitmap it no longer references. `graphicCacheLimit` reads and writes
the scene's limit: `0` disables caching, `-1` means "the system limit", and
the value is capped at 512 MiB. The setter truncates to a 32-bit integer,
like the reference's `(tjs_int)` cast.

### The inert members

`shellExecute` returns **true** and `system` returns **0** without doing
anything — the reference's own values on a build with no shell. Both log a
warning. `system` additionally clears the graphic cache, which is what the
reference's max-level compact event does.

`touchImages` reproduces the reference's **argument validation** only; the
pre-caching itself is not performed, because the port caches per layer on
demand.

`nullpo` logs an error and continues where the reference crashes the
process.

`drawThreadNum` is stored "for compatibility" and changes nothing.

---

## 3. State ownership

| state | owner |
|---|---|
| the command-line vector, the exit request, `title`, `eventDisabled`, `exitOnWindowClose`, `exitOnNoWindowStartup`, `drawThreadNum`, the tick epoch, the compaction tick, the rehash tick, the UUID counter, the continuous handlers, the key-query edges | the **per-engine dispatch state** (`CompatServices::state::<SystemState>()`) |
| the string scratch buffer for returned strings | the **per-engine dispatch state** |
| the screen size, the graphic cache, the graphic-cache limit | the **L2 scene** |
| the key state | the **L2 input state** |
| the app path, `RegisterData.tjs` | the **L2 storage** |
| the desktop rectangle | the **host facts seam** (`CompatHost::set_desktop_rect`) |

The helpers public for the host:

```rust
pub fn take_exit_request(services: &CompatServices) -> Option<i32>;
pub fn continuous_handler_poll(ctx: &mut CompatCtx) -> Result<bool, CompatError>;
pub fn rehash_poll(ctx: &mut CompatCtx) -> bool;
pub fn compact_poll(ctx: &mut CompatCtx) -> bool;
```

`take_exit_request` consumes the recorded exit code exactly once, so a request
is reported once and the frame loop maps it to its own exit path.
`continuous_handler_poll` is the frame body's `continuous_handler_poll` step;
`rehash_poll` and `compact_poll` are its rehash-tick and idle-compaction steps.

---

## 4. Errors

Every member that can fail raises. The message vocabulary:

| shape | example |
|---|---|
| `"System.<member> requires N argument(s)"` | `System.getArgument requires 1 argument` |
| `"System.<member>: compat context is not bound"` | the engine binding is missing |
| `"System.<member>: storage is not attached"` | no L2 storage |
| `"System.<member>: the host fact is not installed"` | the desktop fact was never installed |
| `"System.<member>: missing value"` | a setter without a value slot |
| `"System.<member>: input state is not attached"` | no L2 input state |
| `"System.<member>: {error}"` | an inner failure |

The integer/real conversion errors are reported **unprefixed**, because they
come from the shared converter: `value is null, not an integer`, `value is an
object, not an integer`, `value is an octet, not an integer`, `value cannot be
converted to an integer`.

---

## 5. Tests

`crates/compat/tests/system.rs` covers the argument round trip and the prefix
semantics, `toActualColor`, `createUUID`, `inputString`, `inform`, the
`createAppLock` result, `assignMessage`, the property round trips and the
defaults, the version and environment properties, the storage-backed paths
(with and without a storage), `exeName`'s fallback, the screen size from the
L2 scene, `readRegValue`, the no-op members and their arity checks, the
continuous handlers' de-duplication and removal, `exit`/`terminate`, the host
fact errors and the host fact seam, `getKeyState` (held state and the
consumed edge), the constant `touchDevice`, `clearGraphicCache`, and the
graphic-cache limit's edges and truncation.

---

## 6. Known gaps

1. **`touchImages` does not pre-cache** anything.
2. **`inform` and `inputString` are headless**: they log and return the
   reference's inert values instead of opening a dialog.
3. **`nullpo` does not crash** the process.
4. **`shellExecute` and `system` do nothing**; they match the reference's
   inert values and log a warning.
5. **`createAppLock` always returns true.**
6. **`drawThreadNum` has no effect.**
7. **`touchDevice` is a constant** and does not reflect real touch hardware.
8. **`desktop*` raise** when the host fact is not installed — deliberately,
   so a missing fact is never mistaken for a real value.
9. **`readRegValue` evaluates `RegisterData.tjs` as an expression** and walks
   the key in-script, so a miss yields void instead of raising.

---

## 7. Related documents

| topic | document |
|---|---|
| the L3 layer and the surface lock | [`../../arch/l3.md`](../../arch/l3.md) |
| the host-facts seam and the error contract | [`../../rules/arch_rules.md`](../../rules/arch_rules.md) §6, §7 |
| the per-engine dispatch state and the one binding | [`../../rules/api_rules.md`](../../rules/api_rules.md) §7.3 |
| the L2 scene and the storage namespace | [`../../code/engine/scene.md`](../../code/engine/scene.md), [`../../code/engine/storage.md`](../../code/engine/storage.md) |
| the storage naming rules | [`storages.md`](storages.md) |
| the keyboard and the mouse | [`input.md`](input.md) |
| the L3 ↔ L2 protocol | [`../../../docs.old/migration/14-l3-l2-protocol.md`](../../../docs.old/migration/14-l3-l2-protocol.md) |
