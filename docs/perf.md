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

It also carries the **later measurements**: the test sweep on the committed
tree (§5), the **script columns** — the lexer, the parser, the code
generator (§6) — the **render path** (§7), the workload the pixel audit
measured (§8), and the **GPU path**, one part of which has landed and one
of which has not (§9). Each of those names its method and its date, and
keeps what was measured apart from what was inferred. A number here is an
observation of one build on one host; it is not a property of the engine.

---

## 1. The measurement setup

| | |
|---|---|
| Host | x86-64 Linux, 12 cores, 31 GB RAM; the game directory is on the in-kernel NTFS driver (Linux 7.2), not a FUSE mount |
| Game | `【KRKR】魔女的夜宴` — 8 archives, 46 239 members, 1.16 GB; the scenarios are 101 PSB containers (`*.ks.scn`, ~1 MB each) |
| Build | `cargo build --release` (opt-level 3, thin LTO, one codegen unit); the profile binary is the same build with symbols and line tables |
| Engine load | `krkr --headless --frames 200 --base-dir <game>` — the whole startup, then 200 frames. The headless loop paces every frame at the frame-rate limit (60 fps by default), so the frame count adds a sleep per frame: measure the startup with `--frames 1`, and read a longer run as the startup plus that sleep. |
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
| system fonts | ~30 ms, deferred to the first font use | the font registry scans the host's font directories (1 800 faces); the scan is no longer on the startup's critical path |
| entry scripts | 677.0 ms | the startup sequence: `patch.tjs`, `startup.tjs`, `system/Initialize.tjs` |
| images decoded | 22.2 ms | 63 images the startup reads |
| first frame | 804.4 ms after open | the whole run, process start to the first presented frame |

The mount time is the one number that moves between runs (32 ms – 135 ms
on the same build): the index read waits on the filesystem. Everything else
is stable. The same 1.16 GB read from a tmpfs saves at most 17 ms of the
startup, so the filesystem is not the startup's cost.

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

The startup — process to first frame — is **0.70 s**. A longer run adds a paced
sleep per frame (60 fps by default), so `--frames 30` measures 1.22–1.25 s of
which 0.48 s is that sleep; the startup itself is the 0.70 s.

The flat profile of the startup (the flat profile is the honest one: a
call-graph profile truncates every chain inside the inflate and reports the
inflate as far smaller than it is):

| self time | what |
|---|---|
| 36 % | the C library — `malloc`/`free` under the VM's allocation churn |
| 11.4 % | the zlib inflate — the six embedded fonts (`sysscn/sourcehansansjp-*.otf`, 27.3 MB, 98.7 ms), not the scripts |
| ~13 % inclusive | `TJS::parser::parse` — the TJS **compiler** (6.2 % self) |
| ~11 % inclusive | `tTJSScriptBlock::SetText` — the materialisation and the parse |
| the rest | the interpreter, the engine's projection and the audio resampler |

The shape is the same as the reference engine's: a KiriKiri game's startup
is dominated by compiling and running its scripts. The reference pays the
same font cost (`reference/krkr2/cpp/core/visual/FontImpl.cpp:142-158` reads
the whole font file).

### 2.4 What is left to win here

The floor, measured: with every read and inflate free the startup is 0.57 s.
The storage is therefore not the whole story, and prefetching the script
reads is worth at most 8 ms of a 630 ms phase — it is not worth doing.

