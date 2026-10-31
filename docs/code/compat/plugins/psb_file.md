# PSBFile (`psbfile.dll`)

**DLL name:** `psbfile.dll` — linked with `Plugins.link("psbfile.dll")`
**Rust module:** `crates/compat/src/natives/plugins/psb_file/`
**Reference:** `reference/cpp/plugins/psbfile/`
**Registers:** the `PSBFile` class

The plugin reads an Emote PSB container — the header, the name and string
tables, the typed value tree and the raw resource chunks — and exposes the
value tree to a script as native TJS objects. The container itself is
parsed by the shared decoder crate ([`psd`](../../platform/media/psd.md),
`crates/platform/media/psd/src/container.rs`); this module marshals the
arguments, converts the parsed values into TJS objects and registers the
container's resources in the storage media table.

The class is registered on the first `Plugins.link("psbfile.dll")` — the
built-in table entry registers it, exactly as the reference's `V2Link` does.
Only `xp3filter.dll` is a startup module, so a script that reads
`PSBFile` before it links the DLL does not see the class yet. See the
[plugin index](../plugins.md) for the table and the conventions the plugin
pages follow.

The same container parser serves the [`motion_player`](motion_player.md)
port, which reads the PSB data of an emote and draws it.

## Surface

| member | kind | behaviour |
|---|---|---|
| `PSBFile` | constructor | `new PSBFile()` creates an empty instance; `new PSBFile(path)` creates one and loads the container immediately |
| `load(path)` | method | loads a container; returns whether it was loaded |
| `root` | property (read-only) | a fresh `Dictionary` holding the container's root value |

The reference registers a factory that accepts an optional path plus the
`root` and `load` members (`reference/cpp/plugins/psbfile/main.cpp:135-139`).
The class's other accessors are not script-visible, because the PSB values
are converted to native TJS `Array` / `Dictionary` objects when `root` is
read.

## Behaviour

### The constructor

The constructor is the reference factory
(`reference/cpp/plugins/psbfile/main.cpp:119-131`):

* no arguments make an empty instance;
* a single string argument loads the container immediately. A **storage**
  failure raises, like the reference's `TVPCreateStream`, while a **parse**
  failure is swallowed exactly like the reference, which discards the
  `loadPSBFile` result (`reference/cpp/plugins/psbfile/main.cpp:127`);
* every other argument combination raises.

The reference factory takes no seed argument
(`reference/cpp/plugins/psbfile/main.cpp:124-129`), so neither does the
port; an encrypted container answers `false` in both.

### `load(path)`

`load` takes exactly one argument (`reference/cpp/plugins/psbfile/main.cpp:51`):

* a string loads the container and answers `true` or `false`
  (`reference/cpp/plugins/psbfile/main.cpp:54-68`);
* an octet answers `false` — the reference logs "PSBFile::load stream no
  implement!" and does the same
  (`reference/cpp/plugins/psbfile/main.cpp:70-72`);
* anything else raises (`reference/cpp/plugins/psbfile/main.cpp:74`).

The failure modes follow the reference's `loadPSBFile`
(`reference/cpp/plugins/psbfile/PSBFile.cpp:529-555`):

* a **storage-read failure raises** — the reference's `TVPCreateStream`
  throws out of `loadPSBFile`
  (`reference/cpp/core/base/StorageIntf.cpp:1189-1219`);
* a **bad signature raises** — `parsePSBHeader` throws and nothing in
  `loadPSBFile` catches it (`reference/cpp/plugins/psbfile/PSBHeader.h:62-64`,
  `reference/cpp/plugins/psbfile/PSBFile.cpp:330`);
* an **unsupported version, an encrypted container, a decompression failure
  or an unparsable value tree answers `false`**
  (`reference/cpp/plugins/psbfile/PSBFile.cpp:434-451`,
  `reference/cpp/plugins/psbfile/PSBFile.cpp:507-511`). The previous
  document stays intact: the port replaces it only after a successful
  parse, like the reference's `_root` assignment
  (`reference/cpp/plugins/psbfile/PSBFile.cpp:514`).

The container may arrive as a plain PSB byte stream, as an `MDF` archive
whose payload is zlib, or as an Emote LZ4 frame; the reader recognises all
three (`reference/cpp/plugins/psbfile/PSBStreamDecompress.cpp:36-64`).

### `root`

`root` answers a **fresh** `Dictionary` per read — the reference creates a
new object on every call (`reference/cpp/plugins/psbfile/main.cpp:29-44`).
When no container is loaded, or the container's root value is not a
dictionary — `getObjects()` null-casts for every other root
(`reference/cpp/plugins/psbfile/PSBFile.h:97-99`) — the result is an empty
`Dictionary`, never an error and never the scalar itself.

### The serialized value shapes

The conversion follows the reference's `toTJSVal` family
(`reference/cpp/plugins/psbfile/PSBValue.cpp:166-230`):

