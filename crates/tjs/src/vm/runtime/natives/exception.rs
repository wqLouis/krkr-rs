//! The `Exception` class's members: the message and the trace a handler reads,
//! and the empty finalizer every instance carries.
//!
//! The engine registers the class on its global object. A construction stores its
//! first two arguments as the instance's `message` and `trace`, each replaced
//! by the empty text when the argument is absent or the empty value; the class
//! gives every instance its empty `finalize` member. The member table is the
//! one place those names live: a construction, the object a failure becomes,
//! and a read all take their member from it.
//!
//! A message is the value it was constructed with, not a text: an integer
//! message stays an integer. A trace is the text of the frames a failure
//! passed, and the empty text for a construction that gives none.

use std::rc::Rc;

use crate::vm::runtime::object::{CustomObject, Dispatch, MemberFlags, ObjectError};
use crate::vm::runtime::string::TjsString;
use crate::vm::runtime::variant::{Closure, Variant};

/// The name the class is registered under.
pub const CLASS_NAME: &str = "Exception";

/// A member of the `Exception` class.
///
/// [`ExceptionMember::ALL`] lists the members in the order the class gives
/// them to an instance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExceptionMember {
    /// The empty finalizer the class itself carries.
    Finalize,
    /// The value the construction's first argument gave.
    Message,
    /// The value the construction's second argument gave.
    Trace,
}

impl ExceptionMember {
    /// Every member, in the order the class gives them to an instance.
    pub const ALL: &'static [ExceptionMember] = &[Self::Finalize, Self::Message, Self::Trace];

    /// The member `name` denotes, or `None` when it names no member.
    pub fn lookup(name: &TjsString) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|member| name.len() == member.name().len() && name.eq_str(member.name()))
    }

    /// The name the class publishes the member under.
    pub fn name(self) -> &'static str {
        match self {
            Self::Finalize => "finalize",
            Self::Message => "message",
            Self::Trace => "trace",
        }
    }

    /// The construction parameter that sets the member, or `None` for a member
    /// the class itself carries.
    pub fn parameter(self) -> Option<usize> {
        match self {
            Self::Finalize => None,
            Self::Message => Some(0),
            Self::Trace => Some(1),
        }
    }

    /// Whether the class itself carries the member.
    pub fn is_class_member(self) -> bool {
        self.parameter().is_none()
    }

    /// The fewest parameters a call accepts, or `None` for a member that is
    /// read rather than called.
    pub fn min_params(self) -> Option<usize> {
        match self {
            Self::Finalize => Some(0),
            Self::Message | Self::Trace => None,
        }
    }
}

/// The state of one `Exception` instance.
///
/// A construction fills the two values from its arguments; a failure of the
/// engine fills them from its message and trace.
#[derive(Clone, Debug)]
pub struct Exception {
    /// The value the `message` member reads.
    pub message: Variant,
    /// The value the `trace` member reads.
    pub trace: Variant,
}

impl Exception {
    /// The exception a construction makes.
    ///
    /// The first argument is the message and the second is the trace. An
    /// argument that is absent or the empty value is the empty text, and
    /// arguments past the second are ignored.
    pub fn new(arguments: &[Variant]) -> Self {
        let mut exception = Self {
            message: Variant::from(""),
            trace: Variant::from(""),
        };
        for member in Self::members() {
            let Some(index) = member.parameter() else {
                continue;
            };
            let value = match Self::argument(arguments, index) {
                Some(value) => value,
                None => Variant::from(""),
            };
            match member {
                ExceptionMember::Message => exception.message = value,
                ExceptionMember::Trace => exception.trace = value,
                ExceptionMember::Finalize => {}
            }
        }
        exception
    }

    /// The exception a failure of the engine makes.
    pub fn raised(message: Variant, trace: Variant) -> Self {
        Self { message, trace }
    }

    /// The object the exception's members are set on.
    ///
    /// Every member of the table is set from this exception's state, and the
    /// finalizer is the class's own empty method with the object as its
    /// receiver. The object carries the class's name, which is what a class
    /// question asks about; a class object on the right of `instanceof` is read
    /// as its own text form and so never matches that name.
    ///
    /// # Errors
    ///
    /// The object surface's failure while a member is set.
    pub fn object(&self) -> Result<Variant, ObjectError> {
        let members = Rc::new(CustomObject::new([TjsString::from(CLASS_NAME)]));
        let object: Rc<dyn Dispatch> = members.clone();
        for member in Self::members() {
            members.prop_set(
                &TjsString::from(member.name()),
                MemberFlags::ENSURE,
                &self.member(member, &object),
                Some(object.as_ref()),
            )?;
        }
        Ok(Variant::Object(Closure {
            object: Some(object.clone()),
            objthis: Some(object),
        }))
    }

    /// The members an instance carries.
    fn members() -> impl Iterator<Item = ExceptionMember> {
        ExceptionMember::ALL.iter().copied()
    }

    /// The value of one member on an instance.
    fn member(&self, member: ExceptionMember, object: &Rc<dyn Dispatch>) -> Variant {
        match member {
            ExceptionMember::Finalize => {
                let finalize: Rc<dyn Dispatch> = Rc::new(Finalizer);
                Variant::Object(Closure {
                    object: Some(finalize),
                    objthis: Some(object.clone()),
                })
            }
            ExceptionMember::Message => self.message.clone(),
            ExceptionMember::Trace => self.trace.clone(),
        }
    }

