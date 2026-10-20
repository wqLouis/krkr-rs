//! Pure, device-independent buffer policy for the L1 audio output backends.
//!
//! Both the cpal (Linux) and AAudio (Android) backends sit behind a software
//! queue between the frame-loop producer ([`AudioDevice::write`]) and the
//! device callback that consumes it. Keeping the policy here — instead of in
//! each `#[cfg]`-gated backend — makes it identical on every platform and,
//! crucially, unit-testable without an audio device.
//!
//! [`AudioDevice::write`]: crate::AudioDevice::write
//!
//! # Pre-fill
//!
//! The stream is opened but **not started** until the queue holds
//! [`PREFILL_MS`] worth of audio. Starting against an empty queue makes the
//! first callbacks drain nothing, so normal frame jitter (a slow frame, a
//! lock) is immediately audible; a modest pre-fill absorbs it. This mirrors
//! the reference's own `tTJSNI_WaveSoundBuffer::Play`, which fills several
//! buffer units before `SoundBuffer->Play(...)` (`reference/cpp/core/sound/
//! win32/WaveImpl.cpp:2911-2921`, "fill sound buffer with some first
//! samples").
//!
//! # Overrun
//!
//! [`AudioQueue::push`] keeps at most [`MAX_BUFFER_MS`] of audio, dropping the
//! **oldest** samples first. Latency therefore cannot grow without bound: the
//! queue is a fixed two-second window onto the most recent audio.
//!
//! # Underrun
//!
//! When the device asks for more than the queue holds, the last emitted
//! sample **for that channel** is repeated (a zero-order hold) but shaped by a
//! short fade: the shared output gain ramps from `1.0` to `0.0` across the
//! held samples, and back to `1.0` when real samples resume. A bare hold
//! leaves a constant DC plateau; when the next chunk arrives — after a long
//! frame stall the mixer cursor has already skipped ahead — the output steps
//! from the held level to an unrelated waveform sample, a hard discontinuity
//! (the audible pop). The reference mixer `memset`s its callback buffer to
//! silence before mixing (`reference/cpp/core/sound/win32/WaveMixer.cpp:424`),
//! so its own underrun is silence reached without a step; here the fade takes
//! the already-mixed held plateau through `0.0` and brings the resumed content
//! up from `0.0`, so both sides of the seam pass through zero. The gain is a
//! single scalar shared by every channel and advanced once per frame, so a
//! stereo underrun cannot shift the stereo image. Pre-fill keeps underruns
//! rare; this only softens the one after a long stall.
//!
//! # Health
//!
//! The queue counts the **episodes** it sees: one underrun that holds the last
//! sample for a hundred frames is one episode, and one overrun that drops a
//! thousand samples is one episode. The counters live with the queue, inside
//! the caller-owned state, so a caller can read a snapshot between frames and
//! report a bad device. The device callback itself never logs: it only
//! increments a counter.

use std::collections::VecDeque;

use api::error::AudioError;

use crate::audio::AudioHealth;

/// The most audio the queue may hold, in milliseconds.
///
/// This is the documented two-second contract: a slow consumer must never let
/// latency grow without bound, so [`AudioQueue::push`] drops the oldest
/// samples past this.
pub(crate) const MAX_BUFFER_MS: usize = 2_000;

/// How much audio must be buffered before the device is started, in
/// milliseconds.
///
/// A 150 ms target sits in the 100–250 ms window that suits a visual novel:
/// far more than the ~16.7 ms a 60 fps frame adds and comfortably above OS
/// scheduling jitter, but well below the point where audible/visual feedback
/// feels laggy.
pub(crate) const PREFILL_MS: usize = 150;

/// The underrun/resume fade length, in milliseconds.
///
/// Five milliseconds of linear amplitude fade is long enough that the
/// discontinuity carries no audible energy (the click is the step, not the
/// ramp) and short enough that a listener does not hear a new silence gap.
/// Counted in sample frames at `sample_rate`, with the gain advanced once per
/// frame, so the fade is the same wall-clock length for mono and stereo.
#[cfg(any(feature = "platform-linux", target_os = "android"))]
pub(crate) const RAMP_MS: usize = 5;

