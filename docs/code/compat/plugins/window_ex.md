# windowEx (`windowEx.dll`)

**DLL name:** `windowEx.dll` — linked with `Plugins.link("windowEx.dll")`
**Rust module:** `crates/compat/src/natives/plugins/window_ex/`
**Reference:** `reference/cpp/plugins/windowEx.cpp`
**Registers:** members on `System`, `Scripts`, `Window`, `MenuItem`, `Console` and `Pad`, plus `Window._Notifications`

## Surface

The reference plugin has two halves: it attaches members to existing classes,
and it provides the `WindowEx` implementation the reference hooks onto
`Window` (`NCB_ATTACH_CLASS_WITH_HOOK(WindowEx, Window)`).

The compat ABI has no subclassing and `register_instance_members` attaches
objthis-only members (no per-object native payload) to an existing class, so
— exactly like the reference's own `registerExEvent` — the `WindowEx` members
live on `Window` and `MenuItemEx`'s on `MenuItem`. There is no `WindowEx`
global class, matching the reference.

### `System` static methods

| member | kind | behaviour |
|---|---|---|
| `getDisplayMonitors()` | static method | the monitor layout: a `primary` flag plus `monitor` and `work` sub-dictionaries, each `{x, y, w, h}` |
| `getMonitorInfo(...)` | static method | the information for a monitor, by `MonitorFrom*` argument |
| `getCursorPos()` | static method | the cursor position |
| `setCursorPos(x, y)` | static method | moves the cursor |
| `getSystemMetrics(index)` | static method | a system metric; the `System.metrics` dictionary is consulted first |
| `readEnvValue(name)` | static method | an environment variable's value |
| `expandEnvString(text)` | static method | expands environment variables in a string |
| `setApplicationIcon(bitmap)` | static method | sets the application icon |
| `setIconicPreview(bitmap)` | static method | sets the iconic (minimized) preview image |
| `getDoubleClickTime()` | static method | the reference literal `-1` |
| `breathe()` | static method | the reference's breathing-mode toggle |
| `isBreathing()` | static method | whether the breathing mode is on |
| `getAboutString()` | static method | the about string — also the member the `getabout` port would register (see [`get_about`](get_about.md)) |
| `getCPUType()` | static method | the `tjs_uint32` CPU-feature bitmask as an integer, defaulting to `0` |

### `Scripts` static methods

| member | kind | behaviour |
|---|---|---|
| `eval(content[, name[, lineofs[, context]]])` | static method | the error-log-aware override of the core `eval` |
| `setEvalErrorLog(flag)` | static method | stores the flag and returns the previous one |

### `MenuItem` members

| member | kind | behaviour |
|---|---|---|
| `popupEx(flags, x?, y?, hwnd?, rect?, menulist?)` | instance method | accepted and inert — the reference body is an empty `TJS_S_OK` stub |
| `rightJustify` | instance property (read/write) | the right-justify flag |
| `bmpItem` | instance property (read/write) | the item bitmap |
| `bmpChecked` | instance property (read/write) | the checked bitmap |
| `bmpUnchecked` | instance property (read/write) | the unchecked bitmap |
| `biSystem` | static constant | the system-menu identifier |
| `biRestore` | static constant | the restore identifier |
| `biMinimize` | static constant | the minimize identifier |
| `biClose` | static constant | the close identifier |
| `biCloseDisabled` | static constant | the disabled-close identifier |
| `biMinimizeDisabled` | static constant | the disabled-minimize identifier |
| `biPopupClose` | static constant | the popup close identifier |
| `biPopupRestore` | static constant | the popup restore identifier |
| `biPopupMaximize` | static constant | the popup maximize identifier |
| `biPopupMinimize` | static constant | the popup minimize identifier |

### `Console` (`Debug.console`) members

| member | kind | behaviour |
|---|---|---|
| `restoreMaximize` | instance method | accepted and inert |
| `maximize` | instance method | accepted and inert |
| `getRect` | instance method | accepted and inert |
| `setPos(x, y)` | instance method | validates the argument count |
| `getPlacement` | instance method | accepted and inert |
| `setPlacement(value)` | instance method | validates the argument count and the object type |
| `bringAfter` | instance method | accepted and inert |

### `Pad` members

| member | kind | behaviour |
|---|---|---|
| `registerExEvent` | instance method | accepted and inert — the native pad window does not exist on this host |

### `Window` members

| member | kind | behaviour |
|---|---|---|
| `minimize` | instance method | accepted and inert |
| `maximize` | instance method | accepted and inert |
| `showRestore` | instance method | accepted and inert |
| `focusMenuByKey(key)` | instance method | focuses a menu by key |
| `resetWindowIcon` | instance method | accepted and inert |
| `setWindowIcon(bitmap)` | instance method | accepted and inert |
| `getWindowRect()` | instance method | the window rectangle |
| `getClientRect()` | instance method | the client rectangle |
| `getNormalRect()` | instance method | the restored (normal) rectangle |
| `disableResize` | instance property (read/write) | the disable-resize flag |
| `setOverlayBitmap(bitmap)` | instance method | sets the overlay bitmap |
| `exSystemMenu` | instance property (read/write) | the extended system menu flag |
| `enableNCMouseEvent` | instance property (read/write) | the non-client mouse-event flag |
| `ncHitTest(x, y)` | instance method | the non-client hit test |
| `setMessageHook(...)` | instance method | installs a message hook |
| `bringTo` | instance method | accepted and inert |
| `sendToBack` | instance method | accepted and inert |
| `registerExEvent` | instance method | accepted and inert |
| `getNotificationNum(name)` | instance method | the message number for a name |
| `getNotificationName(num)` | instance method | the message name for a number |
| `nchtError` … `nchtBorder` | static constants | the non-client hit-test codes, `65534` … `18` |
| `_Notifications` | static property | the two-way notification name/value dictionary |

