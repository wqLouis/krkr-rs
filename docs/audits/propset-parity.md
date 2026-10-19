# `PropSet` parity audit — the registration mechanisms the extractors missed

- Repo: `/mnt/DATA/Document/code/krkr-rs`
- Date: 2026-10-11
- Scope: the reference C++ registration mechanisms that make a name
  script-visible, and how the `native-parity` and `plugin-parity` checks
  extract them. **No `cargo`/`rustc`/`clippy` was
  run** (AGENTS §0); only `python3`, `grep`, and the guard scripts.
- House rules referenced: AGENTS §4 (no silent fallbacks), D29 (surface lock),
  D30 (tests cite the reference), D36 (a compatibility claim names its
  evidence).

---

## 1. Executive summary

A real device failure, `Member "menu" does not exist`, was a missing core
member the parity reports did **not** flag: the reference installs
`Window.menu` by calling `PropSet(TJS_MEMBERENSURE, …)` on the **already-built
Window class object**

```cpp
// reference/cpp/core/base/ScriptMgnIntf.cpp:538-542
auto *gWindowMenuProperty = new WindowMenuProperty();
val = tTJSVariant(gWindowMenuProperty);
gWindowMenuProperty->Release();
windowclass->PropSet(TJS_MEMBERENSURE, TJS_W("menu"), nullptr, &val, windowclass);
registerObject(TJS_W("MenuItem"), TVPCreateNativeClass_MenuItem());
```

Both scripts extracted the reference surface only from the constructs they
recognised (`TJS_BEGIN_NATIVE_*` blocks and the ncbind macros). Neither read
`PropSet`, so `Window.menu` — and three sibling class-level members — were
absent from the reference table itself. The reports said **0 missing / 100 %
coverage** while the member was missing. A coverage figure that omits a whole
registration mechanism is worse than no figure: it reads as evidence.

This audit does three things:

1. Surveys every way a name becomes script-visible in `reference/cpp/**`
   (§2) and states which mechanism each script saw (§2.3).
2. Teaches both extractors the class-level `PropSet` mechanisms, with a
   fixture self-test that fails if a regex silently stops matching (§4, §6).
3. Re-runs both scripts and triages every newly visible member (§5). No
   genuinely missing member remains on either side after the change; the one
   historical gap (`Window.menu`) is present in the port.

The extractor counts moved **native 636 → 640 reference members** and
**plugin 972 → 974**, both at **100 % coverage**. Coverage was already 100 %;
what changed is that the denominator now includes the members the old
denominator omitted.

---

## 2. Mechanisms survey

### 2.1 The mechanisms, with counts and an example each

Counts are `grep` over `reference/cpp/core/**/*.cpp` and
`reference/cpp/plugins/**/*.cpp`; a count is a registration *site*, not a
member (a macro block contains many members).

