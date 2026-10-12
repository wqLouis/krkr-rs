//! Android platform backend.
//!
//! Selecting `platform-android` compiles this backend so the higher layers
//! build unchanged. Three pieces are real:
//!
//! * the **audio output** backend (AAudio on Android, a typed stub elsewhere);
//! * the **window** on Android: [`PlatformWindow`] owns the [`ANativeWindow`]
//!   the activity acquired and projects it to `raw-window-handle`, so the
//!   renderer can create and recreate its Vulkan surface. Its size is the
//!   surface's physical pixel size and its scale factor is the display's
//!   `densityDpi / 160`;
//! * the pure **input and density translation** the event loop uses (the
//!   `*_from_android` associated functions), which names no Android type and
//!   is therefore host-testable.
//!
//! The **event loop** is still a typed stub: [`run`] returns
//! [`PlatformError::NotImplemented`]. Driving the loop needs the
//! `android-activity` glue (and its `GameActivity` entry point) to deliver the
//! window, lifecycle and input queue to [`crate::EventHandler`]; this crate
//! does not depend on it, so the backend fails fast rather than presenting a
//! black surface to an Activity that no code can start.
//!
//! The `ndk` dependency is target-gated in `Cargo.toml`, so on a non-Android
//! host (where the `platform-android` feature is selected only to
//! compile-check the crate) a typed stub keeps the public API identical and
//! `cargo check` green. The real link (`libaaudio.so`, `libandroid.so`)
//! happens only in an Android build.
//!
//! [`ANativeWindow`]: https://developer.android.com/ndk/reference/group/a-native-window

use raw_window_handle::{RawDisplayHandle, RawWindowHandle};

use api::host::TouchPhase;

use crate::input::{MouseButton, PhysicalKey};
use crate::{EventHandler, PlatformConfig, PlatformError};

/// The bits Android uses for the pointer index inside a motion action.
const MOTION_ACTION_MASK: i32 = 0xFF;

/// The platform window handle on Android.
///
/// Owns a reference to the `ANativeWindow` the activity handed over:
/// constructing the type does not acquire a second reference, and dropping it
/// releases exactly one (the `ndk` wrapper's contract). The renderer obtains a
/// [`RawWindowHandle::AndroidNdk`] from [`PlatformWindow::raw_window_handle`]
/// and binds its Vulkan surface to that window.
#[cfg(target_os = "android")]
pub struct PlatformWindow {
    window: ndk::native_window::NativeWindow,
    density_dpi: i32,
}

#[cfg(target_os = "android")]
impl PlatformWindow {
    /// Wraps an owned `ANativeWindow` and the display density it belongs to.
    ///
    /// `density_dpi` is the activity's `Configuration.densityDpi`; it becomes
    /// the scale factor through [`PlatformWindow::density_scale_from_android`].
    pub fn new(window: ndk::native_window::NativeWindow, density_dpi: i32) -> Self {
        Self {
            window,
            density_dpi,
        }
    }

    /// Returns the raw `ANativeWindow` handle the renderer binds to.
    ///
    /// # Errors
    ///
    /// The `ndk` wrapper never fails to build the handle; the `Result` keeps
    /// the signature identical to the Linux backend.
    pub fn raw_window_handle(&self) -> Result<RawWindowHandle, PlatformError> {
        use raw_window_handle::HasWindowHandle;
        self.window
            .window_handle()
            .map(|handle| handle.as_raw())
            .map_err(|error| PlatformError::Handle(error.to_string()))
    }

    /// Returns the Android display handle.
    ///
    /// # Errors
    ///
    /// Android's display handle carries no data, so this cannot fail; the
    /// `Result` keeps the signature identical to the Linux backend.
    pub fn raw_display_handle(&self) -> Result<RawDisplayHandle, PlatformError> {
        Ok(RawDisplayHandle::Android(
            raw_window_handle::AndroidDisplayHandle::new(),
        ))
    }

