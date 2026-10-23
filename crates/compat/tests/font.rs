//! Integration tests for the `Font` native class text metrics.
//!
//! The measurement methods are backed by the L2 [`engine::text`] text engine.
//! A face is resolved through the shared resource resolver (the `System.addFont`
//! registry, the attached [`engine::Storage`], the host's installed system
//! fonts, then the default-face fallback); the positive cases mount a
//! directory holding a system font and skip when no usable font is present
//! (mirroring the L2/L1 text tests). `getList`, `defaultFaceName` and
//! `getGlyphDrawRect` report the resolver's registered faces.
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
        common::link_all_builtins(&engine);
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

/// Reference: `tTJSNI_Font::GetTextHeight` returns `abs(Font.Height)` and
/// measures nothing (`reference/cpp/core/visual/LayerIntf.cpp:11724-11731`),
/// so the height is the font's own height whatever the text is.
#[test]
fn font_get_text_height_is_the_font_height() {
    let _lock = vm_lock();
    let env = Env::new(None);
    env.run("var f = new Font('Ghost', 24, 0);");

    // The height is the font's own height, whatever the text is.
    assert_eq!(env.eval_int("f.height"), 24);
    assert_eq!(env.eval_int("f.getTextHeight('Hello')"), 24);
    assert_eq!(env.eval_int("f.getTextHeight('')"), 24);

    // A negative height is normalized to positive
    // (`reference/cpp/core/visual/LayerIntf.cpp:5962-5969`).
    env.run("f.height = -18;");
    assert_eq!(env.eval_int("f.height"), 18);
    assert_eq!(env.eval_int("f.getTextHeight('x')"), 18);

    // The string argument is required (the reference's arity).
    assert!(env.eval("f.getTextHeight()").is_err());
}

/// Reference: `tTJSNI_Font::GetTextHeight` returns `abs(Font.Height)` without
/// reading the font's data (`reference/cpp/core/visual/LayerIntf.cpp:11724-11731`),
/// so the height is reported even where the width cannot be measured.
#[test]
fn font_get_text_height_does_not_read_the_font_data() {
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
         var f = new Font('Pre', 12, 0); \
         f.mapPrerenderedFont('sample.tft'); \
         l.font.face = 'Pre'; \
         l.font.height = 12; \
         l.font.bold = false; \
         l.font.italic = false; \
         l.font.angle = 0;",
    );
    // The pre-rendered body is read on first use, so removing it makes the
    // width unreadable.
    std::fs::remove_file(game_dir.join("sample.tft")).expect("remove the tft");
    assert!(
        env.eval("l.font.getTextWidth('AB')").is_err(),
        "the width needs the pre-rendered data"
    );
    // The height is the font's own height and needs no data at all.
    assert_eq!(env.eval_int("l.font.getTextHeight('AB')"), 12);
}

/// Reference: `tTJSNI_Font::GetTextWidthDirect` measures a run one character
/// at a time — the rasterizer's per-character extent, rounded to whole pixels
/// (`reference/cpp/core/visual/LayerIntf.cpp:11700-11714`,
/// `reference/cpp/core/visual/FreeType.cpp:620-630`) — so a run measures as
/// the sum of its characters' advances. A shaped run's width applies kerning
/// and rounds once, so it is narrower than that sum
/// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:1435-1473`).
#[test]
fn font_text_width_sums_the_per_character_advances() {
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
    env.run(&format!("var f = new Font('{name}', 48, 0);"));

    // "AV" is a kerned pair: the shaped run is narrower than the two
    // characters measured on their own.
    let whole = env.eval_int("f.getTextWidth('AV')");
    let parts = env.eval_int("f.getTextWidth('A')") + env.eval_int("f.getTextWidth('V')");
    assert_eq!(
        whole, parts,
        "a run must measure as the sum of its characters' advances"
    );
    assert!(parts > 0, "the characters must have a positive advance");
}

