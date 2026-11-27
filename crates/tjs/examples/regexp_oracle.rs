//! Records what the engine answers for the `RegExp` class's members.
//!
//! Each probe runs a script that builds its own instance, performs the call the
//! probe is about, and answers the kind and the text of the result. A probe
//! whose rule is a refusal catches it and answers the raised message, so the
//! record holds the message a script would read. The probes cover the class's
//! surface, the state one search leaves, the flags, the literal form, the
//! replacement and split rules, and the members a script may write.
//!
//! The record is written to `tests/fixtures/regexp_members.txt` atomically: the
//! whole table is written beside the fixture and renamed over it, so a failed
//! run leaves the old record in place.
//!
//! ```sh
//! cargo run -p tjs --example regexp_oracle
//! ```

use std::fs;
use std::path::PathBuf;

use tjs::{Tjs2Engine, TjsValue};

/// The fixture's first line, so that regenerating the record keeps the file
/// self-describing.
const HEADER: &str = "# the engine's answers, recorded by `crates/tjs/examples/regexp_oracle.rs`; regenerate with the command in the example's docs";

/// The helpers every probe uses.
const SETUP: &str = r#"function describe(value) {
    return (typeof (value)) + ":" + ("" + value);
}
function regexpState(re) {
    return "" + re.start + "|" + re.index + "|" + re.lastIndex + "|" + re.lastMatch + "|" + re.lastParen + "|" + re.leftContext + "|" + re.rightContext + "|" + (typeof (re.matches));
}
function regexpMatchArray(re) {
    return "" + re.matches.count + "|" + re.matches[0] + "|" + re.matches[1] + "|" + re.matches[2];
}
function regexpReplacer(matches) {
    return "[" + matches[0] + "/" + matches[1] + "]";
}
function regexpReplacerThis(matches) {
    return typeof this;
}
function regexpReplacerNumber(matches) {
    return 42;
}
"#;

