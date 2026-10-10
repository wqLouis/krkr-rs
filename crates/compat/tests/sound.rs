//! Reference-behaviour tests for the `WaveSoundBuffer` native class (plus its
//! `WaveFlags` and `PhaseVocoder` helpers).
//!
//! Every test states the **KiriKiri (reference C++) expectation** with a
//! `reference/cpp/**` citation above it and asserts that expectation against
//! the observed script-visible behaviour: argument counts and what a missing
//! or wrong-typed argument does, Integer-vs-Real return types, error-vs-silent
//! paths, the side effects of property writes, event dispatch through the
//! object (a script override wins over the native member), and teardown via
//! `invalidate`.
//!
//! Ground rules applied here:
//! * Time is driven by real sleeps plus the exported
//!   [`compat::sound_poll`] — the same frame step the host runs — so the
//!   reference's 60 ms sound-buffer beat (`TVP_SB_BEAT_INTERVAL`) and the
//!   playback clock advance exactly like they do in an application. Tests
//!   that only inspect types/arity never sleep.
//! * One [`Env`] (engine + host + attached in-memory storage + a stub
//!   decoder) per test, serialized through [`common::vm_lock`].
//! * The vendored TJS2 VM (`crates/tjs/**`) is reference code: member lookup,
//!   `invalidate` ordering and the native dispatch ABI are taken as correct;
//!   divergences are attributed to `crates/compat/src/natives/sound.rs`.
//!
//! The full `open` → `play` → poll → event flow is exercised here through the
//! public surface (`compat::add_audio_decoder` installs the stub decoder), so
//! unlike before, everything runs in one integration test binary.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use api::fs::{FsPath, OpenMode};
use api::media::{AudioSpec, DecodedAudio};
use common::vm_lock;
use compat::{CompatHost, add_audio_decoder, sound_poll, sound_render};
use engine::Storage;
use engine::sound::{LoopInfo, SoundSource};
use fs::FileSystem;
use tjs::{Tjs2Engine, TjsValue};

/// Script label for diagnostics.
const SCRIPT: &str = "sound-behavior-test";

/// The stub tone: 44.1 kHz mono, 1 s — `totalTime == 1000` everywhere.
const TONE_RATE: u32 = 44_100;
const TONE_SECONDS: f64 = 1.0;

/// The `.sli` side-car with a label 0.2 s in (8820 granules).
const SLI_MID: &[u8] = b"#2.00\nLabel { Position=8820; Name='mid'; }\n";
/// A label exactly at the first sample (its event is what a status change
/// cancels in the reference).
const SLI_ZERO: &[u8] = b"#2.00\nLabel { Position=0; Name='zero'; }\n";
/// A side-car the parser rejects (`TVPInvalidLoopInformation`).
const SLI_CORRUPT: &[u8] = b"#2.00\nBogus { From=1; }\n";

/// The storage entry the stub decoder refuses to decode (the reference's
/// `TVPCreateWaveDecoder` throwing on an unknown format).
const UNDECODABLE: &str = "undecodable.wav";

/// A synthetic mono sine tone as an L2 source (the `.sli` parse result is
/// attached by the caller, exactly like the reference decoder chain).
fn tone(rate: u32, channels: u16, seconds: f64, info: Option<LoopInfo>) -> Arc<SoundSource> {
    let frames = (f64::from(rate) * seconds) as usize;
    let mut interleaved = Vec::with_capacity(frames * usize::from(channels));
    for frame in 0..frames {
        let value = (2.0 * std::f64::consts::PI * 440.0 * frame as f64 / f64::from(rate)).sin()
            as f32
            * 0.5;
        for _ in 0..channels {
            interleaved.push(value);
        }
    }
    let spec = AudioSpec {
        sample_rate: rate,
        channels,
        total_frames: Some(frames as u64),
    };
    Arc::new(
        SoundSource::with_loops(Arc::new(DecodedAudio::new(spec, interleaved)), info)
            .expect("valid source"),
    )
}

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

/// A bound VM + host + optional in-memory storage + optional stub decoder.
///
/// `_host` is declared first so it drops first (removing the process-wide
/// binding and releasing every retained script object) before the engine and
/// the storage go away.
struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    _storage: Option<Box<Storage>>,
}

impl Env {
    /// The usual environment: storage attached, stub decoder installed.
    fn new(files: &[(&str, &[u8])]) -> Env {
        Env::configured(files, true, true)
    }

    /// No storage attached (`open` must raise a typed error).
    fn without_storage() -> Env {
        Env::configured(&[], false, true)
    }

    /// Storage plus the built-in decoders (`wutcwf`'s TCWF decoder is always
    /// registered by `host.install`), but none of the test's stub decoders.
    fn without_decoder() -> Env {
        Env::configured(&[("tone.wav", b"RIFF")], true, false)
    }

    fn configured(files: &[(&str, &[u8])], with_storage: bool, with_decoder: bool) -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut storage = if with_storage {
            Some(Box::new(mount_memory(files)))
        } else {
            None
        };
        let mut host = CompatHost::new();
        if let Some(storage) = storage.as_mut() {
            // SAFETY: the boxed storage outlives the host (it is declared
            // after `_host`, so it drops later), matching `attach_storage`.
            unsafe {
                host.services_mut()
                    .attach_storage(storage.as_mut() as *mut Storage);
            }
        }
        host.install(&engine).expect("install compat surface");
        if with_decoder {
            add_audio_decoder(
                host.services(),
                Box::new(|_bytes, info, hint| {
                    if hint == Some(UNDECODABLE) {
                        return Err("stub decoder: unknown wave format".to_string());
                    }
                    Ok(tone(TONE_RATE, 1, TONE_SECONDS, info))
                }),
            );
        }
        Env {
            _host: host,
            engine,
            _storage: storage,
        }
    }

    /// The standard fixtures: `tone.wav` (optionally with `sli`), an
    /// `other.wav` without a side-car, a `bad.wav` with a corrupt side-car and
    /// an `undecodable.wav` the stub rejects.
    fn fixture(sli: Option<&[u8]>) -> Env {
        let mut files: Vec<(&str, &[u8])> = vec![
            ("tone.wav", b"RIFF"),
            ("other.wav", b"RIFF"),
            ("bad.wav", b"RIFF"),
            ("bad.wav.sli", SLI_CORRUPT),
            (UNDECODABLE, b"RIFF"),
        ];
        if let Some(sli) = sli {
            files.push(("tone.wav.sli", sli));
        }
        Env::new(&files)
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, SCRIPT)
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    /// Run a script and keep a failure observable instead of panicking (for
    /// cases where the reference *accepts* the script and our port may raise).
    fn try_run(&self, script: &str) -> Result<(), String> {
        self.engine
            .exec_script(script, SCRIPT)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, SCRIPT)
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

    fn eval_real(&self, expression: &str) -> f64 {
        match self.eval_ok(expression) {
            TjsValue::Real(value) => value,
            TjsValue::Integer(value) => value as f64,
            other => panic!("{expression:?} returned {other:?}, expected a real"),
        }
    }

    fn error(&self, expression: &str) -> String {
        self.eval(expression)
            .expect_err(&format!("{expression:?} should have raised"))
    }

    /// One frame-body `sound_poll` step.
    fn poll(&self) {
        let mut ctx = self._host.ctx().expect("bound host");
        sound_poll(&mut ctx).expect("sound poll");
    }

    /// Poll every `step_ms` until `total_ms` of real time elapsed.
    fn poll_for(&self, total_ms: u64, step_ms: u64) {
        let deadline = Instant::now() + Duration::from_millis(total_ms);
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(step_ms));
            self.poll();
        }
    }

    /// Render the mixer output and report the peak absolute sample — the
    /// observable "is this buffer audibly playing" signal.
    fn peak(&self) -> f32 {
        let ctx = self._host.ctx().expect("bound host");
        let mut out = [0.0f32; 2048];
        let frames = out.len() as u64;
        sound_render(
            &ctx,
            &mut out,
            AudioSpec {
                sample_rate: TONE_RATE,
                channels: 1,
                total_frames: Some(frames),
            },
        )
        .expect("render");
        out.iter().fold(0.0f32, |max, sample| max.max(sample.abs()))
    }
}

// ---------------------------------------------------------------------------
// 1. Constructor
// ---------------------------------------------------------------------------

