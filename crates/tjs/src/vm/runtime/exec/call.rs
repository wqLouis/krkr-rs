//! Calls: the four instructions that hand a value an argument list.
//!
//! A call names the value to invoke, the register the answer goes to, and — for
//! the two member forms — the member to name. What it invokes is a **dispatch
//! object**: a script function is one, a class is one, and a native method is
//! one, so the instruction is a marshalling step and not an interpreter of its
//! own. [`Dispatch::func_call`] answers a call and [`Dispatch::create_new`]
//! answers a construction.
//!
//! # The receiver
//!
//! Every form hands the callee a receiver, and the rule is one line: the value's
//! own receiver when it carries one, and the calling frame's `this` when it does
//! not. That is what makes a member read out of an object a method of it, and
//! what makes a plain function value inherit its caller's receiver.
//!
//! # The two values that are not objects
//!
//! A text and a byte string answer a member call from their own surface, so the
//! two member forms divert to it before any object lookup: the name is looked up
//! in the value's table, the call hands the table the value and the arguments,
//! and a name the table does not hold is a missing member. The two forms that do
//! not name a member do not divert — a text is not a function and is not a
//! class — and neither does a name the table does not hold.
//!
//! # The argument list
//!
//! The list is a count followed by its operands, in one of three shapes:
//!
//! | count | what follows | where the arguments come from |
//! |---|---|---|
//! | `n ≥ 0` | `n` register operands | the registers, in order |
//! | `-1` | nothing | the caller's own argument list, unchanged |
//! | `-2` | a pair count, then `(flag, register)` pairs | the pairs, in written order, each spliced as its flag says |
//!
//! The three flags are the engine's `fatNormal`, `fatExpand` and
//! `fatUnnamedExpand`: the first passes the register's value, the second splices
//! the elements of the array the register holds, and the third splices the
//! caller's surplus arguments from the signature's spread base onward. The
//! splice happens where the pair is, so the list the callee sees is the pairs in
//! the order they were written and not a list of groups.
//!
//! **There is no `arguments` object.** The spread form reads the *caller's* live
//! argument list — the same list the caller was given — so a function that
//! spreads sees what it received and nothing else. It is not a capture and no
//! environment is built.
//!
//! # The answer
//!
//! The result register receives what the call answered, and an operand address
//! of `0` is the discard register: the call still happens and its answer is
//! dropped. A call that fails raises; it does not answer a false.
//!
//! # The one value whose call is code
//!
//! A script context is code as well as a member surface, so its call is the one
//! an object surface cannot answer: the surface has no frame for it, no receiver
//! held as a value, and no channel for the raise its code makes.
//! [`Dispatch::context`] is how the instruction finds one, and the call is run
//! here: a call that names no member runs the context's own body with the
//! receiver the call hands over, and a call that names one looks the name up
//! through the value the call was made on and calls what it reaches. Every other
//! value answers through its own surface.

use std::rc::Rc;

use crate::vm::codegen::context::ContextType;
use crate::vm::runtime::natives::string::{OctetError, OctetMember, StringError, StringMember};
use crate::vm::runtime::object::{Dispatch, MemberFlags, ObjectError};
use crate::vm::runtime::octet::Octet;
use crate::vm::runtime::octet_pack::PackError;
use crate::vm::runtime::string::TjsString;
use crate::vm::runtime::variant::{Closure, Variant};

use super::frame::{Code, Frame};
use super::instruction::{Argument, Arguments, Call, Name, Slot};
use super::member::member_not_found;
use super::throw::Unwind;
use super::{ContextObject, Fault, Machine, Raise};

/// The member an array answers its length with.
const ARRAY_COUNT: &str = "count";

/// The text a declared member without a call implementation raises.
const NOT_IMPLEMENTED: &str = "Called method is not implemented";

/// The class name an array carries.
const ARRAY_CLASS: &str = "Array";

/// The argument lists a call builds its argument list in.
///
/// A call takes a list and gives it back when it returns, so a call that repeats
/// reuses one allocation. The stack is here for the same reason the frame's page
/// stack is: a nested call needs a list of its own while the caller's is still in
/// use, and the spread form of the call builds its list where it stands.
#[derive(Debug, Default)]
pub struct ArgumentStack {
    /// The lists that are free to be handed out.
    lists: Vec<Vec<Variant>>,
}

impl ArgumentStack {
    /// A stack with no list.
    pub fn new() -> Self {
        Self::default()
    }

    /// A list to build a call's arguments in.
    pub(super) fn take(&mut self) -> Vec<Variant> {
        self.lists.pop().unwrap_or_default()
    }

    /// Takes a list back, emptied for the next call.
    pub(super) fn give(&mut self, mut list: Vec<Variant>) {
        list.clear();
        self.lists.push(list);
    }
}

/// Executes a call instruction.
///
/// `argv` is the list the calling frame was given, which is what the spread
/// form reads, and `machine` is what runs the code a context callee is.
pub fn execute(
    machine: &Machine,
    code: &Code,
    frame: &mut Frame,
    argv: &[Variant],
    call: &Call,
) -> Result<(), Unwind> {
    let mut written = machine.take_argument_list();
    let answer = dispatch(machine, code, frame, argv, call, &mut written);
    machine.give_argument_list(written);
    answer
}

