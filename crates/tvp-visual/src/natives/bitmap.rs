//! `Bitmap` — the native image class, implementing
//! `reference/cpp/core/visual/BitmapIntf.cpp` (and the loader behind it,
//! `reference/cpp/core/visual/GraphicsLoaderIntf.cpp`).
//!
//! Surface implemented here (reference `BitmapIntf.cpp` native declaration
//! line; the `tTJSNI_Bitmap` implementation methods precede them):
//!
//! * constructor `Bitmap([name[,colorkey]] | [w,h[,bpp]] | [bitmap[,rect]])`
//!   (`:230`),
//! * `getPixel`/`setPixel` (`:241`/`:252`), `getMaskPixel`/`setMaskPixel`
//!   (`:262`/`:273`),
//! * `independ(copy=true)` (`:283`),
//! * `setSize(w,h)` (`:294`) and `copyFrom(bitmap)` (`:306`),
//! * `save(name,type,meta)` (`:326`),
//! * `load(name[,colorkey])` (`:343`), `loadAsync(name)` (`:367`),
//! * `loadHeader(name)` (`:378`) and `getSaveOption(type)` (`:403`)
//!   — declared `TJS_END_NATIVE_STATIC_METHOD_DECL`, so they are registered
//!   as static members on the `Bitmap` class object (no instance needed);
//! * properties `width`/`height` (`:454`/`:472`), `buffer` (`:490`),
//!   `bufferForWrite` (`:504`), `bufferPitch` (`:519`), `loading` (`:533`),
//! * the `onLoaded(meta, async, error, message)` event (`:432`), dispatched
//!   by [`async_poll`] on the VM thread when a background decode finishes.
//!
//! ## Async loading
//!
//! `loadAsync` sets `loading=true`, retains the calling object (the action
//! owner) and spawns a worker thread that resolves/reads/decodes the image
//! from storage. The worker never touches the scene or the VM: it sends the
//! decoded pixels through a channel. [`async_poll`] — called from
//! [`crate::timer_poll`] each frame on the VM thread — applies the pixels to
//! the scene, clears `loading`, and invokes the object's script `onLoaded`.
//! This keeps the VM thread non-blocking and makes completion race-free.
//!
//! Pixmaps are shared by reference in TVP (the cache and every layer keep
//! them alive), so `destroy` does not remove the scene bitmap.

use std::ffi::{c_char, c_int, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, LazyLock, Mutex, Weak};

use tjs2_sys::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef,
    NativeMethodDef, NativeStaticMembers, RetainedValue, Tjs2Engine, TjsValue, VAL_RETAINED,
    VAL_STRING, Value,
};

use super::ffi::{arg_i64, arg_string, args, error_out, instance_ref, set_int_out, set_void_out};
use super::{context_scene_mut, context_scene_read};
use crate::bitmap::DecodedImage;

/// Sentinel stored in [`BitmapShared::id`] until a bitmap exists.
const NO_BITMAP: u32 = u32::MAX;

/// Result of one background decode: the resolved storage name and the
/// decoded pixels, or an error message.
type AsyncLoadResult = Result<(String, DecodedImage), String>;
/// The VM-side receiver slot for an in-flight [`AsyncLoadResult`].
type PendingLoad = Mutex<Option<Receiver<AsyncLoadResult>>>;

/// State shared between a `Bitmap` native instance and its background async
/// worker. All fields are touched only on the VM thread except the channel
/// `pending` (written by the worker through `Sender`) and the atomic flags.
struct BitmapShared {
    /// Scene bitmap id, or [`NO_BITMAP`] before a bitmap is assigned.
    id: AtomicU32,
    /// True while a background `loadAsync` is in flight.
    loading: AtomicBool,
    /// Receiver for the in-flight async load, polled by [`async_poll`].
    pending: PendingLoad,
    /// Retained TJS object to receive the `onLoaded` event.
    owner: Mutex<Option<DetachedValue>>,
    /// Storage name of the in-flight request (diagnostics).
    request: Mutex<Option<String>>,
}

impl Default for BitmapShared {
    fn default() -> Self {
        Self {
            id: AtomicU32::new(NO_BITMAP),
            loading: AtomicBool::new(false),
            pending: Mutex::new(None),
            owner: Mutex::new(None),
            request: Mutex::new(None),
        }
    }
}

/// Payload of one script-visible `Bitmap` object.
pub(crate) struct BitmapInst {
    shared: Arc<BitmapShared>,
    /// Whether the native constructor has run.
    pub constructed: bool,
}

impl Default for BitmapInst {
    fn default() -> Self {
        Self {
            shared: Arc::new(BitmapShared::default()),
            constructed: false,
        }
    }
}

impl BitmapInst {
    fn id(&self) -> Option<u32> {
        let id = self.shared.id.load(Ordering::Acquire);
        (id != NO_BITMAP).then_some(id)
    }

    fn set_id(&self, id: u32) {
        self.shared.id.store(id, Ordering::Release);
    }

    fn is_loading(&self) -> bool {
        self.shared.loading.load(Ordering::Acquire)
    }
}

/// Bitmaps whose async loads may have completed, polled by [`async_poll`].
static ASYNC_SLOTS: LazyLock<Mutex<Vec<Weak<BitmapShared>>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// `new Bitmap(...)` payload factory.
extern "C" fn bitmap_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::<BitmapInst>::default()) as *mut c_void
}

/// Release a `Bitmap` payload. Bitmaps are shared by reference in TVP (a
/// layer, another script `Bitmap` or the decode cache may still hold one), so
/// this intentionally does not remove the bitmap from the scene. It is freed
/// only when a layer that owns it is destroyed (see
/// `Scene::release_bitmap`); script-owned pixels deliberately outlive the
/// script object, matching the conservative reference semantics.
extern "C" fn bitmap_destroy(_engine: *mut c_void, instance: *mut c_void) {
    // SAFETY: instance came from Box::into_raw.
    unsafe { drop(Box::from_raw(instance as *mut BitmapInst)) };
}

/// Whether a callback argument is one of TJS's numeric types.
fn is_number(v: &Value) -> bool {
    v.ty == tjs2_sys::VAL_INTEGER || v.ty == tjs2_sys::VAL_REAL
}

/// Mark a scene bitmap as owned by a script `Bitmap` object. Such a bitmap
/// may be shared with a layer (`Layer.assignImages(bitmap)` attaches the
/// source id directly), so destroying that layer must not free it.
fn mark_script_owned(id: u32) {
    let mut scene = context_scene_mut();
    if let Some(bitmap) = scene.bitmap_mut(id) {
        bitmap.script_owned = true;
    }
}

/// Create a blank bitmap of the requested size, returning its scene id.
fn blank_bitmap(w: u32, h: u32) -> Result<u32, String> {
    let mut scene = context_scene_mut();
    Ok(crate::bitmap::add_blank_bitmap(&mut scene, w, h))
}

/// Load a bitmap from game storage by name, returning its scene id.
fn load_named_bitmap(name: &str) -> Result<u32, String> {
    let (mut scene, mut storage) = super::context_scene_storage();
    crate::bitmap::load_bitmap_from_storage(
        &mut scene,
        &mut super::bitmap_cache(),
        &mut storage,
        name,
        None,
    )
    .map_err(|e| format!("Bitmap: {e}"))
}

