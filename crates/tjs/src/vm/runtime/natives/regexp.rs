//! The `RegExp` class: the pattern, the match it last made, and the members a
//! script reads from it.
//!
//! A pattern is compiled by the regular-expression engine in its Perl syntax
//! over UTF-16 code units. The class keeps the engine's own expression, the
//! option bits the flags produced, and the state one search leaves behind: the
//! match array, the input, the match's position, the last match, the last
//! captured group, and the text on either side of the match. The member table
//! is the one place those names live.
//!
//! A search starts at `start`, which a script may write, and a global pattern
//! moves `start` to the end of the match after every search that matches. A
//! pattern that does not match still replaces the array, the input, the index,
//! the last index and the last match; the right context keeps the value the
//! previous match gave it.
//!
//! # What the class answers
//!
//! `test` answers whether the search matched and changes the state, `exec`
//! answers the match array and changes it, and `match` answers an array without
//! changing anything. `replace` and `split` read the pattern but leave the
//! state alone.
//!
//! The replacement of `replace` is a text used as it is: a dollar sign and the
//! digits after it are not a group substitution. When the replacement is an
//! object, the call hands it the match array and uses the text of its answer.
//!
//! # The class object
//!
//! A `last` property belongs to the class object, not to an instance: it names
//! the instance whose `test` or `exec` ran last. The part that registers the
//! class owns it, so it is not part of an instance's surface here.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use crate::vm::regexp_engine;
use crate::vm::runtime::array::{ArrayError, ArrayObject};
use crate::vm::runtime::natives::string::Arity;
use crate::vm::runtime::object::{CustomObject, Dispatch, MemberFlags, ObjectError};
use crate::vm::runtime::string::TjsString;
use crate::vm::runtime::variant::{Closure, Variant, VariantError};

/// The name the class is registered under.
pub const CLASS_NAME: &str = "RegExp";

/// The bit the `g` flag sets, outside the engine's option range.
const GLOBAL_SEARCH: u32 = 1 << 31;

/// The flags a pattern with no flag letter gets.
const DEFAULT_FLAGS: u32 =
    regexp_engine::OPTION_CAPTURE_GROUP | regexp_engine::OPTION_FIND_NOT_EMPTY;

/// A member of the class.
///
/// [`RegExpMember::ALL`] lists the members in the order the class registers
/// them. `last` is the class's static property; every other member belongs to
/// an instance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegExpMember {
    /// The empty finalizer every instance carries.
    Finalize,
    /// Compiles a pattern and its flags.
    Compile,
    /// Compiles the `//flags/expression` form.
    CompileLiteral,
    /// Searches and answers whether the pattern matched.
    Test,
    /// Searches and answers the match array without changing the state.
    Match,
    /// Searches and answers the match array, changing the state.
    Exec,
    /// Replaces the matches in a text.
    Replace,
    /// Splits a text around the matches.
    Split,
    /// The match array the last `test` or `exec` produced.
    Matches,
    /// The position a search starts at.
    Start,
    /// The byte position the last match started at.
    Index,
    /// The position the last match ended at.
    LastIndex,
    /// The text the last search was made in.
    Input,
    /// The text the last match matched.
    LastMatch,
    /// The text the last captured group matched.
    LastParen,
    /// The text before the last match.
    LeftContext,
    /// The text after the last match.
    RightContext,
    /// The instance whose `test` or `exec` ran last.
    Last,
}

