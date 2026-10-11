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
use winit::keyboard::{KeyCode as WinitKeyCode, NativeKeyCode, PhysicalKey as WinitPhysicalKey};
use winit::window::{Window, WindowId};

use crate::clock::Ticker;
use crate::event::PlatformEvent;
use crate::input::{InputEvent, MouseButton, PhysicalKey};
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
    /// expose a handle. Callers must propagate this rather than
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
            code: physical_key(event.physical_key),
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

/// Maps a winit touch phase to the `api` value.
fn map_touch_phase(phase: WinitTouchPhase) -> api::host::TouchPhase {
    match phase {
        WinitTouchPhase::Started => api::host::TouchPhase::Started,
        WinitTouchPhase::Moved => api::host::TouchPhase::Moved,
        WinitTouchPhase::Ended => api::host::TouchPhase::Ended,
        WinitTouchPhase::Cancelled => api::host::TouchPhase::Cancelled,
    }
}

/// Maps a winit physical key to the platform-neutral [`PhysicalKey`].
///
/// A key winit identifies maps to its named value; an unidentified key, or a
/// winit code with no named value, maps to [`PhysicalKey::Native`] carrying the
/// raw code.
fn physical_key(physical: WinitPhysicalKey) -> PhysicalKey {
    match physical {
        WinitPhysicalKey::Code(code) => map_winit_key(code),
        WinitPhysicalKey::Unidentified(native) => PhysicalKey::Native(native_key_code(native)),
    }
}

