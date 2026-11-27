//! The frame: one call's register window, and the pages it comes from.
//!
//! The VM has no operand stack. Every context carries a frame size chosen at
//! compile time, and a call allocates that many values as one block, puts the
//! frame base `ra` inside it, and every instruction names registers as a byte
//! offset from `ra`. This module is that block and its layout.
//!
//! # The layout
//!
//! The window has `max_variable_count + variable_reserve_count + 1 +
//! max_frame_count` values, and `ra` sits at
//! `max_variable_count + variable_reserve_count` into it, so the block is
//! divided into a downward run and an upward one:
//!
//! | index | what it holds |
//! |---|---|
//! | `ra[0]` … `ra[max_frame_count]` | the temporaries; `ra[0]` is the discard register, and an operand address of `0` asks for no result |
//! | `ra[-1]` | `this` — the receiver the frame's members are read on |
//! | `ra[-2]` | the objthis-context: the receiver with the global object behind it |
//! | `ra[-3]`, `ra[-4]`, … | the declared arguments, in order |
//! | `ra[-3-n]` and below | the local variables, `n` being the name space's index |
//!
//! The last two rows are one run of registers, not two. A declared argument joins
//! the local name space before it is stored, so name-space index `n` is the same
//! register whichever way the name arrived, and
//! `variable_reserve_count` — two for every context kind except a property
//! declaration, which reserves none — is what saves `ra[-1]` and `ra[-2]` from
//! being a local.
//!
//! # The block allocator
//!
//! A call's block is taken from a stack of pages and handed back when the call
//! returns, so a call that repeats reuses one allocation. A block at least as
//! large as a page is not pooled, because pooling it would hold the memory for
//! one call that may never happen again. The values a call leaves behind are not
//! cleared: a register the new call never writes holds the previous call's value,
//! and the code clears what it needs.

use std::rc::Rc;

use super::Fault;
use super::decode;
use crate::vm::runtime::array::ArrayObject;
use crate::vm::runtime::object::{Dispatch, MemberFlags, ObjectError};
use crate::vm::runtime::string::TjsString;
use crate::vm::runtime::variant::{Closure, Variant};

/// The size of a pooled block, in variants.
///
/// A block at least this large is allocated for its own call and not pooled.
pub const PAGE: usize = 1024;

/// The executable code of one context: the stream, its constant area, and the
/// counts the frame is sized from.
///
/// The counts are the generator's, and the frame trusts them: a context that
/// names a register outside the window it declares is a defect in the stream,
/// and the frame refuses the call rather than reading outside the block.
#[derive(Clone, Debug)]
pub struct Code {
    /// The words of the code stream.
    pub code: Vec<i32>,
    /// The constant area a `CONST` instruction reads.
    pub data: Vec<Variant>,
    /// The greatest number of local variables the context held.
    pub max_variable_count: i32,
    /// The registers the context kind reserves for the receiver: two, or none
    /// for a property declaration.
    pub variable_reserve_count: i32,
    /// The greatest number of temporaries the context needs.
    pub max_frame_count: i32,
    /// The number of the function's declared arguments.
    pub func_decl_arg_count: i32,
    /// The base of the function's collapsed argument, or `-1` for none.
    pub func_decl_collapse_base: i32,
    /// The base of the function's unnamed argument array.
    pub func_decl_unnamed_arg_array_base: i32,
    /// The name of the context, for a message.
    pub name: String,
}

/// A context with nothing declared.
///
/// The collapse base is `-1`, which is the value that names no collapse
/// argument: zero names a real base, the first declared argument, and a default
/// of zero would make every frame build an array nobody asked for.
impl Default for Code {
    fn default() -> Self {
        Self {
            code: Vec::new(),
            data: Vec::new(),
            max_variable_count: 0,
            variable_reserve_count: 0,
            max_frame_count: 0,
            func_decl_arg_count: 0,
            func_decl_collapse_base: -1,
            func_decl_unnamed_arg_array_base: 0,
            name: String::new(),
        }
    }
}

