//! L2 sound model and software mixer.
//!
//! Behavioral reference: `main`'s
//! `crates/tvp-sound/src/{mixer,source,wavesound,waveflags}.rs`. This is
//! the **state** half of sound, re-expressed as explicit L2 values:
//!
//! * [`SoundSource`] — a playable PCM source: either caller-supplied
//!   decoded samples or an [`AudioStream`] that a decode-ahead worker fills a
//!   bounded window from on its own thread while the frame thread reads it; a
//!   read outside the window reseeks the worker. The stream's own
//!   [`AudioSpec`] and optional generic loop information travel with it.
//! * [`SoundChannel`] — one playback voice: play/stop/pause, loop, pan,
//!   volume/volume2, playback rate, 3D position and linear fades.
//! * [`SoundBuffer`] — the script-object half: the loaded source, its
//!   [`LoopFlags`], and label/status event tracking.
//! * [`Mixer`] — a clock-driven collection of channels plus the
//!   [`Mixer::mix`] action that renders the caller's output buffer.
//!
//! # No device, no globals
//!
//! Nothing here opens an audio device and there is no process-global state:
//! the application's update loop calls [`Mixer::advance`] (or
//! [`Mixer::advance_to`]) each frame, and the L1 `AudioDevice` (in
//! `platform`) later pulls [`Mixer::mix`] into its own callback. Two
//! independent [`Mixer`] values in one process never interact. A streaming
//! [`SoundSource`] owns its decode-ahead worker thread and its window as
//! explicit per-source state.
//!
//! # Errors
//!
//! Invalid PCM/spec values are rejected when a [`SoundSource`] is built, a
//! decoder failure is a typed [`SoundError`], and an invalid output spec is a
//! typed [`SoundError`] rather than silent silence. There are no hardcoded
//! chain/size/loop caps.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;

use api::media::{AudioSpec, DecodedAudio};

/// Number of conditional loop-flag slots the engine tracks.
pub const LOOP_FLAG_COUNT: usize = 16;

/// Maximum value a conditional loop flag can hold.
pub const MAX_LOOP_FLAG_VALUE: i32 = 9999;

/// Maximum channel volume (`0..=1`).
pub const MAX_VOLUME: f32 = 1.0;

/// Maximum absolute pan (`-1` full left, `+1` full right).
pub const MAX_PAN: f32 = 1.0;

/// Errors produced by the sound model and mixer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SoundError {
    /// An audio specification is unusable.
    #[error("invalid audio specification: {message}")]
    InvalidSpec {
        /// Human-readable description of the problem.
        message: String,
    },
    /// A source has a channel count the mixer cannot render.
    #[error("unsupported source channel count {channels} (only mono/stereo)")]
    UnsupportedChannels {
        /// Channel count that was rejected.
        channels: u32,
    },
    /// A decoded buffer's length does not match its spec.
    #[error("inconsistent PCM buffer: {len} samples for {frames} frames x {channels} channels")]
    InconsistentBuffer {
        /// Interleaved sample count that was provided.
        len: usize,
        /// Frame count implied by the spec.
        frames: u64,
        /// Channel count implied by the spec.
        channels: u32,
    },
    /// A buffer has no loaded source.
    #[error("sound buffer has no loaded source")]
    NoSource,
    /// An incremental decoder failed while reading or seeking.
    #[error("audio decode failed: {message}")]
    Decode {
        /// Human-readable description of the failure.
        message: String,
    },
    /// The decode-ahead worker thread could not be started.
    #[error("could not start the audio decode thread: {message}")]
    DecodeThread {
        /// Human-readable description of the failure.
        message: String,
    },
}

// ---------------------------------------------------------------------------
// Generic loop information
// ---------------------------------------------------------------------------

/// How a loop link's condition compares a flag against a reference value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LoopCondition {
    /// Always taken.
    None,
    /// Taken when the flag equals the reference value.
    Equal,
    /// Taken when the flag differs from the reference value.
    NotEqual,
    /// Taken when the flag is greater than the reference value.
    Greater,
    /// Taken when the flag is greater than or equal to the reference value.
    GreaterOrEqual,
    /// Taken when the flag is less than the reference value.
    Lesser,
    /// Taken when the flag is less than or equal to the reference value.
    LesserOrEqual,
}

/// A loop link: playback jumps from sample `from` to sample `to` once it
/// reaches `from`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct LoopLink {
    /// Sample granule the loop jumps *from* (the loop end).
    pub from: u64,
    /// Sample granule the loop jumps *to* (the loop start).
    pub to: u64,
    /// Whether the seam should be crossfaded.
    pub smooth: bool,
    /// Flag condition guarding the link.
    pub condition: LoopCondition,
    /// Right-hand value compared against flag `cond_var`.
    pub ref_value: i64,
    /// Flag index the condition reads.
    pub cond_var: i64,
}

impl LoopLink {
    /// Create a loop link between two sample granules.
    pub fn new(
        from: u64,
        to: u64,
        smooth: bool,
        condition: LoopCondition,
        ref_value: i64,
        cond_var: i64,
    ) -> Self {
        Self {
            from,
            to,
            smooth,
            condition,
            ref_value,
            cond_var,
        }
    }

    /// Whether this link's condition holds for `flags`. A non-degenerate
    /// link has `from > to`.
    pub fn matches(&self, flags: &[i32]) -> bool {
        let value = if self.cond_var >= 0 {
            flags.get(self.cond_var as usize).copied().unwrap_or(0)
        } else {
            0
        };
        let value = i64::from(value);
        match self.condition {
            LoopCondition::None => true,
            LoopCondition::Equal => value == self.ref_value,
            LoopCondition::NotEqual => value != self.ref_value,
            LoopCondition::Greater => value > self.ref_value,
            LoopCondition::GreaterOrEqual => value >= self.ref_value,
            LoopCondition::Lesser => value < self.ref_value,
            LoopCondition::LesserOrEqual => value <= self.ref_value,
        }
    }
}

/// A named sample position.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SoundLabel {
    /// Sample granule the label sits at.
    pub position: u64,
    /// Label name.
    pub name: String,
}

impl SoundLabel {
    /// Create a named label at a sample granule.
    pub fn new(position: u64, name: String) -> Self {
        Self { position, name }
    }
}

/// Generic loop/label information for one audio entry, in file order.
///
/// This is the engine-side model only. Format-specific readers live in the
/// compatibility layer and populate it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct LoopInfo {
    /// All loop links, in file order.
    pub links: Vec<LoopLink>,
    /// All labels, in file order.
    pub labels: Vec<SoundLabel>,
}

impl LoopInfo {
    /// Create loop/label information from its entries.
    pub fn new(links: Vec<LoopLink>, labels: Vec<SoundLabel>) -> Self {
        Self { links, labels }
    }

    /// The first unconditional, non-degenerate loop link. `None` when there
    /// are only conditional links or no link at all.
    pub fn active_link(&self) -> Option<&LoopLink> {
        self.links
            .iter()
            .find(|l| l.condition == LoopCondition::None && l.from > l.to)
    }

    /// The first non-degenerate link whose condition holds for `flags`. The
    /// game files have a single link, so file order suffices.
    pub fn active_link_with_flags(&self, flags: &[i32]) -> Option<&LoopLink> {
        self.links
            .iter()
            .find(|l| l.from > l.to && l.matches(flags))
    }
}

// ---------------------------------------------------------------------------
// LoopFlags
// ---------------------------------------------------------------------------

/// The conditional-loop flag surface behind `SoundBuffer.flags`.
///
/// A plain explicit-state value: [`LOOP_FLAG_COUNT`] numeric slots that
/// conditional [`LoopLink`]s are tested against at the loop point. No global
/// registry — the owning [`SoundBuffer`]/[`SoundChannel`] holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct LoopFlags {
    values: [i32; LOOP_FLAG_COUNT],
}

impl LoopFlags {
    /// A fresh flag set with every slot at `0`.
    pub const fn new() -> Self {
        Self {
            values: [0; LOOP_FLAG_COUNT],
        }
    }

    /// Number of flag slots (always [`LOOP_FLAG_COUNT`]).
    pub const fn count(&self) -> usize {
        LOOP_FLAG_COUNT
    }

    /// Read flag `index` (`0` when out of range).
    pub fn get(&self, index: usize) -> i32 {
        self.values.get(index).copied().unwrap_or(0)
    }

    /// Write flag `index`, clamped to `0..=MAX_LOOP_FLAG_VALUE`. An
    /// out-of-range index is ignored.
    pub fn set(&mut self, index: usize, value: i32) {
        if let Some(slot) = self.values.get_mut(index) {
            *slot = value.clamp(0, MAX_LOOP_FLAG_VALUE);
        }
    }

    /// Clear every flag.
    pub fn reset(&mut self) {
        self.values = [0; LOOP_FLAG_COUNT];
    }

    /// The raw flag array, for [`LoopLink::matches`].
    pub fn as_slice(&self) -> &[i32] {
        &self.values
    }
}

impl Default for LoopFlags {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Decoded source
// ---------------------------------------------------------------------------

/// An incremental, seekable PCM decoder supplied to [`SoundSource::streaming`].
///
/// The concrete decoder lives in the bottom media layer; the middle layer
/// owns it and drives it. The stream keeps its own read/seek position:
/// [`read`](AudioStream::read) continues from where the previous call stopped
/// and [`seek`](AudioStream::seek) repositions it. The caller holds the value
/// and calls it with `&mut`, so no decoder state is global or hidden in the
/// bottom service.
pub trait AudioStream: Send + Sync {
    /// The stream's format, including the total frame count when known.
    fn spec(&self) -> AudioSpec;

    /// Decodes up to `out.len()` interleaved samples from the current position
    /// into `out`, returning the number of samples written.
    ///
    /// The count is a whole number of frames and may be less than `out.len()`
    /// when the stream ends; `0` marks the end. A decoder failure is a typed
    /// error, never silence.
    fn read(&mut self, out: &mut [f32]) -> Result<usize, SoundError>;

