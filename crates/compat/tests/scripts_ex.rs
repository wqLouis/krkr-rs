//! Integration tests for the `scripts_ex` plugin: the `Scripts` static
//! members (`getObjectKeys` / `getObjectCount` / `getObjectContext` /
//! `isNullContext` / `equalStruct` / `equalStructNumericLoose` / `foreach` /
//! `getMD5HashString` / `clone` / `propGet` / `propSet` / `safeEvalStorage` /
//! `rehash` / the `pf*` constants).
//!
//! Every test serializes on the shared VM lock, since the process-wide
//! engine → context binding is single.

mod common;

use std::path::{Path, PathBuf};

use common::vm_lock;
use compat::CompatHost;
use engine::Storage;
use fs::FileSystem;
use tjs::{Tjs2Engine, TjsValue};

/// A unique temporary game directory, removed on drop.
struct GameDir(PathBuf);

impl GameDir {
    fn new() -> Self {
        let mut path = std::env::temp_dir();
        let unique = format!(
            "krkr-compat-scripts-ex-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        );
        path.push(unique);
        std::fs::create_dir_all(&path).expect("create temp game dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self.0.join(name), bytes).expect("write fixture");
    }
}

impl Drop for GameDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A bound VM with a mounted storage attached to the compat services.
struct Env {
    // Drop order matters: the host removes the process-wide binding before
    // the boxed engine/storage it points at are freed.
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    _storage: Box<Storage>,
    _dir: GameDir,
}

impl Env {
    fn new() -> Env {
        let dir = GameDir::new();
        let mut storage = Box::new(
            Storage::mount_archives(
                FileSystem::host(dir.path()),
                dir.path().to_path_buf(),
                std::iter::empty::<&str>(),
            )
            .expect("mount storage"),
        );
        // Boxed so the address stays stable when it is moved into `Env`.
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut host = CompatHost::new();
        // SAFETY: `storage` is boxed and outlives the binding, which `_host`
        // removes on drop.
        unsafe { host.services_mut().attach_storage(&mut *storage) };
        host.install(&engine).expect("install compat surface");
        Env {
            _host: host,
            engine,
            _storage: storage,
            _dir: dir,
        }
    }

    fn write_storage(&self, name: &str, text: &str) {
        self._dir.write(name, text.as_bytes());
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "scripts-ex-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval(&self, expression: &str) -> TjsValue {
        self.engine
            .eval(expression, "scripts-ex-test")
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
    }

    fn int(&self, expression: &str) -> i64 {
        match self.eval(expression) {
            TjsValue::Integer(value) => value,
            TjsValue::Real(value) => value as i64,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    fn string(&self, expression: &str) -> String {
        match self.eval(expression) {
            TjsValue::String(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected a string"),
        }
    }

    /// Evaluate an expression that must raise, returning the message.
    fn error(&self, expression: &str) -> String {
        self.engine
            .eval(expression, "scripts-ex-test")
            .expect_err(&format!("{expression:?} should have raised"))
            .to_string()
    }
}

// ---------------------------------------------------------------------------
// getObjectKeys / getObjectCount
// ---------------------------------------------------------------------------

/// `_getKeys` enumerates with `TJS_IGNOREPROP | TJS_ENUM_NO_VALUE` and
/// sorts the result (`reference/cpp/plugins/scriptsEx.cpp:438-450`);
/// `getCount` reads the dispatch `GetCount` (`scriptsEx.cpp:468-485`),
/// which for a dictionary is its own-member count
/// (`reference/cpp/core/tjs2/tjsObject.cpp:1431-1442`). Arrays report `0`
/// through that path — see
/// [`object_keys_and_count_for_arrays_follow_the_reference`].
#[test]
fn object_keys_are_sorted_and_counted() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var dict = %[b: 2, a: 1, c: 3]; \
         var keys = Scripts.getObjectKeys(dict); \
         var count = Scripts.getObjectCount(dict); \
         var array = [10, 20, 30]; \
         var arrayCount = Scripts.getObjectCount(array);",
    );
    assert_eq!(env.string("keys[0]"), "a");
    assert_eq!(env.string("keys[1]"), "b");
    assert_eq!(env.string("keys[2]"), "c");
    assert_eq!(env.int("keys.count"), 3);
    assert_eq!(env.int("count"), 3);
    // A TJS array's elements are not object members: the reference's
    // `GetCount` reports `0` (see the array test below).
    assert_eq!(env.int("arrayCount"), 0);
}

/// A TJS `Array` reports no *object members* through the reference's two
/// introspection helpers:
///
/// * `_getKeys` drives `EnumMembers`
///   (`reference/cpp/plugins/scriptsEx.cpp:443-445`), which `tTJSArrayObject`
///   rejects with `TJS_E_NOTIMPL` (`reference/cpp/core/tjs2/tjsArray.h:221-223`);
///   the callback never runs, the error is discarded and the (sorted)
///   result array stays **empty** (`scriptsEx.cpp:446-449`).
/// * `getCount` reads the dispatch `GetCount` (`scriptsEx.cpp:477-480`),
///   which `tTJSArrayObject` does not override (`tjsArray.h:213-214`) —
///   `tTJSCustomObject::GetCount` then reports the instance's own-member
///   count (`tjsObject.cpp:1431-1442`), `0` for elements stored in the
///   native `Items` vector.
///
/// Direct element access and the `count` *property* are unaffected.
#[test]
fn object_keys_and_count_for_arrays_follow_the_reference() {
    let _lock = vm_lock();
    let env = Env::new();
    // The array is bound to a name so `count` is read through the variable.
    // The scratch variable cannot be called `property`: that word starts a
    // `property` declaration in this grammar, so `var property = …` is a
    // syntax error.
    env.run(
        "var array = [10, 20, 30]; \
         var keys = Scripts.getObjectKeys([10, 20, 30]); \
         var count = Scripts.getObjectCount([10, 20, 30]); \
         var element = array[1]; \
         var prop = array.count;",
    );
    assert_eq!(env.int("keys.count"), 0);
    assert_eq!(env.int("count"), 0);
    assert_eq!(env.int("element"), 20);
    assert_eq!(env.int("prop"), 3);
}

// ---------------------------------------------------------------------------
// getObjectContext / isNullContext
// ---------------------------------------------------------------------------

/// `getObjectContext` returns the variant closure's `ObjThis` and
/// `isNullContext` tests exactly that pointer
/// (`reference/cpp/plugins/scriptsEx.cpp:488-504`): a dictionary carries
/// its own context, a bare anonymous function does not.
#[test]
fn object_context_and_null_context() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var dict = %[a: 1]; \
         var sameContext = (Scripts.getObjectContext(dict) === dict); \
         var nullContext = Scripts.isNullContext(dict); \
         var anonymous = function() { return 1; }; \
         var anonymousNull = Scripts.isNullContext(anonymous);",
    );
    assert_eq!(env.int("sameContext"), 1);
    assert_eq!(env.int("nullContext"), 0);
    // A bare anonymous function has no bound receiver.
    assert_eq!(env.int("anonymousNull"), 1);
}

// ---------------------------------------------------------------------------
// equalStruct / equalStructNumericLoose
// ---------------------------------------------------------------------------

/// `equalStruct` special-cases Array and Dictionary and otherwise falls
/// back to `DiscernCompare` — strict and typed, so `1 !== 1.0`
/// (`reference/cpp/plugins/scriptsEx.cpp:506-585`); the numeric-loose
/// variant accepts mixed integer/real leaves through `NormalCompare`
/// (`scriptsEx.cpp:590-676`).
#[test]
fn structural_equality_is_recursive() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var equalNested = Scripts.equalStruct(%[a: 1, b: [1, [2, 3]]], %[a: 1, b: [1, [2, 3]]]); \
         var unequalValue = Scripts.equalStruct(%[a: 1], %[a: 2]); \
         var unequalKeys = Scripts.equalStruct(%[a: 1], %[b: 1]); \
         var arraysEqual = Scripts.equalStruct([1, 2, 3], [1, 2, 3]); \
         var strictNumeric = Scripts.equalStruct(1, 1.0); \
         var looseNumeric = Scripts.equalStructNumericLoose(1, 1.0); \
         var looseNested = Scripts.equalStructNumericLoose(%[a: 0.0], %[a: 0]); \
         var instanceEqual = Scripts.equalStruct(%[x: [1]], %[x: [1]]); \
         var instanceLoose = Scripts.equalStructNumericLoose([0], [0.0]);",
    );
    assert_eq!(env.int("equalNested"), 1);
    assert_eq!(env.int("unequalValue"), 0);
    assert_eq!(env.int("unequalKeys"), 0);
    assert_eq!(env.int("arraysEqual"), 1);
    assert_eq!(env.int("strictNumeric"), 0);
    assert_eq!(env.int("looseNumeric"), 1);
    assert_eq!(env.int("looseNested"), 1);
    assert_eq!(env.int("instanceEqual"), 1);
    assert_eq!(env.int("instanceLoose"), 1);
}

