//! Input native classes: `Mouse`, `Key`, `Pad`.
//!
//! Behavioral reference: `main`'s `crates/tvp-input/src/{lib.rs,mouse.rs,key.rs}`
//! and the reference `Pad` class (`reference/cpp/core/utils/PadIntf.cpp`).
//!
//! The old `tvp-input` crate kept the input state in process globals
//! (`OnceLock<Mutex<Option<Arc<Mutex<InputState>>>>>`). This port keeps the
//! same *behavior* but stores the state in the per-binding dispatch state
//! ([`CompatServices::state`](crate::CompatServices::state)), exactly like
//! `System`/`Debug` do — there is no process global here (doc 08 §3.1).
//!
//! # Frame protocol
//!
//! The host owns the frame loop and feeds the state from the L1 input events:
//!
//! ```text
//! begin_input_frame                       // clears per-frame data
//!   apply_input_event(event) ...          // pointer/button/key/wheel events
//! end_input_frame                         // commits held counters, frame += 1
//! ```
//!
//! [`InputState::begin_frame`] clears the mouse release flags, the wheel
//! accumulation, the key `released` map and `last_pressed`;
//! [`InputState::end_frame`] bumps the held-frame counters and advances the
//! frame index. Both are driven by the host's per-frame body.
//!
//! # Host wiring
//!
//! The L1→L3 event path is [`crate::EventAdapter`]: `map` maps a
//! [`platform::PlatformEvent`] to a TJS-visible event and `dispatch` delivers
//! it. The input *state* side of that path is
//! [`apply_event`] / [`InputState::apply_event`], reachable through the
//! inherent methods on [`crate::EventAdapter`] defined below
//! ([`crate::EventAdapter::begin_input_frame`],
//! [`crate::EventAdapter::apply_input_event`],
//! [`crate::EventAdapter::end_input_frame`]). The eventual `dispatch`
//! implementation in `event.rs` should call the same seam; it lives here
//! because `event.rs` is out of this change's scope.
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
//! reference setters are accepted and ignored.

// This module exposes the full reference input surface (key-code lookup,
// `TVP_SS_*` shift flags, pad codes, the cursor-warp / raw-button state API).
// Several members are consumed by the host input bridge and frame loop that
// land with the migrated host, so they are intentionally present before every
// call site exists — the same "documented seam" policy `system.rs` uses.
#![allow(dead_code)]

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};

use platform::{InputEvent, MouseButton, PlatformEvent};
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
// Gamepad virtual-key codes (reference `VK_PAD*`, tvpinputdefs.h)
// ---------------------------------------------------------------------------

/// First KiriKiri pad virtual-key code (`VK_PAD_FIRST`).
pub const VK_PAD_FIRST: u32 = 0x1B0;
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
/// Last KiriKiri pad virtual-key code (`VK_PAD_LAST`).
pub const VK_PAD_LAST: u32 = 0x1DF;

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
    /// Event is the second click of a double-click.
    pub const TVP_SS_DOUBLE: u32 = 0x40;
    /// Event is an auto-repeat.
    pub const TVP_SS_REPEAT: u32 = 0x80;

    /// Whether any mouse-button bit is set
    /// (`TVPIsAnyMouseButtonPressedInShiftStateFlags`).
    pub fn any_mouse_button_pressed(state: u32) -> bool {
        state & (TVP_SS_LEFT | TVP_SS_RIGHT | TVP_SS_MIDDLE | TVP_SS_DOUBLE) != 0
    }
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
// Input state
// ---------------------------------------------------------------------------

/// Mouse state shared with the host.
#[derive(Debug, Clone)]
pub struct MouseState {
    /// Cursor X (physical window pixels).
    pub x: i32,
    /// Cursor Y (physical window pixels).
    pub y: i32,
    /// Accumulated wheel deltas `(x, y, z)` since the last `begin_frame`.
    pub wheel: (i32, i32, i32),
    /// Per-button held state, indexed by [`MB_LEFT`]..[`MB_X2`].
    pub buttons: [bool; MOUSE_BUTTONS],
    /// Per-button "released this frame" flags, cleared by `begin_frame`.
    pub released: [bool; MOUSE_BUTTONS],
    /// Per-button consecutive frames held, bumped by `end_frame`.
    pub hold: [u32; MOUSE_BUTTONS],
    /// Per-button presses in the current multi-click sequence.
    pub click_count: [u32; MOUSE_BUTTONS],
    /// Frame index of the last press of the sequence (`u64::MAX` = never).
    pub last_click_frame: [u64; MOUSE_BUTTONS],
    /// Cursor position of the last press of the sequence.
    pub last_click_pos: [(i32, i32); MOUSE_BUTTONS],
    /// Cursor visibility.
    pub visible: bool,
    /// Pending OS-cursor warp from `Mouse.setCursorPos`, consumed by the host.
    pub warp_request: Option<(i32, i32)>,
}

