# The C++ adapter for the Rust script block — step 2 of the VM replacement

**Status:** the specification for step 2. None of it is implemented yet.
**Scope:** the contract the C++ side must meet once the Rust generator owns the
front end, and the exact C ABI the Rust side must expose for it.

**Sources of truth.** The reference checkout is the oracle:
`reference/krkr2/cpp/core/tjs2/`. Every statement below carries a citation of
the form `reference/krkr2/cpp/core/tjs2/<file>:<line>`. The vendored tree
(`crates/tjs/cpp/tjs2/`) is the code that has to change; where it differs from
the reference, the difference is called out in §9 and the reference wins.

**The decided order** (not re-litigated here):

1. the lexer, the parser and the generator are wired to each other in Rust
   first, over native Rust calls, with no C ABI between the columns;
2. then **one** C ABI is exposed: the C++ calls a single entry that compiles
   script text and hands back the script block the C++ executor consumes. The
   C++ never calls the lexer or the parser separately; the C++ grammar is
   replaced wholesale.

This document specifies step 2.

**Vocabulary.** "the image" is the Rust side's description of a compiled
script block: the code stream, the constant area, the variable area, the
function-argument area, the labels, the per-instruction source positions and
the line table, for every context of the block. "the adapter" is the C++ code
that turns the image into the reference's own `tTJSScriptBlock` /
`tTJSInterCodeContext` objects.

---

## 1. The entry

### 1.1 What the C++ does today

The compile entry is a chain of four calls:

| step | where | what it does |
|---|---|---|
| 1 | `reference/krkr2/cpp/core/tjs2/tjs.cpp:455` `tTJS::ExecScript` → `reference/krkr2/cpp/core/tjs2/tjsScriptCache.cpp:37` `tTJSScriptCache::ExecScript` | makes a `tTJSScriptBlock`, names it (`SetName`, `reference/…/tjsScriptBlock.cpp:95`), and calls `SetText` |
| 2 | `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:198` `tTJSScriptBlock::SetText` | copies the script text into the block, **builds the line table** (`reference/…/tjsScriptBlock.cpp:207-232`), calls `Parse`, then runs the top level (`reference/…/tjsScriptBlock.cpp:308`) |
| 3 | `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:421` `tTJSScriptBlock::Parse` | makes the lexical analyser (`reference/…/tjsScriptBlock.cpp:430-431`), runs the bison grammar (`reference/…/tjsScriptBlock.cpp:434`, `parser{ this }.parse()`), and raises `TJS_eTJSScriptError(FirstError, this, FirstErrorPos)` when the grammar recorded an error (`reference/…/tjsScriptBlock.cpp:444-446`) |
| 4 | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:40` `yylex` → `reference/krkr2/cpp/core/tjs2/tjsLex.cpp` `tTJSLexicalAnalyzer::GetNext` | the token stream; the grammar drives the generator through `PushContextStack` / `PopContextStack` (`reference/krkr2/cpp/core/tjs2/bison/tjs.y:202`, `:352`, `:415`, `:430`, `:441`, `:457`) |

The grammar's exit is `tTJSScriptBlock::PopContextStack`
(`reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:410`), which calls
`InterCodeContext->Commit()` and then releases the context. `Commit()` is
where the generator does its post-processing (§5).

The bytecode-export path is a **second** entry onto the same grammar:
`tTJS::CompileScript` (`reference/krkr2/cpp/core/tjs2/tjs.cpp:580`) →
`tTJSScriptBlock::Compile` (`reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:594`)
→ the same `Parse` (`reference/…/tjsScriptBlock.cpp:627`) →
`ExportByteCode` (`reference/…/tjsScriptBlock.cpp:537`).

There is **no** `tjsCompile.cpp` and no `tjsInterCodeContext.h` in the
reference; `tjsScriptBlock.{h,cpp}` *is* the compile entry's home.

### 1.2 The smallest change

**Change `tTJSScriptBlock::Parse` and nothing else at the entry level.**

`Parse` is the single place where the grammar is entered, and it is entered
from both the live path (`SetText`) and the bytecode path (`Compile`). Its
body becomes:

```cpp
void tTJSScriptBlock::Parse(const tjs_char *script, bool isexpr,
                            bool resultneeded) {
    if(!script)
        return;
    CompileErrorCount = 0;
    // NEW: one call into the Rust front end, then the adapter.
    BuildFromRustImage(script, isexpr, resultneeded);
}
```

Everything else — `SetText`'s line table and top-level execution, `Compile`'s
bytecode export, `tTJSScriptCache`, `tTJS` — is untouched, because the
adapter produces **the reference's own objects**: a `tTJSScriptBlock` whose
`InterCodeContextList` holds real `tTJSInterCodeContext`s, each with a real
`CodeArea`, a real `DataArea` and a real `SourcePosArray`. The executor
(`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp`) and the bytecode writer
(`reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:4006`) then work
unchanged.

This is not a new idea in the reference: the reference already has a
"build a block from an external description" path — the bytecode loader
(`reference/krkr2/cpp/core/tjs2/tjsByteCodeLoader.cpp:321-378`). The adapter
is that loader, fed from the Rust image instead of from a file. §4.4 spells the
procedure out, and the reference's own loader is the model for it.

`SetText` keeps building the line table from the text it already owns
(`reference/…/tjsScriptBlock.cpp:207-232`); the image's line table is
*cross-checked* against it, not substituted for it, because the block's own
`Script` buffer is what `GetLine` returns to the disassembler
(`reference/…/tjsScriptBlock.cpp:140-152`).

### 1.3 The exact C ABI the Rust entry must match

The Rust entry is **one** call. It compiles text and returns the image. It
does not take a callback, does not call back into C++, and never unwinds into
the C++ caller (a panic is caught and reported as a status).

```c
/* ---- crates/tjs/cpp/tjs2_codegen.h (the shipped seam) ---------------- */

/* The status of a compile. Zero is success. */
#define TJS2_COMPILE_OK            0
#define TJS2_COMPILE_ERROR_SYNTAX -1   /* the script does not parse        */
#define TJS2_COMPILE_ERROR_PANIC  -2   /* the front end panicked          */
#define TJS2_COMPILE_ERROR_RANGE  -3   /* a value is out of range         */
#define TJS2_COMPILE_ERROR_STATE  -4   /* the handle is in a wrong state  */

/* An opaque compiled script block: the handle owns the image. */
typedef struct tjs2_script tjs2_script;

/* Compiles script text into the image of a script block.
 *
 * script        the text, as UTF-16 code units (tjs_char is UTF-16 in the
 *               reference; reference/krkr2/cpp/core/tjs2/tjsTypes.h:44).
 * length        the number of code units.
 * expression    non-zero when the text is an expression, not a program.
 *               The reference appends a ';' to the text in this mode
 *               (reference/krkr2/cpp/core/tjs2/tjsLex.cpp:1006-1008).
 * result_needed non-zero when the expression's value is wanted; it is read
 *               only in the expression mode
 *               (reference/krkr2/cpp/core/tjs2/tjsLex.cpp:1676).
 *
 * Returns a handle the caller owns (free it with tjs2_script_free). The
 * handle is returned even when the script does not compile: the failure is
 * then read with tjs2_script_status / tjs2_script_error_*. */
tjs2_script *tjs2_script_compile(const uint16_t *script, uint32_t length,
                                  int32_t expression, int32_t result_needed);

/* Frees the handle and every buffer of the image it owns. */
void tjs2_script_free(tjs2_script *script);

/* The status of the compile: TJS2_COMPILE_OK or a negative code. */
int32_t tjs2_script_status(const tjs2_script *script);

/* The message of the first failure; owned by the handle. NULL when there
   was none. The reference's message for a syntax error is
   "Syntax error (%1)" (reference/krkr2/cpp/core/tjs2/tjsError.h:288). */
const char *tjs2_script_error_message(const tjs2_script *script);

/* The source position of the first failure, in code units from the start
   of the script. */
int32_t tjs2_script_error_position(const tjs2_script *script);

/* The number of failures the front end counted. The reference counts every
   recovered syntax error and raises on the first one
   (reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:52-87,
   reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:444-446). */
int32_t tjs2_script_error_count(const tjs2_script *script);

/* Non-zero when the script used a conditional-compilation statement
   ("@if" / "@set"). The reference sets the same flag on the block
   (reference/krkr2/cpp/core/tjs2/tjsLex.cpp:1132, :1174) and the flag
   makes the block non-reusable
   (reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:79-82). */
int32_t tjs2_script_using_preprocessor(const tjs2_script *script);

