//! The name `Config` against the engine's recorded answer.
//!
//! `tests/fixtures/config_members.txt` holds what the engine answered when it
//! was asked for `Config`, for the settings a game's configuration script
//! usually defines, and for a `Config` dictionary a script creates;
//! `crates/tjs/examples/config_oracle.rs` records it. The check pins the
//! native table empty and the recorded answers to their kinds.
//!
//! A record that holds only the header is a failure, not a pass: the table
//! must be recorded before the check means anything.

use std::fs;
use std::path::PathBuf;

use tjs::vm::runtime::natives::config::{ConfigMember, EXPECTED_NAMES};
use tjs::vm::runtime::string::TjsString;

/// The probes the record must hold, in the recorder's order.
const PROBES: &[&str] = &[
    "surface_config",
    "surface_script_config",
    "surface_screen_width",
    "surface_throttle",
    "deferred_surface_math_generator",
    "value_script_config_screen_width",
    "value_global_config_screen_width",
];

// reference/krkr2/cpp/core/tjs2/tjs.cpp:139-191 — the global object registers
// Array, Dictionary, Date, Math, RandomGenerator, Exception and RegExp, and no
// Config class; the settings a game's Config.tjs defines are a script's.
#[test]
fn the_native_config_table_is_empty_and_the_name_is_script_owned() {
    let records = recorded_answers();
    let seen: Vec<&str> = records.iter().map(|record| record.probe.as_str()).collect();
    assert_eq!(
        seen.as_slice(),
        PROBES,
        "the record's probes are not the table this check knows; record it again with \
         `cargo run -p tjs --example config_oracle`"
    );
    assert!(
        ConfigMember::ALL.is_empty(),
        "the engine has no Config class, so the native table must stay empty"
    );
    for name in EXPECTED_NAMES {
        assert!(
            ConfigMember::lookup(&TjsString::from(*name)).is_none(),
            "the table publishes {name}, which is not a native member"
        );
    }
    check(&records, "surface_config", "String", "undefined");
    check(&records, "surface_script_config", "String", "Object");
    check(&records, "surface_screen_width", "String", "undefined");
    check(&records, "surface_throttle", "String", "undefined");
    check(
        &records,
        "deferred_surface_math_generator",
        "String",
        "Object",
    );
    check(
        &records,
        "value_script_config_screen_width",
        "Integer",
        "1280",
    );
    check(
        &records,
        "value_global_config_screen_width",
        "Integer",
        "1280",
    );
}

/// Checks one probe's recorded kind and text.
fn check(records: &[Record], probe: &str, kind: &str, text: &str) {
    let record = records
        .iter()
        .find(|record| record.probe == probe)
        .unwrap_or_else(|| panic!("the record does not hold {probe}"));
    assert_eq!(record.kind, kind, "probe {probe}: the kind");
    assert_eq!(record.text, text, "probe {probe}: the text");
}

/// One recorded answer.
struct Record {
    /// The probe's name.
    probe: String,
    /// The kind `typeof` reports for the answer.
    kind: String,
    /// The answer as text.
    text: String,
}

/// The records of the fixture; a record with no data line is a failure.
fn recorded_answers() -> Vec<Record> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config_members.txt");
    let table = fs::read_to_string(&path).expect("read the config member fixture");
    let mut records = Vec::new();
    for line in table.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split('\t');
        let (Some(probe), Some(kind), Some(text)) = (fields.next(), fields.next(), fields.next())
        else {
            panic!("the fixture line is not `probe<TAB>kind<TAB>text`: {line:?}");
        };
        assert!(
            fields.next().is_none(),
            "the fixture line has more than three fields: {line:?}"
        );
        records.push(Record {
            probe: probe.to_owned(),
            kind: kind.to_owned(),
            text: text.to_owned(),
        });
    }
    assert!(
        !records.is_empty(),
        "the config member fixture records nothing; record it with \
         `cargo run -p tjs --example config_oracle`"
    );
    records
}
