//! The code of the super class of a class.
//!
//! A **proxy context** is made for the super class: a context of its
//! own, whose parent is the class, and whose name is the class's name.
//! The proxy holds the code that reads the super class. The class's own
//! code is not affected by it -- the proxy has its own frame -- and the
//! proxy is never popped, so it is never compared with the Rust side's
//! own contexts.

use std::rc::Rc;

use crate::vm::codegen::context::ContextType;
use crate::vm::codegen::emit::{EmitError, RT_NEEDED, SubParam};
use crate::vm::codegen::node::Node;
use crate::vm::codegen::opcode::{self, vm};

impl crate::vm::codegen::emit::Emitter<'_> {
    /// The code of the super class of a class.
    ///
    /// The proxy context is made here: after the class's function
    /// registration point is set, and before the class's body is made.
    pub fn create_extends_expr_code(
        &mut self,
        node: Option<&Rc<Node>>,
        hold: bool,
    ) -> Result<(), EmitError> {
        let node_pos = node.map(|node| node.position).unwrap_or(-1);
        let mut frame = self.context().frame_base;
        let resaddr = self.gen_node_code(&mut frame, node, RT_NEEDED, SubParam::default())?;

        self.context_mut().put_code(vm::CHGTHIS, node_pos);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(resaddr), node_pos);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(-1), node_pos);

        self.context_mut().put_code(vm::CALL, node_pos);
        self.context_mut().put_code(0, node_pos);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(resaddr), node_pos);
        self.context_mut().put_code(0, node_pos);

        if hold {
            let node = node.cloned();
            self.context_mut().super_class_expr = node;
        }

        // the function registration point moves
        let size = self.context().code_size();
        self.context_mut().function_register_code_point = size;

        // the proxy context is made once, and its parent is the class
        let class = self.current;
        let getter = match self.super_class_getter(class) {
            Some(getter) => getter,
            None => {
                let name = self.context().name_text().unwrap_or("").to_string();
                let name: Vec<u16> = name.encode_utf16().collect();
                let getter = self.push_context(Some(&name), ContextType::SuperClassGetter);
                // the class holds the proxy: the class makes the proxy once
                // and keeps it
                self.contexts[class].super_class_getter = Some(getter);
                getter
            }
        };

        // the proxy's code is made in the proxy's own context
        self.current = getter;
        let result = self.create_extends_expr_proxy_code(node, node_pos);
        self.current = class;
        result
    }

    /// The code of the proxy that reads the super class.
    pub fn create_extends_expr_proxy_code(
        &mut self,
        node: Option<&Rc<Node>>,
        node_pos: i32,
    ) -> Result<(), EmitError> {
        let size = self.context().code_size();
        self.context_mut()
            .super_class_getter_pointer_mut()
            .push(size);

        let mut frame = self.context().frame_base;
        let resaddr = self.gen_node_code(&mut frame, node, RT_NEEDED, SubParam::default())?;
        self.context_mut().put_code(vm::SRV, -1);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(resaddr), -1);
        self.clear_frame(&mut frame, self.context().frame_base);
        self.context_mut().put_code(vm::RET, -1);
        self.context_mut().put_code(vm::NOP, node_pos);
        Ok(())
    }
}
