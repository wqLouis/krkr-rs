//! `VideoOverlay` native class — non-throwing stub of the reference
//! `tTJSNC_VideoOverlay` (`reference/cpp/core/visual/VideoOvlIntf.cpp` +
//! `VideoOvlImpl.{h,cpp}`) with **real** MPEG-1/2 metadata and a real
//! clock-driven playback state machine.
//!
//! The reference `VideoOverlay` is a **standalone** native class (it does
//! *not* derive from `Layer`): it owns a rectangle/visibility/playback state
//! and exposes `layer1`/`layer2` properties plus video decoding. The game's
//! `MovieLayer` (`system/system_movie.tjs`) is a *script* class that extends
//! this native base and supplies its own `Layer`; the separate
//! `layerExMovie.dll` plugin (`reference/cpp/plugins/layerExMovie.cpp`)
//! derives from `Layer` instead and is a different, newer interface.
//!
//! # What is real here
//!
//! * [`open`](VideoOverlay) reads the file through the mounted game storage
//!   ([`engine::Storage`], the same storage the other natives use; injected
//!   via [`set_video_storage`] or lazily mounted from
//!   `System.dataPath`/`project_dir`) and parses the MPEG-1/2 sequence
//!   header + sequence extension for the coded size and frame rate, and
//!   scans picture start codes / PES stream ids for the frame count and
//!   stream inventory. This drives `originalWidth`, `originalHeight`,
//!   `fps`, `totalFrame`, `numberOfFrame`, `totalTime` and
//!   `numberOfAudioStream`/`numberOfVideoStream`.
//! * A clock-driven state machine: `play`/`pause`/`stop`/`rewind` move
//!   `position` (ms) and `frame` off the engine tick clock; `loop` and
//!   `setSegmentLoop` wrap the frame range; `setPeriodEvent` fires the
//!   script `onPeriod` callback (with `perPeriod`/`perLoop`/`perSegLoop`)
//!   when the frame is reached; `onStatusChanged` fires on status changes;
//!   `setTransitionCompleteCall` fires when a non-looping stream (or a
//!   segment loop wrap) completes.
//!
//! # What is NOT real (pixel decode)
//!
//! There is no video codec in krkr-rs, so **no pixel frames are decoded**
//! and nothing is drawn: `layer1`/`layer2` are accepted and ignored. The
//! metadata and the timing/event surface above are real, which is enough
//! for `EnvEffectFilter`/`MovieLayer` to run their timers and sound-cue
//! logic instead of having the effect disabled. Adding real frames needs an
//! MPEG decoder (e.g. an FFmpeg binding) feeding a `Layer` bitmap — out of
//! scope for this stub.
//!
//! # Timer driving
//!
//! There is no decoder thread, so the state machine is advanced once per
//! frame by [`video_overlay_poll`], which [`crate::continuous_handler_poll`]
//! calls on the same clock as the other natives. Instances are registered in
//! a process-global active set while playing.
//!
//! Omitted on purpose (unused by the game, and plain object members on a
//! native instance read as void / accept writes without throwing anyway):
//! the reference's `contrast`/`brightness`/`hue`/`saturation` families and
//! the event dispatcher methods `onCallbackCommand`/`onFrameUpdate` — the
//! game assigns its own callbacks to the event names it uses.

use std::collections::HashSet;
use std::ffi::{c_char, c_int, c_void};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};

use engine::Storage;
use tjs2_sys::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef,
    Tjs2Engine, TjsValue, Value,
};

use crate::{
    args, context_engine, report_error, set_int_out, set_void_out, value_as_i64, value_as_string,
};

#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

// -- playback event reasons (match the TVP globals) -------------------------

const PER_LOOP: i64 = 0;
const PER_PERIOD: i64 = 1;
const PER_PREPARE: i64 = 2;
const PER_SEG_LOOP: i64 = 3;

/// Frame rates indexed by the MPEG `frame_rate_code` (ISO/IEC 13818-2),
/// including the 1000/1001 pulldown rates.
const FRAME_RATES: [f64; 9] = [
    0.0,
    24000.0 / 1001.0,
    24.0,
    25.0,
    30000.0 / 1001.0,
    30.0,
    50.0,
    60000.0 / 1001.0,
    60.0,
];

// ---------------------------------------------------------------------------
// Process-global storage + active playback registry
// ---------------------------------------------------------------------------

/// Explicit game storage override (e.g. the engine's already-mounted
/// storage). When `None`, [`open`](VideoOverlay) falls back to a disk read
/// and then to lazily mounting `System.project_dir`.
static VIDEO_STORAGE: LazyLock<Mutex<Option<Arc<Mutex<Storage>>>>> =
    LazyLock::new(|| Mutex::new(None));

/// Lazily mounted fallback storage keyed by the project dir it was mounted
/// from (avoids reopening every `*.xp3` on each `open`).
type FallbackStorage = Mutex<Option<(PathBuf, Arc<Mutex<Storage>>)>>;
static FALLBACK_STORAGE: LazyLock<FallbackStorage> = LazyLock::new(|| Mutex::new(None));

/// Addresses of instances currently playing (advanced once per frame).
static ACTIVE: LazyLock<Mutex<HashSet<usize>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// Install the game storage `VideoOverlay.open` reads through. The app can
/// point this at the storage it already mounted; when it is never called the
/// overlay lazily mounts `System.project_dir` itself.
pub fn set_video_storage(storage: Option<Arc<Mutex<Storage>>>) {
    *VIDEO_STORAGE.lock().unwrap_or_else(|p| p.into_inner()) = storage;
}

fn set_active(inst: *mut VideoOverlayInst, on: bool) {
    let mut active = ACTIVE.lock().unwrap_or_else(|p| p.into_inner());
    if on {
        active.insert(inst as usize);
    } else {
        active.remove(&(inst as usize));
    }
}

/// Read `name` from the injected storage, a plain disk file, or a lazily
/// mounted game storage (in that order). `Err` means the file truly could
/// not be opened; a file that opens but is not MPEG is *not* an error (the
/// caller logs and keeps zero metadata).
fn read_video_file(name: &str) -> Result<Vec<u8>, String> {
    let injected = VIDEO_STORAGE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    if let Some(storage) = injected {
        let mut storage = storage.lock().unwrap_or_else(|p| p.into_inner());
        return storage
            .read(name)
            .map_err(|e| format!("VideoOverlay.open: cannot read {name:?} from storage: {e}"));
    }
    if let Ok(bytes) = std::fs::read(name) {
        return Ok(bytes);
    }
    let dir = crate::system::project_dir();
    let mut slot = FALLBACK_STORAGE.lock().unwrap_or_else(|p| p.into_inner());
    let needs_mount = match slot.as_ref() {
        Some((mounted, _)) => *mounted != dir,
        None => true,
    };
    if needs_mount {
        match Storage::mount(&dir) {
            Ok(storage) => *slot = Some((dir.clone(), Arc::new(Mutex::new(storage)))),
            Err(e) => {
                log::debug!(
                    "VideoOverlay: cannot mount game storage at {}: {e}",
                    dir.display()
                );
                return Err(format!("VideoOverlay.open: cannot open {name:?}"));
            }
        }
    }
    if let Some((_, storage)) = slot.as_ref() {
        let mut storage = storage.lock().unwrap_or_else(|p| p.into_inner());
        if let Ok(bytes) = storage.read(name) {
            return Ok(bytes);
        }
    }
    Err(format!("VideoOverlay.open: cannot open {name:?}"))
}

