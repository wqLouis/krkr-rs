# varfile (`varfile.dll`)

**DLL name:** `varfile.dll` — linked with `Plugins.link("varfile.dll")`
**Rust module:** `crates/compat/src/natives/plugins/var_file/`
**Reference:** `reference/cpp/plugins/varfile.cpp`
**Registers:** nothing — the `var://` storage media it registers has no counterpart in the storage layer

## Surface

| member | kind | behaviour |
|---|---|---|
| — | — | the plugin has no script-visible class or function |

The reference plugin's entire surface is one `iTVPStorageMedia` registered
through `TVPRegisterStorageMedia`: a `var://` scheme whose "files" are TJS
dictionary members holding `Octet` values. It registers **no** `Storages`
members — the file-manipulation members on the core `Storages` class come from
the `fstat` plugin and are owned by [`fstat`](fstat.md).

## Behaviour

`Plugins.link("varfile.dll")` resolves to the built-in table and succeeds, so
a game that links the DLL is not routed to the null plugin object. No
`var://` storage is served.

## State and lifetime

No state: the module registers nothing.

## Errors

None. `register` always succeeds.

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/storages_io.rs` | that the plugin registers no over-scoped `Storages` members of its own |
| `crates/compat/tests/plugin.rs` | that `Plugins.link("varfile.dll")` resolves to the built-in table |

## Known gaps

* **The `var://` storage media is not ported.** The storage namespace
  (`engine_api::Storage`) is built from an immutable file system plus
  explicitly mounted archives, and its media are fixed to the disk and
  archive backends; there is no `TVPRegisterStorageMedia` equivalent
  (`reference/cpp/plugins/varfile.cpp:371-377` registers the media from the
  pre-registration callback, and `reference/cpp/plugins/varfile.cpp:189-368`
  implements the `iTVPStorageMedia` vtable over script dictionary members —
  `CheckExistentStorage` at `:237`, `Open` at `:244`, `GetListAt` at `:263`,
  all keyed by the `var://` domain walked in `getParentName` at `:285-337`).

  Serving `var://` would need a storage-media extension point — for example a
  `Storage::register_media(name, Box<dyn StorageMedia>)` whose
  `CheckExistentStorage` / `Open` / `GetListAt` /
  `GetLocallyAccessibleName` operations the compat layer could implement over
  a TJS dictionary.

  A script that reads `var://name` therefore gets the ordinary "not found"
  outcome instead of the reference's dictionary-backed file. The port does not
  fabricate a `var://` handler or an over-scoped `Storages` member to hide the
  gap.
