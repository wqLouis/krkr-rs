//! Integration tests for the L3 face of the font registry: the registered
//! name list, the default face name, `System.addFont`'s results, the
//! per-character measurement and the KAG-shaped text render.
//!
//! The faces are synthetic (`testkit::font_fixtures`), so the names, the
//! advances and the face counts are the same on every host: the narrow face
//! carries Latin glyphs only and the wide face carries one CJK glyph.
//!
//! Reference: the font map's oldest entry is the default font and its name
//! list stops there (`reference/cpp/core/visual/FontImpl.cpp:242-245`);
//! `System.addFont` reports the face count of the file
//! (`reference/cpp/plugins/addFont.cpp:21-31`); a measurement follows the
//! requested face (`reference/cpp/core/visual/LayerIntf.cpp:11702-11713`).
//!
//! The tests serialize on the shared VM lock because the engine → context
//! binding is process-wide and single.

mod common;

use std::path::PathBuf;

use common::vm_lock;
use compat::CompatHost;
use engine::Scene;
use engine::Storage;
use engine::text::TextEngine;
use fs::FileSystem;
use testkit::font_fixtures::{NARROW_NAMES, WIDE_NAMES, narrow_face, wide_face};
use tjs::{Tjs2Engine, TjsValue};

/// The narrow face's 'A' advance at 32 pixels: 400 design units of a 1000-unit
/// em, rounded to whole pixels.
const NARROW_A_ADVANCE: i64 = 13;

/// The narrow face's em box at 32 pixels: the extent a character the
/// requested face does not carry measures.
const EM_BOX: i64 = 32;

/// A bound VM + host + a storage holding the synthetic faces.
///
/// Fields drop in declaration order, so `_host` (which owns the process-wide
/// binding) drops before the engine and the storage its raw pointers
/// reference.
struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    /// The attached scene: the `Font` native keeps its state there.
    _scene: Box<Scene>,
    /// The attached text service: the registered faces and the shaping state.
    /// Boxed so the address the host holds stays stable.
    _text: Box<TextEngine>,
    _storage: Box<Storage>,
    /// The temp directory backing `_storage`; removed on drop.
    _dir: PathBuf,
}

impl Env {
    /// A bound env whose storage holds the narrow and the wide face.
    fn new() -> Env {
        let dir = std::env::temp_dir().join(format!(
            "krkr-compat-font-registry-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create temp game dir");
        std::fs::write(dir.join("narrow.ttf"), narrow_face()).expect("write the narrow face");
        std::fs::write(dir.join("wide.ttf"), wide_face()).expect("write the wide face");
        let storage = Storage::mount_archives(
            FileSystem::host(&dir),
            dir.clone(),
            std::iter::empty::<&str>(),
        )
        .expect("mount game dir");

        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut text = Box::new(TextEngine::new());
        let mut storage = Box::new(storage);
        let mut host = CompatHost::new();
        // SAFETY: the boxed scene, text service and storage outlive the host
        // (they are declared after `_host`, so they drop later), matching the
        // `attach_*` contracts.
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
            host.services_mut()
                .attach_text(text.as_mut() as *mut TextEngine);
            host.services_mut()
                .attach_storage(storage.as_mut() as *mut Storage);
        }
        host.install(&engine).expect("install compat surface");
        common::link_all_builtins(&engine);
        Env {
            _host: host,
            engine,
            _scene: scene,
            _text: text,
            _storage: storage,
            _dir: dir,
        }
    }

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, "font-registry-test")
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

    /// The elements of a TJS array expression.
    fn eval_list(&self, expression: &str) -> Vec<String> {
        let count = self.eval_int(&format!("{expression}.length"));
        (0..count)
            .map(|index| self.eval_string(&format!("{expression}[{index}]")))
            .collect()
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "font-registry-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self._dir);
    }
}

/// Reference: `TVPEnumFontsProc` reports the number of faces it loaded from
/// the file, and the reference writes that count as an **Integer**
/// (`reference/cpp/plugins/addFont.cpp:21-31`). The synthetic faces carry one
/// face each, so the count is one.
#[test]
fn system_add_font_reports_the_face_count() {
    let _lock = vm_lock();
    let env = Env::new();
    assert_eq!(env.eval_int("System.addFont('narrow.ttf')"), 1);
    assert_eq!(env.eval_int("System.addFont('wide.ttf')"), 1);
}

