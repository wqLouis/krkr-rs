//! Integration tests for the `TextRenderBase` plugin.
//!
//! The plugin is the KAG text-layout engine ported from
//! `reference/cpp/plugins/TextRender.cpp`. These tests exercise the full
//! registered surface: the constructor, every method, every property, the KAG
//! markup parser, the render-box line breaking, and the L2 text-engine metrics
//! (per-character advance and ascent).
//!
//! Metrics need a real face. Compat has no font enumeration, so a face is
//! loaded from the attached [`engine::Storage`] on first use (the same seam the
//! visual `Font` native uses); the positive cases mount a system font and skip
//! when none is present. The error cases assert a typed error, never a
//! fabricated metric (D11).
//!
//! The tests serialize on the shared VM lock because the engine → context
//! binding is process-wide and single.

mod common;

use std::path::{Path, PathBuf};

use common::vm_lock;
use compat::CompatHost;
use engine::Scene;
use engine::Storage;
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
        if engine::text::TextEngine::new().add_font(&bytes, 0).is_err() {
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

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, "text-render-test")
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

    fn eval_real(&self, expression: &str) -> f64 {
        match self.eval_ok(expression) {
            TjsValue::Real(value) => value,
            TjsValue::Integer(value) => value as f64,
            other => panic!("{expression:?} returned {other:?}, expected a real"),
        }
    }

    fn eval_string(&self, expression: &str) -> String {
        match self.eval_ok(expression) {
            TjsValue::String(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected a string"),
        }
    }

    /// Evaluate an expression that must raise, returning the message.
    fn error(&self, expression: &str) -> String {
        self.eval(expression)
            .expect_err(&format!("{expression:?} should have raised"))
    }
}

/// The reference's `TextRenderState` defaults (`reference/cpp/plugins/TextRender.cpp:89-108`)
/// seed both the active style and `m_default`, `m_vertical` starts false
/// (`TextRender.cpp:377`), and the whole surface is registered on the class
/// (`TextRender.cpp:1067-1101`).
#[test]
fn text_render_defaults_match_the_reference() {
    let _lock = vm_lock();
    let env = Env::new(None);
    env.run("var t = new TextRenderBase();");

    assert_eq!(env.eval_int("t.vertical"), 0);
    assert_eq!(env.eval_int("t.bold"), 0);
    assert_eq!(env.eval_int("t.italic"), 0);
    assert_eq!(env.eval_string("t.face"), "user");
    assert_eq!(env.eval_int("t.fontSize"), 24);
    assert_eq!(env.eval_real("t.fontScale"), 1.0);
    assert_eq!(env.eval_int("t.chColor"), 0x00ff_ffff);
    assert_eq!(env.eval_int("t.rubySize"), 10);
    assert_eq!(env.eval_int("t.rubyOffset"), -2);
    assert_eq!(env.eval_int("t.shadow"), 1);
    assert_eq!(env.eval_int("t.edge"), 0);
    assert_eq!(env.eval_int("t.lineSpacing"), 6);
    assert_eq!(env.eval_int("t.pitch"), 0);
    assert_eq!(env.eval_int("t.lineSize"), 0);
    assert_eq!(env.eval_int("t.renderDelay"), 1000);
    assert_eq!(env.eval_string("t.renderText"), "");
    assert_eq!(env.eval_int("t.renderCount"), 0);
    assert_eq!(env.eval_int("t.renderOver"), 0);
    assert_eq!(env.eval_int("t.renderLeft"), 0);
    assert_eq!(env.eval_int("t.renderTop"), 0);
    assert_eq!(env.eval_int("t.renderRight"), 0);
    assert_eq!(env.eval_int("t.renderBottom"), 0);

    // Default style mirrors the active style on construction.
    assert_eq!(env.eval_int("t.defaultBold"), 0);
    assert_eq!(env.eval_int("t.defaultItalic"), 0);
    assert_eq!(env.eval_string("t.defaultFace"), "user");
    assert_eq!(env.eval_int("t.defaultFontSize"), 24);
    assert_eq!(env.eval_real("t.defaultFontScale"), 1.0);
    assert_eq!(env.eval_int("t.defaultChColor"), 0x00ff_ffff);
    assert_eq!(env.eval_int("t.defaultRubySize"), 10);
    assert_eq!(env.eval_int("t.defaultRubyOffset"), -2);
    assert_eq!(env.eval_int("t.defaultShadow"), 1);
    assert_eq!(env.eval_int("t.defaultEdge"), 0);
    assert_eq!(env.eval_int("t.defaultLineSpacing"), 6);
    assert_eq!(env.eval_int("t.defaultPitch"), 0);
    assert_eq!(env.eval_int("t.defaultLineSize"), 0);
}