impl Default for MouseState {
    fn default() -> Self {
        Self {
            x: 0,
            y: 0,
            wheel: (0, 0, 0),
            buttons: [false; MOUSE_BUTTONS],
            released: [false; MOUSE_BUTTONS],
            hold: [0; MOUSE_BUTTONS],
            click_count: [0; MOUSE_BUTTONS],
            last_click_frame: [u64::MAX; MOUSE_BUTTONS],
            last_click_pos: [(0, 0); MOUSE_BUTTONS],
            visible: true,
            warp_request: None,
        }
    }
}

/// Keyboard state shared with the host, in Windows virtual-key space.
#[derive(Debug, Clone, Default)]
pub struct KeyState {
    /// keycode → consecutive frames held (present while the key is down).
    pub pressed: HashMap<u32, u32>,
    /// keycode → held-frame count at release (one-frame lifetime).
    pub released: HashMap<u32, u32>,
    /// keycode → repeat count for held keys.
    pub repeat: HashMap<u32, u32>,
    /// Keycodes that transitioned to down since the last `begin_frame`.
    pub last_pressed: Vec<u32>,
}

/// The shared input state, stored in
/// [`CompatServices`](crate::CompatServices) dispatch state.
#[derive(Debug, Clone, Default)]
pub struct InputState {
    /// Mouse state.
    pub mouse: MouseState,
    /// Keyboard/gamepad state.
    pub keys: KeyState,
    /// Monotonic frame counter, advanced by [`InputState::end_frame`].
    pub frame: u64,
}

impl InputState {
    /// Create a fresh, empty input state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin a new frame: clear the per-frame mouse release flags, the wheel
    /// deltas, the key `released` map and `last_pressed`.
    pub fn begin_frame(&mut self) {
        self.mouse.released = [false; MOUSE_BUTTONS];
        self.mouse.wheel = (0, 0, 0);
        self.keys.released.clear();
        self.keys.last_pressed.clear();
    }

    /// End the current frame: bump the held counters and advance the frame.
    pub fn end_frame(&mut self) {
        for (code, count) in self.keys.pressed.iter_mut() {
            *count += 1;
            self.keys.repeat.insert(*code, *count);
        }
        for (button, held) in self.mouse.buttons.iter().enumerate() {
            if *held {
                self.mouse.hold[button] += 1;
            }
        }
        self.frame += 1;
    }

    /// Set the absolute cursor position.
    pub fn set_mouse_pos(&mut self, x: i32, y: i32) {
        self.mouse.x = x;
        self.mouse.y = y;
    }

    /// `Mouse.setCursorPos`: move the logical cursor and queue a host warp.
    pub fn warp_mouse_pos(&mut self, x: i32, y: i32) {
        self.mouse.x = x;
        self.mouse.y = y;
        self.mouse.warp_request = Some((x, y));
    }

    /// Take (and clear) the pending OS-cursor warp, if any.
    pub fn take_mouse_warp(&mut self) -> Option<(i32, i32)> {
        self.mouse.warp_request.take()
    }

    /// Accumulate wheel deltas for this frame.
    pub fn add_wheel(&mut self, x: i32, y: i32, z: i32) {
        self.mouse.wheel.0 += x;
        self.mouse.wheel.1 += y;
        self.mouse.wheel.2 += z;
    }

