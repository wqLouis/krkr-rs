# `plugin.rs` — the native-plugin architecture

**Source:** `crates/compat/src/plugin.rs`

Native plugins are **statically built into L3**. One table,
`natives::plugins::BUILTIN_PLUGINS`, maps a canonical DLL name (plus its
aliases) to that module's `register` function. `Plugins.link(name)`
recognizes every ported DLL; a name with no port is **absent** — the link is
recorded, one warning names the plugin, and nothing is registered for it.

**Registration happens at link time.** Only the startup surface
(`natives::plugins::STARTUP_PLUGINS` — today `xp3filter.dll`) is on the VM
before any script runs; every other built-in registers its members on the
first `Plugins.link` that names it.

The layer-level description, including the parity report, is in
[`../../arch/l3.md`](../../arch/l3.md) §5.

---

## 1. The trait

```rust
pub trait NativePlugin {
    fn name(&self) -> &str;
    fn aliases(&self) -> &[&str] { &[] }
    fn dependencies(&self) -> &[&str] { &[] }
    fn register(&self, ctx: &mut CompatCtx) -> Result<(), CompatError>;
    fn poll(&self, _ctx: &mut CompatCtx) -> Result<(), CompatError> { Ok(()) }
}
```

| Method | Purpose |
|---|---|
| `name` | the name scripts pass to `Plugins.link`, e.g. `"menu.dll"` |
| `aliases` | alternate DLL names that resolve to this plugin (e.g. `layerExDraw.dll` and `perspective.dll` both map to the `layerEx` port) |
| `dependencies` | the modules this plugin loads when it activates (the reference's pre-registration hook): `emoteplayer.dll` → `motionplayer.dll`, `getabout.dll` → `windowEx.dll` |
| `register` | registers the plugin's native classes on the VM |
| `poll` | an optional per-frame hook |

`BuiltinPluginDef` *is* the `NativePlugin` implementation over a plain
function pointer, so adding a ported plugin is **one table entry** rather than a
trait impl.

---

## 2. The built-in table

`BuiltinPluginDef { name, aliases, dependencies, register }`.

| Canonical name | Aliases | Dependencies | Registers |
|---|---|---|---|
| `addFont.dll` | — | — | the `System.addFont` registry |
| `csvParser.dll` | — | — | `CSVParser` |
| `dirlist.dll` | — | — | the `Storages` directory members |
| `emoteplayer.dll` | — | `motionplayer.dll` | nothing of its own — it is the loader shim for `motionplayer.dll` |
| `extrans.dll` | — | — | the extra transition providers |
| `fftgraph.dll` | — | — | the FFT graph drawing |
| `fstat.dll` | — | — | the `Storages` file-attribute and directory members |
| `getabout.dll` | — | `windowEx.dll` | nothing of its own — `System.getAboutString` belongs to the `windowEx` port |
| `getSample.dll` | — | — | the `WaveSoundBuffer` sample members |
| `json.dll` | — | — | the JSON members |
| `KAGParserEx.dll` | `KAGParser.dll` | — | nothing of its own — `KAGParser` is a core native registered at startup |
| `layerExDraw.dll` | `layerExMovie.dll`, `perspective.dll` | — | the layer drawing, movie and perspective members |
| `menu.dll` | — | — | the `MenuItem` surface (registered by the core `MenuItem` module) |
| `motionplayer.dll` | — | — | the `Motion` / `Player` classes |
| `psbfile.dll` | — | — | `PSBFile` |
| `psd.dll` | — | — | `PSD` |
| `saveStruct.dll` | — | — | the `Array` / `Dictionary` struct members |
| `ScriptsEx.dll` | — | — | the `Scripts` extension members |
| `krkrsteam.dll` | `steam_api.dll`, `DrawDeviceForSteam.dll` | — | the Steam overlay classes |
| `TextRender.dll` | — | — | `TextRenderBase` |
| `varfile.dll` | — | — | the `Storages` variable-file members |
| `win32dialog.dll` | — | — | `WIN32Dialog` |
| `windowEx.dll` | — | — | the `Window` / `System` / `MenuItem` / `Console` extensions |
| `wutcwf.dll` | — | — | the TCWF audio decoder |
| `wuvorbis.dll` | — | — | the Wuvorbis audio decoder |
| `xp3filter.dll` | — | — | the XP3 archive filters |

Names and aliases match the reference `NCB_MODULE_NAME` strings under
`reference/cpp/plugins` (plus the CMake source list). `extrans.dll` and
`wuvorbis.dll` have no sources in that tree — their link-time surface was
verified against the shipped binaries.

A **dependency** is the reference's `NCB_PRE_REGIST_CALLBACK`: the module
loads another one before it registers itself. The registry registers each
dependency first and **does not** record it as a link, so `Plugins.getList`
reports only the names the script linked.

An entry whose reference registers no TJS surface — or shares it with a
sibling port — carries a documented no-op `register`.

`compat::builtin_plugin_names()` returns the canonical names of every entry.

### The startup surface

`natives::plugins::STARTUP_PLUGINS` is the list of modules whose surface is
on the VM **before any script runs**. It holds exactly one entry,
`xp3filter.dll`: the reference's `TVPLoadInternalPlugins` loads exactly that
module at startup
(`reference/cpp/core/plugin/PluginImpl.cpp:90-91`), and every other plugin's
members appear from `V2Link`.

Core native classes such as `KAGParser` are registered by
`natives::register_all` before the plugin startup pass; their DLL names in
the table are empty placeholders, not link-time class registrations.

Every other built-in registers its native classes on the **first**
`Plugins.link` that names it. The timing is script-observable, so it is
pinned by a test — see
[`plugins/get_sample.md`](plugins/get_sample.md) for the case that motivated
the split.

`natives::plugins::registers_at_startup(name)` answers the question for one
name, comparing like `Plugins.link` does: base name only, ASCII
case-insensitive.

---

## 3. The registry

```rust
pub struct PluginRegistry {
    plugins:   Vec<Box<dyn NativePlugin>>,
    linked:    Mutex<Vec<LinkedEntry>>,
    registered: Mutex<BTreeSet<String>>,
}
```

| Field | Purpose |
|---|---|
| `plugins` | the built-in emulations |
| `linked` | the linked names in **insertion order**, with the script's original casing — this is what `Plugins.getList` reports |
| `registered` | the built-ins whose native classes are already on the VM |

`registered` exists because **the ABI cannot unregister a class**: the VM
rejects a duplicate class name, so a later `link` of the same built-in skips
`register` instead of hitting the rejection.

### `PluginRegistry::new()` / `PluginRegistry::builtin()`

`new()` builds an empty registry, but it **pre-marks the startup modules as
registered**: `crate::install` always puts their surface on the VM before any
script runs, whichever registry the host was built with.

`builtin()` adds every ported plugin (the table in
`natives::plugins::BUILTIN_PLUGINS`) on top of that. The startup modules stay
marked registered; **every other built-in is unregistered until a script links
it**, so `Plugins.link` activates it and registers its classes then.

### `activate(name, ctx) -> Result<PluginOutcome, CompatError>`

1. If the name was already linked, return the recorded outcome — `link` is
   **idempotent**.
2. If a built-in emulation exists, register it (unless the canonical name is
   already in `registered`) and record the link as `Builtin`. The canonical
   name is tracked, so aliases sharing one `register` register **once**.
3. Otherwise log one warning that names the plugin and record the link as
   `Unported`. **Nothing is registered**: no global under the plugin's stem
   and no object that swallows member accesses, so a script that probes for
   the class (`typeof global[name] == 'Object'`) sees it missing and takes
   its own "class not found" path.

### The lookups

`plugin(name)` returns the built-in a script-visible name resolves to — the
canonical name or any alias, compared the way `Plugins.link` compares them
(base name only, ASCII case-insensitive). `is_builtin(name)` is the boolean
form of the same question, and `linked()` returns the linked names in
insertion order with the script's original casing — that is what
`Plugins.getList` reports.

### `ensure_registered(plugin, ctx)`

The registration step behind `activate`. It registers a built-in **and its
declared dependencies** exactly once:

* the canonical name is already in `registered` → nothing happens;
* otherwise each dependency is registered first (recursively, so a dependency
  of a dependency also lands), then the plugin itself.

A dependency is **not** recorded as a link: `Plugins.getList` reports only
the names the script linked. A dependency with no built-in emulation is
logged at `debug` and skipped — it is not an error, because a missing
emulation leaves the dependent's own surface intact.

### `poll(ctx)`

Runs the per-frame hook of every built-in that has been **activated**. A
plugin no script has linked has no script-visible surface and owns no
per-frame hook, so its `poll` does not run: the `registered` set is the same
one `ensure_registered` fills on the first link.

### `unlink(name) -> bool`

Drops the `getList` entry and reports whether the name had been linked.

* The built-in classes **stay** on the VM; only the list entry goes, and a
  later `link` is idempotent.
* An unported link owns no global, so there is nothing to clear.

### `autoload(names, ctx)`

Links the `.tpm` plugins the composition root found, in order.

The reference scans the executable directory, `<exe>/system` and
`<exe>/plugin` for `*.tpm`, sorts them by file name and loads each in turn.
The **scan** is the composition root's job (compat owns no file-system
access); this receives the ordered file names and links each.

