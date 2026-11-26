//! The array object: a dense sequence of values with the class's members.
//!
//! An array's elements are its own storage, not member-map entries. An index
//! reads, writes and deletes a position in a sequence: a write past the end
//! grows the sequence with empty values, an element removed by index takes the
//! tail with it, and the count is the number of positions, holes included. A
//! hole is an element holding the empty value; a position past the end reads as
//! the empty value too, but it is not present. The two stay apart because a
//! read that requires the member to exist answers the hole and refuses the
//! position past the end.
//!
//! A member name that is not an index is an ordinary named member and lives in
//! the object model's [`CustomObject`], whose member map owns the named-member
//! rules: the property seam, the hidden and static flags, and the enumeration
//! order. The array adds the index space to that surface and the operations
//! the class publishes.
//!
//! # The index name
//!
//! A name names an index when it is spaces, an optional sign, more spaces, at
//! least one digit, then digits and periods, then spaces, and nothing else; the
//! index is the integer the digits before the first period read as. A negative
//! index counts from the end. The conversion is checked: a name whose digits do
//! not fit a 64-bit integer is a typed error, never a wrapped index.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::rc::Rc;

use super::number::Number;
use super::object::Dispatch as ObjectHandle;
use super::object::{CustomObject, Dispatch, MemberFlags, ObjectError};
use super::string::TjsString;
use super::variant::{Closure, Variant, VariantError};

/// The members the class publishes.
///
/// The table is the one place the names live. A member answers the fewest
/// parameters its call accepts, and the two properties take none. The
/// operations themselves are [`ArrayObject`]'s methods.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArrayMember {
    /// Loads a text file, one element per line.
    Load,
    /// Loads a structured file.
    LoadStruct,
    /// Saves the elements as text, one per line.
    Save,
    /// Saves the elements as structured text or as a structure.
    SaveStruct,
    /// Splits a text around delimiters into elements.
    Split,
    /// Joins the elements into one text.
    Join,
    /// Sorts the elements.
    Sort,
    /// Reverses the order of the elements.
    Reverse,
    /// Copies another object's elements.
    Assign,
    /// Copies a structured object's members as name and value pairs.
    AssignStruct,
    /// Removes every element.
    Clear,
    /// Removes one element by position.
    Erase,
    /// Removes the elements equal to a value.
    Remove,
    /// Inserts a value at a position.
    Insert,
    /// Appends a value and answers its position.
    Add,
    /// Appends values and answers the new count.
    Push,
    /// Removes and answers the last element.
    Pop,
    /// Removes and answers the first element.
    Shift,
    /// Inserts values at the front and answers the new count.
    Unshift,
    /// Answers the position of the first element equal to a value.
    Find,
    /// Packs values into an octet.
    Pack,
    /// The number of elements.
    Count,
    /// The number of elements, spelled the other way.
    Length,
}

/// One row of the class's member table.
struct ClassMember {
    /// The member the name denotes.
    member: ArrayMember,
    /// The name the class publishes.
    name: &'static str,
    /// The fewest parameters a call accepts; `None` for a property.
    min_params: Option<usize>,
}

/// The class's member table, in the order the class registers the members.
const CLASS_MEMBERS: &[ClassMember] = &[
    ClassMember {
        member: ArrayMember::Load,
        name: "load",
        min_params: Some(1),
    },
    ClassMember {
        member: ArrayMember::LoadStruct,
        name: "loadStruct",
        min_params: Some(1),
    },
    ClassMember {
        member: ArrayMember::Save,
        name: "save",
        min_params: Some(1),
    },
    ClassMember {
        member: ArrayMember::SaveStruct,
        name: "saveStruct",
        min_params: Some(1),
    },
    ClassMember {
        member: ArrayMember::Split,
        name: "split",
        min_params: Some(2),
    },
    ClassMember {
        member: ArrayMember::Join,
        name: "join",
        min_params: Some(1),
    },
    ClassMember {
        member: ArrayMember::Sort,
        name: "sort",
        min_params: Some(0),
    },
    ClassMember {
        member: ArrayMember::Reverse,
        name: "reverse",
        min_params: Some(0),
    },
    ClassMember {
        member: ArrayMember::Assign,
        name: "assign",
        min_params: Some(1),
    },
    ClassMember {
        member: ArrayMember::AssignStruct,
        name: "assignStruct",
        min_params: Some(1),
    },
    ClassMember {
        member: ArrayMember::Clear,
        name: "clear",
        min_params: Some(0),
    },
    ClassMember {
        member: ArrayMember::Erase,
        name: "erase",
        min_params: Some(1),
    },
    ClassMember {
        member: ArrayMember::Remove,
        name: "remove",
        min_params: Some(1),
    },
    ClassMember {
        member: ArrayMember::Insert,
        name: "insert",
        min_params: Some(2),
    },
    ClassMember {
        member: ArrayMember::Add,
        name: "add",
        min_params: Some(1),
    },
    ClassMember {
        member: ArrayMember::Push,
        name: "push",
        min_params: Some(0),
    },
    ClassMember {
        member: ArrayMember::Pop,
        name: "pop",
        min_params: Some(0),
    },
    ClassMember {
        member: ArrayMember::Shift,
        name: "shift",
        min_params: Some(0),
    },
    ClassMember {
        member: ArrayMember::Unshift,
        name: "unshift",
        min_params: Some(0),
    },
    ClassMember {
        member: ArrayMember::Find,
        name: "find",
        min_params: Some(1),
    },
    ClassMember {
        member: ArrayMember::Pack,
        name: "pack",
        min_params: Some(1),
    },
    ClassMember {
        member: ArrayMember::Count,
        name: "count",
        min_params: None,
    },
    ClassMember {
        member: ArrayMember::Length,
        name: "length",
        min_params: None,
    },
];

