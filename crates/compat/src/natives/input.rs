//! Input native classes: `Mouse`, `Key`, `Pad`.
//!
//! Behavioral reference: `crates/tvp-input/src/{lib.rs,mouse.rs,key.rs}`
//! and the reference `Pad` class (`reference/cpp/core/utils/PadIntf.cpp`).
//!
//! The input state is the generic L2 state
//! ([`engine::input::InputState`]) that the host attaches through
//! [`CompatServices::attach_input`](crate::CompatServices::attach_input) —
//! the TVP-specific parts (the VK code table, the click-sequence constants
//! and the `TVP_SS_*` flags) stay here.
//!
//! # Frame protocol
//!
//! The host owns the frame loop and feeds the L2 state from the L1 input
//! events:
//!
//! ```text
//! begin_input_frame                       // installs the TVP policy, clears per-frame data
//!   apply_input_event(event) ...          // pointer/button/key/wheel events
//! end_input_frame                         // commits held counters, frame += 1
//! ```
//!
//! [`engine::input::InputState::begin_frame`] clears the per-frame release
//! flags, the wheel accumulation, the released keys and `last_pressed`;
//! [`engine::input::InputState::end_frame`] bumps the held-frame counters and
//! advances the frame index. Both are driven by the host's per-frame body.
//!
//! # Host wiring
//!
//! The L1→L3 event path is [`crate::EventAdapter`]: `map` maps a
//! [`platform::PlatformEvent`] to a TJS-visible event and `dispatch` delivers
//! it. The input *state* side of that path is reachable through the inherent
//! methods on [`crate::EventAdapter`] defined below
//! ([`crate::EventAdapter::begin_input_frame`],
//! [`crate::EventAdapter::apply_input_event`],
//! [`crate::EventAdapter::end_input_frame`]). Applying an input event also
//! caches the pointer position and shift mask the mapping needs.
//!
//! # Script surface
//!
//! `Mouse.*`: `getCursorPos([obj])`, `getCursorX`, `getCursorY`,
//! `setCursorPos`, `getWheelRot`, `getWheelRotX/Y/Z`, `isVisible`,
//! `setVisible`, `getPressed`, `getReleased`, `getRepeat`, `getClickCount`.
//!
//! `Key.*`: `getPressed`, `getReleased`, `getRepeat` plus the `kXXX`
//! read-only key-code constants.
//!
//! `Pad`: the reference's read-only property stub (`text`, `fileName`,
//! `color`, `visible`, `title`, the font properties, `readOnly`, …); the
//! reference setters are accepted and ignored. `Pad` is the reference's
//! *text editor* placeholder (`reference/cpp/core/utils/PadIntf.cpp`); it is
//! not a gamepad class. Gamepad input reaches scripts as `VK_PAD*` keys in
//! the L2 [`InputState`], through `Key`/`System`.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use engine::input::InputState;
use platform::{MouseButton, PhysicalKey, PlatformEvent};
use tjs::{
    NativeClassBuilder, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef,
    NativeMethodDef, NativePropertyDef, TjsValue, VAL_OBJECT, Value,
};

use crate::native_util::{args, report_error, set_bool, set_int, set_void};
use crate::{CompatCtx, CompatError};

use super::system::{resolve, set_string_out, value_as_bool, value_as_i64};

// ---------------------------------------------------------------------------
// Mouse button indices (reference `enum tTVPMouseButton`, tvpinputdefs.h)
// ---------------------------------------------------------------------------

/// Left mouse button (`mbLeft`).
pub const MB_LEFT: usize = 0;
/// Right mouse button (`mbRight`).
pub const MB_RIGHT: usize = 1;
/// Middle mouse button (`mbMiddle`).
pub const MB_MIDDLE: usize = 2;
/// First extra button (`mbX1`).
pub const MB_X1: usize = 3;
/// Second extra button (`mbX2`).
pub const MB_X2: usize = 4;
/// The number of tracked mouse buttons.
pub const MOUSE_BUTTONS: usize = 5;

/// The Windows virtual-key code of a TVP mouse button (`VK_LBUTTON` …).
mod mouse_vk {
    /// `VK_LBUTTON` (`mbLeft`).
    pub const VK_LBUTTON: u32 = 0x01;
    /// `VK_RBUTTON` (`mbRight`).
    pub const VK_RBUTTON: u32 = 0x02;
    /// `VK_MBUTTON` (`mbMiddle`).
    pub const VK_MBUTTON: u32 = 0x04;
    /// `VK_XBUTTON1` (`mbX1`).
    pub const VK_XBUTTON1: u32 = 0x05;
    /// `VK_XBUTTON2` (`mbX2`).
    pub const VK_XBUTTON2: u32 = 0x06;
}

use mouse_vk::{VK_LBUTTON, VK_MBUTTON, VK_RBUTTON, VK_XBUTTON1, VK_XBUTTON2};