// ---------------------------------------------------------------------------
// MPEG-1/2 metadata parser
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
struct MpegMetadata {
    width: u32,
    height: u32,
    fps: f64,
    total_frames: i64,
    total_time_ms: i64,
    audio_streams: i64,
    video_streams: i64,
}

/// Find the next `00 00 01 <code>` start code at or after `from`, returning
/// the index of its first `00`.
fn find_start_code(data: &[u8], code: u8, from: usize) -> Option<usize> {
    let mut i = from;
    while i + 3 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 && data[i + 3] == code {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Count non-overlapping `00 00 01 <code>` start codes. Picture start codes
/// (`code == 0x00`) are a frame-count proxy for an elementary/program
/// stream; field-coded streams would count each field, which is documented
/// as a limitation.
fn count_start_code(data: &[u8], code: u8) -> usize {
    let mut count = 0;
    let mut i = 0;
    while i + 3 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 && data[i + 3] == code {
            count += 1;
            i += 4;
        } else {
            i += 1;
        }
    }
    count
}

/// Count distinct PES stream ids: audio `0xC0..=0xDF`, video `0xE0..=0xEF`.
fn scan_pes_streams(data: &[u8]) -> (i64, i64) {
    let mut audio = [false; 32];
    let mut video = [false; 16];
    let mut i = 0;
    while i + 3 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let id = data[i + 3];
            if (0xC0..=0xDF).contains(&id) {
                audio[(id - 0xC0) as usize] = true;
            } else if (0xE0..=0xEF).contains(&id) {
                video[(id - 0xE0) as usize] = true;
            }
            i += 4;
        } else {
            i += 1;
        }
    }
    (
        audio.iter().filter(|b| **b).count() as i64,
        video.iter().filter(|b| **b).count() as i64,
    )
}

/// MSB-first bit reader over a byte slice.
struct BitReader<'a> {
    data: &'a [u8],
    bit: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, bit: 0 }
    }

    fn read(&mut self, count: usize) -> Option<u32> {
        let mut value = 0u32;
        for _ in 0..count {
            let byte = self.bit / 8;
            if byte >= self.data.len() {
                return None;
            }
            let shift = 7 - (self.bit % 8);
            value = (value << 1) | u32::from((self.data[byte] >> shift) & 1);
            self.bit += 1;
        }
        Some(value)
    }
}

/// Parse the MPEG-1/2 sequence header (and MPEG-2 sequence extension, when
/// present). Returns `None` when no sequence header exists (e.g. AVI/WMV).
///
/// The coded size comes from the 12-bit sequence-header values, refined by
/// the 2-bit sequence-extension values for MPEG-2; the frame rate comes from
/// the table plus the extension's numerator/denominator.
fn parse_mpeg_metadata(data: &[u8]) -> Option<MpegMetadata> {
    let seq = find_start_code(data, 0xB3, 0)?;
    let mut br = BitReader::new(data.get(seq + 4..)?);
    let mut width = br.read(12)?;
    let mut height = br.read(12)?;
    let _aspect_ratio = br.read(4)?;
    let frame_rate_code = br.read(4)?;
    let mut fps = FRAME_RATES
        .get(frame_rate_code as usize)
        .copied()
        .unwrap_or(0.0);

    if let Some(ext) = find_start_code(data, 0xB5, seq + 4) {
        let mut ebr = BitReader::new(data.get(ext + 4..)?);
        let extension_id = ebr.read(4)?;
        if extension_id == 1 {
            // sequence_extension(): profile_and_level, progressive,
            // chroma_format, 2-bit size extensions, ...
            let _profile_and_level = ebr.read(8)?;
            let _progressive = ebr.read(1)?;
            let _chroma_format = ebr.read(2)?;
            let horizontal_size_extension = ebr.read(2)?;
            let vertical_size_extension = ebr.read(2)?;
            width |= horizontal_size_extension << 12;
            height |= vertical_size_extension << 12;
            let _bit_rate_extension = ebr.read(12)?;
            let _marker = ebr.read(1)?;
            let _vbv_buffer_size_extension = ebr.read(8)?;
            let _low_delay = ebr.read(1)?;
            let frame_rate_extension_n = ebr.read(2)?;
            let frame_rate_extension_d = ebr.read(5)?;
            if fps > 0.0 {
                fps *=
                    (frame_rate_extension_n as f64 + 1.0) / (frame_rate_extension_d as f64 + 1.0);
            }
        }
    }

    let total_frames = count_start_code(data, 0x00) as i64;
    let total_time_ms = if fps > 0.0 && total_frames > 0 {
        (total_frames as f64 * 1000.0 / fps).round() as i64
    } else {
        0
    };
    let (audio_streams, video_streams) = scan_pes_streams(data);

    Some(MpegMetadata {
        width,
        height,
        fps,
        total_frames,
        total_time_ms,
        audio_streams,
        video_streams,
    })
}

// ---------------------------------------------------------------------------
// Playback state
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
enum PlayStatus {
    #[default]
    Unload,
    Stop,
    Play,
    Pause,
}

impl PlayStatus {
    fn as_str(self) -> &'static str {
        match self {
            PlayStatus::Unload => "unload",
            PlayStatus::Stop => "stop",
            PlayStatus::Play => "play",
            PlayStatus::Pause => "pause",
        }
    }
}

/// Per-object state: rectangle/visibility, real media metadata, playback
/// clock anchors and retained script callbacks.
struct VideoOverlayInst {
    // rectangle / visibility
    left: i64,
    top: i64,
    width: i64,
    height: i64,
    visible: bool,

    // media metadata (real, parsed by `open`)
    original_width: i64,
    original_height: i64,
    fps: f64,
    total_frame: i64,
    total_time_ms: i64,
    number_of_audio_stream: i64,
    number_of_video_stream: i64,

    // playback
    status: PlayStatus,
    playing: bool,
    play_rate: f64,
    position_ms: i64,
    play_anchor_ms: u64,
    play_anchor_position_ms: i64,
    looping: bool,
    segment_loop_start: i64,
    segment_loop_end: i64,
    period_event_frame: i64,
    period_fired: bool,

    // retained script state
    /// The TJS object this payload backs (valid while the object is alive).
    objthis: *mut c_void,
    transition_call: Option<DetachedValue>,

    // audio / mixing passthrough
    mode: i64,
    audio_balance: i64,
    audio_volume: i64,
    enabled_audio_stream: i64,
    enabled_video_stream: i64,
    mixing_alpha: f64,
    mixing_bg: i64,
}

