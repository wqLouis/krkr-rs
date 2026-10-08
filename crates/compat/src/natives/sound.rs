//! `WaveSoundBuffer` native class.
//!
//! Behavioral reference: `main`'s `crates/tvp-sound` (`wavesound.rs` +
//! `natives.rs`) and the reference C++
//! `reference/cpp/core/sound/WaveIntf.cpp` +
//! `reference/cpp/core/sound/win32/WaveImpl.cpp`. This is a reimplementation
//! on the layered API: the class composes the L2 [`engine::sound`] model and
//! mixer and owns no process global — the one shared [`Mixer`] and the
//! per-object [`SoundBuffer`]/[`SoundChannel`]/[`SoundSource`] handles live in
//! [`CompatServices::state`] (doc 08 §3.1).
//!
//! # Members
//!
//! The reference surface (33 members, `docs/native_parity.md`) is registered:
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
//! `compat`), so the host installs one with [`set_audio_decoder`]. `open`
//! reads the audio entry and its `<name>.sli` side-car through the attached L2
//! [`Storage`](engine::Storage), parses the side-car with the module's
//! [`parse_sli`], and hands both to the decoder — a typed error
//! (D11) when no decoder or no storage is installed. The decoder returns a
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
//! # Deliberately deferred
//!
//! * `bits` reports `32` for a loaded source (the L2 pipeline is `f32`);
//!   `AudioSpec` carries no bit depth, so the reference's 16-bit WAV value
//!   cannot be reproduced truthfully.
//! * `getVisBuffer` returns `0`: L3 captures no visualization buffer (the
//!   reference writes into a raw sample pointer, which has no safe L2
//!   counterpart). `useVisBuffer` still round-trips its stored flag.
//! * `flags` returns a snapshot TJS array of the 16 conditional-loop slots
//!   and `labels` a dictionary of the loaded `.sli` labels; neither is a live
//!   proxy object (the reference's `WaveFlags`/`WaveLabels` helper classes are
//!   not registered by the Rust port and are tracked separately in
//!   `docs/native_parity.md`).
//! * `filters` is a stable, always-empty TJS array: the L2 mixer has no filter
//!   chain, so there is nothing truthful to insert.
//! * `frequency` is applied as the channel's playback-rate multiplier
//!   (`frequency / source_rate`), matching the reference's real behaviour.
//! * `freeDirectSound` is a no-op — there is no DirectSound in this port.

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};
use std::sync::Arc;

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

// ---------------------------------------------------------------------------
// TVP `.sli` side-car reader (moved out of L2; the engine model is generic)
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
/// installs it with [`set_audio_decoder`]. `hint` is the storage name, used
/// only for backend selection.
pub type AudioDecoder =
    Box<dyn Fn(&[u8], Option<LoopInfo>, Option<&str>) -> Result<Arc<SoundSource>, String>>;

