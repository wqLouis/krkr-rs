//! The KAG window object does **not** own the title's after-init callback
//! tables: they are globals of the title's own script runtime.
//!
//! A shipped title keeps its KAG runtime in its own scripts — the window
//! class (`system/mainwindow.tjs`), the world plugin (`system/world.tjs`)
//! and the scenario overrides (`sysscn/override.tjs`). The engine ships
//! only the scenario **parser**, the `KAGParser` native class, and the
//! reference registers exactly these members on it
//! (`reference/cpp/core/base/KAGParser.cpp:2213-2545`): the methods
//! `loadScenario`, `goToLabel`, `callLabel`, `getNextTag`, `assign`,
//! `clear`, `store`, `restore`, `clearCallStack`, `popMacroArgs`,
//! `interrupt`, `resetInterrupt`, and the properties `curLine`, `curPos`,
//! `curLineStr`, `processSpecialTags`, `ignoreCR`, `debugLevel`, `macros`,
//! `macroParams`, `mp`, `callStackDepth`, `curStorage`, `curLabel`. There is
//! no `tagHandlers`, no `afterInitCallback` and no `kagHookEntries`: the
//! tag-handler table and the two after-init tables belong to the title.
//!
//! The title's `sysscn/override.tjs` creates the two after-init tables as
//! **globals** at its top level — `this.afterInitCallback = new Array();`
//! and `this.kagHookEntries = new Array();` — and a script's top-level
//! `this` is the global object: the reference runs a top-level context with
//! `objthis ? objthis : GetGlobalNoAddRef()`
//! (`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2786-2793`), and
//! `Scripts.execStorage` without a context passes no context
//! (`reference/cpp/core/base/ScriptMgnIntf.cpp:1323-1325`).
//!
//! The title queues its tag handlers with `addKagHandler` (which pushes a
//! dictionary onto the global table) and applies them in `callAfterInit`,
//! which reads the table and then **deletes** it: the table is one-shot.
//! Reading it afterwards raises the title's
//! `Member "afterInitCallback" does not exist` — a missing member of a
//! script object is `TJS_E_MEMBERNOTFOUND`
//! (`reference/cpp/core/tjs2/tjsObject.cpp:1276-1315`) and the VM turns
//! that into the message
//! (`reference/cpp/core/tjs2/tjsError.cpp:255-266`).
//!
//! This test pins that contract: the engine's KAG window object owns
//! neither table, and the title's chain — create, queue, apply, delete —
//! runs against the engine as it stands.

mod common;

use common::{Env, vm_lock};
use tjs::TjsValue;

/// The title's `sysscn/override.tjs` top level and its `addKagHandler` /
/// `callAfterInit`, in the title's own shape.
const TITLE_RUNTIME: &str = r#"
    // The two after-init tables are globals: a script's top-level `this` is
    // the global object.
    this.afterInitCallback = new Array();
    this.kagHookEntries = new Array();

    global.applied = "";
    global.queued = 0;
    global.entryMode = "";
    global.entryHandle = "";
    global.result = "";

    // The engine's KAG window base class, as the title's window class uses it.
    class Runtime extends KAGParser
    {
        function Runtime() { super.KAGParser(); }
    }
    global.runtime = new Runtime();

    function handler(ctx) { global.applied = "ran"; }

    function addKagHandler(handle, func)
    {
        var d = new Dictionary();
        d.mode = "handler";
        d.handle = handle;
        d.func = func;
        d.kagcontext = this;
        this.afterInitCallback.push(d);
    }

    function callAfterInit()
    {
        var out = "";
        for (var i = 0; i < this.afterInitCallback.count; i++)
        {
            var e = this.afterInitCallback[i];
            if (e.mode == "handler") {
                e.func(e.kagcontext);
                out += e.handle;
            }
        }
        delete global.afterInitCallback;
        return out;
    }

    addKagHandler("title_bg", handler);
    global.queued = this.afterInitCallback.count;
    global.entryMode = this.afterInitCallback[0].mode;
    global.entryHandle = this.afterInitCallback[0].handle;
    global.result = callAfterInit();
"#;

/// The engine's KAG window object owns neither after-init table: the
/// reference's `KAGParser` registers no such member, so the title's tables
/// are the title's own globals.
#[test]
fn the_kag_window_object_does_not_own_the_after_init_tables() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(TITLE_RUNTIME);

    for member in ["afterInitCallback", "kagHookEntries", "tagHandlers"] {
        assert_eq!(
            env.eval_ok(&format!("typeof global.runtime.{member}")),
            TjsValue::String("undefined".to_string()),
            "the engine's KAG window object owns no {member}"
        );
    }
    // The reference's members are there: the window object is the parser.
    assert_eq!(
        env.eval_ok("typeof global.runtime.getNextTag"),
        TjsValue::String("Object".to_string())
    );
}

/// The title's after-init chain: the table is a global, the handler is
/// queued on it, `callAfterInit` applies the handler and then deletes the
/// table.
#[test]
fn the_after_init_table_is_a_global_the_title_creates_queues_and_consumes() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(TITLE_RUNTIME);

    // The handler was queued on the global table.
    assert_eq!(env.eval_int("global.queued"), 1);
    assert_eq!(
        env.eval_ok("global.entryMode"),
        TjsValue::String("handler".to_string())
    );
    assert_eq!(
        env.eval_ok("global.entryHandle"),
        TjsValue::String("title_bg".to_string())
    );
    // `callAfterInit` applied the handler and returned its handle.
    assert_eq!(
        env.eval_ok("global.result"),
        TjsValue::String("title_bg".to_string())
    );
    assert_eq!(
        env.eval_ok("global.applied"),
        TjsValue::String("ran".to_string())
    );
    // The table is one-shot: `callAfterInit` deleted it, so the next read
    // is a missing member.
    let error = env.error("global.afterInitCallback");
    assert!(
        error.contains("afterInitCallback") && error.contains("does not exist"),
        "the deleted table reads as a missing member: {error}"
    );
}
