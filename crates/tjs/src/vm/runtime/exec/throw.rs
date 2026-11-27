//! The exception machinery: the protected region, the throw, and the raise.
//!
//! A `try` in this language is **not** a handler stack. The protected region is
//! a *recursive* run of the interpreter: `ENTRY` runs the body and answers what
//! the body answered, so the entry's own frame is where a failure of the body
//! surfaces — the innermost entry sees it first, and each entry outward adds its
//! own frame to the trace. That is why the catch's destination is an ordinary
//! register: once the entry has handed the value over, nothing about the catch's
//! body is special.
//!
//! # The two failures
//!
//! | raised by | carries | what a catch sees |
//! |---|---|---|
//! | a script's `throw` | the value it threw | the value itself |
//! | the engine | a reason with no value | an exception object with a message and a trace |
//!
//! The distinction is kept by the failure *type*, not by a string: a script's
//! failure holds a [`Variant`] and a catch can reach into it, and the engine's
//! holds a [`Raise`] and a catch reads a message. One has a value, the other has
//! a reason, and a catch is allowed to tell them apart.
//!
//! # The silent kind
//!
//! A failure raised while flagging itself silent passes a catch by and is never
//! reported. It exists so that a cancellation is not turned into a script object
//! a catch could inspect, and it passes every entry until something outside the
//! code's own regions deals with it.
//!
//! # The trace
//!
//! One line per frame the failure passed, innermost first. A throw adds the
//! frame it was raised in at the throw instruction, and a call adds the caller's
//! frame at the call instruction as the failure crosses back into it, so the
//! list reads in unwind order. An engine failure adds no line for the frame that
//! raised it; only the frames it later crosses are named. The engine's message
//! carries the lines; a script's thrown value does not — it is the value, and
//! the trace travels beside it for whoever reports it.
//!
//! # Where the trace is kept
//!
//! The machine boundary answers the [`Unwind`] itself, so a caller that reports
//! an uncaught failure reads the same frames a handler inside the code reads.
//! The C interface between this crate and the vendored executor still carries a
//! message only, which is the known seam defect.

use crate::vm::runtime::message::{
    self, TJS_ACCESS_DENIED, TJS_BAD_PARAM_COUNT, TJS_DIVIDE_BY_ZERO, TJS_INTEGER_OUT_OF_RANGE,
    TJS_INVALID_FORMAT_STRING, TJS_INVALID_PARAM, TJS_INVALID_TYPE, TJS_MEMBER_NOT_FOUND,
    TJS_NOT_IMPLEMENTED, TJS_NULL_ACCESS, TJS_RANGE_ERROR, TJS_SPECIFY_ARRAY,
    TJS_UNKNOWN_BIT_STRING_CHARACTER, TJS_UNKNOWN_HEX_STRING_CHARACTER,
    TJS_UNKNOWN_PACK_UNPACK_TEMPLATE, TJS_VARIANT_CONVERT_ERROR,
    TJS_VARIANT_CONVERT_ERROR_TO_OBJECT,
};
use crate::vm::runtime::natives::exception::Exception;
use crate::vm::runtime::object::ObjectError;
use crate::vm::runtime::string::TjsString;
use crate::vm::runtime::variant::Variant;

use super::frame::{Code, Frame};
use super::instruction::{Protected, Slot};
use super::{ContextObject, Fault, Raise};

/// One line of a trace: the frame a failure passed and the place in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameTrace {
    /// The name of the frame's context, when it has one.
    pub name: String,
    /// The code position the failure was at inside it.
    pub position: usize,
}

impl FrameTrace {
    /// The line as a message carries it.
    fn line(&self) -> String {
        if self.name.is_empty() {
            format!("at code position {}", self.position)
        } else {
            format!("{} at code position {}", self.name, self.position)
        }
    }
}

