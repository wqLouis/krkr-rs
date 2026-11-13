//! The emission of the operations that reach into an object.
//!
//! The operations that reach into an object: the member access, the
//! call, the variable, the `this` and the inline arrays and dictionaries.

use std::rc::Rc;

use crate::vm::codegen::context::{ContextType, NestKind};
use crate::vm::codegen::emit::{
    EmitError, RT_NEEDED, SubParam, is_frame, is_modify_sub_type, kind,
};
use crate::vm::codegen::node::Node;
use crate::vm::codegen::opcode::{self, vm};
use crate::vm::codegen::token;
use crate::vm::codegen::value::Value;

/// The error the generator raises when the left hand side cannot be
/// modified.
fn cannot_modify() -> EmitError {
    EmitError::Shape("the left hand side of the operation cannot be modified".to_string())
}

/// The sub-type of the operation that reaches into an object.
fn object_sub_type(opcode: i32) -> i32 {
    match opcode {
        token::T_TYPEOF => kind::TYPEOF,
        token::T_DELETE => kind::DELETE,
        token::T_INCREMENT => kind::PRE_INC,
        token::T_DECREMENT => kind::PRE_DEC,
        token::T_POSTINCREMENT => kind::POST_INC,
        token::T_POSTDECREMENT => kind::POST_DEC,
        _ => kind::NONE,
    }
}

/// Whether the sub-type is one of the arithmetic ones.
fn is_arithmetic_kind(sub_type: i32) -> bool {
    matches!(
        sub_type,
        kind::BIT_AND
            | kind::BIT_OR
            | kind::BIT_XOR
            | kind::SUB
            | kind::ADD
            | kind::MOD
            | kind::DIV
            | kind::IDIV
            | kind::MUL
            | kind::LOG_OR
            | kind::LOG_AND
            | kind::SAR
            | kind::SAL
            | kind::SR
    )
}

