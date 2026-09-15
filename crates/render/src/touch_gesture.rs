//! Pure touch-gesture → virtual-mouse classification for the input bridge.
//!
//! KiriKiri games are mouse-and-keyboard driven, so the host turns touch into
//! the same mouse events a desktop mouse produces. This module owns the
//! *decision* half of that bridge: it takes plain [`TouchSample`]s (phase, id,
//! game-space position, injected [`Duration`] timestamp) and emits
//! [`MouseAction`]s. It deliberately knows nothing about Bevy: the system glue
//! in [`crate::input_bridge`] maps Bevy `TouchInput` events into samples and
//! applies the actions to `tvp_input::InputState`.
//!
//! # Gesture rules (the thresholds are [`GestureConfig`]'s defaults)
//!
//! Only the **primary** pointer — the first finger that goes down while the
//! machine is idle — is tracked. Any other pointer id is ignored, so a second
//! finger can never corrupt the first finger's in-flight gesture. An `Ended`
//! whose id is not the primary pointer is a no-op (an "up without a down").
//!
//! * **Tap** (a short press): down, then up before the long-press timeout and
//!   without moving further than the drag slop → a left press + release at the
//!   touch position. This is the common case and drives the games' `onClick`.
//! * **Long press**: held past `long_press` (default **500 ms**) without
//!   exceeding the drag slop → a **right** press + release. KAG's right-click
//!   opens its menu; games also poll `System.getMouseButtonState(1)`. The touch
//!   is consumed: the following up does nothing.
//! * **Drag**: movement beyond `drag_slop` (default **24 game pixels**) from the
//!   down point before the long-press fires → a left press **held**, followed
//!   by mouse moves, released on up. The press is delivered at the *down* point
//!   (as the reference `TVPWindowLayer::onTouchMoved` does), so the layer under
//!   the initial touch owns the drag. It is emitted as
//!   [`MouseAction::PressWithoutClick`]: a drag is not a click, so it must not
//!   fire `onClick` or extend a preceding tap's double-click sequence.
//! * **Cancel**: a cancelled touch (Android delivers this when a gesture is
//!   intercepted, e.g. the Kotlin back gesture) releases an in-flight drag but
//!   never turns into a click.
//!
//! The drag slop is measured in **game (primary-layer) coordinates**, because
//! the machine runs after the window→game transform; `24` game pixels is about
//! 4 mm on a 1280×720 game, close to the reference's `DPI/10` threshold
//! (`reference/cpp/core/environ/cocos2d/MainScene.cpp:494`).
//!
//! # Edge preservation
//!
//! A tap emits a left press and a release in the same call. `tvp-input` is a
//! *sampled* state and [`crate::input_bridge::collect_frame_events`] recovers
//! button edges by diffing consecutive frames, so applying both in one frame
//! would collapse them into "no edge" and lose the click. The glue therefore
//! drains [`Self::update`]'s actions with
//! [`crate::input_bridge::apply_pending_mouse_actions`], which applies at most
//! one button transition per frame and defers the rest; the machine itself
//! stays stateless with respect to the frame rate.
//!
//! # What is tested
//!
//! Every rule above is unit-tested here on the host with synthetic samples and
//! an injected clock: tap, long press, drag (including the press-at-down-point
//! ordering and the non-click press), second-finger isolation, up-without-down,
//! and cancellation. No touchscreen or Android device was available, so none
//! of this has been exercised on real hardware.

use std::time::Duration;

/// The primary pointer's phase, decoupled from Bevy's `TouchPhase`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchPhase {
    /// Finger went down.
    Started,
    /// Finger moved while down.
    Moved,
    /// Finger lifted.
    Ended,
    /// The platform cancelled the gesture.
    Canceled,
}

/// One touch sample in game (primary-layer) coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TouchSample {
    /// Platform touch id. Only the primary id is tracked.
    pub id: u64,
    /// The event phase.
    pub phase: TouchPhase,
    /// Game-space X (primary-layer coordinates).
    pub x: i32,
    /// Game-space Y (primary-layer coordinates).
    pub y: i32,
    /// Injected monotonic timestamp (since the bridge's clock origin).
    pub time: Duration,
}

/// The virtual mouse buttons the classifier can synthesize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GestureButton {
    /// A tap / drag button.
    Left,
    /// A long-press button.
    Right,
}

