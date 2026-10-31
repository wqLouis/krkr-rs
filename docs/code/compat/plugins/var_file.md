# varfile (`varfile.dll`)

**DLL name:** `varfile.dll` (aliases: none) — linked with `Plugins.link("varfile.dll")`
**Rust module:** `crates/compat/src/natives/plugins/var_file/`
**Reference:** `reference/cpp/plugins/varfile.cpp`
**Registers:** the `var://` storage media (no script-visible class or function)

## Surface

| member | kind | behaviour |
|---|---|---|
| — | — | the plugin registers no script-visible class or function |

The reference plugin's entire surface is one `iTVPStorageMedia` registered
through `TVPRegisterStorageMedia` (`reference/cpp/plugins/varfile.cpp:373-378`):
a `var://` scheme whose "files" are the members of the script object graph. It
registers **no** `Storages` members — the file-manipulation members on the
core `Storages` class come from the `fstat` plugin and are owned by
[`fstat`](fstat.md).

## Behaviour

`Plugins.link("varfile.dll")` resolves to the built-in table and succeeds, so
the DLL is not absent. The link registers the `var://` media with the storage
namespace: a storage name that carries the `var://` scheme is dispatched to
the media before the disk and archive resolution.

The media's "files" are the members of the script object graph:

* the domain is `.` — the script global object — and the path names the
  members below it (`reference/cpp/plugins/varfile.cpp:291-346`);
* the members are read through `PropGet`, so a property's getter runs and an
  array's numeric member is the element it names;
* a member that holds an `Octet` is a file, a member that holds an object is
  a directory (`reference/cpp/plugins/varfile.cpp:10-15`).

The title's own route is the shape under test: the game keeps its packed
images in a global dictionary and reaches a layer's pixels through
`var://./<dictionary>/<container>/root/<layer id>.tlg`.

## State and lifetime

The media holds the VM it walks. It resolves the global object on every read,
so it keeps no script object alive between reads.

## Errors

The media's own failures keep the reference's wording:

| condition | message | reference |
|---|---|---|
| a domain that is not `.` | `no such domain:<domain>` | `reference/cpp/plugins/varfile.cpp:298` |
| a path without a member separator | `invalid path:<path>` | `reference/cpp/plugins/varfile.cpp:301` |
| an entry that is not a file | `cannot open memfile:var://<path>` | `reference/cpp/plugins/varfile.cpp:257` |

A scheme that no media registered is refused by the namespace itself with
`Not supported media type "<scheme>"`
(`reference/cpp/core/base/StorageIntf.cpp:201`).

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/var_file.rs` | that a member behind a property getter is read through the media, that only an octet member is a file, that the three failure messages keep the reference's wording, and that an unknown scheme is refused |
| `crates/compat/tests/psb_file.rs` | that the title's route — a `PSBFile` in a global dictionary — is read through the `var://` media |
| `crates/compat/tests/streams.rs` | that a write to a `var://` name raises "Cannot open storage" and that no `var:` directory is created |
| `crates/compat/tests/storages_io.rs` | that the file-manipulation `Storages` members belong to `fstat` alone: the `dirlist` plugin's `Storages.getDirList` is absent, and the `var://` media adds no member either |

## Known gaps

* **A write to a `var://` name is refused.** The reference's var media accepts
  a write and discards it (`reference/cpp/plugins/varfile.cpp:38-134`,
  `:150-176`); the storage namespace has no write-through-media seam, so the
  write raises "Cannot open storage" instead.
* **The media does not list its entries.** The reference's `GetListAt` walks
  the members of a directory (`reference/cpp/plugins/varfile.cpp:263-275`),
  and the storage manager builds an auto path from those listings
  (`reference/cpp/core/base/StorageIntf.cpp:1029`). The port's namespace has
  no media listing, and no script-visible member reads one.
* **A member read that raises is a media error.** The reference lets the
  script's own exception travel; the port reports it as the media's failure
  with the exception's message.
