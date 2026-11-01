# `resource.rs` and `storage.rs` — face resolution, the image codec, and the storage-name boundary

**Sources:**

* `crates/compat/src/resource.rs` — the per-engine resource resolver
* `crates/compat/src/storage.rs` — the script-name → VFS-name boundary

Both are L3-only helpers that sit between the native classes and the L2
engine. Neither owns a class.

---

## 1. `storage.rs` — the `file://` media prefix

L3 owns the TVP storage-name grammar; the L2 `Storage` namespace takes a
**bare** name within a mount. A script name may carry the local `file` media
prefix, which the namespace does not understand, so every native that hands a
script-supplied name to L2 routes it through `strip_file_media` first.

```rust
pub(crate) fn strip_file_media(name: &str) -> &str;
```

The prefixes are tried **longest first**, so `file://.` wins over `file://`
and `file:`:

| Input | Result |
|---|---|
| `file://./tmp/a.tjs` | `/tmp/a.tjs` |
| `file:///tmp/a.tjs` | `/tmp/a.tjs` |
| `file:a.tjs` | `a.tjs` |
| `data/a.tjs` | `data/a.tjs` (unchanged) |

The **script spelling** is kept in diagnostics even when the lookup uses the
stripped name, so an error message names what the script wrote.

Callers: the storage read seam in [`binding.md`](binding.md), the image load
and save path in [`visual/pixels.md`](visual/pixels.md) and
[`visual/bitmap.md`](visual/bitmap.md), the pre-rendered font map, and the
`Storages` family.

---

## 2. `resource.rs` — the `ResourceResolver`

`ResourceResolver` is the **per-engine** dispatch state (never a process
global) that owns:

| Field | Purpose |
|---|---|
| `coverage_cache: TextCoverageCache` | the rasterized character masks and their blurred shadows, keyed like the reference font cache |
| `decode` / `probe` / `save` | the host image codec closures |
| `prerendered_storage: HashMap<String, Rc<PrerenderedFont>>` | the loaded pre-rendered fonts, keyed by the resolved placed path |
| `prerendered: Vec<(PrerenderedKey, Rc<PrerenderedFont>)>` | the font-value → pre-rendered font mappings, in registration order |

It is reached through:

```rust
pub(crate) fn resource_state(ctx: &CompatCtx) -> RefMut<'_, ResourceResolver>;
```

The consumers are the visual `Font` native, `Layer.drawText`/`drawGlyph`,
`ImageFunction.drawText`/`drawGlyph` and `Layer.saveLayerImage`.

The **faces** are not in this state: they live in the L2 text service the
engine owns, which the host attaches to the services container. Every text
consumer — the visual `Font` native, the `TextRenderBase` plugin, the
layer-ex draw plugin and `System.addFont` — reaches that one registry
through `text_service(ctx)`, so there is no second face table in L3.

---

## 3. The host image codec

L3 never links the L1 `media` codec. The host installs three closures:

```rust
host.set_image_codec(decode, probe, save);
```

| Closure | Signature | Backs |
|---|---|---|
| `decode` | `Fn(&[u8], Option<&str>) -> Result<DecodedImage, String>` | `Bitmap.load`, `Bitmap.loadAsync`, `Layer.loadImages` |
| `probe` | `Fn(&[u8], Option<&str>) -> Result<ImageInfo, String>` | `Bitmap.loadHeader` |
| `save` | `Fn(&str, &str, u32, u32, &[u8]) -> Result<(), String>` | `Bitmap.save`, `Layer.saveLayerImage` |

The province plane is the image's own **8-bit sample** — the palette index of
a palettized image, the gray level of a grayscale one — so it is decoded
apart from the colour image, through its own closure:

```rust
host.set_index_plane_decoder(decode);
```

| Closure | Signature | Backs |
|---|---|---|
| `decode` | `Fn(&[u8], Option<&str>) -> Result<DecodedImage, String>` | `Layer.loadProvinceImage` |

The plane travels as a gray image: every pixel's red, green and blue channels
carry the plane value, and the red channel is what the province plane reads.

**A missing closure raises a typed error** — `"no image decoder is
installed"`, `"no image header prober is installed"`, `"no image encoder is
installed"`, `"no index-plane decoder is installed"` — instead of fabricating
a result.

The save path strips the media prefix before handing the name to the host
writer, exactly as the read path does on the decode side.

---

## 4. Face resolution

`resolve_face(ctx, service, face)` resolves a face string to a `FontId` in
the shared text service. The face may be a **comma-separated candidate list**
with an optional leading `@` (vertical writing).

Resolution order:

1. for each candidate, in order:
   * the registry's registered faces;
   * the attached storage — **only** when the candidate is spelled like a font
     *file* (`.ttf`, `.ttc`, `.otf`, `.otc`, `.tte`, `.dfont`), because the
     reference resolves a face only through its registered font names, and
     probing the storage for a bare family name would miss on every text
     draw;
   * the host's installed system fonts (exact family only);
2. the registry's **default face** — the face registered under the oldest
   name. A registry that holds no face yet gets the host's default font
   first, so a face the host provides is never lost to an empty registry;
3. otherwise the typed error `"no font is available for face '{face}'"`.

A candidate is tried like any other name: the reference's font map is keyed
by family name, so a family the host or the game provides under a `*` name
resolves through the same lookup.