/// What a failure carries beside the reason it names.
///
/// The reason is static, so the data its text names lives here: it is made only
/// when the failure is raised, and it is owned only for as long as the failure
/// takes to reach a handler or the machine boundary.
#[derive(Clone, Debug, Default)]
enum Detail {
    /// Nothing: the reason's text names no argument.
    #[default]
    None,
    /// The value the operation was asked of.
    Value(Box<Variant>),
    /// The name an instruction named, or the front end's own text.
    Text(TjsString),
    /// The value a script threw.
    Thrown(Box<Variant>),
}

/// A failure on its way out of the code, and the frames it has passed.
///
/// This is what the run loop and the machine boundary answer with, so that a
/// failure can carry a value and a trace across the frames of the code it
/// unwinds. The reason it names is static; everything the reason's message or a
/// catch needs travels beside it, so nothing is built until the failure happens.
#[derive(Clone, Debug)]
pub struct Unwind {
    /// What failed.
    failure: Fault,
    /// What the failure names beside its reason.
    detail: Detail,
    /// The frames it passed, innermost first.
    trace: Vec<FrameTrace>,
}

impl Unwind {
    /// A failure that has not passed a frame yet.
    pub fn new(failure: Fault) -> Self {
        Self {
            failure,
            detail: Detail::None,
            trace: Vec::new(),
        }
    }

    /// A raise that has not passed a frame yet.
    pub fn raised(raise: Raise) -> Self {
        Self::new(Fault::Raised(raise))
    }

    /// A failure that passes every protected region it meets.
    ///
    /// The silent kind exists so that a cancellation is not turned into a value
    /// a catch could inspect.
    pub fn silent() -> Self {
        Self::new(Fault::Silent)
    }

    /// A failure a script threw.
    ///
    /// A throw is a failure a handler can reach into, so the value travels as
    /// itself rather than as a message.
    pub fn thrown(value: Variant) -> Self {
        Self {
            failure: Fault::Thrown,
            detail: Detail::Thrown(Box::new(value)),
            trace: Vec::new(),
        }
    }

    /// The failure with the value its raise names.
    #[must_use]
    pub fn with_value(mut self, value: &Variant) -> Self {
        self.detail = Detail::Value(Box::new(value.clone()));
        self
    }

    /// The failure with the text its raise names: a member's name, or the front
    /// end's own message.
    #[must_use]
    pub fn with_text(mut self, text: TjsString) -> Self {
        self.detail = Detail::Text(text);
        self
    }

    /// The failure itself.
    pub fn fault(&self) -> Fault {
        self.failure
    }

    /// The frames the failure has passed.
    pub fn trace(&self) -> &[FrameTrace] {
        &self.trace
    }

    /// The value the failure names, when it names one: the operand a conversion
    /// was asked of.
    pub fn value(&self) -> Option<&Variant> {
        match &self.detail {
            Detail::Value(value) => Some(value),
            _ => None,
        }
    }

    /// The text the failure names, when it names one: the member an instruction
    /// named, or the front end's own message.
    pub fn text(&self) -> Option<&TjsString> {
        match &self.detail {
            Detail::Text(text) => Some(text),
            _ => None,
        }
    }

    /// The value a script threw, when the failure is a throw.
    pub fn thrown_value(&self) -> Option<&Variant> {
        match &self.detail {
            Detail::Thrown(value) => Some(value),
            _ => None,
        }
    }

    /// Adds the frame the failure has just left.
    ///
    /// `context` and `ip` are the caller's: a call boundary records the call
    /// instruction, not the callee's throw.
    pub fn add_frame(&mut self, context: Option<&ContextObject>, ip: usize) {
        self.push(line_of(context, ip));
    }

    /// Adds a frame the failure passed.
    fn push(&mut self, frame: FrameTrace) {
        self.trace.push(frame);
    }

    /// Whether the failure must pass a protected region by.
    fn is_silent(&self) -> bool {
        matches!(self.failure, Fault::Silent)
    }
}

impl std::fmt::Display for Unwind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.failure.fmt(formatter)
    }
}

impl std::error::Error for Unwind {}

