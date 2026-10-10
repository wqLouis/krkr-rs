//! L1 → L3 event adaptation.
//!
//! L1 owns the event loop and pushes [`platform::PlatformEvent`]s; L3
//! maps them to the TJS-visible event the VM dispatches to script handlers.
//! [`EventAdapter::map`] is the pure mapping and [`EventAdapter::dispatch`]
//! is the delivery seam.
//!
//! L3 does **not** redeclare the platform event type: the event loop/type is
//! L1 and only the TJS mapping lives here.
#![allow(unsafe_code)]

use engine::Scene;
use platform::{InputEvent, MouseButton, PlatformEvent};
use tjs::{RetainedValue, TjsValue};

use crate::natives::visual::{ObjectKind, duplicate_object};
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
        }
    }

    /// Deliver a mapped event to the script handler that owns it.
    ///
    /// The target is resolved from the attached L2 [`Scene`]: a mouse event
    /// goes to the layer under the cursor (or the layer that captured the
    /// mouse), a key event to the window's focused layer, and every other
    /// event to the window. The handler is resolved on the object by member
    /// name and invoked with one argument: the event dictionary built from
    /// [`TjsEvent::members`]. A missing handler is a no-op; a throwing
    /// handler is logged and never aborts the frame.
    pub fn dispatch(&self, ctx: &mut CompatCtx, event: &TjsEvent) -> Result<(), CompatError> {
        let Some(target) = resolve_target(ctx, event) else {
            log::debug!(
                "event adapter: no target for {} ({} member(s))",
                event.name,
                event.members.len(),
            );
            return Ok(());
        };
        if let Err(error) = self.deliver(ctx, event, target) {
            if is_missing_member(&error) {
                log::debug!("event adapter: {} has no handler ({error})", event.name);
            } else {
                log::warn!("event adapter: {} dispatch failed ({error})", event.name);
            }
        }
        Ok(())
    }

    /// Build the event dictionary from `event.members` and call `event.name`
    /// on the target's registered script object.
    fn deliver(&self, ctx: &CompatCtx, event: &TjsEvent, target: Target) -> Result<(), String> {
        // SAFETY: a `CompatCtx` is only created for a bound, live engine.
        let host = unsafe { ctx.engine() };
        let (kind, id) = match target {
            Target::Window(id) => (ObjectKind::Window, id),
            Target::Layer(id) => (ObjectKind::Layer, id),
        };
        let Some(object) = duplicate_object(ctx, host, kind, id) else {
            return Err(format!("event target {id} has no script object"));
        };
        let dictionary = match host.eval_retained("%[]", "event") {
            Ok(RetainedValue::Object(dictionary)) => dictionary,
            Ok(RetainedValue::Value(value)) => {
                return Err(format!("event dictionary came back as {value:?}"));
            }
            Err(error) => return Err(format!("event dictionary creation failed: {error}")),
        };
        host.set_member(
            dictionary.raw_id(),
            "type",
            &TjsValue::String(event.name.into()),
        )?;
        if let Ok(target_arg) = host.duplicate_retained(object.raw_id()) {
            // The retention is consumed while the variant copy is built; the
            // handle's drop is then an idempotent release.
            let _ = host.set_member(
                dictionary.raw_id(),
                "target",
                &TjsValue::Retained(target_arg.raw_id() as usize as u64),
            );
            drop(target_arg);
        }
        for (name, value) in &event.members {
            let value = match value {
                EventValue::Int(value) => TjsValue::Integer(*value),
                EventValue::Real(value) => TjsValue::Real(*value),
                EventValue::Str(value) => TjsValue::String(value.clone()),
                EventValue::Bool(value) => TjsValue::Integer(i64::from(*value)),
            };
            host.set_member(dictionary.raw_id(), name, &value)?;
        }
        let dictionary_id = dictionary.raw_id() as usize as u64;
        let result = host.call_member(
            object.raw_id(),
            event.name,
            &[TjsValue::Retained(dictionary_id)],
        );
        // The call consumed the dictionary retention; the drop is a safe no-op.
        drop(dictionary);
        result.map(|_| ())
    }
}

/// The script object a mapped event is delivered to.
#[derive(Clone, Copy)]
pub(crate) enum Target {
    /// A scene window (focus, resize, close).
    Window(u32),
    /// A scene layer (pointer and key events).
    Layer(u32),
}

/// Mouse state with no L2 home: the layer that captured the mouse. It lives
/// in [`CompatServices::state`](crate::CompatServices::state), keyed by type,
/// like the other dispatch state.
#[derive(Default)]
struct InputDispatchState {
    /// The layer that received `onMouseDown` and owns the following
    /// `onMouseMove`/`onMouseUp` until every mouse button is released.
    mouse_capture: Option<u32>,
}

