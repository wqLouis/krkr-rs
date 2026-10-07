//! Integration tests for the stateless video decode API.
//!
//! A tiny clip is generated with the `ffmpeg` CLI (MPEG-4 part 2 video + AAC
//! audio in MP4) into a temporary directory. The tests assert the decoded
//! dimensions, that the frame has non-empty pixels, and that the audio track
//! decodes. When no usable FFmpeg encoder is available the tests skip
//! gracefully instead of failing.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use api::media::VideoSpec;
use media::{Media, VideoDecoderState, open_video, present_video, video_audio};

/// Nominal clip geometry: small enough to decode in milliseconds.
const WIDTH: u32 = 64;
const HEIGHT: u32 = 48;
/// Bytes in one tightly packed RGBA8 frame.
const FRAME_BYTES: usize = (WIDTH as usize) * (HEIGHT as usize) * 4;

/// A generated clip plus its temp directory, removed on drop.
struct TempClip {
    dir: PathBuf,
    path: PathBuf,
}

impl Drop for TempClip {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Returns `true` when the `ffmpeg` binary runs.
fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Generates the test clip, or `None` when no usable encoder is available.
fn generate_clip() -> Option<TempClip> {
    // The MediaCodec build has no video decoder yet, so there is nothing to
    // exercise; skip rather than fail.
    if !cfg!(feature = "video-ffmpeg") || !ffmpeg_available() {
        return None;
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("krkr-media-video-{}-{}", std::process::id(), nanos));
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("clip.mp4");

    // Prefer the always-built-in MPEG-4 part 2 encoder; fall back to x264 so
    // the test still works on a build that only ships libx264.
    for video_codec in ["mpeg4", "libx264"] {
        let status = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
            ])
            .arg(format!("testsrc=size={WIDTH}x{HEIGHT}:rate=10:duration=3"))
            .args([
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=44100:duration=3",
                "-c:v",
                video_codec,
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
            ])
            .arg(&path)
            .status()
            .ok()?;
        let non_empty = path.metadata().map(|meta| meta.len() > 0).unwrap_or(false);
        if status.success() && non_empty {
            return Some(TempClip { dir, path });
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
    None
}

/// Opens the generated clip, or skips the test when none can be produced.
fn open_clip() -> Option<(Media, VideoSpec, VideoDecoderState)> {
    let clip = generate_clip()?;
    let bytes = std::fs::read(&clip.path).expect("read generated clip");
    let media = Media::new();
    let (spec, state) = open_video(
        &media,
        Arc::from(bytes.into_boxed_slice()),
        Some("clip.mp4"),
    )
    .expect("open generated clip");
    assert_eq!(spec.width, WIDTH);
    assert_eq!(spec.height, HEIGHT);
    // `clip` drops here, removing the temp directory; `state` keeps the bytes
    // alive through its own `Arc`.
    Some((media, spec, state))
}

#[test]
fn open_reports_video_dimensions_and_an_audio_track() {
    let Some((_media, spec, _state)) = open_clip() else {
        eprintln!("skipping video test: no usable ffmpeg encoder");
        return;
    };
    assert!(spec.width == WIDTH && spec.height == HEIGHT);
    assert!(spec.has_audio(), "expected an audio track: {spec:?}");
    assert!(spec.sample_rate > 0);
    assert!(spec.channels > 0);
}

#[test]
fn present_decodes_tightly_packed_rgba_with_non_empty_pixels() {
    let Some((media, _spec, mut state)) = open_clip() else {
        eprintln!("skipping video test: no usable ffmpeg encoder");
        return;
    };
    let frame = present_video(&media, &mut state, 0).expect("present frame at 0ms");
    assert_eq!(frame.width, WIDTH);
    assert_eq!(frame.height, HEIGHT);
    assert_eq!(frame.rgba.len(), FRAME_BYTES);
    assert!(frame.is_consistent(), "frame bytes must be width*height*4");
    assert!(
        frame.rgba.iter().any(|byte| *byte != 0),
        "decoded frame must not be entirely zero"
    );
}

#[test]
fn present_seeks_backwards_past_the_tolerance_and_decodes_forward() {
    let Some((media, _spec, mut state)) = open_clip() else {
        eprintln!("skipping video test: no usable ffmpeg encoder");
        return;
    };

    // Play forward, then request an earlier time: more than the tolerance
    // behind the current frame, so the backend must seek rather than reuse it.
    let later = present_video(&media, &mut state, 2_000).expect("present at 2s");
    assert!(
        later.pts_ms <= 2_500,
        "unexpected forward frame {}",
        later.pts_ms
    );

    let rewound = present_video(&media, &mut state, 0).expect("present back at 0s");
    assert!(
        rewound.pts_ms <= 200,
        "backward seek landed at {}ms instead of near 0",
        rewound.pts_ms
    );
    assert_eq!(rewound.rgba.len(), FRAME_BYTES);

    // A forward request still decodes forward from the current position.
    let forward = present_video(&media, &mut state, 2_500).expect("present at 2.5s");
    assert!(
        forward.pts_ms >= 2_000,
        "forward decode regressed to {}ms",
        forward.pts_ms
    );
}

#[test]
fn video_audio_returns_interleaved_pcm() {
    let Some((media, _spec, state)) = open_clip() else {
        eprintln!("skipping video test: no usable ffmpeg encoder");
        return;
    };
    let audio = video_audio(&media, &state).expect("decode audio track");
    assert!(audio.spec.sample_rate > 0);
    assert!(audio.spec.channels > 0);
    assert!(audio.frame_count() > 0, "audio track decoded no frames");
    assert_eq!(
        audio.interleaved.len() as u64,
        audio.frame_count() * u64::from(audio.spec.channels)
    );
}
