# `natives/` — the non-visual native families

**Source:** `crates/compat/src/natives/`

Every family lives in its own module with a `register(ctx)` entry point;
`register_all` is the single call the host makes. The classes are **thin
adapters**: they translate TJS values to and from the L2 state and actions,
and they own no engine state themselves.

The visual families are on [`visual.md`](visual.md) and its sub-pages.

---

## 1. The registration order

`natives::register_all` calls, in order:

```
constants → system → debug → storages → scripts → visual → input → sound
→ cdda_midi → async_trigger → menu_item → video → rect → image_function
→ clipboard → plugins → tree_owner
```

The crate-internal `install` registers the null-plugin hook first, then
`register_all`, then the `Plugins` class.

The order matters in two places:

* `visual::register` creates the `Window` class **before** the draw devices,
  because the draw-device classes are attached to the `Window` class object;
* `plugins::register` runs last, because the plugin registrars attach members
  to classes the core families registered (`Layer`, `Window`, `Storages`,
  `Scripts`, `MenuItem`, `WaveSoundBuffer`, `System`).

---

## 2. What every family does

| Concern | Mechanism |
|---|---|
| registration | `register(ctx)` per family; `register_all` calls them all |
| L2 access | only through `crate::engine_api` ([`engine_api.md`](engine_api.md)) |
| L2 handles | the four raw pointers the host attaches to `CompatServices` |
| per-family state | `CompatServices::state::<T>()` — a `TypeId`-keyed map of boxed `RefCell` cells |
| native payload | instance families allocate a `Box` in `create` and free it in `destroy`; `invalidate` is separate where the reference needs it |
| dispatch | instance members resolve their payload **only** from `objthis`; objthis families pass the raw pointer; static families discard it |
| scratch buffers | `out.string` results go into a per-family `StringScratch` in the dispatch state, because the C++ trampoline copies the string after the callback returns |
| errors | every failure is a typed TJS error raised through `report_error` |

**Subclass overrides win** for every event forwarder (`onFire`, `onClick`,
`onStatusChanged`, `onFadeCompleted`, `onLabel`, `onPeriod`, `onFrameUpdate`,
`onTimer`), because the seams resolve the member **on the object** rather than
invoking the native callback directly.

**Strict integer conversion** (`try_value_as_i64`) raises on `null`, an
object or an octet; the loose `value_as_i64` is used only where the
reference's own conversion is loose.

---

## 3. `System` — the process and OS facts

**Module:** `natives/system.rs`

`System` is the process/OS fact class: paths, version, screen size, the
command line, the tick clock, the exit request, the continuous handlers and
the graphic cache.

### The members

| Group | Members | Behaviour |
|---|---|---|
| paths | `exePath`, `dataPath`, `personalPath`, `savedGamesPath`, `appDataPath`, `exeName` | the app directory (with a trailing `/`), `savedata/` under it, and the normalized app path. The reference's `TVPGetAppDataPath()` returns the app path, so `personalPath` and `savedGamesPath` are the same directory. |
| identity | `versionString`, `versionInformation`, `platformName`, `osName`, `processorNum`, `exeBits`, `osBits` | `versionString` is `"2.32.2.426"`; `platformName` and `osName` are the same value (`Win32`, `MacOS`, `Android`, `iPhone`, `Linux`); `processorNum` is the available parallelism; the bit counts are the pointer width. |
| screen | `screenWidth`, `screenHeight` | the **logical screen** — the first window's primary layer, else the window client size, else `(0, 0)`. |
| desktop | `desktopLeft`, `desktopTop`, `desktopWidth`, `desktopHeight` | the installed host rectangle. Without the host fact the member **raises** rather than returning a fabricated value. |
| command line | `getArgument`, `setArgument` | `getArgument` prefix-matches (the next character must be `=` or the end of the string) and returns `"yes"` for a value-less entry, or **void** on a miss — the reference has no default parameter. `setArgument` rewrites the first prefix match or inserts `name=value` at the front. |
| clock | `getTickCount` | the engine's **frame clock** in whole milliseconds — the value the frame advances and the timer registry is fired against. Without an attached clock the member **raises**. |
| exit | `exit`, `terminate` | record a one-shot exit request; the host drains it with `take_exit_request`. |
| UI | `inform`, `inputString`, `showVersion`, `dumpHeap`, `nullpo` | no GUI: `inform` logs at info and returns void (no buttons) or `0` (one or two buttons) / `-1` (an unsupported button set); `inputString` logs and returns the initial value; `dumpHeap` is a no-op; `nullpo` logs an error instead of crashing. |
| colour / identity | `toActualColor`, `createUUID`, `createAppLock`, `assignMessage` | `toActualColor` maps a TVP system colour (high byte set) to `0xRRGGBB`; `createUUID` returns an RFC-4122 v4 string; `createAppLock` always returns `true`; `assignMessage` reports whether a message by that name exists. |
| inert Win32 | `shellExecute`, `system` | `shellExecute` returns `true` and logs a warning; `system` clears the graphic cache, logs a warning and returns `0`. |
| GC / cache | `doCompact`, `clearGraphicCache`, `graphicCacheLimit` | `doCompact(level = 100)` runs the TJS collector at level ≥ 5 and clears the graphic cache at level ≥ 15; `graphicCacheLimit` reads and writes the L2 scene's cache bound (`0` disables it). |
| registry | `readRegValue` | evaluates `RegisterData.tjs` from the attached storage and walks the `/`-separated key in-script; a miss is **void**. |
| images | `touchImages` | validates the argument shapes only. |
| continuous handlers | `addContinuousHandler`, `removeContinuousHandler` | a per-frame callback, identity-deduplicated; `null` is an accepted empty closure; a handler may remove itself. |
| input | `getKeyState` | reads the attached `InputState` through the same VK mapping as `Key` (the mouse-button VKs and `VK_PADANY` included). `getcurrent = false` reports "went down since the previous query for this code". |
| data members | `exceptionHandler`, `onActivate`, `onDeactivate` | registered as `null`. |

