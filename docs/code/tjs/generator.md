# `tjs::vm::codegen` — the code generator

Source: `crates/tjs/src/vm/codegen/` · module `tjs::vm::codegen` · L4

The code generator turns the parser's expression tree into the
**intermediate code** the VM executes: one code stream per context, the
constant area, the variable area, the function-argument area, the labels
and the line table. The generator is the third of the three Rust script
columns (the lexer is the first, the parser the second); the column it
replaces is `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp`.

Two interfaces sit over one implementation:

| interface | where | what it is |
|---|---|---|
| the native Rust API | `crates/tjs/src/vm/codegen/` | the tree the parser makes, in and the code out |
| the shipped C ABI | `crates/tjs/src/vm/codegen/ffi.rs`, `crates/tjs/cpp/tjs2_codegen.h` | the image of a script block, for the C++ VM |
| the proof instrument | `crates/tjs/src/vm/codegen/proof.rs` | node-building calls, so the two code streams can be compared word for word; **not** the shipped interface |

## 1. What the generator does

A **context** is the intermediate code of one function, class or handler.
It holds the code stream, the constant area, the name space that maps
local variables to registers, and the fix-ups that are applied when the
context is finished. The generator makes a context per function, class
and handler of the script block, and the **script block** is the tree of
them.

The generator walks the expression tree the parser built and emits the
instructions the executor runs. Every instruction carries the place in
the script it came from, so the executor's error messages and the
debugger can name the line.

Two things are worth stating plainly, because both are easy to get
wrong:

* **The place of an instruction comes from the analyser, not from the
  node.** The parser reads the lookahead before it reduces a rule whose
  last symbol is an expression, and a node is made when the rule that
  makes it is complete — so the node's place is the analyser's place at
  that moment, and it is the place the generator gives the node.
* **The `while` loop and the `do` loop hold the same nest.** The
  difference between them is in the place of the condition, and the
  condition's code reads it.

## 2. The rules the generator implements

Every rule below is the reference's own behaviour, with the place it is
defined. The rules are what the column is written against; the tests
that prove them cite the same lines.

### 2.1 The context — `context.rs`

| rule | reference |
|---|---|
| the code context: the intermediate code of one function, class or handler, with its code stream, constant area, name space and fix-ups | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:246-680` (`tTJSInterCodeContext`) |
| the name of a context kind | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:428` (`GetContextTypeName`) |
| the number of variables a context kind reserves: a property reserves none, every other kind two | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:239-261` |
| the name of the context | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3765` (`GetName`) |
| a value the constant area already holds is re-used: the last twenty entries are looked at | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:638-690` |
| the code of a function-argument list | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3683-3707` (`GenerateFuncCallArgCode`) |
| the code re-positioning and the patch processing | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:702-884` (`FixCode`) |
| the jump list is walked once and every jump a fix-up consumes is erased, so no jump moves twice | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:779-806` |
| the place of the jump the last closed `if` left to be patched | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:2921-3651` (the `if` commands) |

### 2.2 The emission — `emit.rs`

| rule | reference |
|---|---|
| the emission of one node: the operation switch | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:1110-2736` (`tTJSInterCodeContext::GenNodeCode` / `tTJSInterCodeContext::_GenNodeCode`) |
| the statements the grammar drives | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:2737-3800` |
| the tree has a shape the generator does not support: a typed error, never a silent one | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:1110-2736` |
| the emission and the context management are one thing | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:246-680` (one class holds both) |
| the place of an instruction comes from the analyser, not from the node | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:25` (`LEX_POS`) |
| the generator holds that place and the emission writes it, and the parser refreshes it where it reads a token | `reference/krkr2/cpp/core/tjs2/tjsLex.cpp:1656-1658` (`Current - Script`) |
| the global context cannot reach a local variable, so the name space is not consulted | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:2200-2210` |
| the declaration of a local variable | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:2845-2884` (`AddLocalVariable`) |
| the sub-types that modify the left hand side, from the pre-increment to the type query | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:1110-2736` (the operation switch) |
| the sub-type of an operation on a member is the opcode of that operation, and the sub-types that have no opcode of their own start where the opcodes end | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:135-153` (the `tTJSSubType` enum) |

### 2.3 The super class — `extends.rs`

| rule | reference |
|---|---|
| the code of the super class of a class | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3601-3651` (`tTJSInterCodeContext::CreateExtendsExprCode`; `tTJSInterCodeContext::CreateExtendsExprProxyCode` at `:3636`) |
| a **proxy context** is made for the super class: a context of its own, whose parent is the class, and whose name is the class's name | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3601-3651` |
| the proxy has its own frame, so the class's own code is not affected by it, and the proxy is never popped, so it is never compared with the Rust side's own contexts | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3601-3651` |
| the proxy context is made after the class's function registration point is set, and before the class's body is made | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3601-3651` |

### 2.4 The folding — `folding.rs`

