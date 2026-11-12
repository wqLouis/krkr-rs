//! The C interface of the code generator.
//!
//! There are two interfaces here, and they are two faces of one
//! implementation:
//!
//! * the **script-block** interface, which is the shipped one: it hands
//!   the native executor the image of a script block — the code stream
//!   of every context, the constant area, the variable area, the
//!   function-argument area, the labels and the line table;
//! * the **node-building** interface, which is the **proof instrument**:
//!   it lets the native grammar drive the Rust generator so that the two
//!   code streams can be compared word for word. It is not the shipped
//!   interface, and its marshalling cost is not a shipping number.
//!
//! Every entry point catches a panic and reports it as an error value —
//! a negative code, plus the message, which the caller reads with
//! `tjs2_codegen_last_error_message`. No entry point unwinds into the
//! native caller.
//!
//! The interface is safe to call from several threads as long as one handle
//! is used by one thread at a time.

#![allow(unsafe_code)]

use std::ffi::{CString, c_char, c_int};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use crate::vm::codegen::Generator;
use crate::vm::codegen::context::{ContextType, FuncArgKind};
use crate::vm::codegen::node::Node;
use crate::vm::codegen::value::{ObjectId, Value};

/// The generator succeeded.
/// The `TJS2_CG_OK` value of the interface.
pub const TJS2_CG_OK: c_int = 0;

/// The tree has a shape the generator does not support.
/// The `TJS2_CG_ERROR_SYNTAX` value of the interface.
pub const TJS2_CG_ERROR_SYNTAX: c_int = -1;

/// The generator panicked. The message carries the panic's text.
/// The `TJS2_CG_ERROR_PANIC` value of the interface.
pub const TJS2_CG_ERROR_PANIC: c_int = -2;

/// A value is outside the range the interface can carry.
/// The `TJS2_CG_ERROR_RANGE` value of the interface.
pub const TJS2_CG_ERROR_RANGE: c_int = -3;

/// The handle is in a state the call does not fit.
/// The `TJS2_CG_ERROR_STATE` value of the interface.
pub const TJS2_CG_ERROR_STATE: c_int = -4;

/// The kinds of a code context.
/// The `TJS2_CG_CONTEXT_TOP_LEVEL` value of the interface.
pub const TJS2_CG_CONTEXT_TOP_LEVEL: c_int = 0;
/// The `TJS2_CG_CONTEXT_FUNCTION` value of the interface.
pub const TJS2_CG_CONTEXT_FUNCTION: c_int = 1;
/// The `TJS2_CG_CONTEXT_EXPR_FUNCTION` value of the interface.
pub const TJS2_CG_CONTEXT_EXPR_FUNCTION: c_int = 2;
/// The `TJS2_CG_CONTEXT_PROPERTY` value of the interface.
pub const TJS2_CG_CONTEXT_PROPERTY: c_int = 3;
/// The `TJS2_CG_CONTEXT_PROPERTY_SETTER` value of the interface.
pub const TJS2_CG_CONTEXT_PROPERTY_SETTER: c_int = 4;
/// The `TJS2_CG_CONTEXT_PROPERTY_GETTER` value of the interface.
pub const TJS2_CG_CONTEXT_PROPERTY_GETTER: c_int = 5;
/// The `TJS2_CG_CONTEXT_CLASS` value of the interface.
pub const TJS2_CG_CONTEXT_CLASS: c_int = 6;
/// The `TJS2_CG_CONTEXT_SUPER_CLASS_GETTER` value of the interface.
pub const TJS2_CG_CONTEXT_SUPER_CLASS_GETTER: c_int = 7;

