//! L1 → L3 event adaptation.
//!
//! L1 owns the event loop and pushes [`platform::PlatformEvent`]s; L3
//! maps them to the TJS-visible events the VM dispatches to script handlers.
//! [`EventAdapter::map`] is the mapping, [`EventAdapter::dispatch`] is the
//! delivery seam and [`EventAdapter::dispatch_frame`] is the batch the VM
//! host's frame body runs.
//!
//! L3 does **not** redeclare the platform event type: the event loop/type is
//! L1 and only the TJS mapping lives here.
#![allow(unsafe_code)]

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use api::host::TouchPhase;

use crate::collections::Map;
use crate::engine_api::HitCandidate;
use crate::engine_api::InputState;
use crate::engine_api::Scene;
use platform::{InputEvent, MouseButton, PlatformEvent};
use tjs::TjsValue;

use crate::natives::input::shift_state::{TVP_SS_ALT, TVP_SS_CTRL, TVP_SS_SHIFT};
use crate::natives::input::{MOUSE_CLICK_MAX_MOVE, mouse_button, shift_flags, vk_from_physical};
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

impl TjsEvent {
    /// The argument list the window handler receives.
    ///
    /// The window and layer handlers differ only for the key events. The
    /// reference window `onKeyDown`/`onKeyUp` take `(key, shift)` and
    /// `onKeyPress` takes `(key)`
    /// (`reference/cpp/core/visual/WindowIntf.cpp:984-1010`,
    /// `reference/cpp/core/visual/WindowIntf.cpp:1130-1160`); the layer
    /// versions append the `process` flag
    /// (`reference/cpp/core/visual/LayerIntf.cpp:3844-3877`,
    /// `reference/cpp/core/visual/LayerIntf.cpp:10294-10319`). Every other
    /// input event carries the same arguments to both.
    fn window_args(&self) -> Vec<EventValue> {
        match self.name {
            "onKeyDown" | "onKeyUp" => self.args.iter().take(2).cloned().collect(),
            "onKeyPress" => self.args.iter().take(1).cloned().collect(),
            _ => self.args.clone(),
        }
    }
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
///
/// The adapter also holds the events a frame did not dispatch: the dispatch
/// budget stops a frame, and the next frame delivers what was left.
pub struct EventAdapter {
    /// The latest L2 pointer position in physical pixels.
    pointer: (f32, f32),
    /// The latest TVP `TVP_SS_*` shift mask.
    shift: u32,
    /// The events a frame did not dispatch, oldest first.
    deferred: VecDeque<PlatformEvent>,
    /// The wall-clock time one frame may spend dispatching events.
    dispatch_budget: Duration,
}

impl Default for EventAdapter {
    fn default() -> Self {
        Self {
            pointer: (0.0, 0.0),
            shift: 0,
            deferred: VecDeque::new(),
            dispatch_budget: Self::DEFAULT_DISPATCH_BUDGET,
        }
    }
}

impl EventAdapter {
    /// The wall-clock time one frame may spend dispatching events.
    ///
    /// This is a presentation policy, not a compatibility rule: a frame
    /// that collects a flood of events dispatches as many of them as fit in
    /// the budget and defers the rest to the next frame, so one frame
    /// cannot spend seconds in the event loop. The budget is a quarter of a
    /// 60 fps frame, which leaves the rest of the frame for the
    /// presentation, and it is far above the cost of ordinary input.
    pub const DEFAULT_DISPATCH_BUDGET: Duration = Duration::from_millis(4);

    /// Create an adapter.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an adapter whose frame dispatch budget is `budget`.
    ///
    /// The budget bounds the wall-clock time one frame spends dispatching
    /// events; the events that do not fit stay queued for the next frame.
    /// A budget of zero still dispatches one event per frame, so no event
    /// waits forever.
    pub fn with_dispatch_budget(budget: Duration) -> Self {
        Self {
            dispatch_budget: budget,
            ..Self::default()
        }
    }

    /// Map a platform event to the TJS-visible event it produces, or `None`
    /// for events the script surface ignores.
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