impl Default for VideoOverlayInst {
    fn default() -> Self {
        Self {
            left: 0,
            top: 0,
            width: 0,
            height: 0,
            visible: false,
            original_width: 0,
            original_height: 0,
            fps: 0.0,
            total_frame: 0,
            total_time_ms: 0,
            number_of_audio_stream: 0,
            number_of_video_stream: 0,
            status: PlayStatus::Unload,
            playing: false,
            play_rate: 0.0,
            position_ms: 0,
            play_anchor_ms: 0,
            play_anchor_position_ms: 0,
            looping: false,
            segment_loop_start: -1,
            segment_loop_end: -1,
            period_event_frame: -1,
            period_fired: false,
            objthis: std::ptr::null_mut(),
            transition_call: None,
            mode: 0,
            audio_balance: 0,
            audio_volume: 0,
            enabled_audio_stream: 0,
            enabled_video_stream: 0,
            mixing_alpha: 0.0,
            mixing_bg: 0,
        }
    }
}

/// Current engine time in ms (overridable in tests).
#[cfg(test)]
static TEST_NOW_MS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    #[cfg(test)]
    {
        let overridden = TEST_NOW_MS.load(Ordering::SeqCst);
        if overridden != 0 {
            return overridden;
        }
    }
    crate::system::tick_count_ms().max(0) as u64
}

fn frame_to_ms(fps: f64, frame: i64) -> i64 {
    if fps > 0.0 {
        (frame as f64 * 1000.0 / fps).round() as i64
    } else {
        0
    }
}

fn ms_to_frame(fps: f64, ms: i64) -> i64 {
    if fps > 0.0 {
        (ms.max(0) as f64 * fps / 1000.0) as i64
    } else {
        0
    }
}

fn current_position_ms(inst: &VideoOverlayInst, now: u64) -> i64 {
    if !inst.playing {
        return inst.position_ms;
    }
    let rate = if inst.play_rate > 0.0 {
        inst.play_rate
    } else {
        1.0
    };
    let elapsed = now.saturating_sub(inst.play_anchor_ms) as f64 * rate;
    (inst.play_anchor_position_ms + elapsed as i64).max(0)
}

fn current_frame(inst: &VideoOverlayInst, now: u64) -> i64 {
    ms_to_frame(inst.fps, current_position_ms(inst, now))
}

fn segment_bounds(inst: &VideoOverlayInst) -> (i64, i64) {
    let start = if inst.segment_loop_start >= 0 {
        inst.segment_loop_start
    } else {
        0
    };
    let end = if inst.segment_loop_end >= start {
        inst.segment_loop_end
    } else {
        (inst.total_frame - 1).max(start)
    };
    (start, end)
}

fn set_status(inst: &mut VideoOverlayInst, status: PlayStatus) -> Option<PlayStatus> {
    if inst.status != status {
        inst.status = status;
        Some(status)
    } else {
        None
    }
}

/// Fire a member callback on the overlay object. The object is retained only
/// for the duration of the call: a persistent self-reference would create a
/// reference cycle that corrupts the engine's retained map at shutdown.
fn fire_member(objthis: *mut c_void, member: &str, args: &[TjsValue]) {
    if objthis.is_null() {
        return;
    }
    let engine = context_engine();
    // SAFETY: `objthis` is the live TJS object of an active overlay (ACTIVE
    // membership and method-call duration guarantee it); the temporary
    // retention is released when `owner` drops.
    if let Ok(owner) = engine.retain_object_detached(objthis) {
        let _ = engine.call_member(owner.raw_id(), member, args);
    }
}

fn fire_status(objthis: *mut c_void, status: PlayStatus) {
    fire_member(
        objthis,
        "onStatusChanged",
        &[TjsValue::String(status.as_str().to_string())],
    );
}

fn fire_period(objthis: *mut c_void, reason: i64) {
    fire_member(objthis, "onPeriod", &[TjsValue::Integer(reason)]);
}

/// Result of one playback advance: which script events are due.
#[derive(Default)]
struct AdvanceOutcome {
    period_reason: Option<i64>,
    status_changed: Option<PlayStatus>,
    transition_complete: bool,
    finished: bool,
}

/// Advance the state machine to `now`. Pure state mutation, no script calls:
/// the caller fires the returned events after dropping its borrow.
fn advance_playback(inst: &mut VideoOverlayInst, now: u64) -> AdvanceOutcome {
    let mut outcome = AdvanceOutcome::default();
    if !inst.playing || inst.fps <= 0.0 || inst.total_frame <= 0 {
        return outcome;
    }
    let (start_frame, end_frame) = segment_bounds(inst);
    let end_ms = frame_to_ms(inst.fps, end_frame + 1);
    let position = current_position_ms(inst, now);
    let frame = ms_to_frame(inst.fps, position);

    if frame > end_frame || position >= end_ms {
        if inst.looping {
            let start_ms = frame_to_ms(inst.fps, start_frame);
            inst.play_anchor_ms = now;
            inst.play_anchor_position_ms = start_ms;
            inst.position_ms = start_ms;
            inst.period_fired = false;
            let segmented = inst.segment_loop_start >= 0 || inst.segment_loop_end >= 0;
            outcome.period_reason = Some(if segmented { PER_SEG_LOOP } else { PER_LOOP });
            // A configured segment loop is the "transition" this stub can
            // complete; fire the transition callback once on the first wrap.
            outcome.transition_complete = segmented;
        } else {
            inst.position_ms = end_ms;
            inst.playing = false;
            outcome.status_changed = set_status(inst, PlayStatus::Stop);
            outcome.transition_complete = true;
            outcome.finished = true;
        }
        return outcome;
    }

    if inst.period_event_frame >= 0 && !inst.period_fired && frame >= inst.period_event_frame {
        inst.period_fired = true;
        outcome.period_reason = Some(PER_PERIOD);
    }
    outcome
}

/// Advance every playing overlay once (called per frame on the engine
/// clock).
pub fn video_overlay_poll(engine: &Tjs2Engine) {
    poll_with_now(engine, now_ms());
}

fn poll_with_now(engine: &Tjs2Engine, now: u64) {
    let snapshot: Vec<usize> = ACTIVE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .copied()
        .collect();
    for addr in snapshot {
        if addr == 0
            || !ACTIVE
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .contains(&addr)
        {
            continue;
        }
        let ptr = addr as *mut VideoOverlayInst;
        // SAFETY: while `addr` is in ACTIVE it points at a live payload;
        // destroy removes it first. The borrow ends before any script
        // callback runs, so a re-entrant native call cannot alias it.
        let (outcome, objthis, transition) = {
            let inst = unsafe { &mut *ptr };
            let outcome = advance_playback(inst, now);
            let objthis = inst.objthis;
            let transition = if outcome.transition_complete {
                inst.transition_call.take()
            } else {
                None
            };
            (outcome, objthis, transition)
        };
        if outcome.finished {
            set_active(ptr, false);
        }
        if let Some(status) = outcome.status_changed {
            fire_status(objthis, status);
        }
        if let Some(reason) = outcome.period_reason {
            fire_period(objthis, reason);
        }
        if let Some(callback) = transition {
            let _ = engine.call_detached(&callback, &[]);
        }
    }
}

