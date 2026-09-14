//! `VideoOverlay` native class — the reference `tTJSNC_VideoOverlay`
//! (`reference/cpp/core/visual/VideoOvlIntf.cpp` + `VideoOvlImpl.{h,cpp}`)
//! with **real FFmpeg-backed movie decoding** and a real clock-driven
//! playback state machine.
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
//!   `System.dataPath`/`project_dir`) and opens it with FFmpeg (see
//!   [`video`]): the game's `.mpg` files are really **H.264/AAC MP4**
//!   containers. `originalWidth`, `originalHeight`, `fps`, `totalFrame`,
//!   `numberOfFrame`, `totalTime` and `numberOfAudioStream`/
//!   `numberOfVideoStream` come from the container. If FFmpeg cannot open
//!   the bytes the legacy MPEG-1/2 sequence-header parser is used as a
//!   fallback (the synthetic streams the unit tests build).
//! * Video is decoded with `libavcodec` and converted to RGBA with
//!   `libswscale`; [`present_current`] updates the overlay's layer bitmap on
//!   the movie clock (`play`/`pause`/`stop`/`rewind`, `position`/`frame`
//!   setters and [`video_overlay_poll`]). The latest frame is exposed to
//!   scripts through `frameWidth`/`frameHeight`/`frameBytes`/
//!   `frameChecksum`.
//! * AAC audio is decoded (via `libswresample`) to interleaved `f32` PCM and
//!   exposed through `audioSampleCount`/`audioSampleRate`/`audioChannels`
//!   and [`video::MovieDecoder::audio_pcm`].
//! * A clock-driven state machine: `play`/`pause`/`stop`/`rewind` move
//!   `position` (ms) and `frame` off the engine tick clock; `loop` and
//!   `setSegmentLoop` wrap the frame range; `setPeriodEvent` fires the
//!   script `onPeriod` callback (with `perPeriod`/`perLoop`/`perSegLoop`)
//!   when the frame is reached; `onStatusChanged` fires on status changes;
//!   `setTransitionCompleteCall` fires when a non-looping stream (or a
//!   segment loop wrap) completes.
//!
//! # Mixing layer (`vomMixer`)
//!
//! `setMixingLayer(layer)` / `resetMixingLayer()` are real. The argument is
//! validated as a `Layer` through its `nativeId` (the reference throws
//! `TVPSpecifyLayer` otherwise) and its `visible`/`opacity` are resolved
//! exactly like `tTJSNI_VideoOverlay::SetMixingLayer` (`VideoOvlImpl.cpp:882`):
//! a null or non-visible layer clears the mixing bitmap, otherwise the object
//! is retained and its opacity remembered. While a mixing layer is set, every
//! presented frame is composited over that layer's MainImage (read through
//! the reference `mainImageBuffer`/`mainImageBufferPitch` plugin ABI,
//! `LayerIntf.cpp:3005`) and the `mixingMovieBGColor` base at
//! `mixingMovieAlpha` — the `vomMixer` output. The result is held in this
//! native's frame buffer and is observable through `frameBytes` /
//! `frameChecksum`; uploading it to the script `layer1` scene is still
//! render-side (see below).
//!
//! # Remaining gap (layer attachment / audio device)
//!
//! The decoded frame is held in this native's own layer bitmap and surfaced
//! through the properties above; it is **not** yet uploaded into the script
//! `layer1` tvp-visual bitmap, because that scene lives in another crate
//! (a future render-side frame sink can consume the same buffer). The
//! decoded PCM is ready but is not yet fed into the engine mixer channel
//! (`tvp-sound`); the property surface exposes it instead.
//!
//! # Timer driving
//!
//! There is no decoder thread, so the state machine is advanced once per
//! frame by [`video_overlay_poll`], which [`crate::continuous_handler_poll`]
//! calls on the same clock as the other natives. Instances are registered in
//! a process-global active set while playing.
//!
//! The reference's native event entry points
//! [`onStatusChanged`](VideoOverlay)/`onCallbackCommand`/`onPeriod`/
//! `onFrameUpdate` (`VideoOvlIntf.cpp:421-480`) are registered as fallbacks:
//! when a script subclass does not override the event name, the native
//! method builds the `TVP_ACTION_INVOKE` event dictionary and calls
//! `actionOwner.action(ev)` (mirroring `Layer`/`Window`). The clock poll
//! delivers `onStatusChanged`/`onPeriod`/`onFrameUpdate` through the member
//! lookup, so a script override wins exactly like the reference's
//! `TVPPostEvent(Owner, Owner, ...)`.

use std::collections::HashSet;
use std::ffi::{c_char, c_int, c_void};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};

use engine::Storage;
use tjs2_sys::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef,
    RetainedValue, Tjs2Engine, TjsValue, Value,
};

use crate::{
    args, context_engine, report_error, set_int_out, set_void_out, value_as_i64, value_as_string,
};

