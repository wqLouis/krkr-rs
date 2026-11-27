# `tjs::Tjs2Engine` — the engine handle, and the order that removes it

Source: `crates/tjs/src/lib.rs`, `crates/tjs/cpp/` · crate `tjs` · the L4 anchor

`Tjs2Engine` is a thin handle over one vendored C++ `TJS::tTJS`: `inner` is a
`*mut Engine` (`crates/tjs/src/lib.rs:971`), `new` calls `tjs2_create`
(`:1392`), and its 64 public methods sit on a 74-entry C ABI
(`crates/tjs/cpp/tjs2_abi.h`; `crates/tjs/cpp/tjs2_abi.cpp`, 4 237 lines). Every
Rust caller reaches the VM through it, so the C++ cannot be deleted while the
handle is a C pointer.

The runtime that replaces what the handle stands on is already in the tree: the
front end (`crates/tjs/src/vm/{lexer,parser,codegen}`), the executor
(`crates/tjs/src/vm/runtime/exec/`), the value and object model
(`crates/tjs/src/vm/runtime/{variant,object,member,array,dictionary,…}.rs`), the
builtin natives (`crates/tjs/src/vm/runtime/natives/`) and the script entry
(`crates/tjs/src/vm/runtime/engine.rs`). This page is the record between them:
what each handle method stands on today, which of it the Rust side already
answers, and the order that makes the handle a Rust object.

The layers and the columns (C1–C6) are
[`../../arch/l4.md`](../../arch/l4.md) §8; the per-part pages are
[`runtime.md`](runtime.md) and [`interpreter.md`](interpreter.md). This page is
the handle those parts meet at, and the reason each part still has a C++ census.

---

## 1. The shape of the handle

| part | where | what it is |
|---|---|---|
| `inner` | `lib.rs:971` | the C++ engine pointer |
| `retained` | `lib.rs:971` | the ids this handle releases at drop |
| `evaluations` | `lib.rs:971` | the count `script_evaluation_count` reports |
| `life` | `lib.rs:971` | `Rc<EngineLife>`, the ownership `share` splits |

`raw()` (`lib.rs:1358`) publishes `inner` to `compat`, which keeps the one
process-wide engine→context table (`crates/compat/src/binding.rs:79`, `:92`)
because the ABI has no user-data slot. That table is how a native callback that
receives only the raw pointer finds the Rust engine; it is also why the compat
state cannot be dropped while the engine lives.

Three pieces of state sit inside the C++ and outside the struct: the spdlog
bridge, a process-wide atomic naming the engine that receives the VM's records
(`cpp/tjs2_abi.cpp:135`, `:172`); the message map, a static table
(`cpp/tjs2/tjsMessage.cpp:22`); and the stream factories, a data directory plus
`std::fopen` (`cpp/streams.cpp:75`, `:319`).

---

## 2. The inventory

Column 2 decides the order. A count is production call sites under
`crates/{compat,vm,app}/src` — tests are the proof (§3), not the dependency. The
counts move as the migration lands; the ratio they show does not.

### 2.1 Lifecycle, execution and services