/// Reference `TVPCurrentlyAsyncLoadBitmap` error.
const ERR_ASYNC: &str = "Currently async load bitmap";
/// Reference `TVPNotDrawableLayerType` error.
const ERR_NOT_DRAWABLE: &str = "Not drawable layer type";

/// Read the bitmap id for an operation that requires a ready image, mapping
/// the reference's loading/not-drawable checks.
fn require_bitmap(inst: &BitmapInst) -> Result<u32, String> {
    if inst.is_loading() {
        return Err(ERR_ASYNC.into());
    }
    inst.id().ok_or_else(|| ERR_NOT_DRAWABLE.into())
}

/// Resolve a `Bitmap` argument (an object exposing `nativeId`/`id`, or a raw
/// bitmap id) to a scene bitmap id.
fn bitmap_id_from_arg(arg: &Value) -> Result<u32, String> {
    if is_number(arg) {
        let id = arg_i64(arg);
        return (id >= 0)
            .then_some(id as u32)
            .ok_or_else(|| "Bitmap: source id must be non-negative".to_string());
    }
    let engine = crate::natives::context_engine();
    let dv = engine
        .retain_value_detached(&tjs2_sys::TjsValue::Object)
        .map_err(|e| format!("Bitmap: cannot read source bitmap ({e})"))?;
    for member in ["nativeId", "id"] {
        match engine.get_member(dv.raw_id(), member) {
            Ok(tjs2_sys::TjsValue::Integer(v)) if v >= 0 => return Ok(v as u32),
            Ok(tjs2_sys::TjsValue::Real(v)) if v >= 0.0 => return Ok(v as u32),
            _ => {}
        }
    }
    Err("Bitmap: source object is not a Bitmap".into())
}

/// `Bitmap(bitmap)` — copy the source bitmap's pixels into a new bitmap (the
/// reference `tTJSNI_Bitmap::CopyFrom` / `tTVPBaseBitmap::Assign`).
fn copy_bitmap_arg(arg: &Value) -> Result<u32, String> {
    let src_id = bitmap_id_from_arg(arg)?;
    let mut scene = context_scene_mut();
    let Some((w, h, rgba)) = scene
        .bitmap(src_id)
        .map(|b| (b.width, b.height, b.rgba.clone()))
    else {
        return Err("Bitmap: source bitmap no longer exists".into());
    };
    Ok(scene.add_bitmap(w, h, rgba))
}

/// Build a TJS metainfo dictionary expression `%["width"=>w, "height"=>h,
/// "bpp"=>bpp]` (all values numeric, so no escaping is needed). The
/// reference's per-format `TVPLoadHeader*` fill in richer tags; the size/bpp
/// keys are the common, game-visible subset.
fn meta_expr(width: u32, height: u32, bpp: u8) -> String {
    format!("%[\"width\"=>{width}, \"height\"=>{height}, \"bpp\"=>{bpp}]")
}

/// Evaluate an object expression and write the retained result into `*out`.
/// Returns false (leaving `*out` untouched) when the expression does not
/// produce an object.
fn eval_object_out(engine: &Tjs2Engine, expr: &str, out: *mut Value) -> bool {
    match engine.eval_retained(expr, "bitmap.meta") {
        Ok(RetainedValue::Object(dv)) => {
            // SAFETY: out is a valid result slot; the C++ side consumes the
            // retention before the callback returns.
            unsafe {
                (*out).ty = VAL_RETAINED;
                (*out).integer = 0;
                (*out).real = 0.0;
                (*out).string = ptr::null();
                (*out).array = ptr::null();
                (*out).array_count = 0;
                (*out).retained = dv.raw_id() as usize;
            }
            std::mem::forget(dv);
            true
        }
        _ => false,
    }
}

/// Build a retained metainfo dictionary argument for `onLoaded`.
fn meta_arg(engine: &Tjs2Engine, width: u32, height: u32, bpp: u8) -> Option<TjsValue> {
    match engine.eval_retained(&meta_expr(width, height, bpp), "bitmap.onLoaded") {
        Ok(RetainedValue::Object(dv)) => {
            let raw = dv.raw_id() as usize as u64;
            std::mem::forget(dv);
            Some(TjsValue::Retained(raw))
        }
        _ => None,
    }
}

/// `__TvpBitmap(...)` — constructor hook. Accepts the reference forms
/// `Bitmap()`, `Bitmap(name[, colorkey])`, `Bitmap(w, h[, bpp])` and the
/// crate's `Bitmap(bitmap[, rect])` copy convenience. Returns the new
/// bitmap's scene id.
extern "C" fn bitmap_ctor(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/out/out_error are valid for the call.
    let args = unsafe { args(argc, argv) };
    let inst = unsafe { instance_ref::<BitmapInst>(instance) };
    if inst.constructed {
        return error_out(out_error, "Bitmap: this bitmap is already constructed");
    }
    let (id, colorkey) = match args {
        // `Bitmap()` — the empty bitmap the game's derived-class constructors
        // call (e.g. `class PreviewThumbnail extends Bitmap`).
        [] => (blank_bitmap(0, 0), crate::bitmap::COLOR_KEY_NONE),
        // `Bitmap(name)` — load from storage.
        [a] if a.ty == VAL_STRING => (
            load_named_bitmap(&arg_string(a)),
            crate::bitmap::COLOR_KEY_NONE,
        ),
        // `Bitmap(bitmap)` — copy.
        [a] if a.ty == tjs2_sys::VAL_OBJECT => (copy_bitmap_arg(a), crate::bitmap::COLOR_KEY_NONE),
        // `Bitmap(name, colorkey)` — load with color-key transparency.
        [a, key] if a.ty == VAL_STRING => (load_named_bitmap(&arg_string(a)), arg_i64(key) as u32),
        // `Bitmap(bitmap, rect)` — copy; rect ignored (whole bitmap).
        [a, _rect] if a.ty == tjs2_sys::VAL_OBJECT => {
            (copy_bitmap_arg(a), crate::bitmap::COLOR_KEY_NONE)
        }
        // `Bitmap(w, h[, bpp])` — blank; bpp ignored (always 32bpp RGBA8).
        [a, b, ..] if is_number(a) && is_number(b) => (
            blank_bitmap(arg_i64(a).max(0) as u32, arg_i64(b).max(0) as u32),
            crate::bitmap::COLOR_KEY_NONE,
        ),
        _ => (
            Err(
                "Bitmap: constructor expects (), a storage name, (width, height), or a Bitmap"
                    .into(),
            ),
            crate::bitmap::COLOR_KEY_NONE,
        ),
    };
    match id {
        Ok(id) => {
            inst.set_id(id);
            inst.constructed = true;
            mark_script_owned(id);
            apply_color_key(id, colorkey);
            set_int_out(out, i64::from(id));
            0
        }
        Err(msg) => error_out(out_error, &msg),
    }
}