/* The image of the script block. NULL when the compile failed. */
const tjs2_codegen_block *tjs2_script_block_image(const tjs2_script *script);
```

**Why the text is UTF-16.** The reference's `tjs_char` is UTF-16
(`reference/krkr2/cpp/core/tjs2/tjsTypes.h:44`) and every source position,
every line-table offset and every string constant is counted in code units.
Handing the Rust side UTF-8 would force a conversion whose result the
positions no longer index.

**Why the handle is returned even on failure.** The reference's error path
needs three things *after* the front end has failed: the message, the source
position and the error count (`reference/…/tjsInterCodeGen.cpp:52-87`), and
the block's `FirstError` / `FirstErrorPos` / `CompileErrorCount` must be set
before `TJS_eTJSScriptError` is raised (`reference/…/tjsScriptBlock.cpp:444-446`).
A status-only return would lose the position.

**Why there is no `tjs2_script` per-context accessor.** The image is one
block; the C++ adapter walks it in one pass. A per-context accessor would be
the "proof instrument" shape, not the shipped shape.

### 1.4 The C++ side of the seam

```cpp
// crates/tjs/cpp/tjs2/tjsScriptBlock.cpp  (new private method)
void tTJSScriptBlock::BuildFromRustImage(const tjs_char *script, bool isexpr,
                                          bool resultneeded);
```

Its steps are §4.4. It lives in `tjsScriptBlock.cpp` because the block owns
the contexts (`reference/…/tjsScriptBlock.h:42`, `Add`/`Remove` at
`reference/…/tjsScriptBlock.h:68-69`) and the reference's own "build from an
image" code lives in the block's neighbourhood
(`reference/…/tjsByteCodeLoader.cpp`).

### 1.5 The rules the entry itself implements

The entry is `crates/tjs/src/vm/mod.rs`.

| rule | reference |
|---|---|
| the handle is returned even when the script does not compile, because the error path needs the message, the position and the count after the front end has failed | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:52-87`, `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:444-446` |
| the conditional-compilation values go in and come back out, because a value one script sets is visible in the next one | `reference/krkr2/cpp/core/tjs2/tjsCompileControl.cpp:205` |
| the text is UTF-16 code units, and every source position and every string constant is counted in code units | `reference/krkr2/cpp/core/tjs2/tjsTypes.h:44` |

---

## 2. The image, field by field

The full table is [`FIELD_TABLE.md`](FIELD_TABLE.md). This section states the
rules the table's rows obey.

### 2.1 The block

The image carries the block's **line table** and the **top-level context's
index**; everything else about the block is the C++'s own (the owner, the
name, the script text, the reference count).

| what | where in the image | where it lands in C++ |
|---|---|---|
| the line table | `tjs2_codegen_block.lines` / `.line_count` | `tTJSScriptBlock::LineVector` and `LineLengthVector` (`reference/…/tjsScriptBlock.h:50-51`) |
| the top-level context | `tjs2_codegen_block.top_level` | `tTJSScriptBlock::TopLevelContext` (`reference/…/tjsScriptBlock.h:53`) |
| the contexts | `tjs2_codegen_block.contexts` / `.context_count` | `tTJSScriptBlock::InterCodeContextList` (`reference/…/tjsScriptBlock.h:42`), in the image's order |
| the compile failure | the handle, not the image | `FirstError`, `FirstErrorPos`, `CompileErrorCount` (`reference/…/tjsScriptBlock.h:55-56`, `:61`) |
| the preprocessor flag | the handle, not the image | `UsingPreProcessor` (`reference/…/tjsScriptBlock.h:58`), via `NotifyUsingPreProcessor()` (`reference/…/tjsScriptBlock.h:107`) |

**The line table's rule** (the reference's own, `reference/…/tjsScriptBlock.cpp:207-232`):
the text is split on `\r` and `\n`; a `\r\n` counts as one break; each line
contributes its **start offset** and its **length in code units, excluding the
break**; a final line without a trailing break is added if the text does not
end with one. The reference's `GetLine` reads `LineLengthVector[line]` and
`Script + LineVector[line]` (`reference/…/tjsScriptBlock.cpp:140-152`), and
`SrcPosToLine` binary-searches `LineVector` (`reference/…/tjsScriptBlock.cpp:154-167`).

**The line table is not optional.** `SrcPosToLine` is called from the error
path (`reference/…/tjsInterCodeGen.cpp:83`), from `GetLineDescriptionString`
(`reference/…/tjsScriptBlock.cpp:176-192`), from the disassembler
(`reference/…/tjsDisassemble.cpp:57`) and from `eTJSScriptError::GetSourceLine`
(`reference/…/tjsError.cpp:84-86`). A block without a line table cannot report
a line number.

**The contexts' order is the creation order.** The reference's
`InterCodeContextList` is appended in the context constructor
(`reference/…/tjsInterCodeGen.cpp:257`, `block->Add(this)`), and the order is
observable through `GetCodeIndex` (`reference/…/tjsScriptBlock.cpp:509-519`),
`Dump` (`reference/…/tjsScriptBlock.cpp:466-486`) and `ExportByteCode`
(`reference/…/tjsScriptBlock.cpp:537-576`). The image's context order must be
the creation order.

### 2.2 The context

The image's per-context record carries, in the reference's own terms:

| the reference's field | the image's field | notes |
|---|---|---|
| `CodeArea`, `CodeAreaSize` (`reference/…/tjsInterCodeGen.h:394-396`) | `code`, `code_size` | the final instruction stream; see §3.1 |
| `DataArea`, `DataAreaSize` (`reference/…/tjsInterCodeGen.h:401-402`) | `constants`, `constant_count` | the constant area; see §3.2 |
| `SourcePosArray`, `SourcePosArraySize` (`reference/…/tjsInterCodeGen.h:427-429`) | `labels`, `label_count` | the per-instruction source positions; see §6 |
| `VariableReserveCount` (`reference/…/tjsInterCodeGen.h:433`) | `variable_reserve_count` | 2 for every context type but `ctProperty` (`reference/…/tjsInterCodeGen.cpp:227-254`) |
| `MaxVariableCount` (`reference/…/tjsInterCodeGen.h:438`) | `variable_count` | the number of local-variable slots; see §3.4 |
| `MaxFrameCount` (`reference/…/tjsInterCodeGen.h:437`) | `max_frame_count` | the number of frame registers |
| `FuncDeclArgCount` (`reference/…/tjsInterCodeGen.h:440`) | `func_arg_count` | the number of declared arguments |
| `FuncDeclUnnamedArgArrayBase` (`reference/…/tjsInterCodeGen.h:441`) | `func_unnamed_arg_base` | the base of the unnamed `*` argument |
| `FuncDeclCollapseBase` (`reference/…/tjsInterCodeGen.h:442`) | `func_collapse_base` | the base of the named `*` argument; −1 when there is none |
| `SuperClassGetterPointer` (`reference/…/tjsInterCodeGen.h:444`) | `super_class_getter_pointer`, `super_class_getter_pointer_count` | the code addresses at which the super class is evaluated |
| `Name` (`reference/…/tjsInterCodeGen.h:446`) | `name`, `name_length` | the context's name |
| `ContextType` (`reference/…/tjsInterCodeGen.h:449`) | `kind` | one of the eight `tTJSContextType` values (`reference/…/tjsInterCodeGen.h:166-175`) |
| `Parent` (`reference/…/tjsInterCodeGen.h:447`) | `parent` | the parent context's index, −1 for none |
| `PropSetter` (`reference/…/tjsInterCodeGen.h:450`) | `prop_setter` | the setter context's index, −1 for none |
| `PropGetter` (`reference/…/tjsInterCodeGen.h:451`) | `prop_getter` | the getter context's index, −1 for none |
| `SuperClassGetter` (`reference/…/tjsInterCodeGen.h:452`) | `super_class_getter` | the proxy context's index, −1 for none |
| `Properties` (`reference/…/tjsInterCodeGen.h:390`) | `properties`, `property_count` | the members this context contributes to its parent |

**What does *not* cross.** The compile-time fields — `FrameBase`
(`reference/…/tjsInterCodeGen.h:392`), `Namespace` (`:404`), the node vectors
(`:406-408`), the argument and array stacks (`:410-412`), the nest vector
(`:415`), the jump and fix lists (`:417-418`), the non-local function
declarations (`:420`), `FunctionRegisterCodePoint` (`:422`),
`PrevSourcePos` / `SourcePosArraySorted` (`:424-425`), `SuperClassExpr`
(`:431`), `AsGlobalContextMode` (`:435`) and the build-time half of the
constant area (`_DataArea`, `_DataAreaSize`, `_DataAreaCapa`,
`reference/…/tjsInterCodeGen.h:398-400`) — are the Rust generator's private
state. The C++ side has no use for them: the reference clears the namespace in
`Finalize` (`reference/…/tjsInterCodeGen.cpp:399`) and never reads the
compile-time fields at run time.

### 2.3 The constant area

A constant is one of:

| kind | the reference's value | how the C++ builds it |
|---|---|---|
| void | `tTJSVariant()` — `vt = tvtVoid` (`reference/…/tjsVariant.h:546`) | `tTJSVariant()` |
| integer | `tTJSVariant(tjs_int64)` — `vt = tvtInteger` (`reference/…/tjsVariant.h:627-632`); `true`/`false` are integers 1/0 (`reference/…/tjsVariant.h:617-620`) | `tTJSVariant((tjs_int64)value)` |
| real | `tTJSVariant(tjs_real)` — `vt = tvtReal` | `tTJSVariant(value)` |
| string | `tTJSVariant(tTJSString)` — `vt = tvtString` | `tTJSVariant(ttstr(text))` |
| octet | `tTJSVariant(tTJSVariantOctet)` — `vt = tvtOctet` | `tTJSVariant(octet)` |
| null | `tTJSVariant((iTJSDispatch2 *)nullptr)` — `vt = tvtObject` with a null object (`reference/…/tjsLex.cpp:1553`) | `tTJSVariant((iTJSDispatch2 *)nullptr)` |
| **context** | `tTJSVariant(this)` — the context object itself (`reference/…/tjsInterCodeGen.cpp:911`, `:926`) | `tTJSVariant(context_object)` — the C++ resolves the index to the `tTJSInterCodeContext *` |

The **context** kind is the one that cannot be carried as a value: the Rust
side has no C++ pointer. The reference's own format solves the same problem
the same way — the bytecode writer tags such a constant `TYPE_INTER_OBJECT`
and writes the block's context index
(`reference/…/tjsConstArrayData.cpp:139-148`, `:187-190`), and the loader
resolves it back to the object afterwards
(`reference/…/tjsByteCodeLoader.cpp:367-372`). The image does the same: a
constant of kind `TJS2_CG_VALUE_CONTEXT` carries the **context index within
the block** in its `object` field.

No other object can appear in a constant: the only object-valued constants
the reference creates are the null literal and the context objects.

### 2.4 The variable area

The reference's "variable area" is the **local name space**
(`tTJSLocalNamespace`, `reference/…/tjsInterCodeGen.h:404`), not an array.
Its only surviving trace at run time is `MaxVariableCount`
(`reference/…/tjsInterCodeGen.cpp:994`), which sizes the register frame
(`reference/…/tjsInterCodeExec.cpp:641`, `:648`).

So the C++ side needs **the number, not the names**: `variable_count` in the
image is `MaxVariableCount`, and the per-variable records (`name`,
`register_`) exist in the image for the Rust side's own bookkeeping and for
the debugger, and the C++ adapter may ignore them.

**The register numbering** (so the C++ side can check the image):
`ra[-1]` is `objthis`, `ra[-2]` is the global/objthis proxy, `ra[-3]` and
below are the declared arguments and then the local variables in name-space
order, `ra[0]` is the result register and `ra[1]` and above are the frame
(`reference/…/tjsInterCodeExec.cpp:648`, `:700-728`;
`reference/…/tjsInterCodeGen.cpp:2845-2884`; the name space's index → register
rule is `-(index) - VariableReserveCount - 1`,
`reference/…/tjsInterCodeGen.cpp:2864-2865`).

### 2.5 The function-argument area

The reference has **no array** of declared arguments. What it has is three
numbers per context:

- `FuncDeclArgCount` — how many arguments the context declares
  (`reference/…/tjsInterCodeGen.cpp:3736`, `:3761`);
- `FuncDeclUnnamedArgArrayBase` — the base of the unnamed `*` argument
  (`reference/…/tjsInterCodeGen.cpp:3748`);
- `FuncDeclCollapseBase` — the base of the named `*` argument, −1 when there
  is none (`reference/…/tjsInterCodeGen.cpp:3751`).

They are read by the executor when the context is *entered* as a callee
(`reference/…/tjsInterCodeExec.cpp:704-741`) and when it *calls*
(`reference/…/tjsInterCodeExec.cpp:2023`, `:2062`). The argument **names**
live in the name space (`reference/…/tjsInterCodeGen.cpp:3714`,
`Namespace.Add(varname)`), not in a separate array.

The image's `func_args` list (kind + register + name) is therefore
**redundant for the C++ side**: the C++ adapter needs the three numbers and
nothing else. The list may stay in the image for the debugger and for the
Rust side's own checks.

The *call-site* argument description — how many arguments, which registers,
whether an argument expands (`fatNormal` / `fatExpand` /
`fatUnnamedExpand`, `reference/…/tjsInterCodeGen.h:164`) — is **in the code
stream**, not in an area: `GenerateFuncCallArgCode`
(`reference/…/tjsInterCodeGen.cpp:3683-3706`) writes it as ordinary
instructions, and the executor reads it back out of the code
(`reference/…/tjsInterCodeExec.cpp:1991-2084`).

### 2.6 The labels

The reference calls its per-instruction source positions "labels" in the
image's vocabulary, but they are `tSourcePos` records
(`reference/…/tjsInterCodeGen.h:274-279`): a **code position** and a
**source position**. They are the block's "labels" in the sense that they
name the places in the script the code came from. §6 treats them in full.

### 2.7 The super-class getter pointer

`SuperClassGetterPointer` (`reference/…/tjsInterCodeGen.h:444`) is a
`std::vector<tjs_int>` of **code addresses inside the super-class-getter
context's own code area**, one per `extends` clause
(`reference/…/tjsInterCodeGen.cpp:3638`). The executor runs the proxy from
each of them, in reverse order, to find the super class
(`reference/…/tjsInterCodeExec.cpp:2755-2762`).

It is a per-context field of the **super-class-getter** context, and the
image must carry it. The reference's bytecode format carries it too
(`reference/…/tjsInterCodeGen.cpp:4088-4093`,
`reference/…/tjsByteCodeLoader.cpp:302-306`).

### 2.8 The member registrations

`RegisterFunction` registers a child context as a **member of its parent
object** when the parent is a function or a class
(`reference/…/tjsInterCodeGen.cpp:933-945`):

```cpp
tTJSVariant val = this;
Parent->PropSet(TJS_MEMBERENSURE | TJS_IGNOREPROP, Name, nullptr, &val, Parent);
```

This is a compile-time mutation of the parent's *object*, not code
generation, so it cannot be done by the Rust side — the Rust side has no
object. The reference's own bytecode format solves this by recording the
registration in the child's `Properties` list
(`reference/…/tjsInterCodeGen.cpp:935-939`, `tProperty` at
`reference/…/tjsInterCodeGen.h:380-388`) and replaying it in the loader
(`reference/…/tjsByteCodeLoader.cpp:351-365`).

The image does the same: per context, a list of
`(member name, member context index)`. The C++ adapter replays the list with
`PropSet(TJS_MEMBERENSURE | TJS_IGNOREPROP, …)` on the **parent** object.

---

## 3. The layout rules

### 3.0 The premise, corrected

The reference does **not** pack the areas into flat block-level arrays with
per-context base offsets. Each context owns its **own** code area
(`tjs_int32 *CodeArea`, `reference/…/tjsInterCodeGen.h:394`) and its **own**
constant area (`tTJSVariant *DataArea`, `reference/…/tjsInterCodeGen.h:401`).
The executor indexes them **per context, with no base offset**:

```cpp
tjs_int32 *code = codesave = CodeArea + startip;      // reference/…/tjsInterCodeExec.cpp:859
tTJSVariant *da = DataArea;                            // reference/…/tjsInterCodeExec.cpp:865
```

The only flat, block-level arrays in the reference are the **line table**
(`LineVector` / `LineLengthVector`, `reference/…/tjsScriptBlock.h:50-51`) and
the **context list** (`reference/…/tjsScriptBlock.h:42`).

There *is* a flat-array-with-indices layout in the reference, but it belongs
to the **bytecode file format**: the writer pools every constant of every
context into one type-separated pool (`tjsConstArrayData`,
`reference/…/tjsConstArrayData.h:24-35`) and writes each context's constant
area as a list of `(type, index)` pairs
(`reference/…/tjsInterCodeGen.cpp:4080-4086`), and the writer pools the
strings and octets the same way.

**Consequence for the image.** The image's flat arrays with per-context bases
(the shape the existing `tjs2_codegen_block` already has: `context_constants`,
`context_variables`, `context_args`, `context_labels` in
`crates/tjs/src/vm/codegen/ffi.rs`) are a **transport** choice, not the
reference's runtime layout. They are fine, provided the adapter copies each
context's slice into that context's own C++ array and the C++ side never
indexes the flat array directly. The base and the count are the contract:

| area | the image's shape | the reference's shape |
|---|---|---|
| code | `code` + `code_size`, one array per context | `CodeArea` + `CodeAreaSize`, one array per context — **identical** |
| constants | one flat array + a per-context base and count | one array per context — the adapter copies the slice |
| variables | one flat array + a per-context base and count | not an array at all — only `MaxVariableCount` survives |
| arguments | one flat array + a per-context base and count | not an array at all — only three numbers survive |
| labels | one flat array + a per-context base and count | `SourcePosArray` + `SourcePosArraySize`, one array per context — the adapter copies the slice |
| lines | one flat array, block-level | `LineVector` + `LineLengthVector`, block-level — **identical** |

### 3.1 The code area

* **Unit.** One `tjs_int32` word. The array is `tjs_int32[CodeAreaSize]`
  (`reference/…/tjsInterCodeGen.h:394`); the executor's instruction pointer is
  a `tjs_int32 *` (`reference/…/tjsInterCodeExec.cpp:859`).
