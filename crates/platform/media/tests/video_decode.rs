//! Integration tests for the video decode path over the committed fixtures.
//!
//! The fixtures are short clips whose every picture differs, so a decoder
//! that keeps returning one picture shows up as a frozen clip. Each fixture
//! is decoded through this crate's own backend, and the decoded picture
//! count, the per-picture timestamps, the picture size and the pixel format
//! are checked.
//!
//! The clips are committed under `tests/fixtures`, so the tests read them
//! from there and never run a command line tool: the CI installs the FFmpeg
//! libraries but not the `ffmpeg` command line. The command lines that made
//! the clips are in the fixtures' README.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use api::media::VideoSpec;
use media::{Media, VideoDecoderState, open_video, present_video};

/// The directory the committed fixtures live in.
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// The picture width and height of every fixture.
const SIZE: u32 = 64;

/// The frames per second of every fixture but the raw one.
const FPS: f32 = 10.0;

/// The number of pictures every fixture holds.
const PICTURES: usize = 20;

/// The picture interval of every fixture, in milliseconds.
const INTERVAL_MS: u64 = 100;

/// The bytes one RGBA8 picture of `SIZE` x `SIZE` holds.
const PICTURE_BYTES: usize = (SIZE as usize) * (SIZE as usize) * 4;

/// The path of the fixture called `name`.
fn fixture(name: &str) -> PathBuf {
    Path::new(FIXTURES).join(name)
}

/// Whether this build decodes video with the FFmpeg backend.
///
/// The MediaCodec backend needs an Android device, so there is nothing to
/// exercise on a desktop host.
fn decodes_with_ffmpeg() -> bool {
    cfg!(feature = "video-ffmpeg")
}

/// One decoded picture: its timestamp and its pixels.
struct Picture {
    /// The picture's presentation timestamp, in milliseconds.
    pts_ms: u64,
    /// The picture's RGBA8 pixels.
    rgba: Vec<u8>,
}

/// Opens the fixture called `name`.
fn open_fixture(name: &str) -> (Media, VideoSpec, VideoDecoderState) {
    let path = fixture(name);
    let bytes =
        std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let media = Media::new();
    let (spec, state) = open_video(&media, Arc::from(bytes.into_boxed_slice()), Some(name))
        .unwrap_or_else(|error| panic!("open {}: {error}", path.display()));
    (media, spec, state)
}

/// Decodes the fixture called `name` and returns its spec and its pictures.
///
/// The clock walks over the clip's whole duration in half-picture steps, and
/// the pictures that differ from the one before are kept: the clip's
/// pictures all differ, so a decoder that returned one picture forever
/// shows up as a single picture.
fn decode(name: &str) -> (VideoSpec, Vec<Picture>) {
    let (media, spec, mut state) = open_fixture(name);
    let mut pictures: Vec<Picture> = Vec::new();
    let mut at_ms = 0u64;
    while at_ms <= PICTURES as u64 * INTERVAL_MS {
        let frame = present_video(&media, &mut state, at_ms)
            .unwrap_or_else(|error| panic!("present {name} at {at_ms} ms: {error}"));
        let is_new = pictures
            .last()
            .is_none_or(|last: &Picture| last.rgba != frame.rgba);
        if is_new {
            pictures.push(Picture {
                pts_ms: frame.pts_ms,
                rgba: frame.rgba.clone(),
            });
        }
        at_ms += INTERVAL_MS / 2;
    }
    (spec, pictures)
}

/// Asserts that a picture is a tightly packed RGBA8 picture of the
/// fixture's size.
///
/// The backend hands out RGBA8 whatever the container's own format is
/// (`crates/platform/media/src/video/mod.rs`): the picture's four bytes per
/// pixel are the check that the conversion ran.
fn assert_rgba8(rgba: &[u8]) {
    assert_eq!(
        rgba.len(),
        PICTURE_BYTES,
        "the picture is not tightly packed RGBA8"
    );
}

/// Asserts that the pictures are the clip's whole set: one per picture
/// interval, each with its own timestamp.
fn assert_the_whole_set(pictures: &[Picture]) {
    assert_eq!(
        pictures.len(),
        PICTURES,
        "the fixture decoded {} pictures instead of {PICTURES}",
        pictures.len()
    );
    for (index, picture) in pictures.iter().enumerate() {
        assert_eq!(
            picture.pts_ms,
            index as u64 * INTERVAL_MS,
            "picture {index} has the wrong timestamp"
        );
        assert_rgba8(&picture.rgba);
    }
}