impl RegExpMember {
    /// Every member, in the order the class registers them.
    pub const ALL: &'static [RegExpMember] = &[
        Self::Finalize,
        Self::Compile,
        Self::CompileLiteral,
        Self::Test,
        Self::Match,
        Self::Exec,
        Self::Replace,
        Self::Split,
        Self::Matches,
        Self::Start,
        Self::Index,
        Self::LastIndex,
        Self::Input,
        Self::LastMatch,
        Self::LastParen,
        Self::LeftContext,
        Self::RightContext,
        Self::Last,
    ];

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
            Self::Compile => "compile",
            Self::CompileLiteral => "_compile",
            Self::Test => "test",
            Self::Match => "match",
            Self::Exec => "exec",
            Self::Replace => "replace",
            Self::Split => "split",
            Self::Matches => "matches",
            Self::Start => "start",
            Self::Index => "index",
            Self::LastIndex => "lastIndex",
            Self::Input => "input",
            Self::LastMatch => "lastMatch",
            Self::LastParen => "lastParen",
            Self::LeftContext => "leftContext",
            Self::RightContext => "rightContext",
            Self::Last => "last",
        }
    }

    /// How many parameters a call accepts, or `None` for a member that is read
    /// rather than called.
    pub fn arity(self) -> Option<Arity> {
        match self {
            Self::Finalize => Some(Arity::AtLeast(0)),
            Self::Compile | Self::Test | Self::Match | Self::Exec | Self::Split => {
                Some(Arity::AtLeast(1))
            }
            Self::CompileLiteral => Some(Arity::Exactly(1)),
            Self::Replace => Some(Arity::AtLeast(2)),
            Self::Matches
            | Self::Start
            | Self::Index
            | Self::LastIndex
            | Self::Input
            | Self::LastMatch
            | Self::LastParen
            | Self::LeftContext
            | Self::RightContext
            | Self::Last => None,
        }
    }

    /// Whether the member belongs to the class object rather than an instance.
    pub fn is_static(self) -> bool {
        matches!(self, Self::Last)
    }

    /// Whether a write to the member is a write to its value.
    pub fn is_writable(self) -> bool {
        matches!(self, Self::Start)
    }

    /// Calls the member on `state`.
    ///
    /// `receiver` is the object the call was made on, which the replacement
    /// call hands a function replacement. A member that is not called, or a
    /// call whose parameter count is not one the member accepts, is a typed
    /// failure.
    ///
    /// # Errors
    ///
    /// [`RegExpError::ParameterCount`] for a call with the wrong count,
    /// [`RegExpError::NotCallable`] for a member that is read, the compile's
    /// own message for a pattern the engine refuses, and the array's or a
    /// conversion's failure when the answer cannot be built.
    pub fn call(self, state: &mut RegExp, parameters: &[Variant]) -> Result<Variant, RegExpError> {
        self.call_with_receiver(state, parameters, None)
    }

    /// Calls the member on `state` with the receiver a function replacement
    /// inherits.
    ///
    /// # Errors
    ///
    /// The failures [`RegExpMember::call`] names.
    pub fn call_with_receiver(
        self,
        state: &mut RegExp,
        parameters: &[Variant],
        receiver: Option<&dyn Dispatch>,
    ) -> Result<Variant, RegExpError> {
        let Some(arity) = self.arity() else {
            return Err(RegExpError::NotCallable { member: self });
        };
        if !arity.accepts(parameters.len()) {
            return Err(RegExpError::ParameterCount { arity });
        }
        match self {
            Self::Finalize => Ok(Variant::Void),
            Self::Compile => state.compile(parameters).map(|()| Variant::Void),
            Self::CompileLiteral => state.compile_literal(parameters).map(|()| Variant::Void),
            Self::Test => state
                .test(parameters)
                .map(|matched| Variant::Integer(i64::from(matched))),
            Self::Match => state.match_text(parameters),
            Self::Exec => state.exec(parameters),
            Self::Replace => state.replace(parameters, receiver),
            Self::Split => state.split(parameters),
            Self::Matches
            | Self::Start
            | Self::Index
            | Self::LastIndex
            | Self::Input
            | Self::LastMatch
            | Self::LastParen
            | Self::LeftContext
            | Self::RightContext
            | Self::Last => Err(RegExpError::NotCallable { member: self }),
        }
    }

    /// Reads the member from `state`.
    ///
    /// # Errors
    ///
    /// [`RegExpError::NotReadable`] for a member that is called, and
    /// [`RegExpError::Static`] for the class's own property.
    pub fn get(self, state: &RegExp) -> Result<Variant, RegExpError> {
        match self {
            Self::Matches => Ok(state.matches.clone()),
            Self::Start => Ok(Variant::Integer(i64::from(state.start))),
            Self::Index => Ok(Variant::Integer(i64::from(state.index))),
            Self::LastIndex => Ok(Variant::Integer(i64::from(state.last_index))),
            Self::Input => Ok(Variant::String(state.input.clone())),
            Self::LastMatch => Ok(Variant::String(state.last_match.clone())),
            Self::LastParen => Ok(Variant::String(state.last_paren.clone())),
            Self::LeftContext => Ok(Variant::String(state.left_context.clone())),
            Self::RightContext => Ok(Variant::String(state.right_context.clone())),
            Self::Last => Err(RegExpError::Static { member: self }),
            Self::Finalize
            | Self::Compile
            | Self::CompileLiteral
            | Self::Test
            | Self::Match
            | Self::Exec
            | Self::Replace
            | Self::Split => Err(RegExpError::NotReadable { member: self }),
        }
    }

    /// Writes the member on `state`.
    ///
    /// # Errors
    ///
    /// [`RegExpError::ReadOnly`] for every member but `start`, and the
    /// conversion's failure when `start` is given a value with no integer.
    pub fn set(self, state: &mut RegExp, value: &Variant) -> Result<(), RegExpError> {
        match self {
            Self::Start => state.set_start(value),
            Self::Last => Err(RegExpError::Static { member: self }),
            _ => Err(RegExpError::ReadOnly { member: self }),
        }
    }
}

