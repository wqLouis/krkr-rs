# 08 — TJS VM, compatibility surface, and native plugins

Status: implemented (the TJS2 Rust port in §10 remains deferred).
Related: [`02-target-architecture.md`](02-target-architecture.md) (layers),
[`09-game-loading.md`](09-game-loading.md) (loading).

---

## 1. Placement

```
L4  vm       TJS VM host over tjs              (topmost)
L3  compat   native classes + KAG + event adapter + plugins
L2  engine   generic game engine (layer system, audio, VFS)
L1  platform window/host, fs + archive parse, render, media
```

The VM is **not** the host. The event loop is platform code and lives in
**L1**. L4 executes scripts and runs the per-frame body; L3 adapts between
the C++ VM and the L2/L1 engine. Nothing above L1 contains platform
`#[cfg]`.

## 2. L4 — the VM host (`vm`)

Owns:

* the `Tjs2Engine`, the log callback, bytecode compilation and the script
  cache;
* one VM per process, **single-threaded**;
* the per-frame body (the polls, §6).

Uses from L1: the clock, threads, and savedata/state IO — through the L1
platform abstraction, so L4 has no OS-specific code.

Small host API (illustrative):

```rust
impl VmHost {
    fn with_default() -> Result<Self, VmError>;          // built-in plugin registry
    fn new(registry: PluginRegistry) -> Result<Self, VmError>;
    fn engine(&self) -> &Tjs2Engine;
    fn compat(&self) -> &CompatHost;
    fn exec(&self, source: &str, name: &str) -> Result<TjsValue, VmError>;
    fn eval(&self, expression: &str, name: &str) -> Result<TjsValue, VmError>;
    fn tick(&mut self, frame: FrameCtx<'_>) -> Result<(), VmError>;
}
```

## 3. L3 — the compatibility surface (`compat`)

Sub-parts:

1. **binding glue** — marshalling TJS values ↔ Rust and registering classes
   through the `tjs` `NativeClassBuilder`;
2. **native classes** — the KiriKiri-visible object model (`System`,
   `Storages`, `Scripts`, `Window`/`Layer`/`Bitmap`/`Font`/`Timer`,
   `SoundBuffer`, `VideoOverlay`, `Plugins`, `Debug`, `AsyncTrigger`,
   transitions, menu, …);
3. **KAG** — parser + layer semantics;
4. **event adapter** — §5;
5. **plugins** — §4.

L3 is **thin glue**. TVP behaviours that are a combination of primitives
L2 already exposes are composed in L3 rather than pushed into L2. The
generic **layer system (scene graph) is L2**; L3 adds the TVP semantics
(masks, province images, exact property names, focus/hit-test,
screen-buffer primary layers, storage naming) on top of it.

### 3.1 The ABI constraint — the one allowed global

The C++ ABI hands native callbacks only the raw `tjs2_engine*`; `tjs`
exposes **no user-data slot**. The old `tvp-*` code worked around this with
process-global `LazyLock<Mutex<…>>` contexts. We keep exactly **one**
process-wide binding — an engine → context table in `compat` — because
the ABI forces it. It is L3 (never L1), it holds *dispatch* state (a
reference to the L2 session/engine), never game state, and it is documented
here. **D9 (no globals) still holds for L1.**

### 3.2 Error messages carry the script location

`tjs2_exec_script`/`tjs2_eval` format a caught `eTJSScriptError` as
`<block>(<line>): <message>` (C++ `make_error_message`), so a host-side
failure names the failing script and line instead of only the raw message —
e.g. `startup.tjs(1681): … AffineLayer.tjs(451): Cannot convert …`. The
throw-time stack is the script `Exception` object's `trace` member;
`Scripts.getTraceString()` returns the *current* (catch-site) stack instead.

When a native returns a result, remember that TJS distinguishes **`void`**
from **`null`**: a script guard like `if (ret != null) ret.mode` passes for
`void` and then throws on the member access. A member with no value returns
`null` (`set_null_out`), not `void`.