    /// Set a mouse button's held state, advancing the multi-click sequence on
    /// a rising edge. Out-of-range indices are ignored (returns false).
    pub fn set_mouse_button(&mut self, button: usize, down: bool) -> bool {
        if button >= MOUSE_BUTTONS {
            return false;
        }
        let was_down = self.mouse.buttons[button];
        if down == was_down {
            return true;
        }
        self.mouse.buttons[button] = down;
        if down {
            self.mouse.hold[button] = 0;
            let last_frame = self.mouse.last_click_frame[button];
            let (lx, ly) = self.mouse.last_click_pos[button];
            let close_in_time = last_frame != u64::MAX
                && self.frame.saturating_sub(last_frame) <= MOUSE_CLICK_SEQUENCE_FRAMES;
            let close_in_space = (self.mouse.x - lx).abs() <= MOUSE_CLICK_MAX_MOVE
                && (self.mouse.y - ly).abs() <= MOUSE_CLICK_MAX_MOVE;
            if close_in_time && close_in_space {
                self.mouse.click_count[button] = self.mouse.click_count[button].saturating_add(1);
            } else {
                self.mouse.click_count[button] = 1;
            }
            self.mouse.last_click_frame[button] = self.frame;
            self.mouse.last_click_pos[button] = (self.mouse.x, self.mouse.y);
        } else {
            self.mouse.released[button] = true;
            self.mouse.hold[button] = 0;
        }
        true
    }

    /// Set a mouse button's held state **without** registering a click (the
    /// touch-drag gesture). Returns false for an out-of-range index.
    pub fn set_mouse_button_raw(&mut self, button: usize, down: bool) -> bool {
        if button >= MOUSE_BUTTONS {
            return false;
        }
        let was_down = self.mouse.buttons[button];
        if down == was_down {
            return true;
        }
        self.mouse.buttons[button] = down;
        if down {
            self.mouse.hold[button] = 0;
        } else {
            self.mouse.released[button] = true;
            self.mouse.hold[button] = 0;
        }
        true
    }

    /// Forget the current multi-click sequence for `button`.
    pub fn clear_click_sequence(&mut self, button: usize) {
        if button >= MOUSE_BUTTONS {
            return;
        }
        self.mouse.click_count[button] = 0;
        self.mouse.last_click_frame[button] = u64::MAX;
    }

    /// Set the cursor visibility.
    pub fn set_mouse_visible(&mut self, visible: bool) {
        self.mouse.visible = visible;
    }

    /// Record that `code` went down this frame (a repeated down is a no-op).
    pub fn set_key_down(&mut self, code: u32) {
        if self.keys.pressed.contains_key(&code) {
            return;
        }
        self.keys.pressed.insert(code, 0);
        self.keys.last_pressed.push(code);
    }

    /// Record that `code` went up this frame.
    pub fn set_key_up(&mut self, code: u32) {
        if let Some(held) = self.keys.pressed.remove(&code) {
            self.keys.released.insert(code, held);
        }
        self.keys.repeat.remove(&code);
    }

    /// Whether `code` is currently held.
    pub fn is_key_down(&self, code: u32) -> bool {
        self.keys.pressed.contains_key(&code)
    }

    /// Whether `code` was released this frame.
    pub fn is_key_released(&self, code: u32) -> bool {
        self.keys.released.contains_key(&code)
    }

    /// Frames `code` has been held (0 when not held).
    pub fn key_repeat(&self, code: u32) -> u32 {
        self.keys.repeat.get(&code).copied().unwrap_or(0)
    }

    /// Whether `button` is currently held.
    pub fn is_mouse_button_down(&self, button: usize) -> bool {
        self.mouse.buttons.get(button).copied().unwrap_or(false)
    }

    /// Whether `button` was released this frame.
    pub fn is_mouse_button_released(&self, button: usize) -> bool {
        self.mouse.released.get(button).copied().unwrap_or(false)
    }

    /// Frames `button` has been held (0 for an out-of-range button).
    pub fn mouse_button_repeat(&self, button: usize) -> u32 {
        if !self.mouse.buttons.get(button).copied().unwrap_or(false) {
            return 0;
        }
        self.mouse.hold.get(button).copied().unwrap_or(0)
    }

    /// Whether virtual-key `code` is held: mouse-button VKs resolve to the
    /// mouse state, [`VK_PADANY`] is true while any pad button is held.
    pub fn vk_pressed(&self, code: u32) -> bool {
        if let Some(b) = mouse_button_from_vk(code) {
            return self.mouse.buttons[b];
        }
        if code == VK_PADANY {
            return PAD_CODES
                .iter()
                .any(|&c| self.keys.pressed.contains_key(&c));
        }
        self.keys.pressed.contains_key(&code)
    }

