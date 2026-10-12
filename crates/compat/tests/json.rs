//! Integration tests for the `json` plugin.
//!
//! The plugin attaches four static members to `Scripts`: `evalJSON`,
//! `evalJSONStorage`, `saveJSON` and `toJSONString`. The tests drive them
//! through TJS against a bound [`CompatHost`] and check the parsed values,
//! the reference writer's exact output (indentation and newline handling),
//! storage/file encodings and the typed error surface.
//!
//! Every test serializes on the shared VM lock, since the process-wide
//! engine → context binding is single.
//!
//! # Citation note
//!
//! The vendored reference for this plugin is a *stub*: the four callbacks
//! check their arity and then return their input verbatim (`evalJSON`,
//! `reference/cpp/plugins/json/jsonPlugin.cpp:16-27`), load text without
//! parsing it (`evalJSONStorage`, `jsonPlugin.cpp:30-54`) or stringify via
//! `AsStringNoAddRef` (`toJSONString`, `jsonPlugin.cpp:81-94`), each with an
//! `// 実際は解析…` ("should actually parse/stringify") remark. The
//! behavioural spec that *is* in the tree is the plugin's own manual,
//! `reference/cpp/plugins/json/anaul.tjs` (signatures, defaults, encodings,
//! newline codes), plus whatever the stub itself observably does (argument
//! counts, the `utf8`/`newline` defaults, `saveJSON`'s `true` result, the
//! storage read failure). Every test below cites those; the exact writer
//! byte format and the relaxed-dialect grammar have no in-tree implementation
//! to cite and are unverifiable in-tree.

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
            "krkr-compat-json-{}-{}",
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
        Env {
            _host: host,
            engine,
            _storage: storage,
            _dir: dir,
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "json-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval_ok(&self, expression: &str) -> TjsValue {
        self.engine
            .eval(expression, "json-test")
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
    }

    fn error(&self, expression: &str) -> String {
        self.engine
            .eval(expression, "json-test")
            .expect_err("expected a TJS error")
            .to_string()
    }

    fn string(&self, expression: &str) -> String {
        match self.eval_ok(expression) {
            TjsValue::String(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected a string"),
        }
    }

    fn integer(&self, expression: &str) -> i64 {
        match self.eval_ok(expression) {
            TjsValue::Integer(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    fn real(&self, expression: &str) -> f64 {
        match self.eval_ok(expression) {
            TjsValue::Real(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected a real"),
        }
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self._dir.path().join(name), bytes).expect("write fixture");
    }

    fn read(&self, name: &str) -> Vec<u8> {
        std::fs::read(self._dir.path().join(name)).expect("read output file")
    }
}

// ---------------------------------------------------------------------------
// evalJSON
// ---------------------------------------------------------------------------

/// `evalJSON` parses its argument into an `Array`/`Dictionary`/scalar
/// (`reference/cpp/plugins/json/anaul.tjs:6-11`; the vendored implementation
/// defers the parse itself — `jsonPlugin.cpp:23-25` — but pins the arity at
/// `jsonPlugin.cpp:19-20`). TJS2 has no boolean type: `true`/`false` are
/// integers and `null` void, and JSON numbers map onto the integer/real
/// variant split (`reference/cpp/core/tjs2/tjsVariant.h:100-110`).
#[test]
fn eval_json_parses_scalars_with_the_reference_types() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var i = Scripts.evalJSON(\"42\"); \
         var r = Scripts.evalJSON(\"3.5\"); \
         var s = Scripts.evalJSON(\"\\\"hi\\\"\"); \
         var t = Scripts.evalJSON(\"true\"); \
         var f = Scripts.evalJSON(\"false\"); \
         var n = Scripts.evalJSON(\"null\"); \
         var ti = typeof i; var tr = typeof r; var ts = typeof s; \
         var tt = typeof t; var tn = typeof n;",
    );

    assert_eq!(env.integer("i"), 42);
    assert_eq!(env.real("r"), 3.5);
    assert_eq!(env.string("s"), "hi");
    // TJS2 has no boolean type: `true`/`false` are integers and `null` is void.
    assert_eq!(env.integer("t"), 1);
    assert_eq!(env.integer("f"), 0);
    assert_eq!(env.string("ti"), "Integer");
    assert_eq!(env.string("tr"), "Real");
    assert_eq!(env.string("ts"), "String");
    assert_eq!(env.string("tt"), "Integer");
    // TJS `typeof` reports `void` in lower case.
    assert_eq!(env.string("tn"), "void");
}

/// JSON objects and arrays become TJS `Dictionary`/`Array`
/// (`reference/cpp/plugins/json/anaul.tjs:7-11`), readable through the
/// builtin `count` and index/key accessors.
#[test]
fn eval_json_parses_arrays_and_dictionaries() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var a = Scripts.evalJSON(\"[10, 20, \\\"thirty\\\"]\"); \
         var d = Scripts.evalJSON(\"{\\\"name\\\":\\\"Kirikiri\\\",\\\"count\\\":3}\"); \
         var aCount = a.count; var a1 = a[1]; var a2 = a[2]; \
         var name = d[\"name\"]; var count = d[\"count\"];",
    );

    assert_eq!(env.integer("aCount"), 3);
    assert_eq!(env.integer("a1"), 20);
    assert_eq!(env.string("a2"), "thirty");
    assert_eq!(env.string("name"), "Kirikiri");
    assert_eq!(env.integer("count"), 3);
}

/// `toJSONString` produces JSON text again (`reference/cpp/plugins/json/anaul.tjs:30-36`),
/// so a `null` member — parsed to TJS `void`, the reference's `var.Clear()`
/// path (`jsonPlugin.cpp:50-51`) — survives the round trip as `null`.
#[test]
fn eval_json_round_trips_null_like_the_reference() {
    let _lock = vm_lock();
    let env = Env::new();

    // JSON null becomes a TJS void member; the writer maps void back to
    // `null`, so the key survives the round-trip.
    env.run("var d = Scripts.evalJSON(\"{\\\"a\\\":null}\"); var j = Scripts.toJSONString(d);");
    assert!(
        env.string("j").contains("\"a\":null"),
        "{}",
        env.string("j")
    );
}

/// Containers nest to any depth (`reference/cpp/plugins/json/anaul.tjs:7-11`);
/// a JSON `null` member is stored as TJS `void` (`jsonPlugin.cpp:50-51`).
#[test]
fn eval_json_parses_nested_containers() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var d = Scripts.evalJSON(\"{\\\"list\\\":[1,2],\\\"inner\\\":{\\\"z\\\":null}}\"); \
         var list = d[\"list\"]; var n = list.count; var v = list[1]; \
         var inner = d[\"inner\"]; var t = typeof inner[\"z\"];",
    );

    assert_eq!(env.integer("n"), 2);
    assert_eq!(env.integer("v"), 2);
    assert_eq!(env.string("t"), "void");
}

