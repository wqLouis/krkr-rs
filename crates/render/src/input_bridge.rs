//! Input bridge: Bevy input events → shared `tvp-input` state → the game
//! window's script `onMouseDown`/`onKeyDown`/... methods.
//!
//! This is the "later wave" the `tvp-input` crate docs promise: the app
//! drives the process-global [`InputState`] with the documented frame
//! protocol each frame, and then diffuses the state into the game's script
//! objects so the title screen's buttons become clickable.
//!
//! Two `Update` systems, chained and ordered *after* [`crate::runner::run_vm`] (see
//! `runner.rs`) so they never touch the single-threaded TJS VM concurrently:
//!
//! * [`capture_input`] drains Bevy's `ButtonInput<MouseButton>`,
//!   `ButtonInput<KeyCode>`, `Query<&Gamepad>`, `CursorMoved`, `MouseWheel`
//!   and `TouchInput` events into the shared `tvp_input::InputState`
//!   (`begin_frame` → `set_*` → `end_frame`), queues any
//!   `Mouse.setCursorPos` OS-cursor warp, and buffers touch samples.
//! * [`dispatch_input`] computes this frame's edges (button press/release,
//!   key down/up, cursor movement, wheel, click/double-click) against the
//!   previous frame's snapshot ([`BridgeState`]), applies the cursor warp to
//!   the Bevy `Window`, and calls the matching script methods on every
//!   registered scene window and the layer under the cursor:
//!   `onMouseDown/Up/Move/Wheel`, `onKeyDown/Up` and
//!   `onClick`/`onDoubleClick` — exactly the methods `system/window.tjs`'s
//!   `MainWindow` uses to feed `dispatchInputNotify`.
//!
//! Input source mapping:
//!
//! * mouse buttons → `tvp_input` mouse state (`set_mouse_button`), which the
//!   `Mouse.getClickCount` multi-click sequence feeds
//!   [`FrameEvents::click_counts`];
//! * keyboard + gamepad buttons → `tvp_input` key state as Windows `VK_*`
//!   codes, gamepad through the KiriKiri `VK_PAD*` range
//!   (`gamepad_button_to_vk`, reference `DInputMgn.cpp:686-700`);
//! * the `shift` argument comes from `tvp_input`'s reference `TVP_SS_*` mask
//!   (`InputState::shift_flags`), so held modifiers and mouse-button VKs
//!   resolve identically everywhere;
//! * touch → the **same** mouse state, through the pure gesture classifier in
//!   [`crate::touch_gesture`] (see below).
//!
//! # Touch input
//!
//! KiriKiri games are mouse-and-keyboard driven, so touch is bridged to
//! mouse, not modelled as a new input source. [`capture_input`] maps Bevy's
//! `TouchInput` events through the very same window→game transform the cursor
//! uses, then feeds them to [`crate::touch_gesture::TouchGesture`] and applies
//! the resulting virtual mouse actions to the shared `tvp_input` state. From
//! there the ordinary desktop path takes over: layer hit-testing, the games'
//! `onMouseDown/Up/Move`, `Mouse.getCursorX/Y`, `Mouse.getClickCount` and
//! `System.getMouseButtonState` all see touch exactly as they see a mouse.
//!
//! The classifier's rules and thresholds (all unit-tested on the host): a short
//! press is a **tap** (left click); a press held past 500 ms without moving
//! beyond the slop is a **long press** (right click, KAG's menu); movement
//! beyond 24 game pixels starts a **drag** with the left button held (a drag
//! press is held but is
//! *not* a click, so it never fires `onClick`); only the primary pointer is
//! tracked, so a second finger cannot disturb it; an up without a down is
//! ignored. A cancelled touch never clicks.
//!
//! Two correctness points beyond the classifier itself:
//!
//! * **No double-fire.** The games' `system/window.tjs` forwards its
//!   `onTouchDown/Up/Move` handlers to `onMouseDown/Up/Move`, so dispatching
//!   both the raw touch methods *and* the synthesized mouse events would fire
//!   two clicks per tap. The bridge therefore does **not** call the
//!   `onTouch*` script methods; touch is expressed solely as mouse state. A
//!   touch device that also emits Bevy mouse events (or a physical mouse used
//!   mid-touch) is gated off while a touch is in flight.
//! * **Edge preservation.** `tvp-input` is sampled per frame, so a tap whose
//!   down and up land in the same Bevy frame would collapse to "no edge". The
//!   classifier emits an action sequence and [`apply_pending_mouse_actions`]
//!   drains at most one button transition per frame, keeping every edge visible
//!   to `collect_frame_events`.
//!
//! `Window.getTouchVelocity` is still fed from the raw samples; it is not part
//! of the mouse bridge.
//!
//! Nothing here has been exercised on a touchscreen or an Android device —
//! only the pure classifier and the action-draining helper have host tests.

use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

use bevy::ecs::message::MessageReader;
use bevy::input::ButtonInput;
use bevy::input::gamepad::{Gamepad, GamepadButton};
use bevy::input::keyboard::KeyCode;
use bevy::input::mouse::{MouseButton, MouseWheel};
use bevy::input::touch::{TouchInput, TouchPhase};
use bevy::prelude::{Query, Res, ResMut, Resource, Vec2};
use bevy::window::{CursorMoved, Window};
use tjs2_sys::{Tjs2Engine, Tjs2ValueId, TjsValue};
use tvp_input::{
    InputState, MB_LEFT, MB_MIDDLE, MB_RIGHT, MB_X1, MB_X2, MOUSE_BUTTONS, pad,
    shift_state::any_mouse_button_pressed,
};
use tvp_visual::scene::Scene;

use crate::runner::VmRuntime;
use crate::sync::{SharedScene, WindowRedrawRequested};
use crate::touch_gesture::{
    GestureButton, MouseAction, TouchGesture, TouchPhase as GesturePhase, TouchSample,
};