### 3.3 Script loading

`Scripts.execStorage`/`evalStorage` read the named script through the L2
storage and offer the raw bytes to the TJS2 **bytecode loader first**
(`TJS2100`); only when the loader reports the input is not bytecode is it
decoded as text, which is the reference `TVPExecuteStorage` order
(`reference/cpp/core/base/ScriptMgnIntf.cpp:691-699,713-760`). Text decoding
handles the `FE FE` cipher / zlib container, a UTF-8/UTF-16 BOM, an explicit
`Scripts.textEncoding` override, then UTF-8 and CP932; an undecodable script
raises a typed error instead of producing mojibake. A debug-only `script
read` diagnostic logs the storage name, byte length, first 32 bytes and the
encoding decision, so a device log can identify the raw bytes without dumping
the script. `compat::exec_storage` is the core of `Scripts.execStorage`
without the native wrapper; the composition root uses it to run the startup
sequence.

## 4. Native plugins

### 4.1 Static, built into L3 (D14)

We build native plugins **into the binary**. (A WASM host was considered and
deferred, §4.3.) A plugin is a named unit:

```rust
pub trait NativePlugin {
    /// The name scripts use: "menu.dll", "windowEx.dll", "fstat.dll", ...
    fn name(&self) -> &str;
    /// Register the plugin's native classes on the VM.
    fn register(&self, ctx: &mut CompatCtx) -> Result<(), CompatError>;
    /// Optional per-frame hook (async triggers, video overlay, timers).
    fn poll(&self, _ctx: &mut CompatCtx) -> Result<(), CompatError> {
        Ok(())
    }
}
```

The built-in set is the TVP core classes plus emulations for the DLL
plugins the shipped games reference (`menu.dll`, `windowEx.dll`,
`fstat.dll`, …). Registration happens at startup; `ctx` carries the L2
services explicitly, so plugins own no hidden state.

The ported set lives in one table, `natives::plugins::BUILTIN_PLUGINS`,
each entry mapping a canonical DLL name (plus its aliases —
`layerExDraw.dll`/`layerExMovie.dll`/`perspective.dll` share a module) to
that module's `register`. The same table feeds `PluginRegistry::builtin()`,
so `Plugins.link(name)` recognizes **every** DLL we have ported;
`PluginRegistry::{plugin,is_builtin}` query the table. `link` returns `void`,
exactly as the reference (`PluginImpl.cpp:300` → `TVPLoadPlugin(name); return
TJS_S_OK;`).

One plugin is registered with a deferred half. `xp3filter.dll` is part of
`BUILTIN_PLUGINS`, but its `setXP3ArchiveExtractionFilter`/`ContentFilter`
wrappers need the L2 storage, which the composition root attaches after
`CompatHost::install`; `CompatHost::activate_deferred_plugins` completes the
read of `xp3filter.tjs` and installs the L2 filter seam **before** the
startup scripts run. This mirrors the reference, which loads
`xp3filter.dll` before any game script. A separate mechanism is the
reference's `tvpLoadPlugins` scan: the composition root discovers `*.tpm`
files in the exe directory, `<exe>/system` and `<exe>/plugin`, sorts them by
name and calls `PluginRegistry::autoload`, which maps `foo.tpm` to `foo.dll`
and links it (`PluginImpl.cpp:113-177`).

### 4.2 Missing DLLs → the null plugin object

Linux and Android cannot load the Windows DLLs the games name. Behaviour of
`Plugins.link(name)`:

* **built-in emulation exists** → activate it (the classes are already
  registered, so the link is recorded and reported as built-in);
* **otherwise** → install a **null plugin object** that:
  * accepts *every* member access and call,
  * returns void/undefined and does nothing,
  * logs a `warn!` per call and a `debug!` with the call name and arguments;
* **never throws.**

