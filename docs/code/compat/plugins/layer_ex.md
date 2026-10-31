# layerExDraw (`layerExDraw.dll`)

**DLL name:** `layerExDraw.dll` (aliases: `layerExMovie.dll`, `perspective.dll`) — linked with `Plugins.link("layerExDraw.dll")`
**Rust module:** `crates/compat/src/natives/plugins/layer_ex/`
**Reference:** `reference/cpp/plugins/layerex_draw/`, `layerExMovie.cpp`, `layerExPerspective.cpp`
**Registers:** the `GdiPlus` namespace and its classes; members on `Layer`

The three reference DLLs are one port. `layerExDraw.dll` publishes the
`GdiPlus` namespace and attaches the GDI+ draw members to `Layer`;
`layerExMovie.dll` attaches the movie members to `Layer`; `perspective.dll`
attaches `Layer.perspectiveCopy`. The port installs all three from one
registration call, so a script that links any of the three names gets the
whole surface.

The port follows the **blend2d** build of the reference — the one the
non-Windows build compiles
(`reference/cpp/plugins/layerex_draw/CMakeLists.txt:16-24`).

| Reference file | What it contributes |
|---|---|
| `reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp` | the `GdiPlus` namespace, the value types, the `Layer` draw/transform/record members |
| `reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp` | the C++ class definitions the value types mirror |
| `reference/cpp/plugins/layerex_draw/windows/main.cpp` | the GDI+ (Windows) registration tables, used for the argument adaptors and the member lists |
| `reference/cpp/plugins/layerExMovie.cpp` | the movie members and the `onStartMovie` / `onStopMovie` / `onUpdateMovie` callbacks |
| `reference/cpp/plugins/layerExPerspective.cpp` | `Layer.perspectiveCopy` |

`layerExBase` is an internal native-instance base, not a TJS class: the
reference only reaches for it in the compiled-out software branch
(`reference/cpp/plugins/layerExPerspective.cpp:255-296`), and the live branch
adds a method to the `Layer` class object. `LayerExMovie` and
`LayerExPerspective` are likewise never published as classes. The port
installs none of the three.

## Surface

The counts below are the live ones the surface lock freezes
(`docs/surface/runtime_surface.json`): the distinct member names the VM can
actually reach after `CompatHost::install`.

### `GdiPlus` namespace

`GdiPlus` is a class object (`NCB_REGISTER_CLASS(GdiPlus)`,
`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:3463`). It
carries **155 read-only integer constants**, **two static methods** and the
**seven nested class objects** — 164 members in all.

