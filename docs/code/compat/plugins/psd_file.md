# PSD (`psd.dll`)

**DLL name:** `psd.dll` — linked with `Plugins.link("psd.dll")`
**Rust module:** `crates/compat/src/natives/plugins/psd_file/`
**Reference:** `reference/cpp/plugins/psdfile/`
**Registers:** the `PSD` class

The plugin reads a Photoshop document — header, colour-mode data, image
resources, the layer/mask section and the merged composite — expands the
RLE and zip channel data, and hands the decoded pixels to a script `Layer`.
The parsing and the pixel expansion live in the shared decoder crate
([`psd`](../../platform/media/psd.md), `crates/platform/media/psd/`); this
module marshals the arguments, places the pixels in the engine's scene and
marshals the results back.

The class is registered on the first `Plugins.link("psd.dll")` — the built-in
table entry registers it, exactly as the reference's `V2Link` does. Only
`xp3filter.dll` is a startup module, so a script that reads `PSD` before it
links the DLL does not see the class yet. See the
[plugin index](../plugins.md) for the table and the conventions the plugin
pages follow.

## Surface

### Instance members

| member | kind | behaviour |
|---|---|---|
| `PSD` | constructor | `new PSD()` creates an empty instance |
| `load(filename)` | method | reads and parses a document; returns `true` when the document was loaded |
| `getLayerType(no)` | method | the layer's kind: `0` normal, `1` hidden, `2` folder, `3` adjust, `4` fill |
| `getLayerName(no)` | method | the layer's name — the Unicode name when the record carries one, otherwise the Pascal name |
| `getLayerInfo(no)` | method | a dictionary describing the layer |
| `getLayerData(layer, no)` | method | the layer image with the mask folded into the alpha channel |
| `getLayerDataRaw(layer, no)` | method | the layer image without its mask |
| `getLayerDataMask(layer, no)` | method | the mask alone, as grayscale |
| `getSlices()` | method | the slice resource as a dictionary; `void` when the document carries none |
| `getGuides()` | method | the grid/guide resource as a dictionary; `void` when the document carries none |
| `getBlend(layer)` | method | the merged composite, placed on the layer; returns whether it was written |
| `getLayerComp()` | method | the layer compositions as a dictionary; `void` when the document carries none |

### Instance properties

All six are read-only `Integer` properties; a script write raises.

| member | kind | behaviour |
|---|---|---|
| `width` | property (read-only) | the document width in pixels |
| `height` | property (read-only) | the document height in pixels |
| `channels` | property (read-only) | the channel count |
| `depth` | property (read-only) | the bit depth |
| `color_mode` | property (read-only) | the colour mode |
| `layer_count` | property (read-only) | the number of layer records |

### Static members

| member | kind | behaviour |
|---|---|---|
| `clearStorageCache()` | static method | returns void; the port keeps no process-global PSD cache, so there is nothing to clear |

The class constants are read-only static `Integer` properties.

Colour modes (`reference/cpp/plugins/psdfile/psdparse/psddata.h:44-51`):

| member | value |
|---|---|
| `color_mode_bitmap` | 0 |
| `color_mode_grayscale` | 1 |
| `color_mode_indexed` | 2 |
| `color_mode_rgb` | 3 |
| `color_mode_cmyk` | 4 |
| `color_mode_multichannel` | 7 |
| `color_mode_duotone` | 8 |
| `color_mode_lab` | 9 |

Blend modes (`reference/cpp/plugins/psdfile/psdparse/psddata.h:79-108`):

| member | value | member | value |
|---|---|---|---|
| `blend_mode_normal` | 0 | `blend_mode_vivid_light` | 13 |
| `blend_mode_dissolve` | 1 | `blend_mode_linear_light` | 14 |
| `blend_mode_darken` | 2 | `blend_mode_pin_light` | 15 |
| `blend_mode_multiply` | 3 | `blend_mode_hard_mix` | 16 |
| `blend_mode_color_burn` | 4 | `blend_mode_difference` | 17 |
| `blend_mode_linear_burn` | 5 | `blend_mode_exclusion` | 18 |
| `blend_mode_lighten` | 6 | `blend_mode_hue` | 19 |
| `blend_mode_screen` | 7 | `blend_mode_saturation` | 20 |
| `blend_mode_color_dodge` | 8 | `blend_mode_color` | 21 |
| `blend_mode_linear_dodge` | 9 | `blend_mode_luminosity` | 22 |
| `blend_mode_overlay` | 10 | `blend_mode_pass_through` | 23 |
| `blend_mode_soft_light` | 11 | `blend_mode_darker_color` | 24 |
| `blend_mode_hard_light` | 12 | `blend_mode_lighter_color` | 25 |
| | | `blend_mode_subtract` | 26 |
| | | `blend_mode_divide` | 27 |

