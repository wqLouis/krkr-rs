# windowEx (`windowEx.dll`)

**DLL name:** `windowEx.dll` — linked with `Plugins.link("windowEx.dll")`
**Rust module:** `crates/compat/src/natives/plugins/window_ex/`
**Reference:** `reference/cpp/plugins/windowEx.cpp`
**Registers:** members on `System`, `Scripts`, `Window`, `MenuItem`, `Console` and `Pad`, plus the `Window._Notifications` dictionary

The plugin has no class of its own: the reference hooks its `WindowEx`,
`MenuItemEx` and `PadEx` implementations onto the existing core classes
(`NCB_ATTACH_CLASS_WITH_HOOK(WindowEx, Window)`,
`reference/cpp/plugins/windowEx.cpp:934`, `:1219`, `:1416`) and attaches its
`Console` members to the `Debug.console` singleton
(`reference/cpp/plugins/windowEx.cpp:1333-1346`). The compat VM has no
subclassing and its `register_instance_members` attaches objthis-only members
(no per-object native payload) to an existing class, so the members live on
the core classes themselves: `WindowEx`'s on `Window`, `MenuItemEx`'s on
`MenuItem`, `PadEx`'s on `Pad` and `ConsoleEx`'s on `Console`. There is no
`WindowEx` global class, matching the reference.

The `getabout.dll` entry depends on `windowEx.dll` and registers nothing
itself, because the member it owns — `System.getAboutString` — is registered
here (see [`get_about`](get_about.md)). The plugin index is
[`../plugins.md`](../plugins.md).

## Surface

### `System` members

Fourteen static methods, registered in
`crates/compat/src/natives/plugins/window_ex/system.rs`. The reference
attaches fifteen (`reference/cpp/plugins/windowEx.cpp:1513-1527`); the port
does not re-register `clearGraphicCache`, because the core `System` class
already owns that member.

| member | kind | behaviour |
|---|---|---|
| `getDisplayMonitors()` | static method | the monitor layout: an array whose entries carry a `primary` flag plus `monitor` and `work` sub-dictionaries of `{x, y, w, h}` |
| `getMonitorInfo(...)` | static method | one monitor's dictionary, in the same shape; the argument form selects the monitor |
| `getCursorPos()` | static method | the cursor position as `{x, y}` |
| `setCursorPos(x, y)` | static method | moves the cursor; needs two arguments |
| `getSystemMetrics(name)` | static method | one system metric, by its uppercased name; the `System.metrics` dictionary is consulted first |
| `readEnvValue(name)` | static method | an environment variable's value, or `void` when it is unset |
| `expandEnvString(text)` | static method | the text with its `%NAME%` references expanded |
| `setApplicationIcon(path)` | static method | hands the icon path to the host; no argument validation, and with no argument it does nothing |
| `setIconicPreview(enabled)` | static method | enables the minimized-window preview; returns `true` |
| `getDoubleClickTime()` | static method | the double-click time in milliseconds; `-1` unless the host answers |
| `breathe()` | static method | lets the process breathe; returns `void` |
| `isBreathing()` | static method | whether the breathing mode is on; `0` or `1` |
| `getAboutString()` | static method | the about string — the member the `getabout` port would register (see [`get_about`](get_about.md)) |
| `getCPUType()` | static method | the CPU-feature bitmask as an *integer*; `0` unless the host answers |

### `Scripts` members

Two static methods, registered in
`crates/compat/src/natives/plugins/window_ex/scripts.rs`.

| member | kind | behaviour |
|---|---|---|
| `eval(content[, name[, lineofs[, context]]])` | static method | the error-log-aware override of the core `eval` |
| `setEvalErrorLog(flag)` | static method | stores the flag and returns the previous one |

### `Window` members

Seventeen instance methods, three instance properties and twenty-four class
constants, registered in
`crates/compat/src/natives/plugins/window_ex/window.rs`, plus the
`Window._Notifications` dictionary.

