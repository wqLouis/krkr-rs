//! Visual native classes: `Window`, `Layer`, `Bitmap`, `Font`, `Timer`.
//!
//! Behavioral reference: `main`'s `crates/tvp-visual/src/natives/*` (window.rs,
//! layer.rs, bitmap.rs, font.rs, timer.rs) and the old `scene.rs`. This is a
//! reimplementation on the layered API: every class composes the L2
//! [`engine::Scene`] attached through [`CompatServices::scene`] and keeps no
//! engine state of its own. The old code hid that state in process globals;
//! here the per-family **dispatch** state (the id → script-object registries
//! and the Timer registry) lives in [`CompatServices::state`] (doc 08 §3.1),
//! and the per-object state lives in the native instance payload.
//!
//! # Architecture
//!
//! The native class is constructed by `new Window()` / `new Layer(...)` /
//! ...; the **class-name method** (`Window`, `Layer`, ...) is the constructor
//! hook and returns the new scene id (the reference wrappers use it). The
//! instance payload holds the scene id; all persistent state is read and
//! written through the L2 [`Scene`] API, so the model is never duplicated.
//!
//! Cross-object properties (`Window.primaryLayer`, `Layer.parent`,
//! `Layer.window`, `Layer.children`, ...) return real TJS objects. The mapping
//! scene id → retained script object is the [`ObjectRegistry`] in
//! [`VisualState`]; each class registers its `objthis` at construction and
//! removes it on destroy.
//!
//! # Implemented
//!
//! * `Window`: constructor, `close`, `bringToFront`, `update`, `add`,
//!   `remove`, `setSize`, `setInnerSize`, `setPos`, `setMinSize`,
//!   `setMaxSize`, `setZoom`, `showModal`, `setMaskRegion`,
//!   `removeMaskRegion`, `hideMouseCursor`, `changeScreenMode`,
//!   `postInputEvent`; properties `id`, `visible`, `caption`, `width`,
//!   `height`, `innerWidth`, `innerHeight`, `minWidth`, `minHeight`,
//!   `maxWidth`, `maxHeight`, `left`, `top`, `focusable`, `zoomNumer`,
//!   `zoomDenom`, `fullScreen`, `mainWindow`, `focusedLayer`, `primaryLayer`,
//!   `waitVSync`, `enableTouch`, `hintDelay`, `mouseCursorState`, `imeMode`,
//!   `useMouseKey`, `trapKey`.
//! * `Layer`: constructor, `setPos`, `setSize`, `setSizeToImageSize`,
//!   `setImagePos`, `setImageSize`, `setBitmap`, `setImage`, `assignImages`,
//!   `copyFromBitmapToMainImage`, `independMainImage`, `bringToFront`,
//!   `moveToFront`, `setParentId`, `getLayerAt`, `focus`, `update`, `setClip`,
//!   `getProvincePixel`, `setProvincePixel`, `fillRect`; the full property
//!   set used by KAG (`visible`, `opacity`, `width`, `height`, `left`, `top`,
//!   `type`, `hasImage`, the buffer/pitch properties, `absolute`/`order`,
//!   hit/attention/clip/focus properties, `window`, `parent`, `children`,
//!   `font`, ...).
//! * `Bitmap`: constructor, `getPixel`, `setPixel`, `getMaskPixel`,
//!   `setMaskPixel`, `independ`, `setSize`, `copyFrom`; properties `id`,
//!   `nativeId`, `width`, `height`, `buffer`, `bufferForWrite`, `bufferPitch`,
//!   `loading`.
//! * `Font`: constructor, `__bind`; properties `id`, `face`, `height`,
//!   `color`, `bold`, `italic`, `strikeout`, `underline`, `angle`,
//!   `faceIsFileName`.
//! * `Timer`: constructor, `onTimer`; properties `id`, `interval`, `enabled`,
//!   `capacity`, `count`, `mode`; [`timer_poll`] fires due timers through the
//!   object's `onTimer` member (so a script subclass override runs).
//!
//! # Deliberately deferred
//!
//! These members need a subsystem that is not exposed through the L2/L3
//! service set yet. They are **not registered** (calling them raises a TJS
//! "member not found", never a silent stub):
//!
//! * `Bitmap.load` / `Bitmap.loadAsync` / `Bitmap.loadHeader`, `Layer.loadImages`,
//!   `Layer.saveLayerImage`, `Bitmap.save` — no image decoder/encoder is
//!   exposed through L2. L1 owns those; when a decode service lands these
//!   become thin wrappers.
//! * `Font.getTextWidth` / `getTextHeight` / `getEsc*` / `getGlyphDrawRect`
//!   and `Layer.drawText` — no font rasterizer/metrics service is exposed
//!   through L2 (L1 `render`), so text measurement cannot be made truthful.
//! * `Layer` drawing primitives (`drawLine`, `drawPolygon`, `copyRect`,
//!   `stretchCopy`, `affineCopy`, `doBoxBlur`, `colorize`, `noise`, the
//!   `operate*`/`piled*`/`blend*` families, `copy9Patch`, ...) — L2
//!   [`engine::layer_ops`]/[`engine::raster`] already implement the math, but
//!   wiring the ~40 methods here is a mechanical follow-up rather than a
//!   missing primitive; only `fillRect` is wired now.
//! * `Window`/`Layer` input handlers (`onClick`, `onKeyDown`, ...) are not
//!   registered: the L1→L3 event adapter (`compat::EventAdapter`) is not yet
//!   wired to the scene, so there is nothing truthful to dispatch.
//! * `Window.getMouseVelocity`/`getTouchVelocity`/`getTouchPoint`/
//!   `touchPointCount` and `Layer.cursorX`/`cursorY` — need the L1 pointer
//!   state, which is not part of the L2 service set.
//!
//! # Timer polling
//!
//! [`timer_poll`] is the `timer_poll` step of the documented frame body
//! (`compat::frame::FrameBody`); the host calls it once per frame. It is a
//! `pub` free function (like `system::continuous_handler_poll`) so the host
//! can wire it without the natives exposing anything else.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};
use std::time::Instant;

use engine::{FontState, LayerState, Rect, Scene};
use tjs::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef,
    RetainedValue, Tjs2Engine, TjsValue, VAL_INTEGER, VAL_OBJECT, VAL_REAL, VAL_RETAINED, VAL_VOID,
    Value,
};

use crate::native_util::{args, report_error, set_bool, set_int, set_void, value_as_string};
use crate::natives::system::{resolve, set_string_out, value_as_bool, value_as_i64};
use crate::{CompatCtx, CompatError};

/// Default font face used when a `Font` is created without a face (the
/// reference `TVPDefaultFontFaceName`).
const DEFAULT_FONT_FACE: &str = "MS Gothic";
/// Default font height (`TVPDefaultFontHeight`).
const DEFAULT_FONT_HEIGHT: i32 = 16;
/// Default font color (opaque white, ARGB).
const DEFAULT_FONT_COLOR: i64 = 0xffff_ffff;

// ---------------------------------------------------------------------------
// Per-family dispatch state (never a process global)
// ---------------------------------------------------------------------------

/// Scene id → the retained script object (`objthis`) that wraps it. This is
/// the one cross-object mapping the visual natives need: it lets
/// `Window.primaryLayer`, `Layer.parent`, `Layer.window`, `Layer.children`
/// and `Layer.getLayerAt` hand back the *real* object a script created.
#[derive(Default)]
struct ObjectRegistry {
    windows: HashMap<u32, DetachedValue>,
    layers: HashMap<u32, DetachedValue>,
    bitmaps: HashMap<u32, DetachedValue>,
    fonts: HashMap<u32, DetachedValue>,
}

/// One registered `Timer` (reference `tTJSNI_BaseTimer`).
struct TimerEntry {
    /// The constructor callback (a plain `Timer`'s `onTimer` invokes it).
    /// `None` while the callback is running (taken out so the state borrow is
    /// released before re-entering the VM).
    callback: Option<DetachedValue>,
    /// The timer object itself, for `onTimer` dispatch through its class
    /// chain (a script subclass override wins).
    owner: Option<DetachedValue>,
    /// Interval in milliseconds (`0` is clamped to `1`).
    interval_ms: u64,
    /// Whether the timer fires.
    enabled: bool,
    /// Monotonic-clock time of the next fire.
    next_fire_ms: u64,
    /// How many times the timer has fired.
    count: u64,
    /// Reference `Capacity` (stored; one callback is delivered per interval).
    capacity: i64,
    /// Event delivery mode (`tTVPAsyncTriggerMode`).
    mode: i64,
}

/// `VisualState` — the visual family's dispatch state, keyed by type inside
/// [`CompatServices::state`](crate::CompatServices::state).
#[derive(Default)]
struct VisualState {
    objects: ObjectRegistry,
    timers: HashMap<u32, TimerEntry>,
    next_timer: u32,
    /// Monotonic epoch for [`Self::now_ms`].
    epoch: Option<Instant>,
    /// The most recent clock value seen by [`timer_poll`]; an enable/interval
    /// change reschedules from here (the reference uses `TVPGetTickCount()`).
    last_now_ms: u64,
    /// `Window.bringToFront` requests for the host to drain.
    bring_to_front: Vec<u32>,
    /// `Window.update` requests `(window_id, update_type)`.
    update_requests: Vec<(u32, i32)>,
    /// Reusable `out.string` scratch. The C++ trampoline copies the string
    /// before the callback returns, so the buffer only has to outlive the
    /// call. (System owns a shared `StringScratch`; the visual property
    /// setters need their own so a re-entrant visual callback cannot clobber
    /// a System result mid-marshal.)
    scratch: RefCell<Vec<u8>>,
}

impl VisualState {
    /// Milliseconds since the first call (the visual family's monotonic
    /// clock, shared by [`timer_poll`]).
    fn now_ms(&mut self) -> u64 {
        let epoch = self.epoch.get_or_insert_with(Instant::now);
        epoch.elapsed().as_millis() as u64
    }
}

// ---------------------------------------------------------------------------
// Native instance payloads
// ---------------------------------------------------------------------------

/// `Window` payload: the scene id plus the window properties the L2
/// [`engine::WindowState`] does not model (there is no OS chrome).
struct WindowInst {
    id: Option<u32>,
    left: i32,
    top: i32,
    min_width: i32,
    min_height: i32,
    max_width: i32,
    max_height: i32,
    focusable: bool,
    zoom_numer: i32,
    zoom_denom: i32,
    full_screen: bool,
    wait_vsync: bool,
    enable_touch: bool,
    hint_delay: i32,
    mouse_cursor_state: i32,
    ime_mode: i32,
    use_mouse_key: bool,
    trap_key: bool,
    modal: bool,
    mask_region: Option<i32>,
}

impl Default for WindowInst {
    fn default() -> Self {
        Self {
            id: None,
            left: 0,
            top: 0,
            min_width: 0,
            min_height: 0,
            max_width: 0,
            max_height: 0,
            // Reference `iWindowLayer::GetFocusable` default is true.
            focusable: true,
            zoom_numer: 1,
            zoom_denom: 1,
            full_screen: false,
            wait_vsync: false,
            enable_touch: false,
            hint_delay: 500,
            mouse_cursor_state: 0,
            ime_mode: 0,
            use_mouse_key: false,
            trap_key: false,
            modal: false,
            mask_region: None,
        }
    }
}

/// `Layer` payload: the scene id and the lazily-created `Font` object (a
/// retained script object, so repeated `layer.font` reads return the same
/// wrapper).
#[derive(Default)]
struct LayerInst {
    id: Option<u32>,
    font_object: Option<DetachedValue>,
}

/// `Bitmap` payload: the scene bitmap id plus the async-load flag.
#[derive(Default)]
struct BitmapInst {
    id: Option<u32>,
    loading: bool,
}

/// `Font` payload: the scene font id. `owns_state` is true while this object
/// created the font itself (`new Font(...)`) and is false after `__bind`
/// repoints it at a layer-owned font.
#[derive(Default)]
struct FontInst {
    id: Option<u32>,
    owns_state: bool,
}

/// `Timer` payload: the timer registry id.
#[derive(Default)]
struct TimerInst {
    id: Option<u32>,
}

// ---------------------------------------------------------------------------
// Scenes, binding and object marshalling
// ---------------------------------------------------------------------------

/// Resolve the bound engine for a callback and return its dispatch context.
fn bound(
    engine: *mut c_void,
) -> Result<(crate::natives::system::EngineBinding, CompatCtx), String> {
    let resolved =
        resolve(engine).ok_or_else(|| "visual: compat context is not bound".to_string())?;
    let ctx = resolved.ctx();
    Ok((resolved, ctx))
}

/// The attached L2 scene, mutably.
fn scene_mut<'a>(ctx: &CompatCtx) -> Result<&'a mut Scene, String> {
    // SAFETY: the host keeps the attached scene alive for the binding's
    // lifetime and the VM is single-threaded (the documented `attach_scene`
    // contract), so forming this reference is sound within the callback.
    let ptr = unsafe { ctx.services() }
        .scene()
        .ok_or_else(|| "visual: no scene attached".to_string())?;
    // SAFETY: see above.
    Ok(unsafe { &mut *ptr })
}

/// The attached L2 scene, shared.
fn scene_ref<'a>(ctx: &CompatCtx) -> Result<&'a Scene, String> {
    // SAFETY: see [`scene_mut`].
    let ptr = unsafe { ctx.services() }
        .scene()
        .ok_or_else(|| "visual: no scene attached".to_string())?;
    // SAFETY: see [`scene_mut`].
    Ok(unsafe { &*ptr })
}

/// Write a retained script object into `out` (the C++ trampoline consumes the
/// retention). Falls back to `void` on the (unexpected) duplication failure.
fn set_retained_out(_host: &Tjs2Engine, out: *mut Value, id: tjs::Tjs2ValueId) {
    if out.is_null() {
        return;
    }
    // SAFETY: the trampoline passes a valid out slot for the call.
    unsafe {
        (*out).ty = VAL_RETAINED;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = std::ptr::null();
        (*out).array = std::ptr::null();
        (*out).array_count = 0;
        (*out).retained = id as usize;
    }
}

/// Duplicate a registry object into `out`; `None` writes TJS `null`.
fn set_object_out(host: &Tjs2Engine, out: *mut Value, object: Option<&DetachedValue>) {
    let Some(object) = object else {
        set_null_out(host, out);
        return;
    };
    match host.duplicate_retained(object.raw_id()) {
        Ok(duplicate) => {
            set_retained_out(host, out, duplicate.raw_id());
            // The C++ conversion consumed the retention; the `DetachedValue`
            // drop is a safe no-op.
            std::mem::forget(duplicate);
        }
        Err(error) => {
            log::warn!("visual: could not duplicate a retained object: {error}");
            set_void(out);
        }
    }
}

/// Write TJS `null` into `out`. The C ABI has no null literal, so the `null`
/// expression is evaluated and its object variant retained (as the reference
/// native adapter does). Falls back to `void`, which is still falsy.
fn set_null_out(host: &Tjs2Engine, out: *mut Value) {
    if out.is_null() {
        return;
    }
    if let Ok(RetainedValue::Object(object)) = host.eval_retained("null", "visual.null") {
        set_retained_out(host, out, object.raw_id());
        std::mem::forget(object);
        return;
    }
    set_void(out);
}

/// Write a real result into `out`.
fn set_real(out: *mut Value, value: f64) {
    if out.is_null() {
        return;
    }
    // SAFETY: the trampoline passes a valid out slot for the call.
    unsafe {
        (*out).ty = VAL_REAL;
        (*out).integer = 0;
        (*out).real = value;
        (*out).string = std::ptr::null();
        (*out).array = std::ptr::null();
        (*out).array_count = 0;
        (*out).retained = 0;
    }
}