// Real FFmpeg-backed decoding lives in `src/video/`; declared here (rather
// than in `lib.rs`) so the new module stays inside this class's scope. Some
// of its API (whole-track PCM, blank-frame check) is exercised by tests and
// reserved for the render-side frame sink, hence `dead_code`.
#[allow(dead_code)]
#[path = "video/mod.rs"]
mod video;

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
                log::warn!(
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
    log::warn!("VideoOverlay.open: cannot read {name:?} from any mounted storage");
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
/// A retained reference to the overlay's action owner (the window object
/// passed as the first constructor argument). The native event methods
/// dispatch to `action_owner.action(event)`.
struct ActionOwner {
    /// Raw TJS object handle (re-retained per event dispatch).
    raw: *mut c_void,
    /// Keeps the object alive (AddRef); released when the overlay is
    /// destroyed.
    _keepalive: DetachedValue,
}

/// One `setMixingLayer` target: the retained Layer object and the opacity it
/// had when the call was made. The reference
/// `tTJSNI_VideoOverlay::SetMixingLayer` (`VideoOvlImpl.cpp:882`) resolves
/// the layer's `GetMainImage()` and `GetOpacity()/255` once and hands them to
/// the platform video player with `SetMixingBitmap`; this mirrors that.
struct MixingLayer {
    /// Keeps the Layer TJS object alive while the overlay references it.
    keepalive: DetachedValue,
    /// Engine-internal layer id (the Layer `nativeId` property); the render
    /// side can resolve this to the scene layer if it wants to draw it.
    id: u32,
    /// `opacity / 255` at call time (`0.0..=1.0`).
    alpha: f64,
}

/// One `layer1`/`layer2` target. The reference `tTJSNI_VideoOverlay` stores
/// the raw `tTJSNI_BaseLayer *` (`VideoOvlImpl.h:48`) that the `layer1`/
/// `layer2` setters assign and the getters hand back; krkr-rs keeps the
/// Layer TJS object retained (so scripts can read `v.layer1` back and keep
/// using it) plus the raw object handle for the getter.
struct LayerRef {
    /// Keeps the Layer TJS object alive while the overlay references it.
    #[allow(dead_code)]
    keepalive: DetachedValue,
    /// Raw TJS object pointer, valid for re-retention by the getter.
    objthis: *mut c_void,
    /// Engine-internal layer id (the Layer `nativeId` property); kept for the
    /// render-side mixing path.
    #[allow(dead_code)]
    id: u32,
}

/// A copy of a mixing layer's MainImage RGBA pixels. The reference passes the
/// layer's live image pointer to the video player; krkr-rs copies it once per
/// presented frame because tvp-natives cannot hold the tvp-visual scene lock
/// across the crate boundary.
struct MixingBackground {
    width: usize,
    height: usize,
    /// Bytes per source row (the layer bitmap's pitch, `width * 4` for a
    /// tightly packed RGBA8 image or more when an image window is narrower
    /// than the bitmap).
    pitch: usize,
    rgba: Vec<u8>,
}

/// Largest layer dimension accepted when reading a mixing bitmap; guards the
/// raw-pointer read against a bogus `imageWidth`/`imageHeight`.
const MAX_MIXING_DIM: i64 = 16384;

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
    /// Reference `tTJSNI_BaseVideoOverlay::ActionOwner`: the object passed as
    /// the first constructor argument (the window). `None` when constructed
    /// with `null`/no object, like `new VideoOverlay(null)`.
    action_owner: Option<ActionOwner>,
    /// Highest frame index for which `onFrameUpdate` has fired; `-1` before
    /// the first frame. Used to fire the event only when the frame advances.
    last_frame_update: i64,
    transition_call: Option<DetachedValue>,
    /// `setMixingLayer` state; `None` means no mixing bitmap
    /// (`resetMixingLayer` / a non-visible or null argument).
    mixing_layer: Option<MixingLayer>,
    /// `layer1`/`layer2` assignment state (reference `Layer1`/`Layer2`).
    layer1: Option<LayerRef>,
    layer2: Option<LayerRef>,

    // real decoding (FFmpeg) and the presented layer bitmap
    /// Decoder for the currently-open movie (`None` for unparsed files).
    decoder: Option<video::MovieDecoder>,
    /// The RGBA layer bitmap last presented by the movie clock.
    frame: Option<video::RgbaFrame>,

    // audio / mixing passthrough
    mode: i64,
    audio_balance: i64,
    audio_volume: i64,
    enabled_audio_stream: i64,
    enabled_video_stream: i64,
    mixing_alpha: f64,
    mixing_bg: i64,

    // video adjustment passthrough (inert without a decoder)
    contrast: f64,
    brightness: f64,
    hue: f64,
    saturation: f64,
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
            action_owner: None,
            last_frame_update: -1,
            transition_call: None,
            mixing_layer: None,
            layer1: None,
            layer2: None,
            decoder: None,
            frame: None,
            mode: 0,
            audio_balance: 0,
            audio_volume: 0,
            enabled_audio_stream: 0,
            enabled_video_stream: 0,
            mixing_alpha: 1.0,
            mixing_bg: 0,
            contrast: 1.0,
            brightness: 0.0,
            hue: 0.0,
            saturation: 1.0,
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

/// Decode and store the RGBA layer bitmap for the overlay's current movie
/// position. A no-op when the movie has no real decoder (the legacy MPEG
/// fallback accepts metadata but cannot decode pixels).
///
/// When `setMixingLayer` is active the decoded movie is composited over the
/// mixing layer's MainImage (opacity-weighted) and the `mixingMovieBGColor`
/// base at `mixingMovieAlpha`, the reference's `vomMixer` output.
fn present_current(inst: &mut VideoOverlayInst, now: u64) {
    if inst.decoder.is_none() {
        return;
    }
    let position = current_position_ms(inst, now);
    let decoded = inst
        .decoder
        .as_mut()
        .map(|decoder| decoder.present_at(position));
    match decoded {
        Some(Ok(Some(mut frame))) => {
            if let Some(layer) = inst.mixing_layer.as_ref() {
                let background = read_mixing_background(context_engine(), layer);
                compose_mixing(
                    &mut frame,
                    background.as_ref(),
                    layer.alpha,
                    inst.mixing_alpha,
                    inst.mixing_bg,
                );
            }
            inst.frame = Some(frame);
        }
        Some(Ok(None)) => {}
        Some(Err(e)) => log::debug!("VideoOverlay: present at {position} ms failed: {e}"),
        None => {}
    }
}

/// Copy a mixing layer's MainImage RGBA pixels out of the scene.
///
/// `mainImageBuffer` / `mainImageBufferPitch` are the reference's plugin ABI
/// (`LayerIntf.cpp:3005`/`:3020`): the live MainImage pixel address and its
/// row stride. The VM is single-threaded, so the getters and this copy run in
/// one native call and the buffer cannot be resized or moved concurrently.
fn read_mixing_background(engine: &Tjs2Engine, layer: &MixingLayer) -> Option<MixingBackground> {
    log::trace!("VideoOverlay: reading mixing layer {} MainImage", layer.id);
    let id = layer.keepalive.raw_id();
    let width = match engine.get_member(id, "imageWidth") {
        Ok(TjsValue::Integer(w)) if (1..=MAX_MIXING_DIM).contains(&w) => w as usize,
        _ => return None,
    };
    let height = match engine.get_member(id, "imageHeight") {
        Ok(TjsValue::Integer(h)) if (1..=MAX_MIXING_DIM).contains(&h) => h as usize,
        _ => return None,
    };
    let addr = match engine.get_member(id, "mainImageBuffer") {
        Ok(TjsValue::Integer(a)) if a > 0 => a as usize,
        _ => return None,
    };
    let pitch = match engine.get_member(id, "mainImageBufferPitch") {
        Ok(TjsValue::Integer(p)) if p > 0 => p as usize,
        _ => width.checked_mul(4)?,
    };
    let row_bytes = width.checked_mul(4)?;
    if pitch < row_bytes {
        return None;
    }
    let len = pitch.checked_mul(height)?;
    // SAFETY: `mainImageBuffer` returns the address of the layer's live
    // RGBA MainImage (the reference's `GetMainImagePixelBuffer` contract).
    // `len = pitch * height` is exactly the allocation the getter's `pitch`
    // and `imageHeight` describe; `imageWidth <= pitch/4`, so each copied row
    // stays inside the buffer. No VM code runs between the getters and the
    // copy, so the buffer cannot be freed or reallocated underneath us.
    let data = unsafe { std::slice::from_raw_parts(addr as *const u8, len) };
    let mut rgba = Vec::with_capacity(row_bytes.checked_mul(height)?);
    for y in 0..height {
        let start = y.checked_mul(pitch)?;
        rgba.extend_from_slice(&data[start..start + row_bytes]);
    }
    Some(MixingBackground {
        width,
        height,
        pitch: row_bytes,
        rgba,
    })
}

/// Composite the decoded movie frame over an optional mixing-layer image and
/// the `mixingMovieBGColor` base:
///
/// ```text
/// background = layer.rgb * (layer.a * layerOpacity) + bgColor * (1 - layer.a * layerOpacity)
/// out        = movie.rgb * (movie.a * movieAlpha) + background * (1 - movie.a * movieAlpha)
/// ```
///
/// The result is opaque because the mixer produces a full display surface
/// (`SetMixingBitmap` + `SetMixingMovieBGColor` fill the transparent areas).
fn compose_mixing(
    frame: &mut video::RgbaFrame,
    background: Option<&MixingBackground>,
    layer_opacity: f64,
    movie_alpha: f64,
    bg_color: i64,
) {
    let base_r = ((bg_color >> 16) & 0xff) as f64;
    let base_g = ((bg_color >> 8) & 0xff) as f64;
    let base_b = (bg_color & 0xff) as f64;
    let layer_opacity = layer_opacity.clamp(0.0, 1.0);
    let movie_alpha = movie_alpha.clamp(0.0, 1.0);
    let width = frame.width as usize;
    for y in 0..frame.height as usize {
        for x in 0..width {
            let i = (y * width + x) * 4;
            let (mut br, mut bg, mut bb) = (base_r, base_g, base_b);
            if let Some(image) = background
                && x < image.width
                && y < image.height
            {
                let j = y * image.pitch + x * 4;
                let ba = (image.rgba[j + 3] as f64 / 255.0) * layer_opacity;
                let inv = 1.0 - ba;
                br = image.rgba[j] as f64 * ba + base_r * inv;
                bg = image.rgba[j + 1] as f64 * ba + base_g * inv;
                bb = image.rgba[j + 2] as f64 * ba + base_b * inv;
            }
            let ma = (frame.data[i + 3] as f64 / 255.0) * movie_alpha;
            let inv = 1.0 - ma;
            let blend = |src: u8, dst: f64| -> u8 {
                (src as f64 * ma + dst * inv).round().clamp(0.0, 255.0) as u8
            };
            frame.data[i] = blend(frame.data[i], br);
            frame.data[i + 1] = blend(frame.data[i + 1], bg);
            frame.data[i + 2] = blend(frame.data[i + 2], bb);
            frame.data[i + 3] = 255;
        }
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

// ---------------------------------------------------------------------------
// Action dispatch (TVP_ACTION_INVOKE)
// ---------------------------------------------------------------------------

/// One member value copied into the event dictionary the action receives.
enum VideoEventArg {
    Int(i64),
    Str(String),
}

/// The TJS helper implementing `TVP_ACTION_INVOKE` (`EventIntf.h:208`): build
/// the event dictionary `%[type, target, ...members]` and call
/// `owner.action(ev)`. The reference's video-overlay events carry at most two
/// members (`onCallbackCommand(command, arg)`).
const VIDEO_OVERLAY_EVENT_DISPATCH: &str = "(function(owner,target,t,n1,v1,n2,v2){\
    var ev=%[type:t,target:target];\n    if(n1!==void)ev[n1]=v1;\n    if(n2!==void)ev[n2]=v2;\n    return owner.action(ev);})";

/// Dispatch one video-overlay event to its action owner: retain the owner and
/// the target (`objthis`), evaluate the helper closure and invoke it with the
/// event type plus alternating member name/value pairs. Mirrors the reference
/// `TVP_ACTION_INVOKE_END(tTJSVariantClosure(ActionOwner))`
/// (`VideoOvlIntf.cpp:421-480`).
fn dispatch_video_overlay_event(
    engine: &Tjs2Engine,
    owner_raw: *mut c_void,
    target: *mut c_void,
    event_type: &str,
    members: &[(&str, VideoEventArg)],
) {
    if owner_raw.is_null() || target.is_null() {
        return;
    }
    let Ok(owner) = engine.retain_object_detached(owner_raw) else {
        return;
    };
    let Ok(target_dv) = engine.retain_object_detached(target) else {
        return;
    };
    let Ok(helper) = engine.eval_retained(VIDEO_OVERLAY_EVENT_DISPATCH, "videoOverlayEvent") else {
        return;
    };
    let RetainedValue::Object(helper_dv) = helper else {
        return;
    };
    let mut args: Vec<TjsValue> = vec![
        TjsValue::Retained(owner.raw_id() as u64),
        TjsValue::Retained(target_dv.raw_id() as u64),
        TjsValue::String(event_type.to_string()),
    ];
    for i in 0..2 {
        match members.get(i) {
            Some((name, value)) => {
                args.push(TjsValue::String((*name).to_string()));
                args.push(match value {
                    VideoEventArg::Int(v) => TjsValue::Integer(*v),
                    VideoEventArg::Str(s) => TjsValue::String(s.clone()),
                });
            }
            None => {
                args.push(TjsValue::Void);
                args.push(TjsValue::Void);
            }
        }
    }
    // `owner`/`target_dv` retentions are consumed by the argument copy; their
    // drops are no-ops. Errors surface as a script `action` throw, which the
    // VM reports elsewhere; the native method itself stays void.
    if let Err(e) = engine.call_detached(&helper_dv, &args) {
        log::warn!("video overlay event dispatch ({event_type}) failed: {e}");
    }
}

fn dispatch_from_instance(
    instance: *mut c_void,
    objthis: *mut c_void,
    event_type: &str,
    members: &[(&str, VideoEventArg)],
) {
    // SAFETY: `instance` is a live VideoOverlayInst payload during the call.
    let inst = unsafe { &*(instance as *const VideoOverlayInst) };
    if let Some(owner) = &inst.action_owner {
        dispatch_video_overlay_event(context_engine(), owner.raw, objthis, event_type, members);
    }
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
        let (outcome, objthis, transition, frame_update) = {
            let inst = unsafe { &mut *ptr };
            let outcome = advance_playback(inst, now);
            present_current(inst, now);
            // Reference fires `onFrameUpdate` for every decoded frame while
            // playing; the clock tick is the closest equivalent here. Skip
            // repeats of the same frame so a paused/repeated tick does not
            // spam the callback.
            let frame_update = if inst.playing && inst.decoder.is_some() {
                let frame = current_frame(inst, now);
                if frame != inst.last_frame_update {
                    inst.last_frame_update = frame;
                    Some(frame)
                } else {
                    None
                }
            } else {
                None
            };
            let objthis = inst.objthis;
            let transition = if outcome.transition_complete {
                inst.transition_call.take()
            } else {
                None
            };
            (outcome, objthis, transition, frame_update)
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
        if let Some(frame) = frame_update {
            fire_member(objthis, "onFrameUpdate", &[TjsValue::Integer(frame)]);
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

/// Read-only real property that always returns a fixed value (the
/// `*RangeMin`/`*RangeMax`/`*DefaultValue`/`*StepSize` metadata the
/// reference's video backend reports).
macro_rules! ro_real_const {
    ($getter:ident, $value:expr) => {
        extern "C" fn $getter(
            _engine: *mut c_void,
            _instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            crate::set_real_out(out, $value);
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

// -- video adjustment (inert without a decoder) -----------------------------
//
// The reference forwards these to the platform video player (`KRMoviePlayer`
// in this fork overrides every getter with an empty body), so the ranges are
// effectively backend-defined. These are the documented neutral ranges used
// by the emulator; the values are stored but cannot affect pixels until a
// decoder exists.
real_prop!(contrast_get, contrast_set, contrast);
real_prop!(brightness_get, brightness_set, brightness);
real_prop!(hue_get, hue_set, hue);
real_prop!(saturation_get, saturation_set, saturation);
ro_real_const!(contrast_range_min_get, 0.0);
ro_real_const!(contrast_range_max_get, 2.0);
ro_real_const!(contrast_default_value_get, 1.0);
ro_real_const!(contrast_step_size_get, 0.01);
ro_real_const!(brightness_range_min_get, -1.0);
ro_real_const!(brightness_range_max_get, 1.0);
ro_real_const!(brightness_default_value_get, 0.0);
ro_real_const!(brightness_step_size_get, 0.01);
ro_real_const!(hue_range_min_get, -180.0);
ro_real_const!(hue_range_max_get, 180.0);
ro_real_const!(hue_default_value_get, 0.0);
ro_real_const!(hue_step_size_get, 1.0);
ro_real_const!(saturation_range_min_get, 0.0);
ro_real_const!(saturation_range_max_get, 2.0);
ro_real_const!(saturation_default_value_get, 1.0);
ro_real_const!(saturation_step_size_get, 0.01);

// -- real media metadata ----------------------------------------------------

ro_int_field!(original_width_get, original_width);
ro_int_field!(original_height_get, original_height);
ro_int_field!(total_frame_get, total_frame);
ro_int_field!(number_of_frame_get, total_frame);
ro_int_field!(total_time_get, total_time_ms);
ro_int_field!(number_of_audio_stream_get, number_of_audio_stream);
ro_int_field!(number_of_video_stream_get, number_of_video_stream);
ro_real_field!(fps_get, fps);

// -- decoded layer bitmap diagnostics ---------------------------------------
//
// These expose the real decoded RGBA layer bitmap and the decoded audio
// track to scripts (and to the integration tests). They are read-only and
// report 0 when the file has no real decoder (legacy MPEG fallback).

extern "C" fn frame_width_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &*(instance as *const VideoOverlayInst) };
    set_int_out(out, inst.frame.as_ref().map_or(0, |f| i64::from(f.width)));
    0
}

extern "C" fn frame_height_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &*(instance as *const VideoOverlayInst) };
    set_int_out(out, inst.frame.as_ref().map_or(0, |f| i64::from(f.height)));
    0
}

extern "C" fn frame_bytes_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &*(instance as *const VideoOverlayInst) };
    set_int_out(out, inst.frame.as_ref().map_or(0, |f| f.data.len() as i64));
    0
}

extern "C" fn frame_checksum_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &*(instance as *const VideoOverlayInst) };
    set_int_out(out, inst.frame.as_ref().map_or(0, |f| f.checksum() as i64));
    0
}

extern "C" fn audio_sample_count_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &*(instance as *const VideoOverlayInst) };
    let count = inst
        .decoder
        .as_ref()
        .and_then(|d| d.cached_audio())
        .map_or(0, |pcm| pcm.sample_values() as i64);
    set_int_out(out, count);
    0
}

