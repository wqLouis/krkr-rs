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
//! * The file is capped at [`LOG_MAX_BYTES`] and rotated through
//!   `krkr.log.1` … `krkr.log.{LOG_ROTATED_FILES}`; each rotation shifts the
//!   existing files up one slot and drops the oldest. A long session can
//!   therefore not fill the device's storage, and a failure cascade cannot
//!   push the start of the same session out of the history as easily as the
//!   single rotation did.
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

/// Rotate the current file once it would exceed this. The launcher keeps one
/// rotated copy of its own log at 512 KiB (`LauncherLog.MAX_BYTES`); the
/// engine's is twice that because the engine emits more per frame.
pub(crate) const LOG_MAX_BYTES: u64 = 1024 * 1024;

/// How many rotated files are kept beside the current one.
///
/// A rotation moves `krkr.log` to `krkr.log.1`, shifts every existing
/// rotation up one slot and drops the file in the last slot, so the sink
/// holds `krkr.log` plus this many rotations.
///
/// Six is chosen against the 1 MiB cap and the spam a broken title produces.
/// A session whose failure repeats a VM exception dump (the observed cascade
/// was 87% repeated dumps) writes several times the cap, so the old
/// single-rotation policy could lose the start of the very session that
/// failed — including the original exception. Six rotations hold roughly the
/// last 7 MiB of output, a few launches' startup plus the failure cascade,
/// while keeping the sink's disk use bounded (see [`FileSink`]).
pub(crate) const LOG_ROTATED_FILES: usize = 6;

