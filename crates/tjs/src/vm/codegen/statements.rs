//! The emission of the statements.
//!
//! These are the commands the grammar drives: the `if`, the `while`,
//! the `for`, the `switch`, the `with`, the `try` and the jumps. Each
//! command opens, fills or closes a nest.

use std::rc::Rc;

use crate::vm::codegen::context::NestKind;
use crate::vm::codegen::emit::{
    EmitError, GNC_CFLAG_I, RT_CFLAG, RT_NEEDED, SubParam, is_cond_flag,
};
use crate::vm::codegen::node::Node;
use crate::vm::codegen::opcode::{self, vm};

/// Whether the nest at the given place is of the given kind.
fn nest_is(context: &crate::vm::codegen::context::Context, index: usize, kind: NestKind) -> bool {
    context.nest_at(index).kind == kind
}

impl crate::vm::codegen::emit::Emitter<'_> {
    /// The code of the condition of a statement.
    ///
    /// The condition is emitted, and the jump that leaves the statement
    /// when the condition is false is emitted with it. The result says
    /// whether the jump is taken when the condition is **false**.
    fn gen_condition(&mut self, node: Option<&Rc<Node>>) -> Result<(bool, i32), EmitError> {
        let mut frame = self.context().frame_base;
        let node_pos = node.map(|node| node.position).unwrap_or(-1);
        let resaddr =
            self.gen_node_code(&mut frame, node, RT_NEEDED | RT_CFLAG, SubParam::default())?;
        let mut inverted = false;
        if !is_cond_flag(resaddr) {
            self.context_mut().put_code(vm::TT, node_pos);
            self.context_mut()
                .put_code(opcode::vm_reg_addr(resaddr), node_pos);
        } else if resaddr == GNC_CFLAG_I {
            inverted = true;
        }
        self.clear_frame(&mut frame, self.context().frame_base);
        Ok((inverted, node_pos))
    }

    /// Opens a `while` loop.
    ///
    /// A `while` loop and a `do` loop hold the same nest: the
    /// difference between them is in the place of the condition, and
    /// the condition's code reads it.
    pub fn enter_while_code(&mut self, _do_while: bool) {
        let start = self.context().code_size();
        self.context_mut().push_nest(NestKind::While);
        let index = self.context().nest_len() - 1;
        self.context_mut().nest_data_mut(index).loop_start_ip = start;
    }

    /// The code of the condition of a `while` loop.
    pub fn create_while_expr_code(
        &mut self,
        node: Option<&Rc<Node>>,
        do_while: bool,
    ) -> Result<(), EmitError> {
        let index = self.context().nest_len() - 1;
        if do_while {
            self.do_continue_patch(index);
        }
        let (inverted, node_pos) = self.gen_condition(node)?;
        if !do_while {
            let size = self.context().code_size();
            self.context_mut()
                .nest_data_mut(index)
                .exit_patches
                .push(size);
            self.context_mut().add_jump();
            self.context_mut()
                .put_code(if inverted { vm::JF } else { vm::JNF }, node_pos);
            self.context_mut().put_code(0, node_pos);
        } else {
            let jump_ip = self.context().code_size();
            let start = self.context().nest_at(index).loop_start_ip;
            self.context_mut().add_jump();
            self.context_mut()
                .put_code(if inverted { vm::JNF } else { vm::JF }, node_pos);
            self.context_mut().put_code(start - jump_ip, node_pos);
        }
        Ok(())
    }

    /// Closes a `while` loop.
    pub fn exit_while_code(&mut self, do_while: bool) -> Result<(), EmitError> {
        if self.context().nest_len() == 0 {
            return Err(EmitError::Shape("the 'while' is not open".to_string()));
        }
        let index = self.context().nest_len() - 1;
        if !nest_is(self.context(), index, NestKind::While) {
            return Err(EmitError::Shape("the 'while' is not open".to_string()));
        }
        if !do_while {
            let jump_ip = self.context().code_size();
            let start = self.context().nest_at(index).loop_start_ip;
            self.context_mut().add_jump();
            self.context_mut().put_code(vm::JMP, -1);
            self.context_mut().put_code(start - jump_ip, -1);
        }
        self.do_nest_top_exit_patch(index);
        self.context_mut().pop_nest();
        Ok(())
    }

    /// Opens an `if`.
    pub fn enter_if_code(&mut self) {
        self.context_mut().push_nest(NestKind::Block);
        let index = self.context().nest_len() - 1;
        self.context_mut().nest_data_mut(index).kind = NestKind::Block;
    }

    /// The code of the condition of an `if`.
    pub fn create_if_expr_code(&mut self, node: Option<&Rc<Node>>) -> Result<(), EmitError> {
        let (inverted, node_pos) = self.gen_condition(node)?;
        let index = self.context().nest_len() - 1;
        let size = self.context().code_size();
        self.context_mut().nest_data_mut(index).patch1 = size;
        self.context_mut().add_jump();
        self.context_mut()
            .put_code(if inverted { vm::JF } else { vm::JNF }, node_pos);
        self.context_mut().put_code(0, node_pos);
        Ok(())
    }

    /// Closes an `if`.
    pub fn exit_if_code(&mut self) -> Result<(), EmitError> {
        if self.context().nest_len() == 0 {
            return Err(EmitError::Shape("the 'if' is not open".to_string()));
        }
        let index = self.context().nest_len() - 1;
        let patch1 = self.context().nest_at(index).patch1;
        let size = self.context().code_size();
        self.context_mut().code_mut()[(patch1 + 1) as usize] = size - patch1;
        self.context_mut().prev_if_exit_patch = patch1;
        self.context_mut().pop_nest();
        Ok(())
    }

    /// Opens an `else`.
    pub fn enter_else_code(&mut self) {
        self.context_mut().push_nest(NestKind::Block);
        let index = self.context().nest_len() - 1;
        self.context_mut().nest_data_mut(index).kind = NestKind::Block;
        let size = self.context().code_size();
        self.context_mut().nest_data_mut(index).patch2 = size;
        self.context_mut().add_jump();
        self.context_mut().put_code(vm::JMP, -1);
        self.context_mut().put_code(0, -1);
        let previous = self.context().prev_if_exit_patch;
        let size = self.context().code_size();
        self.context_mut().code_mut()[(previous + 1) as usize] = size - previous;
    }

    /// Closes an `else`.
    pub fn exit_else_code(&mut self) -> Result<(), EmitError> {
        if self.context().nest_len() == 0 {
            return Err(EmitError::Shape("the 'else' is not open".to_string()));
        }
        let index = self.context().nest_len() - 1;
        let patch2 = self.context().nest_at(index).patch2;
        let size = self.context().code_size();
        self.context_mut().code_mut()[(patch2 + 1) as usize] = size - patch2;
        self.context_mut().pop_nest();
        Ok(())
    }

    /// Opens a `for`.
    pub fn enter_for_code(&mut self) {
        self.context_mut().push_nest(NestKind::For);
        self.enter_block();
    }

    /// The code of the condition of a `for`.
    pub fn create_for_expr_code(&mut self, node: Option<&Rc<Node>>) -> Result<(), EmitError> {
        let index = self.context().nest_len() - 1;
        let size = self.context().code_size();
        self.context_mut().nest_data_mut(index).loop_start_ip = size;
        if node.is_some() {
            let (inverted, node_pos) = self.gen_condition(node)?;
            let size = self.context().code_size();
            self.context_mut()
                .nest_data_mut(index)
                .exit_patches
                .push(size);
            self.context_mut().add_jump();
            self.context_mut()
                .put_code(if inverted { vm::JF } else { vm::JNF }, node_pos);
            self.context_mut().put_code(0, node_pos);
        }
        Ok(())
    }

    /// The code of the third clause of a `for`.
    ///
    /// The expression is held by the context and made when the loop is
    /// left.
    pub fn set_for_third_expr_code(&mut self, node: Option<&Rc<Node>>) {
        self.context_mut().post_loop_expr = node.cloned();
    }

    /// Closes a `for`.
    pub fn exit_for_code(&mut self) -> Result<(), EmitError> {
        let nest_size = self.context().nest_len();
        if nest_size == 0 {
            return Err(EmitError::Shape("the 'for' is not open".to_string()));
        }
        // the `for` opens two nests: the loop itself and the block that
        // holds its first clause. The loop's head is on the block's
        // nest, and the loop's own nest is below it.
        let block = nest_size - 1;
        let loop_nest = nest_size - 2;

        // the continue patches of the loop
        if self.context().nest_at(block).kind == NestKind::For {
            self.do_continue_patch(block);
        }
        if loop_nest < nest_size && self.context().nest_at(loop_nest).kind == NestKind::For {
            self.do_continue_patch(loop_nest);
        }

        // the third clause runs before the loop jumps back
        if let Some(expression) = self.context().post_loop_expr.clone() {
            let mut frame = self.context().frame_base;
            self.gen_node_code(&mut frame, Some(&expression), 0, SubParam::default())?;
            self.clear_frame(&mut frame, self.context().frame_base);
        }

        // the jump back to the head of the loop
        let start = self.context().nest_at(block).loop_start_ip;
        let jump_ip = self.context().code_size();
        self.context_mut().add_jump();
        self.context_mut().put_code(vm::JMP, -1);
        self.context_mut().put_code(start - jump_ip, -1);

        // the block's exit patches, then the loop's
        self.do_nest_top_exit_patch(block);
        self.exit_block();
        self.do_nest_top_exit_patch(loop_nest);
        self.context_mut().pop_nest();
        Ok(())
    }

    /// Opens a `switch`.
    pub fn enter_switch_code(&mut self, node: Option<&Rc<Node>>) -> Result<(), EmitError> {
        self.context_mut().push_nest(NestKind::Switch);
        let index = self.context().nest_len() - 1;
        {
            let nest = self.context_mut().nest_data_mut(index);
            nest.patch1 = -1;
            nest.patch2 = -1;
            nest.is_first_case = true;
        }

        let mut frame = self.context().frame_base;
        let node_pos = node.map(|node| node.position).unwrap_or(-1);
        let resaddr = self.gen_node_code(&mut frame, node, RT_NEEDED, SubParam::default())?;
        let frame_base = self.context().frame_base;
        if frame_base != resaddr {
            self.context_mut().put_code(vm::CP, node_pos);
            self.context_mut()
                .put_code(opcode::vm_reg_addr(frame_base), node_pos);
            self.context_mut()
                .put_code(opcode::vm_reg_addr(resaddr), node_pos);
        }
        let index = self.context().nest_len() - 1;
        self.context_mut().nest_data_mut(index).ref_register = frame_base;

        if frame - 1 > self.context().max_frame_count {
            self.context_mut().max_frame_count = frame - 1;
        }
        self.context_mut().frame_base += 1;
        if self.context().frame_base - 1 > self.context().max_frame_count {
            self.context_mut().max_frame_count = self.context().frame_base - 1;
        }
        self.clear_frame(&mut frame, frame_base);
        self.enter_block();
        Ok(())
    }

    /// Closes a `switch`.
    pub fn exit_switch_code(&mut self) -> Result<(), EmitError> {
        self.exit_block();
        if self.context().nest_len() == 0 {
            return Err(EmitError::Shape("the 'switch' is not open".to_string()));
        }
        let index = self.context().nest_len() - 1;
        let is_first_case = self.context().nest_at(index).is_first_case;
        let patch1 = self.context().nest_at(index).patch1;
        let patch2 = self.context().nest_at(index).patch2;
        let mut patch3 = -1;
        if !is_first_case {
            patch3 = self.context().code_size();
            self.context_mut().add_jump();
            self.context_mut().put_code(vm::JMP, -1);
            self.context_mut().put_code(0, -1);
        }
        if patch1 != -1 {
            let size = self.context().code_size();
            self.context_mut().code_mut()[(patch1 + 1) as usize] = size - patch1;
        }
        if patch2 != -1 {
            self.context_mut().add_jump();
            let jump_start = self.context().code_size();
            self.context_mut().put_code(vm::JMP, -1);
            self.context_mut().put_code(patch2 - jump_start, -1);
        }
        if !is_first_case {
            let size = self.context().code_size();
            self.context_mut().code_mut()[(patch3 + 1) as usize] = size - patch3;
        }
        self.do_nest_top_exit_patch(index);
        self.context_mut().frame_base -= 1;
        self.context_mut().pop_nest();
        Ok(())
    }

    /// The code of a `case` or a `default`.
    ///
    /// The `case` and the `default` sit in a block that sits in a block
    /// that sits in the `switch`; the stack must be that shape.
    pub fn process_case_code(&mut self, node: Option<&Rc<Node>>) -> Result<(), EmitError> {
        let nest_size = self.context().nest_len();
        if nest_size < 3 {
            return Err(EmitError::Shape("the 'case' is misplaced".to_string()));
        }
        if !nest_is(self.context(), nest_size - 1, NestKind::Block)
            || !nest_is(self.context(), nest_size - 2, NestKind::Block)
            || !nest_is(self.context(), nest_size - 3, NestKind::Switch)
        {
            return Err(EmitError::Shape("the 'case' is misplaced".to_string()));
        }

        // the switch is three nests below the top
        let switch = nest_size - 3;
        let nest = self.context().nest_at(switch);
        let mut patch3 = -1;
        if !nest.is_first_case {
            patch3 = self.context().code_size();
            self.context_mut().add_jump();
            self.context_mut().put_code(vm::JMP, -1);
            self.context_mut().put_code(0, -1);
        }

        self.exit_block();
        let patch1 = self.context().nest_at(switch).patch1;
        if patch1 != -1 {
            let size = self.context().code_size();
            self.context_mut().code_mut()[(patch1 + 1) as usize] = size - patch1;
        }

        match node {
            Some(node) => {
                let mut frame = self.context().frame_base;
                let node_pos = node.position;
                let resaddr =
                    self.gen_node_code(&mut frame, Some(node), RT_NEEDED, SubParam::default())?;
                let reference = self.context().nest_at(switch).ref_register;
                self.context_mut().put_code(vm::CEQ, node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(reference), node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(resaddr), node_pos);
                self.clear_frame(&mut frame, self.context().frame_base);
                let size = self.context().code_size();
                self.context_mut().nest_data_mut(switch).patch1 = size;
                self.context_mut().add_jump();
                self.context_mut().put_code(vm::JNF, node_pos);
                self.context_mut().put_code(0, node_pos);
            }
            None => {
                // the default case: the jump to it is patched after the
                // jump instruction has been emitted
                let size = self.context().code_size();
                self.context_mut().nest_data_mut(switch).patch1 = size;
                self.context_mut().add_jump();
                self.context_mut().put_code(vm::JMP, -1);
                self.context_mut().put_code(0, -1);
                let size = self.context().code_size();
                self.context_mut().nest_data_mut(switch).patch2 = size;
            }
        }

        if patch3 != -1 {
            let size = self.context().code_size();
            self.context_mut().code_mut()[(patch3 + 1) as usize] = size - patch3;
        }
        self.context_mut().nest_data_mut(switch).is_first_case = false;

        self.enter_block();
        Ok(())
    }

    /// Opens a `with`.
    pub fn enter_with_code(&mut self, node: Option<&Rc<Node>>) -> Result<(), EmitError> {
        let mut frame = self.context().frame_base;
        let node_pos = node.map(|node| node.position).unwrap_or(-1);
        let resaddr = self.gen_node_code(&mut frame, node, RT_NEEDED, SubParam::default())?;
        let frame_base = self.context().frame_base;
        if frame_base != resaddr {
            self.context_mut().put_code(vm::CP, node_pos);
            self.context_mut()
                .put_code(opcode::vm_reg_addr(frame_base), node_pos);
            self.context_mut()
                .put_code(opcode::vm_reg_addr(resaddr), node_pos);
        }

        self.context_mut().push_nest(NestKind::With);
        let index = self.context().nest_len() - 1;
        self.context_mut().nest_data_mut(index).ref_register = frame_base;

        if frame - 1 > self.context().max_frame_count {
            self.context_mut().max_frame_count = frame - 1;
        }
        self.context_mut().frame_base += 1;
        if self.context().frame_base - 1 > self.context().max_frame_count {
            self.context_mut().max_frame_count = self.context().frame_base - 1;
        }
        self.clear_frame(&mut frame, frame_base);
        self.enter_block();
        Ok(())
    }

    /// Closes a `with`.
    pub fn exit_with_code(&mut self) -> Result<(), EmitError> {
        self.exit_block();
        if self.context().nest_len() == 0 {
            return Err(EmitError::Shape("the 'with' is not open".to_string()));
        }
        let index = self.context().nest_len() - 1;
        if !nest_is(self.context(), index, NestKind::With) {
            return Err(EmitError::Shape("the 'with' is not open".to_string()));
        }
        self.context_mut().frame_base -= 1;
        self.context_mut().pop_nest();
        Ok(())
    }

    /// Opens a block.
    pub fn enter_block(&mut self) {
        self.context_mut().namespace.push();
        self.context_mut().push_nest(NestKind::Block);
    }

    /// Closes a block.
    pub fn exit_block(&mut self) {
        if self.context().nest_len() == 0 {
            return;
        }
        self.context_mut().pop_nest();
        self.context_mut().namespace.pop();
    }

    /// The code of a `break`.
    pub fn do_break(&mut self) -> Result<(), EmitError> {
        let nests = self.context().nest_len();
        for index in (0..nests).rev() {
            let nest = self.context().nest_at(index);
            match nest.kind {
                NestKind::Switch | NestKind::While | NestKind::For => {
                    // the jump leaves the statement
                    let size = self.context().code_size();
                    self.context_mut()
                        .nest_data_mut(index)
                        .exit_patches
                        .push(size);
                    self.context_mut().add_jump();
                    self.context_mut().put_code(vm::JMP, -1);
                    self.context_mut().put_code(0, -1);
                    return Ok(());
                }
                NestKind::Try => {
                    self.context_mut().put_code(vm::EXTRY, -1);
                }
                _ => {}
            }
        }
        Err(EmitError::Shape(
            "the 'break' is not inside a loop or a switch".to_string(),
        ))
    }

    /// The code of a `continue`.
    pub fn do_continue(&mut self) -> Result<(), EmitError> {
        let nests = self.context().nest_len();
        for index in (0..nests).rev() {
            let nest = self.context().nest_at(index);
            match nest.kind {
                NestKind::While => {
                    // the jump goes back to the condition
                    let jump_start = self.context().code_size();
                    let start = nest.loop_start_ip;
                    self.context_mut().add_jump();
                    self.context_mut().put_code(vm::JMP, -1);
                    self.context_mut().put_code(start - jump_start, -1);
                    return Ok(());
                }
                NestKind::For => {
                    // the jump goes to the end of the loop, where the
                    // third clause is
                    let size = self.context().code_size();
                    self.context_mut()
                        .nest_data_mut(index)
                        .exit_patches
                        .push(size);
                    self.context_mut().add_jump();
                    self.context_mut().put_code(vm::JMP, -1);
                    self.context_mut().put_code(0, -1);
                    return Ok(());
                }
                NestKind::Try => {
                    self.context_mut().put_code(vm::EXTRY, -1);
                }
                _ => {}
            }
        }
        Err(EmitError::Shape(
            "the 'continue' is not inside a loop".to_string(),
        ))
    }

    /// The code of a `debugger` statement.
    pub fn do_debugger(&mut self) {
        self.context_mut().put_code(vm::DEBUGGER, -1);
    }

    /// The code of a `return`.
    pub fn return_from_func(&mut self, node: Option<&Rc<Node>>) -> Result<(), EmitError> {
        let node_pos = node.map(|node| node.position).unwrap_or(-1);
        match node {
            None => {
                self.context_mut().put_code(vm::SRV, node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(0), node_pos);
            }
            Some(node) => {
                let mut frame = self.context().frame_base;
                let resaddr =
                    self.gen_node_code(&mut frame, Some(node), RT_NEEDED, SubParam::default())?;
                self.context_mut().put_code(vm::SRV, node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(resaddr), node_pos);
                self.clear_frame(&mut frame, self.context().frame_base);
            }
        }

        // the frame is cleared
        let frame_base = self.context().frame_base;
        let mut frame = frame_base;
        self.clear_frame(&mut frame, 1);

        // the local variables are cleared
        let count = self.context().namespace.count();
        self.clear_local_variables(count);

        // the try blocks are left
        let nests = self.context().nest_len();
        for index in (0..nests).rev() {
            if self.context().nest_at(index).kind == NestKind::Try {
                self.context_mut().put_code(vm::EXTRY, -1);
            }
        }
        self.context_mut().put_code(vm::RET, node_pos);
        Ok(())
    }

    /// The code of a `throw`.
    pub fn process_throw_code(&mut self, node: Option<&Rc<Node>>) -> Result<(), EmitError> {
        let node_pos = node.map(|node| node.position).unwrap_or(-1);
        let mut frame = self.context().frame_base;
        let resaddr = self.gen_node_code(&mut frame, node, RT_NEEDED, SubParam::default())?;
        self.context_mut().put_code(vm::THROW, node_pos);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(resaddr), node_pos);
        if frame - 1 > self.context().max_frame_count {
            self.context_mut().max_frame_count = frame - 1;
        }
        Ok(())
    }

    /// Opens a `try`.
    pub fn enter_try_code(&mut self) {
        self.context_mut().push_nest(NestKind::Try);
        let index = self.context().nest_len() - 1;
        let size = self.context().code_size();
        self.context_mut().nest_data_mut(index).patch1 = size;
        self.context_mut().add_jump();
        self.context_mut().put_code(vm::ENTRY, -1);
        self.context_mut().put_code(0, -1);
        let frame_base = self.context().frame_base;
        self.context_mut()
            .put_code(opcode::vm_reg_addr(frame_base), -1);
        if frame_base > self.context().max_frame_count {
            self.context_mut().max_frame_count = frame_base;
        }
    }

    /// Opens the `catch` of a `try`.
    pub fn enter_catch_code(&mut self, name: Option<&str>) -> Result<(), EmitError> {
        self.context_mut().put_code(vm::EXTRY, -1);
        let index = self.context().nest_len() - 1;
        let size = self.context().code_size();
        self.context_mut().nest_data_mut(index).patch2 = size;
        self.context_mut().add_jump();
        self.context_mut().put_code(vm::JMP, -1);
        self.context_mut().put_code(0, -1);

        let patch1 = self.context().nest_at(index).patch1;
        let size = self.context().code_size();
        self.context_mut().code_mut()[(patch1 + 1) as usize] = size - patch1;

        // the local variables are cleared
        let max_count = self.context().namespace.max_count;
        let count = self.context().namespace.count();
        self.clear_local_variables(max_count - count);

        // the frame is cleared
        let mut frame = self.context().max_frame_count + 1;
        let frame_base = self.context().frame_base;
        let base = if name.is_some() {
            frame_base + 1
        } else {
            frame_base
        };
        self.clear_frame(&mut frame, base);

        // the nest becomes the catch
        self.context_mut().nest_data_mut(index).kind = NestKind::Catch;

        if let Some(name) = name {
            self.context_mut().nest_data_mut(index).has_variable = true;
            self.enter_block();
            let frame_base = self.context().frame_base;
            self.add_local_variable(name, frame_base);
        }
        Ok(())
    }

    /// Closes a `try`.
    pub fn exit_try_code(&mut self) -> Result<(), EmitError> {
        let nests = self.context().nest_len();
        if nests >= 2 {
            let below = self.context().nest_at(nests - 2);
            if below.kind == NestKind::Catch && below.has_variable {
                self.exit_block();
            }
        }
        if self.context().nest_len() == 0 {
            return Err(EmitError::Shape("the 'try' is not open".to_string()));
        }
        let index = self.context().nest_len() - 1;
        if self.context().nest_at(index).kind != NestKind::Catch {
            return Err(EmitError::Shape("the 'try' is not open".to_string()));
        }
        let patch2 = self.context().nest_at(index).patch2;
        let size = self.context().code_size();
        self.context_mut().code_mut()[(patch2 + 1) as usize] = size - patch2;
        self.context_mut().pop_nest();
        Ok(())
    }

    /// The declaration of a function's argument.
    pub fn add_function_decl_arg(
        &mut self,
        name: &str,
        node: Option<&Rc<Node>>,
    ) -> Result<(), EmitError> {
        self.context_mut().namespace.add(name);
        let node_pos = node.map(|node| node.position).unwrap_or(-1);
        if let Some(node) = node {
            let count = self.context().func_decl_arg_count;
            self.context_mut().put_code(vm::CDEQ, node_pos);
            self.context_mut()
                .put_code(opcode::vm_reg_addr(-3 - count), node_pos);
            self.context_mut()
                .put_code(opcode::vm_reg_addr(0), node_pos);
            let jump_ip = self.context().code_size();
            self.context_mut().add_jump();
            self.context_mut().put_code(vm::JNF, node_pos);
            self.context_mut().put_code(0, node_pos);

            let mut frame = self.context().frame_base;
            let resaddr =
                self.gen_node_code(&mut frame, Some(node), RT_NEEDED, SubParam::default())?;
            self.context_mut().put_code(vm::CP, node_pos);
            self.context_mut()
                .put_code(opcode::vm_reg_addr(-3 - count), node_pos);
            self.context_mut()
                .put_code(opcode::vm_reg_addr(resaddr), node_pos);
            self.clear_frame(&mut frame, self.context().frame_base);

            let size = self.context().code_size();
            self.context_mut().code_mut()[(jump_ip + 1) as usize] = size - jump_ip;
        }
        self.context_mut().func_decl_arg_count += 1;
        Ok(())
    }

    /// The declaration of a function's collapsed argument.
    pub fn add_function_decl_arg_collapse(&mut self, name: Option<&str>) {
        let count = self.context().func_decl_arg_count;
        match name {
            None => {
                // the arguments arrive in an unnamed array
                self.context_mut().func_decl_unnamed_arg_array_base = count;
            }
            Some(name) => {
                // the arguments arrive in a named array
                self.context_mut().func_decl_collapse_base = count;
                self.context_mut().namespace.add(name);
            }
        }
    }

    /// The declaration of a property setter's argument.
    pub fn set_property_decl_arg(&mut self, name: &str) {
        self.context_mut().namespace.add(name);
        self.context_mut().func_decl_arg_count = 1;
    }

    /// Clears the local variables above the given count.
    ///
    /// Nothing is emitted: the registers are cleared by the frame's own
    /// bookkeeping.
    fn clear_local_variables(&mut self, _count: i32) {}

    /// The patches of the nest that are left.
    fn do_nest_top_exit_patch(&mut self, index: usize) {
        let patches = self.context().nest_at(index).exit_patches.clone();
        let size = self.context().code_size();
        for patch in patches {
            self.context_mut().code_mut()[(patch + 1) as usize] = size - patch;
        }
    }

    /// The patches of the loop that are left.
    fn do_continue_patch(&mut self, index: usize) {
        let patches = self.context().nest_at(index).continue_patches.clone();
        let size = self.context().code_size();
        for patch in patches {
            self.context_mut().code_mut()[(patch + 1) as usize] = size - patch;
        }
    }
}
