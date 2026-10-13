# Fonts and face resolution

krkr-rs selects text faces in the reference's order. A requested family is
looked up first through the faces a script registered (`System.addFont`,
`GdiPlus.addPrivateFont`), then as a storage name, then in the host's installed
system fonts, and finally falls back to the host's default face. There is no
project-specific font-configuration file: the application supplies the host
font directories, and scripts supply extra faces.

Reference behaviour: `reference/cpp/core/visual/FontImpl.cpp:159-168`
(unknown name resolves to the default face) and the bundled default face at
`FontImpl.cpp:220-243`.

---

## System font sources

The engine resolves a face through a lazily built `fontdb::Database`
(`engine::text::TextEngine`). The database is built the first time a face is
requested from the host, from two sources:

1. `fontdb`'s own system scan (`Database::load_system_fonts`). It knows the
   Windows, macOS and Unix/X11 font directories, but it deliberately excludes
   Android, so on Android this source contributes nothing.
2. The directories the composition root passes to
   `engine::text::TextEngine::set_system_font_dirs`, obtained from the L1
   `platform::system_font_dirs` (`crates/platform/platform/src/fonts.rs`):
   * Android — `/system/fonts`, `/product/fonts`, `/vendor/fonts`;
   * Unix-like desktop — `/usr/share/fonts`, `/usr/local/share/fonts`, the
     XDG per-user directory (`$XDG_DATA_HOME/fonts`, defaulting to
     `$HOME/.local/share/fonts`) and the legacy `$HOME/.fonts`.

A directory in the list that is not present on the host is skipped: it is an
absent source, not an error. Adding the directory list is what lets Android
reach its `NotoSansCJK-Regular.ttc`, which `fontdb` cannot find on its own.
The scan stays lazy, so the start-up cost is unchanged until text first needs
a host face.

> **Unverified on this host:** `platform::ANDROID_SYSTEM_FONT_DIRS` is
> compiled and unit-tested on the desktop host (the constant and its
> `/system/fonts` entry), but no Android device is exercised by this
> repository's test gate, so the device's actual font directories and their
> contents are not verified here.

---

## Face resolution

`compat::resource::resolve_face` resolves the layer's requested face string.
The string may be a comma-separated candidate list with an optional leading
`@` (vertical writing); each candidate is tried in order and the first that
resolves wins. For each candidate:

1. Already-registered family (`TextEngine::font_id`), including faces added by
   `System.addFont` (the per-engine `FontRegistry`).
2. The attached storage: the candidate string read as a storage name and
   parsed as a font file (`TextEngine::add_font`).
3. The host's installed system fonts, exact family only
   (`TextEngine::load_system_font_exact`); this never substitutes an unrelated
   face, so a missing candidate does not claim the next one's place.

When no candidate resolves, the host's default face is tried
(`TextEngine::load_system_font`), which walks the fallback in the reference's
order:

1. the Japanese/CJK fallback families: `Noto Sans CJK JP`, `Noto Sans CJK SC`,
   `Noto Sans CJK TC`, `Noto Sans CJK KR`, then `Droid Sans Fallback`;
2. the well-known Noto CJK collection paths;
3. otherwise `TextError::FontNotFound`.

Only when the host default also fails does the engine use its first registered
face (`TextEngine::default_font_for`); an engine with no registered face at all
raises. Only a resolution found through a candidate is cached under the
requested face string. The default-face fallback is loaded under a fixed name
and is never cached under the requested name, so a face registered later for
that name (`System.addFont`, `GdiPlus.addPrivateFont`) is found on the next
lookup instead of being shadowed by a stale fallback entry.

`Font.defaultFaceName`/`Font.getList` report the resolver's registered faces,
and `GdiPlus.getFontList` adds the private faces (`GdiPlus.addPrivateFont`).

---

## Pre-rendered `.tft` fonts

The visual `Font.mapPrerenderedFont` maps `.tft` pre-rendered bitmap fonts.
`crates/compat/src/resource.rs` reads the storage and validates the file header
at map time and defers the glyph body to first use (`PrerenderedFont::map` /
`ensure_body`). This is an internal optimisation — a game that maps many fonts
does not pay for glyph data it never draws — not a reference-parity claim: the
reference also validates only the header at this point
(`reference/cpp/core/visual/PrerenderedFont.cpp:9-76`). Loaded fonts are shared
by resolved placed path and released when the last mapping goes.

A layer-bound font measures width and height from the mapped `.tft` (the sum
of its per-character `Inc` metrics), while a standalone `new Font()` measures
directly through the shaping engine.

---

## How the requested face reaches the resolver

The game sets its text font through `layer.font`, e.g.
`system/MessageArea.tjs`'s `setFontStyle`:

```tjs
with(font){        // `font` is `this.font` (the Layer's font)
    .face = face;
    .height = size;
    ...
}
```

The L2 `Scene` owns one `FontState` per layer. `layer.font` returns a `Font`
object bound to the layer's state: every access returns the same wrapper, so
writes persist the way the reference's cached `FontObject` does.
`Layer.drawText` reads the layer's font id and resolves the face through the
resolver; when the layer has no font it uses the default face
(`DEFAULT_FONT_FACE`) rather than a per-call guess. `Layer.font = f` copies the
source `Font`'s properties into the layer's own state.

---

## Tests

* `crates/engine/src/text.rs` unit tests cover system-face registration, the
  exact-family query and its no-substitution rule, and the CJK fallback.
* `crates/compat/tests/font.rs` and `crates/compat/tests/add_font.rs` drive
  the real `Font`/`System.addFont` surface through the VM and cover face
  resolution, the `.tft` path and the fallback.
* `crates/platform/platform/src/fonts.rs` covers the host directory list and
  the Android constant.