/// The software queue shared by [`AudioDevice::write`] and the device
/// callback.
///
/// All evolving state lives here, inside the caller-owned
/// [`AudioOutputState`](crate::AudioOutputState); the device object stays
/// stateless.
///
/// [`AudioDevice::write`]: crate::AudioDevice::write
pub(crate) struct AudioQueue {
    /// Interleaved samples not yet consumed by the device.
    samples: VecDeque<f32>,
    /// Maximum buffered samples (the two-second cap).
    capacity: usize,
    /// Samples required before the device may start.
    prefill: usize,
    /// Underrun episodes seen (a run of held samples).
    underrun_episodes: u64,
    /// Overrun episodes seen (a run of pushes that dropped samples).
    overrun_episodes: u64,
    /// Whether the queue is holding the last sample right now.
    underrunning: bool,
    /// Whether the last push dropped samples.
    overrunning: bool,
    /// Interleaved samples per second, so the queue's depth reads as a time.
    #[cfg(any(feature = "platform-linux", target_os = "android"))]
    per_second: usize,
    /// Channels per frame (at least one).
    #[cfg(any(feature = "platform-linux", target_os = "android"))]
    channels: usize,
    /// The last emitted sample of each channel (the hold source).
    #[cfg(any(feature = "platform-linux", target_os = "android"))]
    last_frame: Vec<f32>,
    /// Frames in a full fade (`RAMP_MS` at `sample_rate`, at least one).
    #[cfg(any(feature = "platform-linux", target_os = "android"))]
    ramp_samples: usize,
    /// The shared output gain in `0.0..=1.0`, applied to every channel.
    #[cfg(any(feature = "platform-linux", target_os = "android"))]
    gain: f32,
    /// Fade position in `0..=ramp_samples`; `gain == level / ramp_samples`.
    #[cfg(any(feature = "platform-linux", target_os = "android"))]
    gain_level: usize,
    /// Position within the current frame (`0..channels`).
    #[cfg(any(feature = "platform-linux", target_os = "android"))]
    phase: usize,
}

impl AudioQueue {
    /// Creates an empty queue for an interleaved stream of `channels`
    /// channels at `sample_rate` Hz.
    ///
    /// # Errors
    ///
    /// [`AudioError::InvalidSpec`] when the channel count cannot be
    /// represented in the address space.
    pub(crate) fn new(sample_rate: u32, channels: u32) -> Result<Self, AudioError> {
        let channels = usize::try_from(channels)
            .map_err(|_| AudioError::InvalidSpec {
                message: format!("channel count {channels} does not fit in memory"),
            })?
            .max(1);
        // `sample_rate * channels` is samples per second; both caps are
        // expressed in milliseconds.
        let per_second = (sample_rate as usize).saturating_mul(channels);
        let capacity = per_second.saturating_mul(MAX_BUFFER_MS) / 1_000;
        let prefill = (per_second.saturating_mul(PREFILL_MS) / 1_000).min(capacity);
        // The ramp is one gain step per frame, so it is measured in frames
        // (not interleaved samples) and is channel-count independent.
        #[cfg(any(feature = "platform-linux", target_os = "android"))]
        let ramp_samples = ((sample_rate as usize).saturating_mul(RAMP_MS) / 1_000).max(1);
        Ok(Self {
            samples: VecDeque::new(),
            capacity,
            prefill,
            underrun_episodes: 0,
            overrun_episodes: 0,
            underrunning: false,
            overrunning: false,
            #[cfg(any(feature = "platform-linux", target_os = "android"))]
            per_second,
            #[cfg(any(feature = "platform-linux", target_os = "android"))]
            channels,
            #[cfg(any(feature = "platform-linux", target_os = "android"))]
            last_frame: vec![0.0; channels],
            #[cfg(any(feature = "platform-linux", target_os = "android"))]
            ramp_samples,
            #[cfg(any(feature = "platform-linux", target_os = "android"))]
            gain: 1.0,
            #[cfg(any(feature = "platform-linux", target_os = "android"))]
            gain_level: ramp_samples,
            #[cfg(any(feature = "platform-linux", target_os = "android"))]
            phase: 0,
        })
    }

    /// Queues interleaved `pcm`, dropping the oldest samples beyond the cap,
    /// and returns whether the pre-fill target is now buffered.
    pub(crate) fn push(&mut self, pcm: &[f32]) -> bool {
        let overflow = (self.samples.len() + pcm.len()).saturating_sub(self.capacity);
        if overflow > 0 {
            let drop_count = overflow.min(self.samples.len());
            self.samples.drain(..drop_count);
            if !self.overrunning {
                self.overrunning = true;
                self.overrun_episodes = self.overrun_episodes.saturating_add(1);
            }
        } else {
            self.overrunning = false;
        }
        self.samples.extend(pcm.iter().copied());
        self.prefilled()
    }

    /// Whether the queue holds at least the pre-fill target.
    pub(crate) fn prefilled(&self) -> bool {
        self.samples.len() >= self.prefill
    }