/// The kinds of a constant.
/// The `TJS2_CG_VALUE_VOID` value of the interface.
pub const TJS2_CG_VALUE_VOID: c_int = 0;
/// The `TJS2_CG_VALUE_INTEGER` value of the interface.
pub const TJS2_CG_VALUE_INTEGER: c_int = 1;
/// The `TJS2_CG_VALUE_REAL` value of the interface.
pub const TJS2_CG_VALUE_REAL: c_int = 2;
/// The `TJS2_CG_VALUE_STRING` value of the interface.
pub const TJS2_CG_VALUE_STRING: c_int = 3;
/// The `TJS2_CG_VALUE_OCTET` value of the interface.
pub const TJS2_CG_VALUE_OCTET: c_int = 4;
/// The `TJS2_CG_VALUE_OBJECT` value of the interface.
pub const TJS2_CG_VALUE_OBJECT: c_int = 5;
/// The `TJS2_CG_VALUE_NULL` value of the interface.
pub const TJS2_CG_VALUE_NULL: c_int = 6;
/// The `TJS2_CG_VALUE_CONTEXT` value of the interface.
pub const TJS2_CG_VALUE_CONTEXT: c_int = 7;

/// The kinds of a function argument.
/// The `TJS2_CG_ARG_NORMAL` value of the interface.
pub const TJS2_CG_ARG_NORMAL: c_int = 0;
/// The `TJS2_CG_ARG_EXPAND` value of the interface.
pub const TJS2_CG_ARG_EXPAND: c_int = 1;
/// The `TJS2_CG_ARG_UNNAMED_EXPAND` value of the interface.
pub const TJS2_CG_ARG_UNNAMED_EXPAND: c_int = 2;

/// A value of the constant area.
///
/// The kind says which of the payload fields holds the value.
#[repr(C)]
pub struct tjs2_codegen_value {
    /// The kind of the value.
    pub kind: c_int,
    /// The value, when it is an integer.
    pub integer: i64,
    /// The value, when it is a real.
    pub real: f64,
    /// The value, when it is a string.
    pub string: *const u16,
    /// The length of the string, in code units.
    pub string_length: c_int,
    /// The value, when it is a byte string.
    pub octet: *const u8,
    /// The length of the byte string.
    pub octet_length: c_int,
    /// The identity of the value, when it is an object.
    pub object: u64,
}

/// A local variable of the variable area.
#[repr(C)]
pub struct tjs2_codegen_variable {
    /// The name of the variable.
    pub name: *const u16,
    /// The length of the name, in code units.
    pub name_length: c_int,
    /// The register the variable holds.
    pub register_: c_int,
}

/// One argument of the function-argument area.
#[repr(C)]
pub struct tjs2_codegen_func_arg {
    /// The kind of the argument.
    pub kind: c_int,
    /// The register the argument holds.
    pub register_: c_int,
    /// The name of the argument.
    pub name: *const u16,
    /// The length of the name, in code units.
    pub name_length: c_int,
}

/// A place in the script, and the place in the code it names.
#[repr(C)]
pub struct tjs2_codegen_source_pos {
    /// The place in the code stream.
    pub code_pos: i32,
    /// The place in the script.
    pub source_pos: i32,
}

/// One line of the script.
#[repr(C)]
pub struct tjs2_codegen_line {
    /// The place in the script the line starts at.
    pub position: i32,
    /// The length of the line, in code units.
    pub length: i32,
}

/// The image of one context.
/// The lines of a script.
///
/// The text is split on `\r` and `\n`, a `\r\n` counts as one break, and
/// every line contributes its start offset and its length in code units,
/// without the break.
pub(crate) fn line_table(units: &[u16]) -> Vec<tjs2_codegen_line> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index < units.len() {
        if units[index] == u16::from(b'\r') || units[index] == u16::from(b'\n') {
            lines.push(tjs2_codegen_line {
                position: start as c_int,
                length: (index - start) as c_int,
            });
            if units[index] == u16::from(b'\r')
                && index + 1 < units.len()
                && units[index + 1] == u16::from(b'\n')
            {
                index += 1;
            }
            index += 1;
            start = index;
        } else {
            index += 1;
        }
    }
    if index != start {
        lines.push(tjs2_codegen_line {
            position: start as c_int,
            length: (index - start) as c_int,
        });
    }
    lines
}

