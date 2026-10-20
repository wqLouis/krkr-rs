# `natives/visual/bitmap.rs` — the `Bitmap` class

**Source:** `crates/compat/src/natives/visual/bitmap.rs`

`Bitmap` is the script-visible image buffer. It owns no pixels itself: the
pixel data lives in the L2 scene's bitmap store, and the native payload holds
the scene bitmap id and the async-loading flag.

The registered surface is 19 members (the surface dump lists 21 entries —
the class-name constructor and the VM's `finalize` are two of them).

---

## 1. The constructor

The constructor has four shapes, distinguished by its arguments:

| Form | Behaviour |
|---|---|
| `Bitmap(name[, colorkey])` | loads the named storage image into a fresh bitmap and returns the scene bitmap id as an integer. An empty name raises. |
| `Bitmap(width, height[, bpp])` | allocates a `width × height` all-zero (transparent) RGBA image. `bpp` is accepted and ignored. |
| `Bitmap()` | creates the reference's 32×32 transparent-white initial bitmap (`[255, 255, 255, 0]` per pixel). |
| `Bitmap(x)` — one non-string argument | constructs but leaves the bitmap **null** (no image) and returns `0`. Every image-dependent member then raises, while `buffer` and `bufferPitch` report `0`. |

A second construction on the same payload raises
`"Bitmap: this bitmap is already constructed"`.

---

## 2. The members

| Member | Behaviour |
|---|---|
| `getPixel(x, y)` | the pixel as ARGB (`0xAARRGGBB`). `0` for an out-of-range or negative coordinate — **no raise**. |
| `setPixel(x, y, color)` | writes ARGB into the bitmap and marks it dirty. An out-of-range write is silently ignored. |
| `getMaskPixel(x, y)` | the alpha channel `0..255`. |
| `setMaskPixel(x, y, alpha)` | writes only the alpha byte, **clamped** to `0..255`. |
| `independ()` | a no-op — every `Bitmap` already owns a private scene bitmap. |
| `setSize(width, height)` | resizes, preserving the overlapping top-left region and filling the rest with `[0, 0, 0, 0]`. A zero dimension raises. |
| `copyFrom(bitmap)` | copies the source bitmap's pixels and dimensions into this one. |
| `save(name[, type[, meta]])` | encodes and writes through the host saver. |
| `load(name[, colorkey])` | decodes the storage image and installs it; returns a `{width, height, bpp}` metainfo dictionary (`bpp` is always 32). |
| `loadAsync(name)` | runs the load synchronously and dispatches `onLoaded(meta, async, error, message)` immediately with `async == 0`. |
| `onLoaded(meta, async, error, message)` | the registered completion hook; returns void. |
| `width`, `height` | the scene bitmap's dimension. The setters resize, preserving the overlap. |
| `buffer` | the RGBA buffer address as an integer; `0` when there is no image. Read-only. |
| `bufferForWrite` | the RGBA buffer address, marking the bitmap dirty; `0` when there is no image. Read-only. |
| `bufferPitch` | `width * 4` (saturating) bytes per row; `0` when there is no image. Read-only. |
| `loading` | `1` while an async load is in flight — always `0` outside the `loadAsync` call in this port. Read-only. |
| `Bitmap.loadHeader(name)` (static) | probes the header through the host prober and returns `{width, height, bpp}` (`bpp` is 32 with alpha, else 24). Returns **void** (no raise) when no handler accepts the data. |
| `Bitmap.getSaveOption(type)` (static) | the option dictionary for a known type (`bmp`/`png`/`jpg`/`tlg*`), `void` for an unknown type. |

There is **no** `id` / `nativeId` member — the scene bitmap id is read back
through the VM's native-instance lookup instead.

---

## 3. The image codec seam

L3 never links the L1 `media` codec. The host installs the three closures:

```rust
host.set_image_codec(decode, probe, save);
```

| Closure | Backs |
|---|---|
| `decode` | `Bitmap.load`, `Bitmap.loadAsync` |
| `probe` | `Bitmap.loadHeader` |
| `save` | `Bitmap.save` |

A member whose closure is missing raises a typed error — `"no image decoder
is installed"`, `"no image header prober is installed"`, `"no image encoder is
installed"` — instead of fabricating a result.

### The name resolution

The literal name is tried **first**; only on a storage miss are the 21 common
image extensions probed, in the reference's `TVPFindGraphicLoadHandler`
order:

```
.webp .bmp .dib .jpeg .jpg .jif .png .tlg .tlg5 .tlg6 .gif
.tif .tiff .tga .dds .pnm .ico .qoi .hdr .exr .ff
```

A name that resolves as given composes no extension. A miss raises
`"storage not found: {name}"`; any other storage error is surfaced, not
swallowed.

The media prefix is stripped before the lookup, and the **script spelling**
is kept in the diagnostics.

### The colour key

| Constant | Value | Meaning |
|---|---|---|
| `COLOR_KEY_NONE` | `0x1fff_ffff` | no key |
| `COLOR_KEY_ADAPT` | `0x01ff_ffff` | the most frequent colour on the first scanline |

A key with a zero high byte is masked to `keyidx & 0x00ff_ffff`. Matching
pixels become alpha `0`; every other pixel becomes alpha `255`.

---

## 4. The save path

| Argument | Rule |
|---|---|
| `name` | the storage name; the media prefix is stripped. |
| `type` | used **verbatim** when present and non-void (including `""`); otherwise `"bmp"`. |
| `meta` | must be an object. `meta.bpp` is an **index** into the reference's BMP mode table: `0` → `"bmp"` (4 bytes per pixel), `1` → `"bmp24"` (3), `2` → `"bmp8"` (1); anything else leaves the type unchanged. |

The saver receives `(name, type, width, height, rgba)`.

---

## 5. The cache

The decoded-image cache lives in the L2 scene, keyed by
`DecodedImageKey { name, color_key, mode, desired_width, desired_height }`.

* The cache returns an **owned copy**, so a `setPixel` on one bitmap cannot
  corrupt a cached entry.
* With `System.graphicCacheLimit == 0` the cache is disabled and every load
  decodes.

---

## 6. State ownership

| State | Owner |
|---|---|
| the pixels, the dimensions, the dirty flag, the script-ownership flag, the content generation | L2 `Scene` / `BitmapState` |
| the decoded-image cache | L2 `Scene` |
| the scene bitmap id, the async `loading` flag | the L3 `BitmapInst` payload |
| the host codec closures | the L3 `ResourceResolver` |

---

## 7. Invariants

* **Construct once** — a second construction raises.
* **Script ownership** — every bitmap created through the natives is marked
  `script_owned`; the scene releases the image only when no layer references
  it.
* **Size arithmetic** — `width * height * 4` is checked; an overflow raises
  `"Bitmap: the requested size is too large"`.
* **Pixel writes** — a negative or out-of-range coordinate is ignored (no
  raise); `setMaskPixel` clamps the alpha to `0..=255`.
* **`setSize`** rejects a zero width or height.
* **The decoded image must be consistent** — an inconsistent decode raises
  `"the decoded image is malformed"`.
* **`copyFrom`** resolves the source through the VM's native-instance lookup,
  so a non-`Bitmap` object is a typed error, not a sentinel.

---

## 8. Errors

| Message | Raised when |
|---|---|
| `"Bitmap: compat context is not bound"` | the binding is not installed |
| `"Bitmap: this bitmap is already constructed"` | the constructor ran twice |
| `"Bitmap: the storage name is empty"` | the name argument is empty |
| `"Bitmap: the requested size is too large"` | the size arithmetic overflowed |
| `"Bitmap: this bitmap has no image"` | the member needs an image and there is none |
| `"Bitmap: bitmap no longer exists"` | the scene bitmap is gone |
| `"Bitmap.getPixel requires 2 arguments"`, `"Bitmap.setPixel requires 3 arguments"`, `"Bitmap.getMaskPixel requires 2 arguments"`, `"Bitmap.setMaskPixel requires 3 arguments"`, `"Bitmap.setSize requires 2 arguments"` | the arity checks |
| `"Bitmap.setSize: cannot create empty layer image"` | a zero dimension |
| `"Bitmap.copyFrom requires a Bitmap argument"`, `"Bitmap.copyFrom: not a Bitmap object"`, `"Bitmap.copyFrom: the source bitmap does not exist"` | the source argument |
| `"Bitmap.save requires a storage name"`, `"Bitmap.save: the storage name is empty"`, `"Bitmap.save: the meta argument is not an object"`, `"Bitmap.save: currently async load bitmap"`, `"Bitmap.save: this bitmap has no image"`, `"Bitmap.save: this bitmap no longer exists"`, `"Bitmap.save: {error}"` | the save path |
| `"Bitmap.load requires a storage name"`, `"Bitmap.load: the storage name is empty"`, `"Bitmap.load: currently async load bitmap"`, `"Bitmap.load: {error}"` | the load path |
| `"Bitmap.loadAsync requires a storage name"`, `"Bitmap.loadAsync: the storage name is empty"`, `"Bitmap.loadAsync: currently async load bitmap"` | the async load path |
| `"Bitmap.loadHeader requires a storage name"`, `"Bitmap.loadHeader: no storage attached"`, `"Bitmap.loadHeader: {error}"` | the header probe |
| `"Bitmap.getSaveOption requires a type"`, `"Bitmap.getSaveOption: could not build the option object: {error}"` | the save-option table |
| `"no image decoder is installed"`, `"no image header prober is installed"`, `"no image encoder is installed"` | the host codec is missing |

### Non-raising paths

* `Bitmap.loadHeader` logs `"Bitmap.loadHeader: no header handler accepted
  {name:?}: {error}"` and yields **void**.
* `Bitmap.load` logs `"Bitmap.load: {error}"` and yields void when the
  metainfo object cannot be built.
* `loadAsync` reports the failure through `onLoaded` instead of raising.

---

## 9. Tests

| Test | What it asserts |
|---|---|
| `crates/compat/tests/bitmap.rs` | the load populates the bitmap and returns the metainfo; `id`/`nativeId` raise; `loadHeader` probes without decoding; `getSaveOption` returns real objects; the save path encodes and writes through the host seam; the media prefix is stripped; the type is passed verbatim; `meta.bpp` is read as an index; `loadAsync` dispatches `onLoaded`; the typed errors when the codec is absent; the four constructor shapes; dropping a script bitmap releases its image; a stored member result does not pin the bitmap; a single numeric argument leaves the bitmap null; the load into a resized and a same-size bitmap; the name resolution order and the extension probe; the cache limit behaviour and the pristine-pixel cache hit. |
| `crates/compat/tests/visual.rs` | `bitmap_pixels_and_size`, `bitmap_copy_from_copies_pixels`. |
| `crates/compat/tests/dispatch_visual.rs` | the receiver routing. |
| `crates/compat/tests/layer_behaviour.rs` | the Layer-only members reject a bitmap source. |

---

## See also

* [`../resource.md`](../resource.md) — the codec seam and the face resolution
* [`pixels.md`](pixels.md) — the layer-side image path that shares the codec
* [`../engine_api.md`](../engine_api.md) — the L2 bitmap state
