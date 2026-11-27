//! Records what the executor answers for three systematic probe tables.
//!
//! The tables prove the language surface the interpreter runs, one probe at a
//! time. The **answer** table is 130 expressions: the literals and the kinds
//! they carry, the unary and binary operators, the comparisons, the text and
//! collection members, the conversions and `typeof`, `in` and `instanceof`.
//! Every operator's operands come through `probeValue`, so the constant folder
//! cannot answer an instruction at compile time. The **ops** table is 81
//! scripts, each declaring one function and calling it: the frame and constant
//! instructions, the condition flag and the jumps, the step instructions, the
//! operators in their plain and member forms, the unary instructions, calls,
//! members, exceptions and the context instructions. Each script travels with
//! its record, so the check replays exactly the script that was recorded. The
//! **throw** table is 20 scripts whose body must raise at run time, caught in
//! the script, recording the message a handler reads.
//!
//! An answer is the kind the language's `typeof` names and the value's text,
//! joined by `probeAnswer` in one call, so a probe is evaluated once. A probe
//! that answers an object or a byte string is refused: an object's text is the
//! two addresses it carries and a byte string has no text form, so neither is
//! reproducible. Such a probe asks for a `typeof`, a member or a scalar
//! instead.
//!
//! The records are written atomically: each whole table is written beside its
//! fixture and renamed over it, so a failed run leaves the old record in place.
//! Each table is recorded on its own engine, with the setup script run first,
//! so the environment a record is made in is the environment its check runs in.
//!
//! ```sh
//! cargo run -p tjs --example interpreter_oracle
//! ```

use std::fs;
use std::path::PathBuf;

use tjs::{Tjs2Engine, TjsValue};

/// The answer fixture's first line, so a check can tell a recorded table from
/// a hand-written or stale one.
const HEADER_ANSWERS: &str = "# the engine's answers, recorded by `crates/tjs/examples/interpreter_oracle.rs` (the answer table); regenerate with `cargo run -p tjs --example interpreter_oracle`";

/// The ops fixture's first line.
const HEADER_OPS: &str = "# the engine's answers, recorded by `crates/tjs/examples/interpreter_oracle.rs` (the ops table, each row carrying its script); regenerate with `cargo run -p tjs --example interpreter_oracle`";

/// The throw fixture's first line.
const HEADER_THROW: &str = "# the engine's answers, recorded by `crates/tjs/examples/interpreter_oracle.rs` (the throw table, each row carrying its script); regenerate with `cargo run -p tjs --example interpreter_oracle`";

/// The sentinel a throw script answers when its body did not raise.
const NO_CATCH: &str = "!nocatch";

/// The script that gives the probes their helpers.
///
/// `probeValue` is the constant-folder bypass: an operand the folder cannot see
/// through, so the instruction runs. `probeAnswer` answers the kind and the
/// text in one call, so a probe is evaluated once. The other helpers exist so a
/// probe can ask for a member or a call without repeating the setup.
const SETUP: &str = r#"function probeValue(value) {
    return value;
}
function probeAnswer(value) {
    return (typeof value) + "|" + ("" + value);
}
function probeAddOne(value) {
    return value + 1;
}
function probeSumThree(a, b, c) {
    return a + b + c;
}
function probeThisMember() {
    return this.n;
}
var probeGlobalValue = 0;
function probeReadGlobal() {
    return probeGlobalValue;
}
function probeDictionary() {
    var d = new Dictionary();
    d.n = 11;
    d.s = "text";
    d.v = void;
    return d;
}
function probeArray() {
    var a = new Array();
    a[0] = 10;
    a[1] = 20;
    a.push(30);
    return a;
}
"#;