/// Reference: the name list enumerates the font map and stops at its oldest
/// entry, so the list is newest first and the default face's name is not in
/// it (`reference/cpp/core/visual/FontImpl.cpp:242-245`). A map that holds a
/// single name reports an empty list.
#[test]
fn the_registered_name_list_is_newest_first_without_the_oldest_name() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("Plugins.link('addFont.dll'); System.addFont('narrow.ttf'); var probe = new Font();");
    assert_eq!(
        env.eval_list("probe.getList(0)"),
        vec![NARROW_NAMES[1].to_string()],
        "the oldest name is the default face's name and stays out of the list"
    );
    env.run("System.addFont('wide.ttf');");
    assert_eq!(
        env.eval_list("probe.getList(0)"),
        vec![
            WIDE_NAMES[1].to_string(),
            WIDE_NAMES[0].to_string(),
            NARROW_NAMES[1].to_string(),
        ],
        "the list is newest first and leaves the oldest name out"
    );
}

/// The flags argument selects the list the reference reports; the port
/// reports the same registered names for every flag value
/// (`reference/cpp/core/visual/LayerIntf.cpp:11958-11985`).
#[test]
fn the_name_list_does_not_depend_on_the_flags() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("Plugins.link('addFont.dll'); System.addFont('narrow.ttf'); var probe = new Font();");
    assert_eq!(
        env.eval_list("probe.getList(0)"),
        env.eval_list("probe.getList(1)")
    );
}

/// Reference: the default font is the oldest entry of the font map
/// (`reference/cpp/core/visual/FontImpl.cpp:242-245`), so a face registered
/// later does not move it.
#[test]
fn the_default_face_name_is_the_oldest_registered_name() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("Plugins.link('addFont.dll'); System.addFont('narrow.ttf');");
    assert_eq!(env.eval_string("Font.defaultFaceName"), NARROW_NAMES[0]);
    env.run("System.addFont('wide.ttf');");
    assert_eq!(
        env.eval_string("Font.defaultFaceName"),
        NARROW_NAMES[0],
        "a later registration must not move the default face"
    );
}

/// Reference: the measurement is the requested face's extent
/// (`reference/cpp/core/visual/LayerIntf.cpp:11702-11713`), so a character
/// the requested face does not carry measures the face's em box and never
/// the advance of the face that would draw it.
#[test]
fn the_measurement_follows_the_requested_face() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&format!(
        "Plugins.link('addFont.dll'); \
         System.addFont('narrow.ttf'); \
         var narrow = new Font('{}', 32, 0);",
        NARROW_NAMES[0]
    ));
    assert_eq!(
        env.eval_int("narrow.getTextWidth('A')"),
        NARROW_A_ADVANCE,
        "a character the requested face carries measures the face's advance"
    );
    assert_eq!(
        env.eval_int("narrow.getTextWidth('\u{4E00}')"),
        EM_BOX,
        "a character the requested face lacks measures the face's em box"
    );
}

/// The KAG text renderer measures with the registered face: the characters it
/// positions carry that face's per-character advance.
#[test]
fn the_kag_text_render_measures_with_the_registered_face() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "Plugins.link('addFont.dll'); \
         System.addFont('narrow.ttf'); \
         var t = new TextRenderBase(); \
         t.defaultFace = 'narrow.ttf'; \
         t.setRenderSize(200, 100); \
         t.render('AB', 0, 0, 0, false); \
         t.done();",
    );
    assert_eq!(env.eval_int("t.renderCount"), 2);
    assert_eq!(env.eval_string("t.renderText"), "AB");
    // The narrow face's 'A' and 'B' advances at the default 24-pixel size:
    // 400 and 500 design units of a 1000-unit em.
    assert_eq!(env.eval_int("t.getCharacters(0, 0)[0].cw"), 10);
    assert_eq!(env.eval_int("t.getCharacters(0, 0)[1].cw"), 12);
}

/// The path the face was read from resolves the face it registered, so a
/// script can name a font file instead of its family.
#[test]
fn the_registered_file_name_resolves_its_face() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "Plugins.link('addFont.dll'); \
         System.addFont('narrow.ttf'); \
         var by_file = new Font('narrow.ttf', 24, 0); \
         var by_family = new Font('Krkr Test Narrow', 24, 0);",
    );
    assert_eq!(
        env.eval_int("by_file.getTextWidth('A')"),
        env.eval_int("by_family.getTextWidth('A')"),
        "the file name and the family name must resolve the same face"
    );
}
