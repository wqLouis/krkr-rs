//! The `FontInfo` value type and the text rendering used by
//! `Layer.drawString` / `measureString`.
//!
//! Reference: `reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp`
//! (`class FontInfo`, `drawString`, `measureString`) and the blend2d build.
//! The port resolves the family through the L2 [`engine::text::TextEngine`]
//! (the same shaping service the visual `Font` native uses) and rasterizes
//! glyphs with the L1 glyph rasterizer exposed by the text engine. A face
//! that cannot be loaded is a typed error, never a fabricated metric.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};

use engine::BitmapState;
use engine::text::{FontId, TextEngine, TextStyle};
use tjs::{
    NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, Tjs2Engine,
    TjsValue, Value,
};

use super::appearance::{Paint, paint_color_at};
use super::{set_real, value_as_f64};
use crate::native_util::{args, report_error, set_void};
use crate::{CompatCtx, CompatError};

/// The script-visible fields of a `FontInfo`.
#[derive(Clone, Debug, Default)]
pub(super) struct FontData {
    pub(super) family: String,
    pub(super) em_size: f64,
    pub(super) style: i32,
    pub(super) force_self_path_draw: bool,
}

impl FontData {
    pub(super) fn bold(&self) -> bool {
        self.style & 1 != 0
    }

    pub(super) fn italic(&self) -> bool {
        self.style & 2 != 0
    }
}

/// Shared text shaping/rasterization state, keyed in
/// [`CompatServices::state`](crate::CompatServices::state).
#[derive(Default)]
pub(super) struct TextState {
    pub(super) engine: TextEngine,
    pub(super) faces: HashMap<String, FontId>,
}

/// Borrow the layer_ex shared text state.
pub(super) fn text_state(ctx: &CompatCtx) -> std::cell::RefMut<'_, TextState> {
    // SAFETY: `ctx` was resolved from the installed binding.
    unsafe { ctx.services() }.state::<TextState>()
}

/// Resolve `family` to an L2 [`FontId`], loading it from the attached storage
/// on first use (compat has no font-enumeration seam).
pub(super) fn ensure_face(
    ctx: &CompatCtx,
    state: &mut TextState,
    family: &str,
) -> Result<FontId, String> {
    if let Some(id) = state.faces.get(family).copied() {
        return Ok(id);
    }
    if family.is_empty() {
        return Err("FontInfo: the family name is empty".to_string());
    }
    // SAFETY: the host keeps the attached storage alive for the binding's
    // lifetime (the `attach_storage` contract).
    let storage = unsafe { ctx.services() }
        .storage()
        .ok_or_else(|| format!("FontInfo: no storage attached to load face '{family}'"))?;
    // SAFETY: see above.
    let storage = unsafe { &*storage };
    let bytes = storage.read(family).map_err(|error| {
        format!(
            "FontInfo: face '{family}' is not registered and cannot be read from storage \
             ({error}); no font-enumeration service is exposed"
        )
    })?;
    let id = state
        .engine
        .add_font(&bytes, 0)
        .map_err(|error| format!("FontInfo: face '{family}' is not a usable font: {error}"))?;
    state.faces.insert(family.to_string(), id);
    Ok(id)
}

/// Read a `FontInfo` argument's public properties.
pub(super) fn read_font(host: &Tjs2Engine, value: &Value, what: &str) -> Result<FontData, String> {
    let object = host
        .retain_object_arg(value)
        .map_err(|error| format!("{what}: the font argument is not an object: {error}"))?;
    let family = match host.member(object.raw_id(), "familyName") {
        Ok(TjsValue::String(value)) => value,
        _ => String::new(),
    };
    let em_size = match host.member(object.raw_id(), "emSize") {
        Ok(TjsValue::Integer(value)) => value as f64,
        Ok(TjsValue::Real(value)) => value,
        _ => 12.0,
    };
    let style = match host.member(object.raw_id(), "style") {
        Ok(TjsValue::Integer(value)) => value as i32,
        Ok(TjsValue::Real(value)) => value as i32,
        _ => 0,
    };
    let force_self_path_draw = match host.member(object.raw_id(), "forceSelfPathDraw") {
        Ok(TjsValue::Integer(value)) => value != 0,
        Ok(TjsValue::Real(value)) => value != 0.0,
        _ => false,
    };
    Ok(FontData {
        family,
        em_size: if em_size > 0.0 { em_size } else { 12.0 },
        style,
        force_self_path_draw,
    })
}