| rule | reference |
|---|---|
| the constant folding | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3812-3990` |
| the folding follows the variant's own semantics: the operations are the variant's operators | `reference/krkr2/cpp/core/tjs2/tjsVariant.h:475` (the variant and its operators) |
| a conversion that has no result simply does not fold | `reference/krkr2/cpp/core/tjs2/tjsVariant.h:475` (the variant's conversion operators) |
| the ordered comparison is numeric | `reference/krkr2/cpp/core/tjs2/tjsVariant.h:1058` (`operator<`) |
| a node with one child folds when the operand folds | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3812-3886` (`MakeNP1`) |
| a node with two children folds when the operands fold | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3888-3980` (`MakeNP2`) |

### 2.5 The generator — `mod.rs`

| rule | reference |
|---|---|
| the generator makes the intermediate code of a script block: the top level and every function, class and handler it holds | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp` (`tTJSInterCodeContext`) |
| the tree has a shape the generator does not support: a typed error | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:1110-2736` |
| a value that cannot be converted where the generator converts it | `reference/krkr2/cpp/core/tjs2/tjsVariant.h:475` (the variant's conversion operators) |
| the finishing of a context | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:952-1039` (`Commit`) |
| the declaration of a local variable | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:2845-2884` (`AddLocalVariable`) |

### 2.6 The statements — `statements.rs`

| rule | reference |
|---|---|
| the commands the grammar drives: `Enter*Code`, `Create*ExprCode`, `Exit*Code` | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:2921-3651` |
| the kind of a nest | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:282` (`tNestType`) |
| the code of a `break` | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3362-3397` (`DoBreak`) |
| the code of a `continue` | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3400-3449` (`DoContinue`, defined at `:3399`) |
| the code of a `return` | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3457-3500` (`ReturnFromFunc`) |
| the code of a `throw` | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3581-3599` (`ProcessThrowCode`) |
| the code that opens a `try` | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3502-3519` (`EnterTryCode`) |
| the code that opens the `catch` of a `try` | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3521-3553` (`EnterCatchCode`) |
| the code that closes a `try` | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3555-3579` (`ExitTryCode`) |
| the declaration of a function's argument | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3708-3745` (`AddFunctionDeclArg`) |
| the declaration of a function's collapsed argument | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3747-3765` (`AddFunctionDeclArgCollapse`, defined at `:3741`) |
| the declaration of a property setter's argument | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3757-3766` (`SetPropertyDeclArg`) |
| the third clause of a `for` is held by the context and made when the loop is left | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3114` (`SetForThirdExprCode`) |
| the local variables are not cleared: the registers are cleared by the frame's own bookkeeping | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:2798-2816` (`ClearLocalVariable` — its whole body is inside `#if 0`, so the reference emits nothing either) |

### 2.7 The values — `value.rs`

