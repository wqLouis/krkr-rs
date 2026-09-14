//! `__TvpFont` — the native implementation class behind the script `Font`
//! wrapper (see `natives/mod.rs` for the architecture).
//!
//! Minimal per milestone 3A: `new Font(face, height, color)` registers a
//! [`crate::scene::FontState`] and `face`/`height`/`color` are read/write
//! properties. Text rendering (glyph atlases, `getTextWidth`, `drawText`)
//! arrives in milestone 3B; until then `Layer.font` returns a script-side
//! font object with no-op text helpers (see the setup script in `mod.rs`).

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use tjs2_sys::{
    NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, Tjs2Engine, Value,
};

use super::ffi::{
    arg_bool, arg_f64, arg_i64, arg_string, error_out, instance_ref, set_int_out, set_real_out,
    set_string_out,
};
use super::{context_engine, context_scene_mut, context_scene_read};
use tvp_text::{
    EscapedExtents, FaceRequest, FontFace, PrerenderedFont, PrerenderedKey,
    clear_prerendered_fonts, escaped_extents_of, font_config, map_prerendered_font, measure_height,
    measure_width, prerendered_font, resolve_face, with_cached_atlas_styled,
};

/// Default face name for a freshly constructed `Font` (reference's
/// `MS Gothic`-ish default). Also the face a layer's lazily created font
/// starts with before `setFontStyle` overrides it.
pub(crate) const DEFAULT_FONT_FACE: &str = "MS Gothic";
/// Default `Font.height` in pixels.
pub(crate) const DEFAULT_FONT_HEIGHT: i32 = 12;

/// The only font rasterizer index the reference compiles in
/// (`FONT_RASTER_FREE_TYPE`, `LayerBitmapImpl.cpp:62`). The reference exposes
/// `Font.rasterizer` as an index into its rasterizer array; krkr-rs has a
/// single outline rasterizer (`tvp-text`), so its handle is always this value.
const FONT_RASTER_FREE_TYPE: i64 = 0;
/// Number of entries in the reference rasterizer array (`FONT_RASTER_EOT`,
/// `LayerBitmapImpl.cpp:64`). Only index 0 is ever valid here.
const FONT_RASTERIZER_COUNT: i64 = 1;

/// `Font.rasterizer`'s stored index. The reference `TVPSetFontRasterizer`
/// (`LayerBitmapImpl.cpp:94`) rejects out-of-range indices and clears the font
/// cache when the index changes; only index 0 exists, so this stays 0.
static CURRENT_RASTERIZER: AtomicI64 = AtomicI64::new(FONT_RASTER_FREE_TYPE);

/// Per-font `faceIsFileName` flag (`TVP_TF_FONTFILE`), keyed by scene font id
/// for the same reason the reference keeps it in `tTVPFont.Flags`: a
/// `Layer.font` wrapper shares its layer's state, so the flag must be shared
/// by id, not per wrapper object.
static FACE_IS_FILE_NAME: LazyLock<Mutex<HashMap<u32, bool>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Every `(face, height, style)` combination mapped through
/// [`font_map_prerendered`]. `tvp-text`'s registry is private and exposes no
/// single-key removal, so `unmapPrerenderedFont` removes a key by clearing the
/// registry and re-inserting these remaining entries (font.rs is the only
/// producer of mappings; see `font_unmap_prerendered`).
static MAPPED_KEYS: LazyLock<Mutex<HashMap<PrerenderedKey, Arc<PrerenderedFont>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Payload of one script-visible `Font` object.
#[derive(Default)]
pub(crate) struct FontInst {
    /// Scene font id, assigned by the constructor.
    pub id: u32,
    /// Whether the native constructor has run.
    pub constructed: bool,
    /// Whether this object *owns* its [`FontState`](crate::scene::FontState)
    /// and must remove it on destruction.
    ///
    /// A plain `new Font()` owns its state. A wrapper produced by
    /// `Layer.font` is created by `new Font()` too, but immediately rebound to
    /// the layer's shared state via `__bind`, which clears this flag and drops
    /// the throwaway state. Destroying a non-owning wrapper must never delete
    /// the layer's font.
    pub owns_state: bool,
}

/// `new Font(...)` payload factory.
extern "C" fn font_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::<FontInst>::default()) as *mut c_void
}

/// Release a `Font` payload; removes the font from the scene.
extern "C" fn font_destroy(_engine: *mut c_void, instance: *mut c_void) {
    // SAFETY: the trampoline passes the payload from font_create.
    let inst = unsafe { instance_ref::<FontInst>(instance) };
    if inst.constructed && inst.owns_state {
        let mut scene = context_scene_mut();
        scene.fonts.retain(|f| f.id != inst.id);
        FACE_IS_FILE_NAME
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&inst.id);
    }
    // SAFETY: instance came from Box::into_raw.
    unsafe { drop(Box::from_raw(instance as *mut FontInst)) };
}

/// TJS color `0xAARRGGBB` → RGBA (straight alpha), same convention as the
/// layer fill color.
fn argb_to_rgba(color: i64) -> [u8; 4] {
    let c = color as u32;
    [
        ((c >> 16) & 0xff) as u8,
        ((c >> 8) & 0xff) as u8,
        (c & 0xff) as u8,
        ((c >> 24) & 0xff) as u8,
    ]
}

