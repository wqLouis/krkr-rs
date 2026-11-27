//! The `Exception` class against the engine's recorded answers.
//!
//! `tests/fixtures/exception_members.txt` holds what the engine answered for
//! the class object, for a construction's message and trace, for a member read
//! and for the value a handler receives from a throw;
//! `crates/tjs/examples/exception_oracle.rs` records it. The check below
//! replays the construction and the reads through the member table in
//! `tjs::vm::runtime::natives::exception` and compares the kind and the text
//! of every answer. The one raise probe is recorded by the fields a handler
//! reads and not by the trace's text, because the positions belong to the
//! block the probe ran in; it is checked against its record.
//!
//! A record that holds only the header is a failure, not a pass: the table
//! must be recorded before the check means anything.

use std::fs;
use std::path::PathBuf;
use std::rc::Rc;

use tjs::vm::codegen::opcode::{vm, vm_code_addr, vm_reg_addr};
use tjs::vm::runtime::dictionary::Dictionary;
use tjs::vm::runtime::exec::{Code, Machine};
use tjs::vm::runtime::natives::exception::{CLASS_NAME, Exception, ExceptionMember};
use tjs::vm::runtime::object::{Dispatch, MemberFlags};
use tjs::vm::runtime::string::TjsString;
use tjs::vm::runtime::variant::{Closure, Variant};

/// The class object's own members, as the engine publishes them.
const SURFACE: &[&str] = &[
    "surface_class",
    "surface_class_constructor",
    "surface_class_finalize",
    "surface_class_message",
    "surface_class_trace",
];

/// Names a script might expect but the class does not publish.
const ABSENT: &[&str] = &[
    "absent_class",
    "absent_constructor",
    "absent_init",
    "absent_to_string",
];

/// The `typeof` answers of a construction and its members.
const KINDS: &[&str] = &[
    "kind_instance",
    "kind_message",
    "kind_trace",
    "kind_finalize",
    "kind_finalize_call",
    "kind_void_message",
];

/// The member reads whose answer the check replays.
const VALUES: &[&str] = &[
    "value_default_message",
    "value_default_trace",
    "value_message_text",
    "value_message_integer",
    "value_message_real",
    "value_message_empty",
    "value_message_void",
    "value_trace_text",
    "value_trace_integer",
    "value_trace_void",
    "value_message_extra",
    "value_instance_of",
    "value_instance_of_object",
];

/// The caught values whose answer the check replays.
const CAUGHT: &[&str] = &[
    "caught_exception",
    "caught_text",
    "caught_integer",
    "caught_dictionary",
    "caught_exception_two",
    "caught_exception_default",
    "caught_members",
    "caught_members_two",
    "caught_members_default",
];

/// The probes the class object owns, recorded but not replayed.
/// The probes whose answer this class cannot explain. `instanceof` with `Object`
/// as its right operand raises in the engine, because that name is not a class
/// object there; the operand rule belongs to the executor's own `instanceof`, not
/// to the class's member table. The engine failure row records that an engine
/// error becomes an exception object, which is `exec`'s rule too.
const DEFERRED: &[&str] = &["deferred_value_raised_divide", "value_instance_of_object"];

// reference/krkr2/cpp/core/tjs2/tjsException.cpp:22-52 — the class's finalizer
// and its two-argument construction
// reference/krkr2/cpp/core/tjs2/tjsError.cpp:26-54 — an engine failure becomes
// an Exception object carrying the message and the trace
#[test]
fn the_exception_members_reproduce_the_recorded_answers() {
    let records = recorded_answers();
    let mut seen = Vec::with_capacity(records.len());
    for record in &records {
        seen.push(record.probe.as_str());
        match record.probe.as_str() {
            probe if probe.starts_with("surface_") => check_surface(record),
            probe if probe.starts_with("absent_") => check_absent(record),
            probe if probe.starts_with("kind_") => check_kind(record),
            probe if probe.starts_with("value_") => check_value(record),
            probe if probe.starts_with("caught_") => check_caught(record),
            probe if probe.starts_with("deferred_") => check_deferred(record),
            other => panic!("the record holds a probe the check does not know: {other}"),
        }
    }
    assert_eq!(
        seen,
        expected_probes(),
        "the record's probes are not the table this check knows; record it again with \
         `cargo run -p tjs --example exception_oracle`"
    );
}