/// The image of one context.
///
/// Every pointer is owned by the generator and stays valid until the
/// generator is freed or the context is released; the caller must not
/// free them.
#[repr(C)]
pub struct tjs2_codegen_context {
    /// The code stream.
    pub code: *const i32,
    /// The number of words in the code stream.
    pub code_size: i32,
    /// The constant area.
    pub constants: *const tjs2_codegen_value,
    /// The number of values in the constant area.
    pub constant_count: i32,
    /// The variable area.
    pub variables: *const tjs2_codegen_variable,
    /// The number of variables in the variable area.
    pub variable_count: i32,
    /// The number of registers the context reserves for itself.
    pub variable_reserve_count: i32,
    /// The greatest number of frames the context needs.
    pub max_frame_count: i32,
    /// The function-argument area.
    pub func_args: *const tjs2_codegen_func_arg,
    /// The number of arguments in the function-argument area.
    pub func_arg_count: i32,
    /// The base of the unnamed argument array.
    pub func_unnamed_arg_base: i32,
    /// The base of the collapsed argument.
    pub func_collapse_base: i32,
    /// The labels: the places in the script the code names.
    pub labels: *const tjs2_codegen_source_pos,
    /// The number of labels.
    pub label_count: i32,
    /// The kind of the context.
    pub kind: c_int,
    /// The name of the context.
    pub name: *const u16,
    /// The length of the name, in code units.
    pub name_length: i32,
    /// The index of the parent context; -1 for none.
    pub parent: i32,
    /// The index of the property's setter context; -1 for none.
    pub prop_setter: i32,
    /// The index of the property's getter context; -1 for none.
    pub prop_getter: i32,
    /// The index of the super-class proxy context; -1 for none.
    pub super_class_getter: i32,
    /// The places in the super-class proxy's own code where the super
    /// class is evaluated.
    pub super_class_getter_pointer: *const i32,
    /// The number of those places.
    pub super_class_getter_pointer_count: i32,
    /// The members this context contributes to its parent object.
    pub properties: *const tjs2_codegen_property,
    /// The number of member registrations.
    pub property_count: i32,
}

/// One conditional-compilation value: a name and the whole number it
/// holds.
#[repr(C)]
pub struct tjs2_pp_value {
    /// The name of the value, in UTF-16.
    pub name: *const u16,
    /// The length of the name, in code units.
    pub name_length: c_int,
    /// The value the name holds.
    pub integer: i64,
}

/// The conditional-compilation values that go into a compile and come
/// back out of it.
#[repr(C)]
pub struct tjs2_pp_values {
    /// The values.
    pub values: *const tjs2_pp_value,
    /// The number of values.
    pub count: c_int,
}

/// One member registration: the name of a member and the context it
/// holds.
#[repr(C)]
pub struct tjs2_codegen_property {
    /// The name of the member.
    pub name: *const u16,
    /// The length of the name, in code units.
    pub name_length: i32,
    /// The index of the context the member holds.
    pub member: i32,
}

/// The image of a whole script block.
#[repr(C)]
pub struct tjs2_codegen_block {
    /// The contexts of the block.
    pub contexts: *const tjs2_codegen_context,
    /// The number of contexts.
    pub context_count: i32,
    /// The line table of the block.
    pub lines: *const tjs2_codegen_line,
    /// The number of lines.
    pub line_count: i32,
    /// The index of the top-level context; -1 when there is none.
    pub top_level: c_int,
}

