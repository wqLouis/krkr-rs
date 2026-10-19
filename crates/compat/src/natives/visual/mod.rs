//! Visual native classes: `Window`, `Layer`, `Bitmap`, `Font`, `Timer`.
//!
//! Behavioral reference: `main`'s `crates/tvp-visual/src/natives/*` (window.rs,
//! layer.rs, bitmap.rs, font.rs, timer.rs) and the old `scene.rs`. This is a
//! reimplementation on the layered API: every class composes the L2
//! `engine_api::Scene` attached through [`CompatServices::scene`] and keeps no
//! engine state of its own. The old code hid that state in process globals;
//! here the per-family **dispatch** state (the id → script-object registries
//! and the `Timer` callback bindings) lives in [`CompatServices::state`]
//! (doc 08 §3.1); the generic input state and the timer schedule are L2 state
//! (`engine_api::InputState`, `engine_api::TimerState`) attached to
//! the services, and the per-object state lives in the native instance
//! payload.
//!
//! # Architecture
//!
//! The native class is constructed by `new Window()` / `new Layer(...)` /
//! ...; the **class-name method** (`Window`, `Layer`, ...) is the constructor
//! hook and returns the new scene id (the reference wrappers use it). The
//! instance payload holds the scene id; all persistent state is read and
//! written through the L2 [`Scene`] API, so the model is never duplicated.
//!
//! Cross-object properties (`Window.primaryLayer`, `Layer.parent`,
//! `Layer.window`, `Layer.children`, ...) return real TJS objects. The mapping
//! scene id → retained script object is the [`ObjectRegistry`] in
//! [`VisualState`]; each class registers its `objthis` at construction and
//! removes it on destroy.
//!
//! # Implemented
//!
//! * `Window`: constructor, `close`, `bringToFront`, `update`, `add`,
//!   `remove`, `setSize`, `setInnerSize`, `setPos`, `setMinSize`,
//!   `setMaxSize`, `setZoom`, `showModal`, `setMaskRegion`,
//!   `removeMaskRegion`, `hideMouseCursor`,
//!   `postInputEvent`, `getTouchPoint`, `getTouchVelocity`; properties `id`,
//!   `visible`, `caption`, `width`,
//!   `height`, `innerWidth`, `innerHeight`, `minWidth`, `minHeight`,
//!   `maxWidth`, `maxHeight`, `left`, `top`, `focusable`, `zoomNumer`,
//!   `zoomDenom`, `fullScreen`, `mainWindow`, `focusedLayer`, `primaryLayer`,
//!   `touchPointCount`, `waitVSync`, `enableTouch`, `hintDelay`,
//!   `mouseCursorState`, `imeMode`, `useMouseKey`, `trapKey`.
//! * `Layer`: constructor, `setPos`, `setSize`, `setSizeToImageSize`,
//!   `setImagePos`, `setImageSize`, `assignImages`,
//!   `copyFromBitmapToMainImage`, `independMainImage`, `bringToFront`,
//!   `getLayerAt`, `focus`, `update`, `setClip`,
//!   `getProvincePixel`, `setProvincePixel`, `fillRect`; the full property
//!   set used by KAG (`visible`, `opacity`, `width`, `height`, `left`, `top`,
//!   `type`, `hasImage`, the buffer/pitch properties, `absolute`/`order`,
//!   `cursorX`/`cursorY`, hit/attention/clip/focus properties, `window`,
//!   `parent`, `children`, `font`, ...).
//! * `Bitmap`: constructor, `getPixel`, `setPixel`, `getMaskPixel`,
//!   `setMaskPixel`, `independ`, `setSize`, `copyFrom`; properties `width`,
//!   `height`, `buffer`, `bufferForWrite`, `bufferPitch`, `loading`.
//! * `Font`: constructor; properties `face`, `height`, `color`, `bold`,
//!   `italic`, `strikeout`, `underline`, `angle`, `faceIsFileName`.
//! * `Timer`: constructor, `onTimer`; properties `interval`, `enabled`,
//!   `capacity`, `mode`; [`timer_posts_poll`] delivers the queued `onTimer`
//!   events through the object's `onTimer` member (so a script subclass
//!   override runs), and [`timer_poll`] advances the attached registry first.
//!
//! # Deliberately deferred
//!
//! These members are **not registered** (calling them raises a TJS "member
//! not found", never a silent stub):
//!
//! * `Layer.colorize` / `Layer.noise` and the remaining `operate*`/`blend*`
//!   pile variants — L2 `engine_api::layer_ops` already implements the math
//!   (`colorize`, `noise`, `operate_rect`, ...); wiring them here is a
//!   mechanical follow-up rather than a missing primitive.
//!
//! # Timer polling
//!
//! The L2 `engine_api::TimerState` registry is advanced by the L2 frame
//! clock; its callbacks only queue `onTimer` events. [`timer_posts_poll`]
//! delivers those queued events and is the first step of the frame body
//! (`compat::frame::run_frame_body`). [`timer_poll`] stays for a host or test
//! that drives the attached registry itself: it advances the registry at the
//! registry's own injected `now_ms` and then drains. Both are `pub` free
//! functions (like `system::continuous_handler_poll`) so the host can wire them
//! without the natives exposing anything else; the dispatch itself stays in
//! L3.
#![allow(unsafe_code)]

