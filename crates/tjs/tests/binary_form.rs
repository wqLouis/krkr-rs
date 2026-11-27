//! The structured binary form against the engine's recorded bytes.
//!
//! `tests/fixtures/binary_form.txt` holds the bytes the engine wrote for a
//! one-item array holding one value of each shape, and the value the same file
//! read back as; `crates/tjs/examples/binary_form_oracle.rs` records it. Two
//! checks read it:
//!
//! * the record is the form the reference writes — every row's bytes are
//!   asserted against the reference's own branch, cited above the row;
//! * the writer and the reader reproduce the record — the same value written
//!   here is the recorded byte string, and the recorded byte string reads back
//!   as the recorded value.
//!
//! A row is the whole file: the signature, the one-item array that frames the
//! value, and the value. A record that holds only the header is a failure, not
//! a pass: the table must be recorded before the check means anything.

use std::fs;
use std::path::PathBuf;
use std::rc::Rc;

use tjs::vm::runtime::array::ArrayObject;
use tjs::vm::runtime::dictionary::{
    BINARY_HEADER, ClassNames, Dictionary, read_structure, structure_binary, structure_text,
};
use tjs::vm::runtime::object::{Dispatch, MemberFlags};
use tjs::vm::runtime::octet::Octet;
use tjs::vm::runtime::string::TjsString;
use tjs::vm::runtime::variant::{Closure, Variant};

/// The value one row's frame holds.
///
/// The rows carry the value as a shape rather than as a value, so that the
/// same table builds both the file and the expectation.
enum Cell {
    /// The empty value.
    Void,
    /// The null object.
    Null,
    /// An integer.
    Integer(i64),
    /// A real.
    Real(f64),
    /// A text.
    Text(&'static str),
    /// A byte string.
    Octet(&'static [u8]),
    /// An array of integers.
    Integers(&'static [i64]),
    /// A dictionary whose members are written in the given order.
    Dictionary(&'static [(&'static str, Cell)]),
}

/// The rows: the name the record knows a row by, the file the reference writes
/// for it, and the value the frame holds.
///
/// The file of every row is the signature
/// (`reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.cpp:18-21`), the
/// one-item array's count tag (`tjsBinarySerializer.h:355-360`), then the
/// value's bytes; the citation above a row is the value's own branch.
const ROWS: &[(&str, &str, Cell)] = &[
    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.h:118-128
    // a negative value compares against the small-negative tag as +224, so it
    // never takes a tag of its own and is written as the one-byte width
    (
        "integer_minus_one",
        "4B4241443130300091D0FF",
        Cell::Integer(-1),
    ),
    (
        "integer_minus_32",
        "4B4241443130300091D0E0",
        Cell::Integer(-32),
    ),
    (
        "integer_minus_33",
        "4B4241443130300091D0DF",
        Cell::Integer(-33),
    ),
    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.h:159-163
    ("integer_zero", "4B424144313030009100", Cell::Integer(0)),
    ("integer_31", "4B42414431303000911F", Cell::Integer(31)),
    ("integer_32", "4B424144313030009120", Cell::Integer(32)),
    ("integer_127", "4B42414431303000917F", Cell::Integer(127)),
    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.h:164-168
    ("integer_128", "4B4241443130300091CC80", Cell::Integer(128)),
    ("integer_255", "4B4241443130300091CCFF", Cell::Integer(255)),
    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.h:169-175
    (
        "integer_256",
        "4B4241443130300091CD0001",
        Cell::Integer(256),
    ),
    (
        "integer_65535",
        "4B4241443130300091CDFFFF",
        Cell::Integer(65535),
    ),
    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.h:176-184
    (
        "integer_65536",
        "4B4241443130300091CE00000100",
        Cell::Integer(65536),
    ),
    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.h:253-266
    (
        "real_half",
        "4B4241443130300091CB0000000000000C40",
        Cell::Real(3.5),
    ),
    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.h:202-207
    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.h:232-235
    (
        "text_five",
        "4B4241443130300091A561006200630064006500",
        Cell::Text("abcde"),
    ),
    (
        "text_sixteen",
        "4B4241443130300091B03000310032003300340035003600370038003900610062006300640065006600",
        Cell::Text("0123456789abcdef"),
    ),
    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.h:355-360
    ("array_empty", "4B424144313030009190", Cell::Integers(&[])),
    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.h:328-333
    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.h:355-360
    (
        "dictionary_nested",
        "4B424144313030009181A16B009107",
        Cell::Dictionary(&[("k", Cell::Integers(&[7]))]),
    ),
    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.h:129-135
    (
        "integer_minus_129",
        "4B4241443130300091D17FFF",
        Cell::Integer(-129),
    ),
    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.h:136-144
    (
        "integer_minus_32769",
        "4B4241443130300091D2FF7FFFFF",
        Cell::Integer(-32769),
    ),
    // The eight-byte negative form has no row: the width check above the
    // eight-byte tag compares against `LONG_MIN`, which is the 64-bit minimum
    // on this host, so the oracle truncates a value below the 32-bit minimum
    // into the four-byte form instead of writing the eight-byte one. The
    // shipped engine, whose `long` is 32 bits, writes the eight-byte form, and
    // that is the form this writer writes — the one the value round-trips
    // through, which a recorded row here could not prove.
    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.h:145-158
    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.h:185-198
    (
        "integer_4294967296",
        "4B4241443130300091CF0000000001000000",
        Cell::Integer(4294967296),
    ),
    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.h:208-212
    (
        "text_thirty_two",
        "4B4241443130300091C42030003100320033003400350036003700380039006100620063006400650066006700680069006A006B006C006D006E006F007000710072007300740075007600",
        Cell::Text("0123456789abcdefghijklmnopqrstuv"),
    ),
    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.h:268-273
    (
        "octet_three",
        "4B4241443130300091D700ABFF",
        Cell::Octet(&[0x00, 0xAB, 0xFF]),
    ),
    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.h:274-280
    (
        "octet_six",
        "4B4241443130300091DA0600010203040506",
        Cell::Octet(&[1, 2, 3, 4, 5, 6]),
    ),
    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.h:328-333
    (
        "dictionary_empty",
        "4B424144313030009180",
        Cell::Dictionary(&[]),
    ),
    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.h:361-367
    (
        "array_sixteen",
        "4B4241443130300091DC1000000102030405060708090A0B0C0D0E0F",
        Cell::Integers(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]),
    ),
    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.cpp:35-39
    ("void", "4B4241443130300091C1", Cell::Void),
    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.h:382-386
    ("null", "4B4241443130300091C0", Cell::Null),
];
/// One recorded row.
struct Record {
    /// The name the record and this check know the row by.
    id: String,
    /// The file's bytes, hexadecimal.
    bytes: String,
    /// The value the file read back as, in the structured text form.
    value: String,
}