/// Write a string result into `out` using the visual family's scratch buffer.
fn set_visual_string_out(ctx: &CompatCtx, out: *mut Value, text: &str) {
    if out.is_null() {
        return;
    }
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let state = services.state::<VisualState>();
    let mut scratch = state.scratch.borrow_mut();
    scratch.clear();
    scratch.extend_from_slice(text.as_bytes());
    scratch.push(0);
    // SAFETY: the trampoline passes a valid out slot for the call.
    unsafe {
        (*out).ty = tjs::VAL_STRING;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = scratch.as_ptr() as *const c_char;
        (*out).array = std::ptr::null();
        (*out).array_count = 0;
        (*out).retained = 0;
    }
}

/// ARGB (`0xAARRGGBB`) → straight-alpha RGBA.
fn argb_to_rgba(color: i64) -> [u8; 4] {
    let value = color as u32;
    [
        (value >> 16) as u8,
        (value >> 8) as u8,
        value as u8,
        (value >> 24) as u8,
    ]
}

/// Straight-alpha RGBA → ARGB (`0xAARRGGBB`).
fn rgba_to_argb(color: [u8; 4]) -> i64 {
    (i64::from(color[3]) << 24)
        | (i64::from(color[0]) << 16)
        | (i64::from(color[1]) << 8)
        | i64::from(color[2])
}

/// Retain `objthis` under `id` so cross-object properties can return it.
fn register_window(ctx: &CompatCtx, host: &Tjs2Engine, id: u32, objthis: *mut c_void) {
    if objthis.is_null() {
        return;
    }
    match host.retain_object_detached(objthis) {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        Ok(object) => {
            unsafe { ctx.services() }
                .state::<VisualState>()
                .objects
                .windows
                .insert(id, object);
        }
        Err(error) => log::warn!("Window: could not retain the script object: {error}"),
    }
}

/// See [`register_window`].
fn register_layer(ctx: &CompatCtx, host: &Tjs2Engine, id: u32, objthis: *mut c_void) {
    if objthis.is_null() {
        return;
    }
    match host.retain_object_detached(objthis) {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        Ok(object) => {
            unsafe { ctx.services() }
                .state::<VisualState>()
                .objects
                .layers
                .insert(id, object);
        }
        Err(error) => log::warn!("Layer: could not retain the script object: {error}"),
    }
}

/// See [`register_window`].
fn register_bitmap(ctx: &CompatCtx, host: &Tjs2Engine, id: u32, objthis: *mut c_void) {
    if objthis.is_null() {
        return;
    }
    match host.retain_object_detached(objthis) {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        Ok(object) => {
            unsafe { ctx.services() }
                .state::<VisualState>()
                .objects
                .bitmaps
                .insert(id, object);
        }
        Err(error) => log::warn!("Bitmap: could not retain the script object: {error}"),
    }
}

/// See [`register_window`].
fn register_font(ctx: &CompatCtx, host: &Tjs2Engine, id: u32, objthis: *mut c_void) {
    if objthis.is_null() {
        return;
    }
    match host.retain_object_detached(objthis) {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        Ok(object) => {
            unsafe { ctx.services() }
                .state::<VisualState>()
                .objects
                .fonts
                .insert(id, object);
        }
        Err(error) => log::warn!("Font: could not retain the script object: {error}"),
    }
}

/// Which [`ObjectRegistry`] map an id belongs to.
#[derive(Clone, Copy)]
enum ObjectKind {
    Window,
    Layer,
}

/// Duplicate a registered object into an owned [`DetachedValue`].
fn duplicate_object(
    ctx: &CompatCtx,
    host: &Tjs2Engine,
    kind: ObjectKind,
    id: u32,
) -> Option<DetachedValue> {
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let state = services.state::<VisualState>();
    let source = match kind {
        ObjectKind::Window => state.objects.windows.get(&id),
        ObjectKind::Layer => state.objects.layers.get(&id),
    }?;
    let raw = source.raw_id();
    drop(state);
    host.duplicate_retained(raw).ok()
}

/// Resolve a script object argument to a scene id by reading its `nativeId`
/// then `id` member (the reference wrappers expose both).
fn resolve_object_id(host: &Tjs2Engine, value: &Value) -> Result<i64, String> {
    match value.ty {
        VAL_INTEGER => Ok(value.integer),
        VAL_REAL => Ok(value.real as i64),
        VAL_OBJECT => {
            let object = host.retain_object_arg(value)?;
            let mut last_error = None;
            for member in ["nativeId", "id"] {
                match host.get_member(object.raw_id(), member) {
                    Ok(TjsValue::Integer(v)) => return Ok(v),
                    Ok(TjsValue::Real(v)) => return Ok(v as i64),
                    Ok(_) => {}
                    Err(error) => last_error = Some(error),
                }
            }
            match last_error {
                Some(error) => Err(error),
                None => Ok(-1),
            }
        }
        _ => Ok(-1),
    }
}

/// Detach `layer_id` from its current parent and attach it to
/// `new_parent` (`None` = the window root). Rejects self/descendant cycles.
fn reparent_layer(scene: &mut Scene, layer_id: u32, new_parent: Option<u32>) {
    let Some(layer) = scene.layer(layer_id) else {
        return;
    };
    let window = layer.window;
    let old_parent = layer.parent;
    let new_parent = new_parent.filter(|&parent| {
        if parent == layer_id {
            return false;
        }
        if scene.layer(parent).is_none_or(|p| p.window != window) {
            return false;
        }
        // Reject a cycle: the candidate must not be a descendant of this
        // layer.
        let mut cursor = Some(parent);
        while let Some(id) = cursor {
            if id == layer_id {
                return false;
            }
            cursor = scene.layer(id).and_then(|l| l.parent);
        }
        true
    });
    if old_parent == new_parent {
        return;
    }
    match old_parent {
        Some(parent) => {
            if let Some(parent_layer) = scene.layer_mut(parent) {
                parent_layer.children.retain(|&child| child != layer_id);
            }
        }
        None => {
            if let Some(window_state) = scene.window_mut(window) {
                window_state.layers.retain(|&child| child != layer_id);
            }
        }
    }
    if let Some(layer) = scene.layer_mut(layer_id) {
        layer.parent = new_parent;
    }
    match new_parent {
        Some(parent) => {
            if let Some(parent_layer) = scene.layer_mut(parent) {
                parent_layer.children.retain(|&child| child != layer_id);
                parent_layer.children.push(layer_id);
            }
        }
        None => {
            if let Some(window_state) = scene.window_mut(window) {
                window_state.layers.retain(|&child| child != layer_id);
                window_state.layers.push(layer_id);
            }
        }
    }
}

/// Allocate (or resize) a layer's MainImage to at least `width` x `height`,
/// preserving content and filling expansion with the layer's `neutral_color`
/// (reference `AllocateImage` / `ChangeImageSize`).
fn ensure_layer_image(scene: &mut Scene, layer_id: u32, width: u32, height: u32) {
    let width = width.max(1);
    let height = height.max(1);
    let (bitmap, fill) = {
        let Some(layer) = scene.layer(layer_id) else {
            return;
        };
        (layer.bitmap, argb_to_rgba(layer.neutral_color))
    };
    match bitmap {
        Some(bitmap_id) => {
            scene.resize_bitmap(bitmap_id, width, height, fill);
        }
        None => {
            let Some(pixels) = (width as usize)
                .checked_mul(height as usize)
                .and_then(|count| count.checked_mul(4))
            else {
                return;
            };
            let mut rgba = vec![0u8; pixels];
            for chunk in rgba.chunks_exact_mut(4) {
                chunk.copy_from_slice(&fill);
            }
            let bitmap_id = scene.add_bitmap(width, height, rgba);
            if let Some(layer) = scene.layer_mut(layer_id) {
                layer.bitmap = Some(bitmap_id);
                layer.image_left = 0;
                layer.image_top = 0;
            }
        }
    }
    if let Some(layer) = scene.layer_mut(layer_id) {
        layer.image_width = width;
        layer.image_height = height;
        layer.clip = None;
        layer.image_modified = true;
    }
    if scene.layer(layer_id).is_some_and(|l| l.province.is_some()) {
        scene.resize_province_image(layer_id, width, height);
    }
}

/// Mark a layer's image modified and refresh the tracked image window.
fn mark_layer_image_changed(scene: &mut Scene, layer_id: u32) {
    let dims = scene
        .layer(layer_id)
        .and_then(|l| l.bitmap)
        .and_then(|id| scene.bitmap(id))
        .map(|b| (b.width, b.height));
    if let Some((width, height)) = dims
        && let Some(layer) = scene.layer_mut(layer_id)
    {
        layer.image_width = width;
        layer.image_height = height;
        layer.image_modified = true;
    }
}

/// Build a TJS array of retained Layer objects (used by `Layer.children`).
fn layer_array(
    host: &Tjs2Engine,
    ctx: &CompatCtx,
    ids: &[u32],
) -> Result<Option<DetachedValue>, String> {
    let array = match host
        .eval_retained("[]", "visual.array")
        .map_err(|e| e.to_string())?
    {
        RetainedValue::Object(object) => object,
        RetainedValue::Value(_) => return Ok(None),
    };
    for (index, id) in ids.iter().enumerate() {
        let Some(object) = duplicate_object(ctx, host, ObjectKind::Layer, *id) else {
            continue;
        };
        host.set_member(
            array.raw_id(),
            &index.to_string(),
            &TjsValue::Retained(object.raw_id() as u64),
        )?;
        std::mem::forget(object);
    }
    Ok(Some(array))
}

// ---------------------------------------------------------------------------
// Window
// ---------------------------------------------------------------------------

extern "C" fn window_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(WindowInst::default())) as *mut c_void
}

extern "C" fn window_destroy(engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    let inst = unsafe { &*(instance as *const WindowInst) };
    if let (Some(id), Ok((_resolved, ctx))) = (inst.id, bound(engine)) {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        unsafe { ctx.services() }
            .state::<VisualState>()
            .objects
            .windows
            .remove(&id);
    }
    // SAFETY: instance came from `window_create`'s `Box::into_raw` and is
    // freed exactly once by the VM.
    drop(unsafe { Box::from_raw(instance as *mut WindowInst) });
}

/// `Window()` — add a window to the scene and return its id.
extern "C" fn window_ctor(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window: compat context is not bound");
    };
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    if inst.id.is_some() {
        return report_error(out_error, "Window: this window is already constructed");
    }
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    // The game immediately sizes the window; a non-zero default keeps
    // `win.width`/`win.height` sane before `setInnerSize` (the reference
    // opens with a default size).
    let id = scene.add_window(String::new(), (1280, 720));
    inst.id = Some(id);
    register_window(&ctx, resolved.engine(), id, objthis);
    set_int(out, i64::from(id));
    0
}

/// `Window.close()` — destroy the window and its layer tree.
extern "C" fn window_close(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window.close: compat context is not bound");
    };
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Window.close: this window is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    scene.remove_window(id);
    inst.id = None;
    // SAFETY: the caller resolved `ctx` from the installed binding.
    unsafe { ctx.services() }
        .state::<VisualState>()
        .objects
        .windows
        .remove(&id);
    set_void(out);
    0
}

/// `Window.bringToFront()` — record a host raise request.
extern "C" fn window_bring_to_front(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Window.bringToFront: compat context is not bound",
        );
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    if let Some(id) = inst.id {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        unsafe { ctx.services() }
            .state::<VisualState>()
            .bring_to_front
            .push(id);
    }
    set_void(out);
    0
}

/// `Window.update([type])` — mark every layer of the window for repaint and
/// record a host redraw request.
extern "C" fn window_update(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window.update: compat context is not bound");
    };
    let update_type = if argc >= 1 {
        value_as_i64(&unsafe { args(argv, argc) }[0]) as i32
    } else {
        0
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Window.update: this window is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    for layer_id in scene.window_layer_order(id) {
        if let Some(layer) = scene.layer_mut(layer_id) {
            layer.pending_paint = true;
        }
    }
    // SAFETY: the caller resolved `ctx` from the installed binding.
    unsafe { ctx.services() }
        .state::<VisualState>()
        .update_requests
        .push((id, update_type));
    set_void(out);
    0
}

/// `Window.add(layer)` — attach a layer under `primaryLayer`.
extern "C" fn window_add(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Window.add requires 1 argument");
    }
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window.add: compat context is not bound");
    };
    let a = unsafe { args(argv, argc) };
    let layer_id = match resolve_object_id(resolved.engine(), &a[0]) {
        Ok(id) if id >= 0 => id as u32,
        Ok(_) => return report_error(out_error, "Window.add: not a Layer object"),
        Err(error) => return report_error(out_error, &error),
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(window) = inst.id else {
        return report_error(out_error, "Window.add: this window is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let primary = scene
        .window(window)
        .and_then(|w| w.primary_layer)
        .filter(|&primary| primary != layer_id);
    reparent_layer(scene, layer_id, primary);
    set_void(out);
    0
}

/// `Window.remove(layer)` — part a layer from this window (`Part()`
/// detachment; the children stay alive).
extern "C" fn window_remove(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Window.remove requires 1 argument");
    }
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window.remove: compat context is not bound");
    };
    let a = unsafe { args(argv, argc) };
    let layer_id = match resolve_object_id(resolved.engine(), &a[0]) {
        Ok(id) if id >= 0 => id as u32,
        Ok(_) => return report_error(out_error, "Window.remove: not a Layer object"),
        Err(error) => return report_error(out_error, &error),
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(window) = inst.id else {
        return report_error(out_error, "Window.remove: this window is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if scene.layer(layer_id).is_some_and(|l| l.window == window) {
        scene.remove_layer(layer_id);
    }
    set_void(out);
    0
}

/// `Window.setSize(width, height)` and `setInnerSize(width, height)`.
extern "C" fn window_set_size(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Window.setSize requires 2 arguments");
    }
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window.setSize: compat context is not bound");
    };
    let a = unsafe { args(argv, argc) };
    let width = value_as_i64(&a[0]).max(0) as u32;
    let height = value_as_i64(&a[1]).max(0) as u32;
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Window.setSize: this window is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if let Some(window) = scene.window_mut(id) {
        window.inner_size = (width, height);
    }
    set_void(out);
    0
}

/// `Window.setInnerSize(width, height)`.
extern "C" fn window_set_inner_size_method(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    window_set_size(engine, instance, argc, argv, out, out_error, _objthis)
}

/// `Window.setPos(left, top)`.
extern "C" fn window_set_pos(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Window.setPos requires 2 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    inst.left = value_as_i64(&a[0]) as i32;
    inst.top = value_as_i64(&a[1]) as i32;
    set_void(out);
    0
}

/// `Window.setMinSize(width, height)`.
extern "C" fn window_set_min_size(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Window.setMinSize requires 2 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    inst.min_width = value_as_i64(&a[0]) as i32;
    inst.min_height = value_as_i64(&a[1]) as i32;
    set_void(out);
    0
}

/// `Window.setMaxSize(width, height)`.
extern "C" fn window_set_max_size(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Window.setMaxSize requires 2 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    inst.max_width = value_as_i64(&a[0]) as i32;
    inst.max_height = value_as_i64(&a[1]) as i32;
    set_void(out);
    0
}

