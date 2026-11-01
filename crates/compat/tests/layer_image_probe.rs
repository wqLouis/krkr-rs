//! The L3 layer image path resolves an image request through the shared
//! image-extension list.
//!
//! The graphic loader registers one handler per extension
//! (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:176-206`) and, for a
//! request that carries no extension of its own, suggests the first
//! registered extension whose file exists
//! (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1591-1613`). The `.tlg`,
//! `.tlg5` and `.tlg6` handlers are registered there, so a request that names
//! no extension reaches a TLG file when one exists.
//!
//! The shipped 【KR】LimeLight Lemonade Jam stores exactly that: `uipsd.xp3`
//! holds `title__pack.tlg`, `title_jp__pack.tlg` and `title__bg0.tlg` (the
//! magic `TLG5.0\0raw\x1a`), and the game's UI definitions load them by their
//! base name — `title__pack`, `title_jp__pack`, `title__bg0`. The test below
//! pins that a `Layer` load reaches the `.tlg` candidate and hands the
//! *resolved* name to the decoder, so the TLG decoder is the one that runs.

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use api::fs::{FsPath, OpenMode};
use api::media::{DecodedImage, ImageInfo};
use common::vm_lock;
use compat::CompatHost;
use engine::{Scene, Storage};
use fs::FileSystem;
use testkit::{encode_tlg5, test_image};
use tjs::Tjs2Engine;

/// The image the synthetic codec decodes every request to.
const WIDTH: u32 = 24;
const HEIGHT: u32 = 16;

/// Writes `data` to `path` in an in-memory file system.
fn write_memory_file(fs: &FileSystem, path: &str, data: &[u8]) {
    let file = fs::open(fs, &FsPath::host(path), OpenMode::Write).expect("open memory file");
    fs::write_at(fs, &file, 0, data).expect("write memory file");
    fs::flush(fs, &file).expect("flush memory file");
}

/// A bound VM + host + scene whose storage holds `shot.tlg`, decoded by a
/// synthetic codec that records the name it was handed.
///
/// The boxed scene and storage are declared **after** the host so the host
/// (which stores their raw addresses) is dropped first.
struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    _scene: Box<Scene>,
    /// Kept alive for the host's raw pointer; never read directly.
    _storage: Box<Storage>,
    /// The names the codec was asked to decode, in order.
    hints: Rc<RefCell<Vec<String>>>,
}

impl Env {
    /// Mounts a storage holding a TLG5 image under the base name `shot`.
    fn with_tlg_image() -> Env {
        let fs = FileSystem::memory();
        let width = usize::try_from(WIDTH).expect("image width");
        let height = usize::try_from(HEIGHT).expect("image height");
        let pixels = test_image(width, height);
        let encoded = encode_tlg5(&pixels, width, height, 4, 8);
        write_memory_file(&fs, "shot.tlg", &encoded);
        let storage =
            Storage::mount_archives(fs, "/", std::iter::empty::<&str>()).expect("mount storage");
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut storage = Box::new(storage);
        let mut host = CompatHost::new();
        let hints: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let decode_hints = Rc::clone(&hints);
        let probe_hints = Rc::clone(&hints);
        let pixel_count = usize::try_from(WIDTH * HEIGHT * 4).expect("pixel count");
        let rgba = vec![0x7fu8; pixel_count];
        host.set_image_codec(
            move |_bytes, hint| {
                if let Some(name) = hint {
                    decode_hints.borrow_mut().push(name.to_string());
                }
                Ok(DecodedImage::new(WIDTH, HEIGHT, rgba.clone()))
            },
            move |_bytes, hint| {
                if let Some(name) = hint {
                    probe_hints.borrow_mut().push(name.to_string());
                }
                Ok(ImageInfo {
                    width: WIDTH,
                    height: HEIGHT,
                    has_alpha: true,
                    format: Some("tlg5".to_string()),
                })
            },
            |_name, _type, _width, _height, _rgba| Ok(()),
        );
        // SAFETY: the boxed scene and storage outlive the host (they are
        // declared after the host), matching `attach_scene`/`attach_storage`.
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
            host.services_mut()
                .attach_storage(storage.as_mut() as *mut Storage);
        }
        host.install(&engine).expect("install compat surface");
        Env {
            _host: host,
            engine,
            _scene: scene,
            _storage: storage,
            hints,
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script_discard(script, "layer-image-probe-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval_int(&self, expression: &str) -> i64 {
        match self
            .engine
            .eval(expression, "layer-image-probe-test")
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
        {
            tjs::TjsValue::Integer(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }
}

/// A layer load that names no extension reaches the `.tlg` file and installs
/// it: the image request is probed with the shared extension list, and the
/// resolved name (`shot.tlg`) is what the decoder is handed.
///
/// Reference: `reference/cpp/core/visual/GraphicsLoaderIntf.cpp:176-206` (the
/// `.tlg` handler is registered) and `:1591-1613` (a request without an
/// extension is resolved to the first registered extension whose file exists).
#[test]
fn a_layer_load_reaches_the_tlg_candidate_of_the_shared_list() {
    let _lock = vm_lock();
    let env = Env::with_tlg_image();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.loadImages('shot');",
    );
    // The image is installed, so the layer's image size is the decoded one.
    assert_eq!(env.eval_int("l.imageWidth"), i64::from(WIDTH));
    assert_eq!(env.eval_int("l.imageHeight"), i64::from(HEIGHT));
    // The decoder was handed the resolved name, so the TLG decoder is the one
    // that ran: the hint carries the `.tlg` extension.
    let hints = env.hints.borrow().clone();
    assert!(
        hints.iter().any(|hint| hint == "shot.tlg"),
        "the decoder must see the resolved name: {hints:?}"
    );
}

/// The same request spelled with its extension loads the same file: the
/// resolved name is the one the decoder sees either way.
#[test]
fn a_layer_load_with_the_extension_resolves_to_the_same_name() {
    let _lock = vm_lock();
    let env = Env::with_tlg_image();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.loadImages('shot.tlg');",
    );
    assert_eq!(env.eval_int("l.imageWidth"), i64::from(WIDTH));
    let hints = env.hints.borrow().clone();
    assert!(
        hints.iter().any(|hint| hint == "shot.tlg"),
        "the decoder must see the resolved name: {hints:?}"
    );
}
