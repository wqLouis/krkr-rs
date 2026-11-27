//! The save/load members and the structured form against the engine's
//! recorded answers.
//!
//! `tests/fixtures/stream_members.txt` holds what the engine answered for the
//! five array members that reach a stream, for the structured form both
//! containers write, and for the refusals each of them answers;
//! `crates/tjs/examples/stream_oracle.rs` records it. The check below runs the
//! same probes through `tjs::vm::runtime::{array, dictionary, stream}` against
//! an in-memory host and compares every answer, so the seam a save writes
//! through, the walker's text and byte forms and the refusals a member gives
//! are the engine's and not only this module's idea of them.
//!
//! A save's stream is named by the probe, not by the check: the recorder wrote
//! a file under its own temporary directory, and the check keys the in-memory
//! host by the same name, so the name never reaches the record. A text save is
//! compared as text (the record carries it escaped, losslessly) and a binary
//! save as bytes.
//!
//! The four members that write or read a stream answer the receiver, and
//! `loadStruct` the value it read. The dispatch surface here cannot hand a
//! receiver back — a call is made on a borrowed container — so it answers the
//! empty value. Those rows are compared on the record, and the surface's own
//! answer is asserted beside it.
//!
//! A record that holds only the header is a failure, not a pass: the table must
//! be recorded before the check means anything.

use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard};

use tjs::vm::runtime::array::ArrayObject;
use tjs::vm::runtime::dictionary::{BINARY_HEADER, Dictionary, structure_binary};
use tjs::vm::runtime::object::{Dispatch, MemberFlags, ObjectError};
use tjs::vm::runtime::octet::Octet;
use tjs::vm::runtime::stream::{self, MemoryStreams, SharedHost};
use tjs::vm::runtime::string::TjsString;
use tjs::vm::runtime::variant::{Closure, Variant};

/// The probes the record must hold, in the recorder's order.
const PROBES: &[&str] = &[
    "save_show_text",
    "save_show_text_reloaded",
    "save_show_lines",
    "load_lines_count",
    "load_lines_first",
    "load_lines_real",
    "load_lines_octet",
    "save_lines_again",
    "save_show_binary",
    "load_struct_bin_count",
    "load_struct_bin_huge",
    "load_struct_bin_real",
    "load_struct_bin_real_kind",
    "load_struct_bin_octet",
    "load_struct_bin_null",
    "save_struct_bin_again",
    "save_saved_frame",
    "save_plain_frame",
    "save_plain_frame_binary",
    "save_plain_reload",
    "save_copy_binary",
    "save_tiny_binary",
    "load_absent",
    "load_struct_absent",
    "assign_struct_mismatch",
    "assign_struct_null",
    "load_struct_not_binary",
    "load_struct_truncated",
    "save_answer_kind",
    "save_answer_self",
    "save_struct_answer_kind",
    "save_struct_answer_self",
    "load_answer_kind",
    "load_answer_self",
    "load_struct_answer_kind",
    "load_struct_answer_self",
    "assign_answer_kind",
];

/// The name no host serves, the same one the recorder asks for.
const ABSENT: &str = "@tjs-stream-fixture-absent";

/// The name a structured text save of the probe array writes.
const SHOW_TEXT: &str = "show.txt";
/// The name the recorder's round trip writes the same structure to.
const SHOW_AGAIN: &str = "show-again.txt";
/// The name a line save of the probe array writes.
const SHOW_LINES: &str = "lines.txt";
/// The name the line save of the loaded lines writes.
const LINES_AGAIN: &str = "lines-again.txt";
/// The name a structured binary save of the probe array writes.
const SHOW_BIN: &str = "show.bin";
/// The name the binary save of the loaded array writes.
const SHOW_AGAIN_BIN: &str = "show-again.bin";
/// The name a structured text save of the recursive dictionary's frame writes.
const SAVED_FRAME: &str = "saved.txt";
/// The name a structured text save of the plain dictionary's frame writes.
const PLAIN_TEXT: &str = "plain.txt";
/// The name a structured binary save of the plain dictionary's frame writes.
const PLAIN_BIN: &str = "plain.bin";
/// The name the binary save of the reloaded plain dictionary's frame writes.
const PLAIN_AGAIN_BIN: &str = "plain-again.bin";
/// The name a structured binary save of the array the copy made writes.
const COPY_BIN: &str = "copy.bin";
/// The name a structured binary save of the one-element array writes.
const TINY_BIN: &str = "tiny.bin";
/// The name the truncated binary form is stored under.
const TINY_CUT: &str = "tiny-cut.bin";

/// The name the plain dictionary's own text save writes. A dictionary's
/// structured members belong to its class object, which the engine does not
/// publish on an instance, so the check reaches the dictionary's own form
/// through the frame the recorder saved and writes this name for itself.
const PLAIN_OWN_TEXT: &str = "plain-own.txt";

/// The name the plain dictionary's own binary save writes.
const PLAIN_OWN_BIN: &str = "plain-own.bin";

