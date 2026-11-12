//! The image of a script block: its shape, and the builder that makes it.
//!
//! The **image** is the Rust side's description of a compiled script
//! block: the code stream of every context, the constant area, the
//! labels and the line table. The records below are its shape: every
//! field is a plain C type, and every pointer is owned by the image.
//!
//! The C interface the tree ships is the compile entry, at the module
//! root (`crate::vm`): it compiles script text and hands the image back.
//! This module holds the image the entry hands back — the records, and
//! the one builder that makes it.
//!
//! # The unsafe code
//!
//! The module allows `unsafe` because a C string is cut at its first
//! zero byte, and the cut is the transformation the caller can see.

#![allow(unsafe_code)]

use std::ffi::{CString, c_int};
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::vm::codegen::Generator;
use crate::vm::codegen::context::ContextType;
use crate::vm::codegen::value::Value;

/// The image's number for the top-level context.
pub const TJS2_CG_CONTEXT_TOP_LEVEL: c_int = 0;
/// The image's number for a function's context.
pub const TJS2_CG_CONTEXT_FUNCTION: c_int = 1;
/// The image's number for a function expression's context.
pub const TJS2_CG_CONTEXT_EXPR_FUNCTION: c_int = 2;
/// The image's number for a property's context.
pub const TJS2_CG_CONTEXT_PROPERTY: c_int = 3;
/// The image's number for a property setter's context.
pub const TJS2_CG_CONTEXT_PROPERTY_SETTER: c_int = 4;
/// The image's number for a property getter's context.
pub const TJS2_CG_CONTEXT_PROPERTY_GETTER: c_int = 5;
/// The image's number for a class's context.
pub const TJS2_CG_CONTEXT_CLASS: c_int = 6;
/// The image's number for a class's super-class proxy context.
pub const TJS2_CG_CONTEXT_SUPER_CLASS_GETTER: c_int = 7;

/// The image's number for the void constant.
pub const TJS2_CG_VALUE_VOID: c_int = 0;
/// The image's number for the integer constant.
pub const TJS2_CG_VALUE_INTEGER: c_int = 1;
/// The image's number for the real constant.
pub const TJS2_CG_VALUE_REAL: c_int = 2;
/// The image's number for the string constant.
pub const TJS2_CG_VALUE_STRING: c_int = 3;
/// The image's number for the byte-string constant.
pub const TJS2_CG_VALUE_OCTET: c_int = 4;
/// The image's number for the object constant.
pub const TJS2_CG_VALUE_OBJECT: c_int = 5;
/// The image's number for the null constant.
pub const TJS2_CG_VALUE_NULL: c_int = 6;
/// The image's number for the context constant.
pub const TJS2_CG_VALUE_CONTEXT: c_int = 7;
/// The image's number for the constant inline array.
///
/// A constant inline array is the object the `(const)[...]` form builds
/// while it is parsed, one element at a time. The generator cannot build
/// a live object, so the image carries the elements and the side that has
/// the object builds it.
pub const TJS2_CG_VALUE_ARRAY: c_int = 8;
/// The image's number for the constant inline dictionary.
///
/// A constant inline dictionary is the object the `(const)%[...]` form
/// builds while it is parsed, one member at a time.
pub const TJS2_CG_VALUE_DICTIONARY: c_int = 9;

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

/// One constant object of the block: a constant inline array or a constant
/// inline dictionary.
///
/// The grammar builds the object while it parses it; the Rust side cannot
/// build a live object, so the image carries the kind, the elements and
/// their number, and the native side builds the object.
#[repr(C)]
pub struct tjs2_codegen_object {
    /// The values of the elements.
    pub values: *const tjs2_codegen_value,
    /// The number of elements.
    pub element_count: i32,
    /// The keys of the elements: the value the member name is made of. It
    /// is null for the elements of an array.
    pub keys: *const tjs2_codegen_value,
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
    /// The number of local variables the context's name space held.
    pub variable_count: i32,
    /// The number of registers the context reserves for itself.
    pub variable_reserve_count: i32,
    /// The greatest number of frames the context needs.
    pub max_frame_count: i32,
    /// The number of arguments the context declares.
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
    /// The constant objects of the block: the constant inline arrays and
    /// the constant inline dictionaries the constant areas hold.
    pub objects: *const tjs2_codegen_object,
    /// The number of constant objects.
    pub object_count: i32,
}

