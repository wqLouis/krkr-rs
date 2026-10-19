//! `text_render` plugin — full port of the reference `TextRenderBase` class.
//!
//! Behavioural reference: `reference/cpp/plugins/TextRender.cpp`. The plugin
//! provides the KAG text-layout engine `TextRenderBase`: it parses the KAG
//! text markup (`%f`, `%b`, `\n`, `#rrggbb;`, …) into positioned
//! [`CharacterInfo`]s, tracks the render box and the per-instance style, and
//! exposes the reference's full method/property surface.
//!
//! # L2 composition
//!
//! The reference asks the process-wide [`FontRasterizer`] for per-character
//! extents and the ascent. Compat has one rasterizer and no font enumeration,
//! so the metrics come from the L2 [`engine::text::TextEngine`] (cosmic-text
//! shaping) and a face is loaded from the attached [`engine::Storage`] on
//! first use — the same seam the visual `Font` native uses. The reference's
//! `"user"` face resolves to the newest font registered in the attached engine
//! [`engine::Scene`], which is the compat equivalent of the rasterizer's
//! currently applied face.
//!
//! Every piece of layout state lives in the per-instance payload; the shared
//! shaping engine and face cache live in
//! [`CompatServices::state`](crate::CompatServices::state), never in a process
//! global.
//!
//! [`FontRasterizer`]: https://github.com/khronos/kirikiri
#![allow(unsafe_code)]

mod layout;
mod metrics;
mod state;

use std::ffi::{c_char, c_int, c_void};

use tjs::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef,
    TjsValue, Value,
};

use crate::native_util::{
    args, report_error, set_bool, set_int, set_void, value_as_string, value_ref,
};
use crate::natives::system::{resolve, value_as_bool, value_as_i64};
use crate::{CompatCtx, CompatError};

use state::{CharacterInfo, TextRenderOptions, TextRenderState};

/// Per-object `TextRenderBase` payload: the reference's private members.
pub(super) struct TextRenderInst {
    pub(super) state: TextRenderState,
    pub(super) default: TextRenderState,
    pub(super) options: TextRenderOptions,
    pub(super) box_width: i32,
    pub(super) box_height: i32,
    pub(super) x: i32,
    pub(super) y: i32,
    pub(super) indent: i32,
    pub(super) auto_indent: i32,
    pub(super) is_beginning_of_line: bool,
    pub(super) vertical: bool,
    pub(super) characters: Vec<CharacterInfo>,
    pub(super) buffer: Vec<CharacterInfo>,
    pub(super) mode: u32,
}

impl Default for TextRenderInst {
    fn default() -> Self {
        Self {
            state: TextRenderState::default(),
            default: TextRenderState::default(),
            options: TextRenderOptions::default(),
            box_width: 0,
            box_height: 0,
            x: 0,
            y: 0,
            indent: 0,
            auto_indent: 0,
            is_beginning_of_line: true,
            vertical: false,
            characters: Vec::new(),
            buffer: Vec::new(),
            mode: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

extern "C" fn text_render_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(TextRenderInst::default())) as *mut c_void
}

extern "C" fn text_render_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `instance` came from `text_render_create` and is released once.
    drop(unsafe { Box::from_raw(instance as *mut TextRenderInst) });
}