/// Maps a winit `KeyCode` to the platform-neutral key.
///
/// winit's enum is non-exhaustive; a code with no named value (a future key, or
/// one with no KiriKiri key code) keeps its winit discriminant as a
/// [`PhysicalKey::Native`] identity.
fn map_winit_key(code: WinitKeyCode) -> PhysicalKey {
    match code {
        WinitKeyCode::Backquote => PhysicalKey::Backquote,
        WinitKeyCode::Backslash => PhysicalKey::Backslash,
        WinitKeyCode::BracketLeft => PhysicalKey::BracketLeft,
        WinitKeyCode::BracketRight => PhysicalKey::BracketRight,
        WinitKeyCode::Comma => PhysicalKey::Comma,
        WinitKeyCode::Digit0 => PhysicalKey::Digit0,
        WinitKeyCode::Digit1 => PhysicalKey::Digit1,
        WinitKeyCode::Digit2 => PhysicalKey::Digit2,
        WinitKeyCode::Digit3 => PhysicalKey::Digit3,
        WinitKeyCode::Digit4 => PhysicalKey::Digit4,
        WinitKeyCode::Digit5 => PhysicalKey::Digit5,
        WinitKeyCode::Digit6 => PhysicalKey::Digit6,
        WinitKeyCode::Digit7 => PhysicalKey::Digit7,
        WinitKeyCode::Digit8 => PhysicalKey::Digit8,
        WinitKeyCode::Digit9 => PhysicalKey::Digit9,
        WinitKeyCode::Equal => PhysicalKey::Equal,
        WinitKeyCode::IntlBackslash => PhysicalKey::IntlBackslash,
        WinitKeyCode::IntlRo => PhysicalKey::IntlRo,
        WinitKeyCode::IntlYen => PhysicalKey::IntlYen,
        WinitKeyCode::KeyA => PhysicalKey::KeyA,
        WinitKeyCode::KeyB => PhysicalKey::KeyB,
        WinitKeyCode::KeyC => PhysicalKey::KeyC,
        WinitKeyCode::KeyD => PhysicalKey::KeyD,
        WinitKeyCode::KeyE => PhysicalKey::KeyE,
        WinitKeyCode::KeyF => PhysicalKey::KeyF,
        WinitKeyCode::KeyG => PhysicalKey::KeyG,
        WinitKeyCode::KeyH => PhysicalKey::KeyH,
        WinitKeyCode::KeyI => PhysicalKey::KeyI,
        WinitKeyCode::KeyJ => PhysicalKey::KeyJ,
        WinitKeyCode::KeyK => PhysicalKey::KeyK,
        WinitKeyCode::KeyL => PhysicalKey::KeyL,
        WinitKeyCode::KeyM => PhysicalKey::KeyM,
        WinitKeyCode::KeyN => PhysicalKey::KeyN,
        WinitKeyCode::KeyO => PhysicalKey::KeyO,
        WinitKeyCode::KeyP => PhysicalKey::KeyP,
        WinitKeyCode::KeyQ => PhysicalKey::KeyQ,
        WinitKeyCode::KeyR => PhysicalKey::KeyR,
        WinitKeyCode::KeyS => PhysicalKey::KeyS,
        WinitKeyCode::KeyT => PhysicalKey::KeyT,
        WinitKeyCode::KeyU => PhysicalKey::KeyU,
        WinitKeyCode::KeyV => PhysicalKey::KeyV,
        WinitKeyCode::KeyW => PhysicalKey::KeyW,
        WinitKeyCode::KeyX => PhysicalKey::KeyX,
        WinitKeyCode::KeyY => PhysicalKey::KeyY,
        WinitKeyCode::KeyZ => PhysicalKey::KeyZ,
        WinitKeyCode::Minus => PhysicalKey::Minus,
        WinitKeyCode::Period => PhysicalKey::Period,
        WinitKeyCode::Quote => PhysicalKey::Quote,
        WinitKeyCode::Semicolon => PhysicalKey::Semicolon,
        WinitKeyCode::Slash => PhysicalKey::Slash,
        WinitKeyCode::AltLeft => PhysicalKey::AltLeft,
        WinitKeyCode::AltRight => PhysicalKey::AltRight,
        WinitKeyCode::Backspace => PhysicalKey::Backspace,
        WinitKeyCode::CapsLock => PhysicalKey::CapsLock,
        WinitKeyCode::ContextMenu => PhysicalKey::ContextMenu,
        WinitKeyCode::ControlLeft => PhysicalKey::ControlLeft,
        WinitKeyCode::ControlRight => PhysicalKey::ControlRight,
        WinitKeyCode::Enter => PhysicalKey::Enter,
        WinitKeyCode::SuperLeft => PhysicalKey::SuperLeft,
        WinitKeyCode::SuperRight => PhysicalKey::SuperRight,
        WinitKeyCode::ShiftLeft => PhysicalKey::ShiftLeft,
        WinitKeyCode::ShiftRight => PhysicalKey::ShiftRight,
        WinitKeyCode::Space => PhysicalKey::Space,
        WinitKeyCode::Tab => PhysicalKey::Tab,
        WinitKeyCode::Convert => PhysicalKey::Convert,
        WinitKeyCode::KanaMode => PhysicalKey::KanaMode,
        WinitKeyCode::NonConvert => PhysicalKey::NonConvert,
        WinitKeyCode::Delete => PhysicalKey::Delete,
        WinitKeyCode::End => PhysicalKey::End,
        WinitKeyCode::Help => PhysicalKey::Help,
        WinitKeyCode::Home => PhysicalKey::Home,
        WinitKeyCode::Insert => PhysicalKey::Insert,
        WinitKeyCode::PageDown => PhysicalKey::PageDown,
        WinitKeyCode::PageUp => PhysicalKey::PageUp,
        WinitKeyCode::ArrowDown => PhysicalKey::ArrowDown,
        WinitKeyCode::ArrowLeft => PhysicalKey::ArrowLeft,
        WinitKeyCode::ArrowRight => PhysicalKey::ArrowRight,
        WinitKeyCode::ArrowUp => PhysicalKey::ArrowUp,
        WinitKeyCode::NumLock => PhysicalKey::NumLock,
        WinitKeyCode::Numpad0 => PhysicalKey::Numpad0,
        WinitKeyCode::Numpad1 => PhysicalKey::Numpad1,
        WinitKeyCode::Numpad2 => PhysicalKey::Numpad2,
        WinitKeyCode::Numpad3 => PhysicalKey::Numpad3,
        WinitKeyCode::Numpad4 => PhysicalKey::Numpad4,
        WinitKeyCode::Numpad5 => PhysicalKey::Numpad5,
        WinitKeyCode::Numpad6 => PhysicalKey::Numpad6,
        WinitKeyCode::Numpad7 => PhysicalKey::Numpad7,
        WinitKeyCode::Numpad8 => PhysicalKey::Numpad8,
        WinitKeyCode::Numpad9 => PhysicalKey::Numpad9,
        WinitKeyCode::NumpadAdd => PhysicalKey::NumpadAdd,
        WinitKeyCode::NumpadClear => PhysicalKey::Clear,
        WinitKeyCode::NumpadComma => PhysicalKey::NumpadSeparator,
        WinitKeyCode::NumpadDecimal => PhysicalKey::NumpadDecimal,
        WinitKeyCode::NumpadDivide => PhysicalKey::NumpadDivide,
        WinitKeyCode::NumpadEnter => PhysicalKey::NumpadEnter,
        WinitKeyCode::NumpadMultiply | WinitKeyCode::NumpadStar => PhysicalKey::NumpadMultiply,
        WinitKeyCode::NumpadSubtract => PhysicalKey::NumpadSubtract,
        WinitKeyCode::Escape => PhysicalKey::Escape,
        WinitKeyCode::PrintScreen => PhysicalKey::PrintScreen,
        WinitKeyCode::ScrollLock => PhysicalKey::ScrollLock,
        WinitKeyCode::Pause => PhysicalKey::Pause,
        WinitKeyCode::BrowserBack => PhysicalKey::BrowserBack,
        WinitKeyCode::BrowserFavorites => PhysicalKey::BrowserFavorites,
        WinitKeyCode::BrowserForward => PhysicalKey::BrowserForward,
        WinitKeyCode::BrowserHome => PhysicalKey::BrowserHome,
        WinitKeyCode::BrowserRefresh => PhysicalKey::BrowserRefresh,
        WinitKeyCode::BrowserSearch => PhysicalKey::BrowserSearch,
        WinitKeyCode::BrowserStop => PhysicalKey::BrowserStop,
        WinitKeyCode::LaunchApp1 => PhysicalKey::LaunchApp1,
        WinitKeyCode::LaunchApp2 => PhysicalKey::LaunchApp2,
        WinitKeyCode::LaunchMail => PhysicalKey::LaunchMail,
        WinitKeyCode::MediaPlayPause => PhysicalKey::MediaPlayPause,
        WinitKeyCode::MediaSelect => PhysicalKey::MediaSelect,
        WinitKeyCode::MediaStop => PhysicalKey::MediaStop,
        WinitKeyCode::MediaTrackNext => PhysicalKey::MediaTrackNext,
        WinitKeyCode::MediaTrackPrevious => PhysicalKey::MediaTrackPrevious,
        WinitKeyCode::Sleep => PhysicalKey::Sleep,
        WinitKeyCode::AudioVolumeDown => PhysicalKey::VolumeDown,
        WinitKeyCode::AudioVolumeMute => PhysicalKey::VolumeMute,
        WinitKeyCode::AudioVolumeUp => PhysicalKey::VolumeUp,
        WinitKeyCode::Select => PhysicalKey::Select,
        WinitKeyCode::F1 => PhysicalKey::F1,
        WinitKeyCode::F2 => PhysicalKey::F2,
        WinitKeyCode::F3 => PhysicalKey::F3,
        WinitKeyCode::F4 => PhysicalKey::F4,
        WinitKeyCode::F5 => PhysicalKey::F5,
        WinitKeyCode::F6 => PhysicalKey::F6,
        WinitKeyCode::F7 => PhysicalKey::F7,
        WinitKeyCode::F8 => PhysicalKey::F8,
        WinitKeyCode::F9 => PhysicalKey::F9,
        WinitKeyCode::F10 => PhysicalKey::F10,
        WinitKeyCode::F11 => PhysicalKey::F11,
        WinitKeyCode::F12 => PhysicalKey::F12,
        WinitKeyCode::F13 => PhysicalKey::F13,
        WinitKeyCode::F14 => PhysicalKey::F14,
        WinitKeyCode::F15 => PhysicalKey::F15,
        WinitKeyCode::F16 => PhysicalKey::F16,
        WinitKeyCode::F17 => PhysicalKey::F17,
        WinitKeyCode::F18 => PhysicalKey::F18,
        WinitKeyCode::F19 => PhysicalKey::F19,
        WinitKeyCode::F20 => PhysicalKey::F20,
        WinitKeyCode::F21 => PhysicalKey::F21,
        WinitKeyCode::F22 => PhysicalKey::F22,
        WinitKeyCode::F23 => PhysicalKey::F23,
        WinitKeyCode::F24 => PhysicalKey::F24,
        other => PhysicalKey::Native(other as u32),
    }
}

