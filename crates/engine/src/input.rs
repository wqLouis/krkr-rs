//! L2 generic input state.
//!
//! L1 (`platform`) emits input events; L2 folds them into an explicit state
//! value ([`InputState`]) that the rest of the engine and L3 read. The state
//! is generic — pointer position, buttons, wheel, key edges and multi-touch
//! contacts — with no TVP/VK semantics; the TVP `Mouse`/`Key` surface (VK
//! codes, click-sequence constants, shift flags) belongs to L3.
//!
//! Behavioral reference: the L3 port in `compat/src/natives/input.rs` and
//! `main`'s `crates/tvp-input`.
//!
//! # Frame protocol
//!
//! The host clears the per-frame data at the start of the frame, drains the
//! raw events, then commits so held counters advance exactly once:
//!
//! ```text
//! begin_frame            // clear release flags, wheel, last-pressed
//!   apply(event) ...     // fold in every raw L1 event
//! end_frame              // bump held counters, frame += 1
//! ```
//!
//! [`InputState`] owns no global and no clock: all state is explicit.

use api::host::{TouchPhase, TouchPoint};
use api::input::{InputEvent, MouseButton};

use crate::hasher::{Map, map};

/// Default number of frames a follow-up press stays in the same click
/// sequence.
///
/// L3 overrides this with its TVP-specific value through
/// [`InputState::set_click_sequence`].
pub const DEFAULT_CLICK_SEQUENCE_FRAMES: u64 = 30;
/// Default maximum pointer movement for a press to continue a click sequence.
pub(crate) const DEFAULT_CLICK_MAX_MOVE: f32 = 4.0;

/// Per-button state tracked by [`InputState`].
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
struct ButtonState {
    /// Whether the button is currently held.
    pub held: bool,
    /// Whether the button was released during the current frame.
    pub released: bool,
    /// Consecutive frames the button has been held (bumped by `end_frame`).
    pub hold_frames: u32,
    /// Presses in the current multi-click sequence.
    pub click_count: u32,
    /// Pointer position of the last press in the sequence.
    pub last_click_pos: (f32, f32),
    /// Frame index of the last press (`u64::MAX` = never).
    pub last_click_frame: u64,
}

impl Default for ButtonState {
    fn default() -> Self {
        Self {
            held: false,
            released: false,
            hold_frames: 0,
            click_count: 0,
            last_click_pos: (0.0, 0.0),
            last_click_frame: u64::MAX,
        }
    }
}

/// Per-key state tracked by [`InputState`], keyed by the opaque platform code.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
struct KeyState {
    /// Whether the key is currently held.
    pub held: bool,
    /// Whether the key was released during the current frame.
    pub released: bool,
    /// Consecutive frames the key has been held (bumped by `end_frame`).
    pub hold_frames: u32,
}

/// One active touch contact.
///
/// The contact stores the latest [`TouchPoint`] plus the position at the
/// previous update, so [`InputState::touch_velocity`] can report the delta.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct TouchContact {
    /// The latest point reported for the contact.
    pub point: TouchPoint,
    /// Position where the contact started.
    pub start_pos: (f32, f32),
    /// Position at the previous update.
    pub prev_pos: (f32, f32),
}

/// The generic input state fed by [`api::input::InputEvent`].
///
/// See the module docs for the frame protocol. The state is intentionally
/// TVP-free: L3 owns the virtual-key table, the click-sequence constants and
/// the `TVP_SS_*` flags.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct InputState {
    pointer: (f32, f32),
    pointer_inside: bool,
    /// Whether the pointer is visible.
    pub pointer_visible: bool,
    warp_request: Option<(f32, f32)>,
    buttons: Map<MouseButton, ButtonState>,
    wheel: f32,
    keys: Map<u32, KeyState>,
    last_pressed: Vec<u32>,
    touches: Map<u64, TouchContact>,
    text_input: String,
    frame: u64,
    click_sequence_frames: u64,
    click_max_move: f32,
}

