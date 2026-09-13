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
//! | `fillRect(x, y, w, h, color)` | solid fill (0xAARRGGBB → RGBA) |
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
//! | `setCursorPos(x,y)` / `focus()` | no-ops (input: later) |
//! | `setCenter(x,y)`, `setAffineOffset(x,y)`, `setImagePos`, `setImageSize` | no-ops (affine: later) |
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
//! | `pileRect`, `piledCopy`, `blendRect`, `convertType`, `setCenter`, `setAffineOffset`, `drawImage*`, `drawGlyph/String/Curve/Pie/Ellipse/Path`, `stopTransition` | no-op stubs (pixel ops: later) |

use std::collections::HashSet;
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
    set_void_out,
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
static PENDING_TRANSITIONS: LazyLock<Mutex<Vec<tjs2_sys::DetachedValue>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

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
        let mut scene = context_scene_mut();
        // The layer's font state is owned by the layer (created lazily by
        // `layer.font`), so destroy it with the layer.
        let font_id = scene.layer(inst.id).and_then(|l| l.font_id);
        if let Some(font_id) = font_id {
            scene.fonts.retain(|f| f.id != font_id);
        }
        scene.remove_layer(inst.id);
    }
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
    let Some(layer) = scene.layer_mut(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    layer.rect.x = arg_i64(&args[0]) as i32;
    layer.rect.y = arg_i64(&args[1]) as i32;
    if args.len() >= 4 {
        layer.rect.w = arg_i64(&args[2]).max(0) as u32;
        layer.rect.h = arg_i64(&args[3]).max(0) as u32;
    }
    set_void_out(out);
    0
}

/// Reference `InternalSetImageSize`: set the drawn image size, keeping the
/// image covering the layer rect (shrinking the layer or shifting the image
/// offset as needed).
fn internal_set_image_size(layer: &mut LayerState, width: u32, height: u32) {
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
    layer.image_width = width;
    layer.image_height = height;
}