/// Map a Windows virtual-key code to the TVP button index
/// ([`MB_LEFT`]..[`MB_X2`]); `None` when `vk` is not a mouse-button code.
pub fn mouse_button_from_vk(vk: u32) -> Option<usize> {
    match vk {
        VK_LBUTTON => Some(MB_LEFT),
        VK_RBUTTON => Some(MB_RIGHT),
        VK_MBUTTON => Some(MB_MIDDLE),
        VK_XBUTTON1 => Some(MB_X1),
        VK_XBUTTON2 => Some(MB_X2),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Physical key <-> reference virtual-key code (`VK_*`)
// ---------------------------------------------------------------------------

/// The reference Windows virtual-key code a platform-neutral physical key
/// names, or `0` when the reference names no such key.
pub fn vk_from_physical(key: PhysicalKey) -> u32 {
    PHYSICAL_VK_TABLE
        .iter()
        .find(|(candidate, _)| *candidate == key)
        .map_or(0, |(_, vk)| *vk)
}

/// The physical key a reference virtual-key code names, or `None` for a code
/// the reference uses as a query pseudo-code (`VK_PADANY`) or a mouse button.
///
/// The generic modifier codes `VK_SHIFT`/`VK_CONTROL`/`VK_MENU` name either
/// side and are resolved by [`modifier_keys`]. The explicit side codes
/// (`VK_LSHIFT` … `VK_RMENU`) resolve to that side.
pub fn physical_from_vk(vk: u32) -> Option<PhysicalKey> {
    let side = match vk {
        VK_LSHIFT => Some(PhysicalKey::ShiftLeft),
        VK_RSHIFT => Some(PhysicalKey::ShiftRight),
        VK_LCONTROL => Some(PhysicalKey::ControlLeft),
        VK_RCONTROL => Some(PhysicalKey::ControlRight),
        VK_LMENU => Some(PhysicalKey::AltLeft),
        VK_RMENU => Some(PhysicalKey::AltRight),
        _ => None,
    };
    side.or_else(|| {
        PHYSICAL_VK_TABLE
            .iter()
            .find(|(_, value)| *value == vk)
            .map(|(key, _)| *key)
    })
}

/// The explicit side virtual-key codes (`VK_LSHIFT` … `VK_RMENU`).
const VK_LSHIFT: u32 = 0xA0;
const VK_RSHIFT: u32 = 0xA1;
const VK_LCONTROL: u32 = 0xA2;
const VK_RCONTROL: u32 = 0xA3;
const VK_LMENU: u32 = 0xA4;
const VK_RMENU: u32 = 0xA5;

/// The physical keys a `vk` query against the generic modifier codes matches.
fn modifier_keys(vk: u32) -> Option<&'static [PhysicalKey]> {
    match vk {
        VK_SHIFT => Some(&[PhysicalKey::ShiftLeft, PhysicalKey::ShiftRight]),
        VK_CONTROL => Some(&[PhysicalKey::ControlLeft, PhysicalKey::ControlRight]),
        VK_MENU => Some(&[PhysicalKey::AltLeft, PhysicalKey::AltRight]),
        _ => None,
    }
}

/// The reference virtual-key code for a stored physical-key identity.
///
/// `index` is a [`PhysicalKey::index`] value; an identity with no named key
/// maps to `0`.
pub fn vk_from_index(index: u32) -> u32 {
    PHYSICAL_VK_TABLE
        .iter()
        .find(|(key, _)| key.index() == index)
        .map_or(0, |(_, vk)| *vk)
}

/// Physical key -> reference Windows virtual-key code.
///
/// The values are the codes the KiriKiri scripts compare against
/// (`system/window.tjs:119-193`), the C++ `VK_*` constants
/// (`reference/cpp/core/base/ScriptMgnIntf.cpp:180-198`) and the reference
/// platform's key conversion, which collapses the left/right modifiers onto the
/// generic codes (`reference/cpp/core/environ/cocos2d/CCKeyCodeConv.cpp:91-103`).
const PHYSICAL_VK_TABLE: &[(PhysicalKey, u32)] = &[
    (PhysicalKey::KeyA, 0x41),
    (PhysicalKey::KeyB, 0x42),
    (PhysicalKey::KeyC, 0x43),
    (PhysicalKey::KeyD, 0x44),
    (PhysicalKey::KeyE, 0x45),
    (PhysicalKey::KeyF, 0x46),
    (PhysicalKey::KeyG, 0x47),
    (PhysicalKey::KeyH, 0x48),
    (PhysicalKey::KeyI, 0x49),
    (PhysicalKey::KeyJ, 0x4A),
    (PhysicalKey::KeyK, 0x4B),
    (PhysicalKey::KeyL, 0x4C),
    (PhysicalKey::KeyM, 0x4D),
    (PhysicalKey::KeyN, 0x4E),
    (PhysicalKey::KeyO, 0x4F),
    (PhysicalKey::KeyP, 0x50),
    (PhysicalKey::KeyQ, 0x51),
    (PhysicalKey::KeyR, 0x52),
    (PhysicalKey::KeyS, 0x53),
    (PhysicalKey::KeyT, 0x54),
    (PhysicalKey::KeyU, 0x55),
    (PhysicalKey::KeyV, 0x56),
    (PhysicalKey::KeyW, 0x57),
    (PhysicalKey::KeyX, 0x58),
    (PhysicalKey::KeyY, 0x59),
    (PhysicalKey::KeyZ, 0x5A),
    (PhysicalKey::Digit0, 0x30),
    (PhysicalKey::Digit1, 0x31),
    (PhysicalKey::Digit2, 0x32),
    (PhysicalKey::Digit3, 0x33),
    (PhysicalKey::Digit4, 0x34),
    (PhysicalKey::Digit5, 0x35),
    (PhysicalKey::Digit6, 0x36),
    (PhysicalKey::Digit7, 0x37),
    (PhysicalKey::Digit8, 0x38),
    (PhysicalKey::Digit9, 0x39),
    (PhysicalKey::F1, 0x70),
    (PhysicalKey::F2, 0x71),
    (PhysicalKey::F3, 0x72),
    (PhysicalKey::F4, 0x73),
    (PhysicalKey::F5, 0x74),
    (PhysicalKey::F6, 0x75),
    (PhysicalKey::F7, 0x76),
    (PhysicalKey::F8, 0x77),
    (PhysicalKey::F9, 0x78),
    (PhysicalKey::F10, 0x79),
    (PhysicalKey::F11, 0x7A),
    (PhysicalKey::F12, 0x7B),
    (PhysicalKey::F13, 0x7C),
    (PhysicalKey::F14, 0x7D),
    (PhysicalKey::F15, 0x7E),
    (PhysicalKey::F16, 0x7F),
    (PhysicalKey::F17, 0x80),
    (PhysicalKey::F18, 0x81),
    (PhysicalKey::F19, 0x82),
    (PhysicalKey::F20, 0x83),
    (PhysicalKey::F21, 0x84),
    (PhysicalKey::F22, 0x85),
    (PhysicalKey::F23, 0x86),
    (PhysicalKey::F24, 0x87),
    (PhysicalKey::Backspace, 0x08),
    (PhysicalKey::Tab, 0x09),
    (PhysicalKey::Clear, 0x0C),
    (PhysicalKey::Enter, 0x0D),
    (PhysicalKey::NumpadEnter, 0x0D),
    (PhysicalKey::Escape, 0x1B),
    (PhysicalKey::Space, 0x20),
    (PhysicalKey::Delete, 0x2E),
    (PhysicalKey::Insert, 0x2D),
    (PhysicalKey::Home, 0x24),
    (PhysicalKey::End, 0x23),
    (PhysicalKey::PageUp, 0x21),
    (PhysicalKey::PageDown, 0x22),
    (PhysicalKey::ArrowUp, 0x26),
    (PhysicalKey::ArrowDown, 0x28),
    (PhysicalKey::ArrowLeft, 0x25),
    (PhysicalKey::ArrowRight, 0x27),
    (PhysicalKey::Select, 0x29),
    (PhysicalKey::Print, 0x2A),
    (PhysicalKey::Execute, 0x2B),
    (PhysicalKey::PrintScreen, 0x2C),
    (PhysicalKey::Help, 0x2F),
    (PhysicalKey::CapsLock, 0x14),
    (PhysicalKey::NumLock, 0x90),
    (PhysicalKey::ScrollLock, 0x91),
    (PhysicalKey::Pause, 0x13),
    (PhysicalKey::SuperLeft, 0x5B),
    (PhysicalKey::SuperRight, 0x5C),
    (PhysicalKey::ContextMenu, 0x5D),
    (PhysicalKey::Sleep, 0x5F),
    (PhysicalKey::ShiftLeft, 0x10),
    (PhysicalKey::ShiftRight, 0x10),
    (PhysicalKey::ControlLeft, 0x11),
    (PhysicalKey::ControlRight, 0x11),
    (PhysicalKey::AltLeft, 0x12),
    (PhysicalKey::AltRight, 0x12),
    (PhysicalKey::Numpad0, 0x60),
    (PhysicalKey::Numpad1, 0x61),
    (PhysicalKey::Numpad2, 0x62),
    (PhysicalKey::Numpad3, 0x63),
    (PhysicalKey::Numpad4, 0x64),
    (PhysicalKey::Numpad5, 0x65),
    (PhysicalKey::Numpad6, 0x66),
    (PhysicalKey::Numpad7, 0x67),
    (PhysicalKey::Numpad8, 0x68),
    (PhysicalKey::Numpad9, 0x69),
    (PhysicalKey::NumpadMultiply, 0x6A),
    (PhysicalKey::NumpadAdd, 0x6B),
    (PhysicalKey::NumpadSeparator, 0x6C),
    (PhysicalKey::NumpadSubtract, 0x6D),
    (PhysicalKey::NumpadDecimal, 0x6E),
    (PhysicalKey::NumpadDivide, 0x6F),
    (PhysicalKey::Backquote, 0xC0),
    (PhysicalKey::Backslash, 0xDC),
    (PhysicalKey::BracketLeft, 0xDB),
    (PhysicalKey::BracketRight, 0xDD),
    (PhysicalKey::Comma, 0xBC),
    (PhysicalKey::Period, 0xBE),
    (PhysicalKey::Semicolon, 0xBA),
    (PhysicalKey::Quote, 0xDE),
    (PhysicalKey::Minus, 0xBD),
    (PhysicalKey::Equal, 0xBB),
    (PhysicalKey::Slash, 0xBF),
    (PhysicalKey::IntlBackslash, 0xE2),
    (PhysicalKey::IntlRo, 0xC1),
    (PhysicalKey::IntlYen, 0xC2),
    (PhysicalKey::KanaMode, 0x15),
    (PhysicalKey::Hangul, 0x15),
    (PhysicalKey::Junja, 0x17),
    (PhysicalKey::Final, 0x18),
    (PhysicalKey::Hanja, 0x19),
    (PhysicalKey::Convert, 0x1C),
    (PhysicalKey::NonConvert, 0x1D),
    (PhysicalKey::Accept, 0x1E),
    (PhysicalKey::ModeChange, 0x1F),
    (PhysicalKey::ProcessKey, 0xE5),
    (PhysicalKey::BrowserBack, 0xA6),
    (PhysicalKey::BrowserForward, 0xA7),
    (PhysicalKey::BrowserRefresh, 0xA8),
    (PhysicalKey::BrowserStop, 0xA9),
    (PhysicalKey::BrowserSearch, 0xAA),
    (PhysicalKey::BrowserFavorites, 0xAB),
    (PhysicalKey::BrowserHome, 0xAC),
    (PhysicalKey::VolumeMute, 0xAD),
    (PhysicalKey::VolumeDown, 0xAE),
    (PhysicalKey::VolumeUp, 0xAF),
    (PhysicalKey::MediaTrackNext, 0xB0),
    (PhysicalKey::MediaTrackPrevious, 0xB1),
    (PhysicalKey::MediaStop, 0xB2),
    (PhysicalKey::MediaPlayPause, 0xB3),
    (PhysicalKey::LaunchMail, 0xB4),
    (PhysicalKey::MediaSelect, 0xB5),
    (PhysicalKey::LaunchApp1, 0xB6),
    (PhysicalKey::LaunchApp2, 0xB7),
];

// ---------------------------------------------------------------------------
// Gamepad virtual-key codes (reference `VK_PAD*`, tvpinputdefs.h)
// ---------------------------------------------------------------------------

/// Gamepad "left" code (`VK_PADLEFT`).
pub const VK_PADLEFT: u32 = 0x1B5;
/// Gamepad "up" code (`VK_PADUP`).
pub const VK_PADUP: u32 = 0x1B6;
/// Gamepad "right" code (`VK_PADRIGHT`).
pub const VK_PADRIGHT: u32 = 0x1B7;
/// Gamepad "down" code (`VK_PADDOWN`).
pub const VK_PADDOWN: u32 = 0x1B8;
/// Gamepad button 1 code (`VK_PAD1`).
pub const VK_PAD1: u32 = 0x1C0;
/// Gamepad button 2 code (`VK_PAD2`).
pub const VK_PAD2: u32 = 0x1C1;
/// Gamepad button 3 code (`VK_PAD3`).
pub const VK_PAD3: u32 = 0x1C2;
/// Gamepad button 4 code (`VK_PAD4`).
pub const VK_PAD4: u32 = 0x1C3;
/// Gamepad button 5 code (`VK_PAD5`).
pub const VK_PAD5: u32 = 0x1C4;
/// Gamepad button 6 code (`VK_PAD6`).
pub const VK_PAD6: u32 = 0x1C5;
/// Gamepad button 7 code (`VK_PAD7`).
pub const VK_PAD7: u32 = 0x1C6;
/// Gamepad button 8 code (`VK_PAD8`).
pub const VK_PAD8: u32 = 0x1C7;
/// Gamepad button 9 code (`VK_PAD9`).
pub const VK_PAD9: u32 = 0x1C8;
/// Gamepad button 10 code (`VK_PAD10`).
pub const VK_PAD10: u32 = 0x1C9;
/// "Any pad button held" pseudo-code (`VK_PADANY`).
pub const VK_PADANY: u32 = 0x1DF;

/// Every concrete pad button code (d-pad + pad buttons 1..10), in reference
/// order. [`VK_PADANY`] is a query pseudo-code, not a button.
pub const PAD_CODES: [u32; 14] = [
    VK_PADLEFT,
    VK_PADUP,
    VK_PADRIGHT,
    VK_PADDOWN,
    VK_PAD1,
    VK_PAD2,
    VK_PAD3,
    VK_PAD4,
    VK_PAD5,
    VK_PAD6,
    VK_PAD7,
    VK_PAD8,
    VK_PAD9,
    VK_PAD10,
];

/// The Windows virtual-key code of `VK_SHIFT`.
pub const VK_SHIFT: u32 = 0x10;
/// The Windows virtual-key code of `VK_CONTROL`.
pub const VK_CONTROL: u32 = 0x11;
/// The Windows virtual-key code of `VK_MENU` (Alt).
pub const VK_MENU: u32 = 0x12;

/// Reference `TVP_SS_*` shift-state flag bits (`tvpinputdefs.h`).
pub mod shift_state {
    /// Shift key held.
    pub const TVP_SS_SHIFT: u32 = 0x01;
    /// Alt/Menu key held.
    pub const TVP_SS_ALT: u32 = 0x02;
    /// Control key held.
    pub const TVP_SS_CTRL: u32 = 0x04;
    /// Left mouse button held.
    pub const TVP_SS_LEFT: u32 = 0x08;
    /// Right mouse button held.
    pub const TVP_SS_RIGHT: u32 = 0x10;
    /// Middle mouse button held.
    pub const TVP_SS_MIDDLE: u32 = 0x20;
}

use shift_state::{
    TVP_SS_ALT, TVP_SS_CTRL, TVP_SS_LEFT, TVP_SS_MIDDLE, TVP_SS_RIGHT, TVP_SS_SHIFT,
};

/// Frames within which a follow-up press counts as the next click of a
/// multi-click sequence (`Mouse.getClickCount`).
pub const MOUSE_CLICK_SEQUENCE_FRAMES: u64 = 30;
/// Maximum cursor movement (device pixels) between presses for them to stay
/// in one multi-click sequence.
pub const MOUSE_CLICK_MAX_MOVE: i32 = 4;

// ---------------------------------------------------------------------------
// TVP input semantics over the L2 state
// ---------------------------------------------------------------------------

/// The generic L2 mouse button for a TVP button index
/// ([`MB_LEFT`]..[`MB_X2`]); `None` for an out-of-range index.
pub fn mouse_button(index: usize) -> Option<MouseButton> {
    match index {
        MB_LEFT => Some(MouseButton::Left),
        MB_RIGHT => Some(MouseButton::Right),
        MB_MIDDLE => Some(MouseButton::Middle),
        MB_X1 => Some(MouseButton::Back),
        MB_X2 => Some(MouseButton::Forward),
        _ => None,
    }
}

/// Borrow the L2 input state attached to `ctx`'s services.
///
/// # Safety
/// The caller must be inside a native callback for a bound engine whose
/// services have the input state attached (the
/// [`CompatServices::attach_input`](crate::CompatServices::attach_input)
/// contract). The returned borrow is tied to `ctx` and never outlives the
/// callback.
unsafe fn input_state(ctx: &CompatCtx) -> Option<&InputState> {
    // SAFETY: the caller upholds the attach contract.
    unsafe { ctx.services().input().map(|ptr| &*ptr) }
}

/// Whether virtual-key `code` is held: mouse-button VKs resolve to the
/// generic mouse state, [`VK_PADANY`] is true while any pad button is held.
pub fn vk_pressed(state: &InputState, code: u32) -> bool {
    if let Some(index) = mouse_button_from_vk(code) {
        return mouse_button(index).is_some_and(|button| state.is_button_down(button));
    }
    if code == VK_PADANY {
        return PAD_CODES.iter().any(|&c| state.is_key_down(c));
    }
    if PAD_CODES.contains(&code) {
        return state.is_key_down(code);
    }
    if let Some(keys) = modifier_keys(code) {
        return keys.iter().any(|key| state.is_key_down(key.index()));
    }
    physical_from_vk(code).is_some_and(|key| state.is_key_down(key.index()))
}

/// Whether virtual-key `code` was released this frame (same mapping as
/// [`vk_pressed`]).
pub fn vk_released(state: &InputState, code: u32) -> bool {
    if let Some(index) = mouse_button_from_vk(code) {
        return mouse_button(index).is_some_and(|button| state.is_button_released(button));
    }
    if code == VK_PADANY {
        return PAD_CODES.iter().any(|&c| state.is_key_released(c));
    }
    if PAD_CODES.contains(&code) {
        return state.is_key_released(code);
    }
    if let Some(keys) = modifier_keys(code) {
        return keys.iter().any(|key| state.is_key_released(key.index()));
    }
    physical_from_vk(code).is_some_and(|key| state.is_key_released(key.index()))
}

/// Held-frame count for virtual-key `code` (`VK_PADANY` = longest pad).
pub fn vk_repeat(state: &InputState, code: u32) -> u32 {
    if let Some(index) = mouse_button_from_vk(code) {
        return mouse_button(index).map_or(0, |button| state.button_hold_frames(button));
    }
    if code == VK_PADANY {
        return PAD_CODES
            .iter()
            .map(|&c| state.key_hold_frames(c))
            .max()
            .unwrap_or(0);
    }
    if PAD_CODES.contains(&code) {
        return state.key_hold_frames(code);
    }
    if let Some(keys) = modifier_keys(code) {
        return keys
            .iter()
            .map(|key| state.key_hold_frames(key.index()))
            .max()
            .unwrap_or(0);
    }
    physical_from_vk(code).map_or(0, |key| state.key_hold_frames(key.index()))
}

/// The reference `TVP_SS_*` shift mask for the currently held state.
pub fn shift_flags(state: &InputState) -> u32 {
    let mut flags = 0;
    if vk_pressed(state, VK_SHIFT) {
        flags |= TVP_SS_SHIFT;
    }
    if vk_pressed(state, VK_MENU) {
        flags |= TVP_SS_ALT;
    }
    if vk_pressed(state, VK_CONTROL) {
        flags |= TVP_SS_CTRL;
    }
    if state.is_button_down(MouseButton::Left) {
        flags |= TVP_SS_LEFT;
    }
    if state.is_button_down(MouseButton::Right) {
        flags |= TVP_SS_RIGHT;
    }
    if state.is_button_down(MouseButton::Middle) {
        flags |= TVP_SS_MIDDLE;
    }
    flags
}

/// The reference `Mouse.getClickCount` value for TVP button `index`.
pub fn mouse_click_count(state: &InputState, index: usize) -> u32 {
    mouse_button(index).map_or(0, |button| state.button_click_count(button))
}

// ---------------------------------------------------------------------------
// Host-facing seam (see the module docs)
// ---------------------------------------------------------------------------

impl crate::EventAdapter {
    /// Clear the per-frame L2 input state and install the TVP click-sequence
    /// window. Call once at the top of the frame, before draining events.
    pub fn begin_input_frame(&self, ctx: &mut CompatCtx) {
        // SAFETY: a `CompatCtx` is only created for a bound, live engine.
        let services = unsafe { ctx.services() };
        if let Some(input) = services.input() {
            // SAFETY: `attach_input` guarantees the pointer stays valid while
            // the binding is installed.
            let input = unsafe { &mut *input };
            input.set_click_sequence(MOUSE_CLICK_SEQUENCE_FRAMES, MOUSE_CLICK_MAX_MOVE as f32);
            input.begin_frame();
        }
    }

    /// Apply one raw L1 platform event to the script-visible L2 input state
    /// and cache the pointer/shift the event mapping needs.
    pub fn apply_input_event(&mut self, ctx: &mut CompatCtx, event: &PlatformEvent) {
        // SAFETY: a `CompatCtx` is only created for a bound, live engine.
        let services = unsafe { ctx.services() };
        if let (Some(input), PlatformEvent::Input(raw)) = (services.input(), event) {
            // SAFETY: `attach_input` guarantees the pointer stays valid.
            let input = unsafe { &mut *input };
            input.apply(raw);
            self.cache_input_state(input);
        }
    }

    /// Commit the input frame: bump the held counters and advance the frame
    /// index. Call after draining events.
    pub fn end_input_frame(&self, ctx: &mut CompatCtx) {
        // SAFETY: a `CompatCtx` is only created for a bound, live engine.
        let services = unsafe { ctx.services() };
        if let Some(input) = services.input() {
            // SAFETY: `attach_input` guarantees the pointer stays valid.
            unsafe { &mut *input }.end_frame();
        }
    }
}

// ---------------------------------------------------------------------------
// `Key` key-code table (reference `VK_*` table, ScriptMgnIntf.cpp /
// tvpinputdefs.h)
// ---------------------------------------------------------------------------

/// Declare the `Key.kXXX` constant table: `($rname, "kName", value)` where
/// `$rname` is the property getter name, `"kName"` the TJS name and `value`
/// the Windows virtual-key code. Generates the table, the [`key_code`]
/// lookup and the property definitions from one source of truth.
macro_rules! key_code_table {
    ($(($rname:ident, $tjs:literal, $code:expr)),* $(,)?) => {
        /// The `Key` class key-code constants: TJS constant name → Windows
        /// virtual-key code, in declaration order.
        pub const KEY_CODE_TABLE: &[(&str, u32)] = &[ $(($tjs, $code)),* ];

        /// Property definitions for every constant: get-only properties
        /// returning the virtual-key code.
        pub fn key_properties() -> Vec<NativePropertyDef> {
            let mut props = Vec::with_capacity(KEY_CODE_TABLE.len());
            $(
                #[allow(non_snake_case)]
                extern "C" fn $rname(
                    _engine: *mut c_void,
                    out: *mut Value,
                    _out_error: *mut *mut c_char,
                ) -> c_int {
                    set_int(out, $code as i64);
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

key_code_table! {
    // -- mouse buttons (VK_LBUTTON..VK_XBUTTON2) --------------------------
    (kLButton, "kLButton", 0x01),     // VK_LBUTTON
    (kRButton, "kRButton", 0x02),     // VK_RBUTTON
    (kCancel, "kCancel", 0x03),       // VK_CANCEL
    (kMButton, "kMButton", 0x04),     // VK_MBUTTON
    (kXButton1, "kXButton1", 0x05),   // VK_XBUTTON1
    (kXButton2, "kXButton2", 0x06),   // VK_XBUTTON2

    // -- navigation / control --------------------------------------------
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
    (kHangul, "kHangul", 0x15),       // VK_HANGUL
    (kJunja, "kJunja", 0x17),         // VK_JUNJA
    (kFinal, "kFinal", 0x18),         // VK_FINAL
    (kHanja, "kHanja", 0x19),         // VK_HANJA
    (kKanji, "kKanji", 0x19),         // VK_KANJI
    (kEscape, "kEscape", 0x1B),       // VK_ESCAPE
    (kConvert, "kConvert", 0x1C),     // VK_CONVERT
    (kNonConvert, "kNonConvert", 0x1D), // VK_NONCONVERT
    (kAccept, "kAccept", 0x1E),       // VK_ACCEPT
    (kModeChange, "kModeChange", 0x1F), // VK_MODECHANGE
    (kSpace, "kSpace", 0x20),         // VK_SPACE
    (kPrior, "kPrior", 0x21),         // VK_PRIOR
    (kNext, "kNext", 0x22),           // VK_NEXT
    (kEnd, "kEnd", 0x23),             // VK_END
    (kHome, "kHome", 0x24),           // VK_HOME
    (kLeft, "kLeft", 0x25),           // VK_LEFT
    (kUp, "kUp", 0x26),               // VK_UP
    (kRight, "kRight", 0x27),         // VK_RIGHT
    (kDown, "kDown", 0x28),           // VK_DOWN
    (kSelect, "kSelect", 0x29),       // VK_SELECT
    (kPrint, "kPrint", 0x2A),         // VK_PRINT
    (kExecute, "kExecute", 0x2B),     // VK_EXECUTE
    (kSnapshot, "kSnapshot", 0x2C),   // VK_SNAPSHOT
    (kInsert, "kInsert", 0x2D),       // VK_INSERT
    (kDelete, "kDelete", 0x2E),       // VK_DELETE
    (kHelp, "kHelp", 0x2F),           // VK_HELP

    // -- digits -----------------------------------------------------------
    (k0, "k0", 0x30),
    (k1, "k1", 0x31),
    (k2, "k2", 0x32),
    (k3, "k3", 0x33),
    (k4, "k4", 0x34),
    (k5, "k5", 0x35),
    (k6, "k6", 0x36),
    (k7, "k7", 0x37),
    (k8, "k8", 0x38),
    (k9, "k9", 0x39),

    // -- letters ----------------------------------------------------------
    (kA, "kA", 0x41),
    (kB, "kB", 0x42),
    (kC, "kC", 0x43),
    (kD, "kD", 0x44),
    (kE, "kE", 0x45),
    (kF, "kF", 0x46),
    (kG, "kG", 0x47),
    (kH, "kH", 0x48),
    (kI, "kI", 0x49),
    (kJ, "kJ", 0x4A),
    (kK, "kK", 0x4B),
    (kL, "kL", 0x4C),
    (kM, "kM", 0x4D),
    (kN, "kN", 0x4E),
    (kO, "kO", 0x4F),
    (kP, "kP", 0x50),
    (kQ, "kQ", 0x51),
    (kR, "kR", 0x52),
    (kS, "kS", 0x53),
    (kT, "kT", 0x54),
    (kU, "kU", 0x55),
    (kV, "kV", 0x56),
    (kW, "kW", 0x57),
    (kX, "kX", 0x58),
    (kY, "kY", 0x59),
    (kZ, "kZ", 0x5A),

    // -- Windows keys -----------------------------------------------------
    (kLWin, "kLWin", 0x5B),           // VK_LWIN
    (kRWin, "kRWin", 0x5C),           // VK_RWIN
    (kApps, "kApps", 0x5D),           // VK_APPS
    (kSleep, "kSleep", 0x5F),         // VK_SLEEP

    // -- numpad -----------------------------------------------------------
    (kNumPad0, "kNumPad0", 0x60),
    (kNumPad1, "kNumPad1", 0x61),
    (kNumPad2, "kNumPad2", 0x62),
    (kNumPad3, "kNumPad3", 0x63),
    (kNumPad4, "kNumPad4", 0x64),
    (kNumPad5, "kNumPad5", 0x65),
    (kNumPad6, "kNumPad6", 0x66),
    (kNumPad7, "kNumPad7", 0x67),
    (kNumPad8, "kNumPad8", 0x68),
    (kNumPad9, "kNumPad9", 0x69),
    (kMultiply, "kMultiply", 0x6A),
    (kAdd, "kAdd", 0x6B),
    (kSeparator, "kSeparator", 0x6C),
    (kSubtract, "kSubtract", 0x6D),
    (kDecimal, "kDecimal", 0x6E),
    (kDivide, "kDivide", 0x6F),

    // -- function keys ----------------------------------------------------
    (kF1, "kF1", 0x70),
    (kF2, "kF2", 0x71),
    (kF3, "kF3", 0x72),
    (kF4, "kF4", 0x73),
    (kF5, "kF5", 0x74),
    (kF6, "kF6", 0x75),
    (kF7, "kF7", 0x76),
    (kF8, "kF8", 0x77),
    (kF9, "kF9", 0x78),
    (kF10, "kF10", 0x79),
    (kF11, "kF11", 0x7A),
    (kF12, "kF12", 0x7B),
    (kF13, "kF13", 0x7C),
    (kF14, "kF14", 0x7D),
    (kF15, "kF15", 0x7E),
    (kF16, "kF16", 0x7F),
    (kF17, "kF17", 0x80),
    (kF18, "kF18", 0x81),
    (kF19, "kF19", 0x82),
    (kF20, "kF20", 0x83),
    (kF21, "kF21", 0x84),
    (kF22, "kF22", 0x85),
    (kF23, "kF23", 0x86),
    (kF24, "kF24", 0x87),

    (kNumLock, "kNumLock", 0x90),     // VK_NUMLOCK
    (kScroll, "kScroll", 0x91),       // VK_SCROLL

    // -- modifier keys ----------------------------------------------------
    (kLShift, "kLShift", 0xA0),       // VK_LSHIFT
    (kRShift, "kRShift", 0xA1),       // VK_RSHIFT
    (kLControl, "kLControl", 0xA2),   // VK_LCONTROL
    (kRControl, "kRControl", 0xA3),   // VK_RCONTROL
    (kLMenu, "kLMenu", 0xA4),         // VK_LMENU
    (kRMenu, "kRMenu", 0xA5),         // VK_RMENU

    // -- browser / media / launch keys ------------------------------------
    (kBrowserBack, "kBrowserBack", 0xA6),
    (kBrowserForward, "kBrowserForward", 0xA7),
    (kBrowserRefresh, "kBrowserRefresh", 0xA8),
    (kBrowserStop, "kBrowserStop", 0xA9),
    (kBrowserSearch, "kBrowserSearch", 0xAA),
    (kBrowserFavorites, "kBrowserFavorites", 0xAB),
    (kBrowserHome, "kBrowserHome", 0xAC),
    (kVolumeMute, "kVolumeMute", 0xAD),
    (kVolumeDown, "kVolumeDown", 0xAE),
    (kVolumeUp, "kVolumeUp", 0xAF),
    (kMediaNextTrack, "kMediaNextTrack", 0xB0),
    (kMediaPrevTrack, "kMediaPrevTrack", 0xB1),
    (kMediaStop, "kMediaStop", 0xB2),
    (kMediaPlayPause, "kMediaPlayPause", 0xB3),
    (kLaunchMail, "kLaunchMail", 0xB4),
    (kLaunchMediaSelect, "kLaunchMediaSelect", 0xB5),
    (kLaunchApp1, "kLaunchApp1", 0xB6),
    (kLaunchApp2, "kLaunchApp2", 0xB7),

    // -- OEM keys ---------------------------------------------------------
    (kOEM1, "kOEM1", 0xBA),
    (kOEMPlus, "kOEMPlus", 0xBB),
    (kOEMComma, "kOEMComma", 0xBC),
    (kOEMMinus, "kOEMMinus", 0xBD),
    (kOEMPeriod, "kOEMPeriod", 0xBE),
    (kOEM2, "kOEM2", 0xBF),
    (kOEM3, "kOEM3", 0xC0),
    (kOEM4, "kOEM4", 0xDB),
    (kOEM5, "kOEM5", 0xDC),
    (kOEM6, "kOEM6", 0xDD),
    (kOEM7, "kOEM7", 0xDE),
    (kOEM8, "kOEM8", 0xDF),

    // -- KiriKiri gamepad (VK_PAD*, tvpinputdefs.h) -----------------------
    (kPadLeft, "kPadLeft", 0x1B5),
    (kPadUp, "kPadUp", 0x1B6),
    (kPadRight, "kPadRight", 0x1B7),
    (kPadDown, "kPadDown", 0x1B8),
    (kPad1, "kPad1", 0x1C0),
    (kPad2, "kPad2", 0x1C1),
    (kPad3, "kPad3", 0x1C2),
    (kPad4, "kPad4", 0x1C3),
    (kPad5, "kPad5", 0x1C4),
    (kPad6, "kPad6", 0x1C5),
    (kPad7, "kPad7", 0x1C6),
    (kPad8, "kPad8", 0x1C7),
    (kPad9, "kPad9", 0x1C8),
    (kPad10, "kPad10", 0x1C9),
    (kPadAny, "kPadAny", 0x1DF),

    // -- IME / rare keys --------------------------------------------------
    (kProcessKey, "kProcessKey", 0xE5),
    (kAttn, "kAttn", 0xF6),
    (kCrsel, "kCrsel", 0xF7),
    (kExsel, "kExsel", 0xF8),
    (kEreof, "kEreof", 0xF9),
    (kPlay, "kPlay", 0xFA),
    (kZoom, "kZoom", 0xFB),
    (kNoName, "kNoName", 0xFC),
    (kPa1, "kPa1", 0xFD),
    (kOEMClear, "kOEMClear", 0xFE),
}

/// Coerce a TJS argument to a key code (negative/out-of-range never match).
fn key_code_arg(v: &Value) -> u32 {
    value_as_i64(v) as u32
}

/// Map a TJS argument to a TVP mouse-button index; `None` when out of range.
fn button_index(v: &Value) -> Option<usize> {
    let b = value_as_i64(v);
    if (0..MOUSE_BUTTONS as i64).contains(&b) {
        Some(b as usize)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// `Mouse` natives
// ---------------------------------------------------------------------------

/// `Mouse.getCursorPos([obj])` → `"x,y"` string, or fills `obj.x`/`obj.y`
/// and returns void.
extern "C" fn native_get_cursor_pos(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Mouse.getCursorPos: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let Some(input) = (unsafe { input_state(&ctx) }) else {
        return report_error(out_error, "Mouse.getCursorPos: input state is not attached");
    };
    let (px, py) = input.pointer();
    let (x, y) = (px.round() as i32, py.round() as i32);
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let a = unsafe { args(argv, argc) };
    if let Some(obj) = a.first().filter(|v| v.ty == VAL_OBJECT) {
        let host = resolved.engine();
        let Ok(id) = host.retain_object_arg(obj) else {
            return report_error(out_error, "Mouse.getCursorPos: cannot retain object");
        };
        let fill = host
            .set_member(id.raw_id(), "x", &TjsValue::Integer(i64::from(x)))
            .and_then(|()| host.set_member(id.raw_id(), "y", &TjsValue::Integer(i64::from(y))));
        if let Err(e) = fill {
            return report_error(out_error, &format!("Mouse.getCursorPos: {e}"));
        }
        set_void(out);
    } else {
        set_string_out(&ctx, out, &format!("{x},{y}"));
    }
    0
}

/// `Mouse.getCursorX()` → int.
extern "C" fn native_get_cursor_x(
    engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Mouse.getCursorX: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let Some(input) = (unsafe { input_state(&ctx) }) else {
        return report_error(out_error, "Mouse.getCursorX: input state is not attached");
    };
    let x = input.pointer().0.round() as i32;
    set_int(out, i64::from(x));
    0
}

/// `Mouse.getCursorY()` → int.
extern "C" fn native_get_cursor_y(
    engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Mouse.getCursorY: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let Some(input) = (unsafe { input_state(&ctx) }) else {
        return report_error(out_error, "Mouse.getCursorY: input state is not attached");
    };
    let y = input.pointer().1.round() as i32;
    set_int(out, i64::from(y));
    0
}

/// `Mouse.setCursorPos(x, y)` → void.
extern "C" fn native_set_cursor_pos(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Mouse.setCursorPos requires 2 arguments");
    }
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Mouse.setCursorPos: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let Some(ptr) = (unsafe { ctx.services() }).input() else {
        return report_error(out_error, "Mouse.setCursorPos: input state is not attached");
    };
    // SAFETY: `attach_input` guarantees the pointer stays valid for the
    // binding's lifetime.
    unsafe { &mut *ptr }.warp_pointer(x as f32, y as f32);
    set_void(out);
    0
}

/// `Mouse.getWheelRot()` → int (the classic vertical Y wheel).
extern "C" fn native_get_wheel_rot(
    engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Mouse.getWheelRot: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let Some(input) = (unsafe { input_state(&ctx) }) else {
        return report_error(out_error, "Mouse.getWheelRot: input state is not attached");
    };
    let y = input.wheel().round() as i32;
    set_int(out, i64::from(y));
    0
}

/// `Mouse.getWheelRotX()` → int.
extern "C" fn native_get_wheel_rot_x(
    engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Mouse.getWheelRotX: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let Some(_input) = (unsafe { input_state(&ctx) }) else {
        return report_error(out_error, "Mouse.getWheelRotX: input state is not attached");
    };
    // The generic wheel is vertical-only; the X axis is always zero.
    set_int(out, 0);
    0
}

/// `Mouse.getWheelRotY()` → int.
extern "C" fn native_get_wheel_rot_y(
    engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Mouse.getWheelRotY: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let Some(input) = (unsafe { input_state(&ctx) }) else {
        return report_error(out_error, "Mouse.getWheelRotY: input state is not attached");
    };
    let y = input.wheel().round() as i32;
    set_int(out, i64::from(y));
    0
}

/// `Mouse.getWheelRotZ()` → int.
extern "C" fn native_get_wheel_rot_z(
    engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Mouse.getWheelRotZ: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let Some(_input) = (unsafe { input_state(&ctx) }) else {
        return report_error(out_error, "Mouse.getWheelRotZ: input state is not attached");
    };
    // The generic wheel is vertical-only; the Z axis is always zero.
    set_int(out, 0);
    0
}

/// `Mouse.isVisible()` → bool.
extern "C" fn native_is_visible(
    engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Mouse.isVisible: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let Some(input) = (unsafe { input_state(&ctx) }) else {
        return report_error(out_error, "Mouse.isVisible: input state is not attached");
    };
    set_bool(out, input.pointer_visible);
    0
}

/// `Mouse.setVisible(visible)` → void.
extern "C" fn native_set_visible(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Mouse.setVisible requires 1 argument");
    }
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Mouse.setVisible: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let visible = value_as_bool(&unsafe { args(argv, argc) }[0]);
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let Some(ptr) = (unsafe { ctx.services() }).input() else {
        return report_error(out_error, "Mouse.setVisible: input state is not attached");
    };
    // SAFETY: `attach_input` guarantees the pointer stays valid.
    unsafe { &mut *ptr }.pointer_visible = visible;
    set_void(out);
    0
}

/// `Mouse.getPressed(button)` → bool.
extern "C" fn native_get_pressed(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Mouse.getPressed requires 1 argument");
    }
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Mouse.getPressed: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let Some(input) = (unsafe { input_state(&ctx) }) else {
        return report_error(out_error, "Mouse.getPressed: input state is not attached");
    };
    let pressed = button_index(&a[0])
        .and_then(mouse_button)
        .is_some_and(|button| input.is_button_down(button));
    set_bool(out, pressed);
    0
}

/// `Mouse.getReleased(button)` → bool.
extern "C" fn native_get_released(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Mouse.getReleased requires 1 argument");
    }
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Mouse.getReleased: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let Some(input) = (unsafe { input_state(&ctx) }) else {
        return report_error(out_error, "Mouse.getReleased: input state is not attached");
    };
    let released = button_index(&a[0])
        .and_then(mouse_button)
        .is_some_and(|button| input.is_button_released(button));
    set_bool(out, released);
    0
}

/// `Mouse.getRepeat(button)` → int.
extern "C" fn native_get_repeat(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Mouse.getRepeat requires 1 argument");
    }
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Mouse.getRepeat: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let Some(input) = (unsafe { input_state(&ctx) }) else {
        return report_error(out_error, "Mouse.getRepeat: input state is not attached");
    };
    let repeat = button_index(&a[0])
        .and_then(mouse_button)
        .map_or(0, |button| input.button_hold_frames(button));
    set_int(out, i64::from(repeat));
    0
}

/// `Mouse.getClickCount(button)` → int.
extern "C" fn native_get_click_count(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Mouse.getClickCount requires 1 argument");
    }
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "Mouse.getClickCount: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let Some(input) = (unsafe { input_state(&ctx) }) else {
        return report_error(
            out_error,
            "Mouse.getClickCount: input state is not attached",
        );
    };
    let count = button_index(&a[0]).map_or(0, |index| mouse_click_count(input, index));
    set_int(out, i64::from(count));
    0
}

// ---------------------------------------------------------------------------
// `Key` natives
// ---------------------------------------------------------------------------

/// `Key.getPressed(code)` → bool.
extern "C" fn native_key_get_pressed(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Key.getPressed requires 1 argument");
    }
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Key.getPressed: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let code = key_code_arg(&unsafe { args(argv, argc) }[0]);
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let Some(input) = (unsafe { input_state(&ctx) }) else {
        return report_error(out_error, "Key.getPressed: input state is not attached");
    };
    set_bool(out, vk_pressed(input, code));
    0
}