impl ArrayMember {
    /// The member `name` denotes, or `None` when it names no class member.
    pub fn lookup(name: &TjsString) -> Option<Self> {
        let units = name.as_utf16();
        CLASS_MEMBERS
            .iter()
            .find(|spec| units.iter().copied().eq(spec.name.encode_utf16()))
            .map(|spec| spec.member)
    }

    /// The fewest parameters a call to the member accepts.
    ///
    /// `None` for a property, which takes none and is read and written rather
    /// than called.
    pub fn min_params(self) -> Option<usize> {
        CLASS_MEMBERS
            .iter()
            .find(|spec| spec.member == self)
            .and_then(|spec| spec.min_params)
    }
}

/// The order an array's elements are sorted in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SortOrder {
    /// Normal ascending: two texts compare by code unit, every other pair as
    /// numbers.
    Ascending,
    /// Normal descending.
    Descending,
    /// Numeric ascending: two texts are read as numbers first.
    NumericAscending,
    /// Numeric descending.
    NumericDescending,
    /// Text ascending: every value compares by its text.
    TextAscending,
    /// Text descending.
    TextDescending,
}

impl SortOrder {
    /// The order a sort letter names.
    ///
    /// `-`, `0`, `9`, `a` and `z` name the descending, numeric ascending,
    /// numeric descending, text ascending and text descending orders; every
    /// other letter, and no letter at all, is the normal ascending order.
    pub fn from_letter(letter: Option<char>) -> Self {
        match letter {
            Some('-') => Self::Descending,
            Some('0') => Self::NumericAscending,
            Some('9') => Self::NumericDescending,
            Some('a') => Self::TextAscending,
            Some('z') => Self::TextDescending,
            _ => Self::Ascending,
        }
    }
}

/// The failure of an array operation.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ArrayError {
    /// The array has been invalidated.
    #[error("the array is invalid")]
    InvalidObject,

    /// A position is outside the range an operation accepts.
    #[error("the index is out of range")]
    IndexOutOfRange,

    /// A requested length cannot be held.
    #[error("the length is out of range")]
    LengthOutOfRange,

    /// A value cannot be converted where the operation needs it.
    #[error("the value cannot be converted")]
    Convert(#[from] VariantError),
}

/// An array object: a dense sequence of values and the class's member surface.
///
/// The elements are behind a [`RefCell`] because the dispatch surface takes
/// `&self` and an index write changes the sequence. The named members are the
/// object model's [`CustomObject`], which owns their map, their properties and
/// their enumeration order.
pub struct ArrayObject {
    /// The elements, in index order.
    elements: RefCell<Vec<Variant>>,
    /// The named members and the object's identity.
    members: CustomObject,
}

impl ArrayObject {
    /// An array with no element.
    pub fn new() -> Self {
        Self {
            elements: RefCell::new(Vec::new()),
            members: CustomObject::new([TjsString::from("Array")]),
        }
    }

    /// The number of elements, holes included.
    pub fn len(&self) -> usize {
        self.elements.borrow().len()
    }

    /// Whether the array holds no element.
    pub fn is_empty(&self) -> bool {
        self.elements.borrow().is_empty()
    }

    /// Appends `value` and answers the position it was stored at.
    pub fn append(&self, value: Variant) -> Result<usize, ArrayError> {
        self.check()?;
        let mut elements = self.elements.borrow_mut();
        elements
            .try_reserve(1)
            .map_err(|_| ArrayError::LengthOutOfRange)?;
        elements.push(value);
        Ok(elements.len() - 1)
    }

    /// Appends `values` and answers the new count.
    pub fn push(&self, values: &[Variant]) -> Result<usize, ArrayError> {
        self.check()?;
        let index = i64::try_from(self.len()).map_err(|_| ArrayError::LengthOutOfRange)?;
        self.insert_many(index, values)?;
        Ok(self.len())
    }

    /// Appends `values` at the front and answers the new count.
    pub fn unshift(&self, values: &[Variant]) -> Result<usize, ArrayError> {
        self.check()?;
        self.insert_many(0, values)?;
        Ok(self.len())
    }

    /// Inserts `value` at `index`.
    ///
    /// A negative index counts from the end. An index below the start or past
    /// the end is [`ArrayError::IndexOutOfRange`]; an index equal to the count
    /// appends.
    pub fn insert(&self, index: i64, value: Variant) -> Result<(), ArrayError> {
        self.insert_many(index, std::slice::from_ref(&value))
    }

