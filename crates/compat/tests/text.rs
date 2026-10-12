//! End-to-end tests for KiriKiri script-text decoding through `Scripts`.
//!
//! `compat::text::decode_text` is crate-private, so these tests drive it
//! through `Scripts.execStorage` against a real [`engine::Storage`] mounted
//! over an on-disk [`fs::FileSystem`]. They cover the `FE FE` cipher and zlib
//! containers, UTF-8/UTF-16 BOMs, CP932 and the `Scripts.textEncoding`
//! override.
//!
//! Each test owns a unique temporary game directory (removed on drop) and
//! serializes on the shared VM lock, since the process-wide engine → context
//! binding is single.

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};

use common::vm_lock;
use compat::CompatHost;
use engine::Storage;
use fs::FileSystem;
use tjs::{Tjs2Engine, TjsValue};

/// A unique temporary game directory, removed on drop.
struct GameDir(PathBuf);

impl GameDir {
    fn new() -> Self {
        let mut path = std::env::temp_dir();
        let unique = format!(
            "krkr-compat-text-{}-{}",
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

    /// Write one fixture file (creating parent directories).
    fn write(&self, name: &str, bytes: &[u8]) {
        let path = self.0.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(path, bytes).expect("write fixture");
    }
}

impl Drop for GameDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A bound VM with a mounted storage attached to the compat services.
struct Env {
    // Drop order matters: the host removes the process-wide binding before
    // the boxed engine/storage it points at are freed.
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    _storage: Box<Storage>,
    _dir: GameDir,
}

impl Env {
    fn new(dir: GameDir) -> Env {
        let mut storage = Box::new(
            Storage::mount_archives(
                FileSystem::host(dir.path()),
                dir.path().to_path_buf(),
                std::iter::empty::<&str>(),
            )
            .expect("mount storage"),
        );
        // Boxed so the address stays stable when it is moved into `Env`.
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut host = CompatHost::new();
        // SAFETY: `storage` is boxed and outlives the binding, which `_host`
        // removes on drop.
        unsafe { host.services_mut().attach_storage(&mut *storage) };
        host.install(&engine).expect("install compat surface");
        Env {
            _host: host,
            engine,
            _storage: storage,
            _dir: dir,
        }
    }

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, "test")
            .map_err(|error| error.to_string())
    }

    fn eval_ok(&self, expression: &str) -> TjsValue {
        self.eval(expression)
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
    }

    fn error(&self, expression: &str) -> String {
        self.eval(expression)
            .expect_err(&format!("{expression:?} should have raised"))
    }
}

/// Encode `text` as CP932 (Windows-31J).
fn cp932(text: &str) -> Vec<u8> {
    let (bytes, _, had_errors) = encoding_rs::SHIFT_JIS.encode(text);
    assert!(!had_errors, "CP932 cannot encode {text:?}");
    bytes.into_owned()
}

/// Encrypt `text` as an `FE FE 00` legacy XOR container (writer side).
fn fefe_mode0(text: &str) -> Vec<u8> {
    let mut out = vec![0xFE, 0xFE, 0x00, 0xFF, 0xFE];
    for unit in text.encode_utf16() {
        let encrypted = if unit >= 0x20 {
            unit ^ (((unit & 0xfe) << 8) ^ 1)
        } else {
            unit
        };
        out.extend_from_slice(&encrypted.to_le_bytes());
    }
    out
}

/// Encrypt `text` as an `FE FE 01` adjacent-bit-swap container (writer side).
fn fefe_mode1(text: &str) -> Vec<u8> {
    let mut out = vec![0xFE, 0xFE, 0x01, 0xFF, 0xFE];
    for unit in text.encode_utf16() {
        let encrypted = ((unit & 0xaaaa) >> 1) | ((unit & 0x5555) << 1);
        out.extend_from_slice(&encrypted.to_le_bytes());
    }
    out
}

/// Compress `text` as an `FE FE 02` zlib container (writer side).
fn fefe_mode2(text: &str) -> Vec<u8> {
    let utf16: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&utf16).expect("write utf16");
    let compressed = encoder.finish().expect("finish zlib");
    let mut out = vec![0xFE, 0xFE, 0x02, 0xFF, 0xFE];
    out.extend_from_slice(&(compressed.len() as u64).to_le_bytes());
    out.extend_from_slice(&(utf16.len() as u64).to_le_bytes());
    out.extend_from_slice(&compressed);
    out
}

/// The script used to probe decoding: it returns a distinctive string.
fn probe_script() -> &'static str {
    "return \"日本語 OK\";"
}

