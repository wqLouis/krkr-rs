//! The native-plugin architecture: the [`NativePlugin`] trait
//! and the [`PluginRegistry`] that activates built-ins by name.

use std::collections::BTreeSet;
use std::sync::Mutex;

use crate::CompatCtx;
use crate::CompatError;

/// A statically built-in native plugin emulation.
///
/// A plugin is a named unit (the name scripts pass to `Plugins.link`, e.g.
/// `"menu.dll"`); registration happens at link time and the optional
/// [`Self::poll`] hook runs once per frame.
pub trait NativePlugin {
    /// The name scripts use: `"menu.dll"`, `"windowEx.dll"`, ...
    fn name(&self) -> &str;

    /// Alternate DLL names that resolve to this plugin (for example
    /// `layerExDraw.dll` and `perspective.dll` both map to the `layerEx`
    /// port). Default: none.
    fn aliases(&self) -> &[&str] {
        &[]
    }

    /// Modules this plugin loads when it activates (the pre-registration
    /// dependency, e.g. `emoteplayer.dll` → `motionplayer.dll`). The registry
    /// registers each dependency first and does not report it in
    /// `Plugins.getList`. Default: none.
    fn dependencies(&self) -> &[&str] {
        &[]
    }

    /// Register the plugin's native classes on the VM.
    ///
    /// # Errors
    ///
    /// Returns [`CompatError::Register`] when a class or a member of the
    /// plugin cannot be registered on the VM.
    fn register(&self, ctx: &mut CompatCtx) -> Result<(), CompatError>;

    /// Optional per-frame hook (async triggers, video overlay, timers).
    ///
    /// # Errors
    ///
    /// Returns [`CompatError::Engine`] when the hook cannot advance its
    /// subsystem; the frame body stops at the first failing poll.
    fn poll(&self, _ctx: &mut CompatCtx) -> Result<(), CompatError> {
        Ok(())
    }
}

/// One entry of the built-in plugin table
/// ([`crate::natives::plugins::BUILTIN_PLUGINS`]).
///
/// `aliases` are additional DLL names that resolve to the same `register`,
/// so the classes are registered once even when a game links several of them.
///
/// The entry *is* the [`NativePlugin`] implementation: adding a plugin to the
/// table is one row rather than a trait impl.
#[derive(Clone, Copy)]
pub struct BuiltinPluginDef {
    /// The canonical DLL name (`"layerExDraw.dll"`).
    pub name: &'static str,
    /// Alternate DLL names resolving to the same `register`.
    pub aliases: &'static [&'static str],
    /// Prerequisite modules, registered first when this plugin activates;
    /// they are not recorded as links.
    pub dependencies: &'static [&'static str],
    /// Registers the plugin's native classes when `Plugins.link` activates
    /// it, except for the [`crate::natives::plugins::STARTUP_PLUGINS`] the
    /// host registers once at startup (see [`crate::natives::plugins::register`]).
    pub register: fn(&mut CompatCtx) -> Result<(), CompatError>,
}

impl NativePlugin for BuiltinPluginDef {
    fn name(&self) -> &str {
        self.name
    }

    fn aliases(&self) -> &[&str] {
        self.aliases
    }

    fn dependencies(&self) -> &[&str] {
        self.dependencies
    }

    fn register(&self, ctx: &mut CompatCtx) -> Result<(), CompatError> {
        (self.register)(ctx)
    }
}

/// The result of activating a plugin name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginOutcome {
    /// A built-in emulation was registered.
    Builtin,
    /// The plugin has no emulation: the link is recorded and one warning
    /// names the plugin, and nothing is registered for it — an absent plugin
    /// reads as absent.
    Unported,
}

/// What a linked name resolves to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LinkedKind {
    Builtin,
    Unported,
}

/// One entry of the `Plugins.getList` list.
#[derive(Clone, Debug)]
struct LinkedEntry {
    /// The name the script passed, case preserved (`getList` uses it).
    name: String,
    /// The normalized base name used for built-in lookup.
    key: String,
    kind: LinkedKind,
}

