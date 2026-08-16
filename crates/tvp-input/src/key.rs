//! `Key` native class — port of the reference `Key` class (in the full
//! krkrz tree: `core/base/KeyIntf.cpp`, `TVPCreateNativeClass_Key`; not
//! part of this repository's reference subset).
//!
//! A static class: `Key.getPressed(code)` / `getReleased(code)` /
//! `getRepeat(code)` query the shared [`crate::InputState`] (see the crate
//! docs for the frame protocol), and the `kXXX` constants are get-only
//! properties carrying the Windows virtual-key codes.
//!
//! The constant table is ported from `reference/cpp/core/environ/vkdefine.h`
//! (the `VK_*` codes this engine uses everywhere, e.g. in
//! `System.getKeyState`) under the standard krkrz `Key` constant names;
//! the browser/media/launch/OEM keys come from the standard Windows
//! virtual-key table (they are not in the reference's `vkdefine.h` but are
//! part of the krkrz `Key` class).

use std::ffi::{c_char, c_int, c_void};

use tjs2_sys::{NativeClassBuilder, NativeMethodDef, NativePropertyDef, Tjs2Engine, Value};

use crate::{args, report_error, set_int_out, value_as_i64, with_state};

/// Declare the `Key.kXXX` constant table: each entry is
/// `($rname, "kName", value)` — `$rname` becomes the getter fn name,
/// `"kName"` the registered property name, `value` the Windows virtual-key
/// code. Generates [`KEY_CODE_TABLE`], the [`key_code`] lookup and the
/// property definitions from one source of truth, so the table and the
/// registered properties cannot drift.
macro_rules! key_code_table {
    ($(($rname:ident, $tjs:literal, $code:expr)),* $(,)?) => {
        /// The `Key` class key-code constants: TJS constant name → Windows
        /// virtual-key code, in declaration order (port of the reference
        /// `KeyIntf.cpp` table; codes from `environ/vkdefine.h` / the
        /// standard Windows virtual-key table).
        pub const KEY_CODE_TABLE: &[(&str, u32)] = &[ $(($tjs, $code)),* ];

        /// Look up the Windows virtual-key code for a TJS `Key.kXXX`
        /// constant name (e.g. `key_code("kReturn") == Some(0x0D)`). The
        /// Bevy wiring uses this to map engine key events onto the same
        /// codes the natives read.
        pub fn key_code(name: &str) -> Option<u32> {
            KEY_CODE_TABLE
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, code)| *code)
        }

        /// Property definitions for every constant: get-only properties
        /// returning the virtual-key code (a write raises the VM's
        /// access-denied error, like the reference's read-only props).
        pub(crate) fn key_properties() -> Vec<NativePropertyDef> {
            let mut props = Vec::with_capacity(KEY_CODE_TABLE.len());
            $(
                #[allow(non_snake_case)]
                extern "C" fn $rname(
                    _engine: *mut c_void,
                    out: *mut Value,
                    _out_error: *mut *mut c_char,
                ) -> c_int {
                    set_int_out(out, $code as i64);
                    0
                }
                props.push(NativePropertyDef {
                    name: $tjs,
                    get: Some($rname),
                    set: None,
                });
            )*
            props
        }
    };
}

