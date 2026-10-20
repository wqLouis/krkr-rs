//! Integration tests for the `wuvorbis.dll` plugin port (OGG Vorbis).
//!
//! `system/sound.tjs:4` links `wuvorbis.dll` so `WaveSoundBuffer` can open
//! `.ogg` storages. The reference plugin registers **no** TJS class: it
//! decodes through the bundled libvorbisfile (`wu_ov_*`), attaches as the
//! win32 TSS sound module (`GetModuleInstance`), and its `OggVorbis` /
//! `wuvorbis` wide strings are that module's metadata (see the module docs
//! and `docs.old/plugin_host_exe.md` §2.4/§5.3). In krkr-rs the decoder creator
//! maps to the host decoder chain ([`compat::add_audio_decoder`]), which the
//! composition root fills with the L1 `media` symphonia ogg/vorbis backend
//! (`crates/app/src/audio.rs`) — the test below plays that role itself.
//!
//! This file proves what `crates/compat/src/natives/plugins/wuvorbis/mod.rs`
//! documents, instead of asserting it:
//!
//! * `Plugins.link("wuvorbis.dll")` resolves to the built-in table — no null
//!   plugin object, no duplicate registration on re-link;
//! * a `WaveSoundBuffer` opened over a real committed `.ogg` fixture decodes
//!   through the host chain, installed below exactly the way the composition
//!   root does (this test *is* the L4 host: `compat` itself never links a
//!   codec — `media` is a dev-dependency only);
//! * without the host decoder, or with a corrupt `.ogg`, `open` reports a
//!   typed error (D11) instead of silence.
//!
//! **wuvorbis exception:** the reference ships no TJS surface for
//! `wuvorbis.dll` (no `reference/cpp/plugins/wuvorbis*` source exists; its
//! exports are the C decoder API, `docs.old/plugin_host_exe.md:137`), so these
//! tests are **regression pins for the decode chain**, not a surface audit.
//! Each one carries a `reference/cpp/**` citation for the core behaviour it
//! leans on (the `ov_open_callbacks`/`ov_read` chain of
//! `reference/cpp/core/sound/VorbisWaveDecoder.cpp`) instead of a plugin
//! `:line` reference.

mod common;

use std::sync::Arc;

use api::fs::{FsPath, OpenMode};
use common::vm_lock;
use compat::{CompatHost, add_audio_decoder};
use engine::Storage;
use engine::sound::SoundSource;
use fs::FileSystem;
use media::{Media, decode_audio};
use tjs::{Tjs2Engine, TjsValue};

/// The committed OGG Vorbis fixture shared with the `media` tests: an
/// `ffmpeg -c:a libvorbis` encode of a 440 Hz sine tone, 0.2 s, 44.1 kHz
/// mono (~4 KB, `crates/platform/media/tests/assets/sine_440hz_0.2s.ogg`).
const OGG_SINE: &[u8] = include_bytes!("../../platform/media/tests/assets/sine_440hz_0.2s.ogg");

/// A truncated stream that starts with a real Ogg capture pattern: the
/// decoder chain must reject it with its typed error.
const BROKEN_OGG: &[u8] = b"OggS\x00\x02vorbis\x01\x00\x00\x00\x00";

/// An in-memory L2 storage populated with the given entries.
fn mount_memory(files: &[(&str, &[u8])]) -> Storage {
    let fs = FileSystem::memory();
    for (path, data) in files {
        let path_obj = std::path::Path::new(path);
        if let Some(parent) = path_obj.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(&fs, &FsPath::host(parent)).expect("create parent dirs");
        }
        let file = fs::open(&fs, &FsPath::host(path), OpenMode::Write).expect("open memory file");
        fs::write_at(&fs, &file, 0, data).expect("write memory file");
        fs::flush(&fs, &file).expect("flush memory file");
    }
    Storage::mount_archives(fs, "/", [] as [&str; 0]).expect("mount")
}