    /// The argument at `index`, with the empty value read as absent.
    fn argument(arguments: &[Variant], index: usize) -> Option<Variant> {
        match arguments.get(index) {
            Some(Variant::Void) | None => None,
            Some(value) => Some(value.clone()),
        }
    }
}

/// The empty finalizer every instance carries.
struct Finalizer;

impl Dispatch for Finalizer {
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
        Ok(())
    }

    fn is_instance_of(
        &self,
        class: &TjsString,
        _objthis: Option<&dyn Dispatch>,
    ) -> Result<bool, ObjectError> {
        Ok(class.eq_str("Object") || class.eq_str("Function"))
    }

    fn class_names(&self) -> Vec<TjsString> {
        Vec::new()
    }

    fn add_class_name(&self, _name: &TjsString) -> Result<(), ObjectError> {
        Err(ObjectError::CallNotServed)
    }

    fn is_valid(&self) -> bool {
        true
    }

    fn invalidate(&self) -> bool {
        false
    }

    fn func_call(
        &self,
        name: &TjsString,
        _argv: Option<&[Variant]>,
        _objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        if name.is_empty() {
            Ok(Variant::Void)
        } else {
            Err(ObjectError::MemberNotFound)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CLASS_NAME, Exception, ExceptionMember};
    use crate::vm::runtime::object::MemberFlags;
    use crate::vm::runtime::string::TjsString;
    use crate::vm::runtime::variant::Variant;

    // reference/krkr2/cpp/core/tjs2/tjsException.cpp:22-52 — the class's one
    // member and its two-argument construction

    /// Reads a member of the built object.
    fn member(exception: &Exception, member: ExceptionMember) -> Variant {
        let Variant::Object(closure) = exception.object().expect("the object builds") else {
            panic!("the exception is not an object");
        };
        let object = closure.object.expect("the object is there");
        object
            .prop_get(&TjsString::from(member.name()), MemberFlags::NONE, None)
            .expect("the member reads")
    }

    /// The construction takes the first two arguments and defaults both to the
    /// empty text, and an absent value is the empty text.
    #[test]
    fn a_construction_fills_the_message_and_the_trace() {
        let exception = Exception::new(&[]);
        assert!(
            matches!(member(&exception, ExceptionMember::Message), Variant::String(text) if text.is_empty())
        );
        assert!(
            matches!(member(&exception, ExceptionMember::Trace), Variant::String(text) if text.is_empty())
        );
        let exception = Exception::new(&[
            Variant::from("m"),
            Variant::from("t"),
            Variant::from("ignored"),
        ]);
        assert!(
            matches!(member(&exception, ExceptionMember::Message), Variant::String(text) if text == TjsString::from("m"))
        );
        assert!(
            matches!(member(&exception, ExceptionMember::Trace), Variant::String(text) if text == TjsString::from("t"))
        );
        let exception = Exception::new(&[Variant::Void, Variant::Void]);
        assert!(
            matches!(member(&exception, ExceptionMember::Message), Variant::String(text) if text.is_empty())
        );
        assert!(
            matches!(member(&exception, ExceptionMember::Trace), Variant::String(text) if text.is_empty())
        );
    }

    /// The object carries exactly the table's members and the class's name.
    #[test]
    // reference/krkr2/cpp/core/tjs2/tjsException.cpp:22-52 - the class publishes
    // its finalizer and its constructor, and a construction adds the class's name
    // to the object it makes
    fn the_object_carries_the_table_and_its_class_name() {
        let exception = Exception::new(&[]);
        let Variant::Object(closure) = exception.object().expect("the object builds") else {
            panic!("the exception is not an object");
        };
        let object = closure.object.expect("the object is there");
        assert_eq!(object.class_names(), vec![TjsString::from(CLASS_NAME)]);
        assert_eq!(
            object.is_instance_of(&TjsString::from(CLASS_NAME), None),
            Ok(true)
        );
        let mut names = Vec::new();
        object
            .enum_members(MemberFlags::NONE, &mut |name, _, _| {
                names.push(name.to_string());
                true
            })
            .expect("the members enumerate");
        let mut expected: Vec<String> = ExceptionMember::ALL
            .iter()
            .map(|member| member.name().to_owned())
            .collect();
        names.sort();
        expected.sort();
        assert_eq!(names, expected);
    }

    /// The finalizer is a method value that answers the empty value.
    #[test]
    fn the_finalizer_is_a_method() {
        let exception = Exception::new(&[]);
        let Variant::Object(closure) = member(&exception, ExceptionMember::Finalize) else {
            panic!("the finalizer is not an object");
        };
        let finalize = closure.object.expect("the finalizer is there");
        assert_eq!(
            finalize.is_instance_of(&TjsString::from("Function"), None),
            Ok(true)
        );
        let answer = finalize
            .func_call(&TjsString::new(), Some(&[]), None)
            .expect("the finalizer is callable");
        assert!(matches!(answer, Variant::Void));
    }
}
