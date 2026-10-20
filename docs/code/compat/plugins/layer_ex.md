# layerExDraw (`layerExDraw.dll`)

**DLL name:** `layerExDraw.dll` (aliases: `layerExMovie.dll`, `perspective.dll`) — linked with `Plugins.link("layerExDraw.dll")`
**Rust module:** `crates/compat/src/natives/plugins/layer_ex/`
**Reference:** `reference/cpp/plugins/layerex_draw/`, `reference/cpp/plugins/layerExMovie.cpp`, `reference/cpp/plugins/layerExPerspective.cpp`, `reference/cpp/plugins/layerExBase.cpp`
**Registers:** the `GdiPlus` namespace and its classes; members on `Layer`

## Surface

`layerExBase` is an internal native-instance base and is not a TJS class
(`reference/cpp/plugins/layerExPerspective.cpp:255-296`); `LayerExMovie` and
`LayerExPerspective` are likewise not classes, only `Layer` member sets.

### `GdiPlus` namespace

| member | kind | behaviour |
|---|---|---|
| `GdiPlus.addPrivateFont(...)` | static method | registers a private font file |
| `GdiPlus.getFontList()` | static method | the list of available fonts |

The namespace also carries the reference's enum constants, in the blend2d
backend's values (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:65-262`):

* **status** — `Ok` `0` … `ProfileNotFound` `21`;
* **font style** — `FontStyleRegular` `0`, `FontStyleBold` `1`,
  `FontStyleItalic` `2`, `FontStyleBoldItalic` `3`, `FontStyleUnderline` `4`,
  `FontStyleStrikeout` `8`;
* **brush type** — `BrushTypeSolidColor` `0` … `BrushTypeLinearGradient` `4`;
* **dash cap** — `DashCapFlat` `0`, `DashCapRound` `2`,
  `DashCapTriangle` `3`;
* **dash style** — `DashStyleSolid` `0` … `DashStyleDashDotDot` `4`;
* **hatch style** — `HatchStyleHorizontal` … `HatchStyleMax`;
* **gradient mode** — `LinearGradientModeHorizontal` …
  `LinearGradientModeBackwardDiagonal`;
* **line cap** — `LineCapFlat` … `LineCapArrowAnchor`;
* **line join** — `LineJoinMiter`, `LineJoinBevel`, `LineJoinRound`,
  `LineJoinMiterClipped`;
* **pen alignment** — `PenAlignmentCenter`, `PenAlignmentInset`;
* **wrap mode** — `WrapModeTile` … `WrapModeClamp`;
* **matrix order** — `MatrixOrderPrepend`, `MatrixOrderAppend`;
* **image type** — `ImageTypeUnknown`, `ImageTypeBitmap`, `ImageTypeMetafile`;
* **rotate flip** — `RotateNoneFlipNone` … `RotateNoneFlipXY`;
* **smoothing mode** — `SmoothingModeInvalid` … `SmoothingModeAntiAlias`;
* **text rendering hint** — `TextRenderingHintSystemDefault` …
  `TextRenderingHintClearTypeGridFit`.

### `GdiPlus.PointF`

| member | kind | behaviour |
|---|---|---|
| `PointF(x, y)` | constructor | the reference arity is 2, so a shorter call raises `TJS_E_BADPARAMCOUNT` |
| `x` | property (read/write) | the x coordinate |
| `y` | property (read/write) | the y coordinate |
| `Equals(other)` | method | point equality |
| `Clone()` | method | a copy of the point |

### `GdiPlus.RectF`