/// Install the host's audio decoder into the per-engine sound state.
///
/// The L4 host calls this once at startup; the `compat` integration tests that
/// cannot link a codec leave it unset, and `open` then reports a typed error.
#[allow(dead_code)]
pub fn set_audio_decoder(services: &CompatServices, decoder: AudioDecoder) {
    services.state::<SoundState>().decoder = Some(decoder);
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
#[allow(dead_code)]
struct SoundObject {
    /// L2 script-object half: loaded source, loop flags, label bookkeeping.
    buffer: SoundBuffer,
    /// The mixer channel this object drives.
    channel_id: u64,
    /// The loaded decoded source, kept beside the buffer for the
    /// `frequency`/`channels`/`bits` translation.
    source: Option<Arc<SoundSource>>,
    /// Retained `objthis` (the `WaveSoundBuffer` or a script subclass); the
    /// poll delivers events through its class chain.
    self_obj: Option<DetachedValue>,
    /// Stable `filters` array (reference `GetFiltersNoAddRef`).
    filters: Option<DetachedValue>,
    /// `useVisBuffer` (stored; see the module docs).
    use_vis_buffer: bool,
    /// `frequency` override in Hz (`None` = the source's native rate).
    frequency: Option<u32>,
    /// `paused` is settable before `play` (the reference keeps the flag on the
    /// buffer, not the device).
    paused: bool,
    /// True once `play`/`open` put the source on the channel; `play` while
    /// already started is a no-op (reference `BufferPlaying`).
    started: bool,
}

/// `Sound` dispatch state, keyed by type inside
/// [`CompatServices::state`](crate::CompatServices::state).
#[allow(dead_code)]
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
    /// The host decoder (see [`set_audio_decoder`]).
    decoder: Option<AudioDecoder>,
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

/// Hand a retained object to a native property getter (duplicated so the
/// cached handle stays live).
fn set_object_out(host: &Tjs2Engine, out: *mut Value, object: Option<&DetachedValue>) {
    let Some(object) = object else {
        set_void(out);
        return;
    };
    match host.duplicate_retained(object.raw_id()) {
        Ok(duplicate) => {
            set_retained_out(out, duplicate.raw_id());
            // The C++ conversion consumed the retention.
            std::mem::forget(duplicate);
        }
        Err(error) => {
            log::warn!("WaveSoundBuffer: could not duplicate a retained object: {error}");
            set_void(out);
        }
    }
}

/// Read one flag (`LoopFlags::count` is always `LOOP_FLAG_COUNT`).
fn flags_expr(buffer: &SoundBuffer) -> String {
    let flags = buffer.flags();
    let mut out = String::from("[");
    for index in 0..flags.count() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&flags.get(index).to_string());
    }
    out.push(']');
    out
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

/// Apply the stored `frequency` override as the channel's playback-rate
/// multiplier.
fn apply_frequency(object: &SoundObject, channel: &mut SoundChannel) {
    let Some(source) = &object.source else {
        return;
    };
    let rate = match object.frequency {
        Some(hz) if source.sample_rate() > 0 => f64::from(hz) / f64::from(source.sample_rate()),
        _ => 1.0,
    };
    channel.set_rate(rate);
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
    let mut state = services.state::<SoundState>();
    if let Some(object) = state.objects.remove(&id) {
        let SoundState { mixer, .. } = &mut *state;
        mixer.remove_channel(object.channel_id);
    }
}