impl Code {
    /// The number of values one call of this context needs.
    ///
    /// A count outside the range a length can hold is refused: a context whose
    /// frame does not fit is a defect in the stream, and there is no substitute
    /// size to guess.
    pub fn block_len(&self) -> Result<usize, Fault> {
        let counts = [
            self.max_variable_count,
            self.variable_reserve_count,
            self.max_frame_count,
            self.func_decl_arg_count,
        ];
        for count in counts {
            if count < 0 {
                return Err(Fault::Broken("a frame count is negative"));
            }
        }
        let window = self
            .max_variable_count
            .checked_add(self.variable_reserve_count)
            .and_then(|sum| sum.checked_add(1))
            .and_then(|sum| sum.checked_add(self.max_frame_count))
            .ok_or(Fault::Broken("a frame does not fit a length"))?;
        usize::try_from(window).map_err(|_| Fault::Broken("a frame does not fit a length"))
    }

    /// The index of the frame base inside a block of [`Self::block_len`].
    pub fn frame_base(&self) -> Result<usize, Fault> {
        let base = self
            .max_variable_count
            .checked_add(self.variable_reserve_count)
            .ok_or(Fault::Broken("a frame does not fit a length"))?;
        usize::try_from(base).map_err(|_| Fault::Broken("a frame does not fit a length"))
    }

    /// A value of the constant area, or the failure that says the instruction
    /// named a slot that is not there.
    ///
    /// The operand is an offset in the constant area, in the same units a
    /// register address uses, so the slot is that offset over the size of a
    /// variant.
    pub fn constant(&self, offset: i32) -> Result<&Variant, Fault> {
        let slot = super::decode::register(offset)?;
        let slot =
            usize::try_from(slot).map_err(|_| Fault::Broken("a constant slot is negative"))?;
        self.data.get(slot).ok_or(Fault::Broken(
            "a constant slot is outside the constant area",
        ))
    }
}

/// The pages a call's block comes from.
///
/// The page is the unit of reuse: a call of a pooled size takes the page the
/// previous call of that size gave back, so a call in a loop allocates once.
#[derive(Debug, Default)]
pub struct BlockStack {
    /// The pages that are free to be handed out, most recently returned last.
    pages: Vec<Vec<Variant>>,
    /// How many pages have ever been made, for the measurement.
    made: usize,
}

impl BlockStack {
    /// A stack with no page.
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of pages the stack holds.
    pub fn pages(&self) -> usize {
        self.pages.len()
    }

    /// The number of pages that have ever been made.
    ///
    /// A call that repeats the same size makes one page and reuses it, which is
    /// what this counter proves.
    pub fn made(&self) -> usize {
        self.made
    }

    /// A block of `len` values, reusing a page when there is one.
    ///
    /// A block at least as large as a page is not pooled: it is made for its own
    /// call and dropped when the call returns.
    pub fn allocate(&mut self, len: usize) -> Vec<Variant> {
        if len >= PAGE {
            return vec![Variant::Void; len];
        }
        let mut block = match self.pages.pop() {
            Some(page) => page,
            None => {
                self.made += 1;
                Vec::with_capacity(PAGE)
            }
        };
        block.clear();
        block.resize(len, Variant::Void);
        block
    }

    /// Takes a block back, to be handed to the next call of a pooled size.
    ///
    /// The block's values are dropped first: a register that held an object must
    /// not keep the object alive until the page is used again.
    pub fn release(&mut self, mut block: Vec<Variant>) {
        if block.len() >= PAGE || block.capacity() < PAGE {
            return;
        }
        block.clear();
        self.pages.push(block);
    }
}

/// The objthis-context: the receiver a frame's own scope looks in, with the
/// global object behind it.
///
/// The two are one view, and a member the receiver does not hold is looked for
/// in the global object. Only a lookup that answers "no such member" falls
/// through; any other failure is the receiver's answer and travels unchanged.
/// The walk does not run when the two name one object, because that would look
/// twice in the same place.
pub struct ObjThisContext {
    /// The receiver the frame acts on.
    primary: Rc<dyn Dispatch>,
    /// The object the lookup continues into.
    fallback: Option<Rc<dyn Dispatch>>,
}

impl ObjThisContext {
    /// The view over `primary`, continuing into `fallback`.
    pub fn new(primary: Rc<dyn Dispatch>, fallback: Option<Rc<dyn Dispatch>>) -> Self {
        Self { primary, fallback }
    }

    /// The fallback, when it is a different object than the primary.
    fn second(&self) -> Option<&Rc<dyn Dispatch>> {
        match &self.fallback {
            Some(fallback) if !Rc::ptr_eq(fallback, &self.primary) => Some(fallback),
            _ => None,
        }
    }

    /// The receiver to pass to the primary: the caller's own when it named one,
    /// and otherwise the primary itself.
    fn receiver<'a>(&'a self, objthis: Option<&'a dyn Dispatch>) -> Option<&'a dyn Dispatch> {
        match objthis {
            Some(objthis) => Some(objthis),
            None => Some(self.primary.as_ref()),
        }
    }

