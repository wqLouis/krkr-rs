# krkrsteam (`krkrsteam.dll`)

**DLL name:** `krkrsteam.dll` (aliases: `steam_api.dll`, `DrawDeviceForSteam.dll`) — linked with `Plugins.link("krkrsteam.dll")`
**Rust module:** `crates/compat/src/natives/plugins/steam/`
**Reference:** `reference/cpp/plugins/steam/steamPlugin.cpp`, `reference/cpp/plugins/steam/steam_api.cpp`, `reference/cpp/plugins/DrawDeviceForSteam/DrawDeviceForSteam.cpp`
**Registers:** the `Steam`, `steam_api` and `DrawDeviceForSteam` classes

The three DLL names are one port: the built-in table entry
(`crates/compat/src/natives/plugins/mod.rs`) lists `krkrsteam.dll` with
`steam_api.dll` and `DrawDeviceForSteam.dll` as aliases, and the one
`register` installs all three classes. Linking any of the three names
resolves to the built-in, so none of them is absent. The table and the
linking rules are on the
[plugin index](../plugins.md); the member counts are in the
[parity report](../../../plugin_parity.md).

## Surface

### `Steam`

Every member is a **static** member, attached to the class object: the
reference registers the callbacks with the `TJS_STATICMEMBER` flag and
declares the property getters as static member functions.

| member | kind | behaviour |
|---|---|---|
| `Steam.getAchievement(n)` | static method | the mock achievement display name `"Achievement 1"` |
| `Steam.setAchievement(n)` | static method | always succeeds, returns `1` |
| `Steam.clearAchievement(n)` | static method | always succeeds, returns `1` |
| `Steam.getLanguage()` | static method | `"English"` |
| `Steam.requestInitialize()` | static method | always succeeds, returns `1` |
| `Steam.getCloudQuota()` | static method | `1073741824` (1 GiB) |
| `Steam.getCloudFileInfo(n)` | static method | the mock file name `"file1.txt"` |
| `Steam.deleteCloudFile(name)` | static method | always succeeds, returns `1` |
| `Steam.copyCloudFile(src, dest)` | static method | always succeeds, returns `1` |
| `Steam.triggerScreenshot()` | static method | does nothing, returns `void` |
| `Steam.hookScreenshots(callback)` | static method | does nothing, returns `void` |
| `Steam.writeScreenshot(layer, location)` | static method | does nothing, returns `void` |
| `Steam.isBroadcasting()` | static method | `0` |
| `Steam.hookBroadcasting(callback)` | static method | does nothing, returns `void` |
| `Steam.isIsSubscribedApp(appID)` | static method | `1` |
| `Steam.ssDlcInstalled(appID)` | static method | `1` |
| `Steam.getDLCCount()` | static method | `2` |
| `Steam.getDLCData(no)` | static method | the mock DLC name `"DLC1"` |
| `Steam.initialized` | static property (read-only) | `1` |
| `Steam.achievementsCount` | static property (read-only) | `0` |
| `Steam.cloudEnabled` | static property (read-only) | `1` |
| `Steam.cloudFileCount` | static property (read-only) | `0` |
| `Steam.dlcCount` | static property (read-only) | `0` |
| `Steam()` | constructor | **raises** — the reference registers no constructor |

The reference's registration block
(`reference/cpp/plugins/steam/steamPlugin.cpp:218-253`) is the whole
surface: eighteen `RawCallback`s and five `Property`s. The C++ helpers
`getInitialized`, `setInitialized`, `getAchievementsCount`,
`setAchievementsCount`, `getCloudEnabled`, `setCloudEnabled`,
`getCloudFileCount` and `_getDlcsCount`
(`reference/cpp/plugins/steam/steamPlugin.cpp:53-63,103-107,129,201`) are
only the property getters and setters behind the registered names, so they
are not script members under their own names.

### `steam_api`

| member | kind | behaviour |
|---|---|---|
| `steam_api()` | constructor | allocates an empty instance |

`steam_api` is the reference's trivial empty class: it registers only a
constructor (`reference/cpp/plugins/steam/steam_api.cpp:12`). The plain C++
field `vserion` (`:9`) is never bound to TJS, so the port registers an
empty instance class with no script-visible members.

### `DrawDeviceForSteam`

| member | kind | behaviour |
|---|---|---|
| `DrawDeviceForSteam()` | constructor | allocates the empty proxy |
| `interface` | property (read-only) | the draw device as an integer |

## Behaviour

### The mock `Steam` class

The reference `Steam` class links no `libsteam_api`: every member returns a
hard-coded value or does nothing, and the port reproduces those values one
for one.

