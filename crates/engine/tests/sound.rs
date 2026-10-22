//! Integration tests for the L2 sound model and software mixer
//! (`engine::sound`).
//!
//! Headless by construction: the mixer never opens a device, so every test
//! here drives playback with `Mixer::advance`/`advance_to` and renders into
//! a caller-owned buffer. The reference behaviour is `main`'s
//! `crates/tvp-sound/src/{mixer,waveflags}.rs`.

use std::f64::consts::PI;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use api::media::{AudioSpec, DecodedAudio};
use engine::sound::{
    AudioStream, LOOP_FLAG_COUNT, LoopCondition, LoopFlags, LoopInfo, LoopLink,
    MAX_LOOP_FLAG_VALUE, MAX_VOLUME, Mixer, SoundBuffer, SoundError, SoundLabel, SoundSource,
    Status, mix_source,
};

fn out_spec(sample_rate: u32, channels: u32) -> AudioSpec {
    AudioSpec {
        sample_rate,
        channels,
        total_frames: None,
    }
}

/// A 440 Hz sine tone as a decoded source, optionally with loop info.
fn tone_with_loops(
    rate: u32,
    channels: u32,
    seconds: f64,
    info: Option<LoopInfo>,
) -> Arc<SoundSource> {
    let frames = (rate as f64 * seconds) as usize;
    let mut interleaved =
        Vec::with_capacity(frames * usize::try_from(channels).expect("channel count fits"));
    for f in 0..frames {
        let value = (2.0 * PI * 440.0 * f as f64 / rate as f64).sin() as f32 * 0.5;
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

fn tone(rate: u32, channels: u32, seconds: f64) -> Arc<SoundSource> {
    tone_with_loops(rate, channels, seconds, None)
}

/// A looping tone whose loop link jumps from `from_sec` back to `to_sec`.
fn tone_with_loop(rate: u32, seconds: f64, from_sec: f64, to_sec: f64) -> Arc<SoundSource> {
    let info = LoopInfo::new(
        vec![LoopLink::new(
            (from_sec * f64::from(rate)) as u64,
            (to_sec * f64::from(rate)) as u64,
            false,
            LoopCondition::None,
            0,
            0,
        )],
        Vec::new(),
    );
    tone_with_loops(rate, 1, seconds, Some(info))
}

#[test]
fn play_advance_reaches_done() {
    let mut mixer = Mixer::new();
    let id = mixer.spawn_channel();
    {
        let channel = mixer.channel(id).unwrap();
        channel.play(tone(44100, 1, 1.0));
        assert!(channel.is_playing());
        assert!(!channel.is_done());
        assert_eq!(channel.status(), Status::Play);
    }
    mixer.advance(0.4);
    let channel = mixer.channel_ref(id).unwrap();
    assert!((channel.position_seconds() - 0.4).abs() < 1e-9);
    assert!(channel.is_playing());
    mixer.advance(0.7); // total 1.1 > 1.0
    let channel = mixer.channel_ref(id).unwrap();
    assert!(!channel.is_playing());
    assert!(channel.is_done());
    assert_eq!(channel.status(), Status::Stop);
    assert_eq!(channel.position_seconds(), 1.0);
}

#[test]
fn loop_wraps_position() {
    let mut mixer = Mixer::new();
    let id = mixer.spawn_channel();
    mixer.channel(id).unwrap().play(tone(44100, 2, 1.0));
    mixer.channel(id).unwrap().looping = true;
    mixer.advance(1.2);
    let channel = mixer.channel_ref(id).unwrap();
    assert!(channel.is_playing(), "looping channel keeps playing");
    assert!(!channel.is_done());
    assert!(
        (channel.position_seconds() - 0.2).abs() < 1e-9,
        "position wraps: {}",
        channel.position_seconds()
    );
    mixer.advance(2.5); // wraps twice more
    let channel = mixer.channel_ref(id).unwrap();
    assert!(
        (channel.position_seconds() - 0.7).abs() < 1e-9,
        "position wraps: {}",
        channel.position_seconds()
    );
    assert!(channel.is_playing());
}

#[test]
fn pause_resume_stop() {
    let mut mixer = Mixer::new();
    let id = mixer.spawn_channel();
    mixer.channel(id).unwrap().play(tone(44100, 1, 2.0));
    mixer.advance(1.0);
    mixer.channel(id).unwrap().pause();
    assert!(!mixer.channel_ref(id).unwrap().is_playing());
    mixer.advance(0.5); // paused: no movement
    assert_eq!(mixer.channel_ref(id).unwrap().position_seconds(), 1.0);
    mixer.channel(id).unwrap().resume();
    mixer.advance(0.5);
    assert!((mixer.channel_ref(id).unwrap().position_seconds() - 1.5).abs() < 1e-9);
    mixer.channel(id).unwrap().stop();
    let channel = mixer.channel_ref(id).unwrap();
    assert!(!channel.is_playing());
    assert!(!channel.is_done());
}

#[test]
fn volume_pan_clamp_and_render() {
    let mut mixer = Mixer::new();
    let id = mixer.spawn_channel();
    {
        let channel = mixer.channel(id).unwrap();
        channel.play(tone(44100, 2, 0.1));
        channel.set_volume(2.0);
        assert_eq!(channel.volume(), MAX_VOLUME);
        channel.set_volume(-1.0);
        assert_eq!(channel.volume(), 0.0);
        channel.set_volume(0.5);
        channel.set_pan(5.0);
        assert_eq!(channel.pan(), 1.0);
        channel.set_pan(-5.0);
        assert_eq!(channel.pan(), -1.0);
        channel.set_pan(0.0);
        assert_eq!(channel.effective_volume(), 0.5);
    }
    {
        let channel = mixer.channel(id).unwrap();
        channel.set_volume(1.0);
        channel.set_pan(-1.0);
    }
    let mut out = vec![0.0f32; 4410];
    mixer.mix(&mut out, out_spec(44100, 2)).unwrap();
    let left_peak = out.iter().step_by(2).fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(
        left_peak > 0.3,
        "left channel carries the tone: {left_peak}"
    );
    let right_peak = out
        .iter()
        .skip(1)
        .step_by(2)
        .fold(0.0f32, |m, s| m.max(s.abs()));
    assert_eq!(right_peak, 0.0, "right channel silenced at pan -1");

    // A stereo source at pan 0 renders both channels.
    mixer.channel(id).unwrap().set_pan(0.0);
    mixer.mix(&mut out, out_spec(44100, 2)).unwrap();
    assert!(
        (out[50] - out[51]).abs() < 1e-6,
        "stereo source, pan 0: channels equal"
    );
    assert!(out[50].abs() > 0.3, "tone audible at pan 0: {}", out[50]);
}

#[test]
fn fade_ramps_and_completes() {
    let mut mixer = Mixer::new();
    let id = mixer.spawn_channel();
    let channel = mixer.channel(id).unwrap();
    channel.play(tone(44100, 1, 2.0));
    channel.fade(0.0, 1.0, 0.0); // fade out over 1s
    mixer.advance(0.5);
    let channel = mixer.channel_ref(id).unwrap();
    assert!(
        (channel.effective_volume() - 0.5).abs() < 0.01,
        "halfway fade: {}",
        channel.effective_volume()
    );
    assert!(!channel.fade_finished());
    mixer.advance(0.5);
    let channel = mixer.channel_ref(id).unwrap();
    assert_eq!(channel.volume(), 0.0, "fade completes at target");
    assert!(channel.active_fade().is_none());
    assert!(channel.fade_finished());
    assert!(channel.is_playing(), "fade-out does not stop playback");
}

#[test]
fn render_mix_sums_channels() {
    let mut mixer = Mixer::new();
    let a = mixer.spawn_channel();
    let b = mixer.spawn_channel();
    let source = tone(44100, 1, 0.1);
    mixer.channel(a).unwrap().play(source.clone());
    mixer.channel(b).unwrap().play(source);
    mixer.channel(a).unwrap().set_volume(0.25);
    mixer.channel(b).unwrap().set_volume(0.75);

    let mut out = vec![0.0f32; 4410];
    mixer.mix(&mut out, out_spec(44100, 1)).unwrap();
    let mono_peak = out.iter().map(|s| s.abs()).fold(0.0f32, f32::max);

    let mut single = vec![0.0f32; 4410];
    let mut other = Mixer::new();
    let s = other.spawn_channel();
    other.channel(s).unwrap().play(tone(44100, 1, 0.1));
    other.mix(&mut single, out_spec(44100, 1)).unwrap();
    let single_peak = single.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    assert!(
        (mono_peak - single_peak).abs() < 1e-4,
        "{mono_peak} vs {single_peak}"
    );
}

/// A 48 kHz source rendered on a 44.1 kHz device must keep its pitch
/// (linear interpolation, not a sample-rate change).
#[test]
fn render_resamples_48k_to_44k_at_correct_pitch() {
    let mut mixer = Mixer::new();
    let id = mixer.spawn_channel();
    mixer.channel(id).unwrap().play(tone(48000, 1, 1.0));
    let mut out = vec![0.0f32; 44100];
    mixer.mix(&mut out, out_spec(44100, 1)).unwrap();
    let crossings = out
        .windows(2)
        .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
        .count();
    let freq = crossings as f64 / 2.0;
    assert!(
        (freq - 440.0).abs() < 15.0,
        "resampled pitch {freq} Hz, expected ~440"
    );
    let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(peak > 0.4, "resampled peak {peak}");
}

#[test]
fn loop_info_wraps_into_the_loop_region() {
    let mut mixer = Mixer::new();
    let id = mixer.spawn_channel();
    // 2.0s track, loop from sample 1.0s back to 0.5s.
    mixer
        .channel(id)
        .unwrap()
        .play(tone_with_loop(44100, 2.0, 1.0, 0.5));
    mixer.channel(id).unwrap().looping = true;

    // Cross the loop end (1.0s) by 0.1s -> 0.5 + 0.1 = 0.6s.
    mixer.advance(1.1);
    let channel = mixer.channel_ref(id).unwrap();
    assert!(channel.is_playing(), "loop keeps playing");
    assert!(
        (channel.position_seconds() - 0.6).abs() < 1e-6,
        "looped to loop start + overshoot, got {}",
        channel.position_seconds()
    );

    // A second pass wraps again.
    mixer.advance(0.5);
    let channel = mixer.channel_ref(id).unwrap();
    assert!(
        (channel.position_seconds() - 0.6).abs() < 1e-6,
        "second wrap, got {}",
        channel.position_seconds()
    );
}

#[test]
fn conditional_loop_link_uses_channel_flags() {
    // A conditional link (flag 0 == 1) to 0.8s listed first, and an
    // unconditional fallback to 0.5s.
    let info = LoopInfo::new(
        vec![
            LoopLink::new(44100, 35280, false, LoopCondition::Equal, 1, 0),
            LoopLink::new(44100, 22050, false, LoopCondition::None, 0, 0),
        ],
        Vec::new(),
    );
    let track = tone_with_loops(44100, 1, 2.0, Some(info));
    let mut mixer = Mixer::new();
    let id = mixer.spawn_channel();
    mixer.channel(id).unwrap().play(track);
    mixer.channel(id).unwrap().looping = true;

    // Flag clear -> fallback link to 0.5s.
    mixer.advance(1.1);
    assert!(
        (mixer.channel_ref(id).unwrap().position_seconds() - 0.6).abs() < 1e-6,
        "fallback link, got {}",
        mixer.channel_ref(id).unwrap().position_seconds()
    );

    // Flag set -> conditional link to 0.8s.
    mixer.channel(id).unwrap().flags_mut().set(0, 1);
    mixer.advance(0.5);
    assert!(
        (mixer.channel_ref(id).unwrap().position_seconds() - 0.9).abs() < 1e-6,
        "conditional link, got {}",
        mixer.channel_ref(id).unwrap().position_seconds()
    );
}

#[test]
fn rate_advances_position_faster() {
    let mut mixer = Mixer::new();
    let id = mixer.spawn_channel();
    let channel = mixer.channel(id).unwrap();
    channel.play(tone(44100, 1, 2.0));
    channel.set_rate(2.0);
    mixer.advance(0.5);
    assert!(
        (mixer.channel_ref(id).unwrap().position_seconds() - 1.0).abs() < 1e-9,
        "rate 2.0 plays 1.0s of source in 0.5s"
    );
}

#[test]
fn advance_to_absolute_clock() {
    let mut mixer = Mixer::new();
    let id = mixer.spawn_channel();
    mixer.channel(id).unwrap().play(tone(44100, 1, 5.0));
    mixer.advance_to(1.0);
    assert!((mixer.channel_ref(id).unwrap().position_seconds() - 1.0).abs() < 1e-9);
    mixer.advance_to(2.5);
    assert!((mixer.channel_ref(id).unwrap().position_seconds() - 2.5).abs() < 1e-9);
    mixer.advance_to(2.0); // clock must never go backwards
    assert_eq!(mixer.channel_ref(id).unwrap().position_seconds(), 2.5);
}

#[test]
fn mix_advancing_moves_positions_by_the_chunk() {
    let mut mixer = Mixer::new();
    let id = mixer.spawn_channel();
    mixer.channel(id).unwrap().play(tone(44100, 1, 2.0));
    // 4410 mono frames at 44.1 kHz is exactly 0.1 s.
    let mut out = vec![0.0f32; 4410];
    mixer.mix_advancing(&mut out, out_spec(44100, 1)).unwrap();
    assert!((mixer.clock() - 0.1).abs() < 1e-9);
    assert!(
        (mixer.channel_ref(id).unwrap().position_seconds() - 0.1).abs() < 1e-9,
        "device-driven chunk advances the position: {}",
        mixer.channel_ref(id).unwrap().position_seconds()
    );
}

#[test]
fn volume2_and_global_volume_multiply_the_gain() {
    let mut mixer = Mixer::new();
    let id = mixer.spawn_channel();
    {
        let channel = mixer.channel(id).unwrap();
        channel.play(tone(44100, 1, 1.0));
        channel.set_volume(1.0);
        channel.set_volume2(0.5);
        // GetVolume reports the base volume only.
        assert_eq!(channel.effective_volume(), 1.0);
        assert_eq!(channel.volume2(), 0.5);
        assert!((channel.output_gain() - 0.5).abs() < 1e-6);
    }
    mixer.set_global_volume(0.5);
    assert!((mixer.global_volume() - 0.5).abs() < 1e-6);

    let mut out = vec![0.0f32; 4410];
    mixer.mix(&mut out, out_spec(44100, 1)).unwrap();
    let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(
        (0.1..=0.13).contains(&peak),
        "tone at gain 0.25 peak ~0.125, got {peak}"
    );
}

#[test]
fn spatial_attenuation_matches_directsound_defaults() {
    let mut mixer = Mixer::new();
    let id = mixer.spawn_channel();
    let channel = mixer.channel(id).unwrap();
    channel.set_pos(0.0, 0.0, 0.0);
    assert_eq!(channel.spatial_attenuation(), 1.0);
    channel.set_pos(0.5, 0.0, 0.0);
    assert_eq!(channel.spatial_attenuation(), 1.0);
    channel.set_pos(2.0, 0.0, 0.0);
    assert!((channel.spatial_attenuation() - 0.5).abs() < 1e-6);
    channel.set_pos(0.0, 0.0, 3.0);
    assert!((channel.spatial_attenuation() - 1.0 / 3.0).abs() < 1e-6);
    channel.set_volume(1.0);
    channel.set_volume2(1.0);
    channel.set_pos(3.0, 0.0, 0.0);
    assert!((channel.output_gain() - 1.0 / 3.0).abs() < 1e-6);
}

#[test]
fn global_focus_mode_mutes_by_host_state() {
    let mut mixer = Mixer::new();
    assert_eq!(mixer.focus_gain(), 1.0);
    mixer.app_minimized = true;
    assert_eq!(mixer.focus_gain(), 1.0);
    mixer.set_global_focus_mode(1);
    assert_eq!(mixer.focus_gain(), 0.0);
    mixer.app_minimized = false;
    assert_eq!(mixer.focus_gain(), 1.0);
    mixer.set_global_focus_mode(2);
    mixer.app_focused = false;
    assert_eq!(mixer.focus_gain(), 0.0);
    mixer.app_focused = true;
    mixer.app_minimized = true;
    assert_eq!(mixer.focus_gain(), 0.0);
    mixer.app_minimized = false;
    assert_eq!(mixer.focus_gain(), 1.0);
    mixer.set_global_focus_mode(9);
    assert_eq!(mixer.global_focus_mode(), 2);
    mixer.set_global_focus_mode(-1);
    assert_eq!(mixer.global_focus_mode(), 0);
}

#[test]
fn loop_flags_clamp_and_reset() {
    let mut flags = LoopFlags::new();
    assert_eq!(flags.count(), LOOP_FLAG_COUNT);
    flags.set(0, 12_345);
    assert_eq!(flags.get(0), MAX_LOOP_FLAG_VALUE, "clamped to the max");
    flags.set(1, -5);
    assert_eq!(flags.get(1), 0, "clamped up to 0");
    flags.set(99, 1); // out of range ignored
    assert_eq!(flags.get(99), 0);
    flags.reset();
    assert_eq!(flags.get(0), 0);
}

#[test]
fn mix_rejects_invalid_output_spec() {
    let mixer = Mixer::new();
    let mut out = vec![0.0f32; 4];
    assert!(matches!(
        mixer.mix(&mut out, out_spec(0, 2)),
        Err(SoundError::InvalidSpec { .. })
    ));
    assert!(matches!(
        mixer.mix(&mut out, out_spec(44100, 3)),
        Err(SoundError::InvalidSpec { .. })
    ));
    assert!(matches!(
        mixer.mix(&mut out, out_spec(44100, 0)),
        Err(SoundError::InvalidSpec { .. })
    ));
}

#[test]
fn sound_source_rejects_bad_buffers() {
    let zero_rate = Arc::new(DecodedAudio::new(
        AudioSpec {
            sample_rate: 0,
            channels: 1,
            total_frames: None,
        },
        Vec::new(),
    ));
    assert!(matches!(
        SoundSource::new(zero_rate),
        Err(SoundError::InvalidSpec { .. })
    ));

    let three_channels = Arc::new(DecodedAudio::new(
        AudioSpec {
            sample_rate: 44100,
            channels: 3,
            total_frames: None,
        },
        vec![0.0; 3],
    ));
    assert!(matches!(
        SoundSource::new(three_channels),
        Err(SoundError::UnsupportedChannels { channels: 3 })
    ));

    // A partial trailing frame makes the buffer inconsistent.
    let ragged = Arc::new(DecodedAudio::new(
        AudioSpec {
            sample_rate: 44100,
            channels: 2,
            total_frames: None,
        },
        vec![0.0, 0.0, 0.0],
    ));
    assert!(matches!(
        SoundSource::new(ragged),
        Err(SoundError::InconsistentBuffer { .. })
    ));
}

/// The frame count a decoder states is the container's own, so a source's
/// length does not depend on which decoder produced the samples.
///
/// A decoder that reports the length the container states keeps it: the
/// source's frame count is the container's, and the frames past it are not
/// played. A spec without a frame count falls back to the buffer's length.
#[test]
fn a_materialized_source_keeps_the_stated_frame_count() {
    let stated = 1_000u64;
    let spec = AudioSpec {
        sample_rate: 1_000,
        channels: 1,
        total_frames: Some(stated),
    };
    let buffer = vec![0.0f32; 1_500];
    let source = SoundSource::new(Arc::new(DecodedAudio::new(spec, buffer))).expect("valid source");
    assert_eq!(
        source.frames(),
        stated,
        "the container's frame count is the source's length"
    );
    assert!(
        source.frame(stated).expect("a frame").is_none(),
        "the frames past the container's end are not played"
    );

    let spec = AudioSpec {
        sample_rate: 1_000,
        channels: 1,
        total_frames: None,
    };
    let buffer = vec![0.0f32; 1_500];
    let source = SoundSource::new(Arc::new(DecodedAudio::new(spec, buffer))).expect("valid source");
    assert_eq!(
        source.frames(),
        1_500,
        "a spec without a frame count falls back to the buffer's length"
    );
}

#[test]
fn mix_source_accumulates_into_the_caller_buffer() {
    let source = tone(44100, 1, 0.05);
    let mut out = vec![0.0f32; 2205];
    mix_source(&source, 0.0, 1.0, 1.0, 0.0, &mut out, out_spec(44100, 1)).unwrap();
    let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(peak > 0.3, "source audible: {peak}");

    // The output spec is validated here too.
    assert!(matches!(
        mix_source(&source, 0.0, 1.0, 1.0, 0.0, &mut out, out_spec(0, 1)),
        Err(SoundError::InvalidSpec { .. })
    ));
}

/// The number of source frames the kernel convolves per output frame.
const RESAMPLE_TAPS: usize = 65;

/// The frames the kernel reaches back from the interpolated frame.
const RESAMPLE_HALF: usize = RESAMPLE_TAPS / 2;

/// The Blackman window over the kernel.
fn resample_window() -> [f64; RESAMPLE_TAPS] {
    let mut window = [0.0f64; RESAMPLE_TAPS];
    for (k, value) in window.iter_mut().enumerate() {
        let t = k as f64 / (RESAMPLE_TAPS - 1) as f64;
        let phase = std::f64::consts::TAU * t;
        *value = 0.42 - 0.5 * phase.cos() + 0.08 * (2.0 * phase).cos();
    }
    window
}

/// The scale of the kernel's passband.
fn kernel_scale(src_rate: f64, out_rate: f64, rate: f64) -> f64 {
    let ratio = out_rate / (rate * src_rate);
    if ratio.is_finite() && ratio > 0.0 {
        ratio.min(1.0)
    } else {
        1.0
    }
}

/// The windowed-sinc weights for the fractional source position `frac`.
///
/// The kernel's sine is the difference of two products, so the per-tap values
/// are computed once per conversion.
fn resample_weights(
    frac: f64,
    window: &[f64; RESAMPLE_TAPS],
    scale: f64,
    sine: &[f64; RESAMPLE_TAPS],
    cosine: &[f64; RESAMPLE_TAPS],
) -> [f64; RESAMPLE_TAPS] {
    let phase = std::f64::consts::PI * scale * frac;
    let (sin, cos) = (phase.sin(), phase.cos());
    let mut weights = [0.0f64; RESAMPLE_TAPS];
    for (k, weight) in weights.iter_mut().enumerate() {
        let m = k as f64 - RESAMPLE_HALF as f64;
        let distance = scale * (m - frac);
        let sinc = if distance.abs() < f64::EPSILON {
            1.0
        } else {
            (sine[k] * cos - cosine[k] * sin) / (std::f64::consts::PI * distance)
        };
        *weight = sinc * window[k];
    }
    weights
}

/// The per-tap sine and cosine of the kernel's phase.
fn resample_phases(scale: f64) -> ([f64; RESAMPLE_TAPS], [f64; RESAMPLE_TAPS]) {
    let mut sine = [0.0f64; RESAMPLE_TAPS];
    let mut cosine = [0.0f64; RESAMPLE_TAPS];
    for (k, value) in sine.iter_mut().enumerate() {
        let m = k as f64 - RESAMPLE_HALF as f64;
        let phase = std::f64::consts::PI * scale * m;
        *value = phase.sin();
        cosine[k] = phase.cos();
    }
    (sine, cosine)
}

/// Resample `source` into `out` one frame at a time with `SoundSource::frame`.
/// This is the per-frame reference the bulk window path must stay
/// byte-identical to: the bulk path reads a window of source frames and
/// convolves it, this one reads the frames the kernel needs one by one.
fn mix_source_per_frame(
    source: &SoundSource,
    position_seconds: f64,
    rate: f64,
    gain: f32,
    pan: f32,
    out: &mut [f32],
    out_spec: AudioSpec,
) {
    let out_channels = usize::try_from(out_spec.channels).expect("channel count fits");
    let out_rate = f64::from(out_spec.sample_rate);
    let src_rate = f64::from(source.sample_rate().max(1));
    let rate = if rate.is_finite() { rate } else { 1.0 };
    let p = pan.clamp(-1.0, 1.0);
    let (left_gain, right_gain) = (1.0 - p.max(0.0), 1.0 - (-p).max(0.0));
    let window = resample_window();
    let scale = kernel_scale(src_rate, out_rate, rate);
    let (sine, cosine) = resample_phases(scale);
    for (index, frame) in out.chunks_exact_mut(out_channels).enumerate() {
        let t = position_seconds + index as f64 / out_rate * rate;
        let sample_pos = (t * src_rate).max(0.0);
        if !sample_pos.is_finite() || sample_pos >= u64::MAX as f64 {
            continue;
        }
        let i0 = sample_pos.floor();
        let i0u = i0 as u64;
        let frac = sample_pos - i0;
        let weights = resample_weights(frac, &window, scale, &sine, &cosine);
        let mut left = 0.0f64;
        let mut right = 0.0f64;
        let mut weight_sum = 0.0f64;
        for (k, weight) in weights.iter().enumerate() {
            let frame_index = i0u as i64 - RESAMPLE_HALF as i64 + k as i64;
            if frame_index < 0 {
                continue;
            }
            let Some((a, b)) = source.frame(frame_index as u64).expect("read") else {
                continue;
            };
            left += weight * f64::from(a);
            right += weight * f64::from(b);
            weight_sum += weight;
        }
        if weight_sum.abs() > f64::EPSILON {
            left /= weight_sum;
            right /= weight_sum;
        }
        let left = left as f32;
        let right = right as f32;
        if out_channels == 1 {
            frame[0] += (left * left_gain + right * right_gain) * 0.5 * gain;
        } else {
            frame[0] += left * gain * left_gain;
            frame[1] += right * gain * right_gain;
        }
    }
}

/// The mixer pulls a contiguous window of source frames under one lock and
/// interpolates from it. The mixed PCM must not change: the bulk path and the
/// per-frame path must produce byte-identical samples, including at the source
/// end, across resampling ratios, gains and pans.
#[test]
fn bulk_window_matches_the_per_frame_path() {
    for (src_channels, src_rate, out_spec) in [
        (2u32, 44_100u32, out_spec(48_000, 2)),
        (1, 44_100, out_spec(48_000, 1)),
        (2, 48_000, out_spec(22_050, 2)),
    ] {
        let frames = 200_000u64;
        let channel_count = usize::try_from(src_channels).expect("channel count fits");
        let interleaved: Vec<f32> = (0..frames)
            .flat_map(|f| {
                let base = f as f32 * 0.001;
                (0..channel_count).map(move |c| if c == 0 { base } else { -base })
            })
            .collect();
        let spec = AudioSpec {
            sample_rate: src_rate,
            channels: src_channels,
            total_frames: Some(frames),
        };
        let source =
            SoundSource::new(Arc::new(DecodedAudio::new(spec, interleaved))).expect("valid source");

        for position in [0.0f64, 0.5, 1.25, 4.0] {
            for rate in [0.5f64, 1.0, 1.5, 2.0] {
                for (gain, pan) in [(1.0f32, 0.0f32), (0.5, -0.25), (0.75, 0.8)] {
                    let out_frames = 4096;
                    let mut bulk = vec![0.0f32; out_frames * 2];
                    mix_source(&source, position, rate, gain, pan, &mut bulk, out_spec).unwrap();
                    let mut per_frame = vec![0.0f32; out_frames * 2];
                    mix_source_per_frame(
                        &source,
                        position,
                        rate,
                        gain,
                        pan,
                        &mut per_frame,
                        out_spec,
                    );
                    assert_eq!(
                        bulk, per_frame,
                        "bulk window differs at {position}s rate {rate} gain {gain} pan {pan}"
                    );
                }
            }
        }
    }
}

#[test]
fn two_mixers_do_not_share_state() {
    let mut first = Mixer::new();
    let mut second = Mixer::new();
    let first_id = first.spawn_channel();
    let second_id = second.spawn_channel();
    first.channel(first_id).unwrap().play(tone(44100, 1, 1.0));
    assert!(first.channel_ref(first_id).unwrap().is_playing());
    assert!(!second.channel_ref(second_id).unwrap().is_playing());
    first.set_global_volume(0.25);
    assert_eq!(second.global_volume(), MAX_VOLUME);
    first.advance(0.5);
    assert_eq!(second.clock(), 0.0);
}

#[test]
fn buffer_poll_reports_status_change() {
    let mut buffer = SoundBuffer::new();
    let mut mixer = Mixer::new();
    let id = mixer.spawn_channel();
    buffer.open(tone(44100, 1, 1.0));
    buffer.play_on(mixer.channel(id).unwrap()).unwrap();

    let events = buffer.poll(mixer.channel(id).unwrap());
    assert!(
        events.contains(&engine::sound::SoundEvent::StatusChanged(Status::Play)),
        "status becomes play: {events:?}"
    );

    mixer.advance(1.2);
    let events = buffer.poll(mixer.channel(id).unwrap());
    assert!(
        events.contains(&engine::sound::SoundEvent::StatusChanged(Status::Stop)),
        "status becomes stop at end: {events:?}"
    );
}

#[test]
fn buffer_poll_fires_loop_labels_once_per_pass() {
    let loops = LoopInfo::new(
        Vec::new(),
        vec![SoundLabel::new(22_050, "chorus".to_string())], // 0.5s at 44.1 kHz
    );
    let mut buffer = SoundBuffer::new();
    let mut mixer = Mixer::new();
    let id = mixer.spawn_channel();
    buffer.open(tone_with_loops(44100, 1, 1.0, Some(loops)));
    buffer.play_on(mixer.channel(id).unwrap()).unwrap();
    // Establish the status baseline.
    let _ = buffer.poll(mixer.channel(id).unwrap());

    mixer.advance(0.6);
    let events = buffer.poll(mixer.channel(id).unwrap());
    assert!(
        events.contains(&engine::sound::SoundEvent::Label("chorus".to_string())),
        "label fires: {events:?}"
    );

    // Polling again without moving must not re-fire it.
    let events = buffer.poll(mixer.channel(id).unwrap());
    assert!(
        !events.contains(&engine::sound::SoundEvent::Label("chorus".to_string())),
        "label does not repeat: {events:?}"
    );
}

#[test]
fn buffer_play_on_carries_flags_to_the_channel() {
    let mut buffer = SoundBuffer::new();
    let mut mixer = Mixer::new();
    let id = mixer.spawn_channel();
    buffer.open(tone(44100, 1, 1.0));
    buffer.flags_mut().set(3, 7);
    buffer.play_on(mixer.channel(id).unwrap()).unwrap();
    assert_eq!(mixer.channel_ref(id).unwrap().flags().get(3), 7);
    assert!(matches!(
        SoundBuffer::new().play_on(mixer.channel(id).unwrap()),
        Err(SoundError::NoSource)
    ));
}

/// The mixer must be sampled at its current position *before* it is advanced.
/// A frame loop that advances the mixer to the wall clock and then renders
/// `dt` renders from a position that moved by more (or less) than the rendered
/// count whenever `dt` jitters, so consecutive chunks leave a gap (or overlap)
/// of `dt_k − dt_{k−1}` frames — a click at the frame rate. The reference mixes
/// on the fixed-rate device callback
/// (`reference/cpp/core/sound/win32/WaveMixer.cpp`), which has no such jitter;
/// this port makes the mixer render-driven so the cursor advances by exactly
/// the frames rendered ([`Mixer::mix_advancing`]) and the chunks tile.
#[test]
fn the_mixer_must_be_sampled_before_it_is_advanced() {
    let rate = 1000u32;
    let frames = 100_000usize;
    // A ramp: source frame f has value f, so a continuous render of output
    // index m must read source frame m.
    let interleaved: Vec<f32> = (0..frames).map(|f| f as f32).collect();
    let spec = out_spec(rate, 1);
    let source =
        Arc::new(SoundSource::new(Arc::new(DecodedAudio::new(spec, interleaved))).expect("source"));
    let dts = [0.010, 0.031, 0.012, 0.029, 0.011, 0.033, 0.014, 0.027];

    // Render-then-advance: output index m reads source frame m.
    let mut rendered = Vec::new();
    let mut position = 0.0f64;
    for dt in dts {
        let n = (dt * f64::from(rate)).round() as usize;
        let mut out = vec![0.0f32; n];
        mix_source(&source, position, 1.0, 1.0, 0.0, &mut out, spec).unwrap();
        rendered.extend_from_slice(&out);
        position += dt;
    }
    for (m, sample) in rendered.iter().enumerate() {
        assert!(
            (sample - m as f32).abs() < 0.5,
            "render-then-advance output {m} read source frame {sample}"
        );
    }

    // Advance-then-render: the position jumps to the running wall clock, so
    // the chunks no longer tile and a seam appears.
    let mut jittered = Vec::new();
    let mut clock = 0.0f64;
    for dt in dts {
        clock += dt;
        let n = (dt * f64::from(rate)).round() as usize;
        let mut out = vec![0.0f32; n];
        mix_source(&source, clock, 1.0, 1.0, 0.0, &mut out, spec).unwrap();
        jittered.extend_from_slice(&out);
    }
    assert!(
        jittered
            .iter()
            .enumerate()
            .any(|(m, sample)| (sample - m as f32).abs() >= 0.5),
        "advance-then-render must leave a seam under jitter"
    );
}

// ---------------------------------------------------------------------------
// Incremental (streaming) sources
// ---------------------------------------------------------------------------

/// A decoded PCM ramp behind an [`AudioStream`], counting decoder work.
struct CountingStream {
    samples: Vec<f32>,
    channels: usize,
    position: u64,
    spec: AudioSpec,
    reads: Arc<AtomicUsize>,
    seeks: Arc<AtomicUsize>,
}

impl CountingStream {
    /// A mono/stereo ramp whose frame `f` holds the value `f`.
    fn ramp(rate: u32, channels: u32, frames: u64) -> (Self, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let channel_count = usize::try_from(channels).expect("channel count fits");
        let samples: Vec<f32> = (0..frames)
            .flat_map(|frame| std::iter::repeat_n(frame as f32, channel_count))
            .collect();
        let reads = Arc::new(AtomicUsize::new(0));
        let seeks = Arc::new(AtomicUsize::new(0));
        (
            Self {
                samples,
                channels: channel_count,
                position: 0,
                spec: AudioSpec {
                    sample_rate: rate,
                    channels,
                    total_frames: Some(frames),
                },
                reads: reads.clone(),
                seeks: seeks.clone(),
            },
            reads,
            seeks,
        )
    }
}

impl AudioStream for CountingStream {
    fn spec(&self) -> AudioSpec {
        self.spec
    }

    fn read(&mut self, out: &mut [f32]) -> Result<usize, SoundError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        let start = self.position as usize * self.channels;
        if start >= self.samples.len() {
            return Ok(0);
        }
        let available = self.samples.len() - start;
        let aligned = (out.len().min(available) / self.channels) * self.channels;
        out[..aligned].copy_from_slice(&self.samples[start..start + aligned]);
        self.position += (aligned / self.channels) as u64;
        Ok(aligned)
    }

    fn seek(&mut self, frame: u64) -> Result<(), SoundError> {
        self.seeks.fetch_add(1, Ordering::Relaxed);
        self.position = frame.min(self.samples.len() as u64 / self.channels as u64);
        Ok(())
    }
}

/// A decode counter with a condition variable, so a test can wait for the
/// worker to reach a given read/seek count instead of sleeping.
#[derive(Clone, Default)]
struct DecodeSignal {
    state: Arc<(Mutex<usize>, Condvar)>,
}

impl DecodeSignal {
    fn new() -> Self {
        Self {
            state: Arc::new((Mutex::new(0), Condvar::new())),
        }
    }

    fn record(&self) {
        let (count, ready) = &*self.state;
        *count.lock().unwrap() += 1;
        ready.notify_all();
    }

    fn count(&self) -> usize {
        *self.state.0.lock().unwrap()
    }

    /// Waits until at least `n` calls have been recorded, bounded so a broken
    /// worker fails the test instead of hanging it.
    fn wait_at_least(&self, n: usize) -> bool {
        let (count, ready) = &*self.state;
        let mut current = count.lock().unwrap();
        while *current < n {
            let (guard, timeout) = ready
                .wait_timeout(current, Duration::from_secs(10))
                .unwrap();
            current = guard;
            if timeout.timed_out() && *current < n {
                return false;
            }
        }
        true
    }
}

/// A decoded PCM ramp behind an [`AudioStream`], counting decoder reads and
/// seeks so a test can observe the decode-ahead worker.
struct SignallingStream {
    samples: Vec<f32>,
    channels: usize,
    position: u64,
    spec: AudioSpec,
    reads: DecodeSignal,
    seeks: DecodeSignal,
}

impl SignallingStream {
    /// A mono/stereo ramp whose frame `f` holds the value `f`.
    fn ramp(rate: u32, channels: u32, frames: u64) -> (Self, DecodeSignal, DecodeSignal) {
        let channel_count = usize::try_from(channels).expect("channel count fits");
        let samples: Vec<f32> = (0..frames)
            .flat_map(|frame| std::iter::repeat_n(frame as f32, channel_count))
            .collect();
        let reads = DecodeSignal::new();
        let seeks = DecodeSignal::new();
        (
            Self {
                samples,
                channels: channel_count,
                position: 0,
                spec: AudioSpec {
                    sample_rate: rate,
                    channels,
                    total_frames: Some(frames),
                },
                reads: reads.clone(),
                seeks: seeks.clone(),
            },
            reads,
            seeks,
        )
    }
}

impl AudioStream for SignallingStream {
    fn spec(&self) -> AudioSpec {
        self.spec
    }

    fn read(&mut self, out: &mut [f32]) -> Result<usize, SoundError> {
        self.reads.record();
        let start = self.position as usize * self.channels;
        if start >= self.samples.len() {
            return Ok(0);
        }
        let available = self.samples.len() - start;
        let aligned = (out.len().min(available) / self.channels) * self.channels;
        out[..aligned].copy_from_slice(&self.samples[start..start + aligned]);
        self.position += (aligned / self.channels) as u64;
        Ok(aligned)
    }

    fn seek(&mut self, frame: u64) -> Result<(), SoundError> {
        self.seeks.record();
        self.position = frame.min(self.samples.len() as u64 / self.channels as u64);
        Ok(())
    }
}

/// Reference: `Open` creates the decoder, builds the filter chain and reads
/// `FilterOutput->GetFormat()` for the duration
/// (`reference/cpp/core/sound/win32/WaveImpl.cpp:3043-3056`); the decode
/// thread fills the L2 buffer only once playback starts
/// (`WaveImpl.cpp:1823-1866`, `2389-2463`). Opening a source therefore decodes
/// no samples.
#[test]
fn streaming_source_decodes_nothing_at_construction() {
    let (stream, reads, seeks) = SignallingStream::ramp(48_000, 2, 4_800_000);
    let spec = AudioSpec {
        sample_rate: 48_000,
        channels: 2,
        total_frames: Some(4_800_000),
    };
    let source = SoundSource::streaming(spec, Box::new(stream), None).expect("streaming source");

    assert_eq!(reads.count(), 0, "open must decode nothing");
    assert_eq!(seeks.count(), 0, "open must not seek");
    assert!(source.is_streaming());
    // The length comes from the container, not from decoding.
    assert_eq!(source.frames(), 4_800_000);
    assert!((source.duration_seconds() - 100.0).abs() < 1e-9);

    // The first frame read starts the worker and returns the decoded sample.
    assert_eq!(source.frame(0).unwrap(), Some((0.0, 0.0)));
    assert!(
        reads.wait_at_least(1),
        "the first frame read must decode a chunk"
    );
}

/// Reference: `tTVPWaveDecoder::Render` decodes forward from the current
/// position and `SetPosition` is the only backwards operation
/// (`reference/cpp/core/sound/WaveIntf.h:84-113`); the sound buffer reseeks
/// on a backwards `SetSamplePosition` and on a loop wrap
/// (`reference/cpp/core/sound/win32/WaveImpl.cpp:3131-3143`, `2458-2463`).
#[test]
fn streaming_source_reseeks_for_a_backwards_read() {
    let (stream, _reads, seeks) = SignallingStream::ramp(8_000, 1, 1_000_000);
    let spec = AudioSpec {
        sample_rate: 8_000,
        channels: 1,
        total_frames: Some(1_000_000),
    };
    let source = SoundSource::streaming(spec, Box::new(stream), None).expect("source");

    // A far-forward frame and the next frame render the ramp values.
    assert_eq!(source.frame(50_000).unwrap().unwrap().0, 50_000.0);
    assert_eq!(source.frame(50_001).unwrap().unwrap().0, 50_001.0);

    // A backwards read reseeks and yields the earlier sample.
    let before = seeks.count();
    assert_eq!(source.frame(10).unwrap().unwrap().0, 10.0);
    assert!(
        seeks.count() > before,
        "a backwards read reseeks (WaveIntf.h:107-113)"
    );

    // A forward gap reseeks rather than decoding through the skipped frames.
    let before = seeks.count();
    assert_eq!(source.frame(900_000).unwrap().unwrap().0, 900_000.0);
    assert!(seeks.count() > before, "a forward gap reseeks");
}

/// Reference: `Render` reports the end by returning fewer samples than
/// requested (`reference/cpp/core/sound/WaveIntf.h:95-105`), and a frame at
/// or past the container's `TotalSamples` is never read
/// (`reference/cpp/core/sound/win32/WaveImpl.cpp:3135-3136`).
#[test]
fn streaming_source_returns_none_past_the_end() {
    let (stream, _reads, _seeks) = CountingStream::ramp(1_000, 1, 100);
    let spec = AudioSpec {
        sample_rate: 1_000,
        channels: 1,
        total_frames: Some(100),
    };
    let source = SoundSource::streaming(spec, Box::new(stream), None).expect("source");
    assert_eq!(source.frame(99).unwrap().unwrap().0, 99.0);
    assert_eq!(source.frame(100).unwrap(), None);
    assert_eq!(source.frame(10_000).unwrap(), None);
}

/// The mixer pulls a contiguous window with one `SoundSource::read_into`, so
/// the bulk read must return the same frames the per-frame path would and stop
/// exactly at the reported end.
#[test]
fn streaming_read_into_copies_a_contiguous_window() {
    let (stream, _reads, _seeks) = CountingStream::ramp(8_000, 1, 20_000);
    let spec = AudioSpec {
        sample_rate: 8_000,
        channels: 1,
        total_frames: Some(20_000),
    };
    let source = SoundSource::streaming(spec, Box::new(stream), None).expect("source");

    let mut out = vec![0.0f32; 2_048 * 2];
    let mut got = 0usize;
    while got < 2_048 {
        let n = source
            .read_into(&mut out[got * 2..], got as u64, 2_048 - got)
            .unwrap();
        assert!(n > 0, "the stream ended early at {got} frames");
        got += n;
    }
    assert_eq!((out[0], out[1]), (0.0, 0.0));
    assert_eq!((out[2_047 * 2], out[2_047 * 2 + 1]), (2_047.0, 2_047.0));

    // A window that crosses the reported end copies only the real frames.
    let mut tail = vec![0.0f32; 4 * 2];
    assert_eq!(source.read_into(&mut tail, 19_998, 4).unwrap(), 2);
    assert_eq!((tail[0], tail[1]), (19_998.0, 19_998.0));
    assert_eq!((tail[2], tail[3]), (19_999.0, 19_999.0));

    // At or past the total nothing is copied.
    assert_eq!(source.read_into(&mut tail, 20_000, 4).unwrap(), 0);
}

/// A stream whose length the container does not report must stop at the
/// decoder's end instead of reseeking for every trailing output frame.
///
/// The per-frame path already returns at `eof` before the out-of-window
/// reseek; the bulk path must too, or a compressed container is reopened once
/// per output frame at the end of playback (a measured multi-second stall).
#[test]
fn streaming_read_into_past_the_end_does_not_reseek() {
    let (stream, _reads, seeks) = CountingStream::ramp(8_000, 1, 4_000);
    // An unknown total is what leaves the reader to discover the end from the
    // worker, exactly as an OGG stream without a duration does.
    let spec = AudioSpec {
        sample_rate: 8_000,
        channels: 1,
        total_frames: None,
    };
    let source = SoundSource::streaming(spec, Box::new(stream), None).expect("source");

    // Drain the window to its end, so the worker has reported `eof`.
    let mut out = vec![0.0f32; 512 * 2];
    let mut got = 0u64;
    loop {
        let n = source.read_into(&mut out, got, 512).unwrap();
        if n == 0 {
            break;
        }
        got += n as u64;
        assert!(
            got <= 4_000,
            "the stream returned more frames than it holds"
        );
    }
    assert_eq!(got, 4_000, "the stream did not drain to its end");

    // Every request at or past the end reports the end without a reseek.
    let seeks_before = seeks.load(Ordering::Relaxed);
    for start in 4_000..4_064 {
        assert_eq!(
            source.read_into(&mut out, start, 2).unwrap(),
            0,
            "a past-the-end read at {start} did not report the end"
        );
    }
    assert_eq!(
        seeks.load(Ordering::Relaxed),
        seeks_before,
        "past-the-end reads reseeked the decoder"
    );
}

/// Reference: the decode thread keeps only a bounded buffer and drops the
/// consumed prefix (`reference/cpp/core/sound/win32/WaveImpl.cpp:2389-2463`
/// fills a fixed L2 buffer); materializing the stream for a filter must not
/// leave a second copy of the file behind.
#[test]
fn decode_entire_releases_the_streaming_window() {
    let (stream, reads, seeks) = CountingStream::ramp(8_000, 1, 100_000);
    let spec = AudioSpec {
        sample_rate: 8_000,
        channels: 1,
        total_frames: Some(100_000),
    };
    let source = SoundSource::streaming(spec, Box::new(stream), None).expect("source");

    let audio = source.decode_entire().expect("decode all");
    assert_eq!(audio.frame_count(), 100_000);
    // The length survives the release, so the container's duration is kept.
    assert_eq!(source.frames(), 100_000);

    // The decoded window was released: a low frame read reseeks and
    // re-decodes instead of being served from a retained whole file.
    let seeks_before = seeks.load(Ordering::Relaxed);
    let reads_before = reads.load(Ordering::Relaxed);
    assert_eq!(source.frame(0).unwrap().unwrap().0, 0.0);
    assert!(
        seeks.load(Ordering::Relaxed) > seeks_before,
        "the released window must be reseeked"
    );
    assert!(
        reads.load(Ordering::Relaxed) > reads_before,
        "the first frame must be decoded again"
    );
}

/// Reference: the decode thread retains only a bounded window behind the
/// play cursor, so a forward read does not keep the whole file
/// (`reference/cpp/core/sound/win32/WaveImpl.cpp:2389-2463`).
#[test]
fn forward_reads_release_the_decoded_prefix() {
    let (stream, _reads, seeks) = CountingStream::ramp(8_000, 1, 1_000_000);
    let spec = AudioSpec {
        sample_rate: 8_000,
        channels: 1,
        total_frames: Some(1_000_000),
    };
    let source = SoundSource::streaming(spec, Box::new(stream), None).expect("source");

    // Read sequentially across several one-second retention windows.
    let end = 8_000u64 * 5;
    for frame in 0..end {
        assert_eq!(source.frame(frame).unwrap().unwrap().0, frame as f32);
    }
    // The consumed prefix is gone: returning to frame 0 must reseek instead
    // of being served from a file-long window.
    let before = seeks.load(Ordering::Relaxed);
    assert_eq!(source.frame(0).unwrap().unwrap().0, 0.0);
    assert!(
        seeks.load(Ordering::Relaxed) > before,
        "the consumed prefix must have been released"
    );
}

/// The mixer reads by absolute frame index, so a streaming source must render
/// exactly like a materialized buffer of the same samples.
#[test]
fn mixer_renders_a_streaming_source_like_a_materialized_one() {
    let rate = 8_000u32;
    let frames = 20_000u64;
    let samples: Vec<f32> = (0..frames).map(|frame| frame as f32).collect();
    let spec = AudioSpec {
        sample_rate: rate,
        channels: 1,
        total_frames: Some(frames),
    };

    let materialized = Arc::new(
        SoundSource::new(Arc::new(DecodedAudio::new(spec, samples))).expect("materialized"),
    );
    let (stream, _reads, _seeks) = CountingStream::ramp(rate, 1, frames);
    let streaming = Arc::new(SoundSource::streaming(spec, Box::new(stream), None).expect("stream"));

    let mut expected = vec![0.0f32; 4_000];
    mix_source(
        &materialized,
        0.25,
        1.0,
        0.5,
        0.0,
        &mut expected,
        out_spec(rate, 1),
    )
    .unwrap();
    let mut observed = vec![0.0f32; 4_000];
    mix_source(
        &streaming,
        0.25,
        1.0,
        0.5,
        0.0,
        &mut observed,
        out_spec(rate, 1),
    )
    .unwrap();
    assert_eq!(
        expected, observed,
        "a streaming source renders like a materialized one"
    );
}

/// Reference: the playing thread reads the decoder thread's L2 buffer and
/// only the decode thread calls the decoder
/// (`reference/cpp/core/sound/win32/WaveImpl.cpp:1823-1866`, `2389-2463`);
/// once the buffer holds the requested frames, a render performs no decode
/// work on the frame thread.
#[test]
fn render_does_no_decode_when_the_worker_already_buffered() {
    let rate = 8_000u32;
    let frames = 4_000u64;
    let (stream, reads, _seeks) = SignallingStream::ramp(rate, 1, frames);
    let spec = AudioSpec {
        sample_rate: rate,
        channels: 1,
        total_frames: Some(frames),
    };
    let source = Arc::new(SoundSource::streaming(spec, Box::new(stream), None).expect("source"));

    // Start the worker and wait for the whole short stream to be decoded into
    // its window (chunk 2000 + chunk 2000 + the 0-sample end read).
    source.start_decoding();
    assert!(reads.wait_at_least(3), "the worker must decode the stream");
    let before = reads.count();

    // Render the buffered frames: the frame thread only reads the window.
    let mut out = vec![0.0f32; frames as usize];
    mix_source(&source, 0.0, 1.0, 1.0, 0.0, &mut out, out_spec(rate, 1)).unwrap();
    assert_eq!(
        reads.count(),
        before,
        "a render from a full buffer must not decode (WaveImpl.cpp:1448-1487)"
    );
    assert!(out[0].abs() < 1e-12, "the first ramp sample is silence");
    assert_eq!(out[1234], 1234.0);
}

/// Reference: `Play` starts the decode thread
/// (`reference/cpp/core/sound/win32/WaveImpl.cpp:1823-1866`, `2911-2921`) and
/// returns; the decode thread then fills the L2 buffer while the frame thread
/// keeps running. Starting playback must not decode on the caller's thread.
#[test]
fn play_starts_decoding_without_blocking() {
    let rate = 8_000u32;
    let frames = 80_000u64;
    let (stream, reads, _seeks) = SignallingStream::ramp(rate, 1, frames);
    let spec = AudioSpec {
        sample_rate: rate,
        channels: 1,
        total_frames: Some(frames),
    };
    let source = Arc::new(SoundSource::streaming(spec, Box::new(stream), None).expect("source"));
    let mut mixer = Mixer::new();
    let id = mixer.spawn_channel();

    // Nothing is decoded before playback starts.
    assert_eq!(reads.count(), 0, "open must not decode");

    // `play` returns at once and starts the worker.
    mixer.channel(id).unwrap().play(source.clone());
    assert!(
        reads.wait_at_least(1),
        "play must start decoding without waiting for it"
    );
    // The first frame pull reads the window the worker filled.
    assert_eq!(source.frame(0).unwrap().unwrap().0, 0.0);
}

/// The decode-ahead worker must not change the rendered bytes: a streaming
/// source renders exactly like the same samples in memory, across several
/// decode chunks and a backwards reseek.
/// Reference: the decode thread hands the playing thread already-decoded
/// PCM and `Render` produces the same samples in either path
/// (`reference/cpp/core/sound/WaveIntf.h:84-105`, `WaveImpl.cpp:2479-2520`),
/// so a streaming source must render exactly like the same samples in memory.
#[test]
fn decode_ahead_preserves_samples_across_chunks() {
    let rate = 8_000u32;
    let frames = 40_000u64;
    let samples: Vec<f32> = (0..frames).map(|frame| frame as f32).collect();
    let spec = AudioSpec {
        sample_rate: rate,
        channels: 1,
        total_frames: Some(frames),
    };
    let materialized = Arc::new(
        SoundSource::new(Arc::new(DecodedAudio::new(spec, samples))).expect("materialized"),
    );
    let (stream, _reads, _seeks) = SignallingStream::ramp(rate, 1, frames);
    let streaming = Arc::new(SoundSource::streaming(spec, Box::new(stream), None).expect("stream"));

    // A fixed sequence of chunks that crosses several decode boundaries and a
    // backwards jump.
    for position in [0.0f64, 0.25, 0.5, 1.5, 3.0, 0.125] {
        let mut expected = vec![0.0f32; 1024];
        mix_source(
            &materialized,
            position,
            1.0,
            0.75,
            -0.25,
            &mut expected,
            out_spec(rate, 1),
        )
        .unwrap();
        let mut observed = vec![0.0f32; 1024];
        mix_source(
            &streaming,
            position,
            1.0,
            0.75,
            -0.25,
            &mut observed,
            out_spec(rate, 1),
        )
        .unwrap();
        assert_eq!(expected, observed, "streaming differs at {position}s");
    }
}

/// A ramp source stays continuous across ordinary mixer polls: the voice
/// keeps the mixer's own clock, so every rendered sample follows the one
/// before it by the ramp's step.
///
/// A voice that was placed on another clock on every poll would jump by that
/// clock's step instead, which is a cut in the middle of the waveform. The
/// movie audio is never placed that way: the reference appends the decoded
/// frames to a sound buffer that the device plays at its own rate
/// (`reference/cpp/core/movie/ffmpeg/AEFactory.cpp:210-219`).
#[test]
fn a_ramp_source_stays_continuous_across_mixer_polls() {
    /// The output rate, and the ramp's rate.
    const RATE: u32 = 48_000;
    /// The ramp's step per sample; the whole track spans one unit.
    const STEP: f32 = 1.0 / 576_000.0;

    let mut mixer = Mixer::new();
    let id = mixer.spawn_channel();
    mixer
        .channel(id)
        .unwrap()
        .play(ramp_source(RATE, 1, 12.0, STEP));

    // One 60 Hz frame at 48 kHz.
    let mut out = vec![0.0f32; 800];
    // The resampler's kernel reaches a few frames back, so the first samples of
    // a track are interpolated with a truncated kernel. Render one chunk
    // before the measurements start.
    mixer.mix_advancing(&mut out, out_spec(RATE, 1)).unwrap();
    let mut previous: Option<f32> = None;
    for _ in 0..600 {
        mixer.mix_advancing(&mut out, out_spec(RATE, 1)).unwrap();
        for sample in &out {
            if let Some(last) = previous {
                let delta = sample - last;
                assert!(delta > 0.0, "the ramp did not rise: {delta} after {last}");
                assert!(
                    delta < STEP * 2.0,
                    "the ramp jumped: {delta} instead of {STEP}"
                );
            }
            previous = Some(*sample);
        }
    }
}

/// A ramp source: every sample is one step above the one before it.
fn ramp_source(rate: u32, channels: u32, seconds: f64, step: f32) -> Arc<SoundSource> {
    let frames = (rate as f64 * seconds) as usize;
    let mut interleaved =
        Vec::with_capacity(frames * usize::try_from(channels).expect("channels fit"));
    for frame in 0..frames {
        let value = frame as f32 * step;
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
        SoundSource::new(Arc::new(DecodedAudio::new(spec, interleaved))).expect("valid source"),
    )
}

/// The played position is the rendered position minus the output latency.
///
/// The audio device holds a queue of rendered samples, so the sound has
/// reached the listener that much earlier than the mixer has rendered it.
/// The reference compensates the same delay
/// (`reference/cpp/core/movie/ffmpeg/AEFactory.cpp:228`).
#[test]
fn the_played_position_is_the_rendered_position_minus_the_latency() {
    let mut mixer = Mixer::new();
    let id = mixer.spawn_channel();
    mixer.channel(id).unwrap().play(tone(44100, 1, 2.0));
    mixer.set_output_latency(0.15);
    mixer.advance(1.0);

    let channel = mixer.channel_ref(id).unwrap();
    assert!((channel.position_seconds() - 1.0).abs() < 1e-9);
    assert!((channel.played_position_seconds() - 0.85).abs() < 1e-9);
}

/// A voice created after the latency was set inherits it.
#[test]
fn a_new_voice_inherits_the_output_latency() {
    let mut mixer = Mixer::new();
    mixer.set_output_latency(0.2);
    let id = mixer.spawn_channel();
    mixer.channel(id).unwrap().play(tone(44100, 1, 2.0));
    mixer.advance(0.5);

    let channel = mixer.channel_ref(id).unwrap();
    assert!((channel.output_latency() - 0.2).abs() < 1e-9);
    assert!((channel.played_position_seconds() - 0.3).abs() < 1e-9);
}

/// A voice that has not rendered anything yet has reached nothing.
#[test]
fn the_played_position_never_goes_below_zero() {
    let mut mixer = Mixer::new();
    let id = mixer.spawn_channel();
    mixer.channel(id).unwrap().play(tone(44100, 1, 2.0));
    mixer.set_output_latency(1.0);
    mixer.advance(0.5);

    let channel = mixer.channel_ref(id).unwrap();
    assert_eq!(channel.played_position_seconds(), 0.0);
}

/// A sine tone at `frequency` as a decoded source.
fn tone_at(rate: u32, channels: u32, seconds: f64, frequency: f64) -> Arc<SoundSource> {
    let frames = (rate as f64 * seconds) as usize;
    let mut interleaved =
        Vec::with_capacity(frames * usize::try_from(channels).expect("channels fit"));
    for f in 0..frames {
        let value = (2.0 * PI * frequency * f as f64 / rate as f64).sin() as f32 * 0.5;
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
        SoundSource::new(Arc::new(DecodedAudio::new(spec, interleaved))).expect("valid source"),
    )
}

/// The root-mean-square of `samples`.
fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples
        .iter()
        .map(|sample| f64::from(*sample) * f64::from(*sample))
        .sum();
    (sum / samples.len() as f64).sqrt() as f32
}