/// Apply a color key to a freshly loaded/created scene bitmap and mark it
/// dirty. A `none` key is a no-op, matching the reference default.
fn apply_color_key(id: u32, keyidx: u32) {
    if keyidx == crate::bitmap::COLOR_KEY_NONE {
        return;
    }
    let mut scene = context_scene_mut();
    if let Some(b) = scene.bitmap_mut(id) {
        crate::bitmap::apply_color_key(&mut b.rgba, b.width, keyidx);
        b.mark_dirty();
    }
}

/// `getPixel(x, y)` — script color `0xRRGGBB`, alpha ignored
/// (`BitmapIntf.cpp:53`).
extern "C" fn bitmap_get_pixel(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<BitmapInst>(instance) };
    let args = unsafe { args(argc, argv) };
    if args.len() < 2 {
        return error_out(out_error, "Bitmap.getPixel: expects (x, y)");
    }
    let id = match require_bitmap(inst) {
        Ok(id) => id,
        Err(e) => return error_out(out_error, &e),
    };
    let (x, y) = (arg_i64(&args[0]) as i32, arg_i64(&args[1]) as i32);
    let scene = context_scene_read();
    let Some(bmp) = scene.bitmap(id) else {
        return error_out(out_error, ERR_NOT_DRAWABLE);
    };
    let Some(off) = pixel_offset_checked(bmp, x, y) else {
        return error_out(out_error, "Out of Rectangle");
    };
    let c = &bmp.rgba[off..off + 4];
    set_int_out(
        out,
        i64::from((u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2])),
    );
    0
}

/// `setPixel(x, y, color)` — set RGB, preserve alpha (`BitmapIntf.cpp:62`).
extern "C" fn bitmap_set_pixel(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<BitmapInst>(instance) };
    let args = unsafe { args(argc, argv) };
    if args.len() < 3 {
        return error_out(out_error, "Bitmap.setPixel: expects (x, y, color)");
    }
    let id = match require_bitmap(inst) {
        Ok(id) => id,
        Err(e) => return error_out(out_error, &e),
    };
    let (x, y) = (arg_i64(&args[0]) as i32, arg_i64(&args[1]) as i32);
    let color = arg_i64(&args[2]) as u32;
    let mut scene = context_scene_mut();
    let Some(bmp) = scene.bitmap_mut(id) else {
        return error_out(out_error, ERR_NOT_DRAWABLE);
    };
    let Some(off) = pixel_offset_checked(bmp, x, y) else {
        return error_out(out_error, "Out of Rectangle");
    };
    bmp.rgba[off] = ((color >> 16) & 0xff) as u8;
    bmp.rgba[off + 1] = ((color >> 8) & 0xff) as u8;
    bmp.rgba[off + 2] = (color & 0xff) as u8;
    bmp.mark_dirty();
    set_void_out(out);
    0
}

/// `getMaskPixel(x, y)` — alpha byte (`BitmapIntf.cpp:69`).
extern "C" fn bitmap_get_mask_pixel(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<BitmapInst>(instance) };
    let args = unsafe { args(argc, argv) };
    if args.len() < 2 {
        return error_out(out_error, "Bitmap.getMaskPixel: expects (x, y)");
    }
    let id = match require_bitmap(inst) {
        Ok(id) => id,
        Err(e) => return error_out(out_error, &e),
    };
    let (x, y) = (arg_i64(&args[0]) as i32, arg_i64(&args[1]) as i32);
    let scene = context_scene_read();
    let Some(bmp) = scene.bitmap(id) else {
        return error_out(out_error, ERR_NOT_DRAWABLE);
    };
    let Some(off) = pixel_offset_checked(bmp, x, y) else {
        return error_out(out_error, "Out of Rectangle");
    };
    set_int_out(out, i64::from(bmp.rgba[off + 3]));
    0
}

/// `setMaskPixel(x, y, mask)` — set alpha, preserve RGB
/// (`BitmapIntf.cpp:78`).
extern "C" fn bitmap_set_mask_pixel(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<BitmapInst>(instance) };
    let args = unsafe { args(argc, argv) };
    if args.len() < 3 {
        return error_out(out_error, "Bitmap.setMaskPixel: expects (x, y, mask)");
    }
    let id = match require_bitmap(inst) {
        Ok(id) => id,
        Err(e) => return error_out(out_error, &e),
    };
    let (x, y) = (arg_i64(&args[0]) as i32, arg_i64(&args[1]) as i32);
    let mask = (arg_i64(&args[2]) & 0xff) as u8;
    let mut scene = context_scene_mut();
    let Some(bmp) = scene.bitmap_mut(id) else {
        return error_out(out_error, ERR_NOT_DRAWABLE);
    };
    let Some(off) = pixel_offset_checked(bmp, x, y) else {
        return error_out(out_error, "Out of Rectangle");
    };
    bmp.rgba[off + 3] = mask;
    bmp.mark_dirty();
    set_void_out(out);
    0
}

/// `independ([copy=true])` (`BitmapIntf.cpp:87`).
///
/// The reference detaches a `tTVPBaseBitmap` from a shared internal image
/// buffer (`Independ` copies, `IndependNoCopy` does not). Every scene
/// bitmap in this crate owns its `Vec<u8>` exclusively, so the instance is
/// already independent; the call validates state (throwing while loading /
/// when no bitmap exists) and is otherwise a no-op by construction.
extern "C" fn bitmap_independ(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<BitmapInst>(instance) };
    let _ = unsafe { args(argc, argv) };
    if let Err(e) = require_bitmap(inst) {
        return error_out(out_error, &e);
    }
    set_void_out(out);
    0
}

/// `setSize(w, h)` — resize preserving the overlapping pixels
/// (`BitmapIntf.cpp:126`; keepimage defaults true).
extern "C" fn bitmap_set_size(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<BitmapInst>(instance) };
    let args = unsafe { args(argc, argv) };
    if args.len() < 2 {
        return error_out(out_error, "Bitmap.setSize: expects (width, height)");
    }
    let id = match require_bitmap(inst) {
        Ok(id) => id,
        Err(e) => return error_out(out_error, &e),
    };
    let (w, h) = (arg_i64(&args[0]), arg_i64(&args[1]));
    if w <= 0 || h <= 0 {
        return error_out(out_error, "Cannot create empty layer image");
    }
    let mut scene = context_scene_mut();
    crate::bitmap::resize_bitmap_keep(&mut scene, id, w as u32, h as u32);
    set_void_out(out);
    0
}

/// `copyFrom(bitmap)` (`BitmapIntf.cpp:219`) — replace this bitmap's size and
/// pixels with the source's (`tTVPBaseBitmap::Assign`).
extern "C" fn bitmap_copy_from(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<BitmapInst>(instance) };
    let args = unsafe { args(argc, argv) };
    if args.is_empty() {
        return error_out(out_error, "Bitmap.copyFrom: expects a source Bitmap");
    }
    let dst = match require_bitmap(inst) {
        Ok(id) => id,
        Err(e) => return error_out(out_error, &e),
    };
    let src = match bitmap_id_from_arg(&args[0]) {
        Ok(id) => id,
        Err(e) => return error_out(out_error, &e),
    };
    let mut scene = context_scene_mut();
    if src == dst {
        set_void_out(out);
        return 0;
    }
    let Some((w, h, rgba)) = scene
        .bitmap(src)
        .map(|b| (b.width, b.height, b.rgba.clone()))
    else {
        return error_out(out_error, "Bitmap.copyFrom: source bitmap no longer exists");
    };
    crate::bitmap::replace_bitmap_rgba(&mut scene, dst, w, h, rgba);
    set_void_out(out);
    0
}

