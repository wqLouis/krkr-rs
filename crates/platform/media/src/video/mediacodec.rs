//! Android MediaCodec video backend — the `ndk` crate plus an
//! `AMediaExtractor` shim.
//!
//! Selected by the `video-mediacodec` feature (the Android frontend turns the
//! default `video-ffmpeg` off). Codec and format work go through the
//! maintained [`ndk`](https://docs.rs/ndk) crate's safe
//! [`ndk::media::media_codec`] / [`ndk::media::media_format`] bindings; the
//! NDK does not expose `AMediaExtractor` in `ndk`, so a small raw `extern "C"`
//! shim in [`extractor`] demuxes the container. Behind the same
//! `open`/`present`/`audio` surface as the FFmpeg backend — no JNI, no FFmpeg.
//!
//! # In-memory source
//!
//! The layer above hands us an [`Arc<[u8]>`]; the API-21 extractor only takes
//! a path or a file descriptor (the `AMediaDataSource` callbacks are API 28+),
//! so the bytes are written to an anonymous `memfd` and that descriptor is
//! passed to `AMediaExtractor_setDataSourceFd`. Nothing touches the filesystem
//! and no path is guessed; a refused syscall returns a typed [`MediaError`]
//! instead of a fallback.
//!
//! # Verified vs unverified
//!
//! * **Host-tested:** the YUV420 → RGBA conversion and the µs → ms rescale
//!   live in [`crate::yuv`] and are compiled/tested on every target.
//! * **Compile-checked:** the whole module, including the Android path
//!   (`cargo check --target aarch64-linux-android`, which never links).
//! * **On-device validation still required:** the demux/decode loop, the
//!   `memfd` trick, seeking, colour-format negotiation and the exact
//!   stride/slice-height/crop a codec reports. None of this has run here.
//!
//! # State and errors
//!
//! Every demuxer/codec/format handle lives in [`VideoDecoderState`]; [`Media`]
//! stores nothing and there are no globals. Failures return typed
//! [`MediaError`]s (never a silent fallback). Dropping the state stops and
//! deletes every codec, the extractor and the format handles (RAII).
//!
//! On a non-Android host the `ndk` dependency is absent (it is target-gated in
//! `Cargo.toml`), so [`stub`] provides the typed `Unsupported` placeholder
//! that keeps `cargo check` green.

#[cfg(not(target_os = "android"))]
pub use stub::{VideoDecoderState, audio, open, present};

#[cfg(target_os = "android")]
pub use android::{VideoDecoderState, audio, open, present};

/// Non-Android placeholder backend.
///
/// Keeps the public API identical to the Android backend so the middle layer
/// compiles unchanged, but every entry point returns a typed
/// [`MediaError::Unsupported`] — never a silent fallback.
#[cfg(not(target_os = "android"))]
mod stub {
    use std::sync::Arc;

    use api::error::MediaError;
    use api::media::{DecodedAudio, RgbaFrame, VideoSpec};

    use crate::Media;

    /// Bottom-defined state for an incremental MediaCodec decode.
    ///
    /// The MediaCodec backend only runs on Android; on this host the value is
    /// uninhabited from outside the crate and carries no state. The type
    /// exists so the public API is identical to the Android build.
    #[non_exhaustive]
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct VideoDecoderState {}

    /// Opens a video stream. Host stub: always fails.
    pub fn open(
        m: &Media,
        bytes: Arc<[u8]>,
        hint: Option<&str>,
    ) -> Result<(VideoSpec, VideoDecoderState), MediaError> {
        let _ = (m, bytes, hint);
        Err(host_stub_error("video decode (mediacodec)"))
    }

    /// Presents a frame. Host stub: always fails.
    pub fn present(
        m: &Media,
        state: &mut VideoDecoderState,
        at_ms: u64,
    ) -> Result<RgbaFrame, MediaError> {
        let _ = (m, state, at_ms);
        Err(host_stub_error("video decode (mediacodec)"))
    }

    /// Returns the decoded audio track. Host stub: always fails.
    pub fn audio(m: &Media, state: &VideoDecoderState) -> Result<DecodedAudio, MediaError> {
        let _ = (m, state);
        Err(host_stub_error("video audio (mediacodec)"))
    }

    fn host_stub_error(feature: &str) -> MediaError {
        MediaError::Unsupported {
            message: format!("{feature}: MediaCodec only runs on Android"),
        }
    }
}

/// Raw FFI shim for `AMediaExtractor`, which `ndk` does not expose.
///
/// This is the module the crate's scoped `#[allow(unsafe_code)]` exists for;
/// every `unsafe` block has a SAFETY comment. It also owns the `memfd_create`
/// syscall wrapper, since creating an anonymous file descriptor from memory is
/// not exposed by `ndk` either.
#[cfg(target_os = "android")]
#[allow(unsafe_code)]
mod extractor {
    use std::ffi::{c_int, c_long, c_void};
    use std::fs::File;
    use std::io::{self, Write};
    use std::mem::MaybeUninit;
    use std::os::unix::io::{FromRawFd, RawFd};
    use std::ptr::NonNull;

    use ndk::media::media_format::MediaFormat;

    /// `AMEDIA_OK` from `NdkMediaError.h`.
    pub const AMEDIA_OK: c_int = 0;
    /// `AMEDIAEXTRACTOR_SEEK_PREVIOUS_SYNC`.
    pub const AMEDIAEXTRACTOR_SEEK_PREVIOUS_SYNC: c_int = 0;

    /// `MFD_CLOEXEC`.
    const MFD_CLOEXEC: u32 = 1;

