//! Integration tests for the L1 audio output device.
//!
//! The real cpal backend needs audio hardware, so these exercise the null
//! mock (the same call path, no hardware) plus the Android fail-fast path.

use api::error::AudioError;
use api::media::AudioSpec;
use platform::{AudioDevice, PlatformError};

/// A valid stereo 44.1 kHz spec.
fn stereo() -> AudioSpec {
    AudioSpec {
        sample_rate: 44_100,
        channels: 2,
        total_frames: None,
    }
}

#[test]
fn null_device_opens_with_the_requested_spec() {
    let spec = stereo();
    let (device, state) = AudioDevice::null(spec).expect("open null device");
    assert_eq!(device.spec(), spec);
    assert!(device.is_running(&state));
}

#[test]
fn null_device_accepts_frame_aligned_pcm_and_ignores_empty() {
    let (device, mut state) = AudioDevice::null(stereo()).expect("open null device");
    device
        .write(&mut state, &[0.0, 0.0, 0.5, -0.5])
        .expect("aligned write");
    device
        .write(&mut state, &[])
        .expect("empty write is a no-op");
}

#[test]
fn null_device_rejects_misaligned_pcm() {
    let (device, mut state) = AudioDevice::null(stereo()).expect("open null device");
    let error = match device.write(&mut state, &[0.0, 0.0, 0.5]) {
        Ok(()) => panic!("a half frame must be rejected"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        PlatformError::Audio(AudioError::InvalidSpec { .. })
    ));
}

#[test]
fn null_device_rejects_a_zero_sample_rate() {
    let spec = AudioSpec {
        sample_rate: 0,
        channels: 2,
        total_frames: None,
    };
    let error = match AudioDevice::null(spec) {
        Ok(_) => panic!("a zero sample rate must be rejected"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        PlatformError::Audio(AudioError::InvalidSpec { .. })
    ));
}

#[test]
fn null_device_rejects_a_zero_channel_count() {
    let spec = AudioSpec {
        sample_rate: 44_100,
        channels: 0,
        total_frames: None,
    };
    let error = match AudioDevice::null(spec) {
        Ok(_) => panic!("a zero channel count must be rejected"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        PlatformError::Audio(AudioError::InvalidSpec { .. })
    ));
}

#[test]
fn stopping_the_null_device_rejects_further_writes() {
    let (device, mut state) = AudioDevice::null(stereo()).expect("open null device");
    device.stop(&mut state);
    assert!(!device.is_running(&state));
    device.stop(&mut state); // idempotent
    assert!(!device.is_running(&state));

    let error = match device.write(&mut state, &[0.0, 0.0]) {
        Ok(()) => panic!("a stopped stream must reject writes"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        PlatformError::Audio(AudioError::NotRunning)
    ));
}

// The Android backend is real AAudio (via the `ndk` crate) and only compiles
// for an Android target. It cannot be linked or run here (`libaaudio.so` is an
// Android library), so the test is compiled and `#[ignore]`d. Run it on a
// device with `cargo test -p platform --features platform-android -- --ignored`.
#[cfg(feature = "platform-android")]
#[test]
#[ignore = "requires an Android device with libaaudio; cannot run on the host"]
fn real_device_opens_starts_and_stops() {
    let (device, mut state) = AudioDevice::open(stereo()).expect("open AAudio output");
    assert!(device.is_running(&state));
    device
        .write(&mut state, &[0.0, 0.0, 0.5, -0.5])
        .expect("aligned write");
    device.stop(&mut state);
    assert!(!device.is_running(&state));
    device.stop(&mut state); // idempotent
}
