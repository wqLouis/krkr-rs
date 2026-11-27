//! Runtime surface verifier for the compat natives/plugins.
//!
//! Unlike the source-parsing parity scripts, this test asks the **live TJS
//! VM** what surface a script resolves. It walks the global handle a script
//! reads through — the Rust member store, the deleted-name tombstones, the
//! Rust native class registry, then the engine's own global object — the class
//! objects registered on it, and (where constructible) a fresh instance of
//! each class, then writes a deterministic JSON dump to
//! `<workspace>/target/runtime_surface.json`:
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
use std::rc::Rc;

use compat::CompatHost;
use tjs::Tjs2Engine;
use tjs::vm::runtime::bridge::CHandle;
use tjs::vm::runtime::object::{Dispatch, MemberFlags};
use tjs::vm::runtime::string::TjsString;
use tjs::vm::runtime::variant::Variant;

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

/// The member names an object's surface enumerates.
fn member_names(object: &dyn Dispatch) -> Vec<TjsString> {
    let mut names = Vec::new();
    object
        .enum_members(MemberFlags::ENUM_NO_VALUE, &mut |name, _, _| {
            names.push(name.clone());
            true
        })
        .unwrap_or_default();
    names
}

/// The object a member read resolves to, if any.
fn member_object(object: &dyn Dispatch, name: &TjsString) -> Option<Rc<dyn Dispatch>> {
    match object.prop_get(name, MemberFlags::NONE, None) {
        Ok(Variant::Object(closure)) => closure.object,
        _ => None,
    }
}

/// Construct `new <class>()` through the class object, as the executor does;
/// any constructor failure (missing args, host facts) is not an error for the
/// dump.
fn construct(class: &Rc<dyn Dispatch>) -> Option<Rc<dyn Dispatch>> {
    match class.create_new(&TjsString::new(), Some(&[]), None) {
        Ok(Variant::Object(closure)) => closure.object,
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
/// engine object that carries class-instance names is an instance, not a class
/// (`Debug.console`), and a data dictionary declares no function member
/// (`imageTagLayerType.add`). A Rust class object is the one that answers
/// `Class`.
fn is_nested_class(object: &Rc<dyn Dispatch>) -> bool {
    if construct(object).is_some() {
        return true;
    }
    if object.engine_id().is_some() {
        return object.class_names().is_empty() && has_function_member(object);
    }
    object
        .is_instance_of(&TjsString::from("Class"), None)
        .unwrap_or(false)
}

/// Whether the object declares at least one function-valued member, the
/// class-name constructor every class object carries.
fn has_function_member(object: &Rc<dyn Dispatch>) -> bool {
    member_names(object.as_ref())
        .iter()
        .any(|member| member_object(object.as_ref(), member).is_some())
}

/// Record `key` -> members of the class object, plus the members of a
/// constructed instance when the class is constructible. Returns whether an
/// entry was written: an object with no members and no zero-argument
/// constructor is a function/namespace, not a class, and is skipped.
fn record_class(
    class: &Rc<dyn Dispatch>,
    key: &str,
    classes: &mut BTreeMap<String, BTreeSet<String>>,
) -> bool {
    let mut members: BTreeSet<String> = member_names(class.as_ref())
        .into_iter()
        .map(|name| name.to_string())
        .collect();
    let constructible = if let Some(instance) = construct(class) {
        for member in member_names(instance.as_ref()) {
            members.insert(member.to_string());
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
    class: &Rc<dyn Dispatch>,
    prefix: &str,
    depth: usize,
    classes: &mut BTreeMap<String, BTreeSet<String>>,
) {
    for name in member_names(class.as_ref()) {
        let name_text = name.to_string();
        if IGNORED_GLOBALS.contains(&name_text.as_str()) {
            continue;
        }
        let Some(child) = member_object(class.as_ref(), &name) else {
            continue;
        };
        let path = format!("{prefix}{name_text}");
        // A nested namespace member is a class only if it constructs or
        // declares a class-name constructor member; see `is_nested_class`.
        if depth < MAX_DEPTH && is_nested_class(&child) {
            record_class(&child, &path, classes);
            walk_classes(&child, &format!("{path}."), depth + 1, classes);
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

    // The global handle a script reads through: its store, its tombstones,
    // the Rust native class registry, then the engine's global object.
    let globals = host.services().script_globals();
    let global_id = engine
        .global_object()
        .expect("retain the engine's global object");
    let global: Rc<dyn Dispatch> = Rc::new(
        CHandle::with_members(Rc::new(engine.share()), global_id, globals)
            .expect("wrap the global object"),
    );

    let mut classes: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for name in member_names(global.as_ref()) {
        let name_text = name.to_string();
        if IGNORED_GLOBALS.contains(&name_text.as_str()) {
            continue;
        }
        let Some(class) = member_object(global.as_ref(), &name) else {
            continue;
        };
        // Never let a broken constructor abort the dump.
        let _ = record_class(&class, &name_text, &mut classes);
        walk_classes(&class, &format!("{name_text}."), 0, &mut classes);
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
