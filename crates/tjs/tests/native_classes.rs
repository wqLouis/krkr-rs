//! The native classes a script reaches through the global object.
//!
//! The classes are written in the runtime's own member tables, but a script
//! reaches its global object through the engine, whose global holds the
//! engine's classes. The global handle answers the Rust class registry first,
//! so these probes run the Rust classes through `runtime::engine::run` rather
//! than their module tests.

use std::rc::Rc;

use tjs::vm::runtime::engine::run;
use tjs::{Tjs2Engine, TjsValue};

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
/// The integer the engine's own class answers, for a comparison.
fn integer(engine: &Rc<Tjs2Engine>, expression: &str, what: &str) -> i64 {
    match engine.eval(expression, "native_classes") {
        Ok(TjsValue::Integer(value)) => value,
        Ok(other) => panic!("{what} is not an integer: {other:?}"),
        Err(error) => panic!("{what}: {error}"),
    }
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

    // reference/krkr2/cpp/core/tjs2/tjsDate.cpp:28-32 — a text is read by the
    // date grammar, and tjsDate.cpp:62 is the constructor's text branch.
    // reference/krkr2/cpp/core/tjs2/tjsDate.cpp:300-309 — getTime answers the
    // instant in milliseconds.
    assert_eq!(
        answer(
            &engine,
            "var d = new Date(\"1 Jan 1970 09:00:00 JST\");\n\
             return d.getTime();"
        ),
        "0"
    );
    // reference/krkr2/cpp/core/tjs2/tjsDate.cpp:53-93 — a numeric
    // construction names the year, the month, the day and the time of day.
    // reference/krkr2/cpp/core/tjs2/tjsDate.cpp:222-332 — the accessors read
    // the fields the construction placed.
    assert_eq!(
        answer(
            &engine,
            "var d = new Date(2020, 5, 15, 12, 30, 45);\n\
             return d.getYear() + \"-\" + d.getMonth() + \"-\" + d.getDate();"
        ),
        "2020-5-15"
    );
    // reference/krkr2/cpp/core/tjs2/tjsDate.cpp:53-93 — the numeric
    // construction places the same instant the engine's own class does.
    // reference/krkr2/cpp/core/tjs2/tjsDate.cpp:300-309 — getTime reads it.
    let built = integer(
        &engine,
        "(new Date(2020, 5, 15, 12, 30, 45)).getTime()",
        "the built instant",
    );
    assert_eq!(
        answer(
            &engine,
            "var d = new Date(2020, 5, 15, 12, 30, 45);\n\
             return d.getTime();"
        ),
        built.to_string()
    );
    // reference/krkr2/cpp/core/tjs2/tjsDate.cpp:334-345 — parse replaces the
    // instance's instant from a text.
    assert_eq!(
        answer(
            &engine,
            "var d = new Date(0);\n\
             d.parse(\"1 Jan 1970 09:00:00 JST\");\n\
             return d.getTime();"
        ),
        "0"
    );
    // reference/krkr2/cpp/core/tjs2/tjsDate.cpp:95-112 — setYear replaces the
    // year at the instance's zone.
    // reference/krkr2/cpp/core/tjs2/tjsDate.cpp:321-332 — getYear reads it.
    assert_eq!(
        answer(
            &engine,
            "var d = new Date(\"1 Jan 2004 00:00:00 JST\");\n\
             d.setYear(1999);\n\
             return d.getYear();"
        ),
        "1999"
    );
    // reference/krkr2/cpp/core/tjs2/tjsDate.cpp:311-319 — getTimezoneOffset
    // answers the zone's standard offset, from the class and from an
    // instance. The engine's own class answers the same, so the comparison
    // holds at whatever zone the host runs in.
    let zone = integer(
        &engine,
        "(new Date(0)).getTimezoneOffset()",
        "the zone offset",
    );
    assert_eq!(
        answer(
            &engine,
            "var d = new Date(0);\nreturn d.getTimezoneOffset();"
        ),
        zone.to_string()
    );
    assert_eq!(
        answer(&engine, "return Date.getTimezoneOffset();"),
        zone.to_string()
    );
    // reference/krkr2/cpp/core/tjs2/tjsDate.cpp:46-345 — the class publishes
    // no `now`, no `UTC` and no text conversion member; a date is an object.
    assert_eq!(kind(&engine, "new Date(0)"), "Object");
    assert_eq!(kind(&engine, "Date.now"), "undefined");
    assert_eq!(kind(&engine, "Date.UTC"), "undefined");
    assert_eq!(kind(&engine, "(new Date(0)).toString"), "undefined");
}