/// A source at another rate keeps its high end.
///
/// The mixer converts a source to the output rate with a windowed-sinc
/// kernel. A plain interpolation between the neighbouring frames would
/// attenuate a tone near the source's Nyquist frequency: a 15 kHz tone in a
/// 44.1 kHz source would come out a fifth of its level low, and the spectrum
/// above the source's Nyquist frequency would fold back. The kernel keeps the
/// tone at its level.
#[test]
fn a_source_at_another_rate_keeps_its_high_end() {
    let mut mixer = Mixer::new();
    let id = mixer.spawn_channel();
    mixer
        .channel(id)
        .unwrap()
        .play(tone_at(44100, 1, 1.0, 15_000.0));

    // 100 ms at 48 kHz, from a 44.1 kHz source.
    let mut out = vec![0.0f32; 4800];
    mixer.mix(&mut out, out_spec(48000, 1)).unwrap();

    // The source tone's own level: an amplitude of 0.5 has a root-mean-square
    // of 0.5 / sqrt(2).
    let expected = 0.5f32 / 2.0f32.sqrt();
    let measured = rms(&out);
    assert!(
        measured > expected * 0.95,
        "the tone came out at {measured} instead of {expected}"
    );
}

/// The frequency of `samples` measured by its zero crossings, in Hz.
///
/// A tone crosses the mean twice per period: once up, once down.
fn measured_frequency(samples: &[f32], rate: u32) -> f64 {
    if samples.len() < 4 || rate == 0 {
        return 0.0;
    }
    let mean = samples.iter().sum::<f32>() / samples.len() as f32;
    let mut crossings = 0usize;
    let mut previous = samples[0] - mean;
    for sample in &samples[1..] {
        let value = sample - mean;
        // A tone crosses the mean twice per period: once up, once down.
        if (previous < 0.0) != (value < 0.0) {
            crossings += 1;
        }
        previous = value;
    }
    let seconds = samples.len() as f64 / f64::from(rate);
    crossings as f64 / 2.0 / seconds
}

