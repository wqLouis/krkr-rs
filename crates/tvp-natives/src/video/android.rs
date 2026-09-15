//! Android MediaCodec-backed movie decoding (compiled only when the crate is
//! built for Android without the `ffmpeg` feature).
//!
//! The public API and its semantics are identical to the FFmpeg backend in
//! [`super::ffmpeg`]: `MovieDecoder::open` takes an in-memory byte buffer,
//! [`MovieDecoder::present_at`] implements the same "seek backwards, otherwise
//! keep decoding forward" contract, and [`MovieDecoder::audio_pcm`] decodes
//! the whole track once to interleaved `f32`.
//!
//! Demuxing uses `AMediaExtractor` and decoding uses `AMediaCodec` from
//! `libmediandk` (NDK, API 21+). No JVM/JNI is involved in decoding.
//!
//! # Feeding an in-memory buffer to `AMediaExtractor`
//!
//! The XP3 archive hands the engine a `Vec<u8>`, but the API-21 extractor only
//! accepts a *file descriptor* (the custom `AMediaDataSource` callbacks are
//! API 28+, and the build targets API 21). So the bytes are written to an
//! anonymous `memfd` (`memfd_create`, Linux 3.17+/Android kernel 4.x) and the
//! resulting fd is passed to `AMediaExtractor_setDataSourceFd`. Nothing is
//! written to the filesystem and no JNI is needed. If the kernel refuses the
//! syscall, `open` returns a descriptive error instead of falling back to a
//! path the app cannot compute.
//!
//! # What is verified, and what is not
//!
//! * **Host-tested:** the YUV420 → RGBA conversion and the µs → ms rescale,
//!   including stride/slice-height/crop handling and both planar (I420) and
//!   semi-planar (NV12) chroma layouts (see [`super::yuv`]).
//! * **Compile-verified:** this whole module cross-compiles for
//!   `aarch64-linux-android` with the NDK toolchain, and its FFI signatures
//!   are the ones declared by `NdkMediaExtractor.h`/`NdkMediaCodec.h`.
//! * **Unverified here:** *nothing about MediaCodec has been run.* There is no
//!   Android device or emulator on this machine, so the demux/decode loop,
//!   the `memfd` trick, seeking, the colour-format negotiation and the exact
//!   output stride/crop the codec reports are all untested at runtime. The
//!   layout fallback (when a codec reports a bare `COLOR_FormatYUV420Flexible`
//!   without resolving it) is a documented guess, not a measurement.
//!
//! # Known deviations from the FFmpeg backend
//!
//! * `MovieMetadata::fps` / `total_frames` can only be populated when the
//!   container exposes `frame-rate` / `frame-count` extractor keys; MediaCodec
//!   has no `AVStream::nb_frames` and no `avg_frame_rate` fallback, so a
//!   container that omits them yields `fps = 0` and a derived (or zero) frame
//!   count, where FFmpeg might still report a value.
//! * The decoded frame is the cropped display rectangle (`crop-*`), so its
//!   dimensions can be smaller than the coded `width`/`height` where FFmpeg
//!   uses the decoder's frame size.

use std::ffi::{CStr, CString, c_char, c_int, c_long, c_void};
use std::fs::File;
use std::io::{self, Write};
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};

use super::init;
use super::yuv::{CropRect, Yuv420Layout, YuvSampling, micros_to_millis, yuv420_to_rgba};
use super::{DecodedAudioPcm, MovieMetadata, RgbaFrame};

/// `AMEDIA_OK` from `NdkMediaError.h`.
const AMEDIA_OK: c_int = 0;

/// `AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM`.
const AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM: u32 = 4;
/// `AMEDIACODEC_INFO_OUTPUT_BUFFERS_CHANGED`.
const AMEDIACODEC_INFO_OUTPUT_BUFFERS_CHANGED: isize = -3;
/// `AMEDIACODEC_INFO_OUTPUT_FORMAT_CHANGED`.
const AMEDIACODEC_INFO_OUTPUT_FORMAT_CHANGED: isize = -2;
/// `AMEDIACODEC_INFO_TRY_AGAIN_LATER`.
const AMEDIACODEC_INFO_TRY_AGAIN_LATER: isize = -1;

/// `AMEDIAEXTRACTOR_SEEK_PREVIOUS_SYNC`.
const AMEDIAEXTRACTOR_SEEK_PREVIOUS_SYNC: c_int = 0;

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

/// How long `dequeueOutputBuffer` waits before we feed more input.
const DECODE_TIMEOUT_US: i64 = 10_000;
/// Safety valve so a wedged codec returns an error instead of hanging.
const MAX_DECODE_STEPS: u32 = 100_000;