/// Converts a native key code to the raw identity.
fn native_key_code(native: NativeKeyCode) -> u32 {
    match native {
        NativeKeyCode::Android(code) => code,
        NativeKeyCode::MacOS(code) => u32::from(code),
        NativeKeyCode::Windows(code) => u32::from(code),
        NativeKeyCode::Xkb(code) => code,
        _ => {
            log::debug!("platform: unrecognized native key code");
            u32::MAX
        }
    }
}
/// Audio output over cpal (the `platform-linux` backend).
///
/// The public object/state API lives in [`crate::audio`]; this module provides
/// the real backend. The stream's data callback drains a software
/// [`AudioQueue`] that [`write`] fills, so the object stays stateless and the
/// queue lives in the caller-owned state value. The stream is **not** started
/// until the queue reaches the pre-fill target, and an underrun holds the last
/// sample while fading it to silence, then fades the resumed audio back in,
/// rather than stepping to a hard zero (both policies live in
/// [`crate::audio_queue`]).
pub(crate) mod audio {
    use std::sync::{Arc, Mutex, MutexGuard};

    use api::error::AudioError;
    use api::media::AudioSpec;
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::{FromSample, SampleFormat, SampleRate, SizedSample, StreamConfig};

    use crate::PlatformError;
    use crate::audio_queue::AudioQueue;