// The table is grouped like the reference: navigation/control keys, digits,
// letters, Windows keys, numpad, function keys, modifier keys, browser/
// media/launch keys, OEM keys, and the IME/rare keys.
key_code_table! {
    // -- navigation / control ------------------------------------------------
    (kBack, "kBack", 0x08),           // VK_BACK
    (kTab, "kTab", 0x09),             // VK_TAB
    (kClear, "kClear", 0x0C),         // VK_CLEAR
    (kReturn, "kReturn", 0x0D),       // VK_RETURN
    (kShift, "kShift", 0x10),         // VK_SHIFT
    (kControl, "kControl", 0x11),     // VK_CONTROL
    (kMenu, "kMenu", 0x12),           // VK_MENU
    (kPause, "kPause", 0x13),         // VK_PAUSE
    (kCapital, "kCapital", 0x14),     // VK_CAPITAL
    (kKana, "kKana", 0x15),           // VK_KANA
    (kHangul, "kHangul", 0x15),       // VK_HANGUL (== VK_KANA)
    (kJunja, "kJunja", 0x17),         // VK_JUNJA
    (kFinal, "kFinal", 0x18),         // VK_FINAL
    (kHanja, "kHanja", 0x19),         // VK_HANJA
    (kKanji, "kKanji", 0x19),         // VK_KANJI (== VK_HANJA)
    (kEscape, "kEscape", 0x1B),       // VK_ESCAPE
    (kConvert, "kConvert", 0x1C),     // VK_CONVERT
    (kNonConvert, "kNonConvert", 0x1D), // VK_NONCONVERT
    (kAccept, "kAccept", 0x1E),       // VK_ACCEPT
    (kModeChange, "kModeChange", 0x1F), // VK_MODECHANGE
    (kSpace, "kSpace", 0x20),         // VK_SPACE
    (kPrior, "kPrior", 0x21),         // VK_PRIOR (PageUp)
    (kNext, "kNext", 0x22),           // VK_NEXT (PageDown)
    (kEnd, "kEnd", 0x23),             // VK_END
    (kHome, "kHome", 0x24),           // VK_HOME
    (kLeft, "kLeft", 0x25),           // VK_LEFT
    (kUp, "kUp", 0x26),               // VK_UP
    (kRight, "kRight", 0x27),         // VK_RIGHT
    (kDown, "kDown", 0x28),           // VK_DOWN
    (kSelect, "kSelect", 0x29),       // VK_SELECT
    (kPrint, "kPrint", 0x2A),         // VK_PRINT
    (kExecute, "kExecute", 0x2B),     // VK_EXECUTE
    (kSnapshot, "kSnapshot", 0x2C),   // VK_SNAPSHOT (PrintScreen)
    (kInsert, "kInsert", 0x2D),       // VK_INSERT
    (kDelete, "kDelete", 0x2E),       // VK_DELETE
    (kHelp, "kHelp", 0x2F),           // VK_HELP

    // -- digits ---------------------------------------------------------------
    (k0, "k0", 0x30),                 // VK_0
    (k1, "k1", 0x31),                 // VK_1
    (k2, "k2", 0x32),                 // VK_2
    (k3, "k3", 0x33),                 // VK_3
    (k4, "k4", 0x34),                 // VK_4
    (k5, "k5", 0x35),                 // VK_5
    (k6, "k6", 0x36),                 // VK_6
    (k7, "k7", 0x37),                 // VK_7
    (k8, "k8", 0x38),                 // VK_8
    (k9, "k9", 0x39),                 // VK_9

    // -- letters --------------------------------------------------------------
    (kA, "kA", 0x41),                 // VK_A
    (kB, "kB", 0x42),                 // VK_B
    (kC, "kC", 0x43),                 // VK_C
    (kD, "kD", 0x44),                 // VK_D
    (kE, "kE", 0x45),                 // VK_E
    (kF, "kF", 0x46),                 // VK_F
    (kG, "kG", 0x47),                 // VK_G
    (kH, "kH", 0x48),                 // VK_H
    (kI, "kI", 0x49),                 // VK_I
    (kJ, "kJ", 0x4A),                 // VK_J
    (kK, "kK", 0x4B),                 // VK_K
    (kL, "kL", 0x4C),                 // VK_L
    (kM, "kM", 0x4D),                 // VK_M
    (kN, "kN", 0x4E),                 // VK_N
    (kO, "kO", 0x4F),                 // VK_O
    (kP, "kP", 0x50),                 // VK_P
    (kQ, "kQ", 0x51),                 // VK_Q
    (kR, "kR", 0x52),                 // VK_R
    (kS, "kS", 0x53),                 // VK_S
    (kT, "kT", 0x54),                 // VK_T
    (kU, "kU", 0x55),                 // VK_U
    (kV, "kV", 0x56),                 // VK_V
    (kW, "kW", 0x57),                 // VK_W
    (kX, "kX", 0x58),                 // VK_X
    (kY, "kY", 0x59),                 // VK_Y
    (kZ, "kZ", 0x5A),                 // VK_Z

    // -- Windows keys -----------------------------------------------------------
    (kLWin, "kLWin", 0x5B),           // VK_LWIN
    (kRWin, "kRWin", 0x5C),           // VK_RWIN
    (kApps, "kApps", 0x5D),           // VK_APPS (context menu)
    (kSleep, "kSleep", 0x5F),         // VK_SLEEP

    // -- numpad -------------------------------------------------------------
    (kNumPad0, "kNumPad0", 0x60),     // VK_NUMPAD0
    (kNumPad1, "kNumPad1", 0x61),     // VK_NUMPAD1
    (kNumPad2, "kNumPad2", 0x62),     // VK_NUMPAD2
    (kNumPad3, "kNumPad3", 0x63),     // VK_NUMPAD3
    (kNumPad4, "kNumPad4", 0x64),     // VK_NUMPAD4
    (kNumPad5, "kNumPad5", 0x65),     // VK_NUMPAD5
    (kNumPad6, "kNumPad6", 0x66),     // VK_NUMPAD6
    (kNumPad7, "kNumPad7", 0x67),     // VK_NUMPAD7
    (kNumPad8, "kNumPad8", 0x68),     // VK_NUMPAD8
    (kNumPad9, "kNumPad9", 0x69),     // VK_NUMPAD9
    (kMultiply, "kMultiply", 0x6A),   // VK_MULTIPLY
    (kAdd, "kAdd", 0x6B),             // VK_ADD
    (kSeparator, "kSeparator", 0x6C), // VK_SEPARATOR
    (kSubtract, "kSubtract", 0x6D),   // VK_SUBTRACT
    (kDecimal, "kDecimal", 0x6E),     // VK_DECIMAL
    (kDivide, "kDivide", 0x6F),       // VK_DIVIDE

    // -- function keys ---------------------------------------------------------
    (kF1, "kF1", 0x70),               // VK_F1
    (kF2, "kF2", 0x71),               // VK_F2
    (kF3, "kF3", 0x72),               // VK_F3
    (kF4, "kF4", 0x73),               // VK_F4
    (kF5, "kF5", 0x74),               // VK_F5
    (kF6, "kF6", 0x75),               // VK_F6
    (kF7, "kF7", 0x76),               // VK_F7
    (kF8, "kF8", 0x77),               // VK_F8
    (kF9, "kF9", 0x78),               // VK_F9
    (kF10, "kF10", 0x79),             // VK_F10
    (kF11, "kF11", 0x7A),             // VK_F11
    (kF12, "kF12", 0x7B),             // VK_F12
    (kF13, "kF13", 0x7C),             // VK_F13
    (kF14, "kF14", 0x7D),             // VK_F14
    (kF15, "kF15", 0x7E),             // VK_F15
    (kF16, "kF16", 0x7F),             // VK_F16
    (kF17, "kF17", 0x80),             // VK_F17
    (kF18, "kF18", 0x81),             // VK_F18
    (kF19, "kF19", 0x82),             // VK_F19
    (kF20, "kF20", 0x83),             // VK_F20
    (kF21, "kF21", 0x84),             // VK_F21
    (kF22, "kF22", 0x85),             // VK_F22
    (kF23, "kF23", 0x86),             // VK_F23
    (kF24, "kF24", 0x87),             // VK_F24

    (kNumLock, "kNumLock", 0x90),     // VK_NUMLOCK
    (kScroll, "kScroll", 0x91),       // VK_SCROLL

    // -- modifier keys ---------------------------------------------------------
    (kLShift, "kLShift", 0xA0),       // VK_LSHIFT
    (kRShift, "kRShift", 0xA1),       // VK_RSHIFT
    (kLControl, "kLControl", 0xA2),   // VK_LCONTROL
    (kRControl, "kRControl", 0xA3),   // VK_RCONTROL
    (kLMenu, "kLMenu", 0xA4),         // VK_LMENU (left Alt)
    (kRMenu, "kRMenu", 0xA5),         // VK_RMENU (right Alt)

    // -- browser / media / launch keys -----------------------------------------
    (kBrowserBack, "kBrowserBack", 0xA6),         // VK_BROWSER_BACK
    (kBrowserForward, "kBrowserForward", 0xA7),   // VK_BROWSER_FORWARD
    (kBrowserRefresh, "kBrowserRefresh", 0xA8),   // VK_BROWSER_REFRESH
    (kBrowserStop, "kBrowserStop", 0xA9),         // VK_BROWSER_STOP
    (kBrowserSearch, "kBrowserSearch", 0xAA),     // VK_BROWSER_SEARCH
    (kBrowserFavorites, "kBrowserFavorites", 0xAB), // VK_BROWSER_FAVORITES
    (kBrowserHome, "kBrowserHome", 0xAC),         // VK_BROWSER_HOME
    (kVolumeMute, "kVolumeMute", 0xAD),           // VK_VOLUME_MUTE
    (kVolumeDown, "kVolumeDown", 0xAE),           // VK_VOLUME_DOWN
    (kVolumeUp, "kVolumeUp", 0xAF),               // VK_VOLUME_UP
    (kMediaNextTrack, "kMediaNextTrack", 0xB0),   // VK_MEDIA_NEXT_TRACK
    (kMediaPrevTrack, "kMediaPrevTrack", 0xB1),   // VK_MEDIA_PREV_TRACK
    (kMediaStop, "kMediaStop", 0xB2),             // VK_MEDIA_STOP
    (kMediaPlayPause, "kMediaPlayPause", 0xB3),   // VK_MEDIA_PLAY_PAUSE
    (kLaunchMail, "kLaunchMail", 0xB4),           // VK_LAUNCH_MAIL
    (kLaunchMediaSelect, "kLaunchMediaSelect", 0xB5), // VK_LAUNCH_MEDIA_SELECT
    (kLaunchApp1, "kLaunchApp1", 0xB6),           // VK_LAUNCH_APP1
    (kLaunchApp2, "kLaunchApp2", 0xB7),           // VK_LAUNCH_APP2

    // -- OEM keys -------------------------------------------------------------
    (kOEM1, "kOEM1", 0xBA),           // VK_OEM_1 (';:' for US)
    (kOEMPlus, "kOEMPlus", 0xBB),     // VK_OEM_PLUS ('+')
    (kOEMComma, "kOEMComma", 0xBC),   // VK_OEM_COMMA (',')
    (kOEMMinus, "kOEMMinus", 0xBD),   // VK_OEM_MINUS ('-')
    (kOEMPeriod, "kOEMPeriod", 0xBE), // VK_OEM_PERIOD ('.')
    (kOEM2, "kOEM2", 0xBF),           // VK_OEM_2 ('/?' for US)
    (kOEM3, "kOEM3", 0xC0),           // VK_OEM_3 ('`~' for US)
    (kOEM4, "kOEM4", 0xDB),           // VK_OEM_4 ('[{' for US)
    (kOEM5, "kOEM5", 0xDC),           // VK_OEM_5 ('\|' for US)
    (kOEM6, "kOEM6", 0xDD),           // VK_OEM_6 (']}' for US)
    (kOEM7, "kOEM7", 0xDE),           // VK_OEM_7 ('"'' for US)
    (kOEM8, "kOEM8", 0xDF),           // VK_OEM_8

    // -- IME / rare keys ---------------------------------------------------------
    (kProcessKey, "kProcessKey", 0xE5), // VK_PROCESSKEY
    (kAttn, "kAttn", 0xF6),             // VK_ATTN
    (kCrsel, "kCrsel", 0xF7),           // VK_CRSEL
    (kExsel, "kExsel", 0xF8),           // VK_EXSEL
    (kEreof, "kEreof", 0xF9),           // VK_EREOF
    (kPlay, "kPlay", 0xFA),             // VK_PLAY
    (kZoom, "kZoom", 0xFB),             // VK_ZOOM
    (kNoName, "kNoName", 0xFC),         // VK_NONAME
    (kPa1, "kPa1", 0xFD),               // VK_PA1
    (kOEMClear, "kOEMClear", 0xFE),     // VK_OEM_CLEAR
}

