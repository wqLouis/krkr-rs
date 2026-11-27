//! The seam: the front end's image becomes the block the executor runs.
//!
//! The compile entry hands back an [`Image`]: the code stream of every
//! context, its constant area, its labels, and the block's line table. What
//! the executor takes is a [`Block`]: one [`Code`] per context, one
//! [`ContextObject`] per context, and a constant area resolved into live
//! values. This module is that translation. It runs no instruction and reads
//! no script text.
//!
//! # The areas read through the generator
//!
//! An image record holds a raw pointer for the text of a constant, for a byte
//! string, for a context's name and for a member's name. This module is safe
//! Rust, so it reads those through the generator's own values — the same
//! values in the same order — and checks the two against each other on every
//! count. A record that disagrees with the context it names is a failure, not
//! a silently different block.
//!
//! # The order the contexts are built in
//!
//! A context's constant area can name another context: that is the value a
//! declared function is. A value holds the object itself, so the named
//! context must exist first, and the contexts are built from the last to the
//! first. A constant that names its own context or a later one has no object
//! to hold and fails the build.
//!
//! # One value, one object
//!
//! A constant array and a constant dictionary become one object each: the
//! array's elements are appended in order and the dictionary's members are
//! written in order, which is the order the image carries them in. A
//! context's constant is that context's object, so a function value and the
//! member registered for it are one object and not two.
//!
//! # The parent link is made last
//!
//! A context is built before the context that holds it, so its object cannot
//! be given its parent when it is made: the link is set once every object
//! exists, and it is a weak link, because a parent's members hold its children.
//!
//! # What the object does not hold
//!
//! A context's property setter and getter, its super-class proxy, the places
//! the super class is evaluated at, and the labels belong to the context and
//! not to its object: [`ContextObject`] has no entry for them. The block holds
//! them, so that they cross the seam with the code: [`Relations`] answers the
//! relations, [`ContextRecord::super_class_code`] the places, and
//! [`Block::source_position`] the place in the script a code position came
//! from.

use std::rc::Rc;

use crate::vm::codegen::context::{Context as GeneratedContext, SourcePos};
use crate::vm::codegen::ffi::{Image, build_image, line_table, tjs2_codegen_context};
use crate::vm::codegen::value::Value;
use crate::vm::runtime::array::{ArrayError, ArrayObject};
use crate::vm::runtime::dictionary::Dictionary;
use crate::vm::runtime::object::{Dispatch, MemberFlags, ObjectError};
use crate::vm::runtime::octet::Octet;
use crate::vm::runtime::string::TjsString;
use crate::vm::runtime::variant::{Closure, Variant, VariantError};
use crate::vm::{Generator, Lexer, Parser};

use super::context::ContextObject;
use super::frame::Code;

/// A compiled script block: every context of the image, the relations a
/// context has, and the line table.
///
/// The block owns everything it answers. The [`Image`] it was built from is
/// not held and may be freed: a value's text is copied, not pointed at.
pub struct Block {
    /// The contexts, in the image's order.
    contexts: Vec<ContextRecord>,
    /// The index of the top-level context, when the block has one.
    top_level: Option<usize>,
    /// The lines of the script the block was compiled from.
    lines: Vec<Line>,
}

/// One context of a block: the object a member read reaches, and the record
/// the object itself does not hold.
pub struct ContextRecord {
    /// The object the context is.
    object: Rc<ContextObject>,
    /// The contexts this one is related to.
    relations: Relations,
    /// The places in the super-class proxy's own code where the super class is
    /// evaluated.
    super_class_code: Vec<i32>,
    /// The place in the script each code position came from, sorted by code
    /// position.
    source_positions: Vec<SourcePos>,
}

/// The contexts a context is related to.
///
/// Every relation is an index into [`Block::contexts`], and every one of them
/// is the image's: a relation the image names with `-1` is `None` here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Relations {
    /// The context that holds this one.
    pub parent: Option<usize>,
    /// The context of the property's setter.
    pub setter: Option<usize>,
    /// The context of the property's getter.
    pub getter: Option<usize>,
    /// The context of the super-class proxy.
    pub super_class: Option<usize>,
}

/// One line of the script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Line {
    /// The offset of the line's first code unit in the script.
    pub position: i32,
    /// The length of the line, in code units, without its break.
    pub length: i32,
}