/// `__TvpFont(face, height, color)` — constructor hook. All arguments are
/// optional; the defaults match the reference's default font
/// (`MS Gothic`-ish, 12pt, white). Returns the new font's scene id.
extern "C" fn font_ctor(
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
    let inst = unsafe { instance_ref::<FontInst>(instance) };
    if inst.constructed {
        return error_out(out_error, "Font: this font is already constructed");
    }
    let face = args
        .first()
        .filter(|v| v.ty == tjs2_sys::VAL_STRING)
        .map(arg_string)
        .unwrap_or_else(|| DEFAULT_FONT_FACE.to_string());
    let height = args
        .get(1)
        .map(arg_i64)
        .unwrap_or(i64::from(DEFAULT_FONT_HEIGHT)) as i32;
    let color = args.get(2).map(arg_i64).unwrap_or(0xffffffff);
    let mut scene = context_scene_mut();
    let id = scene.add_font(face, height, argb_to_rgba(color));
    inst.id = id;
    inst.constructed = true;
    inst.owns_state = true;
    set_int_out(out, i64::from(id));
    0
}

/// `__bind(id)` — internal: point this `Font` object at an existing
/// [`FontState`](crate::scene::FontState), dropping the throwaway state the
/// constructor just allocated.
///
/// `Layer.font` creates a fresh wrapper via `new Font()` and rebinds it to the
/// layer's shared state. Property writes (`font.face = ...`) then land on the
/// layer's state, matching the reference's cached `FontObject`, while the
/// wrapper stays disposable (`font_destroy` must not delete the shared state).
extern "C" fn font_bind(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let Some(id) = args.first().map(arg_i64) else {
        return error_out(out_error, "Font.__bind requires a font id");
    };
    let id = id.max(0) as u32;
    let inst = unsafe { instance_ref::<FontInst>(instance) };
    let mut scene = context_scene_mut();
    if scene.font(id).is_none() {
        return error_out(out_error, "Font.__bind: font id does not exist");
    }
    if inst.owns_state && inst.id != id {
        scene.fonts.retain(|f| f.id != inst.id);
        FACE_IS_FILE_NAME
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&inst.id);
    }
    inst.id = id;
    inst.owns_state = false;
    set_void_out(out);
    0
}

/// `face` — font face name (get/set).
extern "C" fn font_face_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<FontInst>(instance) };
    let scene = context_scene_read();
    let Some(font) = scene.fonts.iter().find(|f| f.id == inst.id) else {
        return error_out(out_error, "Font: font no longer exists");
    };
    set_string_out(out, &font.face);
    0
}

extern "C" fn font_face_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value is valid for the call.
    let v = unsafe { &*value };
    let inst = unsafe { instance_ref::<FontInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(font) = scene.fonts.iter_mut().find(|f| f.id == inst.id) else {
        return 1;
    };
    font.face = arg_string(v);
    0
}

/// `height` — font height in pixels (get/set).
extern "C" fn font_height_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<FontInst>(instance) };
    let scene = context_scene_read();
    let Some(font) = scene.fonts.iter().find(|f| f.id == inst.id) else {
        return error_out(out_error, "Font: font no longer exists");
    };
    set_int_out(out, i64::from(font.height));
    0
}

extern "C" fn font_height_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value is valid for the call.
    let v = unsafe { &*value };
    let inst = unsafe { instance_ref::<FontInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(font) = scene.fonts.iter_mut().find(|f| f.id == inst.id) else {
        return 1;
    };
    font.height = arg_i64(v) as i32;
    0
}

/// `color` — font color as `0xAARRGGBB` (get/set).
extern "C" fn font_color_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<FontInst>(instance) };
    let scene = context_scene_read();
    let Some(font) = scene.fonts.iter().find(|f| f.id == inst.id) else {
        return error_out(out_error, "Font: font no longer exists");
    };
    let [r, g, b, a] = font.color;
    let argb = (u32::from(a) << 24) | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
    set_int_out(out, i64::from(argb));
    0
}

extern "C" fn font_color_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value is valid for the call.
    let v = unsafe { &*value };
    let inst = unsafe { instance_ref::<FontInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(font) = scene.fonts.iter_mut().find(|f| f.id == inst.id) else {
        return 1;
    };
    font.color = argb_to_rgba(arg_i64(v));
    0
}

/// `id` — the font's scene id (read-only).
extern "C" fn font_id_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<FontInst>(instance) };
    set_int_out(out, i64::from(inst.id));
    0
}

/// Generate the get/set pair for a boolean `Font` property backed by a
/// [`FontState`](crate::scene::FontState) field. The reference exposes these
/// as TJS booleans; we return integers (0/1), which TJS coerces in `if`.
macro_rules! font_bool_prop {
    ($get_fn:ident, $set_fn:ident, $field:ident) => {
        extern "C" fn $get_fn(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            let inst = unsafe { instance_ref::<FontInst>(instance) };
            let scene = context_scene_read();
            let Some(font) = scene.fonts.iter().find(|f| f.id == inst.id) else {
                return error_out(out_error, "Font: font no longer exists");
            };
            set_int_out(out, i64::from(font.$field));
            0
        }

        extern "C" fn $set_fn(
            _engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: value is valid for the call.
            let v = unsafe { &*value };
            let inst = unsafe { instance_ref::<FontInst>(instance) };
            let mut scene = context_scene_mut();
            let Some(font) = scene.fonts.iter_mut().find(|f| f.id == inst.id) else {
                return 1;
            };
            font.$field = arg_i64(v) != 0;
            0
        }
    };
}

