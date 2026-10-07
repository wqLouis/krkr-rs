//! L2 sound model and software mixer.
//!
//! Behavioral reference: `main`'s
//! `crates/tvp-sound/src/{mixer,source,wavesound,sli,waveflags}.rs`. This is
//! the **state** half of sound, re-expressed as explicit L2 values (D9):
//!
//! * [`SoundSource`] — caller-supplied decoded PCM plus its [`AudioSpec`]
//!   and optional `.sli` loop information. It owns no decoder; L1 `media`
//!   produces the buffer and L2 wraps it.
//! * [`SoundChannel`] — one playback voice: play/stop/pause, loop, pan,
//!   volume/volume2, playback rate, 3D position and linear fades.
//! * [`SoundBuffer`] — the script-object half (`WaveSoundBuffer`): the loaded
//!   source, its [`WaveFlags`], and `.sli` label/status event tracking.
//! * [`Mixer`] — a clock-driven collection of channels plus the
//!   [`Mixer::mix`] action that renders the caller's output buffer.
//!
//! # No device, no globals
//!
//! Nothing here opens an audio device and there is no process-global state:
//! the application's update loop calls [`Mixer::advance`] (or
//! [`Mixer::advance_to`]) each frame, and the L1 `AudioDevice` (in
//! `platform`) later pulls [`Mixer::mix`] into its own callback. Two
//! independent [`Mixer`] values in one process never interact.
//!
//! # Errors (D11)
//!
//! Invalid PCM/spec values are rejected when a [`SoundSource`] is built, and
//! an invalid output spec is a typed [`SoundError`] rather than silent
//! silence. There are no hardcoded chain/size/loop caps.

use std::sync::Arc;

use api::media::{AudioSpec, DecodedAudio};

/// Script volume/pan scale used by the reference `WaveSoundBuffer` natives
/// (`0..=100000`). The mixer works in `0..=1`; L3 divides by this when
/// crossing the script boundary.
pub const TVP_VOLUME_SCALE: f64 = 100_000.0;

/// Number of conditional loop flags the reference tracks
/// (`TVP_WL_MAX_FLAGS`).
pub const TVP_WL_MAX_FLAGS: usize = 16;

/// Maximum value of a conditional loop flag (`TVP_WL_MAX_FLAG_VALUE`).
pub const TVP_WL_MAX_FLAG_VALUE: i32 = 9999;

/// Maximum channel volume (`0..=1`).
pub const MAX_VOLUME: f32 = 1.0;

/// Maximum absolute pan (`-1` full left, `+1` full right).
pub const MAX_PAN: f32 = 1.0;

/// Errors produced by the sound model and mixer.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SoundError {
    /// A `.sli` side-car could not be parsed.
    #[error("invalid .sli data: {message}")]
    Sli {
        /// Human-readable description of the failure.
        message: String,
    },
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
        channels: u16,
    },
    /// A decoded buffer's length does not match its spec.
    #[error("inconsistent PCM buffer: {len} samples for {frames} frames x {channels} channels")]
    InconsistentBuffer {
        /// Interleaved sample count that was provided.
        len: usize,
        /// Frame count implied by the spec.
        frames: u64,
        /// Channel count implied by the spec.
        channels: u16,
    },
    /// A buffer has no loaded source.
    #[error("sound buffer has no loaded source")]
    NoSource,
}

// ---------------------------------------------------------------------------
// `.sli` loop information (ported from tvp-sound/src/sli.rs)
// ---------------------------------------------------------------------------

/// A loop-link condition (`tTVPWaveLoopLinkCondition`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LoopCondition {
    /// `Condition=no` — always taken.
    None,
    /// `Condition=eq` — taken when flag `CondVar == RefValue`.
    Equal,
    /// `Condition=ne`.
    NotEqual,
    /// `Condition=gt`.
    Greater,
    /// `Condition=ge`.
    GreaterOrEqual,
    /// `Condition=lt`.
    Lesser,
    /// `Condition=le`.
    LesserOrEqual,
}

impl LoopCondition {
    /// Parse the reference condition spelling (case-insensitive).
    fn parse(text: &str) -> Option<Self> {
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
}

/// One `Link { … }` entry: playback jumps from sample `from` to sample `to`
/// once it reaches `from` (`tTVPWaveLoopLink`).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct LoopLink {
    /// Sample granule the loop jumps *from* (the loop end).
    pub from: u64,
    /// Sample granule the loop jumps *to* (the loop start).
    pub to: u64,
    /// Whether the reference would crossfade the seam (`Smooth=True`).
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

    /// Whether this link's condition holds for `flags` (reference
    /// `GetNearestEvent` condition test). A non-degenerate link has
    /// `from > to`.
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

/// One `Label { … }` entry (a named sample position).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct WaveLabel {
    /// Sample granule the label sits at.
    pub position: u64,
    /// Label name.
    pub name: String,
}