| lever | state |
|---|---|
| the six embedded fonts | 98.7 ms; one contiguous 23.3 MB run in the archive's data area, so they can be decoded in parallel |
| the host font scan | 47 ms, and it runs *inside* the first `System.addFont`, so a lazy registry does not take it off the critical path |
| the VM's page faults | 55 366 faults and 377 MB of RSS — ~50–110 ms |
| the storage namespace | an index by the first path component would save ~20–30 ms |
| the archive mount | eight independent index parses, ~15–18 ms, parallelisable |
| the read pool | dead while the XP3 filter seam is installed (`crates/engine/src/storage.rs:2160-2164`), so every read runs on the calling thread; the reference filters the *decoded* bytes, so only the filter needs the script's thread |
| the TJS2 C++ flags | **measured, no effect**: `-O2` gives 1.232–1.242 s and `-O3` gives 1.238–1.251 s on the same workload. The `-flto` arm still needs a device A/B (`TODO.md` §17) |
| the allocator (`mimalloc`) | measured and rejected (`TODO.md` §17) |
| the zlib backend | `flate2` inflates with `zlib-rs`; the system zlib measures 277 MB/s against it and would save ~45 ms, but the feature links the C library, which the Android target would then need |
| compiling the scripts ahead of time | measured on the game's own 742 KB `mainwindow.tjs`: compile 45.4 ms, bytecode run 6.1 ms, source run 28.9 ms — so ~65–70 ms of the startup, but the bytecode round trip is broken today (`x += 2` and `x++` fail when a script comes from bytecode), and the reference's round trip is broken the same way |

### 2.5 The startup after the performance batch

**2026-11-11.** The tree at `313ed28`, release build, the game's own
directory.

**Method.** A monotonic clock around the **whole process**: `time.monotonic()`
→ `subprocess.run(["./target/release/krkr", "--headless", "--frames", "1",
"--base-dir", <game>])` → the clock again, the process's own output
discarded, three runs in a row. `--frames 1` is the frame count §1 asks
for: the headless loop paces every frame, so one frame is the startup and
nothing else.

| run | wall |
|---|---|
| 1 | 1.096 s |
| 2 | 0.816 s |
| 3 | 0.790 s |

**min 0.790 s, median 0.816 s, max 1.096 s.** The first run is the one
directly after the build, with the archive's 1.16 GB cold in the page
cache; the two after it are the same binary on a warm cache. The 1.096 s
is a cold-cache outlier, not a mode of the startup.

Against the baseline recorded before the performance batch — **0.763 s to
0.799 s**, the same instrument and the same game — **the batch did not move
the startup**. The new minimum (0.790 s) is inside the old band, the
median (0.816 s) is 2 % above its top, and both differences are inside the
run-to-run spread of a process that reads 1.16 GB over NTFS.

This is the **process** wall time, start to exit. The phase timers of §2.1
and the profile of §2.3 measure the interior of the same run (0.70 s from
open to the first presented frame); the two are different instruments and
are not comparable with each other.

**Inferred, and partly wrong.** The batch took two of the levers §2.4 names
— the six embedded fonts and the read pool, which were believed to read the
fonts in parallel — and the total did not move. The fonts lever did not
fire at all: the parallel read was blocked by a guard that §2.6 removes.

### 2.6 The startup after the shared read was unblocked

**2026-11-12.** The tree at `c354af0`, release build, the game's own
directory.

**Method.** The same instrument as §2.5: a monotonic clock around the whole
process, `time.monotonic()` → `subprocess.run([...])` → the clock again,
seven runs in a row, plus the children's user and system time and the peak
RSS.

| | before | after |
|---|---|---|
| wall, min | 0.771 s | **0.671 s** |
| wall, median | 0.777 s | **0.675 s** |
| wall, max | 0.781 s | 0.682 s |
| CPU, median | 0.815 s | 0.857 s |
| peak RSS | 463 MB | 461 MB |

**102 ms off the startup, 13.1 % of the median**, for one deleted guard.

**The cause.** `read_entries_together` returned early whenever an extraction
filter was installed, so the shared read of the eight embedded fonts did
nothing and each font was decoded on its own, on the thread that asked for
it. The guard was a leftover: it belonged to a shape in which the filter was
fused into the decode, and 2181ac5 split the read into two stages — the
decode may run on a worker, and the filter applies to the decoded bytes on
the caller's thread. The shared read was written in that same commit with
the old guard copied into it.

The instrument that found it was a trace line in the real game: the shared
read was called with eight names and returned at once. The game carries an
xp3 filter, so it always took the early return — the parallel font read was
inert for this title, and §2.5's inference that it was parallel was wrong.

The CPU time rose by 42 ms while the wall time fell by 102 ms, which is what
eight decodes running at once on ten workers looks like: the same work, more
of it on other cores.

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