* the string members return `"Achievement 1"`
  (`reference/cpp/plugins/steam/steamPlugin.cpp:65-75`), `"English"`
  (`:94-100`), `"file1.txt"` (`:120-128`) and `"DLC1"` (`:203-209`);
* the numeric members return `1` for `setAchievement`, `clearAchievement`,
  `requestInitialize`, `deleteCloudFile` and `copyCloudFile`
  (`:46-52,77-91,131-145`), `1` for `isIsSubscribedApp` and
  `ssDlcInstalled` (`:178-192`), `2` for `getDLCCount` (`:194-200`), `0` for
  `isBroadcasting` (`:164-170`) and the 1 GiB total for `getCloudQuota`
  (`:109-118`);
* the three screenshot members and `hookBroadcasting` never assign a result,
  so they return `void` (`:148-161,172-175`).

The result keeps the reference's variant type: `*r = int(...)` is a TJS
**Integer**, `*r = "..."` is a **String**, and a callback that never assigns
`*r` leaves the result **void**. Nothing on `Steam` is ever a `Real` — the
property getters return `int` or `bool`, and a `bool` is stored as the
integer `1` (`reference/cpp/core/tjs2/tjsVariant.h:617-620`).

The callbacks never look at their arguments
(`reference/cpp/plugins/steam/steamPlugin.cpp:46-47,65-66,94-95,120-121,203-204`),
so the port ignores them too: `getAchievement()` succeeds without its
argument, and extra arguments change nothing.

The five properties are registered through ncbind's getter-only `Property`
overload (`reference/cpp/plugins/steam/steamPlugin.cpp:226-230`), which
stores a null setter (`reference/cpp/core/plugin/ncbind.hpp:1727`), so a
write answers `TJS_E_ACCESSDENIED`
(`reference/cpp/core/plugin/ncbind.hpp:1477`) and leaves the getter's value
untouched.

The class has no constructor: the reference's registration block declares
none, so ncbind installs its dummy constructor, which returns
`TJS_E_NOTIMPL` (`reference/cpp/core/plugin/ncbind.hpp:1948,1951-1955`).
`new Steam()` therefore raises in the port as well.

### `steam_api`

The class exists so a script can write `new steam_api()`. It carries no
state and no members; the reference's only binding is its constructor
(`reference/cpp/plugins/steam/steam_api.cpp:12`).

### `DrawDeviceForSteam`

`DrawDeviceForSteam` is a transparent wrapper around the main window's draw
device. Its `interface` property returns that device as an integer
(`reference/cpp/plugins/DrawDeviceForSteam/DrawDeviceForSteam.cpp:21-24`,
registered at `:412-415`). This host has no draw-device object, so the
getter reports the stable native payload address as a non-zero synthetic
pointer: a script that only checks the value is non-zero sees the same
shape.

## State and lifetime

* The `Steam` class carries an empty instance payload; the whole mock
  surface lives on the class object, not per instance.
* `steam_api` carries an empty instance payload.
* `DrawDeviceForSteam` carries a marker payload whose address the
  `interface` getter reports. The pointer is stable for the lifetime of the
  object and is never dereferenced — it is an opaque token, not a handle
  back into a device.
* A returned string is copied into a per-engine scratch buffer that the ABI
  trampoline drains immediately after the callback returns.

## Errors

| condition | outcome |
|---|---|
| `new Steam()` | typed error: `Steam: the class has no constructor` — the reference answers `TJS_E_NOTIMPL` |
| `Steam.initialized = 1` (and the other four properties) | typed error — the reference answers `TJS_E_ACCESSDENIED` |
| the compat context is not bound | typed error: `Steam.<member>: compat context is not bound` |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/steam.rs` | every mock member's value and result type; that the members ignore their arguments; that the five properties are read-only; that the unregistered C++ helpers are not script members; that `new Steam()` raises; that `steam_api` instantiates with no members; that `DrawDeviceForSteam.interface` is a stable non-zero pointer and is read-only; and that the complete reference member set is registered |
| `crates/compat/tests/plugin.rs` | that `krkrsteam.dll` and its two aliases resolve to the built-in table |

## Known gaps

* The `Steam` class is a **mock**, not a Steamworks client. The reference
  links no `libsteam_api` either, so every value here is the reference's
  own hard-coded answer — a game that expects real achievements, cloud
  files or broadcasting state sees the same fixed values it would see on the
  reference.
* `DrawDeviceForSteam.interface` reports a synthetic non-zero pointer
  instead of a real draw-device address, because this host has no
  draw-device object. The reference's value is the main window's draw
  device; only the "non-zero integer" shape survives the port.
* The reference's `steam_api` class exposes no member, and neither does the
  port — the misspelled `vserion` field stays in C++.