    /// Map one raw input event to the TJS-visible event(s).
    ///
    /// The argument list is the reference `Fire*` positional list, not an
    /// event dictionary: `onMouseMove(x, y, shift)`,
    /// `onMouseDown`/`onMouseUp(x, y, button, shift)`,
    /// `onMouseWheel(shift, delta, x, y)`,
    /// `onKeyDown`/`onKeyUp(key, shift, process)`,
    /// `onKeyPress(key, process)` and
    /// the touch family `(x, y, cx, cy[, id])`
    /// (`reference/cpp/core/visual/LayerIntf.cpp:3410-3891`,
    /// `reference/cpp/core/visual/LayerIntf.cpp:10294-10319`).
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
            // The pointer leaving the window is the window-level
            // `onMouseLeave` (`tTVPOnMouseLeaveInputEvent`,
            // `reference/cpp/core/visual/WindowIntf.h:479-485`); the layer
            // leave comes from the manager's tracking, emitted in
            // [`Self::dispatch`].
            InputEvent::PointerLeft => Some(TjsEvent {
                name: "onMouseLeave",
                args: Vec::new(),
            }),
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
                    EventValue::Int(wheel_delta(*delta)),
                    EventValue::Int(pointer_x),
                    EventValue::Int(pointer_y),
                ],
            }),
            InputEvent::Key { code, pressed } => Some(TjsEvent {
                name: if *pressed { "onKeyDown" } else { "onKeyUp" },
                args: vec![
                    EventValue::Int(i64::from(vk_from_physical(*code))),
                    EventValue::Int(flags),
                    EventValue::Int(1),
                ],
            }),
            // Committed text posts `onKeyPress(key, process)`
            // (`FireKeyPress`,
            // `reference/cpp/core/visual/LayerIntf.cpp:3868-3877`). The
            // reference platform posts nothing for an empty key
            // (`if(tjsNativeInstance && vk)`,
            // `reference/cpp/core/environ/impl/TVPWindow.cpp:909-919`).
            InputEvent::Text(text) if text.is_empty() => None,
            InputEvent::Text(text) => Some(TjsEvent {
                name: "onKeyPress",
                args: vec![EventValue::Str(text.clone()), EventValue::Int(1)],
            }),
            InputEvent::Touch { id, phase, x, y } => {
                let name = match phase {
                    TouchPhase::Started => "onTouchDown",
                    TouchPhase::Moved => "onTouchMove",
                    // The reference has no touch-cancel input event; a
                    // cancelled contact ends like an ended one
                    // (`tTVPOnTouchUpInputEvent`, `reference/cpp/core/visual/WindowIntf.h:601-614`).
                    TouchPhase::Ended | TouchPhase::Cancelled => "onTouchUp",
                };
                Some(TjsEvent {
                    name,
                    args: vec![
                        EventValue::Real(f64::from(*x)),
                        EventValue::Real(f64::from(*y)),
                        EventValue::Real(f64::from(*x)),
                        EventValue::Real(f64::from(*y)),
                        EventValue::Int(i64::from(*id as u32)),
                    ],
                })
            }
        }
    }

    /// Deliver one frame's platform events to the script handlers.
    ///
    /// This is the batch entry point the VM host's frame body uses: every
    /// event is applied to the L2 input state, mapped and delivered, in the
    /// order the platform produced it. No event is merged with another and
    /// none is dropped.
    ///
    /// The frame stops dispatching when its dispatch time reaches the
    /// budget (see [`EventAdapter::with_dispatch_budget`]). The events that
    /// did not fit stay queued and the next call delivers them **first**, in
    /// the same order, so the delivery order is the arrival order. The
    /// budget is checked after an event was dispatched, so every frame
    /// delivers at least one event and no event waits forever.
    pub fn dispatch_frame(
        &mut self,
        ctx: &mut CompatCtx,
        events: &[PlatformEvent],
    ) -> Result<(), CompatError> {
        let started = Instant::now();
        // The events the last frame did not dispatch come first.
        let mut deferred = std::mem::take(&mut self.deferred);
        let deferred_len = deferred.len();
        // The window's move event is discardable: the pending move event is
        // dropped when a newer one arrives, so only the newest move of the
        // frame reaches the window. The position of that move in the frame's
        // batch is the one the window sees.
        let last_move = events
            .iter()
            .rposition(is_move_event)
            .map(|index| deferred_len + index)
            .or_else(|| deferred.iter().rposition(is_move_event));
        let mut queued = 0;
        let mut index = 0;
        loop {
            let event = if queued < deferred_len {
                &deferred[queued]
            } else if let Some(event) = events.get(index) {
                event
            } else {
                break;
            };
            // The event's position in the frame's batch.
            let position = if queued < deferred_len {
                queued
            } else {
                deferred_len + index
            };
            self.apply_input_event(ctx, event);
            if let Some(mapped) = self.map(event) {
                // The layer's move event fires only when the pointer position
                // changed since the last move event.
                let deliver_layer_move =
                    mapped.name != "onMouseMove" || pointer_moved(ctx, &mapped);
                self.dispatch_event(
                    ctx,
                    &mapped,
                    Some(position) == last_move,
                    deliver_layer_move,
                )?;
            }
            if queued < deferred_len {
                queued += 1;
            } else {
                index += 1;
            }
            // The budget is checked after the dispatch, so a frame always
            // delivers at least one event.
            if started.elapsed() >= self.dispatch_budget {
                break;
            }
        }
        // The events that did not fit stay queued, oldest first.
        deferred.drain(..queued);
        deferred.extend(events[index..].iter().cloned());
        self.deferred = deferred;
        Ok(())
    }

    /// Deliver a mapped event to the script handler that owns it.
    ///
    /// The reference posts every input event to the window **and** to the
    /// layer the manager resolves: `tTVPOnMouseDownInputEvent::Deliver` calls
    /// `window->OnMouseDown`, which posts `onMouseDown` to the window and
    /// then notifies the layer manager through the draw device
    /// (`reference/cpp/core/visual/WindowIntf.cpp:331-345`,
    /// `reference/cpp/core/visual/impl/DrawDevice.cpp:192-238`). The window
    /// handler is resolved first, then the layer
    /// (`onMouseMove`/`onMouseDown`/`onMouseUp` by hit test or mouse capture,
    /// key/wheel by focus, touch by hit test).
    ///
    /// A mapped `onMouseLeave` is the window-level event; the layer-level
    /// `onMouseEnter`/`onMouseLeave` are the manager's hit-test transitions
    /// (`reference/cpp/core/visual/LayerManager.cpp:424-468`). A mouse
    /// release additionally posts `onClick`/`onDoubleClick` when the
    /// press/release pair is a click
    /// (`reference/cpp/core/visual/WindowIntf.cpp:311-322`,
    /// `reference/cpp/core/visual/LayerManager.cpp:358-372`).
    ///
    /// A missing handler is a no-op; a throwing handler is logged and never
    /// aborts the frame.
    pub fn dispatch(&self, ctx: &mut CompatCtx, event: &TjsEvent) -> Result<(), CompatError> {
        self.dispatch_event(ctx, event, true, true)
    }

    /// Deliver a mapped event, leaving the move events out when the frame's
    /// batch says so.
    ///
    /// The window's move event is discardable, so a frame that collected a
    /// burst of moves delivers one move event to the window: the newest one.
    /// The layer's move event fires only when the pointer position changed.
    /// A single event is delivered whole.
    fn dispatch_event(
        &self,
        ctx: &mut CompatCtx,
        event: &TjsEvent,
        deliver_window_move: bool,
        deliver_layer_move: bool,
    ) -> Result<(), CompatError> {
        // A committed multi-character string is one `onKeyPress` per
        // character, matching the reference platform's per-`tjs_char`
        // post (`reference/cpp/core/environ/impl/TVPWindow.cpp:909-919`).
        if event.name == "onKeyPress"
            && let Some(EventValue::Str(text)) = event.args.first()
            && text.chars().count() > 1
        {
            for character in text.chars() {
                let single = TjsEvent {
                    name: "onKeyPress",
                    args: vec![EventValue::Str(character.to_string()), EventValue::Int(1)],
                };
                self.dispatch_event(ctx, &single, deliver_window_move, deliver_layer_move)?;
            }
            return Ok(());
        }

        // Pointer events resolve the captured/hit layer and update the mouse
        // capture. `onMouseLeave` is the window-level event and does not
        // perform a hit test.
        let pointer_layer = if is_pointer_event(event.name) {
            resolve_pointer(ctx, event.name)
        } else {
            None
        };

        // The window that receives the event is resolved once: the window
        // delivery, the enter a move posts and the click a release posts all
        // go to the same window.
        let window = first_window(ctx);

        // The window receives every mapped event but its move event, which
        // is discardable: the pending move event is dropped when a newer one
        // arrives.
        if let Some(id) = window
            && (event.name != "onMouseMove" || deliver_window_move)
        {
            deliver_checked(ctx, event.name, &event.window_args(), Target::Window(id));
        }

        // The layer receives the input events that hit it or have focus.
        let layer = match event.name {
            "onKeyDown" | "onKeyUp" | "onKeyPress" | "onMouseWheel" => focused_layer(ctx),
            name if is_pointer_event(name) => pointer_layer,
            "onTouchDown" | "onTouchMove" | "onTouchUp" => self.touch_layer(ctx, event),
            _ => None,
        };

        // Layer enter/leave is the manager's hit-test transition, checked
        // before the move/press delivery, exactly like `PrimaryMouseMove`
        // (`reference/cpp/core/visual/LayerManager.cpp:437-457`).
        if matches!(event.name, "onMouseMove" | "onMouseDown") {
            track_layer_enter_leave(ctx, layer);
        } else if event.name == "onMouseLeave" {
            track_layer_enter_leave(ctx, None);
            set_pointer_inside(ctx, false);
        }

        if let Some(id) = layer
            && deliver_layer_move
        {
            let args = self.layer_args(ctx, event, id);
            deliver_checked(ctx, event.name, &args, Target::Layer(id));
        }

        // A pointer move that re-enters the window posts the window-level
        // `onMouseEnter` (`tTVPOnMouseEnterInputEvent`,
        // `reference/cpp/core/visual/WindowIntf.h:473-478`).
        if event.name == "onMouseMove" && !pointer_inside(ctx) {
            set_pointer_inside(ctx, true);
            if let Some(id) = window {
                deliver_checked(ctx, "onMouseEnter", &[], Target::Window(id));
            }
        }

        // A mouse release completes a click or a double click.
        if event.name == "onMouseUp"
            && let Some(click) = self.click_event(ctx, event)
        {
            if let Some(id) = pointer_layer {
                let args = self.layer_args(ctx, &click, id);
                deliver_checked(ctx, click.name, &args, Target::Layer(id));
            }
            if let Some(id) = window {
                deliver_checked(ctx, click.name, &click.args, Target::Window(id));
            }
        }
        Ok(())
    }

    /// The layer-level argument list for `event`, converting the pointer
    /// position from window coordinates into the layer's own coordinates by
    /// subtracting the offset to the primary layer. The window handler keeps
    /// the window coordinates.
    fn layer_args(&self, ctx: &CompatCtx, event: &TjsEvent, layer_id: u32) -> Vec<EventValue> {
        if !matches!(
            event.name,
            "onMouseMove"
                | "onMouseDown"
                | "onMouseUp"
                | "onClick"
                | "onDoubleClick"
                | "onTouchDown"
                | "onTouchMove"
                | "onTouchUp"
        ) {
            return event.args.clone();
        }
        let mut args = event.args.clone();
        let (Some(x), Some(y)) = (args.first().cloned(), args.get(1).cloned()) else {
            return args;
        };
        let (local_x, local_y) = match (x, y) {
            (EventValue::Int(x), EventValue::Int(y)) => {
                let (local_x, local_y) = to_layer_local_int(ctx, layer_id, x, y);
                (EventValue::Int(local_x), EventValue::Int(local_y))
            }
            (EventValue::Real(x), EventValue::Real(y)) => {
                let (local_x, local_y) = to_layer_local(ctx, layer_id, x, y);
                (EventValue::Real(local_x), EventValue::Real(local_y))
            }
            _ => return args,
        };
        args[0] = local_x;
        args[1] = local_y;
        args
    }

    /// The layer a touch event targets, honouring the touch capture
    /// (`SetTouchCapture`/`GetTouchCapture`,
    /// `reference/cpp/core/visual/LayerManager.cpp:516-553`).
    fn touch_layer(&self, ctx: &CompatCtx, event: &TjsEvent) -> Option<u32> {
        let id = match event.args.get(4) {
            Some(EventValue::Int(value)) if *value >= 0 => *value as u64,
            _ => return None,
        };
        let stored = {
            // SAFETY: a `CompatCtx` is only created for a bound, live engine.
            let services = unsafe { ctx.services() };
            services
                .state::<InputDispatchState>()
                .touch_capture
                .get(&id)
                .copied()
        };
        match event.name {
            "onTouchDown" => {
                let layer = touch_hit(ctx, event);
                if let Some(layer_id) = layer {
                    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
                    let services = unsafe { ctx.services() };
                    services
                        .state::<InputDispatchState>()
                        .touch_capture
                        .insert(id, layer_id);
                }
                layer
            }
            "onTouchUp" => {
                let layer = stored.or_else(|| touch_hit(ctx, event));
                // SAFETY: a `CompatCtx` is only created for a bound, live engine.
                let services = unsafe { ctx.services() };
                services
                    .state::<InputDispatchState>()
                    .touch_capture
                    .remove(&id);
                layer
            }
            _ => stored.or_else(|| touch_hit(ctx, event)),
        }
    }

    /// The `onClick`/`onDoubleClick` implied by a mouse release.
    ///
    /// The click sequence is the L2 [`InputState`] one: a release is a click
    /// when the current multi-click sequence has not timed out and the
    /// pointer has not moved past the sequence distance. The reference
    /// establishes the sequence in the platform gesture recognizer
    /// (`reference/cpp/core/visual/LayerManager.cpp:358-372`); the vendored
    /// source does not contain the recognizer, so the port reuses the
    /// `Mouse.getClickCount` sequence and its existence comes from the
    /// `layer.onClick`/`layer.onDoubleClick` natives.
    fn click_event(&self, ctx: &CompatCtx, event: &TjsEvent) -> Option<TjsEvent> {
        let index = match event.args.get(2)? {
            EventValue::Int(value) if *value >= 0 => *value as usize,
            _ => return None,
        };
        let button = mouse_button(index)?;
        let state = input(ctx)?;
        let count = state.button_click_count(button);
        if count == 0 {
            return None;
        }
        let press = state.button_last_click_pos(button)?;
        let (pointer_x, pointer_y) = state.pointer();
        let max_move = MOUSE_CLICK_MAX_MOVE as f32;
        if (pointer_x - press.0).abs() > max_move || (pointer_y - press.1).abs() > max_move {
            return None;
        }
        let (x, y) = self.pointer_coordinates();
        let name = if count >= 2 {
            "onDoubleClick"
        } else {
            "onClick"
        };
        Some(TjsEvent {
            name,
            args: vec![EventValue::Int(x), EventValue::Int(y)],
        })
    }

    /// The cached pointer position as TJS integers.
    fn pointer_coordinates(&self) -> (i64, i64) {
        (coordinate(self.pointer.0), coordinate(self.pointer.1))
    }
}

