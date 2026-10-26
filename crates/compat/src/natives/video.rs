//! `VideoOverlay` native class.
//!
//! Behavioral reference: `main`'s `crates/tvp-natives/src/video_overlay.rs`
//! and the reference `tTJSNC_VideoOverlay`
//! (`reference/cpp/core/visual/VideoOvlIntf.cpp` +
//! `reference/cpp/core/visual/impl/VideoOvlImpl.{h,cpp}`).
//!
//! # What this composes
//!
//! The class is a thin adapter over the L2 `engine_api::VideoOverlay`
//! playback machine: playback commands, the position/frame clock, bounds,
//! visibility, whole-stream/segment looping, the period event, the play rate
//! and the event queue all live in the L2 value. L3 owns only the per-object
//! *dispatch* state ([`VideoState`]) — the id → [`VideoSlot`] registry, the
//! `layer1`/`layer2`/mixing-layer script objects and the backend passthrough
//! fields (`mode`, stream selection, color adjustment) the L2 model does not
//! carry — exactly the established per-family pattern
//! ([`CompatServices::state`]).
//!
//! # No decoder
//!
//! L3 does **not** link `media`/FFmpeg: decoded frames are supplied by the
//! host. A host installs a [`VideoSource`] through
//! [`CompatHost::set_video_source`](crate::CompatHost::set_video_source) (a
//! pre-decoded buffer) or through the crate-root [`set_video_source`] (a
//! decoder-backed source opened per path). `open(path)` asks that source for
//! the spec and, for the decoded audio track, raises a TJS error when the host
//! has none; it never fabricates metadata. The frame body's [`video_advance`]
//! step advances every open overlay by its own per-frame `dt`, presents the
//! host frame at the current position and syncs the decoded audio's mixer
//! voice with the overlay's position and transport state.
//!
//! # Events
//!
//! [`VideoOverlay::poll`] events are mapped to the script members
//! `onStatusChanged`/`onPeriod`/`onFrameUpdate`; the `onCallbackCommand` hook
//! is registered (the reference fires it from host decoder callbacks). A
//! `TransitionComplete` event has no registered callback (the reference
//! `setTransitionCompleteCall` is not part of the 70-member surface), so it is
//! dropped.
//!
//! [`CompatServices::state`]: crate::CompatServices::state
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};
use std::sync::Arc;

use crate::engine_api::Scene;
use crate::engine_api::{
    PlayStatus, VideoBounds, VideoEvent, VideoOverlay, blit_frame_to_layer_opacity,
};
use crate::engine_api::{SoundError, SoundSource};
use api::error::MediaError;
use api::media::{DecodedAudio, RgbaFrame, VideoSpec};
use tjs::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef,
    RetainedValue, Tjs2Engine, Tjs2ValueId, TjsValue, VAL_INTEGER, VAL_OBJECT, VAL_REAL,
    VAL_RETAINED, VAL_STRING, Value,
};

use crate::collections::Map;
use crate::native_util::{
    args, report_error, services, set_bool, set_int, set_void, value_as_string, value_ref,
};
use crate::natives::sound::{SoundState, TVP_VOLUME_SCALE};
use crate::natives::system::{frame_clock_f64, resolve, value_as_bool, value_as_i64};
use crate::storage::strip_file_media;
use crate::{CompatCtx, CompatError, CompatServices};

// ---------------------------------------------------------------------------
// Host seam: decoded video data
// ---------------------------------------------------------------------------

/// Host-supplied decoded video data.
///
/// L3 owns no decoder: the host probes the container with L1 `media`, decodes
/// frames and hands both to the overlay through this trait. It is installed
/// with [`CompatHost::set_video_source`](crate::CompatHost::set_video_source);
/// `open(path)` asks for the spec and the frame body asks for the frame at the
/// current position.
pub trait VideoSource {
    /// The decoded stream description for `path`, or `None` when the host has
    /// not decoded that file.
    fn spec(&self, path: &str) -> Option<VideoSpec>;

    /// The decoded frame at `at_ms` for `path`, or `None` when the host has no
    /// frame for that position.
    ///
    /// The frame is shared with the decoder, so the caller does not copy its
    /// pixels.
    fn frame_at(&mut self, path: &str, at_ms: i64) -> Option<Arc<RgbaFrame>>;

    /// The decoded audio track for `path`.
    ///
    /// `Ok(None)` reports that the stream has no audio track; `Err` is a
    /// decode failure (surfaced as a TJS error, never silence). The track is
    /// shared, so a source that decodes it once hands the same buffer to
    /// every caller. The default reports no audio, so a source that only
    /// serves video compiles unchanged.
    fn audio(&mut self, _path: &str) -> Result<Option<Arc<DecodedAudio>>, MediaError> {
        Ok(None)
    }
}

/// Ask a host [`VideoSource`] for a movie's decoded audio track.
///
/// `audio_streams == 0` means the container has no audio stream, so the host
/// is not asked at all. `Ok(None)` reports that the stream has no audio track;
/// `Err` is a real decode failure and is propagated to the caller, never
/// replaced with silence.
///
/// This is the one place both movie natives query the audio seam, so the
/// "does the container carry audio" decision is not duplicated.
pub(crate) fn decoded_audio(
    source: &mut dyn VideoSource,
    path: &str,
    audio_streams: u32,
) -> Result<Option<Arc<DecodedAudio>>, MediaError> {
    if audio_streams == 0 {
        return Ok(None);
    }
    source.audio(path)
}

/// The default host source: a spec plus the pre-decoded frame buffer the host
/// registered. `frame_at` picks the last frame whose timestamp is at or before
/// the requested position (the first frame before the first timestamp).
struct BufferedSource {
    spec: VideoSpec,
    frames: Vec<RgbaFrame>,
}

impl VideoSource for BufferedSource {
    fn spec(&self, _path: &str) -> Option<VideoSpec> {
        Some(self.spec)
    }

    fn frame_at(&mut self, _path: &str, at_ms: i64) -> Option<Arc<RgbaFrame>> {
        let mut chosen = self.frames.first()?;
        for frame in &self.frames {
            if frame.pts_ms as i64 <= at_ms {
                chosen = frame;
            } else {
                break;
            }
        }
        Some(Arc::new(chosen.clone()))
    }
}

/// Install the host-decoded source. Called by
/// [`CompatHost::set_video_source`](crate::CompatHost::set_video_source).
pub(crate) fn set_source(services: &CompatServices, spec: VideoSpec, frames: Vec<RgbaFrame>) {
    let mut state = services.state::<VideoState>();
    state.source = Some(Box::new(BufferedSource { spec, frames }));
}

/// Install an arbitrary decoded [`VideoSource`] for `VideoOverlay`.
///
/// This is the boxed-source analogue of
/// [`CompatHost::set_video_source`](crate::CompatHost::set_video_source): the
/// composition root installs a decoder-backed source that opens each path on
/// demand. `open` raises when no source has been installed.
///
/// The source is owned by the host's dispatch state and lives as long as the
/// host does; it is called from the VM's single thread.
pub fn set_video_source(ctx: &CompatCtx, source: Box<dyn VideoSource>) {
    // SAFETY: the caller holds a live, bound context.
    let services = unsafe { ctx.services() };
    services.state::<VideoState>().source = Some(source);
}

// ---------------------------------------------------------------------------
// Per-family dispatch state (never a process global)
// ---------------------------------------------------------------------------

/// One overlay's L2 playback value plus the L3 dispatch fields the L2 model
/// does not carry.
struct VideoSlot {
    /// The L2 playback machine (position, status, bounds, events, ...).
    overlay: VideoOverlay,
    /// The path the host source was opened with (empty before `open`).
    path: String,
    /// The script object this payload backs (event dispatch target).
    objthis: *mut c_void,
    /// `layer1`/`layer2` assignment (reference `Layer1`/`Layer2`).
    layer1: Option<LayerRef>,
    layer2: Option<LayerRef>,
    /// `setMixingLayer` state (reference `vomMixer` mixing bitmap).
    mixing_layer: Option<LayerRef>,
    /// The mixing layer's opacity in `0.0..=1.0` (reference `opacity / 255`).
    mixing_layer_alpha: f64,
    /// `mode` (`tTVPVideoOverlayMode`; reference default `vomOverlay`).
    mode: i64,
    /// Audio balance/volume in the TVP `-100000..=100000` scale, as the
    /// device stores them (`reference/cpp/core/movie/ffmpeg/KRMoviePlayer.cpp:93-117`).
    audio_balance: i64,
    audio_volume: i64,
    /// The decoder's selected audio/video stream index (`-1` for none).
    enabled_audio_stream: i64,
    enabled_video_stream: i64,
    /// Stream counts from the host [`VideoSpec`].
    number_of_audio_stream: i64,
    number_of_video_stream: i64,
    /// The decoded movie audio and its shared-mixer voice, once `open` has
    /// installed it.
    movie_audio: Option<MovieAudio>,
}

impl VideoSlot {
    fn new() -> Self {
        Self {
            overlay: VideoOverlay::new(),
            path: String::new(),
            objthis: std::ptr::null_mut(),
            layer1: None,
            layer2: None,
            mixing_layer: None,
            mixing_layer_alpha: 1.0,
            mode: 0,
            audio_balance: 0,
            audio_volume: 0,
            enabled_audio_stream: 0,
            enabled_video_stream: 0,
            number_of_audio_stream: 0,
            number_of_video_stream: 0,
            movie_audio: None,
        }
    }

    /// Push this slot's script volume/balance to the mixer voice.
    fn apply_audio(&self, services: &CompatServices) {
        if let Some(audio) = &self.movie_audio {
            audio.apply(
                services,
                SoundState::script_volume_to_gain(self.audio_volume),
                SoundState::script_pan_to_balance(self.audio_balance),
            );
        }
    }

    /// Sync the mixer voice with the overlay's current playback state.
    fn sync_audio(&self, services: &CompatServices, resync: Resync) {
        if let Some(audio) = &self.movie_audio {
            audio.sync(
                services,
                &self.overlay,
                SoundState::script_volume_to_gain(self.audio_volume),
                SoundState::script_pan_to_balance(self.audio_balance),
                resync,
            );
        }
    }

