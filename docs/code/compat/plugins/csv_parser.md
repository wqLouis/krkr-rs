# CSVParser (`csvParser.dll`)

**DLL name:** `csvParser.dll` — linked with `Plugins.link("csvParser.dll")`
**Rust module:** `crates/compat/src/natives/plugins/csv_parser/`
**Reference:** `reference/cpp/plugins/csvParser.cpp`
**Registers:** the `CSVParser` class on the VM global object

## Surface

| member | kind | behaviour |
|---|---|---|
| `CSVParser(target?, separator?, newline?)` | constructor | the parser, its callback target, the field separator and the newline string |
| `init(text)` | method | resets the parser over an in-memory string |
| `initStorage(filename, utf8?)` | method | resets the parser over a storage file |
| `getNextLine()` | method | the next row as an `Array` of field strings, or `void` when the input is exhausted |
| `parse([text])` | method | optionally re-inits, then drives `target.doLine(fields, lineNo)` |
| `parseStorage(filename, utf8?)` | method | optionally re-inits from storage, then drives `target.doLine(fields, lineNo)` |
| `currentLineNumber` | property (read-only) | the number of the last line read |

The class is registered on the VM global object, so a game's `startup.tjs`
doing `new CSVParser()` works.

## Behaviour

The plugin parses CSV text — a string or a storage file — into rows of field
strings, and either returns them one at a time (`getNextLine`) or drives a
caller-supplied `target.doLine(fields, lineNo)` callback (`parse` /
`parseStorage`).

This is a faithful port of the reference state machine, not a simplified CSV
splitter. The `csv` crate is deliberately **not** used: `split` deviates from
RFC 4180 in observable ways — after a closing quote it copies the remainder up
to the next separator into the field, an unterminated quote continues on the
next physical line joined with the configured `newline` string, and the line
buffer persists across calls — none of which its RFC-conforming reader models.

### The character source

`IFileStr` is the reference `IFileStr` character-source cursor
(`getc` / `ungetc` / `eof` / `endOfLine` / `addNextLine`). `addNextLine`
**appends** the next physical line to the caller's buffer, which is how a
quoted field may span lines.

### The split

`CsvParserInst::split` mirrors `NI_CSVParser::split` exactly, including the
`""` escape, the closing-quote remainder copy and the multi-line continuation
joined with the configured `newline`:

* an empty line yields an empty array;
* an unquoted field runs up to the next separator or the line end;
* a quoted field scans to the closing quote, folds `""` into a literal quote,
  and copies the remaining characters up to the next separator into the
  field;
* an unterminated quoted field continues on the next physical line, joined
  with the configured `newline` string.

### The callback

`parse` invokes `target.doLine(fields, lineNo)` for every row. The target is
the constructor argument; when none was given, the parser object itself is the
target. The reference only runs when the target actually has a valid `doLine`
member (`reference/cpp/plugins/csvParser.cpp:98-101`, guard at `:323`), so a
target without that member ends the run quietly rather than raising.

### Encoding

`initStorage(filename, utf8?)` reads the file through the attached storage.
With `utf8` set, the raw bytes are interpreted strictly as UTF-8 (the
reference's `char *buff` path); otherwise the normal text decoder detects the
encoding.

## State and lifetime

Every piece of state — the parse cursor, the separator, the newline, the line
number, the scratch line buffer and the constructor `target` — lives in the
per-instance payload. There are no process globals.

| state | lifetime |
|---|---|
| `target` | the retained constructor argument; released when the instance is invalidated |
| the character source | dropped by `init` and at the end of a `parse` run |
| the scratch line buffer | cleared before each row is split |

## Errors

| condition | outcome |
|---|---|
| the constructor's `target` is not a string-convertible value | the reference's variant-conversion error (`reference/cpp/core/tjs2/tjsVariant.h:777-789`) |
| `initStorage` / `parseStorage` cannot read the file | the storage's error |
| the storage is not attached | typed error |
| `doLine` raises | the error propagates out of `parse` |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/csv_parser.rs` | the field splitting, the `""` escape, the closing-quote remainder, the multi-line continuation, the separator and newline configuration, the `doLine` callback and its line numbers, `currentLineNumber`, and the storage path |

## Known gaps

* The reference operates on UTF-16 code units (`ttstr`); the port keeps
  Unicode scalar values. The two agree for every BMP character and for the
  separators and newlines CSV data uses.
* The reference's `parse` keeps the parse cursor alive across calls in a
  slightly different way when the target lacks `doLine`; the port ends the
  run at the same point the reference's guard does.