impl Default for InputState {
    fn default() -> Self {
        Self {
            pointer: (0.0, 0.0),
            pointer_inside: false,
            pointer_visible: true,
            warp_request: None,
            buttons: map(),
            wheel: 0.0,
            keys: map(),
            last_pressed: Vec::new(),
            touches: map(),
            text_input: String::new(),
            frame: 0,
            click_sequence_frames: DEFAULT_CLICK_SEQUENCE_FRAMES,
            click_max_move: DEFAULT_CLICK_MAX_MOVE,
        }
    }
}

impl InputState {
    /// Create a fresh, empty input state with the default click sequence.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an input state with an explicit click-sequence window.
    pub fn with_click_sequence(click_sequence_frames: u64, click_max_move: f32) -> Self {
        Self {
            click_sequence_frames,
            click_max_move,
            ..Self::default()
        }
    }

    // -- frame protocol ---------------------------------------------------

    /// Begin a new frame: clear the per-frame release flags, the wheel
    /// accumulation, the released keys and the last-pressed list.
    ///
    /// Held buttons/keys and active touch contacts are preserved.
    pub fn begin_frame(&mut self) {
        for button in self.buttons.values_mut() {
            button.released = false;
        }
        self.wheel = 0.0;
        self.keys.retain(|_, key| key.held);
        for key in self.keys.values_mut() {
            key.released = false;
        }
        self.last_pressed.clear();
        self.text_input.clear();
    }

    /// End the current frame: bump the held-frame counters and advance the
    /// frame index. Call exactly once per frame.
    pub fn end_frame(&mut self) {
        for button in self.buttons.values_mut() {
            if button.held {
                button.hold_frames = button.hold_frames.saturating_add(1);
            }
        }
        for key in self.keys.values_mut() {
            if key.held {
                key.hold_frames = key.hold_frames.saturating_add(1);
            }
        }
        self.frame = self.frame.saturating_add(1);
    }

    /// The monotonic frame counter (advanced by [`Self::end_frame`]).
    pub fn frame(&self) -> u64 {
        self.frame
    }

    /// Configure the click-sequence window used by [`Self::set_button`].
    ///
    /// L3 calls this with its TVP constants so the generic mechanism executes
    /// the TVP-specific policy.
    pub fn set_click_sequence(&mut self, frames: u64, max_move: f32) {
        self.click_sequence_frames = frames;
        self.click_max_move = max_move;
    }

    // -- feeding ----------------------------------------------------------

    /// Apply one raw L1 input event to the state.
    ///
    /// The match lists every [`InputEvent`] variant explicitly; there is no
    /// catch-all.
    pub fn apply(&mut self, event: &InputEvent) {
        match event {
            InputEvent::PointerMoved { x, y } => self.set_pointer(*x, *y),
            InputEvent::PointerLeft => self.pointer_inside = false,
            InputEvent::MouseButton { button, pressed } => self.set_button(*button, *pressed),
            InputEvent::Wheel { delta } => self.add_wheel(*delta),
            InputEvent::Key { code, pressed } => self.set_key(code.index(), *pressed),
            InputEvent::Text(text) => self.text_input.push_str(text),
            InputEvent::Touch { id, phase, x, y } => self.apply_touch(*id, *phase, *x, *y),
        }
    }

    // -- pointer ----------------------------------------------------------

    /// The current pointer position in physical window pixels.
    pub fn pointer(&self) -> (f32, f32) {
        self.pointer
    }

    /// Set the absolute pointer position.
    pub fn set_pointer(&mut self, x: f32, y: f32) {
        self.pointer = (x, y);
        self.pointer_inside = true;
    }

    /// Whether the pointer is currently inside the window.
    pub fn pointer_inside(&self) -> bool {
        self.pointer_inside
    }

    /// Move the logical pointer and queue a host cursor warp
    /// (`Mouse.setCursorPos`).
    pub fn warp_pointer(&mut self, x: f32, y: f32) {
        self.pointer = (x, y);
        self.warp_request = Some((x, y));
    }