| item | who calls it | what stands behind it |
|---|---|---|
| `new` | `vm` 1 (`crates/vm/src/host.rs:376`) | `tjs2_create` (`abi.cpp:1875`): a `TJS::tTJS`, the spdlog loggers, the stream factories, a console adapter |
| `share` | `compat` 1 (`natives/scripts.rs:628`) | a second handle over the same `inner` (`Rc<EngineLife>`) |
| `raw` | `compat` 3 (`binding.rs:92`, `natives/scripts.rs:789`, `native_util.rs:303`) | the pointer itself |
| `set_log_cb` | `vm` 2 (`host.rs:380`, `:545`) | the engine's callback fields plus `g_log_engine` (`abi.cpp:1931`) |
| `set_data_dir` | `compat` 1 (`host.rs:258`) | `g_data_dir` in `streams.cpp:77` |
| `exec_script` | `compat` 9 src, and `ScriptScope` is the path that already replaces them (`compat/src/script.rs:102`) | `tjs2_exec_script` (`abi.cpp:2001`) → `tTJS::ExecScript` → the C++ script cache → `tTJSScriptBlock::SetText`/`Parse` (which compiles through the Rust front end, `cpp/tjs2/tjsScriptBlock.cpp:824`) → the C++ executor |
| `exec_script_discard` | `compat` 0 src (tests only) | the same entry with a null result slot, then a variant-stack compact (`abi.cpp:1972`) |
| `eval` | `compat` 2 (`plugins/motion_player/mod.rs:258`, `:272`); the other `eval`-shaped sites call `ScriptScope::eval` (`compat/src/script.rs:124`) | `tjs2_eval` (`abi.cpp:2086`) → `tTJS::Eval` |
| `eval_retained` | `compat` 24 src (`natives/kag/mod.rs:536`, `plugins/fstat/mod.rs:283`, `plugins/layer_ex/draw.rs:2221`) | `tjs2_eval`, then the result retained out of `last_object` (`abi.cpp:86`) |
| `eval_retained_with_context` | `compat` 1 (`natives/kag/mod.rs:225`) | `tjs2_eval` with `this` |
| `eval_retained_with_context_result` | `compat` 1 | the same, with the result flag |
| `eval_retained_cached` | tests only (3) | `tjs2_eval` with a null name, i.e. the C++ script cache |
| `exec_script_retained_with_context` | tests only | `tjs2_exec_script` + a retention |
| `exec_script_retained_wide_with_context`, `eval_retained_wide_with_context` | none | `tjs2_exec_script_wide` / `tjs2_eval_wide` (`abi.cpp:2014`, `:2099`) |
| `compile_script` | `compat` 1 (`natives/scripts.rs:1087`, `Scripts.compileStorage`) | `tjs2_compile_script` (`abi.cpp:2193`) → `tTJS::CompileScript` (`tjs2/tjs.cpp:533`) → `tTJSScriptBlock::Compile`/`ExportByteCode` (`tjs2/tjsScriptBlock.cpp:1030`, `:922`), writing through a storage stream |
| `dump` | `compat` 1 (`natives/scripts.rs:1233`, `Scripts.dump`) | `tjs2_dump` (`abi.cpp:2255`) → `tTJS::Dump` → `tjs2/tjsDisassemble.cpp` |
| `do_rehash` | `compat` 2 (`natives/system.rs:303`, `plugins/scripts_ex/mod.rs:1379`) | `tjs2_do_rehash` (`abi.cpp:2289`) → the rebuild magic every object checks on its next property access |
| `do_gc` | `compat` 2 (`natives/system.rs:382`, `:1459`) | `tjs2_do_gc` (`abi.cpp:2295`): `DoGarbageCollection` **plus** a variant-stack compact |
| `assign_message` | `compat` 1 (`natives/system.rs:1171`) | `tjs2_assign_message_checked` (`abi.cpp:2112`) → `TJSAssignMessage` over the holders `tjs2/tjsError.h:238-243` register at static-init |
| `tjs_version_string`, `compile_date_time` | `compat` 1 each (`natives/system.rs:2088-2089`) | `tjs2_tjs_version_string`, `tjs2_compile_date`, `tjs2_compile_time` (`abi.cpp:2180-2191`): `TJS_VERSION`, `__DATE__`, `__TIME__` of the C++ translation unit |
| `result_needed` | `compat` 2 (`natives/scripts.rs:1030`, `plugins/window_ex/scripts.rs:76`) | `tjs2_result_needed` (`abi.cpp:1665`), the `result_needed` field the dispatcher saves and restores |
| `retained_count`, `script_evaluation_count` | tests only | the C `retained` map's size (`abi.cpp:3199`) / the handle's own counter |

The execution methods as a group are **≈48** production call sites. Their
callers are compat's bootstrap and helper scripts — the TVP constant table
(`natives/constants.rs:29`), a plugin's installed serializer
(`plugins/save_struct/mod.rs:534`), a value only a script can make
(`visual/layer/util.rs:454`) — not a frame loop: `crates/vm` drives
`runtime/engine.rs`, and the game's own scenario path already does
(`natives/scripts.rs:620`).

### 2.2 The object and retained-value surface