/// The state of one `RegExp` instance.
pub struct RegExp {
    /// The compiled expression, absent until a pattern is compiled.
    expression: Option<regexp_engine::Expression>,
    /// The option bits the flags produced.
    flags: u32,
    /// The code unit a search starts at.
    start: u32,
    /// The array the last `test` or `exec` produced.
    matches: Variant,
    /// The byte position the last match started at.
    index: u32,
    /// The text the last search was made in.
    input: TjsString,
    /// The code unit the last match ended at.
    last_index: u32,
    /// The text the last match matched.
    last_match: TjsString,
    /// The text the last captured group matched.
    last_paren: TjsString,
    /// The text before the last match.
    left_context: TjsString,
    /// The text after the last match.
    right_context: TjsString,
}

impl Default for RegExp {
    fn default() -> Self {
        Self {
            expression: None,
            flags: DEFAULT_FLAGS,
            start: 0,
            matches: Variant::Void,
            index: 0,
            input: TjsString::new(),
            last_index: 0,
            last_match: TjsString::new(),
            last_paren: TjsString::new(),
            left_context: TjsString::new(),
            right_context: TjsString::new(),
        }
    }
}

impl RegExp {
    /// The instance a construction makes.
    ///
    /// With no argument the instance holds no expression; a pattern and an
    /// optional flags text compile one.
    ///
    /// # Errors
    ///
    /// The compile's failure for a pattern the engine refuses, and the
    /// conversion's failure for an argument with no text.
    pub fn new(arguments: &[Variant]) -> Result<Self, RegExpError> {
        let mut regexp = Self::default();
        if !arguments.is_empty() {
            regexp.compile(arguments)?;
        }
        Ok(regexp)
    }

    /// The code unit a search starts at.
    pub fn start(&self) -> u32 {
        self.start
    }

    /// The byte position the last match started at.
    pub fn index(&self) -> u32 {
        self.index
    }

    /// The code unit the last match ended at.
    pub fn last_index(&self) -> u32 {
        self.last_index
    }

    /// The text the last search was made in.
    pub fn input(&self) -> &TjsString {
        &self.input
    }

    /// The array the last `test` or `exec` produced.
    pub fn matches(&self) -> &Variant {
        &self.matches
    }

    /// The text the last match matched.
    pub fn last_match(&self) -> &TjsString {
        &self.last_match
    }

    /// The text the last captured group matched.
    pub fn last_paren(&self) -> &TjsString {
        &self.last_paren
    }

    /// The text before the last match.
    pub fn left_context(&self) -> &TjsString {
        &self.left_context
    }

    /// The text after the last match.
    pub fn right_context(&self) -> &TjsString {
        &self.right_context
    }

    /// The option bits the flags produced.
    pub fn flags(&self) -> u32 {
        self.flags
    }

    /// Sets the code unit a search starts at.
    ///
    /// The value is read as an integer and kept as a 32-bit unsigned value,
    /// which is the width the search position has.
    ///
    /// # Errors
    ///
    /// [`RegExpError::Convert`] when the value has no integer.
    pub fn set_start(&mut self, value: &Variant) -> Result<(), RegExpError> {
        let integer = value.to_integer().map_err(|error| RegExpError::Convert {
            parameter: 0,
            error,
        })?;
        self.start = integer as u32;
        Ok(())
    }

    /// Compiles a pattern and its flags.
    ///
    /// The first parameter is the pattern, and the second, when it is given, is
    /// the flags text. The empty pattern compiles as the empty expression.
    ///
    /// # Errors
    ///
    /// [`RegExpError::ParameterCount`] for no parameter, the conversion's
    /// failure for a parameter with no text, and the engine's own message for a
    /// pattern it refuses.
    pub fn compile(&mut self, parameters: &[Variant]) -> Result<(), RegExpError> {
        let Some(pattern) = parameters.first() else {
            return Err(RegExpError::ParameterCount {
                arity: Arity::AtLeast(1),
            });
        };
        let pattern = text_of(pattern, 0)?;
        let flags = match parameters.get(1) {
            Some(flags) => parse_flags(&text_of(flags, 1)?),
            None => DEFAULT_FLAGS,
        };
        let pattern = if pattern.is_empty() {
            TjsString::from("(?:)")
        } else {
            pattern
        };
        self.replace_expression(&pattern, flags)
    }

