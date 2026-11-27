//! The `RegExp` class against the engine's recorded answers.
//!
//! `tests/fixtures/regexp_members.txt` holds what the engine answered for the
//! class's surface and for one call per rule; `crates/tjs/examples/regexp_oracle.rs`
//! records it. The check below replays the probes through the member table in
//! `tjs::vm::runtime::natives::regexp` and compares the kind and the text of
//! every answer. The class's `last` property belongs to the class object, which
//! the table records and the part that registers the class owns; those probes
//! are recorded and named, not replayed.
//!
//! A record that holds only the header is a failure, not a pass: the table must
//! be recorded before the check means anything.
//!
//! reference/krkr2/cpp/core/tjs2/tjsRegExp.cpp:49-81 — the flags text
//! reference/krkr2/cpp/core/tjs2/tjsRegExp.cpp:269-301 — the literal form
//! reference/krkr2/cpp/core/tjs2/tjsRegExp.cpp:303-328 — test
//! reference/krkr2/cpp/core/tjs2/tjsRegExp.cpp:330-380 — match and exec
//! reference/krkr2/cpp/core/tjs2/tjsRegExp.cpp:382-459 — replace
//! reference/krkr2/cpp/core/tjs2/tjsRegExp.cpp:461-505 — split
//! reference/krkr2/cpp/core/tjs2/tjsRegExp.cpp:507-645 — the properties
//! reference/krkr2/cpp/core/tjs2/tjsRegExp.cpp:647-679 — compile and Match
//! reference/krkr2/cpp/core/tjs2/tjsRegExp.cpp:681-725 — Exec's state
//! reference/krkr2/cpp/core/tjs2/tjsRegExp.cpp:727-757 — the result array
//! reference/krkr2/cpp/core/tjs2/tjsString.h:74-80 — the prefix constructor

use std::fs;
use std::path::PathBuf;
use std::rc::Rc;

use tjs::vm::runtime::natives::regexp::{RegExp, RegExpError, RegExpMember, RegExpObject};
use tjs::vm::runtime::object::{Dispatch, MemberFlags, ObjectError};
use tjs::vm::runtime::string::TjsString;
use tjs::vm::runtime::variant::{Closure, Variant};

/// The kind a raised probe records.
const RAISED: &str = "!raise";

/// The probes whose answer belongs to the class object and is recorded only.
const DEFERRED: &[&str] = &[
    "static_last_typeof",
    "static_last_after_test",
    "static_last_after_match",
];

/// The probes the record must hold, in the recorder's order.
const PROBES: &[&str] = &[
    "surface_class_typeof",
    "surface_finalize",
    "surface_compile",
    "surface_compile_literal",
    "surface_test",
    "surface_match",
    "surface_exec",
    "surface_replace",
    "surface_split",
    "surface_matches",
    "surface_start",
    "surface_index",
    "surface_last_index",
    "surface_input",
    "surface_last_match",
    "surface_last_paren",
    "surface_left_context",
    "surface_right_context",
    "surface_last",
    "static_last_typeof",
    "static_last_after_test",
    "static_last_after_match",
    "absent_to_string",
    "absent_flags",
    "absent_source",
    "absent_global",
    "absent_ignore_case",
    "absent_multiline",
    "value_initial_state",
    "value_constructor_pattern",
    "value_constructor_flags_ignorecase",
    "value_constructor_flags_global",
    "value_constructor_unknown_flag",
    "value_constructor_l_flag",
    "value_constructor_number_pattern",
    "value_constructor_number_target",
    "value_constructor_empty_test_text",
    "value_constructor_empty_test_empty",
    "value_constructor_empty_exec",
    "value_compile_replaces",
    "value_compile_extra",
    "value_compile_empty",
    "value_compile_missing",
    "value_literal_compile",
    "value_literal_compile_global",
    "value_literal_format_double_slash",
    "value_literal_format_no_close",
    "value_literal_format_extra",
    "value_literal_bad_pattern",
    "value_invalid_pattern_constructor",
    "value_invalid_pattern_compile",
    "value_exec_match_array",
    "value_exec_match_state",
    "value_exec_index_bug",
    "value_exec_start_bug",
    "value_exec_miss_state",
    "value_exec_global_advance",
    "value_exec_global_group_end",
    "value_exec_global_no_match",
    "value_test_returns",
    "value_test_state",
    "value_match_no_state",
    "value_match_failure",
    "value_matches_unmatched_group",
    "value_lastparen_last_group",
    "value_lastparen_unmatched",
    "value_exec_emoji",
    "value_exec_emoji_global",
    "value_exec_ignorecase",
    "value_exec_dot_newline",
    "value_replace_literal",
    "value_replace_global",
    "value_replace_first",
    "value_replace_callback",
    "value_replace_callback_this",
    "value_replace_callback_number",
    "value_replace_no_match",
    "value_replace_empty_pattern",
    "value_replace_missing",
    "value_replace_extra",
    "value_replace_no_state",
    "value_split_basic",
    "value_split_keep_empty",
    "value_split_purge_empty",
    "value_split_leading_trailing_keep",
    "value_split_leading_trailing_purge",
    "value_split_no_match",
    "value_split_empty_target",
    "value_split_captures_ignored",
    "value_split_global",
    "value_split_start_ignored",
    "value_split_missing",
    "value_split_reserved",
    "value_split_empty_pattern",
    "value_split_no_state",
    "value_start_set_text",
    "value_start_set_real",
    "value_start_set_void",
    "value_start_set_negative",
    "value_readonly_index",
    "value_readonly_matches",
    "value_readonly_last",
];