/// The parser's relaxed dialect (comments, `;` separators, `=>` keys,
/// `\xNN` escapes, the `void` keyword) belongs to the reference parser that
/// the vendored source explicitly stubs out
/// (`reference/cpp/plugins/json/jsonPlugin.cpp:23-25`, "(should actually
/// parse)") — the tree carries no implementation to cite; unverifiable
/// in-tree. The surface it is reached through —
/// `Scripts.evalJSON` as a static member — is pinned at
/// `jsonPlugin.cpp:98-100`.
#[test]
fn eval_json_accepts_the_reference_extensions() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "// line comment\n\
         var a = Scripts.evalJSON(\"[1; 2; /* block */ 3]\"); \
         var d = Scripts.evalJSON(\"{'a'=>1, 'b'=2}\"); \
         var n = a.count; var av = a[2]; var av2 = d[\"b\"];",
    );

    assert_eq!(env.integer("n"), 3);
    assert_eq!(env.integer("av"), 3);
    assert_eq!(env.integer("av2"), 2);

    // `\xNN` escapes become the character they encode.
    env.run(r#"var x = Scripts.evalJSON("[\"\\x41\"]"); var xv = x[0];"#);
    assert_eq!(env.string("xv"), "A");

    // The `void` keyword is the reference's alias for JSON `null`, and `#`
    // starts a line comment.
    env.run("var v = Scripts.evalJSON(\"# c\\nvoid\");");
    assert_eq!(env.string("typeof v"), "void");
}

