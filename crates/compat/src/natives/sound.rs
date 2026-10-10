//! `WaveSoundBuffer` native class.
//!
//! Behavioral reference: `main`'s `crates/tvp-sound` (`wavesound.rs` +
//! `natives.rs`) and the reference C++
//! `reference/cpp/core/sound/WaveIntf.cpp` +
//! `reference/cpp/core/sound/win32/WaveImpl.cpp`. This is a reimplementation
//! on the layered API: the class composes the L2 [`engine::sound`] model and
//! mixer and owns no process global — the one shared [`Mixer`] and the
//! per-object [`SoundBuffer`]/[`SoundChannel`]/[`SoundSource`] handles live in
//! [`CompatServices::state`].
//!
//! # Members
//!
//! The reference surface (33 members) is registered:
//!
//! * methods — `WaveSoundBuffer` (constructor), `open`, `play`, `stop`,
//!   `fade`, `stopFade`, `setPos`, `getVisBuffer`, the event forwarders
//!   `onStatusChanged`/`onFadeCompleted`/`onLabel`, and the static
//!   `freeDirectSound`;
//! * properties — `position`, `samplePosition`, `paused`, `totalTime`,
//!   `looping`, `volume`, `volume2`, `pan`, `posX`, `posY`, `posZ`, `status`,
//!   `frequency`, `bits`, `channels`, `flags`, `labels`, `filters`,
//!   `useVisBuffer`, and the static `globalVolume`/`globalFocusMode`.
//!
//! # TJS ↔ L2 translation
//!
//! The reference exposes volumes/pan on a `0..=100000` scale and positions in
//! milliseconds; the L2 mixer works in `0..=1`, `-1..=1` and seconds. Every
//! boundary divides/multiplies by [`TVP_VOLUME_SCALE`] or `1000` so the script
//! never sees a mixer-native scaled value. `samplePosition` is a granule count
//! (`position_seconds × sample_rate`); `totalTime` is milliseconds.
//!
//! # Opening a source
//!
//! L3 has no codec backend (the L1 `media` crate is not a dependency of
//! `compat`), so the host installs one with [`add_audio_decoder`]. `open`
//! reads the audio entry and its `<name>.sli` side-car through the attached L2
//! [`Storage`](engine::Storage), parses the side-car with the module's
//! [`parse_sli`], and hands both to the decoder — a typed error
//! when no decoder or no storage is installed. The decoder returns a
//! ready [`SoundSource`], which is stored both on the L2 [`SoundBuffer`] and in
//! the per-object handle.
//!
//! # Events
//!
//! [`sound_poll`] is the `sound_poll` step of the documented frame body
//! (`compat::frame::FrameBody`); the host calls it once per frame. It advances
//! the shared mixer to the L1 clock and forwards the L2 [`SoundEvent`]s the
//! [`SoundBuffer::poll`] observed — `onStatusChanged(status)`,
//! `onFadeCompleted()` and `onLabel(name)` — to the object's class chain, so a
//! script subclass override runs first and may call `super`. The dispatch
//! happens with the state borrow released, so a handler may call back into the
//! natives.
//!
//! The reference delivers the three events through **the object** as well
//! (`TVPPostEvent(Owner, Owner, ...)` at `SoundBufferBaseIntf.cpp:83-90` and
//! `WaveIntf.cpp:897-908`), so a script override always wins over the native
//! member; the native members themselves then re-invoke the constructor's
//! **action owner** as `actionOwner.action(event)` with the reference event
//! dictionary (`{type, target, status|name}` — `EventIntf.h:208-234`,
//! `EventIntf.cpp:709-740`). `new WaveSoundBuffer(controller)` installs that
//! owner from `param[0]` (`SoundBufferBaseIntf.cpp:44-45`); a non-object
//! argument yields the reference's empty closure and is silently never invoked.
//!
//! Script-driven status changes (`open`/`play`/`stop`) are dispatched
//! **synchronously inside the call**, like the reference's `TVP_EPT_IMMEDIATE`
//! `SetStatus` (`SoundBufferBaseIntf.cpp:75-92`, `EventIntf.cpp:217-228`), and
//! they cancel the events still queued for that object (reference
//! `TVPCancelSourceEvents`, `SoundBufferBaseIntf.cpp:79-80`). Label and fade
//! completions were posted with `TVP_EPT_POST` in the reference, so they are
//! delivered at the next [`sound_poll`].
//!
//! # Fading
//!
//! The reference fade is driven by the 60 ms sound-buffer beat
//! (`TVP_SB_BEAT_INTERVAL`, `SoundBufferBaseImpl.h:19`): `Fade` stores
//! `TargetVolume`/`DeltaVolume`/`FadeCount`/`BlankLeft` and each beat clamps
//! the ramped volume back through `SetVolume`
//! (`SoundBufferBaseIntf.cpp:104-193`). This port reproduces that state
//! machine verbatim (integer delta, beat countdown, blank delay) on the
//! [`SoundObject`] and writes the stepped volume into the L2 channel on every
//! beat, so mid-fade `volume` readings, the `time < 60` instant completion,
//! the `stopFade` target jump and the "a fade survives `stop()`/`open()`"
//! behaviour all match the reference. The beat only advances when
//! [`sound_poll`] (or the frame body) runs, which is the port's frame clock.
//!
//! # Teardown
//!
//! `invalidate` runs [`sound_invalidate`]: it tears the object down exactly
//! like the reference `Invalidate` chain (`WaveImpl.cpp:1960-1974`,
//! `WaveIntf.cpp:885-894`, `SoundBufferBaseIntf.cpp:46-55`) — playback stops
//! (the mixer channel is removed), every retained value (`self`, `filters`,
//! `flags`, `labels`, the action owner) is released, the cached `labels`
//! dictionary is invalidated, and no further events are dispatched. Dropping
//! the last script reference without `invalidate` cannot destroy the object:
//! the port retains `this` so the poll can dispatch through it (TJS2 has no
//! cycle collector), which is why teardown is `invalidate`-driven here.
//!
//! # Deliberately deferred
//!
//! * `bits` reports `32` for a loaded source (the L2 pipeline is `f32`);
//!   `AudioSpec` carries no bit depth, so the reference's 16-bit WAV value
//!   cannot be reproduced truthfully.
//! * `getVisBuffer` returns `0`: L3 captures no visualization buffer (the
//!   reference writes into a raw sample pointer, which has no safe L2
//!   counterpart). `useVisBuffer` still round-trips its stored flag.
//! * `flags` returns the live `WaveFlags` proxy (cached per buffer, so
//!   `s.flags === s.flags`), and `labels` the cached dictionary of the loaded
//!   `.sli` labels (rebuilt only where the reference calls
//!   `RecreateWaveLabelsObject`, i.e. on a successful `.sli` read and on
//!   `invalidate`).
//! * `filters` is the reference's stable TJS array. A `PhaseVocoder` added to
//!   it is applied by `play` through the real DSP (pitch/time scaling);
//!   elements without a resolvable `interface` are skipped, as the reference
//!   `RebuildFilterChain` does.
//! * `frequency` is plain state, like the reference: `SetFrequency` only
//!   stores the value (`WaveImpl.cpp:3336-3342`) and `SetFrequencyToBuffer`
//!   is a no-op in the reference build (`WaveImpl.cpp:3329-3334`), so the
//!   member never changes the playback rate. `open` re-derives it from the
//!   source (`WaveImpl.cpp:3056`).
//! * `freeDirectSound` is a no-op — there is no DirectSound in this port.
//! * The reference constructor *requires* one argument (the action owner,
//!   `SoundBufferBaseIntf.cpp:32-33`); the port still accepts
//!   `new WaveSoundBuffer()` because three test suites outside this module
//!   construct without arguments, and stores `None` — the reference's empty
//!   closure — so the event forwarders stay silent, exactly like
//!   `new WaveSoundBuffer(null)` does.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};
use std::sync::Arc;

use api::media::AudioSpec;

use engine::sound::{
    LoopCondition, LoopInfo, LoopLink, Mixer, SoundBuffer, SoundChannel, SoundEvent, SoundLabel,
    SoundSource, Status,
};
use platform::Clock;
use tjs::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef,
    NativeMethodDef, NativePropertyDef, NativeStaticMembers, RetainedValue, Tjs2Engine,
    Tjs2ValueId, TjsValue, VAL_INTEGER, VAL_REAL, VAL_RETAINED, VAL_STRING, VAL_VOID, Value,
};

use crate::native_util::{args, report_error, set_int, set_void, value_as_string};
use crate::natives::system::{resolve, set_string_out, value_as_bool, value_as_i64};
use crate::{CompatCtx, CompatError, CompatServices};

mod phase_vocoder;
mod wave_flags;

use phase_vocoder::VocoderConfig;

// ---------------------------------------------------------------------------
// TVP `.sli` side-car reader (the engine model is generic)
// ---------------------------------------------------------------------------

/// Script volume/pan scale used by the reference `WaveSoundBuffer` natives
/// (`0..=100000`). The L2 mixer works in `0..=1`; L3 divides by this when
/// crossing the script boundary.
pub(crate) const TVP_VOLUME_SCALE: f64 = 100_000.0;

/// Errors produced while reading a TVP `.sli` side-car.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub(crate) enum SliError {
    /// The bytes are not one of the two supported `.sli` formats.
    #[error("invalid .sli data: {message}")]
    Invalid {
        /// Human-readable description of the failure.
        message: String,
    },
}

fn sli_error(message: impl Into<String>) -> SliError {
    SliError::Invalid {
        message: message.into(),
    }
}

/// Parse `.sli` bytes. A leading UTF-8 BOM is tolerated.
///
/// Returns [`SliError`] only when the text is not one of the two supported
/// formats; an empty/whitespace/comment-only file yields an empty
/// [`LoopInfo`].
pub(crate) fn parse_sli(bytes: &[u8]) -> Result<LoopInfo, SliError> {
    let text = String::from_utf8_lossy(bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return Ok(LoopInfo::default());
    }
    if trimmed.starts_with('#') && !trimmed.starts_with("#2.") {
        return Ok(LoopInfo::default());
    }
    if !trimmed.starts_with('#') {
        return parse_old(trimmed);
    }
    parse_v2(trimmed)
}

/// Parse the reference condition spelling (case-insensitive).
fn parse_condition(text: &str) -> Option<LoopCondition> {
    match text.trim().to_ascii_lowercase().as_str() {
        "no" | "" => Some(LoopCondition::None),
        "eq" => Some(LoopCondition::Equal),
        "ne" => Some(LoopCondition::NotEqual),
        "gt" => Some(LoopCondition::Greater),
        "ge" => Some(LoopCondition::GreaterOrEqual),
        "lt" => Some(LoopCondition::Lesser),
        "le" => Some(LoopCondition::LesserOrEqual),
        _ => None,
    }
}

/// Old format: `LoopStart=<n>` / `LoopLength=<n>`.
fn parse_old(text: &str) -> Result<LoopInfo, SliError> {
    let find = |key: &str| -> Option<&str> {
        text.find(key).map(|i| {
            let rest = &text[i + key.len()..];
            let end = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            &rest[..end]
        })
    };
    let length: u64 = find("LoopLength=")
        .ok_or_else(|| sli_error("sli: missing LoopLength"))?
        .parse()
        .map_err(|_| sli_error("sli: invalid LoopLength"))?;
    let start: u64 = find("LoopStart=")
        .ok_or_else(|| sli_error("sli: missing LoopStart"))?
        .parse()
        .map_err(|_| sli_error("sli: invalid LoopStart"))?;
    Ok(LoopInfo::new(
        vec![LoopLink::new(
            start.saturating_add(length),
            start,
            false,
            LoopCondition::None,
            0,
            0,
        )],
        Vec::new(),
    ))
}

/// v2.00 format: `Link { … }` / `Label { … }` blocks.
fn parse_v2(text: &str) -> Result<LoopInfo, SliError> {
    let mut info = LoopInfo::default();
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '#' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && (bytes[i] as char).is_ascii_alphabetic() {
            i += 1;
        }
        let word = &text[start..i];
        if word.eq_ignore_ascii_case("Link") || word.eq_ignore_ascii_case("Label") {
            while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
                i += 1;
            }
            if i >= bytes.len() || bytes[i] != b'{' {
                return Err(sli_error(format!("sli: expected '{{' after {word}")));
            }
            let body_start = i + 1;
            let Some(rel_end) = text[body_start..].find('}') else {
                return Err(sli_error(format!("sli: unterminated {word} block")));
            };
            let body = &text[body_start..body_start + rel_end];
            i = body_start + rel_end + 1;
            if word.eq_ignore_ascii_case("Link") {
                info.links.push(parse_link(body)?);
            } else {
                info.labels.push(parse_label(body));
            }
        } else {
            return Err(sli_error(format!("sli: unexpected token '{word}'")));
        }
    }
    Ok(info)
}

/// Parse one `key=value;` block body into a [`LoopLink`].
fn parse_link(body: &str) -> Result<LoopLink, SliError> {
    let mut link = LoopLink::new(0, 0, false, LoopCondition::None, 0, 0);
    for (key, value) in entities(body) {
        match key.to_ascii_lowercase().as_str() {
            "from" => link.from = value.trim().parse().unwrap_or(0),
            "to" => link.to = value.trim().parse().unwrap_or(0),
            "smooth" => link.smooth = value.trim().eq_ignore_ascii_case("true"),
            "condition" => {
                link.condition = parse_condition(&value)
                    .ok_or_else(|| sli_error(format!("sli: unknown Condition '{value}'")))?
            }
            "refvalue" => link.ref_value = value.trim().parse().unwrap_or(0),
            "condvar" => link.cond_var = value.trim().parse().unwrap_or(0),
            _ => {}
        }
    }
    Ok(link)
}

