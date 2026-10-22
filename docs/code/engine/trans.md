# `engine::trans` — the transition state machine

Source: `crates/engine/src/trans/mod.rs` and the `extrans_*` submodules

The module models the TVP `Trans` machine (cross-fade, universal, mask,
scroll, and the seven `extrans.dll` providers) as explicit L2 state over the
[`layer_ops`](layer_ops.md) primitives. L3 exposes the script `Trans` object.

A `Trans` is a **caller-owned value**: it stores the transition kind, the
duration and the elapsed time, and it composites two source bitmaps into a
destination at the current progress. There is no global handler registry, no
plugin lookup and no clock. The application calls `Trans::step` with the
elapsed milliseconds each frame and `Trans::apply` once per frame to render.

---

## 1. Responsibility

* Own the transition state machine: kind, duration, elapsed time, phase,
  rule mask and completion.
* Provide the scroll vocabulary (`ScrollFrom`, `ScrollStay`).
* Composite the two sources into the destination at the current phase.
* Validate the transition's parameters at construction time.

---

## 2. State it owns

```rust
pub struct Trans {
    kind: TransKind,
    duration_ms: f64,
    elapsed_ms: f64,
    phase_max: i32,
    phase: i32,
    rule: Option<RuleMask>,
    completed: bool,
}
```

A `RuleMask` holds the decoded grayscale rule image (`width`, `height` and
the byte plane). The rule is **tiled** over the source, exactly like the
reference's image provider tiles a smaller rule graphic.

---

## 3. Public API

```rust
pub const PHASE_MAX: i32 = 255;

pub enum TransKind {
    CrossFade,
    Universal { vague: i32 },
    Mask,
    Scroll { from: ScrollFrom, stay: ScrollStay },
    Wave { max_height: i32, max_omega: f64, background1: i64, background2: i64, wave_type: i32 },
    Mosaic { max_size: i32 },
    Turn { background: i64 },
    RotateZoom { factor: f64, accel: f64, twist: f64, twist_accel: f64, center: Option<(i32, i32)> },
    RotateVanish { accel: f64, twist: f64, twist_accel: f64, center: Option<(i32, i32)> },
    RotateSwap { background: i64, twist: f64, center: Option<(i32, i32)> },
    Ripple { center: Option<(i32, i32)>, map_width: i32, roundness: f32, speed: f32, max_drift: i32 },
}

pub enum ScrollFrom { Left, Top, Right, Bottom }
pub enum ScrollStay { NoStay, StayDest, StaySrc }

impl RuleMask {
    pub fn new(width: u32, height: u32, gray: Vec<u8>) -> Result<Self, TransError>;
    pub fn from_rgba(width: u32, height: u32, rgba: &[u8]) -> Result<Self, TransError>;
    pub fn width(&self) -> u32;
    pub fn height(&self) -> u32;
    pub fn gray(&self) -> &[u8];
    pub fn at(&self, x: i32, y: i32) -> u8;
}

impl Trans {
    pub fn new(kind: TransKind, duration_ms: f64) -> Result<Self, TransError>;
    pub fn with_rule(kind: TransKind, duration_ms: f64, rule: Option<RuleMask>) -> Result<Self, TransError>;
    pub fn cross_fade(duration_ms: f64) -> Result<Self, TransError>;
    pub fn universal(duration_ms: f64, vague: i32, rule: RuleMask) -> Result<Self, TransError>;
    pub fn mask(duration_ms: f64, rule: RuleMask) -> Result<Self, TransError>;
    pub fn scroll(duration_ms: f64, from: ScrollFrom, stay: ScrollStay) -> Result<Self, TransError>;

    pub fn kind(&self) -> TransKind;
    pub fn duration_ms(&self) -> f64;
    pub fn elapsed_ms(&self) -> f64;
    pub fn progress(&self) -> f32;
    pub fn phase(&self) -> i32;
    pub fn phase_max(&self) -> i32;
    pub fn is_complete(&self) -> bool;
    pub fn rule(&self) -> Option<&RuleMask>;

    pub fn step(&mut self, dt_ms: f64) -> TransStep;
    pub fn seek_ms(&mut self, elapsed_ms: f64) -> TransStep;
    pub fn complete(&mut self);
    pub fn reset(&mut self);
    pub fn apply(&self, dst: &mut BitmapState, src1: &BitmapState, src2: &BitmapState,
                 region: RectI) -> Result<(), TransError>;
    pub fn full_region(bitmap: &BitmapState) -> RectI;
    pub fn transition_frame(&self, src1: u32, src2: u32, size: (u32, u32),
                            covers_layer: bool) -> Option<TransitionFrame>;
}
```

