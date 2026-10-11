//! Runtime surface verifier for the compat natives/plugins.
//!
//! Unlike the source-parsing parity scripts, this test asks the **live TJS
//! VM** what surface `CompatHost::install` actually exposes. It walks the
//! global object, the class objects registered on it, and (where
//! constructible) a fresh instance of each class, then writes a deterministic
//! JSON dump to `<workspace>/target/runtime_surface.json`:
//!
//! ```json
//! { "classes": { "System": ["appDataPath", ...], "Motion.Rect": [...] } }
//! ```
//!
//! The dump is the ground truth `scripts/*_parity.py --runtime` compares the
//! parsed reference C++ surface against. It is a normal `#[test]` so the
//! verifier runs as part of `cargo test`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use compat::CompatHost;
use tjs::{DetachedValue, RetainedValue, Tjs2Engine, Tjs2ValueId, TjsValue};

mod common;
use common::vm_lock;

/// Global members that are bookkeeping, not script surface.
const IGNORED_GLOBALS: &[&str] = &["global"];

/// How deep to walk nested namespaces (`Motion.Player` is depth 1).
const MAX_DEPTH: usize = 1;

/// Class-name aliases that are *not* the global spelling but are the same
/// class object; the dump records the global spelling so a reference class
/// name can be matched by the parity scripts.
fn dump_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/runtime_surface.json")
}

/// Retain the object an engine member resolves to, if any.
fn retain_member(engine: &Tjs2Engine, obj: Tjs2ValueId, name: &str) -> Option<DetachedValue> {
    match engine.member(obj, name) {
        Ok(TjsValue::Object) => engine.retain_value_detached(&TjsValue::Object).ok(),
        _ => None,
    }
}

/// Construct `new <path>()` and retain the result when the class is
/// constructible; any constructor failure (missing args, host facts) is not an
/// error for the dump.
fn construct(engine: &Tjs2Engine, path: &str) -> Option<DetachedValue> {
    match engine.eval_retained(&format!("new {path}()"), "runtime_surface") {
        Ok(RetainedValue::Object(instance)) => Some(instance),
        _ => None,
    }
}

/// Whether the object-valued member at `path` is a nested class.
///
/// A class object constructs with no arguments, or — when its constructor
/// requires arguments — it declares a function member (the class-name
/// constructor the ABI installs). The constructor member is not necessarily
/// named after the path segment (`GdiPlus.Font` is backed by the internal
/// `FontInfo`), so the member check does not depend on the segment. An
/// object that carries class-instance names is an instance, not a class
/// (`Debug.console`), and a data dictionary declares no function member
/// (`imageTagLayerType.add`).
fn is_nested_class(engine: &Tjs2Engine, child: &DetachedValue, path: &str) -> bool {
    if construct(engine, path).is_some() {
        return true;
    }
    if has_class_names(engine, child) {
        return false;
    }
    has_function_member(engine, child)
}

/// Whether the object carries class-instance names (it is an instance of a
/// native or script class, not a class object).
fn has_class_names(engine: &Tjs2Engine, child: &DetachedValue) -> bool {
    let Ok(names) = engine.class_names(child.raw_id()) else {
        return false;
    };
    matches!(
        engine.member(names.raw_id(), "length"),
        Ok(TjsValue::Integer(count)) if count > 0
    )
}

/// Whether the object declares at least one function-valued member, the
/// class-name constructor every class object carries.
fn has_function_member(engine: &Tjs2Engine, child: &DetachedValue) -> bool {
    let Ok(members) = engine.enum_members(child.raw_id()) else {
        return false;
    };
    members
        .iter()
        .any(|member| matches!(engine.member(child.raw_id(), member), Ok(TjsValue::Object)))
}

/// Record `key` -> members of the class object, plus the members of a
/// constructed instance when the class is constructible. Returns whether an
/// entry was written: an object with no members and no zero-argument
/// constructor is a function/namespace, not a class, and is skipped.
fn record_class(
    engine: &Tjs2Engine,
    class: &DetachedValue,
    key: &str,
    classes: &mut BTreeMap<String, BTreeSet<String>>,
) -> bool {
    let mut members: BTreeSet<String> = engine
        .enum_members(class.raw_id())
        .unwrap_or_default()
        .into_iter()
        .collect();
    let constructible = if let Some(instance) = construct(engine, key) {
        for member in engine.enum_members(instance.raw_id()).unwrap_or_default() {
            members.insert(member);
        }
        true
    } else {
        false
    };
    if members.is_empty() && !constructible {
        return false;
    }
    classes.insert(key.to_string(), members);
    true
}

