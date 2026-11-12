# The corpus digest — the net that outlives the front end it measures

Source: `crates/tjs/examples/corpus_digest.rs`, `crates/tjs/tools/extract_corpus.py`,
`crates/tjs/tests/fixtures/corpus_digest.txt` · module `tjs::vm` · L4

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

## 3. The fixture

`crates/tjs/tests/fixtures/corpus_digest.txt` holds one line per script of
the corpus: the digest, and the script's path under the corpus root. A
script the C++ front end cannot compile is listed as `skip` — three of the
175 are, because the tree's analyser stops the process on them.

The digests were recorded **from the C++ front end**, with the tree's own
analyser, grammar and generator, before they were removed. A digest
recorded from the Rust front end would prove nothing: it would only say
that the front end still does what it did when the fixture was written.

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

The other options:

| the option | what it does |
|---|---|
| `--fixture <file>` | the fixture to read; the default is the crate's own |
| `--record <file>` | writes the digests this front end produces, so that they can be compared with the fixture's |
| `--canonical <script>` | writes the canonical form of one script, so that two front ends can be compared by hand |

## 5. The state of the corpus

The fixture and the Rust front end agree on **every field of 79 of the 172
comparable scripts**. For the other 93 the two differ **in the source
positions only**: the code, the constant area, the counts, the relations and
the member registrations agree for all 172.

The difference is the front end's own: the parser reads a token ahead, so
the place it gives a node is the analyser's cursor one code unit later than
the place the C++ front end gives it. The consequence is that the source
position table holds an entry where the C++ table has none, and holds a
different place at the entry before it.

The check is therefore **red on those 93 scripts** until that difference is
settled. The net is doing its job: the source positions are the one field
the corpus comparison never covered, and the check is the first instrument
that holds them to the recorded result.
