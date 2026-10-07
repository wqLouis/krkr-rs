//! Integration tests for the stateless audio decode API.

mod common;

use std::sync::Arc;

use api::error::MediaError;
use common::tiny_wav_mono;
use media::{Media, decode_audio, open_audio, probe_audio, read_audio, seek_audio};

#[test]
fn probe_and_decode_a_wav_header_and_frames() {
    let media = Media::new();
    let samples: Vec<i16> = (0..1000).map(|i| (i % 64) as i16).collect();
    let wav = tiny_wav_mono(&samples, 8000);

    let spec = probe_audio(&media, &wav, Some("test.wav")).expect("probe wav");
    assert_eq!(spec.sample_rate, 8000);
    assert_eq!(spec.channels, 1);

    let decoded = decode_audio(&media, &wav, Some("test.wav")).expect("decode wav");
    assert_eq!(decoded.spec.sample_rate, 8000);
    assert_eq!(decoded.spec.channels, 1);
    assert_eq!(decoded.frame_count(), samples.len() as u64);
    assert_eq!(decoded.interleaved.len(), samples.len());
}

#[test]
fn streaming_read_matches_whole_file() {
    let media = Media::new();
    let samples: Vec<i16> = (0..500).map(|i| ((i % 32) - 16) as i16 * 100).collect();
    let wav = tiny_wav_mono(&samples, 11_025);

    let whole = decode_audio(&media, &wav, None).expect("whole decode");
    let (spec, mut state) =
        open_audio(&media, Arc::from(wav.as_slice()), None).expect("open stream");
    assert_eq!(spec.sample_rate, 11_025);

    let mut all = Vec::new();
    let mut chunk = vec![0.0f32; 64];
    loop {
        match read_audio(&media, &mut state, &mut chunk) {
            Ok(0) => break,
            Ok(written) => all.extend_from_slice(&chunk[..written]),
            Err(MediaError::EndOfStream) => break,
            Err(error) => panic!("stream read failed: {error}"),
        }
    }
    assert_eq!(all.len(), whole.interleaved.len());
}

#[test]
fn seek_reopens_and_lands_on_the_requested_frame() {
    let media = Media::new();
    let samples: Vec<i16> = (0..500).map(|i| i as i16).collect();
    let wav = tiny_wav_mono(&samples, 8000);
    let (_spec, mut state) =
        open_audio(&media, Arc::from(wav.as_slice()), Some("a.wav")).expect("open stream");
    let mut chunk = vec![0.0f32; 8];
    let first = read_audio(&media, &mut state, &mut chunk).expect("first read");
    assert!(first > 0);

    seek_audio(&media, &mut state, 100).expect("seek");
    let mut all = Vec::new();
    loop {
        match read_audio(&media, &mut state, &mut chunk) {
            Ok(0) => break,
            Ok(written) => all.extend_from_slice(&chunk[..written]),
            Err(MediaError::EndOfStream) => break,
            Err(error) => panic!("read after seek failed: {error}"),
        }
    }
    assert_eq!(all.len(), samples.len() - 100);
}

#[test]
fn undecodable_opus_returns_an_error() {
    let media = Media::new();
    // An Ogg Opus signature with no valid stream: probing/decoding must fail
    // with a typed error, never substitute silence.
    let fake_opus = b"OggS\x00\x00\x00\x00OpusHead\x00\x00\x00\x00";
    assert!(probe_audio(&media, fake_opus, Some("voice.opus")).is_err());
    assert!(decode_audio(&media, fake_opus, Some("voice.opus")).is_err());
    assert!(open_audio(&media, Arc::from(fake_opus.as_slice()), Some("voice.opus")).is_err());
}

#[test]
fn hints_accept_names_and_bare_extensions() {
    let media = Media::new();
    let samples: Vec<i16> = (0..64).map(|i| i as i16).collect();
    let wav = tiny_wav_mono(&samples, 8000);
    for hint in ["dir/a.wav", "wav", ".wav"] {
        let spec = probe_audio(&media, &wav, Some(hint))
            .unwrap_or_else(|error| panic!("hint {hint:?} should probe: {error}"));
        assert_eq!(spec.sample_rate, 8000, "hint {hint:?}");
    }
}

#[test]
fn empty_or_absent_hint_is_named_audio_in_errors() {
    let media = Media::new();
    for hint in [Some(""), None] {
        let error = decode_audio(&media, b"not audio", hint).unwrap_err();
        assert!(
            format!("{error}").contains("<audio>"),
            "hint {hint:?} produced unexpected error: {error}"
        );
    }
}

#[test]
fn garbage_is_a_decode_error_not_a_panic() {
    let media = Media::new();
    assert!(matches!(
        probe_audio(&media, b"not audio", Some("x.bin")),
        Err(MediaError::Decode { .. })
    ));
    assert!(matches!(
        decode_audio(&media, b"not audio", None),
        Err(MediaError::Decode { .. })
    ));
}