#[test]
fn the_regexp_class_reproduces_the_recorded_answers() {
    let records = recorded_answers();
    let seen: Vec<&str> = records.iter().map(|record| record.probe.as_str()).collect();
    assert_eq!(
        seen.as_slice(),
        PROBES,
        "the record's probes are not the table this check knows; record it again with \
         `cargo run -p tjs --example regexp_oracle`"
    );
    for record in &records {
        if DEFERRED.contains(&record.probe.as_str()) {
            continue;
        }
        match replay(&record.probe) {
            Ok(answer) => {
                assert_ne!(
                    record.kind, RAISED,
                    "probe {}: the call answers where the engine raised",
                    record.probe
                );
                assert_eq!(
                    record.kind,
                    answer.type_name(),
                    "probe {}: the kind",
                    record.probe
                );
                let text = answer.to_string().unwrap_or_else(|error| {
                    panic!("probe {}: the answer has no text: {error}", record.probe)
                });
                assert_eq!(
                    record.text,
                    escaped(&text),
                    "probe {}: the text",
                    record.probe
                );
            }
            Err(refused) => {
                assert_eq!(
                    record.kind, RAISED,
                    "probe {}: the call raised where the engine answered",
                    record.probe
                );
                if !record.text.is_empty() {
                    assert_eq!(
                        record.text,
                        escaped(&TjsString::from(refused.message.as_str())),
                        "probe {}: the raised message",
                        record.probe
                    );
                }
            }
        }
    }
}

/// The failure a probe's replay produced.
struct Refused {
    /// The message a script would read, when the rule names one.
    message: String,
}

impl From<RegExpError> for Refused {
    fn from(error: RegExpError) -> Self {
        Self::from_error(error)
    }
}

impl Refused {
    /// A refusal with no message.
    fn empty() -> Self {
        Self {
            message: String::new(),
        }
    }

    /// The refusal a table call produced.
    fn from_error(error: RegExpError) -> Self {
        match error {
            RegExpError::Compile { message } => Self {
                message: String::from_utf16_lossy(message.as_utf16()),
            },
            _ => Self::empty(),
        }
    }
}

