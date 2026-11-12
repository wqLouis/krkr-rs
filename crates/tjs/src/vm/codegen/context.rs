//! The code context: the intermediate code of one function, class or
//! handler.
//!
//! A context holds the code stream, the constant area, the name space
//! that maps local variables to registers, and the fix-ups that are
//! applied when the context is finished.

use std::collections::BTreeMap;

use super::namespace::LocalNamespace;
use super::opcode::{self, vm};
use super::value::Value;

/// The kind of a code context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextType {
    /// The top level of a script.
    TopLevel,
    /// A function.
    Function,
    /// A function that is an expression.
    ExprFunction,
    /// A property.
    Property,
    /// The setter of a property.
    PropertySetter,
    /// The getter of a property.
    PropertyGetter,
    /// A class.
    Class,
    /// The proxy that holds the super class of a class.
    SuperClassGetter,
}

impl ContextType {
    /// The name of the kind.
    pub fn name(self) -> &'static str {
        match self {
            ContextType::TopLevel => "top level",
            ContextType::Function => "function",
            ContextType::ExprFunction => "function expression",
            ContextType::Property => "property",
            ContextType::PropertySetter => "property setter",
            ContextType::PropertyGetter => "property getter",
            ContextType::Class => "class",
            ContextType::SuperClassGetter => "super class getter proxy",
        }
    }

    /// The number of variables the kind reserves.
    ///
    /// A property reserves none; every other kind of context reserves
    /// two.
    pub fn variable_reserve_count(self) -> i32 {
        match self {
            ContextType::Property => 0,
            _ => 2,
        }
    }
}

/// The kind of a nest: a block, a loop, a switch, a try or a with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum NestKind {
    /// A block.
    #[default]
    Block,
    /// A `while` loop.
    While,
    /// A `do` loop.
    DoWhile,
    /// A `for` loop.
    For,
    /// A `switch`.
    Switch,
    /// A `try`.
    Try,
    /// A `catch`.
    Catch,
    /// A `with`.
    With,
}

/// What a nest needs to know when it is left.
#[derive(Clone, Debug, Default)]
pub struct NestData {
    /// The kind of the nest.
    pub kind: NestKind,
    /// Whether the nest has a variable of its own.
    pub has_variable: bool,
    /// Whether the nest created its variable.
    pub variable_created: bool,
    /// Whether the nest is the first case of a switch.
    pub is_first_case: bool,
    /// The register the nest's variable holds.
    pub ref_register: i32,
    /// The place the nest's code starts.
    pub start_ip: i32,
    /// The place the nest's loop starts.
    pub loop_start_ip: i32,
    /// The jumps that go back to the loop's head.
    pub continue_patches: Vec<i32>,
    /// The jumps that leave the nest.
    pub exit_patches: Vec<i32>,
    /// The expression the loop runs after every turn.
    ///
    /// The expression belongs to the loop, so it is held by the nest:
    /// two loops that are nested have two expressions.
    pub post_loop_expr: Option<std::rc::Rc<super::node::Node>>,
    /// The first place to patch.
    pub patch1: i32,
    /// The second place to patch.
    pub patch2: i32,
}

/// A fix-up: a piece of code to insert into the code stream.
#[derive(Clone, Debug)]
pub struct FixData {
    /// The place the code is inserted at.
    pub start_ip: i32,
    /// The size of the code the fix-up replaces.
    pub size: i32,
    /// The size of the code the fix-up inserts.
    pub new_size: i32,
    /// The code the fix-up inserts.
    pub code: Vec<i32>,
    /// Whether the insertion happens before the place.
    pub before_insertion: bool,
}

/// A function that is declared outside the context that holds it.
#[derive(Clone, Debug)]
pub struct NonLocalFunctionDecl {
    /// The place of the function's data in the constant area.
    pub data_pos: i32,
    /// The place of the function's name in the constant area.
    pub name_data_pos: i32,
    /// Whether the function is called with a changed `this`.
    pub change_this: bool,
}

/// The kind of a function argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FuncArgKind {
    /// An ordinary argument.
    Normal,
    /// An argument that collects the rest.
    Expand,
    /// An argument that collects the rest, without a name.
    UnnamedExpand,
}

/// One function argument.
#[derive(Clone, Debug)]
pub struct FuncArg {
    /// The register the argument holds.
    pub register: i32,
    /// The kind of the argument.
    pub kind: FuncArgKind,
    /// Whether the argument is left out of the call.
    pub is_omit: bool,
}