    /// Take (and clear) the pending host cursor warp, if any.
    pub fn take_warp_request(&mut self) -> Option<(f32, f32)> {
        self.warp_request.take()
    }

    // -- mouse buttons ----------------------------------------------------

    /// Set a button's held state, advancing the multi-click sequence on a
    /// rising edge.
    pub fn set_button(&mut self, button: MouseButton, down: bool) {
        let pointer = self.pointer;
        let frame = self.frame;
        let sequence_frames = self.click_sequence_frames;
        let max_move = self.click_max_move;
        let entry = self.buttons.entry(button).or_default();
        if down == entry.held {
            return;
        }
        entry.held = down;
        if down {
            entry.hold_frames = 0;
            let close_in_time = entry.last_click_frame != u64::MAX
                && frame.saturating_sub(entry.last_click_frame) <= sequence_frames;
            let close_in_space = (pointer.0 - entry.last_click_pos.0).abs() <= max_move
                && (pointer.1 - entry.last_click_pos.1).abs() <= max_move;
            if close_in_time && close_in_space {
                entry.click_count = entry.click_count.saturating_add(1);
            } else {
                entry.click_count = 1;
            }
            entry.last_click_frame = frame;
            entry.last_click_pos = pointer;
        } else {
            entry.released = true;
            entry.hold_frames = 0;
        }
    }

    /// Set a button's held state **without** registering a click (used by
    /// gesture synthesis that drives a virtual button).
    pub fn set_button_raw(&mut self, button: MouseButton, down: bool) {
        let entry = self.buttons.entry(button).or_default();
        if down == entry.held {
            return;
        }
        entry.held = down;
        if down {
            entry.hold_frames = 0;
        } else {
            entry.released = true;
            entry.hold_frames = 0;
        }
    }

    /// Whether `button` is currently held.
    pub fn is_button_down(&self, button: MouseButton) -> bool {
        self.buttons.get(&button).is_some_and(|state| state.held)
    }

    /// Whether `button` was released during the current frame.
    pub fn is_button_released(&self, button: MouseButton) -> bool {
        self.buttons
            .get(&button)
            .is_some_and(|state| state.released)
    }

    /// Frames `button` has been held (0 when not held).
    pub fn button_hold_frames(&self, button: MouseButton) -> u32 {
        match self.buttons.get(&button) {
            Some(state) if state.held => state.hold_frames,
            _ => 0,
        }
    }

    /// Presses in the current multi-click sequence for `button` (0 once the
    /// sequence times out).
    pub fn button_click_count(&self, button: MouseButton) -> u32 {
        match self.buttons.get(&button) {
            Some(state)
                if state.last_click_frame != u64::MAX
                    && self.frame.saturating_sub(state.last_click_frame)
                        <= self.click_sequence_frames =>
            {
                state.click_count
            }
            _ => 0,
        }
    }

    /// Pointer position of the last press in `button`'s click sequence.
    pub fn button_last_click_pos(&self, button: MouseButton) -> Option<(f32, f32)> {
        self.buttons
            .get(&button)
            .filter(|state| state.last_click_frame != u64::MAX)
            .map(|state| state.last_click_pos)
    }

    /// Forget the current multi-click sequence for `button`.
    pub fn clear_click_sequence(&mut self, button: MouseButton) {
        if let Some(state) = self.buttons.get_mut(&button) {
            state.click_count = 0;
            state.last_click_frame = u64::MAX;
        }
    }

    // -- wheel ------------------------------------------------------------

    /// The wheel delta accumulated since the last [`Self::begin_frame`].
    pub fn wheel(&self) -> f32 {
        self.wheel
    }

    /// Accumulate a vertical wheel delta for the current frame.
    pub fn add_wheel(&mut self, delta: f32) {
        self.wheel += delta;
    }

    // -- keyboard ---------------------------------------------------------