/// A rendered run of text at its layout origin plus the tight ink bounds.
pub(super) struct TextBitmap {
    pub(super) bitmap: BitmapState,
    /// 8-bit per-pixel coverage used by `drawPathString`'s pen outline.
    pub(super) coverage: Vec<u8>,
    pub(super) width: f64,
    pub(super) height: f64,
    /// Tight ink box in bitmap coordinates, or `None` when there is no ink.
    pub(super) ink: Option<(f64, f64, f64, f64)>,
}

/// Render `text` in `font` tinted with `paint` into a fresh bitmap whose
/// origin is the text layout origin.
pub(super) fn render_text(
    ctx: &CompatCtx,
    font: &FontData,
    text: &str,
    paint: &Paint,
) -> Result<TextBitmap, String> {
    let mut state = text_state(ctx);
    let face = ensure_face(ctx, &mut state, &font.family)?;
    let mut style = TextStyle::new(font.em_size as f32);
    style.bold = font.bold();
    style.italic = font.italic();
    style.color = [255, 255, 255, 255];
    let layout = state
        .engine
        .layout(face, text, &style, None)
        .map_err(|error| format!("Layer.drawString: text layout failed: {error}"))?;
    let width = layout.width.max(1.0).ceil() as u32;
    let height = layout.height.max(1.0).ceil() as u32;
    let mut coverage = vec![0u8; width as usize * height as usize];
    let mut ink: Option<(f64, f64, f64, f64)> = None;
    for glyph in &layout.glyphs {
        let mask = state
            .engine
            .rasterize_glyph(face, glyph.codepoint, &style)
            .map_err(|error| format!("Layer.drawString: glyph rasterization failed: {error}"))?;
        if mask.is_empty() {
            continue;
        }
        let dest_x = glyph.x as f64 + mask.bearing_x as f64;
        let dest_y = glyph.baseline_y as f64 - mask.bearing_y as f64;
        let origin_x = dest_x.round() as i64;
        let origin_y = dest_y.round() as i64;
        for row in 0..mask.height as i64 {
            let y = origin_y + row;
            if y < 0 || y >= height as i64 {
                continue;
            }
            for column in 0..mask.width as i64 {
                let x = origin_x + column;
                if x < 0 || x >= width as i64 {
                    continue;
                }
                let value = mask.coverage[(row as usize) * mask.width as usize + column as usize];
                let slot = &mut coverage[y as usize * width as usize + x as usize];
                *slot = (*slot).max(value);
                if value > 0 {
                    let glyph_box = (dest_x, dest_y, mask.width as f64, mask.height as f64);
                    ink = Some(match ink {
                        None => glyph_box,
                        Some((x0, y0, w0, h0)) => {
                            let min_x = x0.min(glyph_box.0);
                            let min_y = y0.min(glyph_box.1);
                            let max_x = (x0 + w0).max(glyph_box.0 + glyph_box.2);
                            let max_y = (y0 + h0).max(glyph_box.1 + glyph_box.3);
                            (min_x, min_y, max_x - min_x, max_y - min_y)
                        }
                    });
                }
            }
        }
    }
    let bitmap = tint_coverage(&coverage, width, height, paint);
    Ok(TextBitmap {
        bitmap,
        coverage,
        width: layout.width as f64,
        height: layout.height as f64,
        ink,
    })
}

/// Build a straight-alpha RGBA bitmap from an 8-bit coverage mask, tinted
/// with `paint`. When `paint` is `None`, only the coverage alpha is written.
pub(super) fn tint_coverage(
    coverage: &[u8],
    width: u32,
    height: u32,
    paint: &Paint,
) -> BitmapState {
    let mut rgba = vec![0u8; width as usize * height as usize * 4];
    for y in 0..height {
        for x in 0..width {
            let cov = coverage[y as usize * width as usize + x as usize];
            if cov == 0 {
                continue;
            }
            let color = paint_color_at(paint, x as f64 + 0.5, y as f64 + 0.5);
            let offset = (y as usize * width as usize + x as usize) * 4;
            rgba[offset] = color[0];
            rgba[offset + 1] = color[1];
            rgba[offset + 2] = color[2];
            rgba[offset + 3] = (u32::from(color[3]) * u32::from(cov) / 255) as u8;
        }
    }
    BitmapState::new(width, height, rgba)
}