impl From<Fault> for Unwind {
    fn from(failure: Fault) -> Self {
        Self::new(failure)
    }
}

impl From<Raise> for Unwind {
    fn from(raise: Raise) -> Self {
        Self::raised(raise)
    }
}

impl From<ObjectError> for Unwind {
    fn from(error: ObjectError) -> Self {
        Self::new(Fault::from(error))
    }
}

/// Runs a protected region and answers for it.
///
/// The body runs from the instruction after the entry. A failure of the body
/// crosses out of this entry: unless it is the silent kind, the catch's register
/// receives it and the catch's body runs from the entry's own instruction. The
/// entry adds no line of its own, because the body's frame already recorded the
/// position the failure was at.
pub fn enter(
    machine: &super::Machine,
    code: &Code,
    frame: &mut Frame,
    argv: &[Variant],
    context: Option<&ContextObject>,
    body: usize,
    entry: &Protected,
) -> Result<Variant, Unwind> {
    match machine.execute(code, frame, argv, context, body) {
        Ok(answer) => Ok(answer),
        Err(unwind) => {
            if unwind.is_silent() {
                return Err(unwind);
            }
            deliver(&unwind, frame, entry.register)?;
            machine.execute(code, frame, argv, context, entry.target)
        }
    }
}

/// The failure a throw instruction raises.
pub fn raise(frame: &Frame, context: Option<&ContextObject>, value: Slot, at: usize) -> Unwind {
    let mut unwind = Unwind::thrown(frame.slot(value).clone());
    unwind.push(line_of(context, at));
    unwind
}

/// Hands a failure to the catch's register.
///
/// A script's thrown value goes there as itself. The engine's own failure becomes
/// an exception object carrying the message and the trace, because that is what a
/// handler reads. A failure that is neither — a defective code stream, say —
/// leaves the register empty, which is what the engine's own catch-all does.
fn deliver(unwind: &Unwind, frame: &mut Frame, register: Slot) -> Result<(), Unwind> {
    let value = match unwind.thrown_value() {
        Some(value) => value.clone(),
        None => match unwind.fault() {
            Fault::Raised(_) => exception_object(&message_of(unwind), unwind.trace())?,
            _ => Variant::Void,
        },
    };
    frame.store(register, value);
    Ok(())
}

/// The exception object a handler reads.
///
/// The exception is constructed with the message and then given the trace, and
/// its members and class name come from the class's member table.
fn exception_object(message: &str, trace: &[FrameTrace]) -> Result<Variant, Fault> {
    let lines: Vec<String> = trace.iter().map(FrameTrace::line).collect();
    let exception = Exception::raised(
        Variant::String(TjsString::from(message)),
        Variant::String(TjsString::from(lines.join("\n"))),
    );
    exception.object().map_err(Fault::from)
}

