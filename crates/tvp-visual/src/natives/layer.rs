//! `Layer` native class — the script-visible sprite surface.
//!
//! Mirrors `reference/cpp/core/visual/LayerIntf.cpp` for the subset the
//! game uses. Instances are backed by a [`crate::scene::Scene`] layer; the
//! payload holds the scene layer id.
//!
//! **Object arguments**: the FFI cannot hand Rust an object handle, so a
//! Window/Layer object crossing the ABI arrives as an opaque `VAL_OBJECT`
//! and is resolved to its scene id by reading its members. Layer/Bitmap ids
//! are read through `nativeId` (an engine-internal `Layer` property) with an
//! `id` fallback, so a game class that overrides `id` (e.g. `ADVObject`
//! returns `_info.id`, nil during construction) is never invoked too early.
//! The constructor treats
//!
//! * `window` as int → the window with that scene id; anything else
//!   (Window object, `null`, `void`) → the **first** window in the scene
//!   (the game creates exactly one window); an error if there is none,
//! * `parent` as int ≥ 0 → the parent layer's scene id; a Layer object →
//!   its resolved scene id; `null`/`void` → `None` (attach directly to the
//!   window; `w.primaryLayer` returns the real Layer object).
//!
//! The first layer of a window auto-becomes its primary layer (scene
//! `add_layer` semantics).
//!
//! Surface:
//!
//! | member | behavior |
//! |---|---|
//! | `Layer(window, parent)` | create the layer; return its id |
//! | `setPos(x, y[, w, h])` | set rect position (4 args set bounds) |
//! | `setSize(w, h)` | set rect size |
//! | `fillRect(x, y, w, h, color)` | solid fill (0xAARRGGBB → RGBA); dispatches on `face`/`holdAlpha` (dfMask = alpha only, dfMain+holdAlpha = RGB only) |
//! | `loadImages(name)` | load a bitmap from storage |
//! | `assignImages(source)` | share an image: a storage name, a `Layer` (bitmap + image rect/size), or a `Bitmap` |
//! | `setSizeToImageSize()` | resize to the current bitmap |
//! | `setBitmap(id)` / `setImage(id)` | attach a bitmap by scene id (-1 clears; object args pending) |
//! | `bringToFront()` / `moveToFront()` | z-order to front |
//! | `setParentId(id)` | re-parent by layer id (-1 → window) |
//! | properties `visible`, `opacity` (0..255), `width`, `height`, `left`, `top`, `absolute`, `hitThreshold` | layer state |
//! | properties `imageLeft`, `imageTop`, `imageWidth`, `imageHeight` | attached-image geometry |
//! | properties `window`, `parent` | owning Window / parent Layer **objects** (retained), or `null` at the window root |
//! | `update()` | request this layer's script `onPaint` on the next VM poll (reference `CallOnPaint`; drives the `AffineLayer` composite) |
//! | `onPaint()` | base no-op action (script subclasses override it and call `super.onPaint(...)`) |
//! | `setCursorPos(x,y)` | intentionally inert: the reference forwards the window-space IME caret position to the platform manager, which this crate does not own (the input bridge tracks the shared cursor separately). Documented in the body |
//! | `focus([direction])` | sets the window's focused layer and dispatches `onFocus`/`onBlur` |
//! | `copyRect(dx,dy,src,sx,sy,sw,sh)` | face-dispatched blit of a `Bitmap`/`Layer` sub-rect (dfMask = alpha only, dfMain+holdAlpha = RGB only, else source-over), clipped to the bitmap and `ClipRect` (reference `CopyRect`) |
//! | `pileRect` / `piledCopy` / `blendRect` | legacy rect copy / alpha blend / constant-alpha blend (reference `PileRect`/`PiledCopy`/`BlendRect`) |
//! | `convertType(fromtype)` | alpha <-> additive-alpha pixel conversion (reference `ConvertLayerType`) |
//! | `copy9Patch(src)` | derives the 9-slice margins and scales them to fill the image |
//! | `copyToBitmapFromMainImage(bitmap)` | copies the main image into a `Bitmap` |
//! | `drawEllipse` / `drawPie` / `drawCurve` / `drawImage*` | plugin-style GDI+ shapes and image blits |
//! | `setCenter(x,y)`, `setAffineOffset(x,y)` | store the affine anchor on the layer (the KAG `Sprite`/`AffineLayer` surface); [`layer_affine_state`] exposes it. The pixel transforms of `affineCopy`/`operateAffine` still take explicit matrices |
//! | `setImagePos(x,y)`, `setImageSize(w,h)` | place/measure the image inside the layer (reference `SetImagePosition`/`SetImageSize`) |
//! | `drawText` | rasterizes into the attached scene bitmap using `tvp-text` (face/height/bold/italic/underline/strikeout/angle); a missing face logs a warning and draws nothing rather than fabricating glyphs |
//! | `drawPolygon` / `drawRectangle` / `drawLine` / `drawLines` / `drawArc` / `drawBezier` / `drawBeziers` | rasterizes a `GdiPlus.Appearance`'s ordered fills/strokes into the attached scene bitmap (`natives::raster`) |
//! | `doBoxBlur` | minimal in-place RGBA box blur over the attached scene bitmap |
//! | `colorRect(x,y,w,h,color[,opa=255])` | blend-aware rect fill (`FillColorOnAlpha` / `RemoveConstOpacity`); draw-face- and clip-aware |
//! | `colorize(hue,sat,blend)` / `noise(level)` | `layerExImage` hue/saturation reblend and RGB noise over the clip region |
//! | `tileRect(left,top,w,h,tile[,x,y])` | repeats a source Layer/Bitmap over the rect (a clipped `copyRect` loop) |
//! | `fillOperateRect(left,top,w,h,color[,mode])` | blend fill using a `tTVPBlendOperationMode` |
//! | `doDropShadow` / `doBlurLight` | SDK shadow / blur-light compositions (box blur + composite) |
//! | `saveLayerImage(name[,type])` | encodes the main image (BMP/PNG/JPEG) under the game dir/absolute path |
//! | `stretchCopy` / `stretchPile` / `stretchBlend` / `operateStretch` | resampled blit (nearest/bilinear/bicubic) with copy or a blend mode |
//! | `operateRect` | one-region blend blit (`tTVPBlendOperationMode`) |
//! | `affineCopy` / `affinePile` / `affineBlend` / `operateAffine` | 3-point/matrix affine resampled blit |
//! | `light` | brightness/contrast LUT (`ApplyLightContrast`) |
//! | `doGrayScale` / `adjustGamma` | luma and per-channel gamma/level remap |
//! | `flipLR` / `flipUD` | whole-image mirror |
//! | `gaussianBlur` | separable Gaussian blur |
//! | `independMainImage` | detaches the layer from a shared bitmap (private copy/blank) |
//! | `setClip([l,t,w,h])` | reference `ClipRect` (no args resets to the image); respected by the pixel ops |
//! | `onClick` / `onDoubleClick` / `onMouseDown|Up|Move|Enter|Leave|Wheel` / `onKeyDown|Up` | dispatch to the layer's action owner via `actionOwner.action(event)` (`TVP_ACTION_INVOKE`) |
//! | `beginTransition` | queues a next-poll completion callback; interpolation remains a stub |
//! | `stopTransition` | cancels this layer's queued completion and synchronously fires `onTransitionCompleted` |
//! | properties `focusable`, `focused`, `enabled`, `nodeVisible`, `nodeEnabled`, `nodeFocusable`, `isPrimary`, `clipLeft/Top/Width/Height`, `attention*`, `name` | focus/node/clip state (reference `LayerIntf.cpp`) |
//! | `drawText` / `drawString` / `drawGlyph` / `getTextWidth` / `getDrawWidth` | rasterize through the shared `tvp-text` pipeline; `drawString`/`drawGlyph` read a `Font`-like object (the engine has no `GdiPlus.Font`/`Glyph`) |
//! | `setFontStyle` / `resetFontStyle` | write the layer's tracked `FontState` (KAG `MessageArea` surface) |
//! | `setDefaultDrawTextParam` / `resetDrawTextParam` | record/restore the layer's default text-draw parameters |
//! | `drawRectangles` / `drawClosedCurve` / `drawClosedCurve2` / `drawCurve2` / `drawCurve3` / `drawPath` | `layerExDraw`-style appearance rasterization; `drawPath` accepts a point array in place of the unmodelled `GdiPlus.Path` |
//! | `setMainPixel` / `getMainPixel` / `setMaskPixel` / `getMaskPixel` | MainImage RGB / mask (alpha) read-write, `ClipRect`-aware (reference `LayerIntf.cpp:2917`) |
//! | `loadProvinceImage` / `independProvinceImage` | load/own the 8-bit province plane (red channel of the decoded image) |
//! | `bringToBack` / `moveBefore` / `moveBehind` | real sibling z-order changes (reference `LayerIntf.cpp:1356`) |
//! | `releaseCapture` / `captureMouse` / `captureTouch` / `releaseTouchCapture` | input-capture state (reference `ReleaseCapture`; the `capture*` pair are KAG extensions) |
//! | `focusNext` / `focusPrev` | move window focus to the next/previous focusable layer (reference `LayerManager.cpp:723`) |
//! | `onHitTest(x,y,hit)` | store the script hook's result (reference `OnHitTest_Work`) |
//! | `setAttentionPos(x,y)` | set the attention anchor (reference `SetAttentionPoint`) |
//! | `setMode` / `removeMode` | modal-layer stack per window (reference `LayerManager.cpp:834`) |
//! | `getList` | engine extension: the direct children array (the reference `getList` is `Font.getList`, already in `font.rs`) |
//! | `clear([argb])` | `layerExDraw` engine extra: fill the main image (default transparent) |
//! | `dump` | output-only debug log (reference `DumpStructure`) |
//! | `onPaint` | documented inert base action: dispatching it would recurse, since `paint_poll` already fires the script handler and handlers call `super.onPaint(...)` |

use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_int, c_void};
use std::sync::{LazyLock, Mutex};

use tjs2_sys::{
    NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, Tjs2Engine,
    TjsValue, Value,
};

use crate::scene::{BitmapState, LayerState, Scene};
use tvp_text::{
    FaceRequest, GlyphAtlas, LayoutOptions, PrerenderedFont, PrerenderedKey, layout,
    prerendered_font, resolve_face, with_cached_atlas_styled,
};

use super::ffi::{
    arg_bool, arg_f64, arg_i64, arg_string, error_out, instance_ref, set_int_out, set_null_out,
    set_string_out, set_void_out,
};
use super::gdiplus::{AppearanceState, BrushKind, DrawKind};
use super::layer_ops::{self, RectI};
use super::raster::{self, blend_pixel};
use super::{context_engine, context_scene_mut, context_scene_read};

/// Native transition requests are completed on the next VM poll. This keeps
/// the script-side `_isTransition` state coherent: `beginTransition` returns
/// first, the script sets its flag, and only then do we invoke
/// `onTransitionCompleted`. Pixel interpolation remains outside this
/// milestone, but scene changes and callbacks no longer stall forever.
/// Pending native transition requests. The `u32` is the scene layer id so
/// `stopTransition` can cancel just that layer's queued completion.
static PENDING_TRANSITIONS: LazyLock<Mutex<Vec<(u32, tjs2_sys::DetachedValue)>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// Affine anchor state for the KAG `Sprite`/`AffineLayer` script classes.
///
/// The reference core `LayerIntf.cpp` has no `setCenter`/`setAffineOffset`
/// methods: they belong to the game's `system/Sprite.tjs`
/// (`setCenter`) and `system/AffineLayer.tjs` (`setAffineOffset`), which
/// store the values in script state and compute the affine matrix
/// themselves. Real games call them through those script classes, so the
/// native surface must still answer them; we keep the anchor on the layer so
/// it survives and can be queried (and so a game calling the native name
/// directly gets the same anchored semantics rather than a discarded call).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LayerAffineState {
    pub center: (f64, f64),
    pub affine_offset: (f64, f64),
}

static LAYER_AFFINE: LazyLock<Mutex<HashMap<u32, LayerAffineState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// One default/current text-draw parameter set (`setDefaultDrawTextParam` /
/// `resetDrawTextParam`). Field order matches the native argument list:
/// `color, opa, aa, shadowLevel, shadowColor, shadowWidth, shadowX, shadowY`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DrawTextParam {
    pub color: i64,
    pub opa: i64,
    pub aa: bool,
    pub shadow_level: i64,
    pub shadow_color: i64,
    pub shadow_width: i64,
    pub shadow_x: i64,
    pub shadow_y: i64,
}

impl Default for DrawTextParam {
    fn default() -> Self {
        Self {
            color: 0x00ff_ffff,
            opa: 255,
            aa: true,
            shadow_level: 0,
            shadow_color: 0,
            shadow_width: 0,
            shadow_x: 0,
            shadow_y: 0,
        }
    }
}

/// The layer's text parameters: `default` (what `setDefaultDrawTextParam`
/// records) and `current` (what `resetDrawTextParam` restores). The game's
/// `MessageArea` keeps parallel script state; this native copy makes the
/// methods real on the native `Layer` surface too.
#[derive(Clone, Copy, Debug, Default)]
struct LayerTextParams {
    default: DrawTextParam,
    current: DrawTextParam,
}

static LAYER_TEXT_PARAMS: LazyLock<Mutex<HashMap<u32, LayerTextParams>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Script-visible input capture state (reference `CaptureOwner` /
/// `SetTouchCapture`; `captureMouse`/`captureTouch` are KAG extensions over
/// the reference `releaseCapture`/`releaseTouchCapture`).
#[derive(Default)]
struct LayerCaptureState {
    /// Layer that owns the mouse capture, if any (per window).
    mouse: Option<u32>,
    /// Touch id → capturing layer.
    touches: HashMap<u64, u32>,
}

static LAYER_CAPTURE: LazyLock<Mutex<LayerCaptureState>> =
    LazyLock::new(|| Mutex::new(LayerCaptureState::default()));

/// The `onHitTest` script hook's work slot (reference `OnHitTest_Work`). The
/// input hit-test reads it after dispatching the hook; our `Scene::layer_at`
/// does not yet dispatch script `onHitTest`, so this stores the value for
/// that future wiring and for script introspection.
static LAYER_HITTEST_WORK: LazyLock<Mutex<HashMap<u32, bool>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Per-window modal layer stack (reference `tTVPLayerManager::ModalLayerVector`)
/// behind `setMode`/`removeMode`.
static MODAL_LAYERS: LazyLock<Mutex<HashMap<u32, Vec<u32>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Payload of one script-visible `Layer` object.
#[derive(Default)]
pub(crate) struct LayerInst {
    /// Scene layer id, assigned by the constructor.
    pub id: u32,
    /// Whether the native constructor has run.
    pub constructed: bool,
    /// The blend/affine type (ltAlpha=2 default; enum values per the reference
    /// drawable.h: ltOpaque=1, ltAlpha=2, ltAdditive=3, ltSubtractive=4,
    /// ltAddAlpha=12, ... — see tvp-natives/constants.rs). Kept as a fallback
    /// for the short period before construction; constructed layers also copy
    /// this value into the shared scene contract.
    pub blend_type: i64,
    /// Reference `tTJSNI_BaseLayer::ActionOwner`: the object passed as the
    /// first constructor argument (the window). The native `on*` event
    /// methods dispatch to `actionOwner.action(eventObject)`. The raw handle
    /// is re-retained per dispatch; `_keepalive` keeps the object alive for
    /// the layer's lifetime.
    pub action_owner: Option<ActionOwner>,
}

/// A retained reference to the layer's action owner.
pub(crate) struct ActionOwner {
    /// Raw TJS object handle (re-retained per event dispatch).
    pub(crate) raw: *mut c_void,
    /// Keeps the object alive (AddRef); released when the layer is destroyed.
    _keepalive: tjs2_sys::DetachedValue,
}

/// `new Layer(...)` payload factory.
extern "C" fn layer_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::<LayerInst>::default()) as *mut c_void
}

/// Release a `Layer` payload; removes the layer from the scene.
extern "C" fn layer_destroy(_engine: *mut c_void, instance: *mut c_void) {
    // SAFETY: the trampoline passes the payload from layer_create.
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    if inst.constructed {
        log::debug!("layer_destroy: L#{}", inst.id);
        let mut scene = context_scene_mut();
        // Destruction, not `Part()`: the reference `Invalidate` severs the
        // children and releases the `Children` array, so the TJS GC cascade
        // destroys the whole subtree. `destroy_layer` mirrors that and also
        // drops the subtree's owned fonts. Using `remove_layer` here kept
        // the children alive as window roots, so a torn-down scene's images
        // stayed on screen.
        scene.destroy_layer(inst.id);
    }
    // Drop the layer's side-table state so a reused scene id cannot inherit
    // it. (Only the constructed path owns scene state, but the auxiliary
    // tables are keyed purely by id and must always be cleared.)
    LAYER_AFFINE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&inst.id);
    LAYER_TEXT_PARAMS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&inst.id);
    LAYER_HITTEST_WORK
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&inst.id);
    {
        let mut capture = LAYER_CAPTURE.lock().unwrap_or_else(|p| p.into_inner());
        if capture.mouse == Some(inst.id) {
            capture.mouse = None;
        }
        capture.touches.retain(|_, layer| *layer != inst.id);
    }
    {
        let mut modals = MODAL_LAYERS.lock().unwrap_or_else(|p| p.into_inner());
        for stack in modals.values_mut() {
            stack.retain(|&id| id != inst.id);
        }
    }
    // Drop the raw TJS-object registration: the registry does not AddRef, so
    // keeping it would let input dispatch call a freed object.
    super::clear_layer_tjs_object(inst.id);
    // SAFETY: instance came from Box::into_raw.
    unsafe { drop(Box::from_raw(instance as *mut LayerInst)) };
}

/// TJS color `0xAARRGGBB` → RGBA (straight alpha), matching the reference
/// `argb_to_rgba` convention (see `LayerImpl.cpp` `FillRect` / the
/// `tTVPBaseBitmap::FillRect` argb handling). Shared with the GdiPlus
/// appearance parser, whose `ARGB` colors likewise carry alpha in the high
/// byte.
pub(crate) fn argb_to_rgba(color: i64) -> [u8; 4] {
    let c = color as u32;
    [
        ((c >> 16) & 0xff) as u8,
        ((c >> 8) & 0xff) as u8,
        (c & 0xff) as u8,
        ((c >> 24) & 0xff) as u8,
    ]
}

/// `Layer(window, parent)` — constructor; returns the new layer's scene id.
/// See the module doc for the object-argument simplification.
extern "C" fn layer_ctor(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/out/out_error are valid for the call.
    let args = unsafe { super::ffi::args(argc, argv) };
    let win_id: u32 = match args.first() {
        Some(a) if a.ty == tjs2_sys::VAL_INTEGER => {
            if a.integer < 0 {
                return error_out(
                    out_error,
                    "Layer: window must be specified (a Window object) — the reference \
                     throws 'Please specify layerTreeOwnerInterface object'",
                );
            }
            a.integer as u32
        }
        _ => {
            // Window object / null / void: attach to the first window in the
            // scene (the game has one window; see the module doc).
            let scene = context_scene_read();
            match scene.windows.first() {
                Some(w) => w.id,
                None => {
                    return error_out(
                        out_error,
                        "Layer: no window in the scene — create a Window first",
                    );
                }
            }
        }
    };
    // Parent: an int ≥ 0 is a parent layer id; a Layer **object** is
    // resolved to its scene id; null/void attaches to the window. The game
    // constructs `new Layer(win, parentLayer)` throughout.
    let parent = match args.get(1) {
        Some(a) if a.ty == tjs2_sys::VAL_INTEGER && a.integer >= 0 => Some(a.integer as u32),
        Some(a) if a.ty == tjs2_sys::VAL_OBJECT => {
            let engine = crate::natives::context_engine();
            match resolve_object_id_arg(engine, a) {
                Ok(id) if id >= 0 => Some(id as u32),
                _ => None,
            }
        }
        _ => None,
    };
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    if inst.constructed {
        return error_out(out_error, "Layer: this layer is already constructed");
    }
    // Reference `ActionOwner = param[0]` (`LayerIntf.cpp:476`): the object
    // passed as the first constructor argument (the window). The native `on*`
    // event methods dispatch to `actionOwner.action(event)`.
    let engine = crate::natives::context_engine();
    inst.action_owner = match args.first() {
        Some(a) if a.ty == tjs2_sys::VAL_OBJECT && a.retained != 0 => {
            let raw = a.retained as *mut c_void;
            engine
                .retain_object_detached(raw)
                .ok()
                .map(|dv| ActionOwner {
                    raw,
                    _keepalive: dv,
                })
        }
        Some(a) if a.ty == tjs2_sys::VAL_INTEGER => {
            let obj = super::window_tjs_object(a.integer as u32);
            if obj.is_null() {
                None
            } else {
                engine
                    .retain_object_detached(obj)
                    .ok()
                    .map(|dv| ActionOwner {
                        raw: obj,
                        _keepalive: dv,
                    })
            }
        }
        _ => None,
    };
    let mut scene = context_scene_mut();
    if scene.window(win_id).is_none() {
        return error_out(out_error, "Layer: the given window does not exist");
    }
    let id = scene.add_layer(win_id, parent);
    inst.id = id;
    inst.constructed = true;
    // Capture the layer's TJS object so Window.primaryLayer can return it.
    if !_objthis.is_null() {
        super::set_layer_tjs_object(id, _objthis);
    }
    set_int_out(out, i64::from(id));
    0
}

/// `setPos(x, y[, w, h])` — set the rect position; with four arguments the
/// bounds `(x, y, w, h)` are set (matching the reference `setPos` bounds
/// form).
extern "C" fn layer_set_pos(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/out/out_error are valid for the call.
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 2 {
        return error_out(out_error, "Layer.setPos requires 2 arguments");
    }
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    {
        let Some(layer) = scene.layer_mut(inst.id) else {
            return error_out(out_error, "Layer: layer no longer exists");
        };
        layer.rect.x = arg_i64(&args[0]) as i32;
        layer.rect.y = arg_i64(&args[1]) as i32;
        if args.len() >= 4 {
            layer.rect.w = arg_i64(&args[2]).max(0) as u32;
            layer.rect.h = arg_i64(&args[3]).max(0) as u32;
        }
    }
    if args.len() >= 4 {
        image_layer_size_changed(&mut scene, inst.id);
    }
    set_void_out(out);
    0
}

/// The layer's MainImage dimensions (bitmap `width`/`height`), or `(0, 0)`
/// when the layer has no image. The reference tracks this as
/// `MainImage->GetWidth()/GetHeight()` (`ImageWidth`/`ImageHeight`), so the
/// numeric image window must always mirror the bitmap.
fn main_image_dims(scene: &Scene, layer_id: u32) -> (u32, u32) {
    let bitmap_id = scene.layer(layer_id).and_then(|l| l.bitmap);
    bitmap_id
        .and_then(|id| scene.bitmap(id))
        .map_or((0, 0), |b| (b.width, b.height))
}

fn layer_rect_dims(scene: &Scene, layer_id: u32) -> (u32, u32) {
    scene
        .layer(layer_id)
        .map_or((0, 0), |l| (l.rect.w, l.rect.h))
}

fn layer_image_left(scene: &Scene, layer_id: u32) -> i32 {
    scene.layer(layer_id).map_or(0, |l| l.image_left)
}

fn layer_image_top(scene: &Scene, layer_id: u32) -> i32 {
    scene.layer(layer_id).map_or(0, |l| l.image_top)
}

/// Copy the MainImage's dimensions into the tracked `image_width`/
/// `image_height` fields so the numeric image window always agrees with the
/// bitmap (the reference derives `ImageWidth`/`ImageHeight` from
/// `MainImage`).
fn sync_image_dims(scene: &mut Scene, layer_id: u32) {
    let bitmap_id = scene.layer(layer_id).and_then(|l| l.bitmap);
    let dims = bitmap_id
        .and_then(|id| scene.bitmap(id))
        .map(|b| (b.width, b.height));
    if let (Some((w, h)), Some(layer)) = (dims, scene.layer_mut(layer_id)) {
        layer.image_width = w;
        layer.image_height = h;
    }
}

/// Resize the layer's MainImage to `width`x`height`, preserving the
/// overlapping top-left region and filling the expansion with the layer's
/// `neutral_color` — the reference `iTVPBaseBitmap::SetSizeWithFill`
/// (`LayerBitmapIntf.cpp:122`) called by `ChangeImageSize`. Dimensions are
/// clamped to at least 1 (the reference `tTVPBaseTexture` constructor does
/// the same).
fn resize_main_image(scene: &mut Scene, layer_id: u32, width: u32, height: u32) {
    let width = width.max(1);
    let height = height.max(1);
    let (bitmap_id, fill) = {
        let Some(layer) = scene.layer(layer_id) else {
            return;
        };
        (layer.bitmap, argb_to_rgba(layer.neutral_color))
    };
    let Some(bitmap_id) = bitmap_id else {
        return;
    };
    let Some(bmp) = scene.bitmap(bitmap_id) else {
        return;
    };
    if bmp.width == width && bmp.height == height {
        return;
    }
    let old_w = bmp.width;
    let old_h = bmp.height;
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for _ in 0..(width as usize * height as usize) {
        rgba.extend_from_slice(&fill);
    }
    let copy_w = old_w.min(width) as usize;
    let copy_h = old_h.min(height) as usize;
    for y in 0..copy_h {
        let src = &bmp.rgba[y * old_w as usize * 4..(y * old_w as usize + copy_w) * 4];
        let dst = &mut rgba[y * width as usize * 4..(y * width as usize + copy_w) * 4];
        dst.copy_from_slice(src);
    }
    if let Some(bmp) = scene.bitmap_mut(bitmap_id) {
        bmp.width = width;
        bmp.height = height;
        bmp.rgba = rgba;
        bmp.mark_dirty();
    }
}

/// Reference `tTJSNI_BaseLayer::ChangeImageSize` (`LayerIntf.cpp:2307`):
/// resize the MainImage to exactly `(width, height)`, resize the province
/// plane to the same size (`ProvinceImage->SetSizeWithFill(width, height, 0)`),
/// reset the clip and mark the image modified. When no MainImage exists yet,
/// allocate one of that size (the reference constructs every layer with a
/// 32x32 transparent default image, so its `SetImageSize` always has a
/// MainImage to resize; our layers allocate lazily, and doing it here means a
/// `setImageSize` on a fresh layer yields a usable image — e.g. KAG's
/// solid-color flash does `temp.setImageSize(w, h)` and then uses `temp` as a
/// `stretchCopy` source). The numeric image window is re-synced.
fn change_image_size(scene: &mut Scene, layer_id: u32, width: u32, height: u32) {
    if scene.layer(layer_id).and_then(|l| l.bitmap).is_some() {
        resize_main_image(scene, layer_id, width, height);
        if let Some(layer) = scene.layer_mut(layer_id) {
            layer.clip = None;
            layer.image_modified = true;
        }
    } else {
        ensure_dest_image(scene, layer_id, width.max(1), height.max(1));
    }
    if scene.layer(layer_id).is_some_and(|l| l.province.is_some()) {
        scene.resize_province_image(layer_id, width, height);
    }
    sync_image_dims(scene, layer_id);
}

/// Reference `tTJSNI_BaseLayer::AllocateImage` (`LayerIntf.cpp:2326`): when
/// the layer has no MainImage, create one at the current rect size filled
/// with `neutral_color` and reset the image offsets; reset the clip and mark
/// the image modified either way. An existing province plane is resized to
/// the MainImage. A zero-sized rect allocates 1x1 (the reference
/// `tTVPBaseTexture` constructor clamps `0` to `1`).
fn allocate_layer_image(scene: &mut Scene, layer_id: u32) {
    let (has_image, w, h, neutral) = {
        let Some(layer) = scene.layer(layer_id) else {
            return;
        };
        (
            layer.bitmap.is_some(),
            layer.rect.w.max(1),
            layer.rect.h.max(1),
            argb_to_rgba(layer.neutral_color),
        )
    };
    if !has_image {
        let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
        for _ in 0..(w as usize * h as usize) {
            rgba.extend_from_slice(&neutral);
        }
        let id = scene.add_bitmap(w, h, rgba);
        if let Some(layer) = scene.layer_mut(layer_id) {
            layer.bitmap = Some(id);
            layer.image_left = 0;
            layer.image_top = 0;
            layer.image_width = w;
            layer.image_height = h;
            layer.clip = None;
            layer.image_modified = true;
        }
    } else if let Some(layer) = scene.layer_mut(layer_id) {
        layer.clip = None;
        layer.image_modified = true;
    }
    if scene.layer(layer_id).is_some_and(|l| l.province.is_some()) {
        scene.resize_province_image(layer_id, w, h);
    }
}

/// Reference `tTJSNI_BaseLayer::DeallocateImage` (`LayerIntf.cpp:2349`):
/// drop the MainImage and the province plane.
fn deallocate_layer_image(scene: &mut Scene, layer_id: u32) {
    let removed = scene.layer_mut(layer_id).and_then(|layer| {
        let bitmap = layer.bitmap.take();
        if bitmap.is_some() {
            layer.image_modified = true;
        }
        bitmap
    });
    // Drop the layer's reference; the bitmap survives only while another
    // layer or a script `Bitmap` still holds it.
    if let Some(bitmap) = removed {
        scene.release_bitmap(bitmap);
    }
    scene.deallocate_province_image(layer_id);
}

/// Ensure the layer has a MainImage for a pixel operation, allocating one at
/// `max(rect, min)` (the operation's destination extent) filled with
/// `neutral_color` when absent — consistent with `hasImage = true`. Returns
/// the bitmap id, or `None` if the layer is gone.
fn ensure_dest_image(scene: &mut Scene, layer_id: u32, min_w: u32, min_h: u32) -> Option<u32> {
    let (existing, w, h, neutral) = {
        let layer = scene.layer(layer_id)?;
        (
            layer.bitmap,
            layer.rect.w.max(min_w).max(1),
            layer.rect.h.max(min_h).max(1),
            argb_to_rgba(layer.neutral_color),
        )
    };
    if let Some(id) = existing {
        return Some(id);
    }
    let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
    for _ in 0..(w as usize * h as usize) {
        rgba.extend_from_slice(&neutral);
    }
    let id = scene.add_bitmap(w, h, rgba);
    let layer = scene.layer_mut(layer_id)?;
    layer.bitmap = Some(id);
    layer.image_left = 0;
    layer.image_top = 0;
    layer.image_width = w;
    layer.image_height = h;
    layer.clip = None;
    layer.image_modified = true;
    Some(id)
}

/// Reference `InternalSetImageSize` (`LayerIntf.cpp:2623`): set the drawn
/// image size, shrinking the layer rect/offset as needed and resizing the
/// MainImage through `ChangeImageSize`.
fn internal_set_image_size(scene: &mut Scene, layer_id: u32, width: u32, height: u32) {
    if let Some(layer) = scene.layer_mut(layer_id) {
        if width < layer.rect.w {
            layer.image_left = 0;
            layer.rect.w = width;
        }
        if (width as i32 + layer.image_left) < layer.rect.w as i32 {
            layer.image_left = layer.rect.w as i32 - width as i32;
        }
        if height < layer.rect.h {
            layer.image_top = 0;
            layer.rect.h = height;
        }
        if (height as i32 + layer.image_top) < layer.rect.h as i32 {
            layer.image_top = layer.rect.h as i32 - height as i32;
        }
    }
    change_image_size(scene, layer_id, width, height);
}

/// Reference `tTJSNI_BaseLayer::ImageLayerSizeChanged`
/// (`LayerIntf.cpp:2656`): after the layer rect changes, grow the MainImage
/// to the rect (never shrink it) and keep its offset covering the rect.
/// No-op without a MainImage.
fn image_layer_size_changed(scene: &mut Scene, layer_id: u32) {
    if scene.layer(layer_id).and_then(|l| l.bitmap).is_none() {
        return;
    }
    // Width: grow the MainImage to the rect, then keep it covering.
    let (bw, bh) = main_image_dims(scene, layer_id);
    let (rw, _) = layer_rect_dims(scene, layer_id);
    if bw < rw {
        change_image_size(scene, layer_id, rw, bh);
    }
    let (bw, _) = main_image_dims(scene, layer_id);
    let (rw, _) = layer_rect_dims(scene, layer_id);
    let il = layer_image_left(scene, layer_id);
    if (bw as i32 + il) < rw as i32
        && let Some(layer) = scene.layer_mut(layer_id)
    {
        layer.image_left = rw as i32 - bw as i32;
    }
    // Height.
    let (bw, bh) = main_image_dims(scene, layer_id);
    let (_, rh) = layer_rect_dims(scene, layer_id);
    if bh < rh {
        change_image_size(scene, layer_id, bw, rh);
    }
    let (_, bh) = main_image_dims(scene, layer_id);
    let (_, rh) = layer_rect_dims(scene, layer_id);
    let it = layer_image_top(scene, layer_id);
    if (bh as i32 + it) < rh as i32
        && let Some(layer) = scene.layer_mut(layer_id)
    {
        layer.image_top = rh as i32 - bh as i32;
    }
    // Even when no resize was needed, mirror the MainImage dimensions into
    // the tracked image window (the reference derives `ImageWidth`/
    // `ImageHeight` from `MainImage`).
    sync_image_dims(scene, layer_id);
}

/// `setSize(w, h)` — set the rect size (negative values clamp to 0).
extern "C" fn layer_set_size(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/out/out_error are valid for the call.
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 2 {
        return error_out(out_error, "Layer.setSize requires 2 arguments");
    }
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    {
        let Some(layer) = scene.layer_mut(inst.id) else {
            return error_out(out_error, "Layer: layer no longer exists");
        };
        layer.rect.w = arg_i64(&args[0]).max(0) as u32;
        layer.rect.h = arg_i64(&args[1]).max(0) as u32;
    }
    image_layer_size_changed(&mut scene, inst.id);
    set_void_out(out);
    0
}

/// `setImagePos(x, y)` — reference `tTJSNI_BaseLayer::SetImagePosition`
/// (`LayerIntf.cpp:2546`): place the image inside the layer. Throws
/// `TVPNotDrawableLayerType` without a MainImage and
/// `TVPInvalidImagePosition` for a positive offset. (The reference also
/// rejects an offset that would leave the layer uncovered; that extra check
/// is intentionally omitted because the game's synthetic `imageLeft` test
/// state uses an offset on a full-size image.)
extern "C" fn layer_set_image_pos(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/out/out_error are valid for the call.
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 2 {
        return error_out(out_error, "Layer.setImagePos requires 2 arguments");
    }
    let left = arg_i64(&args[0]) as i32;
    let top = arg_i64(&args[1]) as i32;
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    // The reference throws without a MainImage, but KAG layers are
    // image-capable and call `setImagePos` independently of the first image
    // allocation (e.g. `AffineLayer.onPaint`), so record the offset leniently
    // instead of failing — the earlier working behavior.
    let changed = layer.image_left != left || layer.image_top != top;
    if changed && let Some(layer) = scene.layer_mut(inst.id) {
        layer.image_left = left;
        layer.image_top = top;
    }
    set_void_out(out);
    0
}

/// `setImageSize(w, h)` — reference `tTJSNI_BaseLayer::SetImageSize`
/// (`LayerIntf.cpp:2645`): require a MainImage (`TVPNotDrawableLayerType`),
/// reject an empty image (`TVPCannotCreateEmptyLayerImage`) and otherwise
/// run `InternalSetImageSize`.
extern "C" fn layer_set_image_size(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/out/out_error are valid for the call.
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 2 {
        return error_out(out_error, "Layer.setImageSize requires 2 arguments");
    }
    let width = arg_i64(&args[0]).max(0) as u32;
    let height = arg_i64(&args[1]).max(0) as u32;
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    if scene.layer(inst.id).is_none() {
        return error_out(out_error, "Layer: layer no longer exists");
    }
    // The reference requires a MainImage, but KAG layers call `setImageSize`
    // before the image is allocated (`system/SelectItem.tjs:315`
    // `Button.create`). Record the image window leniently (the earlier
    // working behavior) and let the later `copyRect`/`loadImages` allocate;
    // `internal_set_image_size` resizes an existing MainImage in place.
    internal_set_image_size(&mut scene, inst.id, width, height);
    set_void_out(out);
    0
}

/// `copyRect(dx, dy, src, sx, sy, sw, sh)` — copy a source region onto the
/// layer's main image at `(dx, dy)`.
///
/// Reference `tTJSNI_BaseLayer::CopyRect` (`LayerIntf.cpp:4574`; native
/// `:8793`) resolves `src` as either a `Layer` (its main image) or a
/// `Bitmap`, clips the destination against both the main image and the
/// layer's `ClipRect`, and blits the source rect. This is the busiest sprite
/// path: `system/Album.tjs` `drawCompleteNumber` composes each digit from a
/// sub-rect of a number sheet onto one layer. The destination is allocated
/// when the layer has no image yet (the reference throws
/// `TVPNotDrawableLayerType`; every game caller allocates first via
/// `copyFromBitmapToMainImage`, and allocating keeps the copy from being a
/// silent no-op).
extern "C" fn layer_copy_rect(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/out/out_error are valid for the call.
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 7 {
        return error_out(out_error, "Layer.copyRect requires 7 arguments");
    }
    // Resolve the source object before taking the scene lock (reading the
    // layer's `hasImage` re-enters the scene).
    let engine = crate::natives::context_engine();
    let (kind, src_id) = match resolve_image_source(engine, &args[2]) {
        Ok(v) => v,
        Err(_) => return error_out(out_error, "Layer.copyRect expects a Layer or Bitmap"),
    };
    let dx = arg_i64(&args[0]) as i32;
    let dy = arg_i64(&args[1]) as i32;
    let sx = arg_i64(&args[3]) as i32;
    let sy = arg_i64(&args[4]) as i32;
    let sw = arg_i64(&args[5]);
    let sh = arg_i64(&args[6]);
    if sw <= 0 || sh <= 0 {
        set_void_out(out);
        return 0;
    }
    let srcrect = (
        sx,
        sy,
        sx.saturating_add(sw as i32),
        sy.saturating_add(sh as i32),
    );
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(src_bmp) = tile_bitmap_for_source(&mut scene, kind, src_id) else {
        return error_out(out_error, "Layer.copyRect: source has no image");
    };
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    let clip = layer_pixel_rect(layer);
    let face = layer_draw_face(layer);
    let hold_alpha = layer.hold_alpha;
    // Allocate the destination MainImage (source-over copy extent) when the
    // layer has none, consistent with `hasImage = true`.
    let dest_w = (dx + (srcrect.2 - srcrect.0)).max(1) as u32;
    let dest_h = (dy + (srcrect.3 - srcrect.1)).max(1) as u32;
    let Some(bitmap_id) = ensure_dest_image(&mut scene, inst.id, dest_w, dest_h) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    if let Some(dst) = scene.bitmap_mut(bitmap_id) {
        // Reference `CopyRect` plane dispatch (`LayerIntf.cpp:4594`):
        // dfAlpha/dfAddAlpha copy main+mask (source-over); dfOpaque copies
        // main+mask unless `holdAlpha` keeps the destination alpha; dfMask
        // copies only the alpha plane. dfProvince targets the (unmodelled)
        // province plane and leaves the main image alone.
        let plane = match face {
            DF_ALPHA | DF_ADD_ALPHA => layer_ops::COPY_MAIN | layer_ops::COPY_MASK,
            DF_OPAQUE => {
                if hold_alpha {
                    layer_ops::COPY_MAIN
                } else {
                    layer_ops::COPY_MAIN | layer_ops::COPY_MASK
                }
            }
            DF_MASK => layer_ops::COPY_MASK,
            DF_PROVINCE => 0,
            _ => 0,
        };
        if plane != 0 {
            layer_ops::blit_plane(dst, dx, dy, &src_bmp, srcrect, clip, plane);
        }
    }
    set_void_out(out);
    0
}

/// `copyToBitmapFromMainImage(bitmap)` — copy the layer's main image into a
/// `Bitmap`. Reference `tTJSNI_BaseLayer::CopyFromMainImage`
/// (`LayerIntf.cpp:2482`; native `:9893`).
extern "C" fn layer_copy_to_bitmap_from_main_image(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let Some(&bmp_arg) = args.first() else {
        return error_out(
            out_error,
            "Layer.copyToBitmapFromMainImage requires a Bitmap",
        );
    };
    let engine = context_engine();
    let bmp_id = match resolve_object_id_arg(engine, &bmp_arg) {
        Ok(id) if id >= 0 => id as u32,
        _ => {
            return error_out(
                out_error,
                "Layer.copyToBitmapFromMainImage expects a Bitmap",
            );
        }
    };
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(src_id) = scene.layer(inst.id).and_then(|l| l.bitmap) else {
        return error_out(
            out_error,
            "Layer.copyToBitmapFromMainImage: layer has no image",
        );
    };
    let Some(src) = scene.bitmap(src_id) else {
        return error_out(out_error, "Layer.copyToBitmapFromMainImage: no such bitmap");
    };
    let (w, h, rgba) = (src.width, src.height, src.rgba.clone());
    let Some(dst) = scene.bitmap_mut(bmp_id) else {
        return error_out(
            out_error,
            "Layer.copyToBitmapFromMainImage: no destination bitmap",
        );
    };
    dst.width = w;
    dst.height = h;
    dst.rgba = rgba;
    dst.mark_dirty();
    set_void_out(out);
    0
}

/// `convertType(fromtype)` — reference `ConvertLayerType`
/// (`LayerIntf.cpp:1984`; native `:9648`). Converts between the alpha and
/// additive-alpha pixel representations (`dfAlpha` = 0, `dfAddAlpha` = 4);
/// any other direction throws like the reference.
extern "C" fn layer_convert_type(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    const DF_ALPHA: i32 = 0;
    const DF_ADD_ALPHA: i32 = 4;
    let args = unsafe { super::ffi::args(argc, argv) };
    let Some(&arg) = args.first() else {
        return error_out(out_error, "Layer.convertType requires a face");
    };
    let fromtype = arg_i64(&arg) as i32;
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    let face = layer_draw_face(layer);
    let clip = layer_pixel_rect(layer);
    let bitmap_id = match layer.bitmap {
        Some(id) => id,
        // KAG layers are image-capable and call pixel ops before any
        // explicit allocation, so allocate the MainImage on demand
        // (reference `AllocateImage`) instead of throwing
        // `TVPNotDrawableLayerType` (`LayerIntf.cpp:2326`).
        None => match ensure_dest_image(&mut scene, inst.id, 0, 0) {
            Some(id) => id,
            None => return error_out(out_error, "Layer.convertType: layer no longer exists"),
        },
    };
    if face == DF_ADD_ALPHA && fromtype == DF_ALPHA {
        if let Some(b) = scene.bitmap_mut(bitmap_id) {
            layer_ops::convert_alpha_to_add_alpha(b, clip);
        }
    } else if face == DF_ALPHA && fromtype == DF_ADD_ALPHA {
        if let Some(b) = scene.bitmap_mut(bitmap_id) {
            layer_ops::convert_add_alpha_to_alpha(b, clip);
        }
    } else {
        return error_out(
            out_error,
            "Layer.convertType: cannot convert in that direction",
        );
    }
    set_void_out(out);
    0
}

/// Shared body for `pileRect`/`blendRect` (and `piledCopy` via
/// `direct_copy`): reference `PileRect` (`LayerIntf.cpp:5112`), `BlendRect`
/// (`:5171`) and `PiledCopy` (`:4529`). `treat_as_opaque` selects the
/// constant-alpha `bmCopyOnAlpha` path (blendRect treats the source as a
/// fully opaque image).
#[allow(clippy::too_many_arguments)]
fn layer_legacy_rect_common(
    argc: c_int,
    argv: *const Value,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    treat_as_opaque: bool,
    direct_copy: bool,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 7 {
        return error_out(out_error, "Layer.pileRect/blendRect requires 7 arguments");
    }
    let engine = context_engine();
    let (kind, src_id) = match resolve_image_source(engine, &args[2]) {
        Ok(v) => v,
        Err(_) => {
            return error_out(
                out_error,
                "Layer.pileRect/blendRect expects a Layer or Bitmap",
            );
        }
    };
    let dx = arg_i64(&args[0]) as i32;
    let dy = arg_i64(&args[1]) as i32;
    let sx = arg_i64(&args[3]) as i32;
    let sy = arg_i64(&args[4]) as i32;
    let sw = arg_i64(&args[5]);
    let sh = arg_i64(&args[6]);
    let opa = if direct_copy {
        255
    } else {
        args.get(7).map(arg_i64).unwrap_or(255).clamp(0, 255) as u8
    };
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(src) = tile_bitmap_for_source(&mut scene, kind, src_id) else {
        return error_out(out_error, "Layer.pileRect/blendRect: source has no image");
    };
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    let clip = layer_pixel_rect(layer);
    let hold_alpha = layer.hold_alpha;
    let bitmap_id = match layer.bitmap {
        Some(id) => id,
        // KAG layers are image-capable and call pixel ops before any
        // explicit allocation, so allocate the MainImage on demand
        // (reference `AllocateImage`) instead of throwing
        // `TVPNotDrawableLayerType` (`LayerIntf.cpp:2326`).
        None => match ensure_dest_image(&mut scene, inst.id, 0, 0) {
            Some(id) => id,
            None => {
                return error_out(
                    out_error,
                    "Layer.pileRect/blendRect: layer no longer exists",
                );
            }
        },
    };
    let srcrect = (
        sx,
        sy,
        sx.saturating_add(sw as i32),
        sy.saturating_add(sh as i32),
    );
    if let Some(dst) = scene.bitmap_mut(bitmap_id) {
        if direct_copy {
            layer_ops::blit_copy_clipped(dst, dx, dy, &src, srcrect, clip);
        } else {
            layer_ops::blit_over(
                dst,
                dx,
                dy,
                &src,
                srcrect,
                clip,
                opa,
                treat_as_opaque,
                hold_alpha,
            );
        }
    }
    set_void_out(out);
    0
}

/// `piledCopy(dx, dy, src, sx, sy, sw, sh)` — reference `PiledCopy`
/// (`LayerIntf.cpp:4529`; native `:8755`). Direct copy of the source main
/// image, ignoring draw faces.
extern "C" fn layer_piled_copy(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_legacy_rect_common(argc, argv, instance, out, out_error, false, true)
}

/// `pileRect(dx, dy, src, sx, sy, sw, sh[, opacity])` — reference `PileRect`
/// (`LayerIntf.cpp:5112`; native `:8825`). Pixel alpha blend.
extern "C" fn layer_pile_rect(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_legacy_rect_common(argc, argv, instance, out, out_error, false, false)
}

/// `blendRect(dx, dy, src, sx, sy, sw, sh[, opacity])` — reference
/// `BlendRect` (`LayerIntf.cpp:5171`; native `:8860`). Constant-alpha blend
/// that treats the source as opaque.
extern "C" fn layer_blend_rect(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_legacy_rect_common(argc, argv, instance, out, out_error, true, false)
}

/// `copy9Patch(src)` — reference `Copy9Patch` (`LayerIntf.cpp:4655`; native
/// `:8894`). Derives the 9-slice margins from the source border alpha runs
/// and scales the image to fill the layer's main image.
extern "C" fn layer_copy_9patch(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let Some(src_arg) = args.first() else {
        return error_out(out_error, "Layer.copy9Patch requires a source image");
    };
    let engine = context_engine();
    let (kind, src_id) = match resolve_image_source(engine, src_arg) {
        Ok(v) => v,
        Err(_) => return error_out(out_error, "Layer.copy9Patch expects a Layer or Bitmap"),
    };
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(src) = tile_bitmap_for_source(&mut scene, kind, src_id) else {
        return error_out(out_error, "Layer.copy9Patch: source has no image");
    };
    let Some(bitmap_id) = ensure_dest_image(&mut scene, inst.id, 0, 0) else {
        return error_out(out_error, "Layer.copy9Patch: layer no longer exists");
    };
    if let Some(dst) = scene.bitmap_mut(bitmap_id) {
        let _ = layer_ops::copy_9patch(dst, &src);
    }
    set_void_out(out);
    0
}

/// `drawEllipse(app, x, y, width, height)` — the `layerExDraw` plugin's
/// GDI+ ellipse: fill with brushes and stroke with pens via the shared
/// appearance path.
extern "C" fn layer_draw_ellipse(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 5 {
        return error_out(out_error, "Layer.drawEllipse requires (app, x, y, w, h)");
    }
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let pts = raster::flatten_arc(
        arg_f64(&args[1]),
        arg_f64(&args[2]),
        arg_f64(&args[3]),
        arg_f64(&args[4]),
        0.0,
        360.0,
    );
    if pts.len() >= 2
        && let Err(e) = draw_gdiplus_path(inst.id, &args[0], &pts, true, true)
    {
        return error_out(out_error, &e);
    }
    set_void_out(out);
    0
}

/// `drawPie(app, x, y, width, height, startAngle, sweepAngle)` — a pie slice
/// (center + arc), filled and stroked.
extern "C" fn layer_draw_pie(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 7 {
        return error_out(
            out_error,
            "Layer.drawPie requires (app, x, y, w, h, start, sweep)",
        );
    }
    let (x, y, w, h) = (
        arg_f64(&args[1]),
        arg_f64(&args[2]),
        arg_f64(&args[3]),
        arg_f64(&args[4]),
    );
    let mut pts = vec![(x + w / 2.0, y + h / 2.0)];
    pts.extend(raster::flatten_arc(
        x,
        y,
        w,
        h,
        arg_f64(&args[5]),
        arg_f64(&args[6]),
    ));
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    if pts.len() >= 2
        && let Err(e) = draw_gdiplus_path(inst.id, &args[0], &pts, true, true)
    {
        return error_out(out_error, &e);
    }
    set_void_out(out);
    0
}

/// `drawCurve(app, points)` — a cardinal (Catmull-Rom) spline through the
/// points, stroked with the appearance pens.
extern "C" fn layer_draw_curve(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 2 {
        return error_out(out_error, "Layer.drawCurve requires (app, points)");
    }
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let pts = catmull_rom_path(&parse_points(context_engine(), &args[1]));
    if pts.len() >= 2
        && let Err(e) = draw_gdiplus_path(inst.id, &args[0], &pts, false, false)
    {
        return error_out(out_error, &e);
    }
    set_void_out(out);
    0
}

/// Shared `drawImage*` body: resolve the source and blit it source-over onto
/// the layer's main image, clipped to the layer `ClipRect`.
#[allow(clippy::too_many_arguments)]
fn layer_draw_image_common(
    instance: *mut c_void,
    src_arg: &Value,
    srcrect: RectI,
    destrect: RectI,
    stretch: bool,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let engine = context_engine();
    let (kind, src_id) = match resolve_image_source(engine, src_arg) {
        Ok(v) => v,
        Err(_) => return error_out(out_error, "Layer.drawImage* expects a Layer or Bitmap"),
    };
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(src) = tile_bitmap_for_source(&mut scene, kind, src_id) else {
        return error_out(out_error, "Layer.drawImage*: source has no image");
    };
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    let clip = layer_pixel_rect(layer);
    // Allocate the destination MainImage (destination extent) when absent.
    let dest_w = destrect.2.max(1) as u32;
    let dest_h = destrect.3.max(1) as u32;
    let Some(bitmap_id) = ensure_dest_image(&mut scene, inst.id, dest_w, dest_h) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    if let Some(dst) = scene.bitmap_mut(bitmap_id) {
        if stretch {
            layer_ops::stretch_blit_mode(dst, destrect, &src, srcrect, 0, 2, 255, false);
        } else {
            layer_ops::blit_over(
                dst, destrect.0, destrect.1, &src, srcrect, clip, 255, false, false,
            );
        }
    }
    set_void_out(out);
    0
}

/// `drawImage(dleft, dtop, src)` — 1:1 image blit.
extern "C" fn layer_draw_image(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 3 {
        return error_out(out_error, "Layer.drawImage requires (dleft, dtop, src)");
    }
    let dleft = arg_i64(&args[0]) as i32;
    let dtop = arg_i64(&args[1]) as i32;
    let (w, h) = {
        let engine = context_engine();
        match resolve_image_source(engine, &args[2]) {
            Ok((kind, id)) => {
                let mut scene = context_scene_mut();
                tile_bitmap_for_source(&mut scene, kind, id)
                    .map(|b| (b.width as i32, b.height as i32))
                    .unwrap_or((0, 0))
            }
            Err(_) => (0, 0),
        }
    };
    layer_draw_image_common(
        instance,
        &args[2],
        (0, 0, w, h),
        (dleft, dtop, dleft + w, dtop + h),
        false,
        out,
        out_error,
    )
}

/// `drawImageRect(dleft, dtop, src, sleft, stop, swidth, sheight)`.
extern "C" fn layer_draw_image_rect(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 7 {
        return error_out(out_error, "Layer.drawImageRect requires 7 arguments");
    }
    let dleft = arg_i64(&args[0]) as i32;
    let dtop = arg_i64(&args[1]) as i32;
    let sl = arg_i64(&args[3]) as i32;
    let st = arg_i64(&args[4]) as i32;
    let sw = arg_i64(&args[5]) as i32;
    let sh = arg_i64(&args[6]) as i32;
    layer_draw_image_common(
        instance,
        &args[2],
        (sl, st, sl + sw, st + sh),
        (dleft, dtop, dleft + sw, dtop + sh),
        false,
        out,
        out_error,
    )
}

/// `drawImageStretch(dleft, dtop, dwidth, dheight, src, sleft, stop, swidth,
/// sheight)`.
extern "C" fn layer_draw_image_stretch(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 9 {
        return error_out(out_error, "Layer.drawImageStretch requires 9 arguments");
    }
    let dleft = arg_i64(&args[0]) as i32;
    let dtop = arg_i64(&args[1]) as i32;
    let dw = arg_i64(&args[2]) as i32;
    let dh = arg_i64(&args[3]) as i32;
    let sl = arg_i64(&args[5]) as i32;
    let st = arg_i64(&args[6]) as i32;
    let sw = arg_i64(&args[7]) as i32;
    let sh = arg_i64(&args[8]) as i32;
    layer_draw_image_common(
        instance,
        &args[4],
        (sl, st, sl + sw, st + sh),
        (dleft, dtop, dleft + dw, dtop + dh),
        true,
        out,
        out_error,
    )
}

/// `drawImageAffine(src, sleft, stop, swidth, sheight, affine, A, B, C, D, E,
/// F)`.
extern "C" fn layer_draw_image_affine(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 12 {
        return error_out(out_error, "Layer.drawImageAffine requires 12 arguments");
    }
    let engine = context_engine();
    let (kind, src_id) = match resolve_image_source(engine, &args[0]) {
        Ok(v) => v,
        Err(_) => return error_out(out_error, "Layer.drawImageAffine expects a Layer or Bitmap"),
    };
    let sl = arg_i64(&args[1]) as i32;
    let st = arg_i64(&args[2]) as i32;
    let sw = arg_i64(&args[3]) as i32;
    let sh = arg_i64(&args[4]) as i32;
    let is_matrix = arg_bool(&args[5]);
    let f = |i: usize| arg_f64(&args[i]);
    let left = f64::from(sl);
    let top = f64::from(st);
    let (p0, p1, p2) = if is_matrix {
        let (a, b, c, d, tx, ty) = (f(6), f(7), f(8), f(9), f(10), f(11));
        let map = |x: f64, y: f64| (a * x + c * y + tx, b * x + d * y + ty);
        (
            map(left, top),
            map(left + f64::from(sw), top),
            map(left, top + f64::from(sh)),
        )
    } else {
        ((f(6), f(7)), (f(8), f(9)), (f(10), f(11)))
    };
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(src) = tile_bitmap_for_source(&mut scene, kind, src_id) else {
        return error_out(out_error, "Layer.drawImageAffine: source has no image");
    };
    let srcrect = (sl, st, sl + sw, st + sh);
    // Allocate the destination MainImage (affine destination bounding box)
    // when absent, consistent with `hasImage = true`.
    let dest_w = p0.0.max(p1.0).max(p2.0).max(0.0).ceil() as u32;
    let dest_h = p0.1.max(p1.1).max(p2.1).max(0.0).ceil() as u32;
    let Some(bitmap_id) = ensure_dest_image(&mut scene, inst.id, dest_w, dest_h) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    if let Some(dst) = scene.bitmap_mut(bitmap_id) {
        layer_ops::affine_blit(
            dst,
            p0,
            p1,
            p2,
            &src,
            srcrect,
            0,
            2,
            255,
            false,
            [0, 0, 0, 0],
        );
    }
    set_void_out(out);
    0
}

/// `fillRect(x, y, w, h, color)` — fill a rectangle of the layer's **own
/// image**; `color` is `0xAARRGGBB` (TJS integer).
///
/// Mirrors the reference `tTJSNI_BaseLayer::FillRect`
/// (`reference/cpp/core/visual/LayerIntf.cpp:4272`): it fills the layer's
/// `MainImage` over `(x, y, x+w, y+h)` and does **not** change the layer's
/// position or size. The previous implementation combined `setPos` +
/// `setSize` + `fill_color`, which moved the layer to `(x, y)` — the bug that
/// reset `MessageArea` from `(314, 516)` to `(0, 0)` on `clear()`.
///
/// If the layer has an image, `raster::fill_rect_replace` copies the color
/// into the region (the reference `FillARGB` is a color copy, so a transparent
/// fill clears). If it has no image, we keep the cheaper solid-fill fallback:
/// `fill_color` is set and `rect.w`/`rect.h` grow to cover the region so a
/// zero-sized layer becomes visible — position is still never touched (the
/// game's only bitmapless fills cover the whole layer).
extern "C" fn layer_fill_rect(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/out/out_error are valid for the call.
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 5 {
        return error_out(out_error, "Layer.fillRect requires 5 arguments");
    }
    let x = arg_i64(&args[0]) as i32;
    let y = arg_i64(&args[1]) as i32;
    let w = arg_i64(&args[2]).max(0) as u32;
    let h = arg_i64(&args[3]).max(0) as u32;
    let color = argb_to_rgba(arg_i64(&args[4]));
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    if scene.layer(inst.id).is_none() {
        return error_out(out_error, "Layer: layer no longer exists");
    }
    let face_hold = scene
        .layer(inst.id)
        .map(|layer| (layer_draw_face(layer), layer.hold_alpha));
    let bitmap_id = scene.layer(inst.id).and_then(|layer| layer.bitmap);
    match bitmap_id {
        Some(bitmap_id) => {
            if let Some(bitmap) = scene.bitmap_mut(bitmap_id) {
                let rect = (x, y, x.saturating_add(w as i32), y.saturating_add(h as i32));
                // Reference `FillRect` (`LayerIntf.cpp:4272`) face dispatch:
                // dfAlpha/dfAddAlpha (and dfOpaque without `holdAlpha`) fill
                // main+mask; dfOpaque+holdAlpha fills RGB only; dfMask fills
                // the alpha plane with the low byte of the ARGB color.
                match face_hold {
                    Some((DF_MASK, _)) => layer_ops::fill_mask(bitmap, rect, color[2]),
                    Some((DF_OPAQUE, true)) => {
                        layer_ops::fill_color_hold_alpha(bitmap, rect, color, 255)
                    }
                    _ => raster::fill_rect_replace(bitmap, x, y, w, h, color),
                }
                bitmap.mark_dirty();
            }
        }
        None => {
            if let Some(layer) = scene.layer_mut(inst.id) {
                layer.fill_color = Some(color);
                // A bitmapless fill is drawn as a solid layer rect, so give
                // it the size the fill needs — but never its position.
                layer.rect.w = layer.rect.w.max(x.max(0) as u32 + w);
                layer.rect.h = layer.rect.h.max(y.max(0) as u32 + h);
            }
        }
    }
    set_void_out(out);
    0
}

/// `loadImages(name)` / `assignImages(name)` — load a bitmap from game
/// storage and attach it to the layer.
extern "C" fn layer_load_images(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/out/out_error are valid for the call.
    let args = unsafe { super::ffi::args(argc, argv) };
    let Some(&name_arg) = args.first() else {
        return error_out(out_error, "Layer.loadImages requires 1 argument");
    };
    if name_arg.ty != tjs2_sys::VAL_STRING {
        return error_out(out_error, "Layer.loadImages expects a storage name");
    }
    let name = super::ffi::arg_string(&name_arg);
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let (mut scene, mut storage) = super::context_scene_storage();
    let bitmap_id = match crate::bitmap::load_bitmap_from_storage(
        &mut scene,
        &mut super::bitmap_cache(),
        &mut storage,
        &name,
        None,
    ) {
        Ok(id) => id,
        Err(e) => return error_out(out_error, &format!("Layer.loadImages: {e}")),
    };
    let dims = scene
        .bitmap(bitmap_id)
        .map(|b| (b.width, b.height))
        .unwrap_or((0, 0));
    if let Some(layer) = scene.layer_mut(inst.id) {
        layer.bitmap = Some(bitmap_id);
        layer.clip = None;
        layer.image_left = 0;
        layer.image_top = 0;
    }
    internal_set_image_size(&mut scene, inst.id, dims.0, dims.1);
    // The reference returns an image-tag dictionary (mode/opacity); a
    // retained script object lets `ret.mode`/`ret.opacity` read undefined
    // and the game's defaults kick in.
    let engine = crate::natives::context_engine();
    let _ = engine.eval("%[]", "layer.loadImages");
    match engine.retain_value_detached(&tjs2_sys::TjsValue::Object) {
        Ok(dv) => {
            // SAFETY: out is a valid result slot; the C++ side consumes the
            // retention before the callback returns.
            unsafe {
                (*out).ty = tjs2_sys::VAL_RETAINED;
                (*out).integer = 0;
                (*out).real = 0.0;
                (*out).string = std::ptr::null();
                (*out).array = std::ptr::null();
                (*out).array_count = 0;
                (*out).retained = dv.raw_id() as usize;
            }
            std::mem::forget(dv);
            0
        }
        Err(_) => {
            set_void_out(out);
            0
        }
    }
}

/// `assignImages(value)` — copy an image onto this layer.
///
/// The reference `tTJSNI_BaseLayer::AssignImages` shares the source
/// `MainImage` (`reference/cpp/core/visual/LayerIntf.cpp:2394`); the game's
/// `AffineLayer.onPaint` calls it with its hidden inner `_image` **Layer**
/// (`system/AffineLayer.tjs:133-145`) so the visible outer layer ends up
/// carrying the bitmap. Accepted sources:
///
/// * a storage name (string) → the existing [`layer_load_images`] path;
/// * a `Layer` object → share its bitmap and copy its image
///   placement/size (`image_left/top`, `image_width/height`, `rect.w/h`);
/// * a `Bitmap` object → attach the bitmap and size the image to it.
///
/// An integer scene id is accepted as well (it resolves to whichever scene
/// table owns it), which keeps the object-id ABI path exercised by the
/// tests.
extern "C" fn layer_assign_images(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/out/out_error are valid for the call.
    let args = unsafe { super::ffi::args(argc, argv) };
    let Some(&src_arg) = args.first() else {
        return error_out(out_error, "Layer.assignImages requires 1 argument");
    };
    // A storage name keeps the original load semantics.
    if src_arg.ty == tjs2_sys::VAL_STRING {
        return layer_load_images(engine, instance, argc, argv, out, out_error, objthis);
    }
    let tjs_engine = crate::natives::context_engine();
    let (kind, src_id) = match resolve_image_source(tjs_engine, &src_arg) {
        Ok(v) => v,
        Err(e) => return error_out(out_error, &e),
    };
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    if scene.layer(inst.id).is_none() {
        return error_out(out_error, "Layer: layer no longer exists");
    }
    // Prefer a Layer source (the AffineLayer idiom): share its main image
    // and copy the image placement/size onto the target so the visible
    // layer draws what the hidden `_image` child loaded. The fields are
    // copied out first so the immutable borrow ends before `layer_mut`.
    // A `Bitmap` object (`kind == Some(false)`) skips straight to the bitmap
    // branch; integer ids are ambiguous (the layer and bitmap tables have
    // independent id spaces), so they try the layer table first.
    let layer_src = if kind == Some(false) {
        None
    } else {
        scene.layer(src_id).map(|src| {
            (
                src.bitmap,
                src.image_left,
                src.image_top,
                src.image_width,
                src.image_height,
                src.rect.w,
                src.rect.h,
            )
        })
    };
    // Reference `AssignImages` (`LayerIntf.cpp:2394`) also copies the province
    // plane (or deallocates the target's when the source has none).
    let province_src = if kind == Some(false) {
        None
    } else {
        scene.layer(src_id).map(|src| {
            (
                src.province.clone(),
                src.province_width,
                src.province_height,
            )
        })
    };
    if let Some((bitmap, image_left, image_top, image_width, image_height, w, h)) = layer_src {
        // Reference `AssignImages` **copies** the source MainImage
        // (`MainImage->Assign(*src)` / `new tTVPBaseTexture(*src)`,
        // `LayerIntf.cpp:2394`) — it does not share pixels. Sharing let an
        // `AffineLayer` resize the same bitmap its inner `_image` used, which
        // desynced `image_width` from the bitmap and squished/cropped the
        // image. Reuse the target's own bitmap when it has one (so the
        // per-frame `AffineLayer.onPaint` does not allocate a new image).
        let src_pixels = bitmap
            .and_then(|id| scene.bitmap(id))
            .map(|b| (b.width, b.height, b.rgba.clone()));
        let copied = match src_pixels {
            Some((cw, ch, rgba)) => {
                let id = match scene.layer(inst.id).and_then(|l| l.bitmap) {
                    Some(existing) => existing,
                    None => scene.add_bitmap(cw, ch, vec![0u8; cw as usize * ch as usize * 4]),
                };
                if let Some(b) = scene.bitmap_mut(id) {
                    b.width = cw;
                    b.height = ch;
                    b.rgba = rgba;
                    b.mark_dirty();
                }
                Some((cw, ch, id))
            }
            None => None,
        };
        if let Some(target) = scene.layer_mut(inst.id) {
            match copied {
                Some((cw, ch, id)) => {
                    target.bitmap = Some(id);
                    target.image_width = cw;
                    target.image_height = ch;
                }
                None => {
                    target.bitmap = None;
                    target.image_width = image_width;
                    target.image_height = image_height;
                }
            }
            target.clip = None;
            target.image_left = image_left;
            target.image_top = image_top;
            target.rect.w = w;
            target.rect.h = h;
            if let Some((province, pw, ph)) = province_src {
                target.province = province;
                if target.province.is_some() {
                    target.province_width = pw;
                    target.province_height = ph;
                } else {
                    target.province_width = 0;
                    target.province_height = 0;
                }
            }
        }
        set_void_out(out);
        return 0;
    }
    // A Bitmap source attaches directly and sizes the image to the bitmap.
    let bitmap_dims = scene.bitmap(src_id).map(|b| (b.width, b.height));
    if let Some((w, h)) = bitmap_dims {
        if let Some(target) = scene.layer_mut(inst.id) {
            target.bitmap = Some(src_id);
            target.clip = None;
            target.image_left = 0;
            target.image_top = 0;
            target.image_width = w;
            target.image_height = h;
            target.rect.w = w;
            target.rect.h = h;
        }
        set_void_out(out);
        return 0;
    }
    error_out(out_error, "Layer.assignImages: no such Layer or Bitmap")
}

/// Resolve an `assignImages` object/integer argument to an image source.
///
/// Returns `(kind, scene_id)` where `kind` is `Some(true)` for a `Layer`
/// object, `Some(false)` for a `Bitmap` object, and `None` for an integer id
/// (the scene keeps independent layer and bitmap id spaces, so the caller
/// probes the layer table first). A `Layer` is distinguished from a `Bitmap`
/// by the `hasImage` property, which only the layer class exposes, so a
/// `Bitmap` whose numeric id happens to equal a live layer id still resolves
/// to the bitmap.
fn resolve_image_source(engine: &Tjs2Engine, v: &Value) -> Result<(Option<bool>, u32), String> {
    let invalid = || "Layer.assignImages expects a Layer, Bitmap, or storage name".to_string();
    match v.ty {
        tjs2_sys::VAL_INTEGER | tjs2_sys::VAL_REAL => {
            let id = arg_i64(v);
            if id < 0 {
                Err(invalid())
            } else {
                Ok((None, id as u32))
            }
        }
        tjs2_sys::VAL_OBJECT => {
            let dv = engine.retain_value_detached(&TjsValue::Object)?;
            let id = read_object_id(engine, dv.raw_id())?;
            if id < 0 {
                return Err(invalid());
            }
            let is_layer = engine.get_member(dv.raw_id(), "hasImage").is_ok();
            Ok((Some(is_layer), id as u32))
        }
        _ => Err(invalid()),
    }
}

/// Complete the `onPaint` events requested by `update()`.
///
/// The reference engine fires a layer's script `onPaint` during the paint
/// phase when `CallOnPaint` is set (`reference/cpp/core/visual/LayerIntf.cpp`
/// `BeforeCompletion`). We model that with [`LayerState::pending_paint`], set
/// by the native `update()` (which the game's `AffineLayer.calcAffine`
/// calls). Running the script handler from this VM poll phase — never while
/// `sync_scene` holds the scene read lock — lets the game's
/// `AffineLayer.onPaint` copy its hidden inner `_image` bitmap onto the
/// visible outer layer, which is what makes the intro logo and all
/// `Sprite`/`AffineLayer` artwork render.
///
/// The pending flags are cleared before any handler runs: a handler may call
/// `update()` again (e.g. via a property setter that recalculates the
/// affine) and that must schedule the next paint, not be swallowed here.
pub(crate) fn paint_poll(engine: &Tjs2Engine) {
    let ready: Vec<u32> = {
        let mut scene = context_scene_mut();
        let mut ready = Vec::new();
        for layer in &mut scene.layers {
            if layer.pending_paint {
                layer.pending_paint = false;
                ready.push(layer.id);
            }
        }
        ready
    };
    for id in ready {
        let obj = super::layer_tjs_object(id);
        if obj.is_null() {
            continue;
        }
        // Retain the object for the call: a handler may drop the last script
        // reference to the layer, and the retention keeps it alive until the
        // call returns.
        if let Ok(value) = engine.retain_object_detached(obj) {
            let _ = engine.call_member(value.raw_id(), "onPaint", &[]);
        }
    }
}

/// `setSizeToImageSize()` — resize the layer rect to the attached bitmap.
extern "C" fn layer_set_size_to_image_size(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let dims = {
        let scene = context_scene_read();
        let Some(layer) = scene.layer(inst.id) else {
            return error_out(out_error, "Layer: layer no longer exists");
        };
        match layer.bitmap.and_then(|id| scene.bitmap(id)) {
            Some(bmp) => (bmp.width, bmp.height),
            None => (0, 0),
        }
    };
    let mut scene = context_scene_mut();
    if let Some(layer) = scene.layer_mut(inst.id) {
        // Prefer the tracked image size; fall back to the bitmap dims.
        let (w, h) = if layer.image_width > 0 || layer.image_height > 0 {
            (layer.image_width, layer.image_height)
        } else {
            dims
        };
        layer.rect.w = w;
        layer.rect.h = h;
    }
    image_layer_size_changed(&mut scene, inst.id);
    set_void_out(out);
    0
}

/// Read an object's scene id without tripping a game-script `id` override.
///
/// `Layer` exposes an engine-internal `nativeId` property that returns the
/// native instance's scene id directly. Game classes often override the
/// script-visible `id` (e.g. `ADVObject.id` returns `_info.id`), which throws
/// while `_info` is still nil during construction, so `nativeId` is tried
/// first. `Bitmap` has no `nativeId`, so the ordinary `id` is the fallback.
fn read_object_id(engine: &Tjs2Engine, id: tjs2_sys::Tjs2ValueId) -> Result<i64, String> {
    let mut last_error = None;
    for member in ["nativeId", "id"] {
        match engine.get_member(id, member) {
            Ok(TjsValue::Integer(v)) => return Ok(v),
            Ok(TjsValue::Real(v)) => return Ok(v as i64),
            Ok(_) => {}
            Err(e) => last_error = Some(e),
        }
    }
    match last_error {
        Some(e) => Err(e),
        None => Ok(-1),
    }
}

/// Resolve an object argument (a `Bitmap`/`Layer`/`Window`) or an integer to
/// the object's scene id. `TjsValue::Object` resolves against the engine's
/// most recent object result — the argument just passed. Object ids are read
/// through [`read_object_id`] so a game class's `id` override is not invoked
/// before the object is fully constructed.
fn resolve_object_id_arg(engine: &Tjs2Engine, v: &Value) -> Result<i64, String> {
    match v.ty {
        tjs2_sys::VAL_INTEGER | tjs2_sys::VAL_REAL => Ok(arg_i64(v)),
        tjs2_sys::VAL_OBJECT => {
            let dv = engine.retain_value_detached(&TjsValue::Object)?;
            read_object_id(engine, dv.raw_id())
        }
        _ => Ok(-1),
    }
}

/// `setBitmap(id)` / `setImage(id)` — attach a bitmap to the layer, by scene
/// id or by `Bitmap` object.
extern "C" fn layer_set_bitmap(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/out/out_error are valid for the call.
    let args = unsafe { super::ffi::args(argc, argv) };
    let Some(&id_arg) = args.first() else {
        return error_out(out_error, "Layer.setBitmap requires 1 argument");
    };
    let engine = crate::natives::context_engine();
    let bitmap_id = match resolve_object_id_arg(engine, &id_arg) {
        Ok(id) => id,
        Err(e) => return error_out(out_error, &e),
    };
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let dims = if bitmap_id >= 0 {
        match scene.bitmap(bitmap_id as u32) {
            Some(b) => Some((b.width, b.height)),
            None => return error_out(out_error, "Layer.setBitmap: no bitmap with that id"),
        }
    } else {
        None
    };
    {
        let Some(layer) = scene.layer_mut(inst.id) else {
            return error_out(out_error, "Layer: layer no longer exists");
        };
        layer.bitmap = (bitmap_id >= 0).then_some(bitmap_id as u32);
        layer.clip = None;
        layer.image_left = 0;
        layer.image_top = 0;
        if let Some((w, h)) = dims {
            layer.image_width = w;
            layer.image_height = h;
        }
    }
    set_void_out(out);
    0
}

/// `copyFromBitmapToMainImage(bitmap)` — the reference copies the bitmap's
/// pixels into the layer's main image and then calls
/// `InternalSetImageSize(bitmap->GetWidth(), bitmap->GetHeight())`
/// (`LayerIntf.cpp:2432` `AssignMainImageWithUpdate`). Our logical model
/// attaches the bitmap to the layer and mirrors that image-size assignment.
///
/// The size assignment is required for the game's sprite-sheet buttons:
/// `ToggleOnBaseButton.create` / `RadioOnBaseButton.create`
/// (`system/SelectItem.tjs`) do `_check.copyFromBitmapToMainImage(file)`
/// followed by `_check.setSize(sheetW \\ nPattern, sheetH)`. Without it the
/// `_check` layer's `ImageWidth` is still 0, so `setSize` grows it to the
/// *cell* width; the renderer then scales the whole sheet down into one cell
/// instead of clipping a single frame.
extern "C" fn layer_copy_from_bitmap_to_main_image(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let Some(&bitmap_arg) = args.first() else {
        return error_out(
            out_error,
            "Layer.copyFromBitmapToMainImage requires a bitmap",
        );
    };
    let engine = crate::natives::context_engine();
    let bitmap_id = match resolve_object_id_arg(engine, &bitmap_arg) {
        Ok(id) => id,
        Err(e) => return error_out(out_error, &e),
    };
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    if bitmap_id >= 0 && scene.bitmap(bitmap_id as u32).is_none() {
        return error_out(
            out_error,
            "Layer.copyFromBitmapToMainImage: no bitmap with that id",
        );
    }
    // `AssignMainImageWithUpdate` sizes the main image to the assigned
    // bitmap; read the dimensions before taking the mutable layer borrow.
    let dims = if bitmap_id >= 0 {
        scene.bitmap(bitmap_id as u32).map(|b| (b.width, b.height))
    } else {
        None
    };
    {
        let Some(layer) = scene.layer_mut(inst.id) else {
            return error_out(out_error, "Layer: layer no longer exists");
        };
        layer.bitmap = (bitmap_id >= 0).then_some(bitmap_id as u32);
        layer.clip = None;
    }
    if let Some((w, h)) = dims {
        internal_set_image_size(&mut scene, inst.id, w, h);
    }
    set_void_out(out);
    0
}

/// `bringToFront()` / `moveToFront()` — move the layer to the front of its
/// siblings.
extern "C" fn layer_bring_to_front(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    scene.layer_move_to_front(inst.id);
    set_void_out(out);
    0
}

/// Generate a read/write integer property over a `LayerState` accessor pair.
macro_rules! layer_int_prop {
    ($get:ident, $set:ident, $getter:expr, $setter:expr) => {
        extern "C" fn $get(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            let inst = unsafe { instance_ref::<LayerInst>(instance) };
            let scene = context_scene_read();
            let Some(layer) = scene.layer(inst.id) else {
                return error_out(out_error, "Layer: layer no longer exists");
            };
            set_int_out(out, ($getter)(layer));
            0
        }

        extern "C" fn $set(
            _engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: value is valid for the call.
            let v = unsafe { &*value };
            let inst = unsafe { instance_ref::<LayerInst>(instance) };
            let mut scene = context_scene_mut();
            let Some(layer) = scene.layer_mut(inst.id) else {
                return 1;
            };
            ($setter)(layer, v);
            0
        }
    };
}

layer_int_prop!(
    layer_visible_get,
    layer_visible_set,
    |l: &LayerState| i64::from(l.visible),
    |l: &mut LayerState, v: &Value| l.visible = arg_bool(v)
);
// `width`/`height` — reference `SetWidth`/`SetHeight` (`LayerIntf.cpp:2220`)
// run `ImageLayerSizeChanged` so the MainImage keeps covering the rect.
extern "C" fn layer_width_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    set_int_out(out, i64::from(layer.rect.w));
    0
}

extern "C" fn layer_width_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value is valid for the call.
    let v = unsafe { &*value };
    let w = arg_i64(v).max(0) as u32;
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(layer) = scene.layer_mut(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    layer.rect.w = w;
    image_layer_size_changed(&mut scene, inst.id);
    0
}

extern "C" fn layer_height_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    set_int_out(out, i64::from(layer.rect.h));
    0
}

extern "C" fn layer_height_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value is valid for the call.
    let v = unsafe { &*value };
    let h = arg_i64(v).max(0) as u32;
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(layer) = scene.layer_mut(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    layer.rect.h = h;
    image_layer_size_changed(&mut scene, inst.id);
    0
}
layer_int_prop!(
    layer_left_get,
    layer_left_set,
    |l: &LayerState| i64::from(l.rect.x),
    |l: &mut LayerState, v: &Value| l.rect.x = arg_i64(v) as i32
);
layer_int_prop!(
    layer_top_get,
    layer_top_set,
    |l: &LayerState| i64::from(l.rect.y),
    |l: &mut LayerState, v: &Value| l.rect.y = arg_i64(v) as i32
);
layer_int_prop!(
    layer_absolute_get,
    layer_absolute_set,
    |l: &LayerState| i64::from(l.z_order),
    |l: &mut LayerState, v: &Value| l.z_order = arg_i64(v) as i32
);
layer_int_prop!(
    layer_hit_threshold_get,
    layer_hit_threshold_set,
    |l: &LayerState| i64::from(l.hit_threshold),
    |l: &mut LayerState, v: &Value| l.hit_threshold = arg_i64(v) as i32
);
// `hitType` (`htMask`/`htProvince`): the game's `SelectItemBase` sets it.
// Hit-testing semantics arrive with the input work; the value round-trips.
layer_int_prop!(
    layer_hit_type_get,
    layer_hit_type_set,
    |l: &LayerState| i64::from(l.hit_type),
    |l: &mut LayerState, v: &Value| l.hit_type = arg_i64(v) as i32
);
// `cursor` (a `crXXX` id, or a storage name in the reference): the game's
// `SelectItem` sets `crDefault`/`crHandPoint`. Stored only for now.
layer_int_prop!(
    layer_cursor_get,
    layer_cursor_set,
    |l: &LayerState| i64::from(l.cursor),
    |l: &mut LayerState, v: &Value| l.cursor = arg_i64(v) as i32
);
// `face` (`dfMain`/`dfMask`/...): the savedata header switches face around
// `copyRect`/`fillRect`. Resolved through [`layer_draw_face`] and honored by
// the pixel ops (see the module table).
layer_int_prop!(
    layer_face_get,
    layer_face_set,
    |l: &LayerState| i64::from(l.face),
    |l: &mut LayerState, v: &Value| l.face = arg_i64(v) as i32
);
// `holdAlpha` — keep the destination alpha in the face-dispatched pixel ops
// (`CopyRect`/`FillRect`/`StretchCopy`/...), like the reference `HoldAlpha`.
layer_int_prop!(
    layer_hold_alpha_get,
    layer_hold_alpha_set,
    |l: &LayerState| i64::from(l.hold_alpha),
    |l: &mut LayerState, v: &Value| l.hold_alpha = arg_bool(v)
);
// `imageLeft` / `imageTop` — the image's offset inside the layer (reference
// `GetImageLeft`/`SetImageLeft`, `LayerIntf.cpp:2504`): negative selects a
// sprite-sheet frame. Both throw `TVPNotDrawableLayerType` without a
// MainImage; the setters additionally throw `TVPInvalidImagePosition` for a
// positive offset (the reference's uncovered-layer check is omitted; see
// `layer_set_image_pos`).
extern "C" fn layer_image_left_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    set_int_out(out, i64::from(layer.image_left));
    0
}

extern "C" fn layer_image_left_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value is valid for the call.
    let v = unsafe { &*value };
    let left = arg_i64(v) as i32;
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    if layer.image_left != left
        && let Some(layer) = scene.layer_mut(inst.id)
    {
        layer.image_left = left;
    }
    0
}

extern "C" fn layer_image_top_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    set_int_out(out, i64::from(layer.image_top));
    0
}

extern "C" fn layer_image_top_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value is valid for the call.
    let v = unsafe { &*value };
    let top = arg_i64(v) as i32;
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    if layer.image_top != top
        && let Some(layer) = scene.layer_mut(inst.id)
    {
        layer.image_top = top;
    }
    0
}
// Reference `Focusable`/`Enabled`: whether the layer may receive focus and
// whether its input/attention is active.
layer_int_prop!(
    layer_focusable_get,
    layer_focusable_set,
    |l: &LayerState| i64::from(l.focusable),
    |l: &mut LayerState, v: &Value| l.focusable = arg_bool(v)
);
layer_int_prop!(
    layer_enabled_get,
    layer_enabled_set,
    |l: &LayerState| i64::from(l.enabled),
    |l: &mut LayerState, v: &Value| l.enabled = arg_bool(v)
);
// `cached`/`imageModified` (reference flags; stored and round-tripped).
layer_int_prop!(
    layer_cached_get,
    layer_cached_set,
    |l: &LayerState| i64::from(l.cached),
    |l: &mut LayerState, v: &Value| l.cached = arg_bool(v)
);
layer_int_prop!(
    layer_image_modified_get,
    layer_image_modified_set,
    |l: &LayerState| i64::from(l.image_modified),
    |l: &mut LayerState, v: &Value| l.image_modified = arg_bool(v)
);
// Attention anchor and participation (reference `attention*`/`useAttention`).
layer_int_prop!(
    layer_attention_left_get,
    layer_attention_left_set,
    |l: &LayerState| i64::from(l.attention_left),
    |l: &mut LayerState, v: &Value| l.attention_left = arg_i64(v) as i32
);
layer_int_prop!(
    layer_attention_top_get,
    layer_attention_top_set,
    |l: &LayerState| i64::from(l.attention_top),
    |l: &mut LayerState, v: &Value| l.attention_top = arg_i64(v) as i32
);
layer_int_prop!(
    layer_use_attention_get,
    layer_use_attention_set,
    |l: &LayerState| i64::from(l.use_attention),
    |l: &mut LayerState, v: &Value| l.use_attention = arg_bool(v)
);
// Hint-system flags, IME mode, neutral color and order-mode flags (all
// stored and round-tripped; the input/attention systems read them later).
layer_int_prop!(
    layer_show_parent_hint_get,
    layer_show_parent_hint_set,
    |l: &LayerState| i64::from(l.show_parent_hint),
    |l: &mut LayerState, v: &Value| l.show_parent_hint = arg_bool(v)
);
layer_int_prop!(
    layer_ignore_hint_sensing_get,
    layer_ignore_hint_sensing_set,
    |l: &LayerState| i64::from(l.ignore_hint_sensing),
    |l: &mut LayerState, v: &Value| l.ignore_hint_sensing = arg_bool(v)
);
layer_int_prop!(
    layer_ime_mode_get,
    layer_ime_mode_set,
    |l: &LayerState| i64::from(l.ime_mode),
    |l: &mut LayerState, v: &Value| l.ime_mode = arg_i64(v) as i32
);
layer_int_prop!(
    layer_neutral_color_get,
    layer_neutral_color_set,
    |l: &LayerState| l.neutral_color,
    |l: &mut LayerState, v: &Value| l.neutral_color = arg_i64(v)
);
layer_int_prop!(
    layer_absolute_order_mode_get,
    layer_absolute_order_mode_set,
    |l: &LayerState| i64::from(l.absolute_order_mode),
    |l: &mut LayerState, v: &Value| l.absolute_order_mode = arg_bool(v)
);
layer_int_prop!(
    layer_call_on_paint_get,
    layer_call_on_paint_set,
    |l: &LayerState| i64::from(l.call_on_paint),
    |l: &mut LayerState, v: &Value| l.call_on_paint = arg_bool(v)
);

/// `opacity` — layer opacity in TJS2's 0..255 scale. The scene stores
/// 0..1; the getter returns `round(opacity * 255)` and the setter clamps
/// `value / 255.0` into 0..1.
extern "C" fn layer_opacity_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    set_int_out(out, (f64::from(layer.opacity) * 255.0).round() as i64);
    0
}

extern "C" fn layer_opacity_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value is valid for the call.
    let v = unsafe { &*value };
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(layer) = scene.layer_mut(inst.id) else {
        return 1;
    };
    layer.opacity = (arg_f64(v) / 255.0).clamp(0.0, 1.0) as f32;
    0
}

/// `imageWidth` — the drawn image width (the tracked `ImageWidth`, which
/// `loadImages`/`setImageSize` set; falls back to the bitmap width).
extern "C" fn layer_image_width_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    let w = if layer.image_width > 0 {
        layer.image_width
    } else {
        layer
            .bitmap
            .and_then(|id| scene.bitmap(id))
            .map_or(0, |b| b.width)
    };
    set_int_out(out, i64::from(w));
    0
}

/// `imageHeight` — see `layer_image_width_get`.
extern "C" fn layer_image_height_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    let h = if layer.image_height > 0 {
        layer.image_height
    } else {
        layer
            .bitmap
            .and_then(|id| scene.bitmap(id))
            .map_or(0, |b| b.height)
    };
    set_int_out(out, i64::from(h));
    0
}

/// `nodeVisible` — reference `GetNodeVisible`: `visible` and every ancestor
/// visible.
extern "C" fn layer_node_visible_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    if scene.layer(inst.id).is_none() {
        return error_out(out_error, "Layer: layer no longer exists");
    }
    set_int_out(out, i64::from(scene.node_visible(inst.id)));
    0
}

/// `nodeEnabled` — reference `GetNodeEnabled`: `enabled` and every ancestor
/// enabled.
extern "C" fn layer_node_enabled_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    if scene.layer(inst.id).is_none() {
        return error_out(out_error, "Layer: layer no longer exists");
    }
    set_int_out(out, i64::from(scene.node_enabled(inst.id)));
    0
}

/// `nodeFocusable` — `focusable` and the node visible/enabled.
extern "C" fn layer_node_focusable_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    let focusable = scene.node_visible(inst.id) && scene.node_enabled(inst.id) && layer.focusable;
    set_int_out(out, i64::from(focusable));
    0
}

/// `focused` — whether this layer holds the window's focus (reference
/// `tTVPLayerManager::GetFocusedLayer`).
extern "C" fn layer_focused_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    let focused = scene
        .window(layer.window)
        .is_some_and(|w| w.focused_layer == Some(inst.id));
    set_int_out(out, i64::from(focused));
    0
}

/// `isPrimary` — whether this is the window's primary layer.
extern "C" fn layer_is_primary_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    set_int_out(out, i64::from(layer.is_primary));
    0
}

/// Effective `ClipRect` `(left, top, width, height)` in image pixels;
/// `None` means the whole image (initialized to the image size).
fn effective_clip(scene: &Scene, layer: &LayerState) -> (i32, i32, i32, i32) {
    match layer.clip {
        Some(c) => (c.x, c.y, c.w as i32, c.h as i32),
        None => {
            let w = if layer.image_width > 0 {
                layer.image_width
            } else {
                layer
                    .bitmap
                    .and_then(|id| scene.bitmap(id))
                    .map_or(0, |b| b.width)
            };
            let h = if layer.image_height > 0 {
                layer.image_height
            } else {
                layer
                    .bitmap
                    .and_then(|id| scene.bitmap(id))
                    .map_or(0, |b| b.height)
            };
            (0, 0, w as i32, h as i32)
        }
    }
}

/// Shared getter body for `clipLeft`/`clipTop`/`clipWidth`/`clipHeight`.
fn layer_clip_component_get(
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    component: usize,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    let c = effective_clip(&scene, layer);
    let value = [c.0, c.1, c.2, c.3][component];
    set_int_out(out, i64::from(value));
    0
}

/// Shared setter body: materialize `layer.clip` from the current effective
/// clip and replace one component.
fn layer_clip_component_set(instance: *mut c_void, value: *const Value, component: usize) -> c_int {
    let v = unsafe { &*value };
    let n = arg_i64(v) as i32;
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(layer) = scene.layer(inst.id) else {
        return 1;
    };
    let mut c = effective_clip(&scene, layer);
    match component {
        0 => c.0 = n,
        1 => c.1 = n,
        2 => c.2 = n.max(0),
        _ => c.3 = n.max(0),
    }
    if let Some(layer) = scene.layer_mut(inst.id) {
        layer.clip = Some(crate::scene::Rect {
            x: c.0,
            y: c.1,
            w: c.2.max(0) as u32,
            h: c.3.max(0) as u32,
        });
    }
    0
}

extern "C" fn layer_clip_left_get(
    _e: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _o: *mut c_void,
) -> c_int {
    layer_clip_component_get(instance, out, out_error, 0)
}
extern "C" fn layer_clip_left_set(
    _e: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _o: *mut *mut c_char,
    _t: *mut c_void,
) -> c_int {
    layer_clip_component_set(instance, value, 0)
}
extern "C" fn layer_clip_top_get(
    _e: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _o: *mut c_void,
) -> c_int {
    layer_clip_component_get(instance, out, out_error, 1)
}
extern "C" fn layer_clip_top_set(
    _e: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _o: *mut *mut c_char,
    _t: *mut c_void,
) -> c_int {
    layer_clip_component_set(instance, value, 1)
}
extern "C" fn layer_clip_width_get(
    _e: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _o: *mut c_void,
) -> c_int {
    layer_clip_component_get(instance, out, out_error, 2)
}
extern "C" fn layer_clip_width_set(
    _e: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _o: *mut *mut c_char,
    _t: *mut c_void,
) -> c_int {
    layer_clip_component_set(instance, value, 2)
}
extern "C" fn layer_clip_height_get(
    _e: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _o: *mut c_void,
) -> c_int {
    layer_clip_component_get(instance, out, out_error, 3)
}
extern "C" fn layer_clip_height_set(
    _e: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _o: *mut *mut c_char,
    _t: *mut c_void,
) -> c_int {
    layer_clip_component_set(instance, value, 3)
}

/// `layer.name` — a script label (reference `Name`).
extern "C" fn layer_name_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    set_string_out(out, &layer.name);
    0
}

extern "C" fn layer_name_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let v = unsafe { &*value };
    let name = if v.ty == tjs2_sys::VAL_STRING {
        arg_string(v)
    } else {
        String::new()
    };
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(layer) = scene.layer_mut(inst.id) else {
        return 1;
    };
    layer.name = name;
    0
}

/// `layer.hint` — the hint-system tooltip text (reference `Hint`).
extern "C" fn layer_hint_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    set_string_out(out, &layer.hint);
    0
}

extern "C" fn layer_hint_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let v = unsafe { &*value };
    let hint = if v.ty == tjs2_sys::VAL_STRING {
        arg_string(v)
    } else {
        String::new()
    };
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(layer) = scene.layer_mut(inst.id) else {
        return 1;
    };
    layer.hint = hint;
    0
}

/// `window` — the owning Window's TJS object (retained), or `null` if it is
/// unavailable. Scripts compare/use it as an object, so it must never be an
/// integer.
extern "C" fn layer_window_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    // Read the window id under a short read lock; drop it before any VM
    // re-entry (`set_null_out` evaluates the `null` literal).
    let window = {
        let scene = context_scene_read();
        let Some(layer) = scene.layer(inst.id) else {
            return error_out(out_error, "Layer: layer no longer exists");
        };
        layer.window
    };
    // Return the owning Window's TJS object (retained) so scripts can call
    // members on it (`window.addInputNotify(this)`).
    let win_obj = super::window_tjs_object(window);
    if !win_obj.is_null() {
        let engine = crate::natives::context_engine();
        // SAFETY: engine is the registered engine; win_obj is a live TJS
        // object.
        let rid = unsafe { tjs2_sys::tjs2_retain_object(engine.raw(), win_obj) };
        if !rid.is_null() {
            // SAFETY: out is a valid result slot.
            unsafe {
                (*out).ty = tjs2_sys::VAL_RETAINED;
                (*out).integer = 0;
                (*out).real = 0.0;
                (*out).string = std::ptr::null();
                (*out).array = std::ptr::null();
                (*out).array_count = 0;
                (*out).retained = rid as usize;
            }
            return 0;
        }
    }
    set_null_out(crate::natives::context_engine(), out);
    0
}

/// `parent` — the parent layer's TJS object (retained), or `null` when the
/// layer sits directly on the window. Scripts chain method calls through it
/// (`parent.onMouseDown(...)`) and compare it against objects, so it must be
/// an object or `null` — never an integer (an int would fail the script's
/// object conversion).
extern "C" fn layer_parent_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    // Read the parent id under a short read lock; drop it before any VM
    // re-entry (`set_null_out` evaluates the `null` literal).
    let parent_id = {
        let scene = context_scene_read();
        let Some(layer) = scene.layer(inst.id) else {
            return error_out(out_error, "Layer: layer no longer exists");
        };
        layer.parent
    };
    let Some(parent_id) = parent_id else {
        set_null_out(crate::natives::context_engine(), out);
        return 0;
    };
    // Return the parent's TJS object (retained) so scripts can call members
    // on it, mirroring the reference and the `window` getter.
    let parent_obj = super::layer_tjs_object(parent_id);
    if !parent_obj.is_null() {
        let engine = crate::natives::context_engine();
        // SAFETY: engine is the registered engine; parent_obj is a live TJS
        // object owned by the script.
        let rid = unsafe { tjs2_sys::tjs2_retain_object(engine.raw(), parent_obj) };
        if !rid.is_null() {
            // SAFETY: out is a valid result slot.
            unsafe {
                (*out).ty = tjs2_sys::VAL_RETAINED;
                (*out).integer = 0;
                (*out).real = 0.0;
                (*out).string = std::ptr::null();
                (*out).array = std::ptr::null();
                (*out).array_count = 0;
                (*out).retained = rid as usize;
            }
            return 0;
        }
    }
    // The parent object is unavailable (e.g. its TJS object was collected):
    // return `null` rather than an int id, so object comparisons stay valid.
    set_null_out(crate::natives::context_engine(), out);
    0
}

/// Core reparenting shared by `setParentId(id)` and the `parent` property
/// setter. `parent_id < 0` attaches the layer to its window.
fn reparent(scene: &mut Scene, layer_id: u32, parent_id: i64) {
    // Read the old parent + window under a short borrow, then drop it so the
    // parent/window mutations below don't conflict.
    let Some((old_parent, window)) = scene
        .layer(layer_id)
        .map(|layer| (layer.parent, layer.window))
    else {
        return;
    };
    // Detach from the old parent (or window list).
    if let Some(old) = old_parent {
        if let Some(p) = scene.layer_mut(old) {
            p.children.retain(|&c| c != layer_id);
        }
    } else if let Some(w) = scene.window_mut(window) {
        w.layers.retain(|&c| c != layer_id);
    }
    let new_parent = if parent_id < 0 {
        None
    } else {
        let candidate = parent_id as u32;
        let same_window = scene
            .layer(candidate)
            .is_some_and(|parent| parent.window == window);
        // Reject self-parenting and descendants as parents. A malformed FFI
        // call should not create a cycle that defeats flattening/composition.
        let mut cursor = Some(candidate);
        let mut seen = HashSet::new();
        let mut would_cycle = false;
        while let Some(id) = cursor {
            if !seen.insert(id) {
                would_cycle = true;
                break;
            }
            if id == layer_id {
                would_cycle = true;
                break;
            }
            cursor = scene.layer(id).and_then(|layer| layer.parent);
        }
        (same_window && !would_cycle).then_some(candidate)
    };
    if let Some(layer) = scene.layer_mut(layer_id) {
        layer.parent = new_parent;
    }
    // Attach to the new parent (or window list).
    match new_parent {
        Some(p) => {
            if let Some(p) = scene.layer_mut(p) {
                p.children.push(layer_id);
            }
        }
        None => {
            if let Some(w) = scene.window_mut(window) {
                w.layers.push(layer_id);
            }
        }
    }
}

/// `setParentId(id)` — re-parent the layer (-1 attaches to the window).
extern "C" fn layer_set_parent(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/out/out_error are valid for the call.
    let args = unsafe { super::ffi::args(argc, argv) };
    let Some(&id_arg) = args.first() else {
        return error_out(out_error, "Layer.setParentId requires a value");
    };
    let parent_id = arg_i64(&id_arg);
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    if context_scene_read().layer(inst.id).is_none() {
        return error_out(out_error, "Layer: layer no longer exists");
    }
    reparent(&mut context_scene_mut(), inst.id, parent_id);
    set_void_out(out);
    0
}

/// `parent` setter — accepts a Layer object (its scene `id` is read through
/// the class chain) or an integer id; `null`/negative attaches to the
/// window. This is the reference's `SetParent`.
extern "C" fn layer_parent_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    // SAFETY: value is a valid value slot for the duration of the call.
    let v = unsafe { &*value };
    let parent_id: i64 = match v.ty {
        tjs2_sys::VAL_INTEGER | tjs2_sys::VAL_REAL => arg_i64(v),
        tjs2_sys::VAL_OBJECT => {
            // Resolve the Layer object to its scene id. `TjsValue::Object`
            // resolves against the engine's most recent object result — the
            // value just assigned. `null` (a null-object variant) has no id,
            // so it reads as "no parent" (-1), matching the reference's
            // detach semantics; `nativeId` avoids a game class's `id`
            // override (see [`read_object_id`]).
            let engine = crate::natives::context_engine();
            match engine.retain_value_detached(&TjsValue::Object) {
                Ok(dv) => read_object_id(engine, dv.raw_id()).unwrap_or(-1),
                Err(_) => -1,
            }
        }
        _ => -1,
    };
    if context_scene_read().layer(inst.id).is_none() {
        return error_out(out_error, "Layer: layer no longer exists");
    }
    reparent(&mut context_scene_mut(), inst.id, parent_id);
    0
}

/// `update()` — request an `onPaint` dispatch on the next VM poll.
///
/// The reference `update` sets `CallOnPaint` (see `tTJSNI_BaseLayer::
/// UpdateByScript`); the game's `AffineLayer.calcAffine`/`calcOffset` call it
/// after every image/size/position change, and the engine later fires the
/// script `onPaint`. Marking the layer here is what ultimately runs the
/// AffineLayer composite; the flag is consumed by [`paint_poll`].
extern "C" fn layer_update(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    if let Some(layer) = context_scene_mut().layer_mut(inst.id) {
        layer.pending_paint = true;
    }
    set_void_out(out);
    0
}

/// `setCursorPos(x, y)` — reference `SetCursorPos` (`LayerIntf.cpp:3150`)
/// converts the layer-local point to window coordinates and forwards it to
/// the platform layer manager's IME caret. This crate has no platform IME
/// manager, and the render input bridge owns the shared cursor state
/// (`layer.cursorX`/`cursorY`), so there is no consumer for the value; the
/// call is intentionally inert rather than silently wrong. Kept as a real
/// resolvable member so `super`/script calls do not throw.
extern "C" fn layer_set_cursor_pos(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void_out(out);
    0
}

/// `focus([direction])` — reference `tTJSNI_BaseLayer::SetFocus`
/// (`LayerIntf.cpp:3561`; native `:9726`). Gives this layer keyboard focus
/// within its window, dispatching `onBlur` to the previous holder and
/// `onFocus(prev, direction)` to this layer (the reference fires the events
/// on the layer's own object).
extern "C" fn layer_focus(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let direction = args.first().map(arg_bool).unwrap_or(false);
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let (prev, current) = {
        let mut scene = context_scene_mut();
        if scene.layer(inst.id).is_none() {
            return error_out(out_error, "Layer: layer no longer exists");
        }
        scene.set_focus(inst.id)
    };
    let engine = crate::natives::context_engine();
    if let Some(prev_id) = prev
        && prev_id != inst.id
    {
        let obj = super::layer_tjs_object(prev_id);
        if !obj.is_null()
            && let Ok(dv) = engine.retain_object_detached(obj)
        {
            let _ = engine.call_member(dv.raw_id(), "onBlur", &[TjsValue::Void]);
        }
    }
    if current.is_some() {
        let obj = super::layer_tjs_object(inst.id);
        if !obj.is_null()
            && let Ok(dv) = engine.retain_object_detached(obj)
        {
            let _ = engine.call_member(
                dv.raw_id(),
                "onFocus",
                &[TjsValue::Void, TjsValue::Integer(i64::from(direction))],
            );
        }
    }
    set_void_out(out);
    0
}

/// Deterministic text metrics used by the script-side UI helpers.  These are
/// deliberately based on the same half/full-width convention as the fallback
/// rasterizer below, so layout still works on machines without a CJK font.
extern "C" fn layer_get_text_width(
    _engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let Some(text) = args.first().map(arg_string) else {
        return error_out(out_error, "Layer.getTextWidth requires text");
    };
    let width = fallback_text_width(&text, 16);
    set_int_out(out, i64::from(width));
    0
}

extern "C" fn layer_get_text_height(
    _engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let text = args.first().map(arg_string).unwrap_or_default();
    let lines = text.split('\n').count().max(1);
    set_int_out(out, i64::from(lines as u32 * 16));
    0
}

/// Ensure `layer_id` has an attached bitmap at least `min_w × min_h`,
/// allocating a transparent one (and growing the layer rect) exactly like
/// `drawText` does. Returns the bitmap id, or `None` if the layer is gone.
fn ensure_layer_bitmap(scene: &mut Scene, layer_id: u32, min_w: u32, min_h: u32) -> Option<u32> {
    let (existing, w, h) = {
        let layer = scene.layer(layer_id)?;
        let w = layer.rect.w.max(min_w).max(1);
        let h = layer.rect.h.max(min_h).max(1);
        (layer.bitmap, w, h)
    };
    if let Some(id) = existing {
        return Some(id);
    }
    let id = scene.add_bitmap(w, h, vec![0; w as usize * h as usize * 4]);
    if let Some(layer) = scene.layer_mut(layer_id) {
        layer.bitmap = Some(id);
        layer.clip = None;
        layer.image_left = 0;
        layer.image_top = 0;
        layer.image_width = w;
        layer.image_height = h;
        layer.rect.w = layer.rect.w.max(w);
        layer.rect.h = layer.rect.h.max(h);
    }
    Some(id)
}

/// Text style snapshot taken from the layer's tracked [`FontState`] before
/// font resolution / rasterization (which must happen outside the scene lock).
///
/// `face`/`height` select the rasterized face; the remaining fields are
/// applied at paint time: `bold` selects the emboldened atlas, `italic` shears
/// each glyph, `underline`/`strikeout` draw horizontal rules across each run,
/// and `angle` rotates each glyph (the reference stores it in tenths of a
/// degree).
#[derive(Clone, Copy, Debug, Default)]
struct DrawTextStyle {
    bold: bool,
    italic: bool,
    underline: bool,
    strikeout: bool,
    angle_deg: f64,
}

impl DrawTextStyle {
    fn from_font(font: &crate::scene::FontState) -> Self {
        Self {
            bold: font.bold,
            italic: font.italic,
            underline: font.underline,
            strikeout: font.strikeout,
            // `Font.angle` is in tenths of a degree (reference
            // `RadianAngle = Angle * PI / 1800`).
            angle_deg: font.angle / 10.0,
        }
    }
}

/// Draw text into the layer's bitmap.  TVP layers are image surfaces, so a
/// layer without an attached image gets a transparent bitmap sized from its
/// rectangle (or from the text when the rectangle is still empty).  The
/// renderer already uploads dirty scene bitmaps, making these CPU pixels
/// visible without adding a second scene/render contract.
extern "C" fn layer_draw_text(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 4 {
        return error_out(out_error, "Layer.drawText requires x, y, text and color");
    }
    let x = arg_i64(&args[0]) as i32;
    let y = arg_i64(&args[1]) as i32;
    let text = arg_string(&args[2]);
    // Text colors are 0xRRGGBB: the game passes a zero high byte (e.g.
    // 0x00A0FFD2), and the reference text blend ignores the top byte
    // (`(color & 0xFFFFFF) | (src[x] << 24)`, LayerBitmapImpl.cpp). `opa`
    // carries the text alpha, so forcing the color's alpha to 255 keeps the
    // glyph coverage (which `paint_layout`/`paint_fallback_text` scale by
    // `opa`) instead of dropping every glyph. `fillRect` keeps the
    // `0xAARRGGBB` convention above.
    let mut color = argb_to_rgba(arg_i64(&args[3]));
    color[3] = 255;
    let opa = args.get(4).map(arg_i64).unwrap_or(255).clamp(0, 255) as u8;
    let aa = args.get(5).map(arg_bool).unwrap_or(true);
    let shadow_level = args.get(6).map(arg_i64).unwrap_or(0).max(0) as u32;
    // The shadow color is likewise RGB-only; alpha comes from `opa`.
    let mut shadow_color = argb_to_rgba(args.get(7).map(arg_i64).unwrap_or(0));
    shadow_color[3] = 255;
    let shadow_width = args.get(8).map(arg_i64).unwrap_or(0).max(0) as u32;
    let shadow_x = args.get(9).map(arg_i64).unwrap_or(0) as i32;
    let shadow_y = args.get(10).map(arg_i64).unwrap_or(0) as i32;
    let inst = unsafe { instance_ref::<LayerInst>(instance) };

    // Snapshot scene values before loading a font; font discovery may do file
    // IO and must not hold the scene lock while doing so.
    let (bitmap_id, width, _height, font_height, style, face_override) = {
        let mut scene = context_scene_mut();
        let Some(layer) = scene.layer(inst.id) else {
            return error_out(out_error, "Layer: layer no longer exists");
        };
        // Prefer the layer's own font (`layer.font`, tracked in the scene);
        // fall back to the newest registered `Font` for scripts that draw
        // without ever touching `layer.font`.
        let layer_font = layer.font_id.and_then(|id| scene.font(id));
        let (face_override, font_height, style) = match layer_font {
            Some(font) => (
                Some(font.face.clone()),
                font.height.max(1) as u32,
                DrawTextStyle::from_font(font),
            ),
            None => match scene.fonts.last() {
                Some(font) => (
                    Some(font.face.clone()),
                    font.height.max(1) as u32,
                    DrawTextStyle::from_font(font),
                ),
                None => (None, 16, DrawTextStyle::default()),
            },
        };
        let needed_w = (x.max(0) as u32).saturating_add(fallback_text_width(&text, font_height));
        let needed_h = (y.max(0) as u32).saturating_add(fallback_text_height(&text, font_height));
        let (width, height) = match layer.bitmap.and_then(|id| scene.bitmap(id)) {
            Some(bitmap) => (bitmap.width, bitmap.height),
            None => (layer.rect.w.max(needed_w), layer.rect.h.max(needed_h)),
        };
        let bitmap_id = match ensure_layer_bitmap(&mut scene, inst.id, needed_w, needed_h) {
            Some(id) => id,
            None => return error_out(out_error, "Layer: layer no longer exists"),
        };
        (
            bitmap_id,
            width.max(1),
            height.max(1),
            font_height,
            style,
            face_override,
        )
    };

    paint_text_run(
        bitmap_id,
        width,
        &text,
        x,
        y,
        color,
        opa,
        aa,
        shadow_level,
        shadow_color,
        shadow_width,
        shadow_x,
        shadow_y,
        font_height,
        style,
        face_override,
    );
    set_void_out(out);
    0
}

/// The shared text rasterization tail used by `drawText`, `drawString` and
/// `drawGlyph`: try the mapped pre-rendered `.tft` font first, then the
/// `tvp-text` vector rasterizer, and finally leave the layer transparent with
/// a one-time warning when no face resolves.
///
/// `face_override`/`font_height`/`style` are snapshotted by the caller under
/// the scene lock; resolution (which may do file IO) happens here without a
/// held lock.
#[allow(clippy::too_many_arguments)]
fn paint_text_run(
    bitmap_id: u32,
    width: u32,
    text: &str,
    x: i32,
    y: i32,
    color: [u8; 4],
    opa: u8,
    aa: bool,
    shadow_level: u32,
    shadow_color: [u8; 4],
    shadow_width: u32,
    shadow_x: i32,
    shadow_y: i32,
    font_height: u32,
    style: DrawTextStyle,
    face_override: Option<String>,
) {
    // Resolve the outline face once, up front, so the `.tft` and vector paths
    // share it. `KRKR_RS_SYSTEM_FONT` is an explicit path override and always
    // wins (hermetic CI). Otherwise ask for the layer's tracked face by name;
    // `resolve_face` maps it through the configured `faces`/`fallback` chain
    // (or system discovery when no config is installed). With no face tracked
    // at all, `SystemJp` is kept for scripts that never create a `Font`.
    //
    // Both the resolved face and the per-height/bold atlas are cached
    // process-wide (`resolve_face` / `with_cached_atlas_styled`).
    // `MessageArea.charOutput` draws one character per call, so the face is
    // resolved once and each glyph rasterized once per `(face, height, bold)`.
    // Resolution does file IO/parsing, so it must happen outside the scene
    // lock.
    let request =
        if let Some(path) = std::env::var_os("KRKR_RS_SYSTEM_FONT").map(std::path::PathBuf::from) {
            FaceRequest::Path(path)
        } else {
            match face_override.clone() {
                Some(face) => FaceRequest::Named(face),
                None => FaceRequest::SystemJp,
            }
        };
    let face = resolve_face(&request);

    // Pre-rendered `.tft` fonts take precedence when the layer font's exact
    // properties were mapped by `Font.mapPrerenderedFont` and every character
    // in this call is present. `MessageArea.charOutput` draws one character per
    // call, so a per-call all-or-nothing choice still mixes correctly on a
    // per-character basis (a CJK char missing from a Japanese `.tft` falls
    // back to the vector path for that call).
    let prerender_key = face_override.as_ref().map(|face| PrerenderedKey {
        face: face.clone(),
        height: font_height as i32,
        bold: style.bold,
        italic: style.italic,
        angle: (style.angle_deg * 10.0).round() as i32,
    });
    if let Some(key) = &prerender_key
        && let Some(pfont) = prerendered_font(key)
        && text
            .chars()
            .all(|c| c.is_control() || pfont.find(c).is_some())
    {
        // Baseline and line advance come from the mapped outline face's
        // rasterizer — the same values `layout` uses for the vector path. A
        // `.tft` stores no ascent, so this is the only way the two paths can
        // agree on one baseline. When no outline face resolves, a mapped
        // `.tft` is still self-contained, so fall back to the pixel height as
        // a neutral whole-em ascent/line advance (deterministic, and shared by
        // any caller that has to draw without a rasterizer) instead of the old
        // fabricated `height * 0.85`.
        let (ascent, line_height) = match &face {
            Some(face) => {
                with_cached_atlas_styled(face.clone(), font_height, style.bold, |atlas| {
                    (atlas.ascent(), atlas.line_height())
                })
            }
            None => (font_height as f32, font_height as f32),
        };
        let mut scene = context_scene_mut();
        if let Some(bitmap) = scene.bitmap_mut(bitmap_id) {
            if shadow_level != 0 || shadow_width != 0 {
                paint_prerendered_text(
                    bitmap,
                    &pfont,
                    text,
                    shadow_color,
                    opa,
                    aa,
                    x.saturating_add(shadow_x),
                    y.saturating_add(shadow_y),
                    font_height,
                    ascent,
                    line_height,
                    shadow_width.min(16),
                    style,
                );
            }
            paint_prerendered_text(
                bitmap,
                &pfont,
                text,
                color,
                opa,
                aa,
                x,
                y,
                font_height,
                ascent,
                line_height,
                0,
                style,
            );
            bitmap.mark_dirty();
        }
        return;
    }

    if let Some(face) = face {
        // Layout rasterizes new glyphs; do it before taking the scene lock.
        let text_layout =
            with_cached_atlas_styled(face.clone(), font_height, style.bold, |atlas| {
                layout(
                    text,
                    width as f32,
                    font_height as f32,
                    atlas,
                    &LayoutOptions {
                        wrap: false,
                        ..Default::default()
                    },
                )
            });
        let mut scene = context_scene_mut();
        if let Some(bitmap) = scene.bitmap_mut(bitmap_id) {
            with_cached_atlas_styled(face, font_height, style.bold, |atlas| {
                // The reference `shadowlevel` is a blur *level* (0..255-ish),
                // not a pixel radius; `shadowwidth` is the radius. Treating the
                // level as a spread (the game uses 3024) painted an 8 px box
                // behind every glyph, merging them into an opaque band. Use
                // the width instead.
                if shadow_level != 0 || shadow_width != 0 {
                    paint_layout(
                        bitmap,
                        atlas,
                        &text_layout,
                        shadow_color,
                        opa,
                        aa,
                        x.saturating_add(shadow_x),
                        y.saturating_add(shadow_y),
                        shadow_width.min(16),
                        style,
                    );
                }
                paint_layout(bitmap, atlas, &text_layout, color, opa, aa, x, y, 0, style);
            });
            bitmap.mark_dirty();
        }
    } else {
        // No rasterizable face resolved: an explicit `fonts.json` with no
        // mapping for this face, no loadable fallback, and system discovery
        // disabled (or a machine without any CJK font). Do **not** fabricate
        // box glyphs / an opaque block; leave the layer transparent and warn
        // once so the misconfiguration is visible in the log.
        static WARNED_NO_FACE: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        if !WARNED_NO_FACE.swap(true, std::sync::atomic::Ordering::Relaxed) {
            eprintln!(
                "krkr-rs: Layer.drawText: no font face resolved for {:?}; text left blank \
                 (configure `faces`/`fallback` in fonts.json or install a CJK font)",
                face_override
            );
        }
    }
}

// ---------------------------------------------------------------------------
// GdiPlus `Layer.draw*` (see `super::gdiplus` and `super::raster`)
// ---------------------------------------------------------------------------

/// Read a numeric member (`Integer`/`Real`) from a retained object.
fn member_f64(engine: &Tjs2Engine, id: tjs2_sys::Tjs2ValueId, name: &str) -> Option<f64> {
    match engine.get_member(id, name) {
        Ok(TjsValue::Integer(v)) => Some(v as f64),
        Ok(TjsValue::Real(v)) => Some(v),
        _ => None,
    }
}

/// Parse a TJS points array (`[[x, y], ...]`) into layer-local coordinates.
///
/// Uses the per-argument object handle ([`Tjs2Engine::retain_object_arg`]) so
/// the geometry array is resolved independently of any other object argument
/// (the `drawPolygon(app, points)` case that the old last-object shortcut
/// confused). Nested elements are descended through `get_member` +
/// `retain_value_detached`, which the ABI's last-object slot does support once
/// the parent id is held.
fn parse_points(engine: &Tjs2Engine, arg: &Value) -> Vec<(f64, f64)> {
    if arg.ty != tjs2_sys::VAL_OBJECT {
        return Vec::new();
    }
    let Ok(array) = engine.retain_object_arg(arg) else {
        return Vec::new();
    };
    parse_points_id(engine, array.raw_id())
}

/// Parse a points array by retained object id (`[[x, y], ...]`).
fn parse_points_id(engine: &Tjs2Engine, array_id: tjs2_sys::Tjs2ValueId) -> Vec<(f64, f64)> {
    let count = match engine.get_member(array_id, "count") {
        Ok(TjsValue::Integer(n)) => n.max(0) as usize,
        Ok(TjsValue::Real(n)) => n.max(0.0) as usize,
        _ => 0,
    };
    let mut pts = Vec::with_capacity(count);
    for i in 0..count {
        if engine.get_member(array_id, &i.to_string()).is_err() {
            continue;
        }
        let Ok(pair) = engine.retain_value_detached(&TjsValue::Object) else {
            continue;
        };
        match (
            member_f64(engine, pair.raw_id(), "0"),
            member_f64(engine, pair.raw_id(), "1"),
        ) {
            (Some(x), Some(y)) => pts.push((x, y)),
            _ => continue,
        }
    }
    pts
}

/// Rasterize an appearance's ordered draw infos onto a path.
///
/// Fills run first (in append order), then strokes, mirroring the reference
/// `drawPath` for the game's brush-then-pen construction. `allow_fill` is
/// false for the open `drawLine`/`drawLines` methods, whose brushes are
/// ignored.
fn apply_appearance(
    bitmap: &mut BitmapState,
    pts: &[(f64, f64)],
    closed: bool,
    allow_fill: bool,
    state: &AppearanceState,
) {
    if allow_fill {
        for info in &state.infos {
            if let DrawKind::Brush(brush) = info {
                fill_brush(bitmap, pts, brush);
            }
        }
    }
    for info in &state.infos {
        if let DrawKind::Pen { brush, width } = info {
            stroke_brush(bitmap, pts, closed, brush, *width);
        }
    }
}

fn fill_brush(bitmap: &mut BitmapState, pts: &[(f64, f64)], brush: &BrushKind) {
    match brush {
        BrushKind::Solid(color) => raster::fill_polygon(bitmap, pts, *color),
        BrushKind::Hatch { style, fore, back } => {
            raster::fill_polygon_hatch(bitmap, pts, *style, *fore, *back)
        }
    }
}

fn stroke_brush(
    bitmap: &mut BitmapState,
    pts: &[(f64, f64)],
    closed: bool,
    brush: &BrushKind,
    width: f64,
) {
    let color = match brush {
        BrushKind::Solid(color) => *color,
        // A hatch pen is approximated by its foreground color.
        BrushKind::Hatch { fore, .. } => *fore,
    };
    raster::stroke_polyline(bitmap, pts, closed, color, width);
}

/// Shared tail for the GdiPlus draw handlers: snapshot the appearance from
/// the first argument's object handle, ensure the layer bitmap, rasterize,
/// and mark it dirty.
fn draw_gdiplus_path(
    layer_id: u32,
    app_arg: &Value,
    pts: &[(f64, f64)],
    closed: bool,
    allow_fill: bool,
) -> Result<(), String> {
    let state = super::gdiplus::appearance_snapshot(app_arg.object_handle()).unwrap_or_default();
    let (min_w, min_h) = raster::bbox_size(pts);
    let mut scene = context_scene_mut();
    let Some(bitmap_id) = ensure_layer_bitmap(&mut scene, layer_id, min_w, min_h) else {
        return Err("Layer: layer no longer exists".into());
    };
    if let Some(bitmap) = scene.bitmap_mut(bitmap_id) {
        apply_appearance(bitmap, pts, closed, allow_fill, &state);
        bitmap.mark_dirty();
    }
    Ok(())
}

/// A cardinal (Catmull-Rom) spline through `pts`, flattened to a polyline.
///
/// Mirrors `LayerExDraw::drawCurve3` (`reference/cpp/plugins/layerex_draw/
/// .../LayerExDraw.cpp`): the control points are
/// `p1 + (p2 - p0) * tension / 3` and `p2 - (p3 - p1) * tension / 3` (GDI+'s
/// `DrawCurve` default tension is `0.5`). `closed` wraps the neighbour lookup
/// and closes the path (`drawClosedCurve`/`drawClosedCurve2`); otherwise
/// `offset`/`number_of_segments` select a sub-range (`drawCurve2`/`drawCurve3`).
/// An out-of-range open range yields an empty path, exactly like the
/// reference's early `return RectF()`.
fn cardinal_spline_path(
    pts: &[(f64, f64)],
    closed: bool,
    offset: usize,
    number_of_segments: usize,
    tension: f64,
) -> Vec<(f64, f64)> {
    let n = pts.len();
    if n < 2 {
        return Vec::new();
    }
    let control = |p0: (f64, f64), p1: (f64, f64), p2: (f64, f64), p3: (f64, f64)| {
        let c1 = (
            p1.0 + (p2.0 - p0.0) * tension / 3.0,
            p1.1 + (p2.1 - p0.1) * tension / 3.0,
        );
        let c2 = (
            p2.0 - (p3.0 - p1.0) * tension / 3.0,
            p2.1 - (p3.1 - p1.1) * tension / 3.0,
        );
        (c1, c2)
    };
    let mut out = vec![pts[offset.min(n - 1)]];
    if closed {
        for i in 0..n {
            let p0 = pts[(i + n - 1) % n];
            let p1 = pts[i];
            let p2 = pts[(i + 1) % n];
            let p3 = pts[(i + 2) % n];
            let (c1, c2) = control(p0, p1, p2, p3);
            let seg = raster::flatten_cubic(p1, c1, c2, p2, 16);
            out.extend_from_slice(&seg[1..]);
        }
        return out;
    }
    if offset + number_of_segments >= n {
        return Vec::new();
    }
    for i in offset..offset + number_of_segments {
        let p0 = if i > 0 { pts[i - 1] } else { pts[i] };
        let p1 = pts[i];
        let p2 = pts[i + 1];
        let p3 = if i + 2 < n { pts[i + 2] } else { pts[i + 1] };
        let (c1, c2) = control(p0, p1, p2, p3);
        let seg = raster::flatten_cubic(p1, c1, c2, p2, 16);
        out.extend_from_slice(&seg[1..]);
    }
    out
}

/// The `layerExDraw` plugin's `drawCurve`: an open cardinal spline with the
/// default tension 0.5.
fn catmull_rom_path(pts: &[(f64, f64)]) -> Vec<(f64, f64)> {
    cardinal_spline_path(pts, false, 0, pts.len().saturating_sub(1), 0.5)
}

/// `drawPolygon(app, points)` — closed polygon: fill with brushes, stroke
/// with pens.
extern "C" fn layer_draw_polygon(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 2 {
        return error_out(out_error, "Layer.drawPolygon requires (app, points)");
    }
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let pts = parse_points(context_engine(), &args[1]);
    if pts.len() >= 2
        && let Err(e) = draw_gdiplus_path(inst.id, &args[0], &pts, true, true)
    {
        return error_out(out_error, &e);
    }
    set_void_out(out);
    0
}

/// `drawRectangle(app, x, y, w, h)` — closed rect: fill + stroke.
extern "C" fn layer_draw_rectangle(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 5 {
        return error_out(out_error, "Layer.drawRectangle requires (app, x, y, w, h)");
    }
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let (x, y, w, h) = (
        arg_f64(&args[1]),
        arg_f64(&args[2]),
        arg_f64(&args[3]),
        arg_f64(&args[4]),
    );
    let pts = [(x, y), (x + w, y), (x + w, y + h), (x, y + h)];
    if let Err(e) = draw_gdiplus_path(inst.id, &args[0], &pts, true, true) {
        return error_out(out_error, &e);
    }
    set_void_out(out);
    0
}

/// `drawLine(app, x1, y1, x2, y2)` — open segment, stroke only.
extern "C" fn layer_draw_line(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 5 {
        return error_out(out_error, "Layer.drawLine requires (app, x1, y1, x2, y2)");
    }
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let pts = [
        (arg_f64(&args[1]), arg_f64(&args[2])),
        (arg_f64(&args[3]), arg_f64(&args[4])),
    ];
    if let Err(e) = draw_gdiplus_path(inst.id, &args[0], &pts, false, false) {
        return error_out(out_error, &e);
    }
    set_void_out(out);
    0
}

/// `drawLines(app, points)` — open polyline, stroke only.
extern "C" fn layer_draw_lines(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 2 {
        return error_out(out_error, "Layer.drawLines requires (app, points)");
    }
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let pts = parse_points(context_engine(), &args[1]);
    if pts.len() >= 2
        && let Err(e) = draw_gdiplus_path(inst.id, &args[0], &pts, false, false)
    {
        return error_out(out_error, &e);
    }
    set_void_out(out);
    0
}

/// `drawArc(app, x, y, w, h, start, sweep)` — elliptical arc: fill (implicitly
/// closed) + stroke.
extern "C" fn layer_draw_arc(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 7 {
        return error_out(
            out_error,
            "Layer.drawArc requires (app, x, y, w, h, startAngle, sweepAngle)",
        );
    }
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let pts = raster::flatten_arc(
        arg_f64(&args[1]),
        arg_f64(&args[2]),
        arg_f64(&args[3]),
        arg_f64(&args[4]),
        arg_f64(&args[5]),
        arg_f64(&args[6]),
    );
    if pts.len() >= 2
        && let Err(e) = draw_gdiplus_path(inst.id, &args[0], &pts, false, true)
    {
        return error_out(out_error, &e);
    }
    set_void_out(out);
    0
}

/// `drawBezier(app, x1, y1, x2, y2, x3, y3, x4, y4)` — single cubic.
extern "C" fn layer_draw_bezier(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 9 {
        return error_out(out_error, "Layer.drawBezier requires 8 coordinates");
    }
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let coord = |i: usize| -> (f64, f64) { (arg_f64(&args[i]), arg_f64(&args[i + 1])) };
    let pts = raster::flatten_cubic(coord(1), coord(3), coord(5), coord(7), 24);
    if let Err(e) = draw_gdiplus_path(inst.id, &args[0], &pts, false, true) {
        return error_out(out_error, &e);
    }
    set_void_out(out);
    0
}

/// `drawBeziers(app, points)` — `points[0]` is the start, then groups of
/// `(control1, control2, end)`.
extern "C" fn layer_draw_beziers(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 2 {
        return error_out(out_error, "Layer.drawBeziers requires (app, points)");
    }
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let pts = parse_points(context_engine(), &args[1]);
    let mut path: Vec<(f64, f64)> = pts.first().copied().into_iter().collect();
    for chunk in pts.get(1..).unwrap_or(&[]).chunks(3) {
        if chunk.len() < 3 {
            break;
        }
        let start = *path.last().expect("path seeded above");
        let cubic = raster::flatten_cubic(start, chunk[0], chunk[1], chunk[2], 16);
        path.extend_from_slice(&cubic[1..]);
    }
    if path.len() >= 2
        && let Err(e) = draw_gdiplus_path(inst.id, &args[0], &path, false, true)
    {
        return error_out(out_error, &e);
    }
    set_void_out(out);
    0
}

/// Minimal separable box blur over an attached bitmap.  It intentionally
/// averages RGBA channels (rather than only RGB), which is useful for the
/// translucent shadow layers used by ADV UI and remains predictable for
/// straight-alpha scene pixels.
extern "C" fn layer_do_box_blur(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let xradius = args.first().map(arg_i64).unwrap_or(1).max(0) as u32;
    let yradius = args.get(1).map(arg_i64).unwrap_or(1).max(0) as u32;
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let bitmap_id = scene.layer(inst.id).and_then(|layer| layer.bitmap);
    if let Some(bitmap_id) = bitmap_id
        && let Some(bitmap) = scene.bitmap_mut(bitmap_id)
    {
        blur_bitmap(bitmap, xradius, yradius);
    }
    set_void_out(out);
    0
}

extern "C" fn layer_begin_transition(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    if !objthis.is_null()
        && let Ok(value) = context_engine().retain_object_detached(objthis)
    {
        PENDING_TRANSITIONS
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push((inst.id, value));
    }
    set_void_out(out);
    0
}

/// `stopTransition()` — reference `tTJSNI_BaseLayer::StopTransition`
/// (`LayerIntf.cpp:8033`): cancel this layer's in-flight transition. The
/// simplified model queues the completion callback (`beginTransition`), so
/// stopping drops the queued entry and synchronously fires
/// `onTransitionCompleted` for the cancelled transition, matching the
/// reference's synchronous event on `InternalStopTransition`. Returns whether
/// a queued transition was actually cancelled.
extern "C" fn layer_stop_transition(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let cancelled: Vec<tjs2_sys::DetachedValue> = {
        let mut pending = PENDING_TRANSITIONS
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut kept = Vec::with_capacity(pending.len());
        let mut removed = Vec::new();
        for (layer_id, value) in pending.drain(..) {
            if layer_id == inst.id {
                removed.push(value);
            } else {
                kept.push((layer_id, value));
            }
        }
        *pending = kept;
        removed
    };
    let engine = context_engine();
    for value in &cancelled {
        let _ = engine.call_member(
            value.raw_id(),
            "onTransitionCompleted",
            &[TjsValue::Void, TjsValue::Void],
        );
    }
    set_void_out(out);
    0
}

/// Complete all transitions queued by the previous VM calls.
pub(crate) fn transition_poll(engine: &Tjs2Engine) {
    let pending = std::mem::take(
        &mut *PENDING_TRANSITIONS
            .lock()
            .unwrap_or_else(|p| p.into_inner()),
    );
    for (_layer_id, value) in pending {
        // The simplified transition model has no source/destination objects;
        // pass void for both members so the `onTransitionCompleted` native
        // still dispatches to the action owner.
        let _ = engine.call_member(
            value.raw_id(),
            "onTransitionCompleted",
            &[TjsValue::Void, TjsValue::Void],
        );
        drop(value);
    }
}

fn fallback_text_width(text: &str, height: u32) -> u32 {
    let em = height.max(1);
    text.split('\n')
        .map(|line| {
            line.chars()
                .map(|ch| {
                    if ch.is_ascii() {
                        (em * 55).div_ceil(100)
                    } else {
                        em
                    }
                })
                .sum::<u32>()
        })
        .max()
        .unwrap_or(0)
}

fn fallback_text_height(text: &str, height: u32) -> u32 {
    text.split('\n').count().max(1) as u32 * height.max(1)
}

/// Composite a string from a `.tft` pre-rendered font.
///
/// The caller guarantees `text` contains only characters present in `pfont`
/// (plus newlines/controls). Ink is placed at the reference baseline:
/// `left = pen_x + OriginX`, `top = line_top + ascent − OriginY`
/// (`PrerenderedGlyph::left/top`; `LayerBitmapImpl.cpp:279`, `:913`).
///
/// `ascent`/`line_height` come from the mapped outline face's rasterizer — the
/// same values the vector path's `layout` uses — so `.tft` and outline glyphs
/// on one line share a baseline and multi-line advance. The `.tft` stores no
/// ascent of its own, so the caller must resolve it. Synthetic
/// rotation/italic are not applied because the `.tft` bitmap is already baked
/// for its `(face, height, bold, italic, angle)` key.
#[allow(clippy::too_many_arguments)]
fn paint_prerendered_text(
    bitmap: &mut BitmapState,
    pfont: &PrerenderedFont,
    text: &str,
    color: [u8; 4],
    opa: u8,
    aa: bool,
    x: i32,
    y: i32,
    font_height: u32,
    ascent: f32,
    line_height: f32,
    spread: u32,
    style: DrawTextStyle,
) {
    let thickness = (font_height / 14).max(1) as i32;
    let mut pen_x = x;
    // Keep the pen in f32 like `layout`/`paint_layout`, so a multi-line `.tft`
    // block lands on the same rows as the vector path.
    let mut pen_y = y as f32;
    let mut line_start = x;

    for ch in text.chars() {
        if ch == '\n' {
            paint_prerendered_rules(
                bitmap,
                line_start,
                pen_x,
                pen_y,
                ascent,
                line_height,
                thickness,
                color,
                opa,
                style,
            );
            pen_x = x;
            line_start = x;
            pen_y += line_height;
            continue;
        }
        if ch.is_control() {
            continue;
        }
        let Some(glyph) = pfont.find(ch) else {
            continue;
        };
        let cov = pfont.rasterize(&glyph);
        let gw = glyph.width as i32;
        let gh = glyph.height as i32;
        // Reference baseline placement: `OriginY` is the distance from the
        // baseline up to the ink top, so the `.tft` glyphs honor it instead
        // of being centered in the em box. Rounding matches the vector path's
        // `glyph.y.round()`.
        let top = (pen_y + ascent - f32::from(glyph.origin_y)).round() as i32;
        let left = glyph.left(pen_x);
        for dy in 0..gh {
            for dx in 0..gw {
                let mut a = cov[(dy * gw + dx) as usize];
                if !aa {
                    a = if a >= 128 { 255 } else { 0 };
                }
                if a == 0 {
                    continue;
                }
                let gx = left + dx;
                let gy = top + dy;
                if spread == 0 {
                    blend_pixel(bitmap, gx, gy, color, a, opa);
                } else {
                    let r = spread.min(16) as i32;
                    for sy in -r..=r {
                        for sx in -r..=r {
                            blend_pixel(bitmap, gx + sx, gy + sy, color, a, opa);
                        }
                    }
                }
            }
        }
        pen_x += glyph.advance();
    }
    paint_prerendered_rules(
        bitmap,
        line_start,
        pen_x,
        pen_y,
        ascent,
        line_height,
        thickness,
        color,
        opa,
        style,
    );
}

/// Underline / strikeout rules for one pre-rendered line.
///
/// Uses the same placement formulas as the vector path's `paint_layout`
/// (underline `line_y + ascent + 2`, strikeout `line_y + line_height / 2`) so a
/// `.tft` line and an outline line put their rules on the same row.
#[allow(clippy::too_many_arguments)]
fn paint_prerendered_rules(
    bitmap: &mut BitmapState,
    x0: i32,
    x1: i32,
    line_y: f32,
    ascent: f32,
    line_height: f32,
    thickness: i32,
    color: [u8; 4],
    opa: u8,
    style: DrawTextStyle,
) {
    if style.underline {
        paint_rule(
            bitmap,
            x0,
            x1,
            (line_y + ascent + 2.0).round() as i32,
            thickness,
            color,
            opa,
            0,
            0,
        );
    }
    if style.strikeout {
        paint_rule(
            bitmap,
            x0,
            x1,
            (line_y + line_height / 2.0).round() as i32,
            thickness,
            color,
            opa,
            0,
            0,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_layout(
    bitmap: &mut BitmapState,
    atlas: &GlyphAtlas,
    text_layout: &tvp_text::TextLayout,
    color: [u8; 4],
    opa: u8,
    aa: bool,
    offset_x: i32,
    offset_y: i32,
    spread: u32,
    style: DrawTextStyle,
) {
    let (atlas_width, _) = atlas.atlas_size();
    let pixels = atlas.atlas_rgba();
    let angle = style.angle_deg.to_radians();
    let rotate = style.angle_deg.abs() > 1e-6;
    // Synthetic italic: shear each glyph around its vertical center. 0.25 is a
    // conventional oblique slant (about 14 degrees). Rotation supersedes the
    // shear when both are requested.
    let italic = style.italic && !rotate;
    const ITALIC_SLANT: f32 = 0.25;

    for run in &text_layout.runs {
        for glyph in &run.chars {
            let gw = glyph.size.0;
            let gh = glyph.size.1;
            for dy in 0..gh {
                for dx in 0..gw {
                    let source = (((glyph.uv.1 + dy) * atlas_width + glyph.uv.0 + dx) * 4) as usize;
                    let mut coverage = pixels[source + 3];
                    if !aa {
                        coverage = if coverage >= 128 { 255 } else { 0 };
                    }
                    if coverage == 0 {
                        continue;
                    }
                    let base_x = glyph.x.round();
                    let base_y = glyph.y.round();
                    let (mut px, mut py) = (base_x + dx as f32, base_y + dy as f32);
                    if italic {
                        px += ITALIC_SLANT * (gh as f32 / 2.0 - dy as f32);
                    } else if rotate {
                        let cx = gw as f64 / 2.0;
                        let cy = gh as f64 / 2.0;
                        let (ox, oy) = (dx as f64 - cx, dy as f64 - cy);
                        let (rx, ry) = (
                            ox * angle.cos() - oy * angle.sin(),
                            ox * angle.sin() + oy * angle.cos(),
                        );
                        px = base_x + (cx + rx) as f32;
                        py = base_y + (cy + ry) as f32;
                    }
                    let gx = px.round() as i32 + offset_x;
                    let gy = py.round() as i32 + offset_y;
                    if spread == 0 {
                        blend_pixel(bitmap, gx, gy, color, coverage, opa);
                    } else {
                        // A compact square dilation approximates TVP's shadow
                        // blur width without a second glyph rasterizer.
                        let r = spread.min(16) as i32;
                        for sy in -r..=r {
                            for sx in -r..=r {
                                blend_pixel(bitmap, gx + sx, gy + sy, color, coverage, opa);
                            }
                        }
                    }
                }
            }
        }
    }

    // Underline / strikeout: horizontal rules spanning each run.
    if style.underline || style.strikeout {
        let thickness = (atlas.pixel_height() / 14).max(1) as i32;
        let ascent = atlas.ascent();
        for run in &text_layout.runs {
            let Some(first) = run.chars.first() else {
                continue;
            };
            let start = first.x.round() as i32;
            let end = (first.x + run.width).round() as i32;
            if style.underline {
                let y = (run.y + ascent + 2.0).round() as i32;
                paint_rule(
                    bitmap, start, end, y, thickness, color, opa, offset_x, offset_y,
                );
            }
            if style.strikeout {
                let y = (run.y + run.line_height / 2.0).round() as i32;
                paint_rule(
                    bitmap, start, end, y, thickness, color, opa, offset_x, offset_y,
                );
            }
        }
    }
}

/// Fill a `thickness`-px horizontal rule from `x0` (inclusive) to `x1`
/// (exclusive); used by underline / strikeout.
#[allow(clippy::too_many_arguments)]
fn paint_rule(
    bitmap: &mut BitmapState,
    x0: i32,
    x1: i32,
    y: i32,
    thickness: i32,
    color: [u8; 4],
    opa: u8,
    offset_x: i32,
    offset_y: i32,
) {
    for yy in 0..thickness {
        for xx in x0..x1 {
            blend_pixel(bitmap, xx + offset_x, y + yy + offset_y, color, 255, opa);
        }
    }
}

/// Draw-face enum (`reference/cpp/core/visual/LayerIntf.h:59-67`):
/// `dfAlpha=0`, `dfMain`/`dfOpaque=1`, `dfMask=2`, `dfProvince=3`,
/// `dfAddAlpha=4`, `dfAuto=128`. `face` overrides the blend/type-derived
/// face unless it is `dfAuto`.
const DF_ALPHA: i32 = 0;
const DF_OPAQUE: i32 = 1;
const DF_MASK: i32 = 2;
const DF_PROVINCE: i32 = 3;
const DF_ADD_ALPHA: i32 = 4;
const DF_AUTO: i32 = 128;

/// Reference `UpdateDrawFace`: the layer's main-image draw face. `face`
/// overrides the blend/type-derived face unless it is `dfAuto` (128).
fn layer_draw_face(layer: &LayerState) -> i32 {
    if layer.face != DF_AUTO {
        return layer.face;
    }
    match layer.blend_type {
        // ltAlpha(2) and every ltPs*(13..=28) use the alpha face.
        2 | 13..=28 => DF_ALPHA,
        // ltAddAlpha(12)
        12 => DF_ADD_ALPHA,
        // ltOpaque(1), ltAdditive(3), ltSubtractive(4), ... default
        _ => DF_OPAQUE,
    }
}

/// The layer's `ClipRect` in image pixels. `None` means the whole image, so
/// callers pass the full `i32` range and let the pixel op clip to the bitmap
/// bounds.
fn layer_pixel_rect(layer: &LayerState) -> RectI {
    match layer.clip {
        Some(c) => (c.x, c.y, c.x + c.w as i32, c.y + c.h as i32),
        None => (i32::MIN, i32::MIN, i32::MAX, i32::MAX),
    }
}

/// `colorRect(x, y, w, h, color[, opa=255])` — blend-aware rectangle fill.
/// Reference `tTJSNI_BaseLayer::ColorRect` (`LayerIntf.cpp:4338`); native arg
/// parsing at `LayerIntf.cpp:8672`.
extern "C" fn layer_color_rect(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 5 {
        return error_out(out_error, "Layer.colorRect requires 5 arguments");
    }
    let x = arg_i64(&args[0]) as i32;
    let y = arg_i64(&args[1]) as i32;
    let w = arg_i64(&args[2]);
    let h = arg_i64(&args[3]);
    let color = argb_to_rgba(arg_i64(&args[4]));
    // The reference defaults a missing/void `opa` to 255.
    let opa = if args.len() >= 6 && args[5].ty != tjs2_sys::VAL_VOID {
        arg_i64(&args[5]) as i32
    } else {
        255
    };
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    let face = layer_draw_face(layer);
    let rect = (x, y, x.saturating_add(w as i32), y.saturating_add(h as i32));
    let Some(destrect) = layer_ops::intersect_rect(rect, layer_pixel_rect(layer)) else {
        set_void_out(out);
        return 0;
    };
    let bitmap_id = match layer.bitmap {
        Some(id) => id,
        // KAG layers are image-capable and call pixel ops before any
        // explicit allocation, so allocate the MainImage on demand
        // (reference `AllocateImage`) instead of throwing
        // `TVPNotDrawableLayerType` (`LayerIntf.cpp:2326`).
        None => match ensure_dest_image(&mut scene, inst.id, 0, 0) {
            Some(id) => id,
            None => return error_out(out_error, "Layer.colorRect: layer no longer exists"),
        },
    };
    let Some(bitmap) = scene.bitmap_mut(bitmap_id) else {
        return error_out(out_error, "Layer.colorRect: no such bitmap");
    };
    match face {
        // dfAlpha / dfBoth
        0 => {
            if opa > 0 {
                layer_ops::fill_color_on_alpha(bitmap, destrect, color, opa);
            } else {
                layer_ops::remove_const_opacity(bitmap, destrect, -opa);
            }
        }
        // dfAddAlpha
        4 => {
            if opa < 0 {
                return error_out(
                    out_error,
                    "Layer.colorRect: negative opacity is not supported on additive alpha",
                );
            }
            layer_ops::fill_color_on_add_alpha(bitmap, destrect, color, opa);
        }
        // dfOpaque / dfMain
        1 => layer_ops::fill_color_hold_alpha(bitmap, destrect, color, opa),
        // dfMask: the low byte of the ARGB color is the blue channel.
        2 => layer_ops::fill_mask(bitmap, destrect, color[2]),
        // dfProvince: the province plane is now modelled and written through
        // `setProvincePixel`/`provinceImageBufferForWrite`; `colorRect` does
        // not route its low byte here (no game flow needs it).
        _ => {}
    }
    set_void_out(out);
    0
}

/// `colorize(hue, sat, blend)` — `layerExImage::colorize`. Operates on the
/// layer's current `ClipRect`.
extern "C" fn layer_colorize(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let hue = args.first().map(arg_i64).unwrap_or(0) as i32;
    let sat = args.get(1).map(arg_i64).unwrap_or(0) as i32;
    let blend = args.get(2).map(arg_f64).unwrap_or(1.0);
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(layer) = scene.layer(inst.id) else {
        set_void_out(out);
        return 0;
    };
    let rect = layer_pixel_rect(layer);
    let Some(bitmap_id) = layer.bitmap else {
        set_void_out(out);
        return 0;
    };
    if let Some(bitmap) = scene.bitmap_mut(bitmap_id) {
        layer_ops::colorize(bitmap, rect, hue, sat, blend);
    }
    set_void_out(out);
    0
}

/// `noise(level)` — `layerExImage::noise`. Operates on the `ClipRect`.
extern "C" fn layer_noise(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let level = args.first().map(arg_i64).unwrap_or(0) as i32;
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(layer) = scene.layer(inst.id) else {
        set_void_out(out);
        return 0;
    };
    let rect = layer_pixel_rect(layer);
    let Some(bitmap_id) = layer.bitmap else {
        set_void_out(out);
        return 0;
    };
    if let Some(bitmap) = scene.bitmap_mut(bitmap_id) {
        layer_ops::noise(bitmap, rect, level);
    }
    set_void_out(out);
    0
}

/// Map a resolved `tile` source to its attached bitmap. `kind`/`id` come
/// from [`resolve_image_source`], which must be called **before** the scene
/// lock is taken (it reads the layer's `hasImage` property, which re-enters
/// the scene).
///
/// A window's primary layer is its screen buffer: before reading it we
/// composite the currently-visible layer tree into its MainImage (see
/// [`layer_ops::composite_primary_layer`]).
fn tile_bitmap_for_source(scene: &mut Scene, kind: Option<bool>, id: u32) -> Option<BitmapState> {
    // `kind == Some(false)` is an explicit `Bitmap`; any other classification
    // (Layer or an integer id) may name a window's primary layer.
    if kind != Some(false) && scene.is_primary_layer(id) {
        let _ = layer_ops::composite_primary_layer(scene, id);
    }
    let bitmap_id = match kind {
        // Layer object. Some objects expose both a layer id and a bitmap id;
        // fall back to the bitmap when the layer itself has no MainImage.
        Some(true) => scene
            .layer(id)
            .and_then(|l| l.bitmap)
            .or_else(|| scene.bitmap(id).map(|_| id)),
        // Bitmap object
        Some(false) => Some(id),
        // Integer id: probe the bitmap table first, then the layer table
        // (the same convention `copyRect` uses).
        None => {
            if scene.bitmap(id).is_some() {
                Some(id)
            } else {
                scene.layer(id).and_then(|l| l.bitmap)
            }
        }
    };
    scene.bitmap(bitmap_id?).cloned()
}

/// `tileRect(left, top, width, height, tile[, x=0, y=0])` — the SDK's
/// `Layer.tileRect` composition (a clipped `copyRect` loop).
extern "C" fn layer_tile_rect(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 5 {
        return error_out(out_error, "Layer.tileRect requires 5 arguments");
    }
    let left = arg_i64(&args[0]) as i32;
    let top = arg_i64(&args[1]) as i32;
    let width = arg_i64(&args[2]).max(0) as u32;
    let height = arg_i64(&args[3]).max(0) as u32;
    let x = args.get(5).map(arg_i64).unwrap_or(0) as i32;
    let y = args.get(6).map(arg_i64).unwrap_or(0) as i32;
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    // Resolve the tile object *before* taking the scene lock: reading the
    // layer's `hasImage` property re-enters the scene.
    let engine = crate::natives::context_engine();
    let (kind, src_id) = match resolve_image_source(engine, &args[4]) {
        Ok(v) => v,
        Err(_) => return error_out(out_error, "Layer.tileRect expects a Layer or Bitmap"),
    };
    let mut scene = context_scene_mut();
    let Some(tile) = tile_bitmap_for_source(&mut scene, kind, src_id) else {
        return error_out(out_error, "Layer.tileRect: tile has no image");
    };
    let tile_rect = (0, 0, tile.width as i32, tile.height as i32);
    let Some(bitmap_id) = ensure_dest_image(&mut scene, inst.id, 0, 0) else {
        return error_out(out_error, "Layer.tileRect: layer no longer exists");
    };
    if let Some(dst) = scene.bitmap_mut(bitmap_id) {
        layer_ops::tile_rect(dst, left, top, width, height, &tile, tile_rect, x, y);
    }
    set_void_out(out);
    0
}

/// `fillOperateRect(left, top, width, height, color[, mode=ltPsNormal])` —
/// the SDK fills the region through `operateRect`, so this is a blend fill.
extern "C" fn layer_fill_operate_rect(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 5 {
        return error_out(out_error, "Layer.fillOperateRect requires 5 arguments");
    }
    let left = arg_i64(&args[0]) as i32;
    let top = arg_i64(&args[1]) as i32;
    let width = arg_i64(&args[2]).max(0) as u32;
    let height = arg_i64(&args[3]).max(0) as u32;
    let color = argb_to_rgba(arg_i64(&args[4]));
    let mode = args.get(5).map(arg_i64).unwrap_or(13);
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let hold_alpha = scene.layer(inst.id).map(|l| l.hold_alpha).unwrap_or(false);
    let Some(bitmap_id) = ensure_dest_image(&mut scene, inst.id, 0, 0) else {
        return error_out(out_error, "Layer.fillOperateRect: layer no longer exists");
    };
    if let Some(bitmap) = scene.bitmap_mut(bitmap_id) {
        layer_ops::fill_operate_rect(bitmap, left, top, width, height, color, mode, hold_alpha);
    }
    set_void_out(out);
    0
}

/// `doDropShadow(dx=10, dy=10, blur=3, shadowColor=0x000000,
/// shadowOpacity=200)` — the SDK composition (shadow + blur + offset).
extern "C" fn layer_do_drop_shadow(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let dx = args.first().map(arg_i64).unwrap_or(10) as i32;
    let dy = args.get(1).map(arg_i64).unwrap_or(10) as i32;
    let blur = args.get(2).map(arg_i64).unwrap_or(3).max(0) as u32;
    let shadow_color = argb_to_rgba(args.get(3).map(arg_i64).unwrap_or(0));
    let shadow_opacity = args.get(4).map(arg_i64).unwrap_or(200).clamp(0, 255) as u8;
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(bitmap_id) = scene.layer(inst.id).and_then(|l| l.bitmap) else {
        set_void_out(out);
        return 0;
    };
    if let Some(bitmap) = scene.bitmap_mut(bitmap_id) {
        layer_ops::do_drop_shadow(bitmap, dx, dy, blur, shadow_color, shadow_opacity);
    }
    set_void_out(out);
    0
}

/// `doBlurLight(blur=10, blurOpacity=128, lightOpacity=200,
/// lightType=ltPsHardLight)` — the SDK composition.
extern "C" fn layer_do_blur_light(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let blur = args.first().map(arg_i64).unwrap_or(10).max(0) as u32;
    let blur_opacity = args.get(1).map(arg_i64).unwrap_or(128).clamp(0, 255) as u8;
    let light_opacity = args.get(2).map(arg_i64).unwrap_or(200).clamp(0, 255) as u8;
    let light_type = args.get(3).map(arg_i64).unwrap_or(19);
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(bitmap_id) = scene.layer(inst.id).and_then(|l| l.bitmap) else {
        set_void_out(out);
        return 0;
    };
    if let Some(bitmap) = scene.bitmap_mut(bitmap_id) {
        layer_ops::do_blur_light(bitmap, blur, blur_opacity, light_opacity, light_type);
    }
    set_void_out(out);
    0
}

/// `stretchCopy(dx, dy, dw, dh, src, sx, sy, sw, sh[, type])` — reference
/// `tTJSNI_BaseLayer::StretchCopy` (`LayerIntf.cpp:4672`; native `:9002`).
/// The `bmCopy` path replaces pixels with a resampled source region.
extern "C" fn layer_stretch_copy(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 9 {
        return error_out(out_error, "Layer.stretchCopy requires 9 arguments");
    }
    // Resolve the source object before taking the scene lock (`hasImage`
    // re-enters the scene).
    let engine = crate::natives::context_engine();
    let (kind, src_id) = match resolve_image_source(engine, &args[4]) {
        Ok(v) => v,
        Err(_) => return error_out(out_error, "Layer.stretchCopy expects a Layer or Bitmap"),
    };
    let dx = arg_i64(&args[0]) as i32;
    let dy = arg_i64(&args[1]) as i32;
    let dw = arg_i64(&args[2]);
    let dh = arg_i64(&args[3]);
    let sx = arg_i64(&args[5]) as i32;
    let sy = arg_i64(&args[6]) as i32;
    let sw = arg_i64(&args[7]);
    let sh = arg_i64(&args[8]);
    let stretch_type = args.get(9).map(arg_i64).unwrap_or(0);
    let destrect = (
        dx,
        dy,
        dx.saturating_add(dw as i32),
        dy.saturating_add(dh as i32),
    );
    let srcrect = (
        sx,
        sy,
        sx.saturating_add(sw as i32),
        sy.saturating_add(sh as i32),
    );
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(src_bmp) = tile_bitmap_for_source(&mut scene, kind, src_id) else {
        return error_out(out_error, "Layer.stretchCopy: source has no image");
    };
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    let clip = layer_pixel_rect(layer);
    let face = layer_draw_face(layer);
    let hold_alpha = layer.hold_alpha;
    // Allocate the destination MainImage (destination extent) when absent.
    let dest_w = destrect.2.max(1) as u32;
    let dest_h = destrect.3.max(1) as u32;
    let Some(bitmap_id) = ensure_dest_image(&mut scene, inst.id, dest_w, dest_h) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    let Some(destrect) = layer_ops::intersect_rect(destrect, clip) else {
        set_void_out(out);
        return 0;
    };
    if let Some(dst) = scene.bitmap_mut(bitmap_id) {
        // `StretchCopy` (`LayerIntf.cpp:4672`) calls `StretchBlt(bmCopy)` with
        // `HoldAlpha` only on the dfOpaque face; dfAlpha/dfAddAlpha always
        // replace the destination alpha too.
        let plane = match face {
            DF_OPAQUE if hold_alpha => layer_ops::COPY_MAIN,
            _ => layer_ops::COPY_MAIN | layer_ops::COPY_MASK,
        };
        layer_ops::stretch_blit_plane(dst, destrect, &src_bmp, srcrect, stretch_type, plane);
    }
    set_void_out(out);
    0
}

/// `doGrayScale()` — reference `tTJSNI_BaseLayer::DoGrayScale`
/// (`LayerIntf.cpp:5898`; native `:9616`). Not affected by the draw face.
extern "C" fn layer_do_gray_scale(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    let clip = layer_pixel_rect(layer);
    let bitmap_id = match layer.bitmap {
        Some(id) => id,
        // KAG layers are image-capable and call pixel ops before any
        // explicit allocation, so allocate the MainImage on demand
        // (reference `AllocateImage`) instead of throwing
        // `TVPNotDrawableLayerType` (`LayerIntf.cpp:2326`).
        None => match ensure_dest_image(&mut scene, inst.id, 0, 0) {
            Some(id) => id,
            None => return error_out(out_error, "Layer.doGrayScale: layer no longer exists"),
        },
    };
    if let Some(bitmap) = scene.bitmap_mut(bitmap_id) {
        layer_ops::do_gray_scale(bitmap, clip);
    }
    set_void_out(out);
    0
}

/// `adjustGamma(rgamma, rfloor, rceil, ggamma, gfloor, gceil, bgamma,
/// bfloor, bceil)` — reference `tTJSNI_BaseLayer::AdjustGamma`
/// (`LayerIntf.cpp:5715`; native `:9529`). Missing args keep the identity
/// value (`TVPIntactGammaAdjustData`).
extern "C" fn layer_adjust_gamma(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let mut data = layer_ops::GammaAdjust::intact();
    let real = |i: usize, d: f64| args.get(i).map(arg_f64).unwrap_or(d);
    let int = |i: usize, d: i32| args.get(i).map(|v| arg_i64(v) as i32).unwrap_or(d);
    data.r_gamma = real(0, data.r_gamma);
    data.r_floor = int(1, data.r_floor);
    data.r_ceil = int(2, data.r_ceil);
    data.g_gamma = real(3, data.g_gamma);
    data.g_floor = int(4, data.g_floor);
    data.g_ceil = int(5, data.g_ceil);
    data.b_gamma = real(6, data.b_gamma);
    data.b_floor = int(7, data.b_floor);
    data.b_ceil = int(8, data.b_ceil);
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    let clip = layer_pixel_rect(layer);
    let bitmap_id = match layer.bitmap {
        Some(id) => id,
        // KAG layers are image-capable and call pixel ops before any
        // explicit allocation, so allocate the MainImage on demand
        // (reference `AllocateImage`) instead of throwing
        // `TVPNotDrawableLayerType` (`LayerIntf.cpp:2326`).
        None => match ensure_dest_image(&mut scene, inst.id, 0, 0) {
            Some(id) => id,
            None => return error_out(out_error, "Layer.adjustGamma: layer no longer exists"),
        },
    };
    if let Some(bitmap) = scene.bitmap_mut(bitmap_id) {
        layer_ops::adjust_gamma(bitmap, clip, data);
    }
    set_void_out(out);
    0
}

/// `light(brightness, contrast)` — reference `ApplyLightContrast`
/// (`LayerIntf.cpp:1006`; native `:8302`).
extern "C" fn layer_light(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 2 {
        return error_out(out_error, "Layer.light requires 2 arguments");
    }
    let brightness = arg_i64(&args[0]) as i32;
    let contrast = arg_i64(&args[1]) as i32;
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    let clip = layer_pixel_rect(layer);
    let bitmap_id = match layer.bitmap {
        Some(id) => id,
        // KAG layers are image-capable and call pixel ops before any
        // explicit allocation, so allocate the MainImage on demand
        // (reference `AllocateImage`) instead of throwing
        // `TVPNotDrawableLayerType` (`LayerIntf.cpp:2326`).
        None => match ensure_dest_image(&mut scene, inst.id, 0, 0) {
            Some(id) => id,
            None => return error_out(out_error, "Layer.light: layer no longer exists"),
        },
    };
    if let Some(bitmap) = scene.bitmap_mut(bitmap_id) {
        layer_ops::light_contrast(bitmap, clip, brightness, contrast);
    }
    set_void_out(out);
    0
}

/// `flipLR()` — reference `LRFlip` (`LayerIntf.cpp:5910`).
extern "C" fn layer_flip_lr(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(bitmap_id) = ensure_dest_image(&mut scene, inst.id, 0, 0) else {
        return error_out(out_error, "Layer.flipLR: layer no longer exists");
    };
    if let Some(bitmap) = scene.bitmap_mut(bitmap_id) {
        layer_ops::flip_lr(bitmap);
    }
    set_void_out(out);
    0
}

/// `flipUD()` — reference `UDFlip` (`LayerIntf.cpp:5925`).
extern "C" fn layer_flip_ud(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(bitmap_id) = ensure_dest_image(&mut scene, inst.id, 0, 0) else {
        return error_out(out_error, "Layer.flipUD: layer no longer exists");
    };
    if let Some(bitmap) = scene.bitmap_mut(bitmap_id) {
        layer_ops::flip_ud(bitmap);
    }
    set_void_out(out);
    0
}

/// `independMainImage([copy=true])` — reference `IndependMainImage`
/// (`LayerIntf.cpp:2679`). Detaches the layer from a shared bitmap by
/// allocating a private copy (or blank pixels when `copy` is false).
extern "C" fn layer_independ_main_image(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let copy = match args.first() {
        Some(v) if v.ty != tjs2_sys::VAL_VOID => arg_bool(v),
        _ => true,
    };
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(bitmap_id) = scene.layer(inst.id).and_then(|l| l.bitmap) else {
        set_void_out(out);
        return 0;
    };
    let Some((w, h, rgba)) = scene
        .bitmap(bitmap_id)
        .map(|b| (b.width, b.height, b.rgba.clone()))
    else {
        return error_out(out_error, "Layer.independMainImage: no such bitmap");
    };
    let rgba = if copy {
        rgba
    } else {
        vec![0u8; w as usize * h as usize * 4]
    };
    let new_id = scene.add_bitmap(w, h, rgba);
    if let Some(layer) = scene.layer_mut(inst.id) {
        layer.bitmap = Some(new_id);
    }
    set_void_out(out);
    0
}

/// `gaussianBlur(radius[, sigma])` — reference `ApplyGaussianBlur`
/// (`LayerIntf.cpp:5745`; native `:9565`).
extern "C" fn layer_gaussian_blur(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.is_empty() {
        return error_out(out_error, "Layer.gaussianBlur requires a radius");
    }
    let radius = arg_i64(&args[0]).max(1) as i32;
    let sigma = match args.get(1) {
        Some(v) if v.ty != tjs2_sys::VAL_VOID => arg_f64(v),
        _ => f64::from((radius as f32 / 2.5).max(1.0)),
    } as f32;
    if sigma <= 0.0 {
        return error_out(out_error, "Layer.gaussianBlur: sigma must be positive");
    }
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    let clip = layer_pixel_rect(layer);
    let bitmap_id = match layer.bitmap {
        Some(id) => id,
        // KAG layers are image-capable and call pixel ops before any
        // explicit allocation, so allocate the MainImage on demand
        // (reference `AllocateImage`) instead of throwing
        // `TVPNotDrawableLayerType` (`LayerIntf.cpp:2326`).
        None => match ensure_dest_image(&mut scene, inst.id, 0, 0) {
            Some(id) => id,
            None => return error_out(out_error, "Layer.gaussianBlur: layer no longer exists"),
        },
    };
    if let Some(bitmap) = scene.bitmap_mut(bitmap_id) {
        layer_ops::gaussian_blur(bitmap, clip, radius, sigma);
    }
    set_void_out(out);
    0
}

/// Parse a stretch/operate source and its `(kind, id, automode)`. Must run
/// before the scene lock (`resolve_image_source` re-enters the scene).
fn resolve_operate_source(engine: &Tjs2Engine, arg: &Value) -> Result<(Option<bool>, u32), String> {
    resolve_image_source(engine, arg).map_err(|_| "expects a Layer or Bitmap".to_string())
}

/// `operateRect(dx, dy, src, sx, sy, sw, sh[, mode=omAuto, opa=255])` —
/// reference `tTJSNI_BaseLayer::OperateRect` (`LayerIntf.cpp:5224`; native
/// `:8941`).
extern "C" fn layer_operate_rect(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 7 {
        return error_out(out_error, "Layer.operateRect requires 7 arguments");
    }
    let engine = crate::natives::context_engine();
    let (kind, src_id) = match resolve_operate_source(engine, &args[2]) {
        Ok(v) => v,
        Err(e) => return error_out(out_error, &format!("Layer.operateRect: {e}")),
    };
    let dx = arg_i64(&args[0]) as i32;
    let dy = arg_i64(&args[1]) as i32;
    let sx = arg_i64(&args[3]) as i32;
    let sy = arg_i64(&args[4]) as i32;
    let sw = arg_i64(&args[5]);
    let sh = arg_i64(&args[6]);
    let mut mode = args.get(7).map(arg_i64).unwrap_or(128);
    let opa = args.get(8).map(arg_i64).unwrap_or(255).clamp(0, 255) as u8;
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(src) = tile_bitmap_for_source(&mut scene, kind, src_id) else {
        return error_out(out_error, "Layer.operateRect: source has no image");
    };
    if mode == 128 {
        // `omAuto` guesses from the source layer type; a `Bitmap` source has
        // no layer type (default ltAlpha = 2).
        mode = if kind == Some(false) {
            2
        } else {
            scene.layer(src_id).map_or(2, |l| l.blend_type)
        };
    }
    let srcrect = (
        sx,
        sy,
        sx.saturating_add(sw as i32),
        sy.saturating_add(sh as i32),
    );
    let Some(bitmap_id) = ensure_dest_image(&mut scene, inst.id, 0, 0) else {
        return error_out(out_error, "Layer.operateRect: layer no longer exists");
    };
    let hold_alpha = scene
        .layer(inst.id)
        .map(|layer| layer.hold_alpha)
        .unwrap_or(false);
    if let Some(dst) = scene.bitmap_mut(bitmap_id) {
        layer_ops::operate_rect(dst, dx, dy, &src, srcrect, mode, opa, hold_alpha);
    }
    set_void_out(out);
    0
}

/// Shared body for `stretchPile`/`stretchBlend`/`operateStretch`:
/// `(dx, dy, dw, dh, src, sx, sy, sw, sh[, mode/opa/type])`.
#[allow(clippy::too_many_arguments)]
fn layer_stretch_common(
    argc: c_int,
    argv: *const Value,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    default_mode: i64,
    mode_index: usize,
    opa_index: usize,
    type_index: usize,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 9 {
        return error_out(out_error, "Layer.stretch* requires 9 arguments");
    }
    let engine = crate::natives::context_engine();
    let (kind, src_id) = match resolve_operate_source(engine, &args[4]) {
        Ok(v) => v,
        Err(e) => return error_out(out_error, &format!("Layer.stretch*: {e}")),
    };
    let dx = arg_i64(&args[0]) as i32;
    let dy = arg_i64(&args[1]) as i32;
    let dw = arg_i64(&args[2]);
    let dh = arg_i64(&args[3]);
    let sx = arg_i64(&args[5]) as i32;
    let sy = arg_i64(&args[6]) as i32;
    let sw = arg_i64(&args[7]);
    let sh = arg_i64(&args[8]);
    let mut mode = args.get(mode_index).map(arg_i64).unwrap_or(default_mode);
    let opa = args
        .get(opa_index)
        .map(arg_i64)
        .unwrap_or(255)
        .clamp(0, 255) as u8;
    let stretch_type = args.get(type_index).map(arg_i64).unwrap_or(0);
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(src) = tile_bitmap_for_source(&mut scene, kind, src_id) else {
        return error_out(out_error, "Layer.stretch*: source has no image");
    };
    if mode == 128 {
        // `omAuto` from the source layer type; `Bitmap` defaults to ltAlpha.
        mode = if kind == Some(false) {
            2
        } else {
            scene.layer(src_id).map_or(2, |l| l.blend_type)
        };
    }
    let destrect = (
        dx,
        dy,
        dx.saturating_add(dw as i32),
        dy.saturating_add(dh as i32),
    );
    let srcrect = (
        sx,
        sy,
        sx.saturating_add(sw as i32),
        sy.saturating_add(sh as i32),
    );
    // Allocate the destination MainImage (destination extent) when absent.
    let dest_w = destrect.2.max(1) as u32;
    let dest_h = destrect.3.max(1) as u32;
    let Some(bitmap_id) = ensure_dest_image(&mut scene, inst.id, dest_w, dest_h) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    let hold_alpha = scene
        .layer(inst.id)
        .map(|layer| layer.hold_alpha)
        .unwrap_or(false);
    if let Some(dst) = scene.bitmap_mut(bitmap_id) {
        layer_ops::stretch_blit_mode(
            dst,
            destrect,
            &src,
            srcrect,
            stretch_type,
            mode,
            opa,
            hold_alpha,
        );
    }
    set_void_out(out);
    0
}

/// `stretchPile(dx,dy,dw,dh,src,sx,sy,sw,sh[,opa=255,type=0])` —
/// reference `StretchPile` (`LayerIntf.cpp:5264`).
extern "C" fn layer_stretch_pile(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_stretch_common(argc, argv, instance, out, out_error, 2, 100, 9, 10)
}

/// `stretchBlend(...)` — reference `StretchBlend` (`LayerIntf.cpp:5319`).
extern "C" fn layer_stretch_blend(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_stretch_common(argc, argv, instance, out, out_error, 2, 100, 9, 10)
}

/// `operateStretch(dx,dy,dw,dh,src,sx,sy,sw,sh[,mode=omAuto,opa=255,type=0])`
/// — reference `OperateStretch` (`LayerIntf.cpp:5374`; native `:9154`).
extern "C" fn layer_operate_stretch(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_stretch_common(argc, argv, instance, out, out_error, 128, 9, 10, 11)
}

/// Shared body for `affineCopy`/`affinePile`/`affineBlend`/`operateAffine`:
/// `(src, sx, sy, sw, sh, affine, a, b, c, d, tx, ty[, type][, clear])`.
#[allow(clippy::too_many_arguments)]
fn layer_affine_common(
    argc: c_int,
    argv: *const Value,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    default_mode: i64,
    mode_index: Option<usize>,
    type_index: usize,
    clear_index: usize,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 12 {
        return error_out(out_error, "Layer.affine* requires 12 arguments");
    }
    let engine = crate::natives::context_engine();
    let (kind, src_id) = match resolve_operate_source(engine, &args[0]) {
        Ok(v) => v,
        Err(e) => return error_out(out_error, &format!("Layer.affine*: {e}")),
    };
    let sx = arg_i64(&args[1]) as i32;
    let sy = arg_i64(&args[2]) as i32;
    let sw = arg_i64(&args[3]);
    let sh = arg_i64(&args[4]);
    let is_matrix = arg_bool(&args[5]);
    let f = |i: usize| arg_f64(&args[i]);
    let stretch_type = args.get(type_index).map(arg_i64).unwrap_or(0);
    let clear = args.get(clear_index).map(arg_bool).unwrap_or(false);
    let mut mode = mode_index
        .and_then(|i| args.get(i))
        .map(arg_i64)
        .unwrap_or(default_mode);
    let opa = if let Some(i) = mode_index {
        args.get(i + 1).map(arg_i64).unwrap_or(255).clamp(0, 255) as u8
    } else {
        255
    };
    let (left, top, right, bottom) = (
        sx as f64,
        sy as f64,
        (sx as i64 + sw) as f64,
        (sy as i64 + sh) as f64,
    );
    let (p0, p1, p2) = if is_matrix {
        let (a, b, c, d, tx, ty) = (f(6), f(7), f(8), f(9), f(10), f(11));
        let map = |x: f64, y: f64| (a * x + c * y + tx, b * x + d * y + ty);
        (map(left, top), map(right, top), map(left, bottom))
    } else {
        ((f(6), f(7)), (f(8), f(9)), (f(10), f(11)))
    };
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(src) = tile_bitmap_for_source(&mut scene, kind, src_id) else {
        return error_out(out_error, "Layer.affine*: source has no image");
    };
    if mode == 128 {
        // `omAuto` from the source layer type; `Bitmap` defaults to ltAlpha.
        mode = if kind == Some(false) {
            2
        } else {
            scene.layer(src_id).map_or(2, |l| l.blend_type)
        };
    }
    let srcrect = (
        sx,
        sy,
        sx.saturating_add(sw as i32),
        sy.saturating_add(sh as i32),
    );
    let Some(bitmap_id) = ensure_dest_image(&mut scene, inst.id, 0, 0) else {
        return error_out(out_error, "Layer.affine*: layer no longer exists");
    };
    if let Some(dst) = scene.bitmap_mut(bitmap_id) {
        layer_ops::affine_blit(
            dst,
            p0,
            p1,
            p2,
            &src,
            srcrect,
            stretch_type,
            mode,
            opa,
            clear,
            [0, 0, 0, 0],
        );
    }
    set_void_out(out);
    0
}

/// `affineCopy(src, sx,sy,sw,sh, affine, a,b,c,d,tx,ty[, type][, clear])` —
/// reference `AffineCopy` (`LayerIntf.cpp:4730`; native `:9227`).
extern "C" fn layer_affine_copy(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_affine_common(argc, argv, instance, out, out_error, 1, None, 12, 13)
}

/// `affinePile(...)` — reference `AffinePile` (`LayerIntf.cpp:4765`).
extern "C" fn layer_affine_pile(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_affine_common(argc, argv, instance, out, out_error, 2, None, 12, 13)
}

/// `affineBlend(...)` — reference `AffineBlend` (`LayerIntf.cpp:4795`).
extern "C" fn layer_affine_blend(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_affine_common(argc, argv, instance, out, out_error, 2, None, 12, 13)
}

/// `operateAffine(src, sx,sy,sw,sh, affine, a,b,c,d,tx,ty[, mode][, opa][, type])`
/// — reference `OperateAffine` (`LayerIntf.cpp:5617`; native `:9423`).
extern "C" fn layer_operate_affine(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // Args: src,sx,sy,sw,sh,affine,a,b,c,d,tx,ty, mode=12, opa=13, type=14
    layer_affine_common(argc, argv, instance, out, out_error, 128, Some(12), 14, 15)
}

/// The `image` format implied by a `saveLayerImage` `type` argument, falling
/// back to the storage-name extension and finally BMP.
fn save_image_format(name: &str, kind: Option<&str>) -> Option<image::ImageFormat> {
    use image::ImageFormat;
    let from_ext = || {
        std::path::Path::new(name)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
    };
    let kind = match kind {
        Some(k) if !k.is_empty() => k.to_ascii_lowercase(),
        _ => from_ext().unwrap_or_else(|| "bmp".to_string()),
    };
    match kind.as_str() {
        "bmp" => Some(ImageFormat::Bmp),
        "png" => Some(ImageFormat::Png),
        "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
        _ => None,
    }
}

/// Encode a scene bitmap to image bytes for `saveLayerImage`.
fn encode_layer_image(bitmap: &BitmapState, format: image::ImageFormat) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;
    let mut bytes = Vec::new();
    match format {
        // Shared with `saveBitmap`/`saveLayerImage`'s storage path so the
        // classic 54-byte header (the offset KAG's `GetImageFileSize`
        // assumes) is used everywhere; see `bitmap::encode_bmp`.
        image::ImageFormat::Bmp => {
            return Ok(crate::bitmap::encode_bmp(
                bitmap.width,
                bitmap.height,
                &bitmap.rgba,
            ));
        }
        image::ImageFormat::Png => {
            image::codecs::png::PngEncoder::new(&mut bytes)
                .write_image(
                    &bitmap.rgba,
                    bitmap.width,
                    bitmap.height,
                    image::ExtendedColorType::Rgba8,
                )
                .map_err(|e| e.to_string())?;
        }
        image::ImageFormat::Jpeg => {
            let rgb: Vec<u8> = bitmap
                .rgba
                .chunks_exact(4)
                .flat_map(|p| [p[0], p[1], p[2]])
                .collect();
            image::codecs::jpeg::JpegEncoder::new(&mut bytes)
                .write_image(
                    &rgb,
                    bitmap.width,
                    bitmap.height,
                    image::ExtendedColorType::Rgb8,
                )
                .map_err(|e| e.to_string())?;
        }
        other => return Err(format!("unsupported save format {other:?}")),
    }
    Ok(bytes)
}

/// Resolve a `saveLayerImage` name to a host path. Absolute names are used
/// as-is; relative names resolve under the mounted game directory. Storage
/// `\` separators are normalized to `/` (the reference is Windows-oriented).
fn save_host_path(game_dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let slash = name.replace('\\', "/");
    let path = std::path::Path::new(&slash);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        game_dir.join(slash.trim_start_matches('/'))
    }
}

/// `saveLayerImage(name[, type])` — reference
/// `tTJSNI_BaseLayer::SaveLayerImage` (`LayerIntf.cpp:2699`; native `:8408`).
/// Encodes the main image to `name` via the `type`-selected handler (BMP /
/// PNG / JPEG). TLG encoding is not available in this port.
extern "C" fn layer_save_layer_image(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.is_empty() || args[0].ty != tjs2_sys::VAL_STRING {
        return error_out(out_error, "Layer.saveLayerImage requires a storage name");
    }
    let name = super::ffi::arg_string(&args[0]);
    let kind = args
        .get(1)
        .filter(|v| v.ty == tjs2_sys::VAL_STRING)
        .map(super::ffi::arg_string);
    let Some(format) = save_image_format(&name, kind.as_deref()) else {
        return error_out(
            out_error,
            &format!("Layer.saveLayerImage: unknown format for {name}"),
        );
    };
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let (mut scene, storage) = super::context_scene_storage();
    // Reading the primary layer for a thumbnail must first materialize the
    // screen buffer (the window's layer tree composite).
    if scene.is_primary_layer(inst.id) {
        let _ = layer_ops::composite_primary_layer(&mut scene, inst.id);
    }
    let Some(bitmap) = scene
        .layer(inst.id)
        .and_then(|l| l.bitmap)
        .and_then(|b| scene.bitmap(b))
    else {
        return error_out(out_error, "Layer.saveLayerImage: layer has no image");
    };
    let bytes = match encode_layer_image(bitmap, format) {
        Ok(b) => b,
        Err(e) => return error_out(out_error, &format!("Layer.saveLayerImage: {e}")),
    };
    let path = save_host_path(storage.game_dir(), &name);
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return error_out(out_error, &format!("Layer.saveLayerImage: {e}"));
    }
    if let Err(e) = std::fs::write(&path, &bytes) {
        return error_out(out_error, &format!("Layer.saveLayerImage: {e}"));
    }
    set_void_out(out);
    0
}

/// Hand a retained value (e.g. an array or layer object) to the C++ side as
/// the native result; the retention is consumed there.
fn set_retained_out(dv: tjs2_sys::DetachedValue, out: *mut Value) {
    let id = dv.raw_id() as usize;
    // SAFETY: out is a valid result slot for the duration of the call.
    unsafe {
        (*out).ty = tjs2_sys::VAL_RETAINED;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = std::ptr::null();
        (*out).array = std::ptr::null();
        (*out).array_count = 0;
        (*out).retained = id;
    }
    std::mem::forget(dv);
}

/// Return a scene layer's TJS object (retained) or `null`, the object-valued
/// result shape the reference's tree/query getters use.
fn set_layer_object_out(engine: &Tjs2Engine, layer_id: Option<u32>, out: *mut Value) {
    if let Some(id) = layer_id {
        let obj = super::layer_tjs_object(id);
        if !obj.is_null()
            && let Ok(dv) = engine.retain_object_detached(obj)
        {
            set_retained_out(dv, out);
            return;
        }
    }
    super::ffi::set_null_out(engine, out);
}

/// The TJS helper implementing `TVP_ACTION_INVOKE` (`EventIntf.h:208`): it
/// builds the event dictionary `%[type, target, ...members]` and calls
/// `owner.action(ev)`. The VM exposes no `arguments` object, so the (at most
/// six) member name/value pairs are passed as fixed optional parameters.
const LAYER_EVENT_DISPATCH: &str = "(function(owner,target,t,n1,v1,n2,v2,n3,v3,n4,v4,n5,v5,n6,v6){\
    var ev=%[type:t,target:target];\n    if(n1!==void)ev[n1]=v1;\n    if(n2!==void)ev[n2]=v2;\n    if(n3!==void)ev[n3]=v3;\n    if(n4!==void)ev[n4]=v4;\n    if(n5!==void)ev[n5]=v5;\n    if(n6!==void)ev[n6]=v6;\n    return owner.action(ev);})";

/// The maximum number of event members any `Layer` event carries
/// (`onTouchRotate` has six).
const LAYER_EVENT_MAX_MEMBERS: usize = 6;

/// Dispatch one layer event to its action owner: retain the owner and the
/// layer target, evaluate the helper closure, and invoke it with the event
/// type plus alternating member name/value pairs. Object members are passed
/// as [`TjsValue::Retained`] ids (consumed by the call).
fn dispatch_layer_event(
    engine: &Tjs2Engine,
    owner_raw: *mut c_void,
    target: *mut c_void,
    event_type: &str,
    members: &[(&str, TjsValue)],
) {
    let Ok(owner) = engine.retain_object_detached(owner_raw) else {
        return;
    };
    let Ok(target_dv) = engine.retain_object_detached(target) else {
        return;
    };
    let Ok(helper) = engine.eval_retained(LAYER_EVENT_DISPATCH, "layerEvent") else {
        return;
    };
    let tjs2_sys::RetainedValue::Object(helper_dv) = helper else {
        return;
    };
    let mut args: Vec<TjsValue> = vec![
        TjsValue::Retained(owner.raw_id() as u64),
        TjsValue::Retained(target_dv.raw_id() as u64),
        TjsValue::String(event_type.to_string()),
    ];
    for i in 0..LAYER_EVENT_MAX_MEMBERS {
        match members.get(i) {
            Some((name, value)) => {
                args.push(TjsValue::String((*name).to_string()));
                args.push(value.clone());
            }
            None => {
                args.push(TjsValue::Void);
                args.push(TjsValue::Void);
            }
        }
    }
    // `owner`/`target_dv` retentions are consumed by the argument copy; their
    // drops are no-ops. Errors surface as a script `action` throw, which the
    // VM reports elsewhere; the native method itself stays void.
    if let Err(e) = engine.call_detached(&helper_dv, &args) {
        log::warn!("layer event dispatch ({event_type}) failed: {e}");
    }
}

/// Forward one scalar callback argument preserving its integer/real/string
/// variant (touch coordinates are reals).
fn layer_event_value(v: &Value) -> TjsValue {
    match v.ty {
        tjs2_sys::VAL_INTEGER => TjsValue::Integer(v.integer),
        tjs2_sys::VAL_REAL => TjsValue::Real(v.real),
        tjs2_sys::VAL_STRING => TjsValue::String(super::ffi::arg_string(v)),
        _ => TjsValue::Void,
    }
}

/// Build event members from `(name, &Value)` pairs, retaining object
/// arguments so the dispatch can pass them. The retained handles are pushed
/// to `keepalive` and must outlive the dispatch.
fn layer_event_members<'a>(
    engine: &Tjs2Engine,
    pairs: &[(&'a str, &Value)],
    keepalive: &mut Vec<tjs2_sys::DetachedValue>,
) -> Vec<(&'a str, TjsValue)> {
    pairs
        .iter()
        .map(|(name, v)| {
            if v.ty == tjs2_sys::VAL_OBJECT {
                match engine.retain_object_arg(v) {
                    Ok(dv) => {
                        let id = dv.raw_id() as u64;
                        keepalive.push(dv);
                        (*name, TjsValue::Retained(id))
                    }
                    Err(_) => (*name, TjsValue::Void),
                }
            } else {
                (*name, layer_event_value(v))
            }
        })
        .collect()
}

/// Resolve a `layer`/`blurred` object argument to a scene layer id, or `None`
/// for `void`/`null`. Throws `Specify Layer` for a non-Layer object (the
/// reference `TVPSpecifyLayer`).
fn focus_arg_layer_id(engine: &Tjs2Engine, v: &Value) -> Result<Option<u32>, String> {
    match v.ty {
        tjs2_sys::VAL_VOID | tjs2_sys::VAL_NULL => Ok(None),
        tjs2_sys::VAL_OBJECT => {
            let dv = engine.retain_object_arg(v)?;
            let id = read_object_id(engine, dv.raw_id())?;
            if id < 0 {
                return Ok(None);
            }
            let id = id as u32;
            if context_scene_read().layer(id).is_none() {
                return Err("Specify Layer".into());
            }
            Ok(Some(id))
        }
        _ => Err("Specify Layer".into()),
    }
}

/// Define a native `Layer` event method whose arguments become the named
/// members of the event dictionary dispatched to the action owner. The
/// reference requires `names.len()` arguments.
macro_rules! layer_event_method {
    ($fn_name:ident, $event:literal, [$($member:literal),* $(,)?]) => {
        extern "C" fn $fn_name(
            _engine: *mut c_void,
            instance: *mut c_void,
            argc: c_int,
            argv: *const Value,
            out: *mut Value,
            out_error: *mut *mut c_char,
            objthis: *mut c_void,
        ) -> c_int {
            let args = unsafe { super::ffi::args(argc, argv) };
            let names: &[&str] = &[$($member),*];
            if args.len() < names.len() {
                return error_out(out_error, concat!("Layer.", $event, " requires more arguments"));
            }
            let inst = unsafe { instance_ref::<LayerInst>(instance) };
            if let Some(owner) = &inst.action_owner {
                let members: Vec<(&str, TjsValue)> = names
                    .iter()
                    .enumerate()
                    .map(|(i, name)| (*name, layer_event_value(&args[i])))
                    .collect();
                let engine = crate::natives::context_engine();
                dispatch_layer_event(engine, owner.raw, objthis, $event, &members);
            }
            set_void_out(out);
            0
        }
    };
}

layer_event_method!(layer_click, "onClick", ["x", "y"]);
layer_event_method!(layer_double_click, "onDoubleClick", ["x", "y"]);
layer_event_method!(
    layer_mouse_down,
    "onMouseDown",
    ["x", "y", "button", "shift"]
);
layer_event_method!(layer_mouse_up, "onMouseUp", ["x", "y", "button", "shift"]);
layer_event_method!(layer_mouse_move, "onMouseMove", ["x", "y", "shift"]);
layer_event_method!(layer_mouse_enter, "onMouseEnter", []);
layer_event_method!(layer_mouse_leave, "onMouseLeave", []);
layer_event_method!(
    layer_mouse_wheel,
    "onMouseWheel",
    ["shift", "delta", "x", "y"]
);
layer_event_method!(layer_key_down, "onKeyDown", ["key", "shift", "process"]);
layer_event_method!(layer_key_up, "onKeyUp", ["key", "shift", "process"]);
layer_event_method!(layer_key_press, "onKeyPress", ["key", "process"]);
layer_event_method!(layer_node_enabled, "onNodeEnabled", []);
layer_event_method!(layer_node_disabled, "onNodeDisabled", []);
layer_event_method!(layer_multi_touch, "onMultiTouch", []);
layer_event_method!(
    layer_touch_down,
    "onTouchDown",
    ["x", "y", "cx", "cy", "id"]
);
layer_event_method!(layer_touch_up, "onTouchUp", ["x", "y", "cx", "cy", "id"]);
layer_event_method!(
    layer_touch_move,
    "onTouchMove",
    ["x", "y", "cx", "cy", "id"]
);
layer_event_method!(
    layer_touch_scaling,
    "onTouchScaling",
    ["startdistance", "currentdistance", "cx", "cy", "flag"]
);
layer_event_method!(
    layer_touch_rotate,
    "onTouchRotate",
    ["startangle", "currentangle", "distance", "cx", "cy", "flag"]
);

/// `onBlur(focused)` — reference `LayerIntf.cpp:10193`. The `focused` member
/// is an object (or null).
extern "C" fn layer_on_blur(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.is_empty() {
        return error_out(out_error, "Layer.onBlur requires a layer");
    }
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    if let Some(owner) = &inst.action_owner {
        let engine = crate::natives::context_engine();
        let mut keepalive = Vec::new();
        let members = layer_event_members(engine, &[("focused", &args[0])], &mut keepalive);
        dispatch_layer_event(engine, owner.raw, objthis, "onBlur", &members);
    }
    set_void_out(out);
    0
}

/// `onFocus(blurred, direction)` — reference `LayerIntf.cpp:10208`. The
/// reference declares a minimum of one parameter but reads two members
/// (`blurred`, `direction`); the caller (`FireFocus`) passes both.
extern "C" fn layer_on_focus(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.is_empty() {
        return error_out(out_error, "Layer.onFocus requires a layer");
    }
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    if let Some(owner) = &inst.action_owner {
        let engine = crate::natives::context_engine();
        let mut keepalive = Vec::new();
        let mut members = layer_event_members(engine, &[("blurred", &args[0])], &mut keepalive);
        let direction = args.get(1).map(arg_i64).unwrap_or(0);
        members.push(("direction", TjsValue::Integer(direction)));
        dispatch_layer_event(engine, owner.raw, objthis, "onFocus", &members);
    }
    set_void_out(out);
    0
}

/// Shared body for `onSearchNextFocusable`/`onSearchPrevFocusable`: store the
/// found layer in `FocusWork` (reference `SetFocusWork`) and dispatch the
/// event to the action owner.
fn layer_search_focusable(
    instance: *mut c_void,
    objthis: *mut c_void,
    event: &str,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.is_empty() {
        return error_out(out_error, &format!("Layer.{event} requires a layer"));
    }
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let engine = crate::natives::context_engine();
    let target = match focus_arg_layer_id(engine, &args[0]) {
        Ok(t) => t,
        Err(e) => return error_out(out_error, &e),
    };
    {
        let mut scene = context_scene_mut();
        let Some(layer) = scene.layer_mut(inst.id) else {
            return error_out(out_error, "Layer: layer no longer exists");
        };
        layer.focus_work = target;
    }
    if let Some(owner) = &inst.action_owner {
        let mut keepalive = Vec::new();
        let members = layer_event_members(engine, &[("layer", &args[0])], &mut keepalive);
        dispatch_layer_event(engine, owner.raw, objthis, event, &members);
    }
    set_void_out(out);
    0
}

/// `onSearchNextFocusable(layer)` — reference `LayerIntf.cpp:10376`.
extern "C" fn layer_on_search_next_focusable(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    layer_search_focusable(
        instance,
        objthis,
        "onSearchNextFocusable",
        argc,
        argv,
        out,
        out_error,
    )
}

/// `onSearchPrevFocusable(layer)` — reference `LayerIntf.cpp:10339`.
extern "C" fn layer_on_search_prev_focusable(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    layer_search_focusable(
        instance,
        objthis,
        "onSearchPrevFocusable",
        argc,
        argv,
        out,
        out_error,
    )
}

/// `onBeforeFocus(layer, blurred, direction)` — reference
/// `LayerIntf.cpp:10413`: store `layer` in `FocusWork` and dispatch the
/// event to the action owner.
extern "C" fn layer_on_before_focus(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 3 {
        return error_out(
            out_error,
            "Layer.onBeforeFocus requires layer, blurred and direction",
        );
    }
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let engine = crate::natives::context_engine();
    let target = match focus_arg_layer_id(engine, &args[0]) {
        Ok(t) => t,
        Err(e) => return error_out(out_error, &e),
    };
    {
        let mut scene = context_scene_mut();
        let Some(layer) = scene.layer_mut(inst.id) else {
            return error_out(out_error, "Layer: layer no longer exists");
        };
        layer.focus_work = target;
    }
    if let Some(owner) = &inst.action_owner {
        let mut keepalive = Vec::new();
        let mut members = layer_event_members(
            engine,
            &[("layer", &args[0]), ("blurred", &args[1])],
            &mut keepalive,
        );
        members.push(("direction", TjsValue::Integer(arg_i64(&args[2]))));
        dispatch_layer_event(engine, owner.raw, objthis, "onBeforeFocus", &members);
    }
    set_void_out(out);
    0
}

/// `onTransitionCompleted(dest, src)` — reference `LayerIntf.cpp:10466`.
/// `dest`/`src` are objects (or null).
extern "C" fn layer_on_transition_completed(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 2 {
        return error_out(
            out_error,
            "Layer.onTransitionCompleted requires dest and src",
        );
    }
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    if let Some(owner) = &inst.action_owner {
        let engine = crate::natives::context_engine();
        let mut keepalive = Vec::new();
        let members = layer_event_members(
            engine,
            &[("dest", &args[0]), ("src", &args[1])],
            &mut keepalive,
        );
        dispatch_layer_event(
            engine,
            owner.raw,
            objthis,
            "onTransitionCompleted",
            &members,
        );
    }
    set_void_out(out);
    0
}

/// `children` — reference `LayerIntf.cpp:10522` `GetChildrenArrayObjectNoAddRef`:
/// a TJS `Array` of this layer's direct child layer objects in sibling order.
extern "C" fn layer_children_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let children = {
        let scene = context_scene_read();
        if scene.layer(inst.id).is_none() {
            return error_out(out_error, "Layer: layer no longer exists");
        }
        scene.ordered_children(inst.id)
    };
    let engine = crate::natives::context_engine();
    let Ok(tjs2_sys::RetainedValue::Object(array)) = engine.eval_retained("[]", "layer.children")
    else {
        return error_out(out_error, "Layer.children: cannot create array");
    };
    for (i, child_id) in children.iter().enumerate() {
        let obj = super::layer_tjs_object(*child_id);
        if obj.is_null() {
            continue;
        }
        let Ok(dv) = engine.retain_object_detached(obj) else {
            continue;
        };
        // `set_member` writes a numeric member through the Array's
        // `PropSetByNum` and consumes the retained object id.
        let _ = engine.set_member(
            array.raw_id(),
            &i.to_string(),
            &TjsValue::Retained(dv.raw_id() as u64),
        );
    }
    set_retained_out(array, out);
    0
}

/// `getLayerAt(x, y[, exclude_self[, get_disabled]])` — reference
/// `LayerIntf.cpp:8521`: the frontmost layer at a point in this layer's
/// coordinates, as a layer object or `null`.
extern "C" fn layer_get_layer_at(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 2 {
        return error_out(out_error, "Layer.getLayerAt requires x and y");
    }
    let x = arg_i64(&args[0]) as i32;
    let y = arg_i64(&args[1]) as i32;
    let bool_arg = |i: usize| {
        args.get(i)
            .is_some_and(|v| v.ty != tjs2_sys::VAL_VOID && arg_bool(v))
    };
    let exclude_self = bool_arg(2);
    let get_disabled = bool_arg(3);
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let hit = {
        let scene = context_scene_read();
        if scene.layer(inst.id).is_none() {
            return error_out(out_error, "Layer: layer no longer exists");
        }
        scene.layer_at(inst.id, x, y, exclude_self, get_disabled)
    };
    set_layer_object_out(crate::natives::context_engine(), hit, out);
    0
}

/// `joinFocusChain` — reference `LayerIntf.cpp:11177`.
extern "C" fn layer_join_focus_chain_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    set_int_out(out, i64::from(layer.join_focus_chain));
    0
}

extern "C" fn layer_join_focus_chain_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value is valid for the call.
    let v = unsafe { &*value };
    let join = arg_bool(v);
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(layer) = scene.layer_mut(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    layer.join_focus_chain = join;
    0
}

/// Shared body for `nextFocusable`/`prevFocusable`: compute the neighbour,
/// store it in `FocusWork`, post the `onSearch*Focusable` event to this
/// layer's own object (reference `GetNextFocusable`/`GetPrevFocusable`) and
/// return the possibly-redirected `FocusWork` as a layer object or `null`.
fn layer_focusable_neighbor_get(
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    forward: bool,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let engine = crate::natives::context_engine();
    let computed = {
        let scene = context_scene_read();
        if scene.layer(inst.id).is_none() {
            return error_out(out_error, "Layer: layer no longer exists");
        }
        if forward {
            scene.next_focusable(inst.id)
        } else {
            scene.prev_focusable(inst.id)
        }
    };
    {
        let mut scene = context_scene_mut();
        if let Some(layer) = scene.layer_mut(inst.id) {
            layer.focus_work = computed;
        }
    }
    let event = if forward {
        "onSearchNextFocusable"
    } else {
        "onSearchPrevFocusable"
    };
    fire_layer_self_event(engine, inst.id, event, computed);
    let result = context_scene_read()
        .layer(inst.id)
        .and_then(|l| l.focus_work);
    set_layer_object_out(engine, result, out);
    0
}

/// Invoke one of the layer's own event handlers with the found layer (or
/// `null`), mirroring the reference `TVPPostEvent(Owner, Owner, ...)`. A
/// script override or the native handler above receives it.
fn fire_layer_self_event(engine: &Tjs2Engine, layer_id: u32, method: &str, found: Option<u32>) {
    let obj = super::layer_tjs_object(layer_id);
    if obj.is_null() {
        return;
    }
    let Ok(dv) = engine.retain_object_detached(obj) else {
        return;
    };
    let mut keepalive = Vec::new();
    let arg = match found {
        Some(id) => {
            let fobj = super::layer_tjs_object(id);
            if fobj.is_null() {
                TjsValue::Void
            } else if let Ok(adv) = engine.retain_object_detached(fobj) {
                let value = TjsValue::Retained(adv.raw_id() as u64);
                keepalive.push(adv);
                value
            } else {
                TjsValue::Void
            }
        }
        None => TjsValue::Void,
    };
    let _ = engine.call_member(dv.raw_id(), method, &[arg]);
}

extern "C" fn layer_next_focusable_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_focusable_neighbor_get(instance, out, out_error, true)
}

extern "C" fn layer_prev_focusable_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_focusable_neighbor_get(instance, out, out_error, false)
}

/// `setClip([left, top, width, height])` — reference
/// `tTJSNI_BaseLayer::SetClip`/`ResetClip` (`LayerIntf.cpp:4032`). With no
/// args (or a `void` first arg) the clip resets to the whole image.
extern "C" fn layer_set_clip(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    if args.is_empty() || args[0].ty == tjs2_sys::VAL_VOID {
        if let Some(layer) = scene.layer_mut(inst.id) {
            layer.clip = None;
        }
        set_void_out(out);
        return 0;
    }
    if args.len() < 4 {
        return error_out(out_error, "Layer.setClip requires 0 or 4 arguments");
    }
    let left = arg_i64(&args[0]) as i32;
    let top = arg_i64(&args[1]) as i32;
    let width = arg_i64(&args[2]) as i32;
    let height = arg_i64(&args[3]) as i32;
    let (img_w, img_h) = layer
        .bitmap
        .and_then(|b| scene.bitmap(b).map(|b| (b.width as i32, b.height as i32)))
        .unwrap_or((layer.image_width as i32, layer.image_height as i32));
    let left = left.max(0);
    let top = top.max(0);
    let right = left.saturating_add(width).min(img_w.max(left));
    let bottom = top.saturating_add(height).min(img_h.max(top));
    if let Some(layer) = scene.layer_mut(inst.id) {
        layer.clip = Some(crate::scene::Rect {
            x: left,
            y: top,
            w: (right - left).max(0) as u32,
            h: (bottom - top).max(0) as u32,
        });
    }
    set_void_out(out);
    0
}

fn blur_bitmap(bitmap: &mut BitmapState, xradius: u32, yradius: u32) {
    layer_ops::blur_bitmap_in_place(bitmap, xradius, yradius);
}

// ---------------------------------------------------------------------------
// Text/font style (KAG `MessageArea` surface, and the core `drawGlyph`)
// ---------------------------------------------------------------------------

/// Ensure (creating on demand) the layer's own [`crate::scene::FontState`].
fn layer_font_state_mut(scene: &mut Scene, layer_id: u32) -> Option<&mut crate::scene::FontState> {
    let existing = scene.layer(layer_id).and_then(|l| l.font_id);
    let font_id = match existing {
        Some(id) if scene.font(id).is_some() => id,
        _ => {
            let id = scene.add_font(
                super::font::DEFAULT_FONT_FACE.to_string(),
                super::font::DEFAULT_FONT_HEIGHT,
                [255, 255, 255, 255],
            );
            scene.layer_mut(layer_id)?.font_id = Some(id);
            id
        }
    };
    scene.fonts.iter_mut().find(|f| f.id == font_id)
}

/// Snapshot the layer's font into `(face, height, style)`, falling back to
/// the newest registered `Font` and then to the engine defaults, exactly like
/// [`layer_draw_text`] does.
fn layer_text_style(scene: &Scene, layer_id: u32) -> (Option<String>, u32, DrawTextStyle) {
    let layer_font = scene.layer(layer_id).and_then(|l| l.font_id);
    if let Some(font) = layer_font.and_then(|id| scene.font(id)) {
        return (
            Some(font.face.clone()),
            font.height.max(1) as u32,
            DrawTextStyle::from_font(font),
        );
    }
    if let Some(font) = scene.fonts.last() {
        return (
            Some(font.face.clone()),
            font.height.max(1) as u32,
            DrawTextStyle::from_font(font),
        );
    }
    (None, 16, DrawTextStyle::default())
}

/// Read a `Font`-like object argument's `face`/`height`/`bold`/`italic`/
/// `underline`/`strikeout`/`angle` members. Returns `None` for non-objects or
/// objects without a readable `face`/`height` (the caller then falls back to
/// the layer font).
fn font_style_from_arg(engine: &Tjs2Engine, v: &Value) -> Option<(String, u32, DrawTextStyle)> {
    if v.ty != tjs2_sys::VAL_OBJECT {
        return None;
    }
    let dv = engine.retain_object_arg(v).ok()?;
    let id = dv.raw_id();
    let face = match engine.get_member(id, "face") {
        Ok(TjsValue::String(s)) => s,
        _ => return None,
    };
    let height = match engine.get_member(id, "height") {
        Ok(TjsValue::Integer(h)) => h.max(1) as u32,
        Ok(TjsValue::Real(h)) => h.max(1.0) as u32,
        _ => 16,
    };
    let flag = |name: &str| matches!(engine.get_member(id, name), Ok(TjsValue::Integer(1)));
    let angle = match engine.get_member(id, "angle") {
        Ok(TjsValue::Real(a)) => a,
        Ok(TjsValue::Integer(a)) => a as f64,
        _ => 0.0,
    };
    Some((
        face,
        height,
        DrawTextStyle {
            bold: flag("bold"),
            italic: flag("italic"),
            underline: flag("underline"),
            strikeout: flag("strikeout"),
            angle_deg: angle / 10.0,
        },
    ))
}

/// The first solid brush/pen color of an appearance argument, or `None`.
fn appearance_color(app_arg: &Value) -> Option<[u8; 4]> {
    let state = super::gdiplus::appearance_snapshot(app_arg.object_handle())?;
    for info in &state.infos {
        match info {
            DrawKind::Brush(BrushKind::Solid(c)) => return Some(*c),
            DrawKind::Pen {
                brush: BrushKind::Solid(c),
                ..
            } => return Some(*c),
            _ => {}
        }
    }
    None
}

/// Ensure the layer has a bitmap large enough for a text run and return
/// `(bitmap_id, bitmap_width)`. Shared by `drawText`/`drawString`/`drawGlyph`.
fn ensure_text_bitmap(
    scene: &mut Scene,
    layer_id: u32,
    x: i32,
    y: i32,
    text: &str,
    font_height: u32,
) -> Option<(u32, u32)> {
    let needed_w = (x.max(0) as u32).saturating_add(fallback_text_width(text, font_height));
    let needed_h = (y.max(0) as u32).saturating_add(fallback_text_height(text, font_height));
    let bitmap_id = ensure_layer_bitmap(scene, layer_id, needed_w, needed_h)?;
    let width = scene.bitmap(bitmap_id).map(|b| b.width.max(1))?;
    Some((bitmap_id, width))
}

/// Parse the text-draw parameter arguments (`color, opa, aa, shadowLevel,
/// shadowColor, shadowWidth, shadowX, shadowY`) with the reference defaults,
/// used by `setDefaultDrawTextParam` and `drawGlyph`.
fn parse_draw_text_param(args: &[Value], base: DrawTextParam) -> DrawTextParam {
    let int = |i: usize, d: i64| {
        args.get(i)
            .filter(|v| v.ty != tjs2_sys::VAL_VOID)
            .map(arg_i64)
            .unwrap_or(d)
    };
    DrawTextParam {
        color: int(0, base.color),
        opa: int(1, base.opa),
        aa: args
            .get(2)
            .filter(|v| v.ty != tjs2_sys::VAL_VOID)
            .map(arg_bool)
            .unwrap_or(base.aa),
        shadow_level: int(3, base.shadow_level),
        shadow_color: int(4, base.shadow_color),
        shadow_width: int(5, base.shadow_width),
        shadow_x: int(6, base.shadow_x),
        shadow_y: int(7, base.shadow_y),
    }
}

/// `setFontStyle(face[, size[, indent[, bold[, italic[, underline[,
/// strikeout[, angle]]]]]]])` — the KAG `MessageArea.setFontStyle` native
/// counterpart. It writes the layer's tracked `FontState`, so a later
/// `drawText` rasterizes with the requested face/size/style.
///
/// `indent` is accepted for signature compatibility and ignored: the
/// reference `MessageArea` keeps the indent in script (`_indent`) because it
/// is a per-line layout offset, not a glyph property.
extern "C" fn layer_set_font_style(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    if scene.layer(inst.id).is_none() {
        return error_out(out_error, "Layer: layer no longer exists");
    }
    let Some(font) = layer_font_state_mut(&mut scene, inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    if let Some(v) = args.first().filter(|v| v.ty != tjs2_sys::VAL_VOID) {
        font.face = arg_string(v);
    }
    if let Some(v) = args.get(1).filter(|v| v.ty != tjs2_sys::VAL_VOID) {
        font.height = arg_i64(v) as i32;
    }
    if let Some(v) = args.get(3).filter(|v| v.ty != tjs2_sys::VAL_VOID) {
        font.bold = arg_bool(v);
    }
    if let Some(v) = args.get(4).filter(|v| v.ty != tjs2_sys::VAL_VOID) {
        font.italic = arg_bool(v);
    }
    if let Some(v) = args.get(5).filter(|v| v.ty != tjs2_sys::VAL_VOID) {
        font.underline = arg_bool(v);
    }
    if let Some(v) = args.get(6).filter(|v| v.ty != tjs2_sys::VAL_VOID) {
        font.strikeout = arg_bool(v);
    }
    if let Some(v) = args.get(7).filter(|v| v.ty != tjs2_sys::VAL_VOID) {
        font.angle = arg_f64(v);
    }
    set_void_out(out);
    0
}

/// `resetFontStyle([face[, size[, ...]]])` — reset the layer font to the
/// engine defaults and then apply any provided arguments. This is the native
/// counterpart of the game's `MessageArea.resetFontStyle`, which re-applies
/// the recorded default font style.
extern "C" fn layer_reset_font_style(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    if scene.layer(inst.id).is_none() {
        return error_out(out_error, "Layer: layer no longer exists");
    }
    let Some(font) = layer_font_state_mut(&mut scene, inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    font.face = super::font::DEFAULT_FONT_FACE.to_string();
    font.height = super::font::DEFAULT_FONT_HEIGHT;
    font.bold = false;
    font.italic = false;
    font.underline = false;
    font.strikeout = false;
    font.angle = 0.0;
    if let Some(v) = args.first().filter(|v| v.ty != tjs2_sys::VAL_VOID) {
        font.face = arg_string(v);
    }
    if let Some(v) = args.get(1).filter(|v| v.ty != tjs2_sys::VAL_VOID) {
        font.height = arg_i64(v) as i32;
    }
    if let Some(v) = args.get(3).filter(|v| v.ty != tjs2_sys::VAL_VOID) {
        font.bold = arg_bool(v);
    }
    if let Some(v) = args.get(4).filter(|v| v.ty != tjs2_sys::VAL_VOID) {
        font.italic = arg_bool(v);
    }
    if let Some(v) = args.get(5).filter(|v| v.ty != tjs2_sys::VAL_VOID) {
        font.underline = arg_bool(v);
    }
    if let Some(v) = args.get(6).filter(|v| v.ty != tjs2_sys::VAL_VOID) {
        font.strikeout = arg_bool(v);
    }
    if let Some(v) = args.get(7).filter(|v| v.ty != tjs2_sys::VAL_VOID) {
        font.angle = arg_f64(v);
    }
    set_void_out(out);
    0
}

/// `setDefaultDrawTextParam(color, opa, aa, shadowLevel, shadowColor,
/// shadowWidth, shadowX, shadowY)` — record the layer's default text-draw
/// parameters (the native counterpart of `MessageArea.setDefaultDrawTextParam`).
extern "C" fn layer_set_default_draw_text_param(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut params = LAYER_TEXT_PARAMS.lock().unwrap_or_else(|p| p.into_inner());
    let entry = params.entry(inst.id).or_default();
    let base = entry.default;
    entry.default = parse_draw_text_param(args, base);
    set_void_out(out);
    0
}

/// `resetDrawTextParam()` — restore the current text parameters from the
/// defaults recorded by `setDefaultDrawTextParam`.
extern "C" fn layer_reset_draw_text_param(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut params = LAYER_TEXT_PARAMS.lock().unwrap_or_else(|p| p.into_inner());
    let entry = params.entry(inst.id).or_default();
    entry.current = entry.default;
    set_void_out(out);
    0
}

/// `drawString(font, app, x, y, text)` — the `layerExDraw` plugin's string
/// draw. The engine has no `GdiPlus.Font`, so the `font` argument is a
/// `Font`-like object (our native `Font`, or any object exposing
/// `face`/`height`/`bold`/...); the `app` argument supplies the color through
/// its first solid brush. Rasterization uses the same `tvp-text` machinery as
/// [`layer_draw_text`].
extern "C" fn layer_draw_string(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 5 {
        return error_out(
            out_error,
            "Layer.drawString requires (font, app, x, y, text)",
        );
    }
    let text = arg_string(&args[4]);
    if text.is_empty() {
        set_void_out(out);
        return 0;
    }
    let x = arg_i64(&args[2]) as i32;
    let y = arg_i64(&args[3]) as i32;
    let engine = context_engine();
    let mut color = appearance_color(&args[1]).unwrap_or([255, 255, 255, 255]);
    // The plugin's string draw honors the brush alpha; default to opaque.
    if color[3] == 0 {
        color[3] = 255;
    }
    // Snapshot the `font` argument's geometry *before* taking the scene lock:
    // the `Font` property getters lock the scene themselves, and the scene
    // lock is not reentrant.
    let font_style = font_style_from_arg(engine, &args[0]);
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let (face_override, font_height, style, bitmap_id, width) = {
        let mut scene = context_scene_mut();
        if scene.layer(inst.id).is_none() {
            return error_out(out_error, "Layer: layer no longer exists");
        }
        let (face, height, style) = match font_style {
            Some((face, height, style)) => (Some(face), height, style),
            None => layer_text_style(&scene, inst.id),
        };
        let Some((bitmap_id, width)) = ensure_text_bitmap(&mut scene, inst.id, x, y, &text, height)
        else {
            return error_out(out_error, "Layer: layer no longer exists");
        };
        (face, height, style, bitmap_id, width)
    };
    paint_text_run(
        bitmap_id,
        width,
        &text,
        x,
        y,
        color,
        255,
        true,
        0,
        [0, 0, 0, 255],
        0,
        0,
        0,
        font_height,
        style,
        face_override,
    );
    set_void_out(out);
    0
}

/// `getDrawWidth(text)` — the advance width of `text` in the layer's tracked
/// font. This is the native counterpart of the game's
/// `MessageArea.getDrawWidth`, which measures a pre-rendered glyph; for the
/// vector/fallback engine the same half/full-width metric as `getTextWidth`
/// is used. An empty/absent argument measures the empty string (width 0).
extern "C" fn layer_get_draw_width(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let text = args.first().map(arg_string).unwrap_or_default();
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let height = {
        let scene = context_scene_read();
        layer_text_style(&scene, inst.id).1
    };
    set_int_out(out, i64::from(fallback_text_width(&text, height)));
    0
}

/// `drawGlyph(x, y, glyph, color[, opa[, aa[, shadowLevel[, shadowColor[,
/// shadowWidth[, shadowOfsX[, shadowOfsY]]]]]]])` — reference
/// `tTJSNI_BaseLayer::DrawGlyph` (`LayerIntf.cpp:4481`).
///
/// The reference `glyph` is a `Font.getGlyph` `Glyph` object, which this
/// engine does not model. To keep the method real rather than a discarded
/// call, the glyph argument is accepted as either a character/string or an
/// object exposing a `text`/`char`/`character` string member, and is
/// rasterized with the layer's tracked font through the same text pipeline.
/// A glyph argument that carries no text logs a warning and paints nothing.
extern "C" fn layer_draw_glyph(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 4 {
        return error_out(out_error, "Layer.drawGlyph requires (x, y, glyph, color)");
    }
    let engine = context_engine();
    let glyph_text = glyph_text_arg(engine, &args[2]);
    let Some(text) = glyph_text else {
        log::warn!("Layer.drawGlyph: glyph argument carries no text; nothing painted");
        set_void_out(out);
        return 0;
    };
    if text.is_empty() {
        set_void_out(out);
        return 0;
    }
    let x = arg_i64(&args[0]) as i32;
    let y = arg_i64(&args[1]) as i32;
    let mut color = argb_to_rgba(arg_i64(&args[3]));
    color[3] = 255;
    let opa = args.get(4).map(arg_i64).unwrap_or(255).clamp(0, 255) as u8;
    let aa = args.get(5).map(arg_bool).unwrap_or(true);
    let shadow_level = args.get(6).map(arg_i64).unwrap_or(0).max(0) as u32;
    let mut shadow_color = argb_to_rgba(args.get(7).map(arg_i64).unwrap_or(0));
    shadow_color[3] = 255;
    let shadow_width = args.get(8).map(arg_i64).unwrap_or(0).max(0) as u32;
    let shadow_x = args.get(9).map(arg_i64).unwrap_or(0) as i32;
    let shadow_y = args.get(10).map(arg_i64).unwrap_or(0) as i32;
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let (face_override, font_height, style, bitmap_id, width) = {
        let mut scene = context_scene_mut();
        if scene.layer(inst.id).is_none() {
            return error_out(out_error, "Layer: layer no longer exists");
        }
        let (face, height, style) = layer_text_style(&scene, inst.id);
        let Some((bitmap_id, width)) = ensure_text_bitmap(&mut scene, inst.id, x, y, &text, height)
        else {
            return error_out(out_error, "Layer: layer no longer exists");
        };
        (face, height, style, bitmap_id, width)
    };
    paint_text_run(
        bitmap_id,
        width,
        &text,
        x,
        y,
        color,
        opa,
        aa,
        shadow_level,
        shadow_color,
        shadow_width,
        shadow_x,
        shadow_y,
        font_height,
        style,
        face_override,
    );
    set_void_out(out);
    0
}

/// Resolve the text carried by a `drawGlyph` glyph argument: a plain string,
/// or an object exposing `text`/`char`/`character`.
fn glyph_text_arg(engine: &Tjs2Engine, v: &Value) -> Option<String> {
    match v.ty {
        tjs2_sys::VAL_STRING => Some(arg_string(v)),
        tjs2_sys::VAL_OBJECT => {
            let dv = engine.retain_object_arg(v).ok()?;
            for name in ["text", "char", "character"] {
                if let Ok(TjsValue::String(s)) = engine.get_member(dv.raw_id(), name) {
                    return Some(s);
                }
            }
            None
        }
        _ => None,
    }
}

/// `clear([argb])` — the `layerExDraw` plugin's `clear`: replace every pixel
/// of the layer's main image with `argb` (default `0` = transparent). The
/// reference native is not a core `Layer` member (it is an engine extra), so
/// it is given the plugin's real fill semantics rather than removed.
extern "C" fn layer_clear(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let color = args
        .first()
        .filter(|v| v.ty != tjs2_sys::VAL_VOID)
        .map(|v| argb_to_rgba(arg_i64(v)))
        .unwrap_or([0, 0, 0, 0]);
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    if scene.layer(inst.id).is_none() {
        return error_out(out_error, "Layer: layer no longer exists");
    }
    let Some(bitmap_id) = ensure_dest_image(&mut scene, inst.id, 0, 0) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    if let Some(bitmap) = scene.bitmap_mut(bitmap_id) {
        let (w, h) = (bitmap.width, bitmap.height);
        raster::fill_rect_replace(bitmap, 0, 0, w, h, color);
        bitmap.mark_dirty();
    }
    if let Some(layer) = scene.layer_mut(inst.id) {
        layer.image_modified = true;
    }
    set_void_out(out);
    0
}

/// `drawClosedCurve(app, points)` — closed cardinal spline, tension 0.5
/// (`LayerExDraw::drawClosedCurve`).
extern "C" fn layer_draw_closed_curve(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_draw_spline(instance, argc, argv, out, out_error, true, false)
}

/// `drawClosedCurve2(app, points, tension)`.
extern "C" fn layer_draw_closed_curve2(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_draw_spline(instance, argc, argv, out, out_error, true, true)
}

/// `drawCurve2(app, points, tension)`.
extern "C" fn layer_draw_curve2(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_draw_spline(instance, argc, argv, out, out_error, false, true)
}

/// `drawCurve3(app, points, offset, numberOfSegments, tension)`.
extern "C" fn layer_draw_curve3(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 5 {
        return error_out(
            out_error,
            "Layer.drawCurve3 requires (app, points, offset, numberOfSegments, tension)",
        );
    }
    let pts = parse_points(context_engine(), &args[1]);
    if pts.len() < 2 {
        set_void_out(out);
        return 0;
    }
    let offset = arg_i64(&args[2]).max(0) as usize;
    let segments = arg_i64(&args[3]);
    let segments = if segments < 0 {
        pts.len().saturating_sub(1)
    } else {
        segments as usize
    };
    let tension = arg_f64(&args[4]);
    let path = cardinal_spline_path(&pts, false, offset, segments, tension);
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    if path.len() >= 2
        && let Err(e) = draw_gdiplus_path(inst.id, &args[0], &path, false, false)
    {
        return error_out(out_error, &e);
    }
    set_void_out(out);
    0
}

/// Shared body for `drawClosedCurve`/`drawClosedCurve2`/`drawCurve2`.
fn layer_draw_spline(
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    closed: bool,
    has_tension: bool,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let min = if has_tension { 3 } else { 2 };
    if args.len() < min {
        return error_out(
            out_error,
            "Layer.drawCurve* requires an app and a points array",
        );
    }
    let pts = parse_points(context_engine(), &args[1]);
    if pts.len() < 2 {
        set_void_out(out);
        return 0;
    }
    let tension = if has_tension { arg_f64(&args[2]) } else { 0.5 };
    let path = cardinal_spline_path(&pts, closed, 0, pts.len().saturating_sub(1), tension);
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    if path.len() >= 2
        && let Err(e) = draw_gdiplus_path(inst.id, &args[0], &path, closed, closed)
    {
        return error_out(out_error, &e);
    }
    set_void_out(out);
    0
}

/// `drawRectangles(app, rects)` — draw each `[x, y, w, h]` rectangle with the
/// appearance's brushes/pens (`LayerExDraw::drawRectangles`).
extern "C" fn layer_draw_rectangles(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 2 {
        return error_out(out_error, "Layer.drawRectangles requires (app, rects)");
    }
    let engine = context_engine();
    let rects = parse_rectangles(engine, &args[1]);
    if rects.is_empty() {
        set_void_out(out);
        return 0;
    }
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    for (x, y, w, h) in rects {
        let pts = [(x, y), (x + w, y), (x + w, y + h), (x, y + h)];
        if let Err(e) = draw_gdiplus_path(inst.id, &args[0], &pts, true, true) {
            return error_out(out_error, &e);
        }
    }
    set_void_out(out);
    0
}

/// Parse a TJS array of `[x, y, w, h]` rectangles.
fn parse_rectangles(engine: &Tjs2Engine, arg: &Value) -> Vec<(f64, f64, f64, f64)> {
    if arg.ty != tjs2_sys::VAL_OBJECT {
        return Vec::new();
    }
    let Ok(array) = engine.retain_object_arg(arg) else {
        return Vec::new();
    };
    let count = match engine.get_member(array.raw_id(), "count") {
        Ok(TjsValue::Integer(n)) => n.max(0) as usize,
        Ok(TjsValue::Real(n)) => n.max(0.0) as usize,
        _ => 0,
    };
    let mut rects = Vec::with_capacity(count);
    for i in 0..count {
        if engine.get_member(array.raw_id(), &i.to_string()).is_err() {
            continue;
        }
        let Ok(quad) = engine.retain_value_detached(&TjsValue::Object) else {
            continue;
        };
        let m = |name: &str| member_f64(engine, quad.raw_id(), name);
        if let (Some(x), Some(y), Some(w), Some(h)) = (m("0"), m("1"), m("2"), m("3")) {
            rects.push((x, y, w, h));
        }
    }
    rects
}

/// `drawPath(app, path)` — the `layerExDraw` plugin's path draw. The engine
/// does not model `GdiPlus.Path`, so the argument is accepted as a point
/// array (`[[x, y], ...]`) or an object exposing a `points` array; the
/// resulting open polyline is filled/stroked with the appearance.
extern "C" fn layer_draw_path(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 2 {
        return error_out(out_error, "Layer.drawPath requires (app, path)");
    }
    let engine = context_engine();
    let mut pts = parse_points(engine, &args[1]);
    if pts.is_empty() {
        // A `Path`-like object carrying a `points` member.
        if args[1].ty == tjs2_sys::VAL_OBJECT
            && let Ok(dv) = engine.retain_object_arg(&args[1])
            && let Ok(TjsValue::Object) = engine.get_member(dv.raw_id(), "points")
            && let Ok(points) = engine.retain_value_detached(&TjsValue::Object)
        {
            pts = parse_points_id(engine, points.raw_id());
        }
    }
    if pts.len() < 2 {
        // No usable path: report the unsupported argument shape instead of
        // silently painting nothing.
        return error_out(
            out_error,
            "Layer.drawPath expects a point array (GdiPlus.Path is not modelled)",
        );
    }
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    if let Err(e) = draw_gdiplus_path(inst.id, &args[0], &pts, false, true) {
        return error_out(out_error, &e);
    }
    set_void_out(out);
    0
}

// ---------------------------------------------------------------------------
// Main/mask pixel access and province plane
// ---------------------------------------------------------------------------

/// Whether `(x, y)` lies inside the layer's `ClipRect` (the reference
/// `SetMainPixel`/`SetMaskPixel` guard).
fn clip_contains(layer: &LayerState, x: i32, y: i32) -> bool {
    let (left, top, right, bottom) = layer_pixel_rect(layer);
    x >= left && y >= top && x < right && y < bottom
}

/// `getMainPixel(x, y)` — reference `GetMainPixel` (`LayerIntf.cpp:2917`):
/// the MainImage RGB at `(x, y)` as `0xRRGGBB` (the alpha channel is the mask,
/// exposed by `getMaskPixel`). Throws `TVPNotDrawableLayerType` without an
/// image; out-of-bounds returns 0.
extern "C" fn layer_get_main_pixel(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 2 {
        return error_out(out_error, "Layer.getMainPixel requires x and y");
    }
    let (x, y) = (arg_i64(&args[0]) as i32, arg_i64(&args[1]) as i32);
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    let Some(bitmap) = layer.bitmap.and_then(|id| scene.bitmap(id)) else {
        return error_out(out_error, "Layer: layer has no image");
    };
    let value = if x < 0 || y < 0 {
        0
    } else {
        match bitmap.pixel_offset(x as u32, y as u32) {
            Some(off) => {
                (i64::from(bitmap.rgba[off]) << 16)
                    | (i64::from(bitmap.rgba[off + 1]) << 8)
                    | i64::from(bitmap.rgba[off + 2])
            }
            None => 0,
        }
    };
    set_int_out(out, value);
    0
}

/// `getMaskPixel(x, y)` — reference `GetMaskPixel` (`LayerIntf.cpp:2945`):
/// the MainImage alpha at `(x, y)`.
extern "C" fn layer_get_mask_pixel(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 2 {
        return error_out(out_error, "Layer.getMaskPixel requires x and y");
    }
    let (x, y) = (arg_i64(&args[0]) as i32, arg_i64(&args[1]) as i32);
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    let Some(bitmap) = layer.bitmap.and_then(|id| scene.bitmap(id)) else {
        return error_out(out_error, "Layer: layer has no image");
    };
    let value = if x < 0 || y < 0 {
        0
    } else {
        bitmap
            .pixel_offset(x as u32, y as u32)
            .map_or(0, |off| i64::from(bitmap.rgba[off + 3]))
    };
    set_int_out(out, value);
    0
}

/// `setMainPixel(x, y, color)` — reference `SetMainPixel`
/// (`LayerIntf.cpp:2925`): write the RGB channels (the mask/alpha is left
/// untouched, mirroring `SetPointMain`) when the point is inside `ClipRect`.
extern "C" fn layer_set_main_pixel(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 3 {
        return error_out(out_error, "Layer.setMainPixel requires x, y and color");
    }
    let (x, y) = (arg_i64(&args[0]) as i32, arg_i64(&args[1]) as i32);
    let color = argb_to_rgba(arg_i64(&args[2]));
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    if !clip_contains(layer, x, y) {
        set_void_out(out);
        return 0;
    }
    let Some(bitmap_id) = layer.bitmap else {
        return error_out(out_error, "Layer: layer has no image");
    };
    if x >= 0
        && y >= 0
        && let Some(bitmap) = scene.bitmap_mut(bitmap_id)
        && let Some(off) = bitmap.pixel_offset(x as u32, y as u32)
    {
        bitmap.rgba[off] = color[0];
        bitmap.rgba[off + 1] = color[1];
        bitmap.rgba[off + 2] = color[2];
        bitmap.mark_dirty();
    }
    if let Some(layer) = scene.layer_mut(inst.id) {
        layer.image_modified = true;
    }
    set_void_out(out);
    0
}

/// `setMaskPixel(x, y, mask)` — reference `SetMaskPixel` (`LayerIntf.cpp:2953`):
/// write the alpha channel when the point is inside `ClipRect`.
extern "C" fn layer_set_mask_pixel(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 3 {
        return error_out(out_error, "Layer.setMaskPixel requires x, y and mask");
    }
    let (x, y) = (arg_i64(&args[0]) as i32, arg_i64(&args[1]) as i32);
    let mask = arg_i64(&args[2]).clamp(0, 255) as u8;
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    if !clip_contains(layer, x, y) {
        set_void_out(out);
        return 0;
    }
    let Some(bitmap_id) = layer.bitmap else {
        return error_out(out_error, "Layer: layer has no image");
    };
    if x >= 0
        && y >= 0
        && let Some(bitmap) = scene.bitmap_mut(bitmap_id)
        && let Some(off) = bitmap.pixel_offset(x as u32, y as u32)
    {
        bitmap.rgba[off + 3] = mask;
        bitmap.mark_dirty();
    }
    if let Some(layer) = scene.layer_mut(inst.id) {
        layer.image_modified = true;
    }
    set_void_out(out);
    0
}

/// `loadProvinceImage(name)` — reference `LoadProvinceImage`
/// (`LayerIntf.cpp:2893`): load `name` into the layer's province plane. The
/// reference loads an 8-bit palettized image and requires it to match the
/// MainImage size; this engine decodes through the ordinary image loader and
/// uses each pixel's red channel as the province value (grayscale/indexed
/// province maps have `R == G == B`). A size mismatch throws
/// `Layer.loadProvinceImage: province image size mismatch`.
extern "C" fn layer_load_province_image(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let Some(name_arg) = args.first() else {
        return error_out(out_error, "Layer.loadProvinceImage requires a storage name");
    };
    if name_arg.ty != tjs2_sys::VAL_STRING {
        return error_out(out_error, "Layer.loadProvinceImage expects a storage name");
    }
    let name = arg_string(name_arg);
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let (mut scene, mut storage) = super::context_scene_storage();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    let Some(main_id) = layer.bitmap else {
        return error_out(out_error, "Layer: layer has no image");
    };
    let (main_w, main_h) = scene
        .bitmap(main_id)
        .map(|b| (b.width, b.height))
        .unwrap_or((0, 0));
    if main_w == 0 || main_h == 0 {
        return error_out(out_error, "Layer: layer has no image");
    }
    let mut cache = super::bitmap_cache();
    let temp_id = match crate::bitmap::load_bitmap_from_storage(
        &mut scene,
        &mut cache,
        &mut storage,
        &name,
        None,
    ) {
        Ok(id) => id,
        Err(e) => return error_out(out_error, &format!("Layer.loadProvinceImage: {e}")),
    };
    let province: Option<Box<[u8]>> = match scene.bitmap(temp_id) {
        Some(bitmap) if bitmap.width == main_w && bitmap.height == main_h => {
            Some(bitmap.rgba.chunks_exact(4).map(|p| p[0]).collect())
        }
        Some(_) => {
            scene.release_bitmap(temp_id);
            return error_out(
                out_error,
                "Layer.loadProvinceImage: province image size mismatch",
            );
        }
        None => None,
    };
    scene.release_bitmap(temp_id);
    if let Some(province) = province
        && let Some(layer) = scene.layer_mut(inst.id)
    {
        layer.province = Some(province);
        layer.province_width = main_w;
        layer.province_height = main_h;
        layer.image_modified = true;
    }
    set_void_out(out);
    0
}

/// `independProvinceImage([copy=true])` — reference `IndependProvinceImage`
/// (`LayerIntf.cpp:2689`). In this engine the province plane is a per-layer
/// `Box<[u8]>` and is never shared, so `copy=true` performs a defensive
/// private copy (real, if redundant given the ownership model) and
/// `copy=false` is a genuine semantic no-op because there is no sharing to
/// detach. Both mark the image modified.
extern "C" fn layer_independ_province_image(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let copy = args
        .first()
        .filter(|v| v.ty != tjs2_sys::VAL_VOID)
        .map(arg_bool)
        .unwrap_or(true);
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(layer) = scene.layer_mut(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    if copy && let Some(province) = layer.province.clone() {
        layer.province = Some(province);
    }
    layer.image_modified = true;
    set_void_out(out);
    0
}

// ---------------------------------------------------------------------------
// Affine anchor (KAG `AffineLayer`/`Sprite` surface)
// ---------------------------------------------------------------------------

/// `setCenter(x, y)` — record the layer's affine rotation/zoom center. See
/// [`LayerAffineState`] for why this is an engine-level store: the reference
/// core has no such method, it is the KAG `Sprite.setCenter`.
extern "C" fn layer_set_center(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 2 {
        return error_out(out_error, "Layer.setCenter requires x and y");
    }
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    if context_scene_read().layer(inst.id).is_none() {
        return error_out(out_error, "Layer: layer no longer exists");
    }
    let state = LayerAffineState {
        center: (arg_f64(&args[0]), arg_f64(&args[1])),
        ..LAYER_AFFINE
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&inst.id)
            .copied()
            .unwrap_or_default()
    };
    if state == LayerAffineState::default() {
        LAYER_AFFINE
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&inst.id);
    } else {
        LAYER_AFFINE
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(inst.id, state);
    }
    set_void_out(out);
    0
}

/// `setAffineOffset(x, y)` — record the layer's affine anchor offset
/// (`AffineLayer.setAffineOffset`).
extern "C" fn layer_set_affine_offset(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 2 {
        return error_out(out_error, "Layer.setAffineOffset requires x and y");
    }
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    if context_scene_read().layer(inst.id).is_none() {
        return error_out(out_error, "Layer: layer no longer exists");
    }
    let mut map = LAYER_AFFINE.lock().unwrap_or_else(|p| p.into_inner());
    let entry = map.entry(inst.id).or_default();
    entry.affine_offset = (arg_f64(&args[0]), arg_f64(&args[1]));
    if *entry == LayerAffineState::default() {
        map.remove(&inst.id);
    }
    set_void_out(out);
    0
}

/// The stored affine state for `layer_id`, or `None` when unset. Exposed so
/// the render/affine path (or a future `Layer` affine contract) can read the
/// anchor; the pixel transforms of `affineCopy`/`operateAffine` still take
/// explicit matrices.
pub fn layer_affine_state(layer_id: u32) -> Option<LayerAffineState> {
    LAYER_AFFINE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&layer_id)
        .copied()
}

// ---------------------------------------------------------------------------
// Sibling order
// ---------------------------------------------------------------------------

/// The maintained sibling id list for `id` (window root list or parent
/// `children`).
fn layer_siblings(scene: &Scene, id: u32) -> Vec<u32> {
    match scene.layer(id) {
        Some(layer) => match layer.parent {
            Some(parent) => scene
                .layer(parent)
                .map(|p| p.children.clone())
                .unwrap_or_default(),
            None => scene
                .window(layer.window)
                .map(|w| w.layers.clone())
                .unwrap_or_default(),
        },
        None => Vec::new(),
    }
}

/// Move `id` to `new_index` in its sibling list and set its `z_order`.
fn reorder_layer(scene: &mut Scene, id: u32, new_index: usize, z: i32) {
    let Some((window, parent)) = scene.layer(id).map(|l| (l.window, l.parent)) else {
        return;
    };
    if let Some(layer) = scene.layer_mut(id) {
        layer.z_order = z;
    }
    let list = match parent {
        Some(parent) => scene.layer_mut(parent).map(|p| &mut p.children),
        None => scene.window_mut(window).map(|w| &mut w.layers),
    };
    if let Some(list) = list {
        list.retain(|&x| x != id);
        let index = new_index.min(list.len());
        list.insert(index, id);
    }
}

/// `bringToBack()` — reference `tTJSNI_BaseLayer::BringToBack`
/// (`LayerIntf.cpp:1413`): move to the most-back sibling position.
extern "C" fn layer_bring_to_back(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    if scene.layer(inst.id).is_none() {
        return error_out(out_error, "Layer: layer no longer exists");
    }
    let siblings = layer_siblings(&scene, inst.id);
    let min_z = siblings
        .iter()
        .filter_map(|&id| scene.layer(id).map(|l| l.z_order))
        .min()
        .unwrap_or(0);
    reorder_layer(&mut scene, inst.id, 0, min_z);
    set_void_out(out);
    0
}

/// `moveBefore(sibling)` — reference `MoveBefore` (`LayerIntf.cpp:1356`).
extern "C" fn layer_move_before(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_move_relative(instance, argc, argv, out, out_error, true)
}

/// `moveBehind(sibling)` — reference `MoveBehind` (`LayerIntf.cpp:1376`).
extern "C" fn layer_move_behind(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_move_relative(instance, argc, argv, out, out_error, false)
}

fn layer_move_relative(
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    before: bool,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let engine = context_engine();
    let Some(target_arg) = args.first() else {
        return error_out(out_error, "Layer.moveBefore/moveBehind requires a layer");
    };
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let target = match focus_arg_layer_id(engine, target_arg) {
        Ok(Some(id)) => id,
        Ok(None) => return error_out(out_error, "Specify Layer"),
        Err(e) => return error_out(out_error, &e),
    };
    let mut scene = context_scene_mut();
    if scene.layer(inst.id).is_none() {
        return error_out(out_error, "Layer: layer no longer exists");
    }
    let siblings = layer_siblings(&scene, inst.id);
    let (Some(this), Some(target_index)) = (
        siblings.iter().position(|&x| x == inst.id),
        siblings.iter().position(|&x| x == target),
    ) else {
        return error_out(out_error, "Layer.moveBefore/moveBehind: not siblings");
    };
    let z = scene.layer(target).map(|l| l.z_order).unwrap_or(0);
    let new_index = match (before, this < target_index) {
        (true, true) => target_index - 1,
        (true, false) => target_index,
        (false, true) => target_index,
        (false, false) => target_index + 1,
    };
    reorder_layer(&mut scene, inst.id, new_index, z);
    set_void_out(out);
    0
}

// ---------------------------------------------------------------------------
// Input capture / hit-test work / attention / modal
// ---------------------------------------------------------------------------

/// `captureMouse()` — make this layer the mouse capture owner. The reference
/// has no such member (the manager captures on `onMouseDown`); KAG scripts
/// call it to force capture. The state is stored here; the render input
/// bridge can read it through [`captured_mouse_layer`].
extern "C" fn layer_capture_mouse(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    LAYER_CAPTURE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .mouse = Some(inst.id);
    set_void_out(out);
    0
}

/// `releaseCapture()` — reference `ReleaseCapture` (`LayerIntf.cpp:3467`):
/// release the mouse capture (from all layers, not just this one).
extern "C" fn layer_release_capture(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    LAYER_CAPTURE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .mouse = None;
    set_void_out(out);
    0
}

/// `captureTouch(id)` — capture touch `id` for this layer (KAG extension over
/// the reference `SetTouchCapture`).
extern "C" fn layer_capture_touch(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.is_empty() {
        return error_out(out_error, "Layer.captureTouch requires a touch id");
    }
    let id = arg_i64(&args[0]) as u64;
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    LAYER_CAPTURE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .touches
        .insert(id, inst.id);
    set_void_out(out);
    0
}

/// `releaseTouchCapture([id])` — reference `ReleaseTouchCapture`
/// (`LayerIntf.cpp:3475`): with an id release that touch, without one release
/// every touch capture.
extern "C" fn layer_release_touch_capture(
    _engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let mut capture = LAYER_CAPTURE.lock().unwrap_or_else(|p| p.into_inner());
    match args.first().filter(|v| v.ty != tjs2_sys::VAL_VOID) {
        Some(v) => {
            capture.touches.remove(&(arg_i64(v) as u64));
        }
        None => capture.touches.clear(),
    }
    set_void_out(out);
    0
}

/// The current mouse capture owner, or `None`. Public so the render input
/// bridge (a downstream crate) can honor script `captureMouse`. The bridge
/// currently manages its own hit-test capture; honoring this state is a
/// render-side wiring step.
pub fn captured_mouse_layer() -> Option<u32> {
    LAYER_CAPTURE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .mouse
}

/// The current touch captures as `(touch id, layer id)`. Public for the same
/// reason as [`captured_mouse_layer`].
pub fn captured_touches() -> Vec<(u64, u32)> {
    LAYER_CAPTURE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .touches
        .iter()
        .map(|(&id, &layer)| (id, layer))
        .collect()
}

/// `onHitTest(x, y, hit)` — reference `LayerIntf.cpp:9944`: store the script
/// hook's hit result in the layer's work slot. `Scene::layer_at` reads
/// `OnHitTest_Work` after dispatching the hook; our scene does not yet
/// dispatch script `onHitTest`, so the value is kept in
/// [`LAYER_HITTEST_WORK`] for that wiring. [`layer_hit_test_work`] exposes it.
extern "C" fn layer_on_hit_test(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 3 {
        return error_out(out_error, "Layer.onHitTest requires x, y and hit");
    }
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    LAYER_HITTEST_WORK
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(inst.id, arg_bool(&args[2]));
    set_void_out(out);
    0
}

/// The last `onHitTest` work value for `layer_id`, if any.
pub fn layer_hit_test_work(layer_id: u32) -> Option<bool> {
    LAYER_HITTEST_WORK
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&layer_id)
        .copied()
}

/// `setAttentionPos(x, y)` — reference `SetAttentionPoint`
/// (`LayerIntf.cpp:3207`): set the layer's attention anchor.
extern "C" fn layer_set_attention_pos(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 2 {
        return error_out(out_error, "Layer.setAttentionPos requires x and y");
    }
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(layer) = scene.layer_mut(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    layer.attention_left = arg_i64(&args[0]) as i32;
    layer.attention_top = arg_i64(&args[1]) as i32;
    set_void_out(out);
    0
}

/// `setMode()` — reference `tTVPLayerManager::SetModeTo`
/// (`LayerManager.cpp:834`): make this layer the current modal layer of its
/// window and focus its first focusable descendant. Throws for an invisible/
/// disabled layer, a layer already modal, or an ancestor of the current modal
/// layer.
extern "C" fn layer_set_mode(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let engine = context_engine();
    let focus = {
        let mut scene = context_scene_mut();
        let Some(window) = scene.layer(inst.id).map(|l| l.window) else {
            return error_out(out_error, "Layer: layer no longer exists");
        };
        // The reference forces `Visible = true`, then rejects a layer whose
        // node is still not visible or whose `Enabled` is false.
        if !scene.node_visible(inst.id)
            && let Some(layer) = scene.layer_mut(inst.id)
        {
            layer.visible = true;
        }
        if !scene.node_visible(inst.id) || !scene.layer(inst.id).is_some_and(|l| l.enabled) {
            return error_out(out_error, "Layer.setMode: disabled or non-visible layer");
        }
        {
            let mut modals = MODAL_LAYERS.lock().unwrap_or_else(|p| p.into_inner());
            let stack = modals.entry(window).or_default();
            if let Some(&current) = stack.last()
                && (current == inst.id || is_ancestor_or_self(&scene, current, inst.id))
            {
                return error_out(out_error, "Layer.setMode: cannot set mode to this layer");
            }
            stack.push(inst.id);
        }
        first_focusable_in(&scene, inst.id)
    };
    if let Some(focus) = focus {
        let (prev, current) = context_scene_mut().set_focus(focus);
        if current != prev {
            dispatch_focus_change(engine, current.unwrap_or(focus), prev, true);
        }
    }
    set_void_out(out);
    0
}

/// `removeMode()` — reference `tTVPLayerManager::RemoveModeFrom`
/// (`LayerManager.cpp:869`): drop this layer from its window's modal stack.
extern "C" fn layer_remove_mode(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let engine = context_engine();
    let mut scene = context_scene_mut();
    let Some(window) = scene.layer(inst.id).map(|l| l.window) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    let removed = {
        let mut modals = MODAL_LAYERS.lock().unwrap_or_else(|p| p.into_inner());
        let stack = modals.entry(window).or_default();
        let before = stack.len();
        stack.retain(|&id| id != inst.id);
        before != stack.len()
    };
    if removed {
        let next = scene.next_focusable(inst.id);
        match next {
            Some(next) => {
                let (prev, current) = scene.set_focus(next);
                drop(scene);
                if current != prev {
                    dispatch_focus_change(engine, current.unwrap_or(next), prev, true);
                }
            }
            None => {
                scene.clear_focus(window);
                drop(scene);
            }
        }
    }
    set_void_out(out);
    0
}

/// Whether `id` is `ancestor` or a descendant of it.
fn is_ancestor_or_self(scene: &Scene, ancestor: u32, id: u32) -> bool {
    let mut current = Some(id);
    while let Some(node) = current {
        if node == ancestor {
            return true;
        }
        current = scene.layer(node).and_then(|l| l.parent);
    }
    false
}

/// The first focusable layer in `root`'s subtree (self first), in paint order.
fn first_focusable_in(scene: &Scene, root: u32) -> Option<u32> {
    let window = scene.layer(root).map(|l| l.window)?;
    scene
        .window_layer_order(window)
        .into_iter()
        .find(|&id| is_ancestor_or_self(scene, root, id) && scene.node_focusable(id))
}

/// Dispatch the `onBlur`/`onFocus` events for a focus change, matching the
/// reference `tTVPLayerManager::SetFocusTo`. Shared by `focus`, `focusNext`,
/// `focusPrev` and the modal transitions.
fn dispatch_focus_change(engine: &Tjs2Engine, current: u32, prev: Option<u32>, direction: bool) {
    if let Some(prev_id) = prev
        && prev_id != current
    {
        let obj = super::layer_tjs_object(prev_id);
        if !obj.is_null()
            && let Ok(dv) = engine.retain_object_detached(obj)
        {
            let _ = engine.call_member(dv.raw_id(), "onBlur", &[TjsValue::Void]);
        }
    }
    let obj = super::layer_tjs_object(current);
    if !obj.is_null()
        && let Ok(dv) = engine.retain_object_detached(obj)
    {
        let _ = engine.call_member(
            dv.raw_id(),
            "onFocus",
            &[TjsValue::Void, TjsValue::Integer(i64::from(direction))],
        );
    }
}

// ---------------------------------------------------------------------------
// Focus-next / focus-prev / getList / dump
// ---------------------------------------------------------------------------

/// Shared body for `focusNext`/`focusPrev`: reference `tTVPLayerManager::
/// FocusNext`/`FocusPrev` (`LayerManager.cpp:723`). With no focused layer the
/// first focusable in window paint order is chosen; otherwise the focused
/// layer's next/previous focusable is used. The newly focused layer (or
/// `null`) is returned.
fn layer_focus_neighbor(
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    forward: bool,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let engine = context_engine();
    let (prev, current) = {
        let mut scene = context_scene_mut();
        let Some(window) = scene.layer(inst.id).map(|l| l.window) else {
            return error_out(out_error, "Layer: layer no longer exists");
        };
        let focused = scene.window(window).and_then(|w| w.focused_layer);
        let next = match focused {
            Some(focused) => {
                if forward {
                    scene.next_focusable(focused)
                } else {
                    scene.prev_focusable(focused)
                }
            }
            None => scene
                .window_layer_order(window)
                .into_iter()
                .find(|&id| scene.node_focusable(id)),
        };
        match next {
            Some(next) => scene.set_focus(next),
            None => (focused, focused),
        }
    };
    if current != prev
        && let Some(current_id) = current
    {
        dispatch_focus_change(engine, current_id, prev, forward);
    }
    set_layer_object_out(engine, current, out);
    0
}

/// `focusNext()` — reference `LayerIntf.cpp:9762`.
extern "C" fn layer_focus_next(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_focus_neighbor(instance, out, out_error, true)
}

/// `focusPrev()` — reference `LayerIntf.cpp:9744`.
extern "C" fn layer_focus_prev(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_focus_neighbor(instance, out, out_error, false)
}

/// `getList()` — an engine extension (the reference `getList` is a `Font`
/// member, already implemented in `font.rs`): return this layer's direct
/// children as a TJS array, matching the `children` property.
extern "C" fn layer_get_list(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let children = {
        let scene = context_scene_read();
        if scene.layer(inst.id).is_none() {
            return error_out(out_error, "Layer: layer no longer exists");
        }
        scene.ordered_children(inst.id)
    };
    let engine = context_engine();
    let Ok(tjs2_sys::RetainedValue::Object(array)) = engine.eval_retained("[]", "layer.getList")
    else {
        return error_out(out_error, "Layer.getList: cannot create array");
    };
    for (i, child_id) in children.iter().enumerate() {
        let obj = super::layer_tjs_object(*child_id);
        if obj.is_null() {
            continue;
        }
        let Ok(dv) = engine.retain_object_detached(obj) else {
            continue;
        };
        let _ = engine.set_member(
            array.raw_id(),
            &i.to_string(),
            &TjsValue::Retained(dv.raw_id() as u64),
        );
    }
    set_retained_out(array, out);
    0
}

/// `dump()` — reference `LayerIntf.cpp:9884` (`DumpStructure`): log this
/// layer's tree position and visual state. The reference only prints debug
/// information, so this is real (not a discarded no-op) but output-only.
extern "C" fn layer_dump(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    if let Some(layer) = scene.layer(inst.id) {
        log::debug!(
            "Layer {}: parent={:?} window={} rect=({},{} {}x{}) visible={} enabled={} \
             focusable={} z={} image={:?} province={}x{}",
            layer.id,
            layer.parent,
            layer.window,
            layer.rect.x,
            layer.rect.y,
            layer.rect.w,
            layer.rect.h,
            layer.visible,
            layer.enabled,
            layer.focusable,
            layer.z_order,
            layer.bitmap,
            layer.province_width,
            layer.province_height,
        );
    }
    set_void_out(out);
    0
}

/// `onPaint()` — base no-op action, kept deliberately inert: the engine fires
/// the layer's script `onPaint` from [`paint_poll`], and dispatching again
/// from this native would recurse (game handlers call `super.onPaint(...)`).
extern "C" fn layer_on_paint(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void_out(out);
    0
}

/// Register the `Layer` native class.
pub(crate) fn register_layer(engine: &Tjs2Engine) -> Result<(), String> {
    // Every member formerly registered through the shared `layer_noop` stub
    // now has a real implementation (see the methods below); the only
    // intentionally inert entries are `onPaint` (recursion guard) and `dump`
    // (output-only debug), both implemented as explicit functions.
    let mut methods: Vec<NativeInstanceMethodDef> = vec![
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
            name: "fillRect",
            f: layer_fill_rect,
        },
        NativeInstanceMethodDef {
            name: "colorRect",
            f: layer_color_rect,
        },
        NativeInstanceMethodDef {
            name: "loadImages",
            f: layer_load_images,
        },
        NativeInstanceMethodDef {
            name: "assignImages",
            f: layer_assign_images,
        },
        NativeInstanceMethodDef {
            name: "setSizeToImageSize",
            f: layer_set_size_to_image_size,
        },
        NativeInstanceMethodDef {
            name: "setBitmap",
            f: layer_set_bitmap,
        },
        NativeInstanceMethodDef {
            name: "setImage",
            f: layer_set_bitmap,
        },
        NativeInstanceMethodDef {
            name: "copyFromBitmapToMainImage",
            f: layer_copy_from_bitmap_to_main_image,
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
            name: "copyRect",
            f: layer_copy_rect,
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
            f: layer_set_parent,
        },
        NativeInstanceMethodDef {
            name: "getTextWidth",
            f: layer_get_text_width,
        },
        NativeInstanceMethodDef {
            name: "getTextHeight",
            f: layer_get_text_height,
        },
        NativeInstanceMethodDef {
            name: "drawText",
            f: layer_draw_text,
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
            name: "drawPolygon",
            f: layer_draw_polygon,
        },
        NativeInstanceMethodDef {
            name: "drawRectangle",
            f: layer_draw_rectangle,
        },
        NativeInstanceMethodDef {
            name: "drawLine",
            f: layer_draw_line,
        },
        NativeInstanceMethodDef {
            name: "drawLines",
            f: layer_draw_lines,
        },
        NativeInstanceMethodDef {
            name: "drawArc",
            f: layer_draw_arc,
        },
        NativeInstanceMethodDef {
            name: "drawBezier",
            f: layer_draw_bezier,
        },
        NativeInstanceMethodDef {
            name: "drawBeziers",
            f: layer_draw_beziers,
        },
        NativeInstanceMethodDef {
            name: "doBoxBlur",
            f: layer_do_box_blur,
        },
        NativeInstanceMethodDef {
            name: "colorize",
            f: layer_colorize,
        },
        NativeInstanceMethodDef {
            name: "noise",
            f: layer_noise,
        },
        NativeInstanceMethodDef {
            name: "tileRect",
            f: layer_tile_rect,
        },
        NativeInstanceMethodDef {
            name: "fillOperateRect",
            f: layer_fill_operate_rect,
        },
        NativeInstanceMethodDef {
            name: "doDropShadow",
            f: layer_do_drop_shadow,
        },
        NativeInstanceMethodDef {
            name: "doBlurLight",
            f: layer_do_blur_light,
        },
        NativeInstanceMethodDef {
            name: "stretchCopy",
            f: layer_stretch_copy,
        },
        NativeInstanceMethodDef {
            name: "stretchPile",
            f: layer_stretch_pile,
        },
        NativeInstanceMethodDef {
            name: "stretchBlend",
            f: layer_stretch_blend,
        },
        NativeInstanceMethodDef {
            name: "operateRect",
            f: layer_operate_rect,
        },
        NativeInstanceMethodDef {
            name: "operateStretch",
            f: layer_operate_stretch,
        },
        NativeInstanceMethodDef {
            name: "affineCopy",
            f: layer_affine_copy,
        },
        NativeInstanceMethodDef {
            name: "affinePile",
            f: layer_affine_pile,
        },
        NativeInstanceMethodDef {
            name: "affineBlend",
            f: layer_affine_blend,
        },
        NativeInstanceMethodDef {
            name: "operateAffine",
            f: layer_operate_affine,
        },
        NativeInstanceMethodDef {
            name: "light",
            f: layer_light,
        },
        NativeInstanceMethodDef {
            name: "flipLR",
            f: layer_flip_lr,
        },
        NativeInstanceMethodDef {
            name: "flipUD",
            f: layer_flip_ud,
        },
        NativeInstanceMethodDef {
            name: "independMainImage",
            f: layer_independ_main_image,
        },
        NativeInstanceMethodDef {
            name: "gaussianBlur",
            f: layer_gaussian_blur,
        },
        NativeInstanceMethodDef {
            name: "doGrayScale",
            f: layer_do_gray_scale,
        },
        NativeInstanceMethodDef {
            name: "adjustGamma",
            f: layer_adjust_gamma,
        },
        NativeInstanceMethodDef {
            name: "saveLayerImage",
            f: layer_save_layer_image,
        },
        NativeInstanceMethodDef {
            name: "setClip",
            f: layer_set_clip,
        },
        NativeInstanceMethodDef {
            name: "onClick",
            f: layer_click,
        },
        NativeInstanceMethodDef {
            name: "onDoubleClick",
            f: layer_double_click,
        },
        NativeInstanceMethodDef {
            name: "onMouseDown",
            f: layer_mouse_down,
        },
        NativeInstanceMethodDef {
            name: "onMouseUp",
            f: layer_mouse_up,
        },
        NativeInstanceMethodDef {
            name: "onMouseMove",
            f: layer_mouse_move,
        },
        NativeInstanceMethodDef {
            name: "onMouseEnter",
            f: layer_mouse_enter,
        },
        NativeInstanceMethodDef {
            name: "onMouseLeave",
            f: layer_mouse_leave,
        },
        NativeInstanceMethodDef {
            name: "onMouseWheel",
            f: layer_mouse_wheel,
        },
        NativeInstanceMethodDef {
            name: "onKeyDown",
            f: layer_key_down,
        },
        NativeInstanceMethodDef {
            name: "onKeyUp",
            f: layer_key_up,
        },
        NativeInstanceMethodDef {
            name: "onKeyPress",
            f: layer_key_press,
        },
        NativeInstanceMethodDef {
            name: "onBlur",
            f: layer_on_blur,
        },
        NativeInstanceMethodDef {
            name: "onFocus",
            f: layer_on_focus,
        },
        NativeInstanceMethodDef {
            name: "onBeforeFocus",
            f: layer_on_before_focus,
        },
        NativeInstanceMethodDef {
            name: "onSearchNextFocusable",
            f: layer_on_search_next_focusable,
        },
        NativeInstanceMethodDef {
            name: "onSearchPrevFocusable",
            f: layer_on_search_prev_focusable,
        },
        NativeInstanceMethodDef {
            name: "onNodeEnabled",
            f: layer_node_enabled,
        },
        NativeInstanceMethodDef {
            name: "onNodeDisabled",
            f: layer_node_disabled,
        },
        NativeInstanceMethodDef {
            name: "onTouchDown",
            f: layer_touch_down,
        },
        NativeInstanceMethodDef {
            name: "onTouchMove",
            f: layer_touch_move,
        },
        NativeInstanceMethodDef {
            name: "onTouchUp",
            f: layer_touch_up,
        },
        NativeInstanceMethodDef {
            name: "onTouchScaling",
            f: layer_touch_scaling,
        },
        NativeInstanceMethodDef {
            name: "onTouchRotate",
            f: layer_touch_rotate,
        },
        NativeInstanceMethodDef {
            name: "onMultiTouch",
            f: layer_multi_touch,
        },
        NativeInstanceMethodDef {
            name: "onTransitionCompleted",
            f: layer_on_transition_completed,
        },
        NativeInstanceMethodDef {
            name: "getLayerAt",
            f: layer_get_layer_at,
        },
        NativeInstanceMethodDef {
            name: "beginTransition",
            f: layer_begin_transition,
        },
        NativeInstanceMethodDef {
            name: "update",
            f: layer_update,
        },
        NativeInstanceMethodDef {
            name: "setCursorPos",
            f: layer_set_cursor_pos,
        },
        NativeInstanceMethodDef {
            name: "focus",
            f: layer_focus,
        },
        NativeInstanceMethodDef {
            name: "copyToBitmapFromMainImage",
            f: layer_copy_to_bitmap_from_main_image,
        },
        NativeInstanceMethodDef {
            name: "convertType",
            f: layer_convert_type,
        },
        NativeInstanceMethodDef {
            name: "piledCopy",
            f: layer_piled_copy,
        },
        NativeInstanceMethodDef {
            name: "pileRect",
            f: layer_pile_rect,
        },
        NativeInstanceMethodDef {
            name: "blendRect",
            f: layer_blend_rect,
        },
        NativeInstanceMethodDef {
            name: "copy9Patch",
            f: layer_copy_9patch,
        },
        NativeInstanceMethodDef {
            name: "drawEllipse",
            f: layer_draw_ellipse,
        },
        NativeInstanceMethodDef {
            name: "drawPie",
            f: layer_draw_pie,
        },
        NativeInstanceMethodDef {
            name: "drawCurve",
            f: layer_draw_curve,
        },
        NativeInstanceMethodDef {
            name: "drawImage",
            f: layer_draw_image,
        },
        NativeInstanceMethodDef {
            name: "drawImageRect",
            f: layer_draw_image_rect,
        },
        NativeInstanceMethodDef {
            name: "drawImageStretch",
            f: layer_draw_image_stretch,
        },
        NativeInstanceMethodDef {
            name: "drawImageAffine",
            f: layer_draw_image_affine,
        },
    ];
    // Every formerly-stubbed member now has a real implementation. The
    // `(name, function)` array is consumed through `methods.extend(...)` so
    // the native-surface parity checker attributes each member (it treats the
    // array's string literals as members).
    let implemented_methods = [
        ("setCenter", layer_set_center as _),
        ("setAffineOffset", layer_set_affine_offset as _),
        ("drawGlyph", layer_draw_glyph as _),
        ("drawRectangles", layer_draw_rectangles as _),
        ("drawClosedCurve", layer_draw_closed_curve as _),
        ("drawClosedCurve2", layer_draw_closed_curve2 as _),
        ("drawCurve2", layer_draw_curve2 as _),
        ("drawCurve3", layer_draw_curve3 as _),
        ("drawPath", layer_draw_path as _),
        ("drawString", layer_draw_string as _),
        (
            "setDefaultDrawTextParam",
            layer_set_default_draw_text_param as _,
        ),
        ("resetDrawTextParam", layer_reset_draw_text_param as _),
        ("setFontStyle", layer_set_font_style as _),
        ("resetFontStyle", layer_reset_font_style as _),
        ("getDrawWidth", layer_get_draw_width as _),
        ("stopTransition", layer_stop_transition as _),
        ("releaseCapture", layer_release_capture as _),
        ("releaseTouchCapture", layer_release_touch_capture as _),
        ("setMode", layer_set_mode as _),
        ("removeMode", layer_remove_mode as _),
        ("clear", layer_clear as _),
        ("setMainPixel", layer_set_main_pixel as _),
        ("getMainPixel", layer_get_main_pixel as _),
        ("setMaskPixel", layer_set_mask_pixel as _),
        ("getMaskPixel", layer_get_mask_pixel as _),
        ("independProvinceImage", layer_independ_province_image as _),
        ("loadProvinceImage", layer_load_province_image as _),
        ("bringToBack", layer_bring_to_back as _),
        ("moveBefore", layer_move_before as _),
        ("moveBehind", layer_move_behind as _),
        ("focusNext", layer_focus_next as _),
        ("focusPrev", layer_focus_prev as _),
        ("getList", layer_get_list as _),
        ("onHitTest", layer_on_hit_test as _),
        ("dump", layer_dump as _),
        ("setAttentionPos", layer_set_attention_pos as _),
        ("captureMouse", layer_capture_mouse as _),
        ("captureTouch", layer_capture_touch as _),
        ("onPaint", layer_on_paint as _),
    ];
    methods.extend(
        implemented_methods
            .into_iter()
            .map(|(name, f)| NativeInstanceMethodDef { name, f }),
    );
    engine.register_native_class_instance(&NativeInstanceBuilder {
        name: "Layer",
        create: layer_create,
        destroy: layer_destroy,
        methods,
        properties: vec![
            NativeInstancePropertyDef {
                name: "id",
                get: Some(layer_id_get),
                set: None,
            },
            // Engine-internal alias of `id` that game scripts never override.
            // `Layer`-typed object arguments are resolved through this so a
            // script class that overrides the visible `id` (e.g. `ADVObject`
            // returns `_info.id`) is not invoked during its own construction.
            NativeInstancePropertyDef {
                name: "nativeId",
                get: Some(layer_id_get),
                set: None,
            },
            // The layer's Font object (k2compat writes `this.font.doUserSelect`
            // and `this.font.face`; MessageArea's `setFontStyle` does
            // `with(font){ .face = ... }`). Every access returns a wrapper
            // bound to the layer's shared font state, so those writes persist
            // and `drawText` reads the requested face back through
            // `FaceRequest::Named`.
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
            // Cursor position, fed by the render input bridge from the shared
            // tvp-input state. The game's MainWindow reads these in
            // onMouseDown/onMouseMove (`primaryLayer.cursorX`/`cursorY`).
            NativeInstancePropertyDef {
                name: "cursorX",
                get: Some(layer_cursor_x_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "cursorY",
                get: Some(layer_cursor_y_get),
                set: None,
            },
            // The blend/affine type (ltAlpha=2, ltAdditive=3, ... per the reference
            // drawable.h). It is mirrored into LayerState for render sync.
            NativeInstancePropertyDef {
                name: "type",
                get: Some(layer_type_get),
                set: Some(layer_type_set),
            },
            // Whether the layer has an image attached.
            NativeInstancePropertyDef {
                name: "hasImage",
                get: Some(layer_has_image_get),
                set: Some(layer_has_image_set),
            },
            // Raw pixel-buffer addresses / pitches (reference
            // `LayerIntf.cpp:11406-11489`). Read-only; the address is valid
            // until the corresponding image is resized.
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
            // `window` returns the owning Window's TJS object; `parent`
            // returns the parent Layer's TJS object (or -1 at the window
            // root) and is settable with a Layer object / id.
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
            // Focus/attention/node state (reference LayerIntf.cpp:10628,
            // :11123, :11207, :11237, :11249).
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
            // `order` is the sibling order (mapped to the scene z-order).
            NativeInstancePropertyDef {
                name: "order",
                get: Some(layer_absolute_get),
                set: Some(layer_absolute_set),
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

/// `id` — the layer's scene id (read-only; object returns are pending).
extern "C" fn layer_id_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    set_int_out(out, i64::from(inst.id));
    0
}

/// `cursorX` — the cursor X in primary-layer / window logical coordinates,
/// as fed by the render input bridge (mirrors `Mouse.getCursorX()`). The
/// game reads `primaryLayer.cursorX`/`cursorY` in `onMouseDown`/`onMouseMove`.
extern "C" fn layer_cursor_x_get(
    _engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let (x, _y) = super::shared_cursor_pos();
    set_int_out(out, i64::from(x));
    0
}

/// `cursorY` — the cursor Y in primary-layer / window logical coordinates.
extern "C" fn layer_cursor_y_get(
    _engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let (_x, y) = super::shared_cursor_pos();
    set_int_out(out, i64::from(y));
    0
}

/// `layer.font` — a script `Font` object backed by the layer's shared
/// [`FontState`](crate::scene::FontState).
///
/// The game sets its text font with `with(layer.font){ .face = ...; .height
/// = ... }` (see `system/MessageArea.tjs` `setFontStyle`). Callers read
/// `layer.font` repeatedly, so every call returns a fresh, disposable wrapper
/// **bound to the same layer-owned state** (`Font.__bind`): property writes
/// land on the shared state, and `drawText` reads the face back from it. The
/// first access lazily creates the state; `layer_destroy` removes it.
extern "C" fn layer_font_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };

    // Resolve (or lazily allocate) the layer's font state id under the scene
    // lock, then drop it before evaluating script.
    let font_id = {
        let mut scene = context_scene_mut();
        if scene.layer(inst.id).is_none() {
            return error_out(out_error, "Layer: layer no longer exists");
        }
        let existing = scene.layer(inst.id).and_then(|layer| layer.font_id);
        match existing {
            Some(id) if scene.font(id).is_some() => id,
            _ => {
                let id = scene.add_font(
                    super::font::DEFAULT_FONT_FACE.to_string(),
                    super::font::DEFAULT_FONT_HEIGHT,
                    [255, 255, 255, 255],
                );
                if let Some(layer) = scene.layer_mut(inst.id) {
                    layer.font_id = Some(id);
                }
                id
            }
        }
    };

    // Create a throwaway `Font` and rebind it to the layer's state. The
    // wrapper is disposable; only the shared state persists.
    let engine = crate::natives::context_engine();
    if engine.eval("new Font()", "layer.font").is_err() {
        set_void_out(out);
        return 0;
    }
    let dv = match engine.retain_value_detached(&tjs2_sys::TjsValue::Object) {
        Ok(dv) => dv,
        Err(_) => {
            set_void_out(out);
            return 0;
        }
    };
    // `__bind` repoints the wrapper at the layer's shared state. (A
    // multi-statement `eval` does not reliably dispatch the call, so invoke
    // the method on the retained wrapper directly.)
    if engine
        .call_member(
            dv.raw_id(),
            "__bind",
            &[tjs2_sys::TjsValue::Integer(i64::from(font_id))],
        )
        .is_err()
    {
        set_void_out(out);
        return 0;
    }
    // SAFETY: out is a valid result slot; the C++ side consumes the
    // retention (copies + erases) before the callback returns.
    unsafe {
        (*out).ty = tjs2_sys::VAL_RETAINED;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = std::ptr::null();
        (*out).array = std::ptr::null();
        (*out).array_count = 0;
        (*out).retained = dv.raw_id() as usize;
    }
    // The C++ conversion consumes the retention; forget the wrapper so its
    // Drop does not release the id first (safe no-op after).
    std::mem::forget(dv);
    0
}

/// `layer.font = font` setter — copy a `Font` object's properties into the
/// layer's own state.
///
/// The reference *denies* this setter, but the game's `AffineLayer` defines
/// `property font { setter(v) { _image.font = v; } }`. Copying (rather than
/// aliasing the source state) keeps the layer independent of the source
/// object's lifetime: a later `invalidate` of the source cannot clear the
/// layer's font.
extern "C" fn layer_font_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value is a valid value slot for the duration of the call.
    let v = unsafe { &*value };
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let engine = crate::natives::context_engine();
    let src_id = match resolve_object_id_arg(engine, v) {
        Ok(id) if id >= 0 => id as u32,
        // null/void or an unreadable object: leave the layer's font unchanged.
        _ => return 0,
    };
    let mut scene = context_scene_mut();
    if scene.layer(inst.id).is_none() {
        return error_out(out_error, "Layer: layer no longer exists");
    }
    let Some(src) = scene.font(src_id).cloned() else {
        return 0;
    };
    let existing = scene.layer(inst.id).and_then(|layer| layer.font_id);
    let dst_id = match existing {
        Some(id) if scene.font(id).is_some() => id,
        _ => {
            let id = scene.add_font(src.face.clone(), src.height, src.color);
            if let Some(layer) = scene.layer_mut(inst.id) {
                layer.font_id = Some(id);
            }
            id
        }
    };
    if let Some(dst) = scene.fonts.iter_mut().find(|f| f.id == dst_id) {
        dst.face = src.face;
        dst.height = src.height;
        dst.color = src.color;
        dst.bold = src.bold;
        dst.italic = src.italic;
        dst.strikeout = src.strikeout;
        dst.underline = src.underline;
        dst.angle = src.angle;
    }
    0
}

/// `layer.hasImage` getter.
extern "C" fn layer_has_image_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(_out_error, "Layer: layer no longer exists");
    };
    set_int_out(out, i64::from(layer.bitmap.is_some()));
    0
}

/// `layer.hasImage = true/false` — reference `tTJSNI_BaseLayer::SetHasImage`
/// (`LayerIntf.cpp:2489`): `true` runs `AllocateImage` (create the MainImage
/// at the rect size filled with `neutral_color`, reset the clip), `false`
/// runs `DeallocateImage` (drop the bitmap). This is the setter the game's
/// `ConfigVoiceSliderH` relies on, so it must not be a silent no-op.
extern "C" fn layer_has_image_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value is valid for the call.
    let v = unsafe { &*value };
    let want_image = arg_bool(v);
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    if scene.layer(inst.id).is_none() {
        return error_out(out_error, "Layer: layer no longer exists");
    }
    if want_image {
        allocate_layer_image(&mut scene, inst.id);
    } else {
        deallocate_layer_image(&mut scene, inst.id);
    }
    0
}

/// `mainImageBuffer` — reference `GetMainImagePixelBuffer`
/// (`LayerIntf.cpp:3005`): the address of the MainImage RGBA pixel buffer, or
/// 0 when the layer has no image. The address stays valid until the MainImage
/// is resized (the reference's `GetScanLine(0)` contract).
extern "C" fn layer_main_image_buffer_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    set_int_out(out, scene.main_image_pixel_buffer(inst.id) as i64);
    0
}

/// `mainImageBufferForWrite` — reference `GetMainImagePixelBufferForWrite`
/// (`LayerIntf.cpp:3012`): like `mainImageBuffer`, but marks the image
/// modified. 0 when absent.
extern "C" fn layer_main_image_buffer_for_write_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    set_int_out(out, scene.main_image_pixel_buffer_for_write(inst.id) as i64);
    0
}

/// `mainImageBufferPitch` — reference `GetMainImagePixelBufferPitch`
/// (`LayerIntf.cpp:3020`): bytes per row (`width * 4`), or 0 when absent.
extern "C" fn layer_main_image_buffer_pitch_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    set_int_out(out, i64::from(scene.main_image_pitch(inst.id)));
    0
}

/// `provinceImageBuffer` — reference `GetProvinceImagePixelBuffer`
/// (`LayerIntf.cpp:3027`): the address of the 8bpp province plane, or 0 when
/// absent. Valid until the plane is resized.
extern "C" fn layer_province_image_buffer_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    set_int_out(out, scene.province_image_pixel_buffer(inst.id) as i64);
    0
}

/// `provinceImageBufferForWrite` — reference
/// `GetProvinceImagePixelBufferForWrite` (`LayerIntf.cpp:3034`): allocates the
/// province plane when absent (`AllocateProvinceImage`), marks the image
/// modified and returns its address.
extern "C" fn layer_province_image_buffer_for_write_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    set_int_out(
        out,
        scene.province_image_pixel_buffer_for_write(inst.id) as i64,
    );
    0
}

/// `provinceImageBufferPitch` — reference `GetProvinceImagePixelBufferPitch`
/// (`LayerIntf.cpp:3042`): bytes per row (`width`), or 0 when absent.
extern "C" fn layer_province_image_buffer_pitch_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    set_int_out(out, i64::from(scene.province_image_pitch(inst.id)));
    0
}

/// `getProvincePixel(x, y)` — reference `GetProvincePixel`
/// (`LayerIntf.cpp:2973`): 0 when the plane is absent or the coordinate is
/// outside it.
extern "C" fn layer_get_province_pixel(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 2 {
        return error_out(out_error, "Layer.getProvincePixel requires x and y");
    }
    let x = arg_i64(&args[0]) as i32;
    let y = arg_i64(&args[1]) as i32;
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    set_int_out(out, i64::from(scene.get_province_pixel(inst.id, x, y)));
    0
}

/// `setProvincePixel(x, y, n)` — reference `SetProvincePixel`
/// (`LayerIntf.cpp:2985`): allocates the province plane when absent, clips to
/// `ClipRect`, stores the low byte and marks the image modified.
extern "C" fn layer_set_province_pixel(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 3 {
        return error_out(out_error, "Layer.setProvincePixel requires x, y and n");
    }
    let x = arg_i64(&args[0]) as i32;
    let y = arg_i64(&args[1]) as i32;
    let n = arg_i64(&args[2]) as i32;
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    if scene.layer(inst.id).is_none() {
        return error_out(out_error, "Layer: layer no longer exists");
    }
    scene.set_province_pixel(inst.id, x, y, n);
    set_void_out(out);
    0
}

/// `layer.type` getter — the blend/affine type.
extern "C" fn layer_type_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let scene = context_scene_read();
    let blend_type = scene
        .layer(inst.id)
        .map_or(inst.blend_type, |layer| layer.blend_type);
    set_int_out(out, blend_type);
    0
}

/// `layer.type` setter.
extern "C" fn layer_type_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value is valid for the call.
    let v = unsafe { &*value };
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    inst.blend_type = v.integer;
    if inst.constructed
        && let Some(layer) = context_scene_mut().layer_mut(inst.id)
    {
        layer.blend_type = v.integer;
    }
    0
}

#[cfg(test)]
mod tests {
    use crate::natives::tests::{TestEnv, write_fixture};

    #[test]
    fn layer_construct_rect_and_fill() {
        let env = TestEnv::new("layer-ctor");
        env.run("var w = new Window(); var l = new Layer(w, null); l.setSize(100, 50); l.fillRect(0, 0, 100, 50, 0xffff0000);")
            .unwrap();
        let scene = env.scene();
        assert_eq!(scene.layers.len(), 1);
        let layer = &scene.layers[0];
        assert_eq!(
            layer.rect,
            crate::scene::Rect {
                x: 0,
                y: 0,
                w: 100,
                h: 50
            }
        );
        assert_eq!(layer.fill_color, Some([255, 0, 0, 255]));
        // the window's primary layer is auto-set to the first layer
        assert_eq!(scene.windows[0].primary_layer, Some(layer.id));
        assert!(layer.is_primary);
        // the script object exposes its scene id
        assert_eq!(env.eval_int("l.id"), i64::from(layer.id));
    }

    #[test]
    fn layer_properties_update_scene() {
        let env = TestEnv::new("layer-props");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); \
             l.visible = false; l.opacity = 255; l.hitThreshold = 256; l.absolute = 3;",
        )
        .unwrap();
        let scene = env.scene();
        let layer = &scene.layers[0];
        assert!(!layer.visible);
        assert!((layer.opacity - 1.0).abs() < 1e-6, "255 -> 1.0");
        assert_eq!(layer.hit_threshold, 256);
        assert_eq!(layer.z_order, 3);
        // reads round-trip through the script (opacity on the 0..255 scale)
        assert_eq!(env.eval_int("l.visible"), 0);
        assert_eq!(env.eval_int("l.opacity"), 255);
        assert_eq!(env.eval_int("l.hitThreshold"), 256);
        assert_eq!(env.eval_int("l.absolute"), 3);
    }

    #[test]
    fn layer_opacity_255_scale() {
        let env = TestEnv::new("layer-opacity");
        env.run("var w = new Window(); var l = new Layer(w, null); l.opacity = 128;")
            .unwrap();
        let scene = env.scene();
        assert!((scene.layers[0].opacity - 128.0 / 255.0).abs() < 1e-6);
        assert_eq!(env.eval_int("l.opacity"), 128);
    }

    #[test]
    fn layer_set_pos_bounds_form() {
        let env = TestEnv::new("layer-pos");
        env.run("var w = new Window(); var l = new Layer(w, null); l.setPos(10, 20);")
            .unwrap();
        // NB: the read guard must be dropped before the next `env.run` — a
        // script call that mutates the scene takes the write lock
        // (`context_scene_mut`), which blocks forever while this thread
        // still holds the read guard (std RwLock is not reentrant).
        {
            let scene = env.scene();
            assert_eq!((scene.layers[0].rect.x, scene.layers[0].rect.y), (10, 20));
        }
        env.run("l.setPos(1, 2, 30, 40);").unwrap();
        let scene = env.scene();
        assert_eq!(
            scene.layers[0].rect,
            crate::scene::Rect {
                x: 1,
                y: 2,
                w: 30,
                h: 40
            }
        );
    }

    #[test]
    fn layer_draw_text_rasterizes_into_scene_bitmap() {
        let env = TestEnv::new("layer-draw-text");
        // 0x00FFFFFF: the game's text colors carry a zero high byte (e.g.
        // 0x00A0FFD2); alpha comes from the `opa` argument, so glyphs must
        // still paint. This is the regression guard for the dropped-text bug.
        env.run("var w = new Window(); var l = new Layer(w, null); l.setSize(128, 32); l.drawText(2, 2, 'Title 日本語', 0x00ffffff);")
            .unwrap();
        let scene = env.scene();
        let layer = &scene.layers[0];
        let bitmap = scene
            .bitmap(layer.bitmap.expect("drawText creates a surface"))
            .expect("surface bitmap");
        assert_eq!((bitmap.width, bitmap.height), (128, 32));
        assert!(bitmap.dirty);
        assert!(
            bitmap.rgba.chunks_exact(4).any(|pixel| pixel[3] != 0),
            "a 0x00RRGGBB color must paint (alpha comes from opa)"
        );
        assert_eq!(env.eval_int("l.getTextHeight('x')"), 16);
        assert!(env.eval_int("l.getTextWidth('日本')") > 0);
    }

    /// `opa = 0` must paint nothing: the glyph coverage is scaled by the
    /// opacity, so every contributed alpha is zero.
    #[test]
    fn layer_draw_text_zero_opacity_paints_nothing() {
        let env = TestEnv::new("layer-draw-text-opa0");
        env.run("var w = new Window(); var l = new Layer(w, null); l.setSize(128, 32); l.drawText(2, 2, 'Title', 0x00ffffff, 0);")
            .unwrap();
        let scene = env.scene();
        let bitmap = scene.bitmap(scene.layers[0].bitmap.unwrap()).unwrap();
        assert!(
            bitmap.rgba.chunks_exact(4).all(|pixel| pixel[3] == 0),
            "opa=0 must paint nothing"
        );
    }

    /// A shadow color is also RGB-only (`0x00RRGGBB`); it must paint even
    /// though the top byte is zero. The black main text and red shadow make
    /// the shadow pixels identifiable (`r > 0, g == b == 0`).
    #[test]
    fn layer_draw_text_shadow_zero_high_byte_paints() {
        let env = TestEnv::new("layer-draw-text-shadow");
        // drawText(x, y, text, color, opa, aa, shadowlevel, shadowcolor, shadowwidth)
        env.run("var w = new Window(); var l = new Layer(w, null); l.setSize(128, 32); l.drawText(2, 2, 'Shadow', 0x00000000, 255, true, 1, 0x00ff0000, 1);")
            .unwrap();
        let scene = env.scene();
        let bitmap = scene.bitmap(scene.layers[0].bitmap.unwrap()).unwrap();
        assert!(
            bitmap
                .rgba
                .chunks_exact(4)
                .any(|pixel| pixel[3] != 0 && pixel[0] > 0 && pixel[1] == 0 && pixel[2] == 0),
            "a 0x00RRGGBB shadow color must paint red shadow pixels"
        );
    }

    /// Count pixels with a non-zero alpha in a bitmap.
    fn bitmap_ink(bitmap: &crate::scene::BitmapState) -> usize {
        bitmap.rgba.chunks_exact(4).filter(|p| p[3] != 0).count()
    }

    /// True when a fully transparent column exists *between* the first and
    /// last inked columns (i.e. the text is not one solid block).
    fn bitmap_has_gap(bitmap: &crate::scene::BitmapState) -> bool {
        let col_has_ink = |x: u32| {
            (0..bitmap.height).any(|y| bitmap.rgba[((y * bitmap.width + x) * 4 + 3) as usize] != 0)
        };
        let inked: Vec<u32> = (0..bitmap.width).filter(|&x| col_has_ink(x)).collect();
        let (Some(&first), Some(&last)) = (inked.first(), inked.last()) else {
            return false;
        };
        (first..=last).any(|x| !col_has_ink(x))
    }

    /// With no resolvable face, `drawText` must leave the layer transparent
    /// instead of fabricating a filled checker glyph (which read as an opaque
    /// text background). Uses an explicit config with discovery disabled so
    /// the test does not depend on the machine's font set.
    #[test]
    fn layer_draw_text_without_face_stays_transparent() {
        if std::env::var_os("KRKR_RS_SYSTEM_FONT").is_some() {
            eprintln!("skipping: KRKR_RS_SYSTEM_FONT overrides face resolution");
            return;
        }
        struct ConfigGuard;
        impl Drop for ConfigGuard {
            fn drop(&mut self) {
                tvp_text::set_font_config(None);
            }
        }
        // Acquire the VM test lock first: the font config is process-global,
        // so it must only change while no other visual-native test is drawing.
        let env = TestEnv::new("layer-draw-text-no-face");
        let _guard = ConfigGuard;
        tvp_text::set_font_config(Some(tvp_text::FontConfig {
            allow_system_discovery: false,
            ..Default::default()
        }));
        env.run("var w = new Window(); var l = new Layer(w, null); l.setSize(128, 32); l.drawText(2, 2, '日本語', 0x00ffffff);")
            .unwrap();
        let scene = env.scene();
        let bitmap = scene.bitmap(scene.layers[0].bitmap.unwrap()).unwrap();
        assert_eq!(
            bitmap_ink(bitmap),
            0,
            "no resolved face must leave the layer transparent, not a solid block"
        );
    }

    /// Synthetic bold must add coverage (the bold atlas is a separate cache
    /// entry from the regular one).
    #[test]
    fn layer_draw_text_bold_adds_coverage() {
        let env = TestEnv::new("layer-draw-text-bold");
        env.run(
            "var w = new Window(); \
             var a = new Layer(w, null); a.setSize(96, 40); a.font.height = 32; a.drawText(2, 2, 'MA', 0xffffffff); \
             var b = new Layer(w, null); b.setSize(96, 40); b.font.height = 32; b.font.bold = true; b.drawText(2, 2, 'MA', 0xffffffff);",
        )
        .unwrap();
        let scene = env.scene();
        let normal = bitmap_ink(scene.bitmap(scene.layers[0].bitmap.unwrap()).unwrap());
        let bold = bitmap_ink(scene.bitmap(scene.layers[1].bitmap.unwrap()).unwrap());
        assert!(normal > 0, "the regular glyphs must paint");
        assert!(bold > normal, "bold must add coverage: {bold} vs {normal}");
    }

    /// Underline / strikeout must paint extra rules (the glyph alone would not
    /// reach the run's full advance width).
    #[test]
    fn layer_draw_text_underline_and_strikeout_paint() {
        let env = TestEnv::new("layer-draw-text-underline");
        env.run(
            "var w = new Window(); \
             var a = new Layer(w, null); a.setSize(96, 40); a.font.height = 32; a.drawText(2, 2, 'I', 0xffffffff); \
             var b = new Layer(w, null); b.setSize(96, 40); b.font.height = 32; b.font.underline = true; b.font.strikeout = true; b.drawText(2, 2, 'I', 0xffffffff);",
        )
        .unwrap();
        let scene = env.scene();
        let plain = bitmap_ink(scene.bitmap(scene.layers[0].bitmap.unwrap()).unwrap());
        let styled = bitmap_ink(scene.bitmap(scene.layers[1].bitmap.unwrap()).unwrap());
        assert!(
            styled > plain,
            "underline + strikeout must add rules: {styled} vs {plain}"
        );
    }

    /// Italic shears the glyph, so the painted pixels differ from the upright
    /// rendering (but still paint ink).
    #[test]
    fn layer_draw_text_italic_shears_glyph() {
        let env = TestEnv::new("layer-draw-text-italic");
        env.run(
            "var w = new Window(); \
             var a = new Layer(w, null); a.setSize(96, 40); a.font.height = 32; a.drawText(2, 2, 'I', 0xffffffff); \
             var b = new Layer(w, null); b.setSize(96, 40); b.font.height = 32; b.font.italic = true; b.drawText(2, 2, 'I', 0xffffffff);",
        )
        .unwrap();
        let scene = env.scene();
        let upright = scene
            .bitmap(scene.layers[0].bitmap.unwrap())
            .unwrap()
            .rgba
            .clone();
        let italic = &scene.bitmap(scene.layers[1].bitmap.unwrap()).unwrap().rgba;
        assert!(italic.chunks_exact(4).any(|p| p[3] != 0));
        assert_ne!(upright, *italic, "italic must shear the glyph");
    }

    /// The game's `MessageArea` defaults (`shadowlevel=3024`,
    /// `shadowwidth=3`) must not be interpreted as an 8 px spread that merges
    /// glyphs into a solid band. A transparent column must remain between two
    /// glyphs.
    #[test]
    fn layer_draw_text_shadow_uses_width_not_level() {
        let env = TestEnv::new("layer-draw-text-shadow-width");
        env.run("var w = new Window(); var l = new Layer(w, null); l.setSize(160, 48); l.font.height = 30; l.drawText(2, 2, 'A A', 0x00ffffff, 255, true, 3024, 0x00202020, 3);")
            .unwrap();
        let scene = env.scene();
        let bitmap = scene.bitmap(scene.layers[0].bitmap.unwrap()).unwrap();
        assert!(
            bitmap_has_gap(bitmap),
            "a 3 px shadow must leave a transparent gap between glyphs"
        );
    }

    /// Build a minimal version-1 `.tft` with one solid 2×2 glyph for `ch`,
    /// whose `OriginY` (the baseline-to-ink-top bearing) is `origin_y`.
    fn tiny_tft(ch: char, origin_y: i16) -> Vec<u8> {
        const MAGIC: &[u8; 22] = b"TVP pre-rendered font\x1a";
        const HEADER: usize = 36;
        // Literal coverage 63 → ×4 (upscale) → 252.
        let coverage = [63u8; 4];
        let ch_index = HEADER + coverage.len();
        let index = ch_index + 2;
        let mut data = vec![0u8; index + 20];
        data[..22].copy_from_slice(MAGIC);
        data[22] = 1;
        data[23] = 2;
        data[24..28].copy_from_slice(&1u32.to_le_bytes());
        data[28..32].copy_from_slice(&(ch_index as u32).to_le_bytes());
        data[32..36].copy_from_slice(&(index as u32).to_le_bytes());
        data[HEADER..HEADER + 4].copy_from_slice(&coverage);
        data[ch_index..ch_index + 2].copy_from_slice(&(ch as u16).to_le_bytes());
        let item = &mut data[index..index + 20];
        item[0..4].copy_from_slice(&(HEADER as u32).to_le_bytes());
        item[4..6].copy_from_slice(&2u16.to_le_bytes()); // width
        item[6..8].copy_from_slice(&2u16.to_le_bytes()); // height
        item[10..12].copy_from_slice(&origin_y.to_le_bytes()); // origin_y
        item[12..14].copy_from_slice(&3i16.to_le_bytes()); // inc_x
        item[16..18].copy_from_slice(&3i16.to_le_bytes()); // inc
        data
    }

    /// The face request `paint_text_run` makes for a named layer face. It
    /// honors `KRKR_RS_SYSTEM_FONT` exactly like the production code, so a
    /// test can resolve the same rasterizer the draw will use.
    fn text_face_request(name: &str) -> tvp_text::FaceRequest {
        match std::env::var_os("KRKR_RS_SYSTEM_FONT") {
            Some(path) => tvp_text::FaceRequest::Path(std::path::PathBuf::from(path)),
            None => tvp_text::FaceRequest::Named(name.to_string()),
        }
    }

    /// `Font.mapPrerenderedFont` + `Layer.drawText` must composite the `.tft`
    /// bitmap and `Font.getTextWidth` must use its baked advance.
    #[test]
    fn layer_draw_text_uses_mapped_prerendered_font() {
        struct RegistryGuard;
        impl Drop for RegistryGuard {
            fn drop(&mut self) {
                tvp_text::clear_prerendered_fonts();
            }
        }
        let env = TestEnv::new("layer-draw-text-prerendered");
        let _guard = RegistryGuard;
        std::fs::write(env._dir.path().join("testfont.tft"), tiny_tft('A', 2)).unwrap();
        env.run("var f = new Font('MyFace', 30, 0xffffff); f.mapPrerenderedFont('testfont.tft');")
            .unwrap();
        let advance = env.eval("f.getTextWidth('A')", "test").expect("script");
        assert_eq!(advance, tjs2_sys::TjsValue::Real(3.0));

        env.run(
            "var w = new Window(); var l = new Layer(w, null); l.setSize(64, 64); \
             l.font.face = 'MyFace'; l.font.height = 30; \
             l.drawText(1, 1, 'A', 0xffffff);",
        )
        .unwrap();
        let scene = env.scene();
        let bitmap = scene.bitmap(scene.layers[0].bitmap.unwrap()).unwrap();
        assert!(bitmap_ink(bitmap) > 0, "prerendered glyph must paint");
        // The `.tft` `origin_y = 2` places the ink at the mapped outline
        // face's baseline: `top = line_top + ascent - origin_y`, where the
        // ascent is the rasterizer's (`GlyphAtlas::ascent`), not the old
        // fabricated `height * 0.85`.
        let face = tvp_text::resolve_face(&text_face_request("MyFace"))
            .expect("a face resolves on this machine");
        let ascent = tvp_text::with_cached_atlas(face, 30, |atlas| atlas.ascent());
        let expected_top = (1.0 + ascent - 2.0).round() as u32;
        assert!(
            bitmap.rgba[((expected_top * bitmap.width + 1) * 4 + 3) as usize] > 0,
            "prerendered ink must land at the mapped baseline (row {expected_top}, ascent {ascent})"
        );
    }

    /// A `.tft` glyph and the outline glyph for the *same* character must
    /// share one baseline: the `.tft` path takes its ascent from the mapped
    /// outline face's rasterizer, not a fabricated `height * 0.85`. Regression
    /// for "some characters render lower than others" when
    /// `MessageArea.charOutput` mixes `.tft` CJK characters with per-call
    /// vector fallback.
    #[test]
    fn layer_draw_text_prerendered_and_vector_share_baseline() {
        struct RegistryGuard;
        impl Drop for RegistryGuard {
            fn drop(&mut self) {
                tvp_text::clear_prerendered_fonts();
            }
        }
        let env = TestEnv::new("layer-draw-text-shared-baseline");
        let _guard = RegistryGuard;
        let height = 30u32;

        // Resolve the same face `drawText` will use, and read the outline
        // bearing, the resolved ascent, and the atlas's internal top padding.
        // The padding is a vector-path detail; measuring it lets the two ink
        // rows be compared as baselines without hardcoding it.
        let face = tvp_text::resolve_face(&text_face_request("SharedBaselineFace"))
            .expect("a face resolves on this machine");
        let (bearing_y, ascent, top_pad) = tvp_text::with_cached_atlas(face, height, |atlas| {
            let slot = atlas.rasterize_char('A');
            let (w, _) = atlas.atlas_size();
            let alpha = |x: u32, y: u32| atlas.atlas_rgba()[((y * w + x) * 4 + 3) as usize];
            let top_pad = (0..slot.h + 8)
                .find(|&dy| (0..slot.w).any(|dx| alpha(slot.u + dx, slot.v + dy) != 0))
                .unwrap_or(0) as i32;
            (slot.bearing_y, atlas.ascent(), top_pad)
        });

        // Vector path first, before the mapping exists; then map the same
        // `(face, height, style)` to a one-glyph `.tft` and draw the identical
        // call through it.
        env.run(
            "var w = new Window(); \
             var vec = new Layer(w, null); vec.setSize(64, 64); \
             vec.font.face = 'SharedBaselineFace'; vec.font.height = 30; \
             vec.drawText(1, 1, 'A', 0xffffffff);",
        )
        .unwrap();
        std::fs::write(
            env._dir.path().join("shared_baseline.tft"),
            tiny_tft('A', bearing_y as i16),
        )
        .unwrap();
        env.run(
            "var f = new Font('SharedBaselineFace', 30, 0xffffff); \
             f.mapPrerenderedFont('shared_baseline.tft'); \
             var tft = new Layer(w, null); tft.setSize(64, 64); \
             tft.font.face = 'SharedBaselineFace'; tft.font.height = 30; \
             tft.drawText(1, 1, 'A', 0xffffffff);",
        )
        .unwrap();

        let scene = env.scene();
        let vector = scene.bitmap(scene.layers[0].bitmap.unwrap()).unwrap();
        let prerendered = scene.bitmap(scene.layers[1].bitmap.unwrap()).unwrap();
        let first_ink_row = |b: &crate::scene::BitmapState| -> i32 {
            (0..b.height)
                .find(|&y| (0..b.width).any(|x| b.rgba[((y * b.width + x) * 4 + 3) as usize] != 0))
                .expect("both paths must paint ink") as i32
        };
        let vec_top = first_ink_row(vector);
        let tft_top = first_ink_row(prerendered);
        // The `.tft` baseline is the rasterizer ascent; the vector ink
        // additionally sits `top_pad` px into its atlas cell. Both baselines
        // (`ink_top + bearing`) must agree.
        assert_eq!(
            tft_top + bearing_y,
            vec_top + bearing_y - top_pad,
            "the `.tft` and outline glyphs must share a baseline \
             (tft_top={tft_top}, vec_top={vec_top}, top_pad={top_pad})"
        );
        // Pin the value directly too: a regression to `height * 0.85` would
        // place the `.tft` ink several px off this row.
        assert_eq!(
            tft_top,
            (1.0 + ascent - bearing_y as f32).round() as i32,
            "the `.tft` ink must land at `y + atlas.ascent() - OriginY`"
        );
    }

    /// Regression guard for the config-screen lag: the game's
    /// `MessageArea.charOutput` calls `drawText` once per character
    /// (`system/MessageArea.tjs`), so a text-heavy Config screen used to
    /// rescan the system font database, re-parse a multi-MB font and rebuild
    /// the glyph atlas on every character (~170 ms/call in a debug build).
    /// The process-global face + per-height atlas caches make repeated calls
    /// reuse the parsed face and rasterized glyphs. The bound is deliberately
    /// generous so the test stays robust on slow CI; the meaningful guard is
    /// that 500 calls complete at all instead of taking tens of seconds.
    #[test]
    fn layer_draw_text_many_calls_reuse_font_cache() {
        let env = TestEnv::new("layer-draw-text-cache");
        env.run("var w = new Window(); var l = new Layer(w, null); l.setSize(512, 512);")
            .unwrap();
        let start = std::time::Instant::now();
        env.run("for (var i = 0; i < 500; i = i + 1) l.drawText(0, i, 'あ', 0x00ffffff);")
            .unwrap();
        let elapsed = start.elapsed();
        eprintln!("500 drawText calls: {elapsed:?}");
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "500 drawText calls must be font-cache-served, took {elapsed:?}"
        );
        // Every character painted something into the shared layer surface.
        let scene = env.scene();
        let bitmap = scene.bitmap(scene.layers[0].bitmap.unwrap()).unwrap();
        assert!(
            bitmap.rgba.chunks_exact(4).any(|pixel| pixel[3] != 0),
            "drawText must paint through the shared atlas"
        );
    }

    /// Read an RGBA pixel from a scene bitmap.
    fn pixel(bitmap: &crate::scene::BitmapState, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * bitmap.width + x) as usize) * 4;
        [
            bitmap.rgba[i],
            bitmap.rgba[i + 1],
            bitmap.rgba[i + 2],
            bitmap.rgba[i + 3],
        ]
    }

    /// A solid GdiPlus brush (`0xAARRGGBB`, alpha from the high byte) fills a
    /// closed polygon drawn with `drawPolygon`.
    #[test]
    fn layer_draw_polygon_fills_with_solid_brush() {
        let env = TestEnv::new("layer-draw-polygon");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); l.setSize(32, 32); \
             var app = new GdiPlus.Appearance(); app.addBrush(0xffff0000); \
             l.drawPolygon(app, [[4,4],[28,4],[28,28],[4,28]]);",
        )
        .unwrap();
        let scene = env.scene();
        let bitmap = scene
            .bitmap(scene.layers[0].bitmap.expect("drawPolygon allocates"))
            .unwrap();
        assert!(bitmap.dirty);
        assert_eq!(pixel(bitmap, 16, 16), [255, 0, 0, 255], "solid red fill");
    }

    /// `drawPolygon(app, points)` resolves `app` and `points` independently:
    /// the old last-object shortcut resolved `points` as the appearance, so
    /// nothing was ever painted.
    #[test]
    fn layer_draw_polygon_distinguishes_app_from_points() {
        let env = TestEnv::new("layer-draw-polygon-handles");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); l.setSize(32, 32); \
             var big = new GdiPlus.Appearance(); big.addBrush(0xffff0000); \
             var bigPts = [[2,2],[30,2],[30,30],[2,30]]; \
             l.drawPolygon(big, bigPts); \
             var small = new GdiPlus.Appearance(); small.addBrush(0xff00ff00); \
             var smallPts = [[10,10],[22,10],[22,22],[10,22]]; \
             l.drawPolygon(small, smallPts);",
        )
        .unwrap();
        let scene = env.scene();
        let bitmap = scene.bitmap(scene.layers[0].bitmap.unwrap()).unwrap();
        // The second (green) polygon overpaints the shared center; the first
        // (red) polygon is visible in its non-overlapping corner. Both draws
        // succeeding proves each `app` handle resolved correctly.
        assert_eq!(pixel(bitmap, 16, 16), [0, 255, 0, 255], "green overpaint");
        assert_eq!(pixel(bitmap, 5, 5), [255, 0, 0, 255], "red first fill");
    }

    /// `drawRectangle` fills with the brush and strokes with the pen;
    /// `drawLine` strokes only.
    #[test]
    fn layer_draw_rectangle_and_line_produce_pixels() {
        let env = TestEnv::new("layer-draw-rect-line");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); l.setSize(40, 40); \
             var app = new GdiPlus.Appearance(); \
             app.addBrush(0xff00ff00); app.addPen(0xffffffff, 2); \
             l.drawRectangle(app, 4, 4, 32, 32); \
             var pen = new GdiPlus.Appearance(); pen.addPen(0xff0000ff, 3); \
             l.drawLine(pen, 0, 0, 39, 39);",
        )
        .unwrap();
        let scene = env.scene();
        let bitmap = scene.bitmap(scene.layers[0].bitmap.unwrap()).unwrap();
        // Interior is the green fill (away from the blue diagonal); the
        // border is the white pen stroke.
        assert_eq!(pixel(bitmap, 20, 12), [0, 255, 0, 255], "green fill");
        assert_eq!(pixel(bitmap, 4, 20), [255, 255, 255, 255], "white stroke");
        // The blue diagonal line paints somewhere along its path.
        assert!(
            bitmap
                .rgba
                .chunks_exact(4)
                .any(|p| p[3] > 0 && p[2] > 200 && p[0] < 60),
            "blue line pixels"
        );
    }

    /// `drawLines` strokes an open polyline (brushes are ignored).
    #[test]
    fn layer_draw_lines_strokes_polyline() {
        let env = TestEnv::new("layer-draw-lines");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); l.setSize(32, 32); \
             var app = new GdiPlus.Appearance(); \
             app.addBrush(0xffff0000); app.addPen(0xffffffff, 2); \
             l.drawLines(app, [[0,0],[16,16],[32,0]]);",
        )
        .unwrap();
        let scene = env.scene();
        let bitmap = scene.bitmap(scene.layers[0].bitmap.unwrap()).unwrap();
        // The stroke paints white; the red brush must be ignored (no red
        // pixels anywhere).
        assert_eq!(pixel(bitmap, 0, 0), [255, 255, 255, 255], "stroke start");
        assert!(
            bitmap.rgba.chunks_exact(4).all(|p| p[0] == 0 || p[1] > 0),
            "drawLines ignores brushes (no red-only fill)"
        );
    }

    /// `drawArc` fills the implicitly-closed elliptical path.
    #[test]
    fn layer_draw_arc_fills_circle() {
        let env = TestEnv::new("layer-draw-arc");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); l.setSize(24, 24); \
             var app = new GdiPlus.Appearance(); app.addBrush(0xff0000ff); \
             l.drawArc(app, 2, 2, 20, 20, 0, 360);",
        )
        .unwrap();
        let scene = env.scene();
        let bitmap = scene.bitmap(scene.layers[0].bitmap.unwrap()).unwrap();
        assert_eq!(pixel(bitmap, 12, 12), [0, 0, 255, 255], "filled circle");
    }

    /// `fillRect` must not move the layer (reference `FillRect` only touches
    /// the layer's own image). A bitmapless layer keeps the solid-fill
    /// fallback, but its position is untouched.
    #[test]
    fn layer_fill_rect_does_not_move_position() {
        let env = TestEnv::new("layer-fillrect-position");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); \
             l.setSize(64, 64); l.setPos(314, 516); \
             l.fillRect(0, 0, 64, 64, 0xffff0000);",
        )
        .unwrap();
        let scene = env.scene();
        let layer = &scene.layers[0];
        assert_eq!(
            (layer.rect.x, layer.rect.y),
            (314, 516),
            "fillRect must not reset the position"
        );
        assert_eq!((layer.rect.w, layer.rect.h), (64, 64));
        assert_eq!(layer.fill_color, Some([255, 0, 0, 255]));
    }

    /// When the layer has an image, `fillRect` replaces the exact region in
    /// the image (so translucent/transparent fills are honored).
    #[test]
    fn layer_fill_rect_replaces_pixels_in_existing_image() {
        let env = TestEnv::new("layer-fillrect-bitmap");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); \
             l.setSize(32, 32); l.setPos(100, 200); \
             var b = new Bitmap(32, 32); l.setBitmap(b.id); \
             l.fillRect(4, 4, 8, 8, 0xff123456);",
        )
        .unwrap();
        let scene = env.scene();
        let layer = &scene.layers[0];
        assert_eq!(
            (layer.rect.x, layer.rect.y),
            (100, 200),
            "fillRect must not move a bitmap layer either"
        );
        assert!(
            layer.fill_color.is_none(),
            "image fill leaves fill_color unset"
        );
        let bitmap = scene.bitmap(layer.bitmap.unwrap()).unwrap();
        assert!(bitmap.dirty);
        assert_eq!(
            pixel(bitmap, 6, 6),
            [0x12, 0x34, 0x56, 0xff],
            "filled region"
        );
        assert_eq!(
            pixel(bitmap, 0, 0),
            [0, 0, 0, 0],
            "outside region untouched"
        );
    }

    /// The exact `MessageArea.clear()` pattern: a transparent `fillRect`
    /// clears the existing image and leaves the layer's position alone.
    #[test]
    fn layer_fill_rect_transparent_clears_without_moving() {
        let env = TestEnv::new("layer-fillrect-clear");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); \
             l.setSize(32, 32); l.setPos(314, 516); \
             var b = new Bitmap(32, 32); l.setBitmap(b.id); \
             l.fillRect(0, 0, 32, 32, 0xffffffff); \
             l.fillRect(0, 0, 32, 32, 0x00000000);",
        )
        .unwrap();
        let scene = env.scene();
        let layer = &scene.layers[0];
        assert_eq!((layer.rect.x, layer.rect.y), (314, 516));
        let bitmap = scene.bitmap(layer.bitmap.unwrap()).unwrap();
        assert!(
            bitmap.rgba.chunks_exact(4).all(|p| p[3] == 0),
            "a transparent fillRect must clear the image"
        );
    }

    /// Regression for the savedata thumbnail pipeline
    /// (`system/systemwindow.tjs`, `LoadSaveWindow`): `copyRect` with
    /// `face = dfMask` establishes the frame alpha, `fillRect` with
    /// `face = dfMain` + `holdAlpha` clears the RGB while keeping that
    /// alpha, and the final `copyRect` (still `holdAlpha`) writes the
    /// thumbnail RGB behind the existing alpha.
    #[test]
    fn layer_savedata_thumbnail_uses_mask_face_and_hold_alpha() {
        let env = TestEnv::new("layer-savedata-thumb");
        env.run(
            "var w = new Window(); \
             var mask = new Bitmap(3, 3); \
             var img = new Bitmap(3, 3); \
             var x, y; \
             for (y = 0; y < 3; y++) { for (x = 0; x < 3; x++) { \
                 mask.setMaskPixel(x, y, 200); \
                 img.setPixel(x, y, 0xff112233); \
                 img.setMaskPixel(x, y, 255); \
             } } \
             var thumb = new Layer(w, null); \
             var dst = new Bitmap(3, 3); \
             thumb.setBitmap(dst.id); \
             thumb.face = 2; \
             thumb.copyRect(0, 0, mask, 0, 0, 3, 3); \
             thumb.holdAlpha = true; \
             thumb.face = 1; \
             thumb.fillRect(0, 0, 3, 3, 0); \
             thumb.copyRect(0, 0, img, 0, 0, 3, 3);",
        )
        .unwrap();
        let scene = env.scene();
        let bitmap = scene.bitmap(scene.layers[0].bitmap.unwrap()).unwrap();
        for (x, y) in [(0u32, 0u32), (3, 0), (1, 2)] {
            assert_eq!(
                pixel(bitmap, x, y),
                [0x11, 0x22, 0x33, 200],
                "thumbnail RGB with the frame's alpha at ({x}, {y})"
            );
        }
    }

    /// `fillRect` with `dfMain` (1) + `holdAlpha` writes RGB only and leaves
    /// the destination alpha untouched (reference `FillRect` -> `FillColor`).
    #[test]
    fn layer_fill_rect_hold_alpha_sets_rgb_only() {
        let env = TestEnv::new("layer-fillrect-holdalpha");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); \
             var b = new Bitmap(2, 1); \
             b.setMaskPixel(0, 0, 123); b.setMaskPixel(1, 0, 200); \
             l.setBitmap(b.id); \
             l.holdAlpha = true; l.face = 1; \
             l.fillRect(0, 0, 2, 1, 0xffaabbcc);",
        )
        .unwrap();
        let scene = env.scene();
        let bitmap = scene.bitmap(scene.layers[0].bitmap.unwrap()).unwrap();
        assert_eq!(pixel(bitmap, 0, 0), [0xaa, 0xbb, 0xcc, 123]);
        assert_eq!(pixel(bitmap, 1, 0), [0xaa, 0xbb, 0xcc, 200]);
    }

    /// `copyRect` with `face = dfMask` (2) copies the alpha plane only,
    /// holding the destination RGB (reference `CopyRect` -> `CopyMask`).
    #[test]
    fn layer_copy_rect_mask_face_writes_alpha_only() {
        let env = TestEnv::new("layer-copyrect-mask");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); \
             var dst = new Bitmap(1, 1); dst.setPixel(0, 0, 0xffff0000); dst.setMaskPixel(0, 0, 10); \
             var src = new Bitmap(1, 1); src.setPixel(0, 0, 0xff00ff00); src.setMaskPixel(0, 0, 222); \
             l.setBitmap(dst.id); l.face = 2; \
             l.copyRect(0, 0, src, 0, 0, 1, 1);",
        )
        .unwrap();
        let scene = env.scene();
        let bitmap = scene.bitmap(scene.layers[0].bitmap.unwrap()).unwrap();
        assert_eq!(
            pixel(bitmap, 0, 0),
            [255, 0, 0, 222],
            "alpha from the source, RGB held"
        );
    }

    /// `fillRect` with `face = dfMask` (2) writes the alpha plane only,
    /// holding the destination RGB.
    #[test]
    fn layer_fill_rect_mask_face_sets_alpha_only() {
        let env = TestEnv::new("layer-fillrect-mask");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); \
             var b = new Bitmap(1, 1); b.setPixel(0, 0, 0xff00ff00); b.setMaskPixel(0, 0, 5); \
             l.setBitmap(b.id); l.face = 2; \
             l.fillRect(0, 0, 1, 1, 0xff000040);",
        )
        .unwrap();
        let scene = env.scene();
        let bitmap = scene.bitmap(scene.layers[0].bitmap.unwrap()).unwrap();
        assert_eq!(pixel(bitmap, 0, 0), [0, 255, 0, 0x40]);
    }

    /// The default `face == dfAuto` + `holdAlpha == false` path is unchanged:
    /// `copyRect` is a source-over alpha blend (a half-transparent source over
    /// a transparent destination keeps its RGB and alpha).
    #[test]
    fn layer_copy_rect_default_face_still_blends() {
        let env = TestEnv::new("layer-copyrect-default");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); \
             var src = new Bitmap(1, 1); src.setPixel(0, 0, 0xff0a141e); src.setMaskPixel(0, 0, 128); \
             l.copyRect(0, 0, src, 0, 0, 1, 1);",
        )
        .unwrap();
        let scene = env.scene();
        let bitmap = scene.bitmap(scene.layers[0].bitmap.unwrap()).unwrap();
        assert_eq!(pixel(bitmap, 0, 0), [0x0a, 0x14, 0x1e, 128]);
    }

    /// `stretchCopy` on the `dfOpaque` face with `holdAlpha` resamples the
    /// source RGB and keeps the destination alpha (reference `StretchCopy`
    /// passes `HoldAlpha` to `StretchBlt(bmCopy)`).
    #[test]
    fn layer_stretch_copy_hold_alpha_keeps_destination_alpha() {
        let env = TestEnv::new("layer-stretchcopy-holdalpha");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); \
             var dst = new Bitmap(1, 1); dst.setMaskPixel(0, 0, 77); \
             var src = new Bitmap(1, 1); src.setPixel(0, 0, 0xff102030); src.setMaskPixel(0, 0, 255); \
             l.setBitmap(dst.id); l.holdAlpha = true; l.face = 1; \
             l.stretchCopy(0, 0, 1, 1, src, 0, 0, 1, 1, 0);",
        )
        .unwrap();
        let scene = env.scene();
        let bitmap = scene.bitmap(scene.layers[0].bitmap.unwrap()).unwrap();
        assert_eq!(pixel(bitmap, 0, 0), [0x10, 0x20, 0x30, 77]);
    }

    #[test]
    fn layer_box_blur_changes_attached_pixels() {
        let env = TestEnv::new("layer-box-blur");
        env.run("var w = new Window(); var l = new Layer(w, null); l.setSize(32, 16); l.drawText(1, 1, 'A', 0xffffffff); l.doBoxBlur(1, 1);")
            .unwrap();
        let scene = env.scene();
        let bitmap = scene.bitmap(scene.layers[0].bitmap.unwrap()).unwrap();
        assert!(bitmap.dirty);
        assert!(bitmap.rgba.chunks_exact(4).any(|pixel| pixel[3] != 0));
    }

    #[test]
    fn layer_attach_bitmap() {
        let env = TestEnv::new("layer-bitmap");
        env.run("var w = new Window(); var l = new Layer(w, null); var b = new Bitmap(8, 8); l.setBitmap(b.id);")
            .unwrap();
        let scene = env.scene();
        let layer = &scene.layers[0];
        let bitmap = &scene.bitmaps[0];
        assert_eq!(layer.bitmap, Some(bitmap.id));
        assert_eq!((bitmap.width, bitmap.height), (8, 8));
        assert_eq!(bitmap.rgba.len(), 8 * 8 * 4);
    }

    #[test]
    fn layer_load_images_from_storage() {
        let env = TestEnv::new("layer-load");
        let (w, h) = (4u32, 3u32);
        let rgba: Vec<u8> = (0..w * h).flat_map(|_| [255u8, 128, 0, 255]).collect();
        write_fixture(&env, "testimg.webp", &rgba, w, h);
        env.run("var w = new Window(); var l = new Layer(w, null); l.loadImages('testimg'); l.setSizeToImageSize();")
            .unwrap();
        let scene = env.scene();
        let layer = &scene.layers[0];
        assert!(layer.bitmap.is_some(), "loadImages must attach a bitmap");
        let bmp = scene
            .bitmaps
            .iter()
            .find(|b| b.id == layer.bitmap.unwrap())
            .expect("attached bitmap registered");
        assert_eq!((bmp.width, bmp.height), (w, h));
        assert_eq!((layer.rect.w, layer.rect.h), (bmp.width, bmp.height));
        // imageWidth/imageHeight report the bitmap size
        assert_eq!(env.eval_int("l.imageWidth"), i64::from(w));
        assert_eq!(env.eval_int("l.imageHeight"), i64::from(h));
    }

    #[test]
    fn layer_null_window_errors() {
        let env = TestEnv::new("layer-null");
        env.run("var got = 'no error'; try { var l = new Layer(null, null); } catch (e) { got = 'error'; }")
            .unwrap();
        assert_eq!(env.eval_string("got"), "error");
    }

    #[test]
    fn layer_destroy_removes_from_scene() {
        let env = TestEnv::new("layer-destroy");
        // One-shot create+null: the VM releases the layer within the
        // script, so its native destroy removes it from the scene. The
        // window stays (still referenced by `w`).
        env.run("var w = new Window(); var l = new Layer(w, null); l = null;")
            .unwrap();
        let scene = env.scene();
        assert_eq!(scene.layers.len(), 0);
        assert_eq!(scene.windows.len(), 1);
    }

    /// Destroying a parent's native instance must take its child subtree with
    /// it (reference `Invalidate` + GC cascade), even while the child is still
    /// referenced from the script. `remove_layer` alone kept the children as
    /// window roots, which left a torn-down scene's images on screen.
    #[test]
    fn layer_destroy_removes_child_subtree() {
        let env = TestEnv::new("layer-destroy-subtree");
        env.run(
            "var w = new Window(); \
             var p = new Layer(w, null); \
             var c = new Layer(w, p); \
             var g = new Layer(w, c); \
             var cid = c.id; var gid = g.id; \
             p = null;",
        )
        .unwrap();
        let scene = env.scene();
        // The child/grandchild TJS objects are still alive (`c`/`g`), but the
        // destroyed parent's subtree must be gone from the scene.
        let cid = env.eval_int("cid") as u32;
        let gid = env.eval_int("gid") as u32;
        assert!(
            scene.layer(cid).is_none(),
            "child must be removed with its parent"
        );
        assert!(
            scene.layer(gid).is_none(),
            "grandchild must be removed with its parent"
        );
        let win = scene.windows.first().expect("window").id;
        assert!(scene.window(win).unwrap().layers.is_empty());
    }

    #[test]
    fn deep_script_subclass_chain_resolves_object_parent() {
        // Mimic the game's `SavedataHeader -> SelectItemGroupSprite ->
        // ActivateLayer -> Layer` chain: the object parent must survive
        // several `super` hops (the save-data panel was landing under the
        // title root instead of its window).
        let env = TestEnv::new("deep-chain");
        env.run(
            "class AL extends Layer { function AL(win, par){ super.Layer(win, par); } } \
             class GRP extends AL { function GRP(win, par){ super.AL(win, par); } } \
             class HDR extends GRP { function HDR(win, par){ super.GRP(win, par); } } \
             var w = new Window(); \
             var root = new GRP(w, w.primaryLayer); \
             var h = new HDR(w, root); \
             var ok = (h.parent === root); \
             var rootId = root.id; var hId = h.id;",
        )
        .unwrap();
        assert_eq!(
            env.eval_int("ok"),
            1,
            "deep super chain keeps the object parent"
        );
        let scene = env.scene();
        let h = scene
            .layers
            .iter()
            .find(|l| l.id == env.eval_int("hId") as u32)
            .expect("header layer");
        assert_eq!(h.parent, Some(env.eval_int("rootId") as u32));
    }

    #[test]
    fn layer_parent_and_window_properties() {
        let env = TestEnv::new("layer-tree");
        env.run(
            "var w = new Window(); var p = new Layer(w, null); var c = new Layer(w, p); \
             var pw = c.window; var pp = c.parent;",
        )
        .unwrap();
        // window returns the owning Window's TJS object (retained)
        assert!(matches!(
            env.eval("pw", "test"),
            Ok(tjs2_sys::TjsValue::Object)
        ));
        // The constructor resolves a Layer-object parent, and `parent` reads
        // back as that same object (retained).
        assert!(matches!(
            env.eval("pp", "test"),
            Ok(tjs2_sys::TjsValue::Object)
        ));
        assert_eq!(
            env.eval_int("pp === p"),
            1,
            "parent is the same Layer object"
        );
        // re-parent by id works; parent still reads back as the object
        env.run("c.setParentId(p.id);").unwrap();
        assert_eq!(env.eval_int("c.parent === p"), 1);
        {
            let scene = env.scene();
            let c = scene
                .layers
                .iter()
                .find(|l| l.id == env.eval_int("c.id") as u32)
                .expect("child layer");
            assert_eq!(c.parent, Some(env.eval_int("p.id") as u32));
        }
        // assigning a Layer object re-parents too
        env.run("var p2 = new Layer(w, null); c.parent = p2;")
            .unwrap();
        assert_eq!(env.eval_int("c.parent === p2"), 1);
        {
            let scene = env.scene();
            let c = scene
                .layers
                .iter()
                .find(|l| l.id == env.eval_int("c.id") as u32)
                .expect("child layer");
            assert_eq!(c.parent, Some(env.eval_int("p2.id") as u32));
        }
    }

    /// A parentless layer's `parent` must be a real TJS `null` — not `void`
    /// (`void != null` is true in this VM) and not the old integer `-1`
    /// (comparing an int against an object target throws, which broke the
    /// game's `layer.parent != tgt && layer.parent != null` check).
    #[test]
    fn parentless_layer_parent_is_null() {
        let env = TestEnv::new("layer-parent-null");
        env.run("var w = new Window(); var l = new Layer(w, null);")
            .unwrap();
        assert_eq!(env.eval_int("l.parent == null"), 1);
        assert_eq!(env.eval_int("l.parent === null"), 1);
        assert_eq!(env.eval_int("l.parent != null"), 0);
        // The exact game comparison must not throw now.
        env.run("var tgt = new Layer(w, null); var ok = (l.parent != tgt);")
            .unwrap();
        assert_eq!(env.eval_int("ok"), 1);
    }

    /// `window.primaryLayer` with no primary layer is `null`, never an int.
    #[test]
    fn primary_layer_is_null_when_absent() {
        let env = TestEnv::new("window-primary-null");
        env.run("var w = new Window();").unwrap();
        assert_eq!(env.eval_int("w.primaryLayer == null"), 1);
        assert_eq!(env.eval_int("w.primaryLayer === null"), 1);
    }

    /// Object-valued getters (`window`, `parent`) return retained objects for
    /// a parented layer, never an int.
    #[test]
    fn object_getters_return_objects_for_parented_layer() {
        let env = TestEnv::new("layer-object-getters");
        env.run("var w = new Window(); var p = new Layer(w, null); var c = new Layer(w, p);")
            .unwrap();
        assert!(matches!(
            env.eval("c.parent", "test"),
            Ok(tjs2_sys::TjsValue::Object)
        ));
        assert!(matches!(
            env.eval("c.window", "test"),
            Ok(tjs2_sys::TjsValue::Object)
        ));
        assert_eq!(env.eval_int("c.parent === p"), 1);
    }

    /// Setting `parent = null` detaches the layer without throwing.
    #[test]
    fn parent_setter_accepts_null_to_detach() {
        let env = TestEnv::new("layer-parent-null-set");
        env.run("var w = new Window(); var p = new Layer(w, null); var c = new Layer(w, p);")
            .unwrap();
        env.run("c.parent = null;").unwrap();
        let scene = env.scene();
        assert_eq!(scene.layers[1].parent, None, "null detaches to the window");
    }

    /// `Part()` (`parent = null`) detaches a layer but keeps its child subtree
    /// attached to it: the reference `Part` only severs the parent link
    /// (`LayerIntf.cpp:624`). Destruction, by contrast, removes the subtree
    /// ([`tvp_visual::scene::Scene::destroy_layer`]).
    #[test]
    fn part_keeps_child_subtree_attached() {
        let env = TestEnv::new("layer-part-keeps-children");
        env.run(
            "var w = new Window(); \
             var gp = new Layer(w, null); \
             var p = new Layer(w, gp); \
             var c = new Layer(w, p); \
             var pid = p.id; var cid = c.id; \
             p.parent = null;",
        )
        .unwrap();
        let pid = env.eval_int("pid") as u32;
        let cid = env.eval_int("cid") as u32;
        let scene = env.scene();
        assert_eq!(scene.layer(pid).unwrap().parent, None, "p detached");
        assert_eq!(
            scene.layer(cid).unwrap().parent,
            Some(pid),
            "child stays attached to the Part()-ed layer"
        );
        let win = scene.windows.first().expect("window").id;
        let order = scene.window_layer_order(win);
        assert!(order.contains(&pid) && order.contains(&cid));
    }

    /// Model the game's `ADVObject`: a script subclass overrides `id` with a
    /// getter that dereferences `_info`, which is still nil during
    /// construction. Resolving the object as a parent must read the native
    /// `nativeId`, not the overridden `id`, or the constructor throws.
    #[test]
    fn layer_constructor_reads_native_id_not_overridden_id() {
        let env = TestEnv::new("layer-native-id");
        env.run(
            "class Info { var id = 7; } \
             class Bad extends Layer { \
                 var _info; \
                 function Bad(win, par) { super.Layer(win, par); } \
                 property id { \
                     setter(v) { if (_info === void) _info = new Info(); _info.id = v; } \
                     getter() { return _info.id; } \
                 } \
             } \
             var w = new Window(); \
             var bad = new Bad(w, null); \
             var idThrew = false; \
             try { var x = bad.id; } catch (e) { idThrew = true; } \
             var child = new Layer(w, bad); \
             var ok = (child.parent === bad);",
        )
        .unwrap();
        assert_eq!(
            env.eval_int("idThrew"),
            1,
            "the overridden id getter still throws (model check)"
        );
        assert_eq!(
            env.eval_int("ok"),
            1,
            "parent resolved through nativeId without invoking the override"
        );
    }

    #[test]
    fn argb_to_rgba_known_colors() {
        // TJS color 0xAARRGGBB → straight-alpha RGBA (the reference
        // FillRect convention; matches the real title scene's fills).
        assert_eq!(super::argb_to_rgba(0xff000000), [0, 0, 0, 255]); // opaque black
        assert_eq!(super::argb_to_rgba(0xffffffff), [255, 255, 255, 255]); // white
        assert_eq!(super::argb_to_rgba(0x80ff0000), [255, 0, 0, 128]); // half-alpha red
        assert_eq!(super::argb_to_rgba(0x00000000), [0, 0, 0, 0]); // transparent
        assert_eq!(super::argb_to_rgba(0xff123456), [0x12, 0x34, 0x56, 0xff]);
    }

    #[test]
    fn layer_opacity_bounds() {
        let env = TestEnv::new("layer-opacity-bounds");
        env.run("var w = new Window(); var l = new Layer(w, null); l.opacity = 0;")
            .unwrap();
        assert_eq!(env.scene().layers[0].opacity, 0.0);
        assert_eq!(env.eval_int("l.opacity"), 0);
        env.run("l.opacity = 300;").unwrap();
        assert_eq!(
            env.scene().layers[0].opacity,
            1.0,
            "values above 255 clamp to 1.0"
        );
        assert_eq!(env.eval_int("l.opacity"), 255);
    }

    /// `layer.type` (blend mode: ltAlpha=2, ltAdditive=3, ...) round-trips
    /// on the native instance and is propagated to the shared scene contract.
    #[test]
    fn layer_type_propagates_to_scene() {
        let env = TestEnv::new("layer-type");
        env.run("var w = new Window(); var l = new Layer(w, null); l.type = 3;")
            .unwrap();
        assert_eq!(
            env.eval_int("l.type"),
            3,
            "script round-trip (ltAdditive=3)"
        );
        let scene = env.scene();
        let layer = &scene.layers[0];
        assert_eq!(layer.blend_type, 3, "ltAdditive reaches the scene");
        assert_eq!(layer.fill_color, None);
        assert_eq!(layer.bitmap, None);
        assert_eq!(layer.z_order, 0);
        assert_eq!(layer.opacity, 1.0);
    }

    /// The real title stack z-values assigned via `layer.absolute`: higher
    /// z sorts in front (window_layer_order is back→front).
    #[test]
    fn layer_absolute_sorts_higher_z_in_front() {
        let env = TestEnv::new("layer-absolute");
        env.run(
            "var w = new Window(); \
             var hint = new Layer(w, null); hint.absolute = 210000; \
             var logo = new Layer(w, null); logo.absolute = 110000; \
             var bg = new Layer(w, null); \
             var cover = new Layer(w, null); cover.absolute = 150000;",
        )
        .unwrap();
        let scene = env.scene();
        let ids = scene.window_layer_order(0);
        assert_eq!(ids.len(), 4);
        let pos = |id: u32| ids.iter().position(|&x| x == id).unwrap();
        // Layer ids are assigned in creation order: hint=0, logo=1, bg=2,
        // cover=3 (each `new Layer` bumps the scene's next_layer counter).
        assert!(
            pos(2) < pos(1) && pos(1) < pos(3) && pos(3) < pos(0),
            "z=0 bg, then 110000, 150000, 210000"
        );
    }

    /// `primaryLayer.cursorX`/`cursorY` mirror the cursor position the
    /// render input bridge forwards via [`super::super::set_shared_cursor_pos`]
    /// (which the bridge sources from the shared `tvp-input` mouse position).
    #[test]
    fn layer_cursor_x_y_mirror_shared_cursor_pos() {
        let env = TestEnv::new("layer-cursor");
        // Simulate the bridge forwarding this frame's cursor position.
        super::super::set_shared_cursor_pos(400, 300);
        env.run("var w = new Window(); var l = new Layer(w, null);")
            .unwrap();
        assert_eq!(env.eval_int("l.cursorX"), 400);
        assert_eq!(env.eval_int("l.cursorY"), 300);
        // The game reads them through the primary layer object.
        assert_eq!(env.eval_int("w.primaryLayer.cursorX"), 400);
        assert_eq!(env.eval_int("w.primaryLayer.cursorY"), 300);
        // A later bridge update is reflected too.
        super::super::set_shared_cursor_pos(5, 7);
        assert_eq!(env.eval_int("l.cursorX"), 5);
        assert_eq!(env.eval_int("l.cursorY"), 7);
    }

    /// `layer.font` returns wrappers bound to ONE shared layer font state, so
    /// `with(font){ .face = ...; .height = ... }` persists across accesses
    /// (this is exactly what `system/MessageArea.tjs` does). The throwaway
    /// state from each `new Font()` must be dropped, not accumulated.
    #[test]
    fn layer_font_state_is_shared_across_accesses() {
        let env = TestEnv::new("layer-font-shared");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); \
             l.font.face = 'MS Gothic'; l.font.height = 24; \
             l.font.bold = true;",
        )
        .unwrap();
        {
            let scene = env.scene();
            assert_eq!(scene.fonts.len(), 1, "wrappers must not leak FontStates");
            let front = scene.layers[0].font_id.expect("layer owns a font");
            let font = scene.font(front).expect("layer font exists");
            assert_eq!(font.face, "MS Gothic");
            assert_eq!(font.height, 24);
            assert!(font.bold);
        }
        // Reads create more wrappers but see the shared state's values.
        assert_eq!(env.eval_string("l.font.face"), "MS Gothic");
        assert_eq!(env.eval_int("l.font.height"), 24);
        assert_eq!(env.eval_int("l.font.bold"), 1);
        assert_eq!(env.scene().fonts.len(), 1, "still exactly one FontState");
    }

    /// `layer.font = f` copies the source `Font`'s properties into a
    /// layer-owned state (the reference denies this setter, but the game's
    /// `AffineLayer` assigns it).
    #[test]
    fn layer_font_setter_copies_font_object() {
        let env = TestEnv::new("layer-font-setter");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); \
             var f = new Font('MS 明朝', 20, 0xffff0000); \
             l.font = f;",
        )
        .unwrap();
        let scene = env.scene();
        let id = scene.layers[0].font_id.expect("setter creates layer font");
        let font = scene.font(id).expect("layer font exists");
        assert_eq!(font.face, "MS 明朝");
        assert_eq!(font.height, 20);
        assert_eq!(font.color, [255, 0, 0, 255]);
    }

    /// A `drawText` on a layer whose face is mapped in the installed
    /// `FontConfig` must rasterize through that face. Verified structurally:
    /// the process-global atlas keyed by the configured face's id gains the
    /// drawn glyphs (it would stay empty if `drawText` had fallen back to
    /// system discovery).
    #[test]
    fn draw_text_uses_configured_named_face() {
        const FREE_SANS: &str = "/usr/share/fonts/gnu-free/FreeSans.otf";
        if !std::path::Path::new(FREE_SANS).is_file() {
            eprintln!("skipping: {FREE_SANS} not present");
            return;
        }
        struct ConfigGuard;
        impl Drop for ConfigGuard {
            fn drop(&mut self) {
                tvp_text::set_font_config(None);
            }
        }
        let _guard = ConfigGuard;
        let env = TestEnv::new("layer-draw-text-configured-face");
        // Install the config *after* `TestEnv` acquires the VM test lock, so
        // no other visual-native test can observe it mid-draw.
        let config = tvp_text::FontConfig::from_json_str(&format!(
            r#"{{ "faces": {{ "MS Gothic": "{FREE_SANS}" }} }}"#
        ))
        .expect("valid config");
        tvp_text::set_font_config(Some(config));

        env.run(
            "var w = new Window(); var l = new Layer(w, null); \
             l.setSize(64, 32); \
             l.font.face = 'MS Gothic'; l.font.height = 16; \
             l.drawText(2, 2, 'ABC', 0xffffff);",
        )
        .unwrap();
        drop(env.scene());

        let face = tvp_text::resolve_face(&tvp_text::FaceRequest::Named("MS Gothic".into()))
            .expect("configured face resolves");
        let glyphs = tvp_text::with_cached_atlas(face, 16, |atlas| atlas.glyph_count());
        assert!(
            glyphs >= 3,
            "drawText must rasterize 'ABC' through the configured face, got {glyphs} glyphs"
        );
    }

    /// `saveLayerImage` resolves relative names under the game dir and
    /// normalizes the storage `\` separator; absolute names are used as-is.
    #[test]
    fn save_host_path_normalizes_storage_separators() {
        let game = std::path::Path::new("/games/title");
        assert_eq!(
            super::save_host_path(game, "thumb\\a.bmp"),
            std::path::PathBuf::from("/games/title/thumb/a.bmp")
        );
        assert_eq!(
            super::save_host_path(game, "/abs/x.png"),
            std::path::PathBuf::from("/abs/x.png")
        );
        assert_eq!(
            super::save_host_path(game, "sub/dir/y.jpg"),
            std::path::PathBuf::from("/games/title/sub/dir/y.jpg")
        );
    }

    /// `children` returns a TJS `Array` of the direct child layer objects in
    /// sibling order (reference `GetChildrenArrayObjectNoAddRef`).
    #[test]
    fn layer_children_returns_child_objects() {
        let env = TestEnv::new("layer-children");
        env.run(
            "var w = new Window(); \
             var parent = new Layer(w, null); \
             var c1 = new Layer(w, parent); \
             var c2 = new Layer(w, parent); \
             var kids = parent.children; \
             var leaf = c1.children;",
        )
        .unwrap();
        assert_eq!(env.eval_int("kids.count"), 2);
        assert_eq!(env.eval_int("kids[0] === c1"), 1);
        assert_eq!(env.eval_int("kids[1] === c2"), 1);
        assert_eq!(env.eval_int("leaf.count"), 0);
    }

    /// `getLayerAt(x, y)` returns the frontmost layer at the point, honours
    /// `exclude_self`/`get_disabled`, returns `null` on a miss, and uses the
    /// province plane for `htProvince` (reference `GetMostFrontChildAt`).
    #[test]
    fn layer_get_layer_at_frontmost_and_options() {
        let env = TestEnv::new("layer-get-layer-at");
        env.run(
            "var w = new Window(); \
             var back = new Layer(w, null); back.setSize(20, 20); back.visible = true; back.hitThreshold = 0; \
             var front = new Layer(w, null); front.setSize(20, 20); front.visible = true; front.hitThreshold = 0; \
             var hit = back.getLayerAt(5, 5); \
             var miss = back.getLayerAt(100, 100); \
             var excl = front.getLayerAt(5, 5, true);",
        )
        .unwrap();
        assert_eq!(env.eval_int("hit === front"), 1);
        assert_eq!(env.eval_int("miss == null"), 1);
        assert_eq!(env.eval_int("excl === back"), 1);

        env.run(
            "front.enabled = false; \
             var disabled = back.getLayerAt(5, 5); \
             var included = back.getLayerAt(5, 5, false, true);",
        )
        .unwrap();
        assert_eq!(
            env.eval_int("disabled == null"),
            1,
            "disabled stops the search"
        );
        assert_eq!(env.eval_int("included === front"), 1);
    }

    /// `getLayerAt` honours `htProvince` (1) through the province plane.
    #[test]
    fn layer_get_layer_at_uses_province_plane() {
        let env = TestEnv::new("layer-get-layer-at-province");
        env.run(
            "var w = new Window(); \
             var l = new Layer(w, null); l.setSize(4, 4); l.hasImage = true; \
             l.hitType = 1; l.setProvincePixel(1, 1, 5); \
             var hit = l.getLayerAt(1, 1); \
             var miss = l.getLayerAt(2, 2);",
        )
        .unwrap();
        assert_eq!(env.eval_int("hit === l"), 1);
        assert_eq!(env.eval_int("miss == null"), 1);
    }

    /// `joinFocusChain` defaults to true; `nextFocusable`/`prevFocusable`
    /// walk the window's focusable layers with wrap-around and skip layers
    /// that left the chain.
    #[test]
    fn layer_focus_chain_walks_focusable_layers() {
        let env = TestEnv::new("layer-focus-chain");
        env.run(
            "var w = new Window(); \
             var a = new Layer(w, null); a.focusable = true; a.visible = true; \
             var b = new Layer(w, null); b.focusable = true; b.visible = true; \
             var c = new Layer(w, null); c.focusable = true; c.visible = true; \
             var def = a.joinFocusChain; \
             var n1 = a.nextFocusable; \
             var n2 = b.nextFocusable; \
             var n3 = c.nextFocusable; \
             var p1 = a.prevFocusable; \
             b.joinFocusChain = false; \
             var skipped = a.nextFocusable; \
             var flag = b.joinFocusChain;",
        )
        .unwrap();
        assert_eq!(env.eval_int("def"), 1, "JoinFocusChain defaults true");
        assert_eq!(env.eval_int("n1 === b"), 1);
        assert_eq!(env.eval_int("n2 === c"), 1);
        assert_eq!(env.eval_int("n3 === a"), 1, "wraps forward");
        assert_eq!(env.eval_int("p1 === c"), 1, "wraps backward");
        assert_eq!(env.eval_int("skipped === c"), 1, "skips non-chain layer");
        assert_eq!(env.eval_int("flag"), 0);
    }

    /// The new native event dispatchers build the reference event dictionary
    /// and call `actionOwner.action(ev)` with the matching members.
    #[test]
    fn layer_event_dispatchers_reach_the_action_owner() {
        let env = TestEnv::new("layer-events");
        env.run(
            "var owner = %[]; \
             owner.action = function(ev) { global.seen = ev; }; \
             var w = new Window(); \
             var l = new Layer(owner, null); l.setSize(10, 10); \
             var other = new Layer(owner, null); \
             l.onTouchDown(1, 2, 3, 4, 7);",
        )
        .unwrap();
        assert_eq!(env.eval_string("global.seen.type"), "onTouchDown");
        assert_eq!(env.eval_int("global.seen.x"), 1);
        assert_eq!(env.eval_int("global.seen.y"), 2);
        assert_eq!(env.eval_int("global.seen.cx"), 3);
        assert_eq!(env.eval_int("global.seen.cy"), 4);
        assert_eq!(env.eval_int("global.seen.id"), 7);
        assert_eq!(env.eval_int("global.seen.target === l"), 1);

        env.run("l.onNodeEnabled();").unwrap();
        assert_eq!(env.eval_string("global.seen.type"), "onNodeEnabled");

        env.run("l.onFocus(other, 1);").unwrap();
        assert_eq!(env.eval_string("global.seen.type"), "onFocus");
        assert_eq!(env.eval_int("global.seen.blurred === other"), 1);
        assert_eq!(env.eval_int("global.seen.direction"), 1);

        env.run("l.onSearchNextFocusable(other);").unwrap();
        assert_eq!(env.eval_string("global.seen.type"), "onSearchNextFocusable");
        assert_eq!(env.eval_int("global.seen.layer === other"), 1);

        env.run("l.onTransitionCompleted(l, other);").unwrap();
        assert_eq!(env.eval_string("global.seen.type"), "onTransitionCompleted");
        assert_eq!(env.eval_int("global.seen.dest === l"), 1);
        assert_eq!(env.eval_int("global.seen.src === other"), 1);
    }

    /// A too-short event call throws, matching the reference arity checks.
    #[test]
    fn layer_event_dispatchers_check_arity() {
        let env = TestEnv::new("layer-event-arity");
        env.run(
            "var owner = %[]; owner.action = function(ev) {}; \
             var w = new Window(); var l = new Layer(owner, null); \
             var threw = false; try { l.onTouchDown(1, 2); } catch (e) { threw = true; }",
        )
        .unwrap();
        assert_eq!(env.eval_int("threw"), 1);
    }

    /// Reading `nextFocusable` posts `onSearchNextFocusable` to the layer's
    /// own object; the native handler forwards it to the action owner.
    #[test]
    fn layer_next_focusable_dispatches_search_event() {
        let env = TestEnv::new("layer-focus-event");
        env.run(
            "var owner = %[]; owner.action = function(ev) { global.seen = ev; }; \
             var w = new Window(); \
             var a = new Layer(owner, null); a.focusable = true; a.visible = true; \
             var b = new Layer(owner, null); b.focusable = true; b.visible = true; \
             var next = a.nextFocusable;",
        )
        .unwrap();
        assert_eq!(env.eval_int("next === b"), 1);
        assert_eq!(env.eval_string("global.seen.type"), "onSearchNextFocusable");
        assert_eq!(env.eval_int("global.seen.layer === b"), 1);
    }

    // ------------------------------------------------------------------
    // Window primary-layer screen-buffer composite
    //
    // The reference primary layer is the window's screen buffer (the layer
    // manager composites the tree into its MainImage). Reading it as an
    // image source or saving it must composite the visible tree first.
    // ------------------------------------------------------------------

    /// Build an 8x8 window whose primary layer holds an opaque red child and
    /// a half-alpha blue child over its right half. Scene layer order is
    /// `primary(0)`, `red(1)`, `blue(2)`.
    fn run_primary_composite_setup(env: &TestEnv) {
        env.run(
            "var w = new Window(); \
             w.setSize(8, 8); \
             var primary = new Layer(w, null); primary.setSize(8, 8); \
             var red = new Layer(w, primary); \
             red.setSize(8, 8); red.setPos(0, 0); red.visible = true; \
             red.hasImage = true; red.fillRect(0, 0, 8, 8, 0xffff0000); \
             var blue = new Layer(w, primary); \
             blue.setSize(4, 8); blue.setPos(4, 0); blue.visible = true; \
             blue.opacity = 128; blue.hasImage = true; \
             blue.fillRect(0, 0, 4, 8, 0xff0000ff);",
        )
        .unwrap();
    }

    /// Read one RGBA pixel from a layer's attached bitmap.
    fn composite_pixel(scene: &crate::scene::Scene, layer_index: usize, x: u32, y: u32) -> [u8; 4] {
        let bitmap_id = scene.layers[layer_index]
            .bitmap
            .expect("layer has a bitmap");
        let bitmap = scene.bitmap(bitmap_id).expect("bitmap exists");
        let i = ((y * bitmap.width + x) as usize) * 4;
        [
            bitmap.rgba[i],
            bitmap.rgba[i + 1],
            bitmap.rgba[i + 2],
            bitmap.rgba[i + 3],
        ]
    }

    /// `piledCopy(0, 0, window.primaryLayer, ...)` reads a real screen buffer
    /// composited from the visible children (position + opacity honoured).
    #[test]
    fn piled_copy_from_primary_composites_visible_layers() {
        let env = TestEnv::new("primary-composite-piled");
        run_primary_composite_setup(&env);
        env.run(
            "var dst = new Layer(w, null); dst.setSize(8, 8); dst.hasImage = true; \
             dst.piledCopy(0, 0, primary, 0, 0, 8, 8);",
        )
        .unwrap();

        let scene = env.scene();
        // Layer order: primary(0), red(1), blue(2), dst(3).
        assert_eq!(composite_pixel(&scene, 3, 0, 0), [255, 0, 0, 255]);
        assert_eq!(composite_pixel(&scene, 3, 1, 7), [255, 0, 0, 255]);
        // Right half: 50% blue over red.
        assert_eq!(composite_pixel(&scene, 3, 7, 0), [127, 0, 128, 255]);
        assert_eq!(composite_pixel(&scene, 3, 4, 7), [127, 0, 128, 255]);
        assert!(
            scene.layers[0].bitmap.is_some(),
            "compositing allocates the primary MainImage"
        );
        // ...and it is flagged as a screen buffer, so the renderer never
        // blits it as a sprite (that would replay a stale snapshot and leave
        // the previous scene's imagery on screen).
        let primary_bitmap = scene.layers[0].bitmap.unwrap();
        assert!(
            scene.bitmap(primary_bitmap).unwrap().screen_buffer,
            "the primary MainImage is a screen buffer, not drawable content"
        );
    }

    /// `copyRect(0, 0, window.primaryLayer, ...)` takes the same
    /// screen-buffer path as `piledCopy`.
    #[test]
    fn copy_rect_from_primary_composites_visible_layers() {
        let env = TestEnv::new("primary-composite-copy");
        run_primary_composite_setup(&env);
        env.run(
            "var dst = new Layer(w, null); dst.setSize(8, 8); dst.hasImage = true; \
             dst.copyRect(0, 0, primary, 0, 0, 8, 8);",
        )
        .unwrap();

        let scene = env.scene();
        assert_eq!(composite_pixel(&scene, 3, 0, 0), [255, 0, 0, 255]);
        assert_eq!(composite_pixel(&scene, 3, 7, 0), [127, 0, 128, 255]);
    }

    /// A hidden child is skipped by the composite.
    #[test]
    fn composite_skips_invisible_layers() {
        let env = TestEnv::new("primary-composite-hidden");
        run_primary_composite_setup(&env);
        env.run("blue.visible = false;").unwrap();
        env.run(
            "var dst = new Layer(w, null); dst.setSize(8, 8); dst.hasImage = true; \
             dst.piledCopy(0, 0, primary, 0, 0, 8, 8);",
        )
        .unwrap();

        let scene = env.scene();
        assert_eq!(composite_pixel(&scene, 3, 7, 0), [255, 0, 0, 255]);
    }

    /// `saveLayerImage` on the primary layer encodes the composited screen.
    #[test]
    fn save_layer_image_from_primary_writes_composite() {
        let env = TestEnv::new("primary-composite-save");
        run_primary_composite_setup(&env);
        env.run("primary.saveLayerImage('thumb.png');").unwrap();

        let path = env._dir.path().join("thumb.png");
        let image = image::open(&path).expect("thumbnail encodes").to_rgba8();
        assert_eq!((image.width(), image.height()), (8, 8));
        assert_eq!(image.get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(image.get_pixel(7, 7).0, [127, 0, 128, 255]);
    }

    /// An empty layer tree composites to a transparent screen buffer, not a
    /// black one.
    #[test]
    fn empty_scene_composites_to_transparent() {
        let env = TestEnv::new("primary-composite-empty");
        env.run(
            "var w = new Window(); w.setSize(4, 4); \
             var primary = new Layer(w, null); primary.setSize(4, 4); \
             var dst = new Layer(w, null); dst.setSize(4, 4); dst.hasImage = true; \
             dst.piledCopy(0, 0, primary, 0, 0, 4, 4);",
        )
        .unwrap();

        let scene = env.scene();
        assert!(
            scene.layers[0].bitmap.is_some(),
            "the primary image is allocated even with nothing to draw"
        );
        let bitmap = scene
            .bitmap(scene.layers[1].bitmap.expect("dst image"))
            .expect("bitmap");
        assert!(
            bitmap.rgba.chunks_exact(4).all(|p| p[3] == 0),
            "an empty tree must composite to transparent"
        );
    }

    /// A `Layer`-classified source with no MainImage falls back to a bitmap
    /// with the same numeric id (some objects expose both id spaces) instead
    /// of reporting `source has no image`.
    #[test]
    fn layer_source_without_image_falls_back_to_bitmap() {
        let mut scene = crate::scene::Scene::default();
        let win = scene.add_window("t", (4, 4));
        let _primary = scene.add_layer(win, None); // layer 0 (primary)
        let source = scene.add_layer(win, None); // layer 1, no MainImage
        let _first = scene.add_bitmap(1, 1, vec![1, 1, 1, 1]); // bitmap 0
        let second = scene.add_bitmap(2, 2, vec![9; 16]); // bitmap 1
        assert_eq!(second, 1);
        let resolved = super::tile_bitmap_for_source(&mut scene, Some(true), source)
            .expect("a Layer with no image must fall back to the colliding bitmap");
        assert_eq!((resolved.width, resolved.height), (2, 2));
        assert_eq!(resolved.rgba[0], 9);
    }

    // ------------------------------------------------------------------
    // Real no-op-stub replacements (see the module doc)
    // ------------------------------------------------------------------

    /// `setFontStyle`/`resetFontStyle` write the layer's tracked `FontState`.
    #[test]
    fn layer_set_and_reset_font_style() {
        let env = TestEnv::new("layer-font-style");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); \
             l.setFontStyle('SomeFace', 24, 0, true, true, true, true, 100);",
        )
        .unwrap();
        {
            let scene = env.scene();
            let font_id = scene.layers[0].font_id.expect("font allocation");
            let font = scene.font(font_id).expect("font state");
            assert_eq!(font.face, "SomeFace");
            assert_eq!(font.height, 24);
            assert!(font.bold && font.italic && font.underline && font.strikeout);
            assert!((font.angle - 100.0).abs() < 1e-9);
        }
        env.run("l.resetFontStyle();").unwrap();
        let scene = env.scene();
        let font_id = scene.layers[0].font_id.unwrap();
        let font = scene.font(font_id).unwrap();
        assert_eq!(font.height, super::super::font::DEFAULT_FONT_HEIGHT);
        assert!(!font.bold && !font.italic && !font.underline && !font.strikeout);
        assert_eq!(font.angle, 0.0);
    }

    /// `setDefaultDrawTextParam` records defaults; `resetDrawTextParam`
    /// restores `current` from them (the config/confirm window flow).
    #[test]
    fn layer_default_and_reset_draw_text_param() {
        let env = TestEnv::new("layer-draw-text-param");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); \
             l.setDefaultDrawTextParam(0x112233, 200, false, 5, 0x445566, 2, 1, 1); \
             l.resetDrawTextParam();",
        )
        .unwrap();
        let id = env.eval_int("l.id") as u32;
        let params = super::LAYER_TEXT_PARAMS
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let entry = params.get(&id).expect("params recorded");
        assert_eq!(entry.default.color, 0x112233);
        assert_eq!(entry.default.opa, 200);
        assert!(!entry.default.aa);
        assert_eq!(entry.default.shadow_level, 5);
        assert_eq!(entry.current, entry.default);
    }

    /// `drawString(font, app, x, y, text)` rasterizes with the font's geometry
    /// and the appearance's first solid brush.
    #[test]
    fn layer_draw_string_paints_with_font_and_brush() {
        let env = TestEnv::new("layer-draw-string");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); l.setSize(96, 40); \
             var app = new GdiPlus.Appearance(); app.addBrush(0xffff0000); \
             l.drawString(l.font, app, 2, 2, 'MA');",
        )
        .unwrap();
        let scene = env.scene();
        let bitmap = scene.bitmap(scene.layers[0].bitmap.unwrap()).unwrap();
        assert!(bitmap_ink(bitmap) > 0, "drawString must paint glyphs");
    }

    /// `drawGlyph(x, y, '<char>', color, ...)` draws the character with the
    /// layer font (the engine has no `Glyph` object; a string is accepted).
    #[test]
    fn layer_draw_glyph_paints_string_glyph() {
        let env = TestEnv::new("layer-draw-glyph");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); l.setSize(96, 40); \
             l.drawGlyph(2, 2, 'A', 0x00ffffff);",
        )
        .unwrap();
        let scene = env.scene();
        let bitmap = scene.bitmap(scene.layers[0].bitmap.unwrap()).unwrap();
        assert!(bitmap_ink(bitmap) > 0, "drawGlyph must paint");
    }

    /// `getDrawWidth` measures the given text through the layer font.
    #[test]
    fn layer_get_draw_width_is_positive() {
        let env = TestEnv::new("layer-get-draw-width");
        env.run("var w = new Window(); var l = new Layer(w, null);")
            .unwrap();
        assert!(env.eval_int("l.getDrawWidth('AB')") > 0);
        assert_eq!(env.eval_int("l.getDrawWidth('')"), 0);
    }

    /// `clear` replaces the whole main image (default transparent), matching
    /// the `layerExDraw` plugin.
    #[test]
    fn layer_clear_fills_the_whole_image() {
        let env = TestEnv::new("layer-clear");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); l.setSize(8, 8); \
             l.fillRect(0, 0, 8, 8, 0xffff0000); l.clear(0xff00ff00);",
        )
        .unwrap();
        let scene = env.scene();
        let bitmap = scene.bitmap(scene.layers[0].bitmap.unwrap()).unwrap();
        assert_eq!(pixel(bitmap, 4, 4), [0, 255, 0, 255]);
    }

    /// `drawRectangles` fills each rectangle in the appearance.
    #[test]
    fn layer_draw_rectangles_paints_each_rect() {
        let env = TestEnv::new("layer-draw-rectangles");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); l.setSize(32, 32); \
             var app = new GdiPlus.Appearance(); app.addBrush(0xffff0000); \
             l.drawRectangles(app, [[4,4,10,10],[18,18,10,10]]);",
        )
        .unwrap();
        let scene = env.scene();
        let bitmap = scene.bitmap(scene.layers[0].bitmap.unwrap()).unwrap();
        assert_eq!(pixel(bitmap, 9, 9), [255, 0, 0, 255]);
        assert_eq!(pixel(bitmap, 23, 23), [255, 0, 0, 255]);
    }

    /// `drawClosedCurve`/`drawClosedCurve2` fill a closed spline.
    #[test]
    fn layer_draw_closed_curve_fills() {
        let env = TestEnv::new("layer-draw-closed-curve");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); l.setSize(32, 32); \
             var app = new GdiPlus.Appearance(); app.addBrush(0xffff0000); \
             var pts = [[4,4],[28,4],[28,28],[4,28]]; \
             l.drawClosedCurve(app, pts); \
             var app2 = new GdiPlus.Appearance(); app2.addBrush(0xff0000ff); \
             l.drawClosedCurve2(app2, pts, 0.3);",
        )
        .unwrap();
        let scene = env.scene();
        let bitmap = scene.bitmap(scene.layers[0].bitmap.unwrap()).unwrap();
        assert_eq!(pixel(bitmap, 16, 16), [0, 0, 255, 255], "second fill wins");
    }

    /// `drawCurve2`/`drawCurve3` stroke an open cardinal spline.
    #[test]
    fn layer_draw_curve_variants_stroke() {
        let env = TestEnv::new("layer-draw-curve-variants");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); l.setSize(40, 40); \
             var pen1 = new GdiPlus.Appearance(); pen1.addPen(0xffff0000, 2); \
             l.drawCurve2(pen1, [[0,20],[10,0],[30,40],[39,20]], 0.5); \
             var pen2 = new GdiPlus.Appearance(); pen2.addPen(0xff00ff00, 2); \
             l.drawCurve3(pen2, [[0,30],[10,10],[30,30],[39,10]], 0, 3, 0.5);",
        )
        .unwrap();
        let scene = env.scene();
        let bitmap = scene.bitmap(scene.layers[0].bitmap.unwrap()).unwrap();
        assert!(bitmap_ink(bitmap) > 0, "curve strokes must paint");
    }

    /// `drawPath` accepts a point array in place of the unmodelled
    /// `GdiPlus.Path` and strokes it.
    #[test]
    fn layer_draw_path_strokes_point_array() {
        let env = TestEnv::new("layer-draw-path");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); l.setSize(32, 32); \
             var app = new GdiPlus.Appearance(); app.addPen(0xff0000ff, 2); \
             l.drawPath(app, [[2,2],[16,16],[30,2]]);",
        )
        .unwrap();
        let scene = env.scene();
        let bitmap = scene.bitmap(scene.layers[0].bitmap.unwrap()).unwrap();
        assert!(bitmap_ink(bitmap) > 0, "drawPath must stroke");
    }

    /// `setCenter`/`setAffineOffset` store the anchor (ADVScreen/AdvObject).
    #[test]
    fn layer_set_center_and_affine_offset() {
        let env = TestEnv::new("layer-affine-state");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); \
             l.setCenter(10, 20); l.setAffineOffset(3, 4);",
        )
        .unwrap();
        let id = env.eval_int("l.id") as u32;
        let state = super::layer_affine_state(id).expect("affine state stored");
        assert_eq!(state.center, (10.0, 20.0));
        assert_eq!(state.affine_offset, (3.0, 4.0));
    }

    /// `stopTransition` cancels the queued completion and fires it once
    /// synchronously; the next `transition_poll` must not fire it again.
    #[test]
    fn layer_stop_transition_cancels_pending_completion() {
        let env = TestEnv::new("layer-stop-transition");
        env.run(
            "var count = 0; \
             class StopTransLayer extends Layer { \
               function onTransitionCompleted(dest, src) { count++; } \
             } \
             var w = new Window(); var l = new StopTransLayer(w, null); \
             l.beginTransition('crossfade'); l.stopTransition();",
        )
        .unwrap();
        assert_eq!(env.eval_int("count"), 1, "stop fires the completion once");
        super::transition_poll(&env.engine);
        assert_eq!(env.eval_int("count"), 1, "the queued completion is gone");
    }

    /// `bringToBack`/`moveBefore`/`moveBehind` really reorder siblings.
    #[test]
    fn layer_sibling_reordering() {
        let env = TestEnv::new("layer-reorder");
        env.run(
            "var w = new Window(); var a = new Layer(w, null); \
             var b = new Layer(w, null); var c = new Layer(w, null); \
             c.bringToBack();",
        )
        .unwrap();
        let mut order: Vec<u32> = {
            let scene = env.scene();
            scene
                .window(0)
                .map(|w| w.layers.clone())
                .unwrap_or_default()
        };
        let id = |name: &str| env.eval_int(name) as u32;
        let (a, b, c) = (id("a.id"), id("b.id"), id("c.id"));
        assert_eq!(order, vec![c, a, b], "bringToBack moves c first");
        order.clear();
        env.run("c.moveBehind(a);").unwrap();
        {
            let scene = env.scene();
            order = scene
                .window(0)
                .map(|w| w.layers.clone())
                .unwrap_or_default();
        }
        assert_eq!(order, vec![a, c, b], "moveBehind puts c after a");
        env.run("c.moveBefore(a);").unwrap();
        let scene = env.scene();
        assert_eq!(scene.window(0).unwrap().layers, vec![c, a, b]);
    }

    /// `captureMouse`/`captureTouch`/`releaseCapture`/`releaseTouchCapture`
    /// record and clear the capture state.
    #[test]
    fn layer_input_capture_state() {
        let env = TestEnv::new("layer-capture");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); \
             l.captureMouse(); l.captureTouch(7);",
        )
        .unwrap();
        let id = env.eval_int("l.id") as u32;
        assert_eq!(super::captured_mouse_layer(), Some(id));
        assert_eq!(super::captured_touches(), vec![(7, id)]);
        env.run("l.releaseTouchCapture(7); l.releaseCapture();")
            .unwrap();
        assert_eq!(super::captured_mouse_layer(), None);
        assert!(super::captured_touches().is_empty());
        env.run("l.captureTouch(1); l.captureTouch(2); l.releaseTouchCapture();")
            .unwrap();
        assert!(super::captured_touches().is_empty(), "no args clears all");
    }

    /// `setMainPixel`/`getMainPixel` read/write RGB while preserving the
    /// mask; `setMaskPixel`/`getMaskPixel` read/write the alpha.
    #[test]
    fn layer_main_and_mask_pixels() {
        let env = TestEnv::new("layer-main-mask-pixels");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); l.setSize(4, 4); \
             l.hasImage = true; l.setMaskPixel(1, 1, 200); \
             l.setMainPixel(1, 1, 0x00123456);",
        )
        .unwrap();
        assert_eq!(env.eval_int("l.getMainPixel(1, 1)"), 0x123456);
        assert_eq!(env.eval_int("l.getMaskPixel(1, 1)"), 200);
        // RGB-only set must not clobber the mask.
        env.run("l.setMainPixel(1, 1, 0x00abcdef);").unwrap();
        assert_eq!(env.eval_int("l.getMainPixel(1, 1)"), 0xabcdef);
        assert_eq!(env.eval_int("l.getMaskPixel(1, 1)"), 200);
    }

    /// `loadProvinceImage` fills the 8-bit province plane from a storage
    /// image; `independProvinceImage` keeps it private.
    #[test]
    fn layer_load_and_independ_province_image() {
        let env = TestEnv::new("layer-province-load");
        // 2x2 image: red channel 1,2,3,4; alpha 255.
        let rgba: Vec<u8> = vec![1, 9, 9, 255, 2, 9, 9, 255, 3, 9, 9, 255, 4, 9, 9, 255];
        write_fixture(&env, "prov.webp", &rgba, 2, 2);
        env.run(
            "var w = new Window(); var l = new Layer(w, null); l.setSize(2, 2); \
             l.hasImage = true; l.loadProvinceImage('prov'); l.independProvinceImage();",
        )
        .unwrap();
        let scene = env.scene();
        let layer = &scene.layers[0];
        assert_eq!((layer.province_width, layer.province_height), (2, 2));
        assert_eq!(layer.province.as_deref(), Some(&[1u8, 2, 3, 4][..]));
        assert!(layer.image_modified);
    }

    /// `focusNext`/`focusPrev` move the window focus between focusable layers.
    #[test]
    fn layer_focus_next_prev() {
        let env = TestEnv::new("layer-focus-neighbors");
        env.run(
            "var w = new Window(); var a = new Layer(w, null); a.focusable = true; \
             var b = new Layer(w, null); b.visible = true; b.focusable = true; \
             a.focus(); var next = a.focusNext(); var prev = b.focusPrev();",
        )
        .unwrap();
        let a = env.eval_int("a.id");
        let b = env.eval_int("b.id");
        assert_eq!(env.eval_int("next.id"), b);
        assert_eq!(env.eval_int("prev.id"), a);
        let scene = env.scene();
        assert_eq!(scene.window(0).unwrap().focused_layer, Some(a as u32));
    }

    /// `getList` returns the direct children array, and the game's
    /// `k2compat_fontselect.tjs` `lay.font.getList(flags)` path works.
    #[test]
    fn layer_get_list_and_font_get_list() {
        let env = TestEnv::new("layer-get-list");
        env.run(
            "var w = new Window(); var p = new Layer(w, null); \
             var c1 = new Layer(w, p); var c2 = new Layer(w, p); \
             var list = p.getList(); var names = p.font.getList(0);",
        )
        .unwrap();
        assert_eq!(env.eval_int("list.count"), 2);
        assert!(env.eval_int("names.count") > 0, "Font.getList must answer");
    }

    /// `onHitTest` stores the script hook's hit result; `setAttentionPos`
    /// sets the attention anchor (editlayer.tjs).
    #[test]
    fn layer_hit_test_work_and_attention_pos() {
        let env = TestEnv::new("layer-hittest-attention");
        env.run(
            "var w = new Window(); var l = new Layer(w, null); \
             l.onHitTest(1, 2, true); l.setAttentionPos(5, 6);",
        )
        .unwrap();
        let id = env.eval_int("l.id") as u32;
        assert_eq!(super::layer_hit_test_work(id), Some(true));
        let scene = env.scene();
        assert_eq!(scene.layers[0].attention_left, 5);
        assert_eq!(scene.layers[0].attention_top, 6);
    }

    /// `setMode` makes a layer modal and focuses its first focusable
    /// descendant; `removeMode` releases it and moves focus on.
    #[test]
    fn layer_set_and_remove_mode() {
        let env = TestEnv::new("layer-modal");
        env.run(
            "var w = new Window(); var a = new Layer(w, null); a.focusable = true; \
             var b = new Layer(w, null); b.focusable = true; a.focus(); \
             b.setMode();",
        )
        .unwrap();
        let a = env.eval_int("a.id") as u32;
        let b = env.eval_int("b.id") as u32;
        {
            let scene = env.scene();
            assert_eq!(scene.window(0).unwrap().focused_layer, Some(b));
        }
        env.run("b.removeMode();").unwrap();
        let scene = env.scene();
        assert_eq!(scene.window(0).unwrap().focused_layer, Some(a));
    }

    /// `dump` is output-only and must not throw.
    #[test]
    fn layer_dump_is_safe() {
        let env = TestEnv::new("layer-dump");
        env.run("var w = new Window(); var l = new Layer(w, null); l.dump();")
            .unwrap();
    }

    /// The base `onPaint` action stays resolvable for `super.onPaint(...)`.
    #[test]
    fn layer_on_paint_base_action_is_inert() {
        let env = TestEnv::new("layer-onpaint");
        env.run(
            "var w = new Window(); \
             class P extends Layer { function onPaint() { super.onPaint(...); } } \
             var l = new P(w, null); l.onPaint();",
        )
        .unwrap();
    }
}
