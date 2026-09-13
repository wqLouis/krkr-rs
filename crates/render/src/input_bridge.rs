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
    // The scene camera uses `ScalingMode::AutoMin` (aspect-preserving), so map
    // the cursor with the same uniform scale + centering offset instead of
    // stretching each axis independently (see `sync::window_to_game_transform`).
    let (scale, offset_x, offset_y) = match windows.iter().next() {
        Some(w) if w.width() > 0.0 && w.height() > 0.0 => {
            krkr_render::sync::window_to_game_transform((w.width(), w.height()), (game_w, game_h))
        }
        _ => krkr_render::sync::window_to_game_transform(
            (game_w as f32, game_h as f32),
            (game_w, game_h),
        ),
    };

    let state = tvp_input::input_state();
    let mut s = state.lock().unwrap_or_else(|p| p.into_inner());
    s.begin_frame();

    // Cursor position: the last CursorMoved of the frame, clamped to the
    // game bounds.
    if let Some(pos) = cursor_moved.read().last() {
        let x = ((pos.position.x / scale + offset_x).round() as i32)
            .clamp(0, game_w.saturating_sub(1) as i32);
        let y = ((pos.position.y / scale + offset_y).round() as i32)
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
    if std::env::var_os("KRKR_INPUT_TRACE").is_some() {
        eprintln!(
            "[input] down={:?} up={:?} moved={:?} pos={:?} wheel={:?}",
            events.button_down, events.button_up, events.moved, events.position, events.wheel,
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
    // to the layer under the cursor. Mirror both.
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

    // Mouse down: hit-test (unless captured) and remember the owner.
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
        }
    }

    // Mouse up: deliver to the capture owner, then release it.
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
            } else if !e.contains("does not exist") {
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

        // Invalidate the layer's TJS object while the layer stays in the
        // scene (exactly what the game does).
        engine.exec_script("invalidate l;", "test").unwrap();

        // Frame 2: the dispatch hits the invalidated object; the layer is
        // marked dead on the first failure and the enter/capture state is
        // cleared, instead of logging once per frame forever.
        let calls = dispatch_layer_frame(&scene, &mut bridge, (20, 20));
        assert!(calls.iter().any(|c| c.layer_id == layer_id));
        execute_layer_calls(engine.as_ref(), &calls, &mut bridge);
        assert!(
            bridge.dead_layers.contains(&layer_id),
            "the invalidated layer must be marked dead"
        );
        assert_eq!(bridge.last_move_layer, None);
        assert_eq!(bridge.capture_layer, None);
        // The handler never ran again.
        assert_eq!(read_log(&engine), "m:10,10,0;");

        // Frame 3: the dead layer is excluded from the hit test, so nothing
        // is planned or dispatched for it.
        let calls = dispatch_layer_frame(&scene, &mut bridge, (30, 30));
        assert!(
            !calls.iter().any(|c| c.layer_id == layer_id),
            "dead layers must not be planned again"
        );
        execute_layer_calls(engine.as_ref(), &calls, &mut bridge);
        assert_eq!(read_log(&engine), "m:10,10,0;");
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