/// A Japanese script long enough for charset detection to report Shift_JIS
/// (cp932). The short `probe_script` above is not detectable on its own; it is
/// only used on the BOM/container/override paths, which do not run detection.
fn detectable_japanese_script() -> &'static str {
    "// これは日本語のスクリプトです。文字コードの判定を確かめるためのテストです。\nreturn \"日本語のテストに成功しました\";"
}

#[test]
fn cp932_script_is_decoded() {
    let _lock = vm_lock();
    let dir = GameDir::new();
    dir.write("cp932.tjs", &cp932(detectable_japanese_script()));
    let env = Env::new(dir);
    assert_eq!(
        env.eval_ok("Scripts.execStorage('cp932.tjs')"),
        TjsValue::String("日本語のテストに成功しました".into())
    );
}

#[test]
fn utf8_and_utf16_boms_are_decoded() {
    let _lock = vm_lock();
    let dir = GameDir::new();

    let mut utf8 = vec![0xEF, 0xBB, 0xBF];
    utf8.extend_from_slice(probe_script().as_bytes());
    dir.write("utf8.tjs", &utf8);

    let mut utf16le = vec![0xFF, 0xFE];
    utf16le.extend(probe_script().encode_utf16().flat_map(u16::to_le_bytes));
    dir.write("utf16le.tjs", &utf16le);

    let mut utf16be = vec![0xFE, 0xFF];
    utf16be.extend(probe_script().encode_utf16().flat_map(u16::to_be_bytes));
    dir.write("utf16be.tjs", &utf16be);

    let env = Env::new(dir);
    for name in ["utf8.tjs", "utf16le.tjs", "utf16be.tjs"] {
        assert_eq!(
            env.eval_ok(&format!("Scripts.execStorage('{name}')")),
            TjsValue::String("日本語 OK".into()),
            "{name}"
        );
    }
}

#[test]
fn fefe_cipher_containers_are_decoded() {
    let _lock = vm_lock();
    let dir = GameDir::new();
    dir.write("mode0.tjs", &fefe_mode0(probe_script()));
    dir.write("mode1.tjs", &fefe_mode1(probe_script()));
    let env = Env::new(dir);
    for name in ["mode0.tjs", "mode1.tjs"] {
        assert_eq!(
            env.eval_ok(&format!("Scripts.execStorage('{name}')")),
            TjsValue::String("日本語 OK".into()),
            "{name}"
        );
    }
}

#[test]
fn fefe_zlib_container_is_decoded() {
    let _lock = vm_lock();
    let dir = GameDir::new();
    dir.write("zlib.tjs", &fefe_mode2(probe_script()));
    let env = Env::new(dir);
    assert_eq!(
        env.eval_ok("Scripts.execStorage('zlib.tjs')"),
        TjsValue::String("日本語 OK".into())
    );
}

#[test]
fn text_encoding_override_forces_the_decoder() {
    let _lock = vm_lock();
    let dir = GameDir::new();
    dir.write("cp932.tjs", &cp932(probe_script()));
    let env = Env::new(dir);

    // Forcing UTF-8 uses the override, not detection: the malformed CP932 bytes
    // are dropped (the reference's Boost.Locale `skip` policy) rather than the
    // script being detected as Shift_JIS or the load raising.
    env.eval_ok("Scripts.textEncoding = 'utf-8'");
    assert_eq!(
        env.eval_ok("Scripts.execStorage('cp932.tjs')"),
        TjsValue::String("{ OK".into()),
        "forcing UTF-8 must drop the malformed bytes, not fall back to detection"
    );

    // An unknown override is a typed error, not a silent auto-detect.
    env.eval_ok("Scripts.textEncoding = 'no-such-encoding'");
    let message = env.error("Scripts.execStorage('cp932.tjs')");
    assert!(
        message.contains("not supported"),
        "expected an unsupported-encoding error, got: {message}"
    );
}

#[test]
fn malformed_containers_are_typed_errors() {
    let _lock = vm_lock();
    let dir = GameDir::new();
    // `FE FE 02` with a header too short for the sizes.
    dir.write("short.tjs", &[0xFE, 0xFE, 0x02, 0xFF, 0xFE, 0x00]);
    // An unknown container mode.
    dir.write("mode9.tjs", &[0xFE, 0xFE, 0x09, 0x00]);
    let env = Env::new(dir);

    let message = env.error("Scripts.execStorage('short.tjs')");
    assert!(
        message.contains("header"),
        "expected a header error, got: {message}"
    );
    let message = env.error("Scripts.execStorage('mode9.tjs')");
    assert!(
        message.contains("mode 9"),
        "expected an unsupported-mode error, got: {message}"
    );
}
