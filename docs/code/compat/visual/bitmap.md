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
| `Bitmap(name[, colorkey])` | loads the named storage image into a fresh bitmap and returns the scene bitmap id as an integer. An empty name raises. The decode may be deferred to the L2 pool — see §4. |
| `Bitmap(width, height[, bpp])` | allocates a `width × height` all-zero (transparent) RGBA image. `bpp` is accepted and ignored. |
| `Bitmap()` | creates the reference's 32×32 transparent-white initial bitmap (`[255, 255, 255, 0]` per pixel). |
| `Bitmap(x)` — one non-string argument | constructs but leaves the bitmap **null** (no image) and returns `0`. Every image-dependent member then raises, while `buffer` and `bufferPitch` report `0`. |

A second construction on the same payload raises
`"Bitmap: this bitmap is already constructed"`.

---

## 2. The members

| Member | Behaviour |
|---|---|
| `getPixel(x, y)` | the pixel as the packed word of the RGBA planes, read little-endian: the **red is in the low byte**, the green in the second, the blue in the third — `0x00BBGGRR` for the 24 colour bits, the alpha masked away. `0` for an out-of-range or negative coordinate — **no raise**. A pixel the buffer does not hold is out of range too, so its answer is `0`. Waits for a pending decode. |
| `setPixel(x, y, color)` | writes the colour's red, green and blue into the first, second and third plane, and marks it dirty; the alpha byte is held. An out-of-range write is silently ignored; a pixel the buffer does not hold is out of range too, so the write is ignored. Waits for a pending decode. |
| `getMaskPixel(x, y)` | the alpha channel `0..255` — the **alpha byte of the same packed word**, not a plane of its own. A pixel the buffer does not hold reads as `0`. Waits for a pending decode. |
| `setMaskPixel(x, y, alpha)` | writes only the alpha byte of the packed word, **clamped** to `0..255`. A pixel the buffer does not hold is ignored. Waits for a pending decode. |
| `independ()` | a no-op — every `Bitmap` already owns a private scene bitmap. |
| `setSize(width, height)` | resizes, preserving the overlapping top-left region and filling the rest with `[0, 0, 0, 0]`. A zero dimension raises. A pending decode is resolved first, so the decoded overlap survives. |
| `copyFrom(bitmap)` | copies the source bitmap's pixels and dimensions into this one. The source is resolved first, and a source whose pixel buffer is not the bytes its size declares **raises** — copying its empty buffer would give this bitmap the same inconsistent state. |
| `save(name[, type[, meta]])` | encodes and writes through the host saver. An image whose pixel buffer is not the bytes its size declares **raises** instead of encoding an empty buffer. |
| `load(name[, colorkey])` | decodes the storage image and installs it; returns a `{width, height, bpp}` metainfo dictionary (`bpp` is always 32). The decode may be deferred to the L2 pool — see §4. |
| `loadAsync(name)` | runs the load synchronously and dispatches `onLoaded(meta, async, error, message)` immediately with `async == 0`. It never defers: the result is reported to the script, so the decode must have run. |
| `onLoaded(meta, async, error, message)` | the registered completion hook; returns void. |
| `width`, `height` | the scene bitmap's dimension. While a decode is pending this is the **probed** size. The setters resize, preserving the overlap; a pending decode is resolved first. |
| `buffer` | the RGBA buffer address as an integer; `0` when there is no image **and** when the resolved image's pixel buffer is not the `width * height * 4` bytes its size declares. Read-only. Waits for a pending decode. |
| `bufferForWrite` | the RGBA buffer address, marking the bitmap dirty; `0` when there is no image **and** when the resolved image's pixel buffer is not the `width * height * 4` bytes its size declares. Read-only. Waits for a pending decode. |
| `bufferPitch` | `width * 4` (saturating) bytes per row; `0` when there is no image. Read-only. |
| `loading` | `1` while an async load is in flight — always `0` outside the `loadAsync` call in this port. Read-only. |
| `Bitmap.loadHeader(name)` (static) | probes the header through the host prober and returns `{width, height, bpp}` (`bpp` is 32 with alpha, else 24). Returns **void** (no raise) when no handler accepts the data. |
| `Bitmap.getSaveOption(type)` (static) | the option dictionary for a known type (`bmp`/`png`/`jpg`/`tlg*`), `void` for an unknown type. |

There is **no** `id` / `nativeId` member — the scene bitmap id is read back
through the VM's native-instance lookup instead.

### The packed pixel word

