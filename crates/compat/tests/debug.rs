//! Integration tests for the `Debug` native class.

mod common;

use common::vm_lock;
use compat::CompatHost;
use tjs::{Tjs2Engine, TjsValue};

/// Build a VM with the full compat surface installed and run `f` against it.
fn with_vm(f: impl FnOnce(&Tjs2Engine)) {
    let engine = Tjs2Engine::new().expect("create engine");
    let mut host = CompatHost::new();
    host.install(&engine).expect("install compat surface");
    f(&engine);
}

/// Seconds-of-day of the current instant under UTC, from the C library's
/// `gmtime_r`. `natives/debug.rs::log_timestamp` derives the stamp from
/// `std::time` in UTC, while the reference `TVPAddLog` uses `localtime`
/// (`reference/cpp/core/utils/DebugIntf.cpp:360-361`) — a documented
/// deviation; the ±2 s window below absorbs the one-second resolution.
fn now_utc_seconds_of_day() -> i64 {
    let mut now: libc::time_t = 0;
    let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: `time` writes `now`, `gmtime_r` fills the `tm` it is handed.
    let filled = unsafe {
        libc::time(&mut now);
        libc::gmtime_r(&now, tm.as_mut_ptr())
    };
    assert!(!filled.is_null(), "gmtime_r must fill the UTC breakdown");
    // SAFETY: `gmtime_r` initialized `tm` above.
    let tm = unsafe { tm.assume_init() };
    tm.tm_hour as i64 * 3600 + tm.tm_min as i64 * 60 + tm.tm_sec as i64
}

/// Cross-check an `HH:MM:SS` log stamp against the current UTC wall clock
/// within ±2 s, wrapping across midnight.
fn assert_utc_stamp_matches_now(stamp: &str, line: &str) {
    let field = |range: std::ops::Range<usize>| -> i64 {
        stamp[range]
            .parse()
            .unwrap_or_else(|_| panic!("bad timestamp in {line:?}"))
    };
    let actual = field(0..2) * 3600 + field(3..5) * 60 + field(6..8);
    let now = now_utc_seconds_of_day();
    let delta = (actual - now).rem_euclid(86_400);
    let distance = delta.min(86_400 - delta);
    assert!(
        distance <= 2,
        "expected the UTC wall clock within ±2 s of {line:?} (off by {distance} s)"
    );
}

/// Assert that `value` is the reference `TVPGetLastLog` rendering of
/// `logs`: every entry is `Time + ' ' + Log + "\r\n"`, with `Time` stamped
/// `%H:%M:%S` by `TVPAddLog` before the entry is queued
/// (`reference/cpp/core/utils/DebugIntf.cpp:419-459`,
/// `reference/cpp/core/utils/DebugIntf.cpp:344-357` — the `%H:%M:%S`
/// format string itself is at `DebugIntf.cpp:361`).
fn assert_stamped_log(value: TjsValue, logs: &[&str]) {
    let text = match value {
        TjsValue::String(text) => text,
        other => panic!("expected a string, got {other:?}"),
    };
    let body = text
        .strip_suffix("\r\n")
        .unwrap_or_else(|| panic!("the log must end with \"\\r\\n\", got {text:?}"));
    let lines: Vec<&str> = body.split("\r\n").collect();
    assert_eq!(lines.len(), logs.len(), "entry count in {text:?}");
    for (line, expected) in lines.iter().zip(logs) {
        let bytes = line.as_bytes();
        assert!(
            bytes.len() >= 9
                && bytes[..8]
                    .iter()
                    .enumerate()
                    .all(|(index, byte)| match index {
                        0 | 1 | 3 | 4 | 6 | 7 => byte.is_ascii_digit(),
                        _ => *byte == b':',
                    })
                && bytes[8] == b' ',
            "expected a `%H:%M:%S {expected}` line, got {line:?}"
        );
        assert_utc_stamp_matches_now(&line[..8], line);
        assert_eq!(&line[9..], *expected, "entry body in {line:?}");
    }
}

