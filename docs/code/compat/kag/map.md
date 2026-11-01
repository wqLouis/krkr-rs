# The KAG clickable province map

Source: the KAG runtime the games ship, and the port's engine half
(`crates/compat/src/natives/visual/layer/pixels.rs`,
`crates/compat/src/natives/visual/layer/mod.rs`,
`crates/compat/src/natives/kag/`) · layer **L3**

A map is a **province image** and an **action table**. The province image's
eight-bit plane says which region a point belongs to; the action table says
what that region does. A click is the whole chain: the point's region, the
region's action, the action's effect.

A title screen uses one: it arms the map, waits for the player to click a
region, and disarms the map again. The map's files are the province image
and the action table — the title's own are `title_bg_main_p` and
`title_bg_main.ma`.

---

## 1. Who owns what

| part | owner | where |
|---|---|---|
| the tags `[mapimage]`, `[mapaction]`, `[mapdisable]` | the KAG runtime | `system/mainwindow.tjs` |
| the action table (the `.ma` file) and the map layer | the KAG runtime | `system/graphiclayer.tjs` |
| the province plane, the value a point answers, the click target | the port | `natives/visual/layer/` |

The **port does not own the map layer**. The reference's `KAGParser` hands
every tag to the runtime that drives it — the tag dictionary carries the
tag's name and its attributes
(`reference/cpp/core/base/KAGParser.cpp:1384-1400`) — and the KAG runtime's
own handlers act on the map tags. A port that executed the map tags itself
would run the map twice and answer a different question than the game's
runtime.

What the port owns is the engine half the runtime calls into: the province
plane, the value a point answers, and the click that reaches the map layer.

---

## 2. The chain

### The province image

`[mapimage layer = base page = fore storage = "title_bg_main_p"]` reaches the
layer the tag names, which loads the province image into the layer's
eight-bit plane (`Layer.loadProvinceImage`,
`reference/cpp/core/visual/LayerIntf.cpp:2893-2918`).

The plane is the image's **own eight-bit sample** — the palette index of a
palettized image, not the colour the palette maps that index to
(`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1709`). A region is
therefore addressed by its index, and a region whose index the action table
does not know is not a region at all.

The plane is sized to the layer's main image: a smaller image is tiled up to
that size, a larger one is a size mismatch.

### The action table

`[mapaction layer = base page = fore storage = "title_bg_main.ma"]` reaches
the same layer, which reads the table and compiles it. The format is the
KAG's: one action per line, `<region>: <body>`, the body a TJS fragment
that fills the action.

```text
0: autodisable = false;
16: target = "*game_start"; exp = "mapAction(16)"; onenter = "mapEnter(16)";
```

A line that is empty or starts with `;` is a comment. The region's action
holds the members the KAG's province context has: `target`, `storage`,
`onenter`, `onleave`, `hint`, `exp`, `cursor`, `countpage` and
`autodisable`.

### The click

The click reaches the map layer through the hit test: the layer under the
pointer is the one that receives it
(`reference/cpp/core/visual/LayerIntf.cpp:3286-3302`). The map layer turns
it into the region's action:

1. the point's region is the plane's value at the point, in the layer's own
   coordinates less the image offset (`Layer.getProvincePixel`,
   `reference/cpp/core/visual/LayerIntf.cpp:2973-2982`);
2. the region's action is the table's entry for that value
   (`queryProvinceAction`);
3. the action's `exp` is evaluated, and the action's `target` is the
   scenario the click jumps to (`processProvince`).

The region outside every region is the value 0, and the table holds no entry
for it: a click there runs nothing.

The pointer's move does the same for the hover: entering a region runs the
region's `onenter`, leaving it runs the region's `onleave`.

### Disarming

`[mapdisable layer = base page = fore]` clears the table, so a click runs no
action any more. The plane itself stays; only the actions are gone.

---

## 3. The engine half in the port

| member | what it does |
|---|---|
| `Layer.loadProvinceImage(name)` | loads the eight-bit plane, sized to the main image ([`visual/pixels.md`](../visual/pixels.md) §`loadProvinceImage`) |
| `Layer.getProvincePixel(x, y)` | the plane's value at a point, 0 outside it |
| `Layer.setProvincePixel(x, y, n)` | writes the plane's value at a point |
| `Layer.provinceImageBuffer`, `provinceImageBufferForWrite`, `provinceImageBufferPitch` | the plane's memory |
| `Layer.independProvinceImage([copy])` | allocates the plane |
| `Layer.hitType` | `htMask` (the alpha against `hitThreshold`) or `htProvince` (a non-zero plane byte) — `reference/cpp/core/visual/LayerIntf.h:113` |

The plane is read through the L1 index-plane decoder the composition root
installs ([`../host.md`](../host.md) §`set_index_plane_decoder`,
[`../resource.md`](../resource.md) §the province plane).

---

## 4. Tests

| test | what it pins |
|---|---|
| `crates/compat/tests/kag_map.rs` | the plane answers the region's id; the action table is keyed by it; the click on a region runs the region's action; a click outside every region runs none; the pointer entering a region runs the enter action; the map tags reach the runtime with their attributes |
| `crates/compat/tests/title_map.rs` | the title's map tags name the title's own files; the tags arm the map, a click runs the region's action and `mapdisable` disarms the map |
| `crates/compat/tests/province.rs` | the plane's own semantics: the palette index, not the colour |

The map's script half — the map layer, the action table's format and the
tag handlers — is the KAG runtime's, and the tests carry it as the runtime
writes it (`crates/compat/tests/common/map.rs`).

---

## 5. Known gaps

**The `<image>_p` convention is not implemented.** The reference's
`LoadImages` computes a province name from the image's name — the name
without its extension, plus `_p`, plus the extension of the file it found —
and loads that file as the layer's province plane when it exists
(`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1662-1680`,
`reference/cpp/core/visual/LayerIntf.cpp:2847-2875`). The port's
`loadImages` does not: only an explicit `loadProvinceImage` loads a plane,
so a game that relies on the convention gets no province plane. The
convention is the KAG map's own naming (`title_bg_main` and
`title_bg_main_p`), so a title that arms its map with the convention alone
has no clickable region.

---

## 6. Related documents

| topic | document |
|---|---|
| the KAG parser and the tag state machine | [`../kag.md`](../kag.md) |
| the layer's pixel members, the province plane | [`../visual/pixels.md`](../visual/pixels.md) |
| the layer's hit test | [`../visual/layer.md`](../visual/layer.md) |
| the event adapter and the click target | [`../event.md`](../event.md) |
| the image codec and the index plane | [`../resource.md`](../resource.md) |