/// Holds the built-in plugins and activates them by name.
///
/// `link` is idempotent: a name that was already activated is not registered
/// twice (the C++ side rejects duplicate class names). A name with no
/// built-in emulation is recorded and warned about, and nothing is
/// registered for it: the absent plugin reads as absent.
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
    ///
    /// The startup modules are marked registered because
    /// The host's install always puts their surface on the VM before any
    /// script runs, whichever registry the host was built with.
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
            linked: Mutex::new(Vec::new()),
            registered: Mutex::new(
                crate::natives::plugins::STARTUP_PLUGINS
                    .iter()
                    .map(|name| normalize_plugin_name(name))
                    .collect(),
            ),
        }
    }

    /// The built-in registry assembled at startup.
    ///
    /// Every plugin in the built-in table is added here. The module
    /// available before any script runs (today `xp3filter.dll`) is marked
    /// registered by [`Self::new`]; a later `Plugins.link` of any
    /// other built-in activates it and registers its classes. A name that is
    /// not in the table is recorded and warned about, and nothing is
    /// registered for it.
    pub fn builtin() -> Self {
        let mut registry = Self::new();
        for def in crate::natives::plugins::BUILTIN_PLUGINS {
            registry.register(Box::new(*def));
        }
        registry
    }

    /// Add a built-in plugin to the registry.
    pub fn register(&mut self, plugin: Box<dyn NativePlugin>) {
        self.plugins.push(plugin);
    }

    /// Look up a built-in plugin by its script-visible name (canonical name
    /// or any alias).
    pub fn plugin(&self, name: &str) -> Option<&dyn NativePlugin> {
        let key = normalize_plugin_name(name);
        self.plugins
            .iter()
            .map(|plugin| plugin.as_ref())
            .find(|plugin| {
                normalize_plugin_name(plugin.name()) == key
                    || plugin
                        .aliases()
                        .iter()
                        .any(|alias| normalize_plugin_name(alias) == key)
            })
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

    /// Activate a plugin name: register the built-in emulation, or record the
    /// link and warn when the name has no emulation.
    ///
    /// An unported name registers nothing: no global under the plugin's stem
    /// and no object that swallows member accesses. The link is recorded, so
    /// `Plugins.getList` reports it, and one warning names the plugin.
    ///
    /// # Errors
    ///
    /// Returns [`CompatError::Register`] when the plugin's own registration
    /// fails.
    pub fn activate(&self, name: &str, ctx: &mut CompatCtx) -> Result<PluginOutcome, CompatError> {
        let key = normalize_plugin_name(name);
        if let Some(kind) = self.linked_kind(&key) {
            return Ok(match kind {
                LinkedKind::Builtin => PluginOutcome::Builtin,
                LinkedKind::Unported => PluginOutcome::Unported,
            });
        }
        if let Some(plugin) = self.plugin(&key) {
            // Track the canonical name so aliases sharing one `register`
            // (e.g. `layerExDraw.dll` / `perspective.dll`) register once.
            self.ensure_registered(plugin, ctx)?;
            self.push_linked(name, &key, LinkedKind::Builtin);
            Ok(PluginOutcome::Builtin)
        } else {
            log::warn!("plugin {name:?}: no built-in emulation; the plugin is not available");
            self.push_linked(name, &key, LinkedKind::Unported);
            Ok(PluginOutcome::Unported)
        }
    }

    /// Unlink a name and report whether it had been linked.
    ///
    /// Built-in classes stay registered on the VM (the ABI cannot remove
    /// them); only the `getList` entry is dropped, and a later `link` is
    /// idempotent. An unported link owns no global, so there is nothing to
    /// clear.
    pub fn unlink(&self, name: &str) -> bool {
        let key = normalize_plugin_name(name);
        let mut linked = self
            .linked
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Some(pos) = linked.iter().position(|entry| entry.key == key) else {
            return false;
        };
        linked.remove(pos);
        true
    }

    /// Run the per-frame hook of every built-in plugin that has been
    /// activated.
    ///
    /// A plugin that no script has linked has no script-visible surface and
    /// owns no per-frame hook, so its `poll` must not run: the `registered`
    /// set is the same one `activate` fills on the first link.
    ///
    /// # Errors
    ///
    /// Returns the first error a plugin's `poll` reports; the remaining
    /// plugins do not run after it.
    pub fn poll(&self, ctx: &mut CompatCtx) -> Result<(), CompatError> {
        let registered = self
            .registered
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        for plugin in &self.plugins {
            if registered.contains(&normalize_plugin_name(plugin.name())) {
                plugin.poll(ctx)?;
            }
        }
        Ok(())
    }

    /// Auto-load the `.tpm` plugins the composition root found, in order.
    ///
    /// The reference `tvpLoadPlugins` scans the exe directory,
    /// `<exe>/system` and `<exe>/plugin` for `*.tpm`, sorts them by file name
    /// and loads each in turn (`reference/cpp/core/plugin/PluginImpl.cpp:148-177`,
    /// called from `reference/cpp/core/environ/Application.cpp:342`). The scan
    /// itself is the composition root's job (compat owns no file-system
    /// access); this receives the ordered file names and links each. A
    /// trailing `.tpm` becomes `.dll` first, matching
    /// `TVPLoadInternalPlugin` (`PluginImpl.cpp:113-133`), and each is logged
    /// with the reference's `(info) Loading <name>` line (`:173-174`).
    ///
    /// Linking goes through the same path as `Plugins.link`: a ported name
    /// activates its built-in emulation, an unported name is recorded and
    /// warned about, and nothing is registered for it (a DLL that cannot be
    /// loaded creates no global).
    ///
    /// # Errors
    ///
    /// Returns [`CompatError::Register`] when a named plugin cannot be
    /// registered. The names already linked stay linked.
    pub fn autoload<I, S>(&self, names: I, ctx: &mut CompatCtx) -> Result<(), CompatError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for name in names {
            let name = name.as_ref();
            log::info!("(info) Loading {name}");
            let dll = tpm_to_dll_name(name);
            self.activate(&dll, ctx)?;
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

    /// Register `plugin` and its dependencies once, without recording a link.
    ///
    /// `Plugins.getList` reports only the names a script linked, so a
    /// dependency loaded on the plugin's behalf (its pre-registration hook)
    /// is registered but not listed. The `registered` set keeps a second load
    /// of the same module (an explicit link or another dependent) from
    /// registering its classes twice.
    fn ensure_registered(
        &self,
        plugin: &dyn NativePlugin,
        ctx: &mut CompatCtx,
    ) -> Result<(), CompatError> {
        let canonical = normalize_plugin_name(plugin.name());
        if self
            .registered
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .contains(&canonical)
        {
            return Ok(());
        }
        for dependency in plugin.dependencies() {
            match self.plugin(dependency) {
                Some(dependency) => self.ensure_registered(dependency, ctx)?,
                None => log::debug!(
                    "plugin {}: dependency {dependency:?} has no built-in emulation",
                    plugin.name()
                ),
            }
        }
        plugin.register(ctx)?;
        self.registered
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(canonical);
        Ok(())
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

/// Replaces a trailing `.tpm` with `.dll` (case-insensitive), preserving the
/// stem's original casing.
///
/// The reference does the same before loading an internal plugin
/// (`reference/cpp/core/plugin/PluginImpl.cpp:113-133`), so an auto-loaded
/// `foo.tpm` resolves the same built-in as `Plugins.link("foo.dll")`. A name
/// without the extension is returned unchanged.
fn tpm_to_dll_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let Some(stem) = lower.strip_suffix(".tpm") else {
        return name.to_string();
    };
    // `to_ascii_lowercase` preserves byte length, so the stem slice is valid.
    let mut out = name[..stem.len()].to_string();
    out.push_str(".dll");
    out
}