/// Reference: `TVPGetLastLog` returns the most recent `n` entries — each
/// stamped `Time + ' ' + Log + "\r\n"`, `Time` being `%H:%M:%S`
/// (`reference/cpp/core/utils/DebugIntf.cpp:419-459`,
/// `reference/cpp/core/utils/DebugIntf.cpp:344-357`) — and `n == 0`
/// yields the empty string even when the buffer holds entries
/// (`reference/cpp/core/utils/DebugIntf.cpp:424-429`).
#[test]
fn message_and_notice_record_into_the_ring_buffer() {
    let _lock = vm_lock();
    with_vm(|e| {
        // An empty buffer reads back as the empty string.
        assert_eq!(
            e.eval("Debug.getLastLog()", "t").unwrap(),
            TjsValue::String(String::new())
        );
        e.exec_script("Debug.message('one'); Debug.notice('two');", "t")
            .unwrap();
        // Without an argument every buffered entry comes back, stamped.
        assert_stamped_log(e.eval("Debug.getLastLog()", "t").unwrap(), &["one", "two"]);
        // An explicit count takes the `n` most-recent entries in
        // chronological order.
        assert_stamped_log(e.eval("Debug.getLastLog(2)", "t").unwrap(), &["one", "two"]);
        assert_stamped_log(e.eval("Debug.getLastLog(1)", "t").unwrap(), &["two"]);
        // `n == 0` returns the empty string.
        assert_eq!(
            e.eval("Debug.getLastLog(0)", "t").unwrap(),
            TjsValue::String(String::new())
        );
        e.exec_script("Debug.message('x');", "t").unwrap();
        assert_eq!(
            e.eval("Debug.getLastLog(0)", "t").unwrap(),
            TjsValue::String(String::new())
        );
        assert_stamped_log(e.eval("Debug.getLastLog(1)", "t").unwrap(), &["x"]);
        assert_stamped_log(e.eval("Debug.getLastLog(2)", "t").unwrap(), &["two", "x"]);
    });
}

/// Reference: with no argument the window is `TVPLogMaxLines + 100` — 2148
/// (`reference/cpp/core/utils/DebugIntf.cpp:1454-1457` for the binding,
/// `DebugIntf.cpp:43` for `TVPLogMaxLines`) — which is also the ring's size
/// bound (`reference/cpp/core/utils/DebugIntf.cpp:366-379`), so the default
/// call returns everything the buffer holds (`DebugIntf.cpp:419-459`).
#[test]
fn default_window_covers_the_whole_reference_ring() {
    let _lock = vm_lock();
    with_vm(|e| {
        // Eviction is in batches of a hundred once the buffer reaches 2148
        // (`reference/cpp/core/utils/DebugIntf.cpp:373-379`), so 2150 messages
        // leave the newest 2050 (`m100` .. `m2149`) in the ring.
        e.exec_script(
            "for (var i = 0; i < 2150; i++) Debug.message('m' + i);",
            "t",
        )
        .unwrap();
        let default = e.eval("Debug.getLastLog()", "t").unwrap();
        assert_eq!(
            default,
            e.eval("Debug.getLastLog(2148)", "t").unwrap(),
            "the no-argument window must be TVPLogMaxLines + 100 (2148)"
        );
        let text = match default {
            TjsValue::String(text) => text,
            other => panic!("expected a string, got {other:?}"),
        };
        let lines: Vec<&str> = text.split("\r\n").collect();
        // The ring never holds more than `TVPLogMaxLines + 100 - 1` entries
        // (eviction fires at 2148), so the default 2148-entry window always
        // covers the whole buffer (`DebugIntf.cpp:1454-1457`, `:373-379`).
        assert_eq!(
            lines.len(),
            2051,
            "2050 stamped entries plus the trailing separator: {text:?}"
        );
        assert!(
            lines[0].ends_with(" m100"),
            "the whole ring is returned, oldest first: {:?}",
            lines[0]
        );
        assert!(
            lines[2049].ends_with(" m2149"),
            "the newest entry must be last: {:?}",
            lines[2049]
        );
        assert_eq!(lines[2050], "");
        // An explicit count below the buffer size clips to the newest `n`.
        let newest = e.eval("Debug.getLastLog(3)", "t").unwrap();
        let newest = match newest {
            TjsValue::String(text) => text,
            other => panic!("expected a string, got {other:?}"),
        };
        let clipped: Vec<&str> = newest.split("\r\n").collect();
        assert_eq!(clipped.len(), 4, "3 entries plus the trailing separator");
        assert!(clipped[0].ends_with(" m2147"), "newest 3: {:?}", clipped[0]);
        assert!(
            clipped[2].ends_with(" m2149"),
            "newest last: {:?}",
            clipped[2]
        );
    });
}

