//! The capped engine log file the Android launcher reads.
//!
//! The launcher's Logs screen and its crash dialog read a **file**, not logcat:
//! `LauncherLog.tail(EngineDiagnostics.engineLogFile(context), …)` reads
//! `<filesDir>/krkr.log` (`android/.../EngineDiagnostics.kt`,
//! `android/.../MainActivity.kt`). The Rust host used to install only an
//! `env_logger` writing to stderr; `android-activity` redirects stderr to
//! logcat, so that file was never written and the screen was always empty.
//!
//! This module is the file half of the fan-out. It is plain `std` + `log` so
//! the host test target exercises the record format, the byte accounting and
//! the rotation without a device; the (`env_logger`) stderr half and the
//! install of the global logger are the Android-only wiring in the entry
//! point.
//!
//! ## Contract with the launcher
//!
//! * Directory `<filesDir>`, the same directory as `krkr_state.json`
//!   ([`crate::state`]); the entry point passes it through
//!   `KrkrGameActivity.pendingStateDir`.
//! * File name [`LOG_FILE_NAME`] (`krkr.log`).
//! * UTF-8 lines, oldest first. The launcher reads the last 400 lines, up to
//!   256 KiB from the end, and drops a leading partial line, so a line may be
//!   any text (`LauncherLog.tail`).
//! * The file is capped at [`LOG_MAX_BYTES`] and rotated once, keeping
//!   `krkr.log.1`. A long session can therefore not fill the device's storage.
//!
//! The line format is `"<rfc3339> <LEVEL> <message>"` ([`format_record`]).
//!
//! ## What needs a device
//!
//! Rotation, reopening an existing file and the format are unit-tested on the
//! host. Only the device can show that `<filesDir>` is writable by the app
//! process and that the records actually appear in the launcher's Logs screen.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Log file name inside the state directory. Fixed by the Kotlin contract.
pub(crate) const LOG_FILE_NAME: &str = "krkr.log";

/// Rotate once the current file would exceed this; one previous file is kept.
///
/// The launcher keeps one rotated copy of its own log at 512 KiB
/// (`LauncherLog.MAX_BYTES`); the engine's is twice that because the engine
/// emits more per frame.
pub(crate) const LOG_MAX_BYTES: u64 = 1024 * 1024;

/// A size-capped, append-only log file with single-step rotation.
///
/// The cap spans launches: [`FileSink::open`] adopts the current on-disk size
/// so a file left behind by a previous process is rotated rather than grown
/// without bound.
pub(crate) struct FileSink {
    path: PathBuf,
    /// The `krkr.log.1` a rotation moves `path` to. Derived from `path` so the
    /// sink also works with a test path.
    rotated: PathBuf,
    max_bytes: u64,
    file: Option<File>,
    written: u64,
    /// Set after the first failure to (re)open the file so a broken sink does
    /// not print on every record. The logger cannot log its own failure
    /// without recursing, so stderr is used once and then kept quiet.
    reported_error: bool,
}

impl FileSink {
    /// Open (creating it if needed) the log at `path` in append mode.
    ///
    /// The directory is not created: a missing directory is the caller's
    /// error to report, not something the log may invent. The current size is
    /// adopted so the cap spans previous launches.
    pub(crate) fn open(path: PathBuf) -> std::io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            rotated: rotated_path(&path),
            path,
            max_bytes: LOG_MAX_BYTES,
            file: Some(file),
            written,
            reported_error: false,
        })
    }

    /// A sink with a smaller cap, for the rotation tests.
    #[cfg(test)]
    fn with_limit(path: PathBuf, max_bytes: u64) -> Self {
        let mut sink = Self::open(path).expect("open scratch log");
        sink.max_bytes = max_bytes;
        sink
    }

    /// Append one line (a newline is added).
    ///
    /// Rotation happens before the write when it would push the file past the
    /// cap. A line larger than the whole cap is still written once, so the
    /// file is bracketed by `2 * max_bytes` rather than truncated mid-record.
    pub(crate) fn write_line(&mut self, line: &str) {
        // One `usize`-to-`u64` widening; a line cannot be larger than memory.
        let line_len = line.len() as u64 + 1;
        if self.written.saturating_add(line_len) > self.max_bytes {
            self.rotate();
        }
        let written = match self.ensure_open() {
            Some(file) => file.write_all(line.as_bytes()).is_ok() && file.write_all(b"\n").is_ok(),
            None => false,
        };
        if written {
            self.written = self.written.saturating_add(line_len);
        }
    }

    /// Flush the current file, ignoring a flush failure (the sink reports its
    /// own errors once in [`Self::ensure_open`]).
    pub(crate) fn flush(&mut self) {
        if let Some(file) = self.file.as_mut() {
            let _ = file.flush();
        }
    }

    /// Close the current file and move it to the rotated name, dropping any
    /// older one. A missing current file is fine (nothing to rotate yet).
    fn rotate(&mut self) {
        self.file = None;
        let _ = std::fs::remove_file(&self.rotated);
        match std::fs::rename(&self.path, &self.rotated) {
            Ok(()) => {}
            // Nothing has been written yet; there is nothing to rotate.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => eprintln!(
                "krkr-rs: cannot rotate log {}: {error}",
                self.path.display()
            ),
        }
        self.written = 0;
    }

    /// The open file, reopening it (and adopting its size) after a rotation.
    ///
    /// A failure is reported to stderr once; the record is then dropped
    /// because there is no file to carry it.
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
                Err(error) => {
                    if !self.reported_error {
                        self.reported_error = true;
                        eprintln!(
                            "krkr-rs: cannot open log file {}: {error}",
                            self.path.display()
                        );
                    }
                    return None;
                }
            }
        }
        self.file.as_mut()
    }
}