| member | kind | behaviour |
|---|---|---|
| `addPrivateFont(fontFileName)` | static method | loads a font file through the attached storage, registers every face it holds and appends the family names to the private list; a missing placed path raises `cannot open:<name>`, a file the font reader rejects raises `blend2d cannot load:<name>` (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:93-126`) |
| `getFontList(privateOnly)` | static method | an array of family names: the registered faces when `privateOnly` is false, then the private faces; empty names are skipped (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:132-157`) |
| `PointF`, `RectF`, `Image`, `Matrix`, `Path`, `Appearance`, `Font` | nested class | the value types below; `Font` is the C++ class `FontInfo` (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:3643-3651`) |
| 155 constants | read-only property | the `ENUM(...)` list, one integer per name; the groups are below |

The constants are grouped as the reference groups them
(`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:65-262`):

| group | count | names |
|---|---:|---|
| status | 22 | `Ok`, `GenericError`, `InvalidParameter`, `OutOfMemory`, `ObjectBusy`, `InsufficientBuffer`, `NotImplemented`, `Win32Error`, `WrongState`, `Aborted`, `FileNotFound`, `ValueOverflow`, `AccessDenied`, `UnknownImageFormat`, `FontFamilyNotFound`, `FontStyleNotFound`, `NotTrueTypeFont`, `UnsupportedGdiplusVersion`, `GdiplusNotInitialized`, `PropertyNotFound`, `PropertyNotSupported`, `ProfileNotFound` |
| `FontStyle*` | 6 | `FontStyleRegular`, `FontStyleBold`, `FontStyleItalic`, `FontStyleBoldItalic`, `FontStyleUnderline`, `FontStyleStrikeout` |
| `BrushType*` | 5 | `BrushTypeSolidColor`, `BrushTypeHatchFill`, `BrushTypeTextureFill`, `BrushTypePathGradient`, `BrushTypeLinearGradient` |
| `DashCap*` | 3 | `DashCapFlat`, `DashCapRound`, `DashCapTriangle` |
| `DashStyle*` | 5 | `DashStyleSolid`, `DashStyleDash`, `DashStyleDot`, `DashStyleDashDot`, `DashStyleDashDotDot` |
| `HatchStyle*` | 57 | `HatchStyleHorizontal`, `HatchStyleVertical`, `HatchStyleForwardDiagonal`, `HatchStyleBackwardDiagonal`, `HatchStyleCross`, `HatchStyleDiagonalCross`, `HatchStyle05Percent`, `HatchStyle10Percent`, `HatchStyle20Percent`, `HatchStyle25Percent`, `HatchStyle30Percent`, `HatchStyle40Percent`, `HatchStyle50Percent`, `HatchStyle60Percent`, `HatchStyle70Percent`, `HatchStyle75Percent`, `HatchStyle80Percent`, `HatchStyle90Percent`, `HatchStyleLightDownwardDiagonal`, `HatchStyleLightUpwardDiagonal`, `HatchStyleDarkDownwardDiagonal`, `HatchStyleDarkUpwardDiagonal`, `HatchStyleWideDownwardDiagonal`, `HatchStyleWideUpwardDiagonal`, `HatchStyleLightVertical`, `HatchStyleLightHorizontal`, `HatchStyleNarrowVertical`, `HatchStyleNarrowHorizontal`, `HatchStyleDarkVertical`, `HatchStyleDarkHorizontal`, `HatchStyleDashedDownwardDiagonal`, `HatchStyleDashedUpwardDiagonal`, `HatchStyleDashedHorizontal`, `HatchStyleDashedVertical`, `HatchStyleSmallConfetti`, `HatchStyleLargeConfetti`, `HatchStyleZigZag`, `HatchStyleWave`, `HatchStyleDiagonalBrick`, `HatchStyleHorizontalBrick`, `HatchStyleWeave`, `HatchStylePlaid`, `HatchStyleDivot`, `HatchStyleDottedGrid`, `HatchStyleDottedDiamond`, `HatchStyleShingle`, `HatchStyleTrellis`, `HatchStyleSphere`, `HatchStyleSmallGrid`, `HatchStyleSmallCheckerBoard`, `HatchStyleLargeCheckerBoard`, `HatchStyleOutlinedDiamond`, `HatchStyleSolidDiamond`, `HatchStyleTotal`, `HatchStyleLargeGrid`, `HatchStyleMin`, `HatchStyleMax` |
| `LinearGradientMode*` | 4 | `LinearGradientModeHorizontal`, `LinearGradientModeVertical`, `LinearGradientModeForwardDiagonal`, `LinearGradientModeBackwardDiagonal` |
| `LineCap*` | 9 | `LineCapFlat`, `LineCapSquare`, `LineCapRound`, `LineCapTriangle`, `LineCapNoAnchor`, `LineCapSquareAnchor`, `LineCapRoundAnchor`, `LineCapDiamondAnchor`, `LineCapArrowAnchor` |
| `LineJoin*` | 4 | `LineJoinMiter`, `LineJoinBevel`, `LineJoinRound`, `LineJoinMiterClipped` |
| `PenAlignment*` | 2 | `PenAlignmentCenter`, `PenAlignmentInset` |
| `WrapMode*` | 5 | `WrapModeTile`, `WrapModeTileFlipX`, `WrapModeTileFlipY`, `WrapModeTileFlipXY`, `WrapModeClamp` |
| `MatrixOrder*` | 2 | `MatrixOrderPrepend`, `MatrixOrderAppend` |
| `ImageType*` | 3 | `ImageTypeUnknown`, `ImageTypeBitmap`, `ImageTypeMetafile` |
| `Rotate*Flip*` | 16 | `RotateNoneFlipNone`, `Rotate90FlipNone`, `Rotate180FlipNone`, `Rotate270FlipNone`, `RotateNoneFlipX`, `Rotate90FlipX`, `Rotate180FlipX`, `Rotate270FlipX`, `RotateNoneFlipY`, `Rotate90FlipY`, `Rotate180FlipY`, `Rotate270FlipY`, `RotateNoneFlipXY`, `Rotate90FlipXY`, `Rotate180FlipXY`, `Rotate270FlipXY` |
| `SmoothingMode*` | 6 | `SmoothingModeInvalid` (-1), `SmoothingModeDefault`, `SmoothingModeHighSpeed`, `SmoothingModeHighQuality`, `SmoothingModeNone`, `SmoothingModeAntiAlias` |
| `TextRenderingHint*` | 6 | `TextRenderingHintSystemDefault`, `TextRenderingHintSingleBitPerPixelGridFit`, `TextRenderingHintSingleBitPerPixel`, `TextRenderingHintAntiAliasGridFit`, `TextRenderingHintAntiAlias`, `TextRenderingHintClearTypeGridFit` |

### `GdiPlus.<Class>` members

Every nested class is a native instance class: the VM allocates a payload, the
class's `create` boxes it and the class's `destroy` drops it. A member that
takes one of these as an argument recovers the payload through the VM's
native-instance lookup, the mechanism behind the reference plugin argument
adaptor — the lookup keys on the argument object itself, so an object of
another class is a typed error, not a coerced value.

Every instance also carries the VM's `finalize` lifecycle member; the counts
below exclude it. A count above the class object therefore holds the
class-level members (the constructor, and a static) plus the instance
members, with a name that appears on both counted once.

#### `GdiPlus.PointF` (5 members)

| member | kind | behaviour |
|---|---|---|
| `PointF(x, y)` | constructor | two arguments required; both convert with the reference `AsReal` rule (`reference/cpp/plugins/layerex_draw/windows/main.cpp:133-136`) |
| `Equals(point)` | method | exact component comparison; a non-object argument converts to the default `PointF()` (0, 0) and never fails (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:344`) |
| `x` | property | real, read/write |
| `y` | property | real, read/write |

