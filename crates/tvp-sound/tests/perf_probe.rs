//! Ignored perf probes for the audio hot paths (not part of the normal test
//! run). Measure with:
//!
//! ```text
//! cargo test -p tvp-sound --test perf_probe -- --ignored --nocapture
//! ```
//!
//! These compare the old per-sample `AudioTrack::frame` access (two ring
//! lock/unlocks per output sample for a streaming source) against the batched
//! `AudioTrack::read_frames` the mixer now uses (one lock per chunk).

use std::time::{Duration, Instant};

use tvp_sound::{AudioTrack, Mixer, open_track_bytes};

/// A minimal PCM16 WAV of silence, `seconds` long. Long enough to exceed the
/// whole-file cap (`FULL_BUFFER_MAX_BYTES`) and therefore stream.
fn wav_pcm16(sample_rate: u32, channels: u16, seconds: u32) -> Vec<u8> {
    let frames = sample_rate * seconds;
    let data_len = frames * u32::from(channels) * 2;
    let mut buf = Vec::with_capacity(44 + data_len as usize);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + data_len).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&channels.to_le_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&(sample_rate * u32::from(channels) * 2).to_le_bytes());
    buf.extend_from_slice(&(channels * 2).to_le_bytes());
    buf.extend_from_slice(&16u16.to_le_bytes());
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_len.to_le_bytes());
    buf.resize(44 + data_len as usize, 0);
    buf
}

fn wait_ready(track: &AudioTrack, frames: u64) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while track.buffered_frames() < frames && !track.is_ready() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
#[ignore = "perf probe; run explicitly with --ignored --nocapture"]
fn perf_frame_access_streaming() {
    const FRAMES: usize = 512;
    const ITERS: usize = 20_000;

    let track = open_track_bytes(wav_pcm16(48_000, 1, 130), "bench_stream.wav").expect("open");
    assert!(track.is_streaming(), "expected the streaming path");
    wait_ready(&track, 48_000);

    let mut acc = 0.0f32;
    let t = Instant::now();
    for _ in 0..ITERS {
        for i in 0..FRAMES {
            let i0 = i as u64;
            if let Some((a, b)) = track.frame(i0) {
                acc += a + b;
            }
            if let Some((a, b)) = track.frame(i0 + 1) {
                acc += a + b;
            }
        }
    }
    let per_sample = t.elapsed();

    let mut buf = vec![None; FRAMES + 2];
    let t = Instant::now();
    for _ in 0..ITERS {
        track.read_frames(0, &mut buf);
        for (a, b) in buf.iter().flatten() {
            acc += *a + *b;
        }
    }
    let batched = t.elapsed();

    println!(
        "streaming frame access: per-sample={per_sample:?} batched={batched:?} speedup={:.2}x",
        per_sample.as_secs_f64() / batched.as_secs_f64()
    );
    std::hint::black_box(acc);
}

#[test]
#[ignore = "perf probe; run explicitly with --ignored --nocapture"]
fn perf_frame_access_whole_file() {
    const FRAMES: usize = 512;
    const ITERS: usize = 50_000;

    let track = open_track_bytes(wav_pcm16(48_000, 1, 1), "bench_full.wav").expect("open");
    assert!(!track.is_streaming(), "expected the whole-file path");
    wait_ready(&track, 0);

    let mut acc = 0.0f32;
    let t = Instant::now();
    for _ in 0..ITERS {
        for i in 0..FRAMES {
            let i0 = i as u64;
            if let Some((a, b)) = track.frame(i0) {
                acc += a + b;
            }
            if let Some((a, b)) = track.frame(i0 + 1) {
                acc += a + b;
            }
        }
    }
    let per_sample = t.elapsed();

    let mut buf = vec![None; FRAMES + 2];
    let t = Instant::now();
    for _ in 0..ITERS {
        track.read_frames(0, &mut buf);
        for (a, b) in buf.iter().flatten() {
            acc += *a + *b;
        }
    }
    let batched = t.elapsed();

    println!(
        "whole-file frame access: per-sample={per_sample:?} batched={batched:?} speedup={:.2}x",
        per_sample.as_secs_f64() / batched.as_secs_f64()
    );
    std::hint::black_box(acc);
}

#[test]
#[ignore = "perf probe; run explicitly with --ignored --nocapture"]
fn perf_compressed_copy_cost() {
    // One full copy of a streamed entry's compressed bytes is what the old
    // path paid in `probe_audio` and again in every `StreamDecoder::open`
    // (initial open plus each seek/loop wrap). The Arc path pays it once.
    let bytes = wav_pcm16(48_000, 1, 130);
    const ITERS: u32 = 200;
    let t = Instant::now();
    for _ in 0..ITERS {
        std::hint::black_box(bytes.clone());
    }
    let elapsed = t.elapsed();
    let total_mb = (bytes.len() as f64 * f64::from(ITERS)) / (1024.0 * 1024.0);
    println!(
        "copy {} KiB compressed: {ITERS} clones in {elapsed:?} ({:.1} ms/clone, {:.1} GiB/s)",
        bytes.len() / 1024,
        elapsed.as_secs_f64() * 1000.0 / f64::from(ITERS),
        total_mb / 1024.0 / elapsed.as_secs_f64()
    );
}

#[test]
#[ignore = "perf probe; run explicitly with --ignored --nocapture"]
fn perf_render_mix_advancing() {
    const ITERS: usize = 20_000;
    let track = open_track_bytes(wav_pcm16(48_000, 1, 130), "bench_adv.wav").expect("open");
    assert!(track.is_streaming());
    wait_ready(&track, 48_000);

    let mut mixer = Mixer::new();
    let id = mixer.spawn_channel();
    mixer.channel(id).unwrap().play_track(track);
    let mut out = vec![0.0f32; 512 * 2];

    let t = Instant::now();
    for _ in 0..ITERS {
        mixer.render_mix_advancing(&mut out, 48_000, 2);
    }
    let elapsed = t.elapsed();
    println!("render_mix_advancing 512 frames x2 ch: {elapsed:?}");
    std::hint::black_box(out[0]);
}
