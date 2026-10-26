//! Integration tests for the per-channel image operations.
//!
//! The reference's image operations work on the packed pixel word. The word
//! is the image's RGBA planes read little-endian, so the red is in the low
//! byte, the green in the second and the blue in the third
//! (`reference/cpp/core/visual/LayerBitmapIntf.h:19-21` stores a colour in
//! that byte order, `reference/cpp/core/visual/impl/LayerImpl.cpp:21-31`
//! names the conversion, and `reference/cpp/core/visual/RenderManager.cpp:346-350`
//! reads the word back as the texture's RGBA planes). A weight the
//! reference applies to the word's low byte is therefore a weight on the red
//! plane, and one it applies to the third byte is a weight on the blue
//! plane.
//!
//! The tests drive the real script surface: they paint through the VM and
//! read the pixels the operation left behind.

mod common;

use common::Env;
use common::vm_lock;

/// The red, green and blue bytes the script's pixel getter reports.
///
/// The getter hands the colour back as the packed word of the RGBA planes,
/// read little-endian, so the red is in the low byte and the blue in the
/// third (`reference/cpp/core/visual/BitmapIntf.cpp:51-58`).
fn reported_rgb(env: &Env, expression: &str) -> [u8; 3] {
    let value = env.eval_int(expression) as u32;
    [value as u8, (value >> 8) as u8, (value >> 16) as u8]
}

/// The reference's grey of a colour, the way `TVPDoGrayScale` computes it:
/// the word's low byte (the red) weighs 19, the second byte (the green)
/// weighs 183 and the third byte (the blue) weighs 54, the sum is shifted
/// right by eight and written to all three colour bytes
/// (`reference/cpp/core/visual/tvpgl.cpp:10359-10365`).
fn reference_grey(red: u8, green: u8, blue: u8) -> u8 {
    ((19 * u32::from(red) + 183 * u32::from(green) + 54 * u32::from(blue)) >> 8) as u8
}

/// The grey of a colour whose red, green and blue differ, so a weight
/// that lands on the wrong channel shows.
#[test]
fn do_gray_scale_weighs_the_channels_as_the_reference_does() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var b = new Bitmap(1, 1); \
         b.setPixel(0, 0, 0x306090); \
         b.setMaskPixel(0, 0, 255);",
    );
    env.run("ImageFunction.doGrayScale(b, null);");
    // The reference's grey is `(19*R + 183*G + 54*B) >> 8`
    // (`reference/cpp/core/visual/tvpgl.cpp:10359-10365`), so
    // `(19*0x30 + 183*0x60 + 54*0x90) >> 8 == 0x66`. The shader spells the
    // same ramp as 0.0743 / 0.7148 / 0.2109
    // (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3970-3972`), and
    // 19/256 is 0.0743, 183/256 is 0.7148 and 54/256 is 0.2109. The member
    // itself is `reference/cpp/core/visual/ImageFunction.cpp:747-785`.
    assert_eq!(reported_rgb(&env, "b.getPixel(0, 0)"), [0x66, 0x66, 0x66]);
    assert_eq!(env.eval_int("b.getMaskPixel(0, 0)"), 255);
}

/// The red and the blue carry different weights, and the reference gives the
/// red the smaller one: the word's low byte is the red plane and it weighs
/// 19, the third byte is the blue plane and it weighs 54
/// (`reference/cpp/core/visual/tvpgl.cpp:10359-10365`).
#[test]
fn do_gray_scale_weighs_the_red_and_the_blue_as_the_reference_does() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var b = new Bitmap(2, 1); \
         b.setPixel(0, 0, 0xff0000); b.setMaskPixel(0, 0, 255); \
         b.setPixel(1, 0, 0x0000ff); b.setMaskPixel(1, 0, 255);",
    );
    env.run("ImageFunction.doGrayScale(b, null);");
    // Pure red is `(19*255) >> 8 == 0x12` and pure blue is
    // `(54*255) >> 8 == 0x35`
    // (`reference/cpp/core/visual/tvpgl.cpp:10359-10365`).
    assert_eq!(reported_rgb(&env, "b.getPixel(0, 0)"), [0x12, 0x12, 0x12]);
    assert_eq!(reported_rgb(&env, "b.getPixel(1, 0)"), [0x35, 0x35, 0x35]);
    assert_eq!(reference_grey(0xff, 0x00, 0x00), 0x12);
    assert_eq!(reference_grey(0x00, 0x00, 0xff), 0x35);
}

