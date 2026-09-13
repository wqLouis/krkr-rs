//! Minimal `GdiPlus` plugin natives.
//!
//! The game builds its ADV `MessageFrame`, `MessageArea`, `SelectItem`,
//! `Album`, `EnvEffectRain` and `DrawFrameCurve` appearances with
//! `new GdiPlus.Appearance()` and then calls `addBrush` / `addPen` /
//! `clear`. Without a `GdiPlus` member, `MessageFrame.tjs:1925` throws
//! `Member "GdiPlus" does not exist` while `setScene(SCENE_ADV)` constructs
//! the frame, so `game.getScene(3)` never becomes valid and the scenario
//! never starts.
//!
//! The real plugin (`GdiPlus.dll`) draws through Windows GDI+; krkr-rs
//! renders the visual model from the `Layer`/`Bitmap` natives, so the
//! appearance only needs to be a harmless, argument-tolerant object. This
//! module therefore registers a native `Appearance` instance class whose
//! brush/pen methods are no-ops, and builds the `GdiPlus` namespace object
//! that the scripts actually reference.
//!
//! # Game-side inventory (verified against `system/*.tjs`)
//!
//! The game scripts reference exactly these members:
//!
//! * `GdiPlus.Appearance` — constructed 10 times (MessageFrame, MessageArea,
//!   SelectItem, Album ×3, EnvEffect, ADVScreen, Utility).
//! * `GdiPlus.BrushTypeHatchFill` — hatch brush selector (`ADVScreen.tjs`).
//! * `GdiPlus.HatchStyleDiagonalBrick` — hatch style (`ADVScreen.tjs`).
//!
//! Methods called on the constructed appearances: `addBrush`, `addPen`,
//! `clear`. `addBrush` is called with 2–4 arguments including a dictionary
//! (`%[type:..., hatchStyle:..., foreColor:..., backColor:...]`); every
//! shape is accepted and ignored.
//!
//! The appearances are also passed to `Layer` drawing calls
//! (`drawPolygon`/`drawArc`/`drawLine`/`drawBeziers`/`drawRectangle`) and to
//! the `invalidate` operator. Those paths live in `tvp-visual` and already
//! accept opaque objects (the drawing methods are no-ops and the native
//! instance's `Invalidate` is a no-op), so no `tvp-visual` change is needed.

use std::ffi::{c_char, c_int, c_void};

use tjs2_sys::{NativeInstanceBuilder, NativeInstanceMethodDef, Tjs2Engine, Value};

use crate::set_void_out;

/// The internal global class name. The game only ever reaches `Appearance`
/// through `GdiPlus.Appearance`, so the class is registered under a unique
/// name and re-exported as a member of the namespace (avoids squatting on a
/// generic bare global).
const APPEARANCE_CLASS: &str = "GdiPlusAppearance";

/// `GdiPlus.BrushTypeHatchFill` — GDI+ `BrushType` enum value.
const BRUSH_TYPE_HATCH_FILL: i64 = 1;
/// `GdiPlus.HatchStyleDiagonalBrick` — GDI+ `HatchStyle` enum value.
const HATCH_STYLE_DIAGONAL_BRICK: i64 = 38;

/// Opaque payload for one `GdiPlus.Appearance`. The paint model is tracked by
/// the visual natives, so the appearance carries no state of its own.
struct AppearanceInst;

extern "C" fn appearance_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(AppearanceInst)) as *mut c_void
}

extern "C" fn appearance_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if !instance.is_null() {
        // SAFETY: the pointer came from appearance_create's Box::into_raw.
        drop(unsafe { Box::from_raw(instance as *mut AppearanceInst) });
    }
}

/// Constructor hook plus the no-op `addBrush` / `addPen` / `clear` methods.
/// All accept any number of arguments and return void, so both the integer
/// color form (`addBrush(0x7ff00000, 0, 0, 0)`) and the dictionary hatch
/// form (`addBrush(%[type:...], 0, 0)`) work.
extern "C" fn appearance_noop(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void_out(out);
    0
}