| item | who calls it | what stands behind it |
|---|---|---|
| `register_native_class` | `compat` 1 (`ctx.rs:374`) | `tjs2_register_native_class_ex` (`abi.cpp:2615`) builds `tjs2_native_method_dispatch` / `…property_dispatch` objects (`:451`, `:792`) over the fn tables and sets them on the global's class object |
| `register_native_class_instance` | `compat` 1 (`ctx.rs:412`) | `tjs2_register_native_class_instance` (`abi.cpp:2714`): a `tTJSNativeInstance` class with `tjs2_native_instance` payloads, recorded in `native_instance_class_ids` (`abi.cpp:74`) |
| `register_native_static_members` | `compat` 1 (`ctx.rs:428`) | `tjs2_register_native_static_members` (`abi.cpp:2824`) |
| `register_native_instance_members` | `compat` 1 (`ctx.rs:450`) | `tjs2_register_native_instance_members` (`abi.cpp:2876`): objthis-only dispatch objects (`:926`, `:1005`) |
| `native_instance_payload` | `compat` 21 src (`natives/kag/mod.rs:992`, `menu_item.rs:612`, `plugins/layer_ex/*`, `motion_player/adaptors.rs:854`, …) | `tjs2_get_native_instance` (`abi.cpp:2343`): `NativeInstanceSupport(TJS_NIS_GETINSTANCE)` + `GetNativePtrForLookup` |
| `retain_object_arg` | `compat` 158 src | `tjs2_retain_value` (`abi.cpp:3049`) over an OBJECT argument, resolved against `last_object` (`abi.cpp:86`) |
| `retain_object_detached` | `compat` 32 src | `tjs2_retain_object` / `tjs2_retain_retained_id` (`abi.cpp:3143`, `:3182`) |
| `retain_value_detached` | `compat` 19 src | `tjs2_retain_value`: a `tTJSVariant` copy in `retained` (`abi.cpp:117`) |
| `duplicate_retained` | `compat` 42 src | `tjs2_retain_retained_id` (`abi.cpp:3182`) |
| `retain_null` | `compat` 2 (`natives/system.rs:2943`, `visual/layer/focus.rs:263`) | `tjs2_retain_value` on the ABI's handle-less OBJECT form |
| `global_object` | `compat` 6 (`plugins/json/mod.rs:1109`, `layer_ex/gdiplus.rs:287`, `motion_player/mod.rs:367`, `save_struct/mod.rs:565`, `save_struct/scripts_member.rs:900`, `var_file/mod.rs:231`) | `tjs2_get_global` (`abi.cpp:3164`) → `tTJS::GetGlobal` |
| `invalidate_retained` | `compat` 6 (`menu_item.rs:534`, `sound.rs:1188`, `:1542`, `visual/window.rs:196`, `:285`, `:2330`) | `tjs2_invalidate_retained` + `tjs2_invalidate_error_message` (`abi.cpp:3224`, `:3265`), which runs the script `finalize` |
| `call_member` | `compat` 43 src | `tjs2_call_member` (`abi.cpp:3486`): member lookup through the object's class chain, then `FuncCall` |
| `call_detached` | `compat` 12 src | `tjs2_call_value` (`abi.cpp:3369`) |
| `call_detached_with_objthis` | `compat` 1 (`plugins/scripts_ex/mod.rs:698`) | `tjs2_call_value_with_objthis` (`abi.cpp:3376`) |
| `set_member` | `compat` 132 src | `tjs2_prop_set` (`abi.cpp:3687`) |
| `member` | `compat` 100 src | `tjs2_prop_get` (`abi.cpp:3622`) |
| `member_read` | `compat` 5 src | `tjs2_prop_get` through `member_read_exact` (`lib.rs:3087`), which is where the ABI's absent/void/null distinction is recovered |
| `delete_member` | `compat` 7 src | `tjs2_prop_delete` (`abi.cpp:3902`) |
| `enum_members` | `compat` 4 src (`natives/kag/mod.rs:470`, `:605`, `:1299`, `:1311`) | `tjs2_enum_members` (`abi.cpp:2493`), which builds a retained TJS Array of names |
| `class_names` | `compat` 5 src | `tjs2_get_class_names` / `tjs2_add_class_name` (`abi.cpp:2375`, `:2447`), a retained TJS Array |
| `new_array`, `new_dictionary` | `compat` 5 + 1 src (`natives/construct.rs:27`, `:37`, …) | `tjs2_new_array` / `tjs2_new_dictionary` (`abi.cpp:3751`, `:3790`): the VM's own `Array`/`Dictionary` classes |
| `array_add`, `array_add_octet` | `compat` 8 + 1 src | `tjs2_array_add` (`abi.cpp:3831`) |
| `set_member_octet` | `compat` 1 (`plugins/psb_file/mod.rs:206`) | `tjs2_prop_set` with `VAL_OCTET` |
| `member_ignore_prop` | `compat` 1 (`plugins/scripts_ex/mod.rs:427`) | `tjs2_prop_get` with `TJS_IGNOREPROP` |
| `set_call_missing` | `compat` 1 (`natives/scripts.rs:1140`) | `tjs2_set_call_missing` (`abi.cpp:2563`) |
| `set_arg` | `compat` 1 (`visual/window.rs:2145`) | `tjs2_set_arg` (`abi.cpp:4188`), which writes the caller's own argument slot from the `param_stack` frame (`abi.cpp:97`) |
| `create_new`, `call_value`, `retain_value`, `release_value`, `is_instance_of`, `is_valid`, `operation`, `set_default_member`, `set_member_with_flags` | none outside `tjs` (tests only) | `tjs2_create_new`, `tjs2_call_value`, `tjs2_retain_value`, `tjs2_release_value`, `tjs2_is_instance_of`, `tjs2_is_valid`, `tjs2_operation`, `tjs2_prop_set_default` (`abi.cpp:3597`, `:3369`, `:3049`, `:3208`, `:4094`, `:4145`, `:4014`, `:3951`) |

The object and retained-value methods are **≈535** production call sites. That
is more than ten times the execution group, and it is the whole ordering
argument: the entry is a small repoint, the objects are the wall.

### 2.3 The free surface