/// Move the gesture classifier's virtual buttons onto the TVP indices. Kept
/// here (not in the pure module) so `touch_gesture` stays free of TVP types.
fn gesture_button_to_tvp(button: GestureButton) -> usize {
    match button {
        GestureButton::Left => MB_LEFT,
        GestureButton::Right => MB_RIGHT,
    }
}

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
///
/// It also remembers layers whose script object turned out to be
/// invalidated ([`Self::dead_layers`]) so the bridge stops dispatching to
/// them instead of logging a failure on every mouse-move frame.
#[derive(Resource, Default)]
pub(crate) struct BridgeState {
    /// Mouse buttons held at the end of the last dispatched frame.
    prev_buttons: [bool; MOUSE_BUTTONS],
    /// Key codes held at the end of the last dispatched frame.
    prev_keys: HashSet<u32>,
    /// Cursor position the last `onMouseMove` was dispatched for.
    last_move_pos: (i32, i32),
    /// Layer that last received `onMouseEnter` (for enter/leave edges), the
    /// reference `LastMouseMoveSent`.
    last_move_layer: Option<u32>,
    /// Layer that received `onMouseDown` and therefore owns the following
    /// `onMouseUp`/moves until release, the reference `CaptureOwner`.
    capture_layer: Option<u32>,
    /// Layer ids whose TJS object is invalidated/unusable. The game can
    /// `invalidate` a Layer's object while the layer is still present in the
    /// scene, which makes `call_member` throw `The object is already
    /// invalidated` once per mouse-move frame. Such layers are treated as
    /// no-hit by [`plan_layer_calls`] and never dispatched to again. The
    /// scene itself is left untouched here — removing the stale layer is a
    /// separate `tvp-visual` fix.
    dead_layers: HashSet<u32>,
    /// Gamepad `VK_PAD*` codes currently held. Diffed each frame so a pad
    /// that disconnects releases its buttons (the reference pad state is
    /// global across devices, `DInputMgn.cpp:707`).
    pad_held: HashSet<u32>,
    /// The pure touch → mouse gesture classifier ([`crate::touch_gesture`]).
    gesture: TouchGesture,
    /// Origin of the gesture clock. Created lazily on the first touch sample;
    /// the classifier itself takes plain [`Duration`] timestamps, so its tests
    /// inject their own clock instead of reading `Instant::now()`.
    gesture_origin: Option<Instant>,
    /// Virtual mouse actions the classifier produced but the sampled
    /// `tvp-input` state has not seen yet. [`apply_pending_mouse_actions`]
    /// drains at most one button transition per frame, so a tap whose down and
    /// up arrive in a single Bevy frame still yields both edges.
    pending_mouse_actions: VecDeque<MouseAction>,
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
    /// The `ss*` shift flag mask (keyboard modifiers + held mouse buttons),
    /// the reference `TVP_SS_*` from `InputState::shift_flags`.
    pub(crate) shift: i32,
    /// Multi-click count at each button's press edge (0 = no press this
    /// frame): 1 for `onClick`, ≥2 for `onDoubleClick`.
    pub(crate) click_counts: [u32; MOUSE_BUTTONS],
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
/// Runs before [`dispatch_input`] (chained in `runner.rs`) and after
/// `run_vm`, so scripts never see input mid-poll. Cursor positions are
/// mapped from the OS window into game (primary-layer) coordinates by
/// [`crate::sync::window_to_game_transform`], which applies the same
/// aspect-preserving scale the scene camera uses.
#[allow(clippy::too_many_arguments)]
pub(crate) fn capture_input(
    mouse_input: Res<ButtonInput<MouseButton>>,
    keyboard: Res<ButtonInput<KeyCode>>,
    gamepads: Query<&Gamepad>,
    mut cursor_moved: MessageReader<CursorMoved>,
    mut wheel_events: MessageReader<MouseWheel>,
    mut touch_events: MessageReader<TouchInput>,
    windows: Query<&Window>,
    shared: Res<SharedScene>,
    mut bridge: ResMut<BridgeState>,
) {
    let (game_w, game_h) = game_size(&shared);
    // The scene camera uses `ScalingMode::AutoMin` (aspect-preserving), so map
    // the cursor with the same uniform scale + centering offset instead of
    // stretching each axis independently (see `sync::window_to_game_transform`).
    let window_size = windows
        .iter()
        .next()
        .map(|w| (w.width(), w.height()))
        .filter(|(w, h)| *w > 0.0 && *h > 0.0)
        .unwrap_or((game_w as f32, game_h as f32));
    let (scale, offset_x, offset_y) =
        crate::sync::window_to_game_transform(window_size, (game_w, game_h));
    // Logical window pixels → game (primary-layer) coordinates, clamped to
    // the game bounds. The inverse of this mapping is `game_to_window_pixel`.
    let to_game = |px: f32, py: f32| -> (i32, i32) {
        (
            ((px / scale + offset_x).round() as i32).clamp(0, game_w.saturating_sub(1) as i32),
            ((py / scale + offset_y).round() as i32).clamp(0, game_h.saturating_sub(1) as i32),
        )
    };

    let state = tvp_input::input_state();
    let mut s = state.lock().unwrap_or_else(|p| p.into_inner());
    s.begin_frame();

    // Touch → virtual mouse. Bevy touch events are mapped with the *same*
    // window→game transform as the cursor (above) and classified by the pure
    // `touch_gesture` state machine; the actions are queued and drained below.
    // The per-touch velocity tracker that backs `Window.getTouchVelocity` is
    // fed here too.
    let mut touch_samples: Vec<TouchSample> = Vec::new();
    for ev in touch_events.read() {
        let (x, y) = to_game(ev.position.x, ev.position.y);
        match ev.phase {
            TouchPhase::Started | TouchPhase::Moved => {
                tvp_visual::natives::window::record_touch_movement(
                    ev.id as u32,
                    x as f64,
                    y as f64,
                );
            }
            TouchPhase::Ended | TouchPhase::Canceled => {
                tvp_visual::natives::window::clear_touch_velocity(ev.id as u32);
            }
        }
        touch_samples.push(TouchSample {
            id: ev.id,
            phase: match ev.phase {
                TouchPhase::Started => GesturePhase::Started,
                TouchPhase::Moved => GesturePhase::Moved,
                TouchPhase::Ended => GesturePhase::Ended,
                TouchPhase::Canceled => GesturePhase::Canceled,
            },
            x,
            y,
            // Filled with the frame clock below (the classifier is clock
            // injected; the values are equal for all samples of one frame).
            time: Duration::ZERO,
        });
    }
    // A touch is "in flight" for this frame if it produced samples or the
    // classifier is still tracking a finger. While it is, the Bevy mouse path
    // is gated off: a touch device must never deliver a tap twice (once
    // synthesized from touch, once from a Bevy mouse event).
    let touch_active = !touch_samples.is_empty() || bridge.gesture.is_active();
    let now = match bridge.gesture_origin {
        Some(origin) => origin.elapsed(),
        None if touch_active => {
            bridge.gesture_origin = Some(Instant::now());
            Duration::ZERO
        }
        None => Duration::ZERO,
    };
    for sample in &mut touch_samples {
        sample.time = now;
    }
    let actions = bridge.gesture.update(now, &touch_samples);
    bridge.pending_mouse_actions.extend(actions);

    // Cursor position: the last CursorMoved of the frame, clamped to the
    // game bounds. Read unconditionally so unread events do not back up.
    let last_cursor = cursor_moved
        .read()
        .last()
        .map(|pos| to_game(pos.position.x, pos.position.y));
    if !touch_active && let Some((x, y)) = last_cursor {
        s.set_mouse_pos(x, y);
        // Feed the host pointer tracker that backs `Window.getMouseVelocity`.
        tvp_visual::natives::window::record_mouse_movement(x as f64, y as f64);
    }

    // Mouse buttons: press/release edges only (held state persists in the
    // state across frames), gated while a touch is in flight (see above).
    if !touch_active {
        for (bevy_button, tvp_button) in BUTTON_MAP {
            if mouse_input.just_pressed(bevy_button) {
                s.set_mouse_button(tvp_button, true);
            } else if mouse_input.just_released(bevy_button) {
                s.set_mouse_button(tvp_button, false);
            }
        }
    }

    // Keyboard: down/up edges, mapped to Windows VK codes (what the game's
    // onKeyDown compares against VK_*).
    for code in keyboard.get_just_pressed() {
        if let Some(vk) = bevy_key_to_vk(*code) {
            s.set_key_down(vk);
        }
    }
    for code in keyboard.get_just_released() {
        if let Some(vk) = bevy_key_to_vk(*code) {
            s.set_key_up(vk);
        }
    }

    // Gamepad: feed the KiriKiri `VK_PAD*` codes from every connected pad.
    // Diffing against the previously held set releases a pad's buttons when
    // it disconnects (the reference pad state is global across devices).
    let mut now_pad: HashSet<u32> = HashSet::new();
    for gamepad in gamepads.iter() {
        for button in GamepadButton::all() {
            if gamepad.pressed(button)
                && let Some(vk) = gamepad_button_to_vk(button)
            {
                now_pad.insert(vk);
            }
        }
    }
    for &vk in now_pad.difference(&bridge.pad_held) {
        s.set_key_down(vk);
    }
    for &vk in bridge.pad_held.difference(&now_pad) {
        s.set_key_up(vk);
    }
    bridge.pad_held = now_pad;

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

    // Apply the virtual mouse actions the classifier produced, including a
    // deferred transition from an earlier frame, one button change per frame
    // (see `apply_pending_mouse_actions`).
    apply_pending_mouse_actions(&mut s, &mut bridge.pending_mouse_actions);

    s.end_frame();
}

/// Apply queued touch → mouse actions to the shared state.
///
/// The `tvp-input` frame protocol is sampled, and [`collect_frame_events`]
/// recovers button edges by diffing consecutive frames. Applying a tap's press
/// and release in the *same* frame would therefore collapse them into "no
/// edge" and drop the click. So this drains the queue up to and including the
/// first button transition and stops; moves that follow the transition stay
/// queued for the next frame, where they are applied before the release.
///
/// Pure with respect to Bevy (it only writes [`InputState`]); the unit tests
/// drive it directly.
pub(crate) fn apply_pending_mouse_actions(
    state: &mut InputState,
    queue: &mut VecDeque<MouseAction>,
) {
    while let Some(action) = queue.pop_front() {
        match action {
            MouseAction::MoveTo { x, y } => state.set_mouse_pos(x, y),
            MouseAction::Button { button, down } => {
                state.set_mouse_button(gesture_button_to_tvp(button), down);
                break;
            }
            MouseAction::PressWithoutClick { button } => {
                let tvp = gesture_button_to_tvp(button);
                state.set_mouse_button_raw(tvp, true);
                state.clear_click_sequence(tvp);
                break;
            }
        }
    }
}

/// The game resolution to map cursor positions into: the first scene
/// window's logical rendering resolution (the primary layer's size), falling
/// back to the title size.
///
/// This must match [`crate::sync::scene_projection`]'s logical size
/// exactly — the primary layer, *not* the possibly-zoomed client size — so
/// that cursor/input coordinates stay in the coordinate space the game's
/// layers and hit rectangles use.
fn game_size(shared: &SharedScene) -> (u32, u32) {
    let scene = shared.0.read().expect("shared scene lock poisoned");
    crate::sync::first_window_logical_size(&scene).unwrap_or(DEFAULT_GAME_SIZE)
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

/// Map a Bevy [`GamepadButton`] to the KiriKiri `VK_PAD*` code the reference
/// `TVPGetJoyPadAsyncState`/`System.getKeyState` uses (`DInputMgn.cpp:686`).
///
/// The physical layout follows the common KiriKiri pad mapping: the D-pad maps
/// to `VK_PADLEFT/UP/RIGHT/DOWN`, and the ten standard face/shoulder/start/
/// thumb buttons map, in `pkfButton0..9` order (`DInputMgn.cpp:686-698`), to
/// `VK_PAD1..VK_PAD10`.
fn gamepad_button_to_vk(button: GamepadButton) -> Option<u32> {
    let vk = match button {
        GamepadButton::DPadLeft => pad::VK_PADLEFT,
        GamepadButton::DPadUp => pad::VK_PADUP,
        GamepadButton::DPadRight => pad::VK_PADRIGHT,
        GamepadButton::DPadDown => pad::VK_PADDOWN,
        GamepadButton::South => pad::VK_PAD1,
        GamepadButton::East => pad::VK_PAD2,
        GamepadButton::West => pad::VK_PAD3,
        GamepadButton::North => pad::VK_PAD4,
        GamepadButton::LeftTrigger => pad::VK_PAD5,
        GamepadButton::RightTrigger => pad::VK_PAD6,
        GamepadButton::Select => pad::VK_PAD7,
        GamepadButton::Start => pad::VK_PAD8,
        GamepadButton::LeftThumb => pad::VK_PAD9,
        GamepadButton::RightThumb => pad::VK_PAD10,
        // `C`/`Z`/`Mode`/second triggers have no KiriKiri pad code.
        _ => return None,
    };
    Some(vk)
}

/// Inverse of `sync::window_to_game_transform`: a game (primary-layer) point →
/// logical window pixels, used by `Mouse.setCursorPos`'s OS-cursor warp.
fn game_to_window_pixel(game: (i32, i32), transform: (f32, f32, f32)) -> (f32, f32) {
    let (scale, offset_x, offset_y) = transform;
    (
        (game.0 as f32 - offset_x) * scale,
        (game.1 as f32 - offset_y) * scale,
    )
}

/// Apply a pending `Mouse.setCursorPos` warp to the OS cursor via Bevy's
/// `Window::set_cursor_position` (the winit backend moves the real cursor on
/// its next pass). The game point is converted back to logical window pixels
/// with the same aspect-preserving transform the cursor reader uses.
fn apply_mouse_warp(windows: &mut Query<&mut Window>, shared: &SharedScene, game: (i32, i32)) {
    let Some(mut window) = windows.iter_mut().next() else {
        return;
    };
    if window.width() <= 0.0 || window.height() <= 0.0 {
        return;
    }
    let transform =
        crate::sync::window_to_game_transform((window.width(), window.height()), game_size(shared));
    let (px, py) = game_to_window_pixel(game, transform);
    window.set_cursor_position(Some(Vec2::new(px, py)));
}

/// Consume the `tvp-visual` window request queues once per frame.
///
/// * `Window.bringToFront()` → focus/raise the host window. Bevy 0.19 exposes
///   no `raise`/`set_focused` method; assigning [`Window::focused`] is the
///   supported focus request, and the winit backend turns that change into
///   `winit::window::Window::focus_window()` (documented as bringing the
///   window to the foreground). A separate z-order raise is not available
///   through Bevy 0.19, so this is the closest primitive.
/// * `Window.update()` → set [`WindowRedrawRequested`], which
///   [`crate::sync::sync_scene`] consumes this frame to bypass its idle
///   fast path.
/// * `Window.resetMouseVelocity()` → clear the host mouse-velocity tracker
///   that feeds `Window.getMouseVelocity`.
///
/// Every queue is drained, even though the host has a single primary window
/// and a request's scene window id does not distinguish anything, so they can
/// never accumulate. Run before `sync_scene` (see `runner.rs`).
pub(crate) fn consume_window_requests(
    mut windows: Query<&mut Window>,
    mut redraw: ResMut<WindowRedrawRequested>,
) {
    use tvp_visual::natives::window::{
        clear_mouse_velocity, take_mouse_velocity_reset_requests, take_window_raise_requests,
        take_window_update_requests,
    };

    if !take_window_raise_requests().is_empty()
        && let Some(mut window) = windows.iter_mut().next()
    {
        window.focused = true;
    }

    if !take_window_update_requests().is_empty() {
        redraw.0 = true;
    }

    if !take_mouse_velocity_reset_requests().is_empty() {
        clear_mouse_velocity();
    }
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
    mut windows: Query<&mut Window>,
    mut bridge: ResMut<BridgeState>,
) {
    let state = tvp_input::input_state();
    let mut guard = state.lock().unwrap_or_else(|p| p.into_inner());
    // Forward the freshest cursor position so Layer.cursorX/cursorY mirror
    // Mouse.getCursorX/Y even when scripts moved the cursor (setCursorPos).
    tvp_visual::natives::set_shared_cursor_pos(guard.mouse.x, guard.mouse.y);
    let events = collect_frame_events(&guard, &mut bridge);
    // `Mouse.setCursorPos` queues an OS-cursor warp; consume it exactly once.
    let warp = guard.take_mouse_warp();
    drop(guard);

    // Apply the warp even on a frame with no other input.
    if let Some(game) = warp {
        apply_mouse_warp(&mut windows, &shared, game);
    }

    if events.is_empty() {
        return;
    }
    // `System.eventDisabled` is set by both games right before
    // `System.terminate`; stop delivering input to the VM while it is set
    // (the edge snapshot above was still updated so a later re-enable cannot
    // replay stale edges).
    if tvp_visual::natives::window::events_disabled() {
        return;
    }
    if std::env::var_os("KRKR_INPUT_TRACE").is_some() {
        eprintln!(
            "[input] down={:?} up={:?} moved={:?} pos={:?} wheel={:?} clicks={:?}",
            events.button_down,
            events.button_up,
            events.moved,
            events.position,
            events.wheel,
            events.click_counts,
        );
    }
    // Plan the dispatch under the scene read lock (pure: hit-testing and
    // coordinate math only), then release it before calling into the TJS VM.
    // Holding the read lock across `call_member` deadlocks: a script handler
    // reads the scene again, and `sync_scene` may already be queued for the
    // write lock, so the re-entrant read blocks on the writer and the writer
    // blocks on our read.
    let (window_ids, layer_calls) = {
        let scene = shared.0.read().expect("shared scene lock poisoned");
        let window_ids: Vec<u32> = scene
            .windows
            .iter()
            .filter(|w| w.visible)
            .map(|w| w.id)
            .collect();
        (window_ids, plan_layer_calls(&scene, &events, &mut bridge))
    };

    let engine = vm.engine.as_ref();
    // The reference `tTJSNI_BaseWindow::OnMouseDown` first posts the event to
    // the Window object, then forwards it to the draw device, which routes it
    // to the layer under the cursor. Mirror both. Touch reaches this point as
    // the same mouse edges, so there is no separate touch dispatch.
    dispatch_to_windows(engine, &window_ids, &events);
    execute_layer_calls(engine, &layer_calls, &mut bridge);
}

/// One planned script call to a layer: resolved and invoked after the scene
/// lock is released (see [`dispatch_input`]).
struct LayerCall {
    layer_id: u32,
    method: &'static str,
    args: Vec<TjsValue>,
}

/// Plan the layer mouse dispatch, the reference
/// `tTVPLayerManager::PrimaryMouseMove`/`PrimaryMouseDown`/`PrimaryMouseUp`:
/// hit-test the layer tree, fire `onMouseEnter`/`onMouseLeave` on change,
/// `onMouseMove` with layer-local coordinates, and capture the pressed layer
/// for the following `onMouseUp`. Pure: no VM calls, no scene writes.
fn plan_layer_calls(
    scene: &Scene,
    events: &FrameEvents,
    bridge: &mut BridgeState,
) -> Vec<LayerCall> {
    let mut calls = Vec::new();
    let Some(win) = scene.windows.first() else {
        return calls;
    };
    if !win.visible {
        return calls;
    }
    // Drop a capture/last-hit whose layer has been removed or whose script
    // object is known to be dead.
    let dead = &bridge.dead_layers;
    bridge.capture_layer = bridge
        .capture_layer
        .filter(|id| scene.layer(*id).is_some() && !dead.contains(id));
    bridge.last_move_layer = bridge
        .last_move_layer
        .filter(|id| scene.layer(*id).is_some() && !dead.contains(id));

    let (px, py) = events.position;

    // Mouse move: enter/leave edges, then the move handler.
    if let Some((x, y)) = events.moved {
        let hit = bridge
            .capture_layer
            .or_else(|| hit_test_excluding(scene, win.id, x, y, &bridge.dead_layers));
        if bridge.last_move_layer != hit {
            if let Some(prev) = bridge.last_move_layer {
                calls.push(LayerCall {
                    layer_id: prev,
                    method: "onMouseLeave",
                    args: Vec::new(),
                });
            }
            if let Some(l) = hit {
                calls.push(LayerCall {
                    layer_id: l,
                    method: "onMouseEnter",
                    args: Vec::new(),
                });
            }
            bridge.last_move_layer = hit;
        }
        if let Some(l) = hit {
            let (lx, ly) = layer_local(scene, l, x, y);
            calls.push(LayerCall {
                layer_id: l,
                method: "onMouseMove",
                args: vec![
                    TjsValue::Integer(i64::from(lx)),
                    TjsValue::Integer(i64::from(ly)),
                    TjsValue::Integer(i64::from(events.shift)),
                ],
            });
        }
    }

    // Mouse down: hit-test (unless captured) and remember the owner. A press
    // also completes the click/double-click gesture: the reference platform
    // posts `onClick`/`onDoubleClick` between the down and up events, at the
    // down position (`MainScene.cpp:1122`, `WindowIntf.cpp:316`).
    for &b in &events.button_down {
        let l = bridge
            .capture_layer
            .or_else(|| hit_test_excluding(scene, win.id, px, py, &bridge.dead_layers));
        if let Some(l) = l {
            let (lx, ly) = layer_local(scene, l, px, py);
            calls.push(LayerCall {
                layer_id: l,
                method: "onMouseDown",
                args: vec![
                    TjsValue::Integer(i64::from(lx)),
                    TjsValue::Integer(i64::from(ly)),
                    TjsValue::Integer(b as i64),
                    TjsValue::Integer(i64::from(events.shift)),
                ],
            });
            bridge.capture_layer = Some(l);
            // Only the left button is a "click" (the reference `_mouseBtn`
            // default / `WM_LBUTTON*`); `onClick` takes no button argument.
            // Count ≥2 fires `onDoubleClick` instead of a second `onClick`,
            // matching the Windows `WM_LBUTTONDBLCLK` sequence.
            // A drag press is a `Button` with `click_counts == 0`, so it
            // reaches `onMouseDown` but never `onClick`/`onDoubleClick`.
            if b == MB_LEFT && events.click_counts[b] >= 1 {
                let method = if events.click_counts[b] >= 2 {
                    "onDoubleClick"
                } else {
                    "onClick"
                };
                calls.push(LayerCall {
                    layer_id: l,
                    method,
                    args: vec![
                        TjsValue::Integer(i64::from(lx)),
                        TjsValue::Integer(i64::from(ly)),
                    ],
                });
            }
        }
    }

    // Mouse up: deliver to the capture owner. Release the capture only once
    // no mouse button remains held (reference `PrimaryMouseUp` tests
    // `TVPIsAnyMouseButtonPressedInShiftStateFlags(flags)`), so releasing a
    // second button does not drop a still-held button's capture.
    for &b in &events.button_up {
        if let Some(l) = bridge
            .capture_layer
            .or_else(|| hit_test_excluding(scene, win.id, px, py, &bridge.dead_layers))
        {
            let (lx, ly) = layer_local(scene, l, px, py);
            calls.push(LayerCall {
                layer_id: l,
                method: "onMouseUp",
                args: vec![
                    TjsValue::Integer(i64::from(lx)),
                    TjsValue::Integer(i64::from(ly)),
                    TjsValue::Integer(b as i64),
                    TjsValue::Integer(i64::from(events.shift)),
                ],
            });
        }
    }
    if !events.button_up.is_empty() && !any_mouse_button_pressed(events.shift as u32) {
        bridge.capture_layer = None;
    }

    calls
}

/// Execute the planned layer calls with no scene lock held.
///
/// A layer whose script object is missing (`does not exist`) is tolerated and
/// stays live — most layers implement only a few of the mouse events. A layer
/// whose object is **invalidated** (or cannot be retained at all) is recorded
/// in [`BridgeState::dead_layers`] so later frames skip it; any capture/enter
/// state pointing at it is cleared at the same time.
fn execute_layer_calls(engine: &Tjs2Engine, calls: &[LayerCall], bridge: &mut BridgeState) {
    for call in calls {
        let obj = tvp_visual::natives::layer_tjs_object(call.layer_id);
        if obj.is_null() {
            continue;
        }
        let dv = match engine.retain_object_detached(obj) {
            Ok(dv) => dv,
            Err(e) => {
                if mark_layer_dead(bridge, call.layer_id) {
                    log::warn!(
                        "input bridge: layer #{} object cannot be retained; \
                         marking it dead: {e}",
                        call.layer_id
                    );
                }
                continue;
            }
        };
        if let Err(e) = engine.call_member(dv.raw_id(), call.method, &call.args) {
            if is_invalidated_error(&e) {
                if mark_layer_dead(bridge, call.layer_id) {
                    log::warn!(
                        "input bridge: layer #{} object is invalidated; \
                         marking it dead: {e}",
                        call.layer_id
                    );
                }
            } else if !is_missing_member_error(&e) {
                log::warn!(
                    "input bridge: layer #{}.{} failed: {e}",
                    call.layer_id,
                    call.method
                );
            }
        }
        if std::env::var_os("KRKR_INPUT_TRACE").is_some() && call.method != "onMouseMove" {
            eprintln!("[input] call layer #{}.{}", call.layer_id, call.method);
        }
    }
}

/// Whether a `call_member` error means the object is no longer usable
/// (TJS's `TJSInvalidObject`, "The object is already invalidated"). Matched
/// case-insensitively so the exact wording does not matter.
fn is_invalidated_error(msg: &str) -> bool {
    msg.to_ascii_lowercase().contains("invalidated")
}

/// Whether a `call_member` error means the object simply has no such member
/// (TJS `TJS_E_MEMBERNOTFOUND`, `Member "..." does not exist`).
///
/// The reference dispatches every input event to the window/layer without
/// first checking the handler exists, and `tTVPEvent::Deliver` ignores the
/// `FuncCall` result (`EventIntf.cpp:97-118`). A game therefore defines only
/// the handlers it cares about (the title `MainWindow` has no `onClick`), so
/// a missing member is "no handler", never a failure. The message is matched
/// loosely because the empty-name legacy form (`Member "" does not exist`)
/// from older `tjs2_call_member` builds must be tolerated too.
fn is_missing_member_error(msg: &str) -> bool {
    msg.contains("does not exist")
}

/// Mark `id` as having a dead TJS object, dropping any capture/enter state
/// that points at it. Returns `true` when it was not already marked (so the
/// caller logs the first failure only).
fn mark_layer_dead(bridge: &mut BridgeState, id: u32) -> bool {
    let newly_dead = bridge.dead_layers.insert(id);
    if bridge.capture_layer == Some(id) {
        bridge.capture_layer = None;
    }
    if bridge.last_move_layer == Some(id) {
        bridge.last_move_layer = None;
    }
    newly_dead
}

/// A point in the primary layer's coordinates → the layer's local
/// coordinates (subtract each ancestor's `Rect.left/top`, the reference
/// `FromPrimaryCoordinates`).
fn layer_local(scene: &Scene, layer_id: u32, x: i32, y: i32) -> (i32, i32) {
    let mut lx = x;
    let mut ly = y;
    let mut current = Some(layer_id);
    while let Some(id) = current {
        let Some(layer) = scene.layer(id) else {
            break;
        };
        lx -= layer.rect.x;
        ly -= layer.rect.y;
        current = layer.parent;
    }
    (lx, ly)
}

/// The topmost hittable layer at a primary-layer point, or `None`.
///
/// Convenience wrapper over [`hit_test_excluding`] for tests and callers
/// with no dead layers; the production dispatch path passes
/// [`BridgeState::dead_layers`] directly.
#[cfg(test)]
pub(crate) fn hit_test(scene: &Scene, window_id: u32, x: i32, y: i32) -> Option<u32> {
    hit_test_excluding(scene, window_id, x, y, &HashSet::new())
}

/// Like [`hit_test`] but skips `dead` layers: their script object is
/// invalidated, so treating them as no-hit lets the bridge reach a live layer
/// beneath them instead of dispatching into a dead object every frame.
///
/// Our renderer flattens the layer tree by **absolute** rects (it does not
/// clip children to their parent's rect), and the game's `AffineLayer`
/// containers only size their `_image` child — the parent rect is normally
/// fixed up by the script `onPaint` we do not run, so it stays `0×0`. We
/// therefore hit-test the same flattened front-to-back order the renderer
/// uses, composed through ancestors, instead of clipping to each parent.
fn hit_test_excluding(
    scene: &Scene,
    window_id: u32,
    x: i32,
    y: i32,
    dead: &HashSet<u32>,
) -> Option<u32> {
    for layer_id in scene.window_layer_order(window_id).into_iter().rev() {
        if dead.contains(&layer_id) {
            continue;
        }
        let Some(layer) = scene.layer(layer_id) else {
            continue;
        };
        let Some((abs, visible)) = compose_abs(scene, layer_id) else {
            continue;
        };
        if !visible {
            continue;
        }
        let lx = x - abs.x;
        let ly = y - abs.y;
        if lx < 0 || ly < 0 || lx >= layer.rect.w as i32 || ly >= layer.rect.h as i32 {
            continue;
        }
        if hit_test_self(scene, layer, lx, ly) {
            return Some(layer_id);
        }
    }
    None
}

/// A layer's absolute rect + composed visibility (sum of ancestor offsets).
fn compose_abs(scene: &Scene, layer_id: u32) -> Option<(tvp_visual::scene::Rect, bool)> {
    let mut x = 0i32;
    let mut y = 0i32;
    let mut visible = true;
    let mut current = Some(layer_id);
    let (mut w, mut h) = (0, 0);
    while let Some(id) = current {
        let layer = scene.layer(id)?;
        if id == layer_id {
            w = layer.rect.w;
            h = layer.rect.h;
        }
        x += layer.rect.x;
        y += layer.rect.y;
        visible &= layer.visible;
        current = layer.parent;
    }
    Some((tvp_visual::scene::Rect { x, y, w, h }, visible))
}

/// The reference `_HitTestNoVisibleCheck` for `htMask` (`htProvince` and
/// province images are not modelled). `hit_threshold == 0` accepts any pixel
/// (and any no-image layer); `256` rejects everything.
fn hit_test_self(scene: &Scene, layer: &tvp_visual::scene::LayerState, x: i32, y: i32) -> bool {
    if layer.hit_type == 1 {
        return false; // htProvince: no province image is tracked
    }
    match layer.bitmap.and_then(|id| scene.bitmap(id)) {
        Some(bmp) => {
            let iw = if layer.image_width > 0 {
                layer.image_width
            } else {
                bmp.width
            };
            let ih = if layer.image_height > 0 {
                layer.image_height
            } else {
                bmp.height
            };
            let px = x - layer.image_left;
            let py = y - layer.image_top;
            if px < 0 || py < 0 || px >= iw as i32 || py >= ih as i32 {
                return false;
            }
            if layer.hit_threshold <= 0 {
                return true;
            }
            let bx = ((px as u32) * bmp.width / iw.max(1)).min(bmp.width.saturating_sub(1));
            let by = ((py as u32) * bmp.height / ih.max(1)).min(bmp.height.saturating_sub(1));
            let alpha = bmp
                .rgba
                .get((by as usize * bmp.width as usize + bx as usize) * 4 + 3)
                .copied()
                .unwrap_or(0);
            alpha as i32 >= layer.hit_threshold
        }
        None => layer.hit_threshold <= 0,
    }
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

    // Multi-click count for each button pressed this frame: 1 for `onClick`,
    // ≥2 for `onDoubleClick`. `Mouse.getClickCount` already applies the
    // reference sequence timing (time + movement window).
    let mut click_counts = [0u32; MOUSE_BUTTONS];
    for &b in &button_down {
        click_counts[b] = state.mouse_click_count(b);
    }

    FrameEvents {
        button_down,
        button_up,
        keys_down,
        keys_up,
        moved,
        position: pos,
        wheel,
        // The reference `TVP_SS_*` mask, owned by `tvp-input` so modifiers
        // and mouse-button VKs resolve identically everywhere.
        shift: state.shift_flags() as i32,
        click_counts,
    }
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
pub(crate) fn dispatch_to_windows(engine: &Tjs2Engine, window_ids: &[u32], events: &FrameEvents) {
    for &win_id in window_ids {
        let obj = tvp_visual::natives::window_tjs_object(win_id);
        if obj.is_null() {
            continue;
        }
        // Retain the object for the duration of this window's dispatch (the
        // engine MUST outlive the DetachedValue — the caller owns it).
        let Ok(dv) = engine.retain_object_detached(obj) else {
            log::warn!(
                "input bridge: window #{win_id} object cannot be retained; skipping its input"
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
            if std::env::var_os("KRKR_INPUT_TRACE").is_some() {
                eprintln!("[input] window #{win_id} onMouseDown button={b}");
            }
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
            // The native `Window.OnClick` forwards to the draw device after
            // posting the window `onClick`/`onDoubleClick` (`WindowIntf.cpp:316`).
            // A drag press has `click_counts == 0` and is not a click.
            if b == MB_LEFT && events.click_counts[b] >= 1 {
                let method = if events.click_counts[b] >= 2 {
                    "onDoubleClick"
                } else {
                    "onClick"
                };
                call_guarded(
                    engine,
                    id,
                    method,
                    &[
                        TjsValue::Integer(i64::from(x)),
                        TjsValue::Integer(i64::from(y)),
                    ],
                );
            }
        }
        for &b in &events.button_up {
            if std::env::var_os("KRKR_INPUT_TRACE").is_some() {
                eprintln!("[input] window #{win_id} onMouseUp button={b}");
            }
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
/// panicking) on failure — a window whose object was destroyed must not take
/// the app down. A missing handler is expected (the reference ignores
/// `TJS_E_MEMBERNOTFOUND` for posted events, `EventIntf.cpp:97-118`), so it is
/// skipped silently like [`execute_layer_calls`].
fn call_guarded(engine: &Tjs2Engine, id: Tjs2ValueId, method: &str, args: &[TjsValue]) {
    if let Err(e) = engine.call_member(id, method, args)
        && !is_missing_member_error(&e)
    {
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
        let shift = state.shift_flags() as i32;
        assert_eq!(shift, (1 << 0) | (1 << 2) | (1 << 4)); // ssShift|ssCtrl|ssRight
    }

    /// Bevy gamepad buttons map onto the KiriKiri `VK_PAD*` range, and every
    /// mapped code is a real pad code (`DInputMgn.cpp:686-700`).
    #[test]
    fn gamepad_buttons_map_to_kirikiri_pad_vks() {
        assert_eq!(
            gamepad_button_to_vk(GamepadButton::DPadLeft),
            Some(pad::VK_PADLEFT)
        );
        assert_eq!(
            gamepad_button_to_vk(GamepadButton::DPadUp),
            Some(pad::VK_PADUP)
        );
        assert_eq!(
            gamepad_button_to_vk(GamepadButton::DPadRight),
            Some(pad::VK_PADRIGHT)
        );
        assert_eq!(
            gamepad_button_to_vk(GamepadButton::DPadDown),
            Some(pad::VK_PADDOWN)
        );
        assert_eq!(
            gamepad_button_to_vk(GamepadButton::South),
            Some(pad::VK_PAD1)
        );
        assert_eq!(
            gamepad_button_to_vk(GamepadButton::East),
            Some(pad::VK_PAD2)
        );
        assert_eq!(
            gamepad_button_to_vk(GamepadButton::West),
            Some(pad::VK_PAD3)
        );
        assert_eq!(
            gamepad_button_to_vk(GamepadButton::North),
            Some(pad::VK_PAD4)
        );
        assert_eq!(
            gamepad_button_to_vk(GamepadButton::Start),
            Some(pad::VK_PAD8)
        );
        assert_eq!(
            gamepad_button_to_vk(GamepadButton::RightThumb),
            Some(pad::VK_PAD10)
        );
        // Buttons with no KiriKiri pad code are dropped, not mis-mapped.
        assert_eq!(gamepad_button_to_vk(GamepadButton::Mode), None);
        assert_eq!(gamepad_button_to_vk(GamepadButton::C), None);
        assert_eq!(gamepad_button_to_vk(GamepadButton::Other(3)), None);
        for button in GamepadButton::all() {
            if let Some(vk) = gamepad_button_to_vk(button) {
                assert!(
                    pad::is_pad_code(vk),
                    "{button:?} mapped to non-pad code {vk:#x}"
                );
            }
        }
    }

    /// `game_to_window_pixel` inverts the cursor transform (the OS-cursor
    /// warp path), including for a non-16:9 window where the game is
    /// letterboxed.
    #[test]
    fn game_to_window_pixel_inverts_the_cursor_transform() {
        let game = (1280u32, 720u32);
        for window in [
            (2560.0f32, 1440.0f32),
            (640.0, 360.0),
            (1000.0, 720.0),
            (800.0, 600.0),
        ] {
            let transform = crate::sync::window_to_game_transform(window, game);
            let (px, py) = game_to_window_pixel((640, 360), transform);
            assert!(
                (px - window.0 / 2.0).abs() < 1e-3 && (py - window.1 / 2.0).abs() < 1e-3,
                "game center must map to window center for {window:?}, got ({px}, {py})"
            );
        }
        // Exact 16:9 needs no offset.
        let transform = crate::sync::window_to_game_transform((2560.0, 1440.0), game);
        assert_eq!(game_to_window_pixel((0, 0), transform), (0.0, 0.0));
        assert_eq!(
            game_to_window_pixel((1280, 720), transform),
            (2560.0, 1440.0)
        );
    }

    /// The cursor mapping size must be the game's logical rendering
    /// resolution (the primary layer), not the zoomed OS client size, so
    /// hit-testing after a resolution change stays in the game's coordinate
    /// space. A window without a primary layer falls back to its client size.
    #[test]
    fn game_size_uses_primary_layer_not_zoomed_client_size() {
        use tvp_visual::scene::Rect;
        let mut scene = Scene::default();
        let win = scene.add_window("t", (1920, 1080));
        let primary = scene.add_layer(win, None);
        scene.layer_mut(primary).unwrap().rect = Rect {
            x: 0,
            y: 0,
            w: 1280,
            h: 720,
        };
        let shared = SharedScene(Arc::new(RwLock::new(scene)));
        assert_eq!(game_size(&shared), (1280, 720));

        let mut scene = Scene::default();
        scene.add_window("t", (1024, 768));
        let shared = SharedScene(Arc::new(RwLock::new(scene)));
        assert_eq!(game_size(&shared), (1024, 768));
    }

    /// Left-button presses complete a click gesture: the first fires
    /// `onClick`, a second within the multi-click window fires
    /// `onDoubleClick` instead.
    #[test]
    fn clicks_fire_on_click_then_double_click() {
        use tvp_visual::scene::Rect;
        let mut scene = Scene::default();
        let win = scene.add_window("t", (100, 100));
        let layer = scene.add_layer(win, None);
        {
            let l = scene.layer_mut(layer).unwrap();
            l.rect = Rect {
                x: 0,
                y: 0,
                w: 100,
                h: 100,
            };
            l.visible = true;
            l.hit_threshold = 0;
        }
        let mut bridge = BridgeState::default();
        let mut state = InputState::new();

        // Press 1 → onMouseDown + onClick (count 1).
        state.begin_frame();
        state.set_mouse_pos(10, 10);
        state.set_mouse_button(MB_LEFT, true);
        state.end_frame();
        let events = collect_frame_events(&state, &mut bridge);
        assert_eq!(
            events.click_counts[MB_LEFT], 1,
            "first press is a single click"
        );
        let calls = plan_layer_calls(&scene, &events, &mut bridge);
        assert!(
            calls
                .iter()
                .any(|c| c.layer_id == layer && c.method == "onMouseDown")
        );
        assert!(
            calls
                .iter()
                .any(|c| c.layer_id == layer && c.method == "onClick"),
            "left press must fire onClick, got {:?}",
            calls.iter().map(|c| c.method).collect::<Vec<_>>()
        );
        assert!(!calls.iter().any(|c| c.method == "onDoubleClick"));

        // Release, then a second press at the same spot → onDoubleClick.
        state.begin_frame();
        state.set_mouse_button(MB_LEFT, false);
        state.end_frame();
        let events = collect_frame_events(&state, &mut bridge);
        let _ = plan_layer_calls(&scene, &events, &mut bridge);
        state.begin_frame();
        state.set_mouse_button(MB_LEFT, true);
        state.end_frame();
        let events = collect_frame_events(&state, &mut bridge);
        assert_eq!(
            events.click_counts[MB_LEFT], 2,
            "second press is a double click"
        );
        let calls = plan_layer_calls(&scene, &events, &mut bridge);
        assert!(
            calls
                .iter()
                .any(|c| c.layer_id == layer && c.method == "onDoubleClick"),
            "second press must fire onDoubleClick, got {:?}",
            calls.iter().map(|c| c.method).collect::<Vec<_>>()
        );
        assert!(!calls.iter().any(|c| c.method == "onClick"));
    }

    /// Releasing a second button must not drop the capture owned by a button
    /// that is still held (reference `PrimaryMouseUp` keeps the capture until
    /// no mouse button is pressed).
    #[test]
    fn releasing_a_second_button_keeps_left_capture() {
        use tvp_visual::scene::Rect;
        let mut scene = Scene::default();
        let win = scene.add_window("t", (100, 100));
        let layer = scene.add_layer(win, None);
        {
            let l = scene.layer_mut(layer).unwrap();
            l.rect = Rect {
                x: 0,
                y: 0,
                w: 100,
                h: 100,
            };
            l.visible = true;
            l.hit_threshold = 0;
        }
        let mut bridge = BridgeState::default();
        let mut state = InputState::new();

        // Left press → capture the layer.
        state.begin_frame();
        state.set_mouse_button(MB_LEFT, true);
        state.end_frame();
        let events = collect_frame_events(&state, &mut bridge);
        let _ = plan_layer_calls(&scene, &events, &mut bridge);
        assert_eq!(bridge.capture_layer, Some(layer));

        // Right press then release while left stays held → capture retained.
        state.begin_frame();
        state.set_mouse_button(MB_RIGHT, true);
        state.end_frame();
        let events = collect_frame_events(&state, &mut bridge);
        let _ = plan_layer_calls(&scene, &events, &mut bridge);
        state.begin_frame();
        state.set_mouse_button(MB_RIGHT, false);
        state.end_frame();
        let events = collect_frame_events(&state, &mut bridge);
        assert!(
            any_mouse_button_pressed(events.shift as u32),
            "left is still held, so the shift mask still has a button bit"
        );
        let _ = plan_layer_calls(&scene, &events, &mut bridge);
        assert_eq!(
            bridge.capture_layer,
            Some(layer),
            "releasing right must not drop left's capture"
        );

        // Releasing left now does release the capture.
        state.begin_frame();
        state.set_mouse_button(MB_LEFT, false);
        state.end_frame();
        let events = collect_frame_events(&state, &mut bridge);
        let _ = plan_layer_calls(&scene, &events, &mut bridge);
        assert_eq!(bridge.capture_layer, None);
    }

    /// Queue draining preserves button edges across frames: a tap's press and
    /// release must never be applied in the same frame, or the sampled
    /// `collect_frame_events` diff would see no edge at all.
    #[test]
    fn pending_actions_apply_one_button_transition_per_frame() {
        let mut state = InputState::new();
        let mut queue = VecDeque::from(vec![
            MouseAction::MoveTo { x: 10, y: 10 },
            MouseAction::Button {
                button: GestureButton::Left,
                down: true,
            },
            MouseAction::MoveTo { x: 20, y: 20 },
            MouseAction::Button {
                button: GestureButton::Left,
                down: false,
            },
        ]);

        // Frame 1: the move before the press plus the press itself.
        apply_pending_mouse_actions(&mut state, &mut queue);
        assert_eq!((state.mouse.x, state.mouse.y), (10, 10));
        assert!(state.is_mouse_button_down(MB_LEFT));
        assert_eq!(queue.len(), 2, "the post-press move+release stay queued");

        // Frame 2: the move then the release.
        apply_pending_mouse_actions(&mut state, &mut queue);
        assert_eq!((state.mouse.x, state.mouse.y), (20, 20));
        assert!(!state.is_mouse_button_down(MB_LEFT));
        assert!(queue.is_empty());
    }

    /// A drag press holds the left button but is not a click: it must not
    /// produce `onClick` and must not extend a preceding tap's double-click
    /// sequence.
    #[test]
    fn drag_press_holds_button_without_clicking() {
        use tvp_visual::scene::Rect;
        let mut scene = Scene::default();
        let win = scene.add_window("t", (200, 200));
        let layer = scene.add_layer(win, None);
        {
            let l = scene.layer_mut(layer).unwrap();
            l.rect = Rect {
                x: 0,
                y: 0,
                w: 200,
                h: 200,
            };
            l.visible = true;
            l.hit_threshold = 0;
        }
        let mut bridge = BridgeState::default();
        let mut state = InputState::new();

        // Start a click sequence with a real tap press on the first frame.
        state.begin_frame();
        state.set_mouse_button(MB_LEFT, true);
        state.end_frame();
        let events = collect_frame_events(&state, &mut bridge);
        assert_eq!(events.click_counts[MB_LEFT], 1);
        let calls = plan_layer_calls(&scene, &events, &mut bridge);
        assert!(calls.iter().any(|c| c.method == "onClick"));
        // Release.
        state.begin_frame();
        state.set_mouse_button(MB_LEFT, false);
        state.end_frame();
        let events = collect_frame_events(&state, &mut bridge);
        let _ = plan_layer_calls(&scene, &events, &mut bridge);

        // A drag press is raw: held, count 0, and no `onClick`.
        state.begin_frame();
        bridge
            .pending_mouse_actions
            .push_back(MouseAction::PressWithoutClick {
                button: GestureButton::Left,
            });
        apply_pending_mouse_actions(&mut state, &mut bridge.pending_mouse_actions);
        state.end_frame();
        let events = collect_frame_events(&state, &mut bridge);
        assert_eq!(events.button_down, vec![MB_LEFT]);
        assert_eq!(events.click_counts[MB_LEFT], 0);
        let calls = plan_layer_calls(&scene, &events, &mut bridge);
        assert!(
            calls
                .iter()
                .any(|c| c.layer_id == layer && c.method == "onMouseDown")
        );
        assert!(
            !calls
                .iter()
                .any(|c| c.method == "onClick" || c.method == "onDoubleClick"),
            "a drag press must not click, got {:?}",
            calls.iter().map(|c| c.method).collect::<Vec<_>>()
        );

        // The next real tap starts a fresh count of 1, not 2.
        state.begin_frame();
        state.set_mouse_button(MB_LEFT, false);
        state.end_frame();
        let _ = collect_frame_events(&state, &mut bridge);
        state.begin_frame();
        state.set_mouse_button(MB_LEFT, true);
        state.end_frame();
        let events = collect_frame_events(&state, &mut bridge);
        assert_eq!(events.click_counts[MB_LEFT], 1);
    }

    /// A bridged tap reaches the layer as a normal left `onMouseDown` +
    /// `onMouseUp` (with no `onTouch*`): the gesture's action queue is drained
    /// one button transition per frame and the mouse dispatch does the rest.
    #[test]
    fn touch_tap_bridges_to_layer_mouse_events() {
        use tvp_visual::scene::Rect;
        let mut scene = Scene::default();
        let win = scene.add_window("t", (200, 200));
        let layer = scene.add_layer(win, None);
        {
            let l = scene.layer_mut(layer).unwrap();
            l.rect = Rect {
                x: 0,
                y: 0,
                w: 50,
                h: 50,
            };
            l.visible = true;
            l.hit_threshold = 0;
        }

        let mut bridge = BridgeState::default();
        let mut gesture = TouchGesture::default();
        let mut state = InputState::new();
        let down = Duration::ZERO;

        // Frame 1: finger down at (10, 10) — only a cursor move so far.
        state.begin_frame();
        bridge.pending_mouse_actions.extend(gesture.update(
            down,
            &[TouchSample {
                id: 1,
                phase: GesturePhase::Started,
                x: 10,
                y: 10,
                time: down,
            }],
        ));
        apply_pending_mouse_actions(&mut state, &mut bridge.pending_mouse_actions);
        state.end_frame();
        let events = collect_frame_events(&state, &mut bridge);
        assert!(events.button_down.is_empty());

        // Frame 2: lift before the long-press timeout — a tap. The press is
        // applied now, the release is deferred to the next frame.
        let up = Duration::from_millis(80);
        state.begin_frame();
        bridge.pending_mouse_actions.extend(gesture.update(
            up,
            &[TouchSample {
                id: 1,
                phase: GesturePhase::Ended,
                x: 12,
                y: 11,
                time: up,
            }],
        ));
        apply_pending_mouse_actions(&mut state, &mut bridge.pending_mouse_actions);
        state.end_frame();
        let events = collect_frame_events(&state, &mut bridge);
        assert_eq!(events.button_down, vec![MB_LEFT]);
        let calls = plan_layer_calls(&scene, &events, &mut bridge);
        assert!(
            calls
                .iter()
                .any(|c| c.layer_id == layer && c.method == "onMouseDown"),
            "tap must press the layer, got {:?}",
            calls.iter().map(|c| c.method).collect::<Vec<_>>()
        );
        assert!(
            calls.iter().any(|c| c.method == "onClick"),
            "tap must click, got {:?}",
            calls.iter().map(|c| c.method).collect::<Vec<_>>()
        );
        assert_eq!(bridge.capture_layer, Some(layer));

        // Frame 3: the deferred release arrives as an up edge.
        state.begin_frame();
        apply_pending_mouse_actions(&mut state, &mut bridge.pending_mouse_actions);
        state.end_frame();
        let events = collect_frame_events(&state, &mut bridge);
        assert_eq!(events.button_up, vec![MB_LEFT]);
        let calls = plan_layer_calls(&scene, &events, &mut bridge);
        assert!(
            calls
                .iter()
                .any(|c| c.layer_id == layer && c.method == "onMouseUp"),
            "the capture layer must receive the up"
        );
        assert_eq!(bridge.capture_layer, None);
        assert!(bridge.pending_mouse_actions.is_empty());
    }

    /// A long press bridges to a right-click, which never fires `onClick`.
    #[test]
    fn touch_long_press_bridges_to_right_click() {
        let mut bridge = BridgeState::default();
        let mut gesture = TouchGesture::default();
        let mut state = InputState::new();

        state.begin_frame();
        let _ = gesture.update(
            Duration::ZERO,
            &[TouchSample {
                id: 1,
                phase: GesturePhase::Started,
                x: 5,
                y: 5,
                time: Duration::ZERO,
            }],
        );
        state.end_frame();
        let _ = collect_frame_events(&state, &mut bridge);

        // The long-press deadline passes with no further touch samples.
        let deadline = Duration::from_millis(500);
        state.begin_frame();
        bridge
            .pending_mouse_actions
            .extend(gesture.update(deadline, &[]));
        apply_pending_mouse_actions(&mut state, &mut bridge.pending_mouse_actions);
        state.end_frame();
        let events = collect_frame_events(&state, &mut bridge);
        assert_eq!(events.button_down, vec![MB_RIGHT]);
        assert_eq!(events.click_counts[MB_LEFT], 0);
    }

    /// Layer hit-testing mirrors the reference `GetMostFrontChildAt`:
    /// frontmost child wins, the layer rect clips, and `htMask` alpha is
    /// compared against `hitThreshold`.
    #[test]
    fn hit_test_finds_frontmost_layer_and_respects_mask() {
        use tvp_visual::scene::Rect;
        let mut scene = Scene::default();
        let win = scene.add_window("t", (200, 200));

        // Background: full window, fully opaque.
        let bg = scene.add_layer(win, None);
        let bg_bmp = scene.add_bitmap(4, 4, vec![255u8; 4 * 4 * 4]);
        {
            let l = scene.layer_mut(bg).unwrap();
            l.rect = Rect {
                x: 0,
                y: 0,
                w: 200,
                h: 200,
            };
            l.bitmap = Some(bg_bmp);
            l.image_width = 200;
            l.image_height = 200;
            l.hit_threshold = 0;
            l.visible = true;
        }

        // Button on top (50,50,40x40), all pixels opaque except (0,0).
        let btn = scene.add_layer(win, None);
        let mut rgba = vec![255u8; 4 * 4 * 4];
        rgba[3] = 0; // pixel (0,0) alpha 0
        let btn_bmp = scene.add_bitmap(4, 4, rgba);
        {
            let l = scene.layer_mut(btn).unwrap();
            l.rect = Rect {
                x: 50,
                y: 50,
                w: 40,
                h: 40,
            };
            l.bitmap = Some(btn_bmp);
            l.image_width = 40;
            l.image_height = 40;
            l.hit_threshold = 1;
            l.visible = true;
        }

        // Opaque button pixel -> the button.
        assert_eq!(hit_test(&scene, win, 60, 60), Some(btn));
        // Transparent button pixel -> falls through to the background.
        assert_eq!(hit_test(&scene, win, 50, 50), Some(bg));
        // Outside everything.
        assert_eq!(hit_test(&scene, win, 190, 190), Some(bg));
        assert_eq!(hit_test(&scene, win, 500, 500), None);

        // With no bitmap and hitThreshold 0, the layer rect itself hits.
        let flat = scene.add_layer(win, None);
        {
            let l = scene.layer_mut(flat).unwrap();
            l.rect = Rect {
                x: 0,
                y: 0,
                w: 10,
                h: 10,
            };
            l.visible = true;
            l.hit_threshold = 0;
        }
        assert_eq!(hit_test(&scene, win, 5, 5), Some(flat));
    }

    /// Layer-local coordinates subtract every ancestor offset.
    #[test]
    fn layer_local_subtracts_ancestors() {
        use tvp_visual::scene::Rect;
        let mut scene = Scene::default();
        let win = scene.add_window("t", (200, 200));
        let parent = scene.add_layer(win, None);
        scene.layer_mut(parent).unwrap().rect = Rect {
            x: 100,
            y: 50,
            w: 100,
            h: 100,
        };
        let child = scene.add_layer(win, Some(parent));
        scene.layer_mut(child).unwrap().rect = Rect {
            x: 10,
            y: 20,
            w: 30,
            h: 30,
        };
        assert_eq!(layer_local(&scene, child, 130, 90), (20, 20));
    }

    /// Container layers like the game's `AffineLayer` keep a `0×0` rect
    /// (they size only their `_image` child; the parent rect is fixed up by
    /// the script `onPaint` we do not run). A child must still be hittable by
    /// its absolute rect.
    #[test]
    fn hit_test_reaches_children_of_zero_size_containers() {
        use tvp_visual::scene::Rect;
        let mut scene = Scene::default();
        let win = scene.add_window("t", (400, 300));
        let container = scene.add_layer(win, None);
        scene.layer_mut(container).unwrap().visible = true;
        // 0x0 container, as the real title root ends up.
        let btn = scene.add_layer(win, Some(container));
        {
            let l = scene.layer_mut(btn).unwrap();
            l.rect = Rect {
                x: 50,
                y: 50,
                w: 40,
                h: 40,
            };
            l.visible = true;
            l.fill_color = Some([255, 255, 255, 255]);
            l.hit_threshold = 0;
        }
        assert_eq!(hit_test(&scene, win, 60, 60), Some(btn));
        assert_eq!(hit_test(&scene, win, 10, 10), None);
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
                    function onClick(x, y) {
                        global.__log += "c:" + x + "," + y + ";";
                    }
                    function onDoubleClick(x, y) {
                        global.__log += "dc:" + x + "," + y + ";";
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
            // Compute the window ids, then drop the scene guard before the VM
            // calls (holding it across `call_member` deadlocks a handler that
            // takes a scene lock).
            let window_ids: Vec<u32> = scene.read().unwrap().windows.iter().map(|w| w.id).collect();
            dispatch_to_windows(engine.as_ref(), &window_ids, &events);
            drop(s);
            (events, read_log(&engine))
        };
        // ssLeft is set while the button is held → all handlers see shift=8.
        // The left press also fires the window `onClick` (click count 1),
        // after `onMouseDown` and before the key edge.
        assert_eq!(
            log, "m:320,240,8;d:320,240,0,8;c:320,240;k:13,8;",
            "move → onMouseDown → onClick → onKeyDown (button=mbLeft=0, VK_RETURN=13)"
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
            let window_ids: Vec<u32> = scene.read().unwrap().windows.iter().map(|w| w.id).collect();
            dispatch_to_windows(engine.as_ref(), &window_ids, &events);
            drop(s);
            read_log(&engine)
        };
        // Nothing held on release → shift=0 for the up edges.
        assert_eq!(log, "u:320,240,0,0;ku:13,0;");
    }

    /// A layer whose TJS object is invalidated while the layer is still in
    /// the scene must be marked dead on the first failed dispatch and then
    /// skipped, instead of throwing `The object is already invalidated` on
    /// every mouse-move frame forever (see [`BridgeState::dead_layers`]).
    #[test]
    fn invalidated_layer_is_marked_dead_and_skipped() {
        let _vm_lock = tvp_visual::natives::vm_test_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        let dir = std::env::temp_dir().join(format!(
            "krkr-rs-input-bridge-dead-layer-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp game dir");

        let engine = Arc::new(Tjs2Engine::new().expect("engine"));
        tvp_input::register_all(&engine).expect("register Mouse + Key");
        let storage = Arc::new(Mutex::new(
            engine::Storage::mount(dir.to_str().unwrap()).expect("mount"),
        ));
        let scene = Arc::new(RwLock::new(Scene::default()));
        tvp_visual::register_visual(&engine, scene.clone(), storage.clone())
            .expect("register visual natives");

        // A Window + a hittable Layer subclass that records onMouseMove.
        engine
            .exec_script(
                r#"
                global.__log = "";
                class TestWin extends Window {
                    function TestWin() { super.Window(); }
                }
                class TestLayer extends Layer {
                    function TestLayer(win, par) { super.Layer(win, par); }
                    function onMouseMove(x, y, shift) {
                        global.__log += "m:" + x + "," + y + "," + shift + ";";
                    }
                }
                var w = new TestWin();
                var l = new TestLayer(w, null);
                global.__layerId = l.id;
                "#,
                "test",
            )
            .expect("define TestWin/TestLayer");

        let layer_id = match engine.eval("global.__layerId", "test") {
            Ok(TjsValue::Integer(id)) => id as u32,
            other => panic!("global.__layerId -> {other:?}"),
        };
        assert!(!tvp_visual::natives::layer_tjs_object(layer_id).is_null());

        // Make the layer hit-testable across the whole window.
        {
            let mut sc = scene.write().unwrap();
            let l = sc.layer_mut(layer_id).expect("layer");
            l.rect = tvp_visual::scene::Rect {
                x: 0,
                y: 0,
                w: 200,
                h: 200,
            };
            l.visible = true;
            l.hit_threshold = 0;
        }

        let mut bridge = BridgeState::default();

        // Frame 1: the live layer receives `onMouseMove` exactly once.
        let calls = dispatch_layer_frame(&scene, &mut bridge, (10, 10));
        assert!(
            calls
                .iter()
                .any(|c| c.layer_id == layer_id && c.method == "onMouseMove"),
            "the live layer should receive onMouseMove"
        );
        execute_layer_calls(engine.as_ref(), &calls, &mut bridge);
        assert!(!bridge.dead_layers.contains(&layer_id));
        assert_eq!(read_log(&engine), "m:10,10,0;");

        // `invalidate l;` finalizes the native payload. Since the
        // `tjs2-sys` lifetime fix the VM runs the native `destroy` at
        // finalize time, so `tvp-visual`'s `Layer.destroy` removes the
        // layer from the scene immediately (see
        // `crates/tvp-visual/src/natives/layer.rs::layer_destroy`). The
        // input bridge's blacklist still guards the window between a stale
        // object being dispatched to and the scene being rebuilt.
        engine.exec_script("invalidate l;", "test").unwrap();

        // Frame 2: a dispatch planned against the now-stale object is marked
        // dead on the first failure, its capture/enter state is cleared, and
        // the handler never runs again. Build the call directly because the
        // scene no longer offers the layer to the hit test.
        let stale = vec![LayerCall {
            layer_id,
            method: "onMouseMove",
            args: vec![
                TjsValue::Integer(20),
                TjsValue::Integer(20),
                TjsValue::Integer(0),
            ],
        }];
        execute_layer_calls(engine.as_ref(), &stale, &mut bridge);
        assert!(
            bridge.dead_layers.contains(&layer_id),
            "the invalidated layer must be marked dead"
        );
        assert_eq!(bridge.last_move_layer, None);
        assert_eq!(bridge.capture_layer, None);
        // The handler never ran again.
        assert_eq!(read_log(&engine), "m:10,10,0;");

        // Frame 3: the dead layer is excluded from the hit test, so nothing
        // is planned or dispatched for it even if a stale list entry remains.
        let calls = dispatch_layer_frame(&scene, &mut bridge, (30, 30));
        assert!(
            !calls.iter().any(|c| c.layer_id == layer_id),
            "dead layers must not be planned again"
        );
        execute_layer_calls(engine.as_ref(), &calls, &mut bridge);
        assert_eq!(read_log(&engine), "m:10,10,0;");
    }

    /// A layer recorded in [`BridgeState::dead_layers`] is treated as no-hit
    /// even while it is still present in the scene, so a lower live layer (or
    /// nothing) receives the event instead of the stale object.
    #[test]
    fn dead_layers_are_excluded_from_hit_testing() {
        use tvp_visual::scene::Rect;
        let mut scene = Scene::default();
        let win = scene.add_window("t", (200, 200));
        let layer = scene.add_layer(win, None);
        {
            let l = scene.layer_mut(layer).unwrap();
            l.rect = Rect {
                x: 0,
                y: 0,
                w: 200,
                h: 200,
            };
            l.visible = true;
            l.hit_threshold = 0;
        }
        assert_eq!(hit_test(&scene, win, 5, 5), Some(layer));

        let mut dead = HashSet::new();
        assert_eq!(hit_test_excluding(&scene, win, 5, 5, &dead), Some(layer));
        dead.insert(layer);
        assert_eq!(
            hit_test_excluding(&scene, win, 5, 5, &dead),
            None,
            "a dead layer is treated as no-hit"
        );
    }

    /// `mark_layer_dead` drops any capture/enter state pointing at the dead
    /// layer and reports the first transition only.
    #[test]
    fn mark_layer_dead_clears_capture_and_enter_state() {
        let mut bridge = BridgeState {
            last_move_layer: Some(7),
            capture_layer: Some(7),
            ..Default::default()
        };
        assert!(mark_layer_dead(&mut bridge, 7), "first mark is reported");
        assert!(bridge.dead_layers.contains(&7));
        assert_eq!(bridge.last_move_layer, None);
        assert_eq!(bridge.capture_layer, None);
        assert!(!mark_layer_dead(&mut bridge, 7), "second mark is quiet");
    }

    /// A `call_member` failure that means "no such handler" must be treated
    /// as an absent optional event, never logged. The legacy `tjs2_call_member`
    /// build reports the name as `Member "" does not exist`; the fixed build
    /// names the member. Both must classify as missing (the reference stops at
    /// `TJS_E_MEMBERNOTFOUND` for posted events, `EventIntf.cpp:97-118`).
    #[test]
    fn missing_member_errors_are_treated_as_no_handler() {
        assert!(is_missing_member_error("Member \"onClick\" does not exist"));
        assert!(is_missing_member_error("Member \"\" does not exist"));
        assert!(!is_missing_member_error(
            "The object is already invalidated"
        ));
        assert!(!is_missing_member_error("some other failure"));
    }

    /// A window that does not define the optional `onClick`/`onDoubleClick`
    /// handlers still receives `onMouseDown`, and the missing handlers are
    /// skipped without aborting the dispatch (the reference posts every event
    /// and ignores `TJS_E_MEMBERNOTFOUND`).
    #[test]
    fn window_without_optional_handlers_dispatches_safely() {
        let _vm_lock = tvp_visual::natives::vm_test_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        let dir = std::env::temp_dir().join(format!(
            "krkr-rs-input-bridge-optional-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp game dir");

        let engine = Arc::new(Tjs2Engine::new().expect("engine"));
        tvp_input::register_all(&engine).expect("register Mouse + Key");
        let storage = Arc::new(Mutex::new(
            engine::Storage::mount(dir.to_str().unwrap()).expect("mount"),
        ));
        let scene = Arc::new(RwLock::new(Scene::default()));
        tvp_visual::register_visual(&engine, scene.clone(), storage.clone())
            .expect("register visual natives");

        // Deliberately no onClick/onDoubleClick/onTouch* overrides.
        engine
            .exec_script(
                r#"
                global.__log = "";
                class PlainWin extends Window {
                    function PlainWin() { super.Window(); }
                    function onMouseDown(x, y, button, shift) {
                        global.__log += "d:" + x + "," + y + "," + button + ";";
                    }
                }
                var w = new PlainWin();
                "#,
                "test",
            )
            .expect("define PlainWin");

        let window_ids: Vec<u32> = scene.read().unwrap().windows.iter().map(|w| w.id).collect();
        assert!(!window_ids.is_empty());

        // A left press used to log `window.onClick failed: Member "" does
        // not exist`; it must now be silent, and `onMouseDown` must run.
        let events = FrameEvents {
            button_down: vec![MB_LEFT],
            button_up: Vec::new(),
            keys_down: Vec::new(),
            keys_up: Vec::new(),
            moved: None,
            position: (12, 34),
            wheel: None,
            shift: 0,
            click_counts: [1; MOUSE_BUTTONS],
        };
        dispatch_to_windows(engine.as_ref(), &window_ids, &events);
        assert_eq!(read_log(&engine), "d:12,34,0;");
    }

    /// Drive one synthetic mouse-move frame through `collect_frame_events` +
    /// `plan_layer_calls` (the pure steps `dispatch_input` runs under the
    /// scene lock) and return the planned layer calls.
    fn dispatch_layer_frame(
        scene: &Arc<RwLock<Scene>>,
        bridge: &mut BridgeState,
        pos: (i32, i32),
    ) -> Vec<LayerCall> {
        let state = tvp_input::input_state();
        let mut s = state.lock().unwrap();
        s.begin_frame();
        s.set_mouse_pos(pos.0, pos.1);
        s.end_frame();
        let events = collect_frame_events(&s, bridge);
        drop(s);
        let sc = scene.read().unwrap();
        plan_layer_calls(&sc, &events, bridge)
    }

    fn read_log(engine: &Tjs2Engine) -> String {
        match engine.eval("global.__log", "test") {
            Ok(TjsValue::String(s)) => s,
            Ok(TjsValue::Retained(_)) => panic!("global.__log retained (unexpected)"),
            other => panic!("global.__log -> {other:?}"),
        }
    }
}