/// The script object a mapped event is delivered to.
#[derive(Clone, Copy)]
pub(crate) enum Target {
    /// A scene window (focus, resize, close).
    Window(u32),
    /// A scene layer (pointer, key and touch events).
    Layer(u32),
}

/// Mouse state with no L2 home: the layer that captured the mouse and the
/// layer the pointer last entered. It lives in
/// [`CompatServices::state`](crate::CompatServices::state), keyed by type,
/// like the other dispatch state.
#[derive(Default)]
struct InputDispatchState {
    /// The layer that received `onMouseDown` and owns the following
    /// `onMouseMove`/`onMouseUp` until every mouse button is released.
    mouse_capture: Option<u32>,
    /// The layer that received the last `onMouseEnter` (the manager's
    /// `LastMouseMoveSent`), for the enter/leave transition.
    last_mouse_layer: Option<u32>,
    /// The layer that captured each active touch contact
    /// (`SetTouchCapture`), so a drag stays on the pressed layer.
    touch_capture: Map<u64, u32>,
    /// Whether the pointer is currently inside the window (the window-level
    /// enter/leave transition).
    pointer_inside: bool,
    /// The pointer position of the last move event, in window pixels.
    last_move_position: Option<(i64, i64)>,
}

/// Whether the event is a pointer event that hit-tests the layer tree.
fn is_pointer_event(name: &str) -> bool {
    matches!(name, "onMouseMove" | "onMouseDown" | "onMouseUp")
}