    /// Repositions the stream so the next [`Self::read`] starts at `frame`.
    fn seek(&mut self, frame: u64) -> Result<(), SoundError>;
}

/// The PCM storage of a [`SoundSource`].
enum Pcm {
    /// Fully decoded PCM supplied by the caller.
    Buffer(Arc<DecodedAudio>),
    /// An incremental decoder with a decode-ahead worker and the window of
    /// frames it has produced.
    Stream(StreamSource),
}

/// The buffer a streaming source's decode-ahead worker fills and the frame
/// thread reads.
struct StreamBuffer {
    /// Interleaved channel count (1 or 2).
    channels: usize,
    /// Frames to decode per pull.
    chunk_frames: usize,
    /// Decoded interleaved samples for `[start, start + frames)`.
    samples: VecDeque<f32>,
    /// Frame index of `samples.front()`.
    start: u64,
    /// The next frame the reader will request.
    cursor: u64,
    /// Maximum frames the worker decodes ahead of `cursor`.
    lookahead_frames: u64,
    /// Frames kept behind `cursor` so an overlapping read does not reseek.
    retain_frames: u64,
    /// Set once `read` reported the end.
    eof: bool,
    /// Total frames once the decoder reported the end.
    total: Option<u64>,
    /// A reseek the worker must perform before decoding on.
    seek_request: Option<u64>,
    /// Set while `decode_entire` needs the whole stream in `samples`.
    materialize: bool,
    /// Set when the source is dropped; the worker exits.
    stop: bool,
    /// Set once playback begins; the worker decodes nothing before then.
    started: bool,
    /// The first decoder failure, reported to every reader.
    error: Option<SoundError>,
}

impl StreamBuffer {
    /// Number of complete decoded frames in the window.
    fn frame_count(&self) -> u64 {
        (self.samples.len() / self.channels) as u64
    }

    /// One past the last decoded frame.
    fn end(&self) -> u64 {
        self.start.saturating_add(self.frame_count())
    }

    /// Total frames the decoder has produced, once the end is known; the
    /// frames decoded so far otherwise.
    fn known_total(&self) -> u64 {
        self.total.unwrap_or_else(|| self.frame_count())
    }

    /// Whether `index` is decoded into the window.
    fn covers(&self, index: u64) -> bool {
        index >= self.start && index < self.end()
    }

    /// Reads the decoded frame at `index`, which the window must cover.
    fn read_frame(&self, index: u64) -> Result<(f32, f32), SoundError> {
        let offset = usize::try_from(index - self.start).map_err(|_| SoundError::Decode {
            message: "decoded frame offset does not fit in memory".to_string(),
        })?;
        let base = offset
            .checked_mul(self.channels)
            .ok_or_else(|| SoundError::Decode {
                message: "decoded frame offset overflowed".to_string(),
            })?;
        let left = *self.samples.get(base).ok_or_else(|| SoundError::Decode {
            message: "decoded window is shorter than the frame index".to_string(),
        })?;
        let right = if self.channels >= 2 {
            *self
                .samples
                .get(base + 1)
                .ok_or_else(|| SoundError::Decode {
                    message: "decoded window is shorter than the frame index".to_string(),
                })?
        } else {
            left
        };
        Ok((left, right))
    }

    /// Drops decoded frames before the retained history window.
    fn trim(&mut self) {
        let keep_from = self
            .cursor
            .saturating_sub(self.retain_frames)
            .min(self.end());
        while self.start < keep_from {
            for _ in 0..self.channels {
                self.samples.pop_front();
            }
            self.start += 1;
        }
    }

