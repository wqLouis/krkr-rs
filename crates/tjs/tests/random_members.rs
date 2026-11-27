//! The `Math.RandomGenerator` class against the engine's recorded answers.
//!
//! `tests/fixtures/random_members.txt` holds what the engine answered for the
//! class's surface, for one seeded draw per rule, and for the serialised state
//! and its restoration; `crates/tjs/examples/random_oracle.rs` records it. The
//! check below replays the calls through the member table in
//! `tjs::vm::runtime::natives::random` and compares the kind and the text of
//! every answer. The members whose seed is the clock are recorded by their
//! range and deferred: their sequence belongs to the class object.
//!
//! A record that holds only the header is a failure, not a pass: the table
//! must be recorded before the check means anything.

use std::fs;
use std::path::PathBuf;
use std::rc::Rc;

use tjs::vm::runtime::dictionary::Dictionary;
use tjs::vm::runtime::natives::random::{
    RandomError, RandomGenerator, RandomGeneratorMember, Seed, SerializedState,
};
use tjs::vm::runtime::object::{Dispatch, MemberFlags};
use tjs::vm::runtime::string::TjsString;
use tjs::vm::runtime::variant::{Closure, Variant};

/// The kind a raised probe records.
const RAISED: &str = "!raise";

/// The class's own members, which this module's table must publish.
const SURFACE: &[(&str, &str)] = &[
    ("surface_finalize", "finalize"),
    ("surface_randomize", "randomize"),
    ("surface_random", "random"),
    ("surface_random32", "random32"),
    ("surface_random63", "random63"),
    ("surface_random64", "random64"),
    ("surface_serialize", "serialize"),
];

/// Names a script might expect but the class does not publish.
const ABSENT: &[(&str, &str)] = &[
    ("absent_class", "class"),
    ("absent_constructor", "constructor"),
    ("absent_init", "init"),
    ("absent_left", "left"),
    ("absent_next", "next"),
    ("absent_rand", "rand"),
    ("absent_random53", "random53"),
    ("absent_random_double", "randomDouble"),
    ("absent_randomize32", "randomize32"),
    ("absent_seed", "seed"),
    ("absent_set_state", "setState"),
    ("absent_state", "state"),
];

/// The members the class object owns, recorded but not replayed.
const DEFERRED_SURFACE: &[(&str, &str)] = &[
    ("deferred_surface_math_random", "Object"),
    ("deferred_surface_math_random_generator", "Object"),
];

/// The `typeof` answers that are not member names.
const KINDS: &[(&str, &str, &str)] = &[
    ("kind_constructor", "String", "Object"),
    ("kind_math_random", "String", "Real"),
    ("kind_math_random_extra", "String", "Real"),
];

/// The calls whose answer the check replays.
const VALUES: &[&str] = &[
    "value_seed_1_random_4",
    "value_seed_1_random32_6",
    "value_seed_1_random63_3",
    "value_seed_1_random64_3",
    "value_seed_0_random32",
    "value_seed_neg1_random32",
    "value_seed_2p32_random32",
    "value_seed_real_random32",
    "value_seed_text_word_random32",
    "value_seed_text_one_random32",
    "value_seed_empty_random32",
    "value_seed_void_random32",
    "value_seed_extra_random32",
    "value_reseed_seed_random32",
    "value_reseed_extra_random32",
    "value_reseed_text_random32",
    "value_reseed_void_random32",
    "value_reseed_negative_random32",
    "value_serialize_shape",
    "value_serialize_fresh",
    "value_serialize_after_2",
    "value_serialize_after_random",
    "value_restore_continues",
    "value_restore_from_new",
    "value_restore_after_random",
    "value_restore_uppercase",
    "value_state_left_text",
    "value_state_left_word",
    "value_state_left_real",
    "value_state_next_text",
    "value_state_left_624",
    "value_state_left_1",
    "value_state_left_0",
    "value_state_left_neg1",
    "value_state_left_625",
    "value_state_left_2_next_3",
    "value_ctor_octet",
    "value_ctor_null",
    "value_ctor_dictionary",
    "value_randomize_octet",
    "value_randomize_null",
    "value_randomize_dictionary",
    "value_randomize_returns",
    "value_finalize_returns",
    "value_random_extra",
    "value_random32_extra",
    "value_random63_extra",
    "value_random64_extra",
    "value_state_short",
    "value_state_nonhex",
    "value_state_integer",
    "value_state_octet",
    "value_state_void",
    "value_state_missing_state",
    "value_state_missing_left",
    "value_state_missing_next",
];

