//! `Motion.ResourceManager` — PSB module cache, layer-id allocator and the
//! shared snapshot store used by the players.
//!
//! Behavioural reference: `reference/cpp/plugins/motionplayer/ResourceManager.*`.
//! The reference keeps the cache in a `shared_ptr<State>`; this port keeps one
//! store per VM engine in the compat service state, so a `Player` and the
//! manager it was built from observe the same data without smuggling a
//! pointer through the constructor argument.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tjs::{DetachedValue, RetainedValue, TjsValue, VAL_OBJECT, Value};

use super::StoredValue;
use super::emote::ModuleSettings;
use super::helpers::{eval_object, inst_mut, set_retained};
use super::model::MotionSnapshot;
use super::psb::{PsbValue, parse_stream};
use crate::native_util::{args, report_error, set_int, set_void, value_as_string};
use crate::natives::system::{resolve, value_as_i64};
use crate::{CompatCtx, CompatError};

/// Per-engine snapshot store keyed by an opaque id.
#[derive(Default)]
pub(crate) struct MotionStore {
    next: u64,
    snapshots: BTreeMap<u64, Arc<MotionSnapshot>>,
    last: Option<u64>,
    pub(crate) decrypt_seed: i64,
    decrypt_func: Option<DetachedValue>,
    rng: u64,
    /// The `D3DEmoteModule` settings; the reference keeps every one of them
    /// as a class-level static (`reference/cpp/plugins/motionplayer/
    /// D3DEmoteModule.h:43-48`), so all instances share a single value.
    pub(crate) module: ModuleSettings,
}

impl MotionStore {
    /// Insert a snapshot and return its id.
    pub(crate) fn insert(&mut self, snapshot: MotionSnapshot) -> u64 {
        self.next += 1;
        let id = self.next;
        self.snapshots.insert(id, Arc::new(snapshot));
        self.last = Some(id);
        id
    }

    /// Look up a snapshot.
    pub(crate) fn get(&self, id: u64) -> Option<Arc<MotionSnapshot>> {
        self.snapshots.get(&id).cloned()
    }

    /// The most recently inserted snapshot.
    pub(crate) fn last(&self) -> Option<Arc<MotionSnapshot>> {
        self.last.and_then(|id| self.get(id))
    }

    /// The next pseudo-random value in `[0, 1)`.
    ///
    /// The reference `Player::random()` delegates to a
    /// `Math.RandomGenerator` instance owned by the player and converts its
    /// `random()` result to a double
    /// (`reference/cpp/plugins/motionplayer/PlayerCore.cpp:1318-1332`), so
    /// consecutive calls yield fresh reals in `[0, 1)`. This xorshift keeps
    /// the same observable contract with one advancing state per engine.
    pub(crate) fn next_random(&mut self) -> f64 {
        if self.rng == 0 {
            self.rng = 0x2545_F491_4F6C_DD1D;
        }
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        ((x >> 11) as f64) * (1.0 / ((1u64 << 53) as f64))
    }
}

/// Per-instance `ResourceManager` payload.
#[derive(Default)]
pub(crate) struct ResourceInst {
    /// Path key → the module object that `load` returned for it.
    loaded: BTreeMap<String, StoredValue>,
    last: Option<StoredValue>,
    layer_ids: BTreeMap<i64, String>,
    layer_names: BTreeMap<String, i64>,
    used: BTreeSet<i64>,
    next: i64,
}

impl ResourceInst {
    fn require_layer_id(&mut self) -> i64 {
        loop {
            self.next += 1;
            if self.used.insert(self.next) {
                return self.next;
            }
        }
    }

    fn find_key(&self, path: &str) -> Option<&StoredValue> {
        cache_keys(path).iter().find_map(|key| self.loaded.get(key))
    }
}

fn cache_keys(path: &str) -> Vec<String> {
    let mut keys = vec![path.to_string()];
    if let Some(stripped) = path.strip_prefix("./") {
        keys.push(stripped.to_string());
    }
    keys
}

extern "C" fn resource_create(_engine: *mut std::ffi::c_void) -> *mut std::ffi::c_void {
    Box::into_raw(Box::new(ResourceInst::default())) as *mut std::ffi::c_void
}

extern "C" fn resource_destroy(_engine: *mut std::ffi::c_void, instance: *mut std::ffi::c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `instance` came from `resource_create` and is released once.
    drop(unsafe { Box::from_raw(instance as *mut ResourceInst) });
}