/// The layer's own `doGrayScale` reaches the same arithmetic as the
/// `ImageFunction` member.
#[test]
fn layer_do_gray_scale_weighs_the_channels_as_the_reference_does() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize(1, 1); \
         l.setMainPixel(0, 0, 0x306090); \
         l.doGrayScale();",
    );
    // `Layer.doGrayScale` is `reference/cpp/core/visual/LayerIntf.cpp:9616-9624`
    // and reaches the same grey as `TVPDoGrayScale`
    // (`reference/cpp/core/visual/tvpgl.cpp:10359-10365`).
    assert_eq!(
        reported_rgb(&env, "l.getMainPixel(0, 0)"),
        [0x66, 0x66, 0x66]
    );
}

/// The gamma ramp is a table per channel: the reference builds the red table
/// from the red gamma, the floor and the ceiling, and applies the red table
/// to the word's low byte, the green table to the second byte and the blue
/// table to the third
/// (`reference/cpp/core/visual/tvpgl.cpp:10405-10460` builds the tables,
/// `reference/cpp/core/visual/tvpgl.cpp:10473-10486` applies them). The
/// tables are `exp(log(i/255) * (1/gamma)) * (ceil - floor) + 0.5 + floor`,
/// truncated and clamped.
#[test]
fn adjust_gamma_uses_the_reference_channel_tables() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var b = new Bitmap(1, 1); \
         b.setPixel(0, 0, 0x4080c0); \
         b.setMaskPixel(0, 0, 255);",
    );
    env.run("ImageFunction.adjustGamma(b, 0.5, 0, 255, 1.0, 0, 255, 2.0, 0, 255, null, false);");
    // The red plane (0x40) takes the red table: `(0x40/255)^2 * 255 == 16`.
    // The green plane (0x80) takes the green table, which is the identity.
    // The blue plane (0xc0) takes the blue table:
    // `(0xc0/255)^0.5 * 255 == 221`
    // (`reference/cpp/core/visual/tvpgl.cpp:10417-10457`).
    // The member is `reference/cpp/core/visual/ImageFunction.cpp:611-683`.
    assert_eq!(reported_rgb(&env, "b.getPixel(0, 0)"), [16, 128, 221]);
    assert_eq!(env.eval_int("b.getMaskPixel(0, 0)"), 255);
}

/// The gamma pass leaves a fully transparent pixel alone: the reference
/// processes only the pixels that are not fully transparent
/// (`reference/cpp/core/visual/tvpgl.cpp:10473`).
#[test]
fn adjust_gamma_leaves_a_fully_transparent_pixel_alone() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var b = new Bitmap(1, 1); \
         b.setPixel(0, 0, 0x4080c0); \
         b.setMaskPixel(0, 0, 0);",
    );
    env.run("ImageFunction.adjustGamma(b, 0.5, 0, 255, 1.0, 0, 255, 2.0, 0, 255, null, false);");
    assert_eq!(reported_rgb(&env, "b.getPixel(0, 0)"), [0x40, 0x80, 0xc0]);
    assert_eq!(env.eval_int("b.getMaskPixel(0, 0)"), 0);
}

/// The box blur averages every colour channel over the kernel with the same
/// weights, so it cannot tell the channels apart: a red pixel and a blue
/// pixel blur to the same grey. The reference's blur sums the kernel's
/// samples and divides by their count
/// (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3991-4005`), and
/// the member is `reference/cpp/core/visual/ImageFunction.cpp:685-745`.
#[test]
fn do_box_blur_weighs_every_colour_channel_the_same() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var b = new Bitmap(3, 1); \
         b.setPixel(0, 0, 0xff0000); b.setMaskPixel(0, 0, 255); \
         b.setPixel(1, 0, 0x0000ff); b.setMaskPixel(1, 0, 255); \
         b.setPixel(2, 0, 0x000000); b.setMaskPixel(2, 0, 255);",
    );
    env.run("ImageFunction.doBoxBlur(b, 1, 0, null, true);");
    // The centre of a 1x3 horizontal kernel over (red, blue, black) is
    // ((255 + 0 + 0) / 3, (0 + 0 + 0) / 3, (0 + 255 + 0) / 3) == (85, 0, 85):
    // the red and the blue keep their own channel and neither is swapped
    // for the other.
    assert_eq!(reported_rgb(&env, "b.getPixel(1, 0)"), [85, 0, 85]);
}
