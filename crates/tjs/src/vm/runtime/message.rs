//! The message table: the text each registered message id reads as.
//!
//! A message is an id and a default text. A raise names one of the ids and
//! reads the text when it happens, so an assignment made after the raising
//! code was written still changes a later raise. The text may carry `%1`,
//! `%2`, ...; the raise substitutes its own arguments, and a text a script
//! assigned may carry the placeholders too.
//!
//! The assignment map belongs to the thread that owns the engine. An id the
//! table does not hold is not assignable: [`assign`] answers false and stores
//! nothing.

use std::cell::RefCell;

use super::hash::HashTable;
use super::string::TjsString;
use super::variant::DIVISION_BY_ZERO_MESSAGE;

/// The id of the conversion failure that names the target kind.
pub const TJS_VARIANT_CONVERT_ERROR: &str = "TJSVariantConvertError";

/// The id of the conversion failure whose target is an object.
pub const TJS_VARIANT_CONVERT_ERROR_TO_OBJECT: &str = "TJSVariantConvertErrorToObject";

/// The id of a division by zero.
pub const TJS_DIVIDE_BY_ZERO: &str = "TJSDivideByZero";

/// The id of an integer result that does not fit a 64-bit integer.
pub const TJS_INTEGER_OUT_OF_RANGE: &str = "TJSIntegerOutOfRange";

/// The id of a member that does not exist.
pub const TJS_MEMBER_NOT_FOUND: &str = "TJSMemberNotFound";

/// The id of a write to a read-only property, or a read of a write-only one.
pub const TJS_ACCESS_DENIED: &str = "TJSAccessDenyed";

/// The id of a subscript outside the value it indexes.
pub const TJS_RANGE_ERROR: &str = "TJSRangeError";

/// The id of a value that cannot be called.
pub const TJS_INVALID_TYPE: &str = "TJSInvalidType";

/// The id of a member access on a null object.
pub const TJS_NULL_ACCESS: &str = "TJSNullAccess";

/// The id of a call with the wrong number of arguments.
pub const TJS_BAD_PARAM_COUNT: &str = "TJSBadParamCount";

/// The id of a call with an argument the rule cannot use.
pub const TJS_INVALID_PARAM: &str = "TJSInvalidParam";

/// The id of a format string the rule cannot read.
pub const TJS_INVALID_FORMAT_STRING: &str = "TJSInvalidFormatString";

/// The id of an argument expansion whose value is not an array.
pub const TJS_SPECIFY_ARRAY: &str = "TJSSpecifyArray";

/// The id of a declared member that has no call implementation.
pub const TJS_NOT_IMPLEMENTED: &str = "TJSNotImplemented";

/// The id of a pack or unpack template that names no kind it reads.
pub const TJS_UNKNOWN_PACK_UNPACK_TEMPLATE: &str = "TJSUnknownPackUnpackTemplateCharcter";

/// The id of a bit run that holds a character that is not a bit.
pub const TJS_UNKNOWN_BIT_STRING_CHARACTER: &str = "TJSUnknownBitStringCharacter";

/// The id of a hex run that holds a character that is not a hex digit.
pub const TJS_UNKNOWN_HEX_STRING_CHARACTER: &str = "TJSUnknownHexStringCharacter";

/// One registered message id and its default text.
pub struct Message {
    /// The id a script passes to `System.assignMessage`.
    pub id: &'static str,
    /// The text the id reads as before any assignment.
    pub text: &'static str,
}