/// The answer the table gives for one recorded probe.
fn replay(probe: &str) -> Result<Variant, Refused> {
    match probe {
        "surface_class_typeof" => Ok(Variant::String(TjsString::from("Object"))),
        "surface_finalize" => surface("finalize"),
        "surface_compile" => surface("compile"),
        "surface_compile_literal" => surface("_compile"),
        "surface_test" => surface("test"),
        "surface_match" => surface("match"),
        "surface_exec" => surface("exec"),
        "surface_replace" => surface("replace"),
        "surface_split" => surface("split"),
        "surface_matches" => surface("matches"),
        "surface_start" => surface("start"),
        "surface_index" => surface("index"),
        "surface_last_index" => surface("lastIndex"),
        "surface_input" => surface("input"),
        "surface_last_match" => surface("lastMatch"),
        "surface_last_paren" => surface("lastParen"),
        "surface_left_context" => surface("leftContext"),
        "surface_right_context" => surface("rightContext"),
        "surface_last" => surface("last"),
        "static_last_typeof" | "static_last_after_test" | "static_last_after_match" => {
            Err(Refused::empty())
        }
        "absent_to_string" => surface("toString"),
        "absent_flags" => surface("flags"),
        "absent_source" => surface("source"),
        "absent_global" => surface("global"),
        "absent_ignore_case" => surface("ignoreCase"),
        "absent_multiline" => surface("multiline"),
        "value_initial_state" => Ok(answer_text(state(&regexp("a", None)))),
        "value_constructor_pattern" => {
            let mut re = regexp("a", None);
            Ok(integer(re.test(&[text("a")])?))
        }
        "value_constructor_flags_ignorecase" => {
            let mut re = regexp("a", Some("i"));
            Ok(integer(re.test(&[text("A")])?))
        }
        "value_constructor_flags_global" => {
            let mut re = regexp("a", Some("g"));
            Ok(integer(re.test(&[text("aa")])?))
        }
        "value_constructor_unknown_flag" => {
            let mut re = regexp("a", Some("x"));
            Ok(integer(re.test(&[text("a")])?))
        }
        "value_constructor_l_flag" => {
            let mut re = regexp("a", Some("l"));
            Ok(integer(re.test(&[text("a")])?))
        }
        "value_constructor_number_pattern" => {
            let mut re = RegExp::new(&[Variant::from(1i64)]).map_err(Refused::from_error)?;
            Ok(integer(re.test(&[text("1")])?))
        }
        "value_constructor_number_target" => {
            let mut re = regexp("1", None);
            Ok(integer(re.test(&[Variant::from(1i64)])?))
        }
        "value_constructor_empty_test_text" => {
            let mut re = regexp("", None);
            Ok(integer(re.test(&[text("abc")])?))
        }
        "value_constructor_empty_test_empty" => {
            let mut re = regexp("", None);
            Ok(integer(re.test(&[text("")])?))
        }
        "value_constructor_empty_exec" => {
            let mut re = regexp("", None);
            re.exec(&[text("abc")]).map_err(Refused::from_error)?;
            Ok(answer_text(state(&re)))
        }
        "value_compile_replaces" => {
            let mut re = regexp("a", None);
            re.compile(&[text("b")]).map_err(Refused::from_error)?;
            Ok(integer(re.test(&[text("b")])?))
        }
        "value_compile_extra" => {
            let mut re = regexp("a", None);
            re.compile(&[text("b"), text("i"), text("x")])
                .map_err(Refused::from_error)?;
            Ok(integer(re.test(&[text("B")])?))
        }
        "value_compile_empty" => {
            let mut re = regexp("a", None);
            re.compile(&[text("")]).map_err(Refused::from_error)?;
            Ok(integer(re.test(&[text("x")])?))
        }
        "value_compile_missing" => {
            let mut re = regexp("a", None);
            re.compile(&[]).map_err(Refused::from_error)?;
            Ok(Variant::Void)
        }
        "value_literal_compile" => {
            let mut re = RegExp::new(&[]).map_err(Refused::from_error)?;
            re.compile_literal(&[text("//i/a")])
                .map_err(Refused::from_error)?;
            Ok(integer(re.test(&[text("A")])?))
        }
        "value_literal_compile_global" => {
            let mut re = RegExp::new(&[]).map_err(Refused::from_error)?;
            re.compile_literal(&[text("//g/a")])
                .map_err(Refused::from_error)?;
            let first = re.test(&[text("aa")])?;
            let one = state(&re);
            let second = re.test(&[text("aa")])?;
            let two = state(&re);
            let third = re.test(&[text("aa")])?;
            Ok(answer_text(format!(
                "{}:{}:{}|{}|{}|{}",
                i64::from(first),
                i64::from(second),
                i64::from(third),
                one,
                two,
                state(&re)
            )))
        }
        "value_literal_format_double_slash" => {
            let mut re = RegExp::new(&[]).map_err(Refused::from_error)?;
            re.compile_literal(&[text("a")])
                .map_err(Refused::from_error)?;
            Ok(Variant::Void)
        }
        "value_literal_format_no_close" => {
            let mut re = RegExp::new(&[]).map_err(Refused::from_error)?;
            re.compile_literal(&[text("//a")])
                .map_err(Refused::from_error)?;
            Ok(Variant::Void)
        }
        "value_literal_format_extra" => {
            let mut re = RegExp::new(&[]).map_err(Refused::from_error)?;
            re.compile_literal(&[text("//a/"), Variant::from(1i64)])
                .map_err(Refused::from_error)?;
            Ok(Variant::Void)
        }
        "value_literal_bad_pattern" => {
            let mut re = RegExp::new(&[]).map_err(Refused::from_error)?;
            re.compile_literal(&[text("///(")])
                .map_err(Refused::from_error)?;
            Ok(Variant::Void)
        }
        "value_invalid_pattern_constructor" => {
            RegExp::new(&[text("(")]).map_err(Refused::from_error)?;
            Ok(Variant::Void)
        }
        "value_invalid_pattern_compile" => {
            let mut re = regexp("a", None);
            re.compile(&[text("(")]).map_err(Refused::from_error)?;
            Ok(Variant::Void)
        }
        "value_exec_match_array" => {
            let mut re = regexp("a(b)", None);
            re.exec(&[text("ab")]).map_err(Refused::from_error)?;
            Ok(answer_text(match_array(&re)?))
        }
        "value_exec_match_state" => {
            let mut re = regexp("a(b)", None);
            re.exec(&[text("ab")]).map_err(Refused::from_error)?;
            Ok(answer_text(state(&re)))
        }
        "value_exec_index_bug" => {
            let mut re = regexp("b", None);
            re.exec(&[text("ab")]).map_err(Refused::from_error)?;
            Ok(answer_text(state(&re)))
        }
        "value_exec_start_bug" => {
            let mut re = regexp("b", None);
            re.set_start(&Variant::Integer(1))
                .expect("the start is writable");
            re.exec(&[text("ab")]).map_err(Refused::from_error)?;
            Ok(answer_text(state(&re)))
        }
        "value_exec_miss_state" => {
            let mut re = regexp("a", None);
            re.exec(&[text("ab")]).map_err(Refused::from_error)?;
            re.exec(&[text("zz")]).map_err(Refused::from_error)?;
            Ok(answer_text(state(&re)))
        }
        "value_exec_global_advance" => {
            let mut re = regexp("a", Some("g"));
            let first = re.test(&[text("aa")])?;
            let one = state(&re);
            let second = re.test(&[text("aa")])?;
            let two = state(&re);
            let third = re.test(&[text("aa")])?;
            Ok(answer_text(format!(
                "{}:{}:{}|{}|{}|{}",
                i64::from(first),
                i64::from(second),
                i64::from(third),
                one,
                two,
                state(&re)
            )))
        }
        "value_exec_global_group_end" => {
            let mut re = regexp("a(?=(bc))", Some("g"));
            re.exec(&[text("abc")]).map_err(Refused::from_error)?;
            Ok(answer_text(state(&re)))
        }
        "value_exec_global_no_match" => {
            let mut re = regexp("z", Some("g"));
            re.exec(&[text("abc")]).map_err(Refused::from_error)?;
            Ok(answer_text(state(&re)))
        }
        "value_test_returns" => {
            let mut re = regexp("a", None);
            let yes = re.test(&[text("a")])?;
            let no = re.test(&[text("z")])?;
            Ok(answer_text(format!("{}:{}", i64::from(yes), i64::from(no))))
        }
        "value_test_state" => {
            let mut re = regexp("a(b)", None);
            re.test(&[text("ab")]).map_err(Refused::from_error)?;
            Ok(answer_text(state(&re)))
        }
        "value_match_no_state" => {
            let mut re = regexp("a", None);
            let array = re.match_text(&[text("a")]).map_err(Refused::from_error)?;
            let count = array_field(&array, "count")?;
            let first = array_element(&array, 0)?;
            Ok(answer_text(format!("{}|{}|{}", count, first, state(&re))))
        }
        "value_match_failure" => {
            let mut re = regexp("z", None);
            let array = re.match_text(&[text("abc")]).map_err(Refused::from_error)?;
            let count: i64 = array_field(&array, "count")?
                .parse()
                .map_err(|_| Refused::empty())?;
            Ok(Variant::Integer(count))
        }
        "value_matches_unmatched_group" => {
            let mut re = regexp("(a)(b)?", None);
            re.exec(&[text("a")]).map_err(Refused::from_error)?;
            let count = match_array_field(&re, "count")?;
            let first = match_array_element(&re, 0)?;
            let third = match_array_element(&re, 2)?;
            Ok(answer_text(format!(
                "{}|{}|{}|{}|{}",
                count,
                first.type_name(),
                display(&first),
                third.type_name(),
                display(&third)
            )))
        }
        "value_lastparen_last_group" => {
            let mut re = regexp("(a)(b)", None);
            re.exec(&[text("ab")]).map_err(Refused::from_error)?;
            Ok(Variant::String(re.last_paren().clone()))
        }
        "value_lastparen_unmatched" => {
            let mut re = regexp("(a)(b)?", None);
            re.exec(&[text("a")]).map_err(Refused::from_error)?;
            Ok(Variant::String(re.last_paren().clone()))
        }
        "value_exec_emoji" => {
            let mut re = regexp(".", None);
            re.exec(&[text("😀")]).map_err(Refused::from_error)?;
            let count = match_array_field(&re, "count")?;
            let first = match_array_element(&re, 0)?;
            Ok(answer_text(format!(
                "{}|{}|{}|{}|{}|{}",
                count,
                display(&first),
                re.index(),
                re.last_index(),
                re.last_match(),
                re.last_paren()
            )))
        }
        "value_exec_emoji_global" => {
            let mut re = regexp(".", Some("g"));
            let first = re.exec(&[text("😀a")]).map_err(Refused::from_error)?;
            let first_count = array_field(&first, "count")?;
            let first_text = array_element(&first, 0)?;
            let one = state(&re);
            let second = re.exec(&[text("😀a")]).map_err(Refused::from_error)?;
            let second_count = array_field(&second, "count")?;
            let second_text = array_element(&second, 0)?;
            Ok(answer_text(format!(
                "{}|{}|{}|{}|{}|{}",
                first_count,
                first_text,
                one,
                second_count,
                second_text,
                state(&re)
            )))
        }
        "value_exec_ignorecase" => {
            let mut re = regexp("ä", Some("i"));
            Ok(integer(re.test(&[text("Ä")])?))
        }
        "value_exec_dot_newline" => {
            let mut re = regexp(".", None);
            let newline = re.test(&[text("\n")])?;
            let letter = re.test(&[text("a")])?;
            Ok(answer_text(format!(
                "{}:{}",
                i64::from(newline),
                i64::from(letter)
            )))
        }
        "value_replace_literal" => {
            let mut re = regexp("b", None);
            call_replace(&mut re, "abc", "$1", None)
        }
        "value_replace_global" => {
            let mut re = regexp("a", Some("g"));
            call_replace(&mut re, "aaa", "x", None)
        }
        "value_replace_first" => {
            let mut re = regexp("a", None);
            call_replace(&mut re, "aaa", "x", None)
        }
        "value_replace_callback" => {
            let mut re = regexp("(a)", None);
            call_replace(&mut re, "ab", Callback::Words, None)
        }
        "value_replace_callback_this" => {
            let object = RegExpObject::new(&[text("a")]).map_err(Refused::from_error)?;
            let mut state = object.state_mut();
            let answer = state
                .replace(
                    &[text("a"), callback(Callback::This)],
                    Some(object.as_ref() as &dyn Dispatch),
                )
                .map_err(Refused::from_error)?;
            Ok(answer)
        }
        "value_replace_callback_number" => {
            let mut re = regexp("a", None);
            call_replace(&mut re, "a", Callback::Number, None)
        }
        "value_replace_no_match" => {
            let mut re = regexp("a", None);
            call_replace(&mut re, "zzz", "x", None)
        }
        "value_replace_empty_pattern" => {
            let mut re = regexp("", None);
            call_replace(&mut re, "abc", "x", None)
        }
        "value_replace_missing" => {
            let mut re = regexp("a", None);
            re.replace(&[text("abc")], None)
                .map_err(Refused::from_error)?;
            Ok(Variant::Void)
        }
        "value_replace_extra" => {
            let mut re = regexp("a", None);
            re.replace(&[text("abc"), text("x"), Variant::from(9i64)], None)
                .map_err(Refused::from_error)
        }
        "value_replace_no_state" => {
            let mut re = regexp("a", None);
            re.exec(&[text("ab")]).map_err(Refused::from_error)?;
            let before = state(&re);
            let out = re
                .replace(&[text("ab"), text("x")], None)
                .map_err(Refused::from_error)?;
            Ok(answer_text(format!(
                "{}|{}|{}",
                display(&out),
                before,
                state(&re)
            )))
        }
        "value_split_basic" => split_fields("a,b,c", ",", false, false),
        "value_split_keep_empty" => split_fields("a,,b", ",", false, false),
        "value_split_purge_empty" => split_fields("a,,b", ",", true, false),
        "value_split_leading_trailing_keep" => split_fields(",a,", ",", false, false),
        "value_split_leading_trailing_purge" => split_fields(",a,", ",", true, false),
        "value_split_no_match" => split_fields("abc", ",", false, false),
        "value_split_empty_target" => split_fields("", ",", false, false),
        "value_split_captures_ignored" => split_fields("a,b", "(,)", false, false),
        "value_split_global" => split_fields("a,b,c", ",", false, true),
        "value_split_start_ignored" => {
            let mut re = regexp(",", None);
            re.set_start(&Variant::Integer(1))
                .expect("the start is writable");
            let answer = re.split(&[text("a,b")]).map_err(Refused::from_error)?;
            let count = array_field(&answer, "count")?;
            let first = array_element(&answer, 0)?;
            let second = array_element(&answer, 1)?;
            Ok(answer_text(format!("{}|{}|{}", count, first, second)))
        }
        "value_split_missing" => {
            let mut re = regexp(",", None);
            re.split(&[]).map_err(Refused::from_error)?;
            Ok(Variant::Void)
        }
        "value_split_reserved" => split_fields("a,b", ",", true, false),
        "value_split_empty_pattern" => split_fields("abc", "", false, false),
        "value_split_no_state" => {
            let mut re = regexp(",", None);
            re.exec(&[text("a,b")]).map_err(Refused::from_error)?;
            let before = state(&re);
            let array = re.split(&[text("a,b")]).map_err(Refused::from_error)?;
            let count = array_field(&array, "count")?;
            Ok(answer_text(format!("{}|{}|{}", count, before, state(&re))))
        }
        "value_start_set_text" => start_value(Variant::from("2")),
        "value_start_set_real" => start_value(Variant::from(1.9f64)),
        "value_start_set_void" => start_value(Variant::Void),
        "value_start_set_negative" => start_value(Variant::from(-1i64)),
        "value_readonly_index" => readonly(RegExpMember::Index),
        "value_readonly_matches" => readonly(RegExpMember::Matches),
        "value_readonly_last" => readonly(RegExpMember::Last),
        other => panic!("the check has no replay for the recorded probe: {other}"),
    }
}