| item | who calls it | what stands behind it |
|---|---|---|
| `set_storage_read_callback` | `compat` 1 (`binding.rs:101`) | the process-wide read hook the text/binary stream factories call |
| `set_text_decode_callback` | `compat` 1 (`binding.rs:105`) | the process-wide charset probe for a BOM-less payload |
| `tjs2_get_stack_trace_string` | `compat` 1 (`natives/scripts.rs:1210`, `Scripts.getTraceString`) | the C++ script block's call stack |
| `tjs2_free_string`, `tjs2_malloc` | `compat` 6 (`native_util.rs`, `natives/*`) | the C allocator |
| `tjs2_retained_count` | tests only | the C map |
| `retain_object_arg_raw` (`lib.rs:3688`) | none | `tjs2_retain_value` for a raw callback |
| the fn-pointer types (`StorageReadFn`, `TextDecodeFn`, `LogCb`, `Native*Fn`, `lib.rs:128-348`) | the native builders | the C dispatch trampolines |
| the builders (`NativeClassBuilder`, `NativeInstanceBuilder`, `NativeStaticMembers`, `NativeInstanceMembers`, `lib.rs:1246-1335`) | `compat` once each (`ctx.rs:374-450`) | the four `tjs2_register_*` entries |
| `Value`, `TjsValue`, `RetainedValue`, `MemberRead`, `ValueId`, `DetachedValue`, `Tjs2ValueId`, `CompileOptions`, `TjsError` (`lib.rs:357-1187`) | `compat`, `vm`, `app` | the `tjs2_value` marshalling and, in `Tjs2ValueId`, the C retained id |

---

## 3. What the Rust side already answers, and where nothing does

| item | Rust side today | proof | gap |
|---|---|---|---|
| compile | `vm::compile`, `vm::compile_expression_result` | `crates/tjs/tests/vm_pipeline.rs`, `tests/fixtures/corpus_digest.txt` (297 rows) | — |
| execute | `runtime::engine::{run,eval,run_with_members_options,eval_with_members_options}` (`runtime/engine.rs:288`, `:302`, `:317`, and `:389` for the block runner) over `Block::build` (`runtime/exec/build.rs`) and `Machine::with_global` (`runtime/exec/mod.rs:322`) | `crates/tjs/tests/rust_engine.rs:276` replays `value_layer.txt` + `object_members.txt` (89 probes) through the Rust executor against the engine's answers | the entry is reached by `crates/vm` (`host.rs:428`, `:448`), by `compat`'s `ScriptScope` (`compat/src/script.rs:102`, `:124`) and by `Scripts.execStorage` (`natives/scripts.rs:620`), not by `Tjs2Engine` |
| value | `runtime/variant.rs::Variant` | `tests/fixtures/value_layer.txt` (69 rows) replayed by `tests/value_layer.rs` | — |
| object | `runtime/object.rs::Dispatch` (`:232`), `CustomObject` | `tests/fixtures/object_members.txt` (19 rows) replayed by `tests/object_members.rs` and by `rust_engine.rs` | `new Array()`/`new Dictionary()` still resolve to the **engine's** class, so a constructed container is a C object; a Rust container is what a literal and a Rust native make (`exec/build.rs:622`) |
| containers | `runtime/array.rs`, `runtime/dictionary.rs`, `runtime/octet_pack.rs`, `runtime/stream.rs` | `array_members.txt` (166), `dictionary_members.txt` (30), `octet_pack.txt` (293), `binary_form.txt` (27), `stream_members.txt` (37) | `Array` serves its 23 member names through `Dispatch` (`array.rs`), and `Dictionary` its 7 static rules; the class objects are still the engine's, so `new Array()` constructs a C container. The stream members (`save`, `load`, `saveStruct`, `loadStruct`, `assignStruct`) now have rules over `StreamHost`, and `stream_members.txt` records the two rules where the model still differs: the reference answers the receiver and clears before it validates |
| member map | `runtime/member.rs::MemberMap` | `tests/fixtures/member_order.txt`; `tests/rehash_growth.rs` | — |
| builtin classes | `runtime/natives/{math,date,date_parse,exception,random,regexp,string,config}.rs` | `math_members.txt` (140), `date_members.txt` (157), `exception_members.txt` (38), `random_members.txt` (84), `regexp_members.txt` (102), `string_members.txt` (81), `config_members.txt` (7) | the class **objects** exist for four names only (`natives/registry.rs:56`: `Math`, `Exception`, `RegExp`, `Date`); the registry has no registration entry |
| the global object | `runtime/global.rs::GlobalObject` owns the scope's members (`CustomObject`), its class objects (`NativeRegistry`) and the names a script deleted, and asks the engine's global object as one fallback source; `runtime/engine.rs:395` runs a block against the scope's object (`bridge::scope_global`) | `crates/tjs/tests/global_object.rs`; `crates/tjs/tests/engine_core.rs`; `tests/rust_engine.rs` | one resolution order, written in `runtime/global.rs`: own members, then the registry, then the engine's global. `bridge::GlobalMembers` is now only the store of the values an object cannot give the engine (a context's members, an anchor's host), and `bridge::unaddressable` is the one write rule both objects use |
| a Rust-owned object as a script value | `runtime/bridge/mod.rs::retain_host_object` (`:688`) anchors the object on an engine dictionary and mirrors its members; the store keeps the object itself | `crates/tjs/tests/native_classes.rs:39` runs the four Rust classes through the Rust entry | the anchor is an engine object, and the value's identity lives in a store keyed by the anchor's id |
| retained values | `Tjs2Engine::retained` (`lib.rs:971`) is bookkeeping: it remembers ids to release at drop | `tests/retained_teardown.rs`, `tests/invalidate_retained.rs` | the ids, the values and the lifetime are the C++ map's (`abi.cpp:117`); `DetachedValue` holds a C engine pointer (`lib.rs:1038`) |
| native class registration | `natives/registry.rs::NativeRegistry` with a fixed table and a private `Class` trait (`:121`) | `tests/native_classes.rs` | no entry point: a class cannot be registered by name with a fn table, and a class instance has no payload slot |
| native instance payload | — | — | `native_instance_payload` (`lib.rs:3597`) is the only accessor, and it reads the C instance (`abi.cpp:2343`) |
| a class's script-visible member set | the surface dump already walks a mixed object graph (`crates/compat/tests/runtime_surface.rs:113`) | `docs/surface/runtime_surface.json`: 74 classes, 2 543 members | the names come from the engine's class objects |
| message map | `runtime/hash.rs` is the table the message map needs (`hash.rs:17`) | — | the names and texts are `tjsError.h`'s macros; `runtime/exec/throw.rs:251` hardcodes the text a raise uses, so `assignMessage` changes the C++ message and not the Rust one |
| bytecode writer | `vm::codegen::ffi::Image` is the input | — | no serializer; the loader has been deleted, so the writer's oracle is a committed `.tjsb` or the reference |
| script block registry | — | — | `dump` prints the C++ blocks; the Rust entry builds a `Block` per call and keeps none |
| script cache | — | — | `eval_retained_cached` is the only caller of the C++ cache and has no production caller |
| GC / rehash | — | — | there is no cycle collector and no per-object rebuild magic; `MemberMap` rebuilds on growth (`tests/rehash_growth.rs`), so `do_rehash` has nothing to bump and `do_gc` nothing to collect |
| streams (`Array.save/load/saveStruct`) | `runtime/dictionary.rs::save_struct_text` (`:130`) produces the text | `tests/save_struct_stream.rs` drives it through `exec_script` | the file, the `FE FE <mode>` container and the zlib payload are `cpp/streams.cpp` |
| a context receiver | `runtime/engine.rs:445` gives a context handle its own store, keyed by the context's retained id | `rust_engine.rs` (the object table's setup script runs on the engine first) | the receiver is a `CHandle` over a C object |

