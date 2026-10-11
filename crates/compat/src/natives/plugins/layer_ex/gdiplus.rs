//! The `GdiPlus` namespace object published by `layerExDraw`.
//!
//! Reference: `reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:
//! 3463-3651`. `NCB_REGISTER_CLASS(GdiPlus)` publishes one global class
//! object carrying ~150 `ENUM` constants plus the nested `NCB_SUBCLASS`
//! classes `PointF`, `RectF`, `Image`, `Matrix`, `Font` (C++ `FontInfo`),
//! `Appearance` and (blend2d only) `Path`. `NCB_SUBCLASS` attaches a class
//! as a member of the enclosing class object and does not publish it
//! globally (`reference/cpp/core/plugin/ncbind.hpp:1792-1799,1963-1990`), so
//! the port registers each nested class through the always-global
//! registration ABI, aliases the class object into `GdiPlus`, then deletes
//! the registration global. `Image` and `Matrix` are not ported and stay
//! absent.
#![allow(unsafe_code)]

use tjs::{DetachedValue, NativeClassBuilder, RetainedValue, TjsValue};

use crate::{CompatCtx, CompatError};

/// Generate one read-only static property per GDI+ constant.
macro_rules! gdiplus_constants {
    ($($name:ident = $value:expr;)*) => {
        // The getter identifiers mirror the TJS member names, which the
        // reference spells in CamelCase (`ENUM(...)` in
        // `reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:3463`).
        #[allow(non_snake_case)]
        mod constants {
            use std::ffi::{c_char, c_void};

            use tjs::{NativePropertyDef, Value};

            use crate::native_util::set_int;

            $(
                extern "C" fn $name(
                    _engine: *mut c_void,
                    out: *mut Value,
                    _out_error: *mut *mut c_char,
                ) -> std::os::raw::c_int {
                    set_int(out, $value);
                    0
                }
            )*

            pub(super) fn properties() -> Vec<NativePropertyDef> {
                vec![
                    $(NativePropertyDef {
                        name: stringify!($name),
                        get: Some($name),
                        set: None,
                    },)*
                ]
            }
        }
    };
}