font_bool_prop!(font_bold_get, font_bold_set, bold);
font_bool_prop!(font_italic_get, font_italic_set, italic);
font_bool_prop!(font_strikeout_get, font_strikeout_set, strikeout);
font_bool_prop!(font_underline_get, font_underline_set, underline);

/// `angle` — glyph rotation (real; the game divides by 10 before use).
extern "C" fn font_angle_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<FontInst>(instance) };
    let scene = context_scene_read();
    let Some(font) = scene.fonts.iter().find(|f| f.id == inst.id) else {
        return error_out(out_error, "Font: font no longer exists");
    };
    set_real_out(out, font.angle);
    0
}

extern "C" fn font_angle_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value is valid for the call.
    let v = unsafe { &*value };
    let inst = unsafe { instance_ref::<FontInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(font) = scene.fonts.iter_mut().find(|f| f.id == inst.id) else {
        return 1;
    };
    font.angle = arg_f64(v);
    0
}

/// `faceIsFileName` — interpret `face` as a font file path rather than a face
/// name (reference `TVP_TF_FONTFILE`, `LayerIntf.cpp:11681`, property at
/// `:12142`). The FreeType rasterizer opens the path directly; krkr-rs routes
/// it to `tvp-text`'s `FaceRequest::Path`.
extern "C" fn font_face_is_file_name_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<FontInst>(instance) };
    set_int_out(out, i64::from(face_is_file_name(inst.id)));
    0
}

extern "C" fn font_face_is_file_name_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value is valid for the call.
    let v = unsafe { &*value };
    let inst = unsafe { instance_ref::<FontInst>(instance) };
    FACE_IS_FILE_NAME
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(inst.id, arg_bool(v));
    0
}

/// Snapshot the `(face, height, bold, italic, angle)` key of a scene font,
/// matching the reference `tTVPFont` equality used by the prerendered-font map
/// (`TVPMapPrerenderedFont`, `LayerBitmapImpl.cpp:127`).
fn scene_font_key(id: u32) -> Option<PrerenderedKey> {
    context_scene_read()
        .fonts
        .iter()
        .find(|f| f.id == id)
        .map(|f| PrerenderedKey {
            face: f.face.clone(),
            height: f.height,
            bold: f.bold,
            italic: f.italic,
            angle: f.angle as i32,
        })
}

/// Read the shared `faceIsFileName` flag for a font id.
fn face_is_file_name(id: u32) -> bool {
    FACE_IS_FILE_NAME
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&id)
        .copied()
        .unwrap_or(false)
}

/// Resolve the outline face for `key` the way the reference `ApplyFont` does:
/// a `TVP_TF_FONTFILE` font opens `face` as a path, otherwise it is a
/// name/config lookup. `KRKR_RS_SYSTEM_FONT` is the port's explicit hermetic
/// path override and still wins.
fn resolve_for_font(key: &PrerenderedKey, inst_id: u32) -> Option<Arc<FontFace>> {
    if let Some(path) = std::env::var_os("KRKR_RS_SYSTEM_FONT").map(PathBuf::from) {
        return resolve_face(&FaceRequest::Path(path));
    }
    if face_is_file_name(inst_id) {
        return resolve_face(&FaceRequest::Path(PathBuf::from(&key.face)));
    }
    resolve_face(&FaceRequest::Named(key.face.clone()))
}

/// The pixel width `getTextWidth` reports: the mapped `.tft`'s baked advances
/// when one is installed, otherwise `tvp-text`'s `measure_width` (per-glyph
/// rounded advances, missing glyphs as the pixel height — see `measure.rs`).
/// Shared with `getEsc*` so `getEscWidthX == cos(angle) * getTextWidth`,
/// matching the reference `tTVPNativeBaseBitmap::GetEscWidthX`
/// (`LayerBitmapImpl.cpp:1485`).
fn effective_width(text: &str, key: &PrerenderedKey, inst_id: u32) -> u32 {
    let height = key.height.max(1) as f32;
    let face = resolve_for_font(key, inst_id);
    match prerendered_font(key) {
        Some(pfont) => text
            .chars()
            .map(|c| match pfont.find(c) {
                Some(g) => g.advance().max(0) as u32,
                None => face
                    .as_ref()
                    .map_or(0, |f| measure_width(&c.to_string(), f, height)),
            })
            .sum(),
        None => face.as_ref().map_or(0, |f| measure_width(text, f, height)),
    }
}

/// The pixel width of `text`, measured with the reference procedure (see
/// [`effective_width`]). The old `half_or_full` estimate is gone: the message
/// layer advances its cursor with this call, so it must match the drawn
/// glyphs.
extern "C" fn font_text_width(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let Some(text) = args.first().map(arg_string) else {
        return error_out(out_error, "Font.getTextWidth requires text");
    };
    let inst = unsafe { instance_ref::<FontInst>(instance) };
    let Some(key) = scene_font_key(inst.id) else {
        return error_out(out_error, "Font: font no longer exists");
    };
    set_real_out(out, f64::from(effective_width(&text, &key, inst.id)));
    0
}

extern "C" fn font_text_height(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<FontInst>(instance) };
    // Reference `tTJSNI_Font::GetTextHeight` (`LayerIntf.cpp:11724`) returns
    // `abs(Font.Height)`, independent of the string.
    let height = context_scene_read()
        .fonts
        .iter()
        .find(|f| f.id == inst.id)
        .map(|f| f.height)
        .unwrap_or(DEFAULT_FONT_HEIGHT);
    set_int_out(out, i64::from(measure_height(height)));
    0
}