| Mechanism | Where | Count | Example | Makes visible |
|---|---|---:|---|---|
| `TJS_BEGIN_NATIVE_(STATIC_)METHOD_DECL` / `PROP_DECL` inside `TJS_BEGIN_NATIVE_MEMBERS(X)` | core | 423 + 337 | `TJS_BEGIN_NATIVE_METHOD_DECL(load)` | `X.load` |
| `TJS_END_NATIVE_CONSTRUCTOR_DECL(X)` | core | 32 | `TJS_END_NATIVE_CONSTRUCTOR_DECL(Timer)` | `X` (the class object) |
| `registerObject(TJS_W("X"), TVPCreateNativeClass_X())` | core | 22 | `registerObject(TJS_W("Window"), …)` | `global.X` (class object) |
| `classvar->PropSet(TJS_MEMBERENSURE, TJS_W("name"), …)` on a class object | core | 4 | `windowclass->PropSet(… TJS_W("menu") …)` | `X.name` (static/class member) |
| `global->PropSet(TJS_MEMBERENSURE, TJS_W("name"), …)` | core + plugins | many | `global->PropSet(… TJS_W("getDirList") …)` | `global.name` (free function / class object) |
| `var->PropSet(TJS_MEMBERENSURE, TJS_W("name"), …)` on an **instance** (`this`, a dictionary, a method parameter) | core + plugins | most of the 145 core `PropSet(` sites | `dic->PropSet(… TJS_W("curLine") …)` | a member of one object, **not** the class |
| `var->PropSetByNum(TJS_MEMBERENSURE, n, …)` | core + plugins | 22 core / 10 plugin | `array->PropSetByNum(…)` | an indexed member of one object |
| `var->PropSetByVS(…)` | core | 26 | internal TJS object plumbing | not a named class member |
| `NCB_REGISTER_CLASS` / `_SUBCLASS` / `_ATTACH_CLASS` / `NCB_SUBCLASS` | plugins | 13 / 10 / 6 / 20 | `NCB_REGISTER_CLASS(Player) { … }` | `global.Player` / `Namespace.name` |
| `NCB_METHOD*` / `NCB_PROPERTY*` | plugins | 431 / 197 | `NCB_PROPERTY(useD3D, …)` | `Class.method` / `Class.property` |
| `NCB_ATTACH_FUNCTION` / `NCB_REGISTER_FUNCTION` | plugins | 23 | `NCB_ATTACH_FUNCTION(ping, Gadget, ping)` | `Gadget.ping` / `global.f` |
| `TJSNativeClassRegisterNCM` | plugins | 6 | `setXP3ArchiveExtractionFilter` | a static class member |
| `TVPExecuteScript("function f() {}")` | plugins | 2 | `drawFFTGraph` | a global free function |
| `addMethod(dispatch, TJS_W("name"), …)` | plugins | 3 | `layerExPerspective` | a method on the nearest `PropGet` class |
| `#define`-generated name tables | both | — | `TVP_REGISTER_PTDD_ENUM(dtNone)` | `BasicDrawDevice.dtNone` |

Two shapes are **not** class members and must not be counted as such: a
`PropSet` on an instance object (a window, a dictionary, a method parameter)
and a `PropSet` whose receiver is a numeric index. They are the majority of
the `PropSet` sites.

### 2.2 The class-level `PropSet` sites the extractors had to learn

Core (`reference/cpp/core/base/ScriptMgnIntf.cpp`):

| Site | Class | Member |
|---|---|---:|
| line 518 | `WaveSoundBuffer` | `PhaseVocoder` |
| line 528 | `Window` | `BasicDrawDevice` |
| line 531 | `Window` | `PassThroughDrawDevice` |
| line 540 | `Window` | `menu` |

Plugin (`reference/cpp/plugins/**`):

| Site | Surface | Member |
|---|---|---:|
| `motionplayer/main.cpp:523` | `Motion` (namespace attachment) | `Player` |
| `motionplayer/main.cpp:507` | `Player` (static, unresolved receiver) | `useD3D` (already present via `NCB_PROPERTY`) |
| `dirlist.cpp:80` | global object | `getDirList` |
| `fftgraph.cpp:6` | global object (`TVPExecuteScript`) | `drawFFTGraph` |

The `Window.menu` receiver (`windowclass`) is bound by
`registerObject(TJS_W("Window"), (windowclass = TVPCreateNativeClass_Window()))`;
the `Motion` receiver (`motion`) by `global->PropGet(… TJS_W("Motion") …)` then
`motion = motionVar.AsObjectNoAddRef()`. Both bindings are statically
resolvable, so the member can be attributed without guessing.

### 2.3 What each script saw before this change

| Mechanism | `native_parity.py` | `plugin_parity.py` |
|---|---|---|
| native method/prop macro blocks | **saw** | — |
| constructor registration | **saw** | — |
| `registerObject` (class object registration) | saw the class body via `TVPCreateNativeClass_X`; did not read the name mapping | — |
| class-level `PropSet` / `PropSetByNum` | **blind** | **blind** |
| `global->PropSet` free function | **blind** | **blind** |
| instance `PropSet` / dictionary `PropSet` | blind (correctly: not class surface) | blind (correctly) |
| ncbind macros, `TJS_BEGIN_NATIVE_MEMBERS` | — | **saw** |
| `NCB_ATTACH_FUNCTION`, `NCB_REGISTER_FUNCTION` | — | **saw** |
| `addMethod`, `TVPExecuteScript` classes | — | **saw** |
| `TJSNativeClassRegisterNCM` | — | **saw** |
| namespace `PropSet` attachment (`Motion.Player`) | — | **blind** |
| `TVPExecuteScript` global function (`drawFFTGraph`) | — | **blind** |
| data members via `set_member` (`Scripts.pf*`, GdiPlus) | — | **saw** |