Two reports name where a slow step went: the slow frame and the slow load.
The slow frame has two forms — a one-line summary and, at `debug` level, a
detailed report. Both are **observations**: neither changes what a load, a
decode, a container or a frame does.

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

#### The detailed report

The summary is one line, and it is the same line every time. A second form
answers the question the line leaves open — **which single cost dominates
the frame** — and it is logged at `debug` level (`RUST_LOG=debug`), beside
the summary and not instead of it. The threshold is the same 250 ms: a
frame whose gap reaches it produces both forms.

The detailed report lists **every measured cost of the frame, ranked largest
first**. Each entry names its cost and the section it was measured in —
`phases`, `vm`, `pool`, `host`, `gpu` or `wait` — so the ranking stays
readable as the breakdown it came from. The ranking holds the parts that do
not overlap each other; the totals those parts add up to are in the report's
own totals line, so the largest entry names a cause rather than a sum. The residuals (the part of the VM
tick no segment covers, the part of the engine frame no stage covers, and
the part of the gap neither the frame nor the event poll covers) are in the
report as well, and so is the decode pool's job count.

Its first line names the frame, the gap and the dominant cost; the ranked
entries below it are the detail behind that one line.

#### The report budget

Reporting is not free: the summary is a formatted line and the detailed
report is a formatted list, and both are produced while the frame loop
runs. `--slow-report-ms N` bounds that — **N is the total wall-clock time
the run may spend producing slow-frame reports**, the one-line summary and
the detailed report alike.

| value | the run's reporting |
|---|---|
| the option is absent | unbounded — every slow frame is reported |
| `0` | none — no slow frame is reported |
| `N > 0` | the run reports until the budget is spent |

When the budget is spent the run stops reporting, and it says so once: a
single note names the budget and the number of reports it had already
emitted, so a run whose reporting was cut short is told apart from a run in
which no frame was slow.

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

## 5. The test sweep on the committed tree

**2026-11-11.** The tree at `313ed28`.

**Method.** One `cargo test` per crate, the counts read from the `test
result:` lines of each run.

| crate | passed | failed |
|---|---|---|
| api | 52 | 0 |
| fs | 62 | 0 |
| media | 93 | 0 |
| psd | 27 | 0 |
| render | 96 | 0 |
| engine | 437 | 0 |
| compat | 1 707 | 0 |
| vm | 6 | 0 |
| script | 31 | 0 |
| tjs | 152 | 0 |
| devtools | 25 | 0 |
| testkit | 0 | 0 |
| **total** | **2 688** | **0** |

`testkit` is the shared test helper and carries no test of its own.

Two crates reported **no result**, and the two have different causes:

* **`platform`** — the crate has **no default feature** and refuses to
  compile without exactly one of `platform-linux` / `platform-android`
  (`crates/platform/platform/src/lib.rs:71-72` is a `compile_error!`). A
  `cargo test -p platform` on its own is therefore a compile error, and it
  produces no test line at all: the crate is only testable through a
  consumer that selects the platform, which is the app.
* **`app`** — the app's test binaries need the `render` lib, and the log's
  only build diagnostic is a compile error in that crate — "could not
  compile `render` (lib) due to 8 previous errors". The tree carried the
  in-flight headless-Vulkan render change while the sweep ran — that
  change's own gate failed on clippy, which is where the same compile
  error in `render` shows up — so the render lib did not compile and the
  app's tests were never built. **Inferred** from the order of the sweep:
  `app` is the last crate in it that needs `render`, and every crate
  before it that needs `render` passed, so the change landed between them.

Neither is a failing test: nothing failed. The 2 688 that ran are the whole
of the suite that could run, and the two crates above are the two that did
not run.

---

## 6. The script columns

The TJS2 VM is being reimplemented in Rust one column at a time: the
**lexer** (C1), the **parser** (C2), the **code generator** (C3). The corpus
is the game's own scripts — 175 `.tjs` files, 6.5 MB, 3 206 523 code units,
628 254 tokens — extracted from the title's archives. The reports are
`/tmp/lexer-bench/REPORT.md`, `/tmp/parser-c2/REPORT.md` and
`/tmp/codegen-c3/REPORT.md`; the code is in the committed `crates/script`.

