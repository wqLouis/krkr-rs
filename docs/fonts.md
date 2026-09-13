# Explicit font configuration (`fonts.json`)

krkr-rs performs **no implicit font selection of its own**. Which faces exist
and in what order they are tried is supplied by the application, not guessed
from the system font database. This document specifies the schema, the
startup precedence, and the exact resolution order.

When **no** configuration is found, krkr-rs keeps its original out-of-the-box
behavior: it discovers a Japanese-capable CJK face from the system
(`Noto Sans CJK JP` via `fontdb`, or a known Noto CJK path). This is the only
implicit path, and it exists so a game runs before the user has supplied a
`fonts.json`. Once a configuration is installed, krkr-rs never touches the
system font database unless `allow_system_discovery` is explicitly `true`.

## Schema

`fonts.json` is a JSON object with three optional keys:

```json
{
  "faces": {
    "MS Gothic": "fonts/msgothic.ttf",
    "MS 明朝": { "path": "fonts/msmincho.ttc", "index": 0 }
  },
  "fallback": [
    "fonts/noto-sans-jp.ttf",
    { "path": "fonts/noto-cjk.ttc", "index": 1 }
  ],
  "allow_system_discovery": false
}
```

| Key | Type | Meaning |
| --- | --- | --- |
| `faces` | object (name → entry) | Maps a requested `Font.face` name to a font file. Lookup is case-insensitive. |
| `fallback` | array of entries | Ordered list tried when the requested face is not mapped or fails to load. The **first entry that loads** wins. |
| `allow_system_discovery` | bool (default `false`) | Opt back into the `fontdb`/known-path system scan. |

A **font entry** is either:

* a string path — `"fonts/msgothic.ttf"` (collection index `0`), or
* an object — `{ "path": "fonts/msmincho.ttc", "index": 1 }`,
  where `index` defaults to `0`.

Both `faces` values and each `fallback` element accept either form.
Missing keys default to empty (`{}` / `[]`) and `false`.

A malformed file (bad JSON, or a value of the wrong shape) produces a clear
error that includes the file path and the JSON line/column.

## Startup precedence

The first source that yields a path wins:

1. CLI `--font-config <path>` (also works with `--headless`; the `run`
   subcommand accepts `--font-config <path>` and `--font-config=<path>`).
2. Environment variable `KRKR_RS_FONT_CONFIG`.
3. `<game-dir>/fonts.json` (auto-detected; only if the file exists).
4. Nothing.

If no configuration is found, the **no-config behavior** applies: system
discovery is used.

If a configuration *is* found but cannot be read or parsed, krkr-rs logs an
error and installs an **empty** configuration (empty `faces`/`fallback`,
`allow_system_discovery = false`). A user who asked for explicit selection
never silently gets implicit system fonts.

The configuration is installed in `game_startup` (via
`tvp_text::set_font_config`) **before** `startup.tjs` runs, so text layers
created during initialization see it immediately.

## Resolution order

`Layer.drawText` derives a request from the layer's tracked font face
(`layer.font.face`, a KAG `Font.face`). The request is one of:

* **`Path`** — the `KRKR_RS_SYSTEM_FONT` environment variable (used by
  hermetic CI). Always load exactly that file, never remapped by a config.
  This override outranks the layer's face.
* **`Named(name)`** — the layer's requested face name:
  1. Case-insensitive lookup in `faces`. Because KAG `Font.face` values are
     often comma-separated preference lists (`"A,B,C"`), the **whole string**
     is tried first, then each trimmed token in order. Every token must be an
     explicit `faces` key; there is no implicit per-token system lookup.
  2. If a mapped entry fails to load, and no candidate matches, walk
     `fallback` in order and use the first entry that loads.
  3. Only if `allow_system_discovery` is `true`, fall back to
     `FontFace::discover_system_jp`.
  4. Otherwise `None` (the renderer then uses its deterministic bitmap-font
     fallback).
* **`SystemJp`** — no face was tracked at all:
  1. `fallback` chain (first entry that loads),
  2. `allow_system_discovery` → `discover_system_jp`,
  3. otherwise `None`.

With **no configuration installed**, both `Named` and `SystemJp` behave like
the pre-config port: system discovery.

Resolved faces are cached per request (including negative results), and the
cache is invalidated whenever `set_font_config` is called. Glyph atlases are
cached per `(FontFace::id, pixel_height)` as before, so a named face and a
path override never alias each other's atlases.

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

The native `Layer.font` getter returns a disposable `Font` object **bound to a
single layer-owned `FontState`** (`Font.__bind`). Every access returns a new
wrapper but writes go to the same state, so the mutation persists the way the
reference's cached `FontObject` does. `Layer.drawText` reads
`LayerState.font_id` → `FontState.face` and issues `FaceRequest::Named(face)`.

`Layer.font = f` copies the source `Font`'s properties into the layer's own
state. If a script draws without ever touching `layer.font`, `drawText` falls
back to the newest registered `Font` in the scene (`scene.fonts.last()`) and
then to `FaceRequest::SystemJp`; this last-resort path is the only remaining
case without a per-layer face and is documented here as a limitation.

## Tests

`crates/tvp-text/tests/font_config.rs` covers:

* JSON parsing of string and `{ path, index }` entries, the default index, and
  malformed-JSON errors;
* case-insensitive `faces` mapping and comma-separated token matching;
* `fallback` order (a missing first entry is skipped);
* `allow_system_discovery` on/off, and the no-config discovery behavior;
* cache invalidation when the configuration changes.

`crates/tvp-visual` unit tests cover the layer font tracking (shared state
across accesses, the `Layer.font` setter) and that a `drawText` with a
configured named face rasterizes through that face.
