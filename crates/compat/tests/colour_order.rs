//! Integration tests for the colour byte order of the draw paths.
//!
//! A script colour is a `0x00RRGGBB` value. The paths that paint the colour
//! resolve a system colour through the colour table and hand the resolved
//! colour to the image, which reads the value's low bytes as the red, green
//! and blue planes
//! (`reference/cpp/core/visual/impl/LayerImpl.cpp:20-31`,
//! `reference/cpp/core/environ/Application.cpp:874-941`,
//! `reference/cpp/core/visual/RenderManager.cpp:346-350`). The paths that
//! hand the colour to the image as it is paint the red and the blue the
//! other way round (`reference/cpp/core/visual/LayerIntf.h:449-452`,
//! `reference/cpp/core/visual/LayerIntf.cpp:4465-4468`, `:4513-4516`,
//! `reference/cpp/core/visual/ImageFunction.cpp:1057-1059`, `:1159-1162`,
//! `reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:836-903`).
//!
//! The tests drive the real script surface: they draw through the VM and
//! read the pixels the draw left behind.
//!
//! A pixel getter hands the colour back as the packed word of the RGBA
//! planes, read little-endian: the red is in the low byte, the green in the
//! second and the blue in the third
//! (`reference/cpp/core/visual/BitmapIntf.cpp:51-58`,
//! `reference/cpp/core/visual/LayerIntf.cpp:2917-2923`,
//! `reference/cpp/core/visual/RenderManager.cpp:346-350`).

mod common;

use std::path::Path;

use common::Env;
use common::vm_lock;

/// The plain colour the byte-order tests paint with: its red, green and blue
/// differ, so a swapped pair is visible.
const PLAIN: i64 = 0x0020_3040;

/// `clHighlight` — the system colour identifier of the selection
/// (`reference/cpp/core/visual/impl/TVPColor.h:6-45`).
const CL_HIGHLIGHT: i64 = 0x8000_000d;

/// The system fonts the text tests try; a missing or unusable font skips the
/// font-dependent case rather than failing it.
const TEST_FONT_PATHS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
];

/// Copy a font the L2 text engine can shape into the environment's storage
/// and return its file name, so the script can name it as a face.
fn font_in_storage(env: &Env) -> Option<String> {
    for candidate in TEST_FONT_PATHS {
        let path = Path::new(candidate);
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        if engine::text::TextEngine::new().add_font(&bytes, 0).is_err() {
            continue;
        }
        let name = path.file_name()?.to_string_lossy().into_owned();
        let dir = env.game_dir()?;
        std::fs::copy(path, dir.join(&name)).expect("copy the font into the storage");
        return Some(name);
    }
    None
}

/// The red, green and blue bytes the script's pixel getter reports.
///
/// The getter hands the colour back as the packed word of the RGBA planes,
/// read little-endian, so the red is in the low byte and the blue in the
/// third (`reference/cpp/core/visual/BitmapIntf.cpp:51-58`,
/// `reference/cpp/core/visual/LayerIntf.cpp:2917-2923`,
/// `reference/cpp/core/visual/RenderManager.cpp:346-350`).
fn reported_rgb(env: &Env, expression: &str) -> [u8; 3] {
    let value = env.eval_int(expression) as u32;
    [value as u8, (value >> 8) as u8, (value >> 16) as u8]
}

/// The red, green and blue bytes one pixel of the last layer's main image
/// carries.
fn layer_pixel_rgb(env: &Env, x: u32, y: u32) -> [u8; 3] {
    let layer = env.scene.layers.last().expect("the layer");
    let bitmap_id = layer.bitmap.expect("the layer has a main image");
    let bitmap = env.scene.bitmap(bitmap_id).expect("the main image");
    let offset = bitmap.pixel_offset(x, y).expect("the pixel");
    [
        bitmap.rgba[offset],
        bitmap.rgba[offset + 1],
        bitmap.rgba[offset + 2],
    ]
}