    /// A snapshot of the queue's health counters.
    ///
    /// The counters count episodes, not samples: one underrun that holds the
    /// last sample for a hundred frames is one episode.
    pub(crate) fn health(&self) -> AudioHealth {
        AudioHealth {
            underrun_episodes: self.underrun_episodes,
            overrun_episodes: self.overrun_episodes,
            underrunning: self.underrunning,
        }
    }

    /// How much audio is still buffered, in seconds.
    ///
    /// This is the delay between the audio the producer has written and the
    /// audio the device plays: the listener hears the queue's content that
    /// much later than it was rendered.
    #[cfg(any(feature = "platform-linux", target_os = "android"))]
    pub(crate) fn queued_seconds(&self) -> f64 {
        if self.per_second == 0 {
            return 0.0;
        }
        self.samples.len() as f64 / self.per_second as f64
    }

    /// Returns the next queued sample, or repeats the last sample for this
    /// channel on underrun while fading toward silence (see the module docs).
    #[cfg(any(feature = "platform-linux", target_os = "android"))]
    pub(crate) fn pop_or_hold(&mut self) -> f32 {
        let channel = self.phase;
        // The gain is advanced once per frame, before the frame's first
        // channel, so both channels of a frame share one gain value.
        let at_frame_start = self.phase == 0;
        let sample = match self.samples.pop_front() {
            Some(sample) => {
                self.last_frame[channel] = sample;
                // Real audio resumes: the underrun episode is over.
                self.underrunning = false;
                if at_frame_start {
                    self.advance_gain(false);
                }
                sample
            }
            None => {
                if at_frame_start {
                    self.advance_gain(true);
                }
                if !self.underrunning {
                    self.underrunning = true;
                    self.underrun_episodes = self.underrun_episodes.saturating_add(1);
                }
                self.last_frame[channel]
            }
        };
        self.phase += 1;
        if self.phase == self.channels {
            self.phase = 0;
        }
        sample * self.gain
    }

    /// Steps the shared gain one frame toward `1.0` (real audio) or `0.0`
    /// (silence). The level is kept as an integer so a completed fade is
    /// exactly `1.0` or `0.0`, not a rounded near-miss.
    #[cfg(any(feature = "platform-linux", target_os = "android"))]
    fn advance_gain(&mut self, toward_silence: bool) {
        if toward_silence {
            self.gain_level = self.gain_level.saturating_sub(1);
        } else {
            self.gain_level = (self.gain_level + 1).min(self.ramp_samples);
        }
        self.gain = self.gain_level as f32 / self.ramp_samples as f32;
    }

    /// Drains `out` from the queue, returning whether the queue underran.
    ///
    /// This is the exact underrun step the device callbacks drive one sample
    /// at a time; it exists as a slice form so it can be unit-tested without
    /// a device.
    #[cfg(all(test, any(feature = "platform-linux", target_os = "android")))]
    pub(crate) fn drain(&mut self, out: &mut [f32]) -> bool {
        let underrun = self.samples.len() < out.len();
        for slot in out.iter_mut() {
            *slot = self.pop_or_hold();
        }
        underrun
    }
}

#[cfg(all(test, any(feature = "platform-linux", target_os = "android")))]
mod tests {
    use super::*;

    /// A small queue so the policy numbers stay small in assertions.
    fn queue() -> AudioQueue {
        AudioQueue::new(1_000, 1).expect("a 1 kHz mono queue is valid")
    }

    /// The queue's capacity in samples.
    fn queue_capacity() -> usize {
        1_000 * MAX_BUFFER_MS / 1_000
    }

    #[test]
    fn a_fresh_queue_is_empty_and_not_prefilled() {
        let queue = queue();
        assert_eq!(queue.samples.len(), 0);
        assert!(!queue.prefilled());
    }

    #[test]
    fn the_policy_numbers_match_the_documented_contract() {
        assert_eq!(MAX_BUFFER_MS, 2_000, "the two-second cap is a contract");
        assert!(
            (100..=250).contains(&PREFILL_MS),
            "pre-fill must stay in the 100–250 ms window"
        );
        assert!(
            (1..=20).contains(&RAMP_MS),
            "the fade must stay a few milliseconds"
        );
    }

    /// The ramp length in frames for the 1 kHz test queue.
    fn ramp_frames(queue: &AudioQueue) -> usize {
        queue.ramp_samples
    }