| PSB value | TJS value | reference |
|---|---|---|
| null | **void** | `reference/cpp/plugins/psbfile/PSBValue.cpp:166` |
| boolean | integer `0` / `1` | `reference/cpp/plugins/psbfile/PSBValue.cpp:168` |
| integer | integer | `reference/cpp/plugins/psbfile/PSBValue.cpp:170-183` |
| float / double | real | `reference/cpp/plugins/psbfile/PSBValue.cpp:170-183` |
| string | string | `reference/cpp/plugins/psbfile/PSBValue.cpp:199` |
| integer array | an `Array` of **int32** values, so an entry above `i32::MAX` reads back negative | `reference/cpp/plugins/psbfile/PSBValue.cpp:185-197` (`static_cast<tjs_int32>(i)`) |
| resource | a TJS **octet** | `reference/cpp/plugins/psbfile/PSBValue.cpp:201-204` |
| list | a TJS `Array` | `reference/cpp/plugins/psbfile/PSBValue.cpp:218-230` |
| dictionary | a TJS `Dictionary` | `reference/cpp/plugins/psbfile/PSBValue.cpp:206-216` |

An octet value cannot cross the port's member ABI as a scalar — the safe
value surface has no octet variant, and a handle-less object value resolves
against the last object result. The engine therefore carries the bytes
through a dedicated octet member setter, which installs the octet variant
directly on the parent object — the same value the reference builds with
`tTJSVariant(data, size)`
(`reference/cpp/plugins/psbfile/PSBValue.cpp:201-204`). A script reads the
member as `Octet`, with `.length` and index access
(`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:124-162`).

### The `psb://` media

A successful `load` registers every resource of the container in the `psb://`
storage media, so a script can open `psb://…` paths afterwards
(`reference/cpp/plugins/psbfile/main.cpp:60-68`,
`reference/cpp/plugins/psbfile/PSBMedia.cpp:14-36`).

The whole value tree is walked, so a resource nested in a dictionary or a
list is registered under its `/`-joined member path, with a list index
contributing a path segment
(`reference/cpp/plugins/psbfile/PSBMediaRegistry.cpp:16-77`). A resource
directly under the root keeps its own key: `psb://<container>/<key>`.

The registration goes through the engine's storage media table, which
serves those names before disk and archive resolution. The container name
is lower-cased from its first `.`
(`reference/cpp/plugins/psbfile/PSBMedia.cpp:14-20`), so `a.psb` and
`a.PSB` share one name.

## State and lifetime

* The parsed document lives in the native instance payload; nothing lives in
  a process global.
* Building a value for TJS creates real TJS `Dictionary` / `Array` objects
  through the engine, so a game script sees the same shapes as the
  reference's `toTJSVal` conversions.
* The document is dropped when the instance is invalidated or destroyed.
* The `psb://` registrations live in the engine's storage media table, so
  they outlive the instance — as in the reference, where the media is
  registered globally.

## Errors

| condition | outcome |
|---|---|
| the constructor with anything but zero arguments or one string | raises `PSBFile: invalid parameters (expected PSBFile() or PSBFile(path))` |
| the constructor with a string that cannot be read or carries a bad signature | raises `PSBFile: <error>`; every other parse failure is swallowed and leaves an empty instance |
| `load` with anything but one argument | raises `PSBFile.load requires exactly 1 argument` |
| `load` with an argument that is neither a string nor an octet | raises `PSBFile.load: invalid parameter type` |
| `load` with an octet argument | answers `false` |
| `load` on a storage that cannot be read | raises `PSBFile.load: <error>` — the reference's `TVPCreateStream` throws |
| `load` on a container with a bad signature | raises `PSBFile.load: <error>` — the reference's `parsePSBHeader` throws |
| `load` on a container that cannot be decompressed or decrypted, whose version is unsupported, or whose value tree does not parse | answers `false`, with the previous document intact |
| `load` with no storage attached | raises `PSBFile: no storage is attached`; the constructor prefixes that once more, so it reads `PSBFile: PSBFile: no storage is attached` |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/psb_file.rs` | the container fixture and the header layout, the constructor forms, `load`'s argument handling and its `true` / `false` results, the raising storage failure, the `root` dictionary and the value shapes, the octet resource member (its bytes and its installation without a script round trip), the `psb://` registration of the root-level and the nested resources, and a parse failure keeping the previous document |

## Known gaps

* The port's `load` registers the **whole** value tree in the `psb://`
  media, while the reference's `load` registers only the resources that sit
  directly under the root (`reference/cpp/plugins/psbfile/main.cpp:60-68`).
  The reference's tree walk lives in a separate helper
  (`reference/cpp/plugins/psbfile/PSBMediaRegistry.cpp:16-77`) that the
  plugin's own `load` does not call, so a nested resource is reachable in
  the port where the reference's `load` leaves it unregistered.
* The reference's `load` continues into the resource-registration loop
  after a failed parse (`reference/cpp/plugins/psbfile/main.cpp:56-69`); the
  port answers `false` and registers nothing.
* The `psb://` media is registered in the engine's storage media table
  rather than through a `TVPRegisterStorageMedia` call. A script that
  opens a `psb://` path sees the same resource bytes.
* `root` is a `Dictionary`; the reference builds a custom object
  (`reference/cpp/plugins/psbfile/main.cpp:32`). A script that reads a
  missing key sees void in the port and an error in the reference, and
  `root instanceof Dictionary` is true only in the port.
* The reference's `PSBFile` has more C++-side accessors; they are not
  script-visible in the reference either, because the values are converted
  to native TJS objects when `root` is read. The port registers the same
  three members.
