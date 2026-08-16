//! `__TvpBitmap` — the native implementation class behind the script
//! `Bitmap` wrapper (see `natives/mod.rs` for the architecture).
//!
//! Mirrors `reference/cpp/core/visual/BitmapIntf.cpp`:
//!
//! * `new Bitmap("name")` loads the image from game storage via
//!   [`crate::bitmap::load_bitmap_from_storage`] (extension probing + cache;
//!   loading the same file twice returns the **same** bitmap — the reference
//!   shares bitmaps by storage name),
//! * `new Bitmap(w, h)` creates a blank bitmap (transparent black; the
//!   reference leaves the backing texture uninitialized, we define it).
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

/// `__TvpBitmap(name)` / `__TvpBitmap(w, h)` — constructor hook. One string
/// argument loads from storage; two integer arguments create a blank
/// bitmap. Returns the new bitmap's scene id.
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
    let id = match args.len() {
        1 if args[0].ty == tjs2_sys::VAL_STRING => {
            let name = super::ffi::arg_string(&args[0]);
            let (mut scene, mut storage) = super::context_scene_storage();
            crate::bitmap::load_bitmap_from_storage(
                &mut scene,
                &mut super::bitmap_cache(),
                &mut storage,
                &name,
                None,
            )
            .map_err(|e| format!("Bitmap: {e}"))
        }
        1 => Err("Bitmap: constructor expects a storage name or two integers".into()),
        2 => {
            let w = arg_i64(&args[0]).max(0) as u32;
            let h = arg_i64(&args[1]).max(0) as u32;
            let mut scene = context_scene_mut();
            Ok(crate::bitmap::add_blank_bitmap(&mut scene, w, h))
        }
        _ => Err(
            "Bitmap: constructor expects a storage name (1 argument) or a size (2 arguments)"
                .into(),
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
}