    /// Immutable stream description chosen at open time.
    ///
    /// The device format is baked into the [`AudioQueue`] and the cpal
    /// stream; the type exists so the real backend has a stable object type.
    pub(crate) struct Backend;

    /// The OS output stream plus the software queue its callback drains.
    pub(crate) struct StreamState {
        stream: Option<cpal::Stream>,
        queue: Arc<Mutex<AudioQueue>>,
        /// Set once the queue reached pre-fill and the stream was started.
        playing: bool,
    }

    /// The output format used only when the device reports no usable
    /// configuration of its own.
    ///
    /// This is a last resort, not the normal path: [`default_spec`] prefers the
    /// device's own default (or the best 1–2 channel format it supports). 44.1
    /// kHz stereo is the KiriKiri/reference mix rate and the format every
    /// output device we target can render, so it is the safest thing to try
    /// when the device tells us nothing.
    const FALLBACK_SPEC: AudioSpec = AudioSpec {
        sample_rate: 44_100,
        channels: 2,
        total_frames: None,
    };

    /// Opens the default output device for `spec` but does not start it yet.
    ///
    /// The stream is started by [`write`] once the queue holds the pre-fill
    /// target, so the device never drains an empty queue.
    pub(crate) fn open(spec: AudioSpec) -> Result<(Backend, StreamState), PlatformError> {
        let host = open_host()?;
        let device = host
            .default_output_device()
            .ok_or_else(|| AudioError::DeviceUnavailable {
                message: "no default output device".to_owned(),
            })?;
        let supported = choose_config(&device, spec)?;
        let config = supported.config();
        let queue = Arc::new(Mutex::new(AudioQueue::new(
            config.sample_rate.0,
            config.channels,
        )));
        let stream = build_stream(
            &device,
            &config,
            supported.sample_format(),
            Arc::clone(&queue),
        )?;
        Ok((
            Backend,
            StreamState {
                stream: Some(stream),
                queue,
                playing: false,
            },
        ))
    }

    /// Queues `pcm` for playback, keeping at most two seconds buffered, and
    /// starts the stream once the pre-fill target is reached.
    pub(crate) fn write(
        _dev: &Backend,
        state: &mut StreamState,
        pcm: &[f32],
    ) -> Result<(), PlatformError> {
        if state.stream.is_none() {
            return Err(AudioError::NotRunning.into());
        }
        let prefilled = {
            let mut queue = lock_queue(&state.queue);
            queue.push(pcm)
        };
        if prefilled && !state.playing {
            if let Some(stream) = &state.stream {
                stream.play().map_err(|error| AudioError::Stream {
                    message: error.to_string(),
                })?;
            }
            state.playing = true;
        }
        Ok(())
    }

