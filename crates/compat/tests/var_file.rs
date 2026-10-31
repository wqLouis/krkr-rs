//! Integration tests for the `var://` storage media.
//!
//! Reference: `reference/cpp/plugins/varfile.cpp`. The media's "files" are
//! the members of the script object graph: the domain is `.` — the script
//! global object — and the path names the members below it
//! (`reference/cpp/plugins/varfile.cpp:291-346`). A member that holds an
//! `Octet` is a file and a member that holds an object is a directory
//! (`reference/cpp/plugins/varfile.cpp:10-15`).
//!
//! The title's own shape is the one under test: the game keeps its packed
//! images in a global dictionary and reaches a layer's pixels through
//! `var://./<dictionary>/<container>/root/<layer id>.tlg`
//! (`reference/cpp/plugins/psbfile/main.cpp:58-68` registers the container's
//! root resources).

mod common;

use common::{Env, vm_lock};
use tjs::{RetainedValue, TjsValue};

/// Install an octet member on the global object.
///
/// The octet is the shape a resource member has
/// (`reference/cpp/plugins/psbfile/PSBValue.cpp:201-204`); the safe value
/// surface has no octet literal, so the fixture installs one directly.
fn install_octet(env: &Env, name: &str, bytes: &[u8]) {
    let engine = env.engine();
    let global = match engine
        .eval_retained("global", "var-file-test")
        .expect("the global object must resolve")
    {
        RetainedValue::Object(object) => object,
        RetainedValue::Value(other) => panic!("expected an object, got {other:?}"),
    };
    engine
        .set_member_octet(global.raw_id(), name, bytes)
        .expect("the octet member must be installed");
}

/// The string an expression answers with.
fn string(env: &Env, expression: &str) -> String {
    match env.eval_ok(expression) {
        TjsValue::String(value) => value,
        other => panic!("{expression:?} returned {other:?}, expected a string"),
    }
}

/// The title's shape: a packed image in a global dictionary, reached through
/// the media.
///
/// The container's `root` is a property, so the media has to run its getter
/// to find the member below it
/// (`reference/cpp/plugins/varfile.cpp:291-346` walks the members with
/// `PropGet`).
#[test]
fn a_var_member_is_read_through_the_storage() {
    let _lock = vm_lock();
    let env = Env::with_storage();
    env.link_plugin("varfile.dll");
    install_octet(&env, "varFileBytes", b"hello var\n");
    env.run(
        r#"
class PackImageInfo {
    property root {
        getter() { return %["3797.tlg" => varFileBytes]; }
    }
}
var packImageMap = %[];
packImageMap["ev121a.pimg"] = new PackImageInfo();
"#,
    );

    env.run("var lines = []; lines.load('var://./packImageMap/ev121a.pimg/root/3797.tlg');");

    assert_eq!(
        string(&env, "lines[0]"),
        "hello var",
        "the octet behind the property must be read through the media"
    );
}

/// A file is an octet and a directory is an object
/// (`reference/cpp/plugins/varfile.cpp:10-15`).
#[test]
fn only_an_octet_member_is_a_file() {
    let _lock = vm_lock();
    let env = Env::with_storage();
    env.link_plugin("varfile.dll");
    install_octet(&env, "varFileBytes", b"bytes\n");
    env.run(
        r#"
var store = %[];
store["data.bin"] = varFileBytes;
store["text"] = "not a file";
store["nested"] = %[];
"#,
    );

    assert_eq!(
        env.eval_int("Storages.isExistentStorage('var://./store/data.bin')"),
        1,
        "an octet member is a file"
    );
    assert_eq!(
        env.eval_int("Storages.isExistentStorage('var://./store/text')"),
        0,
        "a string member is not a file"
    );
    assert_eq!(
        env.eval_int("Storages.isExistentStorage('var://./store/nested')"),
        0,
        "a directory is not a file"
    );
    assert_eq!(
        env.eval_int("Storages.isExistentStorage('var://./store/missing')"),
        0,
        "an absent member is not a file"
    );
}

/// A domain that is not the global object is refused
/// (`reference/cpp/plugins/varfile.cpp:298`).
#[test]
fn a_var_domain_that_is_not_the_global_one_raises() {
    let _lock = vm_lock();
    let env = Env::with_storage();
    env.link_plugin("varfile.dll");

    let message = env.error("Storages.isExistentStorage('var://foo/store/data.bin')");
    assert!(message.contains("no such domain:foo"), "{message}");
}

/// A path without a member separator is refused
/// (`reference/cpp/plugins/varfile.cpp:301`).
#[test]
fn a_var_path_without_a_member_separator_raises() {
    let _lock = vm_lock();
    let env = Env::with_storage();
    env.link_plugin("varfile.dll");

    let message = env.error("Storages.isExistentStorage('var://foo')");
    assert!(message.contains("invalid path:foo"), "{message}");
}

/// A member that is not a file cannot be opened
/// (`reference/cpp/plugins/varfile.cpp:257`).
#[test]
fn a_missing_var_member_raises_when_it_is_read() {
    let _lock = vm_lock();
    let env = Env::with_storage();
    env.link_plugin("varfile.dll");
    env.run("var store = %[];");

    let error = env
        .engine()
        .exec_script(
            "var lines = []; lines.load('var://./store/missing.bin');",
            "var-file-test",
        )
        .expect_err("a missing member must raise");
    let message = error.to_string();
    assert!(message.contains("cannot open memfile:"), "{message}");
}

/// A scheme that no media registered is not a storage the namespace can
/// place (`reference/cpp/core/base/StorageIntf.cpp:201`,
/// `reference/cpp/core/base/MsgIntfInc.h:29`).
#[test]
fn an_unknown_media_scheme_raises() {
    let _lock = vm_lock();
    let env = Env::with_storage();
    env.link_plugin("varfile.dll");

    let message = env.error("Storages.isExistentStorage('bogus://store/data.bin')");
    assert!(
        message.contains("Not supported media type \"bogus\""),
        "{message}"
    );
}

/// An empty octet is an empty file: the media serves it, and the read yields
/// no line instead of raising.
#[test]
fn an_empty_var_member_is_an_empty_file() {
    let _lock = vm_lock();
    let env = Env::with_storage();
    env.link_plugin("varfile.dll");
    install_octet(&env, "varFileBytes", b"");
    env.run("var store = %[]; store[\"empty.bin\"] = varFileBytes;");

    env.run("var lines = []; lines.load('var://./store/empty.bin');");

    assert_eq!(env.eval_int("lines.length"), 0, "an empty file has no line");
}
