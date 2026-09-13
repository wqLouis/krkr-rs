//! `__TvpBitmap` — the native implementation class behind the script
//! `Bitmap` wrapper (see `natives/mod.rs` for the architecture).
//!
//! Mirrors `reference/cpp/core/visual/BitmapIntf.cpp`:
//!
//! * `new Bitmap("name"[, colorkey])` loads the image from game storage via
//!   [`crate::bitmap::load_bitmap_from_storage`] (extension probing + cache;
//!   loading the same file twice returns the **same** bitmap — the reference
//!   shares bitmaps by storage name),
//! * `new Bitmap(w, h[, bpp])` creates a blank bitmap (transparent black; the
//!   reference leaves the backing texture uninitialized, we define it),
//! * `new Bitmap(bitmap[, rect])` copies another bitmap's pixels into a new
//!   bitmap,
//! * `new Bitmap()` creates an empty bitmap (the game's `class Foo extends
//!   Bitmap { function Foo() { Bitmap(); ... } }` base-constructor call), which
//!   [`crate::bitmap::add_blank_bitmap`] clamps to 1×1 (the reference's 0×0
//!   empty state, kept non-degenerate so downstream `setSize`/`copyRect`/
//!   `load`/`assign` always have valid pixels to touch).
//!
//! The constructor returns the bitmap's scene id; `width`/`height` are
//! exposed through `__width()`/`__height()`.
//!
//! **Destroy semantics**: bitmaps are shared by reference in TVP (the cache
//! and every layer referencing one keep it alive), so `destroy` is a no-op
//! for the scene — the pixels stay available as long as the scene exists.

use std::ffi::{c_char, c_int, c_void};

use tjs2_sys::{
    NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, Tjs2Engine, Value,
};

use super::ffi::{arg_i64, error_out, instance_ref, set_int_out};
use super::{context_scene_mut, context_scene_read};

/// Payload of one script-visible `Bitmap` object.
#[derive(Default)]
pub(crate) struct BitmapInst {
    /// Scene bitmap id, assigned by the constructor.
    pub id: u32,
    /// Whether the native constructor has run.
    pub constructed: bool,
}

/// `new Bitmap(...)` payload factory.
extern "C" fn bitmap_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::<BitmapInst>::default()) as *mut c_void
}

/// Release a `Bitmap` payload. Bitmaps are shared by reference in TVP (the
/// bitmap cache and all layers referencing them keep the pixels alive), so
/// this intentionally does not remove the bitmap from the scene — see the
/// module doc.
extern "C" fn bitmap_destroy(_engine: *mut c_void, instance: *mut c_void) {
    // SAFETY: instance came from Box::into_raw.
    unsafe { drop(Box::from_raw(instance as *mut BitmapInst)) };
}

/// Whether a callback argument is one of TJS's numeric types.
fn is_number(v: &Value) -> bool {
    v.ty == tjs2_sys::VAL_INTEGER || v.ty == tjs2_sys::VAL_REAL
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

/// Resolve a `Bitmap` argument (an object exposing `id`/`nativeId`, or a raw
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

/// `__TvpBitmap(...)` — constructor hook. Accepts the reference forms
/// `Bitmap()`, `Bitmap(name[, colorkey])`, `Bitmap(w, h[, bpp])` and
/// `Bitmap(bitmap[, rect])`. Returns the new bitmap's scene id.
///
/// The legacy `colorkey`/`bpp` parameters are accepted and ignored: bitmaps
/// are always stored as straight-alpha RGBA8, so a colorkey would be a no-op
/// and `bpp` is fixed at 32. The optional `rect` in the copy form is likewise
/// accepted and ignored (the whole source bitmap is copied); the game does not
/// use either form.
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
    let args = unsafe { super::ffi::args(argc, argv) };
    let inst = unsafe { instance_ref::<BitmapInst>(instance) };
    if inst.constructed {
        return error_out(out_error, "Bitmap: this bitmap is already constructed");
    }
    let id = match args {
        // `Bitmap()` — the empty bitmap the game's derived-class constructors
        // call (e.g. `class PreviewThumbnail extends Bitmap`).
        [] => blank_bitmap(0, 0),
        // `Bitmap(name)` — load from storage.
        [a] if a.ty == tjs2_sys::VAL_STRING => load_named_bitmap(&super::ffi::arg_string(a)),
        // `Bitmap(bitmap)` — copy.
        [a] if a.ty == tjs2_sys::VAL_OBJECT => copy_bitmap_arg(a),
        // `Bitmap(name, colorkey)` — load; colorkey ignored (RGBA8).
        [a, _colorkey] if a.ty == tjs2_sys::VAL_STRING => {
            load_named_bitmap(&super::ffi::arg_string(a))
        }
        // `Bitmap(bitmap, rect)` — copy; rect ignored (whole bitmap).
        [a, _rect] if a.ty == tjs2_sys::VAL_OBJECT => copy_bitmap_arg(a),
        // `Bitmap(w, h)` — blank.
        [a, b] if is_number(a) && is_number(b) => {
            blank_bitmap(arg_i64(a).max(0) as u32, arg_i64(b).max(0) as u32)
        }
        // `Bitmap(w, h, bpp)` — blank; bpp ignored (always 32bpp RGBA8).
        [a, b, _bpp] if is_number(a) && is_number(b) => {
            blank_bitmap(arg_i64(a).max(0) as u32, arg_i64(b).max(0) as u32)
        }
        _ => Err(
            "Bitmap: constructor expects (), a storage name, (width, height), or a Bitmap".into(),
        ),
    };
    match id {
        Ok(id) => {
            inst.id = id;
            inst.constructed = true;
            set_int_out(out, i64::from(id));
            0
        }
        Err(msg) => error_out(out_error, &msg),
    }
}