/// A member's type on an instance.
fn surface(name: &str) -> Result<Variant, Refused> {
    let object = RegExpObject::new(&[text("a")]).map_err(Refused::from_error)?;
    match object.prop_get(&TjsString::from(name), MemberFlags::NONE, None) {
        Ok(value) => Ok(Variant::String(TjsString::from(value.type_name()))),
        Err(ObjectError::MemberNotFound) => Ok(Variant::String(TjsString::from("undefined"))),
        Err(_) => Err(Refused::empty()),
    }
}

/// A `RegExp` with a pattern and optional flags.
fn regexp(pattern: &str, flags: Option<&str>) -> RegExp {
    let mut arguments = vec![text(pattern)];
    if let Some(flags) = flags {
        arguments.push(text(flags));
    }
    RegExp::new(&arguments)
        .unwrap_or_else(|error| panic!("the probe's pattern {pattern:?} must compile: {error}"))
}

/// The state a probe's text describes.
fn state(re: &RegExp) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{}|{}",
        re.start(),
        re.index(),
        re.last_index(),
        re.last_match(),
        re.last_paren(),
        re.left_context(),
        re.right_context(),
        re.matches().type_name()
    )
}

/// The match array a probe's text describes.
fn match_array(re: &RegExp) -> Result<String, Refused> {
    let count = match_array_field(re, "count")?;
    let first = match_array_element(re, 0)?;
    let second = match_array_element(re, 1)?;
    let third = match_array_element(re, 2)?;
    Ok(format!(
        "{}|{}|{}|{}",
        count,
        display(&first),
        display(&second),
        display(&third)
    ))
}