    /// The receiver to pass to the fallback.
    fn second_receiver<'a>(
        &'a self,
        objthis: Option<&'a dyn Dispatch>,
        second: &'a Rc<dyn Dispatch>,
    ) -> Option<&'a dyn Dispatch> {
        match objthis {
            Some(objthis) => Some(objthis),
            None => Some(second.as_ref()),
        }
    }
}

impl Dispatch for ObjThisContext {
    fn prop_get(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        let answer = self.primary.prop_get(name, flags, self.receiver(objthis));
        match answer {
            Err(ObjectError::MemberNotFound) => match self.second() {
                Some(second) => second.prop_get(name, flags, self.second_receiver(objthis, second)),
                None => Err(ObjectError::MemberNotFound),
            },
            other => other,
        }
    }

    fn prop_set(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        value: &Variant,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        let answer = self
            .primary
            .prop_set(name, flags, value, self.receiver(objthis));
        match answer {
            Err(ObjectError::MemberNotFound) => match self.second() {
                Some(second) => {
                    second.prop_set(name, flags, value, self.second_receiver(objthis, second))
                }
                None => Err(ObjectError::MemberNotFound),
            },
            other => other,
        }
    }

    fn delete_member(
        &self,
        name: &TjsString,
        flags: MemberFlags,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        let answer = self
            .primary
            .delete_member(name, flags, self.receiver(objthis));
        match answer {
            Err(ObjectError::MemberNotFound) => match self.second() {
                Some(second) => {
                    second.delete_member(name, flags, self.second_receiver(objthis, second))
                }
                None => Err(ObjectError::MemberNotFound),
            },
            other => other,
        }
    }

    /// A member walk is not served.
    ///
    /// The view exists to look a name up, not to enumerate two objects as one,
    /// so a walk of it is not served.
    fn enum_members(
        &self,
        _flags: MemberFlags,
        _callback: &mut dyn FnMut(&TjsString, u32, &Variant) -> bool,
    ) -> Result<(), ObjectError> {
        Err(ObjectError::CallNotServed)
    }

    fn is_instance_of(
        &self,
        class: &TjsString,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<bool, ObjectError> {
        let answer = self.primary.is_instance_of(class, self.receiver(objthis));
        match answer {
            Err(ObjectError::MemberNotFound) => match self.second() {
                Some(second) => second.is_instance_of(class, self.second_receiver(objthis, second)),
                None => Err(ObjectError::MemberNotFound),
            },
            other => other,
        }
    }

    /// The view carries the primary's class names.
    fn class_names(&self) -> Vec<TjsString> {
        self.primary.class_names()
    }

    /// A name added to the view is added to the object it stands in front of.
    fn add_class_name(&self, name: &TjsString) -> Result<(), ObjectError> {
        self.primary.add_class_name(name)
    }

    /// The view is usable while the primary is.
    fn is_valid(&self) -> bool {
        self.primary.is_valid()
    }

    /// An invalidation reaches the primary and nothing else.
    fn invalidate(&self) -> bool {
        self.primary.invalidate()
    }

    /// A view that stands in front of a context is the same call: the code a call
    /// runs is the primary's, and the name is looked up through the view, so an
    /// object behind the context answers a name the context does not hold.
    fn context(&self) -> Option<&super::ContextObject> {
        self.primary.context()
    }

    /// The view crosses as the object it stands in front of.
    fn engine_id(&self) -> Option<crate::Tjs2ValueId> {
        self.primary
            .engine_id()
            .or_else(|| self.second().and_then(|second| second.engine_id()))
    }

    /// A member the engine cannot hold is the primary's, then the fallback's.
    fn rust_member(&self, name: &TjsString) -> Option<Variant> {
        self.primary
            .rust_member(name)
            .or_else(|| self.second().and_then(|second| second.rust_member(name)))
    }

    fn func_call(
        &self,
        name: &TjsString,
        argv: Option<&[Variant]>,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        let answer = self.primary.func_call(name, argv, self.receiver(objthis));
        match answer {
            Err(ObjectError::MemberNotFound) => match self.second() {
                Some(second) => second.func_call(name, argv, self.second_receiver(objthis, second)),
                None => Err(ObjectError::MemberNotFound),
            },
            other => other,
        }
    }

    fn create_new(
        &self,
        name: &TjsString,
        argv: Option<&[Variant]>,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        let answer = self.primary.create_new(name, argv, self.receiver(objthis));
        match answer {
            Err(ObjectError::MemberNotFound) => match self.second() {
                Some(second) => {
                    second.create_new(name, argv, self.second_receiver(objthis, second))
                }
                None => Err(ObjectError::MemberNotFound),
            },
            other => other,
        }
    }

    fn operation(
        &self,
        op: &TjsString,
        value: &Variant,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<bool, ObjectError> {
        let answer = self.primary.operation(op, value, self.receiver(objthis));
        match answer {
            Err(ObjectError::MemberNotFound) => match self.second() {
                Some(second) => second.operation(op, value, self.second_receiver(objthis, second)),
                None => Err(ObjectError::MemberNotFound),
            },
            other => other,
        }
    }

    fn prop_get_by_num(
        &self,
        index: i64,
        flags: MemberFlags,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        let answer = self
            .primary
            .prop_get_by_num(index, flags, self.receiver(objthis));
        match answer {
            Err(ObjectError::MemberNotFound) => match self.second() {
                Some(second) => {
                    second.prop_get_by_num(index, flags, self.second_receiver(objthis, second))
                }
                None => Err(ObjectError::MemberNotFound),
            },
            other => other,
        }
    }

    fn prop_set_by_num(
        &self,
        index: i64,
        flags: MemberFlags,
        value: &Variant,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        let answer = self
            .primary
            .prop_set_by_num(index, flags, value, self.receiver(objthis));
        match answer {
            Err(ObjectError::MemberNotFound) => match self.second() {
                Some(second) => second.prop_set_by_num(
                    index,
                    flags,
                    value,
                    self.second_receiver(objthis, second),
                ),
                None => Err(ObjectError::MemberNotFound),
            },
            other => other,
        }
    }
}

impl std::fmt::Debug for ObjThisContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ObjThisContext")
            .field("primary", &Rc::as_ptr(&self.primary))
            .field("fallback", &self.second().map(Rc::as_ptr))
            .finish()
    }
}