    /// Wakes the decode worker when the decoded window is no longer full.
    ///
    /// The worker sleeps while `end >= cursor + lookahead_frames`, so the
    /// reader must wake it exactly once its cursor has advanced far enough
    /// that the window is no longer full. Waking later than that leaves the
    /// worker asleep while the window drains and the reader starves at the
    /// leading edge.
    fn notify_worker(&mut self, shared: &StreamShared) {
        if self.end() < self.cursor.saturating_add(self.lookahead_frames) {
            shared.work_ready.notify_all();
        }
    }
}

/// The condition variables the decode worker and the frame thread signal each
/// other with.
struct StreamShared {
    /// The decoded window and its bookkeeping.
    buffer: Mutex<StreamBuffer>,
    /// Signalled when the window grows (or `eof`/`error` changes): the frame
    /// thread waits here while starved.
    data_ready: Condvar,
    /// Signalled when the frame thread consumes frames or requests a reseek:
    /// the worker waits here while the lookahead is full.
    work_ready: Condvar,
}

/// A streaming decoder plus the decode-ahead worker that owns it.
///
/// The worker is a plain `std::thread` owned by this value: the decoder
/// ([`AudioStream`]) and the buffer are L2-owned state, the thread is a
/// portable std abstraction rather than platform code, and nothing here is a
/// process global. Dropping the value stops and joins the worker.
struct StreamSource {
    /// The window and signalling the worker shares with the frame thread.
    shared: Arc<StreamShared>,
    /// The decode-ahead worker, joined when the source drops.
    worker: Option<JoinHandle<()>>,
}

/// Locks a stream buffer, recovering from a poisoned mutex.
///
/// No code path panics while the buffer is locked, so the recovery cannot
/// expose a half-updated window.
fn lock_buffer(shared: &StreamShared) -> MutexGuard<'_, StreamBuffer> {
    shared
        .buffer
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Decodes ahead into `shared` until the source is dropped.
///
/// The worker owns the decoder. It waits while the lookahead is full, performs
/// a reseek when the reader asks for a frame outside the window, and decodes
/// the whole stream while `decode_entire` materializes it. The frame thread
/// only reads what this function appends.
fn decode_worker(mut stream: Box<dyn AudioStream>, shared: Arc<StreamShared>) {
    loop {
        let mut buffer = lock_buffer(&shared);
        if buffer.stop {
            break;
        }
        if !buffer.started {
            drop(
                shared
                    .work_ready
                    .wait(buffer)
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
            continue;
        }
        if let Some(target) = buffer.seek_request.take() {
            drop(buffer);
            let result = stream.seek(target);
            let mut buffer = lock_buffer(&shared);
            match result {
                Ok(()) => {
                    buffer.samples.clear();
                    buffer.start = target;
                    buffer.cursor = target;
                    buffer.eof = false;
                    buffer.error = None;
                }
                Err(error) => buffer.error = Some(error),
            }
            shared.data_ready.notify_all();
            shared.work_ready.notify_all();
            continue;
        }
        let full = buffer.end() >= buffer.cursor.saturating_add(buffer.lookahead_frames);
        if buffer.eof || (full && !buffer.materialize) {
            if buffer.materialize && buffer.eof {
                buffer.materialize = false;
                shared.data_ready.notify_all();
            }
            drop(
                shared
                    .work_ready
                    .wait(buffer)
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
            continue;
        }
        let channels = buffer.channels;
        let chunk_frames = buffer.chunk_frames;
        drop(buffer);
        let mut chunk = vec![0.0f32; chunk_frames * channels];
        let result = stream.read(&mut chunk);
        let mut buffer = lock_buffer(&shared);
        match result {
            Ok(0) => {
                buffer.eof = true;
                buffer.total = Some(buffer.end());
            }
            Ok(written) => {
                let take = written.min(chunk.len());
                buffer.samples.extend(chunk[..take].iter().copied());
            }
            Err(error) => {
                buffer.error = Some(error);
                buffer.eof = true;
            }
        }
        shared.data_ready.notify_all();
    }
}

impl StreamSource {
    /// Starts the decode-ahead worker for `rate`/`channels`.
    ///
    /// The worker is created immediately but decodes nothing until the first
    /// read or materialize request, so construction, `open` and `play` never
    /// wait on the decoder.
    fn new(stream: Box<dyn AudioStream>, rate: u32, channels: u32) -> Result<Self, SoundError> {
        // Decode a quarter second per pull and keep one second decoded ahead
        // of the cursor, mirroring the reference's decode-ahead buffer length.
        let rate = rate.max(1);
        let channels = usize::try_from(channels).map_err(|_| SoundError::InvalidSpec {
            message: format!("channel count {channels} does not fit in memory"),
        })?;
        let chunk_frames = (rate as usize / 4).max(1);
        let buffer = StreamBuffer {
            channels,
            chunk_frames,
            samples: VecDeque::new(),
            start: 0,
            cursor: 0,
            lookahead_frames: u64::from(rate).max(1),
            retain_frames: chunk_frames as u64,
            eof: false,
            total: None,
            seek_request: None,
            materialize: false,
            stop: false,
            started: false,
            error: None,
        };
        let shared = Arc::new(StreamShared {
            buffer: Mutex::new(buffer),
            data_ready: Condvar::new(),
            work_ready: Condvar::new(),
        });
        let worker_shared = Arc::clone(&shared);
        let worker = std::thread::Builder::new()
            .name("tvp-audio".to_string())
            .spawn(move || decode_worker(stream, worker_shared))
            .map_err(|error| SoundError::DecodeThread {
                message: error.to_string(),
            })?;
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }

    /// Starts the decode-ahead worker.
    ///
    /// Returns immediately: the worker decodes on its own thread. Repeated
    /// calls are a no-op.
    fn start(&self) {
        let mut buffer = lock_buffer(&self.shared);
        if !buffer.started {
            buffer.started = true;
            self.shared.work_ready.notify_all();
        }
    }

    /// Reads `index`, waiting for the worker while the window is starved.
    ///
    /// The frame thread never decodes: a frame outside the window asks the
    /// worker to reseek, and a frame at the leading edge waits for the next
    /// decoded chunk.
    fn frame(&self, index: u64) -> Result<Option<(f32, f32)>, SoundError> {
        let shared = &self.shared;
        let mut buffer = lock_buffer(shared);
        if !buffer.started {
            buffer.started = true;
            shared.work_ready.notify_all();
        }
        loop {
            if let Some(error) = &buffer.error {
                return Err(error.clone());
            }
            if buffer.eof && index >= buffer.end() {
                return Ok(None);
            }
            if index < buffer.start || index > buffer.end() {
                // Backwards, or a forward gap: reseek instead of decoding
                // through the skipped frames.
                buffer.seek_request = Some(index);
                buffer.cursor = index;
                shared.work_ready.notify_all();
            } else if index == buffer.end() {
                // Starved at the leading edge: the worker is decoding.
                buffer.cursor = index;
                shared.work_ready.notify_all();
            }
            if buffer.covers(index) {
                let sample = buffer.read_frame(index)?;
                buffer.cursor = index.saturating_add(1);
                buffer.trim();
                buffer.notify_worker(shared);
                return Ok(Some(sample));
            }
            buffer = shared
                .data_ready
                .wait(buffer)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// Copies up to `frames` decoded frames starting at `start` into `out` as
    /// interleaved `(left, right)` pairs, returning the number copied.
    ///
    /// Frames at or past the end are not copied, so a short return means the
    /// stream ended. `out` must hold `frames * 2` samples. Like
    /// [`Self::frame`], a frame outside the window asks the worker to reseek
    /// and a frame at the leading edge waits for the next decoded chunk; the
    /// difference is that the whole contiguous window is read under one lock.
    fn read_into(&self, out: &mut [f32], start: u64, frames: usize) -> Result<usize, SoundError> {
        let shared = &self.shared;
        let mut buffer = lock_buffer(shared);
        if !buffer.started {
            buffer.started = true;
            shared.work_ready.notify_all();
        }
        loop {
            if let Some(error) = &buffer.error {
                return Err(error.clone());
            }
            // Once the decoder has reported the end, a request at or past the
            // decoded end is the end of the stream, not a forward gap. The
            // per-frame path returns the same way; without this guard,
            // reseeking here reopens the decoder for every trailing output
            // frame, which re-probes a compressed container each time.
            if buffer.eof && start >= buffer.end() {
                return Ok(0);
            }
            if start < buffer.start || start > buffer.end() {
                // Backwards, or a forward gap: reseek instead of decoding
                // through the skipped frames.
                buffer.seek_request = Some(start);
                buffer.cursor = start;
                shared.work_ready.notify_all();
                buffer = shared
                    .data_ready
                    .wait(buffer)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                continue;
            }
            if start == buffer.end() {
                if buffer.eof {
                    return Ok(0);
                }
                // Starved at the leading edge: the worker is decoding.
                buffer.cursor = buffer.cursor.max(start);
                shared.work_ready.notify_all();
                buffer = shared
                    .data_ready
                    .wait(buffer)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                continue;
            }
            let available = buffer.end().saturating_sub(start).min(frames as u64);
            let available = usize::try_from(available).map_err(|_| SoundError::Decode {
                message: "decoded window length does not fit in memory".to_string(),
            })?;
            for (k, pair) in out.chunks_exact_mut(2).take(available).enumerate() {
                let (left, right) = buffer.read_frame(start.saturating_add(k as u64))?;
                pair[0] = left;
                pair[1] = right;
            }
            buffer.cursor = buffer.cursor.max(start.saturating_add(available as u64));
            buffer.trim();
            buffer.notify_worker(shared);
            return Ok(available);
        }
    }

    /// Decodes the whole stream into one buffer, releasing the window.
    fn materialize(&self, spec: AudioSpec) -> Result<DecodedAudio, SoundError> {
        let shared = &self.shared;
        let mut buffer = lock_buffer(shared);
        buffer.started = true;
        buffer.seek_request = Some(0);
        buffer.materialize = true;
        shared.work_ready.notify_all();
        while buffer.materialize || !buffer.eof {
            if let Some(error) = &buffer.error {
                return Err(error.clone());
            }
            buffer = shared
                .data_ready
                .wait(buffer)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        if let Some(error) = &buffer.error {
            return Err(error.clone());
        }
        let samples = Vec::from(std::mem::take(&mut buffer.samples));
        let frames = (samples.len() / buffer.channels) as u64;
        // The window is released; a later read reseeks from the start.
        buffer.start = frames;
        buffer.cursor = frames;
        buffer.eof = true;
        buffer.total = Some(frames);
        let spec = AudioSpec {
            total_frames: Some(frames),
            ..spec
        };
        Ok(DecodedAudio::new(spec, samples))
    }

    /// Total frames once the end is known, the frames decoded so far
    /// otherwise.
    fn known_total(&self) -> u64 {
        lock_buffer(&self.shared).known_total()
    }

    /// Whether the decoder reported a zero-length stream.
    fn is_empty(&self) -> bool {
        lock_buffer(&self.shared).total == Some(0)
    }
}

impl Drop for StreamSource {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            {
                let mut buffer = lock_buffer(&self.shared);
                buffer.stop = true;
            }
            self.shared.work_ready.notify_all();
            self.shared.data_ready.notify_all();
            if worker.join().is_err() {
                log::warn!("sound: the audio decode thread stopped unexpectedly");
            }
        }
    }
}

/// Rejects a sample rate or channel count the mixer cannot use.
fn validate_source_spec(spec: AudioSpec) -> Result<(), SoundError> {
    if spec.sample_rate == 0 {
        return Err(SoundError::InvalidSpec {
            message: "sample rate must be non-zero".to_string(),
        });
    }
    if spec.channels == 0 || spec.channels > 2 {
        return Err(SoundError::UnsupportedChannels {
            channels: spec.channels,
        });
    }
    Ok(())
}

/// Reads one frame out of a fully decoded buffer.
fn buffer_frame(audio: &DecodedAudio, frame: u64) -> Option<(f32, f32)> {
    let channels = usize::try_from(audio.spec.channels).ok()?;
    if channels == 0 {
        return None;
    }
    let index = usize::try_from(frame).ok()?.checked_mul(channels)?;
    let left = *audio.interleaved.get(index)?;
    let right = if channels >= 2 {
        *audio.interleaved.get(index + 1)?
    } else {
        left
    };
    Some((left, right))
}

/// A playable PCM source: either caller-supplied decoded samples or an
/// incremental [`AudioStream`] that decodes as playback advances.
///
/// Construction validates the format so the mixer can assume a consistent,
/// mono-or-stereo source. A materialized source holds its decoded buffer; a
/// streaming source owns a decode-ahead worker (a plain `std::thread`) and a
/// bounded window of frames the worker fills while the frame thread reads it,
/// reseeking when a read leaves the window. The decoder and the window are
/// this value's own, never a global or a bottom service cache.
#[non_exhaustive]
pub struct SoundSource {
    spec: AudioSpec,
    pcm: Pcm,
    info: Option<LoopInfo>,
    /// Whole-file PCM, materialized on demand from a streaming source.
    materialized: OnceLock<DecodedAudio>,
}

impl fmt::Debug for SoundSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SoundSource")
            .field("spec", &self.spec)
            .field("streaming", &self.is_streaming())
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

impl SoundSource {
    /// Wrap decoded PCM, deriving the frame count from the buffer.
    ///
    /// Fails with a typed error when the spec is unusable or the buffer
    /// length disagrees with `sample_rate`/`channels`.
    pub fn new(audio: Arc<DecodedAudio>) -> Result<Self, SoundError> {
        Self::with_loops(audio, None)
    }

    /// Like [`Self::new`], attaching generic loop/label information.
    pub fn with_loops(
        audio: Arc<DecodedAudio>,
        info: Option<LoopInfo>,
    ) -> Result<Self, SoundError> {
        validate_source_spec(audio.spec)?;
        let frames = audio.frame_count();
        let expected = frames
            .checked_mul(u64::from(audio.spec.channels))
            .and_then(|n| usize::try_from(n).ok());
        if expected != Some(audio.interleaved.len()) {
            return Err(SoundError::InconsistentBuffer {
                len: audio.interleaved.len(),
                frames,
                channels: audio.spec.channels,
            });
        }
        let spec = AudioSpec {
            total_frames: Some(frames),
            ..audio.spec
        };
        Ok(Self {
            spec,
            pcm: Pcm::Buffer(audio),
            info,
            materialized: OnceLock::new(),
        })
    }

    /// Wrap an incremental [`AudioStream`] that decodes as playback advances.
    ///
    /// `spec` comes from the container — including its total frame count when
    /// the container reports one — so the duration is known without decoding
    /// the stream. The stream's own [`AudioStream::spec`] must agree on the
    /// sample rate and channel count. A decode-ahead worker is created for
    /// the stream but decodes nothing until playback starts (or the first
    /// frame read), so `open` never waits on the decoder.
    pub fn streaming(
        spec: AudioSpec,
        stream: Box<dyn AudioStream>,
        info: Option<LoopInfo>,
    ) -> Result<Self, SoundError> {
        validate_source_spec(spec)?;
        let stream_spec = stream.spec();
        if stream_spec.sample_rate != spec.sample_rate || stream_spec.channels != spec.channels {
            return Err(SoundError::InvalidSpec {
                message: format!(
                    "stream format {} Hz/{} ch disagrees with the container's {} Hz/{} ch",
                    stream_spec.sample_rate, stream_spec.channels, spec.sample_rate, spec.channels
                ),
            });
        }
        let source = StreamSource::new(stream, spec.sample_rate, spec.channels)?;
        Ok(Self {
            spec,
            pcm: Pcm::Stream(source),
            info,
            materialized: OnceLock::new(),
        })
    }

    /// The decoded PCM, materializing a streaming source on first access.
    ///
    /// A streaming source decodes the whole stream into memory here. The
    /// phase-vocoder filter chain materializes its input before processing;
    /// every other path reads frames through [`Self::frame`].
    ///
    /// # Errors
    ///
    /// Returns the decoder's typed [`SoundError`] when a streaming source
    /// cannot be materialized. The failure is reported to the caller and is
    /// never replaced with an empty buffer.
    pub fn audio(&self) -> Result<&DecodedAudio, SoundError> {
        match &self.pcm {
            Pcm::Buffer(audio) => Ok(&**audio),
            Pcm::Stream(_) => {
                if let Some(audio) = self.materialized.get() {
                    return Ok(audio);
                }
                let audio = self.decode_entire()?;
                Ok(self.materialized.get_or_init(|| audio))
            }
        }
    }

    /// Decodes the whole stream into one buffer.
    ///
    /// A streaming source is rewound to the beginning and drained; the
    /// returned buffer is an independent copy and the decoded window is
    /// released so the source does not keep a second copy of the file.
    ///
    /// # Errors
    ///
    /// A typed [`SoundError::Decode`] when the decoder fails.
    pub fn decode_entire(&self) -> Result<DecodedAudio, SoundError> {
        match &self.pcm {
            Pcm::Buffer(audio) => Ok((**audio).clone()),
            Pcm::Stream(source) => source.materialize(self.spec),
        }
    }

    /// Whether this source decodes incrementally instead of holding every
    /// decoded sample.
    pub fn is_streaming(&self) -> bool {
        matches!(&self.pcm, Pcm::Stream(_))
    }

    /// Starts the decode-ahead worker for a streaming source.
    ///
    /// Returns immediately: the worker decodes on its own thread, so playback
    /// and decoding overlap. A no-op for a materialized source and for a
    /// repeated call.
    pub fn start_decoding(&self) {
        if let Pcm::Stream(source) = &self.pcm {
            source.start();
        }
    }

    /// The source format, with `total_frames` filled in when known.
    pub fn spec(&self) -> AudioSpec {
        AudioSpec {
            total_frames: Some(self.frames()),
            ..self.spec
        }
    }

    /// The container-reported total frame count, or `None` when the stream's
    /// length is only known once it has been decoded to the end.
    pub fn reported_frames(&self) -> Option<u64> {
        self.spec.total_frames
    }

    /// Samples per second.
    pub fn sample_rate(&self) -> u32 {
        self.spec.sample_rate
    }

    /// Interleaved channel count (1 or 2).
    pub fn channels(&self) -> u32 {
        self.spec.channels
    }

    /// Total number of frames.
    ///
    /// For a streaming source whose container did not report a length, this is
    /// the number of frames decoded so far; it reaches the true total once
    /// playback has decoded to the end.
    pub fn frames(&self) -> u64 {
        match self.spec.total_frames {
            Some(frames) => frames,
            None => match &self.pcm {
                Pcm::Buffer(audio) => audio.frame_count(),
                Pcm::Stream(source) => source.known_total(),
            },
        }
    }

    /// Whether the source has no frames.
    pub fn is_empty(&self) -> bool {
        match self.spec.total_frames {
            Some(0) => true,
            Some(_) => false,
            None => match &self.pcm {
                Pcm::Buffer(audio) => audio.frame_count() == 0,
                Pcm::Stream(source) => source.is_empty(),
            },
        }
    }

    /// Duration in seconds.
    pub fn duration_seconds(&self) -> f64 {
        let rate = self.sample_rate();
        if rate == 0 {
            0.0
        } else {
            self.frames() as f64 / f64::from(rate)
        }
    }

    /// The attached loop information, when one was supplied.
    pub fn loops(&self) -> Option<&LoopInfo> {
        self.info.as_ref()
    }

    /// The first unconditional, non-degenerate loop link.
    pub fn loop_link(&self) -> Option<&LoopLink> {
        self.info.as_ref().and_then(LoopInfo::active_link)
    }

    /// The first loop link whose condition holds for `flags`.
    pub fn loop_link_with_flags(&self, flags: &[i32]) -> Option<&LoopLink> {
        self.info
            .as_ref()
            .and_then(|info| info.active_link_with_flags(flags))
    }

    /// The labels, in file order (empty when none were supplied).
    pub fn labels(&self) -> &[SoundLabel] {
        self.info.as_ref().map_or(&[], |info| &info.labels)
    }

    /// Read one sample frame as `(left, right)` (mono duplicated).
    ///
    /// A streaming source reads from the decode-ahead worker's window: a
    /// forward read inside the window returns at once, a read at the leading
    /// edge waits for the worker, and a read outside the window asks the
    /// worker to reseek. A frame at or past the end returns `None`, and a
    /// decoder failure is a typed error, never silence.
    pub fn frame(&self, frame: u64) -> Result<Option<(f32, f32)>, SoundError> {
        if let Some(total) = self.spec.total_frames
            && frame >= total
        {
            return Ok(None);
        }
        match &self.pcm {
            Pcm::Buffer(audio) => Ok(buffer_frame(audio, frame)),
            Pcm::Stream(source) => source.frame(frame),
        }
    }

    /// Copy up to `frames` decoded frames starting at `start_frame` into `out`
    /// as interleaved `(left, right)` pairs, returning the number copied.
    ///
    /// Frames at or past the reported/decoded end are not copied, so a short
    /// return means the source ended there. `out` must hold `frames * 2`
    /// samples. A streaming source takes its decode-ahead buffer lock once for
    /// the whole window rather than once per frame, which is what the mixer's
    /// resampler uses; a decoder failure is a typed error, never silence.
    ///
    /// # Errors
    ///
    /// Returns [`SoundError::Decode`] when the window does not fit in memory
    /// or the decoder fails.
    pub fn read_into(
        &self,
        out: &mut [f32],
        start_frame: u64,
        frames: usize,
    ) -> Result<usize, SoundError> {
        let frames = match self.spec.total_frames {
            Some(total) if start_frame >= total => return Ok(0),
            // The container length caps the read so a frame at or past the
            // reported end is never handed to the decoder.
            Some(total) => {
                usize::try_from((frames as u64).min(total - start_frame)).map_err(|_| {
                    SoundError::Decode {
                        message: "read window does not fit in memory".to_string(),
                    }
                })?
            }
            None => frames,
        };
        let needed = frames.checked_mul(2).ok_or_else(|| SoundError::Decode {
            message: "read window does not fit in memory".to_string(),
        })?;
        if out.len() < needed {
            return Err(SoundError::Decode {
                message: format!(
                    "read window of {frames} frames needs {needed} samples but the buffer holds {}",
                    out.len()
                ),
            });
        }
        match &self.pcm {
            Pcm::Buffer(audio) => {
                let total = audio.frame_count();
                let available = if start_frame >= total {
                    0
                } else {
                    usize::try_from((total - start_frame).min(frames as u64)).map_err(|_| {
                        SoundError::Decode {
                            message: "decoded window length does not fit in memory".to_string(),
                        }
                    })?
                };
                for (k, pair) in out.chunks_exact_mut(2).take(available).enumerate() {
                    let (left, right) = buffer_frame(audio, start_frame.saturating_add(k as u64))
                        .ok_or_else(|| SoundError::Decode {
                        message: "decoded buffer is shorter than the frame index".to_string(),
                    })?;
                    pair[0] = left;
                    pair[1] = right;
                }
                Ok(available)
            }
            Pcm::Stream(source) => source.read_into(out, start_frame, frames),
        }
    }

    /// Whether playback at `seconds` has reached or passed the end.
    pub fn has_ended_at(&self, seconds: f64) -> bool {
        seconds >= self.duration_seconds()
    }

    /// Whether playback at `seconds` is strictly past the end (the
    /// reference's whole-file "done" rule: exactly at the end still plays).
    pub fn playback_ended(&self, seconds: f64) -> bool {
        seconds > self.duration_seconds()
    }
}

// ---------------------------------------------------------------------------
// Fades
// ---------------------------------------------------------------------------

/// A linear volume fade (`SoundBufferBaseIntf.cpp` `Fade`): ramp from
/// `from` to `to` over `duration` seconds, after an optional `delay`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Fade {
    /// Volume at fade start.
    pub from: f32,
    /// Volume when the fade completes.
    pub to: f32,
    /// Ramp duration in seconds.
    pub duration: f64,
    /// Delay before the ramp starts, in seconds (reference `blanktime`).
    pub delay: f64,
    /// Seconds since the fade started.
    pub elapsed: f64,
}

impl Fade {
    /// Start a fade from `from` to `to` over `duration` seconds, after
    /// `delay` seconds.
    pub fn new(from: f32, to: f32, duration: f64, delay: f64) -> Self {
        Self {
            from,
            to,
            duration,
            delay,
            elapsed: 0.0,
        }
    }

    /// Advance the fade by `dt` seconds.
    pub fn advance(&mut self, dt: f64) {
        self.elapsed += dt;
    }

    /// True once the delay and the ramp have both elapsed.
    pub fn finished(&self) -> bool {
        self.elapsed >= self.delay + self.duration
    }

    /// The volume the fade currently dictates.
    pub fn volume(&self) -> f32 {
        if self.elapsed <= self.delay {
            return self.from;
        }
        if self.duration <= 0.0 {
            return self.to.clamp(0.0, MAX_VOLUME);
        }
        let t = ((self.elapsed - self.delay) / self.duration).clamp(0.0, 1.0) as f32;
        (self.from + (self.to - self.from) * t).clamp(0.0, MAX_VOLUME)
    }
}

// ---------------------------------------------------------------------------
// Status and events
// ---------------------------------------------------------------------------

/// Script-visible playback status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Status {
    /// No source loaded.
    #[default]
    Unload,
    /// Loaded, not playing.
    Stop,
    /// Playing (paused still reports `Play`; pause is not a status).
    Play,
}

impl Status {
    /// The reference status string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Status::Unload => "unload",
            Status::Stop => "stop",
            Status::Play => "play",
        }
    }
}