### 6.1 The lexer (C1) — 1.53× the C++ analyser

**Method.** The corpus above, the C++ `tTJSLexicalAnalyzer` and the Rust
`Lexer` over the same flags, the unoptimised development builds on both
sides, the two **measured alternately** in seven rounds each with the
machine idle.

| | tokens/s |
|---|---|
| the C++ analyser | 12 692 744 |
| the Rust analyser | **19 356 655** |

**1.53× the C++ analyser.** Two proofs of equivalence over the whole
corpus:

* 628 254 tokens compared one by one — kind, value and position — every
  one identical to the C++ analyser's dump;
* all 175 dumps **byte-identical through the vendored bison parser's
  `yylex` seam**: the grammar asks the Rust analyser for its tokens
  through the C interface and gets the byte stream the C++ one produced.

The lexing is therefore no longer a factor in the script path: the corpus
costs about a third less to lex than it does in the C++ tree.

### 6.2 The parser (C2) — the trees are identical, 19.7× the grammar

**Method.** The same corpus, both sides in the **parse-only** mode — the
C++'s generator makes no code, the Rust's is not driven — the C++ with
`-O0 -D_DEBUG` and the Rust with `rustc -O`, the two sides **measured
alternately** in seven rounds each.

**The trees are identical.** Over the 175 scripts the Rust parser built
**341 675 nodes**, and **0 of them differ** from the grammar's node: the
same kind, the same place in the script, the same value. 173 of the 175
scripts compare node for node over their whole tree. The two that do not
(`sysscn/system.tjs` and `system/UILoader.tjs`) stop the parse in the
**code generator's** constant folding, which refuses an operation the
reference's variant performs; the count difference (350 450 against
341 675) is the folding's, not the parser's.

**The speed.** The median of seven rounds each, both sides including the
lexer, the parse, and the building of the expression trees with the
folding:

| | the parse |
|---|---|
| the C++ grammar (bison) | 1.736 s |
| the Rust parser | **0.0880 s** |

**19.7× faster than the C++'s bison parser.** The asymmetry is against the
Rust: the C++ side also creates the script block and the code contexts,
which the Rust's parse-only mode creates none of, so the ratio is a lower
bound. Take each side's own lexer out and the parser's own share is about
1.69 s against 0.058 s — **29×**.

### 6.3 The code generator (C3) — not equivalent yet

**Status: in progress.** The comparison machinery is in place and it finds
the differences, which is its job; the generator itself is not yet the
C++'s, so **the emitted code is not compared**.

**Method.** The C++ grammar drives the Rust generator through the C
interface — the proof seam — and each finished context is compared with the
C++'s, the code area word by word and the data area value by value, over
the whole corpus.

| | |
|---|---|
| scripts | 175 |
| code contexts compared | 10 355 |
| instruction words compared | 844 066 |
| constant values compared | 83 440 |
| scripts that agree completely | **5** |
| scripts that still differ | **170** |

Two defects are open:

* the **jump fix-up can cycle** between two jumps that point at each other,
  and the corpus run then **hangs** — it stops on
  `k2compat/k2compat_modeless.tjs`. The reference's fix-up has the same
  shape and does not cycle, because the code it works on does not;
* the **constant folding** refuses an operation the reference's variant
  performs — the two scripts of §6.2 stop there.

The generator is also **not faster** than the C++'s yet: the C++ generator
alone measured 0.847 s (best of three) against 3.489 s for the C++ and the
Rust together with the comparison, and that difference is an upper bound on
the Rust's own work, not a measurement of it. The known cause is the C
interface copying every node, at `O(subtree)` per construction.

So the column's two questions are in different states: the **trees** are
proved identical, the **code** is not.

---

## 7. The render path

### 7.1 The dirty region: a pixel write must not re-upload the whole image

**Landed** — `313ed28`, 2026-11-11.

**What was wrong.** The bitmap state carried two ways to say "these pixels
changed": the whole bitmap, and a rectangle. The writers picked the first
one for writes that touch a known rectangle — a single-pixel write marked
the whole image.

