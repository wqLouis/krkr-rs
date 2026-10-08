//! The visual `Font` native class.
//!
//! # Text metrics (D11)
//!
//! `getTextWidth`/`getTextHeight`, the `getEsc*` rotation variants and
//! `getGlyphDrawRect` are backed by the L2 [`engine::text::TextEngine`]
//! (cosmic-text shaping) rather than by a per-character extent sum. The
//! compat layer has no font-enumeration/loading seam, so a face is registered
//! lazily from the attached [`engine::Storage`] on first use: the face string
//! is read as a storage name. A face that cannot be resolved that way is a
//! typed error, never a fabricated metric. `getGlyphDrawRect` additionally
//! needs per-glyph bitmap bounds, which `engine::text` does not expose, so it
//! is a typed error; the same is true of `defaultFaceName`/`getList` (no font
//! enumeration) and `doUserSelect` (no host font-selection UI).

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};

use engine::FontState;
use engine::text::{FontId, TextEngine, TextStyle};
use tjs::{
    NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, NativeMethodDef,
    NativePropertyDef, NativeStaticMembers, Tjs2Engine, Value,
};

use super::common::{
    DEFAULT_FONT_COLOR, DEFAULT_FONT_FACE, DEFAULT_FONT_HEIGHT, VisualState, argb_to_rgba, bound,
    scene_mut, scene_ref, set_real, set_visual_string_out,
};
use crate::native_util::{args, report_error, set_int, set_void, value_as_string};
use crate::natives::system::{value_as_bool, value_as_i64};
use crate::{CompatCtx, CompatError};

/// `Font` payload: the scene font id. `owns_state` is true while this object
/// created the font itself (`new Font(...)`) and is false after `__bind`
/// repoints it at a layer-owned font.
#[derive(Default)]
struct FontInst {
    id: Option<u32>,
    owns_state: bool,
}

/// Explicit `Font` dispatch state, keyed by type inside
/// [`CompatServices::state`](crate::CompatServices::state).
///
/// * `engine` is the L2 shaping/layout service the measurement methods use;
///   `faces` memoizes the compat face string → registered [`FontId`] mapping
///   so a face is only loaded once.
/// * `prerendered` is the `mapPrerenderedFont` registration (scene font id →
///   storage name); the reference keeps the same font → prerendered-font map.
/// * `rasterizer` is the active rasterizer index. Compat has exactly one
///   rasterizer, index `0` (the L1 `render` glyph rasterizer).
#[derive(Default)]
struct FontMetricsState {
    engine: TextEngine,
    faces: HashMap<String, FontId>,
    prerendered: HashMap<u32, String>,
    rasterizer: i64,
}

/// Borrow the `Font` dispatch state.
fn font_state(ctx: &CompatCtx) -> std::cell::RefMut<'_, FontMetricsState> {
    // SAFETY: the caller resolved `ctx` from the installed binding.
    unsafe { ctx.services() }.state::<FontMetricsState>()
}

/// The font fields the measurement methods need, copied out of the scene so
/// the scene borrow is released before the text engine runs.
struct MeasuredFont {
    face: String,
    size_px: f32,
    bold: bool,
    italic: bool,
    angle: f64,
}

/// Read the measured font fields from the scene for `instance`.
fn measured_font(engine: *mut c_void, instance: *mut c_void) -> Result<MeasuredFont, String> {
    with_font_read(engine, instance, |f| MeasuredFont {
        face: f.face.clone(),
        size_px: f.height.unsigned_abs() as f32,
        bold: f.bold,
        italic: f.italic,
        angle: f.angle,
    })
}

/// Resolve `face` to an L2 [`FontId`], loading it from the attached storage on
/// first use.
///
/// The reference resolves a face name through its enumerated font map. Compat
/// has no enumeration seam, so the only truthful source is the attached
/// storage: `face` is tried as a storage name and parsed by the L2 text
/// engine. Anything else is a typed error (D11).
fn ensure_face(
    ctx: &CompatCtx,
    state: &mut FontMetricsState,
    face: &str,
) -> Result<FontId, String> {
    if let Some(id) = state.faces.get(face).copied() {
        return Ok(id);
    }
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let storage = unsafe { ctx.services() }
        .storage()
        .ok_or_else(|| format!("Font: no storage attached to load face '{face}'"))?;
    // SAFETY: the host keeps the attached storage alive for the binding's
    // lifetime (the `attach_storage` contract).
    let storage = unsafe { &*storage };
    let bytes = storage.read(face).map_err(|error| {
        format!(
            "Font: face '{face}' is not registered with the text engine and cannot be read 
             from storage ({error}); no font enumeration/loading seam is exposed through L2 (D11)"
        )
    })?;
    let id = state
        .engine
        .add_font(&bytes, 0)
        .map_err(|error| format!("Font: face '{face}' is not a usable font: {error}"))?;
    state.faces.insert(face.to_string(), id);
    Ok(id)
}

