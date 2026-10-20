//! Touch gestures -> mouse input for the Android composition root.
//!
//! KiriKiri games are mouse-and-keyboard driven: a KAG menu resolves a click
//! through `onMouseDown`/`onMouseUp` and hit-tests the layer under the pointer,
//! while a raw touch stream reaches the compatibility layer as the separate
//! `onTouchDown`/`onTouchMove`/`onTouchUp` family, which such a menu never
//! sees. The reference Android environment therefore turns a single finger into
//! a virtual mouse — a tap clicks, a held finger presses, a drag moves the
//! cursor with the button held — and uses two fingers for the secondary
//! button (`reference/cpp/core/environ/cocos2d/MainScene.cpp:1017-1146`).
//!
//! This module is that translation as a pure state machine over milliseconds.
//! The Android wiring (`crate::input`) supplies the clock and forwards the
//! produced [`InputEvent`]s through the same host path as a real mouse; it
//! consumes the raw touch instead of forwarding it, so no event fires twice
//! (`docs.old/android.md` §5).
//!
//! ## The mapping
//!
//! * a single finger that does not travel beyond [`DRAG_SLOP_PX`] and is
//!   released before [`LONG_PRESS_MS`] is a **tap** → left button down, then
//!   up, at the touch position (the L2 input state turns that pair into
//!   `onClick`/`onDoubleClick`);
//! * a finger that travels past the slop is a **drag** → left button down,
//!   followed by pointer moves, then left button up;
//! * a finger held still past [`LONG_PRESS_MS`] is a **long press** → right
//!   button down (KAG opens its menu on the right button;
//!   `System.getMouseButtonState(1)`), then right button up on release;
//! * two fingers that both lift within [`MULTI_TAP_MS`] are a **two-finger
//!   tap**, and two fingers dragged down past [`SWIPE_DOWN_PX`] are the
//!   **back gesture**; both emit `Escape`, the key Android's back button
//!   already maps to (`crate::input`).
//!
//! Only the coordinates and the frame the events arrive in are observable, so
//! the state machine keeps the contact's start position and the position of
//! the last move and nothing else.
//!
//! ## What needs a device
//!
//! The state machine is host-tested against injected timestamps. What a device
//! must still show is that the Android motion actions and timestamps actually
//! reach it in this order, and that the synthesised events hit the KAG menu.

use api::host::TouchPhase;
use api::input::{InputEvent, MouseButton, PhysicalKey};

/// A contact becomes a drag once it has travelled this far from where it went
/// down.
///
/// The reference scales its threshold by the display DPI
/// (`Device::getDPI() / 10`, so 32 px at 320 dpi;
/// `reference/cpp/core/environ/cocos2d/MainScene.cpp:494-496`); a fixed
/// physical-pixel slop keeps this module free of a display service and is
/// small enough that a deliberate drag passes it within a frame.
pub(crate) const DRAG_SLOP_PX: f32 = 16.0;

/// A stationary finger held this long is a long press.
///
/// The reference's virtual-mouse long press fires on the same 1000 ms
/// (`reference/cpp/core/environ/cocos2d/MainScene.cpp:1097-1101`).
pub(crate) const LONG_PRESS_MS: u64 = 1000;

/// A two-finger gesture that ends within this is a tap, not a hold.
pub(crate) const MULTI_TAP_MS: u64 = 300;

/// A two-finger drag down this far is the back gesture.
pub(crate) const SWIPE_DOWN_PX: f32 = 64.0;

/// The state of the first (primary) contact, if one is down.
#[derive(Clone, Copy, Debug)]
struct Contact {
    /// Stable Android pointer id.
    id: u64,
    /// Where the contact went down, for the slop and the long-press stillness.
    start_x: f32,
    start_y: f32,
    /// The most recent position.
    x: f32,
    y: f32,
    /// When the contact went down, for the long-press timeout.
    down_ms: u64,
    /// The button this contact is holding, once a drag or long press resolved.
    button: Option<MouseButton>,
    /// Whether the contact was resolved to a long press.
    long_press: bool,
}