Two facts hold the table together. First, `crates/vm` already drives the Rust
entry, and so does the game's own scenario path — `Scripts.execStorage` →
`run_text` → `runtime::engine` (`natives/scripts.rs:620`, `:630`). Second,
`compat` already has the helper that takes its remaining scripts off the C++
entry: `ScriptScope` (`crates/compat/src/script.rs:74`) runs the text through
`run_text` and answers a `RetainedValue`, so a member that needs a value only a
script can make does not need `Tjs2Engine::eval` at all.

---

## 4. The steps

Each step is one change, and each ends with a command that fails if the step is
not what it claims. The order is by the C++ that stops being reachable, smallest
first; a file is deleted in the step that takes its last caller, and no step
before the last deletes one of the runtime files.

### Step 1 — a Rust-owned object can be a script value

The bridge can carry an object the Rust object model owns out of a call: the
object is anchored on an engine dictionary and its members are mirrored, while
the store keeps the object itself, so a class, a function or a dictionary keeps
its identity after the call that produced it (`runtime/bridge/mod.rs:693`;
`runtime/engine.rs:231`, `retained_value_with_members`).

This is what lets a member answer a value the Rust executor made, and it is the
precondition for every later step: without it, every answer that crosses back
into a native must be a C object.

```sh
cargo build -p tjs --lib
cargo test -p tjs --test native_classes --test rust_engine
cargo test -p compat --test runtime_surface
uv run --project scripts krkr-gate surface-lock --no-regen   # byte-identical dump
```

### Step 2 — `compat`'s scripts run on the Rust entry

`ScriptScope` (`crates/compat/src/script.rs:74`) is the one path a native uses
to run text: `run` compiles and runs a script, `eval` an expression, and the
answer is a `RetainedValue` in the store the compat script entries share. Every
`exec_script` / `eval` / `eval_retained` call site in `crates/compat/src`
becomes a `ScriptScope` call.

Removes: the last production caller of the C++ executor's entries. The count
that decides is in §2.1 — 9 `exec_script`, 2 `eval` and 24 `eval_retained`; when
it reaches zero, `tjs2_exec_script` and `tjs2_eval` are entered
only from the crate's own public methods.

```sh
cargo test -p compat
cargo test -p compat --test scripts --test kag --test save_struct
uv run --project scripts krkr-gate surface-lock --no-regen
```

### Step 3 — the global scope is a Rust object