    /// Put the picture on the sound's position.
    fn follow_audio(&mut self, services: &CompatServices) {
        let Some(audio) = self.movie_audio.as_mut() else {
            return;
        };
        audio.follow_picture(services, &mut self.overlay);
    }

    /// Drop the mixer voice, if any.
    fn release_audio(&mut self, services: &CompatServices) {
        if let Some(audio) = self.movie_audio.take() {
            audio.detach(services);
        }
    }
}

/// The decoded movie audio and the shared-mixer voice it plays on.
///
/// This is the one implementation shared by every native that plays decoded
/// movie audio. The sound family owns the one `engine_api::Mixer`, so the
/// value registers the decoded track through [`SoundState`] and keeps only the
/// voice id; the owner composes it and drives [`Self::sync`] from its own
/// transport. A caller with script volume/pan members passes their normalized
/// values to [`Self::sync`]; a caller with none passes the device defaults
/// (unity gain, centre pan), which is what `layerExMovie` uses.
///
/// # The voice's clock
///
/// The voice keeps the mixer's own clock: the mixer advances it by the frames
/// the audio device actually rendered, so the sound it plays is continuous.
/// The picture never overwrites that clock frame by frame — placing the voice
/// at the picture's position on every frame cuts the waveform at every frame
/// — so the voice is put back onto the picture's position only on an explicit
/// transport command or when the two clocks have really diverged.
pub(crate) struct MovieAudio {
    source: Arc<SoundSource>,
    channel: u64,
    /// The position the voice reached at the last poll, so a sound that did
    /// not move can be told from one that did.
    last_played_ms: Option<f64>,
}

/// When a movie's mixer voice is put back onto the picture's position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Resync {
    /// Reposition the voice now: a transport command (open, play, pause,
    /// stop, rewind, seek, prepare) or a loop restart.
    Force,
    /// Reposition the voice only when it has drifted further than
    /// [`MOVIE_DRIFT_SECS`] from the picture's position.
    OnDrift,
}

/// The most the mixer voice may drift from the picture's position before the
/// per-frame poll corrects it, in seconds.
///
/// The drift is the difference between the position the audio device has
/// rendered and the picture's position. The device's pre-fill keeps the
/// rendered position ahead of the audible one by a fixed amount, so the
/// threshold sits well above it: a threshold the pre-fill could reach would
/// correct the voice on every frame, which is the cut this policy exists to
/// avoid.
const MOVIE_DRIFT_SECS: f64 = 0.25;

impl MovieAudio {
    /// Wrap `decoded` in an L2 [`SoundSource`] and register it on the shared
    /// mixer as a foreign voice (the track is not a script `SoundBuffer`).
    pub(crate) fn attach(
        services: &CompatServices,
        decoded: Arc<DecodedAudio>,
    ) -> Result<Self, SoundError> {
        let source = Arc::new(SoundSource::new(decoded)?);
        let mut sound = services.state::<SoundState>();
        let channel = sound.add_foreign_voice(source.clone())?;
        Ok(Self {
            source,
            channel,
            last_played_ms: None,
        })
    }

    /// Remove the mixer voice.
    pub(crate) fn detach(&self, services: &CompatServices) {
        services
            .state::<SoundState>()
            .remove_foreign_voice(self.channel);
    }

    /// Push only the volume/pan (no playback-state change).
    fn apply(&self, services: &CompatServices, volume: f32, pan: f32) {
        let mut sound = services.state::<SoundState>();
        if let Some(channel) = sound.foreign_voice_mut(self.channel) {
            channel.set_volume(volume);
            channel.set_pan(pan);
        }
    }

    /// Push the volume/pan and the overlay's playback state, and place the
    /// voice on the overlay's position as `resync` asks.
    ///
    /// A voice that ran past the end of its track is not started again by an
    /// ordinary frame: only a transport command or a loop restart does that.
    pub(crate) fn sync(
        &self,
        services: &CompatServices,
        overlay: &VideoOverlay,
        volume: f32,
        pan: f32,
        resync: Resync,
    ) {
        let mut sound = services.state::<SoundState>();
        let Some(channel) = sound.foreign_voice_mut(self.channel) else {
            return;
        };
        channel.set_volume(volume);
        channel.set_pan(pan);
        // The voice does not wrap on its own: the picture's loop restarts
        // it, so the sound and the picture turn over at the same frame. A
        // voice that wrapped at the end of its own track would restart the
        // sound before the picture reached its loop point.
        channel.looping = false;
        if !overlay.is_playing() {
            channel.pause();
            return;
        }
        if channel.is_paused() {
            channel.resume();
        }
        if !channel.is_playing() && !channel.is_done() {
            // The voice has no source yet.
            channel.play(self.source.clone());
        }
        let target = overlay.position_exact_ms() / 1000.0;
        if channel.is_done() {
            // The track ran to its end. Only an explicit command starts it
            // again; an ordinary frame leaves the finished track alone.
            if resync == Resync::Force {
                channel.play(self.source.clone());
                channel.set_position(target);
            }
            return;
        }
        let drift = channel.position_seconds() - target;
        let reposition = match resync {
            Resync::Force => true,
            Resync::OnDrift => drift.abs() > MOVIE_DRIFT_SECS,
        };
        if reposition {
            channel.set_position(target);
        }
    }

    /// The position the voice has reached at the listener, in milliseconds,
    /// when the voice is playing.
    fn played_position_ms(&self, services: &CompatServices) -> Option<f64> {
        let mut sound = services.state::<SoundState>();
        let channel = sound.foreign_voice_mut(self.channel)?;
        if !channel.is_playing() {
            return None;
        }
        Some(channel.played_position_seconds() * 1000.0)
    }

    /// Put the picture on the sound's position.
    ///
    /// The picture is scheduled against the audio device's clock: the
    /// player's clock is the audio clock plus the sound's delay, so the
    /// picture follows the sound instead of the sound following the picture.
    ///
    /// A sound that did not move — no device pumping it, a paused voice —
    /// leaves the picture on its own clock, and a loop restart leaves the
    /// sound on the picture's.
    pub(crate) fn follow_picture(&mut self, services: &CompatServices, overlay: &mut VideoOverlay) {
        let Some(played_ms) = self.played_position_ms(services) else {
            return;
        };
        let previous = self.last_played_ms.replace(played_ms);
        // The first poll only records where the sound is.
        let Some(previous) = previous else {
            return;
        };
        if played_ms == previous {
            // The sound did not move: the picture keeps its own clock.
            return;
        }
        if overlay.restarted_at_loop() {
            // A loop restart puts the sound back onto the picture's
            // position instead.
            return;
        }
        overlay.sync_clock(played_ms);
    }
}

/// A retained script `Layer` object plus its scene id.
struct LayerRef {
    /// Keeps the script object alive while the overlay references it.
    keepalive: DetachedValue,
    /// The scene layer id (the VM native-instance lookup of the `Layer`
    /// class), used for the L2 binding.
    id: u32,
}

/// The video family's dispatch state, keyed by type inside
/// [`CompatServices::state`](crate::CompatServices::state).
struct VideoState {
    /// Id → overlay state; ids are allocated by `create` and freed by
    /// `destroy`.
    overlays: Map<u32, VideoSlot>,
    /// The next object id.
    next_id: u32,
    /// The host-decoded source (installed by the host before `open`).
    source: Option<Box<dyn VideoSource>>,
    /// The engine clock's value at the last advance.
    last_advance_ms: f64,
}