## Behaviour

### The Win32 environment is emulated

Several members' reference behaviour is a Windows facility — a window
message, a menu, a system metric. They are **emulated**, not refused:

* the inert members (`minimize`, `maximize`, `showRestore`,
  `resetWindowIcon`, `setWindowIcon`, `bringTo`, `sendToBack`) are accepted
  and do nothing, matching the reference, whose bodies never reach the Win32
  window off Windows
  (`reference/cpp/plugins/windowEx.cpp:364-397,623-650`) and never read
  their arguments;
* the `Console` members are inert for the same reason
  (`reference/cpp/plugins/windowEx.cpp:1290-1321`); their reference bodies
  only `return TJS_S_OK`, which leaves the result slot untouched, so they
  read back as `void`;
* `setPos` validates the argument count (`if(n < 2) return
  TJS_E_BADPARAMCOUNT;`, `reference/cpp/plugins/windowEx.cpp:1306-1311`) and
  `setPlacement` validates the count and that the argument is an object
  (`if(p[0]->Type() != tvtObject) return TJS_E_INVALIDPARAM;`) — the
  reference stores nothing either way.

### The host seam

The display monitors, the cursor position, the environment variables and the
remaining system metrics are genuine host facts. They go through the closure
seam defined in `host.rs` — the installers are inherent methods on
`CompatHost` — and raise a typed TJS error until the host installs one.

Three members keep a reference literal instead, because the vendored
reference body is not a host query:

* `getDoubleClickTime` reads `-1`
  (`reference/cpp/plugins/windowEx.cpp:1422`);
* `getCPUType` reads the `tjs_uint32` CPU-feature bitmask as an integer,
  defaulting to `0` (`reference/cpp/core/base/DetectCPU.cpp:170-175`);
* `getSystemMetrics` first performs the reference's `System.metrics`
  dictionary lookup (`reference/cpp/plugins/windowEx.cpp:1466-1481`) before
  consulting the host. A missing key reads as no value, so the caller falls
  through to the host seam; a key that exists but holds a non-integer raises
  `TJS_E_INVALIDPARAM`
  (`reference/cpp/plugins/windowEx.cpp:1478-1481`).

`getDisplayMonitors` returns the monitor layout: a `primary` flag plus
`monitor` and `work` sub-dictionaries, each `{x, y, w, h}`. The work
rectangle is derived from the platform's work bounds.

### The notification table

`Window._Notifications` is the two-way Win32 message name/number dictionary.
The reference builds it lazily from the `WM(…)` macro list in
`reference/cpp/plugins/windowEx.cpp` (`:800-871`). The macro stringizes the
bare token with `#key` (`:801`), so the dictionary keys carry no `WM_`
prefix: `WM(NULL)` writes `"NULL" => 0` and `0 => "NULL"`. The port keeps the
same ordered table, so the reverse lookup resolves a duplicated value
(`KEYFIRST`/`KEYDOWN`, `MOUSEFIRST`/`MOUSEMOVE`, …) to the *last* name the
reference would have written.

`getNotificationNum(name)` and `getNotificationName(num)` read that table.

### The `Scripts.eval` override

`Scripts.eval` overrides the core `eval` so that a failure is reported
through the configured error log. `setEvalErrorLog(flag)` stores the flag and
returns the previous one.

## State and lifetime

* The small per-object flag sets — the `Window` flags (`disableResize`,
  `exSystemMenu`, `enableNCMouseEvent`) and the `MenuItem` flags
  (`rightJustify`, `bmpItem`, `bmpChecked`, `bmpUnchecked`) — live in
  `window::WindowEx` and `menu_item::MenuItemExState`, keyed by the
  `objthis` pointer. They are removed when the object is finalized.
* The `Scripts.eval` error-log flag lives in the per-engine dispatch state.
* The notification table is a constant table; the `Window._Notifications`
  dictionary is installed on the class object once at registration.
* The host seam closures live on the host, not in a process global.

## Errors

| condition | outcome |
|---|---|
| a host-backed member is called before the host installed the seam | typed error |
| `getSystemMetrics` with a `System.metrics` key that holds a non-integer | `TJS_E_INVALIDPARAM` |
| `Console.setPos` with fewer than two arguments | `TJS_E_BADPARAMCOUNT` |
| `Console.setPlacement` with no argument, or with a non-object | `TJS_E_BADPARAMCOUNT` / `TJS_E_INVALIDPARAM` |
| `getMonitorInfo` with too few arguments | `TJS_E_BADPARAMCOUNT` |
| the compat context is not bound | typed error |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/window_ex.rs` | the `System` members and the host seam, the `System.metrics` lookup, the `Window` flags and rectangles, the notification table and the two lookups, the `MenuItem` members, the `Console` members, and the `Pad` member |
| `crates/compat/tests/misc_plugins.rs` | the plugin-shim behaviour that shares the file with the other small plugins |

## Known gaps

* The reference's members that reach a real Win32 window (window state,
  menus, message hooks, the non-client hit test) are inert or emulated here.
  The reference itself is inert off Windows
  (`reference/cpp/plugins/windowEx.cpp:364-397,623-650`), so a Linux or
  Android host sees the same `void` results the reference would produce
  there — but a game written for Windows still gets no window management.
* The host-backed members raise a typed error until the host installs the
  seam; a host that does not install it cannot answer those queries.
* `getDoubleClickTime` and `getCPUType` return the reference's literals
  rather than a host fact, because the reference's own bodies are literals.