/// The image of a script block, and the generator that owns its code.
///
/// The image holds the areas its records point at: the contexts, the
/// constants, the labels, the lines, the strings, the octets, the names,
/// the constant objects and the member registrations. Every pointer in the
/// image points into one of them, so they live as long as the image does.
pub struct Image {
    /// The generator, which owns the code the image points at.
    pub generator: Generator,
    /// The image of the script block.
    pub block: Option<tjs2_codegen_block>,
    /// The contexts of the image.
    pub contexts: Vec<tjs2_codegen_context>,
    /// The constants of the image.
    pub constants: Vec<tjs2_codegen_value>,
    /// The labels of the image.
    pub labels: Vec<tjs2_codegen_source_pos>,
    /// The lines of the image.
    pub lines: Vec<tjs2_codegen_line>,
    /// The strings the image points at.
    pub strings: Vec<Box<[u16]>>,
    /// The byte strings the image points at.
    pub octets: Vec<Box<[u8]>>,
    /// The names the image points at.
    pub names: Vec<Box<[u16]>>,
    /// The constant objects of the image.
    pub objects: Vec<tjs2_codegen_object>,
    /// The values of the constant objects' elements.
    pub object_values: Vec<Box<[tjs2_codegen_value]>>,
    /// The keys of the constant objects' elements.
    pub object_keys: Vec<Box<[tjs2_codegen_value]>>,
    /// The member registrations of the image.
    pub properties: Vec<tjs2_codegen_property>,
}

impl Default for Image {
    fn default() -> Self {
        Self {
            generator: Generator::new(),
            block: None,
            contexts: Vec::new(),
            constants: Vec::new(),
            labels: Vec::new(),
            lines: Vec::new(),
            strings: Vec::new(),
            octets: Vec::new(),
            names: Vec::new(),
            objects: Vec::new(),
            object_values: Vec::new(),
            object_keys: Vec::new(),
            properties: Vec::new(),
        }
    }
}

/// The text of a panic.
pub(crate) fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "the front end panicked".to_string()
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