/// `Key.getReleased(code)` → bool.
extern "C" fn native_key_get_released(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Key.getReleased requires 1 argument");
    }
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Key.getReleased: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let code = key_code_arg(&unsafe { args(argv, argc) }[0]);
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let Some(input) = (unsafe { input_state(&ctx) }) else {
        return report_error(out_error, "Key.getReleased: input state is not attached");
    };
    set_bool(out, vk_released(input, code));
    0
}

/// `Key.getRepeat(code)` → int.
extern "C" fn native_key_get_repeat(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Key.getRepeat requires 1 argument");
    }
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Key.getRepeat: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let code = key_code_arg(&unsafe { args(argv, argc) }[0]);
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let Some(input) = (unsafe { input_state(&ctx) }) else {
        return report_error(out_error, "Key.getRepeat: input state is not attached");
    };
    set_int(out, i64::from(vk_repeat(input, code)));
    0
}

// ---------------------------------------------------------------------------
// `Pad` natives (reference read-only property stub)
// ---------------------------------------------------------------------------

/// Per-object `Pad` payload: the reference's fixed, read-only values.
///
/// The reference `tTJSNI_Pad` delegates to a native editor form; the port owns
/// the (default) values on the instance so a host editor backend can
/// replace them without changing the script surface. There is no process
/// global.
struct PadState {
    text: String,
    file_name: String,
    color: i64,
    visible: bool,
    title: String,
    font_color: i64,
    font_height: i64,
    font_size: i64,
    font_bold: bool,
    font_italic: bool,
    font_underline: bool,
    font_strike_out: bool,
    font_face: String,
    read_only: bool,
    word_wrap: bool,
    opacity: i64,
    show_status_bar: bool,
    show_scroll_bars: bool,
    status_text: String,
    border_style: i64,
    width: i64,
    height: i64,
    top: i64,
    left: i64,
}