/// `Window.setZoom(numer, denom)`.
extern "C" fn window_set_zoom(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Window.setZoom requires 2 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    let numer = value_as_i64(&a[0]).max(1) as i32;
    let denom = value_as_i64(&a[1]).max(1) as i32;
    inst.zoom_numer = numer;
    inst.zoom_denom = denom;
    set_void(out);
    0
}

/// `Window.showModal([modal])` — record the modal state.
extern "C" fn window_show_modal(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    inst.modal = if argc >= 1 {
        value_as_bool(&unsafe { args(argv, argc) }[0])
    } else {
        true
    };
    set_void(out);
    0
}

/// `Window.setMaskRegion(threshold)`.
extern "C" fn window_set_mask_region(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Window.setMaskRegion requires 1 argument");
    }
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    inst.mask_region = Some(value_as_i64(&unsafe { args(argv, argc) }[0]) as i32);
    set_void(out);
    0
}

/// `Window.removeMaskRegion()`.
extern "C" fn window_remove_mask_region(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    inst.mask_region = None;
    set_void(out);
    0
}

/// `Window.hideMouseCursor()` — record the cursor-hidden state.
extern "C" fn window_hide_mouse_cursor(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    inst.mouse_cursor_state = 2;
    set_void(out);
    0
}

/// `Window.changeScreenMode([fullscreen [, width, height]])` — record the
/// fullscreen flag (and resize the logical client when a size is given).
extern "C" fn window_change_screen_mode(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Window.changeScreenMode: compat context is not bound",
        );
    };
    let a = unsafe { args(argv, argc) };
    let full_screen = a.first().is_some_and(value_as_bool);
    let size = match (a.get(1), a.get(2)) {
        (Some(width), Some(height)) => Some((
            value_as_i64(width).max(0) as u32,
            value_as_i64(height).max(0) as u32,
        )),
        _ => None,
    };
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    inst.full_screen = full_screen;
    if let (Some(id), Some((width, height))) = (inst.id, size) {
        let scene = match scene_mut(&ctx) {
            Ok(scene) => scene,
            Err(error) => return report_error(out_error, &error),
        };
        if let Some(window) = scene.window_mut(id) {
            window.inner_size = (width, height);
        }
    }
    set_void(out);
    0
}

/// `Window.postInputEvent(name[, params])` — accepted and logged; the host
/// input bridge is not wired yet.
extern "C" fn window_post_input_event(
    _engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Window.postInputEvent requires an event name");
    }
    let name = value_as_string(&unsafe { args(argv, argc) }[0]);
    log::debug!("Window.postInputEvent({name:?}): no input bridge is wired; ignored");
    set_void(out);
    0
}

// --- Window properties -----------------------------------------------------

extern "C" fn window_id_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const WindowInst) };
    match inst.id {
        Some(id) => set_int(out, i64::from(id)),
        None => set_int(out, 0),
    }
    0
}

extern "C" fn window_visible_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window.visible: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        set_bool(out, false);
        return 0;
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    set_bool(out, scene.window(id).is_some_and(|w| w.visible));
    0
}

extern "C" fn window_visible_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window.visible: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    let visible = value_as_bool(unsafe { &*value });
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if let Some(window) = scene.window_mut(id) {
        window.visible = visible;
    }
    0
}

extern "C" fn window_caption_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window.caption: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let title = scene
        .window(id)
        .map(|w| w.title.clone())
        .unwrap_or_default();
    set_string_out(&ctx, out, &title);
    0
}

extern "C" fn window_caption_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window.caption: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    let title = value_as_string(unsafe { &*value });
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if let Some(window) = scene.window_mut(id) {
        window.title = title;
    }
    0
}

extern "C" fn window_width_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window.width: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let width = scene.window(id).map_or(0, |w| w.inner_size.0);
    set_int(out, i64::from(width));
    0
}

extern "C" fn window_width_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window.width: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    let width = value_as_i64(unsafe { &*value }).max(0) as u32;
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if let Some(window) = scene.window_mut(id) {
        window.inner_size.0 = width;
    }
    0
}

extern "C" fn window_height_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window.height: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let height = scene.window(id).map_or(0, |w| w.inner_size.1);
    set_int(out, i64::from(height));
    0
}

extern "C" fn window_height_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window.height: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    let height = value_as_i64(unsafe { &*value }).max(0) as u32;
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if let Some(window) = scene.window_mut(id) {
        window.inner_size.1 = height;
    }
    0
}

/// Shared accessor for the `WindowInst`-held integer/bool properties.
macro_rules! window_i64_prop {
    ($get:ident, $set:ident, $field:ident) => {
        extern "C" fn $get(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            let inst = unsafe { &*(instance as *const WindowInst) };
            set_int(out, i64::from(inst.$field));
            0
        }
        extern "C" fn $set(
            _engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            let inst = unsafe { &mut *(instance as *mut WindowInst) };
            inst.$field = value_as_i64(unsafe { &*value }) as i32;
            0
        }
    };
}

window_i64_prop!(window_left_get, window_left_set, left);
window_i64_prop!(window_top_get, window_top_set, top);
window_i64_prop!(window_min_width_get, window_min_width_set, min_width);
window_i64_prop!(window_min_height_get, window_min_height_set, min_height);
window_i64_prop!(window_max_width_get, window_max_width_set, max_width);
window_i64_prop!(window_max_height_get, window_max_height_set, max_height);
window_i64_prop!(window_zoom_numer_get, window_zoom_numer_set, zoom_numer);
window_i64_prop!(window_zoom_denom_get, window_zoom_denom_set, zoom_denom);
window_i64_prop!(window_hint_delay_get, window_hint_delay_set, hint_delay);
window_i64_prop!(
    window_mouse_cursor_state_get,
    window_mouse_cursor_state_set,
    mouse_cursor_state
);
window_i64_prop!(window_ime_mode_get, window_ime_mode_set, ime_mode);

macro_rules! window_bool_prop {
    ($get:ident, $set:ident, $field:ident) => {
        extern "C" fn $get(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            let inst = unsafe { &*(instance as *const WindowInst) };
            set_bool(out, inst.$field);
            0
        }
        extern "C" fn $set(
            _engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            let inst = unsafe { &mut *(instance as *mut WindowInst) };
            inst.$field = value_as_bool(unsafe { &*value });
            0
        }
    };
}

window_bool_prop!(window_focusable_get, window_focusable_set, focusable);
window_bool_prop!(window_full_screen_get, window_full_screen_set, full_screen);
window_bool_prop!(window_wait_vsync_get, window_wait_vsync_set, wait_vsync);
window_bool_prop!(
    window_enable_touch_get,
    window_enable_touch_set,
    enable_touch
);
window_bool_prop!(
    window_use_mouse_key_get,
    window_use_mouse_key_set,
    use_mouse_key
);
window_bool_prop!(window_trap_key_get, window_trap_key_set, trap_key);

/// `Window.mainWindow` — the scene's first window object (`TVPMainWindow`).
extern "C" fn window_main_window_get(
    engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window.mainWindow: compat context is not bound");
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let id = scene.windows.first().map(|w| w.id);
    let object =
        id.and_then(|id| duplicate_object(&ctx, resolved.engine(), ObjectKind::Window, id));
    set_object_out(resolved.engine(), out, object.as_ref());
    0
}

/// `Window.focusedLayer` — the window's focused layer object, or `null`.
extern "C" fn window_focused_layer_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Window.focusedLayer: compat context is not bound",
        );
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        set_null_out(resolved.engine(), out);
        return 0;
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let focused = scene.window(id).and_then(|w| w.focused_layer);
    let object = focused
        .and_then(|layer| duplicate_object(&ctx, resolved.engine(), ObjectKind::Layer, layer));
    set_object_out(resolved.engine(), out, object.as_ref());
    0
}

/// `Window.focusedLayer = layer` — focus the layer (null/void clears it).
extern "C" fn window_focused_layer_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Window.focusedLayer: compat context is not bound",
        );
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(window) = inst.id else {
        return 0;
    };
    let value = unsafe { &*value };
    let layer_id = match resolve_object_id(resolved.engine(), value) {
        Ok(id) if id >= 0 => Some(id as u32),
        _ => None,
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    match layer_id {
        Some(layer) => {
            scene.set_focus(layer);
        }
        None => {
            scene.clear_focus(window);
        }
    }
    0
}

/// `Window.primaryLayer` — the window's primary layer object, or `null`.
extern "C" fn window_primary_layer_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Window.primaryLayer: compat context is not bound",
        );
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        set_null_out(resolved.engine(), out);
        return 0;
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let primary = scene.window(id).and_then(|w| w.primary_layer);
    let object = primary
        .and_then(|layer| duplicate_object(&ctx, resolved.engine(), ObjectKind::Layer, layer));
    set_object_out(resolved.engine(), out, object.as_ref());
    0
}

// ---------------------------------------------------------------------------
// Layer
// ---------------------------------------------------------------------------

extern "C" fn layer_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(LayerInst::default())) as *mut c_void
}

extern "C" fn layer_destroy(engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    let inst = unsafe { &mut *(instance as *mut LayerInst) };
    if let (Some(id), Ok((_resolved, ctx))) = (inst.id, bound(engine)) {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        unsafe { ctx.services() }
            .state::<VisualState>()
            .objects
            .layers
            .remove(&id);
    }
    // Dropping the payload releases the cached `font` object.
    // SAFETY: instance came from `layer_create`'s `Box::into_raw` and is
    // freed exactly once by the VM.
    drop(unsafe { Box::from_raw(instance as *mut LayerInst) });
}

/// `Layer(window, parent)` — add a layer to the scene and return its id.
///
/// `window` may be a `Window` object (or an integer id); `parent` a `Layer`
/// object/id or null. A missing window attaches to the scene's first window
/// (the game has one window).
extern "C" fn layer_ctor(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer: compat context is not bound");
    };
    let a = unsafe { args(argv, argc) };
    let inst = unsafe { &mut *(instance as *mut LayerInst) };
    if inst.id.is_some() {
        return report_error(out_error, "Layer: this layer is already constructed");
    }
    // Window: explicit object/id, else the scene's first window.
    let window = match a.first() {
        Some(value)
            if value.ty == VAL_OBJECT || value.ty == VAL_INTEGER || value.ty == VAL_REAL =>
        {
            resolve_object_id(resolved.engine(), value)
                .ok()
                .filter(|&id| id >= 0)
        }
        _ => None,
    };
    // Parent: a `Layer` object/id, else the window root. Resolved before the
    // scene borrow so the getter cannot re-enter the VM while `&mut Scene` is
    // held.
    let parent = match a.get(1) {
        Some(value) if value.ty == VAL_OBJECT => resolve_object_id(resolved.engine(), value)
            .ok()
            .filter(|&id| id >= 0)
            .map(|id| id as u32),
        Some(value) if value.ty == VAL_INTEGER && value.integer >= 0 => Some(value.integer as u32),
        _ => None,
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let window = match window {
        Some(id) if scene.window(id as u32).is_some() => id as u32,
        _ => match scene.windows.first() {
            Some(window) => window.id,
            None => {
                return report_error(
                    out_error,
                    "Layer: no window in the scene — create a Window first",
                );
            }
        },
    };
    let id = scene.add_layer(window, parent);
    inst.id = Some(id);
    register_layer(&ctx, resolved.engine(), id, objthis);
    set_int(out, i64::from(id));
    0
}

/// `Layer.setPos(left, top [, width, height])`.
extern "C" fn layer_set_pos(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Layer.setPos requires 2 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let size = match (a.get(2), a.get(3)) {
        (Some(w), Some(h)) => Some((value_as_i64(w).max(0) as u32, value_as_i64(h).max(0) as u32)),
        _ => None,
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.setPos: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.setPos: this layer is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if let Some(layer) = scene.layer_mut(id) {
        layer.rect.x = x;
        layer.rect.y = y;
        if let Some((w, h)) = size {
            layer.rect.w = w;
            layer.rect.h = h;
        }
    }
    if size.is_some() {
        mark_layer_image_changed(scene, id);
    }
    set_void(out);
    0
}

/// `Layer.setSize(width, height)`.
extern "C" fn layer_set_size(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Layer.setSize requires 2 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let width = value_as_i64(&a[0]).max(0) as u32;
    let height = value_as_i64(&a[1]).max(0) as u32;
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.setSize: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.setSize: this layer is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if let Some(layer) = scene.layer_mut(id) {
        layer.rect.w = width;
        layer.rect.h = height;
    }
    mark_layer_image_changed(scene, id);
    set_void(out);
    0
}

/// `Layer.setSizeToImageSize()` — size the layer to its MainImage.
extern "C" fn layer_set_size_to_image_size(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.setSizeToImageSize: compat context is not bound",
        );
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.setSizeToImageSize: this layer is not constructed",
        );
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if let Some((width, height)) = scene
        .layer(id)
        .and_then(|l| l.bitmap)
        .and_then(|bitmap| scene.bitmap(bitmap))
        .map(|b| (b.width, b.height))
        && let Some(layer) = scene.layer_mut(id)
    {
        layer.rect.w = width;
        layer.rect.h = height;
    }
    set_void(out);
    0
}

/// `Layer.setImagePos(left, top)`.
extern "C" fn layer_set_image_pos(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Layer.setImagePos requires 2 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let left = value_as_i64(&a[0]) as i32;
    let top = value_as_i64(&a[1]) as i32;
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.setImagePos: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.setImagePos: this layer is not constructed",
        );
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if let Some(layer) = scene.layer_mut(id) {
        layer.image_left = left;
        layer.image_top = top;
    }
    set_void(out);
    0
}

/// `Layer.setImageSize(width, height)`.
extern "C" fn layer_set_image_size(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Layer.setImageSize requires 2 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let width = value_as_i64(&a[0]).max(0) as u32;
    let height = value_as_i64(&a[1]).max(0) as u32;
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.setImageSize: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.setImageSize: this layer is not constructed",
        );
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    ensure_layer_image(scene, id, width, height);
    set_void(out);
    0
}

/// Shared body of `Layer.setBitmap` / `setImage` / `assignImages`.
fn layer_attach_bitmap(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    method: &str,
) -> c_int {
    if argc < 1 {
        return report_error(
            out_error,
            &format!("Layer.{method} requires a Bitmap argument"),
        );
    }
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            &format!("Layer.{method}: compat context is not bound"),
        );
    };
    let bitmap_id = match resolve_object_id(resolved.engine(), &unsafe { args(argv, argc) }[0]) {
        Ok(id) if id >= 0 => id as u32,
        Ok(_) => return report_error(out_error, &format!("Layer.{method}: not a Bitmap object")),
        Err(error) => return report_error(out_error, &error),
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            &format!("Layer.{method}: this layer is not constructed"),
        );
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let Some((width, height)) = scene.bitmap(bitmap_id).map(|b| (b.width, b.height)) else {
        return report_error(
            out_error,
            &format!("Layer.{method}: the bitmap does not exist"),
        );
    };
    if let Some(layer) = scene.layer_mut(id) {
        layer.bitmap = Some(bitmap_id);
        layer.image_left = 0;
        layer.image_top = 0;
        layer.image_width = width;
        layer.image_height = height;
        layer.image_modified = true;
    }
    set_void(out);
    0
}

extern "C" fn layer_set_bitmap(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_attach_bitmap(engine, instance, argc, argv, out, out_error, "setBitmap")
}

extern "C" fn layer_set_image(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_attach_bitmap(engine, instance, argc, argv, out, out_error, "setImage")
}

extern "C" fn layer_assign_images(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_attach_bitmap(engine, instance, argc, argv, out, out_error, "assignImages")
}

