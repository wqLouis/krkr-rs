//! The generator's side of the parser's contract.
//!
//! The parser hands the generator the commands of the grammar; the
//! generator makes the intermediate code. The contract is the
//! `Commands` trait of the parser module, and this file implements it for
//! the generator.
//!
//! The trait and the generator are one crate's two halves, so the
//! generator's own methods are the ones the trait names: the wrappers
//! here are the commands the grammar drives, and they are the same
//! commands the proof seam drives through the C interface.

use std::rc::Rc;

use crate::vm::codegen::context::ContextType;
use crate::vm::codegen::node::Node;
use crate::vm::codegen::{CodegenError, Generator};
use crate::vm::parser::Commands;

/// The generator is the commands the parser drives.
impl Commands for Generator {
    /// The code of an expression that stands on its own.
    fn create_expr_code(&mut self, node: &Rc<Node>) -> Result<(), CodegenError> {
        Generator::create_expr_code(self, node)
    }

    /// A local variable of the context.
    fn add_local_variable(&mut self, name: &str, init: i32) {
        Generator::add_local_variable(self, name, init);
    }

    /// A local variable of the context, with its first value.
    fn init_local_variable(&mut self, name: &str, node: &Rc<Node>) -> Result<(), CodegenError> {
        Generator::init_local_variable(self, name, node)
    }

    /// A declared argument of a function.
    fn add_function_decl_arg(
        &mut self,
        name: &str,
        node: Option<&Rc<Node>>,
    ) -> Result<(), CodegenError> {
        Generator::add_function_decl_arg(self, name, node)
    }

    /// The collapsed argument of a function.
    fn add_function_decl_arg_collapse(&mut self, name: Option<&str>) {
        Generator::add_function_decl_arg_collapse(self, name);
    }

    /// The argument of a property setter.
    fn set_property_decl_arg(&mut self, name: &str) {
        Generator::set_property_decl_arg(self, name);
    }

    /// Opens a code context.
    fn push_context(&mut self, name: Option<&[u16]>, kind: ContextType) -> usize {
        Generator::push_context(self, name, kind)
    }

    /// Closes the code context the code is generated for.
    fn pop_context(&mut self) -> Result<(), CodegenError> {
        Generator::pop_context(self)
    }

    /// Opens an `if`.
    fn enter_if_code(&mut self) {
        Generator::enter_if_code(self);
    }

    /// The code of the condition of an `if`.
    fn create_if_expr_code(&mut self, node: Option<&Rc<Node>>) -> Result<(), CodegenError> {
        Generator::create_if_expr_code(self, node)
    }

    /// Closes an `if`.
    fn exit_if_code(&mut self) -> Result<(), CodegenError> {
        Generator::exit_if_code(self)
    }

    /// Opens an `else`.
    fn enter_else_code(&mut self) {
        Generator::enter_else_code(self);
    }

    /// Closes an `else`.
    fn exit_else_code(&mut self) -> Result<(), CodegenError> {
        Generator::exit_else_code(self)
    }

    /// Opens a `while` loop.
    fn enter_while_code(&mut self, do_while: bool) {
        Generator::enter_while_code(self, do_while);
    }

    /// The code of the condition of a `while` loop.
    fn create_while_expr_code(
        &mut self,
        node: Option<&Rc<Node>>,
        do_while: bool,
    ) -> Result<(), CodegenError> {
        Generator::create_while_expr_code(self, node, do_while)
    }

    /// Closes a `while` loop.
    fn exit_while_code(&mut self, do_while: bool) -> Result<(), CodegenError> {
        Generator::exit_while_code(self, do_while)
    }

    /// Opens a `for` loop.
    fn enter_for_code(&mut self) {
        Generator::enter_for_code(self);
    }

    /// The code of the condition of a `for` loop.
    fn create_for_expr_code(&mut self, node: Option<&Rc<Node>>) -> Result<(), CodegenError> {
        Generator::create_for_expr_code(self, node)
    }

    /// The code of the third clause of a `for` loop.
    fn set_for_third_expr_code(&mut self, node: Option<&Rc<Node>>) -> Result<(), CodegenError> {
        Generator::set_for_third_expr_code(self, node)
    }

    /// Closes a `for` loop.
    fn exit_for_code(&mut self) -> Result<(), CodegenError> {
        Generator::exit_for_code(self)
    }