/// The name the hidden member's text save writes.
const HIDDEN_TEXT: &str = "hidden.txt";

/// One recorded answer.
struct Record {
    /// The probe's name.
    probe: String,
    /// What the text is: `text`, `bytes`, `!raise`, `answer`, or a `typeof`.
    kind: String,
    /// The answer as the record spells it.
    text: String,
}

/// The VM keeps the installed host, and the containers are not `Sync`; one
/// check at a time.
static VM_LOCK: Mutex<()> = Mutex::new(());

/// The lock that serializes this file's checks.
fn vm_lock() -> MutexGuard<'static, ()> {
    match VM_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// The host the check installs, restored when the check ends.
struct Host {
    /// The store every save in this check writes to.
    streams: Arc<MemoryStreams>,
    /// The host the installation replaced.
    previous: Option<SharedHost>,
}

impl Host {
    /// Installs an empty store and remembers the host it replaced.
    fn install() -> Self {
        let streams = Arc::new(MemoryStreams::new());
        let previous = stream::install(shared(Arc::clone(&streams)));
        Self { streams, previous }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        stream::uninstall();
        if let Some(previous) = self.previous.take() {
            stream::install(previous);
        }
    }
}

/// The containers every probe reads, built the way the recorder's script
/// builds them.
struct World {
    /// The array whose values cover every form the walker writes.
    show: Rc<ArrayObject>,
    /// The dictionary with no recursion, for the round trip and the copy.
    plain: Rc<Dictionary>,
    /// The one-element array the truncated form is cut from.
    tiny: Rc<ArrayObject>,
    /// The value handle that names the dictionary with a member naming itself.
    saved_handle: Rc<dyn Dispatch>,
    /// The value handle that names `plain`.
    plain_handle: Rc<dyn Dispatch>,
}

impl World {
    /// The containers the recorder's script builds, member for member.
    fn build() -> Self {
        let show = Rc::new(ArrayObject::new());
        append(&show, Variant::from("one"));
        append(&show, Variant::from(2i64));
        append(&show, Variant::from(-9i64));
        append(&show, Variant::from(3.5f64));
        append(&show, Variant::from(1_234_567_890_123i64));
        append(&show, Variant::from(Octet::from_slice(&[0x00, 0xab])));
        append(&show, Variant::Void);
        append(&show, Variant::Object(Closure::null()));
        append(&show, Variant::from("a,b\r\n\"q\""));
        let nested = Rc::new(ArrayObject::new());
        append(&nested, Variant::from(1i64));
        append(&nested, Variant::from("two"));
        let deep = Rc::new(Dictionary::new());
        write(&deep, "k", Variant::from(1i64));
        append(&nested, object(&handle(Rc::clone(&deep))));
        append(&show, object(&handle(Rc::clone(&nested))));
        let map = Rc::new(Dictionary::new());
        write(&map, "key", Variant::from("value"));
        write(&map, "num", Variant::from(7i64));
        append(&show, object(&handle(Rc::clone(&map))));
        let empty_array = Rc::new(ArrayObject::new());
        append(&show, object(&handle(Rc::clone(&empty_array))));
        let empty_map = Rc::new(Dictionary::new());
        append(&show, object(&handle(Rc::clone(&empty_map))));

        let saved = Rc::new(Dictionary::new());
        let saved_handle = handle(Rc::clone(&saved));
        write(&saved, "integer", Variant::from(42i64));
        write(&saved, "text", Variant::from("a,b\r\n\"q\""));
        let nested = Rc::new(Dictionary::new());
        write(&nested, "inner", Variant::from(1i64));
        write(&saved, "nested", object(&handle(Rc::clone(&nested))));
        let list = Rc::new(ArrayObject::new());
        append(&list, Variant::from(1i64));
        append(&list, Variant::from(2i64));
        write(&saved, "list", object(&handle(Rc::clone(&list))));
        let empty = Rc::new(Dictionary::new());
        write(&saved, "empty", object(&handle(Rc::clone(&empty))));
        write(&saved, "self", object(&saved_handle));

        let plain = Rc::new(Dictionary::new());
        let plain_handle = handle(Rc::clone(&plain));
        write(&plain, "name", Variant::from("plain"));
        write(&plain, "count", Variant::from(3i64));
        write(&plain, "real", Variant::from(0.5f64));
        write(
            &plain,
            "octet",
            Variant::from(Octet::from_slice(&[0x01, 0x02])),
        );
        let list = Rc::new(ArrayObject::new());
        append(&list, Variant::from(1i64));
        append(&list, Variant::from("two"));
        write(&plain, "list", object(&handle(Rc::clone(&list))));
        let map = Rc::new(Dictionary::new());
        write(&map, "deep", Variant::from(1i64));
        write(&plain, "map", object(&handle(Rc::clone(&map))));

        let tiny = Rc::new(ArrayObject::new());
        append(&tiny, Variant::from("abcdef"));

        Self {
            show,
            plain,
            tiny,
            saved_handle,
            plain_handle,
        }
    }
}

// reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1114-1155
// reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:417-447
// reference/krkr2/cpp/core/tjs2/tjsUtils.cpp:175-238
// reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:252-299
#[test]
fn the_structured_text_form_is_the_recorded_text() {
    let _lock = vm_lock();
    let host = Host::install();
    let world = World::build();
    let record = recorded_answers();

    world
        .show
        .save_struct(&name(SHOW_TEXT), &plain_mode())
        .expect("the structured text save is served");
    let text = text_saved(&host.streams, SHOW_TEXT);
    expect(&record, "save_show_text", "text", &text);

    // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1117-1122 — an array opens
    // with `(const) [` and a line break.
    assert!(
        text.starts_with("(const) [\r\n"),
        "the save does not open an array: {text:?}"
    );
    // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1138-1148 — `,` and a line
    // break between two items, and a line break after the last one.
    assert!(
        text.contains(",\r\n"),
        "the items are not separated by a comma and a line break: {text:?}"
    );
    // reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:420-421 — a dictionary
    // opens with `(const) %[`.
    assert!(
        text.contains("(const) %["),
        "the save does not open a dictionary: {text:?}"
    );
    // reference/krkr2/cpp/core/tjs2/tjsUtils.cpp:208-221 — an octet is its
    // bytes in hexadecimal, uppercase and space separated.
    assert!(
        text.contains("<% 00 AB %>"),
        "the octet is not the walker's form: {text:?}"
    );
    // reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:252-299 — a real is its
    // hexadecimal form, then its decimal text in a comment.
    assert!(
        text.contains("0x1.C000000000000p1 /* 3.5 */"),
        "the real is not written in its hexadecimal form with a decimal comment: {text:?}"
    );
    // reference/krkr2/cpp/core/tjs2/tjsUtils.cpp:201-207 — a text is quoted
    // and escaped, so a comma, a line break and a quote in it survive.
    assert!(
        text.contains("\"a,b\\r\\n\\\"q\\\"\""),
        "the text with a comma, a line break and a quote is not escaped: {text:?}"
    );

    // The engine read the text it wrote back as the expression it is and saved
    // the structure again; the record holds that second save, and the check's
    // second save of the same array has to agree with it.
    world
        .show
        .save_struct(&name(SHOW_AGAIN), &plain_mode())
        .expect("the second save is served");
    expect(
        &record,
        "save_show_text_reloaded",
        "text",
        &text_saved(&host.streams, SHOW_AGAIN),
    );
    assert_eq!(
        decoded(&recorded(&record, "save_show_text").text),
        decoded(&recorded(&record, "save_show_text_reloaded").text),
        "the text the engine wrote does not read back to the same text"
    );
}

// reference/krkr2/cpp/core/tjs2/tjsArray.cpp:240-336
// reference/krkr2/cpp/core/tjs2/tjsArray.cpp:394-421
#[test]
fn the_line_form_writes_a_line_per_element_and_loads_it_back() {
    let _lock = vm_lock();
    let host = Host::install();
    let world = World::build();
    let record = recorded_answers();

    world
        .show
        .save(&name(SHOW_LINES), &plain_mode())
        .expect("the line save is served");
    let saved = text_saved(&host.streams, SHOW_LINES);
    expect(&record, "save_show_lines", "text", &saved);
    // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:412-420 — every element ends
    // its line, whether or not it wrote a text.
    assert!(
        saved.ends_with("\r\n"),
        "the line save does not end the last element's line: {saved:?}"
    );

    let loaded = ArrayObject::new();
    loaded
        .load(&name(SHOW_LINES), &plain_mode())
        .expect("the line load is served");
    expect(
        &record,
        "load_lines_count",
        "Integer",
        &loaded.len().to_string(),
    );
    expect(
        &record,
        "load_lines_first",
        "String",
        &text_of(&element(&loaded, 0)),
    );
    expect(
        &record,
        "load_lines_real",
        "String",
        &text_of(&element(&loaded, 3)),
    );
    expect(
        &record,
        "load_lines_octet",
        "String",
        &text_of(&element(&loaded, 5)),
    );
    // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:304-318 — a line is allocated
    // as a text, whatever the element was before the save.
    assert_eq!(
        element(&loaded, 3).type_name(),
        "String",
        "a saved real does not load back as a text"
    );

    loaded
        .save(&name(LINES_AGAIN), &plain_mode())
        .expect("the second line save is served");
    expect(
        &record,
        "save_lines_again",
        "text",
        &text_saved(&host.streams, LINES_AGAIN),
    );
}

