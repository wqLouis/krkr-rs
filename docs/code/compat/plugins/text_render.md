# TextRender (`TextRender.dll`)

**DLL name:** `TextRender.dll` — linked with `Plugins.link("TextRender.dll")`
**Rust module:** `crates/compat/src/natives/plugins/text_render/`
**Reference:** `reference/cpp/plugins/TextRender.cpp`
**Registers:** the `TextRenderBase` class — one constructor, 12 methods and 36 properties; no static member

The DLL is one entry of the built-in table
(`crates/compat/src/natives/plugins/mod.rs:160-164`), so the class is already
installed when the game's startup scripts run; `Plugins.link("TextRender.dll")`
records the link without registering again. See the
[plugin index](../plugins.md) for the linking rules.

`TextRenderBase` is the KAG text-layout engine: it parses the KAG text markup
into positioned characters, tracks the render box and the per-instance style,
and reports the result to the script.

## Surface

### Methods

| member | kind | behaviour |
|---|---|---|
| `TextRenderBase()` | constructor | takes no argument; `new TextRenderBase()` |
| `render(text, autoIndent, diff, all, same)` | method | lays the text out and returns `!overflow` |
| `setRenderSize(width, height)` | method | sets the render box and clears the layout |
| `setDefault(dictionary)` | method | replaces the default style |
| `setOption(options)` | method | accepted and discarded (the reference's option deserializer is a no-op) |
| `getCharacters(start, end)` | method | the positioned characters as a TJS `Array` of dictionaries |
| `clear()` | method | drops the placed characters and restores the default style |
| `done()` | method | flushes the pending character buffer |
| `newline()` | method | logs a warning and changes nothing |
| `resetFont()` | method | restores the default style |
| `resetStyle()` | method | restores the default style |
| `getKeyWait()` | method | an empty `Array` |
| `calcShowCount()` | method | the number of positioned characters |

### Properties

| member | kind | behaviour |
|---|---|---|
| `keyWait` | property (get/set) | the getter returns an empty `Array`; a write raises `no support setKeyWait` |
| `renderCount` | property (get/set) | the number of UTF-16 units in `renderText`; a write raises `avoid to set renderCount` |
| `renderOver` | property (read/write) | whether the last `render` overflowed the box |
| `renderDelay` | property (read/write) | stored; the layout does not use it |
| `renderText` | property (read/write) | the text laid out so far |
| `renderLeft` | property (read/write) | the pen's `x` |
| `renderRight` | property (read/write) | `x + boxWidth`; the setter sets `boxWidth = value - x` |
| `renderTop` | property (read/write) | the pen's `y` |
| `renderBottom` | property (read/write) | `y + boxHeight`; the setter sets `boxHeight = value - y` |
| `vertical` | property (read/write) | stored; the layout does not use it |
| `bold` | property (read/write) | the bold flag of the active style |
| `italic` | property (read/write) | the italic flag of the active style |
| `face` | property (read/write) | the face name of the active style |
| `fontSize` | property (read/write) | the font size of the active style |
| `fontScale` | property (read/write) | the font scale of the active style |
| `chColor` | property (read/write) | the character colour of the active style |
| `rubySize` | property (read/write) | the ruby size of the active style |
| `rubyOffset` | property (read/write) | the ruby offset of the active style |
| `shadow` | property (read/write) | the shadow flag of the active style |
| `edge` | property (read/write) | the edge flag of the active style |
| `lineSpacing` | property (read/write) | the line spacing of the active style |
| `pitch` | property (read/write) | the character pitch of the active style |
| `lineSize` | property (read/write) | the line size of the active style |
| `defaultBold` | property (read/write) | the bold flag of the default style |
| `defaultItalic` | property (read/write) | the italic flag of the default style |
| `defaultFace` | property (read/write) | the face name of the default style |
| `defaultFontSize` | property (read/write) | the font size of the default style |
| `defaultFontScale` | property (read/write) | the font scale of the default style |
| `defaultChColor` | property (read/write) | the character colour of the default style |
| `defaultRubySize` | property (read/write) | the ruby size of the default style |
| `defaultRubyOffset` | property (read/write) | the ruby offset of the default style |
| `defaultShadow` | property (read/write) | the shadow flag of the default style |
| `defaultEdge` | property (read/write) | the edge flag of the default style |
| `defaultLineSpacing` | property (read/write) | the line spacing of the default style |
| `defaultPitch` | property (read/write) | the character pitch of the default style |
| `defaultLineSize` | property (read/write) | the line size of the default style |

The reference's registration block
(`reference/cpp/plugins/TextRender.cpp:1066-1120`) exposes exactly these
members; [the parity report](../../../plugin_parity.md) counts 48 of them, the
constructor aside. The reference's class body also declares
`shadowColor` and `defaultShadowColor` accessors
(`reference/cpp/plugins/TextRender.cpp:330,358`), but its registration block
never exposes them and the port does not register them either.

The reference registers every one of these with `NCB_METHOD` or
`property_delegate`, and ncbind rejects a call with fewer arguments than the
C++ signature has parameters
(`reference/cpp/core/plugin/ncbind.hpp:1178`): `render` therefore takes all
five arguments, `setRenderSize` two, `setDefault` and `setOption` one and
`getCharacters` two.

`defaultFontScale` is stored through a 32-bit float in the reference
(`reference/cpp/plugins/TextRender.cpp:352`) and in the port, while the active
`fontScale` is a double in both. `chColor` and `defaultChColor` hold an
unsigned 32-bit colour that is read back through a signed 32-bit cast, so a
colour above `0x7fffffff` reads back negative — the reference's behaviour
(`reference/cpp/plugins/TextRender.cpp:326,353`).

## Behaviour

### The markup parser

`render(text, autoIndent, diff, all, same)` walks the text once
(`reference/cpp/plugins/TextRender.cpp:454-847`). The four arguments after
the text are accepted and ignored: the reference never assigns its
`m_autoIndent` field from the `autoIndent` argument, so the pre-indent branch
of `pushCharacter` never runs
(`reference/cpp/plugins/TextRender.cpp:919-934`).

The escapes the parser understands:

| input | effect |
|---|---|
| `%f<name>;` | sets the face of the active style |
| `%t<name>;` | the face name is read and discarded |
| `%b1` / `%b0` | sets / clears bold |
| `%i1` / `%i0` | sets / clears italic |
| `%s1` / `%s0` | sets / clears the shadow |
| `%e1` / `%e0` | sets / clears the edge |
| `%r` | restores the default style |
| `%p<n>;` | sets the character pitch |
| `%d<n>;`, `%w<n>;`, `%D<n>;` | the integer is read and discarded |
| `%l…;` | the text up to `;` is read and discarded |
| `%B`, `%S`, `%C`, `%R`, `%L` | logged and ignored |
| `%<n>;` | the font size becomes `defaultFontSize * n / 100` |
| `\n` | flush the buffer and break the line |
| `\t` | inserts a tab character |
| `\w` | inserts a space character |
| `\i` | the indent becomes the current `x` |
| `\r` | the indent becomes `0` |
| `\k`, `\x` | consumed, nothing inserted |
| `\<other>` | the character itself |
| `[…]` | the ruby text is read and discarded |
| `&…;` | the graphical character is read and discarded |
| `$…;` | the variable name is read and discarded |
| `#rrggbb;` | sets the character colour; `#;` restores `0x00ffff` |

The flag escapes read their flag with a guard the reference can never
satisfy: `if(!readchar(...) && (ch == '0' || ch == '1'))`
(`reference/cpp/plugins/TextRender.cpp:511-579`), and `readchar` leaves `ch`
holding the format character when it runs past the end of the text
(`reference/cpp/plugins/TextRender.cpp:412-421`). The guard therefore never
fires and the flag is simply `ch == '1'` — false at the end of the text and
false for any character that is not `1`. The port reproduces that.

`%D` is the same story: the reference's label branch tests `ch == '$'` while
`ch` still holds `'D'`, so the branch is dead and the escape always reads an
integer (`reference/cpp/plugins/TextRender.cpp:630-659`).

### The layout and the render box

Characters are buffered and placed by the flush
(`reference/cpp/plugins/TextRender.cpp:940-977`): a character's `x` is the pen
position and its `y` the current line, and the pen advances by the
character's advance plus the pitch. When the pen would pass the box's right
edge, the line breaks: the pen returns to the indent and `y` grows by the
face's ascent plus `lineSpacing`
(`reference/cpp/plugins/TextRender.cpp:849-854`).

Three line-breaking modes decide whether the buffer is flushed before the
next character is pushed — *leading*, *normal* and *following*
(`reference/cpp/plugins/TextRender.cpp:400-404`): the buffer is flushed when
the current mode is *following* or is not *leading*
(`reference/cpp/plugins/TextRender.cpp:890-892`). The character sets that
define the modes are the reference's compiled-in options
(`reference/cpp/plugins/TextRender.cpp:167-180`); `setOption` cannot change
them, because the reference's option deserializer is a no-op
(`reference/cpp/plugins/TextRender.cpp:191-196`).

`render` returns `!overflow`, where the overflow is `y > boxHeight` measured
at the end of the parse; the same measurement sets `renderOver`
(`reference/cpp/plugins/TextRender.cpp:838-846`). `done()` flushes the
pending buffer (`reference/cpp/plugins/TextRender.cpp:1047-1050`), and
`setRenderSize` stores the box and then clears everything
(`reference/cpp/plugins/TextRender.cpp:979-987`).

`clear()` drops the placed characters, restores the default style and puts the
pen back to the origin; the pending buffer and the line mode survive it,
exactly as in the reference
(`reference/cpp/plugins/TextRender.cpp:1016-1031`).

### Faces and metrics

The reference asks the process-wide font rasterizer for each character's
advance (`GetTextExtent`) and for the face's ascent (`GetAscentHeight`), and
applies the face to that rasterizer through `updateFont`
(`reference/cpp/plugins/TextRender.cpp:894-899,1033-1045`). The rasterizer
resolves the face name through the font system
(`reference/cpp/core/visual/FreeTypeFontRasterizer.cpp:90-113`), which falls
back to its default face when the name is unknown — the reference's own
comment notes it (`reference/cpp/plugins/TextRender.cpp:1040-1041`).

The port asks the per-engine text engine (cosmic-text shaping) for the same
two numbers. One character is shaped on its own, so no cross-character
kerning is applied: that reproduces the reference's per-character extent
(`reference/cpp/core/visual/FreeTypeFontRasterizer.cpp:139-151`). The size is
used as its absolute value, as the reference's rasterizer does
(`reference/cpp/core/visual/FreeTypeFontRasterizer.cpp:113`), and a zero size
is raised to one, because FreeType rejects a zero em
(`reference/cpp/core/visual/FreeType.cpp:404-410`).

A face name is resolved through the shared resolver
(`crates/compat/src/resource.rs:269-300`): the faces the plugin's own engine
already holds, then the `System.addFont` registry (see
[add_font](add_font.md)), then the attached storage for a file-shaped name,
then the host's installed families, and finally the host's fallback family or
the engine's first registered face. A name may be a comma-separated candidate
list with an optional leading `@`, like the reference's font system
(`reference/cpp/core/visual/FontSystem.cpp:53-102`).

The `"user"` placeholder is special in the port: it resolves to the face name
of the most recently created `Font` in the attached scene and then through the
resolver above. The reference has no such special case — it hands the literal
name to the rasterizer, which holds the face that was applied to it last
(`reference/cpp/core/visual/FreeTypeFontRasterizer.cpp:90-113`).

### The active style and the default style

The class keeps two styles: the active one (`m_state` in the reference) and the
default one (`m_default`). `setDefault` replaces the default style only
(`reference/cpp/plugins/TextRender.cpp:989-992`); the active style follows it
on `clear`, `resetFont` and `resetStyle`
(`reference/cpp/plugins/TextRender.cpp:1016-1031,1052-1054`).

`setDefault` reads a dictionary and takes only the style members — `bold`,
`italic`, `fontSize`, `fontScale`, `face`, `chColor`, `rubySize`,
`rubyOffset`, `shadow`, `shadowColor`, `edge`, `edgeColor`, `lineSpacing`,
`pitch` and `lineSize`
(`reference/cpp/plugins/TextRender.cpp:137-158`). A member that is absent or
void leaves the field unchanged, an argument that is not an object is ignored
altogether, and the render-state members (`renderOver`, `renderDelay`,
`renderText`) are never read.

`fontScale`, `rubySize`, `rubyOffset`, `lineSize`, `renderDelay` and
`vertical` are stored but do not influence the layout: the reference stores
them without using them either, and its ruby, graphical and variable escapes
are read and discarded.

### The characters dictionary

`getCharacters(start, end)` returns an `Array` of dictionaries, one per
placed character, each with the keys `bold`, `italic`, `graph`, `vertical`,
`x`, `y`, `cw`, `size`, `face`, `color`, `edge`, `shadow` and `text`
(`reference/cpp/plugins/TextRender.cpp:225-248`). `cw` is the character's
advance, `size` the face's ascent, `color` the character colour, and `edge`
and `shadow` the edge and shadow colours — a colour of `0` is written as
`void` (`reference/cpp/plugins/TextRender.cpp:239-240`).

The array is filled only for the ranges the reference fills it for: `(0, 0)`
and every range with `end < start`. Any other range returns the empty array
the reference returns from its own unknown-behaviour branch
(`reference/cpp/plugins/TextRender.cpp:999-1013`).

## State and lifetime

* Every piece of layout state lives in the per-instance payload
  (`TextRenderInst`): the active style, the default style, the options, the
  render box, the pen, the indent, the auto-indent, the beginning-of-line
  flag, the vertical flag, the placed characters, the pending buffer and the
  line mode. The payload is created by the native instance's create hook and
  released by its destroy hook.
* The shared shaping engine and the requested-face cache live in the
  per-engine dispatch state
  (`CompatServices::state::<TextRenderEngine>()`), created on first use and
  released with the host. Nothing is kept in a process global.
* The cache maps a requested face name to the loaded face. A resolution found
  through a candidate is cached under that name; the default-face fallback is
  resolved under a fixed name and never shadows a face that is registered
  later (`crates/compat/src/resource.rs:254-300`).
* The `"user"` placeholder is not cached under its own name: it is resolved
  through the scene on every lookup, and the scene's face name is what the
  cache holds (`crates/compat/src/natives/plugins/text_render/metrics.rs:49-87`).

## Errors

| condition | outcome |
|---|---|
| `render` with fewer than five arguments | typed error: `TextRenderBase.render requires 5 arguments` |
| `setRenderSize` with fewer than two arguments | typed error: `TextRenderBase.setRenderSize requires 2 arguments` |
| `setDefault` without an argument | typed error: `TextRenderBase.setDefault requires 1 argument` |
| `setOption` without an argument | typed error: `TextRenderBase.setOption requires 1 argument` |
| `getCharacters` with fewer than two arguments | typed error: `TextRenderBase.getCharacters requires 2 arguments` |
| `renderCount = …` | typed error: `avoid to set renderCount` (the reference's message) |
| `keyWait = …` | typed error: `no support setKeyWait` (the reference's message) |
| the text ends right after `%` or `\` | typed error: `TextRenderBase::render() failed to parse: expected character, found EOF` |
| an integer escape ends without `;` | typed error: `… expected either integer or ';', found EOF` |
| an unknown `%` escape | typed error: `… expected any of 'fbiseBSrCRLpdwD0123456789', found '<c>'` |
| a non-hexadecimal digit in `#…;` | typed error: `… expected hexadecimal number, found '<c>'` |
| a character outside the basic multilingual plane | typed error: `unexpected character: surrogate pair` |
| a face that cannot be resolved | typed error: `no font is available for face '<face>'` |
| the shaping engine fails | typed error: `TextRender: text layout failed: …` or `TextRender: font metrics failed: …` |
| the compat context is not bound | typed error: `TextRenderBase.<member>: compat context is not bound` for the members that resolve the context; the string properties report `TextRenderBase: compat context is not bound` |

The parse messages are the reference's own, including their prefix
(`reference/cpp/plugins/TextRender.cpp:463-468,683-690,748-786`). The
surrogate-pair error is the reference's
(`reference/cpp/plugins/TextRender.cpp:869-873`): the reference sees the two
UTF-16 units of a non-BMP character, the port sees the single code point and
raises the same message.

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/text_render.rs` | the defaults and the style round-trip, the derived `renderRight` / `renderBottom`, the layout against the engine's text engine, the line break at the box width, the KAG markup parser, the typed parse errors, the unknown-face fallback, the `"user"` face through the scene font, `setDefault` / `clear` / `resetFont` / `resetStyle`, `setRenderSize`, the refused setters and `keyWait`, the ignored `setOption`, the argument counts, the flag escapes at the end of the text, the surrogate-pair rejection and the `getCharacters` ranges |
| `crates/compat/src/natives/plugins/text_render/mod.rs` (the `tests` module) | that the class registers and that a fresh instance carries the reference's defaults |
| `crates/compat/tests/add_font.rs` | the `System.addFont` registry the plugin consults for a face; the plugin itself is not exercised there |
| `crates/compat/tests/text.rs` | the decoding of the script text a game hands to `render` (the `FE FE` cipher, the zlib container, the BOMs and CP932); the plugin itself is not exercised there |

The font-dependent cases mount a system font and skip when none is present;
the error cases assert a typed error rather than a fabricated metric.

## Known gaps

* The reference asks a process-wide font rasterizer for the metrics and
  applies the face to it; the port asks the engine's text engine and keeps a
  requested-face cache. A face therefore comes from the `System.addFont`
  registry, the attached storage or the host's installed families rather than
  from the reference's own font map, and a name the reference's font system
  knows is not necessarily one the port can resolve.
* The metrics come from a different shaper. The reference measures a character
  with the FreeType face's `CellIncX` and reports the face's ascent
  (`reference/cpp/core/visual/FreeTypeFontRasterizer.cpp:139-157`); the port
  measures a cosmic-text layout. The algorithm is the reference's, and the
  ascent now follows its integer truncation (`ascender * ppem / units_per_EM`,
  `reference/cpp/core/visual/FreeType.h:96-99`); the per-character advances
  still come from cosmic shaping, so they can differ by a pixel, and with them
  the `x` / `cw` values a script reads back from `getCharacters` and the line
  breaks the layout makes.
* The `"user"` face names a different thing on each side. The reference hands
  the literal `"user"` to the font system, which answers with the default
  face when no font carries that name
  (`reference/cpp/core/visual/FreeTypeFontRasterizer.cpp:92`); the port maps
  it to the face of the newest `Font` in the attached scene and then resolves
  that name. A game that relies on `"user"` meaning "the font the game just
  created" sees that face; a game that relies on it meaning "the platform
  default" sees the scene's face instead.
* The reference's `updateFont` mutates the shared rasterizer, so laying out a
  text changes the font every other consumer of that rasterizer measures with
  (`reference/cpp/plugins/TextRender.cpp:1033-1045`). The port resolves a face
  per character and leaves no shared font state behind.
* `getCharacters` returns an empty array for every range but `(0, 0)` and
  `end < start`, because the reference leaves that case unimplemented
  (`reference/cpp/plugins/TextRender.cpp:1004-1011`). The port reproduces the
  gap rather than inventing a range semantics.
* The four option character sets (leading, following, begin, end) are fixed
  in both, because the reference's option deserializer does nothing
  (`reference/cpp/plugins/TextRender.cpp:191-196`).