    /// Opens a `switch`.
    fn enter_switch_code(&mut self, node: Option<&Rc<Node>>) -> Result<(), CodegenError> {
        Generator::enter_switch_code(self, node)
    }

    /// Closes a `switch`.
    fn exit_switch_code(&mut self) -> Result<(), CodegenError> {
        Generator::exit_switch_code(self)
    }

    /// The code of a `case` or of a `default`.
    fn process_case_code(&mut self, node: Option<&Rc<Node>>) -> Result<(), CodegenError> {
        Generator::process_case_code(self, node)
    }

    /// Opens a `with`.
    fn enter_with_code(&mut self, node: Option<&Rc<Node>>) -> Result<(), CodegenError> {
        Generator::enter_with_code(self, node)
    }

    /// Closes a `with`.
    fn exit_with_code(&mut self) -> Result<(), CodegenError> {
        Generator::exit_with_code(self)
    }

    /// Opens a `try`.
    fn enter_try_code(&mut self) {
        Generator::enter_try_code(self);
    }

    /// Opens the `catch` of a `try`.
    fn enter_catch_code(&mut self, name: Option<&str>) -> Result<(), CodegenError> {
        Generator::enter_catch_code(self, name)
    }

    /// Closes a `try`.
    fn exit_try_code(&mut self) -> Result<(), CodegenError> {
        Generator::exit_try_code(self)
    }

    /// The code of a `throw`.
    fn process_throw_code(&mut self, node: Option<&Rc<Node>>) -> Result<(), CodegenError> {
        Generator::process_throw_code(self, node)
    }

    /// The code of a `break`.
    fn do_break(&mut self) -> Result<(), CodegenError> {
        Generator::do_break(self)
    }

    /// The code of a `continue`.
    fn do_continue(&mut self) -> Result<(), CodegenError> {
        Generator::do_continue(self)
    }

    /// The code of a `debugger`.
    fn do_debugger(&mut self) {
        Generator::do_debugger(self);
    }

    /// The code of a `return`.
    fn return_from_func(&mut self, node: Option<&Rc<Node>>) -> Result<(), CodegenError> {
        Generator::return_from_func(self, node)
    }

    /// The code of the super class of a class.
    fn create_extends_expr_code(
        &mut self,
        node: Option<&Rc<Node>>,
        hold: bool,
    ) -> Result<(), CodegenError> {
        Generator::create_extends_expr_code(self, node, hold)
    }

    /// Opens a block.
    fn enter_block(&mut self) {
        Generator::enter_block(self);
    }

    /// Closes a block.
    fn exit_block(&mut self) {
        Generator::exit_block(self);
    }
}

/// The generator itself is the commands the parser drives.
///
/// The parser holds the generator it drives, so the commands reach it
/// through it.
impl Commands for &mut Generator {
    /// The code of an expression that stands on its own.
    fn create_expr_code(&mut self, node: &Rc<Node>) -> Result<(), CodegenError> {
        (**self).create_expr_code(node)
    }

    /// A local variable of the context.
    fn add_local_variable(&mut self, name: &str, init: i32) {
        (**self).add_local_variable(name, init);
    }

    /// A local variable of the context, with its first value.
    fn init_local_variable(&mut self, name: &str, node: &Rc<Node>) -> Result<(), CodegenError> {
        (**self).init_local_variable(name, node)
    }

    /// A declared argument of a function.
    fn add_function_decl_arg(
        &mut self,
        name: &str,
        node: Option<&Rc<Node>>,
    ) -> Result<(), CodegenError> {
        (**self).add_function_decl_arg(name, node)
    }

    /// The collapsed argument of a function.
    fn add_function_decl_arg_collapse(&mut self, name: Option<&str>) {
        (**self).add_function_decl_arg_collapse(name);
    }

    /// The argument of a property setter.
    fn set_property_decl_arg(&mut self, name: &str) {
        (**self).set_property_decl_arg(name);
    }

    /// Opens a code context.
    fn push_context(&mut self, name: Option<&[u16]>, kind: ContextType) -> usize {
        (**self).push_context(name, kind)
    }

    /// Closes the code context the code is generated for.
    fn pop_context(&mut self) -> Result<(), CodegenError> {
        (**self).pop_context()
    }

    /// Opens an `if`.
    fn enter_if_code(&mut self) {
        (**self).enter_if_code();
    }

