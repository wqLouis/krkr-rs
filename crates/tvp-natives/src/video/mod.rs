//! Real movie decoding for [`VideoOverlay`](crate::video_overlay) via the
//! system FFmpeg libraries (`libavformat` + `libavcodec` + `libswscale` +
//! `libswresample`).
//!
//! The game's movies are ordinary **ISO-BMFF / MP4** files (despite their
//! `.mpg` names): H.264 video + AAC audio. This module opens the media from
//! an in-memory byte buffer (the XP3 entry read through the engine storage)
//! with a custom `AVIOContext` — no temp files — reports the real container
//! metadata, decodes video frames to tightly-packed RGBA8 on demand and
//! decodes the whole audio track to interleaved `f32` PCM.
//!
//! # Shape
//!
//! * [`MovieDecoder::open`] demuxes the buffer, builds the video/audio
//!   decoders and reads [`MovieMetadata`] (`originalWidth`, `originalHeight`,
//!   `fps`, `totalFrame`, `totalTime`, stream counts and the audio sample
//!   format).
//! * [`MovieDecoder::present_at`] seeks (when the request moves backwards)
//!   and decodes forward until the frame covering the requested millisecond
//!   is available, returning it as an [`RgbaFrame`]. This is what the
//!   `VideoOverlay` presentation clock calls each poll.
//! * [`MovieDecoder::audio_pcm`] returns the whole-track [`DecodedAudioPcm`]
//!   (decoded once), ready to hand to the engine sound mixer.
//!
//! Everything is confined to the VM thread: FFmpeg contexts are not shared.

use std::io::Cursor;
use std::sync::Once;

use ffmpeg_next as ffmpeg;

use ffmpeg::{
    ChannelLayout, Error as FfmpegError, Packet, Rational, codec, format, media, software,
    util::frame,
};

/// FFmpeg's internal time base (`AV_TIME_BASE`), microseconds per second.
const AV_TIME_BASE: i128 = 1_000_000;

/// Fallback frame duration when the stream does not report a frame rate.
const DEFAULT_FRAME_DURATION_MS: i64 = 40;

/// Seeking this far before the current frame triggers a real seek; smaller
/// backwards steps are satisfied from the cached frame/pending queue.
const SEEK_TOLERANCE_MS: i64 = 2;

static FFMPEG_INIT: Once = Once::new();

/// Initialize FFmpeg once per process. Safe to call from any thread.
pub fn init() -> Result<(), String> {
    let mut result = Ok(());
    FFMPEG_INIT.call_once(|| {
        if let Err(e) = ffmpeg::init() {
            result = Err(format!("ffmpeg init: {e}"));
        }
    });
    result
}

fn is_eagain(e: &FfmpegError) -> bool {
    matches!(e, FfmpegError::Other { errno } if *errno == ffmpeg::ffi::EAGAIN)
}

fn is_eof(e: &FfmpegError) -> bool {
    matches!(e, FfmpegError::Eof)
}

/// Real container metadata exposed through the `VideoOverlay` properties.
#[derive(Debug, Clone, PartialEq)]
pub struct MovieMetadata {
    /// Coded video width (not display aspect ratio).
    pub width: u32,
    /// Coded video height.
    pub height: u32,
    /// Frames per second (prefers the container's average frame rate).
    pub fps: f64,
    /// Total number of video frames (container value, else derived from the
    /// duration and `fps`).
    pub total_frames: i64,
    /// Video duration in milliseconds.
    pub total_time_ms: i64,
    /// Number of video streams in the container.
    pub video_streams: i64,
    /// Number of audio streams in the container.
    pub audio_streams: i64,
    /// Audio sample rate (0 when there is no audio).
    pub audio_sample_rate: u32,
    /// Audio channel count (0 when there is no audio).
    pub audio_channels: u16,
}

/// One decoded video frame, converted to tightly packed RGBA8.
#[derive(Debug, Clone, PartialEq)]
pub struct RgbaFrame {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Presentation timestamp in milliseconds.
    pub pts_ms: i64,
    /// `width * height * 4` RGBA bytes, no row padding.
    pub data: Vec<u8>,
}

impl RgbaFrame {
    /// A 64-bit FNV-1a hash of the pixel bytes (diagnostics / tests).
    pub fn checksum(&self) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for &byte in &self.data {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }

    /// Whether the frame is entirely transparent black (i.e. no picture).
    pub fn is_blank(&self) -> bool {
        self.data.iter().all(|&byte| byte == 0)
    }
}

