//! Clock-driven audio mixer.
//!
//! The mixer is **not** device-driven: nothing here touches an audio
//! device. The app's update loop calls [`Mixer::advance`] (or the global
//! [`crate::advance`] with a monotonic timestamp) every frame, which moves
//! each playing channel's position forward by the elapsed time and flips
//! per-channel "done" state when a non-looping source reaches its end.
//! The Bevy/rodio side pulls samples later via [`Mixer::render_mix`] /
//! [`crate::player::MixerSource`]; headless machines simply never pull.
//!
//! # Fades
//!
//! The reference's `Fade(to, time, blanktime)` (`SoundBufferBaseIntf.cpp`,
//! `tTJSNI_BaseSoundBuffer::Fade`) ramps the volume linearly from its value
//! at fade start to `to` over `time` milliseconds, after an optional
//! `blanktime` delay, in 25 ms "beats". This port uses the same linear ramp
//! but advances continuously with `dt` instead of in fixed beats (the beat
//! granularity is an implementation detail of the reference's timer).
//! While a fade is active it *is* the channel volume (the reference calls
//! `SetVolume` with the ramped value each beat, and `volume` reads it back);
//! setting `volume` explicitly cancels the active fade.

use std::sync::{Arc, Mutex};

use crate::decode::DecodedAudio;

/// Volume clamp (the task's channel surface uses `0..=1`; the reference
/// engine uses `0..=100000` — see the crate docs for the scale mapping).
pub const MAX_VOLUME: f32 = 1.0;
/// Pan clamp, `-1` (full left) ..= `1` (full right).
pub const MAX_PAN: f32 = 1.0;

/// A linear volume fade (see module docs).
#[derive(Debug, Clone)]
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
        Fade {
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
        let t = ((self.elapsed - self.delay) / self.duration).clamp(0.0, 1.0) as f32;
        (self.from + (self.to - self.from) * t).clamp(0.0, MAX_VOLUME)
    }
}

/// One mixer channel. A channel has at most one source and one playback
/// state; `SoundChannel` native instances are thin handles onto these.
#[derive(Debug, Clone)]
pub struct Channel {
    /// Stable channel id (assigned by the [`Mixer`]).
    pub id: u64,
    /// Decoded source, if one has been `play`ed.
    pub source: Option<Arc<DecodedAudio>>,
    /// Whether playback is running (`false` after `stop`, at end, ...).
    pub playing: bool,
    /// Whether playback is paused (`position` does not advance).
    pub paused: bool,
    /// Base volume in `0..=1`.
    pub volume: f32,
    /// Pan in `-1..=1` (`-1` full left, `+1` full right, `0` center).
    pub pan: f32,
    /// Loop the source forever.
    pub looping: bool,
    /// Playback position in seconds.
    pub position_seconds: f64,
    /// Sticky "reached the end" flag: set by `advance` when a non-looping
    /// source finishes, cleared by `play`/`stop`.
    pub done: bool,
    /// Active volume fade, if any.
    pub fade: Option<Fade>,
    /// Sticky "a fade finished during the last [`Channel::advance`]" flag.
    /// Set when a ramp completes, cleared by `play`/`stop`/`set_volume` and
    /// by starting a new fade. The sound poll reads it to fire the owner's
    /// `onFadeCompleted` event, then clears it.
    pub fade_finished: bool,
}

impl Channel {
    fn new(id: u64) -> Self {
        Channel {
            id,
            source: None,
            playing: false,
            paused: false,
            volume: MAX_VOLUME,
            pan: 0.0,
            looping: false,
            position_seconds: 0.0,
            done: false,
            fade: None,
            fade_finished: false,
        }
    }

    /// True while audio is actually moving (playing, not paused, has a
    /// source with samples).
    pub fn is_playing(&self) -> bool {
        self.playing
            && !self.paused
            && self
                .source
                .as_ref()
                .is_some_and(|s| !s.samples.is_empty() && s.channels > 0)
    }

    /// Start (or restart) `source` from position 0.
    pub fn play(&mut self, source: Arc<DecodedAudio>) {
        self.source = Some(source);
        self.playing = true;
        self.paused = false;
        self.position_seconds = 0.0;
        self.done = false;
        self.fade = None;
        self.fade_finished = false;
    }

    /// Stop playback, keeping the current position (the reference's `stop`
    /// also leaves the position where it was; a later `play` restarts).
    pub fn stop(&mut self) {
        self.playing = false;
        self.paused = false;
        self.done = false;
        self.fade = None;
        self.fade_finished = false;
    }