/// Reference: the constructor chain ends in `tTJSNI_BaseSoundBuffer::Construct`,
/// which stores `ActionOwner = param[0]->AsObjectClosure()`
/// (`reference/cpp/core/sound/SoundBufferBaseIntf.cpp:44-45`) after the
/// instance defaults were set in `tTJSNI_WaveSoundBuffer`'s constructor:
/// `Volume/Volume2 = 100000`, `Pan = 0`, `PosX/Y/Z = 0`
/// (`reference/cpp/core/sound/win32/WaveImpl.cpp:1911-1915`),
/// `Looping/BufferPlaying/Paused = false` (`.../WaveImpl.cpp:1929-1932`) and
/// the base's `Status = ssUnload` (`SoundBufferBaseIntf.cpp:24`), so
/// `status` reads `"unload"` (`SoundBufferBaseIntf.cpp:58-75`). The statics
/// start at `GlobalVolume = 100000` / `sgfmNeverMute == 0`
/// (`.../WaveImpl.cpp:1888-1891`).
#[test]
fn constructor_defaults_match_the_reference_ctor() {
    let _lock = vm_lock();
    let env = Env::fixture(None);

    env.run("var s = new WaveSoundBuffer();");

    assert_eq!(env.eval_int("s.volume"), 100_000);
    assert_eq!(env.eval_int("s.volume2"), 100_000);
    assert_eq!(env.eval_int("s.pan"), 0);
    assert_eq!(env.eval_int("s.looping"), 0);
    assert_eq!(env.eval_int("s.paused"), 0);
    assert_eq!(env.eval_int("s.position"), 0);
    assert_eq!(env.eval_int("s.samplePosition"), 0);
    // `GetTotalTime` on the zeroed `InputFormat` divides by zero in the
    // reference (`WaveImpl.cpp:3162-3165`) — undefined behaviour the port
    // cannot reproduce, so it reports `0` there (see `open` tests for the
    // loaded value).
    assert_eq!(env.eval_int("s.totalTime"), 0);
    assert_eq!(env.eval_int("s.frequency"), 0);
    assert_eq!(env.eval_int("s.bits"), 0);
    assert_eq!(env.eval_int("s.channels"), 0);
    assert_eq!(env.eval_int("s.useVisBuffer"), 0);
    assert_eq!(env.eval_ok("s.status"), TjsValue::String("unload".into()));
    assert_eq!(env.eval_real("s.posX"), 0.0);
    assert_eq!(env.eval_real("s.posY"), 0.0);
    assert_eq!(env.eval_real("s.posZ"), 0.0);

    // `flags` is the live, cached `WaveFlags` proxy
    // (`WaveIntf.cpp:1026-1037`); `filters` is the one array the native
    // instance creates in its constructor and hands out with
    // `GetFiltersNoAddRef` (`WaveIntf.cpp:866-869`,
    // `WaveIntf.h:214`); `labels` is a dictionary (cached the same way,
    // `WaveIntf.cpp:1039-1043`).
    assert_eq!(env.eval_int("s.flags === s.flags"), 1);
    assert_eq!(env.eval_int("s.filters === s.filters"), 1);
    assert_eq!(env.eval_int("s.filters.length"), 0);
    assert!(matches!(env.eval_ok("s.labels"), TjsValue::Object));

    // Statics (`WaveImpl.cpp:1888-1891`).
    assert_eq!(env.eval_int("WaveSoundBuffer.globalVolume"), 100_000);
    assert_eq!(env.eval_int("WaveSoundBuffer.globalFocusMode"), 0);
}

/// Reference: the constructor **requires** one argument — `numparams < 1`
/// returns `TJS_E_BADPARAMCOUNT` (`reference/cpp/core/sound/SoundBufferBaseIntf.cpp:32-33`,
/// reached because the native-constructor macro runs `Construct` before the
/// empty body, `reference/cpp/core/tjs2/tjsNative.h:405-419`). Any *value* is
/// accepted as the action owner: `param[0]->AsObjectClosure()` yields the
/// empty closure for a non-object (`SoundBufferBaseIntf.cpp:44-45`), which the
/// event members never invoke (`reference/cpp/core/sound/WaveIntf.cpp:1197`).
///
/// The port tolerates the missing argument (documented divergence: three test
/// suites outside this module construct `new WaveSoundBuffer()`), storing
/// `None` — exactly the reference's empty closure for `new WaveSoundBuffer(42)`.
#[test]
fn construction_accepts_the_reference_argument_shapes() {
    let _lock = vm_lock();
    let env = Env::fixture(None);

    // Documented port divergence: the reference raises
    // TJS_E_BADPARAMCOUNT here (SoundBufferBaseIntf.cpp:32-33).
    if let Err(error) = env.try_run("var zero = new WaveSoundBuffer();") {
        panic!("the port unexpectedly rejected `new WaveSoundBuffer()`: {error}");
    }
    assert_eq!(
        env.eval_ok("zero.status"),
        TjsValue::String("unload".into())
    );

    // A non-object argument is the reference's empty closure — accepted and
    // never invoked (SoundBufferBaseIntf.cpp:44-45, WaveIntf.cpp:1197).
    env.run("var fortytwo = new WaveSoundBuffer(42);");
    assert_eq!(
        env.eval_ok("fortytwo.status"),
        TjsValue::String("unload".into())
    );
}

// ---------------------------------------------------------------------------
// 2. Argument counts and type coercion
// ---------------------------------------------------------------------------

/// Reference: `open` returns `TJS_E_BADPARAMCOUNT` when `numparams < 1`
/// (`reference/cpp/core/sound/WaveIntf.cpp:1113-1114`); with an argument it
/// tries to create a decoder for the storage and throws when that fails
/// (`WaveIntf.cpp:750-767`), so a missing storage/decoder is an error, not a
/// silent no-op.
#[test]
fn open_requires_an_argument() {
    let _lock = vm_lock();

    // The compat binding is process-global and single
    // (`CompatHost::install` fails with "compat binding already installed",
    // `crates/compat/src/binding.rs:73-84`), so each `Env` must be dropped
    // before the next one installs. Two live `Env`s in one test would
    // otherwise collide here.
    {
        let env = Env::fixture(None);
        env.run("var s = new WaveSoundBuffer();");

        let message = env.error("s.open()");
        assert!(
            message.contains("storage name"),
            "unexpected message: {message}"
        );
    }

    // No *host* decoder installed. The built-in `wutcwf` TCWF decoder is
    // still on the chain (`wutcwf::register` calls `add_audio_decoder`,
    // `crates/compat/src/natives/plugins/wutcwf/mod.rs:418-431`, so
    // `host.install` always leaves at least one decoder), and it rejects the
    // RIFF fixture by signature. `open` reports the aggregated typed failure
    // naming the storage (`crates/compat/src/natives/sound.rs:1246-1270`),
    // like the reference `TVPCreateWaveDecoder` throwing
    // `TVPUnknownWaveFormat` when no decoder accepts the stream
    // (`reference/cpp/core/sound/WaveIntf.cpp:750-767`).
    {
        let env = Env::without_decoder();
        env.run("var s = new WaveSoundBuffer();");
        let message = env.error("s.open('tone.wav')");
        assert!(
            message.contains("cannot decode") && message.contains("tone.wav"),
            "unexpected message: {message}"
        );
    }
}

/// Reference: `fade` returns `TJS_E_BADPARAMCOUNT` for fewer than two
/// arguments (`reference/cpp/core/sound/WaveIntf.cpp:1144-1145`); the delay
/// only counts when a third argument is present and not void
/// (`WaveIntf.cpp:1152-1153`); and `Fade` throws `TVPInvalidParam` —
/// "Invalid argument" — for `time <= 0` or `delay < 0`
/// (`reference/cpp/core/sound/SoundBufferBaseIntf.cpp:155-156`,
/// `reference/cpp/core/base/MsgIntfInc.h:15`).
#[test]
fn fade_requires_two_arguments_and_valid_times() {
    let _lock = vm_lock();
    let env = Env::fixture(None);
    env.run("var s = new WaveSoundBuffer();");

    // Arity.
    let message = env.error("s.fade(0)");
    assert!(message.contains("fade"), "unexpected message: {message}");

    // `time <= 0` → "Invalid argument".
    for script in ["s.fade(0, 0)", "s.fade(0, -50)"] {
        let message = env.error(script);
        assert!(
            message.contains("Invalid argument"),
            "{script}: unexpected message: {message}"
        );
    }

    // `delay < 0` → "Invalid argument".
    let message = env.error("s.fade(0, 500, -1)");
    assert!(
        message.contains("Invalid argument"),
        "unexpected message: {message}"
    );

    // A void third argument is “absent” (`WaveIntf.cpp:1152-1153`), so it
    // neither raises nor contributes a delay: the ramp starts on the first
    // beat (verified by the immediate volume movement below).
    env.run("var voiddelay; var s2 = new WaveSoundBuffer(); s2.fade(0, 600, voiddelay);");
    env.poll_for(150, 40);
    assert!(
        env.eval_int("s2.volume") < 100_000,
        "a void delay must act as delay 0 (SoundBufferBaseIntf.cpp:164-167); \
         observed volume {}",
        env.eval_int("s2.volume")
    );

    // A real third argument is the delay in milliseconds: nothing moves until
    // `BlankLeft` drained (SoundBufferBaseIntf.cpp:137-144).
    env.run("var s3 = new WaveSoundBuffer(); s3.fade(0, 600, 700);");
    env.poll_for(150, 40);
    assert_eq!(
        env.eval_int("s3.volume"),
        100_000,
        "a 700 ms delay must hold the volume for the first beats"
    );
}

