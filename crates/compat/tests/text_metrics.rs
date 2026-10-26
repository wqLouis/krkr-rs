//! Integration tests for the text metrics the script surface exposes.
//!
//! The members under test are Layer.getTextWidth/getTextHeight,
//! Font.getTextWidth/getTextHeight and the pixels Layer.drawText paints.
//! The expectations are the reference own formulas:
//!
//! * a layer whose font is mapped to a pre-rendered .tft measures the
//!   file Inc values, a font that is not attached to a layer measures the
//!   rasterizer per-character advance, and the height is the absolute font
//!   height;
//! * the drawn glyph starts at the truncated ascent below the pen position;
//! * the drawn pixels carry the colour the script asked for.
//!
//! The positive cases mount a directory holding a system font and skip when
//! no usable font is present (mirroring the L2/L1 text tests). The tests
//! serialize on the shared VM lock because the engine to context binding is
//! process-wide and single.

mod common;

use std::path::{Path, PathBuf};

use common::vm_lock;
use engine::text::{TextEngine, TextStyle};

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
/// Returns the font directory (the storage mount root) and its file name.
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

/// Builds a version-1 .tft whose characters carry their own Inc.
///
/// Every character gets a 2x2 coverage of the literal bytes 0x10, 0x20,
/// 0x30, 0x3F; the descriptor inc is the value under test.
fn build_tft(chars: &[(u16, i16)]) -> Vec<u8> {
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
    for (i, (ch, _)) in chars.iter().enumerate() {
        bytes[ch_index_offset + i * 2..ch_index_offset + i * 2 + 2]
            .copy_from_slice(&ch.to_le_bytes());
    }
    for (i, (_, inc)) in chars.iter().enumerate() {
        let at = index_offset + i * 20;
        let offset = (data_offset + i * 4) as u32;
        bytes[at..at + 4].copy_from_slice(&offset.to_le_bytes());
        bytes[at + 4..at + 6].copy_from_slice(&2u16.to_le_bytes());
        bytes[at + 6..at + 8].copy_from_slice(&2u16.to_le_bytes());
        bytes[at + 16..at + 18].copy_from_slice(&inc.to_le_bytes());
    }
    for _ in chars {
        bytes.extend_from_slice(&[0x10, 0x20, 0x30, 0x3F]);
    }
    bytes
}

/// The painted pixels of the layer the script created last.
fn painted_pixels(env: &common::Env) -> Vec<(u32, u32, [u8; 4])> {
    let layer = env.scene.layers.last().expect("the layer exists");
    let bitmap = env
        .scene
        .bitmap(layer.bitmap.expect("the layer has a main image"))
        .expect("the bitmap exists");
    let mut painted = Vec::new();
    for y in 0..bitmap.height {
        for x in 0..bitmap.width {
            let at = ((y * bitmap.width + x) * 4) as usize;
            if bitmap.rgba[at + 3] != 0 {
                painted.push((
                    x,
                    y,
                    [
                        bitmap.rgba[at],
                        bitmap.rgba[at + 1],
                        bitmap.rgba[at + 2],
                        bitmap.rgba[at + 3],
                    ],
                ));
            }
        }
    }
    painted
}

/// Reference: tTVPNativeBaseBitmap::GetTextSize sums the pre-rendered font
/// Inc for every UTF-16 unit and falls back to the rasterizer extent for a
/// unit the .tft does not carry
/// (reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:1435-1473), and
/// tTJSNI_BaseLayer::GetTextWidth/GetTextHeight return that size
/// (reference/cpp/core/visual/LayerIntf.cpp:6065-6084). The layer font is
/// the mapped one, so the width is the .tft Inc sum and the height is the
/// absolute font height.
#[test]
fn a_mapped_layer_font_measures_the_prerendered_inc_sum() {
    let _lock = vm_lock();
    let Some((font_dir, font_name)) = test_font() else {
        return;
    };
    let font_bytes = std::fs::read(font_dir.join(&font_name)).expect("read test font");
    let env = common::Env::with_storage();
    let game_dir = env.game_dir().expect("game dir").clone();
    std::fs::write(game_dir.join("Pre.ttf"), &font_bytes).expect("write face");
    std::fs::write(
        game_dir.join("sample.tft"),
        build_tft(&[(b'A' as u16, 7), (b'B' as u16, 5)]),
    )
    .expect("write tft");
    env.run(
        "var w = new Window(); \
         var base = new Layer(w, null); \
         var l = new Layer(w, base); \
         l.setImageSize(64, 64); \
         var f = new Font('Pre.ttf', 12, 0); \
         f.mapPrerenderedFont('sample.tft'); \
         l.font.face = 'Pre.ttf'; \
         l.font.height = 12; \
         l.font.bold = false; \
         l.font.italic = false; \
         l.font.angle = 0;",
    );
    assert_eq!(
        env.eval_int("l.getTextWidth('AB')"),
        12,
        "the layer measures the .tft Inc sum"
    );
    assert_eq!(
        env.eval_int("l.font.getTextWidth('AB')"),
        12,
        "the layer font measures the .tft Inc sum"
    );
    assert_eq!(
        env.eval_int("l.getTextHeight('AB')"),
        12,
        "the height is the absolute font height"
    );
    assert_eq!(
        env.eval_int("l.font.getTextHeight('AB')"),
        12,
        "the height is the absolute font height"
    );
}