/// The generator: it holds the contexts of one script block.
///
/// The name is the C name the header uses, so the two agree.
#[allow(non_camel_case_types)]
pub struct tjs2_codegen {
    /// The generator itself.
    pub generator: Generator,
    /// The nodes of the tree, for the proof instrument.
    pub nodes: Vec<Rc<Node>>,
    /// The last error's code.
    pub error: c_int,
    /// The last error's message.
    pub message: Option<CString>,
    /// The image of the script block.
    pub block: Option<tjs2_codegen_block>,
    /// The contexts of the image.
    pub image: Vec<tjs2_codegen_context>,
    /// The constants of the image.
    pub constants: Vec<tjs2_codegen_value>,
    /// The variables of the image.
    pub variables: Vec<tjs2_codegen_variable>,
    /// The arguments of the image.
    pub func_args: Vec<tjs2_codegen_func_arg>,
    /// The labels of the image.
    pub labels: Vec<tjs2_codegen_source_pos>,
    /// The lines of the image.
    pub lines: Vec<tjs2_codegen_line>,
    /// The strings the image points at.
    pub strings: Vec<Vec<u16>>,
    /// The byte strings the image points at.
    pub octets: Vec<Vec<u8>>,
    /// The names the image points at.
    pub names: Vec<Vec<u16>>,
    /// The member registrations of the image.
    pub properties: Vec<tjs2_codegen_property>,
    /// The conditional-compilation values the compile hands back.
    pub pp_values: Option<tjs2_pp_values>,
}

impl Default for tjs2_codegen {
    fn default() -> Self {
        Self {
            generator: Generator::new(),
            nodes: Vec::new(),
            error: TJS2_CG_OK,
            message: None,
            block: None,
            image: Vec::new(),
            constants: Vec::new(),
            variables: Vec::new(),
            func_args: Vec::new(),
            labels: Vec::new(),
            lines: Vec::new(),
            strings: Vec::new(),
            octets: Vec::new(),
            names: Vec::new(),
            properties: Vec::new(),
            pp_values: None,
        }
    }
}

/// Runs the given work, and turns a panic into an error value.
fn guard<T>(fallback: T, work: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(work)) {
        Ok(value) => value,
        Err(payload) => {
            let message = if let Some(text) = payload.downcast_ref::<&str>() {
                (*text).to_string()
            } else if let Some(text) = payload.downcast_ref::<String>() {
                text.clone()
            } else {
                "the generator panicked".to_string()
            };
            // the message is kept in a thread-local so that the caller
            // can read it after the fact
            PANIC_MESSAGE.with(|slot| *slot.borrow_mut() = Some(message));
            fallback
        }
    }
}

/// A C string cannot hold a zero byte, so the text is cut at the first
/// one: the reason a failure carries is at the beginning of its text, and
/// the cut is a transformation the caller can see.
pub(crate) fn c_string(text: &str) -> CString {
    let end = text.find('\0').unwrap_or(text.len());
    let mut bytes = text.as_bytes()[..end].to_vec();
    bytes.push(0);
    // SAFETY: the bytes hold no zero byte before the one just added, so
    // the buffer is a C string.
    unsafe { CString::from_vec_unchecked(bytes) }
}

thread_local! {
    pub(crate) static PANIC_MESSAGE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Makes a generator: an empty script block.
#[unsafe(no_mangle)]
pub extern "C" fn tjs2_codegen_new() -> *mut tjs2_codegen {
    guard(std::ptr::null_mut(), || {
        Box::into_raw(Box::new(tjs2_codegen::default()))
    })
}

/// Frees a generator and every image it holds.
///
/// # Safety
///
/// `generator` must be a handle from [`tjs2_codegen_new`] that has not
/// been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_free(generator: *mut tjs2_codegen) {
    if generator.is_null() {
        return;
    }
    // SAFETY: the caller promises the handle is live and not freed.
    drop(unsafe { Box::from_raw(generator) });
}

/// The last error of the generator: a negative code, or zero.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_last_error(generator: *const tjs2_codegen) -> c_int {
    // SAFETY: the caller promises the handle is live.
    unsafe { generator.as_ref() }
        .map(|generator| generator.error)
        .unwrap_or(TJS2_CG_ERROR_PANIC)
}

/// The message of the last error.
///
/// The returned pointer is owned by the generator and stays valid until
/// the next call on it.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_last_error_message(
    generator: *const tjs2_codegen,
) -> *const c_char {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_ref() }) else {
        return std::ptr::null();
    };
    if let Some(message) = generator.message.as_ref() {
        return message.as_ptr().cast::<c_char>();
    }
    PANIC_MESSAGE.with(|slot| match slot.borrow().as_ref() {
        Some(text) => c_string(text.as_str()).into_raw().cast::<c_char>(),
        None => std::ptr::null::<c_char>(),
    })
}