/// The red, green and blue bytes the covered pixels of the last layer's main
/// image carry.
///
/// A pixel counts as covered when its alpha byte is not zero, which is what
/// the drawn text leaves behind.
fn layer_colours(env: &Env) -> Vec<[u8; 3]> {
    let layer = env.scene.layers.last().expect("the layer");
    let bitmap_id = layer.bitmap.expect("the layer has a main image");
    let bitmap = env.scene.bitmap(bitmap_id).expect("the main image");
    bitmap
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| pixel[3] > 0)
        .map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect()
}

/// The red, green and blue bytes every pixel of the last layer's main image
/// carries, whatever its alpha byte is.
///
/// The alpha byte is not a coverage test: the reference's `FillColor` keeps
/// the destination's alpha and replaces the colour bits
/// (`reference/cpp/core/visual/LayerBitmapIntf.cpp:367-380`,
/// `reference/cpp/core/visual/gl/blend_functor_c.h:747-753`), so a fill on
/// the opaque face leaves a fresh image's pixels at alpha zero while they
/// carry the colour.
fn layer_pixel_colours(env: &Env) -> Vec<[u8; 3]> {
    let layer = env.scene.layers.last().expect("the layer");
    let bitmap_id = layer.bitmap.expect("the layer has a main image");
    let bitmap = env.scene.bitmap(bitmap_id).expect("the main image");
    bitmap
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect()
}

/// The red, green and blue bytes the covered pixels of the bitmap at `index`
/// carry.
fn bitmap_colours(env: &Env, index: usize) -> Vec<[u8; 3]> {
    let bitmap = env.scene.bitmaps.get(index).expect("the bitmap");
    bitmap
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| pixel[3] > 0)
        .map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect()
}

/// Reference: `Layer.fillRect` paints the colour through the system-colour
/// table on the faces that resolve it and keeps the `0xAARRGGBB` meaning on
/// the alpha face (`reference/cpp/core/visual/LayerIntf.cpp:4277-4295`,
/// `reference/cpp/core/visual/impl/LayerImpl.cpp:20-31`). The image reads
/// the value's low bytes as the red, green and blue planes
/// (`reference/cpp/core/visual/RenderManager.cpp:346-350`), so a plain
/// `0x00RRGGBB` colour paints the red, green and blue it names.
#[test]
fn layer_fill_rect_paints_the_named_channels() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&format!(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize(2, 2); \
         l.type = ltAlpha; \
         l.fillRect(0, 0, 2, 2, {PLAIN:#x});"
    ));
    assert_eq!(
        reported_rgb(&env, "l.getMainPixel(0,  0)"),
        [0x20, 0x30, 0x40]
    );
    assert_eq!(
        reported_rgb(&env, "l.getMainPixel(1, 1)"),
        [0x20, 0x30, 0x40]
    );
    assert_eq!(layer_pixel_rgb(&env, 0, 0), [0x20, 0x30, 0x40]);
    assert_eq!(layer_pixel_rgb(&env, 1, 1), [0x20, 0x30, 0x40]);
}

/// Reference: `Layer.colorRect` paints the colour through the system-colour
/// table on the faces that paint it (`reference/cpp/core/visual/LayerIntf.cpp:4348-4382`,
/// `reference/cpp/core/visual/impl/LayerImpl.cpp:20-31`), so a plain
/// `0x00RRGGBB` colour paints the red, green and blue it names.
#[test]
fn layer_color_rect_paints_the_named_channels() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&format!(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize(2, 2); \
         l.type = ltAlpha; \
         l.colorRect(0, 0, 2, 2, {PLAIN:#x}, 255);"
    ));
    assert_eq!(
        reported_rgb(&env, "l.getMainPixel(0,  0)"),
        [0x20, 0x30, 0x40]
    );
    assert_eq!(
        reported_rgb(&env, "l.getMainPixel(1, 1)"),
        [0x20, 0x30, 0x40]
    );
    assert_eq!(layer_pixel_rgb(&env, 0, 0), [0x20, 0x30, 0x40]);
    assert!(layer_colours(&env).contains(&[0x20, 0x30, 0x40]));
}

