//! The member map against the engine's recorded enumeration order.
//!
//! `tests/fixtures/member_order.txt` holds the order the engine enumerates a
//! dictionary in; the ignored test
//! `vm::runtime::member::tests::record_member_order` records it. The check
//! below replays the same names through
//! `tjs::vm::runtime::member::MemberMap` and fails on any difference, so the
//! order the map produces is the engine's order and not only its own idea of
//! one.
//!
//! A record that holds only the header is a failure, not a pass: the table
//! must be recorded before the check means anything.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use tjs::vm::runtime::member::MemberMap;
use tjs::vm::runtime::string::TjsString;

/// The names the recorder's dictionary inserts, in the order it inserts them.
const NAMES: [&str; 12] = [
    "key25",
    "k27",
    "v4",
    "e2",
    "key10",
    "at10",
    "gamma",
    "width",
    "key15",
    "key47",
    "member_32",
    "left",
];

/// The names the second record deletes again.
const REMOVED: [&str; 2] = ["key10", "width"];

/// The name the second record writes back after deleting it.
const REINSERTED: &str = "key10";

/// The name the second record adds after the deletions.
const ADDED: &str = "at0";

/// The probes the record must hold, in the recorder's order.
const PROBES: [&str; 2] = ["insert", "remove"];

/// One recorded order.
struct Record {
    /// The probe's name.
    probe: String,
    /// The member names, in the order the engine enumerated them.
    order: Vec<String>,
}

#[test]
fn the_member_map_reproduces_the_recorded_enumeration_order() {
    // the first eight names were chosen so that their hashes share one bucket
    // of the fresh eight-bucket array, which is what makes the record exercise
    // the primary slot, the chain and the growth
    for name in &NAMES[..8] {
        assert_eq!(TjsString::from(*name).member_hash() & 7, 0, "{name}");
    }
    let records = recorded_orders();
    let mut seen = Vec::with_capacity(records.len());
    for record in &records {
        let map = match record.probe.as_str() {
            "insert" => build(false),
            "remove" => build(true),
            other => panic!("the check has no replay for the recorded probe: {other}"),
        };
        let order: Vec<String> = map.iter().map(|(name, _)| name.to_string()).collect();
        let expected: BTreeSet<String> = expected_names(&record.probe).into_iter().collect();
        assert_eq!(
            order.iter().cloned().collect::<BTreeSet<String>>(),
            expected,
            "probe {}: the record holds names the check does not replay",
            record.probe
        );
        assert_eq!(order, record.order, "probe {}: the order", record.probe);
        if record.probe == "insert" {
            let insertion: Vec<String> = NAMES.iter().map(|name| (*name).to_owned()).collect();
            assert_ne!(
                record.order, insertion,
                "the record is the insertion order: the names exercise no collision"
            );
        }
        seen.push(record.probe.as_str());
    }
    assert_eq!(
        seen.as_slice(),
        PROBES,
        "the record's probes are not the table this check knows; record it again with \
         `cargo test -p tjs --lib vm::runtime::member::tests::record_member_order -- --ignored --nocapture`"
    );
}

/// The map the recorder's script builds, replayed.
fn build(removals: bool) -> MemberMap<u32> {
    let mut map = MemberMap::new();
    for (index, name) in NAMES.iter().enumerate() {
        assert_eq!(map.insert(TjsString::from(*name), index as u32), None);
    }
    if removals {
        for name in REMOVED {
            assert!(
                map.remove(&TjsString::from(name)),
                "the name {name} is absent"
            );
        }
        assert_eq!(map.insert(TjsString::from(REINSERTED), 99), None);
        assert_eq!(map.insert(TjsString::from(ADDED), 100), None);
    }
    map
}

/// The names a probe's map holds.
fn expected_names(probe: &str) -> Vec<String> {
    match probe {
        "insert" => NAMES.iter().map(|name| (*name).to_owned()).collect(),
        "remove" => {
            let mut names: Vec<String> = NAMES
                .iter()
                .map(|name| (*name).to_owned())
                .filter(|name| !REMOVED.contains(&name.as_str()))
                .collect();
            names.push(REINSERTED.to_owned());
            names.push(ADDED.to_owned());
            names
        }
        other => panic!("the check has no names for the recorded probe: {other}"),
    }
}

/// The records of the fixture; a record with no data line is a failure.
fn recorded_orders() -> Vec<Record> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/member_order.txt");
    let table = fs::read_to_string(&path).expect("read the member order fixture");
    let mut records = Vec::new();
    for line in table.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((probe, order)) = line.split_once('\t') else {
            panic!("the fixture line is not `probe<TAB>order`: {line:?}");
        };
        let order: Vec<String> = if order.is_empty() {
            Vec::new()
        } else {
            order.split(',').map(str::to_owned).collect()
        };
        records.push(Record {
            probe: probe.to_owned(),
            order,
        });
    }
    assert!(
        !records.is_empty(),
        "the member order fixture records nothing; record it with \
         `cargo test -p tjs --lib vm::runtime::member::tests::record_member_order -- --ignored --nocapture`"
    );
    records
}