/// An event a [`SoundBuffer::poll`] observed since the previous poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoundEvent {
    /// The script-visible status changed.
    StatusChanged(Status),
    /// An active fade completed (`onFadeCompleted`).
    FadeCompleted,
    /// The playhead crossed a label (`onLabel`).
    Label(String),
}

// ---------------------------------------------------------------------------
// Playback channel
// ---------------------------------------------------------------------------

/// One mixer voice: a source plus its playback state.
///
/// Every field is explicit state owned by a [`Mixer`]; there is no shared
/// cursor and no global channel registry. `SoundChannel` native instances in
/// L3 are thin handles onto these.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SoundChannel {
    id: u64,
    source: Option<Arc<SoundSource>>,
    playing: bool,
    paused: bool,
    volume: f32,
    volume2: f32,
    rate: f64,
    pan: f32,
    pos: [f32; 3],
    flags: LoopFlags,
    /// Whether the source loops.
    pub looping: bool,
    position_seconds: f64,
    /// The latency between the rendered position and the audible one, in
    /// seconds.
    output_latency: f64,
    done: bool,
    fade: Option<Fade>,
    fade_finished: bool,
}

impl SoundChannel {
    fn new(id: u64) -> Self {
        Self {
            id,
            source: None,
            playing: false,
            paused: false,
            volume: MAX_VOLUME,
            volume2: MAX_VOLUME,
            rate: 1.0,
            pan: 0.0,
            pos: [0.0; 3],
            flags: LoopFlags::new(),
            looping: false,
            position_seconds: 0.0,
            output_latency: 0.0,
            done: false,
            fade: None,
            fade_finished: false,
        }
    }