use crate::{CompatCtx, CompatError};

mod bitmap;
mod common;
pub(crate) use common::{ObjectKind, duplicate_object};
pub(crate) use layer::{clear_layer_focus, focus_layer, focus_layer_in_input_domain, from_native};
mod draw_device;
mod font;
mod layer;
mod timer;
mod window;

pub use timer::{timer_poll, timer_posts_poll};

/// The number of live `Timer` bindings in the dispatch state.
///
/// A host diagnostic: a binding is removed by the timer's destroy callback, so
/// a count that grows across create/drop cycles is a retention leak.
pub(crate) fn timer_count(services: &crate::CompatServices) -> usize {
    timer::binding_count(services)
}
// The tree-owner / frame-body hand-offs: the L4 frame body drains the host
// requests and advances transitions from its `frame_tail` step.
pub use layer::{advance_transitions, paint_poll, take_capture_releases};
pub use window::{take_bring_to_front_requests, take_update_requests};

/// Register the visual classes.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    window::register_window_class(ctx)?;
    // The draw-device classes are attached to the `Window` class object, so
    // `Window` must exist first.
    draw_device::register_draw_devices(ctx)?;
    layer::register_layer_class(ctx)?;
    bitmap::register_bitmap_class(ctx)?;
    font::register_font_class(ctx)?;
    timer::register_timer_class(ctx)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::timer_poll;
    use crate::CompatHost;
    use crate::engine_api::Scene;
    use crate::engine_api::TimerState;
    use tjs::{Tjs2Engine, TjsValue};

    /// A bound VM + host + attached scene and timer registry.
    ///
    /// Fields drop in declaration order: the host first (releasing every
    /// retained script object), then the engine, the scene and the L2 timer
    /// registry.
    struct Env {
        host: CompatHost,
        engine: Box<Tjs2Engine>,
        /// Kept alive for the host's raw pointer; never read directly.
        _scene: Box<Scene>,
        /// The attached registry; tests move its injected time directly so a
        /// poll never depends on the wall clock.
        timer: RefCell<Box<TimerState>>,
    }

    impl Env {
        fn new() -> Env {
            let engine = Box::new(Tjs2Engine::new().expect("create engine"));
            let mut scene = Box::new(Scene::default());
            let mut timer = Box::new(TimerState::new());
            let mut host = CompatHost::new();
            // SAFETY: the boxed scene and timer registry outlive the host.
            unsafe {
                host.services_mut()
                    .attach_scene(scene.as_mut() as *mut Scene);
                host.services_mut()
                    .attach_timer(timer.as_mut() as *mut TimerState);
            }
            host.install(&engine).expect("install compat surface");
            Env {
                host,
                engine,
                _scene: scene,
                timer: RefCell::new(timer),
            }
        }

        /// Move the attached registry's injected time forward by `ms`.
        fn advance(&self, ms: u64) {
            let mut timer = self.timer.borrow_mut();
            timer.now_ms = timer.now_ms.saturating_add(ms);
        }

        fn eval_int(&self, expression: &str) -> i64 {
            match self.engine.eval(expression, "timer-test") {
                Ok(TjsValue::Integer(value)) => value,
                other => panic!("{expression:?} returned {other:?}, expected an integer"),
            }
        }
    }

    #[test]
    fn timer_poll_fires_the_callback() {
        let _lock = crate::native_util::test_vm_lock();
        let env = Env::new();
        env.engine
            .exec_script(
                "var hits = 0; \
                 var t = new Timer(function() { hits++; }, ''); \
                 t.interval = 1; \
                 t.enabled = true;",
                "timer-test",
            )
            .expect("define timer");

        // `timer_poll` advances the attached registry to its injected
        // `now_ms`; a virtual frame step is enough for the 1 ms timer.
        for _ in 0..200 {
            env.advance(16);
            let mut ctx = env.host.ctx().expect("bound host");
            timer_poll(&mut ctx).expect("timer poll");
            if env.eval_int("hits") >= 1 {
                break;
            }
        }
        assert!(env.eval_int("hits") >= 1, "the timer callback never ran");
    }
}