#### `GdiPlus.RectF` (20 members)

| member | kind | behaviour |
|---|---|---|
| `RectF(x, y, width, height)` | constructor | four arguments required (`reference/cpp/plugins/layerex_draw/windows/main.cpp:181-184`) |
| `Clone()` | method | a fresh `RectF` with the same components |
| `Equals(rect)` | method | exact component comparison; a non-object argument converts to the default `RectF()` (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:370-373`) |
| `Inflate(dw, dh)` | method | grows the rectangle by `(dw, dh)` on every side (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:373-378`) |
| `InflatePoint(point)` | method | `Inflate(point.x, point.y)`; a non-object inflates by 0, 0 (`reference/cpp/plugins/layerex_draw/windows/main.cpp:201-202`) |
| `IntersectsWith(rect)` | method | whether the two rectangles overlap (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:380-385`) |
| `IsEmptyArea()` | method | true when the width or the height is not positive |
| `Offset(dx, dy)` | method | shifts the origin (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:385-388`) |
| `Union(...)` | method | the **instance** form is a port addition: 0 arguments returns a copy, 1 unions with it, 2 unions the two. The reference registers `Union` as the class-level static below |
| `x`, `y`, `width`, `height` | property | real, read/write |
| `left`, `top`, `right`, `bottom` | property | real, read-only; `right` is `x + width`, `bottom` is `y + height` |
| `location` | property | read-only; a fresh `PointF` at the minimum corner |
| `bounds` | property | read-only; a fresh `RectF` with the same components |
| `GdiPlus.RectF.Union(ret, a, b)` | static method | writes the bounding rectangle of `a` and `b` into `ret` and returns `1` (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:389-397`, `reference/cpp/plugins/layerex_draw/windows/main.cpp:210`) |

#### `GdiPlus.Image` (14 members)

| member | kind | behaviour |
|---|---|---|
| `Image()` / `Image(name)` | constructor | no argument is an empty image; a string loads it and raises `cannot open:<name>` when the load yields nothing; any other argument is `invalid parameter` (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:3338-3354`) |
| `load(filename)` | method | replaces the current image with the decoded one; a missing or undecodable file leaves an empty image (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:3356-3363`) |
| `Clone()` | method | a fresh image with the same pixels and transform |
| `GetBounds()` | method | a `RectF`: `(0, 0, width, height)` for a bitmap, `(0, 0, 0, 0)` when empty (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:668-694`) |
| `GetFlags()` | method | the `ImageFlagsHasAlpha` and `ImageFlagsColorSpaceRGB` flags for a non-empty bitmap, the `ImageFlagsReadOnly` and `ImageFlagsColorSpaceRGB` flags for a vector image, `0` for an empty one (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:696-706`) |
| `GetHeight()` | method | integer pixel height |
| `GetWidth()` | method | integer pixel width |
| `GetHorizontalResolution()` | method | 96.0 (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:716`) |
| `GetVerticalResolution()` | method | 96.0 (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:733`) |
| `GetLastStatus()` | method | `0` (`Ok`) for a non-empty image, `1` (`GenericError`) for an empty one (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:708-710`) |
| `GetPixelFormat()` | method | `PixelFormat32bppARGB` when the source had an alpha channel, `PixelFormat32bppRGB` otherwise, `0` for an empty or vector image (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:712-723`) |
| `GetType()` | method | `ImageTypeBitmap` for a non-empty bitmap, `ImageTypeUnknown` for an empty one (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:725-731`) |
| `RotateFlip(type)` | method | applies the rotate/flip transform; `0` (`Ok`) for the seven handled types, `6` (`NotImplemented`) otherwise (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:736-773`) |

#### `GdiPlus.Matrix` (17 members)

| member | kind | behaviour |
|---|---|---|
| `Matrix()` / `Matrix(m11, m12, m21, m22, dx, dy)` | constructor | no argument is the identity, six arguments are the elements, any other arity is `invalid parameter`; the reference's two-argument `(RectF, points)` branch never matches and raises too (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:3206-3285`) |
| `OffsetX()` | method | the cached x translation |
| `OffsetY()` | method | the cached y translation |
| `Equals(matrix)` | method | exact comparison of the six core elements; the argument converts from a `Matrix`, a six-element array or an object with `m11`…`dy` |
| `SetElements(m11, m12, m21, m22, dx, dy)` | method | replaces the core and refreshes the cached values; returns `0` (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:443-455`) |
| `GetLastStatus()` | method | always `0` (`Ok`) |
| `Invert()` | method | inverts the core; `0` on success, `2` (`InvalidParameter`) for a singular matrix (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:459-466`) |
| `IsIdentity()` | method | exact equality with the identity |
| `IsInvertible()` | method | non-zero determinant |
| `Multiply(matrix, order)` | method | multiplies in the requested order; `2` (`InvalidParameter`) for a null matrix, `0` otherwise (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:476-490`) |
| `Reset()` | method | the identity core and the default cached values |
| `Rotate(angle, order)` | method | rotates by `angle` **degrees**; the reference narrows the radians to `float` (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:502-516`) |
| `RotateAt(angle, center, order)` | method | rotates around `center` (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:518-537`) |
| `Scale(sx, sy, order)` | method | scales (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:539-551`) |
| `Shear(shx, shy, order)` | method | shears (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:553-570`) |
| `Translate(dx, dy, order)` | method | translates (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:572-584`) |