// ---------------------------------------------------------------------------
// FontInfo class
// ---------------------------------------------------------------------------

/// `FontInfo` payload.
pub(super) struct FontInst {
    pub(super) data: FontData,
}

extern "C" fn create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(FontInst {
        data: FontData::default(),
    })) as *mut c_void
}

extern "C" fn destroy(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: every payload came from `create`'s `Box::into_raw`.
    drop(unsafe { Box::from_raw(instance as *mut FontInst) });
}

/// `FontInfo(familyName, emSize, style)`.
///
/// Reference arity 3: `NCB_CONSTRUCTOR((const tjs_char *, tjs_real,
/// tjs_int))` (`reference/cpp/plugins/layerex_draw/windows/main.cpp:584`,
/// blend2d `LayerExDraw.cpp:3419`), so a shorter call raises NCB's
/// `TJS_E_BADPARAMCOUNT` (`reference/cpp/core/plugin/ncbind.hpp:1178`).
/// The parameter conversions are `AsReal` for `emSize` and `AsInteger` for
/// `style`, both of which raise on object arguments
/// (`reference/cpp/core/tjs2/tjsVariant.h:865, 953-954`).
extern "C" fn ctor(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let a = unsafe { args(argv, argc) };
    if a.len() < 3 {
        return report_error(out_error, "FontInfo requires 3 arguments");
    }
    let em_size = match strict_real(&a[1]) {
        Ok(value) => value,
        Err(error) => return report_error(out_error, &format!("FontInfo: emSize: {error}")),
    };
    let style = match crate::natives::system::try_value_as_i64(&a[2]) {
        Ok(value) => value,
        Err(error) => return report_error(out_error, &format!("FontInfo: style: {error}")),
    };
    let inst = unsafe { &mut *(instance as *mut FontInst) };
    inst.data.family = crate::native_util::value_as_string(&a[0]);
    inst.data.em_size = em_size;
    inst.data.style = style as i32;
    set_void(out);
    0
}

/// The reference `AsReal` conversion (`reference/cpp/core/tjs2/tjsVariant.h:
/// 947-966`) for the `REAL` parameters: `void`/strings coerce, objects and
/// octets raise a conversion error.
fn strict_real(value: &Value) -> Result<f64, String> {
    match value.ty {
        tjs::VAL_INTEGER | tjs::VAL_REAL | tjs::VAL_STRING | tjs::VAL_VOID => {
            Ok(value_as_f64(value))
        }
        _ => Err("the argument is not a number".to_string()),
    }
}

/// The `FontInfo` metrics derived from the L2 text engine.
struct Metrics {
    ascent: f64,
    descent: f64,
    ascent_leading: f64,
    descent_leading: f64,
    line_spacing: f64,
}

fn metrics(ctx: &CompatCtx, data: &FontData) -> Metrics {
    let zero = Metrics {
        ascent: 0.0,
        descent: 0.0,
        ascent_leading: 0.0,
        descent_leading: 0.0,
        line_spacing: 0.0,
    };
    let mut state = text_state(ctx);
    let Ok(face) = ensure_face(ctx, &mut state, &data.family) else {
        return zero;
    };
    let Ok(font_metrics) = state.engine.font_metrics(face, data.em_size as f32) else {
        return zero;
    };
    // Reference `FontInfo::updateSizeParams`
    // (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:362-381`):
    // `ascent`/`descent` are the cell metrics and `lineSpacing` is
    // `otmTextMetrics.tmHeight`, which equals `tmAscent + tmDescent` (Win32
    // cell metrics), so the three stay consistent here. The two *Leading
    // values need `OUTLINETEXTMETRIC.otmAscent`/`otmDescent` (the highest /
    // lowest glyph-box extents), which the L2 text engine does not expose —
    // they stay at their approximation (line gap / zero) rather than
    // fabricating a metric.
    let ascent = font_metrics.ascent as f64;
    let descent = font_metrics.descent as f64;
    let line_gap = font_metrics.line_gap as f64;
    Metrics {
        ascent,
        descent,
        ascent_leading: line_gap,
        descent_leading: 0.0,
        line_spacing: ascent + descent,
    }
}

extern "C" fn em_size_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const FontInst) };
    set_real(out, inst.data.em_size);
    0
}

