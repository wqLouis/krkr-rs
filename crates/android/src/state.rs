//! The startup-state file the Android launcher reads.
//!
//! The launcher and the engine share one process, but the engine runs native
//! code on its own thread: when a game fails to start, or a native panic kills
//! the process, the Kotlin side is not guaranteed to run afterwards — and a
//! surface with no content is indistinguishable from a slow load. This module
//! gives the launcher a small on-disk signal it can poll instead.
//!
//! The contract with the launcher (`android/.../EngineDiagnostics.kt`) is
//! fixed:
//!
//! * directory `<filesDir>`, passed to native code as
//!   `KrkrGameActivity.pendingStateDir`;
//! * file name [`STATE_FILE_NAME`] (`krkr_state.json`);
//! * a flat JSON object:
//!
//!   ```json
//!   { "state": "starting", "message": null, "pid": 1234, "at": "2026-09-15T12:00:00Z" }
//!   ```
//!
//!   where `state` is one of `starting`, `running`, `failed`, `stopped`,
//!   `message` carries the error text for `failed` (otherwise `null`), `pid`
//!   is [`std::process::id`] and `at` is an RFC 3339 UTC timestamp.
//!
//! The file is **not** cleared on entry. A process that dies by panic or
//! `SIGKILL` therefore leaves the last state it wrote behind — `starting` or
//! `running` — which is exactly how the launcher tells a crash apart from a
//! normal exit (which writes `stopped`). Writes are atomic (temp file +
//! rename) so a reader never sees a half-written object.
//!
//! ## Off-device behaviour
//!
//! A host build has no state directory configured ([`set_state_dir`] is only
//! called from the Android entry point), so [`write_state`] is a no-op there
//! (it logs nothing and touches no file). [`write_state_at`] is the direct,
//! testable form used by the unit tests and works anywhere.
//!
//! ## What needs a device
//!
//! The JSON writer, the atomic replace and the missing-directory path are all
//! covered by unit tests on the host. What can only be checked on a device is
//! that Android's `filesDir` is writable by the app process, that the Kotlin
//! side actually passes it before `super.onCreate`, and that the launcher
//! polling loop interprets a stale `starting`/`running` as a crash.

use std::path::{Path, PathBuf};
#[cfg(target_os = "android")]
use std::sync::OnceLock;

/// File name the launcher reads. Fixed by the Kotlin contract.
pub const STATE_FILE_NAME: &str = "krkr_state.json";

/// Lifecycle state written to the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// The native entry point has begun; the game is not mounted yet.
    Starting,
    /// Storage is mounted, natives are registered and the VM is bootstrapped.
    Running,
    /// Startup did not succeed; [`write_state`]'s `message` carries the error.
    Failed,
    /// The app exited cleanly.
    Stopped,
}

impl State {
    /// String written to the JSON `state` field (also used by the launcher).
    pub fn as_str(self) -> &'static str {
        match self {
            State::Starting => "starting",
            State::Running => "running",
            State::Failed => "failed",
            State::Stopped => "stopped",
        }
    }
}

/// Directory holding `krkr_state.json`, set once at startup on Android from
/// `KrkrGameActivity.pendingStateDir`.
#[cfg(target_os = "android")]
static STATE_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Record the directory for the state file.
///
/// The first call wins; later calls are ignored so a second Activity in the
/// same process cannot redirect the file.
#[cfg(target_os = "android")]
pub fn set_state_dir(dir: impl Into<PathBuf>) {
    let _ = STATE_DIR.set(dir.into());
}

/// Write the state file for the configured directory.
///
/// This is the side-effecting entry point used by the Android entry point. It
/// is a no-op when no directory was configured and never panics: a failure to
/// write (missing directory, read-only storage, full disk) is logged and
/// swallowed, because the state file is a debugging aid and must not itself
/// become a startup failure.
#[cfg(target_os = "android")]
pub fn write_state(state: State, message: Option<&str>) {
    let Some(dir) = STATE_DIR.get() else {
        return;
    };
    if let Err(e) = write_state_at(dir, state, message) {
        log::error!(
            "krkr-state: cannot write {}: {e}",
            dir.join(STATE_FILE_NAME).display()
        );
    }
}