/// Register `GdiPlus.Appearance` and the `GdiPlus` namespace object.
///
/// `GdiPlus` is created as a script dictionary referencing the native class
/// plus the constant members, mirroring how the reference plugin exposes a
/// namespace object with nested classes. Registered once per engine from
/// [`crate::register_all`].
pub fn register_gdiplus(engine: &Tjs2Engine) -> Result<(), String> {
    engine.register_native_class_instance(&NativeInstanceBuilder {
        name: APPEARANCE_CLASS,
        create: appearance_create,
        destroy: appearance_destroy,
        methods: vec![
            NativeInstanceMethodDef {
                name: APPEARANCE_CLASS, // class-name constructor hook
                f: appearance_noop,
            },
            NativeInstanceMethodDef {
                name: "addBrush",
                f: appearance_noop,
            },
            NativeInstanceMethodDef {
                name: "addPen",
                f: appearance_noop,
            },
            NativeInstanceMethodDef {
                name: "clear",
                f: appearance_noop,
            },
        ],
        properties: vec![],
    })?;

    // Expose the namespace the scripts use. The constants are plain integer
    // members (the reference exposes them as static class constants).
    let namespace = format!(
        "global.GdiPlus = %[Appearance: {APPEARANCE_CLASS}, \
         BrushTypeHatchFill: {BRUSH_TYPE_HATCH_FILL}, \
         HatchStyleDiagonalBrick: {HATCH_STYLE_DIAGONAL_BRICK}];"
    );
    engine
        .exec_script(&namespace, "GdiPlus_namespace")
        .map_err(|e| format!("failed to build GdiPlus namespace: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::register_all;
    use crate::test_lock::vm_lock;
    use tjs2_sys::{Tjs2Engine, TjsValue};

    /// Reproduces the exact call shapes from the game scripts:
    /// `system/MessageFrame.tjs:1925`, `system/MessageArea.tjs:857`,
    /// `system/SelectItem.tjs:1915`, `system/Album.tjs`,
    /// `system/EnveEffect.tjs`, `system/Utility.tjs`, and the hatch brush in
    /// `system/ADVScreen.tjs:3032`.
    #[test]
    fn gdiplus_appearance_constructs_and_exposes_constants() {
        let _lock = vm_lock();
        let engine = Tjs2Engine::new().unwrap();
        register_all(&engine).unwrap();

        // The constants must be integers before anything reads them.
        assert_eq!(
            engine
                .eval("GdiPlus.BrushTypeHatchFill", "gdiplus")
                .unwrap(),
            TjsValue::Integer(1)
        );
        assert_eq!(
            engine
                .eval("GdiPlus.HatchStyleDiagonalBrick", "gdiplus")
                .unwrap(),
            TjsValue::Integer(38)
        );

        // MessageFrame / MessageArea / SelectItem / Album / Utility shape.
        engine
            .exec_script(
                "var app = new GdiPlus.Appearance(); \
                 app.addBrush(0x7ff00000, 0, 0, 0); \
                 app.addPen(0xffffffff, 2, 0, 0); \
                 app.clear(); \
                 app.addBrush(0xff000000, 0, 0); \
                 app.addPen(0xff000000, 2, 0, 0, 0);",
                "gdiplus",
            )
            .unwrap();

        // ADVScreen hatch-brush dictionary form (reads both constants).
        engine
            .exec_script(
                "app.addBrush(%[type: GdiPlus.BrushTypeHatchFill, \
                 hatchStyle: GdiPlus.HatchStyleDiagonalBrick, \
                 foreColor: 0, backColor: 0x203040], 0, 0);",
                "gdiplus",
            )
            .unwrap();

        // `clear` returns void and the object survives `invalidate`.
        assert_eq!(
            engine.eval("app.clear()", "gdiplus").unwrap(),
            TjsValue::Void
        );
        engine.exec_script("invalidate app;", "gdiplus").unwrap();

        // The constructor tolerates extra arguments too.
        engine
            .exec_script("var app2 = new GdiPlus.Appearance(1, 'x');", "gdiplus")
            .unwrap();
    }

    /// The exact failing line from `MessageFrame.tjs:1925` must no longer
    /// throw `Member "GdiPlus" does not exist`.
    #[test]
    fn message_frame_gdiplus_construction_no_longer_throws() {
        let _lock = vm_lock();
        let engine = Tjs2Engine::new().unwrap();
        register_all(&engine).unwrap();
        let result = engine.exec_script(
            "var app = new GdiPlus.Appearance(); app.addBrush(0x7ff00000, 0, 0, 0);",
            "MessageFrame.tjs",
        );
        assert!(result.is_ok(), "GdiPlus construction failed: {result:?}");
    }
}