A pixel is four RGBA **bytes**, and a point is read as the little-endian word
of those four bytes (`reference/cpp/core/visual/LayerBitmapIntf.cpp:165-171`,
`reference/cpp/core/visual/RenderManager.cpp:346-350`). The word is therefore
`0xAABBGGRR`: the red in the low byte, the green in the second, the blue in
the third and the alpha in the fourth. `getPixel` reports the word's low 24
bits, so it reports `0x00BBGGRR`
(`reference/cpp/core/visual/BitmapIntf.cpp:51-58`,
`reference/cpp/core/visual/impl/LayerImpl.cpp:33-36`).

`setPixel` writes the other way round: the colour names its planes, and the
named red goes to the first plane
(`reference/cpp/core/visual/BitmapIntf.cpp:60-66`,
`reference/cpp/core/visual/LayerBitmapIntf.cpp:194-209`). A colour whose high
byte is set names a system colour and is resolved through the colour table
first (`reference/cpp/core/visual/impl/LayerImpl.cpp:20-31`).

The mask is the **alpha byte of the same word**, not a plane of its own:
`setMaskPixel` keeps the colour bits and writes the mask into the word's high
byte (`reference/cpp/core/visual/BitmapIntf.cpp:67-85`,
`reference/cpp/core/visual/LayerBitmapIntf.cpp:211-226`). The colour's byte
order does not touch it.

### The image that holds no pixels

A bitmap can **declare** a size and hold a pixel buffer that is not the
`width * height * 4` bytes the size names: a blank image whose allocation
failed keeps the probed size and an empty buffer. The members answer that
state instead of reading past the buffer:

* a pixel read answers the out-of-range value and a pixel write is ignored;
* `buffer` and `bufferForWrite` answer `0` — the reference's "no image"
  answer — so no address of an empty buffer is handed out;
* `copyFrom` and `save` **raise**, so an empty buffer is neither copied into
  another bitmap nor encoded into a file.

`bufferPitch` is unaffected: it is the row pitch the size declares.

---

## 3. The image codec seam

L3 never links the L1 `media` codec. The host installs the three closures:

```rust
host.set_image_codec(decode, probe, save);
```

| Closure | Backs |
|---|---|
| `decode` | the **synchronous** load path: `Bitmap.load`, `Bitmap.loadAsync` and every load that cannot be deferred (§4) |
| `probe` | `Bitmap.loadHeader` and the header probe of a deferred load |
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

## 4. The load path

`Bitmap.load` and the `Bitmap(name)` constructor share one path:

1. The **decoded-image cache** is consulted first. A hit is installed
   synchronously, exactly as a cached graphic is installed (§6).
2. Without a hit, the load reads the bytes through the attached storage and
   **probes the header**, both on the calling thread.
3. The decode is then either **deferred** to the L2 decode pool or run
   **synchronously** on the calling thread.

### The deferred path

When the load can be deferred, the call returns with the image's **size
known** and its **pixels still decoding**:

* the bitmap is installed with the **probed** width and height and no pixels;
* the decode runs on a pool worker, and the bitmap carries the future;
* every read of the pixels — `getPixel`, `setPixel`, `getMaskPixel`,
  `setMaskPixel`, `buffer`, `bufferForWrite` — **waits** for the decode, and
  the frame's image barrier waits for all of them;
* the metainfo the load returns carries the probed size.

A **post-probe decode failure** installs a fully transparent blank of the
probed size and logs the failure. It does **not** raise: the load already
returned, so there is no call to fail. A **probe** failure is different — the
probe runs on the calling thread, so it raises at the load, the way a load
failure raises there.

### When the load is synchronous

The load decodes on the calling thread when:

* **no pool is attached** — the host installed no decode pool;
* the load is a **province** load (`DecodedImageMode::Province`);
* the **pool refused the job** — the queue was full, the memory limit would
  be exceeded, or the size accounting did not fit. The refusal is logged with
  the limit it refused the job against, and the load decodes synchronously.

A **colour key** does not keep the load synchronous: the key is a pass the
worker runs over the decoded pixels, so a keyed load is deferred like an
unkeyed one.

`Bitmap.loadAsync` never defers: it reports the load's result to the script,
so its decode must have run.

---

## 5. The save path

| Argument | Rule |
|---|---|
| `name` | the storage name; the media prefix is stripped. |
| `type` | used **verbatim** when present and non-void (including `""`); otherwise `"bmp"`. |
| `meta` | must be an object. `meta.bpp` is an **index** into the reference's BMP mode table: `0` → `"bmp"` (4 bytes per pixel), `1` → `"bmp24"` (3), `2` → `"bmp8"` (1); anything else leaves the type unchanged. |

The saver receives `(name, type, width, height, rgba)`.

---

## 6. The cache

The decoded-image cache lives in the L2 scene, keyed by
`DecodedImageKey { name, color_key, mode, desired_width, desired_height }`.

