# The corpus digest — the net that outlives the front end it measures

Source: `crates/tjs/examples/corpus_digest.rs`, `crates/tjs/tools/extract_corpus.py`,
`crates/tjs/tests/fixtures/corpus_digest.txt`,
`crates/compat/examples/psb_corpus.rs` · module `tjs::vm` · L4

Two proof instruments measured the Rust script columns against the C++ ones:
the **corpus comparison** (the code and the constant images of every script
of the corpus) and the **adapter's proof** (the two script blocks, field by
field). Both ran the C++ analyser, grammar and generator beside the Rust
ones, and both stopped working the moment those were removed: the net went
with the code it measured.

This page is the net that does not. The **fixture** is the digest every
script of the corpus produced, recorded while the C++ front end was still
there; the **check** compiles the corpus with the Rust front end and
compares the digests. The C++ front end is not needed to run the check, and
the check does not need the C++ front end to be meaningful: it compares the
Rust front end's output with a recorded result, not with a live one.

## 1. The corpus

The corpus is the set of `.tjs` scripts a game ships — 175 scripts, taken
from the game's own `*.xp3` archives. **The corpus is not in this
repository**, and neither is the game: the corpus is rebuilt from the game
on the machine that has it.

```bash
python3 crates/tjs/tools/extract_corpus.py <game directory> <corpus directory>
```

The script reads every archive under the game directory (the shallower path
first, then the name), and writes every `.tjs` file it holds into the corpus
directory, under the path the archive holds it at. The corpus root is the
directory the check is pointed at, and the path of a script under that root
is its key everywhere: in the fixture, in the check's output, and in the
digest's record.

### 1.1 The generated scripts

The game also ships its scenarios as **PSB containers** (`*.ks.scn`), and the
compatibility layer converts a container into TJS at runtime: the `psbfile`
plugin reads the container and hands the script its root as TJS values
(`crates/compat/src/natives/plugins/psb_file/`). That conversion is the
engine's own, so the corpus holds it — one script per container, under the
**`psb`** directory of the corpus root:

```
psb/001.共通－オナニーマスター.ks.scn.tjs
```

The scripts are written by the crate's own producer
(`crates/compat/examples/psb_corpus.rs`): it mounts the archives through the
engine's loader, loads every container through the plugin, and writes the
values the plugin built as TJS source. The extraction script runs the
producer, so one command rebuilds the whole corpus.

The two kinds of script are compared the same way; only the directory they
live in differs.

## 2. The digest

The digest is the SHA-256 of the **canonical form** of one compiled script
block. The canonical form is a text, one line per field, and it is a
property of the *block* — not of the way the block is carried — so the two
front ends write the same text for the same script.

```
block contexts=<n> top=<i> errors=<n>
context <i> kind=<k> name=<hex>
  code <n> <word> ...
  constants <n> <value> ...
  counts maxvar=<n> reserve=<n> maxframe=<n> args=<n> argbase=<n> collapse=<n>
  labels <n> <code position>:<source position> ...
  relations parent=<i> setter=<i> getter=<i> super=<i> superptr=<n> <address> ...
  members <n> <name hex>=<context index> ...
```

Every field the two front ends can differ in is in it:

| the field | what it holds |
|---|---|
| the block's shape | the number of contexts, the index of the top level, the number of failures |
| `kind` | the context's type: the top level, a function, a function expression, a property, a property's setter and getter, a class, a class's super-class proxy |
| `name` | the context's name, as the hex of its UTF-16 code units |
| `code` | the context's whole instruction stream, word for word |
| `constants` | the constant area: the void, the integers, the reals (as the hex of their bits), the strings and the byte strings (as the hex of their code units and bytes), the objects (the null, a context by its index, a constant inline array and a constant inline dictionary with their elements) |
| `counts` | the greatest number of local variables, the number of registers the context reserves for itself, the greatest number of frames, the number of declared arguments, the base of the unnamed argument array, the base of the collapsed argument |
| `labels` | the source positions: the code position and the place in the script it names, for every place the code names |
| `relations` | the parent, the property's setter and getter, the super-class proxy (each as the index of the context in the block), and the places in the proxy's own code where the super class is evaluated |
| `members` | the member registrations: the name of every member a context contributes to its parent object, and the context the member holds |