* **Order.** The instructions are in execution order. An instruction is the
  opcode word followed by its operand words; the number of operand words is
  fixed per opcode (the disassembler's table,
  `reference/…/tjsDisassemble.cpp:91-105`).
* **Contiguity.** A context's code is contiguous within itself; there is no
  base offset and no interleaving with another context's code. The IP is an
  index into the context's own array (`CodeArea + startip`).
* **Register operands.** `register_index * sizeof(tTJSVariant)`
  (`TJS_TO_VM_REG_ADDR`, `reference/…/tjsInterCodeGen.h:35`). On the
  platforms the VM runs on, `sizeof(tTJSVariant)` is **20** — an
  eight-byte closure or payload, a four-byte type tag, packed to four
  (`reference/…/tjsVariant.h:121-154`; `tjs_int` is a 32-bit `int`,
  `reference/…/tjsTypes.h:51`, and `tTVInteger` is a 64-bit integer,
  `reference/…/tjsTypes.h:59`). The C++ side must not re-encode these.
* **Constant-area operands.** `constant_index * sizeof(tTJSVariant)` — the
  same multiplication, because the constant area is an array of
  `tTJSVariant` (`reference/…/tjsInterCodeExec.cpp:1409`,
  `TJS_GET_VM_REG(DataArea, code[3])`).
* **Code-address operands (jumps).** After the generator's fix-up, a jump's
  operand is `(target_word_index - jump_word_index) * 4` — a **byte** offset
  from the jump instruction's own word
  (`TJS_TO_VM_CODE_ADDR`, `reference/…/tjsInterCodeGen.h:34`; the executor
  adds it to the instruction pointer,
  `TJS_ADD_VM_CODE_ADDR`, `reference/…/tjsInterCodeGen.h:38`,
  `reference/…/tjsInterCodeExec.cpp:952-968`). The multiplication is applied
  as the **last** step of the fix-up
  (`reference/…/tjsInterCodeGen.cpp:865-870`).
* **The one exception.** `VM_ENTRY`'s operand is the same byte offset
  (`reference/…/tjsInterCodeExec.cpp:1253-1261` reads it with
  `TJS_FROM_VM_CODE_ADDR`). It is produced by the same conversion because
  `VM_ENTRY` is on the jump list (`reference/…/tjsInterCodeGen.cpp:3510`).
* **Immediate operands.** The raw value: an argument count, an argument kind,
  a `-1` / `-2` marker (`reference/…/tjsInterCodeGen.cpp:3683-3706`).

### 3.2 The constant area

* **Unit.** One `tTJSVariant` (`reference/…/tjsInterCodeGen.h:401`).
* **Order.** The order the constants were created in; the code refers to
  them by index.
* **Contiguity.** Per context; the adapter copies the context's slice out of
  the image's flat array into a fresh `new tTJSVariant[count]`.
* **Dedup.** The reference's `PutData` re-uses an equal constant found among
  the **last twenty** entries (`reference/…/tjsInterCodeGen.cpp:656-670`).
  This changes the *indices*, so it is the Rust side's business and the C++
  side must **not** dedup.

### 3.3 The source positions

* **Unit.** One `tSourcePos` — a code position and a source position
  (`reference/…/tjsInterCodeGen.h:274-279`).
* **Order.** **Sorted by code position.** The reference sorts them before use
  (`SortSourcePos`, `reference/…/tjsInterCodeGen.cpp:693-700`; the binary
  search in `CodePosToSrcPos`, `reference/…/tjsInterCodeGen.cpp:1041-1063`).
* **Contiguity.** Per context.
* **One entry per source-position change.** The reference records an entry
  only when the source position differs from the previous one
  (`PrevSourcePos`, `reference/…/tjsInterCodeGen.cpp:602-627`).

### 3.4 The variable area

* The reference has no array; only `MaxVariableCount` survives
  (`reference/…/tjsInterCodeGen.cpp:994`).
* `MaxVariableCount` is the **maximum** number of name-space entries the
  context ever had (`tTJSLocalNamespace::GetMaxCount`,
  `reference/…/tjsNamespace.h:85`; the count is maintained by
  `Commit`, `reference/…/tjsNamespace.cpp:204-213`), which includes the
  declared arguments.
* The executor allocates `MaxVariableCount + VariableReserveCount + 1 +
  MaxFrameCount` registers (`reference/…/tjsInterCodeExec.cpp:641`) and puts
  `ra` at `regs + MaxVariableCount + VariableReserveCount`
  (`reference/…/tjsInterCodeExec.cpp:648`).

### 3.5 The function-argument area

* No array; three numbers (§2.5).
* `FuncDeclCollapseBase` is −1 when the context declares no named `*`
  argument (`reference/…/tjsInterCodeGen.cpp:199`, `:333`), and the executor
  tests `>= 0` before using it (`reference/…/tjsInterCodeExec.cpp:731`).

### 3.6 The line table

* Two parallel block-level arrays; the image carries them as one array of
  `(position, length)` pairs (§2.1).
* The reference's rule for splitting is at
  `reference/…/tjsScriptBlock.cpp:207-232` (and, for the bytecode path, at
  `reference/…/tjsScriptBlock.cpp:604-628`).

---

## 4. The ownership and the lifetime

### 4.1 What the reference owns

| buffer | allocated by | freed by | reference |
|---|---|---|---|
| `CodeArea` | `TJS_realloc` in `PutCode` (`reference/…/tjsInterCodeGen.cpp:593-600`) | `TJS_free` in `Finalize` (`reference/…/tjsInterCodeGen.cpp:381-382`) | `reference/…/tjsInterCodeGen.h:394` |
| `DataArea` | `new tTJSVariant[]` in `Commit` (`reference/…/tjsInterCodeGen.cpp:975-977`) | `delete[]` in `Finalize` (`reference/…/tjsInterCodeGen.cpp:389-392`) | `reference/…/tjsInterCodeGen.h:401` |
| `SourcePosArray` | `TJS_malloc` / `TJS_realloc` in `PutCode` (`reference/…/tjsInterCodeGen.cpp:606-622`) | `TJS_free` in `Finalize` (`reference/…/tjsInterCodeGen.cpp:403-404`) | `reference/…/tjsInterCodeGen.h:427` |

**Every buffer belongs to the context, and the context frees it.** The
executor holds raw pointers into them for the whole life of the context
(`CodeArea` at `reference/…/tjsInterCodeExec.cpp:859`, `DataArea` at `:865`),
and the disassembler reads them too (`reference/…/tjsDisassemble.cpp:57-105`).

### 4.2 What the image owns

The Rust handle owns every buffer of the image: the code arrays, the
constant arrays, the string and octet payloads, the name and label arrays and
the line table. `tjs2_script_free` releases them all. The image's pointers
stay valid **until the handle is freed** and are **read-only** for the C++.

### 4.3 The rule

**The C++ copies; the Rust frees.**

The adapter must copy every buffer into storage the C++ owns, in the
allocator the context frees with:

| the image's buffer | the C++'s copy | freed by |
|---|---|---|
| `code` | `TJS_malloc(count * sizeof(tjs_int32))` + `memcpy` | `TJS_free` in `Finalize` |
| `constants` | `new tTJSVariant[count]`, each entry assigned | `delete[]` in `Finalize` |
| `labels` | `TJS_malloc(count * sizeof(tSourcePos))` + `memcpy` | `TJS_free` in `Finalize` |
| the strings and octets | `tTJSVariantString` / `tTJSVariantOctet` inside the constants | the variants' own reference counting |
| the names | `new tjs_char[len+1]` in the context | `delete[] Name` in the destructor (`reference/…/tjsInterCodeGen.cpp:367-369`) |
| the line table | copied into the block's two `std::vector`s | the vectors |

This is exactly what the reference's own bytecode loader does: it `TJS_malloc`s
the code (`reference/…/tjsByteCodeLoader.cpp:235-241`), `new[]`s the constant
array (`:254`), and hands both to the context, which frees them in
`Finalize`. The loader's buffers come from the file; the adapter's come from
the image. **The ownership arrangement is identical.**

**One deviation from the loader, and it is deliberate.** The reference's
loader allocates the source positions with `new[]`
(`reference/…/tjsByteCodeLoader.cpp:218`) while `Finalize` frees them with
`TJS_free` (`reference/…/tjsInterCodeGen.cpp:403-404`) — a mismatched
allocator pair that the reference only survives because the bytecode path
rarely carries source positions. The adapter must allocate them with
`TJS_malloc`, so that the pair matches. (The vendored tree already made
this correction in its loader: `crates/tjs/cpp/tjs2/tjsByteCodeLoader.cpp:317`.)

**Why the C++ may not borrow.** The context frees its buffers in `Finalize`
(`reference/…/tjsInterCodeGen.cpp:373-413`), which runs when the block is
released. If the context pointed into the Rust image, the block would outlive
the handle. The alternative — keeping the handle alive for the block's life —
would make the block's lifetime depend on a Rust handle the C++ does not own,
and it would contradict the reference's own model. **Copy.**