    /// Compiles the `//flags/expression` form.
    ///
    /// # Errors
    ///
    /// [`RegExpError::ParameterCount`] unless the call gives one parameter,
    /// [`RegExpError::InvalidFormat`] for a text that is not the form, and the
    /// engine's own message for a pattern it refuses.
    pub fn compile_literal(&mut self, parameters: &[Variant]) -> Result<(), RegExpError> {
        if parameters.len() != 1 {
            return Err(RegExpError::ParameterCount {
                arity: Arity::Exactly(1),
            });
        }
        let expression = text_of(&parameters[0], 0)?;
        let units = expression.as_utf16();
        if units.len() < 2 || units[0] != u16::from(b'/') || units[1] != u16::from(b'/') {
            return Err(RegExpError::InvalidFormat);
        }
        let rest = &units[2..];
        let Some(slash) = rest.iter().position(|&unit| unit == u16::from(b'/')) else {
            return Err(RegExpError::InvalidFormat);
        };
        let flags = parse_flags(&TjsString::from(&rest[..slash]));
        let pattern = TjsString::from(&rest[slash + 1..]);
        self.replace_expression(&pattern, flags)
    }

    /// Searches and answers whether the pattern matched, changing the state.
    ///
    /// # Errors
    ///
    /// [`RegExpError::ParameterCount`] for no target, the conversion's failure
    /// for a target with no text, and [`RegExpError::NotCompiled`] when the
    /// instance holds no expression.
    pub fn test(&mut self, parameters: &[Variant]) -> Result<bool, RegExpError> {
        let (matched, _array) = self.exec_inner(parameters)?;
        Ok(matched)
    }

    /// Searches and answers the match array without changing the state.
    ///
    /// # Errors
    ///
    /// [`RegExpError::ParameterCount`] for no target, the conversion's failure
    /// for a target with no text, and [`RegExpError::NotCompiled`] when the
    /// instance holds no expression.
    pub fn match_text(&mut self, parameters: &[Variant]) -> Result<Variant, RegExpError> {
        let Some(target) = parameters.first() else {
            return Err(RegExpError::ParameterCount {
                arity: Arity::AtLeast(1),
            });
        };
        let target = text_of(target, 0)?;
        let expression = self.expression.as_ref().ok_or(RegExpError::NotCompiled)?;
        let region = self.search(expression, &target, self.start)?;
        result_array(&target, self.start, region.as_ref())
    }

    /// Searches and answers the match array, changing the state.
    ///
    /// # Errors
    ///
    /// [`RegExpError::ParameterCount`] for no target, the conversion's failure
    /// for a target with no text, and [`RegExpError::NotCompiled`] when the
    /// instance holds no expression.
    pub fn exec(&mut self, parameters: &[Variant]) -> Result<Variant, RegExpError> {
        let (_matched, array) = self.exec_inner(parameters)?;
        Ok(array)
    }

    /// Searches and answers whether it matched, with the state the search left.
    fn exec_inner(&mut self, parameters: &[Variant]) -> Result<(bool, Variant), RegExpError> {
        let Some(target) = parameters.first() else {
            return Err(RegExpError::ParameterCount {
                arity: Arity::AtLeast(1),
            });
        };
        let target = text_of(target, 0)?;
        let expression = self.expression.as_ref().ok_or(RegExpError::NotCompiled)?;
        let region = self.search(expression, &target, self.start)?;
        let array = result_array(&target, self.start, region.as_ref())?;
        let matched = region.is_some();
        self.matches = array.clone();
        self.input = target.clone();
        if let Some(region) = region.as_ref().filter(|region| region.len() > 0) {
            let begin = region.beg(0);
            let end = region.end(0);
            let begin_units = unit_offset(begin)?;
            let end_units = unit_offset(end)?;
            let last = region.len() - 1;
            let last_begin = unit_offset(region.beg(last))?;
            let last_end = unit_offset(region.end(last))?;
            let mut lastindex = 0i32;
            for slot in 0..region.len() {
                lastindex = lastindex.max(region.end(slot));
            }
            let last_units = u32::try_from(lastindex.max(0) / 2).map_err(|_| RegExpError::Range)?;
            let last_index = self.start.wrapping_add(last_units);
            let begin = u32::try_from(begin).map_err(|_| RegExpError::Range)?;
            self.index = self.start.wrapping_add(begin);
            self.last_index = last_index;
            self.last_match = slice_units(&target, 0, begin_units, end_units)?;
            self.last_paren = slice_units(&target, 0, last_begin, last_end)?;
            let begin_units = u32::try_from(begin_units).map_err(|_| RegExpError::Range)?;
            let left_end = self.start.wrapping_add(begin_units);
            self.left_context = prefix_units(&target, left_end)?;
            self.right_context = suffix_units(&target, last_index)?;
            if self.flags & GLOBAL_SEARCH != 0 {
                self.start = last_index;
            }
        } else {
            self.index = self.start;
            self.last_index = self.start;
            self.last_match = TjsString::new();
            self.last_paren = TjsString::new();
            self.left_context = prefix_units(&target, self.start)?;
        }
        Ok((matched, array))
    }

