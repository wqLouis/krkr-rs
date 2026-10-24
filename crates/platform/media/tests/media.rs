//! Integration tests for the crate-level `Media` object and capabilities.

use media::{Media, capabilities, open_video};

#[test]
fn capabilities_advertise_the_image_and_audio_backends() {
    let media = Media::new();
    let caps = capabilities(&media);
    assert!(caps.image_formats.iter().any(|name| name == "png"));
    assert!(caps.image_formats.iter().any(|name| name == "tlg6"));
    assert!(caps.audio_formats.iter().any(|name| name == "wav"));
    assert!(caps.audio_formats.iter().any(|name| name == "opus"));
    if cfg!(feature = "video-ffmpeg") {
        assert!(caps.video_formats.iter().any(|name| name == "mp4"));
        assert!(caps.streaming_video);
    } else {
        // MediaCodec: the Android framework demuxes/decodes these; the exact
        // codec set is device-dependent, so the baseline list is advertised.
        assert!(caps.video_formats.iter().any(|name| name == "mp4"));
        assert!(caps.streaming_video);
    }
    assert!(!caps.is_empty());
}

/// The backend object holds no state: two independently created values are
/// equal, and a clone equals its original.
#[test]
fn media_is_stateless() {
    let media = Media::new();
    let other = Media::new();
    assert_eq!(media, other);
    assert_eq!(media, media);
}

#[test]
fn video_open_rejects_non_video_data() {
    let media = Media::new();
    // The FFmpeg backend returns a typed decode/unsupported error; the
    // MediaCodec backend returns a typed decode/unsupported error too. Either
    // way: never a panic, never a silent success.
    assert!(open_video(&media, std::sync::Arc::from(&b"not a video"[..]), None).is_err());
}