#[test]
fn the_record_is_the_form_the_reference_writes() {
    let records = records();
    assert_eq!(
        records
            .iter()
            .map(|record| record.id.as_str())
            .collect::<Vec<_>>(),
        ROWS.iter().map(|row| row.0).collect::<Vec<_>>(),
        "the record's rows are not the table this check knows; record it again with \
         `cargo run -p tjs --example binary_form_oracle`"
    );
    for (record, row) in records.iter().zip(ROWS.iter()) {
        let (id, expected, _cell) = row;
        assert_eq!(record.bytes, *expected, "the bytes of {id}");
    }
}

#[test]
fn the_binary_form_writes_and_reads_the_recorded_rows() {
    let records = records();
    for (record, row) in records.iter().zip(ROWS.iter()) {
        let (id, _, cell) = row;
        let bytes = structure_binary(&*frame(cell)).expect("the frame saves");
        assert_eq!(hex(&bytes), record.bytes, "the bytes {id} writes");
        let recorded = bytes_of(&record.bytes);
        assert_eq!(
            &recorded[..BINARY_HEADER.len()],
            &BINARY_HEADER[..],
            "the signature of {id}"
        );
        let value =
            read_structure(&recorded[BINARY_HEADER.len()..]).expect("the recorded bytes read");
        assert_eq!(text_of(&value), record.value, "the value {id} reads back");
    }
}

