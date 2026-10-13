# Precompiled TJS2 bytecode loading audit

Oracle: `reference/cpp/**`. Scope: how a storage-backed script is executed when
the storage bytes are TJS2 precompiled bytecode (`TJS2100\0DATA ...`) rather
than text, and what this port does about it.

Motivation: a device log showed `startup.tjs` decoding as `windows-1251` with
`nul_bytes=747` and `decoded_has_nul=true`, head
`54 4a 53 32 31 30 30 00 14 05 00 00 44 41 54 41` (`TJS2100`, then `DATA`).
The lexer then rejected the decoded text with a `syntax error`. The reference
does not decode those bytes as text at all.

## 1. The reference mechanism and order

`TVPExecuteStorage` (the body of `Scripts.execStorage` / `Scripts.evalStorage`)
offers the storage bytes to the bytecode loader **before** it opens a text
stream:

| Step | Reference |
|---|---|
| Open a *binary* read stream over the placed path, with the same mode string | `reference/cpp/core/base/ScriptMgnIntf.cpp:716-717` |
| `isbytecode = TVPScriptEngine->LoadByteCode(stream, result, context, shortname)` | `reference/cpp/core/base/ScriptMgnIntf.cpp:724-725` |
| If `isbytecode`, return (the block already ran) | `reference/cpp/core/base/ScriptMgnIntf.cpp:727-780` |
| Otherwise open a *text* stream and `ExecScript` / `EvalExpression` | `reference/cpp/core/base/ScriptMgnIntf.cpp:783-809` |

The stream-based entry is `bool tTJS::LoadByteCode(tTJSBinaryStream*, ...)`
(`reference/cpp/core/tjs2/tjs.cpp:512-546`). It reads the 8-byte
`BYTECODE_FILE_TAG_SIZE` header and branches:

* `tTJSByteCodeLoader::IsTJS2ByteCode(header)` matches `TJS2100\0`
  (`reference/cpp/core/tjs2/tjsByteCodeLoader.cpp:18-27`) → read the whole
  buffer, call the buffer entry `LoadByteCode(buff, len, ...)`
  (`tjs.cpp:503-510`), which runs `tTJSScriptCache::LoadByteCode`
  (`reference/cpp/core/tjs2/tjsScriptCache.cpp:199-212`), and return `true`.
* Else, if `result != nullptr` and `tTJSBinarySerializer::IsBinary(header)`
  matches `KBAD100\0` (`reference/cpp/core/tjs2/tjsBinarySerializer.cpp:15-24`),
  deserialize the stream and return `true`.
* Else return `false`.

The `isbytecode` return is a **three-state** result in effect:

* `true` — input was bytecode (or binary-serialized data) and ran;
* `false` — input was not recognized; the caller falls through to text;
* throw — input was recognized as bytecode and the loader rejected it. The
  buffer entry raises `TJSByteCodeBroken` when `ReadByteCode` returns null
  (`tjsScriptCache.cpp:209-211`); `ReadByteCode` returns null on a declared
  size, `DATA` tag or `OBJS` tag mismatch
  (`reference/cpp/core/tjs2/tjsByteCodeLoader.cpp:40-56,60-66`), and throws
  `TJSByteCodeBroken` directly on a deeper body inconsistency
  (`:181-183,738-740`). A recognized-but-broken blob therefore does **not**
  fall through to text; it raises.

`TVPExecuteBytecode` (`reference/cpp/core/base/ScriptMgnIntf.cpp:691-699`) is
the buffer form and is a thin `TVPScriptEngine->LoadByteCode(content, len, ...)`
call; it is not on the storage path.

`TVPCompileStorage` (`reference/cpp/core/base/ScriptMgnIntf.cpp:813-843`) is the
**writer** and does **not** try bytecode first: it always reads the source as
text and calls `CompileScript`. The reader and writer are the same VM, which is
what the round-trip test below leans on.

## 2. What this port changed

### 2.1 ABI entry `tjs2_load_bytecode`

`crates/tjs/cpp/tjs2_abi.h` / `.cpp` gained

```c
int tjs2_load_bytecode(tjs2_engine *e, const char *buff, size_t len,
                       const char *name, void *context, int *out_is_bytecode,
                       tjs2_value *out_result, char **out_error);
```

