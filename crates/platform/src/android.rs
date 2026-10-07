//! Android platform backend.
//!
//! The window and event loop are still stubs: selecting `platform-android`
//! compiles this backend so the higher layers build unchanged, and [`run`]
//! fails fast with [`PlatformError::NotImplemented`] (D11: no silent
//! fallback). The **audio output** backend is real: on Android it drives
//! AAudio (Android 8.0 / API 26+) through the maintained `ndk` crate, with a
//! data callback that drains the same `Arc<Mutex<VecDeque<f32>>>` the Linux
//! cpal backend uses.
//!
//! The `ndk` dependency is target-gated in `Cargo.toml`, so on a non-Android
//! host (where the `platform-android` feature is selected only to
//! compile-check the crate) a typed stub keeps the public API identical and
//! `cargo check` green. The real link (`libaaudio.so`) happens only in an
//! Android build. See `docs/migration/11-android-backends.md`.

use raw_window_handle::{RawDisplayHandle, RawWindowHandle};

use crate::{EventHandler, PlatformConfig, PlatformError};

/// The platform window handle (Android stub).
///
/// The type exists so [`crate::EventHandler`] signatures are identical across
/// platforms, but it cannot be constructed until the Android window backend
/// lands.
#[allow(dead_code)]
pub struct PlatformWindow {
    _private: (),
}

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

    /// Requests a redraw (no-op in the stub).
    pub fn request_redraw(&self) {}
}

/// Runs the Android event loop (stub: always unsupported).
///
/// # Errors
///
/// Always returns [`PlatformError::NotImplemented`].
pub fn run<H: EventHandler>(_config: PlatformConfig, _handler: H) -> Result<(), PlatformError> {
    Err(PlatformError::NotImplemented(
        "android event loop".to_owned(),
    ))
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
    use std::collections::VecDeque;
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

    /// Immutable stream description chosen at open time.
    pub(crate) struct Backend {
        /// Maximum software-queue length in samples (~2 seconds).
        capacity: usize,
    }

    /// The live AAudio stream plus its running flag.
    pub(crate) struct StreamState {
        inner: Mutex<Inner>,
    }

    /// State protected by the [`StreamState`] mutex.
    struct Inner {
        stream: Option<AudioStream>,
        queue: Arc<Mutex<VecDeque<f32>>>,
        disconnected: Arc<AtomicBool>,
        running: bool,
    }

    /// Opens and starts an AAudio output stream for `spec`.
    pub(crate) fn open(spec: AudioSpec) -> Result<(Backend, StreamState), PlatformError> {
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let disconnected = Arc::new(AtomicBool::new(false));
        let callback_queue = Arc::clone(&queue);
        let callback_flag = Arc::clone(&disconnected);

        let builder = AudioStreamBuilder::new().map_err(open_error)?;
        let builder = builder
            .direction(AudioDirection::Output)
            .sample_rate(spec.sample_rate as i32)
            .channel_count(i32::from(spec.channels))
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
        stream.request_start().map_err(stream_error)?;

        let channels = usize::from(spec.channels).max(1);
        let capacity = (spec.sample_rate as usize)
            .saturating_mul(channels)
            .saturating_mul(2);
        Ok((
            Backend { capacity },
            StreamState {
                inner: Mutex::new(Inner {
                    stream: Some(stream),
                    queue,
                    disconnected,
                    running: true,
                }),
            },
        ))
    }

    /// Queues `pcm` for playback, keeping at most two seconds buffered.
    pub(crate) fn write(
        dev: &Backend,
        state: &mut StreamState,
        pcm: &[f32],
    ) -> Result<(), PlatformError> {
        let inner = inner_mut(state);
        if !inner.running {
            return Err(AudioError::NotRunning.into());
        }
        if is_disconnected(inner) {
            // A disconnected stream must always be stopped and closed.
            inner.stream = None;
            inner.running = false;
            return Err(AudioError::NotRunning.into());
        }
        inner.push(dev.capacity, pcm);
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
        queue: &Mutex<VecDeque<f32>>,
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
        let mut next = || queue.pop_front().unwrap_or(0.0);
        match stream.format() {
            AudioFormat::PCM_I16 => buffer::fill_i16(audio_data, samples, &mut next),
            _ => buffer::fill_f32(audio_data, samples, &mut next),
        }
        AudioCallbackResult::Continue
    }

    impl Inner {
        /// Pushes `pcm` into the software queue, keeping at most `capacity`
        /// samples (the caller owns the capacity policy, as on Linux).
        fn push(&self, capacity: usize, pcm: &[f32]) {
            let mut queue = lock_queue(&self.queue);
            let overflow = (queue.len() + pcm.len()).saturating_sub(capacity);
            if overflow > 0 {
                let drop_count = overflow.min(queue.len());
                queue.drain(..drop_count);
            }
            queue.extend(pcm.iter().copied());
        }
    }

    /// Locks the queue, recovering the data if a callback ever poisoned it.
    fn lock_queue(queue: &Mutex<VecDeque<f32>>) -> MutexGuard<'_, VecDeque<f32>> {
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
/// [`PlatformError::NotImplemented`] — never a silent fallback (D11).
#[cfg(not(target_os = "android"))]
#[allow(dead_code)]
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
