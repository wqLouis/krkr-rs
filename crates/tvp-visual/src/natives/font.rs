//! `__TvpFont` — the native implementation class behind the script `Font`
//! wrapper (see `natives/mod.rs` for the architecture).
//!
//! Minimal per milestone 3A: `new Font(face, height, color)` registers a
//! [`crate::scene::FontState`] and `face`/`height`/`color` are read/write
//! properties. Text rendering (glyph atlases, `getTextWidth`, `drawText`)
//! arrives in milestone 3B; until then `Layer.font` returns a script-side
//! font object with no-op text helpers (see the setup script in `mod.rs`).

use std::ffi::{c_char, c_int, c_void};

use tjs2_sys::{
    NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, Tjs2Engine, Value,
};

use super::ffi::{arg_i64, arg_string, error_out, instance_ref, set_int_out, set_string_out};
use super::{context_scene_mut, context_scene_read};

/// Payload of one script-visible `Font` object.
#[derive(Default)]
pub(crate) struct FontInst {
    /// Scene font id, assigned by the constructor.
    pub id: u32,
    /// Whether the native constructor has run.
    pub constructed: bool,
}

/// `new Font(...)` payload factory.
extern "C" fn font_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::<FontInst>::default()) as *mut c_void
}

/// Release a `Font` payload; removes the font from the scene.
extern "C" fn font_destroy(_engine: *mut c_void, instance: *mut c_void) {
    // SAFETY: the trampoline passes the payload from font_create.
    let inst = unsafe { instance_ref::<FontInst>(instance) };
    if inst.constructed {
        let mut scene = context_scene_mut();
        scene.fonts.retain(|f| f.id != inst.id);
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
        .unwrap_or_else(|| "MS Gothic".to_string());
    let height = args.get(1).map(arg_i64).unwrap_or(12) as i32;
    let color = args.get(2).map(arg_i64).unwrap_or(0xffffffff);
    let mut scene = context_scene_mut();
    let id = scene.add_font(face, height, argb_to_rgba(color));
    inst.id = id;
    inst.constructed = true;
    set_int_out(out, i64::from(id));
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

/// Estimate text width using TVP's common half-width/full-width rule. The
/// renderer can later replace this with `tvp-text` glyph metrics, but this
/// deterministic fallback is already sufficient for layout and hit testing
/// on systems without a matching Japanese font installed.
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
    let height = context_scene_read()
        .fonts
        .iter()
        .find(|f| f.id == inst.id)
        .map(|f| f.height.max(1) as f64)
        .unwrap_or(12.0);
    let width: f64 = text
        .chars()
        .map(|c| if c.is_ascii() { height * 0.55 } else { height })
        .sum();
    unsafe {
        (*out).ty = tjs2_sys::VAL_REAL;
        (*out).integer = 0;
        (*out).real = width;
    }
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
    let height = context_scene_read()
        .fonts
        .iter()
        .find(|f| f.id == inst.id)
        .map(|f| f.height)
        .unwrap_or(12);
    set_int_out(out, i64::from(height));
    0
}

/// Register the `Font` native class.
/// `Font.mapPrerenderedFont(file)` — stubbed no-op (prerendered font
/// mapping is not implemented; the game falls back to vector fonts).
extern "C" fn font_map_prerendered_noop(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const tjs2_sys::Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void_out(out);
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
        methods: vec![
            NativeInstanceMethodDef {
                name: "Font",
                f: font_ctor,
            },
            NativeInstanceMethodDef {
                name: "mapPrerenderedFont",
                f: font_map_prerendered_noop,
            },
            NativeInstanceMethodDef {
                name: "getTextWidth",
                f: font_text_width,
            },
            NativeInstanceMethodDef {
                name: "getTextHeight",
                f: font_text_height,
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
        ],
    })
}

#[cfg(test)]
mod tests {
    use crate::natives::tests::TestEnv;

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
}