extern "C" fn audio_sample_rate_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &*(instance as *const VideoOverlayInst) };
    let rate = inst
        .decoder
        .as_ref()
        .and_then(|d| d.cached_audio())
        .map_or(0, |pcm| i64::from(pcm.sample_rate));
    set_int_out(out, rate);
    0
}

extern "C" fn audio_channels_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &*(instance as *const VideoOverlayInst) };
    let channels = inst
        .decoder
        .as_ref()
        .and_then(|d| d.cached_audio())
        .map_or(0, |pcm| i64::from(pcm.channels));
    set_int_out(out, channels);
    0
}

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
    let now = now_ms();
    inst.position_ms = position;
    inst.play_anchor_position_ms = position;
    inst.play_anchor_ms = now;
    inst.period_fired = false;
    present_current(inst, now);
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
    let now = now_ms();
    inst.position_ms = frame_to_ms(inst.fps, frame);
    inst.play_anchor_position_ms = inst.position_ms;
    inst.play_anchor_ms = now;
    inst.period_fired = false;
    present_current(inst, now);
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

/// `new VideoOverlay(win)` / `super.VideoOverlay(...)`: remember `objthis` so
/// the `on*` callbacks can be invoked later, and retain the action owner
/// (reference `ActionOwner = param[0]`, `VideoOvlIntf.cpp:44`). The
/// constructor's first argument is the window object; `null`/no argument
/// leaves the action owner unset (the reference throws only when the argument
/// is not a Window, which this headless port cannot validate).
extern "C" fn vo_ctor(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is a live payload and `objthis` a live object for
    // the duration of the call.
    let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
    inst.objthis = objthis;
    let values = args(argv, argc);
    if let Some(first) = values.first()
        && first.ty == tjs2_sys::VAL_OBJECT
        && first.retained != 0
    {
        let raw = first.retained as *mut c_void;
        if let Ok(keepalive) = context_engine().retain_object_detached(raw) {
            inst.action_owner = Some(ActionOwner {
                raw,
                _keepalive: keepalive,
            });
        }
    }
    set_void_out(out);
    0
}