/// `width` — bitmap width in pixels.
extern "C" fn bitmap_width(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<BitmapInst>(instance) };
    let scene = context_scene_read();
    let Some(bmp) = scene.bitmap(inst.id) else {
        return error_out(out_error, "Bitmap: bitmap no longer exists");
    };
    set_int_out(out, i64::from(bmp.width));
    0
}

/// `height` — bitmap height in pixels.
extern "C" fn bitmap_height(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<BitmapInst>(instance) };
    let scene = context_scene_read();
    let Some(bmp) = scene.bitmap(inst.id) else {
        return error_out(out_error, "Bitmap: bitmap no longer exists");
    };
    set_int_out(out, i64::from(bmp.height));
    0
}

/// `id` — the bitmap's scene id (read-only; object returns are pending, so
/// scripts pass ids to `layer.setBitmap(id)`).
extern "C" fn bitmap_id_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<BitmapInst>(instance) };
    set_int_out(out, i64::from(inst.id));
    0
}

/// Register the `Bitmap` native class.
pub(crate) fn register_bitmap(engine: &Tjs2Engine) -> Result<(), String> {
    engine.register_native_class_instance(&NativeInstanceBuilder {
        name: "Bitmap",
        create: bitmap_create,
        destroy: bitmap_destroy,
        methods: vec![NativeInstanceMethodDef {
            name: "Bitmap",
            f: bitmap_ctor,
        }],
        properties: vec![
            NativeInstancePropertyDef {
                name: "id",
                get: Some(bitmap_id_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "width",
                get: Some(bitmap_width),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "height",
                get: Some(bitmap_height),
                set: None,
            },
        ],
    })
}

#[cfg(test)]
mod tests {
    use crate::natives::tests::{TestEnv, write_fixture};

    /// A 4x2 solid red fixture, matching the assertions of
    /// `bitmap_loads_from_storage`.
    fn red_fixture(env: &TestEnv) {
        let (w, h) = (4u32, 2u32);
        let rgba: Vec<u8> = (0..w * h).flat_map(|_| [255u8, 0, 0, 255]).collect();
        write_fixture(env, "testimg.webp", &rgba, w, h);
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
    }

    #[test]
    fn bitmap_name_cache_aliases() {
        let env = TestEnv::new("bitmap-cache");
        red_fixture(&env);
        env.run("var b1 = new Bitmap('testimg.webp'); var b2 = new Bitmap('testimg');")
            .unwrap();
        // both constructors resolve to the same storage file, so only one
        // bitmap is registered in the scene (the reference shares cached
        // bitmaps by storage name)
        let scene = env.scene();
        assert_eq!(scene.bitmaps.len(), 1);
        assert_eq!(env.eval_int("b1.id"), env.eval_int("b2.id"));
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
        assert_eq!(scene.bitmaps.len(), 1);
        let bm_id = scene.bitmaps[0].id;
        assert!(
            scene.layers.iter().any(|l| l.bitmap == Some(bm_id)),
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
}
