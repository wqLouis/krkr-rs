# CSVParser (`csvParser.dll`)

**DLL name:** `csvParser.dll` — linked with `Plugins.link("csvParser.dll")`
**Rust module:** `crates/compat/src/natives/plugins/csv_parser/`
**Reference:** `reference/cpp/plugins/csvParser.cpp`
**Registers:** the `CSVParser` class on the VM global object

The class and its members appear on the first `Plugins.link("csvParser.dll")`;
the built-in table and the linking rules are on the [plugin index](../plugins.md).

## Surface

| member | kind | behaviour |
|---|---|---|
| `CSVParser` | global (class) | the native class, published on the VM global object |
| `new CSVParser(target?, separator?, newline?)` | constructor | the parser, its `doLine` target, the field separator and the newline string |
| `init(text)` | method | resets the parser over an in-memory string |
| `initStorage(filename, utf8?)` | method | resets the parser over a storage file |
| `getNextLine()` | method | the next row as an `Array` of field strings, or `void` when the input is exhausted |
| `parse([text])` | method | optionally re-initialises, then drives `target.doLine(fields, lineNo)` |
| `parseStorage(filename, utf8?)` | method | optionally re-initialises from storage, then drives `target.doLine(fields, lineNo)` |
| `currentLineNumber` | property (read-only) | the number of the last line read |

The class is a native *instance* class: the members are registered on the class
object, and `new CSVParser()` creates the object that carries the parse state.
The reference publishes the class the same way
(`reference/cpp/plugins/csvParser.cpp:445`).

## Behaviour

### The character source

The text is read through a character cursor that mirrors the reference's
`IFileStr` (`reference/cpp/plugins/csvParser.cpp:21-69`): `getc` (`:32`),
`ungetc` (`:34-38`), `eof` (`:40`), `endOfLine` (`:45-54`) and `addNextLine`
(`:56-68`).

`addNextLine` **appends** the next physical line to the caller's buffer
instead of replacing it — that is what lets a quoted field span physical
lines. `endOfLine` treats `\r\n` as a single terminator and pushes the
character back after a lone `\r`, so `a\rb` is two rows.

### The split

`CsvParserInst::split`
(`crates/compat/src/natives/plugins/csv_parser/mod.rs:222-291`) mirrors
`NI_CSVParser::split` (`reference/cpp/plugins/csvParser.cpp:155-199`):

* an empty line registers no field at all, so the row is an empty array
  (`:161-163`);
* an unquoted field runs up to the next separator or the line end (`:187-191`);
* a quoted field scans to the closing quote and folds `""` into one literal
  quote (`:172`);
* the characters between the closing quote and the next separator are copied
  into the same field (`:173-179`) — `"a"b,c` is the two fields `ab` and `c`;
* an unterminated quoted field appends the configured `newline` string and
  continues on the next physical line (`:183-185`).

The fields are registered as the numeric members of a TJS `Array`, counting
from `0` (`:193-197`).

### The row counter

`getNextLine` (`reference/cpp/plugins/csvParser.cpp:292-310`) clears the
scratch line, reads one physical line and, when there was one, increments the
line number and returns the field array. At the end of the input it drops the
character source and the call sees `void`.

The number is readable through `currentLineNumber`, which the reference
declares getter-only (`reference/cpp/plugins/csvParser.cpp:409-418`); the port
registers no setter, so a write raises.

### The callback

`parse` (`reference/cpp/plugins/csvParser.cpp:321-336`) resolves the dispatch
target: the constructor's `target`, or the object the method was called on
when no target was given (`:322`). A script member on the parser instance
therefore wins over a missing constructor target.

The run happens only when the target really has a `doLine` member (`:323`).
The port treats a `void` member result as "no callback", which is what a
`Dictionary` answers for an absent key
(`crates/tjs/cpp/tjs2/tjsDictionary.cpp:738-751`). A target without that
member ends the run quietly and leaves the character source in place, so the
rows are still available to `getNextLine` afterwards.

Each row is delivered as `doLine(fields, lineNo)`.

### The constructor arguments

`Construct` (`reference/cpp/plugins/csvParser.cpp:222-234`) reads three
optional arguments:

