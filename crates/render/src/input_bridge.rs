//! Input bridge: Bevy input events → shared `tvp-input` state → the game
//! window's script `onMouseDown`/`onKeyDown`/... methods.
//!
//! This is the "later wave" the `tvp-input` crate docs promise: the app
//! drives the process-global [`InputState`] with the documented frame
//! protocol each frame, and then diffuses the state into the game's script
//! objects so the title screen's buttons become clickable.
//!
//! Two `Update` systems, chained and ordered *after* [`crate::run_vm`] (see
//! `main.rs`) so they never touch the single-threaded TJS VM concurrently:
//!
//! * [`capture_input`] drains Bevy's `ButtonInput<MouseButton>`,
//!   `ButtonInput<KeyCode>`, `CursorMoved` and `MouseWheel` events into the
//!   shared `tvp_input::InputState` (`begin_frame` → `set_*` → `end_frame`).
//! * [`dispatch_input`] computes this frame's edges (button press/release,
//!   key down/up, cursor movement, wheel) against the previous frame's
//!   snapshot ([`BridgeState`]) and calls the matching script methods on
//!   every registered scene window: `onMouseDown(x, y, button, shift)`,
//!   `onMouseUp(x, y, button, shift)`, `onMouseMove(x, y, shift)`,
//!   `onMouseWheel(shift, delta, x, y)`, `onKeyDown(key, shift)`,
//!   `onKeyUp(key, shift)` — exactly the methods `system/window.tjs`'s
//!   `MainWindow` uses to feed `dispatchInputNotify`.
//!
//! The window's script object is looked up via
//! [`tvp_visual::natives::window_tjs_object`], retained with
//! `Tjs2Engine::retain_object_detached` and invoked this-bound with
//! `Tjs2Engine::call_member`. Missing windows/methods are logged and
//! skipped — never panics.

use std::collections::HashSet;

use bevy::ecs::message::MessageReader;
use bevy::input::ButtonInput;
use bevy::input::keyboard::KeyCode;
use bevy::input::mouse::{MouseButton, MouseWheel};
use bevy::prelude::{Res, ResMut, Resource};
use bevy::window::{CursorMoved, Window};
use tjs2_sys::{Tjs2Engine, Tjs2ValueId, TjsValue};
use tvp_input::{InputState, MB_LEFT, MB_MIDDLE, MB_RIGHT, MB_X1, MB_X2, MOUSE_BUTTONS};
use tvp_visual::scene::Scene;

use krkr_render::sync::SharedScene;

use crate::VmRuntime;

/// Fallback game resolution (the title screen's native size) used when the
/// scene has no window yet.
const DEFAULT_GAME_SIZE: (u32, u32) = (1280, 720);

/// Windows virtual-key codes the bridge uses for the shift flags (the same
/// codes as `reference/cpp/core/environ/vkdefine.h`).
const VK_SHIFT: u32 = 0x10;
const VK_CONTROL: u32 = 0x11;
const VK_MENU: u32 = 0x12;

/// Bevy mouse button → TVP button index (`mbLeft`..`mbX2`, the values the
/// game passes around, e.g. `onMouseDown(x, y, mbLeft, 0)`).
const BUTTON_MAP: [(MouseButton, usize); 5] = [
    (MouseButton::Left, MB_LEFT),
    (MouseButton::Right, MB_RIGHT),
    (MouseButton::Middle, MB_MIDDLE),
    (MouseButton::Back, MB_X1),
    (MouseButton::Forward, MB_X2),
];

/// Previous-frame snapshot the bridge diffuses against, so button/key
/// **edges** can be recovered from the persistent held state the
/// `tvp-input` frame protocol keeps.
#[derive(Resource, Default)]
pub(crate) struct BridgeState {
    /// Mouse buttons held at the end of the last dispatched frame.
    prev_buttons: [bool; MOUSE_BUTTONS],
    /// Key codes held at the end of the last dispatched frame.
    prev_keys: HashSet<u32>,
    /// Cursor position the last `onMouseMove` was dispatched for.
    last_move_pos: (i32, i32),
}