// ---------------------------------------------------------------------------
// Instance property accessor generators
// ---------------------------------------------------------------------------

/// Read/write integer property backed by an `i64` field.
macro_rules! int_prop {
    ($getter:ident, $setter:ident, $field:ident) => {
        extern "C" fn $getter(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is a live VideoOverlayInst payload.
            let inst = unsafe { &*(instance as *const VideoOverlayInst) };
            crate::set_int_out(out, inst.$field);
            0
        }

        extern "C" fn $setter(
            _engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is live; `value` is valid for the call.
            let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
            inst.$field = crate::value_as_i64(unsafe { &*value });
            0
        }
    };
}

/// Read/write boolean property backed by a `bool` field.
macro_rules! bool_prop {
    ($getter:ident, $setter:ident, $field:ident) => {
        extern "C" fn $getter(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is a live VideoOverlayInst payload.
            let inst = unsafe { &*(instance as *const VideoOverlayInst) };
            crate::set_int_out(out, i64::from(inst.$field));
            0
        }

        extern "C" fn $setter(
            _engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is live; `value` is valid for the call.
            let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
            inst.$field = crate::value_as_bool(unsafe { &*value });
            0
        }
    };
}

/// Read/write real property backed by an `f64` field.
macro_rules! real_prop {
    ($getter:ident, $setter:ident, $field:ident) => {
        extern "C" fn $getter(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is a live VideoOverlayInst payload.
            let inst = unsafe { &*(instance as *const VideoOverlayInst) };
            crate::set_real_out(out, inst.$field);
            0
        }

        extern "C" fn $setter(
            _engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is live; `value` is valid for the call.
            let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
            inst.$field = crate::value_as_f64(unsafe { &*value });
            0
        }
    };
}

/// Read-only integer property backed by an `i64` field.
macro_rules! ro_int_field {
    ($getter:ident, $field:ident) => {
        extern "C" fn $getter(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is a live VideoOverlayInst payload.
            let inst = unsafe { &*(instance as *const VideoOverlayInst) };
            crate::set_int_out(out, inst.$field);
            0
        }
    };
}

/// Read-only real property backed by an `f64` field.
macro_rules! ro_real_field {
    ($getter:ident, $field:ident) => {
        extern "C" fn $getter(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is a live VideoOverlayInst payload.
            let inst = unsafe { &*(instance as *const VideoOverlayInst) };
            crate::set_real_out(out, inst.$field);
            0
        }
    };
}

// -- rectangle / visibility -------------------------------------------------

int_prop!(left_get, left_set, left);
int_prop!(top_get, top_set, top);
int_prop!(width_get, width_set, width);
int_prop!(height_get, height_set, height);
bool_prop!(visible_get, visible_set, visible);

// -- playback state ---------------------------------------------------------

bool_prop!(loop_get, loop_set, looping);
int_prop!(mode_get, mode_set, mode);
real_prop!(play_rate_get, play_rate_set, play_rate);
int_prop!(
    period_event_frame_get,
    period_event_frame_set,
    period_event_frame
);
ro_int_field!(segment_loop_start_get, segment_loop_start);
ro_int_field!(segment_loop_end_get, segment_loop_end);

// -- audio / video streams --------------------------------------------------

int_prop!(audio_balance_get, audio_balance_set, audio_balance);
int_prop!(audio_volume_get, audio_volume_set, audio_volume);
int_prop!(
    enabled_audio_stream_get,
    enabled_audio_stream_set,
    enabled_audio_stream
);
int_prop!(
    enabled_video_stream_get,
    enabled_video_stream_set,
    enabled_video_stream
);
real_prop!(mixing_alpha_get, mixing_alpha_set, mixing_alpha);
int_prop!(mixing_bg_get, mixing_bg_set, mixing_bg);

// -- real media metadata ----------------------------------------------------

ro_int_field!(original_width_get, original_width);
ro_int_field!(original_height_get, original_height);
ro_int_field!(total_frame_get, total_frame);
ro_int_field!(number_of_frame_get, total_frame);
ro_int_field!(total_time_get, total_time_ms);
ro_int_field!(number_of_audio_stream_get, number_of_audio_stream);
ro_int_field!(number_of_video_stream_get, number_of_video_stream);
ro_real_field!(fps_get, fps);

// ---------------------------------------------------------------------------
// Dynamic (clock-driven) properties
// ---------------------------------------------------------------------------

extern "C" fn position_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &*(instance as *const VideoOverlayInst) };
    set_int_out(out, current_position_ms(inst, now_ms()));
    0
}

extern "C" fn position_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is live; `value` is valid for the call.
    let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
    let position = value_as_i64(unsafe { &*value }).max(0);
    inst.position_ms = position;
    inst.play_anchor_position_ms = position;
    inst.play_anchor_ms = now_ms();
    inst.period_fired = false;
    0
}

extern "C" fn frame_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &*(instance as *const VideoOverlayInst) };
    set_int_out(out, current_frame(inst, now_ms()));
    0
}

extern "C" fn frame_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is live; `value` is valid for the call.
    let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
    let frame = value_as_i64(unsafe { &*value }).max(0);
    inst.position_ms = frame_to_ms(inst.fps, frame);
    inst.play_anchor_position_ms = inst.position_ms;
    inst.play_anchor_ms = now_ms();
    inst.period_fired = false;
    0
}

// ---------------------------------------------------------------------------
// Instance methods
// ---------------------------------------------------------------------------

extern "C" fn vo_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(VideoOverlayInst::default())) as *mut c_void
}

extern "C" fn vo_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    set_active(instance as *mut VideoOverlayInst, false);
    // SAFETY: instance came from vo_create's Box::into_raw.
    drop(unsafe { Box::from_raw(instance as *mut VideoOverlayInst) });
}

/// `new VideoOverlay(win)` / `super.VideoOverlay(...)`: accept any argument
/// list (the stub has no window state) and remember `objthis` so the
/// `onStatusChanged`/`onPeriod` callbacks can be invoked later.
extern "C" fn vo_ctor(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live payload and `objthis` a live object for
    // the duration of the call.
    let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
    inst.objthis = objthis;
    set_void_out(out);
    0
}

/// `open(file)`: read the file through the game storage and parse its MPEG
/// metadata. A missing file raises a TJS error; an opened non-MPEG file is
/// accepted with zero metadata (logged).
extern "C" fn vo_open(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let values = args(argv, argc);
    let Some(file_value) = values.first() else {
        return report_error(out_error, "VideoOverlay.open requires 1 argument");
    };
    let file = value_as_string(file_value);
    let bytes = match read_video_file(&file) {
        Ok(bytes) => bytes,
        Err(e) => return report_error(out_error, &e),
    };
    let metadata = parse_mpeg_metadata(&bytes);
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
    apply_open(inst, metadata, &file);
    set_void_out(out);
    0
}