/// A member the context contributes to its parent object.
///
/// The child context is registered as a member of its parent object
/// while it compiles, which needs a live parent object. The generator
/// decides the list, and the side that has the object performs the
/// registration.
#[derive(Clone, Debug)]
pub struct Property {
    /// The name of the member.
    pub name: String,
    /// The index of the context the member holds.
    pub member: usize,
}

/// One array argument.
#[derive(Clone, Debug)]
pub struct ArrayArg {
    /// The register the argument holds.
    pub register: i32,
    /// The place of the argument's data in the constant area.
    pub object: i32,
    /// The number of elements the argument has.
    pub counter: i32,
}

/// The function argument list being built.
#[derive(Clone, Debug, Default)]
pub struct FuncArgList {
    /// The arguments of the list.
    pub args: Vec<FuncArg>,
    /// Whether the list holds an expanding argument.
    pub has_expand: bool,
    /// Whether the list omits its arguments.
    pub is_omit: bool,
}

/// A place in the script, and the place in the code it names.
#[derive(Clone, Copy, Debug)]
pub struct SourcePos {
    /// The place in the code stream.
    pub code_pos: i32,
    /// The place in the script.
    pub source_pos: i32,
}

/// The intermediate code of one context.
#[derive(Clone, Debug)]
pub struct Context {
    /// The kind of the context.
    pub kind: ContextType,
    /// The name of the context.
    pub name: Option<String>,
    /// The place of the context in the list.
    pub index: usize,
    /// The context that holds this one.
    pub parent: Option<usize>,
    /// The code stream.
    code: Vec<i32>,
    /// The constant area.
    data: Vec<Value>,
    /// The places in the script the code names.
    source_pos: Vec<SourcePos>,
    /// The place in the script the last instruction came from.
    prev_source_pos: i32,
    /// Whether the places in the script are sorted.
    source_pos_sorted: bool,
    /// The jumps whose target the fix-ups move.
    jump_list: Vec<i32>,
    /// The fix-ups to apply.
    fix_list: Vec<FixData>,
    /// The functions declared outside the context.
    non_local_function_decls: Vec<NonLocalFunctionDecl>,
    /// The name space that maps local variables to registers.
    pub namespace: LocalNamespace,
    /// The nests the context holds.
    nest: Vec<NestData>,
    /// The function arguments being built.
    func_args: Vec<FuncArg>,
    /// The function argument lists being built.
    func_arg_stack: Vec<FuncArgList>,
    /// The array arguments being built.
    array_args: Vec<ArrayArg>,
    /// The array argument lists being built.
    array_arg_stack: Vec<ArrayArg>,
    /// The first register the frame uses.
    pub frame_base: i32,
    /// The greatest number of local variables the context held.
    pub max_variable_count: i32,
    /// The greatest number of frames the context needs.
    pub max_frame_count: i32,
    /// The place the function registration code starts at.
    pub function_register_code_point: i32,
    /// The number of the function's declared arguments.
    pub func_decl_arg_count: i32,
    /// The base of the function's unnamed argument array.
    pub func_decl_unnamed_arg_array_base: i32,
    /// The base of the function's collapsed argument.
    pub func_decl_collapse_base: i32,
    /// The expressions the super class of the class is held by.
    pub super_class_expr: Option<std::rc::Rc<super::node::Node>>,
    /// The places of the super class proxy's code.
    pub super_class_getter_pointer: Vec<i32>,
    /// The property's setter context.
    pub prop_setter: Option<usize>,
    /// The property's getter context.
    pub prop_getter: Option<usize>,
    /// The super-class proxy context.
    pub super_class_getter: Option<usize>,
    /// The members the context contributes to its parent object.
    pub properties: Vec<Property>,
    /// Whether the context has left the block.
    ///
    /// A member of a class that is registered again under the same name
    /// replaces the earlier one, and the earlier context is released; it
    /// leaves the block when nothing else holds it.
    pub removed: bool,
    /// The place of the jump the last closed `if` left to be patched.
    pub prev_if_exit_patch: i32,
}