/// The input events one frame produced, ready to dispatch to windows.
pub(crate) struct FrameEvents {
    /// TVP button indices pressed this frame (`onMouseDown`).
    pub(crate) button_down: Vec<usize>,
    /// TVP button indices released this frame (`onMouseUp`).
    pub(crate) button_up: Vec<usize>,
    /// Key codes (VK) pressed this frame (`onKeyDown`).
    pub(crate) keys_down: Vec<u32>,
    /// Key codes (VK) released this frame (`onKeyUp`).
    pub(crate) keys_up: Vec<u32>,
    /// The new cursor position, when it moved this frame (`onMouseMove`).
    pub(crate) moved: Option<(i32, i32)>,
    /// The current cursor position (for click/wheel handlers).
    pub(crate) position: (i32, i32),
    /// The vertical wheel delta this frame (`onMouseWheel`).
    pub(crate) wheel: Option<i32>,
    /// The `ss*` shift flag mask (keyboard modifiers + held mouse buttons).
    pub(crate) shift: i32,
}

impl FrameEvents {
    fn is_empty(&self) -> bool {
        self.button_down.is_empty()
            && self.button_up.is_empty()
            && self.keys_down.is_empty()
            && self.keys_up.is_empty()
            && self.moved.is_none()
            && self.wheel.is_none()
    }
}

// ---------------------------------------------------------------------------
// System 1: Bevy events → tvp-input state (the frame protocol)
// ---------------------------------------------------------------------------

/// Drain Bevy input into the shared `tvp_input::InputState` for this frame.
///
/// Runs before [`dispatch_input`] (chained in `main.rs`) and after
/// `run_vm`, so scripts never see input mid-poll. Cursor positions are
/// mapped from the OS window into game (primary-layer) coordinates — the
/// window is currently 1:1 with the game (1280x720), but a WM-scaled window
/// is handled by scaling through the window's logical size.
pub(crate) fn capture_input(
    mouse_input: Res<ButtonInput<MouseButton>>,
    keyboard: Res<ButtonInput<KeyCode>>,
    mut cursor_moved: MessageReader<CursorMoved>,
    mut wheel_events: MessageReader<MouseWheel>,
    windows: bevy::prelude::Query<&Window>,
    shared: Res<SharedScene>,
) {
    let (game_w, game_h) = game_size(&shared);
    let (scale_x, scale_y) = match windows.iter().next() {
        Some(w) if w.width() > 0.0 && w.height() > 0.0 => (
            f64::from(game_w) / f64::from(w.width()),
            f64::from(game_h) / f64::from(w.height()),
        ),
        _ => (1.0, 1.0),
    };

    let state = tvp_input::input_state();
    let mut s = state.lock().unwrap_or_else(|p| p.into_inner());
    s.begin_frame();

    // Cursor position: the last CursorMoved of the frame, clamped to the
    // game bounds.
    if let Some(pos) = cursor_moved.read().last() {
        let x = ((pos.position.x * scale_x as f32).round() as i32)
            .clamp(0, game_w.saturating_sub(1) as i32);
        let y = ((pos.position.y * scale_y as f32).round() as i32)
            .clamp(0, game_h.saturating_sub(1) as i32);
        s.set_mouse_pos(x, y);
    }

    // Mouse buttons: press/release edges only (held state persists in the
    // state across frames).
    for (bevy_button, tvp_button) in BUTTON_MAP {
        if mouse_input.just_pressed(bevy_button) {
            s.set_mouse_button(tvp_button, true);
        } else if mouse_input.just_released(bevy_button) {
            s.set_mouse_button(tvp_button, false);
        }
    }

    // Keyboard: down/up edges, mapped to Windows VK codes (what the game's
    // onKeyDown compares against VK_*).
    for code in keyboard.get_just_pressed() {
        if let Some(vk) = bevy_key_to_vk(*code) {
            s.set_key_down(vk);
            tvp_natives::set_key_state(vk, true);
        }
    }
    for code in keyboard.get_just_released() {
        if let Some(vk) = bevy_key_to_vk(*code) {
            s.set_key_up(vk);
            tvp_natives::set_key_state(vk, false);
        }
    }

    // Wheel: Bevy deltas are in "lines" (typically ±1 per notch), TVP's
    // Mouse.getWheelRot is in "notches × 120" like the Win32 WHEEL_DELTA.
    let mut wheel = (0i32, 0i32, 0i32);
    for ev in wheel_events.read() {
        wheel.0 += (ev.x * 120.0).round() as i32;
        wheel.1 += (ev.y * 120.0).round() as i32;
    }
    if wheel != (0, 0, 0) {
        s.add_wheel(wheel.0, wheel.1, wheel.2);
    }

    s.end_frame();
}

