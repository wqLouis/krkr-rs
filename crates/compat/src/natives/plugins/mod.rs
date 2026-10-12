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
/// [`BuiltinPlugin`](crate::BuiltinPlugin) per entry; [`register`] runs once
/// at startup (via [`crate::install`]) and `Plugins.link` then resolves each
/// name to `Builtin` without registering a second time. A name that is not
/// in this table installs the null plugin object (doc 08 §4.2) — but only
/// where the plugin's stem does not already name a global.
///
/// `aliases` are additional DLL names whose classes the same `register`
/// installs (`layerExDraw.dll`, `layerExMovie.dll` and `perspective.dll` are
/// one `layerEx` port, and `KAGParser.dll`/`KAGParserEx.dll` are one
/// `KAGParser` port), so linking several of them registers once. Names and
/// aliases match the reference `NCB_MODULE_NAME` strings under
/// `reference/cpp/plugins` (plus the CMake source list); `extrans.dll` and
/// `wuvorbis.dll` have no sources in that tree — their link-time surface was
/// verified against the shipped binaries (`docs/plugin_host_exe.md`,
/// `docs/wine_box64.md`).
///
/// An entry whose reference registers no TJS surface — or shares it with a
/// sibling port — carries a documented no-op [`BuiltinPluginDef::register`]
/// (`emote_player`, `extrans`, `wuvorbis`, `var_file`, `get_about`; see each
/// module's documentation for the reason).
pub const BUILTIN_PLUGINS: &[BuiltinPluginDef] = &[
    BuiltinPluginDef {
        name: "addFont.dll",
        aliases: &[],
        register: add_font::register,
    },
    BuiltinPluginDef {
        name: "csvParser.dll",
        aliases: &[],
        register: csv_parser::register,
    },
    BuiltinPluginDef {
        name: "dirlist.dll",
        aliases: &[],
        register: dirlist::register,
    },
    BuiltinPluginDef {
        name: "emoteplayer.dll",
        aliases: &[],
        register: emote_player::register,
    },
    BuiltinPluginDef {
        name: "extrans.dll",
        aliases: &[],
        register: extrans::register,
    },
    BuiltinPluginDef {
        name: "fftgraph.dll",
        aliases: &[],
        register: fft_graph::register,
    },
    BuiltinPluginDef {
        name: "fstat.dll",
        aliases: &[],
        register: fstat::register,
    },
    BuiltinPluginDef {
        name: "getabout.dll",
        aliases: &[],
        register: get_about::register,
    },
    BuiltinPluginDef {
        name: "getSample.dll",
        aliases: &[],
        register: get_sample::register,
    },
    BuiltinPluginDef {
        name: "json.dll",
        aliases: &[],
        register: json::register,
    },
    BuiltinPluginDef {
        name: "KAGParserEx.dll",
        aliases: &["KAGParser.dll"],
        register: crate::natives::kag::register,
    },
    BuiltinPluginDef {
        name: "layerExDraw.dll",
        aliases: &["layerExMovie.dll", "perspective.dll"],
        register: layer_ex::register,
    },
    BuiltinPluginDef {
        name: "motionplayer.dll",
        aliases: &[],
        register: motion_player::register,
    },
    BuiltinPluginDef {
        name: "psbfile.dll",
        aliases: &[],
        register: psb_file::register,
    },
    BuiltinPluginDef {
        name: "psd.dll",
        aliases: &[],
        register: psd_file::register,
    },
    BuiltinPluginDef {
        name: "saveStruct.dll",
        aliases: &[],
        register: save_struct::register,
    },
    BuiltinPluginDef {
        name: "ScriptsEx.dll",
        aliases: &[],
        register: scripts_ex::register,
    },
    BuiltinPluginDef {
        name: "krkrsteam.dll",
        aliases: &["steam_api.dll", "DrawDeviceForSteam.dll"],
        register: steam::register,
    },
    BuiltinPluginDef {
        name: "TextRender.dll",
        aliases: &[],
        register: text_render::register,
    },
    BuiltinPluginDef {
        name: "varfile.dll",
        aliases: &[],
        register: var_file::register,
    },
    BuiltinPluginDef {
        name: "win32dialog.dll",
        aliases: &[],
        register: win32_dialog::register,
    },
    BuiltinPluginDef {
        name: "windowEx.dll",
        aliases: &[],
        register: window_ex::register,
    },
    BuiltinPluginDef {
        name: "wutcwf.dll",
        aliases: &[],
        register: wutcwf::register,
    },
    BuiltinPluginDef {
        name: "wuvorbis.dll",
        aliases: &[],
        register: wuvorbis::register,
    },
    BuiltinPluginDef {
        name: "xp3filter.dll",
        aliases: &[],
        register: xp3_filter::register,
    },
];

/// Register every ported plugin class on the VM, unconditionally.
///
/// The host calls this once at startup from [`crate::install`]; the names
/// are then pre-marked in [`PluginRegistry::builtin`](crate::PluginRegistry::builtin)
/// so `Plugins.link` resolves them to `Builtin` without registering again
/// (see [`BUILTIN_PLUGINS`]).
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    for def in BUILTIN_PLUGINS {
        (def.register)(ctx)?;
    }
    Ok(())
}