/// `\u` escapes decode to UTF-16, surrogate pairs included — the same
/// input contract as any reference JSON reader. The vendored plugin stubs
/// the parse itself (`reference/cpp/plugins/json/jsonPlugin.cpp:23-25`),
/// so the pair handling is asserted inside TJS where the engine-side value
/// the game sees is observable; unverifiable in-tree.
#[test]
fn eval_json_decodes_surrogate_pairs() {
    let _lock = vm_lock();
    let env = Env::new();

    // A BMP `\u` escape round-trips through the harness as-is.
    env.run(r#"var a = Scripts.evalJSON("\"\\u3042\""); var av = a;"#);
    assert_eq!(env.string("av"), "あ");

    // The non-BMP pair is asserted *inside* TJS: the `tjs` crate's Rust-side
    // `TjsValue::String` extraction mangles surrogate pairs (pre-existing,
    // outside this crate), while the engine-side value the game sees is
    // correct. A lone surrogate would make `evalJSON` itself error.
    env.run(
        "var s = Scripts.evalJSON(\"\\\"\\\\uD83D\\\\uDE00\\\"\"); \
         var len = s.length; var eq = (s == \"😀\");",
    );
    assert_eq!(env.integer("len"), 2);
    assert_eq!(env.integer("eq"), 1);
}

/// The reference parser's number forms (`007`, `+5`, `.5`, `1.`, `-0`)
/// all yield numbers whose TJS type follows the integer/real split
/// (`reference/cpp/core/tjs2/tjsVariant.h:100-110`); the grammar itself is
/// the stubbed one (`reference/cpp/plugins/json/jsonPlugin.cpp:23-25`,
/// unverifiable in-tree).
#[test]
fn eval_json_accepts_the_reference_number_forms() {
    let _lock = vm_lock();
    let env = Env::new();

    // The reference parser accepts leading zeros, `+` signs, bare fractions
    // and trailing dots; the values and TJS types are unchanged.
    env.run(
        "var a = Scripts.evalJSON(\"007\"); \
         var b = Scripts.evalJSON(\"+5\"); \
         var c = Scripts.evalJSON(\".5\"); \
         var d = Scripts.evalJSON(\"1.\"); \
         var e = Scripts.evalJSON(\"-0\"); \
         var ta = typeof a; var tc = typeof c; var td = typeof d; \
         var te = typeof e;",
    );

    assert_eq!(env.integer("a"), 7);
    assert_eq!(env.integer("b"), 5);
    assert_eq!(env.real("c"), 0.5);
    assert_eq!(env.real("d"), 1.0);
    // `-0` stays an integer zero (the old `parse::<i64>` path).
    assert_eq!(env.integer("e"), 0);
    assert_eq!(env.string("ta"), "Integer");
    assert_eq!(env.string("tc"), "Real");
    assert_eq!(env.string("td"), "Real");
    assert_eq!(env.string("te"), "Integer");
}

/// A parsed `Dictionary` must be indistinguishable from a hand-built `%[]`
/// in both value and serialized form (`reference/cpp/plugins/json/anaul.tjs:7-11,30-36`).
#[test]
fn eval_json_dictionary_members_match_a_directly_built_dictionary() {
    let _lock = vm_lock();
    let env = Env::new();

    // `serde_json` runs with `preserve_order` (the reference stores members
    // in document order), but the *observable* enumeration order is the TJS
    // dictionary's own hash order — a directly built `%[]` enumerates the
    // same way on the reference VM — so both must serialize identically.
    env.run(
        r#"var d = Scripts.evalJSON("{\"zeta\":1,\"alpha\":2}");
           var direct = %[]; direct["zeta"] = 1; direct["alpha"] = 2;
           var z = d["zeta"]; var a = d["alpha"];
           var j = Scripts.toJSONString(d);
           var jd = Scripts.toJSONString(direct);"#,
    );
    assert_eq!(env.integer("z"), 1);
    assert_eq!(env.integer("a"), 2);
    assert_eq!(env.string("j"), env.string("jd"));
}

/// Failures are typed and never partial: the arity check answers
/// `TJS_E_BADPARAMCOUNT` (`reference/cpp/plugins/json/jsonPlugin.cpp:19-20`;
/// rendered as "Invalid argument count",
/// `reference/cpp/core/tjs2/tjsError.h:268`) and every malformed input
/// raises with the member prefix instead of returning a half-parsed value
/// (D11; the vendored parse itself is the stub at `jsonPlugin.cpp:23-25`).
#[test]
fn eval_json_rejects_malformed_input() {
    let _lock = vm_lock();
    let env = Env::new();

    let error = env.error("Scripts.evalJSON(\"{broken\")");
    assert!(error.contains("evalJSON"), "unexpected error: {error}");

    let error = env.error("Scripts.evalJSON()");
    assert!(
        error.contains("requires 1 argument"),
        "unexpected error: {error}"
    );

    // Structural, lexer-level and dialect errors all keep the member prefix.
    for bad in ["[1,]", "{\"a\" 1}", "1 2", "\"\\uD83D\"", "[\"\\xZZ\"]"] {
        let expression = format!("Scripts.evalJSON({bad:?})");
        let error = env.error(&expression);
        assert!(
            error.contains("evalJSON"),
            "{bad:?}: unexpected error: {error}"
        );
    }

    // Bare identifiers outside the keyword set keep the old wording.
    let error = env.error("Scripts.evalJSON(\"nope\")");
    assert!(
        error.contains("unknown keyword"),
        "unexpected error: {error}"
    );
}

// ---------------------------------------------------------------------------
// toJSONString
// ---------------------------------------------------------------------------

/// `toJSONString(obj, newline=0)` returns JSON text
/// (`reference/cpp/plugins/json/anaul.tjs:30-36`); `newline = 0` is CRLF,
/// `1` is LF (`anaul.tjs:33`), and scalars keep their JSON spelling —
/// `true`/`false` are the TJS integers `1`/`0`, `null`/`void` serialize as
/// `null` (`reference/cpp/plugins/json/jsonPlugin.cpp:86-92`).
#[test]
fn to_json_string_writes_the_reference_pretty_format() {
    let _lock = vm_lock();
    let env = Env::new();

    assert_eq!(env.string("Scripts.toJSONString(42)"), "42");
    assert_eq!(env.string("Scripts.toJSONString(3.5)"), "3.5");
    assert_eq!(env.string("Scripts.toJSONString(true)"), "1");
    assert_eq!(env.string("Scripts.toJSONString(false)"), "0");
    assert_eq!(env.string("Scripts.toJSONString(null)"), "null");
    assert_eq!(env.string("Scripts.toJSONString(void)"), "null");
    assert_eq!(env.string("Scripts.toJSONString(\"hi\")"), "\"hi\"");
}

/// Array serialization follows the newline mode of
/// `toJSONString(obj, newline=0)` (`reference/cpp/plugins/json/anaul.tjs:33`);
/// the vendored plugin delegates stringification to a JSON library it only
/// gestures at (`jsonPlugin.cpp:89-90`), so the exact indentation is
/// unverifiable in-tree.
#[test]
fn to_json_string_formats_arrays_like_the_reference() {
    let _lock = vm_lock();
    let env = Env::new();

    // CRLF is the default; each element gets its own line and one space of
    // indentation, with a trailing newline before the closing bracket.
    assert_eq!(
        env.string("Scripts.toJSONString([1,2,3])"),
        "[\r\n 1,\r\n 2,\r\n 3\r\n]"
    );
    // `newline` 1 selects LF.
    assert_eq!(
        env.string("Scripts.toJSONString([1,2,3], 1)"),
        "[\n 1,\n 2,\n 3\n]"
    );
    // Empty containers still get the leading/trailing newline.
    assert_eq!(env.string("Scripts.toJSONString([])"), "[\r\n \r\n]");
}

/// Dictionary serialization (`"key":value` members under the same newline
/// mode, `reference/cpp/plugins/json/anaul.tjs:30-36`); exact bytes
/// unverifiable from the stubbed `jsonPlugin.cpp:86-92`.
#[test]
fn to_json_string_formats_dictionaries_like_the_reference() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run("var d = %[]; d[\"only\"] = 1;");
    assert_eq!(
        env.string("Scripts.toJSONString(d)"),
        "{\r\n \"only\":1\r\n}"
    );
    assert_eq!(env.string("Scripts.toJSONString(%[])"), "{\r\n \r\n}");
}