/// The probes whose rules belong to the class object: the recorded answer.
const DEFERRED_VALUES: &[(&str, &str, &str)] = &[
    ("deferred_value_ctor_default_range", "Integer", "1"),
    ("deferred_value_reseed_default_range", "Integer", "1"),
    ("deferred_value_math_random_range", "Integer", "1"),
    ("deferred_value_math_random_many", "Integer", "1"),
];

// reference/krkr2/cpp/core/tjs2/tjsRandomGenerator.cpp:248-352 — the members
// reference/krkr2/cpp/core/tjs2/tjsRandomGenerator.cpp:44-141 — randomize
// reference/krkr2/cpp/core/tjs2/tjsRandomGenerator.cpp:144-193 — serialize
// reference/krkr2/cpp/core/tjs2/tjsMT19937ar-cok.cpp:50-304 — the generator
#[test]
fn the_random_members_reproduce_the_recorded_answers() {
    let records = recorded_answers();
    let mut seen = Vec::with_capacity(records.len());
    for record in &records {
        seen.push(record.probe.as_str());
        match record.probe.as_str() {
            probe if probe.starts_with("surface_") => check_surface(record),
            probe if probe.starts_with("deferred_surface_") => check_deferred_surface(record),
            probe if probe.starts_with("absent_") => check_absent(record),
            probe if probe.starts_with("kind_") => check_kind(record),
            probe if probe.starts_with("value_") => check_value(record),
            probe if probe.starts_with("deferred_value_") => check_deferred_value(record),
            other => panic!("the record holds a probe the check does not know: {other}"),
        }
    }
    assert_eq!(
        seen,
        expected_probes(),
        "the record's probes are not the table this check knows; record it again with \
         `cargo run -p tjs --example random_oracle`"
    );
}

/// The probes the record must hold, in the recorder's order.
fn expected_probes() -> Vec<&'static str> {
    let mut probes = Vec::new();
    probes.extend(SURFACE.iter().map(|(probe, _)| *probe));
    probes.extend(ABSENT.iter().map(|(probe, _)| *probe));
    probes.extend(DEFERRED_SURFACE.iter().map(|(probe, _)| *probe));
    probes.extend(KINDS.iter().map(|(probe, _, _)| *probe));
    probes.extend(VALUES.iter().copied());
    probes.extend(DEFERRED_VALUES.iter().map(|(probe, _, _)| *probe));
    probes
}

/// Checks a member's surface record: it is present and takes no required
/// parameter.
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
    assert_eq!(
        record.text, "Object",
        "probe {}: the class does not publish {member}",
        record.probe
    );
    let member = RandomGeneratorMember::lookup(&TjsString::from(*member)).unwrap_or_else(|| {
        panic!(
            "probe {}: the class publishes {member}, which the table does not",
            record.probe
        )
    });
    assert_eq!(
        member.min_params(),
        0,
        "probe {}: the member requires no parameter",
        record.probe
    );
}

/// Checks a member the class object owns: the engine answers for it.
fn check_deferred_surface(record: &Record) {
    let (_, expected) = DEFERRED_SURFACE
        .iter()
        .find(|(probe, _)| record.probe.as_str() == *probe)
        .unwrap_or_else(|| panic!("the deferred surface table does not hold {}", record.probe));
    assert_eq!(
        record.kind, "String",
        "probe {}: the surface kind",
        record.probe
    );
    assert_eq!(
        record.text, *expected,
        "probe {}: the member is not the surface the engine answers",
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
        RandomGeneratorMember::lookup(&TjsString::from(*member)).is_none(),
        "probe {}: the table publishes {member}, which is not a member",
        record.probe
    );
}