/// Reference: tTJSNI_Font::GetTextWidth returns the layer width when the
/// font is attached to a layer and the rasterizer per-character extent sum
/// otherwise (reference/cpp/core/visual/LayerIntf.cpp:11702-11731; the
/// extent itself is reference/cpp/core/visual/FreeTypeFontRasterizer.cpp:139-152).
/// A font that is not attached to a layer therefore measures the rasterizer
/// and not the pre-rendered file, and the two differ for the same string.
#[test]
fn a_detached_font_measures_the_rasterizer_advance_sum() {
    let _lock = vm_lock();
    let Some((font_dir, font_name)) = test_font() else {
        return;
    };
    let font_bytes = std::fs::read(font_dir.join(&font_name)).expect("read test font");
    let mut text_engine = TextEngine::new();
    let face = text_engine
        .add_font(&font_bytes, 0)
        .expect("register the face");
    let mut style = TextStyle::new(12.0);
    style.color = [255, 255, 255, 255];
    let mut expected = 0i64;
    for ch in "AB".chars() {
        let advance = text_engine
            .glyph_advance(face, ch, &style)
            .expect("the rasterizer advance");
        expected += advance.round() as i64;
    }

    let env = common::Env::with_storage();
    let game_dir = env.game_dir().expect("game dir").clone();
    std::fs::write(game_dir.join("Pre.ttf"), &font_bytes).expect("write face");
    std::fs::write(
        game_dir.join("sample.tft"),
        build_tft(&[(b'A' as u16, 7), (b'B' as u16, 5)]),
    )
    .expect("write tft");
    env.run(
        "var w = new Window(); \
         var base = new Layer(w, null); \
         var l = new Layer(w, base); \
         l.setImageSize(64, 64); \
         var f = new Font('Pre.ttf', 12, 0); \
         f.mapPrerenderedFont('sample.tft'); \
         l.font.face = 'Pre.ttf'; \
         l.font.height = 12; \
         l.font.bold = false; \
         l.font.italic = false; \
         l.font.angle = 0; \
         var detached = new Font('Pre.ttf', 12, 0);",
    );
    assert_eq!(
        env.eval_int("detached.getTextWidth('AB')"),
        expected,
        "a font that is not attached to a layer measures the rasterizer"
    );
    assert_eq!(
        env.eval_int("l.getTextWidth('AB')"),
        12,
        "the layer font measures the .tft Inc sum"
    );
    assert_eq!(
        env.eval_int("detached.getTextHeight('AB')"),
        12,
        "the height is the absolute font height"
    );
}

/// Reference: the glyph is placed with its baseline one truncated ascent
/// below the pen position y - the ascent is the face ascender scaled to the
/// pixel size and cut at the integer boundary
/// (reference/cpp/core/visual/FreeType.h:96-100,
/// reference/cpp/core/visual/FreeType.cpp:494-497) and the glyph origin is
/// baseline - bitmap_top
/// (reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:911-914). The drawn
/// ink therefore starts at y + ascent - bitmap_top.
#[test]
fn the_drawn_glyph_sits_on_the_truncated_ascent() {
    let _lock = vm_lock();
    let Some((font_dir, font_name)) = test_font() else {
        return;
    };
    let font_bytes = std::fs::read(font_dir.join(&font_name)).expect("read test font");
    let mut text_engine = TextEngine::new();
    let face = text_engine
        .add_font(&font_bytes, 0)
        .expect("register the face");
    let mut style = TextStyle::new(12.0);
    style.color = [255, 255, 255, 255];
    let metrics = text_engine.font_metrics(face, 12.0).expect("font metrics");
    let ascent = metrics.ascent.floor() as i32;
    let glyph = text_engine
        .rasterize_glyph(face, 'A', &style)
        .expect("rasterize the glyph");
    let first_row = glyph
        .coverage
        .iter()
        .position(|coverage| *coverage > 0)
        .map(|index| index / glyph.width as usize)
        .expect("the glyph has ink") as i32;

    let env = common::Env::with_storage();
    let game_dir = env.game_dir().expect("game dir").clone();
    std::fs::write(game_dir.join("Pre.ttf"), &font_bytes).expect("write face");
    env.run(
        "var w = new Window(); \
         var base = new Layer(w, null); \
         var l = new Layer(w, base); \
         l.setImageSize(64, 64); \
         l.font.face = 'Pre.ttf'; \
         l.font.height = 12; \
         l.font.bold = false; \
         l.font.italic = false; \
         l.font.angle = 0; \
         l.drawText(4, 4, 'A', 0x00ffffff, 255);",
    );
    let painted = painted_pixels(&env);
    let ink_top = painted
        .iter()
        .map(|pixel| pixel.1)
        .min()
        .expect("the glyph has ink") as i32;
    assert_eq!(
        ink_top,
        4 + ascent - glyph.bearing_y as i32 + first_row,
        "the ink starts one ascent below the pen position"
    );
}