The difference in the two bold-blind rows is the bug: the reports extracted
from the constructs they recognised and silently ignored the rest.

The Rust-side readers also had a matching gap: a class attached **after**
construction (`attach_class(ctx, "Window", "BasicDrawDevice")`, an
`exec_script` alias such as `WaveSoundBuffer.PhaseVocoder = …`, and a
script-defined global function) was not read, so the newly visible reference
members would have looked missing on the port side too. Those were taught as
well (§4.2), otherwise the fix would have traded a blind spot for a false
negative.

---

## 3. Before/after counts

| Report | Before | After | Δ |
|---|---:|---:|---:|
| Native reference members | 636 | 640 | +4 |
| Native missing | 0 | 0 | 0 |
| Native coverage | 100.0 % | 100.0 % | — |
| Native classes | 22 | 22 | 0 |
| Plugin reference members | 972 | 974 | +2 |
| Plugin missing | 0 | 0 | 0 |
| Plugin coverage | 100.0 % | 100.0 % | — |
| Plugin classes | 55 | 56 | +1 (`__global__`) |

Native +4: `Window.menu`, `Window.BasicDrawDevice`,
`Window.PassThroughDrawDevice`, `WaveSoundBuffer.PhaseVocoder`.

Plugin +2: `__global__.getDirList`, `__global__.drawFFTGraph` (the namespace
attachment `Motion.Player` is a structural recognition, not a member count;
`Player.useD3D` was already present through `NCB_PROPERTY`).

The `plugin_parity.py` reference count changed by exactly the two global
functions; the per-class Rust member counts in the regenerated
`docs/plugin_parity.md` also move with concurrent work on the GDI+ plugin,
which is unrelated to this change.

---

## 4. Extractor changes

### 4.1 Reference side

`scripts/krkr_gates/checks/native_parity.py`:

- `MemberInfo` records `kind` (`method` / `property` / `constructor` /
  `unknown`), `static`, and `mechanism` (`native-macro` / `constructor` /
  `propset` / `propset-bynum`). `kind` is `unknown` for a `PropSet` member
  because `PropSet` does not reveal method versus property; a new variant is a
  review, not a guess.
- `_core_class_bindings()` binds a local variable to its TJS class from
  `var = TVPCreateNativeClass_X()` and
  `registerObject(TJS_W("X"), (var = TVPCreateNativeClass_X()))`.
- `collect_propset_members()` reads
  `var->PropSet(TJS_MEMBERENSURE, TJS_W("name"), …)` and `PropSetByNum` on a
  bound receiver and attributes the member to the class. A name that is not a
  string literal is returned as an **unrecoverable site**, not dropped.
- `parse_reference_surface()` returns
  `({class: {member: MemberInfo}}, unrecoverable_sites)`; `parse_reference()`
  remains the name-set wrapper, so the diff and allowlist logic is unchanged.
- The report gains a *Reference registration mechanisms* table, a
  *Class-level members installed by `PropSet`* table (class, member, kind,
  static, mechanism), a *Registration sites with a runtime-built name*
  section, and *Extraction notes and remaining blind spots*.
- The allowlist reader accepts `Class.member — reason`; a reason starting
  `debt:` is the marker used by `scripts/allowlists/layering_allow.txt`. `--update-allow`
  preserves existing reasons. Both scripts support the reasoned form and their
  allowlist headers document it.

`scripts/krkr_gates/checks/plugin_parity.py`:

- `_plugin_namespace_bindings()` resolves `motion` from
  `global->PropGet(… TJS_W("Motion") …)` / `AsObjectNoAddRef()`. A scratch
  variant reused by several `PropGet` calls (`tmp`) is treated as ambiguous
  and left unbound, which is what produced a false `metrics` namespace before
  the guard was added.
- `global->PropSet(TJS_MEMBERENSURE, TJS_W("name"))` adds `name` to the
  synthetic `__global__` class; a namespace receiver adds a namespace
  attachment to `nested_by_ns`; a static member on an unresolved helper
  parameter is recorded as unrecoverable.