/// Reference: `Layer.drawText` hands the colour to the image with the red
/// and the blue bytes swapped (`reference/cpp/core/visual/LayerIntf.cpp:4465-4468`,
/// `reference/cpp/core/visual/LayerIntf.h:449-452`), and the image reads the
/// value's low bytes as the red, green and blue planes
/// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:836-903`). The
/// swapped value paints the red, green and blue the script named.
#[test]
fn layer_draw_text_paints_the_named_channels() {
    let _lock = vm_lock();
    let env = Env::with_storage();
    let Some(name) = font_in_storage(&env) else {
        return;
    };
    env.run(&format!(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize(96, 96); \
         l.type = ltAlpha; \
         l.font.face = '{name}'; \
         l.font.height = 40; \
         l.drawText(20, 20, 'A', {PLAIN:#x}, 255, true);"
    ));
    let colours = layer_colours(&env);
    assert!(
        colours.contains(&[0x20, 0x30, 0x40]),
        "the drawn colour is missing from {colours:?}"
    );
    assert!(
        !colours.contains(&[0x40, 0x30, 0x20]),
        "the drawn colour has the red and the blue swapped: {colours:?}"
    );
}

/// Reference: `Layer.drawGlyph` hands the colour to the image as it is
/// (`reference/cpp/core/visual/LayerIntf.cpp:4513-4516`) and the image
/// reads the value's low bytes as the red, green and blue planes
/// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:836-903`), so the
/// glyph paints the red and the blue the other way round.
#[test]
fn layer_draw_glyph_paints_the_swapped_channels() {
    let _lock = vm_lock();
    let env = Env::with_storage();
    let Some(name) = font_in_storage(&env) else {
        return;
    };
    env.run(&format!(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize(96, 96); \
         l.type = ltAlpha; \
         l.font.face = '{name}'; \
         l.font.height = 40; \
         l.drawGlyph(20, 20, 'A', {PLAIN:#x}, 255, true);"
    ));
    let colours = layer_colours(&env);
    assert!(
        colours.contains(&[0x40, 0x30, 0x20]),
        "the drawn colour is missing from {colours:?}"
    );
    assert!(
        !colours.contains(&[0x20, 0x30, 0x40]),
        "the drawn colour has the red and the blue in the named order: {colours:?}"
    );
}

/// Reference: `ImageFunction.fillRect` paints the colour through the
/// system-colour table (`reference/cpp/core/visual/ImageFunction.cpp:836-845`,
/// `reference/cpp/core/visual/impl/LayerImpl.cpp:20-31`), so a plain
/// `0x00RRGGBB` colour paints the red, green and blue it names.
#[test]
fn image_function_fill_rect_paints_the_named_channels() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&format!(
        "var b = new Bitmap(2, 2); \
         ImageFunction.fillRect(b, {PLAIN:#x}, null, true, null);"
    ));
    assert_eq!(reported_rgb(&env, "b.getPixel(0, 0)"), [0x20, 0x30, 0x40]);
    assert_eq!(reported_rgb(&env, "b.getPixel(1, 1)"), [0x20, 0x30, 0x40]);
}

/// Reference: `ImageFunction.colorRect` paints the colour through the
/// system-colour table on the faces that paint it
/// (`reference/cpp/core/visual/ImageFunction.cpp:917-945`,
/// `reference/cpp/core/visual/impl/LayerImpl.cpp:20-31`), so a plain
/// `0x00RRGGBB` colour paints the red, green and blue it names.
#[test]
fn image_function_color_rect_paints_the_named_channels() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&format!(
        "var b = new Bitmap(2, 2); \
         ImageFunction.colorRect(b, {PLAIN:#x}, 255, null, dfAlpha, null);"
    ));
    assert_eq!(reported_rgb(&env, "b.getPixel(0, 0)"), [0x20, 0x30, 0x40]);
    assert_eq!(reported_rgb(&env, "b.getPixel(1, 1)"), [0x20, 0x30, 0x40]);
}