/// One virtual mouse action for the bridge to apply to `tvp_input::InputState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    /// Move the logical cursor to a game-space point.
    MoveTo { x: i32, y: i32 },
    /// Press or release a virtual mouse button, counting a press as a click
    /// (`onClick` / `Mouse.getClickCount`).
    Button { button: GestureButton, down: bool },
    /// Press a button that must **not** be treated as a click: the touch
    /// drag's left button. The bridge applies it with
    /// `InputState::set_mouse_button_raw` + `clear_click_sequence`, so a drag
    /// does not also fire `onClick` or join a tap's double-click sequence.
    PressWithoutClick { button: GestureButton },
}

/// Tunable gesture thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GestureConfig {
    /// How long a still press is held before it becomes a right click.
    pub long_press: Duration,
    /// Movement (game pixels) from the down point that starts a drag.
    pub drag_slop: i32,
}

impl Default for GestureConfig {
    fn default() -> Self {
        GestureConfig {
            long_press: Duration::from_millis(500),
            drag_slop: 24,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    /// Primary finger down, not yet a drag or a long press.
    Pressed,
    /// Primary finger down and dragging with the left button held.
    Dragging,
    /// The gesture was resolved (long press) and subsequent samples are ignored.
    Consumed,
}

/// The pure gesture state machine. Feed it [`Self::update`] once per frame
/// with the frame's samples (possibly empty) and the current timestamp; it
/// returns the virtual mouse actions for that frame.
#[derive(Debug, Clone)]
pub struct TouchGesture {
    config: GestureConfig,
    primary: Option<u64>,
    start: (i32, i32),
    current: (i32, i32),
    start_time: Duration,
    phase: Phase,
}

impl Default for TouchGesture {
    fn default() -> Self {
        TouchGesture {
            config: GestureConfig::default(),
            primary: None,
            start: (0, 0),
            current: (0, 0),
            start_time: Duration::ZERO,
            phase: Phase::Idle,
        }
    }
}

impl TouchGesture {
    /// Whether a primary pointer is currently being tracked. The glue uses
    /// this to gate the desktop mouse path while a touch is in progress; a
    /// consumed long press is still "in progress" until the finger lifts, so
    /// the gate stays closed.
    pub fn is_active(&self) -> bool {
        matches!(
            self.phase,
            Phase::Pressed | Phase::Dragging | Phase::Consumed
        )
    }

    /// Advance the clock and feed this frame's samples, returning the virtual
    /// mouse actions they produced.
    ///
    /// The long-press check runs *before* the samples so a release that lands
    /// in the same frame the timeout expires is classified as a long press,
    /// not as a tap.
    pub fn update(&mut self, now: Duration, samples: &[TouchSample]) -> Vec<MouseAction> {
        let mut actions = Vec::new();
        self.tick(now, &mut actions);
        for sample in samples {
            self.handle(sample, &mut actions);
        }
        actions
    }

    /// Fire the long press if a still, tracked press has outlived the timeout.
    fn tick(&mut self, now: Duration, actions: &mut Vec<MouseAction>) {
        if self.phase != Phase::Pressed {
            return;
        }
        if now.saturating_sub(self.start_time) < self.config.long_press {
            return;
        }
        self.phase = Phase::Consumed;
        self.current = self.start;
        actions.push(MouseAction::MoveTo {
            x: self.start.0,
            y: self.start.1,
        });
        actions.push(MouseAction::Button {
            button: GestureButton::Right,
            down: true,
        });
        actions.push(MouseAction::Button {
            button: GestureButton::Right,
            down: false,
        });
    }

    /// Classify one touch sample.
    fn handle(&mut self, sample: &TouchSample, actions: &mut Vec<MouseAction>) {
        match sample.phase {
            TouchPhase::Started => {
                // Only the first finger of an idle machine becomes primary;
                // a second finger (or one during a consumed gesture) is
                // ignored outright.
                if self.phase != Phase::Idle {
                    return;
                }
                self.primary = Some(sample.id);
                self.start = (sample.x, sample.y);
                self.current = (sample.x, sample.y);
                self.start_time = sample.time;
                self.phase = Phase::Pressed;
                actions.push(MouseAction::MoveTo {
                    x: sample.x,
                    y: sample.y,
                });
            }
            TouchPhase::Moved => {
                if self.primary != Some(sample.id) {
                    return;
                }
                self.current = (sample.x, sample.y);
                match self.phase {
                    Phase::Pressed => {
                        let dx = i64::from(sample.x) - i64::from(self.start.0);
                        let dy = i64::from(sample.y) - i64::from(self.start.1);
                        let slop = i64::from(self.config.drag_slop);
                        if dx * dx + dy * dy > slop * slop {
                            // Drag starts: press at the *down* point so the
                            // initial layer owns the gesture, then move.
                            self.phase = Phase::Dragging;
                            actions.push(MouseAction::MoveTo {
                                x: self.start.0,
                                y: self.start.1,
                            });
                            actions.push(MouseAction::PressWithoutClick {
                                button: GestureButton::Left,
                            });
                            actions.push(MouseAction::MoveTo {
                                x: sample.x,
                                y: sample.y,
                            });
                        }
                    }
                    Phase::Dragging => actions.push(MouseAction::MoveTo {
                        x: sample.x,
                        y: sample.y,
                    }),
                    Phase::Idle | Phase::Consumed => {}
                }
            }
            TouchPhase::Ended | TouchPhase::Canceled => {
                if self.primary != Some(sample.id) {
                    return;
                }
                let canceled = sample.phase == TouchPhase::Canceled;
                match self.phase {
                    Phase::Dragging => {
                        // Release an in-flight drag even on cancel, so the
                        // left button never sticks.
                        self.current = (sample.x, sample.y);
                        actions.push(MouseAction::MoveTo {
                            x: sample.x,
                            y: sample.y,
                        });
                        actions.push(MouseAction::Button {
                            button: GestureButton::Left,
                            down: false,
                        });
                    }
                    Phase::Pressed if !canceled => {
                        // Tap: press at the down point, release at the lift.
                        self.current = (sample.x, sample.y);
                        actions.push(MouseAction::MoveTo {
                            x: self.start.0,
                            y: self.start.1,
                        });
                        actions.push(MouseAction::Button {
                            button: GestureButton::Left,
                            down: true,
                        });
                        actions.push(MouseAction::MoveTo {
                            x: sample.x,
                            y: sample.y,
                        });
                        actions.push(MouseAction::Button {
                            button: GestureButton::Left,
                            down: false,
                        });
                    }
                    // Consumed (long press), a canceled tap, or an
                    // inconsistent idle state: nothing to do.
                    Phase::Consumed | Phase::Pressed | Phase::Idle => {}
                }
                self.reset();
            }
        }
    }

    fn reset(&mut self) {
        self.primary = None;
        self.phase = Phase::Idle;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(id: u64, phase: TouchPhase, x: i32, y: i32, ms: u64) -> TouchSample {
        TouchSample {
            id,
            phase,
            x,
            y,
            time: Duration::from_millis(ms),
        }
    }

    fn buttons(actions: &[MouseAction]) -> Vec<(GestureButton, bool)> {
        actions
            .iter()
            .filter_map(|a| match a {
                MouseAction::Button { button, down } => Some((*button, *down)),
                // A drag press is still a held button, just not a click.
                MouseAction::PressWithoutClick { button } => Some((*button, true)),
                MouseAction::MoveTo { .. } => None,
            })
            .collect()
    }

    fn moves(actions: &[MouseAction]) -> Vec<(i32, i32)> {
        actions
            .iter()
            .filter_map(|a| match a {
                MouseAction::MoveTo { x, y } => Some((*x, *y)),
                MouseAction::Button { .. } | MouseAction::PressWithoutClick { .. } => None,
            })
            .collect()
    }

    /// A short press with no movement is a left click.
    #[test]
    fn short_press_is_tap() {
        let mut g = TouchGesture::default();
        let down = [sample(1, TouchPhase::Started, 100, 80, 0)];
        let up = [sample(1, TouchPhase::Ended, 101, 81, 120)];
        // The down only moves the cursor; the click is decided on lift.
        let down_actions = g.update(Duration::from_millis(0), &down);
        assert_eq!(moves(&down_actions), vec![(100, 80)]);
        assert!(buttons(&down_actions).is_empty());
        assert!(g.is_active());
        let actions = g.update(Duration::from_millis(120), &up);
        assert_eq!(
            buttons(&actions),
            vec![(GestureButton::Left, true), (GestureButton::Left, false)]
        );
        assert!(
            !buttons(&actions)
                .iter()
                .any(|(b, _)| *b == GestureButton::Right)
        );
        assert!(!g.is_active());
    }

    /// Holding still past the timeout is a right click, exactly once, and the
    /// finger's eventual lift adds nothing.
    #[test]
    fn long_press_is_right_click() {
        let mut g = TouchGesture::default();
        g.update(Duration::ZERO, &[sample(1, TouchPhase::Started, 50, 50, 0)]);
        // No samples: the idle frame still fires the long press at the deadline.
        let actions = g.update(Duration::from_millis(500), &[]);
        assert_eq!(
            buttons(&actions),
            vec![(GestureButton::Right, true), (GestureButton::Right, false)]
        );
        // A second idle frame must not fire again, and the machine still
        // tracks the finger (the host gate must stay closed).
        assert!(g.update(Duration::from_millis(550), &[]).is_empty());
        assert!(g.is_active());
        // Lifting after the long press does nothing and releases tracking.
        let up = [sample(1, TouchPhase::Ended, 50, 50, 560)];
        assert!(g.update(Duration::from_millis(560), &up).is_empty());
        assert!(!g.is_active());
    }

    /// A press that is still down just under the timeout is not a long press.
    #[test]
    fn long_press_fires_at_the_deadline_but_not_before() {
        let mut g = TouchGesture::default();
        g.update(Duration::ZERO, &[sample(1, TouchPhase::Started, 0, 0, 0)]);
        assert!(g.update(Duration::from_millis(499), &[]).is_empty());
        assert_eq!(
            buttons(&g.update(Duration::from_millis(500), &[])),
            vec![(GestureButton::Right, true), (GestureButton::Right, false)]
        );
    }

    /// Movement past the slop becomes a drag with the left button held: the
    /// press is delivered at the down point, then moves follow, then the up
    /// releases.
    #[test]
    fn movement_beyond_slop_is_drag_with_button_held() {
        let mut g = TouchGesture::default();
        g.update(Duration::ZERO, &[sample(1, TouchPhase::Started, 10, 20, 0)]);
        let crossed = [sample(1, TouchPhase::Moved, 60, 20, 40)]; // 50px > 24
        let actions = g.update(Duration::from_millis(40), &crossed);
        assert_eq!(
            actions,
            vec![
                MouseAction::MoveTo { x: 10, y: 20 },
                MouseAction::PressWithoutClick {
                    button: GestureButton::Left,
                },
                MouseAction::MoveTo { x: 60, y: 20 },
            ]
        );
        assert!(g.is_active());
        // Continued movement stays a drag.
        let more = [sample(1, TouchPhase::Moved, 70, 30, 60)];
        assert_eq!(
            g.update(Duration::from_millis(60), &more),
            vec![MouseAction::MoveTo { x: 70, y: 30 }]
        );
        // Release.
        let up = [sample(1, TouchPhase::Ended, 70, 30, 80)];
        assert_eq!(
            g.update(Duration::from_millis(80), &up),
            vec![
                MouseAction::MoveTo { x: 70, y: 30 },
                MouseAction::Button {
                    button: GestureButton::Left,
                    down: false
                },
            ]
        );
        assert!(!g.is_active());
    }

    /// Movement within the slop is still a tap.
    #[test]
    fn small_movement_is_tap() {
        let mut g = TouchGesture::default();
        g.update(Duration::ZERO, &[sample(1, TouchPhase::Started, 10, 10, 0)]);
        let within = [sample(1, TouchPhase::Moved, 20, 10, 50)]; // 10px <= 24
        assert!(g.update(Duration::from_millis(50), &within).is_empty());
        let up = [sample(1, TouchPhase::Ended, 20, 10, 90)];
        assert_eq!(
            buttons(&g.update(Duration::from_millis(90), &up)),
            vec![(GestureButton::Left, true), (GestureButton::Left, false)]
        );
    }

    /// Slop is Euclidean: a 20/20 diagonal (≈28.3px) exceeds a 24px slop.
    #[test]
    fn diagonal_movement_past_slop_starts_drag() {
        let mut g = TouchGesture::default();
        g.update(Duration::ZERO, &[sample(1, TouchPhase::Started, 0, 0, 0)]);
        let actions = g.update(
            Duration::from_millis(20),
            &[sample(1, TouchPhase::Moved, 20, 20, 20)],
        );
        assert!(buttons(&actions).contains(&(GestureButton::Left, true)));
    }

    /// A second pointer never becomes primary and cannot disturb the first
    /// finger's gesture.
    #[test]
    fn second_finger_is_ignored() {
        let mut g = TouchGesture::default();
        g.update(Duration::ZERO, &[sample(1, TouchPhase::Started, 10, 10, 0)]);
        // Second finger down + move + lift: all ignored.
        assert!(
            g.update(
                Duration::from_millis(10),
                &[sample(2, TouchPhase::Started, 100, 100, 10)]
            )
            .is_empty()
        );
        assert!(
            g.update(
                Duration::from_millis(20),
                &[sample(2, TouchPhase::Moved, 200, 200, 20)]
            )
            .is_empty()
        );
        assert!(
            g.update(
                Duration::from_millis(30),
                &[sample(2, TouchPhase::Ended, 200, 200, 30)]
            )
            .is_empty()
        );
        assert!(g.is_active(), "second finger must not end the first");
        // The primary finger still resolves to a tap.
        let up = [sample(1, TouchPhase::Ended, 10, 10, 60)];
        assert_eq!(
            buttons(&g.update(Duration::from_millis(60), &up)),
            vec![(GestureButton::Left, true), (GestureButton::Left, false)]
        );
    }

    /// An up with no matching down is a no-op.
    #[test]
    fn up_without_down_does_nothing() {
        let mut g = TouchGesture::default();
        let up = [sample(9, TouchPhase::Ended, 5, 5, 0)];
        assert!(g.update(Duration::ZERO, &up).is_empty());
        assert!(!g.is_active());
    }

    /// A cancelled tap must not synthesize a click.
    #[test]
    fn cancelled_tap_does_not_click() {
        let mut g = TouchGesture::default();
        g.update(Duration::ZERO, &[sample(1, TouchPhase::Started, 5, 5, 0)]);
        let cancel = [sample(1, TouchPhase::Canceled, 5, 5, 30)];
        assert!(g.update(Duration::from_millis(30), &cancel).is_empty());
        assert!(!g.is_active());
    }

    /// A cancelled drag releases the left button.
    #[test]
    fn cancelled_drag_releases_button() {
        let mut g = TouchGesture::default();
        g.update(Duration::ZERO, &[sample(1, TouchPhase::Started, 0, 0, 0)]);
        g.update(
            Duration::from_millis(20),
            &[sample(1, TouchPhase::Moved, 100, 0, 20)],
        );
        let cancel = [sample(1, TouchPhase::Canceled, 120, 0, 30)];
        assert_eq!(
            buttons(&g.update(Duration::from_millis(30), &cancel)),
            vec![(GestureButton::Left, false)]
        );
    }

    /// A new press after a completed gesture starts a fresh gesture.
    #[test]
    fn gestures_do_not_leak_between_touches() {
        let mut g = TouchGesture::default();
        g.update(Duration::ZERO, &[sample(1, TouchPhase::Started, 0, 0, 0)]);
        g.update(
            Duration::from_millis(10),
            &[sample(1, TouchPhase::Ended, 0, 0, 10)],
        );
        // The next press uses its own start point.
        g.update(
            Duration::from_millis(20),
            &[sample(2, TouchPhase::Started, 300, 300, 20)],
        );
        let crossed = [sample(2, TouchPhase::Moved, 400, 300, 30)];
        assert_eq!(
            moves(&g.update(Duration::from_millis(30), &crossed)),
            vec![(300, 300), (400, 300)]
        );
    }

    /// The machine only fires the long press once even if `update` is called
    /// many times after the deadline while the finger stays down.
    #[test]
    fn long_press_fires_once() {
        let mut g = TouchGesture::default();
        g.update(Duration::ZERO, &[sample(1, TouchPhase::Started, 0, 0, 0)]);
        let mut right_clicks = 0;
        for ms in 500..700 {
            for (button, down) in buttons(&g.update(Duration::from_millis(ms), &[])) {
                if button == GestureButton::Right && down {
                    right_clicks += 1;
                }
            }
        }
        assert_eq!(right_clicks, 1);
    }
}
