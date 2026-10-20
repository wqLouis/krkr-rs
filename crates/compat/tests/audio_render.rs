//! Integration tests for the L3 audio-render seam (`compat::sound_render`).
//!
//! The seam is the only thing between L2's mixer and the L1 device: it hands
//! the composition root whole interleaved frames, fills silence when nothing
//! is playing, and rejects a partial frame instead of writing a truncated one.

mod common;

use api::media::AudioSpec;
use common::vm_lock;
use compat::{CompatError, CompatHost, sound_render};
use tjs::Tjs2Engine;

/// A bound VM + host.
///
/// The engine is boxed and the host declared alongside it so the process-wide
/// binding's stored engine address stays valid when `Env` moves — returning an
/// unboxed engine from a constructor dangles that pointer and segfaults (see
/// `tests/sound.rs`). The field is underscore-prefixed only to keep the unused
/// warning away; the value must stay alive for the whole test.
struct Env {
    _host: CompatHost,
    _engine: Box<Tjs2Engine>,
}

impl Env {
    fn new() -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut host = CompatHost::new();
        host.install(&engine).expect("install compat surface");
        common::link_all_builtins(&engine);
        Env {
            _host: host,
            _engine: engine,
        }
    }

    /// Renders one buffer through the seam, exactly as the composition root
    /// does each frame.
    fn render(&self, out: &mut [f32], spec: AudioSpec) -> Result<(), CompatError> {
        let ctx = self._host.ctx().expect("compat context");
        sound_render(&ctx, out, spec)
    }
}

/// The device spec the composition root opens: 44.1 kHz stereo.
fn stereo() -> AudioSpec {
    AudioSpec {
        sample_rate: 44_100,
        channels: 2,
        total_frames: None,
    }
}

/// With nothing playing, a frame must come back as silence — written, not
/// left untouched, so a stale buffer can never leak into the device.
#[test]
fn an_idle_mixer_renders_silence() {
    let _lock = vm_lock();
    let env = Env::new();

    let mut out = vec![1.0f32; 512];
    env.render(&mut out, stereo())
        .expect("an idle mix is still a mix");
    assert!(
        out.iter().all(|sample| *sample == 0.0),
        "an idle mixer must overwrite the buffer with silence"
    );
}

/// A buffer that is not a whole number of interleaved frames is refused
/// rather than half-written.
#[test]
fn a_partial_frame_is_a_typed_error() {
    let _lock = vm_lock();
    let env = Env::new();

    let mut out = [0.5f32; 3]; // one and a half stereo frames
    let error = env
        .render(&mut out, stereo())
        .expect_err("a partial frame must be refused");
    assert!(
        matches!(&error, CompatError::Engine(message)
            if message.contains("not a whole number of")),
        "unexpected error: {error:?}"
    );
    assert_eq!(out, [0.5; 3], "a refused render must not touch the buffer");
}

/// Mono has a frame size of one sample, so any buffer length is whole frames.
#[test]
fn a_mono_buffer_is_always_whole_frames() {
    let _lock = vm_lock();
    let env = Env::new();

    let mut out = vec![1.0f32; 7];
    env.render(
        &mut out,
        AudioSpec {
            sample_rate: 48_000,
            channels: 1,
            total_frames: None,
        },
    )
    .expect("seven mono samples are seven whole frames");
    assert!(out.iter().all(|sample| *sample == 0.0));
}