/// Reference: `ImageFunction.drawText` hands the colour to the image as it
/// is (`reference/cpp/core/visual/ImageFunction.cpp:1057-1059`) and the image
/// reads the value's low bytes as the red, green and blue planes
/// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:836-903`), so the
/// text paints the red and the blue the other way round.
#[test]
fn image_function_draw_text_paints_the_swapped_channels() {
    let _lock = vm_lock();
    let env = Env::with_storage();
    let Some(name) = font_in_storage(&env) else {
        return;
    };
    let first_bitmap = env.scene.bitmaps.len();
    env.run(&format!(
        "var f = new Font('{name}', 40, 0xffffffff); \
         var b = new Bitmap(96, 96); \
         ImageFunction.drawText(b, f, 20, 20, 'A', {PLAIN:#x}, 255, true, dfAlpha, \
                                0, 0, 0, 0, 0);"
    ));
    let colours = bitmap_colours(&env, first_bitmap);
    assert!(
        colours.contains(&[0x40, 0x30, 0x20]),
        "the drawn colour is missing from {colours:?}"
    );
    assert!(
        !colours.contains(&[0x20, 0x30, 0x40]),
        "the drawn colour has the red and the blue in the named order: {colours:?}"
    );
}

/// Reference: `ImageFunction.drawGlyph` hands the colour to the image as it
/// is (`reference/cpp/core/visual/ImageFunction.cpp:1159-1162`) and the image
/// reads the value's low bytes as the red, green and blue planes
/// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:836-903`), so the
/// glyph paints the red and the blue the other way round.
#[test]
fn image_function_draw_glyph_paints_the_swapped_channels() {
    let _lock = vm_lock();
    let env = Env::with_storage();
    let Some(name) = font_in_storage(&env) else {
        return;
    };
    let first_bitmap = env.scene.bitmaps.len();
    env.run(&format!(
        "var f = new Font('{name}', 40, 0xffffffff); \
         f.getTextWidth('A'); \
         var b = new Bitmap(96, 96); \
         ImageFunction.drawGlyph(b, 20, 20, 'A', {PLAIN:#x}, 255, true);"
    ));
    let colours = bitmap_colours(&env, first_bitmap);
    assert!(
        colours.contains(&[0x40, 0x30, 0x20]),
        "the drawn colour is missing from {colours:?}"
    );
    assert!(
        !colours.contains(&[0x20, 0x30, 0x40]),
        "the drawn colour has the red and the blue in the named order: {colours:?}"
    );
}

/// Reference: a colour whose high byte is set names a system colour. The
/// table holds the `0xBBGGRR` values of the system colours
/// (`reference/cpp/core/visual/environ/Application.cpp:874-941`) and the
/// resolution swaps the red and the blue bytes of the result
/// (`reference/cpp/core/visual/impl/LayerImpl.cpp:20-31`), so `clHighlight`
/// paints the `0x3399ff` of the table's `0xff9933`.
///
/// The opaque face with `holdAlpha` takes the reference's "main only"
/// branch, which fills the colour through `FillColor`
/// (`reference/cpp/core/visual/LayerIntf.cpp:4291-4296`). `FillColor` keeps
/// the destination's alpha byte and replaces the colour bits
/// (`reference/cpp/core/visual/LayerBitmapIntf.cpp:367-380`,
/// `reference/cpp/core/visual/gl/blend_functor_c.h:747-753`), so the filled
/// pixels of a fresh image carry the colour at alpha zero; the assertion
/// reads the pixels without treating the alpha byte as coverage.
#[test]
fn a_system_colour_paints_the_resolved_channels() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&format!(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize(2, 2); \
         l.type = ltOpaque; \
         l.holdAlpha = true; \
         l.fillRect(0, 0, 2, 2, {CL_HIGHLIGHT:#x});"
    ));
    assert_eq!(
        reported_rgb(&env, "l.getMainPixel(0,  0)"),
        [0x33, 0x99, 0xff]
    );
    assert_eq!(layer_pixel_rgb(&env, 0, 0), [0x33, 0x99, 0xff]);
    assert!(layer_pixel_colours(&env).contains(&[0x33, 0x99, 0xff]));
}