extern "C" fn resource_ctor(
    engine: *mut std::ffi::c_void,
    _instance: *mut std::ffi::c_void,
    argc: std::os::raw::c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut std::ffi::c_void,
) -> std::os::raw::c_int {
    let Some(res) = resolve(engine) else {
        return report_error(out_error, super::helpers::NO_BINDING);
    };
    // `NCB_CONSTRUCTOR((iTJSDispatch2 *, tjs_int))` registers an arity of 2,
    // and ncbind rejects shorter calls
    // (`reference/cpp/plugins/motionplayer/main.cpp:405`,
    // `reference/cpp/core/plugin/ncbind.hpp:1178`).
    // The reference stores the cache-size hint; this port has no cache
    // eviction yet, so the arguments are accepted after the count gate.
    // SAFETY: the trampoline guarantees `argc` valid entries.
    let arguments = unsafe { args(argv, argc) };
    if arguments.len() < 2 {
        return report_error(out_error, "ResourceManager: bad parameter count");
    }
    // The reference pre-defines three keybinder dictionaries on the KAG
    // window argument when they are absent, because encrypted
    // `keybinder.tjs` dereferences them unconditionally
    // (`reference/cpp/plugins/motionplayer/ResourceManager.cpp:84-101`).
    if arguments[0].ty == VAL_OBJECT {
        let ctx = res.ctx();
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let engine_ref = unsafe { ctx.engine() };
        // SAFETY: the argument is a live object for the duration of the call.
        if let Ok(window) = engine_ref.retain_object_arg(&arguments[0]) {
            for name in [
                "ShortCutInitialPadKeyMap",
                "ShortCutInitialGamePadKeyMap",
                "_proceedingKeyList",
            ] {
                let defined = engine_ref
                    .get_member(window.raw_id(), name)
                    .is_ok_and(|value| !matches!(value, TjsValue::Void));
                if !defined {
                    let Ok(dictionary) = eval_object(&ctx, "%[]") else {
                        continue;
                    };
                    let id = dictionary.raw_id() as usize as u64;
                    let _ = engine_ref.set_member(window.raw_id(), name, &TjsValue::Retained(id));
                    drop(dictionary);
                }
            }
        }
    }
    set_void(out);
    0
}

/// Read and parse the PSB at `path`, then register it in the store.
fn load_module(ctx: &CompatCtx, inst: &mut ResourceInst, path: &str) -> Result<u64, String> {
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let seed = services.state::<MotionStore>().decrypt_seed;
    let Some(storage) = services.storage() else {
        return Err("motionplayer: no storage is attached".into());
    };
    // SAFETY: the host keeps the attached storage alive for the binding.
    let bytes = unsafe { &*storage }
        .read(path)
        .map_err(|error| error.to_string())?;
    let psb = parse_stream(&bytes, seed).map_err(|error| error.0)?;
    let snapshot = MotionSnapshot::from_psb(psb);
    let id = services.state::<MotionStore>().insert(snapshot);
    let store = services.state::<MotionStore>();
    let snapshot_id = store.get(id);

    // Build the TJS module the scripts read (`data.metadata...`).
    let module = module_object(ctx, id, path, snapshot_id.as_deref())?;
    // The reference registers the module under every cache key of the path
    // (`reference/cpp/plugins/motionplayer/ResourceManager.cpp:166-169`);
    // each key holds its own handle to the same TJS object.
    for key in cache_keys(path) {
        let duplicate = unsafe { ctx.engine() }
            .duplicate_retained(module.raw_id())
            .map_err(|error| error.to_string())?;
        inst.loaded.insert(key, StoredValue::Object(duplicate));
    }
    inst.last = Some(StoredValue::Object(module));
    Ok(id)
}

/// Build the TJS `%{path, metadata, object, __motionSnapshot}` module.
fn module_object(
    ctx: &CompatCtx,
    id: u64,
    path: &str,
    snapshot: Option<&MotionSnapshot>,
) -> Result<DetachedValue, String> {
    let object = eval_object(ctx, "%[]")?;
    let engine = unsafe { ctx.engine() };
    engine
        .set_member(object.raw_id(), "path", &TjsValue::String(path.to_string()))
        .map_err(|error| error.to_string())?;
    engine
        .set_member(
            object.raw_id(),
            "__motionSnapshot",
            &TjsValue::Integer(id as i64),
        )
        .map_err(|error| error.to_string())?;
    if let Some(snapshot) = snapshot {
        let metadata = build_psb(ctx, &snapshot.metadata)?;
        install(ctx, object.raw_id(), "metadata", metadata)?;
        let object_tree = build_psb(ctx, &snapshot.root)?;
        install(ctx, object.raw_id(), "object", object_tree)?;
        engine
            .set_member(object.raw_id(), "width", &TjsValue::Real(snapshot.width))
            .map_err(|error| error.to_string())?;
        engine
            .set_member(object.raw_id(), "height", &TjsValue::Real(snapshot.height))
            .map_err(|error| error.to_string())?;
    }
    Ok(object)
}