    /// Replaces the matches in a text.
    ///
    /// A replacement that is not an object is its text, used as it is. An
    /// object replacement is called with the match array, and the text of its
    /// answer is used. A call that fails ends the replacement with what has
    /// been built so far, which is what the native method does.
    ///
    /// # Errors
    ///
    /// [`RegExpError::ParameterCount`] for fewer than two parameters, the
    /// conversion's failures for the text parameters, and
    /// [`RegExpError::NotCompiled`] when the instance holds no expression.
    pub fn replace(
        &mut self,
        parameters: &[Variant],
        receiver: Option<&dyn Dispatch>,
    ) -> Result<Variant, RegExpError> {
        if parameters.len() < 2 {
            return Err(RegExpError::ParameterCount {
                arity: Arity::AtLeast(2),
            });
        }
        let target = text_of(&parameters[0], 0)?;
        let replacement = &parameters[1];
        let closure = match replacement {
            Variant::Object(closure) => Some(closure.clone()),
            _ => None,
        };
        let literal = match &closure {
            Some(_) => None,
            None => Some(text_of(replacement, 1)?),
        };
        let expression = self.expression.as_ref().ok_or(RegExpError::NotCompiled)?;
        let units = target.as_utf16();
        let end = units.len();
        let global = self.flags & GLOBAL_SEARCH != 0;
        let mut cursor = 0usize;
        let mut output: Vec<u16> = Vec::new();
        let Some(mut region) = expression
            .search(units, 0, end)
            .map_err(|_| RegExpError::Range)?
        else {
            output.extend_from_slice(units);
            return Ok(Variant::String(TjsString::from(output)));
        };
        loop {
            let position = unit_offset(region.beg(0))?;
            let matched_end = unit_offset(region.end(0))?;
            if position > 0 {
                let from = cursor.checked_add(position).ok_or(RegExpError::Range)?;
                let text = units.get(cursor..from).ok_or(RegExpError::Range)?;
                output.extend_from_slice(text);
            }
            match &literal {
                Some(text) => output.extend_from_slice(text.as_utf16()),
                None => {
                    let Some(closure) = &closure else {
                        return Ok(Variant::String(TjsString::from(output)));
                    };
                    let array = result_array_units(units, cursor, Some(&region))?;
                    let Some(object) = &closure.object else {
                        return Ok(Variant::String(TjsString::from(output)));
                    };
                    let arguments = [array];
                    let call_receiver = closure.objthis.as_deref().or(receiver);
                    let answer = match object.func_call(
                        &TjsString::new(),
                        Some(&arguments),
                        call_receiver,
                    ) {
                        Ok(answer) => answer,
                        Err(_refused) => {
                            return Ok(Variant::String(TjsString::from(output)));
                        }
                    };
                    let text = answer.to_string().map_err(|error| RegExpError::Convert {
                        parameter: 1,
                        error,
                    })?;
                    output.extend_from_slice(text.as_utf16());
                }
            }
            cursor = cursor.checked_add(matched_end).ok_or(RegExpError::Range)?;
            if !global || cursor >= end {
                break;
            }
            let Some(next) = expression
                .search(units, cursor, end)
                .map_err(|_| RegExpError::Range)?
            else {
                break;
            };
            region = next;
        }
        let rest = units.get(cursor..).ok_or(RegExpError::Range)?;
        output.extend_from_slice(rest);
        Ok(Variant::String(TjsString::from(output)))
    }

    /// Splits a text around the matches.
    ///
    /// The third parameter is the purge flag: when it is true, an empty field
    /// is left out.
    ///
    /// # Errors
    ///
    /// [`RegExpError::ParameterCount`] for no target, the conversion's failures
    /// for the text parameters, and [`RegExpError::NotCompiled`] when the
    /// instance holds no expression.
    pub fn split(&mut self, parameters: &[Variant]) -> Result<Variant, RegExpError> {
        let Some(target) = parameters.first() else {
            return Err(RegExpError::ParameterCount {
                arity: Arity::AtLeast(1),
            });
        };
        let target = text_of(target, 0)?;
        let purge = match parameters.get(2) {
            Some(value) => truth(value)?,
            None => false,
        };
        let expression = self.expression.as_ref().ok_or(RegExpError::NotCompiled)?;
        let units = target.as_utf16();
        let end = units.len();
        let array = Rc::new(ArrayObject::new());
        let Some(mut region) = expression
            .search(units, 0, end)
            .map_err(|_| RegExpError::Range)?
        else {
            array.push(&[Variant::String(target)])?;
            return Ok(object_value(array));
        };
        let mut cursor = 0usize;
        loop {
            let length = unit_offset(region.beg(0))?;
            if !purge || length > 0 {
                let to = cursor.checked_add(length).ok_or(RegExpError::Range)?;
                let field = units.get(cursor..to).ok_or(RegExpError::Range)?;
                array.push(&[Variant::String(TjsString::from(field))])?;
            }
            let advance = unit_offset(region.end(0))?;
            cursor = cursor.checked_add(advance).ok_or(RegExpError::Range)?;
            let Some(next) = expression
                .search(units, cursor, end)
                .map_err(|_| RegExpError::Range)?
            else {
                break;
            };
            region = next;
        }
        if !purge || cursor < end {
            let rest = units.get(cursor..).ok_or(RegExpError::Range)?;
            array.push(&[Variant::String(TjsString::from(rest))])?;
        }
        Ok(object_value(array))
    }