/// Reference: tTJSNI_BaseLayer::DrawText paints the text with the colour the
/// script passed, after resolving it through the system-colour table
/// (reference/cpp/core/visual/LayerIntf.cpp:4433-4470). A plain 0x00RRGGBB
/// colour is painted as it is, so the pixels of a black glyph are black and
/// the alpha is the glyph coverage.
#[test]
fn the_drawn_text_carries_the_requested_colour() {
    let _lock = vm_lock();
    let Some((font_dir, font_name)) = test_font() else {
        return;
    };
    let font_bytes = std::fs::read(font_dir.join(&font_name)).expect("read test font");
    let env = common::Env::with_storage();
    let game_dir = env.game_dir().expect("game dir").clone();
    std::fs::write(game_dir.join("Pre.ttf"), &font_bytes).expect("write face");
    env.run(
        "var w = new Window(); \
         var base = new Layer(w, null); \
         var l = new Layer(w, base); \
         l.setImageSize(64, 64); \
         l.font.face = 'Pre.ttf'; \
         l.font.height = 12; \
         l.font.bold = false; \
         l.font.italic = false; \
         l.font.angle = 0; \
         l.drawText(4, 4, 'A', 0x00000000, 255);",
    );
    let painted = painted_pixels(&env);
    assert!(!painted.is_empty(), "the glyph has ink");
    for (x, y, pixel) in &painted {
        assert_eq!(
            (pixel[0], pixel[1], pixel[2]),
            (0u8, 0u8, 0u8),
            "the pixel at ({x}, {y}) is painted in the requested colour"
        );
    }
}

/// Reference: the rasterizer measures a character with the glyph metrics the
/// face carries, and the synthetic bold widens the loaded glyph and its drawn
/// advance without touching those metrics
/// (reference/cpp/core/visual/FreeTypeFontRasterizer.cpp:139-152 and
/// reference/cpp/core/visual/FreeType.cpp:640-680). A bold text therefore
/// measures exactly as the plain text does, and the measured width is the
/// sum of the plain per-character advances
/// (reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:1435-1473).
#[test]
fn a_bold_font_measures_the_same_width_as_the_plain_font() {
    let _lock = vm_lock();
    let Some((font_dir, font_name)) = test_font() else {
        return;
    };
    let font_bytes = std::fs::read(font_dir.join(&font_name)).expect("read test font");
    let mut text_engine = TextEngine::new();
    let face = text_engine
        .add_font(&font_bytes, 0)
        .expect("register the face");
    let mut style = TextStyle::new(12.0);
    style.color = [255, 255, 255, 255];
    let mut expected = 0i64;
    for ch in "AB".chars() {
        let advance = text_engine
            .glyph_advance(face, ch, &style)
            .expect("the rasterizer advance");
        expected += advance.round() as i64;
    }

    let env = common::Env::with_storage();
    let game_dir = env.game_dir().expect("game dir").clone();
    std::fs::write(game_dir.join("Pre.ttf"), &font_bytes).expect("write face");
    env.run(
        "var plain = new Font('Pre.ttf', 12, 0); \
         var bold = new Font('Pre.ttf', 12, 0); \
         bold.bold = true;",
    );
    assert_eq!(
        env.eval_int("plain.getTextWidth('AB')"),
        expected,
        "the plain width is the rasterizer advance sum"
    );
    assert_eq!(
        env.eval_int("bold.getTextWidth('AB')"),
        expected,
        "the synthetic bold does not change the measured width"
    );
}
