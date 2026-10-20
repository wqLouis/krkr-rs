//! Integration tests for the stateless audio decode API.

use std::sync::Arc;

use api::error::MediaError;
use media::{Media, decode_audio, open_audio, probe_audio, read_audio, seek_audio};
use testkit::tiny_wav_mono;

#[cfg(feature = "video-ffmpeg")]
use media::decode_audio_ffmpeg;

/// The committed OGG Vorbis fixture: an `ffmpeg -c:a libvorbis` encode of a
/// 440 Hz sine tone, 0.2 s long, 44.1 kHz mono
/// (`crates/platform/media/tests/assets/sine_440hz_0.2s.ogg`, ~4 KB).
const OGG_SINE: &[u8] = include_bytes!("assets/sine_440hz_0.2s.ogg");

/// The committed MP4/AAC fixture: an `ffmpeg -c:a aac` encode of a 440 Hz
/// sine tone, 0.5 s long, 44.1 kHz mono
/// (`crates/platform/media/tests/assets/sine_440hz_0.5s.m4a`, ~5 KB).
const MP4_AAC_SINE: &[u8] = include_bytes!("assets/sine_440hz_0.5s.m4a");

/// Frames a 0.2 s 44.1 kHz stream should decode to; vorbis encoder delay and
/// the final granule may shift the count by a few dozen samples either way.
const OGG_FRAMES: std::ops::RangeInclusive<u64> = 8_600..=9_100;

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
fn ogg_vorbis_fixture_probes_with_a_sensible_spec() {
    let media = Media::new();
    // The `.ogg` storage-name hint must resolve (AUDIO_FORMATS lists `ogg`),
    // and so must the bare extension and no hint at all — the bytes always
    // decide the format.
    for hint in [Some("bgm/tone.ogg"), Some("ogg"), Some(".ogg"), None] {
        let spec = probe_audio(&media, OGG_SINE, hint)
            .unwrap_or_else(|error| panic!("probe with hint {hint:?} failed: {error}"));
        assert_eq!(spec.sample_rate, 44_100, "hint {hint:?}");
        assert_eq!(spec.channels, 1, "hint {hint:?}");
    }
}

#[test]
fn ogg_vorbis_fixture_decodes_to_audible_samples() {
    let media = Media::new();
    let decoded = decode_audio(&media, OGG_SINE, Some("tone.ogg")).expect("decode ogg");
    assert_eq!(decoded.spec.sample_rate, 44_100);
    assert_eq!(decoded.spec.channels, 1);
    let frames = decoded.frame_count();
    assert!(
        OGG_FRAMES.contains(&frames),
        "0.2 s at 44.1 kHz is ~8820 frames, got {frames}"
    );
    // A 440 Hz sine must actually be audible — no substituted silence.
    let peak = decoded
        .interleaved
        .iter()
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    assert!(peak > 0.1, "expected audible samples, peak was {peak}");
    assert!(decoded.interleaved.iter().all(|sample| sample.is_finite()));
}

#[test]
fn ogg_streaming_read_matches_whole_file() {
    let media = Media::new();
    let whole = decode_audio(&media, OGG_SINE, Some("tone.ogg")).expect("whole decode");
    let (_spec, mut state) =
        open_audio(&media, Arc::from(OGG_SINE), Some("tone.ogg")).expect("open stream");

    let mut all = Vec::new();
    let mut chunk = vec![0.0f32; 256];
    loop {
        match read_audio(&media, &mut state, &mut chunk) {
            Ok(0) => break,
            Ok(written) => all.extend_from_slice(&chunk[..written]),
            Err(MediaError::EndOfStream) => break,
            Err(error) => panic!("stream read failed: {error}"),
        }
    }
    assert!(!all.is_empty());
    assert_eq!(all.len(), whole.interleaved.len());
}