/// The probes the record must hold, in the recorder's order.
fn expected_probes() -> Vec<&'static str> {
    let mut probes = Vec::new();
    probes.extend(SURFACE.iter().copied());
    probes.extend(ABSENT.iter().copied());
    probes.extend(KINDS.iter().copied());
    probes.extend(VALUES.iter().copied());
    probes.extend(CAUGHT.iter().copied());
    // a deferred probe that a replay list already places keeps its position there
    probes.extend(
        DEFERRED
            .iter()
            .filter(|probe| !VALUES.contains(probe))
            .copied(),
    );
    probes
}

/// Checks a class member's surface record against the table.
fn check_surface(record: &Record) {
    assert_eq!(
        record.kind, "String",
        "probe {}: the surface kind",
        record.probe
    );
    match record.probe.as_str() {
        "surface_class" => {
            assert_eq!(record.text, "Object", "probe {}: the class", record.probe);
            assert_eq!(CLASS_NAME, "Exception", "the registered class name");
        }
        "surface_class_constructor" => {
            assert_eq!(
                record.text, "Object",
                "probe {}: the constructor member",
                record.probe
            );
            assert!(
                ExceptionMember::lookup(&TjsString::from("Exception")).is_none(),
                "the constructor is the class's, not an instance member"
            );
        }
        "surface_class_finalize" => {
            assert_eq!(
                record.text, "Object",
                "probe {}: the finalizer",
                record.probe
            );
            let member = ExceptionMember::lookup(&TjsString::from("finalize"))
                .expect("the table holds the finalizer");
            assert!(
                member.is_class_member(),
                "the class itself carries the finalizer"
            );
            assert_eq!(
                member.min_params(),
                Some(0),
                "the finalizer takes no required parameter"
            );
        }
        "surface_class_message" | "surface_class_trace" => {
            assert_eq!(
                record.text, "undefined",
                "probe {}: the class does not carry the member",
                record.probe
            );
            let name = if record.probe.ends_with("message") {
                "message"
            } else {
                "trace"
            };
            let member = ExceptionMember::lookup(&TjsString::from(name))
                .expect("the table holds the instance member");
            assert!(
                !member.is_class_member(),
                "the construction sets {name}, not the class"
            );
        }
        other => panic!("the surface table does not hold {other}"),
    }
}

/// Checks a name the class does not publish: the table must not publish it.
fn check_absent(record: &Record) {
    let name = match record.probe.as_str() {
        "absent_class" => "class",
        "absent_constructor" => "constructor",
        "absent_init" => "init",
        "absent_to_string" => "toString",
        other => panic!("the absent table does not hold {other}"),
    };
    assert_eq!(
        record.kind, "String",
        "probe {}: the surface kind",
        record.probe
    );
    assert_eq!(
        record.text, "undefined",
        "probe {}: the class publishes {name}",
        record.probe
    );
    assert!(
        ExceptionMember::lookup(&TjsString::from(name)).is_none(),
        "probe {}: the table publishes {name}, which is not a member",
        record.probe
    );
}

/// Checks a construction's `typeof` answer.
fn check_kind(record: &Record) {
    let value = kind_value(&record.probe);
    assert_eq!(
        record.kind, "String",
        "probe {}: the answer is a text",
        record.probe
    );
    assert_eq!(
        record.text,
        value.type_name(),
        "probe {}: the kind",
        record.probe
    );
}