    /// Inserts `values` at `index`, the first value at the position.
    ///
    /// The position rules are [`ArrayObject::insert`]'s.
    pub fn insert_many(&self, index: i64, values: &[Variant]) -> Result<(), ArrayError> {
        self.check()?;
        let mut elements = self.elements.borrow_mut();
        let position = insert_position(index, elements.len())?;
        elements
            .try_reserve(values.len())
            .map_err(|_| ArrayError::LengthOutOfRange)?;
        elements.splice(position..position, values.iter().cloned());
        Ok(())
    }

    /// Removes and answers the last element, or the empty value when there is
    /// none.
    pub fn pop(&self) -> Result<Variant, ArrayError> {
        self.check()?;
        match self.elements.borrow_mut().pop() {
            Some(value) => Ok(value),
            None => Ok(Variant::Void),
        }
    }

    /// Removes and answers the first element, or the empty value when there is
    /// none.
    pub fn shift(&self) -> Result<Variant, ArrayError> {
        self.check()?;
        let mut elements = self.elements.borrow_mut();
        if elements.is_empty() {
            return Ok(Variant::Void);
        }
        Ok(elements.remove(0))
    }

    /// Removes the element at `index` and shifts the tail down.
    ///
    /// A negative index counts from the end. An index outside the array is
    /// [`ArrayError::IndexOutOfRange`].
    pub fn erase(&self, index: i64) -> Result<(), ArrayError> {
        self.check()?;
        let mut elements = self.elements.borrow_mut();
        let position = erase_position(index, elements.len())?;
        elements.remove(position);
        Ok(())
    }

    /// Removes the elements equal to `value` and answers how many were
    /// removed.
    ///
    /// Equality is the same-kind comparison: two numbers of different kinds
    /// are not equal, and an object is equal only to the same object with the
    /// same receiver. With `all` false only the first equal element is
    /// removed.
    pub fn remove(&self, value: &Variant, all: bool) -> Result<usize, ArrayError> {
        self.check()?;
        let mut elements = self.elements.borrow_mut();
        let mut removed = 0usize;
        elements.retain(|element| {
            if (all || removed == 0) && discern_compare(value, element) {
                removed += 1;
                false
            } else {
                true
            }
        });
        Ok(removed)
    }

    /// Answers the position of the first element equal to `value`, or `-1`.
    ///
    /// The comparison is [`ArrayObject::remove`]'s. `start` is the first
    /// position searched; a negative one counts from the end and is clamped to
    /// the start.
    pub fn find(&self, value: &Variant, start: i64) -> Result<i64, ArrayError> {
        self.check()?;
        let elements = self.elements.borrow();
        let len = elements.len();
        let length = i64::try_from(len).map_err(|_| ArrayError::LengthOutOfRange)?;
        let mut at = if start < 0 {
            start
                .checked_add(length)
                .ok_or(ArrayError::LengthOutOfRange)?
        } else {
            start
        };
        if at < 0 {
            at = 0;
        }
        if at >= length {
            return Ok(-1);
        }
        let mut position = usize::try_from(at).map_err(|_| ArrayError::IndexOutOfRange)?;
        while position < len {
            if discern_compare(value, &elements[position]) {
                return i64::try_from(position).map_err(|_| ArrayError::LengthOutOfRange);
            }
            position += 1;
        }
        Ok(-1)
    }

    /// Removes every element; the named members stay.
    pub fn clear(&self) -> Result<(), ArrayError> {
        self.check()?;
        self.elements.borrow_mut().clear();
        Ok(())
    }

    /// Replaces every element with a copy of `source`'s elements.
    ///
    /// The target is emptied first, so an array assigned from itself ends
    /// empty. The named members are not touched.
    pub fn assign(&self, source: &Self) -> Result<(), ArrayError> {
        self.check()?;
        self.elements.borrow_mut().clear();
        let values = source.snapshot();
        *self.elements.borrow_mut() = values;
        Ok(())
    }

    /// Joins the elements with `delimiter` and answers the text.
    ///
    /// Every element becomes its text; with `purge_empty` the elements holding
    /// the empty value are skipped instead of contributing an empty field.
    pub fn join(&self, delimiter: &TjsString, purge_empty: bool) -> Result<TjsString, ArrayError> {
        self.check()?;
        let values = self.snapshot();
        let mut out = TjsString::new();
        let mut first = true;
        for value in &values {
            if purge_empty && matches!(value, Variant::Void) {
                continue;
            }
            if !first {
                out = out.concat(delimiter);
            }
            first = false;
            let text = value.to_string()?;
            out = out.concat(&text);
        }
        Ok(out)
    }