/// The probes, in the order the check replays them.
const PROBES: &[(&str, &str)] = &[
    ("surface_class_typeof", r#"return describe(typeof RegExp);"#),
    (
        "surface_finalize",
        r#"var re = new RegExp("a"); return describe(typeof re.finalize);"#,
    ),
    (
        "surface_compile",
        r#"var re = new RegExp("a"); return describe(typeof re.compile);"#,
    ),
    (
        "surface_compile_literal",
        r#"var re = new RegExp("a"); return describe(typeof re._compile);"#,
    ),
    (
        "surface_test",
        r#"var re = new RegExp("a"); return describe(typeof re.test);"#,
    ),
    (
        "surface_match",
        r#"var re = new RegExp("a"); return describe(typeof re.match);"#,
    ),
    (
        "surface_exec",
        r#"var re = new RegExp("a"); return describe(typeof re.exec);"#,
    ),
    (
        "surface_replace",
        r#"var re = new RegExp("a"); return describe(typeof re.replace);"#,
    ),
    (
        "surface_split",
        r#"var re = new RegExp("a"); return describe(typeof re.split);"#,
    ),
    (
        "surface_matches",
        r#"var re = new RegExp("a"); return describe(typeof re.matches);"#,
    ),
    (
        "surface_start",
        r#"var re = new RegExp("a"); return describe(typeof re.start);"#,
    ),
    (
        "surface_index",
        r#"var re = new RegExp("a"); return describe(typeof re.index);"#,
    ),
    (
        "surface_last_index",
        r#"var re = new RegExp("a"); return describe(typeof re.lastIndex);"#,
    ),
    (
        "surface_input",
        r#"var re = new RegExp("a"); return describe(typeof re.input);"#,
    ),
    (
        "surface_last_match",
        r#"var re = new RegExp("a"); return describe(typeof re.lastMatch);"#,
    ),
    (
        "surface_last_paren",
        r#"var re = new RegExp("a"); return describe(typeof re.lastParen);"#,
    ),
    (
        "surface_left_context",
        r#"var re = new RegExp("a"); return describe(typeof re.leftContext);"#,
    ),
    (
        "surface_right_context",
        r#"var re = new RegExp("a"); return describe(typeof re.rightContext);"#,
    ),
    (
        "surface_last",
        r#"var re = new RegExp("a"); return describe(typeof re.last);"#,
    ),
    (
        "static_last_typeof",
        r#"return describe(typeof RegExp.last);"#,
    ),
    (
        "static_last_after_test",
        r#"var re = new RegExp("a"); re.test("a"); return describe(typeof RegExp.last);"#,
    ),
    (
        "static_last_after_match",
        r#"var re = new RegExp("a"); re.exec("a"); re.match("a"); return describe(RegExp.last == re);"#,
    ),
    (
        "absent_to_string",
        r#"var re = new RegExp("a"); return describe(typeof re.toString);"#,
    ),
    (
        "absent_flags",
        r#"var re = new RegExp("a"); return describe(typeof re.flags);"#,
    ),
    (
        "absent_source",
        r#"var re = new RegExp("a"); return describe(typeof re.source);"#,
    ),
    (
        "absent_global",
        r#"var re = new RegExp("a"); return describe(typeof re.global);"#,
    ),
    (
        "absent_ignore_case",
        r#"var re = new RegExp("a"); return describe(typeof re.ignoreCase);"#,
    ),
    (
        "absent_multiline",
        r#"var re = new RegExp("a"); return describe(typeof re.multiline);"#,
    ),
    (
        "value_initial_state",
        r#"var re = new RegExp("a"); return describe(regexpState(re));"#,
    ),
    (
        "value_constructor_pattern",
        r#"var re = new RegExp("a"); return describe(re.test("a"));"#,
    ),
    (
        "value_constructor_flags_ignorecase",
        r#"var re = new RegExp("a", "i"); return describe(re.test("A"));"#,
    ),
    (
        "value_constructor_flags_global",
        r#"var re = new RegExp("a", "g"); return describe(re.test("aa"));"#,
    ),
    (
        "value_constructor_unknown_flag",
        r#"var re = new RegExp("a", "x"); return describe(re.test("a"));"#,
    ),
    (
        "value_constructor_l_flag",
        r#"var re = new RegExp("a", "l"); return describe(re.test("a"));"#,
    ),
    (
        "value_constructor_number_pattern",
        r#"var re = new RegExp(1); return describe(re.test("1"));"#,
    ),
    (
        "value_constructor_number_target",
        r#"var re = new RegExp("1"); return describe(re.test(1));"#,
    ),
    (
        "value_constructor_empty_test_text",
        r#"var re = new RegExp(""); return describe(re.test("abc"));"#,
    ),
    (
        "value_constructor_empty_test_empty",
        r#"var re = new RegExp(""); return describe(re.test(""));"#,
    ),
    (
        "value_constructor_empty_exec",
        r#"var re = new RegExp(""); re.exec("abc"); return describe(regexpState(re));"#,
    ),
    (
        "value_compile_replaces",
        r#"var re = new RegExp("a"); re.compile("b"); return describe(re.test("b"));"#,
    ),
    (
        "value_compile_extra",
        r#"var re = new RegExp("a"); re.compile("b", "i", "x"); return describe(re.test("B"));"#,
    ),
    (
        "value_compile_empty",
        r#"var re = new RegExp("a"); re.compile(""); return describe(re.test("x"));"#,
    ),
    (
        "value_compile_missing",
        r#"var re = new RegExp("a"); try { re.compile(); } catch (e) { return "!raise"; } return "answered";"#,
    ),
    (
        "value_literal_compile",
        r#"var re = new RegExp(); re._compile("//i/a"); return describe(re.test("A"));"#,
    ),
    (
        "value_literal_compile_global",
        r#"var re = new RegExp(); re._compile("//g/a"); var first = re.test("aa"); var one = regexpState(re); var second = re.test("aa"); var two = regexpState(re); var third = re.test("aa"); return describe("" + first + ":" + second + ":" + third + "|" + one + "|" + two + "|" + regexpState(re));"#,
    ),
    (
        "value_literal_format_double_slash",
        r#"var re = new RegExp(); try { re._compile("a"); } catch (e) { return "!raise"; } return "answered";"#,
    ),
    (
        "value_literal_format_no_close",
        r#"var re = new RegExp(); try { re._compile("//a"); } catch (e) { return "!raise"; } return "answered";"#,
    ),
    (
        "value_literal_format_extra",
        r#"var re = new RegExp(); try { re._compile("//a/", 1); } catch (e) { return "!raise"; } return "answered";"#,
    ),
    (
        "value_literal_bad_pattern",
        r#"var re = new RegExp(); try { re._compile("///("); } catch (e) { return "!raise:" + e.message; } return "answered";"#,
    ),
    (
        "value_invalid_pattern_constructor",
        r#"try { new RegExp("("); } catch (e) { return "!raise:" + e.message; } return "answered";"#,
    ),
    (
        "value_invalid_pattern_compile",
        r#"var re = new RegExp("a"); try { re.compile("("); } catch (e) { return "!raise:" + e.message; } return "answered";"#,
    ),
    (
        "value_exec_match_array",
        r#"var re = new RegExp("a(b)"); re.exec("ab"); return describe(regexpMatchArray(re));"#,
    ),
    (
        "value_exec_match_state",
        r#"var re = new RegExp("a(b)"); re.exec("ab"); return describe(regexpState(re));"#,
    ),
    (
        "value_exec_index_bug",
        r#"var re = new RegExp("b"); re.exec("ab"); return describe(regexpState(re));"#,
    ),
    (
        "value_exec_start_bug",
        r#"var re = new RegExp("b"); re.start = 1; re.exec("ab"); return describe(regexpState(re));"#,
    ),
    (
        "value_exec_miss_state",
        r#"var re = new RegExp("a"); re.exec("ab"); re.exec("zz"); return describe(regexpState(re));"#,
    ),
    (
        "value_exec_global_advance",
        r#"var re = new RegExp("a", "g"); var first = re.test("aa"); var one = regexpState(re); var second = re.test("aa"); var two = regexpState(re); var third = re.test("aa"); return describe("" + first + ":" + second + ":" + third + "|" + one + "|" + two + "|" + regexpState(re));"#,
    ),
    (
        "value_exec_global_group_end",
        r#"var re = new RegExp("a(?=(bc))", "g"); re.exec("abc"); return describe(regexpState(re));"#,
    ),
    (
        "value_exec_global_no_match",
        r#"var re = new RegExp("z", "g"); re.exec("abc"); return describe(regexpState(re));"#,
    ),
    (
        "value_test_returns",
        r#"var re = new RegExp("a"); var yes = re.test("a"); var no = re.test("z"); return describe("" + yes + ":" + no);"#,
    ),
    (
        "value_test_state",
        r#"var re = new RegExp("a(b)"); re.test("ab"); return describe(regexpState(re));"#,
    ),
    (
        "value_match_no_state",
        r#"var re = new RegExp("a"); var arr = re.match("a"); return describe("" + arr.count + "|" + arr[0] + "|" + regexpState(re));"#,
    ),
    (
        "value_match_failure",
        r#"var re = new RegExp("z"); var arr = re.match("abc"); return describe(arr.count);"#,
    ),
    (
        "value_matches_unmatched_group",
        r#"var re = new RegExp("(a)(b)?"); re.exec("a"); return describe("" + re.matches.count + "|" + (typeof re.matches[0]) + "|" + re.matches[0] + "|" + (typeof re.matches[2]) + "|" + re.matches[2]);"#,
    ),
    (
        "value_lastparen_last_group",
        r#"var re = new RegExp("(a)(b)"); re.exec("ab"); return describe(re.lastParen);"#,
    ),
    (
        "value_lastparen_unmatched",
        r#"var re = new RegExp("(a)(b)?"); re.exec("a"); return describe(re.lastParen);"#,
    ),
    (
        "value_exec_emoji",
        r#"var re = new RegExp("."); re.exec("😀"); return describe("" + re.matches.count + "|" + re.matches[0] + "|" + re.index + "|" + re.lastIndex + "|" + re.lastMatch + "|" + re.lastParen);"#,
    ),
    (
        "value_exec_emoji_global",
        r#"var re = new RegExp(".", "g"); var first = re.exec("😀a"); var one = regexpState(re); var second = re.exec("😀a"); return describe("" + first.count + "|" + first[0] + "|" + one + "|" + second.count + "|" + second[0] + "|" + regexpState(re));"#,
    ),
    (
        "value_exec_ignorecase",
        r#"var re = new RegExp("ä", "i"); return describe(re.test("Ä"));"#,
    ),
    (
        "value_exec_dot_newline",
        r#"var re = new RegExp("."); var newline = re.test("\n"); var letter = re.test("a"); return describe("" + newline + ":" + letter);"#,
    ),
    (
        "value_replace_literal",
        r#"var re = new RegExp("b"); return describe(re.replace("abc", "$1"));"#,
    ),
    (
        "value_replace_global",
        r#"var re = new RegExp("a", "g"); return describe(re.replace("aaa", "x"));"#,
    ),
    (
        "value_replace_first",
        r#"var re = new RegExp("a"); return describe(re.replace("aaa", "x"));"#,
    ),
    (
        "value_replace_callback",
        r#"var re = new RegExp("(a)"); return describe(re.replace("ab", regexpReplacer));"#,
    ),
    (
        "value_replace_callback_this",
        r#"var re = new RegExp("a"); return describe(re.replace("a", regexpReplacerThis));"#,
    ),
    (
        "value_replace_callback_number",
        r#"var re = new RegExp("a"); return describe(re.replace("a", regexpReplacerNumber));"#,
    ),
    (
        "value_replace_no_match",
        r#"var re = new RegExp("a"); return describe(re.replace("zzz", "x"));"#,
    ),
    (
        "value_replace_empty_pattern",
        r#"var re = new RegExp(""); return describe(re.replace("abc", "x"));"#,
    ),
    (
        "value_replace_missing",
        r#"var re = new RegExp("a"); try { re.replace("abc"); } catch (e) { return "!raise"; } return "answered";"#,
    ),
    (
        "value_replace_extra",
        r#"var re = new RegExp("a"); return describe(re.replace("abc", "x", 9));"#,
    ),
    (
        "value_replace_no_state",
        r#"var re = new RegExp("a"); re.exec("ab"); var before = regexpState(re); var out = re.replace("ab", "x"); return describe("" + out + "|" + before + "|" + regexpState(re));"#,
    ),
    (
        "value_split_basic",
        r#"var re = new RegExp(","); var a = re.split("a,b,c"); return describe("" + a.count + "|" + a[0] + "|" + a[1] + "|" + a[2]);"#,
    ),
    (
        "value_split_keep_empty",
        r#"var re = new RegExp(","); var a = re.split("a,,b"); return describe("" + a.count + "|" + a[0] + "|" + a[1] + "|" + a[2]);"#,
    ),
    (
        "value_split_purge_empty",
        r#"var re = new RegExp(","); var a = re.split("a,,b", void, true); return describe("" + a.count + "|" + a[0] + "|" + a[1]);"#,
    ),
    (
        "value_split_leading_trailing_keep",
        r#"var re = new RegExp(","); var a = re.split(",a,"); return describe("" + a.count + "|" + a[0] + "|" + a[1] + "|" + a[2]);"#,
    ),
    (
        "value_split_leading_trailing_purge",
        r#"var re = new RegExp(","); var a = re.split(",a,", void, true); return describe("" + a.count + "|" + a[0]);"#,
    ),
    (
        "value_split_no_match",
        r#"var re = new RegExp(","); var a = re.split("abc"); return describe("" + a.count + "|" + a[0]);"#,
    ),
    (
        "value_split_empty_target",
        r#"var re = new RegExp(","); var a = re.split(""); return describe("" + a.count + "|" + a[0]);"#,
    ),
    (
        "value_split_captures_ignored",
        r#"var re = new RegExp("(,)"); var a = re.split("a,b"); return describe("" + a.count + "|" + a[0] + "|" + a[1]);"#,
    ),
    (
        "value_split_global",
        r#"var re = new RegExp(",", "g"); var a = re.split("a,b,c"); return describe("" + a.count + "|" + a[0] + "|" + a[1] + "|" + a[2]);"#,
    ),
    (
        "value_split_start_ignored",
        r#"var re = new RegExp(","); re.start = 1; var a = re.split("a,b"); return describe("" + a.count + "|" + a[0] + "|" + a[1]);"#,
    ),
    (
        "value_split_missing",
        r#"var re = new RegExp(","); try { re.split(); } catch (e) { return "!raise"; } return "answered";"#,
    ),
    (
        "value_split_reserved",
        r#"var re = new RegExp(","); var a = re.split("a,b", 123, true); return describe("" + a.count + "|" + a[0] + "|" + a[1]);"#,
    ),
    (
        "value_split_empty_pattern",
        r#"var re = new RegExp(""); var a = re.split("abc"); return describe("" + a.count + "|" + a[0]);"#,
    ),
    (
        "value_split_no_state",
        r#"var re = new RegExp(","); re.exec("a,b"); var before = regexpState(re); var a = re.split("a,b"); return describe("" + a.count + "|" + before + "|" + regexpState(re));"#,
    ),
    (
        "value_start_set_text",
        r#"var re = new RegExp("a"); re.start = "2"; return describe(re.start);"#,
    ),
    (
        "value_start_set_real",
        r#"var re = new RegExp("a"); re.start = 1.9; return describe(re.start);"#,
    ),
    (
        "value_start_set_void",
        r#"var re = new RegExp("a"); re.start = void; return describe(re.start);"#,
    ),
    (
        "value_start_set_negative",
        r#"var re = new RegExp("a"); re.start = -1; return describe(re.start);"#,
    ),
    (
        "value_readonly_index",
        r#"var re = new RegExp("a"); try { re.index = 1; } catch (e) { return "!raise"; } return "answered";"#,
    ),
    (
        "value_readonly_matches",
        r#"var re = new RegExp("a"); try { re.matches = 1; } catch (e) { return "!raise"; } return "answered";"#,
    ),
    (
        "value_readonly_last",
        r#"try { RegExp.last = 1; } catch (e) { return "!raise"; } return "answered";"#,
    ),
];

fn main() {
    let engine = match Tjs2Engine::new() {
        Ok(engine) => engine,
        Err(error) => fail(format_args!("the engine did not start: {error}")),
    };
    if let Err(error) = engine.exec_script_discard(SETUP, "regexp_members") {
        fail(format_args!("the engine rejected the setup: {error}"));
    }
    let mut table = String::new();
    table.push_str(HEADER);
    table.push('\n');
    for &(probe, script) in PROBES {
        match engine.exec_script(script, "regexp_members") {
            Ok(TjsValue::String(text)) => match text.split_once(':') {
                Some((kind, value)) => {
                    table.push_str(&format!("{probe}\t{kind}\t{}\n", escaped(value)));
                }
                // a probe that catches its own raise answers the marker, and the
                // row is the one every other record uses for a refusal
                None if text == "!raise" => {
                    table.push_str(&format!("{probe}\t!raise\t\n"));
                }
                None => fail(format_args!(
                    "probe {probe}: the answer has no kind and text: {text:?}"
                )),
            },
            Ok(other) => fail(format_args!(
                "probe {probe}: the answer is not a text: {other:?}"
            )),
            Err(error) => fail(format_args!("probe {probe}: the script failed: {error}")),
        }
    }
    let path = fixture_path();
    let temporary = path.with_extension("txt.tmp");
    if let Err(error) = fs::write(&temporary, table) {
        fail(format_args!(
            "the temporary record {} is not writable: {error}",
            temporary.display()
        ));
    }
    if let Err(error) = fs::rename(&temporary, &path) {
        fail(format_args!(
            "the record could not be renamed to {}: {error}",
            path.display()
        ));
    }
    println!("wrote {}", path.display());
}

/// The fixture the record is written to and the check reads.
fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/regexp_members.txt")
}

/// The text escaped so that one probe stays on one line.
fn escaped(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\t' => escaped.push_str("\\t"),
            '\n' => escaped.push_str("\\n"),
            '\\' => escaped.push_str("\\\\"),
            character if character < ' ' => {
                escaped.push_str(&format!("\\x{:02x}", u32::from(character)));
            }
            character => escaped.push(character),
        }
    }
    escaped
}

/// Reports the failure and exits without writing a record.
fn fail(message: std::fmt::Arguments<'_>) -> ! {
    eprintln!("regexp_oracle: {message}");
    std::process::exit(1)
}