impl Default for PadState {
    /// The reference `Pad` defaults (`PadIntf.cpp`).
    fn default() -> Self {
        Self {
            text: String::new(),
            file_name: String::new(),
            color: 0,
            visible: false,
            title: String::new(),
            font_color: 0,
            font_height: 10,
            font_size: 10,
            font_bold: false,
            font_italic: false,
            font_underline: false,
            font_strike_out: false,
            font_face: String::new(),
            read_only: true,
            word_wrap: false,
            opacity: 255,
            show_status_bar: false,
            show_scroll_bars: false,
            status_text: String::new(),
            border_style: 0,
            width: 100,
            height: 100,
            top: 0,
            left: 0,
        }
    }
}

/// Borrow the `Pad` payload for the object a callback was invoked on.
///
/// # Safety
/// `instance` must be the live payload the VM passed to the callback; the
/// dispatcher only routes `Pad` member access to a `Pad` instance.
unsafe fn pad_state(instance: *mut c_void) -> &'static PadState {
    // SAFETY: the caller upholds the live-payload contract.
    unsafe { &*(instance as *const PadState) }
}

extern "C" fn pad_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(PadState::default())) as *mut c_void
}

extern "C" fn pad_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if !instance.is_null() {
        // SAFETY: `instance` came from `pad_create` and is released once.
        drop(unsafe { Box::from_raw(instance as *mut PadState) });
    }
}