* The cache returns an **owned copy**, so a `setPixel` on one bitmap cannot
  corrupt a cached entry.
* With `System.graphicCacheLimit == 0` the cache is disabled and every load
  decodes.
* **A deferred decode enters the cache.** When the scene resolves a
  pending image it stores the decoded pixels under the key the load named,
  through the same insert and eviction path as the synchronous load. A
  failed, cancelled or malformed decode stores nothing.

---

## 7. State ownership

| State | Owner |
|---|---|
| the pixels, the dimensions, the dirty flag, the script-ownership flag, the content generation | L2 `Scene` / `BitmapState` |
| the pending decode of a bitmap whose pixels have not landed yet | L2 `Scene` / `BitmapState` (`pending`) |
| the decoded-image cache | L2 `Scene` |
| the decode pool, its workers and its queue | L2 `engine::imagedecode::DecodePool`, attached to the host by the composition root |
| the scene bitmap id, the async `loading` flag | the L3 `BitmapInst` payload |
| the host codec closures | the L3 `ResourceResolver` |

---

## 8. Invariants

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
* **An image whose pixel buffer is not the bytes its size declares has no
  address and no pixels to copy or encode.** The buffer addresses are `0`, a
  pixel read is the out-of-range value, a pixel write is ignored, and
  `copyFrom` and `save` raise: an empty buffer is never handed out, copied or
  encoded.
* **`copyFrom`** resolves the source through the VM's native-instance lookup,
  so a non-`Bitmap` object is a typed error, not a sentinel.
* **A pending bitmap knows its size.** The width and the height are the
  probed ones, so they are correct before the decode lands; the pixels are
  empty until the scene resolves the image.
* **Every pixel read waits for the pending decode.** The read resolves the
  bitmap first, so a script never sees a half-decoded image.
* **A failed pending decode installs a blank, not an exception.** The bitmap
  keeps the probed size and every pixel becomes transparent; the failure is
  logged.

---

## 9. Errors

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
| `"Bitmap.copyFrom requires a Bitmap argument"`, `"Bitmap.copyFrom: not a Bitmap object"`, `"Bitmap.copyFrom: the source bitmap does not exist"`, `"Bitmap.copyFrom: the source image does not hold its pixels"` | the source argument |
| `"Bitmap.save requires a storage name"`, `"Bitmap.save: the storage name is empty"`, `"Bitmap.save: the meta argument is not an object"`, `"Bitmap.save: currently async load bitmap"`, `"Bitmap.save: this bitmap has no image"`, `"Bitmap.save: this bitmap no longer exists"`, `"Bitmap.save: this image does not hold its pixels"`, `"Bitmap.save: {error}"` | the save path |
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
* A **deferred** decode that fails after the probe logs the failure and
  installs a transparent blank of the probed size. The load already returned,
  so nothing raises.

---

## 10. Tests

| Test | What it asserts |
|---|---|
| `crates/compat/tests/bitmap.rs` | the load populates the bitmap and returns the metainfo; `id`/`nativeId` raise; `loadHeader` probes without decoding; `getSaveOption` returns real objects; the save path encodes and writes through the host seam; the media prefix is stripped; the type is passed verbatim; `meta.bpp` is read as an index; `loadAsync` dispatches `onLoaded`; the typed errors when the codec is absent; the four constructor shapes; dropping a script bitmap releases its image; a stored member result does not pin the bitmap; a single numeric argument leaves the bitmap null; the load into a resized and a same-size bitmap; the name resolution order and the extension probe; the cache limit behaviour and the pristine-pixel cache hit; an image whose pixel buffer is not the bytes its size declares answers the out-of-range pixel value, hands out no buffer address, and refuses to be saved or copied from. |
| `crates/compat/tests/visual.rs` | `bitmap_pixels_and_size`, `bitmap_copy_from_copies_pixels`. |
| `crates/compat/tests/dispatch_visual.rs` | the receiver routing. |
| `crates/compat/tests/layer_behaviour.rs` | the Layer-only members reject a bitmap source. |
| `crates/compat/tests/pending_image.rs` | the deferred load through the real natives: a load without a pool decodes synchronously, a load with a pool leaves the bitmap pending with the probed size, the first pixel read waits for the decode, a probe failure raises at the load, and a refused submit falls back to the synchronous decode. |

---

## See also

* [`../resource.md`](../resource.md) — the codec seam and the face resolution
* [`pixels.md`](pixels.md) — the layer-side image path that shares the codec
* [`../engine_api.md`](../engine_api.md) — the L2 bitmap state
* [`../../engine/scene.md`](../../engine/scene.md) — the pending bitmap and the image barrier