/// A value ready to install on a TJS object.
enum Built {
    Scalar(TjsValue),
    Object(DetachedValue),
}

fn install(
    ctx: &CompatCtx,
    parent: tjs::Tjs2ValueId,
    key: &str,
    built: Built,
) -> Result<(), String> {
    let engine = unsafe { ctx.engine() };
    match built {
        Built::Scalar(value) => engine
            .set_member(parent, key, &value)
            .map_err(|error| error.to_string()),
        Built::Object(object) => {
            let id = object.raw_id() as usize as u64;
            engine
                .set_member(parent, key, &TjsValue::Retained(id))
                .map_err(|error| error.to_string())?;
            drop(object);
            Ok(())
        }
    }
}

fn build_psb(ctx: &CompatCtx, value: &PsbValue) -> Result<Built, String> {
    match value {
        PsbValue::Null => Ok(Built::Scalar(TjsValue::Void)),
        PsbValue::Bool(value) => Ok(Built::Scalar(TjsValue::Integer(i64::from(*value)))),
        PsbValue::Int(value) => Ok(Built::Scalar(TjsValue::Integer(*value))),
        PsbValue::Float(value) => Ok(Built::Scalar(TjsValue::Real(f64::from(*value)))),
        PsbValue::Double(value) => Ok(Built::Scalar(TjsValue::Real(*value))),
        PsbValue::String(value) => Ok(Built::Scalar(TjsValue::String(value.clone()))),
        PsbValue::Array(values) => {
            let array = eval_object(ctx, "[]")?;
            let engine = unsafe { ctx.engine() };
            for (index, value) in values.iter().enumerate() {
                engine
                    .set_member(
                        array.raw_id(),
                        &index.to_string(),
                        &TjsValue::Integer(i64::from(*value)),
                    )
                    .map_err(|error| error.to_string())?;
            }
            Ok(Built::Object(array))
        }
        PsbValue::List(items) => {
            let array = eval_object(ctx, "[]")?;
            for (index, item) in items.iter().enumerate() {
                let built = build_psb(ctx, item)?;
                install(ctx, array.raw_id(), &index.to_string(), built)?;
            }
            Ok(Built::Object(array))
        }
        PsbValue::Dict(entries) => {
            let dictionary = eval_object(ctx, "%[]")?;
            for (key, item) in entries {
                let built = build_psb(ctx, item)?;
                install(ctx, dictionary.raw_id(), key, built)?;
            }
            Ok(Built::Object(dictionary))
        }
        PsbValue::Resource(index, extra) => Ok(Built::Scalar(TjsValue::Integer(if *extra {
            (*index as i64) | 0x4000_0000
        } else {
            *index as i64
        }))),
    }
}

