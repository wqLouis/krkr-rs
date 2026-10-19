# VM and reference-engine defects, quirks and traps

A durable record of what is wrong, surprising or load-bearing in the vendored
TJS2 VM (`crates/tjs/cpp/tjs2/**`) and the reference engine
(`reference/cpp/**`), written for whoever rewrites the VM, the storage layer or
the script layer. This is a working document, not an audit: it lists the things
a rewrite must not rediscover the hard way.

- Repo: `$KRKR_REPO` · HEAD `4f9f894`, branch `main`
- **Line numbers are working-tree numbers** (other work was in flight while this
  document was written). Function/member/class names are the stable anchor;
  re-grep the name if a number has moved.
- Sources read for this document (2026-10-13): `AGENTS.md` §0/§1/§9/§11;
  `docs/migration/08-tjs-vm-and-native-plugins.md`;
  `docs/audits/unsafe-inventory.md` §2.6/§5.3;
  `docs/audits/bytecode-scripts.md`; `docs/audits/save-crash.md`;
  `docs/migration/README.md` D34–D38; the vendored C++ and the reference it
  was diffed against.
- **Verification rule (D36).** Every entry below was checked against the
  vendored source, the reference source, or a committed test. Anything that
  could not be settled from the tree is under §9 and marked *unverified*. No
  hypothesis is written as a finding.

## How to read an entry

Each entry has: **id**, a one-line **summary**, the **defect**, **evidence**
(`path:line`), **whose bug** it is, whether a **script can observe** it, the
**current workaround**, and **what a rewrite should do**. Where the vendored
file is byte-identical to the reference, the diff was run and the entry says
so; a "vendored == reference" claim was checked with `diff`, not assumed.

A note on the reference: `reference/cpp/**` is itself a reconstruction in
places (entry **REC-1**), so "the reference does X" means "the source in the
tree does X". Where the shipped data could disagree, the entry says so.

---

## BLK-1 — `tTJSScriptCache::LoadByteCode` dereferences a null block on a broken bytecode file

**Summary:** the VM's bytecode-rejection path builds a script error around a
null `tTJSScriptBlock` and crashes; a corrupted or truncated `.tjsb` kills the
process instead of raising a catchable error.

**Defect.** `tTJSScriptCache::LoadByteCode`
(`crates/tjs/cpp/tjs2/tjsScriptCache.cpp:199-212`) calls
`loader->ReadByteCode(...)`, and when it returns `nullptr` it calls
`TJS_eTJSScriptError(TJSByteCodeBroken, blk.get(), 0)` (line 211) with a null
block. `eTJSScriptError` holds a `tScriptBlockHolder Block` whose constructor
does `Block = block; Block->AddRef();`
(`crates/tjs/cpp/tjs2/tjsError.cpp:60-64`) — a null dereference. The block
holder is constructed before any message is built, so the throw itself faults.

**Evidence.**
- `crates/tjs/cpp/tjs2/tjsScriptCache.cpp:199-212`, `crates/tjs/cpp/tjs2/tjsError.cpp:60-64`.
- `tjsScriptCache.cpp` and `tjsError.cpp`/`.h` are byte-identical to
  `reference/cpp/core/tjs2/` (checked with `diff`), so this is the reference's
  bug, not a port regression.
- `tjsByteCodeLoader.cpp:40-56,60-66` returns `nullptr` on a declared-size,
  `DATA`-tag or `OBJS`-tag mismatch; `:181-183,738-740` throws
  `TJSByteCodeBroken` directly. A recognized-but-broken blob therefore hits
  the null-block path.

**Whose bug:** reference.

**Script-observable:** yes. A title that ships a mismatched or corrupt
precompiled script crashes the host (a SIGSEGV, not a TJS error), which is a
hard behaviour difference from a running reference too — the reference also
crashes here, so "matching the reference" would mean reproducing the crash.
We deliberately do not reproduce it.

**Current workaround.** `tjs2_load_bytecode`
(`crates/tjs/cpp/tjs2_abi.cpp`) never lets the VM's null-block path run: when
`len >= BYTECODE_FILE_TAG_SIZE` and `tTJSByteCodeLoader::IsTJS2ByteCode`
matches, it probes with the loader's own `ReadByteCode`; a null probe returns
the same `"Bytecode read error. File is broken or it's not bytecode file."`
message the reference intends, and a non-null probe is released and the stream
entry runs. The probe decides only the malformed-recognized case, never
bytecode-vs-text (`docs/audits/bytecode-scripts.md` §2.1, §4). Tests:
`crates/compat/tests/scripts.rs` `corrupt_bytecode_is_a_typed_error` and
`truncated_bytecode_is_a_typed_error`.

**What a rewrite should do.** Never construct a script error with a null
block: validate the header (declared size equals the actual length, `DATA` and
`OBJS` tags present, per-section sizes in range) *before* allocating the block,
and return a typed loader error for every malformed input. If the error type
needs a block for a source line, make the block optional and make line lookup
return 0 when it is absent.

---

## STR-1 — `ttstr` truncates at an interior NUL, so a NUL-bearing script is silently cut rather than rejected

**Summary:** TJS strings are NUL-terminated C strings; a script containing an
interior NUL is truncated by the VM at the NUL and the prefix runs.

**Defect.** `tTJSVariantString::SetString(const tjs_char *ref, ssize_t maxlen)`
(`crates/tjs/cpp/tjs2/tjsVariantString.h:74-86`) computes the length with
`TJSGetShorterStrLen(ref, maxlen)` when a length is supplied and `TJS_strlen`
otherwise. `TJSGetShorterStrLen` (`crates/tjs/cpp/tjs2/tjsVariantString.cpp:38-46`)
loops `while(*p && --max) p++`, so it stops at the first NUL. The length-aware
allocator `TJSAllocVariantString(const tjs_char *ref, size_t n)`
(`tjsVariantString.cpp:572-584`) forwards to that `SetString`, so the explicit
length does not preserve what follows a NUL. `tTJSScriptBlock::SetText`
(`crates/tjs/cpp/tjs2/tjsScriptBlock.cpp:198-263`) then does
`Script = new tjs_char[TJS_strlen(text) + 1]` (line 211) and `TJS_strcpy`, so
the script text is cut at the first NUL before the lexer ever sees it.

**Evidence.** `tjsVariantString.h:74-86`, `tjsVariantString.cpp:38-46,572-584`,
`tjsScriptBlock.cpp:198-263` (and the bytecode-compile sibling at `:592-615`).
`tjsVariantString.cpp` and `tjsScriptBlock.cpp` are byte-identical to the
reference. `tjsVariantString.h` differs from the reference only in the appended
null-`this` helpers at lines 219-232; the NUL logic is identical.

**Whose bug:** reference (its C-string string type). Not a regression.

**Script-observable:** yes. A source file whose bytes contain a NUL executes
its prefix as if the rest did not exist; a title relying on such a file would
behave differently in a rewrite that rejected the NUL. It is a silent
truncation, not an error.

**Current workaround.** We hand the VM a byte pointer **and a length** and let
the VM own the outcome, so our result matches the reference exactly.
`Tjs2Engine::exec_script_with_context` / `eval_with_context`
(`crates/tjs/src/lib.rs:841-875`, `:891-925`) pass `script.len()` to
`tjs2_exec_script`/`tjs2_eval`; the ABI converts with the length-aware
`utf8_to_u16(s, len)` (`crates/tjs/cpp/tjs2_abi.cpp:190-196`) and builds
`TJS::ttstr(s.c_str(), s.size())`, deliberately keeping the interior NUL in
the payload. The VM then truncates it. We do **not** pre-reject or pre-truncate
— that would be a divergence in the other direction.