/// `SYS_memfd_create` with the `arm64` syscall number (the only ABI built).
const SYS_MEMFD_CREATE: c_long = 279;
/// `MFD_CLOEXEC`.
const MFD_CLOEXEC: u32 = 1;

/// Fallback frame duration when the container reports no frame rate (matches
/// the FFmpeg backend).
const DEFAULT_FRAME_DURATION_MS: i64 = 40;

/// Seeking this far before the current frame triggers a real seek; smaller
/// backwards steps are satisfied from the cached frame (matches FFmpeg).
const SEEK_TOLERANCE_MS: i64 = 2;

#[repr(C)]
struct AMediaExtractor {
    _private: [u8; 0],
}

#[repr(C)]
struct AMediaCodec {
    _private: [u8; 0],
}

#[repr(C)]
struct AMediaFormat {
    _private: [u8; 0],
}

/// Layout of `AMediaCodecBufferInfo` from `NdkMediaCodec.h`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AMediaCodecBufferInfo {
    offset: i32,
    size: i32,
    presentation_time_us: i64,
    flags: u32,
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
    fn AMediaExtractor_getTrackFormat(
        extractor: *mut AMediaExtractor,
        index: usize,
    ) -> *mut AMediaFormat;
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

    fn AMediaFormat_delete(format: *mut AMediaFormat) -> c_int;
    fn AMediaFormat_getInt32(format: *mut AMediaFormat, name: *const c_char, out: *mut i32)
    -> bool;
    fn AMediaFormat_getInt64(format: *mut AMediaFormat, name: *const c_char, out: *mut i64)
    -> bool;
    fn AMediaFormat_getFloat(format: *mut AMediaFormat, name: *const c_char, out: *mut f32)
    -> bool;
    fn AMediaFormat_getString(
        format: *mut AMediaFormat,
        name: *const c_char,
        out: *mut *const c_char,
    ) -> bool;
    fn AMediaFormat_setInt32(format: *mut AMediaFormat, name: *const c_char, value: i32);

    fn AMediaCodec_createDecoderByType(mime: *const c_char) -> *mut AMediaCodec;
    fn AMediaCodec_delete(codec: *mut AMediaCodec) -> c_int;
    fn AMediaCodec_configure(
        codec: *mut AMediaCodec,
        format: *const AMediaFormat,
        surface: *mut c_void,
        crypto: *mut c_void,
        flags: u32,
    ) -> c_int;
    fn AMediaCodec_start(codec: *mut AMediaCodec) -> c_int;
    fn AMediaCodec_stop(codec: *mut AMediaCodec) -> c_int;
    fn AMediaCodec_flush(codec: *mut AMediaCodec) -> c_int;
    fn AMediaCodec_getInputBuffer(
        codec: *mut AMediaCodec,
        index: usize,
        out_size: *mut usize,
    ) -> *mut u8;
    fn AMediaCodec_getOutputBuffer(
        codec: *mut AMediaCodec,
        index: usize,
        out_size: *mut usize,
    ) -> *mut u8;
    fn AMediaCodec_dequeueInputBuffer(codec: *mut AMediaCodec, timeout_us: i64) -> isize;
    fn AMediaCodec_queueInputBuffer(
        codec: *mut AMediaCodec,
        index: usize,
        offset: i64,
        size: usize,
        time: u64,
        flags: u32,
    ) -> c_int;
    fn AMediaCodec_dequeueOutputBuffer(
        codec: *mut AMediaCodec,
        info: *mut AMediaCodecBufferInfo,
        timeout_us: i64,
    ) -> isize;
    fn AMediaCodec_getOutputFormat(codec: *mut AMediaCodec) -> *mut AMediaFormat;
    fn AMediaCodec_releaseOutputBuffer(
        codec: *mut AMediaCodec,
        index: usize,
        render: bool,
    ) -> c_int;
}

// `syscall(2)` from libc, used only for the `memfd_create` raw call so the
// build does not depend on the API-30 `memfd_create` wrapper.
unsafe extern "C" {
    fn syscall(number: c_long, ...) -> c_long;
}

/// Owns an `AMediaExtractor` and frees it on drop.
struct Extractor(*mut AMediaExtractor);

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

/// Owns an `AMediaFormat` and frees it on drop.
struct Format(*mut AMediaFormat);

impl Drop for Format {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `self.0` came from an NDK call that transfers ownership.
            unsafe {
                AMediaFormat_delete(self.0);
            }
        }
    }
}

/// Owns an `AMediaCodec`, stopping it (if started) and freeing it on drop.
struct Codec {
    ptr: *mut AMediaCodec,
    started: bool,
}

impl Drop for Codec {
    fn drop(&mut self) {
        if self.ptr.is_null() {
            return;
        }
        // SAFETY: `self.ptr` is a live codec; `stop`/`delete` are each called
        // once and `delete` frees the handle.
        unsafe {
            if self.started {
                AMediaCodec_stop(self.ptr);
            }
            AMediaCodec_delete(self.ptr);
        }
    }
}

