# addFont (`addFont.dll`)

**DLL name:** `addFont.dll` — linked with `Plugins.link("addFont.dll")`
**Rust module:** `crates/compat/src/natives/plugins/add_font/`
**Reference:** `reference/cpp/plugins/addFont.cpp`
**Registers:** one static member on the core `System` class — `System.addFont`

## Surface

| member | kind | behaviour |
|---|---|---|
| `System.addFont(fontFileName[, extract])` | static method | reads a font file through the attached storage, registers every face in it for later text rendering, returns the number of faces |

The `extract` argument is accepted and ignored, exactly as in the reference
(`reference/cpp/plugins/addFont.cpp:22-28`).

## Behaviour

`System.addFont` takes the storage name of a font file, reads its bytes
through the attached storage and registers every face the file contains.

The reference resolves the name through the placed-path lookup and then runs
`TVPEnumFontsProc(filename)` on the file
(`reference/cpp/plugins/addFont.cpp:22-28`): the file is re-read and
re-enumerated on **every** call, so a source whose contents changed reports
the new face count. The port does the same — it keeps no cached count and
re-reads the file each time.

The return value distinguishes three outcomes:

| situation | result |
|---|---|
| the name resolves and the file holds usable faces | the number of faces (an integer) |
| the name does not resolve | `void` |
| the file exists but is not a usable font | `0` |

An empty name resolves to nothing and therefore yields `void`, matching the
reference's placed-path lookup.

Registered faces are stored per engine in `add_font::registry::FontRegistry`
(not in a process global). The registry keeps, per source file, the file
bytes, the engine the faces were parsed into, and the face ids in collection
order. A lookup resolves either a **family name** or the **storage name** the
file was read from; the most recent registration wins.

Two consumers consult the registry before falling back to reading the face
string as a storage name: the visual `Font` native
(`crates/compat/src/natives/visual/font.rs`) and the `TextRenderBase` plugin
(`crates/compat/src/natives/plugins/text_render/metrics.rs`).

## State and lifetime

* The registry lives in the per-engine dispatch state
  (`CompatServices::state::<FontRegistry>()`), created lazily on first use.
* Re-registering the same source name replaces its previous entry; the
  reference's hash-table `Add` has the same observable effect.
* The registry holds an `Arc<[u8]>` of the file bytes and the parsing engine
  for each source, so the memory is released when the engine is dropped.

## Errors

| condition | outcome |
|---|---|
| no argument | typed error: `System.addFont requires a font file name` |
| no storage attached | typed error: `System.addFont: no storage attached` |
| the storage raises while resolving the name | typed error: `System.addFont: <storage error>` |
| the storage raises while reading the bytes | the reference returns `0` for a file that exists but cannot be opened, so the port logs the read failure at debug level and returns `0` |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/add_font.rs` | the face count, the `void` result for an unresolvable name, `0` for an unusable file, the typed error without an argument, the typed error without a storage, the ignored `extract` argument, and that the member is static on `System` |
| `crates/compat/tests/font.rs` | that a face registered through `System.addFont` resolves for later text rendering |
| `crates/compat/tests/dispatch_fmt.rs` | the member's dispatch shape on the `System` class |

## Known gaps

* The reference keeps the font map in a process-wide table; the port keeps it
  per engine. A script cannot observe the difference — a font registered in
  one engine is not visible to another, and a game has one engine.
* The reference's `extract` argument is a hint that the font file should be
  extracted to a temporary path. The reference ignores it too, and so does the
  port.