**What a rewrite should do.** Use a length-aware string type (`Vec<u16>` or
`&[u16]` with an explicit length) so an interior NUL is ordinary data. Then
decide explicitly whether a NUL in *source text* is legal: either compile past
it or raise a typed lexical error. Do not silently drop the tail. If the
observable truncation must be preserved for compatibility, pin it with a test
that asserts the executed prefix, and document it as a deliberate legacy
behaviour rather than an accident.

---

## TS-1 — the `FE FE` text-stream container's reader and writer disagree (RESOLVED: the vendored offset-4 read was a reconstruction error)

**Summary:** the vendored reference reader starts a mode 0/1 payload at byte 4
(`(size - 4) / 2`) while its own mode-2 reader and its writer use a 5-byte
header (`FE FE <mode> FF FE`), so the vendored reader appears not to read the
writer's mode-1 output. The resolution is that the `+4` is a reconstruction
error: the shipped engine reads the two bytes after the mode as the mandatory
UTF-16LE BOM `FF FE` and starts the payload at **offset 5**, so mode 0/1 is
self-consistent with mode 2 and with the writer. The port followed the
reconstruction's offset 4 for a while and misread every real game script; it
now follows the shipped data.

**Related:** BLK-3 is the same class of reader/writer split, in the bytecode
operand table.

**Defect (historical — the vendored reconstruction).**
- Vendored reader (`reference/cpp/core/base/TextStream.cpp:82-105`): for
  `FE FE m` with `m == 0 || m == 1` the payload pointer is `raw.data() + 4` and
  the length is `(size - 4) / 2` (`:85-88`). For `m == 2` the payload starts at
  `raw.data() + 5` (`:104-116`).
- Writer (`:269-280`): for `_cryptMode == 1 || 2` it writes
  `crypt_mode_sig[0..3]` where `crypt_mode_sig[0]=0xfe, [1]=0xfe, [2]=mode,
  [3]=0` — but `WriteBuffer(crypt_mode_sig, 3)` writes only **3** bytes
  (`:275`); `crypt_mode_sig[3] = 0` at `:274` is dead. It then writes an
  unconditional `FF FE` BOM (`:279-280`). So mode 1 emits
  `FE FE 01 FF FE` (5 bytes). The writer rejects any mode other than 1 or 2
  (`:258-260`), so there is no mode-0 writer.

**Resolution and evidence.** The shipped engine's `tTVPTextReadStream` consumes
the 3-byte signature, then reads **2 further bytes and requires them to be
`FF FE`** (raising `TVPUnsupportedCipherMode` otherwise), and only then reads
the mode 0/1 payload — i.e. from offset 5, exactly like mode 2 and the writer.
The vendored mode-0/1 `+4` contradicts both the shipped data and the rest of
its own file (mode 2 at `+5`). The device evidence settled it:
`krkr-rs-logs (6).txt`'s boot failures carry odd drop counts
(`289 = 2·144 + 1`, `9 = 2·4 + 1` — a trailing half unit), the signature of a
one-byte misalignment, and encrypting a real 70 KB UTF-16 script with the
reference mode-0/1 cipher and reading it back shows the offset-4 read yields 32
unpaired surrogates plus one trailing byte while the offset-5 read matches the
plaintext exactly. This is the AGENTS §9.9 case: the vendored source
contradicted the shipped data, so the code follows the shipped data.

**Whose bug:** the vendored reconstruction, not the shipped engine.

**Script-observable:** it was. Every real Yuzusoft-lineage boot script decoded
through the offset-4 read one byte out of phase, producing unpaired surrogates
the parser rejected (`startup.tjs(1) -> Initialize.tjs(1) -> Config.tjs(1):
syntax error`); at offset 5 the payload reaches the parser byte-for-byte.

**Resolution (code).** The port reads modes 0/1/2 at offset 5 and requires the
`FF FE` BOM for every mode (`crates/compat/src/text.rs` `decode_container` /
`require_container_bom`; `crates/tjs/cpp/streams.cpp` `TextReadStream`, which
also fails a malformed container instead of returning empty). The container
decode keeps raw `Vec<u16>` code units and the script loader hands them to the
VM through the wide exec/eval ABI, so an unpaired surrogate is preserved like
the reference's `std::u16string`. The port's writer emits the same 5-byte
`FE FE <mode> FF FE` header for modes 0/1 as for mode 2, so a port-written
stream round-trips. Pinned by
`crates/compat/src/text.rs::fefe_cipher_payload_starts_at_offset_five`,
`fefe_cipher_requires_the_byte_order_mark`,
`fefe_containers_keep_every_code_unit`,
`crates/tjs/src/lib.rs::rejects_a_cipher_container_without_the_bom` and
`writer_emits_the_reference_crypt_header`.

**What a rewrite should do (unchanged).** Define **one** container format with
a single dimensioned header (magic, version, mode, flags, payload length) and a
round-trip property test. Keep a legacy reader for shipped files, and state the
writer's contract in the format comment and pin it by a byte-level test.

---

## BLK-3 — the bytecode export table and the loader disagree: arithmetic-variant opcodes are translated twice

**Summary:** the bytecode writer's address-translation table registers only the
base opcode of each arithmetic family, so the variant opcodes
(`VM_ADDPD`/`VM_ADDPI`/`VM_ADDP`, and the `VM_INC`/`VM_DEC` variants) are not
found and keep the code generator's byte offsets; the loader translates all of
them, multiplying the already-byte-offset operands a second time. This is the
same reader/writer split as TS-1, in the bytecode operand table.

**Defect.** Both sides have a `TranslateCodeAddress`:

- **Export** (`tTJSScriptBlock::TranslateCodeAddress`,
  `crates/tjs/cpp/tjs2/tjsScriptBlock.cpp:876-921`) walks the in-memory code
  area and converts each operand from the runtime byte-offset form back to an
  array index for the file (`OffsetReg`/`OffsetCode` =
  `TJS_FROM_VM_REG_ADDR`/`TJS_FROM_VM_CODE_ADDR`, `tjsInterCodeGen.h:34-37`).
  It looks the opcode up in `BuildOpTable()` (`:789-871`), an exact-keyed
  `unordered_map<int, OpInfo>`. The arithmetic families are registered by their
  **base** opcode only (`op2_group = { VM_LOR, …, VM_MUL }`,
  `m[op] = {0, 0, false, HandleOp2}`, `:857-861`), and `VM_INC`/`VM_DEC`
  likewise (`:852-853`). The variants are consecutive opcodes
  (`TJS_NORMAL_AND_PROPERTY_ACCESSER(x) x, x##PD, x##PI, x##P`,
  `tjsInterCodeGen.h:42`, `:44-80`), and although `HandleOp2`/`HandleIncDec`
  are written for the `code[i] - opcode` cases 1/2/3
  (`tjsScriptBlock.cpp:664-692`, `:694-719`), the map lookup is by exact
  value, so a variant opcode is **not found**. The walk takes the "unknown
  opcode: advance by 1" branch (`:882-886`) and leaves that instruction's
  operands as byte offsets.