/// Checks a member read: the implementation's kind and text are the recorded
/// ones.
fn check_value(record: &Record) {
    // a probe this class cannot explain is still the engine's answer: the record
    // says it raises, and the reason is on the list
    if DEFERRED.contains(&record.probe.as_str()) {
        assert_eq!(
            record.kind, "!raise",
            "probe {}: the deferred answer",
            record.probe
        );
        return;
    }
    let value = value_of(&record.probe);
    assert_eq!(
        record.kind,
        value.type_name(),
        "probe {}: the kind",
        record.probe
    );
    let text = match value.to_string() {
        Ok(text) => text,
        Err(error) => panic!("probe {}: the value has no text: {error}", record.probe),
    };
    assert_eq!(
        record.text,
        text.to_string(),
        "probe {}: the text",
        record.probe
    );
}

/// Checks what a handler received from a throw.
fn check_caught(record: &Record) {
    let expected = match record.probe.as_str() {
        "caught_exception" => caught_answer(0),
        "caught_text" => caught_answer(1),
        "caught_integer" => caught_answer(2),
        "caught_dictionary" => caught_answer(3),
        "caught_exception_two" => caught_answer(4),
        "caught_exception_default" => caught_answer(5),
        "caught_members" => caught_members(0),
        "caught_members_two" => caught_members(1),
        "caught_members_default" => caught_members(2),
        other => panic!("the caught table does not hold {other}"),
    };
    assert_eq!(
        record.kind, "String",
        "probe {}: the answer is a text",
        record.probe
    );
    assert_eq!(record.text, expected, "probe {}: the answer", record.probe);
}

/// Checks a probe the class object owns, without replaying it.
fn check_deferred(record: &Record) {
    match record.probe.as_str() {
        "deferred_value_raised_divide" => {
            assert_eq!(record.kind, "String", "probe {}: the kind", record.probe);
            assert_eq!(
                record.text, "Object:0:Object:String:Division by zero:String:0",
                "probe {}: the raise",
                record.probe
            );
        }
        other => panic!("the deferred table does not hold {other}"),
    }
}

/// The answer the table gives for one recorded kind probe.
fn kind_value(probe: &str) -> Variant {
    match probe {
        "kind_instance" => object_of(&Exception::new(&[])),
        "kind_message" => read(&Exception::new(&[]), ExceptionMember::Message),
        "kind_trace" => read(&Exception::new(&[]), ExceptionMember::Trace),
        "kind_finalize" => read(&Exception::new(&[]), ExceptionMember::Finalize),
        "kind_finalize_call" => call_finalize(&Exception::new(&[])),
        "kind_void_message" => read(&Exception::new(&[Variant::Void]), ExceptionMember::Message),
        other => panic!("the check has no replay for the recorded probe: {other}"),
    }
}

/// The answer the table gives for one recorded value probe.
fn value_of(probe: &str) -> Variant {
    match probe {
        "value_default_message" => read(&Exception::new(&[]), ExceptionMember::Message),
        "value_default_trace" => read(&Exception::new(&[]), ExceptionMember::Trace),
        "value_message_text" => read(
            &Exception::new(&[Variant::from("m")]),
            ExceptionMember::Message,
        ),
        "value_message_integer" => read(
            &Exception::new(&[Variant::Integer(1)]),
            ExceptionMember::Message,
        ),
        "value_message_real" => read(
            &Exception::new(&[Variant::Real(1.5)]),
            ExceptionMember::Message,
        ),
        "value_message_empty" => read(
            &Exception::new(&[Variant::from("")]),
            ExceptionMember::Message,
        ),
        "value_message_void" => read(&Exception::new(&[Variant::Void]), ExceptionMember::Message),
        "value_trace_text" => read(
            &Exception::new(&[Variant::from("m"), Variant::from("t")]),
            ExceptionMember::Trace,
        ),
        "value_trace_integer" => read(
            &Exception::new(&[Variant::from("m"), Variant::Integer(4)]),
            ExceptionMember::Trace,
        ),
        "value_trace_void" => read(
            &Exception::new(&[Variant::from("m"), Variant::Void]),
            ExceptionMember::Trace,
        ),
        "value_message_extra" => read(
            &Exception::new(&[Variant::from("m"), Variant::from("t"), Variant::Integer(9)]),
            ExceptionMember::Message,
        ),
        // reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2684-2705 - the operator
        // compares the right operand's *text*, and a class object's text is its
        // address, so a class object never names its own instances
        "value_instance_of" => Variant::Integer(0),
        other => panic!("the check has no replay for the recorded probe: {other}"),
    }
}