extern "C" fn resource_load(
    engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    argc: std::os::raw::c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut std::ffi::c_void,
) -> std::os::raw::c_int {
    let Some(res) = resolve(engine) else {
        return report_error(out_error, super::helpers::NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<ResourceInst>(instance) }) else {
        return report_error(out_error, "ResourceManager: null instance");
    };
    // SAFETY: the trampoline guarantees `argc` valid entries.
    let arguments = unsafe { args(argv, argc) };
    // `tTJSVariant load(ttstr path)` / `loadSource(ttstr path)` — ncbind
    // arity 1 (`reference/cpp/plugins/motionplayer/ResourceManager.h:19-20`).
    if arguments.is_empty() {
        return report_error(out_error, "ResourceManager.load: bad parameter count");
    }
    let path = arguments.first().map(value_as_string).unwrap_or_default();
    if path.is_empty() {
        set_void(out);
        return 0;
    }
    match load_module(&res.ctx(), inst, &path) {
        Ok(_) => {
            if let Some(StoredValue::Object(module)) = inst.last.as_ref() {
                let ctx = res.ctx();
                let engine = unsafe { ctx.engine() };
                if let Ok(copy) = engine.duplicate_retained(module.raw_id()) {
                    set_retained(out, copy);
                    return 0;
                }
            }
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

extern "C" fn resource_load_source(
    engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    argc: std::os::raw::c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
    objthis: *mut std::ffi::c_void,
) -> std::os::raw::c_int {
    resource_load(engine, instance, argc, argv, out, out_error, objthis)
}

extern "C" fn resource_unload(
    engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    argc: std::os::raw::c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut std::ffi::c_void,
) -> std::os::raw::c_int {
    let Some(_res) = resolve(engine) else {
        return report_error(out_error, super::helpers::NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<ResourceInst>(instance) }) else {
        return report_error(out_error, "ResourceManager: null instance");
    };
    // SAFETY: the trampoline guarantees `argc` valid entries.
    let arguments = unsafe { args(argv, argc) };
    // `void unload(ttstr path)` — ncbind arity 1
    // (`reference/cpp/plugins/motionplayer/ResourceManager.h:21`).
    if arguments.is_empty() {
        return report_error(out_error, "ResourceManager.unload: bad parameter count");
    }
    let path = arguments.first().map(value_as_string).unwrap_or_default();
    // The reference drops every cache key that points at the same module
    // object, then falls back to erasing the path's own keys
    // (`reference/cpp/plugins/motionplayer/ResourceManager.cpp:172-205`).
    let victim = cache_keys(&path)
        .iter()
        .filter_map(|key| inst.loaded.get(key))
        .find_map(StoredValue::object_id);
    match victim {
        Some(id) => {
            inst.loaded
                .retain(|_, module| module.object_id() != Some(id));
        }
        None => {
            for key in cache_keys(&path) {
                inst.loaded.remove(&key);
            }
        }
    }
    set_void(out);
    0
}

extern "C" fn resource_clear_cache(
    engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    _argc: std::os::raw::c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut std::ffi::c_void,
) -> std::os::raw::c_int {
    let Some(_res) = resolve(engine) else {
        return report_error(out_error, super::helpers::NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<ResourceInst>(instance) }) else {
        return report_error(out_error, "ResourceManager: null instance");
    };
    inst.loaded.clear();
    // `clearCache` also resets the layer-id allocator in the reference
    // (`reference/cpp/plugins/motionplayer/ResourceManager.cpp:207-224`):
    // the next `requireLayerId` hands out 1 again.
    inst.layer_ids.clear();
    inst.layer_names.clear();
    inst.used.clear();
    inst.next = 0;
    set_void(out);
    0
}

extern "C" fn resource_find_source(
    engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    argc: std::os::raw::c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut std::ffi::c_void,
) -> std::os::raw::c_int {
    let Some(_res) = resolve(engine) else {
        return report_error(out_error, super::helpers::NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<ResourceInst>(instance) }) else {
        return report_error(out_error, "ResourceManager: null instance");
    };
    // SAFETY: the trampoline guarantees `argc` valid entries.
    let arguments = unsafe { args(argv, argc) };
    // `tTJSVariant findSource(ttstr path)` — ncbind arity 1
    // (`reference/cpp/plugins/motionplayer/ResourceManager.h:27`).
    if arguments.is_empty() {
        return report_error(out_error, "ResourceManager.findSource: bad parameter count");
    }
    let path = arguments.first().map(value_as_string).unwrap_or_default();
    // `findSource` returns the cached module object itself (void on a miss)
    // (`reference/cpp/plugins/motionplayer/ResourceManager.cpp:263-265`).
    match inst.find_key(&path) {
        Some(module) => {
            let ctx = _res.ctx();
            match module.to_retained(&ctx) {
                Ok(RetainedValue::Object(copy)) => {
                    set_retained(out, copy);
                    0
                }
                _ => {
                    set_void(out);
                    0
                }
            }
        }
        None => {
            set_void(out);
            0
        }
    }
}

extern "C" fn resource_require_layer_id(
    engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    _argc: std::os::raw::c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut std::ffi::c_void,
) -> std::os::raw::c_int {
    let Some(_res) = resolve(engine) else {
        return report_error(out_error, super::helpers::NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<ResourceInst>(instance) }) else {
        return report_error(out_error, "ResourceManager: null instance");
    };
    let id = inst.require_layer_id();
    inst.layer_ids.insert(id, String::new());
    set_int(out, id);
    0
}

extern "C" fn resource_release_layer_id(
    engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    argc: std::os::raw::c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut std::ffi::c_void,
) -> std::os::raw::c_int {
    let Some(_res) = resolve(engine) else {
        return report_error(out_error, super::helpers::NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<ResourceInst>(instance) }) else {
        return report_error(out_error, "ResourceManager: null instance");
    };
    // SAFETY: the trampoline guarantees `argc` valid entries.
    let arguments = unsafe { args(argv, argc) };
    // `void releaseLayerId(tjs_int id)` — ncbind arity 1
    // (`reference/cpp/plugins/motionplayer/ResourceManager.h:37`).
    if arguments.is_empty() {
        return report_error(
            out_error,
            "ResourceManager.releaseLayerId: bad parameter count",
        );
    }
    let id = arguments.first().map(value_as_i64).unwrap_or(0);
    inst.used.remove(&id);
    if let Some(name) = inst.layer_ids.remove(&id) {
        inst.layer_names.remove(&name);
    }
    set_void(out);
    0
}

extern "C" fn resource_set_decrypt_seed(
    engine: *mut std::ffi::c_void,
    argc: std::os::raw::c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
) -> std::os::raw::c_int {
    let Some(res) = resolve(engine) else {
        return report_error(out_error, super::helpers::NO_BINDING);
    };
    // SAFETY: the trampoline guarantees `argc` valid entries.
    let arguments = unsafe { args(argv, argc) };
    // Typed validation straight from the reference static member: exactly
    // one argument, and it must be an integer — anything else fails with
    // `TJS_E_BADPARAMCOUNT` / `TJS_E_INVALIDPARAM` instead of silently
    // defaulting to 0
    // (`reference/cpp/plugins/motionplayer/ResourceManager.cpp:112-121`).
    if arguments.len() != 1 {
        return report_error(out_error, "setEmotePSBDecryptSeed: bad parameter count");
    }
    if arguments[0].ty != tjs::VAL_INTEGER {
        return report_error(out_error, "setEmotePSBDecryptSeed: invalid parameter");
    }
    let seed = value_as_i64(&arguments[0]);
    let ctx = res.ctx();
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    services.state::<MotionStore>().decrypt_seed = seed;
    set_void(out);
    0
}

extern "C" fn resource_set_decrypt_func(
    engine: *mut std::ffi::c_void,
    argc: std::os::raw::c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut std::ffi::c_char,
) -> std::os::raw::c_int {
    let Some(res) = resolve(engine) else {
        return report_error(out_error, super::helpers::NO_BINDING);
    };
    // SAFETY: the trampoline guarantees `argc` valid entries.
    let arguments = unsafe { args(argv, argc) };
    // Exactly one argument, per the reference static member
    // (`reference/cpp/plugins/motionplayer/ResourceManager.cpp:129-132`);
    // an object value stores the closure, anything else clears it
    // (`ResourceManager.cpp:136-141`).
    if arguments.len() != 1 {
        return report_error(out_error, "setEmotePSBDecryptFunc: bad parameter count");
    }
    let ctx = res.ctx();
    let mut store = unsafe { ctx.services() }.state::<MotionStore>();
    store.decrypt_func = None;
    if let Some(value) = arguments.first()
        && value.ty == VAL_OBJECT
        && let Ok(retained) = unsafe { ctx.engine() }.retain_object_arg(value)
    {
        store.decrypt_func = Some(retained);
    }
    set_void(out);
    0
}

/// Register `Motion.ResourceManager` and its static decrypt hooks.
pub(crate) fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    use tjs::{NativeInstanceBuilder, NativeInstanceMethodDef};

    ctx.register_instance(&NativeInstanceBuilder {
        name: "ResourceManager",
        create: resource_create,
        destroy: resource_destroy,
        invalidate: None,
        methods: vec![
            NativeInstanceMethodDef {
                name: "ResourceManager",
                f: resource_ctor,
            },
            NativeInstanceMethodDef {
                name: "load",
                f: resource_load,
            },
            NativeInstanceMethodDef {
                name: "loadSource",
                f: resource_load_source,
            },
            NativeInstanceMethodDef {
                name: "unload",
                f: resource_unload,
            },
            NativeInstanceMethodDef {
                name: "clearCache",
                f: resource_clear_cache,
            },
            NativeInstanceMethodDef {
                name: "findSource",
                f: resource_find_source,
            },
            NativeInstanceMethodDef {
                name: "requireLayerId",
                f: resource_require_layer_id,
            },
            NativeInstanceMethodDef {
                name: "releaseLayerId",
                f: resource_release_layer_id,
            },
        ],
        properties: vec![],
    })?;
    ctx.register_static_members(&tjs::NativeStaticMembers {
        class_name: "ResourceManager",
        methods: vec![
            tjs::NativeMethodDef {
                name: "setEmotePSBDecryptSeed",
                f: resource_set_decrypt_seed,
            },
            tjs::NativeMethodDef {
                name: "setEmotePSBDecryptFunc",
                f: resource_set_decrypt_func,
            },
        ],
        properties: vec![],
    })?;
    Ok(())
}