/// `new TextRenderBase()` — the reference constructor takes no arguments.
extern "C" fn text_render_ctor(
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
// Methods
// ---------------------------------------------------------------------------

/// `render(text, autoIndent, diff, all, same)` → `!overflow`.
///
/// The reference registers `render` with `NCB_METHOD` and ncbind rejects
/// calls with fewer arguments than the C++ signature has parameters
/// (`reference/cpp/core/plugin/ncbind.hpp:1178`), so all five arguments are
/// required.
extern "C" fn text_render_render(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "TextRenderBase.render: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let a = unsafe { args(argv, argc) };
    if a.len() < 5 {
        return report_error(out_error, "TextRenderBase.render requires 5 arguments");
    }
    let text = a.first().map(value_as_string).unwrap_or_default();
    let mut text_engine = metrics::text_engine(&ctx);
    // SAFETY: `instance` is the live payload for the call.
    let inst = unsafe { &mut *(instance as *mut TextRenderInst) };
    match inst.render(&ctx, &mut text_engine, &text) {
        Ok(result) => {
            set_bool(out, result);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `setRenderSize(width, height)` — set the render box and clear.
///
/// Both parameters are mandatory in the reference
/// (`reference/cpp/core/plugin/ncbind.hpp:1178`).
extern "C" fn text_render_set_render_size(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if resolve(engine).is_none() {
        return report_error(
            out_error,
            "TextRenderBase.setRenderSize: compat context is not bound",
        );
    }
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let a = unsafe { args(argv, argc) };
    if a.len() < 2 {
        return report_error(
            out_error,
            "TextRenderBase.setRenderSize requires 2 arguments",
        );
    }
    let width = a.first().map(value_as_i64).unwrap_or(0) as i32;
    let height = a.get(1).map(value_as_i64).unwrap_or(0) as i32;
    // SAFETY: `instance` is the live payload for the call.
    let inst = unsafe { &mut *(instance as *mut TextRenderInst) };
    inst.box_width = width;
    inst.box_height = height;
    inst.clear();
    set_void(out);
    0
}

/// `setDefault(dictionary)` — replace the default style.
extern "C" fn text_render_set_default(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "TextRenderBase.setDefault: compat context is not bound",
        );
    };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let a = unsafe { args(argv, argc) };
    let Some(value) = a.first() else {
        return report_error(out_error, "TextRenderBase.setDefault requires 1 argument");
    };
    // SAFETY: `instance` is the live payload for the call.
    let inst = unsafe { &mut *(instance as *mut TextRenderInst) };
    match inst.default.deserialize(resolved.engine(), value) {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `setOption(options)` — the reference `TextRenderOptions::deserialize` is a
/// no-op, so the argument is accepted and discarded; ncbind still requires
/// it (`reference/cpp/core/plugin/ncbind.hpp:1178`).
extern "C" fn text_render_set_option(
    _engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "TextRenderBase.setOption requires 1 argument");
    }
    set_void(out);
    0
}

/// `getCharacters(start, end)` — the positioned characters as a TJS Array.
extern "C" fn text_render_get_characters(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "TextRenderBase.getCharacters: compat context is not bound",
        );
    };
    let host = resolved.engine();
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let a = unsafe { args(argv, argc) };
    if a.len() < 2 {
        return report_error(
            out_error,
            "TextRenderBase.getCharacters requires 2 arguments",
        );
    }
    let start = a.first().map(value_as_i64).unwrap_or(0) as i32;
    let end = a.get(1).map(value_as_i64).unwrap_or(0) as i32;
    // SAFETY: `instance` is the live payload for the call.
    let inst = unsafe { &*(instance as *const TextRenderInst) };

    let array = match state::retained_object(host, "[]") {
        Ok(array) => array,
        Err(error) => return report_error(out_error, &error),
    };
    // The reference only populates the array for `end < start` or `(0, 0)`.
    if end < start || (start == 0 && end == 0) {
        for (index, character) in inst.characters.iter().enumerate() {
            let dict = match character.serialize(host) {
                Ok(dict) => dict,
                Err(error) => return report_error(out_error, &error),
            };
            let dict_id = dict.raw_id().as_ptr() as u64;
            // The retention is consumed by `set_member`; forget the RAII
            // wrapper so it is not released a second time.
            std::mem::forget(dict);
            if let Err(error) = host.set_member(
                array.raw_id(),
                &index.to_string(),
                &TjsValue::Retained(dict_id),
            ) {
                return report_error(out_error, &format!("TextRenderBase.getCharacters: {error}"));
            }
        }
    }
    set_retained_out(out, array);
    0
}

/// `clear()` — reset the characters and the style to the default.
extern "C" fn text_render_clear(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live payload for the call.
    unsafe { &mut *(instance as *mut TextRenderInst) }.clear();
    set_void(out);
    0
}