/// The image of the script block.
///
/// The returned pointer is owned by the generator and stays valid until
/// the generator is freed.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_block_image(
    generator: *const tjs2_codegen,
) -> *const tjs2_codegen_block {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_ref() }) else {
        return std::ptr::null();
    };
    generator
        .block
        .as_ref()
        .map(|block| block as *const tjs2_codegen_block)
        .unwrap_or(std::ptr::null())
}

/// The image of one context.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_context_image(
    generator: *const tjs2_codegen,
    context: c_int,
) -> *const tjs2_codegen_context {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_ref() }) else {
        return std::ptr::null();
    };
    generator
        .image
        .get(context as usize)
        .map(|image| image as *const tjs2_codegen_context)
        .unwrap_or(std::ptr::null())
}

/// Builds the image of the script block.
///
/// The image holds the code stream of every context, the constant area,
/// the variable area, the function-argument area, the labels and the
/// line table.
///
/// # Safety
///
/// `generator` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tjs2_codegen_build_image(generator: *mut tjs2_codegen) -> c_int {
    // SAFETY: the caller promises the handle is live.
    let Some(generator) = (unsafe { generator.as_mut() }) else {
        return TJS2_CG_ERROR_PANIC;
    };
    build_image(generator)
}

/// Builds the image of the script block.
///
/// The image holds the code stream of every context, the constant area,
/// the variable area, the function-argument area, the labels and the
/// line table.
pub(crate) fn build_image(generator: &mut tjs2_codegen) -> c_int {
    guard(TJS2_CG_ERROR_PANIC, || {
        // the areas are built first, so that the contexts can point at
        // them
        let mut constants: Vec<tjs2_codegen_value> = Vec::new();
        let mut variables: Vec<tjs2_codegen_variable> = Vec::new();
        let mut func_args: Vec<tjs2_codegen_func_arg> = Vec::new();
        let mut labels: Vec<tjs2_codegen_source_pos> = Vec::new();
        let mut strings: Vec<Vec<u16>> = Vec::new();
        // the place of each constant's string in the string area: a
        // constant that is not a string has none
        let mut string_places: Vec<Option<usize>> = Vec::new();
        // the place of each constant's byte string in the byte area
        let mut octet_places: Vec<Option<usize>> = Vec::new();
        let mut octets: Vec<Vec<u8>> = Vec::new();
        let mut names: Vec<Vec<u16>> = Vec::new();
        let mut context_constants: Vec<(usize, usize)> = Vec::new();
        let mut context_variables: Vec<(usize, usize)> = Vec::new();
        let mut context_args: Vec<(usize, usize)> = Vec::new();
        let mut context_labels: Vec<(usize, usize)> = Vec::new();

        for context in generator.generator.contexts() {
            let constant_base = constants.len();
            for value in context.data() {
                constants.push(build_value(
                    value,
                    &mut strings,
                    &mut octets,
                    &mut string_places,
                    &mut octet_places,
                ));
            }
            context_constants.push((constant_base, constants.len() - constant_base));

            let variable_base = variables.len();
            for (index, name) in context.variable_names().into_iter().enumerate() {
                let units: Vec<u16> = name.encode_utf16().collect();
                let length = units.len() as c_int;
                names.push(units);
                variables.push(tjs2_codegen_variable {
                    name: std::ptr::null(),
                    name_length: length,
                    register_: index as c_int,
                });
            }
            context_variables.push((variable_base, variables.len() - variable_base));

            let arg_base = func_args.len();
            for arg in context.func_arg_list() {
                func_args.push(tjs2_codegen_func_arg {
                    kind: match arg.kind {
                        FuncArgKind::Normal => TJS2_CG_ARG_NORMAL,
                        FuncArgKind::Expand => TJS2_CG_ARG_EXPAND,
                        FuncArgKind::UnnamedExpand => TJS2_CG_ARG_UNNAMED_EXPAND,
                    },
                    register_: arg.register,
                    name: std::ptr::null(),
                    name_length: 0,
                });
            }
            context_args.push((arg_base, func_args.len() - arg_base));

            let label_base = labels.len();
            for label in context.source_positions() {
                labels.push(tjs2_codegen_source_pos {
                    code_pos: label.code_pos,
                    source_pos: label.source_pos,
                });
            }
            context_labels.push((label_base, labels.len() - label_base));
        }

        // the member registrations of every context
        let mut properties: Vec<tjs2_codegen_property> = Vec::new();
        let mut property_places: Vec<(usize, usize)> = Vec::new();
        for context in generator.generator.contexts() {
            let base = properties.len();
            for property in context.properties() {
                let units: Vec<u16> = property.name.encode_utf16().collect();
                let length = units.len() as c_int;
                names.push(units);
                properties.push(tjs2_codegen_property {
                    name: std::ptr::null(),
                    name_length: length,
                    member: property.member as c_int,
                });
            }
            property_places.push((base, properties.len() - base));
        }

        // the pointers of the areas are set once the areas will not move
        // again
        for (index, value) in constants.iter_mut().enumerate() {
            if let Some(place) = string_places[index] {
                value.string = strings[place].as_ptr();
            }
            if let Some(place) = octet_places[index] {
                value.octet = octets[place].as_ptr();
            }
        }
        for (index, variable) in variables.iter_mut().enumerate() {
            variable.name = names[index].as_ptr();
        }
        let property_name_base = names.len() - properties.len();
        for (index, property) in properties.iter_mut().enumerate() {
            property.name = names[property_name_base + index].as_ptr();
        }

        let mut image = Vec::new();
        for (index, context) in generator.generator.contexts().iter().enumerate() {
            let (constant_base, constant_count) = context_constants[index];
            let (variable_base, variable_count) = context_variables[index];
            let (arg_base, arg_count) = context_args[index];
            let (label_base, label_count) = context_labels[index];
            let name = context.name.clone().unwrap_or_default();
            let name_units: Vec<u16> = name.encode_utf16().collect();
            let name_length = name_units.len() as c_int;
            names.push(name_units);
            image.push(tjs2_codegen_context {
                code: context.code().as_ptr(),
                code_size: context.code().len() as c_int,
                constants: constants.as_ptr().wrapping_add(constant_base),
                constant_count: constant_count as c_int,
                variables: variables.as_ptr().wrapping_add(variable_base),
                variable_count: variable_count as c_int,
                variable_reserve_count: context.kind.variable_reserve_count(),
                max_frame_count: context.max_frame_count,
                func_args: func_args.as_ptr().wrapping_add(arg_base),
                func_arg_count: arg_count as c_int,
                func_unnamed_arg_base: context.func_decl_unnamed_arg_array_base,
                func_collapse_base: context.func_decl_collapse_base,
                labels: labels.as_ptr().wrapping_add(label_base),
                label_count: label_count as c_int,
                kind: context_kind(context.kind),
                name: names
                    .last()
                    .map(|name| name.as_ptr())
                    .unwrap_or(std::ptr::null()),
                name_length,
                parent: context.parent.map(|parent| parent as c_int).unwrap_or(-1),
                prop_setter: context
                    .prop_setter
                    .map(|setter| setter as c_int)
                    .unwrap_or(-1),
                prop_getter: context
                    .prop_getter
                    .map(|getter| getter as c_int)
                    .unwrap_or(-1),
                super_class_getter: context
                    .super_class_getter
                    .map(|getter| getter as c_int)
                    .unwrap_or(-1),
                super_class_getter_pointer: context.super_class_getter_pointer.as_ptr(),
                super_class_getter_pointer_count: context.super_class_getter_pointer.len() as c_int,
                properties: properties.as_ptr().wrapping_add(property_places[index].0),
                property_count: property_places[index].1 as c_int,
            });
        }

        generator.image = image;
        generator.constants = constants;
        generator.variables = variables;
        generator.func_args = func_args;
        generator.labels = labels;
        generator.strings = strings;
        generator.octets = octets;
        generator.names = names;
        generator.properties = properties;

        let top_level = generator
            .generator
            .contexts()
            .iter()
            .position(|context| context.kind == ContextType::TopLevel)
            .map(|index| index as c_int)
            .unwrap_or(-1);
        let block = tjs2_codegen_block {
            contexts: generator.image.as_ptr(),
            context_count: generator.image.len() as c_int,
            lines: generator.lines.as_ptr(),
            line_count: generator.lines.len() as c_int,
            top_level,
        };
        generator.block = Some(block);
        generator.error = TJS2_CG_OK;
        TJS2_CG_OK
    })
}

