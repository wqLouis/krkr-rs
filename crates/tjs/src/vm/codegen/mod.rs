//! The code generator.
//!
//! The generator makes the intermediate code of a script block: the top
//! level and every function, class and handler it holds.
//!
//! There are two interfaces over this one implementation: the native
//! Rust API the parser uses, and the C interface the script block is
//! handed over with. Both drive the same code, so the two can never
//! disagree.

#![deny(unsafe_code)]

pub mod context;
pub mod emit;
pub mod extends;
pub mod ffi;
pub mod folding;
pub mod namespace;
pub mod node;
pub mod objects;
pub mod opcode;
pub mod operations;
pub mod statements;
pub mod token;
pub mod value;

use std::rc::Rc;

use context::{Context, ContextType, NonLocalFunctionDecl};
use emit::SubParam;
use node::Node;
use value::Value;

/// The reason the generator fails.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodegenError {
    /// The tree has a shape the generator does not support.
    Shape(String),
    /// A value could not be converted where the generator converts it.
    Value(value::ValueError),
}

impl std::fmt::Display for CodegenError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodegenError::Shape(message) => write!(formatter, "{message}"),
            CodegenError::Value(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for CodegenError {}

impl From<emit::EmitError> for CodegenError {
    fn from(error: emit::EmitError) -> Self {
        match error {
            emit::EmitError::Shape(message) => CodegenError::Shape(message),
        }
    }
}

/// The generator: it holds the contexts of one script block.
#[derive(Clone, Debug)]
pub struct Generator {
    /// The contexts, in the order they were made.
    contexts: Vec<Context>,
    /// The context the code is generated for.
    current: usize,
    /// The place in the script the analyser is at.
    ///
    /// The instructions that have no place of their own take this one.
    /// The parser keeps it current: it tells the generator where the
    /// analyser is every time the analyser reads.
    pub lex_position: i32,
}

impl Default for Generator {
    fn default() -> Self {
        Self {
            contexts: Vec::new(),
            current: 0,
            // the analyser has not been read yet: the instructions that
            // have no place of their own have no place
            lex_position: -1,
        }
    }
}

impl Generator {
    /// A generator with no context.
    pub fn new() -> Self {
        Self::default()
    }

    /// The contexts, in the order they were made.
    pub fn contexts(&self) -> &[Context] {
        &self.contexts
    }

    /// The context the code is generated for.
    pub fn current(&self) -> &Context {
        &self.contexts[self.current]
    }

    /// The context the code is generated for, for a change.
    pub fn current_mut(&mut self) -> &mut Context {
        &mut self.contexts[self.current]
    }

    /// The place of the context the code is generated for.
    pub fn current_index(&self) -> usize {
        self.current
    }

    /// Opens a code context.
    ///
    /// The context becomes the one the code is generated for; the context
    /// that held it becomes its parent.
    pub fn push_context(&mut self, name: Option<&[u16]>, kind: ContextType) -> usize {
        let index = {
            let mut emitter =
                emit::Emitter::new(&mut self.contexts, self.current, self.lex_position);
            emitter.push_context(name, kind)
        };
        self.current = index;
        index
    }

    /// Closes the code context the code is generated for.
    ///
    /// The context is finished — its jumps are fixed up, its code is
    /// registered with its parent — and the context that held it becomes
    /// the one the code is generated for again.
    pub fn pop_context(&mut self) -> Result<(), CodegenError> {
        let index = self.current;
        self.commit(index)?;
        self.current = match self.contexts[index].parent {
            Some(parent) => parent,
            None => index,
        };
        Ok(())
    }

    /// Finishes a context.
    pub fn commit(&mut self, index: usize) -> Result<(), CodegenError> {
        let kind = self.contexts[index].kind;
        // the top level is the last context to be committed, and the
        // block is whole when it is
        if kind == ContextType::TopLevel {
            self.finalize();
        }
        // the class commits its super class proxy first
        if kind == ContextType::Class
            && let Some(getter) = self.contexts[index].super_class_getter
        {
            self.commit(getter)?;
        }
        // the epilogue
        if kind != ContextType::Property && kind != ContextType::SuperClassGetter {
            let position = self.lex_position;
            self.contexts[index].put_code(opcode::vm::SRV, position);
            self.contexts[index].put_code(opcode::vm_reg_addr(0), position);
            self.contexts[index].put_code(opcode::vm::RET, position);
        }
        // the registration of the context in its parent
        self.register_function(index)?;
        // the fix-up
        if kind != ContextType::Property && kind != ContextType::SuperClassGetter {
            self.contexts[index].fix_code();
        }
        if kind == ContextType::SuperClassGetter {
            self.contexts[index].max_variable_count = 2;
        } else {
            self.contexts[index].max_variable_count = self.contexts[index].namespace.max_count;
        }
        Ok(())
    }

    /// Finishes the block.
    ///
    /// A member of a class that is registered again under the same name
    /// replaces the earlier one, and the earlier context is released; it
    /// leaves the block when nothing else holds it. The contexts after it
    /// move down, and the places that name them move with them.
    fn finalize(&mut self) {
        if !self.contexts.iter().any(|context| context.removed) {
            return;
        }
        // the place every context moves to
        let mut places: Vec<Option<usize>> = Vec::with_capacity(self.contexts.len());
        let mut live = 0;
        for context in &self.contexts {
            if context.removed {
                places.push(None);
            } else {
                places.push(Some(live));
                live += 1;
            }
        }
        // the places that name the contexts are moved
        for context in self.contexts.iter_mut() {
            for value in context.data_mut().iter_mut() {
                if let Value::Context(named) = value {
                    *named = places[*named].unwrap_or(*named);
                }
            }
            context.parent = context.parent.and_then(|parent| places[parent]);
            context.prop_setter = context.prop_setter.and_then(|setter| places[setter]);
            context.prop_getter = context.prop_getter.and_then(|getter| places[getter]);
            context.super_class_getter =
                context.super_class_getter.and_then(|getter| places[getter]);
            for property in context.properties.iter_mut() {
                property.member = places[property.member].unwrap_or(property.member);
            }
        }
        // the contexts that have left the block are dropped
        let mut contexts: Vec<Context> = Vec::with_capacity(self.contexts.len());
        for context in self.contexts.drain(..) {
            if !context.removed {
                contexts.push(context);
            }
        }
        self.contexts = contexts;
    }

    /// The registration of a context in its parent.
    ///
    /// `RegisterFunction` registers the child context as a member of its
    /// parent object, which needs a live object; the generator decides
    /// the list, and the side that has the object performs it.
    fn register_function(&mut self, index: usize) -> Result<(), CodegenError> {
        let Some(parent) = self.contexts[index].parent else {
            return Ok(());
        };
        let kind = self.contexts[index].kind;
        let name = self.contexts[index].name.clone().unwrap_or_default();

        // the property's setter and getter are linked to their parent
        if kind == ContextType::PropertySetter {
            self.contexts[parent].prop_setter = Some(index);
            return Ok(());
        }
        if kind == ContextType::PropertyGetter {
            self.contexts[parent].prop_getter = Some(index);
            return Ok(());
        }
        // the super class proxy is registered to its parent already
        if kind == ContextType::SuperClassGetter {
            return Ok(());
        }
        // the expression function is not concerned here
        if !matches!(
            kind,
            ContextType::Function | ContextType::Property | ContextType::Class
        ) {
            return Ok(());
        }

        let mut data: Option<i32> = None;
        // the function of a top level is registered so that it can be
        // referred to before it is made
        if self.contexts[parent].kind == ContextType::TopLevel {
            let data_pos = self.contexts[parent].put_data(Value::Context(index));
            let name_pos = self.contexts[parent].put_data(Value::String(name.clone()));
            let change_this = matches!(kind, ContextType::Function | ContextType::Property);
            self.contexts[parent].add_non_local_function_decl(NonLocalFunctionDecl {
                data_pos,
                name_data_pos: name_pos,
                change_this,
            });
            data = Some(data_pos);
        }
        // the local function is a local variable of its parent
        if kind == ContextType::Function && self.contexts[parent].kind == ContextType::Function {
            let data_pos = match data {
                Some(data_pos) => data_pos,
                None => self.contexts[parent].put_data(Value::Context(index)),
            };
            self.init_local_function(parent, &name, data_pos)?;
        }
        // the child is a member of the parent object
        if matches!(
            self.contexts[parent].kind,
            ContextType::Function | ContextType::Class
        ) {
            // the member replaces the one with the same name, and the
            // earlier member's context is released; it leaves the block
            // when nothing else holds it
            let held_by_the_constant_area = kind == ContextType::Function
                && self.contexts[parent].kind == ContextType::Function;
            match self.contexts[parent].property_index(&name) {
                Some(place) => {
                    let earlier = self.contexts[parent].properties[place].member;
                    if !held_by_the_constant_area {
                        self.contexts[earlier].removed = true;
                    }
                    self.contexts[parent].properties[place].member = index;
                }
                None => self.contexts[parent].add_property(name, index),
            }
        }
        Ok(())
    }

    /// The local variable a local function is held by.
    ///
    /// `InitLocalFunction`: the constant that holds the function is copied
    /// into the frame, and the local variable takes it from there.
    fn init_local_function(
        &mut self,
        parent: usize,
        name: &str,
        data: i32,
    ) -> Result<(), CodegenError> {
        let mut frame = self.contexts[parent].frame_base;
        let position = self.lex_position;
        self.contexts[parent].put_code(opcode::vm::CONST, position);
        self.contexts[parent].put_code(opcode::vm_reg_addr(frame), position);
        self.contexts[parent].put_code(opcode::vm_reg_addr(data), position);
        frame += 1;
        {
            let mut emitter = emit::Emitter::new(&mut self.contexts, parent, self.lex_position);
            emitter.add_local_variable(name, frame - 1);
        }
        self.clear_frame_at(parent, &mut frame);
        Ok(())
    }

    /// Clears the frame registers above the context's base.
    ///
    /// `ClearFrame`.
    fn clear_frame_at(&mut self, index: usize, frame: &mut i32) {
        let base = self.contexts[index].frame_base;
        let frame_minus_one = *frame - 1;
        if frame_minus_one > self.contexts[index].max_frame_count {
            self.contexts[index].max_frame_count = frame_minus_one;
        }
        if *frame - base >= 3 {
            *frame = base;
        } else {
            while *frame > base {
                *frame -= 1;
            }
        }
    }

    /// The code of a node.
    pub fn create_expr_code(&mut self, node: &Rc<Node>) -> Result<(), CodegenError> {
        let mut frame = self.current().frame_base;
        self.gen_node_code(&mut frame, Some(node), 0, SubParam::default())?;
        let base = self.current().frame_base;
        self.clear_frame(&mut frame, base);
        Ok(())
    }

    /// The code of a node, with the result in the given register.
    pub fn gen_node_code(
        &mut self,
        frame: &mut i32,
        node: Option<&Rc<Node>>,
        restype: u32,
        param: SubParam,
    ) -> Result<i32, CodegenError> {
        let mut emitter = emit::Emitter::new(&mut self.contexts, self.current, self.lex_position);
        let result = emitter.gen_node_code(frame, node, restype, param);
        self.current = emitter.current;
        Ok(result?)
    }

    /// The emitter over the generator's contexts.
    fn emitter(&mut self) -> emit::Emitter<'_> {
        emit::Emitter::new(&mut self.contexts, self.current, self.lex_position)
    }

    /// Adds a local variable to the context.
    pub fn add_local_variable(&mut self, name: &str, init: i32) {
        self.emitter().add_local_variable(name, init);
    }

    /// Creates a local variable with an initial value.
    pub fn init_local_variable(&mut self, name: &str, node: &Rc<Node>) -> Result<(), CodegenError> {
        let mut frame = self.current().frame_base;
        let result =
            self.gen_node_code(&mut frame, Some(node), emit::RT_NEEDED, SubParam::default())?;
        self.add_local_variable(name, result);
        self.clear_frame(&mut frame, self.current().frame_base);
        Ok(())
    }

    /// Opens a `while` loop.
    pub fn enter_while_code(&mut self, do_while: bool) {
        self.emitter().enter_while_code(do_while);
    }

    /// The code of the condition of a `while` loop.
    pub fn create_while_expr_code(
        &mut self,
        node: Option<&Rc<Node>>,
        do_while: bool,
    ) -> Result<(), CodegenError> {
        self.emitter().create_while_expr_code(node, do_while)?;
        Ok(())
    }

    /// Closes a `while` loop.
    pub fn exit_while_code(&mut self, do_while: bool) -> Result<(), CodegenError> {
        self.emitter().exit_while_code(do_while)?;
        Ok(())
    }

    /// Opens an `if`.
    pub fn enter_if_code(&mut self) {
        self.emitter().enter_if_code();
    }

    /// The code of the condition of an `if`.
    pub fn create_if_expr_code(&mut self, node: Option<&Rc<Node>>) -> Result<(), CodegenError> {
        self.emitter().create_if_expr_code(node)?;
        Ok(())
    }

    /// Closes an `if`.
    pub fn exit_if_code(&mut self) -> Result<(), CodegenError> {
        self.emitter().exit_if_code()?;
        Ok(())
    }

    /// Opens an `else`.
    pub fn enter_else_code(&mut self) {
        self.emitter().enter_else_code();
    }

    /// Closes an `else`.
    pub fn exit_else_code(&mut self) -> Result<(), CodegenError> {
        self.emitter().exit_else_code()?;
        Ok(())
    }

    /// Opens a `for`.
    pub fn enter_for_code(&mut self) {
        self.emitter().enter_for_code();
    }

    /// The code of the condition of a `for`.
    pub fn create_for_expr_code(&mut self, node: Option<&Rc<Node>>) -> Result<(), CodegenError> {
        self.emitter().create_for_expr_code(node)?;
        Ok(())
    }

    /// Closes a `for`.
    pub fn exit_for_code(&mut self) -> Result<(), CodegenError> {
        self.emitter().exit_for_code()?;
        Ok(())
    }

    /// Opens a `switch`.
    pub fn enter_switch_code(&mut self, node: Option<&Rc<Node>>) -> Result<(), CodegenError> {
        self.emitter().enter_switch_code(node)?;
        Ok(())
    }

    /// Closes a `switch`.
    pub fn exit_switch_code(&mut self) -> Result<(), CodegenError> {
        self.emitter().exit_switch_code()?;
        Ok(())
    }

    /// The code of a `case` or a `default`.
    pub fn process_case_code(&mut self, node: Option<&Rc<Node>>) -> Result<(), CodegenError> {
        self.emitter().process_case_code(node)?;
        Ok(())
    }

    /// Opens a `with`.
    pub fn enter_with_code(&mut self, node: Option<&Rc<Node>>) -> Result<(), CodegenError> {
        self.emitter().enter_with_code(node)?;
        Ok(())
    }

    /// Closes a `with`.
    pub fn exit_with_code(&mut self) -> Result<(), CodegenError> {
        self.emitter().exit_with_code()?;
        Ok(())
    }

    /// Opens a block.
    pub fn enter_block(&mut self) {
        self.emitter().enter_block();
    }

    /// Closes a block.
    pub fn exit_block(&mut self) {
        self.emitter().exit_block();
    }

    /// A declared argument of a function.
    /// The declaration of a function's argument.
    pub fn add_function_decl_arg(
        &mut self,
        name: &str,
        node: Option<&Rc<Node>>,
    ) -> Result<(), CodegenError> {
        self.emitter().add_function_decl_arg(name, node)?;
        Ok(())
    }

    /// The collapsed argument of a function.
    /// The declaration of a function's collapsed argument.
    pub fn add_function_decl_arg_collapse(&mut self, name: Option<&str>) {
        self.emitter().add_function_decl_arg_collapse(name);
    }

    /// The argument of a property setter.
    /// The declaration of a property setter's argument.
    pub fn set_property_decl_arg(&mut self, name: &str) {
        self.emitter().set_property_decl_arg(name);
    }

    /// The code of the third clause of a `for` loop.
    /// The code of the third clause of a `for`.
    pub fn set_for_third_expr_code(&mut self, node: Option<&Rc<Node>>) -> Result<(), CodegenError> {
        self.emitter().set_for_third_expr_code(node);
        Ok(())
    }

    /// Opens the `catch` of a `try`.
    /// Opens the `catch` of a `try`.
    pub fn enter_catch_code(&mut self, name: Option<&str>) -> Result<(), CodegenError> {
        self.emitter().enter_catch_code(name)?;
        Ok(())
    }

    /// Closes a `try`.
    pub fn exit_try_code(&mut self) -> Result<(), CodegenError> {
        self.emitter().exit_try_code()?;
        Ok(())
    }

    /// Opens a `try`.
    pub fn enter_try_code(&mut self) {
        self.emitter().enter_try_code();
    }

    /// The code of a `debugger`.
    pub fn do_debugger(&mut self) {
        self.emitter().do_debugger();
    }

    /// The code of a `throw`.
    /// The code of a `throw`.
    pub fn process_throw_code(&mut self, node: Option<&Rc<Node>>) -> Result<(), CodegenError> {
        self.emitter().process_throw_code(node)?;
        Ok(())
    }

    /// The code of a `break`.
    pub fn do_break(&mut self) -> Result<(), CodegenError> {
        self.emitter().do_break()?;
        Ok(())
    }

    /// The code of a `continue`.
    pub fn do_continue(&mut self) -> Result<(), CodegenError> {
        self.emitter().do_continue()?;
        Ok(())
    }

    /// The code of a `return`.
    /// The code of a `return`.
    pub fn return_from_func(&mut self, node: Option<&Rc<Node>>) -> Result<(), CodegenError> {
        self.emitter().return_from_func(node)?;
        Ok(())
    }

    /// The code of the super class of a class.
    /// The code of the super class of a class.
    pub fn create_extends_expr_code(
        &mut self,
        node: Option<&Rc<Node>>,
        hold: bool,
    ) -> Result<(), CodegenError> {
        self.emitter().create_extends_expr_code(node, hold)?;
        Ok(())
    }

    /// Clears the frame registers above the given base.
    pub fn clear_frame(&mut self, frame: &mut i32, base: i32) {
        let base = if base == -1 {
            self.current().frame_base
        } else {
            base
        };
        let frame_minus_one = *frame - 1;
        let max_frame_count = self.current().max_frame_count;
        if frame_minus_one > max_frame_count {
            self.current_mut().max_frame_count = frame_minus_one;
        }
        if *frame - base >= 3 {
            *frame = base;
        } else {
            while *frame > base {
                *frame -= 1;
            }
        }
    }
}