/// The answer probes: the id the check names the rule by, and the expression.
///
/// Every expression answers a scalar, a `typeof` or a member that is one; an
/// object or a byte string is refused by the recorder.
const ANSWER_PROBES: &[(&str, &str)] = &[
    // the literals and the kind each one carries
    ("literal_integer", "42"),
    ("literal_integer_negative", "-42"),
    ("literal_integer_hexadecimal", "0x2a"),
    ("literal_integer_binary", "0b1010"),
    ("literal_integer_largest", "9223372036854775807"),
    ("literal_real_decimal", "1.25"),
    ("literal_real_exponent", "1.5e3"),
    ("literal_real_negative_zero", "-0.0"),
    ("literal_real_infinity", "1e400"),
    ("literal_real_not_a_number", "0.0 / 0.0"),
    ("literal_text_plain", r#""abc""#),
    ("literal_text_empty", r#""""#),
    ("literal_text_escapes", r#""a\tb\n\\""#),
    ("literal_text_wide", r#""あいう""#),
    ("literal_octet_typeof", "typeof <%01 02 03%>"),
    ("literal_regexp_typeof", "typeof /a/"),
    // the unary operators
    ("unary_plus_text", r#"+probeValue("12")"#),
    ("unary_minus_real", "-probeValue(1.5)"),
    ("unary_minus_text", r#"-probeValue("3")"#),
    ("unary_not_zero", "!probeValue(0)"),
    ("unary_not_text_zero", r#"!probeValue("0")"#),
    ("unary_not_empty_text", r#"!probeValue("")"#),
    ("unary_complement", "~probeValue(5)"),
    ("unary_complement_negative", "~probeValue(-1)"),
    ("unary_character_code", r#"#probeValue("A")"#),
    ("unary_character_from", "$probeValue(65)"),
    // integer arithmetic
    ("add_integers", "probeValue(2) + probeValue(3)"),
    ("subtract_integers", "probeValue(2) - probeValue(3)"),
    ("multiply_integers", "probeValue(6) * probeValue(7)"),
    ("divide_integers", "probeValue(7) / probeValue(2)"),
    ("divide_exact", "probeValue(8) / probeValue(2)"),
    (r#"integer_divide"#, r#"probeValue(7) \ probeValue(2)"#),
    (
        "integer_divide_negative",
        r#"probeValue(-7) \ probeValue(2)"#,
    ),
    ("remainder", "probeValue(7) % probeValue(3)"),
    ("remainder_negative", "probeValue(-7) % probeValue(3)"),
    (
        "add_large_integers",
        "probeValue(4294967296) + probeValue(4294967296)",
    ),
    (
        "multiply_large_integers",
        "probeValue(1000000) * probeValue(1000000)",
    ),
    ("truncate_real", "(int)probeValue(3.9)"),
    // real and mixed arithmetic
    ("add_reals", "probeValue(1.5) + probeValue(2.25)"),
    ("subtract_reals", "probeValue(1.5) - probeValue(2.25)"),
    ("multiply_real_integer", "probeValue(1.5) * probeValue(2)"),
    ("divide_reals", "probeValue(1.0) / probeValue(4.0)"),
    ("add_integer_real", "probeValue(1) + probeValue(2.5)"),
    ("multiply_negative_zero", "probeValue(0.0) * probeValue(-1)"),
    ("add_number_text", r#"probeValue(1) + probeValue("2")"#),
    ("add_text_number", r#"probeValue("a") + probeValue(1)"#),
    // the comparisons
    ("equal_integers", "probeValue(1) == probeValue(1)"),
    ("equal_integer_real", "probeValue(1) == probeValue(1.0)"),
    ("equal_integer_text", r#"probeValue(1) == probeValue("1")"#),
    ("equal_texts", r#"probeValue("a") == probeValue("a")"#),
    (
        "equal_void_zero",
        r#"(new Dictionary())["x"] == probeValue(0)"#,
    ),
    ("not_equal", "probeValue(1) != probeValue(2)"),
    ("less", "probeValue(1) < probeValue(2)"),
    ("greater", "probeValue(2) > probeValue(1)"),
    ("less_or_equal", "probeValue(2) <= probeValue(2)"),
    ("greater_or_equal", "probeValue(2) >= probeValue(2)"),
    ("less_texts", r#"probeValue("a") < probeValue("b")"#),
    ("less_text_number", r#"probeValue("10") < probeValue(9)"#),
    ("strict_equal_integers", "probeValue(1) === probeValue(1)"),
    (
        "strict_equal_integer_real",
        "probeValue(1) === probeValue(1.0)",
    ),
    ("strict_not_equal", "probeValue(1) !== probeValue(1.0)"),
    (
        "equal_not_a_number",
        "probeValue(0.0 / 0.0) == probeValue(0.0 / 0.0)",
    ),
    // the logical rules
    ("logical_and_true", "probeValue(1) && probeValue(1)"),
    ("logical_and_false", "probeValue(1) && probeValue(0)"),
    ("logical_or_false", "probeValue(0) || probeValue(0)"),
    ("logical_or_true", "probeValue(0) || probeValue(1)"),
    (
        "logical_and_short_circuit",
        r#"probeValue(0) && (new Dictionary())["x"]"#,
    ),
    (
        "logical_or_short_circuit",
        r#"probeValue(1) || (new Dictionary())["x"]"#,
    ),
    (
        "ternary_true",
        r#"probeValue(1) ? probeValue("yes") : probeValue("no")"#,
    ),
    (
        "ternary_false",
        r#"probeValue(0) ? probeValue("yes") : probeValue("no")"#,
    ),
    // the bitwise rules and the shifts
    ("bit_and", "probeValue(6) & probeValue(3)"),
    ("bit_or", "probeValue(6) | probeValue(3)"),
    ("bit_xor", "probeValue(6) ^ probeValue(3)"),
    ("shift_left", "probeValue(1) << probeValue(4)"),
    ("shift_right", "probeValue(-8) >> probeValue(1)"),
    ("shift_right_unsigned", "probeValue(-8) >>> probeValue(1)"),
    ("shift_real", "probeValue(1.9) << probeValue(1)"),
    ("bit_not_zero", "~probeValue(0)"),
    ("bit_and_negative", "probeValue(-1) & probeValue(255)"),
    ("shift_count_large", "probeValue(1) << probeValue(64)"),
    // the text surface
    ("text_add", r#"probeValue("a") + probeValue("b")"#),
    ("text_add_number", r#"probeValue("a") + probeValue(1)"#),
    ("text_length", r#"probeValue("abc").length"#),
    ("text_length_wide", r#"probeValue("😀").length"#),
    ("text_index", r#"probeValue("abc")[1]"#),
    (
        "text_index_of",
        r#"probeValue("abcabc").indexOf(probeValue("b"), probeValue(2))"#,
    ),
    (
        "text_substring",
        r#"probeValue("abcdef").substring(probeValue(2), probeValue(3))"#,
    ),
    ("text_upper", r#"probeValue("aBc").toUpperCase()"#),
    ("text_char_at", r#"probeValue("abc").charAt(probeValue(1))"#),
    ("text_compare", r#"probeValue("abc") < probeValue("abd")"#),
    (
        "text_sprintf",
        r#"probeValue("%d-%s").sprintf(probeValue(5), probeValue("x"))"#,
    ),
    ("text_trim", r#"probeValue("  a  ").trim()"#),
    // the conversions
    ("convert_integer_text", r#"(int)probeValue("12")"#),
    ("convert_integer_text_spaced", r#"(int)probeValue(" 12 ")"#),
    ("convert_integer_real", "(int)probeValue(3.99)"),
    ("convert_integer_real_negative", "(int)probeValue(-3.99)"),
    ("convert_real_text", r#"(real)probeValue("1.5")"#),
    ("convert_string_integer", "(string)probeValue(42)"),
    ("convert_string_real", "(string)probeValue(1.5)"),
    ("convert_integer_word", r#"(int)probeValue("abc")"#),
    ("convert_real_word", r#"(real)probeValue("abc")"#),
    ("convert_string_boolean", "(string)probeValue(1 == 1)"),
    // typeof and the members it diversifies
    ("typeof_integer", "typeof probeValue(1)"),
    ("typeof_real", "typeof probeValue(1.5)"),
    ("typeof_string", r#"typeof probeValue("a")"#),
    ("typeof_void", r#"typeof (new Dictionary())["x"]"#),
    ("typeof_null", "typeof null"),
    ("typeof_absent_member", "typeof this.absent"),
    (
        "typeof_member_absent_dictionary",
        "typeof probeDictionary().zz",
    ),
    ("typeof_object", "typeof probeDictionary()"),
    ("typeof_class", "typeof Dictionary"),
    ("dictionary_member", "probeDictionary().n"),
    ("dictionary_index", r#"probeDictionary()["s"]"#),
    ("dictionary_member_void", "typeof probeDictionary().v"),
    ("array_element", "probeArray()[1]"),
    ("array_count", "probeArray().count"),
    // the membership questions
    (r#"in_present"#, r#"probeValue("n") in probeDictionary()"#),
    (r#"in_absent"#, r#"probeValue("z") in probeDictionary()"#),
    (
        "instanceof_dictionary",
        "probeDictionary() instanceof Dictionary",
    ),
    (
        "instanceof_array_false",
        "probeDictionary() instanceof Array",
    ),
    ("instanceof_array_true", "probeArray() instanceof Array"),
    (
        "instanceof_integer_false",
        "probeValue(1) instanceof Dictionary",
    ),
    // the arithmetic class
    ("math_abs", "Math.abs(probeValue(-3))"),
    ("math_floor", "Math.floor(probeValue(1.9))"),
    ("math_ceil", "Math.ceil(probeValue(1.1))"),
    ("math_round_half", "Math.round(probeValue(2.5))"),
    (
        "math_max",
        "Math.max(probeValue(1), probeValue(7), probeValue(3))",
    ),
    (
        "math_min",
        "Math.min(probeValue(1), probeValue(7), probeValue(3))",
    ),
    ("math_pow", "Math.pow(probeValue(2), probeValue(10))"),
    ("math_sqrt", "Math.sqrt(probeValue(16))"),
];

/// The ops probes: the id the check names the rule by, and the body.
///
/// The script the recorder runs is `op(id, body)`: a function declaration and
/// its call, so the body's locals are its own and the script's answer is the
/// body's `return`.
const OPS_PROBES: &[(&str, &str)] = &[
    // the frame and the constant area
    (
        "constant_integer",
        r#"var a = 41;
    return probeAnswer(a);"#,
    ),
    (
        "invalidate_and_isvalid",
        r#"var d = probeDictionary();
    var before = isvalid d;
    invalidate d;
    return probeAnswer(before + ":" + isvalid d);"#,
    ),
    (
        "ignore_property",
        r#"var d = probeDictionary();
    &d.n = probeValue(5);
    return probeAnswer(&d.n);"#,
    ),
    (
        "constant_copies",
        r#"var a = 7;
    var b = a;
    var c = b;
    return probeAnswer(c);"#,
    ),
    (
        "constant_cleared_in_loop",
        r#"var total = 0;
    var i = 0;
    while (i < 3) {
        var t = probeValue(i) * 2;
        total = total + t;
        i = i + 1;
    }
    return probeAnswer(total);"#,
    ),
    // the condition flag and the jumps
    (
        "if_true",
        r#"if (probeValue(1)) {
        return probeAnswer("taken");
    }
    return probeAnswer("skipped");"#,
    ),
    (
        "if_false",
        r#"if (probeValue(0)) {
        return probeAnswer("taken");
    }
    return probeAnswer("skipped");"#,
    ),
    (
        "if_else",
        r#"if (probeValue(0)) {
        return probeAnswer("then");
    } else {
        return probeAnswer("else");
    }"#,
    ),
    (
        "if_not",
        r#"if (!probeValue(0)) {
        return probeAnswer(1);
    }
    return probeAnswer(0);"#,
    ),
    (
        "while_count",
        r#"var i = 0;
    var n = 0;
    while (i < 4) {
        n = n + i;
        i = i + 1;
    }
    return probeAnswer(n);"#,
    ),
    (
        "do_while_count",
        r#"var i = 0;
    do {
        i = i + 1;
    } while (i < 3);
    return probeAnswer(i);"#,
    ),
    (
        "for_count",
        r#"var n = 0;
    for (var i = 0; i < 4; i = i + 1) {
        n = n + i;
    }
    return probeAnswer(n);"#,
    ),
    (
        "break_in_loop",
        r#"var n = 0;
    for (var i = 0; i < 10; i = i + 1) {
        if (i == 3) {
            break;
        }
        n = n + 1;
    }
    return probeAnswer(n);"#,
    ),
    (
        "continue_in_loop",
        r#"var n = 0;
    for (var i = 0; i < 5; i = i + 1) {
        if (i == 2) {
            continue;
        }
        n = n + 1;
    }
    return probeAnswer(n);"#,
    ),
    (
        "condition_inverted_or",
        r#"if ((probeValue(1) >= probeValue(2)) || (probeValue(1) == probeValue(1))) {
        return probeAnswer("yes");
    }
    return probeAnswer("no");"#,
    ),
    (
        "condition_equal",
        r#"if (probeValue(2) == probeValue(2)) {
        return probeAnswer("eq");
    }
    return probeAnswer("ne");"#,
    ),
    (
        "condition_strict_equal",
        r#"if (probeValue(2) === probeValue(2)) {
        return probeAnswer("eq");
    }
    return probeAnswer("ne");"#,
    ),
    (
        "comparison_value",
        r#"var f = probeValue(2) < probeValue(1);
    return probeAnswer(f);"#,
    ),
    (
        "negated_comparison_value",
        r#"var f = !(probeValue(2) < probeValue(1));
    return probeAnswer(f);"#,
    ),
    // the step instructions
    (
        "increment_local",
        r#"var a = 1;
    var b = a++;
    return probeAnswer(a * 10 + b);"#,
    ),
    (
        "decrement_local",
        r#"var a = 2;
    var b = a--;
    return probeAnswer(a * 10 + b);"#,
    ),
    (
        "increment_prefix",
        r#"var a = 1;
    var b = ++a;
    return probeAnswer(a * 10 + b);"#,
    ),
    (
        "decrement_prefix",
        r#"var a = 2;
    var b = --a;
    return probeAnswer(a * 10 + b);"#,
    ),
    (
        "increment_direct_member",
        r#"var d = probeDictionary();
    d.n = 1;
    var b = d.n++;
    return probeAnswer(d.n * 10 + b);"#,
    ),
    (
        "decrement_direct_member",
        r#"var d = probeDictionary();
    d.n = 5;
    var b = d.n--;
    return probeAnswer(d.n * 10 + b);"#,
    ),
    (
        "increment_index_member",
        r#"var d = probeDictionary();
    d["n"] = 1;
    var b = d["n"]++;
    return probeAnswer(d.n * 10 + b);"#,
    ),
    (
        "decrement_index_member",
        r#"var d = probeDictionary();
    d["n"] = 5;
    var b = d["n"]--;
    return probeAnswer(d.n * 10 + b);"#,
    ),
    // the operators, plain
    ("add", "return probeAnswer(probeValue(7) + probeValue(2));"),
    (
        "subtract",
        "return probeAnswer(probeValue(7) - probeValue(2));",
    ),
    (
        "multiply",
        "return probeAnswer(probeValue(7) * probeValue(2));",
    ),
    (
        "divide",
        "return probeAnswer(probeValue(7) / probeValue(2));",
    ),
    (
        "integer_divide",
        r#"return probeAnswer(probeValue(7) \ probeValue(2));"#,
    ),
    (
        "remainder",
        "return probeAnswer(probeValue(7) % probeValue(3));",
    ),
    (
        "bit_or",
        "return probeAnswer(probeValue(6) | probeValue(3));",
    ),
    (
        "bit_xor",
        "return probeAnswer(probeValue(6) ^ probeValue(3));",
    ),
    (
        "bit_and",
        "return probeAnswer(probeValue(6) & probeValue(3));",
    ),
    (
        "shift_right",
        "return probeAnswer(probeValue(-8) >> probeValue(1));",
    ),
    (
        "shift_left",
        "return probeAnswer(probeValue(1) << probeValue(4));",
    ),
    (
        "shift_right_unsigned",
        "return probeAnswer(probeValue(-8) >>> probeValue(1));",
    ),
    (
        "logical_or",
        "return probeAnswer(probeValue(0) || probeValue(1));",
    ),
    (
        "logical_and",
        "return probeAnswer(probeValue(1) && probeValue(1));",
    ),
    // the operators against a member, and the compound forms
    (
        "add_direct_member",
        r#"var d = probeDictionary();
    d.n = 40;
    return probeAnswer(d.n + probeValue(2));"#,
    ),
    (
        "add_index_member",
        r#"var d = probeDictionary();
    d["n"] = 40;
    return probeAnswer(d["n"] + probeValue(2));"#,
    ),
    (
        "compound_add_local",
        r#"var a = 40;
    a += probeValue(2);
    return probeAnswer(a);"#,
    ),
    (
        "compound_add_direct_member",
        r#"var d = probeDictionary();
    d.n = 40;
    d.n += probeValue(2);
    return probeAnswer(d.n);"#,
    ),
    (
        "compound_add_index_member",
        r#"var d = probeDictionary();
    d["n"] = 40;
    d["n"] += probeValue(2);
    return probeAnswer(d.n);"#,
    ),
    (
        "compound_multiply_index_member",
        r#"var d = probeDictionary();
    d["n"] = 4;
    d["n"] *= probeValue(3);
    return probeAnswer(d.n);"#,
    ),
    (
        "text_concat",
        r#"return probeAnswer(probeValue("a") + probeValue(1) + probeValue("b"));"#,
    ),
    // the unary instructions
    ("typeof_value", "return probeAnswer(typeof probeValue(1));"),
    (
        "typeof_direct_member",
        r#"var d = probeDictionary();
    return probeAnswer(typeof d.n);"#,
    ),
    (
        "typeof_index_member",
        r#"var d = probeDictionary();
    return probeAnswer(typeof d["n"]);"#,
    ),
    (
        "number_of_text",
        r#"return probeAnswer(+probeValue("12"));"#,
    ),
    ("negate_text", r#"return probeAnswer(-probeValue("12"));"#),
    ("character_from", "return probeAnswer($probeValue(65));"),
    (
        "convert_integer",
        "return probeAnswer((int)probeValue(3.9));",
    ),
    ("convert_real", "return probeAnswer((real)probeValue(3));"),
    (
        "convert_string",
        "return probeAnswer((string)probeValue(3));",
    ),
    (
        "evaluate_expression",
        r#"var text = probeValue("1+2");
    return probeAnswer(text!);"#,
    ),
    (
        "evaluate_statement",
        r#"probeGlobalValue = 0;
    "probeGlobalValue = 5"!;
    return probeAnswer(probeGlobalValue);"#,
    ),
    // the calls
    (
        "call_declared_function",
        "return probeAnswer(probeAddOne(probeValue(41)));",
    ),
    (
        "call_direct_member",
        r#"return probeAnswer(probeValue("abc").charAt(probeValue(1)));"#,
    ),
    (
        "call_index_member",
        r#"var d = new Dictionary();
    d.f = function (a) { return a * 2; };
    return probeAnswer(d["f"](probeValue(3)));"#,
    ),
    (
        "construct_array",
        r#"var a = new Array();
    a.push(probeValue(4));
    return probeAnswer(a.count);"#,
    ),
    (
        "this_member_write",
        r#"this.probeThisWrite = probeValue(4);
    return probeAnswer(this.probeThisWrite);"#,
    ),
    (
        "call_three_arguments",
        "return probeAnswer(probeSumThree(probeValue(1), probeValue(2), probeValue(3)));",
    ),
    // the member instructions
    (
        "member_read_direct",
        r#"var d = probeDictionary();
    return probeAnswer(d.n);"#,
    ),
    (
        "member_write_direct",
        r#"var d = probeDictionary();
    d.n = probeValue(5);
    return probeAnswer(d.n);"#,
    ),
    (
        "member_read_index",
        r#"var d = probeDictionary();
    return probeAnswer(d["n"]);"#,
    ),
    (
        "member_write_index",
        r#"var d = probeDictionary();
    d["n"] = probeValue(5);
    return probeAnswer(d.n);"#,
    ),
    (
        "member_write_new_direct",
        r#"var d = probeDictionary();
    d.created = probeValue(9);
    return probeAnswer(d.created);"#,
    ),
    (
        "member_write_new_index",
        r#"var d = probeDictionary();
    d["created"] = probeValue(9);
    return probeAnswer(d["created"]);"#,
    ),
    (
        "member_delete_direct",
        r#"var d = probeDictionary();
    delete d.n;
    return probeAnswer(typeof d.n);"#,
    ),
    (
        "member_delete_index",
        r#"var d = probeDictionary();
    delete d["n"];
    return probeAnswer(typeof d["n"]);"#,
    ),
    (
        "member_text_length",
        r#"return probeAnswer(probeValue("abc").length);"#,
    ),
    (
        "member_text_index",
        r#"return probeAnswer(probeValue("abc")[1]);"#,
    ),
    // the exceptions
    (
        "try_catch_script_throw",
        r#"try {
        throw probeValue("thrown");
    } catch (e) {
        return probeAnswer(e);
    }
    return probeAnswer("nocatch");"#,
    ),
    (
        "try_catch_engine_raise",
        r#"try {
        probeValue(1) \ probeValue(0);
    } catch (e) {
        return probeAnswer(e instanceof Exception);
    }
    return probeAnswer("nocatch");"#,
    ),
    (
        "try_catch_continue",
        r#"var n = 0;
    try {
        throw probeValue(1);
    } catch (e) {
        n = 2;
    }
    n = n + 1;
    return probeAnswer(n);"#,
    ),
    // the context instructions
    (
        "in_context_of",
        r#"var d = probeDictionary();
    d.n = probeValue(7);
    return probeAnswer((probeThisMember incontextof d)());"#,
    ),
    (
        "global_read",
        r#"probeGlobalValue = probeValue(5);
    return probeAnswer(probeReadGlobal());"#,
    ),
    (
        "debugger_statement",
        r#"debugger;
    return probeAnswer("continued");"#,
    ),
];

/// The throw probes: the id the check names the rule by, and the body.
///
/// The script the recorder runs is `throw_script(body)`: the body in a `try`,
/// a catch that answers `e.message`, and a sentinel for a body that did not
/// raise. A probe whose body does not raise fails the recorder, because the
/// raise is what the row records.
const THROW_PROBES: &[(&str, &str)] = &[
    ("divide_integer_zero", r#"probeValue(7) \ probeValue(0);"#),
    ("remainder_zero", "probeValue(7) % probeValue(0);"),
    (
        "text_index_past_end",
        r#"probeValue("abc")[probeValue(4)];"#,
    ),
    (
        "text_index_negative",
        r#"probeValue("abc")[probeValue(-1)];"#,
    ),
    (
        "text_length_write",
        r#"probeValue("abc").length = probeValue(1);"#,
    ),
    (
        "octet_index_past_end",
        "probeValue(<%01 02%>)[probeValue(2)];",
    ),
    ("octet_to_string", "(string)probeValue(<%01%>);"),
    ("octet_to_number", "probeValue(<%01%>) * probeValue(1);"),
    ("null_member_read", "probeValue(null).n;"),
    ("integer_call", "probeValue(1)();"),
    ("absent_global_read", "probeAbsentName;"),
    ("absent_global_typeof", "typeof probeAbsentName;"),
    ("absent_global_call", "probeAbsentName();"),
    (
        "new_non_class",
        r#"var C = probeValue(1);
        var x = new C();"#,
    ),
    (
        "sprintf_format",
        r#"var answer = probeValue("%").sprintf();"#,
    ),
    (
        "sprintf_missing",
        r#"var answer = probeValue("%d").sprintf();"#,
    ),
    (
        "unpack_format",
        r#"var answer = probeValue(<%01%>).unpack(probeValue("q"));"#,
    ),
    ("convert_null_integer", "(int)probeValue(null);"),
    ("convert_null_real", "(real)probeValue(null);"),
    ("property_access_integer", "*probeValue(1);"),
];

fn main() {
    let mut problems = Vec::new();
    // Each table gets its own engine and its own setup run, so a record's
    // environment is the setup and that table's probes alone: it is the
    // environment its check runs in.
    let answers = answer_table(&start_engine(), &mut problems);
    let ops = ops_table(&start_engine(), &mut problems);
    let throws = throw_table(&start_engine(), &mut problems);
    if !problems.is_empty() {
        for problem in &problems {
            eprintln!("interpreter_oracle: {problem}");
        }
        fail(format_args!(
            "{} probes did not record; nothing was written",
            problems.len()
        ));
    }
    write_table("interpreter_answers.txt", HEADER_ANSWERS, &answers);
    write_table("interpreter_ops.txt", HEADER_OPS, &ops);
    write_table("interpreter_throw.txt", HEADER_THROW, &throws);
}

/// The engine a table is recorded on, with the setup script run.
fn start_engine() -> Tjs2Engine {
    let engine = match Tjs2Engine::new() {
        Ok(engine) => engine,
        Err(error) => fail(format_args!("the engine did not start: {error}")),
    };
    if let Err(error) = engine.exec_script_discard(SETUP, "interpreter_setup") {
        fail(format_args!("the engine rejected the setup: {error}"));
    }
    engine
}

/// The answer table's rows.
fn answer_table(engine: &Tjs2Engine, problems: &mut Vec<String>) -> Vec<String> {
    let mut table = Vec::with_capacity(ANSWER_PROBES.len());
    for &(probe, expression) in ANSWER_PROBES {
        let script = format!("return probeAnswer({expression});");
        match answer(engine, &script, "interpreter_answers") {
            Ok((kind, text)) => {
                table.push(format!("{probe}\t{kind}\t{}\n", escaped(&text)));
            }
            Err(problem) => problems.push(format!("probe {probe}: {problem}")),
        }
    }
    table
}

/// The ops table's rows, each carrying the script that was run.
fn ops_table(engine: &Tjs2Engine, problems: &mut Vec<String>) -> Vec<String> {
    let mut table = Vec::with_capacity(OPS_PROBES.len());
    for &(probe, body) in OPS_PROBES {
        let script = op_script(probe, body);
        match answer(engine, &script, "interpreter_ops") {
            Ok((kind, text)) => {
                table.push(format!(
                    "{probe}\t{}\t{kind}\t{}\n",
                    escaped(&script),
                    escaped(&text)
                ));
            }
            Err(problem) => problems.push(format!("probe {probe}: {problem}")),
        }
    }
    table
}

/// The throw table's rows, each carrying the script that was run.
fn throw_table(engine: &Tjs2Engine, problems: &mut Vec<String>) -> Vec<String> {
    let mut table = Vec::with_capacity(THROW_PROBES.len());
    for &(probe, body) in THROW_PROBES {
        let script = throw_script(body);
        match engine.exec_script(&script, "interpreter_throw") {
            Ok(TjsValue::String(message)) if message == NO_CATCH => {
                problems.push(format!("probe {probe}: the body did not raise"));
            }
            Ok(TjsValue::String(message)) => {
                table.push(format!(
                    "{probe}\t{}\t{}\n",
                    escaped(&script),
                    escaped(&message)
                ));
            }
            Ok(other) => problems.push(format!(
                "probe {probe}: the answer is not a text: {other:?}"
            )),
            Err(error) => problems.push(format!("probe {probe}: the script failed: {error}")),
        }
    }
    table
}

/// The script an ops probe runs.
fn op_script(probe: &str, body: &str) -> String {
    format!("function op_{probe}() {{\n    {body}\n}}\nreturn op_{probe}();\n")
}

/// The script a throw probe runs.
fn throw_script(body: &str) -> String {
    format!(
        "try {{\n    {body}\n}} catch (e) {{\n    return \"\" + e.message;\n}}\nreturn \"{NO_CATCH}\";\n"
    )
}

/// The kind and the text one probe answered.
///
/// A probe the script itself refuses, or one whose answer is not a text, is a
/// problem: a raise belongs in the throw table, and an object or a byte string
/// has no reproducible text.
fn answer(engine: &Tjs2Engine, script: &str, table: &str) -> Result<(String, String), String> {
    let text = match engine.exec_script(script, table) {
        Ok(TjsValue::String(text)) => text,
        Ok(other) => return Err(format!("the answer is not a text: {other:?}")),
        Err(error) => return Err(format!("the script failed: {error}")),
    };
    let Some((kind, value)) = text.split_once('|') else {
        return Err(format!("the answer has no kind and text: {text:?}"));
    };
    if kind == "Object" || kind == "Octet" {
        return Err(format!(
            "an answer of kind {kind} has no reproducible text; the probe must ask for a typeof, a \
             member or a scalar"
        ));
    }
    Ok((kind.to_owned(), value.to_owned()))
}

/// The table written to its fixture, atomically.
fn write_table(name: &str, header: &str, rows: &[String]) {
    let path = fixture_path(name);
    let mut table = String::new();
    table.push_str(header);
    table.push('\n');
    for row in rows {
        table.push_str(row);
    }
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
    println!("wrote {} ({} rows)", path.display(), rows.len());
}

/// The fixture a table is written to and its check reads.
fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// The text escaped so that one row stays on one line.
///
/// A tab, a newline and a backslash are written as their two-character
/// escapes, and a code unit below `0x20` is written as `\xNN`.
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
    eprintln!("interpreter_oracle: {message}");
    std::process::exit(1)
}
