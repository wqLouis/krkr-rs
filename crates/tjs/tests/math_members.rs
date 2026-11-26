//! The `Math` class's statics against the engine's recorded answers.
//!
//! `tests/fixtures/math_members.txt` holds what the engine answered for the
//! class's surface and for one call per rule; `crates/tjs/examples/math_oracle.rs`
//! records it. The check below replays the calls through the member table in
//! `tjs::vm::runtime::natives::math` and compares the kind and the text of
//! every answer. The generator-backed `random` member and the members the
//! class object owns are recorded and deferred: their state belongs to the
//! part that owns the class.
//!
//! A record that holds only the header is a failure, not a pass: the table
//! must be recorded before the check means anything.

use std::fs;
use std::path::PathBuf;

use tjs::vm::runtime::natives::math::{MathError, MathMember};
use tjs::vm::runtime::octet::Octet;
use tjs::vm::runtime::string::TjsString;
use tjs::vm::runtime::variant::{Closure, Variant};

/// The kind a raised probe records.
const RAISED: &str = "!raise";

/// The class's own members, which this module's table must publish.
const SURFACE: &[(&str, &str)] = &[
    ("surface_abs", "abs"),
    ("surface_acos", "acos"),
    ("surface_asin", "asin"),
    ("surface_atan", "atan"),
    ("surface_atan2", "atan2"),
    ("surface_ceil", "ceil"),
    ("surface_cos", "cos"),
    ("surface_exp", "exp"),
    ("surface_floor", "floor"),
    ("surface_log", "log"),
    ("surface_max", "max"),
    ("surface_min", "min"),
    ("surface_pow", "pow"),
    ("surface_round", "round"),
    ("surface_sin", "sin"),
    ("surface_sqrt", "sqrt"),
    ("surface_tan", "tan"),
    ("surface_E", "E"),
    ("surface_LOG2E", "LOG2E"),
    ("surface_LOG10E", "LOG10E"),
    ("surface_LN10", "LN10"),
    ("surface_LN2", "LN2"),
    ("surface_PI", "PI"),
    ("surface_SQRT1_2", "SQRT1_2"),
    ("surface_SQRT2", "SQRT2"),
];

/// The members the class object owns, recorded but not replayed.
const DEFERRED_SURFACE: &[(&str, &str, &str)] = &[
    ("deferred_surface_finalize", "finalize", "Object"),
    ("deferred_surface_random", "random", "Object"),
    (
        "deferred_surface_RandomGenerator",
        "RandomGenerator",
        "Object",
    ),
];

/// Names a script might expect but the class does not publish.
const ABSENT: &[(&str, &str)] = &[
    ("absent_cbrt", "cbrt"),
    ("absent_hypot", "hypot"),
    ("absent_int", "int"),
    ("absent_log10", "log10"),
    ("absent_log2", "log2"),
    ("absent_octetVectorSum", "octetVectorSum"),
    ("absent_percent", "%"),
    ("absent_rand", "rand"),
    ("absent_sign", "sign"),
    ("absent_transcendental", "transcendental"),
    ("absent_trunc", "trunc"),
];

