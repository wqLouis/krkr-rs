//! Integration tests for the L2 font registry, the per-character fallback
//! and the measure-versus-draw split.
//!
//! The faces are synthetic (`testkit::font_fixtures`), so the tests do not
//! depend on the host's fonts: the narrow face carries Latin glyphs only and
//! the wide face carries one CJK glyph the narrow face does not have.
//!
//! Reference: the font map is keyed by the family names of the enumerated
//! faces and its oldest entry is the default font
//! (`reference/cpp/core/visual/FontImpl.cpp:242-245`); a character the
//! requested face does not map is drawn by the fallback face
//! (`reference/cpp/core/visual/FreeTypeFontRasterizer.cpp:165-195`); a
//! measurement follows the requested face
//! (`reference/cpp/core/visual/LayerIntf.cpp:11702-11713`).

use engine::font::FontId;
use engine::text::{TextEngine, TextError, TextStyle};
use testkit::font_fixtures::{
    NARROW_NAMES, WIDE_NAMES, WIDE_ONLY_CODEPOINT, narrow_face, wide_face,
};

/// The pixel size the measurements use.
const SIZE_PX: f32 = 32.0;

/// The narrow face's 'A' advance: 400 design units of a 1000-unit em, so a
/// third of the em, rounded to whole pixels.
const NARROW_A_ADVANCE: f32 = 13.0;

/// The wide face's CJK advance: 900 design units of a 1000-unit em, rounded to
/// whole pixels.
const WIDE_CJK_ADVANCE: f32 = 29.0;

/// The style the measurements use.
fn style() -> TextStyle {
    TextStyle::new(SIZE_PX)
}

/// An engine with the wide face registered first, so the wide face is the
/// default face the per-character fallback reaches for.
fn engine_with_wide_default() -> (TextEngine, FontId, FontId) {
    let mut engine = TextEngine::new();
    let wide = engine
        .add_font(&wide_face(), 0)
        .expect("register the wide face");
    let narrow = engine
        .add_font(&narrow_face(), 0)
        .expect("register the narrow face");
    (engine, wide, narrow)
}

/// An engine with the two roles the host fills: the narrow face is the
/// default face (it supplies the metrics of a face name that resolves to
/// nothing) and the wide face is the coverage face (it draws a character the
/// requested face does not carry).
fn engine_with_two_roles() -> (TextEngine, FontId, FontId) {
    let mut engine = TextEngine::new();
    let narrow = engine
        .add_font(&narrow_face(), 0)
        .expect("register the narrow face");
    let wide = engine
        .add_font(&wide_face(), 0)
        .expect("register the wide face");
    engine
        .designate_coverage_face(wide)
        .expect("designate the coverage face");
    (engine, narrow, wide)
}

/// The two roles are two faces: the default face supplies the metrics of a
/// face name that resolves to nothing, and the coverage face draws the
/// characters the requested face does not carry.
///
/// Reference: a face name that resolves to nothing takes the default font's
/// metrics (`reference/cpp/core/visual/FontSystem.cpp:53-102`), and the
/// rasterizer falls back to the default font for a character the requested
/// face does not carry
/// (`reference/cpp/core/visual/FreeTypeFontRasterizer.cpp:165-195`).
#[test]
fn the_coverage_face_draws_the_characters_the_default_face_lacks() {
    let (mut engine, narrow, wide) = engine_with_two_roles();
    assert_eq!(
        engine.default_face(),
        Some(narrow),
        "the default face is the oldest registered entry"
    );
    assert_eq!(
        engine.coverage_face(),
        Some(wide),
        "the coverage face is the face the host designated"
    );
    assert!(
        !engine.has_glyph(narrow, WIDE_ONLY_CODEPOINT),
        "the default face must not carry the CJK glyph"
    );
    assert_eq!(
        engine
            .provider_for(narrow, WIDE_ONLY_CODEPOINT)
            .expect("the provider"),
        wide,
        "a character the requested face lacks is drawn by the coverage face"
    );
    assert_eq!(
        engine.provider_for(narrow, 'A').expect("the provider"),
        narrow,
        "a character the requested face carries is drawn by it"
    );
}