/// Parse one `key=value;` block body into a [`SoundLabel`].
fn parse_label(body: &str) -> SoundLabel {
    let mut label = SoundLabel::new(0, String::new());
    for (key, value) in entities(body) {
        match key.to_ascii_lowercase().as_str() {
            "position" => label.position = value.trim().parse().unwrap_or(0),
            "name" => {
                let v = value.trim();
                label.name = v.trim_matches(|c| c == '\'' || c == '"').to_string();
            }
            _ => {}
        }
    }
    label
}

/// Split a `key=value; key=value;` body into pairs (`GetEntityToken`).
fn entities(body: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for part in body.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some(eq) = part.find('=') {
            let key = part[..eq].trim().to_string();
            let value = part[eq + 1..].trim().to_string();
            if !key.is_empty() {
                out.push((key, value));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Host-supplied decode hook
// ---------------------------------------------------------------------------

/// A host-supplied whole-file audio decoder.
///
/// `open` reads the audio bytes and the optional `.sli` (already parsed into a
/// generic [`LoopInfo`]) from the attached L2 storage and calls this to produce
/// a ready [`SoundSource`]. The host (L4), which links the L1 `media` crate,
/// installs it with [`add_audio_decoder`]. `hint` is the storage name, used
/// only for backend selection.
pub type AudioDecoder =
    Box<dyn Fn(&[u8], Option<LoopInfo>, Option<&str>) -> Result<Arc<SoundSource>, String>>;

/// Append a decoder to the per-engine chain.
///
/// `open` tries every installed decoder in order and uses the first that
/// succeeds, so format-specific plugin decoders (e.g. the `wutcwf` plugin's
/// TCWF decoder) compose with the host's general codec instead of replacing
/// it. The L4 host appends its `media` decoder; plugin `register` calls append
/// theirs. When no decoder matches, `open` reports every decoder's error,
/// joined in chain order.
pub fn add_audio_decoder(services: &CompatServices, decoder: AudioDecoder) {
    services.state::<SoundState>().decoders.push(decoder);
}

// ---------------------------------------------------------------------------
// Per-engine dispatch state (never a process global)
// ---------------------------------------------------------------------------

/// One live `WaveSoundBuffer`: the L2 script object state, the mixer channel
/// it plays on, and the L3 payload that outlives a callback.
///
/// The [`SoundChannel`] itself is owned by the shared [`Mixer`] (so the device
/// pull sees every voice); this handle keeps its id plus the matching
/// [`SoundBuffer`]/[`SoundSource`] the reference keeps in the native instance.
struct SoundObject {
    /// L2 script-object half: loaded source, loop flags, label bookkeeping.
    buffer: SoundBuffer,
    /// The mixer channel this object drives.
    channel_id: u64,
    /// The loaded decoded source, kept beside the buffer for the
    /// `frequency`/`channels`/`bits` translation.
    source: Option<Arc<SoundSource>>,
    /// The last successfully decoded format — the reference's `InputFormat`,
    /// assigned *before* the `.sli` parse (`WaveImpl.cpp:3054-3055`) and
    /// never zeroed by `Clear()` (`WaveImpl.cpp:2352-2370`). `bits`,
    /// `channels` and `totalTime` read it, not the decoder's presence, so a
    /// side-car failure still reports the format.
    format: Option<SoundFormat>,
    /// Retained `objthis` (the `WaveSoundBuffer` or a script subclass); the
    /// poll delivers events through its class chain.
    self_obj: Option<DetachedValue>,
    /// Retained `param[0]`, the reference `ActionOwner` closure
    /// (`SoundBufferBaseIntf.cpp:44-45`); `None` is the reference's empty
    /// closure, which the native event members skip.
    action_owner: Option<DetachedValue>,
    /// Stable `filters` array (reference `GetFiltersNoAddRef`).
    filters: Option<DetachedValue>,
    /// Cached `flags` proxy (reference `GetWaveFlagsObjectNoAddRef`), so
    /// `buffer.flags === buffer.flags` holds and the flag state stays live.
    flags_obj: Option<DetachedValue>,
    /// Cached `labels` dictionary (reference `GetWaveLabelsObjectNoAddRef`,
    /// `WaveIntf.cpp:1039-1043`): stable identity until the reference calls
    /// `RecreateWaveLabelsObject` (a successful `.sli` read, `WaveImpl.cpp:3083`,
    /// or `invalidate`, `WaveIntf.cpp:887`).
    labels_obj: Option<DetachedValue>,
    /// `useVisBuffer` (stored; see the module docs).
    use_vis_buffer: bool,
    /// `frequency`, stored verbatim like the reference `Frequency` field
    /// (`WaveImpl.cpp:3336-3342`): `0` before the first `open`, the source's
    /// sample rate after `open` (`WaveImpl.cpp:3056`), and whatever the
    /// script wrote in between — including `0` and negatives.
    frequency: i64,
    /// `paused` is settable before `play` (the reference keeps the flag on the
    /// buffer, not the device).
    paused: bool,
    /// True once `play` created the playback device for this object — the
    /// reference's `SoundBuffer != nullptr`, which `GetSamplePosition` /
    /// `GetPosition` require (`WaveImpl.cpp:3099-3100`, `3148-3149`). It is
    /// never cleared by `open` (the reference's device survives `Clear()`).
    device_ready: bool,
    /// The active reference fade (`tTJSNI_BaseSoundBuffer`'s
    /// `TargetVolume`/`DeltaVolume`/`FadeCount`/`BlankLeft`,
    /// `SoundBufferBaseIntf.cpp:160-167`).
    fade: Option<FadeState>,
    /// Milliseconds accumulated towards the next 60 ms fade beat.
    fade_tick_ms: f64,
    /// Events already posted with `TVP_EPT_POST` (labels, natural fade
    /// completions) waiting for the next [`sound_poll`].
    pending: Vec<(&'static str, Vec<TjsValue>)>,
}

/// One reference fade, in script volume scale — the fields of
/// `tTJSNI_BaseSoundBuffer` behind `Fade` (`SoundBufferBaseIntf.cpp:160-167`).
struct FadeState {
    /// `TargetVolume` (the unclamped script value; `SetVolume` clamps).
    to: i64,
    /// `DeltaVolume` — `(to - GetVolume()) * 60 / time`, integer arithmetic.
    delta: i64,
    /// `FadeCount` — `time / TVP_SB_BEAT_INTERVAL` remaining beat steps.
    count: i64,
    /// `BlankLeft` — the delay in milliseconds.
    blank: i64,
}

/// The reference `TVP_SB_BEAT_INTERVAL` (`SoundBufferBaseImpl.h:19`): the
/// sound-buffer timer fires every 60 ms and drives the fade ramp.
const BEAT_INTERVAL_MS: i64 = 60;

/// One decoded format as the reference keeps it in `InputFormat`
/// (`WaveImpl.h:96-98`): sample rate, channel count and total frames.
#[derive(Clone, Copy)]
struct SoundFormat {
    /// Samples per second.
    rate: u32,
    /// Channel count (1 or 2).
    channels: u16,
    /// Total sample frames.
    frames: u64,
}

/// `Sound` dispatch state, keyed by type inside
/// [`CompatServices::state`](crate::CompatServices::state).
#[derive(Default)]
pub struct SoundState {
    /// The single shared mixer (all objects play through it).
    mixer: Mixer,
    /// Live objects by id.
    objects: HashMap<u64, SoundObject>,
    /// Next object id (ids start at 1; 0 is the "not constructed" sentinel).
    next_id: u64,
    /// The L1 monotonic clock that drives the mixer in [`sound_poll`].
    clock: Clock,
    /// The installed decoder chain (see [`add_audio_decoder`]).
    decoders: Vec<AudioDecoder>,
    /// Live `PhaseVocoder` filters by `interface` token (the instance
    /// address), so `sound_play` can resolve the `filters` chain to DSP
    /// parameters. Entries are removed when the filter object is destroyed.
    vocoders: HashMap<usize, VocoderConfig>,
    /// The script-visible `globalFocusMode`, stored verbatim like the
    /// reference static (`WaveImpl.cpp:1889-1891`, `WaveIntf.cpp:1616-1618`
    /// — the reference casts and stores without clamping).
    global_focus_mode: i32,
}

/// Payload of one `WaveSoundBuffer` TJS object.
#[derive(Default)]
struct SoundInst {
    /// Registered object id (`None` until the constructor runs).
    id: Option<u64>,
}

// ---------------------------------------------------------------------------
// Marshalling helpers
// ---------------------------------------------------------------------------

/// Resolve the bound engine for a callback and return its dispatch context.
fn bound(
    engine: *mut c_void,
) -> Result<(crate::natives::system::EngineBinding, CompatCtx), String> {
    let resolved =
        resolve(engine).ok_or_else(|| "sound: compat context is not bound".to_string())?;
    let ctx = resolved.ctx();
    Ok((resolved, ctx))
}

/// TJS numeric coercion for the real-valued members (`fade`, `setPos`).
fn value_as_f64(v: &Value) -> f64 {
    match v.ty {
        VAL_INTEGER => v.integer as f64,
        VAL_REAL => v.real,
        VAL_STRING => value_as_string(v).parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Write a real result into `out`.
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

/// Write a retained id into `out` (the C++ trampoline consumes the
/// retention).
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
        (*out).retained = id as usize;
    }
}

/// Evaluate `expr` and return the resulting object, retained; `None` when the
/// expression did not yield an object.
fn eval_object(host: &Tjs2Engine, expr: &str, what: &str) -> Option<DetachedValue> {
    match host.eval_retained(expr, what) {
        Ok(RetainedValue::Object(object)) => Some(object),
        Ok(_) => None,
        Err(error) => {
            log::warn!("{what}: evaluation failed: {error}");
            None
        }
    }
}

/// Escape a `.sli` label name for a single-quoted TJS string literal.
fn escape_js(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' | '\'' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out
}

/// Build the `labels` dictionary expression (reference `WaveLabels`):
/// `{ name: { name, samplePosition, position(ms) } }`.
fn labels_expr(object: &SoundObject) -> String {
    let rate = object
        .source
        .as_ref()
        .map_or(0, |source| source.sample_rate());
    let mut out = String::from("(function(){var d=%[];");
    for (index, label) in object.buffer.labels().iter().enumerate() {
        if label.name.is_empty() {
            continue;
        }
        let name = escape_js(&label.name);
        let ms = if rate > 0 {
            label.position as i64 * 1000 / i64::from(rate)
        } else {
            0
        };
        out.push_str(&format!(
            "var e{index}=%[];e{index}.name='{name}';e{index}.samplePosition={};e{index}.position={ms};d['{name}']=e{index};",
            label.position
        ));
    }
    out.push_str("return d;})()");
    out
}

/// Access an object and the shared mixer together (both are disjoint fields
/// of [`SoundState`], so the closure gets independent mutable borrows).
fn with_object<R>(
    services: &CompatServices,
    id: u64,
    f: impl FnOnce(&mut SoundObject, &mut Mixer) -> R,
) -> Option<R> {
    let mut state = services.state::<SoundState>();
    let SoundState { mixer, objects, .. } = &mut *state;
    objects.get_mut(&id).map(|object| f(object, mixer))
}

/// Resolve the `filters` array's `interface` tokens to their live
/// `PhaseVocoder` configs, in array order.
///
/// Elements that are not `PhaseVocoder` objects (no `interface`, or an
/// unknown token) are skipped, matching the reference
/// `RebuildFilterChain`'s `continue` on a failed property read.
fn collect_vocoder_configs(
    host: &Tjs2Engine,
    filters: Tjs2ValueId,
    vocoders: &HashMap<usize, VocoderConfig>,
) -> Vec<VocoderConfig> {
    let count = match host.member(filters, "count") {
        Ok(TjsValue::Integer(count)) => count,
        _ => return Vec::new(),
    };
    let mut configs = Vec::new();
    for index in 0..count {
        let Ok(TjsValue::Object) = host.member(filters, &index.to_string()) else {
            continue;
        };
        let Ok(element) = host.retain_value_detached(&TjsValue::Object) else {
            continue;
        };
        if let Ok(TjsValue::Integer(handle)) = host.member(element.raw_id(), "interface")
            && let Some(config) = vocoders.get(&(handle as usize))
        {
            configs.push(*config);
        }
    }
    configs
}

/// Resolve the object's `filters` chain and render the decoded source through
/// it, returning `None` when there is no source or no live `PhaseVocoder`.
///
/// The DSP runs with the state borrow released (it allocates heavily).
fn resolve_filtered_source(
    host: &Tjs2Engine,
    services: &CompatServices,
    id: u64,
) -> Result<Option<Arc<SoundSource>>, String> {
    let (raw_source, filters) = {
        let state = services.state::<SoundState>();
        let Some(object) = state.objects.get(&id) else {
            return Err("not constructed".to_string());
        };
        (
            object.source.clone(),
            object.filters.as_ref().map(|filters| filters.raw_id()),
        )
    };
    let configs = match filters {
        Some(filters) => {
            let state = services.state::<SoundState>();
            collect_vocoder_configs(host, filters, &state.vocoders)
        }
        None => Vec::new(),
    };
    match (&raw_source, configs.is_empty()) {
        (Some(source), false) => phase_vocoder::apply_chain(source, &configs).map(Some),
        _ => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Reference fade state machine (`SoundBufferBaseIntf.cpp:104-193`)
// ---------------------------------------------------------------------------

/// Start a reference fade: `tTJSNI_BaseSoundBuffer::Fade`
/// (`SoundBufferBaseIntf.cpp:149-171`). `time` and `delay` must already be
/// validated (`time > 0`, `delay >= 0`, the caller's `TVPInvalidParam` check).
///
/// Returns the number of `onFadeCompleted` dispatches the caller must deliver
/// **synchronously**: interrupting a running fade posts one
/// (`StopFade(false, false)`, `SoundBufferBaseIntf.cpp:160-162`) and the
/// `FadeCount == 0 && BlankLeft == 0` instant completion posts another
/// (`SoundBufferBaseIntf.cpp:168-170`).
fn start_fade(
    object: &mut SoundObject,
    channel: &mut SoundChannel,
    to: i64,
    time: i64,
    delay: i64,
) -> u32 {
    let mut completions = 0;
    if object.fade.take().is_some() {
        // `if(InFading) StopFade(false, false);` — the volume is untouched.
        completions += 1;
    }
    let current = to_script_volume(channel.volume());
    // Reference: `DeltaVolume = (to - GetVolume()) * TVP_SB_BEAT_INTERVAL /
    // time; TargetVolume = to; FadeCount = time / TVP_SB_BEAT_INTERVAL;
    // BlankLeft = blanktime;` — integer arithmetic throughout.
    let fade = FadeState {
        to,
        delta: (to - current) * BEAT_INTERVAL_MS / time,
        count: time / BEAT_INTERVAL_MS,
        blank: delay,
    };
    if fade.count == 0 && fade.blank == 0 {
        // `if(FadeCount == 0 && BlankLeft == 0) StopFade(false, true);` —
        // the fade completes inside `Fade`, jumping straight to the target.
        channel.set_volume(from_script_volume(to));
        completions += 1;
    } else {
        object.fade = Some(fade);
    }
    completions
}

/// Reference `StopFade(false, true)` (`SoundBufferBaseIntf.cpp:174-193`):
/// only acts while fading — jumps to `TargetVolume` and reports completion.
/// Returns whether a fade was running (a no-op otherwise).
fn stop_fade(object: &mut SoundObject, channel: &mut SoundChannel) -> bool {
    let Some(fade) = object.fade.take() else {
        return false;
    };
    channel.set_volume(from_script_volume(fade.to));
    true
}

/// One reference fade beat — `tTJSNI_BaseSoundBuffer::TimerBeatHandler`
/// (`SoundBufferBaseIntf.cpp:104-128`): drain `BlankLeft`, then step
/// `FadeCount` times by `DeltaVolume`, clamped through `SetVolume`, and
/// finally complete with `StopFade(true, true)` (target volume).
///
/// Returns `true` when the beat completed the fade, so the caller can post
/// `onFadeCompleted` (`TVP_EPT_POST`, `SoundBufferBaseIntf.cpp:186-190`).
fn tick_fade(object: &mut SoundObject, channel: &mut SoundChannel) -> bool {
    let done = {
        let Some(fade) = object.fade.as_mut() else {
            return false;
        };
        if fade.blank > 0 {
            fade.blank = (fade.blank - BEAT_INTERVAL_MS).max(0);
            false
        } else if fade.count == 0 {
            // `time < 60` with a delay: `FadeCount` is 0, so the reference
            // never completes the fade (the `else if(FadeCount)` guard fails
            // forever) — it stays “fading” with a frozen volume.
            false
        } else if fade.count == 1 {
            true
        } else {
            fade.count -= 1;
            let volume = (to_script_volume(channel.volume()) + fade.delta).clamp(0, 100_000);
            channel.set_volume(from_script_volume(volume));
            false
        }
    };
    if done && let Some(fade) = object.fade.take() {
        channel.set_volume(from_script_volume(fade.to));
    }
    done
}

// ---------------------------------------------------------------------------
// Event delivery
// ---------------------------------------------------------------------------

/// Deliver one event through the object's class chain, so a script override
/// wins over the native member (reference: `TVPPostEvent(Owner, Owner, ...)` →
/// `Owner->FuncCall(<member>, ...)`, `SoundBufferBaseIntf.cpp:83-90`).
///
/// Failures are logged and never raised — the reference catches handler
/// exceptions per event (`EventIntf.cpp:224-226`,
/// `TVP_CATCH_AND_SHOW_SCRIPT_EXCEPTION`).
fn dispatch_member(
    host: &Tjs2Engine,
    owner: Option<Tjs2ValueId>,
    member: &'static str,
    args: &[TjsValue],
) {
    let Some(owner) = owner else {
        return;
    };
    if let Err(error) = host.call_member(owner, member, args) {
        log::warn!("WaveSoundBuffer: {member} handler failed: {error}");
    }
}

/// Poll `id`'s buffer **right now** and deliver the status transition
/// synchronously, like the reference `TVP_EPT_IMMEDIATE` `SetStatus`
/// (`SoundBufferBaseIntf.cpp:75-92`, delivered inline per
/// `EventIntf.cpp:217-228`).
///
/// A status change first cancels the events still queued for this object
/// (reference `TVPCancelSourceEvents(Owner)`, `SoundBufferBaseIntf.cpp:79-80`);
/// labels and natural fade completions stay queued for the next
/// [`sound_poll`], because the reference posted those with `TVP_EPT_POST`
/// (`WaveIntf.cpp:897-908`, `SoundBufferBaseIntf.cpp:186-190`).
fn drain_status_now(host: &Tjs2Engine, services: &CompatServices, id: u64) {
    let mut immediate: Vec<(&'static str, Vec<TjsValue>)> = Vec::new();
    let owner = {
        let mut state = services.state::<SoundState>();
        let SoundState { mixer, objects, .. } = &mut *state;
        let Some(object) = objects.get_mut(&id) else {
            return;
        };
        let owner = object.self_obj.as_ref().map(DetachedValue::raw_id);
        if let Some(channel) = mixer.channel(object.channel_id) {
            for event in object.buffer.poll(channel) {
                match event {
                    SoundEvent::StatusChanged(status) => {
                        object.pending.clear();
                        immediate.push((
                            "onStatusChanged",
                            vec![TjsValue::String(status.as_str().to_string())],
                        ));
                    }
                    SoundEvent::FadeCompleted => {
                        object.pending.push(("onFadeCompleted", Vec::new()));
                    }
                    SoundEvent::Label(name) => {
                        object
                            .pending
                            .push(("onLabel", vec![TjsValue::String(name)]));
                    }
                    _ => {}
                }
            }
        }
        owner
    };
    for (member, args) in immediate {
        dispatch_member(host, owner, member, &args);
    }
}

/// Volume/pan scale conversion for the script boundary.
fn to_script_volume(value: f32) -> i64 {
    (f64::from(value) * TVP_VOLUME_SCALE).round() as i64
}

fn from_script_volume(value: i64) -> f32 {
    (value as f64 / TVP_VOLUME_SCALE).clamp(0.0, 1.0) as f32
}

fn to_script_pan(value: f32) -> i64 {
    (f64::from(value) * TVP_VOLUME_SCALE).round() as i64
}

fn from_script_pan(value: i64) -> f32 {
    (value as f64 / TVP_VOLUME_SCALE).clamp(-1.0, 1.0) as f32
}

/// Resolve the bound context and the object id for a native callback (the
/// common prelude of every instance member).
macro_rules! with_sound {
    ($engine:expr, $instance:expr, $what:literal, $out_error:expr,
     |$ctx:ident, $services:ident, $id:ident| $body:block) => {{
        let Ok((_resolved, $ctx)) = bound($engine) else {
            return report_error(
                $out_error,
                concat!("WaveSoundBuffer.", $what, ": compat context is not bound"),
            );
        };
        let Some(inst) = (unsafe { ($instance as *const SoundInst).as_ref() }) else {
            return report_error(
                $out_error,
                concat!("WaveSoundBuffer.", $what, ": missing instance"),
            );
        };
        let Some($id) = inst.id else {
            return report_error(
                $out_error,
                concat!("WaveSoundBuffer.", $what, ": not constructed"),
            );
        };
        // SAFETY: the caller resolved the context from the installed binding.
        let $services = unsafe { $ctx.services() };
        $body
    }};
}

/// Get an integer property from the object's channel.
macro_rules! channel_int_getter {
    ($fn_name:ident, $what:literal, |$object:ident, $channel:ident| $body:expr) => {
        extern "C" fn $fn_name(
            engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            with_sound!(engine, instance, $what, out_error, |_ctx, services, id| {
                let value = with_object(services, id, |$object, mixer| {
                    let $channel = mixer.channel($object.channel_id);
                    $channel.map(|$channel| $body)
                })
                .flatten();
                set_int(out, value.unwrap_or(0));
                0
            })
        }
    };
}

/// Get a real property from the object's channel.
macro_rules! channel_real_getter {
    ($fn_name:ident, $what:literal, |$object:ident, $channel:ident| $body:expr) => {
        extern "C" fn $fn_name(
            engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            with_sound!(engine, instance, $what, out_error, |_ctx, services, id| {
                let value = with_object(services, id, |$object, mixer| {
                    let $channel = mixer.channel($object.channel_id);
                    $channel.map(|$channel| $body)
                })
                .flatten();
                set_real(out, value.unwrap_or(0.0));
                0
            })
        }
    };
}

/// Set a property through a closure over the channel.
macro_rules! channel_setter {
    ($fn_name:ident, $what:literal, $value:ident, $out_error:ident, |$channel:ident| $body:block) => {
        extern "C" fn $fn_name(
            engine: *mut c_void,
            instance: *mut c_void,
            $value: *const Value,
            $out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            with_sound!(engine, instance, $what, $out_error, |_ctx, services, id| {
                with_object(services, id, |object, mixer| {
                    if let Some($channel) = mixer.channel(object.channel_id) {
                        $body
                    }
                });
                0
            })
        }
    };
}

// ---------------------------------------------------------------------------
// Constructor / destructor
// ---------------------------------------------------------------------------

extern "C" fn sound_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(SoundInst::default())) as *mut c_void
}

/// Remove `id`'s object from the dispatch state: drop the mixer channel and
/// every retained value, and invalidate the cached `labels` dictionary — the
/// reference `Invalidate` chain (`WaveImpl.cpp:1960-1974` tears the device
/// down, `WaveIntf.cpp:885-894` recreates the labels object and releases
/// `Filters`, `SoundBufferBaseIntf.cpp:46-55` drops `Owner`/`ActionOwner`).
///
/// Returns whether an object was removed.
fn teardown(host: Option<&Tjs2Engine>, services: &CompatServices, id: u64) -> bool {
    let removed = {
        let mut state = services.state::<SoundState>();
        let SoundState { mixer, objects, .. } = &mut *state;
        objects.remove(&id).inspect(|object| {
            mixer.remove_channel(object.channel_id);
        })
    };
    let Some(object) = removed else {
        return false;
    };
    if let (Some(labels), Some(host)) = (&object.labels_obj, host) {
        // Reference `RecreateWaveLabelsObject` invalidates the dictionary so
        // scripts still holding it observe the invalidation too.
        if let Err(error) = host.invalidate_retained(labels.raw_id()) {
            log::warn!("WaveSoundBuffer: could not invalidate the labels object: {error}");
        }
    }
    // Dropping the object releases `self_obj`, `action_owner`, `filters`,
    // `flags_obj` and `labels_obj` (reference: `Filters->Release()`,
    // `ActionOwner.Release()`).
    drop(object);
    true
}

extern "C" fn sound_destroy(engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `instance` came from `sound_create` and is freed exactly once.
    let inst = unsafe { Box::from_raw(instance as *mut SoundInst) };
    let Some(id) = inst.id else {
        return;
    };
    let Some(resolved) = resolve(engine) else {
        // The binding is removed before the engine is destroyed, in which case
        // the state is dropped with the services anyway.
        return;
    };
    let ctx = resolved.ctx();
    // SAFETY: the resolved context is bound and alive.
    let services = unsafe { ctx.services() };
    teardown(Some(resolved.engine()), services, id);
}

/// The reference `Invalidate` chain for one `WaveSoundBuffer`
/// (`WaveImpl.cpp:1960-1974`, `WaveIntf.cpp:885-894`,
/// `SoundBufferBaseIntf.cpp:46-55`): playback stops (the mixer channel goes
/// away), every retained value is released, the cached `labels` dictionary is
/// invalidated, and — because the object id is cleared — no later poll
/// dispatches anything (`CanDeliverEvents = false`). The payload itself is
/// freed by [`sound_destroy`], exactly like the reference keeps the native
/// instance alive across `invalidate`.
extern "C" fn sound_invalidate(engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `instance` is the payload created by `sound_create`.
    let inst = unsafe { &mut *(instance as *mut SoundInst) };
    let Some(id) = inst.id.take() else {
        return;
    };
    let Some(resolved) = resolve(engine) else {
        return;
    };
    let ctx = resolved.ctx();
    // SAFETY: the resolved context is bound and alive.
    let services = unsafe { ctx.services() };
    teardown(Some(resolved.engine()), services, id);
}

extern "C" fn sound_ctor(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "WaveSoundBuffer: compat context is not bound");
    };
    let inst = unsafe { &mut *(instance as *mut SoundInst) };
    if inst.id.is_some() {
        return report_error(out_error, "WaveSoundBuffer: already constructed");
    }
    let host = resolved.engine();
    // Retain `this` so the poll can dispatch the event handlers through a
    // script subclass override; a script may drop it, which simply means no
    // events are delivered.
    let self_obj = host.retain_object_detached(objthis).ok();
    // Reference `ActionOwner = param[0]->AsObjectClosure()`
    // (`SoundBufferBaseIntf.cpp:44-45`): *any* first argument is accepted, a
    // non-object yields the empty closure — here `None` — which the native
    // event members never invoke. (The reference additionally raises
    // `TJS_E_BADPARAMCOUNT` for a missing argument,
    // `SoundBufferBaseIntf.cpp:32-33`; the port tolerates `argc == 0`, see
    // the module docs — test suites outside this module construct that way.)
    let raw_args = unsafe { args(argv, argc) };
    let action_owner = raw_args
        .first()
        .and_then(|value| host.retain_object_arg(value).ok());
    let filters = eval_object(host, "[]", "WaveSoundBuffer.filters");
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let id = {
        let mut state = services.state::<SoundState>();
        let id = state.next_id.max(1);
        state.next_id = id + 1;
        let channel_id = state.mixer.spawn_channel();
        state.objects.insert(
            id,
            SoundObject {
                buffer: SoundBuffer::new(),
                channel_id,
                source: None,
                format: None,
                self_obj,
                action_owner,
                filters,
                flags_obj: None,
                labels_obj: None,
                use_vis_buffer: false,
                frequency: 0,
                paused: false,
                device_ready: false,
                fade: None,
                fade_tick_ms: 0.0,
                pending: Vec::new(),
            },
        );
        id
    };
    inst.id = Some(id);
    set_int(out, id as i64);
    0
}

// ---------------------------------------------------------------------------
// Methods
// ---------------------------------------------------------------------------

extern "C" fn sound_open(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let raw_args = unsafe { args(argv, argc) };
    if raw_args.is_empty() {
        return report_error(
            out_error,
            "WaveSoundBuffer.open: a storage name is required",
        );
    }
    let name = value_as_string(&raw_args[0]);
    with_sound!(engine, instance, "open", out_error, |ctx, services, id| {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let host = unsafe { ctx.engine() };

        // Reference `Open` begins with `Clear()` (`WaveImpl.cpp:3041`): stop
        // playback, delete the loop manager (a *fresh* set of loop flags,
        // `WaveImpl.cpp:2358-2359`), `Paused = false` and
        // `SetStatus(ssUnload)` (`WaveImpl.cpp:2365,2369`) — even when a later
        // read fails, the buffer is left unloaded. The `Clear` status events
        // are `TVP_EPT_IMMEDIATE` (`SoundBufferBaseIntf.cpp:75-92`), so they
        // are dispatched inside this call.
        let mut immediate: Vec<(&'static str, Vec<TjsValue>)> = Vec::new();
        let owner;
        {
            let mut state = services.state::<SoundState>();
            let SoundState { mixer, objects, .. } = &mut *state;
            let Some(object) = objects.get_mut(&id) else {
                return report_error(out_error, "WaveSoundBuffer.open: not constructed");
            };
            owner = object.self_obj.as_ref().map(DetachedValue::raw_id);
            let was_loaded = object.source.is_some();
            let was_playing = mixer
                .channel(object.channel_id)
                .is_some_and(|channel| channel.status() == Status::Play);
            if let Some(channel) = mixer.channel(object.channel_id) {
                channel.stop();
                channel.set_position(0.0);
            }
            object.buffer.close();
            object.buffer.flags_mut().reset();
            object.source = None;
            object.paused = false;
            if let Some(channel) = mixer.channel(object.channel_id) {
                // Sync the status bookkeeping with the channel's stale state;
                // the transitional event this poll observes is deliberately
                // discarded — the manual stop/unload dispatch below is the
                // reference's `Clear()` output.
                let _ = object.buffer.poll(channel);
            }
            if was_loaded {
                // `Stop()` raises "stop" only when it was playing
                // (`WaveImpl.cpp:2989-2990`), then `SetStatus(ssUnload)`
                // raises "unload" — and each `SetStatus` first cancels the
                // queued events (`SoundBufferBaseIntf.cpp:79-80`).
                object.pending.clear();
                if was_playing {
                    immediate.push(("onStatusChanged", vec![TjsValue::String("stop".into())]));
                }
                immediate.push(("onStatusChanged", vec![TjsValue::String("unload".into())]));
            }
        }
        for (member, args) in &immediate {
            dispatch_member(host, owner, member, args);
        }

        let Some(storage) = services.storage() else {
            return report_error(out_error, "WaveSoundBuffer.open: no storage is attached");
        };
        let audio = match unsafe { &*storage }.read(&name) {
            Ok(bytes) => bytes,
            Err(error) => {
                return report_error(
                    out_error,
                    &format!("WaveSoundBuffer.open: cannot read {name:?}: {error}"),
                );
            }
        };
        let sli_bytes = unsafe { &*storage }.read(&format!("{name}.sli")).ok();
        // The *parse* result is carried past the decode: the reference
        // creates the decoder and assigns `InputFormat`/`Frequency` *before*
        // it parses the side-car (`WaveImpl.cpp:3043-3056`) and only then
        // throws `TVPInvalidLoopInformation` (`WaveImpl.cpp:3077-3080`),
        // which leaves the freshly assigned format in place (`Clear()` never
        // zeroes it, `WaveImpl.cpp:2352-2370`). A decoder failure therefore
        // reports first, exactly like the reference's `Create` throwing
        // before the side-car is ever read.
        let sli_parsed: Option<Result<LoopInfo, SliError>> =
            sli_bytes.as_ref().map(|bytes| parse_sli(bytes));
        let sli_info = sli_parsed
            .as_ref()
            .and_then(|parsed| parsed.as_ref().ok())
            .cloned();
        // Call the decoder outside the state borrow (it may allocate; the
        // closure must not re-enter `CompatServices::state`).
        let source = {
            let state = services.state::<SoundState>();
            if state.decoders.is_empty() {
                return report_error(
                    out_error,
                    "WaveSoundBuffer.open: no audio decoder is installed",
                );
            }
            let mut decoded = None;
            let mut errors: Vec<String> = Vec::new();
            for decoder in &state.decoders {
                match decoder(&audio, sli_info.clone(), Some(&name)) {
                    Ok(source) => {
                        decoded = Some(source);
                        break;
                    }
                    Err(error) => errors.push(error),
                }
            }
            match decoded {
                Some(source) => source,
                None => {
                    // Every decoder rejected the bytes: report *all* of them.
                    // A single one's message would hide the rest — e.g. the
                    // `wutcwf` TCWF mark error standing in for a `.ogg` the
                    // host decoder could have read.
                    let error = if errors.is_empty() {
                        "no decoder matched".to_string()
                    } else {
                        errors.join("; ")
                    };
                    return report_error(
                        out_error,
                        &format!("WaveSoundBuffer.open: cannot decode {name:?}: {error}"),
                    );
                }
            }
        };
        // `InputFormat = FilterOutput->GetFormat();
        //  Frequency = InputFormat.SamplesPerSec;` (`WaveImpl.cpp:3054-3056`).
        let format = SoundFormat {
            rate: source.sample_rate(),
            channels: source.channels(),
            frames: source.frames(),
        };
        let stored = with_object(services, id, |object, _mixer| {
            object.format = Some(format);
            object.frequency = i64::from(format.rate);
        });
        if stored.is_none() {
            return report_error(out_error, "WaveSoundBuffer.open: not constructed");
        }
        if let Some(Err(error)) = &sli_parsed {
            // Reference `TVPThrowExceptionMessage(TVPInvalidLoopInformation,
            // sliname)` + `Clear()` (`WaveImpl.cpp:3077-3080`): the buffer
            // stays unloaded — the clear phase above already ran — while the
            // just-assigned format survives.
            return report_error(
                out_error,
                &format!("WaveSoundBuffer.open: {name}.sli: {error}"),
            );
        }
        let installed = with_object(services, id, |object, mixer| {
            object.source = Some(source.clone());
            object.paused = false;
            object.buffer.open(source);
            if let Some(channel) = mixer.channel(object.channel_id) {
                // Load the source onto the channel so `status` reports
                // `"stop"` (the reference's post-`open` state) while keeping
                // it unstarted. `looping` is *not* reset: the reference
                // re-applies the instance flag to the fresh loop manager
                // (`WaveImpl.cpp:3049`).
                if object.buffer.play_on(channel).is_ok() {
                    channel.stop();
                    channel.set_position(0.0);
                }
            }
        });
        if installed.is_none() {
            return report_error(out_error, "WaveSoundBuffer.open: not constructed");
        }
        if sli_bytes.is_some() {
            // Reference `RecreateWaveLabelsObject()` after a successful `.sli`
            // read (`WaveImpl.cpp:3083`, `WaveIntf.cpp:910-918`): the cached
            // dictionary is invalidated so the next read rebuilds it from the
            // fresh loop manager. (A parse failure returned above, like the
            // reference, without recreating.)
            let cached = with_object(services, id, |object, _| object.labels_obj.take()).flatten();
            if let Some(labels) = cached
                && let Err(error) = host.invalidate_retained(labels.raw_id())
            {
                log::warn!("WaveSoundBuffer.open: could not invalidate the labels object: {error}");
            }
        }
        // Reference `SetStatus(ssStop)` at the end of `Open`
        // (`WaveImpl.cpp:3086`) — `TVP_EPT_IMMEDIATE`, delivered here.
        drain_status_now(host, services, id);
        set_void(out);
        0
    })
}

extern "C" fn sound_play(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_sound!(engine, instance, "play", out_error, |ctx, services, id| {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let host = unsafe { ctx.engine() };
        let filtered = match resolve_filtered_source(host, services, id) {
            Ok(filtered) => filtered,
            Err(error) => {
                return report_error(
                    out_error,
                    &format!("WaveSoundBuffer.play: filter chain failed: {error}"),
                );
            }
        };
        let result = with_object(services, id, |object, mixer| -> Result<bool, String> {
            let Some(channel) = mixer.channel(object.channel_id) else {
                return Err("the mixer channel is gone".to_string());
            };
            if object.source.is_none() {
                // Reference `Play()`: `if(!Decoder) return;`
                // (`WaveImpl.cpp:2958-2959`) — silent, no status change.
                return Ok(false);
            }
            if channel.status() == Status::Play {
                // `if(BufferPlaying) return;` (`WaveImpl.cpp:2960-2961`).
                // `BufferPlaying` is false again once a non-looping track ran
                // out (`WaveImpl.cpp:2658-2662`), so `play` after the end
                // restarts it. `channel.status()` mirrors the flag exactly:
                // a paused buffer still reports `play`.
                return Ok(false);
            }
            // `StartPlay` resumes from the loop manager's position —
            // `Stop()` rewound it to 0 (`WaveImpl.cpp:2993-2994`), a
            // non-looping end resets it (`WaveImpl.cpp:2661-2662`), but a
            // script seek performed before `play` survives ("play from first
            // or current position", `WaveImpl.cpp:2956-2957`).
            let seek = if channel.is_done() {
                0.0
            } else {
                channel.position_seconds()
            };
            object
                .buffer
                .play_on(channel)
                .map_err(|error| error.to_string())?;
            if let Some(filtered) = &filtered {
                // Replace the raw source with the filtered render while
                // keeping the label cursor/flags `play_on` just reset.
                channel.play(filtered.clone());
                object.buffer.apply_flags_to(channel);
            }
            channel.set_position(seek);
            if object.paused {
                // `StartPlay` does not start the device while paused
                // (`WaveImpl.cpp:2920-2925`).
                channel.pause();
            }
            // The reference's secondary buffer survives `Clear()`
            // (`WaveImpl.cpp:2352-2370` never destroys it), so
            // `GetSamplePosition`/`GetPosition` start reporting real values
            // from the first `play` on (`WaveImpl.cpp:3099-3100`).
            object.device_ready = true;
            Ok(true)
        });
        match result {
            None => report_error(out_error, "WaveSoundBuffer.play: not constructed"),
            Some(Err(error)) => report_error(out_error, &format!("WaveSoundBuffer.play: {error}")),
            Some(Ok(false)) => {
                set_void(out);
                0
            }
            Some(Ok(true)) => {
                // Reference `SetStatus(ssPlay)` is `TVP_EPT_IMMEDIATE`
                // (`WaveImpl.cpp:2976-2977`, delivered inline per
                // `EventIntf.cpp:217-228`).
                drain_status_now(host, services, id);
                set_void(out);
                0
            }
        }
    })
}

extern "C" fn sound_stop(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_sound!(engine, instance, "stop", out_error, |ctx, services, id| {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let host = unsafe { ctx.engine() };
        with_object(services, id, |object, mixer| {
            if let Some(channel) = mixer.channel(object.channel_id) {
                channel.stop();
                // The reference `Stop()` rewinds to the start
                // (`WaveImpl.cpp:2992-2994`). The fade state is deliberately
                // untouched: `Stop()` never cancels a fade
                // (`WaveImpl.cpp:2979-2995`).
                channel.set_position(0.0);
            }
        });
        // `if(Status != ssUnload) SetStatus(ssStop);`
        // (`WaveImpl.cpp:2989-2990`) — `TVP_EPT_IMMEDIATE`, delivered here;
        // an unloaded buffer raises nothing.
        drain_status_now(host, services, id);
        set_void(out);
        0
    })
}

extern "C" fn sound_fade(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let raw_args = unsafe { args(argv, argc) };
    if raw_args.len() < 2 {
        // `if(numparams < 2) return TJS_E_BADPARAMCOUNT;`
        // (`WaveIntf.cpp:1144-1145`).
        return report_error(out_error, "WaveSoundBuffer.fade(to, timeMs[, delayMs])");
    }
    // `to = (tjs_int)*param[0]; time = (tjs_int)*param[1];` and the delay
    // only counts when a third argument is present and not void
    // (`WaveIntf.cpp:1148-1153`).
    let to = value_as_i64(&raw_args[0]);
    let time = value_as_i64(&raw_args[1]);
    let delay = if raw_args.len() >= 3 && raw_args[2].ty != VAL_VOID {
        value_as_i64(&raw_args[2])
    } else {
        0
    };
    if time <= 0 || delay < 0 {
        // `if(time <= 0 || blanktime < 0) TVPThrowExceptionMessage(TVPInvalidParam);`
        // (`SoundBufferBaseIntf.cpp:155-156`), message "Invalid argument"
        // (`MsgIntfInc.h:15`).
        return report_error(out_error, "WaveSoundBuffer.fade: Invalid argument");
    }
    with_sound!(engine, instance, "fade", out_error, |ctx, services, id| {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let host = unsafe { ctx.engine() };
        let completions = with_object(services, id, |object, mixer| {
            match mixer.channel(object.channel_id) {
                Some(channel) => start_fade(object, channel, to, time, delay),
                None => 0,
            }
        });
        let Some(completions) = completions else {
            return report_error(out_error, "WaveSoundBuffer.fade: not constructed");
        };
        // Reference `Fade` only manipulates the fade state — it never starts
        // playback (`SoundBufferBaseIntf.cpp:149-171`) — and the
        // `StopFade` calls it makes along the way post `onFadeCompleted`
        // with `TVP_EPT_IMMEDIATE` (`SoundBufferBaseIntf.cpp:186-190`).
        let owner = with_object(services, id, |object, _| {
            object.self_obj.as_ref().map(DetachedValue::raw_id)
        })
        .flatten();
        for _ in 0..completions {
            dispatch_member(host, owner, "onFadeCompleted", &[]);
        }
        set_void(out);
        0
    })
}

extern "C" fn sound_stop_fade(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_sound!(
        engine,
        instance,
        "stopFade",
        out_error,
        |ctx, services, id| {
            // SAFETY: the caller resolved `ctx` from the installed binding.
            let host = unsafe { ctx.engine() };
            let stopped = with_object(services, id, |object, mixer| {
                let channel = mixer.channel(object.channel_id);
                channel.map(|channel| stop_fade(object, channel))
            });
            let Some(stopped) = stopped.flatten() else {
                return report_error(out_error, "WaveSoundBuffer.stopFade: not constructed");
            };
            if stopped {
                // `_this->StopFade(false, true)`: jump to the target volume
                // and post `onFadeCompleted` immediately
                // (`WaveIntf.cpp:1161-1169`, `SoundBufferBaseIntf.cpp:174-193`).
                // With no fade running (`if(InFading)`) it is a strict no-op —
                // no event, no volume write.
                let owner = with_object(services, id, |object, _| {
                    object.self_obj.as_ref().map(DetachedValue::raw_id)
                })
                .flatten();
                dispatch_member(host, owner, "onFadeCompleted", &[]);
            }
            set_void(out);
            0
        }
    )
}

extern "C" fn sound_set_pos(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let raw_args = unsafe { args(argv, argc) };
    if raw_args.len() < 3 {
        return report_error(out_error, "WaveSoundBuffer.setPos(x, y, z)");
    }
    let x = value_as_f64(&raw_args[0]) as f32;
    let y = value_as_f64(&raw_args[1]) as f32;
    let z = value_as_f64(&raw_args[2]) as f32;
    with_sound!(
        engine,
        instance,
        "setPos",
        out_error,
        |_ctx, services, id| {
            with_object(services, id, |object, mixer| {
                if let Some(channel) = mixer.channel(object.channel_id) {
                    channel.set_pos(x, y, z);
                }
            });
            set_void(out);
            0
        }
    )
}

extern "C" fn sound_get_vis_buffer(
    engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if bound(engine).is_err() {
        return report_error(
            out_error,
            "WaveSoundBuffer.getVisBuffer: compat context is not bound",
        );
    }
    if unsafe { args(argv, argc) }.len() < 3 {
        return report_error(
            out_error,
            "WaveSoundBuffer.getVisBuffer(dest, numsamples, channels[, ahead])",
        );
    }
    // L3 captures no visualization buffer (see the module docs): the reference
    // writes into a raw sample pointer that has no safe L2 counterpart.
    set_int(out, 0);
    0
}

// Event forwarders. The frame poll delivers the L2 events by name through the
// object's class chain, so a script override runs first; these native members
// exist so `super.onStatusChanged(...)` still resolves — and, like the
// reference natives, they re-invoke the constructor's action owner
// (`WaveIntf.cpp:1195-1241`).

/// Convert one callback argument into the event-member form. Objects are
/// retained so they can cross as [`TjsValue::Retained`] (the retention is
/// consumed while the variant copy is built); anything else unrepresentable
/// becomes `void`.
fn event_arg(host: &Tjs2Engine, value: &Value) -> (TjsValue, Option<DetachedValue>) {
    match value.ty {
        VAL_INTEGER => (TjsValue::Integer(value.integer), None),
        VAL_REAL => (TjsValue::Real(value.real), None),
        VAL_STRING => (TjsValue::String(value_as_string(value)), None),
        VAL_VOID => (TjsValue::Void, None),
        _ => match host.retain_object_arg(value) {
            Ok(retained) => (TjsValue::Retained(retained.raw_id() as u64), Some(retained)),
            Err(_) => (TjsValue::Void, None),
        },
    }
}

/// Build the reference action event dictionary
/// (`TVPCreateEventObject` + `TVP_ACTION_INVOKE_MEMBER`,
/// `EventIntf.cpp:709-736` and `EventIntf.h:208-225`): a dictionary carrying
/// `type` (the event name), `target` (the object the native was invoked on)
/// and, for `onStatusChanged`/`onLabel`, the first argument as `status`/`name`.
fn action_event(
    host: &Tjs2Engine,
    objthis: *mut c_void,
    event_type: &str,
    member: Option<(&str, TjsValue)>,
) -> Result<DetachedValue, String> {
    let event = match host.eval_retained("%[]", "WaveSoundBuffer.action") {
        Ok(RetainedValue::Object(event)) => event,
        Ok(RetainedValue::Value(value)) => {
            return Err(format!("event dictionary came back as {value:?}"));
        }
        Err(error) => return Err(format!("event dictionary creation failed: {error}")),
    };
    host.set_member(event.raw_id(), "type", &TjsValue::String(event_type.into()))?;
    if !objthis.is_null()
        && let Ok(target) = host.retain_object_detached(objthis)
    {
        // The retention is consumed while the variant copy is built; on
        // failure it is left to `DetachedValue`'s idempotent release.
        let target_id = target.raw_id() as u64;
        let _ = host.set_member(event.raw_id(), "target", &TjsValue::Retained(target_id));
        drop(target);
    }
    if let Some((name, value)) = member {
        host.set_member(event.raw_id(), name, &value)?;
    }
    Ok(event)
}

/// Shared body of the three native event members
/// (`WaveIntf.cpp:1195-1241` with `TVP_ACTION_INVOKE_*`, `EventIntf.h:208-234`):
///
/// * no action owner (the reference's empty `AsObjectClosure`) → nothing runs;
/// * otherwise the argument count is checked (`numparams < argnum` →
///   `TJS_E_BADPARAMCOUNT`, `EventIntf.h:213`) and
///   `actionOwner.action(event)` is invoked — its result is discarded, just
///   like the reference never inspects the `FuncCall` result.
///
/// The owner is taken out of the state across the VM call so a re-entrant
/// native sees consistent state, and restored afterwards unless the object
/// was invalidated meanwhile (its drop then releases the owner).
/// The raw arguments of one native member call, valid for the duration of the
/// call only.
struct CallArgs {
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
}

/// One reference action member: the action name, the event member copied from
/// the first argument (`TVP_ACTION_INVOKE_MEMBER`), and the minimum argument
/// count.
struct ActionSpec {
    /// The member name, which is also the event dictionary's `type` and the
    /// name used in error messages (`onStatusChanged`, `onFadeCompleted`,
    /// `onLabel`).
    what: &'static str,
    /// The event member taken from the first argument, when the action takes
    /// one.
    arg_member: Option<&'static str>,
    /// The reference `argnum` the call must supply.
    required_args: usize,
}

fn forward_action(call: CallArgs, spec: ActionSpec) -> c_int {
    let CallArgs {
        engine,
        instance,
        argc,
        argv,
        out,
        out_error,
        objthis,
    } = call;
    let ActionSpec {
        what,
        arg_member,
        required_args,
    } = spec;
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            &format!("WaveSoundBuffer.{what}: compat context is not bound"),
        );
    };
    let host = resolved.engine();
    // SAFETY: the dispatcher guarantees `instance` is the create payload.
    let Some(inst) = (unsafe { (instance as *const SoundInst).as_ref() }) else {
        return report_error(
            out_error,
            &format!("WaveSoundBuffer.{what}: missing instance"),
        );
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let Some(id) = inst.id else {
        // The constructor never completed, so the reference's `ActionOwner`
        // is null and the member still returns `TJS_S_OK`.
        set_void(out);
        return 0;
    };
    let action = with_object(services, id, |object, _| object.action_owner.take()).flatten();
    let Some(action) = action else {
        // `if(obj.Object)` (`WaveIntf.cpp:1197,1213,1227`): without an action
        // owner the native does nothing — and the argument count is never
        // checked either, because the check lives inside that guard.
        set_void(out);
        return 0;
    };
    let restore = |action: DetachedValue| {
        // When the object is gone, the closure is dropped without running and
        // releases the owner.
        let _ = with_object(services, id, |object, _| {
            object.action_owner = Some(action);
        });
    };
    let raw_args = unsafe { args(argv, argc) };
    if raw_args.len() < required_args {
        restore(action);
        return report_error(
            out_error,
            &format!("WaveSoundBuffer.{what}: needs {required_args} argument(s)"),
        );
    }
    let mut event_member = None;
    let mut member_retained = None;
    if let Some(name) = arg_member {
        let (value, retained) = event_arg(host, &raw_args[0]);
        event_member = Some((name, value));
        member_retained = retained;
    }
    let event = match action_event(host, objthis, what, event_member) {
        Ok(event) => event,
        Err(error) => {
            drop(member_retained);
            restore(action);
            return report_error(out_error, &format!("WaveSoundBuffer.{what}: {error}"));
        }
    };
    // The event dictionary crosses as `TjsValue::Retained`; the C++ side
    // consumes that retention while copying the argument, so dropping `event`
    // afterwards is an idempotent no-op.
    let event_id = event.raw_id() as u64;
    if let Err(error) = host.call_member(action.raw_id(), "action", &[TjsValue::Retained(event_id)])
    {
        // The reference never inspects this `FuncCall` result
        // (`EventIntf.h:227-234`): a plain object without an `action` member
        // and a throwing callback are both swallowed by the delivery seam.
        log::warn!("WaveSoundBuffer.{what}: action dispatch failed: {error}");
    }
    drop(event);
    drop(member_retained);
    restore(action);
    set_void(out);
    0
}

extern "C" fn sound_on_status_changed(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    // `TVP_ACTION_INVOKE_BEGIN(1, "onStatusChanged", objthis)` +
    // `TVP_ACTION_INVOKE_MEMBER("status")` (`WaveIntf.cpp:1195-1208`).
    forward_action(
        CallArgs {
            engine,
            instance,
            argc,
            argv,
            out,
            out_error,
            objthis,
        },
        ActionSpec {
            what: "onStatusChanged",
            arg_member: Some("status"),
            required_args: 1,
        },
    )
}

extern "C" fn sound_on_fade_completed(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    // `TVP_ACTION_INVOKE_BEGIN(0, "onFadeCompleted", objthis)` — no
    // argument, no count requirement (`WaveIntf.cpp:1210-1222`).
    forward_action(
        CallArgs {
            engine,
            instance,
            argc,
            argv,
            out,
            out_error,
            objthis,
        },
        ActionSpec {
            what: "onFadeCompleted",
            arg_member: None,
            required_args: 0,
        },
    )
}

extern "C" fn sound_on_label(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    // `TVP_ACTION_INVOKE_BEGIN(1, "onLabel", objthis)` +
    // `TVP_ACTION_INVOKE_MEMBER("name")` (`WaveIntf.cpp:1224-1241`).
    forward_action(
        CallArgs {
            engine,
            instance,
            argc,
            argv,
            out,
            out_error,
            objthis,
        },
        ActionSpec {
            what: "onLabel",
            arg_member: Some("name"),
            required_args: 1,
        },
    )
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

// Reference `GetPosition` / `GetSamplePosition`: `if(!Decoder) return 0;` and
// `if(!SoundBuffer) return 0;` (`WaveImpl.cpp:3147-3152`, `3097-3102`) —
// nothing is observable before a source is open *and* the first `play`
// created the device.
channel_int_getter!(sound_position_get, "position", |object, channel| {
    if object.source.is_none() || !object.device_ready {
        0
    } else {
        (channel.position_seconds() * 1000.0).round() as i64
    }
});

channel_int_getter!(
    sound_sample_position_get,
    "samplePosition",
    |object, channel| {
        if object.source.is_none() || !object.device_ready {
            0
        } else {
            let rate = object.source.as_ref().map_or(0, |s| s.sample_rate());
            (channel.position_seconds() * f64::from(rate)).round() as i64
        }
    }
);

// Reference `GetTotalTime` is `TotalSamples * 1000 / SamplesPerSec`
// (`WaveImpl.cpp:3162-3165`) off the *stored* `InputFormat`, which survives a
// failed `open`. With an unloaded (zeroed) format that is an integer division
// by zero — undefined behaviour the port cannot reproduce, so it reports
// `0` there.
channel_int_getter!(sound_total_time_get, "totalTime", |object, _channel| {
    object.format.map_or(0, |format| {
        if format.rate == 0 {
            0
        } else {
            (format.frames as f64 * 1000.0 / f64::from(format.rate)).round() as i64
        }
    })
});

channel_int_getter!(sound_looping_get, "looping", |_object, channel| {
    i64::from(channel.looping)
});

channel_int_getter!(sound_volume_get, "volume", |_object, channel| {
    to_script_volume(channel.volume())
});

channel_int_getter!(sound_volume2_get, "volume2", |_object, channel| {
    to_script_volume(channel.volume2())
});

channel_int_getter!(sound_pan_get, "pan", |_object, channel| {
    to_script_pan(channel.pan())
});

channel_real_getter!(sound_pos_x_get, "posX", |_object, channel| {
    f64::from(channel.pos()[0])
});

channel_real_getter!(sound_pos_y_get, "posY", |_object, channel| {
    f64::from(channel.pos()[1])
});

channel_real_getter!(sound_pos_z_get, "posZ", |_object, channel| {
    f64::from(channel.pos()[2])
});

extern "C" fn sound_paused_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_sound!(
        engine,
        instance,
        "paused",
        out_error,
        |_ctx, services, id| {
            let value = with_object(services, id, |object, _mixer| Some(object.paused)).flatten();
            set_int(out, i64::from(value.unwrap_or(false)));
            0
        }
    )
}

extern "C" fn sound_bits_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_sound!(engine, instance, "bits", out_error, |_ctx, services, id| {
        // The L2 pipeline is interleaved `f32`; there is no bit depth in
        // `AudioSpec`, so report the decoded sample width. The reference
        // reads `InputFormat.BitsPerSample`
        // (`WaveImpl.h:157-163`), which survives a failed `open`.
        let value = with_object(services, id, |object, _mixer| {
            Some(if object.format.is_some() { 32 } else { 0 })
        })
        .flatten();
        set_int(out, value.unwrap_or(0));
        0
    })
}

extern "C" fn sound_channels_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_sound!(
        engine,
        instance,
        "channels",
        out_error,
        |_ctx, services, id| {
            let value = with_object(services, id, |object, _mixer| {
                // Reference `GetChannels` reads `InputFormat.Channels`
                // (`WaveImpl.h:165-167`).
                Some(object.format.map_or(0, |format| i64::from(format.channels)))
            })
            .flatten();
            set_int(out, value.unwrap_or(0));
            0
        }
    )
}

extern "C" fn sound_frequency_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_sound!(
        engine,
        instance,
        "frequency",
        out_error,
        |_ctx, services, id| {
            // Reference `GetFrequency` returns the stored field verbatim
            // (`WaveImpl.h:230-232`): `0` before the first `open`, the
            // source rate after it (`WaveImpl.cpp:3056`), whatever the
            // script stored otherwise (`WaveImpl.cpp:3336-3342`).
            let value =
                with_object(services, id, |object, _mixer| Some(object.frequency)).flatten();
            set_int(out, value.unwrap_or(0));
            0
        }
    )
}