/// The message a raise reads as.
///
/// The text is read from [`message`] now, not fixed when the raising code was
/// written, so an assignment made before the raise changes what a handler
/// prints. The value or the name the text carries is the one the failure was
/// raised with.
pub fn message_of(unwind: &Unwind) -> String {
    let Fault::Raised(raise) = unwind.failure else {
        return unwind.failure.to_string();
    };
    match raise {
        Raise::Convert { to } => {
            // a conversion raise is built with the value it was asked of, so the
            // pairing is the raising code's and no condition a script can reach
            let value = match unwind.value() {
                Some(value) => value,
                None => panic!("a conversion raise carries the value it was asked of"),
            };
            let readable = value.readable().to_string();
            if to == "Object" {
                required(TJS_VARIANT_CONVERT_ERROR_TO_OBJECT, &[readable.as_str()])
            } else {
                required(TJS_VARIANT_CONVERT_ERROR, &[readable.as_str(), to])
            }
        }
        Raise::DivideByZero => required(TJS_DIVIDE_BY_ZERO, &[]),
        Raise::IntegerOutOfRange => required(TJS_INTEGER_OUT_OF_RANGE, &[]),
        Raise::NullAccess => required(TJS_NULL_ACCESS, &[]),
        Raise::UnknownPackTemplate => required(TJS_UNKNOWN_PACK_UNPACK_TEMPLATE, &[]),
        Raise::UnknownBitString => required(TJS_UNKNOWN_BIT_STRING_CHARACTER, &[]),
        Raise::UnknownHexString => required(TJS_UNKNOWN_HEX_STRING_CHARACTER, &[]),
        Raise::MemberNotFound => {
            let name = match unwind.text() {
                Some(name) => name.to_string(),
                None => panic!("a member raise carries the name it was raised on"),
            };
            required(TJS_MEMBER_NOT_FOUND, &[name.as_str()])
        }
        Raise::AccessDenied => required(TJS_ACCESS_DENIED, &[]),
        Raise::RangeError => required(TJS_RANGE_ERROR, &[]),
        Raise::NotCallable => required(TJS_INVALID_TYPE, &[]),
        Raise::ParameterCount => required(TJS_BAD_PARAM_COUNT, &[]),
        Raise::InvalidParameter => required(TJS_INVALID_PARAM, &[]),
        Raise::FormatString => required(TJS_INVALID_FORMAT_STRING, &[]),
        Raise::NotAnArray => required(TJS_SPECIFY_ARRAY, &[]),
        // A front end failure carries its own text rather than an id: the
        // message already names what the parse failed on, and there is no
        // registered text to read instead. The one text that is a registered
        // entry is the declaration failure, whose message names TJSNotImplemented.
        Raise::ScriptError => {
            let text = match unwind.text() {
                Some(text) => text.to_string(),
                None => panic!("a compile raise carries the front end's own text"),
            };
            match message::default_text(TJS_NOT_IMPLEMENTED) {
                Some(default) if text == default => required(TJS_NOT_IMPLEMENTED, &[]),
                _ => text,
            }
        }
    }
}

/// The text of a registered id, with this raise's arguments substituted.
///
/// The id is one of this module's message-table constants, so a miss is a
/// defect in the pairing rather than a condition a script can reach.
fn required(id: &str, arguments: &[&str]) -> String {
    match message::format(id, arguments) {
        Some(text) => text,
        None => panic!("the message id {id} is not registered"),
    }
}