/// The game resolution to map cursor positions into: the first scene
/// window's inner (logical client) size, falling back to the title size.
fn game_size(shared: &SharedScene) -> (u32, u32) {
    let scene = shared.0.read().expect("shared scene lock poisoned");
    scene
        .windows
        .first()
        .map(|w| w.inner_size)
        .unwrap_or(DEFAULT_GAME_SIZE)
}

/// Map a Bevy [`KeyCode`] to the Windows virtual-key code the game's
/// `onKeyDown` compares against (`VK_*` from
/// `reference/cpp/core/environ/vkdefine.h`). `None` for keys without a
/// sensible VK mapping.
fn bevy_key_to_vk(key: KeyCode) -> Option<u32> {
    let vk = match key {
        // letters
        KeyCode::KeyA => 0x41,
        KeyCode::KeyB => 0x42,
        KeyCode::KeyC => 0x43,
        KeyCode::KeyD => 0x44,
        KeyCode::KeyE => 0x45,
        KeyCode::KeyF => 0x46,
        KeyCode::KeyG => 0x47,
        KeyCode::KeyH => 0x48,
        KeyCode::KeyI => 0x49,
        KeyCode::KeyJ => 0x4A,
        KeyCode::KeyK => 0x4B,
        KeyCode::KeyL => 0x4C,
        KeyCode::KeyM => 0x4D,
        KeyCode::KeyN => 0x4E,
        KeyCode::KeyO => 0x4F,
        KeyCode::KeyP => 0x50,
        KeyCode::KeyQ => 0x51,
        KeyCode::KeyR => 0x52,
        KeyCode::KeyS => 0x53,
        KeyCode::KeyT => 0x54,
        KeyCode::KeyU => 0x55,
        KeyCode::KeyV => 0x56,
        KeyCode::KeyW => 0x57,
        KeyCode::KeyX => 0x58,
        KeyCode::KeyY => 0x59,
        KeyCode::KeyZ => 0x5A,
        // digits
        KeyCode::Digit0 => 0x30,
        KeyCode::Digit1 => 0x31,
        KeyCode::Digit2 => 0x32,
        KeyCode::Digit3 => 0x33,
        KeyCode::Digit4 => 0x34,
        KeyCode::Digit5 => 0x35,
        KeyCode::Digit6 => 0x36,
        KeyCode::Digit7 => 0x37,
        KeyCode::Digit8 => 0x38,
        KeyCode::Digit9 => 0x39,
        // function keys
        KeyCode::F1 => 0x70,
        KeyCode::F2 => 0x71,
        KeyCode::F3 => 0x72,
        KeyCode::F4 => 0x73,
        KeyCode::F5 => 0x74,
        KeyCode::F6 => 0x75,
        KeyCode::F7 => 0x76,
        KeyCode::F8 => 0x77,
        KeyCode::F9 => 0x78,
        KeyCode::F10 => 0x79,
        KeyCode::F11 => 0x7A,
        KeyCode::F12 => 0x7B,
        KeyCode::F13 => 0x7C,
        KeyCode::F14 => 0x7D,
        KeyCode::F15 => 0x7E,
        KeyCode::F16 => 0x7F,
        KeyCode::F17 => 0x80,
        KeyCode::F18 => 0x81,
        KeyCode::F19 => 0x82,
        KeyCode::F20 => 0x83,
        KeyCode::F21 => 0x84,
        KeyCode::F22 => 0x85,
        KeyCode::F23 => 0x86,
        KeyCode::F24 => 0x87,
        // navigation / control
        KeyCode::Enter => 0x0D,
        KeyCode::NumpadEnter => 0x0D,
        KeyCode::Escape => 0x1B,
        KeyCode::Space => 0x20,
        KeyCode::Tab => 0x09,
        KeyCode::Backspace => 0x08,
        KeyCode::Delete => 0x2E,
        KeyCode::Insert => 0x2D,
        KeyCode::Home => 0x24,
        KeyCode::End => 0x23,
        KeyCode::PageUp => 0x21,
        KeyCode::PageDown => 0x22,
        KeyCode::ArrowLeft => 0x25,
        KeyCode::ArrowUp => 0x26,
        KeyCode::ArrowRight => 0x27,
        KeyCode::ArrowDown => 0x28,
        KeyCode::PrintScreen => 0x2C,
        KeyCode::Pause => 0x13,
        KeyCode::CapsLock => 0x14,
        KeyCode::NumLock => 0x90,
        KeyCode::ScrollLock => 0x91,
        // modifiers (generic VK_* like the reference's Key.kShift/kControl)
        KeyCode::ShiftLeft | KeyCode::ShiftRight => VK_SHIFT,
        KeyCode::ControlLeft | KeyCode::ControlRight => VK_CONTROL,
        KeyCode::AltLeft | KeyCode::AltRight => VK_MENU,
        KeyCode::SuperLeft => 0x5B,   // VK_LWIN
        KeyCode::SuperRight => 0x5C,  // VK_RWIN
        KeyCode::ContextMenu => 0x5D, // VK_APPS
        // numpad
        KeyCode::Numpad0 => 0x60,
        KeyCode::Numpad1 => 0x61,
        KeyCode::Numpad2 => 0x62,
        KeyCode::Numpad3 => 0x63,
        KeyCode::Numpad4 => 0x64,
        KeyCode::Numpad5 => 0x65,
        KeyCode::Numpad6 => 0x66,
        KeyCode::Numpad7 => 0x67,
        KeyCode::Numpad8 => 0x68,
        KeyCode::Numpad9 => 0x69,
        KeyCode::NumpadMultiply => 0x6A, // VK_MULTIPLY
        KeyCode::NumpadAdd => 0x6B,      // VK_ADD
        KeyCode::NumpadComma => 0x6C,    // VK_SEPARATOR
        KeyCode::NumpadSubtract => 0x6D, // VK_SUBTRACT
        KeyCode::NumpadDecimal => 0x6E,  // VK_DECIMAL
        KeyCode::NumpadDivide => 0x6F,   // VK_DIVIDE
        // OEM punctuation (US layout; VK codes match vkdefine.h)
        KeyCode::Semicolon => 0xBA,    // VK_OEM_1
        KeyCode::Equal => 0xBB,        // VK_OEM_PLUS
        KeyCode::Comma => 0xBC,        // VK_OEM_COMMA
        KeyCode::Minus => 0xBD,        // VK_OEM_MINUS
        KeyCode::Period => 0xBE,       // VK_OEM_PERIOD
        KeyCode::Slash => 0xBF,        // VK_OEM_2
        KeyCode::Backquote => 0xC0,    // VK_OEM_3
        KeyCode::BracketLeft => 0xDB,  // VK_OEM_4
        KeyCode::Backslash => 0xDC,    // VK_OEM_5
        KeyCode::BracketRight => 0xDD, // VK_OEM_6
        KeyCode::Quote => 0xDE,        // VK_OEM_7
        // IME keys
        KeyCode::Convert => 0x1C,    // VK_CONVERT
        KeyCode::NonConvert => 0x1D, // VK_NONCONVERT
        KeyCode::KanaMode => 0x15,   // VK_KANA
        // browser / media keys
        KeyCode::BrowserBack => 0xA6,
        KeyCode::BrowserForward => 0xA7,
        KeyCode::BrowserRefresh => 0xA8,
        KeyCode::BrowserStop => 0xA9,
        KeyCode::BrowserSearch => 0xAA,
        KeyCode::BrowserFavorites => 0xAB,
        KeyCode::BrowserHome => 0xAC,
        KeyCode::AudioVolumeMute => 0xAD,
        KeyCode::AudioVolumeDown => 0xAE,
        KeyCode::AudioVolumeUp => 0xAF,
        KeyCode::MediaTrackNext => 0xB0,
        KeyCode::MediaTrackPrevious => 0xB1,
        KeyCode::MediaStop => 0xB2,
        KeyCode::MediaPlayPause => 0xB3,
        KeyCode::LaunchMail => 0xB4,
        KeyCode::MediaSelect => 0xB5,
        KeyCode::LaunchApp1 => 0xB6,
        KeyCode::LaunchApp2 => 0xB7,
        _ => return None,
    };
    Some(vk)
}