    /// `SYS_memfd_create`, per ABI (only these Android ABIs exist).
    #[cfg(target_arch = "aarch64")]
    const SYS_MEMFD_CREATE: c_long = 279;
    /// See [`SYS_MEMFD_CREATE`].
    #[cfg(target_arch = "arm")]
    const SYS_MEMFD_CREATE: c_long = 356;
    /// See [`SYS_MEMFD_CREATE`].
    #[cfg(target_arch = "x86")]
    const SYS_MEMFD_CREATE: c_long = 356;
    /// See [`SYS_MEMFD_CREATE`].
    #[cfg(target_arch = "x86_64")]
    const SYS_MEMFD_CREATE: c_long = 319;

    #[repr(C)]
    struct AMediaExtractor {
        _private: [u8; 0],
    }

    #[link(name = "mediandk")]
    #[allow(non_snake_case)]
    unsafe extern "C" {
        fn AMediaExtractor_new() -> *mut AMediaExtractor;
        fn AMediaExtractor_delete(extractor: *mut AMediaExtractor) -> c_int;
        fn AMediaExtractor_setDataSourceFd(
            extractor: *mut AMediaExtractor,
            fd: c_int,
            offset: i64,
            length: i64,
        ) -> c_int;
        fn AMediaExtractor_getTrackCount(extractor: *mut AMediaExtractor) -> usize;
        // Returns an owned `AMediaFormat *`; the concrete pointee type is
        // private to `ndk`/`ndk-sys`, so it is handled as an opaque pointer.
        fn AMediaExtractor_getTrackFormat(
            extractor: *mut AMediaExtractor,
            index: usize,
        ) -> *mut c_void;
        fn AMediaExtractor_selectTrack(extractor: *mut AMediaExtractor, index: usize) -> c_int;
        fn AMediaExtractor_unselectTrack(extractor: *mut AMediaExtractor, index: usize) -> c_int;
        fn AMediaExtractor_readSampleData(
            extractor: *mut AMediaExtractor,
            buffer: *mut u8,
            capacity: usize,
        ) -> isize;
        fn AMediaExtractor_getSampleTime(extractor: *mut AMediaExtractor) -> i64;
        fn AMediaExtractor_advance(extractor: *mut AMediaExtractor) -> bool;
        fn AMediaExtractor_seekTo(
            extractor: *mut AMediaExtractor,
            seek_pos_us: i64,
            mode: c_int,
        ) -> c_int;
    }

    // `syscall(2)` from libc, used only for the `memfd_create` raw call so the
    // build does not depend on the API-30 `memfd_create` wrapper.
    unsafe extern "C" {
        fn syscall(number: c_long, ...) -> c_long;
    }

    /// Owns an `AMediaExtractor` and frees it on drop.
    pub struct Extractor(*mut AMediaExtractor);

    impl Extractor {
        /// Creates a new, empty extractor, or `None` when the NDK refuses.
        pub fn new() -> Option<Self> {
            // SAFETY: `AMediaExtractor_new` returns a fresh owned handle or null.
            let ptr = unsafe { AMediaExtractor_new() };
            if ptr.is_null() { None } else { Some(Self(ptr)) }
        }

        /// Points the extractor at `length` bytes of `fd` starting at `offset`.
        pub fn set_data_source_fd(&self, fd: RawFd, offset: i64, length: i64) -> c_int {
            // SAFETY: `self.0` is a live extractor and `fd` is an open, seekable
            // descriptor the caller keeps alive for the extractor's lifetime.
            unsafe { AMediaExtractor_setDataSourceFd(self.0, fd, offset, length) }
        }

        /// Number of tracks in the container.
        pub fn track_count(&self) -> usize {
            // SAFETY: `self.0` is a live extractor.
            unsafe { AMediaExtractor_getTrackCount(self.0) }
        }

        /// Takes ownership of a track's format, or `None` when unavailable.
        pub fn track_format(&self, index: usize) -> Option<MediaFormat> {
            // SAFETY: `self.0` is live and `index` is a valid track index.
            let ptr = unsafe { AMediaExtractor_getTrackFormat(self.0, index) };
            // SAFETY: the extractor transfers ownership of the format to the
            // caller; `MediaFormat`'s `Drop` deletes it exactly once.
            Some(unsafe { MediaFormat::from_ptr(NonNull::new(ptr.cast())?) })
        }

        /// Selects a track (idempotent).
        pub fn select_track(&self, index: usize) -> c_int {
            // SAFETY: `self.0` is live and `index` is a valid track index.
            unsafe { AMediaExtractor_selectTrack(self.0, index) }
        }

        /// Unselects a track (idempotent).
        pub fn unselect_track(&self, index: usize) -> c_int {
            // SAFETY: `self.0` is live and `index` is a valid track index.
            unsafe { AMediaExtractor_unselectTrack(self.0, index) }
        }

        /// Reads the next sample into `buffer`, returning its byte length or a
        /// negative value at EOF.
        pub fn read_sample_data(&self, buffer: &mut [MaybeUninit<u8>]) -> isize {
            let capacity = buffer.len();
            // SAFETY: `self.0` is live and `buffer` is a writable slice of at
            // least `capacity` bytes; the extractor writes at most `capacity`.
            unsafe { AMediaExtractor_readSampleData(self.0, buffer.as_mut_ptr().cast(), capacity) }
        }

        /// Presentation time of the current sample in microseconds.
        pub fn sample_time(&self) -> i64 {
            // SAFETY: `self.0` is a live extractor.
            unsafe { AMediaExtractor_getSampleTime(self.0) }
        }

        /// Advances to the next sample, returning `false` at EOF.
        pub fn advance(&self) -> bool {
            // SAFETY: `self.0` is a live extractor.
            unsafe { AMediaExtractor_advance(self.0) }
        }