    /// Stable channel id.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The current source, if one has been played.
    pub fn source(&self) -> Option<&Arc<SoundSource>> {
        self.source.as_ref()
    }

    /// True while audio is actually moving (playing, not paused, non-empty
    /// source).
    pub fn is_playing(&self) -> bool {
        self.playing && !self.paused && self.source.as_ref().is_some_and(|s| !s.is_empty())
    }

    /// True after `pause` until `resume`.
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// True once a non-looping source reached its end.
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// The script-visible status derived from the current state.
    pub fn status(&self) -> Status {
        match &self.source {
            None => Status::Unload,
            Some(_) if !self.playing => Status::Stop,
            Some(_) => Status::Play,
        }
    }

    /// Install `source` on the channel without starting playback (the
    /// reference's post-`Open` state: loaded, `stop`).
    pub fn load(&mut self, source: Arc<SoundSource>) {
        self.source = Some(source);
        self.playing = false;
        self.paused = false;
        self.position_seconds = 0.0;
        self.done = false;
        self.fade = None;
        self.fade_finished = false;
    }

    /// Start (or restart) `source` from position 0.
    pub fn play(&mut self, source: Arc<SoundSource>) {
        source.start_decoding();
        self.source = Some(source);
        self.playing = true;
        self.paused = false;
        self.position_seconds = 0.0;
        self.done = false;
        self.fade = None;
        self.fade_finished = false;
    }

    /// Stop playback, keeping the current position (a later `play` restarts
    /// from 0).
    pub fn stop(&mut self) {
        self.playing = false;
        self.paused = false;
        self.done = false;
        self.fade = None;
        self.fade_finished = false;
    }

    /// Pause: the position stops advancing (no-op when not playing).
    pub fn pause(&mut self) {
        if self.playing {
            self.paused = true;
        }
    }

    /// Resume from pause.
    pub fn resume(&mut self) {
        self.paused = false;
    }

    /// Playback position in seconds.
    pub fn position_seconds(&self) -> f64 {
        self.position_seconds
    }

    /// The latency between the rendered position and the audible one, in
    /// seconds.
    ///
    /// The audio device holds a queue of rendered samples, so a voice's
    /// rendered position runs ahead of the position the listener hears. The
    /// value is the queue's depth; the composition root reads it from the
    /// device and hands it to the mixer.
    pub fn output_latency(&self) -> f64 {
        self.output_latency
    }

    /// Set the latency between the rendered position and the audible one, in
    /// seconds (clamped at `>= 0`).
    pub fn set_output_latency(&mut self, seconds: f64) {
        self.output_latency = seconds.max(0.0);
    }

    /// The position the voice has reached at the listener, in seconds.
    ///
    /// This is the rendered position minus the output latency: the position
    /// the sound has actually reached. A voice that is behind its own
    /// rendered position by more than the latency reports `0`.
    pub fn played_position_seconds(&self) -> f64 {
        (self.position_seconds - self.output_latency).max(0.0)
    }

    /// Seek to `seconds` (clamped to `>= 0`).
    pub fn set_position(&mut self, seconds: f64) {
        self.position_seconds = seconds.max(0.0);
    }

    /// The base volume (`0..=1`).
    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// Set the base volume, clamped to `0..=1`; cancels an active fade.
    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, MAX_VOLUME);
        self.fade = None;
        self.fade_finished = false;
    }

    /// The secondary volume (`0..=1`), multiplied with [`Self::volume`].
    pub fn volume2(&self) -> f32 {
        self.volume2
    }

    /// Set the secondary volume, clamped to `0..=1`.
    pub fn set_volume2(&mut self, volume: f32) {
        self.volume2 = volume.clamp(0.0, MAX_VOLUME);
    }

    /// The playback-rate multiplier (`1.0` is native speed).
    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// Set the playback-rate multiplier, clamped to a sane positive range.
    pub fn set_rate(&mut self, rate: f64) {
        self.rate = rate.clamp(0.01, 100.0);
    }

    /// The pan (`-1` full left, `+1` full right).
    pub fn pan(&self) -> f32 {
        self.pan
    }

    /// Set the pan, clamped to `-1..=1`.
    pub fn set_pan(&mut self, pan: f32) {
        self.pan = pan.clamp(-MAX_PAN, MAX_PAN);
    }

    /// The 3D position (`PosX`/`PosY`/`PosZ`).
    pub fn pos(&self) -> [f32; 3] {
        self.pos
    }

    /// Set the 3D position.
    pub fn set_pos(&mut self, x: f32, y: f32, z: f32) {
        self.pos = [x, y, z];
    }

    /// The conditional loop flags consulted for loop-link selection.
    pub fn flags(&self) -> &LoopFlags {
        &self.flags
    }

    /// Mutable access to the conditional loop flags.
    pub fn flags_mut(&mut self) -> &mut LoopFlags {
        &mut self.flags
    }

    /// The per-buffer volume: the fade's ramped value while a fade is active,
    /// else the base volume (reference `GetVolume`; excludes `volume2` and
    /// the global volume).
    pub fn effective_volume(&self) -> f32 {
        match &self.fade {
            Some(fade) => fade.volume(),
            None => self.volume,
        }
    }

    /// The full channel gain: effective volume × `volume2` × 3D attenuation
    /// (but not the mixer's global volume / focus mute).
    pub fn output_gain(&self) -> f32 {
        self.effective_volume() * self.volume2 * self.spatial_attenuation()
    }

    /// Default-distance 3D attenuation for [`Self::pos`].
    ///
    /// The default listener sits at the origin with minimum distance 1,
    /// maximum distance `1e9` and rolloff 1, so the gain is
    /// `min / (min + rolloff * (d - min))`; at the origin it is exactly 1.
    pub fn spatial_attenuation(&self) -> f32 {
        const MIN_DISTANCE: f32 = 1.0;
        const MAX_DISTANCE: f32 = 1.0e9;
        const ROLLOFF: f32 = 1.0;
        let [x, y, z] = self.pos;
        let d = (x * x + y * y + z * z).sqrt();
        if !d.is_finite() {
            return 0.0;
        }
        let d = d.clamp(MIN_DISTANCE, MAX_DISTANCE);
        (MIN_DISTANCE / (MIN_DISTANCE + ROLLOFF * (d - MIN_DISTANCE))).clamp(0.0, 1.0)
    }

    /// Start a linear fade to `to` over `duration` seconds after `delay`
    /// seconds, from the current effective volume.
    pub fn fade(&mut self, to: f32, duration: f64, delay: f64) {
        self.fade = Some(Fade::new(
            self.effective_volume(),
            to.clamp(0.0, MAX_VOLUME),
            duration,
            delay,
        ));
        self.fade_finished = false;
    }

    /// The active fade, if any.
    pub fn active_fade(&self) -> Option<&Fade> {
        self.fade.as_ref()
    }

    /// Whether a fade finished during the last [`Self::advance`] (cleared by
    /// [`SoundBuffer::poll`], `play`, `stop` and `set_volume`).
    pub fn fade_finished(&self) -> bool {
        self.fade_finished
    }

    /// Duration of the current source in seconds (0 when unloaded).
    pub fn duration_seconds(&self) -> f64 {
        self.source.as_ref().map_or(0.0, |s| s.duration_seconds())
    }

    /// Advance the channel by `dt` seconds. Returns true when a non-looping
    /// source reached its end during this step.
    pub fn advance(&mut self, dt: f64) -> bool {
        if let Some(fade) = &mut self.fade {
            fade.advance(dt);
            if fade.finished() {
                self.volume = fade.to.clamp(0.0, MAX_VOLUME);
                self.fade = None;
                self.fade_finished = true;
            }
        }

        let mut became_done = false;
        if !self.is_playing() {
            return became_done;
        }
        let rate = self.rate.max(0.0);
        // The loop link loops `To..From` instead of the whole file.
        let loop_bounds = if self.looping {
            self.source.as_ref().and_then(|s| {
                let sr = f64::from(s.sample_rate().max(1));
                s.loop_link_with_flags(self.flags.as_slice())
                    .map(|l| (l.to as f64 / sr, l.from as f64 / sr))
            })
        } else {
            None
        };
        let loop_end = loop_bounds.map(|(_, end)| end);

        self.position_seconds += dt * rate;
        let duration = self.duration_seconds();
        let at_end = if let Some(end) = loop_end {
            self.position_seconds >= end
        } else {
            self.source
                .as_ref()
                .is_some_and(|s| s.playback_ended(self.position_seconds))
        };

        if at_end {
            if self.looping {
                if let Some((start, end)) = loop_bounds {
                    // Loop the To..From region, keeping the overshoot.
                    let span = (end - start).max(f64::MIN_POSITIVE);
                    let overshoot = (self.position_seconds - end).max(0.0) % span;
                    self.position_seconds = start + overshoot;
                } else if duration.is_finite() {
                    self.position_seconds %= duration;
                } else {
                    self.position_seconds = 0.0;
                }
            } else {
                if duration.is_finite() {
                    self.position_seconds = self.position_seconds.min(duration);
                }
                self.playing = false;
                self.done = true;
                became_done = true;
            }
        }
        became_done
    }
}

// ---------------------------------------------------------------------------
// Sound object
// ---------------------------------------------------------------------------