Two canonicalisations make the two sides comparable, and neither of them
loses a difference:

* the **member registrations are sorted by their name**. The order the
  members are registered in is not a property of the object that holds them
  — the object is a hash — and the two front ends register a name that is
  registered twice at different places.
* the **members of a constant inline dictionary are sorted by their name**.
  The dictionary holds them in the order of its own hash.
* the **addresses are taken out of the text of an object**. An object turned
  into a string carries the object's addresses
  (`reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:206`), and the address is
  where the heap happened to be — the engine differs from itself the same
  way. Without this the digest would be a property of the heap's layout
  rather than of the compiled script.

## 3. The fixture

`crates/tjs/tests/fixtures/corpus_digest.txt` holds one line per script of
the corpus: the digest, and the script's path under the corpus root. A
script the vendored path cannot compile is listed as `skip` — three of the
279 are: the vendored path raises on them where the Rust front end recovers
and counts the failure, so the two blocks differ.

The digests were recorded **from the tree's own analyser, grammar and
generator**, never from the Rust front end. A digest recorded from the front
end under test would prove nothing: it would only say that the front end
still does what it did when the fixture was written.

The shipped scripts' digests were recorded through the compile step while the
tree's own analyser, grammar and generator took it. The generated scripts'
digests were recorded through the compiler's own path — the same analyser,
grammar and generator, which the tree carried when the fixture was written
and which the VM's compile step does not take. The recorder writes the
canonical form of the block that path compiles, and the digest is the hash of
that text.

## 4. The check

```bash
cargo run -p tjs --example corpus_digest -- --corpus <corpus directory>
```

The check compiles every comparable script of the corpus with the Rust
front end, computes the digest, and compares it with the fixture's. It
fails — with a non-zero exit code and the name of every script whose
digest differs — when:

* a digest differs from the fixture's;
* the corpus holds a script the fixture has no digest for;
* the fixture holds a script the corpus does not have.

It **does not report success when the corpus is missing**: it says that the
corpus is missing, says how to get it, and exits with a failure.

A **generated** script (§1.1) that the fixture holds no digest for is not a
failure: the digests are recorded from the vendored path, and that recording
is a step of its own. The check names those scripts, counts them apart from
the compared ones, and compares them as soon as the fixture holds their
digests.

The other options:

| the option | what it does |
|---|---|
| `--fixture <file>` | the fixture to read; the default is the crate's own |
| `--record <file>` | writes the digests this front end produces, so that they can be compared with the fixture's |
| `--canonical <script>` | writes the canonical form of one script, so that two front ends can be compared by hand |

## 5. The state of the corpus

The fixture and the Rust front end agree on **every field of all the 276
comparable scripts**: the code, the constant area, the counts, the relations,
the member registrations and the source positions. The three the vendored path
cannot compile are listed as skipped, and the check skips them too.

### 5.1 The generated scripts

The 104 generated scripts (§1.1) are in the fixture, and the check compares
them like the shipped ones.

Their blocks were measured against the C++ path's blocks — the same
comparison, field by field, over the three fields the C++ driver dumps (the
code stream, the constant area and the source positions):

| | |
|---|---|
| the scripts | 104 |
| the contexts | 104 — one per script |
| the code words | 249 123 310 |
| the constants | 17 115 807 |
| the source positions | 46 750 027 |
| the scripts the Rust front end cannot compile | 0 |
| the scripts whose blocks the C++ path reproduces | 104 |
| the scripts that differ in the source positions only | 0 |
| the scripts that differ in anything else | 0 |

The generated scripts add no difference of their own: the block the Rust front
end compiles for the game's scenario data is the block the C++ path produced,
field for field, source positions included.

The one shape the rendering has to get right is the dictionary key. The data
carries `class` as a member name, and `class` is a word the language
reserves, so the key cannot be written as a symbol: it is written in the
literal's pair form (`"class",value`), which both front ends read as the key
and the value of one member.
