# GdiPlus / layerExDraw plugin

The `layerExDraw` plugin publishes the global `GdiPlus` namespace and extends
`Layer` with the GDI+ draw methods. Reference:
`reference/cpp/plugins/layerex_draw/` (the `windows/` and `blend2d/` builds;
the blend2d build is the one this port follows).

Implementation: `crates/compat/src/natives/plugins/layer_ex/` — `gdiplus.rs`
(the namespace and its constants), `appearance.rs`, `path.rs`, `draw.rs`,
`geom.rs`, `image.rs`, `font.rs`, `matrix.rs`, `movie.rs`, `perspective.rs`,
`record.rs`.

---

## The `GdiPlus` namespace

`gdiplus.rs` registers one global class object carrying the `ENUM` constants
the reference publishes (about 150: error codes, `BrushType*`, `HatchStyle*`,
`DashStyle*`, `FontStyle*`, …) plus the nested `NCB_SUBCLASS` classes
`PointF`, `RectF`, `Image`, `Matrix`, `Font` (`FontInfo`) and `Appearance`,
and — blend2d only — `Path`. `NCB_SUBCLASS` attaches a class as a member of
the enclosing class object without publishing it globally
(`reference/cpp/core/plugin/ncbind.hpp:1792-1799,1963-1990`); the port
registers each nested class through the always-global registration ABI,
aliases the class object into `GdiPlus`, then deletes the registration global.

The constant values match the blend2d enum definitions
(`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:65-262`), the
backend the Linux/Android build uses
(`reference/cpp/plugins/layerex_draw/CMakeLists.txt`).

---

## `Appearance` — an ordered list of fills and strokes

`Appearance` is **not** a brush. The reference
`Appearance::drawInfos` is a `vector<DrawInfo>`; `addBrush` appends a fill,
`addPen` appends a stroke, and the draw methods iterate the list in insertion
order (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:422,767,778,1150`).
The port keeps that model in `AppearanceData { infos: Vec<DrawInfo> }`, where
each `DrawInfo` is a `Paint`, a pen/brush flag, a width and an offset.

Supported paints (`appearance.rs`):

* `Solid` — a straight-alpha RGBA colour.
* `Hatch` — a style plus `fore`/`back` colours; rasterized through the L2
  `engine::raster::hatch_foreground`.
* `LinearGradient` — two colours and two points.

Texture and path-gradient brushes **raise a typed error** instead of silently
drawing nothing. A non-object colour argument goes through the reference
`AsInteger` conversion
(`reference/cpp/core/tjs2/tjsVariant.h:862-877`): `void` is 0, strings parse,
and objects/octets raise rather than being zeroed.

`addBrush`/`addPen`/`clear` are the only `Appearance` methods; `addPen`'s
second argument is a width when it is an integer and a dictionary with a
`width` key otherwise. Omitted trailing arguments arrive as `void` and convert
to 0 (ncbind default-argument passing,
`reference/cpp/core/plugin/ncbind.hpp:963`).

---

## `Path`

`Path` wraps a set of flattened polylines (`PathData { figures: Vec<Figure> }`).
The reference keeps one "current figure" flag: primitives after
`startFigure()` connect to the current figure, while closed primitives
(`drawRectangle`/`drawEllipse`/`drawRectangles`) always add their own figure
and reset the flag (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1133-1451`).
`closeFigure` closes the current one. The port models the same thing as a list
of figures where the open last figure is the current one. Each figure is
flattened as it is added with the L2 `engine::raster` primitives
(`flatten_arc`, `flatten_cubic`), so `Layer.drawPath` can fill and stroke it
with the existing CPU rasterizer.

---

## Native-instance payloads

The plugin's value types (`Appearance`, `Path`, `Matrix`, `Image`, `Font`,
…) are real native instances: the VM allocates a payload, the class's `create`
boxed it, and the class's `destroy` drops it. A plugin method that takes one of
these as an argument recovers the payload through the VM's native-instance
lookup (`Tjs2Engine::native_instance_payload`,
`crates/compat/src/natives/plugins/layer_ex/mod.rs` `read_instance`) — the
`NativeInstanceSupport(TJS_NIS_GETINSTANCE, classid)` mechanism behind the
reference plugin argument adaptor. The lookup keys on the argument object
itself, not on `ObjThis` and not on any script-visible member; an object of
another class is a typed error. No member stores a pointer as a hidden script
property.

---

## `Layer.draw*` and the rest of the surface

`draw.rs` registers the `Layer` instance methods: `drawPath`, `drawArc`,
`drawPie`, `drawBezier(s)`, `drawCurve*`, `drawEllipse`, `drawLine(s)`,
`drawPolygon`, `drawRectangle(s)`, `drawPathString`, `drawString`, `drawImage`,
`drawImageRect`, `drawImageStretch`, `drawImageAffine`. `geom.rs` holds the
point/rectangle/colour argument readers, `image.rs` the `GdiPlus.Image` type,
`font.rs` the `GdiPlus.Font` type and the static
`addPrivateFont`/`getFontList`, `matrix.rs` the transform type, `movie.rs`
the movie layer extension, `perspective.rs` `Layer.perspectiveCopy`, and
`record.rs` the draw recording.

The draw methods rasterize into the layer's `BitmapState` through the L2
`engine::raster` CPU path, exactly like `Layer.drawText`; no new renderer
path is involved.

---

## `Window.menu` and `MenuItem`

`Window.menu` and the `MenuItem` class are core compat natives, not part of
`layerExDraw`:

* `crates/compat/src/natives/menu_item.rs` — the logical menu tree. The tree
  is explicit engine state (`MenuState`, id → node); a `MenuItem` script object
  owns only its node id. `HMENU` reports `0` and `popup(flags, x, y)` reports
  the reference success value `1` (`MenuItemImpl.cpp:193-196,309-315`).
* `crates/compat/src/natives/visual/window.rs` — `Window.menu`, the per-window
  root `MenuItem`, built on first read and cached
  (`reference/cpp/core/visual/impl/MenuItemImpl.cpp:65-92`).
* `crates/compat/src/natives/plugins/window_ex/menu_item.rs` — the
  `MenuItemEx` members added by `windowEx.dll`: `rightJustify`, `bmpItem`/
  `bmpChecked`/`bmpUnchecked`, `popupEx` and the `bi*` constants.

---

## Verification

Host tests drive the real VM surface: `crates/compat/tests/layer_ex.rs`,
`layer_ex_behaviour.rs`, `menu_item.rs`, `menu_dll_surface.rs` and the parity
checks (`uv run --project scripts krkr-gate native-parity`,
`krkr-gate plugin-parity`) compare the registered members against the
reference tables. A device run is still owed
for the Android pixel path.