    /// Returns the surface size in physical pixels (at least 1×1).
    ///
    /// This is the `ANativeWindow` buffer size, i.e. the same physical space
    /// the input coordinates use.
    pub fn size(&self) -> (u32, u32) {
        (
            self.window.width().unsigned_abs().max(1),
            self.window.height().unsigned_abs().max(1),
        )
    }

    /// Returns the display scale factor (`densityDpi / 160`).
    pub fn scale_factor(&self) -> f64 {
        Self::density_scale_from_android(self.density_dpi)
    }

    /// Returns `None`: Android sizes the surface from the display, not from a
    /// client request, so there is no applied size to report.
    pub fn set_size(&self, _size: (u32, u32)) -> Option<(u32, u32)> {
        None
    }

    /// No-op: the Android frame loop is ticker-driven, so a redraw request does
    /// not need to wake a waiting event loop.
    pub fn request_redraw(&self) {}
}

/// The platform window handle (non-Android placeholder).
///
/// The `ndk` dependency is target-gated, so compiling `platform-android` for a
/// host has no `ANativeWindow` to wrap. The type exists so the
/// [`crate::EventHandler`] signatures are identical across platforms, and every
/// accessor fails with a typed [`PlatformError::NotImplemented`] rather than a
/// fabricated handle.
#[cfg(not(target_os = "android"))]
pub struct PlatformWindow {
    _private: (),
}

#[cfg(not(target_os = "android"))]
impl PlatformWindow {
    /// Returns the raw window handle (always an error in the stub).
    pub fn raw_window_handle(&self) -> Result<RawWindowHandle, PlatformError> {
        Err(PlatformError::NotImplemented(
            "android raw window handle".to_owned(),
        ))
    }

    /// Returns the raw display handle (always an error in the stub).
    pub fn raw_display_handle(&self) -> Result<RawDisplayHandle, PlatformError> {
        Err(PlatformError::NotImplemented(
            "android raw display handle".to_owned(),
        ))
    }

    /// Returns the surface size (always 1×1 in the stub).
    pub fn size(&self) -> (u32, u32) {
        (1, 1)
    }

    /// Returns the display scale factor (always 1.0 in the stub).
    pub fn scale_factor(&self) -> f64 {
        1.0
    }

    /// Requests an inner size (no-op in the stub).
    pub fn set_size(&self, _size: (u32, u32)) -> Option<(u32, u32)> {
        None
    }

    /// Requests a redraw (no-op in the stub).
    pub fn request_redraw(&self) {}
}

/// Android code -> platform-neutral value translation.
///
/// These are associated functions on [`PlatformWindow`] because the event loop
/// owns the window, and they name no Android type, so they compile and are
/// tested on the host. The TJS virtual-key codes are **not** assigned here: the
/// compatibility layer maps [`PhysicalKey`] onto the reference `VK_*` table.
impl PlatformWindow {
    /// Maps an Android `AKEYCODE_*` value to the platform-neutral physical key.
    ///
    /// The table covers the keys the reference engine's key conversion names;
    /// any other code keeps its raw identity as [`PhysicalKey::Native`], which
    /// the compatibility layer maps to no virtual-key code.
    pub fn physical_key_from_android(code: i32) -> PhysicalKey {
        ANDROID_KEY_TABLE
            .iter()
            .find(|(candidate, _)| *candidate == code)
            .map_or(PhysicalKey::Native(code as u32), |(_, key)| *key)
    }

    /// Maps an Android `AMOTION_EVENT_ACTION_*` value to a touch phase.
    ///
    /// The pointer index Android packs into the high bits is masked off, so
    /// `ACTION_POINTER_DOWN`/`ACTION_POINTER_UP` are handled too. Actions that
    /// are not a contact phase (`HOVER_*`, `SCROLL`, `BUTTON_*`, `OUTSIDE`)
    /// return `None`.
    pub fn touch_phase_from_android(action: i32) -> Option<TouchPhase> {
        match action & MOTION_ACTION_MASK {
            0 | 5 => Some(TouchPhase::Started),
            2 => Some(TouchPhase::Moved),
            1 | 6 => Some(TouchPhase::Ended),
            3 => Some(TouchPhase::Cancelled),
            _ => None,
        }
    }