/// Shared implementation for the four rotated-extent getters; the
/// trigonometry lives in `tvp-text`'s [`escaped_extents_of`]
/// (`measure.rs`, reference `LayerBitmapImpl.cpp:1485-1502`).
fn font_escaped(inst_id: u32, text: &str) -> Result<EscapedExtents, &'static str> {
    let Some(key) = scene_font_key(inst_id) else {
        return Err("Font: font no longer exists");
    };
    let width = effective_width(text, &key, inst_id);
    Ok(escaped_extents_of(
        width,
        measure_height(key.height),
        key.angle,
    ))
}

/// Generate one `getEsc*` method callback (reference
/// `LayerIntf.cpp:11860-11910`).
macro_rules! font_esc_method {
    ($name:ident, $field:ident) => {
        extern "C" fn $name(
            _engine: *mut c_void,
            instance: *mut c_void,
            argc: c_int,
            argv: *const Value,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            let args = unsafe { super::ffi::args(argc, argv) };
            let Some(text) = args.first().map(arg_string) else {
                return error_out(out_error, "Font.getEsc* requires text");
            };
            let inst = unsafe { instance_ref::<FontInst>(instance) };
            match font_escaped(inst.id, &text) {
                Ok(extents) => {
                    set_real_out(out, extents.$field);
                    0
                }
                Err(message) => error_out(out_error, message),
            }
        }
    };
}

font_esc_method!(font_esc_width_x, width_x);
font_esc_method!(font_esc_width_y, width_y);
font_esc_method!(font_esc_height_x, height_x);
font_esc_method!(font_esc_height_y, height_y);

/// Write a retained object-expression result into `*out` (`VAL_RETAINED`).
fn eval_object_out(engine: &Tjs2Engine, expr: &str, out: *mut Value) -> bool {
    match engine.eval_retained(expr, "font.getGlyphDrawRect") {
        Ok(tjs2_sys::RetainedValue::Object(dv)) => {
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
            true
        }
        _ => false,
    }
}

/// `getGlyphDrawRect(text)` — the union of every glyph's ink box, in the
/// reference coordinate space (x = pen offset, y = distance from the line top;
/// reference `FreeTypeFontRasterizer::GetGlyphDrawRect`, `FreeTypeFontRasterizer.cpp:225`,
/// driven by `tFreeTypeFace::GetGlyphRectFromCharcode`, `FreeType.cpp:536`).
///
/// The reference always uses the outline rasterizer here (never a mapped
/// `.tft`), and falls back to the default glyph for a character with no glyph
/// (`FreeTypeFontRasterizer.cpp:230-237`); `GlyphAtlas::rasterize_char`
/// performs the same U+FFFD -> `.notdef` fallback. The returned object carries
/// the reference Rect members (`left`/`top`/`right`/`bottom`, plus
/// `width`/`height`).
///
/// Two reference details are not modelled because `tvp-text` does not expose
/// the needed metrics: the ink left/right use the pen position instead of
/// `pen + horiBearingX` (`GlyphSlot` carries `bearing_y` only — this matches
/// the port's own `paint_layout`, which also draws at the pen), and the
/// underline/strikeout rule expansion (`FreeType.cpp:562-585`) is omitted
/// (the font's rule position/thickness are unavailable).
extern "C" fn font_glyph_draw_rect(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let Some(text) = args.first().map(arg_string) else {
        return error_out(out_error, "Font.getGlyphDrawRect requires text");
    };
    let inst = unsafe { instance_ref::<FontInst>(instance) };
    let Some(key) = scene_font_key(inst.id) else {
        return error_out(out_error, "Font: font no longer exists");
    };
    let Some(face) = resolve_for_font(&key, inst.id) else {
        return error_out(
            out_error,
            "Font.getGlyphDrawRect: no font face could be resolved",
        );
    };
    let pixel_height = key.height.max(1) as u32;
    let (left, top, right, bottom) =
        with_cached_atlas_styled(face, pixel_height, key.bold, |atlas| {
            let ascent = atlas.ascent();
            let mut area: Option<(i32, i32, i32, i32)> = None;
            let mut pen = 0_i32;
            for ch in text.chars() {
                if ch == '\0' {
                    break;
                }
                let probe = if ch.is_control() { '\u{FFFD}' } else { ch };
                let slot = atlas.rasterize_char(probe);
                let t = (ascent - slot.bearing_y as f32).round() as i32;
                // `GlyphSlot` exposes `bearing_y` but not `horiBearingX`, so
                // the ink left is the pen position (see the fn doc).
                let l = pen;
                let r = l + slot.w as i32;
                let b = t + slot.h as i32;
                area = Some(match area {
                    Some((al, at, ar, ab)) => (al.min(l), at.min(t), ar.max(r), ab.max(b)),
                    None => (l, t, r, b),
                });
                pen += slot.advance.round() as i32;
            }
            area.unwrap_or((0, 0, 0, 0))
        });
    let rect_w = (right - left).max(0);
    let rect_h = (bottom - top).max(0);
    let expr = format!(
        "%[\"left\"=>{left}, \"top\"=>{top}, \"right\"=>{right}, \"bottom\"=>{bottom}, \
         \"width\"=>{rect_w}, \"height\"=>{rect_h}]"
    );
    if eval_object_out(context_engine(), &expr, out) {
        0
    } else {
        error_out(
            out_error,
            "Font.getGlyphDrawRect: cannot build the rect result",
        )
    }
}