impl WaveLabel {
    /// Create a named label at a sample granule.
    pub fn new(position: u64, name: String) -> Self {
        Self { position, name }
    }
}

/// Parsed `.sli` loop/label information for one audio entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SliInfo {
    /// All `Link` entries, in file order.
    pub links: Vec<LoopLink>,
    /// All `Label` entries, in file order.
    pub labels: Vec<WaveLabel>,
}

impl SliInfo {
    /// Create loop/label information from its entries.
    pub fn new(links: Vec<LoopLink>, labels: Vec<WaveLabel>) -> Self {
        Self { links, labels }
    }

    /// The first unconditional, non-degenerate loop link. `None` when the
    /// file only has conditional links or no link at all.
    pub fn active_link(&self) -> Option<&LoopLink> {
        self.links
            .iter()
            .find(|l| l.condition == LoopCondition::None && l.from > l.to)
    }

    /// The first non-degenerate link whose condition holds for `flags`
    /// (reference `GetNearestEvent`; the game files have a single link, so
    /// file order suffices).
    pub fn active_link_with_flags(&self, flags: &[i32]) -> Option<&LoopLink> {
        self.links
            .iter()
            .find(|l| l.from > l.to && l.matches(flags))
    }
}

/// Parse `.sli` bytes. A leading UTF-8 BOM is tolerated.
///
/// Returns [`SoundError::Sli`] only when the text is not one of the two
/// supported formats; an empty/whitespace/comment-only file yields an empty
/// [`SliInfo`].
pub fn parse_sli(bytes: &[u8]) -> Result<SliInfo, SoundError> {
    let text = String::from_utf8_lossy(bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return Ok(SliInfo::default());
    }
    if trimmed.starts_with('#') && !trimmed.starts_with("#2.") {
        return Ok(SliInfo::default());
    }
    if !trimmed.starts_with('#') {
        return parse_old(trimmed);
    }
    parse_v2(trimmed)
}

fn sli_error(message: impl Into<String>) -> SoundError {
    SoundError::Sli {
        message: message.into(),
    }
}

/// Old format: `LoopStart=<n>` / `LoopLength=<n>`.
fn parse_old(text: &str) -> Result<SliInfo, SoundError> {
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
    Ok(SliInfo {
        links: vec![LoopLink {
            from: start.saturating_add(length),
            to: start,
            smooth: false,
            condition: LoopCondition::None,
            ref_value: 0,
            cond_var: 0,
        }],
        labels: Vec::new(),
    })
}