/// A named field of the instance's match array.
fn match_array_field(re: &RegExp, name: &str) -> Result<String, Refused> {
    let Variant::Object(closure) = re.matches() else {
        return Err(Refused::empty());
    };
    let array = closure.object.as_ref().ok_or_else(Refused::empty)?;
    let value = array
        .prop_get(&TjsString::from(name), MemberFlags::NONE, None)
        .map_err(|_| Refused::empty())?;
    Ok(display(&value))
}

/// One element of the instance's match array.
fn match_array_element(re: &RegExp, index: i64) -> Result<Variant, Refused> {
    let Variant::Object(closure) = re.matches() else {
        return Err(Refused::empty());
    };
    let array = closure.object.as_ref().ok_or_else(Refused::empty)?;
    array
        .prop_get_by_num(index, MemberFlags::NONE, None)
        .map_err(|_| Refused::empty())
}

/// A named field of an array answer.
fn array_field(answer: &Variant, name: &str) -> Result<String, Refused> {
    let Variant::Object(closure) = answer else {
        return Err(Refused::empty());
    };
    let array = closure.object.as_ref().ok_or_else(Refused::empty)?;
    let value = array
        .prop_get(&TjsString::from(name), MemberFlags::NONE, None)
        .map_err(|_| Refused::empty())?;
    Ok(display(&value))
}