/// A registry that designated no coverage face reaches for the default face,
/// so a host whose default font carries the characters keeps one face in both
/// roles.
///
/// Reference: the rasterizer falls back to the default font for a character
/// the requested face does not carry
/// (`reference/cpp/core/visual/FreeTypeFontRasterizer.cpp:165-195`).
#[test]
fn a_registry_without_a_coverage_face_reaches_for_the_default_face() {
    let (mut engine, wide, narrow) = engine_with_wide_default();
    assert_eq!(
        engine.coverage_face(),
        Some(wide),
        "the default face fills the coverage role"
    );
    assert_eq!(
        engine
            .provider_for(narrow, WIDE_ONLY_CODEPOINT)
            .expect("the provider"),
        wide,
        "a character the requested face lacks is drawn by the default face"
    );
}

/// The default face is the oldest registered entry: the face registered
/// first stays the default, and a later registration does not move it.
///
/// Reference: the font map's oldest entry is the default font
/// (`reference/cpp/core/visual/FontImpl.cpp:242-245`).
#[test]
fn the_default_face_is_the_oldest_registered_entry() {
    let mut engine = TextEngine::new();
    let narrow = engine
        .add_font(&narrow_face(), 0)
        .expect("register the narrow face");
    assert_eq!(
        engine.default_face(),
        Some(narrow),
        "the first registered face is the default face"
    );
    assert_eq!(engine.registry().default_name(), Some(NARROW_NAMES[0]));
    let wide = engine
        .add_font(&wide_face(), 0)
        .expect("register the wide face");
    assert_eq!(
        engine.default_face(),
        Some(narrow),
        "a later registration must not move the default face"
    );
    assert_ne!(engine.default_face(), Some(wide));
}

/// The registered-name list is newest first and leaves the oldest name out.
///
/// Reference: the font list enumerates the map and stops at its oldest entry,
/// so the default face's name is not in the list and a map that holds a single
/// name reports an empty list
/// (`reference/cpp/core/visual/FontImpl.cpp:242-245`).
#[test]
fn the_registered_name_list_is_newest_first_without_the_oldest_name() {
    let mut engine = TextEngine::new();
    engine
        .add_font(&narrow_face(), 0)
        .expect("register the narrow face");
    assert_eq!(
        engine.registry().list_names(),
        vec![NARROW_NAMES[1].to_string()],
        "the oldest name is the default face's name and stays out of the list"
    );
    engine
        .add_font(&wide_face(), 0)
        .expect("register the wide face");
    assert_eq!(
        engine.registry().list_names(),
        vec![
            WIDE_NAMES[1].to_string(),
            WIDE_NAMES[0].to_string(),
            NARROW_NAMES[1].to_string(),
        ],
        "the list is newest first and leaves the oldest name out"
    );
}

/// A face answers to each of its names, in either case.
///
/// Reference: the font map is keyed by every family name the enumerated face
/// carries (`reference/cpp/core/visual/FontImpl.cpp:82-124`) and a lookup
/// matches the requested name against that map
/// (`reference/cpp/core/visual/FontImpl.cpp:277-291`).
#[test]
fn a_face_resolves_under_each_of_its_names() {
    let mut engine = TextEngine::new();
    let narrow = engine
        .add_font(&narrow_face(), 0)
        .expect("register the narrow face");
    assert_eq!(engine.font_id(NARROW_NAMES[0]), Some(narrow));
    assert_eq!(engine.font_id(NARROW_NAMES[1]), Some(narrow));
    assert_eq!(
        engine.font_id(&NARROW_NAMES[0].to_lowercase()),
        Some(narrow),
        "the lookup is case-insensitive"
    );
    assert_eq!(engine.font_id("no-such-family"), None);
}

