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
use std::sync::{Arc, LazyLock, Mutex};

use tjs2_sys::{
    NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, Tjs2Engine, Value,
};

use super::ffi::{
    arg_f64, arg_i64, arg_string, error_out, instance_ref, set_int_out, set_real_out,
    set_string_out,
};
use super::{context_scene_mut, context_scene_read};
use tvp_text::{PrerenderedFont, PrerenderedKey, map_prerendered_font, prerendered_font};

/// Default face name for a freshly constructed `Font` (reference's
/// `MS Gothic`-ish default). Also the face a layer's lazily created font
/// starts with before `setFontStyle` overrides it.
pub(crate) const DEFAULT_FONT_FACE: &str = "MS Gothic";
/// Default `Font.height` in pixels.
pub(crate) const DEFAULT_FONT_HEIGHT: i32 = 12;

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

/// Estimate text width using TVP's common half-width/full-width rule. When the
/// font's properties are mapped to a pre-rendered `.tft`, use that font's
/// baked advances instead (the message layer advances its cursor with this
/// call, so it must match the drawn glyphs).
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
    // Snapshot the mapping key, then drop the scene lock before consulting the
    // pre-rendered registry.
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
    let height = key.height.max(1) as f64;
    let width: f64 = if let Some(pfont) = prerendered_font(&key) {
        text.chars()
            .map(|c| {
                pfont
                    .find(c)
                    .map(|g| g.advance() as f64)
                    .unwrap_or_else(|| half_or_full(c, height))
            })
            .sum()
    } else {
        text.chars().map(|c| half_or_full(c, height)).sum()
    };
    set_real_out(out, width);
    0
}

/// TVP's default advance estimate: ASCII ≈ 0.55 em, everything else a full em.
fn half_or_full(c: char, height: f64) -> f64 {
    if c.is_ascii() { height * 0.55 } else { height }
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
            map_prerendered_font(key, font);
            set_void_out(out);
            0
        }
        Err(e) => error_out(out_error, &format!("Font.mapPrerenderedFont({name}): {e}")),
    }
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
                name: "__bind",
                f: font_bind,
            },
            NativeInstanceMethodDef {
                name: "mapPrerenderedFont",
                f: font_map_prerendered,
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