    /// Whether virtual-key `code` was released this frame (same mapping as
    /// [`InputState::vk_pressed`]).
    pub fn vk_released(&self, code: u32) -> bool {
        if let Some(b) = mouse_button_from_vk(code) {
            return self.mouse.released[b];
        }
        if code == VK_PADANY {
            return PAD_CODES
                .iter()
                .any(|&c| self.keys.released.contains_key(&c));
        }
        self.keys.released.contains_key(&code)
    }

    /// Held-frame count for virtual-key `code` (`VK_PADANY` = longest pad).
    pub fn vk_repeat(&self, code: u32) -> u32 {
        if let Some(b) = mouse_button_from_vk(code) {
            return self.mouse_button_repeat(b);
        }
        if code == VK_PADANY {
            return PAD_CODES
                .iter()
                .map(|&c| self.keys.repeat.get(&c).copied().unwrap_or(0))
                .max()
                .unwrap_or(0);
        }
        self.keys.repeat.get(&code).copied().unwrap_or(0)
    }

    /// The reference `TVP_SS_*` shift mask for the currently held state.
    pub fn shift_flags(&self) -> u32 {
        let mut flags = 0;
        if self.vk_pressed(VK_SHIFT) {
            flags |= TVP_SS_SHIFT;
        }
        if self.vk_pressed(VK_MENU) {
            flags |= TVP_SS_ALT;
        }
        if self.vk_pressed(VK_CONTROL) {
            flags |= TVP_SS_CTRL;
        }
        if self.is_mouse_button_down(MB_LEFT) {
            flags |= TVP_SS_LEFT;
        }
        if self.is_mouse_button_down(MB_RIGHT) {
            flags |= TVP_SS_RIGHT;
        }
        if self.is_mouse_button_down(MB_MIDDLE) {
            flags |= TVP_SS_MIDDLE;
        }
        flags
    }

    /// Presses in the current multi-click sequence for `button`
    /// (`Mouse.getClickCount`); 0 once the sequence times out.
    pub fn mouse_click_count(&self, button: usize) -> u32 {
        if button >= MOUSE_BUTTONS {
            return 0;
        }
        let last = self.mouse.last_click_frame[button];
        if last == u64::MAX || self.frame.saturating_sub(last) > MOUSE_CLICK_SEQUENCE_FRAMES {
            0
        } else {
            self.mouse.click_count[button]
        }
    }

    /// Apply one raw L1 platform event to the state.
    pub fn apply_event(&mut self, event: &PlatformEvent) {
        apply_event(self, event);
    }

    /// Apply one raw L1 input event to the state.
    pub fn apply_input(&mut self, event: &InputEvent) {
        apply_input(self, event);
    }
}

/// Round a logical coordinate to the TJS integer the event carries.
fn coordinate(value: f32) -> i32 {
    value.round() as i32
}

/// The TVP mouse-button index for a platform button, if it has one.
fn platform_button_index(button: MouseButton) -> Option<usize> {
    match button {
        MouseButton::Left => Some(MB_LEFT),
        MouseButton::Right => Some(MB_RIGHT),
        MouseButton::Middle => Some(MB_MIDDLE),
        MouseButton::Back => Some(MB_X1),
        MouseButton::Forward => Some(MB_X2),
        // `MouseButton` is `#[non_exhaustive]`; an unknown/raw button has no
        // TVP index.
        _ => None,
    }
}

/// Apply one raw input event (the state side of [`crate::EventAdapter`]).
///
/// The platform key `code` is used directly as the Windows virtual-key code;
/// the final physical-key → `VK_*` mapping belongs to the L1 host bridge,
/// consistent with [`crate::EventAdapter`], which forwards `code` unchanged.
pub fn apply_input(state: &mut InputState, event: &InputEvent) {
    match event {
        InputEvent::PointerMoved { x, y } => {
            state.set_mouse_pos(coordinate(*x), coordinate(*y));
        }
        InputEvent::MouseButton { button, pressed } => {
            if let Some(index) = platform_button_index(*button) {
                state.set_mouse_button(index, *pressed);
            }
        }
        InputEvent::Wheel { delta } => {
            state.add_wheel(0, coordinate(*delta), 0);
        }
        InputEvent::Key { code, pressed } => {
            if *pressed {
                state.set_key_down(*code);
            } else {
                state.set_key_up(*code);
            }
        }
        // Text/IME, touch and every future event are ignored by the state
        // until their handlers land (`InputEvent` is `#[non_exhaustive]`).
        _ => {}
    }
}