extern "C" fn sound_status_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_sound!(
        engine,
        instance,
        "status",
        out_error,
        |ctx, services, id| {
            let status = with_object(services, id, |object, mixer| {
                if object.source.is_none() {
                    // No decoder: the reference reports `ssUnload`
                    // (`SoundBufferBaseIntf.cpp:56-63`) regardless of the
                    // device's stale state.
                    return Some(Status::Unload);
                }
                mixer
                    .channel(object.channel_id)
                    .map(|channel| channel.status())
            })
            .flatten()
            .unwrap_or(Status::Unload);
            set_string_out(&ctx, out, status.as_str());
            0
        }
    )
}

extern "C" fn sound_use_vis_buffer_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_sound!(
        engine,
        instance,
        "useVisBuffer",
        out_error,
        |_ctx, services, id| {
            let value =
                with_object(services, id, |object, _mixer| Some(object.use_vis_buffer)).flatten();
            set_int(out, i64::from(value.unwrap_or(false)));
            0
        }
    )
}

extern "C" fn sound_flags_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_sound!(engine, instance, "flags", out_error, |ctx, services, id| {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let host = unsafe { ctx.engine() };
        // Reference `GetWaveFlagsObjectNoAddRef`: one proxy per buffer,
        // built from the owning buffer and cached.
        let cached = with_object(services, id, |object, _mixer| {
            object.flags_obj.as_ref().map(|flags| flags.raw_id())
        })
        .flatten();
        let raw = match cached {
            Some(raw) => raw,
            None => {
                let object = match eval_object(
                    host,
                    &format!("new WaveFlags({id})"),
                    "WaveSoundBuffer.flags",
                ) {
                    Some(object) => object,
                    None => {
                        return report_error(
                            out_error,
                            "WaveSoundBuffer.flags: could not build the proxy",
                        );
                    }
                };
                let raw = object.raw_id();
                let stored = with_object(services, id, |slot, _mixer| {
                    slot.flags_obj = Some(object);
                });
                if stored.is_none() {
                    return report_error(out_error, "WaveSoundBuffer.flags: not constructed");
                }
                raw
            }
        };
        match host.duplicate_retained(raw) {
            Ok(duplicate) => {
                set_retained_out(out, duplicate.raw_id());
                // The C++ conversion consumed the retention.
                std::mem::forget(duplicate);
                0
            }
            Err(error) => report_error(out_error, &format!("WaveSoundBuffer.flags: {error}")),
        }
    })
}