impl Context {
    /// A context of the given kind.
    pub fn new(
        kind: ContextType,
        name: Option<String>,
        index: usize,
        parent: Option<usize>,
    ) -> Self {
        Self {
            kind,
            name,
            index,
            parent,
            code: Vec::new(),
            data: Vec::new(),
            source_pos: Vec::new(),
            prev_source_pos: -1,
            source_pos_sorted: true,
            jump_list: Vec::new(),
            fix_list: Vec::new(),
            non_local_function_decls: Vec::new(),
            namespace: LocalNamespace::new(),
            nest: Vec::new(),
            func_args: Vec::new(),
            func_arg_stack: Vec::new(),
            array_args: Vec::new(),
            array_arg_stack: Vec::new(),
            frame_base: 1,
            max_variable_count: 0,
            max_frame_count: 0,
            function_register_code_point: 0,
            func_decl_arg_count: 0,
            func_decl_unnamed_arg_array_base: 0,
            func_decl_collapse_base: -1,
            super_class_expr: None,
            super_class_getter_pointer: Vec::new(),
            prop_setter: None,
            prop_getter: None,
            super_class_getter: None,
            properties: Vec::new(),
            removed: false,
            prev_if_exit_patch: 0,
        }
    }

    /// The code stream of the context.
    pub fn code(&self) -> &[i32] {
        &self.code
    }

    /// The code stream of the context, for a change.
    pub fn code_mut(&mut self) -> &mut Vec<i32> {
        &mut self.code
    }

    /// The constant area of the context.
    /// The constant area, for a change.
    pub fn data_mut(&mut self) -> &mut Vec<Value> {
        &mut self.data
    }

    /// The constant area, for a change.
    pub fn data(&self) -> &[Value] {
        &self.data
    }

    /// The places in the script the code names.
    pub fn source_positions(&self) -> &[SourcePos] {
        &self.source_pos
    }

    /// The number of instructions the context holds.
    pub fn code_size(&self) -> i32 {
        self.code.len() as i32
    }

    /// The nests the context holds.
    pub fn nest_len(&self) -> usize {
        self.nest.len()
    }

    /// The nest at the given place.
    pub fn nest_at(&self, index: usize) -> &NestData {
        &self.nest[index]
    }

    /// The nest at the given place, for a change.
    pub fn nest_data_mut(&mut self, index: usize) -> &mut NestData {
        &mut self.nest[index]
    }

    /// Opens a nest.
    pub fn push_nest(&mut self, kind: NestKind) {
        self.nest.push(NestData {
            kind,
            has_variable: false,
            variable_created: false,
            is_first_case: false,
            ref_register: -1,
            start_ip: 0,
            loop_start_ip: 0,
            continue_patches: Vec::new(),
            exit_patches: Vec::new(),
            post_loop_expr: None,
            patch1: 0,
            patch2: 0,
        });
    }

    /// Closes the top nest.
    pub fn pop_nest(&mut self) -> Option<NestData> {
        self.nest.pop()
    }

    /// Puts an instruction into the code stream.
    ///
    /// The place in the script is what the source positions name; a place
    /// of `-1` means the instruction has no place of its own.
    pub fn put_code(&mut self, word: i32, position: i32) -> i32 {
        if position != -1 && self.prev_source_pos != position {
            self.prev_source_pos = position;
            self.source_pos_sorted = false;
            let code_pos = self.code.len() as i32;
            self.source_pos.push(SourcePos {
                code_pos,
                source_pos: position,
            });
        }
        self.code.push(word);
        self.code.len() as i32 - 1
    }

    /// Puts a value into the constant area.
    ///
    /// A value the area already holds is re-used: the last twenty
    /// entries are looked at, and the place of the match is returned.
    pub fn put_data(&mut self, value: Value) -> i32 {
        if !self.data.is_empty() {
            let mut count = 0;
            let mut index = self.data.len();
            while count < 20 {
                index -= 1;
                if self.data[index] == value {
                    return index as i32;
                }
                count += 1;
                if index == 0 {
                    break;
                }
            }
        }
        self.data.push(value);
        self.data.len() as i32 - 1
    }

    /// Records a jump whose target the fix-ups move.
    pub fn add_jump(&mut self) {
        self.jump_list.push(self.code.len() as i32);
    }

    /// The jumps of the context.
    pub fn jump_list(&self) -> &[i32] {
        &self.jump_list
    }

    /// The fix-ups of the context.
    pub fn fix_list(&self) -> &[FixData] {
        &self.fix_list
    }

    /// Adds a fix-up.
    pub fn add_fix(&mut self, fix: FixData) {
        self.fix_list.push(fix);
    }

    /// The functions declared outside the context.
    pub fn non_local_function_decls(&self) -> &[NonLocalFunctionDecl] {
        &self.non_local_function_decls
    }

    /// Adds a function that is declared outside the context.
    pub fn add_non_local_function_decl(&mut self, decl: NonLocalFunctionDecl) {
        self.non_local_function_decls.push(decl);
    }