// The values match the blend2d enum definitions
// (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:65-262`),
// the backend the Linux/Android build uses
// (`reference/cpp/plugins/layerex_draw/CMakeLists.txt`).
gdiplus_constants! {
    Ok = 0;
    GenericError = 1;
    InvalidParameter = 2;
    OutOfMemory = 3;
    ObjectBusy = 4;
    InsufficientBuffer = 5;
    NotImplemented = 6;
    Win32Error = 7;
    WrongState = 8;
    Aborted = 9;
    FileNotFound = 10;
    ValueOverflow = 11;
    AccessDenied = 12;
    UnknownImageFormat = 13;
    FontFamilyNotFound = 14;
    FontStyleNotFound = 15;
    NotTrueTypeFont = 16;
    UnsupportedGdiplusVersion = 17;
    GdiplusNotInitialized = 18;
    PropertyNotFound = 19;
    PropertyNotSupported = 20;
    ProfileNotFound = 21;

    FontStyleRegular = 0;
    FontStyleBold = 1;
    FontStyleItalic = 2;
    FontStyleBoldItalic = 3;
    FontStyleUnderline = 4;
    FontStyleStrikeout = 8;

    BrushTypeSolidColor = 0;
    BrushTypeHatchFill = 1;
    BrushTypeTextureFill = 2;
    BrushTypePathGradient = 3;
    BrushTypeLinearGradient = 4;

    DashCapFlat = 0;
    DashCapRound = 2;
    DashCapTriangle = 3;

    DashStyleSolid = 0;
    DashStyleDash = 1;
    DashStyleDot = 2;
    DashStyleDashDot = 3;
    DashStyleDashDotDot = 4;

    HatchStyleHorizontal = 0;
    HatchStyleVertical = 1;
    HatchStyleForwardDiagonal = 2;
    HatchStyleBackwardDiagonal = 3;
    HatchStyleCross = 4;
    HatchStyleDiagonalCross = 5;
    HatchStyle05Percent = 6;
    HatchStyle10Percent = 7;
    HatchStyle20Percent = 8;
    HatchStyle25Percent = 9;
    HatchStyle30Percent = 10;
    HatchStyle40Percent = 11;
    HatchStyle50Percent = 12;
    HatchStyle60Percent = 13;
    HatchStyle70Percent = 14;
    HatchStyle75Percent = 15;
    HatchStyle80Percent = 16;
    HatchStyle90Percent = 17;
    HatchStyleLightDownwardDiagonal = 18;
    HatchStyleLightUpwardDiagonal = 19;
    HatchStyleDarkDownwardDiagonal = 20;
    HatchStyleDarkUpwardDiagonal = 21;
    HatchStyleWideDownwardDiagonal = 22;
    HatchStyleWideUpwardDiagonal = 23;
    HatchStyleLightVertical = 24;
    HatchStyleLightHorizontal = 25;
    HatchStyleNarrowVertical = 26;
    HatchStyleNarrowHorizontal = 27;
    HatchStyleDarkVertical = 28;
    HatchStyleDarkHorizontal = 29;
    HatchStyleDashedDownwardDiagonal = 30;
    HatchStyleDashedUpwardDiagonal = 31;
    HatchStyleDashedHorizontal = 32;
    HatchStyleDashedVertical = 33;
    HatchStyleSmallConfetti = 34;
    HatchStyleLargeConfetti = 35;
    HatchStyleZigZag = 36;
    HatchStyleWave = 37;
    HatchStyleDiagonalBrick = 38;
    HatchStyleHorizontalBrick = 39;
    HatchStyleWeave = 40;
    HatchStylePlaid = 41;
    HatchStyleDivot = 42;
    HatchStyleDottedGrid = 43;
    HatchStyleDottedDiamond = 44;
    HatchStyleShingle = 45;
    HatchStyleTrellis = 46;
    HatchStyleSphere = 47;
    HatchStyleSmallGrid = 48;
    HatchStyleSmallCheckerBoard = 49;
    HatchStyleLargeCheckerBoard = 50;
    HatchStyleOutlinedDiamond = 51;
    HatchStyleSolidDiamond = 52;
    HatchStyleTotal = 53;
    HatchStyleLargeGrid = 4;
    HatchStyleMin = 0;
    HatchStyleMax = 52;

    LinearGradientModeHorizontal = 0;
    LinearGradientModeVertical = 1;
    LinearGradientModeForwardDiagonal = 2;
    LinearGradientModeBackwardDiagonal = 3;

    LineCapFlat = 0;
    LineCapSquare = 1;
    LineCapRound = 2;
    LineCapTriangle = 3;
    LineCapNoAnchor = 16;
    LineCapSquareAnchor = 17;
    LineCapRoundAnchor = 18;
    LineCapDiamondAnchor = 19;
    LineCapArrowAnchor = 20;

    LineJoinMiter = 0;
    LineJoinBevel = 1;
    LineJoinRound = 2;
    LineJoinMiterClipped = 3;

    PenAlignmentCenter = 0;
    PenAlignmentInset = 1;

    WrapModeTile = 0;
    WrapModeTileFlipX = 1;
    WrapModeTileFlipY = 2;
    WrapModeTileFlipXY = 3;
    WrapModeClamp = 4;

    MatrixOrderPrepend = 0;
    MatrixOrderAppend = 1;

    ImageTypeUnknown = 0;
    ImageTypeBitmap = 1;
    ImageTypeMetafile = 2;

    RotateNoneFlipNone = 0;
    Rotate90FlipNone = 1;
    Rotate180FlipNone = 2;
    Rotate270FlipNone = 3;
    RotateNoneFlipX = 4;
    Rotate90FlipX = 5;
    Rotate180FlipX = 6;
    Rotate270FlipX = 7;
    RotateNoneFlipY = 6;
    Rotate90FlipY = 7;
    Rotate180FlipY = 4;
    Rotate270FlipY = 5;
    RotateNoneFlipXY = 2;
    Rotate90FlipXY = 3;
    Rotate180FlipXY = 0;
    Rotate270FlipXY = 1;

    SmoothingModeInvalid = -1;
    SmoothingModeDefault = 0;
    SmoothingModeHighSpeed = 1;
    SmoothingModeHighQuality = 2;
    SmoothingModeNone = 3;
    SmoothingModeAntiAlias = 4;

    TextRenderingHintSystemDefault = 0;
    TextRenderingHintSingleBitPerPixelGridFit = 1;
    TextRenderingHintSingleBitPerPixel = 2;
    TextRenderingHintAntiAliasGridFit = 3;
    TextRenderingHintAntiAlias = 4;
    TextRenderingHintClearTypeGridFit = 5;
}