/// The image of one value.
///
/// The value's string, when it has one, is in the string area; the
/// place of the string is remembered beside the value, so that a value
/// that is not a string does not shift the ones that are.
fn build_value(
    value: &Value,
    strings: &mut Vec<Vec<u16>>,
    octets: &mut Vec<Vec<u8>>,
    string_places: &mut Vec<Option<usize>>,
    octet_places: &mut Vec<Option<usize>>,
) -> tjs2_codegen_value {
    let mut image = tjs2_codegen_value {
        kind: TJS2_CG_VALUE_VOID,
        integer: 0,
        real: 0.0,
        string: std::ptr::null(),
        string_length: 0,
        octet: std::ptr::null(),
        octet_length: 0,
        object: 0,
    };
    match value {
        Value::Void => {}
        Value::Integer(number) => {
            image.kind = TJS2_CG_VALUE_INTEGER;
            image.integer = *number;
        }
        Value::Real(number) => {
            image.kind = TJS2_CG_VALUE_REAL;
            image.real = *number;
        }
        Value::String(text) => {
            let units: Vec<u16> = text.encode_utf16().collect();
            image.kind = TJS2_CG_VALUE_STRING;
            image.string_length = units.len() as c_int;
            string_places.push(Some(strings.len()));
            strings.push(units);
        }
        Value::Octet(bytes) => {
            image.kind = TJS2_CG_VALUE_OCTET;
            image.octet_length = bytes.len() as c_int;
            octets.push(bytes.clone());
            octet_places.push(Some(octets.len() - 1));
        }
        Value::Object(id) => {
            image.kind = TJS2_CG_VALUE_OBJECT;
            image.object = id.0;
        }
        Value::Context(index) => {
            // the context is named by its index in the block; the side
            // that has the object resolves the index
            image.kind = TJS2_CG_VALUE_CONTEXT;
            image.object = *index as u64;
        }
    }
    if image.kind != TJS2_CG_VALUE_STRING {
        string_places.push(None);
    }
    if image.kind != TJS2_CG_VALUE_OCTET {
        octet_places.push(None);
    }
    image
}

/// The number of a kind of context.
fn context_kind(kind: ContextType) -> c_int {
    match kind {
        ContextType::TopLevel => TJS2_CG_CONTEXT_TOP_LEVEL,
        ContextType::Function => TJS2_CG_CONTEXT_FUNCTION,
        ContextType::ExprFunction => TJS2_CG_CONTEXT_EXPR_FUNCTION,
        ContextType::Property => TJS2_CG_CONTEXT_PROPERTY,
        ContextType::PropertySetter => TJS2_CG_CONTEXT_PROPERTY_SETTER,
        ContextType::PropertyGetter => TJS2_CG_CONTEXT_PROPERTY_GETTER,
        ContextType::Class => TJS2_CG_CONTEXT_CLASS,
        ContextType::SuperClassGetter => TJS2_CG_CONTEXT_SUPER_CLASS_GETTER,
    }
}

/// The identity of an object, for the proof instrument.
#[unsafe(no_mangle)]
pub extern "C" fn tjs2_codegen_object_id(identity: u64) -> u64 {
    ObjectId(identity).0
}