    /// The function arguments being built.
    pub fn func_args(&self) -> &[FuncArg] {
        &self.func_args
    }

    /// The array arguments being built.
    pub fn array_args(&self) -> &[ArrayArg] {
        &self.array_args
    }

    /// The name of the context.
    pub fn name_text(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The places of the super class proxy's code.
    pub fn super_class_getter_pointer_mut(&mut self) -> &mut Vec<i32> {
        &mut self.super_class_getter_pointer
    }

    /// The members the context contributes to its parent object.
    pub fn properties(&self) -> &[Property] {
        &self.properties
    }

    /// Adds a member the context contributes to its parent object.
    pub fn add_property(&mut self, name: String, member: usize) {
        self.properties.push(Property { name, member });
    }

    /// The place of the member of the given name, when the context has
    /// one.
    pub fn property_index(&self, name: &str) -> Option<usize> {
        self.properties
            .iter()
            .position(|property| property.name == name)
    }

    /// The arguments of the function the context declares.
    pub fn func_arg_list(&self) -> &[FuncArg] {
        &self.func_args
    }

    /// Opens a function argument list.
    pub fn start_func_arg(&mut self) {
        self.func_arg_stack.push(FuncArgList::default());
    }

    /// Closes a function argument list.
    pub fn end_func_arg(&mut self) {
        self.func_arg_stack.pop();
    }

    /// Adds an argument to the argument list being built.
    pub fn add_func_arg(&mut self, register: i32, kind: FuncArgKind) {
        let Some(list) = self.func_arg_stack.last_mut() else {
            return;
        };
        list.args.push(FuncArg {
            register,
            kind,
            is_omit: false,
        });
        if kind != FuncArgKind::Normal {
            list.has_expand = true;
        }
    }

    /// Marks the argument list as omitting its arguments.
    pub fn add_omit_arg(&mut self) {
        if let Some(list) = self.func_arg_stack.last_mut() {
            list.is_omit = true;
        }
    }

    /// The code of the argument list.
    pub fn generate_func_call_arg_code(&mut self, position: i32) {
        let Some(list) = self.func_arg_stack.last().cloned() else {
            return;
        };
        if list.is_omit {
            // the arguments are omitted
            self.put_code(-1, position);
        } else if list.has_expand {
            // the arguments hold an expanding argument
            self.put_code(-2, position);
            self.put_code(list.args.len() as i32, position);
            for arg in &list.args {
                self.put_code(arg.kind as i32, position);
                self.put_code(opcode::vm_reg_addr(arg.register), position);
            }
        } else {
            self.put_code(list.args.len() as i32, position);
            for arg in &list.args {
                self.put_code(opcode::vm_reg_addr(arg.register), position);
            }
        }
    }

    /// Opens an array argument.
    pub fn push_array_arg(&mut self, object: i32, counter: i32) {
        self.array_arg_stack.push(ArrayArg {
            register: 0,
            object,
            counter,
        });
    }

    /// Closes an array argument.
    pub fn pop_array_arg(&mut self) {
        self.array_arg_stack.pop();
    }

    /// The object and the counter of the array argument being built.
    pub fn array_arg(&self) -> (i32, i32) {
        match self.array_arg_stack.last() {
            Some(arg) => (arg.object, arg.counter),
            None => (0, 0),
        }
    }

    /// The places the source positions name, sorted.
    fn sorted_source_positions(&mut self) {
        if !self.source_pos_sorted {
            self.source_pos.sort_by_key(|entry| entry.code_pos);
            self.source_pos_sorted = true;
        }
    }

    /// The code re-positioning and the patch processing.
    pub fn fix_code(&mut self) {
        // the class registers its members to the object it makes
        if self.kind == ContextType::Class {
            let code = vec![vm::REGMEMBER];
            self.fix_list.push(FixData {
                start_ip: self.function_register_code_point,
                size: 0,
                new_size: 1,
                code,
                before_insertion: true,
            });
        }

        // the functions that are declared outside the context are
        // registered so that they can be referred to before they are made
        if !self.non_local_function_decls.is_empty() {
            if self.max_frame_count < 1 {
                self.max_frame_count = 1;
            }

            let mut codesize = 2;
            for decl in &self.non_local_function_decls {
                if decl.change_this {
                    codesize += 10;
                } else {
                    codesize += 7;
                }
            }

            let mut code = Vec::with_capacity(codesize as usize);
            for decl in &self.non_local_function_decls {
                // const %1, #funcdata
                code.push(vm::CONST);
                code.push(opcode::vm_reg_addr(1));
                code.push(opcode::vm_reg_addr(decl.data_pos));

                // chgthis %1, %-1
                if decl.change_this {
                    code.push(vm::CHGTHIS);
                    code.push(opcode::vm_reg_addr(1));
                    code.push(opcode::vm_reg_addr(-1));
                }

                // spds %-1.#funcname, %1
                code.push(vm::SPDS);
                code.push(opcode::vm_reg_addr(-1));
                code.push(opcode::vm_reg_addr(decl.name_data_pos));
                code.push(opcode::vm_reg_addr(1));
            }

            // cl %1
            code.push(vm::CL);
            code.push(opcode::vm_reg_addr(1));

            self.fix_list.push(FixData {
                start_ip: self.function_register_code_point,
                size: 0,
                new_size: codesize,
                code,
                before_insertion: true,
            });

            self.non_local_function_decls.clear();
        }

        self.sorted_source_positions();

        // the fix-ups move the code, and the jumps that cross the moved
        // code are moved with it
        //
        // the jump list is walked once, and every jump a fix-up consumes
        // is erased, so no jump is moved twice
        let mut jumps = std::mem::take(&mut self.jump_list);
        for fix in &self.fix_list {
            let mut index = 0;
            while index < jumps.len() {
                let jump = jumps[index];
                let jump_target = self.code[(jump + 1) as usize] + jump;
                if jump >= fix.start_ip && jump < fix.size + fix.start_ip {
                    // the jump is in the moved code: it goes away
                    jumps.remove(index);
                    continue;
                }
                let target_moved = if fix.before_insertion {
                    jump_target < fix.start_ip
                } else {
                    (jump_target <= fix.start_ip) && jump > fix.start_ip + fix.size
                } || (jump < fix.start_ip
                    && jump_target >= fix.start_ip + fix.size);
                if target_moved {
                    self.code[(jump + 1) as usize] += fix.new_size - fix.size;
                }
                if jump >= fix.start_ip + fix.size {
                    // the jump itself moved
                    jumps[index] = jump + fix.new_size - fix.size;
                }
                index += 1;
            }
        }

        // the moved code
        for fix in &self.fix_list {
            let start = (fix.start_ip + fix.size) as usize;
            let end = self.code.len();
            if end > start {
                // the code after the moved part moves with it
                let tail = self.code.split_off(start);
                self.code
                    .resize(fix.start_ip as usize + fix.new_size as usize, 0);
                self.code.extend(tail);

                // the places in the script move with it
                for entry in self.source_pos.iter_mut() {
                    if entry.code_pos >= fix.start_ip + fix.size {
                        entry.code_pos += fix.new_size - fix.size;
                    }
                }
            }

            if fix.new_size != 0 {
                let start = fix.start_ip as usize;
                let end = start + fix.new_size as usize;
                if self.code.len() < end {
                    self.code.resize(end, 0);
                }
                self.code[start..end].copy_from_slice(&fix.code);
            }
        }

        self.jump_list = jumps;
        let jumps = std::mem::take(&mut self.jump_list);

        // the redundant jumps are eliminated
        for jump in &jumps {
            let mut jump_target = self.code[(*jump + 1) as usize] + *jump;
            let jump_code = self.code[*jump as usize];
            let mut address = *jump;
            address += self.code[(address + 1) as usize];
            loop {
                if self.code[address as usize] == vm::JMP
                    || (self.code[address as usize] == jump_code
                        && (jump_code == vm::JF || jump_code == vm::JNF))
                {
                    // a plain jump, or a jump after a jump of the same kind
                    jump_target = self.code[(address + 1) as usize] + address;
                    if self.code[(address + 1) as usize] != 0 {
                        address += self.code[(address + 1) as usize];
                    } else {
                        break;
                    }
                } else if (self.code[address as usize] == vm::JF && jump_code == vm::JNF)
                    || (self.code[address as usize] == vm::JNF && jump_code == vm::JF)
                {
                    // a jump of the other kind: the jump after it does not jump
                    jump_target = address + 2;
                    address += 2;
                } else {
                    break;
                }
            }
            self.code[(*jump + 1) as usize] = jump_target - *jump;
        }

        // the jump addresses become VM addresses
        for jump in &jumps {
            self.code[(*jump + 1) as usize] = opcode::vm_code_addr(self.code[(*jump + 1) as usize]);
        }

        self.jump_list = jumps;
        self.fix_list.clear();
    }
}

/// The names the contexts hold, for the diagnostics.
pub type ContextNames = BTreeMap<usize, Option<String>>;