/// Resolve the event's target object and update the mouse-capture and focus
/// state a press implies.
fn resolve_target(ctx: &CompatCtx, event: &TjsEvent) -> Option<Target> {
    if is_pointer_event(event.name) {
        return resolve_pointer(ctx, event.name);
    }
    match event.name {
        "onKeyDown" | "onKeyUp" | "onKeyPress" | "onMouseWheel" => {
            focused_layer(ctx).map(Target::Layer)
        }
        _ => first_window(ctx).map(Target::Window),
    }
}

/// Whether the event is a pointer event that hit-tests the layer tree.
fn is_pointer_event(name: &str) -> bool {
    matches!(
        name,
        "onMouseMove"
            | "onMouseDown"
            | "onMouseUp"
            | "onClick"
            | "onDoubleClick"
            | "onMouseEnter"
            | "onMouseLeave"
    )
}

/// Resolve a pointer event's layer, honouring the mouse capture.
fn resolve_pointer(ctx: &CompatCtx, name: &str) -> Option<Target> {
    let stored = capture_layer(ctx);
    let captured = stored.filter(|id| scene(ctx).is_some_and(|scene| scene.layer(*id).is_some()));
    if stored.is_some() && captured.is_none() {
        set_capture_layer(ctx, None);
    }
    let hit = captured.or_else(|| hit_layer(ctx));
    if name == "onMouseDown" {
        match hit {
            Some(id) => {
                set_capture_layer(ctx, Some(id));
                focus_clicked_layer(ctx, id);
            }
            None => set_capture_layer(ctx, None),
        }
    } else if name == "onMouseUp" && !any_mouse_button_down(ctx) {
        set_capture_layer(ctx, None);
    }
    hit.map(Target::Layer)
}

/// The layer under the cursor in the first visible window.
fn hit_layer(ctx: &CompatCtx) -> Option<u32> {
    let (x, y) = pointer(ctx)?;
    let scene = scene(ctx)?;
    for window in &scene.windows {
        if !window.visible {
            continue;
        }
        let Some(primary) = window.primary_layer else {
            continue;
        };
        if let Some(layer) = scene.layer_at(primary, x, y, false, false) {
            return Some(layer);
        }
    }
    None
}

/// The focused layer of the first window.
fn focused_layer(ctx: &CompatCtx) -> Option<u32> {
    let window = scene(ctx)?.windows.first()?;
    window.focused_layer
}

/// The first window's id.
fn first_window(ctx: &CompatCtx) -> Option<u32> {
    let window = scene(ctx)?.windows.first()?;
    Some(window.id)
}

/// The attached scene, or `None` when the host did not attach one.
fn scene(ctx: &CompatCtx) -> Option<&Scene> {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let scene = services.scene()?;
    // SAFETY: `attach_scene` keeps the pointer valid while the binding is
    // installed.
    Some(unsafe { &*scene })
}

/// The current pointer position in window pixels.
fn pointer(ctx: &CompatCtx) -> Option<(i32, i32)> {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let input = services.input()?;
    // SAFETY: `attach_input` keeps the pointer valid while the binding is
    // installed; the borrow ends before any VM call.
    let input = unsafe { &*input };
    let (x, y) = input.pointer();
    Some((x.round() as i32, y.round() as i32))
}

/// Whether a standard mouse button is held.
fn any_mouse_button_down(ctx: &CompatCtx) -> bool {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let Some(input) = services.input() else {
        return false;
    };
    // SAFETY: `attach_input` keeps the pointer valid while the binding is
    // installed; the borrow ends before any VM call.
    let input = unsafe { &*input };
    input.is_button_down(MouseButton::Left)
        || input.is_button_down(MouseButton::Right)
        || input.is_button_down(MouseButton::Middle)
}

/// The layer that captured the mouse, if any.
fn capture_layer(ctx: &CompatCtx) -> Option<u32> {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    services.state::<InputDispatchState>().mouse_capture
}

/// Set (or clear) the mouse-capturing layer.
fn set_capture_layer(ctx: &CompatCtx, layer: Option<u32>) {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    services.state::<InputDispatchState>().mouse_capture = layer;
}

/// Give a clicked focusable layer the keyboard focus.
fn focus_clicked_layer(ctx: &CompatCtx, id: u32) {
    if !scene(ctx).is_some_and(|scene| scene.node_focusable(id)) {
        return;
    }
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let Some(scene) = services.scene() else {
        return;
    };
    // SAFETY: `attach_scene` keeps the pointer valid while the binding is
    // installed.
    unsafe { &mut *scene }.set_focus(id);
}

/// Whether a member call failed because the member is absent.
fn is_missing_member(error: &str) -> bool {
    error.contains("does not exist")
}

/// Drop the mouse capture (the `releaseCapture` path).
pub(crate) fn release_mouse_capture(ctx: &CompatCtx) {
    set_capture_layer(ctx, None);
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
    }
}
