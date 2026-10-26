//! Integration tests for the system-colour resolution in the draw paths.
//!
//! A colour whose high byte is set names a system colour: the reference
//! resolves it through its colour table and swaps the red and blue bytes of
//! the result (`reference/cpp/core/visual/impl/LayerImpl.cpp:20-31`,
//! `reference/cpp/core/environ/Application.cpp:874-941`). Which draw path
//! resolves the colour is decided per branch: the faces that paint the
//! colour resolve the whole colour, while the fill that copies the colour as
//! it is keeps the `0xAARRGGBB` meaning, so its high byte stays the alpha
//! (`reference/cpp/core/visual/LayerIntf.cpp:4277-4295`, `:4348-4382`,
//! `reference/cpp/core/visual/ImageFunction.cpp:836-845`, `:917-945`).
//!
//! The tests drive the real script surface: they create layers and bitmaps
//! through the VM and read the pixels the operations leave behind.
//!
//! A pixel getter reports the packed word of the RGBA planes, read
//! little-endian, so the red is in the low byte and the blue in the third
//! (`reference/cpp/core/visual/BitmapIntf.cpp:51-58`,
//! `reference/cpp/core/visual/LayerIntf.cpp:2917-2923`,
//! `reference/cpp/core/visual/RenderManager.cpp:346-350`).

mod common;

use std::path::Path;

use common::Env;
use common::vm_lock;

/// `clWindow` — the system colour identifier of the window background
/// (`reference/cpp/core/visual/impl/TVPColor.h:6-45`).
const CL_WINDOW: i64 = 0x8000_0005;

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

/// The colours the covered pixels of the last layer's main image carry.
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

/// The colours the covered pixels of the bitmap at `index` carry.
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

/// Reference: `FillRect` resolves the whole colour on the opaque face that
/// holds the destination alpha
/// (`reference/cpp/core/visual/LayerIntf.cpp:4288-4295`): the colour goes
/// through the system-colour table, so an identifier names its table entry
/// and the red and blue bytes are swapped. A plain `0x00RRGGBB` colour is
/// not a system colour and passes through unchanged.
#[test]
fn layer_fill_rect_on_the_opaque_face_resolves_a_system_colour() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&format!(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize(2, 2); \
         l.type = ltOpaque; \
         l.holdAlpha = true; \
         l.fillRect(0, 0, 1, 1, {CL_WINDOW:#x}); \
         var window_color = l.getMainPixel(0, 0); \
         l.fillRect(1, 0, 1, 1, {CL_HIGHLIGHT:#x}); \
         var highlight_color = l.getMainPixel(1, 0); \
         l.fillRect(0, 1, 1, 1, 0x00112233); \
         var plain_color = l.getMainPixel(0, 1);",
    ));
    // `clWindow` is white; `clHighlight` is the table's `0xff9933` in the
    // table's `0xBBGGRR` order, which the swap turns into `0x3399ff`.
    // The reported word holds the red in its low byte
    // (`reference/cpp/core/visual/RenderManager.cpp:346-350`).
    assert_eq!(env.eval_int("window_color"), 0x00ff_ffff);
    assert_eq!(env.eval_int("highlight_color"), 0x00ff_9933);
    // A plain colour is not a system colour and passes through unchanged.
    assert_eq!(env.eval_int("plain_color"), 0x33_2211);
}

/// Reference: `FillRect` keeps the `0xAARRGGBB` meaning on the alpha and
/// the additive-alpha face (`reference/cpp/core/visual/LayerIntf.cpp:4280-4287`):
/// only the low 24 bits go through the table, so the high byte stays the
/// alpha and a colour whose high byte is set names no system colour here.
#[test]
fn layer_fill_rect_on_the_alpha_face_keeps_the_argb_colour() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&format!(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize(1, 1); \
         l.type = ltAlpha; \
         l.fillRect(0, 0, 1, 1, {CL_WINDOW:#x}); \
         var color = l.getMainPixel(0, 0); \
         var alpha = l.getMaskPixel(0, 0);",
    ));
    // The high byte is the alpha and the low 24 bits are the colour. The
    // reported word holds the red in its low byte.
    assert_eq!(env.eval_int("color"), 0x05_0000);
    assert_eq!(env.eval_int("alpha"), 0x80);
}

/// Reference: `ColorRect` resolves the whole colour on the faces that paint
/// it — the alpha face (`reference/cpp/core/visual/LayerIntf.cpp:4348-4357`),
/// the additive-alpha face (`:4360-4369`) and the opaque face
/// (`:4373-4382`).
#[test]
fn layer_color_rect_resolves_a_system_colour_on_every_painting_face() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&format!(
        "var w = new Window(); \
         var root = new Layer(w, null); root.setImageSize(1, 1); \
         var alpha = new Layer(w, root); alpha.setImageSize(1, 1); \
         alpha.type = ltAlpha; \
         alpha.colorRect(0, 0, 1, 1, {CL_WINDOW:#x}, 255); \
         var alpha_color = alpha.getMainPixel(0, 0); \
         var additive = new Layer(w, root); additive.setImageSize(1, 1); \
         additive.type = ltAddAlpha; \
         additive.colorRect(0, 0, 1, 1, {CL_HIGHLIGHT:#x}, 255); \
         var additive_color = additive.getMainPixel(0, 0); \
         var opaque = new Layer(w, root); opaque.setImageSize(1, 1); \
         opaque.type = ltOpaque; \
         opaque.colorRect(0, 0, 1, 1, {CL_HIGHLIGHT:#x}, 255); \
         var opaque_color = opaque.getMainPixel(0, 0);",
    ));
    // The alpha face writes the resolved colour and the fill's alpha.
    assert_eq!(env.eval_int("alpha_color"), 0x00ff_ffff);
    // The additive-alpha face writes the resolved colour in the additive
    // representation, where each channel is scaled by the fill's opacity.
    assert_eq!(env.eval_int("additive_color"), 0xfe_9832);
    // The opaque face writes the resolved colour and holds the alpha.
    assert_eq!(env.eval_int("opaque_color"), 0x00ff_9933);
}