    /// The queue's depth reads as the time the device has yet to play: that
    /// is the delay between the audio the producer wrote and the audio the
    /// listener hears.
    #[test]
    fn the_queue_depth_reads_as_a_time() {
        let mut queue = queue();
        assert_eq!(queue.queued_seconds(), 0.0);
        queue.push(&[0.25; 500]);
        assert!(
            (queue.queued_seconds() - 0.5).abs() < 1e-9,
            "500 ms of a 1 kHz queue is half a second: {}",
            queue.queued_seconds()
        );
    }

    #[test]
    fn prefill_needs_the_full_target_not_just_some_audio() {
        let mut queue = queue();
        let target = 1_000 * PREFILL_MS / 1_000;
        queue.push(&vec![0.25; target - 1]);
        assert!(!queue.prefilled(), "one sample short is not enough");
        queue.push(&[0.25]);
        assert!(queue.prefilled());
    }

    #[test]
    fn the_cap_drops_the_oldest_samples_first() {
        let mut queue = queue();
        let capacity = queue_capacity();
        let ramp: Vec<f32> = (0..capacity).map(|i| i as f32).collect();
        queue.push(&ramp);
        assert_eq!(queue.samples.len(), capacity);

        queue.push(&[-1.0, -2.0, -3.0]);
        assert_eq!(queue.samples.len(), capacity, "the cap is maintained");

        let mut out = vec![0.0; capacity];
        queue.drain(&mut out);
        assert_eq!(out[0], 3.0, "the three oldest samples were dropped");
        assert_eq!(out[capacity - 3], -1.0);
        assert_eq!(out[capacity - 2], -2.0);
        assert_eq!(out[capacity - 1], -3.0);
    }

    #[test]
    fn an_underrun_fades_the_held_sample_without_a_step() {
        // The reference mixer feeds the callback silence before mixing
        // (`reference/cpp/core/sound/win32/WaveMixer.cpp:424`); the queue's
        // held plateau must reach that same silence without a step.
        //
        // A held plateau is the source of the pop: without the fade the output
        // would step from the last sample to whatever greets the next chunk
        // (the mixer cursor has moved on after a long stall). The shared gain
        // must never move by more than one ramp step per sample.
        let mut queue = queue();
        queue.push(&[1.0]);
        let mut first = [0.0; 1];
        assert!(!queue.drain(&mut first));
        assert_eq!(first, [1.0]);

        let mut held = [0.0; 50];
        assert!(queue.drain(&mut held));

        let allowed = 1.0 / queue.ramp_samples as f32 + f32::EPSILON;
        let mut previous = first[0];
        for sample in held {
            assert!(
                (sample - previous).abs() <= allowed,
                "step from {previous} to {sample} exceeds the ramp"
            );
            previous = sample;
        }
        assert_eq!(held[held.len() - 1], 0.0, "the fade ends in true silence");
    }

    #[test]
    fn the_underrun_fade_reaches_exactly_silence() {
        // The reference mixer's underrun output is silence
        // (`reference/cpp/core/sound/win32/WaveMixer.cpp:424`); the fade must
        // land on exactly that, not a rounded near-zero.
        let mut queue = queue();
        queue.push(&[1.0]);
        let mut out = [0.0; 64];
        assert!(queue.drain(&mut out));

        assert_eq!(out[0], 1.0, "the pre-underrun sample is untouched");
        assert_eq!(
            out[ramp_frames(&queue)],
            0.0,
            "the gain reaches exactly 0.0 after RAMP_SAMPLES held samples"
        );
        assert_eq!(out[63], 0.0);
    }

    #[test]
    fn playback_fades_back_in_and_recovers_the_exact_sample() {
        // The seam out of the held/silent state must pass through zero for the
        // same reason the reference silences the callback before mixing
        // (`reference/cpp/core/sound/win32/WaveMixer.cpp:424`).
        let mut queue = queue();
        // Establish silence: one real sample, then a long underrun.
        queue.push(&[1.0]);
        let mut silence = [0.0; 32];
        assert!(queue.drain(&mut silence));
        assert_eq!(silence[31], 0.0);

        // A fresh constant block fades in from silence and, once the gain is
        // back at 1.0, is reproduced bit-for-bit.
        let block = vec![0.25_f32; 32];
        queue.push(&block);
        let mut out = [0.0; 32];
        assert!(!queue.drain(&mut out));

        let frames = ramp_frames(&queue);
        assert_eq!(out[frames - 1], 0.25, "the exact sample is recovered");
        assert!(
            out[frames..].iter().all(|&sample| sample == 0.25),
            "after the ramp the steady state is bit-identical"
        );
        for window in out[..frames].windows(2) {
            assert!(
                window[0] > 0.0 && window[1] > window[0],
                "fade-in is monotonic"
            );
        }
    }