impl Default for VideoState {
    fn default() -> Self {
        Self {
            // The overlays are walked to deliver their events, so the table's
            // order is fixed and the same in every run.
            overlays: Map::with_hasher(crate::collections::hasher()),
            next_id: 0,
            source: None,
            last_advance_ms: 0.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Native instance payload
// ---------------------------------------------------------------------------

/// `VideoOverlay` payload: the id of the [`VideoSlot`] in [`VideoState`].
#[derive(Default)]
struct VideoOverlayInst {
    id: Option<u32>,
}

// ---------------------------------------------------------------------------
// Small marshalling helpers
// ---------------------------------------------------------------------------

/// Resolve the installed engine → context binding for a native callback.
fn bound(
    engine: *mut c_void,
) -> Result<(crate::natives::system::EngineBinding, CompatCtx), String> {
    let resolved =
        resolve(engine).ok_or_else(|| "VideoOverlay: compat context is not bound".to_string())?;
    let ctx = resolved.ctx();
    Ok((resolved, ctx))
}

/// Write a real result into a native callback's out slot.
fn set_real(out: *mut Value, value: f64) {
    if out.is_null() {
        return;
    }
    // SAFETY: the trampoline passes a valid out slot for the call.
    unsafe {
        (*out).ty = VAL_REAL;
        (*out).integer = 0;
        (*out).real = value;
        (*out).string = std::ptr::null();
        (*out).array = std::ptr::null();
        (*out).array_count = 0;
        (*out).retained = 0;
    }
}

/// TJS `AsReal` coercion.
fn value_as_f64(v: &Value) -> f64 {
    match v.ty {
        VAL_INTEGER => v.integer as f64,
        VAL_REAL => v.real,
        VAL_STRING => crate::natives::system::tjs_string_to_real(&value_as_string(v)),
        _ => 0.0,
    }
}

/// Write a retained id into a native callback's out slot. The trampoline
/// consumes the retention.
fn set_retained_out(out: *mut Value, id: Tjs2ValueId) {
    if out.is_null() {
        return;
    }
    // SAFETY: the trampoline passes a valid out slot for the call.
    unsafe {
        (*out).ty = VAL_RETAINED;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = std::ptr::null();
        (*out).array = std::ptr::null();
        (*out).array_count = 0;
        (*out).retained = id.as_ptr() as usize;
    }
}

/// Write TJS `null` into `out` (reference `layer1`/`layer2` read back null).
fn set_null_out(host: &Tjs2Engine, out: *mut Value) {
    if out.is_null() {
        return;
    }
    if let Ok(RetainedValue::Object(object)) = host.eval_retained("null", "video.null") {
        set_retained_out(out, object.raw_id());
        std::mem::forget(object);
        return;
    }
    set_void(out);
}

/// Resolve a script object argument to a scene id. An object argument is
/// resolved through the VM's native-instance lookup for the `Window` class
/// (the owning-window argument of the constructor).
fn object_id(host: &Tjs2Engine, value: &Value) -> Result<i64, String> {
    CompatCtx::resolve_object_id(host, value, &["Window"])
}

/// Resolve a `layer1`/`layer2`/`setMixingLayer` argument to a [`LayerRef`].
///
/// A null/void/non-object argument clears the slot; any other object must be
/// a native `Layer`, resolved through the VM's native-instance lookup with no
/// member fallback. A non-Layer object raises `TVPSpecifyLayer`.
fn resolve_layer_ref(host: &Tjs2Engine, value: &Value) -> Result<Option<LayerRef>, String> {
    if value.ty != VAL_OBJECT || value.object_handle().is_null() {
        return Ok(None);
    }
    let keepalive = host
        .retain_object_arg(value)
        .map_err(|_| "VideoOverlay: specify layer".to_string())?;
    let id = CompatCtx::native_instance_id(host, keepalive.raw_id(), &["Layer"])
        .map_err(|error| format!("VideoOverlay: specify layer: {error}"))?
        .ok_or_else(|| "VideoOverlay: specify layer".to_string())?;
    Ok(Some(LayerRef { keepalive, id }))
}

/// Read a boolean member, defaulting to `true` when absent (the reference
/// `Layer.visible` default).
fn member_is_true(host: &Tjs2Engine, id: Tjs2ValueId, name: &str) -> bool {
    match host.member(id, name) {
        Ok(TjsValue::Integer(v)) => v != 0,
        Ok(TjsValue::Real(v)) => v != 0.0,
        _ => true,
    }
}

/// Read a numeric member, or `None` when it is absent.
fn member_as_f64(host: &Tjs2Engine, id: Tjs2ValueId, name: &str) -> Option<f64> {
    match host.member(id, name) {
        Ok(TjsValue::Integer(v)) => Some(v as f64),
        Ok(TjsValue::Real(v)) => Some(v),
        _ => None,
    }
}

/// Clear the mixing-layer binding, preserving the constructor window binding
/// (the L2 `unbind` clears both).
fn reset_mixing(slot: &mut VideoSlot) {
    let window = slot.overlay.binding().window;
    slot.overlay.unbind();
    if let Some(window) = window {
        slot.overlay.bind_window(window);
    }
    slot.mixing_layer = None;
    slot.mixing_layer_alpha = 1.0;
}

// ---------------------------------------------------------------------------
// Instance property accessor macros
// ---------------------------------------------------------------------------

/// Resolve the callback context and the object's [`VideoSlot`], then run
/// `$body` with `$slot` bound. On a missing binding/state it reports the TJS
/// error and returns.
macro_rules! with_slot {
    ($engine:expr, $instance:expr, $out_error:expr, $ctx:ident, $slot:ident, $body:block) => {{
        let Ok((_resolved, $ctx)) = bound($engine) else {
            return report_error($out_error, "VideoOverlay: compat context is not bound");
        };
        // SAFETY: `instance` is the live `VideoOverlayInst` payload the VM allocated for this script object; no `&mut` to it is live during this callback.
        let inst = unsafe { &*($instance as *const VideoOverlayInst) };
        let Some(id) = inst.id else {
            return report_error($out_error, "VideoOverlay: object is not constructed");
        };
        // SAFETY: `ctx` is only created for a bound, live engine, so its service pointer is valid for `reset_mixing`.
        let services = unsafe { $ctx.services() };
        let mut state = services.state::<VideoState>();
        let Some($slot) = state.overlays.get_mut(&id) else {
            return report_error($out_error, "VideoOverlay: object state is missing");
        };
        $body
    }};
}

/// Read/write backend-defined real metadata. The in-tree FFmpeg backend
/// leaves every colour getter at the wrapper's `-1.0` seed even while a stream
/// is open, and its setters are inert.
macro_rules! backend_stub_real_prop {
    ($get:ident, $set:ident) => {
        extern "C" fn $get(
            _engine: *mut c_void,
            _instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            set_real(out, -1.0);
            0
        }
        extern "C" fn $set(
            _engine: *mut c_void,
            _instance: *mut c_void,
            _value: *const Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            0
        }
    };
}

/// Read-only backend-defined real metadata that reports a fixed value with no
/// stream open.
macro_rules! backend_const_real_prop {
    ($get:ident) => {
        extern "C" fn $get(
            _engine: *mut c_void,
            _instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            set_real(out, -1.0);
            0
        }
    };
}

/// Read/write a backend-passthrough real with fixed open/closed values. The
/// in-tree FFmpeg backend's setters are stubs, so the setter is inert.
macro_rules! backend_open_real_prop {
    ($get:ident, $set:ident, $open_value:expr, $closed_value:expr) => {
        extern "C" fn $get(
            engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            with_slot!(engine, instance, out_error, _ctx, slot, {
                set_real(
                    out,
                    if slot.overlay.is_open() {
                        $open_value
                    } else {
                        $closed_value
                    },
                );
                0
            })
        }
        extern "C" fn $set(
            _engine: *mut c_void,
            _instance: *mut c_void,
            _value: *const Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            0
        }
    };
}

/// Read/write an integer backend-passthrough with fixed open/closed values. See
/// [`backend_open_real_prop!`].
macro_rules! backend_open_int_prop {
    ($get:ident, $set:ident, $open_value:expr, $closed_value:expr) => {
        extern "C" fn $get(
            engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            with_slot!(engine, instance, out_error, _ctx, slot, {
                set_int(
                    out,
                    if slot.overlay.is_open() {
                        $open_value
                    } else {
                        $closed_value
                    },
                );
                0
            })
        }
        extern "C" fn $set(
            _engine: *mut c_void,
            _instance: *mut c_void,
            _value: *const Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            0
        }
    };
}

/// Read-only integer [`VideoSlot`] field.
macro_rules! slot_ro_int {
    ($get:ident, $field:ident) => {
        extern "C" fn $get(
            engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            with_slot!(engine, instance, out_error, _ctx, slot, {
                set_int(out, slot.$field);
                0
            })
        }
    };
}

/// Run `command` against the overlay's slot, then return its dispatch target
/// and the status changes it produced. The caller fires those synchronously,
/// after this function has released the state borrow, matching the reference's
/// immediate `onStatusChanged`.
///
/// A command failure is reported through `out_error` and yields `None`.
fn run_command(
    engine: *mut c_void,
    instance: *mut c_void,
    out_error: *mut *mut c_char,
    command: impl FnOnce(&mut VideoSlot) -> Result<(), String>,
) -> Option<(*mut c_void, Vec<PlayStatus>)> {
    let Ok((_resolved, ctx)) = bound(engine) else {
        report_error(out_error, "VideoOverlay: compat context is not bound");
        return None;
    };
    // SAFETY: `instance` is the live `VideoOverlayInst` payload the VM
    // allocated for this script object; no `&mut` to it is live here.
    let inst = unsafe { &*(instance as *const VideoOverlayInst) };
    let Some(id) = inst.id else {
        report_error(out_error, "VideoOverlay: object is not constructed");
        return None;
    };
    // SAFETY: `ctx` is only created for a bound, live engine, so its service
    // pointer is valid for this command.
    let services = unsafe { ctx.services() };
    let mut state = services.state::<VideoState>();
    let Some(slot) = state.overlays.get_mut(&id) else {
        report_error(out_error, "VideoOverlay: object state is missing");
        return None;
    };
    if let Err(message) = command(slot) {
        drop(state);
        report_error(out_error, &message);
        return None;
    }
    let objthis = slot.objthis;
    let statuses = slot.overlay.take_status_events();
    Some((objthis, statuses))
}

/// Run `command` with the overlay's slot and the service set, reporting any
/// failure through `out_error`.
///
/// Unlike [`run_command`], this does not drain status changes: it is for the
/// audio/stream setters, whose only script-visible effect is reported by their
/// getters.
fn run_slot(
    engine: *mut c_void,
    instance: *mut c_void,
    out_error: *mut *mut c_char,
    what: &str,
    command: impl FnOnce(&CompatServices, &mut VideoSlot) -> Result<(), String>,
) -> Result<(), c_int> {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return Err(report_error(
            out_error,
            &format!("VideoOverlay.{what}: compat context is not bound"),
        ));
    };
    // SAFETY: `instance` is the live `VideoOverlayInst` payload the VM
    // allocated for this script object; no `&mut` to it is live here.
    let inst = unsafe { &*(instance as *const VideoOverlayInst) };
    let Some(id) = inst.id else {
        return Err(report_error(
            out_error,
            &format!("VideoOverlay.{what}: object is not constructed"),
        ));
    };
    // SAFETY: `ctx` is only created for a bound, live engine, so its service
    // pointer is valid for this command.
    let services = unsafe { ctx.services() };
    let mut state = services.state::<VideoState>();
    let Some(slot) = state.overlays.get_mut(&id) else {
        return Err(report_error(
            out_error,
            &format!("VideoOverlay.{what}: object state is missing"),
        ));
    };
    command(services, slot).map_err(|message| report_error(out_error, &message))
}

/// Apply the reference's `SelectAudioStream(index)`.
///
/// `index` is the 0-based position among the container's audio streams. An
/// out-of-range index is ignored: the reference's stream lookup returns the
/// invalid stream and the caller skips the switch
/// (`reference/cpp/core/movie/ffmpeg/VideoPlayer.cpp:1346-1356`). This decoder
/// opens a single audio stream, so a different in-range index is a typed error
/// rather than a silent lie.
fn select_audio_stream(slot: &mut VideoSlot, stream: i64) -> Result<(), String> {
    let count = slot.overlay.spec().map_or(0, |spec| spec.audio_streams);
    let in_range = u32::try_from(stream).is_ok_and(|index| index < count);
    if !in_range || slot.enabled_audio_stream == stream {
        return Ok(());
    }
    Err("VideoOverlay.selectAudioStream: this decoder decodes a single audio stream".to_string())
}

/// Apply the reference's `SelectVideoStream(index)`; see
/// [`select_audio_stream`].
fn select_video_stream(slot: &mut VideoSlot, stream: i64) -> Result<(), String> {
    let count = slot.overlay.spec().map_or(0, |spec| spec.video_streams);
    let in_range = u32::try_from(stream).is_ok_and(|index| index < count);
    if !in_range || slot.enabled_video_stream == stream {
        return Ok(());
    }
    Err("VideoOverlay.selectVideoStream: this decoder decodes a single video stream".to_string())
}

/// Fire `onStatusChanged` for each status change, synchronously.
fn dispatch_statuses(engine: *mut c_void, objthis: *mut c_void, statuses: &[PlayStatus]) {
    if objthis.is_null() || statuses.is_empty() {
        return;
    }
    let Ok((resolved, _ctx)) = bound(engine) else {
        return;
    };
    let events: Vec<VideoEvent> = statuses
        .iter()
        .copied()
        .map(VideoEvent::StatusChanged)
        .collect();
    dispatch_events(resolved.engine(), objthis, &events);
}

// ---------------------------------------------------------------------------
// Playback commands
// ---------------------------------------------------------------------------

extern "C" fn video_create(engine: *mut c_void) -> *mut c_void {
    let mut inst = Box::new(VideoOverlayInst::default());
    if let Ok((_resolved, ctx)) = bound(engine) {
        // SAFETY: `ctx` is only created for a bound, live engine, so its service pointer is valid for `video_create`.
        let services = unsafe { ctx.services() };
        let mut state = services.state::<VideoState>();
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        state.overlays.insert(id, VideoSlot::new());
        inst.id = Some(id);
    }
    Box::into_raw(inst) as *mut c_void
}

extern "C" fn video_destroy(engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `instance` is the live `VideoOverlayInst` payload the VM allocated for this script object; no `&mut` to it is live in `video_destroy`.
    let inst = unsafe { &*(instance as *const VideoOverlayInst) };
    if let (Some(id), Ok((_resolved, ctx))) = (inst.id, bound(engine)) {
        // Remove the slot while the engine is alive: dropping it releases the
        // retained `layer1`/`layer2` objects, and `release_audio` removes the
        // movie-audio mixer voice.
        // SAFETY: `ctx` is only created for a bound, live engine, so its service pointer is valid for `video_destroy`.
        let services = unsafe { ctx.services() };
        let mut state = services.state::<VideoState>();
        if let Some(mut slot) = state.overlays.remove(&id) {
            slot.release_audio(services);
        }
    }
    // SAFETY: instance came from `video_create`'s `Box::into_raw` and is freed
    // exactly once by the VM.
    drop(unsafe { Box::from_raw(instance as *mut VideoOverlayInst) });
}

/// `VideoOverlay(win)` — the constructor hook. The reference `Construct`
/// requires a live `Window` argument: a missing
/// argument is a bad parameter count and a null/non-`Window` object raises.
extern "C" fn video_ctor(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "VideoOverlay: compat context is not bound");
    };
    // SAFETY: `instance` is the live `VideoOverlayInst` payload the VM allocated for this script object; no `&mut` to it is live in `video_ctor`.
    let inst = unsafe { &*(instance as *const VideoOverlayInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "VideoOverlay: object is not constructed");
    };
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let values = unsafe { args(argv, argc) };
    let Some(value) = values.first() else {
        return report_error(out_error, "VideoOverlay: a Window argument is required");
    };
    if value.ty != VAL_OBJECT || value.object_handle().is_null() {
        return report_error(out_error, "VideoOverlay: specify a Window");
    }
    let window = match object_id(resolved.engine(), value) {
        Ok(id) if id >= 0 => id as u32,
        Ok(_) => return report_error(out_error, "VideoOverlay: specify a Window"),
        Err(error) => return report_error(out_error, &format!("VideoOverlay: {error}")),
    };
    // SAFETY: `ctx` is only created for a bound, live engine, so its service pointer is valid for `video_ctor`.
    let services = unsafe { ctx.services() };
    let mut state = services.state::<VideoState>();
    let Some(slot) = state.overlays.get_mut(&id) else {
        return report_error(out_error, "VideoOverlay: object state is missing");
    };
    slot.objthis = objthis;
    slot.overlay.bind_window(window);
    drop(state);
    set_void(out);
    0
}