`order` is `GdiPlus.MatrixOrderPrepend` (0) or `GdiPlus.MatrixOrderAppend` (1).

#### `GdiPlus.Path` (19 members)

| member | kind | behaviour |
|---|---|---|
| `Path()` | constructor | an empty path |
| `startFigure()` | method | makes the next primitive connect to the current figure instead of starting a new one (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1133-1134`) |
| `closeFigure()` | method | closes the current figure; with no open figure nothing changes (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1138-1139`) |
| `drawLine(x1, y1, x2, y2)` | method | a two-point polyline (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1368-1381`) |
| `drawLines(points)` | method | a polyline through every point; an empty array changes nothing (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1383-1403`) |
| `drawPolygon(points)` | method | its own closed figure (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1405-1425`) |
| `drawRectangle(x, y, w, h)` | method | its own closed figure (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1429-1437`) |
| `drawRectangles(rects)` | method | one closed figure per rectangle (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1441-1451`) |
| `drawEllipse(x, y, w, h)` | method | its own closed figure (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1348-1359`) |
| `drawArc(x, y, w, h, startAngle, sweepAngle)` | method | appends the arc to the current figure (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1149-1160`) |
| `drawPie(x, y, w, h, startAngle, sweepAngle)` | method | its own closed figure: the arc and the centre (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1312-1341`) |
| `drawBezier(x1, y1, x2, y2, x3, y3, x4, y4)` | method | one cubic Bézier; with an open figure the curve starts at the current point and `(x1, y1)` is ignored (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1172-1184`) |
| `drawBeziers(points)` | method | a chain of cubic Béziers; a point count that is not `3n + 1` adds nothing (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1186-1207`) |
| `drawCurve(points)` | method | a cardinal spline with tension 0.5 (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1253`) |
| `drawCurve2(points, tension)` | method | a cardinal spline with the given tension (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1261-1271`) |
| `drawCurve3(points, offset, numberOfSegments, tension)` | method | a sub-range of a cardinal spline; a point count below 2, a negative offset or `offset + numberOfSegments >= size` makes the call a no-op (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1273-1310`) |
| `drawClosedCurve(points)` | method | a closed cardinal spline with tension 0.5 (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1216-1222`) |
| `drawClosedCurve2(points, tension)` | method | a closed cardinal spline with the given tension (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1224-1247`) |

`Path` exists in the reference's blend2d build only; the Windows build has no
such class.

#### `GdiPlus.Appearance` (5 members)

| member | kind | behaviour |
|---|---|---|
| `Appearance()` | constructor | an empty draw list |
| `clear()` | method | empties the draw list |
| `addBrush(colorOrBrush, ox, oy)` | method | appends a **fill**; three arguments required (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:921`) |
| `addPen(colorOrBrush, widthOrOption, ox, oy)` | method | appends a **stroke**; four arguments required; `widthOrOption` is a number or a dictionary with a `width` member (default 1.0) (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:930-931`) |

#### `GdiPlus.Font` (11 members)

The C++ class is `FontInfo`; the script name is `GdiPlus.Font`
(`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:3649`).

| member | kind | behaviour |
|---|---|---|
| `FontInfo(familyName, emSize, style)` | constructor | three arguments required (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:3419`) |
| `familyName` | property | string, read/write |
| `emSize` | property | real, read/write |
| `style` | property | integer, read/write; bit 0 is bold, bit 1 is italic (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.hpp:81-85`) |
| `forceSelfPathDraw` | property | boolean, read/write |
| `ascent` | property | real, read-only; the Windows cell metric `tmAscent` |
| `descent` | property | real, read-only; the Windows cell metric `tmDescent` |
| `ascentLeading` | property | real, read-only; `tmAscent - otmAscent` |
| `descentLeading` | property | real, read-only; `tmDescent + otmDescent` |
| `lineSpacing` | property | real, read-only; `tmHeight`, i.e. `tmAscent + tmDescent` (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:362-383`) |

### `Layer` members

The plugin installs **48** members on the core `Layer` class — 39 methods, 4
properties, `perspectiveCopy` and the four movie methods. The reference
registers them on an attached `LayerExDraw` instance
(`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:3707-3756`),
`layerExMovie` (`reference/cpp/plugins/layerExMovie.cpp:337-341`) and the
`Layer` class object (`reference/cpp/plugins/layerExPerspective.cpp:286-293`).