/// Nesting keeps the container layout of the outer member
/// (`reference/cpp/plugins/json/anaul.tjs:30-36`); byte-exact indentation
/// unverifiable from the vendored stub (`jsonPlugin.cpp:86-92`).
#[test]
fn to_json_string_indents_nested_containers() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run("var d = %[]; d[\"a\"] = [1,2];");
    assert_eq!(
        env.string("Scripts.toJSONString(d)"),
        "{\r\n \"a\":[\r\n  1,\r\n  2\r\n ]\r\n}"
    );
}

/// Strings are re-escaped into valid JSON text on output
/// (`reference/cpp/plugins/json/anaul.tjs:30-36`); control characters and
/// quotes are the JSON minimum the reference library would emit (exact set
/// unverifiable — `jsonPlugin.cpp:89-90`).
#[test]
fn to_json_string_escapes_strings_like_the_reference() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(r#"var s = "a\"b\\c\nd\te"; var j = Scripts.toJSONString(s);"#);
    assert_eq!(env.string("j"), r#""a\"b\\c\nd\te""#);

    env.run(r#"var d = Scripts.evalJSON("[\"\\u0001\"]"); var j = Scripts.toJSONString(d[0]);"#);
    assert_eq!(env.string("j"), "\"\\u0001\"");
}

/// Octets are not JSON values; the reference's generic variant-string path
/// has no octet case, which the port maps to `null` (the same fallback the
/// stub's `AsStringNoAddRef` would take to an empty/non-scalar —
/// `reference/cpp/plugins/json/jsonPlugin.cpp:86-92`).
#[test]
fn to_json_string_serializes_a_top_level_octet_as_null() {
    let _lock = vm_lock();
    let env = Env::new();

    // The reference's `getVariantString` default writes `null` for octets.
    assert_eq!(env.string("Scripts.toJSONString(<% 01 02 %>)"), "null");
}

/// `toJSONString` output must re-parse through `evalJSON` with every type
/// intact (`reference/cpp/plugins/json/anaul.tjs:6-11,30-36`).
#[test]
fn to_json_string_round_trips_through_eval_json() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var source = %[]; \
         source[\"n\"] = 7; source[\"r\"] = 2.5; source[\"s\"] = \"x\"; \
         source[\"list\"] = [1, null, \"two\"]; \
         var text = Scripts.toJSONString(source); \
         var parsed = Scripts.evalJSON(text); \
         var n = parsed[\"n\"]; var r = parsed[\"r\"]; var s = parsed[\"s\"]; \
         var list = parsed[\"list\"]; var lc = list.count; var l1 = typeof list[1];",
    );

    assert_eq!(env.integer("n"), 7);
    assert_eq!(env.real("r"), 2.5);
    assert_eq!(env.string("s"), "x");
    assert_eq!(env.integer("lc"), 3);
    assert_eq!(env.string("l1"), "void");
}

/// The four members are attached to the *class* only
/// (`reference/cpp/plugins/json/jsonPlugin.cpp:98-102`), and the reference
/// never serializes class members — only an object's own data — so the
/// ABI-attached instance methods must not leak into the output (the
/// reference has no such instance members at all).
#[test]
fn to_json_string_skips_attached_instance_methods() {
    let _lock = vm_lock();
    let env = Env::new();

    // The ABI attaches plugin methods to every `Dictionary`/`Array`
    // instance; the reference never serializes class members.
    env.run("var d = %[]; d[\"k\"] = 1; var j = Scripts.toJSONString(d);");
    let j = env.string("j");
    assert!(!j.contains("toJSONString"), "attached member leaked: {j}");
    assert!(!j.contains("clone"), "attached member leaked: {j}");
    assert!(j.contains("\"k\":1"), "missing member: {j}");
}

/// JSON has no representation for NaN/Infinity; a non-finite real is a
/// typed error rather than a silently wrong document (D11). The reference
/// stringifier's numeric formatting slot is stubbed
/// (`reference/cpp/plugins/json/jsonPlugin.cpp:86-92`); unverifiable
/// in-tree.
#[test]
fn to_json_string_rejects_non_finite_reals() {
    let _lock = vm_lock();
    let env = Env::new();

    // TJS parses an overflowing literal as infinity.
    let error = env.error("Scripts.toJSONString(1e999)");
    assert!(error.contains("not finite"), "unexpected error: {error}");
}

// ---------------------------------------------------------------------------
// evalJSONStorage / saveJSON
// ---------------------------------------------------------------------------

/// `evalJSONStorage(jsonFile, utf8=false)` reads the storage and parses it
/// (`reference/cpp/plugins/json/anaul.tjs:13-19`); the `utf8` default is
/// the stub's own (`jsonPlugin.cpp:37`) and the encoding split (UTF-8 versus
/// the current code page) is the manual's (`anaul.tjs:16`).
#[test]
fn eval_json_storage_reads_a_utf8_fixture() {
    let _lock = vm_lock();
    let env = Env::new();
    env.write("data.json", "{\"a\": [1, 2], \"s\": \"あ\"}".as_bytes());

    env.run(
        "var d = Scripts.evalJSONStorage(\"data.json\", true); \
         var v = d[\"s\"]; var a = d[\"a\"]; var n = a.count;",
    );

    assert_eq!(env.string("v"), "あ");
    assert_eq!(env.integer("n"), 2);
}

/// With `utf8 = false` the bytes come back in the current code page
/// (`reference/cpp/plugins/json/anaul.tjs:16,19`; default at
/// `jsonPlugin.cpp:37`).
#[test]
fn eval_json_storage_decodes_the_current_code_page() {
    let _lock = vm_lock();
    let env = Env::new();
    // `あ` in CP932 is 0x82 0xA0.
    env.write("cp932.json", &[b'"', 0x82, 0xA0, b'"']);

    env.run("var v = Scripts.evalJSONStorage(\"cp932.json\", false);");
    assert_eq!(env.string("v"), "あ");
}

/// A missing file must fail loudly. The vendored stub swallows the read
/// error into `TJS_S_FALSE` (an untouched, i.e. `void`, result —
/// `reference/cpp/plugins/json/jsonPlugin.cpp:41-45`), which D11 forbids;
/// the port raises a typed error instead. The member prefix comes from the
/// registration at `jsonPlugin.cpp:100`.
#[test]
fn eval_json_storage_reports_a_missing_file() {
    let _lock = vm_lock();
    let env = Env::new();

    let error = env.error("Scripts.evalJSONStorage(\"missing.json\")");
    assert!(
        error.contains("evalJSONStorage"),
        "unexpected error: {error}"
    );
}

/// `saveJSON(jsonFile, obj, utf8=false, newline=0)` writes the file through
/// the storage (`reference/cpp/plugins/json/anaul.tjs:21-28`), resolving a
/// relative name against the game directory and honouring the `utf8` /
/// `newline` defaults (`jsonPlugin.cpp:63-65`).
#[test]
fn save_json_writes_utf8_and_resolves_relative_names() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run("Scripts.saveJSON(\"out.json\", %[a:1], true, 1);");
    assert_eq!(env.read("out.json"), b"{\n \"a\":1\n}");
}