// ---------------------------------------------------------------------------
// Native event entry points (`VideoOvlIntf.cpp:421-480`)
// ---------------------------------------------------------------------------

/// `onStatusChanged(status)` — fallback for a script that does not override
/// the event: dispatch `%[type:"onStatusChanged", target:this, status:...]`
/// to `actionOwner.action`.
extern "C" fn vo_on_status_changed(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let values = args(argv, argc);
    let Some(status) = values.first() else {
        return report_error(
            out_error,
            "VideoOverlay.onStatusChanged requires 1 argument",
        );
    };
    dispatch_from_instance(
        instance,
        objthis,
        "onStatusChanged",
        &[("status", VideoEventArg::Str(value_as_string(status)))],
    );
    set_void_out(out);
    0
}

/// `onCallbackCommand(command, arg)` — reference `FireCallbackCommand`
/// (`VideoOvlIntf.cpp:139`): dispatch the decoder callback command to the
/// action owner.
extern "C" fn vo_on_callback_command(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let values = args(argv, argc);
    if values.len() < 2 {
        return report_error(
            out_error,
            "VideoOverlay.onCallbackCommand requires 2 arguments",
        );
    }
    dispatch_from_instance(
        instance,
        objthis,
        "onCallbackCommand",
        &[
            ("command", VideoEventArg::Str(value_as_string(&values[0]))),
            ("arg", VideoEventArg::Str(value_as_string(&values[1]))),
        ],
    );
    set_void_out(out);
    0
}

