//! Integration tests for [`krkr_api::media`].

use krkr_api::media::*;

#[test]
fn decoded_image_consistency() {
    let image = DecodedImage::new(2, 3, vec![0; 24]);
    assert_eq!(image.pixel_count(), 6);
    assert!(image.is_consistent());
    assert!(!DecodedImage::new(2, 3, vec![0; 23]).is_consistent());
}

#[test]
fn decoded_audio_frame_count() {
    let audio = DecodedAudio::new(
        AudioSpec {
            sample_rate: 48_000,
            channels: 2,
            total_frames: Some(3),
        },
        vec![0.0; 6],
    );
    assert_eq!(audio.frame_count(), 3);
    assert_eq!(audio.spec.duration_ms(), Some(0));
}

#[test]
fn decoded_audio_zero_channels_is_safe() {
    let audio = DecodedAudio::new(AudioSpec::default(), vec![0.0; 6]);
    assert_eq!(audio.frame_count(), 0);
    assert_eq!(audio.spec.duration_ms(), None);
}

#[test]
fn rgba_frame_consistency() {
    let frame = RgbaFrame::new(4, 4, 33, vec![0; 64]);
    assert_eq!(frame.pixel_count(), 16);
    assert!(frame.is_consistent());
}

#[test]
fn video_spec_audio_detection() {
    assert!(!VideoSpec::default().has_audio());
    assert!(
        VideoSpec {
            sample_rate: 44_100,
            channels: 2,
            ..VideoSpec::default()
        }
        .has_audio()
    );
}

#[test]
fn media_capabilities_default_is_empty() {
    assert!(MediaCapabilities::none().is_empty());
    assert!(
        !MediaCapabilities {
            image_formats: vec!["png".to_owned()],
            ..MediaCapabilities::default()
        }
        .is_empty()
    );
}