/// Reference: the mask face of `ColorRect` takes the low byte of the colour
/// as the mask value and never resolves the colour
/// (`reference/cpp/core/visual/LayerIntf.cpp:4384-4390`).
#[test]
fn layer_color_rect_on_the_mask_face_keeps_the_low_byte() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&format!(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize(1, 1); \
         l.setMainPixel(0, 0, 0x102030); \
         l.face = dfMask; \
         l.colorRect(0, 0, 1, 1, {CL_HIGHLIGHT:#x}); \
         var color = l.getMainPixel(0, 0); \
         var mask = l.getMaskPixel(0, 0);",
    ));
    // The colour planes are untouched and the mask is the low byte. The
    // reported word holds the red in its low byte.
    assert_eq!(env.eval_int("color"), 0x30_2010);
    assert_eq!(env.eval_int("mask"), 0x0d);
}

/// Reference: `DrawText` resolves the main colour and passes the shadow
/// colour as it is (`reference/cpp/core/visual/LayerIntf.cpp:4463-4470`):
/// the shadow is drawn from the raw colour, so its high byte is no system
/// colour identifier there.
#[test]
fn layer_draw_text_resolves_the_main_colour_and_keeps_the_shadow_raw() {
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
         l.drawText(20, 20, 'A', {CL_WINDOW:#x}, 255, true, 255, {CL_HIGHLIGHT:#x}, 0, 30, 0);"
    ));
    let colours = layer_colours(&env);
    // The main colour is resolved: `clWindow` is white.
    assert!(
        colours.contains(&[0xff, 0xff, 0xff]),
        "the resolved main colour is missing from {colours:?}"
    );
    // The shadow colour is not resolved: the raw colour's low 24 bits are
    // the colour and the high byte is no identifier, so the shadow is the
    // near-black `0x00000d`.
    assert!(
        colours.contains(&[0x00, 0x00, 0x0d]),
        "the raw shadow colour is missing from {colours:?}"
    );
    // The resolved `clHighlight` would be `0x3399ff`; it is not there.
    assert!(
        !colours.contains(&[0x33, 0x99, 0xff]),
        "the shadow colour was resolved: {colours:?}"
    );
}