/// Reference: `setPos` returns `TJS_E_BADPARAMCOUNT` for fewer than three
/// arguments (`reference/cpp/core/sound/WaveIntf.cpp:1176-1177`) and stores
/// all three as `tTVReal` (`WaveIntf.cpp:1179-1185`).
#[test]
fn set_pos_requires_three_arguments() {
    let _lock = vm_lock();
    let env = Env::fixture(None);
    env.run("var s = new WaveSoundBuffer();");

    let message = env.error("s.setPos(1, 2)");
    assert!(message.contains("setPos"), "unexpected message: {message}");
    env.run("s.setPos(1, 2, 3);");
    assert_eq!(env.eval_real("s.posX"), 1.0);
    assert_eq!(env.eval_real("s.posY"), 2.0);
    assert_eq!(env.eval_real("s.posZ"), 3.0);
}

/// Reference: `getVisBuffer` is registered outside the member table and
/// returns `TJS_E_BADPARAMCOUNT` for fewer than three arguments
/// (`reference/cpp/core/sound/win32/WaveImpl.cpp:3510-3531`, check at
/// 3515-3516). The port captures no visualization buffer, so the count it
/// reports is the reference's `0` for `!UseVisBuffer` (first guard,
/// `WaveImpl.cpp:3406-3407`).
#[test]
fn get_vis_buffer_requires_three_arguments_and_reports_zero() {
    let _lock = vm_lock();
    let env = Env::fixture(None);
    env.run("var s = new WaveSoundBuffer();");

    let message = env.error("s.getVisBuffer(0, 16)");
    assert!(
        message.contains("getVisBuffer"),
        "unexpected message: {message}"
    );
    assert_eq!(env.eval_int("s.getVisBuffer(0, 16, 1)"), 0);
    assert_eq!(env.eval_int("s.getVisBuffer(0, 16, 1, 4)"), 0);
}

/// Reference: `status`, `bits`, `channels`, `flags`, `labels` and `filters`
/// are declared with `TJS_DENY_NATIVE_PROP_SETTER`, which returns
/// `TJS_E_ACCESSDENYED` on writes (`reference/cpp/core/tjs2/tjsNative.h:463-466`,
/// used at `reference/cpp/core/sound/WaveIntf.cpp:1495-1497,1533-1535,1547-1549,1561-1563,1575-1577,1589-1591`);
/// `WaveFlags.count` is denied too (`WaveIntf.cpp:1715-1717`) and so is
/// `PhaseVocoder.interface` (`reference/cpp/core/sound/PhaseVocoderFilter.cpp:50-51`).
#[test]
fn read_only_members_reject_assignment() {
    let _lock = vm_lock();
    let env = Env::fixture(None);
    env.run("var s = new WaveSoundBuffer(); var pv = new WaveSoundBuffer.PhaseVocoder();");

    for member in ["status", "bits", "channels", "flags", "labels", "filters"] {
        let script = format!("s.{member} = 1;");
        let message = env
            .try_run(&script)
            .expect_err(&format!("{script} must raise (TJS_E_ACCESSDENYED)"));
        println!("observed {script} error (matches the reference): {message}");
    }
    for script in ["s.flags.count = 1;", "s.flags.N = 1;", "pv.interface = 1;"] {
        let message = env
            .try_run(script)
            .expect_err(&format!("{script} must raise"));
        println!("observed {script} error (matches the reference): {message}");
    }
}

/// Reference: unlike the denied members, `totalTime`'s setter is registered
/// but marked "not yet implemented" (`reference/cpp/core/sound/WaveIntf.cpp:1319-1330`):
/// the write succeeds, is accepted in any type, and changes nothing.
#[test]
fn total_time_is_writable_but_ignored() {
    let _lock = vm_lock();
    let env = Env::fixture(None);

    env.run("var s = new WaveSoundBuffer(); s.totalTime = 42;");
    assert_eq!(env.eval_int("s.totalTime"), 0, "the write changed nothing");

    env.run("s.open('tone.wav'); s.totalTime = 'nonsense';");
    assert_eq!(
        env.eval_int("s.totalTime"),
        1000,
        "the write changed nothing (WaveIntf.cpp:1319-1330)"
    );
}

// ---------------------------------------------------------------------------
// 3. Return types
// ---------------------------------------------------------------------------

/// Reference: the getters cast to `tjs_int64`/`tjs_int` (Integer) —
/// `position` (`WaveIntf.cpp:1247-1251`), `samplePosition` (1269-1273),
/// `paused` (1291-1295), `totalTime` (1313-1317), `looping` (1335-1339),
/// `volume` (1357-1361), `volume2` (1379-1383), `pan` (1401-1405),
/// `frequency` (1503-1507), `bits` (1525-1529), `channels` (1540-1544),
/// `globalVolume` (1593-1595) and `globalFocusMode` (1609-1612) — while
/// `posX`/`posY`/`posZ` cast to `tTVReal` (Real, `WaveIntf.cpp:1423-1427`,
/// 1445-1449, 1467-1471) and `status` is a `ttstr` (String,
/// `WaveIntf.cpp:1488-1492` via `GetStatusString`).
#[test]
fn return_types_are_integer_real_or_string() {
    let _lock = vm_lock();
    let env = Env::fixture(None);
    env.run("var s = new WaveSoundBuffer(); s.open('tone.wav');");

    for member in [
        "s.position",
        "s.samplePosition",
        "s.paused",
        "s.totalTime",
        "s.looping",
        "s.volume",
        "s.volume2",
        "s.pan",
        "s.frequency",
        "s.bits",
        "s.channels",
        "s.useVisBuffer",
        "WaveSoundBuffer.globalVolume",
        "WaveSoundBuffer.globalFocusMode",
        "s.getVisBuffer(0, 4, 1)",
    ] {
        assert!(
            matches!(env.eval_ok(member), TjsValue::Integer(_)),
            "{member} must be an Integer, got {:?}",
            env.eval_ok(member)
        );
    }

    // posX/Y/Z are reals (non-integral to defeat any Integer normalization).
    env.run("s.posX = 1.5;");
    for member in ["s.posX", "s.posY", "s.posZ"] {
        assert!(
            matches!(env.eval_ok(member), TjsValue::Real(_)),
            "{member} must be a Real, got {:?}",
            env.eval_ok(member)
        );
    }

    // status is a string.
    assert_eq!(env.eval_ok("s.status"), TjsValue::String("stop".into()));
}

// ---------------------------------------------------------------------------
// 4. Property write side effects
// ---------------------------------------------------------------------------

/// Reference: `SetVolume` clamps to `0..100000`, `SetPan` to
/// `-100000..100000` (`reference/cpp/core/sound/win32/WaveImpl.cpp:3219-3231`
/// and `3245-3256`); the getters return the stored integers unchanged.
#[test]
fn volume_and_pan_clamp_to_the_reference_scale() {
    let _lock = vm_lock();
    let env = Env::fixture(None);
    env.run("var s = new WaveSoundBuffer();");

    env.run("s.volume = 50000; s.volume2 = 25000; s.pan = -75000;");
    assert_eq!(env.eval_int("s.volume"), 50_000);
    assert_eq!(env.eval_int("s.volume2"), 25_000);
    assert_eq!(env.eval_int("s.pan"), -75_000);

    env.run("s.volume = 200000; s.volume2 = -5; s.pan = 200000;");
    assert_eq!(env.eval_int("s.volume"), 100_000);
    assert_eq!(env.eval_int("s.volume2"), 0);
    assert_eq!(env.eval_int("s.pan"), 100_000);
    env.run("s.pan = -200000;");
    assert_eq!(env.eval_int("s.pan"), -100_000);
}

/// Reference: `SetGlobalVolume` clamps to `0..100000`
/// (`reference/cpp/core/sound/win32/WaveImpl.cpp:3273-3285`), while
/// `globalFocusMode` is stored **verbatim** — the setter casts the script
/// integer into the enum with no range check (`reference/cpp/core/sound/WaveIntf.cpp:1616-1618`)
/// and the getter re-casts the stored static (`WaveIntf.cpp:1609-1612`).
#[test]
fn global_volume_clamps_and_focus_mode_is_stored_verbatim() {
    let _lock = vm_lock();
    let env = Env::fixture(None);

    env.run("WaveSoundBuffer.globalVolume = 250000;");
    assert_eq!(env.eval_int("WaveSoundBuffer.globalVolume"), 100_000);
    env.run("WaveSoundBuffer.globalVolume = -5;");
    assert_eq!(env.eval_int("WaveSoundBuffer.globalVolume"), 0);

    env.run("WaveSoundBuffer.globalFocusMode = 7;");
    assert_eq!(env.eval_int("WaveSoundBuffer.globalFocusMode"), 7);
    env.run("WaveSoundBuffer.globalFocusMode = -1;");
    assert_eq!(env.eval_int("WaveSoundBuffer.globalFocusMode"), -1);
    env.run("WaveSoundBuffer.globalFocusMode = 0;");
    assert_eq!(env.eval_int("WaveSoundBuffer.globalFocusMode"), 0);

    // `freeDirectSound` is the static no-op that releases DirectSound
    // (`WaveImpl.cpp:3499-3508`); the port has no device, but it returns
    // void like the reference returns `TJS_S_OK`.
    assert_eq!(
        env.eval_ok("WaveSoundBuffer.freeDirectSound()"),
        TjsValue::Void
    );
}