/// Every plain property is an accessor over the instance state
/// (`reference/cpp/plugins/TextRender.cpp:319-344`); `renderRight`/
/// `renderBottom` compute from the box (`TextRender.cpp:339-343`), and
/// `renderCount`/`renderText` expose the accumulated text
/// (`TextRender.cpp:345-346`).
#[test]
fn text_render_properties_round_trip() {
    let _lock = vm_lock();
    let env = Env::new(None);
    env.run(
        "var t = new TextRenderBase(); \
         t.vertical = true; \
         t.bold = 1; \
         t.italic = true; \
         t.face = 'Arial'; \
         t.fontSize = 32; \
         t.fontScale = 1.5; \
         t.chColor = 0x123456; \
         t.rubySize = 7; \
         t.rubyOffset = 3; \
         t.shadow = 0; \
         t.edge = 1; \
         t.lineSpacing = 9; \
         t.pitch = 2; \
         t.lineSize = 11; \
         t.renderOver = 1; \
         t.renderDelay = 250; \
         t.renderText = 'hello'; \
         t.renderLeft = 4; \
         t.renderTop = 5;",
    );

    assert_eq!(env.eval_int("t.vertical"), 1);
    assert_eq!(env.eval_int("t.bold"), 1);
    assert_eq!(env.eval_int("t.italic"), 1);
    assert_eq!(env.eval_string("t.face"), "Arial");
    assert_eq!(env.eval_int("t.fontSize"), 32);
    assert_eq!(env.eval_real("t.fontScale"), 1.5);
    assert_eq!(env.eval_int("t.chColor"), 0x12_3456);
    assert_eq!(env.eval_int("t.rubySize"), 7);
    assert_eq!(env.eval_int("t.rubyOffset"), 3);
    assert_eq!(env.eval_int("t.shadow"), 0);
    assert_eq!(env.eval_int("t.edge"), 1);
    assert_eq!(env.eval_int("t.lineSpacing"), 9);
    assert_eq!(env.eval_int("t.pitch"), 2);
    assert_eq!(env.eval_int("t.lineSize"), 11);
    assert_eq!(env.eval_int("t.renderOver"), 1);
    assert_eq!(env.eval_int("t.renderDelay"), 250);
    assert_eq!(env.eval_string("t.renderText"), "hello");
    assert_eq!(env.eval_int("t.renderCount"), 5);
    assert_eq!(env.eval_int("t.renderLeft"), 4);
    assert_eq!(env.eval_int("t.renderTop"), 5);

    // `renderRight`/`renderBottom` are computed from the box.
    env.run("t.renderRight = 100; t.renderBottom = 80;");
    assert_eq!(env.eval_int("t.renderRight"), 100);
    assert_eq!(env.eval_int("t.renderBottom"), 80);
    assert_eq!(env.eval_int("t.renderLeft"), 4);
    assert_eq!(env.eval_int("t.renderTop"), 5);
}

