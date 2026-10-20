# `app::overlay` — the debug panel

Source: `crates/app/src/overlay.rs`

The debug overlay draws the frame rate and its rolling 1% low, the process
CPU usage, the storage I/O rate, the resident memory and the live object
counts in the top-left corner.

It is **not** part of the engine `Scene`: it owns no `Bitmap` a script can
reach and no layer a script can address. It builds a small RGBA panel in
memory, uploads it as a texture and contributes one render node to the
projected tree, so enabling it cannot change what a script observes. The
overlay is off by default and is enabled by `HostConfig::debug_overlay` or
the `--debug-overlay` launch flag.

---

## 1. The panel

| property | value |
|---|---|
| size | 400 × 104 px |
| background | translucent black, RGBA `(0, 0, 0, 176)` |
| text | opaque white, a built-in 5 × 7 bitmap font at scale 2 |
| position | the top-left of the framebuffer, at the panel's own pixel size |
| sort key | `i32::MAX`, so the panel sits above every projected layer |

The lines are:

```
FPS 60.0 1% 45.2
CPU 12%
IO R 1.5M W 0
MEM 256M
OBJ W1 L23 B51 F3 T51
```

A counter the host cannot supply is shown as `?` — never as a fabricated
zero — and the `MEM` line is omitted entirely when the resident size is
unknown.

The **1% low** is the average of the slowest hundredth of the recent frame
times (10 000 are kept), inverted to frames per second. The **CPU
percentage** is the delta of the cumulative process CPU runtime over the
refresh interval, as a percentage of one fully used core: one saturated
core is `100`, two are `200`. The **I/O rates** are the deltas of the
cumulative read and write bytes over the same interval.

---

## 2. The refresh

The engine calls the overlay once per frame through the L2 `EngineOverlay`
implementation, but the panel is re-uploaded only when the refresh interval
(250 ms) has elapsed on the engine's frame clock. The frame counter is
accumulated between refreshes and averaged over the interval, and the host
counters are sampled at the refresh.

The panel's pixels are composed into a `Vec<u8>` and uploaded with
`upload_texture`; the render node is then handed to the engine, which sets
its parent and adds it to the render tree.

The font is a built-in bitmap font that covers exactly the characters the
formatter can emit — the digits, the decimal point, `%`, `?`, the space and
the letters in the line labels. A character outside that set is a typed
error (`OverlayError::MissingGlyph`), not a blank glyph.

---

## 3. Errors

| error | raised when |
|---|---|
| `OverlayError::MissingGlyph(char)` | the panel text holds a character the built-in font does not cover |
| `OverlayError::Render` | the texture could not be created or uploaded |

Both surface as `HostError::Overlay` at the host boundary.

---

## 4. Tests

The unit tests cover the formatting (all five lines, the `?` placeholders,
the byte-rate suffixes), the CPU percentage (a fraction of one core, and
`None` for a missing counter, a counter that went backwards or a
non-positive interval), the 1% low (the slowest hundredth of 100 and of 200
frame times), the glyph coverage of the formatter's alphabet, the
missing-glyph error, and that drawing a glyph marks pixels inside the panel.

The integration tests in `crates/app/tests/debug_overlay.rs` drive the
public host surface: the overlay contributes exactly one texture node when
it is enabled and none when it is not, a frame with an unchanged scene
revision reuses that node instead of appending another, and the panel's
texture is uploaded before it is drawn.

---

## 5. Related documents

| topic | document |
|---|---|
| the crate overview and the wiring | [README.md](README.md) |
| the CLI flag that enables it | [main.md](main.md) |
| the L2 overlay seam and the frame | [`../engine/engine.md`](../engine/engine.md), [`../rules/arch_rules.md`](../../rules/arch_rules.md) §3 |
| the L1 render pipeline | [`../arch/l1.md`](../../arch/l1.md) §3 |
