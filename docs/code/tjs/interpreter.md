# `tjs::vm::runtime::exec` — the interpreter

Source: `crates/tjs/src/vm/runtime/exec/` · module `tjs::vm::runtime::exec` · L4

The machine that runs the intermediate code. The front end compiles script text
into a script block — a tree of contexts, each with a code stream, a constant
area and a frame size — and this module executes that block: it allocates a
register window per call, dispatches one instruction at a time, reaches an
object's members, performs a call, and unwinds a `try` when something throws.

It is the fifth part of the runtime column (`TODO.md` §12.1, R5) and the part
that deletes `tjsInterCodeExec.cpp`, `tjsInterCodeContext.*` and the executing
half of `tjs.cpp`. The value layer, the variant and the object model it runs on
are already in `crates/tjs/src/vm/runtime/`; the pages for those are
[`runtime.md`](runtime.md) and, for the front end that feeds it,
[`generator.md`](generator.md).

This page is the **plan**: the instruction set as it actually is, the execution
model, the exception machinery, the surface the interpreter needs from the
runtime, the order the parts land in, and the inline cache and the A/B
measurement that decide whether it ships. It is written against the reference
checkout, which is the oracle, and against the tree as it stands
(`crates/tjs/cpp/tjs2/`), which is what the parts delete.

---

## 1. The shape of the machine

The VM is **register-based, not stack-based**. There is no operand stack. Every
context carries a fixed frame size, chosen at compile time, and every
instruction names registers by byte-address arithmetic rather than by index:

| encoding | definition |
|---|---|
| a register address | the register number times `sizeof(tTJSVariant)`, 20 bytes (`reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:35`) |
| a code address | the code position times `sizeof(tjs_uint32)` (`reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:34`) |
| a word | one `tjs_int32`; the opcode, then its operands, all words |

A register address is a **byte offset from the frame base**, and it is signed:
the frame grows downward for the locals and the declared arguments, upward for
the temporaries. That is why one instruction can name both `%3` and `%-1`, and
why a register number is not an index into an array.

The areas one execution owns:

| area | what lives there | where it comes from |
|---|---|---|
| the **code stream** | the words of the context being executed | the script block, immutable after `Commit` |
| the **constant area** | the literals, and every member name the code names | the script block, immutable after `Commit` |
| the **register window** | one call's locals, declared arguments, temporaries, and the `this` triple | allocated per call, released on return |
| the **condition flag** | one bit, written by the compare/truth instructions and read by the jumps | a local of the execute loop |
| the **context stack** | which context is executing, for a trace and an error message | the recursion itself |

The register window has a **fixed layout relative to the frame base `ra`**, and
the layout is the contract every instruction is written against:

| register | what it holds |
|---|---|
| `ra[+0] … ra[+MaxFrameCount]` | the temporaries, growing upward from `ra`; `ra[0]` is the **discard register** — an operand address of `0` asks for no result (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2114`, `:1695`, `:1822`) |
| `ra[-1]` | `this` — the object the frame's members are read from |
| `ra[-2]` | the **objthis-context**: a proxy whose object is `ra[-1]` and whose fallback is the global object |
| `ra[-3]`, `ra[-4]`, … | the declared arguments, in order |
| `ra[-3-n]` and below | the local variables, `n` being the name space's index |

The last two rows are **one area, not two**. `AddFunctionDeclArg` adds the
argument's name to the local name space and then stores to register
`-3 - FuncDeclArgCount` (`reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3719`,
`:3730`), and every other local is addressed as
`-n - VariableReserveCount - 1` (`:2864`). With `VariableReserveCount` at 2 for
a function, name-space index 0 is register `-3` whichever way the name arrived.
The declared arguments and the locals are the same downward run of registers,
filled from opposite ends. `VariableReserveCount` is the number of registers
reserved for the `this` triple: 2 for every context kind except `ctProperty`,
which reserves none (`:227-253`).

The window's size is `MaxVariableCount + VariableReserveCount + 1 +
MaxFrameCount` variants, allocated as one block, with `ra` placed at
`MaxVariableCount + VariableReserveCount` into it
(`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:653-657`). The block comes
from a variant array stack that hands out 1 024-variant pages and reuses them
(`:472`, `:578-610`).

---

## 2. The instruction set

### 2.1 The encoding

The encoding is described by one table in the tree, not by the executor: an
opcode maps to a **fixed operand list** plus an optional **tail**
(`crates/tjs/cpp/tjs2/tjsVMCodes.h:133-165`, built by `MakeVMOpLayouts` at
`:182`). Three operand kinds and two tail kinds exist:

| kind | meaning |
|---|---|
| `Plain` | a value that is not an address — the argument count, or a register count |
| `Reg` | a register address: a byte offset that a bytecode translator scales by 20 |
| `Code` | a code address: a byte offset that a bytecode translator scales by 4 |

| tail | meaning |
|---|---|
| `None` | the fixed operands are the whole instruction |
| `RegList` | a count, then that many register operands. `-1` means *no argument*; `-2` means *expanded arguments* and is followed by a pair count and that many `(flag, register)` pairs |

The same table drives the bytecode writer, the bytecode loader and the address
translator (`crates/tjs/cpp/tjs2/tjsVMCodes.h:402`), and a compile-time
`static_assert` refuses an opcode without a layout (`:383`). The interpreter
declares the same table once and derives its decoder from it, so a stream the
loader accepts is a stream the interpreter can execute, or refuses with a typed
error — never one it mis-reads.

### 2.2 The table

Notation: `%d` is the destination register, `%s`/`%a`/`%b`/`%o`/`%p`/`%n`
are source registers, `@k` is a slot of the constant area, `c` is a code
address, `f` is the condition flag. "In place" in the **operands** column means
the register named is both operand and destination. The **effect** column says
what the instruction does to the register window and the flag; nothing here
pushes or pops, because the machine has no operand stack. All 128 opcodes are
implemented by the executor — the dispatch covers the enum exhaustively and
falls through to an invalid-opcode error (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1300`).

The **gen** column says whether the code generator can emit the opcode. It is
the answer to *what is reachable*: a Rust interpreter may still have to execute
an unreachable opcode, because a script block can arrive as `.tjsb` from a
third-party writer, but it can never be asked to *produce* one.

#### Control, comparison, the flag — 0 … 17

| opcode | # | operands | effect | gen |
|---|---|---|---|---|
| `NOP` | 0 | — | nothing | yes — the super-class proxy placeholder (`tjsInterCodeGen.cpp:3649`) |
| `CONST` | 1 | `%d @k` | `%d = copy of const[k]` | yes |
| `CP` | 2 | `%d %s` | `%d = %s` | yes |
| `CL` | 3 | `%d` | `%d = void` | yes |
| `CCL` | 4 | `%d n` | the `n` registers from `%d` cleared | **no** |
| `TT` | 5 | `%s` | `f = truth(%s)` | yes |
| `TF` | 6 | `%s` | `f = !truth(%s)` | yes |
| `CEQ` | 7 | `%a %b` | `f = a == b` (the normal compare) | yes |
| `CDEQ` | 8 | `%a %b` | `f = a === b` (the strict compare) | yes |
| `CLT` | 9 | `%a %b` | `f = a < b` | yes |
| `CGT` | 10 | `%a %b` | `f = a > b` | yes |
| `SETF` | 11 | `%d` | `%d = f`, as an integer | yes |
| `SETNF` | 12 | `%d` | `%d = !f`, as an integer | yes |
| `LNOT` | 13 | `%d` in place | `%d = !%d`, as an integer | yes |
| `NF` | 14 | — | `f = !f` | yes |
| `JF` | 15 | `c` | `ip += c` when `f` | yes |
| `JNF` | 16 | `c` | `ip += c` when `!f` | yes |
| `JMP` | 17 | `c` | `ip += c` | yes |

