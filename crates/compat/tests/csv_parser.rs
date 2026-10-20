//! Integration tests for the `CSVParser` native class.
//!
//! The tests drive the class through TJS against a bound [`CompatHost`]. Most
//! cases use an in-memory `init`; the `parseStorage` case writes a fixture to a
//! temporary game directory mounted through a real [`engine::Storage`].
//!
//! Every test serializes on the shared VM lock, since the process-wide
//! engine → context binding is single.

mod common;

use std::path::{Path, PathBuf};

use common::vm_lock;
use compat::CompatHost;
use engine::Storage;
use fs::FileSystem;
use tjs::{Tjs2Engine, TjsValue};

/// A unique temporary game directory, removed on drop.
struct GameDir(PathBuf);

impl GameDir {
    fn new() -> Self {
        let mut path = std::env::temp_dir();
        let unique = format!(
            "krkr-compat-csv-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        );
        path.push(unique);
        std::fs::create_dir_all(&path).expect("create temp game dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self.0.join(name), bytes).expect("write fixture");
    }
}

impl Drop for GameDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A bound VM with a mounted storage attached to the compat services.
struct Env {
    // Drop order matters: the host removes the process-wide binding before
    // the boxed engine/storage it points at are freed.
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    _storage: Box<Storage>,
    _dir: GameDir,
}

impl Env {
    fn new() -> Env {
        let dir = GameDir::new();
        let mut storage = Box::new(
            Storage::mount_archives(
                FileSystem::host(dir.path()),
                dir.path().to_path_buf(),
                std::iter::empty::<&str>(),
            )
            .expect("mount storage"),
        );
        // Boxed so the address stays stable when it is moved into `Env`.
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut host = CompatHost::new();
        // SAFETY: `storage` is boxed and outlives the binding, which `_host`
        // removes on drop.
        unsafe { host.services_mut().attach_storage(&mut *storage) };
        host.install(&engine).expect("install compat surface");
        common::link_all_builtins(&engine);
        Env {
            _host: host,
            engine,
            _storage: storage,
            _dir: dir,
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "csv-parser-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval_ok(&self, expression: &str) -> TjsValue {
        self.engine
            .eval(expression, "csv-parser-test")
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
    }

    fn int(&self, expression: &str) -> i64 {
        match self.eval_ok(expression) {
            TjsValue::Integer(value) => value,
            TjsValue::Real(value) => value as i64,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    fn string(&self, expression: &str) -> String {
        match self.eval_ok(expression) {
            TjsValue::String(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected a string"),
        }
    }
}

// `NI_CSVParser::split` registers each field as an array member
// (`reference/cpp/plugins/csvParser.cpp:155-199`), `""` is folded into a
// literal quote (`:169-181`) and `getNextLine` hands the array back while
// advancing `lineNo` (`:292-310`).
#[test]
fn quoted_fields_and_escaped_quotes() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var csv = new CSVParser(); \
         csv.init(\"a,b,c\\n\\\"x,y\\\",z,\\\"he said \\\"\\\"hi\\\"\\\"\\\"\\n\"); \
         var r1 = csv.getNextLine(); \
         var n1 = csv.currentLineNumber; \
         var r2 = csv.getNextLine(); \
         var n2 = csv.currentLineNumber; \
         var r3 = csv.getNextLine(); \
         var n3 = csv.currentLineNumber;",
    );

    // Plain fields, including a trailing empty slot.
    assert_eq!(env.int("r1.length"), 3);
    assert_eq!(env.string("r1[0]"), "a");
    assert_eq!(env.string("r1[1]"), "b");
    assert_eq!(env.string("r1[2]"), "c");

    // A quoted field may contain the separator; `""` is a literal quote.
    assert_eq!(env.int("r2.length"), 3);
    assert_eq!(env.string("r2[0]"), "x,y");
    assert_eq!(env.string("r2[1]"), "z");
    assert_eq!(env.string("r2[2]"), "he said \"hi\"");

    // The line number advances per row and stays put once exhausted.
    assert_eq!(env.int("n1"), 1);
    assert_eq!(env.int("n2"), 2);
    assert_eq!(env.int("n3"), 2);

    // Exhaustion returns `void`.
    assert_eq!(env.int("r3 === void"), 1);
}

// An unterminated quoted field appends the configured `newline` string and
// continues on the next physical line (`reference/cpp/plugins/csvParser.cpp:184-185`).
#[test]
fn multi_line_quoted_field_is_joined_with_newline() {
    let _lock = vm_lock();
    let env = Env::new();

    // The physical lines end in `\n`; the default `newline` is `\r\n`, so the
    // continuation is joined with `\r\n`.
    env.run(
        "var csv = new CSVParser(); \
         csv.init(\"\\\"line1\\nline2\\\",b\\n\"); \
         var r = csv.getNextLine();",
    );

    assert_eq!(env.int("r.length"), 2);
    assert_eq!(env.string("r[0]"), "line1\r\nline2");
    assert_eq!(env.string("r[1]"), "b");
}

// The constructor takes `target`, then `separator` (truncated to a 16-bit
// `tjs_char`, `reference/cpp/plugins/csvParser.cpp:227` +
// `reference/cpp/core/tjs2/tjsTypes.h:44`), then `newline` (`:229`).
#[test]
fn separator_override_uses_the_configured_character() {
    let _lock = vm_lock();
    let env = Env::new();

    // 59 is the code of ';'. The first constructor argument (the `doLine`
    // target) is `null`: `AsObject()` accepts only `tvtObject`, and a
    // null-object `tvtObject` is that while `void` (`tvtVoid`) raises
    // (`reference/cpp/core/tjs2/tjsVariant.h:682-692`).
    env.run(
        "var csv = new CSVParser(null, 59); \
         csv.init(\"a;b;c\\n\"); \
         var r = csv.getNextLine();",
    );

    assert_eq!(env.int("r.length"), 3);
    assert_eq!(env.string("r[0]"), "a");
    assert_eq!(env.string("r[1]"), "b");
    assert_eq!(env.string("r[2]"), "c");
}

// `parse` resolves the constructor target (or the receiver), looks `doLine`
// up on it and invokes it with `(fields, lineNo)` per line
// (`reference/cpp/plugins/csvParser.cpp:321-337`).
#[test]
fn parse_drives_the_constructor_target_do_line() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "class Target { var rows; function Target() { rows = []; } \
         function doLine(fields, lineNo) { rows.push([lineNo, fields[0], fields[1]]); } } \
         var target = new Target(); \
         var csv = new CSVParser(target); \
         csv.init(\"a,b\\nc,d\\n\\\"e,f\\\",g\\n\"); \
         csv.parse();",
    );

    assert_eq!(env.int("target.rows.length"), 3);
    assert_eq!(env.int("target.rows[0][0]"), 1);
    assert_eq!(env.string("target.rows[0][1]"), "a");
    assert_eq!(env.string("target.rows[0][2]"), "b");
    assert_eq!(env.int("target.rows[1][0]"), 2);
    assert_eq!(env.string("target.rows[1][1]"), "c");
    assert_eq!(env.int("target.rows[2][0]"), 3);
    assert_eq!(env.string("target.rows[2][1]"), "e,f");
    assert_eq!(env.string("target.rows[2][2]"), "g");
}

// `parse` with a first argument re-inits the parser before driving it
// (`reference/cpp/plugins/csvParser.cpp:386-395`).
#[test]
fn parse_with_a_string_argument_reinitializes_first() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "class Target { var count; function Target() { count = 0; } \
         function doLine(fields, lineNo) { count = lineNo; } } \
         var target = new Target(); \
         var csv = new CSVParser(target); \
         csv.parse(\"one;two\\n\");",
    );
    // The default separator is `,`, so this is a single field and one call.
    assert_eq!(env.int("target.count"), 1);
}

// `initStorage(filename, utf8?)` reads the storage and resets the parser
// (`reference/cpp/plugins/csvParser.cpp:269-290`, method wrapper `:367-376`).
#[test]
fn init_storage_reads_through_the_attached_storage() {
    let _lock = vm_lock();
    let env = Env::new();
    env._dir.write("data.csv", b"h1,h2\nv1,v2\n");

    env.run(
        "var csv = new CSVParser(); \
         csv.initStorage(\"data.csv\"); \
         var head = csv.getNextLine(); \
         var row = csv.getNextLine(); \
         var n = csv.currentLineNumber;",
    );

    assert_eq!(env.string("head[0]"), "h1");
    assert_eq!(env.string("row[1]"), "v2");
    assert_eq!(env.int("n"), 2);
}

// `parseStorage(filename, utf8?)` optionally re-inits from storage, then
// runs `parse` until the source is exhausted
// (`reference/cpp/plugins/csvParser.cpp:397-407`).
#[test]
fn parse_storage_drives_the_target_until_exhausted() {
    let _lock = vm_lock();
    let env = Env::new();
    env._dir.write("data.csv", b"h1,h2\nalpha,1\nbeta,2\n");

    env.run(
        "class Target { var rows; function Target() { rows = []; } \
         function doLine(fields, lineNo) { rows.push(fields[0] + \"=\" + fields[1]); } } \
         var target = new Target(); \
         var csv = new CSVParser(target); \
         csv.parseStorage(\"data.csv\");",
    );

    assert_eq!(env.int("target.rows.length"), 3);
    assert_eq!(env.string("target.rows[0]"), "h1=h2");
    assert_eq!(env.string("target.rows[1]"), "alpha=1");
    assert_eq!(env.string("target.rows[2]"), "beta=2");
}

// `Construct` runs `param[0]->AsObject()` (`reference/cpp/plugins/csvParser.cpp:224-225`);
// `AsObject` raises the variant-convert error for every non-object, non-null
// type (`reference/cpp/core/tjs2/tjsVariant.h:682-692`), so a plain integer
// target must fail construction instead of being silently ignored.
#[test]
fn constructor_rejects_a_non_object_target() {
    let _lock = vm_lock();
    let env = Env::new();

    let error = env
        .engine
        .exec_script("var csv = new CSVParser(5);", "csv-parser-test")
        .expect_err("a non-object target must raise");
    let message = error.to_string();
    assert!(
        message.contains("target") || message.contains("convert"),
        "unexpected error: {message}"
    );
    // `null` is an object-typed variant whose dispatch is null: the
    // reference stores a null target (no callback target) and construction
    // succeeds (`reference/cpp/core/tjs2/tjsVariant.h:683-687`).
    env.run("var ok = new CSVParser(null);");
    assert_eq!(env.int("ok !== null"), 1);
}

// `init` passes `param[0]->AsStringNoAddRef()` (`reference/cpp/plugins/csvParser.cpp:363`),
// which throws for integers/objects/octets and maps `void` to no string
// (`reference/cpp/core/tjs2/tjsVariant.h:777-789`), so `init(5)` raises
// while `init(void)` behaves like an empty source.
#[test]
fn init_with_a_non_string_argument_raises() {
    let _lock = vm_lock();
    let env = Env::new();

    let error = env
        .engine
        .exec_script("var csv = new CSVParser(); csv.init(5);", "csv-parser-test")
        .expect_err("init with an integer must raise");
    assert!(
        error.to_string().contains("string"),
        "unexpected error: {error}"
    );

    // `void` maps to a null `tTJSVariantString*`, i.e. an empty source:
    // the first `getNextLine` immediately exhausts it and returns `void`.
    env.run("var csv2 = new CSVParser(); csv2.init(void); var r = csv2.getNextLine();");
    assert_eq!(env.int("r === void"), 1);
}

// A blank physical line still counts: `addNextLine` returns `true` for an
// empty line (`reference/cpp/plugins/csvParser.cpp:56-66`), `getNextLine`
// bumps `lineNo` (`:297`), and `split` returns before registering anything
// when the line is empty (`:161-163`) — an empty array, not a skipped row.
#[test]
fn blank_line_yields_an_empty_array_and_still_counts() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var csv = new CSVParser(); \
         csv.init(\"a\\n\\nb\\n\"); \
         var r1 = csv.getNextLine(); \
         var r2 = csv.getNextLine(); \
         var r3 = csv.getNextLine(); \
         var n = csv.currentLineNumber;",
    );