/// Coerce a TJS argument to a key code. Out-of-range / negative values
/// simply never match a tracked key — the reference raises
/// `TJS_E_INVALIDPARAM` for codes ≥ 256, this port returns false/0
/// (documented deviation; tests require it).
fn key_code_arg(v: &Value) -> u32 {
    value_as_i64(v) as u32
}

/// `Key.getPressed(code)` → bool — whether the key is currently held.
extern "C" fn native_get_pressed(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Key.getPressed requires 1 argument");
    }
    let code = key_code_arg(&args(argv, argc)[0]);
    let pressed = with_state(|s| s.keys.pressed.contains_key(&code));
    set_int_out(out, i64::from(pressed));
    0
}

/// `Key.getReleased(code)` → bool — whether the key was released this
/// frame (the release flag lives for exactly one frame; see
/// [`crate::KeyState::released`]).
extern "C" fn native_get_released(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Key.getReleased requires 1 argument");
    }
    let code = key_code_arg(&args(argv, argc)[0]);
    let released = with_state(|s| s.keys.released.contains_key(&code));
    set_int_out(out, i64::from(released));
    0
}

/// `Key.getRepeat(code)` → int — consecutive frames the key has been held
/// (increments each `end_frame` while held; 0 when not held).
extern "C" fn native_get_repeat(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Key.getRepeat requires 1 argument");
    }
    let code = key_code_arg(&args(argv, argc)[0]);
    let repeat = with_state(|s| s.keys.repeat.get(&code).copied().unwrap_or(0));
    set_int_out(out, i64::from(repeat));
    0
}

/// Register the `Key` native class: the three state methods plus the
/// `kXXX` key-code constant properties.
pub fn register_key(engine: &Tjs2Engine) -> Result<(), String> {
    engine.register_native_class(&NativeClassBuilder {
        name: "Key",
        methods: vec![
            NativeMethodDef {
                name: "getPressed",
                f: native_get_pressed,
            },
            NativeMethodDef {
                name: "getReleased",
                f: native_get_released,
            },
            NativeMethodDef {
                name: "getRepeat",
                f: native_get_repeat,
            },
        ],
        properties: key_properties(),
    })
}
