//! Integration tests for the `Bitmap` loader/saver members.
//!
//! L3 (`compat`) never links the L1 `media` codec, so the tests install a
//! synthetic codec through `CompatHost::set_image_codec` and attach a real
//! in-memory [`engine::Storage`]. The typed-error paths (no codec installed)
//! and the `loadAsync` → `onLoaded` dispatch are covered here; the full
//! `media`-backed decode is the L4 host's job.

mod common;

use std::path::Path;
use std::sync::{Arc, Mutex};

use api::fs::{FsPath, OpenMode};
use api::media::{DecodedImage, ImageInfo};
use common::vm_lock;
use compat::CompatHost;
use engine::{Scene, Storage};
use fs::FileSystem;
use tjs::{Tjs2Engine, TjsValue};

/// The pixels the synthetic decoder returns: a 2x2 straight-alpha RGBA image.
const DECODED_RGBA: [u8; 16] = [1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255];

/// One recorded `save` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SavedCall {
    name: String,
    type_name: String,
    width: u32,
    height: u32,
    bytes: usize,
}

/// Writes `data` to `path` in an in-memory file system, creating parents.
fn write_memory_file(fs: &FileSystem, path: &str, data: &[u8]) {
    if let Some(parent) = Path::new(path).parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(fs, &FsPath::host(parent)).expect("create parent dirs");
    }
    let file = fs::open(fs, &FsPath::host(path), OpenMode::Write).expect("open memory file");
    fs::write_at(fs, &file, 0, data).expect("write memory file");
    fs::flush(fs, &file).expect("flush memory file");
}

/// A bound VM + host + scene, plus an in-memory storage holding
/// `images/test.img`.
///
/// The boxed engine, scene and storage are declared **after** `host` so the
/// host (which stores their raw addresses) is dropped first.
struct Env {
    host: CompatHost,
    engine: Box<Tjs2Engine>,
    /// Kept alive for the host's raw pointer; never read directly.
    _scene: Box<Scene>,
    /// Kept alive for the host's raw pointer; never read directly.
    _storage: Box<Storage>,
}