/// Executes one call, with `written` as the list a written argument list is
/// built in.
fn dispatch(
    machine: &Machine,
    code: &Code,
    frame: &mut Frame,
    argv: &[Variant],
    call: &Call,
    written: &mut Vec<Variant>,
) -> Result<(), Unwind> {
    // the member's name comes before the argument list, and a name that does not
    // read as text is the conversion's failure rather than the callee's
    let name = match call.named {
        Name::None => None,
        Name::Constant(index) => Some(text_of(code.value(index))?),
        Name::Register(slot) => Some(text_of(frame.slot(slot))?),
    };
    let arguments = arguments_of(code, frame, argv, call.arguments, written)?;
    let value = frame.slot(call.callee);
    // a form that names no member passes the empty name
    let named = name.as_ref().unwrap_or(machine.empty_name());
    // the two member forms divert a text and a byte string to their own surface
    let diverted = match (&name, value) {
        (Some(_), Variant::String(text)) => Some(string_call(text, named, arguments)?),
        (Some(_), Variant::Octet(octet)) => Some(octet_call(octet, named, arguments)?),
        _ => None,
    };
    let answer = match diverted {
        Some(answer) => answer,
        None => call_value(
            machine,
            value,
            named,
            arguments,
            frame.receiver(),
            call.construct,
        )?,
    };
    frame.store(call.result, answer);
    Ok(())
}

/// The answer a value gives a call, with `receiver` handed over when the value
/// carries none of its own.
fn call_value(
    machine: &Machine,
    value: &Variant,
    named: &TjsString,
    arguments: &[Variant],
    receiver: Option<Rc<dyn Dispatch>>,
    construct: bool,
) -> Result<Variant, Unwind> {
    let closure = as_closure(value)?;
    let object = closure.object.clone();
    // the receiver the callee is handed: the value's own, else the one the call
    // hands over
    let receiver = match &closure.objthis {
        Some(objthis) => Some(objthis.clone()),
        None => receiver,
    };
    let holder = match &object {
        Some(holder) => holder.clone(),
        None => return Err(not_callable()),
    };
    // a member the engine cannot hold lives on the handle itself, and a named
    // call reaches it before the engine's own member surface
    if !named.is_empty()
        && let Some(member) = holder.rust_member(named)
    {
        let none = machine.empty_name();
        return call_value(machine, &member, none, arguments, receiver, construct);
    }
    // a value whose call is code is run by the machine, which is the only part
    // that has a frame to run it in
    if let Some(context) = holder.context() {
        if construct {
            // a construction makes an object and runs the class's code in it,
            // which is not a call of the code
            return context.construct(machine, arguments);
        }
        return script_call(machine, context, &holder, named, arguments, receiver);
    }
    // an object that answers a named call from its members hands the member
    // value over here, because a call that reaches code needs this machine
    if !construct
        && !named.is_empty()
        && let Some(member) = holder.func_member(named, receiver.as_deref())
    {
        let member = member.map_err(|error| lookup_fault(error, named))?;
        if !matches!(&member, Variant::Object(_)) {
            return Err(not_callable());
        }
        let none = machine.empty_name();
        return call_value(machine, &member, none, arguments, receiver, construct);
    }
    let called = if construct {
        holder.create_new(named, Some(arguments), receiver.as_deref())
    } else {
        holder.func_call(named, Some(arguments), receiver.as_deref())
    };
    called.map_err(|error| call_fault(error, Some(named), value))
}

/// The answer a value whose call is code gives.
///
/// A context is code: a call that names no member runs the context's own body,
/// and a call that names one is the member the name reaches — the context's own
/// store, then its parent's, and, through the frame's own `this` view, the
/// object behind the context — called with no name of its own.
fn script_call(
    machine: &Machine,
    context: &ContextObject,
    holder: &Rc<dyn Dispatch>,
    named: &TjsString,
    arguments: &[Variant],
    receiver: Option<Rc<dyn Dispatch>>,
) -> Result<Variant, Unwind> {
    // a property's context is not callable: the engine answers the invalid type
    if context.kind() == ContextType::Property {
        return Err(not_callable());
    }
    if named.is_empty() {
        return machine.call_context_traced(context, receiver, arguments);
    }
    // the receiver a named call works on: the one the call hands over, and the
    // object the call was made on when it hands over none
    let receiver = match receiver {
        Some(receiver) => Some(receiver),
        None => Some(holder.clone()),
    };
    let member = holder
        .prop_get(named, MemberFlags::NONE, receiver.as_deref())
        .map_err(|error| lookup_fault(error, named))?;
    // the member is called with no name of its own, which is what the engine's
    // base object does once the lookup has answered
    let none = machine.empty_name();
    call_value(machine, &member, none, arguments, receiver, false)
}

/// The answer a text gives a member call.
///
/// A name the text's table does not hold is a missing member, which is the same
/// failure a member read of that name raises.
fn string_call(
    text: &TjsString,
    name: &TjsString,
    arguments: &[Variant],
) -> Result<Variant, Unwind> {
    let member = StringMember::lookup(name).ok_or_else(|| member_not_found(name))?;
    member
        .call(text, arguments)
        .map_err(|error| string_fault(error, arguments))
}

/// The raise a text member's failure becomes.
///
/// The failures of a member's own rule are the engine's own messages: a count it
/// does not accept, a format string it cannot read, and a value it cannot
/// convert.
fn string_fault(error: StringError, arguments: &[Variant]) -> Unwind {
    match error {
        StringError::ParameterCount { .. } => Raise::ParameterCount.into(),
        StringError::FormatString => Raise::FormatString.into(),
        StringError::Convert { parameter, error } => {
            let value = arguments.get(parameter).cloned().unwrap_or(Variant::Void);
            super::op::refused_conversion(error, &value)
        }
        StringError::LengthOutOfRange => Raise::RangeError.into(),
    }
}