/// Reference: `DrawGlyph` resolves the main colour the way `DrawText` does
/// (`reference/cpp/core/visual/LayerIntf.cpp:4511-4517`), but it hands the
/// colour to the image as it is (`reference/cpp/core/visual/LayerIntf.cpp:4513-4516`)
/// and the image reads the value's low bytes as the red, green and blue
/// planes (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:836-903`), so
/// the red and the blue of the drawn colour are the other way round.
#[test]
fn layer_draw_glyph_resolves_the_main_colour() {
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
         l.drawGlyph(20, 20, 'A', {CL_HIGHLIGHT:#x}, 255, true);"
    ));
    let colours = layer_colours(&env);
    // `clHighlight` is the table's `0xff9933` in the table's `0xBBGGRR`
    // order, which the swap turns into `0x3399ff`; the glyph path paints
    // the red and the blue the other way round.
    assert!(
        colours.contains(&[0xff, 0x99, 0x33]),
        "the resolved main colour is missing from {colours:?}"
    );
}

/// Reference: `ImageFunction.fillRect` resolves the whole colour when it
/// fills without the alpha (`reference/cpp/core/visual/ImageFunction.cpp:841-845`):
/// the colour goes through the system-colour table and the destination alpha
/// is held.
#[test]
fn image_function_fill_rect_without_the_alpha_resolves_a_system_colour() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&format!(
        "var b = new Bitmap(2, 1); \
         b.setPixel(0, 0, 0x00112233); \
         b.setMaskPixel(0, 0, 0x80); \
         ImageFunction.fillRect(b, {CL_WINDOW:#x}, null, false, null); \
         var window_color = b.getPixel(0, 0); \
         var window_alpha = b.getMaskPixel(0, 0); \
         ImageFunction.fillRect(b, {CL_HIGHLIGHT:#x}, new Rect(1, 0, 2, 1), false, null); \
         var highlight_color = b.getPixel(1, 0);",
    ));
    assert_eq!(env.eval_int("window_color"), 0x00ff_ffff);
    // The destination alpha is held.
    assert_eq!(env.eval_int("window_alpha"), 0x80);
    assert_eq!(env.eval_int("highlight_color"), 0x00ff_9933);
}

/// Reference: `ImageFunction.fillRect` with `isalpha = true` copies the
/// colour as it is (`reference/cpp/core/visual/ImageFunction.cpp:838-841`):
/// only the low 24 bits go through the table, so the high byte stays the
/// alpha and a colour whose high byte is set names no system colour here.
#[test]
fn image_function_fill_rect_with_the_alpha_keeps_the_argb_colour() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&format!(
        "var b = new Bitmap(1, 1); \
         ImageFunction.fillRect(b, {CL_WINDOW:#x}, null, true, null); \
         var color = b.getPixel(0, 0); \
         var alpha = b.getMaskPixel(0, 0);",
    ));
    assert_eq!(env.eval_int("color"), 0x05_0000);
    assert_eq!(env.eval_int("alpha"), 0x80);
}

/// Reference: `ImageFunction.colorRect` resolves the whole colour on the
/// faces that paint it — the alpha face
/// (`reference/cpp/core/visual/ImageFunction.cpp:919-928`), the
/// additive-alpha face (`:930-936`) and the opaque face (`:937-943`).
#[test]
fn image_function_color_rect_resolves_a_system_colour_on_every_painting_face() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(&format!(
        "var alpha = new Bitmap(1, 1); \
         ImageFunction.colorRect(alpha, {CL_WINDOW:#x}, 255, null, dfAlpha, null); \
         var alpha_color = alpha.getPixel(0, 0); \
         var additive = new Bitmap(1, 1); \
         ImageFunction.colorRect(additive, {CL_HIGHLIGHT:#x}, 255, null, dfAddAlpha, null); \
         var additive_color = additive.getPixel(0, 0); \
         var opaque = new Bitmap(1, 1); \
         opaque.setMaskPixel(0, 0, 0x80); \
         ImageFunction.colorRect(opaque, {CL_HIGHLIGHT:#x}, 255, null, dfOpaque, null); \
         var opaque_color = opaque.getPixel(0, 0); \
         var opaque_alpha = opaque.getMaskPixel(0, 0);",
    ));
    assert_eq!(env.eval_int("alpha_color"), 0x00ff_ffff);
    // The additive-alpha face writes the resolved colour in the additive
    // representation, where each channel is scaled by the fill's opacity.
    assert_eq!(env.eval_int("additive_color"), 0xfe_9832);
    assert_eq!(env.eval_int("opaque_color"), 0x00ff_9933);
    // The opaque face holds the destination alpha.
    assert_eq!(env.eval_int("opaque_alpha"), 0x80);
}