- **Reader** (`tTJSByteCodeLoader::TranslateCodeAddress`,
  `crates/tjs/cpp/tjs2/tjsByteCodeLoader.cpp:387-741`) has a complete switch:
  `OP2_DISASM(VM_ADD)` and the other families handle `case c` (2 operands),
  `case c+1`/`case c+2` (4 operands, size 5) and `case c+3` (3 operands,
  size 4) (`:421-462`), and `OP1_DISASM(VM_INC)`/`OP1_DISASM(VM_DEC)` handle
  their variants (`:534-559`). It converts every operand from index to byte
  offset (`TJS_TO_VM_REG_ADDR`). For an operand the writer left as a byte
  offset, this is a second multiplication by `sizeof(tTJSVariant)`.

**Evidence.**
- `crates/tjs/cpp/tjs2/tjsScriptBlock.cpp:664-692` (`HandleOp2`),
  `:694-719` (`HandleIncDec`), `:789-871` (`BuildOpTable`; `op2_group` at
  `:857-861`, INC/DEC at `:852-853`), `:876-921` (`TranslateCodeAddress`;
  unknown-opcode branch at `:882-886`).
- `crates/tjs/cpp/tjs2/tjsByteCodeLoader.cpp:387-741` (`OP2_DISASM` variants at
  `:421-462`, `OP1_DISASM(VM_INC/VM_DEC)` at `:534-559`).
- `crates/tjs/cpp/tjs2/tjsInterCodeGen.h:42` and `:44-80` (the 4-opcode family
  expansion).
- gdb, from the bytecode round-trip test: the failing operand is `VM_ADDPD`'s
  object register, with `code[2] = -800` (a byte offset) where `ra[-1] = -20`;
  the VM then raises `Cannot convert the variable type (() to Object)` in
  `tTJSInterCodeContext::OperatePropertyDirect` (`tjsInterCodeExec.cpp:1625`).
  Recorded in `docs/audits/bytecode-scripts.md` §5.
- Vendored `tjsScriptBlock.cpp` is **byte-identical** to
  `reference/cpp/core/tjs2/tjsScriptBlock.cpp` (verified with `diff`), so this
  is the reference snapshot's table, not a port regression.
- Described in the test at `crates/compat/tests/scripts.rs:298-303`.

**Whose bug:** the reference snapshot's — a reconstruction artifact under
AGENTS §9.9 / REC-1. The vendored copy carries it unchanged.

**Script-observable:** yes, but only through the port's own `compileStorage`
output. A `+=`/`-=`/… (`VM_ADDPD`-family) or `++`/`--` (`VM_INC`/`VM_DEC`
variant) statement compiled with `Scripts.compileStorage` and then run with
`Scripts.execStorage` mis-executes (or raises) because the loader
double-translates its operands. **Loading a game-shipped `.tjsb` is
unaffected**: the reader's translation is complete, and the reader is the side
that runs on load.

**Current workaround.** None on the VM. The vendored table is deliberately
left unchanged — AGENTS §2 keeps `crates/tjs/cpp/tjs2/**` the oracle rather
than ours to edit (only the ABI and the port-authored `streams.cpp` are port
code). The affected round trip is documented in
`docs/audits/bytecode-scripts.md` §5, and the bytecode test fixture uses a
plain top-level `var` (`VM_GLOBAL` + `VM_SPDS`), which round-trips, so the
suite passes without exercising the gap.

**What a rewrite should do.** Use **one** translation table (or one canonical
operand form) shared by the writer and reader, so base and variant opcodes
cannot diverge. Better, store operands in a form that does not depend on a
translation pass at all (explicit register indices used by both the runtime and
the file, or a decoded instruction stream). Add a round-trip property test that
covers every opcode family, including `+=`-family compound assignment and
`++`/`--`, so a missing table entry fails immediately.

**Open question — does the shipped engine share the snapshot's incomplete
table?** The snapshot's table is incomplete, but the snapshot is a
reconstruction, so it may not match the original KiriKiri. If the shipped
engine's exporter is **complete**, then keeping the snapshot's table is a
deliberate divergence *toward the snapshot* and must be recorded as such, not
treated as reference behaviour. Evidence that would settle it:
- a real `.tjsb` produced by a shipped KiriKiri (or the original `tjs` bytecode
  writer) containing a `+=`-family or `++`/`--` statement: if it loads and
  runs, the shipped writer stored indices and the snapshot's table is the
  incomplete one; if it double-translates, the shipped writer stored byte
  offsets and the reader is the incomplete one;
- a reference build whose `TranslateCodeAddress` handles the `xPD`/`xPI`/`xP`
  variants, which would show the snapshot's `opTable` is lossy.

Until then the port follows the snapshot and records the divergence (D36).

---

## GLO-1 — the VM has process-global state, but `Tjs2Engine` is `Send + Sync` by `unsafe impl`

**Summary:** the TJS2 VM keeps process-global string/cache state and is
single-VM by design, yet our public `tjs` API makes two engines safely usable
from two threads.

**Defect.** `Tjs2Engine` and `DetachedValue` carry
`unsafe impl Send`/`unsafe impl Sync`
(`crates/tjs/src/lib.rs:607-608`, `:633-634`), and `Tjs2Engine::new()`
(`:806-819`) is a **safe** fn callable from any thread. The VM state that makes
that unsound:
- `tTJSGlobalStringMap` is a raw file-scope pointer plus a single
  `std::mutex` (`crates/tjs/cpp/tjs2/tjsGlobalStringMap.cpp:34`, `:113-118`);
  the `tTJS` constructor/destructor add/release it
  (`crates/tjs/cpp/tjs2/tjs.cpp:112`, `:219`).
- Other file-scope statics hold live TJS values or strings, e.g.
  `static tTJSVariant VoidValue` (`tjsArray.cpp:1382`),
  `static tTJSVariant VoidVal` (`tjsDictionary.cpp:767`),
  `static ttstr InitialType` (`tjsDebug.cpp:414`).

Safe code that breaks the process:
```rust
let a = Tjs2Engine::new().unwrap();
let b = Tjs2Engine::new().unwrap();
std::thread::spawn(move || { let _ = a.eval("1", "t"); });
std::thread::spawn(move || { let _ = b.eval("1", "t"); }); // races the globals
```

**Evidence.** `crates/tjs/src/lib.rs:607-608,633-634,806-819`;
`tjsGlobalStringMap.cpp:34,113-118`; `tjs.cpp:112,219`;
`tjsArray.cpp:1382`; `tjsDictionary.cpp:767`; `tjsDebug.cpp:414`.
`docs/audits/unsafe-inventory.md` §5.3.

**Whose bug:** **both.** The global state is the reference's design (it assumes
one VM per process); the `unsafe impl`s and the safe `new()` are ours. The
audit phrases it as "`Tjs2Engine`/`DetachedValue` `Send + Sync` is unsound
API".

**Script-observable:** not by one well-behaved script. It is a soundness hole
in the public Rust API: a host or integrator can corrupt the process without
writing `unsafe`. That is a compatibility-adjacent hazard (a game that ships an
in-process embedder) and a real API defect.

**Current workaround / live decision.**
- The compat binding is process-global and single
  (`crates/compat/src/binding.rs`, migration doc §3.1), and `CompatCtx` was
  made `!Send + !Sync` via `PhantomData<*mut ()>`
  (`crates/compat/src/ctx.rs:224-236`) so the dispatch handle cannot be moved
  across threads.
- `vm` owns the engine on the host thread; tests run single-threaded
  (`--test-threads=1`).
