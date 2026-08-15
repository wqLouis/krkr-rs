# Wave 3 — Bevy render module (design)

Goal: the real game's `startup.tjs` runs to completion, the Logo/Title scenes
construct, and **the title screen renders** (bitmaps + fills; text rendering
is milestone 3B). Reference module: `reference/cpp/core/visual/` (64K lines —
we port the *minimal surface* the game's init path uses, not all of it).

## Real-game requirements (extracted from the game scripts)

- `k2compat/k2compat.tjs(260)`: `hookInjection(Layer, "Layer", ...)` — `Layer`
  must exist as a native class with the methods k2compat wraps.
- `system/window.tjs`: `class MainWindow extends Window` — `super.Window()`,
  `setInnerSize`, `addInputNotify`, `win.primaryLayer`, `bringToFront`.
- `system/activatelayer.tjs`: `class ActivateLayer extends Layer` +
  `super.Layer(win, par)`; `Logo` uses `setSize`, `fillRect`, `absolute`,
  `hitThreshold`, `width`/`height`.
- `system/sprite.tjs`: `class Sprite extends AffineLayer` — affine natives on
  Layer (transition/envelope methods can be no-op stubs initially).
- `title.tjs`/`logo`: `new Bitmap("FRM_0501b")` etc. — webp decode from xp3.
- `gamescenemanager.tjs`: `new Timer(onWaitSceneChange, "")` — **Timer native
  + event loop** (this is the architectural core of the milestone).
- `begin.tjs`: `win.bringToFront(); new SceneManager(win); game.changeScene(SCENE_LOGO);`

## Crate layout

```
crates/tvp-visual/   natives + logical scene (NO Bevy dependency)
  src/scene.rs       Scene model (below) — the shared contract
  src/bitmap.rs      decode webp/png/jpg/bmp -> RGBA8 (image crate) from Storage
  src/natives/       window.rs, layer.rs, bitmap.rs, font.rs, timer.rs
crates/render/       Bevy app (depends on tvp-visual, engine, tjs2-sys, all tvp-*)
  src/main.rs        App: plugins, Startup (prepare+register+run startup.tjs), Update
  src/sync.rs        Scene -> Bevy sprites/textures/transforms
```

## Threading model

All VM + native execution happens on Bevy's main thread (the tjs2 VM is
single-threaded). `Scene` is `Arc<RwLock<Scene>>`: natives mutate it under
write lock; render systems read it under read lock. A `VmEventQueue`
(`Mutex<VecDeque<VmEvent>>`) carries timer firings from natives to the Bevy
update system, which drains it and calls the TJS callbacks.

## Scene model (shared contract — do not change without updating all users)

```rust
pub struct Scene {
    pub windows: Vec<WindowState>,
    pub layers: Vec<LayerState>,
    pub bitmaps: Vec<BitmapState>,
    pub fonts: Vec<FontState>,
    next_window: u32, next_layer: u32, next_bitmap: u32, next_font: u32,
}

pub struct WindowState {
    pub id: u32,
    pub title: String,
    pub inner_size: (u32, u32),          // game logical size
    pub visible: bool,
    pub opacity: f32,                     // 0..1
    pub primary_layer: Option<u32>,       // layer id
    pub layers: Vec<u32>,                 // render order (back -> front)
    pub input_notify: Vec<InputNotify>,   // script callbacks (stub for now)
}

pub struct LayerState {
    pub id: u32,
    pub window: u32,
    pub parent: Option<u32>,              // layer id (null = attached to window)
    pub children: Vec<u32>,
    pub bitmap: Option<u32>,              // bitmap id
    pub rect: Rect,                       // (x, y, w, h) in window coords
    pub visible: bool,
    pub opacity: f32,
    pub z_order: i32,                     // for parent-less layers
    pub fill_color: Option<[u8; 4]>,      // RGBA solid fill (when no bitmap)
    pub hit_threshold: i32,
    pub is_primary: bool,
}

pub struct BitmapState {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,                    // RGBA8, straight alpha
    pub name: Option<String>,             // storage name it was loaded from
    pub dirty: bool,                      // needs GPU re-upload
}

pub struct FontState {
    pub id: u32,
    pub face: String,
    pub height: i32,
    pub color: [u8; 4],
    pub bold: bool,
    pub italic: bool,
}

pub enum VmEvent { TimerFire { timer_id: u32 } }
```

Scene ops (methods on `Scene`): `add_window`, `add_layer(window, parent)`,
`remove_layer`, `add_bitmap_from_storage(&mut self, storage, name) -> Result<u32>`,
`add_blank_bitmap(w, h)`, `layer_set_bitmap`, `layer_fill_rect`, `layer_set_rect`,
`layer_set_visible`, `layer_set_opacity`, `layer_move_to_front`, `layer_z_reorder`,
`font_add`, plus accessors. All id-based; invalid ids are no-ops.

## FFI extension needed (tjs2-sys, SA-1)

Timer callbacks are TJS function objects; the VM must call them from the
Bevy update loop:

```c
// retain a TJS value (function/object) -> opaque handle id
tjs2_value_id tjs2_retain_value(void* engine, const tjs2_value* v);
void tjs2_release_value(void* engine, tjs2_value_id id);
// invoke the retained value's FuncCall with args (no this)
int tjs2_call_value(void* engine, tjs2_value_id id, int argc, const tjs2_value* argv,
                    tjs2_value* out, char** out_error);
```

C++ side: retain stores the `tTJSVariant` (AddRef) in a per-engine map keyed
by id; call does `variant.AsObjectClosureNoAddRef().FuncCall(...)` (or
`variant.AsObjectNoAddRef()->FuncCall(...)` — check tjs.h for the closure
API). Rust side mirrors it; add `Tjs2Engine::retain_value/call_value`.

## SA-5 render crate (Bevy 0.19)

- `App::new().add_plugins(DefaultPlugins)`; window title "krkr-rs", size from
  `SystemContext.screen_size` (game default 1280x720).
- Startup system: `engine::prepare(game_dir)` → register ALL natives (tvp-* +
  tvp-visual) → set contexts → `run_startup` → log result. Then the Update
  loop takes over.
- Update system A (`run_vm`): drain `VmEvent` queue; fire timers via
  `tjs2_call_value`. (Guard: re-entrant natives that mutate Scene are fine —
  they run on the same thread.)
- Update system B (`sync_scene`): for each window → camera (2D); for each
  layer in z order → SpriteBundle: texture from `bitmaps[id]` (upload on
  `dirty` via `Assets<Image>`), or solid-color sprite for `fill_color`;
  transform from `rect` (y-down → flip); visibility from `visible`/`opacity`
  (Sprite::color alpha). No Bevy ECS state in natives — everything flows
  through `Scene`.
- Exit when the primary window closes.

## Milestone boundaries

- 3A (this wave): window renders; Logo/Title bitmaps + fills visible; timer
  loop drives scene changes. Text rendering (fonts → glyph atlas, KAG text
  layers) is 3B. ADV scenario/KAGParser is 3C.
- Native stubs OK where the reference method needs audio/movie/input beyond
  this milestone — document each stub.
