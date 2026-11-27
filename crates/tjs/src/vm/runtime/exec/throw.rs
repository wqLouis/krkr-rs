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
//! One line per frame the failure passed, innermost first. An entry adds its own
//! line as the failure crosses out of it, so the list reads in unwind order. The
//! engine's message carries the lines; a script's thrown value does not — it is
//! the value, and the trace travels beside it for whoever reports it.
//!
//! # Where the trace is dropped
//!
//! The Rust path keeps the trace and the value for as long as the failure stays
//! inside the interpreter. It is lost at the two boundaries that cannot carry
//! it: the object surface's `func_call` answers an [`ObjectError`], so a failure
//! leaving a called context becomes the surface's own refusal; and the C
//! interface between this crate and the vendored executor carries a message
//! only. The second is the known seam defect; the first is why
//! [`super::Machine::run_context`] exists.

use crate::vm::codegen::opcode::vm;
use crate::vm::runtime::natives::exception::Exception;
use crate::vm::runtime::object::ObjectError;
use crate::vm::runtime::string::TjsString;
use crate::vm::runtime::variant::Variant;

use super::decode;
use super::frame::{Code, Frame};
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

/// A failure on its way out of the code, and the frames it has passed.
///
/// This is what the run loop answers with, so that a failure can carry a value
/// and a trace across the frames of the code it unwinds. It becomes a [`Fault`]
/// at the machine's own boundary.
#[derive(Clone, Debug)]
pub struct Unwind {
    /// What failed.
    failure: Fault,
    /// The frames it passed, innermost first.
    trace: Vec<FrameTrace>,
}

impl Unwind {
    /// A failure that has not passed a frame yet.
    pub fn new(failure: Fault) -> Self {
        Self {
            failure,
            trace: Vec::new(),
        }
    }

    /// A failure that passes every protected region it meets.
    ///
    /// The silent kind exists so that a cancellation is not turned into a value
    /// a catch could inspect.
    pub fn silent() -> Self {
        Self::new(Fault::Silent)
    }

    /// The failure itself, with the trace dropped.
    pub fn fault(self) -> Fault {
        self.failure
    }

    /// The frames the failure has passed.
    pub fn trace(&self) -> &[FrameTrace] {
        &self.trace
    }

    /// Adds the frame the failure has just left.
    fn push(&mut self, frame: FrameTrace) {
        self.trace.push(frame);
    }

    /// Whether the failure must pass a protected region by.
    fn is_silent(&self) -> bool {
        matches!(self.failure, Fault::Silent)
    }
}

impl From<Fault> for Unwind {
    fn from(failure: Fault) -> Self {
        Self::new(failure)
    }
}

impl From<ObjectError> for Unwind {
    fn from(error: ObjectError) -> Self {
        Self::new(Fault::from(error))
    }
}

/// Whether an opcode is one of the three exception instructions.
pub fn handles(opcode: i32) -> bool {
    matches!(opcode, vm::ENTRY | vm::EXTRY | vm::THROW)
}

/// Executes an exception instruction.
///
/// It answers the answer of the whole context, because an entry runs the body and
/// an exit ends it: the caller has nothing left to do afterwards.
pub fn execute(
    machine: &super::Machine,
    code: &Code,
    frame: &mut Frame,
    argv: &[Variant],
    context: Option<&ContextObject>,
    ip: usize,
    opcode: i32,
) -> Result<Variant, Unwind> {
    match opcode {
        vm::ENTRY => enter(machine, code, frame, argv, context, ip),
        // an exit from a protected region ends the context, exactly as a return
        // does
        vm::EXTRY => Ok(Variant::Void),
        vm::THROW => Err(throw(frame, code, context, ip)?),
        _ => Err(Fault::Broken("an exception instruction that is not one").into()),
    }
}

/// Runs a protected region and answers for it.
///
/// The body runs from the word after the entry's two operands. A failure of the
/// body crosses out of this entry: it takes this frame's line, and — unless it
/// is the silent kind — the catch's register receives it and the catch's body
/// runs from the entry's own distance.
fn enter(
    machine: &super::Machine,
    code: &Code,
    frame: &mut Frame,
    argv: &[Variant],
    context: Option<&ContextObject>,
    ip: usize,
) -> Result<Variant, Unwind> {
    let distance = operand(code, ip, 1)?;
    let register = operand(code, ip, 2)?;
    let catch = catch_position(ip, distance)?;
    match machine.execute(code, frame, argv, context, ip + 3) {
        Ok(answer) => Ok(answer),
        Err(mut unwind) => {
            unwind.push(line_of(context, ip));
            if unwind.is_silent() {
                return Err(unwind);
            }
            deliver(&unwind, frame, register)?;
            machine.execute(code, frame, argv, context, catch)
        }
    }
}