/// `Layer.copyFromBitmapToMainImage(bitmap)` — copy the source pixels into
/// the layer's MainImage (allocating/resizing it to match).
extern "C" fn layer_copy_from_bitmap_to_main_image(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(
            out_error,
            "Layer.copyFromBitmapToMainImage requires a Bitmap argument",
        );
    }
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.copyFromBitmapToMainImage: compat context is not bound",
        );
    };
    let bitmap_id = match resolve_object_id(resolved.engine(), &unsafe { args(argv, argc) }[0]) {
        Ok(id) if id >= 0 => id as u32,
        Ok(_) => return report_error(out_error, "Layer.copyFromBitmapToMainImage: not a Bitmap"),
        Err(error) => return report_error(out_error, &error),
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.copyFromBitmapToMainImage: this layer is not constructed",
        );
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let Some((width, height, rgba)) = scene
        .bitmap(bitmap_id)
        .map(|b| (b.width, b.height, b.rgba.clone()))
    else {
        return report_error(
            out_error,
            "Layer.copyFromBitmapToMainImage: the bitmap does not exist",
        );
    };
    ensure_layer_image(scene, id, width, height);
    if let Some(layer_bitmap) = scene.layer(id).and_then(|l| l.bitmap)
        && let Some(bitmap) = scene.bitmap_mut(layer_bitmap)
    {
        bitmap.rgba = rgba;
        bitmap.dirty = true;
    }
    set_void(out);
    0
}

/// `Layer.independMainImage()` — give the layer a private copy of its
/// MainImage.
extern "C" fn layer_independ_main_image(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.independMainImage: compat context is not bound",
        );
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.independMainImage: this layer is not constructed",
        );
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let Some((width, height, rgba)) = scene
        .layer(id)
        .and_then(|l| l.bitmap)
        .and_then(|bitmap| scene.bitmap(bitmap))
        .map(|b| (b.width, b.height, b.rgba.clone()))
    else {
        set_void(out);
        return 0;
    };
    let new_bitmap = scene.add_bitmap(width, height, rgba);
    if let Some(layer) = scene.layer_mut(id) {
        layer.bitmap = Some(new_bitmap);
        layer.image_width = width;
        layer.image_height = height;
        layer.image_modified = true;
    }
    set_void(out);
    0
}

/// `Layer.bringToFront()` / `moveToFront()`.
extern "C" fn layer_bring_to_front(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.bringToFront: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.bringToFront: this layer is not constructed",
        );
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    scene.layer_move_to_front(id);
    set_void(out);
    0
}

/// `Layer.setParentId(parent[, ...])` — re-parent the layer.
extern "C" fn layer_set_parent_id(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.setParentId: compat context is not bound");
    };
    let parent = if argc >= 1 {
        let value = &unsafe { args(argv, argc) }[0];
        if value.ty == VAL_VOID {
            None
        } else {
            resolve_object_id(resolved.engine(), value)
                .ok()
                .filter(|&id| id >= 0)
                .map(|id| id as u32)
        }
    } else {
        None
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.setParentId: this layer is not constructed",
        );
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    reparent_layer(scene, id, parent);
    set_void(out);
    0
}

/// `Layer.focus()` — give the layer keyboard focus.
extern "C" fn layer_focus(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.focus: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.focus: this layer is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    scene.set_focus(id);
    set_void(out);
    0
}

/// `Layer.update()` — request the layer's `onPaint` and a repaint.
extern "C" fn layer_update(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.update: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.update: this layer is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if let Some(layer) = scene.layer_mut(id) {
        layer.pending_paint = true;
    }
    set_void(out);
    0
}

/// `Layer.setClip(left, top, width, height)`.
extern "C" fn layer_set_clip(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 4 {
        return report_error(out_error, "Layer.setClip requires 4 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let w = value_as_i64(&a[2]).max(0) as u32;
    let h = value_as_i64(&a[3]).max(0) as u32;
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.setClip: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.setClip: this layer is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if let Some(layer) = scene.layer_mut(id) {
        layer.clip = Some(Rect { x, y, w, h });
    }
    set_void(out);
    0
}

/// `Layer.getProvincePixel(x, y)`.
extern "C" fn layer_get_province_pixel(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Layer.getProvincePixel requires 2 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.getProvincePixel: compat context is not bound",
        );
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.getProvincePixel: this layer is not constructed",
        );
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    set_int(out, i64::from(scene.get_province_pixel(id, x, y)));
    0
}

/// `Layer.setProvincePixel(x, y, n)`.
extern "C" fn layer_set_province_pixel(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 3 {
        return report_error(out_error, "Layer.setProvincePixel requires 3 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let n = value_as_i64(&a[2]) as i32;
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.setProvincePixel: compat context is not bound",
        );
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.setProvincePixel: this layer is not constructed",
        );
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    scene.set_province_pixel(id, x, y, n);
    set_void(out);
    0
}

/// `Layer.getLayerAt(x, y [, excludeSelf [, getDisabled]])` — the frontmost
/// layer at the point, as a Layer object (or `null`).
extern "C" fn layer_get_layer_at(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Layer.getLayerAt requires 2 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let exclude_self = a.get(2).is_some_and(value_as_bool);
    let get_disabled = a.get(3).is_some_and(value_as_bool);
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.getLayerAt: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.getLayerAt: this layer is not constructed");
    };
    let hit = {
        let scene = match scene_ref(&ctx) {
            Ok(scene) => scene,
            Err(error) => return report_error(out_error, &error),
        };
        scene.layer_at(id, x, y, exclude_self, get_disabled)
    };
    let object =
        hit.and_then(|layer| duplicate_object(&ctx, resolved.engine(), ObjectKind::Layer, layer));
    set_object_out(resolved.engine(), out, object.as_ref());
    0
}

/// `Layer.fillRect(left, top, width, height, color)`.
extern "C" fn layer_fill_rect(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 5 {
        return report_error(out_error, "Layer.fillRect requires 5 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let w = value_as_i64(&a[2]).max(0) as u32;
    let h = value_as_i64(&a[3]).max(0) as u32;
    let color = argb_to_rgba(value_as_i64(&a[4]));
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.fillRect: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.fillRect: this layer is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let bitmap_id = scene.layer(id).and_then(|layer| layer.bitmap);
    match bitmap_id {
        Some(bitmap_id) => {
            if let Some(bitmap) = scene.bitmap_mut(bitmap_id) {
                for py in y.max(0)..y.saturating_add(h as i32) {
                    for px in x.max(0)..x.saturating_add(w as i32) {
                        if let Some(offset) = bitmap.pixel_offset(px as u32, py as u32) {
                            bitmap.rgba[offset..offset + 4].copy_from_slice(&color);
                        }
                    }
                }
                bitmap.mark_dirty();
            }
        }
        None => {
            if let Some(layer) = scene.layer_mut(id) {
                layer.fill_color = Some(color);
                layer.rect.w = layer.rect.w.max(x.max(0) as u32 + w);
                layer.rect.h = layer.rect.h.max(y.max(0) as u32 + h);
            }
        }
    }
    set_void(out);
    0
}

// --- Layer property helpers ------------------------------------------------

/// Read a layer through the bound scene.
fn with_layer_read<R>(
    engine: *mut c_void,
    instance: *mut c_void,
    f: impl FnOnce(&LayerState) -> R,
) -> Result<R, String> {
    let inst = unsafe { &*(instance as *const LayerInst) };
    let id = inst
        .id
        .ok_or_else(|| "Layer: this layer is not constructed".to_string())?;
    let (_resolved, ctx) = bound(engine)?;
    let scene = scene_ref(&ctx)?;
    scene
        .layer(id)
        .map(f)
        .ok_or_else(|| "Layer: layer no longer exists".to_string())
}

/// Mutate a layer through the bound scene.
fn with_layer_write(
    engine: *mut c_void,
    instance: *mut c_void,
    value: &Value,
    f: impl FnOnce(&mut LayerState, &Value),
) -> Result<(), String> {
    let inst = unsafe { &*(instance as *const LayerInst) };
    let id = inst
        .id
        .ok_or_else(|| "Layer: this layer is not constructed".to_string())?;
    let (_resolved, ctx) = bound(engine)?;
    let scene = scene_mut(&ctx)?;
    let layer = scene
        .layer_mut(id)
        .ok_or_else(|| "Layer: layer no longer exists".to_string())?;
    f(layer, value);
    Ok(())
}

/// Define a read/write layer property over an `i64`/bool field.
macro_rules! layer_int_prop {
    ($get:ident, $set:ident, $getter:expr, $setter:expr) => {
        #[allow(dead_code)]
        extern "C" fn $get(
            engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            match with_layer_read(engine, instance, $getter) {
                Ok(value) => {
                    set_int(out, value);
                    0
                }
                Err(error) => report_error(out_error, &error),
            }
        }
        #[allow(dead_code)]
        extern "C" fn $set(
            engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: the trampoline passes a valid value slot.
            let value = unsafe { &*value };
            match with_layer_write(engine, instance, value, $setter) {
                Ok(()) => 0,
                Err(error) => report_error(out_error, &error),
            }
        }
    };
}

layer_int_prop!(
    layer_visible_get,
    layer_visible_set,
    |l: &LayerState| i64::from(l.visible),
    |l: &mut LayerState, v: &Value| l.visible = value_as_bool(v)
);
layer_int_prop!(
    layer_type_get,
    layer_type_set,
    |l: &LayerState| l.blend_type,
    |l: &mut LayerState, v: &Value| l.blend_type = value_as_i64(v)
);
layer_int_prop!(
    layer_left_get,
    layer_left_set,
    |l: &LayerState| i64::from(l.rect.x),
    |l: &mut LayerState, v: &Value| l.rect.x = value_as_i64(v) as i32
);
layer_int_prop!(
    layer_top_get,
    layer_top_set,
    |l: &LayerState| i64::from(l.rect.y),
    |l: &mut LayerState, v: &Value| l.rect.y = value_as_i64(v) as i32
);
layer_int_prop!(
    layer_width_get,
    layer_width_set,
    |l: &LayerState| i64::from(l.rect.w),
    |l: &mut LayerState, v: &Value| l.rect.w = value_as_i64(v).max(0) as u32
);
layer_int_prop!(
    layer_height_get,
    layer_height_set,
    |l: &LayerState| i64::from(l.rect.h),
    |l: &mut LayerState, v: &Value| l.rect.h = value_as_i64(v).max(0) as u32
);
layer_int_prop!(
    layer_id_get,
    layer_id_set,
    |l: &LayerState| i64::from(l.id),
    |_l: &mut LayerState, _v: &Value| {}
);
layer_int_prop!(
    layer_absolute_get,
    layer_absolute_set,
    |l: &LayerState| i64::from(l.z_order),
    |l: &mut LayerState, v: &Value| l.z_order = value_as_i64(v) as i32
);
layer_int_prop!(
    layer_hit_threshold_get,
    layer_hit_threshold_set,
    |l: &LayerState| i64::from(l.hit_threshold),
    |l: &mut LayerState, v: &Value| l.hit_threshold = value_as_i64(v) as i32
);
layer_int_prop!(
    layer_hit_type_get,
    layer_hit_type_set,
    |l: &LayerState| i64::from(l.hit_type),
    |l: &mut LayerState, v: &Value| l.hit_type = value_as_i64(v) as i32
);
layer_int_prop!(
    layer_cursor_get,
    layer_cursor_set,
    |l: &LayerState| i64::from(l.cursor),
    |l: &mut LayerState, v: &Value| l.cursor = value_as_i64(v) as i32
);
layer_int_prop!(
    layer_face_get,
    layer_face_set,
    |l: &LayerState| i64::from(l.face),
    |l: &mut LayerState, v: &Value| l.face = value_as_i64(v) as i32
);
layer_int_prop!(
    layer_hold_alpha_get,
    layer_hold_alpha_set,
    |l: &LayerState| i64::from(l.hold_alpha),
    |l: &mut LayerState, v: &Value| l.hold_alpha = value_as_bool(v)
);
layer_int_prop!(
    layer_image_left_get,
    layer_image_left_set,
    |l: &LayerState| i64::from(l.image_left),
    |l: &mut LayerState, v: &Value| l.image_left = value_as_i64(v) as i32
);
layer_int_prop!(
    layer_image_top_get,
    layer_image_top_set,
    |l: &LayerState| i64::from(l.image_top),
    |l: &mut LayerState, v: &Value| l.image_top = value_as_i64(v) as i32
);
layer_int_prop!(
    layer_image_width_get,
    layer_image_width_set,
    |l: &LayerState| i64::from(l.image_width),
    |_l: &mut LayerState, _v: &Value| {}
);
layer_int_prop!(
    layer_image_height_get,
    layer_image_height_set,
    |l: &LayerState| i64::from(l.image_height),
    |_l: &mut LayerState, _v: &Value| {}
);
layer_int_prop!(
    layer_focusable_get,
    layer_focusable_set,
    |l: &LayerState| i64::from(l.focusable),
    |l: &mut LayerState, v: &Value| l.focusable = value_as_bool(v)
);
layer_int_prop!(
    layer_join_focus_chain_get,
    layer_join_focus_chain_set,
    |l: &LayerState| i64::from(l.join_focus_chain),
    |l: &mut LayerState, v: &Value| l.join_focus_chain = value_as_bool(v)
);
layer_int_prop!(
    layer_enabled_get,
    layer_enabled_set,
    |l: &LayerState| i64::from(l.enabled),
    |l: &mut LayerState, v: &Value| l.enabled = value_as_bool(v)
);
layer_int_prop!(
    layer_cached_get,
    layer_cached_set,
    |l: &LayerState| i64::from(l.cached),
    |l: &mut LayerState, v: &Value| l.cached = value_as_bool(v)
);
layer_int_prop!(
    layer_image_modified_get,
    layer_image_modified_set,
    |l: &LayerState| i64::from(l.image_modified),
    |l: &mut LayerState, v: &Value| l.image_modified = value_as_bool(v)
);
layer_int_prop!(
    layer_attention_left_get,
    layer_attention_left_set,
    |l: &LayerState| i64::from(l.attention_left),
    |l: &mut LayerState, v: &Value| l.attention_left = value_as_i64(v) as i32
);
layer_int_prop!(
    layer_attention_top_get,
    layer_attention_top_set,
    |l: &LayerState| i64::from(l.attention_top),
    |l: &mut LayerState, v: &Value| l.attention_top = value_as_i64(v) as i32
);
layer_int_prop!(
    layer_use_attention_get,
    layer_use_attention_set,
    |l: &LayerState| i64::from(l.use_attention),
    |l: &mut LayerState, v: &Value| l.use_attention = value_as_bool(v)
);
layer_int_prop!(
    layer_show_parent_hint_get,
    layer_show_parent_hint_set,
    |l: &LayerState| i64::from(l.show_parent_hint),
    |l: &mut LayerState, v: &Value| l.show_parent_hint = value_as_bool(v)
);
layer_int_prop!(
    layer_ignore_hint_sensing_get,
    layer_ignore_hint_sensing_set,
    |l: &LayerState| i64::from(l.ignore_hint_sensing),
    |l: &mut LayerState, v: &Value| l.ignore_hint_sensing = value_as_bool(v)
);
layer_int_prop!(
    layer_ime_mode_get,
    layer_ime_mode_set,
    |l: &LayerState| i64::from(l.ime_mode),
    |l: &mut LayerState, v: &Value| l.ime_mode = value_as_i64(v) as i32
);
layer_int_prop!(
    layer_neutral_color_get,
    layer_neutral_color_set,
    |l: &LayerState| l.neutral_color,
    |l: &mut LayerState, v: &Value| l.neutral_color = value_as_i64(v)
);
layer_int_prop!(
    layer_absolute_order_mode_get,
    layer_absolute_order_mode_set,
    |l: &LayerState| i64::from(l.absolute_order_mode),
    |l: &mut LayerState, v: &Value| l.absolute_order_mode = value_as_bool(v)
);
layer_int_prop!(
    layer_call_on_paint_get,
    layer_call_on_paint_set,
    |l: &LayerState| i64::from(l.call_on_paint),
    |l: &mut LayerState, v: &Value| l.call_on_paint = value_as_bool(v)
);
layer_int_prop!(
    layer_clip_left_get,
    layer_clip_left_set,
    |l: &LayerState| i64::from(l.clip.map_or(0, |c| c.x)),
    |l: &mut LayerState, v: &Value| {
        let rect = l.clip.get_or_insert(Rect::default());
        rect.x = value_as_i64(v) as i32;
    }
);
layer_int_prop!(
    layer_clip_top_get,
    layer_clip_top_set,
    |l: &LayerState| i64::from(l.clip.map_or(0, |c| c.y)),
    |l: &mut LayerState, v: &Value| {
        let rect = l.clip.get_or_insert(Rect::default());
        rect.y = value_as_i64(v) as i32;
    }
);
layer_int_prop!(
    layer_clip_width_get,
    layer_clip_width_set,
    |l: &LayerState| i64::from(l.clip.map_or(0, |c| c.w)),
    |l: &mut LayerState, v: &Value| {
        let rect = l.clip.get_or_insert(Rect::default());
        rect.w = value_as_i64(v).max(0) as u32;
    }
);
layer_int_prop!(
    layer_clip_height_get,
    layer_clip_height_set,
    |l: &LayerState| i64::from(l.clip.map_or(0, |c| c.h)),
    |l: &mut LayerState, v: &Value| {
        let rect = l.clip.get_or_insert(Rect::default());
        rect.h = value_as_i64(v).max(0) as u32;
    }
);

/// `layer.opacity` (0..255 scale, reference `GetOpacity`/`SetOpacity`).
extern "C" fn layer_opacity_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    match with_layer_read(engine, instance, |l| {
        (f64::from(l.opacity) * 255.0).round() as i64
    }) {
        Ok(value) => {
            set_int(out, value);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

extern "C" fn layer_opacity_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let value = unsafe { &*value };
    match with_layer_write(engine, instance, value, |l, v| {
        l.opacity = (value_as_i64(v) as f64 / 255.0).clamp(0.0, 1.0) as f32;
    }) {
        Ok(()) => 0,
        Err(error) => report_error(out_error, &error),
    }
}

/// `layer.hasImage` — whether the layer has a MainImage.
extern "C" fn layer_has_image_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    match with_layer_read(engine, instance, |l| l.bitmap.is_some()) {
        Ok(value) => {
            set_bool(out, value);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

extern "C" fn layer_has_image_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let wanted = value_as_bool(unsafe { &*value });
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.hasImage: this layer is not constructed");
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.hasImage: compat context is not bound");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if wanted {
        let (w, h) = scene.layer(id).map_or((1, 1), |l| (l.rect.w, l.rect.h));
        ensure_layer_image(scene, id, w, h);
    } else {
        let removed = scene.layer_mut(id).and_then(|layer| layer.bitmap.take());
        if let Some(bitmap) = removed {
            scene.release_bitmap(bitmap);
        }
        scene.deallocate_province_image(id);
        if let Some(layer) = scene.layer_mut(id) {
            layer.image_modified = true;
        }
    }
    0
}

/// `layer.mainImageBuffer` — the MainImage pixel address (0 when absent).
extern "C" fn layer_main_image_buffer_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.mainImageBuffer: compat context is not bound",
        );
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    set_int(out, scene.main_image_pixel_buffer(id) as i64);
    0
}

/// `layer.mainImageBufferForWrite` — the MainImage pixel address, marking the
/// image modified.
extern "C" fn layer_main_image_buffer_for_write_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.mainImageBufferForWrite: compat context is not bound",
        );
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    set_int(out, scene.main_image_pixel_buffer_for_write(id) as i64);
    0
}