| member | kind | behaviour |
|---|---|---|
| `minimize()` | instance method | accepted and inert |
| `maximize()` | instance method | accepted and inert |
| `showRestore()` | instance method | accepted and inert |
| `focusMenuByKey(key)` | instance method | reads its first argument, so it needs one; returns `void` |
| `resetWindowIcon()` | instance method | accepted and inert |
| `setWindowIcon(icon)` | instance method | accepted and inert |
| `getWindowRect()` | instance method | the window rectangle as `{x, y, w, h}` |
| `getClientRect()` | instance method | the client rectangle — the same body as `getWindowRect` |
| `getNormalRect()` | instance method | the restored rectangle — the same body as `getWindowRect` |
| `setOverlayBitmap(layer)` | instance method | stores the overlay layer on the window |
| `ncHitTest(x, y)` | instance method | the non-client hit test; needs two arguments; returns `void` |
| `setMessageHook(...)` | instance method | arms the message hook and returns the all-messages flag |
| `bringTo(other?)` | instance method | accepted and inert |
| `sendToBack()` | instance method | accepted and inert |
| `registerExEvent()` | instance method | probes the object for the four extended window handlers; returns `void` |
| `getNotificationNum(name)` | instance method | the message number for a notification name, `-1` when unknown |
| `getNotificationName(number)` | instance method | the notification name for a message number, `""` when unknown |
| `disableResize` | instance property (read/write) | the disable-resize flag |
| `enableNCMouseEvent` | instance property (read/write) | the non-client mouse-event flag |
| `exSystemMenu` | instance property (read/write) | the extended system menu object, `null` when unset |
| `_Notifications` | static property | the two-way notification name/value dictionary |

The class constants are the Win32 non-client hit-test codes
(`reference/cpp/plugins/windowEx.cpp:935-958`):

| constant | value | constant | value |
|---|---:|---|---:|
| `nchtError` | 65534 | `nchtMinButton` | 8 |
| `nchtTransparent` | 65535 | `nchtReduce` | 8 |
| `nchtNoWhere` | 0 | `nchtMaxButton` | 9 |
| `nchtClient` | 1 | `nchtZoom` | 9 |
| `nchtCaption` | 2 | `nchtLeft` | 10 |
| `nchtSysMenu` | 3 | `nchtRight` | 11 |
| `nchtSize` | 4 | `nchtTop` | 12 |
| `nchtGrowBox` | 4 | `nchtTopLeft` | 13 |
| `nchtMenu` | 5 | `nchtTopRight` | 14 |
| `nchtHScroll` | 6 | `nchtBottom` | 15 |
| `nchtVScroll` | 7 | `nchtBottomLeft` | 16 |
| | | `nchtBottomRight` | 17 |
| | | `nchtBorder` | 18 |

Members the reference *could* have registered are deliberately absent,
because the reference's own registrations are commented out
(`reference/cpp/plugins/windowEx.cpp:962-967`, `:976-977`, `:981-982`):
`Window.disableMove`, `Window.resetExSystemMenu`, `Window.maximized` /
`Window.minimized` and `Window.maximizeBox` / `Window.minimizeBox`.

### `MenuItem` members

One instance method, four instance properties and ten class constants,
registered in `crates/compat/src/natives/plugins/window_ex/menu_item.rs`.

| member | kind | behaviour |
|---|---|---|
| `popupEx(flags, x?, y?, hwnd?, rect?, menulist?)` | instance method | accepted and inert |
| `rightJustify` | instance property (read/write) | the right-justify flag; `0` or `1` |
| `bmpItem` | instance property (read/write) | the item bitmap state; `-1` for a stored bitmap, `0` otherwise |
| `bmpChecked` | instance property (read/write) | the checked bitmap state; `-1` for a stored bitmap, `0` otherwise |
| `bmpUnchecked` | instance property (read/write) | the unchecked bitmap state; `-1` for a stored bitmap, `0` otherwise |
| `biSystem` | static constant | `1` |
| `biRestore` | static constant | `2` |
| `biMinimize` | static constant | `3` |
| `biClose` | static constant | `5` |
| `biCloseDisabled` | static constant | `6` |
| `biMinimizeDisabled` | static constant | `7` |
| `biPopupClose` | static constant | `8` |
| `biPopupRestore` | static constant | `9` |
| `biPopupMaximize` | static constant | `10` |
| `biPopupMinimize` | static constant | `11` |

