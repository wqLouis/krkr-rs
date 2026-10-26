//! Integration tests for the pixel read/write semantics of `Bitmap` and
//! `Layer`.
//!
//! The reference keeps the colour planes and the mask plane apart. A pixel
//! setter writes the colour alone and holds the destination mask
//! (`reference/cpp/core/visual/BitmapIntf.cpp:60-66`,
//! `reference/cpp/core/visual/LayerIntf.cpp:2925-2946`), a colour getter
//! reports the colour without the mask
//! (`reference/cpp/core/visual/BitmapIntf.cpp:51-58`,
//! `reference/cpp/core/visual/LayerIntf.cpp:2917-2923`) and the mask is a
//! member of its own (`reference/cpp/core/visual/BitmapIntf.cpp:67-85`).
//!
//! The colour a pixel setter writes is not an `0xAARRGGBB` value: a colour
//! whose high byte is set names a system colour, and the reference resolves
//! it through its colour table before swapping the red and blue bytes
//! (`reference/cpp/core/visual/impl/LayerImpl.cpp:20-31`,
//! `reference/cpp/core/environ/Application.cpp:874-941`,
//! `reference/cpp/core/visual/impl/TVPColor.h:6-45`).

mod common;

use common::Env;
use common::vm_lock;

/// `clWindow` — the system colour identifier of the window background
/// (`reference/cpp/core/visual/impl/TVPColor.h:6-45`).
const CL_WINDOW: i64 = 0x8000_0005;

/// `clHighlight` — the system colour identifier of the selection
/// (`reference/cpp/core/visual/impl/TVPColor.h:6-45`).
const CL_HIGHLIGHT: i64 = 0x8000_000d;

/// `Bitmap.getPixel` reports the colour planes alone: the mask plane of an
/// opaque pixel is not part of the value.
///
/// `tTJSNI_Bitmap::GetPixel` masks the packed pixel down to its 24 colour
/// bits (`reference/cpp/core/visual/BitmapIntf.cpp:51-58`) and
/// `TVPFromActualColor` masks them once more
/// (`reference/cpp/core/visual/impl/LayerImpl.cpp:33-36`). The mask is a
/// member of its own (`reference/cpp/core/visual/BitmapIntf.cpp:67-75`).
#[test]
fn bitmap_get_pixel_reports_the_colour_without_the_mask() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var b = new Bitmap(1, 1); \
         b.setMaskPixel(0, 0, 255); \
         b.setPixel(0, 0, 0x123456);",
    );
    assert_eq!(env.eval_int("b.getPixel(0, 0)"), 0x12_3456);
    assert_eq!(env.eval_int("b.getMaskPixel(0, 0)"), 255);
}

/// `Bitmap.setPixel` writes the colour planes and holds the destination
/// mask: the alpha byte of the colour is not an alpha at all.
///
/// `tTJSNI_Bitmap::SetPixel` writes through `SetPointMain`, which keeps the
/// mask of the destination pixel
/// (`reference/cpp/core/visual/BitmapIntf.cpp:60-66`,
/// `reference/cpp/core/visual/LayerBitmapIntf.cpp:194-209`).
#[test]
fn bitmap_set_pixel_writes_the_colour_and_holds_the_mask() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var b = new Bitmap(1, 1); \
         b.setMaskPixel(0, 0, 128); \
         b.setPixel(0, 0, 0xff0000);",
    );
    // The red is written; the 0x80 mask is the one the destination had.
    assert_eq!(env.eval_int("b.getPixel(0, 0)"), 0xff_0000);
    assert_eq!(env.eval_int("b.getMaskPixel(0, 0)"), 128);
}