- **Open decision:** remove `Sync` (and probably `Send`) from
  `Tjs2Engine`/`DetachedValue`, or keep `Send` and add a `!Send + !Sync`
  thread-affinity token the host owns. This is **not landed.** The audit left
  it open because removing the bounds could not be compile-checked under the
  no-build rule. A read-only grep found no workspace path that *requires* the
  bounds — the only cross-thread calls near the VM are
  `std::thread::available_parallelism`
  (`crates/compat/src/natives/system.rs:1693`) and a `thread::sleep`
  (`crates/compat/src/natives/visual/mod.rs:187`), neither moving a
  `Tjs2Engine`. **Unverified whether removal compiles** — it needs the main
  agent's gate (see §9).

**What a rewrite should do.** Give each VM instance its own string pool, caches
and tables; hold no process-global mutable VM state. Make the engine type
`!Send + !Sync` by construction, or make multi-instance support real and
thread-safe. If the C ABI forces one global, make that a type-level invariant
(one owner token) rather than a comment — the C++ ABI's lack of a user-data
slot is the only reason a global is needed at all (migration doc §3.1).

---

## SEM-1 — `typeof` spellings, and `void`/`undefined`/absent, are not JavaScript's and must be reproduced exactly

**Summary:** TJS `typeof` returns `"Object"`, `"Integer"`, `"Real"`,
`"String"`, `"Octet"`, `"void"`; only an absent member is `"undefined"`, a
present-but-unset value is `"void"`, and a bare absent name raises. A rewrite
that "cleans this up" to JavaScript semantics breaks scripts.

**Defect / trap (not a bug).** These are the language's real semantics, and
they are deliberate (`TypeOf` comments say "differs from TJS1"). A rewrite must
reproduce them, not normalize them.

**Evidence.**
- `tTJSInterCodeContext::TypeOf` (`crates/tjs/cpp/tjs2/tjsInterCodeExec.cpp:2614-2650`;
  reference `:2601-2628`) maps `tvtVoid → "void"`, `tvtObject → "Object"`,
  `tvtString → "String"`, `tvtInteger → "Integer"`, `tvtReal → "Real"`,
  `tvtOctet → "Octet"`.
- `TypeOfMemberDirect` / `TypeOfMemberIndirect`
  (`crates/tjs/cpp/tjs2/tjsInterCodeExec.cpp:1877-1919`, `:1922-1990`) return
  the string `"undefined"` only on `TJS_E_MEMBERNOTFOUND`; a present-but-unset
  member is `tvtVoid` → `"void"`.
- `undefined` is not a lexer keyword (`grep` finds no entry in `tjsLex.cpp`)
  and no global is registered under it, so a bare `undefined` (or a bare
  `typeof no_such_global`) raises a member-not-found error, while
  `typeof global.no_such_global` reports `"undefined"`.
- A dictionary read *without* `typeof` swallows a miss as `void`
  (`tjsDictionary.cpp:739-743`), so the `void`/`undefined` split is observable
  only through `typeof`.
- Pinned by `crates/compat/tests/typeof_spellings.rs`
  (`typeof_uses_the_tjs_type_spellings`, `undefined_is_absence_and_void_is_unset`,
  `javascript_object_spellings_are_not_tjs`).

**Whose bug:** n/a — reference semantics, to keep.

**Script-observable:** yes, directly; every `typeof` and every absent-member
probe a title performs.

**Current workaround.** None needed: we match the reference. The test above is
the guard.

**What a rewrite should do.** Preserve every value in the table above
bit-for-bit, including `null` being `"Object"` (a null object pointer is still
an object-typed variant, `tjsVariant.h:552-557`) and functions being
`"Object"`. Reproduce the must-exist read rule that makes a bare absent name
raise. Do not adopt JavaScript's `undefined`/`"object"`/`"number"` spellings.

**Related language traps a rewrite must also reproduce** (AGENTS §9; each cost
real time): `void` is not `undefined` and a bare `undefined` raises;
`property` is a keyword (`var property = …` is a syntax error); inside a
subclass body a bare native class name resolves to `this.<Name>`; `Env::eval`
compiles in expression mode so write-then-read needs two executions; the first
layer created on a window is force-visible; a dictionary swallows a missing
read as `void` while a custom object reports absence; TJS2 does not bind
`function` declarations inside an anonymous function body (no IIFE wrappers);
`Layer.id`/`Bitmap.id`/`Window.id`/`Font.id` do not exist by design. TJS-only
reserved words (`int`, `real`, `string`, `octet`, `getter`, `setter`,
`property`, `global`, `incontextof`, `invalidate`, `isvalid`, `synchronized`,
`goto`, `enum`, `public`/`private`/`protected`/`static`, `NaN`/`Infinity`) are
listed in migration doc §10.0.

---

## REC-1 — the vendored C++ is partly a reconstruction and can contradict the shipped data; treat it as an oracle with a grain of salt

**Summary:** `reference/cpp/**` and the vendored VM are reverse-engineered in
places, so a "reference says X" claim is only as strong as the file it comes
from. Record the evidence when vendored source and shipped data disagree.

**Defect / methodology (not a code bug).** AGENTS §9.9. A reconstruction can
be wrong about a detail the real engine got right (or vice versa), and when it
is, the *data* wins for behaviour that must be compatible.

**Evidence — the worked example.** The `FE FE` reader (TS-1) is the clearest
case: the vendored reader starts a mode 0/1 payload at offset 4
(`TextStream.cpp:85-88`) while its own file, its writer
(`crypt_mode_sig[3] = 0` is declared but never written, `:271-275`) and the
shipped engine all use the 5-byte `FE FE <mode> FF FE` header and offset 5.
Following the reconstruction's offset 4 misread every real game script (odd
drop counts and unpaired surrogates); the shipped `tTVPTextReadStream` confirms
offset 5, and the port now follows the shipped data with the evidence recorded
in TS-1. This is the case where the *data* overruled the vendored source.
Also: `TVPCheckExistentLocalFile`
(`reference/cpp/core/base/impl/StorageImpl.cpp:532-546`) carries a `#if 0`
Windows `GetFileAttributes` body and a reconstructed `tTVP_stat`/`S_IFREG`
body, i.e. the shipped Windows code is commented out and a Linux reconstruction
stands in (see STO-1).

**Whose bug:** n/a — a property of the source tree.

**Script-observable:** through whichever behaviour the reconstruction gets
wrong; that is exactly why it matters.