/// A track picked while scanning the container.
struct PickedTrack {
    index: usize,
    mime: String,
    format: Format,
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

/// Owns the extractor and the video/audio decoders for one open movie.
pub struct MovieDecoder {
    extractor: Extractor,
    /// Keeps the anonymous `memfd` alive while the extractor reads from it.
    _source: File,
    metadata: MovieMetadata,
    video: Option<VideoState>,
    audio: Option<AudioState>,
    video_index: Option<usize>,
    audio_index: Option<usize>,
    audio_pcm: Option<DecodedAudioPcm>,
    /// Last frame selected for presentation.
    current: Option<RgbaFrame>,
    /// A frame decoded ahead of the requested position (matches FFmpeg).
    pending: Option<RgbaFrame>,
}

impl MovieDecoder {
    /// Open a movie from an in-memory byte buffer.
    pub fn open(bytes: Vec<u8>) -> Result<Self, String> {
        init()?;
        let source = memory_backed_file(&bytes)?;
        let extractor_ptr = unsafe { AMediaExtractor_new() };
        if extractor_ptr.is_null() {
            return Err("AMediaExtractor_new returned null".to_string());
        }
        let extractor = Extractor(extractor_ptr);
        let status = unsafe {
            AMediaExtractor_setDataSourceFd(
                extractor_ptr,
                source.as_raw_fd(),
                0,
                bytes.len() as i64,
            )
        };
        status_ok(status, "AMediaExtractor_setDataSourceFd")?;

        let (video_streams, audio_streams, video_track, audio_track) =
            collect_tracks(extractor_ptr)?;
        let metadata = build_metadata(
            video_track.as_ref(),
            audio_track.as_ref(),
            video_streams,
            audio_streams,
        );

        let video = match video_track.as_ref() {
            Some(track) => Some(VideoState {
                codec: create_video_codec(track)?,
                layout: None,
                frame_duration_ms: frame_duration_ms(metadata.fps),
                last_pts_ms: 0,
                input_done: false,
                output_done: false,
            }),
            None => None,
        };
        let audio = match audio_track.as_ref() {
            Some(track) => Some(AudioState {
                codec: create_audio_codec(track)?,
                out_rate: track_int32(track, c"sample-rate").unwrap_or(0).max(0) as u32,
                out_channels: track_int32(track, c"channel-count").unwrap_or(0).max(0) as u16,
                pcm_encoding: PCM_ENCODING_16BIT,
                input_done: false,
                output_done: false,
            }),
            None => None,
        };

        let mut decoder = MovieDecoder {
            extractor,
            _source: source,
            metadata,
            video,
            audio,
            video_index: video_track.as_ref().map(|t| t.index),
            audio_index: audio_track.as_ref().map(|t| t.index),
            audio_pcm: None,
            current: None,
            pending: None,
        };

        // Decode the whole audio track up front (effect movies are short),
        // then rewind so video presentation starts cleanly — the same order
        // the FFmpeg backend uses.
        if decoder.audio.is_some()
            && let Err(e) = decoder.decode_all_audio()
        {
            log::warn!("video: MediaCodec audio decode failed: {e}");
        }
        decoder.reset_to_start()?;
        if let Err(e) = decoder.present_at(0) {
            log::warn!("video: MediaCodec could not decode/present the first frame: {e}");
        }
        Ok(decoder)
    }

    /// Container metadata.
    pub fn metadata(&self) -> &MovieMetadata {
        &self.metadata
    }

    /// The most recently presented frame, if any.
    pub fn current_frame(&self) -> Option<&RgbaFrame> {
        self.current.as_ref()
    }

    /// The cached whole-track PCM, if it has been decoded.
    pub fn cached_audio(&self) -> Option<&DecodedAudioPcm> {
        self.audio_pcm.as_ref()
    }

    /// Decode (once) and return the whole audio track.
    pub fn audio_pcm(&mut self) -> Result<&DecodedAudioPcm, String> {
        if self.audio_pcm.is_none() {
            self.decode_all_audio()?;
            self.reset_to_start()?;
        }
        self.audio_pcm
            .as_ref()
            .ok_or_else(|| "no audio track".to_string())
    }