/// The value a cell names.
fn value(cell: &Cell) -> Variant {
    match cell {
        Cell::Void => Variant::Void,
        Cell::Null => Variant::Object(Closure::null()),
        Cell::Integer(number) => Variant::from(*number),
        Cell::Real(number) => Variant::from(*number),
        Cell::Text(text) => Variant::from(*text),
        Cell::Octet(bytes) => Variant::from(Octet::from_slice(bytes)),
        Cell::Integers(numbers) => {
            let array = Rc::new(ArrayObject::new());
            for number in *numbers {
                array
                    .append(Variant::from(*number))
                    .expect("the element is appended");
            }
            object_value(array)
        }
        Cell::Dictionary(members) => {
            let dictionary = Rc::new(Dictionary::new());
            for (key, member) in *members {
                dictionary
                    .prop_set(
                        &TjsString::from(*key),
                        MemberFlags::ENSURE,
                        &value(member),
                        None,
                    )
                    .expect("the member is written");
            }
            object_value(dictionary)
        }
    }
}

/// The one-item array whose save is a row's file.
fn frame(cell: &Cell) -> Rc<ArrayObject> {
    let array = Rc::new(ArrayObject::new());
    array.append(value(cell)).expect("the value is appended");
    array
}

/// A value that holds a container the check built, the way a script's value
/// holds one.
fn object_value<T: Dispatch + 'static>(container: Rc<T>) -> Variant {
    let handle: Rc<dyn Dispatch> = container;
    Variant::Object(Closure {
        object: Some(Rc::clone(&handle)),
        objthis: Some(handle),
    })
}

/// The structured text of the value the reader answered.
fn text_of(value: &Variant) -> String {
    let Variant::Object(closure) = value else {
        panic!("the recorded file describes an object: {value:?}");
    };
    let object = closure
        .object
        .as_deref()
        .expect("the recorded file describes an object");
    let text = structure_text(object, &ClassNames).expect("the value has a text form");
    String::from_utf16(text.as_utf16()).expect("the save text is valid UTF-16")
}

/// The records of the fixture; a record with no data line is a failure.
fn records() -> Vec<Record> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/binary_form.txt");
    let table = match fs::read_to_string(&path) {
        Ok(table) => table,
        Err(error) => panic!(
            "read {}: {error}; record it with `cargo run -p tjs --example binary_form_oracle`",
            path.display()
        ),
    };
    let mut records = Vec::new();
    for line in table.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split('\t');
        let (Some(id), Some(bytes), Some(value)) = (fields.next(), fields.next(), fields.next())
        else {
            panic!("the fixture line is not `id<TAB>hex<TAB>text`: {line:?}");
        };
        assert!(
            fields.next().is_none(),
            "the fixture line has more than three fields: {line:?}"
        );
        records.push(Record {
            id: id.to_owned(),
            bytes: bytes.to_owned(),
            value: decode(value),
        });
    }
    assert!(
        !records.is_empty(),
        "the binary-form fixture records nothing; record it with \
         `cargo run -p tjs --example binary_form_oracle`"
    );
    records
}

/// The bytes a record's hexadecimal form spells.
fn bytes_of(hex: &str) -> Vec<u8> {
    assert!(
        hex.len().is_multiple_of(2),
        "the recorded bytes are not whole: {hex:?}"
    );
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for pair in hex.as_bytes().as_chunks::<2>().0 {
        let pair = std::str::from_utf8(pair).unwrap_or_else(|error| {
            panic!("the recorded bytes are not hexadecimal: {error}");
        });
        let byte = u8::from_str_radix(pair, 16).unwrap_or_else(|error| {
            panic!("the recorded byte {pair:?} is not hexadecimal: {error}")
        });
        bytes.push(byte);
    }
    bytes
}

/// The bytes as the record spells them.
fn hex(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push_str(&format!("{byte:02X}"));
    }
    hex
}

/// The text a record's one-line form carries.
fn decode(encoded: &str) -> String {
    let mut decoded = String::with_capacity(encoded.len());
    let mut characters = encoded.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            decoded.push(character);
            continue;
        }
        match characters.next() {
            Some('\\') => decoded.push('\\'),
            Some('r') => decoded.push('\r'),
            Some('n') => decoded.push('\n'),
            Some('t') => decoded.push('\t'),
            Some(other) => panic!("the record holds an unknown escape: {other:?}"),
            None => panic!("the record ends with a backslash: {encoded:?}"),
        }
    }
    decoded
}