/// Whether a platform event is a pointer move.
fn is_move_event(event: &PlatformEvent) -> bool {
    matches!(event, PlatformEvent::Input(InputEvent::PointerMoved { .. }))
}

/// Whether the pointer position changed since the last move event.
///
/// The position of every move event is recorded, so a move that repeats the
/// previous position reports no change.
fn pointer_moved(ctx: &CompatCtx, event: &TjsEvent) -> bool {
    let position = match (event.args.first(), event.args.get(1)) {
        (Some(EventValue::Int(x)), Some(EventValue::Int(y))) => Some((*x, *y)),
        _ => None,
    };
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let mut state = services.state::<InputDispatchState>();
    let previous = state.last_move_position;
    state.last_move_position = position;
    previous != position
}

/// Resolve a pointer event's layer, honouring the mouse capture.
fn resolve_pointer(ctx: &CompatCtx, name: &str) -> Option<u32> {
    let stored = capture_layer(ctx);
    let captured = stored.filter(|id| scene(ctx).is_some_and(|scene| scene.layer(*id).is_some()));
    if stored.is_some() && captured.is_none() {
        set_capture_layer(ctx, None);
    }
    let hit = captured.or_else(|| {
        let (x, y) = pointer(ctx)?;
        hit_layer(ctx, x, y)
    });
    if name == "onMouseDown" {
        // The reference press path sets the mouse capture and fires
        // `onMouseDown`; it never moves layer focus
        // (`reference/cpp/core/visual/LayerManager.cpp:374-399`).
        match hit {
            Some(id) => set_capture_layer(ctx, Some(id)),
            None => set_capture_layer(ctx, None),
        }
    } else if name == "onMouseUp" && !any_mouse_button_down(ctx) {
        set_capture_layer(ctx, None);
    }
    hit
}