/// The position the catch's body starts at.
fn catch_position(ip: usize, distance: i32) -> Result<usize, Unwind> {
    let offset = decode::code_position(distance)?;
    let target = i64::try_from(ip)
        .map_err(|_| Fault::Broken("a code position does not fit an address"))?
        .checked_add(i64::from(offset))
        .ok_or(Fault::Broken("a catch position is out of range"))?;
    if target < 0 {
        return Err(Fault::Broken("a catch position is before the code stream").into());
    }
    usize::try_from(target).map_err(|_| Fault::Broken("a catch position is out of range").into())
}

/// Hands a failure to the catch's register.
///
/// A script's thrown value goes there as itself. The engine's own failure becomes
/// an exception object carrying the message and the trace, because that is what a
/// handler reads. A failure that is neither — a defective code stream, say —
/// leaves the register empty, which is what the engine's own catch-all does.
fn deliver(unwind: &Unwind, frame: &mut Frame, register: i32) -> Result<(), Unwind> {
    let value = match &unwind.failure {
        Fault::Thrown { value } => (**value).clone(),
        Fault::Raised(raise) => exception_object(&message_of(raise), unwind.trace())?,
        Fault::Silent => Variant::Void,
        _ => Variant::Void,
    };
    frame.set(register, value).map_err(Unwind::from)
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

/// The message a reason reads as.
///
/// The engine's own text where this tree knows it, and a plain statement of the
/// reason where it does not. A handler that prints `message` sees these.
fn message_of(raise: &Raise) -> String {
    match raise {
        Raise::Convert { to, .. } => format!("Cannot convert the variable type to {to}"),
        Raise::DivideByZero => String::from("Division by zero"),
        Raise::IntegerOutOfRange => String::from("The integer result is out of range"),
        Raise::MemberNotFound { name } => format!("Member \"{name}\" does not exist"),
        Raise::AccessDenied => String::from("Access denied"),
        Raise::RangeError => String::from("RangeError"),
        Raise::NotCallable { .. } => String::from("Not a function or invalid method/property type"),
        Raise::ScriptError { message } => message.to_string(),
    }
}

/// The failure a throw instruction raises.
fn throw(
    frame: &Frame,
    code: &Code,
    context: Option<&ContextObject>,
    ip: usize,
) -> Result<Unwind, Fault> {
    let address = operand(code, ip, 1)?;
    let value = frame.reg(address)?.clone();
    let mut unwind = Unwind::new(Fault::Thrown {
        value: Box::new(value),
    });
    unwind.push(line_of(context, ip));
    Ok(unwind)
}

/// The trace line for the frame at `ip`.
fn line_of(context: Option<&ContextObject>, ip: usize) -> FrameTrace {
    let name = match context {
        Some(context) => context.name().to_string(),
        None => String::new(),
    };
    FrameTrace { name, position: ip }
}

/// The `slot`-th operand of the instruction at `ip`.
fn operand(code: &Code, ip: usize, slot: usize) -> Result<i32, Fault> {
    super::operand(&code.code, ip, slot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    use crate::vm::runtime::object::{CustomObject, Dispatch, MemberFlags};
    use crate::vm::runtime::variant::Closure;

    fn text(value: &str) -> TjsString {
        TjsString::from(value)
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
            Err(Fault::Thrown { value }) => {
                assert!(same(&value, &Variant::String(text("uncaught"))));
            }
            other => panic!("expected the thrown value, got {other:?}"),
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
        // reference/cpp/core/tjs2/tjsException.cpp:22-52 - a native class's
        // instance is not an instance of that class
        assert_eq!(object.is_instance_of(&text("Exception"), None), Ok(false));
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

    /// The three opcodes are the ones handled, and no others.
    #[test]
    fn the_three_instructions_are_the_ones_handled() {
        for opcode in [vm::ENTRY, vm::EXTRY, vm::THROW] {
            assert!(handles(opcode));
        }
        assert!(!handles(vm::CALL));
        assert!(!handles(vm::RET));
    }
}