| member | kind | behaviour |
|---|---|---|
| `RectF(x, y, width, height)` | constructor | the reference arity is 4 |
| `x`, `y`, `width`, `height` | properties (read/write) | the rectangle's fields |
| `left`, `top`, `right`, `bottom` | properties (read/write) | the derived edges |
| `location` | property (read/write) | the top-left point |
| `bounds` | property (read/write) | the rectangle itself |
| `Equals(other)` | method | rectangle equality |
| `Clone()` | method | a copy of the rectangle |
| `Inflate(x, y)` | method | grows the rectangle |
| `InflatePoint(point)` | method | grows the rectangle by a point's offsets |
| `IntersectsWith(other)` | method | whether the rectangles overlap |
| `IsEmptyArea()` | method | whether the rectangle is empty |
| `Offset(x, y)` | method | moves the rectangle |
| `Union(other)` | method | the union of two rectangles |
| `RectF.Union(a, b)` | static method | the union of two rectangles, on the class object |

The reference `Union` is a *static* member (`static bool Union(RectF &ret,
const RectF &a, const RectF &b)`, blend2d `LayerExDraw.hpp:389`): it is
declared with `TJS_STATICMEMBER`, lives on the class object and stays
invisible to instances
(`reference/cpp/core/plugin/ncbind.hpp:1218-1233`).

### `GdiPlus.Path`

| member | kind | behaviour |
|---|---|---|
| `Path()` | constructor | an empty path |
| `startFigure()` | method | starts a new figure |
| `closeFigure()` | method | closes the current figure |
| `drawArc(x, y, width, height, startAngle, sweepAngle)` | method | adds an arc |
| `drawPie(x, y, width, height, startAngle, sweepAngle)` | method | adds a pie |
| `drawBezier(x1, y1, x2, y2, x3, y3, x4, y4)` | method | adds a bezier |
| `drawBeziers(points)` | method | adds a bezier spline |
| `drawClosedCurve(points)` | method | adds a closed curve |
| `drawClosedCurve2(points, tension)` | method | adds a closed curve with a tension |
| `drawCurve(points)` | method | adds a curve |
| `drawCurve2(points, tension)` | method | adds a curve with a tension |
| `drawCurve3(points, start, count, tension)` | method | adds part of a curve |
| `drawEllipse(x, y, width, height)` | method | adds an ellipse |
| `drawLine(x1, y1, x2, y2)` | method | adds a line |
| `drawLines(points)` | method | adds a polyline |
| `drawPolygon(points)` | method | adds a polygon |
| `drawRectangle(x, y, width, height)` | method | adds a rectangle |
| `drawRectangles(rects)` | method | adds several rectangles |

### `GdiPlus.Appearance`

| member | kind | behaviour |
|---|---|---|
| `Appearance()` | constructor | an empty appearance |
| `clear()` | method | removes every brush and pen |
| `addBrush(...)` | method | adds a brush |
| `addPen(...)` | method | adds a pen |

### `GdiPlus.Font`

The class is registered as `FontInfo` internally and published as
`GdiPlus.Font`.

| member | kind | behaviour |
|---|---|---|
| `FontInfo()` | constructor | a font description |
| `familyName` | property (read/write) | the font family |
| `emSize` | property (read/write) | the em size |
| `style` | property (read/write) | the style flags |
| `forceSelfPathDraw` | property (read/write) | whether the font draws its own path |
| `ascent` | property (read-only) | the ascent |
| `descent` | property (read-only) | the descent |
| `ascentLeading` | property (read-only) | the ascent leading |
| `descentLeading` | property (read-only) | the descent leading |
| `lineSpacing` | property (read-only) | the line spacing |

### `GdiPlus.Image`

| member | kind | behaviour |
|---|---|---|
| `Image()` | constructor | an empty image |
| `load(...)` | method | loads the image |
| `Clone()` | method | a copy of the image |
| `GetBounds()` | method | the image's bounds |
| `GetFlags()` | method | the image's flags |
| `GetHeight()` | method | the image's height |
| `GetHorizontalResolution()` | method | the horizontal resolution |
| `GetLastStatus()` | method | the last status code |
| `GetPixelFormat()` | method | the pixel format |
| `GetType()` | method | the image type |
| `GetVerticalResolution()` | method | the vertical resolution |
| `GetWidth()` | method | the image's width |
| `RotateFlip(...)` | method | rotates or flips the image |

### `GdiPlus.Matrix`