    assert_eq!(env.int("r1.length"), 1);
    assert_eq!(env.int("r2.length"), 0);
    assert_eq!(env.int("r3.length"), 1);
    assert_eq!(env.string("r3[0]"), "b");
    assert_eq!(env.int("n"), 3);
}

// After a closing quote the reference copies the remainder up to the next
// separator into the field (`reference/cpp/plugins/csvParser.cpp:173-179`),
// so `"a"b,c` splits into `ab` and `c` — not into three fields, and not
// with `b` dropped.
#[test]
fn closing_quote_copies_the_remainder_into_the_field() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var csv = new CSVParser(); \
         csv.init(\"\\\"a\\\"b,c\\n\"); \
         var r = csv.getNextLine();",
    );

    assert_eq!(env.int("r.length"), 2);
    assert_eq!(env.string("r[0]"), "ab");
    assert_eq!(env.string("r[1]"), "c");
}

// When no constructor target exists `parse` falls back to the object the
// method was invoked on (`reference/cpp/plugins/csvParser.cpp:322`), so a
// `doLine` the script installs on the parser instance itself wins — the
// callback is reached through the object, not through a cached native.
#[test]
fn parse_falls_back_to_the_receiver_so_a_script_override_wins() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var csv = new CSVParser(); \
         var seen = []; \
         csv.doLine = function(fields, lineNo) { seen.push(lineNo + \":\" + fields[0]); }; \
         csv.init(\"x,y\\n\"); \
         csv.parse();",
    );

    assert_eq!(env.int("seen.length"), 1);
    assert_eq!(env.string("seen[0]"), "1:x");
}