/// `open(file)` — open the host-decoded source for `file`.
extern "C" fn vo_open(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "VideoOverlay.open: compat context is not bound");
    };
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let values = unsafe { args(argv, argc) };
    let Some(path_value) = values.first() else {
        return report_error(out_error, "VideoOverlay.open requires 1 argument");
    };
    let path = value_as_string(path_value);
    // The reference `tTJSNI_VideoOverlay::Open` opens the storage through
    // `TVPCreateStream` (`reference/cpp/core/visual/impl/VideoOvlImpl.cpp:132-160`),
    // which normalizes the media name; the L2 namespace takes the bare name.
    // The script spelling stays in the diagnostics.
    let lookup = strip_file_media(&path);
    // SAFETY: `instance` is the live `VideoOverlayInst` payload the VM allocated for this script object; no `&mut` to it is live in `vo_open`.
    let inst = unsafe { &*(instance as *const VideoOverlayInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "VideoOverlay.open: object is not constructed");
    };
    // SAFETY: `ctx` is only created for a bound, live engine, so its service pointer is valid for `vo_open`.
    let services = unsafe { ctx.services() };
    // Ask the host for the spec and the decoded audio without holding the
    // state borrow.
    let (spec, audio) = {
        let mut state = services.state::<VideoState>();
        let Some(source) = state.source.as_mut() else {
            return report_error(
                out_error,
                &format!("VideoOverlay.open: no decoded source for {path:?}"),
            );
        };
        let Some(spec) = source.spec(lookup) else {
            return report_error(
                out_error,
                &format!("VideoOverlay.open: no decoded source for {path:?}"),
            );
        };
        let audio = match decoded_audio(source.as_mut(), lookup, spec.audio_streams) {
            Ok(audio) => audio,
            Err(error) => {
                return report_error(out_error, &format!("VideoOverlay.open: {error}"));
            }
        };
        (spec, audio)
    };
    let mut state = services.state::<VideoState>();
    let Some(slot) = state.overlays.get_mut(&id) else {
        return report_error(out_error, "VideoOverlay.open: object state is missing");
    };
    slot.release_audio(services);
    if let Err(error) = slot.overlay.open(spec) {
        return report_error(out_error, &format!("VideoOverlay.open: {error}"));
    }
    slot.path = lookup.to_string();
    slot.number_of_audio_stream = i64::from(spec.audio_streams);
    slot.number_of_video_stream = i64::from(spec.video_streams);
    slot.enabled_audio_stream = if spec.audio_streams > 0 {
        i64::from(spec.audio_stream)
    } else {
        -1
    };
    slot.enabled_video_stream = if spec.video_streams > 0 {
        i64::from(spec.video_stream)
    } else {
        -1
    };
    // A freshly created audio device starts at unity volume and centre pan
    // (`reference/cpp/core/sound/win32/WaveMixer.cpp:124-126`).
    if spec.audio_streams > 0 {
        slot.audio_volume = TVP_VOLUME_SCALE as i64;
        slot.audio_balance = 0;
    } else {
        slot.audio_volume = 0;
        slot.audio_balance = 0;
    }
    if let Some(decoded) = audio {
        match MovieAudio::attach(services, decoded) {
            Ok(movie_audio) => {
                slot.movie_audio = Some(movie_audio);
                slot.sync_audio(services, Resync::Force);
            }
            Err(error) => {
                slot.overlay.close();
                slot.path.clear();
                slot.number_of_audio_stream = 0;
                slot.number_of_video_stream = 0;
                slot.enabled_audio_stream = -1;
                slot.enabled_video_stream = -1;
                slot.audio_volume = 0;
                slot.audio_balance = 0;
                return report_error(
                    out_error,
                    &format!("VideoOverlay.open: cannot play movie audio: {error}"),
                );
            }
        }
    }
    let objthis = slot.objthis;
    let statuses = slot.overlay.take_status_events();
    drop(state);
    dispatch_statuses(engine, objthis, &statuses);
    set_void(out);
    0
}

/// `close()` — drop the media and return to `unload`.
extern "C" fn vo_close(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "VideoOverlay.close: compat context is not bound");
    };
    let services = services(&ctx);
    let Some((objthis, statuses)) = run_command(engine, instance, out_error, |slot| {
        slot.overlay.close();
        slot.release_audio(services);
        slot.path.clear();
        slot.number_of_audio_stream = 0;
        slot.number_of_video_stream = 0;
        slot.enabled_audio_stream = -1;
        slot.enabled_video_stream = -1;
        slot.audio_volume = 0;
        slot.audio_balance = 0;
        Ok(())
    }) else {
        return 1;
    };
    dispatch_statuses(engine, objthis, &statuses);
    set_void(out);
    0
}

/// `play()`.
extern "C" fn vo_play(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "VideoOverlay.play: compat context is not bound");
    };
    let services = services(&ctx);
    let Some((objthis, statuses)) = run_command(engine, instance, out_error, |slot| {
        if slot.overlay.is_open() {
            slot.overlay
                .play()
                .map_err(|error| format!("VideoOverlay.play: {error}"))?;
            slot.sync_audio(services, Resync::Force);
        }
        Ok(())
    }) else {
        return 1;
    };
    dispatch_statuses(engine, objthis, &statuses);
    set_void(out);
    0
}

/// `pause()`.
extern "C" fn vo_pause(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "VideoOverlay.pause: compat context is not bound");
    };
    let services = services(&ctx);
    let Some((objthis, statuses)) = run_command(engine, instance, out_error, |slot| {
        if slot.overlay.is_open() {
            slot.overlay
                .pause()
                .map_err(|error| format!("VideoOverlay.pause: {error}"))?;
            slot.sync_audio(services, Resync::Force);
        }
        Ok(())
    }) else {
        return 1;
    };
    dispatch_statuses(engine, objthis, &statuses);
    set_void(out);
    0
}