/// The layer under a window-space point in the first visible window.
///
/// The hit test reads a layer's main image, so the images of the window it
/// tests are waited for first: a load whose decode is still running leaves the
/// bitmap with the probed size and no pixels.
fn hit_layer(ctx: &CompatCtx, x: i32, y: i32) -> Option<u32> {
    let roots: Vec<(u32, u32)> = {
        let scene = scene_mut(ctx)?;
        scene
            .windows
            .iter()
            .filter(|window| window.visible)
            .filter_map(|window| window.primary_layer.map(|primary| (window.id, primary)))
            .collect()
    };
    for (window, primary) in roots {
        {
            let scene = scene_mut(ctx)?;
            resolve_layer_images(scene, window);
        }
        if let Some(layer) = hit_test(ctx, primary, x, y, false, false) {
            return Some(layer);
        }
    }
    None
}

/// The layer at the point, with the `onHitTest` hook the hit test fires.
///
/// The hit test posts `onHitTest` to the layer that hits geometrically and
/// takes the layer's hit flag as the result: the flag starts accepted, the
/// event may clear it, and a cleared flag sends the search on to the layer
/// behind. The event is delivered synchronously, inside the hit test, and the
/// layer that receives it is the layer's own script object.
///
/// The scene is read and written one step at a time, and no borrow of it is
/// live while the event runs: the handler is free to change the layer tree.
pub(crate) fn hit_test(
    ctx: &CompatCtx,
    layer_id: u32,
    x: i32,
    y: i32,
    exclude_self: bool,
    get_disabled: bool,
) -> Option<u32> {
    let mut after = None;
    loop {
        let candidate = {
            let scene = scene(ctx)?;
            scene.layer_hit_next(layer_id, x, y, exclude_self, after)
        }?;
        after = Some(candidate.layer);
        {
            let scene = scene_mut(ctx)?;
            scene.set_hit_test_accepted(candidate.layer, true);
        }
        deliver_hit_test(ctx, &candidate);
        let accepted = {
            let scene = scene(ctx)?;
            scene.hit_test_accepted(candidate.layer)
        };
        match accepted {
            Some(true) => {
                let scene = scene(ctx)?;
                return scene.layer_hit_outcome(candidate.layer, get_disabled);
            }
            // The layer cleared its hit flag: the search continues behind it.
            Some(false) => continue,
            // The layer is gone: it cannot be hit.
            None => continue,
        }
    }
}