#[test]
fn ring_drops_the_oldest_hundred_at_once() {
    let _lock = vm_lock();
    with_vm(|e| {
        // Reaching the 2148-entry cap evicts the oldest hundred in one go
        // (`reference/cpp/core/utils/DebugIntf.cpp:373-379`), so 2148 pushed
        // messages leave the newest 2048 (`m100` .. `m2147`) — a buffer that
        // evicted singly would hold 2147 of `m1` .. `m2147` instead.
        e.exec_script(
            "for (var i = 0; i < 2148; i++) Debug.message('m' + i);",
            "t",
        )
        .unwrap();
        let text = match e.eval("Debug.getLastLog(2148)", "t").unwrap() {
            TjsValue::String(text) => text,
            other => panic!("expected a string, got {other:?}"),
        };
        let lines: Vec<&str> = text.split("\r\n").collect();
        assert_eq!(lines.len(), 2049, "2048 entries plus the separator");
        assert!(
            lines[0].ends_with(" m100"),
            "eviction removed exactly the oldest hundred: {:?}",
            lines[0]
        );
        assert!(
            lines[2047].ends_with(" m2147"),
            "newest last: {:?}",
            lines[2047]
        );
    });
}

/// Reference: `Debug.message(...)` joins its arguments with `", "` into one
/// logged line (`reference/cpp/core/utils/DebugIntf.cpp:578-597`), queued
/// through `TVPGetLastLog`'s rendering (`reference/cpp/core/utils/DebugIntf.cpp:419-459`).
/// An empty call is `TJS_E_BADPARAMCOUNT` for `message` and `notice` alike
/// (`reference/cpp/core/utils/DebugIntf.cpp:579-580`, `600-601`).
#[test]
fn message_joins_multiple_arguments() {
    let _lock = vm_lock();
    with_vm(|e| {
        e.exec_script("Debug.message('a', 1, 'b');", "t").unwrap();
        assert_stamped_log(e.eval("Debug.getLastLog()", "t").unwrap(), &["a, 1, b"]);
        assert!(e.eval("Debug.message()", "t").is_err());
        assert!(e.eval("Debug.notice()", "t").is_err());
    });
}

/// Reference: `TVPOnLog(buf)` hands every registered handler the line
/// **prefixed with a space** — `buf` is `" " + line` because `TVPAddLog`
/// writes the (commented-out) timestamp slot first
/// (`reference/cpp/core/utils/DebugIntf.cpp:377-391`), and
/// `_TVPDeliverLoggingEvent` calls back with exactly that buffer
/// (`reference/cpp/core/utils/DebugIntf.cpp:122-147`).
#[test]
fn logging_handler_receives_messages_and_can_be_removed() {
    let _lock = vm_lock();
    with_vm(|e| {
        e.exec_script(
            "var got = []; function h(s) { got.push(s); } \
             Debug.addLoggingHandler(h); Debug.message('one'); Debug.notice('two');",
            "t",
        )
        .unwrap();
        assert_eq!(e.eval("got.count", "t").unwrap(), TjsValue::Integer(2));
        assert_eq!(
            e.eval("got[0]", "t").unwrap(),
            TjsValue::String(" one".into())
        );
        assert_eq!(
            e.eval("got[1]", "t").unwrap(),
            TjsValue::String(" two".into())
        );
        // Removing by identity stops delivery.
        e.exec_script(
            "Debug.removeLoggingHandler(h); Debug.message('three');",
            "t",
        )
        .unwrap();
        assert_eq!(e.eval("got.count", "t").unwrap(), TjsValue::Integer(2));
        // Duplicate registration is ignored.
        e.exec_script(
            "Debug.addLoggingHandler(h); Debug.addLoggingHandler(h); \
             Debug.message('four');",
            "t",
        )
        .unwrap();
        assert_eq!(e.eval("got.count", "t").unwrap(), TjsValue::Integer(3));
        assert_eq!(
            e.eval("got[2]", "t").unwrap(),
            TjsValue::String(" four".into())
        );
    });
}

