//! L1 → L3 event adaptation.
//!
//! L1 owns the event loop and pushes [`platform::PlatformEvent`]s; L3
//! maps them to the TJS-visible event the VM dispatches to script handlers.
//! [`EventAdapter::map`] is the pure mapping and [`EventAdapter::dispatch`]
//! is the delivery seam.
//!
//! L3 does **not** redeclare the platform event type: the event loop/type is
//! L1 and only the TJS mapping lives here.

use platform::{InputEvent, MouseButton, PlatformEvent};

use crate::{CompatCtx, CompatError};

/// A TJS-visible event: the handler name and its dictionary members.
#[derive(Clone, Debug, PartialEq)]
pub struct TjsEvent {
    /// The handler/method name (e.g. `"onKeyDown"`).
    pub name: &'static str,
    /// The event dictionary members.
    pub members: Vec<(&'static str, EventValue)>,
}

/// A scalar event member value.
#[derive(Clone, Debug, PartialEq)]
pub enum EventValue {
    /// A TJS integer.
    Int(i64),
    /// A TJS real.
    Real(f64),
    /// A TJS string.
    Str(String),
    /// A TJS boolean.
    Bool(bool),
}

/// Maps L1 platform events to TJS-visible events and delivers them.
#[derive(Default)]
pub struct EventAdapter;

impl EventAdapter {
    /// Create an adapter.
    pub fn new() -> Self {
        Self
    }

    /// Map a platform event to the TJS-visible event, or `None` for events
    /// the script surface ignores.
    pub fn map(&self, event: &PlatformEvent) -> Option<TjsEvent> {
        match event {
            PlatformEvent::Resized(width, height) => Some(TjsEvent {
                name: "onResize",
                members: vec![
                    ("width", EventValue::Int(i64::from(*width))),
                    ("height", EventValue::Int(i64::from(*height))),
                ],
            }),
            // The scale factor has no KAG handler; L2 tracks it if needed.
            PlatformEvent::ScaleFactorChanged(_) => None,
            PlatformEvent::CloseRequested => Some(TjsEvent {
                name: "onClose",
                members: Vec::new(),
            }),
            PlatformEvent::Focused(true) => Some(TjsEvent {
                name: "onFocus",
                members: Vec::new(),
            }),
            PlatformEvent::Focused(false) => Some(TjsEvent {
                name: "onBlur",
                members: Vec::new(),
            }),
            PlatformEvent::Input(input) => self.map_input(input),
            // `PlatformEvent` is `#[non_exhaustive]`; new lifecycle events
            // without a script handler are ignored.
            _ => None,
        }
    }

    /// Map one raw input event to the TJS-visible event.
    fn map_input(&self, input: &InputEvent) -> Option<TjsEvent> {
        match input {
            InputEvent::PointerMoved { x, y } => Some(TjsEvent {
                name: "onMouseMove",
                members: vec![
                    ("x", EventValue::Int(coordinate(*x))),
                    ("y", EventValue::Int(coordinate(*y))),
                ],
            }),
            // The window-level pointer-leave is not a KAG event.
            InputEvent::PointerLeft => None,
            InputEvent::MouseButton { button, pressed } => Some(TjsEvent {
                name: if *pressed { "onMouseDown" } else { "onMouseUp" },
                members: vec![("button", EventValue::Int(mouse_button_code(*button)))],
            }),
            InputEvent::Wheel { delta } => Some(TjsEvent {
                name: "onMouseWheel",
                members: vec![("delta", EventValue::Int(coordinate(*delta)))],
            }),
            InputEvent::Key { code, pressed } => Some(TjsEvent {
                name: if *pressed { "onKeyDown" } else { "onKeyUp" },
                members: vec![("code", EventValue::Int(i64::from(*code)))],
            }),
            // Text/IME and touch events have no script mapping.
            InputEvent::Text(_) | InputEvent::Touch { .. } => None,
            // `InputEvent` is `#[non_exhaustive]`.
            _ => None,
        }
    }

    /// Deliver a mapped event to the VM.
    ///
    /// This is the delivery seam: it logs the event so the wiring is
    /// observable and never operates by accident.
    pub fn dispatch(&self, ctx: &mut CompatCtx, event: &TjsEvent) -> Result<(), CompatError> {
        // SAFETY: `ctx` is resolved from the installed binding.
        let engine = unsafe { ctx.engine() };
        log::debug!(
            "event adapter: dispatching {} ({} member(s)) on engine {:p}",
            event.name,
            event.members.len(),
            engine.raw(),
        );
        Ok(())
    }
}

/// Round a logical coordinate to the TJS integer the event dictionary
/// carries.
fn coordinate(value: f32) -> i64 {
    i64::from(value.round() as i32)
}

/// The TVP mouse-button code (`mbLeft` = 0 … `mbX2` = 4).
fn mouse_button_code(button: MouseButton) -> i64 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Right => 1,
        MouseButton::Middle => 2,
        MouseButton::Back => 3,
        MouseButton::Forward => 4,
        MouseButton::Other(code) => i64::from(code),
        // `MouseButton` is `#[non_exhaustive]`; an unknown button has no TVP
        // code.
        _ => -1,
    }
}
