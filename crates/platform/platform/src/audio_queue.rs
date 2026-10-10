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
//! sample **for that channel** is repeated (a zero-order hold) rather than
//! injecting a hard `0.0`. The reference mixer instead `memset`s its callback
//! buffer to silence before mixing (`reference/cpp/core/sound/win32/
//! WaveMixer.cpp:424`), but that silence is produced by the mixer *at a mix
//! boundary*; here the queue holds already-mixed samples and injecting a zero
//! mid-waveform is a step discontinuity — the click. Holding the last value
//! is continuous at the seam and, when the next chunk arrives, playback
//! resumes without a synthetic step. The hold is per-channel so a stereo
//! underrun cannot smear one channel's last sample into the other. Pre-fill
//! keeps underruns rare; this only softens the one after a long stall.

use std::collections::VecDeque;

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
    /// Channels per frame (at least one).
    channels: usize,
    /// Maximum buffered samples (the two-second cap).
    capacity: usize,
    /// Samples required before the device may start.
    prefill: usize,
    /// The last emitted sample of each channel (the hold source).
    last_frame: Vec<f32>,
    /// Position within the current frame (`0..channels`).
    phase: usize,
}

impl AudioQueue {
    /// Creates an empty queue for an interleaved stream of `channels`
    /// channels at `sample_rate` Hz.
    pub(crate) fn new(sample_rate: u32, channels: u16) -> Self {
        let channels = usize::from(channels).max(1);
        // `sample_rate * channels` is samples per second; both caps are
        // expressed in milliseconds.
        let per_second = (sample_rate as usize).saturating_mul(channels);
        let capacity = per_second.saturating_mul(MAX_BUFFER_MS) / 1_000;
        let prefill = (per_second.saturating_mul(PREFILL_MS) / 1_000).min(capacity);
        Self {
            samples: VecDeque::new(),
            channels,
            capacity,
            prefill,
            last_frame: vec![0.0; channels],
            phase: 0,
        }
    }

    /// Queues interleaved `pcm`, dropping the oldest samples beyond the cap,
    /// and returns whether the pre-fill target is now buffered.
    pub(crate) fn push(&mut self, pcm: &[f32]) -> bool {
        let overflow = (self.samples.len() + pcm.len()).saturating_sub(self.capacity);
        if overflow > 0 {
            let drop_count = overflow.min(self.samples.len());
            self.samples.drain(..drop_count);
        }
        self.samples.extend(pcm.iter().copied());
        self.prefilled()
    }

    /// Whether the queue holds at least the pre-fill target.
    pub(crate) fn prefilled(&self) -> bool {
        self.samples.len() >= self.prefill
    }

    /// Returns the next queued sample, or repeats the last sample for this
    /// channel on underrun (see the module docs).
    pub(crate) fn pop_or_hold(&mut self) -> f32 {
        let channel = self.phase;
        let sample = match self.samples.pop_front() {
            Some(sample) => {
                self.last_frame[channel] = sample;
                sample
            }
            None => self.last_frame[channel],
        };
        self.phase += 1;
        if self.phase == self.channels {
            self.phase = 0;
        }
        sample
    }

    /// Drains `out` from the queue, returning whether the queue underran.
    ///
    /// This is the exact underrun step the device callbacks drive one sample
    /// at a time; it exists as a slice form so it can be unit-tested without
    /// a device.
    #[cfg(test)]
    pub(crate) fn drain(&mut self, out: &mut [f32]) -> bool {
        let underrun = self.samples.len() < out.len();
        for slot in out.iter_mut() {
            *slot = self.pop_or_hold();
        }
        underrun
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small queue so the policy numbers stay small in assertions.
    fn queue() -> AudioQueue {
        AudioQueue::new(1_000, 1)
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
        let capacity = 1_000 * MAX_BUFFER_MS / 1_000;
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
    fn an_underrun_repeats_the_last_sample_instead_of_a_hard_zero() {
        let mut queue = queue();
        queue.push(&[0.6, -0.6]);
        let mut out = [0.0; 5];
        assert!(queue.drain(&mut out));
        assert_eq!(out, [0.6, -0.6, -0.6, -0.6, -0.6]);
    }

    #[test]
    fn an_empty_callback_continues_the_previous_value() {
        let mut queue = queue();
        queue.push(&[0.75]);
        let mut first = [0.0; 1];
        assert!(!queue.drain(&mut first));
        assert_eq!(first, [0.75]);

        let mut second = [0.0; 4];
        assert!(queue.drain(&mut second));
        assert_eq!(second, [0.75; 4], "an empty callback keeps the last value");
    }

    #[test]
    fn a_hold_keeps_each_channel_separate() {
        // A stereo underrun must not smear the left channel's last value into
        // the right channel (or vice versa).
        let mut queue = AudioQueue::new(1_000, 2);
        queue.push(&[0.3, 0.4]);
        let mut out = [0.0; 4];
        assert!(queue.drain(&mut out));
        assert_eq!(out, [0.3, 0.4, 0.3, 0.4]);
    }

    #[test]
    fn drain_reports_no_underrun_when_the_buffer_is_covered() {
        let mut queue = queue();
        queue.push(&[1.0, 2.0]);
        let mut out = [0.0; 2];
        assert!(!queue.drain(&mut out));
        assert_eq!(out, [1.0, 2.0]);
    }
}