fn apply_open(inst: &mut VideoOverlayInst, metadata: Option<MpegMetadata>, file: &str) {
    inst.playing = false;
    inst.period_fired = false;
    inst.position_ms = 0;
    inst.play_anchor_ms = 0;
    inst.play_anchor_position_ms = 0;
    inst.original_width = 0;
    inst.original_height = 0;
    inst.fps = 0.0;
    inst.total_frame = 0;
    inst.total_time_ms = 0;
    inst.number_of_audio_stream = 0;
    inst.number_of_video_stream = 0;
    inst.status = PlayStatus::Stop;
    set_active(inst as *mut VideoOverlayInst, false);

    if let Some(metadata) = metadata {
        inst.original_width = i64::from(metadata.width);
        inst.original_height = i64::from(metadata.height);
        inst.fps = metadata.fps;
        inst.total_frame = metadata.total_frames;
        inst.total_time_ms = metadata.total_time_ms;
        inst.number_of_audio_stream = metadata.audio_streams;
        inst.number_of_video_stream = metadata.video_streams.max(1);
        if inst.width == 0 {
            inst.width = i64::from(metadata.width);
        }
        if inst.height == 0 {
            inst.height = i64::from(metadata.height);
        }
        log::info!(
            "VideoOverlay.open({file:?}): {}x{} @ {:.3} fps, {} frame(s)",
            metadata.width,
            metadata.height,
            metadata.fps,
            metadata.total_frames
        );
    } else {
        log::warn!("VideoOverlay.open({file:?}): no MPEG sequence header; metadata unavailable");
    }
}

/// `close()`: drop the media and return to the unloaded state.
extern "C" fn vo_close(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
    apply_close(inst);
    set_void_out(out);
    0
}

fn apply_close(inst: &mut VideoOverlayInst) {
    inst.playing = false;
    inst.period_fired = false;
    inst.position_ms = 0;
    inst.play_anchor_position_ms = 0;
    inst.original_width = 0;
    inst.original_height = 0;
    inst.fps = 0.0;
    inst.total_frame = 0;
    inst.total_time_ms = 0;
    inst.number_of_audio_stream = 0;
    inst.number_of_video_stream = 0;
    inst.status = PlayStatus::Unload;
    set_active(inst as *mut VideoOverlayInst, false);
}

/// `play()`: start (or resume) the clock and fire `onStatusChanged("play")`.
extern "C" fn vo_play(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let now = now_ms();
    let (status, objthis) = {
        // SAFETY: `instance` is a live VideoOverlayInst payload.
        let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
        let end_ms = if inst.total_frame > 0 {
            frame_to_ms(inst.fps, inst.total_frame)
        } else {
            0
        };
        let at_end = inst.total_frame > 0 && current_position_ms(inst, now) >= end_ms;
        if !inst.looping && at_end {
            inst.position_ms = 0;
            inst.play_anchor_position_ms = 0;
        } else {
            inst.play_anchor_position_ms = current_position_ms(inst, now);
        }
        inst.play_anchor_ms = now;
        inst.playing = true;
        inst.period_fired = false;
        (set_status(inst, PlayStatus::Play), inst.objthis)
    };
    set_active(instance as *mut VideoOverlayInst, true);
    if let Some(status) = status {
        fire_status(objthis, status);
    }
    set_void_out(out);
    0
}

/// `pause()`: freeze `position` and fire `onStatusChanged("pause")`.
extern "C" fn vo_pause(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let now = now_ms();
    let (status, objthis) = {
        // SAFETY: `instance` is a live VideoOverlayInst payload.
        let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
        if inst.playing {
            inst.position_ms = current_position_ms(inst, now);
            inst.playing = false;
        }
        (set_status(inst, PlayStatus::Pause), inst.objthis)
    };
    set_active(instance as *mut VideoOverlayInst, false);
    if let Some(status) = status {
        fire_status(objthis, status);
    }
    set_void_out(out);
    0
}

/// `stop()`: rewind to the start and fire `onStatusChanged("stop")`.
extern "C" fn vo_stop(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let (status, objthis) = {
        // SAFETY: `instance` is a live VideoOverlayInst payload.
        let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
        inst.playing = false;
        inst.position_ms = 0;
        inst.play_anchor_position_ms = 0;
        inst.period_fired = false;
        (set_status(inst, PlayStatus::Stop), inst.objthis)
    };
    set_active(instance as *mut VideoOverlayInst, false);
    if let Some(status) = status {
        fire_status(objthis, status);
    }
    set_void_out(out);
    0
}

/// `rewind()`: reset to frame 0, keeping the playing/paused state.
extern "C" fn vo_rewind(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let now = now_ms();
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
    inst.position_ms = 0;
    inst.play_anchor_position_ms = 0;
    inst.play_anchor_ms = now;
    inst.period_fired = false;
    set_void_out(out);
    0
}

/// `prepare()`: fire the `perPrepare` `onPeriod` event.
extern "C" fn vo_prepare(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let objthis = {
        let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
        inst.objthis
    };
    fire_period(objthis, PER_PREPARE);
    set_void_out(out);
    0
}

/// Shared body for every argument-less no-op method (`setMixingLayer`,
/// `resetMixingLayer`).
extern "C" fn vo_noop_method(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void_out(out);
    0
}

/// `setPos(left, top)`
extern "C" fn vo_set_pos(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
    let values = args(argv, argc);
    inst.left = values.first().map(value_as_i64).unwrap_or(0);
    inst.top = values.get(1).map(value_as_i64).unwrap_or(0);
    set_void_out(out);
    0
}

/// `setSize(width, height)`
extern "C" fn vo_set_size(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
    let values = args(argv, argc);
    inst.width = values.first().map(value_as_i64).unwrap_or(0);
    inst.height = values.get(1).map(value_as_i64).unwrap_or(0);
    set_void_out(out);
    0
}

/// `setBounds(left, top, width, height)`
extern "C" fn vo_set_bounds(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
    let values = args(argv, argc);
    inst.left = values.first().map(value_as_i64).unwrap_or(0);
    inst.top = values.get(1).map(value_as_i64).unwrap_or(0);
    inst.width = values.get(2).map(value_as_i64).unwrap_or(0);
    inst.height = values.get(3).map(value_as_i64).unwrap_or(0);
    set_void_out(out);
    0
}

/// `setSegmentLoop(startFrame, endFrame)`
extern "C" fn vo_set_segment_loop(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
    let values = args(argv, argc);
    inst.segment_loop_start = values.first().map(value_as_i64).unwrap_or(-1);
    inst.segment_loop_end = values.get(1).map(value_as_i64).unwrap_or(-1);
    inst.period_fired = false;
    set_void_out(out);
    0
}

/// `cancelSegmentLoop()`
extern "C" fn vo_cancel_segment_loop(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
    inst.segment_loop_start = -1;
    inst.segment_loop_end = -1;
    set_void_out(out);
    0
}

/// `setPeriodEvent(frame)` (no argument clears it, like the reference).
extern "C" fn vo_set_period_event(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
    let values = args(argv, argc);
    inst.period_event_frame = values.first().map(value_as_i64).unwrap_or(-1);
    inst.period_fired = false;
    set_void_out(out);
    0
}

/// `cancelPeriodEvent()`
extern "C" fn vo_cancel_period_event(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
    inst.period_event_frame = -1;
    inst.period_fired = false;
    set_void_out(out);
    0
}