**Why the C++ may not free the image's buffers.** The image's memory belongs
to Rust; freeing it in C++ would be a cross-allocator free. `tjs2_script_free`
is the only way to release it.

### 4.4 The adapter's procedure

Modelled on the reference's bytecode loader
(`reference/…/tjsByteCodeLoader.cpp:180-378`).

```cpp
// 0. the block's own state is already in place: the text, the name, the
//    line offset, and the line table (SetText / SetName).
// 1. call the Rust entry.
tjs2_script *script = tjs2_script_compile(text, length, isexpr, resultneeded);
if(!script)
    TJS_eTJSInternalError();                       // allocation failure

// 2. the failure path: the reference raises the first error with its
//    position, after counting every recovered error.
if(tjs2_script_status(script) != TJS2_COMPILE_OK) {
    const char *message = tjs2_script_error_message(script);
    tjs_int position = tjs2_script_error_position(script);
    SetFirstError(message, position);              // reference/…/tjsScriptBlock.cpp:450-453
    CompileErrorCount = tjs2_script_error_count(script);
    tjs2_script_free(script);
    TJS_eTJSScriptError(ttstr(message), this, position);  // reference/…/tjsScriptBlock.cpp:444-446
}

// 3. the preprocessor flag.
if(tjs2_script_using_preprocessor(script))
    NotifyUsingPreProcessor();                     // reference/…/tjsScriptBlock.cpp:107

// 4. the image.
const tjs2_codegen_block *image = tjs2_script_block_image(script);

// 5. pass one: make the contexts.
std::vector<tTJSInterCodeContext *> objs(image->context_count, nullptr);
for(int32_t i = 0; i < image->context_count; i++) {
    const tjs2_codegen_context &c = image->contexts[i];

    tjs_int32 *code = (tjs_int32 *)TJS_malloc(c.code_size * sizeof(tjs_int32));
    memcpy(code, c.code, c.code_size * sizeof(tjs_int32));

    tTJSVariant *data = new tTJSVariant[c.constant_count];
    for(int32_t j = 0; j < c.constant_count; j++)
        FillConstant(data[j], c.constants[j]);      // §2.3

    tTJSInterCodeContext::tSourcePos *src = nullptr;
    if(c.label_count) {
        src = (tTJSInterCodeContext::tSourcePos *)
              TJS_malloc(c.label_count * sizeof(tTJSInterCodeContext::tSourcePos));
        for(int32_t j = 0; j < c.label_count; j++) {
            src[j].CodePos = c.labels[j].code_pos;
            src[j].SourcePos = c.labels[j].source_pos;
        }
    }

    std::vector<tjs_int> scgp(c.super_class_getter_pointer,
                              c.super_class_getter_pointer + c.super_class_getter_pointer_count);

    objs[i] = new tTJSInterCodeContext(
        this, name_of(c), (tTJSContextType)c.kind,
        code, c.code_size, data, c.constant_count,
        c.variable_count,             // MaxVariableCount
        c.variable_reserve_count,     // VariableReserveCount
        c.max_frame_count,            // MaxFrameCount
        c.func_arg_count,             // FuncDeclArgCount
        c.func_unnamed_arg_base,       // FuncDeclUnnamedArgArrayBase
        c.func_collapse_base,         // FuncDeclCollapseBase
        true,                         // the source positions are sorted
        src, c.label_count,
        scgp);                        // reference/…/tjsInterCodeGen.h:849
}

// 6. pass two: the relations between the contexts.
for(int32_t i = 0; i < image->context_count; i++) {
    const tjs2_codegen_context &c = image->contexts[i];
    objs[i]->SetCodeObject(
        c.parent           >= 0 ? objs[c.parent]           : nullptr,
        c.prop_setter      >= 0 ? objs[c.prop_setter]      : nullptr,
        c.prop_getter      >= 0 ? objs[c.prop_getter]      : nullptr,
        c.super_class_getter >= 0 ? objs[c.super_class_getter] : nullptr);
}

// 7. pass three: the member registrations the reference's RegisterFunction
//    performs on the parent objects.
for(int32_t i = 0; i < image->context_count; i++) {
    const tjs2_codegen_context &c = image->contexts[i];
    if(c.property_count == 0) continue;
    tTJSInterCodeContext *parent = objs[c.parent];
    for(int32_t j = 0; j < c.property_count; j++) {
        tTJSVariant val = objs[c.properties[j].member];
        parent->PropSet(TJS_MEMBERENSURE | TJS_IGNOREPROP,
                        name_of(c.properties[j]), nullptr, &val, parent);
    }
}

// 8. pass four: the constants that hold a context.
for(int32_t i = 0; i < image->context_count; i++) {
    const tjs2_codegen_context &c = image->contexts[i];
    for(int32_t j = 0; j < c.constant_count; j++) {
        if(c.constants[j].kind == TJS2_CG_VALUE_CONTEXT) {
            data_area_of(objs[i])[j] = objs[c.constants[j].object];
        }
    }
}

// 9. the block's own state: the top level and the context list.
SetObjects(top_level_of(image), objs, image->context_count);

tjs2_script_free(script);
```

`SetObjects` (`reference/…/tjsScriptBlock.h:145-155`) sets
`TopLevelContext`, adds every context to `InterCodeContextList` and
`AddRef`s the block once per context other than the top level — which is the
block reference the contexts release in `Finalize`
(`reference/…/tjsInterCodeGen.cpp:396-397`).

**The context order must be the creation order** (§2.1), and the image's
context index is the identity the C++ uses for `parent`, `prop_setter`,
`prop_getter`, `super_class_getter`, the member registrations and the
context-valued constants. All of them are resolved against the same array.

---

## 5. The post-processing the C++ does today that the Rust side must own

**This is the decisive section.** The reference's generator does a long list of
things to a freshly generated context after the grammar has built it. Under
the decided order, the Rust generator performs all of them, and the C++ side
must perform **none** of them. Each entry below says what the reference does,
where, and where it goes.

### 5.1 The list