/// The script-object half of a sound buffer: the loaded source, its
/// conditional loop flags, and the label/status bookkeeping the per-frame
/// poll needs.
///
/// It is deliberately separate from [`SoundChannel`] so one loaded buffer can
/// be replayed without losing its flag state, and so the event surface is
/// testable without a VM.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct SoundBuffer {
    source: Option<Arc<SoundSource>>,
    flags: LoopFlags,
    labels: Vec<SoundLabel>,
    next_label: usize,
    last_sample: f64,
    status: Status,
}

impl SoundBuffer {
    /// A fresh, empty buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Install a decoded source, resetting the label cursor.
    pub fn open(&mut self, source: Arc<SoundSource>) {
        self.labels = source.labels().to_vec();
        self.next_label = 0;
        self.last_sample = 0.0;
        self.status = Status::Unload;
        self.source = Some(source);
    }

    /// Drop the loaded source.
    pub fn close(&mut self) {
        self.source = None;
        self.labels.clear();
        self.next_label = 0;
        self.last_sample = 0.0;
        self.status = Status::Unload;
    }

    /// Whether a source is loaded.
    pub fn is_loaded(&self) -> bool {
        self.source.is_some()
    }

    /// The loaded source, if any.
    pub fn source(&self) -> Option<&Arc<SoundSource>> {
        self.source.as_ref()
    }

    /// The conditional loop flags.
    pub fn flags(&self) -> &LoopFlags {
        &self.flags
    }

    /// Mutable access to the conditional loop flags.
    pub fn flags_mut(&mut self) -> &mut LoopFlags {
        &mut self.flags
    }

    /// The loaded labels.
    pub fn labels(&self) -> &[SoundLabel] {
        &self.labels
    }

    /// The last status reported by [`Self::poll`].
    pub fn status(&self) -> Status {
        self.status
    }

    /// Copy this buffer's flags into `channel` (the mixer consults the
    /// channel's copy at the loop point).
    pub fn apply_flags_to(&self, channel: &mut SoundChannel) {
        *channel.flags_mut() = self.flags.clone();
    }

    /// Install this buffer's source on `channel` without playing it,
    /// carrying the buffer's loop flags over.
    pub fn load_on(&mut self, channel: &mut SoundChannel) -> Result<(), SoundError> {
        let Some(source) = &self.source else {
            return Err(SoundError::NoSource);
        };
        channel.load(source.clone());
        self.apply_flags_to(channel);
        self.next_label = 0;
        self.last_sample = 0.0;
        Ok(())
    }

    /// Reset the label cursor and start `channel` on this buffer's source,
    /// carrying the buffer's loop flags over.
    pub fn play_on(&mut self, channel: &mut SoundChannel) -> Result<(), SoundError> {
        let Some(source) = &self.source else {
            return Err(SoundError::NoSource);
        };
        channel.play(source.clone());
        self.apply_flags_to(channel);
        self.next_label = 0;
        self.last_sample = 0.0;
        Ok(())
    }

    /// Observe `channel` and return the events since the previous poll:
    /// status changes, a completed fade, and labels the playhead
    /// crossed. Clears the channel's `fade_finished` flag.
    ///
    /// The caller (L3) dispatches these to the script object; the model stops
    /// at the event list so it is testable without a VM.
    pub fn poll(&mut self, channel: &mut SoundChannel) -> Vec<SoundEvent> {
        let mut events = Vec::new();

        if channel.fade_finished {
            channel.fade_finished = false;
            events.push(SoundEvent::FadeCompleted);
        }

        if !self.labels.is_empty()
            && let Some(source) = &channel.source
        {
            let rate = f64::from(source.sample_rate().max(1));
            let current = channel.position_seconds * rate;
            if !channel.is_playing() {
                // Keep the watermark aligned so a later restart does not fire
                // labels the playhead never passed.
                self.last_sample = current;
            } else {
                if current < self.last_sample {
                    // A loop wrap or backwards seek re-arms from here.
                    self.next_label = self
                        .labels
                        .partition_point(|l| (l.position as f64) < current);
                }
                while self.next_label < self.labels.len() {
                    let position = self.labels[self.next_label].position;
                    if (position as f64) > current {
                        break;
                    }
                    events.push(SoundEvent::Label(self.labels[self.next_label].name.clone()));
                    self.next_label += 1;
                }
                self.last_sample = current;
            }
        }

        let derived = channel.status();
        if derived != self.status {
            self.status = derived;
            events.push(SoundEvent::StatusChanged(derived));
        }
        events
    }
}

// ---------------------------------------------------------------------------
// Mixer
// ---------------------------------------------------------------------------

/// The clock-driven mixer: a collection of [`SoundChannel`]s advanced by the
/// application's update loop and rendered by [`Mixer::mix`].
///
/// A `Mixer` owns all of its state; there is no process-global mixer.
#[derive(Debug)]
#[non_exhaustive]
pub struct Mixer {
    channels: Vec<SoundChannel>,
    next_id: u64,
    clock: f64,
    /// The latency between the rendered position and the audible one, in
    /// seconds.
    output_latency: f64,
    global_volume: f32,
    global_focus_mode: i32,
    /// Whether the host window is currently active.
    pub app_focused: bool,
    /// Whether the host window is currently minimized.
    pub app_minimized: bool,
}

impl Default for Mixer {
    fn default() -> Self {
        Self {
            channels: Vec::new(),
            // Channel ids start at 1: 0 is the "no channel" sentinel used by
            // the script surface.
            next_id: 1,
            clock: 0.0,
            output_latency: 0.0,
            global_volume: MAX_VOLUME,
            global_focus_mode: 0,
            app_focused: true,
            app_minimized: false,
        }
    }
}

impl Mixer {
    /// A fresh, empty mixer.
    pub fn new() -> Self {
        Self::default()
    }

    /// The channels, in id order.
    pub fn channels(&self) -> &[SoundChannel] {
        &self.channels
    }

    /// Create a channel and return its id.
    pub fn spawn_channel(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let mut channel = SoundChannel::new(id);
        channel.set_output_latency(self.output_latency);
        self.channels.push(channel);
        id
    }

    /// Remove a channel.
    pub fn remove_channel(&mut self, id: u64) {
        self.channels.retain(|c| c.id != id);
    }

    /// Mutable access to a channel by id.
    pub fn channel(&mut self, id: u64) -> Option<&mut SoundChannel> {
        self.channels.iter_mut().find(|c| c.id == id)
    }

    /// Read-only access to a channel by id.
    pub fn channel_ref(&self, id: u64) -> Option<&SoundChannel> {
        self.channels.iter().find(|c| c.id == id)
    }

    /// Move all playing channels forward by `dt` seconds.
    pub fn advance(&mut self, dt: f64) {
        self.clock += dt;
        for channel in &mut self.channels {
            channel.advance(dt);
        }
    }

    /// Advance to an absolute monotonic timestamp (`now` in seconds).
    ///
    /// The internal clock starts at 0, so the first call advances by `now`.
    /// Timestamps that move backwards advance by 0.
    pub fn advance_to(&mut self, now: f64) {
        let dt = (now - self.clock).max(0.0);
        self.advance(dt);
    }

    /// The internal clock in seconds.
    pub fn clock(&self) -> f64 {
        self.clock
    }

    /// The latency between the rendered position and the audible one, in
    /// seconds.
    ///
    /// The audio device holds a queue of rendered samples, so every voice's
    /// rendered position runs ahead of the position the listener hears. The
    /// composition root reads the queue's depth from the device and hands it
    /// here; a voice's [`SoundChannel::played_position_seconds`] is its
    /// rendered position minus this latency.
    pub fn output_latency(&self) -> f64 {
        self.output_latency
    }

    /// Set the latency between the rendered position and the audible one, in
    /// seconds (clamped at `>= 0`), for every voice.
    pub fn set_output_latency(&mut self, seconds: f64) {
        let latency = seconds.max(0.0);
        self.output_latency = latency;
        for channel in &mut self.channels {
            channel.set_output_latency(latency);
        }
    }

    /// The global volume multiplier (`0..=1`).
    pub fn global_volume(&self) -> f32 {
        self.global_volume
    }

    /// Set the global volume multiplier, clamped to `0..=1`.
    pub fn set_global_volume(&mut self, volume: f32) {
        self.global_volume = volume.clamp(0.0, MAX_VOLUME);
    }

    /// The global focus mode (`0` never mute, `1` mute-on-minimize, `2`
    /// mute-on-deactivate).
    pub fn global_focus_mode(&self) -> i32 {
        self.global_focus_mode
    }

    /// Set the global focus mode, clamped to `0..=2`.
    pub fn set_global_focus_mode(&mut self, mode: i32) {
        self.global_focus_mode = mode.clamp(0, 2);
    }

    /// The focus mute gain (`1.0` audible, `0.0` muted).
    pub fn focus_gain(&self) -> f32 {
        let muted = match self.global_focus_mode {
            1 => self.app_minimized,
            2 => !self.app_focused || self.app_minimized,
            _ => false,
        };
        if muted { 0.0 } else { 1.0 }
    }

    /// Render the current mix into `out` (interleaved `f32`).
    ///
    /// The output spec must have a non-zero sample rate and 1 or 2 channels;
    /// anything else is a typed [`SoundError::InvalidSpec`]. The caller owns
    /// `out`; the L1 `AudioDevice` consumes it.
    pub fn mix(&self, out: &mut [f32], out_spec: AudioSpec) -> Result<(), SoundError> {
        mix(
            &self.channels,
            MixSettings {
                global_volume: self.global_volume,
                focus_gain: self.focus_gain(),
            },
            out,
            out_spec,
        )
    }

