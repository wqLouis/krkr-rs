//! Integration tests for the `TextRenderBase` plugin's style dictionary.
//!
//! The style dictionary a game hands to `setDefault` has two spellings. The
//! shipped `textrender.dll` build the game installs reads the style members
//! under lower-case names — `face`, `bold`, `fontsize`, `bigfontsize`,
//! `smallfontsize`, `rubysize`, `rubyoffset`, `color`, `shadow`,
//! `shadowcolor`, `shadowdiff`, `edge`, `edgecolor`, `linespacing`,
//! `pitch`, `linesize`, `align` and `valign` — while the tree build's
//! deserializer reads `bold`, `italic`, `fontSize`, `fontScale`, `face`,
//! `chColor`, `rubySize`, `rubyOffset`, `shadow`, `shadowColor`,
//! `edge`, `edgeColor`, `lineSpacing`, `pitch` and `lineSize`.
//!
//! Oracle for the lower-case spellings: the shipped `textrender.dll`
//! (wamsoft `textrend` 1.0.0.1, 2014), file name `textrender.dll`,
//! sha256[:12] `0a014cc2b581`; the member names are the wide strings in the
//! binary's `.rdata` and the state deserializer reads them in the order the
//! committed fixture lists them.
//!
//! Oracle for the camel-case spellings: `reference/cpp/plugins/TextRender.cpp`
//! — the state deserializer reads `bold`, `italic`, `fontSize`,
//! `fontScale`, `face`, `chColor`, `rubySize`, `rubyOffset`,
//! `shadow`, `shadowColor`, `edge`, `edgeColor`, `lineSpacing`,
//! `pitch` and `lineSize` (`TextRender.cpp:137-158`). The tree build
//! reads `italic` and `fontScale` as dictionary keys although no shipped
//! build does: they are class properties in the shipped build, and the tree
//! build's deserializer takes them from the dictionary, so the port does too.
//!
//! The tests serialize on the shared VM lock because the engine → context
//! binding is process-wide and single.

mod common;

use std::path::{Path, PathBuf};

use common::vm_lock;
use compat::CompatHost;
use engine::Scene;
use engine::Storage;
use engine::text::TextEngine;
use fs::FileSystem;
use tjs::{Tjs2Engine, TjsValue};

/// System fonts tried by the tests; a missing/unusable font skips the
/// font-dependent case rather than failing.
const TEST_FONT_PATHS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
];

/// Find a system font the L2 text engine can actually shape.
///
/// Returns the font's directory (the storage mount root) and its file name.
fn test_font() -> Option<(PathBuf, String)> {
    for candidate in TEST_FONT_PATHS {
        let path = Path::new(candidate);
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        if TextEngine::new().add_font(&bytes, 0).is_err() {
            continue;
        }
        let dir = path.parent()?.to_path_buf();
        let name = path.file_name()?.to_string_lossy().into_owned();
        return Some((dir, name));
    }
    None
}

/// A bound VM + host + attached scene and optional storage.
///
/// Fields drop in declaration order, so `_host` (which owns the process-wide
/// binding) drops before the engine/scene/storage its raw pointers reference.
struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    /// Kept alive for the host's raw pointer; never read directly.
    _scene: Box<Scene>,
    /// Kept alive for the host's raw pointer; `None` means no font source.
    _storage: Option<Box<Storage>>,
}