Rationale: games call `Plugins.link` for plugins we cannot load; throwing
would make the game completely unplayable. This is the **single documented
exception to D11** (no silent fallback) — and it is *not* silent: every
call logs a warning. The null path is only reached for a DLL we have **not**
ported: every ported DLL (including `extrans.dll` and `wuvorbis.dll`, whose
surface lives in L2/engine and the host audio chain rather than in a native
class) resolves to the built-in table above.

### 4.4 Unported plugins and the DLL-surface pipeline

A plugin we have not ported has no `reference/cpp` source. Where the
reference is a binary, the DLL is the oracle and is cited by its build hash
(D38): `plugins/<n>/plugin/<Name>.dll <sha256[:12]> @ <decompiled function or
address>`. The extraction pipeline and its measured fidelity live in
`plugins/analysis/`, validated against `layerExDraw.dll` (233 of 234 members
matched, the two differences being real build differences). The DLLs are
gitignored and never loaded by a test: the extracted surface is committed as
a derived fixture and the test gates on that. Bringing the 36 unported plugin
names to native coverage is the next major version (`TODO.md` §10).

### 4.5 WASM (future)

If third-party plugins must actually execute, a WASM host can be added
behind the same `NativePlugin` interface later. Out of scope for the first
milestone.

## 5. Event adaptation (L1 → L3 → L4)

```
platform event   →  L1 platform (winit / android-activity)
                 →  L1 PlatformEvent
                 →  L3 adapter (map to a TJS-visible event)
                 →  L4 VM (dispatch to script handlers)
```

L2 keeps hit-test/focus state; L3 maps raw events to the TJS event surface.
No `#[cfg]` above L1.

## 6. Frame body and poll order

L2 owns the frame (`engine::Engine::step`/`frame`). It advances the timer
registry, runs the `GameFrame` hook, then projects and renders. The L4 VM tick
and the L3 polls are the hook body:

```
L2: advance timers → invoke their callbacks (L3 posts onTimer)
L4/L3 hook: vm.tick (input begin → events → input end)
            → drain the posted onTimer events
            → sound_poll → video_poll → async_trigger_poll
            → continuous_handler_poll → idle compaction
            → frame_tail (movie poll + layer transitions)
            → audio drain
L2: overlay refresh → upload → project → L1 RenderPipeline::render
```

The mixer, video and transition pumps still run in the hook; moving them into
L2 is the rest of the frame phase (see `07-roadmap.md` Phase 9).

## 7. Implementation status

The move is complete: the `tvp-*` native families are folded into `compat`
as `plugins/*`, the L2 scene/bitmap layer system is `engine`, the sound model
is `engine::sound` over the L1 audio device, text raster is `render` with
layout/composition in `engine`/`compat`, and the KAG parser is `compat/kag`.
The VM host is `crates/vm` (`VmHost`, `FrameCtx`). There is no `tvp-*` crate
left.

## 8. Testing

* Marshalling and the null-plugin object are unit-testable with no window.
* The VM host is exercised with small script fixtures.
* Native parity is tracked by `scripts/native_parity.py` against
  `reference/cpp`; plugin parity by `scripts/plugin_parity.py`. Both are
  reports of coverage, not correctness claims (D36).

## 9. Native class structure: share the implementation, register the real surface

The C++ reference gives each class a **distinct** member set (`Layer` 150,
`Window` 83, `Bitmap` 20, `Font` 22, `Timer` 6) even though they share a large
common implementation (the `tTVPBaseBitmap` / native-base node). The
registered surface is now per class and matches the reference: native parity
is 100% with empty allowlists, plus 2 intentional extras
(`KAGParser.multiLineTagEnabled`, `paramMacros`; `docs/native_parity.md`),
and the surface lock (D29) blocks additions and removals.

Target design (do **not** mirror the C++ classes one-to-one):

* **One shared implementation** for the common node behavior. The L2 `Scene`
  accessors already are that implementation; keep the L3 dispatch functions and
  macros shared rather than copied per class.
* **Register each class's real surface**, per `docs/native_parity.md`, not the
  union. Where two classes genuinely share a member, share the Rust function /
  macro — never the registration list.