    /// Pause: position stops advancing (no-op when not playing).
    pub fn pause(&mut self) {
        if self.playing {
            self.paused = true;
        }
    }

    /// Resume from pause.
    pub fn resume(&mut self) {
        self.paused = false;
    }

    /// Seek to `secs` (clamped to `>= 0`; the reference allows seeking past
    /// the end, which stops the channel on the next advance).
    pub fn set_position(&mut self, secs: f64) {
        self.position_seconds = secs.max(0.0);
    }

    /// Set the base volume (clamped to `0..=1`); cancels an active fade.
    pub fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, MAX_VOLUME);
        self.fade = None;
        self.fade_finished = false;
    }

    /// Set the pan (clamped to `-1..=1`).
    pub fn set_pan(&mut self, p: f32) {
        self.pan = p.clamp(-MAX_PAN, MAX_PAN);
    }

    /// The volume actually used for rendering: the fade's ramped value while
    /// a fade is active, else the base volume.
    pub fn effective_volume(&self) -> f32 {
        match &self.fade {
            Some(f) => f.volume(),
            None => self.volume,
        }
    }

    /// Start a linear fade to `to` over `duration` seconds after `delay`
    /// seconds, from the current effective volume (reference `Fade`).
    pub fn fade(&mut self, to: f32, duration: f64, delay: f64) {
        self.fade = Some(Fade::new(
            self.effective_volume(),
            to.clamp(0.0, MAX_VOLUME),
            duration,
            delay,
        ));
        self.fade_finished = false;
    }

    /// Duration of the current source in seconds (0 when unloaded).
    pub fn duration_seconds(&self) -> f64 {
        self.source.as_ref().map_or(0.0, |a| a.duration_seconds())
    }

    /// Advance this channel by `dt` seconds. Returns true if the channel
    /// reached its end during this step (non-looping source).
    pub fn advance(&mut self, dt: f64) -> bool {
        if let Some(f) = &mut self.fade {
            f.advance(dt);
            if f.finished() {
                // The ramp is done: the fade's target becomes the volume.
                self.volume = f.to.clamp(0.0, MAX_VOLUME);
                self.fade = None;
                self.fade_finished = true;
            }
        }

        let mut became_done = false;
        if self.is_playing() {
            let dur = self.duration_seconds();
            if dur <= 0.0 {
                self.playing = false;
                self.done = true;
                return true;
            }
            self.position_seconds += dt;
            if self.position_seconds >= dur {
                if self.looping {
                    // Wrap around, keeping the overshoot remainder.
                    self.position_seconds %= dur;
                } else if self.position_seconds > dur {
                    // Strictly past the end: done. (Exactly at the end the
                    // channel is still "playing" until the next advance,
                    // matching the task's "advance(1.0) → isDone false"
                    // for a 1s source.)
                    self.position_seconds = dur;
                    self.playing = false;
                    self.done = true;
                    became_done = true;
                }
            }
        }
        became_done
    }
}

/// The clock-driven mixer: a collection of channels advanced by the app's
/// update loop.
#[derive(Debug)]
pub struct Mixer {
    channels: Vec<Channel>,
    next_id: u64,
    /// Internal monotonic clock in seconds (the app's timestamps).
    clock: f64,
}

impl Default for Mixer {
    fn default() -> Self {
        Mixer {
            channels: Vec::new(),
            // Channel ids start at **1**: `0` is the "no channel yet"
            // sentinel for the WaveSoundBuffer natives (`Stream::channel_id`),
            // so an id-0 first channel would make every subsequent access
            // spawn a fresh empty channel and the buffer would never play.
            next_id: 1,
            clock: 0.0,
        }
    }
}

impl Mixer {
    /// A fresh, empty mixer.
    pub fn new() -> Self {
        Self::default()
    }

    /// The channels, in id order.
    pub fn channels(&self) -> &[Channel] {
        &self.channels
    }

    /// Create a channel (used by `new SoundChannel()`) and return its id.
    pub fn spawn_channel(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.channels.push(Channel::new(id));
        id
    }

    /// Remove a channel (used when the owning TJS object dies).
    pub fn remove_channel(&mut self, id: u64) {
        self.channels.retain(|c| c.id != id);
    }

    /// Mutable access to a channel by id.
    pub fn channel(&mut self, id: u64) -> Option<&mut Channel> {
        self.channels.iter_mut().find(|c| c.id == id)
    }

    /// Read-only access to a channel by id.
    pub fn channel_ref(&self, id: u64) -> Option<&Channel> {
        self.channels.iter().find(|c| c.id == id)
    }