/// Turns a stream of touch phases into mouse and Escape input.
///
/// `now_ms` is a monotonic millisecond clock; only differences are compared,
/// so the Android wiring can use any monotonic source.
#[derive(Debug, Default)]
pub(crate) struct GestureSynthesizer {
    /// The first contact down, if any.
    primary: Option<Contact>,
    /// How many further contacts are down while the primary is.
    others: usize,
    /// Whether the current gesture became multi-touch (a second finger arrived
    /// before the primary resolved to a drag or a long press).
    multi: bool,
    /// When the second contact arrived, for the two-finger tap timeout.
    multi_start_ms: u64,
    /// Whether the back gesture already fired, so it fires once per gesture.
    multi_fired: bool,
}

impl GestureSynthesizer {
    /// Feed one touch phase and return the input events it produces.
    pub(crate) fn phase(
        &mut self,
        id: u64,
        phase: TouchPhase,
        x: f32,
        y: f32,
        now_ms: u64,
    ) -> Vec<InputEvent> {
        match phase {
            TouchPhase::Started => self.started(id, x, y, now_ms),
            TouchPhase::Moved => self.moved(id, x, y),
            TouchPhase::Ended => self.ended(id, x, y, now_ms),
            TouchPhase::Cancelled => self.cancelled(id),
        }
    }

    /// Advance the clock so a held, stationary contact becomes a long press.
    ///
    /// The Android wiring calls this once per event-loop iteration; a contact
    /// that has not moved past [`DRAG_SLOP_PX`] and has been down for
    /// [`LONG_PRESS_MS`] turns into a right-button press.
    pub(crate) fn tick(&mut self, now_ms: u64) -> Vec<InputEvent> {
        let Some(contact) = self.primary.as_mut() else {
            return Vec::new();
        };
        if self.multi
            || contact.button.is_some()
            || contact.long_press
            || now_ms.saturating_sub(contact.down_ms) < LONG_PRESS_MS
        {
            return Vec::new();
        }
        let dx = contact.x - contact.start_x;
        let dy = contact.y - contact.start_y;
        if dx * dx + dy * dy >= DRAG_SLOP_PX * DRAG_SLOP_PX {
            return Vec::new();
        }
        contact.long_press = true;
        contact.button = Some(MouseButton::Right);
        vec![
            InputEvent::PointerMoved {
                x: contact.x,
                y: contact.y,
            },
            InputEvent::MouseButton {
                button: MouseButton::Right,
                pressed: true,
            },
        ]
    }

    fn started(&mut self, id: u64, x: f32, y: f32, now_ms: u64) -> Vec<InputEvent> {
        if self.primary.is_none() {
            self.primary = Some(Contact {
                id,
                start_x: x,
                start_y: y,
                x,
                y,
                down_ms: now_ms,
                button: None,
                long_press: false,
            });
            return Vec::new();
        }
        self.others = self.others.saturating_add(1);
        // A second finger only starts a two-finger gesture while the primary
        // has not already become a drag or a long press; otherwise the drag
        // simply keeps its capture.
        let unresolved = self
            .primary
            .as_ref()
            .is_some_and(|contact| contact.button.is_none() && !contact.long_press);
        if !self.multi && unresolved {
            self.multi = true;
            self.multi_start_ms = now_ms;
            self.multi_fired = false;
        }
        Vec::new()
    }

    fn moved(&mut self, id: u64, x: f32, y: f32) -> Vec<InputEvent> {
        let Some(contact) = self.primary.as_mut() else {
            return Vec::new();
        };
        if contact.id != id {
            return Vec::new();
        }
        contact.x = x;
        contact.y = y;
        if self.multi {
            if !self.multi_fired && y - contact.start_y >= SWIPE_DOWN_PX {
                self.multi_fired = true;
                return escape();
            }
            return Vec::new();
        }
        if contact.button.is_none() && !contact.long_press {
            let dx = x - contact.start_x;
            let dy = y - contact.start_y;
            if dx * dx + dy * dy < DRAG_SLOP_PX * DRAG_SLOP_PX {
                // Still a candidate tap; report nothing until it resolves.
                return Vec::new();
            }
            contact.button = Some(MouseButton::Left);
            return vec![
                InputEvent::PointerMoved { x, y },
                InputEvent::MouseButton {
                    button: MouseButton::Left,
                    pressed: true,
                },
            ];
        }
        vec![InputEvent::PointerMoved { x, y }]
    }