| member | kind | behaviour |
|---|---|---|
| `clear(argb)` | method | fills the whole main image with the ARGB colour, ignoring the transform (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1623-1633`) |
| `drawPath(app, path)` | method | fills and strokes a `Path` with an `Appearance`; returns the updated `RectF` |
| `drawArc(app, x, y, w, h, startAngle, sweepAngle)` | method | 7 arguments; draws an open arc |
| `drawPie(app, x, y, w, h, startAngle, sweepAngle)` | method | 7 arguments; draws a closed pie |
| `drawBezier(app, x1, y1, x2, y2, x3, y3, x4, y4)` | method | 9 arguments; draws one cubic Bézier |
| `drawBeziers(app, points)` | method | draws a chain of cubic Béziers |
| `drawLines(app, points)` | method | draws a polyline |
| `drawPolygon(app, points)` | method | draws a closed polygon |
| `drawRectangle(app, x, y, w, h)` | method | 5 arguments; draws a closed rectangle |
| `drawRectangles(app, rects)` | method | draws one closed rectangle per rectangle |
| `drawEllipse(app, x, y, w, h)` | method | 5 arguments; draws a closed ellipse |
| `drawCurve(app, points)` | method | draws a cardinal spline with tension 0.5 |
| `drawCurve2(app, points, tension)` | method | draws a cardinal spline with the given tension |
| `drawCurve3(app, points, offset, numberOfSegments, tension)` | method | draws a sub-range of a cardinal spline |
| `drawClosedCurve(app, points)` | method | draws a closed cardinal spline with tension 0.5 |
| `drawClosedCurve2(app, points, tension)` | method | draws a closed cardinal spline with the given tension |
| `drawImage(x, y, image)` | method | copies a `Layer` or `Bitmap` at `(x, y)` (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:2264-2267`) |
| `drawImageRect(dleft, dtop, image, sleft, stop, swidth, sheight)` | method | copies a source rectangle to a destination origin (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:2280-2285`) |
| `drawImageStretch(dleft, dtop, dwidth, dheight, image, sleft, stop, swidth, sheight)` | method | scales the source rectangle into the destination rectangle (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:2300-2308`) |
| `drawImageAffine(image, sleft, stop, swidth, sheight, affine, A, B, C, D, E, F)` | method | copies under an affine transform (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:2319-2323`) |
| `drawString(font, app, x, y, text)` | method | draws a text run tinted with the appearance's first brush; returns the updated `RectF` (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:2157-2158`) |
| `drawPathString(font, app, x, y, text)` | method | draws the text once per appearance entry: a brush fills the glyph coverage, a pen strokes it (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:2141-2143`) |
| `measureString(font, text)` | method | the layout rectangle of the text as a `RectF` (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:2221`) |
| `measureStringInternal(font, text)` | method | the tight ink rectangle of the text as a `RectF` (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:2248-2250`) |
| `setTransform(matrix)` | method | replaces the world transform; the argument is a `Matrix`, a six-element array or an object with `m11`…`dy` |
| `resetTransform()` | method | the identity world transform |
| `rotateTransform(angle)` | method | rotates the world transform by `angle` degrees |
| `scaleTransform(sx, sy)` | method | scales the world transform; `sy` defaults to `sx` |
| `translateTransform(dx, dy)` | method | translates the world transform |
| `setViewTransform(matrix)` | method | replaces the view transform |
| `resetViewTransform()` | method | the identity view transform |
| `rotateViewTransform(angle)` | method | rotates the view transform by `angle` degrees |
| `scaleViewTransform(sx, sy)` | method | scales the view transform |
| `translateViewTransform(dx, dy)` | method | translates the view transform |
| `getRecordImage()` | method | a fresh `Bitmap` with the recording replayed into it; `void` when nothing is recorded |
| `redrawRecord()` | method | re-renders the recording into the layer; returns `true` |
| `saveRecord(filename)` | method | writes the recording to the host filesystem; `true` on success, `false` when nothing is recorded |
| `loadRecord(filename)` | method | reads a recording and re-renders it; returns `true` |
| `perspectiveCopy(src, sleft, stop, swidth, sheight, x1, y1, x2, y2, x3, y3, x4, y4)` | method | projects the source rectangle onto the destination quad; 13 arguments required (`reference/cpp/plugins/layerExPerspective.cpp:166-199`) |
| `openMovie(filename, alpha)` | method | opens a movie, sizes the layer to it and sets the layer type; 2 arguments required (`reference/cpp/plugins/layerExMovie.cpp:72,338`) |
| `startMovie(loop)` | method | starts playback and fires `onStartMovie`; 1 argument required (`reference/cpp/plugins/layerExMovie.cpp:74,339`) |
| `stopMovie()` | method | stops playback, releases the movie and fires `onStopMovie` when it had been playing (`reference/cpp/plugins/layerExMovie.cpp:214-227`) |
| `isPlayingMovie()` | method | whether the layer's movie is playing (`reference/cpp/plugins/layerExMovie.cpp:228`) |
| `updateWhenDraw` | property | boolean, read/write; `true` by default. When false a draw does not mark the layer for repaint (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:1022-1029`) |
| `smoothingMode` | property | integer, read/write; the default is `4` (`SmoothingModeAntiAlias`) (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:1003-1009`) |
| `textRenderingHint` | property | integer, read/write; the default is `4` (`TextRenderingHintAntiAlias`) (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:1011-1013`) |
| `record` | property | boolean, read/write; `true` starts recording the draw operations, `false` drops the recording |

The movie **callbacks** `onStartMovie`, `onStopMovie` and `onUpdateMovie` are
not installed members: the reference reads them off the `Layer` object and
calls them when they are there
(`reference/cpp/plugins/layerExMovie.cpp:104-121,205,222,274`). The port
resolves the layer's script object at dispatch time and calls the member, so
a script override is what fires; a layer without the member logs a warning
and the playback continues.