/// `save(name, [type], [meta])` (`BitmapIntf.cpp:116`) — encode this bitmap
/// and write it under the game directory.
extern "C" fn bitmap_save(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<BitmapInst>(instance) };
    let args = unsafe { args(argc, argv) };
    if args.is_empty() {
        return error_out(out_error, "Bitmap.save: expects (name, [type], [meta])");
    }
    let name = arg_string(&args[0]);
    let type_name = if args.len() >= 2 && args[1].ty != tjs2_sys::VAL_VOID {
        arg_string(&args[1])
    } else {
        "bmp".to_string()
    };
    let id = match require_bitmap(inst) {
        Ok(id) => id,
        Err(e) => return error_out(out_error, &e),
    };
    let format = match crate::bitmap::save_format_from_type(&type_name) {
        Some(f) => f,
        None => return error_out(out_error, &format!("Unknown graphic format {type_name}")),
    };
    // Snapshot pixels, then drop the scene lock before the (blocking) write.
    let (w, h, rgba) = {
        let scene = context_scene_read();
        let Some(bmp) = scene.bitmap(id) else {
            return error_out(out_error, ERR_NOT_DRAWABLE);
        };
        (bmp.width, bmp.height, bmp.rgba.clone())
    };
    let storage = super::context_storage();
    if let Err(e) = crate::bitmap::save_bitmap_to_storage(&storage, &name, format, w, h, &rgba) {
        return error_out(out_error, &e.to_string());
    }
    set_void_out(out);
    0
}

/// `load(name, [colorkey])` (`BitmapIntf.cpp:96`) — synchronous load into
/// this bitmap; returns the metainfo dictionary.
extern "C" fn bitmap_load(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<BitmapInst>(instance) };
    let args = unsafe { args(argc, argv) };
    if args.is_empty() {
        return error_out(out_error, "Bitmap.load: expects a storage name");
    }
    let name = arg_string(&args[0]);
    let colorkey = if args.len() >= 2 && args[1].ty != tjs2_sys::VAL_VOID {
        arg_i64(&args[1]) as u32
    } else {
        crate::bitmap::COLOR_KEY_NONE
    };
    if inst.is_loading() {
        return error_out(out_error, ERR_ASYNC);
    }
    let target = inst.id();
    let (id, width, height) = {
        let (mut scene, mut storage) = super::context_scene_storage();
        let id = match crate::bitmap::load_bitmap_into_storage(
            &mut scene,
            &mut super::bitmap_cache(),
            &mut storage,
            &name,
            target,
            None,
        ) {
            Ok(id) => id,
            Err(e) => return error_out(out_error, &format!("Bitmap.load: {e}")),
        };
        let dims = scene
            .bitmap(id)
            .map(|b| (b.width, b.height))
            .unwrap_or((1, 1));
        (id, dims.0, dims.1)
    };
    inst.set_id(id);
    mark_script_owned(id);
    apply_color_key(id, colorkey);
    // Scene pixels are always RGBA8, so the reported bpp is 32.
    let engine = crate::natives::context_engine();
    if !eval_object_out(engine, &meta_expr(width, height, 32), out) {
        set_void_out(out);
    }
    0
}

/// `loadAsync(name)` (`BitmapIntf.cpp:109`) — request a background load.
extern "C" fn bitmap_load_async(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<BitmapInst>(instance) };
    let args = unsafe { args(argc, argv) };
    if args.is_empty() {
        return error_out(out_error, "Bitmap.loadAsync: expects a storage name");
    }
    let name = arg_string(&args[0]);
    if inst.is_loading() {
        return error_out(out_error, ERR_ASYNC);
    }
    let engine = crate::natives::context_engine();
    // Retain the calling object so it stays alive for the onLoaded dispatch,
    // exactly like the reference's `owner->AddRef()`.
    let owner = engine.retain_object_detached(objthis).ok();
    *inst.shared.owner.lock().unwrap_or_else(|p| p.into_inner()) = owner;
    *inst
        .shared
        .request
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = Some(name.clone());
    inst.shared.loading.store(true, Ordering::Release);

    let (tx, rx) = mpsc::channel();
    *inst
        .shared
        .pending
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = Some(rx);

    std::thread::spawn(move || {
        // Resolve/read/decode entirely off the VM thread; the scene is only
        // mutated later by `async_poll` on the VM thread.
        let result = {
            let mut storage = super::context_storage();
            crate::bitmap::read_and_decode_from_storage(&mut storage, &name)
                .map_err(|e| e.to_string())
        };
        // A send failure only means the Bitmap was destroyed first.
        let _ = tx.send(result);
    });

    ASYNC_SLOTS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(Arc::downgrade(&inst.shared));
    set_void_out(out);
    0
}

/// `loadHeader(name)` (`BitmapIntf.cpp:378`) — image size/alpha dictionary.
/// A `TJS_END_NATIVE_STATIC_METHOD_DECL` member: it lives on the `Bitmap`
/// class object and is reachable without an instance.
extern "C" fn bitmap_load_header(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let args = unsafe { args(argc, argv) };
    if args.is_empty() {
        return error_out(out_error, "Bitmap.loadHeader: expects a storage name");
    }
    let name = arg_string(&args[0]);
    let result = {
        let mut storage = super::context_storage();
        crate::bitmap::load_image_header(&mut storage, &name)
    };
    match result {
        Ok((w, h, has_alpha)) => {
            let engine = crate::natives::context_engine();
            let bpp = if has_alpha { 32 } else { 24 };
            if !eval_object_out(engine, &meta_expr(w, h, bpp), out) {
                set_void_out(out);
            }
            0
        }
        Err(e) => error_out(out_error, &format!("Bitmap.loadHeader: {e}")),
    }
}

/// `getSaveOption(type)` (`BitmapIntf.cpp:399`) — save-option dictionary.
/// A `TJS_END_NATIVE_STATIC_METHOD_DECL` member (see `bitmap_load_header`).
extern "C" fn bitmap_get_save_option(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let args = unsafe { args(argc, argv) };
    if args.is_empty() {
        return error_out(out_error, "Bitmap.getSaveOption: expects a type");
    }
    let type_name = arg_string(&args[0]);
    let engine = crate::natives::context_engine();
    let expr = save_option_expr(&type_name);
    match expr {
        Some(expr) if eval_object_out(engine, &expr, out) => 0,
        Some(_) => {
            set_void_out(out);
            0
        }
        None => {
            // Reference returns void for an unaccepted type.
            set_void_out(out);
            0
        }
    }
}