### State ownership

| Layer | Item |
|---|---|
| L2 | `Storage` (the paths), `InputState` (the key state), `Scene` (the screen size, the graphic cache), `FrameClock` (the tick count) |
| L3 payload | none — `System` is a static class |
| L3 dispatch state | `SystemState { arguments, exit_request, title, event_disabled, exit_on_window_close, exit_on_no_window_startup, draw_thread_num, rough_tick_epoch, last_compacted_ms, uuid_counter, continuous, current_continuous, remove_current_continuous, key_query_seen }`, plus the `HostFacts` desktop provider |

### Invariants

* `exit` / `terminate` report the request **exactly once**.
* `exitOnWindowClose` and `exitOnNoWindowStartup` default to `true`.
* The continuous-handler state borrow is released across each VM callback, so
  a handler may re-enter `System`.

### Host seams

`set_desktop_rect`, the `window_ex` provider set, `begin_native_timing` /
`end_native_timing`, and the per-frame drain of `take_exit_request`.

### Tests

`crates/compat/tests/system.rs`, `crates/compat/tests/reference_surface.rs`
(the reference defaults, the argument semantics, the strict integer writers),
`crates/compat/tests/window_ex.rs`, `crates/compat/tests/get_about.rs`,
`crates/compat/tests/add_font.rs`.

---

## 4. `Storages` — the storage-namespace facade

**Module:** `natives/storages.rs`

| Member | Behaviour |
|---|---|
| `addAutoPath`, `removeAutoPath` | maintain the L2 auto-path list. The path must end with `/`, `\` or `>` and must not be empty. |
| `getFullPath` | the normalized storage name (`\` → `/`, lower-cased; the part after `>` is collapsed too). |
| `getPlacedPath` | the normalized game-relative disk path, `archive>in-archive` form, or `""`. **Documented approximation:** the reference returns an absolute path; the port returns a relative one, and both still resolve back to the same storage. |
| `isExistentStorage` | resolves the bare name in the mounted namespace and the auto paths. |
| `extractStorageExt`, `extractStorageName`, `extractStoragePath`, `chopStorageExt` | the pure string helpers. |
| `clearArchiveCache` | clears the L3 placed-path cache **and** the L2 resolution cache. |
| `getLocalName` | the absolute disk path for a disk file, the name unchanged for a non-absolute name, and a **raise** for an archive-only name. |
| `searchCD`, `selectFile` | the inert reference values: `""` and `false`. |

The **core** `Storages` class registers 13 members: the 13 above. The
file-manipulation members a game may also see — `copyFile`, `deleteFile`,
`moveFile`, `createDirectory`, `removeDirectory`, `truncateFile`,
`selectDirectory`, `dirlist`, the file-attribute members and the XP3
filters — are attached to the same class by the `var_file`, `fstat`,
`dirlist` and `xp3_filter` plugins; see [`plugins.md`](plugins.md). The
reference's `Storages.open` is **not registered** anywhere, so a script gets
the ordinary TJS "member does not exist" error rather than a silent
fallback.

### State ownership

L2 `Storage` (the mounts, the auto paths, `find`, `exists`, `read`,
`clear_resolution_cache`, `game_dir`), `Location`, `StorageReadError` and
`normalize_in_archive_name`; L3 dispatch state `PlacedCache` and a
`StringScratch`.

### Invariants

* The `file://` media prefix is stripped before the L2 lookup, and the
  **script spelling** is kept in the diagnostics.
* `StorageReadError::InvalidPathName` is raised **verbatim** — the reference
  text `"Invalid path name %1"` with the name substituted; every other
  storage error keeps the `Storages.<member>: ` prefix.