## Behaviour

### The namespace and its nested classes

The reference publishes one global class object, `GdiPlus`
(`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:3463`), and
attaches the value types with `NCB_SUBCLASS`, which never publishes a global
of its own
(`reference/cpp/core/plugin/ncbind.hpp:1792-1799,1963-1990`). The port
registers each nested class through the always-global registration ABI,
aliases the class object into `GdiPlus` and then deletes the registration
global, so a script sees `GdiPlus.PointF` and never a bare `PointF`.

### The value types are value holders

`PointF`, `RectF`, `Matrix`, `Image`, `Font` and `Appearance` hold their
state in a native payload; `Path` holds a list of figures. Nothing is stored
in a hidden script property, and no member keeps a pointer to another
object.

### `Appearance` is an ordered draw list, not a brush

`addBrush` appends a fill, `addPen` appends a stroke, and the draw methods
walk the list in insertion order
(`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:422,767,778,1150`).
A brush fills the path, a pen strokes it.

The paint kinds the port rasterizes are solid, hatch and linear gradient.
Texture and path-gradient brushes raise a typed error rather than drawing
nothing. A colour argument is either a number/string (the reference
`AsInteger` conversion: `void` is 0, strings parse, objects and octets
raise) or a brush dictionary whose `type` selects the kind
(`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:626-756`).

### `Path` keeps one current figure

The reference keeps a "current figure" flag: a primitive after
`startFigure()` connects to the current figure, a closed primitive always
adds its own figure, and `closeFigure` closes the current one
(`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1133-1451`). The
port models the same thing as a list of figures where the open last figure
is the current one.

Every figure is **flattened to a polyline as it is added**, with the
engine's arc and cubic flattening primitives, so `Layer.drawPath` can fill
and stroke it with the CPU rasterizer.

### Drawing on a layer

Every `draw*` method rasterizes into the layer's main image through the
engine's CPU paint path: a 4×4 supersampled fill for a brush, a
distance-to-segment stroke for a pen, an affine blit for an image. The
layer's clip rectangle applies, and the layer is marked for repaint when
`updateWhenDraw` is true
(`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1456-1462`).

The `Appearance` and `Path` arguments are resolved through the
native-instance lookup, so an object of the wrong class raises
`…: the argument is not of the Appearance type` instead of being coerced
(`reference/cpp/plugins/layerex_draw/windows/main.cpp:105-113,596,872-902`).

An image argument is a `Layer` (its main image) or a `Bitmap`
(`reference/cpp/plugins/layerex_draw/windows/main.cpp:596`).

### Transforms

Each layer carries a world transform and a view transform; the combined
transform is the world transform followed by the view transform. Every draw
method maps its coordinates through it. The transform members re-render the
recording when they change, so a recorded drawing follows the new transform
(`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1553-1563`).

### Recording and replay

`record = true` starts recording. The recorded operations are the paths with
their appearances, the image blits and the clears — the reference stores an
EMF+ metafile
(`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:1770-1781`),
while the port records the same operations as plain geometry so they can be
replayed with the current transform and serialized without a Windows
metafile dependency.

`getRecordImage()` replays the recording into a fresh image, `redrawRecord()`
re-renders it into the layer, and `saveRecord` / `loadRecord` round-trip it
through a file. The recording format is the plugin's own (`KEXR`, version 1).

### Text

`GdiPlus.Font` resolves its family through the engine's text service — the
same face resolution the visual `Font` native uses, so a face registered
with `System.addFont` or `GdiPlus.addPrivateFont` is reachable by name. The
glyphs are shaped and rasterized by the engine's text engine, and the
coverage is tinted with the paint.

`drawString` uses the first brush of the appearance; `drawPathString` draws
the text once per appearance entry, filling with a brush and stroking with a
pen. `measureString` returns the layout rectangle and `measureStringInternal`
the tight ink rectangle.

### Movie playback

`openMovie` reads the stream metadata through the host-installed decoded
source, sizes the layer to the movie, sets the layer type (`alpha` halves
the width and makes the layer alpha-blended) and installs the decoded audio
track on the shared mixer at the device defaults — unity gain, centre pan,
because `layerExMovie` exposes no volume or pan members
(`reference/cpp/plugins/layerExMovie.cpp:154-227`).

The host drives the playback: `compat::movie_poll(ctx, max_step_ms)` advances
every playing movie by the **engine clock's** elapsed time since the previous
advance, bounded by the caller's cap, presents the frame, copies it into the
layer and dispatches the callbacks. It is the analogue of the reference's
continuous event hook
(`reference/cpp/plugins/layerExMovie.cpp:288-312`).

The compatibility layer owns no decoder: the host installs the same
`VideoSource` it uses for `VideoOverlay` with `compat::set_movie_source`.

### Perspective copy

`perspectiveCopy` maps the source rectangle onto the destination quad with
an inverse homography, samples the source bilinearly and composites the
result into the layer. The reference uses the render manager's
`PerspectiveAlphaBlend_a` method with opacity 255
(`reference/cpp/plugins/layerExPerspective.cpp:195-227`); the port uses the
engine's own blend primitive.