/// The `default*` accessors write the `m_default` style, kept separate from
/// the active state (`reference/cpp/plugins/TextRender.cpp:348-361`);
/// `defaultFontScale` is stored through a `float` (`TextRender.cpp:352`).
#[test]
fn text_render_default_style_properties_round_trip() {
    let _lock = vm_lock();
    let env = Env::new(None);
    env.run(
        "var t = new TextRenderBase(); \
         t.defaultBold = 1; \
         t.defaultItalic = 1; \
         t.defaultFace = 'Times'; \
         t.defaultFontSize = 40; \
         t.defaultFontScale = 2.0; \
         t.defaultChColor = 0xabcdef; \
         t.defaultRubySize = 5; \
         t.defaultRubyOffset = -1; \
         t.defaultShadow = 0; \
         t.defaultEdge = 1; \
         t.defaultLineSpacing = 12; \
         t.defaultPitch = 3; \
         t.defaultLineSize = 14;",
    );

    assert_eq!(env.eval_int("t.defaultBold"), 1);
    assert_eq!(env.eval_int("t.defaultItalic"), 1);
    assert_eq!(env.eval_string("t.defaultFace"), "Times");
    assert_eq!(env.eval_int("t.defaultFontSize"), 40);
    assert_eq!(env.eval_real("t.defaultFontScale"), 2.0);
    assert_eq!(env.eval_int("t.defaultChColor"), 0x00ab_cdef);
    assert_eq!(env.eval_int("t.defaultRubySize"), 5);
    assert_eq!(env.eval_int("t.defaultRubyOffset"), -1);
    assert_eq!(env.eval_int("t.defaultShadow"), 0);
    assert_eq!(env.eval_int("t.defaultEdge"), 1);
    assert_eq!(env.eval_int("t.defaultLineSpacing"), 12);
    assert_eq!(env.eval_int("t.defaultPitch"), 3);
    assert_eq!(env.eval_int("t.defaultLineSize"), 14);
}

/// `render` returns `!overflow`, where overflow is `m_y > m_boxHeight`
/// (`reference/cpp/plugins/TextRender.cpp:838-846`); `done()` flushes the
/// pending buffer (`TextRender.cpp:1047-1050`), `calcShowCount` counts the
/// positioned characters (`TextRender.cpp:1063`) and `getCharacters`
/// serializes each one (`TextRender.cpp:999-1013,225-247`).
#[test]
fn text_render_layout_uses_the_l2_text_engine() {
    let _lock = vm_lock();
    let Some((env, name)) = Env::with_font() else {
        return;
    };
    env.run(&format!(
        "var t = new TextRenderBase(); t.defaultFace = '{name}'; t.setRenderSize(200, 100);"
    ));
    let overflow = env.eval_int("t.render('AB', 0, 0, 0, false)");
    assert_eq!(overflow, 1, "the text should fit in the box");
    env.run("t.done();");

    assert_eq!(env.eval_int("t.calcShowCount()"), 2);
    assert_eq!(env.eval_string("t.renderText"), "AB");
    assert_eq!(env.eval_int("t.renderCount"), 2);
    assert_eq!(env.eval_int("t.renderOver"), 0);

    // The characters are positioned left-to-right with positive metrics.
    assert_eq!(env.eval_int("t.getCharacters(0, 0).count"), 2);
    assert!(env.eval_int("t.getCharacters(0, 0)[0].cw") > 0);
    assert!(env.eval_int("t.getCharacters(0, 0)[1].cw") > 0);
    assert!(env.eval_int("t.getCharacters(0, 0)[0].size") > 0);
    assert!(
        env.eval_int("t.getCharacters(0, 0)[0].x") < env.eval_int("t.getCharacters(0, 0)[1].x")
    );
    assert_eq!(env.eval_int("t.getCharacters(0, 0)[0].y"), 0);
    assert_eq!(env.eval_int("t.getCharacters(0, 0)[0].bold"), 0);
    assert_eq!(env.eval_string("t.getCharacters(0, 0)[0].text"), "A");
    assert_eq!(env.eval_int("t.getCharacters(0, 0)[0].color"), 0x00ff_ffff);

    // `done()` flushes the pending buffer.
    env.run("t.clear(); t.render('CD', 0, 0, 0, false); t.done();");
    assert_eq!(env.eval_int("t.calcShowCount()"), 2);
    assert_eq!(env.eval_string("t.renderText"), "CD");
}