/// `doUserSelect(flags, caption, prompt, sample)` — the reference font-picker
/// hook. The reference body gates the host dialog behind `#if 0`
/// (`LayerIntf.cpp:11931-11956`), so its own result is always `0`; krkr-rs has
/// no font-selection dialog host and returns the same value.
extern "C" fn font_do_user_select(
    _engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 4 {
        return error_out(out_error, "Font.doUserSelect requires 4 arguments");
    }
    set_int_out(out, 0);
    0
}

thread_local! {
    static ARRAY_OUT: std::cell::RefCell<(Vec<*const c_char>, Vec<Vec<u8>>)> =
        const { std::cell::RefCell::new((Vec::new(), Vec::new())) };
}

/// Write an array-of-strings return value (`VAL_ARRAY`) into `*out`. The
/// pointer/byte buffers are thread-local and live until the next native call.
fn set_array_out(out: *mut Value, values: &[String]) {
    ARRAY_OUT.with(|slot| {
        let mut slot = slot.borrow_mut();
        slot.1.clear();
        slot.0.clear();
        for value in values {
            let mut bytes = value.as_bytes().to_vec();
            bytes.push(0);
            slot.1.push(bytes);
        }
        let pointers: Vec<*const c_char> = slot
            .1
            .iter()
            .map(|bytes| bytes.as_ptr() as *const c_char)
            .collect();
        slot.0.extend(pointers);
        // SAFETY: out is a valid result slot and both buffers stay alive in
        // the thread-local until the next native call.
        unsafe {
            (*out).ty = tjs2_sys::VAL_ARRAY;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = std::ptr::null();
            (*out).array = slot.0.as_ptr();
            (*out).array_count = slot.0.len() as c_int;
        }
    });
}

/// `getList(flags)` — the available face names. The reference iterates its
/// enumerated font-name table `TVPFontNames` (`TVPGetAllFontList`,
/// `FontImpl.cpp:21`); krkr-rs' analogous sources are the installed font
/// config's `faces` keys, the effective default face, and the faces of the
/// currently mapped `.tft` pre-rendered fonts.
extern "C" fn font_get_list(
    _engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.is_empty() {
        return error_out(out_error, "Font.getList requires a flags argument");
    }
    let _flags = arg_i64(&args[0]);
    let mut names: Vec<String> = Vec::new();
    if let Some(config) = font_config() {
        names.extend(config.faces.keys().cloned());
    }
    names.push(DEFAULT_FONT_FACE.to_string());
    {
        let mapped = MAPPED_KEYS.lock().unwrap_or_else(|p| p.into_inner());
        names.extend(mapped.keys().map(|key| key.face.clone()));
    }
    names.sort();
    names.dedup();
    set_array_out(out, &names);
    0
}

/// Cache of parsed `.tft` fonts keyed by the storage name they were loaded
/// from, so `PrerenderedFontInit` (which maps every size of every face) never
/// parses the same multi-hundred-KB file twice.
static PRERENDERED_CACHE: LazyLock<Mutex<HashMap<String, Arc<PrerenderedFont>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Load and parse a `.tft` from game storage, caching by storage name.
fn load_prerendered_cached(name: &str) -> Result<Arc<PrerenderedFont>, String> {
    if let Some(font) = PRERENDERED_CACHE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(name)
        .cloned()
    {
        return Ok(font);
    }
    let bytes = {
        let mut storage = super::context_storage();
        storage
            .read(name)
            .or_else(|_| storage.read(&format!("{name}.tft")))
            .map_err(|e| format!("cannot read prerendered font: {e}"))?
    };
    let font = Arc::new(PrerenderedFont::from_bytes(bytes).map_err(|e| e.to_string())?);
    PRERENDERED_CACHE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(name.to_string(), font.clone());
    Ok(font)
}

/// `Font.mapPrerenderedFont(file)` — load a TVP `.tft` pre-rendered bitmap
/// font from game storage and map it to this font's properties. Subsequent
/// `Layer.drawText` calls whose layer font matches those properties composite
/// the pre-rendered glyphs instead of outlining a system face.
///
/// The mapping is keyed by `(face, height, bold, italic, angle)` exactly like
/// the reference `TVPPrerenderedFontMapVector`, so the mapping registered on a
/// throwaway `new Font()` in `PrerenderedFontInit` applies to every later font
/// with the same properties (including a layer's tracked font).
extern "C" fn font_map_prerendered(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const tjs2_sys::Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let args = unsafe { super::ffi::args(argc, argv) };
    let Some(name) = args.first().map(arg_string) else {
        return error_out(out_error, "Font.mapPrerenderedFont requires a file name");
    };
    let inst = unsafe { instance_ref::<FontInst>(instance) };
    let key = {
        let scene = context_scene_read();
        let Some(font) = scene.fonts.iter().find(|f| f.id == inst.id) else {
            return error_out(out_error, "Font: font no longer exists");
        };
        PrerenderedKey {
            face: font.face.clone(),
            height: font.height,
            bold: font.bold,
            italic: font.italic,
            angle: font.angle as i32,
        }
    };
    match load_prerendered_cached(&name) {
        Ok(font) => {
            MAPPED_KEYS
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(key.clone(), font.clone());
            map_prerendered_font(key, font);
            set_void_out(out);
            0
        }
        Err(e) => error_out(out_error, &format!("Font.mapPrerenderedFont({name}): {e}")),
    }
}