/// One element of an array answer.
fn array_element(answer: &Variant, index: i64) -> Result<String, Refused> {
    let Variant::Object(closure) = answer else {
        return Err(Refused::empty());
    };
    let array = closure.object.as_ref().ok_or_else(Refused::empty)?;
    let value = array
        .prop_get_by_num(index, MemberFlags::NONE, None)
        .map_err(|_| Refused::empty())?;
    Ok(display(&value))
}

/// The text of `value`.
fn display(value: &Variant) -> String {
    match value.to_string() {
        Ok(text) => String::from_utf16_lossy(text.as_utf16()),
        Err(error) => panic!("a probe's answer has no text: {error}"),
    }
}

/// A split answer as its count and fields.
fn split_fields(
    target: &str,
    pattern: &str,
    purge: bool,
    global: bool,
) -> Result<Variant, Refused> {
    let mut re = regexp(pattern, if global { Some("g") } else { None });
    let mut arguments = vec![text(target)];
    if purge {
        arguments.push(Variant::Void);
        arguments.push(Variant::Integer(1));
    }
    let answer = re.split(&arguments).map_err(Refused::from_error)?;
    let count = array_field(&answer, "count")?;
    let Variant::Object(closure) = &answer else {
        return Err(Refused::empty());
    };
    let array = closure.object.as_ref().ok_or_else(Refused::empty)?;
    let count_number: usize = count.parse().map_err(|_| Refused::empty())?;
    let mut fields = Vec::with_capacity(count_number);
    for index in 0..count_number {
        let index = i64::try_from(index).map_err(|_| Refused::empty())?;
        let value = array
            .prop_get_by_num(index, MemberFlags::NONE, None)
            .map_err(|_| Refused::empty())?;
        fields.push(display(&value));
    }
    Ok(answer_text(format!("{}|{}", count, fields.join("|"))))
}