/// Reference: `SetFrequency` stores the value verbatim — no clamp, no reset
/// sentinel — into the `Frequency` field (`reference/cpp/core/sound/win32/WaveImpl.cpp:3336-3342`),
/// the getter reads that field (`reference/cpp/core/sound/win32/WaveImpl.h:230-232`),
/// and `Open` overwrites it with `InputFormat.SamplesPerSec`
/// (`WaveImpl.cpp:3056`).
#[test]
fn frequency_stores_verbatim_and_open_rederives_it() {
    let _lock = vm_lock();
    let env = Env::fixture(None);

    env.run("var s = new WaveSoundBuffer();");
    assert_eq!(env.eval_int("s.frequency"), 0);

    env.run("s.frequency = 44100;");
    assert_eq!(env.eval_int("s.frequency"), 44_100);
    // `0` is a *value*, not a “reset to native rate” sentinel in the
    // reference — it reads back as 0.
    env.run("s.frequency = 0;");
    assert_eq!(env.eval_int("s.frequency"), 0);
    env.run("s.frequency = -1000;");
    assert_eq!(env.eval_int("s.frequency"), -1000);

    env.run("s.open('tone.wav');");
    assert_eq!(
        env.eval_int("s.frequency"),
        44_100,
        "Open assigns Frequency = InputFormat.SamplesPerSec (WaveImpl.cpp:3056)"
    );
    env.run("s.frequency = 22050;");
    assert_eq!(env.eval_int("s.frequency"), 22_050);
    env.run("s.open('other.wav');");
    assert_eq!(env.eval_int("s.frequency"), 44_100);
}

/// Reference: the frequency member has **no playback effect** in the checked-in
/// reference: `SetFrequencyToBuffer` is an empty shell whose only action is
/// commented out (`reference/cpp/core/sound/win32/WaveImpl.cpp:3329-3334`)
/// and `BufferCanControlFrequency = true` only exists inside a disabled
/// `#if 0` block (`WaveImpl.cpp:2045-2046`). The playhead therefore advances
/// at real time regardless of the member.
#[test]
fn frequency_never_changes_the_playback_rate() {
    let _lock = vm_lock();
    let env = Env::fixture(None);

    // Warm poll: absorb the environment-setup time from the mixer clock so
    // the measured window starts at `play`.
    env.run("var s = new WaveSoundBuffer();");
    env.poll();

    env.run("s.open('tone.wav'); s.frequency = 22050; s.play();");
    env.poll_for(400, 50);
    let position = env.eval_int("s.position");
    assert!(
        (280..=520).contains(&position),
        "position must advance ~400 ms (frequency must not halve the rate); \
         observed {position}"
    );
}

/// Reference: `looping` is an instance flag (`WaveImpl.cpp:3090-3096`) that
/// `Open` re-applies to the *fresh* loop manager (`WaveImpl.cpp:3047-3049`)
/// — it is never reset by `open`.
#[test]
fn looping_survives_an_open() {
    let _lock = vm_lock();
    let env = Env::fixture(None);

    env.run("var s = new WaveSoundBuffer(); s.looping = true;");
    assert_eq!(env.eval_int("s.looping"), 1);
    env.run("s.open('tone.wav');");
    assert_eq!(
        env.eval_int("s.looping"),
        1,
        "Open must keep the instance Looping flag (WaveImpl.cpp:3049)"
    );
    env.run("s.open('other.wav');");
    assert_eq!(env.eval_int("s.looping"), 1);
    env.run("s.looping = false;");
    assert_eq!(env.eval_int("s.looping"), 0);
}

// ---------------------------------------------------------------------------
// 5. Playback and position
// ---------------------------------------------------------------------------

/// Reference: `GetPosition`/`GetSamplePosition` return `0` while either the
/// decoder or the DirectSound secondary buffer is absent
/// (`reference/cpp/core/sound/win32/WaveImpl.cpp:3147-3152` and `3097-3102`);
/// the buffer is only created by the first `StartPlay` (`WaveImpl.cpp:2912`),
/// so a seek performed *before* `play` is invisible until then — and then
/// shows up, because `StartPlay` fills from the loop manager's position
/// (`WaveImpl.cpp:2956-2957` “play from first or current position”).
/// `Stop()` rewinds that position to `0` (`WaveImpl.cpp:2992-2994`).
#[test]
fn position_is_zero_until_the_first_play_then_seeks() {
    let _lock = vm_lock();
    let env = Env::fixture(None);

    env.run(
        "var s = new WaveSoundBuffer(); \
         s.open('tone.wav'); \
         s.position = 500;",
    );
    assert_eq!(
        env.eval_int("s.position"),
        0,
        "no device before the first play (WaveImpl.cpp:3149-3150)"
    );

    env.run("s.play();");
    assert_eq!(
        env.eval_int("s.position"),
        500,
        "the pre-play seek must be honoured (WaveImpl.cpp:2956-2957)"
    );

    env.run("s.stop();");
    assert_eq!(
        env.eval_int("s.position"),
        0,
        "Stop rewinds (WaveImpl.cpp:2992-2994)"
    );
}

/// Reference: `samplePosition` is the granule position —
/// `GetSamplePosition` reads the device's play cursor in samples
/// (`reference/cpp/core/sound/win32/WaveImpl.cpp:3097-3102`) and the setter
/// seeks there, ignoring values at or past `TotalSamples` (the
/// `TotalSamples <= possamples` guard, `WaveImpl.cpp:3135-3136`) and negative
/// values (they wrap huge through `tjs_uint64`, `WaveIntf.cpp:1278-1281`).
#[test]
fn sample_position_round_trips_in_granules_and_ignores_out_of_range_seeks() {
    let _lock = vm_lock();
    let env = Env::fixture(None);

    env.run("var s = new WaveSoundBuffer(); s.open('tone.wav'); s.play();");
    env.run("s.samplePosition = 22050;");
    assert_eq!(env.eval_int("s.samplePosition"), 22_050);
    assert_eq!(env.eval_int("s.position"), 500, "22050 granules = 500 ms");

    // Out of range: the buffer has 44100 frames, so the seek is ignored.
    env.run("s.samplePosition = 99999999;");
    assert_eq!(env.eval_int("s.samplePosition"), 22_050);
    // Negative: wraps to a huge unsigned value → ignored as well.
    env.run("s.samplePosition = -1;");
    assert_eq!(env.eval_int("s.samplePosition"), 22_050);
}

/// Reference: `SetPaused` only flips the flag (`reference/cpp/core/sound/win32/WaveImpl.cpp:3011-3021`);
/// the device stays “playing” in the reference's `BufferPlaying` sense while
/// paused, and `Play()` still reports `ssPlay`
/// (`WaveImpl.cpp:2976-2977`), so `status` remains `"play"` while the
/// playhead does not move.
#[test]
fn paused_freezes_the_position_while_status_stays_play() {
    let _lock = vm_lock();
    let env = Env::fixture(None);

    env.run(
        "var s = new WaveSoundBuffer(); \
         s.open('tone.wav'); s.play(); s.paused = true;",
    );
    env.poll_for(150, 40);
    assert_eq!(env.eval_int("s.position"), 0, "paused must not advance");
    assert_eq!(env.eval_int("s.paused"), 1);
    assert_eq!(
        env.eval_ok("s.status"),
        TjsValue::String("play".into()),
        "pause is not a status (WaveImpl.cpp:2976-2977)"
    );

    env.run("s.paused = false;");
    env.poll_for(150, 40);
    assert!(
        env.eval_int("s.position") > 20,
        "resuming must advance again; observed {}",
        env.eval_int("s.position")
    );
}

/// Reference: `Play()` restarts a buffer whose track ran out — the device
/// stops itself at the end and resets the loop manager to `0`
/// (`reference/cpp/core/sound/win32/WaveImpl.cpp:2658-2662`), clearing
/// `BufferPlaying` so the `if(BufferPlaying) return;` guard
/// (`WaveImpl.cpp:2960-2961`) does not trip.
#[test]
fn a_completed_track_can_be_replayed() {
    let _lock = vm_lock();
    let env = Env::fixture(None);

    env.run("var s = new WaveSoundBuffer();");
    env.poll();
    env.run("s.open('tone.wav'); s.play();");
    env.poll_for(1150, 50);
    assert_eq!(env.eval_ok("s.status"), TjsValue::String("stop".into()));
    assert_eq!(env.eval_int("s.position"), 1000);

    env.run("s.play();");
    assert_eq!(env.eval_ok("s.status"), TjsValue::String("play".into()));
    assert_eq!(
        env.eval_int("s.position"),
        0,
        "a replay starts from the reset position (WaveImpl.cpp:2661-2662)"
    );
    env.poll_for(150, 40);
    assert!(
        (60..400).contains(&env.eval_int("s.position")),
        "replay must advance; observed {}",
        env.eval_int("s.position")
    );
}

// ---------------------------------------------------------------------------
// 6. Status events (synchronous delivery)
// ---------------------------------------------------------------------------

