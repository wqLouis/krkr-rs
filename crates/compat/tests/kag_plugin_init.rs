//! The KAG plugin init: the runtime's plugin hook loads its configuration
//! storage, and the tag the configuration names reaches the handler the
//! plugin registered.
//!
//! The chain is the one the shipped KAG runtime uses (the KAG the games
//! ship — `system/world.tjs` and `sysscn/override.tjs`):
//!
//!   * `KAGWorldPlugin.initInfo` loads the environment information — the
//!     game's `main/envinit.tjs`, a storage whose text is one dictionary
//!     literal — through `Scripts.evalStorage`, and keeps it as the
//!     plugin's `envinfo`.
//!   * the runtime's after-init step then applies the configuration: the
//!     tag it names is registered in the runtime's `tagHandlers` table.
//!   * the runtime's tag loop reads the tags with `KAGParser.getNextTag`
//!     and hands each one to the handler the table holds.
//!
//! The port's half is the native `KAGParser` (the tag dictionary and the
//! pull state machine) and `Scripts.evalStorage` (a storage whose text is
//! one expression yields the value, not a script run).
//!
//! Reference:
//!   * `reference/cpp/core/base/KAGParser.cpp:1367-1400` — `_GetNextTag`:
//!     the tag dictionary, its `tagname` member and the attributes.
//!   * `reference/cpp/core/base/KAGParser.cpp:1384` — the `tagname` member
//!     name.
//!   * `reference/cpp/core/base/KAGParser.cpp:2255-2271` — the native
//!     `getNextTag` member.
//!   * `reference/cpp/core/base/ScriptMgnIntf.cpp:1335-1354` —
//!     `Scripts.evalStorage`: the storage runs as an expression and its
//!     value is the result.
//!   * `reference/cpp/core/base/ScriptMgnIntf.cpp:711-760` —
//!     `TVPExecuteStorage`: the bytecode is tried first, the text second.
//!
//! The KAG runtime the chain belongs to — the oracle for the plugin hook
//! and the handler table:
//!   * `system/world.tjs` — `KAGWorldPlugin.loadEnvinfo`: the environment
//!     information is loaded and kept as the plugin's `envinfo`.
//!   * `sysscn/override.tjs` — `callAfterInit`: the tag handlers the
//!     configuration names are applied to the runtime's `tagHandlers`.

mod common;

use common::{Env, vm_lock};
use tjs::TjsValue;

/// The environment information the plugin init loads: one dictionary
/// literal, the shape the game's `main/envinit.tjs` has.
const ENVINIT: &str = "%[\n\t\"tag\" => \"title_bg\",\n\t\"file\" => \"bg.png\",\n]\n";

/// The runtime: the KAG window's parser, the plugin, the handler table and
/// the tag loop.
const RUNTIME: &str = r#"
        var seen = "";

        // The plugin: the shipped runtime's `KAGWorldPlugin` shape. Its init
        // hook loads the environment information and keeps it.
        class WorldPlugin
        {
            var envinfo;
            function WorldPlugin() {}
            function initInfo()
            {
                envinfo = Scripts.evalStorage("envinit.tjs");
            }
        }

        class Runtime extends KAGParser
        {
            var tagHandlers = %[];
            var plugin;

            function Runtime()
            {
                super.KAGParser();
                plugin = new WorldPlugin();
                // The plugin init: the configuration is loaded first, then
                // the handler the configuration names is registered.
                plugin.initInfo();
                tagHandlers[plugin.envinfo.tag] = function(tag) {
                    seen = tag.tagname + ":" + tag.file;
                };
            }

            function onScenarioLoad(name)
            {
                return "[title_bg file=\"bg.png\" show]";
            }
        }

        var runtime = new Runtime();
        runtime.loadScenario("title.ks");
        while(true)
        {
            var tag = runtime.getNextTag();
            if(tag === void) break;
            var handler = runtime.tagHandlers[tag.tagname];
            if(handler !== void) handler(tag);
        }
"#;

/// A runtime whose plugin init loaded its configuration storage.
fn runtime_env() -> Env {
    let env = Env::with_storage();
    let dir = env.game_dir().expect("the game dir").clone();
    std::fs::write(dir.join("envinit.tjs"), ENVINIT).expect("write the configuration");
    env.run(RUNTIME);
    env
}

/// The plugin's init hook loads the configuration storage and keeps the
/// dictionary it yields.
#[test]
fn the_plugin_init_keeps_the_configuration_it_loaded() {
    let _lock = vm_lock();
    let env = runtime_env();

    assert_eq!(
        env.eval_ok("runtime.plugin.envinfo.tag"),
        TjsValue::String("title_bg".into()),
        "the plugin's init hook loaded the configuration storage"
    );
    assert_eq!(
        env.eval_ok("runtime.plugin.envinfo.file"),
        TjsValue::String("bg.png".into()),
        "the configuration is the dictionary the storage holds"
    );
}

/// The tag the configuration names reaches the handler the plugin
/// registered, with the tag's own name and attribute.
#[test]
fn the_tag_the_configuration_names_reaches_its_handler() {
    let _lock = vm_lock();
    let env = runtime_env();

    assert_eq!(
        env.eval_ok("seen"),
        TjsValue::String("title_bg:bg.png".into()),
        "the tag reached the handler the plugin registered"
    );
    assert_eq!(
        env.eval_ok("runtime.tagHandlers.title_bg !== void"),
        TjsValue::Integer(1),
        "the handler is in the runtime's table"
    );
}