    /// Render one device chunk and advance the mixer by the chunk's
    /// duration (the reference `render_mix_advancing`).
    ///
    /// This is the device-driven clock: the L1 `AudioDevice` callback calls
    /// it, and each rendered chunk moves the play positions so the next
    /// callback does not replay it. A headless application instead uses
    /// [`Self::advance`]/[`Self::advance_to`]; do not drive both clocks at
    /// once for the same mixer.
    pub fn mix_advancing(
        &mut self,
        out: &mut [f32],
        out_spec: AudioSpec,
    ) -> Result<(), SoundError> {
        self.mix(out, out_spec)?;
        let channels = usize::try_from(out_spec.channels).map_err(|_| SoundError::InvalidSpec {
            message: format!(
                "output channel count {} does not fit in memory",
                out_spec.channels
            ),
        })?;
        if out_spec.sample_rate > 0 && channels > 0 {
            let frames = out.len() / channels;
            if frames > 0 {
                let dt = frames as f64 / f64::from(out_spec.sample_rate);
                self.clock += dt;
                for channel in &mut self.channels {
                    channel.advance(dt);
                }
            }
        }
        Ok(())
    }
}

/// Global gain settings applied on top of each channel's own gain.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct MixSettings {
    /// Global volume multiplier (`0..=1`).
    pub global_volume: f32,
    /// Focus mute gain (`1.0` audible, `0.0` muted).
    pub focus_gain: f32,
}

impl Default for MixSettings {
    fn default() -> Self {
        Self {
            global_volume: MAX_VOLUME,
            focus_gain: 1.0,
        }
    }
}

/// Mix `channels` into `out`, which is first cleared, then clamped to
/// `-1..=1`.
///
/// The PCM buffers are the caller-supplied sources held by each channel; a
/// streaming source decodes forward as it is read. [`Mixer::mix`] is the usual
/// entry point.
pub fn mix(
    channels: &[SoundChannel],
    settings: MixSettings,
    out: &mut [f32],
    out_spec: AudioSpec,
) -> Result<(), SoundError> {
    validate_out_spec(out_spec)?;
    out.fill(0.0);
    for channel in channels {
        if !channel.is_playing() {
            continue;
        }
        let Some(source) = &channel.source else {
            continue;
        };
        let gain = channel.output_gain() * settings.global_volume * settings.focus_gain;
        if gain <= 0.0 {
            continue;
        }
        accumulate_source(
            source,
            channel.position_seconds,
            channel.rate,
            gain,
            channel.pan,
            out,
            out_spec,
        )?;
    }
    for sample in out.iter_mut() {
        *sample = sample.clamp(-1.0, 1.0);
    }
    Ok(())
}

/// Accumulate one source into `out` at `position_seconds`, with playback
/// `rate`, channel `gain` and `pan`, resampling to `out_spec`.
///
/// Existing contents of `out` are added to (use [`mix`] or clear it first).
/// The PCM is the caller-supplied source buffer; nothing is decoded here.
///
/// The conversion to the output rate convolves the source frames around the
/// interpolated one with a windowed-sinc kernel, so a converted track keeps
/// the pitch and the spectrum a source can hold.
pub fn mix_source(
    source: &SoundSource,
    position_seconds: f64,
    rate: f64,
    gain: f32,
    pan: f32,
    out: &mut [f32],
    out_spec: AudioSpec,
) -> Result<(), SoundError> {
    validate_out_spec(out_spec)?;
    accumulate_source(source, position_seconds, rate, gain, pan, out, out_spec)
}

/// The number of source frames the resampler convolves per output frame.
///
/// A windowed sinc needs several frames on either side of the point it
/// interpolates, and its passband reaches `0.5 - 2/taps` of the rate the
/// kernel is scaled to: sixty-five taps keep the passband flat to within a
/// twentieth of that Nyquist frequency, so a converted track keeps the whole
/// spectrum a source can hold, and the kernel's stopband stays below the
/// noise floor of a 16-bit source.
///
/// The count is odd so the kernel is symmetric around the interpolated
/// frame: a symmetric kernel reproduces a ramp exactly, and a ramp is what
/// the mixer's own tests measure against.
const RESAMPLE_TAPS: usize = 65;

/// The frames the kernel reaches back from the interpolated frame.
const RESAMPLE_HALF: usize = RESAMPLE_TAPS / 2;

/// The Blackman window over the resampler's kernel.
///
/// The window is computed once per rendered buffer: it does not depend on
/// the fractional position.
fn resample_window() -> [f64; RESAMPLE_TAPS] {
    let mut window = [0.0f64; RESAMPLE_TAPS];
    for (k, value) in window.iter_mut().enumerate() {
        let t = k as f64 / (RESAMPLE_TAPS - 1) as f64;
        let phase = std::f64::consts::TAU * t;
        *value = 0.42 - 0.5 * phase.cos() + 0.08 * (2.0 * phase).cos();
    }
    window
}

/// The windowed-sinc kernel of one conversion.
///
/// The kernel convolves the source frames around the interpolated one, so it
/// interpolates on the source's sample grid and its passband reaches the
/// source's own Nyquist frequency. A conversion that reads the source faster
/// than the output plays has to narrow that passband: the content above the
/// output's Nyquist frequency would fold back into the band. The scale is
/// the ratio of the two rates, capped at one.
///
/// The kernel's sine is the difference of two products, so the per-tap values
/// are computed once per conversion: `sin(pi·scale·(m - frac))` is
/// `A·cos(pi·scale·frac) - B·sin(pi·scale·frac)` with `A = sin(pi·scale·m)`
/// and `B = cos(pi·scale·m)`.
struct ResampleKernel {
    /// The scale of the kernel's passband.
    scale: f64,
    /// `sin(pi·scale·m)` for every tap.
    sine: [f64; RESAMPLE_TAPS],
    /// `cos(pi·scale·m)` for every tap.
    cosine: [f64; RESAMPLE_TAPS],
}

impl ResampleKernel {
    /// The kernel for a conversion from `src_rate` to `out_rate` that plays
    /// the source at `rate`.
    fn new(src_rate: f64, out_rate: f64, rate: f64) -> Self {
        let scale = kernel_scale(src_rate, out_rate, rate);
        let mut sine = [0.0f64; RESAMPLE_TAPS];
        let mut cosine = [0.0f64; RESAMPLE_TAPS];
        for (k, value) in sine.iter_mut().enumerate() {
            let m = k as f64 - RESAMPLE_HALF as f64;
            let phase = std::f64::consts::PI * scale * m;
            *value = phase.sin();
            cosine[k] = phase.cos();
        }
        Self {
            scale,
            sine,
            cosine,
        }
    }

    /// The windowed-sinc weights for the fractional source position `frac`.
    ///
    /// The kernel is a sinc windowed with a Blackman window. The sinc's
    /// argument is `scale·(m - frac)` with an integer `m`, so every tap's
    /// sine comes from the same `sin(pi·scale·frac)`.
    fn weights(&self, frac: f64, window: &[f64; RESAMPLE_TAPS]) -> [f64; RESAMPLE_TAPS] {
        let phase = std::f64::consts::PI * self.scale * frac;
        let (sin, cos) = (phase.sin(), phase.cos());
        let mut weights = [0.0f64; RESAMPLE_TAPS];
        for (k, weight) in weights.iter_mut().enumerate() {
            let m = k as f64 - RESAMPLE_HALF as f64;
            let distance = self.scale * (m - frac);
            let sinc = if distance.abs() < f64::EPSILON {
                1.0
            } else {
                (self.sine[k] * cos - self.cosine[k] * sin) / (std::f64::consts::PI * distance)
            };
            *weight = sinc * window[k];
        }
        weights
    }
}

/// The scale of the kernel's passband.
///
/// The kernel interpolates on the source's sample grid, so its passband
/// reaches the source's own Nyquist frequency. A conversion that reads the
/// source faster than the output plays has to narrow it: the content above
/// the output's Nyquist frequency would fold back into the band.
///
/// The ratio is the output rate over the source's rate, scaled by the
/// playback rate, and capped at one: an upsampling conversion keeps the
/// source's own band.
fn kernel_scale(src_rate: f64, out_rate: f64, rate: f64) -> f64 {
    let ratio = out_rate / (rate * src_rate);
    if ratio.is_finite() && ratio > 0.0 {
        ratio.min(1.0)
    } else {
        1.0
    }
}