/// The `getSaveOption` expression for a type, mirroring the reference's
/// `AcceptSave` option dictionaries at a useful level of detail.
fn save_option_expr(type_name: &str) -> Option<String> {
    let t = type_name.to_ascii_lowercase();
    if t.starts_with("bmp") || t == ".bmp" || t == ".dib" {
        Some(
            "%[\"bpp\"=>%[\"type\"=>\"select\",\"items\"=>[\"32\",\"24\",\"8\"],\
             \"desc\"=>\"bpp\",\"default\"=>0]]"
                .to_string(),
        )
    } else if t.starts_with("png") || t == ".png" {
        Some("%[]".to_string())
    } else if t.starts_with("jpg") || t == ".jpg" || t == ".jpeg" || t == ".jif" {
        Some(
            "%[\"quality\"=>%[\"type\"=>\"range\",\"min\"=>1,\"max\"=>100,\
             \"desc\"=>\"100 is high quality, 1 is low quality\",\"default\"=>90]]"
                .to_string(),
        )
    } else {
        None
    }
}

/// `onLoaded(meta, async, error, message)` (`BitmapIntf.cpp:287`). The async
/// poll dispatches the object's (script-overridable) `onLoaded` member
/// directly, so this base implementation is a no-op kept for
/// `super.onLoaded(...)`.
extern "C" fn bitmap_on_loaded(
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

/// `width` getter (`BitmapIntf.cpp:429`).
extern "C" fn bitmap_width_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    bitmap_dim_get(instance, out, out_error, true)
}

/// `height` getter (`BitmapIntf.cpp:451`).
extern "C" fn bitmap_height_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    bitmap_dim_get(instance, out, out_error, false)
}

fn bitmap_dim_get(
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    width: bool,
) -> c_int {
    let inst = unsafe { instance_ref::<BitmapInst>(instance) };
    let id = match require_bitmap(inst) {
        Ok(id) => id,
        Err(e) => return error_out(out_error, &e),
    };
    let scene = context_scene_read();
    let Some(bmp) = scene.bitmap(id) else {
        return error_out(out_error, ERR_NOT_DRAWABLE);
    };
    set_int_out(out, i64::from(if width { bmp.width } else { bmp.height }));
    0
}

/// `width` setter (`BitmapIntf.cpp:157`, `SetWidth`).
extern "C" fn bitmap_width_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    bitmap_dim_set(instance, value, out_error, true)
}

/// `height` setter (`BitmapIntf.cpp:177`, `SetHeight`).
extern "C" fn bitmap_height_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    bitmap_dim_set(instance, value, out_error, false)
}

fn bitmap_dim_set(
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    width: bool,
) -> c_int {
    let inst = unsafe { instance_ref::<BitmapInst>(instance) };
    // SAFETY: value is a valid argument for the duration of the call.
    let v = unsafe { &*value };
    let id = match require_bitmap(inst) {
        Ok(id) => id,
        Err(e) => return error_out(out_error, &e),
    };
    let new = arg_i64(v).max(1) as u32;
    let (w, h) = {
        let scene = context_scene_read();
        let Some(bmp) = scene.bitmap(id) else {
            return error_out(out_error, ERR_NOT_DRAWABLE);
        };
        (bmp.width, bmp.height)
    };
    let (w, h) = if width { (new, h) } else { (w, new) };
    if (w, h) == (0, 0) {
        return error_out(out_error, "Cannot create empty layer image");
    }
    let mut scene = context_scene_mut();
    crate::bitmap::resize_bitmap_keep(&mut scene, id, w, h);
    0
}

/// `buffer` getter (`BitmapIntf.cpp:475`) — raw pixel pointer.
extern "C" fn bitmap_buffer_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    bitmap_buffer_addr(instance, out, out_error)
}

/// `bufferForWrite` getter (`BitmapIntf.cpp:487`).
extern "C" fn bitmap_buffer_for_write_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    bitmap_buffer_addr(instance, out, out_error)
}

fn bitmap_buffer_addr(
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let inst = unsafe { instance_ref::<BitmapInst>(instance) };
    let id = match require_bitmap(inst) {
        Ok(id) => id,
        Err(e) => return error_out(out_error, &e),
    };
    let scene = context_scene_read();
    let Some(bmp) = scene.bitmap(id) else {
        return error_out(out_error, ERR_NOT_DRAWABLE);
    };
    // The pointer is valid until the bitmap's pixels are next mutated; the
    // VM is single-threaded, matching the reference's raw `GetScanLine(0)`.
    set_int_out(out, bmp.rgba.as_ptr() as i64);
    0
}

/// `bufferPitch` getter (`BitmapIntf.cpp:501`) — bytes per scanline.
extern "C" fn bitmap_buffer_pitch_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<BitmapInst>(instance) };
    let id = match require_bitmap(inst) {
        Ok(id) => id,
        Err(e) => return error_out(out_error, &e),
    };
    let scene = context_scene_read();
    let Some(bmp) = scene.bitmap(id) else {
        return error_out(out_error, ERR_NOT_DRAWABLE);
    };
    set_int_out(out, i64::from(bmp.width) * 4);
    0
}

/// `loading` getter (`BitmapIntf.cpp:519`).
extern "C" fn bitmap_loading_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<BitmapInst>(instance) };
    set_int_out(out, if inst.is_loading() { 1 } else { 0 });
    0
}

/// `id` / `nativeId` — the scene bitmap id. `nativeId` matches the Layer
/// convention, so an object-argument resolver that reads `nativeId` first
/// does not trip a game class's `id` override.
extern "C" fn bitmap_id_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<BitmapInst>(instance) };
    match inst.id() {
        Some(id) => set_int_out(out, i64::from(id)),
        None => set_int_out(out, 0),
    }
    0
}

/// Bounds-checked pixel offset for a signed `(x, y)`.
fn pixel_offset_checked(bmp: &crate::scene::BitmapState, x: i32, y: i32) -> Option<usize> {
    if x < 0 || y < 0 {
        return None;
    }
    bmp.pixel_offset(x as u32, y as u32)
}

