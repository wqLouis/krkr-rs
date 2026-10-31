//! `menu.dll` surface parity gate.
//!
//! The shipped DLL is the oracle for this plugin (there is no reference
//! source): per the project rule for a binary-only plugin, a behaviour
//! assertion cites the DLL by path and content hash and the test gates on a
//! committed fixture derived from it rather than loading the game binary.
//! The fixture is `tests/fixtures/menu_dll_surface.txt`; its header names the
//! DLL path, the SHA-256 of both shipped builds and the decompiled
//! registration function, and its body lists the recovered members.
//!
//! The DLL registers the **core** `MenuItem` class (the class is registered
//! unconditionally by the reference engine itself,
//! `reference/cpp/core/base/ScriptMgnIntf.cpp:542`), so this port already
//! implements the members in `crates/compat/src/natives/menu_item.rs`. The
//! gate here is what makes that claim checkable: it asks the live VM what
//! `MenuItem` actually exposes and compares the set with the fixture,
//! member-for-member.
//!
//! `windowEx.dll` is a separate ported plugin that attaches members to the
//! same core class (`reference/cpp/plugins/windowEx.cpp:1219-1239`); those
//! are excluded through the explicit, cited [`WINDOW_EX_MEMBERS`] allowance
//! so the comparison stays exact instead of silently drifting.
//!
//! The DLL also attaches a read-only `menu` property to `Window`. That is a
//! **core** member (the reference engine attaches it in
//! `reference/cpp/core/base/ScriptMgnIntf.cpp:538-542`), implemented on the
//! core `Window` class and tested in `tests/window.rs`; it is not part of the
//! `MenuItem` surface this gate covers.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use common::vm_lock;
use compat::CompatHost;
use tjs::{RetainedValue, Tjs2Engine, TjsValue};

/// Members `windowEx.dll` attaches to the core `MenuItem`, cited from
/// `reference/cpp/plugins/windowEx.cpp:1219-1239`: ten `bi*` class constants
/// (lines 1220-1229), four bitmap/justify instance properties (lines
/// 1231-1238) and the `popupEx` function (line 1239). They belong to the
/// `windowEx` port, not to `menu.dll`.
const WINDOW_EX_MEMBERS: &[&str] = &[
    "biSystem",
    "biRestore",
    "biMinimize",
    "biClose",
    "biCloseDisabled",
    "biMinimizeDisabled",
    "biPopupClose",
    "biPopupRestore",
    "biPopupMaximize",
    "biPopupMinimize",
    "rightJustify",
    "bmpItem",
    "bmpChecked",
    "bmpUnchecked",
    "popupEx",
];

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/menu_dll_surface.txt")
}

/// Parse `<kind> <name>` member lines from the fixture; `class` and comment
/// lines are metadata.
fn fixture_members() -> BTreeSet<String> {
    let text = fs::read_to_string(fixture_path()).expect("read the menu.dll surface fixture");
    let mut members = BTreeSet::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("class ") {
            continue;
        }
        let Some((kind, name)) = line.split_once(char::is_whitespace) else {
            panic!("fixture line is not `<kind> <name>`: {line:?}");
        };
        match kind {
            "method" | "ctor" | "property" => {
                assert!(
                    members.insert(name.trim().to_string()),
                    "duplicate fixture member {name:?}"
                );
            }
            other => panic!("unknown fixture kind {other:?} on line {line:?}"),
        }
    }
    members
}

/// The live `MenuItem` surface: the class object's own members unioned with
/// the members of a fresh instance, the same walk
/// `tests/runtime_surface.rs` uses.
fn live_menu_item_members(engine: &Tjs2Engine) -> BTreeSet<String> {
    let class = match engine
        .eval_retained("MenuItem", "menu_dll_surface")
        .expect("evaluate the MenuItem class")
    {
        RetainedValue::Object(class) => class,
        RetainedValue::Value(other) => panic!("MenuItem is not an object: {other:?}"),
    };
    let mut members: BTreeSet<String> = engine
        .enum_members(class.raw_id())
        .expect("enumerate the MenuItem class")
        .into_iter()
        .collect();
    match engine.eval_retained("new MenuItem()", "menu_dll_surface") {
        Ok(RetainedValue::Object(instance)) => {
            members.extend(
                engine
                    .enum_members(instance.raw_id())
                    .expect("enumerate a MenuItem instance"),
            );
        }
        Ok(RetainedValue::Value(other)) => panic!("new MenuItem() returned {other:?}"),
        Err(error) => panic!("new MenuItem() failed: {error}"),
    }
    members
}