// ---------------------------------------------------------------------------
// foreach
// ---------------------------------------------------------------------------

/// `foreach` requires two arguments (`reference/cpp/plugins/scriptsEx.cpp:681-684`),
/// walks arrays by index (`scriptsEx.cpp:707-726`) and dictionaries by
/// `EnumMembers` (`scriptsEx.cpp:731-747`), forwards the caller's extra
/// arguments (`scriptsEx.cpp:704-705`), stops at the first non-`void`
/// callback result and returns it (`scriptsEx.cpp:718-725`).
#[test]
fn foreach_walks_arrays_and_dictionaries() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var staticSum = 0; \
         Scripts.foreach([1, 2, 3], function(key, value) { staticSum += value; }); \
         var stopped = Scripts.foreach([1, 2, 3], function(key, value) { if (value == 2) return value * 10; }); \
         var visited = ''; \
         Scripts.foreach(%[a: 1, b: 2], function(key, value, sep) { visited += key + sep + value; }, ','); \
         var visitedKeys = ''; \
         Scripts.foreach(%[x: 9], function(key, value) { visitedKeys = key; });",
    );
    assert_eq!(env.int("staticSum"), 6);
    assert_eq!(env.int("stopped"), 20);
    assert_eq!(env.string("visited"), "a,1b,2");
    assert_eq!(env.string("visitedKeys"), "x");
}