extern "C" fn sound_labels_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_sound!(
        engine,
        instance,
        "labels",
        out_error,
        |ctx, services, id| {
            // SAFETY: the caller resolved `ctx` from the installed binding.
            let host = unsafe { ctx.engine() };
            // Reference `GetWaveLabelsObjectNoAddRef` caches the dictionary
            // (`WaveIntf.cpp:1041-1043`): repeated reads yield the *same*
            // object until `RecreateWaveLabelsObject` invalidates it (a
            // successful `.sli` read, `WaveImpl.cpp:3083`, or `invalidate`).
            let cached = with_object(services, id, |object, _mixer| {
                object.labels_obj.as_ref().map(DetachedValue::raw_id)
            })
            .flatten();
            let raw = match cached {
                Some(raw) => raw,
                None => {
                    let expr =
                        with_object(services, id, |object, _mixer| Some(labels_expr(object)))
                            .flatten();
                    let Some(expr) = expr else {
                        return report_error(out_error, "WaveSoundBuffer.labels: not constructed");
                    };
                    let object = match eval_object(host, &expr, "WaveSoundBuffer.labels") {
                        Some(object) => object,
                        None => {
                            return report_error(
                                out_error,
                                "WaveSoundBuffer.labels: could not build the dictionary",
                            );
                        }
                    };
                    let raw = object.raw_id();
                    let stored = with_object(services, id, |slot, _mixer| {
                        slot.labels_obj = Some(object);
                    });
                    if stored.is_none() {
                        return report_error(out_error, "WaveSoundBuffer.labels: not constructed");
                    }
                    raw
                }
            };
            match host.duplicate_retained(raw) {
                Ok(duplicate) => {
                    set_retained_out(out, duplicate.raw_id());
                    // The C++ conversion consumed the retention.
                    std::mem::forget(duplicate);
                    0
                }
                Err(error) => report_error(out_error, &format!("WaveSoundBuffer.labels: {error}")),
            }
        }
    )
}