/// The last face registered by `System.addFont` is the default font, so a
/// face no source knows — a pre-rendered `*name`, for example — falls back to
/// it rather than to the host's default family.
#[test]
fn an_unknown_face_falls_back_to_the_last_registered_font() {
    let _lock = vm_lock();
    let Some((font_dir, font_name)) = test_font() else {
        return;
    };
    let bytes = std::fs::read(font_dir.join(&font_name)).expect("read test font");
    let mut engine = TextEngine::new();
    let id = engine.add_font(&bytes, 0).expect("register the face");
    let family = engine.family_name(id).expect("family name").to_string();

    let env = common::Env::with_storage();
    let game_dir = env.game_dir().expect("game dir").clone();
    std::fs::write(game_dir.join("Pre.ttf"), &bytes).expect("write face");
    env.run(
        "Plugins.link('addFont.dll'); System.addFont('Pre.ttf'); \
         var f = new Font('*missing', 12, 0); f.getTextWidth('A');",
    );
    match env.eval_ok("Font.defaultFaceName") {
        TjsValue::String(value) => assert_eq!(
            value, family,
            "the default must be the last registered face, not the host fallback"
        ),
        other => panic!("Font.defaultFaceName returned {other:?}"),
    }
}

/// A pseudo-face (`*name`) is tried as an ordinary candidate and, when no
/// source knows it, falls back to the default face.
///
/// Reference: the font map is keyed by family name
/// (`reference/cpp/core/visual/FontImpl.cpp:96-131`), so a name the map does
/// not hold — a pseudo-face among them — is answered by the default font
/// (`reference/cpp/core/visual/FontImpl.cpp:159-168`).
#[test]
fn a_pseudo_face_measures_with_the_default_registered_face() {
    let _lock = vm_lock();
    let Some((font_dir, font_name)) = test_font() else {
        return;
    };
    let bytes = std::fs::read(font_dir.join(&font_name)).expect("read test font");
    let mut engine = TextEngine::new();
    let id = engine.add_font(&bytes, 0).expect("register the face");
    let family = engine.family_name(id).expect("family name").to_string();

    let env = common::Env::with_storage();
    let game_dir = env.game_dir().expect("game dir").clone();
    std::fs::write(game_dir.join("Pre.ttf"), &bytes).expect("write face");
    env.run(&format!(
        "Plugins.link('addFont.dll'); System.addFont('Pre.ttf'); \
         var pseudo = new Font('*システム', 24, 0); pseudo.getTextWidth('A'); \
         var registered = new Font('{family}', 24, 0);"
    ));
    let pseudo_width = env.eval_int("pseudo.getTextWidth('A')");
    let registered_width = env.eval_int("registered.getTextWidth('A')");
    assert!(
        pseudo_width > 0,
        "a pseudo-face must measure with the default face, got {pseudo_width}"
    );
    assert_eq!(
        pseudo_width, registered_width,
        "a pseudo-face must measure exactly like the default registered face"
    );
}

/// A pseudo-face candidate is tried like any other candidate: a name spelled
/// like a font file is read from the storage.
///
/// Reference: the font map is keyed by family name
/// (`reference/cpp/core/visual/FontImpl.cpp:96-131,277-289`), so a name the
/// map does not hold is looked up among the enumerated fonts and, when it is
/// not there either, answered by the default font
/// (`reference/cpp/core/visual/FontImpl.cpp:159-168`).
#[test]
fn a_pseudo_face_candidate_is_read_from_the_storage() {
    let _lock = vm_lock();
    let Some((font_dir, font_name)) = test_font() else {
        return;
    };
    let bytes = std::fs::read(font_dir.join(&font_name)).expect("read test font");

    let env = common::Env::with_storage();
    let game_dir = env.game_dir().expect("game dir").clone();
    std::fs::write(game_dir.join("*システム.ttf"), &bytes).expect("write pseudo face");
    let storage = env.storage().expect("storage");
    let read_bytes = storage.read_bytes();
    env.run("var pseudo = new Font('*システム.ttf', 24, 0); pseudo.getTextWidth('A');");
    assert!(
        storage.read_bytes() > read_bytes,
        "a pseudo-face candidate must be read from the storage like any other"
    );
}