The `bi*` values are the Win32 `HBMMENU_*` codes
(`reference/cpp/plugins/windowEx.cpp:1220-1229`).

### `Console` members

Seven instance methods, registered in
`crates/compat/src/natives/plugins/window_ex/console.rs`. The `Console` class
itself is a core native (`crates/compat/src/natives/debug.rs`); the
`Debug.console` singleton is an instance of it, so the singleton carries
these members.

| member | kind | behaviour |
|---|---|---|
| `restoreMaximize()` | instance method | accepted and inert; returns `void` |
| `maximize()` | instance method | accepted and inert; returns `void` |
| `getRect()` | instance method | accepted and inert; returns `void` |
| `setPos(x, y)` | instance method | validates the argument count; returns `void` |
| `getPlacement()` | instance method | accepted and inert; returns `void` |
| `setPlacement(value)` | instance method | validates the argument count and the object type; returns `void` |
| `bringAfter()` | instance method | accepted and inert; returns `void` |

### `Pad` members

One instance method, registered in
`crates/compat/src/natives/plugins/window_ex/pad.rs`. The `Pad` class itself
is a core native (`crates/compat/src/natives/input.rs`).

| member | kind | behaviour |
|---|---|---|
| `registerExEvent()` | instance method | accepted and inert — the native pad window does not exist on this host |

## Behaviour

### Host facts and the host seam

The display monitors, the cursor position, the environment variables, the
system metrics, the icon, the double-click time, the CPU features and the
breathing mode are genuine host facts. The compat layer owns no platform
code, so each one goes through a closure seam
(`crates/compat/src/natives/plugins/window_ex/host.rs`): the installers are
inherent methods on `CompatHost`
(`set_window_ex_display_monitors`, `set_window_ex_cursor_pos`,
`set_window_ex_set_cursor_pos`, `set_window_ex_system_metrics`,
`set_window_ex_read_env`, `set_window_ex_expand_env`,
`set_window_ex_application_icon`, `set_window_ex_iconic_preview`,
`set_window_ex_double_click_time`, `set_window_ex_cpu_type`,
`set_window_ex_breathe`, `set_window_ex_is_breathing`).

A member whose fact is not installed raises a typed TJS error naming the
missing provider. The application installs emulated Windows facts on the host
at startup (`crates/app/src/window_ex.rs`): one primary monitor covering the
render surface, the cursor at the origin, the process environment, the
common `SM_*` metrics, a 500 ms double-click time, a CPU type of `0` and
"not breathing".

Three members keep a reference literal instead of asking the host, because
the reference's own body is a literal and not a query:

* `getDoubleClickTime` reads `-1`
  (`reference/cpp/plugins/windowEx.cpp:1422`);
* `getCPUType` reads the `tjs_uint32` CPU-feature bitmask as an integer,
  defaulting to `0` (`reference/cpp/plugins/windowEx.cpp:1527`,
  `reference/cpp/core/environ/DetectCPU.cpp:172-175`);
* `getSystemMetrics` first performs the reference's `System.metrics`
  dictionary lookup (`reference/cpp/plugins/windowEx.cpp:1466-1481`) before
  consulting the host.

`getSystemMetrics` therefore always *creates* the `System.metrics`
dictionary on the `System` class if it is missing — a `TJS_MEMBERENSURE`
write a script can observe afterwards
(`reference/cpp/plugins/windowEx.cpp:1470-1476`) — and reads the
**uppercased** key from it (`reference/cpp/plugins/windowEx.cpp:1458`). A
non-negative dictionary value answers the call; a missing or negative one
falls through to the host.

### The monitor shape

`getDisplayMonitors` returns an array of dictionaries. Each entry carries a
`primary` flag and two sub-dictionaries, `monitor` and `work`, each of
`{x, y, w, h}`: `monitor` is the full bounds of the display and `work` is
its work area. The shape is the one the shipped `k2compat` shims read
(`k2compat/k2compat_deskinfo.tjs:22-25,43-46`,
`k2compat/k2compat_scrinfo.tjs:5-6`); the vendored reference body is a bare
`return TJS_S_OK` (`reference/cpp/plugins/windowEx.cpp:1424-1428`) and so
cannot settle it.

