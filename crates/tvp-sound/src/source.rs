//! Asynchronous audio loading.
//!
//! Opening an audio entry used to decode the whole file on the TJS VM
//! thread: a 5.37 MB BGM measured an **8.5 s** stall and ~125 MB of `f32`
//! PCM. [`AudioTrack`] fixes both:
//!
//! * **Short sounds** (effects, voices) are decoded *off-thread* into a
//!   whole-file [`DecodedAudio`]. The VM thread only reads the compressed
//!   bytes and probes the container ([`crate::source::open_track`]); the
//!   packet decode runs on a background worker.
//! * **Long tracks** (BGM) are decoded lazily into a bounded decode-ahead
//!   [`StreamRing`] instead of being materialized. Memory is capped at
//!   `sample_rate * STREAM_AHEAD_SECONDS` frames plus one chunk of
//!   overflow, regardless of track length.
//!
//! # Concurrency model
//!
//! `AudioTrack` is shared by `Arc` between the mixer (which only *reads*
//! decoded frames) and one decode worker thread (which only *writes*).
//! The audio callback therefore never runs a decode and never blocks on
//! the worker:
//!
//! * the whole-file result lives in a [`OnceLock`] the worker fills once;
//!   readers use `OnceLock::get`, which is lock-free after publication;
//! * the streaming ring is guarded by a short [`Mutex`]; the worker copies
//!   a chunk in, the callback copies an interpolated frame out, and
//!   neither holds the lock while decoding.
//!
//! Cancellation is implicit: when the last external `Arc<AudioTrack>`
//! drops (buffer closed/destroyed or reopened), [`AudioTrack::drop`] sets
//! the worker's cancel flag and wakes its condvar. The worker checks the
//! flag between chunks and exits, dropping the ring it holds.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;

use engine::Storage;

use crate::decode::{
    AudioMetadata, DecodeError, DecodedAudio, StreamDecoder, decode_audio_bytes, probe_audio,
};

/// A whole-file decode is kept in RAM only when the estimated PCM size is
/// at most this many bytes. Longer tracks stream through a bounded ring.
pub const FULL_BUFFER_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// How many seconds of audio the streaming ring decodes ahead. This bounds
/// resident PCM memory to `sample_rate * STREAM_AHEAD_SECONDS` frames.
pub const STREAM_AHEAD_SECONDS: u32 = 2;

/// Frames decoded per worker->ring hand-off.
const STREAM_CHUNK_FRAMES: usize = 8192;

/// Number of decode workers currently alive. Exposed for tests that assert
/// a cancelled/reopened decode actually stopped.
static ACTIVE_DECODES: AtomicUsize = AtomicUsize::new(0);

/// Number of decode worker threads currently running.
pub fn active_decode_workers() -> usize {
    ACTIVE_DECODES.load(Ordering::SeqCst)
}

/// RAII guard that keeps [`ACTIVE_DECODES`] accurate.
struct ActiveWorker;

impl ActiveWorker {
    fn new() -> Self {
        ACTIVE_DECODES.fetch_add(1, Ordering::SeqCst);
        ActiveWorker
    }
}