Layer types (`reference/cpp/plugins/psdfile/psdparse/psddata.h:12-16`):

| member | value |
|---|---|
| `layer_type_normal` | 0 |
| `layer_type_hidden` | 1 |
| `layer_type_folder` | 2 |
| `layer_type_adjust` | 3 |
| `layer_type_fill` | 4 |

## Behaviour

### Reading the document

`load(filename)` reads the file through the attached storage, falling back to
the host file system when the storage does not hold it — the reference's
placed-path lookup followed by a local read
(`reference/cpp/plugins/psdfile/psdclass.cpp:124-148`). The local `file`
media prefix is stripped before the name reaches the storage.

A file that cannot be read and a file that is not a valid document both
answer `false`; neither raises. The instance's document is dropped before
the read, so a failed load never leaves a partial document behind
(`reference/cpp/plugins/psdfile/psdparse/psdfile.cpp:12-49`). The failure
is logged as a warning.

The document itself is parsed by the shared decoder crate: the header, the
colour-mode data, the image resources, the layer/mask section and the merged
composite (`crates/platform/media/psd/src/parse.rs`).

### The header properties

`width`, `height`, `channels`, `depth`, `color_mode` and `layer_count` read
the parsed header. Before a successful load — and after a failed one — each
of them answers `-1`
(`reference/cpp/plugins/psdfile/psdclass.h:55-63`).

### The layer accessors

`getLayerType(no)` answers the layer's kind, `getLayerName(no)` its name.
The name prefers the record's Unicode name and falls back to the Pascal
name (`reference/cpp/plugins/psdfile/psdclass.cpp:181-189`).

`getLayerInfo(no)` answers a dictionary
(`reference/cpp/plugins/psdfile/psdclass.cpp:216-301`) with these members:

| member | value |
|---|---|
| `top`, `left`, `bottom`, `right` | the record's rectangle |
| `width`, `height` | the record's size |
| `opacity` | the layer opacity, `0`–`255` |
| `fill_opacity` | the fill opacity, `0`–`255` |
| `mask` | `1` when the record carries a mask channel |
| `type` | the blend mode mapped onto its layer type — `13` for normal |
| `layer_type` | the layer's kind, `0`–`4` |
| `blend_mode` | the raw blend mode, `0`–`27` |
| `visible` | `1` when the record's visibility flag is clear |
| `name` | the layer's name |
| `clipping` | the record's clipping flags |
| `layer_id` | the id from the `lyid` block, `-1` when the record has none |
| `obsolete`, `transparency_protected`, `pixel_data_irrelevant` | the record's flag bits |
| `group_layer_id` | the enclosing folder's layer id; present only when the layer has a parent |
| `layer_comp` | a dictionary keyed by the composition id; present only when the record carries compositions |

Each `layer_comp` entry carries `id`, `offset_x`, `offset_y` and `enable`.

### Placing pixels on a layer

`getLayerData`, `getLayerDataRaw` and `getLayerDataMask` take a script
`Layer` and a layer number; `getBlend` takes a script `Layer`.

The layer argument is resolved through the VM's native-instance lookup for
the `Layer` class — the reference's `ncbPropAccessor`-based `Layer`
downcast (`reference/cpp/plugins/psdfile/psdclass.cpp:309-313`). An object
that is not a `Layer` is an error.

The decoded image is written into the target layer's own bitmap. The
reference writes it through the layer's `mainImageBufferForWrite`
(`reference/cpp/plugins/psdfile/psdclass.cpp:374-383`), so the layer owns
the pixels and no script `Bitmap` object claims them; destroying the layer
releases the image. A newly allocated bitmap is filled with the layer's
neutral colour before the image is copied in, and an existing bitmap is
resized.

The layer's rectangle, image size, opacity, visibility, blend and name are
taken from the layer record. The three image modes differ in what is
decoded:

* `getLayerData` folds the mask into the alpha channel;
* `getLayerDataRaw` leaves the mask out;
* `getLayerDataMask` produces the mask alone, as grayscale. A layer with no
  stored mask yields the 1×1 default-colour dummy
  (`reference/cpp/plugins/psdfile/psdclass.cpp:327-340`).

`getBlend` writes the document's merged composite and answers `true`. It
answers `false` when the document carries no composite.

### Blend modes

