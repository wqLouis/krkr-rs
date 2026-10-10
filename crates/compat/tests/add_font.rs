//! Reference-behaviour tests for the `add_font` plugin's `System.addFont`.
//!
//! The reference (`reference/cpp/plugins/addFont.cpp:17-31`) attaches one
//! static member to `System`: it resolves the storage name with
//! `TVPGetPlacedPath`, returns `void` when the name does not resolve,
//! `TJS_E_BADPARAMCOUNT` when the argument is missing, and the
//! `TVPEnumFontsProc` face count (`Integer`, `0` when the file is not a
//! usable font) when it does. The tests verify exactly that result contract
//! against a real font (and a garbage file) mounted in a temp storage; a
//! missing host service is a typed error (D11), never a silent zero.
//!
//! The tests serialize on the shared VM lock because the engine → context
//! binding is process-wide and single.

mod common;

use std::path::{Path, PathBuf};

use common::vm_lock;
use compat::CompatHost;
use engine::Storage;
use engine::text::TextEngine;
use fs::FileSystem;
use tjs::{Tjs2Engine, TjsValue};

/// System fonts tried by the tests; a missing/unusable font skips the
/// font-dependent case rather than failing.
const TEST_FONT_PATHS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
];

/// The number of faces the L2 text engine can load from `bytes`.
fn face_count(bytes: &[u8]) -> i64 {
    let mut engine = TextEngine::new();
    let mut count = 0u32;
    while engine.add_font(bytes, count).is_ok() {
        count += 1;
    }
    i64::from(count)
}

/// Find a system font the L2 text engine can actually shape and return its
/// file name plus bytes.
fn test_font() -> Option<(String, Vec<u8>)> {
    for candidate in TEST_FONT_PATHS {
        let path = Path::new(candidate);
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        if TextEngine::new().add_font(&bytes, 0).is_err() {
            continue;
        }
        let name = path.file_name()?.to_string_lossy().into_owned();
        return Some((name, bytes));
    }
    None
}

/// A unique temporary game directory, removed on drop.
struct GameDir(PathBuf);

impl GameDir {
    fn new() -> Self {
        let mut path = std::env::temp_dir();
        let unique = format!(
            "krkr-compat-add-font-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        );
        path.push(unique);
        std::fs::create_dir_all(&path).expect("create temp game dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self.0.join(name), bytes).expect("write fixture");
    }
}

impl Drop for GameDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A bound VM with an optional mounted storage attached to the services.
///
/// Fields drop in declaration order, so `_host` (which owns the process-wide
/// binding) drops before the engine/storage its raw pointers reference.
struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    /// Kept alive for the host's raw pointer; `None` means no font source.
    _storage: Option<Box<Storage>>,
    /// The temp dir backing `_storage`, if any; removed on drop.
    _dir: Option<GameDir>,
}

impl Env {
    fn new(storage: Option<Storage>, dir: Option<GameDir>) -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut storage = storage.map(Box::new);
        let mut host = CompatHost::new();
        // SAFETY: the boxed storage outlives the host (it is declared after
        // `_host`, so it drops later), matching the `attach_storage` contract.
        unsafe {
            if let Some(storage) = storage.as_mut() {
                host.services_mut()
                    .attach_storage(storage.as_mut() as *mut Storage);
            }
        }
        host.install(&engine).expect("install compat surface");
        Env {
            _host: host,
            engine,
            _storage: storage,
            _dir: dir,
        }
    }

    /// A bound env whose storage mounts a copy of a system font plus a file
    /// that is not a font. Returns the font file name and its face count.
    fn with_font() -> Option<(Env, String, i64)> {
        let (name, bytes) = test_font()?;
        let count = face_count(&bytes);
        let dir = GameDir::new();
        dir.write(&name, &bytes);
        dir.write("garbage.ttf", b"this is definitely not a font");
        let storage = Storage::mount_archives(
            FileSystem::host(dir.path()),
            dir.path().to_path_buf(),
            std::iter::empty::<&str>(),
        )
        .expect("mount font directory");
        Some((Env::new(Some(storage), Some(dir)), name, count))
    }

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, "add-font-test")
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

    /// The script `typeof` of an expression.
    fn eval_typeof(&self, expression: &str) -> String {
        match self.eval_ok(&format!("typeof ({expression})")) {
            TjsValue::String(value) => value,
            other => panic!("typeof {expression:?} returned {other:?}"),
        }
    }

    /// Evaluate an expression that must raise, returning the message.
    fn error(&self, expression: &str) -> String {
        self.eval(expression)
            .expect_err(&format!("{expression:?} should have raised"))
    }
}

// ---------------------------------------------------------------------------
// Result contract
// ---------------------------------------------------------------------------