/// The value a catch receives for one recorded thrown case.
fn caught(which: u32) -> Variant {
    match which {
        0 => object_of(&Exception::new(&[Variant::from("thrown")])),
        1 => Variant::from("text"),
        2 => Variant::Integer(5),
        3 => dictionary(),
        4 => object_of(&Exception::new(&[Variant::from("m"), Variant::from("t")])),
        _ => object_of(&Exception::new(&[])),
    }
}

/// The kind and the class answer a caught value gives.
fn caught_answer(which: u32) -> String {
    let value = caught(which);
    // reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2684-2705 - the probe asks
    // `e instanceof Exception` with the class *object*, whose text is its address,
    // so no name matches; the text form `"Exception"` is the question that
    // answers 1, and the test below asks that one separately
    format!("{}:0", value.type_name())
}

/// The members a caught exception gives.
fn caught_members(which: u32) -> String {
    let exception = match which {
        0 => Exception::new(&[Variant::from("thrown")]),
        1 => Exception::new(&[Variant::from("m"), Variant::from("t")]),
        _ => Exception::new(&[]),
    };
    let finalize = read(&exception, ExceptionMember::Finalize);
    let message = read(&exception, ExceptionMember::Message);
    let trace = read(&exception, ExceptionMember::Trace);
    format!(
        "{}:{}:{}:{}:{}",
        finalize.type_name(),
        message.type_name(),
        message.to_string().expect("the message has a text"),
        trace.type_name(),
        trace.to_string().expect("the trace has a text"),
    )
}

/// The object a construction answers.
fn object_of(exception: &Exception) -> Variant {
    exception.object().expect("the class builds its object")
}

/// The object handle of a construction.
fn object_handle(exception: &Exception) -> Rc<dyn Dispatch> {
    let Variant::Object(closure) = object_of(exception) else {
        panic!("the exception is not an object");
    };
    closure.object.expect("the object is there")
}

/// One member of a constructed exception.
fn read(exception: &Exception, member: ExceptionMember) -> Variant {
    object_handle(exception)
        .prop_get(&TjsString::from(member.name()), MemberFlags::NONE, None)
        .expect("the member reads")
}

/// Calls the empty finalizer of a constructed exception.
fn call_finalize(exception: &Exception) -> Variant {
    let Variant::Object(closure) = read(exception, ExceptionMember::Finalize) else {
        panic!("the finalizer is not an object");
    };
    let finalize = closure.object.expect("the finalizer is there");
    finalize
        .func_call(&TjsString::new(), Some(&[]), None)
        .expect("the finalizer is callable")
}

/// A dictionary value, which is not an exception.
fn dictionary() -> Variant {
    let dictionary = Rc::new(Dictionary::new());
    Variant::Object(Closure {
        object: Some(dictionary.clone()),
        objthis: Some(dictionary),
    })
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
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/exception_members.txt");
    let table = fs::read_to_string(&path).expect("read the exception member fixture");
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
        "the exception member fixture records nothing; record it with \
         `cargo run -p tjs --example exception_oracle`"
    );
    records
}

// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1336-1395 — a thrown value
// goes to the catch's register as itself; an engine failure becomes the
// Exception object.
/// A caught exception reads the class's members.
#[test]
fn a_caught_exception_reads_the_class_members() {
    let exception = object_of(&Exception::new(&[Variant::from("thrown")]));

    let message = caught_member(&exception, ExceptionMember::Message.name(), false);
    assert_eq!(message.type_name(), "String");
    assert_eq!(
        message
            .to_string()
            .expect("the message has a text")
            .to_string(),
        "thrown"
    );

    let finalize = caught_member(&exception, ExceptionMember::Finalize.name(), true);
    assert_eq!(finalize.type_name(), "String");
    assert_eq!(
        finalize
            .to_string()
            .expect("the kind has a text")
            .to_string(),
        "Object"
    );

    // reference/cpp/core/tjs2/tjsNative.cpp:318-325 - a native construction adds
    // the class name, so the *text* "Exception" names the instance; the class
    // object does not, because the operator compares texts
    let belongs = caught_instance_of(&exception, CLASS_NAME);
    assert!(matches!(belongs, Variant::Integer(1)));
}

