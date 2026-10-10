//! The visual `Font` native class.
//!
//! # Text metrics
//!
//! `getTextWidth`/`getTextHeight`, the `getEsc*` rotation variants and
//! `getGlyphDrawRect` are backed by the L2 [`engine::text::TextEngine`]
//! (cosmic-text shaping) rather than by a per-character extent sum. A face is
//! resolved through the shared [`crate::resource::ResourceResolver`]: the
//! `System.addFont` registry, then the attached [`engine::Storage`] (the face
//! string read as a storage name), then the host's installed system fonts,
//! with the reference's default-face fallback for an unresolved name.
//! `defaultFaceName`/`getList` report the resolver's registered faces;
//! `doUserSelect` still needs a host font-selection UI and raises a typed
//! error.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};

use engine::FontState;
use engine::text::TextStyle;
use tjs::{
    NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, NativeMethodDef,
    NativePropertyDef, NativeStaticMembers, Tjs2Engine, Tjs2ValueId, Value,
};

use super::common::{
    DEFAULT_FONT_COLOR, DEFAULT_FONT_FACE, DEFAULT_FONT_HEIGHT, VisualState, argb_to_rgba, bound,
    scene_mut, scene_ref, set_real, set_visual_string_out,
};
use crate::native_util::{args, report_error, set_int, set_void, value_as_string};
use crate::natives::system::{value_as_bool, value_as_i64};
use crate::{CompatCtx, CompatError};

/// `Font` payload: the scene font id. `owns_state` is true while this object
/// created the font itself (`new Font(...)`) and is false once it is bound to
/// a layer-owned font (the `Layer.font` wrapper).
#[derive(Default)]
struct FontInst {
    id: Option<u32>,
    owns_state: bool,
}

/// The scene id a `Font` payload carries — the reader behind the VM's
/// native-instance lookup (`native_instance_payload("Font")`), replacing
/// the removed `id` script member for engine call sites.
pub(super) fn font_payload_id(payload: *mut c_void) -> Option<u32> {
    // SAFETY: `native_instance_payload("Font")` only hands out Font payloads.
    unsafe { &*(payload as *const FontInst) }.id
}

/// Explicit `Font` dispatch state, keyed by type inside
/// [`CompatServices::state`](crate::CompatServices::state).
///
/// * `prerendered` is the `mapPrerenderedFont` registration (scene font id →
///   storage name); the reference keeps the same font → prerendered-font map.
/// * `rasterizer` is the active rasterizer index. Compat has exactly one
///   rasterizer, index `0` (the L1 `render` glyph rasterizer).
///
/// The shaping/layout engine and the face cache live in the shared
/// [`crate::resource::ResourceResolver`].
#[derive(Default)]
struct FontMetricsState {
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
    let mut resolver = crate::resource::resource_state(&ctx);
    let id = resolver.resolve_face(&ctx, &font.face)?;
    let mut style = TextStyle::new(font.size_px);
    style.bold = font.bold;
    style.italic = font.italic;
    let layout = resolver
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

/// Point an existing `Font` object at a scene font id, taking ownership of
/// the wrapper's previous font. The `Layer.font` getter uses this to bind the
/// wrapper it creates to the layer-owned font state.
pub(super) fn bind_font_object(
    host: &Tjs2Engine,
    ctx: &CompatCtx,
    object: Tjs2ValueId,
    font_id: u32,
) -> Result<(), String> {
    let payload = host
        .native_instance_payload(object, "Font")?
        .ok_or_else(|| "Font: the wrapper is not a Font instance".to_string())?;
    // SAFETY: `native_instance_payload("Font")` only hands out `Font` payloads
    // and `object` keeps the payload alive for the call.
    let inst = unsafe { &mut *(payload.as_ptr() as *mut FontInst) };
    let owned = inst.owns_state;
    let previous = inst.id.replace(font_id);
    inst.owns_state = false;
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    if owned
        && let Some(previous) = previous
        && previous != font_id
    {
        services
            .state::<VisualState>()
            .objects
            .fonts
            .remove(&previous);
        if let Some(scene) = services.scene() {
            // SAFETY: the scene is live for the binding's lifetime.
            let scene = unsafe { &mut *scene };
            scene.fonts.retain(|font| font.id != previous);
        }
    }
    if let Ok(retained) = host.duplicate_retained(object) {
        services
            .state::<VisualState>()
            .objects
            .fonts
            .insert(font_id, retained);
    }
    Ok(())
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

/// Define a read/write `Font` integer property.
macro_rules! font_int_prop {
    ($get:ident, $set:ident, $getter:expr, $setter:expr) => {
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

/// `font.getGlyphDrawRect(text)` — the rasterized glyphs' bounding rectangle
/// in the text's local coordinate space.
extern "C" fn font_get_glyph_draw_rect(
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
            "Font.getGlyphDrawRect requires a string argument",
        );
    }
    let text = value_as_string(&unsafe { args(argv, argc) }[0]);
    let font = match measured_font(engine, instance) {
        Ok(font) => font,
        Err(error) => return report_error(out_error, &error),
    };
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Font.getGlyphDrawRect: compat context is not bound",
        );
    };
    let result = {
        let mut resolver = crate::resource::resource_state(&ctx);
        let id = match resolver.resolve_face(&ctx, &font.face) {
            Ok(id) => id,
            Err(error) => return report_error(out_error, &error),
        };
        let mut style = TextStyle::new(font.size_px);
        style.bold = font.bold;
        style.italic = font.italic;
        resolver.engine.glyph_draw_rect(id, &text, &style, None)
    };
    match result {
        Ok(rect) => {
            set_glyph_rect_out(resolved.engine(), out, rect.unwrap_or((0, 0, 0, 0)));
            0
        }
        Err(error) => report_error(out_error, &format!("Font.getGlyphDrawRect: {error}")),
    }
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
        "Font.doUserSelect: no host font-selection UI is exposed",
    )
}