/// With `utf8 = false` the output is encoded in the current code page
/// (`reference/cpp/plugins/json/anaul.tjs:25,28`; default at
/// `jsonPlugin.cpp:64`).
#[test]
fn save_json_writes_the_current_code_page() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run("Scripts.saveJSON(\"cp932-out.json\", \"あ\", false);");
    // JSON string `"あ"` encoded as CP932.
    assert_eq!(env.read("cp932-out.json"), [b'"', 0x82, 0xA0, b'"']);
}

/// A `saveJSON` document must parse back through `evalJSONStorage` with
/// `utf8 = true` (`reference/cpp/plugins/json/anaul.tjs:14-19,22-28`).
#[test]
fn save_json_round_trips_through_eval_json_storage() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var d = %[]; d[\"n\"] = 5; d[\"list\"] = [\"a\", \"b\"]; \
         Scripts.saveJSON(\"round.json\", d, true, 0);",
    );
    env.run(
        "var parsed = Scripts.evalJSONStorage(\"round.json\", true); \
         var n = parsed[\"n\"]; var list = parsed[\"list\"]; var c = list.count;",
    );

    assert_eq!(env.integer("n"), 5);
    assert_eq!(env.integer("c"), 2);
}

/// `saveJSON` needs two arguments (`reference/cpp/plugins/json/jsonPlugin.cpp:59-60`);
/// the reference answers `TJS_E_BADPARAMCOUNT` ("Invalid argument count",
/// `reference/cpp/core/tjs2/tjsError.h:268`), this port a typed error
/// naming the member.
#[test]
fn save_json_requires_two_arguments() {
    let _lock = vm_lock();
    let env = Env::new();

    let error = env.error("Scripts.saveJSON(\"only-name.json\")");
    assert!(
        error.contains("requires 2 arguments"),
        "unexpected error: {error}"
    );
}