It mirrors the VM's stream-based `LoadByteCode`, not a re-implementation:

* `out_is_bytecode` mirrors the reference `isbytecode`: 1 when the loader
  recognized and consumed the input (bytecode, or the binary-serialized data
  the same loader entry also accepts), 0 when it did not (the caller falls
  through). **Non-bytecode is not an error.**
* A genuine loader rejection returns non-zero and a malloc'd message in
  `out_error`; `out_result` stays void.
* The decision is made by the VM, not the wrapper. For not-bytecode input the
  wrapper calls the loader and returns its `false` unchanged (the loader also
  still sees the `KBAD100\0` case); recognized bytecode still executes through
  the VM's stream-based `LoadByteCode`.
* The one wrapper-side check is a crash guard (section 4): when the loader's
  own `IsTJS2ByteCode` predicate matches, the wrapper probes with the loader's
  `ReadByteCode` so a blob the loader would reject becomes a typed error
  instead of reaching the loader's null-block failure path. It neither
  changes the bytecode-vs-text decision nor makes "not bytecode" an error.
* `last_object` is cleared at entry and populated by `variant_to_value` exactly
  like `tjs2_exec_script`, so an object result can be retained by the Rust
  wrapper.

### 2.2 In-memory stream

The stream-based `LoadByteCode` needs a `tTJSBinaryStream`. The port-authored
`crates/tjs/cpp/streams.cpp` had no in-memory stream — it has
`FileBinaryStream`, `TextReadStream` and `TextWriteStream` only. A read-only
`MemoryBinaryStream` was therefore added **in that same file**, next to
`FileBinaryStream`, and exposed as `tjs2_create_memory_stream` (declared in
`tjs2_abi.h`). It borrows the caller's buffer, is used only for the duration of
`tjs2_load_bytecode`, and implements `Seek`/`Read`/`GetSize` with the same
`Seek`-returns-the-position convention `FileBinaryStream` uses (the base class
defines `GetPosition()` as `Seek(0, SEEK_CUR)`).

### 2.3 Call site and order

`crates/compat/src/natives/scripts.rs`:

* `read_storage_bytes` reads once and applies the mode's `oN` offset (the
  reference opens both the binary and the text stream with the same mode,
  `ScriptMgnIntf.cpp:716,783`).
* `execute_storage` — used by `execStorage`, `evalStorage` and the public
  `exec_storage` startup entry — calls `engine.load_bytecode(&bytes, ...)`
  first:
  * `Ok(Some(result))` → return it;
  * `Ok(None)` → decode the same bytes as text and run the existing
    exec/eval path unchanged;
  * `Err` → propagate the typed error. No swallow, no fallback.
* `decode_storage_text` is the old `load_storage_text` decode half; the text
  bytes, decode order, error strings and diagnostic format are unchanged.
* `compileStorage` still reads text and compiles it, matching
  `TVPCompileStorage` (`ScriptMgnIntf.cpp:813-843`); it does not attempt
  bytecode first.

### 2.4 Diagnostic

The `script read` debug line keeps every field and its text format. The bytecode
path is reported distinctly:

```
script read 'data/foo.tjs' bytes=1300 head32=[54 4a ...] encoding=precompiled bytecode nul_bytes=747 decoded_has_nul=false
```

Text reads still report the decoder label (`detected ...`, a BOM, the `FE FE`
container mode, or `decode-error`). A recognizable bytecode blob that the
loader rejects is also logged with `encoding=precompiled bytecode` before the
typed error is returned, so the next device log says which path ran.

## 3. Tests

`crates/compat/tests/scripts.rs` (driving the real `Scripts.execStorage` / L2
storage surface):

| Test | Asserts | Citation |
|---|---|---|
| `exec_storage_executes_compiled_bytecode` | `compileStorage` output starts `TJS2100\0`; `execStorage` runs a top-level `var` and the side effect is visible | `ScriptMgnIntf.cpp:810-845`, `:713-727` |
| `exec_storage_still_runs_plain_text` | a non-bytecode file reaches the text path | `ScriptMgnIntf.cpp:713-811` |
| `corrupt_bytecode_is_a_typed_error` | flipping the `DATA` tag makes `execStorage` raise the loader's `Bytecode read error` | `ScriptMgnIntf.cpp:713-727`, `tjsScriptCache.cpp:199-212` |
| `truncated_bytecode_is_a_typed_error` | a header whose declared size exceeds the buffer raises, not falls back | `tjsByteCodeLoader.cpp:49-51` |
| `magic_prefixed_text_is_rejected_by_the_loader_not_run_as_text` | bytes that begin with the bytecode tag are decided by the loader, so the order is observable | `ScriptMgnIntf.cpp:713-727` |

