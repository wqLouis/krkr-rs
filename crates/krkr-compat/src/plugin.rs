//! The native-plugin architecture (doc 08 §4): the [`NativePlugin`] trait
//! and the [`PluginRegistry`] that activates built-ins by name.

use std::collections::BTreeSet;
use std::sync::Mutex;

use crate::CompatCtx;
use crate::CompatError;
use crate::null_plugin;

/// A statically built-in native plugin emulation.
///
/// A plugin is a named unit (the name scripts pass to `Plugins.link`, e.g.
/// `"menu.dll"`); registration happens at link time and the optional
/// [`Self::poll`] hook runs once per frame.
pub trait NativePlugin {
    /// The name scripts use: `"menu.dll"`, `"windowEx.dll"`, ...
    fn name(&self) -> &str;

    /// Register the plugin's native classes on the VM.
    fn register(&self, ctx: &mut CompatCtx) -> Result<(), CompatError>;

    /// Optional per-frame hook (async triggers, video overlay, timers).
    fn poll(&self, _ctx: &mut CompatCtx) -> Result<(), CompatError> {
        Ok(())
    }
}

/// The result of activating a plugin name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginOutcome {
    /// A built-in emulation was registered.
    Builtin,
    /// The plugin is not emulated; the null plugin object was installed.
    Null,
}

/// What a linked name resolves to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LinkedKind {
    Builtin,
    Null,
}

/// One entry of the `Plugins.getList` list.
#[derive(Clone, Debug)]
struct LinkedEntry {
    /// The name the script passed, case preserved (`getList` and the null
    /// global both use it).
    name: String,
    /// The normalized base name used for built-in lookup.
    key: String,
    kind: LinkedKind,
}

/// Holds the built-in plugins and activates them by name.
///
/// `link` is idempotent: a name that was already activated is not registered
/// twice (the C++ side rejects duplicate class names). A name with no
/// built-in emulation installs the [null plugin object](crate::null_plugin)
/// instead — the single documented exception to D11, and it is not silent
/// (every member access logs a warning).
pub struct PluginRegistry {
    plugins: Vec<Box<dyn NativePlugin>>,
    /// The linked names in insertion order, so `Plugins.getList` preserves
    /// both the script's order and the original casing.
    linked: Mutex<Vec<LinkedEntry>>,
    /// Built-ins whose native classes are already on the VM. The ABI cannot
    /// unregister a class, so this survives `unlink`: a later `link` of the
    /// same built-in skips `register` instead of hitting the duplicate-class
    /// rejection.
    registered: Mutex<BTreeSet<String>>,
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginRegistry {
    /// An empty registry (no built-in emulations yet).
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
            linked: Mutex::new(Vec::new()),
            registered: Mutex::new(BTreeSet::new()),
        }
    }

    /// The built-in registry assembled at startup.
    ///
    /// The first wave has no migrated `tvp-*` plugins yet; the migrated
    /// families are appended here as they land (doc 08 §7).
    pub fn builtin() -> Self {
        Self::new()
    }

    /// Add a built-in plugin to the registry.
    pub fn register(&mut self, plugin: Box<dyn NativePlugin>) {
        self.plugins.push(plugin);
    }

    /// Look up a built-in plugin by its script-visible name.
    pub fn plugin(&self, name: &str) -> Option<&dyn NativePlugin> {
        let key = normalize_plugin_name(name);
        self.plugins
            .iter()
            .map(|plugin| plugin.as_ref())
            .find(|plugin| normalize_plugin_name(plugin.name()) == key)
    }

    /// Whether `name` has a built-in emulation.
    pub fn is_builtin(&self, name: &str) -> bool {
        self.plugin(name).is_some()
    }

    /// The names that have been linked so far, in insertion order and with
    /// the script's original casing (`Plugins.getList` order).
    pub fn linked(&self) -> Vec<String> {
        self.linked
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .iter()
            .map(|entry| entry.name.clone())
            .collect()
    }

    /// Activate a plugin name: register the built-in emulation, or install
    /// the null plugin object when none exists.
    ///
    /// The null hook class must already be registered (the host does this in
    /// [`crate::install`]).
    pub fn activate(&self, name: &str, ctx: &mut CompatCtx) -> Result<PluginOutcome, CompatError> {
        let key = normalize_plugin_name(name);
        if let Some(kind) = self.linked_kind(&key) {
            return Ok(match kind {
                LinkedKind::Builtin => PluginOutcome::Builtin,
                LinkedKind::Null => PluginOutcome::Null,
            });
        }
        if let Some(plugin) = self.plugin(&key) {
            {
                let mut registered = self
                    .registered
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                if !registered.contains(&key) {
                    plugin.register(ctx)?;
                    registered.insert(key.clone());
                }
            }
            self.push_linked(name, &key, LinkedKind::Builtin);
            Ok(PluginOutcome::Builtin)
        } else {
            // Install the null object under the script's original name (the
            // stem is derived inside `null_plugin::install`).
            null_plugin::install(ctx, name)?;
            self.push_linked(name, &key, LinkedKind::Null);
            Ok(PluginOutcome::Null)
        }
    }

    /// Unlink a name and report whether it had been linked.
    ///
    /// Built-in classes stay registered on the VM (the ABI cannot remove
    /// them); only the `getList` entry is dropped, and a later `link` is
    /// idempotent. A null plugin's script global is cleared best-effort.
    pub fn unlink(&self, name: &str, ctx: &mut CompatCtx) -> bool {
        let key = normalize_plugin_name(name);
        let mut linked = self
            .linked
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Some(pos) = linked.iter().position(|entry| entry.key == key) else {
            return false;
        };
        let entry = linked.remove(pos);
        drop(linked);
        if entry.kind == LinkedKind::Null {
            // Best-effort: a failure only leaves the harmless global behind.
            let _ = null_plugin::remove(ctx, &entry.name);
        }
        true
    }

    /// Run every built-in plugin's per-frame hook.
    pub fn poll(&self, ctx: &mut CompatCtx) -> Result<(), CompatError> {
        for plugin in &self.plugins {
            plugin.poll(ctx)?;
        }
        Ok(())
    }

    fn linked_kind(&self, key: &str) -> Option<LinkedKind> {
        self.linked
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| entry.kind)
    }

    fn push_linked(&self, name: &str, key: &str, kind: LinkedKind) {
        self.linked
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(LinkedEntry {
                name: name.to_string(),
                key: key.to_string(),
                kind,
            });
    }
}

/// Normalize a plugin name to its base file name, lowercased, so
/// `C:\game\plugin\extrans.dll`, `extrans.tpm` and `Extrans.DLL` compare
/// equal.
pub fn normalize_plugin_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\', '>']).next().unwrap_or(name);
    base.trim().to_ascii_lowercase()
}

/// The base name of a plugin with its extension removed, preserving case.
///
/// Used as the script-global name the null plugin object is installed under
/// (`windowEx.dll` → `windowEx`).
pub fn plugin_stem(name: &str) -> String {
    let base = name.rsplit(['/', '\\', '>']).next().unwrap_or(name);
    match base.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_string(),
        _ => base.to_string(),
    }
}
