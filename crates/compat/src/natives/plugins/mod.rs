//! KiriKiri native plugins: one module per reference plugin.
//!
//! OWNED BY THE MAIN AGENT — DO NOT EDIT while porting a plugin. The per-plugin
//! agents own only their own subdirectory. Editing this aggregator (to "shim
//! out" a sibling) silently unwires plugins for everyone and breaks verification.

use crate::plugin::BuiltinPluginDef;
use crate::{CompatCtx, CompatError};

pub mod add_font;
pub mod csv_parser;
pub mod dirlist;
pub mod emote_player;
pub mod extrans;
pub mod fft_graph;
pub mod fstat;
pub mod get_about;
pub mod get_sample;
pub mod json;
pub mod layer_ex;
pub mod motion_player;
pub mod psb_file;
pub mod psd_file;
pub mod save_struct;
pub mod scripts_ex;
pub mod simple_binder;
pub mod steam;
pub mod text_render;
pub mod var_file;
pub mod win32_dialog;
pub mod window_ex;
pub mod wutcwf;
pub mod wuvorbis;
pub mod xp3_filter;

/// Shared plugin helpers (not a plugin): the reference `TVP_md5_*` hex
/// digest behind `fstat`'s and `scripts_ex`' `getMD5HashString`, so the
/// algorithm exists once instead of one hand-rolled copy per plugin.
mod md5;

/// The ported plugins, keyed by the DLL name scripts pass to `Plugins.link`.
///
/// [`PluginRegistry::builtin`](crate::PluginRegistry::builtin) builds one
/// [`BuiltinPlugin`](crate::BuiltinPlugin) per entry. The list of startup
/// modules is [`STARTUP_PLUGINS`]; every other entry registers its native
/// classes on the first `Plugins.link` that names it. A name that is not in
/// this table installs the null plugin object (doc 08 §4.2) — but only where
/// the plugin's stem does not already name a global.
///
/// `aliases` are additional DLL names whose classes the same `register`
/// installs (`layerExDraw.dll`, `layerExMovie.dll` and `perspective.dll` are
/// one `layerEx` port, and `KAGParser.dll`/`KAGParserEx.dll` are one
/// `KAGParser` port), so linking several of them registers once. Names and
/// aliases match the reference `NCB_MODULE_NAME` strings under
/// `reference/cpp/plugins` (plus the CMake source list); `extrans.dll` and
/// `wuvorbis.dll` have no sources in that tree — their link-time surface was
/// verified against the shipped binaries (`docs.old/plugin_host_exe.md`,
/// `docs.old/wine_box64.md`).
///
/// An entry whose reference registers no TJS surface — or shares it with a
/// sibling port — carries a documented no-op [`BuiltinPluginDef::register`]
/// (`emote_player`, `extrans`, `wuvorbis`, `var_file`, `get_about`; see each
/// module's documentation for the reason).
pub const BUILTIN_PLUGINS: &[BuiltinPluginDef] = &[
    BuiltinPluginDef {
        name: "addFont.dll",
        aliases: &[],
        dependencies: &[],
        register: add_font::register,
    },
    BuiltinPluginDef {
        name: "csvParser.dll",
        aliases: &[],
        dependencies: &[],
        register: csv_parser::register,
    },
    BuiltinPluginDef {
        name: "dirlist.dll",
        aliases: &[],
        dependencies: &[],
        register: dirlist::register,
    },
    BuiltinPluginDef {
        name: "emoteplayer.dll",
        aliases: &[],
        dependencies: &["motionplayer.dll"],
        register: emote_player::register,
    },
    BuiltinPluginDef {
        name: "extrans.dll",
        aliases: &[],
        dependencies: &[],
        register: extrans::register,
    },
    BuiltinPluginDef {
        name: "fftgraph.dll",
        aliases: &[],
        dependencies: &[],
        register: fft_graph::register,
    },
    BuiltinPluginDef {
        name: "fstat.dll",
        aliases: &[],
        dependencies: &[],
        register: fstat::register,
    },
    BuiltinPluginDef {
        name: "getabout.dll",
        aliases: &[],
        // The member lives in the windowEx System extension; linking getabout
        // must therefore bring that surface in even when the script did not
        // link windowEx itself.
        dependencies: &["windowEx.dll"],
        register: get_about::register,
    },
    BuiltinPluginDef {
        name: "getSample.dll",
        aliases: &[],
        dependencies: &[],
        register: get_sample::register,
    },
    BuiltinPluginDef {
        name: "json.dll",
        aliases: &[],
        dependencies: &[],
        register: json::register,
    },
    BuiltinPluginDef {
        name: "KAGParserEx.dll",
        aliases: &["KAGParser.dll"],
        dependencies: &[],
        register: crate::natives::kag::register,
    },
    BuiltinPluginDef {
        name: "layerExDraw.dll",
        aliases: &["layerExMovie.dll", "perspective.dll"],
        dependencies: &[],
        register: layer_ex::register,
    },
    BuiltinPluginDef {
        name: "menu.dll",
        aliases: &[],
        dependencies: &[],
        register: crate::natives::menu_item::register_plugin,
    },
    BuiltinPluginDef {
        name: "motionplayer.dll",
        aliases: &[],
        dependencies: &[],
        register: motion_player::register,
    },
    BuiltinPluginDef {
        name: "psbfile.dll",
        aliases: &[],
        dependencies: &[],
        register: psb_file::register,
    },
    BuiltinPluginDef {
        name: "psd.dll",
        aliases: &[],
        dependencies: &[],
        register: psd_file::register,
    },
    BuiltinPluginDef {
        name: "saveStruct.dll",
        aliases: &[],
        dependencies: &[],
        register: save_struct::register,
    },
    BuiltinPluginDef {
        name: "ScriptsEx.dll",
        aliases: &[],
        dependencies: &[],
        register: scripts_ex::register,
    },
    BuiltinPluginDef {
        name: "krkrsteam.dll",
        aliases: &["steam_api.dll", "DrawDeviceForSteam.dll"],
        dependencies: &[],
        register: steam::register,
    },
    BuiltinPluginDef {
        name: "TextRender.dll",
        aliases: &[],
        dependencies: &[],
        register: text_render::register,
    },
    BuiltinPluginDef {
        name: "varfile.dll",
        aliases: &[],
        dependencies: &[],
        register: var_file::register,
    },
    BuiltinPluginDef {
        name: "win32dialog.dll",
        aliases: &[],
        dependencies: &[],
        register: win32_dialog::register,
    },
    BuiltinPluginDef {
        name: "windowEx.dll",
        aliases: &[],
        dependencies: &[],
        register: window_ex::register,
    },
    BuiltinPluginDef {
        name: "wutcwf.dll",
        aliases: &[],
        dependencies: &[],
        register: wutcwf::register,
    },
    BuiltinPluginDef {
        name: "wuvorbis.dll",
        aliases: &[],
        dependencies: &[],
        register: wuvorbis::register,
    },
    BuiltinPluginDef {
        name: "xp3filter.dll",
        aliases: &[],
        dependencies: &[],
        register: xp3_filter::register,
    },
];