### What the tests prove

* The port asks the loader first and decodes text only when the loader reports
  `false`.
* Recognized bytecode produced by this port's VM writer loads and executes
  through the storage path.
* Recognized-but-broken bytecode raises a typed error instead of falling back.

### What the tests do **not** prove

* **They are a self-consistency round trip.** Both the compiled fixture and the
  loader are the same vendored VM. They show the wiring is correct, not that a
  `.tjsb` produced by the original KiriKiri (or a different compiler revision,
  or a title-shipped file with a different `filesize`/version edge) loads.
* The real evidence for the reported failure is a **game-shipped bytecode
  file**. No such file is on the host. A device run with the failing title
  should show `encoding=precompiled bytecode` for `startup.tjs` (and
  `system/Initialize.tjs`) and get past line 1; that run is what closes the
  loop (D36).
* The corrupt/truncated cases are constructed by editing a file this port
  wrote, so they do not cover every structural inconsistency
  `tjsByteCodeLoader.cpp` can encounter.

## 4. Loader observations / possible divergences

The vendored `tjsByteCodeLoader.cpp`, `tjsScriptCache.cpp` and the
`LoadByteCode` entry are byte-identical to `reference/cpp/core/tjs2/` (checked
with `diff`), so there is no port divergence to record. Two loader behaviours
shared with the reference and one wrapper-side safety guard are worth naming:

* **Binary-serialized data is consumed by the same branch.** `LoadByteCode`
  returns `true` for a `KBAD100\0` stream as well, so `execStorage` on such a
  stream deserializes it and never reaches the text path. This matches the
  reference; it is noted so a future reader does not mistake it for a bug.
* **A magic-prefixed blob shorter than the loader's fixed header would read
  past the buffer.** `IsTJS2ByteCode` needs only 8 bytes, but `ReadByteCode`
  reads the file size at offset 8, the `DATA` tag at 12 and the data-area size
  at 16 (`tjsByteCodeLoader.cpp:40-56`), i.e. through offset 19. `tjs2_load_bytecode`
  therefore rejects a recognized magic shorter than the 20-byte fixed header as
  the same `Bytecode read error.` typed error before it probes or calls the
  loader; `short_magic_prefixed_blob_is_a_typed_error` covers 8, 11 and 19
  bytes. The deeper unbounded counts inside `ReadDataArea`/`ReadObjects` (e.g.
  a huge declared data-area size) remain unguarded in the loader and are not
  covered.
* **The loader's rejection path raises with a null block, and
  `eTJSScriptError`'s block holder dereferences it**
  (`tjsScriptCache.cpp:204-211` passes `blk.get()`,
  `tjsError.cpp:61-64` calls `Block->AddRef()`). Because that path is a crash
  and not a catchable error, `tjs2_load_bytecode` probes the loader with the
  loader's own public `IsTJS2ByteCode` + `ReadByteCode` before calling the
  stream entry: a null probe result is returned as the same
  `Bytecode read error.` message the reference intends, and a non-null probe is
  released and the stream entry runs it. This is the only wrapper-side logic;
  it does not decide bytecode-vs-text (a non-bytecode input skips the probe)
  and it does not sniff the `TJS2100` tag itself. If the vendored loader later
  gets a null-safe block holder, the probe can be deleted.

## 5. Writer gap found while building the round-trip test

The test's first fixture (`var bytecode_ran = 40; bytecode_ran += 2;`) failed
inside the VM with `bytecode.tjsb(1): Cannot convert the variable type (() to
Object)`. A gdb backtrace on the built test binary pinned it:

```
tTJSInterCodeContext::OperatePropertyDirect (ope=5)   tjsInterCodeExec.cpp:1625
  tTJSVariant::AsObjectClosure (void -> Object)
```

