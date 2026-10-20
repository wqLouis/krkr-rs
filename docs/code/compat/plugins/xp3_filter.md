# xp3filter (`xp3filter.dll`)

**DLL name:** `xp3filter.dll` — linked with `Plugins.link("xp3filter.dll")`
**Rust module:** `crates/compat/src/natives/plugins/xp3_filter/`
**Reference:** `reference/cpp/plugins/xp3filter.cpp`
**Registers:** two static members on the core `Storages` class, plus a deferred half

## Surface

| member | kind | behaviour |
|---|---|---|
| `Storages.setXP3ArchiveExtractionFilter(func)` | static method | installs the per-segment decryption callback |
| `Storages.setXP3ArchiveContentFilter(func)` | static method | installs the per-entry content callback |

The plugin reads `xp3filter.tjs`, registers the two static members on
`Storages`, and installs Rust wrappers that the XP3 reader invokes per entry.
The game's script supplies the decryption algorithm; this module is only the
mechanism.

## Behaviour

### The callbacks

The extraction filter runs once per decoded segment of an archive entry and
receives the reference's argument list:

```
(file_hash, offset, buffer, buffer_size, file_name, ctx)
```

The content filter runs once per archive entry read, **before** the extraction
filter, and its `[ret, ctx]` return value contributes `ctx` — the `ret` flag
is ignored, because the archive read is always eager, which is what
`ret == 1` would have forced. The content filter runs whether or not an
extraction filter is installed.

The reference passes a `CBinaryAccessor` whose indexed accessors and `xor`
method rewrite the segment in place. The VM's C ABI cannot expose a borrowed
byte slice as a native object, so the port marshals the segment as a TJS
`Array` of byte values and attaches the reference `ptr` / `count` members and
the `xor` helper as own members of that object, calls the closure, then copies
the array back into the segment. The `xor` helper is a retained script
function matching the reference `CBinaryAccessor::FuncXor`
(`reference/cpp/plugins/xp3filter.cpp:266-297`); it reads `this.ptr`, which
the reference adds to the offset (`:258-260`).

The `Array`'s native indexed access already applies the reference's indexed
operators (`^=`, `+=`, …) to individual bytes.

### Script load timing

The reference reads `<app>/xp3filter.tjs` in a post-registration callback and
executes it in a **dedicated sub-engine** whose global scope holds only
`Debug` / `System` / `Storages` (`reference/cpp/plugins/xp3filter.cpp:552-559`,
`:394-421`), so nothing the filter script declares is visible to the game
script.

The port reads the source through the storage at registration when the storage
is already attached; a read failure is returned to the host as a registration
error. When the composition root attaches the storage after registration, the
read and the seam install are completed at
`CompatHost::activate_deferred_plugins`, which the composition root calls
before the startup sequence.

That completion reports the not-yet-attached storage as its own state
(`ScriptLookup::StorageNotAttached` / `SeamInstall::StorageNotAttached`),
never as an absent script.

The script executes on the first archive read, once the engine → context
binding the setters need is live. The shared engine cannot expose a second
global, so the source is wrapped in a named local function: its declarations
stay local to the filter script — the same isolation the sub-engine provides
for top-level `function` / `var`. A function *expression* cannot host the
wrapper: TJS2 does not bind a `function` declaration inside an anonymous
function body (the name resolves neither locally nor on `global`).

## Layering

* The file-system crate owns the stateless per-segment hook
  (`fs::xp3::Xp3ExtractionFilter`); the filter is passed in and never stored
  there.
* The engine's storage namespace owns the installed filter and hands it to the
  file-system crate on every archive read
  (`engine_api::Storage::xp3_extraction_filter`).
* This module registers the `Storages` members, retains the TJS closures in
  the per-engine dispatch state, and installs a filter that marshals each
  decoded segment to the retained TJS closure.

## State and lifetime

The per-install state lives in `Xp3FilterState` inside the per-engine dispatch
state:

| field | lifetime |
|---|---|
| `extraction`, `content` | the retained closures; replaced when a setter is called again |
| `buffer_xor` | the retained `xor` helper, created once at registration |
| `seam_installed` | whether the engine-side extraction-filter seam is installed |
| `script_read` | whether the `xp3filter.tjs` read has run |
| `pending_script` | the source, awaiting execution on the first archive read |
| `script_loaded` | whether the script has been executed (or a filter was installed programmatically), so the deferred execution never clobbers it |
| `entry` | the content filter's context for the entry currently being decoded |

The retained closures are `DetachedValue`s, so the plugin never holds a
borrow of the engine.

## Errors

| condition | outcome |
|---|---|
| the `xp3filter.tjs` read fails | typed error returned to the host as a registration error: `xp3_filter: <error>` |
| the storage is not attached yet | deferred: the plugin logs at debug level and completes at `activate_deferred_plugins`; this is reported as its own state, never as an absent script |
| `xp3filter.tjs` is absent | the plugin logs that archive content is not filtered and carries on |
| the `xor` helper cannot be created | typed error: `xp3_filter: could not create the buffer xor helper: …` |
| the compat context is not bound | typed error |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/xp3_filter.rs` | the two `Storages` members, the filter installation, the per-segment dispatch and the segment rewrite |
| `crates/compat/tests/xp3_filter_deferred.rs` | the deferred half: the storage-not-attached state, the completion at `activate_deferred_plugins`, and the one-shot script execution |
| `crates/compat/tests/storages_io.rs` | that both setters are registered on `Storages` |

## Known gaps

* The reference's `CBinaryAccessor` cursor can be added to the index inside a
  native `[]` access (`PropGetByNum` / `OperationByNum`,
  `reference/cpp/plugins/xp3filter.cpp:162-197`), which would require a
  custom dispatch method the C ABI does not expose. The port's marshalled
  buffer supports the indexed operators but not that cursor arithmetic.
* The reference executes the filter script in a separate engine whose global
  scope holds only `Debug` / `System` / `Storages`. The port executes it in
  the shared engine, wrapped in a named local function, so the script's
  top-level declarations stay local — but free names still resolve against
  the game's global scope, and an explicit `global.x = …` assignment is
  visible to the game script where the reference keeps it in the sub-engine.
* The content filter's `ret` flag is ignored: the archive read is always
  eager, which is what `ret == 1` would have forced.