/// `stop()`.
extern "C" fn vo_stop(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "VideoOverlay.stop: compat context is not bound");
    };
    let services = services(&ctx);
    let Some((objthis, statuses)) = run_command(engine, instance, out_error, |slot| {
        if slot.overlay.is_open() {
            slot.overlay
                .stop()
                .map_err(|error| format!("VideoOverlay.stop: {error}"))?;
            slot.sync_audio(services, Resync::Force);
        }
        Ok(())
    }) else {
        return 1;
    };
    dispatch_statuses(engine, objthis, &statuses);
    set_void(out);
    0
}

/// `rewind()`.
extern "C" fn vo_rewind(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "VideoOverlay.rewind: compat context is not bound",
        );
    };
    let services = services(&ctx);
    let Some((objthis, statuses)) = run_command(engine, instance, out_error, |slot| {
        slot.overlay.rewind();
        slot.sync_audio(services, Resync::Force);
        Ok(())
    }) else {
        return 1;
    };
    dispatch_statuses(engine, objthis, &statuses);
    set_void(out);
    0
}

/// `prepare()`. The reference acts only in `vomLayer` mode.
extern "C" fn vo_prepare(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "VideoOverlay.prepare: compat context is not bound",
        );
    };
    let services = services(&ctx);
    let Some((objthis, statuses)) = run_command(engine, instance, out_error, |slot| {
        if slot.overlay.is_open() && slot.mode == MODE_LAYER {
            slot.overlay
                .prepare()
                .map_err(|error| format!("VideoOverlay.prepare: {error}"))?;
            slot.sync_audio(services, Resync::Force);
        }
        Ok(())
    }) else {
        return 1;
    };
    dispatch_statuses(engine, objthis, &statuses);
    set_void(out);
    0
}

/// `setPos(left, top)`.
extern "C" fn vo_set_pos(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let values = unsafe { args(argv, argc) };
    if values.len() < 2 {
        return report_error(out_error, "VideoOverlay.setPos requires 2 arguments");
    }
    let left = value_as_i64(&values[0]) as i32;
    let top = value_as_i64(&values[1]) as i32;
    with_slot!(engine, instance, out_error, _ctx, slot, {
        if slot.mode == MODE_LAYER {
            if let Ok(scene) = scene_mut(&_ctx) {
                for layer in [slot.layer1.as_ref(), slot.layer2.as_ref()]
                    .into_iter()
                    .flatten()
                {
                    if let Some(state) = scene.layer_mut(layer.id) {
                        state.rect.x = left;
                        state.rect.y = top;
                    }
                }
            }
        } else {
            slot.overlay.set_position(left, top);
        }
        set_void(out);
        0
    })
}

/// `setSize(width, height)`.
extern "C" fn vo_set_size(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let values = unsafe { args(argv, argc) };
    if values.len() < 2 {
        return report_error(out_error, "VideoOverlay.setSize requires 2 arguments");
    }
    let width = value_as_i64(&values[0]).max(0) as u32;
    let height = value_as_i64(&values[1]).max(0) as u32;
    with_slot!(engine, instance, out_error, _ctx, slot, {
        if slot.mode != MODE_LAYER {
            slot.overlay.set_size(width, height);
        }
        set_void(out);
        0
    })
}

/// `setBounds(left, top, width, height)`.
extern "C" fn vo_set_bounds(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let values = unsafe { args(argv, argc) };
    if values.len() < 4 {
        return report_error(out_error, "VideoOverlay.setBounds requires 4 arguments");
    }
    let bounds = VideoBounds::new(
        value_as_i64(&values[0]) as i32,
        value_as_i64(&values[1]) as i32,
        value_as_i64(&values[2]).max(0) as u32,
        value_as_i64(&values[3]).max(0) as u32,
    );
    with_slot!(engine, instance, out_error, _ctx, slot, {
        if slot.mode != MODE_LAYER {
            slot.overlay.bounds = bounds;
        }
        set_void(out);
        0
    })
}

/// `setSegmentLoop(startFrame, endFrame)`.
extern "C" fn vo_set_segment_loop(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let values = unsafe { args(argv, argc) };
    if values.len() < 2 {
        return report_error(
            out_error,
            "VideoOverlay.setSegmentLoop requires 2 arguments",
        );
    }
    let start = value_as_i64(&values[0]);
    let end = value_as_i64(&values[1]);
    with_slot!(engine, instance, out_error, _ctx, slot, {
        slot.overlay.set_segment_loop(start, end);
        set_void(out);
        0
    })
}

/// `cancelSegmentLoop()`.
extern "C" fn vo_cancel_segment_loop(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        slot.overlay.cancel_segment_loop();
        set_void(out);
        0
    })
}

/// `setPeriodEvent([frame])` — no argument clears it (reference).
extern "C" fn vo_set_period_event(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let values = unsafe { args(argv, argc) };
    let frame = match values.len() {
        0 => -1,
        1 => value_as_i64(&values[0]),
        // The reference ignores 2+ arguments entirely.
        _ => {
            set_void(out);
            return 0;
        }
    };
    with_slot!(engine, instance, out_error, _ctx, slot, {
        slot.overlay.set_period_event(frame);
        set_void(out);
        0
    })
}

/// `cancelPeriodEvent()`.
extern "C" fn vo_cancel_period_event(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        slot.overlay.cancel_period_event();
        set_void(out);
        0
    })
}

/// `setMixingLayer(layer)` — a null or non-visible layer resets the mixing
/// bitmap (reference `tTJSNI_VideoOverlay::SetMixingLayer`).
extern "C" fn vo_set_mixing_layer(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "VideoOverlay.setMixingLayer: compat context is not bound",
        );
    };
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let values = unsafe { args(argv, argc) };
    let Some(value) = values.first() else {
        return report_error(out_error, "VideoOverlay.setMixingLayer requires 1 argument");
    };
    let layer = match resolve_layer_ref(resolved.engine(), value) {
        Ok(layer) => layer,
        Err(error) => return report_error(out_error, &error),
    };
    // A visible layer contributes its opacity; anything else resets.
    let stored = match layer {
        Some(layer) if member_is_true(resolved.engine(), layer.keepalive.raw_id(), "visible") => {
            let opacity = member_as_f64(resolved.engine(), layer.keepalive.raw_id(), "opacity")
                .unwrap_or(255.0);
            Some((layer, (opacity / 255.0).clamp(0.0, 1.0)))
        }
        _ => None,
    };
    // SAFETY: `instance` is the live `VideoOverlayInst` payload the VM allocated for this script object; no `&mut` to it is live in `vo_set_mixing_layer`.
    let inst = unsafe { &*(instance as *const VideoOverlayInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "VideoOverlay.setMixingLayer: object is not constructed",
        );
    };
    // SAFETY: `ctx` is only created for a bound, live engine, so its service pointer is valid for `vo_set_mixing_layer`.
    let services = unsafe { ctx.services() };
    let mut state = services.state::<VideoState>();
    let Some(slot) = state.overlays.get_mut(&id) else {
        return report_error(
            out_error,
            "VideoOverlay.setMixingLayer: object state is missing",
        );
    };
    match stored {
        Some((layer, alpha)) => {
            slot.overlay.bind_layer(layer.id);
            slot.overlay.set_binding_alpha(alpha as f32);
            slot.mixing_layer = Some(layer);
            slot.mixing_layer_alpha = alpha;
        }
        None => reset_mixing(slot),
    }
    drop(state);
    set_void(out);
    0
}

/// `resetMixingLayer()`.
extern "C" fn vo_reset_mixing_layer(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        reset_mixing(slot);
        set_void(out);
        0
    })
}

/// `selectAudioStream(n)`.
extern "C" fn vo_select_audio_stream(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let values = unsafe { args(argv, argc) };
    if values.is_empty() {
        return report_error(
            out_error,
            "VideoOverlay.selectAudioStream requires 1 argument",
        );
    }
    let stream = value_as_i64(&values[0]);
    match run_slot(
        engine,
        instance,
        out_error,
        "selectAudioStream",
        |_services, slot| {
            if slot.overlay.is_open() {
                select_audio_stream(slot, stream)?;
            }
            Ok(())
        },
    ) {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(code) => code,
    }
}

// -- overridable event hooks -------------------------------------------------

extern "C" fn vo_on_status_changed(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void(out);
    0
}

extern "C" fn vo_on_callback_command(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void(out);
    0
}

extern "C" fn vo_on_period(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void(out);
    0
}

extern "C" fn vo_on_frame_update(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

// -- rectangle / visibility -------------------------------------------------

extern "C" fn vo_left_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(out, i64::from(slot.overlay.bounds.left));
        0
    })
}

extern "C" fn vo_left_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
        let left = value_as_i64(unsafe { &*value }) as i32;
        if slot.mode == MODE_LAYER {
            if let Ok(scene) = scene_mut(&_ctx) {
                for layer in [slot.layer1.as_ref(), slot.layer2.as_ref()]
                    .into_iter()
                    .flatten()
                {
                    if let Some(state) = scene.layer_mut(layer.id) {
                        state.rect.x = left;
                    }
                }
            }
        } else {
            let top = slot.overlay.bounds.top;
            slot.overlay.set_position(left, top);
        }
        0
    })
}

extern "C" fn vo_top_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(out, i64::from(slot.overlay.bounds.top));
        0
    })
}

extern "C" fn vo_top_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
        let top = value_as_i64(unsafe { &*value }) as i32;
        if slot.mode == MODE_LAYER {
            if let Ok(scene) = scene_mut(&_ctx) {
                for layer in [slot.layer1.as_ref(), slot.layer2.as_ref()]
                    .into_iter()
                    .flatten()
                {
                    if let Some(state) = scene.layer_mut(layer.id) {
                        state.rect.y = top;
                    }
                }
            }
        } else {
            let left = slot.overlay.bounds.left;
            slot.overlay.set_position(left, top);
        }
        0
    })
}

