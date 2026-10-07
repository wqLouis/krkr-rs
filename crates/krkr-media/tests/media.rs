//! Integration tests for the crate-level `Media` object and capabilities.

use krkr_api::error::MediaError;
use krkr_media::{Media, MediaConfig, capabilities, create_media, open_video};

#[test]
fn capabilities_advertise_the_image_and_audio_backends() {
    let media = Media::new();
    let caps = capabilities(&media);
    assert!(caps.image_formats.iter().any(|name| name == "png"));
    assert!(caps.image_formats.iter().any(|name| name == "tlg6"));
    assert!(caps.audio_formats.iter().any(|name| name == "wav"));
    assert!(caps.audio_formats.iter().any(|name| name == "opus"));
    assert!(caps.video_formats.is_empty());
    assert!(caps.streaming_audio);
    assert!(!caps.streaming_video);
    assert!(!caps.is_empty());
}

#[test]
fn config_is_preserved_and_media_is_stateless() {
    let config = MediaConfig {
        allow_large_images: true,
    };
    let media = Media::with_config(config.clone());
    assert_eq!(media.config(), &config);
    // Two independent objects do not alias.
    let other = create_media(MediaConfig::default());
    assert_ne!(media, other);
    assert_eq!(media, media.clone());
}

#[test]
fn video_scaffold_reports_unsupported() {
    let media = Media::new();
    assert!(matches!(
        open_video(&media, std::sync::Arc::from(&[][..]), None),
        Err(MediaError::Unsupported { .. })
    ));
}