* The placed-path cache is cleared whenever the search path changes.

### Tests

`crates/compat/tests/storages.rs`, `crates/compat/tests/storages_io.rs`,
`crates/compat/tests/storage_media_prefix.rs`, `crates/compat/tests/fstat.rs`,
`crates/compat/tests/xp3_filter.rs`.

---

## 5. `Scripts` — script execution and introspection

**Module:** `natives/scripts.rs`

| Member | Behaviour |
|---|---|
| `execStorage(name[, mode[, context]])`, `evalStorage(...)` | read the storage and run it as a script / expression; an explicit top-level `return` is the result. The `mode` honours the reference's `oN` byte offset; `context` is the script's `this`. |
| `exec(script[, name[, lineofs[, context]]])`, `eval(...)` | run a string. |
| `compileStorage(name, output[, isresult[, outputdebug[, isexpression]]])` | compile to a bytecode file. A storage script is offered to the TJS2 **bytecode loader first** and decoded as text only when the loader rejects it. |
| `setCallMissing(obj)` | installs the `missing` handler. |
| `getClassNames(obj)` | the class-name chain as an array. |
| `getTraceString([limit])`, `dump()` | the VM stack trace and every live script block. |
| `textEncoding` | the fallback text decoder. An **unset** property means "automatic" (BOM / charset detection, then UTF-8, then CP932) — not a forced UTF-8. |

### State ownership

L2 `Storage` (the script bytes) and `crate::text` (the decoding); L3 dispatch
state `ScriptsState { text_encoding, depth }`, a `StringScratch`, and the
shared `NativeTiming` entry.

### Invariants

* Script decoding never produces mojibake: an undecodable script raises a
  typed error.
* The script-execution nesting depth is restored on **every** exit path,
  including an early storage or decode error, so the timing lines show the
  correct nesting.
* The timing guard exists only while `debug` logging is on; a release build
  pays nothing.

### Host seams

`compat::exec_storage(engine, storage, name)` is the composition-root entry
point for the startup sequence; it resolves the binding itself so the nesting
depth stays continuous across the native boundary.

### Tests

`crates/compat/tests/scripts.rs`, `crates/compat/tests/scripts_ex.rs`,
`crates/compat/tests/json.rs`, `crates/compat/tests/storage_media_prefix.rs`,
`crates/compat/tests/dispatch_fmt.rs`.

---

## 6. `Mouse`, `Key` and `Pad` — the input surface

**Module:** `natives/input.rs`

`Mouse` and `Key` are **port-only** classes: the reference declares the
handlers but exposes no such classes.

| Class | Members | Behaviour |
|---|---|---|
| `Mouse` | `getCursorPos`, `getCursorX`, `getCursorY`, `setCursorPos`, `getWheelRot`, `getWheelRotX`, `getWheelRotY`, `getWheelRotZ`, `isVisible`, `setVisible`, `getPressed`, `getReleased`, `getRepeat`, `getClickCount` | `getCursorPos([obj])` returns `"x,y"` or fills `obj.x`/`obj.y` and returns void; `getWheelRotX`/`getWheelRotZ` are always `0` (the generic wheel is vertical-only); the button flags and the click count come from the L2 `InputState`. |
| `Key` | `getPressed`, `getReleased`, `getRepeat`, plus the read-only `kXXX` virtual-key constants | the generic modifier codes match either side, the explicit side codes match that side, `VK_PADANY` matches any gamepad key, and the mouse-button VKs resolve to the mouse buttons. An **unknown** code is `false`/`0`, not an error. |
| `Pad` | the reference's *text-editor* placeholder properties | a read-only stub whose setters are accepted and ignored. |

### The frame protocol

The host drives the L2 input state:

```
begin_input_frame → apply_input_event(event)… → end_input_frame
```

`begin_frame` clears the release flags, the wheel accumulation, the released
keys and `last_pressed`; `end_frame` bumps the held counters and advances
the frame index.

### State ownership

L2 `InputState`; the click-sequence policy (`30` frames, `4` px) is
installed on the L2 state by the host. No L3 payload, no L3 dispatch state.

### Tests

`crates/compat/tests/input.rs`, `crates/compat/tests/dispatch_ui.rs`,
`crates/compat/tests/event.rs`, `crates/compat/tests/event_dispatch.rs`.

---

## 7. `WaveSoundBuffer` — the script sound object

**Module:** `natives/sound.rs` (plus `sound/wave_flags.rs` and
`sound/phase_vocoder.rs`)