/// `layer.mainImageBufferPitch` — the MainImage row pitch in bytes.
extern "C" fn layer_main_image_buffer_pitch_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.mainImageBufferPitch: compat context is not bound",
        );
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    set_int(out, i64::from(scene.main_image_pitch(id)));
    0
}

/// `layer.provinceImageBuffer` — the province plane address (0 when absent).
extern "C" fn layer_province_image_buffer_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.provinceImageBuffer: compat context is not bound",
        );
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    set_int(out, scene.province_image_pixel_buffer(id) as i64);
    0
}

/// `layer.provinceImageBufferForWrite` — allocate the province plane and
/// return its address.
extern "C" fn layer_province_image_buffer_for_write_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.provinceImageBufferForWrite: compat context is not bound",
        );
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    set_int(out, scene.province_image_pixel_buffer_for_write(id) as i64);
    0
}

/// `layer.provinceImageBufferPitch` — the province plane row pitch.
extern "C" fn layer_province_image_buffer_pitch_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.provinceImageBufferPitch: compat context is not bound",
        );
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    set_int(out, i64::from(scene.province_image_pitch(id)));
    0
}

/// `layer.nodeVisible` / `nodeEnabled` / `nodeFocusable`.
extern "C" fn layer_node_visible_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_node_flag(engine, instance, out, out_error, Scene::node_visible)
}

extern "C" fn layer_node_enabled_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_node_flag(engine, instance, out, out_error, Scene::node_enabled)
}

extern "C" fn layer_node_focusable_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_node_flag(engine, instance, out, out_error, Scene::node_focusable)
}

fn layer_node_flag(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    f: impl Fn(&Scene, u32) -> bool,
) -> c_int {
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        set_bool(out, false);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer: compat context is not bound");
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    set_bool(out, f(scene, id));
    0
}

/// `layer.focused` — whether the layer holds its window's focus.
extern "C" fn layer_focused_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        set_bool(out, false);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.focused: compat context is not bound");
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let focused = scene
        .layer(id)
        .and_then(|layer| scene.window(layer.window))
        .and_then(|window| window.focused_layer)
        == Some(id);
    set_bool(out, focused);
    0
}

/// `layer.isPrimary`.
extern "C" fn layer_is_primary_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        set_bool(out, false);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.isPrimary: compat context is not bound");
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    set_bool(out, scene.is_primary_layer(id));
    0
}

/// `layer.name`.
extern "C" fn layer_name_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.name: compat context is not bound");
    };
    let name = match with_layer_read(engine, instance, |l| l.name.clone()) {
        Ok(name) => name,
        Err(error) => return report_error(out_error, &error),
    };
    set_visual_string_out(&ctx, out, &name);
    0
}

extern "C" fn layer_name_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let text = value_as_string(unsafe { &*value });
    match with_layer_write(engine, instance, unsafe { &*value }, |l, _| l.name = text) {
        Ok(()) => 0,
        Err(error) => report_error(out_error, &error),
    }
}

/// `layer.hint`.
extern "C" fn layer_hint_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.hint: compat context is not bound");
    };
    let hint = match with_layer_read(engine, instance, |l| l.hint.clone()) {
        Ok(hint) => hint,
        Err(error) => return report_error(out_error, &error),
    };
    set_visual_string_out(&ctx, out, &hint);
    0
}

extern "C" fn layer_hint_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let text = value_as_string(unsafe { &*value });
    match with_layer_write(engine, instance, unsafe { &*value }, |l, _| l.hint = text) {
        Ok(()) => 0,
        Err(error) => report_error(out_error, &error),
    }
}

/// `layer.window` — the owning window object.
extern "C" fn layer_window_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.window: compat context is not bound");
    };
    let window = match with_layer_read(engine, instance, |l| l.window) {
        Ok(window) => window,
        Err(error) => return report_error(out_error, &error),
    };
    let object = duplicate_object(&ctx, resolved.engine(), ObjectKind::Window, window);
    set_object_out(resolved.engine(), out, object.as_ref());
    0
}

/// `layer.parent` — the parent layer object, or `null`.
extern "C" fn layer_parent_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.parent: compat context is not bound");
    };
    let parent = match with_layer_read(engine, instance, |l| l.parent) {
        Ok(parent) => parent,
        Err(error) => return report_error(out_error, &error),
    };
    let object = parent
        .and_then(|layer| duplicate_object(&ctx, resolved.engine(), ObjectKind::Layer, layer));
    set_object_out(resolved.engine(), out, object.as_ref());
    0
}

/// `layer.parent = layer` — re-parent (null/void detaches to the window
/// root).
extern "C" fn layer_parent_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_set_parent_id(
        engine,
        instance,
        1,
        value,
        std::ptr::null_mut(),
        out_error,
        std::ptr::null_mut(),
    )
}

/// `layer.children` — an array of the direct child Layer objects.
extern "C" fn layer_children_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.children: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.children: this layer is not constructed");
    };
    let children = {
        let scene = match scene_ref(&ctx) {
            Ok(scene) => scene,
            Err(error) => return report_error(out_error, &error),
        };
        scene.ordered_children(id)
    };
    match layer_array(resolved.engine(), &ctx, &children) {
        Ok(Some(array)) => {
            set_retained_out(resolved.engine(), out, array.raw_id());
            std::mem::forget(array);
        }
        Ok(None) => set_null_out(resolved.engine(), out),
        Err(error) => return report_error(out_error, &error),
    }
    0
}

/// `layer.nextFocusable` / `prevFocusable`.
extern "C" fn layer_next_focusable_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_focusable_neighbor(engine, instance, out, out_error, Scene::next_focusable)
}

extern "C" fn layer_prev_focusable_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_focusable_neighbor(engine, instance, out, out_error, Scene::prev_focusable)
}

fn layer_focusable_neighbor(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    f: impl Fn(&Scene, u32) -> Option<u32>,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer: this layer is not constructed");
    };
    let neighbor = {
        let scene = match scene_ref(&ctx) {
            Ok(scene) => scene,
            Err(error) => return report_error(out_error, &error),
        };
        f(scene, id)
    };
    let object = neighbor
        .and_then(|layer| duplicate_object(&ctx, resolved.engine(), ObjectKind::Layer, layer));
    set_object_out(resolved.engine(), out, object.as_ref());
    0
}

/// `layer.font` — the layer's `Font` object (created lazily and cached).
extern "C" fn layer_font_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.font: compat context is not bound");
    };
    let host = resolved.engine();
    let inst = unsafe { &mut *(instance as *mut LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.font: this layer is not constructed");
    };
    boxed_font_object(host, &ctx, inst, id, out, out_error)
}

/// Look up (or build) the layer's font wrapper.
fn boxed_font_object(
    host: &Tjs2Engine,
    ctx: &CompatCtx,
    inst: &mut LayerInst,
    layer_id: u32,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if let Some(cached) = inst.font_object.as_ref() {
        set_object_out(host, out, Some(cached));
        return 0;
    }
    let font_id = {
        let scene = match scene_mut(ctx) {
            Ok(scene) => scene,
            Err(error) => return report_error(out_error, &error),
        };
        let Some(layer) = scene.layer(layer_id) else {
            return report_error(out_error, "Layer: layer no longer exists");
        };
        match layer.font_id.filter(|id| scene.font(*id).is_some()) {
            Some(id) => id,
            None => {
                let id = scene.add_font(
                    DEFAULT_FONT_FACE.to_string(),
                    DEFAULT_FONT_HEIGHT,
                    argb_to_rgba(DEFAULT_FONT_COLOR),
                );
                if let Some(layer) = scene.layer_mut(layer_id) {
                    layer.font_id = Some(id);
                }
                id
            }
        }
    };
    let wrapper = match host.eval_retained("new Font()", "layer.font") {
        Ok(RetainedValue::Object(object)) => object,
        Ok(RetainedValue::Value(_)) => {
            set_void(out);
            return 0;
        }
        Err(error) => return report_error(out_error, &format!("Layer.font: {error}")),
    };
    if let Err(error) = host.call_member(
        wrapper.raw_id(),
        "__bind",
        &[TjsValue::Integer(i64::from(font_id))],
    ) {
        return report_error(out_error, &format!("Layer.font: {error}"));
    }
    inst.font_object = Some(wrapper);
    set_object_out(host, out, inst.font_object.as_ref());
    0
}

/// `layer.font = font` — copy the source Font's state into the layer's font.
extern "C" fn layer_font_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.font: compat context is not bound");
    };
    let source_id = match resolve_object_id(resolved.engine(), unsafe { &*value }) {
        Ok(id) if id >= 0 => id as u32,
        _ => return 0,
    };
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(layer_id) = inst.id else {
        return report_error(out_error, "Layer.font: this layer is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let Some(source) = scene.font(source_id).cloned() else {
        return 0;
    };
    let destination = match scene
        .layer(layer_id)
        .and_then(|layer| layer.font_id)
        .filter(|id| scene.font(*id).is_some())
    {
        Some(id) => id,
        None => {
            let id = scene.add_font(source.face.clone(), source.height, source.color);
            if let Some(layer) = scene.layer_mut(layer_id) {
                layer.font_id = Some(id);
            }
            id
        }
    };
    if let Some(font) = scene.fonts.iter_mut().find(|font| font.id == destination) {
        *font = FontState {
            id: destination,
            ..source
        };
    }
    0
}

// ---------------------------------------------------------------------------
// Bitmap
// ---------------------------------------------------------------------------

extern "C" fn bitmap_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(BitmapInst::default())) as *mut c_void
}

extern "C" fn bitmap_destroy(engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    let inst = unsafe { &*(instance as *const BitmapInst) };
    if let (Some(id), Ok((_resolved, ctx))) = (inst.id, bound(engine)) {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        unsafe { ctx.services() }
            .state::<VisualState>()
            .objects
            .bitmaps
            .remove(&id);
    }
    // SAFETY: instance came from `bitmap_create`'s `Box::into_raw` and is
    // freed exactly once by the VM.
    drop(unsafe { Box::from_raw(instance as *mut BitmapInst) });
}

/// `Bitmap([width, height])` — create a bitmap and return its id.
extern "C" fn bitmap_ctor(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Bitmap: compat context is not bound");
    };
    let a = unsafe { args(argv, argc) };
    let width = a.first().map(value_as_i64).unwrap_or(1).max(1) as u32;
    let height = a.get(1).map(value_as_i64).unwrap_or(1).max(1) as u32;
    let inst = unsafe { &mut *(instance as *mut BitmapInst) };
    if inst.id.is_some() {
        return report_error(out_error, "Bitmap: this bitmap is already constructed");
    }
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let Some(pixels) = (width as usize)
        .checked_mul(height as usize)
        .and_then(|count| count.checked_mul(4))
    else {
        return report_error(out_error, "Bitmap: the requested size is too large");
    };
    let id = scene.add_bitmap(width, height, vec![0u8; pixels]);
    if let Some(bitmap) = scene.bitmap_mut(id) {
        bitmap.script_owned = true;
    }
    inst.id = Some(id);
    register_bitmap(&ctx, resolved.engine(), id, objthis);
    set_int(out, i64::from(id));
    0
}

/// `Bitmap.getPixel(x, y)` → ARGB.
extern "C" fn bitmap_get_pixel(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Bitmap.getPixel requires 2 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Bitmap.getPixel: compat context is not bound");
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    set_int(out, read_bitmap_pixel(scene, id, x, y));
    0
}

