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
use crate::source::AudioTrack;

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
    /// Source, if one has been `play`ed. The track may still be loading or
    /// streaming; the mixer renders silence until frames are available.
    pub source: Option<Arc<AudioTrack>>,
    /// Whether playback is running (`false` after `stop`, at end, ...).
    pub playing: bool,
    /// Whether playback is paused (`position` does not advance).
    pub paused: bool,
    /// Base volume in `0..=1`.
    pub volume: f32,
    /// Secondary volume in `0..=1`, multiplied with [`Self::volume`] and
    /// the mixer's global volume (reference `Volume2`; the game's
    /// `volume2` property, used for config/per-voice volume).
    pub volume2: f32,
    /// Playback-rate multiplier (reference `frequency / sampleRate`); `1.0`
    /// is native speed. Changes both pitch and tempo, matching the
    /// reference's DirectSound frequency control.
    pub rate: f64,
    /// Pan in `-1..=1` (`-1` full left, `+1` full right, `0` center).
    pub pan: f32,
    /// 3D position `(x, y, z)` (reference `PosX`/`PosY`/`PosZ`), used for
    /// the DirectSound3D default distance attenuation. Defaults to the
    /// origin (full volume).
    pub pos: [f32; 3],
    /// Conditional `.sli` loop-link flags (reference `WaveFlags`), consulted
    /// when choosing which link to jump at the loop point.
    pub loop_flags: [i32; crate::sli::TVP_WL_MAX_FLAGS],
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
            volume2: MAX_VOLUME,
            rate: 1.0,
            pan: 0.0,
            pos: [0.0; 3],
            loop_flags: [0; crate::sli::TVP_WL_MAX_FLAGS],
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
                .is_some_and(|s| s.is_ready() && !s.is_empty())
    }

    /// Start (or restart) a fully-decoded source from position 0.
    pub fn play(&mut self, source: Arc<DecodedAudio>) {
        self.play_track(AudioTrack::from_decoded(source));
    }

    /// Start (or restart) `track` from position 0. Accepts a still-loading
    /// or streaming track: playback begins as soon as frames are available.
    pub fn play_track(&mut self, track: Arc<AudioTrack>) {
        self.source = Some(track);
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
        // A streaming track must restart its decoder near the new position;
        // a whole-file track already has every frame in memory.
        if let Some(source) = &self.source {
            source.request_seek_seconds(self.position_seconds);
        }
    }

    /// Set the base volume (clamped to `0..=1`); cancels an active fade.
    pub fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, MAX_VOLUME);
        self.fade = None;
        self.fade_finished = false;
    }

    /// Set the secondary volume (clamped to `0..=1`).
    pub fn set_volume2(&mut self, v: f32) {
        self.volume2 = v.clamp(0.0, MAX_VOLUME);
    }

    /// Set the playback-rate multiplier (reference `frequency`). Clamped to
    /// a sane positive range; `1.0` is native speed.
    pub fn set_rate(&mut self, r: f64) {
        self.rate = r.clamp(0.01, 100.0);
    }

    /// Set the pan (clamped to `-1..=1`).
    pub fn set_pan(&mut self, p: f32) {
        self.pan = p.clamp(-MAX_PAN, MAX_PAN);
    }

    /// The per-buffer volume actually used for rendering: the fade's ramped
    /// value while a fade is active, else the base volume. This is what the
    /// reference `GetVolume` reports (it does **not** include `volume2` or
    /// the global volume).
    pub fn effective_volume(&self) -> f32 {
        match &self.fade {
            Some(f) => f.volume(),
            None => self.volume,
        }
    }

    /// The full channel gain including [`Self::volume2`] and the 3D
    /// distance attenuation (but not the mixer's global volume / focus
    /// mute, which [`Mixer::render_mix`] applies).
    pub fn output_gain(&self) -> f32 {
        self.effective_volume() * self.volume2 * self.spatial_attenuation()
    }

    /// Set the 3D position (reference `SetPos`).
    pub fn set_pos(&mut self, x: f32, y: f32, z: f32) {
        self.pos = [x, y, z];
    }

    /// DirectSound3D default-distance attenuation for [`Self::pos`].
    ///
    /// DirectSound3D's default listener sits at the origin with minimum
    /// distance 1.0, maximum distance 1e9 and rolloff 1.0, so the linear
    /// gain is `min / (min + rolloff * (d - min))` with `d` clamped to
    /// `[min, max]` (`DS3D_DEFAULTMINDISTANCE`/`_MAXDISTANCE`/`_ROLLOFF`).
    /// At the origin this is exactly 1.0, so channels that never touch
    /// `posX/Y/Z` are unaffected.
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

    /// Duration of the current source in seconds (0 when unloaded,
    /// `INFINITY` when the source's length is unknown).
    pub fn duration_seconds(&self) -> f64 {
        self.source.as_ref().map_or(0.0, |a| a.duration_seconds())
    }

    /// Advance this channel by `dt` seconds. Returns true if the channel
    /// reached its end during this step (non-looping source).
    ///
    /// This is the headless clock: the app's update loop drives it, and no
    /// audio is pulled from the result. The device path instead uses
    /// [`Mixer::render_mix_advancing`], which advances each streaming
    /// channel only by the frames it actually rendered.
    pub fn advance(&mut self, dt: f64) -> bool {
        self.advance_inner(dt, None)
    }

    /// [`Channel::advance`] with an explicit decoded watermark (seconds).
    ///
    /// `watermark` is `Some(end)` for a streaming source and `None` for a
    /// whole-file source. Capturing it before rendering is what keeps the
    /// position in lock-step with the frames actually played.
    fn advance_inner(&mut self, dt: f64, watermark: Option<f64>) -> bool {
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
            let rate = self.rate.max(0.0);
            // The `.sli` loop link (reference `tTVPWaveLoopManager`): when
            // looping, the track loops `To..From` instead of the whole file.
            let loop_bounds = if self.looping {
                self.source.as_ref().and_then(|s| {
                    let sr = f64::from(s.sample_rate().max(1));
                    s.loop_link_with_flags(&self.loop_flags)
                        .map(|l| (l.to as f64 / sr, l.from as f64 / sr))
                })
            } else {
                None
            };
            let loop_end = loop_bounds.map(|(_, end)| end);

            // A streaming source may not have decoded the frames the wall
            // clock has reached (the worker is still starting up, or `open`
            // was followed immediately by `play`). Advancing the position
            // past the end of decoded audio silently skips the head of the
            // track and, once the clock outruns the whole decode-ahead ring,
            // permanently drops every decoded frame (the position is always
            // ahead of the data). Hold the position at the decoded watermark
            // until frames arrive instead.
            let step = match watermark {
                Some(available) if available >= self.position_seconds => {
                    (self.position_seconds + dt * rate).min(available) - self.position_seconds
                }
                // A seek/loop restart is in flight (the watermark is behind
                // the requested position): hold until the worker republishes
                // frames there.
                Some(_) => 0.0,
                // Whole-file source: every frame is available, so the clock
                // is the position.
                None => dt * rate,
            };
            self.position_seconds += step;
            let dur = self.duration_seconds();
            // End detection uses the frames the source actually produced, not
            // just a container-declared duration: the device clock clamps a
            // streaming position to the decoded watermark, so it can reach
            // the duration exactly and must still flip to `done`. A
            // whole-file source keeps the reference's "strictly past the
            // end" rule (exactly at the end is still playing). When a `.sli`
            // loop link is active, its `From` is the loop end instead of the
            // full duration.
            let at_end = if let Some(end) = loop_end {
                self.position_seconds >= end
            } else if dur.is_finite() {
                self.source
                    .as_ref()
                    .is_some_and(|s| s.playback_ended(self.position_seconds))
            } else {
                self.source
                    .as_ref()
                    .is_some_and(|s| s.has_ended_at(self.position_seconds))
            };
            if at_end {
                if self.looping {
                    if let Some((start, end)) = loop_bounds {
                        // Jump back to the loop start, keeping any overshoot
                        // as an offset into the loop body.
                        let span = (end - start).max(f64::MIN_POSITIVE);
                        let overshoot = (self.position_seconds - end).max(0.0) % span;
                        self.position_seconds = start + overshoot;
                        if let Some(source) = &self.source {
                            let frame = (self.position_seconds * source.sample_rate() as f64)
                                .round() as u64;
                            source.on_loop_wrap_to_frame(frame);
                        }
                    } else if dur.is_finite() {
                        // Wrap around, keeping the overshoot remainder.
                        self.position_seconds %= dur;
                        if let Some(source) = &self.source {
                            source.on_loop_wrap();
                        }
                    } else {
                        // A streaming loop restarts its decoder at 0.
                        self.position_seconds = 0.0;
                        if let Some(source) = &self.source {
                            source.on_loop_wrap();
                        }
                    }
                } else {
                    if dur.is_finite() {
                        self.position_seconds = self.position_seconds.min(dur);
                    }
                    self.playing = false;
                    self.done = true;
                    became_done = true;
                }
            }
            // Free decoded frames the playback position has passed so a
            // streaming worker can keep decoding ahead with bounded memory.
            if let Some(source) = &self.source {
                source.release_before(self.position_seconds);
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
    /// Global volume multiplier in `0..=1` (reference
    /// `tTJSNI_WaveSoundBuffer::GlobalVolume`) applied to every channel at
    /// render time.
    global_volume: f32,
    /// Global focus mode (reference `tTVPSoundGlobalFocusMode`): `0` never
    /// mute, `1` mute when minimized, `2` mute when deactivated. The host
    /// feeds [`Mixer::set_app_focused`]/[`Mixer::set_app_minimized`].
    global_focus_mode: i32,
    /// Whether the host window is active (reference focus state).
    app_focused: bool,
    /// Whether the host window is minimized.
    app_minimized: bool,
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

    /// The global volume multiplier (`0..=1`).
    pub fn global_volume(&self) -> f32 {
        self.global_volume
    }

    /// Set the global volume multiplier, clamped to `0..=1`.
    pub fn set_global_volume(&mut self, v: f32) {
        self.global_volume = v.clamp(0.0, MAX_VOLUME);
    }

    /// The global focus mode (`0` never mute, `1` mute-on-minimize, `2`
    /// mute-on-deactivate; reference `tTVPSoundGlobalFocusMode`).
    pub fn global_focus_mode(&self) -> i32 {
        self.global_focus_mode
    }

    /// Set the global focus mode, clamped to the three reference values.
    pub fn set_global_focus_mode(&mut self, mode: i32) {
        self.global_focus_mode = mode.clamp(0, 2);
    }

    /// Feed the host window's active state (reference focus tracking).
    pub fn set_app_focused(&mut self, focused: bool) {
        self.app_focused = focused;
    }

    /// Feed the host window's minimized state.
    pub fn set_app_minimized(&mut self, minimized: bool) {
        self.app_minimized = minimized;
    }

    /// The focus mute gain (`1.0` audible, `0.0` muted) for the current
    /// focus mode and host state (reference `SetVolumeToSoundBuffer`'s
    /// `mutevol`).
    pub fn focus_gain(&self) -> f32 {
        let muted = match self.global_focus_mode {
            1 => self.app_minimized,
            2 => !self.app_focused || self.app_minimized,
            _ => false,
        };
        if muted { 0.0 } else { 1.0 }
    }

    /// Render one audio-device chunk and advance the mixer by its duration.
    ///
    /// This is what [`crate::player::MixerSource`] calls: the audio callback
    /// is the clock while a device is streaming, so each rendered chunk must
    /// move the play positions (otherwise the next callback replays it).
    pub fn render_mix_advancing(&mut self, out: &mut [f32], out_rate: u32, out_channels: u16) {
        // Snapshot each channel's decoded watermark *before* rendering. The
        // advance below must only move over audio that was renderable for
        // this chunk; querying the watermark after the render would consume
        // frames the worker published mid-render without ever playing them.
        let watermarks: Vec<Option<f64>> = self
            .channels
            .iter()
            .map(|c| c.source.as_ref().and_then(|s| s.available_end_seconds()))
            .collect();
        self.render_mix(out, out_rate, out_channels);
        let channels = usize::from(out_channels);
        if out_rate > 0 && channels > 0 {
            let frames = out.len() / channels;
            if frames > 0 {
                let dt = frames as f64 / f64::from(out_rate);
                self.clock += dt;
                for (c, watermark) in self.channels.iter_mut().zip(watermarks) {
                    c.advance_inner(dt, watermark);
                }
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
            let v = c.output_gain() * self.global_volume * self.focus_gain();
            if v <= 0.0 {
                continue;
            }
            let (gl, gr) = pan_gains(c.pan);
            let src_rate = f64::from(src.sample_rate().max(1));
            let src_ch = src.channels();
            if src_ch == 0 || src_ch > 2 {
                continue;
            }
            for (i, frame) in out.chunks_mut(out_ch).enumerate() {
                // Render from the channel's current playback position; the
                // app's clock (`advance_to`) keeps it moving. Ignoring the
                // position made the output device replay the first buffer
                // forever (effectively silence). `rate` preserves the
                // reference's `frequency` control (pitch + tempo).
                let t = c.position_seconds + i as f64 / out_rate_f * c.rate;
                let sample_pos = (t * src_rate).max(0.0);
                let i0 = sample_pos.floor() as u64;
                let frac = (sample_pos - i0 as f64) as f32;
                // A frame that is not decoded yet (loading/underrun) yields
                // no output for this sample instead of blocking; the last
                // frame of a complete source reuses itself (clamped), which
                // is what the old `i1.min(total_frames - 1)` did.
                let Some((a0, b0)) = src.frame(i0) else {
                    continue;
                };
                let (a1, b1) = src.frame(i0 + 1).unwrap_or((a0, b0));
                // Linear interpolation: source and device rates usually
                // differ (48 kHz Vorbis/Opus on a 44.1 kHz device), and
                // nearest-neighbour sampling there is audibly aliased.
                let (l, r) = if src_ch == 1 {
                    let s = a0 + (a1 - a0) * frac;
                    (s, s)
                } else {
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
    use crate::sli::{LoopCondition, LoopLink, SliInfo};

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

    /// A `.sli` loop link makes a looping channel loop `To..From` instead
    /// of the whole file (the game's BGM intro/loop behaviour).
    fn tone_with_loop(rate: u32, seconds: f64, from_sec: f64, to_sec: f64) -> Arc<AudioTrack> {
        let info = SliInfo {
            links: vec![LoopLink {
                from: (from_sec * f64::from(rate)) as u64,
                to: (to_sec * f64::from(rate)) as u64,
                smooth: false,
                condition: LoopCondition::None,
                ref_value: 0,
                cond_var: 0,
            }],
            labels: Vec::new(),
        };
        AudioTrack::from_decoded_with_loop(Arc::new(tone(rate, 1, seconds)), Some(info))
    }

    #[test]
    fn sli_loop_point_wraps_into_the_loop_region() {
        let mut m = Mixer::new();
        let id = m.spawn_channel();
        // 2.0s track, loop from sample 1.0s back to 0.5s.
        m.channel(id)
            .unwrap()
            .play_track(tone_with_loop(44100, 2.0, 1.0, 0.5));
        m.channel(id).unwrap().looping = true;

        // Cross the loop end (1.0s) by 0.1s -> 0.5 + 0.1 = 0.6s.
        m.advance(1.1);
        let c = m.channel_ref(id).unwrap();
        assert!(c.is_playing(), "loop keeps playing");
        assert!(
            (c.position_seconds - 0.6).abs() < 1e-6,
            "looped to the loop start + overshoot, got {}",
            c.position_seconds
        );

        // A second pass wraps again.
        m.advance(0.5);
        let c = m.channel_ref(id).unwrap();
        assert!(
            (c.position_seconds - 0.6).abs() < 1e-6,
            "second wrap, got {}",
            c.position_seconds
        );
    }

    #[test]
    fn volume2_and_global_volume_multiply_the_gain() {
        let mut m = Mixer::new();
        let id = m.spawn_channel();
        {
            let c = m.channel(id).unwrap();
            c.play(Arc::new(tone(44100, 1, 1.0)));
            c.set_volume(1.0);
            c.set_volume2(0.5);
            // The reference `GetVolume` reports the base volume only.
            assert_eq!(c.effective_volume(), 1.0);
            assert_eq!(c.volume2, 0.5);
            assert!((c.output_gain() - 0.5).abs() < 1e-6);
        }
        m.set_global_volume(0.5);
        assert!((m.global_volume() - 0.5).abs() < 1e-6);

        // Render is actually attenuated by the product.
        let mut out = vec![0.0f32; 4410];
        m.render_mix(&mut out, 44100, 1);
        let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            (0.1..=0.13).contains(&peak),
            "tone at gain 0.25 peak ~0.125, got {peak}"
        );
    }

    #[test]
    fn spatial_attenuation_matches_directsound_defaults() {
        let mut m = Mixer::new();
        let id = m.spawn_channel();
        let c = m.channel(id).unwrap();
        // Origin and anything inside DirectSound3D's minimum distance is
        // full volume (the default listener at the origin).
        c.set_pos(0.0, 0.0, 0.0);
        assert_eq!(c.spatial_attenuation(), 1.0);
        c.set_pos(0.5, 0.0, 0.0);
        assert_eq!(c.spatial_attenuation(), 1.0);
        // Beyond the minimum distance the linear rolloff applies:
        // 1 / (1 + 1 * (d - 1)).
        c.set_pos(2.0, 0.0, 0.0);
        assert!((c.spatial_attenuation() - 0.5).abs() < 1e-6);
        c.set_pos(0.0, 0.0, 3.0);
        assert!((c.spatial_attenuation() - 1.0 / 3.0).abs() < 1e-6);
        // output_gain folds the attenuation in.
        c.set_volume(1.0);
        c.set_volume2(1.0);
        c.set_pos(3.0, 0.0, 0.0);
        assert!((c.output_gain() - 1.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn global_focus_mode_mutes_by_host_state() {
        let mut m = Mixer::new();
        // Mode 0 never mutes.
        assert_eq!(m.focus_gain(), 1.0);
        m.set_app_minimized(true);
        assert_eq!(m.focus_gain(), 1.0);
        // Mode 1 mutes only while minimized.
        m.set_global_focus_mode(1);
        assert_eq!(m.focus_gain(), 0.0);
        m.set_app_minimized(false);
        assert_eq!(m.focus_gain(), 1.0);
        // Mode 2 mutes when deactivated or minimized.
        m.set_global_focus_mode(2);
        m.set_app_focused(false);
        assert_eq!(m.focus_gain(), 0.0);
        m.set_app_focused(true);
        m.set_app_minimized(true);
        assert_eq!(m.focus_gain(), 0.0);
        m.set_app_minimized(false);
        assert_eq!(m.focus_gain(), 1.0);
        // The mode clamps to the three reference values.
        m.set_global_focus_mode(9);
        assert_eq!(m.global_focus_mode(), 2);
        m.set_global_focus_mode(-1);
        assert_eq!(m.global_focus_mode(), 0);
    }

    #[test]
    fn conditional_sli_link_uses_channel_flags() {
        // Two links: a conditional one (flag 0 == 1) to 0.8s listed first,
        // and an unconditional fallback to 0.5s. File order picks the
        // conditional one only when its flag matches.
        let info = SliInfo {
            links: vec![
                LoopLink {
                    from: 44100,
                    to: 35280,
                    smooth: false,
                    condition: LoopCondition::Equal,
                    ref_value: 1,
                    cond_var: 0,
                },
                LoopLink {
                    from: 44100,
                    to: 22050,
                    smooth: false,
                    condition: LoopCondition::None,
                    ref_value: 0,
                    cond_var: 0,
                },
            ],
            labels: Vec::new(),
        };
        let track = AudioTrack::from_decoded_with_loop(Arc::new(tone(44100, 1, 2.0)), Some(info));
        let mut m = Mixer::new();
        let id = m.spawn_channel();
        m.channel(id).unwrap().play_track(track);
        m.channel(id).unwrap().looping = true;

        // Flag clear -> fallback link to 0.5s.
        m.advance(1.1);
        assert!(
            (m.channel_ref(id).unwrap().position_seconds - 0.6).abs() < 1e-6,
            "fallback link, got {}",
            m.channel_ref(id).unwrap().position_seconds
        );

        // Flag set -> conditional link to 0.8s.
        m.channel(id).unwrap().loop_flags[0] = 1;
        m.advance(0.5);
        assert!(
            (m.channel_ref(id).unwrap().position_seconds - 0.9).abs() < 1e-6,
            "conditional link, got {}",
            m.channel_ref(id).unwrap().position_seconds
        );
    }

    #[test]
    fn rate_advances_position_faster() {
        let mut m = Mixer::new();
        let id = m.spawn_channel();
        let c = m.channel(id).unwrap();
        c.play(Arc::new(tone(44100, 1, 2.0)));
        c.set_rate(2.0);
        m.advance(0.5);
        assert!(
            (m.channel_ref(id).unwrap().position_seconds - 1.0).abs() < 1e-9,
            "rate 2.0 plays 1.0s of source in 0.5s"
        );
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