| Group | Members | Behaviour |
|---|---|---|
| transport | `open`, `play`, `stop`, `fade`, `stopFade`, `setPos` | `open` reads the audio and its `<name>.sli` side-car through the L2 storage and hands both to the installed decoder chain. `fade(to, timeMs[, delayMs])` reproduces the reference's 60 ms-beat fade state machine; `stopFade` jumps to the target. |
| position | `position`, `samplePosition`, `totalTime` | `position` and `totalTime` are milliseconds; `samplePosition` is a granule count (`position_seconds × sample_rate`). |
| volume | `volume`, `volume2`, `pan`, `globalVolume`, `globalFocusMode` | the script scale is `0..=100000` / `-100000..=100000`; the L2 mixer works in `0..=1` / `-1..=1`, so every boundary divides by `TVP_VOLUME_SCALE = 100000.0`. `globalFocusMode` is stored verbatim (the reference casts and stores without clamping). |
| state | `paused`, `looping`, `status`, `frequency`, `bits`, `channels` | `status` is the reference status string (`"unload"`, `"stop"`, `"play"`, `"pause"`, …). `bits` is always `32` — a **documented approximation**, because the L2 `AudioSpec` carries no bit depth. `frequency` is plain state: the reference's `SetFrequency` only stores the value, so the member never changes the playback rate. |
| caches | `flags`, `labels`, `filters`, `useVisBuffer` | `flags` returns a live `WaveFlags` proxy (identity-stable per buffer); `labels` is a cached dictionary of the `.sli` labels; `filters` is the reference's stable TJS array. |
| events | `onStatusChanged`, `onFadeCompleted`, `onLabel` | dispatched **through the object's class chain**, so a script subclass override wins. A script-driven `open`/`play`/`stop` dispatches **synchronously inside the call** (the reference's immediate mode) and cancels the events still queued for that object; label and fade completions were posted and arrive at the next poll. |
| statics | `freeDirectSound` | a no-op; there is no DirectSound. |

### State ownership

L2 `Mixer` (one shared mixer), `SoundBuffer`, `SoundChannel`, `SoundSource`,
`SoundLabel`, `LoopInfo`/`LoopLink`/`LoopCondition`, `SoundEvent`, `Status`;
L3 dispatch state `SoundState { mixer, objects, next_id, clock, last_poll_ms,
render_driven, decoders, vocoders, vis_buffers, global_focus_mode }`.

### Invariants

* The port retains `this` so the poll can dispatch through it — TJS2 has no
  cycle collector, so teardown is `invalidate`-driven, not drop-driven.
* `invalidate` mirrors the reference chain: playback stops, every retained
  value is released, the cached `labels` dictionary is invalidated, and no
  further events are dispatched.
* The decoder chain is tried **in order**; the first success wins. When
  every decoder rejects the bytes, **all** their errors are reported joined
  with `"; "`.
* The `.sli` **parse** result is carried past the decode: the reference
  assigns the input format and the frequency *before* it parses the
  side-car, so a side-car failure still leaves the format in place.

### Host seams

`compat::add_audio_decoder`, `compat::set_sound_render_driven`,
`compat::sound_render`, `compat::set_output_latency`, `compat::sound_poll`.

### Tests

`crates/compat/tests/sound.rs`, `crates/compat/tests/dispatch_sound.rs`,
`crates/compat/tests/get_sample.rs`, `crates/compat/tests/wuvorbis.rs`,
`crates/compat/tests/audio_render.rs`.

---

## 8. `VideoOverlay` — movie playback

**Module:** `natives/video.rs`

`VideoOverlay` plays a movie into a layer, a mixing layer or the overlay.