    /// Sorts the elements in `order`.
    ///
    /// With `stable` the elements that compare equal keep their order; without
    /// it their order is the sort's own and is not part of the contract. An
    /// element that has no order (an octet or an object) is
    /// [`ArrayError::Convert`], and the elements are left as they were.
    pub fn sort(&self, order: SortOrder, stable: bool) -> Result<(), ArrayError> {
        self.check()?;
        let mut values = self.snapshot();
        if stable {
            values = merge_sort(values, order)?;
        } else {
            heap_sort(&mut values, order)?;
        }
        *self.elements.borrow_mut() = values;
        Ok(())
    }

    /// Reverses the order of the elements.
    pub fn reverse(&self) -> Result<(), ArrayError> {
        self.check()?;
        self.elements.borrow_mut().reverse();
        Ok(())
    }

    /// Reads the element at `index`.
    ///
    /// A negative index counts from the end. A position past the end, in either
    /// direction, answers the empty value and does not grow the array; with
    /// [`MemberFlags::MUST_EXIST`] it is [`ObjectError::MemberNotFound`] instead.
    pub fn prop_get_by_num(&self, index: i64, flags: MemberFlags) -> Result<Variant, ObjectError> {
        self.check_valid()?;
        let len = self.len();
        let resolved = resolve_index(index, len).map_err(object_error)?;
        let length = i64::try_from(len).map_err(|_| ObjectError::CallNotServed)?;
        if resolved < 0 || resolved >= length {
            if flags.contains(MemberFlags::MUST_EXIST) {
                return Err(ObjectError::MemberNotFound);
            }
            return Ok(Variant::Void);
        }
        let position = usize::try_from(resolved).map_err(|_| ObjectError::CallNotServed)?;
        let elements = self.elements.borrow();
        elements
            .get(position)
            .cloned()
            .ok_or(ObjectError::MemberNotFound)
    }

    /// Writes `value` to the element at `index`.
    ///
    /// A negative index counts from the end. A write past the end grows the
    /// array with empty values, unless [`MemberFlags::MUST_EXIST`] is set, when
    /// it is [`ObjectError::MemberNotFound`]; a negative index below the start
    /// is [`ObjectError::MemberNotFound`] as well.
    pub fn prop_set_by_num(
        &self,
        index: i64,
        flags: MemberFlags,
        value: &Variant,
    ) -> Result<(), ObjectError> {
        self.check_valid()?;
        let len = self.len();
        let resolved = resolve_index(index, len).map_err(object_error)?;
        let length = i64::try_from(len).map_err(|_| ObjectError::CallNotServed)?;
        if resolved >= length {
            if flags.contains(MemberFlags::MUST_EXIST) {
                return Err(ObjectError::MemberNotFound);
            }
            let new_length = resolved.checked_add(1).ok_or(ObjectError::CallNotServed)?;
            let new_length = usize::try_from(new_length).map_err(|_| ObjectError::CallNotServed)?;
            self.resize(new_length).map_err(object_error)?;
        }
        if resolved < 0 {
            return Err(ObjectError::MemberNotFound);
        }
        let position = usize::try_from(resolved).map_err(|_| ObjectError::CallNotServed)?;
        let mut elements = self.elements.borrow_mut();
        let slot = elements
            .get_mut(position)
            .ok_or(ObjectError::CallNotServed)?;
        *slot = value.clone();
        Ok(())
    }

    /// Removes the element at `index`, shifting the tail down.
    ///
    /// A negative index counts from the end. A position outside the array is
    /// [`ObjectError::MemberNotFound`].
    pub fn delete_member_by_num(&self, index: i64) -> Result<(), ObjectError> {
        self.check_valid()?;
        let len = self.len();
        let resolved = resolve_index(index, len).map_err(object_error)?;
        let length = i64::try_from(len).map_err(|_| ObjectError::CallNotServed)?;
        if resolved < 0 || resolved >= length {
            return Err(ObjectError::MemberNotFound);
        }
        let position = usize::try_from(resolved).map_err(|_| ObjectError::CallNotServed)?;
        let mut elements = self.elements.borrow_mut();
        if position >= elements.len() {
            return Err(ObjectError::MemberNotFound);
        }
        elements.remove(position);
        Ok(())
    }

    /// The failure an invalidated array answers with.
    fn check_valid(&self) -> Result<(), ObjectError> {
        if self.members.is_valid() {
            Ok(())
        } else {
            Err(ObjectError::InvalidObject)
        }
    }

    /// The failure an invalidated array answers to an operation.
    fn check(&self) -> Result<(), ArrayError> {
        if self.members.is_valid() {
            Ok(())
        } else {
            Err(ArrayError::InvalidObject)
        }
    }

    /// The elements, cloned out so that no borrow covers a callback.
    fn snapshot(&self) -> Vec<Variant> {
        self.elements.borrow().clone()
    }

    /// Sets the number of elements, filling a grown tail with empty values.
    fn resize(&self, length: usize) -> Result<(), ArrayError> {
        let mut elements = self.elements.borrow_mut();
        if length <= elements.len() {
            elements.truncate(length);
            return Ok(());
        }
        let additional = length - elements.len();
        elements
            .try_reserve(additional)
            .map_err(|_| ArrayError::LengthOutOfRange)?;
        elements.resize(length, Variant::Void);
        Ok(())
    }