    /// Replaces the expression, leaving the old one freed first.
    fn replace_expression(&mut self, pattern: &TjsString, flags: u32) -> Result<(), RegExpError> {
        self.expression = None;
        let expression = regexp_engine::Expression::compile(
            pattern.as_utf16(),
            flags & regexp_engine::OPTION_MASK,
        )
        .map_err(|message| RegExpError::Compile {
            message: TjsString::from(message),
        })?;
        self.expression = Some(expression);
        self.flags = flags;
        Ok(())
    }

    /// Searches `target` from `start`.
    fn search(
        &self,
        expression: &regexp_engine::Expression,
        target: &TjsString,
        start: u32,
    ) -> Result<Option<regexp_engine::Region>, RegExpError> {
        let length = u32::try_from(target.len()).map_err(|_| RegExpError::Range)?;
        if start >= length {
            return Ok(None);
        }
        let units = target.as_utf16();
        let start = usize::try_from(start).map_err(|_| RegExpError::Range)?;
        let end = usize::try_from(length).map_err(|_| RegExpError::Range)?;
        expression
            .search(units, start, end)
            .map_err(|_| RegExpError::Range)
    }
}

/// A `RegExp` instance as an object.
///
/// The state is shared with the method objects the instance answers, so a
/// method read out of the object still reaches this instance's state.
pub struct RegExpObject {
    /// The instance's state.
    state: Rc<RefCell<RegExp>>,
    /// The object itself, for a method that has to hand the call a receiver.
    this: Weak<Self>,
    /// The instance's own members, which a write may shadow a class member
    /// with, and its class name.
    members: CustomObject,
}

impl RegExpObject {
    /// The object a construction makes.
    ///
    /// # Errors
    ///
    /// The construction's compile or conversion failure.
    pub fn new(arguments: &[Variant]) -> Result<Rc<Self>, RegExpError> {
        let state = Rc::new(RefCell::new(RegExp::new(arguments)?));
        Ok(Rc::new_cyclic(|this| Self {
            state,
            this: this.clone(),
            members: CustomObject::new([TjsString::from(CLASS_NAME)]),
        }))
    }

    /// The object an existing state becomes.
    pub fn from_state(state: RegExp) -> Rc<Self> {
        Rc::new_cyclic(|this| Self {
            state: Rc::new(RefCell::new(state)),
            this: this.clone(),
            members: CustomObject::new([TjsString::from(CLASS_NAME)]),
        })
    }

    /// The state, borrowed.
    pub fn state(&self) -> std::cell::Ref<'_, RegExp> {
        self.state.borrow()
    }

    /// The state, borrowed for a write.
    pub fn state_mut(&self) -> std::cell::RefMut<'_, RegExp> {
        self.state.borrow_mut()
    }
}

impl Dispatch for RegExpObject {
    fn prop_get(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        match self.members.prop_get(name, flags, objthis) {
            Ok(value) => return Ok(value),
            Err(ObjectError::MemberNotFound) => {}
            Err(error) => return Err(error),
        }
        let Some(member) = RegExpMember::lookup(name) else {
            return Err(ObjectError::MemberNotFound);
        };
        if member.is_static() {
            return Err(ObjectError::MemberNotFound);
        }
        if member.arity().is_some() {
            let method: Rc<dyn Dispatch> = Rc::new(RegExpMethod {
                state: self.state.clone(),
                this: self.this.clone(),
                member,
            });
            let objthis = self.this.upgrade().map(|object| object as Rc<dyn Dispatch>);
            return Ok(Variant::Object(Closure {
                object: Some(method),
                objthis,
            }));
        }
        let state = self.state.borrow();
        member.get(&state).map_err(|_| ObjectError::CallNotServed)
    }