/// One call's register window.
///
/// The window borrows nothing: it owns its block, which the [`BlockStack`] takes
/// back when the call returns.
#[derive(Debug)]
pub struct Frame {
    /// The values of the window, in the order the block holds them.
    block: Vec<Variant>,
    /// The index of the frame base inside the block.
    base: usize,
    /// What a register below the window reads as when no caller filled it.
    absent: Variant,
}

impl Frame {
    /// The window for one call of `code`, with its receiver, its receiver's
    /// fallback, and the arguments the caller passed.
    ///
    /// The order is: place the base, build the objthis-context, set the
    /// receiver, clear the discard register, copy the declared arguments, and
    /// build the collapse array from the surplus ones.
    pub fn new(
        block: Vec<Variant>,
        code: &Code,
        receiver: Option<Rc<dyn Dispatch>>,
        fallback: Option<Rc<dyn Dispatch>>,
        argv: &[Variant],
    ) -> Result<Self, Fault> {
        let base = code.frame_base()?;
        if block.len() != code.block_len()? || base >= block.len() {
            return Err(Fault::Broken("a block is not the size its code asks for"));
        }
        // the objthis-context, in ra[-2]: the receiver with the global object
        // behind it, or the fallback alone when there is no receiver
        let context = match (&receiver, &fallback) {
            (Some(receiver), fallback) => Some(Rc::new(ObjThisContext::new(
                receiver.clone(),
                fallback.clone(),
            )) as Rc<dyn Dispatch>),
            (None, Some(fallback)) => Some(fallback.clone()),
            (None, None) => None,
        };
        let mut frame = Self {
            block,
            base,
            absent: Variant::Void,
        };
        // The three registers below the base are the caller's. A frame with no
        // caller writes them through the path that leaves them absent, so a
        // context that reserves none (`variable_reserve_count` zero) still
        // builds.
        frame.set(REG_CONTEXT, object_or_null(context))?;
        // ra[-1]: the receiver itself, with itself as its own receiver
        frame.set(REG_THIS, object_or_null(receiver.clone()))?;
        // ra[0]: the discard register, cleared
        frame.set(0, Variant::Void)?;
        // the declared arguments, into ra[-3] downwards, the missing ones empty
        let declared = usize::try_from(code.func_decl_arg_count)
            .map_err(|_| Fault::Broken("a declared argument count is negative"))?;
        for index in 0..declared {
            let value = match argv.get(index) {
                Some(value) => value.clone(),
                None => Variant::Void,
            };
            let address = address_of_argument(index)?;
            frame.set(address, value)?;
        }
        // the named collapse argument: an array of the surplus arguments
        if code.func_decl_collapse_base >= 0 {
            frame.build_collapse(code, argv)?;
        }
        Ok(frame)
    }

