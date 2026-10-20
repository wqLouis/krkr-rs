# TextRender (`TextRender.dll`)

**DLL name:** `TextRender.dll` — linked with `Plugins.link("TextRender.dll")`
**Rust module:** `crates/compat/src/natives/plugins/text_render/`
**Reference:** `reference/cpp/plugins/TextRender.cpp`
**Registers:** the `TextRenderBase` class

## Surface

### Methods

| member | kind | behaviour |
|---|---|---|
| `TextRenderBase()` | constructor | the reference constructor takes no arguments |
| `render(text, autoIndent, diff, all, same)` | method | lays out the text and returns `!overflow` |
| `setRenderSize(width, height)` | method | sets the render box and clears the layout |
| `setDefault(dictionary)` | method | replaces the default style |
| `setOption(options)` | method | accepted and discarded (the reference's deserializer is a no-op) |
| `getCharacters(start, end)` | method | the positioned characters as a TJS `Array` |
| `clear()` | method | resets the characters and the style to the default |
| `done()` | method | flushes the pending character buffer |
| `newline()` | method | the reference logs a warning and does nothing |
| `resetFont()` | method | restores the default style |
| `resetStyle()` | method | restores the default style |
| `getKeyWait()` | method | the reference returns an empty array |
| `calcShowCount()` | method | the number of positioned characters |

### Properties

| member | kind | behaviour |
|---|---|---|
| `vertical` | property (read/write) | vertical writing mode |
| `bold` | property (read/write) | the bold style |
| `italic` | property (read/write) | the italic style |
| `face` | property (read/write) | the font face |
| `fontSize` | property (read/write) | the font size |
| `fontScale` | property (read/write) | the font scale |
| `chColor` | property (read/write) | the character colour |
| `rubySize` | property (read/write) | the ruby size |
| `rubyOffset` | property (read/write) | the ruby offset |
| `shadow` | property (read/write) | the shadow setting |
| `edge` | property (read/write) | the edge setting |
| `lineSpacing` | property (read/write) | the line spacing |
| `pitch` | property (read/write) | the character pitch |
| `lineSize` | property (read/write) | the line size |
| `defaultBold` | property (read/write) | the default bold style |
| `defaultItalic` | property (read/write) | the default italic style |
| `defaultFace` | property (read/write) | the default face |
| `defaultFontSize` | property (read/write) | the default font size |
| `defaultFontScale` | property (read/write) | the default font scale |
| `defaultChColor` | property (read/write) | the default character colour |
| `defaultRubySize` | property (read/write) | the default ruby size |
| `defaultRubyOffset` | property (read/write) | the default ruby offset |
| `defaultShadow` | property (read/write) | the default shadow setting |
| `defaultEdge` | property (read/write) | the default edge setting |
| `defaultLineSpacing` | property (read/write) | the default line spacing |
| `defaultPitch` | property (read/write) | the default character pitch |
| `defaultLineSize` | property (read/write) | the default line size |
| `renderText` | property (read/write) | the text laid out by the last `render` |
| `renderLeft` | property (read/write) | the left edge of the render box |
| `renderRight` | property (read/write) | `x + boxWidth` |
| `renderTop` | property (read/write) | the top edge of the render box |
| `renderBottom` | property (read/write) | `y + boxHeight` |
| `renderCount` | property (read-only) | the number of UTF-16 units in `renderText` |
| `renderOver` | property (read-only) | whether the last render overflowed the box |
| `renderDelay` | property (read-only) | the render delay |
| `keyWait` | property (read-only) | an empty array |

The reference registers `render` with `NCB_METHOD`, and ncbind rejects calls
with fewer arguments than the C++ signature has parameters
(`reference/cpp/core/plugin/ncbind.hpp:1178`), so all five `render`
arguments are required; `setRenderSize` requires two and `setDefault` and
`setOption` require one.

`renderRight` and `renderBottom` are derived: the getter is `x + boxWidth`
and `y + boxHeight`, and the setter is `boxWidth = value - x` (respectively
`boxHeight = value - y`).

`renderCount` and `keyWait` refuse writes — the reference has no setter for
them.

## Behaviour

The plugin provides the KAG text-layout engine `TextRenderBase`: it parses
the KAG text markup (`%f`, `%b`, `\n`, `#rrggbb;`, …) into positioned
characters, tracks the render box and the per-instance style, and exposes the
reference's full method and property surface.

### Layout

`render(text, autoIndent, diff, all, same)` lays out the text and returns
`!overflow` — the negation of whether the text overflowed the render box.
`setRenderSize(width, height)` sets the box and clears the layout.

The layout engine works in three modes, matching the reference's
`kTextRenderModeLeading` / `kTextRenderModeNormal` /
`kTextRenderModeFollowing`.

### Metrics

The reference asks the process-wide font rasterizer for per-character extents
and the ascent. The compat layer has one rasterizer and no font enumeration,
so the metrics come from the engine's `TextEngine` (cosmic-text shaping) and
a face is loaded from the attached storage on first use — the same seam the
visual `Font` native uses.

The reference's `"user"` face resolves to the newest font registered in the
attached engine scene, which is the compat equivalent of the rasterizer's
currently applied face.

The reference's `GetTextExtent(ch, w, h).w` — the shaped advance of a
character in pixels — is reproduced by shaping that one character alone,
deliberately, so there is no cross-character kerning, exactly as in the
reference's per-character extent.

### Member behaviour that mirrors a reference no-op

`newline()` logs a warning and does nothing; `getKeyWait()` returns an empty
array; `setOption(options)` accepts the argument and discards it, because the
reference's `TextRenderOptions::deserialize` is a no-op.

## State and lifetime

Every piece of layout state lives in the per-instance payload
(`TextRenderInst`): the current style, the default style, the options, the
render box, the cursor, the indent, the vertical flag, the positioned
characters, the pending buffer and the mode.

The shared shaping engine and the face cache live in the per-engine dispatch
state (`CompatServices::state`), never in a process global.

## Errors

| condition | outcome |
|---|---|
| `render` with fewer than five arguments | typed error: `TextRenderBase.render requires 5 arguments` |
| `setRenderSize` with fewer than two arguments | typed error: `TextRenderBase.setRenderSize requires 2 arguments` |
| `setDefault` without an argument | typed error: `TextRenderBase.setDefault requires 1 argument` |
| `setOption` without an argument | typed error: `TextRenderBase.setOption requires 1 argument` |
| a style dictionary that cannot be deserialized | typed error with the deserializer's message |
| a face that cannot be resolved or loaded | typed error |
| the compat context is not bound | typed error |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/text_render.rs` | the markup parsing, the layout result, the style properties and their defaults, the render box and the derived `renderRight` / `renderBottom`, `getCharacters`, `calcShowCount`, `renderCount`, the `done` / `clear` / `resetFont` / `resetStyle` behaviour, and the no-op members |
| `crates/compat/tests/text.rs` | the core text rendering the plugin's metrics build on |

## Known gaps

* The reference resolves a face through a process-wide font rasterizer with
  its own enumeration and application model. The compat layer has one
  shaping engine; a face is loaded from the attached storage on first use,
  and the reference's `"user"` placeholder resolves to the newest font in the
  attached scene. A game that relies on the reference's face *enumeration*
  (rather than naming a face) sees the scene's newest font instead.
* The reference's `TextRenderOptions::deserialize` is a no-op, so `setOption`
  discards its argument in the port as well.
