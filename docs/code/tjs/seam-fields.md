# The field-by-field table — the image the C++ builds the script block from

Every row is one field. "Who fills it" names the side that writes the value:
**Rust** = the Rust generator (it lands in the image), **C++** = the C++
adapter (it writes the value into the reference's own object).

The reference citation is the path relative to the repository root and the
line: `reference/krkr2/cpp/core/tjs2/<file>:<line>`.

**Legend for the "type" column.** `tjs_int` is a 32-bit `int`
(`reference/krkr2/cpp/core/tjs2/tjsTypes.h:51`); `tjs_int32` is a 32-bit
integer; `tjs_char` is UTF-16 (`reference/krkr2/cpp/core/tjs2/tjsTypes.h:44`).

---

## 1. The handle — `tjs2_script`

The opaque handle the compile entry returns. It owns the whole image.

| field | type | meaning | who fills it | reference line |
|---|---|---|---|---|
| `status` | `int32_t` | `TJS2_COMPILE_OK` or a negative failure code. The reference's equivalent is the pair (`CompileErrorCount`, `FirstError`) on the block. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:52-87`; `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:55-61` |
| `error_message` | `const char *` | the **first** failure's message; `"Syntax error (%1)"` for a syntax failure. UTF-8, owned by the handle. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:68-73`; `reference/krkr2/cpp/core/tjs2/tjsError.h:288` |
| `error_position` | `int32_t` | the first failure's source position, in code units from the start of the script. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:75-76`; `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:450-453` |
| `error_count` | `int32_t` | how many failures were counted. The reference recovers from a syntax error, counts every one, and raises on the first. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:82`; `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:444-446` |
| `using_preprocessor` | `int32_t` | non-zero when the script used `@if` / `@set`. The C++ copies it into the block's `UsingPreProcessor`. | Rust | `reference/krkr2/cpp/core/tjs2/tjsLex.cpp:1132`, `:1174`; `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:58`, `:107` |
| `image` | `tjs2_codegen_block *` | the block image; null when the compile failed. | Rust | — (new) |

---

## 2. The block image — `tjs2_codegen_block`

| field | type | meaning | who fills it | reference line |
|---|---|---|---|---|
| `contexts` | `const tjs2_codegen_context *` | the contexts of the block, **in creation order**. The order is observable through `GetCodeIndex`, `Dump` and the bytecode writer. | Rust | `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:42`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:257`; `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:509-519` |
| `context_count` | `int32_t` | the number of contexts. The C++ copies them into `InterCodeContextList`. | Rust | `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:42`, `:71-73` |
| `top_level` | `int32_t` | the index of the top-level context; −1 when there is none. The C++ puts it in `TopLevelContext`. | Rust | `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:53`; `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:145-155` |
| `lines` | `const tjs2_codegen_line *` | the block's line table: one entry per line, in order. The C++ copies it into `LineVector` and `LineLengthVector`. | Rust | `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:50-51`; `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:207-232` |
| `line_count` | `int32_t` | the number of lines. | Rust | `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:50-51` |

---

## 3. The context image — `tjs2_codegen_context`

| field | type | meaning | who fills it | reference line |
|---|---|---|---|---|
| `code` | `const int32_t *` | the context's **final** instruction stream: opcode words and their operands, in execution order. The C++ copies it into `CodeArea`. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:394`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:584-628` |
| `code_size` | `int32_t` | the number of words in the code stream. The C++ puts it in `CodeAreaSize`; the executor's IP is an index into it. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:396`, `:536`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:859` |
| `constants` | `const tjs2_codegen_value *` | the context's constant area, in creation order. The C++ copies it into `DataArea` as `tTJSVariant`s. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:401`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:638-681` |
| `constant_count` | `int32_t` | the number of constants. The C++ puts it in `DataAreaSize`. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:402`, `:538`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:865` |
| `variable_count` | `int32_t` | the number of name-space entries the context ever had, **including the declared arguments**. The C++ puts it in `MaxVariableCount`; the executor sizes the register frame with it. The image carries no per-variable records: the reference's names are compile-time state and the runtime never sees them. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:438`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:994`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:641`, `:648`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:399` |
| `variable_reserve_count` | `int32_t` | the number of registers the context reserves below `ra[0]` for itself: **2** for every context type but `ctProperty`, which reserves **0**. The C++ puts it in `VariableReserveCount`. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:433`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:227-254`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:641` |
| `max_frame_count` | `int32_t` | the greatest number of frame registers the context needs. The C++ puts it in `MaxFrameCount`. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:437`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:2822-2823`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:641` |
| `func_arg_count` | `int32_t` | the number of declared arguments. The C++ puts it in `FuncDeclArgCount`; the executor uses it to lay the arguments out at `ra[-3]`, `ra[-4]`, … The image carries no argument records: the reference keeps the argument names in the name space, which is cleared before the runtime sees the context, and the *call-site* description is in the code stream. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:440`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3736`, `:3761`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:704-728`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:399` |
| `func_unnamed_arg_base` | `int32_t` | the base of the unnamed `*` argument, i.e. the index of the first argument the `*` swallows. The C++ puts it in `FuncDeclUnnamedArgArrayBase`. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:441`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3748`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2023`, `:2062` |
| `func_collapse_base` | `int32_t` | the base of the **named** `*` argument; **−1** when the context declares none. The C++ puts it in `FuncDeclCollapseBase`; the executor tests `>= 0` before using it. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:442`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:199`, `:3751`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:731-741` |
| `labels` | `const tjs2_codegen_source_pos *` | the context's per-instruction source positions: `(code position, source position)`, **sorted by code position**. The C++ copies it into `SourcePosArray`. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:274-279`, `:427`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:602-627`, `:693-700` |
| `label_count` | `int32_t` | the number of source positions. The C++ puts it in `SourcePosArraySize`. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:428`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:1041-1063` |
| `kind` | `int32_t` | the context's type: one of `ctTopLevel`, `ctFunction`, `ctExprFunction`, `ctProperty`, `ctPropertySetter`, `ctPropertyGetter`, `ctClass`, `ctSuperClassGetter`. The C++ puts it in `ContextType`; it decides how the context is entered and what `VariableReserveCount` and `MaxVariableCount` mean. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:166-175`, `:449`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2781-2806` |
| `name` | `const uint16_t *` | the context's name, in UTF-16; null when it has none. The C++ copies it into the context's `Name`. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:446`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:215-219`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:452` |
| `name_length` | `int32_t` | the name's length in code units. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:446` |
| `parent` | `int32_t` | the index of the parent context; −1 for none. The C++ links it with `SetCodeObject`. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:447`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:176` |
| `prop_setter` | `int32_t` | the index of the property's setter context; −1 for none. The C++ links it with `SetCodeObject`. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:450`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:887-891` |
| `prop_getter` | `int32_t` | the index of the property's getter context; −1 for none. The C++ links it with `SetCodeObject`. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:451`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:892-896` |
| `super_class_getter` | `int32_t` | the index of the super-class proxy context; −1 for none. The C++ links it with `SetCodeObject`. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:452`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3626-3632` |
| `super_class_getter_pointer` | `const int32_t *` | the code addresses **inside the super-class-getter context's own code** at which the super class is evaluated, one per `extends` clause. The C++ copies it into the context's `SuperClassGetterPointer`; the executor runs the proxy from each of them. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:444`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3638`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2755-2762` |
| `super_class_getter_pointer_count` | `int32_t` | the number of those addresses. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:444`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:4088-4093` |
| `properties` | `const tjs2_codegen_property *` | the members this context contributes to its **parent** object: a name and the index of the context the member holds. The C++ replays the list with `PropSet(TJS_MEMBERENSURE \| TJS_IGNOREPROP, …)` on the parent. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:380-390`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:933-947`; `reference/krkr2/cpp/core/tjs2/tjsByteCodeLoader.cpp:351-365` |
| `property_count` | `int32_t` | the number of member registrations. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:390`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:4094-4101` |

---

## 4. The constant image — `tjs2_codegen_value`

One entry of a context's constant area. The `kind` says which payload field
holds the value.

| field | type | meaning | who fills it | reference line |
|---|---|---|---|---|
| `kind` | `int32_t` | the kind of the value: `TJS2_CG_VALUE_VOID`, `_INTEGER`, `_REAL`, `_STRING`, `_OCTET`, `_NULL`, `_CONTEXT`. | Rust | `reference/krkr2/cpp/core/tjs2/tjsVariant.h:103-110` |
| `integer` | `int64_t` | the value, when the kind is `_INTEGER`. `true` / `false` are the integers 1 / 0. | Rust | `reference/krkr2/cpp/core/tjs2/tjsVariant.h:617-632`; `reference/krkr2/cpp/core/tjs2/tjsLex.cpp:1550`, `:1556` |
| `real` | `double` | the value, when the kind is `_REAL`. | Rust | `reference/krkr2/cpp/core/tjs2/tjsVariant.h:627-632`; `reference/krkr2/cpp/core/tjs2/tjsLex.cpp:1558-1571` |
| `string` | `const uint16_t *` | the value, when the kind is `_STRING`: UTF-16 code units. The C++ builds a `tTJSVariantString` from it. | Rust | `reference/krkr2/cpp/core/tjs2/tjsLex.cpp:1544`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:672-678` |
| `string_length` | `int32_t` | the string's length in code units. | Rust | `reference/krkr2/cpp/core/tjs2/tjsLex.cpp:1544` |
| `octet` | `const uint8_t *` | the value, when the kind is `_OCTET`. The C++ builds a `tTJSVariantOctet` from it. | Rust | `reference/krkr2/cpp/core/tjs2/tjsLex.cpp:1243-1272` |
| `octet_length` | `int32_t` | the octet's length in bytes. | Rust | `reference/krkr2/cpp/core/tjs2/tjsLex.cpp:1243-1272` |
| `object` | `uint64_t` | the value, when the kind is `_CONTEXT`: the **index of the context within the block**. The C++ resolves it to the `tTJSInterCodeContext *` and stores it in the constant. This is the only kind the Rust side cannot fill by itself. | Rust fills the index; **C++** fills the object | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:911`, `:926`; `reference/krkr2/cpp/core/tjs2/tjsConstArrayData.cpp:139-148`, `:187-190`; `reference/krkr2/cpp/core/tjs2/tjsByteCodeLoader.cpp:367-372` |

---

## 5. The source-position image — `tjs2_codegen_source_pos`

| field | type | meaning | who fills it | reference line |
|---|---|---|---|---|
| `code_pos` | `int32_t` | the word index in the context's code area that the source position applies to. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:275`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:625-626` |
| `source_pos` | `int32_t` | the position in the script, in code units from the start. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:276`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:627` |

---

## 6. The line image — `tjs2_codegen_line`

| field | type | meaning | who fills it | reference line |
|---|---|---|---|---|
| `position` | `int32_t` | the offset of the line's first character in the script. The C++ puts it in `LineVector`. | Rust | `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:50`; `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:213-227` |
| `length` | `int32_t` | the line's length in code units, **excluding** the line break. The C++ puts it in `LineLengthVector`. | Rust | `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:51`; `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:214-226` |

---

## 7. The member-registration image — `tjs2_codegen_property`

| field | type | meaning | who fills it | reference line |
|---|---|---|---|---|
| `name` | `const uint16_t *` | the member's name, in UTF-16. The C++ passes it to `PropSet`. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:381`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:938` |
| `name_length` | `int32_t` | the name's length in code units. | Rust | as above |
| `member` | `int32_t` | the index of the context the member holds. The C++ resolves it to the `tTJSInterCodeContext *`. | Rust fills the index; **C++** fills the object | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:382`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:938`, `:941-947` |

---

## 8. Where each image field lands in the reference's own objects

The right-hand column is the reference's field; the C++ adapter writes it.

### 8.1 `tTJSScriptBlock`

| the reference's field | type | meaning | who fills it | reference line |
|---|---|---|---|---|
| `Owner` | `tTJS *` | the engine the block belongs to. | C++ (`new tTJSScriptBlock(tjs)`) | `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:36`; `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:21-35` |
| `RefCount` | `tjs_int` | the block's reference count. One reference per context other than the top level. | C++ (`SetObjects` AddRefs) | `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:37`; `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:145-155`; released at `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:396-397` |
| `Script` | `tjs_char *` | the block's own copy of the script text. | C++ (`SetText`) | `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:38`; `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:207-209` |
| `Name` | `tjs_char *` | the block's name, for diagnostics. | C++ (`SetName`) | `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:39`; `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:95-105` |
| `LineOffset` | `tjs_int` | the line-number offset of the block. | C++ (`SetName`) | `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:40`; `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:99` |
| `InterCodeContextList` | `std::list<tTJSInterCodeContext *>` | every context of the block, in creation order. | C++ (`SetObjects`) | `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:42`; `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:106-108` |
| `ContextStack` | `std::stack<tTJSInterCodeContext *>` | the open contexts during the parse. Empty afterwards. | C++ | `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:44` |
| `LexicalAnalyzer` | `tTJSLexicalAnalyzer *` | the analyser, alive only during the parse. Null afterwards — and null for good once the Rust entry owns the front end. | C++ (null) | `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:46`; `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:430-442` |
| `InterCodeContext` | `tTJSInterCodeContext *` | the context being generated. Null after the parse. | C++ (null) | `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:48` |
| `LineVector` | `std::vector<tjs_int>` | each line's start offset. | C++ (from the image's `lines`) | `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:50`; `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:213-227` |
| `LineLengthVector` | `std::vector<tjs_int>` | each line's length. | C++ (from the image's `lines`) | `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:51`; `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:214-226` |
| `TopLevelContext` | `tTJSInterCodeContext *` | the context that is the script's top level. | C++ (from the image's `top_level`) | `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:53`; `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:147` |
| `FirstError` | `tTJSString` | the first compile error's message. | C++ (from the handle's `error_message`) | `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:55`; `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:450-453` |
| `FirstErrorPos` | `tjs_int` | the first compile error's source position. | C++ (from the handle's `error_position`) | `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:56` |
| `UsingPreProcessor` | `bool` | whether the script used `@if` / `@set`. | C++ (from the handle's `using_preprocessor`) | `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:58`, `:107`; `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:79-82` |
| `CompileErrorCount` | `tjs_int` | the number of compile errors. | C++ (from the handle's `error_count`) | `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:61`; `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:444-446` |

### 8.2 `tTJSInterCodeContext` — the fields the C++ writes

| the reference's field | type | meaning | who fills it | reference line |
|---|---|---|---|---|
| `CodeArea` | `tjs_int32 *` | the instruction stream. | **C++** (a `TJS_malloc`'d copy of the image's `code`) | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:394`; freed at `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:381-382` |
| `CodeAreaCapa` | `tjs_int` | the allocated capacity. | C++ (= `code_size`) | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:395`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:307-309` |
| `CodeAreaSize` | `tjs_int` | the number of words in use. | C++ (= `code_size`) | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:396`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:307-309` |
| `DataArea` | `tTJSVariant *` | the constant area. | **C++** (a `new[]`'d array of `tTJSVariant`, one per image constant) | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:401`; freed at `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:389-392` |
| `DataAreaSize` | `tjs_int` | the number of constants. | C++ (= `constant_count`) | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:402`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:307-309` |
| `SourcePosArray` | `tSourcePos *` | the per-instruction source positions, sorted by code position. | **C++** (a `TJS_malloc`'d copy of the image's `labels`) | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:427`; freed at `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:403-404` |
| `SourcePosArraySize` | `tjs_int` | the number of source positions. | C++ (= `label_count`) | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:428`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:338-340` |
| `SourcePosArraySorted` | `bool` | whether the table is sorted. | C++ (**true**) | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:425`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:337` |
| `VariableReserveCount` | `tjs_int` | the registers the context reserves below `ra[0]`. | C++ (= `variable_reserve_count`) | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:433`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:331` |
| `MaxFrameCount` | `tjs_int` | the frame size. | C++ (= `max_frame_count`) | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:437`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:327` |
| `MaxVariableCount` | `tjs_int` | the number of local-variable slots. | C++ (= `variable_count`) | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:438`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:328` |
| `FuncDeclArgCount` | `tjs_int` | the number of declared arguments. | C++ (= `func_arg_count`) | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:440`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:331` |
| `FuncDeclUnnamedArgArrayBase` | `tjs_int` | the base of the unnamed `*` argument. | C++ (= `func_unnamed_arg_base`) | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:441`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:332` |
| `FuncDeclCollapseBase` | `tjs_int` | the base of the named `*` argument; −1 for none. | C++ (= `func_collapse_base`) | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:442`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:333` |
| `SuperClassGetterPointer` | `std::vector<tjs_int>` | the super class's evaluation addresses. | C++ (a copy of the image's `super_class_getter_pointer`) | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:444`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:317-322` |
| `Name` | `tjs_char *` | the context's name. | C++ (a copy of the image's `name`) | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:446`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:347-352`; freed at `:367-369` |
| `ContextType` | `tTJSContextType` | the context's type. | C++ (= `kind`) | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:449`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:356` |
| `Block` | `tTJSScriptBlock *` | the block the context belongs to. | C++ | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:448`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:357` |
| `Parent` | `tTJSInterCodeContext *` | the parent context. | C++ (via `SetCodeObject`) | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:447`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:835` |
| `PropSetter` | `tTJSInterCodeContext *` | the property's setter. | C++ (via `SetCodeObject`) | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:450`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:836-838` |
| `PropGetter` | `tTJSInterCodeContext *` | the property's getter. | C++ (via `SetCodeObject`) | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:451`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:839-841` |
| `SuperClassGetter` | `tTJSInterCodeContext *` | the super-class proxy. | C++ (via `SetCodeObject`) | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:452`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:842` |
| `Properties` | `std::vector<tProperty *> *` | the members the context contributes to its parent. | C++ (a copy of the image's `properties`) — **only** when a bytecode export is being written; otherwise the C++ performs the `PropSet` directly. | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:390`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:935-939` |

### 8.3 `tTJSInterCodeContext` — the fields that do **not** cross

They are the Rust generator's private state; the reference keeps them only for
the duration of the compile.

| the reference's field | type | meaning | who fills it | reference line |
|---|---|---|---|---|
| `FrameBase` | `tjs_int` | the frame's base register. Always 1. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:392`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:191`, `:325` |
| `Namespace` | `tTJSLocalNamespace` | the local name space: the names and their registers. Cleared when the context is finalised. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:404`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:399` |
| `NodeToDeleteVector` | `std::vector<tTJSExprNode *>` | the expression nodes to delete at the end of the compile. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:406`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:997-999` |
| `CurrentNodeVector` | `std::vector<tTJSExprNode *>` | the expression-node stack. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:408` |
| `FuncArgStack` | `std::stack<tFuncArg>` | the call site's argument description under construction. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:410`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:2737-2768` |
| `ArrayArgStack` | `std::stack<tArrayArg>` | the inline-array construction state. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:412` |
| `PrevIfExitPatch` | `tjs_int` | the previous `if`'s exit patch. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:414` |
| `NestVector` | `std::vector<tNestData>` | the open blocks, loops, switches, tries and withs. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:415`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:295-310` |
| `JumpList` | `std::list<tjs_int>` | the addresses of the jump instructions, for the fix-up. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:417`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:865-870` |
| `FixList` | `std::list<tFixData>` | the pending code insertions, for the fix-up. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:418`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:777-826` |
| `NonLocalFunctionDeclVector` | `std::vector<tNonLocalFunctionDecl>` | the functions declared outside the context, for the registration prologue. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:420`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:719-772` |
| `FunctionRegisterCodePoint` | `tjs_int` | where the registration prologue is inserted. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:422`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:707-717` |
| `PrevSourcePos` | `tjs_int` | the previous instruction's source position, for the dedup of the source-position table. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:424`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:603-604` |
| `SuperClassExpr` | `tTJSExprNode *` | the super class's expression. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:431`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:997` |
| `AsGlobalContextMode` | `bool` | whether the context runs as a global. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:435` |
| `_DataArea` | `tTJSVariant **` | the build-time constant area (pointers to the individual constants). | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:398`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:983-988` |
| `_DataAreaSize` | `tjs_int` | the build-time constant count. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:399` |
| `_DataAreaCapa` | `tjs_int` | the build-time constant capacity. | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:400` |

---

## 9. The operand encodings the C++ side must not re-encode

There is no field for these — they are *inside* the code stream — but they are
part of the contract, so they are tabulated here.

| what | encoding | who fills it | reference line |
|---|---|---|---|
| a register operand | `register_index * sizeof(tTJSVariant)` = `register_index * 20` | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:35`; `sizeof(tTJSVariant)` from `reference/krkr2/cpp/core/tjs2/tjsVariant.h:121-154` |
| a constant-area operand | `constant_index * sizeof(tTJSVariant)` = `constant_index * 20` | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:35`; read at `reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1409` |
| a jump operand | `(target_word - jump_word) * 4`, a byte offset from the jump's own word | Rust (the conversion is the last step of the fix-up) | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:34`, `:38`; `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:865-870`; read at `reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:952-968` |
| a `VM_ENTRY` operand | the same byte offset (the catch address) | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3511-3514`; read at `reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1253-1261` |
| an immediate operand | the raw value (an argument count, an argument kind, −1 / −2) | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3683-3706` |
| the opcode | the `tTJSVMCodes` number | Rust | `reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.h:44-132` |
