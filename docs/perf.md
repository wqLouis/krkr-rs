# The load-time measurements

**What this page is:** the measured cost of the two loads a game performs —
the **engine load** (process start → the first frame) and the **scene load**
(a scenario's assets → the objects a script sees) — with the profile that
names the hotspot in each, and the change that moved the scene load.

It is a **measurement record**, not a contract: the numbers are one host,
one game and one build (see §1), and they are here so the next change can be
compared against them. The behaviour the engine keeps is stated in
[`arch/`](arch/overall.md) and [`code/`](code/README.md); this page only
says how long the paths take and where the time goes.

---

## 1. The measurement setup

| | |
|---|---|
| Host | x86-64 Linux, 12 cores, 31 GB RAM; the game directory is a FUSE mount |
| Game | `【KRKR】魔女的夜宴` — 8 archives, 46 239 members, 1.16 GB; the scenarios are 101 PSB containers (`*.ks.scn`, ~1 MB each) |
| Build | `cargo build --release` (opt-level 3, thin LTO, one codegen unit); the profile binary is the same build with symbols and line tables |
| Engine load | `krkr --headless --frames 200 --base-dir <game>` — the whole startup, then 200 frames |
| Scene load | the engine's own path for one scenario: the archive member is read, the PSB container is parsed, and the value tree is converted into the TJS objects a script sees |

**The three instruments.**

* **Phase timers** — the engine's own: the startup counters
  (`app::game`: archives mounted, images decoded, entry scripts), the
  per-script timer (`compat::natives::scripts`, at `debug` log level) and
  the per-native timer (`compat::natives`). They answer *where the wall
  time goes*, with the nesting the script tree has.
* **A CPU profile** — `perf` is unavailable on this host
  (`kernel.perf_event_paranoid=2` refuses every event, and
  `ptrace_scope=1` refuses a profiler that is not the process's parent), so
  the profile is an **in-process sampler**: a `SIGPROF` handler behind
  `ITIMER_PROF` records the instruction pointer of every CPU-time tick,
  and the addresses are symbolised afterwards with `addr2line`. The sampler
  is a measurement tool, not part of the tree: it was added for this page
  and removed again.

* **A call-stack profile** — `gprofng collect app` (Oracle's profiler,
  which samples on its own signal and needs neither `perf_event` nor
  `ptrace`). It samples coarser than the in-process timer — about one
  sample per 20 ms of CPU — so it is used where there is enough CPU to
  sample (the conversion, §3.5) and where the *inclusive* time is the
  answer.

**What the profiles can and cannot say.** The in-process sampler records the
instruction pointer only, so it answers *self time* (*where the CPU is*)
and not the call chain; addresses outside the binary are the C library —
almost all of them `malloc`/`free` — and are reported as "C library"
rather than as an unknown. `gprofng` answers both, but with fewer samples.

---

## 2. The engine load

### 2.1 The phases

One headless run of the game (release build):

| phase | cost | what it is |
|---|---|---|
| archives mounted | 36.7 ms | 8 archives, 46 239 members, 1.16 GB — the index of each archive is parsed, the members are not read |
| system fonts | ~30 ms | the font registry scans the host's font directories (1 800 faces) |
| entry scripts | 677.0 ms | the startup sequence: `patch.tjs`, `startup.tjs`, `system/Initialize.tjs` |
| images decoded | 22.2 ms | 63 images the startup reads |
| first frame | 804.4 ms after open | the whole run, process start to the first presented frame |

The mount time is the one number that moves between runs (32 ms – 135 ms
on the same build): the game directory is a FUSE mount, so the index read
waits on the filesystem. Everything else is stable.

### 2.2 The startup is script-bound

The per-script timer (at `debug` log level) attributes the entry scripts:

| script | cost | inside it |
|---|---|---|
| `startup.tjs` → `system/Initialize.tjs` | 770.8 ms¹ | the whole sequence |
| ↳ `AfterInit.tjs` | 279.4 ms | `embFontLoader.tjs` 128.3 ms, `custom.tjs` 52.7 ms |
| ↳ `Override.tjs` | 211.9 ms | |
| ↳ `MainWindow.tjs` | 29.7 ms | |
| ↳ `world.tjs` | 26.6 ms | |

¹ the `debug` run is slower than the release run above (logging is on).

The per-native timer says where the time is **not**:

```
startup: native time (top 10): Scripts.execStorage=1538.5ms/115;
Scripts.evalStorage=9.0ms/6; Font.mapPrerenderedFont=0.5ms/13; Scripts.exec=0.0ms/3
```

Every native the engine exposes costs less than a millisecond in total.
The `Scripts.execStorage` figure is inclusive (it contains the scripts it
runs), so the engine's own work during the startup is the image decode
(22 ms), the font scan (30 ms) and the archive mount (37 ms) — about 10 % of
the startup. **The other 90 % is the game's own TJS source being compiled
and executed by the vendored VM.**

### 2.3 The profile

The profile of a whole headless run (896 CPU-time samples):

| self time | what |
|---|---|
| 36 % | the C library — `malloc`/`free` under the VM's allocation churn |
| 10 % | `miniz_oxide` — the zlib inflate of the archive index and of the members the startup reads |
| ~6 % | `TJS::parser::parse` + the bison tables — the TJS **compiler** |
| ~4 % | `TJS::tTJSLexicalAnalyzer::GetToken` — the TJS **lexer** |
| the rest | the interpreter, the engine's projection and the audio resampler |

The shape is the same as the reference engine's: a KiriKiri game's startup
is dominated by compiling and running its scripts.

### 2.4 What is left to win here

| lever | state |
|---|---|
| the TJS2 C++ flags (`-O3`, `-O2 -flto`) | measured with `scripts/perf/bench-tjs-opt.sh`; the objects become LLVM bitcode and only the Android link validates them, so it needs a device A/B (`TODO.md` §17) |
| the allocator (`mimalloc`) | measured and rejected (`TODO.md` §17) |
| the zlib inflate | `flate2` currently inflates with the pure-Rust `miniz_oxide`; the system zlib is typically 1.5–2× faster, and `flate2`'s `zlib` feature would do it — but the feature links the C library, which the Android target would then need |
| the font registry scan | ~30 ms; a lazy registry would move it out of the startup, at the cost of a script-visible change |

---

## 3. The scene load

### 3.1 What a scene load is

This game's scenarios are **PSB containers**, not KAG text: the archive holds
`001.共通－オナニーマスター.ks.scn` and its siblings (101 files, ~1 MB
each), and the game reads them through the `psbfile.dll` plugin
(`new PSBFile(name + ".scn")`, then `data.root`). The engine's share of
one scene load is therefore:

1. **read** — the archive member (the XP3 index, then the member's bytes);
2. **parse** — the PSB container: the header, the name and string tables, the
   typed value tree (`psd::container::parse_stream`);
3. **convert** — the value tree into the TJS objects the script sees (the
   `PSBFile.root` getter).

Step 3 is the port's own work: the tree is handed to the script as real
TJS `Array`/`Dictionary` objects.

### 3.2 The measurement

One scene (`001.共通－オナニーマスター.ks.scn`, 1 023 542 bytes), through
the engine's own code path, release build:

| step | before | after |
|---|---|---|
| the XP3 index | 0.65 ms | 0.67 ms |
| the member read | 1.72 ms | 2.21 ms |
| the PSB parse | 22.9 ms | 24.7 ms |
| **the conversion** | **403.3 ms** (cold) / **340.6 ms** (warm) | **209.9 ms** (cold) / **134.0 ms** (warm) |
| **the scene load** | **~428 ms** (cold) / **~367 ms** (warm) | **~237 ms** (cold) / **~162 ms** (warm) |

"Cold" is the first conversion in the process (the allocator has not grown
its arenas yet); "warm" is the second and third. The read and the parse are
the same code in both columns — the difference between them is the
measurement noise.

The tree the conversion walks:

| | |
|---|---|
| dictionaries | 26 373 |
| lists | 37 506 |
| strings | 74 004 |
| dictionary members | 91 220 |
| list items | 106 538 |

### 3.3 The hotspot

The call-stack profile of the conversion **before** the change (`gprofng`,
100 conversions, 41.1 s of CPU) puts the time in the **TJS script
compiler**:

| inclusive time | what |
|---|---|
| 84.9 % | `build_value` — the conversion itself |
| ↳ 70.8 % | `eval_decoded` — the script evaluation the conversion took for every container |
| ↳ 63.6 % | `tTJSScriptCache::EvalExpression` |
| ↳ 24.3 % | `TJS::parser::parse` — the compiler proper |
| ↳ 29.3 % | `tTJSInterCodeContext::ExecuteCode` — running the compiled literal |
| ↳ 23.4 % | the container creation (the same cost as after the change) |

The conversion created every container by *evaluating a script* — `[]` for
an array and `%[]` for a dictionary — so a 1 MB scene file compiled and ran
**63 879 little scripts**, one per container, at ~5.3 µs each (1 000
evaluations of `[]` measured 5.3 ms). That is the ~340 ms.

The shape behind it: the L3↔L4 bridge had no way to ask the VM for a
container, so it reached for the one it had — the script compiler. The
reference builds the same objects with the VM's own array and dictionary
classes (`TJSCreateArrayObject()`, `TJSCreateDictionaryObject()`), so
the port was paying a compiler where the reference pays a constructor.

### 3.4 The change

The VM's own constructors are now reachable from L4, and the conversion
builds its tree through them:

* `crates/tjs/cpp/tjs2_abi.h`, `crates/tjs/cpp/tjs2_abi.cpp` — three ABI
  entry points: `tjs2_new_array`, `tjs2_new_dictionary` (both create the
  VM's own class and retain it) and `tjs2_array_add` (the array's `add`,
  which is what the reference's list conversion calls);
* `crates/tjs/src/lib.rs` — the safe wrappers `Tjs2Engine::new_array`,
  `new_dictionary`, `array_add` and `array_add_octet`;
* `crates/compat/src/natives/plugins/psb_file/mod.rs` — `build_value`
  creates a container with the constructor and appends to it with `add`,
  instead of evaluating a literal per container.

The A/B in one process, same document, same engine:

| round | before | after | |
|---|---|---|---|
| 1 | 395.0 ms | 135.0 ms | 2.93× |
| 2 | 337.5 ms | 139.7 ms | 2.42× |
| 3 | 335.6 ms | 138.5 ms | 2.42× |

**The conversion is 2.4–2.9× faster**, and the whole engine-side scene load
drops from ~370 ms to ~160 ms.

Nothing script-visible changed: the objects are the same kinds (the VM's
own array and dictionary classes), the values are the same, and the
`psb_file` tests — which pin the value shapes, the octet members and the
`psb://` registrations — pass unchanged.

### 3.5 What the conversion costs now

The profile of the conversion after the change (3 895 samples) is the VM's
object machinery, not the port's:

| self time | what |
|---|---|
| 16 % | the C library — the allocator under the VM's allocations |
| ~15 % | the VM's reference counting (`AddRef`, `Release`, the variant's content release) |
| ~10 % | the member tables (the name hash, the member lookup, the member insert) |
| ~5 % | the destruction of the tree (the member tables are deleted with it) |
| the rest | the conversion walk, the string marshalling, the variant copies |

The call-stack profile (`gprofng`, 100 conversions, 19.9 s of CPU) says
where the *inclusive* time goes:

| inclusive time | what |
|---|---|
| 72.7 % | `build_value` — the conversion itself |
| ↳ 45.9 % | `tjs2_new_array` — creating the arrays |
| ↳ 5.4 % | `tjs2_new_dictionary` — creating the dictionaries |
| ↳ 7.4 % | `tjs2_array_add` — appending the list items |
| ↳ 21.9 % | the dictionary member writes (`PropSetByVS`) |
| the rest | the teardown of the tree and the walk |

So **creating the arrays is the cost**: 45.9 % of the whole conversion, and
63 % of the conversion proper. The reason is in the VM: creating an
instance copies the class's native methods into it
(`tTJSNativeClass::FuncCall` → `InternalEnumMembers`, 45.3 % inclusive),
and the `Array` class has more members than the `Dictionary` class. The
measured cost inside the C++ VM is **2.55 µs per array** and **0.31 µs per
dictionary**; 37 506 arrays are therefore ~96 ms of the 135 ms, and the rest
of the conversion is ~40 ms.

The reference pays the same: it creates the same arrays with the same call
(`TJSCreateArrayObject()`). **The port is now at the reference's own cost
for this step.** What is left would need a change inside the vendored VM —
the copy of the native methods into every instance — and that is the
reference's own VM.