extern "C" fn sound_filters_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_sound!(
        engine,
        instance,
        "filters",
        out_error,
        |_ctx, services, id| {
            let Ok((resolved, _)) = bound(engine) else {
                return report_error(out_error, "WaveSoundBuffer.filters: not bound");
            };
            let host = resolved.engine();
            let filters = with_object(services, id, |object, _mixer| {
                object.filters.as_ref().map(|filters| filters.raw_id())
            })
            .flatten();
            match filters {
                Some(raw) => {
                    match host.duplicate_retained(raw) {
                        Ok(duplicate) => {
                            set_retained_out(out, duplicate.raw_id());
                            std::mem::forget(duplicate);
                        }
                        Err(error) => {
                            return report_error(
                                out_error,
                                &format!("WaveSoundBuffer.filters: {error}"),
                            );
                        }
                    }
                    0
                }
                None => report_error(out_error, "WaveSoundBuffer.filters: not constructed"),
            }
        }
    )
}

/// Reference `SetPosition`/`SetSamplePosition` (the setters that also touch
/// the [`SoundObject`] are written out explicitly — see
/// `WaveImpl.cpp:3131-3160`).
extern "C" fn sound_position_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let ms = value_as_i64(unsafe { &*value });
    with_sound!(
        engine,
        instance,
        "position",
        out_error,
        |_ctx, services, id| {
            with_object(services, id, |object, mixer| {
                // The reference casts through `tjs_uint64`
                // (`WaveIntf.cpp:1257-1263`) so a negative value wraps huge
                // and the `TotalSamples <= possamples` guard silently ignores
                // it (`WaveImpl.cpp:3131-3136`); a position at or past the
                // end is ignored the same way. With no decoder there is no
                // loop manager — the reference would dereference null, the
                // port skips the write and the getter reports 0 regardless
                // (`WaveImpl.cpp:3147-3152`).
                let Some(source) = object.source.as_ref() else {
                    return;
                };
                if ms < 0 {
                    return;
                }
                let seconds = ms as f64 / 1000.0;
                if seconds >= source.duration_seconds() {
                    return;
                }
                if let Some(channel) = mixer.channel(object.channel_id) {
                    channel.set_position(seconds);
                }
            });
            0
        }
    )
}