impl Drop for ActiveWorker {
    fn drop(&mut self) {
        ACTIVE_DECODES.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Spawn a named decode worker, counting it for the lifetime of the job.
///
/// Returns `false` when the OS refused to spawn a thread; the caller then
/// publishes a failure so a track never stays stuck "loading".
fn spawn_worker<F>(name: String, job: F) -> bool
where
    F: FnOnce() + Send + 'static,
{
    match thread::Builder::new()
        .name(format!("tvp-audio-{name}"))
        .spawn(move || {
            let _active = ActiveWorker::new();
            job();
        }) {
        Ok(_handle) => true,
        Err(e) => {
            log::warn!("tvp-sound: failed to spawn decode worker for {name}: {e}");
            false
        }
    }
}

/// The state of an [`AudioTrack`].
enum TrackState {
    /// Whole-file decode. `None` means "still decoding"; the worker fills
    /// it exactly once with either the PCM or an error string.
    Full(Arc<OnceLock<Result<Arc<DecodedAudio>, String>>>),
    /// Bounded streaming decode-ahead ring.
    Stream(Arc<StreamRing>),
}

/// A playable audio source that may still be loading.
///
/// This replaces the old `Arc<DecodedAudio>` in [`crate::mixer::Channel`].
/// It is cheap to clone (it lives behind an `Arc`) and answers every query
/// the mixer and natives need, whether the data is ready yet or not.
pub struct AudioTrack {
    sample_rate: u32,
    channels: u16,
    total_frames: Option<u64>,
    duration: Option<f64>,
    state: TrackState,
    cancel: Arc<AtomicBool>,
}

impl std::fmt::Debug for AudioTrack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match &self.state {
            TrackState::Full(slot) => match slot.get() {
                None => "full:loading",
                Some(Ok(_)) => "full:ready",
                Some(Err(_)) => "full:failed",
            },
            TrackState::Stream(_) => "stream",
        };
        f.debug_struct("AudioTrack")
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("total_frames", &self.total_frames)
            .field("kind", &kind)
            .finish()
    }
}

impl AudioTrack {
    fn new(
        metadata: &AudioMetadata,
        state: TrackState,
        cancel: Arc<AtomicBool>,
    ) -> Arc<AudioTrack> {
        let channels = metadata.channels.max(1);
        let duration = metadata.duration_seconds.or_else(|| {
            metadata
                .total_frames
                .map(|f| f as f64 / f64::from(metadata.sample_rate.max(1)))
        });
        Arc::new(AudioTrack {
            sample_rate: metadata.sample_rate.max(1),
            channels,
            total_frames: metadata.total_frames,
            duration,
            state,
            cancel,
        })
    }

    /// Wrap an already-decoded buffer as a ready track (used by the
    /// synchronous [`crate::mixer::Channel::play`] API and tests).
    pub fn from_decoded(audio: Arc<DecodedAudio>) -> Arc<AudioTrack> {
        let metadata = AudioMetadata {
            sample_rate: audio.sample_rate.max(1),
            channels: audio.channels.max(1),
            total_frames: Some(audio.frames()),
            duration_seconds: Some(audio.duration_seconds()),
        };
        let slot = Arc::new(OnceLock::new());
        let _ = slot.set(Ok(audio));
        AudioTrack::new(
            &metadata,
            TrackState::Full(slot),
            Arc::new(AtomicBool::new(false)),
        )
    }

    /// Start decoding `bytes` fully into memory on a background worker.
    fn start_full(metadata: AudioMetadata, bytes: Vec<u8>, name: String) -> Arc<AudioTrack> {
        let slot = Arc::new(OnceLock::new());
        let cancel = Arc::new(AtomicBool::new(false));
        let track = AudioTrack::new(&metadata, TrackState::Full(slot.clone()), cancel.clone());
        let worker_slot = slot.clone();
        let worker_cancel = cancel.clone();
        let worker_name = name.clone();
        let ok = spawn_worker(name, move || {
            if worker_cancel.load(Ordering::Acquire) {
                return;
            }
            let result = decode_audio_bytes(&bytes, &worker_name)
                .map(Arc::new)
                .map_err(|e| e.to_string());
            let _ = worker_slot.set(result);
        });
        if !ok {
            let _ = slot.set(Err("failed to spawn decode worker".to_string()));
        }
        track
    }

    /// Start decoding `bytes` into a bounded ring buffer on a background
    /// worker.
    fn start_stream(metadata: AudioMetadata, bytes: Vec<u8>, name: String) -> Arc<AudioTrack> {
        let ring = StreamRing::new(metadata.sample_rate.max(1), metadata.channels.max(1));
        let cancel = Arc::new(AtomicBool::new(false));
        let track = AudioTrack::new(&metadata, TrackState::Stream(ring.clone()), cancel.clone());
        let worker_ring = ring.clone();
        let worker_cancel = cancel.clone();
        let worker_name = name.clone();
        let ok = spawn_worker(name, move || {
            run_stream(worker_ring, worker_cancel, bytes, worker_name);
        });
        if !ok {
            ring.fail("failed to spawn decode worker".to_string());
        }
        track
    }