extern "C" fn em_size_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let raw = unsafe { &*value };
    let em_size = match strict_real(raw) {
        Ok(value) => value,
        Err(error) => return report_error(out_error, &format!("FontInfo.emSize: {error}")),
    };
    let inst = unsafe { &mut *(instance as *mut FontInst) };
    inst.data.em_size = em_size;
    0
}

/// `style` — the reference getter returns `INT`
/// (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.hpp:81`,
/// registered at `windows/main.cpp:587`), so the value is a TJS integer,
/// not a real.
extern "C" fn style_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const FontInst) };
    crate::native_util::set_int(out, i64::from(inst.data.style));
    0
}

/// `style = n` — `setStyle(INT)` converts with `AsInteger`
/// (`reference/cpp/core/tjs2/tjsVariant.h:862-877`), which raises on
/// object/null/octet arguments.
extern "C" fn style_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let raw = unsafe { &*value };
    let style = match crate::natives::system::try_value_as_i64(raw) {
        Ok(value) => value,
        Err(error) => return report_error(out_error, &format!("FontInfo.style: {error}")),
    };
    let inst = unsafe { &mut *(instance as *mut FontInst) };
    inst.data.style = style as i32;
    0
}

extern "C" fn force_self_path_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const FontInst) };
    crate::native_util::set_bool(out, inst.data.force_self_path_draw);
    0
}

extern "C" fn force_self_path_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &mut *(instance as *mut FontInst) };
    inst.data.force_self_path_draw = crate::natives::system::value_as_bool(unsafe { &*value });
    0
}

extern "C" fn family_name_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const FontInst) };
    if let Err(error) = super::set_string_out(engine, out, &inst.data.family) {
        return report_error(out_error, &error);
    }
    0
}

extern "C" fn family_name_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &mut *(instance as *mut FontInst) };
    inst.data.family = crate::native_util::value_as_string(unsafe { &*value });
    0
}

macro_rules! font_metric_getter {
    ($name:ident, $field:ident) => {
        extern "C" fn $name(
            engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            let Ok((_host, ctx)) = super::bound(engine) else {
                return report_error(out_error, "FontInfo: compat context is not bound");
            };
            let inst = unsafe { &*(instance as *const FontInst) };
            let m = metrics(&ctx, &inst.data);
            set_real(out, m.$field);
            0
        }
    };
}

font_metric_getter!(ascent_get, ascent);
font_metric_getter!(descent_get, descent);
font_metric_getter!(ascent_leading_get, ascent_leading);
font_metric_getter!(descent_leading_get, descent_leading);
font_metric_getter!(line_spacing_get, line_spacing);

/// The method table.
pub(super) fn method_defs() -> Vec<NativeInstanceMethodDef> {
    Vec::new()
}

/// The property table.
pub(super) fn property_defs() -> Vec<NativeInstancePropertyDef> {
    vec![
        NativeInstancePropertyDef {
            name: "familyName",
            get: Some(family_name_get),
            set: Some(family_name_set),
        },
        NativeInstancePropertyDef {
            name: "emSize",
            get: Some(em_size_get),
            set: Some(em_size_set),
        },
        NativeInstancePropertyDef {
            name: "style",
            get: Some(style_get),
            set: Some(style_set),
        },
        NativeInstancePropertyDef {
            name: "forceSelfPathDraw",
            get: Some(force_self_path_get),
            set: Some(force_self_path_set),
        },
        NativeInstancePropertyDef {
            name: "ascent",
            get: Some(ascent_get),
            set: None,
        },
        NativeInstancePropertyDef {
            name: "descent",
            get: Some(descent_get),
            set: None,
        },
        NativeInstancePropertyDef {
            name: "ascentLeading",
            get: Some(ascent_leading_get),
            set: None,
        },
        NativeInstancePropertyDef {
            name: "descentLeading",
            get: Some(descent_leading_get),
            set: None,
        },
        NativeInstancePropertyDef {
            name: "lineSpacing",
            get: Some(line_spacing_get),
            set: None,
        },
    ]
}

/// Register the `FontInfo` class.
pub(super) fn register_class(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    let mut methods = vec![NativeInstanceMethodDef {
        name: "FontInfo",
        f: ctor,
    }];
    methods.extend(method_defs());
    ctx.register_instance(&NativeInstanceBuilder {
        name: "FontInfo",
        create,
        destroy,
        invalidate: None,
        methods,
        properties: property_defs(),
    })
}