/// Reference: `SetStatus` posts `onStatusChanged` with `TVP_EPT_IMMEDIATE` —
/// the event is delivered **inside** the call that changed the status
/// (`reference/cpp/core/sound/SoundBufferBaseIntf.cpp:77-95`,
/// `reference/cpp/core/base/EventIntf.cpp:217-228`) — for `open`
/// (`WaveImpl.cpp:3086`), `play` (`WaveImpl.cpp:2976-2977`) and `stop`
/// (`WaveImpl.cpp:2989-2990`, which raises nothing while `ssUnload`). The
/// natural end of a track goes through `SetStatusAsync`
/// (`WaveImpl.cpp:3022-3034`, `SoundBufferBaseIntf.cpp:98-116`) and is
/// therefore delivered at the next pump — here, the next `sound_poll`.
#[test]
fn status_events_are_delivered_inside_the_call() {
    let _lock = vm_lock();
    let env = Env::fixture(None);

    env.run(
        "var got = []; \
         var s = new WaveSoundBuffer(); \
         s.onStatusChanged = function(st) { got.add(st); };",
    );

    // `stop` on the never-opened buffer: `Status != ssUnload` fails, no event.
    env.run("s.stop();");
    assert_eq!(
        env.eval_int("got.count"),
        0,
        "stop on unload raises nothing"
    );

    env.run("s.open('tone.wav');");
    assert_eq!(
        env.eval_int("got.count"),
        1,
        "SetStatus(ssStop) must be delivered during open (WaveImpl.cpp:3086)"
    );
    assert_eq!(env.eval_str("got[0]"), "stop");

    env.run("s.play();");
    assert_eq!(env.eval_str("got[1]"), "play");

    env.run("s.stop();");
    assert_eq!(env.eval_str("got[2]"), "stop");
    env.run("s.stop();");
    assert_eq!(
        env.eval_int("got.count"),
        3,
        "a second stop changes no status"
    );

    // The natural end is asynchronous: delivered by the next poll.
    env.run("s.play();");
    assert_eq!(env.eval_int("got.count"), 4);
    assert_eq!(env.eval_str("got[3]"), "play");
    env.poll_for(1150, 50);
    assert_eq!(env.eval_int("got.count"), 5);
    assert_eq!(
        env.eval_str("got[4]"),
        "stop",
        "the async status change arrives with the poll"
    );
}

/// Reference: the status event is posted to **the object** —
/// `TVPPostEvent(Owner, Owner, ...)` (`SoundBufferBaseIntf.cpp:86-91`), so a
/// script member (own property or subclass override) shadows the native
/// member, which is what re-invokes the action owner
/// (`reference/cpp/core/sound/WaveIntf.cpp:1195-1208`). An override that
/// does not call `super` therefore keeps the constructor's action owner out
/// of the dispatch; `super.<member>(...)` reaches the native (and with it the
/// owner) — the vendored VM resolves `super` against the super class
/// (`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2814-2851`).
#[test]
fn script_override_wins_over_the_native_event_member() {
    let _lock = vm_lock();
    let env = Env::fixture(Some(SLI_MID));

    env.run(
        "var got = []; var native = []; \
         class Ctrl { \
             function Ctrl() {} \
             function action(ev) { native.add(ev.type); } \
         } \
         var ctrl = new Ctrl(); \
         class Sub extends WaveSoundBuffer { \
             function Sub(c) { super.WaveSoundBuffer(c); } \
             function onLabel(name) { got.add('sub:' + name); super.onLabel(name); } \
         } \
         var a = new Sub(ctrl); \
         a.onLabel('mid');",
    );
    assert_eq!(env.eval_int("got.count"), 1, "the subclass override ran");
    assert_eq!(env.eval_str("got[0]"), "sub:mid");
    assert_eq!(
        env.eval_int("native.count"),
        1,
        "super.onLabel must reach the native forwarder"
    );
    assert_eq!(env.eval_str("native[0]"), "onLabel");

    // An own property shadows everything — neither the subclass override nor
    // the native (nor the action owner) is reached.
    env.run(
        "var b = new Sub(ctrl); \
         b.onLabel = function(name) { got.add('own:' + name); }; \
         b.onLabel('mid');",
    );
    assert_eq!(env.eval_str("got[1]"), "own:mid");
    assert_eq!(
        env.eval_int("native.count"),
        1,
        "an own-property override must win over the native"
    );
}

/// Reference: the native `onStatusChanged`/`onFadeCompleted`/`onLabel`
/// members re-invoke the constructor's action owner as
/// `actionOwner.action(event)` with the reference event dictionary
/// `{type, target, status|name}` (`reference/cpp/core/sound/WaveIntf.cpp:1195-1241`
/// with `reference/cpp/core/base/EventIntf.h:208-234`,
/// built by `TVPCreateEventObject` — `reference/cpp/core/base/EventIntf.cpp:709-736`
/// — and named by `TVPActionName == "action"`, `EventIntf.cpp:740`). Without
/// an action owner the `if(obj.Object)` guard skips the whole body
/// (`WaveIntf.cpp:1197,1213,1227`).
#[test]
fn native_event_members_forward_to_the_action_owner() {
    let _lock = vm_lock();
    let env = Env::fixture(None);

    env.run(
        "var type = ''; var name = ''; var status = ''; var isSelf = 0; \
         class Ctrl { \
             function Ctrl() {} \
             function action(ev) { \
                 type = ev.type; \
                 name = ev.name; \
                 status = ev.status; \
                 isSelf = (ev.target === s) ? 1 : 0; \
             } \
         } \
         var ctrl = new Ctrl(); \
         var s = new WaveSoundBuffer(ctrl);",
    );

    // Direct member call: the argument lands in the event dictionary.
    env.run("s.onLabel('mid');");
    assert_eq!(env.eval_str("type"), "onLabel");
    assert_eq!(env.eval_str("name"), "mid");
    assert!(
        matches!(env.eval_ok("status"), TjsValue::Void),
        "the onLabel event carries no `status` member"
    );
    assert_eq!(env.eval_int("isSelf"), 1, "target is the buffer object");

    env.run("s.onStatusChanged('play');");
    assert_eq!(env.eval_str("type"), "onStatusChanged");
    assert_eq!(env.eval_str("status"), "play");
    assert!(
        matches!(env.eval_ok("name"), TjsValue::Void),
        "the onStatusChanged event carries no `name` member"
    );
    assert_eq!(env.eval_str("type"), "onStatusChanged");

    env.run("s.onFadeCompleted();");
    assert_eq!(env.eval_str("type"), "onFadeCompleted");

    // And the poll reaches the native through the object, so an object
    // *without* an override still delivers `open`'s status to the owner.
    env.run("var s2 = new WaveSoundBuffer(ctrl); s2.open('tone.wav');");
    assert_eq!(env.eval_str("type"), "onStatusChanged");
    assert_eq!(env.eval_str("status"), "stop");
}

/// Reference: `TVP_ACTION_INVOKE_BEGIN(argnum, ...)` checks
/// `numparams < argnum` **inside** the `if(obj.Object)` guard
/// (`reference/cpp/core/base/EventIntf.h:208-215`, guard at
/// `WaveIntf.cpp:1197`): with an action owner a missing `status`/`name`
/// argument raises `TJS_E_BADPARAMCOUNT`, without one the same call succeeds
/// as a silent no-op. `onFadeCompleted` takes zero arguments, so it never
/// checks (`WaveIntf.cpp:1210-1222`).
#[test]
fn event_members_check_arguments_only_with_an_action_owner() {
    let _lock = vm_lock();
    let env = Env::fixture(None);

    env.run(
        "class Ctrl { function Ctrl() {} function action(ev) {} } \
         var ctrl = new Ctrl(); \
         var withOwner = new WaveSoundBuffer(ctrl); \
         var withoutOwner = new WaveSoundBuffer();",
    );

    for script in ["withOwner.onLabel();", "withOwner.onStatusChanged();"] {
        let message = env
            .try_run(script)
            .expect_err(&format!("{script} must raise (EventIntf.h:213)"));
        println!("observed {script} error (matches the reference): {message}");
    }
    // No argument requirement for the zero-argument event.
    env.run("withOwner.onFadeCompleted();");
    // Without an owner the guard short-circuits before the check.
    env.run("withoutOwner.onLabel(); withoutOwner.onStatusChanged();");
}

// ---------------------------------------------------------------------------
// 7. Labels
// ---------------------------------------------------------------------------

