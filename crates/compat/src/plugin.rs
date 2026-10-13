//! The native-plugin architecture: the [`NativePlugin`] trait
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

    /// Alternate DLL names that resolve to this plugin (for example
    /// `layerExDraw.dll` and `perspective.dll` both map to the `layerEx`
    /// port). Default: none.
    fn aliases(&self) -> &[&str] {
        &[]
    }

    /// Register the plugin's native classes on the VM.
    fn register(&self, ctx: &mut CompatCtx) -> Result<(), CompatError>;

    /// Optional per-frame hook (async triggers, video overlay, timers).
    fn poll(&self, _ctx: &mut CompatCtx) -> Result<(), CompatError> {
        Ok(())
    }
}

/// One entry of the built-in plugin table
/// ([`crate::natives::plugins::BUILTIN_PLUGINS`]).
///
/// `aliases` are additional DLL names that resolve to the same `register`,
/// so the classes are registered once even when a game links several of them.
pub struct BuiltinPluginDef {
    /// The canonical DLL name (`"layerExDraw.dll"`).
    pub name: &'static str,
    /// Alternate DLL names resolving to the same `register`.
    pub aliases: &'static [&'static str],
    /// Registers the plugin's native classes — once at startup for the
    /// built-ins (the host calls [`crate::natives::plugins::register`], and
    /// [`PluginRegistry::builtin`] pre-marks the names), or on first link
    /// for a registry assembled by hand.
    pub register: fn(&mut CompatCtx) -> Result<(), CompatError>,
}

/// A built-in plugin whose classes are registered by a plain function.
///
/// The registry is assembled from the table, so adding a ported plugin is one
/// entry rather than a trait impl.
pub struct BuiltinPlugin {
    name: &'static str,
    aliases: &'static [&'static str],
    register: fn(&mut CompatCtx) -> Result<(), CompatError>,
}

impl BuiltinPlugin {
    /// Creates a built-in plugin entry from a [`BuiltinPluginDef`].
    pub const fn new(def: &BuiltinPluginDef) -> Self {
        Self {
            name: def.name,
            aliases: def.aliases,
            register: def.register,
        }
    }
}

impl NativePlugin for BuiltinPlugin {
    fn name(&self) -> &str {
        self.name
    }

    fn aliases(&self) -> &[&str] {
        self.aliases
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
    /// The plugin is not emulated; the null-plugin path was taken. The null
    /// object becomes `global[stem]` only when that name is not already
    /// bound — an existing binding is kept, never clobbered (see
    /// [`crate::null_plugin`]).
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
    /// For a null link: whether *this* link installed `global[stem]`. An
    /// existing global is never clobbered, so a link whose stem was already
    /// taken must not clear it on `unlink` — only the owner does.
    installed_global: bool,
}

/// Holds the built-in plugins and activates them by name.
///
/// `link` is idempotent: a name that was already activated is not registered
/// twice (the C++ side rejects duplicate class names). A name with no
/// built-in emulation installs the [null plugin object](crate::null_plugin)
/// instead — the single documented exception, and it is not silent
/// (every member access logs a warning). The null global is installed only
/// where the stem is free: a `global[stem]` that already holds a native
/// class or script value is kept, and `unlink` then leaves it alone.
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
    /// Every ported plugin (the table in
    /// [`crate::natives::plugins::BUILTIN_PLUGINS`]) is added here, and
    /// their classes are already on the VM (the host registered them in
    /// [`crate::install`]), so a later `Plugins.link` resolves the name to
    /// [`PluginOutcome::Builtin`] without registering anything a second
    /// time. A name that is not in the table falls back to the null plugin
    /// object — installed only where the stem is free.
    pub fn builtin() -> Self {
        let mut registry = Self::new();
        for def in crate::natives::plugins::BUILTIN_PLUGINS {
            registry.register(Box::new(BuiltinPlugin::new(def)));
        }
        // The classes are installed once at startup
        // (`natives::plugins::register`), so mark the built-ins registered: a
        // later `Plugins.link` reports `Builtin` without registering them a
        // second time (the VM rejects duplicate class names).
        registry.registered = Mutex::new(
            crate::natives::plugins::BUILTIN_PLUGINS
                .iter()
                .map(|def| normalize_plugin_name(def.name))
                .collect(),
        );
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

    /// Activate a plugin name: register the built-in emulation, or install
    /// the null plugin object when none exists — never over a `global[stem]`
    /// that is already bound (that binding wins and is preserved on
    /// [`Self::unlink`]; see [`crate::null_plugin`]).
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
            // Track the canonical name so aliases sharing one `register`
            // (e.g. `layerExDraw.dll` / `perspective.dll`) register once.
            let canonical = normalize_plugin_name(plugin.name());
            {
                let mut registered = self
                    .registered
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                if !registered.contains(&canonical) {
                    plugin.register(ctx)?;
                    registered.insert(canonical);
                }
            }
            self.push_linked(name, &key, LinkedKind::Builtin, false);
            Ok(PluginOutcome::Builtin)
        } else {
            // Install the null object under the script's original name (the
            // stem is derived inside `null_plugin::install`). A global that
            // already exists is kept, not replaced; record whether this link
            // owns the binding so `unlink` only clears what it installed.
            let installed_global = null_plugin::install(ctx, name)?;
            self.push_linked(name, &key, LinkedKind::Null, installed_global);
            Ok(PluginOutcome::Null)
        }
    }

    /// Unlink a name and report whether it had been linked.
    ///
    /// Built-in classes stay registered on the VM (the ABI cannot remove
    /// them); only the `getList` entry is dropped, and a later `link` is
    /// idempotent. A null plugin's script global is cleared best-effort —
    /// and only when this link installed it and it still holds the null
    /// object (a binding the link kept, or one a script wrote over, is
    /// preserved).
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
        if entry.kind == LinkedKind::Null && entry.installed_global {
            // Best-effort: a failure only leaves the harmless global behind.
            // A link that kept a pre-existing binding installed no global,
            // so it must not delete one here.
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
    /// activates its built-in emulation, an unported name gets the null-plugin
    /// object and one warning (the reference tries to load the DLL and creates
    /// no global when it fails).
    pub fn autoload<I, S>(&self, names: I, ctx: &mut CompatCtx) -> Result<(), CompatError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        // One null-plugin warning per distinct unported name, even when the
        // same file is found in more than one scanned directory.
        let mut warned: BTreeSet<String> = BTreeSet::new();
        for name in names {
            let name = name.as_ref();
            log::info!("(info) Loading {name}");
            let dll = tpm_to_dll_name(name);
            if self.activate(&dll, ctx)? == PluginOutcome::Null && warned.insert(dll) {
                log::warn!("Plugins auto-load {name:?}: no built-in emulation; null plugin path");
            }
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

    fn push_linked(&self, name: &str, key: &str, kind: LinkedKind, installed_global: bool) {
        self.linked
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(LinkedEntry {
                name: name.to_string(),
                key: key.to_string(),
                kind,
                installed_global,
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