`getMonitorInfo` selects one monitor and returns the same dictionary shape.
Its calling convention is the one of the shipped Windows `windowEx.dll`:
`()` for the primary monitor, `(flag, window)`, `(flag, x, y)` and
`(flag, x, y, w, h)`; every other argument count raises. The first argument
is the `MonitorFrom*` fallback flag — truthy selects
`MONITOR_DEFAULTTONEAREST`, falsy `MONITOR_DEFAULTTONULL` — so an
off-screen probe with a falsy flag finds no monitor and returns `void`,
while a truthy flag falls back to the nearest one. The selection is resolved
against the same list `getDisplayMonitors` returns.

### The emulated Windows facilities

Several members' reference behaviour is a Windows facility — a window
message, a menu, a system metric, a window placement. They are **emulated**,
not refused:

* the window actions (`minimize`, `maximize`, `showRestore`,
  `resetWindowIcon`, `setWindowIcon`, `bringTo`, `sendToBack`) are accepted
  and do nothing. Their reference bodies are `return TJS_S_OK` without
  reading their arguments or writing a result
  (`reference/cpp/plugins/windowEx.cpp:364-397`, `:623-649`), so the
  reference itself is inert off Windows and the observable result — `void` —
  matches;
* `focusMenuByKey` reads its first argument unconditionally
  (`reference/cpp/plugins/windowEx.cpp:379-383`), so a call without one
  raises;
* `ncHitTest` validates its argument count
  (`if(n < 2) return TJS_E_BADPARAMCOUNT;`,
  `reference/cpp/plugins/windowEx.cpp:586-594`) and returns `void`;
* `setMessageHook` arms the hook. With one argument it assigns the
  all-messages flag and returns it
  (`setMessageHookAll`, `reference/cpp/plugins/windowEx.cpp:749`); with a
  second argument it validates the message number (`0..0x400`, outside which
  the reference raises `TJS_E_FAIL`,
  `reference/cpp/plugins/windowEx.cpp:611-612`) and returns the
  all-messages flag *unchanged*, storing nothing
  (`setMessageHookOnel`, `reference/cpp/plugins/windowEx.cpp:747`). The
  first argument is coerced with `!!value.AsInteger()`
  (`reference/cpp/plugins/windowEx.cpp:601`);
* `registerExEvent` probes the script object for `onResizing`, `onMoving`,
  `onMove` and `onNcMouseMove` (`reference/cpp/plugins/windowEx.cpp:730-736`,
  the `EXEV_*` names at `:316-340`) and returns `void`. The reference keeps
  the four flags and fires the handlers from the window procedure; the port
  has no window message dispatcher, so the probe results are not retained.
  None of the `EXEV_*` names is registered as a class member — the reference
  registers none of them either, it looks a handler up on the script object
  when it needs to fire one (`reference/cpp/plugins/windowEx.cpp:657-661`);
* `exSystemMenu` stores the object it is given and hands it back; the
  reference additionally rewrites the Win32 system menu
  (`reference/cpp/plugins/windowEx.cpp:547-559`, `:1277-1286`). `null` is
  accepted and clears the slot; a scalar raises the variant conversion
  error;
* `setOverlayBitmap` stores the layer. The reference creates a Win32 overlay
  window from it (`reference/cpp/plugins/windowEx.cpp:768-781`, `:900-916`);
  the port keeps the layer with the window and composites nothing. A
  non-object argument never enters the reference's body and is a silent
  success, while `null` and non-`Layer` objects reach
  `OverlayBitmap::setBitmap`, which rejects them, so they raise;
* the `Console` members are inert for the same reason
  (`reference/cpp/plugins/windowEx.cpp:1290-1321`). Their reference bodies
  only `return TJS_S_OK`, which leaves the result slot untouched, so they
  read back as `void`. `setPos` validates the argument count
  (`if(n < 2) return TJS_E_BADPARAMCOUNT;`,
  `reference/cpp/plugins/windowEx.cpp:1306-1311`) and `setPlacement`
  validates the count and that the argument is an object
  (`if(p[0]->Type() != tvtObject) return TJS_E_INVALIDPARAM;`,
  `reference/cpp/plugins/windowEx.cpp:1323-1331`);