// ---------------------------------------------------------------------------
// System 2: tvp-input state → game window script methods
// ---------------------------------------------------------------------------

/// Diffuse the frame's input into the scene's windows.
///
/// Computes the frame's edges against the previous snapshot and calls the
/// script methods on every window with a registered TJS object (the game
/// has a single primary window; a future multi-window milestone can pick
/// the focused one).
pub(crate) fn dispatch_input(
    vm: Res<VmRuntime>,
    shared: Res<SharedScene>,
    mut bridge: ResMut<BridgeState>,
) {
    let state = tvp_input::input_state();
    let guard = state.lock().unwrap_or_else(|p| p.into_inner());
    // Forward the freshest cursor position so Layer.cursorX/cursorY mirror
    // Mouse.getCursorX/Y even when scripts moved the cursor (setCursorPos).
    tvp_visual::natives::set_shared_cursor_pos(guard.mouse.x, guard.mouse.y);
    let events = collect_frame_events(&guard, &mut bridge);
    drop(guard);

    if events.is_empty() {
        return;
    }
    let scene = shared.0.read().expect("shared scene lock poisoned");
    dispatch_to_windows(vm.engine.as_ref(), &scene, &events);
}

/// Recover this frame's input edges from the shared state vs the previous
/// snapshot, and update the snapshot for the next frame. Pure — no VM, no
/// Bevy — so it is unit-testable.
pub(crate) fn collect_frame_events(state: &InputState, bridge: &mut BridgeState) -> FrameEvents {
    // Mouse button edges.
    let mut button_down = Vec::new();
    let mut button_up = Vec::new();
    for b in 0..MOUSE_BUTTONS {
        let now = state.is_mouse_button_down(b);
        let prev = bridge.prev_buttons[b];
        if now && !prev {
            button_down.push(b);
        } else if prev && !now {
            button_up.push(b);
        }
    }
    bridge.prev_buttons = state
        .mouse
        .buttons
        .as_slice()
        .try_into()
        .expect("5 buttons");

    // Key edges.
    let mut keys_down = Vec::new();
    let mut keys_up = Vec::new();
    let now_keys: HashSet<u32> = state.keys.pressed.keys().copied().collect();
    for &k in &now_keys {
        if !bridge.prev_keys.contains(&k) {
            keys_down.push(k);
        }
    }
    for &k in &bridge.prev_keys {
        if !now_keys.contains(&k) {
            keys_up.push(k);
        }
    }
    bridge.prev_keys = now_keys;

    // Cursor movement (only when the position actually changed).
    let pos = (state.mouse.x, state.mouse.y);
    let moved = (pos != bridge.last_move_pos).then_some(pos);
    if moved.is_some() {
        bridge.last_move_pos = pos;
    }

    // Vertical wheel delta accumulated this frame.
    let wheel = (state.mouse.wheel.1 != 0).then_some(state.mouse.wheel.1);

    FrameEvents {
        button_down,
        button_up,
        keys_down,
        keys_up,
        moved,
        position: pos,
        wheel,
        shift: shift_flags(state),
    }
}