// ---------------------------------------------------------------------------
// clone
// ---------------------------------------------------------------------------

/// `clone` deep-copies Arrays (`reference/cpp/plugins/scriptsEx.cpp:818-837`)
/// and Dictionaries (`scriptsEx.cpp:839-853`) recursively; scalars and
/// strings come back unchanged (`scriptsEx.cpp:864`).
#[test]
fn clone_deep_copies_collections() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var original = [1, [2, 3], %[k: 4]]; \
         var copy = Scripts.clone(original); \
         copy[1].add(9); \
         copy[2].k = 99; \
         var originalNestedCount = original[1].count; \
         var copyNestedCount = copy[1].count; \
         var originalDict = original[2].k; \
         var copyDict = copy[2].k; \
         var dict = %[x: [1, 2]]; \
         var dictCopy = Scripts.clone(dict); \
         dictCopy.x.add(3); \
         var scalar = Scripts.clone(5); \
         var text = Scripts.clone('hi');",
    );
    assert_eq!(env.int("originalNestedCount"), 2);
    assert_eq!(env.int("copyNestedCount"), 3);
    assert_eq!(env.int("originalDict"), 4);
    assert_eq!(env.int("copyDict"), 99);
    assert_eq!(env.int("dict.x.count"), 2);
    assert_eq!(env.int("dictCopy.x.count"), 3);
    assert_eq!(env.int("scalar"), 5);
    assert_eq!(env.string("text"), "hi");
}

