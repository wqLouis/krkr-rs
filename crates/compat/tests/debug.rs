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

#[test]
fn message_and_notice_record_into_the_ring_buffer() {
    let _lock = vm_lock();
    with_vm(|e| {
        e.exec_script("Debug.message('one'); Debug.notice('two');", "t")
            .unwrap();
        assert_eq!(
            e.eval("Debug.getLastLog()", "t").unwrap(),
            TjsValue::String("two".into())
        );
        // Up to N most-recent entries, in chronological order, joined by \n.
        assert_eq!(
            e.eval("Debug.getLastLog(2)", "t").unwrap(),
            TjsValue::String("one\ntwo".into())
        );
        // An empty buffer reads back as the empty string.
        e.exec_script("Debug.message('x');", "t").unwrap();
        assert_eq!(
            e.eval("Debug.getLastLog(0)", "t").unwrap(),
            TjsValue::String(String::new())
        );
    });
}

#[test]
fn message_joins_multiple_arguments() {
    let _lock = vm_lock();
    with_vm(|e| {
        e.exec_script("Debug.message('a', 1, 'b');", "t").unwrap();
        assert_eq!(
            e.eval("Debug.getLastLog()", "t").unwrap(),
            TjsValue::String("a, 1, b".into())
        );
        assert!(e.eval("Debug.message()", "t").is_err());
        assert!(e.eval("Debug.notice()", "t").is_err());
    });
}

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
            TjsValue::String("one".into())
        );
        assert_eq!(
            e.eval("got[1]", "t").unwrap(),
            TjsValue::String("two".into())
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
    });
}

#[test]
fn non_object_handler_argument_is_ignored() {
    let _lock = vm_lock();
    with_vm(|e| {
        e.exec_script(
            "Debug.addLoggingHandler(42); Debug.message('still works');",
            "t",
        )
        .unwrap();
        assert_eq!(
            e.eval("Debug.getLastLog()", "t").unwrap(),
            TjsValue::String("still works".into())
        );
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
    let path = dir.join("debug.log");
    let literal = path.to_string_lossy().replace('\\', "/");

    with_vm(|e| {
        let script = format!(
            "Debug.startLogToFile('{literal}'); Debug.message('hello'); Debug.notice('world');"
        );
        e.exec_script(&script, "t").unwrap();
    });

    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.contains("hello"), "log: {written:?}");
    assert!(written.contains("world"), "log: {written:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