/// Apply one raw L1 platform event to the input state, ignoring the
/// lifecycle events the input surface does not track.
pub fn apply_event(state: &mut InputState, event: &PlatformEvent) {
    if let PlatformEvent::Input(input) = event {
        apply_input(state, input);
    }
}

// ---------------------------------------------------------------------------
// Host-facing seam (see the module docs)
// ---------------------------------------------------------------------------

impl crate::EventAdapter {
    /// Clear the per-frame input state. Call once at the top of the frame,
    /// before draining events.
    pub fn begin_input_frame(&self, ctx: &mut CompatCtx) {
        // SAFETY: a `CompatCtx` is only created for a bound, live engine.
        let services = unsafe { ctx.services() };
        services.state::<InputState>().begin_frame();
    }

    /// Apply one raw L1 platform event to the script-visible input state.
    pub fn apply_input_event(&self, ctx: &mut CompatCtx, event: &PlatformEvent) {
        // SAFETY: a `CompatCtx` is only created for a bound, live engine.
        let services = unsafe { ctx.services() };
        let mut state = services.state::<InputState>();
        apply_event(&mut state, event);
    }

    /// Commit the input frame: bump the held counters and advance the frame
    /// index. Call after draining events.
    pub fn end_input_frame(&self, ctx: &mut CompatCtx) {
        // SAFETY: a `CompatCtx` is only created for a bound, live engine.
        let services = unsafe { ctx.services() };
        services.state::<InputState>().end_frame();
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

        /// Look up the Windows virtual-key code for a TJS `Key.kXXX` name.
        pub fn key_code(name: &str) -> Option<u32> {
            KEY_CODE_TABLE
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, code)| *code)
        }

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
    let services = unsafe { ctx.services() };
    let (x, y) = {
        let state = services.state::<InputState>();
        (state.mouse.x, state.mouse.y)
    };
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
    let x = unsafe { ctx.services() }.state::<InputState>().mouse.x;
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
    let y = unsafe { ctx.services() }.state::<InputState>().mouse.y;
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
    let a = unsafe { args(argv, argc) };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    unsafe { ctx.services() }
        .state::<InputState>()
        .warp_mouse_pos(x, y);
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
    let y = unsafe { ctx.services() }
        .state::<InputState>()
        .mouse
        .wheel
        .1;
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
    let x = unsafe { ctx.services() }
        .state::<InputState>()
        .mouse
        .wheel
        .0;
    set_int(out, i64::from(x));
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
    let y = unsafe { ctx.services() }
        .state::<InputState>()
        .mouse
        .wheel
        .1;
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
    let z = unsafe { ctx.services() }
        .state::<InputState>()
        .mouse
        .wheel
        .2;
    set_int(out, i64::from(z));
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
    let visible = unsafe { ctx.services() }
        .state::<InputState>()
        .mouse
        .visible;
    set_bool(out, visible);
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
    let visible = value_as_bool(&unsafe { args(argv, argc) }[0]);
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    unsafe { ctx.services() }
        .state::<InputState>()
        .set_mouse_visible(visible);
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
    let a = unsafe { args(argv, argc) };
    let pressed = {
        let state = unsafe { ctx.services() }.state::<InputState>();
        button_index(&a[0]).is_some_and(|b| state.mouse.buttons[b])
    };
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
    let a = unsafe { args(argv, argc) };
    let released = {
        let state = unsafe { ctx.services() }.state::<InputState>();
        button_index(&a[0]).is_some_and(|b| state.mouse.released[b])
    };
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
    let a = unsafe { args(argv, argc) };
    let repeat = {
        let state = unsafe { ctx.services() }.state::<InputState>();
        button_index(&a[0]).map_or(0, |b| state.mouse_button_repeat(b))
    };
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
    let a = unsafe { args(argv, argc) };
    let count = {
        let state = unsafe { ctx.services() }.state::<InputState>();
        button_index(&a[0]).map_or(0, |b| state.mouse_click_count(b))
    };
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
    let code = key_code_arg(&unsafe { args(argv, argc) }[0]);
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let pressed = unsafe { ctx.services() }
        .state::<InputState>()
        .vk_pressed(code);
    set_bool(out, pressed);
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
    let code = key_code_arg(&unsafe { args(argv, argc) }[0]);
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let released = unsafe { ctx.services() }
        .state::<InputState>()
        .vk_released(code);
    set_bool(out, released);
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
    let code = key_code_arg(&unsafe { args(argv, argc) }[0]);
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let repeat = unsafe { ctx.services() }
        .state::<InputState>()
        .vk_repeat(code);
    set_int(out, i64::from(repeat));
    0
}