    fn prop_set(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        value: &Variant,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        if let Some(member) = RegExpMember::lookup(name) {
            if member.is_static() {
                return Err(ObjectError::MemberNotFound);
            }
            if member.is_writable() {
                let mut state = self.state.borrow_mut();
                return member
                    .set(&mut state, value)
                    .map_err(|_| ObjectError::CallNotServed);
            }
            if member.arity().is_none() {
                return Err(ObjectError::CallNotServed);
            }
        }
        self.members.prop_set(name, flags, value, objthis)
    }

    fn delete_member(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        self.members.delete_member(name, flags, objthis)
    }

    fn enum_members(
        &self,
        flags: MemberFlags,
        callback: &mut dyn FnMut(&TjsString, u32, &Variant) -> bool,
    ) -> Result<(), ObjectError> {
        let state = self.state.borrow();
        let without_value = flags.contains(MemberFlags::ENUM_NO_VALUE);
        for member in RegExpMember::ALL.iter().copied() {
            if member.is_static() {
                continue;
            }
            let value = if without_value || member.arity().is_some() {
                Variant::Void
            } else {
                member.get(&state).map_err(|_| ObjectError::CallNotServed)?
            };
            if !callback(&TjsString::from(member.name()), 0, &value) {
                break;
            }
        }
        Ok(())
    }

    fn is_instance_of(
        &self,
        class: &TjsString,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<bool, ObjectError> {
        self.members.is_instance_of(class, objthis)
    }

    fn class_names(&self) -> Vec<TjsString> {
        self.members.class_names()
    }

    fn add_class_name(&self, name: &TjsString) -> Result<(), ObjectError> {
        self.members.add_class_name(name)
    }

    fn is_valid(&self) -> bool {
        self.members.is_valid()
    }

    fn invalidate(&self) -> bool {
        self.members.invalidate()
    }

    fn func_call(
        &self,
        name: &TjsString,
        argv: Option<&[Variant]>,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        let Some(member) = RegExpMember::lookup(name) else {
            return Err(ObjectError::CallNotServed);
        };
        if member.arity().is_none() {
            return Err(ObjectError::CallNotServed);
        }
        let arguments = argv.unwrap_or(&[]);
        let this = self.this.upgrade();
        let receiver = match objthis {
            Some(receiver) => Some(receiver),
            None => this.as_deref().map(|object| object as &dyn Dispatch),
        };
        let mut state = self.state.borrow_mut();
        member
            .call_with_receiver(&mut state, arguments, receiver)
            .map_err(|_| ObjectError::CallNotServed)
    }
}

/// A method read out of a `RegExpObject`.
struct RegExpMethod {
    /// The instance's state.
    state: Rc<RefCell<RegExp>>,
    /// The object the method was read from, for the call's receiver.
    this: Weak<RegExpObject>,
    /// The member the method calls.
    member: RegExpMember,
}

impl Dispatch for RegExpMethod {
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
        argv: Option<&[Variant]>,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        if !name.is_empty() {
            return Err(ObjectError::CallNotServed);
        }
        let arguments = argv.unwrap_or(&[]);
        let this = self.this.upgrade();
        let receiver = match objthis {
            Some(receiver) => Some(receiver),
            None => this.as_deref().map(|object| object as &dyn Dispatch),
        };
        let mut state = self.state.borrow_mut();
        self.member
            .call_with_receiver(&mut state, arguments, receiver)
            .map_err(|_| ObjectError::CallNotServed)
    }
}

/// The failure of a member's call or read.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum RegExpError {
    /// The call gives a number of parameters the member does not accept.
    #[error("the member does not accept this many parameters")]
    ParameterCount {
        /// The counts the member accepts.
        arity: Arity,
    },
    /// A parameter has no conversion the rule needs.
    #[error("the parameter cannot be converted")]
    Convert {
        /// The position of the parameter the rule read.
        parameter: usize,
        /// The conversion that failed.
        error: VariantError,
    },
    /// The engine refused the pattern, with its own message.
    #[error("the pattern does not compile: {message}")]
    Compile {
        /// The message the engine gave the refusal.
        message: TjsString,
    },
    /// A literal expression is not `//flags/expression`.
    #[error("the expression is not //flags/expression")]
    InvalidFormat,
    /// The instance holds no expression.
    #[error("the instance holds no expression")]
    NotCompiled,
    /// The member is called, not read.
    #[error("the member is called")]
    NotReadable {
        /// The member.
        member: RegExpMember,
    },
    /// The member is read, not called.
    #[error("the member is read")]
    NotCallable {
        /// The member.
        member: RegExpMember,
    },
    /// The member belongs to the class object.
    #[error("the member belongs to the class object")]
    Static {
        /// The member.
        member: RegExpMember,
    },
    /// The member is read-only.
    #[error("the member is read-only")]
    ReadOnly {
        /// The member.
        member: RegExpMember,
    },
    /// A position or a text does not fit.
    #[error("the match position is out of range")]
    Range,
    /// The answer array could not hold the values.
    #[error("the answer cannot hold the values")]
    Array(#[from] ArrayError),
}