`runtime/engine.rs:395` built the machine's global by asking the engine for its
global object and wrapping that in a `CHandle`. The change is the inversion: a
`GlobalObject` implementing `Dispatch` owns the root members and the
`NativeRegistry`, and asks the engine's global as **one source** for a name it
does not hold. The side table becomes the object and the engine's global becomes
the fallback. `CHandle` keeps its store for context handles
(`runtime/engine.rs:447`), which is what a context receiver needs.

**Landed.** `runtime/global.rs` is the one place the order is written: own
members, then the registry's class objects, then the engine's global object; a
deletion tombstones a class name so the registry does not answer it again. The
scope owns one object (`bridge::scope_global`), so a `var` or a `function` a
call declares is there for the next. A write stays on the object only when the
engine cannot address the value (`bridge::unaddressable`, the crossing rule the
bridge already had) or when it shadows a published class name; every other value
the engine holds, because the engine's own code reads its global object
directly. `bridge::CHandle` no longer carries the registry or the global's
store: `bridge::GlobalMembers` is the store of the values an *object* cannot
give the engine, and a handle's root is a weak reference to its scope (the scope
owns the global object, which owns the handle over the engine's global).

What that does **not** yet move is construction: `new Array()` and
`new Dictionary()` still resolve to the engine's class object, because the Rust
`Array` and `Dictionary` class objects publish a member set that is not the one
the engine's class carries. `crates/compat/tests/save_struct.rs:171` runs
`var a = ['a"b\\c']; var s = a.toStructString();` through the Rust entry, and
`save2`/`saveStruct2`/`toStructString` are registered by the `save_struct`
plugin on the **engine's** `Array` class object
(`tjs2_register_native_instance_members`), so a Rust `ArrayObject` would raise
where the plugin answers, and the surface lock would lose the three names. The
class object is the unit (§6, “A class cannot be deleted by porting its rules”):
its table, its construction and its instance members move together, and that is
the class migration, not this step.

```sh
cargo test -p tjs --test rust_engine
cargo test -p tjs --test global_object
cargo test -p compat --test runtime_surface
uv run --project scripts krkr-gate surface-lock --no-regen
cargo test -p compat --test kag
```

### Step 4 — the native classes and their instances register in Rust

`NativeRegistry` gains a registration entry; `CompatCtx::register_class`,
`register_instance`, `register_static_members` and `register_instance_members`
(`crates/compat/src/ctx.rs:374`, `:412`, `:428`, `:450`) build Rust class
objects from the builders instead of calling the C ABI. A class instance becomes
a Rust object with a payload slot, and `native_instance_payload` reads it.

The family is the unit, because each carries its own dispatch tests: `visual`
(`layer_behaviour`, `extend_dispatch`), `kag`, `sound`/`cdda_midi`, `plugins`
(each with its own file), `system`/`storages`/`scripts`. `register_all`
(`crates/compat/src/natives/mod.rs:209`) is the list.

```sh
cargo test -p compat --test layer_behaviour --test extend_dispatch
cargo test -p compat --test runtime_surface
uv run --project scripts krkr-gate surface-lock --no-regen
```

When the last family flips, the four `tjs2_register_native_*` entries and the
`tjs2_dispatch_native_*` / objthis trampolines (`abi.cpp:440-1660`) have no
caller, and `tjs2_get_native_instance` (`:2343`) goes with the payload lookup.
The `native-instance-lookup` gate check guards that seam and is deleted in the
same change (`docs/rules/gates.md` §3).

### Step 5 — retained values are Rust handles

`Tjs2ValueId` becomes a key into an arena the engine owns; `DetachedValue` owns
a slot and releases it on drop; `ValueId` keeps borrowing; `duplicate_retained`,
`invalidate_retained`, `is_valid`, `retained_count`, `class_names` (a Rust
array), `member_read` (`MemberShape`), `set_member`, `call_member`,
`call_detached` and `enum_members` dispatch through `Dispatch` instead of the
`tjs2_prop_*` / `tjs2_call_*` entries.

The C++ this removes: the `retained` map, `last_object`, `resolve_value_variant`
and `variant_to_value` (`abi.cpp:117`, `:86`, `:3049-3200`), and with them
`tjs2_retain_value`, `tjs2_release_value`, `tjs2_retain_retained_id`,
`tjs2_retain_object`, `tjs2_get_global`, `tjs2_invalidate_retained`,
`tjs2_prop_get/set/delete`, `tjs2_call_value*`, `tjs2_get_class_names` and
`tjs2_enum_members`.

```sh
cargo test -p tjs --test retained_teardown --test invalidate_retained
cargo test -p tjs --test member_read --test native_instance_lookup
cargo test -p vm --lib dropping_the_host_releases_native_state_before_the_engine
cargo test -p compat --test extend_dispatch
```

### Step 6 — the execution entries themselves (the point of no return)

`exec_script`, `exec_script_discard`, `eval`, the five retained forms and the two
wide ones call `runtime::engine::run*` / `eval*`. The call's own
frame answers `result_needed` and `set_arg`; `set_call_missing` is the object's
own flag; `share` and `raw` exist only for the binding and the log callback.