| rule | reference |
|---|---|
| the folding follows the variant's own semantics | `reference/krkr2/cpp/core/tjs2/tjsVariant.h:475` (the variant and its operators) |
| the values carry the variant's distinctions | `reference/krkr2/cpp/core/tjs2/tjsVariant.h:475` (`tTJSVariant`) |
| the identity of an object is the address of its dispatch: what is printed and what is compared | `reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:187` (the object text) and `:665` (`NormalCompare`) |
| an object converts to `(object <address>:<address>)`; the null prints as `(object 0x0:0x0)` | `reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:187` (`TJSObjectToString`) |
| the conversion fails when the value has no numeric conversion; an object has none | `reference/krkr2/cpp/core/tjs2/tjsVariant.h:475` (the variant's conversion operators) |
| the comparison converts a value to a string when the two values are of different kinds and one of them is a string | `reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:665` (`NormalCompare`) |
| an object is true when it has a dispatch; the null has none | `reference/krkr2/cpp/core/tjs2/tjsVariant.h:911` (`operator bool`) |
| the comparison of two values | `reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:665` |
| the conversion error is caught and reported as "the two values are not equal"; the comparison never fails | `reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:665` |
| the conversion is the only failure the comparison can have | `reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:665` |
| both values are converted to a string when one of the two is a string | `reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:665` |
| the object text holds the address of the dispatch and the address of the `objthis` | `reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:187` |
| the real is printed with the shortest form that reads back as the same number | `reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:241` (`TJSRealToString`) |
| a constant array: the `(const)[...]` form's array, and the elements it puts in it | `reference/krkr2/cpp/core/tjs2/bison/tjs.y:796-806` |
| a constant dictionary: the `(const)%[...]` form's dictionary, and the entries it puts in it | `reference/krkr2/cpp/core/tjs2/bison/tjs.y:831-841` |
| a dictionary entry's name is a value and not a text: the side that builds the object converts it | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:132-139` (the element is added with a text for the name) |

### 2.8 The object operations — `objects.rs`

| rule | reference |
|---|---|
| the operations that reach into an object: member access, call, variable, `this`, inline arrays and dictionaries | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:1799-2736` (the second half of `_GenNodeCode`) |
| the left hand side of an operation cannot be modified | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:1110-2736` (the operation switch; the column raises a typed error where the reference has no arm) |

### 2.9 The operations — `operations.rs`

| rule | reference |
|---|---|
| the emission is a switch over the opcode of a node | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:1110-2736` (`tTJSInterCodeContext::_GenNodeCode`) |
| the left hand side of an operation cannot be modified | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:1110-2736` (the operation switch; the column raises a typed error where the reference has no arm) |
| an operation that has no result raises | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:1110-2736` (the operation switch; the column raises a typed error where the reference has no arm) |

### 2.10 The name space — `namespace.rs`

| rule | reference |
|---|---|
| the compile-time name space: a name maps to a register, and a removed name frees its register for good | `reference/krkr2/cpp/core/tjs2/tjsNamespace.cpp:106-224` (`tTJSLocalNamespace`) |

### 2.11 The tree — `node.rs`

| rule | reference |
|---|---|
| the node is an opcode, a place in the script, a value and a list of children | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:178-236` (`tTJSExprNode`) |

### 2.12 The opcodes — `opcode.rs`

| rule | reference |
|---|---|
| the opcodes are the intermediate VM's, in the VM's order; the executor reads the stream the generator writes | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:44-137` (`tTJSVMCodes`) and `reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:862` (the stream the executor runs) |
| the list of the opcodes, in the same order and with the same numbers | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:44-137` |
| the register address multiplies the register number by the size of a variant (20 bytes) | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:35` (`TJS_TO_VM_REG_ADDR`) |
| the register address is the register number times the size of a variant | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:35` |
| the code address is the code position times the size of a word | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:34` (`TJS_TO_VM_CODE_ADDR`) |

### 2.13 The tokens — `token.rs`

| rule | reference |
|---|---|
| the numbering is the grammar's: the token numbers are part of the tree the generator walks, so they must match the grammar's exactly | `reference/krkr2/cpp/core/tjs2/bison/tjs.y:44-186` (and the generated `tjs.tab.hpp`) |

### 2.14 The C ABI — `ffi.rs`

| rule | reference |
|---|---|
| the script-block interface hands the native executor the image of a script block | `reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:862` (the code the executor runs) |
| the node-building interface lets the native grammar drive the Rust generator | `reference/krkr2/cpp/core/tjs2/bison/tjs.y` |
| no entry point unwinds into the native caller | `reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:862` (the native call site) |
| the tree has a shape the generator does not support: a typed error | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:1110-2736` |
| the error's detail is read separately, through `tjs2_codegen_last_error_message`, and never as a Rust type across the boundary | `reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:862` (the native call site) |
| the line table: the text is split on `\r` and `\n`, a `\r\n` counts as one break, and every line contributes its start offset and its length in code units, without the break | `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:207-232` |
| the constant array's kind and the constant dictionary's kind: the image carries the elements and the side that has the object builds it | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:123-130` (the array's `add`), `:132-139` (the dictionary's `PropSet`) |
| the class's super-class proxy: the class makes it once and keeps it | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3626-3632` |

### 2.15 The proof instrument — `proof.rs`

| rule | reference |
|---|---|
| the node-building interface lets the native grammar drive the Rust generator, so the two code streams can be compared word for word | `reference/krkr2/cpp/core/tjs2/bison/tjs.y` (the grammar) |
| the native side consumes the script-block image | `reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:862` |
| no entry point unwinds into the native caller | `reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:862` (the native call site) |
| the kind of a context, from the native number | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:282` (the context's kind enum) |
| the kind of a value, from the native number | `reference/krkr2/cpp/core/tjs2/tjsVariant.h:103` (`tTJSVariantType`) |
| the constant expressions are folded where the node is made | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3812-3886` (`MakeNP1`) |

### 2.16 The tests — `tests.rs`

| rule | reference |
|---|---|
| the image builder's tests are about what the native side reads: the code stream, the constant area, the variable area, the function-argument area, the labels and the line table | `reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:862` |
| the context closes with the return sequence | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3457-3500` (`ReturnFromFunc`: `SRV`, the value, `RET`) |

### 2.17 The module root — `crates/tjs/src/vm/mod.rs`

| rule | reference |
|---|---|
| the script columns replace the vendored ones | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp` (the generator being replaced) |
| the columns replace the vendored tree beside them | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp` |
| the module root is the front end's door: the compile entry lives there | `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:421` (the reference's own compile entry) |

## 3. The proofs the column still owes

* The corpus comparison: the reference's image and the column's, field for
  field, over the game's own scripts. The measure is **identical images**,
  not equivalent behaviour: the same contexts, the same code words, the
  same constants, the same variable and argument counts, the same labels,
  the same source positions. The gate is 172 of the 172 comparable scripts
  and every context of the real tree. The gate itself is written out in
  `seam.md` §10.
* The tests that cite the rules above: each rule in §2 wants a test that
  cites its line. The column's tests are owed; the rules are the
  specification they are written against.

## 4. The module map

| page | covers |
|---|---|
| [`columns.md`](columns.md) | the three script columns and the seam between them |
| [`generator.md`](generator.md) | this page: the code generator and its rules |
| [`parser.md`](parser.md) | the parser and the grammar's productions |
| [`seam.md`](seam.md), [`seam-fields.md`](seam-fields.md) | the compile entry, the C ABI and the field-by-field table of the script block image |