/// `unmapPrerenderedFont()` — undo `mapPrerenderedFont` for this font's
/// `(face, height, style)` key (reference `TVPUnmapPrerenderedFont`,
/// `LayerBitmapImpl.cpp:169`). `tvp-text` exposes no single-key removal, so
/// this rebuilds its registry from the remaining [`MAPPED_KEYS`]; font.rs is
/// the only producer of mappings, and the VM is serialized, so no reader
/// observes the rebuild window.
extern "C" fn font_unmap_prerendered(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<FontInst>(instance) };
    let Some(key) = scene_font_key(inst.id) else {
        return error_out(out_error, "Font: font no longer exists");
    };
    let mut mapped = MAPPED_KEYS.lock().unwrap_or_else(|p| p.into_inner());
    let tracked = mapped.remove(&key).is_some();
    // Only rebuild when the key was actually present in `tvp-text`'s registry:
    // tests may have called `clear_prerendered_fonts` directly, and rebuilding
    // would otherwise resurrect entries the caller deliberately dropped.
    if tracked && prerendered_font(&key).is_some() {
        clear_prerendered_fonts();
        for (other_key, other_font) in mapped.iter() {
            map_prerendered_font(other_key.clone(), other_font.clone());
        }
    }
    drop(mapped);
    set_void_out(out);
    0
}

/// `Font.rasterizer` (static) — the current rasterizer handle. The reference
/// exposes an index into `TVPFontRasterizers`; krkr-rs compiles one outline
/// rasterizer, so the handle is always [`FONT_RASTER_FREE_TYPE`] (0).
extern "C" fn font_rasterizer_get(
    _engine: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    set_int_out(out, CURRENT_RASTERIZER.load(Ordering::Relaxed));
    0
}

/// `Font.rasterizer = index` (static). Reference `TVPSetFontRasterizer`
/// (`LayerBitmapImpl.cpp:94`) ignores out-of-range indices and clears the font
/// cache when the valid index changes. Only index 0 exists, so the store is
/// the same value (and there is nothing to re-rasterize).
extern "C" fn font_rasterizer_set(
    _engine: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: value is valid for the call.
    let index = arg_i64(unsafe { &*value });
    if (0..FONT_RASTERIZER_COUNT).contains(&index) {
        CURRENT_RASTERIZER.store(index, Ordering::Relaxed);
    }
    0
}

/// `Font.defaultFaceName` (static, read-only from script) — the process
/// default face used by a freshly constructed `Font` (reference
/// `TVPGetDefaultFontName`, `FontImpl.cpp:20`; property `LayerIntf.cpp:12176`).
extern "C" fn font_default_face_name_get(
    _engine: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    set_string_out(out, DEFAULT_FONT_FACE);
    0
}

/// The reference setter deliberately ignores the assignment ("don't override,
/// specified by preference", `LayerIntf.cpp:12183`).
extern "C" fn font_default_face_name_set(
    _engine: *mut c_void,
    _value: *const Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    0
}

fn set_void_out(out: *mut Value) {
    // SAFETY: out is a valid result slot.
    unsafe {
        (*out).ty = tjs2_sys::VAL_VOID;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = std::ptr::null();
    }
}