/// A bound VM + host + attached in-memory storage, optionally with the
/// `media` decoder installed on the chain.
///
/// The decoder installation mirrors `crates/app/src/audio.rs`: this test
/// plays the composition root, because L3 itself must never link a codec.
///
/// Fields drop in declaration order: the host first (removing the binding
/// and releasing every retained script object), then the engine, then the
/// storage (which outlives the binding).
struct Env {
    /// Kept alive so `Drop` removes the engine binding before the VM dies.
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    _storage: Box<Storage>,
}

impl Env {
    /// `with_media_decoder` installs the L1 `media` decoder on the chain
    /// exactly the way the host app does; `false` models a host that never
    /// called [`compat::add_audio_decoder`].
    fn new(with_media_decoder: bool) -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut storage = Box::new(mount_memory(&[
            ("sine.ogg", OGG_SINE),
            ("broken.ogg", BROKEN_OGG),
        ]));
        let mut host = CompatHost::new();
        // SAFETY: the boxed storage outlives the host (it is declared after
        // `host`, so it drops later), matching `attach_storage`.
        unsafe {
            host.services_mut()
                .attach_storage(storage.as_mut() as *mut Storage);
        }
        host.install(&engine).expect("install compat surface");
        if with_media_decoder {
            // The composition root's decoder (`crates/app/src/audio.rs`).
            let media = Media::new();
            add_audio_decoder(
                host.services(),
                Box::new(move |bytes, loops, hint| {
                    let audio =
                        decode_audio(&media, bytes, hint).map_err(|error| error.to_string())?;
                    let source = SoundSource::with_loops(Arc::new(audio), loops)
                        .map_err(|error| error.to_string())?;
                    Ok(Arc::new(source))
                }),
            );
        }
        Env {
            _host: host,
            engine,
            _storage: storage,
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "wuvorbis-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, "wuvorbis-test")
            .map_err(|error| error.to_string())
    }

    fn eval_ok(&self, expression: &str) -> TjsValue {
        self.eval(expression)
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
    }