    /// Samples per second.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Channel count (1 or 2 for playable sources).
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Total frames when the container stated it (also `Some` once a
    /// whole-file decode finishes).
    pub fn total_frames(&self) -> Option<u64> {
        self.total_frames
    }

    /// Duration in seconds when known.
    pub fn known_duration_seconds(&self) -> Option<f64> {
        self.duration
    }

    /// Whether this track streams from a bounded ring rather than holding
    /// the whole file in memory.
    pub fn is_streaming(&self) -> bool {
        matches!(self.state, TrackState::Stream(_))
    }

    /// Whether the source is decoded enough to play (whole-file: decoded;
    /// streaming: metadata known, frames arrive incrementally).
    pub fn is_ready(&self) -> bool {
        match &self.state {
            TrackState::Full(slot) => slot.get().is_some(),
            TrackState::Stream(_) => true,
        }
    }

    /// Whether the whole-file decode failed.
    pub fn is_failed(&self) -> bool {
        match &self.state {
            TrackState::Full(slot) => matches!(slot.get(), Some(Err(_))),
            TrackState::Stream(ring) => ring.error().is_some(),
        }
    }

    /// Whether a ready track has no playable samples.
    pub fn is_empty(&self) -> bool {
        match &self.state {
            TrackState::Full(slot) => match slot.get() {
                Some(Ok(a)) => a.samples.is_empty() || a.channels == 0,
                _ => true,
            },
            TrackState::Stream(_) => false,
        }
    }

    /// Playback duration in seconds, or `f64::INFINITY` when the container
    /// did not state it (the mixer then uses end-of-stream detection).
    pub fn duration_seconds(&self) -> f64 {
        self.duration.unwrap_or(f64::INFINITY)
    }

    /// Read one sample frame (mono duplicated to both channels).
    ///
    /// Returns `None` when the frame is not available yet (loading,
    /// underrun, or past the end) — the mixer renders silence rather than
    /// blocking.
    pub fn frame(&self, frame: u64) -> Option<(f32, f32)> {
        match &self.state {
            TrackState::Full(slot) => {
                let audio = slot.get()?.as_ref().ok()?;
                let ch = usize::from(audio.channels);
                if ch == 0 || frame >= audio.frames() {
                    return None;
                }
                let i = frame as usize * ch;
                let l = audio.samples[i];
                let r = if ch >= 2 { audio.samples[i + 1] } else { l };
                Some((l, r))
            }
            TrackState::Stream(ring) => ring.frame(frame),
        }
    }

    /// Release decoded frames before `seconds` (streaming only). Called as
    /// playback advances so the worker can refill the ring.
    pub fn release_before(&self, seconds: f64) {
        if let TrackState::Stream(ring) = &self.state {
            let frame = (seconds.max(0.0) * f64::from(self.sample_rate)) as u64;
            ring.release_before(frame);
        }
    }

    /// Request a decoder seek to `seconds` (streaming only). No-op for a
    /// whole-file source.
    pub fn request_seek_seconds(&self, seconds: f64) {
        if let TrackState::Stream(ring) = &self.state {
            let frame = (seconds.max(0.0) * f64::from(self.sample_rate)) as u64;
            ring.request_seek(frame);
        }
    }

    /// Whether a stream has been fully produced up to `seconds` (used for
    /// tracks whose duration the container did not state).
    pub fn has_ended_at(&self, seconds: f64) -> bool {
        if let Some(d) = self.duration {
            return seconds >= d;
        }
        match &self.state {
            TrackState::Full(slot) => slot
                .get()
                .and_then(|r| r.as_ref().ok())
                .is_some_and(|a| seconds >= a.duration_seconds()),
            TrackState::Stream(ring) => ring.playback_ended(seconds, self.sample_rate),
        }
    }

    /// Whether playback has consumed every frame the source actually
    /// produced.
    ///
    /// Unlike [`Self::has_ended_at`], this does **not** short-circuit on a
    /// container-declared duration. The device clock clamps a streaming
    /// position to the decoded watermark, so it can reach the declared
    /// duration exactly without ever exceeding it; this is what lets the
    /// channel flip to `done` at that point. A whole-file source keeps the
    /// reference's exact-end semantics (done only strictly past the end).
    pub fn playback_ended(&self, seconds: f64) -> bool {
        match &self.state {
            TrackState::Full(slot) => slot
                .get()
                .and_then(|r| r.as_ref().ok())
                .is_some_and(|a| seconds > a.duration_seconds()),
            TrackState::Stream(ring) => ring.playback_ended(seconds, self.sample_rate),
        }
    }

    /// Rewind a looping streaming source to the beginning.
    pub fn on_loop_wrap(&self) {
        self.request_seek_seconds(0.0);
    }

    /// Frames currently resident in the streaming ring (0 for whole-file).
    pub fn buffered_frames(&self) -> u64 {
        match &self.state {
            TrackState::Full(_) => 0,
            TrackState::Stream(ring) => ring.buffered_frames(),
        }
    }

    /// For a streaming track, the absolute end of decoded audio in seconds
    /// (the playback watermark). `None` for a whole-file track, whose frames
    /// are all available the moment it is ready.
    ///
    /// The mixer must never advance a streaming channel's position past this
    /// watermark: doing so silently skips the undecoded head of the track
    /// (the `open`→`play` race) and, once the wall clock is further ahead
    /// than the whole ring, drops every decoded frame and plays permanent
    /// silence.
    pub fn available_end_seconds(&self) -> Option<f64> {
        match &self.state {
            TrackState::Full(_) => None,
            TrackState::Stream(ring) => {
                Some(ring.available_end() as f64 / f64::from(self.sample_rate.max(1)))
            }
        }
    }

    /// Capacity of the streaming ring in frames (0 for whole-file).
    pub fn ring_capacity_frames(&self) -> u64 {
        match &self.state {
            TrackState::Full(_) => 0,
            TrackState::Stream(ring) => ring.capacity_frames(),
        }
    }

    /// The resident PCM bytes for a fully-buffered track (0 while loading
    /// or streaming), used by tests to assert memory is bounded.
    pub fn resident_pcm_bytes(&self) -> usize {
        match &self.state {
            TrackState::Full(slot) => slot
                .get()
                .and_then(|r| r.as_ref().ok())
                .map_or(0, |a| a.samples.len() * std::mem::size_of::<f32>()),
            TrackState::Stream(_) => 0,
        }
    }
}

impl Drop for AudioTrack {
    fn drop(&mut self) {
        // The last external reference is gone (closed/destroyed/reopened):
        // cancel the worker and wake it if it is waiting for ring space.
        self.cancel.store(true, Ordering::Release);
        if let TrackState::Stream(ring) = &self.state {
            ring.wake_all();
        }
    }
}

// ---------------------------------------------------------------------------
// synchronous entry point used by the natives
// ---------------------------------------------------------------------------

/// Read `name` from `storage`, probe its metadata, and start decoding it on
/// a background worker.
///
/// Only the compressed read (a few MB at most) and the header probe run on
/// the calling (VM) thread; the expensive packet decode — whole-file for
/// short sounds, streaming for long ones — runs off-thread. The returned
/// track is immediately playable (the whole-file variant renders silence
/// until its buffer lands, then plays).
pub fn open_track(
    storage: &Arc<Mutex<Storage>>,
    name: &str,
) -> Result<Arc<AudioTrack>, DecodeError> {
    let bytes = {
        let mut storage = storage
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        storage.read(name)?
    };
    open_track_bytes(bytes, name)
}

/// Like [`open_track`] but takes the already-read (compressed) bytes.
pub fn open_track_bytes(bytes: Vec<u8>, name: &str) -> Result<Arc<AudioTrack>, DecodeError> {
    let metadata = probe_audio(&bytes, name)?;
    // Estimate the decoded PCM size from the container metadata; fall back
    // to a generous bytes->PCM ratio when the container omits the frame
    // count. Above the cap the track streams instead of materializing.
    let estimated_pcm = metadata
        .total_frames
        .map(|f| {
            f.saturating_mul(u64::from(metadata.channels.max(1)))
                .saturating_mul(4)
        })
        .unwrap_or_else(|| (bytes.len() as u64).saturating_mul(24));
    if estimated_pcm > FULL_BUFFER_MAX_BYTES {
        log::info!(
            "decode_audio: {name}: streaming {} Hz, {} ch, {} frames (decode-ahead {STREAM_AHEAD_SECONDS}s)",
            metadata.sample_rate.max(1),
            metadata.channels.max(1),
            metadata
                .total_frames
                .map_or_else(|| "?".to_string(), |f| f.to_string()),
        );
        Ok(AudioTrack::start_stream(metadata, bytes, name.to_string()))
    } else {
        Ok(AudioTrack::start_full(metadata, bytes, name.to_string()))
    }
}

// ---------------------------------------------------------------------------
// streaming worker
// ---------------------------------------------------------------------------

/// Decode `bytes` into `ring` until EOF, cancellation, or error.
fn run_stream(ring: Arc<StreamRing>, cancel: Arc<AtomicBool>, bytes: Vec<u8>, name: String) {
    let mut decoder = match StreamDecoder::open(&bytes, &name) {
        Ok(d) => d,
        Err(e) => {
            log::warn!("tvp-sound: streaming decode failed to open {name}: {e}");
            ring.fail(e.to_string());
            return;
        }
    };

    // Frames that have been decoded but not yet copied into the ring
    // (the ring was momentarily full).
    let mut pending: Vec<f32> = Vec::new();
    // Frames to drop after a seek (the decoder always restarts at 0).
    let mut skip_remaining: u64 = 0;

    loop {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        // A seek (explicit or a loop wrap) discards the ring and restarts
        // the decoder from the beginning, then skips to the target.
        if let Some(target) = ring.take_seek() {
            match StreamDecoder::open(&bytes, &name) {
                Ok(d) => decoder = d,
                Err(e) => {
                    log::warn!("tvp-sound: streaming reseek failed for {name}: {e}");
                    ring.fail(e.to_string());
                    return;
                }
            }
            pending.clear();
            skip_remaining = target;
            continue;
        }

        if pending.is_empty() {
            match decoder.next_chunk(STREAM_CHUNK_FRAMES) {
                Ok(Some(chunk)) => pending = chunk,
                Ok(None) => {
                    ring.mark_eof();
                    break;
                }
                Err(e) => {
                    if !cancel.load(Ordering::Acquire) {
                        log::warn!("tvp-sound: streaming decode error for {name}: {e}");
                        ring.fail(e.to_string());
                    }
                    break;
                }
            }
        }

        let channels = usize::from(decoder.channels().max(1));
        if skip_remaining > 0 {
            let frames = (pending.len() / channels) as u64;
            if frames <= skip_remaining {
                skip_remaining -= frames;
                pending.clear();
                continue;
            }
            let drop = skip_remaining as usize * channels;
            pending.drain(0..drop);
            skip_remaining = 0;
        }

        if !ring.wait_for_space(&cancel) {
            break;
        }
        let pushed = ring.push(&pending);
        if pushed > 0 {
            let consumed = pushed as usize * channels;
            pending.drain(0..consumed);
        }
    }
}

// ---------------------------------------------------------------------------
// bounded ring buffer
// ---------------------------------------------------------------------------

/// Mutable ring state, guarded by [`StreamRing::state`].
struct RingState {
    data: Vec<f32>,
    cap_frames: u64,
    channels: usize,
    /// Absolute frame index of the oldest retained frame.
    base: u64,
    /// Number of valid frames stored (`base .. base + len`).
    len: u64,
    /// Absolute number of frames ever produced (for end detection).
    produced: u64,
    eof: bool,
    error: Option<String>,
    /// Pending seek target, consumed by the worker.
    seek: Option<u64>,
}

/// A bounded decode-ahead ring of interleaved `f32` frames.
///
/// The worker appends (`push`) and the mixer reads by absolute frame index
/// (`frame`); `release_before` frees frames the playback position has
/// passed. The lock is only ever held for a memory copy, never across a
/// decode or a device callback.
pub struct StreamRing {
    state: Mutex<RingState>,
    space: Condvar,
}

impl StreamRing {
    /// Create a ring holding `STREAM_AHEAD_SECONDS` of audio.
    fn new(sample_rate: u32, channels: u16) -> Arc<StreamRing> {
        let channels = usize::from(channels.max(1));
        let cap_frames = u64::from(sample_rate.max(1)) * u64::from(STREAM_AHEAD_SECONDS);
        Arc::new(StreamRing {
            state: Mutex::new(RingState {
                data: vec![0.0; cap_frames as usize * channels],
                cap_frames,
                channels,
                base: 0,
                len: 0,
                produced: 0,
                eof: false,
                error: None,
                seek: None,
            }),
            space: Condvar::new(),
        })
    }