/// Lay `text` out with the instance's face/size/style and return
/// `(width, height, angle)`.
///
/// The angle is the font's rotation in the reference's tenths-of-a-degree
/// units and is returned unchanged for the `getEsc*` callers.
fn measure_text(
    engine: *mut c_void,
    instance: *mut c_void,
    text: &str,
) -> Result<(f64, f64, f64), String> {
    let font = measured_font(engine, instance)?;
    let (_resolved, ctx) = bound(engine)?;
    let mut state = font_state(&ctx);
    let id = ensure_face(&ctx, &mut state, &font.face)?;
    let mut style = TextStyle::new(font.size_px);
    style.bold = font.bold;
    style.italic = font.italic;
    let layout = state
        .engine
        .layout(id, text, &style, None)
        .map_err(|error| format!("Font: text layout failed: {error}"))?;
    Ok((layout.width as f64, layout.height as f64, font.angle))
}

/// The reference's rotation conversion: the font angle is in tenths of a
/// degree, so `angle * PI / 1800` radians.
fn angle_radians(angle_tenths: f64) -> f64 {
    angle_tenths * std::f64::consts::PI / 1800.0
}

/// See `register_window`.
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

// --- Font text metrics -----------------------------------------------------

/// `font.getTextWidth(text)` — shaped width in pixels.
extern "C" fn font_get_text_width(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Font.getTextWidth requires a string argument");
    }
    let text = value_as_string(&unsafe { args(argv, argc) }[0]);
    match measure_text(engine, instance, &text) {
        Ok((width, _, _)) => {
            set_int(out, width.round() as i64);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `font.getTextHeight(text)` — shaped block height in pixels.
extern "C" fn font_get_text_height(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Font.getTextHeight requires a string argument");
    }
    let text = value_as_string(&unsafe { args(argv, argc) }[0]);
    match measure_text(engine, instance, &text) {
        Ok((_, height, _)) => {
            set_int(out, height.round() as i64);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `font.getEscWidthX(text)` — the text width projected onto the x axis.
extern "C" fn font_get_esc_width_x(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Font.getEscWidthX requires a string argument");
    }
    let text = value_as_string(&unsafe { args(argv, argc) }[0]);
    match measure_text(engine, instance, &text) {
        Ok((width, _, angle)) => {
            set_real(out, angle_radians(angle).cos() * width);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `font.getEscWidthY(text)` — the text width projected onto the y axis.
extern "C" fn font_get_esc_width_y(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Font.getEscWidthY requires a string argument");
    }
    let text = value_as_string(&unsafe { args(argv, argc) }[0]);
    match measure_text(engine, instance, &text) {
        Ok((width, _, angle)) => {
            set_real(out, angle_radians(angle).sin() * -width);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `font.getEscHeightX(text)` — the text height projected onto the x axis.
extern "C" fn font_get_esc_height_x(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Font.getEscHeightX requires a string argument");
    }
    let text = value_as_string(&unsafe { args(argv, argc) }[0]);
    match measure_text(engine, instance, &text) {
        Ok((_, height, angle)) => {
            set_real(out, angle_radians(angle).sin() * height);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `font.getEscHeightY(text)` — the text height projected onto the y axis.
extern "C" fn font_get_esc_height_y(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Font.getEscHeightY requires a string argument");
    }
    let text = value_as_string(&unsafe { args(argv, argc) }[0]);
    match measure_text(engine, instance, &text) {
        Ok((_, height, angle)) => {
            set_real(out, angle_radians(angle).cos() * height);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `font.getGlyphDrawRect(text)` — the reference returns the rasterized
/// glyphs' bounding rectangle. `engine::text` exposes layout positions but
/// not per-glyph bitmap bounds, so this is a typed error (D11) rather than a
/// fabricated rectangle.
extern "C" fn font_get_glyph_draw_rect(
    _engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    _out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(
            out_error,
            "Font.getGlyphDrawRect requires a string argument",
        );
    }
    report_error(
        out_error,
        "Font.getGlyphDrawRect: engine::text does not expose per-glyph draw rectangles (D11)",
    )
}

/// `font.doUserSelect(flags, caption, prompt, samplestring)` — the reference
/// delegates to a host font-selection UI. No such seam exists in compat.
extern "C" fn font_do_user_select(
    _engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    _out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 4 {
        return report_error(
            out_error,
            "Font.doUserSelect requires flags, caption, prompt and samplestring",
        );
    }
    report_error(
        out_error,
        "Font.doUserSelect: no host font-selection UI is exposed through L2 (D11)",
    )
}

/// `font.getList(flags)` — the reference returns the enumerated font faces.
/// Compat has no font-enumeration seam, so this is a typed error (D11).
extern "C" fn font_get_list(
    _engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    _out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Font.getList requires a flags argument");
    }
    report_error(
        out_error,
        "Font.getList: no platform/L2 font enumeration is exposed (D11)",
    )
}

/// `font.mapPrerenderedFont(storage)` — record the prerendered-font storage
/// for this font. The reference maps the font to a prerendered font object;
/// without a prerendered-font subsystem the registration is explicit state
/// (D9) and never affects metrics silently.
extern "C" fn font_map_prerendered_font(
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
            "Font.mapPrerenderedFont requires a storage argument",
        );
    }
    let storage = value_as_string(&unsafe { args(argv, argc) }[0]);
    let inst = unsafe { &*(instance as *const FontInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Font.mapPrerenderedFont: this font is not constructed",
        );
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Font.mapPrerenderedFont: compat context is not bound",
        );
    };
    font_state(&ctx).prerendered.insert(id, storage);
    set_void(out);
    0
}

/// `font.unmapPrerenderedFont()` — drop the prerendered-font registration.
extern "C" fn font_unmap_prerendered_font(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const FontInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Font.unmapPrerenderedFont: this font is not constructed",
        );
    };
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Font.unmapPrerenderedFont: compat context is not bound",
        );
    };
    font_state(&ctx).prerendered.remove(&id);
    set_void(out);
    0
}

// --- Font static members ---------------------------------------------------

/// `Font.rasterizer` (get) — the active rasterizer index.
extern "C" fn font_rasterizer_get(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Font.rasterizer: compat context is not bound");
    };
    set_int(out, font_state(&ctx).rasterizer);
    0
}

/// `Font.rasterizer` (set) — only index `0`, the single L1 `render`
/// rasterizer, is available; anything else is a typed error (D11).
extern "C" fn font_rasterizer_set(
    engine: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let index = value_as_i64(unsafe { &*value });
    if index != 0 {
        return report_error(
            out_error,
            "Font.rasterizer: only rasterizer 0 (the L1 render rasterizer) is available",
        );
    }
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Font.rasterizer: compat context is not bound");
    };
    font_state(&ctx).rasterizer = index;
    0
}

/// `Font.defaultFaceName` (get) — the reference returns the platform default
/// face from its font enumeration. Compat has no enumeration seam, so this is
/// a typed error (D11) rather than a hardcoded name.
extern "C" fn font_default_face_name_get(
    _engine: *mut c_void,
    _out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    report_error(
        out_error,
        "Font.defaultFaceName: no platform/L2 font enumeration is exposed (D11)",
    )
}

/// `Font.defaultFaceName` (set) — the reference deliberately ignores writes
/// (the value comes from the preference/enumeration), so this is a no-op.
extern "C" fn font_default_face_name_set(
    _engine: *mut c_void,
    _value: *const Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    0
}

pub(super) fn register_font_class(ctx: &mut CompatCtx) -> Result<(), CompatError> {
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
            NativeInstanceMethodDef {
                name: "getTextWidth",
                f: font_get_text_width,
            },
            NativeInstanceMethodDef {
                name: "getTextHeight",
                f: font_get_text_height,
            },
            NativeInstanceMethodDef {
                name: "getEscWidthX",
                f: font_get_esc_width_x,
            },
            NativeInstanceMethodDef {
                name: "getEscWidthY",
                f: font_get_esc_width_y,
            },
            NativeInstanceMethodDef {
                name: "getEscHeightX",
                f: font_get_esc_height_x,
            },
            NativeInstanceMethodDef {
                name: "getEscHeightY",
                f: font_get_esc_height_y,
            },
            NativeInstanceMethodDef {
                name: "getGlyphDrawRect",
                f: font_get_glyph_draw_rect,
            },
            NativeInstanceMethodDef {
                name: "doUserSelect",
                f: font_do_user_select,
            },
            NativeInstanceMethodDef {
                name: "getList",
                f: font_get_list,
            },
            NativeInstanceMethodDef {
                name: "mapPrerenderedFont",
                f: font_map_prerendered_font,
            },
            NativeInstanceMethodDef {
                name: "unmapPrerenderedFont",
                f: font_unmap_prerendered_font,
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
    ctx.register_static_members(&NativeStaticMembers {
        class_name: "Font",
        methods: Vec::<NativeMethodDef>::new(),
        properties: vec![
            NativePropertyDef {
                name: "rasterizer",
                get: Some(font_rasterizer_get),
                set: Some(font_rasterizer_set),
            },
            NativePropertyDef {
                name: "defaultFaceName",
                get: Some(font_default_face_name_get),
                set: Some(font_default_face_name_set),
            },
        ],
    })?;
    Ok(())
}
