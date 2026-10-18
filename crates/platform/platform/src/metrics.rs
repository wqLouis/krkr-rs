//! Host process metrics for the debug overlay.
//!
//! Every reader is a stateless free function: it reads the host interface on
//! each call and holds no cache. The overlay owns the previous sample and the
//! refresh cadence, so a rate is computed from the counters it asks for.

use std::fs;

/// Cumulative bytes this process has read from and written to storage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessIo {
    /// Bytes read from the storage layer (`/proc/self/io` `read_bytes`).
    pub read_bytes: u64,
    /// Bytes written to the storage layer (`/proc/self/io` `write_bytes`).
    pub write_bytes: u64,
}

/// Returns this process's cumulative storage read/write bytes.
///
/// Reads `/proc/self/io` on Linux and Android. A missing or malformed file
/// returns `None`, so the overlay shows an unknown value rather than a
/// fabricated zero.
pub fn process_io() -> Option<ProcessIo> {
    if !(cfg!(target_os = "linux") || cfg!(target_os = "android")) {
        return None;
    }
    let text = match fs::read_to_string("/proc/self/io") {
        Ok(text) => text,
        Err(error) => {
            log::debug!("cannot read /proc/self/io: {error}");
            return None;
        }
    };
    parse_process_io(&text)
}

/// Parses the `read_bytes` and `write_bytes` lines of `/proc/self/io`.
fn parse_process_io(text: &str) -> Option<ProcessIo> {
    let mut read_bytes = None;
    let mut write_bytes = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = match value.trim().parse::<u64>() {
            Ok(value) => value,
            Err(error) => {
                log::warn!("cannot parse {key:?} in /proc/self/io: {error}");
                return None;
            }
        };
        match key {
            "read_bytes" => read_bytes = Some(value),
            "write_bytes" => write_bytes = Some(value),
            _ => {}
        }
    }
    Some(ProcessIo {
        read_bytes: read_bytes?,
        write_bytes: write_bytes?,
    })
}

/// Returns this process's cumulative CPU runtime in nanoseconds.
///
/// Sums the first field of `/proc/self/task/*/schedstat` so every thread is
/// counted without assuming a clock-tick rate. A task that exits during the
/// scan is skipped; `None` means no task exposed a readable counter.
pub fn process_cpu_ns() -> Option<u64> {
    if !(cfg!(target_os = "linux") || cfg!(target_os = "android")) {
        return None;
    }
    let entries = match fs::read_dir("/proc/self/task") {
        Ok(entries) => entries,
        Err(error) => {
            log::debug!("cannot read /proc/self/task: {error}");
            return None;
        }
    };
    let mut total = 0u64;
    let mut seen = false;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                log::debug!("cannot read a /proc/self/task entry: {error}");
                continue;
            }
        };
        let path = entry.path().join("schedstat");
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => {
                log::debug!("cannot read {}: {error}", path.display());
                continue;
            }
        };
        let Some(runtime) = parse_schedstat(&text) else {
            log::warn!("cannot parse the CPU runtime in {}", path.display());
            continue;
        };
        total = match total.checked_add(runtime) {
            Some(total) => total,
            None => {
                log::warn!("the process CPU runtime overflowed u64");
                return None;
            }
        };
        seen = true;
    }
    seen.then_some(total)
}

/// Parses the first (runtime nanoseconds) field of a `schedstat` line.
fn parse_schedstat(text: &str) -> Option<u64> {
    let value = text.split_whitespace().next()?;
    match value.parse::<u64>() {
        Ok(runtime) => Some(runtime),
        Err(error) => {
            log::warn!("cannot parse a schedstat runtime {value:?}: {error}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_process_io_counters() {
        let text = "rchar: 100\nwchar: 200\nsyscr: 1\nsyscw: 2\nread_bytes: 4096\nwrite_bytes: 8192\ncancelled_write_bytes: 0\n";
        assert_eq!(
            parse_process_io(text),
            Some(ProcessIo {
                read_bytes: 4096,
                write_bytes: 8192
            })
        );
    }

    #[test]
    fn missing_io_counters_are_unknown() {
        assert_eq!(parse_process_io("rchar: 1\n"), None);
        assert_eq!(parse_process_io("read_bytes: 1\nwrite_bytes: nope\n"), None);
    }

    #[test]
    fn parses_schedstat_runtime() {
        assert_eq!(parse_schedstat("12345 0 1\n"), Some(12345));
        assert_eq!(parse_schedstat("nope 0 1\n"), None);
        assert_eq!(parse_schedstat(""), None);
    }
}