/// The line breaker wraps only when the pen would leave the box
/// (`reference/cpp/plugins/TextRender.cpp:940-963`), advancing `m_y` by
/// the ascent plus `lineSpacing` (`TextRender.cpp:849-856`);
/// `renderRight` reads back `m_x + m_boxWidth` (`TextRender.cpp:339`).
#[test]
fn text_render_breaks_lines_at_the_box_width() {
    let _lock = vm_lock();
    let Some((env, name)) = Env::with_font() else {
        return;
    };
    // A wide box for the first character, then a box narrower than two
    // characters forces the second onto a new line.
    env.run(&format!(
        "var t = new TextRenderBase(); t.defaultFace = '{name}'; t.setRenderSize(1000, 1000);"
    ));
    env.run("t.render('MMMM', 0, 0, 0, false); t.done();");
    let wide = env.eval_int("t.renderRight");
    assert!(wide > 0, "the layout should advance the pen");

    env.run("t.setRenderSize(1, 1000);");
    env.run("t.render('MMMM', 0, 0, 0, false); t.done();");
    // Every character wraps, so the characters are spread across lines.
    let first_y = env.eval_int("t.getCharacters(0, 0)[0].y");
    let last_y = env.eval_int("t.getCharacters(0, 0)[3].y");
    assert!(
        last_y > first_y,
        "a one-pixel box should wrap the characters (first y {first_y}, last y {last_y})"
    );
}

/// The markup switch of `TextRenderBase::render`
/// (`reference/cpp/plugins/TextRender.cpp:463-836`): `%f`/`%b`/`%i`/`%s`/`%e`
/// style escapes, `#rrggbb;`, `%p` pitch, `%50;` scaling against the
/// *default* size (`TextRender.cpp:670-674`), the `\n`/`\t`/`\i`/`\r`/`\w`
/// escapes (`TextRender.cpp:693-724`), and the consumed `[ruby]`, `&graph;`
/// and `$var;` groups (`TextRender.cpp:726-833`).
#[test]
fn text_render_parser_handles_the_kag_markup() {
    let _lock = vm_lock();
    let Some((env, name)) = Env::with_font() else {
        return;
    };
    env.run(&format!(
        "var t = new TextRenderBase(); t.defaultFace = '{name}'; \
         t.setDefault(%['shadowColor'=>0x010101]); t.setRenderSize(1000, 1000);"
    ));

    // `%f` changes the face, `%b1`/`%i1`/`%e1`/`%s0` the style flags,
    // `#rrggbb;` the colour, `%p` the pitch.
    env.run(&format!(
        "t.render('A%f{name};B%b1C%i1D%e1E%s0F#ff0000;G%p4;H', 0, 0, 0, false); t.done();"
    ));
    assert_eq!(env.eval_int("t.calcShowCount()"), 8);
    assert_eq!(env.eval_string("t.renderText"), "ABCDEFGH");
    assert_eq!(env.eval_int("t.getCharacters(0, 0)[0].bold"), 0);
    assert_eq!(env.eval_int("t.getCharacters(0, 0)[1].bold"), 0);
    assert_eq!(env.eval_int("t.getCharacters(0, 0)[2].bold"), 1);
    assert_eq!(env.eval_int("t.getCharacters(0, 0)[3].italic"), 1);
    assert_eq!(env.eval_int("t.getCharacters(0, 0)[4].edge"), 0x00_0080ff);
    assert_eq!(env.eval_int("t.getCharacters(0, 0)[4].shadow"), 0x01_0101);
    assert_eq!(
        env.eval_ok("t.getCharacters(0, 0)[5].shadow"),
        TjsValue::Void
    );
    assert_eq!(env.eval_int("t.getCharacters(0, 0)[6].color"), 0x00ff_0000);
    assert_eq!(env.eval_string("t.getCharacters(0, 0)[6].face"), name);
    assert_eq!(env.eval_int("t.pitch"), 4);

    // `%50;` scales the font size relative to the default (24 * 50 / 100).
    env.run("t.clear(); t.render('%50;X', 0, 0, 0, false); t.done();");
    assert_eq!(env.eval_int("t.fontSize"), 12);
    assert_eq!(env.eval_int("t.calcShowCount()"), 1);

    // Line break, indent, ruby, graphical and variable escapes are consumed
    // without becoming characters; `\w`/`\t` insert a space/tab.
    env.run("t.clear(); t.render('A\\\\nB\\\\iC\\\\rD[ruby]&graph;$var;E\\\\wF\\\\tG', 0, 0, 0, false); t.done();");
    assert_eq!(env.eval_string("t.renderText"), "ABCDE F\tG");
    assert_eq!(env.eval_int("t.calcShowCount()"), 9);
    assert_eq!(env.eval_string("t.getCharacters(0, 0)[5].text"), " ");
    assert_eq!(env.eval_string("t.getCharacters(0, 0)[7].text"), "\t");

    // `%D` reads an integer (the reference's label branch is unreachable).
    env.run("t.clear(); t.render('A%D100;B', 0, 0, 0, false); t.done();");
    assert_eq!(env.eval_string("t.renderText"), "AB");
}