- `_script_globals()` reads `function f(...)` from a `TVPExecuteScript`
  string and adds it to `__global__` (excluding functions the class reader
  already harvested).
- The reference model gains `unrecoverable` and `propset_members`, and the
  report gains the same two sections as the native report.

### 4.2 Rust side (so the new reference members are not false misses)

- `parse_runtime_class_attachments()` reads
  `attach_class(ctx, "Target", "Member")` and a `Class.Member = …` assignment
  inside an `exec_script` string, filtered to tracked classes.
- `parse_rust()` accepts the reference global-function names and keeps only
  the script-defined functions that are part of that surface, so an internal
  helper such as `__krkr_json_is_null` is not mistaken for API.
- The alias loop now keeps a `Namespace.name` visible when the port keeps the
  global (the reference does both, e.g. `Motion.Player`), so the
  namespace-structure check does not fail on a correct port.

### 4.3 Self-test

Both scripts gained `--self-test`, which runs the reference extractor over a
fixture and asserts an **exact** member set, an exact namespace/propset set,
and that a runtime-built name is reported as unrecoverable rather than
counted:

```sh
uv run --project scripts krkr-gate native-parity --self-test
uv run --project scripts krkr-gate plugin-parity --self-test
```

Fixtures:

- `scripts/fixtures/parity/native_core.cpp` + `native_core.expected.json` —
  native macros, constructor, `registerObject`, class-level `PropSet` and
  `PropSetByNum`, and a runtime-built `PropSet` name.
- `scripts/fixtures/parity/plugin_surface.cpp` +
  `plugin_surface.expected.json` — `NCB_REGISTER_CLASS`/`_SUBCLASS`/
  `NCB_SUBCLASS`, `NCB_METHOD`/`NCB_PROPERTY`, `NCB_ATTACH_FUNCTION`,
  `NCB_REGISTER_FUNCTION`, global/namespace `PropSet`, a script global, and an
  unresolved static member.

A regex that stops matching flags its bad fixture, exactly as
the `layering --self-test` check does. The self-test runs
**automatically on every invocation** (like the guard scripts), so a broken
extractor fails the parity check itself rather than quietly shrinking the
surface; `--quiet` suppresses the `ok` line, and `--self-test` runs only the
self-test and exits.

Measured runtime (this host, `python3`, cold): native self-test ≈ 55 ms,
plugin self-test ≈ 75 ms; a full native/plugin run ≈ 2.5 s each. The
self-tests are fast enough to run on every invocation.

---

## 5. Missing-member work list

After the change, the source-mode reports are green with **0 missing** on both
sides. Triaging each newly visible member:

| Member | Present in the port? | Assessment |
|---|---|---|
| `Window.menu` | Yes — `crates/compat/src/natives/visual/window.rs` (`window_menu_get`, `name: "menu"`); the live `target/runtime_surface.json` now contains it | The historical device failure. The port registers it as an **instance** property, while the reference installs it on the **class** object. The union dump cannot tell the two apart; the class-level access path is a behavioural question for D29, not a surface gap. |
| `Window.BasicDrawDevice` | Yes — `attach_class(ctx, "Window", "BasicDrawDevice")` in `draw_device.rs`; present in `target/runtime_surface.json` | Correct. |
| `Window.PassThroughDrawDevice` | Yes — `attach_class(ctx, "Window", "PassThroughDrawDevice")` | Correct. |
| `WaveSoundBuffer.PhaseVocoder` | Yes — `crates/compat/src/natives/sound.rs` aliases `__TvpPhaseVocoder`; present in `target/runtime_surface.json` | Correct. |
| `__global__.getDirList` | Yes — `crates/compat/src/natives/plugins/dirlist/mod.rs` installs the TJS function | Correct. |
| `__global__.drawFFTGraph` | Yes — `crates/compat/src/natives/plugins/fft_graph/mod.rs` | Correct. |
| `Motion.Player` | Yes — present in `target/runtime_surface.json`; port aliases `Player` into `Motion` | Correct. |
| `Player.useD3D` | Yes — already present via `NCB_PROPERTY` | Correct. |