/// The `krkr.log.1` name for a log path.
fn rotated_path(path: &Path) -> PathBuf {
    path.with_extension("log.1")
}

/// The line written for one `log` record: `"<rfc3339> <LEVEL> <message>"`.
///
/// The timestamp reuses the state file's RFC 3339 helper so both diagnostics
/// files agree. A multi-line message is written verbatim (the launcher splits
/// on `\n`); only the first line carries the timestamp.
pub(crate) fn format_record(record: &log::Record<'_>) -> String {
    format!(
        "{} {} {}",
        crate::state::now_iso8601(),
        record.level(),
        record.args()
    )
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

    /// The file line is the launcher's whole contract: an RFC 3339 stamp, the
    /// level and the message, one per line.
    #[test]
    fn format_record_has_timestamp_level_and_message() {
        let record = log::Record::builder()
            .args(format_args!("mount failed"))
            .level(log::Level::Warn)
            .target("krkr-rs-test")
            .build();
        let line = format_record(&record);
        assert!(line.contains(" WARN "), "line: {line}");
        assert!(line.ends_with("mount failed"), "line: {line}");
        // `YYYY-MM-DDTHH:MM:SSZ` prefix.
        assert_eq!(&line[4..5], "-", "line: {line}");
        assert_eq!(&line[10..11], "T", "line: {line}");
        assert!(
            line[19..20].chars().all(|c| c == 'Z' || c == ' '),
            "line: {line}"
        );
    }

    /// A line written to the sink is readable back, and a previous launch's
    /// bytes count against the cap.
    #[test]
    fn sink_appends_and_adopts_an_existing_size() {
        let dir = scratch_dir("append");
        let path = dir.join(LOG_FILE_NAME);
        {
            let mut sink = FileSink::with_limit(path.clone(), 1024);
            sink.write_line("first");
        }
        let mut sink = FileSink::with_limit(path.clone(), 1024);
        sink.write_line("second");
        let content = std::fs::read_to_string(&path).expect("read log");
        assert!(content.contains("first\n"), "content: {content}");
        assert!(content.contains("second\n"), "content: {content}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Flushing an open sink must not panic and must not lose the line.
    #[test]
    fn flush_after_a_write_is_a_no_op() {
        let dir = scratch_dir("flush");
        let path = dir.join(LOG_FILE_NAME);
        let mut sink = FileSink::with_limit(path.clone(), 1024);
        sink.write_line("line");
        // Flushing must not panic and must not lose the line.
        sink.flush();
        let content = std::fs::read_to_string(&path).expect("read log");
        assert!(content.contains("line\n"), "content: {content}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Rotation keeps exactly one previous file and the new line lands in the
    /// current one.
    #[test]
    fn sink_rotates_once_and_keeps_the_previous_file() {
        let dir = scratch_dir("rotate");
        let path = dir.join(LOG_FILE_NAME);
        let mut sink = FileSink::with_limit(path.clone(), 32);

        sink.write_line("first line"); // 11 bytes
        sink.write_line("second line"); // 12 -> 23
        sink.write_line("third line"); // would be 34 > 32 -> rotate
        sink.write_line("fourth line");

        let current = std::fs::read_to_string(&path).expect("current log");
        let previous = std::fs::read_to_string(dir.join("krkr.log.1")).expect("rotated log");
        assert!(previous.contains("first line"), "previous: {previous}");
        assert!(previous.contains("second line"), "previous: {previous}");
        assert!(!previous.contains("third line"), "previous: {previous}");
        assert!(current.contains("third line"), "current: {current}");
        assert!(current.contains("fourth line"), "current: {current}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A second rotation replaces the previous file rather than appending to
    /// it, so only one rotated copy ever exists.
    #[test]
    fn second_rotation_replaces_the_previous_file() {
        let dir = scratch_dir("rotate-twice");
        let path = dir.join(LOG_FILE_NAME);
        let mut sink = FileSink::with_limit(path.clone(), 20);

        sink.write_line("old batch a");
        sink.write_line("old batch b"); // rotate
        sink.write_line("new batch a"); // rotate again
        sink.write_line("new batch b");

        let previous = std::fs::read_to_string(dir.join("krkr.log.1")).expect("rotated log");
        assert!(previous.contains("new batch a"), "previous: {previous}");
        assert!(!previous.contains("old batch a"), "previous: {previous}");
        assert!(
            !previous.contains("old batch b"),
            "the previous file must be replaced, not appended: {previous}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A missing directory is a returned error, never a panic and never an
    /// invented directory.
    #[test]
    fn open_reports_a_missing_directory() {
        let missing = std::env::temp_dir().join(format!(
            "krkr-rs-log-missing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        let result = FileSink::open(missing.join(LOG_FILE_NAME));
        assert!(result.is_err(), "a missing directory must be an error");
        assert!(!missing.exists(), "the sink must not create the directory");
    }
}