/// Every parse failure is thrown as a typed message: EOF right after `%`
/// (`reference/cpp/plugins/TextRender.cpp:463-468`), the integer reader
/// (`TextRender.cpp:423-451`), an unknown `%` escape
/// (`TextRender.cpp:683-690`), a non-hex colour digit
/// (`TextRender.cpp:771-777`) and a dangling `\` (`TextRender.cpp:693-698`).
#[test]
fn text_render_parser_errors_are_typed() {
    let _lock = vm_lock();
    let Some((env, name)) = Env::with_font() else {
        return;
    };
    env.run(&format!(
        "var t = new TextRenderBase(); t.defaultFace = '{name}'; t.setRenderSize(1000, 1000);"
    ));

    assert!(
        env.error("t.render('%', 0, 0, 0, false)")
            .contains("found EOF")
    );
    assert!(
        env.error("t.render('%z', 0, 0, 0, false)")
            .contains("expected any of")
    );
    assert!(
        env.error("t.render('#zz;', 0, 0, 0, false)")
            .contains("expected hexadecimal number")
    );
    assert!(
        env.error("t.render('#', 0, 0, 0, false)")
            .contains("found EOF")
    );
    assert!(
        env.error("t.render('%5x', 0, 0, 0, false)")
            .contains("expected either integer or ';'")
    );
    assert!(
        env.error("t.render('\\\\', 0, 0, 0, false)")
            .contains("found EOF")
    );
}

/// The reference pulls per-character extents from the process-wide
/// rasterizer (`reference/cpp/plugins/TextRender.cpp:874-880`); compat has
/// no font enumeration, so an unresolvable face raises a typed error (D11)
/// rather than fabricating a metric — the port's documented seam, pinned
/// here so it cannot become silent.
#[test]
fn text_render_metric_failure_is_a_typed_error() {
    let _lock = vm_lock();
    // No storage and no scene font: the default "user" face cannot resolve.
    let env = Env::new(None);
    env.run("var t = new TextRenderBase(); t.setRenderSize(100, 100);");
    let message = env.error("t.render('A', 0, 0, 0, false)");
    assert!(
        message.contains("no storage attached"),
        "unexpected message: {message}"
    );
}

/// `updateFont` hands `m_state.face` (default `"user"`) to the active
/// rasterizer (`reference/cpp/plugins/TextRender.cpp:1033-1045`); the port
/// resolves that face from the newest font registered in the attached
/// scene, which is its equivalent of "currently applied face".
#[test]
fn text_render_resolves_the_user_face_through_the_scene_font() {
    let _lock = vm_lock();
    let Some((env, name)) = Env::with_font() else {
        return;
    };
    // A script font in the scene supplies the face for the reference's
    // "user" placeholder.
    env.run(&format!("var f = new Font('{name}', 24, 0xffffffff);"));
    env.run("var t = new TextRenderBase(); t.setRenderSize(100, 100);");
    assert_eq!(env.eval_int("t.render('A', 0, 0, 0, false)"), 1);
    env.run("t.done();");
    assert_eq!(env.eval_int("t.calcShowCount()"), 1);
}