* Optionally express the common surface once as a **TJS base class** and
  `extends` it (the ABI already supports `class X extends Y`). The reference does
  not use TJS inheritance for these classes, so prefer per-class registration
  unless a base class proves cleaner.
* Consequence: the parity "extra" column becomes truthful (today ~567 of 613
  extras are this over-registration) and the L3 source shrinks substantially.

This also lets classes that perform the *same action* share one implementation
rather than each re-wrapping the C++ shape — the guiding rule is "model the
script-visible behavior, not the C++ class tree".

### Shared behavior structs (the concrete pattern)

The way to do this is a **shared behavior struct** that each native class
*composes* instead of re-implementing:

* **Audio:** one `Audio` core owns buffering, volume/pan, loop, fades, the
  play/pause/stop state machine and the event queue. That core already exists as
  the L2 `engine::sound::{SoundSource, SoundBuffer, SoundChannel, Mixer}` model.
  `WaveSoundBuffer`, `CDDASoundBuffer`, `MIDISoundBuffer` are then thin L3
  classes that differ only in how the source is obtained (file decode / CD track
  / MIDI sequence — a host seam) and in which members they register. One
  implementation, several thin registrations.
* **Visual nodes:** one node core — the L2 `Scene` + `LayerState`/`BitmapState`
  accessors — is the shared behavior; `Window`/`Layer`/`Bitmap`/`Font`/`Timer`
  are thin classes that register only their own surface and delegate to it.
* **General rule:** put the reusable behavior in the **lowest** layer that can
  own it without TVP specifics (usually L2); the L3 class is a thin adapter that
  registers the script surface and calls the shared struct. When a struct is
  genuinely TVP-specific it still lives **once** in L3 and is composed by the
  classes that need it (e.g. the `Timer`/`AsyncTrigger` registries, the KAG
  parser).

### L3 must stay thin (D16)

L3 is a VM↔engine adapter, **not a second engine**:

* Every native member should reduce to *marshal args → one L2 call → marshal the
  result*. If a member contains real logic (loops, state machines, blending,
  timing, scheduling), that logic belongs in L2 unless it is genuinely
  TVP-specific.
* Express the marshalling **declaratively**. The `layer_int_prop!`-style macros
  (one line per member) are the model; generalizing them across the families so
  a member is a table entry — not a hand-written 40-line trampoline — is the
  single biggest lever on L3's size.
* Audit signal: if an L3 file is larger than the L2 module it wraps, the
  abstraction is leaking upward. Current suspects: `visual/layer.rs` (2.4k),
  `system.rs` (2.2k), `video.rs` (2.2k), `sound.rs` (2.0k).
* **Genericity test (don't over-abstract either).** When moving behavior into
  L2 it must be expressible with **no TVP vocabulary**. If the only way to place
  something in L2 is to name a TVP concept — an `lt*` blend type, a VK key code,
  a `.sli` field, the `0..100000` volume scale, a KAG tag — it stays in L3. L2 is
  a generic engine (layer/scene graph, audio, video, VFS, timers), **not** a TVP
  compatibility layer. A TVP-shaped L2 API is just as wrong as a fat L3.

### Follow-up refactors (D16)

1. **Sound:** move the `.sli` format (`parse_sli`/`SliInfo`/`WaveLabel`/
   `LoopLink`) and the `TVP_*` constants out of `engine::sound`; L2 keeps only a
   generic loop/label model that L3 populates.
2. **Layer:** `LayerState.blend` is the renderer's generic
   `api::blend::BlendMode` (done); the native `lt*` numbering and the
   int↔enum mapping live in L3 (`natives/visual/layer/blend.rs`).
3. **Menu:** do **not** add a menu-specific L2 model. The menu is just another
   use of the engine's existing **generic tree** (the node / parent / children
   structure the layer system already provides); the L3 `MenuItem` tree is built
   on those primitives, with `HMENU`/`keycodeToText` staying in L3. General rule:
   L2 exposes a few *generic* primitives (tree/node, layer, sound, video, VFS,
   timer); L3 composes them — we do **not** add an L2 abstraction per TVP
   concept.