impl Env {
    /// Builds an environment with a storage holding `images/test.img`. When
    /// `codec` is true the synthetic decode/probe/save closures are installed;
    /// otherwise every codec-backed member raises.
    fn new(codec: bool) -> Env {
        let fs = FileSystem::memory();
        write_memory_file(&fs, "/images/test.img", b"not-a-real-image");
        let storage =
            Storage::mount_archives(fs, "/", std::iter::empty::<&str>()).expect("mount storage");

        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut storage = Box::new(storage);
        let mut host = CompatHost::new();
        if codec {
            host.set_image_codec(
                |_bytes, _hint| Ok(DecodedImage::new(2, 2, DECODED_RGBA.to_vec())),
                |_bytes, _hint| {
                    Ok(ImageInfo {
                        width: 2,
                        height: 2,
                        has_alpha: true,
                        format: Some("img".to_string()),
                    })
                },
                |_name, _type, _width, _height, _rgba| Ok(()),
            );
        }
        // SAFETY: the boxed scene and storage outlive the host.
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
            host.services_mut()
                .attach_storage(storage.as_mut() as *mut Storage);
        }
        host.install(&engine).expect("install compat surface");
        Env {
            host,
            engine,
            _scene: scene,
            _storage: storage,
        }
    }

    /// Replaces the installed codec with one whose save closure records the
    /// call instead of writing.
    fn record_saves(&self, saved: Arc<Mutex<Option<SavedCall>>>) {
        self.host.set_image_codec(
            |_bytes, _hint| Ok(DecodedImage::new(2, 2, DECODED_RGBA.to_vec())),
            |_bytes, _hint| {
                Ok(ImageInfo {
                    width: 2,
                    height: 2,
                    has_alpha: true,
                    format: Some("img".to_string()),
                })
            },
            move |name, type_name, width, height, rgba| {
                *saved.lock().expect("save lock") = Some(SavedCall {
                    name: name.to_string(),
                    type_name: type_name.to_string(),
                    width,
                    height,
                    bytes: rgba.len(),
                });
                Ok(())
            },
        );
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "bitmap-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, "bitmap-test")
            .map_err(|error| error.to_string())
    }

    fn eval_ok(&self, expression: &str) -> TjsValue {
        self.eval(expression)
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
    }

    fn eval_int(&self, expression: &str) -> i64 {
        match self.eval_ok(expression) {
            TjsValue::Integer(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    fn error(&self, expression: &str) -> String {
        self.eval(expression)
            .expect_err(&format!("{expression:?} should have raised"))
    }
}

#[test]
fn load_populates_the_bitmap_and_returns_metainfo() {
    let _lock = vm_lock();
    let env = Env::new(true);

    env.run("var b = new Bitmap(1, 1); var meta = b.load('images/test.img');");

    // The reference `Bitmap` member list registers no `id`/`nativeId`
    // (`reference/cpp/core/visual/BitmapIntf.cpp:227`), so reading them raises.
    assert!(env.eval("b.id").is_err(), "Bitmap has no `id` member");
    assert!(
        env.eval("b.nativeId").is_err(),
        "Bitmap has no `nativeId` member"
    );
    assert_eq!(env.eval_int("b.width"), 2);
    assert_eq!(env.eval_int("b.height"), 2);
    // Straight-alpha RGBA (1, 2, 3, 255) reads back as ARGB 0xff010203.
    assert_eq!(env.eval_int("b.getPixel(0, 0)"), 0xff01_0203);
    assert_eq!(env.eval_int("b.getMaskPixel(0, 0)"), 255);
    assert_eq!(env.eval_int("meta.width"), 2);
    assert_eq!(env.eval_int("meta.height"), 2);
    assert_eq!(env.eval_int("meta.bpp"), 32);
}

#[test]
fn load_header_probes_without_decoding() {
    let _lock = vm_lock();
    let env = Env::new(true);

    assert_eq!(
        env.eval_int("Bitmap.loadHeader('images/test.img').width"),
        2
    );
    assert_eq!(
        env.eval_int("Bitmap.loadHeader('images/test.img').height"),
        2
    );
    assert_eq!(env.eval_int("Bitmap.loadHeader('images/test.img').bpp"), 32);
}

#[test]
fn get_save_option_returns_real_objects() {
    let _lock = vm_lock();
    let env = Env::new(false);

    assert_eq!(
        env.eval_ok("Bitmap.getSaveOption('bmp').bpp.type"),
        TjsValue::String("select".into())
    );
    assert_eq!(env.eval_int("Bitmap.getSaveOption('bmp').bpp.default"), 0);
    assert_eq!(
        env.eval_int("Bitmap.getSaveOption('bmp').bpp.items.length"),
        3
    );
    assert_eq!(
        env.eval_int("Bitmap.getSaveOption('jpg').quality.default"),
        90
    );
    // An accepted PNG has an (empty) options dictionary, not `void`.
    assert!(matches!(
        env.eval_ok("Bitmap.getSaveOption('png')"),
        TjsValue::Object
    ));
    // An unaccepted type yields `void`, matching the reference.
    assert!(matches!(
        env.eval_ok("Bitmap.getSaveOption('nope')"),
        TjsValue::Void
    ));
}

#[test]
fn save_encodes_and_writes_through_the_host_seam() {
    let _lock = vm_lock();
    let env = Env::new(true);
    let saved = Arc::new(Mutex::new(None));
    env.record_saves(saved.clone());

    env.run("var b = new Bitmap(2, 2); b.save('out/shot.bmp', 'bmp');");

    let call = saved
        .lock()
        .expect("save lock")
        .clone()
        .expect("save called");
    assert_eq!(call.name, "out/shot.bmp");
    assert_eq!(call.type_name, "bmp");
    assert_eq!(call.width, 2);
    assert_eq!(call.height, 2);
    assert_eq!(call.bytes, 16);
}

#[test]
fn save_defaults_to_bmp() {
    let _lock = vm_lock();
    let env = Env::new(true);
    let saved = Arc::new(Mutex::new(None));
    env.record_saves(saved.clone());

    env.run("var b = new Bitmap(1, 1); b.save('out/shot');");

    assert_eq!(
        saved
            .lock()
            .expect("save lock")
            .clone()
            .expect("save called")
            .type_name,
        "bmp"
    );
}

#[test]
fn load_async_dispatches_on_loaded() {
    let _lock = vm_lock();
    let env = Env::new(true);

    env.run(
        "var b = new Bitmap(); \
         b.onLoaded = function(meta, async, error, message) { \
             b.metaWidth = meta.width; \
             b.asyncFlag = async; \
             b.errorFlag = error; \
             b.errorMessage = message; \
         }; \
         b.loadAsync('images/test.img');",
    );

    assert_eq!(env.eval_int("b.width"), 2);
    assert_eq!(env.eval_int("b.metaWidth"), 2);
    assert_eq!(env.eval_int("b.asyncFlag"), 0);
    assert_eq!(env.eval_int("b.errorFlag"), 0);
    assert_eq!(
        env.eval_ok("b.errorMessage"),
        TjsValue::String(String::new())
    );
}

#[test]
fn typed_errors_when_the_codec_is_absent() {
    let _lock = vm_lock();
    let env = Env::new(false);

    let load_error = env.error("(new Bitmap(1, 1)).load('images/test.img')");
    assert!(
        load_error.contains("no image decoder is installed"),
        "{load_error}"
    );
    let header_error = env.error("Bitmap.loadHeader('images/test.img')");
    assert!(
        header_error.contains("no image header prober is installed"),
        "{header_error}"
    );
    let save_error = env.error("(new Bitmap(1, 1)).save('out/x.bmp', 'bmp')");
    assert!(
        save_error.contains("no image encoder is installed"),
        "{save_error}"
    );

    // `loadAsync` reports the failure through `onLoaded` instead of raising.
    env.run(
        "var f = new Bitmap(); \
         f.onLoaded = function(meta, async, error, message) { \
             f.sawError = error; \
             f.sawMessage = message; \
         }; \
         f.loadAsync('images/test.img');",
    );
    assert_eq!(env.eval_int("f.sawError"), 1);
    match env.eval_ok("f.sawMessage") {
        TjsValue::String(message) => {
            assert!(
                message.contains("no image decoder is installed"),
                "{message}"
            );
        }
        other => panic!("expected the load error message, got {other:?}"),
    }
}

#[test]
fn the_name_constructor_loads_the_storage_image() {
    let _lock = vm_lock();
    let env = Env::new(true);

    env.run("var b = new Bitmap('images/test.img');");

    // reference/cpp/core/visual/BitmapIntf.cpp:20-27: a string first argument
    // loads the named storage image.
    assert_eq!(env.eval_int("b.width"), 2);
    assert_eq!(env.eval_int("b.height"), 2);
    assert_eq!(env.eval_int("b.getPixel(0, 0)"), 0xff01_0203);
    assert_eq!(env.eval_int("b.getPixel(1, 1)"), 0xff0a_0b0c);
}

#[test]
fn the_size_constructor_allocates_a_transparent_bitmap() {
    let _lock = vm_lock();
    let env = Env::new(true);

    env.run("var b = new Bitmap(4, 3);");

    // reference/cpp/core/visual/BitmapIntf.cpp:28-36: two numeric arguments
    // size a new all-zero image.
    assert_eq!(env.eval_int("b.width"), 4);
    assert_eq!(env.eval_int("b.height"), 3);
    assert_eq!(env.eval_int("b.getPixel(0, 0)"), 0);
    assert_eq!(env.eval_int("b.getMaskPixel(0, 0)"), 0);
}

#[test]
fn the_empty_constructor_makes_the_initial_bitmap() {
    let _lock = vm_lock();
    let env = Env::new(true);

    env.run("var b = new Bitmap();");

    // reference/cpp/core/visual/LayerIntf.cpp:104-110: the initial bitmap is a
    // 32x32 transparent white rectangle.
    assert_eq!(env.eval_int("b.width"), 32);
    assert_eq!(env.eval_int("b.height"), 32);
    assert_eq!(env.eval_int("b.getPixel(0, 0)"), 0x00ff_ffff);
    assert_eq!(env.eval_int("b.getMaskPixel(0, 0)"), 0);
}

#[test]
fn a_single_numeric_argument_leaves_the_bitmap_null() {
    let _lock = vm_lock();
    let env = Env::new(true);

    env.run("var b = new Bitmap(5);");

    // reference/cpp/core/visual/BitmapIntf.cpp:37-43: a lone non-string
    // argument takes neither branch, so the image stays null and every member
    // that needs one raises `TVPNotDrawableLayerType`.
    assert!(env.eval("b.width").is_err(), "a null bitmap has no width");
    assert!(env.eval("b.getPixel(0, 0)").is_err());
    assert!(env.eval("b.save('out.bmp')").is_err());
    // The buffer members report absence instead
    // (reference/cpp/core/visual/BitmapIntf.cpp:194-210).
    assert_eq!(env.eval_int("b.buffer"), 0);
    assert_eq!(env.eval_int("b.bufferPitch"), 0);
}
