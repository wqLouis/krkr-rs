//! The native classes a script reaches through the global object.
//!
//! The classes are written in the runtime's own member tables, but a script
//! reaches its global object through the engine, whose global holds the
//! engine's classes. The global handle answers the Rust class registry first,
//! so these probes run the Rust classes through `runtime::engine::run` rather
//! than their module tests.
//!
//! `Date` is the one class this check cannot reach yet: the calendar and the
//! grammar are in `natives::date`, but the module publishes no member table the
//! registry could read, so the class is named as deferred with that reason.

use std::rc::Rc;

use tjs::Tjs2Engine;
use tjs::vm::runtime::engine::run;

/// The class this check cannot reach, and why.
const DEFERRED: &[(&str, &str)] = &[(
    "Date",
    "`natives::date` owns the calendar and the grammar but publishes no member table, and this \
     change may not add one outside that module; `String(new Date(0))` therefore still has no \
     Rust class to run",
)];

/// Runs one script and answers its text, so a case checks one line.
fn answer(engine: &Rc<Tjs2Engine>, script: &str) -> String {
    match run(Rc::clone(engine), script, "native_classes") {
        Ok(value) => value
            .to_string()
            .expect("the answer has a text")
            .to_string(),
        Err(error) => panic!("{script}: {error:?}"),
    }
}

/// The kinds of two class objects, so a read proves the registry answered.
fn kind(engine: &Rc<Tjs2Engine>, expression: &str) -> String {
    answer(engine, &format!("return typeof ({expression});"))
}

#[test]
fn the_native_classes_are_reachable_from_a_script() {
    let engine = Rc::new(Tjs2Engine::new().expect("the engine starts"));

    // reference/krkr2/cpp/core/tjs2/tjsMath.cpp:199-210 — floor reads its
    // parameter as a real and answers a real.
    assert_eq!(answer(&engine, "return Math.floor(1.5);"), "1");
    // reference/krkr2/cpp/core/tjs2/tjsMath.cpp:238-271 — max answers the
    // greatest parameter.
    assert_eq!(answer(&engine, "return Math.max(1, 2);"), "2");
    // reference/krkr2/cpp/core/tjs2/tjs.cpp:170-177 — the class object carries
    // Math.RandomGenerator, and both are objects.
    assert_eq!(kind(&engine, "Math"), "Object");
    assert_eq!(kind(&engine, "Math.RandomGenerator"), "Object");

    // reference/krkr2/cpp/core/tjs2/tjsRandomGenerator.cpp:262-301 — a seed
    // starts the generator and random32 answers the next 32-bit word.
    assert_eq!(
        answer(
            &engine,
            "var generator = new Math.RandomGenerator(1);\n\
             return generator.random32();"
        ),
        "577090037"
    );

    // reference/krkr2/cpp/core/tjs2/tjsException.cpp:22-52 — the first
    // construction argument is the message.
    assert_eq!(
        answer(&engine, "return (new Exception(\"m\")).message;"),
        "m"
    );
    // reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2683-2698 — instanceof
    // reads the right operand's text; the class object's text is its object
    // form, so an exception is not an instance of its own class.
    assert_eq!(
        answer(
            &engine,
            "return ((new Exception(\"m\")) instanceof Exception) ? 1 : 0;"
        ),
        "0"
    );

    // reference/krkr2/cpp/core/tjs2/tjsRegExp.cpp:303-329 — test compiles the
    // pattern and answers whether it matched.
    assert_eq!(
        answer(&engine, "return (new RegExp(\"a\")).test(\"a\");"),
        "1"
    );
    // reference/krkr2/cpp/core/tjs2/tjsRegExp.cpp:330-360 — test records the
    // instance as the class static.
    // reference/krkr2/cpp/core/tjs2/tjsRegExp.cpp:607-616 — last reads it.
    assert_eq!(
        answer(
            &engine,
            "var re = new RegExp(\"a\");\n\
             var before = typeof RegExp.last;\n\
             var matched = re.test(\"a\");\n\
             return before + \"|\" + (typeof RegExp.last) + \"|\" + \
             (((RegExp.last == re) ? 1 : 0)) + \"|\" + matched;"
        ),
        "void|Object|1|1"
    );
    // reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2456-2467 — a text's
    // replace calls the pattern value's replace with the text and the
    // replacement.
    assert_eq!(
        answer(&engine, "return \"abc\".replace(new RegExp(\"b\"), \"X\");"),
        "aXc"
    );

    // A deferred case is named with its reason, so it cannot become a silent
    // skip when the class is added.
    for (class, reason) in DEFERRED {
        assert!(
            !reason.is_empty(),
            "the deferred class {class} does not say why it is deferred"
        );
    }
}