`TransStep` carries the normalized `progress`, the integer `phase` and the
`completed` flag.

---

## 4. The kinds

| kind | reference provider | options |
|---|---|---|
| `CrossFade` | `crossfade` | a linear blend from source 1 to source 2 |
| `Universal` | `universal` | `vague`, rule image |
| `Mask` | `universal` with `vague = 0` | rule image |
| `Scroll` | `scroll` | `from`, `stay` |
| `Wave` | `extrans` (`wave.cpp`) | `maxh`, `maxomega`, `bgcolor1`, `bgcolor2`, `wavetype` |
| `Mosaic` | `extrans` (`mosaic.cpp`) | `maxsize` |
| `Turn` | `extrans` (`turn.cpp`) | `bgcolor` |
| `RotateZoom` | `extrans` (`rotatetrans.cpp`) | `factor`, `accel`, `twist`, `twistaccel`, `center` |
| `RotateVanish` | `extrans` (`rotatetrans.cpp`) | `accel`, `twist`, `twistaccel`, `center` |
| `RotateSwap` | `extrans` (`rotatetrans.cpp`) | `bgcolor`, `twist`, `center` |
| `Ripple` | `extrans` (`ripple.cpp`) | `center`, `rwidth`, `roundness`, `speed`, `maxdrift` |

All of them are `ttExchange` (two sources).

`CrossFade` and `Scroll` are the two **composition** kinds: their frame is the
two sources placed in one rectangle (a per-pixel fade of the second source, or
two disjoint placements), so a host that draws the projected scene never
rasterizes it. `Universal`, `Mask` and the seven `extrans` kinds need a
per-pixel rule or coordinate warp and are always blended by `Trans::apply`.

---

## 5. Invariants

* **The phase runs from 0 to `phase_max`.** The reference advances a `phase`
  from `0` to `PhaseMax` (255); the model keeps both the normalized
  `progress` and the integer `phase`, so the ports of the reference blend
  tables stay exact.
* **`phase = elapsed × phase_max / duration`, truncated** (not rounded), and
  clamped to `0..=phase_max`.
* **A completed transition stays complete.** `step` and `seek_ms` leave the
  state pinned at `progress = 1.0` and `phase = phase_max`.
* **A non-finite or non-positive `dt_ms` leaves the state unchanged**; a
  non-finite or negative `elapsed_ms` does the same for `seek_ms`.
* **The endpoints are exact**: at `phase == 0` the destination is a copy of
  source 1, at `phase == phase_max` a copy of source 2. Every `ttExchange`
  provider is `tutDivisible`, so the reference short-circuits the endpoints;
  `scroll` is positional and does its own clipping, so it stays outside that
  shortcut.
* **A rule is required by `Universal` and `Mask`, and rejected by the other
  kinds** — a rule supplied to a kind that does not use one is an error, not
  a silently ignored argument.
* **A composited frame is the same two sources the blend reads.**
  `transition_frame` returns the frame for `CrossFade` and `Scroll` only, with
  the endpoints as one source at full opacity (the reference's endpoint
  short-circuit) and the fade source carrying `phase / 256` — the weight
  `blend_level` applies. Every other kind returns `None`; a zero-sized frame
  returns `None` too.
* **The rule mask is tiled** over the source; `RuleMask::at` does the tiling.
* **`phase_max` is `PHASE_MAX + vague` for `Universal`**, so the soft edge
  has room; every other kind uses `PHASE_MAX`.
* **The `extrans` providers keep the reference's integer arithmetic.** The
  integer operations of the C code (the halved time, the truncated
  amplitudes and ratios) stay integer operations, while the `double` maths
  stays `f64`; the sine arguments use the truncated π literal so the table
  entries keep their exact values.

---

## 6. Data flow

```
L3 native (the script `Trans` object)
   │  a kind + a duration (+ a rule image)
   ▼
Trans::new / Trans::with_rule ──► TransError on an invalid parameter
   │
   ▼  every frame
Trans::step(dt_ms) ──► TransStep { progress, phase, completed }
   │
   ▼
two hosts, one frame:
   ├─ Trans::apply(dst, src1, src2, region)
   │     → the destination bitmap (VM pixels)
   │     → the layer's transition slot (cache_bitmap / transition_self)
   │     → the projection draws that slot in place of the layer's image/subtree
   └─ Trans::transition_frame(src1, src2, size, covers_layer)
         → LayerState::transition_frame (a TransitionFrame)
         → the projection draws the frame's two sources in its place
```