// `parse` only runs when the target actually has a `doLine`
// (`reference/cpp/plugins/csvParser.cpp:323`); a target without the member
// leaves the character source untouched, so the lines are still available to
// `getNextLine` afterwards.
#[test]
fn parse_without_do_line_leaves_the_source_untouched() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var target = %[]; \
         var csv = new CSVParser(target); \
         csv.init(\"keep,me\\n\"); \
         csv.parse(); \
         var r = csv.getNextLine();",
    );

    assert_eq!(env.int("r.length"), 2);
    assert_eq!(env.string("r[1]"), "me");
}

// `currentLineNumber` is declared getter-only: the reference marks the
// property with `TJS_DENY_NATIVE_PROP_SETTER`
// (`reference/cpp/plugins/csvParser.cpp:409-418`), so a write is an error.
#[test]
fn current_line_number_is_read_only() {
    let _lock = vm_lock();
    let env = Env::new();

    let error = env
        .engine
        .exec_script(
            "var csv = new CSVParser(); csv.currentLineNumber = 5;",
            "csv-parser-test",
        )
        .expect_err("writing the read-only property must raise");
    assert!(!error.to_string().is_empty());
}

// Reading `getNextLine` before any `init` sees a null character source and
// returns `false` — `void` to the script (`reference/cpp/plugins/csvParser.cpp:292-308`).
#[test]
fn get_next_line_before_init_returns_void() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run("var csv = new CSVParser(); var r = csv.getNextLine();");
    assert_eq!(env.int("r === void"), 1);
    assert_eq!(env.int("csv.currentLineNumber"), 0);
}