**What it cost.** The frame's upload sends the marked region alone. With
the whole bitmap marked, a full-screen layer written per frame sent
**8.3 MB** where the write had changed **4 bytes**. The 8.3 MB is the
layer's own size (1920×1080×4) and the upload volume the path would send;
it is not a measured frame time.

**What changed.** The rule is now uniform: *a write that touches a known
rectangle marks that rectangle; a write that rewrites the whole image marks
the whole image.* Thirty sites across the engine and the compat layer
follow it: the four pinned pixel writes, the fill family, the tone
operations, the blur, the polygon and stroke writers, the glyph blit, and
the four sites that overrode a good region with a whole mark.

The marked region is a **conservative bounding rectangle** — the box that
covers the pixels written, not the set of them. Two distant pixels in one
frame mark the box between them; that is the existing rule of
`mark_dirty_rect`, not a new one.

**The evidence.** Five tests in `crates/engine/tests/layer_ops.rs` and five
in `crates/compat/tests/dirty_region.rs`: a fill, a clipped fill, a tone
operation, a rectangle fill and a blur each mark exactly the rectangle they
wrote; a single-pixel write marks one pixel; a write outside the image
marks nothing.

The report is `/tmp/dirty-region/REPORT.md`.

### 7.2 The two divergences between the CPU and the GPU, and what closed them

**(a) The Photoshop mix arithmetic — closed, and the old claim was wrong.**

The reference's Photoshop mix is a **word-wide** operation: a subtraction
borrows out of the blue lane into the red one, and the addition of the
destination carries back into it. A per-lane formula cannot reproduce a
borrow, so the shared blend table (`crates/platform/render/src/blend.rs`) was
measured against the reference's own compiled kernels and reported to differ
on 0.68 % of the vectors.

That 0.68 % was **the audit's own model**: it masked the shifted product
before adding the destination, which drops the carry that crosses from the
blue lane into the red one. The borrow and the carry cancel, so the
per-channel mix *is* the packed mix's result.

**Method.** The reference's own kernels, compiled from
`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h` and called through
their real entry points (`ps_alpha_blend_func()` and the rest), against the
shared table, against the rasterizer's word-wide path, and against the
audit's model — over 16 modes x 9 295 104 vectors, the red lane and the
carrying blue lane swept over their whole ranges on both sides of the
borrow threshold, the extremes in every lane, and the whole weight range.

**148 721 664 comparisons, no difference.** The audit's model reproduces its
own 0.695 %, and the reference's kernel agrees with the table on the audit's
two example vectors.

The rasterizer's duplicated word-wide kernels are therefore deleted: it
reads the shared table now, so the two paths cannot drift. Its output does
not change — its word-wide path and the table agree on every vector of that
sweep.

**(b) The sampling filter — closed.**

The reference's default stretch is the **nearest** copy
(`reference/krkr2/cpp/core/visual/RenderManager.cpp:2902`, `:3191-3226`;
`reference/krkr2/cpp/core/visual/tvpgl.cpp:8577-8606`, the floor
convention), and the **CPU rasterizer samples nearest**. The **Vulkan scene
sampler was `LINEAR`**, so a magnified or minified layer was a different
image on the two paths.

The scene sampler takes the **nearest source texel** now
(`crates/platform/render/src/vulkan/pipelines.rs`, `create_sampler`). The
destination-dependent program blends per destination texel and reads the
destination through the same sampler, so it cannot read an interpolation of
the destination's bytes.

**This is a visible change**: a magnified layer is blocky on the GPU, and a
minified one aliases. It is the reference's **software-path** behaviour, and
the port follows it because that is what a script's `getPixel` reads. The
reference's own **OpenGL** path filters linearly
(`reference/krkr2/cpp/core/visual/RenderManager_ogl.cpp:879-887`,
`:1203-1204`, `:2325-2326`), so the port deliberately differs from the
reference's graphics path and matches its software path.

### 7.3 The GPU gap inventory, and what the game's own code reaches