**Current workaround.** Tests cite the reference line they assert (D30), and
device logs are treated as the tie-breaker when a real title disagrees with the
source (D36: "an engine release requires at least one real KiriKiri title run
on a real device with its log captured"). The bytecode audit is explicit that
its round trip is a self-consistency check, not proof a shipped `.tjsb` loads
(`docs/audits/bytecode-scripts.md` §3).

**What a rewrite should do.** Keep the reference as the behavioural oracle, but
record the class of each claim (source-verified, device-verified, or inferred)
and prefer shipped data where it can be observed. When a reconstruction and a
real file disagree, say so in the test/entry rather than silently following the
source.

---

## 7. Other defects and traps

### BLK-2 — the bytecode loader reads out of bounds on a short magic-prefixed blob

**Summary:** the reference loader accepts an 8-byte `TJS2100\0` header and then
reads the declared size at offset 8 and the `DATA` tag at offset 12, so a
buffer of 8–19 bytes is read past its end. Our ABI wrapper now rejects that
window before probing, but the loader itself still has no length check.

**Defect.** `tTJSByteCodeLoader::IsTJS2ByteCode`
(`crates/tjs/cpp/tjs2/tjsByteCodeLoader.cpp:18-27`) needs only 8 bytes, but
`ReadByteCode` (`:30-66`) immediately reads `read4byte(&(databuff[8]))`
(size), then `&(databuff[12])` and `&(databuff[16])`. `read4byte`
(`tjsByteCodeLoader.h:102-107`) is an unguarded 4-byte load. The loader takes
no buffer length, so a magic-prefixed blob of 8–19 bytes is an out-of-bounds
read in the reference; our ABI wrapper rejects that window before it probes
(`tjs2_abi.cpp` `tjs2_load_bytecode`, see Current workaround), but the loader
itself is unchanged. The stream path in `tjs.cpp:512-546` has the same shape
(`streamlen >= 8`, then read buffer, then the loader).

**Evidence.** `tjsByteCodeLoader.cpp:18-66`, `tjsByteCodeLoader.h:102-107`,
`tjsScriptBlock.h:33`, `tjs.cpp:512-546`, and the port probe in
`tjs2_abi.cpp` (`tjs2_load_bytecode`). Noted in
`docs/audits/bytecode-scripts.md` §4 ("a magic-prefixed blob shorter than the
loader's fixed header would read past the buffer … `tjs2_load_bytecode`
therefore rejects a recognized magic shorter than the 20-byte fixed header").
The 8–19-byte window is now pinned by
`crates/compat/tests/scripts.rs` `short_magic_prefixed_blob_is_a_typed_error`
(8, 11 and 19 bytes), but that test exercises the wrapper guard, not the
loader.

**Whose bug:** reference (the loader takes no length). The port's ABI wrapper
works around the 8–19-byte window but does not fix the loader.

**Script-observable:** yes for the reference loader — a game shipping a
truncated `.tjsb` can cause an out-of-bounds read (crash or, worse, a
misparse). Our wrapper turns the 8–19-byte window into the intended
`Bytecode read error`; the loader path it guards is unchanged.

**Current workaround.** `tjs2_load_bytecode` (`crates/tjs/cpp/tjs2_abi.cpp`)
rejects a recognized magic shorter than the 20-byte fixed header (8-byte file
tag + file size + `DATA` tag + data-area size) as the same `Bytecode read
error.` typed error before it probes or calls the loader, so the 8–19-byte
window raises instead of reading past the buffer. Test:
`crates/compat/tests/scripts.rs` `short_magic_prefixed_blob_is_a_typed_error`
(8, 11 and 19 bytes). This is a wrapper-level guard, not a loader fix: the
loader still takes no length and the deeper unbounded section counts
(`ReadDataArea`/`ReadObjects`) remain unguarded.

**What a rewrite should do.** Make the loader take an explicit length and
bounds-check every field read: require at least the fixed header
(`filesize` field at 8, `DATA` tag at 12, data-size at 16), reject
`declared filesize != actual length`, and validate each section's range before
reading it. Do the same in the stream entry. The typed loader error replaces
both the crash and the OOB.

---

### SER-1 — the built-in text serializer recurses without a depth bound

**Summary:** `Array`/`Dictionary.saveStruct` recurses one native frame per
nested container and detects cycles only by a pointer stack, so a deep
acyclic graph overflows the stack.

**Defect.** `tTJSArrayNI::SaveStructuredData` / `SaveStructuredDataForObject`
(`crates/tjs/cpp/tjs2/tjsArray.cpp:1114-1198`) and the `Dictionary` counterpart
(`tjsDictionary.cpp:417-500`) recurse with no depth limit; the cycle guard is a
pointer search (`tjsArray.cpp:1160-1168`). A 40 000-deep nested dictionary
reproduced `fatal runtime error: stack overflow, aborting`
(`docs/audits/save-crash.md` C2). The code is byte-identical to
`reference/cpp/core/tjs2/tjsArray.cpp:1114-1198` (diffed per that audit), so
the reference overflows too.

**Evidence.** `crates/tjs/cpp/tjs2/tjsArray.cpp:1114-1198`,
`tjsDictionary.cpp:417-500`; `docs/audits/save-crash.md` C2 (measured on a
release binary).

**Whose bug:** reference (shared code); the reference is only safe because a
shipped title is assumed not to build such a graph. Note the port also injects
extra members into `Dictionary` (see ABI-2/LIFE-2 below), which adds frames.

**Script-observable:** yes — a script can build a deep enough graph and abort
the process; if the cycle guard ever misses (a nested method receiver, see
save-crash C3), it becomes an unbounded, non-cyclic recursion.

**Current workaround.** None; the crash is recorded.

**What a rewrite should do.** Add an explicit depth counter and raise the
reference's recursion error (or a typed equivalent) past a bound; do not rely
on the pointer-stack cycle test alone. Keep the exact output for the legal
depth range.

---

### VIS-1 — `getVisBuffer` writes through a script-supplied integer pointer

**Summary:** the `WaveSoundBuffer.getVisBuffer` native treats a script integer
as a `*mut i16` destination and writes `count * channels` samples there.

**Defect.** `compat/src/natives/sound.rs` `sound_get_vis_buffer` /
`read_vis_buffer` (`:1828-1889`) do
`let dest = value_as_i64(&raw_args[0]) as usize as *mut i16;` (`:1889`) and then
`*dest.add(index) = …`. The reference does the same cast
(`reference/cpp/core/sound/win32/WaveImpl.cpp:3517`:
`tjs_int16 *dest = (tjs_int16 *)(tjs_int64)(*param[0]);`), so this is
reference-faithful and reference-unsafe; the only legitimate caller is the
port's own `get_sample` plugin, which passes a live `Vec<i16>`
(`plugins/get_sample/mod.rs:157-173`, `:274-282`). A script string is parsed
as an integer, so `getVisBuffer("1234", n, c)` dereferences `0x4d2`.

**Evidence.** `docs/audits/unsafe-inventory.md` §5.1 (rank 1 before shipping),
`docs/audits/save-crash.md` C4; reference `WaveImpl.cpp:3517`.

**Whose bug:** reference (the contract is unchecked); the port reproduces it.
One divergence: the reference's `AsInteger` throws for an Octet, while our
`value_as_i64` yields `0`.

**Script-observable:** yes — an arbitrary write primitive reachable from script.
No title in the tree calls it directly, but it is UB by Rust rules.

**Current workaround.** None; recorded as unsound. The `get_sample` path is
sound because the `Vec` outlives the call.

**What a rewrite should do.** Keep a per-engine registry of buffers the process
handed to scripts (starting with the `get_sample` scratch), accept only an
address from that registry, and raise a typed error otherwise; or funnel it
through one audited "script-provided pointer" primitive. Preserve the
reference-visible behaviour for the legitimate caller.

---

### TIM-1 — the timer `capacity`/`discardable`/`tag` model is an approximation

**Summary:** our timer posts at most one fire per timer per frame; the
reference bounds entries in a shared event queue and posts several, with
`TVP_EPT_DISCARDABLE` and a per-post `tag` we do not model.

**Defect (approximation, D36's worked example).** The reference
`tTJSNI_BaseTimer::Fire` (`reference/cpp/core/utils/TimerIntf.cpp:69-111`)
computes `cap = TVPLimitTimerCapacity ? 1 : (Capacity == 0 ? 65535 : Capacity)`,
counts queued `onTimer` events with `TVPCountEventsInQueue`
(`:74`, global at `:22`), posts while `cap - count > 0`, and tags each post
`1 + (Counter << 1)` with flags
`TVP_EPT_POST | TVP_EPT_DISCARDABLE | mode` (`:87-100`). Our L3
(`compat/src/natives/visual/timer.rs:117-150`) checks the same capacity but
against its own per-poll posting list, and the L2 registry fires a timer at
most once per `TimerState::advance` and never catches up a late frame, so the
queue depth is at most one per timer. `TVP_EPT_DISCARDABLE` and the `tag` are
not modelled. `DEFAULT_TIMER_CAPACITY` (6) matches
(`reference TimerIntf.cpp:17,32`; `timer.rs:28,302`). `D27`/`D36` deliberately
leave this as a named approximation.

**Evidence.** `reference/cpp/core/utils/TimerIntf.cpp:22,69-111`;
`compat/src/natives/visual/timer.rs:117-150`; migration doc D36/D27.

**Whose bug:** ours (an accepted approximation), not the reference.

**Script-observable:** yes in a narrow range: a script that sets a large
`capacity` and expects several queued fires in one tick, or that observes the
discardable/tag queue semantics, sees a difference. For the common
`capacity = 6` case with one fire per frame it matches.

**Current workaround.** Documented as an approximation until the shared event
queue is modelled; the capacity check is retained so the mapping stays
explicit.

**What a rewrite should do.** Model the reference's shared event queue:
`TVP_EPT_DISCARDABLE`, the post `tag`, `TVPCountEventsInQueue`, and
`TVPLimitTimerCapacity`. Keep the script-visible `capacity` values (default 6,
0 means 65535).

---

### NIS-1 — `native_instance_payload` / `TJS_NIS_GETINSTANCE` is the plugin-object mechanism to keep

**Summary:** native plugins recover another plugin's object through the VM's
native-instance support, keyed by class id — not through a hidden script
property. This is a **feature to preserve**, not a defect.

**Mechanism.** The reference's `TJS_GET_NATIVE_INSTANCE` /
`TJS_GET_NATIVE_INSTANCE_OUTER` macros
(`reference/cpp/core/tjs2/tjsNative.h:298-322`) resolve a native instance from
`objthis` via `NativeInstanceSupport(TJS_NIS_GETINSTANCE, classid, …)`
(`TJS_NIS_GETINSTANCE = 0x00000002`, `tjsInterface.h:38`). Our ABI exposes the
same lookup as `Tjs2Engine::native_instance_payload`
(`crates/tjs/src/lib.rs:2118-2140`) → `tjs2_get_native_instance`
(`crates/tjs/cpp/tjs2_abi.cpp:2197-2225`), which maps a class name to the
registered class id (`native_instance_class_ids`) and calls the same
`NativeInstanceSupport(TJS_NIS_GETINSTANCE, …)`. Consumers:
`natives/rect.rs`, `kag/mod.rs`, `menu_item.rs`, `layer_ex/{draw,image,matrix,movie,perspective}.rs`,
`motion_player/{adaptors,player}.rs`, `sound.rs`, `visual/{bitmap,font,layer,common}.rs`.

**Evidence.** `reference/cpp/core/tjs2/tjsNative.h:298-322`,
`tjsInterface.h:38`; `crates/tjs/src/lib.rs:2118-2140`;
`crates/tjs/cpp/tjs2_abi.cpp:2197-2225`; D37.

**Whose bug:** n/a — reference mechanism.

**Script-observable:** no script sees the mechanism itself, but it is the only
sound way for one native object to obtain another's payload. A rewrite that
replaces it with a hidden script property (the old `__handle` pattern) would
introduce the D37 aarch64 pointer-sign bug.

**Current workaround / status.** Implemented and in use; D37 makes it the rule
("A native plugin member obtains another plugin object through the ABI's
native-instance mechanism …, never through a hidden script property").

**What a rewrite should do.** Keep an equivalent typed handle
(`slotmap`/generation key or a class-id lookup) and delete any hidden-property
shortcut. The payload must stay owned by the VM and be invalidated with the
object; `native_instance_payload` returns null for the class object itself and
after invalidation, which a rewrite must preserve.

---

### STO-1 — `Scripts` / `Storage` semantics that surprised us

These are behaviours that are correct-but-non-obvious or that our port got
wrong and must not reintroduce.

1. **`Scripts.execStorage` / `evalStorage` try the bytecode loader before
   text; `compileStorage` only compiles text.** `TVPExecuteStorage` opens a
   binary stream and calls `LoadByteCode` first
   (`reference/cpp/core/base/ScriptMgnIntf.cpp:713-727`), falling through to
   the text path only when the loader returns `false` (`:783-809`); a
   recognized-but-broken blob raises and does **not** fall through.
   `TVPCompileStorage` always reads text (`:813-843`). The port matches
   (`compat/src/natives/scripts.rs` `engine.load_bytecode` at `:380`). A rewrite
   must keep the loader-first order — a title's `startup.tjs` can be
   precompiled (`TJS2100\0DATA`), which is exactly why the lexer must never be
   handed raw bytecode.
2. **`Scripts.getTraceString()` returns the *current* (catch-site) stack, while
   the throw-time stack is the thrown `Exception` object's `trace` member.**
   `TJSGetStackTraceString` / `tTJSStackTracer::GetTraceString`
   (`reference/cpp/core/tjs2/tjsDebug.cpp:617-660`) walks the live stack;
   the `Exception` object's `trace` member is populated when the object is
   created (`tjsError.cpp:26-51`).
   Migration doc §3.2. A rewrite must keep the two distinct.
3. **`Storages.isExistentStorage` matches for a directory.** The reference
   path is `TVPIsExistentStorage` → `TVPGetPlacedPath` →
   `TVPIsExistentStorageNoSearchNoNormalize` → `tTVPFileMedia::CheckExistentStorage`
   → `TVPCheckExistentLocalFile`, and that function returns
   `s.st_mode & S_IFREG` (`reference/cpp/core/base/impl/StorageImpl.cpp:532-546`)
   — **false for a directory**; the port matches (TODO §17 `F7`).
4. **`TVPCheckExistentLocalFile`'s Windows body is `#if 0`-ed out.** The same
   function contains the original `GetFileAttributes` logic under `#if 0` and a
   Linux port based on `tTVP_stat`. Both return false for directories, but the
   body is a reconstruction signal (REC-1) and the shipped Windows behaviour
   should be confirmed against a real title before it is relied on.

**Whose bug:** mixed — item 3's claim is a documentation error at least;
the others are reference semantics or port choices.

**Script-observable:** items 1 and 2 yes; item 3 yes if a title probes
directory existence; item 4 underlies item 3.

---

### LIFE-1 — `VmHost` drops the C++ engine before the compat state that holds its retained values

**Summary:** a teardown use-after-free: `CompatServices` holds `DetachedValue`s
whose `Drop` calls `tjs2_release_value`, but `VmHost`'s field order drops the
engine first.

**Defect.** `VmHost { engine, input, timer, compat, adapter }`
(`crates/vm/src/host.rs:32-38`) drops fields in declaration order, so the
`Box<Tjs2Engine>` (whose `Drop` calls `tjs2_destroy`) is dropped before
`compat` (the `CompatHost` whose `Box<CompatServices>` owns the state map).
`DetachedValue::drop` calls `tjs2_release_value(self.engine, self.id)`
(`crates/tjs/src/lib.rs:592-598`), and its own doc says "The engine MUST
outlive every `DetachedValue`". Many state structs hold live `DetachedValue`s
not cleared by `CompatHost::uninstall` (ObjectRegistry, WindowInst, LayerInst,
menu keycode caches, windowEx flags, motion/xp3 caches, KAG, sound,
debug, …). A 2-frame headless run released the ObjectRegistry entries during
`Shutdown`, so the reachable-for-real cases are the caches that are never
drained.

**Evidence.** `docs/audits/unsafe-inventory.md` §5.2 (rank 2 before shipping);
`crates/vm/src/host.rs:32-38,168`; `crates/tjs/src/lib.rs:583-598`;
`crates/compat/src/host.rs:16-22,148`.

**Whose bug:** ours (drop order and cache lifetime).

**Script-observable:** only at teardown; a title that touches the un-drained
caches then exits can crash on shutdown. Not a gameplay behaviour.

**Current workaround.** None; recorded. `CompatServices::noop_closure` already
stores a raw id instead of a `DetachedValue` precisely to dodge this.

**What a rewrite should do.** Make the retained-value store own the engine
lifetime (e.g. `DetachedValue` borrows the engine with a lifetime, or the state
map is cleared/owned before the engine drops), and give the host an explicit
`shutdown()` order that drains all caches. The generational-key design removes
this class entirely.

---

### ABI-1 — `tjs2_assign_message` has no `try/catch`; a C++ exception can cross the C ABI

**Summary:** the one C++ entry point without a guard can throw a `boost`
conversion error or a TJS error across `extern "C"`, which is UB, and a Rust
panic at any callback boundary aborts the process because none is guarded.

**Defect.** `tjs2_assign_message` (`crates/tjs/cpp/tjs2_abi.cpp:1921-1923`)
calls `tjs2_assign_message_checked` without a `try/catch`; that path uses
`boost::locale::conv::utf_to_utf` and `TJSAssignMessage`, both of which can
throw. `tjs2_set_data_dir` in `streams.cpp` similarly assigns unguarded. Rust
only ever passes valid UTF-8 (`CString::new` on a `&str`), so it is latent, not
currently reachable. Separately, the ~1 041 `extern "C"` natives have no
`catch_unwind`; a panic aborts (not UB on current rustc, but a script-triggered
DoS).

**Evidence.** `docs/audits/unsafe-inventory.md` §7.2.1, §6, §9 (findings 6–7).

**Whose bug:** ours (port C++ and the callback boundary).

**Script-observable:** no today (all inputs are valid UTF-8), but a future call
path with malformed input would be UB; a script-triggered panic aborts.

**Current workaround.** All other entry points wrap in
`try { … } TJS_CONVERT_TO_TJS_EXCEPTION`; the two above do not.

**What a rewrite should do.** A pure-Rust VM removes the C ABI boundary and
this whole class of defect; if the C++ stays, wrap every `extern "C"` entry in
`catch(...)` and add one `catch_unwind` shim per callback family so a panic
becomes a typed TJS error.

---

### ABI-2 — ABI length fields narrow to `int`/`c_int`; >2 GiB octets truncate

**Summary:** the ABI writes `(int)oct->GetLength()` and reads
`in->array_count` as `int`, so an octet or array over 2 GiB silently
truncates.

**Defect.** `tjs2_abi.cpp:305,488` write `(int)oct->GetLength()`;
`value_to_variant` reads `in->array_count`. `tjs2_value.array_count` is
`c_int`. Implausible from a game, but unchecked.

**Evidence.** `docs/audits/unsafe-inventory.md` §7.2.3 (finding 12).

**Whose bug:** ours (the ABI).

**Script-observable:** only for an octet/array over 2 GiB — effectively no.

**Current workaround.** None; recorded.

**What a rewrite should do.** Use `usize`/`tjs_uint` consistently and
`try_from` for any narrowing, or carry a length-and-buffer pair.

---

### LIFE-2 — the single process-global binding and the null-plugin case are load-bearing and easy to misuse

**Summary:** the C ABI forces exactly one process-wide engine→context binding,
and the ABI's `last_object` slot is a single-value scratch shared by every
call. Both are load-bearing and easy to misuse.

**Defect / trap.** The one documented global is `binding.rs`
(`LazyLock<Mutex<Option<Binding>>>`, migration doc §3.1). It holds dispatch
state only and is installed once, before any native callback can run. The ABI
also keeps one `last_object` per engine (`tjs2_abi.cpp:80`), overwritten by
every object argument (`:469`); a native that retains an argument must use the
per-argument `retained` handle the marshaller records (`:462-476`), because the
next conversion overwrites the slot. `CompatServices` exposes the scene and
storage as raw pointers (`compat/src/ctx.rs` `scene`/`storage`), so every
dereference needs the same single-VM-thread argument the binding has. None of
these are reference defects; they are consequences of emulating a
user-data-less C ABI.

**Evidence.** `crates/compat/src/binding.rs`; `null_plugin.rs` (`install`
keeps the requested stem); `compat/src/ctx.rs`; `tjs2_abi.cpp:80,462-476`;
migration doc §3.1.

**Whose bug:** ours.

**Script-observable:** the lowered global name and `Plugins.getList()` order
are script-visible; the scratch and `last_object` are not directly visible but
can produce wrong receiver/values.

**Current workaround.** The binding is documented and
The `stateless` check guards L1's statelessness; `CompatCtx` is now
`!Send + !Sync`. The case/order issues are recorded in the review and not yet
all fixed.

**What a rewrite should do.** If the VM is in Rust, the binding disappears
(natives become direct calls / trait objects, migration doc §10.3). Until then,
keep exactly one documented global, pass the original plugin name alongside the
normalized lookup key, and own scratch state on the service rather than in a
`thread_local`.

---

## 8. What a rewrite gains, and what it must preserve bit-for-bit

**Gains.**
- A **length-aware string type** (`Vec<u16>`/`&[u16]`) removes the
  interior-NUL truncation trap (STR-1), the empty-string null-`this` UB that
  the port currently patches around (`tjsVariantString.h:219-232`), and the
  `array_count` narrowing (ABI-2).
- **No process-global VM state** makes multi-instance use honest and lets the
  engine be `!Send + !Sync` or genuinely thread-safe (GLO-1), and removes the
  one ABI-forced binding and its traps (LIFE-2).
- **A bounds-checked bytecode loader** removes the null-block crash (BLK-1),
  the short-blob out-of-bounds read (BLK-2), and lets every malformed input
  produce a typed error.
- **One consistent container format and one operand-translation table** with
  round-trip property tests remove the `FE FE` reader/writer disagreement
  (TS-1) and the bytecode export/loader double-translation (BLK-3).
- **A depth-bounded serializer** turns a stack overflow into a typed error
  (SER-1).
- **Generational handles** (`slotmap`) make retained values own the engine
  lifetime and remove the teardown use-after-free (LIFE-1) and the raw-pointer
  sites (VIS-1) by construction.
- A pure-Rust VM also removes the whole `extern "C"` exception/panic boundary
  (ABI-1) and the build's zig/bison/oniguruma dependencies (migration doc §10).

**Must preserve bit-for-bit.**
- The `typeof` spellings and the `void`/`undefined`/absent rules (SEM-1),
  including `null` → `"Object"` and the must-exist read raising.
- The distinguished **`void` vs `null` vs `undefined`** results for native
  members (migration doc §3.2: a member with no value returns `null`, not
  `void`).
- The **native-instance mechanism** (`native_instance_payload` /
  `TJS_NIS_GETINSTANCE`, NIS-1) and D37's rule that a pointer is never a
  sign-checked handle.
- The **loader-first order** of `execStorage`/`evalStorage`
  (`compileStorage` compiles text only) (STO-1.1).
- **`getTraceString()` = current stack** vs the thrown Exception's `trace`
  member (STO-1.2).
- Script-visible **error strings** where a test cites them (e.g. the bytecode
  `"Bytecode read error. File is broken or it's not bytecode file."`) and the
  reference's **string/number conversions** (D34).
- The **legacy container readers** for shipped files even after a new writer
  exists, and the reader-offset contract that keeps port-written files
  reference-readable (TS-1).

The surface lock (`krkr-gate surface-lock`) and both parity reports
(`native-parity`, `plugin-parity`) are the mechanical guard that the
rewrite does not drift the dispatch surface while changing internals.

---

## 9. Unverified / open

These are recorded so the next reader does not mistake a hypothesis for a
finding (D36).

1. **Does removing `Send + Sync` from `Tjs2Engine`/`DetachedValue` compile?**
   A read-only grep found no workspace path that requires the bounds, but that
   is not a build. Needs the main agent's gate. (GLO-1)
2. **Does a real mode-0/1 `FE FE` file exist, and what does it look like?**
   No such sample is in the tree; the in-tree fixtures are port-generated.
   The reader-follow decision (TS-1) is based on the reader being the
   compatibility side, not on a shipped sample.
3. **Does a real KiriKiri title ship a precompiled `startup.tjs`/`system`
   script?** The bytecode path is correct end to end only if a device run logs
   `encoding=precompiled bytecode` and gets past line 1. The round trip is
   self-consistency, not proof. (`docs/audits/bytecode-scripts.md` §3, §5)
4. **Does `TVPIsExistentStorage` report directories as existing?**
   `fs_engine.md` §1.10 says yes; the source path in the tree ends in
   `TVPCheckExistentLocalFile`, which returns `S_IFREG` (false for
   directories). The claim is contradicted by the reconstructed source and
   must be settled by a device script probe before either side is changed.
   (STO-1.3, REC-1)
5. **The 8–19-byte bytecode window (BLK-2) is covered by a wrapper guard, not
   the loader.** `short_magic_prefixed_blob_is_a_typed_error` pins the typed
   error for 8/11/19-byte blobs, but the loader itself still takes no length
   and reads past the buffer for those sizes; the guard is in `tjs2_abi.cpp`.
   A rewrite must move the bounds check into the loader. (BLK-2, BLK-3)
6. **Timer queue fidelity.** Whether any shipped title observes the
   `capacity > 1`, discardable or tag semantics (TIM-1) is unknown; it is an
   accepted approximation, not a verified non-issue.
7. **Does the shipped KiriKiri engine share the snapshot's incomplete bytecode
   export table (BLK-3)?** The snapshot registers only the base arithmetic
   opcodes, so the writer leaves variant operands as byte offsets and the
   loader double-translates them. A shipped `.tjsb` containing a `+=`-family
   or `++`/`--` statement would decide whether the snapshot's writer or reader
   is the lossy side (see BLK-3 for the exact evidence). Until then the port
   follows the snapshot and the divergence is recorded, not hidden.

---

## SEM-2 — a bare `typeof <undeclared identifier>` raises; only the property form reports `"undefined"`

**Summary:** `typeof SomeAbsentGlobal` (a bare identifier, no property
access) raises `Member "SomeAbsentGlobal" does not exist`; it does **not**
return `"undefined"`. The device-log shape `typeof Motion` therefore logs an
exception in the reference engine too, and this is reference behaviour, not a
port defect. The `typeof <obj>.<prop>` / `typeof global.<name>` form is the one
that reports `"undefined"`.

**Finding / trap (not a bug).** The `typeof` operator special-cases its operand
only when it is already a property access (`T_DOT` / `T_LBRACKET` /
`T_WITHDOT`): that path emits `VM_TYPEOFD` / `VM_TYPEOFI` with
`TJS_MEMBERMUSTEXIST`, and `TypeOfMemberDirect` maps `TJS_E_MEMBERNOTFOUND` to
the string `"undefined"`. A bare symbol takes the "normal operation" branch,
which reads the symbol as an ordinary variable first and only then applies
`VM_TYPEOF`. The code generator rewrites a non-local symbol to `this.<name>` /
`global.<name>` and reads it with `VM_GPD` (flags `0`, so not must-exist). The
script global object is a `tTJSCustomObject`, whose `PropGet` returns
`TJS_E_MEMBERNOTFOUND` for an absent member; `GetPropertyDirect` sees
`TJS_FAILED(hr)` and throws through `TJSThrowFrom_tjs_error`, producing
`Member "<name>" does not exist`. A bare absent name therefore can never reach
the `"undefined"` arm — the read raises first. This is the same must-exist read
rule that makes a bare `undefined` raise (SEM-1), and it is deliberate TJS2
semantics (`TypeOf`'s comments say the spellings "differ from TJS1").

**Evidence.**
- Only property-access operands get the safe path:
  `reference/cpp/core/tjs2/tjsInterCodeGen.cpp:1805-1812` (`haspropnode`),
  `:1815-1839` (bare path emits `VM_TYPEOF` after an ordinary read).
- A non-local symbol is rewritten to `this.<name>` / `global.<name>` and read
  normally: `reference/cpp/core/tjs2/tjsInterCodeGen.cpp:2320-2345`; `stNone` →
  `VM_GPD`: `:2039-2049`.
- `VM_GPD` passes flags `0` and throws on failure:
  `reference/cpp/core/tjs2/tjsInterCodeExec.cpp:1166-1167`, `:1400-1428`.
- The global object rejects absent members: `tTJSCustomObject::PropGet`,
  `reference/cpp/core/tjs2/tjsObject.cpp:1276-1310`
  (`return TJS_E_MEMBERNOTFOUND` at `:1309`).
- The error text: `TJSThrowFrom_tjs_error` → `TJSMemberNotFound`
  (`Member "%1" does not exist`), `reference/cpp/core/tjs2/tjsError.cpp:254-268`;
  message declaration `reference/cpp/core/tjs2/tjsError.h:264`.
- The safe path: `TypeOfMemberDirect`'s `"undefined"` arm,
  `reference/cpp/core/tjs2/tjsInterCodeExec.cpp:1865-1904`.
- The reference's own `motionplayer` probes a **member**, not a bare name:
  `reference/cpp/plugins/motionplayer/main.cpp:505`
  (`typeof Motion.Player.useD3D`).
- Pinned by `crates/compat/tests/typeof_spellings.rs`
  `bare_undeclared_global_typeof_raises` (property form:
  `undefined_is_absence_and_void_is_unset`).

**Whose bug:** n/a — reference semantics, to keep. Not a defect.

**Script-observable:** yes, directly. A top-level `typeof <undeclared global>`
dumps an exception (caught or ignored by the script) in both engines; a script
that wants a silent feature probe must use `typeof global.<name>` or
`typeof <obj>.<member>`. The scripts then continuing is consistent with the
exception being caught or ignored at startup; it is not evidence of a port
defect.

**Current workaround.** None needed: we match the reference. Note that the
device-log names (`Motion`, `Player`, `D3DEmotePlayer`, `D3DEmoteModule`,
`ShortCutInitialPadKeyMap`) are published **unconditionally** by the port's
`motionplayer.dll` port at install time
(`crates/compat/src/natives/plugins/motion_player/mod.rs:298-424`,
`crates/compat/src/natives/plugins/mod.rs:64-225`), so on a current build those
specific `typeof` reads resolve and do not raise. A device that still logs them
is running a build without those registrations (or before them); investigate
plugin registration, not `typeof`.

**What a rewrite should do.** Preserve the split exactly:
`VM_TYPEOFD` / `VM_TYPEOFI` (property access) returns `"undefined"` for
`TJS_E_MEMBERNOTFOUND`, while a bare identifier does an ordinary must-exist read
and raises. Do not make `typeof <bare name>` silently return `"undefined"` the
way JavaScript does (SEM-1).