The flag is **not** a register. It survives across instructions that do not
touch it, which is what lets `TT` and `JNF` be separated by the operands'
evaluation, and what `NF` exists to invert. Its materialisation is an integer,
not a separate boolean kind (`tjsVariant.cpp:1057-1062` for the in-place form).

#### Increment and decrement — 18 … 25

| opcode | # | operands | effect | gen |
|---|---|---|---|---|
| `INC` | 18 | `%d` in place | `%d = %d + 1` | yes |
| `INCPD` | 19 | `%d %o @k` | `%d = ++o[k]` — the property's own increment operation | yes — unary `*` |
| `INCPI` | 20 | `%d %o %n` | the same, the name read from `%n` | yes — unary `*` |
| `INCP` | 21 | `%d %o` | the same, as the object's *closure* operation | yes — unary `*` |
| `DEC` | 22 | `%d` in place | `%d = %d - 1` | yes |
| `DECPD` | 23 | `%d %o @k` | `%d = --o[k]` | yes — unary `*` |
| `DECPI` | 24 | `%d %o %n` | the same, the name read from `%n` | yes — unary `*` |
| `DECP` | 25 | `%d %o` | the same, as the closure operation | yes — unary `*` |

#### The two-operand operators — 26 … 81

Fourteen operators, each a family of four consecutive opcodes: the plain form,
the **direct** property form (`PD`), the **indirect** property form (`PI`), and
the **closure** property form (`P`). The family base numbers are
`LOR 26, LAND 30, BOR 34, BXOR 38, BAND 42, SAR 46, SAL 50, SR 54, ADD 58,
SUB 62, MOD 66, DIV 70, IDIV 74, MUL 78`; the order and the `+1/+2/+3` offsets
are the reference's own (`reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:42`).

| opcode | # | operands | effect | gen |
|---|---|---|---|---|
| `OP` | base | `%d %s` | `%d op= %s` | yes |
| `OPPD` | base+1 | `%d %o @k %p` | `%d = Object.Operation(op, o[k], %p)` | yes |
| `OPPI` | base+2 | `%d %o %n %p` | the same, the name read from `%n` | yes |
| `OPP` | base+3 | `%d %o %p` | the same, as the closure operation | yes |

| family | operator | family | operator |
|---|---|---|---|
| `LOR` | `\|\|=` | `SAR` | `>>=` |
| `LAND` | `&&=` | `SAL` | `<<=` |
| `BOR` | `\|=` | `SR` | `>>>=` |
| `BXOR` | `^=` | `ADD` | `+=` |
| `BAND` | `&=` | `SUB` | `-=` |
| `MOD` | `%=` | `DIV` | `/=` |
| `IDIV` | `\=` | `MUL` | `*=` |

Note that `OPPD`/`OPPI`/`OPP` **do not read the member and write it back**. They
hand the operation to the object, so a container or a property that defines a
compound assignment sees one operation rather than a read and a write
(`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1592-1690`).

#### The single-operand operators — 82 … 98

| opcode | # | operands | effect | gen |
|---|---|---|---|---|
| `BNOT` | 82 | `%d` in place | `%d = ~%d` | yes |
| `TYPEOF` | 83 | `%d` in place | `%d = typeof %d`, one of `"void"`, `"Object"`, `"String"`, `"Integer"`, `"Real"`, `"Octet"` (`:2601-2637`) | yes |
| `TYPEOFD` | 84 | `%d %o @k` | `%d = typeof o[k]`; a member that is not there is the text `"undefined"` (`:1951`) | yes |
| `TYPEOFI` | 85 | `%d %o %n` | the same, the name read from `%n` | yes |
| `EVAL` | 86 | `%d` in place | `%d = eval(%d)` — compile and run the text as an expression, result needed | yes |
| `EEXP` | 87 | `%d` in place | the same, with no result | yes |
| `CHKINS` | 88 | `%t %n` | `%t = (n is a class name %n is an instance of)` | yes |
| `ASC` | 89 | `%d` in place | `%d = the character code of the first unit` | yes |
| `CHR` | 90 | `%d` in place | `%d = the one-unit string of the code` | yes |
| `NUM` | 91 | `%d` in place | `%d = +%d`, the number conversion | yes |
| `CHS` | 92 | `%d` in place | `%d = -%d` | yes |
| `INV` | 93 | `%d` in place | `%d = invalidate %d`; a value that is not an object is `false` (`:1070-1080`) | yes |
| `CHKINV` | 94 | `%d` in place | `%d = isvalid %d`; a value that is not an object is `true` (`:1082-1093`) | yes |
| `INT` | 95 | `%d` in place | `%d = int %d` | yes |
| `REAL` | 96 | `%d` in place | `%d = real %d` | yes |
| `STR` | 97 | `%d` in place | `%d = string %d` | yes |
| `OCTET` | 98 | `%d` in place | `%d = octet %d` | yes |

#### Calls — 99 … 102

All four end with the `RegList` tail (`crates/tjs/cpp/tjs2/tjsVMCodes.h:345-364`).

| opcode | # | operands | effect | gen |
|---|---|---|---|---|
| `CALL` | 99 | `%r %o` + args | `%r = o(args)`; `%r` may be `%0` | yes |
| `CALLD` | 100 | `%r %o @k` + args | `%r = o.k(args)`, the name from the constant area | yes |
| `CALLI` | 101 | `%r %o %n` + args | `%r = o[n](args)`, the name from a register | yes |
| `NEW` | 102 | `%r %o` + args | `%r = new o(args)` | yes |

#### Member access — 103 … 117

The direct forms take the member's name from the constant area (`@k`); the
indirect forms take it from a register (`%n`). The suffix letters are the
reference's: `D` direct, `I` indirect, `S` **ignore the property** — read or
write the member's value without invoking its getter or setter (`:103`,
`tjsInterCodeGen.cpp:2384`).