/// Write `state` into `dir/krkr_state.json` atomically.
///
/// Returns the underlying I/O error (rather than logging) so callers and tests
/// can decide what to do; [`write_state`] logs and ignores it. A missing `dir`
/// is reported as an ordinary error, never a panic.
pub fn write_state_at(dir: &Path, state: State, message: Option<&str>) -> std::io::Result<()> {
    let json = state_json(state, message);
    let destination = dir.join(STATE_FILE_NAME);
    // Same directory as the destination so the rename is atomic; the pid keeps
    // two processes from clobbering each other's temp file.
    let temp = dir.join(format!("{STATE_FILE_NAME}.tmp.{}", std::process::id()));
    // Write the whole record and flush it to the backing store *before* the
    // rename. The rename is what makes the replace atomic (a reader sees either
    // the old file or the whole new one), but without the fsync a power loss
    // can leave the previous `starting`/`running` visible after we already
    // reported the new state — which the launcher then reads as a crash. A
    // failed create/write/sync/rename leaves the previous state intact and
    // never panics.
    use std::io::Write as _;
    let mut file = std::fs::File::create(&temp)?;
    if let Err(e) = file.write_all(json.as_bytes()) {
        drop(file);
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    if let Err(e) = file.sync_all() {
        drop(file);
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    drop(file);
    if let Err(e) = std::fs::rename(&temp, &destination) {
        // Leave no temp file behind if the replace failed.
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    Ok(())
}

/// Serialize one state record. Exposed inside the crate so the log format and
/// the JSON shape stay next to the writer.
pub(crate) fn state_json(state: State, message: Option<&str>) -> String {
    let message = match message {
        Some(m) => format!("\"{}\"", escape_json(m)),
        None => "null".to_string(),
    };
    format!(
        "{{ \"state\": \"{}\", \"message\": {message}, \"pid\": {}, \"at\": \"{}\" }}",
        state.as_str(),
        std::process::id(),
        now_iso8601(),
    )
}

/// Current UTC time as RFC 3339 (`2026-09-15T12:00:00Z`), without pulling in a
/// date library for one field.
pub(crate) fn now_iso8601() -> String {
    let secs = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(since_epoch) => since_epoch.as_secs() as i64,
        // Clock before the epoch: keep going with a negative value rather than
        // panicking; the timestamp is informational only.
        Err(e) => -(e.duration().as_secs() as i64),
    };
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (hour, minute, second) = (rem / 3_600, (rem % 3_600) / 60, rem % 60);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Days since 1970-01-01 → (year, month, day). Howard Hinnant's `civil_from_days`
/// (public domain), valid for the whole `i64` range we can reach here.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Escape a string for embedding in a JSON string literal.
fn escape_json(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A process-unique scratch directory, removed by the caller.
    fn scratch_dir(tag: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "krkr-rs-state-test-{tag}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    /// The four state names are the launcher's contract; the writer emits the
    /// same spelling the Kotlin side reads.
    #[test]
    fn state_names_match_the_launcher_contract() {
        assert_eq!(State::Starting.as_str(), "starting");
        assert_eq!(State::Running.as_str(), "running");
        assert_eq!(State::Failed.as_str(), "failed");
        assert_eq!(State::Stopped.as_str(), "stopped");
    }

    #[test]
    fn state_file_round_trips() {
        let dir = scratch_dir("roundtrip");
        write_state_at(&dir, State::Running, None).expect("write running");

        let json = std::fs::read_to_string(dir.join(STATE_FILE_NAME)).expect("read state file");
        assert!(json.contains("\"state\": \"running\""), "json: {json}");
        assert!(json.contains("\"message\": null"), "json: {json}");
        assert!(
            json.contains(&format!("\"pid\": {}", std::process::id())),
            "json: {json}"
        );
        assert!(json.contains("\"at\": \""), "json: {json}");
        assert!(json.trim_end().ends_with('}'), "json: {json}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn state_file_replaces_atomically_and_escapes() {
        let dir = scratch_dir("replace");
        write_state_at(&dir, State::Starting, None).expect("write starting");
        // A message with a quote and a newline must not break the JSON.
        write_state_at(&dir, State::Failed, Some("mount \"x\"\nbroken")).expect("write failed");

        let json = std::fs::read_to_string(dir.join(STATE_FILE_NAME)).expect("read state file");
        assert!(json.contains("\"state\": \"failed\""), "json: {json}");
        assert!(
            json.contains("\"message\": \"mount \\\"x\\\"\\nbroken\""),
            "json: {json}"
        );
        // The replace must leave exactly one state file and no temp file.
        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .expect("read dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(STATE_FILE_NAME))
            .collect();
        assert_eq!(
            leftovers,
            vec![STATE_FILE_NAME.to_string()],
            "only the destination may remain"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn state_write_to_missing_directory_fails_without_panicking() {
        let missing = std::env::temp_dir().join(format!(
            "krkr-rs-state-missing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        // The directory is deliberately not created.
        let result = write_state_at(&missing, State::Failed, Some("boom"));
        assert!(result.is_err(), "a missing directory must be an error");
        assert!(
            !missing.exists(),
            "the writer must not create the directory"
        );
    }

    #[test]
    fn iso8601_format_is_well_formed() {
        let stamp = now_iso8601();
        // YYYY-MM-DDTHH:MM:SSZ
        assert_eq!(stamp.len(), 20, "stamp: {stamp}");
        assert_eq!(&stamp[4..5], "-");
        assert_eq!(&stamp[7..8], "-");
        assert_eq!(&stamp[10..11], "T");
        assert_eq!(&stamp[13..14], ":");
        assert_eq!(&stamp[16..17], ":");
        assert!(stamp.ends_with('Z'), "stamp: {stamp}");
    }

    #[test]
    fn civil_from_days_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(20_000), (2024, 10, 4));
    }
}
