//! The emission of the operations.
//!
//! The emission is a switch over the opcode of a node that emits the
//! instructions of the operation.
//!
//! The operations come in families: the ones with one operand, the ones
//! with two, the ones that assign, and the ones that reach into an
//! object. The families share their shape, so the families are one
//! function each.

use std::rc::Rc;

use crate::vm::codegen::emit::{
    EmitError, GNC_CFLAG, GNC_CFLAG_I, RT_CFLAG, RT_NEEDED, SubParam, is_cond_flag, is_frame,
    is_modify_sub_type, kind,
};
use crate::vm::codegen::node::Node;
use crate::vm::codegen::opcode::{self, vm};
use crate::vm::codegen::token;

/// The sub-type of an operation that assigns.
fn assign_kind(opcode: i32) -> i32 {
    match opcode {
        token::T_AMPERSANDEQUAL => kind::BIT_AND,
        token::T_VERTLINEEQUAL => kind::BIT_OR,
        token::T_CHEVRONEQUAL => kind::BIT_XOR,
        token::T_MINUSEQUAL => kind::SUB,
        token::T_PLUSEQUAL => kind::ADD,
        token::T_PERCENTEQUAL => kind::MOD,
        token::T_SLASHEQUAL => kind::DIV,
        token::T_BACKSLASHEQUAL => kind::IDIV,
        token::T_ASTERISKEQUAL => kind::MUL,
        token::T_LOGICALOREQUAL => kind::LOG_OR,
        token::T_LOGICALANDEQUAL => kind::LOG_AND,
        token::T_RARITHSHIFTEQUAL => kind::SAR,
        token::T_LARITHSHIFTEQUAL => kind::SAL,
        token::T_RBITSHIFTEQUAL => kind::SR,
        _ => kind::NONE,
    }
}

/// The instruction of a two-term operation.
fn binary_opcode(opcode: i32) -> i32 {
    match opcode {
        token::T_VERTLINE => vm::BOR,
        token::T_CHEVRON => vm::BXOR,
        token::T_AMPERSAND => vm::BAND,
        token::T_RARITHSHIFT => vm::SAR,
        token::T_LARITHSHIFT => vm::SAL,
        token::T_RBITSHIFT => vm::SR,
        token::T_PLUS => vm::ADD,
        token::T_MINUS => vm::SUB,
        token::T_PERCENT => vm::MOD,
        token::T_SLASH => vm::DIV,
        token::T_BACKSLASH => vm::IDIV,
        token::T_ASTERISK => vm::MUL,
        _ => vm::NOP,
    }
}

/// The instruction of a unary operation.
fn unary_opcode(opcode: i32, restype: u32) -> i32 {
    match opcode {
        token::T_TILDE => vm::BNOT,
        token::T_SHARP => vm::ASC,
        token::T_DOLLAR => vm::CHR,
        token::T_UPLUS => vm::NUM,
        token::T_UMINUS => vm::CHS,
        token::T_INVALIDATE => vm::INV,
        token::T_ISVALID => vm::CHKINV,
        token::T_TYPEOF => vm::TYPEOF,
        token::T_EVAL => {
            if restype & RT_NEEDED != 0 {
                vm::EVAL
            } else {
                vm::EEXP
            }
        }
        token::T_INT => vm::INT,
        token::T_REAL => vm::REAL,
        token::T_STRING => vm::STR,
        token::T_OCTET => vm::OCTET,
        _ => vm::NOP,
    }
}

/// The two instructions of a comparison.
fn compare_opcodes(opcode: i32) -> (i32, i32) {
    match opcode {
        token::T_NOTEQUAL => (vm::CEQ, vm::SETNF),
        token::T_EQUALEQUAL => (vm::CEQ, vm::SETF),
        token::T_DISCNOTEQUAL => (vm::CDEQ, vm::SETNF),
        token::T_DISCEQUAL => (vm::CDEQ, vm::SETF),
        token::T_LT => (vm::CLT, vm::SETF),
        token::T_GT => (vm::CGT, vm::SETF),
        token::T_LTOREQUAL => (vm::CGT, vm::SETNF),
        token::T_GTOREQUAL => (vm::CLT, vm::SETNF),
        _ => (vm::NOP, vm::NOP),
    }
}