/// The picture at a position is the one whose timestamp the clock reached.
///
/// The reference presents the picture whose timestamp the clock has
/// reached, that is the latest picture at or before the requested time
/// (`reference/cpp/core/movie/ffmpeg/KRMovieLayer.cpp:48-56`). A clock at
/// sixteen milliseconds has reached the picture stamped zero and not the
/// one stamped a hundred.
#[test]
fn the_picture_at_a_position_is_the_one_the_clock_reached() {
    if !decodes_with_ffmpeg() {
        return;
    }
    let (media, _spec, mut state) = open_fixture("clip.mp4");

    let frame = present_video(&media, &mut state, 16).expect("present at 16 ms");
    assert_eq!(
        frame.pts_ms, 0,
        "the picture is ahead of the clock that asked for it"
    );
    assert_rgba8(&frame.rgba);

    let frame = present_video(&media, &mut state, 100).expect("present at 100 ms");
    assert_eq!(
        frame.pts_ms, INTERVAL_MS,
        "the clock has reached the picture stamped 100 ms"
    );
    assert_rgba8(&frame.rgba);
}

/// The MP4 fixture decodes every picture, each with its own timestamp.
///
/// The reference presents a picture when the clock has reached the
/// picture's own timestamp
/// (`reference/cpp/core/movie/ffmpeg/KRMovieLayer.cpp:48-56`), so a stream
/// whose timestamps do not advance cannot show a new picture.
#[test]
fn the_mp4_fixture_decodes_every_picture() {
    if !decodes_with_ffmpeg() {
        return;
    }
    let (spec, pictures) = decode("clip.mp4");
    assert_eq!(spec.width, SIZE);
    assert_eq!(spec.height, SIZE);
    assert_eq!(spec.fps, FPS);
    assert_eq!(spec.total_frames, Some(PICTURES as u64));
    assert_the_whole_set(&pictures);
}

/// The GIF fixture decodes every picture, each with its own timestamp.
///
/// The reference tree has no GIF path at all: no GIF demuxer is registered
/// for the movie player and no GIF decoder exists in the tree, so a GIF is
/// not a stream the reference can play.
#[test]
fn the_gif_fixture_decodes_every_picture() {
    if !decodes_with_ffmpeg() {
        return;
    }
    let (spec, pictures) = decode("clip.gif");
    assert_eq!(spec.width, SIZE);
    assert_eq!(spec.height, SIZE);
    assert_eq!(spec.fps, FPS);
    assert_the_whole_set(&pictures);
}

/// The WebM fixture decodes every picture, each with its own timestamp.
///
/// The container is not the variable: the same clip in a second container
/// decodes the same pictures with the same timestamps.
#[test]
fn the_webm_fixture_decodes_every_picture() {
    if !decodes_with_ffmpeg() {
        return;
    }
    let (spec, pictures) = decode("clip.webm");
    assert_eq!(spec.width, SIZE);
    assert_eq!(spec.height, SIZE);
    assert_eq!(spec.fps, FPS);
    assert_the_whole_set(&pictures);
}

/// The raw elementary stream is read from its start.
///
/// A raw elementary stream has no index to seek in, so the demuxer refuses
/// the seek. The reference reads such a stream from its start and seeks
/// only on a transport command
/// (`reference/cpp/core/movie/ffmpeg/DemuxFFmpeg.cpp:988-1044`), so the
/// stream is read forward instead of being refused at the first picture.
#[test]
fn the_raw_elementary_stream_is_read_from_its_start() {
    if !decodes_with_ffmpeg() {
        return;
    }
    let (media, spec, mut state) = open_fixture("clip.h264");
    assert_eq!(spec.width, SIZE);
    assert_eq!(spec.height, SIZE);

    let frame = present_video(&media, &mut state, 0).expect("present at 0 ms");
    assert_eq!(frame.pts_ms, 0, "the first picture is stamped 0 ms");
    assert_rgba8(&frame.rgba);

    let (_spec, pictures) = decode("clip.h264");
    assert_eq!(
        pictures.len(),
        PICTURES,
        "the raw stream decoded {} pictures instead of {PICTURES}",
        pictures.len()
    );
    for (index, picture) in pictures.iter().enumerate() {
        assert_eq!(
            picture.pts_ms,
            index as u64 * INTERVAL_MS,
            "picture {index} has the wrong timestamp"
        );
        assert_rgba8(&picture.rgba);
    }
}

/// The clip with an audio track still decodes every picture.
///
/// The reference anchors the clock on the audio's played position when the
/// file has an audio track
/// (`reference/cpp/core/movie/ffmpeg/VideoPlayer.cpp:2044`), so the audio
/// track and the picture timestamps have to be read together.
#[test]
fn the_clip_with_an_audio_track_decodes_every_picture() {
    if !decodes_with_ffmpeg() {
        return;
    }
    let (spec, pictures) = decode("clip_av.mp4");
    assert_eq!(spec.width, SIZE);
    assert_eq!(spec.height, SIZE);
    assert_eq!(spec.fps, FPS);
    assert!(
        spec.has_audio(),
        "the clip must carry an audio track: {spec:?}"
    );
    assert_the_whole_set(&pictures);
}