/// Every registered message, with its default text.
pub const MESSAGES: &[Message] = &[
    Message {
        id: "TJSInternalError",
        text: "Internal error",
    },
    Message {
        id: "TJSWarning",
        text: "Warning: ",
    },
    Message {
        id: "TJSWarnEvalOperator",
        text: "Non-global post-! operator is used (note that the post-! operator behavior is changed on TJS2 version 2.4.1)",
    },
    Message {
        id: "TJSNarrowToWideConversionError",
        text: "Cannot convert given narrow string to wide string",
    },
    Message {
        id: TJS_VARIANT_CONVERT_ERROR,
        text: "Cannot convert the variable type (%1 to %2)",
    },
    Message {
        id: TJS_VARIANT_CONVERT_ERROR_TO_OBJECT,
        text: "Cannot convert the variable type (%1 to Object)",
    },
    Message {
        id: "TJSIDExpected",
        text: "Specify an ID",
    },
    Message {
        id: "TJSSubstitutionInBooleanContext",
        text: "Substitution in boolean context (use form of '(A=B)!=0' to compare to zero)",
    },
    Message {
        id: "TJSCannotModifyLHS",
        text: "This expression cannot be used as a lvalue",
    },
    Message {
        id: "TJSInsufficientMem",
        text: "Insufficient memory",
    },
    Message {
        id: "TJSCannotGetResult",
        text: "Cannot get the value of this expression",
    },
    Message {
        id: TJS_NULL_ACCESS,
        text: "Accessing to nullptr object",
    },
    Message {
        id: TJS_MEMBER_NOT_FOUND,
        text: "Member \"%1\" does not exist",
    },
    Message {
        id: "TJSMemberNotFoundNoNameGiven",
        text: "Member does not exist",
    },
    Message {
        id: TJS_NOT_IMPLEMENTED,
        text: "Called method is not implemented",
    },
    Message {
        id: TJS_INVALID_PARAM,
        text: "Invalid argument",
    },
    Message {
        id: TJS_BAD_PARAM_COUNT,
        text: "Invalid argument count",
    },
    Message {
        id: TJS_INVALID_TYPE,
        text: "Not a function or invalid method/property type",
    },
    Message {
        id: "TJSSpecifyDicOrArray",
        text: "Specify a Dictionary object or an Array object",
    },
    Message {
        id: TJS_SPECIFY_ARRAY,
        text: "Specify an Array object",
    },
    Message {
        id: "TJSStringDeallocError",
        text: "Cannot free the string memory block",
    },
    Message {
        id: "TJSStringAllocError",
        text: "Cannot allocate the string memory block",
    },
    Message {
        id: "TJSMisplacedBreakContinue",
        text: "Cannot place \"break\" or \"continue\" here",
    },
    Message {
        id: "TJSMisplacedCase",
        text: "Cannot place \"case\" here",
    },
    Message {
        id: "TJSMisplacedReturn",
        text: "Cannot place \"return\" here",
    },
    Message {
        id: "TJSStringParseError",
        text: "Un-terminated string/regexp/octet literal",
    },
    Message {
        id: "TJSNumberError",
        text: "Cannot be parsed as a number",
    },
    Message {
        id: "TJSUnclosedComment",
        text: "Un-terminated comment",
    },
    Message {
        id: "TJSInvalidChar",
        text: "Invalid character '%1'",
    },
    Message {
        id: "TJSExpected",
        text: "Expected %1",
    },
    Message {
        id: "TJSSyntaxError",
        text: "Syntax error (%1)",
    },
    Message {
        id: "TJSPPError",
        text: "Error in conditional compiling expression",
    },
    Message {
        id: "TJSCannotGetSuper",
        text: "Super class does not exist or cannot specify the super class",
    },
    Message {
        id: "TJSInvalidOpecode",
        text: "Invalid VM code",
    },
    Message {
        id: TJS_RANGE_ERROR,
        text: "The value is out of the range",
    },
    Message {
        id: TJS_ACCESS_DENIED,
        text: "Invalid operation for Read-only or Write-only property",
    },
    Message {
        id: "TJSNativeClassCrash",
        text: "Invalid object context",
    },
    Message {
        id: "TJSInvalidObject",
        text: "The object is already invalidated",
    },
    Message {
        id: "TJSDuplicatedPropHandler",
        text: "Duplicated \"setter\" or \"getter\"",
    },
    Message {
        id: "TJSCannotOmit",
        text: "\"...\" is used out of functions",
    },
    Message {
        id: "TJSCannotParseDate",
        text: "Invalid date format",
    },
    Message {
        id: "TJSInvalidValueForTimestamp",
        text: "Invalid value for date/time",
    },
    Message {
        id: "TJSExceptionNotFound",
        text: "Cannot convert exception because \"Exception\" does not exist",
    },
    Message {
        id: TJS_INVALID_FORMAT_STRING,
        text: "Invalid format string",
    },
    Message {
        id: TJS_DIVIDE_BY_ZERO,
        text: DIVISION_BY_ZERO_MESSAGE,
    },
    Message {
        id: TJS_INTEGER_OUT_OF_RANGE,
        text: "The integer result is out of range",
    },
    Message {
        id: "TJSNotReconstructiveRandomizeData",
        text: "Cannot reconstruct random seeds",
    },
    Message {
        id: "TJSSymbol",
        text: "ID",
    },
    Message {
        id: "TJSCallHistoryIsFromOutOfTJS2Script",
        text: "[out of TJS2 script]",
    },
    Message {
        id: "TJSNObjectsWasNotFreed",
        text: "Total %1 Object(s) was not freed",
    },
    Message {
        id: "TJSObjectCreationHistoryDelimiter",
        text: " <-- ",
    },
    Message {
        id: "TJSObjectWasNotFreed",
        text: "Object %1 [%2] wes not freed / The object was created at : %2",
    },
    Message {
        id: "TJSGroupByObjectTypeAndHistory",
        text: "Group by object type and location where the object was created",
    },
    Message {
        id: "TJSGroupByObjectType",
        text: "Group by object type",
    },
    Message {
        id: "TJSObjectCountingMessageGroupByObjectTypeAndHistory",
        text: "%1 time(s) : [%2] %3",
    },
    Message {
        id: "TJSObjectCountingMessageTJSGroupByObjectType",
        text: "%1 time(s) : [%2]",
    },
    Message {
        id: "TJSWarnRunningCodeOnDeletingObject",
        text: "%4: Running code on deleting-in-progress object %1[%2] / The object was created at : %3",
    },
    Message {
        id: "TJSWriteError",
        text: "Write error",
    },
    Message {
        id: "TJSReadError",
        text: "Read error",
    },
    Message {
        id: "TJSSeekError",
        text: "Seek error",
    },
    Message {
        id: "TJSByteCodeBroken",
        text: "Bytecode read error. File is broken or it's not bytecode file.",
    },
    Message {
        id: "TJSObjectHashMapLogVersionMismatch",
        text: "Object Hash Map log version mismatch",
    },
    Message {
        id: "TJSCurruptedObjectHashMapLog",
        text: "Currupted Object Hash Map log",
    },
    Message {
        id: "TJSUnknownFailure",
        text: "Unknown failure : %08X",
    },
    Message {
        id: TJS_UNKNOWN_PACK_UNPACK_TEMPLATE,
        text: "Unknown pack/unpack TEMPLATE charcter",
    },
    Message {
        id: TJS_UNKNOWN_BIT_STRING_CHARACTER,
        text: "Unknown bit string charcter",
    },
    Message {
        id: TJS_UNKNOWN_HEX_STRING_CHARACTER,
        text: "Unknown Hex string charcter",
    },
    Message {
        id: "TJSNotSupportedUuencode",
        text: "Not supported uuencode",
    },
    Message {
        id: "TJSNotSupportedBER",
        text: "Not supported BER",
    },
    Message {
        id: "TJSNotSupportedUnpackLP",
        text: "Not supported unpack 'lp'",
    },
    Message {
        id: "TJSNotSupportedUnpackP",
        text: "Not supported unpack 'p'",
    },
];