/// A replacement's answer.
fn call_replace(
    re: &mut RegExp,
    target: &str,
    replacement: impl IntoReplacement,
    receiver: Option<&dyn Dispatch>,
) -> Result<Variant, Refused> {
    let arguments = [text(target), replacement.into_replacement()];
    re.replace(&arguments, receiver)
        .map_err(Refused::from_error)
}

/// A value used as a replacement.
trait IntoReplacement {
    /// The replacement value.
    fn into_replacement(self) -> Variant;
}

impl IntoReplacement for &str {
    fn into_replacement(self) -> Variant {
        text(self)
    }
}

impl IntoReplacement for Callback {
    fn into_replacement(self) -> Variant {
        callback(self)
    }
}

/// The script-visible replacement functions the probes use.
#[derive(Clone, Copy)]
enum Callback {
    /// Builds a text from the whole match and its first group.
    Words,
    /// Answers the kind of the call's receiver.
    This,
    /// Answers a number, whose text the replacement uses.
    Number,
}

/// The object value a callback stands for.
fn callback(callback: Callback) -> Variant {
    let object: Rc<dyn Dispatch> = Rc::new(Replacement { callback });
    Variant::Object(Closure {
        object: Some(object),
        objthis: None,
    })
}

/// A replacement function as an object.
struct Replacement {
    /// The answer the function gives.
    callback: Callback,
}

impl Dispatch for Replacement {
    fn prop_get(
        &self,
        _name: &TjsString,
        _flags: MemberFlags,
        _objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        Err(ObjectError::MemberNotFound)
    }

    fn prop_set(
        &self,
        _name: &TjsString,
        _flags: MemberFlags,
        _value: &Variant,
        _objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        Err(ObjectError::MemberNotFound)
    }

    fn delete_member(
        &self,
        _name: &TjsString,
        _flags: MemberFlags,
        _objthis: Option<&dyn Dispatch>,
    ) -> Result<(), ObjectError> {
        Err(ObjectError::MemberNotFound)
    }

