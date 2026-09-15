//! Engine logging: stderr on desktop, logcat + a capped file on Android.
//!
//! Android discards a process's stderr, so the engine's normal log output —
//! and Bevy's, since Bevy's own `LogPlugin` cannot install a backend once the
//! global logger is claimed — would otherwise be invisible on a device. This
//! module owns the single global `log::Log` implementation and fans each
//! record out to the sinks that exist on the current platform:
//!
//! * desktop: stderr (unchanged from before this module existed);
//! * Android: logcat, tag [`LOG_TAG`] (`krkr-rs`), via `android_logger`;
//! * Android: a file, `<stateDir>/krkr.log`, so a user can send it to a
//!   developer. It is capped at [`LOG_MAX_BYTES`] and rotates once, keeping
//!   the previous file as `krkr.log.1` — a runaway log inside the app's
//!   private storage is its own bug.
//!
//! `android_logger` is *not* installed as the global logger (only one logger
//! can be, and it must also feed the file); instead its `AndroidLogger::log`
//! method is called from our fan-out. The level honours the existing
//! `verbose` flag.
//!
//! A panic hook is installed as well: it logs the panic message, its source
//! location and a backtrace through the same sink before delegating to the
//! previous (default) hook. On Android the default hook's stderr output is
//! discarded, so the panic line in logcat / `krkr.log` is the only trace a
//! native crash leaves.
//!
//! ## What needs a device
//!
//! Fan-out and rotation are unit-tested on the host. What can only be checked
//! on a device is that records actually appear under tag `krkr-rs` in
//! `adb logcat`, that `<filesDir>` is writable, and that the backtrace is
//! symbolised in a stripped release build.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, Once, OnceLock};

use crate::state;

/// Logcat tag (and the stem the Kotlin side knows). Fixed by the Android
/// contract.
pub const LOG_TAG: &str = "krkr-rs";

/// Log file name inside the state directory. Fixed by the Android contract.
pub const LOG_FILE_NAME: &str = "krkr.log";

/// Rotate once the current file would exceed this; one previous file is kept.
pub const LOG_MAX_BYTES: u64 = 1024 * 1024;

/// The one global logger. Constructed on first [`init_logging`].
static LOGGER: OnceLock<FanoutLogger> = OnceLock::new();
/// Guards against installing the panic hook more than once.
static PANIC_HOOK: Once = Once::new();

/// A size-capped, append-only log file with single-step rotation.
struct FileSink {
    path: PathBuf,
    /// `krkr.log.1` for the contract path; computed from `path` so the sink
    /// stays usable with a test path.
    rotated: PathBuf,
    max_bytes: u64,
    file: Option<File>,
    written: u64,
    /// Set after the first open failure so a broken sink does not print on
    /// every record. (The logger cannot `log!` its own failure — that would
    /// recurse — so stderr is the only option and must be quiet.)
    reported_error: bool,
}

impl FileSink {
    fn new(path: PathBuf) -> Self {
        let rotated = path.with_extension("log.1");
        Self {
            path,
            rotated,
            max_bytes: LOG_MAX_BYTES,
            file: None,
            written: 0,
            reported_error: false,
        }
    }

    #[cfg(test)]
    fn with_limit(path: PathBuf, max_bytes: u64) -> Self {
        let mut sink = Self::new(path);
        sink.max_bytes = max_bytes;
        sink
    }

    /// Append one line (a newline is added). Rotation happens before the
    /// write when it would push the file past the cap; a file opened by an
    /// earlier process keeps its size so the cap spans launches.
    fn write_line(&mut self, line: &str) {
        let bytes = line.as_bytes();
        let line_len = bytes.len() as u64 + 1;
        if self.written.saturating_add(line_len) > self.max_bytes {
            self.rotate();
        }
        let written = match self.ensure_open() {
            Some(file) => file.write_all(bytes).is_ok() && file.write_all(b"\n").is_ok(),
            None => false,
        };
        if written {
            self.written = self.written.saturating_add(line_len);
        }
    }

    /// Close the current file and move it to the rotated name, dropping any
    /// older one. A missing current file is fine (nothing to rotate yet).
    fn rotate(&mut self) {
        self.file = None;
        let _ = std::fs::remove_file(&self.rotated);
        if let Err(e) = std::fs::rename(&self.path, &self.rotated)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("krkr-rs: cannot rotate log {}: {e}", self.path.display());
        }
        self.written = 0;
    }

    /// Open the file lazily (append) and adopt its current size.
    fn ensure_open(&mut self) -> Option<&mut File> {
        if self.file.is_none() {
            match OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
            {
                Ok(file) => {
                    self.written = file.metadata().map(|m| m.len()).unwrap_or(0);
                    self.file = Some(file);
                }
                Err(e) => {
                    if !self.reported_error {
                        self.reported_error = true;
                        eprintln!("krkr-rs: cannot open log file {}: {e}", self.path.display());
                    }
                    return None;
                }
            }
        }
        self.file.as_mut()
    }
}

/// Fan-out `log::Log`: platform console sink plus the optional file.
struct FanoutLogger {
    /// `None` on desktop (no state directory configured) so a normal desktop
    /// run never creates `krkr.log` in the working directory.
    file: Mutex<Option<FileSink>>,
    #[cfg(target_os = "android")]
    logcat: android_logger::AndroidLogger,
}