* A trailing `.tpm` becomes `.dll` first, so an auto-loaded `foo.tpm`
  resolves the same built-in as `Plugins.link("foo.dll")`.
* Each is logged with the reference's `(info) Loading <name>` line.
* Linking goes through the same path as `Plugins.link`: a ported name
  activates its built-in emulation, an unported name is recorded and warned
  about — **one** warning, even when the same file was found in more than one
  scanned directory.

---

## 4. Name normalisation

```rust
pub fn normalize_plugin_name(name: &str) -> String;
```

`normalize_plugin_name` takes the base file name (splitting on `/`, `\` and
`>`), trims it and lowercases it, so `C:\game\plugin\extrans.dll`,
`extrans.tpm` and `Extrans.DLL` compare equal.

`tpm_to_dll_name` replaces a trailing `.tpm` with `.dll`, case-insensitively,
preserving the stem's original casing.

---

## 5. The outcomes

```rust
pub enum PluginOutcome {
    Builtin,   // a built-in emulation was registered
    Unported,  // the plugin has no emulation: the link is recorded, one
               // warning names it, and nothing is registered for it
}
```

---

## 6. Errors

| Error | Raised when |
|---|---|
| `CompatError::Register(...)` | the built-in's `register` failed |

`unlink` never fails: it only drops a list entry.

---

## 7. Tests

| Test | What it asserts |
|---|---|
| `crates/compat/tests/plugin.rs` | `link` activates a built-in, `unlink` reports whether the name had been linked, `getList` preserves the order and the casing, `link` is idempotent; an unported plugin registers nothing and the game's class probe fails |
| `crates/compat/tests/plugin_autoload.rs` | the `.tpm` autoload: the name conversion, the order, the single warning per distinct unported name |
| `crates/compat/tests/menu_dll_surface.rs` | `menu.dll` links as a built-in |
| `crates/compat/tests/misc_plugins.rs` | the plugin classes are registered |
| `crates/compat/tests/get_sample.rs` | a built-in's members appear only after its `Plugins.link` |
| `crates/compat/tests/timer_behavior.rs` | the `getSample` members' timing, through the shipped game's `voice.tjs` path |
| `crates/compat/src/natives/plugins/mod.rs` `mod tests` | the startup list is exactly `xp3filter.dll` and every startup name is a ported table entry |
| `crates/compat/tests/plugin_parity.rs` / `docs/plugin_parity.md` | the plugin surface parity report |
| `crates/compat/tests/runtime_surface.rs` | the whole-surface dump, including the plugin-attached members |

---

## See also

* [`plugins_native.md`](plugins_native.md) — the `Plugins` class scripts call
* [`host.md`](host.md) — who owns the registry
* [`../../arch/l3.md`](../../arch/l3.md) §5 — plugins at the layer level