/// Reference: `addLoggingHandler` converts its argument with
/// `AsObjectClosureNoAddRef()` (`reference/cpp/core/utils/DebugIntf.cpp:639-652`),
/// which throws on a non-object — so `addLoggingHandler(42)` raises — while
/// TJS `null` *is* an object variant (a null closure): it is accepted
/// silently and simply never invoked
/// (`reference/cpp/core/utils/DebugIntf.cpp:124-137`). Missing arguments
/// remain `TJS_E_BADPARAMCOUNT` (`reference/cpp/core/utils/DebugIntf.cpp:642-643`).
#[test]
fn non_object_handler_argument_raises_while_null_is_accepted() {
    let _lock = vm_lock();
    with_vm(|e| {
        let error = e
            .eval("Debug.addLoggingHandler(42)", "t")
            .expect_err("a non-object handler must raise")
            .to_string();
        assert!(error.contains("not an object"), "unexpected error: {error}");
        assert!(e.eval("Debug.addLoggingHandler()", "t").is_err());
        // `null` is stored silently; logging keeps working either way.
        e.exec_script(
            "Debug.addLoggingHandler(null); Debug.message('still works');",
            "t",
        )
        .unwrap();
        assert_stamped_log(e.eval("Debug.getLastLog()", "t").unwrap(), &["still works"]);
    });
}

#[test]
fn properties_round_trip() {
    let _lock = vm_lock();
    with_vm(|e| {
        assert_eq!(
            e.eval("Debug.logToFileOnError", "t").unwrap(),
            TjsValue::Integer(0)
        );
        assert_eq!(
            e.eval("Debug.clearLogFileOnError", "t").unwrap(),
            TjsValue::Integer(0)
        );
        e.exec_script(
            "Debug.logLocation = 'logs/'; Debug.logToFileOnError = true; \
             Debug.clearLogFileOnError = true;",
            "t",
        )
        .unwrap();
        assert_eq!(
            e.eval("Debug.logLocation", "t").unwrap(),
            TjsValue::String("logs/".into())
        );
        assert_eq!(
            e.eval("Debug.logToFileOnError", "t").unwrap(),
            TjsValue::Integer(1)
        );
        assert_eq!(
            e.eval("Debug.clearLogFileOnError", "t").unwrap(),
            TjsValue::Integer(1)
        );
    });
}