/// For every object that is neither an Array nor a Dictionary the reference
/// invokes the object's `clone` member but returns its result only when
/// `FuncCall(...) == TJS_S_TRUE` (`reference/cpp/plugins/scriptsEx.cpp:855-863`).
/// `TJS_S_TRUE` is `1` while a successfully executed script function
/// returns `TJS_S_OK` = `0`
/// (`reference/cpp/core/tjs2/tjsErrorDefs.h:32-35`), so the member's side
/// effects run and the **original** object comes back.
#[test]
fn clone_invokes_the_object_but_returns_the_original() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "class Holder { \
           function Holder() { this.tag = 7; this.calls = 0; } \
           function clone() { this.calls += 1; return 'cloned'; } \
         } \
         var h = new Holder(); \
         var copy = Scripts.clone(h); \
         var same = (copy === h); \
         var calls = h.calls; \
         var tag = copy.tag;",
    );
    assert_eq!(env.int("same"), 1, "clone must return the original object");
    assert_eq!(env.int("calls"), 1, "the clone member must have run");
    assert_eq!(env.int("tag"), 7);
}

// ---------------------------------------------------------------------------
// getMD5HashString
// ---------------------------------------------------------------------------

/// `getMD5HashString` requires one octet argument
/// (`reference/cpp/plugins/scriptsEx.cpp:756-761`) and renders the digest
/// with the lowercase hex alphabet (`scriptsEx.cpp:772-778`) — the same
/// `TVP_md5_*` output as `Storages.getMD5HashString`.
#[test]
fn md5_matches_the_reference_vector() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var abc = [97, 98, 99].pack('C3'); \
         var hash = Scripts.getMD5HashString(abc);",
    );
    assert_eq!(env.string("hash"), "900150983cd24fb0d6963f7d28e17f72");
}

// ---------------------------------------------------------------------------
// propGet / propSet
// ---------------------------------------------------------------------------

/// `propSet` defaults its flag to `TJS_MEMBERENSURE` and `propGet` to
/// `TJS_MEMBERMUSTEXIST` (`reference/cpp/plugins/scriptsEx.cpp:875-876,896-897`).
/// An integer key routes through `PropSetByNum`/`PropGetByNum`, which a TJS
/// Array resolves to element access by numeric name
/// (`reference/cpp/core/tjs2/tjsArray.cpp:1584,1606`), so both spellings
/// address the same element.
#[test]
fn prop_get_set_work_for_scripts() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var dict = %[]; \
         Scripts.propSet(dict, 'k', 9); \
         var staticGet = Scripts.propGet(dict, 'k'); \
         Scripts.propSet(dict, 'inst', 7); \
         var instanceGet = Scripts.propGet(dict, 'inst'); \
         var array = [1, 2, 3]; \
         Scripts.propSet(array, 0, 99); \
         var numericGet = Scripts.propGet(array, 0); \
         Scripts.propSet(array, 1, 88); \
         var numericInstance = Scripts.propGet(array, 1);",
    );
    assert_eq!(env.int("staticGet"), 9);
    assert_eq!(env.int("instanceGet"), 7);
    assert_eq!(env.int("numericGet"), 99);
    assert_eq!(env.int("numericInstance"), 88);
    // The underlying array really changed.
    assert_eq!(env.int("array[0]"), 99);
    assert_eq!(env.int("array[1]"), 88);
}

/// The reference forwards the flag to `PropSet`/`PropGet`
/// (`reference/cpp/plugins/scriptsEx.cpp:875-876,896-897`): without
/// `TJS_MEMBERENSURE` a write to a missing member fails, while
/// `TJS_MEMBERENSURE` *creates* a missing member as `void` on a read
/// (`reference/cpp/core/tjs2/tjsObject.cpp:1352-1383,1276-1309`). The
/// defaults keep the documented `pf*` constants working
/// (`scriptsEx.cpp:983-987`).
#[test]
fn prop_flags_control_creation_and_existence() {
    let _lock = vm_lock();
    let env = Env::new();

    // Default flag: the member is created.
    env.run("var d = %[]; Scripts.propSet(d, 'made', 7);");
    assert_eq!(env.int("d['made']"), 7);

    // Explicit TJS_MEMBERENSURE behaves the same.
    env.run("Scripts.propSet(d, 'ensured', 8, Scripts.pfMemberEnsure);");
    assert_eq!(env.int("d['ensured']"), 8);

    // Flag 0 on a missing member: the write fails (typed error, nothing
    // is stored).
    let error = env.error("Scripts.propSet(d, 'absent', 1, 0)");
    assert!(error.contains("propSet"), "unexpected error: {error}");
    assert_eq!(env.int("Scripts.getObjectCount(d)"), 2);

    // Default propGet: a missing member is an error.
    let error = env.error("Scripts.propGet(d, 'never')");
    assert!(error.contains("propGet"), "unexpected error: {error}");

    // TJS_MEMBERENSURE on a read: the member appears as void.
    env.run("var r = Scripts.propGet(d, 'fresh', Scripts.pfMemberEnsure);");
    assert_eq!(env.string("typeof r"), "void");
    assert_eq!(env.int("Scripts.getObjectCount(d)"), 3);
}