/// The fallback is per character: a character the requested face does not map
/// is drawn by the default face, and a character it does map is drawn by
/// itself.
///
/// Reference: the rasterizer asks the requested face first and falls back to
/// the default face for a character the requested face does not carry
/// (`reference/cpp/core/visual/FreeTypeFontRasterizer.cpp:165-195`).
#[test]
fn the_fallback_is_per_character() {
    let (engine, wide, narrow) = engine_with_wide_default();
    assert!(
        !engine.has_glyph(narrow, WIDE_ONLY_CODEPOINT),
        "the narrow face must not carry the CJK glyph"
    );
    assert!(engine.has_glyph(wide, WIDE_ONLY_CODEPOINT));
    assert_eq!(
        engine.provider_for(narrow, 'A').expect("the provider"),
        narrow,
        "a character the requested face carries is drawn by it"
    );
    assert_eq!(
        engine
            .provider_for(narrow, WIDE_ONLY_CODEPOINT)
            .expect("the provider"),
        wide,
        "a character the requested face lacks is drawn by the default face"
    );
    assert!(
        matches!(
            engine.provider_for(wide, WIDE_ONLY_CODEPOINT),
            Ok(id) if id == wide
        ),
        "the default face draws the character it carries"
    );
}

/// A character no registered face carries has no glyph at all: the fallback
/// reports it instead of drawing another character's shape.
#[test]
fn a_character_no_face_carries_has_no_provider() {
    let (engine, _wide, narrow) = engine_with_wide_default();
    assert!(matches!(
        engine.provider_for(narrow, '\u{10FFFD}'),
        Err(TextError::MissingGlyph { .. })
    ));
}

/// The measurement follows the requested face, the drawing follows the face
/// that maps the character.
///
/// Reference: the measurement is the requested face's extent, and a character
/// the requested face does not carry measures the face's height box
/// (`reference/cpp/core/visual/LayerIntf.cpp:11702-11713`); the drawn advance
/// is the drawn glyph's own advance
/// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:1367`).
#[test]
fn the_measurement_follows_the_requested_face_and_the_drawing_the_provider() {
    let (mut engine, wide, narrow) = engine_with_wide_default();
    let style = style();
    assert_eq!(
        engine
            .measure_code_unit(narrow, 'A', &style)
            .expect("measure"),
        NARROW_A_ADVANCE,
        "a character the requested face carries measures the face's advance"
    );
    assert_eq!(
        engine
            .measure_code_unit(narrow, WIDE_ONLY_CODEPOINT, &style)
            .expect("measure"),
        SIZE_PX,
        "a character the requested face lacks measures the face's em box"
    );
    let drawn = engine
        .rasterize_glyph(wide, WIDE_ONLY_CODEPOINT, &style)
        .expect("the default face draws the character");
    assert_eq!(
        drawn.advance, WIDE_CJK_ADVANCE,
        "the drawn advance is the provider's advance"
    );
    assert_ne!(
        drawn.advance,
        engine
            .measure_code_unit(narrow, WIDE_ONLY_CODEPOINT, &style)
            .expect("measure the requested face"),
        "the measured and the drawn advance differ for a fallback character"
    );
}

/// A shaped run names the face that draws each of its characters, so a
/// character the requested face lacks is drawn by the fallback face.
#[test]
fn a_shaped_run_names_the_face_that_draws_each_character() {
    let (mut engine, wide, narrow) = engine_with_wide_default();
    let style = style();
    let layout = engine
        .layout(narrow, "A\u{4E00}", &style, None)
        .expect("shape the run");
    assert_eq!(layout.glyphs.len(), 2);
    assert_eq!(layout.glyphs[0].codepoint, 'A');
    assert_eq!(layout.glyphs[0].font, narrow);
    assert_eq!(layout.glyphs[1].codepoint, WIDE_ONLY_CODEPOINT);
    assert_eq!(
        layout.glyphs[1].font, wide,
        "the fallback character names the face that draws it"
    );
}