/// The answer a byte string gives a member call.
fn octet_call(octet: &Octet, name: &TjsString, arguments: &[Variant]) -> Result<Variant, Unwind> {
    let member = OctetMember::lookup(name).ok_or_else(|| member_not_found(name))?;
    member
        .call(octet, arguments)
        .map_err(|error| octet_fault(error, arguments))
}

/// The raise a byte-string member's failure becomes.
///
/// The format is read as a text, so a parameter of another kind is a conversion
/// failure, and a template the pack rule cannot read names its own message: the
/// engine's template failures are one message per kind of character it did not
/// read.
fn octet_fault(error: OctetError, arguments: &[Variant]) -> Unwind {
    match error {
        OctetError::InvalidParameter { parameter } => {
            let value = arguments.get(parameter).cloned().unwrap_or(Variant::Void);
            Unwind::raised(Raise::Convert { to: "String" }).with_value(&value)
        }
        OctetError::ParameterCount { .. } => Raise::ParameterCount.into(),
        OctetError::Pack(PackError::UnknownTemplate) => Raise::UnknownPackTemplate.into(),
        OctetError::Pack(PackError::BitString) => Raise::UnknownBitString.into(),
        OctetError::Pack(PackError::HexString) => Raise::UnknownHexString.into(),
        OctetError::Pack(_) | OctetError::Array(_) => Raise::NotCallable.into(),
    }
}

/// The arguments a call hands its callee, in the order the instruction writes
/// them.
///
/// A call that passes its caller's list through reads that list where it lies,
/// so nothing is copied; a written list is built in `written`, which the call
/// gives back when it returns.
fn arguments_of<'a>(
    code: &Code,
    frame: &Frame,
    argv: &'a [Variant],
    list: Arguments,
    written: &'a mut Vec<Variant>,
) -> Result<&'a [Variant], Unwind> {
    let (start, count, spliced) = match list {
        // the caller's own list, unchanged
        Arguments::Caller => return Ok(argv),
        Arguments::Listed { start, count } => (start, count, false),
        Arguments::Expanded { start, count } => (start, count, true),
    };
    let entries = code.stream.entries(start, count)?;
    written.reserve(entries.len());
    for entry in entries {
        match (spliced, *entry) {
            (_, Argument::Value(slot)) => written.push(frame.slot(slot).clone()),
            (true, Argument::Expand(slot)) => splice_array(written, frame, slot)?,
            (true, Argument::ExpandCaller) => splice_caller(written, argv, code),
            // a listed list is a list of registers and nothing else
            (false, _) => return Err(Fault::Broken("a listed argument is not a register").into()),
        }
    }
    Ok(written)
}

/// Splices the elements of the array a register holds.
///
/// A value that is not an array is refused, because the expansion names the
/// array it expands rather than answering an empty list.
fn splice_array(arguments: &mut Vec<Variant>, frame: &Frame, slot: Slot) -> Result<(), Unwind> {
    let value = frame.slot(slot).clone();
    let closure = as_closure(&value)?;
    let object = closure
        .object
        .as_deref()
        .ok_or_else(|| not_an_array(&value))?;
    let names = object.class_names();
    if !names
        .iter()
        .any(|name| name == &TjsString::from(ARRAY_CLASS))
    {
        return Err(not_an_array(&value));
    }
    let objthis = closure.objthis.as_deref();
    let count = object.prop_get(&TjsString::from(ARRAY_COUNT), MemberFlags::NONE, objthis)?;
    let count = count
        .to_integer()
        .map_err(|error| super::op::refused_conversion(error, &count))?;
    let count =
        usize::try_from(count).map_err(|_| Fault::Broken("an array's count is out of range"))?;
    for index in 0..count {
        let index = i64::try_from(index).map_err(|_| Fault::Broken("an array is too long"))?;
        let element = object.prop_get_by_num(index, MemberFlags::NONE, objthis)?;
        arguments.push(element);
    }
    Ok(())
}

/// Splices the caller's surplus arguments.
///
/// The surplus starts at the signature's spread base and runs to the end of the
/// list the caller was given. This is the form that proves there is no capture:
/// what is spliced is the caller's own list, read now.
fn splice_caller(arguments: &mut Vec<Variant>, argv: &[Variant], code: &Code) {
    let base = code.func_decl_unnamed_arg_array_base;
    if base < 0 {
        return;
    }
    let base = usize::try_from(base).ok();
    if let Some(base) = base {
        for value in argv.iter().skip(base) {
            arguments.push(value.clone());
        }
    }
}

/// The name a member call names, as text.
fn text_of(value: &Variant) -> Result<TjsString, Unwind> {
    value
        .to_string()
        .map_err(|error| super::op::refused_conversion(error, value))
}

/// The object a call reaches.
fn as_closure(value: &Variant) -> Result<&Closure, Unwind> {
    match value {
        Variant::Object(closure) => Ok(closure),
        _ => Err(Unwind::raised(Raise::Convert { to: "Object" }).with_value(value)),
    }
}

/// The raise a failed call becomes.
///
/// A value that cannot serve a call is not a failure of the object surface but
/// the language's own "this is not callable", so it travels as a raise a script
/// can catch rather than as a refusal the caller cannot see.
fn call_fault(error: ObjectError, name: Option<&TjsString>, callee: &Variant) -> Unwind {
    match error {
        ObjectError::CallNotServed => Raise::NotCallable.into(),
        ObjectError::ParameterCount => Raise::ParameterCount.into(),
        ObjectError::NotImplemented => {
            Unwind::raised(Raise::ScriptError).with_text(TjsString::from(NOT_IMPLEMENTED))
        }
        ObjectError::MemberNotFound | ObjectError::MemberMustExist => match name {
            Some(name) => member_not_found(name),
            None => Unwind::from(error),
        },
        ObjectError::InvalidObject => {
            Unwind::raised(Raise::Convert { to: "Object" }).with_value(callee)
        }
    }
}

