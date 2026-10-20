# krkrsteam (`krkrsteam.dll`)

**DLL name:** `krkrsteam.dll` (aliases: `steam_api.dll`, `DrawDeviceForSteam.dll`) — linked with `Plugins.link("krkrsteam.dll")`
**Rust module:** `crates/compat/src/natives/plugins/steam/`
**Reference:** `reference/cpp/plugins/steam/steamPlugin.cpp`, `reference/cpp/plugins/steam/steam_api.cpp`, `reference/cpp/plugins/DrawDeviceForSteam/DrawDeviceForSteam.cpp`
**Registers:** the `Steam`, `steam_api` and `DrawDeviceForSteam` classes

## Surface

### `Steam`

All members carry the reference `TJS_STATICMEMBER` flag, so they live on the
class object.

| member | kind | behaviour |
|---|---|---|
| `Steam.getAchievement(n)` | static method | the mock achievement display name `"Achievement 1"` |
| `Steam.setAchievement(n)` | static method | always succeeds, returns `1` |
| `Steam.clearAchievement(n)` | static method | always succeeds, returns `1` |
| `Steam.getLanguage()` | static method | `"English"` |
| `Steam.requestInitialize()` | static method | always succeeds, returns `1` |
| `Steam.getCloudQuota()` | static method | `1073741824` (1 GiB) |
| `Steam.getCloudFileInfo()` | static method | the mock file name `"file1.txt"` |
| `Steam.deleteCloudFile(name)` | static method | always succeeds, returns `1` |
| `Steam.copyCloudFile(from, to)` | static method | always succeeds, returns `1` |
| `Steam.triggerScreenshot()` | static method | does nothing, returns `void` |
| `Steam.hookScreenshots(...)` | static method | does nothing, returns `void` |
| `Steam.writeScreenshot(...)` | static method | does nothing, returns `void` |
| `Steam.isBroadcasting()` | static method | `0` |
| `Steam.hookBroadcasting(...)` | static method | does nothing, returns `void` |
| `Steam.isIsSubscribedApp()` | static method | `1` |
| `Steam.ssDlcInstalled(n)` | static method | `1` |
| `Steam.getDLCCount()` | static method | `2` |
| `Steam.getDLCData(n)` | static method | the mock DLC name `"DLC1"` |
| `Steam.initialized` | static property (read-only) | `1` |
| `Steam.achievementsCount` | static property (read-only) | `0` |
| `Steam.cloudEnabled` | static property (read-only) | `1` |
| `Steam.cloudFileCount` | static property (read-only) | `0` |
| `Steam.dlcCount` | static property (read-only) | `0` |

The reference registers no `Constructor()`, so `new Steam()` raises
`TJS_E_NOTIMPL` (`reference/cpp/core/plugin/ncbind.hpp:1948,1951`). The port
reproduces that: the constructor reports a typed error.

### `steam_api`

| member | kind | behaviour |
|---|---|---|
| `steam_api()` | constructor | the reference's trivial empty class |

`steam_api` is the reference's trivial empty class: it registers only a
constructor. The port registers an empty instance class with no
script-visible members — neither the reference's plain `vserion` field nor
any other state is exposed to TJS.

### `DrawDeviceForSteam`

| member | kind | behaviour |
|---|---|---|
| `DrawDeviceForSteam()` | constructor | allocates the empty proxy |
| `interface` | property (read-only) | the draw device as an integer |

## Behaviour

### The mock `Steam` class

The reference `Steam` class links no `libsteam_api`: every member returns a
hard-coded value or does nothing. The port reproduces those values one for
one.

`DrawDeviceForSteam` is a transparent wrapper around the main window's draw
device whose `interface` property returns that device as an integer
(`reference/cpp/plugins/DrawDeviceForSteam/DrawDeviceForSteam.cpp:21-24`,
registered at `:412-415`). This host has no host draw-device object, so the
getter reports the stable native payload address as a non-zero synthetic
pointer.

## State and lifetime

* The `Steam` class carries an empty instance payload; the mock surface is
  on the class object, not per instance.
* `steam_api` carries an empty instance payload.
* `DrawDeviceForSteam` carries a marker payload whose address the `interface`
  getter reports. The pointer is stable for the lifetime of the object and is
  never dereferenced — it is an opaque token, never read back as a handle.
* Returned strings are copied into a per-engine scratch buffer that the ABI
  trampoline drains immediately after the callback returns.

## Errors

| condition | outcome |
|---|---|
| `new Steam()` | typed error: `Steam: the class has no constructor` — the reference raises `TJS_E_NOTIMPL` |
| the compat context is not bound | typed error |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/steam.rs` | each mock member's value, the `Steam` constructor's error, the `steam_api` class, and the `DrawDeviceForSteam.interface` pointer |
| `crates/compat/tests/plugin.rs` | that `Plugins.link("krkrsteam.dll")` and its aliases resolve to the built-in table |

## Known gaps

* The `Steam` class is a **mock**, not a Steamworks client. Every value is
  the reference's hard-coded one, so a game that expects real achievements,
  cloud files or broadcasting sees the same fixed answer it would see on the
  reference — which itself links no `libsteam_api`.
* `DrawDeviceForSteam.interface` reports a synthetic non-zero pointer rather
  than a real draw-device address, because this host has no draw-device
  object. The reference's value is the main window's draw device; a script
  that only checks it is non-zero sees the same shape.
* The reference's `steam_api` class exposes no member, and neither does the
  port.