```sh
cargo test -p tjs
cargo test -p app --test boot_game            # needs KRKR_TEST_GAME
uv run --project scripts krkr-gate test
```

Prerequisites, each its own change, each landing before this one or accepting a
script-visible regression: the bytecode writer, the save/load stream, the
message table, `dump`, `do_gc` / `do_rehash`, and `compile_date_time` (§6).

### Step 7 — the ABI, the tree and the build chain

`crates/tjs/cpp/` is deleted: `tjs2_abi.cpp` (4 237) and `tjs2_abi.h` (1 025),
`tjs2_codegen.h` (331), `streams.cpp` (1 556), `value_bench.cpp` (379),
`hash_order_test.cpp` (120), and `tjs2/`'s remaining 21 003 lines. `build.rs`
loses its compile step and `Cargo.toml` its `links = "tjs2"`
(`crates/tjs/Cargo.toml:7`); the `#[link(name = "z")]` in `lib.rs:824` goes with
the container it served.

```sh
ls crates/tjs/cpp                    # no such directory
cargo build                          # no C++ compiler invoked
uv run --project scripts krkr-gate gate
uv run --project scripts krkr-gate test
```

---

## 5. The point of no return

**Step 6.** After the handle's own execution methods run the Rust entry,
`tjs2_exec_script` and `tjs2_eval` have no caller at all, and every script value
a game can see is made by the Rust object model. From that point the remaining
C++ is deletion: the compile and execute block, the object model, the builtin
class files, the ABI and the build chain.

Two things keep a C++ entry alive past Step 6 unless they move first, and both
are named as prerequisites because neither is the engine: the bytecode writer
(`tjsScriptBlock::Compile`, `tjsScriptBlock.cpp:1030`) and the stream
implementation (`streams.cpp`). Their callers are `Scripts.compileStorage` and
`Array.save`/`load`, not the frame. Step 2 removes the *script* entry; these two
remove the *service* entries, and they are the only C++ a running game still
touches after Step 6.

---

## 6. The risks, and the ones already on the record