// reference/krkr2/cpp/core/tjs2/tjsArray.cpp:455-480
// reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1203-1219
// reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.cpp:22-27
#[test]
fn the_binary_form_is_the_recorded_bytes() {
    let _lock = vm_lock();
    let host = Host::install();
    let world = World::build();
    let record = recorded_answers();

    world
        .show
        .save_struct(&name(SHOW_BIN), &binary_mode())
        .expect("the binary save is served");
    let saved = bytes_saved(&host.streams, SHOW_BIN);
    expect(&record, "save_show_binary", "bytes", &hex(&saved));
    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.cpp:22-27 — the body
    // follows the eight-byte signature.
    assert!(
        saved.starts_with(&BINARY_HEADER),
        "the binary save does not start with the signature: {saved:?}"
    );

    let loaded = ArrayObject::new();
    loaded
        .load_struct(&name(SHOW_BIN), &plain_mode())
        .expect("the structured load is served");
    expect(
        &record,
        "load_struct_bin_count",
        "Integer",
        &loaded.len().to_string(),
    );
    expect(
        &record,
        "load_struct_bin_huge",
        "Integer",
        &text_of(&element(&loaded, 4)),
    );
    expect(
        &record,
        "load_struct_bin_real",
        "Real",
        &text_of(&element(&loaded, 3)),
    );
    expect(
        &record,
        "load_struct_bin_real_kind",
        "String",
        element(&loaded, 3).type_name(),
    );
    expect(
        &record,
        "load_struct_bin_octet",
        "String",
        element(&loaded, 5).type_name(),
    );
    expect(
        &record,
        "load_struct_bin_null",
        "String",
        element(&loaded, 7).type_name(),
    );

    loaded
        .save_struct(&name(SHOW_AGAIN_BIN), &binary_mode())
        .expect("the second binary save is served");
    let again = bytes_saved(&host.streams, SHOW_AGAIN_BIN);
    expect(&record, "save_struct_bin_again", "bytes", &hex(&again));
    assert_eq!(
        hex(&again),
        hex(&saved),
        "the binary form does not survive a structured round trip"
    );
    assert_eq!(
        recorded(&record, "save_show_binary").text,
        recorded(&record, "save_struct_bin_again").text,
        "the engine's bytes are not an identity across the round trip"
    );
}

// reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:52-120
// reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:133-184
// reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:567-700
// reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:468-474
// reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1158-1196
// reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1203-1260
// reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1265-1360
#[test]
fn the_dictionary_structure_is_the_recorded_structure() {
    let _lock = vm_lock();
    let host = Host::install();
    let world = World::build();
    let record = recorded_answers();

    // reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:120 — `loadStruct` ends
    // its declaration as a static member, and `saveStruct` (line 184) and
    // `assignStruct` (line 233) end theirs the same way: a dictionary's
    // structured members belong to the class object, so the reachable path is
    // an array holding the dictionary, and the array's walk reaches it.
    let saved_frame = frame(&world.saved_handle);
    saved_frame
        .save_struct(&name(SAVED_FRAME), &plain_mode())
        .expect("the recursive dictionary saves");
    expect(
        &record,
        "save_saved_frame",
        "text",
        &text_saved(&host.streams, SAVED_FRAME),
    );
    // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1161-1170 — a container
    // already on the way down is a null with the recursion comment.
    assert!(
        decoded(&recorded(&record, "save_saved_frame").text)
            .contains("null /* object recursion detected */"),
        "the member that names its own dictionary is not written as the walker's null"
    );

    let plain_frame = frame(&world.plain_handle);
    plain_frame
        .save_struct(&name(PLAIN_TEXT), &plain_mode())
        .expect("the plain dictionary saves");
    let recorded_frame = decoded(&recorded(&record, "save_plain_frame").text);
    expect(
        &record,
        "save_plain_frame",
        "text",
        &text_saved(&host.streams, PLAIN_TEXT),
    );
    // The dictionary's own members, against the structure the frame's record
    // holds: the frame is the array's text around the dictionary's own.
    let dictionary_text = unframe_text(&recorded_frame);
    world
        .plain
        .save_struct(&name(PLAIN_OWN_TEXT), &plain_mode())
        .expect("the dictionary's own text save is served");
    assert_eq!(
        text_saved(&host.streams, PLAIN_OWN_TEXT),
        dictionary_text,
        "the dictionary's own text is not the framed structure"
    );

    plain_frame
        .save_struct(&name(PLAIN_BIN), &binary_mode())
        .expect("the plain dictionary's binary save is served");
    let frame_bytes = bytes_saved(&host.streams, PLAIN_BIN);
    expect(
        &record,
        "save_plain_frame_binary",
        "bytes",
        &hex(&frame_bytes),
    );
    let dictionary_bytes = unframe_bytes(&frame_bytes);
    world
        .plain
        .save_struct(&name(PLAIN_OWN_BIN), &binary_mode())
        .expect("the dictionary's own binary save is served");
    assert_eq!(
        bytes_saved(&host.streams, PLAIN_OWN_BIN),
        dictionary_bytes,
        "the dictionary's own bytes are not the framed structure"
    );
    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.cpp:327-343 — the
    // reader accepts the dictionary-only body the writer produces. It inserts
    // the members it reads into a fresh dictionary, so the loaded form's order
    // is the order the record's own reload wrote, not the order the source was
    // written in: the two differ for this dictionary, and the record holds
    // both.
    let value = Dictionary::load_struct(&name(PLAIN_OWN_BIN), &plain_mode())
        .expect("the dictionary's own body loads");
    let loaded_dictionary = object_of(&value);
    let reloaded_bytes = unframe_bytes(&unhex(&recorded(&record, "save_plain_reload").text));
    assert_eq!(
        structure_binary(&**loaded_dictionary).expect("the loaded dictionary saves"),
        reloaded_bytes,
        "the loaded dictionary is not the structure the record's reload holds"
    );

    // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:369-372 — the array's
    // structured load answers the array it read, and saving it again writes
    // the same bytes.
    let reloaded = ArrayObject::new();
    reloaded
        .load_struct(&name(PLAIN_BIN), &plain_mode())
        .expect("the framed binary form loads");
    let inner = element(&reloaded, 0);
    let again = frame(object_of(&inner));
    again
        .save_struct(&name(PLAIN_AGAIN_BIN), &binary_mode())
        .expect("the reloaded dictionary saves");
    expect(
        &record,
        "save_plain_reload",
        "bytes",
        &hex(&bytes_saved(&host.streams, PLAIN_AGAIN_BIN)),
    );

    // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1265-1360 — an array's
    // structure is copied element for element, and a container element is
    // copied into a new container.
    let nested = Rc::new(ArrayObject::new());
    append(&nested, Variant::from(4i64));
    append(&nested, Variant::from(5i64));
    let source = Rc::new(ArrayObject::new());
    append(&source, Variant::from(1i64));
    append(&source, Variant::from("two"));
    append(&source, Variant::from(3.5f64));
    append(&source, object(&handle(Rc::clone(&nested))));
    let source_handle = handle(Rc::clone(&source));
    let destination = ArrayObject::new();
    destination
        .assign_struct(&object(&source_handle))
        .expect("the array assigns a structure");
    destination
        .save_struct(&name(COPY_BIN), &binary_mode())
        .expect("the copy saves");
    expect(
        &record,
        "save_copy_binary",
        "bytes",
        &hex(&bytes_saved(&host.streams, COPY_BIN)),
    );
    let copied_nested = element(&destination, 3);
    let original_nested = element(&source, 3);
    assert!(
        !Rc::ptr_eq(object_of(&copied_nested), object_of(&original_nested)),
        "the copy shares the source's nested array"
    );

    // reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:567-700 — the
    // dictionary's own assignment copies the same way: every member the source
    // holds comes along, and a member that is a container is a new container.
    let own = Dictionary::new();
    own.assign_struct(&object(&world.plain_handle))
        .expect("the dictionary assigns a structure");
    for member in ["name", "count", "real"] {
        let original = world
            .plain
            .prop_get(&name(member), MemberFlags::NONE, None)
            .expect("the source's member is read");
        let copy = own
            .prop_get(&name(member), MemberFlags::NONE, None)
            .expect("the copy's member is read");
        assert_eq!(
            text_of(&copy),
            text_of(&original),
            "the copy's member {member}"
        );
        assert_eq!(
            copy.type_name(),
            original.type_name(),
            "the kind of the copy's member {member}"
        );
    }
    let copy_octet = own
        .prop_get(&name("octet"), MemberFlags::NONE, None)
        .expect("the copy's byte string is read");
    assert_eq!(copy_octet.type_name(), "Octet", "the copied byte string");
    for member in ["list", "map"] {
        let original = world
            .plain
            .prop_get(&name(member), MemberFlags::NONE, None)
            .expect("the source's member is read");
        let copy = own
            .prop_get(&name(member), MemberFlags::NONE, None)
            .expect("the copy's member is read");
        assert!(
            !Rc::ptr_eq(object_of(&copy), object_of(&original)),
            "the copy shares the source's {member}"
        );
    }
    let copied_list = own
        .prop_get(&name("list"), MemberFlags::NONE, None)
        .expect("the copy's list is read");
    let copied_first = object_of(&copied_list)
        .prop_get(&name("0"), MemberFlags::NONE, None)
        .expect("the copied list's first element is read");
    assert_eq!(
        text_of(&copied_first),
        "1",
        "the copied list's first element"
    );

    // reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:611-617 — a hidden
    // member is skipped by the copy, as it is by a save.
    let secret = Rc::new(Dictionary::new());
    write(&secret, "seen", Variant::from(1i64));
    secret
        .prop_set(
            &name("tucked"),
            MemberFlags::ENSURE | MemberFlags::HIDDEN,
            &Variant::from(2i64),
            None,
        )
        .expect("the hidden member is written");
    let copied = Dictionary::new();
    copied
        .assign_struct(&object(&handle(Rc::clone(&secret))))
        .expect("the dictionary assigns the structure it holds");
    let tucked = copied
        .prop_get(&name("tucked"), MemberFlags::NONE, None)
        .expect("the copy answers the read");
    assert!(
        matches!(tucked, Variant::Void),
        "the copy holds the hidden member"
    );
    let seen = copied
        .prop_get(&name("seen"), MemberFlags::NONE, None)
        .expect("the copy's visible member is read");
    assert_eq!(text_of(&seen), "1", "the copy's visible member");

    // reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:468-474 — a hidden
    // member is skipped, through the same stream a visible member is written
    // through.
    let hidden = Dictionary::new();
    write(&hidden, "seen", Variant::from(1i64));
    hidden
        .prop_set(
            &name("tucked"),
            MemberFlags::ENSURE | MemberFlags::HIDDEN,
            &Variant::from(2i64),
            None,
        )
        .expect("the hidden member is written");
    hidden
        .save_struct(&name(HIDDEN_TEXT), &plain_mode())
        .expect("the hidden member's save is served");
    let text = text_saved(&host.streams, HIDDEN_TEXT);
    assert!(text.contains("\"seen\" => 1"), "{text:?}");
    assert!(!text.contains("tucked"), "{text:?}");
}