/// `done()` — flush the pending character buffer.
extern "C" fn text_render_done(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "TextRenderBase.done: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    let mut text_engine = metrics::text_engine(&ctx);
    // SAFETY: `instance` is the live payload for the call.
    let inst = unsafe { &mut *(instance as *mut TextRenderInst) };
    match inst.flush(&ctx, &mut text_engine, false) {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `newline()` — the reference logs a warning and does nothing.
extern "C" fn text_render_newline(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    log::warn!("TextRenderBase.newline: no support newline");
    set_void(out);
    0
}

/// `resetFont()` — restore the default style.
extern "C" fn text_render_reset_font(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live payload for the call.
    unsafe { &mut *(instance as *mut TextRenderInst) }.reset_font();
    set_void(out);
    0
}

/// `resetStyle()` — restore the default style.
extern "C" fn text_render_reset_style(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live payload for the call.
    unsafe { &mut *(instance as *mut TextRenderInst) }.reset_style();
    set_void(out);
    0
}

/// `getKeyWait()` — the reference returns an empty array.
extern "C" fn text_render_get_key_wait(
    engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    empty_array_out(engine, out, out_error)
}

/// `calcShowCount()` — the number of positioned characters.
extern "C" fn text_render_calc_show_count(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live payload for the call.
    let inst = unsafe { &*(instance as *const TextRenderInst) };
    set_int(out, inst.characters.len() as i64);
    0
}

/// The shared body of `getKeyWait()` and the `keyWait` property getter.
fn empty_array_out(engine: *mut c_void, out: *mut Value, out_error: *mut *mut c_char) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "TextRenderBase.getKeyWait: compat context is not bound",
        );
    };
    match state::retained_object(resolved.engine(), "[]") {
        Ok(array) => {
            set_retained_out(out, array);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

// ---------------------------------------------------------------------------
// Property accessor macros
// ---------------------------------------------------------------------------

macro_rules! int_prop {
    ($get:ident, $set:ident, $g:expr, $s:expr) => {
        extern "C" fn $get(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is the live payload for the call.
            let inst = unsafe { &*(instance as *const TextRenderInst) };
            set_int(out, i64::from($g(inst)));
            0
        }

        extern "C" fn $set(
            _engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is the live payload and `value` is the
            // setter argument, both valid for the call.
            let inst = unsafe { &mut *(instance as *mut TextRenderInst) };
            $s(inst, value_as_i64(value_ref(value)));
            0
        }
    };
}

macro_rules! bool_prop {
    ($get:ident, $set:ident, $g:expr, $s:expr) => {
        extern "C" fn $get(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is the live payload for the call.
            let inst = unsafe { &*(instance as *const TextRenderInst) };
            set_bool(out, $g(inst));
            0
        }

        extern "C" fn $set(
            _engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is the live payload and `value` is the
            // setter argument, both valid for the call.
            let inst = unsafe { &mut *(instance as *mut TextRenderInst) };
            $s(inst, value_as_bool(value_ref(value)));
            0
        }
    };
}

macro_rules! real_prop {
    ($get:ident, $set:ident, $g:expr, $s:expr) => {
        extern "C" fn $get(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is the live payload for the call.
            let inst = unsafe { &*(instance as *const TextRenderInst) };
            set_real(out, $g(inst));
            0
        }

        extern "C" fn $set(
            _engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is the live payload and `value` is the
            // setter argument, both valid for the call.
            let inst = unsafe { &mut *(instance as *mut TextRenderInst) };
            $s(inst, value_as_f64(value_ref(value)));
            0
        }
    };
}

macro_rules! string_prop {
    ($get:ident, $set:ident, $g:expr, $s:expr) => {
        extern "C" fn $get(
            engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            let Some(resolved) = resolve(engine) else {
                return report_error(out_error, "TextRenderBase: compat context is not bound");
            };
            let ctx = resolved.ctx();
            // SAFETY: `instance` is the live payload for the call.
            let inst = unsafe { &*(instance as *const TextRenderInst) };
            crate::natives::system::set_string_out(&ctx, out, &$g(inst));
            0
        }

        extern "C" fn $set(
            _engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is the live payload and `value` is the
            // setter argument, both valid for the call.
            let inst = unsafe { &mut *(instance as *mut TextRenderInst) };
            $s(inst, value_as_string(value_ref(value)));
            0
        }
    };
}

// --- style / state properties ---------------------------------------------

bool_prop!(
    prop_vertical_get,
    prop_vertical_set,
    |inst: &TextRenderInst| inst.vertical,
    |inst: &mut TextRenderInst, v: bool| inst.vertical = v
);
bool_prop!(
    prop_bold_get,
    prop_bold_set,
    |inst: &TextRenderInst| inst.state.bold,
    |inst: &mut TextRenderInst, v: bool| inst.state.bold = v
);
bool_prop!(
    prop_italic_get,
    prop_italic_set,
    |inst: &TextRenderInst| inst.state.italic,
    |inst: &mut TextRenderInst, v: bool| inst.state.italic = v
);
string_prop!(
    prop_face_get,
    prop_face_set,
    |inst: &TextRenderInst| inst.state.face.clone(),
    |inst: &mut TextRenderInst, v: String| inst.state.face = v
);
int_prop!(
    prop_font_size_get,
    prop_font_size_set,
    |inst: &TextRenderInst| inst.state.font_size,
    |inst: &mut TextRenderInst, v: i64| inst.state.font_size = v as i32
);
real_prop!(
    prop_font_scale_get,
    prop_font_scale_set,
    |inst: &TextRenderInst| inst.state.font_scale,
    |inst: &mut TextRenderInst, v: f64| inst.state.font_scale = v
);
int_prop!(
    prop_ch_color_get,
    prop_ch_color_set,
    |inst: &TextRenderInst| inst.state.ch_color as i32,
    |inst: &mut TextRenderInst, v: i64| inst.state.ch_color = v as u32
);
int_prop!(
    prop_ruby_size_get,
    prop_ruby_size_set,
    |inst: &TextRenderInst| inst.state.ruby_size,
    |inst: &mut TextRenderInst, v: i64| inst.state.ruby_size = v as i32
);
int_prop!(
    prop_ruby_offset_get,
    prop_ruby_offset_set,
    |inst: &TextRenderInst| inst.state.ruby_offset,
    |inst: &mut TextRenderInst, v: i64| inst.state.ruby_offset = v as i32
);
bool_prop!(
    prop_shadow_get,
    prop_shadow_set,
    |inst: &TextRenderInst| inst.state.shadow,
    |inst: &mut TextRenderInst, v: bool| inst.state.shadow = v
);
bool_prop!(
    prop_edge_get,
    prop_edge_set,
    |inst: &TextRenderInst| inst.state.edge,
    |inst: &mut TextRenderInst, v: bool| inst.state.edge = v
);
int_prop!(
    prop_line_spacing_get,
    prop_line_spacing_set,
    |inst: &TextRenderInst| inst.state.line_spacing,
    |inst: &mut TextRenderInst, v: i64| inst.state.line_spacing = v as i32
);
int_prop!(
    prop_pitch_get,
    prop_pitch_set,
    |inst: &TextRenderInst| inst.state.pitch,
    |inst: &mut TextRenderInst, v: i64| inst.state.pitch = v as i32
);
int_prop!(
    prop_line_size_get,
    prop_line_size_set,
    |inst: &TextRenderInst| inst.state.line_size,
    |inst: &mut TextRenderInst, v: i64| inst.state.line_size = v as i32
);

// --- render state properties ----------------------------------------------

bool_prop!(
    prop_render_over_get,
    prop_render_over_set,
    |inst: &TextRenderInst| inst.state.render_over,
    |inst: &mut TextRenderInst, v: bool| inst.state.render_over = v
);
int_prop!(
    prop_render_delay_get,
    prop_render_delay_set,
    |inst: &TextRenderInst| inst.state.render_delay,
    |inst: &mut TextRenderInst, v: i64| inst.state.render_delay = v as i32
);
int_prop!(
    prop_render_left_get,
    prop_render_left_set,
    |inst: &TextRenderInst| inst.x,
    |inst: &mut TextRenderInst, v: i64| inst.x = v as i32
);
int_prop!(
    prop_render_top_get,
    prop_render_top_set,
    |inst: &TextRenderInst| inst.y,
    |inst: &mut TextRenderInst, v: i64| inst.y = v as i32
);
string_prop!(
    prop_render_text_get,
    prop_render_text_set,
    |inst: &TextRenderInst| inst.state.render_text.clone(),
    |inst: &mut TextRenderInst, v: String| inst.state.render_text = v
);

/// `renderRight` (get) — `x + boxWidth`.
extern "C" fn prop_render_right_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live payload for the call.
    let inst = unsafe { &*(instance as *const TextRenderInst) };
    set_int(out, i64::from(inst.x.wrapping_add(inst.box_width)));
    0
}

/// `renderRight` (set) — `boxWidth = value - x`.
extern "C" fn prop_render_right_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live payload and `value` is the setter
    // argument, both valid for the call.
    let inst = unsafe { &mut *(instance as *mut TextRenderInst) };
    let right = value_as_i64(value_ref(value)) as i32;
    inst.box_width = right.wrapping_sub(inst.x);
    0
}