    /// Present the frame covering `target_ms`, seeking if the request moved
    /// before the frame currently held.
    pub fn present_at(&mut self, target_ms: i64) -> Result<Option<RgbaFrame>, String> {
        if self.video.is_none() {
            return Ok(None);
        }
        let target = target_ms.max(0);
        let need_seek = match &self.current {
            Some(current) => target + SEEK_TOLERANCE_MS < current.pts_ms,
            None => true,
        };
        if need_seek {
            self.seek(target)?;
        }
        loop {
            match self.next_video_frame()? {
                Some(frame) => {
                    if frame.pts_ms > target {
                        if self.current.is_none() {
                            self.current = Some(frame);
                            return Ok(self.current.clone());
                        }
                        self.pending = Some(frame);
                        return Ok(self.current.clone());
                    }
                    self.current = Some(frame);
                }
                None => return Ok(self.current.clone()),
            }
        }
    }

    /// Rewind the extractor and reset the video decoder to the start.
    fn reset_to_start(&mut self) -> Result<(), String> {
        select_only(self.extractor.0, self.video_index, self.audio_index);
        let status = unsafe {
            AMediaExtractor_seekTo(self.extractor.0, 0, AMEDIAEXTRACTOR_SEEK_PREVIOUS_SYNC)
        };
        if status != AMEDIA_OK {
            log::debug!("video: MediaCodec rewind failed: {status}");
        }
        if let Some(video) = self.video.as_mut() {
            // SAFETY: the codec is live and started.
            unsafe {
                AMediaCodec_flush(video.codec.ptr);
            }
            video.input_done = false;
            video.output_done = false;
            video.last_pts_ms = 0;
        }
        self.current = None;
        self.pending = None;
        Ok(())
    }

    /// Seek the extractor and drop the decoder's queued state.
    fn seek(&mut self, target_ms: i64) -> Result<(), String> {
        let timestamp_us = target_ms.saturating_mul(1000);
        let status = unsafe {
            AMediaExtractor_seekTo(
                self.extractor.0,
                timestamp_us,
                AMEDIAEXTRACTOR_SEEK_PREVIOUS_SYNC,
            )
        };
        if status != AMEDIA_OK {
            log::debug!("video: MediaCodec seek to {target_ms} ms failed: {status}");
        }
        if let Some(video) = self.video.as_mut() {
            // SAFETY: the codec is live and started.
            unsafe {
                AMediaCodec_flush(video.codec.ptr);
            }
            video.input_done = false;
            video.output_done = false;
            video.last_pts_ms = 0;
        }
        self.current = None;
        self.pending = None;
        Ok(())
    }

    fn next_video_frame(&mut self) -> Result<Option<RgbaFrame>, String> {
        let extractor = self.extractor.0;
        let Some(video) = self.video.as_mut() else {
            return Ok(None);
        };
        decode_next_video(extractor, video)
    }

    /// Decode every audio sample into interleaved `f32`, once.
    fn decode_all_audio(&mut self) -> Result<(), String> {
        let Some(audio_index) = self.audio_index else {
            return Ok(());
        };
        let extractor = self.extractor.0;
        select_only(extractor, Some(audio_index), self.video_index);
        let status =
            unsafe { AMediaExtractor_seekTo(extractor, 0, AMEDIAEXTRACTOR_SEEK_PREVIOUS_SYNC) };
        status_ok(status, "AMediaExtractor_seekTo(audio)")?;

        let Some(audio) = self.audio.as_mut() else {
            return Ok(());
        };
        // SAFETY: the codec is live and started.
        unsafe {
            AMediaCodec_flush(audio.codec.ptr);
        }
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
                return Err("MediaCodec audio decode did not terminate".to_string());
            }
            if audio.output_done {
                break;
            }
            if !audio.input_done {
                let in_index = unsafe { AMediaCodec_dequeueInputBuffer(audio.codec.ptr, 0) };
                if in_index >= 0 {
                    feed_audio_sample(extractor, audio, in_index as usize)?;
                    continue;
                }
                if in_index != AMEDIACODEC_INFO_TRY_AGAIN_LATER {
                    return Err(format!("MediaCodec audio dequeueInputBuffer: {in_index}"));
                }
            }

            let mut info = AMediaCodecBufferInfo::default();
            let out_index = unsafe {
                AMediaCodec_dequeueOutputBuffer(audio.codec.ptr, &mut info, DECODE_TIMEOUT_US)
            };
            if out_index >= 0 {
                let index = out_index as usize;
                if info.flags & AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM != 0 {
                    release_output(audio.codec.ptr, index);
                    audio.output_done = true;
                    continue;
                }
                if info.size > 0
                    && let Some(slice) = output_slice(audio.codec.ptr, index, &info)
                {
                    append_pcm(slice, encoding, &mut samples);
                }
                release_output(audio.codec.ptr, index);
            } else if out_index == AMEDIACODEC_INFO_OUTPUT_FORMAT_CHANGED {
                if let Some(format) = output_format(audio.codec.ptr) {
                    if let Some(rate) = format_int32(format.0, c"sample-rate") {
                        out_rate = rate.max(0) as u32;
                    }
                    if let Some(channels) = format_int32(format.0, c"channel-count") {
                        out_channels = channels.max(0) as u16;
                    }
                    if let Some(value) = format_int32(format.0, c"pcm-encoding") {
                        encoding = value;
                    }
                }
            } else if out_index == AMEDIACODEC_INFO_OUTPUT_BUFFERS_CHANGED
                || out_index == AMEDIACODEC_INFO_TRY_AGAIN_LATER
            {
                // Retry on the next iteration.
            } else {
                return Err(format!("MediaCodec audio dequeueOutputBuffer: {out_index}"));
            }
        }