| risk | evidence | what the step must keep |
|---|---|---|
| **The retained-value API is how `compat` holds an object across calls.** Six of its methods alone take ≈265 production call sites (`retain_object_arg` 158, `duplicate_retained` 42, `retain_object_detached` 32, `retain_value_detached` 19, `invalidate_retained` 6, `global_object` 6). A release can run a script `finalize`, which can call a native, which can retain again — so a release mutates the registry it is walking | `abi.cpp:117` (the map), `:1913` and `:1925` (`tjs2_destroy` drains twice for exactly this), `lib.rs:1038` (`DetachedValue` drops through the engine) | release idempotent; an engine outlives every handle; a forgotten id cannot leak or crash at drop; `retained_count` stays a leak diagnostic; teardown runs the finalizers before the dispatch state goes (`crates/vm/src/host.rs:545`) |
| **The class registration is what the surface lock dumps.** 74 classes and 2 543 members are the committed baseline; the dump walks the live global, each class and one constructed instance | `crates/compat/tests/runtime_surface.rs:203`, `:113`; `docs/surface/runtime_surface.json` | the same names, the same members, the same nesting depth and the same constructibility — the dump is the review (`krkr-gate surface-lock`), and any diff is a deliberate baseline change |
| **`Scripts.compileStorage` has no home for the writer.** The loader is gone, so the writer cannot be checked against the C++ reader; `compileStorage` is its only caller | `natives/scripts.rs:1087`; `tjsScriptBlock.cpp:1030`, `:922` | a `.tjsb` committed as the oracle, or a reference recording; `Scripts.compileStorage` must not become a silent no-op |
| **`Array.save`/`load`/`saveStruct` are `std::fopen` behind a process-global data directory.** The stream is the one IO path the storage namespace cannot reach, and the save path itself is a script (`save_struct` installs a serializer through a script entry and keeps its namespace retained on the global) | `cpp/streams.cpp:75`, `:319`; `crates/compat/src/host.rs:246`; `natives/plugins/save_struct/mod.rs:534` | a write seam symmetric with `StorageReadFn`, and the `FE FE <mode>` container plus its zlib payload. `flate2` with `zlib-rs` is already a workspace dependency (`Cargo.toml:59`), so no C zlib has to stay |
| **The log callback's user pointer must outlive every log, including one emitted from a destructor.** The ABI keeps the most recently installed callback in a process-wide atomic and clears it before the engine is freed | `abi.cpp:135`, `:1931`, `:1900`; `crates/vm/src/host.rs:380`, `:545` | a per-engine sink, cleared before the state it points at dies, with the same order the C++ has |
| **`tjsRegExp.cpp` still owns the class registration and `onig_end`.** The Rust `RegExp` is complete and passes its 102 recorded answers over `fancy-regex`, but the class object a script resolves is still the C++ one, and that file is what calls `onig_end` on shutdown | `cpp/tjs2/tjs.cpp:180`, `:214` → `cpp/tjs2/tjsRegExp.cpp:179` | the class registers from the Rust side; then the file, the library link and the `onig_end` question go together |
| **`Date`'s zone offset belongs to a host service.** The class reads `TZ`, `/usr/share/zoneinfo` and `/etc/localtime` from inside the registry | `runtime/natives/registry.rs:1153`; `tests/fixtures/date_members.txt` is pinned to `JST-9` | the offset arrives from the composition root; a platform with neither path must not raise where the reference answers |
| **The message table is two tables today.** `System.assignMessage` rewrites the text of later raises in C++, while the Rust raise paths hardcode theirs | `tjs2/tjsError.h:238-243`, `tjs2/tjsMessage.cpp:116`; `runtime/exec/throw.rs:251`, `runtime/variant.rs:590`; `tests/fixtures/raise_messages.txt` (19 rows) | one table, read by the raise sites; `assignMessage` answers `1` for exactly the registered names |
| **A class cannot be deleted by porting its rules.** `new Array()` resolves to the engine's class object, so the instance is a C object; the Rust `Array` publishes 24 names and serves one call | `runtime/array.rs:103`, `:869`; `runtime/dictionary.rs` (no member table) | the class object is the unit: its statics, its construction, its instance members and the `save`/`load` rules together |
| **`compile_date_time` is `__DATE__`/`__TIME__` of a translation unit that is going away.** `System.getAbout` prints it | `abi.cpp:2189`, `:2191`; `natives/system.rs:2088` | a defined value (the crate's build time or the version alone), stated as the deliberate change it is |
| **Finalizer and GC timing.** `do_gc` collects and compacts a variant stack; `do_rehash` bumps a rebuild magic. Neither has a counterpart, and a native instance's destructor runs at collection time today | `abi.cpp:2295`, `:2289`; `runtime/member.rs` rebuilds on growth (`tests/rehash_growth.rs`) | a destructor's effects must fire at a moment no script can tell apart; `System.doCompact`'s idle interval must stay a call that cannot fail |
| **`unsafe impl Send + Sync for Tjs2Engine`** is unsound and nothing needs it; a Rust engine holding `Rc` makes it a compile error | `lib.rs:1073`; `TODO.md` §12 | removed in the step that makes the internals `Rc`, with the compiler naming any caller |

---

## 7. What keeps a C dependency

**Nothing on the Rust path.** The regular expression engine is `fancy-regex`
— pure Rust, `oniguruma_mode` for the Perl constructs the reference's syntax
enables, and the UTF-16 to UTF-8 index mapping done in
`crates/tjs/src/vm/regexp_engine.rs` — and it reproduces all 102 recorded answers
(`tests/fixtures/regexp_members.txt`), including a capture inside a lookahead and
the astral-plane probes. Two approximations are recorded in `TODO.md` §12.1: a
search that exhausts the engine's step budget raises where the reference searches
until the pattern settles, and only the unmatched-parenthesis refusal keeps the
reference's text.

The C library is still *linked* because `cpp/tjs2/tjsRegExp.cpp` is still
compiled: that file owns the class registration and `onig_end`. It dies when the
class registers from the Rust side, and the library, the `libonig.a` link and
zig's oniguruma target die with it.

**A C++ compiler, and for now zig.** What fetches and stages the header-only
`fmt`/`spdlog`/`boost` for the vendored tree is `zig build` in `deps/`
(`crates/tjs/build.rs:8`; `deps/build.zig:1-20`). Deleting `crates/tjs/cpp/`
removes those three but not Oniguruma: **zig leaves when the Oniguruma fetch and
build move off it**, a small `deps` build of that one library or a vendored
source compiled by `cc`.

**zlib does not stay.** `#[link(name = "z")]` (`lib.rs:824`) exists for the
`FE FE 02` save container in `cpp/streams.cpp`; `flate2` with `zlib-rs` is
already a workspace dependency and is pure Rust (`Cargo.toml:59`).

The other C++ dependencies leave with the tree and need no replacement: `fmt`,
`spdlog` and `boost` are the vendored VM's logging and text-conversion headers,
and the Rust side uses `log` and its own text types.

---

## 8. Related documents

| topic | document |
|---|---|
| the crate, the ABI groups and the value types | [`../tjs.md`](../tjs.md) |
| the L4 layer, the columns and the per-column plan | [`../../arch/l4.md`](../../arch/l4.md) §8 |
| the runtime parts that replace the handle's contents | [`runtime.md`](runtime.md) |
| the executor | [`interpreter.md`](interpreter.md) |
| the compile seam and the script block image | [`seam.md`](seam.md), [`seam-fields.md`](seam-fields.md) |
| the front end | [`columns.md`](columns.md), [`generator.md`](generator.md) |
| the VM host that drives this handle | [`../vm.md`](../vm.md) |
| the work left, and what each column owes | `TODO.md` §12.1 |
| the surface baseline | [`../../surface/runtime_surface.json`](../../surface/runtime_surface.json), `docs/rules/gates.md` |
