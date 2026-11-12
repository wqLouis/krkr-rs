//! The emission of the intermediate code.
//!
//! The generator walks the expression tree the parser built and emits
//! the instructions the executor runs. Every instruction carries the
//! place in the script it came from, so the executor's error messages and
//! the debugger can name the line.

use std::rc::Rc;

use super::context::{Context, ContextType, FuncArgKind};
use super::node::Node;
use super::opcode::{self, vm};
use super::token;
use super::value::Value;

/// The result a piece of code is needed for.
pub const RT_NEEDED: u32 = 0x0001;
/// The result is a condition flag.
pub const RT_CFLAG: u32 = 0x0002;
/// The result is the true condition flag.
pub const GNC_CFLAG: i32 = 1 << (i32::BITS - 1);
/// The result is the inverted condition flag.
pub const GNC_CFLAG_I: i32 = GNC_CFLAG + 1;

/// Whether the result is a register.
pub fn is_frame(result: i32) -> bool {
    result > 0
}

/// Whether the result is a condition flag.
pub fn is_cond_flag(result: i32) -> bool {
    result == GNC_CFLAG || result == GNC_CFLAG_I
}

/// What the emission of a node needs to know besides the node.
#[derive(Clone, Copy, Debug, Default)]
pub struct SubParam {
    /// The sub-type of the operation, when it has one.
    pub sub_type: i32,
    /// The register the operation reads, when it has one.
    pub sub_address: i32,
}

/// The sub-types of the operations that modify their left hand side.
pub mod sub_type {
    /// The operation does not modify its left hand side.
    pub const NONE: i32 = 0;
    /// The operation increments before the value is read.
    pub const PRE_INC: i32 = 1;
    /// The operation decrements before the value is read.
    pub const PRE_DEC: i32 = 2;
    /// The operation increments after the value is read.
    pub const POST_INC: i32 = 3;
    /// The operation decrements after the value is read.
    pub const POST_DEC: i32 = 4;
    /// The operation deletes the member.
    pub const DELETE: i32 = 5;
    /// The operation calls the member.
    pub const FUNC_CALL: i32 = 6;
    /// The operation reads the member without the property access.
    pub const IGNORE_PROP_GET: i32 = 7;
    /// The operation writes the member without the property access.
    pub const IGNORE_PROP_SET: i32 = 8;
    /// The operation asks for the type of the member.
    pub const TYPEOF: i32 = 9;
}

/// Whether the sub-type modifies its left hand side.
pub fn is_modify_sub_type(sub_type: i32) -> bool {
    (sub_type::PRE_INC..=sub_type::TYPEOF).contains(&sub_type)
}

/// The reason the emission fails.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EmitError {
    /// The tree has a shape the generator does not support.
    Shape(String),
}

/// The result of a piece of emission.
pub type EmitResult = Result<(), EmitError>;

/// The emitter: it drives the emission into a set of contexts.
///
/// The emitter borrows the contexts of the generator, so that the
/// emission and the context management are one thing.
pub struct Emitter<'a> {
    /// The contexts, in the order they were made.
    pub contexts: &'a mut Vec<Context>,
    /// The context the emission is for.
    pub current: usize,
    /// The place in the script the analyser is at.
    ///
    /// The place of an instruction comes from the analyser, not from the
    /// node.
    pub lex_position: i32,
    /// Whether the code is generated in the global context.
    pub as_global_context_mode: bool,
}

impl<'a> Emitter<'a> {
    /// An emitter over the given contexts.
    pub fn new(contexts: &'a mut Vec<Context>, current: usize, lex_position: i32) -> Self {
        Self {
            contexts,
            current,
            lex_position,
            as_global_context_mode: false,
        }
    }

    /// The place the analyser is at.
    pub fn lex_pos(&self) -> i32 {
        self.lex_position
    }

    /// The context the emission is for.
    pub(crate) fn context(&self) -> &Context {
        &self.contexts[self.current]
    }

    /// The context the emission is for, for a change.
    pub(crate) fn context_mut(&mut self) -> &mut Context {
        &mut self.contexts[self.current]
    }

    /// Whether the code is generated in the global context.
    ///
    /// The global context cannot reach a local variable, so the name
    /// space is not consulted.
    pub fn as_global_context_mode(&self) -> bool {
        self.as_global_context_mode
    }

    /// Whether the name of the given node is a variable of the enclosing
    /// scope rather than a member of `this`.
    pub fn is_non_local_symbol(&self, node: &Rc<Node>) -> bool {
        if self.as_global_context_mode {
            return true;
        }
        let name = match node.value() {
            Some(Value::String(name)) => name.clone(),
            _ => return false,
        };
        self.context().namespace.find(&name) == -1
    }

    /// Opens a function argument list.
    pub fn start_func_arg(&mut self) {
        self.context_mut().start_func_arg();
    }

    /// Closes a function argument list.
    pub fn end_func_arg(&mut self) {
        self.context_mut().end_func_arg();
    }

    /// Adds an argument to the argument list being built.
    pub fn add_func_arg(&mut self, register: i32, kind: FuncArgKind) {
        self.context_mut().add_func_arg(register, kind);
    }

    /// Marks the argument list as omitting its arguments.
    pub fn add_omit_arg(&mut self) {
        self.context_mut().add_omit_arg();
    }

    /// The code of the argument list.
    pub fn generate_func_call_arg_code(&mut self) {
        self.context_mut().generate_func_call_arg_code();
    }

    /// Opens an array argument.
    pub fn push_array_arg(&mut self, object: i32, counter: i32) {
        self.context_mut().push_array_arg(object, counter);
    }