#[test]
fn log_location_clears_archive_names_and_honours_forcelog() {
    let _lock = vm_lock();
    let dir = std::env::temp_dir().join(format!(
        "krkr_debug_force_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let literal = dir.to_string_lossy().replace('\\', "/");
    let file = dir.join("krkr.console.log");

    with_vm(|e| {
        // An in-archive name has no local form and clears the location
        // (`DebugIntf.cpp:518-532`, `StorageIntf.cpp:554-560`).
        e.exec_script("Debug.logLocation = 'x.xp3>y';", "t")
            .unwrap();
        assert_eq!(
            e.eval("Debug.logLocation", "t").unwrap(),
            TjsValue::String(String::new())
        );

        // `-forcelog=yes` starts logging at the next location
        // (`DebugIntf.cpp:534-543`).
        e.exec_script("System.setArgument('-forcelog', 'yes');", "t")
            .unwrap();
        e.exec_script(&format!("Debug.logLocation = '{literal}';"), "t")
            .unwrap();
        e.exec_script("Debug.message('forced');", "t").unwrap();
    });

    let written = std::fs::read_to_string(&file)
        .unwrap_or_else(|error| panic!("-forcelog=yes must open the log file: {error}"));
    assert!(written.contains("forced"), "log: {written:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Reference: `startLogToFile` takes an **optional clear flag**, not a
/// filename (`reference/cpp/core/utils/DebugIntf.cpp:620-630`); the stream
/// is `{logLocation}/krkr.console.log`
/// (`reference/cpp/core/utils/DebugIntf.cpp:247-258`) and each logged line
/// lands in it as `" " + text + "\r\n"` (`reference/cpp/core/utils/DebugIntf.cpp:303-324`).
/// Without the clear flag the existing file is opened in append mode —
/// `TVPLogStreamHolder::Clear()` (the `clear == true` path) is what
/// truncates (`reference/cpp/core/utils/DebugIntf.cpp:294-300`,
/// `reference/cpp/core/utils/DebugIntf.cpp:471-483`).
#[test]
fn start_log_to_file_appends_messages() {
    let _lock = vm_lock();
    let dir = std::env::temp_dir().join(format!(
        "krkr_debug_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("krkr.console.log");
    let literal = dir.to_string_lossy().replace('\\', "/");
    // A pre-existing log survives a start *without* the clear flag.
    std::fs::write(&path, "stale-marker\r\n").unwrap();

    with_vm(|e| {
        let script = format!(
            "Debug.logLocation = '{literal}'; Debug.startLogToFile(); \
             Debug.message('hello'); Debug.notice('world');"
        );
        e.exec_script(&script, "t").unwrap();
    });

    let written = std::fs::read_to_string(&path).unwrap();
    assert!(
        written.contains("stale-marker"),
        "without the clear flag the file is appended to: {written:?}"
    );
    assert!(written.contains(" hello\r\n"), "log: {written:?}");
    assert!(written.contains(" world\r\n"), "log: {written:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Reference: with an empty `logLocation` there is no stream — the holder
/// records the failed open and writes nothing
/// (`reference/cpp/core/utils/DebugIntf.cpp:247-250`) — but
/// `TVPStartLogToFile` still sets `TVPLoggingToFile`, clear flag or not
/// (`reference/cpp/core/utils/DebugIntf.cpp:471-502`), so file logging is
/// already enabled when a location appears later: lines logged before that
/// are lost, the first line after it (re)opens the stream
/// (`reference/cpp/core/utils/DebugIntf.cpp:303-306`).
#[test]
fn start_log_to_file_without_a_location_has_no_stream_but_still_enables() {
    let _lock = vm_lock();
    let dir = std::env::temp_dir().join(format!(
        "krkr_debug_noloc_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("krkr.console.log");
    let literal = dir.to_string_lossy().replace('\\', "/");

    with_vm(|e| {
        // The clear flag is accepted even though no stream can exist yet.
        e.exec_script("Debug.startLogToFile(true); Debug.message('early');", "t")
            .unwrap();
        assert!(
            !path.exists(),
            "no log location means no stream is ever opened"
        );
        // Setting a location later reopens the stream: the logging flag was
        // already taken by `startLogToFile(true)`, so this line is written.
        e.exec_script(
            &format!("Debug.logLocation = '{literal}'; Debug.message('late');"),
            "t",
        )
        .unwrap();
    });

    let written = std::fs::read_to_string(&path).unwrap();
    assert!(
        !written.contains("early"),
        "lines logged while there was no stream are gone: {written:?}"
    );
    assert!(written.contains(" late\r\n"), "log: {written:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn console_and_controller_are_singletons_with_a_visible_flag() {
    let _lock = vm_lock();
    with_vm(|e| {
        // Each property returns a stable singleton object; the two differ.
        assert_eq!(
            e.eval("Debug.console === Debug.console", "t").unwrap(),
            TjsValue::Integer(1)
        );
        assert_eq!(
            e.eval("Debug.controller === Debug.controller", "t")
                .unwrap(),
            TjsValue::Integer(1)
        );
        assert_eq!(
            e.eval("Debug.console === Debug.controller", "t").unwrap(),
            TjsValue::Integer(0)
        );
        // `visible` defaults to false and is independent per object.
        assert_eq!(
            e.eval("Debug.console.visible", "t").unwrap(),
            TjsValue::Integer(0)
        );
        assert_eq!(
            e.eval("Debug.controller.visible", "t").unwrap(),
            TjsValue::Integer(0)
        );
        e.exec_script("Debug.console.visible = true;", "t").unwrap();
        assert_eq!(
            e.eval("Debug.console.visible", "t").unwrap(),
            TjsValue::Integer(1)
        );
        assert_eq!(
            e.eval("Debug.controller.visible", "t").unwrap(),
            TjsValue::Integer(0)
        );
        e.exec_script("Debug.controller.visible = true;", "t")
            .unwrap();
        assert_eq!(
            e.eval("Debug.controller.visible", "t").unwrap(),
            TjsValue::Integer(1)
        );
        // A script-created instance shares the DebugState visibility flags.
        e.exec_script("var c = new Console(); c.visible = false;", "t")
            .unwrap();
        assert_eq!(
            e.eval("Debug.console.visible", "t").unwrap(),
            TjsValue::Integer(0)
        );
        assert_eq!(
            e.eval("Debug.controller.visible", "t").unwrap(),
            TjsValue::Integer(1)
        );
    });
}
