# `devtools` — the developer tools

Source: `crates/devtools/` · crate `devtools` · binary `krkr-devtools` ·
**dev only** · **Linux only**

`devtools` is a host program for the development host. It is **not** part of
the engine's runtime architecture: no composition root links it, it is not a
layer, and it is never part of a release build. It exists so that diagnosing a
game's data does not need a throwaway script.

```toml
#![forbid(unsafe_code)]
#![warn(missing_docs)]
```

The binary refuses to build on another target:

```rust
#[cfg(not(target_os = "linux"))]
compile_error!("krkr-devtools is a Linux-only development tool");
```

---

## 1. Building it

```bash
cargo build -p devtools
./target/debug/krkr-devtools --help
```

The crate is a workspace member, so `cargo build --workspace` builds it too.
It links no engine layer: the `xp3` family uses the L1 container parser
(`fs::xp3`) and the `tjs-compile` family links the vendored VM (`tjs`).

### Features

| Feature | Default | What it adds |
|---|---|---|
| `xp3` | on | the `xp3` family: the archive inspector and extractor |
| `tjsb` | on | the `tjsb` family: the bytecode inspector and disassembler |
| `tjs-compile` | off | the `tjs` family: the script compiler (it links the vendored VM) |

```bash
cargo build -p devtools --features tjs-compile
```

A family whose feature is off is refused with a message that names the
feature, not with an unknown-command error.

---

## 2. The commands

Every command prints its result on stdout. A failure prints one message on
stderr and exits with a non-zero status: **2** for a command-line failure, **1**
for every other failure.

### XP3 archives

| Command | What it does |
|---|---|
| `xp3 info <archive.xp3>` | the archive's shape: the entry count, the index blocks and one line per entry |
| `xp3 list <archive.xp3> [--filter SUBSTR]` | the entry names, one per line |
| `xp3 cat <archive.xp3> <name> [--lines A:B]` | the entry's bytes on stdout |
| `xp3 extract <archive.xp3> <name> -o <path>` | the entry's bytes in a file |
| `xp3 extract-all <archive.xp3> -o <dir> [--filter SUBSTR]` | every matching entry below a directory |

```bash
krkr-devtools xp3 info game.xp3
krkr-devtools xp3 list game.xp3 --filter scenario
krkr-devtools xp3 cat game.xp3 data/system/mainwindow.tjs --lines 1:40
krkr-devtools xp3 extract game.xp3 data/system/mainwindow.tjs -o /tmp/mainwindow.tjs
krkr-devtools xp3 extract-all game.xp3 -o /tmp/out --filter scenario
```

**Names.** An entry name is matched after normalization: the name is
lowercased, a backslash becomes a slash, runs of slashes collapse into one and
leading slashes are dropped. A raw stored name and its normalized form reach
the same entry, so both of these work:

```bash
krkr-devtools xp3 cat game.xp3 'Data\Scenario\startup.tjs'
krkr-devtools xp3 cat game.xp3 data/scenario/startup.tjs
```

**Filters.** `--filter` keeps the entries whose normalized name contains the
given text, compared without regard to case.

**`--lines`.** The range is one-based and inclusive, and the entry's bytes
must be text: a byte-order mark selects the encoding (UTF-8, UTF-16 little
endian or UTF-16 big endian), and bytes without a mark must be UTF-8. Bytes
that are text in neither encoding are a failure — the tool does not replace
characters it cannot decode. Read such an entry without `--lines` to get its
raw bytes.

**Writing.** `extract` and `extract-all` create the directories they need. An
entry name that would leave the output directory is refused: an archive is
data, and its names do not decide where the tool writes.

### TJS bytecode

| Command | What it does |
|---|---|
| `tjsb info <file.tjsb>` | the file's shape and one line per object |
| `tjsb strings <file.tjsb> [--min-len N]` | the file's string table |
| `tjsb disasm <file.tjsb> [--object N] [--grep TEXT]` | the listing |

```bash
krkr-devtools tjsb info messagelayer.tjsb
krkr-devtools tjsb strings messagelayer.tjsb --min-len 4
krkr-devtools tjsb disasm messagelayer.tjsb --object 12
krkr-devtools tjsb disasm messagelayer.tjsb --grep onMouseUp
```