/// The reason a script could not become a block.
///
/// A script that does not compile is the front end's own report, and the two
/// failures after it are the ones a compile that finished can hit: an image
/// that did not build, and an image that does not describe a block.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CompileError {
    /// The script does not compile.
    #[error("the script does not compile: {message}")]
    Syntax {
        /// The front end's message for the first failure.
        message: TjsString,
    },

    /// The image of a compile that did not finish cannot be built.
    #[error("the image of the script cannot be built: {0}")]
    Image(String),

    /// The image does not describe a block.
    #[error(transparent)]
    Build(BuildError),
}

/// Compiles script text into the block the executor runs.
///
/// The front end and the seam in one step: the analyser, the parser and the
/// generator make the image, and [`Block::build`] makes the block from it. A
/// script that does not compile answers the failure the parser reported, so no
/// block is ever built from a failed compile.
///
/// `expression` reads the text as an expression instead of a statement script,
/// and `result_needed` makes it keep the value the expression produced.
pub fn compile(
    text: &TjsString,
    expression: bool,
    result_needed: bool,
) -> Result<Block, CompileError> {
    let units = text.as_utf16();
    let mut lexer = if expression {
        Lexer::expression(units, result_needed)
    } else {
        Lexer::new(units)
    };
    let mut generator = Generator::new();
    let parsed = Parser::new(&mut lexer, &mut generator).parse();
    if let Some(error) = parsed.first_error {
        return Err(CompileError::Syntax {
            message: TjsString::from(error.message()),
        });
    }
    let mut image = Image {
        generator,
        lines: line_table(units),
        ..Image::default()
    };
    build_image(&mut image).map_err(CompileError::Image)?;
    Block::build(&image).map_err(CompileError::Build)
}