#[test]
fn font_metrics_without_a_font_source_do_not_fabricate_a_metric() {
    let _lock = vm_lock();
    let env = Env::new(None);
    env.run("var f = new Font('Ghost', 16, 0);");

    // There is no storage to load a face from. Whether the host has any
    // usable face at all is host-dependent: the resolver tries the
    // `System.addFont` registry, the storage, the installed system fonts, the
    // well-known Noto CJK paths and finally the engine's first registered
    // face (`crates/compat/src/resource.rs`'s `resolve_face`). A host with a
    // fallback resolves it; a host with no font at all raises the resolver's
    // no-face error. Pin both exact outcomes so a fabricated `0`, a
    // non-integer or a different error fails.
    match env.eval("f.getTextWidth('x')") {
        Ok(TjsValue::Integer(width)) => assert!(
            width > 0,
            "a resolved fallback must have positive width, got {width}"
        ),
        Ok(other) => panic!("unexpected result: {other:?}"),
        Err(message) => assert!(
            message.contains("no font is available for face 'Ghost'"),
            "a missing face must raise the resolver's no-face error, got: {message}"
        ),
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

/// The reference reads the four arguments, disables the host font-selection
/// call and returns `0` without touching the font
/// (`reference/cpp/core/visual/LayerIntf.cpp:11931-11956`).
#[test]
fn font_user_select_returns_zero_without_changing_the_font() {
    let _lock = vm_lock();
    let env = Env::new(None);
    env.run("var f = new Font('Ghost', 16, 0);");

    assert_eq!(env.eval_int("f.doUserSelect(0, '', '', '')"), 0);
    assert_eq!(env.eval_int("f.face == 'Ghost'"), 1);
    // Fewer than four arguments is still an error (TJS_E_BADPARAMCOUNT).
    assert!(env.eval("f.doUserSelect(0, '', '')").is_err());
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

/// Reference: `tTVPPrerenderedFont`'s constructor reads the file and checks
/// the header, wrapping every failure in `TVPPrerenderedFontMappingFailed` =
/// "Prerendered font mapping failed : %1"
/// (`reference/cpp/core/base/impl/MsgImpl.h:58-59`). The sub-messages are
/// `TVPFileSizeIsZero` "File size is zero."
/// (`reference/cpp/core/base/MsgIntfInc.h:218`), `TVPInvalidPrerenderedFontFile`
/// "Signature not found or invalid pre-rendered font file." (`:221-222`),
/// `TVPNot16BitUnicodeFontFile` "Not a 16-bit UNICODE font file." (`:223`) and
/// `TVPInvalidHeaderVersion` "Invalid header version." (`:224`); the checks
/// themselves are at `reference/cpp/core/visual/PrerenderedFont.cpp:22-58`.
#[test]
fn font_map_prerendered_font_raises_the_reference_header_errors() {
    let _lock = vm_lock();
    let env = common::Env::with_storage();
    let game_dir = env.game_dir().expect("game dir").clone();
    std::fs::write(game_dir.join("empty.tft"), b"").expect("write empty");
    std::fs::write(game_dir.join("bad-signature.tft"), vec![0u8; 64]).expect("write bad signature");
    let mut bad_version = build_tft(&[b'A' as u16], 7);
    bad_version[22] = 2;
    std::fs::write(game_dir.join("bad-version.tft"), &bad_version).expect("write bad version");
    let mut not_unicode = build_tft(&[b'A' as u16], 7);
    not_unicode[23] = 1;
    std::fs::write(game_dir.join("not-unicode.tft"), &not_unicode).expect("write not-unicode");
    env.run("var f = new Font('Ghost', 16, 0);");

    let empty = env.error("f.mapPrerenderedFont('empty.tft')");
    assert!(
        empty.contains("Prerendered font mapping failed : File size is zero."),
        "unexpected empty-file message: {empty}"
    );
    let signature = env.error("f.mapPrerenderedFont('bad-signature.tft')");
    assert!(
        signature.contains(
            "Prerendered font mapping failed : Signature not found or invalid pre-rendered font file."
        ),
        "unexpected signature message: {signature}"
    );
    let version = env.error("f.mapPrerenderedFont('bad-version.tft')");
    assert!(
        version.contains("Prerendered font mapping failed : Invalid header version."),
        "unexpected version message: {version}"
    );
    let unicode = env.error("f.mapPrerenderedFont('not-unicode.tft')");
    assert!(
        unicode.contains("Prerendered font mapping failed : Not a 16-bit UNICODE font file."),
        "unexpected unicode message: {unicode}"
    );
    // A failed mapping is not recorded: the font stays usable and unmapped.
    assert_eq!(env.eval_int("f.height"), 16);
}

/// Reference: a mapped `.tft` supplies each character's `Inc` and
/// `GetTextSize` sums them
/// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:1435-1473`). Mapping
/// many fonts in one run must serve every one of them, so this maps 24 fonts
/// and asserts each one measures its own `Inc`. The port reads the glyph body
/// lazily where the reference reads the whole file at map time
/// (`reference/cpp/core/visual/PrerenderedFont.cpp:9-76`), so only the
/// retrievable metric is pinned — a lazy reader that never loaded a glyph no
/// longer passes.
#[test]
fn font_prerendered_map_serves_each_mapped_font() {
    let _lock = vm_lock();
    let Some((font_dir, font_name)) = test_font() else {
        return;
    };
    let font_bytes = std::fs::read(font_dir.join(&font_name)).expect("read test font");
    let env = common::Env::with_storage();
    let game_dir = env.game_dir().expect("game dir").clone();
    std::fs::write(game_dir.join("Pre"), &font_bytes).expect("write face");
    for i in 0..24u16 {
        std::fs::write(
            game_dir.join(format!("f{i}.tft")),
            build_tft(&[b'A' as u16], 100 + i as i16),
        )
        .expect("write tft");
    }
    env.run("var w = new Window(); var l = new Layer(w, null);");
    for i in 0..24u16 {
        let height = i + 1;
        env.run(&format!(
            "var f = new Font('Pre', {height}, 0); \
             f.mapPrerenderedFont('f{i}.tft'); \
             l.font.face = 'Pre'; \
             l.font.height = {height}; \
             l.font.bold = false; \
             l.font.italic = false; \
             l.font.angle = 0;"
        ));
        assert_eq!(
            env.eval_int("l.font.getTextWidth('A')"),
            i64::from(100 + i as i16),
            "font {i} should measure the `Inc` from its own `.tft`"
        );
    }
}

/// Reference: `TVPMapPrerenderedFont` reuses the `tTVPPrerenderedFont` already
/// in `TVPPrerenderedFonts` for the same placed path and `AddRef`s it instead
/// of reading again (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:129-147`).
/// The port's read counter is an internal detail, so this pins the observable
/// consequence: two font keys mapped to the same `.tft` both measure its
/// `Inc` from the shared body.
#[test]
fn font_prerendered_remap_serves_both_mappings() {
    let _lock = vm_lock();
    let Some((font_dir, font_name)) = test_font() else {
        return;
    };
    let font_bytes = std::fs::read(font_dir.join(&font_name)).expect("read test font");
    let env = common::Env::with_storage();
    let game_dir = env.game_dir().expect("game dir").clone();
    std::fs::write(game_dir.join("Pre"), &font_bytes).expect("write face");
    std::fs::write(game_dir.join("sample.tft"), build_tft(&[b'A' as u16], 7)).expect("write tft");
    env.run("var w = new Window(); var l = new Layer(w, null);");
    for height in [12i16, 16] {
        env.run(&format!(
            "var f = new Font('Pre', {height}, 0); \
             f.mapPrerenderedFont('sample.tft'); \
             l.font.face = 'Pre'; \
             l.font.height = {height}; \
             l.font.bold = false; \
             l.font.italic = false; \
             l.font.angle = 0;"
        ));
        assert_eq!(
            env.eval_int("l.font.getTextWidth('A')"),
            7,
            "height {height} should measure the shared `.tft`'s `Inc`"
        );
    }
}

/// Reference: `tTVPNativeBaseBitmap::DrawTextMultiple` writes each character's
/// pre-rendered coverage through `Retrieve` and `TVPUpscale65_255`
/// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:1285-1433`,
/// `reference/cpp/core/visual/PrerenderedFont.cpp:122-175`). This pins the
/// exact pixels, so the lazily read body must produce what the eager path did.
#[test]
fn layer_draw_text_prerendered_pixels_match_the_coverage() {
    let _lock = vm_lock();
    let Some((font_dir, font_name)) = test_font() else {
        return;
    };
    let font_bytes = std::fs::read(font_dir.join(&font_name)).expect("read test font");
    let env = common::Env::with_storage();
    let game_dir = env.game_dir().expect("game dir").clone();
    std::fs::write(game_dir.join("Pre"), &font_bytes).expect("write face");
    std::fs::write(game_dir.join("sample.tft"), build_tft(&[b'A' as u16], 3)).expect("write tft");
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
    // A fresh transparent layer painted source-over gives alpha == coverage, so
    // the 2x2 descriptor's decoded `[0x40, 0x80, 0xC0, 0xFC]` is the exact
    // painted block.
    let mut painted: Vec<(u32, u32, u8)> = Vec::new();
    for y in 0..bitmap.height {
        for x in 0..bitmap.width {
            let at = ((y * bitmap.width + x) * 4) as usize;
            if bitmap.rgba[at + 3] != 0 {
                painted.push((x, y, bitmap.rgba[at + 3]));
            }
        }
    }
    assert_eq!(painted.len(), 4, "exactly the 2x2 glyph is painted");
    let x0 = painted.iter().map(|p| p.0).min().expect("a painted pixel");
    let y0 = painted.iter().map(|p| p.1).min().expect("a painted pixel");
    assert_eq!(
        (
            painted.iter().map(|p| p.0).max().unwrap() - x0,
            painted.iter().map(|p| p.1).max().unwrap() - y0
        ),
        (1, 1),
        "the painted pixels form one 2x2 block"
    );
    let mut alphas = [0u8; 4];
    for (x, y, alpha) in &painted {
        alphas[((y - y0) * 2 + (x - x0)) as usize] = *alpha;
    }
    assert_eq!(alphas, [0x40, 0x80, 0xC0, 0xFC]);
}

/// Reference: a character the mapped `.tft` does not carry falls back to the
/// outline rasterizer, whose glyph box starts at `y + ascent - bitmap_top`
/// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:911-914`) with the
/// ascent truncated to whole pixels
/// (`reference/cpp/core/visual/FreeType.cpp:494`). A rounded ascent puts the
/// fallback glyph a pixel below the row the reference draws it on.
#[test]
fn a_prerendered_fallback_glyph_sits_on_the_truncated_ascent() {
    let _lock = vm_lock();
    let Some((font_dir, font_name)) = test_font() else {
        return;
    };
    let font_bytes = std::fs::read(font_dir.join(&font_name)).expect("read test font");
    let mut text_engine = engine::text::TextEngine::new();
    let face = text_engine
        .add_font(&font_bytes, 0)
        .expect("register the face");
    let mut style = engine::text::TextStyle::new(12.0);
    style.color = [255, 255, 255, 255];
    let metrics = text_engine.font_metrics(face, 12.0).expect("font metrics");
    let ascent = metrics.ascent.floor() as i32;
    let fallback = text_engine
        .rasterize_glyph(face, 'B', &style)
        .expect("rasterize the fallback glyph");
    let first_row = fallback
        .coverage
        .iter()
        .position(|coverage| *coverage > 0)
        .map(|index| index / fallback.width as usize)
        .expect("the fallback glyph has ink") as i32;

    let env = common::Env::with_storage();
    let game_dir = env.game_dir().expect("game dir").clone();
    // The face name carries a font extension, so the face resolves to the
    // stored bytes and the test does not depend on the host's fonts.
    std::fs::write(game_dir.join("Pre.ttf"), &font_bytes).expect("write face");
    // The `.tft` carries only 'A', so 'B' takes the rasterizer's path.
    std::fs::write(game_dir.join("sample.tft"), build_tft(&[b'A' as u16], 3)).expect("write tft");
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize(64, 64); \
         var f = new Font('Pre.ttf', 12, 0); \
         f.mapPrerenderedFont('sample.tft'); \
         l.font.face = 'Pre.ttf'; \
         l.font.height = 12; \
         l.font.bold = false; \
         l.font.italic = false; \
         l.font.angle = 0; \
         l.drawText(4, 4, 'B', 0x00ffffff, 255);",
    );
    let layer = env.scene.layers.last().expect("the layer exists");
    let bitmap = env
        .scene
        .bitmap(layer.bitmap.expect("the layer has a main image"))
        .expect("the bitmap exists");
    let ink_top = (0..bitmap.height)
        .position(|row| {
            (0..bitmap.width).any(|column| {
                let at = ((row * bitmap.width + column) * 4) as usize;
                bitmap.rgba[at + 3] != 0
            })
        })
        .map(|row| row as i32)
        .expect("the fallback glyph has ink");
    assert_eq!(
        ink_top,
        4 + ascent - fallback.bearing_y as i32 + first_row,
        "the fallback glyph must start at the baseline minus its bitmap top"
    );
}

#[test]
fn a_prerendered_glyph_is_drawn_at_the_ascent_minus_its_origin() {
    let _lock = vm_lock();
    let Some((font_dir, font_name)) = test_font() else {
        return;
    };
    let font_bytes = std::fs::read(font_dir.join(&font_name)).expect("read test font");
    let mut text_engine = engine::text::TextEngine::new();
    let face = text_engine
        .add_font(&font_bytes, 0)
        .expect("register the face");
    let metrics = text_engine.font_metrics(face, 12.0).expect("font metrics");
    let ascent = metrics.ascent.floor() as i32;

    let env = common::Env::with_storage();
    let game_dir = env.game_dir().expect("game dir").clone();
    // The face name carries a font extension, so the face resolves to the
    // stored bytes and the test does not depend on the host's fonts.
    std::fs::write(game_dir.join("Pre.ttf"), &font_bytes).expect("write face");
    // The fixture's descriptor has `origin_y == 0`, so the ink starts one
    // ascent below the draw position.
    std::fs::write(game_dir.join("sample.tft"), build_tft(&[b'A' as u16], 3)).expect("write tft");
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize(64, 64); \
         var f = new Font('Pre.ttf', 12, 0); \
         f.mapPrerenderedFont('sample.tft'); \
         l.font.face = 'Pre.ttf'; \
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
    let mut painted: Vec<(u32, u32, u8)> = Vec::new();
    for y in 0..bitmap.height {
        for x in 0..bitmap.width {
            let at = ((y * bitmap.width + x) * 4) as usize;
            if bitmap.rgba[at + 3] != 0 {
                painted.push((x, y, bitmap.rgba[at + 3]));
            }
        }
    }
    assert_eq!(painted.len(), 4, "exactly the 2x2 glyph is painted");
    let x0 = painted.iter().map(|p| p.0).min().expect("a painted pixel");
    let y0 = painted.iter().map(|p| p.1).min().expect("a painted pixel");
    assert_eq!(x0, 4, "the glyph starts at the draw position");
    assert_eq!(
        y0,
        (4 + ascent) as u32,
        "the glyph must start one ascent below the draw position"
    );
    let mut alphas = [0u8; 4];
    for (x, y, alpha) in &painted {
        alphas[((y - y0) * 2 + (x - x0)) as usize] = *alpha;
    }
    assert_eq!(alphas, [0x40, 0x80, 0xC0, 0xFC]);
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
        bitmap
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[3] != 0),
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

/// A script subclass may touch a native `Font` member before it calls
/// `super.Font(...)`. The reference's `CreateNativeInstance` returns a font
/// whose fields are usable (`reference/cpp/core/visual/LayerIntf.cpp:11500-11532`),
/// and `Construct` then replaces the native font with the default font or the
/// layer's font, so the constructor arguments win over a pre-constructor
/// write. The port used to reject such a write with
/// `Font: this font is not constructed`.
#[test]
fn subclass_member_writes_before_the_base_constructor_do_not_raise() {
    let _lock = vm_lock();
    let env = Env::new(None);
    env.run(
        r#"
        class EarlyFont extends Font {
            function EarlyFont() {
                face = 'pre-constructor';
                height = 99;
                bold = true;
                super.Font('constructor-face', 24, 0xffffffff);
            }
        }
        var f = new EarlyFont();
        "#,
    );
    // reference/cpp/core/visual/LayerIntf.cpp:11506-11530 — `Construct`
    // resets the native font, so the constructor's face/height/style win.
    assert_eq!(
        env.eval_ok("f.face"),
        TjsValue::String("constructor-face".into())
    );
    assert_eq!(env.eval_int("f.height"), 24);
    assert_eq!(env.eval_int("f.bold"), 0);
}