// ---------------------------------------------------------------------------
// `Pad` natives (reference read-only property stub)
// ---------------------------------------------------------------------------

/// Per-object `Pad` payload (the reference `tTJSNI_Pad` carries no data
/// either; the class is a fixed read-only stub).
#[derive(Default)]
struct PadState {
    /// Keeps the allocation non-zero-sized.
    _private: u8,
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

/// `new Pad()` constructor: no state to initialize.
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

/// Generate an integer/bool `Pad` property getter with a fixed value.
macro_rules! pad_get_int {
    ($getter:ident, $value:expr) => {
        extern "C" fn $getter(
            _engine: *mut c_void,
            _instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            set_int(out, $value);
            0
        }
    };
}

/// Generate a string `Pad` property getter with a fixed value.
macro_rules! pad_get_str {
    ($getter:ident, $value:literal) => {
        extern "C" fn $getter(
            engine: *mut c_void,
            _instance: *mut c_void,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            let Some(resolved) = resolve(engine) else {
                return report_error(out_error, "Pad: compat context is not bound");
            };
            let ctx = resolved.ctx();
            set_string_out(&ctx, out, $value);
            0
        }
    };
}

pad_get_str!(pad_get_text, "");
pad_get_str!(pad_get_file_name, "");
pad_get_int!(pad_get_color, 0);
pad_get_int!(pad_get_visible, 0);
pad_get_str!(pad_get_title, "");
pad_get_int!(pad_get_font_color, 0);
pad_get_int!(pad_get_font_height, 10);
pad_get_int!(pad_get_font_size, 10);
pad_get_int!(pad_get_font_bold, 0);
pad_get_int!(pad_get_font_italic, 0);
pad_get_int!(pad_get_font_underline, 0);
pad_get_int!(pad_get_font_strike_out, 0);
pad_get_str!(pad_get_font_face, "");
pad_get_int!(pad_get_read_only, 1);
pad_get_int!(pad_get_word_wrap, 0);
pad_get_int!(pad_get_opacity, 255);
pad_get_int!(pad_get_show_status_bar, 0);
pad_get_int!(pad_get_show_scroll_bars, 0);
pad_get_str!(pad_get_status_text, "");
pad_get_int!(pad_get_border_style, 0);
pad_get_int!(pad_get_width, 100);
pad_get_int!(pad_get_height, 100);
pad_get_int!(pad_get_top, 0);
pad_get_int!(pad_get_left, 0);

/// The `Pad` instance property list.
fn pad_properties() -> Vec<NativeInstancePropertyDef> {
    let mut props = Vec::new();
    let mut push = |name: &'static str, get| {
        props.push(NativeInstancePropertyDef {
            name,
            get: Some(get),
            set: Some(pad_noop_set),
        });
    };
    push("text", pad_get_text);
    push("fileName", pad_get_file_name);
    push("color", pad_get_color);
    push("visible", pad_get_visible);
    push("title", pad_get_title);
    push("fontColor", pad_get_font_color);
    push("fontHeight", pad_get_font_height);
    push("fontSize", pad_get_font_size);
    push("fontBold", pad_get_font_bold);
    push("fontItalic", pad_get_font_italic);
    push("fontUnderline", pad_get_font_underline);
    push("fontStrikeOut", pad_get_font_strike_out);
    push("fontFace", pad_get_font_face);
    push("readOnly", pad_get_read_only);
    push("wordWrap", pad_get_word_wrap);
    push("opacity", pad_get_opacity);
    push("showStatusBar", pad_get_show_status_bar);
    push("showScrollBars", pad_get_show_scroll_bars);
    push("statusText", pad_get_status_text);
    push("borderStyle", pad_get_border_style);
    push("width", pad_get_width);
    push("height", pad_get_height);
    push("top", pad_get_top);
    push("left", pad_get_left);
    props
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
// Unit tests for the pure state / event mapping (no VM needed)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_event_maps_pointer_button_wheel_and_key() {
        let mut state = InputState::new();
        state.begin_frame();
        state.apply_event(&PlatformEvent::Input(InputEvent::PointerMoved {
            x: 3.4,
            y: -2.6,
        }));
        state.apply_event(&PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        }));
        state.apply_event(&PlatformEvent::Input(InputEvent::Wheel { delta: 120.0 }));
        state.apply_event(&PlatformEvent::Input(InputEvent::Key {
            code: 0x0D,
            pressed: true,
        }));
        state.end_frame();

        assert_eq!((state.mouse.x, state.mouse.y), (3, -3));
        assert!(state.is_mouse_button_down(MB_LEFT));
        assert_eq!(state.mouse.wheel, (0, 120, 0));
        assert!(state.is_key_down(0x0D));
        assert_eq!(state.key_repeat(0x0D), 1);

        // A release is visible for exactly one frame.
        state.begin_frame();
        state.apply_event(&PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: false,
        }));
        state.apply_event(&PlatformEvent::Input(InputEvent::Key {
            code: 0x0D,
            pressed: false,
        }));
        state.end_frame();
        assert!(state.is_mouse_button_released(MB_LEFT));
        assert!(state.is_key_released(0x0D));

        state.begin_frame();
        state.end_frame();
        assert!(!state.is_mouse_button_released(MB_LEFT));
        assert!(!state.is_key_released(0x0D));
    }

    #[test]
    fn lifecycle_and_unknown_events_are_ignored() {
        let mut state = InputState::new();
        state.apply_event(&PlatformEvent::Resized(1, 2));
        state.apply_event(&PlatformEvent::Focused(true));
        state.apply_event(&PlatformEvent::CloseRequested);
        assert_eq!(state.frame, 0);
        assert!(state.mouse.visible);
    }

    #[test]
    fn click_sequence_counts_and_times_out() {
        let mut state = InputState::new();
        state.begin_frame();
        state.set_mouse_pos(10, 10);
        state.set_mouse_button(MB_LEFT, true);
        state.end_frame();
        assert_eq!(state.mouse_click_count(MB_LEFT), 1);

        state.begin_frame();
        state.set_mouse_button(MB_LEFT, false);
        state.set_mouse_button(MB_LEFT, true);
        state.end_frame();
        assert_eq!(state.mouse_click_count(MB_LEFT), 2);

        // Move far: a new sequence.
        state.begin_frame();
        state.set_mouse_button(MB_LEFT, false);
        state.set_mouse_pos(500, 500);
        state.set_mouse_button(MB_LEFT, true);
        state.end_frame();
        assert_eq!(state.mouse_click_count(MB_LEFT), 1);

        for _ in 0..=MOUSE_CLICK_SEQUENCE_FRAMES {
            state.begin_frame();
            state.end_frame();
        }
        assert_eq!(state.mouse_click_count(MB_LEFT), 0);
    }

    #[test]
    fn vk_lookup_resolves_mouse_and_pad_any() {
        let mut state = InputState::new();
        state.begin_frame();
        state.set_mouse_button(MB_LEFT, true);
        state.set_key_down(VK_PAD1);
        state.set_key_down(VK_PADUP);
        state.end_frame();

        assert!(state.vk_pressed(VK_LBUTTON));
        assert!(!state.vk_pressed(VK_RBUTTON));
        assert!(state.vk_pressed(VK_PADANY));
        assert_eq!(state.vk_repeat(VK_PADANY), 1);
        assert!(state.shift_flags() & TVP_SS_LEFT != 0);
    }

    #[test]
    fn unknown_codes_are_false_not_errors() {
        let state = InputState::new();
        assert!(!state.vk_pressed(0x9999));
        assert!(!state.vk_released(0x9999));
        assert_eq!(state.vk_repeat(0x9999), 0);
        assert_eq!(state.mouse_click_count(99), 0);
    }

    #[test]
    fn key_code_table_has_reference_values() {
        assert_eq!(key_code("kReturn"), Some(0x0D));
        assert_eq!(key_code("kLButton"), Some(0x01));
        assert_eq!(key_code("kPadAny"), Some(0x1DF));
        assert_eq!(key_code("kNoSuchKey"), None);
    }
}