/// The trace line for the frame at `ip`.
fn line_of(context: Option<&ContextObject>, ip: usize) -> FrameTrace {
    let name = match context {
        Some(context) => context.name().to_string(),
        None => String::new(),
    };
    FrameTrace { name, position: ip }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::codegen::opcode::vm;
    use std::rc::Rc;

    use crate::vm::runtime::object::{CustomObject, Dispatch, MemberFlags};
    use crate::vm::runtime::octet::Octet;
    use crate::vm::runtime::variant::{Closure, Target};

    fn text(value: &str) -> TjsString {
        TjsString::from(value)
    }

    /// The raise a refused conversion of `value` makes.
    fn conversion(value: Variant, target: Target) -> Unwind {
        Unwind::raised(Raise::Convert { to: target.name() }).with_value(&value)
    }

    fn same(left: &Variant, right: &Variant) -> bool {
        super::super::op::discern_equals(left, right)
    }

    fn register(number: i32) -> i32 {
        crate::vm::codegen::opcode::vm_reg_addr(number)
    }

    fn code_of(words: Vec<i32>, data: Vec<Variant>) -> Code {
        Code {
            code: words,
            data,
            variable_reserve_count: 2,
            max_frame_count: 6,
            ..Code::default()
        }
        .decoded()
        .expect("the stream decodes")
    }

    /// A thrown value reaches the catch's register as itself, and the catch's
    /// body runs from the entry's distance
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1336-1395`).
    #[test]
    fn a_throw_hands_the_caught_value_to_the_catch() {
        let thrown: Rc<dyn Dispatch> = Rc::new(CustomObject::new([text("Thrown")]));
        let code = code_of(
            vec![
                vm::ENTRY,
                crate::vm::codegen::opcode::vm_code_addr(12),
                register(1),
                vm::CONST,
                register(2),
                register(0),
                vm::THROW,
                register(2),
                vm::SRV,
                register(2),
                vm::RET,
                vm::NOP,
                vm::CONST,
                register(3),
                register(1),
                vm::SRV,
                register(3),
                vm::RET,
            ],
            vec![
                Variant::Object(Closure {
                    object: Some(thrown.clone()),
                    objthis: None,
                }),
                Variant::Integer(7),
            ],
        );
        let mut machine = super::super::Machine::new();
        let answer = machine.run(&code, None, &[]).expect("the stream runs");
        assert!(
            same(&answer, &Variant::Integer(7)),
            "the catch's body answered, so the throw was caught"
        );
    }

    /// A failure of the engine becomes an exception object whose message a
    /// handler reads (`reference/krkr2/cpp/core/tjs2/tjsError.cpp:26-54`,
    /// `TJSGetExceptionObject`).
    #[test]
    fn a_raise_becomes_an_exception_with_a_message() {
        let code = code_of(
            vec![
                vm::ENTRY,
                crate::vm::codegen::opcode::vm_code_addr(15),
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
                vm::SRV,
                register(2),
                vm::RET,
                vm::CONST,
                register(4),
                register(2),
                vm::GPI,
                register(5),
                register(1),
                register(4),
                vm::SRV,
                register(5),
                vm::RET,
            ],
            vec![
                Variant::Integer(7),
                Variant::Integer(0),
                Variant::String(text("message")),
            ],
        );
        let mut machine = super::super::Machine::new();
        let answer = machine.run(&code, None, &[]).expect("the stream runs");
        let Variant::String(message) = answer else {
            panic!("the catch did not read a message");
        };
        assert!(
            message.to_string().contains("ivision by zero"),
            "the handler reads the reason: {message}"
        );
    }

    /// An exit ends the context, exactly as a return does
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1263-1265`).
    #[test]
    fn an_exit_ends_the_context() {
        let code = code_of(
            vec![
                vm::ENTRY,
                crate::vm::codegen::opcode::vm_code_addr(15),
                register(1),
                vm::CONST,
                register(2),
                register(0),
                vm::SRV,
                register(2),
                vm::EXTRY,
                vm::CONST,
                register(2),
                register(1),
                vm::SRV,
                register(2),
                vm::RET,
                vm::CONST,
                register(2),
                register(2),
                vm::SRV,
                register(2),
                vm::RET,
            ],
            vec![
                Variant::Integer(1),
                Variant::Integer(2),
                Variant::Integer(3),
            ],
        );
        let mut machine = super::super::Machine::new();
        let answer = machine.run(&code, None, &[]).expect("the stream runs");
        assert!(
            same(&answer, &Variant::Integer(1)),
            "the exit answered the value stored before it"
        );
    }

    /// The silent kind passes the catch by
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1359-1360`).
    #[test]
    fn a_silent_failure_passes_the_catch() {
        let mut unwind = Unwind::silent();
        unwind.push(line_of(None, 5));
        assert!(unwind.is_silent());
        assert_eq!(unwind.trace().len(), 1);
        assert!(matches!(unwind.fault(), Fault::Silent));
    }

    /// The trace is innermost first, one line per entry the failure crossed
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1306-1308`).
    #[test]
    fn the_trace_is_in_unwind_order() {
        let mut unwind = Unwind::new(Fault::Raised(Raise::RangeError));
        unwind.push(FrameTrace {
            name: String::from("inner"),
            position: 3,
        });
        unwind.push(FrameTrace {
            name: String::from("outer"),
            position: 9,
        });
        let names: Vec<&str> = unwind
            .trace()
            .iter()
            .map(|line| line.name.as_str())
            .collect();
        assert_eq!(names, vec!["inner", "outer"]);
        assert!(unwind.trace()[0].line().contains("inner"));
    }

    /// A failure that no protected region catches leaves the machine as its own
    /// fault, with the value still in it
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1266-1270`).
    #[test]
    fn an_uncaught_throw_leaves_as_its_value() {
        let code = code_of(
            vec![vm::CONST, register(1), 0, vm::THROW, register(1), vm::RET],
            vec![Variant::String(text("uncaught"))],
        );
        let mut machine = super::super::Machine::new();
        match machine.run(&code, None, &[]) {
            Err(unwind) => {
                assert_eq!(unwind.fault(), Fault::Thrown);
                let value = unwind.thrown_value().expect("the thrown value");
                assert!(same(value, &Variant::String(text("uncaught"))));
            }
            Ok(answer) => panic!("expected the thrown value, got {answer:?}"),
        }
    }

    /// The exception object carries the class name a handler asks about, the
    /// trace as a text and the class's finalizer
    /// (`reference/krkr2/cpp/core/tjs2/tjsError.cpp:47-51`,
    /// `reference/krkr2/cpp/core/tjs2/tjsException.cpp:26`).
    #[test]
    fn the_exception_object_carries_its_trace() {
        let trace = [
            FrameTrace {
                name: String::from("inner"),
                position: 1,
            },
            FrameTrace {
                name: String::from("outer"),
                position: 2,
            },
        ];
        let value = exception_object("boom", &trace).expect("the object builds");
        let Variant::Object(closure) = value else {
            panic!("the exception is not an object");
        };
        let object = closure.object.expect("an object");
        // reference/cpp/core/tjs2/tjsNative.cpp:318-325 - a native construction
        // adds the class name, so the text "Exception" names the instance; the
        // class *object* does not, because the operator compares texts
        assert_eq!(object.is_instance_of(&text("Exception"), None), Ok(true));
        let message = object
            .prop_get(&text("message"), MemberFlags::NONE, None)
            .expect("the message");
        assert!(same(&message, &Variant::String(text("boom"))));
        let trace = object
            .prop_get(&text("trace"), MemberFlags::NONE, None)
            .expect("the trace");
        let Variant::String(trace) = trace else {
            panic!("the trace is not a text");
        };
        assert_eq!(trace.to_string().lines().count(), 2);
        let finalize = object
            .prop_get(&text("finalize"), MemberFlags::NONE, None)
            .expect("the finalizer");
        assert!(matches!(finalize, Variant::Object(_)));
    }

    /// The message a handler reads is the engine's own text.
    ///
    /// reference/cpp/core/tjs2/tjsError.h:250-253 (the conversion message),
    /// :294 (the range), :295-296 (the access), :307 (the format),
    /// reference/cpp/core/tjs2/tjsError.h:267-268 (the parameter messages),
    /// :276 (the array), :308 (the division).
    #[test]
    fn the_message_table_is_the_engines() {
        let null = Variant::Object(Closure::null());
        let octet = Variant::from(Octet::from_slice(&[1]));
        for (unwind, message) in [
            (
                conversion(null, Target::Real),
                "Cannot convert the variable type ((object)(object 0x0:0x0) to real)",
            ),
            (
                conversion(octet, Target::Text),
                "Cannot convert the variable type ((octet)<% 01 %> to string)",
            ),
            (
                conversion(Variant::Integer(1), Target::Octet),
                "Cannot convert the variable type ((int)1 to octet)",
            ),
            (
                Raise::AccessDenied.into(),
                "Invalid operation for Read-only or Write-only property",
            ),
            (Raise::RangeError.into(), "The value is out of the range"),
            (Raise::FormatString.into(), "Invalid format string"),
            (Raise::ParameterCount.into(), "Invalid argument count"),
            (Raise::InvalidParameter.into(), "Invalid argument"),
            (Raise::NotAnArray.into(), "Specify an Array object"),
            (Raise::DivideByZero.into(), "Division by zero"),
        ] {
            assert_eq!(message_of(&unwind), message);
        }
    }
}