extern "C" fn vo_width_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(out, i64::from(slot.overlay.bounds.width));
        0
    })
}

extern "C" fn vo_width_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        if slot.mode != MODE_LAYER {
            let height = slot.overlay.bounds.height;
            slot.overlay
                .set_size(value_as_i64(value_ref(value)).max(0) as u32, height);
        }
        0
    })
}

extern "C" fn vo_height_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(out, i64::from(slot.overlay.bounds.height));
        0
    })
}

extern "C" fn vo_height_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        if slot.mode != MODE_LAYER {
            let width = slot.overlay.bounds.width;
            slot.overlay
                .set_size(width, value_as_i64(value_ref(value)).max(0) as u32);
        }
        0
    })
}

extern "C" fn vo_visible_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_bool(out, slot.overlay.visible());
        0
    })
}

extern "C" fn vo_visible_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
        let visible = value_as_bool(unsafe { &*value });
        slot.overlay.set_visible(visible);
        if slot.overlay.is_open()
            && slot.mode == MODE_LAYER
            && let Ok(scene) = scene_mut(&_ctx)
        {
            for layer in [slot.layer1.as_ref(), slot.layer2.as_ref()]
                .into_iter()
                .flatten()
            {
                if let Some(state) = scene.layer_mut(layer.id) {
                    state.visible = visible;
                }
            }
        }
        0
    })
}

// -- playback state ---------------------------------------------------------

extern "C" fn vo_position_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(
            out,
            if slot.overlay.is_open() {
                slot.overlay.position_ms()
            } else {
                0
            },
        );
        0
    })
}

extern "C" fn vo_position_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        if slot.overlay.is_open() {
            // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
            slot.overlay.seek_ms(value_as_i64(unsafe { &*value }));
            // The sound follows the picture to the new position.
            let services = services(&_ctx);
            slot.sync_audio(services, Resync::Force);
        }
        0
    })
}

extern "C" fn vo_frame_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(
            out,
            if slot.overlay.is_open() {
                slot.overlay.frame()
            } else {
                0
            },
        );
        0
    })
}

extern "C" fn vo_frame_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        if slot.overlay.is_open() {
            // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
            slot.overlay.seek_frame(value_as_i64(unsafe { &*value }));
            // The sound follows the picture to the new position.
            let services = services(&_ctx);
            slot.sync_audio(services, Resync::Force);
        }
        0
    })
}

extern "C" fn vo_loop_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_bool(out, slot.overlay.looping);
        0
    })
}

extern "C" fn vo_loop_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
        slot.overlay.looping = value_as_bool(unsafe { &*value });
        0
    })
}

extern "C" fn vo_play_rate_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_real(
            out,
            if slot.overlay.is_open() {
                slot.overlay.play_rate
            } else {
                0.0
            },
        );
        0
    })
}

extern "C" fn vo_play_rate_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        if slot.overlay.is_open() {
            // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
            slot.overlay.play_rate = value_as_f64(unsafe { &*value });
        }
        0
    })
}

extern "C" fn vo_period_event_frame_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(out, slot.overlay.period_event().unwrap_or(-1));
        0
    })
}

extern "C" fn vo_period_event_frame_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        slot.overlay
            // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
            .set_period_event(value_as_i64(unsafe { &*value }));
        0
    })
}

extern "C" fn vo_segment_loop_start_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(
            out,
            slot.overlay.segment_loop().map_or(-1, |(start, _)| start),
        );
        0
    })
}

extern "C" fn vo_segment_loop_end_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(out, slot.overlay.segment_loop().map_or(-1, |(_, end)| end));
        0
    })
}

// -- audio / video streams --------------------------------------------------

extern "C" fn vo_audio_balance_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(
            out,
            if slot.overlay.is_open() {
                slot.audio_balance
            } else {
                0
            },
        );
        0
    })
}

extern "C" fn vo_audio_balance_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `value` is the valid `Value` the caller holds for this call.
    let balance = value_as_i64(unsafe { &*value });
    match run_slot(
        engine,
        instance,
        out_error,
        "audioBalance",
        |services, slot| {
            if slot.overlay.is_open() {
                slot.audio_balance = balance;
                slot.apply_audio(services);
            }
            Ok(())
        },
    ) {
        Ok(()) => 0,
        Err(code) => code,
    }
}

extern "C" fn vo_audio_volume_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(
            out,
            if slot.overlay.is_open() {
                slot.audio_volume
            } else {
                0
            },
        );
        0
    })
}

extern "C" fn vo_audio_volume_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `value` is the valid `Value` the caller holds for this call.
    let volume = value_as_i64(unsafe { &*value });
    match run_slot(
        engine,
        instance,
        out_error,
        "audioVolume",
        |services, slot| {
            if slot.overlay.is_open() {
                slot.audio_volume = volume;
                slot.apply_audio(services);
            }
            Ok(())
        },
    ) {
        Ok(()) => 0,
        Err(code) => code,
    }
}

extern "C" fn vo_enabled_audio_stream_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(
            out,
            if slot.overlay.is_open() {
                slot.enabled_audio_stream
            } else {
                -1
            },
        );
        0
    })
}

extern "C" fn vo_enabled_audio_stream_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `value` is the valid `Value` the caller holds for this call.
    let stream = value_as_i64(unsafe { &*value });
    match run_slot(
        engine,
        instance,
        out_error,
        "enabledAudioStream",
        |_services, slot| {
            if slot.overlay.is_open() {
                select_audio_stream(slot, stream)?;
            }
            Ok(())
        },
    ) {
        Ok(()) => 0,
        Err(code) => code,
    }
}

extern "C" fn vo_enabled_video_stream_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(
            out,
            if slot.overlay.is_open() {
                slot.enabled_video_stream
            } else {
                -1
            },
        );
        0
    })
}

extern "C" fn vo_enabled_video_stream_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `value` is the valid `Value` the caller holds for this call.
    let stream = value_as_i64(unsafe { &*value });
    match run_slot(
        engine,
        instance,
        out_error,
        "enabledVideoStream",
        |_services, slot| {
            if slot.overlay.is_open() {
                select_video_stream(slot, stream)?;
            }
            Ok(())
        },
    ) {
        Ok(()) => 0,
        Err(code) => code,
    }
}

// -- mixing -----------------------------------------------------------------

// The in-tree FFmpeg backend stubs the mixing setters and returns fixed
// values.
backend_open_real_prop!(vo_mixing_alpha_get, vo_mixing_alpha_set, 1.0, 0.0);
backend_open_int_prop!(vo_mixing_bg_get, vo_mixing_bg_set, 0xFF00_0000_i64, 0);

// -- mode -------------------------------------------------------------------

extern "C" fn vo_mode_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(out, slot.mode);
        0
    })
}

/// The reference ignores a `mode` write while a stream is open.
extern "C" fn vo_mode_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        if !slot.overlay.is_open() {
            // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
            slot.mode = value_as_i64(unsafe { &*value });
        }
        0
    })
}

// -- color adjustment (backend-defined; stubbed by the FFmpeg backend) -------

backend_stub_real_prop!(vo_contrast_get, vo_contrast_set);
backend_stub_real_prop!(vo_brightness_get, vo_brightness_set);
backend_stub_real_prop!(vo_hue_get, vo_hue_set);
backend_stub_real_prop!(vo_saturation_get, vo_saturation_set);

backend_const_real_prop!(vo_contrast_range_min_get);
backend_const_real_prop!(vo_contrast_range_max_get);
backend_const_real_prop!(vo_contrast_default_value_get);
backend_const_real_prop!(vo_contrast_step_size_get);
backend_const_real_prop!(vo_brightness_range_min_get);
backend_const_real_prop!(vo_brightness_range_max_get);
backend_const_real_prop!(vo_brightness_default_value_get);
backend_const_real_prop!(vo_brightness_step_size_get);
backend_const_real_prop!(vo_hue_range_min_get);
backend_const_real_prop!(vo_hue_range_max_get);
backend_const_real_prop!(vo_hue_default_value_get);
backend_const_real_prop!(vo_hue_step_size_get);
backend_const_real_prop!(vo_saturation_range_min_get);
backend_const_real_prop!(vo_saturation_range_max_get);
backend_const_real_prop!(vo_saturation_default_value_get);
backend_const_real_prop!(vo_saturation_step_size_get);

// -- media metadata ---------------------------------------------------------

extern "C" fn vo_original_width_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(
            out,
            slot.overlay.spec().map_or(0, |spec| i64::from(spec.width)),
        );
        0
    })
}

extern "C" fn vo_original_height_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(
            out,
            slot.overlay.spec().map_or(0, |spec| i64::from(spec.height)),
        );
        0
    })
}

extern "C" fn vo_number_of_frame_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(out, slot.overlay.total_frames());
        0
    })
}

extern "C" fn vo_fps_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_real(out, slot.overlay.fps());
        0
    })
}

extern "C" fn vo_total_time_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_slot!(engine, instance, out_error, _ctx, slot, {
        set_int(out, slot.overlay.duration_ms());
        0
    })
}

slot_ro_int!(vo_number_of_audio_stream_get, number_of_audio_stream);
slot_ro_int!(vo_number_of_video_stream_get, number_of_video_stream);

// -- layer1 / layer2 --------------------------------------------------------

extern "C" fn vo_layer1_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "VideoOverlay.layer1: compat context is not bound",
        );
    };
    // SAFETY: `instance` is the live `VideoOverlayInst` payload the VM allocated for this script object; no `&mut` to it is live in `vo_layer1_get`.
    let inst = unsafe { &*(instance as *const VideoOverlayInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "VideoOverlay.layer1: object is not constructed");
    };
    // SAFETY: `ctx` is only created for a bound, live engine, so its service pointer is valid for `vo_layer1_get`.
    let services = unsafe { ctx.services() };
    let raw = {
        let state = services.state::<VideoState>();
        state
            .overlays
            .get(&id)
            .and_then(|slot| slot.layer1.as_ref().map(|layer| layer.keepalive.raw_id()))
    };
    match raw {
        Some(raw) => match resolved.engine().duplicate_retained(raw) {
            Ok(duplicate) => {
                set_retained_out(out, duplicate.raw_id());
                std::mem::forget(duplicate);
            }
            Err(_) => set_null_out(resolved.engine(), out),
        },
        None => set_null_out(resolved.engine(), out),
    }
    0
}