channel_setter!(sound_looping_set, "looping", value, out_error, |channel| {
    channel.looping = value_as_bool(unsafe { &*value });
});

channel_setter!(sound_volume_set, "volume", value, out_error, |channel| {
    channel.set_volume(from_script_volume(value_as_i64(unsafe { &*value })));
});

channel_setter!(sound_volume2_set, "volume2", value, out_error, |channel| {
    channel.set_volume2(from_script_volume(value_as_i64(unsafe { &*value })));
});

channel_setter!(sound_pan_set, "pan", value, out_error, |channel| {
    channel.set_pan(from_script_pan(value_as_i64(unsafe { &*value })));
});

// The setters that also touch `SoundObject` are written out explicitly.

extern "C" fn sound_sample_position_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let samples = value_as_i64(unsafe { &*value });
    with_sound!(
        engine,
        instance,
        "samplePosition",
        out_error,
        |_ctx, services, id| {
            with_object(services, id, |object, mixer| {
                // Reference `SetSamplePosition`: a negative value wraps huge
                // through `tjs_uint64` and the `TotalSamples <= possamples`
                // guard ignores it, as does a seek past the end
                // (`WaveImpl.cpp:3131-3136`, `WaveIntf.cpp:1275-1284`);
                // without a decoder there is no loop manager to seek.
                let Some(source) = object.source.as_ref() else {
                    return;
                };
                if samples < 0 || samples as u64 >= source.frames() {
                    return;
                }
                let rate = source.sample_rate();
                if rate > 0
                    && let Some(channel) = mixer.channel(object.channel_id)
                {
                    channel.set_position(samples as f64 / f64::from(rate));
                }
            });
            0
        }
    )
}

extern "C" fn sound_paused_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let paused = value_as_bool(unsafe { &*value });
    with_sound!(
        engine,
        instance,
        "paused",
        out_error,
        |_ctx, services, id| {
            with_object(services, id, |object, mixer| {
                object.paused = paused;
                if let Some(channel) = mixer.channel(object.channel_id) {
                    if paused {
                        channel.pause();
                    } else {
                        channel.resume();
                    }
                }
            });
            0
        }
    )
}

extern "C" fn sound_frequency_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // Reference `SetFrequency((tjs_int)*param)` stores verbatim — no clamp,
    // no reset sentinel (`WaveImpl.cpp:3336-3342`); the member has no
    // playback effect because `SetFrequencyToBuffer` is a no-op
    // (`WaveImpl.cpp:3329-3334`).
    let frequency = value_as_i64(unsafe { &*value });
    with_sound!(
        engine,
        instance,
        "frequency",
        out_error,
        |_ctx, services, id| {
            with_object(services, id, |object, _mixer| {
                object.frequency = frequency;
            });
            0
        }
    )
}

/// Reference `totalTime` setter: registered but marked "not yet implemented"
/// (`WaveIntf.cpp:1319-1330`) — the write succeeds and changes nothing (it
/// must not raise like the `TJS_DENY_NATIVE_PROP_SETTER` members).
extern "C" fn sound_total_time_set(
    engine: *mut c_void,
    instance: *mut c_void,
    _value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    with_sound!(
        engine,
        instance,
        "totalTime",
        out_error,
        |_ctx, _services, _id| { 0 }
    )
}

extern "C" fn sound_use_vis_buffer_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let enabled = value_as_bool(unsafe { &*value });
    with_sound!(
        engine,
        instance,
        "useVisBuffer",
        out_error,
        |_ctx, services, id| {
            with_object(services, id, |object, _mixer| {
                object.use_vis_buffer = enabled;
            });
            0
        }
    )
}