/// Deliver the `onHitTest` event to the hit layer.
///
/// The event carries the point in the layer's own coordinates and `true` as
/// the third argument.
fn deliver_hit_test(ctx: &CompatCtx, candidate: &HitCandidate) {
    deliver_checked(
        ctx,
        "onHitTest",
        &[
            EventValue::Int(i64::from(candidate.x)),
            EventValue::Int(i64::from(candidate.y)),
            EventValue::Bool(true),
        ],
        Target::Layer(candidate.layer),
    );
}

/// Waits for the main images of `window`'s layers.
///
/// A layer can carry an image whose decode is still running: the bitmap then
/// has the probed size and no pixels. A hit test that reads those pixels waits
/// here first, and only for this window's layers — a decode for another window
/// stays with the frame's image barrier.
pub(crate) fn resolve_layer_images(scene: &mut Scene, window: u32) {
    // Nothing is decoding in the steady state, and the walk below allocates.
    if !scene.bitmaps.iter().any(|bitmap| bitmap.pending.is_some()) {
        return;
    }
    let bitmap_ids: Vec<u32> = scene
        .window_layer_order(window)
        .into_iter()
        .filter_map(|id| scene.layer(id).and_then(|layer| layer.bitmap))
        .collect();
    for bitmap_id in bitmap_ids {
        scene.resolve_bitmap(bitmap_id);
    }
}