    /// The code of the condition of an `if`.
    fn create_if_expr_code(&mut self, node: Option<&Rc<Node>>) -> Result<(), CodegenError> {
        (**self).create_if_expr_code(node)
    }

    /// Closes an `if`.
    fn exit_if_code(&mut self) -> Result<(), CodegenError> {
        (**self).exit_if_code()
    }

    /// Opens an `else`.
    fn enter_else_code(&mut self) {
        (**self).enter_else_code();
    }

    /// Closes an `else`.
    fn exit_else_code(&mut self) -> Result<(), CodegenError> {
        (**self).exit_else_code()
    }

    /// Opens a `while` loop.
    fn enter_while_code(&mut self, do_while: bool) {
        (**self).enter_while_code(do_while);
    }

    /// The code of the condition of a `while` loop.
    fn create_while_expr_code(
        &mut self,
        node: Option<&Rc<Node>>,
        do_while: bool,
    ) -> Result<(), CodegenError> {
        (**self).create_while_expr_code(node, do_while)
    }

    /// Closes a `while` loop.
    fn exit_while_code(&mut self, do_while: bool) -> Result<(), CodegenError> {
        (**self).exit_while_code(do_while)
    }

    /// Opens a `for` loop.
    fn enter_for_code(&mut self) {
        (**self).enter_for_code();
    }

    /// The code of the condition of a `for` loop.
    fn create_for_expr_code(&mut self, node: Option<&Rc<Node>>) -> Result<(), CodegenError> {
        (**self).create_for_expr_code(node)
    }

    /// The code of the third clause of a `for` loop.
    fn set_for_third_expr_code(&mut self, node: Option<&Rc<Node>>) -> Result<(), CodegenError> {
        (**self).set_for_third_expr_code(node)
    }

    /// Closes a `for` loop.
    fn exit_for_code(&mut self) -> Result<(), CodegenError> {
        (**self).exit_for_code()
    }

    /// Opens a `switch`.
    fn enter_switch_code(&mut self, node: Option<&Rc<Node>>) -> Result<(), CodegenError> {
        (**self).enter_switch_code(node)
    }

    /// Closes a `switch`.
    fn exit_switch_code(&mut self) -> Result<(), CodegenError> {
        (**self).exit_switch_code()
    }

    /// The code of a `case` or of a `default`.
    fn process_case_code(&mut self, node: Option<&Rc<Node>>) -> Result<(), CodegenError> {
        (**self).process_case_code(node)
    }

    /// Opens a `with`.
    fn enter_with_code(&mut self, node: Option<&Rc<Node>>) -> Result<(), CodegenError> {
        (**self).enter_with_code(node)
    }

    /// Closes a `with`.
    fn exit_with_code(&mut self) -> Result<(), CodegenError> {
        (**self).exit_with_code()
    }

    /// Opens a `try`.
    fn enter_try_code(&mut self) {
        (**self).enter_try_code();
    }

    /// Opens the `catch` of a `try`.
    fn enter_catch_code(&mut self, name: Option<&str>) -> Result<(), CodegenError> {
        (**self).enter_catch_code(name)
    }

    /// Closes a `try`.
    fn exit_try_code(&mut self) -> Result<(), CodegenError> {
        (**self).exit_try_code()
    }

    /// The code of a `throw`.
    fn process_throw_code(&mut self, node: Option<&Rc<Node>>) -> Result<(), CodegenError> {
        (**self).process_throw_code(node)
    }

    /// The code of a `break`.
    fn do_break(&mut self) -> Result<(), CodegenError> {
        (**self).do_break()
    }

    /// The code of a `continue`.
    fn do_continue(&mut self) -> Result<(), CodegenError> {
        (**self).do_continue()
    }

    /// The code of a `debugger`.
    fn do_debugger(&mut self) {
        (**self).do_debugger();
    }

    /// The code of a `return`.
    fn return_from_func(&mut self, node: Option<&Rc<Node>>) -> Result<(), CodegenError> {
        (**self).return_from_func(node)
    }

    /// The code of the super class of a class.
    fn create_extends_expr_code(
        &mut self,
        node: Option<&Rc<Node>>,
        hold: bool,
    ) -> Result<(), CodegenError> {
        (**self).create_extends_expr_code(node, hold)
    }

    /// Opens a block.
    fn enter_block(&mut self) {
        (**self).enter_block();
    }

    /// Closes a block.
    fn exit_block(&mut self) {
        (**self).exit_block();
    }
}