    /// Builds the array a named collapse argument receives.
    fn build_collapse(&mut self, code: &Code, argv: &[Variant]) -> Result<(), Fault> {
        let base = usize::try_from(code.func_decl_collapse_base)
            .map_err(|_| Fault::Broken("a collapse base is negative"))?;
        let address = address_of_argument(base)?;
        let array = Rc::new(ArrayObject::new());
        for value in argv.iter().skip(base) {
            array
                .append(value.clone())
                .map_err(|_| Fault::Broken("a collapse array does not fit a length"))?;
        }
        let handle = array as Rc<dyn Dispatch>;
        self.set(
            address,
            Variant::Object(Closure {
                object: Some(handle.clone()),
                objthis: Some(handle),
            }),
        )?;
        Ok(())
    }

    /// The block slot a register address names, when the window holds it.
    ///
    /// `Ok(None)` is a register below the frame base: those belong to the
    /// caller, which wrote the arguments into its own stack, so a frame with no
    /// caller has nothing there. A register above the window is a defect in the
    /// stream and is refused.
    fn locate(&self, address: i32) -> Result<Option<usize>, Fault> {
        // an instruction's address is an offset in bytes from the base register,
        // so a negative one names the caller's registers below it
        let offset = decode::register(address)?;
        let index = i64::try_from(self.base)
            .map_err(|_| Fault::Broken("a frame base does not fit an address"))?
            .checked_add(i64::from(offset))
            .ok_or(Fault::Broken("a register address is out of range"))?;
        if index < 0 {
            return Ok(None);
        }
        let index = usize::try_from(index).map_err(|_| Fault::RegisterOutOfWindow { address })?;
        if index >= self.block.len() {
            return Err(Fault::RegisterOutOfWindow { address });
        }
        Ok(Some(index))
    }

    /// The value a register address names.
    ///
    /// A register below the base reads as the absent value: no caller filled it,
    /// and reading it is not an error.
    pub fn reg(&self, address: i32) -> Result<&Variant, Fault> {
        match self.locate(address)? {
            Some(index) => self
                .block
                .get(index)
                .ok_or(Fault::RegisterOutOfWindow { address }),
            None => Ok(&self.absent),
        }
    }

    /// Writes a register, tolerating the registers that belong to the caller.
    ///
    /// This is the frame setup's writer: an argument past the window's low end
    /// was the caller's to place, and a frame with no caller places nothing, so
    /// the register stays absent rather than the call failing.
    pub fn set(&mut self, address: i32, value: Variant) -> Result<(), Fault> {
        match self.locate(address)? {
            Some(index) => match self.block.get_mut(index) {
                Some(slot) => {
                    *slot = value;
                    Ok(())
                }
                None => Err(Fault::RegisterOutOfWindow { address }),
            },
            None => Ok(()),
        }
    }

    /// The value a register address names, for change.
    ///
    /// An instruction never writes below its own frame: the registers there are
    /// the caller's, so a write to one is a defect and is refused.
    pub fn reg_mut(&mut self, address: i32) -> Result<&mut Variant, Fault> {
        match self.locate(address)? {
            Some(index) => self
                .block
                .get_mut(index)
                .ok_or(Fault::RegisterOutOfWindow { address }),
            None => Err(Fault::RegisterOutOfWindow { address }),
        }
    }

    /// The receiver the frame acts on, when it has one.
    pub fn receiver(&self) -> Option<Rc<dyn Dispatch>> {
        match self.reg(REG_THIS) {
            Ok(Variant::Object(closure)) => closure.object.clone(),
            _ => None,
        }
    }

    /// The objthis-context, when the frame has one.
    pub fn context(&self) -> Option<Rc<dyn Dispatch>> {
        match self.reg(REG_CONTEXT) {
            Ok(Variant::Object(closure)) => closure.object.clone(),
            _ => None,
        }
    }

    /// The number of values in the window.
    pub fn len(&self) -> usize {
        self.block.len()
    }

    /// Whether the window holds no value.
    pub fn is_empty(&self) -> bool {
        self.block.is_empty()
    }