/// The raise for a value that names no object to call.
fn not_callable() -> Unwind {
    Raise::NotCallable.into()
}

/// The raise a name a call reaches answers when the lookup fails.
///
/// A name the callee does not hold is the language's own failure, which a script
/// can catch, and the message names the member; any other failure of the object
/// is the object's own.
fn lookup_fault(error: ObjectError, name: &TjsString) -> Unwind {
    match error {
        ObjectError::MemberNotFound => member_not_found(name),
        // a name the object declares as a property has no call implementation,
        // which the engine raises rather than failing the machine
        ObjectError::NotImplemented => {
            Unwind::raised(Raise::ScriptError).with_text(TjsString::from(NOT_IMPLEMENTED))
        }
        other => Unwind::from(other),
    }
}

/// The raise for an expansion whose value is not an array.
fn not_an_array(value: &Variant) -> Unwind {
    Unwind::raised(Raise::Convert { to: "Array" }).with_value(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::codegen::opcode::vm;
    use crate::vm::runtime::array::ArrayObject;
    use crate::vm::runtime::object::{CustomObject, Dispatch};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// What one call was handed.
    #[derive(Default)]
    struct Seen {
        /// 1 for a call, 2 for a construction, 0 for neither.
        called: u32,
        /// The member the instruction named.
        name: String,
        /// The class name of the receiver the callee was handed.
        receiver: Option<String>,
        /// The arguments the callee was handed.
        arguments: Vec<Variant>,
    }

    /// A value that answers a call and records what it was given.
    struct Callee {
        answer: Variant,
        serves: bool,
        seen: RefCell<Seen>,
    }

    impl Callee {
        fn new(answer: Variant) -> Rc<Self> {
            Rc::new(Self {
                answer,
                serves: true,
                seen: RefCell::new(Seen::default()),
            })
        }

        fn silent() -> Rc<Self> {
            Rc::new(Self {
                answer: Variant::Void,
                serves: false,
                seen: RefCell::new(Seen::default()),
            })
        }

        fn record(
            &self,
            called: u32,
            name: &TjsString,
            argv: Option<&[Variant]>,
            objthis: Option<&dyn Dispatch>,
        ) -> Result<Variant, ObjectError> {
            let mut seen = self.seen.borrow_mut();
            seen.called = called;
            seen.name = name.to_string();
            seen.receiver = objthis.and_then(|o| o.class_names().first().map(|c| c.to_string()));
            seen.arguments = argv.map(|values| values.to_vec()).unwrap_or_default();
            if !self.serves {
                return Err(ObjectError::CallNotServed);
            }
            Ok(self.answer.clone())
        }

        fn seen(&self) -> (u32, String, Option<String>, Vec<Variant>) {
            let seen = self.seen.borrow();
            (
                seen.called,
                seen.name.clone(),
                seen.receiver.clone(),
                seen.arguments.clone(),
            )
        }
    }

    impl Dispatch for Callee {
        fn prop_get(
            &self,
            _name: &TjsString,
            _flags: MemberFlags,
            _objthis: Option<&dyn Dispatch>,
        ) -> Result<Variant, ObjectError> {
            Err(ObjectError::MemberNotFound)
        }

        fn prop_set(
            &self,
            _name: &TjsString,
            _flags: MemberFlags,
            _value: &Variant,
            _objthis: Option<&dyn Dispatch>,
        ) -> Result<(), ObjectError> {
            Err(ObjectError::MemberNotFound)
        }

        fn delete_member(
            &self,
            _name: &TjsString,
            _flags: MemberFlags,
            _objthis: Option<&dyn Dispatch>,
        ) -> Result<(), ObjectError> {
            Err(ObjectError::MemberNotFound)
        }

        fn enum_members(
            &self,
            _flags: MemberFlags,
            _callback: &mut dyn FnMut(&TjsString, u32, &Variant) -> bool,
        ) -> Result<(), ObjectError> {
            Err(ObjectError::CallNotServed)
        }

        fn is_instance_of(
            &self,
            _class: &TjsString,
            _objthis: Option<&dyn Dispatch>,
        ) -> Result<bool, ObjectError> {
            Ok(false)
        }

        fn class_names(&self) -> Vec<TjsString> {
            vec![TjsString::from("callee")]
        }

        fn is_valid(&self) -> bool {
            true
        }

        fn invalidate(&self) -> bool {
            false
        }

        fn add_class_name(&self, _name: &TjsString) -> Result<(), ObjectError> {
            Ok(())
        }

        fn func_call(
            &self,
            name: &TjsString,
            argv: Option<&[Variant]>,
            objthis: Option<&dyn Dispatch>,
        ) -> Result<Variant, ObjectError> {
            self.record(1, name, argv, objthis)
        }

        fn create_new(
            &self,
            name: &TjsString,
            argv: Option<&[Variant]>,
            objthis: Option<&dyn Dispatch>,
        ) -> Result<Variant, ObjectError> {
            self.record(2, name, argv, objthis)
        }
    }

    /// An object whose class name says which receiver it is.
    fn receiver(name: &str) -> Rc<dyn Dispatch> {
        Rc::new(CustomObject::new([TjsString::from(name)]))
    }

    /// The value a call reaches: the callee, and the receiver it carries.
    fn value(callee: &Rc<Callee>, objthis: Option<Rc<dyn Dispatch>>) -> Variant {
        let object: Rc<dyn Dispatch> = callee.clone();
        Variant::Object(Closure {
            object: Some(object),
            objthis,
        })
    }

    fn register(number: i32) -> i32 {
        crate::vm::codegen::opcode::vm_reg_addr(number)
    }

    fn code_of(words: Vec<i32>, data: Vec<Variant>) -> Code {
        Code {
            code: words,
            data,
            variable_reserve_count: 2,
            max_frame_count: 8,
            ..Code::default()
        }
        .decoded()
        .expect("the stream decodes")
    }

    /// A stream that stores the value at `%1` and answers what `op` stores in
    /// `%2`.
    fn one_call(op: &[i32], data: Vec<Variant>) -> Code {
        let mut words = vec![vm::CONST, register(1), 0];
        // the call's own words start at operand slot 1, so the register it
        // stores its answer in is named by the caller's stream
        words.extend_from_slice(op);
        words.extend_from_slice(&[vm::SRV, register(2), vm::RET]);
        code_of(words, data)
    }

    fn run(
        code: &Code,
        this: Option<Rc<dyn Dispatch>>,
        argv: &[Variant],
    ) -> Result<Variant, Unwind> {
        super::super::Machine::new().run(code, this, argv)
    }

    /// A value with no receiver of its own hands the callee the calling frame's
    /// receiver (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2114-2117`).
    #[test]
    fn a_call_hands_the_frame_receiver_when_the_value_has_none() {
        let callee = Callee::new(Variant::Integer(4));
        let code = one_call(
            &[vm::CALL, register(2), register(1), 0],
            vec![value(&callee, None)],
        );
        let answer = run(&code, Some(receiver("frame-this")), &[]).expect("the call runs");
        assert!(super::super::op::discern_equals(
            &answer,
            &Variant::Integer(4)
        ));
        let (called, name, seen_receiver, arguments) = callee.seen();
        assert_eq!(called, 1);
        assert_eq!(
            name, "",
            "a call that names no member passes the empty name"
        );
        assert_eq!(seen_receiver.as_deref(), Some("frame-this"));
        assert!(arguments.is_empty());
    }

    /// A value that carries a receiver hands that one, not the frame's
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2116`).
    #[test]
    fn a_call_hands_the_values_own_receiver_when_it_has_one() {
        let callee = Callee::new(Variant::Integer(4));
        let code = one_call(
            &[vm::CALL, register(2), register(1), 0],
            vec![value(&callee, Some(receiver("callee-this")))],
        );
        let _ = run(&code, Some(receiver("frame-this")), &[]).expect("the call runs");
        let (_, _, seen_receiver, _) = callee.seen();
        assert_eq!(seen_receiver.as_deref(), Some("callee-this"));
    }

    /// The two member forms name the member, one from the constant area and one
    /// from a register (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2148-2200`,
    /// `:2197-2250`).
    #[test]
    fn the_member_forms_name_the_member() {
        let direct = Callee::new(Variant::Integer(1));
        let code = one_call(
            &[vm::CALLD, register(2), register(1), register(1), 0],
            vec![value(&direct, None), Variant::String(TjsString::from("m"))],
        );
        let _ = run(&code, None, &[]).expect("the call runs");
        assert_eq!(direct.seen().1, "m");

        let indirect = Callee::new(Variant::Integer(1));
        let code = code_of(
            vec![
                vm::CONST,
                register(1),
                register(0),
                vm::CONST,
                register(3),
                register(1),
                vm::CALLI,
                register(2),
                register(1),
                register(3),
                0,
                vm::SRV,
                register(2),
                vm::RET,
            ],
            vec![
                value(&indirect, None),
                Variant::String(TjsString::from("k")),
            ],
        );
        let _ = run(&code, None, &[]).expect("the call runs");
        assert_eq!(indirect.seen().1, "k");
    }

    /// A construction asks the value to construct, not to call
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2128-2145`).
    #[test]
    fn a_construction_asks_the_value_to_construct() {
        let callee = Callee::new(Variant::Integer(9));
        let code = one_call(
            &[vm::NEW, register(2), register(1), 0],
            vec![value(&callee, None)],
        );
        let answer = run(&code, None, &[]).expect("the construction runs");
        assert!(super::super::op::discern_equals(
            &answer,
            &Variant::Integer(9)
        ));
        assert_eq!(callee.seen().0, 2);
    }

    /// A result register of zero is the discard register: the call happens and
    /// its answer is dropped (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2114`).
    #[test]
    fn a_call_without_a_result_slot_discards_the_answer() {
        let callee = Callee::new(Variant::Integer(7));
        let code = code_of(
            vec![
                vm::CONST,
                register(1),
                register(0),
                vm::CALL,
                0,
                register(1),
                0,
                vm::RET,
            ],
            vec![value(&callee, None)],
        );
        let answer = run(&code, None, &[]).expect("the call runs");
        assert_eq!(callee.seen().0, 1, "the call still happened");
        assert!(
            matches!(answer, Variant::Void),
            "nothing was stored, so the stream answers the empty value"
        );
    }

    /// A listed count passes that many registers, in order
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2078-2098`).
    #[test]
    fn a_listed_count_passes_that_many_registers() {
        let callee = Callee::new(Variant::Void);
        let code = code_of(
            vec![
                vm::CONST,
                register(1),
                register(0),
                vm::CONST,
                register(3),
                register(1),
                vm::CONST,
                register(4),
                register(2),
                vm::CALL,
                register(2),
                register(1),
                2,
                register(3),
                register(4),
                vm::RET,
            ],
            vec![
                value(&callee, None),
                Variant::Integer(7),
                Variant::Integer(8),
            ],
        );
        let _ = run(&code, None, &[]).expect("the call runs");
        let (_, _, _, arguments) = callee.seen();
        assert_eq!(arguments.len(), 2);
        assert!(super::super::op::discern_equals(
            &arguments[0],
            &Variant::Integer(7)
        ));
        assert!(super::super::op::discern_equals(
            &arguments[1],
            &Variant::Integer(8)
        ));
    }

    /// The pass-through count hands the callee the caller's own list, so the
    /// values are the ones the caller was given
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1994-2000`).
    #[test]
    fn the_pass_through_count_hands_the_caller_its_own_arguments() {
        let callee = Callee::new(Variant::Void);
        let code = one_call(
            &[vm::CALL, register(2), register(1), -1],
            vec![value(&callee, None)],
        );
        let argv = [Variant::Integer(11), Variant::String(TjsString::from("x"))];
        let _ = run(&code, None, &argv).expect("the call runs");
        let (_, _, _, arguments) = callee.seen();
        assert_eq!(arguments.len(), 2);
        assert!(super::super::op::discern_equals(&arguments[0], &argv[0]));
        assert!(super::super::op::discern_equals(&arguments[1], &argv[1]));
    }

    /// The three flags splice in written order: one register, an array's
    /// elements, then the caller's surplus from the spread base on
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2004-2077`).
    #[test]
    fn an_expanded_list_splices_each_flag_in_written_order() {
        let callee = Callee::new(Variant::Void);
        let array = Rc::new(ArrayObject::new());
        array.append(Variant::Integer(9)).expect("an element");
        array.append(Variant::Integer(10)).expect("an element");
        let expanded: Rc<dyn Dispatch> = array;
        let code = Code {
            code: vec![
                vm::CONST,
                register(1),
                register(0),
                vm::CONST,
                register(3),
                register(1),
                vm::CONST,
                register(4),
                register(2),
                vm::CALL,
                register(2),
                register(1),
                -2,
                3,
                0,
                register(3),
                1,
                register(4),
                2,
                0,
                vm::RET,
            ],
            data: vec![
                value(&callee, None),
                Variant::Integer(5),
                Variant::Object(Closure {
                    object: Some(expanded.clone()),
                    objthis: Some(expanded),
                }),
            ],
            variable_reserve_count: 2,
            max_frame_count: 8,
            func_decl_unnamed_arg_array_base: 1,
            ..Code::default()
        }
        .decoded()
        .expect("the stream decodes");
        let argv = [
            Variant::Integer(11),
            Variant::Integer(12),
            Variant::Integer(13),
        ];
        let _ = run(&code, None, &argv).expect("the call runs");
        let (_, _, _, arguments) = callee.seen();
        let expected = [5i64, 9, 10, 12, 13];
        assert_eq!(arguments.len(), expected.len(), "{arguments:?}");
        for (value, wanted) in arguments.iter().zip(expected) {
            assert!(super::super::op::discern_equals(
                value,
                &Variant::Integer(wanted)
            ));
        }
    }

    /// An expansion of a value that is not an array is refused rather than
    /// answering an empty list
    /// (`reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1856-1863`).
    #[test]
    fn an_expansion_of_something_that_is_not_an_array_is_refused() {
        let callee = Callee::new(Variant::Void);
        let plain: Rc<dyn Dispatch> = Rc::new(CustomObject::new(Vec::new()));
        let code = Code {
            code: vec![
                vm::CONST,
                register(1),
                register(0),
                vm::CONST,
                register(4),
                register(1),
                vm::CALL,
                register(2),
                register(1),
                -2,
                1,
                1,
                register(4),
                vm::RET,
            ],
            data: vec![
                value(&callee, None),
                Variant::Object(Closure {
                    object: Some(plain.clone()),
                    objthis: Some(plain),
                }),
            ],
            variable_reserve_count: 2,
            max_frame_count: 8,
            ..Code::default()
        }
        .decoded()
        .expect("the stream decodes");
        let answer = run(&code, None, &[]);
        assert!(matches!(
            answer,
            Err(unwind) if unwind.fault() == Fault::Raised(Raise::Convert { to: "Array" })
        ));
    }

    /// A value that cannot serve a call raises, and a script can see it
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2136-2138`).
    #[test]
    fn a_value_that_is_not_callable_raises() {
        let callee = Callee::silent();
        let code = one_call(
            &[vm::CALLD, register(2), register(1), register(1), 0],
            vec![value(&callee, None), Variant::String(TjsString::from("m"))],
        );
        let answer = run(&code, None, &[]);
        assert!(matches!(
            answer,
            Err(unwind) if unwind.fault() == Fault::Raised(Raise::NotCallable)
        ));
    }

    /// A stream that calls `name` on `callee` with `arguments` and answers what
    /// the call answered.
    fn member_call(callee: Variant, name: &str, arguments: &[Variant]) -> Code {
        let mut data = vec![callee, Variant::String(TjsString::from(name))];
        let mut words = vec![vm::CONST, register(1), 0];
        let mut slots = Vec::with_capacity(arguments.len());
        for (index, argument) in arguments.iter().enumerate() {
            // the argument registers follow the callee's, and each argument's
            // constant sits at its own slot
            let position = i32::try_from(index).expect("an argument fits a register") + 4;
            let slot = i32::try_from(index).expect("an argument fits a slot") + 2;
            data.push(argument.clone());
            words.extend_from_slice(&[vm::CONST, register(position), register(slot)]);
            slots.push(register(position));
        }
        words.extend_from_slice(&[vm::CALLD, register(2), register(1), register(1)]);
        words.push(i32::try_from(slots.len()).expect("a count fits"));
        words.extend_from_slice(&slots);
        words.extend_from_slice(&[vm::SRV, register(2), vm::RET]);
        code_of(words, data)
    }

    /// A text answers a member call from its own surface, and the call never
    /// reaches an object (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2148-2175`).
    #[test]
    fn a_text_answers_its_own_member_call() {
        let code = member_call(
            Variant::String(TjsString::from("abc")),
            "charAt",
            &[Variant::Integer(1)],
        );
        let answer = run(&code, None, &[]).expect("the call runs");
        assert!(super::super::op::discern_equals(
            &answer,
            &Variant::String(TjsString::from("b"))
        ));
    }

    /// A name the text's table does not hold is a missing member, even when the
    /// same name is a property the read diversion answers
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2583-2585`).
    #[test]
    fn a_text_has_no_member_its_table_does_not_hold() {
        let code = member_call(Variant::String(TjsString::from("abc")), "length", &[]);
        assert!(matches!(
            run(&code, None, &[]),
            Err(unwind) if unwind.fault() == Fault::Raised(Raise::MemberNotFound)
        ));
    }

    /// A text member's own count decides whether the call is served, and a count
    /// it does not accept is the engine's argument-count message
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2304-2312`,
    /// `reference/krkr2/cpp/core/tjs2/tjsError.cpp:272-273`).
    #[test]
    fn a_text_member_with_a_count_it_does_not_accept_raises() {
        let code = member_call(Variant::String(TjsString::from("abc")), "charAt", &[]);
        assert!(matches!(
            run(&code, None, &[]),
            Err(unwind) if unwind.fault() == Fault::Raised(Raise::ParameterCount)
        ));
    }

    /// A byte string answers its one member, and the answer is an array
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2586-2604`).
    #[test]
    fn a_byte_string_answers_unpack() {
        let code = member_call(
            Variant::from(Octet::from_slice(&[1, 2, 3])),
            "unpack",
            &[Variant::from("c3")],
        );
        let answer = run(&code, None, &[]).expect("the call runs");
        let Variant::Object(closure) = answer else {
            panic!("the answer is not an object");
        };
        let object = closure.object.expect("the answer is an array");
        let count = object
            .prop_get(&TjsString::from("count"), MemberFlags::NONE, None)
            .expect("the array has a count");
        assert!(super::super::op::discern_equals(
            &count,
            &Variant::Integer(3)
        ));
    }

    /// A byte string has no member its table does not hold
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2602-2604`).
    #[test]
    fn a_byte_string_has_no_member_its_table_does_not_hold() {
        let code = member_call(
            Variant::from(Octet::from_slice(&[1])),
            "pack",
            &[Variant::from("c")],
        );
        assert!(matches!(
            run(&code, None, &[]),
            Err(unwind) if unwind.fault() == Fault::Raised(Raise::MemberNotFound)
        ));
    }

    /// The two forms that do not name a member do not divert: a text is not a
    /// function and not a class
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2078-2112`,
    /// `:2128-2145`).
    #[test]
    fn the_forms_without_a_member_name_do_not_divert() {
        for opcode in [vm::CALL, vm::NEW] {
            let code = one_call(
                &[opcode, register(2), register(1), 0],
                vec![Variant::String(TjsString::from("abc"))],
            );
            assert!(
                matches!(
                    run(&code, None, &[]),
                    Err(unwind) if unwind.fault() == Fault::Raised(Raise::Convert { to: "Object" })
                ),
                "the instruction {opcode} diverted"
            );
        }
    }

    /// The body a context runs: `%2 = %-1.base + %-3`, which is the receiver's
    /// member and the first declared argument.
    fn receiver_and_argument_code() -> Code {
        Code {
            code: vec![
                vm::GPD,
                register(2),
                register(-1),
                register(0),
                vm::ADD,
                register(2),
                register(-3),
                vm::SRV,
                register(2),
                vm::RET,
            ],
            data: vec![Variant::String(TjsString::from("base"))],
            max_variable_count: 1,
            variable_reserve_count: 2,
            max_frame_count: 2,
            func_decl_arg_count: 1,
            ..Code::default()
        }
        .decoded()
        .expect("the stream decodes")
    }

    /// A script context is code: an unnamed call runs its body with the receiver
    /// the call hands it and the arguments the caller passed
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2788-2797`,
    /// `:704-731` for the argument layout).
    #[test]
    fn an_unnamed_call_of_a_context_runs_its_code() {
        let context = Rc::new(ContextObject::new(
            receiver_and_argument_code(),
            ContextType::Function,
            "f",
        ));
        let this = Rc::new(CustomObject::new(Vec::new()));
        this.prop_set(
            &TjsString::from("base"),
            MemberFlags::ENSURE,
            &Variant::Integer(100),
            None,
        )
        .expect("the receiver holds the member");
        let this: Rc<dyn Dispatch> = this;
        let callee: Rc<dyn Dispatch> = context;
        let code = code_of(
            vec![
                vm::CONST,
                register(1),
                register(0),
                vm::CONST,
                register(3),
                register(1),
                vm::CALL,
                register(2),
                register(1),
                1,
                register(3),
                vm::SRV,
                register(2),
                vm::RET,
            ],
            vec![
                Variant::Object(Closure {
                    object: Some(callee),
                    objthis: None,
                }),
                Variant::Integer(41),
            ],
        );
        let answer = run(&code, Some(this), &[]).expect("the call runs");
        assert!(
            super::super::op::discern_equals(&answer, &Variant::Integer(141)),
            "the receiver's member plus the argument: 100 + 41"
        );
    }

    /// A named call of a context is the member the name reaches, called with no
    /// name of its own (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2809-2821`).
    #[test]
    fn a_named_call_of_a_context_calls_the_member_it_names() {
        let member = Rc::new(ContextObject::new(
            Code {
                code: vec![
                    vm::CONST,
                    register(1),
                    register(0),
                    vm::SRV,
                    register(1),
                    vm::RET,
                ],
                data: vec![Variant::Integer(7)],
                variable_reserve_count: 2,
                max_frame_count: 1,
                ..Code::default()
            }
            .decoded()
            .expect("the stream decodes"),
            ContextType::Function,
            "m",
        ));
        let holder = Rc::new(ContextObject::new(Code::default(), ContextType::Class, "C"));
        let member: Rc<dyn Dispatch> = member;
        holder
            .define(
                &TjsString::from("m"),
                Variant::Object(Closure {
                    object: Some(member),
                    objthis: None,
                }),
            )
            .expect("the member is stored");
        let callee: Rc<dyn Dispatch> = holder;
        let code = code_of(
            vec![
                vm::CONST,
                register(1),
                register(0),
                vm::CALLD,
                register(2),
                register(1),
                register(1),
                0,
                vm::SRV,
                register(2),
                vm::RET,
            ],
            vec![
                Variant::Object(Closure {
                    object: Some(callee),
                    objthis: None,
                }),
                Variant::String(TjsString::from("m")),
            ],
        );
        let answer = run(&code, None, &[]).expect("the call runs");
        assert!(
            super::super::op::discern_equals(&answer, &Variant::Integer(7)),
            "the member answered seven"
        );
    }

    /// A name a context does not hold is a missing member, which a script can
    /// catch (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2248-2250`).
    #[test]
    fn a_named_call_of_a_context_that_holds_no_such_member_raises() {
        let callee: Rc<dyn Dispatch> =
            Rc::new(ContextObject::new(Code::default(), ContextType::Class, "C"));
        let code = code_of(
            vec![
                vm::CONST,
                register(1),
                register(0),
                vm::CALLD,
                register(2),
                register(1),
                register(1),
                0,
                vm::RET,
            ],
            vec![
                Variant::Object(Closure {
                    object: Some(callee),
                    objthis: None,
                }),
                Variant::String(TjsString::from("nope")),
            ],
        );
        let Err(unwind) = run(&code, None, &[]) else {
            panic!("a missing member does not answer");
        };
        assert_eq!(unwind.fault(), Fault::Raised(Raise::MemberNotFound));
        assert!(
            crate::vm::runtime::exec::throw::message_of(&unwind).contains("nope"),
            "the missing member raises its own name"
        );
    }

    /// A property's context is not callable: the engine answers the invalid type
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2799-2801`).
    #[test]
    fn a_property_context_is_not_callable() {
        let callee: Rc<dyn Dispatch> = Rc::new(ContextObject::new(
            Code::default(),
            ContextType::Property,
            "p",
        ));
        let code = one_call(
            &[vm::CALL, register(2), register(1), 0],
            vec![Variant::Object(Closure {
                object: Some(callee),
                objthis: None,
            })],
        );
        assert!(
            matches!(
                run(&code, None, &[]),
                Err(unwind) if unwind.fault() == Fault::Raised(Raise::NotCallable)
            ),
            "a property is not a function"
        );
    }

    /// A call made on the frame's own `this` view: the context in front holds no
    /// such member, so the object behind the context answers, which is the
    /// engine's own view rule
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:239-247`).
    #[test]
    fn a_named_call_through_the_this_view_reaches_the_object_behind_the_context() {
        let primary: Rc<dyn Dispatch> = Rc::new(ContextObject::new(
            Code::default(),
            ContextType::TopLevel,
            "global",
        ));
        let callee = Callee::new(Variant::Integer(9));
        let behind = Rc::new(CustomObject::new(Vec::new()));
        behind
            .prop_set(
                &TjsString::from("f"),
                MemberFlags::ENSURE,
                &value(&callee, None),
                None,
            )
            .expect("the object behind the context holds the member");
        let behind: Rc<dyn Dispatch> = behind;
        let view: Rc<dyn Dispatch> =
            Rc::new(super::super::ObjThisContext::new(primary, Some(behind)));
        let code = code_of(
            vec![
                vm::CONST,
                register(1),
                register(0),
                vm::CALLD,
                register(2),
                register(1),
                register(1),
                0,
                vm::SRV,
                register(2),
                vm::RET,
            ],
            vec![
                Variant::Object(Closure {
                    object: Some(view),
                    objthis: None,
                }),
                Variant::String(TjsString::from("f")),
            ],
        );
        let answer = run(&code, None, &[]).expect("the call runs");
        assert!(
            super::super::op::discern_equals(&answer, &Variant::Integer(9)),
            "the object behind the context answered"
        );
        let (called, name, _, _) = callee.seen();
        assert_eq!(called, 1, "the member value was called once");
        assert_eq!(name, "", "a member is called with no name of its own");
    }
}