/// Hit-test a touch event's window-space point.
fn touch_hit(ctx: &CompatCtx, event: &TjsEvent) -> Option<u32> {
    let x = match event.args.first()? {
        EventValue::Real(value) => *value,
        EventValue::Int(value) => *value as f64,
        _ => return None,
    };
    let y = match event.args.get(1)? {
        EventValue::Real(value) => *value,
        EventValue::Int(value) => *value as f64,
        _ => return None,
    };
    hit_layer(ctx, x.round() as i32, y.round() as i32)
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

/// The attached scene, mutably, or `None` when the host did not attach one.
fn scene_mut<'a>(ctx: &CompatCtx) -> Option<&'a mut Scene> {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let scene = services.scene()?;
    // SAFETY: `attach_scene` keeps the pointer valid while the binding is
    // installed, and the VM is single-threaded.
    Some(unsafe { &mut *scene })
}

/// The attached input state, or `None` when the host did not attach one.
fn input<'a>(ctx: &CompatCtx) -> Option<&'a InputState> {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let input = services.input()?;
    // SAFETY: `attach_input` keeps the pointer valid while the binding is
    // installed.
    Some(unsafe { &*input })
}

/// The current pointer position in window pixels.
fn pointer(ctx: &CompatCtx) -> Option<(i32, i32)> {
    let (x, y) = input(ctx)?.pointer();
    Some((x.round() as i32, y.round() as i32))
}

/// Whether a standard mouse button is held.
fn any_mouse_button_down(ctx: &CompatCtx) -> bool {
    let Some(input) = input(ctx) else {
        return false;
    };
    input.is_button_down(MouseButton::Left)
        || input.is_button_down(MouseButton::Right)
        || input.is_button_down(MouseButton::Middle)
}

/// Whether the pointer is currently inside the window.
fn pointer_inside(ctx: &CompatCtx) -> bool {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    services.state::<InputDispatchState>().pointer_inside
}

/// Set the pointer-inside flag.
fn set_pointer_inside(ctx: &CompatCtx, inside: bool) {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    services.state::<InputDispatchState>().pointer_inside = inside;
}

/// Fire the layer enter/leave transition for a new hit layer.
///
/// The reference tracks `LastMouseMoveSent` and fires `onMouseLeave` on the
/// previous layer and `onMouseEnter` on the new one
/// (`reference/cpp/core/visual/LayerManager.cpp:437-457`).
fn track_layer_enter_leave(ctx: &CompatCtx, new_layer: Option<u32>) {
    let previous = {
        // SAFETY: a `CompatCtx` is only created for a bound, live engine.
        let services = unsafe { ctx.services() };
        services.state::<InputDispatchState>().last_mouse_layer
    };
    if previous == new_layer {
        return;
    }
    if let Some(old) = previous
        && scene(ctx).is_some_and(|scene| scene.layer(old).is_some())
    {
        deliver_checked(ctx, "onMouseLeave", &[], Target::Layer(old));
    }
    if let Some(new) = new_layer {
        deliver_checked(ctx, "onMouseEnter", &[], Target::Layer(new));
    }
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    services.state::<InputDispatchState>().last_mouse_layer = new_layer;
}

/// Sum of the rect offsets of `layer_id` and its non-primary ancestors.
fn layer_primary_offset(ctx: &CompatCtx, layer_id: u32) -> (i64, i64) {
    let Some(scene) = scene(ctx) else {
        return (0, 0);
    };
    let mut offset = (0i64, 0i64);
    let mut current = scene.layer(layer_id);
    while let Some(layer) = current {
        if layer.parent.is_none() {
            break;
        }
        offset.0 += i64::from(layer.rect.x);
        offset.1 += i64::from(layer.rect.y);
        current = layer.parent.and_then(|parent| scene.layer(parent));
    }
    offset
}

/// Convert a window-space point into `layer_id`'s coordinate space.
///
/// Mirrors the reference `FromPrimaryCoordinates`: each layer's rect offset
/// is subtracted while walking up the parent chain
/// (`reference/cpp/core/visual/LayerIntf.cpp:2282-2297`).
fn to_layer_local(ctx: &CompatCtx, layer_id: u32, x: f64, y: f64) -> (f64, f64) {
    let (offset_x, offset_y) = layer_primary_offset(ctx, layer_id);
    (x - offset_x as f64, y - offset_y as f64)
}

/// Integer form of [`to_layer_local`], matching the reference's integer
/// coordinate subtraction exactly.
fn to_layer_local_int(ctx: &CompatCtx, layer_id: u32, x: i64, y: i64) -> (i64, i64) {
    let (offset_x, offset_y) = layer_primary_offset(ctx, layer_id);
    (x - offset_x, y - offset_y)
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

/// Deliver a mapped event and report a missing or throwing handler.
fn deliver_checked(ctx: &CompatCtx, name: &str, args: &[EventValue], target: Target) {
    if let Err(error) = deliver(ctx, name, args, target) {
        if is_missing_member(&error) {
            log::debug!("event adapter: {name} has no handler ({error})");
        } else {
            log::warn!("event adapter: {name} dispatch failed ({error})");
        }
    }
}

/// Call `name` on the target's registered script object with `args` as
/// positional arguments.
fn deliver(ctx: &CompatCtx, name: &str, args: &[EventValue], target: Target) -> Result<(), String> {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let host = unsafe { ctx.engine() };
    let (kind, id) = match target {
        Target::Window(id) => (ObjectKind::Window, id),
        Target::Layer(id) => (ObjectKind::Layer, id),
    };
    let Some(object) = duplicate_object(ctx, host, kind, id) else {
        return Err(format!("event target {id} has no script object"));
    };
    let values: Vec<TjsValue> = args.iter().map(event_value_to_tjs).collect();
    host.call_member(object.raw_id(), name, &values).map(|_| ())
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

/// The reference wheel delta for one scroll notch.
///
/// The Cocos window layer posts `±120` per notch
/// (`reference/cpp/core/environ/impl/TVPWindow.cpp:109-115`); the scripts read
/// the delta in those units.
const WHEEL_DELTA: f32 = 120.0;

/// Convert a platform scroll amount into the reference wheel-delta units.
fn wheel_delta(value: f32) -> i64 {
    coordinate(value * WHEEL_DELTA)
}

/// The shift mask the reference `onKeyUp` handler receives from a posted
/// event: `TVP_TShiftState_From_uint32(shift)` — the TVP `ssShift`/`ssCtrl`/
/// `ssAlt` bits mapped to the Windows `MK_SHIFT`/`MK_CONTROL`/`MK_ALT` bits —
/// OR-ed with the held mouse buttons (`reference/cpp/core/visual/impl/WindowImpl.cpp:53-67`,
/// `reference/cpp/core/environ/impl/TVPWindow.h:437-441`).
pub(crate) fn key_up_shift(ctx: &CompatCtx, shift: i64) -> i64 {
    const MK_LBUTTON: i64 = 0x0001;
    const MK_RBUTTON: i64 = 0x0002;
    const MK_SHIFT: i64 = 0x0004;
    const MK_CONTROL: i64 = 0x0008;
    const MK_MBUTTON: i64 = 0x0010;
    const MK_ALT: i64 = 0x0020;

    let shift = shift as u32;
    let mut result = 0i64;
    if shift & TVP_SS_SHIFT != 0 {
        result |= MK_SHIFT;
    }
    if shift & TVP_SS_CTRL != 0 {
        result |= MK_CONTROL;
    }
    if shift & TVP_SS_ALT != 0 {
        result |= MK_ALT;
    }
    if let Some(input) = input(ctx) {
        if input.is_button_down(MouseButton::Left) {
            result |= MK_LBUTTON;
        }
        if input.is_button_down(MouseButton::Right) {
            result |= MK_RBUTTON;
        }
        if input.is_button_down(MouseButton::Middle) {
            result |= MK_MBUTTON;
        }
    }
    result
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