/// Built-ins that are available before any game script runs.
///
/// Only `xp3filter.dll` is loaded during startup; every other built-in
/// module becomes visible when a script links it. The timing is
/// script-observable: the shipped `system/voice.tjs` probes
/// `typeof WaveSoundBuffer.setDefaultAheads` before it links `getSample.dll`
/// and only then publishes `WaveSoundBuffer.enableGetSample`. Registering
/// that surface at startup makes the probe false and leaves the flag unset,
/// so the script's `onFlip` timer action raises `Member "enableGetSample"
/// does not exist` on every fire.
pub const STARTUP_PLUGINS: &[&str] = &["xp3filter.dll"];

/// Whether `name` (a canonical table name or an alias) is in
/// [`STARTUP_PLUGINS`]. Names are compared like `Plugins.link` compares them:
/// base name only, ASCII case-insensitive.
pub fn registers_at_startup(name: &str) -> bool {
    let normalized = crate::plugin::normalize_plugin_name(name);
    STARTUP_PLUGINS
        .iter()
        .any(|entry| crate::plugin::normalize_plugin_name(entry) == normalized)
}

/// Register the built-in surfaces that are available before any script runs.
///
/// The host calls this once at startup from [`crate::install`]. Only
/// [`STARTUP_PLUGINS`] are registered here and pre-marked in
/// [`PluginRegistry::builtin`](crate::PluginRegistry::builtin); every other
/// built-in plugin registers its members on the first `Plugins.link` (see
/// [`BUILTIN_PLUGINS`]).
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    for def in BUILTIN_PLUGINS
        .iter()
        .filter(|def| registers_at_startup(def.name))
    {
        (def.register)(ctx)?;
    }
    Ok(())
}

/// Completes the deferred, service-dependent half of plugin registration.
///
/// A plugin can only read an L2 service at registration when that service is
/// already attached. The composition root attaches the storage after
/// [`CompatHost::install`](crate::CompatHost::install), so
/// [`CompatHost::activate_deferred_plugins`](crate::CompatHost::activate_deferred_plugins)
/// completes the read and the seam install before the startup scripts run,
/// returning a read failure to the caller. Idempotent.
pub fn activate_deferred(
    engine: &tjs::Tjs2Engine,
    services: &crate::CompatServices,
) -> Result<(), CompatError> {
    xp3_filter::activate(engine, services)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_startup_plugin_is_a_ported_table_entry() {
        for name in STARTUP_PLUGINS {
            assert!(
                BUILTIN_PLUGINS.iter().any(|def| def.name == *name),
                "{name} must be a BUILTIN_PLUGINS entry"
            );
            assert!(registers_at_startup(name));
        }
    }

    #[test]
    fn only_xp3filter_has_a_reference_startup_registration() {
        // `TVPLoadInternalPlugins` loads exactly one module at startup
        // (`reference/cpp/core/plugin/PluginImpl.cpp:90-91`); every other
        // plugin's `NCB_ATTACH_*` members appear from `V2Link`.
        assert_eq!(STARTUP_PLUGINS.len(), 1);
        assert_eq!(STARTUP_PLUGINS[0], "xp3filter.dll");
        assert!(!registers_at_startup("getSample.dll"));
        assert!(!registers_at_startup("layerExDraw.dll"));
        assert!(registers_at_startup(r"C:\game\plugin\Xp3Filter.DLL"));
    }
}