/// Reference: `SetStatus` first cancels the source's undelivered events
/// (`TVPCancelSourceEvents(Owner)`,
/// `reference/cpp/core/sound/SoundBufferBaseIntf.cpp:81-83`), and the label
/// events are posted with the same `Owner` as source
/// (`reference/cpp/core/sound/WaveIntf.cpp:897-906`, whose comment says the
/// queued label “is to be erased when the SetStatus is called”). A label
/// crossed during the same synchronous drain as the `play` status is
/// therefore cancelled — and, in the port, never surfaces later either,
/// because the label cursor advanced past it.
#[test]
fn label_events_are_dropped_by_a_status_change() {
    let _lock = vm_lock();
    let env = Env::fixture(Some(SLI_ZERO));

    env.run(
        "var labels = []; var statuses = []; \
         var s = new WaveSoundBuffer(); \
         s.onLabel = function(name) { labels.add(name); }; \
         s.onStatusChanged = function(st) { statuses.add(st); }; \
         s.open('tone.wav'); \
         s.play();",
    );
    env.poll();
    env.poll_for(150, 40);
    assert_eq!(
        env.eval_int("labels.count"),
        0,
        "a label at the playhead start is cancelled by SetStatus(ssPlay)"
    );
    // The status event is still delivered: only the queued label was
    // cancelled (`WaveImpl.cpp:897-906`).
    assert_eq!(env.eval_int("statuses.count"), 2);
    assert_eq!(env.eval_str("statuses[0]"), "stop");
    assert_eq!(env.eval_str("statuses[1]"), "play");
}

/// Reference: `GetWaveLabelsObjectNoAddRef` builds the dictionary **once**
/// and caches it (`reference/cpp/core/sound/WaveIntf.cpp:1039-1043`), so
/// `s.labels === s.labels`; a successful `.sli` read calls
/// `RecreateWaveLabelsObject` (`WaveImpl.cpp:3083`,
/// `WaveIntf.cpp:910-918`), which *invalidates* the old dictionary (scripts
/// still holding it observe the invalidation) and drops the cache; without a
/// side-car the cache is untouched, so stale labels keep being reported. Each
/// entry carries `name`, `samplePosition` (granules) and `position`
/// (milliseconds, `WaveIntf.cpp:1064-1075`).
#[test]
fn labels_dictionary_is_cached_and_rebuilt_like_the_reference() {
    let _lock = vm_lock();
    let env = Env::fixture(Some(SLI_MID));

    env.run("var s = new WaveSoundBuffer(); var before = s.labels;");
    assert_eq!(
        env.eval_int("s.labels === s.labels"),
        1,
        "the dictionary is cached (WaveIntf.cpp:1041-1043)"
    );
    // A missing dictionary member is observed through `typeof`, which maps
    // `TJS_E_MEMBERNOTFOUND` to the string "undefined"
    // (`crates/tjs/cpp/tjs2/tjsInterCodeExec.cpp:1906-1916`); a bare
    // `undefined` is not a TJS keyword and is itself a member lookup that
    // raises.
    assert_eq!(
        env.eval_str("typeof before.mid"),
        "undefined",
        "an empty pre-open dictionary has no `mid` entry"
    );

    env.run("s.open('tone.wav'); var after = s.labels;");
    assert_eq!(
        env.eval_int("after === before"),
        0,
        "RecreateWaveLabelsObject replaced the dictionary (WaveImpl.cpp:3083)"
    );
    assert_eq!(env.eval_int("s.labels === after"), 1);
    assert_eq!(env.eval_int("after.mid.samplePosition"), 8820);
    assert_eq!(
        env.eval_int("after.mid.position"),
        200,
        "position is milliseconds (WaveIntf.cpp:1072-1074)"
    );
    assert_eq!(env.eval_str("after.mid.name"), "mid");
    if let Err(error) = env.eval("before.mid") {
        println!("observed invalidated old dictionary: {error}");
    } else {
        panic!(
            "the reference invalidates the previous labels dictionary \
             (WaveIntf.cpp:912-914)"
        );
    }

    // Without a side-car the cache is untouched — the stale labels persist,
    // exactly like the reference's cached dictionary.
    env.run("s.open('other.wav'); var stale = s.labels;");
    assert_eq!(env.eval_int("stale === after"), 1);
    assert_eq!(env.eval_int("stale.mid.samplePosition"), 8820);
}

// ---------------------------------------------------------------------------
// 8. Loop flags (`WaveFlags`)
// ---------------------------------------------------------------------------

/// Reference: the loop flags live in the `tTVPWaveLoopManager`, which `Open`
/// creates fresh (`reference/cpp/core/sound/win32/WaveImpl.cpp:3047-3051`,
/// `Clear()` deletes it, `WaveImpl.cpp:2358-2359`) and which does not exist
/// before the first open (`WaveImpl.cpp:1904`). While it is null the flag
/// properties read `0` and drop writes (`reference/cpp/core/sound/WaveIntf.cpp:1720-1745`,
/// the `if(manager)` guards at 1726 and 1740), values clamp to
/// `0..=9999` (`reference/cpp/core/sound/WaveLoopManager.cpp:200-209`,
/// `WaveLoopManager.h:21-23`) and `reset()` clears everything
/// (`WaveIntf.cpp:1688-1705`).
#[test]
fn flags_exist_only_while_open_and_reset_on_each_open() {
    let _lock = vm_lock();
    let env = Env::fixture(None);

    env.run("var s = new WaveSoundBuffer();");
    // No loop manager yet: reads yield 0, writes are dropped.
    env.run("s.flags[3] = 7;");
    assert_eq!(env.eval_int("s.flags[3]"), 0);

    env.run("s.open('tone.wav');");
    assert_eq!(
        env.eval_int("s.flags[3]"),
        0,
        "Open starts a fresh loop manager (WaveImpl.cpp:3047)"
    );

    env.run("s.flags[3] = 7;");
    assert_eq!(env.eval_int("s.flags[3]"), 7);
    env.run("s.flags[0] = 100000;");
    assert_eq!(env.eval_int("s.flags[0]"), 9999, "TVP_WL_MAX_FLAG_VALUE");
    env.run("s.flags[1] = -5;");
    assert_eq!(env.eval_int("s.flags[1]"), 0, "clamped at zero");

    env.run("s.flags.reset();");
    assert_eq!(env.eval_int("s.flags[3]"), 0);

    // A second open wipes the flags again (new manager).
    env.run("s.flags[3] = 7; s.open('other.wav');");
    assert_eq!(
        env.eval_int("s.flags[3]"),
        0,
        "every Open replaces the loop manager (WaveImpl.cpp:3047-3051)"
    );
}

/// Reference: `WaveFlags` exposes `count`/`N` = 16
/// (`reference/cpp/core/sound/WaveIntf.cpp:1707-1718`,
/// `WaveLoopManager.h:21`), `reset()` (1688-1705) and the numeric members
/// 0..15; its constructor requires one argument (`WaveIntf.cpp:1644-1645`)
/// and the proxy is built once per buffer and cached
/// (`WaveIntf.cpp:1026-1037`).
#[test]
fn wave_flags_surface_matches_the_reference() {
    let _lock = vm_lock();
    let env = Env::fixture(None);
    env.run("var s = new WaveSoundBuffer();");

    assert_eq!(env.eval_int("s.flags.count"), 16);
    assert_eq!(env.eval_int("s.flags.N"), 16);
    assert_eq!(env.eval_int("s.flags === s.flags"), 1);

    let message = env.error("new WaveFlags()");
    println!("observed `new WaveFlags()` error (matches the reference): {message}");

    env.run("s.open('tone.wav');");
    env.run("s.flags[15] = 42;");
    assert_eq!(env.eval_int("s.flags[15]"), 42);
    env.run("s.flags.reset();");
    assert_eq!(env.eval_int("s.flags[15]"), 0);
}

// ---------------------------------------------------------------------------
// 9. `filters` and the PhaseVocoder
// ---------------------------------------------------------------------------

/// Reference: `Filters` is created once in the native constructor
/// (`reference/cpp/core/sound/WaveIntf.cpp:866-869`) and handed out with
/// `GetFiltersNoAddRef` (`WaveIntf.h:214`,
/// getter `WaveIntf.cpp:1578-1590`), so it is the same array for the
/// object's lifetime — `open` never replaces it (`WaveImpl.cpp:3036-3088`
/// does not touch `Filters`). Only `Invalidate` releases it
/// (`WaveIntf.cpp:890-892`).
#[test]
fn filters_array_is_stable_across_opens() {
    let _lock = vm_lock();
    let env = Env::fixture(None);

    env.run("var s = new WaveSoundBuffer(); var f = s.filters;");
    assert_eq!(env.eval_int("s.filters === f"), 1);
    env.run("s.open('tone.wav');");
    assert_eq!(
        env.eval_int("s.filters === f"),
        1,
        "open keeps the same array object"
    );
    assert_eq!(env.eval_int("f.length"), 0);
}

