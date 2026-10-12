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
    assert_eq!(out[0], 0.0);
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