/// The parity gate: every fixture member is registered, and the only
/// registered members outside the fixture are `windowEx`'s.
#[test]
fn menu_item_surface_matches_the_dll_fixture() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().expect("create the TJS2 engine");
    let mut host = CompatHost::new();
    host.install(&engine).expect("install the compat surface");
    // Link every ported plugin so the live surface is complete, exactly like
    // the runtime-surface dump.
    for name in compat::builtin_plugin_names() {
        engine
            .exec_script(&format!("Plugins.link('{name}');"), "menu_dll_surface")
            .expect("link a built-in plugin");
    }

    let fixture = fixture_members();
    let live = live_menu_item_members(&engine);
    let window_ex: BTreeSet<String> = WINDOW_EX_MEMBERS.iter().map(|m| m.to_string()).collect();

    let missing: Vec<&String> = fixture.difference(&live).collect();
    assert!(
        missing.is_empty(),
        "menu.dll registers members the port does not: {missing:?}"
    );

    let unexpected: Vec<&String> = live
        .difference(&fixture)
        .filter(|member| !window_ex.contains(*member))
        .collect();
    assert!(
        unexpected.is_empty(),
        "the port registers MenuItem members neither menu.dll nor windowEx.dll does: {unexpected:?}"
    );

    // The allowance itself must not go stale: windowEx's members are all
    // really registered, and each is outside the menu.dll fixture.
    let stray: Vec<&str> = WINDOW_EX_MEMBERS
        .iter()
        .copied()
        .filter(|member| !live.contains(*member))
        .collect();
    assert!(
        stray.is_empty(),
        "windowEx allowance lists members the port does not register: {stray:?}"
    );
    let overlap: Vec<&str> = WINDOW_EX_MEMBERS
        .iter()
        .copied()
        .filter(|member| fixture.contains(*member))
        .collect();
    assert!(
        overlap.is_empty(),
        "the windowEx allowance overlaps the menu.dll fixture: {overlap:?}"
    );
}

/// The fixture is not empty and covers the recovered surface exactly, so the
/// parity assertion above cannot pass vacuously.
#[test]
fn fixture_has_the_recovered_menu_item_surface() {
    let members = fixture_members();
    assert_eq!(members.len(), 23, "unexpected fixture size: {members:?}");
    // The seven methods the DLL registers (including the framework
    // `finalize`), the constructor and the fifteen properties.
    for method in [
        "finalize",
        "add",
        "insert",
        "remove",
        "popup",
        "onClick",
        "fireClick",
    ] {
        assert!(
            members.contains(method),
            "fixture is missing method {method}"
        );
    }
    assert!(
        members.contains("MenuItem"),
        "fixture is missing the constructor"
    );
    for property in [
        "caption",
        "checked",
        "enabled",
        "group",
        "radio",
        "shortcut",
        "visible",
        "parent",
        "children",
        "root",
        "window",
        "index",
        "HMENU",
        "textToKeycode",
        "keycodeToText",
    ] {
        assert!(
            members.contains(property),
            "fixture is missing property {property}"
        );
    }
}

/// `menu.dll` must resolve to the built-in table: the link registers the
/// built-in's own surface, and the core `MenuItem` class stays available.
#[test]
fn menu_dll_links_as_a_builtin() {
    let _lock = vm_lock();
    let engine = Box::new(Tjs2Engine::new().expect("create engine"));
    let mut host = CompatHost::new();
    host.install(&engine).expect("install the compat surface");

    engine
        .exec_script("Plugins.link('menu.dll');", "menu_dll_surface")
        .expect("link menu.dll");
    assert_eq!(
        host.registry().linked(),
        vec!["menu.dll".to_string()],
        "the link must be recorded"
    );
    // The plugin's own surface is on the core class; nothing is published
    // under the plugin's stem.
    assert_eq!(
        engine
            .eval("typeof global['menu']", "menu_dll_surface")
            .expect("probe the stem"),
        TjsValue::String("undefined".into()),
        "a built-in link must not publish a global under the stem"
    );
    // The core class is present and constructible through the link.
    assert!(
        matches!(
            engine.eval_retained("new MenuItem()", "menu_dll_surface"),
            Ok(RetainedValue::Object(_))
        ),
        "the core MenuItem class must survive the built-in link"
    );
}