/// `renderBottom` (get) — `y + boxHeight`.
extern "C" fn prop_render_bottom_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live payload for the call.
    let inst = unsafe { &*(instance as *const TextRenderInst) };
    set_int(out, i64::from(inst.y.wrapping_add(inst.box_height)));
    0
}

/// `renderBottom` (set) — `boxHeight = value - y`.
extern "C" fn prop_render_bottom_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live payload and `value` is the setter
    // argument, both valid for the call.
    let inst = unsafe { &mut *(instance as *mut TextRenderInst) };
    let bottom = value_as_i64(value_ref(value)) as i32;
    inst.box_height = bottom.wrapping_sub(inst.y);
    0
}

/// `renderCount` (get) — the number of UTF-16 units in `renderText`.
extern "C" fn prop_render_count_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live payload for the call.
    let inst = unsafe { &*(instance as *const TextRenderInst) };
    set_int(out, inst.state.render_text.encode_utf16().count() as i64);
    0
}

/// `renderCount` (set) — the reference refuses writes.
extern "C" fn prop_render_count_set(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    report_error(out_error, "avoid to set renderCount")
}

/// `keyWait` (get) — an empty array.
extern "C" fn prop_key_wait_get(
    engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    empty_array_out(engine, out, out_error)
}

/// `keyWait` (set) — the reference refuses writes.
extern "C" fn prop_key_wait_set(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    report_error(out_error, "no support setKeyWait")
}