extern "C" fn vo_layer1_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "VideoOverlay.layer1: compat context is not bound",
        );
    };
    // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
    let layer = match resolve_layer_ref(resolved.engine(), unsafe { &*value }) {
        Ok(layer) => layer,
        Err(error) => return report_error(out_error, &error),
    };
    // SAFETY: `instance` is the live `VideoOverlayInst` payload the VM allocated for this script object; no `&mut` to it is live in `vo_layer1_set`.
    let inst = unsafe { &*(instance as *const VideoOverlayInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "VideoOverlay.layer1: object is not constructed");
    };
    // SAFETY: `ctx` is only created for a bound, live engine, so its service pointer is valid for `vo_layer1_set`.
    let services = unsafe { ctx.services() };
    let mut state = services.state::<VideoState>();
    if let Some(slot) = state.overlays.get_mut(&id) {
        slot.layer1 = layer;
    }
    0
}

extern "C" fn vo_layer2_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "VideoOverlay.layer2: compat context is not bound",
        );
    };
    // SAFETY: `instance` is the live `VideoOverlayInst` payload the VM allocated for this script object; no `&mut` to it is live in `vo_layer2_get`.
    let inst = unsafe { &*(instance as *const VideoOverlayInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "VideoOverlay.layer2: object is not constructed");
    };
    // SAFETY: `ctx` is only created for a bound, live engine, so its service pointer is valid for `vo_layer2_get`.
    let services = unsafe { ctx.services() };
    let raw = {
        let state = services.state::<VideoState>();
        state
            .overlays
            .get(&id)
            .and_then(|slot| slot.layer2.as_ref().map(|layer| layer.keepalive.raw_id()))
    };
    match raw {
        Some(raw) => match resolved.engine().duplicate_retained(raw) {
            Ok(duplicate) => {
                set_retained_out(out, duplicate.raw_id());
                std::mem::forget(duplicate);
            }
            Err(_) => set_null_out(resolved.engine(), out),
        },
        None => set_null_out(resolved.engine(), out),
    }
    0
}

extern "C" fn vo_layer2_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "VideoOverlay.layer2: compat context is not bound",
        );
    };
    // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
    let layer = match resolve_layer_ref(resolved.engine(), unsafe { &*value }) {
        Ok(layer) => layer,
        Err(error) => return report_error(out_error, &error),
    };
    // SAFETY: `instance` is the live `VideoOverlayInst` payload the VM allocated for this script object; no `&mut` to it is live in `vo_layer2_set`.
    let inst = unsafe { &*(instance as *const VideoOverlayInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "VideoOverlay.layer2: object is not constructed");
    };
    // SAFETY: `ctx` is only created for a bound, live engine, so its service pointer is valid for `vo_layer2_set`.
    let services = unsafe { ctx.services() };
    let mut state = services.state::<VideoState>();
    if let Some(slot) = state.overlays.get_mut(&id) {
        slot.layer2 = layer;
    }
    0
}

// ---------------------------------------------------------------------------
// Event dispatch
// ---------------------------------------------------------------------------

/// A sink the event mapper calls for each queued [`VideoEvent`].
trait EventSink {
    /// Call `member` on the overlay object with `args`.
    fn call_member(
        &self,
        objthis: *mut c_void,
        member: &str,
        args: &[TjsValue],
    ) -> Result<TjsValue, String>;
}

/// Map queued L2 events to the script `on*` members (the reference
/// `FireStatusChanged`/`FirePeriodEvent`/`FireFrameUpdateEvent`).
fn dispatch_events_with(sink: &dyn EventSink, objthis: *mut c_void, events: &[VideoEvent]) {
    if objthis.is_null() {
        return;
    }
    for event in events {
        let (member, args) = match event {
            VideoEvent::StatusChanged(status) => (
                "onStatusChanged",
                vec![TjsValue::String(status.as_str().to_string())],
            ),
            VideoEvent::Period(reason) => ("onPeriod", vec![TjsValue::Integer(reason.code())]),
            VideoEvent::FrameUpdate(frame) => ("onFrameUpdate", vec![TjsValue::Integer(*frame)]),
            // No registered `setTransitionCompleteCall` in the 70-member
            // surface: the host observes the completion through the queue.
            VideoEvent::TransitionComplete => continue,
        };
        if let Err(error) = sink.call_member(objthis, member, &args) {
            log::warn!("VideoOverlay: {member} failed: {error}");
        }
    }
}

/// The real sink: retain the object for the duration of the dispatch and call
/// the member through the VM (a script override wins over the native hook).
struct EngineSink<'a> {
    host: &'a Tjs2Engine,
    owner: DetachedValue,
}

impl EventSink for EngineSink<'_> {
    fn call_member(
        &self,
        _objthis: *mut c_void,
        member: &str,
        args: &[TjsValue],
    ) -> Result<TjsValue, String> {
        self.host.call_member(self.owner.raw_id(), member, args)
    }
}

/// Dispatch an overlay's queued events through the VM.
fn dispatch_events(host: &Tjs2Engine, objthis: *mut c_void, events: &[VideoEvent]) {
    if objthis.is_null() {
        return;
    }
    let Ok(owner) = host.retain_object_detached(objthis) else {
        return;
    };
    let sink = EngineSink { host, owner };
    dispatch_events_with(&sink, objthis, events);
}

// ---------------------------------------------------------------------------
// Frame-body poll
// ---------------------------------------------------------------------------

/// `vomLayer`: the decoded frame is assigned to `layer1`/`layer2`.
const MODE_LAYER: i64 = 1;
/// `vomMixer`: the frame is presented through the mixing layer.
const MODE_MIXER: i64 = 2;

/// Advance every open overlay to the engine's clock, present the host frame
/// at the new position, composite it into the script layers the mode selects
/// and dispatch the queued events.
///
/// The picture's clock is the engine's: the step is the difference against the
/// previous advance. The host bounds the step by the same cap its audio pump
/// puts on one frame's playback (`max_step_ms`), so the picture and the sound
/// fall behind together.
///
/// # Errors
///
/// Returns [`CompatError::Engine`] when an overlay cannot be advanced or
/// its frame cannot be composited into the script layers.
pub fn video_advance(ctx: &CompatCtx, max_step_ms: f64) -> Result<(), CompatError> {
    // SAFETY: `ctx` is only created for a bound, live engine, so its VM pointer is valid for `video_advance`.
    let host = unsafe { ctx.engine() };
    // SAFETY: `ctx` is only created for a bound, live engine, so its service pointer is valid for `video_advance`.
    let services = unsafe { ctx.services() };
    // Phase 1: advance, present and composite. The state borrow is released
    // before any script callback runs, so a re-entrant native call cannot
    // alias it.
    let pending: Vec<(*mut c_void, Vec<VideoEvent>)> = {
        let mut state = services.state::<VideoState>();
        let mut source = state.source.take();
        let mut pending = Vec::new();
        // The engine clock's elapsed time is the picture's step.
        let now_ms = frame_clock_f64(services)?;
        let dt = (now_ms - state.last_advance_ms).max(0.0).min(max_step_ms);
        state.last_advance_ms = now_ms;
        for slot in state.overlays.values_mut() {
            if !slot.overlay.is_open() {
                continue;
            }
            slot.overlay.advance(dt);
            slot.follow_audio(services);
            // The frame is fetched once, after the clock moved, so the picture
            // belongs to the position the overlay is at.
            let presented = present(&mut source, slot);
            // The composite happens while the overlay is still playing, so a
            // prepared picture reaches the layer before prepare pauses it.
            composite_overlay(ctx, slot);
            if presented {
                slot.overlay.complete_prepare();
            }
            slot.sync_audio(services, frame_resync(&slot.overlay));
            let events = slot.overlay.poll();
            if !events.is_empty() {
                pending.push((slot.objthis, events));
            }
        }
        state.source = source;
        pending
    };
    // Phase 2: dispatch outside the state borrow.
    for (objthis, events) in pending {
        dispatch_events(host, objthis, &events);
    }
    Ok(())
}

/// The resync policy for one frame of a movie.
///
/// A loop restart puts the voice back onto the picture's position; an
/// ordinary frame only corrects a voice that has really drifted away.
pub(crate) fn frame_resync(overlay: &VideoOverlay) -> Resync {
    if overlay.restarted_at_loop() {
        Resync::Force
    } else {
        Resync::OnDrift
    }
}

/// Present the host frame at the overlay's current position, if any.
///
/// The frame is shared with the host, so presenting it copies no pixels. The
/// return value says whether a frame was presented, so the caller can finish
/// a prepare only after a delivered picture has been shown.
fn present(source: &mut Option<Box<dyn VideoSource>>, slot: &mut VideoSlot) -> bool {
    let Some(source) = source.as_mut() else {
        return false;
    };
    let at = slot.overlay.position_ms();
    let Some(frame) = source.frame_at(&slot.path, at) else {
        return false;
    };
    if let Err(error) = slot.overlay.present_frame(frame) {
        log::warn!("VideoOverlay: inconsistent decoded frame: {error}");
        return false;
    }
    true
}

/// Composite the overlay's current frame into the layer(s) the mode selects.
///
/// `vomOverlay` has no script-visible destination in this port, so it is left
/// alone. `vomLayer` mirrors the reference assigning the frame to
/// `layer1`/`layer2`; `vomMixer` draws it over the mixing layer.
fn composite_overlay(ctx: &CompatCtx, slot: &VideoSlot) {
    if !slot.overlay.is_playing() {
        return;
    }
    let Some(frame) = slot.overlay.current_frame() else {
        return;
    };
    match slot.mode {
        MODE_LAYER => {
            let layers = [slot.layer1.as_ref(), slot.layer2.as_ref()];
            for layer in layers.into_iter().flatten() {
                prepare_layer_for_frame(ctx, layer.id, frame);
                composite_frame(ctx, layer.id, frame, false, 255);
            }
        }
        MODE_MIXER => {
            if let Some(layer) = slot.mixing_layer.as_ref() {
                let opacity = (slot.mixing_layer_alpha * 255.0).clamp(0.0, 255.0).round() as u8;
                composite_frame(ctx, layer.id, frame, true, opacity);
            }
        }
        _ => {}
    }
}