#[test]
fn truncated_ogg_returns_a_typed_error_not_silence() {
    let media = Media::new();
    // A real Ogg capture pattern followed by nothing: probing must fail with
    // a typed error rather than decode to an empty/zero-filled buffer.
    let truncated = b"OggS\x00\x02vorbis\x01\x00\x00\x00\x00";
    assert!(matches!(
        probe_audio(&media, truncated, Some("broken.ogg")),
        Err(MediaError::Decode { .. } | MediaError::Unsupported { .. })
    ));
    assert!(decode_audio(&media, truncated, Some("broken.ogg")).is_err());
    assert!(open_audio(&media, Arc::from(truncated.as_slice()), Some("broken.ogg")).is_err());
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

#[test]
fn mp4_aac_fixture_probes_even_when_the_name_hint_is_wrong() {
    let media = Media::new();
    // A storage entry whose name says `.ogg` but whose content is an MP4
    // must still be probed: the bytes decide the format, and a hint that
    // sends the probe down the wrong path is retried without it.
    for hint in [
        Some("voice/len419_020.ogg"),
        Some("ogg"),
        Some(".ogg"),
        None,
    ] {
        let spec = probe_audio(&media, MP4_AAC_SINE, hint)
            .unwrap_or_else(|error| panic!("probe with hint {hint:?} failed: {error}"));
        assert_eq!(spec.sample_rate, 44_100, "hint {hint:?}");
        assert_eq!(spec.channels, 1, "hint {hint:?}");
    }
}

#[test]
fn mp4_aac_fixture_decodes_to_audible_samples() {
    let media = Media::new();
    let decoded = decode_audio(&media, MP4_AAC_SINE, Some("voice.ogg")).expect("decode mp4");
    assert_eq!(decoded.spec.sample_rate, 44_100);
    assert_eq!(decoded.spec.channels, 1);
    // The container states the timing of the track in its edit list: the
    // encoder's priming (1,024 samples) is not part of the recording, and
    // neither are the samples it padded the track with (478). The track is
    // the 22,050 samples it was encoded from — the same length the platform
    // backend returns.
    let frames = decoded.frame_count();
    assert_eq!(
        frames, 22_050,
        "0.5 s at 44.1 kHz is 22,050 frames, the container's priming and padding \
         must be removed, got {frames}"
    );
    assert_eq!(decoded.spec.total_frames, Some(22_050));
    let peak = decoded
        .interleaved
        .iter()
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    assert!(peak > 0.1, "expected audible samples, peak was {peak}");
    assert!(decoded.interleaved.iter().all(|sample| sample.is_finite()));
}

#[test]
fn mp4_aac_streaming_read_matches_whole_file() {
    let media = Media::new();
    let whole = decode_audio(&media, MP4_AAC_SINE, Some("voice.ogg")).expect("whole decode");
    let (spec, mut state) =
        open_audio(&media, Arc::from(MP4_AAC_SINE), Some("voice.ogg")).expect("open stream");

    let mut all = Vec::new();
    let mut chunk = vec![0.0f32; 256];
    loop {
        match read_audio(&media, &mut state, &mut chunk) {
            Ok(0) => break,
            Ok(written) => all.extend_from_slice(&chunk[..written]),
            Err(MediaError::EndOfStream) => break,
            Err(error) => panic!("stream read failed: {error}"),
        }
    }
    assert!(!all.is_empty());
    assert_eq!(all.len(), whole.interleaved.len());
    // The streaming reader stops where the container says the track ends:
    // the samples the encoder padded it with are not part of it.
    assert_eq!(spec.total_frames, Some(22_050));
    assert_eq!(all.len(), 22_050);
}

/// The FFmpeg backend decodes the MP4/AAC fixture into the format the
/// container declares.
///
/// The backend is the fallback for a track the built-in decoder cannot
/// handle (an AAC track with the SBR extension, whose decoder has no SBR
/// synthesis). It materializes the whole track, so the caller owns every
/// decoded sample.
///
/// The container's priming is removed: an AAC track is encoded with a delay
/// of a frame or more, and the container says how many samples it is (the
/// `initial_padding` of the track, which the demuxer hands to the decoder as
/// the skip-samples side data of the first packet). The fixture is a 0.5 s
/// encode of 22,050 samples; the decoder produces 23 AAC frames (23,552
/// samples) of which the priming (1,024) and the encoder's padding (478) are
/// removed, so the track is the length it was encoded from.
#[cfg(feature = "video-ffmpeg")]
#[test]
fn the_ffmpeg_backend_decodes_the_mp4_aac_fixture() {
    let media = Media::new();
    let decoded = decode_audio_ffmpeg(&media, Arc::from(MP4_AAC_SINE), Some("voice.ogg"))
        .expect("the ffmpeg backend decodes the track");
    assert_eq!(decoded.spec.sample_rate, 44_100);
    assert_eq!(decoded.spec.channels, 1);
    let frames = decoded.frame_count();
    assert!(
        (21_000..=22_600).contains(&frames),
        "0.5 s at 44.1 kHz is 22,050 frames, the encoder's priming must be \
         removed, got {frames}"
    );
    let peak = decoded
        .interleaved
        .iter()
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    assert!(peak > 0.1, "expected audible samples, peak was {peak}");
    assert!(decoded.interleaved.iter().all(|sample| sample.is_finite()));
}

/// The FFmpeg backend decodes a container the built-in decoder has no reader
/// for.
///
/// The AU container is the proof that the fallback is reachable: the built-in
/// decoder has no AU reader, so the track can only be decoded by FFmpeg.
#[cfg(feature = "video-ffmpeg")]
#[test]
fn the_ffmpeg_backend_decodes_a_container_the_builtin_decoder_has_no_reader_for() {
    let media = Media::new();
    let au = au_tone(8_000, 800);
    assert!(
        decode_audio(&media, &au, Some("tone.au")).is_err(),
        "the built-in decoder has no reader for this container"
    );
    let decoded = decode_audio_ffmpeg(&media, Arc::from(au), Some("tone.au"))
        .expect("the ffmpeg backend decodes the track");
    assert_eq!(decoded.spec.sample_rate, 8_000);
    assert_eq!(decoded.spec.channels, 1);
    assert_eq!(decoded.frame_count(), 800);
}

/// A Sun/NeXT AU file with 16-bit linear PCM.
///
/// The AU header is big-endian.
#[cfg(feature = "video-ffmpeg")]
fn au_tone(rate: u32, frames: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b".snd");
    bytes.extend_from_slice(&24u32.to_be_bytes());
    bytes.extend_from_slice(&(frames * 2).to_be_bytes());
    bytes.extend_from_slice(&3u32.to_be_bytes());
    bytes.extend_from_slice(&rate.to_be_bytes());
    bytes.extend_from_slice(&1u32.to_be_bytes());
    for frame in 0..frames {
        let sample = (i16::try_from(frame % 8).expect("the phase fits") - 4) * 1000;
        bytes.extend_from_slice(&sample.to_be_bytes());
    }
    bytes
}