/// `Bitmap.setPixel(x, y, color)`.
extern "C" fn bitmap_set_pixel(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 3 {
        return report_error(out_error, "Bitmap.setPixel requires 3 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let color = argb_to_rgba(value_as_i64(&a[2]));
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Bitmap.setPixel: compat context is not bound");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    write_bitmap_pixel(scene, id, x, y, color, false);
    set_void(out);
    0
}

/// `Bitmap.getMaskPixel(x, y)` → alpha.
extern "C" fn bitmap_get_mask_pixel(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Bitmap.getMaskPixel requires 2 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Bitmap.getMaskPixel: compat context is not bound",
        );
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    set_int(out, (read_bitmap_pixel(scene, id, x, y) >> 24) & 0xff);
    0
}

/// `Bitmap.setMaskPixel(x, y, alpha)`.
extern "C" fn bitmap_set_mask_pixel(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 3 {
        return report_error(out_error, "Bitmap.setMaskPixel requires 3 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let alpha = value_as_i64(&a[2]).clamp(0, 255) as u8;
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Bitmap.setMaskPixel: compat context is not bound",
        );
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if let Some(bitmap) = scene.bitmap_mut(id)
        && let Some(offset) = bitmap.pixel_offset(x as u32, y as u32)
    {
        bitmap.rgba[offset + 3] = alpha;
        bitmap.mark_dirty();
    }
    set_void(out);
    0
}

/// Read one bitmap pixel as ARGB.
fn read_bitmap_pixel(scene: &Scene, id: u32, x: i32, y: i32) -> i64 {
    let Some(bitmap) = scene.bitmap(id) else {
        return 0;
    };
    if x < 0 || y < 0 {
        return 0;
    }
    match bitmap.pixel_offset(x as u32, y as u32) {
        Some(offset) => rgba_to_argb([
            bitmap.rgba[offset],
            bitmap.rgba[offset + 1],
            bitmap.rgba[offset + 2],
            bitmap.rgba[offset + 3],
        ]),
        None => 0,
    }
}

/// Write one bitmap pixel. `hold_alpha` keeps the destination alpha.
fn write_bitmap_pixel(
    scene: &mut Scene,
    id: u32,
    x: i32,
    y: i32,
    color: [u8; 4],
    hold_alpha: bool,
) {
    if x < 0 || y < 0 {
        return;
    }
    let Some(bitmap) = scene.bitmap_mut(id) else {
        return;
    };
    if let Some(offset) = bitmap.pixel_offset(x as u32, y as u32) {
        if hold_alpha {
            bitmap.rgba[offset] = color[0];
            bitmap.rgba[offset + 1] = color[1];
            bitmap.rgba[offset + 2] = color[2];
        } else {
            bitmap.rgba[offset..offset + 4].copy_from_slice(&color);
        }
        bitmap.mark_dirty();
    }
}

/// `Bitmap.setSize(width, height)`.
extern "C" fn bitmap_set_size(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Bitmap.setSize requires 2 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let width = value_as_i64(&a[0]).max(0) as u32;
    let height = value_as_i64(&a[1]).max(0) as u32;
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Bitmap.setSize: this bitmap is not constructed");
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Bitmap.setSize: compat context is not bound");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    scene.resize_bitmap(id, width, height, [0, 0, 0, 0]);
    set_void(out);
    0
}

/// `Bitmap.copyFrom(bitmap)` — copy the source pixels into this bitmap.
extern "C" fn bitmap_copy_from(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Bitmap.copyFrom requires a Bitmap argument");
    }
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Bitmap.copyFrom: compat context is not bound");
    };
    let source_id = match resolve_object_id(resolved.engine(), &unsafe { args(argv, argc) }[0]) {
        Ok(id) if id >= 0 => id as u32,
        Ok(_) => return report_error(out_error, "Bitmap.copyFrom: not a Bitmap object"),
        Err(error) => return report_error(out_error, &error),
    };
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Bitmap.copyFrom: this bitmap is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let Some((width, height, rgba)) = scene
        .bitmap(source_id)
        .map(|b| (b.width, b.height, b.rgba.clone()))
    else {
        return report_error(
            out_error,
            "Bitmap.copyFrom: the source bitmap does not exist",
        );
    };
    scene.resize_bitmap(id, width, height, [0, 0, 0, 0]);
    if let Some(bitmap) = scene.bitmap_mut(id) {
        bitmap.rgba = rgba;
        bitmap.width = width;
        bitmap.height = height;
        bitmap.mark_dirty();
    }
    set_void(out);
    0
}

/// `Bitmap.independ()` — ensure the bitmap owns its pixels. Each `Bitmap`
/// object already has a private scene bitmap, so this is a no-op.
extern "C" fn bitmap_independ(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void(out);
    0
}

// --- Bitmap properties -----------------------------------------------------

extern "C" fn bitmap_id_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const BitmapInst) };
    set_int(out, inst.id.map_or(0, i64::from));
    0
}

/// `bitmap.width`.
extern "C" fn bitmap_width_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    bitmap_dimension_get(engine, instance, out, out_error, true)
}

/// `bitmap.height`.
extern "C" fn bitmap_height_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    bitmap_dimension_get(engine, instance, out, out_error, false)
}

/// `bitmap.width = n` — resize (preserving the overlapping region).
extern "C" fn bitmap_width_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    bitmap_dimension_set(engine, instance, value, out_error, true)
}

/// `bitmap.height = n` — resize (preserving the overlapping region).
extern "C" fn bitmap_height_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    bitmap_dimension_set(engine, instance, value, out_error, false)
}

fn bitmap_dimension_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    width: bool,
) -> c_int {
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Bitmap: compat context is not bound");
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let Some(bitmap) = scene.bitmap(id) else {
        return report_error(out_error, "Bitmap: bitmap no longer exists");
    };
    set_int(
        out,
        i64::from(if width { bitmap.width } else { bitmap.height }),
    );
    0
}

fn bitmap_dimension_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    width: bool,
) -> c_int {
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Bitmap: compat context is not bound");
    };
    let requested = value_as_i64(unsafe { &*value }).max(0) as u32;
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let Some((current_width, current_height)) = scene.bitmap(id).map(|b| (b.width, b.height))
    else {
        return report_error(out_error, "Bitmap: bitmap no longer exists");
    };
    let (new_width, new_height) = if width {
        (requested, current_height)
    } else {
        (current_width, requested)
    };
    scene.resize_bitmap(id, new_width, new_height, [0, 0, 0, 0]);
    0
}

/// `bitmap.buffer` — the pixel buffer address (0 when absent).
extern "C" fn bitmap_buffer_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Bitmap.buffer: compat context is not bound");
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    set_int(out, scene.bitmap(id).map_or(0, |b| b.rgba.as_ptr() as i64));
    0
}

/// `bitmap.bufferForWrite` — the pixel buffer address, marking the bitmap
/// dirty.
extern "C" fn bitmap_buffer_for_write_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Bitmap.bufferForWrite: compat context is not bound",
        );
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let address = match scene.bitmap_mut(id) {
        Some(bitmap) => {
            bitmap.mark_dirty();
            bitmap.rgba.as_mut_ptr() as i64
        }
        None => 0,
    };
    set_int(out, address);
    0
}

/// `bitmap.bufferPitch` — the row pitch in bytes.
extern "C" fn bitmap_buffer_pitch_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const BitmapInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Bitmap.bufferPitch: compat context is not bound");
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    set_int(
        out,
        scene
            .bitmap(id)
            .map_or(0, |b| i64::from(b.width.saturating_mul(4))),
    );
    0
}

/// `bitmap.loading`.
extern "C" fn bitmap_loading_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const BitmapInst) };
    set_bool(out, inst.loading);
    0
}

/// `Bitmap.onLoaded()` — the (unused) async load completion hook.
extern "C" fn bitmap_on_loaded(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// Font
// ---------------------------------------------------------------------------

extern "C" fn font_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(FontInst::default())) as *mut c_void
}

extern "C" fn font_destroy(engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    let inst = unsafe { &mut *(instance as *mut FontInst) };
    if let (Some(id), Ok((_resolved, ctx))) = (inst.id, bound(engine)) {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let services = unsafe { ctx.services() };
        services.state::<VisualState>().objects.fonts.remove(&id);
        if inst.owns_state {
            let scene = services.scene();
            if let Some(scene) = scene {
                // SAFETY: the scene is live for the binding's lifetime.
                let scene = unsafe { &mut *scene };
                scene.fonts.retain(|font| font.id != id);
            }
        }
    }
    // SAFETY: instance came from `font_create`'s `Box::into_raw` and is freed
    // exactly once by the VM.
    drop(unsafe { Box::from_raw(instance as *mut FontInst) });
}

/// `Font([face, height, color])` — create a font and return its id.
extern "C" fn font_ctor(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Font: compat context is not bound");
    };
    let a = unsafe { args(argv, argc) };
    let face = a
        .first()
        .filter(|v| v.ty == tjs::VAL_STRING)
        .map(value_as_string)
        .unwrap_or_else(|| DEFAULT_FONT_FACE.to_string());
    let height = a
        .get(1)
        .map(value_as_i64)
        .unwrap_or(i64::from(DEFAULT_FONT_HEIGHT)) as i32;
    let color = a.get(2).map(value_as_i64).unwrap_or(DEFAULT_FONT_COLOR);
    let inst = unsafe { &mut *(instance as *mut FontInst) };
    if inst.id.is_some() {
        return report_error(out_error, "Font: this font is already constructed");
    }
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let id = scene.add_font(face, height, argb_to_rgba(color));
    inst.id = Some(id);
    inst.owns_state = true;
    register_font(&ctx, resolved.engine(), id, objthis);
    set_int(out, i64::from(id));
    0
}

/// `Font.__bind(id)` — repoint this wrapper at a layer-owned font state.
extern "C" fn font_bind(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Font.__bind requires a font id");
    }
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Font.__bind: compat context is not bound");
    };
    let id = value_as_i64(&unsafe { args(argv, argc) }[0]).max(0) as u32;
    let inst = unsafe { &mut *(instance as *mut FontInst) };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if scene.font(id).is_none() {
        return report_error(out_error, "Font.__bind: font id does not exist");
    }
    if inst.owns_state
        && let Some(old) = inst.id.take()
        && old != id
    {
        scene.fonts.retain(|font| font.id != old);
        // SAFETY: the caller resolved `ctx` from the installed binding.
        unsafe { ctx.services() }
            .state::<VisualState>()
            .objects
            .fonts
            .remove(&old);
    }
    inst.id = Some(id);
    inst.owns_state = false;
    register_font(&ctx, resolved.engine(), id, objthis);
    set_void(out);
    0
}

// --- Font properties -------------------------------------------------------

/// Read a font field from the scene.
fn with_font_read<R>(
    engine: *mut c_void,
    instance: *mut c_void,
    f: impl FnOnce(&FontState) -> R,
) -> Result<R, String> {
    let inst = unsafe { &*(instance as *const FontInst) };
    let id = inst
        .id
        .ok_or_else(|| "Font: this font is not constructed".to_string())?;
    let (_resolved, ctx) = bound(engine)?;
    let scene = scene_ref(&ctx)?;
    scene
        .font(id)
        .map(f)
        .ok_or_else(|| "Font: font no longer exists".to_string())
}

/// Mutate a font field in the scene.
fn with_font_write(
    engine: *mut c_void,
    instance: *mut c_void,
    value: &Value,
    f: impl FnOnce(&mut FontState, &Value),
) -> Result<(), String> {
    let inst = unsafe { &*(instance as *const FontInst) };
    let id = inst
        .id
        .ok_or_else(|| "Font: this font is not constructed".to_string())?;
    let (_resolved, ctx) = bound(engine)?;
    let scene = scene_mut(&ctx)?;
    let font = scene
        .fonts
        .iter_mut()
        .find(|font| font.id == id)
        .ok_or_else(|| "Font: font no longer exists".to_string())?;
    f(font, value);
    Ok(())
}

macro_rules! font_int_prop {
    ($get:ident, $set:ident, $getter:expr, $setter:expr) => {
        #[allow(dead_code)]
        extern "C" fn $get(
            engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            match with_font_read(engine, instance, $getter) {
                Ok(value) => {
                    set_int(out, value);
                    0
                }
                Err(error) => report_error(out_error, &error),
            }
        }
        #[allow(dead_code)]
        extern "C" fn $set(
            engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            let value = unsafe { &*value };
            match with_font_write(engine, instance, value, $setter) {
                Ok(()) => 0,
                Err(error) => report_error(out_error, &error),
            }
        }
    };
}

font_int_prop!(
    font_id_get,
    font_id_set,
    |f: &FontState| i64::from(f.id),
    |_f: &mut FontState, _v: &Value| {}
);
font_int_prop!(
    font_height_get,
    font_height_set,
    |f: &FontState| i64::from(f.height),
    |f: &mut FontState, v: &Value| f.height = value_as_i64(v) as i32
);
font_int_prop!(
    font_color_get,
    font_color_set,
    |f: &FontState| rgba_to_argb(f.color),
    |f: &mut FontState, v: &Value| f.color = argb_to_rgba(value_as_i64(v))
);
font_int_prop!(
    font_bold_get,
    font_bold_set,
    |f: &FontState| i64::from(f.bold),
    |f: &mut FontState, v: &Value| f.bold = value_as_bool(v)
);
font_int_prop!(
    font_italic_get,
    font_italic_set,
    |f: &FontState| i64::from(f.italic),
    |f: &mut FontState, v: &Value| f.italic = value_as_bool(v)
);
font_int_prop!(
    font_strikeout_get,
    font_strikeout_set,
    |f: &FontState| i64::from(f.strikeout),
    |f: &mut FontState, v: &Value| f.strikeout = value_as_bool(v)
);
font_int_prop!(
    font_underline_get,
    font_underline_set,
    |f: &FontState| i64::from(f.underline),
    |f: &mut FontState, v: &Value| f.underline = value_as_bool(v)
);
font_int_prop!(
    font_angle_get,
    font_angle_set,
    |f: &FontState| f.angle.round() as i64,
    |f: &mut FontState, v: &Value| f.angle = value_as_i64(v) as f64
);
font_int_prop!(
    font_face_is_file_name_get,
    font_face_is_file_name_set,
    |_f: &FontState| 0,
    |_f: &mut FontState, _v: &Value| {}
);

/// `font.face`.
extern "C" fn font_face_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Font.face: compat context is not bound");
    };
    let face = match with_font_read(engine, instance, |f| f.face.clone()) {
        Ok(face) => face,
        Err(error) => return report_error(out_error, &error),
    };
    set_visual_string_out(&ctx, out, &face);
    0
}

extern "C" fn font_face_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let text = value_as_string(unsafe { &*value });
    match with_font_write(engine, instance, unsafe { &*value }, |f, _| f.face = text) {
        Ok(()) => 0,
        Err(error) => report_error(out_error, &error),
    }
}

// ---------------------------------------------------------------------------
// Timer
// ---------------------------------------------------------------------------

extern "C" fn timer_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(TimerInst::default())) as *mut c_void
}

extern "C" fn timer_destroy(engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    let inst = unsafe { &*(instance as *const TimerInst) };
    if let (Some(id), Ok((_resolved, ctx))) = (inst.id, bound(engine)) {
        // SAFETY: the caller resolved `ctx` from the installed binding. The
        // dropped `TimerEntry` releases the callback and owner.
        unsafe { ctx.services() }
            .state::<VisualState>()
            .timers
            .remove(&id);
    }
    // SAFETY: instance came from `timer_create`'s `Box::into_raw` and is
    // freed exactly once by the VM.
    drop(unsafe { Box::from_raw(instance as *mut TimerInst) });
}