| opcode | # | operands | effect | gen |
|---|---|---|---|---|
| `GPD` | 103 | `%d %o @k` | `%d = o[k]` | yes |
| `SPD` | 104 | `%o @k %v` | `o[k] = %v`, the member must exist | yes |
| `SPDE` | 105 | `%o @k %v` | `o[k] = %v`, created when absent | yes |
| `SPDEH` | 106 | `%o @k %v` | the same, created **hidden** | **no** |
| `GPI` | 107 | `%d %o %n` | `%d = o[n]` | yes |
| `SPI` | 108 | `%o %n %v` | `o[n] = %v` | yes |
| `SPIE` | 109 | `%o %n %v` | the same, created when absent | yes |
| `GPDS` | 110 | `%d %o @k` | `%d = o[k]`, the property ignored | yes |
| `SPDS` | 111 | `%o @k %v` | `o[k] = %v`, created, property ignored | yes |
| `GPIS` | 112 | `%d %o %n` | `%d = o[n]`, the property ignored | yes |
| `SPIS` | 113 | `%o %n %v` | `o[n] = %v`, created, property ignored | yes |
| `SETP` | 114 | `%o %v` | `*%o = %v` — write through the member as a property object | yes |
| `GETP` | 115 | `%d %s` | `%d = *%s` — read through it | yes |
| `DELD` | 116 | `%d %o @k` | `%d = delete o[k]`; `%d` may be `%0` (`:1806-1834`) | yes |
| `DELI` | 117 | `%d %o %n` | the same, the name from a register (`:1837-1868`) | yes |

`GETP`/`SETP` are the unary `*` operator: the member is forced to be a
*property object* and the object's own property surface is bypassed
(`tjsInterCodeGen.cpp:2374`, `:2381`).

#### Return, the try boundary, throw, the context — 118 … 127

| opcode | # | operands | effect | gen |
|---|---|---|---|---|
| `SRV` | 118 | `%v` | copy `%v` into the caller's result; nothing when the caller passed no result slot (`:1244`) | yes |
| `RET` | 119 | — | return from this context | yes |
| `ENTRY` | 120 | `c %r` | enter a `try`-protected block; the catch is at `c`, the exception object goes to `%r` | yes |
| `EXTRY` | 121 | — | leave a `try` block; the same as `RET` (`:1263`) | yes |
| `THROW` | 122 | `%v` | throw `%v` as a script exception (`:1266-1270`) | yes |
| `CHGTHIS` | 123 | `%c %o` | rebind the closure in `%c` so its receiver is `%o` | yes |
| `GLOBAL` | 124 | `%d` | `%d = the global object` | yes |
| `ADDCI` | 125 | `%o %v` | add `%v` to the object's class-instance information (`:1285`, `:2245-2256`) | yes |
| `REGMEMBER` | 126 | — | publish this context's members onto `this`, so a class's functions become the instance's methods (`:1290`, `:2708-2740`) | yes — inserted by the context's fix-up, not by the node walk (`tjsInterCodeGen.cpp:711`) |
| `DEBUGGER` | 127 | — | break into the native debugger | yes |

### 2.3 What the generator can emit — 126 of 128

**126 of the 128 opcodes are reachable from the code generator. Two are not.**

| unreachable | why |
|---|---|
| `CCL` (4) | the only emission sites are inside `#if 0` blocks: `ClearLocalVariable` and `ClearFrame` both emit nothing, and the frame is cleared by the frame's own bookkeeping instead (`tjsInterCodeGen.cpp:2793-2811`, `:2814-2837`) |
| `SPDEH` (106) | no generator path emits it at all — the write and class-member paths use `SPD`/`SPDE`/`SPDS` (`:2051-2063`, `:2871-2880`) |

Both remain part of the instruction set. The executor implements them
(`tjsInterCodeExec.cpp:893`, `:1186`), the disassembler names them
(`tjsDisassemble.cpp:253`, `:420`), and the loader's layout table accepts them,
so a block that arrives as `.tjsb` may contain either. **The interpreter
implements 128; the generator produces 126.**

The `P`, `PD` and `PI` variants are easy to mistake for dead. They are not:
the property forms are emitted arithmetically — `param.SubType + 1` and `+ 2`
for a member left-hand side (`tjsInterCodeGen.cpp:2081`) and
`param.SubType + 3` for a forced property access (`:2402`), with the same
offsets for `INC`/`DEC` (`:2097`, `:2121`, `:2419`, `:2442`). The Rust generator
computes the same three offsets (`crates/tjs/src/vm/codegen/objects.rs:709`,
`:730`, `:759`), so the port's own front end already reaches them.

### 2.4 The surprises

Six rules that reading the opcode names will not give you. Each one is a
behaviour a port gets wrong silently.

1. **`CLT` compares *less-than*, but the variant method it calls is named
   `GreaterThan`.** `VM_CLT` evaluates `a.GreaterThan(b)`
   (`tjsInterCodeExec.cpp:920-924`), and `tTJSVariant::GreaterThan` answers
   `a < b` (`tjsVariant.cpp:856-875`). `CGT`/`LittlerThan` are the mirror
   (`:926-930`). `a <= b` is compiled as `CGT` followed by `SETNF`
   (`tjsInterCodeGen.cpp:1651-1656`), so the inversion is deliberate. The
   opcode names are right and the method names are inverted.
2. **`DELETE` answers `true`/`false`, not the member's value**, and answers it
   through the *result register*, which may be `%0`
   (`tjsInterCodeExec.cpp:1823-1826`). A failed `delete` is not an error.
3. **`TYPEOFD` swallows a missing member as the text `"undefined"`** while
   `TYPEOF` of a present-but-unset member is `"void"`
   (`tjsInterCodeExec.cpp:1951`, `:2603-2626`). `TYPEOFD` is called with
   `TJS_MEMBERMUSTEXIST`, so an object whose member read means something else
   by absence still answers `"undefined"` (`:1119-1122`).
4. **A string and an octet are their own member surface.** `GPD`/`SPD`/`GPI`/
   `SPI` dispatch to `GetStringProperty`/`SetStringProperty`/`GetOctetProperty`
   before they ever treat the value as an object
   (`tjsInterCodeExec.cpp:1400-1428`, `:38-88`, `:91-120`). `"abc".length` and
   `"abc"[1]` are therefore not object reads, and a write to either raises
   *access denied* rather than creating a member.
5. **A member read can reorder the object.** The object's lookup moves a member
   that it found more than two places down its chain to the chain's head
   (`crates/tjs/src/vm/runtime/member.rs`, `find`), so a read changes the order
   `for (name in obj)` walks. An interpreter that memoises a read into the
   object breaks that.
6. **An integer subscript takes a different entry than a text one.** `GPI` with
   an integer key calls `PropGetByNum`, whose default is the decimal text of the
   number but which a container may override to answer an index without building
   the text (`tjsInterCodeExec.cpp:1529`, `tjsObject.cpp:144-168`).

---

## 3. The execution model

### 3.1 A call

`ExecuteAsFunction` is the whole prologue, and its order is the contract
(`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:635-780`):

| step | what it does |
|---|---|
| 1 | allocate `MaxVariableCount + VariableReserveCount + 1 + MaxFrameCount` variants from the array stack (`:653-657`) |
| 2 | place `ra` past the variable area (`:655`) |
| 3 | build the **objthis-context** into `ra[-2]`: a proxy over `objthis`, falling back to the global object (`:659-672`) |
| 4 | set `ra[-1] = (objthis, objthis)` (`:707`) |
| 5 | clear `ra[0]` (`:708`) |
| 6 | copy the declared arguments into `ra[-3]`, `ra[-4]`, …, clearing the ones not supplied (`:711-731`) |
| 7 | when the signature has a named collapse argument, build the array from the surplus arguments (`:734-750`) |
| 8 | run `ExecuteCode` (`:752`) |
| 9 | clear `ra[-2]` and hand the block back (`:766-771`) |

