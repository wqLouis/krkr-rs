//! Integration tests for the shared L3 resource resolver: the default-face
//! fallback, font enumeration, layer text drawing and layer image saving.
//!
//! The positive cases mount a directory holding a system font and skip
//! gracefully when no usable font is present (mirroring the L2/L1 text
//! tests). The image saver is a synthetic host codec that writes the raw
//! bitmap bytes into the mounted storage, so the saved bytes can be read back
//! through the same [`engine::Storage`].
//!
//! The tests serialize on the shared VM lock because the engine → context
//! binding is process-wide and single.

mod common;

use std::path::{Path, PathBuf};

use common::{unique_dir, vm_lock};
use compat::CompatHost;
use engine::Scene;
use engine::Storage;
use fs::FileSystem;
use tjs::{Tjs2Engine, TjsValue};

/// System fonts tried by the tests; a missing/unusable font skips the case
/// rather than failing.
const TEST_FONT_PATHS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
];

/// Find a system font the L2 text engine can actually shape.
fn find_test_font() -> Option<PathBuf> {
    for candidate in TEST_FONT_PATHS {
        let path = Path::new(candidate);
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        if engine::text::TextEngine::new().add_font(&bytes, 0).is_err() {
            continue;
        }
        return Some(path.to_path_buf());
    }
    None
}

/// A bound VM + host + scene + storage mounted over a temp directory that
/// holds a copy of the test font as `gamefont.ttf`.
///
/// Fields drop in declaration order, so `_host` (which owns the process-wide
/// binding and the raw scene/storage pointers) drops before the values it
/// references.
struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    scene: Box<Scene>,
    storage: Box<Storage>,
    /// Temp game directory (also the save target of the synthetic codec).
    _game_dir: PathBuf,
}

impl Env {
    fn new(font: &Path) -> Env {
        let game_dir = unique_dir("krkr-compat-resource");
        std::fs::create_dir_all(&game_dir).expect("create temp game dir");
        std::fs::copy(font, game_dir.join("gamefont.ttf")).expect("copy test font");
        let storage = Storage::mount_archives(
            FileSystem::host(&game_dir),
            game_dir.clone(),
            std::iter::empty::<&str>(),
        )
        .expect("mount game dir");

        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut storage = Box::new(storage);
        let mut host = CompatHost::new();
        let save_dir = game_dir.clone();
        // The saver writes the raw RGBA bytes into the mounted directory so
        // `Storage::read` sees the saved file (the production host encodes
        // through the L1 `media` codec instead).
        host.set_image_codec(
            |_bytes, _hint| Err("decode is not used by these tests".to_string()),
            |_bytes, _hint| Err("probe is not used by these tests".to_string()),
            move |name, _type_name, _width, _height, rgba| {
                let path = save_dir.join(name);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                }
                std::fs::write(&path, rgba).map_err(|error| error.to_string())
            },
        );
        // SAFETY: the boxed scene and storage outlive the host (declared after
        // `_host`/`engine`), matching the `attach_*` contracts.
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
            host.services_mut()
                .attach_storage(storage.as_mut() as *mut Storage);
        }
        host.install(&engine).expect("install compat surface");
        common::link_all_builtins(&engine);
        Env {
            _host: host,
            engine,
            scene,
            storage,
            _game_dir: game_dir,
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "resource-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, "resource-test")
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

    fn eval_string(&self, expression: &str) -> String {
        match self.eval_ok(expression) {
            TjsValue::String(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected a string"),
        }
    }
}

/// Reference: `TVPCreateFontStream` falls back to the default face when the
/// requested name is absent (`reference/cpp/core/visual/FontImpl.cpp:159-168`)
/// and `TVPFindFont` strips a leading `@` for vertical writing
/// (`FontImpl.cpp:277-285`). An unknown face must therefore resolve once a
/// default face has been registered, without changing the default.
#[test]
fn unknown_face_resolves_to_the_registered_default() {
    let Some(font) = find_test_font() else {
        return;
    };
    let _lock = vm_lock();
    let env = Env::new(&font);
    // Load the mounted face first so it becomes the registered default.
    env.run("var f = new Font('gamefont.ttf', 24, 0xffffffff);");
    assert!(env.eval_int("f.getTextWidth('A')") > 0);
    let default_before = env.eval_string("Font.defaultFaceName");
    assert!(!default_before.is_empty(), "a default face should be set");

    // A face that is neither in storage nor an installed family resolves
    // through the fallback rather than raising.
    env.run("var g = new Font('this-family-is-not-installed', 24, 0xffffffff);");
    let width = env.eval_int("g.getTextWidth('A')");
    assert!(
        width > 0,
        "the fallback face should shape text, got {width}"
    );
    // Falling back must not change the already-registered default.
    assert_eq!(env.eval_string("Font.defaultFaceName"), default_before);
}

/// Reference: `Font.getList` builds a TJS array of the enumerated face names
/// (`reference/cpp/core/visual/LayerIntf.cpp:11958-11985`).
#[test]
fn font_get_list_is_non_empty_after_a_face_is_registered() {
    let Some(font) = find_test_font() else {
        return;
    };
    let _lock = vm_lock();
    let env = Env::new(&font);
    env.run("var f = new Font('gamefont.ttf', 24, 0xffffffff);");
    assert!(env.eval_int("f.getTextWidth('A')") > 0);

    assert!(
        env.eval_int("f.getList(0).length") > 0,
        "getList should report at least the registered face"
    );
}

/// Reference: `Font.getGlyphDrawRect` returns the rasterized glyphs' bounding
/// rectangle (`reference/cpp/core/visual/FreeTypeFontRasterizer.cpp:225-257`).
#[test]
fn font_glyph_draw_rect_returns_a_non_empty_rect() {
    let Some(font) = find_test_font() else {
        return;
    };
    let _lock = vm_lock();
    let env = Env::new(&font);
    env.run("var f = new Font('gamefont.ttf', 24, 0xffffffff);");

    let left = env.eval_int("f.getGlyphDrawRect('A').left");
    let top = env.eval_int("f.getGlyphDrawRect('A').top");
    let right = env.eval_int("f.getGlyphDrawRect('A').right");
    let bottom = env.eval_int("f.getGlyphDrawRect('A').bottom");
    assert!(
        right > left && bottom > top,
        "getGlyphDrawRect should be a non-empty rect, got ({left}, {top}, {right}, {bottom})"
    );
}

/// Reference: `FontSystem::GetBeingFont` splits a comma-separated candidate
/// list and strips a leading `@` for vertical writing
/// (`reference/cpp/core/visual/FontSystem.cpp:58-105`).
#[test]
fn comma_separated_candidates_and_a_leading_at_resolve() {
    let Some(font) = find_test_font() else {
        return;
    };
    let _lock = vm_lock();
    let env = Env::new(&font);
    // The first candidate misses; the mounted face is the second.
    env.run("var f = new Font('not-a-real-face,gamefont.ttf', 24, 0xffffffff);");
    assert!(
        env.eval_int("f.getTextWidth('A')") > 0,
        "the second candidate should resolve"
    );
    // A leading `@` marks the vertical face; resolution strips it.
    env.run("var g = new Font('@gamefont.ttf', 24, 0xffffffff);");
    assert!(
        env.eval_int("g.getTextWidth('A')") > 0,
        "the vertical marker should not prevent resolution"
    );
}

/// Reference: `tTJSNI_BaseLayer::DrawText` rasterizes into the layer's main
/// image (`reference/cpp/core/visual/LayerIntf.cpp:4433-4477`).
#[test]
fn layer_draw_text_writes_non_transparent_pixels() {
    let Some(font) = find_test_font() else {
        return;
    };
    let _lock = vm_lock();
    let env = Env::new(&font);
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize(64, 64); \
         l.font.face = 'gamefont.ttf'; \
         l.font.height = 32; \
         l.drawText(4, 4, 'A', 0x00ffffff, 255);",
    );

    let layer = env.scene.layers.last().expect("the layer exists");
    let bitmap = env
        .scene
        .bitmap(layer.bitmap.expect("the layer has a main image"))
        .expect("the bitmap exists");
    assert!(
        bitmap.rgba.chunks_exact(4).any(|pixel| pixel[3] != 0),
        "drawText should write non-transparent pixels"
    );
}