There is therefore **no genuinely missing member to baseline** from this
change; `scripts/allowlists/native_parity_allow.txt` and
`scripts/allowlists/plugin_parity_allow.txt` stay empty. The `debt:` machinery is in
place so the next genuinely-missing member is baselined visibly instead of
disappearing.

### 5.1 Pre-existing observations (not from this change, not new)

The runtime comparison surfaces two things that were already true before the
`PropSet` work and are outside its scope:

1. **`BasicDrawDevice.name` / `PassThroughDrawDevice.name`.** These are a
   **false member** in the reference extractor: `BasicDrawDevice.cpp:899`
   declares the macro `#define TVP_REGISTER_PTDD_ENUM(name)` whose body
   contains `TJS_BEGIN_NATIVE_PROP_DECL(name)`; the extractor reads the macro
   *definition* and records a member literally named `name`. The real members
   are the invocations `TVP_REGISTER_PTDD_ENUM(dtNone)` etc. `native_parity`
   does not blank `#define` bodies (the plugin reader does), so the false
   member reaches the runtime report where `BasicDrawDevice` **is** a runtime
   class. This is a pre-existing false positive, not a missing member.
2. **`__global__.*` in `--runtime` mode.** `runtime_surface.rs` walks class
   objects and instances; it does not enumerate the global object's free
   functions, so `getDirList`/`drawFFTGraph` can never be found in the dump.
   The runtime report lists them as missing; they are present in the live VM.

Both are recorded as remaining limitations in the regenerated reports.

### 5.2 Recorded findings

No new plugin/DLL coverage work is created by this change. The two
pre-existing observations above are recorded in the audit and in the report
blind-spot sections; the false `name` member is a script-side extractor fix,
not a plugin port, and is left for the next extractor revision rather than
inflating the plugin list in `TODO.md` §10.

---

## 6. Remaining blind spots (do not read coverage as completeness)

The source readers still cannot see:

- **A member whose name is built at runtime.** `menuex->PropSet(idstr.c_str(), …)`
  and `global->PropSet(dynamicName.c_str(), …)` install real members whose
  names are not literals. The extractors report these as *unrecoverable sites*
  (visible in the report) and exclude them from the counts. The coverage
  figure is therefore a lower bound for these classes, not a claim they have
  no other members.
- **`global.X = …`** in a script string (and the `TVPExecuteScript` alias
  form). Only `function f(...)` definitions are harvested from script strings;
  a global object assigned with `=` other than the recognised class-member
  shape is not.
- **A member installed by a plugin DLL at load time** with no
  `reference/cpp` source. D38 defines the DLL-as-oracle pipeline; until a
  plugin is extracted, its surface is outside both reports.
- **Instance members.** A `PropSet` on `this`, a dictionary or a method
  parameter is genuinely not a class member; it is excluded by design, and
  the class-level counts do not imply anything about instance behaviour.
- **`#define` bodies** in the core reader. `native_parity._preprocess_cpp`
  handles `#if`, but not `#define`, so a `TJS_BEGIN_NATIVE_*` inside a macro
  definition is read as if it were a declaration (the `name` false positive
  above). The plugin reader blanks `#define` bodies.
- **Class-level vs instance access.** The parity surface is a name set; it
  cannot tell `Window.menu` (class object) from `window.menu` (instance).
  `Window.menu` is the concrete example: the port has the name but registers
  it on the instance. The surface lock (D29) cannot catch that; it is a
  behavioural check.

The reports carry these as extraction notes so the next reader does not
mistake a coverage figure for a completeness proof.

---

## 7. Files changed

- `scripts/krkr_gates/checks/native_parity.py` — `MemberInfo`, class-level `PropSet` reader,
  runtime attachments, self-test, report sections, reasoned allowlist.
- `scripts/krkr_gates/checks/plugin_parity.py` — namespace/global `PropSet` reader, script
  globals, static-member/global self-test, report sections.
- `scripts/fixtures/parity/{native_core,plugin_surface}.cpp` and their
  `.expected.json` — the self-test fixtures.
- `docs/native_parity.md`, `docs/plugin_parity.md` — regenerated.
- `docs/audits/propset-parity.md` — this audit.

Regenerate with:

```sh
uv run --project scripts krkr-gate native-parity
uv run --project scripts krkr-gate plugin-parity
```