    /// Sets the count from the value a write to `count` or `length` holds.
    fn set_length(&self, length: &Variant) -> Result<(), ArrayError> {
        self.check()?;
        let length = length.to_integer()?;
        if length < 0 {
            return Err(ArrayError::LengthOutOfRange);
        }
        let length = usize::try_from(length).map_err(|_| ArrayError::LengthOutOfRange)?;
        self.resize(length)
    }
}

impl Default for ArrayObject {
    fn default() -> Self {
        Self::new()
    }
}

impl Dispatch for ArrayObject {
    fn prop_get(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        self.check_valid()?;
        if let Some(index) = script_index(name).map_err(object_error)? {
            return self.prop_get_by_num(index, flags);
        }
        // The instance's named member is looked up before the class member, the
        // way a write to a class member's name installs the write on the
        // instance.
        match self
            .members
            .prop_get(name, MemberFlags::IGNORE_PROPERTY, objthis)
        {
            Ok(_) => return self.members.prop_get(name, flags, objthis),
            Err(ObjectError::MemberNotFound) => {}
            Err(error) => return Err(error),
        }
        match ArrayMember::lookup(name) {
            Some(ArrayMember::Count | ArrayMember::Length) => {
                if flags.contains(MemberFlags::IGNORE_PROPERTY) {
                    return Err(ObjectError::CallNotServed);
                }
                let length = i64::try_from(self.len()).map_err(|_| ObjectError::CallNotServed)?;
                Ok(Variant::from(length))
            }
            // A method's value is a callable object, which the value layer does
            // not hold; the operation is reached through the array's methods.
            Some(_) => Err(ObjectError::CallNotServed),
            None => self.members.prop_get(name, flags, objthis),
        }
    }

    fn prop_set(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        value: &Variant,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        self.check_valid()?;
        if let Some(index) = script_index(name).map_err(object_error)? {
            return self.prop_set_by_num(index, flags, value);
        }
        match self
            .members
            .prop_get(name, MemberFlags::IGNORE_PROPERTY, objthis)
        {
            Ok(_) => return self.members.prop_set(name, flags, value, objthis),
            Err(ObjectError::MemberNotFound) => {}
            Err(error) => return Err(error),
        }
        match ArrayMember::lookup(name) {
            Some(ArrayMember::Count | ArrayMember::Length)
                if !flags.contains(MemberFlags::IGNORE_PROPERTY) =>
            {
                self.set_length(value).map_err(object_error)
            }
            // A write to a class member installs the value on this instance,
            // where it stands in front of the class's member from then on.
            Some(_) => {
                let flags = MemberFlags::from_bits(
                    (flags.bits() & !MemberFlags::MUST_EXIST.bits()) | MemberFlags::ENSURE.bits(),
                );
                self.members.prop_set(name, flags, value, objthis)
            }
            None => self.members.prop_set(name, flags, value, objthis),
        }
    }