`--grep` keeps only the lines that hold the text, so it answers "which
function holds this name" in one step: the object's header line names the
object, and the instructions name the constants.

The tool is a **disassembler, not a decompiler**. It shows the objects, their
opcodes, their operands and the constants they name; it never rebuilds source
text from the bytecode.

### The script compiler

The `tjs` family is built only with the `tjs-compile` feature.

```bash
krkr-devtools tjs compile script.tjs -o script.tjsb
```

The script is compiled as a statement script, and the output path is created
when its directory does not exist.

---

## 3. The bytecode format

The reader accepts the container the VM's loader reads:

```text
"TJS2"            the file's magic
"100\0"           the version
u32               the file's own size
"DATA"            the constant pool's tag
u32               the pool's size, counting this tag and this size field
...               the constant pool
"OBJS"            the object section's tag
u32               the object section's size
i32               the top-level object's index
u32               the object's count
...               the objects
```

The pool holds, in this order, the byte, short, integer, long-integer and real
arrays, then the strings and then the octets. Every array starts with its own
count; the byte array is padded to a four-byte boundary, the short array to a
two-byte boundary, every string to a two-byte boundary and every octet to a
four-byte boundary.

Every object starts with the `"TJS2"` tag and its own size, and holds the
twelve context fields (the parent, the name, the context kind, the two
variable counts, the frame count, the argument count, the two argument bases
and the three property indices), the source positions, the code, the constant
slots, the super-class getter positions and the member list.

A file whose declared size differs from its length, whose section tag is
wrong or that ends early is a failure with a named cause.

---

## 4. The output

`xp3 info` prints the archive's shape and one line per entry, ordered by the
entry's normalized name:

```text
archive: game.xp3
entries: 2
index block 0: offset=32 encode=raw file-chunks=2 new-names=2

name                                            size       stored   segments
data/scenario/startup.tjs                         102         102         1
data/system/mainwindow.tjs                       2048        2048         1
```

`xp3 list` prints one name per line. `xp3 cat` writes the entry's bytes and
nothing else, so it pipes into other tools unchanged.

`tjsb info` prints the file's shape and one line per object:

```text
file: messagelayer.tjsb
size: 24576 bytes
toplevel: 0
objects: 2
constants: bytes=0 shorts=0 longs=3 longlongs=0 doubles=0 strings=57 octets=0
object 0: name="main" ctx=ctTopLevel parent=-1 maxvar=4 varreserve=0 maxframe=8 args=0 code=42 vars=5 props=0
object 1: name="onMouseUp" ctx=ctFunction parent=0 maxvar=2 varreserve=0 maxframe=4 args=1 code=9 vars=3 props=0
```

`tjsb disasm` prints the listing: a register is written `%N`, a constant slot
`*N`, and a jump's target is the address it lands on.

```text
object 1: name="onMouseUp" ctx=ctFunction parent=0 maxvar=2 varreserve=0 maxframe=4 args=1 code=9 vars=3 props=0
  0000  const        %0, *1 = "onMouseUp"
  0003  ret
```

---

## 5. The tests

| File | What it pins |
|---|---|
| `src/cli.rs` | the command line: the positionals, the options, the `--` marker and the usage failures |
| `src/xp3.rs` | the name normalization, the line range, the text decoding and the output path's boundary |
| `src/tjsb/format.rs` | the container reader, on a file the test assembles field by field |
| `src/tjsb/opcode.rs` | the opcode table's numbering and its four-way groups |
| `src/tjsb/disasm.rs` | the listing of the assembled file |

The archive tests use the workspace's in-memory archive builder, so the tree
holds no game data.

---

## 6. Current limits

* The tool is Linux only.
* `--lines` decodes UTF-8 and UTF-16; a legacy code page is not decoded, and
  the tool reports that instead of guessing.
* A string in the table whose UTF-16 units are not a valid sequence is shown
  with the replacement character.
* The disassembler lists the bytecode; it does not rebuild source text.
* The `tjs-compile` family is off by default, so the default build does not
  link the vendored VM.