/// The calls whose answer the check replays, in the recorder's order.
const VALUES: &[&str] = &[
    "value_constant_e",
    "value_constant_log2e",
    "value_constant_log10e",
    "value_constant_ln10",
    "value_constant_ln2",
    "value_constant_pi",
    "value_constant_sqrt1_2",
    "value_constant_sqrt2",
    "value_abs_integer",
    "value_abs_real",
    "value_abs_negative_zero",
    "value_abs_text",
    "value_abs_word",
    "value_abs_empty_text",
    "value_abs_void",
    "value_abs_extra",
    "value_abs_missing",
    "value_abs_object",
    "value_abs_octet",
    "value_floor_positive",
    "value_floor_negative",
    "value_floor_integer",
    "value_floor_text",
    "value_floor_missing",
    "value_ceil_positive",
    "value_ceil_negative",
    "value_ceil_integer",
    "value_ceil_missing",
    "value_round_half",
    "value_round_half_negative",
    "value_round_below_half",
    "value_round_negative_zero",
    "value_round_negative_small",
    "value_round_text",
    "value_round_missing",
    "value_sin_zero",
    "value_sin_half_pi",
    "value_sin_text",
    "value_sin_missing",
    "value_cos_zero",
    "value_cos_pi",
    "value_cos_missing",
    "value_tan_zero",
    "value_tan_quarter_pi",
    "value_tan_missing",
    "value_asin_one",
    "value_asin_domain",
    "value_asin_missing",
    "value_acos_one",
    "value_acos_domain",
    "value_acos_missing",
    "value_atan_one",
    "value_atan_text",
    "value_atan_missing",
    "value_atan2_quarter",
    "value_atan2_text",
    "value_atan2_zero_zero",
    "value_atan2_negative_zero",
    "value_atan2_one_argument",
    "value_atan2_missing",
    "value_exp_zero",
    "value_exp_one",
    "value_exp_text",
    "value_exp_missing",
    "value_log_one",
    "value_log_e",
    "value_log_zero",
    "value_log_negative",
    "value_log_missing",
    "value_pow_integer",
    "value_pow_negative",
    "value_pow_fractional",
    "value_pow_domain",
    "value_pow_zero_zero",
    "value_pow_text",
    "value_pow_one_argument",
    "value_pow_missing",
    "value_sqrt_integer",
    "value_sqrt_two",
    "value_sqrt_negative",
    "value_sqrt_text",
    "value_sqrt_missing",
    "value_max_none",
    "value_max_one",
    "value_max_mixed",
    "value_max_negative",
    "value_max_text",
    "value_max_positive_zero",
    "value_max_negative_zero",
    "value_max_nan",
    "value_max_object",
    "value_min_none",
    "value_min_one",
    "value_min_mixed",
    "value_min_positive_zero",
    "value_min_negative_zero",
    "value_min_nan",
    "value_min_text",
    "value_min_octet",
];

/// The probes whose rules belong to the class object: the recorded answer.
const DEFERRED_VALUES: &[(&str, &str, &str)] = &[
    ("deferred_value_random_kind", "String", "Real"),
    ("deferred_value_random_range", "Integer", "1"),
];

// reference/cpp/core/tjs2/tjsMath.cpp:108-236 — the unary and binary statics
// reference/cpp/core/tjs2/tjsMath.cpp:238-308 — max, min and the signed zeroes
// reference/cpp/core/tjs2/tjsMath.cpp:318-381 — round, sin, cos, sqrt, tan
// reference/cpp/core/tjs2/tjsMath.cpp:383-459 — the constants
// reference/cpp/core/tjs2/tjsMath.cpp:310-316 — random, deferred with the generator
// reference/cpp/core/tjs2/tjs.cpp:166-172 — the RandomGenerator member the class object carries
#[test]
fn the_math_members_reproduce_the_recorded_answers() {
    let records = recorded_answers();
    let mut seen = Vec::with_capacity(records.len());
    for record in &records {
        seen.push(record.probe.as_str());
        match record.probe.as_str() {
            probe if probe.starts_with("surface_") => check_surface(record),
            probe if probe.starts_with("deferred_surface_") => check_deferred_surface(record),
            probe if probe.starts_with("absent_") => check_absent(record),
            probe if probe.starts_with("value_") => check_value(record),
            probe if probe.starts_with("deferred_value_") => check_deferred_value(record),
            other => panic!("the record holds a probe the check does not know: {other}"),
        }
    }
    assert_eq!(
        seen,
        expected_probes(),
        "the record's probes are not the table this check knows; record it again with \
         `cargo run -p tjs --example math_oracle`"
    );
}

/// The probes the record must hold, in the recorder's order.
fn expected_probes() -> Vec<&'static str> {
    let mut probes = Vec::new();
    probes.extend(SURFACE.iter().map(|(probe, _)| *probe));
    probes.extend(DEFERRED_SURFACE.iter().map(|(probe, _, _)| *probe));
    probes.extend(ABSENT.iter().map(|(probe, _)| *probe));
    probes.extend(VALUES.iter().copied());
    probes.extend(DEFERRED_VALUES.iter().map(|(probe, _, _)| *probe));
    probes
}

/// Checks a member's surface record: it is present, and it is the function or
/// the constant this table says it is.
fn check_surface(record: &Record) {
    let (_, member) = SURFACE
        .iter()
        .find(|(probe, _)| record.probe.as_str() == *probe)
        .unwrap_or_else(|| panic!("the surface table does not hold {}", record.probe));
    assert_eq!(
        record.kind, "String",
        "probe {}: the surface kind",
        record.probe
    );
    let member = MathMember::lookup(&TjsString::from(*member)).unwrap_or_else(|| {
        panic!(
            "probe {}: the class publishes {member}, which the table does not",
            record.probe
        )
    });
    match record.text.as_str() {
        "Object" => assert!(
            member.min_params().is_some(),
            "probe {}: a function must have an arity",
            record.probe
        ),
        "Real" => {
            assert!(
                member.min_params().is_none(),
                "probe {}: a constant must not have an arity",
                record.probe
            );
            assert!(
                member.value().is_some(),
                "probe {}: the constant has no value",
                record.probe
            );
        }
        other => panic!(
            "probe {}: the surface answers {other}, not a function or a constant",
            record.probe
        ),
    }
}