/// v2.00 format: `Link { … }` / `Label { … }` blocks.
fn parse_v2(text: &str) -> Result<SliInfo, SoundError> {
    let mut info = SliInfo::default();
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
fn parse_link(body: &str) -> Result<LoopLink, SoundError> {
    let mut link = LoopLink {
        from: 0,
        to: 0,
        smooth: false,
        condition: LoopCondition::None,
        ref_value: 0,
        cond_var: 0,
    };
    for (key, value) in entities(body) {
        match key.to_ascii_lowercase().as_str() {
            "from" => link.from = value.trim().parse().unwrap_or(0),
            "to" => link.to = value.trim().parse().unwrap_or(0),
            "smooth" => link.smooth = value.trim().eq_ignore_ascii_case("true"),
            "condition" => {
                link.condition = LoopCondition::parse(&value)
                    .ok_or_else(|| sli_error(format!("sli: unknown Condition '{value}'")))?
            }
            "refvalue" => link.ref_value = value.trim().parse().unwrap_or(0),
            "condvar" => link.cond_var = value.trim().parse().unwrap_or(0),
            _ => {}
        }
    }
    Ok(link)
}

/// Parse one `key=value;` block body into a [`WaveLabel`].
fn parse_label(body: &str) -> WaveLabel {
    let mut label = WaveLabel {
        position: 0,
        name: String::new(),
    };
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
// WaveFlags
// ---------------------------------------------------------------------------

/// The conditional-loop flag surface behind `WaveSoundBuffer.flags`.
///
/// A plain explicit-state value: 16 numeric slots that `.sli`
/// `Link { Condition=… }` entries are tested against at the loop point. No
/// global registry — the owning [`SoundBuffer`]/[`SoundChannel`] holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct WaveFlags {
    values: [i32; TVP_WL_MAX_FLAGS],
}

impl WaveFlags {
    /// A fresh flag set with every slot at `0`.
    pub const fn new() -> Self {
        Self {
            values: [0; TVP_WL_MAX_FLAGS],
        }
    }

    /// Number of flag slots (always [`TVP_WL_MAX_FLAGS`]).
    pub const fn count(&self) -> usize {
        TVP_WL_MAX_FLAGS
    }

    /// Read flag `index` (`0` when out of range).
    pub fn get(&self, index: usize) -> i32 {
        self.values.get(index).copied().unwrap_or(0)
    }

    /// Write flag `index`, clamped to `0..=TVP_WL_MAX_FLAG_VALUE`. An
    /// out-of-range index is ignored.
    pub fn set(&mut self, index: usize, value: i32) {
        if let Some(slot) = self.values.get_mut(index) {
            *slot = value.clamp(0, TVP_WL_MAX_FLAG_VALUE);
        }
    }

    /// Clear every flag (`WaveFlags.reset`).
    pub fn reset(&mut self) {
        self.values = [0; TVP_WL_MAX_FLAGS];
    }

    /// The raw flag array, for [`LoopLink::matches`].
    pub fn as_slice(&self) -> &[i32; TVP_WL_MAX_FLAGS] {
        &self.values
    }
}

impl Default for WaveFlags {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Decoded source
// ---------------------------------------------------------------------------

/// A playable decoded PCM source: caller-supplied interleaved `f32` samples
/// plus their [`AudioSpec`] and optional `.sli` loop information.
///
/// Construction validates the buffer so the mixer can assume a consistent,
/// mono-or-stereo source. The value is cheap to clone (the PCM lives behind
/// an [`Arc`]) and is shared by every channel that plays it.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SoundSource {
    audio: Arc<DecodedAudio>,
    frames: u64,
    sli: Option<SliInfo>,
}

impl SoundSource {
    /// Wrap decoded PCM, deriving the frame count from the buffer.
    ///
    /// Fails with a typed error when the spec is unusable or the buffer
    /// length disagrees with `sample_rate`/`channels`.
    pub fn new(audio: Arc<DecodedAudio>) -> Result<Self, SoundError> {
        Self::with_sli(audio, None)
    }

    /// Like [`Self::new`], attaching parsed `.sli` loop information.
    pub fn with_sli(audio: Arc<DecodedAudio>, sli: Option<SliInfo>) -> Result<Self, SoundError> {
        let spec = audio.spec;
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
        let frames = audio.frame_count();
        let expected = frames
            .checked_mul(u64::from(spec.channels))
            .and_then(|n| usize::try_from(n).ok());
        if expected != Some(audio.interleaved.len()) {
            return Err(SoundError::InconsistentBuffer {
                len: audio.interleaved.len(),
                frames,
                channels: spec.channels,
            });
        }
        Ok(Self { audio, frames, sli })
    }

    /// The decoded buffer.
    pub fn audio(&self) -> &DecodedAudio {
        &self.audio
    }

    /// The source format, with `total_frames` always filled in.
    pub fn spec(&self) -> AudioSpec {
        AudioSpec {
            sample_rate: self.audio.spec.sample_rate,
            channels: self.audio.spec.channels,
            total_frames: Some(self.frames),
        }
    }

    /// Samples per second.
    pub fn sample_rate(&self) -> u32 {
        self.audio.spec.sample_rate
    }

    /// Interleaved channel count (1 or 2).
    pub fn channels(&self) -> u16 {
        self.audio.spec.channels
    }

    /// Total number of frames.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Whether the source has no frames.
    pub fn is_empty(&self) -> bool {
        self.frames == 0
    }

    /// Duration in seconds.
    pub fn duration_seconds(&self) -> f64 {
        let rate = self.sample_rate();
        if rate == 0 {
            0.0
        } else {
            self.frames as f64 / f64::from(rate)
        }
    }

    /// The attached `.sli` information, when a side-car was present.
    pub fn sli(&self) -> Option<&SliInfo> {
        self.sli.as_ref()
    }

    /// The first unconditional, non-degenerate `.sli` loop link.
    pub fn loop_link(&self) -> Option<&LoopLink> {
        self.sli.as_ref().and_then(SliInfo::active_link)
    }

    /// The first `.sli` loop link whose condition holds for `flags`.
    pub fn loop_link_with_flags(&self, flags: &[i32]) -> Option<&LoopLink> {
        self.sli
            .as_ref()
            .and_then(|s| s.active_link_with_flags(flags))
    }

    /// The `.sli` labels, in file order (empty when no sidebar).
    pub fn labels(&self) -> &[WaveLabel] {
        self.sli.as_ref().map_or(&[], |s| &s.labels)
    }

    /// Read one sample frame as `(left, right)` (mono duplicated).
    ///
    /// Returns `None` past the end.
    pub fn frame(&self, frame: u64) -> Option<(f32, f32)> {
        if frame >= self.frames {
            return None;
        }
        let channels = usize::from(self.audio.spec.channels);
        let index = usize::try_from(frame).ok()?.checked_mul(channels)?;
        let left = *self.audio.interleaved.get(index)?;
        let right = if channels >= 2 {
            *self.audio.interleaved.get(index + 1)?
        } else {
            left
        };
        Some((left, right))
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

/// Script-visible playback status (`tTVPSoundStatus`).
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
#[non_exhaustive]
pub enum SoundEvent {
    /// The script-visible status changed.
    StatusChanged(Status),
    /// An active fade completed (`onFadeCompleted`).
    FadeCompleted,
    /// The playhead crossed a `.sli` label (`onLabel`).
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
    flags: WaveFlags,
    looping: bool,
    position_seconds: f64,
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
            flags: WaveFlags::new(),
            looping: false,
            position_seconds: 0.0,
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

    /// Start (or restart) `source` from position 0.
    pub fn play(&mut self, source: Arc<SoundSource>) {
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

    /// The conditional loop flags consulted for `.sli` link selection.
    pub fn flags(&self) -> &WaveFlags {
        &self.flags
    }

    /// Mutable access to the conditional loop flags.
    pub fn flags_mut(&mut self) -> &mut WaveFlags {
        &mut self.flags
    }

    /// Whether the source loops.
    pub fn looping(&self) -> bool {
        self.looping
    }

    /// Set whether the source loops.
    pub fn set_looping(&mut self, looping: bool) {
        self.looping = looping;
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

    /// DirectSound3D default-distance attenuation for [`Self::pos`].
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
        // The `.sli` loop link loops `To..From` instead of the whole file.
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
// Sound object (WaveSoundBuffer)
// ---------------------------------------------------------------------------

/// The script-object half of a `WaveSoundBuffer`: the loaded source, its
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
    flags: WaveFlags,
    labels: Vec<WaveLabel>,
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
    pub fn flags(&self) -> &WaveFlags {
        &self.flags
    }

    /// Mutable access to the conditional loop flags.
    pub fn flags_mut(&mut self) -> &mut WaveFlags {
        &mut self.flags
    }

    /// The loaded `.sli` labels.
    pub fn labels(&self) -> &[WaveLabel] {
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
    /// status changes, a completed fade, and `.sli` labels the playhead
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
    global_volume: f32,
    global_focus_mode: i32,
    app_focused: bool,
    app_minimized: bool,
}

impl Default for Mixer {
    fn default() -> Self {
        Self {
            channels: Vec::new(),
            // Channel ids start at 1: 0 is the "no channel" sentinel used by
            // the script surface.
            next_id: 1,
            clock: 0.0,
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
        self.channels.push(SoundChannel::new(id));
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

    /// Feed the host window's active state.
    pub fn set_app_focused(&mut self, focused: bool) {
        self.app_focused = focused;
    }

    /// Feed the host window's minimized state.
    pub fn set_app_minimized(&mut self, minimized: bool) {
        self.app_minimized = minimized;
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
        let channels = usize::from(out_spec.channels);
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
/// The PCM buffers are the caller-supplied sources held by each channel; the
/// function decodes nothing. [`Mixer::mix`] is the usual entry point.
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
        );
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
    accumulate_source(source, position_seconds, rate, gain, pan, out, out_spec);
    Ok(())
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
) {
    if out.is_empty() || gain <= 0.0 {
        return;
    }
    let out_channels = usize::from(out_spec.channels);
    let out_rate = f64::from(out_spec.sample_rate);
    let src_rate = f64::from(source.sample_rate().max(1));
    let src_channels = usize::from(source.channels());
    let rate = if rate.is_finite() { rate } else { 1.0 };
    let (left_gain, right_gain) = pan_gains(pan);

    for (index, frame) in out.chunks_exact_mut(out_channels).enumerate() {
        let t = position_seconds + index as f64 / out_rate * rate;
        let sample_pos = (t * src_rate).max(0.0);
        if !sample_pos.is_finite() || sample_pos >= u64::MAX as f64 {
            continue;
        }
        let i0 = sample_pos.floor();
        let i0u = i0 as u64;
        let frac = (sample_pos - i0) as f32;
        let Some((a0, b0)) = source.frame(i0u) else {
            continue;
        };
        let (a1, b1) = source.frame(i0u.saturating_add(1)).unwrap_or((a0, b0));
        let (left, right) = if src_channels == 1 {
            let s = a0 + (a1 - a0) * frac;
            (s, s)
        } else {
            (a0 + (a1 - a0) * frac, b0 + (b1 - b0) * frac)
        };
        if out_channels == 1 {
            frame[0] += (left * left_gain + right * right_gain) * 0.5 * gain;
        } else {
            frame[0] += left * gain * left_gain;
            frame[1] += right * gain * right_gain;
        }
    }
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