        audio.out_rate = out_rate;
        audio.out_channels = out_channels;
        audio.pcm_encoding = encoding;
        self.audio_pcm = Some(DecodedAudioPcm {
            sample_rate: out_rate,
            channels: out_channels,
            samples,
        });
        Ok(())
    }
}

/// Run the video decoder one step at a time until it yields a frame or EOF.
///
/// Output is drained first; when no output is ready the next input sample is
/// queued, and `dequeueOutputBuffer` waits a short while so a lagging decoder
/// does not spin the CPU.
fn decode_next_video(
    extractor: *mut AMediaExtractor,
    video: &mut VideoState,
) -> Result<Option<RgbaFrame>, String> {
    let mut steps = 0u32;
    loop {
        steps += 1;
        if steps > MAX_DECODE_STEPS {
            return Err("MediaCodec video decode did not terminate".to_string());
        }
        if video.output_done {
            return Ok(None);
        }
        if !video.input_done {
            let in_index = unsafe { AMediaCodec_dequeueInputBuffer(video.codec.ptr, 0) };
            if in_index >= 0 {
                feed_video_sample(extractor, video, in_index as usize)?;
                continue;
            }
            if in_index != AMEDIACODEC_INFO_TRY_AGAIN_LATER {
                return Err(format!("MediaCodec video dequeueInputBuffer: {in_index}"));
            }
        }

        let mut info = AMediaCodecBufferInfo::default();
        let out_index = unsafe {
            AMediaCodec_dequeueOutputBuffer(video.codec.ptr, &mut info, DECODE_TIMEOUT_US)
        };
        if out_index >= 0 {
            let index = out_index as usize;
            if info.flags & AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM != 0 {
                release_output(video.codec.ptr, index);
                video.output_done = true;
                return Ok(None);
            }
            if info.size > 0
                && let Some(layout) = video.layout
                && let Some(slice) = output_slice(video.codec.ptr, index, &info)
            {
                let data = yuv420_to_rgba(slice, &layout);
                let fallback = video.last_pts_ms + video.frame_duration_ms;
                let pts_ms = if info.presentation_time_us >= 0 {
                    micros_to_millis(info.presentation_time_us)
                } else {
                    fallback
                };
                video.last_pts_ms = pts_ms;
                release_output(video.codec.ptr, index);
                return Ok(Some(RgbaFrame {
                    width: layout.width() as u32,
                    height: layout.height() as u32,
                    pts_ms,
                    data,
                }));
            }
            release_output(video.codec.ptr, index);
        } else if out_index == AMEDIACODEC_INFO_OUTPUT_FORMAT_CHANGED {
            if let Some(format) = output_format(video.codec.ptr) {
                video.layout = Some(parse_video_output(format.0));
            }
        } else if out_index == AMEDIACODEC_INFO_OUTPUT_BUFFERS_CHANGED
            || out_index == AMEDIACODEC_INFO_TRY_AGAIN_LATER
        {
            // Retry on the next iteration.
        } else {
            return Err(format!("MediaCodec video dequeueOutputBuffer: {out_index}"));
        }
    }
}

/// Move one sample from the extractor into the video decoder.
fn feed_video_sample(
    extractor: *mut AMediaExtractor,
    video: &mut VideoState,
    input_index: usize,
) -> Result<(), String> {
    let mut capacity = 0usize;
    let buffer = unsafe { AMediaCodec_getInputBuffer(video.codec.ptr, input_index, &mut capacity) };
    if buffer.is_null() || capacity == 0 {
        return Err("MediaCodec video input buffer unavailable".to_string());
    }
    let read = unsafe { AMediaExtractor_readSampleData(extractor, buffer, capacity) };
    if read < 0 {
        let status = unsafe {
            AMediaCodec_queueInputBuffer(
                video.codec.ptr,
                input_index,
                0,
                0,
                0,
                AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM,
            )
        };
        status_ok(status, "MediaCodec queue video EOS")?;
        video.input_done = true;
    } else {
        let pts = unsafe { AMediaExtractor_getSampleTime(extractor) };
        let time = if pts < 0 { 0 } else { pts as u64 };
        let status = unsafe {
            AMediaCodec_queueInputBuffer(video.codec.ptr, input_index, 0, read as usize, time, 0)
        };
        status_ok(status, "MediaCodec queue video sample")?;
        // SAFETY: the extractor is live and the sample was read.
        unsafe {
            AMediaExtractor_advance(extractor);
        }
    }
    Ok(())
}