/// Checks a `typeof` answer.
fn check_kind(record: &Record) {
    let (_, kind, text) = KINDS
        .iter()
        .find(|(probe, _, _)| record.probe.as_str() == *probe)
        .unwrap_or_else(|| panic!("the kind table does not hold {}", record.probe));
    assert_eq!(record.kind, *kind, "probe {}: the kind", record.probe);
    assert_eq!(record.text, *text, "probe {}: the text", record.probe);
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
fn replay(probe: &str) -> Result<Variant, RandomError> {
    use RandomGeneratorMember as Member;
    match probe {
        "value_seed_1_random_4" => draw(1, 1, 4),
        "value_seed_1_random32_6" => draw(1, 32, 6),
        "value_seed_1_random63_3" => draw(1, 63, 3),
        "value_seed_1_random64_3" => draw(1, 64, 3),
        "value_seed_0_random32" => one(0),
        "value_seed_neg1_random32" => one(-1),
        "value_seed_2p32_random32" => one(4_294_967_296),
        "value_seed_real_random32" => one_seeded(Variant::Real(1.9)),
        "value_seed_text_word_random32" => one_seeded(Variant::from("abc")),
        "value_seed_text_one_random32" => one_seeded(Variant::from("1")),
        "value_seed_empty_random32" => one_seeded(Variant::from("")),
        "value_seed_void_random32" => one_seeded(Variant::Void),
        "value_seed_extra_random32" => one_extra(),
        "value_reseed_seed_random32" => reseed(1, Variant::Integer(2), false),
        "value_reseed_extra_random32" => reseed(1, Variant::Integer(2), true),
        "value_reseed_text_random32" => reseed(1, Variant::from("abc"), false),
        "value_reseed_void_random32" => reseed(1, Variant::Void, false),
        "value_reseed_negative_random32" => reseed(1, Variant::Integer(-1), false),
        "value_serialize_shape" => serialized_shape(1),
        "value_serialize_fresh" => serialized(1, 0),
        "value_serialize_after_2" => serialized(1, 2),
        "value_serialize_after_random" => serialized(9, 2),
        "value_restore_continues" => restored(7, 2),
        "value_restore_from_new" => restored_from_new(7, 2),
        "value_restore_after_random" => restored_random(9, 1),
        "value_restore_uppercase" => state_uppercase(1),
        "value_state_left_text" => mutated(1, Variant::from("1"), Variant::Integer(0), 1),
        "value_state_left_word" => mutated(1, Variant::from("abc"), Variant::Integer(0), 1),
        "value_state_left_real" => mutated(1, Variant::Real(1.9), Variant::Integer(0), 1),
        "value_state_next_text" => mutated(1, Variant::Integer(1), Variant::from("0"), 1),
        "value_state_left_624" => mutated(1, Variant::Integer(624), Variant::Integer(0), 1),
        "value_state_left_1" => mutated(1, Variant::Integer(1), Variant::Integer(0), 1),
        "value_state_left_0" => mutated(1, Variant::Integer(0), Variant::Integer(0), 1),
        "value_state_left_neg1" => mutated(1, Variant::Integer(-1), Variant::Integer(0), 1),
        "value_state_left_625" => mutated(1, Variant::Integer(625), Variant::Integer(0), 1),
        "value_state_left_2_next_3" => mutated(1, Variant::Integer(2), Variant::Integer(3), 2),
        "value_ctor_octet" => constructed(Variant::from(octet())),
        "value_ctor_null" => constructed(Variant::Object(Closure::null())),
        "value_ctor_dictionary" => constructed(dictionary()),
        "value_randomize_octet" => randomize_argument(Variant::from(octet())),
        "value_randomize_null" => randomize_argument(Variant::Object(Closure::null())),
        "value_randomize_dictionary" => randomize_argument(dictionary()),
        "value_randomize_returns" => {
            let mut generator = RandomGenerator::seeded(1);
            Member::Randomize
                .call(&mut generator, &[Variant::Integer(2)], Seed::Time(0))
                .map(|_| Variant::String(TjsString::from("void")))
        }
        "value_finalize_returns" => {
            let mut generator = RandomGenerator::seeded(1);
            Member::Finalize
                .call(&mut generator, &[], Seed::Time(0))
                .map(|_| Variant::String(TjsString::from("void")))
        }
        "value_random_extra" => extra(Member::Random),
        "value_random32_extra" => extra(Member::Random32),
        "value_random63_extra" => extra(Member::Random63),
        "value_random64_extra" => extra(Member::Random64),
        "value_state_short" => short_state(1),
        "value_state_nonhex" => nonhex_state(1),
        "value_state_integer" => integer_state(1),
        "value_state_octet" => octet_state(1),
        "value_state_void" => void_state(1),
        "value_state_missing_state" => missing_state(1, 0),
        "value_state_missing_left" => missing_state(1, 1),
        "value_state_missing_next" => missing_state(1, 2),
        other => panic!("the check has no replay for the recorded probe: {other}"),
    }
}

/// The draws a seeded generator makes, joined by commas.
fn draw(seed: i64, which: u32, count: usize) -> Result<Variant, RandomError> {
    let mut generator = RandomGenerator::seeded(seed);
    let mut answers = Vec::with_capacity(count);
    for _ in 0..count {
        let answer = match which {
            1 => Variant::Real(generator.random()?),
            32 => Variant::Integer(i64::from(generator.random32()?)),
            63 => Variant::Integer(generator.random63()?),
            _ => Variant::Integer(generator.random64()?),
        };
        answers.push(answer.to_string()?.to_string());
    }
    Ok(Variant::String(TjsString::from(answers.join(","))))
}

/// One integer-seeded 32-bit answer.
fn one(seed: i64) -> Result<Variant, RandomError> {
    Ok(Variant::Integer(i64::from(
        RandomGenerator::seeded(seed).random32()?,
    )))
}

/// One 32-bit answer from a seed given as a value.
fn one_seeded(seed: Variant) -> Result<Variant, RandomError> {
    let mut generator = RandomGenerator::seeded(0);
    generator.randomize(&[seed], Seed::Time(0))?;
    Ok(Variant::Integer(i64::from(generator.random32()?)))
}

/// One 32-bit answer with two extra arguments.
fn one_extra() -> Result<Variant, RandomError> {
    let mut generator = RandomGenerator::seeded(1);
    generator.randomize(
        &[
            Variant::Integer(1),
            Variant::Integer(2),
            Variant::Integer(3),
        ],
        Seed::Time(0),
    )?;
    Ok(Variant::Integer(i64::from(generator.random32()?)))
}

/// A reseed after one draw, then one answer.
fn reseed(seed: i64, next: Variant, extra: bool) -> Result<Variant, RandomError> {
    let mut generator = RandomGenerator::seeded(seed);
    generator.random32()?;
    if extra {
        generator.randomize(
            &[next, Variant::Integer(3), Variant::Integer(4)],
            Seed::Time(0),
        )?;
    } else {
        generator.randomize(&[next], Seed::Time(0))?;
    }
    Ok(Variant::Integer(i64::from(generator.random32()?)))
}

/// The state text and its two counters.
fn serialized(seed: i64, count: usize) -> Result<Variant, RandomError> {
    let mut generator = RandomGenerator::seeded(seed);
    for _ in 0..count {
        generator.random32()?;
    }
    let state = generator.serialize();
    Ok(Variant::String(TjsString::from(format!(
        "{}|{}|{}",
        state.state, state.left, state.next
    ))))
}

/// The serialised state's member kinds and values.
fn serialized_shape(seed: i64) -> Result<Variant, RandomError> {
    let state = RandomGenerator::seeded(seed).serialize();
    Ok(Variant::String(TjsString::from(format!(
        "Object:String:{}:Integer:{}:Integer:{}",
        state.state.as_utf16().len(),
        state.left,
        state.next
    ))))
}

/// The two answers after a state is serialised and restored.
fn restored(seed: i64, count: usize) -> Result<Variant, RandomError> {
    let mut generator = RandomGenerator::seeded(seed);
    for _ in 0..count {
        generator.random32()?;
    }
    let state = generator.serialize();
    generator.randomize(&[state_object(&state)], Seed::Time(0))?;
    let first = generator.random32()?;
    let second = generator.random32()?;
    Ok(Variant::String(TjsString::from(format!(
        "{first},{second}"
    ))))
}

/// The two answers after a state is restored through the constructor.
fn restored_from_new(seed: i64, count: usize) -> Result<Variant, RandomError> {
    let mut generator = RandomGenerator::seeded(seed);
    for _ in 0..count {
        generator.random32()?;
    }
    let state = generator.serialize();
    let mut restored = RandomGenerator::seeded(0);
    restored.randomize(&[state_object(&state)], Seed::Time(0))?;
    let first = restored.random32()?;
    let second = restored.random32()?;
    Ok(Variant::String(TjsString::from(format!(
        "{first},{second}"
    ))))
}

/// The next real after a real was drawn and the state restored.
fn restored_random(seed: i64, count: usize) -> Result<Variant, RandomError> {
    let mut generator = RandomGenerator::seeded(seed);
    for _ in 0..count {
        generator.random()?;
    }
    let state = generator.serialize();
    generator.randomize(&[state_object(&state)], Seed::Time(0))?;
    Ok(Variant::Real(generator.random()?))
}

/// A state whose text is upper-cased.
fn state_uppercase(seed: i64) -> Result<Variant, RandomError> {
    let mut generator = RandomGenerator::seeded(seed);
    let state = generator.serialize();
    let upper = TjsString::from(state.state.to_string().to_uppercase());
    let state = SerializedState {
        state: upper,
        ..state
    };
    generator.randomize(&[state_object(&state)], Seed::Time(0))?;
    Ok(Variant::Integer(i64::from(generator.random32()?)))
}

/// A state with the left count and next index replaced.
fn mutated(seed: i64, left: Variant, next: Variant, count: usize) -> Result<Variant, RandomError> {
    let mut generator = RandomGenerator::seeded(seed);
    let state = generator.serialize();
    let object = state_dictionary(&state, Some((&left, &next)), &[]);
    generator.randomize(&[object], Seed::Time(0))?;
    let first = generator.random32()?;
    if count == 2 {
        let second = generator.random32()?;
        return Ok(Variant::String(TjsString::from(format!(
            "{first},{second}"
        ))));
    }
    Ok(Variant::Integer(i64::from(first)))
}

/// A constructor argument that is not a state.
fn constructed(argument: Variant) -> Result<Variant, RandomError> {
    let mut generator = RandomGenerator::new(&[argument], Seed::Time(0))?;
    Ok(Variant::Integer(i64::from(generator.random32()?)))
}

/// A `randomize` argument that is not a state.
fn randomize_argument(argument: Variant) -> Result<Variant, RandomError> {
    constructed(argument)
}

/// A member call with two extra arguments.
fn extra(member: RandomGeneratorMember) -> Result<Variant, RandomError> {
    let mut generator = RandomGenerator::seeded(1);
    let arguments = [Variant::Integer(9), Variant::Integer(8)];
    member.call(&mut generator, &arguments, Seed::Time(0))
}

/// A state whose text is too short.
fn short_state(seed: i64) -> Result<Variant, RandomError> {
    let mut generator = RandomGenerator::seeded(seed);
    let state = generator.serialize();
    let short = SerializedState {
        state: TjsString::from("00"),
        ..state
    };
    generator.randomize(&[state_object(&short)], Seed::Time(0))?;
    Ok(Variant::Integer(i64::from(generator.random32()?)))
}

/// A state whose text holds a non-hexadecimal digit.
fn nonhex_state(seed: i64) -> Result<Variant, RandomError> {
    let mut generator = RandomGenerator::seeded(seed);
    let state = generator.serialize();
    let mut units = state.state.as_utf16().to_vec();
    units[0] = u16::from(b'z');
    let broken = SerializedState {
        state: TjsString::from(units),
        ..state
    };
    generator.randomize(&[state_object(&broken)], Seed::Time(0))?;
    Ok(Variant::Integer(i64::from(generator.random32()?)))
}

/// A state whose `state` member is an integer.
fn integer_state(seed: i64) -> Result<Variant, RandomError> {
    let mut generator = RandomGenerator::seeded(seed);
    let state = generator.serialize();
    let object = state_dictionary(&state, None, &[("state", Variant::Integer(123))]);
    generator.randomize(&[object], Seed::Time(0))?;
    Ok(Variant::Integer(i64::from(generator.random32()?)))
}

/// A state whose `state` member is an octet.
fn octet_state(seed: i64) -> Result<Variant, RandomError> {
    let mut generator = RandomGenerator::seeded(seed);
    let state = generator.serialize();
    let object = state_dictionary(&state, None, &[("state", Variant::from(octet()))]);
    generator.randomize(&[object], Seed::Time(0))?;
    Ok(Variant::Integer(i64::from(generator.random32()?)))
}

/// A state whose `state` member is the empty value.
fn void_state(seed: i64) -> Result<Variant, RandomError> {
    let mut generator = RandomGenerator::seeded(seed);
    let state = generator.serialize();
    let object = state_dictionary(&state, None, &[("state", Variant::Void)]);
    generator.randomize(&[object], Seed::Time(0))?;
    Ok(Variant::Integer(i64::from(generator.random32()?)))
}

/// A state with one member missing.
fn missing_state(seed: i64, which: usize) -> Result<Variant, RandomError> {
    let mut generator = RandomGenerator::seeded(seed);
    let state = generator.serialize();
    let name = match which {
        0 => "state",
        1 => "left",
        _ => "next",
    };
    let object = state_dictionary(&state, None, &[]);
    if let Variant::Object(closure) = &object
        && let Some(dictionary) = &closure.object
    {
        dictionary
            .delete_member(&TjsString::from(name), MemberFlags::NONE, None)
            .expect("the state dictionary has the member");
    }
    generator.randomize(&[object], Seed::Time(0))?;
    Ok(Variant::Integer(i64::from(generator.random32()?)))
}

/// A dictionary object value.
fn dictionary() -> Variant {
    let dictionary = Rc::new(Dictionary::new());
    Variant::Object(Closure {
        object: Some(dictionary.clone()),
        objthis: Some(dictionary),
    })
}

/// A state dictionary, with the three members the generator reads.
fn state_dictionary(
    state: &SerializedState,
    replace: Option<(&Variant, &Variant)>,
    extra: &[(&str, Variant)],
) -> Variant {
    let dictionary = Rc::new(Dictionary::new());
    let (left, next) = match replace {
        Some((left, next)) => (left.clone(), next.clone()),
        None => (Variant::Integer(state.left), Variant::Integer(state.next)),
    };
    let members = [
        ("state", Variant::String(state.state.clone())),
        ("left", left),
        ("next", next),
    ];
    for (name, value) in members {
        dictionary
            .prop_set(&TjsString::from(name), MemberFlags::ENSURE, &value, None)
            .expect("the state dictionary takes a member");
    }
    for (name, value) in extra {
        dictionary
            .prop_set(&TjsString::from(*name), MemberFlags::ENSURE, value, None)
            .expect("the state dictionary takes a member");
    }
    Variant::Object(Closure {
        object: Some(dictionary.clone()),
        objthis: Some(dictionary),
    })
}

/// The object value the state is.
fn state_object(state: &SerializedState) -> Variant {
    state_dictionary(state, None, &[])
}

/// An octet value, which is not a state.
fn octet() -> tjs::vm::runtime::octet::Octet {
    tjs::vm::runtime::octet::Octet::from_slice(&[0])
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
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/random_members.txt");
    let table = fs::read_to_string(&path).expect("read the random member fixture");
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
        "the random member fixture records nothing; record it with \
         `cargo run -p tjs --example random_oracle`"
    );
    records
}