* `MenuItem.popupEx` is an empty `TJS_S_OK` stub
  (`reference/cpp/plugins/windowEx.cpp:1185-1188`), so it accepts any
  arguments and returns `void`. The `bi*` constants name the Win32 menu
  bitmaps the popup would use;
* `Pad.registerExEvent` has an empty body
  (`reference/cpp/plugins/windowEx.cpp:1396`), so it accepts any arguments
  and returns `void`.

### The per-object flags

`Window.disableResize`, `Window.enableNCMouseEvent` and
`MenuItem.rightJustify` are boolean flags. Their writes use the reference's
`!!value.AsInteger()` coercion
(`reference/cpp/plugins/windowEx.cpp:507`, `:581`, `:1084-1088`): a real
truncates toward zero, a string parses a leading number, `void` reads `0`
and an object — `null` included — raises the variant conversion error
(`reference/cpp/core/tjs2/tjsVariant.h:862-878`).

`MenuItem.bmpItem`, `MenuItem.bmpChecked` and `MenuItem.bmpUnchecked` follow
the reference's type switch
(`reference/cpp/plugins/windowEx.cpp:1104-1127`): an object-typed value
(including `null`) stores the bitmap state and reads back as `-1`, while
`void`, an integer and a string store the system state and a real falls
through the switch — all three read back as `0`.

`Window.exSystemMenu` is not a flag: it stores the object it is given, and
its setter accepts an object or `null`
(`reference/cpp/plugins/windowEx.cpp:547-559`).

### The notification table

`Window._Notifications` is the two-way Win32 message name/number
dictionary. The reference builds it lazily from the `WM(…)` macro list
(`reference/cpp/plugins/windowEx.cpp:800-871`); the macro stringizes the
bare token with `#key` (`reference/cpp/plugins/windowEx.cpp:801`), so the
keys carry no `WM_` prefix: `WM(NULL)` writes `"NULL" => 0` and
`0 => "NULL"`. The port keeps the same ordered table of 227 entries, so a
value that appears twice resolves in the reverse direction to the *last*
name the reference would have written — `KEYDOWN` for 256, `SETTINGCHANGE`
for 26, `MOUSELAST` for 526, `KEYLAST` for 265.

The dictionary is installed on the `Window` class object at registration
(`reference/cpp/plugins/windowEx.cpp:874-876`).

`getNotificationNum(name)` and `getNotificationName(number)` read the same
table. The name lookup is case-sensitive — the reference's
`getIntValue(key, -1)` compares member names with `strcmp`
(`reference/cpp/core/tjs2/tjsObject.h:331-336`) — so a differently-cased or
`WM_`-prefixed name misses and reads `-1`. The number lookup converts its
argument with the reference's `AsInteger` rules
(`reference/cpp/core/tjs2/tjsVariant.h:862-878`), so a string parses a
leading number and an object raises.

### The `Scripts.eval` override

`Scripts.eval` replaces the core member so that an evaluation failure is
reported through the configured error log. `setEvalErrorLog(flag)` stores
the flag and returns the previous one
(`reference/cpp/plugins/windowEx.cpp:1535-1539`); the default is `true`
(`reference/cpp/plugins/windowEx.cpp:1577`).

While the flag is set, the override evaluates like the core member — the
`name`, the `lineofs` and the `context` included — and logs a failure.
Once it is cleared, the evaluation keeps the `name` and the `lineofs` but
runs without a context and without the log
(`reference/cpp/plugins/windowEx.cpp:1544-1556`). The flag only controls the
logging: an evaluation error raises in both states.

## State and lifetime

* The per-window flag set (`disableResize`, `enableNCMouseEvent`,
  `exSystemMenu`, the message-hook flag, the overlay layer) lives in
  `window::WindowEx`; the per-item flag set (`rightJustify`, `bmpItem`,
  `bmpChecked`, `bmpUnchecked`) lives in `menu_item::MenuItemExState`. Both
  are maps keyed by the `objthis` pointer, because the compat VM hands an
  objthis-only member the raw receiver and no native payload.