/// Checks a member the class object owns: the engine answers for it.
fn check_deferred_surface(record: &Record) {
    let (_, member, expected) = DEFERRED_SURFACE
        .iter()
        .find(|(probe, _, _)| record.probe.as_str() == *probe)
        .unwrap_or_else(|| panic!("the deferred surface table does not hold {}", record.probe));
    assert_eq!(
        record.kind, "String",
        "probe {}: the surface kind",
        record.probe
    );
    assert_eq!(
        record.text, *expected,
        "probe {}: the member {member} is not the surface the engine answers",
        record.probe
    );
}

/// Checks a name the class does not publish: the table must not publish it.
fn check_absent(record: &Record) {
    let (_, member) = ABSENT
        .iter()
        .find(|(probe, _)| record.probe.as_str() == *probe)
        .unwrap_or_else(|| panic!("the absent table does not hold {}", record.probe));
    assert_eq!(
        record.kind, "String",
        "probe {}: the surface kind",
        record.probe
    );
    assert_eq!(
        record.text, "undefined",
        "probe {}: the class publishes {member}",
        record.probe
    );
    assert!(
        MathMember::lookup(&TjsString::from(*member)).is_none(),
        "probe {}: the table publishes {member}, which is not a member",
        record.probe
    );
}

/// Checks a call: the implementation's kind and text are the recorded ones.
fn check_value(record: &Record) {
    if record.kind == RAISED {
        assert_eq!(
            record.text, RAISED,
            "probe {}: the raise has a text",
            record.probe
        );
        assert!(
            replay(&record.probe).is_err(),
            "probe {}: the call answers where the engine raised",
            record.probe
        );
        return;
    }
    let value = match replay(&record.probe) {
        Ok(value) => value,
        Err(error) => panic!(
            "probe {}: the call raised {error} where the engine answered",
            record.probe
        ),
    };
    assert_eq!(
        record.kind,
        value.type_name(),
        "probe {}: the kind",
        record.probe
    );
    let text = value
        .to_string()
        .unwrap_or_else(|error| panic!("probe {}: the value has no text: {error}", record.probe));
    assert_eq!(
        record.text,
        text.to_string(),
        "probe {}: the text",
        record.probe
    );
}

/// Checks a probe the class object owns, without replaying it.
fn check_deferred_value(record: &Record) {
    let (_, kind, text) = DEFERRED_VALUES
        .iter()
        .find(|(probe, _, _)| record.probe.as_str() == *probe)
        .unwrap_or_else(|| panic!("the deferred value table does not hold {}", record.probe));
    assert_eq!(record.kind, *kind, "probe {}: the kind", record.probe);
    assert_eq!(record.text, *text, "probe {}: the text", record.probe);
}