/// Reference `ImageLayerSizeChanged`: after the layer rect changes, keep the
/// image at least as large as the layer and its offset such that the layer
/// stays covered.
fn image_layer_size_changed(layer: &mut LayerState) {
    if layer.image_width < layer.rect.w {
        layer.image_width = layer.rect.w;
    }
    if (layer.image_width as i32 + layer.image_left) < layer.rect.w as i32 {
        layer.image_left = layer.rect.w as i32 - layer.image_width as i32;
    }
    if layer.image_height < layer.rect.h {
        layer.image_height = layer.rect.h;
    }
    if (layer.image_height as i32 + layer.image_top) < layer.rect.h as i32 {
        layer.image_top = layer.rect.h as i32 - layer.image_height as i32;
    }
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
    let Some(layer) = scene.layer_mut(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    layer.rect.w = arg_i64(&args[0]).max(0) as u32;
    layer.rect.h = arg_i64(&args[1]).max(0) as u32;
    image_layer_size_changed(layer);
    set_void_out(out);
    0
}

/// `setImagePos(x, y)` — reference `SetImagePosition`: place the image inside
/// the layer (offsets are typically ≤ 0; a sprite sheet uses `-frameW*n`).
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
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(layer) = scene.layer_mut(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    layer.image_left = arg_i64(&args[0]) as i32;
    layer.image_top = arg_i64(&args[1]) as i32;
    set_void_out(out);
    0
}

/// `setImageSize(w, h)` — reference `SetImageSize`.
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
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(layer) = scene.layer_mut(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    internal_set_image_size(
        layer,
        arg_i64(&args[0]).max(0) as u32,
        arg_i64(&args[1]).max(0) as u32,
    );
    set_void_out(out);
    0
}

/// `copyRect(dx, dy, src, sx, sy, sw, sh)` — the game's `Button.create`
/// passes a `Bitmap` object and copies the whole sheet into the layer's main
/// image. Full pixel blitting is not modelled yet; we attach the source
/// bitmap and size the image to the copied region, which renders the same
/// for the full-sheet copy the title UI uses.
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
    let Some(src_arg) = args.get(2) else {
        return error_out(out_error, "Layer.copyRect requires a source image");
    };
    let engine = crate::natives::context_engine();
    let bitmap_id = match resolve_object_id_arg(engine, src_arg) {
        Ok(id) => id,
        Err(e) => return error_out(out_error, &e),
    };
    let inst = unsafe { instance_ref::<LayerInst>(instance) };
    let mut scene = context_scene_mut();
    // `src` may be a `Bitmap` (its id is a bitmap id) or a `Layer` (the
    // reference accepts both: a Layer contributes its main image). Resolve
    // the object id against the scene so a Layer source copies its bitmap.
    let bitmap = if bitmap_id < 0 {
        None
    } else if scene.bitmap(bitmap_id as u32).is_some() {
        Some(bitmap_id as u32)
    } else if let Some(src_layer) = scene.layer(bitmap_id as u32) {
        src_layer.bitmap
    } else {
        return error_out(out_error, "Layer.copyRect: no such bitmap");
    };
    // The copied source region size (args 5/6) if provided, else the bitmap
    // size; the renderer maps `image_width/height` onto the bitmap pixels.
    let Some(layer) = scene.layer_mut(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    layer.bitmap = bitmap;
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
    let bitmap_id = scene.layer(inst.id).and_then(|layer| layer.bitmap);
    match bitmap_id {
        Some(bitmap_id) => {
            if let Some(bitmap) = scene.bitmap_mut(bitmap_id) {
                raster::fill_rect_replace(bitmap, x, y, w, h, color);
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
        internal_set_image_size(layer, dims.0, dims.1);
    }
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
    if let Some((bitmap, image_left, image_top, image_width, image_height, w, h)) = layer_src {
        if let Some(target) = scene.layer_mut(inst.id) {
            target.bitmap = bitmap;
            target.clip = None;
            target.image_left = image_left;
            target.image_top = image_top;
            target.image_width = image_width;
            target.image_height = image_height;
            target.rect.w = w;
            target.rect.h = h;
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
        image_layer_size_changed(layer);
    }
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
    if bitmap_id >= 0 && scene.bitmap(bitmap_id as u32).is_none() {
        return error_out(out_error, "Layer.setBitmap: no bitmap with that id");
    }
    let Some(layer) = scene.layer_mut(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    layer.bitmap = (bitmap_id >= 0).then_some(bitmap_id as u32);
    layer.clip = None;
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
    let Some(layer) = scene.layer_mut(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    layer.bitmap = (bitmap_id >= 0).then_some(bitmap_id as u32);
    layer.clip = None;
    if let Some((w, h)) = dims {
        internal_set_image_size(layer, w, h);
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
layer_int_prop!(
    layer_width_get,
    layer_width_set,
    |l: &LayerState| i64::from(l.rect.w),
    |l: &mut LayerState, v: &Value| l.rect.w = arg_i64(v).max(0) as u32
);
layer_int_prop!(
    layer_height_get,
    layer_height_set,
    |l: &LayerState| i64::from(l.rect.h),
    |l: &mut LayerState, v: &Value| l.rect.h = arg_i64(v).max(0) as u32
);
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
// `copyRect`. The renderer always draws the main image; the value round-trips.
layer_int_prop!(
    layer_face_get,
    layer_face_set,
    |l: &LayerState| i64::from(l.face),
    |l: &mut LayerState, v: &Value| l.face = arg_i64(v) as i32
);
// `holdAlpha` — stored only.
layer_int_prop!(
    layer_hold_alpha_get,
    layer_hold_alpha_set,
    |l: &LayerState| i64::from(l.hold_alpha),
    |l: &mut LayerState, v: &Value| l.hold_alpha = arg_bool(v)
);
// `imageLeft` / `imageTop` — the image's offset inside the layer (reference
// `ImageLeft`/`ImageTop`; negative selects a sprite-sheet frame).
layer_int_prop!(
    layer_image_left_get,
    layer_image_left_set,
    |l: &LayerState| i64::from(l.image_left),
    |l: &mut LayerState, v: &Value| l.image_left = arg_i64(v) as i32
);
layer_int_prop!(
    layer_image_top_get,
    layer_image_top_set,
    |l: &LayerState| i64::from(l.image_top),
    |l: &mut LayerState, v: &Value| l.image_top = arg_i64(v) as i32
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

/// `setCursorPos(x, y)` — no-op (input is beyond milestone 3A).
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

/// `focus()` — no-op (input is beyond milestone 3A).
extern "C" fn layer_focus(
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
        let mut scene = context_scene_mut();
        if let Some(bitmap) = scene.bitmap_mut(bitmap_id) {
            if shadow_level != 0 || shadow_width != 0 {
                paint_prerendered_text(
                    bitmap,
                    &pfont,
                    &text,
                    shadow_color,
                    opa,
                    aa,
                    x.saturating_add(shadow_x),
                    y.saturating_add(shadow_y),
                    font_height,
                    shadow_width.min(16),
                    style,
                );
            }
            paint_prerendered_text(
                bitmap,
                &pfont,
                &text,
                color,
                opa,
                aa,
                x,
                y,
                font_height,
                0,
                style,
            );
            bitmap.mark_dirty();
        }
        set_void_out(out);
        return 0;
    }

    // Prefer tvp-text's real rasterizer. `KRKR_RS_SYSTEM_FONT` is an explicit
    // path override and always wins (hermetic CI). Otherwise ask for the
    // layer's tracked face by name; `resolve_face` maps it through the
    // configured `faces`/`fallback` chain (or system discovery when no config
    // is installed). With no face tracked at all, `SystemJp` is kept for
    // scripts that never create a `Font`.
    //
    // Both the resolved face and the per-height/bold atlas are cached
    // process-wide (`resolve_face` / `with_cached_atlas_styled`).
    // `MessageArea.charOutput` draws one character per call, so the face is
    // resolved once and each glyph rasterized once per `(face, height, bold)`.
    let request =
        if let Some(path) = std::env::var_os("KRKR_RS_SYSTEM_FONT").map(std::path::PathBuf::from) {
            FaceRequest::Path(path)
        } else {
            match face_override.clone() {
                Some(face) => FaceRequest::Named(face),
                None => FaceRequest::SystemJp,
            }
        };
    if let Some(face) = resolve_face(&request) {
        // Layout rasterizes new glyphs; do it before taking the scene lock.
        let text_layout =
            with_cached_atlas_styled(face.clone(), font_height, style.bold, |atlas| {
                layout(
                    &text,
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
        let _ = bitmap_id;
    }
    set_void_out(out);
    0
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
    let count = match engine.get_member(array.raw_id(), "count") {
        Ok(TjsValue::Integer(n)) => n.max(0) as usize,
        Ok(TjsValue::Real(n)) => n.max(0.0) as usize,
        _ => 0,
    };
    let mut pts = Vec::with_capacity(count);
    for i in 0..count {
        if engine.get_member(array.raw_id(), &i.to_string()).is_err() {
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
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    if !objthis.is_null()
        && let Ok(value) = context_engine().retain_object_detached(objthis)
    {
        PENDING_TRANSITIONS
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(value);
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
    for value in pending {
        let _ = engine.call_member(value.raw_id(), "onTransitionCompleted", &[]);
        drop(value);
    }
}

/// Generic no-op stub for methods that need real pixel/affine operations
/// (a later wave; documented in the module doc).
extern "C" fn layer_noop(
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
/// (plus newlines/controls). Glyphs are vertically centered in the em box,
/// matching the vector layout convention. Synthetic rotation/italic are not
/// applied because the `.tft` bitmap is already baked for its
/// `(face, height, bold, italic, angle)` key.
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
    spread: u32,
    style: DrawTextStyle,
) {
    let line_height = font_height.max(1) as i32;
    let ascent = (font_height as f32 * 0.85) as i32;
    let thickness = (font_height / 14).max(1) as i32;
    let mut pen_x = x;
    let mut pen_y = y;
    let mut line_start = x;

    for ch in text.chars() {
        if ch == '\n' {
            paint_prerendered_rules(
                bitmap, line_start, pen_x, pen_y, ascent, thickness, color, opa, style,
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
        let top = pen_y + (line_height - gh) / 2;
        let left = pen_x + glyph.origin_x as i32;
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
        bitmap, line_start, pen_x, pen_y, ascent, thickness, color, opa, style,
    );
}

/// Underline / strikeout rules for one pre-rendered line.
#[allow(clippy::too_many_arguments)]
fn paint_prerendered_rules(
    bitmap: &mut BitmapState,
    x0: i32,
    x1: i32,
    line_y: i32,
    ascent: i32,
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
            line_y + ascent + 2,
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
            line_y + ascent - ascent / 3,
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

/// Reference `UpdateDrawFace`: the layer's main-image draw face. `face`
/// overrides the blend/type-derived face unless it is `dfAuto` (128).
fn layer_draw_face(layer: &LayerState) -> i32 {
    const DF_ALPHA: i32 = 0;
    const DF_OPAQUE: i32 = 1;
    const DF_ADD_ALPHA: i32 = 4;
    const DF_AUTO: i32 = 128;
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
    let Some(bitmap_id) = layer.bitmap else {
        return error_out(out_error, "Layer.colorRect: layer has no image");
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
        // dfProvince: the engine has no province plane; ignore (the
        // reference writes ProvinceImage, which is not modelled here).
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
fn tile_bitmap_for_source(scene: &Scene, kind: Option<bool>, id: u32) -> Option<BitmapState> {
    let bitmap_id = match kind {
        // Layer object
        Some(true) => scene.layer(id).and_then(|l| l.bitmap),
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
    let Some(tile) = tile_bitmap_for_source(&scene, kind, src_id) else {
        return error_out(out_error, "Layer.tileRect: tile has no image");
    };
    let tile_rect = (0, 0, tile.width as i32, tile.height as i32);
    let Some(bitmap_id) = scene.layer(inst.id).and_then(|l| l.bitmap) else {
        return error_out(out_error, "Layer.tileRect: layer has no image");
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
    let Some(bitmap_id) = scene.layer(inst.id).and_then(|l| l.bitmap) else {
        return error_out(out_error, "Layer.fillOperateRect: layer has no image");
    };
    if let Some(bitmap) = scene.bitmap_mut(bitmap_id) {
        layer_ops::fill_operate_rect(bitmap, left, top, width, height, color, mode);
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
    let Some(src_bmp) = tile_bitmap_for_source(&scene, kind, src_id) else {
        return error_out(out_error, "Layer.stretchCopy: source has no image");
    };
    let Some(layer) = scene.layer(inst.id) else {
        return error_out(out_error, "Layer: layer no longer exists");
    };
    let clip = layer_pixel_rect(layer);
    let Some(bitmap_id) = layer.bitmap else {
        return error_out(out_error, "Layer.stretchCopy: layer has no image");
    };
    let Some(destrect) = layer_ops::intersect_rect(destrect, clip) else {
        set_void_out(out);
        return 0;
    };
    if let Some(dst) = scene.bitmap_mut(bitmap_id) {
        layer_ops::stretch_blit(dst, destrect, &src_bmp, srcrect, stretch_type);
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
    let Some(bitmap_id) = layer.bitmap else {
        return error_out(out_error, "Layer.doGrayScale: layer has no image");
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
    let Some(bitmap_id) = layer.bitmap else {
        return error_out(out_error, "Layer.adjustGamma: layer has no image");
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
    let Some(bitmap_id) = layer.bitmap else {
        return error_out(out_error, "Layer.light: layer has no image");
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
    let Some(bitmap_id) = scene.layer(inst.id).and_then(|l| l.bitmap) else {
        return error_out(out_error, "Layer.flipLR: layer has no image");
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
    let Some(bitmap_id) = scene.layer(inst.id).and_then(|l| l.bitmap) else {
        return error_out(out_error, "Layer.flipUD: layer has no image");
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
    let Some(bitmap_id) = layer.bitmap else {
        return error_out(out_error, "Layer.gaussianBlur: layer has no image");
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
    let Some(src) = tile_bitmap_for_source(&scene, kind, src_id) else {
        return error_out(out_error, "Layer.operateRect: source has no image");
    };
    if mode == 128 {
        mode = scene.layer(src_id).map_or(2, |l| l.blend_type);
    }
    let srcrect = (
        sx,
        sy,
        sx.saturating_add(sw as i32),
        sy.saturating_add(sh as i32),
    );
    let Some(bitmap_id) = scene.layer(inst.id).and_then(|l| l.bitmap) else {
        return error_out(out_error, "Layer.operateRect: layer has no image");
    };
    if let Some(dst) = scene.bitmap_mut(bitmap_id) {
        layer_ops::operate_rect(dst, dx, dy, &src, srcrect, mode, opa);
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
    let Some(src) = tile_bitmap_for_source(&scene, kind, src_id) else {
        return error_out(out_error, "Layer.stretch*: source has no image");
    };
    if mode == 128 {
        mode = scene.layer(src_id).map_or(2, |l| l.blend_type);
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
    let Some(bitmap_id) = scene.layer(inst.id).and_then(|l| l.bitmap) else {
        return error_out(out_error, "Layer.stretch*: layer has no image");
    };
    if let Some(dst) = scene.bitmap_mut(bitmap_id) {
        layer_ops::stretch_blit_mode(dst, destrect, &src, srcrect, stretch_type, mode, opa);
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
    let Some(src) = tile_bitmap_for_source(&scene, kind, src_id) else {
        return error_out(out_error, "Layer.affine*: source has no image");
    };
    if mode == 128 {
        mode = scene.layer(src_id).map_or(2, |l| l.blend_type);
    }
    let srcrect = (
        sx,
        sy,
        sx.saturating_add(sw as i32),
        sy.saturating_add(sh as i32),
    );
    let Some(bitmap_id) = scene.layer(inst.id).and_then(|l| l.bitmap) else {
        return error_out(out_error, "Layer.affine*: layer has no image");
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
        image::ImageFormat::Bmp => {
            image::codecs::bmp::BmpEncoder::new(&mut bytes)
                .write_image(
                    &bitmap.rgba,
                    bitmap.width,
                    bitmap.height,
                    image::ExtendedColorType::Rgba8,
                )
                .map_err(|e| e.to_string())?;
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
    let (scene, storage) = super::context_scene_storage();
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

/// The TJS helper implementing `TVP_ACTION_INVOKE` (`EventIntf.h:208`): it
/// builds the event dictionary `%[type, target, ...members]` and calls
/// `owner.action(ev)`. The VM exposes no `arguments` object, so the (at most
/// four) member name/value pairs are passed as fixed optional parameters.
const LAYER_EVENT_DISPATCH: &str = "(function(owner,target,t,n1,v1,n2,v2,n3,v3,n4,v4){\
    var ev=%[type:t,target:target];\n    if(n1!==void)ev[n1]=v1;\n    if(n2!==void)ev[n2]=v2;\n    if(n3!==void)ev[n3]=v3;\n    if(n4!==void)ev[n4]=v4;\n    return owner.action(ev);})";

/// The maximum number of event members any `Layer` event carries (`x`, `y`,
/// `button`, `shift` for `onMouseDown`).
const LAYER_EVENT_MAX_MEMBERS: usize = 4;

/// Dispatch one layer event to its action owner: retain the owner and the
/// layer target, evaluate the helper closure, and invoke it with the event
/// type plus alternating member name/value pairs.
fn dispatch_layer_event(
    engine: &Tjs2Engine,
    owner_raw: *mut c_void,
    target: *mut c_void,
    event_type: &str,
    members: &[(&str, i64)],
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
                args.push(TjsValue::Integer(*value));
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
                let members: Vec<(&str, i64)> = names
                    .iter()
                    .enumerate()
                    .map(|(i, name)| (*name, arg_i64(&args[i])))
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

/// Register the `Layer` native class.
pub(crate) fn register_layer(engine: &Tjs2Engine) -> Result<(), String> {
    let noop_stubs = [
        "setCenter",
        "setAffineOffset",
        "drawGlyph",
        "drawRectangles",
        "drawClosedCurve",
        "drawClosedCurve2",
        "drawCurve",
        "drawCurve2",
        "drawCurve3",
        "drawPie",
        "drawEllipse",
        "drawPath",
        "drawString",
        "drawImage",
        "drawImageRect",
        "drawImageStretch",
        "drawImageAffine",
        "setDefaultDrawTextParam",
        "resetDrawTextParam",
        "setFontStyle",
        "getDrawWidth",
        "pileRect",
        "piledCopy",
        "blendRect",
        "convertType",
        "stopTransition",
        "releaseCapture",
        "releaseTouchCapture",
        "setMode",
        "removeMode",
        "clear",
        // Pixel/province access and sibling ordering used by UI scripts; the
        // logical model covers the visible behavior, these are no-ops.
        "setMainPixel",
        "getMainPixel",
        "setMaskPixel",
        "getMaskPixel",
        "setProvincePixel",
        "getProvincePixel",
        "independProvinceImage",
        "loadProvinceImage",
        "bringToBack",
        "moveBefore",
        "moveBehind",
        "focusNext",
        "focusPrev",
        "getList",
        "onHitTest",
        "dump",
        "setAttentionPos",
        "captureMouse",
        "captureTouch",
        // Base `onPaint` action. The reference native dispatches the layer's
        // own script `onPaint` action; our engine already invokes the script
        // handler from [`paint_poll`], so the base implementation only needs
        // to exist for the game's `super.onPaint(...)` call to resolve. It
        // must stay a no-op: dispatching here would recurse (the script's
        // `onPaint` calls `super.onPaint`).
        "onPaint",
    ];
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
    ];
    methods.extend(noop_stubs.into_iter().map(|name| NativeInstanceMethodDef {
        name,
        f: layer_noop,
    }));
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

/// `layer.hasImage = true/false` — accepted; the scene derives it from the
/// attached bitmap (the game forces it true during blends).
extern "C" fn layer_has_image_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    _value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let _ = instance;
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

    /// Build a minimal version-1 `.tft` with one solid 2×2 glyph for `ch`.
    fn tiny_tft(ch: char) -> Vec<u8> {
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
        item[10..12].copy_from_slice(&2i16.to_le_bytes()); // origin_y
        item[12..14].copy_from_slice(&3i16.to_le_bytes()); // inc_x
        item[16..18].copy_from_slice(&3i16.to_le_bytes()); // inc
        data
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
        std::fs::write(env._dir.path().join("testfont.tft"), tiny_tft('A')).unwrap();
        env.run("var f = new Font('MyFace', 30, 0xffffff); f.mapPrerenderedFont('testfont.tft');")
            .unwrap();
        let advance = env.eval("f.getTextWidth('A')", "test").expect("script");
        assert_eq!(advance, tjs2_sys::TjsValue::Real(3.0));

        env.run(
            "var w = new Window(); var l = new Layer(w, null); l.setSize(32, 32); \
             l.font.face = 'MyFace'; l.font.height = 30; \
             l.drawText(1, 1, 'A', 0xffffff);",
        )
        .unwrap();
        let scene = env.scene();
        let bitmap = scene.bitmap(scene.layers[0].bitmap.unwrap()).unwrap();
        assert!(bitmap_ink(bitmap) > 0, "prerendered glyph must paint");
        // The 2×2 ink is centered in the 30 px line at y = 1 + (30-2)/2 = 15.
        assert!(
            bitmap.rgba[((15 * bitmap.width + 1) * 4 + 3) as usize] > 0,
            "prerendered ink must land at the centered glyph position"
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
}
