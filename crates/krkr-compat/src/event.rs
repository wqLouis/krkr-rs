//! L1 → L3 event adaptation (doc 08 §5).
//!
//! L1 owns the event loop and will push a platform event; L3 maps it to the
//! TJS-visible event the VM dispatches to script handlers. This module is the
//! adapter **skeleton**: [`PlatformEvent`] is the abstract input the host
//! feeds in (it will become `krkr_platform`'s event type), [`EventAdapter::map`]
//! is the pure mapping and [`EventAdapter::dispatch`] is the delivery seam
//! that the focus/KAG machinery fills in.

use crate::{CompatCtx, CompatError};

/// An abstract platform event pushed by L1.
///
/// This is a placeholder for `krkr_platform`'s event type: the adapter is
/// written against this abstract surface so L3 stays platform-free.
#[derive(Clone, Debug, PartialEq)]
pub enum PlatformEvent {
    /// A key was pressed. `code` is the platform key code, `key` its name.
    KeyDown {
        /// Platform key code.
        code: i64,
        /// Key name (e.g. `"Enter"`).
        key: String,
    },
    /// A key was released.
    KeyUp {
        /// Platform key code.
        code: i64,
        /// Key name.
        key: String,
    },
    /// The pointer moved.
    PointerMove {
        /// X in logical pixels.
        x: i32,
        /// Y in logical pixels.
        y: i32,
    },
    /// A pointer button was pressed.
    PointerDown {
        /// X in logical pixels.
        x: i32,
        /// Y in logical pixels.
        y: i32,
        /// TVP mouse button code (`mbLeft`, `mbRight`, `mbMiddle`).
        button: i64,
    },
    /// A pointer button was released.
    PointerUp {
        /// X in logical pixels.
        x: i32,
        /// Y in logical pixels.
        y: i32,
        /// TVP mouse button code.
        button: i64,
    },
    /// The wheel moved.
    Wheel {
        /// Wheel delta (positive = up/away).
        delta: i32,
    },
    /// The surface was resized.
    Resize {
        /// New width in pixels.
        width: u32,
        /// New height in pixels.
        height: u32,
    },
    /// The window gained focus.
    FocusGained,
    /// The window lost focus.
    FocusLost,
    /// The user requested the window be closed.
    CloseRequested,
}

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

/// Maps abstract platform events to TJS-visible events and delivers them.
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
            PlatformEvent::KeyDown { code, key } => Some(TjsEvent {
                name: "onKeyDown",
                members: vec![
                    ("code", EventValue::Int(*code)),
                    ("key", EventValue::Str(key.clone())),
                ],
            }),
            PlatformEvent::KeyUp { code, key } => Some(TjsEvent {
                name: "onKeyUp",
                members: vec![
                    ("code", EventValue::Int(*code)),
                    ("key", EventValue::Str(key.clone())),
                ],
            }),
            PlatformEvent::PointerMove { x, y } => Some(TjsEvent {
                name: "onMouseMove",
                members: vec![
                    ("x", EventValue::Int(i64::from(*x))),
                    ("y", EventValue::Int(i64::from(*y))),
                ],
            }),
            PlatformEvent::PointerDown { x, y, button } => Some(TjsEvent {
                name: "onMouseDown",
                members: vec![
                    ("x", EventValue::Int(i64::from(*x))),
                    ("y", EventValue::Int(i64::from(*y))),
                    ("button", EventValue::Int(*button)),
                ],
            }),
            PlatformEvent::PointerUp { x, y, button } => Some(TjsEvent {
                name: "onMouseUp",
                members: vec![
                    ("x", EventValue::Int(i64::from(*x))),
                    ("y", EventValue::Int(i64::from(*y))),
                    ("button", EventValue::Int(*button)),
                ],
            }),
            PlatformEvent::Wheel { delta } => Some(TjsEvent {
                name: "onMouseWheel",
                members: vec![("delta", EventValue::Int(i64::from(*delta)))],
            }),
            PlatformEvent::Resize { width, height } => Some(TjsEvent {
                name: "onResize",
                members: vec![
                    ("width", EventValue::Int(i64::from(*width))),
                    ("height", EventValue::Int(i64::from(*height))),
                ],
            }),
            PlatformEvent::FocusGained => Some(TjsEvent {
                name: "onFocus",
                members: Vec::new(),
            }),
            PlatformEvent::FocusLost => Some(TjsEvent {
                name: "onBlur",
                members: Vec::new(),
            }),
            PlatformEvent::CloseRequested => Some(TjsEvent {
                name: "onClose",
                members: Vec::new(),
            }),
        }
    }

    /// Deliver a mapped event to the VM.
    ///
    /// This is the delivery seam: the focus object / KAG layer lookup that
    /// turns `event.name` and its members into a script call lands with the
    /// KAG migration. Until then the adapter logs the event so the wiring is
    /// observable and never operates by accident.
    pub fn dispatch(&self, ctx: &mut CompatCtx, event: &TjsEvent) -> Result<(), CompatError> {
        log::debug!(
            "event adapter: dispatching {} ({} member(s)) on engine {:p}",
            event.name,
            event.members.len(),
            ctx.engine().raw(),
        );
        Ok(())
    }
}