/// Apply a completed background decode on the VM thread and dispatch
/// `onLoaded`.
///
/// **Hook**: the app's VM poll phase must call this every frame (alongside
/// the timer/paint polls). It is `pub` because `natives/mod.rs` is outside
/// this task's edit scope; the intended call site is
/// `timer_poll` in `natives/mod.rs`.
pub fn async_poll(engine: &Tjs2Engine) {
    let slots: Vec<Arc<BitmapShared>> = {
        let mut guard = ASYNC_SLOTS.lock().unwrap_or_else(|p| p.into_inner());
        guard.retain(|w| w.strong_count() > 0);
        guard.iter().filter_map(Weak::upgrade).collect()
    };
    for shared in slots {
        let received = {
            let mut pending = shared.pending.lock().unwrap_or_else(|p| p.into_inner());
            match pending.as_ref() {
                Some(rx) => match rx.try_recv() {
                    Ok(result) => {
                        *pending = None;
                        Some(result)
                    }
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => {
                        *pending = None;
                        Some(Err("background image loader terminated".to_string()))
                    }
                },
                None => None,
            }
        };
        let Some(result) = received else { continue };

        let (is_error, message, meta) = match result {
            Ok((_resolved, decoded)) => {
                let DecodedImage {
                    width,
                    height,
                    rgba,
                    has_alpha,
                } = decoded;
                let bpp = if has_alpha { 32 } else { 24 };
                let id = {
                    let mut scene = context_scene_mut();
                    let id = match shared.id.load(Ordering::Acquire) {
                        id if id != NO_BITMAP && scene.bitmap(id).is_some() => {
                            crate::bitmap::replace_bitmap_rgba(&mut scene, id, width, height, rgba);
                            id
                        }
                        _ => scene.add_bitmap(width, height, rgba),
                    };
                    if let Some(bitmap) = scene.bitmap_mut(id) {
                        bitmap.script_owned = true;
                    }
                    id
                };
                shared.id.store(id, Ordering::Release);
                (false, String::new(), Some((width, height, bpp)))
            }
            Err(e) => (true, e, None),
        };
        shared.loading.store(false, Ordering::Release);
        dispatch_on_loaded(engine, &shared, is_error, &message, meta);

        // The retained owner is consumed once; drop the stale request name.
        *shared.request.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
}

/// Invoke the object's `onLoaded(meta, async, error, message)` handler, if it
/// is still alive and a handler exists.
fn dispatch_on_loaded(
    engine: &Tjs2Engine,
    shared: &Arc<BitmapShared>,
    is_error: bool,
    message: &str,
    meta: Option<(u32, u32, u8)>,
) {
    let owner = shared
        .owner
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take();
    let Some(dv) = owner else { return };
    let meta_value = meta
        .and_then(|(w, h, bpp)| meta_arg(engine, w, h, bpp))
        .unwrap_or(TjsValue::Void);
    let args = [
        meta_value,
        TjsValue::Integer(1),
        TjsValue::Integer(i64::from(is_error)),
        TjsValue::String(message.to_string()),
    ];
    let _ = engine.call_member(dv.raw_id(), "onLoaded", &args);
}

/// Register the `Bitmap` native class.
pub(crate) fn register_bitmap(engine: &Tjs2Engine) -> Result<(), String> {
    engine.register_native_class_instance(&NativeInstanceBuilder {
        name: "Bitmap",
        create: bitmap_create,
        destroy: bitmap_destroy,
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
                name: "save",
                f: bitmap_save,
            },
            NativeInstanceMethodDef {
                name: "load",
                f: bitmap_load,
            },
            NativeInstanceMethodDef {
                name: "loadAsync",
                f: bitmap_load_async,
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
    })?;
    // `loadHeader` / `getSaveOption` are `TJS_END_NATIVE_STATIC_METHOD_DECL`
    // members in the reference (`BitmapIntf.cpp:378` / `:403`): they live on
    // the class object (`Bitmap.loadHeader(name)`), not on instances.
    engine.register_native_static_members(&NativeStaticMembers {
        class_name: "Bitmap",
        methods: vec![
            NativeMethodDef {
                name: "loadHeader",
                f: bitmap_load_header,
            },
            NativeMethodDef {
                name: "getSaveOption",
                f: bitmap_get_save_option,
            },
        ],
        properties: vec![],
    })
}

// `save` intentionally does not implement the reference's TLG/TLG5/TLG6
// writer (there is no TLG encoder in this crate); it throws
// `Unknown graphic format` for those types instead of silently doing
// nothing.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::natives::tests::{TestEnv, write_fixture};
    use std::path::Path;

    /// A 4x2 solid red fixture, matching the assertions of
    /// `bitmap_loads_from_storage`.
    fn red_fixture(env: &TestEnv) {
        let (w, h) = (4u32, 2u32);
        let rgba: Vec<u8> = (0..w * h).flat_map(|_| [255u8, 0, 0, 255]).collect();
        write_fixture(env, "testimg.webp", &rgba, w, h);
    }

    /// Poll the async completion hook until `loading` clears (or time out).
    /// The production poll runs from `timer_poll`; the inline test calls the
    /// hook directly because `natives/mod.rs` is outside this task's scope.
    fn wait_async(env: &TestEnv, timeout_ms: u64) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
        loop {
            async_poll(&env.engine);
            if env.eval_int("b.loading") == 0 {
                return;
            }
            if std::time::Instant::now() > deadline {
                panic!("async load did not complete");
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn bitmap_loads_from_storage() {
        let env = TestEnv::new("bitmap-storage");
        red_fixture(&env);
        env.run("var b = new Bitmap('testimg');").unwrap();
        let scene = env.scene();
        assert_eq!(scene.bitmaps.len(), 1);
        let bmp = &scene.bitmaps[0];
        // fixture: 4x2 red webp
        assert_eq!((bmp.width, bmp.height), (4, 2));
        assert_eq!(bmp.rgba.len(), 4 * 2 * 4);
        assert!(bmp.rgba.iter().all(|&b| b == 255 || b == 0));
        assert_eq!(env.eval_int("b.width"), 4);
        assert_eq!(env.eval_int("b.height"), 2);
        assert_eq!(env.eval_int("b.id"), i64::from(bmp.id));
        assert_eq!(env.eval_int("b.nativeId"), i64::from(bmp.id));
    }

    #[test]
    fn bitmap_name_cache_aliases_decode_once_but_owns_separately() {
        let env = TestEnv::new("bitmap-cache");
        red_fixture(&env);
        env.run("var b1 = new Bitmap('testimg.webp'); var b2 = new Bitmap('testimg');")
            .unwrap();
        // Both constructors resolve to the same storage file (the cache avoids
        // re-decoding) but each `Bitmap` owns an independent pixel buffer,
        // matching the reference `TVPLoadGraphic`'s `AssignToTexture` copy.
        let scene = env.scene();
        assert_eq!(scene.bitmaps.len(), 2, "template + one owned copy");
        let b1 = scene.bitmap(env.eval_int("b1.id") as u32).unwrap();
        let b2 = scene.bitmap(env.eval_int("b2.id") as u32).unwrap();
        assert_ne!(b1.id, b2.id);
        assert_eq!(b1.rgba, b2.rgba);
    }

    #[test]
    fn blank_bitmap_size_and_pixels() {
        let env = TestEnv::new("bitmap-blank");
        env.run("var b = new Bitmap(64, 64);").unwrap();
        let scene = env.scene();
        let bmp = &scene.bitmaps[0];
        assert_eq!((bmp.width, bmp.height), (64, 64));
        assert_eq!(bmp.rgba.len(), 64 * 64 * 4);
        assert!(
            bmp.rgba.iter().all(|&b| b == 0),
            "blank bitmap is transparent black"
        );
        assert_eq!(env.eval_int("b.width"), 64);
        assert_eq!(env.eval_int("b.height"), 64);
    }

    #[test]
    fn bitmap_missing_file_errors() {
        let env = TestEnv::new("bitmap-missing");
        env.run("var got = 'no error'; try { var b = new Bitmap('definitely_not_here'); } catch (e) { got = 'error'; }")
            .unwrap();
        assert_eq!(env.eval_string("got"), "error");
    }

    #[test]
    fn zero_arg_constructor_creates_blank_bitmap() {
        // `class PreviewThumbnail extends Bitmap { function PreviewThumbnail(...) {
        // Bitmap(); ... } }` (system/Album.tjs:1244, system/Staffroll.tjs:575)
        // calls the base constructor with no arguments. `add_blank_bitmap(0, 0)`
        // clamps the empty bitmap to a non-degenerate 1x1.
        let env = TestEnv::new("bitmap-zero-arg");
        env.run("var b = new Bitmap();").unwrap();
        let scene = env.scene();
        assert_eq!(scene.bitmaps.len(), 1);
        let bmp = &scene.bitmaps[0];
        assert_eq!((bmp.width, bmp.height), (1, 1));
        assert_eq!(bmp.rgba, vec![0, 0, 0, 0]);
        assert_eq!(env.eval_int("b.width"), 1);
        assert_eq!(env.eval_int("b.height"), 1);
    }

    #[test]
    fn zero_arg_bitmap_is_a_valid_copy_rect_source() {
        // Mirrors the album path: `Bitmap()` then construct a Sprite and copy
        // the empty preview bitmap into it (`_thumb.copyRect(0, 0, this, ...)`).
        let env = TestEnv::new("bitmap-zero-arg-copyrect");
        env.run(
            "var win = new Window(); \
             var bm = new Bitmap(); \
             var par = new Layer(win, null); \
             var thumb = new Layer(win, par); \
             thumb.setSize(16, 16); \
             thumb.copyRect(0, 0, bm, 0, 0, 16, 16);",
        )
        .unwrap();
        let scene = env.scene();
        assert!(
            scene.layers.iter().any(|l| l.bitmap.is_some()),
            "the empty Bitmap must be attachable via copyRect"
        );
    }

    #[test]
    fn bitmap_copy_constructor_duplicates_pixels() {
        let env = TestEnv::new("bitmap-copy-ctor");
        env.run("var src = new Bitmap(4, 2); var cp = new Bitmap(src);")
            .unwrap();
        let scene = env.scene();
        assert_eq!(scene.bitmaps.len(), 2);
        assert_ne!(scene.bitmaps[0].id, scene.bitmaps[1].id);
        assert_eq!((scene.bitmaps[1].width, scene.bitmaps[1].height), (4, 2));
        assert_eq!(scene.bitmaps[0].rgba, scene.bitmaps[1].rgba);
        assert_eq!(env.eval_int("cp.width"), 4);
        assert_eq!(env.eval_int("cp.height"), 2);
    }

    #[test]
    fn blank_bitmap_three_args_ignores_bpp() {
        let env = TestEnv::new("bitmap-blank-bpp");
        env.run("var b = new Bitmap(5, 3, 24);").unwrap();
        let scene = env.scene();
        assert_eq!((scene.bitmaps[0].width, scene.bitmaps[0].height), (5, 3));
        assert_eq!(env.eval_int("b.width"), 5);
        assert_eq!(env.eval_int("b.height"), 3);
    }

    #[test]
    fn bitmap_name_with_colorkey_loads() {
        let env = TestEnv::new("bitmap-colorkey");
        red_fixture(&env);
        env.run("var b = new Bitmap('testimg', 0x00ff00ff);")
            .unwrap();
        assert_eq!(env.eval_int("b.width"), 4);
        assert_eq!(env.eval_int("b.height"), 2);
    }

    #[test]
    fn pixel_and_mask_accessors_roundtrip() {
        let env = TestEnv::new("bitmap-pixels");
        env.run(
            "var b = new Bitmap(4, 4); \
             b.setPixel(1, 2, 0xff112233); \
             b.setMaskPixel(1, 2, 200);",
        )
        .unwrap();
        assert_eq!(env.eval_int("b.getPixel(1, 2)"), 0x112233);
        assert_eq!(env.eval_int("b.getMaskPixel(1, 2)"), 200);
        // setPixel preserves the mask; setMaskPixel preserves the color.
        env.run("b.setPixel(1, 2, 0xffaabbcc);").unwrap();
        assert_eq!(env.eval_int("b.getMaskPixel(1, 2)"), 200);
        assert_eq!(env.eval_int("b.getPixel(1, 2)"), 0xaabbcc);
    }

    #[test]
    fn out_of_range_pixel_access_throws() {
        let env = TestEnv::new("bitmap-oob");
        env.run("var b = new Bitmap(2, 2);").unwrap();
        env.run(
            "var got = 'no error'; \
             try { b.getPixel(9, 9); } catch (e) { got = 'error'; }",
        )
        .unwrap();
        assert_eq!(env.eval_string("got"), "error");
    }

    #[test]
    fn set_size_keeps_image_and_fills_with_transparent() {
        let env = TestEnv::new("bitmap-setsize");
        env.run(
            "var b = new Bitmap(2, 2); \
             b.setPixel(0, 0, 0xffff0000); \
             b.setMaskPixel(0, 0, 255); \
             b.setSize(4, 4);",
        )
        .unwrap();
        let scene = env.scene();
        let bmp = &scene.bitmaps[0];
        assert_eq!((bmp.width, bmp.height), (4, 4));
        assert_eq!(&bmp.rgba[0..4], &[255, 0, 0, 255], "existing pixel kept");
        // A newly created pixel is transparent black.
        let off = (3 * 4 + 3) * 4;
        assert_eq!(&bmp.rgba[off..off + 4], &[0, 0, 0, 0]);
    }

    #[test]
    fn width_and_height_setters_resize() {
        let env = TestEnv::new("bitmap-dim-set");
        env.run("var b = new Bitmap(4, 4); b.width = 8; b.height = 2;")
            .unwrap();
        assert_eq!(env.eval_int("b.width"), 8);
        assert_eq!(env.eval_int("b.height"), 2);
    }

    #[test]
    fn copy_from_replaces_pixels_and_size() {
        let env = TestEnv::new("bitmap-copyfrom");
        env.run(
            "var src = new Bitmap(3, 5); src.setMaskPixel(0, 0, 77); \
             var dst = new Bitmap(1, 1); dst.copyFrom(src);",
        )
        .unwrap();
        assert_eq!(env.eval_int("dst.width"), 3);
        assert_eq!(env.eval_int("dst.height"), 5);
        assert_eq!(env.eval_int("dst.getMaskPixel(0, 0)"), 77);
    }

    #[test]
    fn buffer_pitch_and_pointers() {
        let env = TestEnv::new("bitmap-buffer");
        env.run("var b = new Bitmap(6, 3);").unwrap();
        assert_eq!(env.eval_int("b.bufferPitch"), 24);
        let buf = env.eval_int("b.buffer");
        assert_ne!(buf, 0, "buffer returns a non-null address");
        assert_eq!(buf, env.eval_int("b.bufferForWrite"));
    }

    #[test]
    fn load_async_transitions_and_sizes() {
        let env = TestEnv::new("bitmap-async");
        red_fixture(&env);
        env.run("var b = new Bitmap(); b.loadAsync('testimg');")
            .unwrap();
        wait_async(&env, 5000);
        assert_eq!(env.eval_int("b.loading"), 0);
        assert_eq!(env.eval_int("b.width"), 4);
        assert_eq!(env.eval_int("b.height"), 2);
    }

    #[test]
    fn load_async_dispatches_on_loaded() {
        let env = TestEnv::new("bitmap-async-event");
        red_fixture(&env);
        env.run(
            "var got = 'none'; \
             class Probe extends Bitmap { \
               function Probe() { Bitmap(); } \
               function onLoaded(meta, async, error, message) { \
                 got = 'loaded:' + (error ? 'err' : 'ok') + ':' + meta.width; \
               } \
             } \
             var b = new Probe(); b.loadAsync('testimg');",
        )
        .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            async_poll(&env.engine);
            if env.eval_string("got") != "none" {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "onLoaded never fired");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(env.eval_string("got"), "loaded:ok:4");
    }

    #[test]
    fn load_async_error_still_clears_loading() {
        let env = TestEnv::new("bitmap-async-missing");
        env.run(
            "var got = 'none'; \
             class Probe extends Bitmap { \
               function Probe() { Bitmap(); } \
               function onLoaded(meta, async, error, message) { got = 'err:' + error; } \
             } \
             var b = new Probe(); b.loadAsync('nope_missing');",
        )
        .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            async_poll(&env.engine);
            if env.eval_string("got") != "none" {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "onLoaded never fired");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(env.eval_string("got"), "err:1");
        assert_eq!(env.eval_int("b.loading"), 0);
    }

    #[test]
    fn sync_load_returns_metainfo_and_updates_size() {
        let env = TestEnv::new("bitmap-load-sync");
        red_fixture(&env);
        env.run("var b = new Bitmap(1, 1); var meta = b.load('testimg');")
            .unwrap();
        assert_eq!(env.eval_int("meta.width"), 4);
        assert_eq!(env.eval_int("meta.height"), 2);
        assert_eq!(env.eval_int("meta.bpp"), 32);
        assert_eq!(env.eval_int("b.width"), 4);
        assert_eq!(env.eval_int("b.height"), 2);
    }

    #[test]
    fn load_header_reads_dimensions_without_scene_bitmap() {
        let env = TestEnv::new("bitmap-header");
        red_fixture(&env);
        // `Bitmap.loadHeader` is a static member (`BitmapIntf.cpp:378`,
        // closed by TJS_END_NATIVE_STATIC_METHOD_DECL): callable on the
        // class object with no instance.
        env.run("var h = Bitmap.loadHeader('testimg');").unwrap();
        assert_eq!(env.eval_int("h.width"), 4);
        assert_eq!(env.eval_int("h.height"), 2);
        assert_eq!(env.eval_int("h.bpp"), 32);
        assert_eq!(env.scene().bitmaps.len(), 0, "loadHeader creates no bitmap");
    }

    #[test]
    fn static_bitmap_members_are_not_copied_onto_instances() {
        let env = TestEnv::new("bitmap-static-instance");
        red_fixture(&env);
        env.run("var b = new Bitmap();").unwrap();
        // The members live on the class object (TJS_STATICMEMBER), so an
        // instance does not see them; the reference raises member-not-found.
        env.run(
            "var got_header = 'no error'; \
             try { b.loadHeader('testimg'); } catch (e) { got_header = 'error'; } \
             var got_save = 'no error'; \
             try { b.getSaveOption('bmp'); } catch (e) { got_save = 'error'; }",
        )
        .unwrap();
        assert_eq!(env.eval_string("got_header"), "error");
        assert_eq!(env.eval_string("got_save"), "error");
    }

    #[test]
    fn save_png_loads_back_via_storage() {
        let env = TestEnv::new("bitmap-save");
        env.run(
            "var b = new Bitmap(3, 2); b.setPixel(1, 1, 0xffabcdef); \
             b.setMaskPixel(1, 1, 128); b.save('saved/out.png', 'png');",
        )
        .unwrap();
        let path = env._dir.path().join("saved/out.png");
        assert!(Path::new(&path).is_file(), "save wrote the file");
        // Load it back through the same native surface.
        env.run("var c = new Bitmap('saved/out.png');").unwrap();
        assert_eq!(env.eval_int("c.width"), 3);
        assert_eq!(env.eval_int("c.height"), 2);
        assert_eq!(env.eval_int("c.getMaskPixel(1, 1)"), 128);
    }

    #[test]
    fn get_save_option_returns_dictionary_for_known_types() {
        let env = TestEnv::new("bitmap-saveopt");
        // `Bitmap.getSaveOption` is a static member (`BitmapIntf.cpp:403`).
        env.run("var o = Bitmap.getSaveOption('bmp');").unwrap();
        // The bpp option is a dictionary with a "type" key.
        assert_eq!(env.eval_string("o.bpp.type"), "select");
        // PNG exposes an (empty) option dictionary rather than void.
        env.run("var p = Bitmap.getSaveOption('png'); var png_ok = (p !== void);")
            .unwrap();
        assert_eq!(env.eval_int("png_ok"), 1);
    }

    #[test]
    fn exact_color_key_makes_matching_pixels_transparent() {
        let env = TestEnv::new("bitmap-colorkey-exact");
        let (w, h) = (2u32, 1u32);
        let rgba: Vec<u8> = vec![255, 0, 0, 255, 0, 0, 255, 255];
        write_fixture(&env, "ck.webp", &rgba, w, h);
        env.run("var b = new Bitmap('ck', 0x00ff0000);").unwrap();
        assert_eq!(
            env.eval_int("b.getMaskPixel(0, 0)"),
            0,
            "key is transparent"
        );
        assert_eq!(env.eval_int("b.getMaskPixel(1, 0)"), 255, "other is opaque");
        assert_eq!(env.eval_int("b.getPixel(1, 0)"), 0x0000ff);
    }

    #[test]
    fn adaptive_color_key_uses_first_row_majority() {
        let env = TestEnv::new("bitmap-colorkey-adapt");
        // First scanline: red, red, blue → adaptive key = red.
        let (w, h) = (3u32, 1u32);
        let rgba: Vec<u8> = vec![255, 0, 0, 255, 255, 0, 0, 255, 0, 0, 255, 255];
        write_fixture(&env, "cka.webp", &rgba, w, h);
        env.run("var b = new Bitmap('cka', 0x01ffffff);").unwrap();
        assert_eq!(env.eval_int("b.getMaskPixel(0, 0)"), 0);
        assert_eq!(env.eval_int("b.getMaskPixel(1, 0)"), 0);
        assert_eq!(env.eval_int("b.getMaskPixel(2, 0)"), 255);
    }

    #[test]
    fn async_slot_is_reused_across_loads() {
        // Repeated loadAsync on the same Bitmap must not orphan a bitmap per
        // call: the instance-owned bitmap is overwritten in place.
        let env = TestEnv::new("bitmap-async-reuse");
        red_fixture(&env);
        env.run("var b = new Bitmap(); b.loadAsync('testimg');")
            .unwrap();
        wait_async(&env, 5000);
        let after_first = env.scene().bitmaps.len();
        env.run("b.loadAsync('testimg');").unwrap();
        wait_async(&env, 5000);
        assert_eq!(env.eval_int("b.width"), 4);
        assert_eq!(
            env.scene().bitmaps.len(),
            after_first,
            "second async load reuses the owned bitmap"
        );
    }
}