| argument | conversion | notes |
|---|---|---|
| `target` | `AsObject` (`:225`) | an object is retained; TJS `null` (an object variant with a null dispatch) means "no target"; every other type raises the variant-conversion error (`reference/cpp/core/tjs2/tjsVariant.h:682-692`) |
| `separator` | `AsInteger` (`:227`) | a string is parsed, a real truncates, an object or an octet raises (`reference/cpp/core/tjs2/tjsVariant.h:862-879`) |
| `newline` | the string form of the argument (`:229`) | the default is a carriage return followed by a line feed (`:211`) |

The separator is stored in a 16-bit `tjs_char`
(`reference/cpp/core/tjs2/tjsTypes.h:44`), so only the low 16 bits of the
integer count.

### Reading a file

`initStorage(filename, utf8?)` (`reference/cpp/plugins/csvParser.cpp:269-289`)
reads the file and resets the parser: the reference opens a raw byte stream
when `utf8` is set and a text stream otherwise. The port reads the file
through the attached storage — with `utf8` the bytes are decoded strictly as
UTF-8, otherwise the text decoder detects the encoding.

`parseStorage(filename, utf8?)` (`:397-407`) does the same read and then runs
the parse. Both members take the file name through the storage-name boundary,
so a `file://` prefixed name is accepted.

## State and lifetime

Every piece of state — the retained `target`, the character source, the line
number, the separator, the newline string and the scratch line buffer — lives
in the per-instance payload. There are no process globals.

| state | lifetime |
|---|---|
| `target` | retained at construction; released when the instance is invalidated |
| the character source | replaced by `init` / `initStorage`, dropped when the input is exhausted, and dropped at the end of a `parse` run |
| the line number | reset to `0` by `init` / `initStorage`, otherwise only read |
| the scratch line buffer | cleared before each row is split |

The reference's `Invalidate` releases the character source and the target
(`reference/cpp/plugins/csvParser.cpp:249-255`); the port does the same, and
the payload itself is freed with the object.

## Errors

| condition | outcome |
|---|---|
| the constructor's `target` is not an object | typed error: `CSVParser: the target argument cannot be converted to an object` |
| the constructor's `separator` is not convertible to an integer | typed error: `CSVParser: separator: …` |
| `init` / `initStorage` / `parse` / `parseStorage` is called with a value that is neither a string nor `void` | typed error: `CSVParser.<member>: cannot convert a type-<n> value to a string` |
| `init` / `initStorage` is called without its argument | typed error: `CSVParser.init requires 1 argument` |
| the file cannot be read | typed error: `CSVParser.initStorage: …` / `CSVParser.parseStorage: …` |
| no storage is attached | typed error: `no storage attached` |
| the `doLine` callback raises | the error propagates out of `parse` |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/csv_parser.rs` | the field splitting and the `""` escape, the remainder copy after a closing quote, the multi-line continuation joined with the configured `newline`, the separator override and its 16-bit truncation, the blank line that yields an empty array and still counts, the `\r` / `\r\n` line endings, the `doLine` callback and its line numbers, the receiver fallback, the target without a `doLine`, the read-only `currentLineNumber`, the `void` result at the end of the input, the constructor's object-only target, the strict string argument of `init`, and the storage path of `initStorage` / `parseStorage` |

## Known gaps

* The `csv` crate is not a dependency of the `compat` crate, and the parser is
  not a general CSV splitter: it reproduces the reference's state machine,
  whose result differs from RFC 4180 in observable ways — the remainder copy
  after a closing quote, the continuation of an unterminated quoted field on
  the next physical line, and the line buffer that persists across calls.
* The reference works on UTF-16 code units (`ttstr`); the port keeps Unicode
  scalar values. The two agree for every BMP character and for the
  separators and newlines CSV data uses.
* The reference stores the separator as a raw 16-bit `tjs_char`. The port
  truncates the integer to 16 bits as well, but it then maps the value
  through `char::from_u32`, which rejects the surrogate range
  (U+D800–U+DFFF) and falls back to `,` for a separator inside it.
* The reference's `initStorage` opens a *text* stream with an empty encoding
  name when `utf8` is not set, so the engine's own default decides the
  encoding. The port decodes the bytes with the text decoder's detection
  instead.