/// Reference: the `PhaseVocoder` constructor defaults are `window = 4096`,
/// `overlap = 0`, `pitch = 1.0`, `time = 1.0`
/// (`reference/cpp/core/sound/PhaseVocoderFilter.cpp:140-145`);
/// `window` only accepts the powers of two 64..32768
/// (`PhaseVocoderFilter.cpp:170-188`, message at
/// `reference/cpp/core/base/MsgIntfInc.h:126-127`) and `overlap` only
/// `0, 2, 4, 8, 16, 32` (`PhaseVocoderFilter.cpp:191-205`,
/// `MsgIntfInc.h:128-129`), each throwing *before* the value is stored;
/// `pitch`/`time` are reals (`PhaseVocoderFilter.cpp:91-125`) and
/// `interface` is a read-only integer (`PhaseVocoderFilter.cpp:43-52`).
#[test]
fn phase_vocoder_surface_matches_the_reference() {
    let _lock = vm_lock();
    let env = Env::fixture(None);
    env.run("var pv = new WaveSoundBuffer.PhaseVocoder();");

    assert_eq!(env.eval_int("pv.window"), 4096);
    assert_eq!(env.eval_int("pv.overlap"), 0);
    assert!((env.eval_real("pv.pitch") - 1.0).abs() < 1e-9);
    assert!((env.eval_real("pv.time") - 1.0).abs() < 1e-9);
    assert_ne!(env.eval_int("pv.interface"), 0);

    env.run("pv.window = 2048; pv.overlap = 8; pv.pitch = 1.5; pv.time = 2.0;");
    assert_eq!(env.eval_int("pv.window"), 2048);
    assert_eq!(env.eval_int("pv.overlap"), 8);
    assert!((env.eval_real("pv.pitch") - 1.5).abs() < 1e-9);
    assert!((env.eval_real("pv.time") - 2.0).abs() < 1e-9);

    // The reference messages are reproduced verbatim, and a rejected value
    // leaves the old one in place (the throw precedes the assignment).
    let message = env.error("pv.window = 100;");
    assert!(
        message.contains("Invalid window size must be in 64 to 32768"),
        "unexpected message: {message}"
    );
    assert_eq!(env.eval_int("pv.window"), 2048);
    let message = env.error("pv.overlap = 3;");
    assert!(
        message.contains("Invalid overlap count must be in 2 to 32"),
        "unexpected message: {message}"
    );
    assert_eq!(env.eval_int("pv.overlap"), 8);
}

// ---------------------------------------------------------------------------
// 10. Fading
// ---------------------------------------------------------------------------

/// Reference: `Fade` only manipulates the fade state — it never checks the
/// decoder and never starts playback (`reference/cpp/core/sound/SoundBufferBaseIntf.cpp:149-171`)
/// — and the 60 ms beat ramps the volume and completes the fade even on an
/// unloaded buffer (`SoundBufferBaseIntf.cpp:118-146`, completion via
/// `StopFade(true, true)` at 174-193, which posts `onFadeCompleted`).
#[test]
fn fade_on_an_unloaded_buffer_still_ramps_and_completes() {
    let _lock = vm_lock();
    let env = Env::fixture(None);

    env.run(
        "var fades = 0; \
         var s = new WaveSoundBuffer(); \
         s.onFadeCompleted = function() { fades++; }; \
         s.fade(0, 300);",
    );
    assert_eq!(env.eval_int("s.volume"), 100_000, "no beat has elapsed yet");

    env.poll_for(150, 40);
    let volume = env.eval_int("s.volume");
    assert!(
        volume > 0 && volume < 100_000,
        "the beat ramp must be midway after ~150 ms; observed {volume}"
    );
    assert_eq!(env.eval_str("s.status"), "unload", "fade changes no status");

    env.poll_for(300, 40);
    assert_eq!(env.eval_int("s.volume"), 0, "the ramp reaches the target");
    assert_eq!(env.eval_int("fades"), 1, "onFadeCompleted fired once");
    assert_eq!(env.eval_str("s.status"), "unload");
}

/// Reference: `Fade` never calls `Play()`/`StartPlay` — it only sets
/// `TargetVolume`/`DeltaVolume`/`FadeCount`/`BlankLeft`
/// (`reference/cpp/core/sound/SoundBufferBaseIntf.cpp:159-171`), so a loaded
/// but stopped buffer keeps its `stop` status and position while the volume
/// ramps (the beat runs from `TimerBeatHandler`, `SoundBufferBaseIntf.cpp:118-146`).
#[test]
fn fade_never_starts_playback() {
    let _lock = vm_lock();
    let env = Env::fixture(None);

    env.run(
        "var fades = 0; \
         var s = new WaveSoundBuffer(); \
         s.onFadeCompleted = function() { fades++; }; \
         s.open('tone.wav'); \
         s.fade(0, 300);",
    );
    env.poll_for(450, 40);

    assert_eq!(
        env.eval_ok("s.status"),
        TjsValue::String("stop".into()),
        "Fade must not start playback (SoundBufferBaseIntf.cpp:149-171)"
    );
    assert_eq!(env.eval_int("s.volume"), 0, "the volume still ramped");
    assert_eq!(env.eval_int("fades"), 1);
}

/// Reference: `Fade` computes `FadeCount = time / 60` and, when that is `0`
/// with no blank time, completes *inside the call* —
/// `if(FadeCount == 0 && BlankLeft == 0) StopFade(false, true);`
/// (`reference/cpp/core/sound/SoundBufferBaseIntf.cpp:168-170`): the volume
/// jumps to the target and `onFadeCompleted` is posted (`TVP_EPT_IMMEDIATE`)
/// before `fade` returns.
#[test]
fn time_below_one_beat_completes_the_fade_instantly() {
    let _lock = vm_lock();
    let env = Env::fixture(None);

    env.run(
        "var fades = 0; \
         var s = new WaveSoundBuffer(); \
         s.onFadeCompleted = function() { fades++; }; \
         s.fade(0, 50);",
    );
    assert_eq!(
        env.eval_int("s.volume"),
        0,
        "time < 60 ms completes inside Fade (SoundBufferBaseIntf.cpp:168-170)"
    );
    assert_eq!(
        env.eval_int("fades"),
        1,
        "the completion is delivered immediately"
    );
    env.poll();
    assert_eq!(env.eval_int("fades"), 1, "and only once");
}

/// Reference: `stopFade` runs `StopFade(false, true)`
/// (`reference/cpp/core/sound/WaveIntf.cpp:1161-1169`): **only while
/// fading** (`if(InFading)`, `SoundBufferBaseIntf.cpp:180`) it jumps to
/// `TargetVolume` (`SetVolume(TargetVolume)`, 183-184) and posts
/// `onFadeCompleted` (186-190). A second `stopFade` with no fade left is a
/// strict no-op — no event, no write.
#[test]
fn stop_fade_jumps_to_the_target_and_reports_only_while_fading() {
    let _lock = vm_lock();
    let env = Env::fixture(None);

    env.run(
        "var fades = 0; \
         var s = new WaveSoundBuffer(); \
         s.onFadeCompleted = function() { fades++; };",
    );
    // No fade running: no event, no volume change.
    env.run("s.volume = 50000; s.stopFade();");
    assert_eq!(env.eval_int("s.volume"), 50_000);
    assert_eq!(env.eval_int("fades"), 0);

    env.run("s.fade(0, 5000); s.stopFade();");
    assert_eq!(
        env.eval_int("s.volume"),
        0,
        "StopFade(setvolume) jumps to TargetVolume (SoundBufferBaseIntf.cpp:183-184)"
    );
    assert_eq!(env.eval_int("fades"), 1);
    env.run("s.stopFade();");
    assert_eq!(
        env.eval_int("fades"),
        1,
        "no fade left: `if(InFading)` keeps it silent (SoundBufferBaseIntf.cpp:180)"
    );
    assert_eq!(env.eval_int("s.volume"), 0);
}

/// Reference: starting a fade while one is running first stops the old fade
/// with `StopFade(false, false)` — which **posts `onFadeCompleted` for the
/// interrupted fade** while leaving the volume alone
/// (`reference/cpp/core/sound/SoundBufferBaseIntf.cpp:160-162`, event at
/// 186-190) — before installing the new parameters.
#[test]
fn an_interrupted_fade_reports_completion() {
    let _lock = vm_lock();
    let env = Env::fixture(None);

    env.run(
        "var fades = 0; \
         var s = new WaveSoundBuffer(); \
         s.onFadeCompleted = function() { fades++; }; \
         s.fade(0, 5000);",
    );
    assert_eq!(env.eval_int("fades"), 0);
    env.run("s.fade(50000, 300);");
    assert_eq!(
        env.eval_int("fades"),
        1,
        "the interrupted fade completes immediately (SoundBufferBaseIntf.cpp:160-162)"
    );
    assert_eq!(
        env.eval_int("s.volume"),
        100_000,
        "StopFade(false, false) leaves the volume alone (settargetvol = false)"
    );

    env.poll_for(450, 40);
    assert_eq!(env.eval_int("fades"), 2, "the second fade completes too");
    assert_eq!(env.eval_int("s.volume"), 50_000);
}

/// Reference: a script volume write goes through `SetVolume`, which never
/// touches the fade state (`reference/cpp/core/sound/win32/WaveImpl.cpp:3219-3231`);
/// the beat simply continues from the written value
/// (`v = GetVolume() + DeltaVolume`, `SoundBufferBaseIntf.cpp:130-137`) and
/// the fade still ends on the target.
#[test]
fn a_volume_write_during_a_fade_keeps_fading() {
    let _lock = vm_lock();
    let env = Env::fixture(None);

    env.run(
        "var fades = 0; \
         var s = new WaveSoundBuffer(); \
         s.onFadeCompleted = function() { fades++; }; \
         s.open('tone.wav'); s.play(); \
         s.fade(0, 600);",
    );
    env.run("s.volume = 80000;");
    assert_eq!(env.eval_int("s.volume"), 80_000, "the write lands");

    env.poll_for(750, 40);
    assert_eq!(
        env.eval_int("fades"),
        1,
        "the fade was not cancelled by the write"
    );
    assert_eq!(
        env.eval_int("s.volume"),
        0,
        "the ramp still ends on TargetVolume"
    );
}