/// `new Pad()` constructor: the payload already holds the reference defaults.
extern "C" fn pad_ctor(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void(out);
    0
}

/// The reference accepts and ignores writes to every `Pad` property.
extern "C" fn pad_noop_set(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    0
}

/// Generate a string `Pad` getter reading its per-object field.
macro_rules! pad_get_str {
    ($getter:ident, $field:ident, $name:literal) => {
        extern "C" fn $getter(
            engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            let Some(resolved) = resolve(engine) else {
                return report_error(
                    out_error,
                    concat!("Pad.", $name, ": compat context is not bound"),
                );
            };
            let ctx = resolved.ctx();
            // SAFETY: `instance` is the live `PadState` payload the VM passed to this getter, and the borrow is used only within the call.
            let state = unsafe { pad_state(instance) };
            set_string_out(&ctx, out, &state.$field);
            0
        }
    };
}

/// Generate an integer `Pad` getter reading its per-object field.
macro_rules! pad_get_int {
    ($getter:ident, $field:ident) => {
        extern "C" fn $getter(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is the live `PadState` payload the VM passed to this getter, and the borrow is used only within the call.
            let state = unsafe { pad_state(instance) };
            set_int(out, state.$field);
            0
        }
    };
}

/// Generate a boolean `Pad` getter reading its per-object field.
macro_rules! pad_get_bool {
    ($getter:ident, $field:ident) => {
        extern "C" fn $getter(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is the live `PadState` payload the VM passed to this getter, and the borrow is used only within the call.
            let state = unsafe { pad_state(instance) };
            set_bool(out, state.$field);
            0
        }
    };
}