// `endOfLine` treats a lone `\r` as a terminator (pushing the following
// character back) and `\r\n` as one terminator
// (`reference/cpp/plugins/csvParser.cpp:45-54`), so `a\rb\n` is two rows.
#[test]
fn carriage_return_line_endings_split_rows() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var csv = new CSVParser(); \
         csv.init(\"a\\rb\\n\"); \
         var r1 = csv.getNextLine(); \
         var r2 = csv.getNextLine(); \
         var r3 = csv.getNextLine();",
    );

    assert_eq!(env.int("r1.length"), 1);
    assert_eq!(env.string("r1[0]"), "a");
    assert_eq!(env.string("r2[0]"), "b");
    assert_eq!(env.int("r3 === void"), 1);
}

// The separator is stored in a 16-bit `tjs_char`
// (`reference/cpp/plugins/csvParser.cpp:227` +
// `reference/cpp/core/tjs2/tjsTypes.h:44`): the integer `0x1003B` truncates
// to `0x3B` (`';'`), which then acts as the field separator.
#[test]
fn separator_integer_is_truncated_to_16_bits() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var csv = new CSVParser(null, 0x1003B); \
         csv.init(\"a;b\\n\"); \
         var r = csv.getNextLine();",
    );

    assert_eq!(env.int("r.length"), 2);
    assert_eq!(env.string("r[0]"), "a");
    assert_eq!(env.string("r[1]"), "b");
}