/// The answer the member table gives for one recorded call.
fn replay(probe: &str) -> Result<Variant, MathError> {
    use MathMember as Member;
    match probe {
        "value_constant_e" => constant(Member::E),
        "value_constant_log2e" => constant(Member::Log2E),
        "value_constant_log10e" => constant(Member::Log10E),
        "value_constant_ln10" => constant(Member::Ln10),
        "value_constant_ln2" => constant(Member::Ln2),
        "value_constant_pi" => constant(Member::Pi),
        "value_constant_sqrt1_2" => constant(Member::Sqrt1_2),
        "value_constant_sqrt2" => constant(Member::Sqrt2),
        "value_abs_integer" => Member::Abs.call(&[Variant::Integer(-5)]),
        "value_abs_real" => Member::Abs.call(&[Variant::Real(-1.5)]),
        "value_abs_negative_zero" => Member::Abs.call(&[Variant::Real(-0.0)]),
        "value_abs_text" => Member::Abs.call(&[Variant::from("-5")]),
        "value_abs_word" => Member::Abs.call(&[Variant::from("abc")]),
        "value_abs_empty_text" => Member::Abs.call(&[Variant::from("")]),
        "value_abs_void" => Member::Abs.call(&[Variant::Void]),
        "value_abs_extra" => Member::Abs.call(&[Variant::Integer(-2), Variant::Integer(3)]),
        "value_abs_missing" => Member::Abs.call(&[]),
        "value_abs_object" => Member::Abs.call(&[object()]),
        "value_abs_octet" => Member::Abs.call(&[octet()]),
        "value_floor_positive" => Member::Floor.call(&[Variant::Real(3.7)]),
        "value_floor_negative" => Member::Floor.call(&[Variant::Real(-3.2)]),
        "value_floor_integer" => Member::Floor.call(&[Variant::Integer(3)]),
        "value_floor_text" => Member::Floor.call(&[Variant::from("3.7")]),
        "value_floor_missing" => Member::Floor.call(&[]),
        "value_ceil_positive" => Member::Ceil.call(&[Variant::Real(3.2)]),
        "value_ceil_negative" => Member::Ceil.call(&[Variant::Real(-3.7)]),
        "value_ceil_integer" => Member::Ceil.call(&[Variant::Integer(3)]),
        "value_ceil_missing" => Member::Ceil.call(&[]),
        "value_round_half" => Member::Round.call(&[Variant::Real(2.5)]),
        "value_round_half_negative" => Member::Round.call(&[Variant::Real(-2.5)]),
        "value_round_below_half" => Member::Round.call(&[Variant::Real(2.4999)]),
        "value_round_negative_zero" => Member::Round.call(&[Variant::Real(-0.5)]),
        "value_round_negative_small" => Member::Round.call(&[Variant::Real(-0.6)]),
        "value_round_text" => Member::Round.call(&[Variant::from("2.5")]),
        "value_round_missing" => Member::Round.call(&[]),
        "value_sin_zero" => Member::Sin.call(&[Variant::Integer(0)]),
        "value_sin_half_pi" => Member::Sin.call(&[Variant::Real(std::f64::consts::PI / 2.0)]),
        "value_sin_text" => Member::Sin.call(&[Variant::from("0")]),
        "value_sin_missing" => Member::Sin.call(&[]),
        "value_cos_zero" => Member::Cos.call(&[Variant::Integer(0)]),
        "value_cos_pi" => Member::Cos.call(&[Variant::Real(std::f64::consts::PI)]),
        "value_cos_missing" => Member::Cos.call(&[]),
        "value_tan_zero" => Member::Tan.call(&[Variant::Integer(0)]),
        "value_tan_quarter_pi" => Member::Tan.call(&[Variant::Real(std::f64::consts::PI / 4.0)]),
        "value_tan_missing" => Member::Tan.call(&[]),
        "value_asin_one" => Member::Asin.call(&[Variant::Integer(1)]),
        "value_asin_domain" => Member::Asin.call(&[Variant::Integer(2)]),
        "value_asin_missing" => Member::Asin.call(&[]),
        "value_acos_one" => Member::Acos.call(&[Variant::Integer(1)]),
        "value_acos_domain" => Member::Acos.call(&[Variant::Integer(2)]),
        "value_acos_missing" => Member::Acos.call(&[]),
        "value_atan_one" => Member::Atan.call(&[Variant::Integer(1)]),
        "value_atan_text" => Member::Atan.call(&[Variant::from("1")]),
        "value_atan_missing" => Member::Atan.call(&[]),
        "value_atan2_quarter" => Member::Atan2.call(&[Variant::Integer(1), Variant::Integer(1)]),
        "value_atan2_text" => Member::Atan2.call(&[Variant::from("2"), Variant::from("1")]),
        "value_atan2_zero_zero" => Member::Atan2.call(&[Variant::Integer(0), Variant::Integer(0)]),
        "value_atan2_negative_zero" => {
            Member::Atan2.call(&[Variant::Real(-0.0), Variant::Integer(1)])
        }
        "value_atan2_one_argument" => Member::Atan2.call(&[Variant::Integer(1)]),
        "value_atan2_missing" => Member::Atan2.call(&[]),
        "value_exp_zero" => Member::Exp.call(&[Variant::Integer(0)]),
        "value_exp_one" => Member::Exp.call(&[Variant::Integer(1)]),
        "value_exp_text" => Member::Exp.call(&[Variant::from("0")]),
        "value_exp_missing" => Member::Exp.call(&[]),
        "value_log_one" => Member::Log.call(&[Variant::Integer(1)]),
        "value_log_e" => Member::Log.call(&[Variant::Real(std::f64::consts::E)]),
        "value_log_zero" => Member::Log.call(&[Variant::Integer(0)]),
        "value_log_negative" => Member::Log.call(&[Variant::Integer(-1)]),
        "value_log_missing" => Member::Log.call(&[]),
        "value_pow_integer" => Member::Pow.call(&[Variant::Integer(2), Variant::Integer(10)]),
        "value_pow_negative" => Member::Pow.call(&[Variant::Integer(2), Variant::Integer(-1)]),
        "value_pow_fractional" => Member::Pow.call(&[Variant::Integer(9), Variant::Real(0.5)]),
        "value_pow_domain" => Member::Pow.call(&[Variant::Integer(-2), Variant::Real(0.5)]),
        "value_pow_zero_zero" => Member::Pow.call(&[Variant::Integer(0), Variant::Integer(0)]),
        "value_pow_text" => Member::Pow.call(&[Variant::from("2"), Variant::from("3")]),
        "value_pow_one_argument" => Member::Pow.call(&[Variant::Integer(2)]),
        "value_pow_missing" => Member::Pow.call(&[]),
        "value_sqrt_integer" => Member::Sqrt.call(&[Variant::Integer(9)]),
        "value_sqrt_two" => Member::Sqrt.call(&[Variant::Integer(2)]),
        "value_sqrt_negative" => Member::Sqrt.call(&[Variant::Integer(-1)]),
        "value_sqrt_text" => Member::Sqrt.call(&[Variant::from("16")]),
        "value_sqrt_missing" => Member::Sqrt.call(&[]),
        "value_max_none" => Member::Max.call(&[]),
        "value_max_one" => Member::Max.call(&[Variant::Integer(3)]),
        "value_max_mixed" => {
            Member::Max.call(&[Variant::Integer(1), Variant::Real(2.5), Variant::Integer(2)])
        }
        "value_max_negative" => Member::Max.call(&[Variant::Integer(-3), Variant::Integer(-2)]),
        "value_max_text" => Member::Max.call(&[Variant::from("10"), Variant::Integer(9)]),
        "value_max_positive_zero" => Member::Max.call(&[Variant::Real(-0.0), Variant::Real(0.0)]),
        "value_max_negative_zero" => Member::Max.call(&[Variant::Real(-0.0), Variant::Real(-0.0)]),
        "value_max_nan" => Member::Max.call(&[
            Variant::Integer(1),
            Variant::Real(f64::NAN),
            Variant::Integer(2),
        ]),
        "value_max_object" => Member::Max.call(&[Variant::Integer(1), object()]),
        "value_min_none" => Member::Min.call(&[]),
        "value_min_one" => Member::Min.call(&[Variant::Integer(3)]),
        "value_min_mixed" => {
            Member::Min.call(&[Variant::Integer(1), Variant::Real(2.5), Variant::Integer(2)])
        }
        "value_min_positive_zero" => Member::Min.call(&[Variant::Real(0.0), Variant::Real(-0.0)]),
        "value_min_negative_zero" => Member::Min.call(&[Variant::Real(-0.0), Variant::Real(-0.0)]),
        "value_min_nan" => Member::Min.call(&[Variant::Integer(1), Variant::Real(f64::NAN)]),
        "value_min_text" => Member::Min.call(&[Variant::from("10"), Variant::Integer(9)]),
        "value_min_octet" => Member::Min.call(&[octet()]),
        other => panic!("the check has no replay for the recorded probe: {other}"),
    }
}

/// The value of a constant member.
fn constant(member: MathMember) -> Result<Variant, MathError> {
    member.value().ok_or(MathError::NotCallable)
}

/// An object value, which has no real.
fn object() -> Variant {
    Variant::Object(Closure::null())
}

/// An octet value, which has no real.
fn octet() -> Variant {
    Variant::from(Octet::from_slice(&[0]))
}

/// One recorded answer.
struct Record {
    /// The probe's name.
    probe: String,
    /// The kind `typeof` reports for the answer.
    kind: String,
    /// The answer as text.
    text: String,
}

/// The records of the fixture; a record with no data line is a failure.
fn recorded_answers() -> Vec<Record> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/math_members.txt");
    let table = fs::read_to_string(&path).expect("read the math member fixture");
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
        "the math member fixture records nothing; record it with \
         `cargo run -p tjs --example math_oracle`"
    );
    records
}