    /// Pauses the stream and releases the OS resources (idempotent).
    pub(crate) fn stop(_dev: &Backend, state: &mut StreamState) {
        if let Some(stream) = state.stream.take() {
            let _ = stream.pause();
        }
        state.playing = false;
    }

    /// Whether the stream is playing (i.e. has reached pre-fill and started).
    pub(crate) fn is_running(state: &StreamState) -> bool {
        state.playing
    }

    /// Returns the first available cpal host without panicking.
    fn open_host() -> Result<cpal::Host, PlatformError> {
        let id = cpal::available_hosts().into_iter().next().ok_or_else(|| {
            AudioError::DeviceUnavailable {
                message: "no cpal audio host is available".to_owned(),
            }
        })?;
        cpal::host_from_id(id).map_err(|error| {
            AudioError::DeviceUnavailable {
                message: format!("audio host {id:?} is unavailable: {error}"),
            }
            .into()
        })
    }

    /// Picks a supported config whose channels and sample rate match `spec`.
    fn choose_config(
        device: &cpal::Device,
        spec: AudioSpec,
    ) -> Result<cpal::SupportedStreamConfig, PlatformError> {
        let wanted = SampleRate(spec.sample_rate);
        let configs = device
            .supported_output_configs()
            .map_err(|error| AudioError::Open {
                message: error.to_string(),
            })?;
        for range in configs {
            if range.channels() == spec.channels
                && let Some(config) = range.try_with_sample_rate(wanted)
            {
                return Ok(config);
            }
        }
        Err(AudioError::Unsupported {
            message: format!(
                "no {} Hz / {} channel output format on the default device",
                spec.sample_rate, spec.channels
            ),
        }
        .into())
    }

    /// Returns the default output device's preferred format.
    ///
    /// Prefers the device's own default config when it has 1–2 channels. If the
    /// device will not report a default (or reports a multichannel layout the
    /// mixer cannot consume), it picks the best supported 1–2 channel format;
    /// only a device that reports nothing usable falls back to
    /// [`FALLBACK_SPEC`].
    pub(crate) fn default_spec() -> Result<AudioSpec, PlatformError> {
        let host = open_host()?;
        let device = host
            .default_output_device()
            .ok_or_else(|| AudioError::DeviceUnavailable {
                message: "no default output device".to_owned(),
            })?;
        match device.default_output_config() {
            Ok(config) if (1..=2).contains(&config.channels()) => {
                return Ok(AudioSpec {
                    sample_rate: config.sample_rate().0,
                    channels: config.channels(),
                    total_frames: None,
                });
            }
            Ok(config) => log::warn!(
                "audio: default output format is {} channels; the mixer is stereo-only, \
                 selecting a supported 1-2 channel format",
                config.channels()
            ),
            Err(error) => log::warn!(
                "audio: the output device reported no default format ({error}); selecting a \
                 supported 1-2 channel format"
            ),
        }
        let chosen = device.supported_output_configs().ok().and_then(|ranges| {
            choose_output_spec(ranges.map(|range| {
                (
                    range.channels(),
                    range.min_sample_rate().0,
                    range.max_sample_rate().0,
                )
            }))
        });
        match chosen {
            Some(spec) => Ok(spec),
            None => {
                log::warn!(
                    "audio: the output device reports no usable 1-2 channel format; using \
                     the documented fallback {FALLBACK_SPEC:?}"
                );
                Ok(FALLBACK_SPEC)
            }
        }
    }

    /// Builds the stream, dispatching on the device's sample format.
    fn build_stream(
        device: &cpal::Device,
        config: &StreamConfig,
        format: SampleFormat,
        queue: Arc<Mutex<AudioQueue>>,
    ) -> Result<cpal::Stream, PlatformError> {
        match format {
            SampleFormat::F32 => build_typed::<f32>(device, config, queue),
            SampleFormat::I16 => build_typed::<i16>(device, config, queue),
            SampleFormat::U16 => build_typed::<u16>(device, config, queue),
            SampleFormat::I32 => build_typed::<i32>(device, config, queue),
            SampleFormat::U32 => build_typed::<u32>(device, config, queue),
            SampleFormat::I8 => build_typed::<i8>(device, config, queue),
            SampleFormat::U8 => build_typed::<u8>(device, config, queue),
            SampleFormat::I64 => build_typed::<i64>(device, config, queue),
            SampleFormat::U64 => build_typed::<u64>(device, config, queue),
            SampleFormat::F64 => build_typed::<f64>(device, config, queue),
            other => Err(AudioError::Unsupported {
                message: format!("unsupported sample format: {other}"),
            }
            .into()),
        }
    }