/// Render one source into `out` without revalidating the output spec.
fn accumulate_source(
    source: &SoundSource,
    position_seconds: f64,
    rate: f64,
    gain: f32,
    pan: f32,
    out: &mut [f32],
    out_spec: AudioSpec,
) -> Result<(), SoundError> {
    if out.is_empty() || gain <= 0.0 {
        return Ok(());
    }
    let out_channels = usize::try_from(out_spec.channels).map_err(|_| SoundError::InvalidSpec {
        message: format!(
            "output channel count {} does not fit in memory",
            out_spec.channels
        ),
    })?;
    let out_rate = f64::from(out_spec.sample_rate);
    let src_rate = f64::from(source.sample_rate().max(1));
    let rate = if rate.is_finite() { rate } else { 1.0 };
    let (left_gain, right_gain) = pan_gains(pan);
    let out_frames = out.len() / out_channels;
    let window = resample_window();
    let kernel = ResampleKernel::new(src_rate, out_rate, rate);

    // The source frames the resampler needs are contiguous, so load them into
    // a scratch window in bulk and convolve from there. A streaming source
    // then takes its decode-ahead buffer lock once per window instead of once
    // per interpolated frame. `scratch` holds `(left, right)` pairs for
    // `[window_start, window_start + window_frames)`; frames past the source
    // end are absent and set `end_reached`.
    let mut scratch: Vec<f32> = Vec::new();
    let mut window_start = 0u64;
    let mut window_frames = 0usize;
    let mut end_reached = false;

    for (index, frame) in out.chunks_exact_mut(out_channels).enumerate() {
        let t = position_seconds + index as f64 / out_rate * rate;
        let sample_pos = (t * src_rate).max(0.0);
        if !sample_pos.is_finite() || sample_pos >= u64::MAX as f64 {
            continue;
        }
        let i0 = sample_pos.floor();
        let i0u = i0 as u64;
        let frac = sample_pos - i0;

        // A backwards read, or a forward gap the worker would otherwise
        // decode through, starts a fresh window at the requested frame.
        if i0u < window_start || i0u > window_start.saturating_add(window_frames as u64) {
            window_start = i0u.saturating_sub(RESAMPLE_HALF as u64);
            window_frames = 0;
            end_reached = false;
        }
        if !end_reached {
            // Grow the window until it covers the frames the kernel
            // convolves, requesting the rest of the output as one batch.
            while i0u.saturating_add(RESAMPLE_HALF as u64)
                >= window_start.saturating_add(window_frames as u64)
            {
                let remaining = (out_frames - index) as u64;
                let needed = i0u
                    .saturating_add(RESAMPLE_HALF as u64)
                    .saturating_sub(window_start.saturating_add(window_frames as u64));
                let want = needed.max(remaining.saturating_add(RESAMPLE_HALF as u64));
                let want = usize::try_from(want).map_err(|_| SoundError::Decode {
                    message: "interpolation window does not fit in memory".to_string(),
                })?;
                let want_samples = want.checked_mul(2).ok_or_else(|| SoundError::Decode {
                    message: "interpolation window does not fit in memory".to_string(),
                })?;
                let base = window_frames
                    .checked_mul(2)
                    .ok_or_else(|| SoundError::Decode {
                        message: "interpolation window does not fit in memory".to_string(),
                    })?;
                let grown = base
                    .checked_add(want_samples)
                    .ok_or_else(|| SoundError::Decode {
                        message: "interpolation window does not fit in memory".to_string(),
                    })?;
                scratch.resize(grown, 0.0);
                let loaded_start = window_start.saturating_add(window_frames as u64);
                let written = source.read_into(&mut scratch[base..], loaded_start, want)?;
                if written == 0 {
                    end_reached = true;
                    break;
                }
                window_frames += written;
            }
        }

        // The kernel convolves the frames around the interpolated one. The
        // weights are normalised over the frames the window actually holds,
        // so a track that ends inside the kernel keeps its gain.
        let weights = kernel.weights(frac, &window);
        let mut left = 0.0f64;
        let mut right = 0.0f64;
        let mut weight_sum = 0.0f64;
        for (k, weight) in weights.iter().enumerate() {
            let frame_index = i0u as i64 - RESAMPLE_HALF as i64 + k as i64;
            if frame_index < 0 {
                // The kernel reaches before the start of the track.
                continue;
            }
            let offset = frame_index - window_start as i64;
            if offset < 0 || offset >= window_frames as i64 {
                // The track ended inside the kernel.
                continue;
            }
            let at = offset as usize * 2;
            left += weight * f64::from(scratch[at]);
            right += weight * f64::from(scratch[at + 1]);
            weight_sum += weight;
        }
        if weight_sum.abs() > f64::EPSILON {
            left /= weight_sum;
            right /= weight_sum;
        }
        let left = left as f32;
        let right = right as f32;
        if out_channels == 1 {
            frame[0] += (left * left_gain + right * right_gain) * 0.5 * gain;
        } else {
            frame[0] += left * gain * left_gain;
            frame[1] += right * gain * right_gain;
        }
    }
    Ok(())
}

/// Validate an output spec for the mixer.
fn validate_out_spec(spec: AudioSpec) -> Result<(), SoundError> {
    if spec.sample_rate == 0 {
        return Err(SoundError::InvalidSpec {
            message: "output sample rate must be non-zero".to_string(),
        });
    }
    if spec.channels == 0 || spec.channels > 2 {
        return Err(SoundError::InvalidSpec {
            message: format!("output channels must be 1 or 2, got {}", spec.channels),
        });
    }
    Ok(())
}

/// Pan gains (balance law): `-1` silences the right channel, `+1` silences
/// the left, `0` leaves both at full gain.
fn pan_gains(pan: f32) -> (f32, f32) {
    let p = pan.clamp(-MAX_PAN, MAX_PAN);
    (1.0 - p.max(0.0), 1.0 - (-p).max(0.0))
}

#[cfg(test)]
mod tests {
    use api::media::AudioSpec;

    use super::*;

    /// The magnitude of the kernel's response at `frequency`, in decibels.
    ///
    /// The response is measured on the normalised kernel — the weights are
    /// divided by their sum, the way the mixer convolves with them — so it is
    /// relative to the signal's own level.
    fn kernel_gain_db(
        kernel: &ResampleKernel,
        window: &[f64; RESAMPLE_TAPS],
        frac: f64,
        frequency: f64,
        rate: f64,
    ) -> f64 {
        let weights = kernel.weights(frac, window);
        let sum: f64 = weights.iter().sum();
        let mut real = 0.0;
        let mut imaginary = 0.0;
        for (k, weight) in weights.iter().enumerate() {
            let m = k as f64 - RESAMPLE_HALF as f64;
            let phase = std::f64::consts::TAU * frequency * m / rate;
            let scaled = weight / sum;
            real += scaled * phase.cos();
            imaginary -= scaled * phase.sin();
        }
        let magnitude = (real * real + imaginary * imaginary).sqrt();
        20.0 * magnitude.max(f64::MIN_POSITIVE).log10()
    }

    /// The window is the Blackman window over the kernel: it vanishes at both
    /// of its ends and peaks at its centre, and it is symmetric about that
    /// centre. A window that is not symmetric makes the kernel one-sided, and
    /// the response it produces is neither flat nor linear in phase.
    #[test]
    fn the_resample_window_is_symmetric_about_its_centre() {
        let window = resample_window();
        for k in 0..RESAMPLE_TAPS {
            let mirrored = RESAMPLE_TAPS - 1 - k;
            assert!(
                (window[k] - window[mirrored]).abs() < 1e-12,
                "tap {k} is {} and tap {mirrored} is {}",
                window[k],
                window[mirrored]
            );
        }
        assert!(
            window[0].abs() < 1e-12,
            "the window must vanish at its ends, it starts at {}",
            window[0]
        );
        assert!(
            (window[RESAMPLE_HALF] - 1.0).abs() < 1e-12,
            "the window must peak at its centre, it is {} there",
            window[RESAMPLE_HALF]
        );
    }

    /// The kernel passes the band the output can hold and rejects the band
    /// above it: a conversion to a lower output rate narrows the passband to
    /// the output's Nyquist frequency, so the content above it is removed
    /// instead of folding back into the band.
    #[test]
    fn the_kernel_keeps_the_passband_and_rejects_the_folded_band() {
        let window = resample_window();
        // A 48 kHz source played into a 44.1 kHz output: the output's
        // Nyquist frequency is 22.05 kHz.
        let kernel = ResampleKernel::new(48_000.0, 44_100.0, 1.0);
        for frac in [0.0, 0.5] {
            let passband = kernel_gain_db(&kernel, &window, frac, 1_000.0, 48_000.0);
            assert!(
                passband.abs() < 0.01,
                "the 1 kHz gain is {passband} dB at frac {frac}"
            );
            let upper = kernel_gain_db(&kernel, &window, frac, 20_000.0, 48_000.0);
            assert!(
                upper.abs() < 0.05,
                "the 20 kHz gain is {upper} dB at frac {frac}"
            );
            let folded = kernel_gain_db(&kernel, &window, frac, 23_500.0, 48_000.0);
            assert!(
                folded < -20.0,
                "the 23.5 kHz gain is {folded} dB at frac {frac}, the band above the \
                 output's Nyquist frequency must be rejected"
            );
        }
    }

    /// A conversion that does not lower the rate keeps the source's own
    /// band: the kernel is not narrowed, so the source's top is passed.
    #[test]
    fn an_equal_rate_conversion_keeps_the_source_band() {
        let window = resample_window();
        let kernel = ResampleKernel::new(48_000.0, 48_000.0, 1.0);
        let top = kernel_gain_db(&kernel, &window, 0.0, 23_000.0, 48_000.0);
        assert!(
            top.abs() < 0.01,
            "the 23 kHz gain is {top} dB, an equal rate conversion must not narrow the band"
        );
    }

    /// The scale follows the rates: a conversion to a lower rate narrows the
    /// kernel, a conversion to a higher one keeps it, and a playback rate
    /// counts as a faster read of the source.
    #[test]
    fn the_kernel_scale_follows_the_rates() {
        assert_eq!(kernel_scale(48_000.0, 44_100.0, 1.0), 44_100.0 / 48_000.0);
        assert_eq!(kernel_scale(48_000.0, 48_000.0, 1.0), 1.0);
        assert_eq!(kernel_scale(44_100.0, 48_000.0, 1.0), 1.0);
        assert_eq!(kernel_scale(48_000.0, 48_000.0, 2.0), 0.5);
        // A rate that does not describe a conversion keeps the source's band.
        assert_eq!(kernel_scale(48_000.0, 48_000.0, 0.0), 1.0);
        assert_eq!(kernel_scale(48_000.0, 48_000.0, -1.0), 1.0);
    }

    /// A stream whose decoder reports a hard failure on the first read.
    struct FailingStream {
        spec: AudioSpec,
    }

    impl AudioStream for FailingStream {
        fn spec(&self) -> AudioSpec {
            self.spec
        }

        fn read(&mut self, _out: &mut [f32]) -> Result<usize, SoundError> {
            Err(SoundError::Decode {
                message: "corrupt frame".to_string(),
            })
        }

        fn seek(&mut self, _frame: u64) -> Result<(), SoundError> {
            Ok(())
        }
    }

    /// A decode failure surfaces as a typed error from `audio()` rather than
    /// an empty buffer. Reference: an undecodable stream fails the wave buffer
    /// load instead of yielding silence (`reference/cpp/core/sound/WaveIntf.cpp:750-767`).
    #[test]
    fn materialize_reports_decode_failure_instead_of_silence() {
        let spec = AudioSpec {
            sample_rate: 44_100,
            channels: 1,
            total_frames: Some(44_100),
        };
        let source = SoundSource::streaming(spec, Box::new(FailingStream { spec }), None)
            .expect("stream construction does not decode");
        match source.audio() {
            Ok(audio) => panic!(
                "expected a decode error, got {} frames",
                audio.frame_count()
            ),
            Err(SoundError::Decode { message }) => assert_eq!(message, "corrupt frame"),
            Err(other) => panic!("unexpected error: {other}"),
        }
    }
}