    /// Move all playing channels forward by `dt` seconds.
    ///
    /// This is the workhorse the app's update loop calls once per frame;
    /// pure computation, no device involved.
    pub fn advance(&mut self, dt: f64) {
        self.clock += dt;
        for c in &mut self.channels {
            c.advance(dt);
        }
    }

    /// Advance using an absolute monotonic timestamp (`now` in seconds).
    ///
    /// The internal clock starts at 0, so the *first* call advances by
    /// `now` (the app is expected to pass a monotonic clock such as
    /// `Instant::elapsed().as_secs_f64()`, not wall-clock epoch seconds).
    pub fn advance_to(&mut self, now: f64) {
        let dt = (now - self.clock).max(0.0);
        // `advance` moves the internal clock forward by `dt`, ending at
        // `now`.
        self.advance(dt);
    }

    /// The internal clock value in seconds.
    pub fn clock(&self) -> f64 {
        self.clock
    }

    /// Render one audio-device chunk and advance the mixer by its duration.
    ///
    /// This is what [`crate::player::MixerSource`] calls: the audio callback
    /// is the clock while a device is streaming, so each rendered chunk must
    /// move the play positions (otherwise the next callback replays it).
    pub fn render_mix_advancing(&mut self, out: &mut [f32], out_rate: u32, out_channels: u16) {
        self.render_mix(out, out_rate, out_channels);
        let channels = usize::from(out_channels);
        if out_rate > 0 && channels > 0 {
            let frames = out.len() / channels;
            if frames > 0 {
                self.advance(frames as f64 / f64::from(out_rate));
            }
        }
    }

    /// Render the current mix into `out` (interleaved `f32`).
    ///
    /// Every playing channel is resampled from its own sample rate to
    /// `out_rate` (linear/nearest interpolation into the fully-decoded PCM)
    /// and accumulated with its effective volume and pan. `out_channels`
    /// must be 1 (mono: channels summed) or 2 (stereo: pan balance law,
    /// `pan = -1` left only, `+1` right only, `0` both at full gain).
    /// Sources with more than 2 channels are skipped (documented limit).
    pub fn render_mix(&self, out: &mut [f32], out_rate: u32, out_channels: u16) {
        out.fill(0.0);
        let out_ch = usize::from(out_channels);
        if out_ch == 0 || out_rate == 0 || out.is_empty() {
            return;
        }
        let out_rate_f = f64::from(out_rate);
        for c in &self.channels {
            if !c.is_playing() {
                continue;
            }
            let Some(src) = &c.source else { continue };
            let v = c.effective_volume();
            if v <= 0.0 {
                continue;
            }
            let (gl, gr) = pan_gains(c.pan);
            let src_rate = f64::from(src.sample_rate.max(1));
            let src_ch = usize::from(src.channels);
            let total_frames = src.frames() as usize;
            if src_ch > 2 || total_frames == 0 {
                continue;
            }
            for (i, frame) in out.chunks_mut(out_ch).enumerate() {
                // Render from the channel's current playback position; the
                // app's clock (`advance_to`) keeps it moving. Ignoring the
                // position made the output device replay the first buffer
                // forever (effectively silence).
                let t = c.position_seconds + i as f64 / out_rate_f;
                let sample_pos = (t * src_rate).max(0.0);
                let i0 = sample_pos.floor() as usize;
                let frac = (sample_pos - i0 as f64) as f32;
                let i0 = i0.min(total_frames - 1);
                let i1 = (i0 + 1).min(total_frames - 1);
                // Linear interpolation: source and device rates usually
                // differ (48 kHz Vorbis/Opus on a 44.1 kHz device), and
                // nearest-neighbour sampling there is audibly aliased.
                let (l, r) = if src_ch == 1 {
                    let a = src.samples[i0];
                    let b = src.samples[i1];
                    let s = a + (b - a) * frac;
                    (s, s)
                } else {
                    let (a0, b0) = (src.samples[i0 * 2], src.samples[i0 * 2 + 1]);
                    let (a1, b1) = (src.samples[i1 * 2], src.samples[i1 * 2 + 1]);
                    (a0 + (a1 - a0) * frac, b0 + (b1 - b0) * frac)
                };
                if out_ch == 1 {
                    frame[0] += (l * gl + r * gr) * 0.5 * v;
                } else {
                    frame[0] += l * v * gl;
                    frame[1] += r * v * gr;
                }
            }
        }
        // Keep the summed mix in range so the device never hard-clips a
        // loud voice + BGM overlap into crackle.
        for s in out.iter_mut() {
            *s = s.clamp(-1.0, 1.0);
        }
    }
}