/// `font.getList(flags)` — the registered face family names as a TJS array
/// (reference `LayerIntf.cpp:11958-11985`).
extern "C" fn font_get_list(
    engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Font.getList requires a flags argument");
    }
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Font.getList: compat context is not bound");
    };
    let names = {
        let mut resolver = crate::resource::resource_state(&ctx);
        resolver.face_names(&ctx)
    };
    set_string_array_out(&ctx, out, &names);
    0
}

/// `font.mapPrerenderedFont(storage)` — record the prerendered-font storage
/// for this font. The reference maps the font to a prerendered font object;
/// without a prerendered-font subsystem the registration is explicit state
/// and never affects metrics silently.
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

/// Marshal a glyph bounding rectangle into a fresh `Rect` object.
fn set_glyph_rect_out(host: &Tjs2Engine, out: *mut Value, rect: (i32, i32, i32, i32)) {
    if out.is_null() {
        return;
    }
    let (left, top, right, bottom) = rect;
    let object = match host.eval_retained("new Rect()", "Font.rect") {
        Ok(tjs::RetainedValue::Object(object)) => object,
        Ok(tjs::RetainedValue::Value(_)) => {
            log::warn!("Font: `new Rect()` did not return an object");
            set_void(out);
            return;
        }
        Err(error) => {
            log::warn!("Font: cannot create the result Rect: {error}");
            set_void(out);
            return;
        }
    };
    for (name, value) in [
        ("left", left),
        ("top", top),
        ("right", right),
        ("bottom", bottom),
    ] {
        if let Err(error) = host.set_member(
            object.raw_id(),
            name,
            &tjs::TjsValue::Integer(i64::from(value)),
        ) {
            log::warn!("Font: cannot set the result Rect.{name}: {error}");
            set_void(out);
            return;
        }
    }
    // SAFETY: the trampoline passes a valid out slot for the call.
    unsafe {
        (*out).ty = tjs::VAL_RETAINED;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = std::ptr::null();
        (*out).array = std::ptr::null();
        (*out).array_count = 0;
        (*out).retained = object.raw_id() as usize;
    }
    std::mem::forget(object);
}

/// Write a TJS string array into `out` (the `Font.getList` result).
fn set_string_array_out(ctx: &CompatCtx, out: *mut Value, values: &[String]) {
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let mut slot = services.linked_array_scratch();
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
    slot.0 = pointers;
    if !out.is_null() {
        // SAFETY: the trampoline passes a valid out slot for the call.
        unsafe {
            (*out).ty = tjs::VAL_ARRAY;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = std::ptr::null();
            (*out).array = slot.0.as_ptr();
            (*out).array_count = slot.0.len() as c_int;
            (*out).retained = 0;
        }
    }
}

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
/// rasterizer, is available; anything else is a typed error.
extern "C" fn font_rasterizer_set(
    engine: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let index = value_as_i64(unsafe { &*value });
    if index != 0 {
        return report_error(
            out_error,
            "Font.rasterizer: only rasterizer 0 (the platform render rasterizer) is available",
        );
    }
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Font.rasterizer: compat context is not bound");
    };
    font_state(&ctx).rasterizer = index;
    0
}

/// `Font.defaultFaceName` (get) — the default face name, or the empty string
/// when no face is registered (reference `LayerIntf.cpp:12177`).
extern "C" fn font_default_face_name_get(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Font.defaultFaceName: compat context is not bound",
        );
    };
    let name = {
        let mut resolver = crate::resource::resource_state(&ctx);
        resolver.default_face_name(&ctx)
    };
    set_visual_string_out(&ctx, out, &name);
    0
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
