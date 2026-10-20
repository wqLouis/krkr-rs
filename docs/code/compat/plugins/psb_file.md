# PSBFile (`psbfile.dll`)

**DLL name:** `psbfile.dll` — linked with `Plugins.link("psbfile.dll")`
**Rust module:** `crates/compat/src/natives/plugins/psb_file/`
**Reference:** `reference/cpp/plugins/psbfile/` (`main.cpp`, `PSBFile.{h,cpp}`, `PSBValue.{h,cpp}`, `PSBHeader.h`, `PSBEnums.h`, `PSBStreamDecompress.cpp`, `PSBLz4Stream.cpp`, `types/*`)
**Registers:** the `PSBFile` class

## Surface

| member | kind | behaviour |
|---|---|---|
| `PSBFile()` / `PSBFile(path)` | constructor | no arguments make an empty instance; a single string argument loads immediately |
| `load(path)` | method | loads a container and returns whether it succeeded |
| `root` | property (read-only) | a fresh `Dictionary` per read |

The reference registers a factory that accepts an optional path plus the
`root` and `load` members. The class's other accessors are not
script-visible, because PSB values are converted to native TJS `Array` /
`Dictionary` objects when `root` is read.

## Behaviour

### The constructor

The constructor is exactly the reference factory
(`reference/cpp/plugins/psbfile/main.cpp:119-131`):

* no arguments make an empty instance;
* a single string argument loads immediately — a **storage** failure still
  raises, like the reference's `TVPCreateStream`, while a **parse** failure
  is ignored exactly like the reference, which discards the `loadPSBFile`
  result;
* every other argument combination raises `TJS_E_INVALIDPARAM`.

The reference factory takes no seed argument
(`reference/cpp/plugins/psbfile/main.cpp:124-129`), so neither does the port.

### `load(path)`

`load` takes exactly one argument (`reference/cpp/plugins/psbfile/main.cpp:51`):

* a string loads and returns `true` or `false`
  (`reference/cpp/plugins/psbfile/main.cpp:54-78`);
* an octet returns `false`
  (`reference/cpp/plugins/psbfile/main.cpp:70-72`);
* anything else raises `TJS_E_INVALIDPARAM`
  (`reference/cpp/plugins/psbfile/main.cpp:74`).

The failure modes follow the reference's `loadPSBFile`
(`reference/cpp/plugins/psbfile/PSBFile.cpp:531-551`):

* a **storage-read failure raises** — the reference's `TVPCreateStream`
  throws out of `loadPSBFile`
  (`reference/cpp/core/base/StorageIntf.cpp:1189-1219`,
  `reference/cpp/plugins/psbfile/PSBFile.cpp:531-534`);
* a **bad signature or `MDF`/`MFL` magic raises** — `parsePSBHeader` throws
  and nothing in `loadPSBFile` catches it
  (`reference/cpp/plugins/psbfile/PSBHeader.h:69-75`,
  `reference/cpp/plugins/psbfile/PSBFile.cpp:546`);
* **every other parse failure** — decompression, encryption, an unsupported
  version, unparsable objects — returns `false` with the previous document
  intact (`reference/cpp/plugins/psbfile/PSBFile.cpp:535-551`,
  `reference/cpp/plugins/psbfile/main.cpp:56-58`).

### `root`

`root` returns a **fresh** `Dictionary` per read (the reference `getRoot`,
`reference/cpp/plugins/psbfile/main.cpp:29-42`). When no file is loaded, or
the container's root value is not a dictionary — `getObjects()` null-casts to
an empty object — the result is an empty `Dictionary`, never an error.

### The serialized value shapes

The mapping follows `reference/cpp/plugins/psbfile/PSBValue.cpp:166-226`:

| PSB value | TJS value |
|---|---|
| `PSBNull` | **void** (`:166`) |
| `PSBBool` | integer `0` / `1` (`:168`) |
| integer | integer |
| float / double | real (`:170-183`) |
| string | string (`:199`) |
| `PSBArray` of `u32` | an `Array` of **int32** values (`:185-197` — `static_cast<tjs_int32>(i)`), so entries above `i32::MAX` wrap to negative |
| `PSBResource` | a TJS **octet** (`:201-204`) |
| nested list / dictionary | real TJS `Array` / `Dictionary` objects |

An octet value cannot cross the port's member ABI as a scalar — the safe
`TjsValue` surface has no octet variant, and a handle-less `Object` value
resolves against the last object result. A TJS *script* can build a
`<% … %>` literal, so a resource member is installed by a script-side
literal assignment against the parent object: the parent is parked in a
temporary global, the assignment runs as script text, and the globals are
cleared afterwards. The reference's octet member is therefore reproduced
without changing the VM surface.

### The `psb://` media

`load` registers every resource of the container in the `psb://` storage
media (`PSBMedia`, `reference/cpp/plugins/psbfile/main.cpp:60-67` and
`PSBMedia.h:6-45`), so scripts can open `psb://…` paths afterwards. The
whole value tree is walked, so a resource nested in a dictionary or a list is
registered under its `/`-joined member path with list indices as path
segments.

The registration goes through the engine's `Storage::register_media` table,
which serves those names before disk and archive resolution. The container
name is lower-cased from its first `.`, so `.psb` and `.PSB` containers share
one name.

## State and lifetime

* The class owns its parsed document in the native instance payload; nothing
  lives in a process global.
* Encoding a value for TJS builds real TJS `Dictionary` / `Array` objects
  through the engine, so a game script sees the same shapes as the
  reference's `toTJSVal` conversions.
* The document is released when the instance is invalidated or destroyed.
* The `psb://` media registration lives in the engine's storage media table,
  so it outlives the instance — as in the reference, where the media is
  registered globally.

## Errors

| condition | outcome |
|---|---|
| the constructor with an unsupported argument combination | `TJS_E_INVALIDPARAM` |
| `load` with an unsupported argument type | `TJS_E_INVALIDPARAM` |
| `load` on a storage that cannot be read | the storage's error — the reference's `TVPCreateStream` throws |
| `load` on a container with a bad signature or magic | the header parser's error — the reference's `parsePSBHeader` throws |
| `load` on a container that cannot be decompressed, decrypted, or whose version is unsupported | `false`, with the previous document intact |
| `load` with an octet argument | `false` |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/psb_file.rs` | the constructor forms, `load`'s argument handling, the `true` / `false` results, the raising storage failure, the `root` dictionary and its value shapes, and the `psb://` media registration |

## Known gaps

* The reference's `PSBFile` has more C++-side accessors; they are not
  script-visible in the reference either, because the values are converted
  to native TJS objects when `root` is read. The port registers the same
  three members.
* The reference factory takes no seed argument, so neither does the port.
* The `psb://` media is registered in the engine's storage media table rather
  than through a `TVPRegisterStorageMedia` call. A script that opens a
  `psb://` path sees the same resource bytes.
