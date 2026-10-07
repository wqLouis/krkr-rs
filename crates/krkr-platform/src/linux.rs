//! Linux/desktop platform backend over winit 0.30.
//!
//! This module is the only place that names winit types. It normalizes the
//! native event loop into [`PlatformEvent`]/[`EventHandler`] and owns the
//! window, the frame pacer and the dt computation.

use std::time::Instant;

use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{
    ElementState, Ime, MouseButton as WinitMouseButton, MouseScrollDelta,
    TouchPhase as WinitTouchPhase, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{NativeKeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use crate::clock::Ticker;
use crate::event::PlatformEvent;
use crate::input::{InputEvent, MouseButton};
use crate::{EventHandler, Flow, PlatformConfig, PlatformError};

/// The window created by [`run`].
///
/// Higher layers use it to obtain the raw window/display handles the render
/// backend binds to, to query the surface size, and to request a redraw.
/// Platform details stay behind this type; the winit `Window` never leaks.
pub struct PlatformWindow {
    window: Window,
}

impl PlatformWindow {
    /// Returns the raw window handle the render backend binds to.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::Handle`] if the backend does not currently
    /// expose a handle. Callers must propagate this (D11) rather than
    /// substituting a synthetic handle.
    pub fn raw_window_handle(&self) -> Result<RawWindowHandle, PlatformError> {
        self.window
            .window_handle()
            .map(|handle| handle.as_raw())
            .map_err(|error| PlatformError::Handle(error.to_string()))
    }

    /// Returns the raw display handle (Wayland/X11/XCB connection).
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::Handle`] if the backend does not currently
    /// expose a display handle. A Vulkan surface needs it, so this is fatal.
    pub fn raw_display_handle(&self) -> Result<RawDisplayHandle, PlatformError> {
        self.window
            .display_handle()
            .map(|handle| handle.as_raw())
            .map_err(|error| PlatformError::Handle(error.to_string()))
    }

    /// Returns the surface size in physical pixels (at least 1×1).
    pub fn size(&self) -> (u32, u32) {
        let size = self.window.inner_size();
        (size.width.max(1), size.height.max(1))
    }

    /// Returns the current display scale factor (DPI scale).
    ///
    /// Input coordinates and sizes are physical, so this is only needed by a
    /// higher layer that wants to convert to logical units itself.
    pub fn scale_factor(&self) -> f64 {
        self.window.scale_factor()
    }

    /// Requests that the window redraw its contents.
    pub fn request_redraw(&self) {
        self.window.request_redraw();
    }
}

/// Creates the event loop and runs `handler` until it asks to exit.
///
/// # Errors
///
/// Returns [`PlatformError::EventLoop`] if the loop cannot be created or
/// fails, and [`PlatformError::Window`] if the window could not be created
/// during `resumed`.
pub fn run<H: EventHandler>(config: PlatformConfig, handler: H) -> Result<(), PlatformError> {
    let event_loop =
        EventLoop::new().map_err(|error| PlatformError::EventLoop(error.to_string()))?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = AppLoop::new(config, handler);
    let result = event_loop
        .run_app(&mut app)
        .map_err(|error| PlatformError::EventLoop(error.to_string()));
    match app.failure.take() {
        Some(error) => Err(error),
        None => result,
    }
}

/// The winit `ApplicationHandler` that drives the higher-layer `EventHandler`.
///
/// # Field order (teardown)
///
/// `handler` is declared **before** `window`: Rust drops fields in declaration
/// order, so the higher layer (which owns the GPU surface/swapchain) is
/// dropped while the window's display connection is still alive. Reordering
/// these two fields would tear down the display connection first and is a
/// use-after-free risk for a Vulkan surface — keep `handler` above `window`.
struct AppLoop<H: EventHandler> {
    config: PlatformConfig,
    handler: H,
    window: Option<PlatformWindow>,
    ticker: Ticker,
    last_frame: Instant,
    /// First fatal error seen inside the loop, returned by [`run`].
    failure: Option<PlatformError>,
}

impl<H: EventHandler> AppLoop<H> {
    fn new(config: PlatformConfig, handler: H) -> Self {
        let now = Instant::now();
        Self {
            ticker: Ticker::new(config.frame_interval, now),
            config,
            handler,
            window: None,
            last_frame: now,
            failure: None,
        }
    }

    /// Records a fatal error and exits the loop.
    fn fail(&mut self, event_loop: &ActiveEventLoop, error: PlatformError) {
        log::error!("platform: {error}");
        self.failure = Some(error);
        event_loop.exit();
    }
}

impl<H: EventHandler> ApplicationHandler for AppLoop<H> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // On a re-resume (Android suspend/resume, desktop backends that emit
        // it again) the window already exists, but the handler must still be
        // notified so a higher layer can recreate its surface. Only the
        // window creation itself is guarded.
        if self.window.is_none() {
            let attributes = Window::default_attributes()
                .with_title(self.config.title.as_str())
                .with_inner_size(LogicalSize::new(
                    f64::from(self.config.size.0),
                    f64::from(self.config.size.1),
                ));
            let window = match event_loop.create_window(attributes) {
                Ok(window) => window,
                Err(error) => {
                    self.fail(event_loop, PlatformError::Window(error.to_string()));
                    return;
                }
            };

            let now = Instant::now();
            self.last_frame = now;
            self.ticker = Ticker::new(self.config.frame_interval, now);
            self.window = Some(PlatformWindow { window });
        }

        let window = self.window.as_ref().expect("window exists after creation");
        if self.handler.resumed(window) == Flow::Exit {
            event_loop.exit();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        let flow = match event {
            WindowEvent::CloseRequested => {
                // A close request is authoritative: the loop always stops, so
                // a higher layer can only observe it, not veto it.
                let _ = self.handler.event(PlatformEvent::CloseRequested);
                event_loop.exit();
                return;
            }
            WindowEvent::Resized(size) => self.handler.event(PlatformEvent::Resized(
                size.width.max(1),
                size.height.max(1),
            )),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => self
                .handler
                .event(PlatformEvent::ScaleFactorChanged(scale_factor)),
            WindowEvent::Focused(focused) => self.handler.event(PlatformEvent::Focused(focused)),
            WindowEvent::RedrawRequested => {
                let now = Instant::now();
                let dt_ms = now.saturating_duration_since(self.last_frame).as_secs_f32() * 1000.0;
                self.last_frame = now;
                self.handler.frame(dt_ms)
            }
            other => match map_input(other) {
                Some(input) => self.handler.event(PlatformEvent::Input(input)),
                None => Flow::Continue,
            },
        };

        if flow == Flow::Exit {
            event_loop.exit();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if self.ticker.poll(now)
            && let Some(window) = &self.window
        {
            window.request_redraw();
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.ticker.deadline()));
        if self.handler.about_to_wait() == Flow::Exit {
            event_loop.exit();
        }
    }
}

/// Translates an owned winit window event into a raw [`InputEvent`].
///
/// Taking the event by value avoids cloning the IME payload. Returns `None`
/// for events that carry no raw input (resize, redraw, …), which are handled
/// as [`PlatformEvent`]s instead. Coordinates are copied verbatim as physical
/// pixels (see [`crate::input`]).
fn map_input(event: WindowEvent) -> Option<InputEvent> {
    match event {
        WindowEvent::CursorMoved { position, .. } => Some(InputEvent::PointerMoved {
            x: position.x as f32,
            y: position.y as f32,
        }),
        WindowEvent::CursorLeft { .. } => Some(InputEvent::PointerLeft),
        WindowEvent::MouseInput { state, button, .. } => Some(InputEvent::MouseButton {
            button: map_button(button),
            pressed: state == ElementState::Pressed,
        }),
        WindowEvent::MouseWheel { delta, .. } => Some(InputEvent::Wheel {
            delta: match delta {
                MouseScrollDelta::LineDelta(_, y) => y,
                MouseScrollDelta::PixelDelta(position) => position.y as f32,
            },
        }),
        WindowEvent::KeyboardInput { event, .. } => Some(InputEvent::Key {
            code: key_code(event.physical_key),
            pressed: event.state == ElementState::Pressed,
        }),
        WindowEvent::Ime(Ime::Commit(text)) => Some(InputEvent::Text(text)),
        WindowEvent::Touch(touch) => Some(InputEvent::Touch {
            id: touch.id,
            phase: map_touch_phase(touch.phase),
            x: touch.location.x as f32,
            y: touch.location.y as f32,
        }),
        _ => None,
    }
}

/// Maps a winit mouse button to the platform-agnostic value.
fn map_button(button: WinitMouseButton) -> MouseButton {
    match button {
        WinitMouseButton::Left => MouseButton::Left,
        WinitMouseButton::Right => MouseButton::Right,
        WinitMouseButton::Middle => MouseButton::Middle,
        WinitMouseButton::Back => MouseButton::Back,
        WinitMouseButton::Forward => MouseButton::Forward,
        WinitMouseButton::Other(code) => MouseButton::Other(code),
    }
}

/// Maps a winit touch phase to the `krkr-api` value.
fn map_touch_phase(phase: WinitTouchPhase) -> krkr_api::host::TouchPhase {
    match phase {
        WinitTouchPhase::Started => krkr_api::host::TouchPhase::Started,
        WinitTouchPhase::Moved => krkr_api::host::TouchPhase::Moved,
        WinitTouchPhase::Ended => krkr_api::host::TouchPhase::Ended,
        WinitTouchPhase::Cancelled => krkr_api::host::TouchPhase::Cancelled,
    }
}

/// Returns the opaque platform key identifier for a physical key.
///
/// Known keys use the winit `KeyCode` discriminant; unidentified keys use the
/// native code with [`NATIVE_KEY_FLAG`] set, so the two ranges cannot collide.
///
/// The value is only meaningful inside the running process and within a build
/// of winit (the `KeyCode` discriminants are not a stable ABI). Mapping it to
/// a TJS key code is L3's job; if L3 needs a cross-build-stable id, an explicit
/// L1 key enum should replace this cast.
fn key_code(physical: PhysicalKey) -> u32 {
    match physical {
        PhysicalKey::Code(code) => code as u32,
        PhysicalKey::Unidentified(native) => native_key_code(native),
    }
}

/// High bit marking native (platform-specific) key codes.
///
/// Keeps them disjoint from the winit `KeyCode` discriminants returned by
/// [`key_code`] for known keys.
const NATIVE_KEY_FLAG: u32 = 0x8000_0000;

/// Converts a native key code to the opaque identifier.
fn native_key_code(native: NativeKeyCode) -> u32 {
    let raw = match native {
        NativeKeyCode::Android(code) => code,
        NativeKeyCode::MacOS(code) => u32::from(code),
        NativeKeyCode::Windows(code) => u32::from(code),
        NativeKeyCode::Xkb(code) => code,
        _ => {
            log::debug!("platform: unrecognized native key code");
            u32::MAX
        }
    };
    raw | NATIVE_KEY_FLAG
}