impl crate::vm::codegen::emit::Emitter<'_> {
    /// The code of the operations that reach into an object.
    pub fn gen_remaining_code(
        &mut self,
        frame: &mut i32,
        node: &Rc<Node>,
        restype: u32,
        param: SubParam,
    ) -> Result<i32, EmitError> {
        let node_pos = node.position;
        match node.opcode {
            token::T_TYPEOF => {
                if is_modify_sub_type(param.sub_type) {
                    return Err(cannot_modify());
                }
                let child = node.child(0);
                let has_property = child.is_some_and(|child| {
                    matches!(
                        child.opcode,
                        token::T_DOT | token::T_LBRACKET | token::T_WITHDOT
                    )
                });
                if has_property {
                    let param2 = SubParam {
                        sub_type: kind::TYPEOF,
                        sub_address: 0,
                    };
                    return self.gen_node_code(frame, child, RT_NEEDED, param2);
                }
                let mut resaddr =
                    self.gen_node_code(frame, child, RT_NEEDED, SubParam::default())?;
                if !is_frame(resaddr) {
                    self.context_mut().put_code(vm::CP, node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(*frame), node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(resaddr), node_pos);
                    resaddr = *frame;
                    *frame += 1;
                }
                self.context_mut().put_code(vm::TYPEOF, node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(resaddr), node_pos);
                Ok(resaddr)
            }
            token::T_DELETE
            | token::T_INCREMENT
            | token::T_DECREMENT
            | token::T_POSTINCREMENT
            | token::T_POSTDECREMENT => {
                if is_modify_sub_type(param.sub_type) {
                    return Err(cannot_modify());
                }
                let param2 = SubParam {
                    sub_type: object_sub_type(node.opcode),
                    sub_address: 0,
                };
                self.gen_node_code(frame, node.child(0), restype, param2)
            }
            token::T_LPARENTHESIS | token::T_NEW => {
                // a call, or the making of an object
                let child = node.child(0);
                let has_property = node.opcode == token::T_LPARENTHESIS
                    && child.is_some_and(|child| {
                        matches!(child.opcode, token::T_DOT | token::T_LBRACKET)
                    });
                let has_non_local_symbol = node.opcode == token::T_LPARENTHESIS
                    && child.is_some_and(|child| child.opcode == token::T_SYMBOL)
                    && child.is_some_and(|child| self.is_non_local_symbol(child));
                let direct = has_property || has_non_local_symbol;

                if !direct && restype & RT_NEEDED != 0 {
                    // the register that holds the result is reserved
                    *frame += 1;
                }

                self.start_func_arg();
                let frame_start = *frame;
                let result = self.gen_call_code(frame, node, restype, direct, frame_start);
                self.end_func_arg();
                result
            }
            token::T_ARG => {
                // one argument of a call
                if node.len() >= 2
                    && let Some(child) = node.child(1)
                {
                    self.gen_node_code(frame, Some(child), RT_NEEDED, SubParam::default())?;
                }
                let arg = node.child(0);
                match arg {
                    None => {
                        self.add_func_arg(0, crate::vm::codegen::context::FuncArgKind::Normal);
                    }
                    Some(arg) if arg.opcode == token::T_EXPANDARG => {
                        // the argument collects the rest
                        match arg.child(0) {
                            Some(inner) => {
                                let register = self.gen_node_code(
                                    frame,
                                    Some(inner),
                                    RT_NEEDED,
                                    SubParam::default(),
                                )?;
                                self.add_func_arg(
                                    register,
                                    crate::vm::codegen::context::FuncArgKind::Expand,
                                );
                            }
                            None => {
                                self.add_func_arg(
                                    0,
                                    crate::vm::codegen::context::FuncArgKind::UnnamedExpand,
                                );
                            }
                        }
                    }
                    Some(arg) => {
                        let register =
                            self.gen_node_code(frame, Some(arg), RT_NEEDED, SubParam::default())?;
                        self.add_func_arg(
                            register,
                            crate::vm::codegen::context::FuncArgKind::Normal,
                        );
                    }
                }
                Ok(0)
            }
            token::T_OMIT => {
                self.add_omit_arg();
                Ok(0)
            }
            token::T_DOT | token::T_LBRACKET => {
                self.gen_member_access_code(frame, node, restype, param, node_pos)
            }
            token::T_SYMBOL => self.gen_symbol_code(frame, node, restype, param, node_pos),
            token::T_IGNOREPROP | token::T_PROPACCESS => {
                self.gen_property_access_code(frame, node, restype, param, node_pos)
            }
            token::T_SUPER => {
                // the super class of the class the context belongs to
                let parent = self.context().parent;
                let source = match parent {
                    Some(parent) => {
                        let parent_kind = self.contexts[parent].kind;
                        if parent_kind == ContextType::Property {
                            self.contexts[parent].parent.and_then(|grandparent| {
                                self.contexts[grandparent].super_class_expr.clone()
                            })
                        } else {
                            self.contexts[parent].super_class_expr.clone()
                        }
                    }
                    None => None,
                };
                let Some(source) = source else {
                    return Err(EmitError::Shape(
                        "the super class is not known here".to_string(),
                    ));
                };
                let was_global = self.as_global_context_mode;
                self.as_global_context_mode = true;
                let result = self.gen_node_code(frame, Some(&source), restype, param);
                self.as_global_context_mode = was_global;
                result
            }
            token::T_THIS => {
                if param.sub_type != kind::NONE {
                    return Err(cannot_modify());
                }
                Ok(-1)
            }
            token::T_THIS_PROXY => Ok(-self.context().kind.variable_reserve_count()),
            token::T_WITHDOT => {
                // the dot without an object: the `with` block's object
                let mut proxy = Node::new(token::T_DOT, node_pos);
                let left = Node::new(token::T_WITHDOT_PROXY, node_pos);
                crate::vm::codegen::node::add_child(&mut proxy, left.clone());
                if let Some(child) = node.child(0) {
                    crate::vm::codegen::node::add_child(&mut proxy, child.clone());
                }
                self.gen_node_code(frame, Some(&proxy), restype, param)
            }
            token::T_WITHDOT_PROXY => {
                // the object of the innermost `with` block
                let nests = self.context().nest_len();
                for index in (0..nests).rev() {
                    let nest = self.context().nest_at(index);
                    if nest.kind == NestKind::With {
                        return Ok(nest.ref_register);
                    }
                }
                // the `with` block is not here: the global object is
                self.gen_global_code(frame, restype, param, node_pos)
            }
            token::T_GLOBAL => self.gen_global_code(frame, restype, param, node_pos),
            token::T_INLINEARRAY => self.gen_inline_array_code(frame, node, restype, node_pos),
            token::T_ARRAYARG => self.gen_array_arg_code(frame, node, node_pos),
            token::T_INLINEDIC => self.gen_inline_dic_code(frame, node, restype, node_pos),
            token::T_DICELM => self.gen_dic_elm_code(frame, node, node_pos),
            token::T_REGEXP => self.gen_regexp_code(frame, node, restype, node_pos),
            token::T_VOID => {
                if param.sub_type != kind::NONE {
                    return Err(cannot_modify());
                }
                Ok(0)
            }
            token::T_IN => {
                // the `in` operator has no case of its own: the grammar
                // makes the node and the generator's switch falls out of
                // it, so the expression makes no code at all and its
                // operands are not even evaluated
                Ok(0)
            }
            _ => Err(EmitError::Shape(format!(
                "the operation {} has no code",
                node.opcode
            ))),
        }
    }

    /// The code of the global object.
    fn gen_global_code(
        &mut self,
        frame: &mut i32,
        restype: u32,
        param: SubParam,
        node_pos: i32,
    ) -> Result<i32, EmitError> {
        if param.sub_type != kind::NONE {
            return Err(cannot_modify());
        }
        if restype & RT_NEEDED == 0 {
            return Ok(0);
        }
        self.context_mut().put_code(vm::GLOBAL, node_pos);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(*frame), node_pos);
        *frame += 1;
        Ok(*frame - 1)
    }

    /// The code of a call.
    fn gen_call_code(
        &mut self,
        frame: &mut i32,
        node: &Rc<Node>,
        restype: u32,
        direct: bool,
        frame_start: i32,
    ) -> Result<i32, EmitError> {
        let node_pos = node.position;
        // the arguments
        let arguments = node.child(1);
        let empty = match arguments {
            Some(arguments) => arguments.len() == 1 && arguments.child(0).is_none(),
            None => true,
        };
        if !empty && let Some(arguments) = arguments {
            self.gen_node_code(frame, Some(arguments), RT_NEEDED, SubParam::default())?;
        }

        let mut param2 = SubParam::default();
        if direct {
            param2.sub_type = kind::FUNC_CALL;
            return self.gen_node_code(frame, node.child(0), restype, param2);
        }
        let resaddr = self.gen_node_code(frame, node.child(0), RT_NEEDED, SubParam::default())?;
        let result = if restype & RT_NEEDED != 0 {
            frame_start - 1
        } else {
            0
        };
        self.context_mut().put_code(
            if node.opcode == token::T_NEW {
                vm::NEW
            } else {
                vm::CALL
            },
            node_pos,
        );
        self.context_mut()
            .put_code(opcode::vm_reg_addr(result), node_pos);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(resaddr), node_pos);
        self.generate_func_call_arg_code();
        let base = frame_start;
        self.clear_frame(frame, base);
        Ok(result)
    }

    /// The code of a member access.
    fn gen_member_access_code(
        &mut self,
        frame: &mut i32,
        node: &Rc<Node>,
        restype: u32,
        param: SubParam,
        node_pos: i32,
    ) -> Result<i32, EmitError> {
        let direct = node.opcode == token::T_DOT;
        let param2 = SubParam::default();
        let resaddr = self.gen_node_code(frame, node.child(0), RT_NEEDED, param2)?;

        let dp = if direct {
            let value = node
                .child(1)
                .and_then(|child| child.value().cloned())
                .unwrap_or(Value::Void);
            self.context_mut().put_data(value)
        } else {
            self.gen_node_code(frame, node.child(1), RT_NEEDED, SubParam::default())?
        };

        match param.sub_type {
            kind::NONE | kind::IGNORE_PROP_GET => {
                let code = match (direct, param.sub_type) {
                    (true, kind::NONE) => vm::GPD,
                    (true, _) => vm::GPDS,
                    (false, kind::NONE) => vm::GPI,
                    (false, _) => vm::GPIS,
                };
                self.context_mut().put_code(code, node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(*frame), node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(resaddr), node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(dp), node_pos);
                *frame += 1;
                Ok(*frame - 1)
            }
            kind::EQUAL | kind::IGNORE_PROP_SET => {
                let code = if param.sub_type == kind::EQUAL {
                    let is_this_proxy = node
                        .child(0)
                        .is_some_and(|child| child.opcode == token::T_THIS_PROXY);
                    match (direct, is_this_proxy) {
                        (true, true) => vm::SPD,
                        (true, false) => vm::SPDE,
                        (false, true) => vm::SPI,
                        (false, false) => vm::SPIE,
                    }
                } else if direct {
                    vm::SPDS
                } else {
                    vm::SPIS
                };
                self.context_mut().put_code(code, node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(resaddr), node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(dp), node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(param.sub_address), node_pos);
                Ok(param.sub_address)
            }
            _ if is_arithmetic_kind(param.sub_type) => {
                // the operation is done on the member
                self.context_mut()
                    .put_code(param.sub_type + if direct { 1 } else { 2 }, node_pos);
                self.context_mut().put_code(
                    opcode::vm_reg_addr(if restype & RT_NEEDED != 0 { *frame } else { 0 }),
                    node_pos,
                );
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(resaddr), node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(dp), node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(param.sub_address), node_pos);
                if restype & RT_NEEDED != 0 {
                    *frame += 1;
                    return Ok(*frame - 1);
                }
                Ok(0)
            }
            kind::PRE_INC | kind::PRE_DEC => {
                let code = if param.sub_type == kind::PRE_INC {
                    vm::INC
                } else {
                    vm::DEC
                };
                self.context_mut()
                    .put_code(code + if direct { 1 } else { 2 }, node_pos);
                self.context_mut().put_code(
                    opcode::vm_reg_addr(if restype & RT_NEEDED != 0 { *frame } else { 0 }),
                    node_pos,
                );
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(resaddr), node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(dp), node_pos);
                if restype & RT_NEEDED != 0 {
                    *frame += 1;
                    return Ok(*frame - 1);
                }
                Ok(0)
            }
            kind::POST_INC | kind::POST_DEC => {
                let mut result = 0;
                if restype & RT_NEEDED != 0 {
                    self.context_mut()
                        .put_code(if direct { vm::GPD } else { vm::GPI }, node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(*frame), node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(resaddr), node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(dp), node_pos);
                    result = *frame;
                    *frame += 1;
                }
                let code = if param.sub_type == kind::POST_INC {
                    vm::INC
                } else {
                    vm::DEC
                };
                self.context_mut()
                    .put_code(code + if direct { 1 } else { 2 }, node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(0), node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(resaddr), node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(dp), node_pos);
                Ok(result)
            }
            kind::TYPEOF => {
                self.context_mut()
                    .put_code(if direct { vm::TYPEOFD } else { vm::TYPEOFI }, node_pos);
                self.context_mut().put_code(
                    opcode::vm_reg_addr(if restype & RT_NEEDED != 0 { *frame } else { 0 }),
                    node_pos,
                );
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(resaddr), node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(dp), node_pos);
                if restype & RT_NEEDED != 0 {
                    *frame += 1;
                    return Ok(*frame - 1);
                }
                Ok(0)
            }
            kind::DELETE => {
                self.context_mut()
                    .put_code(if direct { vm::DELD } else { vm::DELI }, node_pos);
                self.context_mut().put_code(
                    opcode::vm_reg_addr(if restype & RT_NEEDED != 0 { *frame } else { 0 }),
                    node_pos,
                );
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(resaddr), node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(dp), node_pos);
                if restype & RT_NEEDED != 0 {
                    *frame += 1;
                    return Ok(*frame - 1);
                }
                Ok(0)
            }
            kind::FUNC_CALL => {
                self.context_mut()
                    .put_code(if direct { vm::CALLD } else { vm::CALLI }, node_pos);
                self.context_mut().put_code(
                    opcode::vm_reg_addr(if restype & RT_NEEDED != 0 { *frame } else { 0 }),
                    node_pos,
                );
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(resaddr), node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(dp), node_pos);
                self.generate_func_call_arg_code();
                if restype & RT_NEEDED != 0 {
                    *frame += 1;
                    return Ok(*frame - 1);
                }
                Ok(0)
            }
            _ => Err(cannot_modify()),
        }
    }

    /// The code of a variable.
    fn gen_symbol_code(
        &mut self,
        frame: &mut i32,
        node: &Rc<Node>,
        restype: u32,
        param: SubParam,
        node_pos: i32,
    ) -> Result<i32, EmitError> {
        let name = match node.value() {
            Some(Value::String(name)) => name.clone(),
            _ => String::new(),
        };
        let reserve = self.context().kind.variable_reserve_count();
        let index = if self.as_global_context_mode {
            // the global context cannot reach a local variable
            -1
        } else {
            self.context().namespace.find(&name)
        };

        if index == -1 {
            // the variable is not a local one: it is a member of `this`
            let mut member = Node::new(token::T_DOT, node_pos);
            let object = Node::new(
                if self.as_global_context_mode {
                    token::T_GLOBAL
                } else {
                    token::T_THIS_PROXY
                },
                node_pos,
            );
            let mut member_name = Node::new(token::T_SYMBOL, node_pos);
            crate::vm::codegen::node::set_value(&mut member_name, Value::String(name));
            crate::vm::codegen::node::add_child(&mut member, object.clone());
            crate::vm::codegen::node::add_child(&mut member, member_name.clone());
            return self.gen_node_code(frame, Some(&member), restype, param);
        }

        let register = -index - reserve - 1;
        if is_modify_sub_type(param.sub_type) {
            match param.sub_type {
                kind::EQUAL => {
                    self.context_mut().put_code(vm::CP, node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(register), node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(param.sub_address), node_pos);
                }
                _ if is_arithmetic_kind(param.sub_type) => {
                    self.context_mut().put_code(param.sub_type, node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(register), node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(param.sub_address), node_pos);
                    if restype & RT_NEEDED != 0 {
                        return Ok(register);
                    }
                    return Ok(0);
                }
                kind::PRE_INC => {
                    self.context_mut().put_code(vm::INC, node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(register), node_pos);
                    if restype & RT_NEEDED != 0 {
                        return Ok(register);
                    }
                    return Ok(0);
                }
                kind::PRE_DEC => {
                    self.context_mut().put_code(vm::DEC, node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(register), node_pos);
                    if restype & RT_NEEDED != 0 {
                        return Ok(register);
                    }
                    return Ok(0);
                }
                kind::POST_INC | kind::POST_DEC => {
                    if restype & RT_NEEDED != 0 {
                        self.context_mut().put_code(vm::CP, node_pos);
                        self.context_mut()
                            .put_code(opcode::vm_reg_addr(*frame), node_pos);
                        self.context_mut()
                            .put_code(opcode::vm_reg_addr(register), node_pos);
                        *frame += 1;
                    }
                    self.context_mut().put_code(
                        if param.sub_type == kind::POST_INC {
                            vm::INC
                        } else {
                            vm::DEC
                        },
                        node_pos,
                    );
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(register), node_pos);
                    if restype & RT_NEEDED != 0 {
                        return Ok(*frame - 1);
                    }
                    return Ok(0);
                }
                kind::DELETE => {
                    self.context_mut().namespace.remove(&name);
                    if restype & RT_NEEDED != 0 {
                        let dp = self.context_mut().put_data(Value::Integer(1));
                        self.context_mut().put_code(vm::CONST, node_pos);
                        self.context_mut()
                            .put_code(opcode::vm_reg_addr(*frame), node_pos);
                        self.context_mut()
                            .put_code(opcode::vm_reg_addr(dp), node_pos);
                        return Ok(*frame - 1);
                    }
                    return Ok(0);
                }
                _ => return Err(cannot_modify()),
            }
            return Ok(0);
        }

        // the variable is read
        Ok(register)
    }

    /// The code of the property access operators.
    fn gen_property_access_code(
        &mut self,
        frame: &mut i32,
        node: &Rc<Node>,
        restype: u32,
        param: SubParam,
        node_pos: i32,
    ) -> Result<i32, EmitError> {
        let ignores_property = node.opcode == token::T_IGNOREPROP;
        if ignores_property {
            // the property access is ignored
            let mut param2 = param;
            if param2.sub_type == kind::NONE {
                param2.sub_type = kind::IGNORE_PROP_GET;
            } else if param2.sub_type == kind::EQUAL {
                param2.sub_type = kind::IGNORE_PROP_SET;
            } else {
                return Err(cannot_modify());
            }
            return self.gen_node_code(frame, node.child(0), restype, param2);
        }

        // the property access is forced
        let resaddr = self.gen_node_code(frame, node.child(0), RT_NEEDED, SubParam::default())?;
        match param.sub_type {
            kind::NONE => {
                self.context_mut().put_code(vm::GETP, node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(*frame), node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(resaddr), node_pos);
                *frame += 1;
                Ok(*frame - 1)
            }
            kind::EQUAL => {
                self.context_mut().put_code(vm::SETP, node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(resaddr), node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(param.sub_address), node_pos);
                Ok(param.sub_address)
            }
            _ if is_arithmetic_kind(param.sub_type) => {
                self.context_mut().put_code(param.sub_type + 3, node_pos);
                self.context_mut().put_code(
                    opcode::vm_reg_addr(if restype & RT_NEEDED != 0 { *frame } else { 0 }),
                    node_pos,
                );
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(resaddr), node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(param.sub_address), node_pos);
                if restype & RT_NEEDED != 0 {
                    *frame += 1;
                    return Ok(*frame - 1);
                }
                Ok(0)
            }
            kind::PRE_INC | kind::PRE_DEC => {
                let code = if param.sub_type == kind::PRE_INC {
                    vm::INC
                } else {
                    vm::DEC
                };
                self.context_mut().put_code(code + 3, node_pos);
                self.context_mut().put_code(
                    opcode::vm_reg_addr(if restype & RT_NEEDED != 0 { *frame } else { 0 }),
                    node_pos,
                );
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(resaddr), node_pos);
                if restype & RT_NEEDED != 0 {
                    *frame += 1;
                    return Ok(*frame - 1);
                }
                Ok(0)
            }
            kind::POST_INC | kind::POST_DEC => {
                let mut result = 0;
                if restype & RT_NEEDED != 0 {
                    self.context_mut().put_code(vm::GETP, node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(*frame), node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(resaddr), node_pos);
                    result = *frame;
                    *frame += 1;
                }
                let code = if param.sub_type == kind::POST_INC {
                    vm::INC
                } else {
                    vm::DEC
                };
                self.context_mut().put_code(code + 3, node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(0), node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(resaddr), node_pos);
                Ok(result)
            }
            _ => Err(cannot_modify()),
        }
    }

    /// The code of an inline array.
    fn gen_inline_array_code(
        &mut self,
        frame: &mut i32,
        node: &Rc<Node>,
        restype: u32,
        node_pos: i32,
    ) -> Result<i32, EmitError> {
        let array_data = self
            .context_mut()
            .put_data(Value::String("Array".to_string()));
        let frame0 = *frame;
        self.context_mut().put_code(vm::GLOBAL, node_pos);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(*frame), node_pos);
        self.context_mut().put_code(vm::GPD, node_pos);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(*frame + 1), node_pos);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(*frame), node_pos);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(array_data), node_pos);
        self.context_mut().put_code(vm::NEW, node_pos);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(*frame), node_pos);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(*frame + 1), node_pos);
        self.context_mut().put_code(0, -1);
        let zero_data = self.context_mut().put_data(Value::Integer(0));
        self.context_mut().put_code(vm::CONST, node_pos);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(*frame + 1), node_pos);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(zero_data), node_pos);
        *frame += 2;

        self.push_array_arg(frame0, frame0 + 1);

        // the array's one element is empty: the array holds nothing, and
        // the element makes no code
        let empty = node.len() == 1 && node.child(0).and_then(|child| child.child(0)).is_none();
        if !empty {
            for index in 0..node.len() {
                self.gen_node_code(frame, node.child(index), RT_NEEDED, SubParam::default())?;
            }
        }
        self.pop_array_arg();
        Ok(if restype & RT_NEEDED != 0 { frame0 } else { 0 })
    }

    /// The code of one element of an inline array.
    fn gen_array_arg_code(
        &mut self,
        frame: &mut i32,
        node: &Rc<Node>,
        node_pos: i32,
    ) -> Result<i32, EmitError> {
        let frame_start = *frame;
        let resaddr = match node.child(0) {
            Some(child) => {
                self.gen_node_code(frame, Some(child), RT_NEEDED, SubParam::default())?
            }
            None => 0,
        };
        let (object, counter) = self.array_arg();
        self.context_mut().put_code(vm::SPIS, node_pos);
        self.context_mut().put_code(opcode::vm_reg_addr(object), -1);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(counter), -1);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(resaddr), -1);
        self.context_mut().put_code(vm::INC, -1);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(counter), -1);
        self.clear_frame(frame, frame_start);
        Ok(0)
    }

    /// The code of an inline dictionary.
    fn gen_inline_dic_code(
        &mut self,
        frame: &mut i32,
        node: &Rc<Node>,
        restype: u32,
        node_pos: i32,
    ) -> Result<i32, EmitError> {
        let dic_data = self
            .context_mut()
            .put_data(Value::String("Dictionary".to_string()));
        let frame0 = *frame;
        self.context_mut().put_code(vm::GLOBAL, node_pos);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(*frame), node_pos);
        self.context_mut().put_code(vm::GPD, node_pos);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(*frame + 1), node_pos);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(*frame), node_pos);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(dic_data), node_pos);
        self.context_mut().put_code(vm::NEW, node_pos);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(*frame), node_pos);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(*frame + 1), node_pos);
        self.context_mut().put_code(0, -1);
        *frame += 2;
        self.clear_frame(frame, frame0 + 1);

        self.push_array_arg(frame0, 0);

        for index in 0..node.len() {
            self.gen_node_code(frame, node.child(index), RT_NEEDED, SubParam::default())?;
        }
        self.pop_array_arg();
        Ok(if restype & RT_NEEDED != 0 { frame0 } else { 0 })
    }

    /// The code of one element of an inline dictionary.
    fn gen_dic_elm_code(
        &mut self,
        frame: &mut i32,
        node: &Rc<Node>,
        node_pos: i32,
    ) -> Result<i32, EmitError> {
        let frame_start = *frame;
        let name = self.gen_node_code(frame, node.child(0), RT_NEEDED, SubParam::default())?;
        let value = self.gen_node_code(frame, node.child(1), RT_NEEDED, SubParam::default())?;
        let (object, _) = self.array_arg();
        self.context_mut().put_code(vm::SPIS, node_pos);
        self.context_mut().put_code(opcode::vm_reg_addr(object), -1);
        self.context_mut().put_code(opcode::vm_reg_addr(name), -1);
        self.context_mut().put_code(opcode::vm_reg_addr(value), -1);
        self.clear_frame(frame, frame_start);
        Ok(0)
    }

    /// The code of a regular expression.
    fn gen_regexp_code(
        &mut self,
        frame: &mut i32,
        node: &Rc<Node>,
        restype: u32,
        node_pos: i32,
    ) -> Result<i32, EmitError> {
        if restype & RT_NEEDED == 0 {
            return Ok(0);
        }
        let regexp_data = self
            .context_mut()
            .put_data(Value::String("RegExp".to_string()));
        let pattern_data = self
            .context_mut()
            .put_data(node.value().cloned().unwrap_or(Value::Void));
        let compile_data = self
            .context_mut()
            .put_data(Value::String("_compile".to_string()));
        let frame0 = *frame;
        self.context_mut().put_code(vm::GLOBAL, node_pos);
        self.context_mut().put_code(opcode::vm_reg_addr(*frame), -1);
        self.context_mut().put_code(vm::GPD, -1);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(*frame + 1), -1);
        self.context_mut().put_code(opcode::vm_reg_addr(*frame), -1);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(regexp_data), -1);
        self.context_mut().put_code(vm::CONST, -1);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(*frame + 2), -1);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(pattern_data), -1);
        self.context_mut().put_code(vm::NEW, -1);
        self.context_mut().put_code(opcode::vm_reg_addr(*frame), -1);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(*frame + 1), -1);
        self.context_mut().put_code(0, -1);
        self.context_mut().put_code(vm::CALLD, -1);
        self.context_mut().put_code(opcode::vm_reg_addr(0), -1);
        self.context_mut().put_code(opcode::vm_reg_addr(frame0), -1);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(compile_data), -1);
        self.context_mut().put_code(1, -1);
        self.context_mut()
            .put_code(opcode::vm_reg_addr(*frame + 2), -1);
        *frame += 3;
        self.clear_frame(frame, frame0 + 1);
        Ok(frame0)
    }
}
