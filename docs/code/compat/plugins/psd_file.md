# PSD (`psd.dll`)

**DLL name:** `psd.dll` — linked with `Plugins.link("psd.dll")`
**Rust module:** `crates/compat/src/natives/plugins/psd_file/`
**Reference:** `reference/cpp/plugins/psdfile/` (`main.cpp`, `psdclass.{h,cpp}`, `psdclass_load*.cpp`, `psdparse/`)
**Registers:** the `PSD` class, with instance members and static constants

## Surface

### Instance members

| member | kind | behaviour |
|---|---|---|
| `PSD()` | constructor | creates an empty instance |
| `load(filename)` | method | reads, parses and stores the document |
| `getLayerType(no)` | method | the layer's type flag |
| `getLayerName(no)` | method | the layer's name (the Unicode name when present) |
| `getLayerInfo(no)` | method | a dictionary describing the layer |
| `getLayerData(layer, no)` | method | the layer image with its mask folded into alpha |
| `getLayerDataRaw(layer, no)` | method | the layer image without its mask |
| `getLayerDataMask(layer, no)` | method | the mask only, as grayscale |
| `getSlices()` | method | the document's slices |
| `getGuides()` | method | the document's guides |
| `getBlend(layer)` | method | the stored composite image, placed on the layer |
| `getLayerComp()` | method | the document's layer compositions |

### Properties

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
| `clearStorageCache()` | static method | a no-op — the port keeps no process-global PSD storage cache |
| `color_mode_bitmap` … `color_mode_lab` | static property (read-only) | the colour-mode constants: `0` bitmap, `1` grayscale, `2` indexed, `3` RGB, `4` CMYK, `7` multichannel, `8` duotone, `9` Lab |
| `blend_mode_normal` … `blend_mode_divide` | static property (read-only) | the blend-mode constants, `0` normal through `27` divide |
| `layer_type_normal` … `layer_type_fill` | static property (read-only) | the layer-type constants: `0` normal, `1` hidden, `2` folder, `3` adjust, `4` fill |

## Behaviour

The plugin parses a Photoshop document — header, colour mode, image
resources, layers and the merged composite — expands the RLE and zip channel
data, and exposes the decoded pixels as engine bitmaps on a script `Layer`.

### Reading the document

`load(filename)` reads the file through the attached storage, falling back to
the host file system (the reference's placed-path lookup then local read),
parses it, and stores the result on the instance. A file that cannot be read
raises.

### The layer accessors

`getLayerType(no)` returns the layer's type flag, `getLayerName(no)` its
name — preferring the Unicode name when the document carries one — and
`getLayerInfo(no)` a dictionary with the layer's metadata.

The three `getLayerData*` methods take a script `Layer` and a layer number.
The image is written into the layer's own bitmap: the reference writes it
through `mainImageBufferForWrite`
(`reference/cpp/plugins/psdfile/psdclass.cpp:380-385`), so the layer owns it
and no script `Bitmap` object claims it.

* `getLayerData` folds the mask into the alpha channel;
* `getLayerDataRaw` leaves the mask out;
* `getLayerDataMask` produces the mask alone, as grayscale.

`getBlend(layer)` places the stored composite image on the layer.

### Blend modes

`convBlendMode` maps each PSD blend onto its `tTVPLayerType`
(`reference/cpp/plugins/psdfile/psdclass.cpp:14-48`); a mode the plugin does
not support reports `ltPsNormal`. The blend a decoded layer is placed with is
that layer type, read through the render pipeline's blend vocabulary.

### Layer lookup

A layer object argument is resolved through the VM's native-instance lookup
for the `Layer` class — the reference's `ncbPropAccessor`-based `Layer`
downcast. A non-`Layer` object is an error.

## State and lifetime

* The parsed document lives on the instance and is released when the object
  is invalidated or destroyed.
* The decoded pixels live in the engine's scene, in the target layer's
  bitmap. A newly allocated layer bitmap is filled with the layer's neutral
  colour before the image is copied in.
* `clearStorageCache()` is a no-op: the compat port keeps no process-global
  PSD storage cache, so there is nothing to clear.

## Errors

| condition | outcome |
|---|---|
| `load` on a file that cannot be read | the storage's error |
| `load` on a file that is not a valid PSD document | the parser's typed error |
| `getLayerType` / `getLayerName` / `getLayerInfo` with an out-of-range layer number | typed error |
| `getLayerData*` / `getBlend` with an argument that is not a `Layer` | typed error |
| a member used before a successful `load` | typed error |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/psd_file.rs` | the class surface, the constants, `load`, and the header properties |
| `crates/compat/tests/psd_behaviour.rs` | the layer accessors, the decoded pixels written into the layer, the mask handling and the blend placement |
| `crates/compat/src/natives/plugins/psd_file/mod.rs` (unit tests) | that each PSD blend maps onto its layer type and reaches the scene with that blend |

## Known gaps

* **The `psd://` storage media is not ported.** The reference registers a
  `psd://` media so a layer image can be opened as
  `psd://file.psd/root/folder/layer.bmp`; the compat storage namespace has no
  media-provider seam, so that part is missing. The `PSD` class surface
  itself is complete.
* The reference's `loadMemory` and `loadStream` are plain C++ helpers rather
  than TJS members, so they are not registered — and the port does not
  register them either.
* `clearStorageCache()` is a no-op, because the port keeps no process-global
  PSD cache to clear.