    /// Closes an array argument.
    pub fn pop_array_arg(&mut self) {
        self.context_mut().pop_array_arg();
    }

    /// The proxy context of the given class, when it has one.
    pub fn super_class_getter(&self, class: usize) -> Option<usize> {
        self.contexts.iter().position(|context| {
            context.kind == ContextType::SuperClassGetter && context.parent == Some(class)
        })
    }

    /// Opens a code context, and makes it the one the code is generated
    /// for.
    pub fn push_context(&mut self, name: Option<&[u16]>, kind: ContextType) -> usize {
        let index = self.contexts.len();
        let parent = if index == 0 { None } else { Some(self.current) };
        let name = name.map(String::from_utf16_lossy);
        let mut context = Context::new(kind, name, index, parent);
        if kind == ContextType::Class {
            // the class registers its members to the object it makes
            context.max_frame_count = 1;
            let data = context.put_data(crate::vm::codegen::value::Value::String(
                context.name.clone().unwrap_or_default(),
            ));
            context.put_code(vm::CONST, -1);
            context.put_code(opcode::vm_reg_addr(1), -1);
            context.put_code(opcode::vm_reg_addr(data), -1);
            context.put_code(vm::ADDCI, -1);
            context.put_code(opcode::vm_reg_addr(-1), -1);
            context.put_code(opcode::vm_reg_addr(1), -1);
            context.put_code(vm::CL, -1);
            context.put_code(opcode::vm_reg_addr(1), -1);
            context.function_register_code_point = context.code_size();
        }
        self.contexts.push(context);
        index
    }

    /// The object and the counter of the array argument being built.
    pub fn array_arg(&self) -> (i32, i32) {
        self.context().array_arg()
    }

    /// Adds a local variable to the context.
    pub fn add_local_variable(&mut self, name: &str, init: i32) {
        let position = self.lex_pos();
        let base = if self.context().kind == ContextType::Class {
            2
        } else {
            1
        };
        if self.context().namespace.level() >= base {
            // the variable is a local one
            self.context_mut().namespace.add(name);
            let index = self.context().namespace.find(name);
            let reserve = self.context().kind.variable_reserve_count();
            let register = -index - reserve - 1;
            if init != 0 {
                self.context_mut().put_code(vm::CP, position);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(register), position);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(init), position);
            } else {
                self.context_mut().put_code(vm::CL, position);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(register), position);
            }
        } else {
            // the variable is a member of `this`
            let data = self.context_mut().put_data(Value::String(name.to_string()));
            self.context_mut().put_code(vm::SPDS, position);
            self.context_mut()
                .put_code(opcode::vm_reg_addr(-1), position);
            self.context_mut()
                .put_code(opcode::vm_reg_addr(data), position);
            self.context_mut()
                .put_code(opcode::vm_reg_addr(init), position);
        }
    }

    /// Clears the frame registers above the given base.
    pub fn clear_frame(&mut self, frame: &mut i32, base: i32) {
        let base = if base == -1 {
            self.context().frame_base
        } else {
            base
        };
        let frame_minus_one = *frame - 1;
        let max_frame_count = self.context().max_frame_count;
        if frame_minus_one > max_frame_count {
            self.context_mut().max_frame_count = frame_minus_one;
        }
        if *frame - base >= 3 {
            *frame = base;
        } else {
            while *frame > base {
                *frame -= 1;
            }
        }
    }

    /// Emits the code of a node.
    ///
    /// The frame is the register stack; the result is the register that
    /// holds the value, or a condition flag.
    pub fn gen_node_code(
        &mut self,
        frame: &mut i32,
        node: Option<&Rc<Node>>,
        restype: u32,
        param: SubParam,
    ) -> Result<i32, EmitError> {
        let Some(node) = node else {
            return Ok(0);
        };
        let node_pos = node.position;
        match node.opcode {
            token::T_CONSTVAL => {
                if is_modify_sub_type(param.sub_type) {
                    return Err(EmitError::Shape(
                        "the left hand side of the operation is a constant".to_string(),
                    ));
                }
                if restype & RT_NEEDED == 0 {
                    return Ok(0);
                }
                let value = node.value.clone().unwrap_or(Value::Void);
                let dp = self.context_mut().put_data(value);
                self.context_mut().put_code(vm::CONST, node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(*frame), node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(dp), node_pos);
                let result = *frame;
                *frame += 1;
                Ok(result)
            }
            token::T_IF => {
                if restype & RT_NEEDED != 0 {
                    return Err(EmitError::Shape(
                        "the 'if' operator has no result".to_string(),
                    ));
                }
                let condition = self.gen_node_code(
                    frame,
                    node.child(1),
                    RT_NEEDED | RT_CFLAG,
                    SubParam::default(),
                )?;
                let mut inverted = false;
                if !is_cond_flag(condition) {
                    self.context_mut().put_code(vm::TT, node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(condition), node_pos);
                } else if condition == GNC_CFLAG_I {
                    inverted = true;
                }
                let address = self.context().code_size();
                self.context_mut().add_jump();
                self.context_mut()
                    .put_code(if inverted { vm::JF } else { vm::JNF }, node_pos);
                self.context_mut().put_code(0, node_pos);
                self.gen_node_code(frame, node.child(0), 0, param)?;
                let size = self.context().code_size();
                self.context_mut().code_mut()[(address + 1) as usize] = size - address;
                Ok(0)
            }
            _ => {
                // the operations the generator does not fold are emitted
                // through the operation table
                self.gen_operation_code(frame, node, restype, param)
            }
        }
    }
}