The source argument must be a `Layer`; the reference raises
`TVPSpecifyLayer` for anything else
(`reference/cpp/plugins/layerExPerspective.cpp:169-176`).

## State and lifetime

| state | where it lives | released |
|---|---|---|
| per-layer draw state (world transform, view transform, smoothing mode, text hint, `updateWhenDraw`, the recording) | the `DrawState` map in the compatibility services' state, keyed by layer id | the layer's invalidate and destroy hooks call `layer_ex::forget_layer` (`crates/compat/src/natives/visual/layer/mod.rs:408,450,478`) |
| per-layer movie slot (the playback machine, the path, the alpha flag, the mixer voice) | the `LayerExMovieState` map in the same state map, keyed by layer id | the same hook; dropping the slot releases the voice and closes the overlay |
| the host-installed decoded movie source | the same map, one per engine | the engine |
| the text engine, the face cache and the private face list | the `TextState` in the same state map | the engine |
| the value types (`PointF`, `RectF`, `Image`, `Matrix`, `Path`, `Appearance`, `Font`) | a native payload per script object | the VM frees the object; the class's `destroy` drops the payload |
| the string and array result scratch buffers | the services' state and the linked-array scratch | overwritten by the next result |

The reference keeps the same per-layer state on the attached native instance
(`reference/cpp/plugins/layerExMovie.cpp:316-329`); the port keys it by layer
id because the Rust `Layer` class has no attached instance to hang it on.

Nothing in this family is process-global: the state belongs to the
compatibility services of one engine.

## Errors

Every failure is a typed TJS error. The messages are grouped by the family
that raises them.

| family | representative message | raised when |
|---|---|---|
| argument counts | `Layer.drawArc requires 7 arguments`, `PointF requires 2 arguments`, `RectF requires 4 arguments`, `GdiPlus.Matrix.SetElements requires 6 arguments`, `Appearance.addPen requires 4 arguments`, `FontInfo requires 3 arguments`, `Layer.perspectiveCopy requires 13 arguments`, `Layer.openMovie requires 2 arguments` | fewer arguments than the reference member declares |
| wrong argument type | `Layer.drawPolygon: the argument is not of the Appearance type`, `Layer.drawPath: the argument is not of the Path type`, `Layer.perspectiveCopy: the source is not a Layer` | the native-instance lookup finds no payload of the expected class |
| numeric conversion | `PointF: x and y must be numbers`, `the argument is not a number`, `GdiPlus.Matrix.Rotate: the argument is not a number` | an object or an octet is passed where the reference `AsReal` / `AsInteger` conversion raises |
| a shape or a list | `Layer.drawBeziers requires a point array`, `Path.drawRectangles: invalid rectangles` | the point/rectangle array is malformed |
| unsupported paint | `Appearance.addBrush: texture brushes are not supported by the CPU layer rasterizer`, `Appearance.addPen: path-gradient brushes are not supported by the CPU layer rasterizer`, `Appearance.addBrush: unknown brush type 9` | the brush kind the CPU rasterizer does not implement |
| a missing file | `cannot open:<name>`, `blend2d cannot load:<name>` | a font or an image the storage cannot supply |
| a malformed recording | `record: not a layerExDraw recording`, `record: unsupported version 2`, `record: truncated data`, `record: unknown op tag 7` | a file that is not one of this plugin's recordings |
| a missing movie | `Layer.openMovie: no movie source installed`, `Layer.openMovie: no video metadata for "movie.mpg"`, `Layer.startMovie: no movie opened` | the host installed no decoded source, the stream has no metadata, or playback was asked for without an open movie |
| the receiver | `Layer.drawPath: the receiver is not a Layer`, `Layer.perspectiveCopy: the receiver is not a Layer` | the member was called on an object that is not a `Layer` |
| the engine seam | `LayerEx: compat context is not bound`, `LayerEx: no scene attached` | the native callback ran without a bound engine or scene |

