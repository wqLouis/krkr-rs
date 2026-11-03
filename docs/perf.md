# The load-time measurements

**What this page is:** the measured cost of the two loads a game performs —
the **engine load** (process start → the first frame) and the **scene load**
(a scenario's assets → the objects a script sees) — with the profile that
names the hotspot in each, and the changes that took the scene load from
~370 ms to ~80 ms and a packed image's tiles from ~955 ms to ~3 ms.

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
* **A CPU profile** — `perf record -F 999 -g --call-graph=dwarf`, on a
  build with symbols and line tables
  (`CARGO_PROFILE_RELEASE_STRIP=none CARGO_PROFILE_RELEASE_DEBUG=1
  TJS2_EXTRA_CXXFLAGS=-g cargo build --release`). The host refused every
  event until `kernel.perf_event_paranoid` was lowered to 1; with that,
  `perf` gives the self time *and* the call chain. The profiles in this
  page were taken that way.
* **A call-stack profile** — `gprofng collect app` (Oracle's profiler,
  which samples on its own signal and needs neither `perf_event` nor
  `ptrace`). It samples coarser than the in-process timer — about one
  sample per 20 ms of CPU — so it is used where there is enough CPU to
  sample (the conversion, §3.5) and where the *inclusive* time is the
  answer.

**What the profiles can and cannot say.** `perf` answers both the self
time and the call chain; the addresses outside the binary are the C
library — almost all of them `malloc`/`free` — and are reported as
"C library" rather than as an unknown. `gprofng` answers the same
questions, but with fewer samples (about one per 20 ms of CPU), so it is
used where there is enough CPU to sample (the conversion, §3.5).

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
| the TJS2 C++ flags (`-O3`, `-O2 -flto`) | the objects become LLVM bitcode and only the Android link validates them, so it needs a device A/B (`TODO.md` §17) |
| the allocator (`mimalloc`) | measured and rejected (`TODO.md` §17) |
| the zlib inflate | `flate2` currently inflates with the pure-Rust `miniz_oxide`; the system zlib is typically 1.5–2× faster, and `flate2`'s `zlib` feature would do it — but the feature links the C library, which the Android target would then need |
| the font registry scan | ~30 ms; a lazy registry would move it out of the startup, at the cost of a script-visible change |

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

| step | the script-built containers | the native constructors | the shared class members |
|---|---|---|---|
| the XP3 index | 0.65 ms | 0.67 ms | — |
| the member read | 1.72 ms | 2.21 ms | — |
| the PSB parse | 22.9 ms | 24.7 ms | — |
| **the conversion** | **403.3 ms** (cold) / **340.6 ms** (warm) | **209.9 ms** (cold) / **134.0 ms** (warm) | **~52 ms** (warm) |
| **the scene load** | **~428 ms** (cold) / **~367 ms** (warm) | **~237 ms** (cold) / **~162 ms** (warm) | **~79 ms** (warm) |

"Cold" is the first conversion in the process (the allocator has not grown
its arenas yet); "warm" is the second and third. The read and the parse are
the same code in every column — the difference between them is the
measurement noise. The three columns are the three steps of §3.4 and §3.6.

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

### 3.5 The next hotspot: the VM copies the class members into every instance

With the conversion at 134 ms, the profile no longer shows the script
compiler: the time is in the VM's object machinery.

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

**Creating the arrays is the cost**: 45.9 % of the whole conversion, and
63 % of the conversion proper. The measured cost of one container
creation (a standalone driver over the vendored VM):

| | time | allocations | bytes |
|---|---|---|---|
| `TJSCreateArrayObject()` | 2.61 µs | 20 | 3 520 |
| `TJSCreateDictionaryObject()` | 0.32 µs | 4 | 568 |

**The reason is the VM's object model**: `tTJSNativeClass::FuncCall` copies
the class's native methods and properties into every instance it creates —
one member at a time, into a hash table that starts with eight buckets and
grows to 32 as the members go in. The `Array` class has 24 instance
members; the `Dictionary` class has none (all eight of its members are
static), which is why a dictionary is eight times cheaper than an array.

The reference's VM does the same: the vendored `tjsObject.cpp` and
`tjsNative.cpp` are identical to the reference's. So this cost is the
reference's own cost, and the scene load pays it 63 879 times.

### 3.6 The change: the instances share the class members

The VM now keeps the class's instance members in one place — the class's
*template* — and the instances share it:

* `tTJSNativeClass` builds the template once (the first time an instance
  is created) and hands it to every instance it creates;
* an instance looks a member up in its own table first and in the template
  second, and copies the template's members into its own table when it
  modifies its own members (`Add`, `PropSet`, `DeleteMember`);
* a shared member is handed out with **the object that uses it** as its
  "objthis", the way the per-instance copies had it: the call and the
  property read pass the object, and a value the script keeps carries it;
* an object's member table is created with its first member, so an object
  that only shares its class's members has no table of its own.

The result, measured with the same driver:

| | before | after |
|---|---|---|
| the array creation | 2.61 µs, 20 allocations, 3 520 B | **0.13 µs, 3 allocations, 216 B** |
| the dictionary creation | 0.32 µs, 4 allocations, 568 B | **0.12 µs, 3 allocations, 200 B** |

and on the real scene (the same machine, back to back; the host was
busy during this A/B, so both numbers are above the idle ones):

| | before | after |
|---|---|---|
| the conversion | 239 ms | **73 ms** (3.2×) |
| the allocations per scene | 1.44 M / 169 MB | **0.80 M / 46 MB** |

The script-visible behaviour is unchanged: the same classes, the same
members, the same values, and a member a script reads or calls still sees
its own object as the "objthis" — the VM's own test suite and the
`psb_file` tests pass unchanged.

**The scene load is now ~80 ms**: ~54 ms of conversion, ~25 ms of parse and
~2 ms of archive read. The two changes together took it from ~370 ms: the
first (the native constructors) from ~370 ms to ~160 ms, the second (the
shared class members) from ~160 ms to ~80 ms.

### 3.7 The packed image's tiles

A scene change's heaviest asset is a packed image: a `.pimg` file that is a
PSB container, read through the `var://` media. The game's own
`system/packimage.tjs` holds each container in a `PackImageInfo`, and the
script reads a tile through `var://./packImageMap/<name>/root/<tile>`.

The `var://` media has no storage of its own — its whole state is a
refcount (`reference/cpp/plugins/varfile.cpp:364-365`) — so it answers a
read by **walking the script object graph**: from the global object, every
path segment is a member of the object the previous segment named
(`reference/cpp/plugins/varfile.cpp:291-340`). Nothing caches the walk, so
every read walks again.

The segment that costs is `root`. It is `PSBFile.root`, a native property
read, so the VM invokes its getter on every read
(`reference/cpp/plugins/psbfile/main.cpp:137`, `main.cpp:29-44`): it builds
a **fresh** object from the container's whole parsed tree, and every
resource in it is **copied** into a fresh octet
(`reference/cpp/plugins/psbfile/PSBValue.cpp:201-204`; the copy itself at
`reference/cpp/core/tjs2/tjsVariant.cpp:27-28`). The parse is not repeated
— the reference caches it in the `PSBFile` instance
(`reference/cpp/plugins/psbfile/PSBFile.h:132,31-33`) — but the projection
is, so **one tile read cost the whole container**, and a packed image with N
visible layers paid that N times over.

The media now answers a tile from the container itself: a load publishes the
container under the path the script reads it through, and the media looks
the path up there first — a name lookup, a chunk lookup, and a copy of that
one resource's bytes. The graph walk stays as the fallback for anything
that is not published.

Measured on the game's largest packed image (`ev122a.pimg`, 12 558 815
bytes, 147 members), through the storage's media API:

| | cost |
|---|---|
| the load (the parse), once per container | 8.7 ms |
| the projection of the whole container — the old cost of every tile | 6.5 ms |
| one tile read through the media | 4.9 µs |
| **all 147 tiles, the old path** | **955 ms** |
| **all 147 tiles, the new path** | **3.0 ms** |

**~320×, and the whole packed image now costs about the time one of its
tiles used to.** The old path copied the container 147 times — about 1.8 GB
of memcpy and 21 609 VM objects — to deliver 12.5 MB of tiles; the new path
copies each tile's own bytes once and builds no VM objects at all.

What is left of the quadratic is the two lookups a tile read still does — a
linear scan of the container's members and of its chunk table — 0.24 µs and
0.03 µs per tile. Replacing them with hash maps would buy 0.3 µs per tile;
it is not worth the shape.

This is a performance divergence and not a behaviour change: the bytes, the
names, the path grammar and the failures are the same, and the reference
still pays the quadratic. The reference's own async graphic loader does the
read and the decode on a worker, so its tile reads overlap where ours now
do too.

---

## 4. The reports that name a slow step

Two reports name where a slow step went. Both are **observations**: neither
changes what a load, a decode, a container or a frame does.

### 4.1 The slow frame

A frame whose gap reaches 250 ms (`SLOW_FRAME_MS`) logs one line that
attributes the gap. The line names the **previous** frame's phases (the
five polls), the VM tick's segments, the host's costs and the GPU's costs.

The line also names the **decode pool's own work** over that frame:

| field | meaning |
|---|---|
| `pool_jobs` | the number of jobs the decode pool completed during the frame |
| `pool_ms` | the time the pool spent running them |

The pool's counters are cumulative for the pool's whole life, so the report
takes the difference between the counters at the end of the frame and the
counters at its start: a frame that waited for decodes is told apart from a
frame that ran script. A frame in which the pool ran nothing reports zero. A
job is counted when it **completes**, so a decode that finishes during the
next frame is counted there — the numbers name the pool's work, not the frame
that waited for it.

### 4.2 The slow load

A single step that took longer than 50 ms is reported with its phase times:

| report | phases |
|---|---|
| the image load (the deferred path) | `resolve` (the name lookup and the scheduling of the read), `storage_read` (the bytes the scheduled read produced), `header_probe` (the decoder's look at the header, which yields the size) |
| the image load (the synchronous path) | `storage_read`, `decode` — the decode runs on the calling thread on that path |
| the container load | `storage_read`, `parse` — the container's parse |
| the root dictionary's build | the build's own duration |
| the `psb://` read | `lookup`, `copy` |

Every report is bounded: a step below the threshold is never reported, and a
step above it is reported at most once every five seconds, so a step that is
slow in every frame does not report in every frame.

---
---