/// The `ss*` shift-flag mask for input events: keyboard modifiers
/// (`ssShift=(1<<0)`, `ssAlt=(1<<1)`, `ssCtrl=(1<<2)`) plus held mouse
/// buttons (`ssLeft=(1<<3)`, `ssRight=(1<<4)`, `ssMiddle=(1<<5)`), read
/// from the current held state like the reference does.
fn shift_flags(state: &InputState) -> i32 {
    let mut shift = 0i32;
    if state.is_key_down(VK_SHIFT) {
        shift |= 1 << 0;
    }
    if state.is_key_down(VK_MENU) {
        shift |= 1 << 1;
    }
    if state.is_key_down(VK_CONTROL) {
        shift |= 1 << 2;
    }
    if state.is_mouse_button_down(MB_LEFT) {
        shift |= 1 << 3;
    }
    if state.is_mouse_button_down(MB_RIGHT) {
        shift |= 1 << 4;
    }
    if state.is_mouse_button_down(MB_MIDDLE) {
        shift |= 1 << 5;
    }
    shift
}

/// Call the six `onXxx` methods on every scene window that has a script
/// object registered (via `tvp_visual::natives::window_tjs_object`),
/// mirroring how the native `Window` class in real TVP forwards OS events.
///
/// Argument mapping matches `system/window.tjs`:
/// `onMouseDown(x, y, button, shift)`, `onMouseUp(x, y, button, shift)`,
/// `onMouseMove(x, y, shift)`, `onMouseWheel(shift, delta, x, y)`,
/// `onKeyDown(key, shift)`, `onKeyUp(key, shift)` — with `button` the TVP
/// `mbLeft`..`mbX2` index, `key` the Windows VK code, and `shift` the
/// `ss*` mask.
pub(crate) fn dispatch_to_windows(engine: &Tjs2Engine, scene: &Scene, events: &FrameEvents) {
    for win in &scene.windows {
        let obj = tvp_visual::natives::window_tjs_object(win.id);
        if obj.is_null() {
            continue;
        }
        // Retain the object for the duration of this window's dispatch (the
        // engine MUST outlive the DetachedValue — the caller owns it).
        let Ok(dv) = engine.retain_object_detached(obj) else {
            log::warn!(
                "input bridge: window #{} object cannot be retained; skipping its input",
                win.id
            );
            continue;
        };
        let id = dv.raw_id();
        let (x, y) = events.position;

        if let Some((mx, my)) = events.moved {
            call_guarded(
                engine,
                id,
                "onMouseMove",
                &[
                    TjsValue::Integer(i64::from(mx)),
                    TjsValue::Integer(i64::from(my)),
                    TjsValue::Integer(i64::from(events.shift)),
                ],
            );
        }
        for &b in &events.button_down {
            call_guarded(
                engine,
                id,
                "onMouseDown",
                &[
                    TjsValue::Integer(i64::from(x)),
                    TjsValue::Integer(i64::from(y)),
                    TjsValue::Integer(b as i64),
                    TjsValue::Integer(i64::from(events.shift)),
                ],
            );
        }
        for &b in &events.button_up {
            call_guarded(
                engine,
                id,
                "onMouseUp",
                &[
                    TjsValue::Integer(i64::from(x)),
                    TjsValue::Integer(i64::from(y)),
                    TjsValue::Integer(b as i64),
                    TjsValue::Integer(i64::from(events.shift)),
                ],
            );
        }
        if let Some(delta) = events.wheel {
            call_guarded(
                engine,
                id,
                "onMouseWheel",
                &[
                    TjsValue::Integer(i64::from(events.shift)),
                    TjsValue::Integer(i64::from(delta)),
                    TjsValue::Integer(i64::from(x)),
                    TjsValue::Integer(i64::from(y)),
                ],
            );
        }
        for &k in &events.keys_down {
            call_guarded(
                engine,
                id,
                "onKeyDown",
                &[
                    TjsValue::Integer(i64::from(k)),
                    TjsValue::Integer(i64::from(events.shift)),
                ],
            );
        }
        for &k in &events.keys_up {
            call_guarded(
                engine,
                id,
                "onKeyUp",
                &[
                    TjsValue::Integer(i64::from(k)),
                    TjsValue::Integer(i64::from(events.shift)),
                ],
            );
        }
    }
}