extern "C" fn sound_pos_x_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    sound_pos_component_set(engine, instance, value, out_error, 0)
}

extern "C" fn sound_pos_y_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    sound_pos_component_set(engine, instance, value, out_error, 1)
}

extern "C" fn sound_pos_z_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    sound_pos_component_set(engine, instance, value, out_error, 2)
}

fn sound_pos_component_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    axis: usize,
) -> c_int {
    let component = value_as_f64(unsafe { &*value }) as f32;
    with_sound!(engine, instance, "pos", out_error, |_ctx, services, id| {
        with_object(services, id, |object, mixer| {
            if let Some(channel) = mixer.channel(object.channel_id) {
                let mut pos = channel.pos();
                pos[axis] = component;
                channel.set_pos(pos[0], pos[1], pos[2]);
            }
        });
        0
    })
}

// ---------------------------------------------------------------------------
// Static members
// ---------------------------------------------------------------------------

extern "C" fn sound_global_volume_get(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "WaveSoundBuffer.globalVolume: not bound");
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let volume = services.state::<SoundState>().mixer.global_volume();
    set_int(out, to_script_volume(volume));
    0
}

extern "C" fn sound_global_volume_set(
    engine: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let volume = from_script_volume(value_as_i64(unsafe { &*value }));
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "WaveSoundBuffer.globalVolume: not bound");
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    services
        .state::<SoundState>()
        .mixer
        .set_global_volume(volume);
    0
}

extern "C" fn sound_global_focus_mode_get(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "WaveSoundBuffer.globalFocusMode: not bound");
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    // The reference getter just re-casts the stored static — no clamping
    // (`WaveIntf.cpp:1609-1613`, stored at `WaveImpl.cpp:1889-1891`).
    let mode = services.state::<SoundState>().global_focus_mode;
    set_int(out, i64::from(mode));
    0
}

extern "C" fn sound_global_focus_mode_set(
    engine: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // Reference `SetGlobalFocusMode((tTVPSoundGlobalFocusMode)(tjs_int)*param)`
    // stores whatever integer the script wrote (`WaveIntf.cpp:1616-1618`).
    let mode = value_as_i64(unsafe { &*value }) as i32;
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "WaveSoundBuffer.globalFocusMode: not bound");
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let mut state = services.state::<SoundState>();
    state.global_focus_mode = mode;
    // The L2 mixer only understands the three documented modes; out-of-range
    // values fall back to “never mute”, the reference's `switch(mode)`
    // behaviour of `SetVolumeToSoundBuffer` (no case matches → no muting).
    state
        .mixer
        .set_global_focus_mode(if (0..=2).contains(&mode) { mode } else { 0 });
    0
}