| member | kind | behaviour |
|---|---|---|
| `Matrix()` | constructor | the identity matrix |
| `OffsetX` | property (read-only) | the x translation |
| `OffsetY` | property (read-only) | the y translation |
| `Equals(other)` | method | matrix equality |
| `SetElements(m11, m12, m21, m22, dx, dy)` | method | sets the elements |
| `GetLastStatus()` | method | the last status code |
| `Invert()` | method | inverts the matrix |
| `IsIdentity()` | method | whether the matrix is the identity |
| `IsInvertible()` | method | whether the matrix can be inverted |
| `Multiply(other)` | method | multiplies by another matrix |
| `Reset()` | method | resets to the identity |
| `Rotate(angle)` | method | rotates |
| `RotateAt(angle, x, y)` | method | rotates around a point |
| `Scale(scaleX, scaleY)` | method | scales |
| `Shear(shearX, shearY)` | method | shears |
| `Translate(dx, dy)` | method | translates |

### `Layer` members

#### Transforms

| member | kind | behaviour |
|---|---|---|
| `setViewTransform(...)` | method | sets the view transform |
| `resetViewTransform()` | method | resets the view transform |
| `rotateViewTransform(angle, x, y)` | method | rotates the view transform |
| `scaleViewTransform(scaleX, scaleY, x, y)` | method | scales the view transform |
| `translateViewTransform(dx, dy)` | method | translates the view transform |
| `setTransform(...)` | method | sets the draw transform |
| `resetTransform()` | method | resets the draw transform |
| `rotateTransform(angle, x, y)` | method | rotates the draw transform |
| `scaleTransform(scaleX, scaleY, x, y)` | method | scales the draw transform |
| `translateTransform(dx, dy)` | method | translates the draw transform |

#### Drawing

| member | kind | behaviour |
|---|---|---|
| `clear()` | method | clears the layer |
| `drawPath(path, appearance)` | method | draws a path with an appearance |
| `drawArc(...)`, `drawPie(...)`, `drawBezier(...)`, `drawBeziers(...)`, `drawClosedCurve(...)`, `drawClosedCurve2(...)`, `drawCurve(...)`, `drawCurve2(...)`, `drawCurve3(...)`, `drawEllipse(...)`, `drawLine(...)`, `drawLines(...)`, `drawPolygon(...)`, `drawRectangle(...)`, `drawRectangles(...)` | methods | the shape primitives, mirroring the `GdiPlus.Path` members |
| `drawPathString(path, appearance, string, ...)` | method | draws text along a path |
| `drawString(string, appearance, ...)` | method | draws text |
| `measureString(string, appearance, ...)` | method | measures a string |
| `measureStringInternal(string, appearance, ...)` | method | the internal measurement the reference uses |
| `drawImage(image, x, y)` | method | draws an image |
| `drawImageRect(image, x, y, width, height)` | method | draws an image into a rectangle |
| `drawImageStretch(image, x, y, width, height)` | method | stretches an image into a rectangle |
| `drawImageAffine(image, matrix, ...)` | method | draws an image through an affine matrix |

#### Recording

| member | kind | behaviour |
|---|---|---|
| `getRecordImage()` | method | the recorded image |
| `redrawRecord()` | method | replays the recording |
| `saveRecord(...)` | method | saves the recording |
| `loadRecord(...)` | method | loads a recording |
| `record` | property | the recording state |

#### Properties

| member | kind | behaviour |
|---|---|---|
| `updateWhenDraw` | property (read/write) | whether a draw updates the layer |
| `smoothingMode` | property (read/write) | the smoothing mode |
| `textRenderingHint` | property (read/write) | the text rendering hint |

#### Perspective and movie

| member | kind | behaviour |
|---|---|---|
| `perspectiveCopy(...)` | method | the perspective copy (`reference/cpp/plugins/layerExPerspective.cpp`) |
| `openMovie(...)` | method | opens a movie for the layer |
| `startMovie()` | method | starts the movie |
| `stopMovie()` | method | stops the movie |
| `isPlayingMovie()` | method | whether the movie is playing |