    /// The index of the frame base.
    pub fn base(&self) -> usize {
        self.base
    }

    /// The block, to be handed back to the [`BlockStack`].
    pub fn into_block(self) -> Vec<Variant> {
        self.block
    }

    /// Clears `count` registers from `address` upwards.
    ///
    /// The consecutive clear, which the generator does not emit and a bytecode
    /// block can carry.
    pub fn clear_run(&mut self, address: i32, count: i32) -> Result<(), Fault> {
        let count =
            usize::try_from(count).map_err(|_| Fault::Broken("a clear count is negative"))?;
        let step = crate::vm::codegen::opcode::VARIANT_SIZE;
        for position in 0..count {
            let position =
                i32::try_from(position).map_err(|_| Fault::Broken("a clear run is too long"))?;
            let address = address
                .checked_add(
                    position
                        .checked_mul(step)
                        .ok_or(Fault::Broken("a clear run is too long"))?,
                )
                .ok_or(Fault::Broken("a clear run is too long"))?;
            *self.reg_mut(address)? = Variant::Void;
        }
        Ok(())
    }
}

/// The register address of the receiver.
pub const REG_THIS: i32 = -crate::vm::codegen::opcode::VARIANT_SIZE;

/// The register address of the objthis-context.
pub const REG_CONTEXT: i32 = -2 * crate::vm::codegen::opcode::VARIANT_SIZE;

/// The register address the `index`-th declared argument is stored at.
///
/// The first argument is at `ra[-3]`, so the argument's own register is
/// `-3 - index`.
pub fn address_of_argument(index: usize) -> Result<i32, Fault> {
    let index = i32::try_from(index)
        .map_err(|_| Fault::Broken("a declared argument count is too large"))?;
    index
        .checked_add(3)
        .and_then(|offset| offset.checked_neg())
        .and_then(|register| register.checked_mul(crate::vm::codegen::opcode::VARIANT_SIZE))
        .ok_or(Fault::Broken(
            "a declared argument register is out of range",
        ))
}