/// The reason an image could not become a block.
///
/// Every branch says which record disagreed with which: an image that does not
/// describe itself is refused rather than built with a guess.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    /// The image holds no block, which is the image of a compile that failed
    /// before the block was made.
    #[error("the image holds no block")]
    NoBlock,

    /// A count of the image is negative.
    #[error("the image's {what} is negative")]
    NegativeCount {
        /// The count that is negative.
        what: &'static str,
    },

    /// Two areas of the image disagree on a count.
    #[error("the image's {what} does not agree with the area it describes")]
    Mismatch {
        /// The area that disagrees.
        what: &'static str,
    },

    /// The image's context count and the contexts the generator holds differ.
    #[error("the image holds {image} contexts and the generator holds {generator}")]
    ContextCount {
        /// The number the image's block record holds.
        image: usize,
        /// The number of contexts the generator holds.
        generator: usize,
    },

    /// A context record and the context it names disagree.
    #[error("context {context}'s {what} does not agree with the context")]
    ContextMismatch {
        /// The context whose record disagrees.
        context: usize,
        /// The record that disagrees.
        what: &'static str,
    },

    /// A context of the image has no object, which is a defect in the build.
    #[error("context {context} has no object")]
    MissingContext {
        /// The context that has no object.
        context: usize,
    },

    /// A constant names a context the image does not hold.
    #[error("context {context} names the context {named}, which does not exist")]
    UnknownContext {
        /// The context whose constant names it.
        context: usize,
        /// The context the constant names.
        named: usize,
    },

    /// A constant names a context that is not built yet, so there is no object
    /// to hold: a constant can only name a context its own context made.
    #[error("context {context} names the context {named} before it is built")]
    ContextNotBuilt {
        /// The context whose constant names it.
        context: usize,
        /// The context the constant names.
        named: usize,
    },

    /// A constant names an object the image cannot describe, which only the
    /// null object may be.
    #[error("context {context} holds an object the image cannot build")]
    ObjectConstant {
        /// The context whose constant names the object.
        context: usize,
    },

    /// The name of a dictionary's member has no text form.
    #[error("a member name of context {context} has no text")]
    Name {
        /// The context whose constant holds the name.
        context: usize,
        /// The conversion that failed.
        #[source]
        error: VariantError,
    },

    /// A relation names a context the block does not hold.
    #[error("context {context} names a {what} context that does not exist")]
    Relation {
        /// The context whose relation it is.
        context: usize,
        /// The relation that names it.
        what: &'static str,
    },

    /// The block's top-level index names a context the block does not hold.
    #[error("the block's top-level index {index} names no context")]
    TopLevel {
        /// The index the block holds.
        index: i32,
    },

    /// A member is registered under a name other than the one the context it
    /// holds carries.
    #[error("context {context} registers a member under a name the member does not carry")]
    MemberName {
        /// The context that registers the member.
        context: usize,
    },

    /// The labels of a context are not sorted by code position, so a source
    /// position cannot be looked up.
    #[error("the labels of context {context} are not sorted by code position")]
    UnsortedLabels {
        /// The context whose labels are unsorted.
        context: usize,
    },

    /// An array refused an element while the constant was being built.
    #[error(transparent)]
    Array(#[from] ArrayError),

    /// The object surface refused a member while the constant was being built.
    #[error(transparent)]
    Object(#[from] ObjectError),
}

impl Block {
    /// Builds the block the image describes.
    ///
    /// The image must be the one the compile entry built: its records name the
    /// generator's contexts, which is where the constant values and the member
    /// names are read from.
    pub fn build(image: &Image) -> Result<Self, BuildError> {
        let block = image.block.as_ref().ok_or(BuildError::NoBlock)?;
        let count = count_of(block.context_count, "the context count")?;
        let line_count = count_of(block.line_count, "the line count")?;
        if line_count != image.lines.len() {
            return Err(BuildError::Mismatch {
                what: "the line table",
            });
        }

        // the contexts the image holds, in its order: one that left the block
        // is not in the image
        let mut generated: Vec<&GeneratedContext> = Vec::with_capacity(count);
        for context in image.generator.contexts() {
            if !context.removed {
                generated.push(context);
            }
        }
        if generated.len() != count {
            return Err(BuildError::ContextCount {
                image: count,
                generator: generated.len(),
            });
        }

        // the objects, from the last context to the first: a constant can name
        // a context, and the value holds that context's object
        let mut objects: Vec<Option<Rc<ContextObject>>> = vec![None; count];
        for index in (0..count).rev() {
            let context = generated[index];
            let code = code_of(context, shape_of(image, index)?, &objects, index)?;
            let name = context.name.clone().unwrap_or_default();
            objects[index] = Some(Rc::new(ContextObject::new(code, context.kind, name)));
        }

        // the members each context contributes to its parent object
        for (index, context) in generated.iter().enumerate() {
            let object = object_of(&objects, index)?;
            for property in context.properties() {
                let member = generated
                    .get(property.member)
                    .ok_or(BuildError::UnknownContext {
                        context: index,
                        named: property.member,
                    })?;
                if member.name.as_deref() != Some(property.name.as_str()) {
                    return Err(BuildError::MemberName { context: index });
                }
                let child = object_of(&objects, property.member)?;
                object.define(
                    &TjsString::from(property.name.as_str()),
                    function_value(child),
                )?;
            }
        }

        // the parent links, now that every object exists: a context is built
        // before the one that holds it, so the link cannot be made when the
        // context's own object is
        for (index, context) in generated.iter().enumerate() {
            let Some(parent) = context.parent else {
                continue;
            };
            object_of(&objects, index)?.set_parent(object_of(&objects, parent)?);
        }

        // the records, in the image's order
        let mut contexts = Vec::with_capacity(count);
        for (index, context) in generated.iter().enumerate() {
            let object = objects[index]
                .take()
                .ok_or(BuildError::MissingContext { context: index })?;
            let shape = shape_of(image, index)?;
            let positions = context.source_positions();
            if !positions.is_sorted_by(|left, right| left.code_pos <= right.code_pos) {
                return Err(BuildError::UnsortedLabels { context: index });
            }
            let places = count_of(
                shape.super_class_getter_pointer_count,
                "the super-class places",
            )?;
            if places != context.super_class_getter_pointer.len() {
                return Err(BuildError::ContextMismatch {
                    context: index,
                    what: "the super-class places",
                });
            }
            let relations = Relations {
                parent: relation(shape.parent, count, index, "parent")?,
                setter: relation(shape.prop_setter, count, index, "setter")?,
                getter: relation(shape.prop_getter, count, index, "getter")?,
                super_class: relation(shape.super_class_getter, count, index, "super class")?,
            };
            // the image's relations are the generator's, with a context that
            // left the block's place taken out; a context's constant area is
            // read from the generator, so the two must name the same contexts
            if relations.parent != context.parent {
                return Err(BuildError::ContextMismatch {
                    context: index,
                    what: "the parent relation",
                });
            }
            contexts.push(ContextRecord {
                object,
                relations,
                super_class_code: context.super_class_getter_pointer.clone(),
                source_positions: positions.to_vec(),
            });
        }

        let top_level = if block.top_level < 0 {
            None
        } else {
            let index = usize::try_from(block.top_level).map_err(|_| BuildError::TopLevel {
                index: block.top_level,
            })?;
            if index >= count {
                return Err(BuildError::TopLevel {
                    index: block.top_level,
                });
            }
            Some(index)
        };

        let lines = image
            .lines
            .iter()
            .map(|line| Line {
                position: line.position,
                length: line.length,
            })
            .collect();

        Ok(Self {
            contexts,
            top_level,
            lines,
        })
    }

    /// The contexts of the block, in the image's order.
    pub fn contexts(&self) -> &[ContextRecord] {
        &self.contexts
    }

    /// The context at `index`, when the block holds one there.
    pub fn context(&self, index: usize) -> Option<&ContextRecord> {
        self.contexts.get(index)
    }

    /// The index of the top-level context, when the block has one.
    pub fn top_level_index(&self) -> Option<usize> {
        self.top_level
    }

    /// The top-level context, when the block has one.
    pub fn top_level(&self) -> Option<&ContextRecord> {
        self.top_level.and_then(|index| self.contexts.get(index))
    }

    /// The line table of the script.
    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    /// The line a place in the script falls in: the last line that starts at
    /// or before it, which is the line a message names for that place.
    ///
    /// `None` when the block has no line at all.
    pub fn line(&self, position: i32) -> Option<&Line> {
        let after = self.lines.partition_point(|line| line.position <= position);
        self.lines.get(after.saturating_sub(1))
    }

    /// The place in the script that the `position`-th word of the context's
    /// code came from.
    ///
    /// A position before the first label answers the first label's place,
    /// which is the place the instruction most nearly came from; `None` when
    /// the context holds no label.
    pub fn source_position(&self, context: usize, position: usize) -> Option<i32> {
        let record = self.contexts.get(context)?;
        let labels = &record.source_positions;
        let after = match i32::try_from(position) {
            Ok(position) => labels.partition_point(|entry| entry.code_pos <= position),
            // past every code position an instruction can have, so the last
            // label answers
            Err(_) => labels.len(),
        };
        labels
            .get(after.saturating_sub(1))
            .map(|entry| entry.source_pos)
    }
}

impl ContextRecord {
    /// The object the context is.
    pub fn object(&self) -> &Rc<ContextObject> {
        &self.object
    }

    /// The contexts this one is related to.
    pub fn relations(&self) -> &Relations {
        &self.relations
    }

    /// The places in the super-class proxy's own code where the super class is
    /// evaluated.
    pub fn super_class_code(&self) -> &[i32] {
        &self.super_class_code
    }

    /// The place in the script each code position came from, sorted by code
    /// position.
    pub fn source_positions(&self) -> &[SourcePos] {
        &self.source_positions
    }
}

/// The code of one context: the stream, the constant area resolved, and the
/// counts the frame is sized from.
fn code_of(
    context: &GeneratedContext,
    shape: &tjs2_codegen_context,
    built: &[Option<Rc<ContextObject>>],
    index: usize,
) -> Result<Code, BuildError> {
    let code_size = count_of(shape.code_size, "the code size")?;
    if code_size != context.code().len() {
        return Err(BuildError::ContextMismatch {
            context: index,
            what: "the code stream",
        });
    }
    let constant_count = count_of(shape.constant_count, "the constant count")?;
    if constant_count != context.data().len() {
        return Err(BuildError::ContextMismatch {
            context: index,
            what: "the constant area",
        });
    }
    let label_count = count_of(shape.label_count, "the label count")?;
    if label_count != context.source_positions().len() {
        return Err(BuildError::ContextMismatch {
            context: index,
            what: "the labels",
        });
    }
    let property_count = count_of(shape.property_count, "the member count")?;
    if property_count != context.properties().len() {
        return Err(BuildError::ContextMismatch {
            context: index,
            what: "the members",
        });
    }
    if shape.variable_reserve_count != context.kind.variable_reserve_count() {
        return Err(BuildError::ContextMismatch {
            context: index,
            what: "the reserved registers",
        });
    }
    let mut data = Vec::with_capacity(constant_count);
    for value in context.data() {
        data.push(constant_of(value, built, index)?);
    }
    Ok(Code {
        code: context.code().to_vec(),
        data,
        max_variable_count: shape.variable_count,
        variable_reserve_count: shape.variable_reserve_count,
        max_frame_count: shape.max_frame_count,
        func_decl_arg_count: shape.func_arg_count,
        func_decl_collapse_base: shape.func_collapse_base,
        func_decl_unnamed_arg_array_base: shape.func_unnamed_arg_base,
        name: context.name.clone().unwrap_or_default(),
    })
}

/// One value of the constant area, as a live value.
fn constant_of(
    value: &Value,
    built: &[Option<Rc<ContextObject>>],
    context: usize,
) -> Result<Variant, BuildError> {
    match value {
        Value::Void => Ok(Variant::Void),
        Value::Integer(number) => Ok(Variant::Integer(*number)),
        Value::Real(number) => Ok(Variant::Real(*number)),
        Value::String(text) => Ok(Variant::String(TjsString::from(text.as_str()))),
        Value::Octet(bytes) => Ok(Variant::Octet(Octet::from_slice(bytes))),
        // the null object is the identity an object with no dispatch has; any
        // other identity names an object the image cannot describe
        Value::Object(id) if id.is_null() => Ok(Variant::Object(Closure::null())),
        Value::Object(_) => Err(BuildError::ObjectConstant { context }),
        Value::Context(named) => match built.get(*named) {
            None => Err(BuildError::UnknownContext {
                context,
                named: *named,
            }),
            Some(None) => Err(BuildError::ContextNotBuilt {
                context,
                named: *named,
            }),
            Some(Some(object)) => Ok(function_value(object)),
        },
        Value::ConstArray { elements, .. } => {
            let array = ArrayObject::new();
            for element in elements {
                array.append(constant_of(element, built, context)?)?;
            }
            let object: Rc<dyn Dispatch> = Rc::new(array);
            Ok(Variant::Object(Closure {
                object: Some(object.clone()),
                objthis: Some(object),
            }))
        }
        Value::ConstDictionary { entries, .. } => {
            let dictionary = Dictionary::new();
            for (key, item) in entries {
                let key = constant_of(key, built, context)?;
                let name = key
                    .to_string()
                    .map_err(|error| BuildError::Name { context, error })?;
                let item = constant_of(item, built, context)?;
                dictionary.prop_set(&name, MemberFlags::ENSURE, &item, None)?;
            }
            let object: Rc<dyn Dispatch> = Rc::new(dictionary);
            Ok(Variant::Object(Closure {
                object: Some(object.clone()),
                objthis: Some(object),
            }))
        }
    }
}

/// The value a context is: its object, with no receiver.
///
/// A value with no receiver takes the receiver of the frame that calls it,
/// which is what a function value and a member that holds one both do.
fn function_value(object: &Rc<ContextObject>) -> Variant {
    let object: Rc<dyn Dispatch> = object.clone();
    Variant::Object(Closure {
        object: Some(object),
        objthis: None,
    })
}

/// The object at `index`, or the failure that says the build left it out.
fn object_of(
    built: &[Option<Rc<ContextObject>>],
    index: usize,
) -> Result<&Rc<ContextObject>, BuildError> {
    match built.get(index) {
        Some(Some(object)) => Ok(object),
        _ => Err(BuildError::MissingContext { context: index }),
    }
}

/// The `index`-th context record of the image.
fn shape_of(image: &Image, index: usize) -> Result<&tjs2_codegen_context, BuildError> {
    image
        .contexts
        .get(index)
        .ok_or(BuildError::ContextMismatch {
            context: index,
            what: "the record",
        })
}

/// A count the image holds, as a length.
fn count_of(value: i32, what: &'static str) -> Result<usize, BuildError> {
    usize::try_from(value).map_err(|_| BuildError::NegativeCount { what })
}

/// A relation the image holds, as an index.
///
/// The image names no relation with `-1`.
fn relation(
    index: i32,
    count: usize,
    context: usize,
    what: &'static str,
) -> Result<Option<usize>, BuildError> {
    if index < 0 {
        return Ok(None);
    }
    let place = usize::try_from(index).map_err(|_| BuildError::Relation { context, what })?;
    if place >= count {
        return Err(BuildError::Relation { context, what });
    }
    Ok(Some(place))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::codegen::context::ContextType;
    use crate::vm::codegen::value::{ObjectId, Value as GeneratedValue};
    use crate::vm::runtime::exec::Machine;
    use crate::vm::runtime::exec::op::discern_equals;
    use crate::vm::runtime::object::CustomObject;

    /// A block compiled from the script text, the way the compile entry
    /// compiles one: the analyser, the parser and the generator, and then the
    /// image the block is built from.
    fn compiled(text: &str, expression: bool) -> Block {
        compile(&TjsString::from(text), expression, expression).expect("the script compiles")
    }

    /// Equality over every kind, which a variant does not carry.
    fn same(left: &Variant, right: &Variant) -> bool {
        discern_equals(left, right)
    }

    /// The object a value names.
    fn object_of(value: &Variant) -> &Rc<dyn Dispatch> {
        match value {
            Variant::Object(closure) => match &closure.object {
                Some(object) => object,
                None => panic!("the value names no object"),
            },
            other => panic!("the value is not an object: {other:?}"),
        }
    }

    /// Whether the value is the record's own object, and not another one.
    fn is_object_of(value: &Variant, record: &ContextRecord) -> bool {
        let Variant::Object(closure) = value else {
            return false;
        };
        let Some(object) = &closure.object else {
            return false;
        };
        std::ptr::eq(
            Rc::as_ptr(object) as *const (),
            Rc::as_ptr(record.object()) as *const (),
        )
    }

    /// Whether the two values are one object.
    fn same_object(left: &Variant, right: &Variant) -> bool {
        let (Variant::Object(left), Variant::Object(right)) = (left, right) else {
            return false;
        };
        match (&left.object, &right.object) {
            (Some(left), Some(right)) => std::ptr::eq(
                Rc::as_ptr(left) as *const (),
                Rc::as_ptr(right) as *const (),
            ),
            (None, None) => true,
            _ => false,
        }
    }

    /// The element count an array answers.
    fn length_of(object: &Rc<dyn Dispatch>) -> usize {
        let length = object
            .prop_get(&TjsString::from("length"), MemberFlags::NONE, None)
            .expect("the array answers its length");
        match length {
            Variant::Integer(value) => usize::try_from(value).expect("a length is a count"),
            other => panic!("the length is not an integer: {other:?}"),
        }
    }

    /// A constant array and a constant dictionary become one live object each,
    /// and the top level answers the constant its area holds.
    ///
    /// reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:534-620 (the constant
    /// inline array and dictionary: the elements are added in order and the
    /// members are set in order);
    /// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:880-883 (a constant
    /// is copied out of the data area into a register);
    /// reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:145-155 (the block's top
    /// level is the context a script's body runs in).
    #[test]
    fn the_top_level_answers_the_constant_its_area_holds() {
        let block = compiled("(const)[1, 2, \"x\"]", true);
        assert_eq!(block.contexts().len(), 1, "the expression has one context");
        assert_eq!(block.top_level_index(), Some(0));
        let top = block.top_level().expect("the block names its top level");
        assert_eq!(top.object().kind(), ContextType::TopLevel);
        assert_eq!(top.object().name(), &TjsString::from("global"));
        assert_eq!(top.relations().parent, None);
        let code = top.object().code();
        assert_eq!(code.data.len(), 1, "the array is the one constant");

        // the array is live before anything runs: three elements, in order
        let array = object_of(&code.data[0]);
        assert_eq!(length_of(array), 3);
        let last = array
            .prop_get_by_num(2, MemberFlags::NONE, None)
            .expect("the third element");
        assert!(same(&last, &Variant::from("x")));

        // and running the top level answers that same array
        let mut machine = Machine::new();
        let answer = machine
            .run_context(top.object(), None, &[])
            .expect("the top level runs");
        assert!(
            same_object(&answer, &code.data[0]),
            "the answer is the array the constant area holds, not another object"
        );
        assert_eq!(length_of(object_of(&answer)), 3);

        // the dictionary: one member an integer, one a nested array, and both
        // are the objects the source made
        let block = compiled("(const)%[\"a\", 1, \"b\", (const)[2, 3]]", true);
        let top = block.top_level().expect("the block names its top level");
        let mut machine = Machine::new();
        let answer = machine
            .run_context(top.object(), None, &[])
            .expect("the top level runs");
        let dictionary = object_of(&answer);
        let member = dictionary
            .prop_get(&TjsString::from("a"), MemberFlags::NONE, None)
            .expect("the member is there");
        assert!(same(&member, &Variant::Integer(1)));
        let nested = dictionary
            .prop_get(&TjsString::from("b"), MemberFlags::NONE, None)
            .expect("the member is there");
        assert_eq!(length_of(object_of(&nested)), 2);
        // a dictionary answers the class a container belongs to
        assert_eq!(
            dictionary.class_names(),
            vec![TjsString::from("Dictionary")]
        );
    }

    /// A declared function is a member of the object that holds it, every
    /// context of the image is its own object, and a constant that names a
    /// nested context names that context and not the one it sits in.
    ///
    /// reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:886-949
    /// (`RegisterFunction`: the child's context is registered on the parent's
    /// object, and the parent's constant area holds the child);
    /// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:704-731 (the frame
    /// lays the declared arguments out at `ra[-3]` downwards);
    /// reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:743-760 (the member
    /// registrations are replayed in the image's order).
    #[test]
    fn a_declared_function_is_a_member_of_the_object_that_holds_it() {
        let block = compiled(
            "function outer(x) { function inner(y) { return y + x; } return inner; }",
            false,
        );
        assert_eq!(block.contexts().len(), 3, "the top level and two functions");
        let top = block.top_level().expect("the block names its top level");
        assert_eq!(top.relations().parent, None);
        // the top level's body made the outer, and the top level holds it in
        // its constant area so that the name can be used before it is stored
        let outer = block.context(1).expect("the outer is a context");
        assert_eq!(outer.object().kind(), ContextType::Function);
        assert_eq!(outer.object().name(), &TjsString::from("outer"));
        assert_eq!(outer.relations().parent, Some(0));
        assert_eq!(outer.object().code().func_decl_arg_count, 1);
        assert_eq!(outer.object().code().variable_reserve_count, 2);
        assert!(is_object_of(&top.object().code().data[0], outer));

        // the outer's body made the inner, the outer publishes it as a member,
        // and the outer's constant area holds it
        let inner = block.context(2).expect("the inner is a context");
        assert_eq!(inner.object().kind(), ContextType::Function);
        assert_eq!(inner.object().name(), &TjsString::from("inner"));
        assert_eq!(inner.relations().parent, Some(1));
        let holder = inner
            .object()
            .parent()
            .expect("the inner is held by the outer");
        assert!(
            std::ptr::eq(Rc::as_ptr(&holder), Rc::as_ptr(outer.object())),
            "the parent link names the context that holds it"
        );
        let member = outer
            .object()
            .prop_get(&TjsString::from("inner"), MemberFlags::NONE, None)
            .expect("the inner is a member of the outer");
        assert!(
            is_object_of(&member, inner),
            "the member is the inner's own object"
        );
        assert_eq!(outer.object().code().data.len(), 1);
        assert!(
            is_object_of(&outer.object().code().data[0], inner),
            "the outer's constant names the inner, not the outer"
        );

        // the labels are the image's, sorted, and the lookups walk them
        let positions = inner.source_positions();
        assert!(!positions.is_empty());
        for entry in positions {
            let position = usize::try_from(entry.code_pos).expect("a code position is a count");
            assert_eq!(block.source_position(2, position), Some(entry.source_pos));
        }
        assert_eq!(
            block.source_position(2, 0),
            Some(positions[0].source_pos),
            "a position before the first label answers the first one"
        );
        let place = block
            .source_position(1, 0)
            .expect("the outer names a place in the script");
        let line = block.line(place).expect("the place falls in a line");
        assert_eq!(line.position, 0, "the script is one line");

        // the inner runs with its declared argument and the receiver's member
        let receiver = Rc::new(CustomObject::new(Vec::new()));
        receiver
            .prop_set(
                &TjsString::from("x"),
                MemberFlags::ENSURE,
                &Variant::Integer(10),
                None,
            )
            .expect("the receiver holds the member");
        let receiver: Rc<dyn Dispatch> = receiver;
        let mut machine = Machine::new();
        let answer = machine
            .run_context(inner.object(), Some(receiver), &[Variant::Integer(4)])
            .expect("the inner runs");
        assert!(same(&answer, &Variant::Integer(14)), "4 + 10");
    }

    /// The builder attaches every context to the context that holds it, after
    /// every object exists: a context is built before the one that holds it, so
    /// the link cannot be made when the context's own object is.
    ///
    /// reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:176 (the parent is the
    /// context the child was made in) and :886-949 (`RegisterFunction`).
    #[test]
    fn the_builder_attaches_each_context_to_the_one_that_holds_it() {
        let block = compiled("class C extends B { function m() { return 1; } }", false);
        let top = block.top_level().expect("the block names its top level");
        assert!(
            top.object().parent().is_none(),
            "the top level is held by none"
        );
        let class = block
            .contexts()
            .iter()
            .find(|record| record.object().kind() == ContextType::Class)
            .expect("the class is a context");
        assert_eq!(class.relations().parent, Some(0));
        let holder = class
            .object()
            .parent()
            .expect("the class is held by the top level");
        assert!(
            std::ptr::eq(Rc::as_ptr(&holder), Rc::as_ptr(top.object())),
            "the class's parent is the context that holds it"
        );
        // the class carries its own name and nothing of the context that holds
        // it, which has no name of its own
        assert_eq!(class.object().class_names(), vec![TjsString::from("C")]);
    }

    /// A function expression is a constant of the context it sits in, and the
    /// value is that context's object: running it runs the expression's own
    /// code.
    ///
    /// reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:911-926 (a function
    /// expression's context is put into the constant area by its index);
    /// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2107-2155 (a call
    /// runs the closure's context);
    /// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:683-707 (the
    /// receiver is `ra[-1]`, and the objthis-context is `ra[-2]`).
    #[test]
    fn a_function_expression_is_a_constant_that_runs() {
        let block = compiled("var f = function (x) { return x * 2; };", false);
        assert_eq!(block.contexts().len(), 2);
        let top = block.top_level().expect("the block names its top level");
        let code = top.object().code();
        assert_eq!(
            code.data.len(),
            2,
            "the function and the name it is stored under"
        );
        assert!(same(&code.data[1], &Variant::from("f")));

        let expression = block
            .context(1)
            .expect("the function expression is a context");
        assert_eq!(expression.object().kind(), ContextType::ExprFunction);
        assert_eq!(expression.relations().parent, Some(0));
        assert!(
            is_object_of(&code.data[0], expression),
            "the constant names the function expression, not the top level"
        );
        assert_eq!(expression.object().code().func_decl_arg_count, 1);

        let mut machine = Machine::new();
        let answer = machine
            .run_context(expression.object(), None, &[Variant::Integer(3)])
            .expect("the expression runs");
        assert!(same(&answer, &Variant::Integer(6)));
    }

    /// The kinds of a constant the build cannot make are failures, not
    /// guesses.
    ///
    /// reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:474-560 (a constant of
    /// a kind the image cannot build raises the script error).
    #[test]
    fn a_constant_the_image_cannot_build_is_a_failure() {
        let built: Vec<Option<Rc<ContextObject>>> = vec![None];
        let value = GeneratedValue::Object(ObjectId(7));
        assert!(matches!(
            constant_of(&value, &built, 0),
            Err(BuildError::ObjectConstant { context: 0 })
        ));
        let value = GeneratedValue::Context(0);
        assert!(matches!(
            constant_of(&value, &built, 0),
            Err(BuildError::ContextNotBuilt { named: 0, .. })
        ));
        let value = GeneratedValue::Context(4);
        assert!(matches!(
            constant_of(&value, &built, 0),
            Err(BuildError::UnknownContext { named: 4, .. })
        ));
    }

    /// A block built from an image with no block is refused, and so is a
    /// relation or a top level that names a context the block does not hold.
    #[test]
    fn an_image_that_does_not_describe_itself_is_refused() {
        let image = Image::default();
        assert!(matches!(Block::build(&image), Err(BuildError::NoBlock)));
        assert!(matches!(
            relation(2, 2, 0, "parent"),
            Err(BuildError::Relation { what: "parent", .. })
        ));
        assert!(matches!(relation(-1, 2, 0, "parent"), Ok(None)));
        assert!(matches!(relation(1, 2, 0, "parent"), Ok(Some(1))));
    }
}