// reference/krkr2/cpp/core/tjs2/tjsArray.cpp:240-336
// reference/krkr2/cpp/core/tjs2/tjsArray.cpp:353-356
// reference/krkr2/cpp/core/tjs2/tjsArray.cpp:759-772
// reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:52-120
// reference/krkr2/cpp/core/base/MsgIntfInc.h:22
// reference/krkr2/cpp/core/tjs2/tjsError.h:263
// reference/krkr2/cpp/core/tjs2/tjsError.h:267
// reference/krkr2/cpp/core/tjs2/tjsError.h:271-272
// reference/krkr2/cpp/core/tjs2/tjsError.h:330
#[test]
fn a_refused_member_answers_the_recorded_message() {
    let _lock = vm_lock();
    let host = Host::install();
    let world = World::build();
    let record = recorded_answers();

    let absent = name(ABSENT);
    let error = ArrayObject::new()
        .load(&absent, &plain_mode())
        .expect_err("a name the host cannot open is refused");
    expect_raised(&record, "load_absent", &error.to_string());

    let error = ArrayObject::new()
        .load_struct(&absent, &plain_mode())
        .expect_err("a name the host cannot open is refused");
    expect_raised(&record, "load_struct_absent", &error.to_string());

    let error = ArrayObject::new()
        .assign_struct(&object(&world.saved_handle))
        .expect_err("a dictionary is not a source an array assigns");
    expect_raised(&record, "assign_struct_mismatch", &error.to_string());

    let error = ArrayObject::new()
        .assign_struct(&Variant::Object(Closure::null()))
        .expect_err("the null object is not a structure");
    expect_raised(&record, "assign_struct_null", &error.to_string());

    // A text save is not the binary form the structured load reads. The
    // store keeps a text and a block of bytes in two tables, while the
    // engine's file is one stream of the text's bytes, so the bytes the text
    // stream is made of are put where a binary read of the same name finds
    // them.
    world
        .show
        .save_struct(&name(SHOW_TEXT), &plain_mode())
        .expect("the text save is served");
    let saved_text = text_saved(&host.streams, SHOW_TEXT);
    host.streams
        .set_bytes(SHOW_TEXT, &text_file_bytes(&saved_text));
    let error = ArrayObject::new()
        .load_struct(&name(SHOW_TEXT), &plain_mode())
        .expect_err("a text form is not a structured body");
    expect_raised(&record, "load_struct_not_binary", &error.to_string());

    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.cpp:147-170 — a body that
    // ends inside a value is the read error; here the last byte is removed.
    world
        .tiny
        .save_struct(&name(TINY_BIN), &binary_mode())
        .expect("the tiny save is served");
    let bytes = bytes_saved(&host.streams, TINY_BIN);
    let Some((_last, rest)) = bytes.split_last() else {
        panic!("the tiny save holds no byte to remove");
    };
    host.streams.set_bytes(TINY_CUT, rest);
    let error = ArrayObject::new()
        .load_struct(&name(TINY_CUT), &plain_mode())
        .expect_err("a truncated body is refused");
    expect_raised(&record, "load_struct_truncated", &error.to_string());
}