* The reference gives every object **its own** native instance and destroys
  it with the object (`reference/cpp/plugins/windowEx.cpp:920-931`,
  `:1191-1199`), so a released object's address being reused must not carry
  the previous occupant's flags. The port detects the reuse with an identity
  token stamped on the object itself (`__windowExToken`, see
  `crates/compat/src/natives/plugins/window_ex/util.rs`): the first access
  claims a fresh token and resets the entry to the constructor defaults, a
  later access with a matching token keeps the state.
* The `Scripts` error-log flag lives in the per-engine dispatch state
  (`ScriptsExState`), so it is per engine and not process-global.
* The host-fact closures live on the host (`WindowExHost`), not in a
  process global.
* The notification table is a compile-time constant; the
  `Window._Notifications` dictionary is built once and installed on the
  `Window` class object at registration.

## Errors

| condition | outcome |
|---|---|
| a host-backed member is called before the host installed the provider | typed error naming the missing provider |
| `System.setCursorPos` with fewer than two arguments | typed error |
| `System.getSystemMetrics` with no argument, a non-string argument or an empty name | typed error |
| `System.readEnvValue` with no argument, a non-string argument or an empty name | typed error |
| `System.expandEnvString` with no argument | typed error |
| `System.setIconicPreview` with no argument | typed error |
| `System.getMonitorInfo` with an argument count other than 0, 2, 3 or 5 | typed error |
| `Window.ncHitTest` with fewer than two arguments | typed error |
| `Window.focusMenuByKey` with no argument | typed error |
| `Window.setMessageHook` with a message number outside `0..0x400` | typed error (`message number out of range`) |
| `Window.setOverlayBitmap` with `null` or a non-`Layer` object | typed error (`the argument must be a Layer`) |
| `Window.exSystemMenu` with a scalar | typed error (`the value must be an object or null`) |
| `Window.getNotificationNum` / `Window.getNotificationName` with no argument | typed error |
| `Console.setPos` with fewer than two arguments | typed error |
| `Console.setPlacement` with no argument, or with a non-object argument | typed error |
| a boolean flag write (`disableResize`, `enableNCMouseEvent`, `rightJustify`) with an object-typed value (including `null`) | typed error (the variant conversion error) |
| the compat context is not bound | typed error |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/window_ex.rs` | the whole plugin surface: the `System` members and their argument validation, the `System.metrics` lookup, the host seam before and after a provider is installed, the `getMonitorInfo` calling convention, the `Window` rectangles and flags, the `exSystemMenu` object lifecycle, `setOverlayBitmap`, the message-hook flag, the notification table and its two lookups, the `MenuItem` members, the `Console` members, the `Pad` member and the `Scripts.eval` override |
| `crates/compat/tests/dispatch_ui.rs` | that the objthis-keyed flag maps reach the right receiver — `window_ex_window_flags_reach_the_target` and `window_ex_menu_item_flags_reach_the_target` — and that `System.getDoubleClickTime` is the static `-1` literal |
| `crates/compat/tests/menu_dll_surface.rs` | that the `MenuItem` members the `windowEx` port adds are exactly the `bi*` constants, the four instance properties and `popupEx`, and that they are the only members outside the `menu.dll` fixture |
| `crates/compat/tests/get_about.rs` | that `System.getAboutString` — owned by this port — is attached to the `System` class |
| `crates/app/src/window_ex.rs` | the emulated host facts the application installs: the primary monitor, the desktop rectangle, the `SM_*` metrics and the `%NAME%` expansion |

The plugin-parity report
([`docs/plugin_parity.md`](../../../plugin_parity.md)) lists every class this
plugin touches at full coverage, with no missing and no extra member. The
`Window` (44), `MenuItem` (15), `Console` (7) and `Pad` (1) figures are
exactly the members listed above; the `System` figure (16) adds the
`clearGraphicCache` the reference's `windowEx` also attaches — the core
`System` class owns that member in the port — and the `addFont` member of
the `addFont` port. The `Scripts` row also covers the `ScriptsEx` port.

## Known gaps

* **The window-management members are inert.** `minimize`, `maximize`,
  `showRestore`, `resetWindowIcon`, `setWindowIcon`, `bringTo`, `sendToBack`,
  `focusMenuByKey` and `ncHitTest` are accepted and do nothing. The
  reference's own bodies never reach the Win32 window off Windows
  (`reference/cpp/plugins/windowEx.cpp:364-397`, `:623-649`), so the
  observable result matches, but a game written for Windows gets no window
  management from them.
* **No window message dispatch.** `setMessageHook` arms the all-messages
  flag, but nothing ever delivers a Win32 message to the script, so
  `onWindowsMessageHook` and the `EXEV_*` handlers never fire. The
  reference registers a message receiver with the window
  (`reference/cpp/plugins/windowEx.cpp:709-718`). `registerExEvent` probes
  the four handler names and discards the result
  (`reference/cpp/plugins/windowEx.cpp:730-736`).
* **The menus and the overlay are stored, not applied.** `exSystemMenu` keeps
  the object the script gives it but rewrites no system menu
  (`reference/cpp/plugins/windowEx.cpp:1277-1286`); `setOverlayBitmap` keeps
  the layer but composites no overlay
  (`reference/cpp/plugins/windowEx.cpp:900-916`); `MenuItem.popupEx` shows
  no popup; the `bi*` constants name bitmaps no menu ever draws.
* **The console placement is inert.** The `Console` members validate their
  arguments and return `void`; the reference's console window is never
  moved or resized (`reference/cpp/plugins/windowEx.cpp:1290-1331`).
* **The vendored reference bodies are stubs where the port answers.**
  `getDisplayMonitors`, `getMonitorInfo`, `getCursorPos`, `readEnvValue`,
  `expandEnvString` and `getSystemMetrics` are bare `return TJS_S_OK` in
  `reference/cpp/plugins/windowEx.cpp:1424-1439` and `:1448-1502`, so the
  reference returns `void` for them, while the port returns the host's
  values. The shapes the port uses come from the shipped `k2compat` shims
  (`k2compat/k2compat_deskinfo.tjs:22-25,43-46`,
  `k2compat/k2compat_scrinfo.tjs:5-6`) and from the `getMonitorInfo`
  calling convention of the shipped Windows `windowEx.dll`.
* **`getSystemMetrics` returns the metric.** The reference computes the
  value but never writes it into the result slot
  (`reference/cpp/plugins/windowEx.cpp:1478-1482`), so it returns `void`;
  the port returns the integer.
* **A non-numeric `System.metrics` value misses.** The reference's
  `getIntValue` converts any variant to an integer, so a numeric string in
  the dictionary is read as a number
  (`reference/cpp/core/plugin/ncbind.hpp:689-695`); the port reads only
  integer and real values, so such a key misses and the call falls through
  to the host.
* **The CPU feature bitmask and the double-click time are literals.** They
  read `0` and `-1` until a host installs the real values
  (`reference/cpp/plugins/windowEx.cpp:1422`, `:1527`), because the
  reference's own bodies are literals rather than queries.
* **`getAboutString` returns the port's own text** — `"krkr-rs windowEx
  compat "` plus the crate version — not the reference engine's about
  string (`reference/cpp/plugins/windowEx.cpp:1526`).
* **`breathe` and `isBreathing` are host facts.** The reference attaches the
  core engine's breathing mode (`reference/cpp/plugins/windowEx.cpp:1523-1524`);
  the port routes them through the host seam, and the application answers
  "not breathing".
* **The `Console` members are class members.** The reference attaches them
  to the `Debug.console` singleton only
  (`reference/cpp/plugins/windowEx.cpp:1333-1346`); the port attaches them
  to the `Console` class, so `new Console()` carries them too.
* **`System.clearGraphicCache` is not registered again.** The reference
  attaches it (`reference/cpp/plugins/windowEx.cpp:1525`), but the core
  `System` class already owns the member, and the VM rejects a duplicate
  registration.