pad_get_str!(pad_get_text, text, "text");
pad_get_str!(pad_get_file_name, file_name, "fileName");
pad_get_int!(pad_get_color, color);
pad_get_bool!(pad_get_visible, visible);
pad_get_str!(pad_get_title, title, "title");
pad_get_int!(pad_get_font_color, font_color);
pad_get_int!(pad_get_font_height, font_height);
pad_get_int!(pad_get_font_size, font_size);
pad_get_bool!(pad_get_font_bold, font_bold);
pad_get_bool!(pad_get_font_italic, font_italic);
pad_get_bool!(pad_get_font_underline, font_underline);
pad_get_bool!(pad_get_font_strike_out, font_strike_out);
pad_get_str!(pad_get_font_face, font_face, "fontFace");
pad_get_bool!(pad_get_read_only, read_only);
pad_get_bool!(pad_get_word_wrap, word_wrap);
pad_get_int!(pad_get_opacity, opacity);
pad_get_bool!(pad_get_show_status_bar, show_status_bar);
pad_get_bool!(pad_get_show_scroll_bars, show_scroll_bars);
pad_get_str!(pad_get_status_text, status_text, "statusText");
pad_get_int!(pad_get_border_style, border_style);
pad_get_int!(pad_get_width, width);
pad_get_int!(pad_get_height, height);
pad_get_int!(pad_get_top, top);
pad_get_int!(pad_get_left, left);