    /// Maps an Android `AMOTION_EVENT_BUTTON_*` value to a mouse button.
    pub fn mouse_button_from_android(button: i32) -> Option<MouseButton> {
        match button {
            1 => Some(MouseButton::Left),
            2 => Some(MouseButton::Right),
            4 => Some(MouseButton::Middle),
            8 => Some(MouseButton::Back),
            16 => Some(MouseButton::Forward),
            _ => None,
        }
    }

    /// Converts an Android `Configuration.densityDpi` to the display scale.
    ///
    /// Android defines `DisplayMetrics.density = densityDpi / 160`, so 160 dpi
    /// is scale 1.0 and a 420 dpi phone is scale 2.625.
    pub fn density_scale_from_android(density_dpi: i32) -> f64 {
        f64::from(density_dpi) / 160.0
    }
}

/// Android `AKEYCODE_*` -> platform-neutral physical key.
///
/// The values are the stable AOSP `android/keycodes.h` constants. The mapping
/// stops at the physical key; the reference virtual-key code is chosen by the
/// compatibility layer. `AKEYCODE_BACK` is `Escape`, so the Android back key
/// reaches the engine as Escape.
const ANDROID_KEY_TABLE: &[(i32, PhysicalKey)] = &[
    (29, PhysicalKey::KeyA),
    (30, PhysicalKey::KeyB),
    (31, PhysicalKey::KeyC),
    (32, PhysicalKey::KeyD),
    (33, PhysicalKey::KeyE),
    (34, PhysicalKey::KeyF),
    (35, PhysicalKey::KeyG),
    (36, PhysicalKey::KeyH),
    (37, PhysicalKey::KeyI),
    (38, PhysicalKey::KeyJ),
    (39, PhysicalKey::KeyK),
    (40, PhysicalKey::KeyL),
    (41, PhysicalKey::KeyM),
    (42, PhysicalKey::KeyN),
    (43, PhysicalKey::KeyO),
    (44, PhysicalKey::KeyP),
    (45, PhysicalKey::KeyQ),
    (46, PhysicalKey::KeyR),
    (47, PhysicalKey::KeyS),
    (48, PhysicalKey::KeyT),
    (49, PhysicalKey::KeyU),
    (50, PhysicalKey::KeyV),
    (51, PhysicalKey::KeyW),
    (52, PhysicalKey::KeyX),
    (53, PhysicalKey::KeyY),
    (54, PhysicalKey::KeyZ),
    (7, PhysicalKey::Digit0),
    (8, PhysicalKey::Digit1),
    (9, PhysicalKey::Digit2),
    (10, PhysicalKey::Digit3),
    (11, PhysicalKey::Digit4),
    (12, PhysicalKey::Digit5),
    (13, PhysicalKey::Digit6),
    (14, PhysicalKey::Digit7),
    (15, PhysicalKey::Digit8),
    (16, PhysicalKey::Digit9),
    (131, PhysicalKey::F1),
    (132, PhysicalKey::F2),
    (133, PhysicalKey::F3),
    (134, PhysicalKey::F4),
    (135, PhysicalKey::F5),
    (136, PhysicalKey::F6),
    (137, PhysicalKey::F7),
    (138, PhysicalKey::F8),
    (139, PhysicalKey::F9),
    (140, PhysicalKey::F10),
    (141, PhysicalKey::F11),
    (142, PhysicalKey::F12),
    (144, PhysicalKey::Numpad0),
    (145, PhysicalKey::Numpad1),
    (146, PhysicalKey::Numpad2),
    (147, PhysicalKey::Numpad3),
    (148, PhysicalKey::Numpad4),
    (149, PhysicalKey::Numpad5),
    (150, PhysicalKey::Numpad6),
    (151, PhysicalKey::Numpad7),
    (152, PhysicalKey::Numpad8),
    (153, PhysicalKey::Numpad9),
    (157, PhysicalKey::NumpadAdd),
    (156, PhysicalKey::NumpadSubtract),
    (155, PhysicalKey::NumpadMultiply),
    (154, PhysicalKey::NumpadDivide),
    (158, PhysicalKey::NumpadDecimal),
    (159, PhysicalKey::NumpadSeparator),
    (160, PhysicalKey::NumpadEnter),
    (67, PhysicalKey::Backspace),
    (112, PhysicalKey::Delete),
    (61, PhysicalKey::Tab),
    (66, PhysicalKey::Enter),
    (111, PhysicalKey::Escape),
    (62, PhysicalKey::Space),
    (124, PhysicalKey::Insert),
    (122, PhysicalKey::Home),
    (123, PhysicalKey::End),
    (92, PhysicalKey::PageUp),
    (93, PhysicalKey::PageDown),
    (19, PhysicalKey::ArrowUp),
    (20, PhysicalKey::ArrowDown),
    (21, PhysicalKey::ArrowLeft),
    (22, PhysicalKey::ArrowRight),
    (59, PhysicalKey::ShiftLeft),
    (60, PhysicalKey::ShiftRight),
    (113, PhysicalKey::ControlLeft),
    (114, PhysicalKey::ControlRight),
    (57, PhysicalKey::AltLeft),
    (58, PhysicalKey::AltRight),
    (117, PhysicalKey::SuperLeft),
    (118, PhysicalKey::SuperRight),
    (115, PhysicalKey::CapsLock),
    (143, PhysicalKey::NumLock),
    (116, PhysicalKey::ScrollLock),
    (120, PhysicalKey::PrintScreen),
    (121, PhysicalKey::Pause),
    (68, PhysicalKey::Backquote),
    (73, PhysicalKey::Backslash),
    (71, PhysicalKey::BracketLeft),
    (72, PhysicalKey::BracketRight),
    (55, PhysicalKey::Comma),
    (56, PhysicalKey::Period),
    (74, PhysicalKey::Semicolon),
    (75, PhysicalKey::Quote),
    (69, PhysicalKey::Minus),
    (70, PhysicalKey::Equal),
    (76, PhysicalKey::Slash),
    (82, PhysicalKey::ContextMenu),
    (4, PhysicalKey::Escape),
    (3, PhysicalKey::Home),
    (24, PhysicalKey::VolumeUp),
    (25, PhysicalKey::VolumeDown),
    (164, PhysicalKey::VolumeMute),
    (85, PhysicalKey::MediaPlayPause),
    (86, PhysicalKey::MediaStop),
    (87, PhysicalKey::MediaTrackNext),
    (88, PhysicalKey::MediaTrackPrevious),
    (28, PhysicalKey::Clear),
];