/// `setDefault` only replaces `m_default` (`reference/cpp/plugins/TextRender.cpp:989-992`);
/// the active style changes on `clear` (`TextRender.cpp:1016-1031`) and on
/// `resetFont`/`resetStyle`, which both restore `m_default`
/// (`TextRender.cpp:1052-1054`). Absent dictionary keys never overwrite
/// (the `getprop` macro, `TextRender.cpp:53-66`).
#[test]
fn text_render_set_default_clear_and_reset() {
    let _lock = vm_lock();
    let env = Env::new(None);
    env.run(
        "var t = new TextRenderBase(); \
         t.setDefault(%['fontSize'=>40, 'face'=>'Fancy', 'bold'=>true, \
                        'shadow'=>false, 'chColor'=>0x112233]);",
    );
    // `setDefault` only updates the default; the active style is unchanged
    // until `clear`/`resetFont`/`resetStyle`.
    assert_eq!(env.eval_int("t.fontSize"), 24);
    env.run("t.clear();");
    assert_eq!(env.eval_int("t.fontSize"), 40);
    assert_eq!(env.eval_string("t.face"), "Fancy");
    assert_eq!(env.eval_int("t.bold"), 1);
    assert_eq!(env.eval_int("t.shadow"), 0);
    assert_eq!(env.eval_int("t.chColor"), 0x11_2233);

    env.run("t.fontSize = 8; t.face = 'Other'; t.bold = 0;");
    env.run("t.resetFont();");
    assert_eq!(env.eval_int("t.fontSize"), 40);
    assert_eq!(env.eval_string("t.face"), "Fancy");
    env.run("t.fontSize = 8; t.resetStyle();");
    assert_eq!(env.eval_int("t.fontSize"), 40);
}

/// `setRenderSize` stores the box and then `clear()`s everything
/// (`reference/cpp/plugins/TextRender.cpp:979-987,1016-1031`);
/// `renderRight`/`renderBottom` read the box back (`TextRender.cpp:339-343`).
#[test]
fn text_render_set_render_size_clears_state() {
    let _lock = vm_lock();
    let env = Env::new(None);
    env.run("var t = new TextRenderBase(); t.renderText = 'stale'; t.fontSize = 8;");
    env.run("t.setRenderSize(320, 240);");
    assert_eq!(env.eval_string("t.renderText"), "");
    assert_eq!(env.eval_int("t.renderCount"), 0);
    assert_eq!(env.eval_int("t.fontSize"), 24);
    assert_eq!(env.eval_int("t.renderRight"), 320);
    assert_eq!(env.eval_int("t.renderBottom"), 240);
    assert_eq!(env.eval_int("t.calcShowCount()"), 0);
}

/// `renderCount` and `keyWait` are one-way: their setters throw the
/// reference's messages (`reference/cpp/plugins/TextRender.cpp:315,346`),
/// `getKeyWait`/`keyWait` return an empty array (`TextRender.cpp:1056-1061`)
/// and `newline` only logs (`TextRender.cpp:310`).
#[test]
fn text_render_refused_setters_and_key_wait() {
    let _lock = vm_lock();
    let env = Env::new(None);
    env.run("var t = new TextRenderBase();");
    assert!(env.error("t.renderCount = 3").contains("renderCount"));
    assert!(env.error("t.keyWait = []").contains("setKeyWait"));

    // Both the method and the property return an empty array.
    assert_eq!(env.eval_int("t.getKeyWait().count"), 0);
    assert_eq!(env.eval_int("t.keyWait.count"), 0);

    // `newline` is the reference's no-op.
    env.run("t.newline();");
}

/// `TextRenderOptions::deserialize` checks for an object and does nothing
/// else (`reference/cpp/plugins/TextRender.cpp:191-196`), so `setOption`
/// accepts the dictionary without changing the compiled-in option sets.
#[test]
fn text_render_set_option_is_accepted_and_ignored() {
    let _lock = vm_lock();
    let env = Env::new(None);
    env.run("var t = new TextRenderBase(); t.setOption(%['leading'=>'x']);");
    // The reference `TextRenderOptions::deserialize` is a no-op.
    assert_eq!(env.eval_int("t.calcShowCount()"), 0);
}