impl Env {
    fn new(storage: Option<Storage>) -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut storage = storage.map(Box::new);
        let mut host = CompatHost::new();
        // SAFETY: the boxed scene and storage outlive the host (they are
        // declared after `_host`, so they drop later), matching the `attach_*`
        // contracts.
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
            if let Some(storage) = storage.as_mut() {
                host.services_mut()
                    .attach_storage(storage.as_mut() as *mut Storage);
            }
        }
        host.install(&engine).expect("install compat surface");
        common::link_all_builtins(&engine);
        Env {
            _host: host,
            engine,
            _scene: scene,
            _storage: storage,
        }
    }

    /// A bound env with a mounted system font directory.
    fn with_font() -> Option<(Env, String)> {
        let (dir, name) = test_font()?;
        let storage = Storage::mount_archives(
            FileSystem::host(&dir),
            dir.clone(),
            std::iter::empty::<&str>(),
        )
        .expect("mount font directory");
        Some((Env::new(Some(storage)), name))
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "text-render-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval_ok(&self, expression: &str) -> TjsValue {
        self.engine
            .eval(expression, "text-render-test")
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
    }

    fn eval_int(&self, expression: &str) -> i64 {
        match self.eval_ok(expression) {
            TjsValue::Integer(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    fn eval_real(&self, expression: &str) -> f64 {
        match self.eval_ok(expression) {
            TjsValue::Real(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected a real"),
        }
    }
}

/// One row of the committed fixture with the shipped build's style members.
struct FixtureMember {
    /// The member's name in the style dictionary.
    member: String,
    /// The class property that shows the member, if the plugin has one.
    property: Option<String>,
    /// The value the member is driven with.
    drive: String,
    /// The value the property must hold afterwards.
    expected: Option<String>,
}

/// The committed fixture with the shipped build's style member names.
///
/// The fixture is `tests/fixtures/textrender_dll_state.txt`; its header names
/// the DLL, its sha256 and where the names come from, and every row is the
/// member name, the offset of its wide string in the binary, the state field
/// the deserializer stores it in, the class property that shows it, the value
/// the member is driven with and the value the property must then hold.
fn fixture_members() -> Vec<FixtureMember> {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/textrender_dll_state.txt");
    let text = std::fs::read_to_string(&path).expect("read the TextRender DLL fixture");
    let mut members = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(fields.len(), 6, "unexpected fixture row: {line:?}");
        let property = (fields[3] != "-").then(|| fields[3].to_string());
        let expected = (fields[5] != "-").then(|| fields[5].to_string());
        members.push(FixtureMember {
            member: fields[0].to_string(),
            property,
            drive: fields[4].to_string(),
            expected,
        });
    }
    assert!(!members.is_empty(), "the fixture lists no member");
    members
}

/// The style dictionary the shipped `textrender.dll` reads: the lower-case
/// member names. The dictionary is the one a game builds for the plugin, so
/// the values must reach the default style — the shipped build reads
/// `color`/`fontsize`/`linespacing`/… (sha256[:12] `0a014cc2b581`,
/// the state deserializer at 0x100022a0).
#[test]
fn set_default_reads_the_lower_case_style_spellings() {
    let _lock = vm_lock();
    let env = Env::new(None);
    env.run(
        "var t = new TextRenderBase(); \
         t.setDefault(%[ \
             color: 0x442200, \
             fontsize: 16, \
             linespacing: 0, \
             rubysize: 12, \
             rubyoffset: -3, \
             linesize: 16, \
             face: 'Arial', \
             bold: 1, \
             italic: 1, \
             shadow: 0, \
             edge: 0, \
             pitch: 2 ]);",
    );

    assert_eq!(env.eval_int("t.defaultChColor"), 0x442200);
    assert_eq!(env.eval_int("t.defaultFontSize"), 16);
    assert_eq!(env.eval_int("t.defaultLineSpacing"), 0);
    assert_eq!(env.eval_int("t.defaultRubySize"), 12);
    assert_eq!(env.eval_int("t.defaultRubyOffset"), -3);
    assert_eq!(env.eval_int("t.defaultLineSize"), 16);
    assert_eq!(env.eval_int("t.defaultBold"), 1);
    assert_eq!(env.eval_int("t.defaultItalic"), 1);
    assert_eq!(env.eval_int("t.defaultShadow"), 0);
    assert_eq!(env.eval_int("t.defaultEdge"), 0);
    assert_eq!(env.eval_int("t.defaultPitch"), 2);
}

/// The camel-case spellings the tree build's deserializer reads are read as
/// well (`reference/cpp/plugins/TextRender.cpp:137-158`); the tree build
/// reads `italic` and `fontScale` from the dictionary although no shipped
/// build does.
#[test]
fn set_default_reads_the_camel_case_style_spellings() {
    let _lock = vm_lock();
    let env = Env::new(None);
    env.run(
        "var t = new TextRenderBase(); \
         t.setDefault(%[ \
             chColor: 0x112233, \
             fontSize: 20, \
             lineSpacing: 3, \
             rubySize: 7, \
             rubyOffset: -1, \
             lineSize: 9, \
             italic: 1, \
             fontScale: 1.5, \
             face: 'Times' ]);",
    );

    assert_eq!(env.eval_int("t.defaultChColor"), 0x112233);
    assert_eq!(env.eval_int("t.defaultFontSize"), 20);
    assert_eq!(env.eval_int("t.defaultLineSpacing"), 3);
    assert_eq!(env.eval_int("t.defaultRubySize"), 7);
    assert_eq!(env.eval_int("t.defaultRubyOffset"), -1);
    assert_eq!(env.eval_int("t.defaultLineSize"), 9);
    assert_eq!(env.eval_int("t.defaultItalic"), 1);
    assert_eq!(env.eval_real("t.defaultFontScale"), 1.5);
}

/// A dictionary that carries both spellings takes the lower-case value: the
/// shipped build reads only the lower-case names (sha256[:12] `0a014cc2b581`).
#[test]
fn the_lower_case_spelling_wins_when_a_dictionary_carries_both() {
    let _lock = vm_lock();
    let env = Env::new(None);
    env.run(
        "var t = new TextRenderBase(); \
         t.setDefault(%[ color: 1, chColor: 2, fontsize: 30, fontSize: 40 ]);",
    );

    assert_eq!(env.eval_int("t.defaultChColor"), 1);
    assert_eq!(env.eval_int("t.defaultFontSize"), 30);
}

/// `clear` restores the default style, so the active style follows the
/// dictionary the lower-case spelling filled
/// (`reference/cpp/plugins/TextRender.cpp:1016-1031`).
#[test]
fn the_active_style_follows_the_default_after_clear() {
    let _lock = vm_lock();
    let env = Env::new(None);
    env.run(
        "var t = new TextRenderBase(); \
         t.setDefault(%[ color: 0x442200, fontsize: 16 ]); \
         t.clear();",
    );

    assert_eq!(env.eval_int("t.chColor"), 0x442200);
    assert_eq!(env.eval_int("t.fontSize"), 16);
}

/// Every member the shipped build reads reaches the style: the fixture's rows
/// drive `setDefault` one by one and the property the row names must take
/// the value. A row without a property is a member the shipped build keeps in
/// the style and never reads back, so the port applies the same nil effect —
/// the dictionary is accepted and the style keeps the rest.
#[test]
fn every_member_of_the_shipped_builds_fixture_reaches_the_style() {
    let _lock = vm_lock();
    let env = Env::new(None);
    env.run("var t;");
    for entry in fixture_members() {
        env.run(&format!(
            "t = new TextRenderBase(); t.setDefault(%[{}: {}]);",
            entry.member, entry.drive
        ));
        let (Some(property), Some(expected)) =
            (entry.property.as_deref(), entry.expected.as_deref())
        else {
            continue;
        };
        let actual = match env.eval_ok(&format!("t.{property}")) {
            TjsValue::Integer(number) => number.to_string(),
            TjsValue::String(text) => text,
            other => panic!("t.{property} returned {other:?}, expected a number or a string"),
        };
        assert_eq!(actual, expected, "{} reaches {property}", entry.member);
    }
}

/// The big and the small size reach the layout: the `%B` and `%S` escapes
/// select them, and a dictionary that names only the base size leaves the
/// shipped build's derived sizes in place — twice the base size and half of
/// it (sha256[:12] `0a014cc2b581`, the state deserializer at 0x100022a0).
#[test]
fn the_big_and_the_small_size_reach_the_layout() {
    let _lock = vm_lock();
    let Some((env, name)) = Env::with_font() else {
        return;
    };
    env.run(&format!(
        "var t = new TextRenderBase(); \
         t.setDefault(%[ fontsize: 20, bigfontsize: 40, smallfontsize: 10, face: '{name}' ]); \
         t.setRenderSize(400, 100); \
         t.clear(); t.render('%BA', 0, 0, 0, false); t.done();"
    ));
    let big = env.eval_int("t.getCharacters(0, 0)[0].cw");
    env.run("t.clear(); t.render('A', 0, 0, 0, false); t.done();");
    let base = env.eval_int("t.getCharacters(0, 0)[0].cw");
    env.run("t.clear(); t.render('%SA', 0, 0, 0, false); t.done();");
    let small = env.eval_int("t.getCharacters(0, 0)[0].cw");
    assert!(
        big > base && base > small,
        "the big and the small size reach the layout ({big} > {base} > {small})"
    );

    // A dictionary that names only the base size leaves the derived sizes:
    // twice the base size and half of it.
    env.run(&format!(
        "var u = new TextRenderBase(); \
         u.setDefault(%[ fontsize: 30, face: '{name}' ]); \
         u.setRenderSize(400, 100); \
         u.clear(); u.render('%BA', 0, 0, 0, false); u.done();"
    ));
    let derived_big = env.eval_int("u.getCharacters(0, 0)[0].cw");
    env.run("u.clear(); u.render('%SA', 0, 0, 0, false); u.done();");
    let derived_small = env.eval_int("u.getCharacters(0, 0)[0].cw");
    assert!(
        derived_big > derived_small,
        "the derived sizes reach the layout ({derived_big} > {derived_small})"
    );
}

/// The character the layout produces carries the colour and the size the
/// dictionary gave: the drawn colour is the default style's colour and the
/// drawn position follows the size, so a dictionary in the lower-case
/// spelling lays the text out at the game's size (the shipped build reads
/// `fontsize`, sha256[:12] `0a014cc2b581`; the character dictionary is
/// `reference/cpp/plugins/TextRender.cpp:225-248`).
#[test]
fn the_drawn_characters_carry_the_dictionary_colour_and_size() {
    let _lock = vm_lock();
    let Some((env, name)) = Env::with_font() else {
        return;
    };
    env.run(&format!(
        "var t = new TextRenderBase(); \
         t.setDefault(%[ color: 0x442200, fontsize: 16, face: '{name}', \
             shadowcolor: 0x010203, edgecolor: 0x040506, edge: 1 ]); \
         t.setRenderSize(400, 100); \
         t.render('AB', 0, 0, 0, false); \
         t.done();"
    ));

    // The colour of the drawn characters is the dictionary's colour.
    assert_eq!(env.eval_int("t.getCharacters(0, 0)[0].color"), 0x442200);
    assert_eq!(env.eval_int("t.getCharacters(0, 0)[1].color"), 0x442200);

    // The shadow and the edge colours reach the character as well.
    assert_eq!(env.eval_int("t.getCharacters(0, 0)[0].shadow"), 0x010203);
    assert_eq!(env.eval_int("t.getCharacters(0, 0)[0].edge"), 0x040506);

    // The first character sits at the origin and the second one behind the
    // first one's advance.
    assert_eq!(env.eval_int("t.getCharacters(0, 0)[0].x"), 0);
    assert_eq!(env.eval_int("t.getCharacters(0, 0)[0].y"), 0);
    let advance_16 = env.eval_int("t.getCharacters(0, 0)[1].x");
    assert!(advance_16 > 0, "the second character follows the first one");

    // The size in the dictionary reaches the layout: the same text at the
    // default size (24) is wider than at the dictionary's size (16).
    env.run(&format!(
        "var u = new TextRenderBase(); \
         u.setDefault(%[ face: '{name}' ]); \
         u.setRenderSize(400, 100); \
         u.render('AB', 0, 0, 0, false); \
         u.done();"
    ));
    let advance_24 = env.eval_int("u.getCharacters(0, 0)[1].x");
    assert!(
        advance_16 < advance_24,
        "the dictionary's size reaches the layout ({advance_16} < {advance_24})"
    );
}