`convBlendMode` maps a Photoshop blend mode onto its layer type
(`reference/cpp/plugins/psdfile/psdclass.cpp:14-57`): normal to `ltPsNormal`
(`13`), multiply to `ltPsMultiplicative`, and so on. A mode the plugin does
not support — dissolve, the vivid/linear/pin light and hard-mix modes, the
hue/saturation/colour/luminosity modes, pass-through, darker and lighter
colour, subtract and divide — reports `ltPsNormal`. The same mapping
produces the `type` member of `getLayerInfo` and the blend a decoded layer
is placed with.

## State and lifetime

* The parsed document lives in the native instance payload and is dropped
  when the instance is invalidated or destroyed.
* The decoded pixels live in the engine's scene, in the target layer's
  bitmap. The layer owns that bitmap, so the image is released with the
  layer.
* The port keeps no process-global PSD cache, so `clearStorageCache()` has
  nothing to clear.

## Errors

| condition | outcome |
|---|---|
| `load()` without a filename | raises `PSD.load requires 1 argument` |
| `load` on a file that cannot be read | answers `false` |
| `load` on a document that does not parse | answers `false` |
| `getLayerType` / `getLayerName` / `getLayerInfo` without an argument | raises `PSD.<name> requires 1 argument` |
| `getLayerData` / `getLayerDataRaw` / `getLayerDataMask` with fewer than two arguments | raises `PSD.<name> requires 2 arguments` |
| `getBlend` without an argument | raises `PSD.getBlend requires 1 argument` |
| a member that needs a document, before a successful `load` | raises `PSD: no data` |
| a layer number outside the document | raises `PSD: not such layer` |
| a layer whose kind is not normal — a folder is accepted for `getLayerDataMask` | raises `PSD: invalid layer type` |
| a layer argument that is not a `Layer` | raises `PSD: not a layer` |
| `getLayerData*` / `getBlend` with no scene attached | raises `PSD: no scene attached` |

The messages mirror the reference's own wording — `no data`,
`not such layer` and `invalid layer type`
(`reference/cpp/plugins/psdfile/psdclass.cpp:168-175`,
`reference/cpp/plugins/psdfile/psdclass.cpp:319-323`).

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/psd_file.rs` | the header properties and the class constants, the layer metadata dictionary, the guide and slice resources, the decoded RLE pixels placed on a layer, the raw and mask variants, the merged composite, and a missing file answering `false` |
| `crates/compat/tests/psd_behaviour.rs` | the header grammar, the `-1` answers before a load, the typed `false` load failures, the read-only header properties, the constant values, `clearStorageCache` being a class-only member, the layer records and their flags, the layer-index errors, the `void` answers when the slice and guide resources are absent, the image-resource padding, and the image being released with its layer |
| `crates/compat/src/natives/plugins/psd_file/mod.rs` (unit tests) | that each blend mode maps onto its layer type and reaches the scene with that blend |

## Known gaps

* **The `psd://` storage media is not ported.** The reference registers a
  `psd://` media so a layer image can be opened as
  `psd://file.psd/root/folder/layer.bmp`
  (`reference/cpp/plugins/psdfile/main.cpp:15-125`,
  `reference/cpp/plugins/psdfile/psdclass.cpp:744-799`). The `PSD` class
  surface itself is complete. The engine's storage media table can serve
  such a name (`crates/engine/src/storage.rs`), but the plugin registers
  none.
* The reference's `loadMemory` and `loadStream` are plain C++ helpers rather
  than TJS members (`reference/cpp/plugins/psdfile/psdclass.h:150-167`), so
  neither they nor the port registers them.
* `clearStorageCache()` is a no-op, because the port keeps no
  process-global PSD cache to clear. The reference clears the `psd://`
  media's cache (`reference/cpp/plugins/psdfile/main.cpp:234-238`).
* The error messages carry a `PSD: ` prefix, and the layer-argument message
  reads `not a layer` where the reference says `not layer`
  (`reference/cpp/plugins/psdfile/psdclass.cpp:309-313`).
* `getLayerData*` validates the document, the layer number and the layer
  kind before it validates the layer argument, while the reference checks
  the layer argument first
  (`reference/cpp/plugins/psdfile/psdclass.cpp:309-314`). When more than one
  condition holds, the two report different messages.
* `getBlend` raises `PSD: no data` on an instance that was never loaded. The
  reference has no loaded check there and answers `false` when the document
  carries no composite
  (`reference/cpp/plugins/psdfile/psdclass.cpp:517-546`).
* The reference also writes `fill_opacity` and, for the mask mode,
  `defaultMaskColor` onto the target layer
  (`reference/cpp/plugins/psdfile/psdclass.cpp:357-371`). The port writes the
  opacity and leaves those two off.