4. **Prune** native members the reference does not expose (in progress).
5. **Declarative marshalling:** generalize the `*_prop!`/`*_method!` macros across
   the families so a member is a table entry, not a hand-written trampoline.
6. **Win32 emulation shim (D18):** the Windows-native members
   (`Window.HWND`/`drawDevice`/`registerMessageReceiver`, the `System` Win32
   members, the message loop) must return **emulated** values/handles — a
   synthetic `HWND`, an emulated draw device, an emulated message receiver — not
   typed errors, so a game that probes Win32 sees a plausible Windows.

## 10. TJS2 Rust port audit (merged from `12-tjs-rust-port-audit.md`)

Status: **audit + feasibility, no code changed.** **Deferred by D17**: the Rust
port is not a first priority; the C++ VM stays until a real game runs
end-to-end. Question: can we reimplement
the TJS2 scripting VM (currently the vendored C++ in `crates/tjs/cpp/tjs2`)
in pure Rust, so the build collapses to `cargo build` and the C/C++/bison/zig
"dev deps" disappear?

### 10.0. Is TJS just JavaScript? (Could we use QuickJS?)

No — TJS2 is a **C/Java/JS-family embedded scripting language with its own
semantics**, not JavaScript. It *looks* familiar (`var`, `function`,
`if/for/while/switch`, `try/catch/finally`, `this`, `new`, `class … extends …
super`, object/array literals, `typeof`, `instanceof`, `in`, `with`, `delete`),
which is why it reads like a QuickJS-style embedded VM — but the grammar and
runtime diverge enough that **no JS engine (QuickJS, Boa, rquickjs, V8) can run
it**.

TJS-only reserved words (from `tjsLex.cpp`): `int`, `real`, `string`, `octet`,
`getter`, `setter`, `property`, `global`, `incontextof`, `invalidate`, `isvalid`,
`synchronized`, `goto`, `enum`, `public`/`private`/`protected`/`static`, and
`NaN`/`Infinity` as keywords.

Notably **absent vs JS**: `let`, `of`, arrow functions, template strings,
destructuring, spread, generators / `async`/`await`, and `undefined`.

Semantic differences that would break real scripts:
- an explicit **int/real** numeric split (integer division, overflow, bit ops —
  JS has only doubles);
- the **`octet`** binary-buffer type;
- explicit **`getter`/`setter`/`property`** declarations and Java-style class
  access modifiers;
- **`incontextof`** (this-rebinding call), **`isvalid`/`invalidate`**
  object-lifetime calls, **`global`**, **`synchronized`**;
- a different **`typeof`** result set and a different **stdlib** (`Array`,
  `Dictionary`, `RegExp`, `Date`, `Math`, `Octet`, …) with a **`delegate`**
  mechanism.

Consequences:
- **Drop-in JS engine:** impossible — wrong language.
- **Transpile TJS2 → JS to run on QuickJS:** needs a full TJS2 front-end *plus*
  emulation of TJS semantics (int/real, octet, classes, delegates) and a stdlib
  shim — strictly more work and lossier than porting the VM, and QuickJS is C
  (another C dependency, the opposite of the goal).
- The resemblance **helps** the Rust port: the parser is a familiar C-family
  recursive-descent/Pratt job and the VM is a register machine, so the syntax is
  not the risk — regex/string/class semantics are (§4).

### 10.1. What we build today (dependency inventory)

`crates/tjs` compiles ~**41 k lines of C++** (37 `.cpp` + 39 `.h`) plus three
**bison** grammars, and links a C **oniguruma** regex library. `build.rs`
(900+ lines) orchestrates the whole thing. The tool/deps pulled in *only* to
produce the VM:

| Dependency | Why | Removable by a Rust port |
|---|---|---|
| **zig** (`zig c++` + package manager) | C++ compiler + fetches deps | ✅ yes |
| **bison** | generates `tjs.y` / `tjsdate.y` / `tjspp.y` parsers | ✅ yes |
| **python3** | `create_world_map.py` → `tjsDateWordMap.inc` | ✅ yes |
| **oniguruma** (C, ~50 sources) | TJS `RegExp` | ⚠️ replace with a Rust regex engine (parity risk) |
| **fmt / spdlog / boost** (header C++) | string formatting/logging in the vendored sources | ✅ yes |
| **libc++ / libc++abi** | C++ runtime at link | ✅ yes |
| C++ compiler + arch guard | everything above | ✅ yes |

So a full port removes essentially **all** of the current dev deps; the only
remaining native dependency in the project would be the **Android NDK** (and only
for Android builds of `platform`/`media`), which is unrelated to the VM and
unavoidable on that target.

Current Rust side: `crates/tjs/src/lib.rs` = **4 724 lines** of C-ABI binding
(`Tjs2Engine`, native-class registration, value retain/release, the
`tjs2_*` `extern "C"` surface) plus the 2 519-line `tjs2_abi.cpp` shim.

### 10.2. Component map (C++ → Rust)

| Component | C++ (approx LOC) | Rust port notes |
|---|---|---|
| Lexer | `tjsLex.cpp` 1 895 | straightforward; hand-written scanner |
| Grammars/parsers | 3 bison grammars + `tjsInterCodeGen.cpp` 4 125 + `.h` 872 | largest single piece; port grammar to `lalrpop`/`chumsky`/hand-rolled recursive descent, or keep a generated parser |
| Bytecode VM | `tjsInterCodeExec.cpp` 3 123 | register VM; internal bytecode, so we are free to redesign (no wire-compat needed) |
| Object model | `tjsObject.cpp` 2 023, `tjsNative`, `tjsArray.cpp` 1 884, `tjsDictionary.cpp` 808, `tjsNamespace`, `tjsObjectExtendable` | the semantic heart: member lookup order, `Class`/`delegate`, `with`, property getters/setters |
| Variant/string | `tjsVariant.{h,cpp}` 2 596, `tjsVariantString.cpp` 1 048, `tjsString`, `tjsOctPack.cpp` 1 045 | numbers (int/real), string interning, octet type |
| Regex | `tjsRegExp.cpp` → oniguruma | **hardest parity item** (see §4) |
| Serialization | `tjsBinarySerializer.cpp`, `tjsByteCodeLoader.cpp` | only needed if we must load precompiled `.tjs`/OctPack; source games don't |
| Date | `tjsDate.cpp`, `tjsDateParser.cpp` | locale/format source of truth |
| Math/RNG | `tjsMath.cpp`, `tjsMT19937ar-cok.cpp` | MT19937 must match exactly (reproducibility) |
| Script block/cache | `tjsScriptBlock.cpp` 921, `tjsScriptCache.cpp` | |
| Interface/errors | `tjsInterface.cpp`, `tjsNative.cpp`, `tjsError.cpp`, `tjsException.cpp` | |
| ABI shim | `tjs2_abi.cpp` 2 519 | **deleted**; natives become direct Rust calls |