    fn enum_members(
        &self,
        _flags: MemberFlags,
        _callback: &mut dyn FnMut(&TjsString, u32, &Variant) -> bool,
    ) -> Result<(), ObjectError> {
        Ok(())
    }

    fn is_instance_of(
        &self,
        class: &TjsString,
        _objthis: Option<&dyn Dispatch>,
    ) -> Result<bool, ObjectError> {
        let object = TjsString::from("Object");
        let function = TjsString::from("Function");
        Ok(*class == object || *class == function)
    }

    fn class_names(&self) -> Vec<TjsString> {
        Vec::new()
    }

    fn add_class_name(&self, _name: &TjsString) -> Result<(), ObjectError> {
        Err(ObjectError::CallNotServed)
    }

    fn is_valid(&self) -> bool {
        true
    }

    fn invalidate(&self) -> bool {
        false
    }

    fn func_call(
        &self,
        _name: &TjsString,
        argv: Option<&[Variant]>,
        objthis: Option<&dyn Dispatch>,
    ) -> Result<Variant, ObjectError> {
        match self.callback {
            Callback::Words => {
                let Some(Variant::Object(closure)) = argv.and_then(|values| values.first()) else {
                    return Err(ObjectError::CallNotServed);
                };
                let array = closure.object.as_ref().ok_or(ObjectError::CallNotServed)?;
                let first = array.prop_get_by_num(0, MemberFlags::NONE, None)?;
                let second = array.prop_get_by_num(1, MemberFlags::NONE, None)?;
                let first = first.to_string().map_err(|_| ObjectError::CallNotServed)?;
                let second = second.to_string().map_err(|_| ObjectError::CallNotServed)?;
                let text = TjsString::from("[")
                    .concat(&first)
                    .concat(&TjsString::from("/"))
                    .concat(&second)
                    .concat(&TjsString::from("]"));
                Ok(Variant::String(text))
            }
            Callback::This => Ok(Variant::String(TjsString::from(if objthis.is_some() {
                "Object"
            } else {
                "void"
            }))),
            Callback::Number => Ok(Variant::Integer(42)),
        }
    }
}

/// The start a probe writes and reads back.
fn start_value(value: Variant) -> Result<Variant, Refused> {
    let mut re = regexp("a", None);
    RegExpMember::Start
        .set(&mut re, &value)
        .map_err(Refused::from_error)?;
    RegExpMember::Start.get(&re).map_err(Refused::from_error)
}

/// A write to a read-only member.
fn readonly(member: RegExpMember) -> Result<Variant, Refused> {
    let mut re = regexp("a", None);
    member
        .set(&mut re, &Variant::from(1i64))
        .map_err(Refused::from_error)?;
    Ok(Variant::Void)
}

/// A text variant.
fn text(value: &str) -> Variant {
    Variant::String(TjsString::from(value))
}

/// An integer answer.
fn integer(value: bool) -> Variant {
    Variant::Integer(i64::from(value))
}

/// A text answer.
fn answer_text(value: String) -> Variant {
    Variant::String(TjsString::from(value))
}

/// The text escaped so that one probe stays on one line.
fn escaped(text: &TjsString) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in String::from_utf16_lossy(text.as_utf16()).chars() {
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

/// One recorded answer.
struct Record {
    /// The probe's name.
    probe: String,
    /// The kind `typeof` reports for the answer.
    kind: String,
    /// The answer as text, escaped.
    text: String,
}

/// The records of the fixture; a record with no data line is a failure.
fn recorded_answers() -> Vec<Record> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/regexp_members.txt");
    let table = match fs::read_to_string(&path) {
        Ok(table) => table,
        Err(error) => panic!(
            "the member fixture {} is not readable: {error}; record it with \
             `cargo run -p tjs --example regexp_oracle`",
            path.display()
        ),
    };
    let mut records = Vec::new();
    for line in table.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split('\t');
        let (Some(probe), Some(kind), Some(text)) = (fields.next(), fields.next(), fields.next())
        else {
            panic!("the fixture line is not `probe<TAB>kind<TAB>text`: {line:?}");
        };
        assert!(
            fields.next().is_none(),
            "the fixture line has more than three fields: {line:?}"
        );
        records.push(Record {
            probe: probe.to_owned(),
            kind: kind.to_owned(),
            text: text.to_owned(),
        });
    }
    assert!(
        !records.is_empty(),
        "the member fixture records nothing; record it with \
         `cargo run -p tjs --example regexp_oracle`"
    );
    records
}