thread_local! {
    /// The texts assigned on this thread, by message id.
    static ASSIGNED: RefCell<HashTable<TjsString, TjsString>> = RefCell::new(HashTable::new());
}

/// The table entry an id names, when the id is registered.
fn find(id: &str) -> Option<&'static Message> {
    MESSAGES.iter().find(|message| message.id == id)
}

/// The default text of a registered id.
///
/// `None` means the id is not registered, which is the same condition
/// [`assign`] reports with `false`.
pub fn default_text(id: &str) -> Option<&'static str> {
    find(id).map(|message| message.text)
}

/// The text a registered id reads as now: its assigned text, or its default.
///
/// `None` means the id is not registered.
pub fn text(id: &str) -> Option<String> {
    let message = find(id)?;
    let key = TjsString::from(id);
    ASSIGNED.with(|assigned| {
        let assigned = assigned.borrow();
        match assigned.get(&key) {
            Some(text) => Some(text.to_string()),
            None => Some(message.text.to_string()),
        }
    })
}

/// Replaces the text of a registered id.
///
/// An id the table does not hold is not stored and answers `false`; a
/// registered id answers `true`, including when the assigned text is empty.
pub fn assign(id: &str, message: &str) -> bool {
    if find(id).is_none() {
        return false;
    }
    let key = TjsString::from(id);
    let value = TjsString::from(message);
    ASSIGNED.with(|assigned| {
        assigned.borrow_mut().insert(key, value);
    });
    true
}