/// The error the generator raises when the left hand side cannot be
/// modified.
fn cannot_modify() -> EmitError {
    EmitError::Shape("the left hand side of the operation cannot be modified".to_string())
}

/// The error the generator raises when the operation has no result.
fn cannot_get_result() -> EmitError {
    EmitError::Shape("the operation has no result".to_string())
}

impl crate::vm::codegen::emit::Emitter<'_> {
    /// The code of an operation.
    pub fn gen_operation_code(
        &mut self,
        frame: &mut i32,
        node: &Rc<Node>,
        restype: u32,
        param: SubParam,
    ) -> Result<i32, EmitError> {
        let node_pos = node.position;
        match node.opcode {
            token::T_INCONTEXTOF => {
                if restype & RT_NEEDED == 0 {
                    return Ok(0);
                }
                // the object whose context is changed keeps the sub-type
                // (reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:1189-1190)
                let mut resaddr1 = self.gen_node_code(frame, node.child(0), RT_NEEDED, param)?;
                let resaddr2 =
                    self.gen_node_code(frame, node.child(1), RT_NEEDED, SubParam::default())?;
                if !is_frame(resaddr1) {
                    self.context_mut().put_code(vm::CP, node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(*frame), node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(resaddr1), node_pos);
                    resaddr1 = *frame;
                    *frame += 1;
                }
                self.context_mut().put_code(vm::CHGTHIS, node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(resaddr1), node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(resaddr2), node_pos);
                Ok(resaddr1)
            }
            token::T_COMMA => {
                self.gen_node_code(frame, node.child(0), 0, SubParam::default())?;
                self.gen_node_code(frame, node.child(1), restype, param)
            }
            token::T_SWAP => {
                if restype & RT_NEEDED != 0 {
                    return Err(cannot_get_result());
                }
                if param.sub_type != kind::NONE {
                    return Err(cannot_modify());
                }
                let mut resaddr1 =
                    self.gen_node_code(frame, node.child(0), RT_NEEDED, SubParam::default())?;
                if !is_frame(resaddr1) {
                    self.context_mut().put_code(vm::CP, node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(*frame), node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(resaddr1), node_pos);
                    resaddr1 = *frame;
                    *frame += 1;
                }
                let resaddr2 =
                    self.gen_node_code(frame, node.child(1), RT_NEEDED, SubParam::default())?;
                // the two sides are exchanged
                let mut param2 = SubParam {
                    sub_type: kind::EQUAL,
                    sub_address: resaddr2,
                };
                self.gen_node_code(frame, node.child(0), 0, param2)?;
                param2.sub_type = kind::EQUAL;
                param2.sub_address = resaddr1;
                self.gen_node_code(frame, node.child(1), 0, param2)?;
                Ok(0)
            }
            token::T_EQUAL => {
                if param.sub_type != kind::NONE {
                    return Err(cannot_modify());
                }
                let resaddr = self.gen_node_code(frame, node.child(1), RT_NEEDED, param)?;
                let param2 = SubParam {
                    sub_type: kind::EQUAL,
                    sub_address: resaddr,
                };
                self.gen_node_code(frame, node.child(0), 0, param2)?;
                Ok(resaddr)
            }
            token::T_AMPERSANDEQUAL
            | token::T_VERTLINEEQUAL
            | token::T_CHEVRONEQUAL
            | token::T_MINUSEQUAL
            | token::T_PLUSEQUAL
            | token::T_PERCENTEQUAL
            | token::T_SLASHEQUAL
            | token::T_BACKSLASHEQUAL
            | token::T_ASTERISKEQUAL
            | token::T_LOGICALOREQUAL
            | token::T_LOGICALANDEQUAL
            | token::T_RARITHSHIFTEQUAL
            | token::T_LARITHSHIFTEQUAL
            | token::T_RBITSHIFTEQUAL => {
                if param.sub_type != kind::NONE {
                    return Err(cannot_modify());
                }
                let resaddr =
                    self.gen_node_code(frame, node.child(1), RT_NEEDED, SubParam::default())?;
                let param2 = SubParam {
                    sub_type: assign_kind(node.opcode),
                    sub_address: resaddr,
                };
                self.gen_node_code(frame, node.child(0), restype, param2)
            }
            token::T_QUESTION => {
                let resaddr = self.gen_node_code(
                    frame,
                    node.child(0),
                    RT_NEEDED | RT_CFLAG,
                    SubParam::default(),
                )?;
                let mut inverted = false;
                if !is_cond_flag(resaddr) {
                    self.context_mut().put_code(vm::TT, node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(resaddr), node_pos);
                } else if resaddr == GNC_CFLAG_I {
                    inverted = true;
                }
                let current_frame = *frame;
                let address1 = self.context().code_size();
                self.context_mut().add_jump();
                self.context_mut()
                    .put_code(if inverted { vm::JF } else { vm::JNF }, node_pos);
                self.context_mut().put_code(0, node_pos);

                let mut resaddr1 = self.gen_node_code(frame, node.child(1), restype, param)?;

                if restype & RT_CFLAG != 0 {
                    if !is_cond_flag(resaddr1) {
                        self.context_mut().put_code(vm::TT, node_pos);
                        self.context_mut()
                            .put_code(opcode::vm_reg_addr(resaddr1), node_pos);
                    } else if resaddr1 == GNC_CFLAG_I {
                        self.context_mut().put_code(vm::NF, node_pos);
                    }
                } else if restype & RT_NEEDED != 0 && !is_cond_flag(resaddr1) && !is_frame(resaddr1)
                {
                    self.context_mut().put_code(vm::CP, node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(*frame), node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(resaddr1), node_pos);
                    resaddr1 = *frame;
                    *frame += 1;
                }
                let frame1 = *frame;

                let address2 = self.context().code_size();
                self.context_mut().add_jump();
                self.context_mut().put_code(vm::JMP, node_pos);
                self.context_mut().put_code(0, node_pos);
                let size = self.context().code_size();
                self.context_mut().code_mut()[(address1 + 1) as usize] = size - address1;

                *frame = current_frame;
                let resaddr2 = self.gen_node_code(frame, node.child(2), restype, param)?;

                if restype & RT_CFLAG != 0 {
                    if !is_cond_flag(resaddr2) {
                        self.context_mut().put_code(vm::TT, node_pos);
                        self.context_mut()
                            .put_code(opcode::vm_reg_addr(resaddr2), node_pos);
                    } else if resaddr2 == GNC_CFLAG_I {
                        self.context_mut().put_code(vm::NF, node_pos);
                    }
                } else if restype & RT_NEEDED != 0
                    && !is_cond_flag(resaddr1)
                    && resaddr1 != resaddr2
                {
                    self.context_mut().put_code(vm::CP, node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(resaddr1), node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(resaddr2), node_pos);
                    *frame += 1;
                }

                let frame2 = *frame;
                let size = self.context().code_size();
                self.context_mut().code_mut()[(address2 + 1) as usize] = size - address2;
                // the frame is the greater of the two branches'
                // (reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3225-3227)
                *frame = frame2.max(frame1);
                Ok(if restype & RT_CFLAG != 0 {
                    GNC_CFLAG
                } else {
                    resaddr1
                })
            }
            token::T_LOGICALOR | token::T_LOGICALAND => {
                if param.sub_type != kind::NONE {
                    return Err(cannot_modify());
                }
                let resaddr1 = self.gen_node_code(
                    frame,
                    node.child(0),
                    RT_NEEDED | RT_CFLAG,
                    SubParam::default(),
                )?;
                let mut inverted = false;
                if !is_cond_flag(resaddr1) {
                    self.context_mut().put_code(vm::TT, node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(resaddr1), node_pos);
                }
                if resaddr1 == GNC_CFLAG_I {
                    inverted = true;
                }
                let address1 = self.context().code_size();
                self.context_mut().add_jump();
                let jump = if node.opcode == token::T_LOGICALOR {
                    if inverted { vm::JNF } else { vm::JF }
                } else if inverted {
                    vm::JF
                } else {
                    vm::JNF
                };
                self.context_mut().put_code(jump, node_pos);
                self.context_mut().put_code(0, node_pos);

                let resaddr2 = self.gen_node_code(
                    frame,
                    node.child(1),
                    RT_NEEDED | RT_CFLAG,
                    SubParam::default(),
                )?;
                if !is_cond_flag(resaddr2) {
                    self.context_mut()
                        .put_code(if inverted { vm::TF } else { vm::TT }, node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(resaddr2), node_pos);
                } else if inverted != (resaddr2 == GNC_CFLAG_I) {
                    self.context_mut().put_code(vm::NF, node_pos);
                }
                let size = self.context().code_size();
                self.context_mut().code_mut()[(address1 + 1) as usize] = size - address1;

                if restype & RT_CFLAG == 0 {
                    if is_cond_flag(resaddr1) || !is_frame(resaddr1) {
                        self.context_mut()
                            .put_code(if inverted { vm::SETNF } else { vm::SETF }, node_pos);
                        self.context_mut()
                            .put_code(opcode::vm_reg_addr(*frame), node_pos);
                        let result = *frame;
                        *frame += 1;
                        return Ok(result);
                    }
                    self.context_mut()
                        .put_code(if inverted { vm::SETNF } else { vm::SETF }, node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(resaddr1), node_pos);
                }
                Ok(if restype & RT_CFLAG != 0 {
                    if inverted { GNC_CFLAG_I } else { GNC_CFLAG }
                } else {
                    resaddr1
                })
            }
            token::T_INSTANCEOF => {
                let mut resaddr1 =
                    self.gen_node_code(frame, node.child(0), RT_NEEDED, SubParam::default())?;
                if !is_frame(resaddr1) {
                    self.context_mut().put_code(vm::CP, node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(*frame), node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(resaddr1), node_pos);
                    resaddr1 = *frame;
                    *frame += 1;
                }
                let resaddr2 =
                    self.gen_node_code(frame, node.child(1), RT_NEEDED, SubParam::default())?;
                self.context_mut().put_code(vm::CHKINS, node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(resaddr1), node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(resaddr2), node_pos);
                Ok(resaddr1)
            }
            token::T_VERTLINE
            | token::T_CHEVRON
            | token::T_AMPERSAND
            | token::T_RARITHSHIFT
            | token::T_LARITHSHIFT
            | token::T_RBITSHIFT
            | token::T_PLUS
            | token::T_MINUS
            | token::T_PERCENT
            | token::T_SLASH
            | token::T_BACKSLASH
            | token::T_ASTERISK => {
                if is_modify_sub_type(param.sub_type) {
                    return Err(cannot_modify());
                }
                let mut resaddr1 =
                    self.gen_node_code(frame, node.child(0), RT_NEEDED, SubParam::default())?;
                if !is_frame(resaddr1) {
                    self.context_mut().put_code(vm::CP, node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(*frame), node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(resaddr1), node_pos);
                    resaddr1 = *frame;
                    *frame += 1;
                }
                let resaddr2 =
                    self.gen_node_code(frame, node.child(1), RT_NEEDED, SubParam::default())?;
                self.context_mut()
                    .put_code(binary_opcode(node.opcode), node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(resaddr1), node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(resaddr2), node_pos);
                Ok(resaddr1)
            }
            token::T_NOTEQUAL
            | token::T_EQUALEQUAL
            | token::T_DISCNOTEQUAL
            | token::T_DISCEQUAL
            | token::T_LT
            | token::T_GT
            | token::T_LTOREQUAL
            | token::T_GTOREQUAL => {
                if is_modify_sub_type(param.sub_type) {
                    return Err(cannot_modify());
                }
                let mut resaddr1 =
                    self.gen_node_code(frame, node.child(0), RT_NEEDED, SubParam::default())?;
                if restype & RT_CFLAG == 0 && !is_frame(resaddr1) {
                    self.context_mut().put_code(vm::CP, node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(*frame), node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(resaddr1), node_pos);
                    resaddr1 = *frame;
                    *frame += 1;
                }
                let resaddr2 =
                    self.gen_node_code(frame, node.child(1), RT_NEEDED, SubParam::default())?;
                let (code1, code2) = compare_opcodes(node.opcode);
                self.context_mut().put_code(code1, node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(resaddr1), node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(resaddr2), node_pos);
                if restype & RT_CFLAG == 0 {
                    self.context_mut().put_code(code2, node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(resaddr1), node_pos);
                }
                Ok(if restype & RT_CFLAG != 0 {
                    if code2 == vm::SETNF {
                        GNC_CFLAG_I
                    } else {
                        GNC_CFLAG
                    }
                } else {
                    resaddr1
                })
            }
            token::T_EXCRAMATION => {
                if is_modify_sub_type(param.sub_type) {
                    return Err(cannot_modify());
                }
                let mut resaddr =
                    self.gen_node_code(frame, node.child(0), restype, SubParam::default())?;
                if restype & RT_CFLAG == 0 {
                    if !is_frame(resaddr) {
                        self.context_mut().put_code(vm::CP, node_pos);
                        self.context_mut()
                            .put_code(opcode::vm_reg_addr(*frame), node_pos);
                        self.context_mut()
                            .put_code(opcode::vm_reg_addr(resaddr), node_pos);
                        resaddr = *frame;
                        *frame += 1;
                    }
                    self.context_mut().put_code(vm::LNOT, node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(resaddr), node_pos);
                    Ok(resaddr)
                } else {
                    if !is_cond_flag(resaddr) {
                        self.context_mut().put_code(vm::TF, node_pos);
                        self.context_mut()
                            .put_code(opcode::vm_reg_addr(resaddr), node_pos);
                        return Ok(GNC_CFLAG);
                    }
                    Ok(if resaddr == GNC_CFLAG_I {
                        GNC_CFLAG
                    } else {
                        GNC_CFLAG_I
                    })
                }
            }
            token::T_TILDE
            | token::T_SHARP
            | token::T_DOLLAR
            | token::T_UPLUS
            | token::T_UMINUS
            | token::T_INVALIDATE
            | token::T_ISVALID
            | token::T_EVAL
            | token::T_INT
            | token::T_REAL
            | token::T_STRING
            | token::T_OCTET => {
                if is_modify_sub_type(param.sub_type) {
                    return Err(cannot_modify());
                }
                let mut resaddr =
                    self.gen_node_code(frame, node.child(0), RT_NEEDED, SubParam::default())?;
                if !is_frame(resaddr) {
                    self.context_mut().put_code(vm::CP, node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(*frame), node_pos);
                    self.context_mut()
                        .put_code(opcode::vm_reg_addr(resaddr), node_pos);
                    resaddr = *frame;
                    *frame += 1;
                }
                self.context_mut()
                    .put_code(unary_opcode(node.opcode, restype), node_pos);
                self.context_mut()
                    .put_code(opcode::vm_reg_addr(resaddr), node_pos);
                Ok(resaddr)
            }
            _ => self.gen_remaining_code(frame, node, restype, param),
        }
    }
}