/// `propSet` needs three arguments and `propGet` two
/// (`reference/cpp/plugins/scriptsEx.cpp:871-872,892-893`); the reference
/// answers `TJS_E_BADPARAMCOUNT` (`"Invalid argument count"`,
/// `reference/cpp/core/tjs2/tjsError.h:268`), this port a typed error
/// naming the member.
#[test]
fn prop_argument_counts_are_typed_errors() {
    let _lock = vm_lock();
    let env = Env::new();

    let error = env.error("Scripts.propSet(%[], 'k')");
    assert!(
        error.contains("requires at least 3 arguments"),
        "unexpected error: {error}"
    );
    let error = env.error("Scripts.propGet(%[])");
    assert!(
        error.contains("requires at least 2 arguments"),
        "unexpected error: {error}"
    );
}

// ---------------------------------------------------------------------------
// safeEvalStorage
// ---------------------------------------------------------------------------

/// `safeEvalStorage` requires one argument
/// (`reference/cpp/plugins/scriptsEx.cpp:909-913`), wraps the storage
/// content in `(const)[…]` (`scriptsEx.cpp:938-947`), evaluates it under
/// the storage's base name (`scriptsEx.cpp:950`) and returns element `0`
/// of the resulting array (`scriptsEx.cpp:951-958`).
#[test]
fn safe_eval_storage_reads_element_zero() {
    let _lock = vm_lock();
    let env = Env::new();
    // `safeEvalStorage` wraps the storage content in `(const)[...]`, so the
    // fixture must be the constant-literal form (as written by `saveStruct`).
    env.write_storage("values.tjs", "1, 2, 3");
    env.write_storage("object.tjs", "(const)%['name', 'kirikiri', 'n', 7]");
    env.run(
        "var first = Scripts.safeEvalStorage('values.tjs'); \
         var object = Scripts.safeEvalStorage('object.tjs'); \
         var name = object.name; \
         var number = object.n;",
    );
    assert_eq!(env.int("first"), 1);
    assert_eq!(env.string("name"), "kirikiri");
    assert_eq!(env.int("number"), 7);
}

// ---------------------------------------------------------------------------
// rehash / constants
// ---------------------------------------------------------------------------

/// The `pf*` constants are the VM's own member flags
/// (`reference/cpp/plugins/scriptsEx.cpp:983-987`,
/// `reference/cpp/core/tjs2/tjsInterface.h:24-29`) and `rehash` is bound
/// straight to the VM's `TJSDoRehash` (`scriptsEx.cpp:993`), which returns
/// `void`.
#[test]
fn rehash_and_flag_constants() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("Scripts.rehash();");
    assert_eq!(env.eval("Scripts.rehash()"), TjsValue::Void);
    assert_eq!(env.int("Scripts.pfMemberEnsure"), 0x0000_0200);
    assert_eq!(env.int("Scripts.pfMemberMustExist"), 0x0000_0400);
    assert_eq!(env.int("Scripts.pfIgnoreProp"), 0x0000_0800);
    assert_eq!(env.int("Scripts.pfHiddenMember"), 0x0000_1000);
    assert_eq!(env.int("Scripts.pfStaticMember"), 0x0001_0000);

    // The reference registers each `pf*` with `Variant(name, value)`
    // (`reference/cpp/plugins/scriptsEx.cpp:983-987`); `Variant` writes a
    // `TJS_STATICMEMBER` data member through `PropSet`
    // (`reference/cpp/core/plugin/ncbind.hpp:1801,1875-1877`), so a script
    // assignment overwrites the value instead of raising.
    env.run("Scripts.pfMemberEnsure = 5;");
    assert_eq!(env.int("Scripts.pfMemberEnsure"), 5);
}