// reference/krkr2/cpp/core/tjs2/tjsArray.cpp:334-335
// reference/krkr2/cpp/core/tjs2/tjsArray.cpp:369-372
// reference/krkr2/cpp/core/tjs2/tjsArray.cpp:427-428
// reference/krkr2/cpp/core/tjs2/tjsArray.cpp:477-478
// reference/krkr2/cpp/core/tjs2/tjsArray.cpp:759-772
#[test]
fn the_member_answers_are_the_recorded_answers() {
    let _lock = vm_lock();
    let host = Host::install();
    let world = World::build();
    let record = recorded_answers();

    world
        .show
        .save_struct(&name(SHOW_TEXT), &plain_mode())
        .expect("the text save is served");
    world
        .show
        .save(&name(SHOW_LINES), &plain_mode())
        .expect("the line save is served");
    world
        .show
        .save_struct(&name(SHOW_BIN), &binary_mode())
        .expect("the binary save is served");
    // the members answered below really wrote their streams
    assert!(
        !text_saved(&host.streams, SHOW_TEXT).is_empty(),
        "the text save wrote no text"
    );
    assert!(
        !bytes_saved(&host.streams, SHOW_BIN).is_empty(),
        "the binary save wrote no bytes"
    );

    // The engine answers the receiver for the four members that write or read
    // a stream, and the value it read for `loadStruct`.
    expect(&record, "save_answer_kind", "String", "Object");
    expect(&record, "save_answer_self", "Integer", "1");
    expect(&record, "save_struct_answer_kind", "String", "Object");
    expect(&record, "save_struct_answer_self", "Integer", "1");
    expect(&record, "load_answer_kind", "String", "Object");
    expect(&record, "load_answer_self", "Integer", "1");
    expect(&record, "load_struct_answer_kind", "String", "Object");
    expect(&record, "load_struct_answer_self", "Integer", "1");

    // The dispatch surface answers the empty value for those four: a call is
    // made on a borrowed container, which has no handle to answer with, and
    // the receiver is the call machinery's.
    let work = ArrayObject::new();
    let answer = call(
        &work,
        "save",
        &[Variant::from(SHOW_TEXT), Variant::from("")],
    )
    .expect("the save member is served");
    assert_eq!(answer.type_name(), "void");
    let answer = call(
        &work,
        "saveStruct",
        &[Variant::from(SHOW_TEXT), Variant::from("")],
    )
    .expect("the saveStruct member is served");
    assert_eq!(answer.type_name(), "void");
    let answer = call(
        &work,
        "load",
        &[Variant::from(SHOW_LINES), Variant::from("")],
    )
    .expect("the load member is served");
    assert_eq!(answer.type_name(), "void");
    let answer = call(
        &work,
        "loadStruct",
        &[Variant::from(SHOW_BIN), Variant::from("")],
    )
    .expect("the loadStruct member is served");
    assert_eq!(answer.type_name(), "void");

    // `assignStruct` answers nothing on either side.
    let one = Rc::new(ArrayObject::new());
    append(&one, Variant::from(1i64));
    let one_handle = handle(Rc::clone(&one));
    let destination = ArrayObject::new();
    let answer = call(&destination, "assignStruct", &[object(&one_handle)])
        .expect("the assignStruct member is served");
    assert_eq!(answer.type_name(), "void");
    expect(&record, "assign_answer_kind", "String", "void");
}

#[test]
fn the_record_holds_the_probes_this_check_knows() {
    let record = recorded_answers();
    let seen: Vec<&str> = record.iter().map(|row| row.probe.as_str()).collect();
    assert_eq!(
        seen.as_slice(),
        PROBES,
        "the record's probes are not the table this check knows; record it again with \
         `cargo run -p tjs --example stream_oracle`"
    );
}

/// The records of the fixture; a record with no data line is a failure.
fn recorded_answers() -> Vec<Record> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stream_members.txt");
    let table = match fs::read_to_string(&path) {
        Ok(table) => table,
        Err(error) => panic!(
            "read {}: {error}; record it with `cargo run -p tjs --example stream_oracle`",
            path.display()
        ),
    };
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
        "the stream fixture records nothing; record it with \
         `cargo run -p tjs --example stream_oracle`"
    );
    records
}

/// The record one probe holds.
fn recorded<'a>(record: &'a [Record], probe: &str) -> &'a Record {
    match record.iter().find(|row| row.probe == probe) {
        Some(row) => row,
        None => panic!(
            "the record does not hold probe {probe}; record it again with \
             `cargo run -p tjs --example stream_oracle`"
        ),
    }
}

/// Compares one probe's answer with the record.
fn expect(record: &[Record], probe: &str, kind: &str, text: &str) {
    let row = recorded(record, probe);
    assert_eq!(row.kind, kind, "probe {probe}: the kind");
    assert_eq!(decoded(&row.text), text, "probe {probe}: the text");
}

/// Compares one refusal with the record.
fn expect_raised(record: &[Record], probe: &str, message: &str) {
    let row = recorded(record, probe);
    assert_eq!(
        row.kind, "!raise",
        "probe {probe}: the engine answered {:?} instead of raising",
        row.text
    );
    assert_eq!(decoded(&row.text), message, "probe {probe}: the message");
}