The second form exists for a host that draws the projected scene: the frame is
composited by the pipeline from the two source textures, so the transition
writes no pixels and uploads no bitmap per tick. The headless CPU path keeps
`Trans::apply` and the display bitmap.

### 6.1 The composited frame's tolerance

The composited frame draws source 1 and then source 2 over it at the phase
weight, while `Trans::apply` writes the reference's per-channel lerp of the two
straight-alpha sources. Where source 2 is opaque the two agree within one
8-bit level per channel; where it carries alpha `a`, the frame's contribution
fades by `phase × a` instead of `phase`, so a channel may differ by up to
`255 × t × (1 − a)` (with `t = phase / 256`). A translucent *first* source is
additionally composited over what lies beneath the layer, where the CPU blend
writes straight-alpha pixels. The transition display is not a script-observable
value (scripts read `MainImage`, never the blend result), so this is the D34
tolerance for non-script-visible composition; `Scroll` has no blend and
matches the CPU blit exactly.

---

## 7. Failure modes

| failure | behaviour |
|---|---|
| a non-positive or non-finite duration | `TransError::InvalidDuration` |
| a negative `vague` | `TransError::InvalidVague` |
| a rule-based kind without a rule | `TransError::MissingRule` |
| a rule supplied to a kind that does not use one | `TransError::UnexpectedRule` |
| a rule whose buffer length disagrees with its dimensions | `TransError::InconsistentRule` |
| the two sources have different dimensions | `TransError::SizeMismatch` |
| an `extrans` option outside the range the reference accepts | `TransError::InvalidOption` |

Nothing is silently clamped to a fallback: every rejected parameter is a
typed error, and L3 turns it into the script error text.

---

## 8. Current limits

* **Only `crossfade` and `scroll` are composited.** `universal`, `mask` and the
  seven `extrans` kinds return no frame: their per-pixel rule or coordinate
  warp would need a fragment shader, so `Trans::apply` blends them on every
  host, GPU included. That is a renderer limitation, not a per-run fallback.
  The generic shader loading that would replace it is deferred work
  (`TODO.md` §16); the registry that will carry it is documented in
  [shader.md](shader.md).
* **The non-self-updating transition pumps still run inside the L3 hook**,
  not inside L2. The self-updating ones advance from the frame hook's
  `after_project`, which sees the projection's draw report.
* **The CPU blend runs a row at a time.** `Trans::apply` blends the
  cross-fade and the rule blend row by row: the row slices of the two
  sources and of the destination are taken once per row, and the per-channel
  arithmetic of `blend_level` is applied to the whole row. The rule's
  grayscale value is read from the rule's row, so a row costs one rule
  lookup per pixel instead of two divisions. A source that does not cover
  the destination's columns falls back to the per-pixel read, which reads
  the missing columns as transparent.

---

## 9. Tests

* `crates/engine/tests/trans.rs` — the state machine, the kinds, the rule
  mask and the errors.
* `crates/engine/tests/trans_blend.rs` — the blend the CPU path writes: the
  cross-fade and the rule blend against the per-channel arithmetic of the
  reference, and a timing measurement of the 1920x1080 cross-fade.
* `crates/engine/tests/extrans_wave.rs`, `extrans_mosaic.rs`,
  `extrans_turn.rs`, `extrans_rotate.rs`, `extrans_ripple.rs` — one file per
  `extrans` provider.

---

## 10. See also

| topic | document |
|---|---|
| the L3 ↔ L2 protocol for this module | [`rules/api_rules.md`](../../rules/api_rules.md) §4 |
| the transition model in the architecture | [`arch/l2.md`](../../arch/l2.md) §8 |
| the pixel primitives the transitions build on | [layer_ops.md](layer_ops.md) |
| the layer slots the transitions fill | [scene.md](scene.md) |
| the frame that advances the transitions (still in the hook) | [engine.md](engine.md) |
| the archived protocol trail | [`docs.old/migration/14-l3-l2-protocol.md`](../../../docs.old/migration/14-l3-l2-protocol.md) §6.8 |