/// The text the parameter at `index` is read as.
fn text_of(value: &Variant, parameter: usize) -> Result<TjsString, RegExpError> {
    value
        .to_string()
        .map_err(|error| RegExpError::Convert { parameter, error })
}

/// The flags a flags text names.
fn parse_flags(text: &TjsString) -> u32 {
    let mut flags = DEFAULT_FLAGS;
    for &unit in text.as_utf16() {
        if unit == 0 || unit == u16::from(b'/') {
            break;
        }
        match unit {
            unit if unit == u16::from(b'g') => flags |= GLOBAL_SEARCH,
            unit if unit == u16::from(b'i') => flags |= regexp_engine::OPTION_IGNORECASE,
            _ => {}
        }
    }
    flags
}

/// The code-unit offset a byte offset names.
fn unit_offset(bytes: i32) -> Result<usize, RegExpError> {
    if bytes < 0 {
        return Ok(0);
    }
    let bytes = usize::try_from(bytes).map_err(|_| RegExpError::Range)?;
    if bytes % 2 != 0 {
        return Err(RegExpError::Range);
    }
    Ok(bytes / 2)
}

/// The first `length` code units of `text`.
fn prefix_units(text: &TjsString, length: u32) -> Result<TjsString, RegExpError> {
    let length = usize::try_from(length).map_err(|_| RegExpError::Range)?;
    let units = text.as_utf16().get(..length).ok_or(RegExpError::Range)?;
    Ok(TjsString::from(units))
}

/// The code units of `text` from `start`.
fn suffix_units(text: &TjsString, start: u32) -> Result<TjsString, RegExpError> {
    let start = usize::try_from(start).map_err(|_| RegExpError::Range)?;
    let units = text.as_utf16().get(start..).ok_or(RegExpError::Range)?;
    Ok(TjsString::from(units))
}

/// The code units a region slot names, relative to `base`.
fn slice_units(
    text: &TjsString,
    base: u32,
    begin: usize,
    end: usize,
) -> Result<TjsString, RegExpError> {
    let base = usize::try_from(base).map_err(|_| RegExpError::Range)?;
    let start = base.checked_add(begin).ok_or(RegExpError::Range)?;
    let end = base.checked_add(end).ok_or(RegExpError::Range)?;
    let units = text.as_utf16().get(start..end).ok_or(RegExpError::Range)?;
    Ok(TjsString::from(units))
}

/// The array a search answers.
fn result_array(
    target: &TjsString,
    start: u32,
    region: Option<&regexp_engine::Region>,
) -> Result<Variant, RegExpError> {
    let start = usize::try_from(start).map_err(|_| RegExpError::Range)?;
    result_array_units(target.as_utf16(), start, region)
}

/// The array a search answers, over the target's code units.
fn result_array_units(
    target: &[u16],
    start: usize,
    region: Option<&regexp_engine::Region>,
) -> Result<Variant, RegExpError> {
    let array = Rc::new(ArrayObject::new());
    if let Some(region) = region {
        if region.len() == 0 {
            array.push(&[Variant::String(TjsString::new())])?;
        } else {
            for index in 0..region.len() {
                let begin = unit_offset(region.beg(index))?;
                let end = unit_offset(region.end(index))?;
                let from = start.checked_add(begin).ok_or(RegExpError::Range)?;
                let to = start.checked_add(end).ok_or(RegExpError::Range)?;
                let text = target.get(from..to).ok_or(RegExpError::Range)?;
                array.push(&[Variant::String(TjsString::from(text))])?;
            }
        }
    }
    Ok(object_value(array))
}

/// The object value an object handle stands for.
fn object_value(object: Rc<ArrayObject>) -> Variant {
    let object: Rc<dyn Dispatch> = object;
    Variant::Object(Closure {
        object: Some(object.clone()),
        objthis: Some(object),
    })
}

/// The truth of a value, as the purge flag reads it.
fn truth(value: &Variant) -> Result<bool, RegExpError> {
    match value {
        Variant::Void => Ok(false),
        Variant::Integer(value) => Ok(*value != 0),
        Variant::Real(value) => Ok(*value != 0.0),
        Variant::String(_) => Ok(value.to_integer().map_err(|error| RegExpError::Convert {
            parameter: 2,
            error,
        })? != 0),
        Variant::Octet(value) => Ok(!value.is_empty()),
        Variant::Object(value) => Ok(!value.is_null()),
    }
}