        /// Seeks the extractor to `pos_us` in the given mode.
        pub fn seek_to(&self, pos_us: i64, mode: c_int) -> c_int {
            // SAFETY: `self.0` is a live extractor.
            unsafe { AMediaExtractor_seekTo(self.0, pos_us, mode) }
        }
    }

    impl Drop for Extractor {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: `self.0` came from `AMediaExtractor_new` and is freed once.
                unsafe {
                    AMediaExtractor_delete(self.0);
                }
            }
        }
    }

    /// Writes `bytes` to an anonymous `memfd` and returns the open file.
    pub fn memory_backed_file(bytes: &[u8]) -> io::Result<File> {
        // SAFETY: `syscall` is variadic; `MFD_CLOEXEC` is the documented flag
        // and the name is a valid NUL-terminated C string.
        let fd = unsafe { syscall(SYS_MEMFD_CREATE, c"krkr-movie".as_ptr(), MFD_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is a freshly created, owned descriptor.
        let mut file = unsafe { File::from_raw_fd(fd as RawFd) };
        file.write_all(bytes)?;
        Ok(file)
    }
}

/// Android backend: `ndk` for codec/format, [`extractor`] for demuxing.
#[cfg(target_os = "android")]
mod android {
    use std::ffi::c_int;
    use std::fmt;
    use std::fs::File;
    use std::os::unix::io::AsRawFd;
    use std::sync::Arc;
    use std::time::Duration;

    use api::error::MediaError;
    use api::media::{AudioSpec, DecodedAudio, RgbaFrame, VideoSpec};
    use ndk::media::media_codec::{
        BufferInfo, DequeuedInputBufferResult, DequeuedOutputBufferInfoResult, InputBuffer,
        MediaCodec, MediaCodecDirection, OutputBuffer,
    };
    use ndk::media::media_format::MediaFormat;

    use super::extractor::{self, AMEDIA_OK, AMEDIAEXTRACTOR_SEEK_PREVIOUS_SYNC, Extractor};
    use crate::Media;
    use crate::video::SEEK_TOLERANCE_MS;
    use crate::yuv::{CropRect, Yuv420Layout, YuvSampling, micros_to_millis, yuv420_to_rgba};

    /// `AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM`.
    const AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM: u32 = 4;

    /// `COLOR_FormatYUV420Flexible`.
    const COLOR_FORMAT_YUV420_FLEXIBLE: i32 = 0x7F42_0888;
    /// `COLOR_FormatYUV420Planar`.
    const COLOR_FORMAT_YUV420_PLANAR: i32 = 19;
    /// `COLOR_FormatYUV420PackedPlanar`.
    const COLOR_FORMAT_YUV420_PACKED_PLANAR: i32 = 20;
    /// `COLOR_FormatYUV420SemiPlanar`.
    const COLOR_FORMAT_YUV420_SEMI_PLANAR: i32 = 21;
    /// `COLOR_FormatYUV420PackedSemiPlanar`.
    const COLOR_FORMAT_YUV420_PACKED_SEMI_PLANAR: i32 = 39;

    /// `AudioFormat.ENCODING_PCM_16BIT`, the MediaCodec PCM default.
    const PCM_ENCODING_16BIT: i32 = 2;
    /// `AudioFormat.ENCODING_PCM_FLOAT`.
    const PCM_ENCODING_FLOAT: i32 = 4;

    /// How long output dequeue waits before another input sample is fed.
    const DECODE_TIMEOUT: Duration = Duration::from_micros(10_000);
    /// Safety valve so a wedged codec returns an error instead of hanging.
    const MAX_DECODE_STEPS: u32 = 100_000;

    /// Fallback frame duration when the container reports no frame rate.
    const DEFAULT_FRAME_DURATION_MS: i64 = 40;

    /// An `ndk` [`MediaCodec`] that stops itself on drop.
    ///
    /// `MediaCodec::drop` only calls `AMediaCodec_delete`; stopping first (when
    /// the codec was started) mirrors the reference backend's teardown.
    struct Codec {
        codec: MediaCodec,
        started: bool,
    }

    impl std::ops::Deref for Codec {
        type Target = MediaCodec;

        fn deref(&self) -> &MediaCodec {
            &self.codec
        }
    }

    impl Drop for Codec {
        fn drop(&mut self) {
            if self.started {
                let _ = self.codec.stop();
            }
        }
    }

    /// A track picked while scanning the container.
    struct PickedTrack {
        index: usize,
        mime: String,
        format: MediaFormat,
    }

    /// Per-video-stream decoder state.
    struct VideoState {
        codec: Codec,
        layout: Option<Yuv420Layout>,
        frame_duration_ms: i64,
        last_pts_ms: i64,
        input_done: bool,
        output_done: bool,
    }

    /// Per-audio-stream decoder state.
    struct AudioState {
        codec: Codec,
        out_rate: u32,
        out_channels: u16,
        pcm_encoding: i32,
        input_done: bool,
        output_done: bool,
    }

    /// Bottom-defined state for an incremental MediaCodec video decode.
    ///
    /// Holds the in-memory extractor over a `memfd`, the video and audio
    /// `ndk` codecs, the current frame and the fully decoded audio track. The
    /// middle layer owns this value and passes it back on every call; [`Media`]
    /// never stores it. It is opaque on purpose, so the backend implementation
    /// can change without an API break.
    ///
    /// Dropping the state stops/deletes every codec, deletes the extractor and
    /// the format handles (RAII). It is deliberately neither `Clone` nor
    /// `PartialEq`: it owns open codec contexts.
    #[non_exhaustive]
    pub struct VideoDecoderState {
        /// Display name used in error messages (the hint or `<video>`).
        name: String,
        /// The probed stream spec (video dimensions/fps plus audio parameters).
        spec: VideoSpec,
        /// The in-memory demuxer (`AMediaExtractor`).
        extractor: Extractor,
        /// Keeps the anonymous `memfd` alive while the extractor reads from it.
        _source: File,
        /// The opened video decoder.
        video: Option<VideoState>,
        /// The opened audio decoder.
        audio: Option<AudioState>,
        /// Demuxer track index of the video track.
        video_index: Option<usize>,
        /// Demuxer track index of the audio track.
        audio_index: Option<usize>,
        /// The fully decoded audio track (if the container has one).
        audio_pcm: Option<DecodedAudio>,
        /// The frame most recently returned by [`present`].
        current: Option<RgbaFrame>,
    }

    impl fmt::Debug for VideoDecoderState {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("VideoDecoderState")
                .field("spec", &self.spec)
                .finish_non_exhaustive()
        }
    }

    impl VideoDecoderState {
        /// Decode every audio sample into interleaved `f32`, once.
        fn decode_all_audio(&mut self) -> Result<(), MediaError> {
            let Some(audio_index) = self.audio_index else {
                return Ok(());
            };
            select_only(&self.extractor, Some(audio_index), self.video_index);
            let status = self
                .extractor
                .seek_to(0, AMEDIAEXTRACTOR_SEEK_PREVIOUS_SYNC);
            status_ok(status, &self.name, "AMediaExtractor_seekTo(audio)")?;

            let name = self.name.clone();
            let extractor = &self.extractor;
            let Some(audio) = self.audio.as_mut() else {
                return Ok(());
            };
            audio
                .codec
                .flush()
                .map_err(|error| decode_error(&name, format!("audio flush: {error}")))?;
            audio.input_done = false;
            audio.output_done = false;

            let mut samples: Vec<f32> = Vec::new();
            let mut out_rate = audio.out_rate;
            let mut out_channels = audio.out_channels;
            let mut encoding = audio.pcm_encoding;

            let mut steps = 0u32;
            loop {
                steps += 1;
                if steps > MAX_DECODE_STEPS {
                    return Err(decode_error(&name, "audio decode did not terminate"));
                }
                if audio.output_done {
                    break;
                }
                if !audio.input_done {
                    match audio.codec.dequeue_input_buffer(Duration::ZERO) {
                        Ok(DequeuedInputBufferResult::Buffer(input)) => {
                            if feed_audio_sample(&audio.codec, input, extractor, &name)? {
                                audio.input_done = true;
                            }
                            continue;
                        }
                        Ok(DequeuedInputBufferResult::TryAgainLater) => {}
                        Err(error) => {
                            return Err(decode_error(
                                &name,
                                format!("audio dequeueInputBuffer: {error}"),
                            ));
                        }
                    }
                }

                match audio.codec.dequeue_output_buffer(DECODE_TIMEOUT) {
                    Ok(DequeuedOutputBufferInfoResult::Buffer(output)) => {
                        let info = *output.info();
                        if info.flags() & AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM != 0 {
                            release(&audio.codec, output, &name)?;
                            audio.output_done = true;
                            continue;
                        }
                        if info.size() > 0 {
                            append_pcm(output_slice(&output, &info), encoding, &mut samples);
                        }
                        release(&audio.codec, output, &name)?;
                    }
                    Ok(DequeuedOutputBufferInfoResult::OutputFormatChanged) => {
                        let format = audio.codec.output_format();
                        if let Some(rate) = format.i32("sample-rate") {
                            out_rate = rate.max(0) as u32;
                        }
                        if let Some(channels) = format.i32("channel-count") {
                            out_channels = channels.max(0) as u16;
                        }
                        if let Some(value) = format.i32("pcm-encoding") {
                            encoding = value;
                        }
                    }
                    Ok(DequeuedOutputBufferInfoResult::OutputBuffersChanged)
                    | Ok(DequeuedOutputBufferInfoResult::TryAgainLater) => {}
                    Err(error) => {
                        return Err(decode_error(
                            &name,
                            format!("audio dequeueOutputBuffer: {error}"),
                        ));
                    }
                }
            }

            audio.out_rate = out_rate;
            audio.out_channels = out_channels;
            audio.pcm_encoding = encoding;
            let spec = AudioSpec {
                sample_rate: out_rate,
                channels: out_channels,
                total_frames: if out_channels > 0 {
                    Some(samples.len() as u64 / u64::from(out_channels))
                } else {
                    None
                },
            };
            self.audio_pcm = Some(DecodedAudio::new(spec, samples));
            Ok(())
        }
    }

    /// Opens a video stream and probes its [`VideoSpec`].
    ///
    /// Picks the first video and audio tracks, configures/starts an
    /// `ndk` [`MediaCodec`] decoder for each, decodes the audio track once, and
    /// returns the spec plus the caller-owned [`VideoDecoderState`]. A
    /// container with no video track, an unknown decoder or a malformed source
    /// returns a typed [`MediaError`].
    pub fn open(
        m: &Media,
        bytes: Arc<[u8]>,
        hint: Option<&str>,
    ) -> Result<(VideoSpec, VideoDecoderState), MediaError> {
        let _ = m;
        let name = display_name(hint).to_string();
        let source = extractor::memory_backed_file(&bytes).map_err(|error| MediaError::Io {
            message: format!("{name}: {error}"),
        })?;

        let extractor = Extractor::new().ok_or_else(|| MediaError::Unsupported {
            message: format!("{name}: AMediaExtractor_new returned null"),
        })?;
        let status = extractor.set_data_source_fd(source.as_raw_fd(), 0, bytes.len() as i64);
        status_ok(status, &name, "AMediaExtractor_setDataSourceFd")?;

        let (video_track, audio_track) = collect_tracks(&extractor)?;
        let Some(mut video_track) = video_track else {
            return Err(MediaError::Unsupported {
                message: format!("{name}: no video stream"),
            });
        };

        // MediaCodec extractors often omit `frame-rate`/`frame-count`, which
        // FFmpeg gets from the stream index. Walk the sample table once to
        // recover the real frame count and average frame rate; the first
        // `present` re-seeks before decoding.
        let (scanned_frames, scanned_fps) = scan_video_timing(&extractor, video_track.index);
        let spec = build_spec(
            &name,
            &video_track,
            audio_track.as_ref(),
            scanned_frames,
            scanned_fps,
        )?;

        let video = VideoState {
            codec: create_video_codec(&mut video_track, &name)?,
            layout: None,
            frame_duration_ms: frame_duration_ms(f64::from(spec.fps)),
            last_pts_ms: 0,
            input_done: false,
            output_done: false,
        };
        let audio = match audio_track.as_ref() {
            Some(track) => Some(AudioState {
                codec: create_audio_codec(track, &name)?,
                out_rate: track.format.i32("sample-rate").unwrap_or(0).max(0) as u32,
                out_channels: track.format.i32("channel-count").unwrap_or(0).max(0) as u16,
                pcm_encoding: PCM_ENCODING_16BIT,
                input_done: false,
                output_done: false,
            }),
            None => None,
        };

        let mut state = VideoDecoderState {
            name,
            spec,
            extractor,
            _source: source,
            video: Some(video),
            audio,
            video_index: Some(video_track.index),
            audio_index: audio_track.as_ref().map(|track| track.index),
            audio_pcm: None,
            current: None,
        };

        // Decode the whole audio track up front (matches the FFmpeg backend),
        // then leave the extractor for the first `present`, which always seeks.
        if state.audio.is_some() {
            state.decode_all_audio()?;
        }
        Ok((spec, state))
    }

    /// Presents the frame at (or after) `at_ms`.
    ///
    /// Mirrors the FFmpeg backend: a request within `SEEK_TOLERANCE_MS` behind
    /// the current frame is served from that frame; a further-back request
    /// seeks the extractor and flushes the decoder; otherwise the decoder
    /// decodes forward to the first frame at/after `at_ms`. Returns a tightly
    /// packed RGBA8 [`RgbaFrame`].
    pub fn present(
        m: &Media,
        state: &mut VideoDecoderState,
        at_ms: u64,
    ) -> Result<RgbaFrame, MediaError> {
        let _ = m;

        if let Some(current) = &state.current
            && at_ms <= current.pts_ms
            && at_ms.saturating_add(SEEK_TOLERANCE_MS) >= current.pts_ms
        {
            return Ok(current.clone());
        }

        let need_seek = match &state.current {
            Some(current) => at_ms.saturating_add(SEEK_TOLERANCE_MS) < current.pts_ms,
            None => true,
        };
        if need_seek {
            seek_to(state, at_ms)?;
        }

        loop {
            match decode_next(state)? {
                Some(frame) => {
                    if frame.pts_ms >= at_ms {
                        state.current = Some(frame.clone());
                        return Ok(frame);
                    }
                    state.current = Some(frame);
                }
                None => return state.current.clone().ok_or(MediaError::EndOfStream),
            }
        }
    }

    /// Returns the decoded audio track, if the container has one.
    pub fn audio(m: &Media, state: &VideoDecoderState) -> Result<DecodedAudio, MediaError> {
        let _ = m;
        state
            .audio_pcm
            .clone()
            .ok_or_else(|| MediaError::Unsupported {
                message: format!("{}: no audio track", state.name),
            })
    }

    /// Seeks the extractor to `at_ms` and flushes the video decoder.
    fn seek_to(state: &mut VideoDecoderState, at_ms: u64) -> Result<(), MediaError> {
        select_only(&state.extractor, state.video_index, state.audio_index);
        let timestamp_us = seek_micros(at_ms, &state.name)?;
        let status = state
            .extractor
            .seek_to(timestamp_us, AMEDIAEXTRACTOR_SEEK_PREVIOUS_SYNC);
        status_ok(status, &state.name, "AMediaExtractor_seekTo(video)")?;
        if let Some(video) = state.video.as_mut() {
            video
                .codec
                .flush()
                .map_err(|error| decode_error(&state.name, format!("video flush: {error}")))?;
            video.input_done = false;
            video.output_done = false;
            video.last_pts_ms = 0;
        }
        state.current = None;
        Ok(())
    }

    /// Decodes the next video frame, feeding samples until the decoder produces
    /// a frame. Returns `None` at end of stream.
    fn decode_next(state: &mut VideoDecoderState) -> Result<Option<RgbaFrame>, MediaError> {
        let VideoDecoderState {
            name,
            extractor,
            video,
            ..
        } = state;
        let Some(video) = video.as_mut() else {
            return Ok(None);
        };
        decode_next_video(extractor, video, name)
    }

    /// Run the video decoder one step at a time until it yields a frame or EOF.
    ///
    /// Output is drained first; when no output is ready the next input sample
    /// is queued, and output dequeue waits a short while so a lagging decoder
    /// does not spin the CPU.
    fn decode_next_video(
        extractor: &Extractor,
        video: &mut VideoState,
        name: &str,
    ) -> Result<Option<RgbaFrame>, MediaError> {
        let mut steps = 0u32;
        loop {
            steps += 1;
            if steps > MAX_DECODE_STEPS {
                return Err(decode_error(name, "video decode did not terminate"));
            }
            if video.output_done {
                return Ok(None);
            }
            if !video.input_done {
                match video.codec.dequeue_input_buffer(Duration::ZERO) {
                    Ok(DequeuedInputBufferResult::Buffer(input)) => {
                        if feed_video_sample(&video.codec, input, extractor, name)? {
                            video.input_done = true;
                        }
                        continue;
                    }
                    Ok(DequeuedInputBufferResult::TryAgainLater) => {}
                    Err(error) => {
                        return Err(decode_error(
                            name,
                            format!("video dequeueInputBuffer: {error}"),
                        ));
                    }
                }
            }

            match video.codec.dequeue_output_buffer(DECODE_TIMEOUT) {
                Ok(DequeuedOutputBufferInfoResult::Buffer(output)) => {
                    let info = *output.info();
                    if info.flags() & AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM != 0 {
                        release(&video.codec, output, name)?;
                        video.output_done = true;
                        return Ok(None);
                    }
                    if info.size() > 0
                        && let Some(layout) = video.layout
                    {
                        let rgba = yuv420_to_rgba(output_slice(&output, &info), &layout);
                        let fallback = video.last_pts_ms.saturating_add(video.frame_duration_ms);
                        let pts_ms = if info.presentation_time_us() >= 0 {
                            micros_to_millis(info.presentation_time_us())
                        } else {
                            fallback
                        };
                        video.last_pts_ms = pts_ms;
                        release(&video.codec, output, name)?;
                        return Ok(Some(RgbaFrame::new(
                            layout.width() as u32,
                            layout.height() as u32,
                            pts_millis(pts_ms, name)?,
                            rgba,
                        )));
                    }
                    release(&video.codec, output, name)?;
                }
                Ok(DequeuedOutputBufferInfoResult::OutputFormatChanged) => {
                    let format = video.codec.output_format();
                    video.layout = Some(parse_video_output(&format));
                }
                Ok(DequeuedOutputBufferInfoResult::OutputBuffersChanged)
                | Ok(DequeuedOutputBufferInfoResult::TryAgainLater) => {}
                Err(error) => {
                    return Err(decode_error(
                        name,
                        format!("video dequeueOutputBuffer: {error}"),
                    ));
                }
            }
        }
    }

    /// Move one sample from the extractor into the video decoder.
    ///
    /// Returns `true` once the end-of-stream flag was queued.
    fn feed_video_sample(
        codec: &MediaCodec,
        mut input: InputBuffer<'_>,
        extractor: &Extractor,
        name: &str,
    ) -> Result<bool, MediaError> {
        let read = {
            let buffer = input.buffer_mut();
            extractor.read_sample_data(buffer)
        };
        if read < 0 {
            codec
                .queue_input_buffer(input, 0, 0, 0, AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM)
                .map_err(|error| decode_error(name, format!("queue video EOS: {error}")))?;
            Ok(true)
        } else {
            let pts = extractor.sample_time();
            let time = if pts < 0 { 0 } else { pts as u64 };
            codec
                .queue_input_buffer(input, 0, read as usize, time, 0)
                .map_err(|error| decode_error(name, format!("queue video sample: {error}")))?;
            extractor.advance();
            Ok(false)
        }
    }

    /// Move one sample from the extractor into the audio decoder.
    ///
    /// Returns `true` once the end-of-stream flag was queued.
    fn feed_audio_sample(
        codec: &MediaCodec,
        mut input: InputBuffer<'_>,
        extractor: &Extractor,
        name: &str,
    ) -> Result<bool, MediaError> {
        let read = {
            let buffer = input.buffer_mut();
            extractor.read_sample_data(buffer)
        };
        if read < 0 {
            codec
                .queue_input_buffer(input, 0, 0, 0, AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM)
                .map_err(|error| decode_error(name, format!("queue audio EOS: {error}")))?;
            Ok(true)
        } else {
            let pts = extractor.sample_time();
            let time = if pts < 0 { 0 } else { pts as u64 };
            codec
                .queue_input_buffer(input, 0, read as usize, time, 0)
                .map_err(|error| decode_error(name, format!("queue audio sample: {error}")))?;
            extractor.advance();
            Ok(false)
        }
    }

    /// Release a dequeued output buffer without rendering it to a surface.
    fn release(codec: &MediaCodec, output: OutputBuffer<'_>, name: &str) -> Result<(), MediaError> {
        codec
            .release_output_buffer(output, false)
            .map_err(|error| decode_error(name, format!("release output buffer: {error}")))
    }

    /// Borrow the valid bytes of a dequeued output buffer (`info.offset`/`size`).
    fn output_slice<'a>(output: &'a OutputBuffer<'_>, info: &BufferInfo) -> &'a [u8] {
        let buffer = output.buffer();
        let start = (info.offset().max(0) as usize).min(buffer.len());
        let end = start
            .saturating_add(info.size().max(0) as usize)
            .min(buffer.len());
        &buffer[start..end]
    }

    /// Translate the codec's YUV420 output format into plane geometry.
    fn parse_video_output(format: &MediaFormat) -> Yuv420Layout {
        let mut width = format.i32("width").unwrap_or(0).max(0) as usize;
        let mut height = format.i32("height").unwrap_or(0).max(0) as usize;
        let color_format = format
            .i32("color-format")
            .unwrap_or(COLOR_FORMAT_YUV420_FLEXIBLE);
        let sampling = sampling_for(color_format);

        let stride = format
            .i32("stride")
            .map(|value| value.max(0) as usize)
            .filter(|value| *value >= width)
            .unwrap_or(width);
        let slice_height = format
            .i32("slice-height")
            .map(|value| value.max(0) as usize)
            .filter(|value| *value >= height)
            .unwrap_or(height);
        if width == 0 {
            width = stride.max(1);
        }
        if height == 0 {
            height = slice_height.max(1);
        }

        let crop = resolve_crop(format, width, height);
        Yuv420Layout::new(stride, slice_height, sampling, crop)
    }

    /// Pick the chroma layout for a MediaCodec color-format value.
    ///
    /// `COLOR_FormatYUV420Flexible` is meant to be resolved through
    /// `AMediaCodec_getOutputImage`, which `ndk`/the NDK does not expose for a
    /// byte buffer. Codecs almost always report a concrete format in the
    /// output format; if one reports the flexible value verbatim the planar
    /// layout is assumed (what the software AVC decoder uses). This is the one
    /// guess in this file, reached only when the format is genuinely ambiguous.
    fn sampling_for(color_format: i32) -> YuvSampling {
        match color_format {
            COLOR_FORMAT_YUV420_PLANAR | COLOR_FORMAT_YUV420_PACKED_PLANAR => YuvSampling::Planar,
            COLOR_FORMAT_YUV420_SEMI_PLANAR | COLOR_FORMAT_YUV420_PACKED_SEMI_PLANAR => {
                YuvSampling::SemiPlanar
            }
            _ => YuvSampling::Planar,
        }
    }

    /// Resolve the crop rectangle, tolerating both the exclusive
    /// (`right - left`) and the historically inclusive (`right - left + 1`)
    /// conventions.
    fn resolve_crop(format: &MediaFormat, width: usize, height: usize) -> CropRect {
        let left = non_negative(format.i32("crop-left"));
        let top = non_negative(format.i32("crop-top"));
        let right = non_negative(format.i32("crop-right"));
        let bottom = non_negative(format.i32("crop-bottom"));

        if right <= left || bottom <= top {
            return CropRect::full(width, height);
        }

        // MediaCodec has shipped both conventions. Whichever reproduces the
        // reported display size is the one this codec meant.
        let right = match right - left {
            span if span == width => right,
            span if span + 1 == width => right + 1,
            _ => left + width,
        };
        let bottom = match bottom - top {
            span if span == height => bottom,
            span if span + 1 == height => bottom + 1,
            _ => top + height,
        };
        CropRect {
            left,
            top,
            right,
            bottom,
        }
    }

    /// Scan the container and keep the first video and audio tracks.
    fn collect_tracks(
        extractor: &Extractor,
    ) -> Result<(Option<PickedTrack>, Option<PickedTrack>), MediaError> {
        let count = extractor.track_count();
        let mut video = None;
        let mut audio = None;
        for index in 0..count {
            let Some(mut format) = extractor.track_format(index) else {
                continue;
            };
            let Some(mime) = format.str("mime").map(str::to_owned) else {
                continue;
            };
            if mime.starts_with("video/") {
                if video.is_none() {
                    video = Some(PickedTrack {
                        index,
                        mime,
                        format,
                    });
                }
            } else if mime.starts_with("audio/") && audio.is_none() {
                audio = Some(PickedTrack {
                    index,
                    mime,
                    format,
                });
            }
        }
        Ok((video, audio))
    }

    /// Walk the video sample table once to recover the frame count and average
    /// frame rate.
    ///
    /// `AMediaExtractor` exposes no `nb_frames`/`avg_frame_rate`, and the
    /// `frame-rate`/`frame-count` keys are frequently absent, so this mirrors
    /// what FFmpeg reads from the stream index. It only advances the cursor and
    /// reads sample times (no decode); the caller re-seeks before decoding.
    fn scan_video_timing(extractor: &Extractor, video_index: usize) -> (i64, f64) {
        // Only the video track matters; unselecting everything else keeps the
        // cursor stepping frame to frame.
        let count = extractor.track_count();
        for index in 0..count {
            if index != video_index {
                extractor.unselect_track(index);
            }
        }
        extractor.select_track(video_index);
        let status = extractor.seek_to(0, AMEDIAEXTRACTOR_SEEK_PREVIOUS_SYNC);
        if status != AMEDIA_OK {
            return (0, 0.0);
        }

        let mut frames = 0i64;
        let mut first_us = i64::MIN;
        let mut last_us = i64::MIN;
        // A safety cap far above any real movie; the loop also stops at EOF.
        const MAX_SAMPLES: i64 = 10_000_000;
        loop {
            let time = extractor.sample_time();
            if time >= 0 {
                if first_us == i64::MIN {
                    first_us = time;
                }
                last_us = time;
                frames += 1;
            }
            if !extractor.advance() || frames >= MAX_SAMPLES {
                break;
            }
        }

        let fps = if frames > 1 && last_us > first_us {
            (frames - 1) as f64 * 1_000_000.0 / (last_us - first_us) as f64
        } else {
            0.0
        };
        (frames, fps)
    }

    /// Build the [`VideoSpec`] from the container metadata.
    ///
    /// `scanned_frames`/`scanned_fps` come from [`scan_video_timing`] and are
    /// preferred over the format keys, which are frequently absent.
    fn build_spec(
        name: &str,
        video: &PickedTrack,
        audio: Option<&PickedTrack>,
        scanned_frames: i64,
        scanned_fps: f64,
    ) -> Result<VideoSpec, MediaError> {
        let width = video.format.i32("width").unwrap_or(0).max(0) as u32;
        let height = video.format.i32("height").unwrap_or(0).max(0) as u32;
        if width == 0 || height == 0 {
            return Err(MediaError::InvalidData {
                message: format!("{name}: video dimensions are not known"),
            });
        }

        let format_fps = video.format.f32("frame-rate").map_or(0.0, f64::from);
        let fps = if scanned_fps > 0.0 {
            scanned_fps
        } else {
            format_fps
        };
        let duration_us = video.format.i64("duration").unwrap_or(0);
        let total_time_ms = if duration_us > 0 {
            micros_to_millis(duration_us)
        } else {
            0
        };
        let declared_frames = video.format.i32("frame-count").unwrap_or(0);
        let total_frames = if scanned_frames > 0 {
            Some(scanned_frames as u64)
        } else if declared_frames > 0 {
            Some(u64::from(declared_frames as u32))
        } else if fps > 0.0 && total_time_ms > 0 {
            Some((total_time_ms as f64 * fps / 1000.0).round() as u64)
        } else {
            None
        };
        let sample_rate = audio
            .and_then(|track| track.format.i32("sample-rate"))
            .unwrap_or(0)
            .max(0) as u32;
        let channels = audio
            .and_then(|track| track.format.i32("channel-count"))
            .unwrap_or(0)
            .max(0) as u16;
        // This backend opens a single video and a single (optional) audio
        // track, so the counts are the tracks it actually decodes.
        let audio_streams = if audio.is_some() { 1 } else { 0 };
        let audio_stream = if audio.is_some() { 0 } else { -1 };

        Ok(VideoSpec {
            width,
            height,
            fps: fps as f32,
            total_frames,
            sample_rate,
            channels,
            audio_streams,
            video_streams: 1,
            audio_stream,
            video_stream: 0,
        })
    }

    /// Create, configure (for a byte-buffer YUV420 output) and start the video
    /// decoder.
    fn create_video_codec(track: &mut PickedTrack, name: &str) -> Result<Codec, MediaError> {
        let codec =
            MediaCodec::from_decoder_type(&track.mime).ok_or_else(|| MediaError::Unsupported {
                message: format!("{name}: no MediaCodec decoder for {}", track.mime),
            })?;
        // Request an uncompressed YUV420 byte-buffer output (no Surface).
        track
            .format
            .set_i32("color-format", COLOR_FORMAT_YUV420_FLEXIBLE);
        codec
            .configure(&track.format, None, MediaCodecDirection::Decoder)
            .map_err(|error| decode_error(name, format!("configure(video): {error}")))?;
        codec
            .start()
            .map_err(|error| decode_error(name, format!("start(video): {error}")))?;
        Ok(Codec {
            codec,
            started: true,
        })
    }

    /// Create, configure and start the audio decoder.
    fn create_audio_codec(track: &PickedTrack, name: &str) -> Result<Codec, MediaError> {
        let codec =
            MediaCodec::from_decoder_type(&track.mime).ok_or_else(|| MediaError::Unsupported {
                message: format!("{name}: no MediaCodec decoder for {}", track.mime),
            })?;
        codec
            .configure(&track.format, None, MediaCodecDirection::Decoder)
            .map_err(|error| decode_error(name, format!("configure(audio): {error}")))?;
        codec
            .start()
            .map_err(|error| decode_error(name, format!("start(audio): {error}")))?;
        Ok(Codec {
            codec,
            started: true,
        })
    }

    /// Select the track we need and unselect the other one we know about.
    fn select_only(extractor: &Extractor, wanted: Option<usize>, unwanted: Option<usize>) {
        if let Some(index) = unwanted
            && Some(index) != wanted
        {
            extractor.unselect_track(index);
        }
        if let Some(index) = wanted {
            extractor.select_track(index);
        }
    }

    /// Append PCM bytes as interleaved `f32` samples.
    fn append_pcm(bytes: &[u8], encoding: i32, out: &mut Vec<f32>) {
        if encoding == PCM_ENCODING_FLOAT {
            for chunk in bytes.chunks_exact(4) {
                out.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }
        } else {
            for chunk in bytes.chunks_exact(2) {
                let sample = i16::from_le_bytes([chunk[0], chunk[1]]);
                out.push(f32::from(sample) / 32768.0);
            }
        }
    }

    /// Map a `media_status_t` to a `Result`, describing the failure.
    fn status_ok(status: c_int, name: &str, what: &str) -> Result<(), MediaError> {
        if status == AMEDIA_OK {
            Ok(())
        } else {
            Err(decode_error(
                name,
                format!("{what}: MediaCodec error {status}"),
            ))
        }
    }

    /// Builds the typed decode error for this stream.
    fn decode_error(name: &str, message: impl fmt::Display) -> MediaError {
        MediaError::Decode {
            format: String::new(),
            message: format!("{name}: {message}"),
        }
    }

    /// Returns the hint used in error messages.
    fn display_name(hint: Option<&str>) -> &str {
        hint.filter(|name| !name.is_empty()).unwrap_or("<video>")
    }

    /// Interpreting `None` as zero.
    fn non_negative(value: Option<i32>) -> usize {
        value.unwrap_or(0).max(0) as usize
    }

    /// Nominal frame duration in milliseconds, with a safe fallback.
    fn frame_duration_ms(fps: f64) -> i64 {
        if fps > 0.0 {
            (1000.0 / fps).round().max(1.0) as i64
        } else {
            DEFAULT_FRAME_DURATION_MS
        }
    }

    /// Converts a seek target in milliseconds to the microseconds
    /// `AMediaExtractor_seekTo` takes.
    fn seek_micros(at_ms: u64, name: &str) -> Result<i64, MediaError> {
        at_ms
            .checked_mul(1000)
            .and_then(|micros| i64::try_from(micros).ok())
            .ok_or_else(|| MediaError::InvalidData {
                message: format!("{name}: seek target {at_ms} ms is out of range"),
            })
    }

    /// Converts a decoded presentation time in milliseconds to the `u64` the
    /// frame carries.
    fn pts_millis(pts_ms: i64, name: &str) -> Result<u64, MediaError> {
        u64::try_from(pts_ms).map_err(|_| MediaError::InvalidData {
            message: format!("{name}: presentation time {pts_ms} ms is out of range"),
        })
    }
}