**Method.** The inventory is a read of the render layer's own surface
(`crates/platform/render/src/lib.rs` and the Vulkan backend): every action
the engine can ask for, and what the Vulkan path answers. What the game
reaches is a scan of the game's own entries: the `.tjs` entries extracted
from the archives and converted from UTF-16LE, and the scenario entries
scanned with token-only searches.

**Twelve gaps**, of which eleven are refusals (`RenderError::Unsupported`)
and one is a correctness gap:

| | gap |
|---|---|
| G1 | a destination-dependent blend in an **offscreen** target — the target has no copy of the destination |
| G2 | a destination-dependent blend with a **solid** payload or the **effect** program |
| G3 | a **mask clip** (both backends) |
| G4 | a **GPU glyph run** (both backends) |
| G5 | an **effect draw** on the CPU rasterizer — the GPU is the only path |
| G6 | an **effect pipeline** on the null backend |
| G7 | an effect program with a **destination-dependent blend** |
| G8 | an effect program with a **non-`UnitQuad`** vertex layout |
| G9 | an effect sampler kind outside the three supported |
| G10 | `install_presented_frame_slot` on Vulkan |
| G11 | `begin_render_state` without a display handle on Linux |
| G12 | the **arithmetic** of the destination-dependent blend (§7.2a) — the one that is not a refusal |

**The game's own code reaches none of them.** The render tree the game
builds is made of `SourceOver`, `Replace` and `AddAlpha` nodes with
rectangle clips and ordinary textures, and every one of those has a
fixed-function Vulkan pipeline. The game asks for no destination-dependent
blend in the render tree — its one `operateRect(…, omPsOverlay)` is a CPU
pixel operation — no mask clip and no glyph run, and its only transition
kind is `crossfade`, which has a GPU program.

**Inferred from that:** the gaps are the boundaries of the Vulkan path, not
of this title's workload. A title that did use a destination-dependent blend
would meet G1 and G2 on the first offscreen target it drew.

---

## 8. The workload: the pixel audit

**Method.** The game's own entries — the 175 `.tjs` files and the 28 `.func`
entries of the script corpus, and the 134 scenario entries — searched for
`getPixel`, `setPixel`, `copyPixels` and `.pixels`.

**Zero hits in the scripts. Zero in the scenarios.** The title never
reaches a single pixel through the script API.

**What that says about the workload (inferred):** it is **decode, blit and
alpha-composite**. Nothing in the title reads or writes individual pixels,
so the per-pixel script paths are not on its hot path, and a measurement of
this title is a measurement of the decode and the compositing, not of the
pixel accessors.

**The caution every such audit needs.** A byte-level search for an ASCII
identifier over this game's entries returns **zero** — not because the
identifier is absent, but because the entries are not ASCII: they are
**UTF-16LE, Shift-JIS or PSB containers**. An audit over this corpus must
state its decoding step and print a **coverage figure beside its hit
count**. A zero over the raw bytes is a statement about the encoding, not
about the code.

---

## 9. The GPU path: one landed, one in flight

**The GPU-first fallback policy — landed** (`86972ab`, 2026-11-11). If the
GPU becomes the primary path and the CPU the fallback, the decision is made
**once per subtree, in L2**, and never per node: a subtree's nodes blend
against each other, so a subtree split between the two paths would break
that order. The backend's capability is read once at engine construction and
carried on the scene; a subtree the GPU cannot draw is composited on the
CPU and handed to the GPU as **one texture** — the cache bitmap a
whole-subtree transition already uses, so the projection needed no change.
The rule is published under one name in the render crate and delegates to
the blend mode's own predicate, so the modes are listed once.

On this workload no subtree fallback is triggered: the game's scene is made
of the three blend modes that have fixed-function pipelines (§7.3).

**The headless Vulkan device — in flight.** The logical device's lifetime is
being separated from the presentation surface's, so the GPU path is usable —
and measurable — on a host with a GPU and no display: a render state without
a surface renders into offscreen targets and reads them back, and the CPU and
the GPU can then be compared on one scene with no window anywhere. The change
is in the tree and has not passed the gate yet (its gate run failed on
clippy); the worker reports it as **not compiled**
(`/tmp/headless-vulkan/REPORT.md`). No number from it is on this page yet.

---
---