A storage read that fails for a reason other than "not found" is surfaced as
itself — it is not retried as the next candidate and not reported as a
missing file.

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/layer_ex.rs` | the end-to-end surface: `perspectiveCopy` (identity, a smaller quad, the argument count), the movie surface and its error paths, the movie callbacks as layer members, a movie layer released when the script drops it, the `PointF`/`RectF` value types, `Appearance` with solid and hatch brushes, `Path` construction and drawing, `Layer.clear` and the strokes, the shape draws, the native-instance lookup (no `__handle` member), the game's `ResizeButton` shape, the rejection of a non-`Appearance` argument, the transforms, the image draws, the record round-trip, the `FontInfo` properties and metrics, the text measurement and drawing, the movie audio at the device gain, the audio decode failure, and the `file:` media prefix |
| `crates/compat/tests/layer_ex_behaviour.rs` | the reference-behaviour parity of the value types: the `GdiPlus` namespace shape and its enum constants, the `PointF`/`RectF` constructor arities, components, derived edges, `Clone`, `Equals`, `Inflate`, `Offset`, `IntersectsWith`, `IsEmptyArea` and the static `Union`, the `FontInfo` arity, property types and metrics, the `Appearance` arities, `clear` and the dictionary defaults, the `Path` arities, the closed areas and an invalid Bézier chain, the `Matrix` arities, `SetElements`, `Invert`, `Multiply`, `Translate` and the array/object conversions, the `Image` factory, accessors, `Clone`, `RotateFlip` and the name-resolution rules, the private-font registration and listing, `perspectiveCopy`, the movie arities, and the record lifecycle |
| `crates/compat/src/natives/plugins/layer_ex/movie.rs` (unit tests) | the movie playback itself: the layer is sized to the movie and the frames are copied, the callbacks fire through the layer object, and `openMovie` without a host source raises |
| `crates/compat/tests/runtime_surface.rs` | the live surface: the members `CompatHost::install` registered, frozen by `docs/surface/runtime_surface.json` |
| `docs/plugin_parity.md` | the machine diff of the reference plugin surface against the registered classes: `GdiPlus` 157/157, `GdiPlus.PointF` 3/4, `GdiPlus.RectF` 18/19, `GdiPlus.Path` 17/18, `GdiPlus.Appearance` 3/4, `GdiPlus.Font` 9/10, `GdiPlus.Image` 0/34, `GdiPlus.Matrix` 0/16, `Layer` 48/48 — 100% coverage, 0 missing |

The parity counts are the extractor's: it reads the reference's `NCB_*`
macros and does not count `NCB_CONSTRUCTOR` as a member, which is the one
extra Rust member per class. It also cannot read the members of the two GDI+
wrapper classes, which the reference registers through
`NCB_REGISTER_GDIP_SUBCLASS2`
(`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:3286,3397`), so
`GdiPlus.Image` and `GdiPlus.Matrix` have no reference members in the report.

## Known gaps

* **`Layer.loadRecord` returns `true`; the reference returns `false`.** The
  reference computes the redraw result and then falls through to
  `return false` (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:1890-1901`,
  `reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:2504-2513`). The
  recorded content still round-trips; only the boolean differs. The same
  holds for `saveRecord`: the reference's blend2d build always returns
  `false` (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:2474-2496`),
  while the port reports the write result.
* **`Layer.getRecordImage` returns a core `Bitmap`, the reference returns a
  `GdiPlus.Image`** (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:2458-2467`).
  The pixels match; the object type does not.
* **`GdiPlus.RectF.Union` also exists as an instance method.** The reference
  registers it only as the class-level static
  (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:389-397`); the
  instance form is a port addition.
* **`smoothingMode` and `textRenderingHint` are stored but not applied.** The
  CPU rasterizer's anti-aliasing is fixed; the reference feeds the mode into
  the blend2d context
  (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:1003-1013`).
* **`FontInfo.forceSelfPathDraw` is stored but not applied.** The reference
  switches `drawString` to the glyph-outline path when it is set
  (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:2159-2160`);
  the port always draws the glyph coverage.
* **`drawImageAffine` with `affine == false` maps the three given points
  straight onto the destination corners.** The reference derives the
  transform from the differences between the three points
  (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:2329-2337`),
  so the two agree only for a unit source rectangle.
* **A linear-gradient brush is interpolated along a straight line.** The
  reference builds a GDI+ brush from the rectangle and the angle
  (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:725-731`); the
  port's two-colour interpolation is an approximation of it, not a clone.
* **`openMovie` raises where the reference logs.** The reference logs the
  file name followed by `:ファイルが開けません` and returns
  (`reference/cpp/plugins/layerExMovie.cpp:160-166`); the port reports the
  missing source or metadata as a typed error. `startMovie` after a
  `stopMovie` likewise raises, where the reference's `if(VideoOverlay)` guard
  skips the call silently
  (`reference/cpp/plugins/layerExMovie.cpp:199`).
* **The movie callbacks are read at dispatch time.** The reference reads
  `onStartMovie` / `onStopMovie` / `onUpdateMovie` once, when the attached
  instance is built (`reference/cpp/plugins/layerExMovie.cpp:104-121`), so a
  later override is not seen. The port resolves the member when the event
  fires, so a later override wins; a layer without the member logs a warning.
* **`GdiPlus.Path` is installed in every build.** The reference publishes it
  in the blend2d build only
  (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:3651`); the
  Windows build has no such class.
* **A `GdiPlus.Image` is always a bitmap.** The reference's `GdipImage` can
  also hold a vector metafile, and `GetType`, `GetBounds` and `GetFlags` have
  a metafile branch
  (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:668-731`); the
  port only ever decodes a bitmap, so those branches are unreachable.
* **The hatch and gradient rasterization is the engine's, not GDI+'s.** The
  visible result is close, but the pixel output is not a clone of the
  reference renderer.

## See also

* [`../plugins.md`](../plugins.md) — the plugin index and the built-in table
* [`../plugin.md`](../plugin.md) — the registry that decides built-in vs absent
* [`../plugins_native.md`](../plugins_native.md) — the `Plugins` class
* [`../README.md`](../README.md) — the compatibility layer index
* [`../../../../docs.old/gdiplus.md`](../../../../docs.old/gdiplus.md) — the archived GDI+ notes (history, not contract)
* [`../../../plugin_parity.md`](../../../plugin_parity.md) — the plugin parity report
* [`../../../surface/runtime_surface.json`](../../../surface/runtime_surface.json) — the frozen live surface