/// A colour whose high byte is set names a system colour: the reference
/// resolves it through its colour table and swaps the red and blue bytes.
///
/// `TVPToActualColor` (`reference/cpp/core/visual/impl/LayerImpl.cpp:20-31`)
/// calls `ColorToRGB` (`reference/cpp/core/environ/Application.cpp:874-941`),
/// whose table holds the `0xBBGGRR` values of the system colours
/// (`reference/cpp/core/visual/impl/TVPColor.h:6-45`), and swaps the red and
/// blue bytes of the result. An identifier the table does not name keeps its
/// low 24 bits.
#[test]
fn bitmap_set_pixel_resolves_a_system_colour() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var b = new Bitmap(1, 1);");
    // `clWindow` is the window background: white.
    env.run(&format!("b.setPixel(0, 0, {CL_WINDOW:#x});"));
    assert_eq!(env.eval_int("b.getPixel(0, 0)"), 0x00ff_ffff);
    // `clHighlight` is `0xff9933` in the table's `0xBBGGRR` order, so the
    // colour the pixel gets is `0x3399ff`.
    env.run(&format!("b.setPixel(0, 0, {CL_HIGHLIGHT:#x});"));
    assert_eq!(env.eval_int("b.getPixel(0, 0)"), 0x33_99ff);
    // An identifier the table does not name keeps its low 24 bits, with the
    // red and blue bytes swapped.
    env.run("b.setPixel(0, 0, 0x80112233);");
    assert_eq!(env.eval_int("b.getPixel(0, 0)"), 0x33_2211);
}

/// The mask plane is written and read through the mask members alone.
///
/// `tTJSNI_Bitmap::SetMaskPixel` writes the mask byte of the pixel
/// (`reference/cpp/core/visual/BitmapIntf.cpp:77-85`) and `GetMaskPixel`
/// reads it back (`reference/cpp/core/visual/BitmapIntf.cpp:67-75`).
#[test]
fn bitmap_mask_members_round_trip_the_alpha() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var b = new Bitmap(2, 2);");
    env.run("b.setMaskPixel(0, 0, 77); b.setMaskPixel(1, 1, 255);");
    assert_eq!(env.eval_int("b.getMaskPixel(0, 0)"), 77);
    assert_eq!(env.eval_int("b.getMaskPixel(1, 1)"), 255);
    // The mask plane is not part of the colour the colour member reports.
    assert_eq!(env.eval_int("b.getPixel(0, 0)"), 0);
}

/// `Layer.setMainPixel` writes the colour planes and holds the destination
/// mask, and `Layer.getMainPixel` reports the colour without the mask.
///
/// `tTJSNI_BaseLayer::SetMainPixel` writes through `SetPointMain`
/// (`reference/cpp/core/visual/LayerIntf.cpp:2925-2946`,
/// `reference/cpp/core/visual/LayerBitmapIntf.cpp:194-209`) and
/// `GetMainPixel` masks the packed pixel down to its 24 colour bits
/// (`reference/cpp/core/visual/LayerIntf.cpp:2917-2923`).
#[test]
fn layer_main_pixel_writes_the_colour_and_holds_the_mask() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setMaskPixel(0, 0, 128); \
         l.setMainPixel(0, 0, 0xff0000);",
    );
    // The red is written; the 0x80 mask is the one the destination had.
    assert_eq!(env.eval_int("l.getMainPixel(0, 0)"), 0xff_0000);
    assert_eq!(env.eval_int("l.getMaskPixel(0, 0)"), 128);
}

/// `Layer.setMainPixel` resolves a system colour the way `Bitmap.setPixel`
/// does.
///
/// `tTJSNI_BaseLayer::SetMainPixel` writes `TVPToActualColor(color)`
/// (`reference/cpp/core/visual/LayerIntf.cpp:2925-2946`), the same
/// conversion `tTJSNI_Bitmap::SetPixel` uses
/// (`reference/cpp/core/visual/BitmapIntf.cpp:60-66`).
#[test]
fn layer_set_main_pixel_resolves_a_system_colour() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var w = new Window(); var l = new Layer(w, null);");
    env.run(&format!("l.setMainPixel(0, 0, {CL_WINDOW:#x});"));
    assert_eq!(env.eval_int("l.getMainPixel(0, 0)"), 0x00ff_ffff);
    env.run(&format!("l.setMainPixel(0, 0, {CL_HIGHLIGHT:#x});"));
    assert_eq!(env.eval_int("l.getMainPixel(0, 0)"), 0x33_99ff);
    env.run("l.setMainPixel(0, 0, 0x80112233);");
    assert_eq!(env.eval_int("l.getMainPixel(0, 0)"), 0x33_2211);
}