/// The `Pad` instance property list.
///
/// Written as explicit [`NativeInstancePropertyDef`] literals (rather than a
/// helper closure) so `scripts/native_parity.py` can see every member; the
/// order matches the reference `PadIntf.cpp`.
fn pad_properties() -> Vec<NativeInstancePropertyDef> {
    vec![
        NativeInstancePropertyDef {
            name: "text",
            get: Some(pad_get_text),
            set: Some(pad_noop_set),
        },
        NativeInstancePropertyDef {
            name: "fileName",
            get: Some(pad_get_file_name),
            set: Some(pad_noop_set),
        },
        NativeInstancePropertyDef {
            name: "color",
            get: Some(pad_get_color),
            set: Some(pad_noop_set),
        },
        NativeInstancePropertyDef {
            name: "visible",
            get: Some(pad_get_visible),
            set: Some(pad_noop_set),
        },
        NativeInstancePropertyDef {
            name: "title",
            get: Some(pad_get_title),
            set: Some(pad_noop_set),
        },
        NativeInstancePropertyDef {
            name: "fontColor",
            get: Some(pad_get_font_color),
            set: Some(pad_noop_set),
        },
        NativeInstancePropertyDef {
            name: "fontHeight",
            get: Some(pad_get_font_height),
            set: Some(pad_noop_set),
        },
        NativeInstancePropertyDef {
            name: "fontSize",
            get: Some(pad_get_font_size),
            set: Some(pad_noop_set),
        },
        NativeInstancePropertyDef {
            name: "fontBold",
            get: Some(pad_get_font_bold),
            set: Some(pad_noop_set),
        },
        NativeInstancePropertyDef {
            name: "fontItalic",
            get: Some(pad_get_font_italic),
            set: Some(pad_noop_set),
        },
        NativeInstancePropertyDef {
            name: "fontUnderline",
            get: Some(pad_get_font_underline),
            set: Some(pad_noop_set),
        },
        NativeInstancePropertyDef {
            name: "fontStrikeOut",
            get: Some(pad_get_font_strike_out),
            set: Some(pad_noop_set),
        },
        NativeInstancePropertyDef {
            name: "fontFace",
            get: Some(pad_get_font_face),
            set: Some(pad_noop_set),
        },
        NativeInstancePropertyDef {
            name: "readOnly",
            get: Some(pad_get_read_only),
            set: Some(pad_noop_set),
        },
        NativeInstancePropertyDef {
            name: "wordWrap",
            get: Some(pad_get_word_wrap),
            set: Some(pad_noop_set),
        },
        NativeInstancePropertyDef {
            name: "opacity",
            get: Some(pad_get_opacity),
            set: Some(pad_noop_set),
        },
        NativeInstancePropertyDef {
            name: "showStatusBar",
            get: Some(pad_get_show_status_bar),
            set: Some(pad_noop_set),
        },
        NativeInstancePropertyDef {
            name: "showScrollBars",
            get: Some(pad_get_show_scroll_bars),
            set: Some(pad_noop_set),
        },
        NativeInstancePropertyDef {
            name: "statusText",
            get: Some(pad_get_status_text),
            set: Some(pad_noop_set),
        },
        NativeInstancePropertyDef {
            name: "borderStyle",
            get: Some(pad_get_border_style),
            set: Some(pad_noop_set),
        },
        NativeInstancePropertyDef {
            name: "width",
            get: Some(pad_get_width),
            set: Some(pad_noop_set),
        },
        NativeInstancePropertyDef {
            name: "height",
            get: Some(pad_get_height),
            set: Some(pad_noop_set),
        },
        NativeInstancePropertyDef {
            name: "top",
            get: Some(pad_get_top),
            set: Some(pad_noop_set),
        },
        NativeInstancePropertyDef {
            name: "left",
            get: Some(pad_get_left),
            set: Some(pad_noop_set),
        },
    ]
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the input classes: `Mouse`, `Key` and `Pad`.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_class(&NativeClassBuilder {
        name: "Mouse",
        methods: vec![
            NativeMethodDef {
                name: "getCursorPos",
                f: native_get_cursor_pos,
            },
            NativeMethodDef {
                name: "getCursorX",
                f: native_get_cursor_x,
            },
            NativeMethodDef {
                name: "getCursorY",
                f: native_get_cursor_y,
            },
            NativeMethodDef {
                name: "setCursorPos",
                f: native_set_cursor_pos,
            },
            NativeMethodDef {
                name: "getWheelRot",
                f: native_get_wheel_rot,
            },
            NativeMethodDef {
                name: "getWheelRotX",
                f: native_get_wheel_rot_x,
            },
            NativeMethodDef {
                name: "getWheelRotY",
                f: native_get_wheel_rot_y,
            },
            NativeMethodDef {
                name: "getWheelRotZ",
                f: native_get_wheel_rot_z,
            },
            NativeMethodDef {
                name: "isVisible",
                f: native_is_visible,
            },
            NativeMethodDef {
                name: "setVisible",
                f: native_set_visible,
            },
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
            NativeMethodDef {
                name: "getClickCount",
                f: native_get_click_count,
            },
        ],
        properties: Vec::new(),
    })?;

    ctx.register_class(&NativeClassBuilder {
        name: "Key",
        methods: vec![
            NativeMethodDef {
                name: "getPressed",
                f: native_key_get_pressed,
            },
            NativeMethodDef {
                name: "getReleased",
                f: native_key_get_released,
            },
            NativeMethodDef {
                name: "getRepeat",
                f: native_key_get_repeat,
            },
        ],
        properties: key_properties(),
    })?;

    ctx.register_instance(&NativeInstanceBuilder {
        name: "Pad",
        create: pad_create,
        destroy: pad_destroy,
        invalidate: None,
        methods: vec![NativeInstanceMethodDef {
            name: "Pad",
            f: pad_ctor,
        }],
        properties: pad_properties(),
    })?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Unit tests for the TVP input helpers (no VM needed)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vk_lookup_resolves_mouse_and_pad_any() {
        let mut state = InputState::new();
        state.begin_frame();
        state.set_button(MouseButton::Left, true);
        state.set_key(VK_PAD1, true);
        state.set_key(VK_PADUP, true);
        state.end_frame();

        assert!(vk_pressed(&state, VK_LBUTTON));
        assert!(!vk_pressed(&state, VK_RBUTTON));
        assert!(vk_pressed(&state, VK_PADANY));
        assert_eq!(vk_repeat(&state, VK_PADANY), 1);
        assert!(shift_flags(&state) & TVP_SS_LEFT != 0);
    }

    #[test]
    fn physical_keys_map_to_the_reference_vk_families() {
        // reference/cpp/core/base/ScriptMgnIntf.cpp:180-198
        // reference/cpp/core/environ/cocos2d/CCKeyCodeConv.cpp:33-160
        for (key, vk) in [
            (PhysicalKey::Enter, 0x0D),
            (PhysicalKey::NumpadEnter, 0x0D),
            (PhysicalKey::Escape, 0x1B),
            (PhysicalKey::KeyA, 0x41),
            (PhysicalKey::KeyZ, 0x5A),
            (PhysicalKey::Digit5, 0x35),
            (PhysicalKey::F1, 0x70),
            (PhysicalKey::F12, 0x7B),
            (PhysicalKey::F24, 0x87),
            (PhysicalKey::ArrowUp, 0x26),
            (PhysicalKey::PageDown, 0x22),
            (PhysicalKey::Space, 0x20),
            (PhysicalKey::Numpad5, 0x65),
            (PhysicalKey::NumpadMultiply, 0x6A),
            (PhysicalKey::Semicolon, 0xBA),
            (PhysicalKey::Slash, 0xBF),
            (PhysicalKey::Backquote, 0xC0),
            (PhysicalKey::ShiftLeft, 0x10),
            (PhysicalKey::ShiftRight, 0x10),
            (PhysicalKey::ControlLeft, 0x11),
            (PhysicalKey::AltRight, 0x12),
            (PhysicalKey::SuperLeft, 0x5B),
            (PhysicalKey::ContextMenu, 0x5D),
        ] {
            assert_eq!(vk_from_physical(key), vk, "{key:?}");
        }
    }

    #[test]
    fn vk_queries_resolve_the_physical_keys_that_are_held() {
        // reference/cpp/core/base/ScriptMgnIntf.cpp:180-198
        let mut state = InputState::new();
        state.begin_frame();
        state.set_key(PhysicalKey::KeyA.index(), true);
        state.set_key(PhysicalKey::ShiftRight.index(), true);
        state.set_key(VK_PAD1, true);
        state.end_frame();

        assert!(vk_pressed(&state, 0x41));
        assert!(vk_pressed(&state, VK_SHIFT));
        assert!(vk_pressed(&state, VK_RSHIFT));
        assert!(!vk_pressed(&state, VK_LSHIFT));
        assert!(vk_pressed(&state, VK_PAD1));
        assert!(vk_pressed(&state, VK_PADANY));
        assert!(shift_flags(&state) & TVP_SS_SHIFT != 0);
        assert_eq!(vk_from_index(PhysicalKey::KeyA.index()), 0x41);
    }

    #[test]
    fn unknown_codes_are_false_not_errors() {
        let state = InputState::new();
        assert!(!vk_pressed(&state, 0x9999));
        assert!(!vk_released(&state, 0x9999));
        assert_eq!(vk_repeat(&state, 0x9999), 0);
        assert_eq!(mouse_click_count(&state, 99), 0);
    }

    #[test]
    fn key_code_table_has_reference_values() {
        let find = |name: &str| {
            KEY_CODE_TABLE
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, code)| *code)
        };
        assert_eq!(find("kReturn"), Some(0x0D));
        assert_eq!(find("kLButton"), Some(0x01));
        assert_eq!(find("kPadAny"), Some(0x1DF));
        assert_eq!(find("kNoSuchKey"), None);
    }

    #[test]
    fn mouse_button_indices_map_to_platform_buttons() {
        for index in 0..MOUSE_BUTTONS {
            assert!(mouse_button(index).is_some(), "index {index} is not mapped");
        }
        assert!(mouse_button(MOUSE_BUTTONS).is_none());
    }

    #[test]
    fn tvp_policy_drives_the_generic_click_sequence() {
        let mut state = InputState::new();
        state.set_click_sequence(MOUSE_CLICK_SEQUENCE_FRAMES, MOUSE_CLICK_MAX_MOVE as f32);
        state.begin_frame();
        state.set_pointer(10.0, 10.0);
        state.set_button(MouseButton::Left, true);
        state.end_frame();
        assert_eq!(mouse_click_count(&state, MB_LEFT), 1);

        state.begin_frame();
        state.set_button(MouseButton::Left, false);
        state.set_button(MouseButton::Left, true);
        state.end_frame();
        assert_eq!(mouse_click_count(&state, MB_LEFT), 2);
    }
}
