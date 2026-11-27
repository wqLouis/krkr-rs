//! The Rust `Dictionary` class object and the engine's class of the same name.
//!
//! The Rust registry publishes the class object and its seven builtin statics.
//! A member the plugin attaches to the engine's `Dictionary` class must remain
//! reachable through the Rust class object, so a class object falls back to the
//! engine's class for a name it does not publish.
//!
//! The engine's answer is recorded first, then the same call runs through
//! `Scripts.eval`, which runs on the Rust executor, where `Dictionary` resolves
//! to the Rust class object. The `incontextof` receiver is an engine
//! dictionary, so the plugin's serializer sees a real dictionary either way.

mod common;

use std::path::Path;

use common::{link_all_builtins, vm_lock};
use tjs::TjsValue;

/// The text an answer holds.
fn text(value: TjsValue) -> String {
    match value {
        TjsValue::String(value) => value,
        other => panic!("the answer is not a text: {other:?}"),
    }
}

/// The UTF-16LE text the plugin's file writers produce.
fn read_struct(path: &Path) -> String {
    let bytes = std::fs::read(path).expect("read the structured file");
    assert_eq!(bytes.len() % 2, 0, "an odd UTF-16LE byte count");
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16(&units).expect("valid UTF-16LE")
}

#[test]
fn the_rust_dictionary_class_reaches_the_plugin_statics() {
    let _lock = vm_lock();
    let env = common::Env::with_storage();
    link_all_builtins(env.engine());
    let dir = env.game_dir().expect("the temp game directory").clone();

    // The engine's own answers, recorded first: the class members are on the
    // engine's class object, and a dictionary receiver via `incontextof` is
    // what the plugin serializes.
    env.run(
        "var d = %[a: 1, text: \"x\"]; \
         global.engine_struct = (Dictionary.toStructString incontextof d)(); \
         (Dictionary.saveStruct2 incontextof d)('engine-dictionary.txt');",
    );
    let engine_struct = text(env.eval_ok("engine_struct"));
    let engine_saved = read_struct(&dir.join("engine-dictionary.txt"));
    assert_eq!(
        text(env.eval_ok("typeof Dictionary.toStructString")),
        "Object"
    );
    assert_eq!(text(env.eval_ok("typeof Dictionary.saveStruct2")), "Object");

    // The same lookups and calls through the Rust class object. `this` is the
    // engine dictionary, so the plugin receives a dictionary on both paths.
    assert_eq!(
        text(env.eval_ok("Scripts.eval('typeof Dictionary.toStructString')")),
        "Object"
    );
    assert_eq!(
        text(env.eval_ok("Scripts.eval('typeof Dictionary.saveStruct2')")),
        "Object"
    );
    let rust_struct =
        text(env.eval_ok("Scripts.eval('(Dictionary.toStructString incontextof d)()')"));
    assert_eq!(rust_struct, engine_struct);

    env.run("Scripts.eval(\"(Dictionary.saveStruct2 incontextof d)('rust-dictionary.txt')\");");
    assert_eq!(read_struct(&dir.join("rust-dictionary.txt")), engine_saved);
}