/// Pan gains (balance law): `-1` silences the right channel, `+1` silences
/// the left, `0` leaves both at full gain.
fn pan_gains(pan: f32) -> (f32, f32) {
    let p = pan.clamp(-MAX_PAN, MAX_PAN);
    (1.0 - p.max(0.0), 1.0 - (-p).max(0.0))
}

/// Lock a mutex, recovering from poisoning (never panic across the FFI
/// boundary; a panic on the VM thread aborts the process).
pub fn lock_ok<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, channels: u16, seconds: f64) -> DecodedAudio {
        let frames = (rate as f64 * seconds) as usize;
        let mut samples = Vec::with_capacity(frames * usize::from(channels));
        for f in 0..frames {
            let v =
                (2.0 * std::f64::consts::PI * 440.0 * f as f64 / rate as f64).sin() as f32 * 0.5;
            for _ in 0..channels {
                samples.push(v);
            }
        }
        DecodedAudio {
            sample_rate: rate,
            channels,
            samples,
        }
    }

    #[test]
    fn play_advance_reaches_done() {
        let mut m = Mixer::new();
        let id = m.spawn_channel();
        {
            let c = m.channel(id).unwrap();
            c.play(Arc::new(tone(44100, 1, 1.0)));
            assert!(c.is_playing());
            assert!(!c.done);
        }
        m.advance(0.4);
        let c = m.channel_ref(id).unwrap();
        assert!((c.position_seconds - 0.4).abs() < 1e-9);
        assert!(c.is_playing());
        assert!(!c.done);
        m.advance(0.7); // total 1.1 > 1.0
        let c = m.channel_ref(id).unwrap();
        assert!(!c.is_playing());
        assert!(c.done);
        assert_eq!(c.position_seconds, 1.0);
    }

    #[test]
    fn loop_wraps_position() {
        let mut m = Mixer::new();
        let id = m.spawn_channel();
        m.channel(id).unwrap().play(Arc::new(tone(44100, 2, 1.0)));
        m.channel(id).unwrap().looping = true;
        m.advance(1.2);
        let c = m.channel_ref(id).unwrap();
        assert!(c.is_playing(), "looping channel keeps playing");
        assert!(!c.done);
        assert!(
            (c.position_seconds - 0.2).abs() < 1e-9,
            "position wraps: {}",
            c.position_seconds
        );
        m.advance(2.5); // wraps twice more
        let c = m.channel_ref(id).unwrap();
        assert!(
            (c.position_seconds - 0.7).abs() < 1e-9,
            "position wraps: {}",
            c.position_seconds
        );
        assert!(c.is_playing());
    }

    #[test]
    fn pause_resume_stop() {
        let mut m = Mixer::new();
        let id = m.spawn_channel();
        let c = m.channel(id).unwrap();
        c.play(Arc::new(tone(44100, 1, 2.0)));
        m.advance(1.0);
        m.channel(id).unwrap().pause();
        assert!(!m.channel_ref(id).unwrap().is_playing());
        m.advance(0.5); // paused: no movement
        assert_eq!(m.channel_ref(id).unwrap().position_seconds, 1.0);
        m.channel(id).unwrap().resume();
        m.advance(0.5);
        assert!((m.channel_ref(id).unwrap().position_seconds - 1.5).abs() < 1e-9);
        m.channel(id).unwrap().stop();
        let c = m.channel_ref(id).unwrap();
        assert!(!c.playing);
        assert!(!c.done);
        assert!(!c.is_playing());
    }

    #[test]
    fn volume_pan_clamp_and_render() {
        let mut m = Mixer::new();
        let id = m.spawn_channel();
        {
            let c = m.channel(id).unwrap();
            c.play(Arc::new(tone(44100, 2, 0.1)));
            c.set_volume(2.0);
            assert_eq!(c.volume, MAX_VOLUME);
            c.set_volume(-1.0);
            assert_eq!(c.volume, 0.0);
            c.set_volume(0.5);
            c.set_pan(5.0);
            assert_eq!(c.pan, 1.0);
            c.set_pan(-5.0);
            assert_eq!(c.pan, -1.0);
            c.set_pan(0.0);
            assert_eq!(c.effective_volume(), 0.5);
        }

        // render: stereo pan -1 must put everything on the left channel.
        {
            let c = m.channel(id).unwrap();
            c.set_volume(1.0);
            c.set_pan(-1.0);
        }
        let mut out = vec![0.0f32; 4410];
        m.render_mix(&mut out, 44100, 2);
        let left_peak = out.iter().step_by(2).fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            left_peak > 0.3,
            "left channel should carry the tone: {left_peak}"
        );
        let right_peak = out
            .iter()
            .skip(1)
            .step_by(2)
            .fold(0.0f32, |m, s| m.max(s.abs()));
        assert_eq!(right_peak, 0.0, "right channel silenced at pan -1");
        // and the two channels of a stereo source at pan 0 both render.
        {
            let c = m.channel(id).unwrap();
            c.set_pan(0.0);
        }
        m.render_mix(&mut out, 44100, 2);
        // a frame where the tone is non-zero (quarter period at 440 Hz ≈
        // sample 25) must be equal on both channels
        assert!(
            (out[50] - out[51]).abs() < 1e-6,
            "stereo source, pan 0: channels equal"
        );
        assert!(out[50].abs() > 0.3, "tone audible at pan 0: {}", out[50]);
    }

    #[test]
    fn fade_ramps_and_completes() {
        let mut m = Mixer::new();
        let id = m.spawn_channel();
        let c = m.channel(id).unwrap();
        c.play(Arc::new(tone(44100, 1, 2.0)));
        c.fade(0.0, 1.0, 0.0); // fade out over 1s
        m.advance(0.5);
        let c = m.channel_ref(id).unwrap();
        assert!(
            (c.effective_volume() - 0.5).abs() < 0.01,
            "halfway fade: {}",
            c.effective_volume()
        );
        m.advance(0.5);
        let c = m.channel_ref(id).unwrap();
        assert_eq!(c.volume, 0.0, "fade completes at target");
        assert!(c.fade.is_none());
        assert!(c.is_playing(), "fade-out does not stop playback");
    }

    #[test]
    fn render_mix_sums_channels() {
        let mut m = Mixer::new();
        let a = m.spawn_channel();
        let b = m.spawn_channel();
        let src = Arc::new(tone(44100, 1, 0.1));
        m.channel(a).unwrap().play(src.clone());
        m.channel(b).unwrap().play(src);
        m.channel(a).unwrap().set_volume(0.25);
        m.channel(b).unwrap().set_volume(0.75);
        let mut out = vec![0.0f32; 4410];
        m.render_mix(&mut out, 44100, 1);
        let mono_peak = out.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
        // two channels at 0.25+0.75 = 1.0 total -> same as a single channel at 1.0
        let mut single = vec![0.0f32; 4410];
        let mut m2 = Mixer::new();
        let s = m2.spawn_channel();
        m2.channel(s).unwrap().play(Arc::new(tone(44100, 1, 0.1)));
        m2.render_mix(&mut single, 44100, 1);
        let single_peak = single.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
        assert!(
            (mono_peak - single_peak).abs() < 1e-4,
            "{mono_peak} vs {single_peak}"
        );
    }

    /// A 48 kHz source rendered on a 44.1 kHz device must stay at the same
    /// pitch (linear interpolation, not a playback-rate change).
    #[test]
    fn render_resamples_48k_to_44k_at_correct_pitch() {
        let mut m = Mixer::new();
        let c = m.spawn_channel();
        m.channel(c).unwrap().play(Arc::new(tone(48000, 1, 1.0)));
        // One second of output at 44.1 kHz consumes exactly the 48 kHz source.
        let mut out = vec![0.0f32; 44100];
        m.render_mix(&mut out, 44100, 1);
        let crossings = out
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        let freq = crossings as f64 / 2.0;
        assert!(
            (freq - 440.0).abs() < 15.0,
            "resampled pitch {freq} Hz, expected ~440"
        );
        // And it is actually audible, not a replay of the first buffer.
        let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.4, "resampled peak {peak}");
    }

    #[test]
    fn advance_to_absolute_clock() {
        let mut m = Mixer::new();
        let id = m.spawn_channel();
        m.channel(id).unwrap().play(Arc::new(tone(44100, 1, 5.0)));
        m.advance_to(1.0);
        assert!((m.channel_ref(id).unwrap().position_seconds - 1.0).abs() < 1e-9);
        m.advance_to(2.5); // +1.5s
        assert!((m.channel_ref(id).unwrap().position_seconds - 2.5).abs() < 1e-9);
        m.advance_to(2.0); // clock must never go backwards
        assert_eq!(m.channel_ref(id).unwrap().position_seconds, 2.5);
    }
}