pub(crate) fn register_font(engine: &Tjs2Engine) -> Result<(), String> {
    engine.register_native_class_instance(&NativeInstanceBuilder {
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
            NativeInstanceMethodDef {
                name: "mapPrerenderedFont",
                f: font_map_prerendered,
            },
            NativeInstanceMethodDef {
                name: "unmapPrerenderedFont",
                f: font_unmap_prerendered,
            },
            NativeInstanceMethodDef {
                name: "getTextWidth",
                f: font_text_width,
            },
            NativeInstanceMethodDef {
                name: "getTextHeight",
                f: font_text_height,
            },
            NativeInstanceMethodDef {
                name: "getEscWidthX",
                f: font_esc_width_x,
            },
            NativeInstanceMethodDef {
                name: "getEscWidthY",
                f: font_esc_width_y,
            },
            NativeInstanceMethodDef {
                name: "getEscHeightX",
                f: font_esc_height_x,
            },
            NativeInstanceMethodDef {
                name: "getEscHeightY",
                f: font_esc_height_y,
            },
            NativeInstanceMethodDef {
                name: "getGlyphDrawRect",
                f: font_glyph_draw_rect,
            },
            NativeInstanceMethodDef {
                name: "doUserSelect",
                f: font_do_user_select,
            },
            NativeInstanceMethodDef {
                name: "getList",
                f: font_get_list,
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
    })?;
    // `rasterizer` and `defaultFaceName` are `TJS_END_NATIVE_STATIC_PROP_DECL`
    // members in the reference (`LayerIntf.cpp:12162` / `:12176`): they live
    // on the class object (`Font.rasterizer`), not on instances.
    engine.register_native_static_members(&tjs2_sys::NativeStaticMembers {
        class_name: "Font",
        methods: vec![],
        properties: vec![
            tjs2_sys::NativePropertyDef {
                name: "rasterizer",
                get: Some(font_rasterizer_get),
                set: Some(font_rasterizer_set),
            },
            tjs2_sys::NativePropertyDef {
                name: "defaultFaceName",
                get: Some(font_default_face_name_get),
                set: Some(font_default_face_name_set),
            },
        ],
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use tjs2_sys::TjsValue;

    use crate::natives::tests::TestEnv;

    use super::{FACE_IS_FILE_NAME, MAPPED_KEYS};

    /// The repo's bundled Noto CJK collection; deterministic across machines
    /// (unlike a system font), so the rasterizing members have a real face to
    /// measure at any CI host that has the checkout.
    const NOTO_TTC: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../reference/ui/cocos-studio/NotoSansCJK-Regular.ttc"
    );

    /// Restore the process-global font config / prerendered registry that the
    /// new members touch. Tests hold the shared `vm_test_lock` via `TestEnv`,
    /// so this is the only writer.
    struct FontGlobalGuard;

    impl FontGlobalGuard {
        fn reset() {
            tvp_text::set_font_config(None);
            tvp_text::clear_prerendered_fonts();
            MAPPED_KEYS
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clear();
            FACE_IS_FILE_NAME
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clear();
        }
    }

    impl Drop for FontGlobalGuard {
        fn drop(&mut self) {
            Self::reset();
        }
    }

    /// Create an env (taking the VM lock) with `TestFace` mapped to the
    /// bundled CJK face, then run `f` and restore the globals.
    fn with_test_face(name: &str, f: impl FnOnce(&TestEnv)) {
        let env = TestEnv::new(name);
        FontGlobalGuard::reset();
        let _guard = FontGlobalGuard;
        let mut faces = HashMap::new();
        faces.insert(
            "TestFace".to_string(),
            tvp_text::FontEntry::Path(NOTO_TTC.to_string()),
        );
        tvp_text::set_font_config(Some(tvp_text::FontConfig {
            faces,
            fallback: vec![],
            allow_system_discovery: false,
        }));
        f(&env);
    }

    fn eval_real(env: &TestEnv, expr: &str) -> f64 {
        match env.eval(expr, "test") {
            Ok(TjsValue::Real(v)) => v,
            Ok(TjsValue::Integer(v)) => v as f64,
            other => panic!("eval {expr:?} -> {other:?}"),
        }
    }

    /// Build a minimal version-1 `.tft` with one solid 2×2 glyph for `ch`.
    fn tiny_tft(ch: char) -> Vec<u8> {
        const MAGIC: &[u8; 22] = b"TVP pre-rendered font\x1a";
        const HEADER: usize = 36;
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
        item[4..6].copy_from_slice(&2u16.to_le_bytes());
        item[6..8].copy_from_slice(&2u16.to_le_bytes());
        item[10..12].copy_from_slice(&2i16.to_le_bytes());
        item[12..14].copy_from_slice(&3i16.to_le_bytes());
        item[16..18].copy_from_slice(&3i16.to_le_bytes());
        data
    }

    #[test]
    fn font_constructor_and_properties() {
        let env = TestEnv::new("font-ctor");
        env.run(
            "var f = new Font('MS 明朝', 16, 0xffff0000); f.face = 'MS Gothic'; f.height = 18;",
        )
        .unwrap();
        let scene = env.scene();
        assert_eq!(scene.fonts.len(), 1);
        let font = &scene.fonts[0];
        assert_eq!(font.face, "MS Gothic");
        assert_eq!(font.height, 18);
        assert_eq!(font.color, [255, 0, 0, 255]);
        assert_eq!(env.eval_string("f.face"), "MS Gothic");
        assert_eq!(env.eval_int("f.height"), 18);
        assert_eq!(env.eval_int("f.id"), i64::from(font.id));
    }

    #[test]
    fn font_defaults() {
        let env = TestEnv::new("font-defaults");
        env.run("var f = new Font();").unwrap();
        let scene = env.scene();
        assert_eq!(scene.fonts[0].face, "MS Gothic");
        assert_eq!(scene.fonts[0].height, 12);
        assert_eq!(scene.fonts[0].color, [255, 255, 255, 255]);
    }

    #[test]
    fn font_destroy_removes_from_scene() {
        let env = TestEnv::new("font-destroy");
        // One-shot create+null (like the tjs2-sys destroy test): the VM
        // releases the object synchronously within the script, so the
        // native destroy runs and removes the font from the scene.
        env.run("var f = new Font(); f = null;").unwrap();
        assert_eq!(env.scene().fonts.len(), 0);
    }

    #[test]
    fn font_static_rasterizer_and_default_face_name() {
        let env = TestEnv::new("font-static-props");
        // The reference's default font name is read-only from script even
        // though the setter exists.
        assert_eq!(env.eval_string("Font.defaultFaceName"), "MS Gothic");
        env.run("Font.defaultFaceName = 'NoOverride';").unwrap();
        assert_eq!(env.eval_string("Font.defaultFaceName"), "MS Gothic");
        // One rasterizer (FreeType-equivalent): the handle is 0 and another
        // index is rejected, leaving the current one in place.
        assert_eq!(env.eval_int("Font.rasterizer"), 0);
        env.run("Font.rasterizer = 7;").unwrap();
        assert_eq!(env.eval_int("Font.rasterizer"), 0);
    }

    #[test]
    fn font_face_is_file_name_round_trips() {
        let env = TestEnv::new("font-face-is-filename");
        env.run("var f = new Font();").unwrap();
        assert_eq!(env.eval_int("f.faceIsFileName"), 0);
        env.run("f.faceIsFileName = true;").unwrap();
        assert_eq!(env.eval_int("f.faceIsFileName"), 1);
        env.run("f.faceIsFileName = false;").unwrap();
        assert_eq!(env.eval_int("f.faceIsFileName"), 0);
    }

    #[test]
    fn font_text_height_is_absolute() {
        let env = TestEnv::new("font-text-height-abs");
        // The constructor stores the requested height verbatim; the reference
        // `getTextHeight` reports `abs(Font.Height)`.
        env.run("var f = new Font('TestFace', -24);").unwrap();
        assert_eq!(env.eval_int("f.getTextHeight('ignored')"), 24);
    }

    #[test]
    fn font_esc_extents_match_the_rotation() {
        with_test_face("font-esc-extents", |env| {
            env.run("var f = new Font('TestFace', 32);").unwrap();
            let width = eval_real(env, "f.getTextWidth('A')");
            assert!(width > 0.0, "the bundled face must measure 'A'");
            // angle 0: width on X, height on Y, nothing on the crossed axes.
            assert_eq!(eval_real(env, "f.getEscWidthX('A')"), width);
            assert!(eval_real(env, "f.getEscWidthY('A')").abs() < 1e-9);
            assert!(eval_real(env, "f.getEscHeightX('A')").abs() < 1e-9);
            assert_eq!(eval_real(env, "f.getEscHeightY('A')"), 32.0);
            // angle 900 tenths = 90 degrees: the width rotates onto -Y and the
            // height onto +X (reference `LayerBitmapImpl.cpp:1485-1502`).
            env.run("f.angle = 900;").unwrap();
            assert!((eval_real(env, "f.getEscWidthY('A')") + width).abs() < 1e-9);
            assert!((eval_real(env, "f.getEscHeightX('A')") - 32.0).abs() < 1e-9);
        });
    }

    #[test]
    fn font_glyph_draw_rect_is_the_ink_union() {
        with_test_face("font-glyph-draw-rect", |env| {
            env.run("var f = new Font('TestFace', 32); var r = f.getGlyphDrawRect('A');")
                .unwrap();
            let left = env.eval_int("r.left");
            let top = env.eval_int("r.top");
            let right = env.eval_int("r.right");
            let bottom = env.eval_int("r.bottom");
            assert!(left <= right, "{left}..{right}");
            assert!(top <= bottom, "{top}..{bottom}");
            assert!(right - left > 0, "ASCII 'A' must have ink width");
            assert!(bottom - top > 0, "ASCII 'A' must have ink height");
            assert_eq!(env.eval_int("r.width"), right - left);
            assert_eq!(env.eval_int("r.height"), bottom - top);
            // The ink top is above the baseline (ascent = 32).
            assert!(top < 32, "ink top {top} must be above the baseline");
            // Empty text leaves the reference rect at all zeros.
            env.run("var e = f.getGlyphDrawRect('');").unwrap();
            assert_eq!(env.eval_int("e.left"), 0);
            assert_eq!(env.eval_int("e.top"), 0);
            assert_eq!(env.eval_int("e.right"), 0);
            assert_eq!(env.eval_int("e.bottom"), 0);
        });
    }

    #[test]
    fn font_get_list_and_unmap_prerendered() {
        with_test_face("font-get-list-unmap", |env| {
            std::fs::write(env._dir.path().join("testfont.tft"), tiny_tft('A')).unwrap();
            std::fs::write(env._dir.path().join("testfont2.tft"), tiny_tft('B')).unwrap();
            env.run(
                "var f = new Font('Gamma', 30, 0xffffff); f.mapPrerenderedFont('testfont.tft'); \
                 var f2 = new Font('Delta', 30, 0xffffff); f2.mapPrerenderedFont('testfont2.tft');",
            )
            .unwrap();
            env.run(
                "var names = f.getList(0); var hasFace = false; var hasMapped = false; \
                 var hasDefault = false; \
                 for (var i = 0; i < names.count; i = i + 1) { \
                     if (names[i] == 'TestFace') hasFace = true; \
                     if (names[i] == 'Gamma') hasMapped = true; \
                     if (names[i] == 'MS Gothic') hasDefault = true; \
                 }",
            )
            .unwrap();
            assert_eq!(env.eval_int("hasFace"), 1, "configured faces are listed");
            assert_eq!(env.eval_int("hasMapped"), 1, "mapped .tft faces are listed");
            assert_eq!(env.eval_int("hasDefault"), 1, "the default face is listed");

            let gamma = tvp_text::PrerenderedKey {
                face: "Gamma".into(),
                height: 30,
                bold: false,
                italic: false,
                angle: 0,
            };
            let delta = tvp_text::PrerenderedKey {
                face: "Delta".into(),
                ..gamma.clone()
            };
            assert!(tvp_text::prerendered_font(&gamma).is_some());
            assert!(tvp_text::prerendered_font(&delta).is_some());

            env.run("f.unmapPrerenderedFont();").unwrap();
            assert!(
                tvp_text::prerendered_font(&gamma).is_none(),
                "unmap must remove the font's own mapping"
            );
            assert!(
                tvp_text::prerendered_font(&delta).is_some(),
                "unmap must keep other mappings"
            );
            env.run(
                "var names2 = f.getList(0); var stillMapped = false; \
                 for (var i = 0; i < names2.count; i = i + 1) \
                     if (names2[i] == 'Gamma') stillMapped = true;",
            )
            .unwrap();
            assert_eq!(env.eval_int("stillMapped"), 0);
        });
    }

    #[test]
    fn font_do_user_select_returns_zero() {
        let env = TestEnv::new("font-do-user-select");
        env.run("var f = new Font();").unwrap();
        assert_eq!(
            env.eval_int("f.doUserSelect(0, 'caption', 'prompt', 'sample')"),
            0
        );
    }
}