Steps 3 and 4 are the reason `this` is not simply a parameter. `ra[-1]` is the
receiver the *frame's* member reads go to, and `ra[-2]` is a **second**,
fallback-carrying view of the same object whose lookup continues into the global
object (`:264`, the proxy's `PropGet` chains `Dispatch1` then `Dispatch2`). A
bare name that the local name space does not hold is compiled as a member read
on `this` (`tjsInterCodeGen.cpp:2320-2345`), which is how a top-level script's
variables become global: on the top level `this` *is* the global object.

### 3.2 `this`, `objthis`, and the objthis-context

The three names are three things and the reference uses all three:

| name | what it is | who reads it |
|---|---|---|
| the **object** | the `iTJSDispatch2` a closure names | the member surface |
| the **receiver** (`this`, `ra[-1]`) | the object the *frame* acts on | every member read/write that has no better receiver; `GLOBAL` is its escape (`:1279`) |
| the **objthis-context** (`ra[-2]`) | the receiver with the global object behind it | the frame's own scope, and the fallback the proxy provides |

When a call is made, the callee's receiver is chosen by one rule: **the
closure's own receiver if it has one, otherwise the caller's** —
`clo.ObjThis ? clo.ObjThis : ra[-1]` (`tjsInterCodeExec.cpp:2116`, `:2179`,
`:2228` for the three call forms, `:1673` and `:1795` for the operator forms).
That is what makes `a.f()` give `f` the receiver `a`, and what makes a function
value that was read out of nowhere inherit its reader's receiver.

`VM_CHGTHIS` is the operator that rewrites a closure's receiver in place
(`:1272-1277`, `tjsVariant.h:530`). It has three callers, and each one is a
place where a value has to become a method:

| caller | what it binds | where |
|---|---|---|
| `incontextof` | the right operand's receiver, to the left operand | `tjsInterCodeGen.cpp:1198` |
| a class's super-class expression | the class under construction | `:3610-3612` |
| the context's own fix-up | a **top-level function or property**, to the object it is declared in | `:943-945`, from the non-local declaration list at `:915-918` (`changethis` is true only for `ctFunction` and `ctProperty`) |

The third one is the rule that surprises: a `function f()` written at a script's
top level is not a free function. It is registered as a member of `this` with
its receiver already bound, so calling it later reads `this` as the object it
was declared on.

### 3.3 How a closure captures — it does not

**There is no lexical capture of variables in this language.** A nested function
value carries two things and neither is an environment:

| what it carries | how |
|---|---|
| its **code** | the value's object is a context, which is itself a dispatch object holding the nested functions and classes as members (`tjsInterCodeGen.cpp:886-949`, `RegisterFunction`) |
| its **receiver** | the variant's objthis, rebound by `CHGTHIS` where a method is wanted (§3.2) |

A local variable lives in the frame's register area (`§1`), and that area is
handed back to the array stack when the call returns
(`tjsInterCodeExec.cpp:766-771`). A function value that outlives the call
therefore cannot see it. What a nested function can reach is:

* its own registers,
* `this` — the receiver,
* the members of the object it was read from, through the context object,
* the global object, through the objthis-context proxy.

That is the complete list, and it is why the language's idiom is `var self =
this` rather than a closure over a local. A port that builds a real closure
environment would let a script observe a variable it must not see, and a port
that heap-allocates the frame to make capture work would pay for it on every
call.

### 3.4 How member access is compiled

Six shapes, chosen by the generator, mapping onto six instruction groups:

| source | shape the generator emits | why |
|---|---|---|
| `o.name` | `GPD %d %o @name` | the name is a constant, so it lives in the constant area and the hash can be cached on it |
| `o[expr]` | `GPI %d %o %n` | the name is computed; a numeric key takes the by-number entry |
| `o.name = v` | `SPDE`, or `SPD` when the receiver is the frame's own proxy | created when absent, unless the object is `this`'s proxy — then it must exist (`tjsInterCodeGen.cpp:2051-2063`) |
| `&o.name` | `GPDS`/`SPDS`/`GPIS`/`SPIS` | the unary `&` asks for the value and *not* the property (`:2348-2356`, `:2372-2379`) |
| `*o.name` | `GETP`/`SETP`, or `OPP` for a compound write | the unary `*` forces the property surface (`:2374`, `:2381`, `:2402`) |
| `o.name op= v` | `OPPD` | one operation handed to the object, not a read and a write |

The decision between `SPD` and `SPDE` is made by looking at the receiver's
node: when the left operand of `.` is the frame's `this` proxy the reference
emits `SPD`, because `this`'s members must already exist (`:2051-2063`). It is a
compile-time distinction over a run-time object, and the two opcodes differ only
in the `TJS_MEMBERENSURE` flag (`tjsInterCodeExec.cpp:1176-1181`).

Each of the six then follows the **same three-step shape**: read the receiver's
type, divert if it is a string or an octet, otherwise build the closure and call
the surface with `clo.ObjThis` if the value had one and `ra[-1]` if not
(`tjsInterCodeExec.cpp:1400-1428` for `GPD`). The diversion in step 2 is not a
special case of the object model — it *is* the language's rule that a string and
an octet answer `length` and an index, and the port keeps it in the interpreter
for the same reason.

### 3.5 Calls and the argument list

The argument list is a tail on `CALL`/`CALLD`/`CALLI`/`NEW`, in one of three
shapes (`tjsInterCodeGen.cpp:3683-3706`, `tjsInterCodeExec.cpp:1994-2098`):

| count word | meaning | where the arguments come from |
|---|---|---|
| `n ≥ 0` | `n` arguments | the `n` registers that follow |
| `-1` | `...` with nothing after it — **pass the caller's arguments through untouched** | the caller's own argument array, by pointer, not copied |
| `-2` | at least one argument is expanded | a pair count, then `(flag, register)` pairs: `fatNormal` passes the register, `fatExpand` splices an array's elements, `fatUnnamedExpand` splices the caller's surplus arguments from `FuncDeclUnnamedArgArrayBase` onward |

Two consequences a port must not lose:

* **`...` is not `arguments`.** There is no `arguments` object anywhere in the
  tree; the language exposes the caller's surplus arguments only through a
  *named* collapse parameter, which `ExecuteAsFunction` builds as a plain array
  (`tjsInterCodeExec.cpp:734-750`), and through `fatUnnamedExpand`, which reads
  the caller's live argument slots. A port that materialises an `arguments`
  object would make it observable, and there is nothing to make observable.
* **A forward call shares its argument registers.** Up to twenty arguments are
  passed as a pointer array built on the caller's stack; beyond that the array
  is heap-allocated (`:1989`, `:2076-2098`). The callee receives pointers, so a
  by-reference write lands in the caller's register.

### 3.6 The dispatch the interpreter itself must provide

A context is a dispatch object (`tjsInterCodeGen.h:246`), and its `FuncCall`
branches on the context's kind (`tjsInterCodeExec.cpp:2772-2807`):

| kind | what a call means |
|---|---|
| `ctTopLevel` | run with the global object as the receiver, no arguments |
| `ctFunction`, `ctExprFunction`, `ctPropertyGetter`, `ctPropertySetter` | run with the caller's receiver and arguments |
| `ctClass` | run — this is how `new C()` builds an instance |
| `ctProperty` | refuse: a property declaration is not callable |
| `ctSuperClassGetter` | succeed and do nothing (`:2802-2803`) |

When a member name was given instead, the context's own members answer first and
a name it does not hold falls through to the super-class proxy — which is how an
inherited method is reached (`:2809-2820`).

---

## 4. The exception machinery

### 4.1 What `try` compiles to

Three statements and one expression, compiled by four commands, and the jump
distances are patched at the `catch`
(`reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3502-3599`):

| source | code |
|---|---|
| `try {` | `ENTRY c, %r` — `c` is the distance to the catch, patched later; `%r` is a **frame register the handler writes the exception object into** (`:3511-3517`) |
| `} catch (e) {` | `EXTRY` (leave the protected block), `JMP` past the handler (patched at `}`), and the patch of the `ENTRY` distance to here; then the frame is cleared and, when a name was given, a local variable is created at the register the handler wrote (`:3521-3553`) |
| `}` | patch the `JMP` distance to here, pop the nest (`:3555-3579`) |
| `throw e;` | `THROW %v` (`:3581-3599`) |

A context closes by emitting `SRV %0` and `RET`, preceded by **one `EXTRY` per
still-open `try` nest** (`:3488-3501`). That is a *static* count, emitted at
compile time, and it is why a `return` inside a `try` is not a special case at
run time: the ordinary return path already knows how many protected blocks it is
leaving.

### 4.2 How the unwinding actually works

`ENTRY` does not push a handler record on a stack. It **calls** — the executor
re-enters itself for the protected range and treats a return as the answer
(`tjsInterCodeExec.cpp:1253-1262`, `:1336-1395`):

```text
VM_ENTRY  →  ExecuteCodeInTryBlock(ra, startip, …, catchip, exobjreg)
             ├─ ExecuteCode(…, tryCatch = true)
             │    └─ any failure inside is converted to a script error carrying the ip
             ├─ eTJSScriptException  → the thrown *value* goes to ra[exobjreg]
             ├─ eTJSScriptError      → an Exception object carrying the message and the trace
             ├─ eTJS                  → an Exception object carrying the message
             └─ std::exception       → an Exception object carrying the text
             and in every case the answer is `catchip`, i.e. where to resume