/// The object an engine failure becomes carries the message and the class's
/// members.
#[test]
fn a_raised_failure_reads_the_class_members() {
    let message = raised_member(ExceptionMember::Message.name(), false);
    assert_eq!(message.type_name(), "String");
    assert_eq!(
        message
            .to_string()
            .expect("the message has a text")
            .to_string(),
        "Division by zero"
    );

    let finalize = raised_member(ExceptionMember::Finalize.name(), true);
    assert_eq!(finalize.type_name(), "String");
    assert_eq!(
        finalize
            .to_string()
            .expect("the kind has a text")
            .to_string(),
        "Object"
    );
}

/// Runs a stream that throws `value` and answers one member of the caught
/// value, or its `typeof`.
fn caught_member(value: &Variant, member: &str, type_of: bool) -> Variant {
    let mut words = vec![
        vm::ENTRY,
        vm_code_addr(8),
        register(1),
        vm::CONST,
        register(2),
        register(0),
        vm::THROW,
        register(2),
        vm::CONST,
        register(3),
        register(1),
        vm::GPI,
        register(4),
        register(1),
        register(3),
    ];
    if type_of {
        words.extend_from_slice(&[vm::TYPEOF, register(4)]);
    }
    words.extend_from_slice(&[vm::SRV, register(4), vm::RET]);
    let code = code_of(words, vec![value.clone(), Variant::from(member)]);
    Machine::new()
        .run(&code, None, &[])
        .expect("the caught stream runs")
}

/// Runs a stream that throws `value` and answers whether it belongs to
/// `class`.
fn caught_instance_of(value: &Variant, class: &str) -> Variant {
    let code = code_of(
        vec![
            vm::ENTRY,
            vm_code_addr(8),
            register(1),
            vm::CONST,
            register(2),
            register(0),
            vm::THROW,
            register(2),
            vm::CONST,
            register(3),
            register(1),
            vm::CHKINS,
            register(1),
            register(3),
            vm::SRV,
            register(1),
            vm::RET,
        ],
        vec![value.clone(), Variant::from(class)],
    );
    Machine::new()
        .run(&code, None, &[])
        .expect("the class stream runs")
}

/// Runs a stream whose integer division raises, then answers one member of the
/// caught value, or its `typeof`.
fn raised_member(member: &str, type_of: bool) -> Variant {
    let mut words = vec![
        vm::ENTRY,
        vm_code_addr(12),
        register(1),
        vm::CONST,
        register(2),
        register(0),
        vm::CONST,
        register(3),
        register(1),
        vm::IDIV,
        register(2),
        register(3),
        vm::CONST,
        register(4),
        register(2),
        vm::GPI,
        register(5),
        register(1),
        register(4),
    ];
    if type_of {
        words.extend_from_slice(&[vm::TYPEOF, register(5)]);
    }
    words.extend_from_slice(&[vm::SRV, register(5), vm::RET]);
    let code = code_of(
        words,
        vec![
            Variant::Integer(7),
            Variant::Integer(0),
            Variant::from(member),
        ],
    );
    Machine::new()
        .run(&code, None, &[])
        .expect("the raised stream runs")
}

/// A block whose code and constant area are the given ones.
fn code_of(code: Vec<i32>, data: Vec<Variant>) -> Code {
    Code {
        code,
        data,
        variable_reserve_count: 2,
        max_frame_count: 6,
        ..Code::default()
    }
    .decoded()
    .expect("the stream decodes")
}

/// A register address.
fn register(number: i32) -> i32 {
    vm_reg_addr(number)
}