/// Whole-track decoded audio: interleaved `f32` samples in `[-1, 1]`.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedAudioPcm {
    /// Samples per second.
    pub sample_rate: u32,
    /// Channel count.
    pub channels: u16,
    /// Interleaved samples (`samples[frame * channels + channel]`).
    pub samples: Vec<f32>,
}

impl DecodedAudioPcm {
    /// Number of interleaved sample values.
    pub fn sample_values(&self) -> usize {
        self.samples.len()
    }

    /// Number of sample frames (one value per channel).
    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels.max(1))
    }

    /// Duration in seconds.
    pub fn duration_seconds(&self) -> f64 {
        self.frames() as f64 / f64::from(self.sample_rate.max(1))
    }
}

struct VideoState {
    index: usize,
    decoder: codec::decoder::Video,
    time_base: Rational,
    scaler: Option<software::scaling::Context>,
}

struct AudioState {
    index: usize,
    decoder: codec::decoder::Audio,
    resampler: Option<software::resampling::Context>,
    out_channels: u16,
    out_rate: u32,
}

/// Owns the demuxer and the video/audio decoders for one open movie.
pub struct MovieDecoder {
    input: format::context::Input,
    metadata: MovieMetadata,
    video: Option<VideoState>,
    audio: Option<AudioState>,
    audio_pcm: Option<DecodedAudioPcm>,
    /// Last frame selected for presentation.
    current: Option<RgbaFrame>,
    /// A frame decoded ahead of the requested position.
    pending: Option<RgbaFrame>,
    /// True once the video decoder has been drained at EOF.
    video_eof: bool,
    /// PTS of the last decoded frame, for streams without timestamps.
    last_pts_ms: i64,
    frame_duration_ms: i64,
}

/// Structure captured while scanning the stream table.
struct StreamPick {
    index: usize,
    params: codec::Parameters,
    time_base: Rational,
    fps: f64,
    frames: i64,
    duration_ms: i64,
}

impl MovieDecoder {
    /// Open a movie from an in-memory byte buffer.
    pub fn open(bytes: Vec<u8>) -> Result<Self, String> {
        init()?;
        let io = format::context::StreamIo::from_read_seek(Cursor::new(bytes))
            .map_err(|e| format!("ffmpeg custom io: {e}"))?;
        let input = format::input_from_stream(io, Some("movie.mpg"), None)
            .map_err(|e| format!("ffmpeg open: {e}"))?;
        Self::from_input(input)
    }