/// The object a handle names, or the null object when there is none.
fn object_or_null(handle: Option<Rc<dyn Dispatch>>) -> Variant {
    match handle {
        Some(handle) => Variant::Object(Closure {
            object: Some(handle.clone()),
            objthis: Some(handle),
        }),
        None => Variant::Object(Closure::null()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::runtime::object::CustomObject;

    fn object() -> Rc<dyn Dispatch> {
        Rc::new(CustomObject::new(Vec::new()))
    }

    fn block_of(code: &Code) -> Vec<Variant> {
        vec![Variant::Void; code.block_len().expect("the frame fits")]
    }

    /// A context that names no local and no argument still has its receiver and
    /// its receiver's fallback, and the window is the four values the counts
    /// name: two reserved registers, the discard register and no temporary
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:653-657`).
    #[test]
    fn the_window_is_the_counts_and_the_base_sits_inside_it() {
        let code = Code {
            variable_reserve_count: 2,
            ..Code::default()
        };
        assert_eq!(code.block_len(), Ok(3));
        assert_eq!(code.frame_base(), Ok(2));
        let frame = Frame::new(block_of(&code), &code, None, None, &[]).expect("a frame");
        assert_eq!(frame.base(), 2);
        assert_eq!(frame.len(), 3);
    }

    /// The receiver is `ra[-1]` and the objthis-context is `ra[-2]`, and both
    /// name the receiver when there is no global object
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:659-707`).
    #[test]
    fn the_receiver_and_its_context_are_the_two_registers_below_the_base() {
        let code = Code {
            variable_reserve_count: 2,
            ..Code::default()
        };
        let this = object();
        let frame =
            Frame::new(block_of(&code), &code, Some(this.clone()), None, &[]).expect("a frame");
        assert!(matches!(frame.reg(REG_THIS), Ok(Variant::Object(_))));
        assert!(matches!(frame.reg(REG_CONTEXT), Ok(Variant::Object(_))));
        let receiver = frame.receiver().expect("a receiver");
        assert!(
            Rc::ptr_eq(&receiver, &this),
            "ra[-1] is the receiver itself"
        );
        let context = frame.context().expect("a context");
        assert!(
            !Rc::ptr_eq(&context, &this),
            "ra[-2] is a view of the receiver, not the receiver"
        );
        // %-3 is past the reserved pair on a context with no local, so it names
        // a register no caller filled: it reads as the absent value, and reading
        // it is not an error
        assert!(matches!(
            frame.reg(address_of_argument(0).expect("an address")),
            Ok(Variant::Void)
        ));
    }

    /// The discard register is `ra[0]` and it is cleared
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:708`).
    #[test]
    fn the_discard_register_is_the_base_itself() {
        let code = Code {
            variable_reserve_count: 2,
            max_frame_count: 2,
            ..Code::default()
        };
        let frame = Frame::new(block_of(&code), &code, None, None, &[]).expect("a frame");
        assert!(matches!(frame.reg(0), Ok(Variant::Void)));
        // the window is the base and the temporaries above it: two registers of
        // reserve, the discard register and two temporaries - so a third
        // temporary is past the end
        assert!(matches!(
            frame.reg(60),
            Err(Fault::RegisterOutOfWindow { address: 60 })
        ));
    }

    /// The declared arguments go into `ra[-3]` downwards, the ones the caller did
    /// not pass are empty, and the surplus ones are dropped when the signature
    /// declares none to collapse them into
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:711-731`).
    #[test]
    fn the_declared_arguments_are_copied_downwards() {
        let code = Code {
            variable_reserve_count: 2,
            max_variable_count: 2,
            max_frame_count: 1,
            func_decl_arg_count: 2,
            ..Code::default()
        };
        let argv = [Variant::Integer(7)];
        let frame = Frame::new(block_of(&code), &code, None, None, &argv).expect("a frame");
        assert!(matches!(
            frame.reg(address_of_argument(0).expect("an address")),
            Ok(Variant::Integer(7))
        ));
        assert!(
            matches!(
                frame.reg(address_of_argument(1).expect("an address")),
                Ok(Variant::Void)
            ),
            "an argument the caller did not pass is empty"
        );
        assert!(
            matches!(
                frame.reg(address_of_argument(2).expect("an address")),
                Ok(Variant::Void)
            ),
            "an argument below the window is the caller's, and no caller filled it"
        );
    }

    /// The named collapse argument receives an array of the surplus arguments
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:734-750`).
    #[test]
    fn the_collapse_argument_receives_the_surplus() {
        let code = Code {
            variable_reserve_count: 2,
            max_variable_count: 2,
            max_frame_count: 1,
            func_decl_arg_count: 1,
            func_decl_collapse_base: 1,
            ..Code::default()
        };
        let argv = [
            Variant::Integer(1),
            Variant::Integer(2),
            Variant::Integer(3),
        ];
        let frame = Frame::new(block_of(&code), &code, None, None, &argv).expect("a frame");
        let collapse = frame
            .reg(address_of_argument(1).expect("an address"))
            .expect("the collapse register");
        let Variant::Object(closure) = collapse else {
            panic!("the collapse argument is not an object");
        };
        let array = closure.object.as_ref().expect("an array");
        let length = array
            .prop_get(&TjsString::from("length"), MemberFlags::NONE, None)
            .expect("the array answers its length");
        assert!(
            matches!(length, Variant::Integer(2)),
            "the two surplus arguments"
        );
        let second = array
            .prop_get_by_num(1, MemberFlags::NONE, None)
            .expect("the second element");
        assert!(matches!(second, Variant::Integer(3)));
    }

    /// The block allocator grows one page and then reuses it, and a block at
    /// least as large as a page is not pooled
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:566-610`).
    #[test]
    fn the_block_allocator_grows_once_and_reuses() {
        let mut stack = BlockStack::new();
        assert_eq!(stack.pages(), 0);
        let first = stack.allocate(8);
        assert_eq!(first.len(), 8);
        assert_eq!(stack.made(), 1);
        stack.release(first);
        assert_eq!(stack.pages(), 1);
        let second = stack.allocate(8);
        assert_eq!(second.len(), 8);
        assert_eq!(stack.made(), 1, "the second call reused the page");
        stack.release(second);
        let big = stack.allocate(PAGE);
        assert_eq!(big.len(), PAGE);
        assert_eq!(stack.made(), 1, "a large block is not pooled");
        stack.release(big);
        assert_eq!(stack.pages(), 1, "and it is not kept");
    }

    /// A released block drops the values it held, so a page cannot keep an object
    /// alive.
    #[test]
    fn a_released_block_drops_its_values() {
        let mut stack = BlockStack::new();
        let block = stack.allocate(3);
        stack.release(block);
        let reused = stack.allocate(3);
        for value in &reused {
            assert!(matches!(value, Variant::Void));
        }
    }

    /// A register address outside the window is refused, never read.
    #[test]
    fn the_low_registers_are_absent_and_the_high_ones_are_refused() {
        let code = Code {
            variable_reserve_count: 2,
            max_frame_count: 0,
            ..Code::default()
        };
        let mut frame = Frame::new(block_of(&code), &code, None, None, &[]).expect("a frame");
        assert!(matches!(
            frame.reg(20),
            Err(Fault::RegisterOutOfWindow { address: 20 })
        ));
        assert!(
            matches!(frame.reg(-60), Ok(Variant::Void)),
            "below the base is the caller's, and no caller filled it"
        );
        assert!(matches!(frame.reg(3), Err(Fault::MisalignedRegister(3))));
        assert!(matches!(
            frame.reg_mut(-60),
            Err(Fault::RegisterOutOfWindow { address: -60 })
        ));
    }

    /// The consecutive clear empties the run it names
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1388-1396`).
    #[test]
    fn the_consecutive_clear_empties_a_run() {
        let code = Code {
            variable_reserve_count: 2,
            max_frame_count: 3,
            ..Code::default()
        };
        let mut frame = Frame::new(block_of(&code), &code, None, None, &[]).expect("a frame");
        *frame.reg_mut(0).expect("a register") = Variant::Integer(1);
        *frame.reg_mut(20).expect("a register") = Variant::Integer(2);
        *frame.reg_mut(40).expect("a register") = Variant::Integer(3);
        frame.clear_run(0, 3).expect("the run is inside the window");
        assert!(matches!(frame.reg(0), Ok(Variant::Void)));
        assert!(matches!(frame.reg(20), Ok(Variant::Void)));
        assert!(matches!(frame.reg(40), Ok(Variant::Void)));
        assert!(frame.clear_run(0, 9).is_err(), "the run leaves the window");
    }

    /// The objthis-context falls back to the global object on a member the
    /// receiver does not hold, and on nothing else
    /// (`crates/tjs/cpp/tjs2/tjsInterCodeExec.cpp:264-271`).
    #[test]
    fn the_context_falls_back_only_for_a_missing_member() {
        let this = Rc::new(CustomObject::new(Vec::new())) as Rc<dyn Dispatch>;
        let global = Rc::new(CustomObject::new(Vec::new())) as Rc<dyn Dispatch>;
        global
            .prop_set(
                &TjsString::from("g"),
                MemberFlags::ENSURE,
                &Variant::Integer(9),
                None,
            )
            .expect("the global member is stored");
        let context = ObjThisContext::new(this.clone(), Some(global.clone()));
        let found = context
            .prop_get(&TjsString::from("g"), MemberFlags::NONE, None)
            .expect("the fallback answers");
        assert!(matches!(found, Variant::Integer(9)));
        let missing = context.prop_get(&TjsString::from("nope"), MemberFlags::NONE, None);
        assert!(matches!(missing, Err(ObjectError::MemberNotFound)));
    }

    /// A name added to the view is added to the object the view stands in front
    /// of (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:237-238`,
    /// `OBJ1`).
    #[test]
    fn the_context_forwards_a_class_name_to_its_receiver() {
        let primary = Rc::new(CustomObject::new(Vec::new()));
        let handle: Rc<dyn Dispatch> = primary.clone();
        let context = ObjThisContext::new(handle, None);
        context
            .add_class_name(&TjsString::from("Added"))
            .expect("the name is added");
        assert_eq!(primary.class_names(), vec![TjsString::from("Added")]);
    }

    /// The context does not look twice in one object.
    #[test]
    fn the_context_does_not_fall_back_into_itself() {
        let this = Rc::new(CustomObject::new(Vec::new())) as Rc<dyn Dispatch>;
        let context = ObjThisContext::new(this.clone(), Some(this));
        assert!(context.second().is_none());
        assert!(matches!(
            context.prop_get(&TjsString::from("nope"), MemberFlags::NONE, None),
            Err(ObjectError::MemberNotFound)
        ));
    }

    /// A member walk of the view is not served
    /// (`crates/tjs/cpp/tjs2/tjsInterCodeExec.cpp:328-331`).
    #[test]
    fn the_context_does_not_enumerate() {
        let this = Rc::new(CustomObject::new(Vec::new())) as Rc<dyn Dispatch>;
        let context = ObjThisContext::new(this, None);
        let answer = context.enum_members(MemberFlags::NONE, &mut |_, _, _| true);
        assert_eq!(answer, Err(ObjectError::CallNotServed));
    }
}
