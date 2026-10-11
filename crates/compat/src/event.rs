//! L1 → L3 event adaptation.
//!
//! L1 owns the event loop and pushes [`platform::PlatformEvent`]s; L3
//! maps them to the TJS-visible event the VM dispatches to script handlers.
//! [`EventAdapter::map`] is the mapping and [`EventAdapter::dispatch`] is the
//! delivery seam.
//!
//! L3 does **not** redeclare the platform event type: the event loop/type is
//! L1 and only the TJS mapping lives here.
#![allow(unsafe_code)]

use engine::Scene;
use engine::input::InputState;
use platform::{InputEvent, MouseButton, PlatformEvent};
use tjs::TjsValue;

use crate::natives::input::shift_flags;
use crate::natives::visual::{ObjectKind, duplicate_object};
use crate::{CompatCtx, CompatError};

/// A TJS-visible event: the handler name and its positional arguments.
#[derive(Clone, Debug, PartialEq)]
pub struct TjsEvent {
    /// The handler/method name (e.g. `"onKeyDown"`).
    pub name: &'static str,
    /// The positional arguments handed to the handler, in the order the
    /// reference `Fire*`/`On*` method passes them.
    pub args: Vec<EventValue>,
}

/// A scalar event argument value.
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
///
/// The pointer position and shift mask are cached from the most recent
/// [`EventAdapter::apply_input_event`] so the state-free [`Self::map`] can
/// build the positional arguments the reference input events carry.
#[derive(Default)]
pub struct EventAdapter {
    /// The latest L2 pointer position in physical pixels.
    pointer: (f32, f32),
    /// The latest TVP `TVP_SS_*` shift mask.
    shift: u32,
}

impl EventAdapter {
    /// Create an adapter.
    pub fn new() -> Self {
        Self::default()
    }

    /// Map a platform event to the TJS-visible event, or `None` for events
    /// the script surface ignores.
    pub fn map(&self, event: &PlatformEvent) -> Option<TjsEvent> {
        match event {
            // `OnResize` posts `onResize` with no arguments
            // (`reference/cpp/core/visual/WindowIntf.cpp:302-310`).
            PlatformEvent::Resized(_, _) => Some(TjsEvent {
                name: "onResize",
                args: Vec::new(),
            }),
            // The scale factor has no KAG handler; L2 tracks it if needed.
            PlatformEvent::ScaleFactorChanged(_) => None,
            // `OnClose` posts `onCloseQuery(true)`
            // (`reference/cpp/core/visual/WindowIntf.cpp:292-300`).
            PlatformEvent::CloseRequested => Some(TjsEvent {
                name: "onCloseQuery",
                args: vec![EventValue::Int(1)],
            }),
            // `OnActivate` posts `onActivate`/`onDeactivate` with no
            // arguments (`reference/cpp/core/visual/WindowIntf.cpp:556-570`).
            PlatformEvent::Focused(true) => Some(TjsEvent {
                name: "onActivate",
                args: Vec::new(),
            }),
            PlatformEvent::Focused(false) => Some(TjsEvent {
                name: "onDeactivate",
                args: Vec::new(),
            }),
            PlatformEvent::Input(input_event) => self.map_input(input_event),
        }
    }

    /// Cache the L2 pointer position and shift mask after an input event is
    /// applied. [`Self::map`] reads them to build positional arguments.
    pub(crate) fn cache_input_state(&mut self, state: &InputState) {
        self.pointer = state.pointer();
        self.shift = shift_flags(state);
    }

    /// Map one raw input event to the TJS-visible event.
    ///
    /// The argument list is the reference `Fire*` positional list, not an
    /// event dictionary: `onMouseMove(x, y, shift)`,
    /// `onMouseDown`/`onMouseUp(x, y, button, shift)`,
    /// `onMouseWheel(shift, delta, x, y)` and
    /// `onKeyDown`/`onKeyUp(key, shift, process)`
    /// (`reference/cpp/core/visual/LayerIntf.cpp:3410-3891`).
    fn map_input(&self, input: &InputEvent) -> Option<TjsEvent> {
        let flags = i64::from(self.shift);
        let (pointer_x, pointer_y) = self.pointer_coordinates();
        match input {
            InputEvent::PointerMoved { x, y } => Some(TjsEvent {
                name: "onMouseMove",
                args: vec![
                    EventValue::Int(coordinate(*x)),
                    EventValue::Int(coordinate(*y)),
                    EventValue::Int(flags),
                ],
            }),
            // The window-level pointer-leave is not a KAG event.
            InputEvent::PointerLeft => None,
            InputEvent::MouseButton { button, pressed } => Some(TjsEvent {
                name: if *pressed { "onMouseDown" } else { "onMouseUp" },
                args: vec![
                    EventValue::Int(pointer_x),
                    EventValue::Int(pointer_y),
                    EventValue::Int(mouse_button_code(*button)),
                    EventValue::Int(flags),
                ],
            }),
            InputEvent::Wheel { delta } => Some(TjsEvent {
                name: "onMouseWheel",
                args: vec![
                    EventValue::Int(flags),
                    EventValue::Int(coordinate(*delta)),
                    EventValue::Int(pointer_x),
                    EventValue::Int(pointer_y),
                ],
            }),
            InputEvent::Key { code, pressed } => Some(TjsEvent {
                name: if *pressed { "onKeyDown" } else { "onKeyUp" },
                args: vec![
                    EventValue::Int(i64::from(*code)),
                    EventValue::Int(flags),
                    EventValue::Int(1),
                ],
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
    /// name and invoked with [`TjsEvent::args`] as positional arguments. A
    /// missing handler is a no-op; a throwing handler is logged and never
    /// aborts the frame.
    pub fn dispatch(&self, ctx: &mut CompatCtx, event: &TjsEvent) -> Result<(), CompatError> {
        let Some(target) = resolve_target(ctx, event) else {
            log::debug!(
                "event adapter: no target for {} ({} argument(s))",
                event.name,
                event.args.len(),
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

    /// Call `event.name` on the target's registered script object with
    /// `event.args` as positional arguments.
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
        let args: Vec<TjsValue> = event.args.iter().map(event_value_to_tjs).collect();
        host.call_member(object.raw_id(), event.name, &args)
            .map(|_| ())
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

/// Round a logical coordinate to the TJS integer the event handlers receive.
fn coordinate(value: f32) -> i64 {
    i64::from(value.round() as i32)
}

impl EventAdapter {
    /// The cached pointer position as TJS integers.
    fn pointer_coordinates(&self) -> (i64, i64) {
        (coordinate(self.pointer.0), coordinate(self.pointer.1))
    }
}

/// Convert a mapped event argument to the TJS value the handler receives.
fn event_value_to_tjs(value: &EventValue) -> TjsValue {
    match value {
        EventValue::Int(value) => TjsValue::Integer(*value),
        EventValue::Real(value) => TjsValue::Real(*value),
        EventValue::Str(value) => TjsValue::String(value.clone()),
        EventValue::Bool(value) => TjsValue::Integer(i64::from(*value)),
    }
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