    fn from_input(input: format::context::Input) -> Result<Self, String> {
        let mut video_streams = 0i64;
        let mut audio_streams = 0i64;
        let mut video_pick: Option<StreamPick> = None;
        let mut audio_pick: Option<StreamPick> = None;

        for stream in input.streams() {
            let params = stream.parameters();
            let time_base = stream.time_base();
            let duration_ms = rescale_to_ms(stream.duration(), time_base);
            match params.medium() {
                media::Type::Video => {
                    video_streams += 1;
                    if video_pick.is_none() {
                        video_pick = Some(StreamPick {
                            index: stream.index(),
                            params,
                            time_base,
                            fps: stream_fps(&stream),
                            frames: stream.frames(),
                            duration_ms,
                        });
                    }
                }
                media::Type::Audio => {
                    audio_streams += 1;
                    if audio_pick.is_none() {
                        audio_pick = Some(StreamPick {
                            index: stream.index(),
                            params,
                            time_base,
                            fps: 0.0,
                            frames: stream.frames(),
                            duration_ms,
                        });
                    }
                }
                _ => {}
            }
        }

        // Summary values are read before the picks are consumed by decoder
        // construction below.
        let video_fps = video_pick.as_ref().map_or(0.0, |p| p.fps);
        let video_duration_ms = video_pick.as_ref().map_or(0, |p| p.duration_ms);
        let declared_frames = video_pick.as_ref().map_or(0, |p| p.frames);
        let container_duration_ms = if input.duration() > 0 {
            (i128::from(input.duration()) * 1000 / AV_TIME_BASE) as i64
        } else {
            0
        };
        let total_time_ms = if video_duration_ms > 0 {
            video_duration_ms
        } else if container_duration_ms > 0 {
            container_duration_ms
        } else {
            0
        };
        let total_frames = if declared_frames > 0 {
            declared_frames
        } else if video_fps > 0.0 && total_time_ms > 0 {
            (total_time_ms as f64 * video_fps / 1000.0).round() as i64
        } else {
            0
        };

        let video = match video_pick {
            Some(pick) => {
                let ctx = codec::context::Context::from_parameters(pick.params)
                    .map_err(|e| format!("video codec context: {e}"))?;
                let decoder = ctx
                    .decoder()
                    .video()
                    .map_err(|e| format!("video decoder: {e}"))?;
                Some(VideoState {
                    index: pick.index,
                    decoder,
                    time_base: pick.time_base,
                    scaler: None,
                })
            }
            None => None,
        };

        let audio = match audio_pick {
            Some(pick) => {
                let ctx = codec::context::Context::from_parameters(pick.params)
                    .map_err(|e| format!("audio codec context: {e}"))?;
                let decoder = ctx
                    .decoder()
                    .audio()
                    .map_err(|e| format!("audio decoder: {e}"))?;
                let channels = decoder.channels();
                Some(AudioState {
                    index: pick.index,
                    out_channels: channels,
                    out_rate: decoder.rate(),
                    decoder,
                    resampler: None,
                })
            }
            None => None,
        };

        let (width, height) = video
            .as_ref()
            .map_or((0, 0), |v| (v.decoder.width(), v.decoder.height()));
        let frame_duration_ms = if video_fps > 0.0 {
            (1000.0 / video_fps).round().max(1.0) as i64
        } else {
            DEFAULT_FRAME_DURATION_MS
        };

        let metadata = MovieMetadata {
            width,
            height,
            fps: video_fps,
            total_frames,
            total_time_ms,
            video_streams,
            audio_streams,
            audio_sample_rate: audio.as_ref().map_or(0, |a| a.decoder.rate()),
            audio_channels: audio.as_ref().map_or(0, |a| a.decoder.channels()),
        };

        let mut decoder = MovieDecoder {
            input,
            metadata,
            video,
            audio,
            audio_pcm: None,
            current: None,
            pending: None,
            video_eof: false,
            last_pts_ms: 0,
            frame_duration_ms,
        };

        // Decode the whole audio track up front (effect movies are short),
        // then rewind the demuxer so video presentation starts cleanly.
        if decoder.audio.is_some()
            && let Err(e) = decoder.decode_all_audio()
        {
            log::warn!("video: audio decode failed: {e}");
        }
        decoder.reset_to_start();
        if let Some(video) = decoder.video.as_mut() {
            video.decoder.flush();
        }
        if let Err(e) = decoder.present_at(0) {
            log::warn!("video: could not decode/present the first frame: {e}");
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
            self.reset_to_start();
            if let Some(video) = self.video.as_mut() {
                video.decoder.flush();
            }
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

    fn reset_to_start(&mut self) {
        let _ = self.input.seek(0, ..0);
        self.current = None;
        self.pending = None;
        self.video_eof = false;
        self.last_pts_ms = 0;
    }

    fn seek(&mut self, target_ms: i64) -> Result<(), String> {
        let ts = target_ms.saturating_mul(1000);
        if let Err(e) = self.input.seek(ts, ..ts) {
            log::debug!("video: seek to {target_ms} ms failed: {e}");
        }
        if let Some(video) = self.video.as_mut() {
            video.decoder.flush();
        }
        if let Some(audio) = self.audio.as_mut() {
            audio.decoder.flush();
        }
        self.current = None;
        self.pending = None;
        self.video_eof = false;
        Ok(())
    }

    fn next_video_frame(&mut self) -> Result<Option<RgbaFrame>, String> {
        if self.video.is_none() {
            return Ok(None);
        }
        loop {
            let mut raw = frame::Video::empty();
            let received = self
                .video
                .as_mut()
                .expect("video state checked above")
                .decoder
                .receive_frame(&mut raw);
            match received {
                Ok(()) => {
                    let fallback = self.last_pts_ms + self.frame_duration_ms;
                    let video = self.video.as_mut().expect("video state checked above");
                    let frame = convert_video(video, &raw, fallback)?;
                    self.last_pts_ms = frame.pts_ms;
                    return Ok(Some(frame));
                }
                Err(e) if is_eof(&e) => return Ok(None),
                Err(e) if is_eagain(&e) => {
                    let want = self.video.as_ref().expect("video state checked").index;
                    match self.read_packet()? {
                        Some(packet) => {
                            if packet.stream() == want
                                && let Some(video) = self.video.as_mut()
                                && let Err(e) = video.decoder.send_packet(&packet)
                                && !is_eagain(&e)
                            {
                                log::debug!("video: send_packet: {e}");
                            }
                        }
                        None => {
                            if self.video_eof {
                                return Ok(None);
                            }
                            self.video_eof = true;
                            if let Some(video) = self.video.as_mut()
                                && let Err(e) = video.decoder.send_eof()
                                && !is_eof(&e)
                            {
                                log::debug!("video: send_eof: {e}");
                            }
                        }
                    }
                }
                Err(e) => {
                    log::debug!("video: receive_frame: {e}");
                    return Ok(None);
                }
            }
        }
    }

    fn read_packet(&mut self) -> Result<Option<Packet>, String> {
        let mut packet = Packet::empty();
        match packet.read(&mut self.input) {
            Ok(()) => Ok(Some(packet)),
            Err(e) if is_eof(&e) => Ok(None),
            Err(e) => {
                log::debug!("video: read packet: {e}");
                Ok(None)
            }
        }
    }

    fn decode_all_audio(&mut self) -> Result<(), String> {
        let Self {
            input,
            audio,
            audio_pcm,
            ..
        } = self;
        let Some(audio) = audio.as_mut() else {
            return Ok(());
        };
        let _ = input.seek(0, ..0);
        audio.decoder.flush();
        let mut samples: Vec<f32> = Vec::new();
        let mut out_rate = audio.decoder.rate();
        let mut out_channels = audio.out_channels;
        loop {
            let mut packet = Packet::empty();
            match packet.read(input) {
                Ok(()) => {}
                Err(e) if is_eof(&e) => break,
                Err(e) => {
                    log::debug!("video: audio read packet: {e}");
                    break;
                }
            }
            if packet.stream() != audio.index {
                continue;
            }
            match audio.decoder.send_packet(&packet) {
                Ok(()) => {}
                Err(e) if is_eagain(&e) => {}
                Err(e) => {
                    log::debug!("video: audio send_packet: {e}");
                    continue;
                }
            }
            drain_audio(audio, &mut samples, &mut out_rate, &mut out_channels)?;
        }
        if let Err(e) = audio.decoder.send_eof()
            && !is_eof(&e)
        {
            log::debug!("video: audio send_eof: {e}");
        }
        drain_audio(audio, &mut samples, &mut out_rate, &mut out_channels)?;
        *audio_pcm = Some(DecodedAudioPcm {
            sample_rate: out_rate,
            channels: out_channels,
            samples,
        });
        Ok(())
    }
}

/// Convert one decoded video frame to tightly packed RGBA8.
fn convert_video(
    video: &mut VideoState,
    raw: &frame::Video,
    fallback_pts_ms: i64,
) -> Result<RgbaFrame, String> {
    if video.scaler.is_none() {
        let scaler = software::scaling::Context::get(
            raw.format(),
            raw.width(),
            raw.height(),
            format::Pixel::RGBA,
            raw.width(),
            raw.height(),
            software::scaling::Flags::BILINEAR,
        )
        .map_err(|e| format!("swscale context: {e}"))?;
        video.scaler = Some(scaler);
    }
    let scaler = video.scaler.as_mut().expect("scaler created above");
    let mut converted = frame::Video::empty();
    scaler
        .run(raw, &mut converted)
        .map_err(|e| format!("swscale run: {e}"))?;

    let width = converted.width();
    let height = converted.height();
    let stride = converted.stride(0);
    let src = converted.data(0);
    let row_bytes = width as usize * 4;
    let mut data = Vec::with_capacity(row_bytes * height as usize);
    for row in 0..height as usize {
        let start = row * stride;
        data.extend_from_slice(&src[start..start + row_bytes]);
    }

    let pts_ms = match raw.pts() {
        Some(pts) if pts != ffmpeg::ffi::AV_NOPTS_VALUE => rescale_to_ms(pts, video.time_base),
        _ => fallback_pts_ms,
    };
    Ok(RgbaFrame {
        width,
        height,
        pts_ms,
        data,
    })
}

/// Drain decoded audio frames from `decoder` into interleaved `f32` samples.
fn drain_audio(
    audio: &mut AudioState,
    samples: &mut Vec<f32>,
    out_rate: &mut u32,
    out_channels: &mut u16,
) -> Result<(), String> {
    loop {
        let mut decoded = frame::Audio::empty();
        match audio.decoder.receive_frame(&mut decoded) {
            Ok(()) => {
                let src_format = decoded.format();
                let src_layout = decoded.channel_layout();
                let src_rate = decoded.rate();
                let src_channels = decoded.channels();
                if audio.resampler.is_none() {
                    let dst_layout = if src_layout.is_empty() {
                        ChannelLayout::default(i32::from(src_channels))
                    } else {
                        src_layout
                    };
                    let resampler = software::resampling::Context::get(
                        src_format,
                        src_layout,
                        src_rate,
                        format::Sample::F32(format::sample::Type::Packed),
                        dst_layout,
                        src_rate,
                    )
                    .map_err(|e| format!("swresample context: {e}"))?;
                    audio.resampler = Some(resampler);
                    audio.out_rate = src_rate;
                    audio.out_channels = src_channels;
                }
                let mut converted = frame::Audio::empty();
                audio
                    .resampler
                    .as_mut()
                    .expect("resampler created above")
                    .run(&decoded, &mut converted)
                    .map_err(|e| format!("swresample run: {e}"))?;
                samples.extend_from_slice(converted.plane::<f32>(0));
                *out_rate = audio.out_rate;
                *out_channels = audio.out_channels;
            }
            Err(e) if is_eof(&e) => return Ok(()),
            Err(e) if is_eagain(&e) => return Ok(()),
            Err(e) => {
                log::debug!("video: audio receive_frame: {e}");
                return Ok(());
            }
        }
    }
}

fn stream_fps(stream: &format::stream::Stream<'_>) -> f64 {
    let avg = stream.avg_frame_rate();
    if avg.numerator() > 0 && avg.denominator() > 0 {
        return f64::from(avg);
    }
    let rate = stream.rate();
    if rate.numerator() > 0 && rate.denominator() > 0 {
        return f64::from(rate);
    }
    0.0
}

/// Rescale an FFmpeg timestamp in `time_base` units to milliseconds.
fn rescale_to_ms(value: i64, time_base: Rational) -> i64 {
    let den = i128::from(time_base.denominator());
    if den == 0 {
        return 0;
    }
    let num = i128::from(time_base.numerator());
    ((i128::from(value) * num * 1000) / den) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Path to the reference game archive (a real title used by the repo's
    /// manual verification steps).
    const GAME_XP3: &str = "/mnt/DATA/Games/Others/test/data.xp3";
    /// The movie the task singled out: H.264 1280x720 + AAC, ~5 s.
    const MOVIE_ENTRY: &str = "effect/watch_long.mpg";

    /// Extract the real movie from the XP3 archive with the repo script.
    /// Returns `None` (so the test skips) when the game or script is absent.
    fn extract_real_movie() -> Option<Vec<u8>> {
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let root = manifest.parent()?.parent()?;
        let script = root.join("scripts/extract_xp3.py");
        if !script.exists() || !std::path::Path::new(GAME_XP3).exists() {
            return None;
        }
        let output = std::process::Command::new("python3")
            .arg(&script)
            .arg(GAME_XP3)
            .arg(MOVIE_ENTRY)
            .output()
            .ok()?;
        if !output.status.success() || output.stdout.is_empty() {
            return None;
        }
        Some(output.stdout)
    }

    #[test]
    fn open_rejects_garbage() {
        assert!(MovieDecoder::open(b"this is not a movie".to_vec()).is_err());
    }

    #[test]
    fn decodes_real_mp4_metadata_frames_and_audio() {
        let Some(bytes) = extract_real_movie() else {
            eprintln!("skipping: {GAME_XP3} / scripts/extract_xp3.py not available");
            return;
        };
        let original_len = bytes.len();
        let mut decoder = MovieDecoder::open(bytes).expect("real movie must open");
        let metadata = decoder.metadata().clone();
        eprintln!(
            "metadata: {}x{} @ {:.3} fps, {} frames, {} ms, video_streams={}, audio_streams={}, audio={}Hz/{}ch",
            metadata.width,
            metadata.height,
            metadata.fps,
            metadata.total_frames,
            metadata.total_time_ms,
            metadata.video_streams,
            metadata.audio_streams,
            metadata.audio_sample_rate,
            metadata.audio_channels,
        );

        // -- metadata (cross-checked against ffprobe below) -----------------
        assert_eq!(metadata.width, 1280, "width");
        assert_eq!(metadata.height, 720, "height");
        assert_eq!(metadata.video_streams, 1, "video streams");
        assert_eq!(metadata.audio_streams, 1, "audio streams");
        assert!(
            (metadata.fps - 30.0).abs() < 0.01,
            "fps {} should be ~30",
            metadata.fps
        );
        assert_eq!(metadata.total_frames, 150, "total frames");
        assert!(
            (metadata.total_time_ms as f64 / 1000.0 - 5.0).abs() < 0.2,
            "duration {} ms should be ~5.0 s",
            metadata.total_time_ms
        );
        assert_eq!(metadata.audio_sample_rate, 48_000);
        assert_eq!(metadata.audio_channels, 2);

        // -- first frame: tightly packed RGBA of the coded size -------------
        let first = decoder
            .present_at(0)
            .expect("present frame 0")
            .expect("movie has a video frame");
        assert_eq!(first.width, 1280);
        assert_eq!(first.height, 720);
        assert_eq!(first.data.len(), 1280 * 720 * 4, "RGBA byte count");
        assert!(!first.is_blank(), "frame 0 must contain pixels");
        let first_checksum = first.checksum();
        eprintln!(
            "frame 0: {}x{} bytes={} checksum={first_checksum:016x}",
            first.width,
            first.height,
            first.data.len(),
        );

        // -- a later frame decodes to different pixels ----------------------
        // (frames 0..=6 are identical in this clip; 1 s is safely different)
        let later = decoder
            .present_at(1000)
            .expect("present frame at 1000 ms")
            .expect("movie has a frame at 1000 ms");
        assert_eq!(later.data.len(), 1280 * 720 * 4);
        assert!(!later.is_blank());
        assert_ne!(later.checksum(), first_checksum, "frames must differ");
        assert!(later.pts_ms >= 990, "pts {} ~= 1000 ms", later.pts_ms);
        eprintln!(
            "frame @{}ms: {}x{} bytes={} checksum={:016x}",
            later.pts_ms,
            later.width,
            later.height,
            later.data.len(),
            later.checksum(),
        );

        // -- audio decodes to interleaved f32 -------------------------------
        let pcm = decoder.audio_pcm().expect("audio track").clone();
        assert_eq!(pcm.sample_rate, 48_000);
        assert_eq!(pcm.channels, 2);
        assert!(pcm.sample_values() > 0, "audio must yield samples");
        assert!(
            (pcm.duration_seconds() - 5.0).abs() < 0.5,
            "audio duration {:.3} s",
            pcm.duration_seconds()
        );
        eprintln!(
            "audio: {} samples ({} frames), {}Hz x {}ch, {:.3}s",
            pcm.sample_values(),
            pcm.frames(),
            pcm.sample_rate,
            pcm.channels,
            pcm.duration_seconds(),
        );
        // The bytes were moved into the decoder; the fixture was real.
        assert_eq!(original_len, 2_266_977);
    }

    #[test]
    fn metadata_matches_ffprobe() {
        let Some(bytes) = extract_real_movie() else {
            eprintln!("skipping: game archive / extractor not available");
            return;
        };
        let decoder = MovieDecoder::open(bytes.clone()).expect("open");
        let metadata = decoder.metadata().clone();

        // Write the bytes to a temp file only so the `ffprobe` CLI can read
        // them; the decoder itself never uses a temp file.
        let path = std::env::temp_dir().join(format!(
            "tvp_video_ffprobe_{}_{}.mpg",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        if std::fs::write(&path, &bytes).is_err() {
            eprintln!("skipping ffprobe cross-check: cannot write temp file");
            return;
        }
        let output = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=width,height,nb_frames",
                "-of",
                "default=noprint_wrappers=1",
            ])
            .arg(&path)
            .output();
        let _ = std::fs::remove_file(&path);
        let Ok(output) = output else {
            eprintln!("skipping ffprobe cross-check: ffprobe unavailable");
            return;
        };
        if !output.status.success() {
            eprintln!("skipping ffprobe cross-check: ffprobe failed");
            return;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let mut width = 0u32;
        let mut height = 0u32;
        let mut frames = 0i64;
        for line in text.lines() {
            if let Some((key, value)) = line.split_once('=') {
                match key {
                    "width" => width = value.trim().parse().unwrap_or(0),
                    "height" => height = value.trim().parse().unwrap_or(0),
                    "nb_frames" => frames = value.trim().parse().unwrap_or(0),
                    _ => {}
                }
            }
        }
        assert_eq!(metadata.width, width, "width vs ffprobe");
        assert_eq!(metadata.height, height, "height vs ffprobe");
        assert_eq!(metadata.total_frames, frames, "frame count vs ffprobe");
    }
}