/// A size-capped, append-only log file with multi-step rotation.
///
/// The cap spans launches: [`FileSink::open`] adopts the current on-disk size
/// so a file left behind by a previous process is rotated rather than grown
/// without bound.
///
/// Worst-case disk use is `(1 + LOG_ROTATED_FILES) * LOG_MAX_BYTES` — about
/// 7 MiB — for records no larger than the cap. A record larger than the whole
/// cap is still written whole (the file is never truncated mid-record); each
/// file can hold at most one such record, so a single oversized record adds
/// its own size once rather than being silently dropped.
pub(crate) struct FileSink {
    path: PathBuf,
    /// How many rotated files to keep, `krkr.log.1` … `krkr.log.keep`.
    keep: usize,
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
        let written = file.metadata()?.len();
        Ok(Self {
            path,
            keep: LOG_ROTATED_FILES,
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

    /// A sink with a smaller cap and fewer rotations, for the rotation tests.
    #[cfg(test)]
    fn with_limit_and_keep(path: PathBuf, max_bytes: u64, keep: usize) -> Self {
        let mut sink = Self::open(path).expect("open scratch log");
        sink.max_bytes = max_bytes;
        sink.keep = keep;
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

    /// Close the current file and shift the rotations up, dropping the oldest.
    ///
    /// A missing file at any step is fine: the slots fill in as rotations
    /// accumulate, and the first rotation has nothing to shift.
    ///
    /// A failed rename is reported to stderr and the next slot is still
    /// shifted, so one unreadable file cannot stop the current log from being
    /// preserved. A rotation that could not move `path` leaves it in place;
    /// the next write reopens it and continues, so no record is invented.
    fn rotate(&mut self) {
        self.file = None;
        let _ = std::fs::remove_file(rotated_path(&self.path, self.keep));
        for index in (1..self.keep).rev() {
            let from = rotated_path(&self.path, index);
            let to = rotated_path(&self.path, index + 1);
            match std::fs::rename(&from, &to) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => eprintln!(
                    "krkr-rs: cannot rotate log {} to {}: {error}",
                    from.display(),
                    to.display()
                ),
            }
        }
        match std::fs::rename(&self.path, rotated_path(&self.path, 1)) {
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
                Ok(file) => match file.metadata() {
                    Ok(metadata) => {
                        self.written = metadata.len();
                        self.file = Some(file);
                    }
                    Err(error) => {
                        if !self.reported_error {
                            self.reported_error = true;
                            eprintln!(
                                "krkr-rs: cannot stat log file {}: {error}",
                                self.path.display()
                            );
                        }
                        return None;
                    }
                },
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

/// The rotation file name for slot `index` (`1` is the most recent).
fn rotated_path(path: &Path, index: usize) -> PathBuf {
    path.with_extension(format!("log.{index}"))
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

    /// One rotation moves the current file into the first slot and the new
    /// line lands in the current one.
    #[test]
    fn sink_rotates_and_keeps_the_previous_file() {
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

    /// Each rotation shifts the existing slots up one and moves the current
    /// file into slot 1, so the earlier history is preserved rather than
    /// overwritten.
    #[test]
    fn rotation_shifts_older_slots_and_keeps_the_history() {
        let dir = scratch_dir("rotate-shift");
        let path = dir.join(LOG_FILE_NAME);
        let mut sink = FileSink::with_limit(path.clone(), 20);

        sink.write_line("old batch a");
        sink.write_line("old batch b"); // rotate: .1 = old a
        sink.write_line("new batch a"); // rotate: .2 = old a, .1 = old b
        sink.write_line("new batch b"); // rotate: .3 = old a, .2 = old b, .1 = new a

        let current = std::fs::read_to_string(&path).expect("current log");
        let slot1 = std::fs::read_to_string(dir.join("krkr.log.1")).expect("slot 1");
        let slot2 = std::fs::read_to_string(dir.join("krkr.log.2")).expect("slot 2");
        let slot3 = std::fs::read_to_string(dir.join("krkr.log.3")).expect("slot 3");
        assert!(current.contains("new batch b"), "current: {current}");
        assert!(slot1.contains("new batch a"), "slot 1: {slot1}");
        assert!(slot2.contains("old batch b"), "slot 2: {slot2}");
        assert!(slot3.contains("old batch a"), "slot 3: {slot3}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The sink keeps exactly [`LOG_ROTATED_FILES`] rotations: every configured
    /// name exists and the next slot never does.
    #[test]
    fn rotation_keeps_the_configured_number_of_files() {
        let dir = scratch_dir("rotate-count");
        let path = dir.join(LOG_FILE_NAME);
        // 8-byte records and an 8-byte cap: every record after the first
        // rotates, so the sink is exercised well past a full set of slots.
        let mut sink = FileSink::with_limit(path.clone(), 8);
        for index in 0..(LOG_ROTATED_FILES + 3) {
            sink.write_line(&format!("entry-{index}"));
        }
        assert!(path.is_file(), "current log");
        for slot in 1..=LOG_ROTATED_FILES {
            let file = dir.join(format!("{LOG_FILE_NAME}.{slot}"));
            assert!(file.is_file(), "slot {slot} must exist");
        }
        assert!(
            !dir.join(format!("{LOG_FILE_NAME}.{}", LOG_ROTATED_FILES + 1))
                .exists(),
            "the sink must not keep a slot past LOG_ROTATED_FILES"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Once the slots are full the oldest is dropped rather than kept in a
    /// growing file list.
    #[test]
    fn rotation_drops_the_oldest_file() {
        let dir = scratch_dir("rotate-oldest");
        let path = dir.join(LOG_FILE_NAME);
        let mut sink = FileSink::with_limit_and_keep(path.clone(), 14, 3);
        for index in 1..=9 {
            sink.write_line(&format!("line-{index}"));
        }

        // 7-byte records and a 14-byte cap: two records per file, so slot 3
        // holds records 3 and 4 and the first two records are gone.
        let slot3 = std::fs::read_to_string(dir.join("krkr.log.3")).expect("slot 3");
        assert!(slot3.contains("line-3"), "slot 3: {slot3}");
        assert!(slot3.contains("line-4"), "slot 3: {slot3}");
        for name in [
            LOG_FILE_NAME.to_string(),
            "krkr.log.1".to_string(),
            "krkr.log.2".to_string(),
        ] {
            let text = std::fs::read_to_string(dir.join(&name)).expect(&name);
            assert!(!text.contains("line-1"), "oldest dropped: {name}: {text}");
            assert!(!text.contains("line-2"), "oldest dropped: {name}: {text}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file left by a previous process is rotated into the first slot on the
    /// next write after it reaches the cap, not appended past it.
    #[test]
    fn a_previous_process_file_is_rotated_not_appended() {
        let dir = scratch_dir("previous-process");
        let path = dir.join(LOG_FILE_NAME);
        std::fs::write(&path, "before\n").expect("previous process log");
        // The cap is exactly the previous file's size, so the next record
        // rotates it into slot 1 instead of appending to it.
        let mut sink = FileSink::with_limit_and_keep(path.clone(), 7, 3);
        sink.write_line("after");

        let current = std::fs::read_to_string(&path).expect("current log");
        let slot1 = std::fs::read_to_string(dir.join("krkr.log.1")).expect("slot 1");
        assert!(slot1.contains("before"), "slot 1: {slot1}");
        assert!(!slot1.contains("after"), "slot 1: {slot1}");
        assert!(current.contains("after"), "current: {current}");
        assert!(!current.contains("before"), "current: {current}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// No file grows past the cap: the byte accounting holds for the current
    /// log and every rotation.
    #[test]
    fn the_cap_holds_for_every_file() {
        let dir = scratch_dir("cap");
        let path = dir.join(LOG_FILE_NAME);
        let cap = 20;
        let mut sink = FileSink::with_limit_and_keep(path.clone(), cap, 3);
        for index in 0..40 {
            sink.write_line(&format!("l{index}")); // 3-4 bytes each
        }
        for slot in 0..=3 {
            let file = if slot == 0 {
                path.clone()
            } else {
                dir.join(format!("{LOG_FILE_NAME}.{slot}"))
            };
            if !file.exists() {
                continue;
            }
            let len = file.metadata().expect("metadata").len();
            assert!(len <= cap, "slot {slot} is {len} bytes, cap {cap}");
        }
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