extern "C" fn sound_free_direct_sound(
    engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if bound(engine).is_err() {
        return report_error(
            out_error,
            "WaveSoundBuffer.freeDirectSound: compat context is not bound",
        );
    }
    // There is no DirectSound device in this port.
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// Per-frame poll
// ---------------------------------------------------------------------------

/// Advance the shared mixer by `dt` seconds and dispatch the events the L2
/// buffers observed. The state borrow is released before any script handler
/// runs, so a handler may call back into the natives.
fn advance_and_dispatch(ctx: &mut CompatCtx, dt: f64) -> Result<(), CompatError> {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let host = unsafe { ctx.engine() };
    // SAFETY: see above.
    let services = unsafe { ctx.services() };

    // Collect `(handler, args)` pairs with the borrow held, then release it
    // before touching the VM.
    let mut dispatches: Vec<(Tjs2ValueId, &'static str, Vec<TjsValue>)> = Vec::new();
    {
        let mut state = services.state::<SoundState>();
        let SoundState { mixer, objects, .. } = &mut *state;
        if dt > 0.0 {
            mixer.advance(dt);
        }
        for object in objects.values_mut() {
            let owner = object.self_obj.as_ref().map(DetachedValue::raw_id);
            // The reference's 60 ms sound-buffer beat drives the fade ramp
            // (`SoundBufferBaseIntf.cpp:104-128`); a natural completion is
            // posted with `TVP_EPT_POST` (`SoundBufferBaseIntf.cpp:186-190`)
            // and delivered with this frame's other queued events.
            if dt > 0.0 {
                object.fade_tick_ms += dt * 1000.0;
                let mut completions = 0u32;
                while object.fade_tick_ms >= BEAT_INTERVAL_MS as f64 {
                    object.fade_tick_ms -= BEAT_INTERVAL_MS as f64;
                    if object.fade.is_some()
                        && let Some(channel) = mixer.channel(object.channel_id)
                        && tick_fade(object, channel)
                    {
                        completions += 1;
                    }
                }
                for _ in 0..completions {
                    object.pending.push(("onFadeCompleted", Vec::new()));
                }
            }
            let Some(channel) = mixer.channel(object.channel_id) else {
                continue;
            };
            // Queued (`TVP_EPT_POST`) events first: they were posted before
            // anything this poll observes.
            if let Some(owner) = owner {
                for (member, args) in std::mem::take(&mut object.pending) {
                    dispatches.push((owner, member, args));
                }
            } else {
                object.pending.clear();
            }
            // Always poll — the watermark/status bookkeeping must advance
            // even when the script object could not be retained.
            let events = object.buffer.poll(channel);
            let Some(owner) = owner else {
                continue;
            };
            for event in events {
                match event {
                    SoundEvent::StatusChanged(status) => dispatches.push((
                        owner,
                        "onStatusChanged",
                        vec![TjsValue::String(status.as_str().to_string())],
                    )),
                    SoundEvent::FadeCompleted => {
                        dispatches.push((owner, "onFadeCompleted", Vec::new()));
                    }
                    SoundEvent::Label(name) => {
                        dispatches.push((owner, "onLabel", vec![TjsValue::String(name)]));
                    }
                    _ => {}
                }
            }
        }
    }

    for (id, member, args) in dispatches {
        dispatch_member(host, Some(id), member, &args);
    }
    Ok(())
}

/// The `sound_poll` step of the documented frame body (`compat::frame`).
///
/// Advances the shared mixer to the L1 monotonic clock and forwards the L2
/// playback events to the object's `onStatusChanged`/`onFadeCompleted`/
/// `onLabel` handlers. The host calls it once per frame.
pub fn sound_poll(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let dt = {
        let state = services.state::<SoundState>();
        let now = state.clock.now_ms() as f64 / 1000.0;
        // The mixer's internal clock starts at 0, so the first poll advances
        // by `now`; timestamps that move backwards advance by 0 (the same
        // rule as `Mixer::advance_to`).
        (now - state.mixer.clock()).max(0.0)
    };
    advance_and_dispatch(ctx, dt)
}

/// Render the mixer's current output into `out` (interleaved, `out_spec`).
///
/// This is the L3↔L2 side of the playback path: L1 owns the audio device,
/// L2 owns the mixing ([`Mixer::mix`]), and this is the thin adapter that
/// hands the composition root a buffer to `write` to the device. It advances
/// nothing — the frame body runs [`sound_poll`] first so the mixer tracks the
/// L1 clock, and this call only samples the current mix into `out`.
///
/// `out.len()` must be a whole number of frames (`len % channels == 0`); a
/// partial frame is a typed error rather than a truncated write.
pub fn sound_render(
    ctx: &CompatCtx,
    out: &mut [f32],
    out_spec: AudioSpec,
) -> Result<(), CompatError> {
    let channels = usize::from(out_spec.channels.max(1));
    if !out.len().is_multiple_of(channels) {
        return Err(CompatError::Engine(format!(
            "sound_render: {} samples is not a whole number of {channels}-channel frames",
            out.len()
        )));
    }
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let state = services.state::<SoundState>();
    state
        .mixer
        .mix(out, out_spec)
        .map_err(|error| CompatError::Engine(format!("sound_render: {error}")))
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the `WaveSoundBuffer` class.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "WaveSoundBuffer",
        create: sound_create,
        destroy: sound_destroy,
        invalidate: Some(sound_invalidate),
        methods: vec![
            NativeInstanceMethodDef {
                name: "WaveSoundBuffer",
                f: sound_ctor,
            },
            NativeInstanceMethodDef {
                name: "open",
                f: sound_open,
            },
            NativeInstanceMethodDef {
                name: "play",
                f: sound_play,
            },
            NativeInstanceMethodDef {
                name: "stop",
                f: sound_stop,
            },
            NativeInstanceMethodDef {
                name: "fade",
                f: sound_fade,
            },
            NativeInstanceMethodDef {
                name: "stopFade",
                f: sound_stop_fade,
            },
            NativeInstanceMethodDef {
                name: "setPos",
                f: sound_set_pos,
            },
            NativeInstanceMethodDef {
                name: "getVisBuffer",
                f: sound_get_vis_buffer,
            },
            NativeInstanceMethodDef {
                name: "onStatusChanged",
                f: sound_on_status_changed,
            },
            NativeInstanceMethodDef {
                name: "onFadeCompleted",
                f: sound_on_fade_completed,
            },
            NativeInstanceMethodDef {
                name: "onLabel",
                f: sound_on_label,
            },
        ],
        properties: vec![
            NativeInstancePropertyDef {
                name: "position",
                get: Some(sound_position_get),
                set: Some(sound_position_set),
            },
            NativeInstancePropertyDef {
                name: "samplePosition",
                get: Some(sound_sample_position_get),
                set: Some(sound_sample_position_set),
            },
            NativeInstancePropertyDef {
                name: "paused",
                get: Some(sound_paused_get),
                set: Some(sound_paused_set),
            },
            NativeInstancePropertyDef {
                name: "totalTime",
                get: Some(sound_total_time_get),
                set: Some(sound_total_time_set),
            },
            NativeInstancePropertyDef {
                name: "looping",
                get: Some(sound_looping_get),
                set: Some(sound_looping_set),
            },
            NativeInstancePropertyDef {
                name: "volume",
                get: Some(sound_volume_get),
                set: Some(sound_volume_set),
            },
            NativeInstancePropertyDef {
                name: "volume2",
                get: Some(sound_volume2_get),
                set: Some(sound_volume2_set),
            },
            NativeInstancePropertyDef {
                name: "pan",
                get: Some(sound_pan_get),
                set: Some(sound_pan_set),
            },
            NativeInstancePropertyDef {
                name: "posX",
                get: Some(sound_pos_x_get),
                set: Some(sound_pos_x_set),
            },
            NativeInstancePropertyDef {
                name: "posY",
                get: Some(sound_pos_y_get),
                set: Some(sound_pos_y_set),
            },
            NativeInstancePropertyDef {
                name: "posZ",
                get: Some(sound_pos_z_get),
                set: Some(sound_pos_z_set),
            },
            NativeInstancePropertyDef {
                name: "status",
                get: Some(sound_status_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "frequency",
                get: Some(sound_frequency_get),
                set: Some(sound_frequency_set),
            },
            NativeInstancePropertyDef {
                name: "bits",
                get: Some(sound_bits_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "channels",
                get: Some(sound_channels_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "flags",
                get: Some(sound_flags_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "labels",
                get: Some(sound_labels_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "filters",
                get: Some(sound_filters_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "useVisBuffer",
                get: Some(sound_use_vis_buffer_get),
                set: Some(sound_use_vis_buffer_set),
            },
        ],
    })?;

    ctx.register_static_members(&NativeStaticMembers {
        class_name: "WaveSoundBuffer",
        methods: vec![NativeMethodDef {
            name: "freeDirectSound",
            f: sound_free_direct_sound,
        }],
        properties: vec![
            NativePropertyDef {
                name: "globalVolume",
                get: Some(sound_global_volume_get),
                set: Some(sound_global_volume_set),
            },
            NativePropertyDef {
                name: "globalFocusMode",
                get: Some(sound_global_focus_mode_get),
                set: Some(sound_global_focus_mode_set),
            },
        ],
    })?;
    // `WaveSoundBuffer.flags` builds a `WaveFlags` proxy, so the class must
    // exist before a script reads `flags`.
    wave_flags::register(ctx)?;
    // The reference attaches the `PhaseVocoder` class to the `WaveSoundBuffer`
    // class object (`ScriptMgnIntf.cpp`), so scripts use
    // `new WaveSoundBuffer.PhaseVocoder()`. Register it under a hidden global
    // name and alias it onto the class, keeping the global namespace clean.
    phase_vocoder::register(ctx)?;
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    unsafe { ctx.engine() }
        .exec_script(
            "if(typeof WaveSoundBuffer.PhaseVocoder == \"undefined\"){\
                 WaveSoundBuffer.PhaseVocoder = __TvpPhaseVocoder;\
             }",
            "WaveSoundBuffer_PhaseVocoder",
        )
        .map_err(|error| {
            CompatError::Register(format!("register WaveSoundBuffer.PhaseVocoder: {error}"))
        })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CompatHost;
    use api::fs::{FsPath, OpenMode};
    use api::media::{AudioSpec, DecodedAudio};
    use engine::Storage;
    use fs::FileSystem;
    use std::f64::consts::PI;

    /// Serialize the tests that share the process-global VM binding (also
    /// shared with the other in-crate unit-test modules).
    fn vm_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::native_util::test_vm_lock()
    }

    /// A synthetic mono/stereo sine tone as an L2 source.
    fn tone(rate: u32, channels: u16, seconds: f64, info: Option<LoopInfo>) -> Arc<SoundSource> {
        let frames = (rate as f64 * seconds) as usize;
        let mut interleaved = Vec::with_capacity(frames * usize::from(channels));
        for frame in 0..frames {
            let value = (2.0 * PI * 440.0 * frame as f64 / f64::from(rate)).sin() as f32 * 0.5;
            for _ in 0..channels {
                interleaved.push(value);
            }
        }
        let spec = AudioSpec {
            sample_rate: rate,
            channels,
            total_frames: Some(frames as u64),
        };
        Arc::new(
            SoundSource::with_loops(Arc::new(DecodedAudio::new(spec, interleaved)), info)
                .expect("valid source"),
        )
    }

    /// An in-memory L2 storage populated with the given entries.
    fn mount_memory(files: &[(&str, &[u8])]) -> Storage {
        let fs = FileSystem::memory();
        for (path, data) in files {
            let path_obj = std::path::Path::new(path);
            if let Some(parent) = path_obj.parent()
                && !parent.as_os_str().is_empty()
            {
                fs::create_dir_all(&fs, &FsPath::host(parent)).expect("create parent dirs");
            }
            let file =
                fs::open(&fs, &FsPath::host(path), OpenMode::Write).expect("open memory file");
            fs::write_at(&fs, &file, 0, data).expect("write memory file");
            fs::flush(&fs, &file).expect("flush memory file");
        }
        Storage::mount_archives(fs, "/", [] as [&str; 0]).expect("mount")
    }

    /// A bound VM + host + attached in-memory storage with a stub decoder.
    ///
    /// Fields drop in declaration order: the host first (removing the binding
    /// and releasing every retained script object), then the engine, then the
    /// storage (which outlives the binding).
    struct Env {
        host: CompatHost,
        engine: Box<Tjs2Engine>,
        _storage: Box<Storage>,
    }

    impl Env {
        fn new(sli: Option<&str>) -> Env {
            let engine = Box::new(Tjs2Engine::new().expect("create engine"));
            let sli_bytes = sli.unwrap_or("").as_bytes();
            let mut storage = Box::new(mount_memory(&[
                ("tone.wav", b"RIFF"),
                ("tone.wav.sli", sli_bytes),
            ]));
            let mut host = CompatHost::new();
            // SAFETY: the boxed storage outlives the host (it is declared
            // after `host`, so it drops later), matching `attach_storage`.
            unsafe {
                host.services_mut()
                    .attach_storage(storage.as_mut() as *mut Storage);
            }
            host.install(&engine).expect("install compat surface");
            // A stub decoder: the L2 source is synthesized, ignoring the bytes
            // (the `.sli` parsed by `open` is still attached to it).
            add_audio_decoder(
                host.services(),
                Box::new(
                    |_bytes: &[u8], info: Option<LoopInfo>, _hint: Option<&str>| {
                        Ok(tone(44_100, 1, 1.0, info))
                    },
                ),
            );
            Env {
                host,
                engine,
                _storage: storage,
            }
        }

        fn run(&self, script: &str) {
            self.engine
                .exec_script(script, "sound-unit")
                .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
        }

        fn eval_int(&self, expression: &str) -> i64 {
            match self.engine.eval(expression, "sound-unit") {
                Ok(TjsValue::Integer(value)) => value,
                other => panic!("{expression:?} returned {other:?}, expected an integer"),
            }
        }

        fn eval_str(&self, expression: &str) -> String {
            match self.engine.eval(expression, "sound-unit") {
                Ok(TjsValue::String(value)) => value,
                other => panic!("{expression:?} returned {other:?}, expected a string"),
            }
        }

        /// Advance the shared mixer and dispatch events (the frame poll's
        /// core, without the wall-clock).
        fn advance(&self, dt: f64) {
            let mut ctx = self.host.ctx().expect("bound host");
            advance_and_dispatch(&mut ctx, dt).expect("advance");
        }
    }

    /// Install per-instance event handlers on a fresh `WaveSoundBuffer`; a
    /// native method can be shadowed by an own property, and the poll
    /// dispatches through the object's member lookup.
    const EVENT_SCRIPT: &str = "\
        var events = [];\
        var s = new WaveSoundBuffer();\
        s.onStatusChanged = function(status) { events.add('status:' + status); };\
        s.onFadeCompleted = function() { events.add('fade'); };\
        s.onLabel = function(name) { events.add('label:' + name); };\
        ";

    #[test]
    fn open_reports_metadata_and_the_stop_status() {
        let _lock = vm_lock();
        let env = Env::new(None);
        env.run("var s = new WaveSoundBuffer(); s.open('tone.wav');");
        assert_eq!(env.eval_int("s.totalTime"), 1000, "1 s = 1000 ms");
        assert_eq!(env.eval_int("s.channels"), 1);
        assert_eq!(env.eval_int("s.bits"), 32, "the engine pipeline is f32");
        assert_eq!(env.eval_int("s.frequency"), 44_100);
        // `status` derives from the loaded (unstarted) channel, so `open`
        // already reports `stop` (the reference's post-open state).
        assert_eq!(env.eval_str("s.status"), "stop");
        env.advance(0.0);
        assert_eq!(env.eval_str("s.status"), "stop");
    }

    #[test]
    fn play_advances_and_fires_status_events() {
        let _lock = vm_lock();
        let env = Env::new(None);
        env.run(EVENT_SCRIPT);
        env.run("s.open('tone.wav'); s.play();");
        env.advance(0.4);
        assert_eq!(env.eval_int("s.position"), 400);
        assert_eq!(env.eval_str("s.status"), "play");
        // Reference `SetStatus` is `TVP_EPT_IMMEDIATE` (`WaveImpl.cpp:3086`,
        // `2976-2977`): `open` raises "stop" and `play` raises "play"
        // *inside the call*, so both are queued before the first poll.
        assert_eq!(env.eval_int("events.count"), 2);
        assert_eq!(env.eval_str("events[0]"), "status:stop");
        assert_eq!(env.eval_str("events[1]"), "status:play");
        // Finish the non-looping track; the next poll reports `stop`.
        env.advance(0.7);
        assert_eq!(env.eval_str("s.status"), "stop");
        assert_eq!(env.eval_str("events[events.count - 1]"), "status:stop");
    }

    #[test]
    fn play_while_paused_does_not_restart() {
        let _lock = vm_lock();
        let env = Env::new(None);
        env.run("var s = new WaveSoundBuffer(); s.open('tone.wav'); s.paused = true; s.play();");
        env.advance(0.3);
        assert_eq!(env.eval_int("s.position"), 0, "paused does not advance");
        assert_eq!(env.eval_int("s.paused"), 1);
        env.run("s.paused = false;");
        env.advance(0.2);
        assert_eq!(env.eval_int("s.position"), 200);
        // `play` while started is a no-op and keeps the position.
        env.run("s.play();");
        assert_eq!(env.eval_int("s.position"), 200);
    }

    #[test]
    fn fade_completion_fires_on_fade_completed() {
        let _lock = vm_lock();
        let env = Env::new(None);
        env.run(EVENT_SCRIPT);
        env.run("s.open('tone.wav'); s.play();");
        // `time = 50` is below the 60 ms beat: the reference completes the
        // fade *inside* `Fade` (`SoundBufferBaseIntf.cpp:168-170`) — volume
        // jumps to the target and `onFadeCompleted` is posted before `fade`
        // returns.
        env.run("s.fade(0, 50);");
        assert_eq!(env.eval_str("events[events.count - 1]"), "fade");
        assert_eq!(env.eval_int("s.volume"), 0);
        // A later poll raises nothing new for that fade.
        env.advance(0.08);
        assert_eq!(env.eval_str("events[events.count - 1]"), "fade");
        assert_eq!(env.eval_int("s.volume"), 0);
    }

    #[test]
    fn sli_labels_fire_on_label() {
        let _lock = vm_lock();
        let env = Env::new(Some("#2.00\nLabel { Position=8820; Name='mid'; }\n"));
        env.run(EVENT_SCRIPT);
        env.run("s.open('tone.wav'); s.play();");
        // 8820 granules at 44.1 kHz is 0.2 s; the label also appears in the
        // `labels` dictionary.
        assert_eq!(env.eval_int("s.labels.mid.samplePosition"), 8820);
        assert_eq!(env.eval_int("s.labels.mid.position"), 200);
        env.advance(0.25);
        assert_eq!(env.eval_str("events[events.count - 1]"), "label:mid");
    }

    #[test]
    fn looping_keeps_the_buffer_playing() {
        let _lock = vm_lock();
        let env = Env::new(None);
        env.run("var s = new WaveSoundBuffer(); s.open('tone.wav'); s.looping = true; s.play();");
        env.advance(2.5);
        assert_eq!(env.eval_str("s.status"), "play");
        assert!(env.eval_int("s.position") < 1000);
    }

    #[test]
    fn phase_vocoder_filter_time_stretches_playback() {
        let _lock = vm_lock();
        let env = Env::new(None);
        // A 1 s tone stretched 2x is still playing at 1.5 s; without the
        // filter it would have stopped.
        env.run(
            "var s = new WaveSoundBuffer(); s.open('tone.wav'); \
             var pv = new WaveSoundBuffer.PhaseVocoder(); pv.time = 2.0; \
             s.filters.add(pv); s.play();",
        );
        env.advance(1.5);
        assert_eq!(env.eval_str("s.status"), "play");
    }

    #[test]
    fn phase_vocoder_dsp_time_stretches_and_pitch_shifts() {
        use phase_vocoder::{PhaseVocoderDsp, VocoderConfig};

        let rate = 44_100u32;
        let frames = rate as usize;
        let input: Vec<f32> = (0..frames)
            .map(|i| (2.0 * PI * 440.0 * i as f64 / f64::from(rate)).sin() as f32 * 0.5)
            .collect();

        // time = 2 -> roughly twice the output frames.
        let mut stretch = PhaseVocoderDsp::new(
            rate,
            1,
            VocoderConfig {
                window: 4096,
                overlap: 0,
                pitch: 1.0,
                time: 2.0,
            },
        );
        let stretched = stretch.process(&input);
        assert!(
            stretched.len() > frames * 3 / 2 && stretched.len() < frames * 5 / 2,
            "unexpected stretched length: {}",
            stretched.len()
        );
        assert!(stretched.iter().all(|sample| sample.is_finite()));

        // Identity: pitch = 1, time = 1 should reconstruct the 440 Hz tone.
        let mut identity = PhaseVocoderDsp::new(
            rate,
            1,
            VocoderConfig {
                window: 4096,
                overlap: 0,
                pitch: 1.0,
                time: 1.0,
            },
        );
        let ident = identity.process(&input);
        let ident_freq = dominant_frequency(&ident, rate);
        assert!(
            (ident_freq - 440.0).abs() < 80.0,
            "identity expected ~440 Hz, got {ident_freq}"
        );

        // pitch = 2 -> the dominant frequency roughly doubles.
        let mut shift = PhaseVocoderDsp::new(
            rate,
            1,
            VocoderConfig {
                window: 4096,
                overlap: 0,
                pitch: 2.0,
                time: 1.0,
            },
        );
        let shifted = shift.process(&input);
        assert!(!shifted.is_empty());
        let frequency = dominant_frequency(&shifted, rate);
        assert!(
            (frequency - 880.0).abs() < 120.0,
            "expected ~880 Hz, got {frequency}"
        );
    }

    /// Dominant frequency estimate via zero-crossing rate (mono).
    fn dominant_frequency(samples: &[f32], rate: u32) -> f64 {
        let crossings = samples
            .windows(2)
            .filter(|pair| (pair[0] < 0.0) != (pair[1] < 0.0))
            .count();
        let seconds = samples.len() as f64 / f64::from(rate);
        crossings as f64 / 2.0 / seconds
    }

    #[test]
    fn sli_parser_reads_v2_and_old() {
        let text = b"#2.00\n# Sound Loop Information\nLink { From=6821039; To=1242931; Smooth=False; Condition=no; RefValue=0; CondVar=0; }\n";
        let info = parse_sli(text).expect("parse v2");
        assert_eq!(info.links.len(), 1);
        let link = info.active_link().expect("active link");
        assert_eq!(link.from, 6_821_039);
        assert_eq!(link.to, 1_242_931);
        assert_eq!(link.condition, LoopCondition::None);

        let info = parse_sli(b"LoopStart=1000\nLoopLength=500\n").expect("parse old");
        let link = info.active_link().unwrap();
        assert_eq!(link.from, 1500);
        assert_eq!(link.to, 1000);

        // Labels and conditions round-trip.
        let text = b"#2.00\nLabel { Position=42; Name='chorus'; }\nLink { From=100; To=10; Smooth=True; Condition=eq; RefValue=1; CondVar=2; }\n";
        let info = parse_sli(text).expect("parse labels");
        assert_eq!(info.labels.len(), 1);
        assert_eq!(info.labels[0].position, 42);
        assert_eq!(info.labels[0].name, "chorus");
        assert_eq!(info.links[0].condition, LoopCondition::Equal);
        assert!(info.active_link().is_none(), "conditional is not default");

        // Empty/comment-only files are empty, not errors.
        assert!(parse_sli(b"").unwrap().links.is_empty());
        assert!(parse_sli(b"# comment\n").unwrap().links.is_empty());
    }

    #[test]
    fn sli_parser_rejects_malformed_input() {
        let error = parse_sli(b"#2.00\nBogus { From=1; }\n").expect_err("bogus token");
        assert!(matches!(error, SliError::Invalid { .. }));
        assert!(
            error.to_string().contains("Bogus"),
            "unexpected message: {error}"
        );

        let error = parse_sli(b"#2.00\nLink { From=1; To=2; Condition=nope; }\n")
            .expect_err("unknown condition");
        assert!(
            error.to_string().contains("Condition"),
            "unexpected: {error}"
        );
    }
}