    fn ended(&mut self, id: u64, x: f32, y: f32, now_ms: u64) -> Vec<InputEvent> {
        if self
            .primary
            .as_ref()
            .is_some_and(|contact| contact.id == id)
        {
            let contact = self.primary.take().expect("checked just above");
            let mut events = Vec::new();
            if self.multi {
                // A multi-touch gesture never holds a mouse button.
            } else if let Some(button) = contact.button {
                events.push(InputEvent::MouseButton {
                    button,
                    pressed: false,
                });
            } else {
                // A tap: move to the contact, press and release, which the
                // engine's input state turns into `onMouseDown`/`onMouseUp`
                // and `onClick`.
                events.push(InputEvent::PointerMoved { x, y });
                events.push(InputEvent::MouseButton {
                    button: MouseButton::Left,
                    pressed: true,
                });
                events.push(InputEvent::MouseButton {
                    button: MouseButton::Left,
                    pressed: false,
                });
            }
            if self.active() == 0 {
                self.finish_multi(now_ms, &mut events);
            }
            return events;
        }
        if self.others > 0 {
            self.others -= 1;
            let mut events = Vec::new();
            if self.active() == 0 {
                self.finish_multi(now_ms, &mut events);
            }
            return events;
        }
        Vec::new()
    }

    fn cancelled(&mut self, id: u64) -> Vec<InputEvent> {
        let mut events = Vec::new();
        if self
            .primary
            .as_ref()
            .is_some_and(|contact| contact.id == id)
        {
            if let Some(contact) = self.primary.take()
                && let Some(button) = contact.button
            {
                events.push(InputEvent::MouseButton {
                    button,
                    pressed: false,
                });
            }
        } else if self.others > 0 {
            self.others -= 1;
        }
        if self.active() == 0 {
            // A cancelled gesture is not a tap; drop the multi-touch state
            // without firing the back gesture.
            self.multi = false;
            self.multi_fired = false;
        }
        events
    }

    /// The contacts currently down.
    fn active(&self) -> usize {
        usize::from(self.primary.is_some()) + self.others
    }

    /// End a gesture once every finger is up: a quick two-finger gesture is a
    /// tap and emits `Escape`, matching the Android back button.
    fn finish_multi(&mut self, now_ms: u64, events: &mut Vec<InputEvent>) {
        if self.multi
            && !self.multi_fired
            && now_ms.saturating_sub(self.multi_start_ms) <= MULTI_TAP_MS
        {
            events.extend(escape());
        }
        self.multi = false;
        self.multi_fired = false;
    }
}