| Group | Members | Behaviour |
|---|---|---|
| transport | `open`, `close`, `play`, `pause`, `stop`, `rewind`, `prepare` | `open` asks the host `VideoSource` for the spec and **never fabricates metadata** — without a source it raises. |
| position | `position`, `frame`, `loop`, `playRate`, `periodEventFrame`, `segmentLoopStartFrame`, `segmentLoopEndFrame` | `position` is milliseconds, `frame` is the frame index. |
| geometry | `left`, `top`, `width`, `height`, `setPos`, `setSize`, `setBounds`, `layer1`, `layer2` | the destination rectangle and the target layers. `layer1`/`layer2` read back as `null` when unset. |
| modes | `mode`, `mixingLayer`, `resetMixingLayer`, `mixingMovieAlpha`, `mixingMovieBGColor` | `mode` is `vomOverlay` (0, the default), `vomLayer` (1) or `vomMixer` (2). The mode is **frozen after `open`**. |
| colour | `contrast`, `brightness`, `hue`, `saturation` (+ each one's range/default/step) | passthrough fields the backend honours. |
| audio | `audioVolume`, `audioBalance`, `enabledAudioStream`, `enabledVideoStream`, `numberOfAudioStream`, `numberOfVideoStream` | the TVP `-100000..=100000` scale, pushed to the shared mixer voice. |
| events | `onStatusChanged`, `onPeriod`, `onFrameUpdate`, `onCallbackCommand` | dispatched from the L2 `VideoEvent`s. A `TransitionComplete` event has no registered callback and is dropped. |

### State ownership

L2 `VideoOverlay` (the whole playback machine), `VideoBounds`, `PlayStatus`,
`VideoEvent`, the frame blits, `Scene` (the target layers) and the mixer
voice; L3 dispatch state `VideoState` and the `VideoOverlayInst` payload.

### Host seams

`CompatHost::set_video_source(spec, frames)` or
`compat::set_video_source(ctx, Box<dyn VideoSource>)`, and
`compat::video_advance(ctx, dt_ms)`.

### Tests

`crates/compat/tests/video.rs`, `crates/compat/tests/dispatch_kg.rs`.

---

## 9. `AsyncTrigger` — the deferred callback class

**Module:** `natives/async_trigger.rs`

`trigger()` only **schedules** a handler; the frame body fires it later.

| Member | Behaviour |
|---|---|
| `AsyncTrigger(action[, actionName])` | retains the action. An **empty** action name invokes the action object itself; an omitted or void name defaults to `"action"`. |
| `trigger` | schedules a fire. **No VM call happens inside `trigger()`**, so a handler can never re-enter the VM through the call that scheduled it. |
| `cancel` | drops every scheduled fire. |
| `onFire` | invokes the retained action with the reference event dictionary `{type: "onFire", target: <the trigger>}`. |
| `cached` | defaults to **true**. While a fire is scheduled, another `trigger()` cancels it and re-posts it (the re-triggered fire moves to the end of the queue). With `cached = false` each `trigger()` queues its own fire. |
| `mode` | `atmNormal` (0), `atmExclusive` (1), `atmAtIdle` (2). |

Changing `cached` or `mode` cancels the scheduled fires. After `invalidate`,
`trigger` and `cancel` are **silent no-ops**.

### State ownership

L3 dispatch state `AsyncTriggerState { next_id, triggers, pending }`; the
payload holds `cached` and `mode`, because the reference keeps them as
native-instance members that **survive `invalidate`**. No L2 state — the
class does not use the timer registry.

### Tests

`crates/compat/tests/async_trigger.rs`,
`crates/compat/tests/reference_surface.rs`,
`crates/compat/tests/extend_dispatch.rs`.

---

## 10. `MenuItem` — the logical menu tree

**Module:** `natives/menu_item.rs`

The tree itself is the authoritative state; there is **no OS menu** in this
port.

| Member | Behaviour |
|---|---|
| `add`, `insert`, `remove`, `index` | the tree operations; `index` is the position in the parent. |
| `caption`, `checked`, `enabled`, `radio`, `group`, `visible`, `shortcut` | the flags. `enabled` and `visible` default to **true**. Setting `checked` on a `radio` item clears the other items of the same `group`. |
| `parent`, `root`, `children`, `window` | the traversal. `children` returns the **same** array object until the child list changes. |
| `fireClick` | walks the ancestor chain and does nothing if any ancestor is disabled; then requires a `Window` in the ancestor chain and that window's `visible` state; then invokes the object's `onClick` member. |
| `onClick` | dispatches **only** `owner.action(ev)` with the reference event dictionary; a missing or failing `action` is ignored. |
| `HMENU` | `0` — the reference's `GetMenuItemHandleForPlugin` returns `nullptr`. |
| `popup(flags, x, y)` | returns the reference success value `1` after the reference `AsInteger` coercion. |
| `textToKeycode`, `keycodeToText` | the read-only shortcut tables, built once at registration. |

### State ownership

No L2 state — the tree is pure L3 logical state. L3 dispatch state
`MenuState { nodes, next_id, instances, text_to_keycode, keycode_to_text }`;
the payload holds only the node id.

### Invariants

* The TJS object owns only its node id; the node holds the retained script
  objects needed to hand the **real** objects back.
* A child is kept alive while it is attached, so `root.add(new MenuItem(…))`
  keeps it alive.
* The item's own destroy callback removes the node;
  `CompatHost::menu_node_count()` is the leak diagnostic.

### Tests

`crates/compat/tests/menu_item.rs`, `crates/compat/tests/menu_dll_surface.rs`,
`crates/compat/tests/reference_surface.rs`,
`crates/compat/tests/dispatch_ui.rs`.

---

## 11. `Debug` — the logging service

**Module:** `natives/debug.rs`

`Debug` writes to the log, keeps a ring buffer of recent lines, mirrors the
lines to a file, and exposes the `Controller` / `Console` singletons.

| Member | Behaviour |
|---|---|
| `message(...)`, `notice(...)` | write to the `log` crate (`message` at info, `notice` at debug), push to the ring buffer, append to the file when one is open, and dispatch the line to every retained logging handler. Multiple arguments are joined with `", "`. |
| `getLastLog([lines])` | without an argument, the most recent entry (or `""`); with one, up to `lines` recent entries joined with `\n`, each formatted `Time + ' ' + Log + "\r\n"`. |
| `startLogToFile([clear])` | an **optional boolean**, no filename: the file is `{logLocation}/krkr.console.log`. Ignored while already logging; optionally truncates; then flushes the accumulated important log, the separator and the last 100 lines. |
| `logAsError()` | starts the error log file when `logToFileOnError` is set, using `clearLogFileOnError` as the clear flag. |
| `logLocation` | normalized like a storage name; an in-archive name has no local form and clears the location; the `-forcelog=yes\|clear` command-line option starts logging. |
| `addLoggingHandler`, `removeLoggingHandler` | matched by closure identity; a duplicate registration is ignored; a non-object argument raises while `null` is accepted. |
| `controller`, `console` | lazily created, cached singletons backed by the same visibility fields. |

The ring buffer holds `TVPLogMaxLines + 100 = 2148` entries; the oldest **100**
are dropped at once when the cap is reached. The **file and the handlers see
the prefixed line** (`" " + line`) while the ring buffer stores the raw line.

### State ownership

L1 `fs` (the log file, opened on the host filesystem); L3 dispatch state
`DebugState` — the log file handle, the ring buffer, the important log, the
location, the flags, the handlers and the console/controller singletons.

### Tests

`crates/compat/tests/debug.rs`,
`crates/compat/tests/reference_surface.rs`,
`crates/compat/tests/dispatch_ui.rs`.

---

## 12. `Rect` — a geometry value object

**Module:** `natives/rect.rs`

`Rect` is small and self-contained: its entire state is four 32-bit edges in
the per-object payload, and it composes **no** engine or scene state.

| Member | Behaviour |
|---|---|
| `Rect()`, `Rect(left, top, right, bottom)`, `Rect(other)` | 0, 1 or 4 arguments; other counts are ignored. |
| `left`, `top`, `right`, `bottom`, `width`, `height`, `isEmpty` | `width` = `right - left`, `height` = `bottom - top`; `isEmpty` = `left >= right \|\| top >= bottom`. |
| `set`, `setSize`, `setOffset`, `addOffset`, `clear` | the mutators. `setSize` keeps `left`/`top`; `setOffset` moves and keeps the size. |
| `clip`, `union`, `intersects`, `included`, `includedPos`, `equal` | the relations. `clip` intersects **in place** and returns whether a non-empty intersection remains — on an empty intersection the receiver is unchanged. The interval convention is **half-open**: `includedPos(10, 10)` on a `0..10` rectangle is false. |
| `nativeArray` | a read-only pointer value to the first edge; the payload is `#[repr(C)]` with the fields in reference order. |

The cross-object members take another **script object**: the argument is
resolved through the VM's native-instance lookup, so only a **live native
`Rect`** is accepted. `null` follows the reference's null branch; a plain
script object with `left`/`top`/`right`/`bottom` members is the reference's
invalid-parameter error, not a duck-typed success.

All the arithmetic is `wrapping_*`, matching the reference's 32-bit edges.

### Tests

`crates/compat/tests/rect.rs`, `crates/compat/tests/reference_surface.rs`,
`crates/compat/tests/dispatch_fmt.rs`.

---

## 13. `ImageFunction` — the stateless bitmap service

**Module:** `natives/image_function.rs`

`new ImageFunction()` allocates an empty instance; every member is a
**static** method whose first argument is the destination `Bitmap`.

| Member | Behaviour |
|---|---|
| `operateRect`, `operateStretch`, `operateAffine` | the blits. |
| `flipLR`, `flipUD` | mirror a region in place. |
| `fillRect`, `colorRect` | `fillRect` replaces RGB (and the alpha when given); without an alpha argument it **holds** the destination alpha. `colorRect` writes the alpha channel on the `dfMask` face and RGB on the `dfOpaque` face. |
| `adjustGamma`, `doBoxBlur`, `doGrayScale` | the filters; `doGrayScale` uses the reference luma. |
| `drawText`, `drawGlyph` | `drawText` reads the face and the style from the `Font` argument; `drawGlyph` extracts the glyph's text and draws it with the resolver's default face. |

The rectangle convention is half-open, the arguments are read through
`left`/`top`/`right`/`bottom` from a native **`Rect`** object (`null` follows
the reference's null branch); a plain dictionary or any other non-`Rect`
object raises `TJS_E_INVALIDPARAM` instead of being accepted. The result
marshaller builds a **real `Rect` object**, not a dictionary.

### State ownership

L2 `Scene` (the bitmap store), `BitmapState`, `layer_ops` and `raster`
primitives, and the shared `ResourceResolver` for the text faces. No L3
payload and no L3 dispatch state — the class owns no engine state and keeps
nothing in globals.

### Tests

`crates/compat/tests/image_function.rs`, `crates/compat/tests/dispatch_fmt.rs`,
`crates/compat/tests/resource.rs`.

---

## 14. `Clipboard` — the platform clipboard

**Module:** `natives/clipboard.rs`

| Member | Behaviour |
|---|---|
| `hasFormat(format)` | the reference static method (`cbfText == 1`); it returns **`false`** for any format other than `cbfText` **without consulting the backend**. |
| `asText` | reads the clipboard text, or **void** when the clipboard holds no text. The setter replaces the text. |

L3 owns no window system: the host installs a `ClipboardBackend` (three
platform functions). **Without a backend every member raises a typed TJS
error** — the native never fabricates a `false`/void/empty value. This
deliberately differs from the reference's non-platform stub.

### Tests

`crates/compat/tests/clipboard.rs`,
`crates/compat/tests/reference_surface.rs`.

---

## 15. `BitmapLayerTreeOwner` — the layer-tree event owner

**Module:** `natives/tree_owner.rs`

The draw target for a layer tree: it turns host and input events into a
dispatch on the tree (`fire*`) and receives notifications back (`on*` hooks).

| Member | Behaviour |
|---|---|
| `width`, `height` | from the primary layer, falling back to the scene window size. |
| `bitmap` | the lazily created internal render-target bitmap. |
| `focusedLayer` | reads and writes the focused layer object; the setter also pushes the id into the L2 scene, so the scene stays the single source of truth. |
| `primaryLayer` | **read-only**; the getter **raises** until a primary layer is attached. |
| `fireClick`, `fireDoubleClick`, `fireMouseDown`, `fireMouseUp`, `fireMouseMove`, `fireMouseWheel`, `fireMouseOutOfWindow`, `fireReleaseCapture`, `fireTouchDown`, `fireTouchUp`, `fireTouchMove`, `fireTouchScaling`, `fireTouchRotate`, `fireMultiTouch`, `fireKeyDown`, `fireKeyUp`, `fireKeyPress`, `fireDisplayRotate`, `fireRecheckInputState` | each call resolves the handler **by name at dispatch time** on the focused layer (falling back to the primary layer) and invokes it. The event is reported as consumed **only when the handler returns a truthy value**; the port's no-op defaults return void, so an unimplemented event is not consumed. A missing argument is filled from the attached L2 input state; explicit arguments win. |
| `onChangeLayerImage`, `onDisableAttentionPoint`, `onGetCursorPos`, `onReleaseMouseCapture`, `onResetImeMode`, `onResizeLayer`, `onSetAttentionPoint`, `onSetCursorPos`, `onSetHintText`, `onSetImeMode`, `onSetMouseCursor` | assignable no-op methods whose default is **callable**, so a direct call never raises. |

### Tests

`crates/compat/tests/tree_owner.rs`, `crates/compat/tests/layer_behaviour.rs`,
`crates/compat/tests/extend_dispatch.rs`.

---

## 16. `CDDASoundBuffer` and `MIDISoundBuffer` — the disabled disc-sound build

**Module:** `natives/cdda_midi.rs`

The reference guards each class's playback surface behind a feature macro
that is **defined nowhere in the tree**. The port mirrors the **disabled**
build faithfully:

* `open`, `play`, `stop`, `fade`, `stopFade` are no-ops that only check their
  argument counts;
* `status` is always `"unload"`, `volume2` is always `0`; the CD-DA `volume`
  getter is always void and the MIDI `volume` getter is always `0`;
* the property setters are inert;
* the event members (`onStatusChanged`, `onFadeCompleted`) are **outside** the
  guards and still forward to the constructor's action owner;
* `midiOut(data)` is a no-op when MIDI is disabled.

No object owns a mixer channel, a decoder or a playback device: the classes
are constructed and queried, never played.

### Tests

`crates/compat/tests/cdda_midi.rs`.

---

## 17. `KAGParser` — the scenario parser

**Modules:** `natives/kag/{mod,parser,state,kvp}.rs`

| Module | Role |
|---|---|
| `kag/parser.rs` | a dependency-free **push** parser: text → `Vec<Event>` plus a label index. Pure `std` + `thiserror`. |
| `kag/state.rs` | the **pull** state machine: parses once into events, then walks them one tag at a time (the reference's `_GetNextTag`). It holds the `Environ` hooks, the call stack, the macro recording and expansion, and `store`/`restore`. |
| `kag/kvp.rs` | the string codec for the dictionary-shaped data kept as strings: `key=value` lines joined with `\n`, with `\` → `\\`, `\n` → `\n`, `\r` → `\r` escaping. |
| `kag/mod.rs` | the thin native adapter: registers `KAGParser` in the core startup pass and marshals the script values. |

### The members

`loadScenario`, `goToLabel`, `callLabel`, `getNextTag`, `assign`, `clear`,
`store`, `restore`, `clearCallStack`, `popMacroArgs`, `interrupt`,
`resetInterrupt`; the properties `ignoreCR`, `processSpecialTags`,
`multiLineTagEnabled`, `debugLevel`, `curLine`, `curPos`, `curLineStr`,
`callStackDepth`, `curStorage`, `curLabel`, `macros`, `paramMacros`,
`macroParams`, `mp`.

`getNextTag` returns a **real TJS Dictionary** (not the reference's string
encoding): `tagname` plus every attribute plus `taglist`, an array of the
member names in source order. The dictionary is built with `%[]` and
populated through the VM's `set_member` — **no scenario text is evaluated as
TJS**.

`macros`, `macroParams` and `paramMacros` are real TJS Dictionaries;
`macros` is read-only and identity-stable while its content is unchanged.

`store` / `restore` use a Dictionary snapshot: a lossless `data` string plus
the reference's top-level members.

### The parser line model

Lines are split on `\r`, `\n`, `\r\n` and the leading tabs are stripped.

| Line | Meaning |
|---|---|
| `;` | a comment |
| `*name` or `*name\|macro` | a label |
| `@` | a line-command tag |
| `%` | a directive |
| `[` | a square-bracket tag |
| anything else | plain text |

Tag and attribute names are lower-cased. Attribute values may be
`name=value`, quoted with `"` or `'`, or a bare flag (the value `"true"`). A
backtick escapes the next character; the `&` and `%` value prefixes are
preserved verbatim.

### Invariants

* The parser state lives in the **instance payload** — there are no process
  globals beyond the documented binding.
* The `macros` / `macroParams` dictionaries keep a stable identity while
  their content is unchanged.
* A missing owner handler, or a handler that errors, leaves the tag
  processed.
* The `if`/`elsif`/`ignore` conditions, the `emb` expressions and the `&entity`
  values are evaluated by calling back into the TJS VM through `Environ::eval`.
* Label names keep their leading `*`; a duplicate label is suffixed `:2`,
  `:3`, … and an empty label aliases the previous one.
* `curLine` / `curPos` are **0-based** (the parser's own event line numbers
  are 1-based).

### The `Environ`

The state machine calls out through an injected `Environ`: `eval`,
`load_storage`, and the optional owner callbacks `fire_label`, `fire_script`,
`fire_jump`, `fire_call`, `fire_return`, `fire_scenario_load` and
`fire_scenario_loaded`. The owner callbacks are resolved on the script object
the game passes as `objthis` — the host/game installs them, not the engine.

### Tests

`crates/compat/tests/kag.rs` (the largest suite in the crate),
`crates/compat/tests/dispatch_kg.rs`, `crates/compat/tests/extend_dispatch.rs`.

---

## 18. The support modules

### `constants.rs`

Publishes the TVP global constants by evaluating a single `const` declaration
list on the engine at install time: 411 scalar constants plus the
`imageTagLayerType` dictionary. The list is a single declaration in source
order and the aliases (`omAlpha = ltAlpha`) reference earlier names, so the
**evaluation order is load-bearing** and it must run exactly once per VM.

A failure raises `CompatError::Engine("TVP global constants: {error}")`.

### `tjsnum.rs`

The TJS `string → integer` coercion: it parses the **leading** number token
with the lexer's grammar and integer-ises it — an optional sign (plus
following spaces), `true`/`false`, `0x`/`0b`/leading-zero octal, and decimal
integer/real literals with `e` exponents. Anything unparsable yields `0`;
trailing garbage (`"12abc"`) is ignored.

It is kept free of imports beyond `std` so the KAG attribute walk can
include it directly.

---

## 19. The documented approximations

These are the places where a member's value is **known** to differ from the
reference. Each one is a deliberate, recorded divergence:

| Member | The port | The reference |
|---|---|---|
| `Storages.getPlacedPath` | a game-relative disk path or `archive>path` | an absolute `file://domain/path` |
| `WaveSoundBuffer.bits` | always `32` | the source's bit depth |
| `System.touchDevice` | the constant `0x300` | the detected device class |
| `System.createAppLock` | always `true` | a real lock |
| `System.shellExecute`, `System.system` | inert values with a warning | the Win32 calls |
| `System.inform`, `System.inputString` | logged, no GUI | a message box / an input dialog |
| `Mouse`, `Key` | port-only classes | the reference declares the handlers but no such classes |

The aggregate list of open L3 gaps is in
[`../../arch/l3.md`](../../arch/l3.md) §9.

---

## See also

* [`visual.md`](visual.md) — the visual families
* [`engine_api.md`](engine_api.md) — the L2 items these families compose
* [`ctx.md`](ctx.md) — the dispatch state these families keep
* [`../../arch/l3.md`](../../arch/l3.md) §3 — the native classes at the layer level
