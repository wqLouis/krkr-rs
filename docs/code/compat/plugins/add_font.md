# addFont (`addFont.dll`)

**DLL name:** `addFont.dll` (aliases: none) — linked with `Plugins.link("addFont.dll")`
**Rust module:** `crates/compat/src/natives/plugins/add_font/`
**Reference:** `reference/cpp/plugins/addFont.cpp`
**Registers:** one static member on the core `System` class — `System.addFont`

The member appears when the script links `addFont.dll`: a ported plugin installs
its members on the first `Plugins.link` that names it, not at engine startup.
See the [plugin index](../plugins.md) for the linking model.

## Surface

| member | kind | behaviour |
|---|---|---|
| `System.addFont(fontFileName[, extract])` | static method | reads a font file through the attached storage, registers every face in it for later text rendering, and returns the number of faces |

The member lives on the `System` class object only — the reference attaches it
with `TJS_STATICMEMBER` (`reference/cpp/plugins/addFont.cpp:35-37`), so a
`System` instance cannot reach it.

The second argument, `extract`, is accepted and ignored. The reference
documents it ("extract from the archive to a temporary path") but never reads
it (`reference/cpp/plugins/addFont.cpp:12-13,19-20`).

## Behaviour

### Resolution and result

The reference resolves the name with `TVPGetPlacedPath` and then runs
`TVPEnumFontsProc(filename)` on the file
(`reference/cpp/plugins/addFont.cpp:22-28`). The file is re-read and
re-enumerated on **every** call — the port keeps no cached count, so a source
whose contents changed reports the new face count.

| situation | result |
|---|---|
| the name resolves and the file holds usable faces | the number of faces, an **Integer** |
| the name does not resolve | `void` |
| the file exists but is not a usable font | `0`, an **Integer** |

An empty name resolves to nothing and therefore yields `void`, like the
reference's placed-path lookup.

The port resolves the name through the attached storage namespace: it asks the
namespace whether the name exists and then reads the bytes, after stripping the
L3 `file://` media prefix
(`crates/compat/src/storage.rs`). The reference's `TVPEnumFontsProc` returns
its failure value `0` when the storage exists but the stream cannot be opened
(`reference/cpp/core/visual/FontImpl.cpp:142-156`), and the port reports the
same `0` in that case.

### The face registry

The reference stores the faces in the process-wide `TVPFontNames` map the
renderer consults when a `Font.face` is set
(`reference/cpp/core/visual/FontImpl.cpp:114-128`). The port keeps the same
information in the per-engine `FontRegistry` dispatch state
(`crates/compat/src/natives/plugins/add_font/registry.rs`). For each
registered file it holds:

* the file bytes, shared by every face of that file;
* the L2 `TextEngine` the faces were parsed into;
* the face ids in collection order.

A lookup resolves either a **family name** (through that engine's face
database) or the **storage name** the file was read from, and the most recent
registration wins. The reference's hash-table `Add` replaces the entry for the
same family (`reference/cpp/core/visual/FontImpl.cpp:118,127`), and re-adding
the same source in the port replaces its previous registration.

The shared face resolver `crates/compat/src/resource.rs` consults the registry
first, then the attached storage, then the host's installed system fonts. A
face no source knows — a pre-rendered `*name`, for example — falls back to the
**last registered face**, which the reference makes the default font
(`reference/cpp/core/visual/FontImpl.cpp:243-244`, `:159-168`). Both the visual
`Font` native
(`crates/compat/src/natives/visual/font.rs`) and the `TextRenderBase` plugin
(`crates/compat/src/natives/plugins/text_render/metrics.rs`) resolve their
faces through it, so a face registered by `System.addFont` is found by family
name or by the storage name it was read from.

## State and lifetime

* The registry lives in the per-engine dispatch state
  (`CompatServices::state::<FontRegistry>()`), created lazily on first use and
  released with the engine.
* The bytes and the parsing engine are held per source file, so the memory
  goes away when the engine drops.
* The registry holds no process-wide state: two engines never see each other's
  faces.

## Errors

| condition | outcome |
|---|---|
| no argument | typed error: `System.addFont requires a font file name` |
| no storage attached | typed error: `System.addFont: no storage attached` |
| the storage raises while resolving the name | typed error: `System.addFont: <storage error>` |
| the storage raises while reading the bytes | the reference returns `0` for a file that exists but cannot be opened, so the port logs the failure at debug level and returns `0` |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/add_font.rs` | the face count and its `Integer` type, `void` for an unresolvable or empty name, `0` for an unusable file, the typed error without an argument, the typed error without a storage, the ignored `extract` argument, and that the member is a class static rather than an instance member |
| `crates/compat/tests/dispatch_fmt.rs` | that `System.addFont` is reachable on the class object (`typeof` is `"Object"`) and that the no-op plugins (`GetAbout`, `VarFile`, `Extrans`) publish no global of their own |

## Known gaps

* The reference keeps the font map in a process-wide table; the port keeps it
  per engine. A game has one engine, so a script cannot observe the difference
  — a face registered in one engine is invisible to another.
* The reference's `extract` argument is documented as a request to extract the
  font to a temporary path. The reference never reads it, and neither does the
  port.
* The reference's face map is keyed by the family names the font file
  declares (`reference/cpp/core/visual/FontImpl.cpp:75-128`). The port's
  registry also answers to the storage name the file was read from, so a
  script that uses that name as a `Font.face` resolves a face the reference
  would not find.