// --- default style properties ---------------------------------------------

bool_prop!(
    prop_default_bold_get,
    prop_default_bold_set,
    |inst: &TextRenderInst| inst.default.bold,
    |inst: &mut TextRenderInst, v: bool| inst.default.bold = v
);
bool_prop!(
    prop_default_italic_get,
    prop_default_italic_set,
    |inst: &TextRenderInst| inst.default.italic,
    |inst: &mut TextRenderInst, v: bool| inst.default.italic = v
);
string_prop!(
    prop_default_face_get,
    prop_default_face_set,
    |inst: &TextRenderInst| inst.default.face.clone(),
    |inst: &mut TextRenderInst, v: String| inst.default.face = v
);
int_prop!(
    prop_default_font_size_get,
    prop_default_font_size_set,
    |inst: &TextRenderInst| inst.default.font_size,
    |inst: &mut TextRenderInst, v: i64| inst.default.font_size = v as i32
);
real_prop!(
    prop_default_font_scale_get,
    prop_default_font_scale_set,
    |inst: &TextRenderInst| f64::from(inst.default.font_scale as f32),
    |inst: &mut TextRenderInst, v: f64| inst.default.font_scale = f64::from(v as f32)
);
int_prop!(
    prop_default_ch_color_get,
    prop_default_ch_color_set,
    |inst: &TextRenderInst| inst.default.ch_color as i32,
    |inst: &mut TextRenderInst, v: i64| inst.default.ch_color = v as u32
);
int_prop!(
    prop_default_ruby_size_get,
    prop_default_ruby_size_set,
    |inst: &TextRenderInst| inst.default.ruby_size,
    |inst: &mut TextRenderInst, v: i64| inst.default.ruby_size = v as i32
);
int_prop!(
    prop_default_ruby_offset_get,
    prop_default_ruby_offset_set,
    |inst: &TextRenderInst| inst.default.ruby_offset,
    |inst: &mut TextRenderInst, v: i64| inst.default.ruby_offset = v as i32
);
bool_prop!(
    prop_default_shadow_get,
    prop_default_shadow_set,
    |inst: &TextRenderInst| inst.default.shadow,
    |inst: &mut TextRenderInst, v: bool| inst.default.shadow = v
);
bool_prop!(
    prop_default_edge_get,
    prop_default_edge_set,
    |inst: &TextRenderInst| inst.default.edge,
    |inst: &mut TextRenderInst, v: bool| inst.default.edge = v
);
int_prop!(
    prop_default_line_spacing_get,
    prop_default_line_spacing_set,
    |inst: &TextRenderInst| inst.default.line_spacing,
    |inst: &mut TextRenderInst, v: i64| inst.default.line_spacing = v as i32
);
int_prop!(
    prop_default_pitch_get,
    prop_default_pitch_set,
    |inst: &TextRenderInst| inst.default.pitch,
    |inst: &mut TextRenderInst, v: i64| inst.default.pitch = v as i32
);
int_prop!(
    prop_default_line_size_get,
    prop_default_line_size_set,
    |inst: &TextRenderInst| inst.default.line_size,
    |inst: &mut TextRenderInst, v: i64| inst.default.line_size = v as i32
);