/// The text a record's one-line form carries.
fn decoded(encoded: &str) -> String {
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

/// The text a save wrote to `file`.
fn text_saved(streams: &MemoryStreams, file: &str) -> String {
    let text = match streams.text(file) {
        Some(text) => text,
        None => panic!("the save wrote no text to {file}"),
    };
    match String::from_utf16(text.as_utf16()) {
        Ok(text) => text,
        Err(error) => panic!("the text save to {file} is not UTF-16: {error}"),
    }
}

/// The bytes a save wrote to `file`.
fn bytes_saved(streams: &MemoryStreams, file: &str) -> Vec<u8> {
    match streams.bytes(file) {
        Some(bytes) => bytes,
        None => panic!("the save wrote no bytes to {file}"),
    }
}

/// The bytes a text stream is a file of: the byte-order mark and the code
/// units. A text save and a binary save reach the store through two tables,
/// while the engine's file is one byte stream, so a probe that reads a text
/// save as a binary stream is given the bytes the text stream is made of.
fn text_file_bytes(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xfe];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

/// The bytes a record's hexadecimal form carries.
fn unhex(encoded: &str) -> Vec<u8> {
    let digits: Vec<char> = encoded.chars().collect();
    assert!(
        digits.len().is_multiple_of(2),
        "the record holds half a byte: {encoded:?}"
    );
    let mut bytes = Vec::with_capacity(digits.len() / 2);
    for pair in digits.chunks(2) {
        let text: String = pair.iter().collect();
        match u8::from_str_radix(&text, 16) {
            Ok(byte) => bytes.push(byte),
            Err(error) => {
                panic!("the record holds a byte that is not hexadecimal: {text:?}: {error}")
            }
        }
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

/// The dictionary text inside a one-item array's text.
///
/// The array writes its one item one indentation step deeper than its own
/// frame, so that step is removed here.
fn unframe_text(framed: &str) -> String {
    let body = match framed.strip_prefix("(const) [\r\n ") {
        Some(body) => body,
        None => panic!("the frame is not a one-item array: {framed:?}"),
    };
    let body = match body.strip_suffix("\r\n]") {
        Some(body) => body,
        None => panic!("the frame is not a one-item array: {framed:?}"),
    };
    let mut text = String::with_capacity(body.len());
    for (index, line) in body.split("\r\n").enumerate() {
        if index == 0 {
            text.push_str(line);
            continue;
        }
        let Some(rest) = line.strip_prefix(' ') else {
            panic!("the frame has no array indentation to remove: {line:?}");
        };
        text.push_str("\r\n");
        text.push_str(rest);
    }
    text
}

/// The dictionary bytes inside a one-item array's binary form.
///
/// reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.h:71-73 — a one-element
/// array is the one-byte tag `0x91`, so the dictionary's own body starts one
/// byte after the signature.
fn unframe_bytes(framed: &[u8]) -> Vec<u8> {
    let Some(body) = framed.get(BINARY_HEADER.len()..) else {
        panic!("the frame is shorter than the signature");
    };
    assert_eq!(
        body.first(),
        Some(&0x91),
        "the frame is not a one-item array: {body:?}"
    );
    let mut bytes = Vec::with_capacity(framed.len() - 1);
    bytes.extend_from_slice(&framed[..BINARY_HEADER.len()]);
    bytes.extend_from_slice(&body[1..]);
    bytes
}

/// The value handle a container is named by.
fn handle<T: Dispatch + 'static>(object: Rc<T>) -> Rc<dyn Dispatch> {
    object
}

/// The shared stream host a store is installed as.
fn shared(streams: Arc<MemoryStreams>) -> SharedHost {
    streams
}

/// The value that names a container.
fn object(handle: &Rc<dyn Dispatch>) -> Variant {
    Variant::Object(Closure {
        object: Some(Rc::clone(handle)),
        objthis: None,
    })
}

/// The array a dictionary is reached through, holding it as its only item.
fn frame(dictionary: &Rc<dyn Dispatch>) -> Rc<ArrayObject> {
    let array = Rc::new(ArrayObject::new());
    append(&array, object(dictionary));
    array
}

/// Appends one element.
fn append(array: &ArrayObject, value: Variant) {
    array.append(value).expect("the array grows");
}

/// Writes one dictionary member the way an assignment does.
fn write(dictionary: &Dictionary, key: &str, value: Variant) {
    dictionary
        .prop_set(&name(key), MemberFlags::ENSURE, &value, None)
        .expect("the member is written");
}

/// The element at `index`, which a probe expects to be present.
fn element(array: &ArrayObject, index: i64) -> Variant {
    array
        .prop_get_by_num(index, MemberFlags::NONE)
        .expect("the element is present")
}

/// The object a value names, which a probe expects to be a container.
fn object_of(value: &Variant) -> &Rc<dyn Dispatch> {
    match value {
        Variant::Object(closure) => match &closure.object {
            Some(object) => object,
            None => panic!("the value is the null object"),
        },
        other => panic!("the value is not an object: {other:?}"),
    }
}

/// The text of a value.
fn text_of(value: &Variant) -> String {
    match value.to_string() {
        Ok(text) => text.to_string(),
        Err(error) => panic!("the value has no text: {error}"),
    }
}

/// Calls one of an array's members through the dispatch surface.
fn call(array: &ArrayObject, member: &str, arguments: &[Variant]) -> Result<Variant, ObjectError> {
    array.func_call(&name(member), Some(arguments), None)
}

/// A name.
fn name(text: &str) -> TjsString {
    TjsString::from(text)
}

/// The mode a probe gives when it selects no form.
fn plain_mode() -> TjsString {
    TjsString::from("")
}

/// The mode a binary save writes with.
fn binary_mode() -> TjsString {
    TjsString::from("b")
}