/// Reference: a resolvable name runs `TVPEnumFontsProc(filename)` and stores
/// `*result = (int)ret` — the number of faces loaded from the file, an
/// **Integer** (`addFont.cpp:22-28`; `FontImpl.cpp:142-156` returns the
/// `TVPInternalEnumFonts` face count, `FontImpl.cpp:54-70`). Re-adding the
/// same source goes through the same path and reports the same count (the
/// reference's `TVPFontNames.Add` replaces the entry,
/// `FontImpl.cpp:121-124`).
#[test]
fn add_font_registers_every_face_and_returns_the_count() {
    let _lock = vm_lock();
    let Some((env, name, count)) = Env::with_font() else {
        return;
    };
    assert!(
        count >= 1,
        "a test font should expose at least one face, got {count}"
    );
    assert_eq!(env.eval_int(&format!("System.addFont('{name}')")), count);
    // The result is the reference's `(int)` write — Integer, never Real.
    assert_eq!(
        env.eval_typeof(&format!("System.addFont('{name}')")),
        "Integer"
    );
    // Re-adding the same source reports the same count.
    assert_eq!(env.eval_int(&format!("System.addFont('{name}')")), count);
}

/// Reference: when `TVPGetPlacedPath` yields nothing (a name the storage
/// cannot resolve, or the empty string), `addFont` returns `TJS_S_OK`
/// **without touching the result** — the expression evaluates to `void`
/// (`addFont.cpp:22-30`).
#[test]
fn add_font_unresolvable_name_is_void() {
    let _lock = vm_lock();
    let Some((env, _, _)) = Env::with_font() else {
        return;
    };
    assert_eq!(
        env.eval_ok("System.addFont('does-not-exist.ttf')"),
        TjsValue::Void
    );
    assert_eq!(
        env.eval_typeof("System.addFont('does-not-exist.ttf')"),
        "void"
    );
    // The reference's placed-path lookup yields nothing for an empty name.
    assert_eq!(env.eval_ok("System.addFont('')"), TjsValue::Void);
}

/// Reference: a file that resolves but is not a usable font returns
/// `TVPEnumFontsProc`'s failure value `0` — an **Integer** zero, the
/// reference-defined "font registration failed" result, not `void`
/// (`addFont.cpp:24-26`, `FontImpl.cpp:142-156` with `FT_New_Memory_Face`
/// failing in `FontImpl.cpp:60-66`).
#[test]
fn add_font_unusable_file_is_zero() {
    let _lock = vm_lock();
    let Some((env, _, _)) = Env::with_font() else {
        return;
    };
    assert_eq!(env.eval_int("System.addFont('garbage.ttf')"), 0);
    assert_eq!(env.eval_typeof("System.addFont('garbage.ttf')"), "Integer");
}

// ---------------------------------------------------------------------------
// Errors and ignored parameters
// ---------------------------------------------------------------------------

/// Reference: `if(numparams < 1) return TJS_E_BADPARAMCOUNT;`
/// (`addFont.cpp:19-20`) — a call without the file name raises; it never
/// falls through to a silent result. The port raises its own typed message
/// (the text may differ, the raising must not).
#[test]
fn add_font_without_argument_is_a_typed_error() {
    let _lock = vm_lock();
    let env = Env::new(None, None);
    let message = env.error("System.addFont()");
    assert!(
        message.contains("requires a font file name"),
        "unexpected message: {message}"
    );
}

/// Reference: the file is read through the TVP storage
/// (`TVPGetPlacedPath`, `addFont.cpp:22`), which always exists in a running
/// KiriKiri. A host without an attached storage cannot honour that contract,
/// so the port reports a typed error (D11) instead of returning a fabricated
/// `0`/`void` — the brief's "never a silent zero".
#[test]
fn add_font_without_storage_is_a_typed_error() {
    let _lock = vm_lock();
    let env = Env::new(None, None);
    let message = env.error("System.addFont('default.ttf')");
    assert!(
        message.contains("no storage attached"),
        "unexpected message: {message}"
    );
}

/// Reference: `addFont` checks only `numparams < 1`; the documented second
/// parameter `extract` (`addFont.cpp:12-13,19-20`) is never read, so any
/// extra argument is accepted and ignored — the result is identical.
#[test]
fn add_font_ignores_the_extract_parameter() {
    let _lock = vm_lock();
    let Some((env, name, count)) = Env::with_font() else {
        return;
    };
    assert_eq!(
        env.eval_int(&format!("System.addFont('{name}', true)")),
        count
    );
    assert_eq!(
        env.eval_int(&format!("System.addFont('{name}', 12345)")),
        count
    );
}

/// Reference: `addFont` is attached with `TJS_STATICMEMBER`
/// (`addFont.cpp:35-37`), so it lives on the `System` class object only —
/// the instance-member enumeration skips statics
/// (`reference/cpp/core/tjs2/tjsNative.cpp:347`) and an instance therefore
/// cannot reach it.
#[test]
fn add_font_is_static_on_the_system_class() {
    let _lock = vm_lock();
    let Some((env, name, _)) = Env::with_font() else {
        return;
    };
    // The class member works and returns the reference's `(int)` write.
    assert_eq!(
        env.eval_typeof(&format!("System.addFont('{name}')")),
        "Integer"
    );
    // ... but a `System` instance must not carry it.
    let instance_probe = "(function(){ var s = new System(); return typeof s.addFont; })()";
    assert_eq!(
        env.eval_ok(instance_probe),
        TjsValue::String("undefined".to_string()),
        "System.addFont must not appear on an instance (addFont.cpp:35-37)"
    );
}