/// Reference: `ImageFunction.drawText` resolves the main colour and passes
/// the shadow colour as it is (`reference/cpp/core/visual/ImageFunction.cpp:1057-1070`).
#[test]
fn image_function_draw_text_resolves_the_main_colour_and_keeps_the_shadow_raw() {
    let _lock = vm_lock();
    let env = Env::with_storage();
    let Some(name) = font_in_storage(&env) else {
        return;
    };
    let first_bitmap = env.scene.bitmaps.len();
    env.run(&format!(
        "var f = new Font('{name}', 40, 0xffffffff); \
         var b = new Bitmap(96, 96); \
         ImageFunction.drawText(b, f, 20, 20, 'A', {CL_WINDOW:#x}, 255, true, dfAlpha, \
                                255, {CL_HIGHLIGHT:#x}, 0, 30, 0);"
    ));
    let colours = bitmap_colours(&env, first_bitmap);
    assert!(
        colours.contains(&[0xff, 0xff, 0xff]),
        "the resolved main colour is missing from {colours:?}"
    );
    // The text path hands the shadow colour to the image as it is
    // (`reference/cpp/core/visual/ImageFunction.cpp:1060-1063`), and the image
    // reads the value's low bytes as the red, green and blue planes
    // (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:836-903`), so the
    // shadow's low byte is the red.
    assert!(
        colours.contains(&[0x0d, 0x00, 0x00]),
        "the raw shadow colour is missing from {colours:?}"
    );
    assert!(
        !colours.contains(&[0x33, 0x99, 0xff]),
        "the shadow colour was resolved: {colours:?}"
    );
}

/// Reference: `ImageFunction.drawGlyph` resolves the main colour the way
/// `drawText` does (`reference/cpp/core/visual/ImageFunction.cpp:1159-1162`),
/// but it hands the colour to the image as it is
/// (`reference/cpp/core/visual/ImageFunction.cpp:1161`), and the image reads
/// the value's low bytes as the red, green and blue planes
/// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:836-903`), so the red
/// and the blue of the drawn colour are the other way round.
#[test]
fn image_function_draw_glyph_resolves_the_main_colour() {
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
         ImageFunction.drawGlyph(b, 20, 20, 'A', {CL_HIGHLIGHT:#x}, 255, true);"
    ));
    let colours = bitmap_colours(&env, first_bitmap);
    assert!(
        colours.contains(&[0xff, 0x99, 0x33]),
        "the resolved main colour is missing from {colours:?}"
    );
}

/// Reference: the additive-alpha face of `Layer.colorRect` raises
/// `TVPNegativeOpacityNotSupportedOnThisFace` for a negative opacity
/// (`reference/cpp/core/visual/LayerIntf.cpp:4369-4372`,
/// `reference/cpp/core/base/MsgIntfInc.h:85-86`), while the same face of
/// `ImageFunction.colorRect` has no `else` and does nothing
/// (`reference/cpp/core/visual/ImageFunction.cpp:930-936`).
#[test]
fn a_negative_opacity_on_the_add_alpha_face_raises_for_the_layer_only() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize(1, 1); \
         l.type = ltAddAlpha; \
         var b = new Bitmap(1, 1);",
    );
    let message = env.error(&format!("l.colorRect(0, 0, 1, 1, {CL_WINDOW:#x}, -1);"));
    assert!(
        message.contains("Negative opacity not supported on this face"),
        "unexpected message: {message}"
    );
    // The image function's member does nothing instead of raising.
    env.run(&format!(
        "ImageFunction.colorRect(b, {CL_WINDOW:#x}, -1, null, dfAddAlpha, null);"
    ));
    assert_eq!(env.eval_int("b.getPixel(0, 0)"), 0);
    assert_eq!(env.eval_int("b.getMaskPixel(0, 0)"), 0);
}