/// `Timer(callback[, actionName])` — retain the callback and the timer
/// object. The action name is accepted and ignored.
extern "C" fn timer_ctor(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Timer: constructor requires a callback");
    }
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Timer: compat context is not bound");
    };
    let host = resolved.engine();
    let callback = match host.retain_object_arg(&unsafe { args(argv, argc) }[0]) {
        Ok(callback) => callback,
        Err(error) => return report_error(out_error, &format!("Timer: {error}")),
    };
    let owner = host.retain_object_detached(objthis).ok();
    let inst = unsafe { &mut *(instance as *mut TimerInst) };
    if inst.id.is_some() {
        return report_error(out_error, "Timer: already constructed");
    }
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let mut state = unsafe { ctx.services() }.state::<VisualState>();
    let id = state.next_timer;
    state.next_timer += 1;
    state.timers.insert(
        id,
        TimerEntry {
            callback: Some(callback),
            owner,
            interval_ms: 1000,
            enabled: false,
            next_fire_ms: 0,
            count: 0,
            capacity: 1,
            mode: 0,
        },
    );
    drop(state);
    inst.id = Some(id);
    set_int(out, i64::from(id));
    0
}

/// The native `onTimer` method: invoke the constructor callback. A script
/// subclass that overrides `onTimer` never reaches here.
extern "C" fn timer_on_timer(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Timer.onTimer: compat context is not bound");
    };
    let host = resolved.engine();
    let inst = unsafe { &*(instance as *const TimerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Timer.onTimer: this timer is not constructed");
    };
    // Take the callback out so the state borrow is released across the call
    // (the callback may register/disable timers).
    let callback = {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let mut state = unsafe { ctx.services() }.state::<VisualState>();
        state
            .timers
            .get_mut(&id)
            .and_then(|timer| timer.callback.take())
    };
    let Some(callback) = callback else {
        set_void(out);
        return 0;
    };
    let result = host.call_detached(&callback, &[]);
    {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let mut state = unsafe { ctx.services() }.state::<VisualState>();
        match state.timers.get_mut(&id) {
            Some(timer) => timer.callback = Some(callback),
            None => drop(callback),
        }
    }
    if let Err(error) = result {
        log::warn!("Timer {id}: callback failed ({error}); disabling");
        // SAFETY: the caller resolved `ctx` from the installed binding.
        if let Some(timer) = unsafe { ctx.services() }
            .state::<VisualState>()
            .timers
            .get_mut(&id)
        {
            timer.enabled = false;
        }
    }
    set_void(out);
    0
}

/// `timer.interval` (ms).
extern "C" fn timer_interval_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Timer.interval: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const TimerInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let state = unsafe { ctx.services() }.state::<VisualState>();
    let interval = state.timers.get(&id).map_or(0, |timer| timer.interval_ms);
    drop(state);
    set_int(out, interval as i64);
    0
}

extern "C" fn timer_interval_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Timer.interval: compat context is not bound");
    };
    let interval = value_as_i64(unsafe { &*value }).max(1) as u64;
    let inst = unsafe { &*(instance as *const TimerInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let mut state = unsafe { ctx.services() }.state::<VisualState>();
    let last = state.last_now_ms;
    if let Some(timer) = state.timers.get_mut(&id) {
        timer.interval_ms = interval;
        if timer.enabled {
            timer.next_fire_ms = last.saturating_add(interval);
        }
    }
    0
}

/// `timer.enabled`.
extern "C" fn timer_enabled_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Timer.enabled: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const TimerInst) };
    let Some(id) = inst.id else {
        set_bool(out, false);
        return 0;
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let state = unsafe { ctx.services() }.state::<VisualState>();
    let enabled = state.timers.get(&id).is_some_and(|timer| timer.enabled);
    drop(state);
    set_bool(out, enabled);
    0
}

extern "C" fn timer_enabled_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Timer.enabled: compat context is not bound");
    };
    let enabled = value_as_bool(unsafe { &*value });
    let inst = unsafe { &*(instance as *const TimerInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let mut state = unsafe { ctx.services() }.state::<VisualState>();
    let last = state.last_now_ms;
    if let Some(timer) = state.timers.get_mut(&id) {
        timer.enabled = enabled;
        if enabled {
            // The reference reschedules on enable: fire at now + interval.
            timer.next_fire_ms = last.saturating_add(timer.interval_ms);
        }
    }
    0
}

/// `timer.capacity`.
extern "C" fn timer_capacity_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Timer.capacity: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const TimerInst) };
    let Some(id) = inst.id else {
        set_int(out, 1);
        return 0;
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let state = unsafe { ctx.services() }.state::<VisualState>();
    let capacity = state.timers.get(&id).map_or(1, |timer| timer.capacity);
    drop(state);
    set_int(out, capacity);
    0
}

extern "C" fn timer_capacity_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Timer.capacity: compat context is not bound");
    };
    let capacity = value_as_i64(unsafe { &*value });
    let inst = unsafe { &*(instance as *const TimerInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    if let Some(timer) = unsafe { ctx.services() }
        .state::<VisualState>()
        .timers
        .get_mut(&id)
    {
        timer.capacity = capacity;
    }
    0
}

/// `timer.count`.
extern "C" fn timer_count_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Timer.count: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const TimerInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let state = unsafe { ctx.services() }.state::<VisualState>();
    let count = state.timers.get(&id).map_or(0, |timer| timer.count);
    drop(state);
    set_int(out, count as i64);
    0
}

/// `timer.mode`.
extern "C" fn timer_mode_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Timer.mode: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const TimerInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let state = unsafe { ctx.services() }.state::<VisualState>();
    let mode = state.timers.get(&id).map_or(0, |timer| timer.mode);
    drop(state);
    set_int(out, mode);
    0
}