/// Reference: `tTJSNI_BaseLayer::SaveLayerImage` encodes the main image and
/// writes it (`reference/cpp/core/visual/LayerIntf.cpp:2699-2740`).
#[test]
fn save_layer_image_writes_bytes_readable_through_storage() {
    let Some(font) = find_test_font() else {
        return;
    };
    let _lock = vm_lock();
    let env = Env::new(&font);
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize(4, 4); \
         l.saveLayerImage('out.bmp', 'bmp');",
    );

    let bytes = env
        .storage
        .read("out.bmp")
        .expect("the saved image should be readable back through storage");
    assert_eq!(bytes.len(), 4 * 4 * 4, "the saved bytes are the RGBA image");
}

/// Reference: `ImageFunction.drawText` rasterizes into the destination bitmap
/// (`reference/cpp/core/visual/ImageFunction.cpp:967-1078`).
#[test]
fn image_function_draw_text_writes_into_the_destination_bitmap() {
    let Some(font) = find_test_font() else {
        return;
    };
    let _lock = vm_lock();
    let env = Env::new(&font);
    env.run(
        "var w = new Window(); var l = new Layer(w, null); \
         l.setImageSize(64, 64); l.font.face = 'gamefont.ttf'; l.font.height = 32; \
         var f = l.font; \
         var b = new Bitmap(64, 64); \
         ImageFunction.drawText(b, f, 4, 4, 'A', 0x00ffffff, 255, true, 0, 0, 0, 0, 0, 0, false, null);",
    );

    let bitmap = env.scene.bitmaps.last().expect("the destination bitmap");
    assert!(
        bitmap.rgba.chunks_exact(4).any(|pixel| pixel[3] != 0),
        "ImageFunction.drawText should write non-transparent pixels"
    );
}