// ---------------------------------------------------------------------------
// attach surface
// ---------------------------------------------------------------------------

/// The four callbacks are static members of the *existing* `Scripts` class
/// (`reference/cpp/plugins/json/jsonPlugin.cpp:98-102`), reachable as
/// `Scripts.<name>` but never as free globals.
#[test]
fn members_live_on_scripts_only() {
    let _lock = vm_lock();
    let env = Env::new();

    // Reachable as `Scripts.<member>`...
    assert_eq!(env.string("Scripts.toJSONString(1)"), "1");
    // ...but not as a global function.
    let error = env.error("toJSONString(1)");
    assert!(!error.is_empty(), "expected an error for a global call");
}

/// The reference registers no free function for the plugin's internal
/// null-classification helper — `NCB_ATTACH_CLASS` adds only the four
/// `Scripts` static members (`reference/cpp/plugins/json/jsonPlugin.cpp:98-103`)
/// — so the port retains the helper internally and removes the script global
/// the registration ABI created for it. The global object carries the
/// function, so its absence is observed through `typeof global.<name>`; a
/// bare `typeof <name>` is a must-exist read that raises for the same absence
/// (`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:1899-1904`, pinned by
/// `crates/compat/tests/typeof_spellings.rs`).
#[test]
fn null_helper_is_not_a_global() {
    let _lock = vm_lock();
    let env = Env::new();

    assert_eq!(env.string("typeof global.__krkr_json_is_null"), "undefined");
    // A bare `typeof` is a must-exist read, so it raises for the absent name;
    // a leaked global would report `"Object"` instead.
    assert!(
        !env.error("typeof __krkr_json_is_null").is_empty(),
        "a bare absent global must raise"
    );
}