/// Builds the image of the script block.
///
/// The image holds the code stream of every context, the constant area,
/// the labels and the line table.
///
/// A panic in the builder is caught, and the panic's text is the error.
pub(crate) fn build_image(image: &mut Image) -> Result<(), String> {
    match catch_unwind(AssertUnwindSafe(|| {
        // the areas are built first, so that the contexts can point at
        // them
        let mut constants: Vec<tjs2_codegen_value> = Vec::new();
        let mut labels: Vec<tjs2_codegen_source_pos> = Vec::new();
        let mut strings: Vec<Box<[u16]>> = Vec::new();
        let mut octets: Vec<Box<[u8]>> = Vec::new();
        let mut names: Vec<Box<[u16]>> = Vec::new();
        // the elements of the constant objects: one area per object, and
        // the area does not move when the others grow
        let mut objects: Vec<tjs2_codegen_object> = Vec::new();
        let mut object_values: Vec<Box<[tjs2_codegen_value]>> = Vec::new();
        let mut object_keys: Vec<Box<[tjs2_codegen_value]>> = Vec::new();
        let mut context_constants: Vec<(usize, usize)> = Vec::new();
        let mut context_labels: Vec<(usize, usize)> = Vec::new();

        // the contexts that have left the block are not in the image, and
        // the contexts after them move down
        let mut places: Vec<Option<usize>> = Vec::with_capacity(image.generator.contexts().len());
        let mut live = 0;
        for context in image.generator.contexts() {
            if context.removed {
                places.push(None);
            } else {
                places.push(Some(live));
                live += 1;
            }
        }

        for context in image.generator.contexts() {
            if context.removed {
                continue;
            }
            let constant_base = constants.len();
            for value in context.data() {
                constants.push(build_value(
                    value,
                    &places,
                    &mut strings,
                    &mut octets,
                    &mut objects,
                    &mut object_values,
                    &mut object_keys,
                ));
            }
            context_constants.push((constant_base, constants.len() - constant_base));

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
        for context in image.generator.contexts() {
            if context.removed {
                continue;
            }
            let base = properties.len();
            for property in context.properties() {
                let units: Vec<u16> = property.name.encode_utf16().collect();
                let length = units.len() as c_int;
                names.push(units.into_boxed_slice());
                properties.push(tjs2_codegen_property {
                    name: std::ptr::null(),
                    name_length: length,
                    member: places[property.member].unwrap_or(0) as c_int,
                });
            }
            property_places.push((base, properties.len() - base));
        }

        // the pointers of the areas that the values hold are set when the
        // values are made; the names of the members are set once the
        // names area holds them all
        for (index, property) in properties.iter_mut().enumerate() {
            property.name = names[index].as_ptr();
        }

        let mut contexts = Vec::new();
        for (index, context) in image.generator.contexts().iter().enumerate() {
            if context.removed {
                continue;
            }
            let (constant_base, constant_count) = context_constants[index];
            let (label_base, label_count) = context_labels[index];
            let name = context.name.clone().unwrap_or_default();
            let name_units: Vec<u16> = name.encode_utf16().collect();
            let name_length = name_units.len() as c_int;
            names.push(name_units.into_boxed_slice());
            contexts.push(tjs2_codegen_context {
                code: context.code().as_ptr(),
                code_size: context.code().len() as c_int,
                constants: constants.as_ptr().wrapping_add(constant_base),
                constant_count: constant_count as c_int,
                variable_count: context.max_variable_count,
                variable_reserve_count: context.kind.variable_reserve_count(),
                max_frame_count: context.max_frame_count,
                func_arg_count: context.func_decl_arg_count,
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
                parent: context
                    .parent
                    .and_then(|parent| places[parent])
                    .map(|parent| parent as c_int)
                    .unwrap_or(-1),
                prop_setter: context
                    .prop_setter
                    .and_then(|setter| places[setter])
                    .map(|setter| setter as c_int)
                    .unwrap_or(-1),
                prop_getter: context
                    .prop_getter
                    .and_then(|getter| places[getter])
                    .map(|getter| getter as c_int)
                    .unwrap_or(-1),
                super_class_getter: context
                    .super_class_getter
                    .and_then(|getter| places[getter])
                    .map(|getter| getter as c_int)
                    .unwrap_or(-1),
                super_class_getter_pointer: context.super_class_getter_pointer.as_ptr(),
                super_class_getter_pointer_count: context.super_class_getter_pointer.len() as c_int,
                properties: properties.as_ptr().wrapping_add(property_places[index].0),
                property_count: property_places[index].1 as c_int,
            });
        }

        image.contexts = contexts;
        image.constants = constants;
        image.labels = labels;
        image.strings = strings;
        image.octets = octets;
        image.names = names;
        image.objects = objects;
        image.object_values = object_values;
        image.object_keys = object_keys;
        image.properties = properties;

        let top_level = image
            .generator
            .contexts()
            .iter()
            .position(|context| context.kind == ContextType::TopLevel)
            .map(|index| index as c_int)
            .unwrap_or(-1);
        let block = tjs2_codegen_block {
            contexts: image.contexts.as_ptr(),
            context_count: image.contexts.len() as c_int,
            lines: image.lines.as_ptr(),
            line_count: image.lines.len() as c_int,
            top_level,
            // the constant objects: the constant inline arrays and the
            // constant inline dictionaries the constant areas hold
            objects: image.objects.as_ptr(),
            object_count: image.objects.len() as c_int,
        };
        image.block = Some(block);
    })) {
        Ok(()) => Ok(()),
        Err(payload) => Err(panic_message(payload)),
    }
}

/// The image of one value.
///
/// The value's string, when it has one, is in the string area; the
/// place of the string is remembered beside the value, so that a value
/// that is not a string does not shift the ones that are.
fn build_value(
    value: &Value,
    places: &[Option<usize>],
    strings: &mut Vec<Box<[u16]>>,
    octets: &mut Vec<Box<[u8]>>,
    objects: &mut Vec<tjs2_codegen_object>,
    object_values: &mut Vec<Box<[tjs2_codegen_value]>>,
    object_keys: &mut Vec<Box<[tjs2_codegen_value]>>,
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
            let units = units.into_boxed_slice();
            image.string = units.as_ptr();
            strings.push(units);
        }
        Value::Octet(bytes) => {
            image.kind = TJS2_CG_VALUE_OCTET;
            image.octet_length = bytes.len() as c_int;
            let bytes = bytes.clone().into_boxed_slice();
            image.octet = bytes.as_ptr();
            octets.push(bytes);
        }
        Value::Object(id) => {
            image.kind = TJS2_CG_VALUE_OBJECT;
            image.object = id.0;
        }
        Value::Context(index) => {
            // the context is named by its index in the block; the side
            // that has the object resolves the index
            image.kind = TJS2_CG_VALUE_CONTEXT;
            image.object = places
                .get(*index)
                .and_then(|place| *place)
                .unwrap_or(*index) as u64;
        }
        Value::ConstArray {
            elements: items, ..
        } => {
            // the array the `(const)` form builds at compile time; the side
            // that has the object builds it from the elements
            image.kind = TJS2_CG_VALUE_ARRAY;
            let mut values: Vec<tjs2_codegen_value> = Vec::new();
            for item in items {
                values.push(build_value(
                    item,
                    places,
                    strings,
                    octets,
                    objects,
                    object_values,
                    object_keys,
                ));
            }
            let count = values.len() as c_int;
            let values = values.into_boxed_slice();
            let pointer = values.as_ptr();
            object_values.push(values);
            image.object = objects.len() as u64;
            objects.push(tjs2_codegen_object {
                values: pointer,
                element_count: count,
                keys: std::ptr::null(),
            });
        }
        Value::ConstDictionary { entries, .. } => {
            // the dictionary the `(const)` form builds at compile time; the
            // side that has the object builds it from the entries
            image.kind = TJS2_CG_VALUE_DICTIONARY;
            let mut values: Vec<tjs2_codegen_value> = Vec::new();
            let mut keys: Vec<tjs2_codegen_value> = Vec::new();
            for (key, item) in entries {
                keys.push(build_value(
                    key,
                    places,
                    strings,
                    octets,
                    objects,
                    object_values,
                    object_keys,
                ));
                values.push(build_value(
                    item,
                    places,
                    strings,
                    octets,
                    objects,
                    object_values,
                    object_keys,
                ));
            }
            let count = values.len() as c_int;
            let values = values.into_boxed_slice();
            let value_pointer = values.as_ptr();
            object_values.push(values);
            let keys = keys.into_boxed_slice();
            let key_pointer = keys.as_ptr();
            object_keys.push(keys);
            image.object = objects.len() as u64;
            objects.push(tjs2_codegen_object {
                values: value_pointer,
                element_count: count,
                keys: key_pointer,
            });
        }
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