/// Move one sample from the extractor into the audio decoder.
fn feed_audio_sample(
    extractor: *mut AMediaExtractor,
    audio: &mut AudioState,
    input_index: usize,
) -> Result<(), String> {
    let mut capacity = 0usize;
    let buffer = unsafe { AMediaCodec_getInputBuffer(audio.codec.ptr, input_index, &mut capacity) };
    if buffer.is_null() || capacity == 0 {
        return Err("MediaCodec audio input buffer unavailable".to_string());
    }
    let read = unsafe { AMediaExtractor_readSampleData(extractor, buffer, capacity) };
    if read < 0 {
        let status = unsafe {
            AMediaCodec_queueInputBuffer(
                audio.codec.ptr,
                input_index,
                0,
                0,
                0,
                AMEDIACODEC_BUFFER_FLAG_END_OF_STREAM,
            )
        };
        status_ok(status, "MediaCodec queue audio EOS")?;
        audio.input_done = true;
    } else {
        let pts = unsafe { AMediaExtractor_getSampleTime(extractor) };
        let time = if pts < 0 { 0 } else { pts as u64 };
        let status = unsafe {
            AMediaCodec_queueInputBuffer(audio.codec.ptr, input_index, 0, read as usize, time, 0)
        };
        status_ok(status, "MediaCodec queue audio sample")?;
        // SAFETY: the extractor is live and the sample was read.
        unsafe {
            AMediaExtractor_advance(extractor);
        }
    }
    Ok(())
}

/// Copy the valid bytes of a dequeued output buffer (honouring `info.offset`).
///
/// The returned slice borrows the codec's buffer and is only valid until the
/// buffer is released.
fn output_slice<'a>(
    codec: *mut AMediaCodec,
    index: usize,
    info: &AMediaCodecBufferInfo,
) -> Option<&'a [u8]> {
    let mut capacity = 0usize;
    let buffer = unsafe { AMediaCodec_getOutputBuffer(codec, index, &mut capacity) };
    if buffer.is_null() {
        return None;
    }
    let start = (info.offset.max(0) as usize).min(capacity);
    let end = start
        .saturating_add(info.size.max(0) as usize)
        .min(capacity);
    // SAFETY: `buffer` points to `capacity` readable bytes owned by the codec,
    // and `start..end` is clamped inside that range.
    Some(unsafe { std::slice::from_raw_parts(buffer.add(start), end - start) })
}

/// Release a dequeued output buffer exactly once.
fn release_output(codec: *mut AMediaCodec, index: usize) {
    // SAFETY: the index was returned by `dequeueOutputBuffer` and not yet
    // released; the return status is not actionable here.
    unsafe {
        AMediaCodec_releaseOutputBuffer(codec, index, false);
    }
}

/// Fetch and take ownership of a codec's current output format.
fn output_format(codec: *mut AMediaCodec) -> Option<Format> {
    let ptr = unsafe { AMediaCodec_getOutputFormat(codec) };
    if ptr.is_null() {
        None
    } else {
        Some(Format(ptr))
    }
}