/// Walk the object `class`, recording each object-valued member as a dotted
/// class path up to [`MAX_DEPTH`].
fn walk_classes(
    engine: &Tjs2Engine,
    class: &DetachedValue,
    prefix: &str,
    depth: usize,
    classes: &mut BTreeMap<String, BTreeSet<String>>,
) {
    let Ok(members) = engine.enum_members(class.raw_id()) else {
        return;
    };
    for name in members {
        if IGNORED_GLOBALS.contains(&name.as_str()) {
            continue;
        }
        let Some(child) = retain_member(engine, class.raw_id(), &name) else {
            continue;
        };
        let path = format!("{prefix}{name}");
        // A nested namespace member is a class only if it constructs or
        // declares a class-name constructor member; see `is_nested_class`.
        if depth < MAX_DEPTH && is_nested_class(engine, &child, &path) {
            record_class(engine, &child, &path, classes);
            walk_classes(engine, &child, &format!("{path}."), depth + 1, classes);
        }
    }
}

/// Serialize the class map as deterministic JSON (keys and members sorted).
fn to_json(classes: &BTreeMap<String, BTreeSet<String>>) -> String {
    fn escape(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => {
                    let _ = write!(out, "\\u{:04x}", c as u32);
                }
                c => out.push(c),
            }
        }
        out
    }

    let mut json = String::from("{\n  \"classes\": {\n");
    for (index, (class, members)) in classes.iter().enumerate() {
        let _ = write!(json, "    \"{}\": [", escape(class));
        for (member_index, member) in members.iter().enumerate() {
            if member_index > 0 {
                json.push_str(", ");
            }
            let _ = write!(json, "\"{}\"", escape(member));
        }
        json.push(']');
        if index + 1 < classes.len() {
            json.push(',');
        }
        json.push('\n');
    }
    json.push_str("  }\n}\n");
    json
}

#[test]
fn runtime_surface_dump() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().expect("create the TJS2 engine");
    let mut host = CompatHost::new();
    host.install(&engine).expect("install the compat surface");

    // Plugin classes register on `Plugins.link` (doc 08 §4.1); link every
    // ported plugin so the dump covers the full surface.
    for name in compat::builtin_plugin_names() {
        engine
            .exec_script(&format!("Plugins.link('{name}');"), "runtime_surface")
            .expect("link a built-in plugin");
    }

    let global = match engine
        .eval_retained("global", "runtime_surface")
        .expect("evaluate the global object")
    {
        RetainedValue::Object(global) => global,
        RetainedValue::Value(other) => panic!("global is not an object: {other:?}"),
    };

    let mut classes: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let global_members = engine
        .enum_members(global.raw_id())
        .expect("enumerate the global object");
    for name in global_members {
        if IGNORED_GLOBALS.contains(&name.as_str()) {
            continue;
        }
        let Some(class) = retain_member(&engine, global.raw_id(), &name) else {
            continue;
        };
        // Never let a broken constructor abort the dump.
        let _ = record_class(&engine, &class, &name, &mut classes);
        walk_classes(&engine, &class, &format!("{name}."), 0, &mut classes);
    }

    assert!(
        classes.contains_key("System"),
        "the dump must contain core classes: {:?}",
        classes.keys().collect::<Vec<_>>()
    );
    assert!(
        classes.contains_key("Motion.Rect"),
        "the dump must contain Motion.* dotted classes: {:?}",
        classes.keys().collect::<Vec<_>>()
    );

    let path = dump_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create the target directory");
    }
    fs::write(&path, to_json(&classes)).expect("write runtime_surface.json");

    // The dump is only useful if it is non-trivial.
    let total: usize = classes.values().map(BTreeSet::len).sum();
    assert!(
        classes.len() > 20,
        "too few runtime classes: {}",
        classes.len()
    );
    assert!(total > 500, "too few runtime members: {total}");
}