/// Attach `GdiPlus.<public>` = the global class object `internal`.
fn attach_named(
    ctx: &CompatCtx,
    public: &str,
    internal: &str,
    gdiplus_id: tjs::Tjs2ValueId,
) -> Result<bool, String> {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let engine = unsafe { ctx.engine() };
    let Ok(RetainedValue::Object(class)) = engine.eval_retained(internal, "layer_ex") else {
        return Ok(false);
    };
    let id = class.raw_id() as usize as u64;
    engine
        .set_member(gdiplus_id, public, &TjsValue::Retained(id))
        .map_err(|error| error.to_string())?;
    drop(class);
    Ok(true)
}

/// Register the `GdiPlus` class object, its enum constants and its nested
/// classes, and remove the bare registration globals.
pub(super) fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_class(&NativeClassBuilder {
        name: "GdiPlus",
        methods: vec![],
        properties: constants::properties(),
    })?;

    // The nested classes are ordinary instance classes in the port; the
    // registration ABI always publishes a global that the reference does not.
    super::geom::register_classes(ctx)?;
    super::path::register_class(ctx)?;
    super::appearance::register_class(ctx)?;
    super::font::register_class(ctx)?;

    let (gdiplus_handle, global_handle) = {
        // SAFETY: a `CompatCtx` is only created for a bound, live engine.
        let engine = unsafe { ctx.engine() };
        let gdiplus = match engine.eval_retained("GdiPlus", "layer_ex") {
            Ok(RetainedValue::Object(object)) => Some(object),
            _ => None,
        };
        let global = match engine.eval_retained("global", "layer_ex") {
            Ok(RetainedValue::Object(object)) => Some(object),
            _ => None,
        };
        (gdiplus, global)
    };
    let gdiplus_id = gdiplus_handle
        .as_ref()
        .map(DetachedValue::raw_id)
        .unwrap_or(std::ptr::null_mut());
    let global_id = global_handle
        .as_ref()
        .map(DetachedValue::raw_id)
        .unwrap_or(std::ptr::null_mut());

    // (public `GdiPlus.*` name, registration class name, keeps a global).
    // `Font` is registered as `FontInfo` because that is the C++ class name
    // (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:3648`).
    for (public, internal, keeps_global) in [
        ("PointF", "PointF", false),
        ("RectF", "RectF", false),
        ("Path", "Path", false),
        ("Appearance", "Appearance", false),
        ("Font", "FontInfo", false),
    ] {
        if gdiplus_id.is_null() {
            continue;
        }
        let attached =
            attach_named(ctx, public, internal, gdiplus_id).map_err(CompatError::Register)?;
        // The global exists only because the registration ABI always
        // publishes one; the reference exposes these classes solely under
        // `GdiPlus`. `delete_member`'s absent-member error is harmless: it can
        // only occur when an earlier install already removed the global.
        if attached && !keeps_global && !global_id.is_null() {
            let _ = ctx.delete_member(global_id, internal);
        }
    }
    Ok(())
}