/// `onPeriod(reason)` — reference `FirePeriodEvent` (`VideoOvlIntf.cpp:152`).
extern "C" fn vo_on_period(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let values = args(argv, argc);
    let Some(reason) = values.first() else {
        return report_error(out_error, "VideoOverlay.onPeriod requires 1 argument");
    };
    dispatch_from_instance(
        instance,
        objthis,
        "onPeriod",
        &[("reason", VideoEventArg::Int(value_as_i64(reason)))],
    );
    set_void_out(out);
    0
}

/// `onFrameUpdate(frame)` — reference `FireFrameUpdateEvent`
/// (`VideoOvlIntf.cpp:168`).
extern "C" fn vo_on_frame_update(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let values = args(argv, argc);
    let Some(frame) = values.first() else {
        return report_error(out_error, "VideoOverlay.onFrameUpdate requires 1 argument");
    };
    dispatch_from_instance(
        instance,
        objthis,
        "onFrameUpdate",
        &[("frame", VideoEventArg::Int(value_as_i64(frame)))],
    );
    set_void_out(out);
    0
}

/// `open(file)`: read the file through the game storage and open it with
/// FFmpeg (the game's `.mpg` files are really H.264/AAC MP4 containers),
/// falling back to the legacy MPEG-1/2 sequence parser when FFmpeg cannot
/// open the bytes. A missing file raises a TJS error; an opened file that
/// neither decoder understands is accepted with zero metadata (logged).
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
    // The MPEG header parser is cheap and kept as a fallback for the
    // synthetic elementary streams the tests build.
    let mpeg = parse_mpeg_metadata(&bytes);
    let decoder = match video::MovieDecoder::open(bytes) {
        Ok(decoder) => Some(decoder),
        Err(e) => {
            log::warn!("VideoOverlay.open({file:?}): media decode failed: {e}");
            None
        }
    };
    let ffmpeg = decoder.as_ref().map(|d| d.metadata().clone());
    let frame = decoder.as_ref().and_then(|d| d.current_frame().cloned());
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
    apply_open(inst, ffmpeg, mpeg, decoder, frame, &file);
    set_void_out(out);
    0
}

fn apply_open(
    inst: &mut VideoOverlayInst,
    ffmpeg: Option<video::MovieMetadata>,
    mpeg: Option<MpegMetadata>,
    decoder: Option<video::MovieDecoder>,
    frame: Option<video::RgbaFrame>,
    file: &str,
) {
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
    inst.decoder = decoder;
    inst.frame = frame;
    set_active(inst as *mut VideoOverlayInst, false);

    if let Some(metadata) = ffmpeg {
        apply_metadata(
            inst,
            metadata.width,
            metadata.height,
            metadata.fps,
            metadata.total_frames,
            metadata.total_time_ms,
            metadata.audio_streams,
            metadata.video_streams,
        );
        log::info!(
            "VideoOverlay.open({file:?}): {}x{} @ {:.3} fps, {} frame(s), {} audio stream(s) [ffmpeg]",
            metadata.width,
            metadata.height,
            metadata.fps,
            metadata.total_frames,
            metadata.audio_streams,
        );
    } else if let Some(metadata) = mpeg {
        apply_metadata(
            inst,
            metadata.width,
            metadata.height,
            metadata.fps,
            metadata.total_frames,
            metadata.total_time_ms,
            metadata.audio_streams,
            metadata.video_streams,
        );
        log::info!(
            "VideoOverlay.open({file:?}): {}x{} @ {:.3} fps, {} frame(s) [mpeg sequence header]",
            metadata.width,
            metadata.height,
            metadata.fps,
            metadata.total_frames,
        );
    } else {
        log::warn!("VideoOverlay.open({file:?}): no decodable video; metadata unavailable");
    }
}

