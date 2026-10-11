//! Integration tests for the `Font` native class text metrics.
//!
//! The measurement methods are backed by the L2 [`engine::text`] text engine.
//! A face is resolved through the shared resource resolver (the `System.addFont`
//! registry, the attached [`engine::Storage`], the host's installed system
//! fonts, then the default-face fallback); the positive cases mount a
//! directory holding a system font and skip when no usable font is present
//! (mirroring the L2/L1 text tests). `getList`, `defaultFaceName` and
//! `getGlyphDrawRect` report the resolver's registered faces; only
//! `doUserSelect` remains deferred (no host font-selection UI).
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

/// Builds a version-1 `.tft` for `chars`, each a `2x2` coverage whose four
/// literal bytes are `0x10, 0x20, 0x30, 0x3F` and whose descriptor `inc` is
/// `inc`.
fn build_tft(chars: &[u16], inc: i16) -> Vec<u8> {
    let header = 36usize;
    let ch_index_offset = header;
    let index_offset = ch_index_offset + chars.len() * 2;
    let data_offset = index_offset + chars.len() * 20;
    let mut bytes = vec![0u8; data_offset];
    bytes[..22].copy_from_slice(b"TVP pre-rendered font\x1a");
    bytes[22] = 1;
    bytes[23] = 2;
    bytes[24..28].copy_from_slice(&(chars.len() as u32).to_le_bytes());
    bytes[28..32].copy_from_slice(&(ch_index_offset as u32).to_le_bytes());
    bytes[32..36].copy_from_slice(&(index_offset as u32).to_le_bytes());
    for (i, ch) in chars.iter().enumerate() {
        bytes[ch_index_offset + i * 2..ch_index_offset + i * 2 + 2]
            .copy_from_slice(&ch.to_le_bytes());
    }
    for i in 0..chars.len() {
        let at = index_offset + i * 20;
        let offset = (data_offset + i * 4) as u32;
        bytes[at..at + 4].copy_from_slice(&offset.to_le_bytes());
        bytes[at + 4..at + 6].copy_from_slice(&2u16.to_le_bytes());
        bytes[at + 6..at + 8].copy_from_slice(&2u16.to_le_bytes());
        bytes[at + 12..at + 14].copy_from_slice(&3i16.to_le_bytes());
        bytes[at + 16..at + 18].copy_from_slice(&inc.to_le_bytes());
    }
    for _ in chars {
        bytes.extend_from_slice(&[0x10, 0x20, 0x30, 0x3F]);
    }
    bytes
}

/// A bound VM + host + attached scene and optional storage. Fields drop in
/// declaration order, so `_host` (which owns the process-wide binding) drops
/// before the engine/scene/storage its raw pointers reference.
struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    scene: Box<Scene>,
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
            scene,
            _storage: storage,
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "font-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, "font-test")
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

    /// Evaluate an expression that must raise, returning the message.
    fn error(&self, expression: &str) -> String {
        self.eval(expression)
            .expect_err(&format!("{expression:?} should have raised"))
    }
}

#[test]
fn font_metrics_use_the_l2_text_engine() {
    let Some((dir, name)) = test_font() else {
        return;
    };
    let _lock = vm_lock();
    let storage = Storage::mount_archives(
        FileSystem::host(&dir),
        dir.clone(),
        std::iter::empty::<&str>(),
    )
    .expect("mount font directory");
    let env = Env::new(Some(storage));

    env.run(&format!("var f = new Font('{name}', 24, 0xffffffff);"));

    let width = env.eval_int("f.getTextWidth('Hello')");
    let height = env.eval_int("f.getTextHeight('Hello')");
    assert!(width > 0, "shaped width should be positive, got {width}");
    assert!(height > 0, "shaped height should be positive, got {height}");

    // Angle 0: the escape projections collapse to the width/height along x/y.
    assert!(
        (env.eval_real("f.getEscWidthX('Hello')") - width as f64).abs() < 1.0,
        "getEscWidthX should equal the width at angle 0"
    );
    assert!(
        env.eval_real("f.getEscWidthY('Hello')").abs() < 1.0,
        "getEscWidthY should be zero at angle 0"
    );
    assert!(
        env.eval_real("f.getEscHeightX('Hello')").abs() < 1.0,
        "getEscHeightX should be zero at angle 0"
    );
    assert!(
        (env.eval_real("f.getEscHeightY('Hello')") - height as f64).abs() < 1.0,
        "getEscHeightY should equal the height at angle 0"
    );

    // A quarter turn: width projects onto y, height onto x.
    env.run("f.angle = 900;");
    assert!(
        env.eval_real("f.getEscWidthX('Hello')").abs() < 1.0,
        "getEscWidthX should vanish at 90 degrees"
    );
    assert!(
        env.eval_real("f.getEscWidthY('Hello')") < 0.0,
        "getEscWidthY should be negative at 90 degrees"
    );
    assert!(
        env.eval_real("f.getEscHeightX('Hello')") > 0.0,
        "getEscHeightX should be positive at 90 degrees"
    );
    assert!(
        env.eval_real("f.getEscHeightY('Hello')").abs() < 1.0,
        "getEscHeightY should vanish at 90 degrees"
    );
    // The reference `getTextWidth`/`getTextHeight` are unrotated; only the
    // `getEsc*` variants apply the angle
    // (`reference/cpp/core/visual/LayerIntf.cpp:11716-11746`).
    assert_eq!(env.eval_int("f.getTextWidth('Hello')"), width);
    assert_eq!(env.eval_int("f.getTextHeight('Hello')"), 24);
}