| # | the step | where in the reference | where it goes |
|---|---|---|---|
| P1 | the super-class-getter context is committed **before** the class that owns it | `reference/…/tjsInterCodeGen.cpp:958-962` | **Rust** |
| P2 | the `VM_SRV %0` + `VM_RET` epilogue is appended, **except** for `ctProperty` and `ctSuperClassGetter` | `reference/…/tjsInterCodeGen.cpp:964-968` | **Rust** |
| P3 | `RegisterFunction`: the setter context is linked to its parent | `reference/…/tjsInterCodeGen.cpp:887-891` | **Rust** decides; the C++ links (`SetCodeObject`) |
| P4 | `RegisterFunction`: the getter context is linked to its parent | `reference/…/tjsInterCodeGen.cpp:892-896` | **Rust** decides; the C++ links (`SetCodeObject`) |
| P5 | `RegisterFunction`: for a top-level parent, the **child context object** and the **child's name** are appended to the parent's constant area, and the pair is recorded with a `ChangeThis` flag | `reference/…/tjsInterCodeGen.cpp:909-918` | **Rust** decides the slots and the flag; the **C++** fills the context-valued slot with the object |
| P6 | `RegisterFunction`: for a function child of a function parent, the child context object is appended to the parent's constant area and `InitLocalFunction` creates the parent's local variable **and its `VM_CONST` initialisation code** | `reference/…/tjsInterCodeGen.cpp:921-931`; `InitLocalFunction` at `:2898-2911` | **Rust** (the code and the slot); the C++ fills the slot |
| P7 | `RegisterFunction`: the child is registered as a **member of the parent object** (`PropSet(TJS_MEMBERENSURE \| TJS_IGNOREPROP, Name, …)`) | `reference/…/tjsInterCodeGen.cpp:933-945` | **Rust** decides the list; the **C++** performs the `PropSet` (§4.4 step 7) |
| P8 | `FixCode`: for a class, a one-word `VM_REGMEMBER` instruction is inserted at `FunctionRegisterCodePoint` | `reference/…/tjsInterCodeGen.cpp:707-717` | **Rust** |
| P9 | `FixCode`: for a non-empty non-local-function list, `MaxFrameCount` is raised to at least 1 and a registration prologue is built and inserted at `FunctionRegisterCodePoint` — per declaration `VM_CONST %1, #data`, optionally `VM_CHGTHIS %1, %-1`, then `VM_SPDS %-1, #name, %1` — followed by one `VM_CL %1` | `reference/…/tjsInterCodeGen.cpp:719-772` | **Rust** |
| P10 | `FixCode`: the source-position table is sorted by code position | `reference/…/tjsInterCodeGen.cpp:774-775`; `SortSourcePos` at `:693-700` | **Rust** (the image's table is sorted; the C++ passes `srcsorted = true`) |
| P11 | `FixCode`: the fix-up list is applied — every jump is re-based across the inserted regions, the code is moved, and **the source positions at or after the moved region are shifted by the size delta** | `reference/…/tjsInterCodeGen.cpp:777-826` | **Rust** |
| P12 | `FixCode`: the redundant-jump elimination — a jump whose target is a jump is rewritten to skip it; a `JF` after a `JNF` (or the reverse) becomes a fall-through | `reference/…/tjsInterCodeGen.cpp:828-863` | **Rust** |
| P13 | `FixCode`: the jump operands are converted from word offsets to **byte** offsets (`TJS_TO_VM_CODE_ADDR`) | `reference/…/tjsInterCodeGen.cpp:865-870` | **Rust** |
| P14 | `Commit`: the constant area is materialised — `DataArea = new tTJSVariant[_DataAreaSize]`, each entry `CopyRef`'d, the build-time area freed | `reference/…/tjsInterCodeGen.cpp:975-989` | the **contents and their order** are Rust's; the **materialisation** is the C++'s (§4.4 step 5) |
| P15 | `Commit`: `MaxVariableCount` is set — 2 for `ctSuperClassGetter`, otherwise the name space's maximum | `reference/…/tjsInterCodeGen.cpp:991-995` | **Rust** (the number is in the image) |
| P16 | `Commit`: `SuperClassExpr` is cleared and the expression nodes are deleted | `reference/…/tjsInterCodeGen.cpp:997-999` | **Rust** (the nodes never cross) |
| P17 | `Commit`: the source-position table is compacted to its exact size | `reference/…/tjsInterCodeGen.cpp:1001-1008` | **Rust** (the image's table is already exact) |
| P18 | `Commit`: the code area is compacted to its exact size | `reference/…/tjsInterCodeGen.cpp:1010-1017` | **Rust** (the image's code is already exact) |
| P19 | `Commit`: the debug object-type information is set | `reference/…/tjsInterCodeGen.cpp:1019-1021` | **Rust** (or the C++ — it is a debug-only label; the reference calls `GetShortDescriptionWithClassName`, which needs the context's name and kind, both in the image) |
| P20 | `Commit`: the compile-time vectors and stacks are destroyed and re-created so their storage is released | `reference/…/tjsInterCodeGen.cpp:1023-1038` | **Rust** |
| P21 | `PutData`: the constant dedup over the last twenty entries | `reference/…/tjsInterCodeGen.cpp:656-670` | **Rust** (it changes the indices) |
| P22 | `PutCode`: the source-position table is grown and the entries are recorded, one per source-position change | `reference/…/tjsInterCodeGen.cpp:602-627` | **Rust** |
| P23 | `PutData`: the string constants are interned in the global string map | `reference/…/tjsInterCodeGen.cpp:672-678` (`TJSMapGlobalStringMap`) | **either** — it is a pure optimisation with no observable effect; the C++ side may intern when it builds the `tTJSVariant`, or not |
| P24 | `ClearFrame` / `ClearLocalVariable`: the frame is rewound and `MaxFrameCount` is raised; the clearing code itself is `#if 0`'d out | `reference/…/tjsInterCodeGen.cpp:2793-2812`, `:2814-2843` | **Rust** (the `MaxFrameCount` number is in the image) |
| P25 | `AddLocalVariable`: a local that is really a member of `this` is compiled to `VM_SPDS %-1, #name, %init` | `reference/…/tjsInterCodeGen.cpp:2875-2882` | **Rust** (it is code) |
| P26 | the class prologue: `VM_CONST %1, #name`, `VM_ADDCI %-1, %1`, `VM_CL %1` and `FunctionRegisterCodePoint` | `reference/…/tjsInterCodeGen.cpp:264-285` | **Rust** |
| P27 | the super-class proxy: the `SuperClassGetter` context is created and its `SuperClassGetterPointer` is filled | `reference/…/tjsInterCodeGen.cpp:3626-3650` | **Rust** decides; the C++ links (`SetCodeObject`) and copies the pointer vector |

### 5.2 What stays on the C++ side

| # | the step | where in the reference |
|---|---|---|
| C1 | the block's own text is copied and the **line table** is computed from it | `reference/…/tjsScriptBlock.cpp:207-232`; the bytecode path at `:604-628` |
| C2 | the block's name and line offset are set | `reference/…/tjsScriptBlock.cpp:95-105` |
| C3 | the contexts are **created** and their buffers are **copied** from the image | the loader's model, `reference/…/tjsByteCodeLoader.cpp:321-327` |
| C4 | the relations between the contexts are **linked** | `SetCodeObject`, `reference/…/tjsInterCodeGen.h:831-847`; the loader's model, `reference/…/tjsByteCodeLoader.cpp:348-349` |
| C5 | the **member registrations** are performed on the parent objects | `reference/…/tjsByteCodeLoader.cpp:351-365` (the model's own replay) |
| C6 | the **context-valued constants** are resolved to the context objects | `reference/…/tjsByteCodeLoader.cpp:367-372` (the model's own resolution) |
| C7 | the block's **top level** and the **context list** are set | `SetObjects`, `reference/…/tjsScriptBlock.h:145-155` |
| C8 | the **compile error** is raised | `reference/…/tjsScriptBlock.cpp:444-446` |
| C9 | the **preprocessor flag** is set on the block | `reference/…/tjsLex.cpp:1132`, `:1174` → `reference/…/tjsScriptBlock.h:107` |
| C10 | the **top-level script is executed** | `reference/…/tjsScriptBlock.cpp:308`, `:378-393` |

### 5.3 The rule that follows

**The image is final.** The C++ side may not add to, remove from, reorder or
re-encode the code stream, the constant area, the source positions or the
counts it receives. In particular it must **not**:

- run any part of `FixCode` — no re-basing of jumps, no redundant-jump
  elimination, no jump-address conversion (P11, P12, P13);
- append the `SRV`/`RET` epilogue (P2) or the `VM_REGMEMBER` word (P8) or the
  non-local function prologue (P9);
- sort or shift the source positions (P10, P11);
- dedup the constants (P21);
- raise `MaxFrameCount`, `MaxVariableCount` or `VariableReserveCount` (P15,
  P24);
- run `TranslateCodeAddress` on the live path. That function
  (`reference/…/tjsScriptBlock.cpp:876-…`, the opcode table at `:789-873`) is
  the **bytecode round-trip**: it converts byte-unit operands back to
  word-unit operands when a bytecode file is written
  (`reference/…/tjsInterCodeGen.cpp:4072`) and when one is read
  (`reference/…/tjsByteCodeLoader.cpp:242`). The live path has no such
  conversion: the generator's fix-up already produced the final byte-unit
  operands.

---

## 6. The line table and the source positions

### 6.1 What the reference records

**Per context**, a table of `tSourcePos` records
(`reference/…/tjsInterCodeGen.h:274-279`):

```cpp
struct tSourcePos {
    tjs_int CodePos;    // the word index in the context's code area
    tjs_int SourcePos;  // the code-unit offset in the script
};
```

The table is filled by `PutCode` (`reference/…/tjsInterCodeGen.cpp:584-628`):
when a code word is emitted with a source position that differs from the
previous one, a record is appended with `CodePos` = the word index the word
is about to occupy and `SourcePos` = the position. `PutCode`'s `pos` is
`LEX_POS` (the analyser's current position,
`reference/…/tjsInterCodeGen.cpp:25`) or `NODE_POS` (the expression node's
position, `reference/…/tjsInterCodeGen.cpp:26`).

The table is sorted by code position before use
(`reference/…/tjsInterCodeGen.cpp:693-700`) and is shifted when the fix-up
moves code (`reference/…/tjsInterCodeGen.cpp:807-812`).

**Per block**, the line table: `LineVector` (each line's start offset) and
`LineLengthVector` (each line's length), built by scanning the text
(`reference/…/tjsScriptBlock.cpp:207-232`).

### 6.2 What the executor does with them

| use | where |
|---|---|
| a code position → a source position (binary search over the sorted table) | `CodePosToSrcPos`, `reference/…/tjsInterCodeGen.cpp:1041-1063` |
| a code position → the first instruction of its source line | `FindSrcLineStartCodePos`, `reference/…/tjsInterCodeGen.cpp:1066-1088` |
| a code position → a human-readable position (`name(line)[context]`) | `GetPositionDescriptionString`, `reference/…/tjsInterCodeGen.cpp:1090-1093` |
| a source position → a line number (binary search over `LineVector`) | `SrcPosToLine`, `reference/…/tjsScriptBlock.cpp:154-167` |
| a line → the line's text and length | `GetLine`, `reference/…/tjsScriptBlock.cpp:140-152` |
| a source position → `name(line)` | `GetLineDescriptionString`, `reference/…/tjsScriptBlock.cpp:176-192` |
| the exception's line number | `eTJSScriptError::GetSourceLine`, `reference/…/tjsError.cpp:84-86` |
| the exception's trace line | `eTJSScriptError::AddTrace`, `reference/…/tjsError.cpp:95-119` |
| the `VM_THROW` handler's source position | `reference/…/tjsInterCodeExec.cpp:1266-1270` |
| the error report's disassembly of the failing line | `DisplayExceptionGeneratedCode` → `DisassembleSrcLine`, `reference/…/tjsInterCodeExec.cpp:786-792`; `reference/…/tjsDisassemble.cpp:655-…` |
| the disassembler's source-line comments | `reference/…/tjsDisassemble.cpp:57-85` |
| the source section of a bytecode file | `reference/…/tjsInterCodeGen.cpp:4030-4034`, `:4059-4067` |

### 6.3 What the C++ side must do with them

1. **Copy** the context's table into a `TJS_malloc`'d `tSourcePos` array,
   **already sorted by code position**, and pass it to the context with
   `srcsorted = true` (`reference/…/tjsInterCodeGen.h:849-857`). The
   reference's bytecode constructor takes the flag and sets
   `SourcePosArraySorted = true` (`reference/…/tjsInterCodeGen.cpp:337-340`).
2. **Copy** the block's line table into `LineVector` and `LineLengthVector`,
   in the order the image gives (the image's `lines[i].position` →
   `LineVector[i]`, `lines[i].length` → `LineLengthVector[i]`).
3. **Do not re-derive** either table. The C++ side must not rescan the text
   to rebuild the line table (the reference's `SetText` already does that,
   and the two must agree — the image's table is the cross-check), and it must
   not re-sort the source positions (`SortSourcePos` would be a no-op on a
   sorted table, but re-sorting is a C++-side post-processing step that the
   Rust side owns — P10).
4. **Not do anything else with them.** They are read-only diagnostics. The
   C++ side does not need them to execute anything.

**The line offset.** The block's `LineOffset` (`reference/…/tjsScriptBlock.h:40`)
shifts the line numbers: `SrcPosToLine` adds it (`reference/…/tjsScriptBlock.cpp:164`)
and `GetLine` subtracts it (`:143`). It is the C++'s own (`SetName`), and the
image does not carry it.

---

## 7. The what-must-not-be-done list

### 7.1 The C++ side must not

1. **Run the grammar.** No `parser{ this }.parse()`
   (`reference/…/tjsScriptBlock.cpp:434`), no bison-generated parser, no
   `tjs.y`.
2. **Run the lexer.** No `tTJSLexicalAnalyzer`
   (`reference/…/tjsScriptBlock.cpp:430-431`), no `tjsLex.cpp`'s analyser. The
   Rust entry does the lexing, the parsing and the code generation.
3. **Call the lexer or the parser separately.** One entry, one call
   (§1.3).
4. **Do any of the post-processing in §5.1.** In particular no `FixCode`, no
   jump re-basing, no redundant-jump elimination, no jump-address
   conversion, no `SRV`/`RET` epilogue, no `VM_REGMEMBER`, no non-local
   function prologue, no source-position sorting or shifting, no constant
   dedup, no `MaxFrameCount` / `MaxVariableCount` adjustment.
5. **Modify the image.** The code stream, the constant area, the source
   positions, the counts and the relations are final. The C++ side may only
   *copy* them and *resolve* the two indirections the image cannot carry:
   the context-valued constants (§2.3) and the member registrations (§2.8).
6. **Keep a pointer into the image.** Every buffer is copied into
   C++-owned storage (§4.3), because the context frees its own buffers in
   `Finalize` and the Rust handle dies first.
7. **Free the image's buffers.** `tjs2_script_free` is the only way.
8. **Re-encode the operands.** The register and constant operands are byte
   offsets with `sizeof(tTJSVariant) == 20`
   (`reference/…/tjsInterCodeGen.h:35`; `reference/…/tjsVariant.h:121-154`),
   and the jump operands are byte offsets
   (`reference/…/tjsInterCodeGen.h:34`). The C++ side must not normalise them
   and must not run `TranslateCodeAddress` on the live path (§5.3).
9. **Reorder the contexts.** The image's context order is the creation order,
   and it is observable through `GetCodeIndex`, `Dump` and `ExportByteCode`
   (§2.1).
10. **Rebuild the line table by scanning the text** when the image carries
    one. The reference's `SetText` builds it from the block's own text
    (`reference/…/tjsScriptBlock.cpp:207-232`); the image's table must agree
    with it, and the adapter checks rather than substitutes.
11. **Raise a different error.** The reference's compile error is
    `TJS_eTJSScriptError(FirstError, this, FirstErrorPos)` with the message
    `"Syntax error (%1)"` for a syntax failure
    (`reference/…/tjsError.h:288`, `reference/…/tjsInterCodeGen.cpp:68-73`),
    and the position is the first failure's, not the last's.
12. **Change the bytecode export path's behaviour.** `ExportByteCode`
    (`reference/…/tjsScriptBlock.cpp:537-576`) and
    `tTJSInterCodeContext::ExportByteCode`
    (`reference/…/tjsInterCodeGen.cpp:4006-4110`) consume the contexts the
    adapter built; they must keep working unchanged.

### 7.2 Every place in the vendored tree that has to change

| the vendored file | the change |
|---|---|
| `crates/tjs/cpp/tjs2/tjsScriptBlock.cpp` | `Parse` (`:437`) becomes the call to the Rust entry plus the adapter (§1.2, §4.4). `SetText` (`:236`) keeps its line table and its top-level execution; it stops building the line table only if the image's table is adopted instead (it must not be). `Compile` (`:664`) is unchanged apart from riding on the new `Parse`. |
| `crates/tjs/cpp/tjs2/tjsScriptBlock.h` | `Parse`'s signature is unchanged (`reference/…/tjsScriptBlock.h:94`); a private `BuildFromRustImage` is added; the `tjsLex.h` include goes when the analyser's header goes (§7.3). |
| `crates/tjs/cpp/tjs2/tjsInterCodeGen.cpp` | the **generator half** is deleted: `PutCode` (`:584`), `PutData` (`:638`), `SortSourcePos` (`:693`), `FixCode` (`:702`), `RegisterFunction` (`:881`), `Commit` (`:952`), the whole `GenNodeCode` / `_GenNodeCode` pair (`:1110`, `:2724`), the nest / jump / fix bookkeeping (`EnterWhileCode` … `AddFunctionDeclArgCollapse`), the `MakeNP*` node builders (`:3800` …), and the `tTJSExprNode` class (`tjsInterCodeGen.h:181-234`). The **executor half** stays. |
| `crates/tjs/cpp/tjs2/tjsInterCodeGen.h` | the class shrinks to what the executor and the adapter need: the code area, the constant area, the source positions, the counts, the relations and the `tSourcePos` record. `tTJSExprNode` goes; `tSubParam` goes; the compile-time fields go; the VM codes stay in `crates/tjs/cpp/tjs2/tjsVMCodes.h` (the vendored tree already moved them out of this header). |
| `crates/tjs/cpp/tjs2/tjsLex.cpp`, `tjsLex.h` | the analyser goes. `tjsLex.h` also declares `tTJSLexicalAnalyzer`, which `tjsScriptBlock.h` includes — that include must go with it. The date literal parser (`tjsDateParser.cpp`, `bison/tjsdate.y`) and the preprocessor's expression parser (`bison/tjspp.y`, `tjsCompileControl.cpp`) are **not** part of the script front end and stay. |
| `crates/tjs/cpp/tjs2/bison/tjs.y` | the script grammar goes. `bison/tjspp.y` and `bison/tjsdate.y` stay. |
| `crates/tjs/cpp/tjs2_codegen.h` | the shipped seam's header: the compile entry, the extended image records (`parent`, `prop_setter`, `prop_getter`, `super_class_getter`, `super_class_getter_pointer`, `properties`, `top_level`) and the `TJS2_CG_VALUE_CONTEXT` / `TJS2_CG_VALUE_NULL` constant kinds. |
| `crates/tjs/src/vm/mod.rs` | the Rust side of the same seam: `tjs2_script_compile` and its accessors. |
| `crates/tjs/src/vm/codegen/ffi.rs` | the image records the same seam carries. |
| `crates/tjs/src/vm/parser/` | the parser column (it does not exist yet; see `docs/code/tjs/columns.md` §1). |
| `crates/tjs/build.rs` | the bison step drops `tjs.y` (`:96`, `:470-478`, `:553`); the source list drops `tjsLex.cpp` (`:62`); the Rust codegen object is linked. |
| `crates/tjs/cpp/tjs2_abi.cpp`, `tjs2_abi.h` | unchanged — `tjs2_exec_script` and `tjs2_compile_script` reach the same `SetText` / `Compile` and inherit the new behaviour. |
| `scripts/krkr_gates/checks/layering.py` | the reviewed FFI location list names the seam file: `crates/tjs/src/vm/mod.rs`. |

### 7.3 The vendored tree and the reference differ

The differences that bear on this design (the reference wins):

| the difference | where | the effect on this design |
|---|---|---|
| the VM opcodes moved out of `tjsInterCodeGen.h` into a new `tjsVMCodes.h` | `crates/tjs/cpp/tjs2/tjsVMCodes.h` (new file) | none — the opcode numbering is the same |
| `parser::error` now raises instead of only logging | `crates/tjs/cpp/tjs2/tjsInterCodeGen.cpp:46-56` | the vendored tree aborts the parse on a grammar error; the reference recovers and counts the errors (§5.1, P-list / §1.3's `tjs2_script_error_count`). The Rust entry must reproduce the **reference's** behaviour, and the vendored patch's behaviour is a divergence to reconcile. |
| the block grew `SetSourceText` and the bytecode format grew a `SRCS` section | `crates/tjs/cpp/tjs2/tjsScriptBlock.h:96`, `crates/tjs/cpp/tjs2/tjsScriptBlock.cpp:201-233`, `:551-553` | the vendored bytecode format is **not** the reference's. It does not change the live path's shape, but the adapter must not assume the reference's file format. |
| the context grew `GetSourcePosArray`, `GetSourcePosArraySize` and `SetSourcePosArray` | `crates/tjs/cpp/tjs2/tjsInterCodeGen.h:762-779` | these exist for the vendored bytecode writer; the adapter does not need them (it writes the table straight into the constructor). |
| the bytecode loader allocates the source positions with `TJS_malloc` instead of `new[]` | `crates/tjs/cpp/tjs2/tjsByteCodeLoader.cpp:317` (the reference uses `new[]` at `reference/…/tjsByteCodeLoader.cpp:218`, which `Finalize` frees with `TJS_free` — a mismatched pair) | the vendored tree is right; the adapter must use `TJS_malloc` for the source positions (§4.3) |
| the null-safe string-length helper | `crates/tjs/cpp/tjs2/tjsInterCodeExec.cpp:55-70` | an unrelated hardening; no effect. |
| `TJSVariantStringChars` instead of `*str` at the property calls | `crates/tjs/cpp/tjs2/tjsInterCodeExec.cpp:1517`, `:1577`, `:1653` | unrelated; no effect. |

---

## 8. Ambiguities in the reference

| # | the ambiguity | the two readings | which I believe, and why |
|---|---|---|---|
| A1 | **The order of the member registrations.** In the live path the registrations happen in **commit order** (a child registers itself on its parent when the child commits — `reference/…/tjsInterCodeGen.cpp:933-945`); in the bytecode path they are **replayed** grouped by child in **creation order** (`reference/…/tjsByteCodeLoader.cpp:351-365`). | (a) The live order is the truth and the image must carry the registrations in commit order. (b) The two are equivalent and the image may carry them grouped per child. | **(a)**, and the image should carry the registrations on the **parent** in commit order, because `PropSet` with `TJS_MEMBERENSURE` overwrites an existing member (`reference/…/tjsObject.cpp:1375-1380`, `:1418-1420`), so the last write wins and the order is observable when two children of one parent register the same member name. The reference's live path is the oracle. |
| A2 | **The `SuperClassGetter` reference count.** `SetCodeObject` assigns `SuperClassGetter` **without** an `AddRef` (`reference/…/tjsInterCodeGen.h:831-847`), but `Finalize` **releases** it (`reference/…/tjsInterCodeGen.cpp:378-379`). | (a) The creation reference (the context starts at `RefCount == 1`, `reference/…/tjsObject.cpp:85-91`) is the one the class's `Finalize` consumes — balanced, exactly as in the live path. (b) It is an imbalance that the bytecode path happens not to hit. | **(a)**: the live path creates the proxy with `new` (reference count 1) and the class's `Finalize` releases it — the same net effect. The adapter must **not** add an `AddRef` of its own, or the proxy leaks. |
| A3 | **Whether `FixCode` runs for `ctProperty` and `ctSuperClassGetter`.** `Commit` skips `FixCode` for those two types (`reference/…/tjsInterCodeGen.cpp:972-973`), so their jump operands would stay in **word** units while every other context's are in **byte** units. | (a) It never matters: a `ctProperty` context has no code at all (it gets no `SRV`/`RET`, `reference/…/tjsInterCodeGen.cpp:964-968`), and a `ctSuperClassGetter`'s code is `SRV`/`RET`/`NOP` plus an expression (`reference/…/tjsInterCodeGen.cpp:3636-3650`), neither of which contains a jump. (b) It is a latent inconsistency that the bytecode round-trip would expose. | **(a)**, and the Rust side must mirror it exactly: the byte-unit conversion applies to the contexts the reference converts. If the Rust generator ever emits a jump in such a context, the C++ side will mis-execute it — so the Rust side must keep the reference's rule and the C++ side must not "fix" it. |
| A4 | **Whether the error is raised at the end of the parse or at the first error.** The reference's `_yyerror` **recovers**: it counts the error, keeps the first one, prints to the console, and returns 0 so the grammar continues (`reference/…/tjsInterCodeGen.cpp:52-87`); the grammar's own recovery rule aborts after more than 20 errors (`reference/krkr2/cpp/core/tjs2/bison/tjs.y:211-213`). `Parse` raises at the end (`reference/…/tjsScriptBlock.cpp:444-446`). | (a) The Rust entry must reproduce the recovery and report the first error and the count. (b) The Rust entry may fail fast and report the first error. | **(a)**, because the console output and the error count are observable: the reference prints one line per error (`reference/…/tjsInterCodeGen.cpp:82`) and the raised exception carries the **first** error's message and position. The image must therefore carry the count as well as the first message and position. |
| A5 | **Whether the image's line table replaces the block's or is checked against it.** The reference's `SetText` builds the line table from the text it owns (`reference/…/tjsScriptBlock.cpp:207-232`); the image also carries one. | (a) The C++ keeps its own and checks the image's against it. (b) The C++ adopts the image's. | **(a)**: the block's `Script` buffer is the text `GetLine` returns (`reference/…/tjsScriptBlock.cpp:140-152`), and the reference's line table is built from **that** buffer, so the two must be the same text. Adopting the image's table would mean the block's table no longer matches the block's text. The adapter checks and fails loudly on a mismatch. |
| A6 | **Whether the "expression mode" `;` and the `#!` rewrite are the Rust side's business.** The reference's analyser appends a `;` in expression mode and rewrites a leading `#!` to `//` (`reference/…/tjsLex.cpp:1006-1015`). | (a) They are lexer's business, so the Rust side's. (b) They are the compile entry's business, so the C++ side's. | **(a)**: they are inside `tTJSLexicalAnalyzer`'s constructor, and the Rust lexer already does both (`crates/tjs/src/vm/lexer/mod.rs:416-436`). The C++ side passes the text through unchanged. The consequence to note: in expression mode the source positions refer to the text **plus the appended `;`**, and the block's line table is built from the text **without** it — the reference has the same discrepancy, and it is harmless because the appended `;` is at the end. |
| A7 | **Whether the variable area's names cross the ABI.** The reference throws them away (`Namespace.Clear()` in `Finalize`, `reference/…/tjsInterCodeGen.cpp:399`); only `MaxVariableCount` survives. | (a) The image carries only the count. (b) The image carries the names too, for the debugger. | **(b)**: the existing `tjs2_codegen_context` already carries them, the debugger benefits, and the C++ adapter simply ignores them. The C++ side must not treat the list as authoritative for anything but diagnostics. |

---

## 9. A checklist for the implementer

- [ ] `tjs2_script_compile` returns a handle for **every** input, success or
      failure; the failure carries the message, the position and the count.
- [ ] The image's contexts are in **creation order**; the block's top level is
      named by an index.
- [ ] Every context's code, constants and source positions are **copied** into
      C++-owned storage; the image is freed with `tjs2_script_free`.
- [ ] The context-valued constants and the member registrations are resolved
      **after** all the contexts exist and **after** the relations are linked.
- [ ] The image's source-position tables are sorted; the C++ passes
      `srcsorted = true`.
- [ ] The image's line table agrees with the block's own; a mismatch is a
      hard error, not a silent substitution.
- [ ] The C++ side runs **no** part of §5.1.
- [ ] The bytecode export path (`tjs2_compile_script`) still produces a file
      the loader can read back.

## 10. The gate the switch must pass

The switch replaces the reference's own front end with this one, for every
script the game compiles. A divergence is not a rounding error: the game
reads the compiled scripts, so a difference in the image is a difference in
what the game does.

The gate is therefore **identical images**, not equivalent behaviour:

| what | the measure |
|---|---|
| the comparable scripts | **172 of 172** compile to the same image: the same contexts, the same code words, the same constants, the same variable and argument counts, the same labels, the same source positions |
| the real tree | **every context of every script** matches the reference's field for field: the code, the constants, the variable and argument counts, the frame's size, the labels, the member registrations, the relations, the source positions |
| the constant inline objects | compared on the **materialised** object: the same members, the same order, the same values, and **one shared object** per constant, because the reference builds the object once and every reference to that constant sees that same object |
| the conditional-compilation values | they round-trip between compiles, so both paths process the same text |
| the end-to-end test | green |

Identical images give identical behaviour by construction. A field that
differs is a defect and not a tolerance: it is closed, or the switch does
not land.

The proof instrument that measures this stays in the tree. It is the
regression net for the front end that replaces the vendored one.