/// `selectAudioStream(n)`
extern "C" fn vo_select_audio_stream(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
    inst.enabled_audio_stream = args(argv, argc).first().map(value_as_i64).unwrap_or(0);
    set_void_out(out);
    0
}

/// `setTransitionCompleteCall(cb)`: retain `cb` (a no-arg function) and fire
/// it when a non-looping stream — or a segment loop wrap — completes.
/// Calling it with no/void argument clears the callback.
extern "C" fn vo_set_transition_complete_call(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let engine = context_engine();
    let values = args(argv, argc);
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
    let callback = values
        .first()
        .filter(|value| value.ty == tjs2_sys::VAL_OBJECT);
    inst.transition_call = match callback {
        Some(value) => match engine.retain_object_arg(value) {
            Ok(retained) => Some(retained),
            Err(e) => {
                return report_error(
                    out_error,
                    &format!("VideoOverlay.setTransitionCompleteCall: {e}"),
                );
            }
        },
        None => None,
    };
    set_void_out(out);
    0
}

/// `layer1`/`layer2` getter: no Layer is attached, so read as void
/// (reference returns the layer or null).
extern "C" fn vo_layer_get(
    _engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void_out(out);
    0
}

/// `layer1`/`layer2` setter: accept any value and ignore it.
extern "C" fn vo_layer_set(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    0
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the `VideoOverlay` native class on the engine's global object.
pub fn register_video_overlay(engine: &Tjs2Engine) -> Result<(), String> {
    engine.register_native_class_instance(&NativeInstanceBuilder {
        name: "VideoOverlay",
        create: vo_create,
        destroy: vo_destroy,
        methods: vec![
            NativeInstanceMethodDef {
                name: "VideoOverlay", // class-name ctor hook
                f: vo_ctor,
            },
            NativeInstanceMethodDef {
                name: "open",
                f: vo_open,
            },
            NativeInstanceMethodDef {
                name: "close",
                f: vo_close,
            },
            NativeInstanceMethodDef {
                name: "play",
                f: vo_play,
            },
            NativeInstanceMethodDef {
                name: "stop",
                f: vo_stop,
            },
            NativeInstanceMethodDef {
                name: "pause",
                f: vo_pause,
            },
            NativeInstanceMethodDef {
                name: "rewind",
                f: vo_rewind,
            },
            NativeInstanceMethodDef {
                name: "prepare",
                f: vo_prepare,
            },
            NativeInstanceMethodDef {
                name: "setTransitionCompleteCall",
                f: vo_set_transition_complete_call,
            },
            NativeInstanceMethodDef {
                name: "setMixingLayer",
                f: vo_noop_method,
            },
            NativeInstanceMethodDef {
                name: "resetMixingLayer",
                f: vo_noop_method,
            },
            NativeInstanceMethodDef {
                name: "setPos",
                f: vo_set_pos,
            },
            NativeInstanceMethodDef {
                name: "setSize",
                f: vo_set_size,
            },
            NativeInstanceMethodDef {
                name: "setBounds",
                f: vo_set_bounds,
            },
            NativeInstanceMethodDef {
                name: "setSegmentLoop",
                f: vo_set_segment_loop,
            },
            NativeInstanceMethodDef {
                name: "cancelSegmentLoop",
                f: vo_cancel_segment_loop,
            },
            NativeInstanceMethodDef {
                name: "setPeriodEvent",
                f: vo_set_period_event,
            },
            NativeInstanceMethodDef {
                name: "cancelPeriodEvent",
                f: vo_cancel_period_event,
            },
            NativeInstanceMethodDef {
                name: "selectAudioStream",
                f: vo_select_audio_stream,
            },
        ],
        properties: vec![
            NativeInstancePropertyDef {
                name: "left",
                get: Some(left_get),
                set: Some(left_set),
            },
            NativeInstancePropertyDef {
                name: "top",
                get: Some(top_get),
                set: Some(top_set),
            },
            NativeInstancePropertyDef {
                name: "width",
                get: Some(width_get),
                set: Some(width_set),
            },
            NativeInstancePropertyDef {
                name: "height",
                get: Some(height_get),
                set: Some(height_set),
            },
            NativeInstancePropertyDef {
                name: "visible",
                get: Some(visible_get),
                set: Some(visible_set),
            },
            NativeInstancePropertyDef {
                name: "position",
                get: Some(position_get),
                set: Some(position_set),
            },
            NativeInstancePropertyDef {
                name: "frame",
                get: Some(frame_get),
                set: Some(frame_set),
            },
            NativeInstancePropertyDef {
                name: "loop",
                get: Some(loop_get),
                set: Some(loop_set),
            },
            NativeInstancePropertyDef {
                name: "mode",
                get: Some(mode_get),
                set: Some(mode_set),
            },
            NativeInstancePropertyDef {
                name: "playRate",
                get: Some(play_rate_get),
                set: Some(play_rate_set),
            },
            NativeInstancePropertyDef {
                name: "periodEventFrame",
                get: Some(period_event_frame_get),
                set: Some(period_event_frame_set),
            },
            NativeInstancePropertyDef {
                name: "segmentLoopStartFrame",
                get: Some(segment_loop_start_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "segmentLoopEndFrame",
                get: Some(segment_loop_end_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "audioBalance",
                get: Some(audio_balance_get),
                set: Some(audio_balance_set),
            },
            NativeInstancePropertyDef {
                name: "audioVolume",
                get: Some(audio_volume_get),
                set: Some(audio_volume_set),
            },
            NativeInstancePropertyDef {
                name: "enabledAudioStream",
                get: Some(enabled_audio_stream_get),
                set: Some(enabled_audio_stream_set),
            },
            NativeInstancePropertyDef {
                name: "enabledVideoStream",
                get: Some(enabled_video_stream_get),
                set: Some(enabled_video_stream_set),
            },
            NativeInstancePropertyDef {
                name: "mixingMovieAlpha",
                get: Some(mixing_alpha_get),
                set: Some(mixing_alpha_set),
            },
            NativeInstancePropertyDef {
                name: "mixingMovieBGColor",
                get: Some(mixing_bg_get),
                set: Some(mixing_bg_set),
            },
            NativeInstancePropertyDef {
                name: "originalWidth",
                get: Some(original_width_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "originalHeight",
                get: Some(original_height_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "totalFrame",
                get: Some(total_frame_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "fps",
                get: Some(fps_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "numberOfFrame",
                get: Some(number_of_frame_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "totalTime",
                get: Some(total_time_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "numberOfAudioStream",
                get: Some(number_of_audio_stream_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "numberOfVideoStream",
                get: Some(number_of_video_stream_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "layer1",
                get: Some(vo_layer_get),
                set: Some(vo_layer_set),
            },
            NativeInstancePropertyDef {
                name: "layer2",
                get: Some(vo_layer_get),
                set: Some(vo_layer_set),
            },
        ],
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use crate::test_lock::vm_lock;

    use super::*;

    // -- test fixtures -----------------------------------------------------

    /// MSB-first bit writer used to build a synthetic MPEG sequence header.
    struct BitWriter {
        bytes: Vec<u8>,
        current: u8,
        bits: u8,
    }

    impl BitWriter {
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                current: 0,
                bits: 0,
            }
        }

        fn write(&mut self, value: u32, count: u32) {
            for i in (0..count).rev() {
                let bit = ((value >> i) & 1) as u8;
                self.current = (self.current << 1) | bit;
                self.bits += 1;
                if self.bits == 8 {
                    self.bytes.push(self.current);
                    self.current = 0;
                    self.bits = 0;
                }
            }
        }

        fn finish(mut self) -> Vec<u8> {
            if self.bits > 0 {
                self.current <<= 8 - self.bits;
                self.bytes.push(self.current);
            }
            self.bytes
        }
    }

    /// 720x480 @ 29.97 fps MPEG-2 sequence header + extension + one video
    /// and one audio PES stream id.
    fn synthetic_mpeg_sequence() -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&[0, 0, 1, 0xB3]);
        let mut header = BitWriter::new();
        header.write(720, 12);
        header.write(480, 12);
        header.write(1, 4); // aspect_ratio_information
        header.write(4, 4); // frame_rate_code: 29.97
        header.write(0, 18); // bit_rate_value
        header.write(1, 1); // marker_bit
        header.write(0, 10); // vbv_buffer_size_value
        header.write(0, 1); // constrained_parameters_flag
        header.write(0, 1); // load_intra_quantiser_matrix
        header.write(0, 1); // load_non_intra_quantiser_matrix
        buf.extend(header.finish());

        buf.extend_from_slice(&[0, 0, 1, 0xB5]);
        let mut extension = BitWriter::new();
        extension.write(1, 4); // sequence_extension id
        extension.write(0x44, 8); // profile_and_level_indication
        extension.write(0, 1); // progressive_sequence
        extension.write(1, 2); // chroma_format: 4:2:0
        extension.write(0, 2); // horizontal_size_extension
        extension.write(0, 2); // vertical_size_extension
        extension.write(0, 12); // bit_rate_extension
        extension.write(1, 1); // marker_bit
        extension.write(0, 8); // vbv_buffer_size_extension
        extension.write(0, 1); // low_delay
        extension.write(0, 2); // frame_rate_extension_n
        extension.write(0, 5); // frame_rate_extension_d
        buf.extend(extension.finish());

        buf.extend_from_slice(&[0, 0, 1, 0xE0]); // video PES
        buf.extend_from_slice(&[0, 0, 1, 0xC0]); // audio PES
        buf
    }

    fn synthetic_mpeg(pictures: usize) -> Vec<u8> {
        let mut buf = synthetic_mpeg_sequence();
        for _ in 0..pictures {
            buf.extend_from_slice(&[0, 0, 1, 0x00, 0x00, 0x08]);
        }
        buf
    }

    static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_temp_dir(name: &str) -> PathBuf {
        let n = DIR_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("tvp_vo_{name}_{}_{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn write_synthetic_mpeg(dir: &std::path::Path, name: &str, pictures: usize) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, synthetic_mpeg(pictures)).expect("write synthetic mpeg");
        path
    }

    fn install_storage(dir: &std::path::Path) {
        let storage = Storage::mount(dir).expect("mount temp storage");
        set_video_storage(Some(Arc::new(Mutex::new(storage))));
    }

    fn set_now(now: u64) {
        TEST_NOW_MS.store(now, Ordering::SeqCst);
    }

    fn reset_now() {
        TEST_NOW_MS.store(0, Ordering::SeqCst);
    }

    // -- parser ------------------------------------------------------------

    #[test]
    fn parses_mpeg2_sequence_header() {
        let metadata = parse_mpeg_metadata(&synthetic_mpeg(30)).expect("sequence header");
        assert_eq!(metadata.width, 720);
        assert_eq!(metadata.height, 480);
        assert!((metadata.fps - 30000.0 / 1001.0).abs() < 1e-9);
        assert_eq!(metadata.total_frames, 30);
        assert_eq!(metadata.audio_streams, 1);
        assert_eq!(metadata.video_streams, 1);
        assert!(metadata.total_time_ms > 0);
    }

    #[test]
    fn rejects_non_mpeg_data() {
        assert!(parse_mpeg_metadata(b"not a video").is_none());
    }

    // -- open / real metadata ----------------------------------------------

    #[test]
    fn open_reads_real_metadata_from_storage() {
        let _vm_lock = vm_lock();
        let dir = unique_temp_dir("open");
        write_synthetic_mpeg(&dir, "movie.mpg", 30);
        install_storage(&dir);

        let engine = Tjs2Engine::new().expect("create engine");
        crate::register_all(&engine).expect("register all natives");
        engine
            .exec_script(
                r#"
                var v = new VideoOverlay(null);
                v.open("movie.mpg");
                var ow = v.originalWidth;
                var oh = v.originalHeight;
                var ofps = v.fps;
                var otf = v.totalFrame;
                var onf = v.numberOfFrame;
                var oa = v.numberOfAudioStream;
                "#,
                "video_overlay_open",
            )
            .expect("open must not throw");
        assert_eq!(engine.eval("ow", "test").unwrap(), TjsValue::Integer(720));
        assert_eq!(engine.eval("oh", "test").unwrap(), TjsValue::Integer(480));
        assert_eq!(engine.eval("otf", "test").unwrap(), TjsValue::Integer(30));
        assert_eq!(engine.eval("onf", "test").unwrap(), TjsValue::Integer(30));
        assert_eq!(engine.eval("oa", "test").unwrap(), TjsValue::Integer(1));
        match engine.eval("ofps", "test").unwrap() {
            TjsValue::Real(fps) => assert!((fps - 30000.0 / 1001.0).abs() < 1e-9),
            other => panic!("expected Real fps, got {other:?}"),
        }

        set_video_storage(None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_missing_file_raises() {
        let _vm_lock = vm_lock();
        let dir = unique_temp_dir("missing");
        install_storage(&dir);
        let engine = Tjs2Engine::new().expect("create engine");
        crate::register_all(&engine).expect("register all natives");
        engine
            .exec_script(
                "var v = new VideoOverlay(null); var caught = 0; \
                 try { v.open('no-such-file.mpg'); } catch (e) { caught = 1; }",
                "video_overlay_open",
            )
            .expect("script runs");
        assert_eq!(engine.eval("caught", "test").unwrap(), TjsValue::Integer(1));
        set_video_storage(None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- timing state machine (pure) ---------------------------------------

    #[test]
    fn playback_state_machine_advances_and_loops() {
        let _vm_lock = vm_lock();
        let mut inst = VideoOverlayInst {
            fps: 30.0,
            total_frame: 30,
            total_time_ms: 1000,
            looping: true,
            playing: true,
            ..VideoOverlayInst::default()
        };
        assert_eq!(current_frame(&inst, 500), 15);
        assert!(advance_playback(&mut inst, 500).period_reason.is_none());

        // Crossing the end wraps to frame 0 and fires perLoop.
        let outcome = advance_playback(&mut inst, 1100);
        assert_eq!(outcome.period_reason, Some(PER_LOOP));
        assert_eq!(inst.position_ms, 0);
        assert_eq!(inst.play_anchor_position_ms, 0);
        assert!(inst.playing);
    }

    #[test]
    fn non_looping_playback_finishes_and_fires_transition() {
        let _vm_lock = vm_lock();
        let mut inst = VideoOverlayInst {
            fps: 30.0,
            total_frame: 30,
            total_time_ms: 1000,
            looping: false,
            playing: true,
            ..VideoOverlayInst::default()
        };
        let outcome = advance_playback(&mut inst, 2000);
        assert!(outcome.finished);
        assert!(outcome.transition_complete);
        assert_eq!(outcome.status_changed, Some(PlayStatus::Stop));
        assert!(!inst.playing);
        assert_eq!(inst.position_ms, 1000);
    }

    #[test]
    fn segment_loop_wraps_inside_the_segment() {
        let _vm_lock = vm_lock();
        let mut inst = VideoOverlayInst {
            fps: 30.0,
            total_frame: 300,
            total_time_ms: 10_000,
            looping: true,
            playing: true,
            segment_loop_start: 10,
            segment_loop_end: 19,
            ..VideoOverlayInst::default()
        };
        let outcome = advance_playback(&mut inst, 700); // frame 21 > 19
        assert_eq!(outcome.period_reason, Some(PER_SEG_LOOP));
        assert!(outcome.transition_complete);
        assert_eq!(inst.play_anchor_position_ms, frame_to_ms(30.0, 10));
    }

    // -- callbacks ---------------------------------------------------------

    #[test]
    fn period_event_fires_on_period_callback() {
        let _vm_lock = vm_lock();
        let dir = unique_temp_dir("period");
        write_synthetic_mpeg(&dir, "movie.mpg", 300);
        install_storage(&dir);

        let engine = Tjs2Engine::new().expect("create engine");
        crate::register_all(&engine).expect("register all natives");
        set_now(1000);
        engine
            .exec_script(
                r#"
                var fired = -1;
                var v = new VideoOverlay(null);
                v.open("movie.mpg");
                v.setPeriodEvent(3);
                v.onPeriod = function(reason) { fired = reason; v.cancelPeriodEvent(); };
                v.play();
                "#,
                "video_overlay_period",
            )
            .expect("script runs");

        // 120 ms at 29.97 fps crosses frame 3.
        poll_with_now(&engine, 1200);
        assert_eq!(
            engine.eval("fired", "test").unwrap(),
            TjsValue::Integer(PER_PERIOD)
        );

        // The callback cancelled the event: nothing fires again.
        engine.exec_script("fired = -1;", "test").unwrap();
        poll_with_now(&engine, 1300);
        assert_eq!(engine.eval("fired", "test").unwrap(), TjsValue::Integer(-1));

        reset_now();
        set_video_storage(None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transition_complete_callback_fires_on_finish() {
        let _vm_lock = vm_lock();
        let dir = unique_temp_dir("transition");
        write_synthetic_mpeg(&dir, "movie.mpg", 30);
        install_storage(&dir);

        let engine = Tjs2Engine::new().expect("create engine");
        crate::register_all(&engine).expect("register all natives");
        set_now(1000);
        engine
            .exec_script(
                r#"
                var done = 0;
                var v = new VideoOverlay(null);
                v.open("movie.mpg");
                v.setTransitionCompleteCall(function() { done++; });
                v.play();
                "#,
                "video_overlay_transition",
            )
            .expect("script runs");

        poll_with_now(&engine, 5000); // past 30 frames @ ~30 fps
        assert_eq!(engine.eval("done", "test").unwrap(), TjsValue::Integer(1));

        reset_now();
        set_video_storage(None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- non-throwing surface ----------------------------------------------

    /// The members `EnvEffectFilter` (system_enveffect.tjs) calls on its
    /// `_player`, plus the constructor. None may throw.
    #[test]
    fn video_overlay_stub_members_do_not_throw() {
        let _vm_lock = vm_lock();
        let dir = unique_temp_dir("members");
        write_synthetic_mpeg(&dir, "watch_long.mpg", 30);
        install_storage(&dir);

        let engine = Tjs2Engine::new().expect("create engine");
        crate::register_all(&engine).expect("register all natives");
        engine
            .exec_script(
                r#"
                var v = new VideoOverlay(null);
                v.open("watch_long.mpg");
                var w = v.originalWidth;
                var h = v.originalHeight;
                var total = v.totalFrame;
                v.play();
                v.pause();
                v.stop();
                v.setTransitionCompleteCall(null);
                v.setPos(1, 2);
                v.setSize(3, 4);
                v.setBounds(5, 6, 7, 8);
                v.setSegmentLoop(0, 10);
                v.cancelSegmentLoop();
                v.setPeriodEvent(3);
                v.cancelPeriodEvent();
                v.mode = vomLayer;
                v.layer1 = null;
                v.layer2 = null;
                v.loop = true;
                v.frame = 12;
                v.visible = true;
                v.position = 42;
                v.audioVolume = 100;
                v.enabledAudioStream = 0;
                "#,
                "video_overlay_test",
            )
            .expect("stub members must not throw");
        // Real parsed metadata, not hard-coded zeros.
        assert_eq!(engine.eval("w", "test").unwrap(), TjsValue::Integer(720));
        assert_eq!(engine.eval("h", "test").unwrap(), TjsValue::Integer(480));
        assert_eq!(engine.eval("total", "test").unwrap(), TjsValue::Integer(30));
        assert_eq!(
            engine.eval("v.width", "test").unwrap(),
            TjsValue::Integer(7)
        );
        assert_eq!(
            engine.eval("v.height", "test").unwrap(),
            TjsValue::Integer(8)
        );
        assert_eq!(
            engine.eval("v.segmentLoopStartFrame", "test").unwrap(),
            TjsValue::Integer(-1)
        );
        assert_eq!(
            engine.eval("v.mode === vomLayer", "test").unwrap(),
            TjsValue::Integer(1)
        );

        set_video_storage(None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `MovieLayer` (system_movie.tjs) is a script class that extends the
    /// native base; its members must resolve through to the stub.
    #[test]
    fn script_subclass_inherits_video_overlay_members() {
        let _vm_lock = vm_lock();
        let dir = unique_temp_dir("subclass");
        write_synthetic_mpeg(&dir, "movie.mpg", 30);
        install_storage(&dir);

        let engine = Tjs2Engine::new().expect("create engine");
        crate::register_all(&engine).expect("register all natives");
        engine
            .exec_script(
                r#"
                class TestMovie extends VideoOverlay {
                    function TestMovie(win) {
                        super.VideoOverlay(...);
                    }
                    function go() {
                        open("movie.mpg");
                        play();
                        pause = true;
                        play();
                        return originalWidth;
                    }
                }
                var movie = new TestMovie(null);
                var r = movie.go();
                "#,
                "video_overlay_test",
            )
            .expect("script subclass must inherit the stub");
        assert_eq!(engine.eval("r", "test").unwrap(), TjsValue::Integer(720));

        set_video_storage(None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