#[test]
fn font_metrics_without_a_font_source_do_not_fabricate_a_metric() {
    let _lock = vm_lock();
    let env = Env::new(None);
    env.run("var f = new Font('Ghost', 16, 0);");

    // There is no storage to load a face from. A host with no matching
    // installed font raises a typed error rather than returning a fabricated
    // metric; a host whose CJK fallback matches resolves it. Either is a
    // valid, non-fabricated outcome.
    match env.eval("f.getTextWidth('x')") {
        Ok(TjsValue::Integer(width)) => assert!(width > 0, "fallback width should be positive"),
        Ok(other) => panic!("unexpected result: {other:?}"),
        Err(message) => assert!(!message.is_empty(), "empty error message"),
    }
}

#[test]
fn font_glyph_draw_rect_returns_the_rasterized_bounds() {
    let Some((dir, name)) = test_font() else {
        return;
    };
    let _lock = vm_lock();
    let storage = Storage::mount_archives(
        FileSystem::host(&dir),
        dir.clone(),
        std::iter::empty::<&str>(),
    )
    .expect("mount font directory");
    let env = Env::new(Some(storage));
    env.run(&format!("var f = new Font('{name}', 24, 0xffffffff);"));

    // Reference: `FreeTypeFontRasterizer::GetGlyphDrawRect` unions each
    // rasterized glyph box and returns it
    // (`reference/cpp/core/visual/FreeTypeFontRasterizer.cpp:225-257`).
    let left = env.eval_int("f.getGlyphDrawRect('A').left");
    let right = env.eval_int("f.getGlyphDrawRect('A').right");
    let top = env.eval_int("f.getGlyphDrawRect('A').top");
    let bottom = env.eval_int("f.getGlyphDrawRect('A').bottom");
    assert!(
        right > left && bottom > top,
        "getGlyphDrawRect should be a non-empty rect, got ({left}, {top}, {right}, {bottom})"
    );
}

#[test]
fn font_get_list_and_default_face_name_report_the_registered_face() {
    let Some((dir, name)) = test_font() else {
        return;
    };
    let _lock = vm_lock();
    let storage = Storage::mount_archives(
        FileSystem::host(&dir),
        dir.clone(),
        std::iter::empty::<&str>(),
    )
    .expect("mount font directory");
    let env = Env::new(Some(storage));
    env.run(&format!("var f = new Font('{name}', 24, 0xffffffff);"));
    // Load the face so it is registered with the resolver.
    assert!(env.eval_int("f.getTextWidth('A')") > 0);

    // Reference: `Font.getList` returns the enumerated face names as a TJS
    // array (`reference/cpp/core/visual/LayerIntf.cpp:11958-11985`).
    assert!(
        env.eval_int("f.getList(0).length") > 0,
        "getList should report the registered face"
    );
    // Reference: `Font.defaultFaceName` returns the default face
    // (`reference/cpp/core/visual/LayerIntf.cpp:12177`).
    match env.eval("Font.defaultFaceName") {
        Ok(TjsValue::String(name)) => {
            assert!(!name.is_empty(), "default face name should be set")
        }
        other => panic!("expected a string default face name, got {other:?}"),
    }
}

#[test]
fn font_user_select_is_deferred() {
    let _lock = vm_lock();
    let env = Env::new(None);
    env.run("var f = new Font('Ghost', 16, 0);");

    assert!(
        env.error("f.doUserSelect(0, '', '', '')")
            .contains("font-selection UI")
    );
}

#[test]
fn font_rasterizer_is_the_single_l1_rasterizer() {
    let _lock = vm_lock();
    let env = Env::new(None);

    assert_eq!(env.eval_int("Font.rasterizer"), 0);
    // Re-selecting the only rasterizer is accepted.
    env.run("Font.rasterizer = 0;");
    let message = env.error("Font.rasterizer = 1");
    assert!(
        message.contains("only rasterizer 0"),
        "unexpected message: {message}"
    );
}

