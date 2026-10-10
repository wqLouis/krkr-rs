//! Integration tests for the L2 sound model and software mixer
//! (`engine::sound`).
//!
//! Headless by construction: the mixer never opens a device, so every test
//! here drives playback with `Mixer::advance`/`advance_to` and renders into
//! a caller-owned buffer. The reference behaviour is `main`'s
//! `crates/tvp-sound/src/{mixer,waveflags}.rs`.

use std::f64::consts::PI;
use std::sync::Arc;

use api::media::{AudioSpec, DecodedAudio};
use engine::sound::{
    LOOP_FLAG_COUNT, LoopCondition, LoopFlags, LoopInfo, LoopLink, MAX_LOOP_FLAG_VALUE, MAX_VOLUME,
    Mixer, SoundBuffer, SoundError, SoundLabel, SoundSource, Status, mix_source,
};

fn out_spec(sample_rate: u32, channels: u16) -> AudioSpec {
    AudioSpec {
        sample_rate,
        channels,
        total_frames: None,
    }
}

/// A 440 Hz sine tone as a decoded source, optionally with loop info.
fn tone_with_loops(
    rate: u32,
    channels: u16,
    seconds: f64,
    info: Option<LoopInfo>,
) -> Arc<SoundSource> {
    let frames = (rate as f64 * seconds) as usize;
    let mut interleaved = Vec::with_capacity(frames * usize::from(channels));
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

fn tone(rate: u32, channels: u16, seconds: f64) -> Arc<SoundSource> {
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