/// Runs the Android event loop.
///
/// # Errors
///
/// Always returns [`PlatformError::NotImplemented`]. The window is real, but
/// the loop needs the `android-activity` glue that delivers the
/// `ANativeWindow`, the lifecycle and the input queue to [`crate::EventHandler`];
/// this crate does not depend on it, so the backend fails fast instead of
/// running a frame loop that no Activity can start.
pub fn run<H: EventHandler>(_config: PlatformConfig, _handler: H) -> Result<(), PlatformError> {
    Err(PlatformError::NotImplemented(
        "android event loop (the android-activity glue is not compiled in)".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference turns the Android back key into Escape before converting
    /// it to a virtual-key code.
    /// reference/cpp/core/environ/cocos2d/MainScene.cpp:820-829
    #[test]
    fn back_becomes_escape() {
        assert_eq!(
            PlatformWindow::physical_key_from_android(4),
            PhysicalKey::Escape
        );
    }

    /// The reference maps the letter/digit families one-to-one and collapses
    /// the left/right modifiers onto their own side at the physical layer.
    /// reference/cpp/core/environ/cocos2d/CCKeyCodeConv.cpp:29-64,93-103
    #[test]
    fn letters_digits_and_modifiers_map() {
        assert_eq!(
            PlatformWindow::physical_key_from_android(29),
            PhysicalKey::KeyA
        );
        assert_eq!(
            PlatformWindow::physical_key_from_android(54),
            PhysicalKey::KeyZ
        );
        assert_eq!(
            PlatformWindow::physical_key_from_android(7),
            PhysicalKey::Digit0
        );
        assert_eq!(
            PlatformWindow::physical_key_from_android(16),
            PhysicalKey::Digit9
        );
        assert_eq!(
            PlatformWindow::physical_key_from_android(59),
            PhysicalKey::ShiftLeft
        );
        assert_eq!(
            PlatformWindow::physical_key_from_android(60),
            PhysicalKey::ShiftRight
        );
    }

    /// A code the reference names no key for keeps its raw identity instead of
    /// being dropped or guessed at.
    /// reference/cpp/core/environ/cocos2d/CCKeyCodeConv.cpp:236-237
    #[test]
    fn unmapped_key_keeps_its_native_code() {
        // AKEYCODE_STAR (17) has no entry in the reference key conversion.
        assert_eq!(
            PlatformWindow::physical_key_from_android(17),
            PhysicalKey::Native(17)
        );
    }

    /// Android packs the pointer index into the high bits of a pointer action,
    /// so the contact phase must be read from the low byte.
    /// reference/cpp/core/environ/cocos2d/MainScene.cpp:1017-1146
    #[test]
    fn pointer_actions_are_masked_to_their_phase() {
        assert_eq!(
            PlatformWindow::touch_phase_from_android(5 | (3 << 8)),
            Some(TouchPhase::Started)
        );
        assert_eq!(
            PlatformWindow::touch_phase_from_android(6 | (2 << 8)),
            Some(TouchPhase::Ended)
        );
    }

    /// A hover or scroll motion is not a contact phase.
    /// reference/cpp/core/environ/cocos2d/MainScene.cpp:1017-1146
    #[test]
    fn non_contact_actions_have_no_phase() {
        assert_eq!(PlatformWindow::touch_phase_from_android(7), None);
        assert_eq!(PlatformWindow::touch_phase_from_android(8), None);
    }
}

/// Audio output backend (Android, AAudio via the `ndk` crate).
///
/// The public object/state API lives in [`crate::audio`]; this module is the
/// real backend. As on Linux, the stream's data callback drains a software
/// queue that `write` fills, so the object stays stateless and the queue lives
/// in the caller-owned state value.
///
/// # Format handling
///
/// We request [`AudioFormat::PCM_Float`]. AAudio may instead return the
/// device's [`AudioFormat::PCM_I16`], which the callback converts exactly as
/// cpal's `FromSample` does. This reads the format with `AudioStream::format`
/// (the ABI's `AAudioStream_getFormat`); the guide's `AAudioStream_getDataFormat`
/// snippet is a documentation typo — no such symbol exists.
///
/// # Callback ownership
///
/// The stream owns a data callback, so `AAudioStream_write` must never be
/// called on it (AAudio forbids it, and the `ndk` wrapper marks it `unsafe`
/// for that reason). `write` only pushes into the queue.
///
/// # `unsafe`
///
/// `ndk` wraps the whole AAudio surface safely except for one thing: the data
/// callback hands the audio buffer over as a raw `*mut c_void`. The private
/// [`buffer`] module is the only `unsafe` in the crate and converts that
/// pointer into a writable slice, with a `SAFETY:` comment on each block.
#[cfg(target_os = "android")]
pub(crate) mod audio {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard};

    use api::error::AudioError;
    use api::media::AudioSpec;
    use ndk::audio::{
        AudioCallbackResult, AudioDirection, AudioError as NdkAudioError, AudioFormat,
        AudioPerformanceMode, AudioSharingMode, AudioStream, AudioStreamBuilder, AudioStreamState,
    };

    use crate::PlatformError;
    use crate::audio_queue::AudioQueue;

    /// Immutable stream description chosen at open time.
    ///
    /// The device format is baked into the [`AudioQueue`] and the AAudio
    /// stream; the type exists so the real backend has a stable object type.
    pub(crate) struct Backend;

    /// The output format used only when the device reports no usable format of
    /// its own (see [`default_spec`]). 44.1 kHz stereo is the KiriKiri/reference
    /// mix rate and the format every device we target can render.
    const FALLBACK_SPEC: AudioSpec = AudioSpec {
        sample_rate: 44_100,
        channels: 2,
        total_frames: None,
    };

    /// The live AAudio stream plus its running flag.
    pub(crate) struct StreamState {
        inner: Mutex<Inner>,
    }

    /// State protected by the [`StreamState`] mutex.
    struct Inner {
        stream: Option<AudioStream>,
        queue: Arc<Mutex<AudioQueue>>,
        disconnected: Arc<AtomicBool>,
        /// Set once the queue reached pre-fill and `request_start` succeeded.
        running: bool,
    }

    /// Opens an AAudio output stream for `spec` but does not start it yet.
    ///
    /// The stream is started by [`write`] once the queue holds the pre-fill
    /// target, so AAudio never drains an empty queue.
    pub(crate) fn open(spec: AudioSpec) -> Result<(Backend, StreamState), PlatformError> {
        let queue = Arc::new(Mutex::new(AudioQueue::new(
            spec.sample_rate,
            spec.channels,
        )?));
        let disconnected = Arc::new(AtomicBool::new(false));
        let callback_queue = Arc::clone(&queue);
        let callback_flag = Arc::clone(&disconnected);

        let builder = AudioStreamBuilder::new().map_err(open_error)?;
        let channel_count = i32::try_from(spec.channels).map_err(|_| AudioError::InvalidSpec {
            message: format!(
                "channel count {} does not fit in an AAudio stream",
                spec.channels
            ),
        })?;
        let builder = builder
            .direction(AudioDirection::Output)
            .sample_rate(spec.sample_rate as i32)
            .channel_count(channel_count)
            .format(AudioFormat::PCM_Float)
            .sharing_mode(AudioSharingMode::Shared)
            .performance_mode(AudioPerformanceMode::LowLatency)
            .data_callback(Box::new(move |stream, audio_data, num_frames| {
                data_callback(&callback_queue, stream, audio_data, num_frames)
            }))
            .error_callback(Box::new(move |_stream, error| {
                if error == NdkAudioError::Disconnected {
                    callback_flag.store(true, Ordering::Release);
                }
                log::error!("android audio stream error: {error}");
            }));

        let stream = builder.open_stream().map_err(open_error)?;
        match stream.format() {
            AudioFormat::PCM_Float | AudioFormat::PCM_I16 => {}
            other => {
                return Err(AudioError::Unsupported {
                    message: format!(
                        "AAudio returned unsupported format {other:?}; only PCM float and i16 are \
                         handled"
                    ),
                }
                .into());
            }
        }
        Ok((
            Backend,
            StreamState {
                inner: Mutex::new(Inner {
                    stream: Some(stream),
                    queue,
                    disconnected,
                    running: false,
                }),
            },
        ))
    }

    /// Returns the default output device's preferred format.
    ///
    /// AAudio has no "query the default" call, so this opens a short-lived
    /// probe stream with no data callback, reads the rate and channel count
    /// AAudio chose, and closes it. A device that opens but reports more than
    /// two channels (which the mixer cannot consume) falls back to
    /// [`FALLBACK_SPEC`].
    pub(crate) fn default_spec() -> Result<AudioSpec, PlatformError> {
        let builder = AudioStreamBuilder::new().map_err(open_error)?;
        let stream = builder
            .direction(AudioDirection::Output)
            .format(AudioFormat::PCM_Float)
            .sharing_mode(AudioSharingMode::Shared)
            .open_stream()
            .map_err(open_error)?;
        let rate = stream.sample_rate();
        let channels = stream.channel_count();
        // `stream` drops here, closing the probe before it is ever started.
        if rate > 0 && (1..=2).contains(&channels) {
            let channels = u32::try_from(channels).map_err(|_| AudioError::InvalidSpec {
                message: format!("AAudio reported {channels} channels, which is out of range"),
            })?;
            return Ok(AudioSpec {
                sample_rate: rate as u32,
                channels,
                total_frames: None,
            });
        }
        log::warn!(
            "android audio: the device reports {rate} Hz/{channels} ch; using the documented \
             fallback {FALLBACK_SPEC:?}"
        );
        Ok(FALLBACK_SPEC)
    }

    /// Queues `pcm` for playback, keeping at most two seconds buffered, and
    /// starts the stream once the pre-fill target is reached.
    pub(crate) fn write(
        _dev: &Backend,
        state: &mut StreamState,
        pcm: &[f32],
    ) -> Result<(), PlatformError> {
        let inner = inner_mut(state);
        if inner.stream.is_none() {
            return Err(AudioError::NotRunning.into());
        }
        if is_disconnected(inner) {
            // A disconnected stream must always be stopped and closed.
            inner.stream = None;
            inner.running = false;
            return Err(AudioError::NotRunning.into());
        }
        let prefilled = inner.push(pcm);
        if prefilled && !inner.running {
            if let Some(stream) = &inner.stream {
                stream.request_start().map_err(stream_error)?;
            }
            inner.running = true;
        }
        Ok(())
    }

    /// Stops the stream and releases the OS resources (idempotent).
    pub(crate) fn stop(_dev: &Backend, state: &mut StreamState) {
        let inner = inner_mut(state);
        if let Some(stream) = inner.stream.take() {
            if let Err(error) = stream.request_stop() {
                log::warn!("android audio: request_stop failed: {error}");
            }
            // `stream` drops here, closing it.
        }
        inner.running = false;
    }

    /// Whether the stream is currently running.
    ///
    /// Reading the state also reaps a stream that AAudio reports
    /// disconnected: it is stopped and closed here, on a non-callback thread
    /// (closing from the callback thread is forbidden by the AAudio guide).
    pub(crate) fn is_running(state: &StreamState) -> bool {
        let mut inner = lock(state);
        if !inner.running {
            return false;
        }
        if is_disconnected(&inner) {
            inner.stream = None;
            inner.running = false;
            return false;
        }
        inner.stream.as_ref().is_some_and(|stream| {
            matches!(
                stream.state(),
                AudioStreamState::Started | AudioStreamState::Starting
            )
        })
    }

    /// Whether AAudio reported the stream disconnected (error callback or the
    /// live stream state).
    fn is_disconnected(inner: &Inner) -> bool {
        inner.disconnected.load(Ordering::Acquire)
            || inner
                .stream
                .as_ref()
                .is_some_and(|stream| stream.state() == AudioStreamState::Disconnected)
    }

    /// The AAudio data callback: drains the software queue into `audioData`.
    ///
    /// The format and channel count are queried from the stream (AAudio's
    /// `get*` calls are explicitly allowed from the callback).
    fn data_callback(
        queue: &Mutex<AudioQueue>,
        stream: &AudioStream,
        audio_data: *mut c_void,
        num_frames: i32,
    ) -> AudioCallbackResult {
        if audio_data.is_null() || num_frames < 0 {
            return AudioCallbackResult::Continue;
        }
        let channels = stream.channel_count().max(1) as usize;
        let samples = num_frames as usize * channels;
        let mut queue = lock_queue(queue);
        let mut next = || queue.pop_or_hold();
        match stream.format() {
            AudioFormat::PCM_I16 => buffer::fill_i16(audio_data, samples, &mut next),
            _ => buffer::fill_f32(audio_data, samples, &mut next),
        }
        AudioCallbackResult::Continue
    }

    impl Inner {
        /// Pushes `pcm` into the software queue and returns whether the
        /// pre-fill target is now buffered (the same policy as Linux).
        fn push(&self, pcm: &[f32]) -> bool {
            let mut queue = lock_queue(&self.queue);
            queue.push(pcm)
        }
    }

    /// Locks the queue, recovering the data if a callback ever poisoned it.
    fn lock_queue(queue: &Mutex<AudioQueue>) -> MutexGuard<'_, AudioQueue> {
        match queue.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Locks the state, recovering the data if a panic ever poisoned it.
    fn lock(state: &StreamState) -> MutexGuard<'_, Inner> {
        match state.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Mutable access to the state for the `&mut` entry points.
    fn inner_mut(state: &mut StreamState) -> &mut Inner {
        match state.inner.get_mut() {
            Ok(inner) => inner,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Wraps an `ndk` error as an [`AudioError::Open`].
    fn open_error(error: NdkAudioError) -> PlatformError {
        AudioError::Open {
            message: error.to_string(),
        }
        .into()
    }

    /// Wraps an `ndk` error as an [`AudioError::Stream`].
    fn stream_error(error: NdkAudioError) -> PlatformError {
        AudioError::Stream {
            message: error.to_string(),
        }
        .into()
    }

    /// The one AAudio operation `ndk` does not wrap: turning the callback's
    /// raw `audioData` pointer into a writable slice.
    ///
    /// This is the only `unsafe` in the crate, hence the scoped
    /// `#[allow(unsafe_code)]` and the `SAFETY:` comments.
    #[allow(unsafe_code)]
    mod buffer {
        use std::ffi::c_void;

        /// Fills the callback buffer with interleaved `f32`.
        pub(super) fn fill_f32(ptr: *mut c_void, samples: usize, mut next: impl FnMut() -> f32) {
            // SAFETY: AAudio guarantees that, for an output stream, the
            // callback's `audioData` points to at least
            // `numFrames * channelCount` samples in the stream's data format.
            // The caller derives `samples` from `numFrames` and the stream's
            // channel count, and this branch is selected only when the stream
            // format is `PCM_Float`.
            let out = unsafe { std::slice::from_raw_parts_mut(ptr.cast::<f32>(), samples) };
            for slot in out.iter_mut() {
                *slot = next();
            }
        }

        /// Fills the callback buffer with interleaved `i16`, converting `f32`.
        pub(super) fn fill_i16(ptr: *mut c_void, samples: usize, mut next: impl FnMut() -> f32) {
            // SAFETY: as `fill_f32`, but the stream's data format is
            // `PCM_I16`, so the buffer holds `samples` `i16` values.
            let out = unsafe { std::slice::from_raw_parts_mut(ptr.cast::<i16>(), samples) };
            for slot in out.iter_mut() {
                *slot = f32_to_i16(next());
            }
        }

        /// Converts a sample exactly like cpal's `FromSample<f32> for i16`
        /// (`dasp_sample`: `(s * 32768.0) as i16`, saturating in Rust).
        fn f32_to_i16(sample: f32) -> i16 {
            (sample * 32_768.0) as i16
        }
    }
}

/// Non-Android placeholder backend.
///
/// The `ndk` dependency is target-gated, so on a host that selects
/// `platform-android` only to compile-check the crate this stub keeps the
/// public API identical. Every entry point fails with a typed
/// [`PlatformError::NotImplemented`] — never a silent fallback.
#[cfg(not(target_os = "android"))]
pub(crate) mod audio {
    use api::media::AudioSpec;

    use crate::PlatformError;

    /// Immutable stream-description placeholder.
    pub(crate) struct Backend;

    /// Stream-state placeholder.
    pub(crate) struct StreamState;

    /// Always fails: the AAudio backend only exists on Android.
    pub(crate) fn open(_spec: AudioSpec) -> Result<(Backend, StreamState), PlatformError> {
        Err(PlatformError::NotImplemented(
            "android audio output (AAudio only runs on Android)".to_owned(),
        ))
    }

    /// Always fails: there is no AAudio device on a non-Android host.
    pub(crate) fn default_spec() -> Result<AudioSpec, PlatformError> {
        Err(PlatformError::NotImplemented(
            "android audio output (AAudio only runs on Android)".to_owned(),
        ))
    }

    /// Always fails: the AAudio backend only exists on Android.
    pub(crate) fn write(
        _dev: &Backend,
        _state: &mut StreamState,
        _pcm: &[f32],
    ) -> Result<(), PlatformError> {
        Err(PlatformError::NotImplemented(
            "android audio output (AAudio only runs on Android)".to_owned(),
        ))
    }

    /// No-op in the stub.
    pub(crate) fn stop(_dev: &Backend, _state: &mut StreamState) {}

    /// Always `false` in the stub.
    pub(crate) fn is_running(_state: &StreamState) -> bool {
        false
    }
}