    #[test]
    fn steady_state_playback_is_bit_identical_to_the_unfaded_path() {
        let mut queue = queue();
        let input: Vec<f32> = (0..256).map(|i| ((i as f32) * 0.1).sin()).collect();
        queue.push(&input);
        let mut out = vec![0.0; input.len()];
        assert!(!queue.drain(&mut out));
        assert_eq!(out, input, "gain 1.0 must not alter a single sample");
    }

    #[test]
    fn a_held_frame_keeps_each_channel_and_the_stereo_image_separate() {
        // A stereo underrun must not smear the left channel's last value into
        // the right channel (or vice versa), and the shared gain must scale
        // both by the same factor so the image never shifts.
        let mut queue = AudioQueue::new(1_000, 2).expect("a 1 kHz stereo queue is valid");
        queue.push(&[0.3, 0.4]);
        let mut out = [0.0; 4];
        assert!(queue.drain(&mut out));
        assert_eq!(out[0], 0.3);
        assert_eq!(out[1], 0.4);
        assert!((out[2] / out[3] - out[0] / out[1]).abs() < 1e-6);
        assert!(out[2] > 0.0 && out[2] < out[0], "left fades out");
        assert!(out[3] > 0.0 && out[3] < out[1], "right fades out");
    }

    #[test]
    fn drain_reports_no_underrun_when_the_buffer_is_covered() {
        let mut queue = queue();
        queue.push(&[1.0, 2.0]);
        let mut out = [0.0; 2];
        assert!(!queue.drain(&mut out));
        assert_eq!(out, [1.0, 2.0]);
    }

    #[test]
    fn a_held_run_is_one_underrun_episode() {
        // One underrun that holds the last sample for two hundred frames is
        // one episode, not one per held sample: the counter is what a caller
        // reports, and a per-sample count would drown the report.
        let mut queue = queue();
        queue.push(&[1.0]);
        let mut held = vec![0.0; 200];
        assert!(queue.drain(&mut held));

        let health = queue.health();
        assert_eq!(
            health.underrun_episodes, 1,
            "one episode, not one per sample"
        );
        assert!(health.underrunning, "the episode is still open");
        assert_eq!(health.overrun_episodes, 0, "no audio was dropped");
    }

    #[test]
    fn the_health_clears_when_real_samples_resume() {
        let mut queue = queue();
        queue.push(&[1.0]);
        let mut held = vec![0.0; 50];
        assert!(queue.drain(&mut held));
        assert!(queue.health().underrunning, "the hold is still going");

        queue.push(&[0.5; 10]);
        let mut out = vec![0.0; 10];
        assert!(!queue.drain(&mut out));

        let health = queue.health();
        assert!(!health.underrunning, "the resumed audio closes the episode");
        assert_eq!(health.underrun_episodes, 1, "the episode is counted once");
    }

    #[test]
    fn an_underrun_after_a_resume_is_a_second_episode() {
        let mut queue = queue();
        queue.push(&[1.0]);
        let mut held = vec![0.0; 10];
        assert!(queue.drain(&mut held));

        queue.push(&[0.5; 10]);
        let mut out = vec![0.0; 10];
        assert!(!queue.drain(&mut out));

        let mut held = vec![0.0; 10];
        assert!(queue.drain(&mut held));
        assert_eq!(queue.health().underrun_episodes, 2);
    }

    #[test]
    fn a_dropping_run_is_one_overrun_episode() {
        let mut queue = queue();
        queue.push(&vec![0.0; queue_capacity()]);
        assert_eq!(
            queue.health().overrun_episodes,
            0,
            "the cap is reached but not exceeded"
        );

        queue.push(&[1.0, 2.0, 3.0]);
        assert_eq!(queue.health().overrun_episodes, 1);

        queue.push(&[4.0, 5.0]);
        assert_eq!(
            queue.health().overrun_episodes,
            1,
            "the episode is still open, so it is not counted again"
        );
    }

    #[test]
    fn an_overrun_after_the_queue_drains_is_a_second_episode() {
        let mut queue = queue();
        queue.push(&vec![0.0; queue_capacity()]);
        queue.push(&[1.0]);
        assert_eq!(queue.health().overrun_episodes, 1);

        // The device consumes the queue, so the next push fits again and the
        // episode ends.
        let mut out = vec![0.0; queue_capacity()];
        queue.drain(&mut out);
        queue.push(&[2.0]);
        assert_eq!(queue.health().overrun_episodes, 1, "the push fits again");

        queue.push(&vec![3.0; queue_capacity()]);
        assert_eq!(
            queue.health().overrun_episodes,
            2,
            "a new episode is counted"
        );
    }
}
