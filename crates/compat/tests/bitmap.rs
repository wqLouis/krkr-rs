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
        Env::with_files(codec, &[("/images/test.img", b"not-a-real-image")])
    }

    /// Like [`Env::new`], but the in-memory storage also holds `files`
    /// (`(path, bytes)`), written before the storage is mounted.
    fn with_files(codec: bool, files: &[(&str, &[u8])]) -> Env {
        let fs = FileSystem::memory();
        for &(path, data) in files {
            write_memory_file(&fs, path, data);
        }
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

    /// Replaces the installed codec with one whose decode closure records each
    /// resolved name (the storage name that actually resolved) and returns the
    /// shared 2x2 test image.
    fn record_decodes(&self, hints: Arc<Mutex<Vec<String>>>) {
        self.host.set_image_codec(
            move |_bytes, hint| {
                hints
                    .lock()
                    .expect("decode lock")
                    .push(hint.unwrap_or_default().to_string());
                Ok(DecodedImage::new(2, 2, DECODED_RGBA.to_vec()))
            },
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
fn save_strips_the_file_media_prefix() {
    let _lock = vm_lock();
    let env = Env::new(true);
    let saved = Arc::new(Mutex::new(None));
    env.record_saves(saved.clone());

    // The reference resolves the write target through
    // `TVPCreateStream(TVPNormalizeStorageName(name), TJS_BS_WRITE)`, which
    // parses the `file://` media form down to the local path
    // (`reference/cpp/core/visual/impl/GraphicsLoaderImpl.cpp:56-63`,
    // `reference/cpp/core/base/StorageIntf.cpp:241-430`). The seam that reaches
    // the host writer must hand it the bare name, exactly as the decode path
    // strips before `Storage::read`.
    env.run("var b = new Bitmap(1, 1); b.save('file://./out/shot.bmp', 'bmp');");

    assert_eq!(
        saved
            .lock()
            .expect("save lock")
            .clone()
            .expect("save called")
            .name,
        "/out/shot.bmp"
    );
}

#[test]
fn save_with_a_file_media_prefix_writes_the_stripped_path() {
    let _lock = vm_lock();

    // The reference resolves the write target through
    // `TVPCreateStream(TVPNormalizeStorageName(storagename), TJS_BS_WRITE)`,
    // which parses the `file://` media form to the local path
    // (`reference/cpp/core/visual/impl/GraphicsLoaderImpl.cpp:56-63`,
    // `reference/cpp/core/base/StorageIntf.cpp:241-430`), so the host writer
    // receives `/out/shot.bmp` and the file lands there, not under a
    // `file:`-named directory.
    let fs = FileSystem::memory();
    let mut storage = Box::new(
        Storage::mount_archives(fs.clone(), "/", std::iter::empty::<&str>())
            .expect("mount storage"),
    );
    let engine = Box::new(Tjs2Engine::new().expect("create engine"));
    let mut scene = Box::new(Scene::default());
    let mut host = CompatHost::new();
    let writer = fs.clone();
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
        move |name, _type, _width, _height, rgba| {
            write_memory_file(&writer, name, rgba);
            Ok(())
        },
    );
    // SAFETY: the boxed scene and storage outlive the host.
    unsafe {
        host.services_mut()
            .attach_scene(scene.as_mut() as *mut Scene);
        host.services_mut()
            .attach_storage(storage.as_mut() as *mut Storage);
    }
    host.install(&engine).expect("install compat surface");

    engine
        .exec_script(
            "var b = new Bitmap(2, 2); b.save('file://./out/shot.bmp', 'bmp');",
            "bitmap-test",
        )
        .expect("run the save script");

    assert!(
        fs::exists(&fs, &FsPath::host("/out/shot.bmp")),
        "the prefixed save must land at the stripped path"
    );
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
fn save_passes_the_type_verbatim() {
    let _lock = vm_lock();
    let env = Env::new(true);
    let saved = Arc::new(Mutex::new(None));
    env.record_saves(saved.clone());

    // A present, non-void `type` is used verbatim, including the empty
    // string; it is not coerced to "bmp". Only an absent/void argument falls
    // back to "bmp" (`BitmapIntf.cpp:330-333`).
    env.run("var b = new Bitmap(1, 1); b.save('out/blank', '');");
    assert_eq!(
        saved
            .lock()
            .expect("save lock")
            .clone()
            .expect("save")
            .type_name,
        ""
    );

    env.run("var b = new Bitmap(1, 1); b.save('out/shot.bmp', 'bmp');");
    assert_eq!(
        saved
            .lock()
            .expect("save lock")
            .clone()
            .expect("save")
            .type_name,
        "bmp"
    );
}

#[test]
fn save_reads_the_bpp_index_from_meta() {
    let _lock = vm_lock();
    let env = Env::new(true);
    let saved = Arc::new(Mutex::new(None));

    // `TVPSaveTextureAsBMP` switches on `meta.bpp` as an **index**: 0 selects 4
    // bytes per pixel, 1 selects 3 and 2 selects 1, and an index outside that
    // range leaves the mode's own default in place
    // (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:740-770`). The type
    // string itself is never mutated (`BitmapIntf.cpp:326-341`).
    for (index, expected) in [(0, "bmp"), (1, "bmp24"), (2, "bmp8"), (24, "bmp")] {
        env.record_saves(saved.clone());
        env.run(&format!(
            "var b = new Bitmap(1, 1); b.save('out/shot.bmp', 'bmp', %[bpp: {index}]);"
        ));
        assert_eq!(
            saved
                .lock()
                .expect("save lock")
                .clone()
                .expect("save")
                .type_name,
            expected,
            "bpp index {index}"
        );
    }
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
    let header_result = env.eval_ok("Bitmap.loadHeader('images/test.img')");
    // The reference header router leaves the result void when no handler
    // accepts the data (`BitmapIntf.cpp:378-401`); it does not raise.
    assert_eq!(header_result, TjsValue::Void);
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
fn dropping_a_script_bitmap_releases_its_image() {
    let _lock = vm_lock();
    let env = Env::new(true);

    env.run("var b = new Bitmap(2, 2); b.setPixel(0, 0, 0xff112233);");
    let id = env._scene.bitmaps.last().expect("the script bitmap").id;
    // reference/cpp/core/visual/BitmapIntf.cpp:60-63: `SetPixel` writes the
    // object's image, so the scene holds the color as straight-alpha RGBA.
    assert_eq!(
        &env._scene.bitmap(id).expect("the image is real").rgba[..4],
        &[0x11, 0x22, 0x33, 0xff],
        "the script Bitmap's pixels are real"
    );

    env.run("b = null;");
    env.run("System.doCompact();");
    // reference/cpp/core/visual/BitmapIntf.cpp:46-49: `Invalidate` deletes the
    // object's bitmap; the pixel buffer is freed with the last owner
    // (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:495-503`).
    assert!(
        env._scene.bitmap(id).is_none(),
        "the destroyed script Bitmap's image is released"
    );
}

#[test]
fn a_stored_member_result_does_not_pin_the_script_bitmap() {
    let _lock = vm_lock();

    // A member-call result stored in a variable must not keep the receiver's
    // native instance alive once the script drops its last reference. Several
    // compiler shapes are covered: the same execution, a separate execution,
    // a function-local object, a helper receiving the object and a holder
    // whose member is read before the call.
    let scripts = [
        "var b = new Bitmap(2, 2); var p = b.getPixel(0, 0); b = null; System.doCompact();",
        "var b = new Bitmap(2, 2); var p = b.getPixel(0, 0); b = null;",
        "function f() { var b = new Bitmap(2, 2); var p = b.getPixel(0, 0); } f(); System.doCompact();",
        "function read(b) { return b.getPixel(0, 0); } var b = new Bitmap(2, 2); var p = read(b); b = null; System.doCompact();",
        "var h = %[b: new Bitmap(2, 2)]; var p = h.b.getPixel(0, 0); h = null; System.doCompact();",
        "var b = new Bitmap(2, 2); b.setPixel(0, 0, 0xff112233); b = null; System.doCompact();",
    ];
    for (index, script) in scripts.iter().enumerate() {
        let env = Env::new(true);
        env.run(script);
        env.run("System.doCompact();");
        let surviving = env._scene.bitmaps.len();
        assert_eq!(
            surviving, 0,
            "shape {index} leaked {surviving} bitmap(s): {script}"
        );
    }
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

/// A load that changes the image dimensions installs the decoded buffer
/// byte-for-byte, reports the new dimensions and leaves the bitmap dirty.
///
/// reference/cpp/core/visual/BitmapIntf.cpp:98-106 (`tTJSNI_Bitmap::Load`
/// replaces the object's image through `TVPLoadGraphic`).
#[test]
fn load_into_a_resized_bitmap_installs_the_decoded_pixels() {
    let _lock = vm_lock();
    let env = Env::new(true);

    env.run("var b = new Bitmap(1, 1);");
    let revision = env._scene.revision();
    env.run("b.load('images/test.img');");

    assert_eq!(env.eval_int("b.width"), 2);
    assert_eq!(env.eval_int("b.height"), 2);
    assert_eq!(env.eval_int("b.bufferPitch"), 8);
    // Straight-alpha RGBA (1, 2, 3, 255) reads back as ARGB 0xff010203, and
    // every pixel comes from the decoded buffer, not the zero-filled resize.
    assert_eq!(env.eval_int("b.getPixel(0, 0)"), 0xff01_0203);
    assert_eq!(env.eval_int("b.getPixel(1, 0)"), 0xff04_0506);
    assert_eq!(env.eval_int("b.getPixel(0, 1)"), 0xff07_0809);
    assert_eq!(env.eval_int("b.getPixel(1, 1)"), 0xff0a_0b0c);
    assert_eq!(env.eval_int("b.getMaskPixel(0, 1)"), 255);

    let bitmap = env._scene.bitmaps.last().expect("the loaded image");
    assert_eq!(bitmap.width, 2);
    assert_eq!(bitmap.height, 2);
    assert_eq!(bitmap.rgba.as_slice(), DECODED_RGBA.as_slice());
    assert!(bitmap.dirty, "a loaded image is dirty");
    assert!(
        bitmap.content_generation > 0,
        "a loaded image bumps its content generation"
    );
    assert!(
        env._scene.revision() > revision,
        "a load invalidates the scene projection"
    );
}

/// A load whose image size matches the bitmap's current size takes the same
/// field-assignment path and installs the decoded buffer unchanged.
///
/// reference/cpp/core/visual/BitmapIntf.cpp:98-106.
#[test]
fn load_into_a_same_size_bitmap_installs_the_decoded_pixels() {
    let _lock = vm_lock();
    let env = Env::new(true);

    env.run("var b = new Bitmap(2, 2); b.setPixel(0, 0, 0xff112233);");
    env.run("b.load('images/test.img');");

    assert_eq!(env.eval_int("b.width"), 2);
    assert_eq!(env.eval_int("b.height"), 2);
    assert_eq!(env.eval_int("b.getPixel(0, 0)"), 0xff01_0203);
    assert_eq!(env.eval_int("b.getPixel(1, 1)"), 0xff0a_0b0c);

    let bitmap = env._scene.bitmaps.last().expect("the loaded image");
    assert_eq!(bitmap.rgba.as_slice(), DECODED_RGBA.as_slice());
    assert!(bitmap.dirty, "a loaded image is dirty");
}

/// The literal name resolves before any extension is appended: with both
/// `images/plain` and `images/plain.png` present, the decoder is handed
/// `images/plain`.
///
/// reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1591-1610 (when the name
/// has no extension the reference appends its registered extensions until one
/// exists; the port probes the literal name first).
#[test]
fn resolution_tries_the_plain_name_before_any_extension() {
    let _lock = vm_lock();
    let files: &[(&str, &[u8])] = &[
        ("/images/plain", b"plain"),
        ("/images/plain.png", b"extended"),
    ];
    let env = Env::with_files(true, files);
    let hints = Arc::new(Mutex::new(Vec::new()));
    env.record_decodes(hints.clone());

    env.run("var b = new Bitmap(); b.load('images/plain');");

    assert_eq!(env.eval_int("b.width"), 2);
    assert_eq!(
        hints.lock().expect("decode lock").clone(),
        vec!["images/plain".to_string()],
        "the literal name is resolved without probing an extension"
    );
}

/// A name that only resolves with an appended extension is decoded under the
/// resolved name.
///
/// reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1591-1610.
#[test]
fn resolution_probes_extensions_when_the_plain_name_misses() {
    let _lock = vm_lock();
    let files: &[(&str, &[u8])] = &[("/images/only.png", b"extended")];
    let env = Env::with_files(true, files);
    let hints = Arc::new(Mutex::new(Vec::new()));
    env.record_decodes(hints.clone());

    env.run("var b = new Bitmap(); b.load('images/only');");

    assert_eq!(env.eval_int("b.width"), 2);
    assert_eq!(
        hints.lock().expect("decode lock").clone(),
        vec!["images/only.png".to_string()],
        "the `.png` candidate is resolved and handed to the decoder"
    );
}

/// A name that resolves to no candidate keeps failing with the unresolved
/// request in the message.
///
/// reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1605-1610: when no
/// extension candidate exists the reference raises.
#[test]
fn a_name_that_resolves_to_nothing_raises_and_names_the_request() {
    let _lock = vm_lock();
    let env = Env::with_files(true, &[]);

    let error = env.error("(new Bitmap(1, 1)).load('images/missing');");
    assert!(
        error.contains("Bitmap.load: storage not found: images/missing"),
        "{error}"
    );
    let header_error = env.error("Bitmap.loadHeader('images/missing')");
    assert!(
        header_error.contains("Bitmap.loadHeader: storage not found: images/missing"),
        "{header_error}"
    );
}

/// With the default limit (`0`) the cache is off, so the same name loaded
/// twice decodes twice and both loads see the same pixels.
///
/// reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1433-1434 (the cache
/// starts disabled) and `:1962-1969` (a disabled cache installs the freshly
/// loaded image instead of consulting the hash).
#[test]
fn the_default_cache_limit_decodes_every_load() {
    let _lock = vm_lock();
    let env = Env::new(true);
    let hints = Arc::new(Mutex::new(Vec::new()));
    env.record_decodes(hints.clone());

    // The disabled default reads back through the script property
    // (`reference/cpp/core/base/SystemIntf.cpp:358-361`).
    assert_eq!(env.eval_int("System.graphicCacheLimit"), 0);
    env.run("var first = new Bitmap(); first.load('images/test.img');");
    env.run("var second = new Bitmap(); second.load('images/test.img');");

    assert_eq!(
        hints.lock().expect("decode lock").clone(),
        vec!["images/test.img".to_string(), "images/test.img".to_string()],
        "the default limit decodes every load"
    );
    for (x, y) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        let first_pixel = env.eval_int(&format!("first.getPixel({x}, {y})"));
        assert_eq!(
            first_pixel,
            env.eval_int(&format!("second.getPixel({x}, {y})")),
            "pixel ({x}, {y})"
        );
    }
    // Each load installed the decoded buffer
    // (`reference/cpp/core/visual/BitmapIntf.cpp:98-106`).
    assert_eq!(env.eval_int("second.width"), 2);
    assert_eq!(env.eval_int("second.getPixel(0, 0)"), 0xff01_0203);
    assert_eq!(env.eval_int("second.getPixel(1, 1)"), 0xff0a_0b0c);
}

/// A limit above zero enables the cache: the second load of the same name
/// runs no decoder and its pixels are identical to the first load's.
/// Returning the limit to `0` disables the cache again and the next loads
/// decode.
///
/// reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1886-1908 (the load
/// consults the cache and returns the stored image), `:1937-1960` (a miss
/// decodes and stores), `:2423-2441` (the limit's setter).
#[test]
fn an_enabled_cache_decodes_once_per_name() {
    let _lock = vm_lock();
    let env = Env::new(true);
    let hints = Arc::new(Mutex::new(Vec::new()));
    env.record_decodes(hints.clone());

    env.run("System.graphicCacheLimit = 1048576;");
    assert_eq!(env.eval_int("System.graphicCacheLimit"), 1048576);
    env.run("var first = new Bitmap(); first.load('images/test.img');");
    env.run("var second = new Bitmap(); second.load('images/test.img');");

    assert_eq!(
        hints.lock().expect("decode lock").clone(),
        vec!["images/test.img".to_string()],
        "the second load is a cache hit"
    );
    for (x, y) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        let first_pixel = env.eval_int(&format!("first.getPixel({x}, {y})"));
        assert_eq!(
            first_pixel,
            env.eval_int(&format!("second.getPixel({x}, {y})")),
            "pixel ({x}, {y})"
        );
    }
    assert_eq!(env.eval_int("second.width"), 2);
    assert_eq!(env.eval_int("second.getPixel(0, 0)"), 0xff01_0203);

    env.run("System.graphicCacheLimit = 0;");
    env.run("var third = new Bitmap(); third.load('images/test.img');");
    env.run("var fourth = new Bitmap(); fourth.load('images/test.img');");
    assert_eq!(
        hints.lock().expect("decode lock").clone(),
        vec![
            "images/test.img".to_string(),
            "images/test.img".to_string(),
            "images/test.img".to_string()
        ],
        "limit 0 disables the cache and both loads decode"
    );
}

/// A cache hit installs the stored pixels into the loading Bitmap, so a
/// `setPixel` write on one Bitmap cannot reach the cached image: the next
/// load of the same name still reads the pristine decoded pixels.
///
/// `SetPixel` writes the receiving Bitmap's own image
/// (`reference/cpp/core/visual/BitmapIntf.cpp:60-66`); the load returns the
/// cached image (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1897-1906`)
/// and a write severs that sharing before touching pixels
/// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:641-661`).
#[test]
fn a_cache_hit_returns_pristine_pixels_after_a_set_pixel_write() {
    let _lock = vm_lock();
    let env = Env::new(true);
    let hints = Arc::new(Mutex::new(Vec::new()));
    env.record_decodes(hints.clone());

    env.run("System.graphicCacheLimit = 1048576;");
    env.run("var first = new Bitmap(); first.load('images/test.img');");
    env.run("first.setPixel(0, 0, 0xff112233);");
    env.run("var second = new Bitmap(); second.load('images/test.img');");

    assert_eq!(
        hints.lock().expect("decode lock").len(),
        1,
        "the second load is a cache hit"
    );
    // The write reached only the first Bitmap.
    assert_eq!(env.eval_int("first.getPixel(0, 0)"), 0xff11_2233);
    // The reloaded Bitmap still has the decoded pixels.
    assert_eq!(env.eval_int("second.getPixel(0, 0)"), 0xff01_0203);
    assert_eq!(env.eval_int("second.getPixel(1, 1)"), 0xff0a_0b0c);
}

/// Two different names occupy two cache entries: each decodes once and
/// every later load hits its own entry with its own pixels.
///
/// The cache key is the name plus the load parameters
/// (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1257-1264`), and the
/// load consults exactly that key
/// (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1886-1897`).
#[test]
fn two_different_names_are_cached_separately() {
    let _lock = vm_lock();
    let files: &[(&str, &[u8])] = &[
        ("/images/alpha.img", b"AAAA"),
        ("/images/beta.img", b"BBBB"),
    ];
    let env = Env::with_files(true, files);
    let hints = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&hints);
    // Each file decodes to a distinct grey so the two entries' pixels are
    // distinguishable; the resolved name is recorded like
    // `Env::record_decodes`.
    env.host.set_image_codec(
        move |bytes, hint| {
            recorder
                .lock()
                .expect("decode lock")
                .push(hint.unwrap_or_default().to_string());
            let seed = *bytes.first().expect("the test file has content");
            let mut rgba = Vec::with_capacity(16);
            for _ in 0..4 {
                rgba.extend_from_slice(&[seed, seed, seed, 0xff]);
            }
            Ok(DecodedImage::new(2, 2, rgba))
        },
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

    env.run("System.graphicCacheLimit = 1048576;");
    env.run("var alpha = new Bitmap(); alpha.load('images/alpha.img');");
    env.run("var beta = new Bitmap(); beta.load('images/beta.img');");
    env.run("var alphaAgain = new Bitmap(); alphaAgain.load('images/alpha.img');");
    env.run("var betaAgain = new Bitmap(); betaAgain.load('images/beta.img');");

    assert_eq!(
        hints.lock().expect("decode lock").clone(),
        vec![
            "images/alpha.img".to_string(),
            "images/beta.img".to_string()
        ],
        "each name decodes once and both reloads hit"
    );
    // The files start with 'A' (0x41) and 'B' (0x42): each reload reads its
    // own entry's pixels.
    assert_eq!(env.eval_int("alpha.getPixel(0, 0)"), 0xff41_4141);
    assert_eq!(env.eval_int("alphaAgain.getPixel(0, 0)"), 0xff41_4141);
    assert_eq!(env.eval_int("beta.getPixel(0, 0)"), 0xff42_4242);
    assert_eq!(env.eval_int("betaAgain.getPixel(0, 0)"), 0xff42_4242);
}