extern "C" fn sound_ctor(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
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
                self_obj,
                filters,
                use_vis_buffer: false,
                frequency: None,
                paused: false,
                started: false,
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
        let sli = match unsafe { &*storage }.read(&format!("{name}.sli")) {
            Ok(bytes) => match parse_sli(&bytes) {
                Ok(info) => Some(info),
                Err(error) => {
                    return report_error(
                        out_error,
                        &format!("WaveSoundBuffer.open: {name}.sli: {error}"),
                    );
                }
            },
            Err(_) => None,
        };
        // Call the decoder outside the state borrow (it may allocate; the
        // closure must not re-enter `CompatServices::state`).
        let source = {
            let state = services.state::<SoundState>();
            let Some(decoder) = state.decoder.as_ref() else {
                return report_error(
                    out_error,
                    "WaveSoundBuffer.open: no audio decoder is installed",
                );
            };
            match decoder(&audio, sli, Some(&name)) {
                Ok(source) => source,
                Err(error) => {
                    return report_error(
                        out_error,
                        &format!("WaveSoundBuffer.open: cannot decode {name:?}: {error}"),
                    );
                }
            }
        };
        let installed = with_object(services, id, |object, mixer| {
            object.source = Some(source.clone());
            object.frequency = None;
            object.paused = false;
            object.started = false;
            object.buffer.open(source);
            if let Some(channel) = mixer.channel(object.channel_id) {
                // Load the source onto the channel so `status` reports
                // `"stop"` (the reference's post-`open` state) while keeping
                // it unstarted.
                if object.buffer.play_on(channel).is_ok() {
                    channel.stop();
                    channel.set_position(0.0);
                }
                channel.set_looping(false);
            }
        });
        if installed.is_none() {
            return report_error(out_error, "WaveSoundBuffer.open: not constructed");
        }
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
    with_sound!(engine, instance, "play", out_error, |_ctx, services, id| {
        let result = with_object(services, id, |object, mixer| -> Result<(), String> {
            let Some(channel) = mixer.channel(object.channel_id) else {
                return Err("the mixer channel is gone".to_string());
            };
            if !object.started {
                if object.source.is_none() {
                    // The reference `Play()` returns when no decoder is open.
                    return Ok(());
                }
                object
                    .buffer
                    .play_on(channel)
                    .map_err(|error| error.to_string())?;
                object.started = true;
                apply_frequency(object, channel);
            }
            if object.paused {
                channel.pause();
            }
            Ok(())
        });
        match result {
            None => report_error(out_error, "WaveSoundBuffer.play: not constructed"),
            Some(Err(error)) => report_error(out_error, &format!("WaveSoundBuffer.play: {error}")),
            Some(Ok(())) => {
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
    with_sound!(engine, instance, "stop", out_error, |_ctx, services, id| {
        with_object(services, id, |object, mixer| {
            object.started = false;
            if let Some(channel) = mixer.channel(object.channel_id) {
                channel.stop();
                // The reference `Stop()` rewinds to the start.
                channel.set_position(0.0);
            }
        });
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
        return report_error(out_error, "WaveSoundBuffer.fade(to, timeMs[, delayMs])");
    }
    let to = value_as_i64(&raw_args[0]) as f64 / TVP_VOLUME_SCALE;
    let duration = value_as_i64(&raw_args[1]).max(0) as f64 / 1000.0;
    let delay = if raw_args.len() >= 3 && raw_args[2].ty != VAL_VOID {
        value_as_i64(&raw_args[2]).max(0) as f64 / 1000.0
    } else {
        0.0
    };
    with_sound!(engine, instance, "fade", out_error, |_ctx, services, id| {
        let result = with_object(services, id, |object, mixer| -> Result<(), String> {
            let Some(channel) = mixer.channel(object.channel_id) else {
                return Err("the mixer channel is gone".to_string());
            };
            if !object.started {
                if object.source.is_none() {
                    // The reference `Fade()` plays only a loaded buffer.
                    return Ok(());
                }
                object
                    .buffer
                    .play_on(channel)
                    .map_err(|error| error.to_string())?;
                object.started = true;
                apply_frequency(object, channel);
            }
            object.paused = false;
            channel.fade(to.clamp(0.0, 1.0) as f32, duration, delay);
            Ok(())
        });
        match result {
            None => report_error(out_error, "WaveSoundBuffer.fade: not constructed"),
            Some(Err(error)) => report_error(out_error, &format!("WaveSoundBuffer.fade: {error}")),
            Some(Ok(())) => {
                set_void(out);
                0
            }
        }
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
        |_ctx, services, id| {
            with_object(services, id, |object, mixer| {
                if let Some(channel) = mixer.channel(object.channel_id) {
                    // Cancelling the fade while keeping the current effective
                    // value (`set_volume` clears the fade; reference
                    // `StopFade(setvolume)`).
                    let current = channel.effective_volume();
                    channel.set_volume(current);
                }
            });
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
// exist so `super.onStatusChanged(...)` still resolves.

extern "C" fn sound_on_status_changed(
    engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if bound(engine).is_err() {
        return report_error(
            out_error,
            "WaveSoundBuffer.onStatusChanged: compat context is not bound",
        );
    }
    set_void(out);
    0
}

extern "C" fn sound_on_fade_completed(
    engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if bound(engine).is_err() {
        return report_error(
            out_error,
            "WaveSoundBuffer.onFadeCompleted: compat context is not bound",
        );
    }
    set_void(out);
    0
}

extern "C" fn sound_on_label(
    engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if bound(engine).is_err() {
        return report_error(
            out_error,
            "WaveSoundBuffer.onLabel: compat context is not bound",
        );
    }
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

channel_int_getter!(sound_position_get, "position", |_object, channel| {
    (channel.position_seconds() * 1000.0).round() as i64
});

channel_int_getter!(
    sound_sample_position_get,
    "samplePosition",
    |object, channel| {
        let rate = object.source.as_ref().map_or(0, |s| s.sample_rate());
        (channel.position_seconds() * f64::from(rate)).round() as i64
    }
);

channel_int_getter!(sound_total_time_get, "totalTime", |object, _channel| {
    object.source.as_ref().map_or(0, |source| {
        (source.duration_seconds() * 1000.0).round() as i64
    })
});

channel_int_getter!(sound_looping_get, "looping", |_object, channel| {
    i64::from(channel.looping())
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
        // `AudioSpec`, so report the decoded sample width.
        let value = with_object(services, id, |object, _mixer| {
            Some(if object.source.is_some() { 32 } else { 0 })
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
                Some(i64::from(
                    object.source.as_ref().map_or(0, |source| source.channels()),
                ))
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
            let value = with_object(services, id, |object, _mixer| {
                Some(object.frequency.map_or_else(
                    || {
                        object
                            .source
                            .as_ref()
                            .map_or(0, |source| i64::from(source.sample_rate()))
                    },
                    i64::from,
                ))
            })
            .flatten();
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
    with_sound!(
        engine,
        instance,
        "flags",
        out_error,
        |_ctx, services, id| {
            let Ok((resolved, _)) = bound(engine) else {
                return report_error(out_error, "WaveSoundBuffer.flags: not bound");
            };
            let host = resolved.engine();
            let expr = with_object(services, id, |object, _mixer| {
                Some(flags_expr(&object.buffer))
            })
            .flatten();
            let Some(expr) = expr else {
                return report_error(out_error, "WaveSoundBuffer.flags: not constructed");
            };
            match eval_object(host, &expr, "WaveSoundBuffer.flags") {
                Some(object) => {
                    set_object_out(host, out, Some(&object));
                    0
                }
                None => report_error(
                    out_error,
                    "WaveSoundBuffer.flags: could not build the array",
                ),
            }
        }
    )
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
        |_ctx, services, id| {
            let Ok((resolved, _)) = bound(engine) else {
                return report_error(out_error, "WaveSoundBuffer.labels: not bound");
            };
            let host = resolved.engine();
            let expr =
                with_object(services, id, |object, _mixer| Some(labels_expr(object))).flatten();
            let Some(expr) = expr else {
                return report_error(out_error, "WaveSoundBuffer.labels: not constructed");
            };
            match eval_object(host, &expr, "WaveSoundBuffer.labels") {
                Some(object) => {
                    set_object_out(host, out, Some(&object));
                    0
                }
                None => report_error(
                    out_error,
                    "WaveSoundBuffer.labels: could not build the dictionary",
                ),
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

channel_setter!(
    sound_position_set,
    "position",
    value,
    out_error,
    |channel| {
        channel.set_position(value_as_i64(unsafe { &*value }).max(0) as f64 / 1000.0);
    }
);

channel_setter!(sound_looping_set, "looping", value, out_error, |channel| {
    channel.set_looping(value_as_bool(unsafe { &*value }));
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
    let samples = value_as_i64(unsafe { &*value }).max(0);
    with_sound!(
        engine,
        instance,
        "samplePosition",
        out_error,
        |_ctx, services, id| {
            with_object(services, id, |object, mixer| {
                let rate = object.source.as_ref().map_or(0, |s| s.sample_rate());
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
    let frequency = value_as_i64(unsafe { &*value }).clamp(0, i64::from(u32::MAX)) as u32;
    with_sound!(
        engine,
        instance,
        "frequency",
        out_error,
        |_ctx, services, id| {
            with_object(services, id, |object, mixer| {
                object.frequency = if frequency == 0 {
                    None
                } else {
                    Some(frequency)
                };
                if let Some(channel) = mixer.channel(object.channel_id) {
                    apply_frequency(object, channel);
                }
            });
            0
        }
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
    let mode = services.state::<SoundState>().mixer.global_focus_mode();
    set_int(out, i64::from(mode));
    0
}

extern "C" fn sound_global_focus_mode_set(
    engine: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let mode = value_as_i64(unsafe { &*value }) as i32;
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "WaveSoundBuffer.globalFocusMode: not bound");
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    services
        .state::<SoundState>()
        .mixer
        .set_global_focus_mode(mode);
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
#[allow(dead_code)]
fn advance_and_dispatch(ctx: &mut CompatCtx, dt: f64) -> Result<(), CompatError> {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let host = unsafe { ctx.engine() };
    // SAFETY: see above.
    let services = unsafe { ctx.services() };

    if dt > 0.0 {
        let mut state = services.state::<SoundState>();
        state.mixer.advance(dt);
    }

    // Collect `(handler, args)` pairs with the borrow held, then release it
    // before touching the VM.
    let mut dispatches: Vec<(Tjs2ValueId, &'static str, Vec<TjsValue>)> = Vec::new();
    {
        let mut state = services.state::<SoundState>();
        let SoundState { mixer, objects, .. } = &mut *state;
        for object in objects.values_mut() {
            let Some(channel) = mixer.channel(object.channel_id) else {
                continue;
            };
            let events = object.buffer.poll(channel);
            let Some(self_obj) = &object.self_obj else {
                continue;
            };
            let self_id = self_obj.raw_id();
            for event in events {
                match event {
                    SoundEvent::StatusChanged(status) => dispatches.push((
                        self_id,
                        "onStatusChanged",
                        vec![TjsValue::String(status.as_str().to_string())],
                    )),
                    SoundEvent::FadeCompleted => {
                        dispatches.push((self_id, "onFadeCompleted", Vec::new()));
                    }
                    SoundEvent::Label(name) => {
                        dispatches.push((self_id, "onLabel", vec![TjsValue::String(name)]));
                    }
                    _ => {}
                }
            }
        }
    }

    for (id, member, args) in dispatches {
        if let Err(error) = host.call_member(id, member, &args) {
            log::warn!("WaveSoundBuffer: {member} handler failed: {error}");
        }
    }
    Ok(())
}

/// The `sound_poll` step of the documented frame body (`compat::frame`).
///
/// Advances the shared mixer to the L1 monotonic clock and forwards the L2
/// playback events to the object's `onStatusChanged`/`onFadeCompleted`/
/// `onLabel` handlers. The host calls it once per frame.
#[allow(dead_code)]
pub fn sound_poll(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let now = services.state::<SoundState>().clock.now_ms() as f64 / 1000.0;
    {
        let mut state = services.state::<SoundState>();
        state.mixer.advance_to(now);
    }
    advance_and_dispatch(ctx, 0.0)
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
        invalidate: None,
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
                set: None,
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
            set_audio_decoder(
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
        assert_eq!(env.eval_int("s.bits"), 32, "the L2 pipeline is f32");
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
        // The poll observed the unload -> play transition.
        assert_eq!(env.eval_int("events.count"), 1);
        assert_eq!(env.eval_str("events[0]"), "status:play");
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
        env.run("s.open('tone.wav'); s.play(); s.fade(0, 50);");
        env.advance(0.08);
        assert!(env.eval_int("events.count") >= 1);
        assert_eq!(env.eval_str("events[0]"), "fade");
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
        assert_eq!(env.eval_str("events[0]"), "label:mid");
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