/// Restores every registered id to its default text.
pub fn reset() {
    ASSIGNED.with(|assigned| assigned.borrow_mut().clear());
}

/// The current text of a registered id with `%1`, `%2`, ... substituted.
///
/// `None` means the id is not registered.
pub fn format(id: &str, arguments: &[&str]) -> Option<String> {
    text(id).map(|template| substitute(&template, arguments))
}

/// Replaces each `%n` in `template` with the `n`-th argument.
///
/// The replacements run in argument order, and every occurrence is replaced.
/// A later replacement therefore also reaches a marker an earlier argument
/// carried, which keeps the substitution rule independent of how a text was
/// built.
pub fn substitute(template: &str, arguments: &[&str]) -> String {
    let mut text = template.to_owned();
    for (index, argument) in arguments.iter().enumerate() {
        let marker = format!("%{}", index + 1);
        text = text.replace(&marker, argument);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{
        TJS_BAD_PARAM_COUNT, TJS_VARIANT_CONVERT_ERROR, assign, default_text, format, reset,
        substitute, text,
    };

    /// A registered id reads its default until it is assigned, and an unknown
    /// id is neither read nor stored
    /// (`reference/krkr2/cpp/core/tjs2/tjsMessage.cpp:36-48`).
    #[test]
    fn an_assignment_is_read_by_a_later_lookup() {
        reset();
        assert_eq!(
            default_text(TJS_BAD_PARAM_COUNT),
            Some("Invalid argument count")
        );
        assert_eq!(
            text(TJS_BAD_PARAM_COUNT).as_deref(),
            Some("Invalid argument count")
        );
        assert!(assign(TJS_BAD_PARAM_COUNT, "count changed"));
        assert_eq!(
            format(TJS_BAD_PARAM_COUNT, &[]).as_deref(),
            Some("count changed")
        );
        assert!(!assign("not-a-message", "ignored"));
        assert_eq!(text("not-a-message"), None);
        reset();
    }

    /// The placeholders stay in the stored text and are substituted when the
    /// text is read (`reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:146-176`).
    #[test]
    fn a_placeholder_is_substituted_when_the_text_is_read() {
        reset();
        assert!(assign(TJS_VARIANT_CONVERT_ERROR, "changed %1 to %2"));
        assert_eq!(
            format(TJS_VARIANT_CONVERT_ERROR, &["the value", "int"]).as_deref(),
            Some("changed the value to int")
        );
        assert_eq!(
            substitute("%1 and %2 and %1", &["first", "second"]),
            "first and second and first"
        );
        reset();
    }
}