    fn eval_int(&self, expression: &str) -> i64 {
        match self.eval_ok(expression) {
            TjsValue::Integer(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    fn eval_str(&self, expression: &str) -> String {
        match self.eval_ok(expression) {
            TjsValue::String(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected a string"),
        }
    }

    fn error(&self, expression: &str) -> String {
        self.eval(expression)
            .expect_err(&format!("{expression:?} should have raised"))
    }
}

/// Regression pin (no TJS surface in the reference — see the module docs):
/// `Plugins.link("wuvorbis.dll")` must resolve like any built-in and stay
/// idempotent, and because the reference plugin registers no class
/// (`docs.old/plugin_host_exe.md:137`: 40 C exports, no `NCB_REGISTER_CLASS`),
/// neither `wuvorbis` nor `OggVorbis` may appear on the VM.
#[test]
fn plugins_link_wuvorbis_resolves_to_the_builtin_without_a_null_object() {
    let _lock = vm_lock();
    assert!(
        compat::builtin_plugin_names().any(|name| name == "wuvorbis.dll"),
        "wuvorbis.dll must be in the built-in table"
    );
    let env = Env::new(false);

    env.run("Plugins.link('wuvorbis.dll');");
    // The link is recorded with the script's casing and is idempotent: a
    // second link neither duplicates the list nor re-registers anything.
    env.run("Plugins.link('wuvorbis.dll');");
    assert_eq!(env.eval_int("Plugins.getList().length"), 1);
    assert_eq!(env.eval_str("Plugins.getList()[0]"), "wuvorbis.dll");

    // Built-in resolution installs no null plugin object (that would define
    // a `wuvorbis` global), and the reference plugin registers no TJS class:
    // its `OggVorbis`/`wuvorbis` wide strings are TSS module metadata, so
    // neither name exists on the VM.
    for name in ["wuvorbis", "Wuvorbis", "OggVorbis"] {
        assert_eq!(
            env.eval_str(&format!("typeof global.{name}")),
            "undefined",
            "{name} must not be registered"
        );
    }
}

/// Regression pin: the reference decode chain opens the stream with
/// `ov_open_callbacks` and then reads with `ov_read`
/// (`reference/cpp/core/sound/VorbisWaveDecoder.cpp:220-226,181-186`),
/// reporting format facts (rate/channels/total time) from `ov_info` /
/// `ov_pcm_total` (`VorbisWaveDecoder.cpp:230-256`) — here observed through
/// `WaveSoundBuffer` (frequency, channels, f32 pipeline, loaded-but-stopped
/// status after `open`).
#[test]
fn wave_sound_buffer_decodes_the_ogg_fixture_through_the_host_chain() {
    let _lock = vm_lock();
    // The host appends its `media` decoder *after* `host.install`, so the
    // chain already holds the `wutcwf` plugin's TCWF decoder first: opening
    // the `.ogg` walks past it to the first decoder that succeeds — exactly
    // the composition order in `crates/app/src/game.rs`.
    let env = Env::new(true);
    env.run("Plugins.link('wuvorbis.dll');");
    env.run("var s = new WaveSoundBuffer(); s.open('sine.ogg');");

    // 0.2 s of 44.1 kHz mono Vorbis: `totalTime` is milliseconds and may
    // wobble by a few ms with encoder delay / granule rounding.
    let total = env.eval_int("s.totalTime");
    assert!(
        (190..=210).contains(&total),
        "expected ~200 ms from the fixture, got {total}"
    );
    assert_eq!(env.eval_int("s.channels"), 1);
    assert_eq!(env.eval_int("s.frequency"), 44_100);
    assert_eq!(env.eval_int("s.bits"), 32, "the engine pipeline is f32");
    // The reference's post-open state: loaded but unstarted.
    assert_eq!(env.eval_str("s.status"), "stop");

    env.run("s.play();");
    assert_eq!(env.eval_str("s.status"), "play");
    // No poll has run, so the position has not advanced.
    assert_eq!(env.eval_int("s.position"), 0);
}

/// Regression pin: the reference treats a stream it cannot open as a
/// failure — `ov_open_callbacks < 0` tears the decoder down and reports
/// false (`reference/cpp/core/sound/VorbisWaveDecoder.cpp:221-226`), never
/// silence. Without a host decoder installed, `open` must surface the
/// chain's typed error (D11) and leave the buffer unloaded.
#[test]
fn open_without_the_host_decoder_is_a_typed_error_not_silence() {
    let _lock = vm_lock();
    // The composition root's `media` decoder is absent. The only chain entry
    // is the `wutcwf` plugin's TCWF decoder (every built-in registers at
    // install), which rejects the Ogg stream by its signature; `open` reports
    // that typed failure — never a silent/zero-filled buffer — and leaves the
    // buffer unloaded.
    let env = Env::new(false);
    env.run("var s = new WaveSoundBuffer();");

    let message = env.error("s.open('sine.ogg')");
    assert!(
        message.contains("cannot decode") && message.contains("sine.ogg"),
        "unexpected message: {message}"
    );
    assert_eq!(env.eval_str("s.status"), "unload");
}

/// Regression pin: a corrupt `.ogg` fails the same open step
/// (`reference/cpp/core/sound/VorbisWaveDecoder.cpp:220-226` —
/// `ov_open_callbacks` rejects it) and the failure is reported as a typed
/// error naming the storage, never a zero-filled/silent buffer.
#[test]
fn corrupt_ogg_reports_the_decoders_typed_error() {
    let _lock = vm_lock();
    let env = Env::new(true);
    env.run("var s = new WaveSoundBuffer();");

    let message = env.error("s.open('broken.ogg')");
    assert!(
        message.contains("cannot decode") && message.contains("broken.ogg"),
        "unexpected message: {message}"
    );
    assert_eq!(env.eval_str("s.status"), "unload");
}