/// Store decoded metadata in the instance fields and adopt the coded size
/// as the default rectangle when the script has not set one.
#[allow(clippy::too_many_arguments)]
fn apply_metadata(
    inst: &mut VideoOverlayInst,
    width: u32,
    height: u32,
    fps: f64,
    total_frames: i64,
    total_time_ms: i64,
    audio_streams: i64,
    video_streams: i64,
) {
    inst.original_width = i64::from(width);
    inst.original_height = i64::from(height);
    inst.fps = fps;
    inst.total_frame = total_frames;
    inst.total_time_ms = total_time_ms;
    inst.number_of_audio_stream = audio_streams;
    inst.number_of_video_stream = video_streams.max(1);
    if inst.width == 0 {
        inst.width = i64::from(width);
    }
    if inst.height == 0 {
        inst.height = i64::from(height);
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
    inst.decoder = None;
    inst.frame = None;
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
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    present_current(unsafe { &mut *(instance as *mut VideoOverlayInst) }, now);
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
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    present_current(unsafe { &mut *(instance as *mut VideoOverlayInst) }, now);
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
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    present_current(
        unsafe { &mut *(instance as *mut VideoOverlayInst) },
        now_ms(),
    );
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
    present_current(inst, now);
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

/// Resolve a retained Layer object to the `setMixingLayer` state.
///
/// Reference `tTJSNI_VideoOverlay::SetMixingLayer` (`VideoOvlImpl.cpp:882`):
/// a null layer clears the mixing bitmap; a layer that is not visible resets
/// it; otherwise the mixing bitmap is the layer's MainImage drawn with
/// `opacity / 255`. The reference throws `TVPSpecifyLayer` for a non-Layer
/// object; here that is a missing `nativeId` (every real/user `Layer`
/// carries it, including script subclasses).
fn resolve_mixing_layer(
    engine: &Tjs2Engine,
    keepalive: DetachedValue,
) -> Result<Option<MixingLayer>, String> {
    let id = match engine.get_member(keepalive.raw_id(), "nativeId") {
        Ok(TjsValue::Integer(id)) if id >= 0 => id as u32,
        _ => return Err("VideoOverlay.setMixingLayer: specify layer".to_string()),
    };
    let visible = match engine.get_member(keepalive.raw_id(), "visible") {
        Ok(TjsValue::Integer(v)) => v != 0,
        Ok(TjsValue::Real(v)) => v != 0.0,
        _ => true,
    };
    if !visible {
        return Ok(None);
    }
    let opacity = match engine.get_member(keepalive.raw_id(), "opacity") {
        Ok(TjsValue::Integer(o)) => o.clamp(0, 255),
        Ok(TjsValue::Real(o)) => (o as i64).clamp(0, 255),
        _ => 255,
    };
    log::debug!(
        "VideoOverlay.setMixingLayer: layer {id}, alpha {:.3}",
        opacity as f64 / 255.0
    );
    Ok(Some(MixingLayer {
        keepalive,
        id,
        alpha: opacity as f64 / 255.0,
    }))
}

/// `setMixingLayer(layer)` — install (or clear) the layer the movie is mixed
/// over. A null/void argument and a non-visible layer both clear the mixing
/// bitmap, matching the reference; a non-Layer object raises the reference's
/// `TVPSpecifyLayer` error.
extern "C" fn vo_set_mixing_layer(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let values = args(argv, argc);
    let Some(value) = values.first() else {
        return report_error(out_error, "VideoOverlay.setMixingLayer requires 1 argument");
    };
    // SAFETY: `instance` is a live VideoOverlayInst payload.
    let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
    if value.ty != tjs2_sys::VAL_OBJECT || value.retained == 0 {
        inst.mixing_layer = None;
        set_void_out(out);
        return 0;
    }
    let keepalive = match context_engine().retain_object_arg(value) {
        Ok(dv) => dv,
        Err(_) => return report_error(out_error, "VideoOverlay.setMixingLayer: specify layer"),
    };
    match resolve_mixing_layer(context_engine(), keepalive) {
        Ok(layer) => {
            inst.mixing_layer = layer;
            set_void_out(out);
            0
        }
        Err(e) => report_error(out_error, &e),
    }
}

/// `resetMixingLayer()` — drop the mixing bitmap (`VideoOvlImpl.cpp:921`).
extern "C" fn vo_reset_mixing_layer(
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
    inst.mixing_layer = None;
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

/// Write a TJS `null` result (distinct from `void`) into `out`.
fn vo_set_null_out(out: *mut Value) {
    // SAFETY: `out` is a valid result slot for the duration of the call.
    unsafe {
        (*out).ty = tjs2_sys::VAL_NULL;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = std::ptr::null();
        (*out).array = std::ptr::null();
        (*out).array_count = 0;
        (*out).retained = 0;
    }
}

/// Hand a retained object id back as the callback result (the trampoline
/// consumes the id while copying the value).
fn vo_set_retained_out(out: *mut Value, dv: DetachedValue) {
    let id = dv.raw_id();
    // SAFETY: `out` is a valid result slot for the duration of the call.
    unsafe {
        (*out).ty = tjs2_sys::VAL_RETAINED;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = std::ptr::null();
        (*out).array = std::ptr::null();
        (*out).array_count = 0;
        (*out).retained = id as usize;
    }
    std::mem::forget(dv);
}

/// Resolve a `layer1`/`layer2` setter argument to a [`LayerRef`].
///
/// Reference `tTJSNI_VideoOverlay::SetLayer1`/`SetLayer2`
/// (`VideoOvlImpl.cpp:792`): a null/void argument clears the slot; any other
/// object must be a Layer (the reference resolves `tTJSNC_Layer::ClassID`
/// through `NativeInstanceSupport` and throws `TVPSpecifyLayer` otherwise).
/// krkr-rs validates the same way [`resolve_mixing_layer`] does: a Layer
/// object (including a script subclass) carries a non-negative `nativeId`.
fn resolve_layer_ref(engine: &Tjs2Engine, value: &Value) -> Result<Option<LayerRef>, String> {
    let objthis = value.object_handle();
    if value.ty != tjs2_sys::VAL_OBJECT || objthis.is_null() {
        return Ok(None);
    }
    let keepalive = engine
        .retain_object_arg(value)
        .map_err(|_| "VideoOverlay: specify layer".to_string())?;
    let id = match engine.get_member(keepalive.raw_id(), "nativeId") {
        Ok(TjsValue::Integer(id)) if id >= 0 => id as u32,
        _ => return Err("VideoOverlay: specify layer".to_string()),
    };
    Ok(Some(LayerRef {
        keepalive,
        objthis,
        id,
    }))
}

/// Write the retained `layer1`/`layer2` object (or TJS `null`) into `out`.
fn return_layer_ref(engine: &Tjs2Engine, layer: Option<&LayerRef>, out: *mut Value) {
    let Some(layer) = layer else {
        return vo_set_null_out(out);
    };
    match engine.retain_object_detached(layer.objthis) {
        Ok(dv) => vo_set_retained_out(out, dv),
        // The Layer object is gone; the reference reads back as null.
        Err(_) => vo_set_null_out(out),
    }
}

/// Generate the `layer1`/`layer2` getter/setter pair for one instance field.
macro_rules! vo_layer_property {
    ($get:ident, $set:ident, $field:ident) => {
        extern "C" fn $get(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is a live VideoOverlayInst payload.
            let inst = unsafe { &*(instance as *const VideoOverlayInst) };
            return_layer_ref(context_engine(), inst.$field.as_ref(), out);
            0
        }

        extern "C" fn $set(
            _engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: `instance` is a live VideoOverlayInst payload and
            // `value` is valid for the duration of the callback.
            let inst = unsafe { &mut *(instance as *mut VideoOverlayInst) };
            let value = unsafe { &*value };
            match resolve_layer_ref(context_engine(), value) {
                Ok(layer) => {
                    inst.$field = layer;
                    0
                }
                Err(e) => report_error(out_error, &e),
            }
        }
    };
}

vo_layer_property!(vo_layer1_get, vo_layer1_set, layer1);
vo_layer_property!(vo_layer2_get, vo_layer2_set, layer2);

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
                f: vo_set_mixing_layer,
            },
            NativeInstanceMethodDef {
                name: "resetMixingLayer",
                f: vo_reset_mixing_layer,
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
            NativeInstanceMethodDef {
                name: "onStatusChanged",
                f: vo_on_status_changed,
            },
            NativeInstanceMethodDef {
                name: "onCallbackCommand",
                f: vo_on_callback_command,
            },
            NativeInstanceMethodDef {
                name: "onPeriod",
                f: vo_on_period,
            },
            NativeInstanceMethodDef {
                name: "onFrameUpdate",
                f: vo_on_frame_update,
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
                name: "contrast",
                get: Some(contrast_get),
                set: Some(contrast_set),
            },
            NativeInstancePropertyDef {
                name: "contrastRangeMin",
                get: Some(contrast_range_min_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "contrastRangeMax",
                get: Some(contrast_range_max_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "contrastDefaultValue",
                get: Some(contrast_default_value_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "contrastStepSize",
                get: Some(contrast_step_size_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "brightness",
                get: Some(brightness_get),
                set: Some(brightness_set),
            },
            NativeInstancePropertyDef {
                name: "brightnessRangeMin",
                get: Some(brightness_range_min_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "brightnessRangeMax",
                get: Some(brightness_range_max_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "brightnessDefaultValue",
                get: Some(brightness_default_value_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "brightnessStepSize",
                get: Some(brightness_step_size_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "hue",
                get: Some(hue_get),
                set: Some(hue_set),
            },
            NativeInstancePropertyDef {
                name: "hueRangeMin",
                get: Some(hue_range_min_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "hueRangeMax",
                get: Some(hue_range_max_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "hueDefaultValue",
                get: Some(hue_default_value_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "hueStepSize",
                get: Some(hue_step_size_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "saturation",
                get: Some(saturation_get),
                set: Some(saturation_set),
            },
            NativeInstancePropertyDef {
                name: "saturationRangeMin",
                get: Some(saturation_range_min_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "saturationRangeMax",
                get: Some(saturation_range_max_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "saturationDefaultValue",
                get: Some(saturation_default_value_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "saturationStepSize",
                get: Some(saturation_step_size_get),
                set: None,
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
                name: "frameWidth",
                get: Some(frame_width_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "frameHeight",
                get: Some(frame_height_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "frameBytes",
                get: Some(frame_bytes_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "frameChecksum",
                get: Some(frame_checksum_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "audioSampleCount",
                get: Some(audio_sample_count_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "audioSampleRate",
                get: Some(audio_sample_rate_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "audioChannels",
                get: Some(audio_channels_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "layer1",
                get: Some(vo_layer1_get),
                set: Some(vo_layer1_set),
            },
            NativeInstancePropertyDef {
                name: "layer2",
                get: Some(vo_layer2_get),
                set: Some(vo_layer2_set),
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

    // -- native event entry points (TVP_ACTION_INVOKE) ---------------------

    /// The four reference native event methods build the event dictionary
    /// and invoke `actionOwner.action(ev)` when the script does not override
    /// the event name (`VideoOvlIntf.cpp:421-480`).
    #[test]
    fn native_event_methods_dispatch_to_action_owner() {
        let _vm_lock = vm_lock();
        let engine = Tjs2Engine::new().expect("create engine");
        crate::register_all(&engine).expect("register all natives");
        engine
            .exec_script(
                r#"
                // The action method mutates `this.log` (TJS dictionary-literal
                // functions do not close over the outer script scope).
                var owner = %[log: [], action: function(ev) {
                    this.log.push(ev.type);
                    if (ev.type === 'onStatusChanged') this.log.push(ev.status);
                    if (ev.type === 'onCallbackCommand') { this.log.push(ev.command); this.log.push(ev.arg); }
                    if (ev.type === 'onPeriod') this.log.push(ev.reason);
                    if (ev.type === 'onFrameUpdate') this.log.push(ev.frame);
                }];
                var v = new VideoOverlay(owner);
                v.onStatusChanged('play');
                v.onCallbackCommand('cmd', 'arg');
                v.onPeriod(1);
                v.onFrameUpdate(7);
                "#,
                "video_overlay_events",
            )
            .expect("script runs");
        let expect = [
            TjsValue::String("onStatusChanged".into()),
            TjsValue::String("play".into()),
            TjsValue::String("onCallbackCommand".into()),
            TjsValue::String("cmd".into()),
            TjsValue::String("arg".into()),
            TjsValue::String("onPeriod".into()),
            TjsValue::Integer(1),
            TjsValue::String("onFrameUpdate".into()),
            TjsValue::Integer(7),
        ];
        assert_eq!(
            engine.eval("owner.log.count", "test").unwrap(),
            TjsValue::Integer(expect.len() as i64)
        );
        for (i, want) in expect.iter().enumerate() {
            assert_eq!(
                engine.eval(&format!("owner.log[{i}]"), "test").unwrap(),
                want.clone(),
                "owner.log[{i}]"
            );
        }
    }

    /// A script override of the event name wins over the native fallback,
    /// like the reference's `TVPPostEvent` member lookup.
    #[test]
    fn script_override_wins_over_native_event_method() {
        let _vm_lock = vm_lock();
        let engine = Tjs2Engine::new().expect("create engine");
        crate::register_all(&engine).expect("register all natives");
        engine
            .exec_script(
                r#"
                var fired = '';
                var owner = %[acted: 0, action: function(ev) { this.acted++; }];
                var v = new VideoOverlay(owner);
                v.onStatusChanged = function(s) { fired = s; };
                v.onStatusChanged('pause');
                "#,
                "video_overlay_override",
            )
            .expect("script runs");
        assert_eq!(
            engine.eval("fired", "test").unwrap(),
            TjsValue::String("pause".into())
        );
        assert_eq!(
            engine.eval("owner.acted", "test").unwrap(),
            TjsValue::Integer(0)
        );
    }

    /// Without an action owner (`new VideoOverlay(null)`) the native event
    /// methods are safe no-ops (the reference only dispatches when
    /// `ActionOwner.Object` is set).
    #[test]
    fn native_event_methods_without_action_owner_are_safe() {
        let _vm_lock = vm_lock();
        let engine = Tjs2Engine::new().expect("create engine");
        crate::register_all(&engine).expect("register all natives");
        engine
            .exec_script(
                "var v = new VideoOverlay(null); v.onStatusChanged('play'); \
                 v.onCallbackCommand('a', 'b'); v.onPeriod(1); v.onFrameUpdate(2);",
                "video_overlay_no_owner",
            )
            .expect("script runs");
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

    #[test]
    fn adjustment_properties_round_trip() {
        let _vm_lock = vm_lock();
        let engine = Tjs2Engine::new().expect("create engine");
        crate::register_all(&engine).expect("register all natives");
        engine
            .exec_script(
                r#"
                var v = new VideoOverlay(null);
                var before = v.contrast;
                var cmin = v.contrastRangeMin;
                v.contrast = 1.5;
                v.brightness = -0.5;
                v.hue = 90.0;
                v.saturation = 0.25;
                var after = v.contrast;
                var sat = v.saturation;
                var smax = v.saturationRangeMax;
                "#,
                "video_overlay_adjust",
            )
            .expect("adjustment members must not throw");
        assert_eq!(engine.eval("before", "test").unwrap(), TjsValue::Real(1.0));
        assert_eq!(engine.eval("after", "test").unwrap(), TjsValue::Real(1.5));
        assert_eq!(engine.eval("sat", "test").unwrap(), TjsValue::Real(0.25));
        assert_eq!(engine.eval("cmin", "test").unwrap(), TjsValue::Real(0.0));
        assert_eq!(engine.eval("smax", "test").unwrap(), TjsValue::Real(2.0));
    }

    // -- mixing layer (setMixingLayer / resetMixingLayer) -------------------

    /// The mixer compositor implements the reference's movie-over-layer
    /// formula (movie over `mixingMovieBGColor`, layer opacity-weighted).
    #[test]
    fn compose_mixing_matches_the_reference_formula() {
        let _vm_lock = vm_lock();
        // Fully opaque movie wins over an opaque layer background.
        let mut frame = video::RgbaFrame {
            width: 2,
            height: 1,
            pts_ms: 0,
            data: vec![255, 255, 255, 255, 255, 0, 0, 255],
        };
        let background = MixingBackground {
            width: 2,
            height: 1,
            pitch: 8,
            rgba: vec![0, 255, 0, 255, 0, 0, 255, 255],
        };
        compose_mixing(&mut frame, Some(&background), 1.0, 1.0, 0x000000);
        assert_eq!(&frame.data[0..4], &[255, 255, 255, 255]);
        assert_eq!(&frame.data[4..8], &[255, 0, 0, 255]);

        // Half movie alpha blends white over the black background.
        let mut frame = video::RgbaFrame {
            width: 1,
            height: 1,
            pts_ms: 0,
            data: vec![255, 255, 255, 255],
        };
        let black = MixingBackground {
            width: 1,
            height: 1,
            pitch: 4,
            rgba: vec![0, 0, 0, 255],
        };
        compose_mixing(&mut frame, Some(&black), 1.0, 0.5, 0x000000);
        assert_eq!(frame.data, vec![128, 128, 128, 255]);

        // A fully transparent movie exposes the `mixingMovieBGColor` base
        // (0xRRGGBB): 0x00ff0000 is red.
        let mut frame = video::RgbaFrame {
            width: 1,
            height: 1,
            pts_ms: 0,
            data: vec![255, 255, 255, 0],
        };
        compose_mixing(&mut frame, None, 0.0, 1.0, 0x00ff_0000);
        assert_eq!(frame.data, vec![255, 0, 0, 255]);

        // A half-opacity layer contributes its image to the base colour.
        let mut frame = video::RgbaFrame {
            width: 1,
            height: 1,
            pts_ms: 0,
            data: vec![0, 0, 0, 0],
        };
        let white = MixingBackground {
            width: 1,
            height: 1,
            pitch: 4,
            rgba: vec![255, 255, 255, 255],
        };
        compose_mixing(&mut frame, Some(&white), 0.5, 0.0, 0x000000);
        assert_eq!(frame.data, vec![128, 128, 128, 255]);
    }

    /// `resolve_mixing_layer` mirrors the reference: a visible Layer resolves
    /// to its id + opacity; a non-visible layer clears the bitmap; a non-Layer
    /// object raises `TVPSpecifyLayer`.
    #[test]
    fn resolve_mixing_layer_matches_reference_semantics() {
        let _vm_lock = vm_lock();
        let engine = Tjs2Engine::new().expect("create engine");
        engine
            .exec_script(
                "var shown = %[nativeId: 3, visible: 1, opacity: 128]; \
                 var hidden = %[nativeId: 4, visible: 0, opacity: 255]; \
                 var plain = %[foo: 1];",
                "video_overlay_mixing",
            )
            .unwrap();

        let RetainedValue::Object(dv) = engine.eval_retained("shown", "test").unwrap() else {
            panic!("shown must be an object");
        };
        let layer = resolve_mixing_layer(&engine, dv)
            .unwrap()
            .expect("a visible layer resolves");
        assert_eq!(layer.id, 3);
        assert!((layer.alpha - 128.0 / 255.0).abs() < 1e-9);

        let RetainedValue::Object(dv) = engine.eval_retained("hidden", "test").unwrap() else {
            panic!("hidden must be an object");
        };
        assert!(
            resolve_mixing_layer(&engine, dv).unwrap().is_none(),
            "a hidden layer resets the mixing bitmap"
        );

        let RetainedValue::Object(dv) = engine.eval_retained("plain", "test").unwrap() else {
            panic!("plain must be an object");
        };
        assert!(
            resolve_mixing_layer(&engine, dv).is_err(),
            "a non-Layer object raises TVPSpecifyLayer"
        );
    }

    /// The layer's MainImage is read through the reference's
    /// `mainImageBuffer`/`mainImageBufferPitch` ABI and copied row by row.
    #[test]
    fn read_mixing_background_copies_the_layer_main_image() {
        let _vm_lock = vm_lock();
        let engine = Tjs2Engine::new().expect("create engine");
        // A 2x2 RGBA image with a 4-byte gap per row would exercise padding;
        // here the rows are tightly packed (pitch = width * 4).
        let pixels: Vec<u8> = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        let addr = pixels.as_ptr() as i64;
        engine
            .exec_script(
                &format!(
                    "var fake = %[nativeId: 1, visible: 1, opacity: 255, \
                     imageWidth: 2, imageHeight: 2, \
                     mainImageBuffer: {addr}, mainImageBufferPitch: 8];"
                ),
                "video_overlay_mixing",
            )
            .unwrap();
        let RetainedValue::Object(dv) = engine.eval_retained("fake", "test").unwrap() else {
            panic!("fake must be an object");
        };
        let layer = resolve_mixing_layer(&engine, dv).unwrap().unwrap();
        let background = read_mixing_background(&engine, &layer).expect("image buffer");
        assert_eq!(
            (background.width, background.height, background.pitch),
            (2, 2, 8)
        );
        assert_eq!(background.rgba, pixels);
    }

    /// `layer1`/`layer2` store the assigned Layer object and return it
    /// (reference `tTJSNI_VideoOverlay::SetLayer1/GetLayer1`,
    /// `VideoOvlImpl.cpp:792`). A non-Layer object raises `TVPSpecifyLayer`.
    #[test]
    fn video_overlay_layer_refs_roundtrip_and_validate() {
        let _vm_lock = vm_lock();
        let engine = Tjs2Engine::new().expect("create engine");
        crate::register_all(&engine).expect("register all natives");
        engine
            .exec_script(
                "var v = new VideoOverlay(null); \
                 var l = %[nativeId: 7]; \
                 v.layer1 = l; \
                 var same1 = (v.layer1 === l); \
                 v.layer2 = l; \
                 var same2 = (v.layer2 === l); \
                 v.layer1 = null; \
                 var cleared = (v.layer1 === null);",
                "video_overlay_layer_refs",
            )
            .expect("layer refs must round-trip");
        assert_eq!(engine.eval("same1", "test").unwrap(), TjsValue::Integer(1));
        assert_eq!(engine.eval("same2", "test").unwrap(), TjsValue::Integer(1));
        assert_eq!(
            engine.eval("cleared", "test").unwrap(),
            TjsValue::Integer(1)
        );
        // A non-Layer object (no `nativeId`) must raise, not be stored.
        let thrown = engine
            .eval(
                "(function(){ try { v.layer1 = %[]; return 0; } catch(e){ return 1; } })()",
                "test",
            )
            .unwrap();
        assert_eq!(thrown, TjsValue::Integer(1));
    }
}