### 10.3. Why port (benefits)
- **One toolchain.** `cargo build` only; no zig/bison/python/boost/oniguruma.
- **Cross-compilation.** Android no longer needs NDK clang + bison + a C++
  stdlib *for the VM* (only `platform`/`media`'s NDK C APIs remain). Today the
  VM build is the fragile part of the Android story.
- **Safety & debuggability.** No `extern "C"` boundary, no C++ UB (the vendored
  tree already needed `-O2`-unsafe UB fixes), real stack traces, `cargo test`.
- **Smaller/faster.** No libc++ link; native calls become direct.
- **Integration.** Natives register as Rust trait objects, not C function
  pointers; the one process-global ABI binding (D15/`binding.rs`) disappears.

### 10.4. Hard parts / risks
1. **Regex parity.** TJS uses **Oniguruma** semantics (backreferences,
   lookaround, `\p{}`, Oniguruma-specific syntax). Rust options:
   `regex` (no backrefs/lookaround) ✗; `fancy-regex` (backtracking, backrefs,
   lookaround — close, not Oniguruma-exact); `regress` (ECMAScript) ✗;
   `pcre2` (still C). **Recommendation:** start with `fancy-regex` and keep an
   `onig` feature as the escape hatch; add parity tests for the regex subset the
   test title actually exercises.
2. **String semantics.** TJS strings are UTF-16-ish (`tjs_char`) with specific
   comparison/case/member-name rules; Rust `String` is UTF-8. A port must define
   indexing and `charAt`/`length` semantics to match TJS, not Rust. Big parity
   surface; needs a dedicated conformance suite.
3. **Member lookup / class semantics.** Order of `Class` instance → `delegate`
   → `extends`, `with` scoping, getter/setter invocation, exception class
   hierarchy. Subtle, and scripts depend on it.
4. **Bytecode / OctPack.** Internal, so a Rust VM can use its own IR — *unless* a
   title ships precompiled bytecode/OctPack. The test title ships source; verify
   the target corpus before dropping `tjsByteCodeLoader`/`tjsOctPack`.
5. **Number formatting / date.** TJS `%`-formatting and the date parser are
   byte-exact outputs scripts may compare.
6. **Effort.** ~35–40 k lines of Rust to reach parity, plus a conformance
   corpus. This is a multi-week/month project, not a refactor.

### 10.5. Rust crate inventory (stand on existing crates)

Do **not** reimplement the mechanical parts. All crates below are current and
heavily used (crates.io, checked 2026-10):

| Component | Crate | Why |
|---|---|---|
| Lexer | **`logos` 0.16** (85 M dl) | derive-based DFA, very fast; keywords/operators/numbers/strings. The regex-literal vs division ambiguity needs lexer modes/callbacks — if that gets awkward, a hand-written scanner tuned like `tjsLex.cpp` is the safe fallback. |
| Parser | **`lalrpop` 0.23** (72 M dl) *or* **`chumsky` 0.13** (29 M dl) *or* hand-written Pratt | `lalrpop` mirrors the existing bison LALR grammar most directly; `chumsky` gives better errors; a hand-written **Pratt** expression parser + recursive-descent statements is fastest and keeps TJS's regex/semicolon quirks exact (recommended). |
| Regex | **`fancy-regex` 0.19** (249 M dl) primary; `onig` (C) as a feature fallback | Backreferences + lookaround (Oniguruma's common features). Lacks possessive/atomic quantifiers, so keep `onig` behind a feature for the rare script that needs full Oniguruma. `regress` is ECMAScript-flavoured — not an Oniguruma substitute. |
| String interning | **`lasso` 0.7** (13 M dl) or **`string-interner` 0.20** (38 M dl) | TJS interns strings (`tjsGlobalStringMap`); interning makes member lookup/compare O(1) on handles. |
| Object/value/frame handles | **`slotmap` 1.1** (112 M dl) | Generational keys → safe handles with cheap insert/remove, no `unsafe` pointer juggling (replaces much of the C++ bookkeeping). |
| Member/property tables | **`indexmap` 2.14** (1.6 B dl) + **`rustc-hash` 2.1** (862 M dl) | TJS preserves member order and looks members up constantly: `IndexMap<K,V,FxBuildHasher>` = insertion order + fast non-crypto hash. V8-style hidden classes are a later step. |
| AST / bytecode / constants | **`bumpalo` 3.20** (624 M dl) | Arena-allocate the AST and constant pool; freed at once, no per-node `Box`. |
| Number ↔ string | **`ryu` 1.0** (1.2 B dl) + **`lexical-core` 1.0** (159 M dl) | Shortest round-trip float formatting (what scripts compare) + fast parse. |
| MT19937 RNG | **`rand_mt` 6.1** | Must match TJS's MT19937 exactly for reproducible `Math.random`/seeding. |
| Object GC / cycles | **`slotmap` arena + explicit mark/sweep**, or **`gc-arena` 0.7** / **`dumpster` 2.1** | TJS is refcounted and cycles need a collector. An explicit mark/sweep over a `slotmap` arena fits the D9 philosophy best. |
| Interpreter | hand-written dispatch over a `Vec<Instr>` (frames in a `slotmap`) | Fastest and simplest; avoid `dyn` in the hot loop. |
| Unicode | **`unicode-segmentation` 1.13** (591 M dl), **`simdutf8` 0.1** (239 M dl) | TJS string semantics are UTF-16 code-unit based — settle the internal representation first (§4.2). |
| Keyword/member fast paths | **`aho-corasick` 1.1** (1.2 B dl) | Only if a scan-based fast path is needed. |

**Perf levers** (crates + design): member lookup on interned handles with
`FxHashMap`/hidden classes; `slotmap` handles instead of `Rc<RefCell>` in hot
paths; arena AST/bytecode; a `match`-on-opcode loop (compiles to a jump table);
and small inline values (int/real inline, string as an interned handle), exactly
like the C++ `tTJSVariant`.

### 10.6. Strategy (if we proceed)
1. **Differential harness first.** A test rig that runs the same script on the
   C++ VM and the Rust VM and compares value dumps/errors. Keep the C++ VM
   behind a `tjs-cpp` feature until parity; both implement the same `TjsEngine`
   trait. This de-risks every phase.
2. **Port bottom-up:** `Variant`/string/object model + native registration →
   lexer → parser/AST → compiler/IR → VM → stdlib (regex/date/math/serialize).
3. **Parity gates per phase** against the real test title plus a script corpus
   (the existing `crates/tjs/tests` + new cases).
4. **Cut over** when parity holds: delete `cpp/`, `build.rs`, `deps/`,
   `scripts/vendor-tjs2.sh`, and the `tjs-cpp` feature; drop the dev deps.

### 10.7. Recommendation
Feasible and high-value for build hygiene + Android, but it is the **single
largest remaining work item** in the project — larger than all of L1/L2
combined — with regex and string/class semantics as the real risk. Suggest:
- **Do the differential harness + a thin Rust `TjsEngine` trait now** (small,
  unlocks the port incrementally and immediately improves testability).
- Treat the full port as its own milestone, ported component-by-component,
  rather than a big-bang rewrite.
- Until then, keep the C++ VM; it works and is tested end-to-end on the real
  title.

## 11. The dispatch surface is the contract (D29)

Everything the TJS VM can reach — which classes exist and which members they
expose — is the **frozen contract** of this layer. It is measured by
`crates/compat/tests/runtime_surface.rs`, which asks the *live* VM what
`CompatHost::install` actually registered (rather than parsing Rust source) and
dumps it to `target/runtime_surface.json`, locked against the committed
baseline `docs/surface/runtime_surface.json`:

```bash
python3 scripts/surface_lock.py            # regenerate + check; exit 1 on any diff
python3 scripts/surface_lock.py --update   # accept a deliberate change
```

What that buys:

* **L3 is freely refactorable.** Any change that leaves the dump byte-identical
  cannot change how a call reaches a native, so internals, state layout and file
  organisation above this line are open ground.
* **Invented surface is blocked mechanically.** Adding a member the reference
  does not have changes the dump, fails the gate, and forces a new baseline into
  the same review — where the addition appears as a diff, not as a claim in a
  report.
* **Silent breakage is blocked too**: a member that disappears would fail
  dispatch at runtime, and it fails the same gate.

This is deliberately *not* a completeness measure. Parity against
`reference/cpp` answers "what is missing?" and stays a **report**; it counts
members present, not behaviour correct. The lock answers the different
question, "what changed?", and behaviour is constrained separately by D28:
every semantic must be reference-cited.