/// Invoke one script method on a retained object id, logging (never
/// panicking) on failure — a window whose class lacks the method, or whose
/// object was destroyed, must not take the app down.
fn call_guarded(engine: &Tjs2Engine, id: Tjs2ValueId, method: &str, args: &[TjsValue]) {
    if let Err(e) = engine.call_member(id, method, args) {
        log::warn!("input bridge: window.{method} failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, RwLock};

    use tjs2_sys::Tjs2Engine;

    use super::*;

    /// Edge recovery + shift flags, driven by the synthetic frame protocol —
    /// no VM, no Bevy.
    #[test]
    fn collect_frame_events_detects_edges_pos_wheel_shift() {
        let mut bridge = BridgeState::default();
        let mut state = InputState::new();

        // Frame 1: left button pressed, cursor moved, Return pressed.
        state.begin_frame();
        state.set_mouse_pos(100, 50);
        state.set_mouse_button(MB_LEFT, true);
        state.set_key_down(0x0D); // VK_RETURN
        state.end_frame();
        let ev = collect_frame_events(&state, &mut bridge);
        assert_eq!(ev.button_down, vec![MB_LEFT]);
        assert!(ev.button_up.is_empty());
        assert_eq!(ev.keys_down, vec![0x0D]);
        assert!(ev.keys_up.is_empty());
        assert_eq!(ev.moved, Some((100, 50)));
        assert_eq!(ev.position, (100, 50));
        assert!(ev.wheel.is_none());
        // ssLeft is set while the left button is held.
        assert_eq!(ev.shift & (1 << 3), 1 << 3);

        // Frame 2: nothing changes — no edges, no move, no wheel.
        state.begin_frame();
        state.end_frame();
        let ev = collect_frame_events(&state, &mut bridge);
        assert!(ev.is_empty());

        // Frame 3: release + wheel + key up.
        state.begin_frame();
        state.set_mouse_button(MB_LEFT, false);
        state.set_key_up(0x0D);
        state.add_wheel(0, 120, 0);
        state.end_frame();
        let ev = collect_frame_events(&state, &mut bridge);
        assert_eq!(ev.button_up, vec![MB_LEFT]);
        assert!(ev.button_down.is_empty());
        assert_eq!(ev.keys_up, vec![0x0D]);
        assert_eq!(ev.wheel, Some(120));
        assert!(ev.moved.is_none());
        // Nothing held → no shift flags.
        assert_eq!(ev.shift, 0);
    }

    /// Keyboard modifiers and held buttons compose into the ss* mask.
    #[test]
    fn shift_flags_compose_modifiers_and_buttons() {
        let mut state = InputState::new();
        state.begin_frame();
        state.set_mouse_button(MB_RIGHT, true);
        state.set_key_down(0x10); // VK_SHIFT
        state.set_key_down(0x11); // VK_CONTROL
        state.end_frame();
        let shift = shift_flags(&state);
        assert_eq!(shift, (1 << 0) | (1 << 2) | (1 << 4)); // ssShift|ssCtrl|ssRight
    }

    /// Full end-to-end dispatch with a *stub* script window (no real game):
    /// build a VM, register the tvp-input + tvp-visual natives, subclass
    /// Window with recording onMouseDown/onMouseMove/onKeyDown, drive a
    /// synthetic frame, and check the exact arguments land.
    #[test]
    fn dispatch_calls_script_window_methods() {
        let _vm_lock = tvp_visual::natives::vm_test_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        // A throwaway game dir so register_visual's storage context exists.
        let dir =
            std::env::temp_dir().join(format!("krkr-rs-input-bridge-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp game dir");

        let engine = Arc::new(Tjs2Engine::new().expect("engine"));
        tvp_input::register_all(&engine).expect("register Mouse + Key");
        let storage = Arc::new(Mutex::new(
            engine::Storage::mount(dir.to_str().unwrap()).expect("mount"),
        ));
        let scene = Arc::new(RwLock::new(Scene::default()));
        tvp_visual::register_visual(&engine, scene.clone(), storage.clone())
            .expect("register visual natives");

        // A MainWindow-like subclass that records every input callback.
        engine
            .exec_script(
                r#"
                global.__log = "";
                class TestWin extends Window {
                    function TestWin() { super.Window(); }
                    function onMouseMove(x, y, shift) {
                        global.__log += "m:" + x + "," + y + "," + shift + ";";
                    }
                    function onMouseDown(x, y, button, shift) {
                        global.__log += "d:" + x + "," + y + "," + button + "," + shift + ";";
                    }
                    function onMouseUp(x, y, button, shift) {
                        global.__log += "u:" + x + "," + y + "," + button + "," + shift + ";";
                    }
                    function onMouseWheel(shift, delta, x, y) {
                        global.__log += "w:" + shift + "," + delta + "," + x + "," + y + ";";
                    }
                    function onKeyDown(key, shift) {
                        global.__log += "k:" + key + "," + shift + ";";
                    }
                    function onKeyUp(key, shift) {
                        global.__log += "ku:" + key + "," + shift + ";";
                    }
                }
                var w = new TestWin();
                "#,
                "test",
            )
            .expect("define TestWin");

        // The native Window ctor registered the TestWin object.
        assert!(!scene.read().unwrap().windows.is_empty());
        let win_id = scene.read().unwrap().windows[0].id;
        assert!(!tvp_visual::natives::window_tjs_object(win_id).is_null());

        // Frame 1: move + left press + Return down, all in one frame.
        {
            let state = tvp_input::input_state();
            let mut s = state.lock().unwrap();
            s.begin_frame();
            s.set_mouse_pos(320, 240);
            s.set_mouse_button(MB_LEFT, true);
            s.set_key_down(0x0D);
            s.end_frame();
        }
        let mut bridge = BridgeState::default();
        let (events, log) = {
            let state = tvp_input::input_state();
            let s = state.lock().unwrap();
            let events = collect_frame_events(&s, &mut bridge);
            let scene_guard = scene.read().unwrap();
            dispatch_to_windows(engine.as_ref(), &scene_guard, &events);
            drop(scene_guard);
            drop(s);
            (events, read_log(&engine))
        };
        // ssLeft is set while the button is held → all handlers see shift=8.
        assert_eq!(
            log, "m:320,240,8;d:320,240,0,8;k:13,8;",
            "move → onMouseDown → onKeyDown, button=mbLeft(0), key=VK_RETURN(13)"
        );
        assert_eq!(events.button_down, vec![MB_LEFT]);
        engine.exec_script("global.__log = '';", "test").unwrap();

        // Frame 2: release + Return up.
        {
            let state = tvp_input::input_state();
            let mut s = state.lock().unwrap();
            s.begin_frame();
            s.set_mouse_button(MB_LEFT, false);
            s.set_key_up(0x0D);
            s.end_frame();
        }
        let log = {
            let state = tvp_input::input_state();
            let s = state.lock().unwrap();
            let events = collect_frame_events(&s, &mut bridge);
            let scene_guard = scene.read().unwrap();
            dispatch_to_windows(engine.as_ref(), &scene_guard, &events);
            drop(scene_guard);
            drop(s);
            read_log(&engine)
        };
        // Nothing held on release → shift=0 for the up edges.
        assert_eq!(log, "u:320,240,0,0;ku:13,0;");
    }

    fn read_log(engine: &Tjs2Engine) -> String {
        match engine.eval("global.__log", "test") {
            Ok(TjsValue::String(s)) => s,
            other => panic!("global.__log -> {other:?}"),
        }
    }
}