    fn delete_member(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        self.check_valid()?;
        if let Some(index) = script_index(name).map_err(object_error)? {
            return self.delete_member_by_num(index);
        }
        match self.members.delete_member(name, flags, objthis) {
            Ok(()) => Ok(()),
            // A class member is not held by the instance, so a delete removes
            // only an instance's own copy and leaves the class's member in
            // place.
            Err(ObjectError::MemberNotFound) if ArrayMember::lookup(name).is_some() => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn enum_members(
        &self,
        flags: MemberFlags,
        callback: &mut dyn FnMut(&TjsString, u32, &Variant) -> bool,
    ) -> Result<(), ObjectError> {
        self.check_valid()?;
        let without_value = flags.contains(MemberFlags::ENUM_NO_VALUE);
        let elements: Vec<(TjsString, Variant)> = {
            let elements = self.elements.borrow();
            elements
                .iter()
                .enumerate()
                .map(|(index, element)| {
                    let name = TjsString::from(index.to_string());
                    let value = if without_value {
                        Variant::Void
                    } else {
                        element.clone()
                    };
                    (name, value)
                })
                .collect()
        };
        for (name, value) in &elements {
            if !callback(name, 0, value) {
                return Ok(());
            }
        }
        self.members.enum_members(flags, callback)
    }

    fn is_instance_of(
        &self,
        class: &TjsString,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<bool, ObjectError> {
        self.check_valid()?;
        let _ = (class, objthis);
        Ok(false)
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
        if !self.members.invalidate() {
            return false;
        }
        self.elements.borrow_mut().clear();
        true
    }
}

/// Maps an array operation's failure onto the dispatch surface.
///
/// A position error, a length that cannot be held, and a conversion failure
/// are calls the object cannot serve; the dispatch's own member errors keep
/// their identity.
fn object_error(error: ArrayError) -> ObjectError {
    match error {
        ArrayError::InvalidObject => ObjectError::InvalidObject,
        ArrayError::IndexOutOfRange | ArrayError::LengthOutOfRange | ArrayError::Convert(_) => {
            ObjectError::CallNotServed
        }
    }
}

/// The index a negative position resolves to, counting from the end.
fn resolve_index(index: i64, len: usize) -> Result<i64, ArrayError> {
    if index >= 0 {
        return Ok(index);
    }
    let length = i64::try_from(len).map_err(|_| ArrayError::LengthOutOfRange)?;
    index
        .checked_add(length)
        .ok_or(ArrayError::LengthOutOfRange)
}

/// The position an insert accepts: a negative index counts from the end, and
/// the count itself appends.
fn insert_position(index: i64, len: usize) -> Result<usize, ArrayError> {
    let resolved = resolve_index(index, len)?;
    if resolved < 0 {
        return Err(ArrayError::IndexOutOfRange);
    }
    let position = usize::try_from(resolved).map_err(|_| ArrayError::IndexOutOfRange)?;
    if position > len {
        return Err(ArrayError::IndexOutOfRange);
    }
    Ok(position)
}

/// The position an erase accepts: a negative index counts from the end, and
/// the count itself is out of range.
fn erase_position(index: i64, len: usize) -> Result<usize, ArrayError> {
    let resolved = resolve_index(index, len)?;
    if resolved < 0 {
        return Err(ArrayError::IndexOutOfRange);
    }
    let position = usize::try_from(resolved).map_err(|_| ArrayError::IndexOutOfRange)?;
    if position >= len {
        return Err(ArrayError::IndexOutOfRange);
    }
    Ok(position)
}

/// The index a member name denotes, when the name is an index name.
///
/// The shape is the class's: spaces, an optional sign, spaces, a digit, then
/// digits and periods, then spaces, and nothing else. The index is the integer
/// the digits before the first period read as, with the sign applied. A name
/// that is not an index is `None`; digits that do not fit a 64-bit integer are
/// [`ArrayError::IndexOutOfRange`].
fn script_index(name: &TjsString) -> Result<Option<i64>, ArrayError> {
    let units = name.as_utf16();
    let mut at = 0;
    while at < units.len() && is_space(units[at]) {
        at += 1;
    }
    if at == units.len() {
        return Ok(None);
    }
    let negative = match units[at] {
        unit if unit == u16::from(b'-') => {
            at += 1;
            true
        }
        unit if unit == u16::from(b'+') => {
            at += 1;
            false
        }
        _ => false,
    };
    while at < units.len() && is_space(units[at]) {
        at += 1;
    }
    if at == units.len() || !is_digit(units[at]) {
        return Ok(None);
    }
    let digits = at;
    while at < units.len() && (is_digit(units[at]) || units[at] == u16::from(b'.')) {
        at += 1;
    }
    while at < units.len() && is_space(units[at]) {
        at += 1;
    }
    if at != units.len() {
        return Ok(None);
    }
    let mut value = 0i64;
    let mut at = digits;
    while at < units.len() && is_digit(units[at]) {
        let digit = i64::from(units[at] - u16::from(b'0'));
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(digit))
            .ok_or(ArrayError::IndexOutOfRange)?;
        at += 1;
    }
    if negative {
        value = value.checked_neg().ok_or(ArrayError::IndexOutOfRange)?;
    }
    Ok(Some(value))
}

/// Whether the code unit is one the name rule skips.
fn is_space(unit: u16) -> bool {
    matches!(unit, 0x20 | 0x09 | 0x0a | 0x0b | 0x0c | 0x0d)
}

/// Whether the code unit is an ASCII digit.
fn is_digit(unit: u16) -> bool {
    unit >= u16::from(b'0') && unit <= u16::from(b'9')
}

/// The same-kind equality `find` and `remove` use.
fn discern_compare(left: &Variant, right: &Variant) -> bool {
    match (left, right) {
        (Variant::Void, Variant::Void) => true,
        (Variant::Integer(left), Variant::Integer(right)) => left == right,
        (Variant::Real(left), Variant::Real(right)) => left == right,
        (Variant::String(left), Variant::String(right)) => left == right,
        (Variant::Octet(left), Variant::Octet(right)) => left == right,
        (Variant::Object(left), Variant::Object(right)) => closure_identity(left, right),
        _ => false,
    }
}

/// Whether two object values are the same object with the same receiver.
fn closure_identity(left: &Closure, right: &Closure) -> bool {
    same_handle(&left.object, &right.object) && same_handle(&left.objthis, &right.objthis)
}

/// Whether two object handles name the same object; two absent handles match.
fn same_handle(left: &Option<Rc<dyn ObjectHandle>>, right: &Option<Rc<dyn ObjectHandle>>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => Rc::ptr_eq(left, right),
        (None, None) => true,
        _ => false,
    }
}

/// Whether `left` sorts before `right` in `order`.
fn less(left: &Variant, right: &Variant, order: SortOrder) -> Result<bool, ArrayError> {
    match order {
        SortOrder::Ascending => Ok(left.compare(right)? == Some(Ordering::Less)),
        SortOrder::Descending => Ok(left.compare(right)? == Some(Ordering::Greater)),
        SortOrder::NumericAscending => {
            if both_text(left, right) {
                Ok(number_less(left.to_number()?, right.to_number()?))
            } else {
                Ok(left.compare(right)? == Some(Ordering::Less))
            }
        }
        SortOrder::NumericDescending => {
            if both_text(left, right) {
                Ok(number_less(right.to_number()?, left.to_number()?))
            } else {
                Ok(left.compare(right)? == Some(Ordering::Greater))
            }
        }
        SortOrder::TextAscending => {
            let left = left.to_string()?;
            let right = right.to_string()?;
            Ok(left.cmp(&right) == Ordering::Less)
        }
        SortOrder::TextDescending => {
            let left = left.to_string()?;
            let right = right.to_string()?;
            Ok(left.cmp(&right) == Ordering::Greater)
        }
    }
}

/// Whether both values are texts.
fn both_text(left: &Variant, right: &Variant) -> bool {
    matches!(left, Variant::String(_)) && matches!(right, Variant::String(_))
}

/// The order of two numbers: integers compare exactly, every other pair as
/// reals.
fn number_less(left: Number, right: Number) -> bool {
    match (left, right) {
        (Number::Integer(left), Number::Integer(right)) => left < right,
        (left, right) => left.to_real() < right.to_real(),
    }
}

/// A stable merge sort over `values`.
fn merge_sort(mut values: Vec<Variant>, order: SortOrder) -> Result<Vec<Variant>, ArrayError> {
    let len = values.len();
    if len < 2 {
        return Ok(values);
    }
    let mut buffer = values.clone();
    let mut width = 1;
    while width < len {
        let doubled = width.saturating_mul(2);
        let mut start = 0;
        while start < len {
            let middle = usize::min(start.saturating_add(width), len);
            let end = usize::min(start.saturating_add(doubled), len);
            let mut left = start;
            let mut right = middle;
            let mut out = start;
            while left < middle && right < end {
                if less(&values[right], &values[left], order)? {
                    buffer[out] = values[right].clone();
                    right += 1;
                } else {
                    buffer[out] = values[left].clone();
                    left += 1;
                }
                out += 1;
            }
            while left < middle {
                buffer[out] = values[left].clone();
                left += 1;
                out += 1;
            }
            while right < end {
                buffer[out] = values[right].clone();
                right += 1;
                out += 1;
            }
            start = end;
        }
        std::mem::swap(&mut values, &mut buffer);
        width = doubled;
    }
    Ok(values)
}

/// An in-place heap sort over `values`.
fn heap_sort(values: &mut [Variant], order: SortOrder) -> Result<(), ArrayError> {
    let len = values.len();
    if len < 2 {
        return Ok(());
    }
    for start in (0..len / 2).rev() {
        sift_down(values, start, len, order)?;
    }
    for end in (1..len).rev() {
        values.swap(0, end);
        sift_down(values, 0, end, order)?;
    }
    Ok(())
}

/// Restores the heap below `root`, within `end`.
fn sift_down(
    values: &mut [Variant],
    mut root: usize,
    end: usize,
    order: SortOrder,
) -> Result<(), ArrayError> {
    loop {
        let left = root.saturating_mul(2).saturating_add(1);
        if left >= end {
            return Ok(());
        }
        let mut child = left;
        let right = left.saturating_add(1);
        if right < end && less(&values[child], &values[right], order)? {
            child = right;
        }
        if !less(&values[root], &values[child], order)? {
            return Ok(());
        }
        values.swap(root, child);
        root = child;
    }
}

#[cfg(test)]
mod tests {
    use super::{ArrayError, ArrayObject, SortOrder, Variant, script_index};
    use crate::vm::runtime::object::{Dispatch, MemberFlags, ObjectError};
    use crate::vm::runtime::string::TjsString;