    /// Builds a typed stream whose callback converts queued `f32` to `T`.
    fn build_typed<T>(
        device: &cpal::Device,
        config: &StreamConfig,
        queue: Arc<Mutex<AudioQueue>>,
    ) -> Result<cpal::Stream, PlatformError>
    where
        T: SizedSample + FromSample<f32>,
    {
        device
            .build_output_stream(
                config,
                move |data: &mut [T], _info: &cpal::OutputCallbackInfo| {
                    let mut queue = lock_queue(&queue);
                    for sample in data.iter_mut() {
                        *sample = T::from_sample(queue.pop_or_hold());
                    }
                },
                |error| log::error!("audio output stream error: {error}"),
                None,
            )
            .map_err(|error| {
                AudioError::Open {
                    message: error.to_string(),
                }
                .into()
            })
    }

    /// Locks the queue, recovering the data if a callback ever poisoned it.
    fn lock_queue(queue: &Mutex<AudioQueue>) -> MutexGuard<'_, AudioQueue> {
        match queue.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Picks the rate inside `min..=max`: 48 kHz, then 44.1 kHz, then the
    /// maximum.
    fn pick_rate(min: u32, max: u32) -> u32 {
        for wanted in [48_000, 44_100] {
            if (min..=max).contains(&wanted) {
                return wanted;
            }
        }
        max
    }

    /// Ranks a rate for [`choose_output_spec`] (higher is better).
    fn rate_rank(rate: u32) -> u8 {
        match rate {
            48_000 => 2,
            44_100 => 1,
            _ => 0,
        }
    }

    /// Picks a usable output format from a device's supported
    /// `(channels, min_hz, max_hz)` ranges.
    ///
    /// The mixer renders at most stereo, so only 1- and 2-channel ranges are
    /// considered: stereo over mono, then 48 kHz, then 44.1 kHz, then the
    /// range's maximum rate. Returns `None` when the device offers no
    /// mono/stereo range, in which case the caller uses [`FALLBACK_SPEC`].
    fn choose_output_spec(ranges: impl IntoIterator<Item = (u16, u32, u32)>) -> Option<AudioSpec> {
        let mut best: Option<((u8, u8, u32), AudioSpec)> = None;
        for (channels, min, max) in ranges {
            if channels == 0 || channels > 2 {
                continue;
            }
            let rate = pick_rate(min, max);
            let score = (u8::from(channels == 2), rate_rank(rate), rate);
            if best.as_ref().is_none_or(|(current, _)| score > *current) {
                best = Some((
                    score,
                    AudioSpec {
                        sample_rate: rate,
                        channels,
                        total_frames: None,
                    },
                ));
            }
        }
        best.map(|(_, spec)| spec)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn stereo_is_chosen_over_mono() {
            let spec = choose_output_spec([(1, 44_100, 48_000), (2, 44_100, 44_100)])
                .expect("a stereo range exists");
            assert_eq!(spec.channels, 2);
            assert_eq!(spec.sample_rate, 44_100);
        }

        #[test]
        fn common_rates_are_preferred_then_the_maximum() {
            let common = choose_output_spec([(2, 44_100, 192_000)]).expect("a stereo range exists");
            assert_eq!(common.sample_rate, 48_000);

            let uncommon =
                choose_output_spec([(2, 50_000, 96_000)]).expect("a stereo range exists");
            assert_eq!(uncommon.sample_rate, 96_000);
        }

        #[test]
        fn multichannel_and_zero_channel_layouts_are_rejected() {
            assert!(choose_output_spec([(6, 44_100, 48_000), (0, 44_100, 48_000)]).is_none());
        }

        #[test]
        fn the_fallback_is_the_documented_stereo_format() {
            assert_eq!(FALLBACK_SPEC.sample_rate, 44_100);
            assert_eq!(FALLBACK_SPEC.channels, 2);
        }
    }
}