```

Three consequences follow, and each one is load-bearing:

1. **A `try` costs a host call frame.** N nested `try` blocks inside one
   function are N recursive entries into the executor, each with its own C++
   frame and its own `catch`. A port that models the handler as a stack of
   records over one interpreter loop is *faster* and must prove it is
   observationally the same.
2. **The catch's register is in the same frame.** `ENTRY`'s `%r` is the
   current `FrameBase` (`tjsInterCodeGen.cpp:3511-3517`), i.e. an ordinary
   frame register. The `catch` clears the frame down to `FrameBase + 1` and
   stops, keeping it (`:3536-3539`), then binds the caught name to that same
   register (`:3546-3549`), so the exception object is a normal local variable
   and `catch (e)` hands `e` a register in the ordinary way. There is no
   separate handler scope.
3. **An already-script-level error is re-thrown, not re-wrapped.** The inner
   `ExecuteCode` catches `eTJSSilent` and `eTJSScriptError`, adds this context to
   the trace, and re-throws (`:1304-1308`), so the trace accumulates the call
   chain as the exception passes through each protected block. Only the outer
   handler converts it to an object. `eTJSSilent` passes through everything
   untouched and is never reported (`:1304-1305`, `:1359-1360`) — it is the
   cancel signal.

### 4.3 A `throw` across a native call

This is the part where the current tree and a Rust interpreter differ most, and
the difference has to be named before the packet that closes it is planned.

**In the reference**, a script `throw` raises a `eTJSScriptException` carrying a
`tTJSVariant` (`tjsError.h:176-201`), and a native that fails calls
`TJSThrowFrom_tjs_error` or `TJS_eTJSError`, which raises a `eTJS` carrying a
message (`:65-83`) — a **different class**, because it has no thrown value. Both
unwind by C++ stack unwinding, so they cross a native frame for free and are
sorted out at the nearest `catch`: a thrown value becomes `ra[exobjreg]`, a
message becomes a fresh `Exception` object built by `TJSGetExceptionObject`
(`tjsError.cpp:26-54`), which looks the class up in the global object and copies
the `trace` in when there is one.

**In this tree**, a Rust native cannot unwind. It returns a status and, on
failure, a malloc'd message, and the C++ adapter converts that into a plain
`eTJSError`
(`crates/tjs/cpp/tjs2_abi.cpp:751-758`); and the one entry that calls *into* a
script from Rust (`tjs2_call_value`) catches at the boundary and turns the
failure into a status plus a message
(`crates/tjs/cpp/tjs2_abi.cpp:3358-3379`). So today:

| what a native raises | what a script's `catch` receives |
|---|---|
| `throw new SomeException("x")` from script, **not** passing a Rust frame | the object itself, with its members — correct |
| an error raised by a Rust native | a **new** `Exception` carrying the message text — the class the author would have used is lost |
| a script throw on the far side of `tjs2_call_value` | a message text; the thrown **value** and its `trace` are lost, and the raising cannot continue past the Rust frame |

The interpreter packet closes this by making the throw a **value**, not a host
unwind: every frame the interpreter owns is a Rust frame, and the crossing is a
returned `Err(Thrown)` that each interpreter level rethrows, exactly as
`:1304-1308` does. The native boundary stays a status plus a message, but the
**value** it carries becomes a `Variant` handle rather than a UTF-8 string, so a
`catch` on the far side of a native call sees the object that was thrown. That is
a C ABI addition (one out-parameter on the failure path), and it is the only
place in the plan that touches the seam.

### 4.4 What must not change

* An uncatchable `eTJSSilent` is never turned into an object and never logged.
* A thrown non-object value is still catchable and still reproducible as
  `throw` sent it; only an *error* becomes an `Exception` object.
* The `trace` accumulates one entry per protected block the exception passed
  through, so the message a script reads names the call chain in the order it
  unwound.
* A `catch` with **no** name binds nothing, and its frame clear reaches all the
  way down to `FrameBase`, so the object it received is discarded before the
  handler runs (`tjsInterCodeGen.cpp:3536-3539`). The object is created either
  way; only the name decides whether it survives into the handler's body.

---

## 5. What the interpreter needs from the runtime

### 5.1 The surface

The interpreter is the runtime's *user*: it arranges the objects, and the value
and object modules do the work. Everything it needs already exists except where
§5.2 lists a gap.

| what it needs | where it is | state |
|---|---|---|
| the tagged value, its six kinds, its coercions and its operators | `runtime/variant.rs` (`Variant`, `Closure`) | landed |
| the object surface: read, write, delete, enumerate, `isinstanceof`, `isvalid`, `invalidate` | `runtime/object.rs` (`Dispatch`) | landed |
| the default object, with a member map and the property seam | `runtime/object.rs` (`CustomObject`, `Property`, `PropertyCall`) | landed |
| the member map's order, growth and lookup | `runtime/member.rs` (`MemberMap`) | landed |
| an immutable text, its member hash and its comparison | `runtime/string.rs` (`TjsString`) | landed |
| the number conversion | `runtime/number.rs` | landed |
| the byte string | `runtime/octet.rs` | landed |
| the containers — array, dictionary | — | R4, before this one |
| the debugger hook, the profiler hook | — | not planned here |
| the disassembler | — | not planned here (`tjsDisassemble.cpp`, 660 lines, is a diagnostic and stays until R7) |

### 5.2 The gaps, named

Six. Each is a decision the interpreter cannot make for itself.

| # | gap | why it blocks the interpreter | where |
|---|---|---|---|
| **G1** | `Dispatch` has no **`func_call`** and no **`create_new`** | a script function *is* a dispatch object, and `CALL`/`CALLD`/`CALLI`/`NEW` are four of the 128 opcodes; without these two entries a function value cannot be invoked, and the interpreter cannot be its own dispatch | `runtime/object.rs`, the trait (8 methods, all member access) |
| **G2** | `Dispatch` has no **`operation`** entry | `OPPD`/`OPPI`/`OPP` and `INCPD`/`INCPI`/`INCP`/`DECPD`/`DECPI`/`DECP` — 48 of the 128 opcodes — hand a compound assignment to the object; without it they either become a read-modify-write (which changes what a property and a container observe) or cannot be executed | `runtime/object.rs` |
| **G3** | no **by-number** member entry | an integer subscript goes to `PropGetByNum`/`PropSetByNum`, whose default is the decimal text but which a container overrides (`tjsObject.cpp:144-168`); the port needs the entry so a container can answer an index without building the text | `runtime/object.rs` |
| **G4** | no **object identity** for a cache site | a per-site cache has to name *which* object it cached, and a `dyn Dispatch` has no id. One monotonically-assigned `u32` per object, carried on `CustomObject`, gives the interpreter the site key. The reference needs no such id only because its cache is a per-name hash hint that any object may be tried against (`tjsVariantString.h:193`) | `runtime/object.rs` |
| **G5** | `Closure`'s doc says it carries "the object's identity" but the trait is an empty marker | the equality rule compares two objects by pointer (`tjsVariant.cpp:665`), and `INV`/`CHKINV` need `invalidate`/`is_valid` through the closure; the marker has to become the real surface before the interpreter can be written against it | `runtime/variant.rs:35` |
| **G6** | there is **no throw value on the native boundary** | §4.3: a script throw crossing a native call loses its value and its trace. The interpreter can keep its own throws as values, but the crossing needs one out-parameter on the failure path | `crates/tjs/cpp/tjs2_abi.cpp` |

G1, G2 and G3 are additions to one trait and belong in **one** change that lands
before the first interpreter packet, because every later packet is written
against that trait. Each arrives with the base class's own answer for an object
that does not serve it — `CallNotServed` for a value that cannot be invoked, and
the not-served answer for an operator — because that *is* the rule: the engine's
base object serves neither, the executor turns the refusal into the script's own
error, and an entry that raised something of its own would report a different
failure than the engine reports. G4 lands with the inline cache (§7.3). G5 is a
documentation-versus-code mismatch in a landed module and is fixed in the packet
that first needs it. G6 is the only one that touches the seam, and it is the
last packet before the C++ executor is deleted.

---

## 6. The packet plan

Eight packets, in order, each one provable **without the interpreter being
complete**. Every packet lands a module, its reference-cited tests, and the
deletion of the C++ it takes over — or, where the C++ cannot go yet, the
narrowest possible note in `TODO.md` §12.1 naming what still runs it.

The order is forced by two dependencies: the trait of §5.2 G1–G3 gate
everything, and the frame gates every opcode.

| # | packet | files | size | what it proves |
|---|---|---|---|---|
| **P0** | **the dispatch surface** — `func_call`, `create_new`, `operation`, by-number access, and the real `Closure` surface | `runtime/object.rs`, `runtime/variant.rs` (G1–G3, G5) | ~600 lines | a script-shaped value can be invoked, compound-assigned and subscripted through the trait; the object-model oracle (`crates/tjs/examples/object_oracle.rs`) still reproduces its record, and new probes cover the three added entries |
| **P0b** | **the throw value on the native boundary** (G6) | `crates/tjs/cpp/tjs2_abi.cpp`, `crates/tjs/cpp/tjs2_abi.h`, `crates/tjs/src/lib.rs` | ~250 lines | a script `throw` that crosses a native call leaves a `catch` with the object it threw; the probe drives a native that calls back into a script that throws, and asserts the object's members on the far side |
| **P1** | **the frame** — the register window, the argument copy, the collapse array, the discard register, the `this` triple, the block allocator | `runtime/exec/frame.rs` | ~450 lines | a hand-built code stream answers what the frame put there: `[CONST %1 #3][SRV %1][RET]` answers 3; a call with more, fewer and exactly the declared arguments leaves the registers the reference leaves; a collapse parameter answers an array of the surplus |
| **P2** | **the decoder and the flag** — the layout table, the dispatch, the 18 control/comparison opcodes, the 17 single-operand ones, and the 56 arithmetic and property forms | `runtime/exec/decode.rs`, `runtime/exec/op.rs` | ~1 100 lines | every arithmetic and comparison opcode answers what the reference answers, including the `GreaterThan`/`LittlerThan` inversion and the `<=`-as-`CGT`+`SETNF` pair; the flag survives an intervening `TT`; the two unreachable opcodes are decoded and executed |
| **P3** | **member access** — the 15 access opcodes, the string and octet diversions, the property flags | `runtime/exec/member.rs` | ~700 lines | the six source shapes of §3.4 answer what the reference answers on a plain object, a dictionary, a string, an octet and a property; `delete` answers a boolean and a missing member is not an error; `typeof` distinguishes `"void"` from `"undefined"` |
| **P4** | **calls and returns** — the four call opcodes, the three argument-list shapes, `SRV`/`RET`, the context as a dispatch object | `runtime/exec/call.rs` | ~500 lines | recursion terminates with the right answer; `...` passes the caller's arguments through with no copy and no `arguments` object; the collapse parameter and `fatExpand` splice the right elements; a call with no result slot discards nothing |
| **P5** | **the context objects** — `GLOBAL`, `CHGTHIS`, `ADDCI`, `REGMEMBER`, the class and super-class chain | `runtime/exec/context.rs` | ~450 lines | a top-level function is a method of the object it was declared on; `incontextof` rebinds; `new C()` registers the class's functions on the instance; `super` reaches the parent's member |
| **P6** | **the exception machinery** — `ENTRY`/`EXTRY`/`THROW`, the throw value, the `Exception` object, the trace | `runtime/exec/throw.rs` | ~400 lines | a `try` catches a throw from a nested call, an error from a native and a throw from three frames down; the trace names the chain in unwind order; `eTJSSilent`-shaped failures pass through; the catch's register is an ordinary local |

**Total: about 4 450 lines** of implementation plus tests. The C++ this packet
set deletes is `tjsInterCodeExec.cpp` (3 117 lines), `tjsInterCodeContext.cpp`
and `.h` (827), the executing half of `tjs.cpp`, and — at P0b — nothing, because
P0b *adds* to the seam.

### 6.1 The oracle each packet needs

The pattern is the one the object model already uses
(`crates/tjs/examples/object_oracle.rs` → `tests/fixtures/object_members.txt`,
replayed by `crates/tjs/tests/object_members.rs`): a committed record of what the
engine answered, and a check that needs no engine.

| packet | fixture | recorded by | what the record holds |
|---|---|---|---|
| P0, P1, P3, P4, P5 | `tests/fixtures/interpreter_answers.txt` | `crates/tjs/examples/interpreter_oracle.rs` | one `id<TAB>kind<TAB>text` line per probe: the answer, its `typeof`, and the text form |
| P2 | `tests/fixtures/interpreter_ops.txt` | the same example, the `ops` table | per-opcode rows: the two operands, the operator, the result and its kind — plus one row per flag rule |
| P6 | `tests/fixtures/interpreter_throw.txt` | the same example, the `throw` table | the caught value's kind and members, the message, and the trace line count per depth |
| P0b | `tests/fixtures/interpreter_throw.txt`, the crossing rows | the same example | what a `catch` received on the far side of a native call |

The probes are **stable, self-cleaning script fragments**, one per rule, that
build their own object and delete it afterwards — the shape `object_oracle.rs`
established. What each record shares is that it outlives the code it was taken
from: when P6 deletes `tjsInterCodeExec.cpp`, replaying
`interpreter_throw.txt` still proves the behaviour with no C++ in the loop.

### 6.2 The C++ the packets retire

| packet | deletes from `crates/tjs/cpp/tjs2/` |
|---|---|
| P0 | nothing (the object model's C++ is already gone) |
| P1, P2, P3, P4, P5 | nothing on its own — the C++ executor is one file, and a part of it cannot be deleted while the rest runs |
| P6 | `tjsInterCodeExec.cpp`, `tjsInterCodeContext.cpp`, `tjsInterCodeContext.h`, and the executing half of `tjs.cpp` |
| P7 (R7, outside this plan) | the C ABI and the C++ tree |

That is the honest shape of the deletion: **the executor is one file, so six
packets of work retire it in one commit.** What each earlier packet retires is a
*proof*: it lands the fixture that the deletion is checked against. The packets
are ordered so that by P6 the record set covers every opcode, every access shape
and every throw path, and the deletion costs the proofs nothing.

---

## 7. The performance plan

### 7.1 Where the time goes

Three costs, each named by a citation and each measured before the port starts.

**Type dispatch.** Every operation is a switch over a six-way tag on a 20-byte
value, and every read of it goes through a converting accessor — `AsInteger`,
`AsReal`, `AsString`, `AsObjectClosureNoAddRef` — which for an object is a
refcount bump and for a string a `Release` on the way out. The measured share of
the VM's time in reference counting is **~15 %**, and the allocator under the
VM's churn is **16–36 %** depending on the workload
(`docs/perf.md` §2.3, §3.5). This is the reason the port's `Variant` is a
plain `enum` with `Rc` only on the object arm: no refcount on the integer path,
no allocation to read one.

**Member lookup.** A lookup is a name hash over the text, one array index, then
a chain walk (`tjsObject.cpp:984-1030`). The measured share of the member
tables — the hash, the lookup and the insert — is **~10 %** (`docs/perf.md`
§3.5). Two costs inside it are worth separating, because they have different
fixes:

| cost | paid | fixable by |
|---|---|---|
| hashing the **name** | once per read, because the reference computes the hash unless the string carries a hint (`tjsVariantString.h:63`, `:193`; `tjsObject.cpp:984-1000`) | caching the hash on the constant (§7.2) |
| walking the **chain** | once per read, ending with a name comparison | caching the slot (§7.3) |

The reference's own cache is the first of these and only the first: `GetHint()`
returns a `tjs_uint32*` the object writes the name's hash into, so a second read
of the same name skips the hash loop (`tjsObject.cpp:991-1000`). It never
caches the slot, so the chain walk is paid every time. That is the gap the port
can take.

**Frame allocation.** Every call allocates a block of `MaxVariableCount +
VariableReserveCount + 1 + MaxFrameCount` variants and copies the declared
arguments into it one at a time (`tjsInterCodeExec.cpp:653-657`, `:711-731`).
The block is reused through a
page stack (`:578-610`), so the *allocation* is cheap, but the **arguments are
copied** and the register area is not zeroed — the `CCL` that would have done it
is dead code (§2.3), so a lane that a call never writes keeps the previous
call's value. That is a rule, not a saving: `CL` materialises `void` where the
language requires it, and the port must reproduce the reuse.

### 7.2 The name hash moves to the constant

A `GPD`/`SPD`/`TYPEOFD` names its member as a slot of the constant area, and the
constant area is **immutable after `Commit`**. So the hash of every name the
code names is computed once, into a parallel `Vec<u32>` the interpreter indexes
with the same operand:

```text
VM_GPD %d %o @k   →   let hash = hashes[k];  let name = &data[k];
                      map.lookup(hash, name)
```

`TjsString::member_hash()` recomputes the mix loop on every call today
(`crates/tjs/src/vm/runtime/string.rs:110-123`), and `MemberMap::get` calls it on
every lookup (`runtime/member.rs:170-174`). Hoisting it out of the loop is what the
reference's hint buys, without the mutable-hint hazard: the constant is
immutable, so there is nothing to invalidate and nothing a script can observe.

### 7.3 The inline cache for member access

A cache entry is **three words at a site**, and a site is the instruction's own
constant slot — `DataArea` never moves and never changes, so the site is static
and the entry is a parallel array:

```text
struct Slot { object: u32, generation: u32, slot: u32 }   // the site's cache
                                     // `slot` = the bucket or arena slot last used
```

| what is cached | why it is that |
|---|---|
| the **object id** (G4) | the site is shared by every object the instruction ever sees, so the entry has to say whose slot it holds |
| the map's **generation** | one `Cell<u32>` per `MemberMap`, bumped by `insert` of an absent name, by `remove`, by `grow` and by `clear` — and **not** by a read |
| the **slot** | the bucket index for a primary member, the arena index for a chain member — the two encodings `MemberMap::Found` already has |

A hit is:

```text
entry.object == object.id  &&  entry.generation == map.generation
  → read the member at entry.slot
  → accept only when its hash == hash and its name == name
  → else fall through to the full lookup and refill the entry
```

**What invalidates it, and why it stays correct.**

| event | the generation | the entry after the event |
|---|---|---|
| a write to a **present** name | unchanged — the member keeps its slot (`runtime/member.rs:147-150`) | still valid; the value at the slot changed, which is the right answer |
| an insert of an **absent** name | bumped — a new member may take the free primary, and may trigger `grow` | a miss; the next read refills |
| a **delete** | bumped — the arena slot joins `free` and is reused by the next insert (`:192-225`) | a miss; a reused slot would otherwise answer a *different* member |
| a **growth** | bumped — every member is re-filed into a different bucket (`grow`) | a miss; slots do not survive a rehash |
| a **read** | unchanged — the lookup relinks a chain and moves no member between slots (`find`) | still valid |
| a **clear** | bumped | a miss |

The invalidation is therefore **one `u32` compare**, not a protocol: nothing has
to walk a list of caches and nothing has to be un-registered. And the design is
correct even if the generation were wrong, because the entry is a **hint, never
a fact**: the slot's member is read and its hash and name re-compared before the
value is accepted, so a stale entry can only cost a wasted read, never a wrong
answer. Two independent guards, and the cheap one is the one that fires.

Two deliberate limits:

* **The indirect forms get no per-site cache.** `GPI`/`SPI` take the name from a
  register, so there is no static site. They keep the hash-only path (§7.2 does
  not apply either) — measuring first is the point, and the reference does not
  cache them at all.
* **A dictionary written in a loop during a read loop misses every time.** That
  is the honest cost of the generation: a map that is mutated between reads
  invalidates its sites. A per-bucket epoch would keep the untouched buckets
  cached, and it is the second step — not the first, because it needs a
  derivation of the bucket index that the cache can verify, and the first step
  should be measurable on its own.

### 7.4 What the port may not do for speed

* It may not cache a member's **value**. A read of a property runs the getter
  (`runtime/object.rs`, `Property::read`), and the reference runs it on every
  read: `docs/perf.md` §3.7 is the measurement of a scene that spent its time in
  a native property's getter, invoked once per read with nothing caching the
  walk underneath it. A cache over a property would change what a script
  observes — the getter is the read.
* It may not reorder the members. The enumeration order is what a dictionary's
  saved file holds (`runtime/member.rs`, the module doc), and `find`'s relink
  is part of it.
* It may not skip the string/octet diversion. `"abc".length` is a rule of the
  language, not an optimisation (`tjsInterCodeExec.cpp:38-88`, `:91-120`).

---

## 8. The A/B measurement

The bar is the same one the value layer had to clear (`docs/perf.md` §6.5): the
port must be **faster**, on the same work, at the same optimisation level, with
the numbers recorded. Interpreter behaviour is coarse-grained enough that a
single script exercises everything, so the measurement is one run of one script
on two engines, with **two checksums that must agree**.

### 8.1 The fixture

One script, in the tree, run against both engines:
`crates/tjs/tests/fixtures/interpreter_probe.tjs` — **not** in this repository's
corpus, and not a game's script. It is written for this measurement and holds one
named function per rule, called once each in a fixed order:

| group | what it covers |
|---|---|
| `arith` | every arithmetic and bitwise operator, on integer / real / string operands, including the division-by-zero and the coercion paths |
| `cmp` | every comparison, on both sides of the string/number boundary, plus `typeof` of each result |
| `member` | the six access shapes of §3.4 on a plain object, a dictionary, a string, an octet and a property |
| `call` | recursion, a method call, `new`, `...` pass-through, the collapse parameter, and `fatExpand` |
| `scope` | a top-level function's receiver, `incontextof`, `super`, a class's member registration |
| `throw` | a nested `try`, a caught error, a caught value, a rethrow, and a throw from three frames down |
| `loop` | one hot loop over a dictionary, one over an array, one over local integers — the shapes §7.3's cache is for |

The script's output is **canonical and total**: one line per observation,
`id<TAB>kind<TAB>text`, no ordering that depends on a pointer, no timing.

### 8.2 The two checksums

| checksum | what it is | what a disagreement means |
|---|---|---|
| the **answers** | the transcript above, hashed line by line, with the first differing line reported | a behavioural divergence, and the line names the rule |
| the **trace** | a digest of the executed instruction stream: per instruction, the opcode, the *relative* ip step, and the register operands' kinds — never a pointer and never a value | a divergence in *control flow* that the answers do not show: a missed jump, a reordered argument list, a `try` that unwound one block too many |

The trace checksum is the sharper one and it is only possible because the two
engines run the **same code stream**: the Rust generator produces the block, and
both engines execute that same image. So the ip sequence is a well-defined
number and the two must agree instruction for instruction.

Both engines are driven over the same image through the same entry, so the only
difference measured is the executor:

```sh
# answers + trace, vendored executor, into a record
cargo run -p tjs --release --example interpreter_ab -- --engine vendored \
    > /tmp/ab_vendored.txt
# answers + trace, Rust interpreter
cargo run -p tjs --release --example interpreter_ab -- --engine rust \
    > /tmp/ab_rust.txt
cmp /tmp/ab_vendored.txt /tmp/ab_rust.txt
```

### 8.3 The acceptance bar

Five conditions, all of which must hold. **The checksums agreeing is the
correctness gate; the time is the shipping gate; neither substitutes for the
other.**

1. **The answers and the trace are identical** on the probe script. Not
   "equivalent": byte-identical, including the first differing line when they
   are not.
2. **The corpus agrees.** The game's own scripts run headless for a fixed frame
   count on both engines, and the frame body's observable output is hashed: the
   log lines, the values the script reads back, and the render command stream the
   compatibility surface produces. The corpus is the set
   `crates/tjs/tools/extract_corpus.py` writes, and the check is the corpus
   digest's shape (`docs/code/tjs/corpus.md`). A game title's startup is the
   real bar, because that is where the opcode mix stops being the probe's.
3. **The interpreter is faster** on the probe script and on the startup:
   whole-run wall clock, and per-opcode nanoseconds from the interpreter's own
   counter, both reported. The method and the fairness caveats are
   `docs/perf.md` §6.5 — same build level on both sides, warm-up discarded,
   best of three, and the load average reported with the numbers. A ratio within
   a few percent of 1.0 is a wash, not a win.
4. **The C++ executor is deleted** in the same change, and the records of §6.1
   still replay with no C++ in the loop.

A further condition is the one that costs the most if it is skipped: **the
memory and the frame budget do not regress**. The vendored executor enters a
host frame
per `try` (§4.2); a port that keeps that shape has kept the bug. The port keeps
one interpreter frame per *script* frame, and the packet records the measured
depth at which a script's own recursion stops, because a script that recursed
until the C++ frame limit must still recurse to the same depth or the language
has changed.

---

## 9. What this page does not decide

* **The debugger.** `VM_DEBUGGER` is one opcode and the debugger behind it
  (`tjsDebug.cpp`) is not in this plan; the opcode is executed as a no-op until
  the debugger's own column.
* **The disassembler.** 660 lines of diagnostic that read the same code stream;
  it stays with the C++ tree until R7 and is not on any packet's path.
* **The `.tjsb` round trip.** `TODO.md` §12 records that the writer's address
  translation registers only the base opcode of a family, so a `+=`-family or
  `++` statement does not survive a save and reload. That defect is the
  writer's, not the interpreter's, and it is inherited — the port must decide
  whether to reproduce it or to record it as an approximation with its
  reference gap.
* **A stack of handler records instead of one host frame per `try`.** §4.2
  argues it is the faster shape; whether it is observationally identical is the
  one design question the exception packet has to answer with a measurement, not
  an argument.

---

See also: [`runtime.md`](runtime.md) — the runtime this module executes,
[`generator.md`](generator.md) — the front end that produces the stream,
[`columns.md`](columns.md) — the columns and the seam,
[`corpus.md`](corpus.md) — the corpus digest the acceptance bar reuses, and
[`../../perf.md`](../../perf.md) — the measurements.