The exact system-font lookup memoizes its misses inside the L2 text engine,
so a candidate no source knows is not scanned against the host's font
database again on the next draw. The memo holds misses only, so a family
that resolves later is not shadowed by it.

A candidate that resolves is recorded as an **alias** under the requested
name, so the next lookup finds the same face. The default-face fallback is
not recorded: a face registered later for that name (`System.addFont`,
`GdiPlus.addPrivateFont`) is found on the next lookup instead of being
shadowed by a stale fallback entry.

`Font.getList` reports the registry's registered names and
`Font.defaultFaceName` its oldest name — the name the default face is
registered under.

---

## 5. Pre-rendered fonts

A `.tft` file is a pre-rendered font: a header plus per-character bitmaps. The
mapping is a **font value** → font file association:

| Operation | Behaviour |
|---|---|
| `map_prerendered(ctx, key, storage)` | resolves the storage, validates the header now, and records the mapping. The glyph body is read on first use — the reference validates only the header at map time too, so the deferral is not script-observable. |
| `unmap_prerendered(key)` | drops the mapping and releases the parsed font once no other mapping uses it. Loaded fonts are shared by storage, so the storage entry is dropped only when the last mapping goes. |
| `prerendered_for(key)` | the font mapped to `key`, if any. |

The key is a `PrerenderedKey { face, height, bold, italic, angle }` — the
same shape as the font value it belongs to.

The storage is resolved through the L2 namespace and the **placed path** is
the cache key, so the same file reached through two mounts is loaded once.

### The header errors

| Message | Cause |
|---|---|
| `"Cannot find storage {storage}"` | the storage does not resolve |
| `"Prerendered font mapping failed : File size is zero."` | an empty file |
| `"Prerendered font mapping failed : Signature not found or invalid pre-rendered font file."` | a bad signature |
| `"Prerendered font mapping failed : Not a 16-bit UNICODE font file."` | a bad code-unit width |
| `"Prerendered font mapping failed : Invalid header version."` | an unknown version |

---

## 6. Drawing text

`draw_text_into_bitmap(resolver, ctx, bitmap, face, clip, params)` is the
shared text path. It:

1. resolves the face through the resolver;
2. uses a **pre-rendered** font mapped under the same key when one exists,
   and otherwise lays the text out with `TextEngine::layout` and rasterizes
   the coverage;
3. runs the shadow pass first when a shadow level is set (the coverage is
   blurred with the cone kernel), and reuses the unblurred character data
   when the shadow level is 255 with a zero shadow width — the reference's
   "normal shadow";
4. rasterizes each character's coverage **once** and reuses it for the shadow
   and the main pass;
5. clips to the supplied rectangle;
6. returns the union of the drawn glyph rectangles, or `None` when nothing had
   ink.

`TextDrawParams` carries the script parameters: text, pen position, colour,
opacity, antialias flag, shadow level, shadow colour, shadow width and shadow
offset. `FaceSpec` carries the resolved face and the style it is drawn with.

`glyph_text(host, value)` extracts the text a `drawGlyph` argument carries: a
string, or an object's `text`, `char` or `character` member.

---

## 7. Host seams

| Seam | Installed by | Purpose |
|---|---|---|
| `set_image_codec(decode, probe, save)` | `CompatHost` (defined in `host.rs`) | the L1 `media` image codec |
| `set_system_font_dirs(dirs)` | the composition root, on the attached L2 text service | where the host's system fonts live |

The attached L2 text service and the attached storage are the other face
sources.

---

## 8. Errors

| Message | Raised when |
|---|---|
| `"no image decoder is installed"` | a decode was requested and no decoder is installed |
| `"no image header prober is installed"` | a header probe was requested and no prober is installed |
| `"no image encoder is installed"` | a save was requested and no encoder is installed |
| `"no font is available for face '{face}'"` | no candidate resolved and the registry holds no face at all |
| `"Cannot find storage {storage}"` | the pre-rendered font's storage does not resolve |
| `"Prerendered font mapping failed : {sub}"` | the `.tft` header is invalid |
| `"text layout failed: {error}"` | the L2 text engine rejected the layout |
| `"text rasterization failed: {error}"` | the L2 rasterizer failed |
| `"shadow blur failed: {error}"` | the coverage blur failed |

---

## 9. Tests

| Test | What it asserts |
|---|---|
| `crates/compat/tests/resource.rs` | the resolver: face resolution, the registered family list, the pre-rendered map/unmap, the codec seam |
| `crates/compat/tests/font.rs` | the face metrics, `getList`, `defaultFaceName`, the pre-rendered measurements and the reference header errors |
| `crates/compat/tests/font_registry.rs` | the registered name list, the default face name, `System.addFont`'s results, the per-character measurement and the KAG-shaped text render |
| `crates/compat/tests/bitmap.rs` | the codec seam: the load/save/probe round-trips and the missing-codec errors |
| `crates/compat/tests/layer_behaviour.rs::layer_draw_text_prerendered_pixels_match_the_coverage` | a pre-rendered font's pixels match the rasterizer's coverage |
| `crates/compat/tests/storage_media_prefix.rs` | the `file://` prefix on the storage-backed members |
| `crates/compat/src/storage.rs` (unit test) | the prefix stripping table |

---

## See also

* [`visual/font.md`](visual/font.md) — the `Font` class that drives face resolution
* [`visual/pixels.md`](visual/pixels.md) — the text and image paths that use the resolver
* [`../../arch/l3.md`](../../arch/l3.md) §1 — the crate surface