The movie members come with the `onStartMovie` / `onStopMovie` /
`onUpdateMovie` callbacks the reference dispatches on the layer.

## Behaviour

### Composition

The port composes the engine's `layer_ops` / `raster` primitives for the
pixel work and the engine's `Scene` / `LayerState` for layer state. The
extension owns only the dispatch state that has no home in the engine — the
movie playback slots and the per-layer draw transform and record state — kept
in the per-engine dispatch state, not in a process global.

### Argument conversion

The reference's `PointFConvertor` and `RectFConvertor`
(`reference/cpp/plugins/layerex_draw/windows/main.cpp:75-104,145-178`;
blend2d `LayerExDraw.cpp:2922-2943,2965-3000`) treat a non-object argument
— including TJS `null` — as the default value and never fail. The port
matches that.

The `REAL` parameters use the reference's `AsReal` conversion
(`reference/cpp/core/tjs2/tjsVariant.h:947-966`): `void` and strings coerce,
while an object or an octet raises a conversion error instead of silently
coercing to `0`.

### The nested classes

The reference publishes the nested classes inside the `GdiPlus` namespace
object. The compat ABI always publishes a global for a registration, so the
port registers each nested class as an ordinary instance class and then
aliases it into `GdiPlus`, deleting the bare registration global.

### Argument-type checks

A `GdiPlus` value argument is resolved through the VM's native-instance
lookup — the `NativeInstanceSupport(TJS_NIS_GETINSTANCE, classid)` mechanism
behind the reference plugin's argument adaptor. The lookup keys on the
argument object itself, not on its `ObjThis` or on any script-visible
member; an object of another class is a typed error
(`<what>: the argument is not of the <Class> type`).

## State and lifetime

* Every value type (`PointF`, `RectF`, `Path`, `Appearance`, `Font`,
  `Image`, `Matrix`) keeps its payload in the native instance.
* The per-layer draw transform and record state live in the per-engine
  dispatch state, keyed by the layer's scene id.
* The movie playback slots live in the same per-engine dispatch state.
* The layer's invalidate and destroy hooks call `forget_layer`, which drops
  every piece of per-layer state the plugin keeps for that layer — so no
  transform, recording or movie playback state outlives the layer.

## Errors

| condition | outcome |
|---|---|
| a value argument that is not of the expected `GdiPlus` class | typed error: `<what>: the argument is not of the <Class> type` |
| a `REAL` argument that is an object or an octet | typed error: the argument is not a number |
| a constructor called with too few arguments | typed error: `<Class> requires N arguments` |
| an element argument that is not a rectangle | typed error: `<what>: element <n> is not a rectangle` |
| a movie source that cannot be opened | the media error |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/layer_ex.rs` | the `GdiPlus` namespace and its nested classes, the constants, the value types' arithmetic, and the `Layer` members |
| `crates/compat/tests/layer_ex_behaviour.rs` | the drawing behaviour, the transforms, the recording, the perspective copy and the movie members |
| `crates/compat/tests/layer_geometry.rs` | the geometry the value types build on |

## Known gaps

* **The rendering backend differs.** The reference's Linux and Android build
  uses blend2d (`reference/cpp/plugins/layerex_draw/CMakeLists.txt`); the port
  draws through the engine's raster primitives. The script-visible values
  match the reference; the pixel output of a rasterization-sensitive effect
  is the engine's, not blend2d's.
* The reference's nested classes are published only inside `GdiPlus`; the
  port registers them as globals first and then aliases them, so the bare
  names exist briefly during registration. The final script-visible shape is
  the nested one, and the extra top-level names are locked into the surface
  baseline.
* The reference's `CBinaryAccessor`-style cursor arithmetic (used by the
  `xp3filter` port) is not exposed here either; the `GdiPlus` value types do
  not need it.
* The `measureStringInternal` member exists because the reference exposes an
  internal measurement path; the port reproduces the member and its result,
  not the reference's private call graph.