/// The reference's `saveJSON` always leaves `*result = true`: its `false`
/// assignment inside the `catch` is dead because
/// `if(result) *result = true;` runs unconditionally afterwards
/// (`reference/cpp/plugins/json/jsonPlugin.cpp:67-79`, result write at
/// `:74-76`). TJS booleans are the integers `1`/`0`.
#[test]
fn save_json_reports_true() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run("var ok = Scripts.saveJSON('true.json', %[a:1], true);");
    assert_eq!(env.integer("ok"), 1);
    // `newline` defaults to 0 = CRLF (`anaul.tjs:26`).
    assert_eq!(env.read("true.json"), b"{\r\n \"a\":1\r\n}");
}

/// TJS numbers are either 64-bit integers or reals
/// (`reference/cpp/core/tjs2/tjsVariant.h:100-110`): a digit-only literal
/// stays an `Integer` even beyond 32 bits, while a decimal point or
/// exponent yields a `Real` — and `toJSONString` prints the real without a
/// fractional part. The reference parser's number handling is the stubbed
/// one (`reference/cpp/plugins/json/jsonPlugin.cpp:23-25`); the typing rule
/// is pinned here and is unverifiable in-tree.
#[test]
fn eval_json_number_typing_splits_at_the_integer_range() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var big = Scripts.evalJSON(\"2147483648\"); \
         var exp = Scripts.evalJSON(\"1e2\"); \
         var frac = Scripts.evalJSON(\"0.5\"); \
         var intMax = Scripts.evalJSON(\"9223372036854775807\"); \
         var tBig = typeof big; var tExp = typeof exp; \
         var tFrac = typeof frac; var tIntMax = typeof intMax; \
         var expText = Scripts.toJSONString(exp);",
    );

    // Beyond i32 but still integral → Integer.
    assert_eq!(env.string("tBig"), "Integer");
    assert_eq!(env.integer("big"), 2_147_483_648);
    // Exponent and fraction → Real.
    assert_eq!(env.string("tExp"), "Real");
    assert_eq!(env.real("exp"), 100.0);
    assert_eq!(env.string("tFrac"), "Real");
    assert_eq!(env.real("frac"), 0.5);
    // The full 64-bit integer range survives exactly.
    assert_eq!(env.string("tIntMax"), "Integer");
    assert_eq!(env.integer("intMax"), i64::MAX);
    // A whole real serializes without its (absent) fraction.
    assert_eq!(env.string("expText"), "100");
}