/// A source at another rate is converted without shifting its pitch.
///
/// The mixer converts a source to the output rate by interpolating between
/// the neighbouring source frames. The conversion must keep the pitch: a
/// movie track that plays a fifth too high or too low is a defect no rate
/// conversion may have.
#[test]
fn a_source_at_another_rate_keeps_its_pitch() {
    let mut mixer = Mixer::new();
    let id = mixer.spawn_channel();
    mixer.channel(id).unwrap().play(tone(44100, 1, 1.0));

    // 100 ms at 48 kHz, from a 44.1 kHz source.
    let mut out = vec![0.0f32; 4800];
    mixer.mix(&mut out, out_spec(48000, 1)).unwrap();

    let hz = measured_frequency(&out, 48000);
    assert!(
        (hz - 440.0).abs() < 5.0,
        "the tone measured {hz:.1} Hz instead of 440 Hz"
    );
}

/// The amplitude of `samples` at `frequency`.
///
/// The Goertzel algorithm: the magnitude of one bin of the discrete Fourier
/// transform, normalised to the amplitude of a sine in the same samples.
fn amplitude_at(samples: &[f32], rate: u32, frequency: f64) -> f32 {
    if samples.is_empty() || rate == 0 {
        return 0.0;
    }
    let omega = 2.0 * PI * frequency / f64::from(rate);
    let coefficient = 2.0 * omega.cos();
    let mut previous = 0.0f64;
    let mut before = 0.0f64;
    for sample in samples {
        let current = f64::from(*sample) + coefficient * previous - before;
        before = previous;
        previous = current;
    }
    let power = previous * previous + before * before - coefficient * previous * before;
    let magnitude = power.max(0.0).sqrt();
    (2.0 * magnitude / samples.len() as f64) as f32
}

/// A source at another rate does not fold its top band back.
///
/// The mixer converts a source to the output rate with a windowed-sinc
/// kernel whose passband follows the output rate: the content above the
/// output's Nyquist frequency is rejected instead of being folded back into
/// the band. A 23.5 kHz tone in a 48 kHz source played into a 44.1 kHz
/// output would otherwise come out as a 20.6 kHz tone at its own level.
#[test]
fn a_source_at_another_rate_does_not_fold_its_top_band_back() {
    let mut mixer = Mixer::new();
    let id = mixer.spawn_channel();
    mixer
        .channel(id)
        .unwrap()
        .play(tone_at(48_000, 1, 0.4, 23_500.0));

    // 200 ms at 44.1 kHz, from a 48 kHz source.
    let mut out = vec![0.0f32; 8820];
    mixer.mix(&mut out, out_spec(44_100, 1)).unwrap();

    // The tone would fold to 44.1 - 23.5 = 20.6 kHz.
    let folded = amplitude_at(&out[2205..], 44_100, 20_600.0);
    assert!(
        folded < 0.02,
        "the folded tone measured {folded}, the band above the output's Nyquist \
         frequency must be rejected"
    );
}