/// Translate the codec's YUV420 output format into plane geometry.
fn parse_video_output(format: *mut AMediaFormat) -> Yuv420Layout {
    let mut width = format_int32(format, c"width").unwrap_or(0).max(0) as usize;
    let mut height = format_int32(format, c"height").unwrap_or(0).max(0) as usize;
    let color_format =
        format_int32(format, c"color-format").unwrap_or(COLOR_FORMAT_YUV420_FLEXIBLE);
    let sampling = sampling_for(color_format);

    let stride = format_int32(format, c"stride")
        .map(|value| value.max(0) as usize)
        .filter(|value| *value >= width)
        .unwrap_or(width);
    let slice_height = format_int32(format, c"slice-height")
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
/// `AMediaCodec_getOutputImage`, which the NDK does not expose. Codecs almost
/// always report a concrete format in `AMediaCodec_getOutputFormat`; if one
/// reports the flexible value verbatim the planar layout is assumed (the
/// layout the software AVC decoder uses). This is the one guess in this file,
/// and it is only reached when the format is genuinely ambiguous.
fn sampling_for(color_format: i32) -> YuvSampling {
    match color_format {
        COLOR_FORMAT_YUV420_PLANAR | COLOR_FORMAT_YUV420_PACKED_PLANAR => YuvSampling::Planar,
        COLOR_FORMAT_YUV420_SEMI_PLANAR | COLOR_FORMAT_YUV420_PACKED_SEMI_PLANAR => {
            YuvSampling::SemiPlanar
        }
        _ => YuvSampling::Planar,
    }
}

/// Resolve the crop rectangle, tolerating both the exclusive (`right - left`)
/// and the historically inclusive (`right - left + 1`) conventions.
fn resolve_crop(format: *mut AMediaFormat, width: usize, height: usize) -> CropRect {
    let left = non_negative(format_int32(format, c"crop-left"));
    let top = non_negative(format_int32(format, c"crop-top"));
    let right = non_negative(format_int32(format, c"crop-right"));
    let bottom = non_negative(format_int32(format, c"crop-bottom"));

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

/// Scan the container, count video/audio streams and keep the first of each.
fn collect_tracks(
    extractor: *mut AMediaExtractor,
) -> Result<(i64, i64, Option<PickedTrack>, Option<PickedTrack>), String> {
    let count = unsafe { AMediaExtractor_getTrackCount(extractor) };
    let mut video_streams = 0i64;
    let mut audio_streams = 0i64;
    let mut video = None;
    let mut audio = None;
    for index in 0..count {
        let ptr = unsafe { AMediaExtractor_getTrackFormat(extractor, index) };
        if ptr.is_null() {
            continue;
        }
        let format = Format(ptr);
        let Some(mime) = format_string(ptr, c"mime") else {
            continue;
        };
        if mime.starts_with("video/") {
            video_streams += 1;
            if video.is_none() {
                video = Some(PickedTrack {
                    index,
                    mime,
                    format,
                });
            }
        } else if mime.starts_with("audio/") {
            audio_streams += 1;
            if audio.is_none() {
                audio = Some(PickedTrack {
                    index,
                    mime,
                    format,
                });
            }
        }
    }
    Ok((video_streams, audio_streams, video, audio))
}

/// Read the container metadata that the MediaCodec NDK exposes.
fn build_metadata(
    video: Option<&PickedTrack>,
    audio: Option<&PickedTrack>,
    video_streams: i64,
    audio_streams: i64,
) -> MovieMetadata {
    let width = video
        .and_then(|track| track_int32(track, c"width"))
        .unwrap_or(0)
        .max(0) as u32;
    let height = video
        .and_then(|track| track_int32(track, c"height"))
        .unwrap_or(0)
        .max(0) as u32;
    let fps = video
        .and_then(|track| format_f32(track.format.0, c"frame-rate"))
        .map_or(0.0, f64::from);
    let duration_us = video
        .and_then(|track| format_int64(track.format.0, c"duration"))
        .unwrap_or(0);
    let total_time_ms = if duration_us > 0 {
        micros_to_millis(duration_us)
    } else {
        0
    };
    let declared_frames = video
        .and_then(|track| track_int32(track, c"frame-count"))
        .unwrap_or(0);
    let total_frames = if declared_frames > 0 {
        i64::from(declared_frames)
    } else if fps > 0.0 && total_time_ms > 0 {
        (total_time_ms as f64 * fps / 1000.0).round() as i64
    } else {
        0
    };
    let audio_sample_rate = audio
        .and_then(|track| track_int32(track, c"sample-rate"))
        .unwrap_or(0)
        .max(0) as u32;
    let audio_channels = audio
        .and_then(|track| track_int32(track, c"channel-count"))
        .unwrap_or(0)
        .max(0) as u16;

    MovieMetadata {
        width,
        height,
        fps,
        total_frames,
        total_time_ms,
        video_streams,
        audio_streams,
        audio_sample_rate,
        audio_channels,
    }
}

/// Create, configure (for a byte-buffer YUV420 output) and start the H.264
/// decoder.
fn create_video_codec(track: &PickedTrack) -> Result<Codec, String> {
    let mime =
        CString::new(track.mime.as_str()).map_err(|e| format!("invalid video mime type: {e}"))?;
    let ptr = unsafe { AMediaCodec_createDecoderByType(mime.as_ptr()) };
    if ptr.is_null() {
        return Err(format!("no MediaCodec decoder for {}", track.mime));
    }
    let mut codec = Codec {
        ptr,
        started: false,
    };
    // Request an uncompressed YUV420 byte-buffer output (no Surface).
    unsafe {
        AMediaFormat_setInt32(
            track.format.0,
            c"color-format".as_ptr(),
            COLOR_FORMAT_YUV420_FLEXIBLE,
        );
    }
    let status = unsafe {
        AMediaCodec_configure(
            ptr,
            track.format.0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        )
    };
    status_ok(status, "AMediaCodec_configure(video)")?;
    let status = unsafe { AMediaCodec_start(ptr) };
    status_ok(status, "AMediaCodec_start(video)")?;
    codec.started = true;
    Ok(codec)
}

/// Create, configure and start the AAC decoder.
fn create_audio_codec(track: &PickedTrack) -> Result<Codec, String> {
    let mime =
        CString::new(track.mime.as_str()).map_err(|e| format!("invalid audio mime type: {e}"))?;
    let ptr = unsafe { AMediaCodec_createDecoderByType(mime.as_ptr()) };
    if ptr.is_null() {
        return Err(format!("no MediaCodec decoder for {}", track.mime));
    }
    let mut codec = Codec {
        ptr,
        started: false,
    };
    let status = unsafe {
        AMediaCodec_configure(
            ptr,
            track.format.0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        )
    };
    status_ok(status, "AMediaCodec_configure(audio)")?;
    let status = unsafe { AMediaCodec_start(ptr) };
    status_ok(status, "AMediaCodec_start(audio)")?;
    codec.started = true;
    Ok(codec)
}

/// Select the track we need and unselect the other one we know about.
fn select_only(extractor: *mut AMediaExtractor, wanted: Option<usize>, unwanted: Option<usize>) {
    if let Some(index) = unwanted
        && Some(index) != wanted
    {
        // SAFETY: tracks are valid indices from `collect_tracks`; the status
        // is not actionable (unselecting an unselected track is harmless).
        unsafe {
            AMediaExtractor_unselectTrack(extractor, index);
        }
    }
    if let Some(index) = wanted {
        // SAFETY: as above; `selectTrack` is idempotent.
        unsafe {
            AMediaExtractor_selectTrack(extractor, index);
        }
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

/// Write `bytes` to an anonymous `memfd` and return the open file.
fn memory_backed_file(bytes: &[u8]) -> Result<File, String> {
    // SAFETY: `syscall` is variadic; `MFD_CLOEXEC` is the documented flag and
    // the name is a valid NUL-terminated C string.
    let fd = unsafe { syscall(SYS_MEMFD_CREATE, c"krkr-movie".as_ptr(), MFD_CLOEXEC) };
    if fd < 0 {
        return Err(format!(
            "memfd_create for in-memory movie failed: {}",
            io::Error::last_os_error()
        ));
    }
    // SAFETY: `fd` is a freshly created, owned descriptor.
    let mut file = unsafe { File::from_raw_fd(fd as RawFd) };
    file.write_all(bytes)
        .map_err(|e| format!("writing movie into memory fd: {e}"))?;
    Ok(file)
}

/// Map a `media_status_t` to a `Result`, describing the failure.
fn status_ok(status: c_int, what: &str) -> Result<(), String> {
    if status == AMEDIA_OK {
        Ok(())
    } else {
        Err(format!("{what}: MediaCodec error {status}"))
    }
}

/// Interpreting `None` as zero.
fn non_negative(value: Option<i32>) -> usize {
    value.unwrap_or(0).max(0) as usize
}

fn frame_duration_ms(fps: f64) -> i64 {
    if fps > 0.0 {
        (1000.0 / fps).round().max(1.0) as i64
    } else {
        DEFAULT_FRAME_DURATION_MS
    }
}

fn track_int32(track: &PickedTrack, key: &CStr) -> Option<i32> {
    format_int32(track.format.0, key)
}

fn format_int32(format: *mut AMediaFormat, key: &CStr) -> Option<i32> {
    let mut out = 0i32;
    // SAFETY: `format` is live and `out` is a valid pointer.
    if unsafe { AMediaFormat_getInt32(format, key.as_ptr(), &mut out) } {
        Some(out)
    } else {
        None
    }
}

fn format_int64(format: *mut AMediaFormat, key: &CStr) -> Option<i64> {
    let mut out = 0i64;
    // SAFETY: `format` is live and `out` is a valid pointer.
    if unsafe { AMediaFormat_getInt64(format, key.as_ptr(), &mut out) } {
        Some(out)
    } else {
        None
    }
}

fn format_f32(format: *mut AMediaFormat, key: &CStr) -> Option<f32> {
    let mut out = 0.0f32;
    // SAFETY: `format` is live and `out` is a valid pointer.
    if unsafe { AMediaFormat_getFloat(format, key.as_ptr(), &mut out) } {
        Some(out)
    } else {
        None
    }
}

fn format_string(format: *mut AMediaFormat, key: &CStr) -> Option<String> {
    let mut out: *const c_char = std::ptr::null();
    // SAFETY: `format` is live and `out` is a valid pointer; the returned
    // string is owned by the format and copied immediately.
    if !unsafe { AMediaFormat_getString(format, key.as_ptr(), &mut out) } || out.is_null() {
        return None;
    }
    // SAFETY: `out` is a NUL-terminated string owned by the format.
    let text = unsafe { CStr::from_ptr(out) };
    text.to_str().ok().map(str::to_owned)
}