    /// Append as many whole frames of `samples` as fit; returns the number
    /// of frames copied.
    fn push(&self, samples: &[f32]) -> u64 {
        let mut st = lock_ring(&self.state);
        if st.len >= st.cap_frames {
            return 0;
        }
        let channels = st.channels;
        let frames = samples.len() / channels;
        let n = frames.min((st.cap_frames - st.len) as usize);
        for f in 0..n {
            let dst = ((st.base + st.len + f as u64) % st.cap_frames) as usize * channels;
            let src = f * channels;
            st.data[dst..dst + channels].copy_from_slice(&samples[src..src + channels]);
        }
        st.len += n as u64;
        st.produced += n as u64;
        n as u64
    }

    /// Block until there is room, a seek is pending, or the track is
    /// cancelled. Returns `false` when the worker should stop.
    fn wait_for_space(&self, cancel: &AtomicBool) -> bool {
        let mut st = lock_ring(&self.state);
        while st.len >= st.cap_frames && st.seek.is_none() && !cancel.load(Ordering::Acquire) {
            st = self
                .space
                .wait(st)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        !cancel.load(Ordering::Acquire)
    }

    /// Read one frame by absolute index (mono duplicated).
    fn frame(&self, idx: u64) -> Option<(f32, f32)> {
        let st = lock_ring(&self.state);
        if idx < st.base || idx >= st.base + st.len {
            return None;
        }
        let off = (idx % st.cap_frames) as usize * st.channels;
        let l = st.data[off];
        let r = if st.channels >= 2 {
            st.data[off + 1]
        } else {
            l
        };
        Some((l, r))
    }

    /// Drop frames strictly before `frame` and wake the worker.
    fn release_before(&self, frame: u64) {
        {
            let mut st = lock_ring(&self.state);
            if frame <= st.base {
                return;
            }
            let end = st.base + st.len;
            let target = frame.min(end);
            let drop = target - st.base;
            st.base = target;
            st.len -= drop;
        }
        self.space.notify_all();
    }

    /// Ask the worker to restart decoding at absolute `frame`.
    ///
    /// The buffered window is dropped immediately (not when the worker gets
    /// around to the seek) so the playback watermark jumps to the target at
    /// once: the mixer then holds the (new) position until the worker
    /// republishes frames there, instead of briefly advancing over the stale
    /// window and drifting the seek.
    fn request_seek(&self, frame: u64) {
        {
            let mut st = lock_ring(&self.state);
            st.seek = Some(frame);
            st.base = frame;
            st.len = 0;
            st.produced = frame;
            st.eof = false;
            st.error = None;
        }
        self.space.notify_all();
    }

    /// Consume a pending seek: reset the ring to start at `target`.
    fn take_seek(&self) -> Option<u64> {
        let mut st = lock_ring(&self.state);
        let target = st.seek.take()?;
        st.base = target;
        st.len = 0;
        st.produced = target;
        st.eof = false;
        st.error = None;
        Some(target)
    }

    /// Mark the producer finished.
    fn mark_eof(&self) {
        let mut st = lock_ring(&self.state);
        st.eof = true;
    }

    /// Mark the producer failed (treated as an immediate end).
    fn fail(&self, reason: String) {
        let mut st = lock_ring(&self.state);
        st.error = Some(reason);
        st.eof = true;
    }

    /// The producer error, if any.
    fn error(&self) -> Option<String> {
        lock_ring(&self.state).error.clone()
    }

    /// Whether the producer has finished and playback has consumed
    /// everything produced up to `seconds` (the declared duration is
    /// ignored — see [`crate::source::AudioTrack::playback_ended`]).
    fn playback_ended(&self, seconds: f64, sample_rate: u32) -> bool {
        let st = lock_ring(&self.state);
        if !st.eof {
            return false;
        }
        let pos = (seconds.max(0.0) * f64::from(sample_rate.max(1))) as u64;
        pos >= st.produced
    }

    fn buffered_frames(&self) -> u64 {
        lock_ring(&self.state).len
    }

    /// Absolute frame index one past the newest decoded frame.
    fn available_end(&self) -> u64 {
        let st = lock_ring(&self.state);
        st.base + st.len
    }

    fn capacity_frames(&self) -> u64 {
        lock_ring(&self.state).cap_frames
    }

    /// Wake a worker blocked in [`Self::wait_for_space`].
    fn wake_all(&self) {
        self.space.notify_all();
    }
}

/// Lock the ring, recovering from poisoning (a panic must never poison the
/// audio path permanently).
fn lock_ring(state: &Mutex<RingState>) -> std::sync::MutexGuard<'_, RingState> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ring that writes at `frame % capacity` must read back the same
    /// slot. The original implementation read linearly from `base`, so as
    /// soon as `release_before` advanced `base` (every playback step) the
    /// read and write cursors diverged and playback read the wrong — or
    /// silent — samples.
    #[test]
    fn ring_reads_back_written_frames_across_release_and_wrap() {
        let cap = 48000 * u64::from(STREAM_AHEAD_SECONDS);
        let ring = StreamRing::new(48000, 1);

        // Fill the ring, encoding the absolute frame index in the sample.
        let first: Vec<f32> = (0..cap).map(|i| i as f32).collect();
        assert_eq!(ring.push(&first), cap);
        assert_eq!(ring.frame(0), Some((0.0, 0.0)));
        assert_eq!(
            ring.frame(cap - 1),
            Some(((cap - 1) as f32, (cap - 1) as f32))
        );

        // Consume all but the last 10 frames. `base` is now non-zero.
        ring.release_before(cap - 10);
        assert_eq!(ring.frame(cap - 11), None, "released frame must be gone");
        assert_eq!(
            ring.frame(cap - 10),
            Some(((cap - 10) as f32, (cap - 10) as f32))
        );

        // Push the next frames: the ring is nearly empty (10 frames
        // retained), so all 20 fit and the write wraps the buffer.
        let second: Vec<f32> = (cap..cap + 20).map(|i| i as f32).collect();
        assert_eq!(ring.push(&second), 20);
        for idx in (cap - 10)..(cap + 20) {
            assert_eq!(
                ring.frame(idx),
                Some((idx as f32, idx as f32)),
                "frame {idx} read back wrong"
            );
        }
        assert_eq!(ring.available_end(), cap + 20);
    }

    /// A seek must immediately publish the new watermark (and drop the old
    /// window) so the mixer holds the new position instead of advancing over
    /// stale audio.
    #[test]
    fn ring_seek_resets_window_before_worker_consumes_it() {
        let ring = StreamRing::new(48000, 1);
        let first: Vec<f32> = (0..1000).map(|i| i as f32).collect();
        ring.push(&first);
        assert_eq!(ring.available_end(), 1000);

        ring.request_seek(500);
        assert_eq!(
            ring.available_end(),
            500,
            "watermark jumps to the seek target"
        );
        assert_eq!(
            ring.buffered_frames(),
            0,
            "old window is dropped immediately"
        );
        assert_eq!(ring.frame(500), None);

        assert_eq!(ring.take_seek(), Some(500));
        let resumed: Vec<f32> = (500..600).map(|i| i as f32).collect();
        ring.push(&resumed);
        assert_eq!(ring.frame(500), Some((500.0, 500.0)));
        assert_eq!(ring.available_end(), 600);
    }
}