extern "C" fn timer_mode_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Timer.mode: compat context is not bound");
    };
    let mode = value_as_i64(unsafe { &*value });
    let inst = unsafe { &*(instance as *const TimerInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    if let Some(timer) = unsafe { ctx.services() }
        .state::<VisualState>()
        .timers
        .get_mut(&id)
    {
        timer.mode = mode;
    }
    0
}

/// Fire every due timer (`FrameBody::timer_poll`). Returns whether any timer
/// remains enabled (the `system::continuous_handler_poll` convention).
#[allow(dead_code)]
pub fn timer_poll(ctx: &mut CompatCtx) -> Result<bool, CompatError> {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let host = unsafe { ctx.engine() };
    let services = unsafe { ctx.services() };
    let now = {
        let mut state = services.state::<VisualState>();
        state.last_now_ms = state.now_ms();
        state.last_now_ms
    };
    // Snapshot the due timer ids, then fire one at a time so a callback that
    // registers/disables timers is safe.
    let due: Vec<u32> = {
        let state = services.state::<VisualState>();
        state
            .timers
            .iter()
            .filter(|(_, timer)| timer.enabled && timer.next_fire_ms <= now)
            .map(|(&id, _)| id)
            .collect()
    };
    for id in due {
        // Reschedule and count under a short borrow.
        let (owner, interval) = {
            let mut state = services.state::<VisualState>();
            match state.timers.get_mut(&id) {
                Some(timer) if timer.enabled && timer.next_fire_ms <= now => {
                    timer.next_fire_ms = now.saturating_add(timer.interval_ms);
                    timer.count += 1;
                    (
                        timer.owner.as_ref().map(DetachedValue::raw_id),
                        timer.interval_ms,
                    )
                }
                _ => continue,
            }
        };
        // Dispatch the object's `onTimer` member (a script subclass override
        // wins); fall back to the callback when the owner could not be
        // retained.
        let result = match owner {
            Some(owner) => host.call_member(owner, "onTimer", &[]),
            None => {
                let callback = {
                    let mut state = services.state::<VisualState>();
                    state
                        .timers
                        .get_mut(&id)
                        .and_then(|timer| timer.callback.take())
                };
                match callback {
                    Some(callback) => {
                        let result = host.call_detached(&callback, &[]);
                        let mut state = services.state::<VisualState>();
                        match state.timers.get_mut(&id) {
                            Some(timer) => timer.callback = Some(callback),
                            None => drop(callback),
                        }
                        result
                    }
                    None => continue,
                }
            }
        };
        if let Err(error) = result {
            log::warn!("Timer {id}: onTimer failed ({error}) after {interval} ms; disabling");
            if let Some(timer) = services.state::<VisualState>().timers.get_mut(&id) {
                timer.enabled = false;
            }
        }
    }
    let state = services.state::<VisualState>();
    Ok(state.timers.values().any(|timer| timer.enabled))
}

/// Drain the queued `Window.bringToFront` requests (`(window_id)`).
#[allow(dead_code)]
pub fn take_bring_to_front_requests(services: &crate::CompatServices) -> Vec<u32> {
    std::mem::take(&mut services.state::<VisualState>().bring_to_front)
}

/// Drain the queued `Window.update` requests (`(window_id, update_type)`).
#[allow(dead_code)]
pub fn take_update_requests(services: &crate::CompatServices) -> Vec<(u32, i32)> {
    std::mem::take(&mut services.state::<VisualState>().update_requests)
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the visual classes.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    register_window_class(ctx)?;
    register_layer_class(ctx)?;
    register_bitmap_class(ctx)?;
    register_font_class(ctx)?;
    register_timer_class(ctx)?;
    Ok(())
}

fn register_window_class(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "Window",
        create: window_create,
        destroy: window_destroy,
        invalidate: None,
        methods: vec![
            NativeInstanceMethodDef {
                name: "Window",
                f: window_ctor,
            },
            NativeInstanceMethodDef {
                name: "close",
                f: window_close,
            },
            NativeInstanceMethodDef {
                name: "bringToFront",
                f: window_bring_to_front,
            },
            NativeInstanceMethodDef {
                name: "update",
                f: window_update,
            },
            NativeInstanceMethodDef {
                name: "add",
                f: window_add,
            },
            NativeInstanceMethodDef {
                name: "remove",
                f: window_remove,
            },
            NativeInstanceMethodDef {
                name: "setSize",
                f: window_set_size,
            },
            NativeInstanceMethodDef {
                name: "setInnerSize",
                f: window_set_inner_size_method,
            },
            NativeInstanceMethodDef {
                name: "setPos",
                f: window_set_pos,
            },
            NativeInstanceMethodDef {
                name: "setMinSize",
                f: window_set_min_size,
            },
            NativeInstanceMethodDef {
                name: "setMaxSize",
                f: window_set_max_size,
            },
            NativeInstanceMethodDef {
                name: "setZoom",
                f: window_set_zoom,
            },
            NativeInstanceMethodDef {
                name: "showModal",
                f: window_show_modal,
            },
            NativeInstanceMethodDef {
                name: "setMaskRegion",
                f: window_set_mask_region,
            },
            NativeInstanceMethodDef {
                name: "removeMaskRegion",
                f: window_remove_mask_region,
            },
            NativeInstanceMethodDef {
                name: "hideMouseCursor",
                f: window_hide_mouse_cursor,
            },
            NativeInstanceMethodDef {
                name: "changeScreenMode",
                f: window_change_screen_mode,
            },
            NativeInstanceMethodDef {
                name: "postInputEvent",
                f: window_post_input_event,
            },
        ],
        properties: vec![
            NativeInstancePropertyDef {
                name: "id",
                get: Some(window_id_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "visible",
                get: Some(window_visible_get),
                set: Some(window_visible_set),
            },
            NativeInstancePropertyDef {
                name: "caption",
                get: Some(window_caption_get),
                set: Some(window_caption_set),
            },
            NativeInstancePropertyDef {
                name: "width",
                get: Some(window_width_get),
                set: Some(window_width_set),
            },
            NativeInstancePropertyDef {
                name: "height",
                get: Some(window_height_get),
                set: Some(window_height_set),
            },
            NativeInstancePropertyDef {
                name: "innerWidth",
                get: Some(window_width_get),
                set: Some(window_width_set),
            },
            NativeInstancePropertyDef {
                name: "innerHeight",
                get: Some(window_height_get),
                set: Some(window_height_set),
            },
            NativeInstancePropertyDef {
                name: "left",
                get: Some(window_left_get),
                set: Some(window_left_set),
            },
            NativeInstancePropertyDef {
                name: "top",
                get: Some(window_top_get),
                set: Some(window_top_set),
            },
            NativeInstancePropertyDef {
                name: "minWidth",
                get: Some(window_min_width_get),
                set: Some(window_min_width_set),
            },
            NativeInstancePropertyDef {
                name: "minHeight",
                get: Some(window_min_height_get),
                set: Some(window_min_height_set),
            },
            NativeInstancePropertyDef {
                name: "maxWidth",
                get: Some(window_max_width_get),
                set: Some(window_max_width_set),
            },
            NativeInstancePropertyDef {
                name: "maxHeight",
                get: Some(window_max_height_get),
                set: Some(window_max_height_set),
            },
            NativeInstancePropertyDef {
                name: "focusable",
                get: Some(window_focusable_get),
                set: Some(window_focusable_set),
            },
            NativeInstancePropertyDef {
                name: "zoomNumer",
                get: Some(window_zoom_numer_get),
                set: Some(window_zoom_numer_set),
            },
            NativeInstancePropertyDef {
                name: "zoomDenom",
                get: Some(window_zoom_denom_get),
                set: Some(window_zoom_denom_set),
            },
            NativeInstancePropertyDef {
                name: "fullScreen",
                get: Some(window_full_screen_get),
                set: Some(window_full_screen_set),
            },
            NativeInstancePropertyDef {
                name: "waitVSync",
                get: Some(window_wait_vsync_get),
                set: Some(window_wait_vsync_set),
            },
            NativeInstancePropertyDef {
                name: "enableTouch",
                get: Some(window_enable_touch_get),
                set: Some(window_enable_touch_set),
            },
            NativeInstancePropertyDef {
                name: "hintDelay",
                get: Some(window_hint_delay_get),
                set: Some(window_hint_delay_set),
            },
            NativeInstancePropertyDef {
                name: "mouseCursorState",
                get: Some(window_mouse_cursor_state_get),
                set: Some(window_mouse_cursor_state_set),
            },
            NativeInstancePropertyDef {
                name: "imeMode",
                get: Some(window_ime_mode_get),
                set: Some(window_ime_mode_set),
            },
            NativeInstancePropertyDef {
                name: "useMouseKey",
                get: Some(window_use_mouse_key_get),
                set: Some(window_use_mouse_key_set),
            },
            NativeInstancePropertyDef {
                name: "trapKey",
                get: Some(window_trap_key_get),
                set: Some(window_trap_key_set),
            },
            NativeInstancePropertyDef {
                name: "mainWindow",
                get: Some(window_main_window_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "focusedLayer",
                get: Some(window_focused_layer_get),
                set: Some(window_focused_layer_set),
            },
            NativeInstancePropertyDef {
                name: "primaryLayer",
                get: Some(window_primary_layer_get),
                set: None,
            },
        ],
    })
}

fn register_layer_class(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "Layer",
        create: layer_create,
        destroy: layer_destroy,
        invalidate: None,
        methods: vec![
            NativeInstanceMethodDef {
                name: "Layer",
                f: layer_ctor,
            },
            NativeInstanceMethodDef {
                name: "setPos",
                f: layer_set_pos,
            },
            NativeInstanceMethodDef {
                name: "setSize",
                f: layer_set_size,
            },
            NativeInstanceMethodDef {
                name: "setSizeToImageSize",
                f: layer_set_size_to_image_size,
            },
            NativeInstanceMethodDef {
                name: "setImagePos",
                f: layer_set_image_pos,
            },
            NativeInstanceMethodDef {
                name: "setImageSize",
                f: layer_set_image_size,
            },
            NativeInstanceMethodDef {
                name: "setBitmap",
                f: layer_set_bitmap,
            },
            NativeInstanceMethodDef {
                name: "setImage",
                f: layer_set_image,
            },
            NativeInstanceMethodDef {
                name: "assignImages",
                f: layer_assign_images,
            },
            NativeInstanceMethodDef {
                name: "copyFromBitmapToMainImage",
                f: layer_copy_from_bitmap_to_main_image,
            },
            NativeInstanceMethodDef {
                name: "independMainImage",
                f: layer_independ_main_image,
            },
            NativeInstanceMethodDef {
                name: "bringToFront",
                f: layer_bring_to_front,
            },
            NativeInstanceMethodDef {
                name: "moveToFront",
                f: layer_bring_to_front,
            },
            NativeInstanceMethodDef {
                name: "setParentId",
                f: layer_set_parent_id,
            },
            NativeInstanceMethodDef {
                name: "focus",
                f: layer_focus,
            },
            NativeInstanceMethodDef {
                name: "update",
                f: layer_update,
            },
            NativeInstanceMethodDef {
                name: "setClip",
                f: layer_set_clip,
            },
            NativeInstanceMethodDef {
                name: "getProvincePixel",
                f: layer_get_province_pixel,
            },
            NativeInstanceMethodDef {
                name: "setProvincePixel",
                f: layer_set_province_pixel,
            },
            NativeInstanceMethodDef {
                name: "getLayerAt",
                f: layer_get_layer_at,
            },
            NativeInstanceMethodDef {
                name: "fillRect",
                f: layer_fill_rect,
            },
        ],
        properties: vec![
            NativeInstancePropertyDef {
                name: "id",
                get: Some(layer_id_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "nativeId",
                get: Some(layer_id_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "font",
                get: Some(layer_font_get),
                set: Some(layer_font_set),
            },
            NativeInstancePropertyDef {
                name: "visible",
                get: Some(layer_visible_get),
                set: Some(layer_visible_set),
            },
            NativeInstancePropertyDef {
                name: "opacity",
                get: Some(layer_opacity_get),
                set: Some(layer_opacity_set),
            },
            NativeInstancePropertyDef {
                name: "width",
                get: Some(layer_width_get),
                set: Some(layer_width_set),
            },
            NativeInstancePropertyDef {
                name: "height",
                get: Some(layer_height_get),
                set: Some(layer_height_set),
            },
            NativeInstancePropertyDef {
                name: "left",
                get: Some(layer_left_get),
                set: Some(layer_left_set),
            },
            NativeInstancePropertyDef {
                name: "top",
                get: Some(layer_top_get),
                set: Some(layer_top_set),
            },
            NativeInstancePropertyDef {
                name: "type",
                get: Some(layer_type_get),
                set: Some(layer_type_set),
            },
            NativeInstancePropertyDef {
                name: "hasImage",
                get: Some(layer_has_image_get),
                set: Some(layer_has_image_set),
            },
            NativeInstancePropertyDef {
                name: "mainImageBuffer",
                get: Some(layer_main_image_buffer_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "mainImageBufferForWrite",
                get: Some(layer_main_image_buffer_for_write_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "mainImageBufferPitch",
                get: Some(layer_main_image_buffer_pitch_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "provinceImageBuffer",
                get: Some(layer_province_image_buffer_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "provinceImageBufferForWrite",
                get: Some(layer_province_image_buffer_for_write_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "provinceImageBufferPitch",
                get: Some(layer_province_image_buffer_pitch_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "absolute",
                get: Some(layer_absolute_get),
                set: Some(layer_absolute_set),
            },
            NativeInstancePropertyDef {
                name: "order",
                get: Some(layer_absolute_get),
                set: Some(layer_absolute_set),
            },
            NativeInstancePropertyDef {
                name: "hitThreshold",
                get: Some(layer_hit_threshold_get),
                set: Some(layer_hit_threshold_set),
            },
            NativeInstancePropertyDef {
                name: "hitType",
                get: Some(layer_hit_type_get),
                set: Some(layer_hit_type_set),
            },
            NativeInstancePropertyDef {
                name: "cursor",
                get: Some(layer_cursor_get),
                set: Some(layer_cursor_set),
            },
            NativeInstancePropertyDef {
                name: "face",
                get: Some(layer_face_get),
                set: Some(layer_face_set),
            },
            NativeInstancePropertyDef {
                name: "holdAlpha",
                get: Some(layer_hold_alpha_get),
                set: Some(layer_hold_alpha_set),
            },
            NativeInstancePropertyDef {
                name: "imageLeft",
                get: Some(layer_image_left_get),
                set: Some(layer_image_left_set),
            },
            NativeInstancePropertyDef {
                name: "imageTop",
                get: Some(layer_image_top_get),
                set: Some(layer_image_top_set),
            },
            NativeInstancePropertyDef {
                name: "imageWidth",
                get: Some(layer_image_width_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "imageHeight",
                get: Some(layer_image_height_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "window",
                get: Some(layer_window_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "parent",
                get: Some(layer_parent_get),
                set: Some(layer_parent_set),
            },
            NativeInstancePropertyDef {
                name: "focusable",
                get: Some(layer_focusable_get),
                set: Some(layer_focusable_set),
            },
            NativeInstancePropertyDef {
                name: "focused",
                get: Some(layer_focused_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "nodeVisible",
                get: Some(layer_node_visible_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "nodeEnabled",
                get: Some(layer_node_enabled_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "nodeFocusable",
                get: Some(layer_node_focusable_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "joinFocusChain",
                get: Some(layer_join_focus_chain_get),
                set: Some(layer_join_focus_chain_set),
            },
            NativeInstancePropertyDef {
                name: "nextFocusable",
                get: Some(layer_next_focusable_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "prevFocusable",
                get: Some(layer_prev_focusable_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "children",
                get: Some(layer_children_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "enabled",
                get: Some(layer_enabled_get),
                set: Some(layer_enabled_set),
            },
            NativeInstancePropertyDef {
                name: "isPrimary",
                get: Some(layer_is_primary_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "cached",
                get: Some(layer_cached_get),
                set: Some(layer_cached_set),
            },
            NativeInstancePropertyDef {
                name: "imageModified",
                get: Some(layer_image_modified_get),
                set: Some(layer_image_modified_set),
            },
            NativeInstancePropertyDef {
                name: "name",
                get: Some(layer_name_get),
                set: Some(layer_name_set),
            },
            NativeInstancePropertyDef {
                name: "attentionLeft",
                get: Some(layer_attention_left_get),
                set: Some(layer_attention_left_set),
            },
            NativeInstancePropertyDef {
                name: "attentionTop",
                get: Some(layer_attention_top_get),
                set: Some(layer_attention_top_set),
            },
            NativeInstancePropertyDef {
                name: "useAttention",
                get: Some(layer_use_attention_get),
                set: Some(layer_use_attention_set),
            },
            NativeInstancePropertyDef {
                name: "clipLeft",
                get: Some(layer_clip_left_get),
                set: Some(layer_clip_left_set),
            },
            NativeInstancePropertyDef {
                name: "clipTop",
                get: Some(layer_clip_top_get),
                set: Some(layer_clip_top_set),
            },
            NativeInstancePropertyDef {
                name: "clipWidth",
                get: Some(layer_clip_width_get),
                set: Some(layer_clip_width_set),
            },
            NativeInstancePropertyDef {
                name: "clipHeight",
                get: Some(layer_clip_height_get),
                set: Some(layer_clip_height_set),
            },
            NativeInstancePropertyDef {
                name: "hint",
                get: Some(layer_hint_get),
                set: Some(layer_hint_set),
            },
            NativeInstancePropertyDef {
                name: "showParentHint",
                get: Some(layer_show_parent_hint_get),
                set: Some(layer_show_parent_hint_set),
            },
            NativeInstancePropertyDef {
                name: "ignoreHintSensing",
                get: Some(layer_ignore_hint_sensing_get),
                set: Some(layer_ignore_hint_sensing_set),
            },
            NativeInstancePropertyDef {
                name: "imeMode",
                get: Some(layer_ime_mode_get),
                set: Some(layer_ime_mode_set),
            },
            NativeInstancePropertyDef {
                name: "neutralColor",
                get: Some(layer_neutral_color_get),
                set: Some(layer_neutral_color_set),
            },
            NativeInstancePropertyDef {
                name: "absoluteOrderMode",
                get: Some(layer_absolute_order_mode_get),
                set: Some(layer_absolute_order_mode_set),
            },
            NativeInstancePropertyDef {
                name: "callOnPaint",
                get: Some(layer_call_on_paint_get),
                set: Some(layer_call_on_paint_set),
            },
        ],
    })
}

fn register_bitmap_class(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "Bitmap",
        create: bitmap_create,
        destroy: bitmap_destroy,
        invalidate: None,
        methods: vec![
            NativeInstanceMethodDef {
                name: "Bitmap",
                f: bitmap_ctor,
            },
            NativeInstanceMethodDef {
                name: "getPixel",
                f: bitmap_get_pixel,
            },
            NativeInstanceMethodDef {
                name: "setPixel",
                f: bitmap_set_pixel,
            },
            NativeInstanceMethodDef {
                name: "getMaskPixel",
                f: bitmap_get_mask_pixel,
            },
            NativeInstanceMethodDef {
                name: "setMaskPixel",
                f: bitmap_set_mask_pixel,
            },
            NativeInstanceMethodDef {
                name: "independ",
                f: bitmap_independ,
            },
            NativeInstanceMethodDef {
                name: "setSize",
                f: bitmap_set_size,
            },
            NativeInstanceMethodDef {
                name: "copyFrom",
                f: bitmap_copy_from,
            },
            NativeInstanceMethodDef {
                name: "onLoaded",
                f: bitmap_on_loaded,
            },
        ],
        properties: vec![
            NativeInstancePropertyDef {
                name: "id",
                get: Some(bitmap_id_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "nativeId",
                get: Some(bitmap_id_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "width",
                get: Some(bitmap_width_get),
                set: Some(bitmap_width_set),
            },
            NativeInstancePropertyDef {
                name: "height",
                get: Some(bitmap_height_get),
                set: Some(bitmap_height_set),
            },
            NativeInstancePropertyDef {
                name: "buffer",
                get: Some(bitmap_buffer_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "bufferForWrite",
                get: Some(bitmap_buffer_for_write_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "bufferPitch",
                get: Some(bitmap_buffer_pitch_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "loading",
                get: Some(bitmap_loading_get),
                set: None,
            },
        ],
    })
}

fn register_font_class(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "Font",
        create: font_create,
        destroy: font_destroy,
        invalidate: None,
        methods: vec![
            NativeInstanceMethodDef {
                name: "Font",
                f: font_ctor,
            },
            NativeInstanceMethodDef {
                name: "__bind",
                f: font_bind,
            },
        ],
        properties: vec![
            NativeInstancePropertyDef {
                name: "id",
                get: Some(font_id_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "face",
                get: Some(font_face_get),
                set: Some(font_face_set),
            },
            NativeInstancePropertyDef {
                name: "height",
                get: Some(font_height_get),
                set: Some(font_height_set),
            },
            NativeInstancePropertyDef {
                name: "color",
                get: Some(font_color_get),
                set: Some(font_color_set),
            },
            NativeInstancePropertyDef {
                name: "bold",
                get: Some(font_bold_get),
                set: Some(font_bold_set),
            },
            NativeInstancePropertyDef {
                name: "italic",
                get: Some(font_italic_get),
                set: Some(font_italic_set),
            },
            NativeInstancePropertyDef {
                name: "strikeout",
                get: Some(font_strikeout_get),
                set: Some(font_strikeout_set),
            },
            NativeInstancePropertyDef {
                name: "underline",
                get: Some(font_underline_get),
                set: Some(font_underline_set),
            },
            NativeInstancePropertyDef {
                name: "angle",
                get: Some(font_angle_get),
                set: Some(font_angle_set),
            },
            NativeInstancePropertyDef {
                name: "faceIsFileName",
                get: Some(font_face_is_file_name_get),
                set: Some(font_face_is_file_name_set),
            },
        ],
    })
}

fn register_timer_class(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "Timer",
        create: timer_create,
        destroy: timer_destroy,
        invalidate: None,
        methods: vec![
            NativeInstanceMethodDef {
                name: "Timer",
                f: timer_ctor,
            },
            NativeInstanceMethodDef {
                name: "onTimer",
                f: timer_on_timer,
            },
        ],
        properties: vec![
            NativeInstancePropertyDef {
                name: "id",
                get: Some(timer_id_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "interval",
                get: Some(timer_interval_get),
                set: Some(timer_interval_set),
            },
            NativeInstancePropertyDef {
                name: "enabled",
                get: Some(timer_enabled_get),
                set: Some(timer_enabled_set),
            },
            NativeInstancePropertyDef {
                name: "capacity",
                get: Some(timer_capacity_get),
                set: Some(timer_capacity_set),
            },
            NativeInstancePropertyDef {
                name: "count",
                get: Some(timer_count_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "mode",
                get: Some(timer_mode_get),
                set: Some(timer_mode_set),
            },
        ],
    })
}

extern "C" fn timer_id_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const TimerInst) };
    set_int(out, inst.id.map_or(0, i64::from));
    0
}

// `set_real` is used by future real-valued properties; keep it available.
#[allow(dead_code)]
const _: fn(*mut Value, f64) = set_real;

#[cfg(test)]
mod tests {
    use super::timer_poll;
    use crate::CompatHost;
    use engine::Scene;
    use tjs::{Tjs2Engine, TjsValue};

    /// A bound VM + host + attached scene.
    ///
    /// Fields drop in declaration order: the host first (releasing every
    /// retained script object), then the engine, then the scene.
    struct Env {
        host: CompatHost,
        engine: Box<Tjs2Engine>,
        /// Kept alive for the host's raw pointer; never read directly.
        _scene: Box<Scene>,
    }

    impl Env {
        fn new() -> Env {
            let engine = Box::new(Tjs2Engine::new().expect("create engine"));
            let mut scene = Box::new(Scene::default());
            let mut host = CompatHost::new();
            // SAFETY: the boxed scene outlives the host.
            unsafe {
                host.services_mut()
                    .attach_scene(scene.as_mut() as *mut Scene);
            }
            host.install(&engine).expect("install compat surface");
            Env {
                host,
                engine,
                _scene: scene,
            }
        }

        fn eval_int(&self, expression: &str) -> i64 {
            match self.engine.eval(expression, "timer-test") {
                Ok(TjsValue::Integer(value)) => value,
                other => panic!("{expression:?} returned {other:?}, expected an integer"),
            }
        }
    }

    #[test]
    fn timer_poll_fires_the_callback() {
        let env = Env::new();
        env.engine
            .exec_script(
                "var hits = 0; \
                 var t = new Timer(function() { hits++; }, ''); \
                 t.interval = 1; \
                 t.enabled = true;",
                "timer-test",
            )
            .expect("define timer");

        // `timer_poll` is the frame body's timer step; poll until the 1 ms
        // timer has fired (it is enabled on the first poll, then due after
        // the interval).
        for _ in 0..200 {
            let mut ctx = env.host.ctx().expect("bound host");
            timer_poll(&mut ctx).expect("timer poll");
            if env.eval_int("hits") >= 1 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(env.eval_int("hits") >= 1, "the timer callback never ran");
        assert!(env.eval_int("t.count") >= 1);
    }
}