// ---------------------------------------------------------------------------
// Value helpers
// ---------------------------------------------------------------------------

/// Write a real result into `out`.
fn set_real(out: *mut Value, value: f64) {
    if out.is_null() {
        return;
    }
    // SAFETY: the trampoline passes a valid out slot for the call.
    unsafe {
        (*out).ty = tjs::VAL_REAL;
        (*out).integer = 0;
        (*out).real = value;
        (*out).string = std::ptr::null();
        (*out).array = std::ptr::null();
        (*out).array_count = 0;
        (*out).retained = 0;
    }
}

/// TJS `AsReal` coercion for a setter argument.
fn value_as_f64(v: &Value) -> f64 {
    match v.ty {
        tjs::VAL_INTEGER => v.integer as f64,
        tjs::VAL_REAL => v.real,
        tjs::VAL_STRING => crate::natives::system::tjs_string_to_real(&value_as_string(v)),
        _ => 0.0,
    }
}

/// Write a retained object into `out` as `VAL_RETAINED`; the C++ trampoline
/// consumes the retention when it copies the result.
fn set_retained_out(out: *mut Value, object: DetachedValue) {
    if !out.is_null() {
        // SAFETY: the trampoline passes a valid out slot for the call.
        unsafe {
            (*out).ty = tjs::VAL_RETAINED;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = std::ptr::null();
            (*out).array = std::ptr::null();
            (*out).array_count = 0;
            (*out).retained = object.raw_id().as_ptr() as usize;
        }
    }
    // The C++ conversion consumes the id; forgetting the wrapper avoids a
    // redundant (but safe) release.
    std::mem::forget(object);
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the `TextRenderBase` native class on the VM.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "TextRenderBase",
        create: text_render_create,
        destroy: text_render_destroy,
        invalidate: None,
        methods: vec![
            NativeInstanceMethodDef {
                name: "TextRenderBase",
                f: text_render_ctor,
            },
            NativeInstanceMethodDef {
                name: "render",
                f: text_render_render,
            },
            NativeInstanceMethodDef {
                name: "setRenderSize",
                f: text_render_set_render_size,
            },
            NativeInstanceMethodDef {
                name: "setDefault",
                f: text_render_set_default,
            },
            NativeInstanceMethodDef {
                name: "setOption",
                f: text_render_set_option,
            },
            NativeInstanceMethodDef {
                name: "getCharacters",
                f: text_render_get_characters,
            },
            NativeInstanceMethodDef {
                name: "clear",
                f: text_render_clear,
            },
            NativeInstanceMethodDef {
                name: "done",
                f: text_render_done,
            },
            NativeInstanceMethodDef {
                name: "newline",
                f: text_render_newline,
            },
            NativeInstanceMethodDef {
                name: "resetFont",
                f: text_render_reset_font,
            },
            NativeInstanceMethodDef {
                name: "resetStyle",
                f: text_render_reset_style,
            },
            NativeInstanceMethodDef {
                name: "getKeyWait",
                f: text_render_get_key_wait,
            },
            NativeInstanceMethodDef {
                name: "calcShowCount",
                f: text_render_calc_show_count,
            },
        ],
        properties: vec![
            NativeInstancePropertyDef {
                name: "keyWait",
                get: Some(prop_key_wait_get),
                set: Some(prop_key_wait_set),
            },
            NativeInstancePropertyDef {
                name: "renderCount",
                get: Some(prop_render_count_get),
                set: Some(prop_render_count_set),
            },
            NativeInstancePropertyDef {
                name: "renderOver",
                get: Some(prop_render_over_get),
                set: Some(prop_render_over_set),
            },
            NativeInstancePropertyDef {
                name: "renderDelay",
                get: Some(prop_render_delay_get),
                set: Some(prop_render_delay_set),
            },
            NativeInstancePropertyDef {
                name: "renderText",
                get: Some(prop_render_text_get),
                set: Some(prop_render_text_set),
            },
            NativeInstancePropertyDef {
                name: "renderLeft",
                get: Some(prop_render_left_get),
                set: Some(prop_render_left_set),
            },
            NativeInstancePropertyDef {
                name: "renderRight",
                get: Some(prop_render_right_get),
                set: Some(prop_render_right_set),
            },
            NativeInstancePropertyDef {
                name: "renderTop",
                get: Some(prop_render_top_get),
                set: Some(prop_render_top_set),
            },
            NativeInstancePropertyDef {
                name: "renderBottom",
                get: Some(prop_render_bottom_get),
                set: Some(prop_render_bottom_set),
            },
            NativeInstancePropertyDef {
                name: "vertical",
                get: Some(prop_vertical_get),
                set: Some(prop_vertical_set),
            },
            NativeInstancePropertyDef {
                name: "bold",
                get: Some(prop_bold_get),
                set: Some(prop_bold_set),
            },
            NativeInstancePropertyDef {
                name: "italic",
                get: Some(prop_italic_get),
                set: Some(prop_italic_set),
            },
            NativeInstancePropertyDef {
                name: "face",
                get: Some(prop_face_get),
                set: Some(prop_face_set),
            },
            NativeInstancePropertyDef {
                name: "fontSize",
                get: Some(prop_font_size_get),
                set: Some(prop_font_size_set),
            },
            NativeInstancePropertyDef {
                name: "fontScale",
                get: Some(prop_font_scale_get),
                set: Some(prop_font_scale_set),
            },
            NativeInstancePropertyDef {
                name: "chColor",
                get: Some(prop_ch_color_get),
                set: Some(prop_ch_color_set),
            },
            NativeInstancePropertyDef {
                name: "rubySize",
                get: Some(prop_ruby_size_get),
                set: Some(prop_ruby_size_set),
            },
            NativeInstancePropertyDef {
                name: "rubyOffset",
                get: Some(prop_ruby_offset_get),
                set: Some(prop_ruby_offset_set),
            },
            NativeInstancePropertyDef {
                name: "shadow",
                get: Some(prop_shadow_get),
                set: Some(prop_shadow_set),
            },
            NativeInstancePropertyDef {
                name: "edge",
                get: Some(prop_edge_get),
                set: Some(prop_edge_set),
            },
            NativeInstancePropertyDef {
                name: "lineSpacing",
                get: Some(prop_line_spacing_get),
                set: Some(prop_line_spacing_set),
            },
            NativeInstancePropertyDef {
                name: "pitch",
                get: Some(prop_pitch_get),
                set: Some(prop_pitch_set),
            },
            NativeInstancePropertyDef {
                name: "lineSize",
                get: Some(prop_line_size_get),
                set: Some(prop_line_size_set),
            },
            NativeInstancePropertyDef {
                name: "defaultBold",
                get: Some(prop_default_bold_get),
                set: Some(prop_default_bold_set),
            },
            NativeInstancePropertyDef {
                name: "defaultItalic",
                get: Some(prop_default_italic_get),
                set: Some(prop_default_italic_set),
            },
            NativeInstancePropertyDef {
                name: "defaultFace",
                get: Some(prop_default_face_get),
                set: Some(prop_default_face_set),
            },
            NativeInstancePropertyDef {
                name: "defaultFontSize",
                get: Some(prop_default_font_size_get),
                set: Some(prop_default_font_size_set),
            },
            NativeInstancePropertyDef {
                name: "defaultFontScale",
                get: Some(prop_default_font_scale_get),
                set: Some(prop_default_font_scale_set),
            },
            NativeInstancePropertyDef {
                name: "defaultChColor",
                get: Some(prop_default_ch_color_get),
                set: Some(prop_default_ch_color_set),
            },
            NativeInstancePropertyDef {
                name: "defaultRubySize",
                get: Some(prop_default_ruby_size_get),
                set: Some(prop_default_ruby_size_set),
            },
            NativeInstancePropertyDef {
                name: "defaultRubyOffset",
                get: Some(prop_default_ruby_offset_get),
                set: Some(prop_default_ruby_offset_set),
            },
            NativeInstancePropertyDef {
                name: "defaultShadow",
                get: Some(prop_default_shadow_get),
                set: Some(prop_default_shadow_set),
            },
            NativeInstancePropertyDef {
                name: "defaultEdge",
                get: Some(prop_default_edge_get),
                set: Some(prop_default_edge_set),
            },
            NativeInstancePropertyDef {
                name: "defaultLineSpacing",
                get: Some(prop_default_line_spacing_get),
                set: Some(prop_default_line_spacing_set),
            },
            NativeInstancePropertyDef {
                name: "defaultPitch",
                get: Some(prop_default_pitch_get),
                set: Some(prop_default_pitch_set),
            },
            NativeInstancePropertyDef {
                name: "defaultLineSize",
                get: Some(prop_default_line_size_get),
                set: Some(prop_default_line_size_set),
            },
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::register;
    use crate::{CompatCtx, CompatHost, CompatServices};
    use engine::Scene;
    use tjs::{Tjs2Engine, TjsValue};

    /// Registration smoke test that installs the engine → context binding
    /// directly and registers only this plugin. It is independent of the
    /// shared plugin aggregator (`plugins/mod.rs`), so it verifies the class
    /// surface even while sibling plugins are being ported.
    #[test]
    fn registers_the_class_and_defaults() {
        let _lock = crate::native_util::test_vm_lock();
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut host = CompatHost::new();
        // SAFETY: the boxed scene outlives the host and the binding.
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
        }
        let services = host.services_mut() as *mut CompatServices;
        crate::binding::install(&engine, services, host.registry()).expect("install binding");
        {
            // SAFETY: `engine` and `services` are live for the test body.
            let mut ctx = unsafe {
                CompatCtx::from_raw(&*engine as *const Tjs2Engine as usize, services as usize)
            };
            register(&mut ctx).expect("register TextRenderBase");
        }

        engine
            .exec_script("var t = new TextRenderBase(); t.fontSize = 30;", "unit")
            .expect("construct TextRenderBase");
        assert_eq!(
            engine.eval("t.fontSize", "unit").expect("read fontSize"),
            TjsValue::Integer(30)
        );
        assert_eq!(
            engine.eval("t.face", "unit").expect("read face"),
            TjsValue::String("user".to_string())
        );

        crate::binding::uninstall();
    }
}