    /// A name.
    fn text(name: &str) -> TjsString {
        TjsString::from(name)
    }

    /// The error a dispatch call answered, panicking with what it answered
    /// instead.
    fn error_of<T: std::fmt::Debug>(result: Result<T, ObjectError>) -> ObjectError {
        match result {
            Ok(value) => panic!("the call answered {value:?} instead of an error"),
            Err(error) => error,
        }
    }

    // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:53-93
    #[test]
    fn a_numeric_name_is_an_index() {
        assert_eq!(script_index(&text("0")), Ok(Some(0)));
        assert_eq!(script_index(&text(" 12 ")), Ok(Some(12)));
        assert_eq!(script_index(&text("-2")), Ok(Some(-2)));
        assert_eq!(script_index(&text(" + 3")), Ok(Some(3)));
        // the rule reads digits and periods, and the index stops at the period
        assert_eq!(script_index(&text("2.9")), Ok(Some(2)));
        assert_eq!(script_index(&text("1.")), Ok(Some(1)));
        // a sign with no digit, a letter, and an empty or blank name are not
        // indexes
        assert_eq!(script_index(&text("-")), Ok(None));
        assert_eq!(script_index(&text(".5")), Ok(None));
        assert_eq!(script_index(&text("2x")), Ok(None));
        assert_eq!(script_index(&text("")), Ok(None));
        assert_eq!(script_index(&text("  ")), Ok(None));
        // digits beyond a 64-bit integer are a typed failure, not a wrap
        assert_eq!(
            script_index(&text("999999999999999999999999")),
            Err(ArrayError::IndexOutOfRange)
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1389-1404
    // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1596-1607
    // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1618-1646
    #[test]
    fn a_hole_and_a_position_past_the_end_are_told_apart() {
        let array = ArrayObject::new();
        array
            .prop_set_by_num(2, MemberFlags::NONE, &Variant::from(1i64))
            .expect("the write grows the array");
        assert_eq!(array.len(), 3);
        // a hole is present and reads as the empty value
        assert!(matches!(
            array.prop_get_by_num(0, MemberFlags::MUST_EXIST),
            Ok(Variant::Void)
        ));
        // a position past the end reads as the empty value but does not exist
        assert!(matches!(
            array.prop_get_by_num(3, MemberFlags::NONE),
            Ok(Variant::Void)
        ));
        assert_eq!(
            error_of(array.prop_get_by_num(3, MemberFlags::MUST_EXIST)),
            ObjectError::MemberNotFound
        );
        // the read past the end did not grow the array
        assert_eq!(array.len(), 3);
        // a write at the count appends, and a write with the must-exist flag
        // does not grow
        array
            .prop_set_by_num(3, MemberFlags::NONE, &Variant::from(2i64))
            .expect("the write appends");
        assert_eq!(array.len(), 4);
        assert_eq!(
            error_of(array.prop_set_by_num(8, MemberFlags::MUST_EXIST, &Variant::from(3i64))),
            ObjectError::MemberNotFound
        );
        assert_eq!(array.len(), 4);
    }

    // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1512-1538
    // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1660-1684
    #[test]
    fn an_index_delete_shifts_and_a_negative_index_counts_from_the_end() {
        let array = ArrayObject::new();
        array
            .push(&[
                Variant::from(1i64),
                Variant::from(2i64),
                Variant::from(3i64),
            ])
            .expect("the values are appended");
        array
            .delete_member_by_num(-2)
            .expect("the middle element is removed");
        assert_eq!(array.len(), 2);
        assert!(matches!(
            array.prop_get_by_num(1, MemberFlags::NONE),
            Ok(Variant::Integer(3))
        ));
        assert_eq!(
            error_of(array.delete_member_by_num(2)),
            ObjectError::MemberNotFound
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:145-222
    // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:602-724
    #[test]
    fn the_sort_orders_are_the_letters() {
        assert_eq!(SortOrder::from_letter(Some('-')), SortOrder::Descending);
        assert_eq!(
            SortOrder::from_letter(Some('0')),
            SortOrder::NumericAscending
        );
        assert_eq!(SortOrder::from_letter(Some('a')), SortOrder::TextAscending);
        assert_eq!(SortOrder::from_letter(Some('?')), SortOrder::Ascending);
        assert_eq!(SortOrder::from_letter(None), SortOrder::Ascending);

        let array = ArrayObject::new();
        array
            .push(&[Variant::from("10"), Variant::from("9"), Variant::from("1")])
            .expect("the texts are appended");
        array
            .sort(SortOrder::NumericAscending, false)
            .expect("the values sort");
        let joined = array
            .join(&TjsString::from(","), false)
            .expect("the values join");
        assert_eq!(joined, TjsString::from("1,9,10"));
        array
            .sort(SortOrder::TextAscending, true)
            .expect("the values sort");
        let joined = array
            .join(&TjsString::from(","), false)
            .expect("the values join");
        assert_eq!(joined, TjsString::from("1,10,9"));
    }

    // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1455-1509
    #[test]
    fn the_find_and_remove_comparison_is_by_kind() {
        let array = ArrayObject::new();
        array
            .push(&[Variant::from(1i64), Variant::from(1.0f64)])
            .expect("the numbers are appended");
        // an integer and a real are different values for this comparison
        assert_eq!(array.find(&Variant::from(1i64), 0), Ok(0));
        assert_eq!(array.find(&Variant::from(1.0f64), 0), Ok(1));
        assert_eq!(array.remove(&Variant::from(1i64), false), Ok(1));
        assert_eq!(array.len(), 1);
    }

    // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:994-1031
    #[test]
    fn the_length_property_reads_and_writes_the_count() {
        let array = ArrayObject::new();
        array
            .prop_set(
                &text("length"),
                MemberFlags::ENSURE,
                &Variant::from(3i64),
                None,
            )
            .expect("the length grows the array");
        assert_eq!(array.len(), 3);
        assert!(matches!(
            array.prop_get(&text("length"), MemberFlags::NONE, None),
            Ok(Variant::Integer(3))
        ));
        array
            .prop_set(
                &text("count"),
                MemberFlags::NONE,
                &Variant::from(1i64),
                None,
            )
            .expect("the count shrinks the array");
        assert_eq!(array.len(), 1);
        // a negative length cannot be held
        assert!(matches!(
            array.prop_set(
                &text("length"),
                MemberFlags::NONE,
                &Variant::from(-1i64),
                None
            ),
            Err(ObjectError::CallNotServed)
        ));
    }
}