/// Reference: `Stop()` never touches the fade state — it only stops the
/// device and rewinds (`reference/cpp/core/sound/win32/WaveImpl.cpp:2979-2995`);
/// the beat keeps ramping the volume and completes the fade afterwards.
#[test]
fn a_fade_survives_stop() {
    let _lock = vm_lock();
    let env = Env::fixture(None);

    env.run(
        "var fades = 0; \
         var s = new WaveSoundBuffer(); \
         s.onFadeCompleted = function() { fades++; }; \
         s.open('tone.wav'); s.play(); \
         s.fade(0, 600); \
         s.stop();",
    );
    assert_eq!(env.eval_int("fades"), 0);

    env.poll_for(250, 40);
    let volume = env.eval_int("s.volume");
    assert!(
        volume < 100_000,
        "the fade must keep ramping while stopped; observed {volume}"
    );
    assert_eq!(env.eval_ok("s.status"), TjsValue::String("stop".into()));

    env.poll_for(450, 40);
    assert_eq!(env.eval_int("s.volume"), 0, "the fade completed");
    assert_eq!(env.eval_int("fades"), 1);
    assert_eq!(env.eval_ok("s.status"), TjsValue::String("stop".into()));
}

// ---------------------------------------------------------------------------
// 11. `open` failures and side effects
// ---------------------------------------------------------------------------

/// Reference: `Open` starts with `Clear()` — stop, drop the loop manager,
/// `Paused = false`, `SetStatus(ssUnload)`
/// (`reference/cpp/core/sound/win32/WaveImpl.cpp:3041`, `2352-2370`) — and a
/// later failure leaves the buffer unloaded while `InputFormat`/`Frequency`,
/// assigned *before* the `.sli` parse (`WaveImpl.cpp:3054-3056`), survive the
/// `Clear()` in the failure path (`WaveImpl.cpp:3079`). A decoder-creation
/// failure happens before that assignment
/// (`WaveImpl.cpp:3043-3044` + `WaveIntf.cpp:750-767`), so it keeps the
/// previous format.
#[test]
fn failed_opens_leave_the_buffer_unloaded_but_keep_the_format() {
    let _lock = vm_lock();
    let env = Env::fixture(Some(SLI_MID));

    env.run(
        "var got = []; \
         var s = new WaveSoundBuffer(); \
         s.onStatusChanged = function(st) { got.add(st); };",
    );

    // (a) A corrupt side-car after a successful decode: format assigned,
    // status unloaded, only the `Clear()` event fires.
    let message = env.error("s.open('bad.wav')");
    assert!(message.contains(".sli"), "unexpected message: {message}");
    assert_eq!(env.eval_ok("s.status"), TjsValue::String("unload".into()));
    assert_eq!(
        env.eval_int("s.channels"),
        1,
        "InputFormat survived (WaveImpl.cpp:3054-3055 + 2352-2370)"
    );
    assert_eq!(env.eval_int("s.totalTime"), 1000);
    assert_eq!(env.eval_int("s.frequency"), 44_100);
    assert_eq!(
        env.eval_int("got.count"),
        0,
        "a never-loaded buffer raises nothing in Clear()"
    );

    // (b) The successful open, then a decode failure: the *previous* format
    // is kept and `Clear()` reports `unload`.
    env.run("s.open('tone.wav');");
    assert_eq!(env.eval_int("got.count"), 1);
    assert_eq!(env.eval_str("got[0]"), "stop");
    let message = env.error(&format!("s.open('{UNDECODABLE}')"));
    assert!(
        message.contains("cannot decode"),
        "unexpected message: {message}"
    );
    assert_eq!(env.eval_ok("s.status"), TjsValue::String("unload".into()));
    assert_eq!(env.eval_int("s.channels"), 1, "InputFormat unchanged");
    assert_eq!(env.eval_int("s.frequency"), 44_100, "Frequency unchanged");
    assert_eq!(
        env.eval_int("got.count"),
        2,
        "Clear() raises the `unload` status (WaveImpl.cpp:2369)"
    );
    assert_eq!(env.eval_str("got[1]"), "unload");
    assert_eq!(
        env.eval_int("s.position"),
        0,
        "no decoder → GetPosition is 0 (WaveImpl.cpp:3148-3151)"
    );
}

/// Reference: without a storage the reference throws from
/// `tTVPStreamHolder` while creating the decoder
/// (`reference/cpp/core/sound/WaveIntf.cpp:537-546` reads the storage inside
/// `Create`, `WaveIntf.cpp:750-767`) — the port reports its typed "no
/// storage" error instead of silently succeeding, and the buffer stays
/// unloaded.
#[test]
fn open_without_a_storage_reports_a_typed_error() {
    let _lock = vm_lock();
    let env = Env::without_storage();
    env.run("var s = new WaveSoundBuffer();");

    let message = env.error("s.open('tone.wav')");
    assert!(
        message.contains("no storage is attached"),
        "unexpected message: {message}"
    );
    assert_eq!(env.eval_ok("s.status"), TjsValue::String("unload".into()));
}

// ---------------------------------------------------------------------------
// 12. Teardown
// ---------------------------------------------------------------------------

/// Reference: `Invalidate` runs the whole teardown chain —
/// `tTJSNI_WaveSoundBuffer::Invalidate` → `Clear()` + `DestroySoundBuffer()`
/// (`reference/cpp/core/sound/win32/WaveImpl.cpp:1960-1974`), the base's
/// `Filters->Release()` and labels invalidation
/// (`reference/cpp/core/sound/WaveIntf.cpp:885-894`), and
/// `CanDeliverEvents = false; Owner = nullptr`
/// (`SoundBufferBaseIntf.cpp:46-55`) so **no event is ever dispatched
/// afterwards**. Member access on the invalidated object raises
/// ("The object is already invalidated",
/// `reference/cpp/core/tjs2/tjsError.h:298`,
/// `reference/cpp/core/tjs2/tjsError.cpp:279`).
#[test]
fn invalidate_stops_playback_and_dispatches_nothing() {
    let _lock = vm_lock();
    let env = Env::fixture(None);

    env.run(
        "var hits = 0; \
         var s = new WaveSoundBuffer(); \
         s.onStatusChanged = function(st) { hits++; }; \
         s.open('tone.wav'); s.play();",
    );
    assert_eq!(
        env.eval_int("hits"),
        2,
        "open raises stop and play raises play (WaveImpl.cpp:3086, 2976-2977)"
    );
    env.poll();
    assert!(
        env.peak() > 0.1,
        "the buffer must be audibly playing before the teardown"
    );

    env.run("invalidate s;");
    assert_eq!(
        env.peak(),
        0.0,
        "DestroySoundBuffer must stop playback (WaveImpl.cpp:1968)"
    );

    let hits = env.eval_int("hits");
    env.poll_for(200, 40);
    assert_eq!(
        env.eval_int("hits"),
        hits,
        "no events after CanDeliverEvents = false (SoundBufferBaseIntf.cpp:48-49)"
    );

    for script in ["s.status", "s.play()", "s.open('tone.wav')"] {
        let message = env
            .eval(script)
            .expect_err(&format!("{script} must raise on an invalidated object"));
        println!("observed {script} error (matches the reference): {message}");
    }
}

// ---------------------------------------------------------------------------
// 13. Plain callability (no arguments, unloaded buffer)
// ---------------------------------------------------------------------------

/// Reference: `play`/`stop` take no arguments (`WaveIntf.cpp:1121-1139`) and
/// are silent on an unloaded buffer — `Play` returns before touching anything
/// when `!Decoder` (`WaveImpl.cpp:2958-2959`) and `Stop` skips the status
/// change while `ssUnload` (`WaveImpl.cpp:2989-2990`); `stopFade` with no
/// fade is a no-op (`SoundBufferBaseIntf.cpp:180`). The event forwarders with
/// no action owner return `TJS_S_OK` without doing anything
/// (`WaveIntf.cpp:1197`).
#[test]
fn methods_are_callable_on_an_unloaded_buffer() {
    let _lock = vm_lock();
    let env = Env::fixture(None);
    env.run("var s = new WaveSoundBuffer();");

    assert_eq!(env.eval_ok("s.play()"), TjsValue::Void);
    assert_eq!(env.eval_ok("s.stop()"), TjsValue::Void);
    assert_eq!(env.eval_ok("s.stopFade()"), TjsValue::Void);
    assert_eq!(env.eval_ok("s.onStatusChanged('play')"), TjsValue::Void);
    assert_eq!(env.eval_ok("s.onFadeCompleted()"), TjsValue::Void);
    assert_eq!(env.eval_ok("s.onLabel('mid')"), TjsValue::Void);
    assert_eq!(env.eval_ok("s.status"), TjsValue::String("unload".into()));
}