/// Size `layer_id`'s MainImage and rectangle to the frame when they differ.
fn prepare_layer_for_frame(ctx: &CompatCtx, layer_id: u32, frame: &RgbaFrame) {
    let Ok(scene) = scene_mut(ctx) else {
        return;
    };
    let matches = scene.layer(layer_id).is_some_and(|layer| {
        layer.image_width == frame.width && layer.image_height == frame.height
    });
    if !matches && !scene.set_layer_image_size(layer_id, frame.width, frame.height) {
        return;
    }
    if let Some(layer) = scene.layer_mut(layer_id)
        && (layer.rect.w != frame.width || layer.rect.h != frame.height)
    {
        layer.rect.w = frame.width;
        layer.rect.h = frame.height;
    }
}

/// Blit `frame` into `layer_id`'s MainImage with `opacity` in the
/// alpha-compositing path.
fn composite_frame(
    ctx: &CompatCtx,
    layer_id: u32,
    frame: &RgbaFrame,
    source_over: bool,
    opacity: u8,
) {
    let Ok(scene) = scene_mut(ctx) else {
        return;
    };
    blit_frame_to_layer_opacity(scene, layer_id, frame, source_over, opacity);
}

/// The attached L2 scene, mutably.
fn scene_mut<'a>(ctx: &CompatCtx) -> Result<&'a mut Scene, String> {
    // SAFETY: the host keeps the attached scene alive for the binding's
    // lifetime and the VM is single-threaded.
    let ptr = unsafe { ctx.services() }
        .scene()
        .ok_or_else(|| "VideoOverlay: no scene attached".to_string())?;
    // SAFETY: see above.
    Ok(unsafe { &mut *ptr })
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the `VideoOverlay` class.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "VideoOverlay",
        create: video_create,
        destroy: video_destroy,
        invalidate: None,
        methods: vec![
            NativeInstanceMethodDef {
                name: "VideoOverlay",
                f: video_ctor,
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
                name: "pause",
                f: vo_pause,
            },
            NativeInstanceMethodDef {
                name: "stop",
                f: vo_stop,
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
                name: "setMixingLayer",
                f: vo_set_mixing_layer,
            },
            NativeInstanceMethodDef {
                name: "resetMixingLayer",
                f: vo_reset_mixing_layer,
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
                get: Some(vo_left_get),
                set: Some(vo_left_set),
            },
            NativeInstancePropertyDef {
                name: "top",
                get: Some(vo_top_get),
                set: Some(vo_top_set),
            },
            NativeInstancePropertyDef {
                name: "width",
                get: Some(vo_width_get),
                set: Some(vo_width_set),
            },
            NativeInstancePropertyDef {
                name: "height",
                get: Some(vo_height_get),
                set: Some(vo_height_set),
            },
            NativeInstancePropertyDef {
                name: "visible",
                get: Some(vo_visible_get),
                set: Some(vo_visible_set),
            },
            NativeInstancePropertyDef {
                name: "position",
                get: Some(vo_position_get),
                set: Some(vo_position_set),
            },
            NativeInstancePropertyDef {
                name: "frame",
                get: Some(vo_frame_get),
                set: Some(vo_frame_set),
            },
            NativeInstancePropertyDef {
                name: "loop",
                get: Some(vo_loop_get),
                set: Some(vo_loop_set),
            },
            NativeInstancePropertyDef {
                name: "mode",
                get: Some(vo_mode_get),
                set: Some(vo_mode_set),
            },
            NativeInstancePropertyDef {
                name: "playRate",
                get: Some(vo_play_rate_get),
                set: Some(vo_play_rate_set),
            },
            NativeInstancePropertyDef {
                name: "periodEventFrame",
                get: Some(vo_period_event_frame_get),
                set: Some(vo_period_event_frame_set),
            },
            NativeInstancePropertyDef {
                name: "segmentLoopStartFrame",
                get: Some(vo_segment_loop_start_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "segmentLoopEndFrame",
                get: Some(vo_segment_loop_end_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "audioBalance",
                get: Some(vo_audio_balance_get),
                set: Some(vo_audio_balance_set),
            },
            NativeInstancePropertyDef {
                name: "audioVolume",
                get: Some(vo_audio_volume_get),
                set: Some(vo_audio_volume_set),
            },
            NativeInstancePropertyDef {
                name: "enabledAudioStream",
                get: Some(vo_enabled_audio_stream_get),
                set: Some(vo_enabled_audio_stream_set),
            },
            NativeInstancePropertyDef {
                name: "enabledVideoStream",
                get: Some(vo_enabled_video_stream_get),
                set: Some(vo_enabled_video_stream_set),
            },
            NativeInstancePropertyDef {
                name: "mixingMovieAlpha",
                get: Some(vo_mixing_alpha_get),
                set: Some(vo_mixing_alpha_set),
            },
            NativeInstancePropertyDef {
                name: "mixingMovieBGColor",
                get: Some(vo_mixing_bg_get),
                set: Some(vo_mixing_bg_set),
            },
            NativeInstancePropertyDef {
                name: "contrast",
                get: Some(vo_contrast_get),
                set: Some(vo_contrast_set),
            },
            NativeInstancePropertyDef {
                name: "contrastRangeMin",
                get: Some(vo_contrast_range_min_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "contrastRangeMax",
                get: Some(vo_contrast_range_max_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "contrastDefaultValue",
                get: Some(vo_contrast_default_value_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "contrastStepSize",
                get: Some(vo_contrast_step_size_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "brightness",
                get: Some(vo_brightness_get),
                set: Some(vo_brightness_set),
            },
            NativeInstancePropertyDef {
                name: "brightnessRangeMin",
                get: Some(vo_brightness_range_min_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "brightnessRangeMax",
                get: Some(vo_brightness_range_max_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "brightnessDefaultValue",
                get: Some(vo_brightness_default_value_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "brightnessStepSize",
                get: Some(vo_brightness_step_size_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "hue",
                get: Some(vo_hue_get),
                set: Some(vo_hue_set),
            },
            NativeInstancePropertyDef {
                name: "hueRangeMin",
                get: Some(vo_hue_range_min_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "hueRangeMax",
                get: Some(vo_hue_range_max_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "hueDefaultValue",
                get: Some(vo_hue_default_value_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "hueStepSize",
                get: Some(vo_hue_step_size_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "saturation",
                get: Some(vo_saturation_get),
                set: Some(vo_saturation_set),
            },
            NativeInstancePropertyDef {
                name: "saturationRangeMin",
                get: Some(vo_saturation_range_min_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "saturationRangeMax",
                get: Some(vo_saturation_range_max_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "saturationDefaultValue",
                get: Some(vo_saturation_default_value_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "saturationStepSize",
                get: Some(vo_saturation_step_size_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "originalWidth",
                get: Some(vo_original_width_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "originalHeight",
                get: Some(vo_original_height_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "numberOfFrame",
                get: Some(vo_number_of_frame_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "fps",
                get: Some(vo_fps_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "totalTime",
                get: Some(vo_total_time_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "numberOfAudioStream",
                get: Some(vo_number_of_audio_stream_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "numberOfVideoStream",
                get: Some(vo_number_of_video_stream_get),
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

// ---------------------------------------------------------------------------
// Unit tests (pure: no VM, so they are parallel-safe)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_api::PlayStatus;
    use std::cell::RefCell;

    struct FakeSink {
        calls: RefCell<Vec<(String, Vec<TjsValue>)>>,
    }

    impl EventSink for FakeSink {
        fn call_member(
            &self,
            _objthis: *mut c_void,
            member: &str,
            args: &[TjsValue],
        ) -> Result<TjsValue, String> {
            self.calls
                .borrow_mut()
                .push((member.to_string(), args.to_vec()));
            Ok(TjsValue::Void)
        }
    }

    #[test]
    fn dispatch_maps_events_to_script_members() {
        let sink = FakeSink {
            calls: RefCell::new(Vec::new()),
        };
        let events = vec![
            VideoEvent::StatusChanged(PlayStatus::Play),
            VideoEvent::Period(engine::video::PeriodReason::SegmentLoop),
            VideoEvent::FrameUpdate(7),
            VideoEvent::TransitionComplete,
        ];
        dispatch_events_with(&sink, std::ptr::dangling_mut::<c_void>(), &events);
        let calls = sink.calls.borrow();
        assert_eq!(calls.len(), 3, "TransitionComplete has no script member");
        assert_eq!(
            calls[0],
            (
                "onStatusChanged".to_string(),
                vec![TjsValue::String("play".to_string())]
            )
        );
        assert_eq!(
            calls[1],
            ("onPeriod".to_string(), vec![TjsValue::Integer(3)])
        );
        assert_eq!(
            calls[2],
            ("onFrameUpdate".to_string(), vec![TjsValue::Integer(7)])
        );
    }

    #[test]
    fn buffered_source_selects_the_frame_at_a_position() {
        let frame = |pts_ms: u64| RgbaFrame::new(1, 1, pts_ms, vec![0; 4]);
        let mut source = BufferedSource {
            spec: VideoSpec {
                width: 1,
                height: 1,
                fps: 30.0,
                total_frames: Some(3),
                sample_rate: 0,
                channels: 0,
                ..VideoSpec::default()
            },
            frames: vec![frame(0), frame(100), frame(200)],
        };
        assert_eq!(source.frame_at("x", 0).unwrap().pts_ms, 0);
        assert_eq!(source.frame_at("x", 150).unwrap().pts_ms, 100);
        assert_eq!(source.frame_at("x", 9999).unwrap().pts_ms, 200);
        assert!(
            BufferedSource {
                spec: source.spec,
                frames: Vec::new(),
            }
            .frame_at("x", 0)
            .is_none()
        );
    }
}
