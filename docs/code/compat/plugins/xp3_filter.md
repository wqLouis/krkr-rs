# xp3filter (`xp3filter.dll`)

**DLL name:** `xp3filter.dll` — linked with `Plugins.link("xp3filter.dll")`
**Rust module:** `crates/compat/src/natives/plugins/xp3_filter/`
**Reference:** `reference/cpp/plugins/xp3filter.cpp`
**Registers:** two static members on the core `Storages` class, plus a deferred half

The DLL name is in the built-in table and the linking rules are on the
[plugin index](../plugins.md). The other plugin that adds members to
`Storages` is [fstat](fstat.md).

## Surface

| member | kind | behaviour |
|---|---|---|
| `Storages.setXP3ArchiveExtractionFilter(func)` | static method | installs the per-segment decryption callback; `null` clears it |
| `Storages.setXP3ArchiveContentFilter(func)` | static method | installs the per-entry content callback; `null` clears it |

The plugin reads `xp3filter.tjs`, registers the two static members on
`Storages` (`reference/cpp/plugins/xp3filter.cpp:407-416`), and installs
Rust wrappers that the XP3 reader invokes per entry. The game's script
supplies the decryption algorithm; this module is only the mechanism.

Both setters take exactly one argument, an object closure. The reference
converts the argument with `AsObjectClosure()`
(`reference/cpp/core/tjs2/tjsVariant.h:719-726`), so a non-object raises and
`null` clears the installed filter.

## Behaviour

### The callbacks

The extraction filter runs once per decoded segment of an archive entry and
receives the reference's argument list:

```
(file_hash, offset, buffer, buffer_size, file_name, ctx)
```

(`reference/cpp/plugins/xp3filter.cpp:501-510`). The `buffer` argument is
the segment itself, and the closure rewrites it in place.

The content filter runs once per archive entry read, **before** the
extraction filter, and receives `(filepath, archivename, filesize)`
(`reference/cpp/plugins/xp3filter.cpp:475-478`). Its `[ret, ctx]` return
value contributes `ctx` — the `ret` flag is ignored, because the archive
read is always eager, which is what `ret == 1` would have forced. The
content filter runs whether or not an extraction filter is installed: the
reference registers the two wrappers independently
(`reference/cpp/plugins/xp3filter.cpp:562-563`), and so does the port.

### The buffer object

The reference passes a `CBinaryAccessor` whose indexed accessors and `xor`
method rewrite the segment in place. The VM's C ABI cannot expose a
borrowed byte slice as a native object, so the port marshals the segment as
a TJS `Array` of byte values and attaches the reference `ptr` / `count`
members and the `xor` helper as own members of that object, calls the
closure, then copies the array back into the segment.

* `count` is the `Array`'s own member, so it reports the number of
  marshalled bytes;
* `ptr` is an own member, so the filter script can move the cursor
  (`reference/cpp/plugins/xp3filter.cpp:178-214`);
* `xor(offset, length, value)` is a retained script helper matching the
  reference `CBinaryAccessor::FuncXor`
  (`reference/cpp/plugins/xp3filter.cpp:266-297`). It adds the `ptr` cursor
  to the offset, as the reference does (`:258-260`), and xors the bytes in
  place. It is attached to the per-call buffer object, so the core `Array`
  class does not carry it.

The `Array`'s native indexed access already applies the reference's indexed
operators (`^=`, `+=`, …) to individual bytes: the VM's `Array` class
implements the by-index operation
(`reference/cpp/core/tjs2/tjsArray.cpp:1786-1814`, the same TJS2 code the
VM crate vendors).

### Script load timing

The reference reads `<app>/xp3filter.tjs` in a post-registration callback
(`reference/cpp/plugins/xp3filter.cpp:550-564`) and executes it in a
**dedicated sub-engine** whose global scope holds only `Debug` / `System` /
`Storages` (`:394-421`), so nothing the filter script declares is visible
to the game script.

The port reads the source through the storage at registration when the
storage is already attached; a read failure is returned to the host as a
registration error. When the composition root attaches the storage after
registration, the read and the seam install are completed at
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
* The engine's storage namespace owns the installed filter and hands it to
  the file-system crate on every archive read
  (`engine_api::Storage::xp3_extraction_filter`).
* This module registers the `Storages` members, retains the TJS closures in
  the per-engine dispatch state, and installs a filter that marshals each
  decoded segment to the retained TJS closure.

## State and lifetime

The per-install state lives in `Xp3FilterState` inside the per-engine
dispatch state:

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
borrow of the engine. Installing a filter programmatically marks the script
as loaded, so the deferred `xp3filter.tjs` execution never overwrites a
filter the game installed itself.

## Errors

| condition | outcome |
|---|---|
| the `xp3filter.tjs` read fails | typed error returned to the host as a registration error: `xp3_filter: could not read xp3filter.tjs: …` |
| the storage is not attached yet | deferred: the plugin logs at debug level and completes at `activate_deferred_plugins`; this is reported as its own state, never as an absent script |
| `xp3filter.tjs` is absent | the plugin logs that archive content is not filtered and carries on |
| the `xor` helper cannot be created | typed error: `xp3_filter: could not create the buffer xor helper: …` |
| the setter is called without an argument | typed error: `Storages.<method> requires 1 argument` |
| the setter is called with a non-object, non-`null` value | typed error: `Storages.<method>: the filter cannot be converted to an object` |
| the compat context is not bound | typed error: `Storages.<method>: compat context is not bound` |
| the content filter raises | logged warning: `xp3_filter: content filter for \`<name>\` raised` — the entry still reads, with `ctx` left `void` |
| the extraction filter raises | logged warning: `xp3_filter: filter for \`<name>\` failed: …` — the segment keeps its bytes |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/xp3_filter.rs` | the two `Storages` members; that an extraction closure decrypts an archive member end to end; that a content filter's `ctx` reaches the extraction filter; that the `xor` helper rewrites the buffer; that the content filter runs without an extraction filter and exactly once per entry; that the setters reject a missing and a non-object argument; that `null` clears the filter; that the `xor` helper is not a member of `Array`; that `ptr` and `count` are published and the `ptr` cursor offsets the `xor`; that the filter script's top-level declarations stay local; and that an unreadable `xp3filter.tjs` fails registration |
| `crates/compat/tests/xp3_filter_deferred.rs` | the deferred half: a storage attached after `install` is completed by `activate_deferred_plugins`, and the filtered archive then decrypts |
| `crates/compat/tests/storages_io.rs` | that both setters are registered on `Storages` |

## Known gaps

* The reference's `CBinaryAccessor` cursor can be added to the index inside
  a native `[]` access (`OperationByNum` / `PropGetByNum`,
  `reference/cpp/plugins/xp3filter.cpp:17-81,189-199`), which would require
  a custom dispatch method the C ABI does not expose. The port's marshalled
  buffer supports the indexed operators and the `ptr` cursor, but not that
  cursor arithmetic inside a native `[]` access.
* The reference executes the filter script in a separate engine whose global
  scope holds only `Debug` / `System` / `Storages`. The port executes it in
  the shared engine, wrapped in a named local function, so the script's
  top-level declarations stay local — but free names still resolve against
  the game's global scope, and an explicit `global.x = …` assignment is
  visible to the game script where the reference keeps it in the sub-engine.
* The content filter's `ret` flag is ignored: the archive read is always
  eager, which is what `ret == 1` would have forced.
* The reference's `xor` is a native method with a vectorised inner loop; the
  port's helper is a retained script function with the same semantics
  (`xor(offset, length, value)`, cursor added to the offset).