    /// Record a key press/release edge for the opaque platform `code`.
    pub fn set_key(&mut self, code: u32, pressed: bool) {
        if pressed {
            let entry = self.keys.entry(code).or_default();
            if entry.held {
                return;
            }
            entry.held = true;
            entry.released = false;
            entry.hold_frames = 0;
            self.last_pressed.push(code);
        } else if let Some(entry) = self.keys.get_mut(&code)
            && entry.held
        {
            entry.held = false;
            entry.released = true;
        }
    }

    /// Whether `code` is currently held.
    pub fn is_key_down(&self, code: u32) -> bool {
        self.keys.get(&code).is_some_and(|state| state.held)
    }

    /// Whether `code` was released during the current frame.
    pub fn is_key_released(&self, code: u32) -> bool {
        self.keys.get(&code).is_some_and(|state| state.released)
    }

    /// Frames `code` has been held (0 when not held).
    pub fn key_hold_frames(&self, code: u32) -> u32 {
        match self.keys.get(&code) {
            Some(state) if state.held => state.hold_frames,
            _ => 0,
        }
    }

    /// The key codes that transitioned to down since the last
    /// [`Self::begin_frame`], in arrival order.
    pub fn last_pressed(&self) -> &[u32] {
        &self.last_pressed
    }

    /// Iterate the codes of all currently held keys.
    pub fn held_keys(&self) -> impl Iterator<Item = u32> + '_ {
        self.keys
            .iter()
            .filter(|(_, state)| state.held)
            .map(|(code, _)| *code)
    }

    /// Iterate the codes of all keys released during the current frame.
    pub fn released_keys(&self) -> impl Iterator<Item = u32> + '_ {
        self.keys
            .iter()
            .filter(|(_, state)| state.released)
            .map(|(code, _)| *code)
    }

    // -- text -------------------------------------------------------------

    /// The committed text accumulated since the last [`Self::begin_frame`].
    pub fn text_input(&self) -> &str {
        &self.text_input
    }

    // -- touch ------------------------------------------------------------

    fn apply_touch(&mut self, id: u64, phase: TouchPhase, x: f32, y: f32) {
        match phase {
            TouchPhase::Started => {
                self.touches.insert(
                    id,
                    TouchContact {
                        point: TouchPoint {
                            id,
                            pos: (x, y),
                            phase,
                        },
                        start_pos: (x, y),
                        prev_pos: (x, y),
                    },
                );
            }
            TouchPhase::Moved => {
                let entry = self.touches.entry(id).or_insert(TouchContact {
                    point: TouchPoint {
                        id,
                        pos: (x, y),
                        phase,
                    },
                    start_pos: (x, y),
                    prev_pos: (x, y),
                });
                entry.prev_pos = entry.point.pos;
                entry.point = TouchPoint {
                    id,
                    pos: (x, y),
                    phase,
                };
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                self.touches.remove(&id);
            }
        }
    }

    /// The number of active touch contacts.
    pub fn touch_point_count(&self) -> usize {
        self.touches.len()
    }

    /// The latest point for contact `id`, if it is active.
    pub fn touch_point(&self, id: u64) -> Option<TouchPoint> {
        self.touches.get(&id).map(|contact| contact.point)
    }

    /// The full contact record for `id`, if it is active.
    pub fn touch_contact(&self, id: u64) -> Option<&TouchContact> {
        self.touches.get(&id)
    }

    /// Iterate the active touch contacts.
    pub fn touch_contacts(&self) -> impl Iterator<Item = &TouchContact> + '_ {
        self.touches.values()
    }

    /// The position delta of contact `id` since its previous update, in
    /// physical pixels per update.
    pub fn touch_velocity(&self, id: u64) -> Option<(f32, f32)> {
        self.touches.get(&id).map(|contact| {
            (
                contact.point.pos.0 - contact.prev_pos.0,
                contact.point.pos.1 - contact.prev_pos.1,
            )
        })
    }

    /// The speed of contact `id` since its previous update (the magnitude of
    /// [`Self::touch_velocity`]).
    pub fn touch_speed(&self, id: u64) -> Option<f32> {
        self.touch_velocity(id)
            .map(|(dx, dy)| (dx * dx + dy * dy).sqrt())
    }
}