The failing operand is the object register of `VM_ADDPD`; gdb showed
`code[2]` = `-800` (byte offset) where `ra[-1]` is `-20`. Root cause is the
**writer**, not the context (the script block's `ContextType` is
`ctTopLevel`, and the top-level `var` executed before the `+=` failed):

* `tTJSScriptBlock::TranslateCodeAddress` (`tjsScriptBlock.cpp`) runs on
  export. Its `opTable` registers only the base opcode of each arithmetic
  family (`VM_ADD`, ...), so `VM_ADDPD`/`VM_ADDPI`/`VM_ADDP` are not found.
  The loop advances by one word, and those operands are left as the code
  generator's byte offsets.
* `tTJSByteCodeLoader::TranslateCodeAddress` (the reader) **does** offset
  `VM_ADDPD` (4 register operands) and `VM_INC`/`VM_DEC` variants, so it
  multiplies the already-byte-offset operands a second time.
* The same gap exists for the `VM_INC`/`VM_DEC` variants in the writer
  (`m[VM_INC]`/`m[VM_DEC]` only); the reader handles them
  (`OP1_DISASM(VM_INC)`/`OP1_DISASM(VM_DEC)` in `tjsByteCodeLoader.cpp`).

The vendored `tjsScriptBlock.cpp` is byte-identical to
`reference/cpp/core/tjs2/tjsScriptBlock.cpp`, so this contradicts the loader
format in *both* trees, i.e. the reference snapshot's export table is itself
an incomplete reconstruction (AGENTS §9.9).

**Patching the vendored table is not this port's remedy.** AGENTS §2 keeps
`crates/tjs/cpp/tjs2/**` vendored and unchanged — the VM is the oracle, not
ours to edit; only the ABI (`tjs2_abi.*`) and the port-authored `streams.cpp`
are. Editing `BuildOpTable` to register the `base+1/+2/+3` variants with the
loader's operand counts (`{5,4}`, `{5,4}`, `{4,3}` for the arithmetic family;
`{4,3}`, `{4,3}`, `{3,2}` for INC/DEC) would make `compileStorage` round-trip,
but it would also change the vendored VM to match our reconstruction rather
than the reference, so it must not be done silently. The test fixture uses a
plain top-level `var` (`VM_GLOBAL` + `VM_SPDS`), which the writer does
round-trip. This does not affect loading a game's `.tjsb`: the reader's
translation is the one that runs there, and it is complete for these opcodes.

**Open question — does the shipped engine share the snapshot's table?** The
snapshot and the vendored copy are the same incomplete `opTable`, but the
snapshot is a reconstruction (AGENTS §9.9), so it may not match the original
KiriKiri. If the shipped engine's exporter is **complete**, then our decision
to keep the snapshot's table is a *documented divergence from the shipped
engine in favour of the snapshot* — the same choice the whole port makes when
it treats `reference/cpp/**` as the oracle. That is a call for the main agent,
not a silent patch. Evidence that would settle it:

* a real `.tjsb` compiled by a shipped KiriKiri (or the original `tjs`
  bytecode writer) that contains a `+=`-family or `++`/`--` statement: the
  loader either executes it (writer stored indices; snapshot table is the
  incomplete one) or produces the double-multiplied operands (writer stored
  byte offsets; the reader is the incomplete one);
* a reference-build `tjsScriptBlock.cpp` whose `TranslateCodeAddress` handles
  the `xPD`/`xPI`/`xP` variants, which would show the snapshot's `opTable` is
  a lossy reconstruction.

`docs/vm-defects.md` (outside this change's edit scope) is the canonical place
for the writer defect; this section is the reference-fidelity note that
explains why the port leaves the table alone.

## 6. Device-unverified

Not verifiable without running the VM (`cargo` build/test and a real title):

* That the loader's `ReadByteCode` accepts the exact structural layout of the
  failing title's `startup.tjs` / `Initialize.tjs` (data area, object table,
  string table, address translation).
* The end-to-end startup path (`app` → `compat::exec_storage` → bytecode
  execution) on the device, including the diagnostic line naming the bytecode
  path.
* Any bytecode-version or `filesize` edge the title uses that this port's
  writer does not emit.