#[test]
fn font_map_prerendered_font_raises_for_a_missing_storage() {
    let _lock = vm_lock();
    let env = Env::new(None);
    env.run("var f = new Font('Ghost', 16, 0);");
    // Reference: `tTVPPrerenderedFont` raises `TVPCannotFindStorage` when the
    // storage cannot be placed (`reference/cpp/core/visual/PrerenderedFont.cpp:9-13`,
    // `reference/cpp/core/base/StorageIntf.cpp:1135-1140`).
    let message = env.error("f.mapPrerenderedFont('missing.tft')");
    assert!(
        message.contains("Cannot find storage"),
        "unexpected message: {message}"
    );
    // The font itself stays usable.
    assert_eq!(env.eval_int("f.height"), 16);
    // The reference `Font` member list registers no `id`
    // (`reference/cpp/core/visual/LayerIntf.cpp:11820`), so reading it raises.
    env.error("f.id");
}

/// Reference: `tTVPNativeBaseBitmap::GetTextSize` sums each character's
/// pre-rendered `Inc` and reports `abs(Font.Height)` as the height
/// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:1435-1473`).
#[test]
fn font_prerendered_metrics_use_the_mapped_inc_and_height() {
    let _lock = vm_lock();
    let Some((font_dir, font_name)) = test_font() else {
        return;
    };
    let font_bytes = std::fs::read(font_dir.join(&font_name)).expect("read test font");
    let env = common::Env::with_storage();
    let game_dir = env.game_dir().expect("game dir").clone();
    // The face string is read from storage, so the test does not depend on the
    // host's installed fonts.
    std::fs::write(game_dir.join("Pre"), &font_bytes).expect("write face");
    std::fs::write(
        game_dir.join("sample.tft"),
        build_tft(&[b'A' as u16, b'B' as u16], 100),
    )
    .expect("write tft");
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         var f = new Font('Pre', 12, 0); \
         f.mapPrerenderedFont('sample.tft'); \
         l.font.face = 'Pre'; \
         l.font.height = 12; \
         l.font.bold = false; \
         l.font.italic = false; \
         l.font.angle = 0;",
    );

    // Reference: `tTVPNativeBaseBitmap::GetTextSize` sums each character's
    // pre-rendered `Inc` and reports `abs(Font.Height)` as the height
    // (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:1435-1473`).
    assert_eq!(env.eval_int("l.font.getTextWidth('AB')"), 200);
    assert_eq!(env.eval_int("l.font.getTextHeight('AB')"), 12);
    // At angle 0 the escape projection equals the pre-rendered width
    // (`reference/cpp/core/visual/LayerIntf.cpp:11734-11740`).
    assert!((env.eval_real("l.font.getEscWidthX('AB')") - 200.0).abs() < 1.0);

    // The reference only consults the pre-rendered font through a layer-bound
    // font; a standalone `new Font().getTextWidth` measures directly
    // (`reference/cpp/core/visual/LayerIntf.cpp:11716-11722`).
    assert_ne!(env.eval_int("f.getTextWidth('AB')"), 200);
}

/// Reference: `tTVPNativeBaseBitmap::UnmapPrerenderedFont` →
/// `TVPUnmapPrerenderedFont` drops the font-value mapping, so a later measure
/// falls back to the rasterizer's width instead of the pre-rendered `Inc`
/// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:169-185`,
/// `reference/cpp/core/visual/LayerIntf.cpp:11798-11803`).
#[test]
fn font_unmap_prerendered_font_drops_the_mapping() {
    let _lock = vm_lock();
    let Some((font_dir, font_name)) = test_font() else {
        return;
    };
    let font_bytes = std::fs::read(font_dir.join(&font_name)).expect("read test font");
    let env = common::Env::with_storage();
    let game_dir = env.game_dir().expect("game dir").clone();
    std::fs::write(game_dir.join("Pre"), &font_bytes).expect("write face");
    std::fs::write(
        game_dir.join("sample.tft"),
        build_tft(&[b'A' as u16, b'B' as u16], 100),
    )
    .expect("write tft");
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         var f = new Font('Pre', 12, 0); \
         f.mapPrerenderedFont('sample.tft'); \
         l.font.face = 'Pre'; \
         l.font.height = 12; \
         l.font.bold = false; \
         l.font.italic = false; \
         l.font.angle = 0;",
    );
    assert_eq!(env.eval_int("l.font.getTextWidth('AB')"), 200);

    env.run("f.unmapPrerenderedFont();");
    assert_ne!(
        env.eval_int("l.font.getTextWidth('AB')"),
        200,
        "the unmap drops the pre-rendered mapping"
    );
}