/// ncbind rejects a call with fewer arguments than the C++ signature has
/// parameters (`reference/cpp/core/plugin/ncbind.hpp:1178`) for every
/// `NCB_METHOD` (`reference/cpp/plugins/TextRender.cpp:1067-1081`): `render`
/// takes five (`TextRender.cpp:301-302`), `setRenderSize` two (`:303`),
/// `setDefault`/`setOption` one (`:304-305`) and `getCharacters` two
/// (`:306`). The reference renders the failure as "Invalid argument count"
/// (`reference/cpp/core/tjs2/tjsError.h:268`); this port names the member.
#[test]
fn text_render_argument_counts_are_typed_errors() {
    let _lock = vm_lock();
    let env = Env::new(None);
    env.run("var t = new TextRenderBase();");

    for (expression, expected) in [
        ("t.render('A', 0, 0, 0)", "requires 5 arguments"),
        ("t.setRenderSize(100)", "requires 2 arguments"),
        ("t.getCharacters(0)", "requires 2 arguments"),
        ("t.setDefault()", "requires 1 argument"),
        ("t.setOption()", "requires 1 argument"),
    ] {
        let message = env.error(expression);
        assert!(
            message.contains(expected),
            "{expression:?}: unexpected message: {message}"
        );
    }
}

/// The flag escapes guard EOF with
/// `if(!readchar(...) && (ch == '0' || ch == '1'))`
/// (`reference/cpp/plugins/TextRender.cpp:511-518`); at EOF `readchar`
/// leaves `ch` holding the format character (`TextRender.cpp:412-419`), so
/// the guard never fires and the flag is `ch == '1'` — `false` at end of
/// text (or for any other character), `true` for `%b1`.
#[test]
fn text_render_percent_flag_escapes_never_fail_at_end_of_text() {
    let _lock = vm_lock();
    let env = Env::new(None);
    env.run("var t = new TextRenderBase(); t.italic = 1; t.edge = 1;");

    assert_eq!(env.eval_int("t.render('%b1', 0, 0, 0, false)"), 1);
    assert_eq!(env.eval_int("t.bold"), 1);
    assert_eq!(env.eval_int("t.render('%i', 0, 0, 0, false)"), 1);
    assert_eq!(env.eval_int("t.italic"), 0);
    // `shadow` defaults to true and is cleared by `%s` at EOF.
    assert_eq!(env.eval_int("t.render('%s', 0, 0, 0, false)"), 1);
    assert_eq!(env.eval_int("t.shadow"), 0);
    assert_eq!(env.eval_int("t.render('%e', 0, 0, 0, false)"), 1);
    assert_eq!(env.eval_int("t.edge"), 0);

    // A non-flag character after `%b` is consumed and read as `false`.
    assert_eq!(env.eval_int("t.render('%bX', 0, 0, 0, false)"), 1);
    assert_eq!(env.eval_int("t.bold"), 0);
    assert_eq!(env.eval_string("t.renderText"), "");
}

/// `pushCharacter` rejects UTF-16 surrogate code units outright
/// (`reference/cpp/plugins/TextRender.cpp:869-873`); the port sees the
/// non-BMP character where the reference sees its surrogate pair and
/// raises the same typed message — before any metric is needed, so the
/// failure holds with or without a font.
#[test]
fn text_render_rejects_surrogate_pairs() {
    let _lock = vm_lock();
    let env = Env::new(None);
    env.run("var t = new TextRenderBase(); t.setRenderSize(100, 100);");

    let message = env.error("t.render('😀', 0, 0, 0, false)");
    assert!(
        message.contains("surrogate pair"),
        "unexpected message: {message}"
    );
}

/// `getCharacters` populates its result only for `end < start` or
/// `(0, 0)`; every other range hits the reference's `// TODO: unknown
/// behaviour` branch and returns the array it created empty
/// (`reference/cpp/plugins/TextRender.cpp:999-1013`).
#[test]
fn text_render_get_characters_only_populates_the_reference_ranges() {
    let _lock = vm_lock();
    let Some((env, name)) = Env::with_font() else {
        return;
    };
    env.run(&format!(
        "var t = new TextRenderBase(); t.defaultFace = '{name}'; \
         t.setRenderSize(1000, 1000);"
    ));
    env.run("t.render('AB', 0, 0, 0, false); t.done();");
    assert_eq!(env.eval_int("t.calcShowCount()"), 2);

    // `(0, 0)` and `end < start` list everything…
    assert_eq!(env.eval_int("t.getCharacters(0, 0).count"), 2);
    assert_eq!(env.eval_int("t.getCharacters(5, 1).count"), 2);
    // …any other range stays empty.
    assert_eq!(env.eval_int("t.getCharacters(1, 2).count"), 0);
}