impl FanoutLogger {
    fn new() -> Self {
        let file = state::state_dir().map(|dir| FileSink::new(dir.join(LOG_FILE_NAME)));
        Self {
            file: Mutex::new(file),
            #[cfg(target_os = "android")]
            logcat: android_logger::AndroidLogger::new(
                android_logger::Config::default()
                    .with_tag(LOG_TAG)
                    .with_max_level(log::LevelFilter::Trace),
            ),
        }
    }

    fn write_file(&self, record: &log::Record) {
        let mut guard = match self.file.lock() {
            Ok(guard) => guard,
            // A panic during logging poisons the mutex but must not stop the
            // panic hook from recording the reason.
            Err(poisoned) => poisoned.into_inner(),
        };
        let Some(sink) = guard.as_mut() else {
            return;
        };
        sink.write_line(&format!(
            "{} {} {}",
            state::now_iso8601(),
            record.level(),
            record.args()
        ));
    }
}

impl log::Log for FanoutLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        #[cfg(not(target_os = "android"))]
        eprintln!("{}", record.args());
        #[cfg(target_os = "android")]
        self.logcat.log(record);
        self.write_file(record);
    }

    fn flush(&self) {
        let mut guard = match self.file.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(sink) = guard.as_mut()
            && let Some(file) = sink.file.as_mut()
        {
            let _ = file.flush();
        }
        #[cfg(target_os = "android")]
        self.logcat.flush();
    }
}

/// Install the global logger and the panic hook, and set the level from
/// `verbose` (DEBUG) vs the default (INFO).
///
/// Safe to call more than once: [`log::set_logger`] rejects a second install
/// and the panic hook is installed at most once. The state directory, if any,
/// must be configured *before* the first call so the file sink uses it.
pub fn init_logging(verbose: bool) {
    let logger = LOGGER.get_or_init(FanoutLogger::new);
    let _ = log::set_logger(logger);
    log::set_max_level(if verbose {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    });
    install_panic_hook();
}

/// Log every panic (message, location, backtrace) before the default hook
/// prints it. The backtrace is captured unconditionally — cheap enough, and a
/// stripped release build with no `RUST_BACKTRACE` would otherwise show
/// nothing at all.
fn install_panic_hook() {
    PANIC_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let location = info
                .location()
                .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
                .unwrap_or_else(|| "<unknown>".to_string());
            let backtrace = std::backtrace::Backtrace::force_capture();
            log::error!(
                "krkr-rs: PANIC at {location}: {}\n{backtrace}",
                panic_message(info)
            );
            previous(info);
        }));
    });
}

/// Message text of a panic payload.
fn panic_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    if let Some(s) = info.payload().downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = info.payload().downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(tag: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "krkr-rs-log-test-{tag}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn log_file_rotates_and_keeps_one_previous() {
        let dir = scratch_dir("rotate");
        let path = dir.join(LOG_FILE_NAME);
        let mut sink = FileSink::with_limit(path.clone(), 32);

        sink.write_line("first line"); // 11 bytes
        sink.write_line("second line"); // 12 → 23
        sink.write_line("third line"); // would be 34 > 32 → rotate
        sink.write_line("fourth line");

        let current = std::fs::read_to_string(&path).expect("current log");
        let previous =
            std::fs::read_to_string(dir.join(format!("{LOG_FILE_NAME}.1"))).expect("rotated log");
        assert!(previous.contains("first line"), "previous: {previous}");
        assert!(previous.contains("second line"), "previous: {previous}");
        assert!(!previous.contains("third line"), "previous: {previous}");
        assert!(current.contains("third line"), "current: {current}");
        assert!(current.contains("fourth line"), "current: {current}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn second_rotation_replaces_the_previous_file() {
        let dir = scratch_dir("rotate-twice");
        let path = dir.join(LOG_FILE_NAME);
        let mut sink = FileSink::with_limit(path.clone(), 20);

        sink.write_line("old batch a"); // 12
        sink.write_line("old batch b"); // would be 24 > 20 → rotate, then 12
        // written now 12; next write rotates the "old batch b" file away.
        sink.write_line("new batch a"); // 24 > 20? 12+12=24 > 20 → rotate
        sink.write_line("new batch b");

        let previous =
            std::fs::read_to_string(dir.join(format!("{LOG_FILE_NAME}.1"))).expect("rotated log");
        assert!(previous.contains("new batch a"), "previous: {previous}");
        assert!(!previous.contains("old batch a"), "previous: {previous}");
        assert!(
            !previous.contains("old batch b"),
            "previous must be replaced, not appended: {previous}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sink_reopens_an_existing_file_and_spans_the_cap() {
        let dir = scratch_dir("reopen");
        let path = dir.join(LOG_FILE_NAME);
        {
            let mut sink = FileSink::with_limit(path.clone(), 100);
            sink.write_line("before restart");
        }
        // A new sink (a new process) appends and adopts the on-disk size.
        let mut sink = FileSink::with_limit(path.clone(), 100);
        sink.write_line("after restart");
        let content = std::fs::read_to_string(&path).expect("log");
        assert!(content.contains("before restart"), "content: {content}");
        assert!(content.contains("after restart"), "content: {content}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_log_directory_is_not_fatal() {
        let missing = std::env::temp_dir().join(format!(
            "krkr-rs-log-missing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        let mut sink = FileSink::with_limit(missing.join(LOG_FILE_NAME), 100);
        // Must silently drop the line, not panic.
        sink.write_line("nowhere to go");
        assert!(!missing.exists());
    }

    #[test]
    fn fanout_without_state_dir_has_no_file() {
        let logger = FanoutLogger::new();
        assert!(
            logger.file.lock().expect("file lock").is_none(),
            "desktop (no state dir) must not create a log file"
        );
    }
}