/// An Escape press/release pair, the Android back equivalent.
fn escape() -> Vec<InputEvent> {
    vec![
        InputEvent::Key {
            code: PhysicalKey::Escape,
            pressed: true,
        },
        InputEvent::Key {
            code: PhysicalKey::Escape,
            pressed: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started(id: u64, x: f32, y: f32, at: u64) -> Vec<InputEvent> {
        GestureSynthesizer::default().phase(id, TouchPhase::Started, x, y, at)
    }

    /// A tap presses and releases the left button at the touch position; the
    /// pair is what the engine turns into a click.
    #[test]
    fn tap_emits_a_left_click_at_the_touch_position() {
        let mut gestures = GestureSynthesizer::default();
        assert!(
            gestures
                .phase(1, TouchPhase::Started, 10.0, 20.0, 0)
                .is_empty()
        );
        let events = gestures.phase(1, TouchPhase::Ended, 10.0, 20.0, 50);
        assert_eq!(
            events,
            vec![
                InputEvent::PointerMoved { x: 10.0, y: 20.0 },
                InputEvent::MouseButton {
                    button: MouseButton::Left,
                    pressed: true,
                },
                InputEvent::MouseButton {
                    button: MouseButton::Left,
                    pressed: false,
                },
            ]
        );
    }

    /// A small wobble under the slop is still a tap, at the release position.
    #[test]
    fn a_sub_slop_move_stays_a_tap() {
        let mut gestures = GestureSynthesizer::default();
        gestures.phase(1, TouchPhase::Started, 10.0, 20.0, 0);
        assert!(
            gestures
                .phase(1, TouchPhase::Moved, 12.0, 21.0, 10)
                .is_empty()
        );
        let events = gestures.phase(1, TouchPhase::Ended, 12.0, 21.0, 20);
        assert_eq!(
            events,
            vec![
                InputEvent::PointerMoved { x: 12.0, y: 21.0 },
                InputEvent::MouseButton {
                    button: MouseButton::Left,
                    pressed: true,
                },
                InputEvent::MouseButton {
                    button: MouseButton::Left,
                    pressed: false,
                },
            ]
        );
    }

    /// A drag presses the left button once it passes the slop, moves while it
    /// is held and releases on lift.
    #[test]
    fn drag_holds_the_left_button_across_moves() {
        let mut gestures = GestureSynthesizer::default();
        gestures.phase(1, TouchPhase::Started, 0.0, 0.0, 0);
        let first = gestures.phase(1, TouchPhase::Moved, 40.0, 0.0, 10);
        assert_eq!(
            first,
            vec![
                InputEvent::PointerMoved { x: 40.0, y: 0.0 },
                InputEvent::MouseButton {
                    button: MouseButton::Left,
                    pressed: true,
                },
            ]
        );
        let next = gestures.phase(1, TouchPhase::Moved, 60.0, 5.0, 20);
        assert_eq!(next, vec![InputEvent::PointerMoved { x: 60.0, y: 5.0 }]);
        let up = gestures.phase(1, TouchPhase::Ended, 60.0, 5.0, 30);
        assert_eq!(
            up,
            vec![InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: false,
            }]
        );
    }

    /// A stationary hold past the threshold is a right click, held until the
    /// finger lifts.
    #[test]
    fn long_press_emits_a_right_button_click() {
        let mut gestures = GestureSynthesizer::default();
        gestures.phase(1, TouchPhase::Started, 10.0, 10.0, 0);
        assert!(gestures.tick(LONG_PRESS_MS - 1).is_empty());
        assert_eq!(
            gestures.tick(LONG_PRESS_MS),
            vec![
                InputEvent::PointerMoved { x: 10.0, y: 10.0 },
                InputEvent::MouseButton {
                    button: MouseButton::Right,
                    pressed: true,
                },
            ]
        );
        assert_eq!(
            gestures.phase(1, TouchPhase::Ended, 10.0, 10.0, LONG_PRESS_MS + 5),
            vec![InputEvent::MouseButton {
                button: MouseButton::Right,
                pressed: false,
            }]
        );
    }

    /// A finger that has already become a drag never long-presses.
    #[test]
    fn a_drag_does_not_long_press() {
        let mut gestures = GestureSynthesizer::default();
        gestures.phase(1, TouchPhase::Started, 0.0, 0.0, 0);
        gestures.phase(1, TouchPhase::Moved, 40.0, 0.0, 10);
        assert!(gestures.tick(LONG_PRESS_MS).is_empty());
        assert_eq!(
            gestures.phase(1, TouchPhase::Ended, 40.0, 0.0, LONG_PRESS_MS + 5),
            vec![InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: false,
            }]
        );
    }

    /// Two fingers that lift quickly emit Escape once, when the last one
    /// lifts.
    #[test]
    fn two_finger_tap_emits_escape() {
        let mut gestures = GestureSynthesizer::default();
        gestures.phase(1, TouchPhase::Started, 0.0, 0.0, 0);
        gestures.phase(2, TouchPhase::Started, 10.0, 10.0, 10);
        assert!(
            gestures
                .phase(1, TouchPhase::Ended, 0.0, 0.0, 40)
                .is_empty()
        );
        assert_eq!(
            gestures.phase(2, TouchPhase::Ended, 10.0, 10.0, 50),
            escape()
        );
    }

    /// A two-finger hold longer than the tap window emits nothing (it is not
    /// a tap).
    #[test]
    fn a_slow_two_finger_lift_is_not_a_tap() {
        let mut gestures = GestureSynthesizer::default();
        gestures.phase(1, TouchPhase::Started, 0.0, 0.0, 0);
        gestures.phase(2, TouchPhase::Started, 10.0, 10.0, 10);
        gestures.phase(1, TouchPhase::Ended, 0.0, 0.0, MULTI_TAP_MS + 20);
        assert!(
            gestures
                .phase(2, TouchPhase::Ended, 10.0, 10.0, MULTI_TAP_MS + 30)
                .is_empty()
        );
    }

    /// A downward two-finger drag emits Escape once and nothing more.
    #[test]
    fn two_finger_swipe_down_emits_escape_once() {
        let mut gestures = GestureSynthesizer::default();
        gestures.phase(1, TouchPhase::Started, 0.0, 0.0, 0);
        gestures.phase(2, TouchPhase::Started, 10.0, 0.0, 10);
        assert_eq!(
            gestures.phase(1, TouchPhase::Moved, 0.0, SWIPE_DOWN_PX, 20),
            escape()
        );
        assert!(
            gestures
                .phase(1, TouchPhase::Moved, 0.0, SWIPE_DOWN_PX * 2.0, 30)
                .is_empty()
        );
        // The gesture is already consumed, so lifting does not fire again.
        assert!(
            gestures
                .phase(1, TouchPhase::Ended, 0.0, SWIPE_DOWN_PX * 2.0, 40)
                .is_empty()
        );
        assert!(
            gestures
                .phase(2, TouchPhase::Ended, 10.0, 0.0, 50)
                .is_empty()
        );
    }

    /// A cancelled (system-interrupted) drag releases the button it held.
    #[test]
    fn cancel_releases_a_held_button() {
        let mut gestures = GestureSynthesizer::default();
        gestures.phase(1, TouchPhase::Started, 0.0, 0.0, 0);
        gestures.phase(1, TouchPhase::Moved, 40.0, 0.0, 10);
        assert_eq!(
            gestures.phase(1, TouchPhase::Cancelled, 40.0, 0.0, 20),
            vec![InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: false,
            }]
        );
    }

    /// A cancelled two-finger gesture is not a back gesture.
    #[test]
    fn cancel_does_not_emit_a_back_gesture() {
        let mut gestures = GestureSynthesizer::default();
        gestures.phase(1, TouchPhase::Started, 0.0, 0.0, 0);
        gestures.phase(2, TouchPhase::Started, 10.0, 10.0, 10);
        assert!(
            gestures
                .phase(1, TouchPhase::Cancelled, 0.0, 0.0, 20)
                .is_empty()
        );
        assert!(
            gestures
                .phase(2, TouchPhase::Cancelled, 10.0, 10.0, 30)
                .is_empty()
        );
    }

    /// A second finger arriving after a drag started keeps the drag's capture
    /// instead of turning the gesture into a back gesture.
    #[test]
    fn a_second_finger_does_not_cancel_an_active_drag() {
        let mut gestures = GestureSynthesizer::default();
        gestures.phase(1, TouchPhase::Started, 0.0, 0.0, 0);
        gestures.phase(1, TouchPhase::Moved, 40.0, 0.0, 10);
        assert!(
            gestures
                .phase(2, TouchPhase::Started, 10.0, 10.0, 20)
                .is_empty()
        );
        // The drag keeps the cursor with the button held; no back gesture.
        assert_eq!(
            gestures.phase(1, TouchPhase::Moved, 0.0, 200.0, 30),
            vec![InputEvent::PointerMoved { x: 0.0, y: 200.0 }]
        );
        assert_eq!(
            gestures.phase(2, TouchPhase::Ended, 10.0, 10.0, 40),
            Vec::<InputEvent>::new()
        );
        assert_eq!(
            gestures.phase(1, TouchPhase::Ended, 0.0, 200.0, 50),
            vec![InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: false,
            }]
        );
    }

    /// A sub-slop contact carries no state into the next gesture.
    #[test]
    fn a_fresh_synthesizer_starts_idle() {
        assert!(started(1, 0.0, 0.0, 0).is_empty());
    }
}