// ---------------------------------------------------------------------------
// builtins are still intact after instance member attachment
// ---------------------------------------------------------------------------

/// The plugin attaches only *static* members to the `Scripts` class
/// (`reference/cpp/plugins/scriptsEx.cpp:968-991`), so `Array.add`,
/// `Array.sort` and `Dictionary` assignment remain the VM's builtins
/// (`reference/cpp/core/tjs2/tjsArray.cpp:846,596`).
#[test]
fn builtin_array_and_dictionary_members_still_work() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var array = [3, 1, 2]; \
         array.add(4); \
         array.sort(); \
         var count = array.count; \
         var dictionary = %[b: 2]; \
         dictionary.a = 1; \
         var dictionaryCount = Scripts.getObjectCount(dictionary);",
    );
    assert_eq!(env.int("count"), 4);
    assert_eq!(env.int("array[0]"), 1);
    assert_eq!(env.int("array[3]"), 4);
    assert_eq!(env.int("dictionaryCount"), 2);
}

/// Reference: `safeEvalStorage(name, mode, context)` evaluates the wrapped
/// expression with the third argument as its execution context
/// (`tTJSVariantClosure`/`TVPExecuteExpression(..., context, ...)`,
/// `reference/cpp/plugins/scriptsEx.cpp:921-956`). A content expression that
/// reads `this` therefore resolves against that context.
#[test]
fn safe_eval_storage_evaluates_a_const_literal_storage() {
    let _lock = vm_lock();
    let env = Env::new();
    env.write_storage("data.tjs", "42");
    // `safeEvalStorage` wraps the storage in `(const)[...]` and returns
    // element 0 (`reference/cpp/plugins/scriptsEx.cpp:909-960`).
    env.run("var value = Scripts.safeEvalStorage('data.tjs');");
    assert_eq!(env.int("value"), 42);
}

// The wrapper is what makes the member safe: a `(const)[...]` literal admits
// constants only, so a storage that is not a constant raises instead of
// running (`reference/cpp/plugins/scriptsEx.cpp:909-960`). The third argument
// is still accepted, matching the reference's `TVPExecuteExpression` call.
#[test]
fn safe_eval_storage_rejects_a_non_constant_storage() {
    let _lock = vm_lock();
    let env = Env::new();
    env.write_storage("code.tjs", "this.answer");
    let error = env.error("Scripts.safeEvalStorage('code.tjs', '', %[answer: 42]);");
    assert!(error.contains("syntax error"), "{error}");
}

// ---------------------------------------------------------------------------
// plain dictionary enumeration
// ---------------------------------------------------------------------------

/// A plain `%[x: 1]` must enumerate exactly its own key. The built-in C++
/// serializer walks a `Dictionary` with `EnumMembers`
/// (`reference/cpp/core/tjs2/tjsDictionary.cpp:417-447`); the plugin members
/// the ABI used to attach to every instance must not appear. The port's
/// `Array.assign` flattening runs that same `EnumMembers` walk.
#[test]
fn plain_dictionary_enumerates_only_its_own_keys() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var d = %[x: 1]; \
         var flat = []; flat.assign(d); \
         var keys = Scripts.getObjectKeys(d); \
         var count = Scripts.getObjectCount(d);",
    );
    assert_eq!(env.int("flat.count"), 2, "assign must flatten [key, value]");
    assert_eq!(env.string("flat[0]"), "x");
    assert_eq!(env.int("flat[1]"), 1);
    assert_eq!(env.int("keys.count"), 1);
    assert_eq!(env.string("keys[0]"), "x");
    assert_eq!(env.int("count"), 1);
}