/// Reference: `tTVPNativeBaseBitmap::DrawTextMultiple` uses the mapped
/// pre-rendered coverage for each character
/// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:1285-1433`).
#[test]
fn layer_draw_text_uses_a_mapped_prerendered_font() {
    let _lock = vm_lock();
    let Some((font_dir, font_name)) = test_font() else {
        return;
    };
    let font_bytes = std::fs::read(font_dir.join(&font_name)).expect("read test font");
    let env = common::Env::with_storage();
    let game_dir = env.game_dir().expect("game dir").clone();
    std::fs::write(game_dir.join("Pre"), &font_bytes).expect("write face");
    std::fs::write(game_dir.join("sample.tft"), build_tft(&[b'A' as u16], 7)).expect("write tft");
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize(64, 64); \
         var f = new Font('Pre', 12, 0); \
         f.mapPrerenderedFont('sample.tft'); \
         l.font.face = 'Pre'; \
         l.font.height = 12; \
         l.font.bold = false; \
         l.font.italic = false; \
         l.font.angle = 0; \
         l.drawText(4, 4, 'A', 0x00ffffff, 255);",
    );
    let layer = env.scene.layers.last().expect("the layer exists");
    let bitmap = env
        .scene
        .bitmap(layer.bitmap.expect("the layer has a main image"))
        .expect("the bitmap exists");
    assert!(
        bitmap.rgba.chunks_exact(4).any(|pixel| pixel[3] != 0),
        "drawText should use the mapped pre-rendered coverage"
    );
}

#[test]
fn font_properties_round_trip() {
    let _lock = vm_lock();
    let env = Env::new(None);

    env.run(
        "var f = new Font('Arial', 12, 0xffff0000); \
         f.bold = true; \
         f.italic = 1; \
         f.strikeout = true; \
         f.underline = 1; \
         f.angle = 450; \
         f.faceIsFileName = true;",
    );

    assert_eq!(env.eval_int("f.face == 'Arial'"), 1);
    assert_eq!(env.eval_int("f.height"), 12);
    assert_eq!(env.eval_int("f.bold"), 1);
    assert_eq!(env.eval_int("f.italic"), 1);
    assert_eq!(env.eval_int("f.strikeout"), 1);
    assert_eq!(env.eval_int("f.underline"), 1);
    assert_eq!(env.eval_int("f.angle"), 450);
    // Reference: `Font.faceIsFileName` round-trips through the font value's
    // `TVP_TF_FONTFILE` flag (`reference/cpp/core/visual/LayerIntf.cpp:11677-11693`).
    assert_eq!(env.eval_int("f.faceIsFileName"), 1);
    env.run("f.faceIsFileName = false;");
    assert_eq!(env.eval_int("f.faceIsFileName"), 0);

    let font = env.scene.fonts.first().expect("one font");
    assert_eq!(font.face, "Arial");
    assert_eq!(font.height, 12);
    assert!(font.bold && font.italic && font.strikeout && font.underline);
}

/// Reference: `TVPFindFont` searches the enumerated font map that
/// `TVPInitFontNames` fills with the system faces
/// (`reference/cpp/core/visual/FontImpl.cpp:159-164,277-285`), and
/// `TVPCreateFontStream` falls back to the default face when the requested
/// name is absent (`FontImpl.cpp:160-164`). A face the game did not ship and
/// the storage does not hold must therefore resolve through the engine's
/// system-font fallback rather than raise. A host with no Japanese-capable
/// system font has no default face either, so the test skips that case
/// instead of failing spuriously.
#[test]
fn font_metrics_fall_back_to_a_system_face() {
    let _lock = vm_lock();
    let Some((dir, _name)) = test_font() else {
        return;
    };
    // A storage that holds a font under a different name does not satisfy the
    // requested family, forcing the system-font fallback.
    let storage = Storage::mount_archives(
        FileSystem::host(&dir),
        dir.clone(),
        std::iter::empty::<&str>(),
    )
    .expect("mount font directory");
    let env = Env::new(Some(storage));
    env.run("var f = new Font('this-family-is-not-installed', 24, 0xffffffff);");

    match env.eval("f.getTextWidth('Hello')") {
        Ok(TjsValue::Integer(width)) => {
            assert!(
                width > 0,
                "the system fallback should shape real text, got {width}"
            );
        }
        Ok(other) => panic!("expected an integer width, got {other:?}"),
        Err(message) if message.contains("no font is available") => {}
        Err(message) => panic!("unexpected error: {message}"),
    }
}
