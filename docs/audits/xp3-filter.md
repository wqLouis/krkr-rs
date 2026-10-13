# XP3 filter path audit

Four Yuzusoft-lineage titles fail to start on Android (千恋万花, 星光咖啡馆,
魔女的夜宴, 宠物志愿). The archive indexes parse, the content is encrypted, and
the device log contains no `xp3filter` line. This audit establishes the
reference's ordering and lookup, compares it with the port, and separates what
the source proves from what only the next device log can decide.

**Conclusion.** The reference reads `xp3filter.tjs` from the application path
at plugin-load time, before archives are mounted, exactly as the port intends.
The read-before-mounting hypothesis is therefore **not** the defect. The real
defect is a composition-root ordering bug: `GameHost::open_with_preload`
installed the compat surface (which registers the plugin) *before* it attached
the L2 storage, so at registration `CompatServices::storage()` was `None` and
`xp3_filter::register` silently skipped both the `xp3filter.tjs` read and the
L2 extraction-filter seam install. The plugin was inert for every game, not
only the four.

The fix is now landed: production attaches scene/storage before
`CompatHost::install` (`VmHost::with_default_services`), so `xp3filter.tjs` is
read during registration; the two silent states are distinct typed outcomes;
and the deferred hook is an explicit, fallible
`CompatHost::activate_deferred_plugins` the composition root calls as a safety
net for an embedder that attaches late (§2.3–2.4).
`.tpm` auto-load, the other reference behavior that makes internal-vs-external
plugin loading well-defined, is also landed (§6).

---

## 1. The reference: ordering and lookup

### 1.1 Where the script is read from

`reference/cpp/plugins/xp3filter.cpp:550-564`:

```cpp
static void PostRegistCallback() {
    ttstr path = TVPGetAppPath() + TJS_W("xp3filter.tjs");
    if(TVPIsExistentStorageNoSearch(path)) {
        iTJSTextReadStream *stream = TVPCreateTextStreamForRead(path, "");
        ...
        stream->Read(sXP3FilterScript, 0);
        ...
        TVPSetXP3ArchiveExtractionFilter(TVPXP3ArchiveExtractionFilterWrapper);
        TVPSetXP3ArchiveContentFilter(TVPXP3ArchiveContentFilterWrapper);
    }
}
NCB_POST_REGIST_CALLBACK(PostRegistCallback);   // :568
```

* `TVPGetAppPath()` is `TVPExtractStoragePath(TVPProjectDir)`
  (`reference/cpp/core/base/impl/StorageImpl.cpp:505-508`) — the project
  directory with its trailing delimiter.
* At this point `TVPProjectDir` has **not** yet been turned into an archive
  storage name: that happens in `TVPBeforeSystemInit`
  (`reference/cpp/core/base/impl/SysInitImpl.cpp:149-165`), which appends the
  archive delimiter if the project path is an archive, and which runs inside
  `TVPSystemInit` (`reference/cpp/core/base/SysInitIntf.cpp:34-56`, call at
  `:48`) — i.e. *after* plugin loading.
* `TVPIsExistentStorageNoSearch` only checks the literal storage name; it does
  **not** search the auto-path list
  (`reference/cpp/core/base/StorageIntf.cpp:766-768`, `:737-757`). On the
  Android launcher the project path is the game folder
  (`reference/cpp/core/environ/android/AndroidUtils.cpp:742-...`), and
  `TVPSetCurrentDirectory` points at the exe folder
  (`reference/cpp/core/base/impl/SysInitImpl.cpp:131-136`), so the read is the
  **loose file `<game folder>/xp3filter.tjs` on disk**.

The reference does not read the filter script out of a mounted archive at this
point; a script that lives only inside an archive is invisible to it too.

### 1.2 When the script is read and executed

The plugin is an internal plugin loaded by `TVPLoadInternalPlugins`
(`reference/cpp/core/plugin/PluginImpl.cpp:90-93`, `ncbAutoRegister::LoadModule(
TJS_W("xp3filter.dll"))`), called by `tvpLoadPlugins`
(`PluginImpl.cpp:148-149`). `tTVPApplication::StartApplication` runs it at
`reference/cpp/core/environ/Application.cpp:342`, before `TVPSystemInit`
(`:343`) and before `TVPInitializeStartupScript` (`:356`) — i.e. before
`startup.tjs`/`patch.tjs` execute and before any archive is mounted.

`PostRegistCallback` only **reads the text**; it stores it in
`sXP3FilterScript` and installs the two C wrappers. The text is **executed**
lazily on the first filter use: `AddXP3Decoder`
(`reference/cpp/plugins/xp3filter.cpp:394-421`) ends with
`decoder->ScriptEngine->ExecScript(sXP3FilterScript)`, and `AddXP3Decoder` is
reached through `FetchXP3Decoder` (`:432-457`), called from the two wrappers.
The script's `Storages.setXP3ArchiveExtractionFilter(...)` /
`setXP3ArchiveContentFilter(...)` calls register into a dedicated sub-engine
(`:408-421`); the game script cannot see the filter's globals.

### 1.3 How the filter is installed and invoked

`TVPSetXP3ArchiveExtractionFilter` / `TVPSetXP3ArchiveContentFilter` store the
wrappers in process globals (`reference/cpp/core/archive/xp3/XP3Archive.cpp:29-40`):

```cpp
tTVPXP3ArchiveExtractionFilter TVPXP3ArchiveExtractionFilter = nullptr;
...
static tTVPXP3ArchiveContentFilter TVPXP3ArchiveContentFilter = nullptr;
```

The engine side invokes them per read:

* content filter, once when an entry's stream is opened:
  `XP3Archive.cpp:551-573` (`CreateStreamByIndex` calls
  `TVPXP3ArchiveContentFilter(item.Name, ArchiveName, item.OrgSize, ...)` and
  reads `[ret, ctx]` from the returned object; `ret == 1` forces the whole
  entry into memory);
* extraction filter, once per decoded segment:
  `XP3Archive.cpp:998-1003` (`TVPXP3ArchiveExtractionFilter(&info, &FilterContext)`),
  with `info` carrying `(offset, buffer, size, file hash, file name)`.

The two wrappers run independently: the extraction wrapper checks only
`decoder->ManagedDecoder.Object`
(`reference/cpp/plugins/xp3filter.cpp:493-530`), the content wrapper only
`_ManagedFilterInited` / `decoder->ManagedFilter.Object` (`:465-491`).

### 1.4 When the file is absent

Silence. `PostRegistCallback` has no `else`: if
`TVPIsExistentStorageNoSearch(path)` is false, no wrapper is installed and no
diagnostic is emitted (`xp3filter.cpp:552-564`). The extraction/content globals
stay `nullptr` and the reader copies stored bytes unchanged. The reference has
no separate "the app path was not yet resolvable" state, because its storage is
process-global from the start; the port must, because the L2 storage is
attached by the composition root (see §2.5).

---

## 2. The port: ordering and divergence

### 2.1 Intended ordering (matches the reference)

`crates/compat/src/natives/plugins/xp3_filter/mod.rs`:

* `register` registers the two `Storages` static members, creates the buffer
  `xor` helper, then calls `activate`. `activate` reads `xp3filter.tjs` and
  installs the L2 seam once the storage is attached. A read failure is
  reported as `CompatError::Register`; `NotFound` is not an error
  (`reference/cpp/plugins/xp3filter.cpp:553-559`).
* Execution of the source is deferred to the first archive read
  (`ensure_script_loaded` / `execute_pending_script`), mirroring the
  reference's `AddXP3Decoder`-on-first-use. The only eager execution is the
  harness branch in `register` when the binding is already live.
* The lookup and the seam install return typed states, never a bare
  `Option`/early `return`: `ScriptLookup::{StorageNotAttached, Absent, Found}`
  and `SeamInstall::{Installed, StorageNotAttached}`. "Storage not attached
  yet" therefore cannot be mistaken for "the script is absent" (AGENTS §4/D11).

So the read happens at plugin registration and the execution on first use,
which is the reference's shape.

### 2.2 The confirmed defect: registration ran before storage attach

`crates/app/src/game.rs` (`GameHost::open_with_preload`) did this:

1. mount the archives (`Storage::mount_archives_with` / `mount_discovered_…`);
2. `let mut vm = VmHost::with_default()?;` — this runs `CompatHost::install`,
   which registers every plugin, including `xp3_filter::register`;
3. only afterwards, attach the scene and storage:
   `vm.compat_mut().services_mut().attach_storage(...)`.

`CompatHost::services_mut` documents the contract as "attach scene/storage
before installing" (`crates/compat/src/host.rs`), and the compat tests follow
it (`crates/compat/tests/xp3_filter.rs` `Env::new` attaches the storage before
`host.install`). The app did not.

Consequences at registration, both confirmed by reading the old code:

* `install_storage_filter` did `let Some(storage) = services.storage() else { return; }`
  — with `None` it returned without setting
  `Storage::xp3_extraction_filter`. The L2 seam was never installed, so even a
  programmatically installed TJS filter could never run.
* `read_filter_script` did `let Some(storage) = services.storage() else { return Ok(None); }`
  — the script was never read, and no message was logged.

This is a silent fallback (the failure mode the project bans), and it is the
only path the app used, so the whole `xp3filter.dll` emulation was inert in
the product while the compat tests passed.

### 2.3 The fix

* `xp3_filter::activate(engine, services)` reads the script (once) and installs
  the seam (once) as soon as the storage is attached, reporting the
  not-yet-attached storage as its own state. `register` calls it for the eager
  case (tests, and any composition that attaches first).
* `read_filter_script` returns `ScriptLookup::StorageNotAttached` /
  `Absent` / `Found`; `install_storage_filter` returns
  `SeamInstall::Installed` / `StorageNotAttached`. The caller must handle all
  variants, so the two conditions can no longer collapse into one `Ok(None)`.
* `natives::plugins::activate_deferred` dispatches deferred activation.
* `CompatHost::activate_deferred_plugins` is the named, fallible step the
  composition root calls; `begin_native_timing` only opens the timing window.
  `GameHost::open_with_preload` calls the activation before `run_startup`,
  which is after `storage.startup_plan()` (existence probes only, no content
  decode) and before the first `exec_storage`. A read failure is returned to
  the caller, which stops the open instead of running unfiltered.
* The activation is idempotent; neither path can double install the seam
  (`seam_installed`) or re-read the script (`script_read`).

### 2.4 The composition-root ordering (fixed)

`GameHost::open_with_preload` now attaches the scene and storage **before** the
compat surface is installed, matching the `CompatHost::services_mut` contract
and what the compat tests (`Env::new`) already do. The `vm` API addition:

```rust
// crates/vm/src/host.rs
pub fn with_default_services(
    attach: impl FnOnce(&mut compat::CompatServices),
) -> Result<Self, VmError> {
    Self::new_with_services(PluginRegistry::builtin(), attach)
}

pub fn new_with_services(
    registry: PluginRegistry,
    attach: impl FnOnce(&mut compat::CompatServices),
) -> Result<Self, VmError> {
    // ... create engine/input/timer ...
    let mut compat = CompatHost::with_registry(registry);
    attach(compat.services_mut());   // before install
    compat.install(&engine)?;
    // ...
}
```

```rust
// crates/app/src/game.rs, GameHost::open_with_preload
let scene_ptr = host.scene_mut() as *mut engine::Scene;
let storage_ptr = storage.as_ref() as *const Storage as *mut Storage;
// SAFETY: ... the process-global binding is the pre-existing reason the
// pointers must outlive the host.
let mut vm = VmHost::with_default_services(|services| unsafe {
    services.attach_scene(scene_ptr);
    services.attach_storage(storage_ptr);
})?;
```

`with_default` and `new` are unchanged for every other caller (Android, the
compat tests, harnesses): both delegate to `new_with_services(registry, |_| {})`,
so nothing that does not opt in can see a service before install. Production now
reaches `xp3_filter::register` with the storage attached, so the registration
time read and the seam install happen there.

The deferred `activate` behind `CompatHost::activate_deferred_plugins` is kept
as an idempotent safety net for embedders that attach late; in production it is
a **no-op** (it finds `script_read` and `seam_installed` already set), and the
only way it is observable is the `debug` "deferred: storage not attached yet"
line, which must not appear on the production path. The step is fallible: a
read failure is a `CompatError::Register` the composition root propagates, so
a late-attach failure is never silently dropped.

The production/test divergence that hid this bug is therefore closed: both the
compat tests and production attach services before `CompatHost::install`.

### 2.5 Remaining, deliberate divergence

The port resolves `xp3filter.tjs` through `Storage::read`, whose order is:
explicit `arc>path`, then the app directory on disk, then auto-paths, then
mounted archives later-first (`crates/engine/src/storage.rs` `find_recorded`).
The reference checks only the literal app-path file with no auto-path search.
So the port can additionally find a filter script **inside a mounted archive**.
That is a superset of the reference and is the behavior the device task asked
for; a loose `<app>/xp3filter.tjs` still wins because disk is checked before
mounts. One caveat: an archive member that is itself encrypted cannot be read
before the filter exists, so a `xp3filter.tjs` stored encrypted is
unreadable here (the reference sidesteps this by reading the loose app-path
file only). A filter script is normally stored raw, because it is the key.

---

## 3. The `Config.tjs` question

`Config.tjs` is a KAG-level file, not an engine special case: core has no
`Config.tjs` handling. A storage lookup for a bare name resolves
(`reference/cpp/core/base/StorageIntf.cpp:1061-1107`, `TVPGetPlacedPath`):

1. the current directory (`TVPIsExistentStorageNoSearchNoNormalize` at
   `:1083-1088`), which `TVPBeforeSystemInit` set to `TVPProjectDir`
   (`reference/cpp/core/base/impl/SysInitImpl.cpp:165`);
2. then the auto-path table of mounted folders/archives (`:1090-1098`);
3. `TVPSearchPlacedPath` raises `TVPCannotFindStorage` on a miss (`:1111-1115`).

The save directory is **not** part of that search. It is
`TVPGetAppPath() + "/savedata/"` (`ApplicationSpecialPath.h:13-17`), used as the
data path and log location (`SysInitImpl.cpp:526`, `:556`, `:560`), and it is
never passed to `TVPAddAutoPath` (the only caller is the `Storages.addAutoPath`
native, `StorageIntf.cpp:1252-1264`). So there is **no** reference-supported
expectation that an arbitrary `Config.tjs` lives in `savedata`, and a missing
file is a hard "cannot find storage" error, not a fallback.

The task verified that `Config.tjs` is absent from the working title
不可视之药 too, yet that title starts; its KAG version simply never asks for it.
So the `Config.tjs が見つかりません` on 魔女的夜宴 / 宠物志愿 is a property of
those titles' KAG version asking for a file their mounted storage set does not
provide. Whether the missing file is in an archive the port did not mount, is
provided by `startup.tjs`, or is genuinely part of the title's data is beyond
what the reference or this tree can decide without the device's archive list
and the storage-miss trace (`Storage::find` logs one `debug` line naming every
location searched).

---

## 4. Ranked candidate causes of the device symptoms

1. **The registration-before-storage-attach defect — CONFIRMED by source.**
   The plugin's seam was never installed and the script never read, so archive
   content stayed encrypted for every title. This alone explains "no `xp3filter`
   line" and undecrypted boot scripts. Fixed in this change; the two silently
   collapsing states now report separately (§2.3), and the production ordering
   is fixed when the `vm` API addition in §2.4 lands.
2. **The filter script is genuinely absent — HYPOTHESIS, decided by the next
   log.** If a title ships `xp3filter.tjs` neither loose beside the data
   archives nor inside any mounted archive, no filter can be installed. The
   new `xp3_filter: xp3filter.tjs not found; archive content is not filtered`
   line confirms this.
3. **The script is found but its execution fails or installs the wrong filter —
   HYPOTHESIS, decided by the next log.** The existing
   `xp3_filter: {error}` warning (execution failure, from
   `ensure_script_loaded`) and the new
   `xp3_filter: xp3filter.tjs loaded (N bytes); archive content is filtered`
   line separate "found and executed" from "found but wrong".
4. **Read-before-mounting — NOT SUPPORTED.** §1 shows the reference also reads
   before mounting (loose app-path at plugin load). The port's read timing
   relative to mounting is not the defect; do not "fix" it.
5. **`Config.tjs` should come from savedata/current-dir — NOT SUPPORTED.** §3
   shows the save directory is not a general storage root and core has no
   `Config.tjs` special case. This needs device evidence, not a code change.

---

## 5. The exact lines the next device run will produce

At `info` (the app's default level), once per process:

```
xp3_filter: xp3filter.tjs loaded (N bytes); archive content is filtered
xp3_filter: xp3filter.tjs not found; archive content is not filtered
```

`FileSink` writes each record as `<rfc3339> <LEVEL> <message>`; the message
text is shown below.

Exactly one of those two appears when the storage is attached before install
(the production composition): `xp3filter.tjs` is read during registration. At
`debug`:

```
xp3_filter: XP3 extraction-filter seam installed
xp3_filter: deferred: storage not attached yet; xp3filter.tjs not looked for
xp3_filter: deferred: storage not attached; extraction-filter seam not installed
script read '<name>' bytes=<len> head32=[<first 32 bytes, lowercase hex, space-separated>] encoding=<decision> nul_bytes=<count> decoded_has_nul=<true|false>
```

The `debug` "deferred: … not looked for" line is deliberately worded so it
cannot be grepped and mistaken for the `info` "not found" line; it means the
storage was not attached yet, not that the script is absent. On the intended
composition (storage attached before install) it must not appear: production
reads the script during registration, and the deferred step
(`CompatHost::activate_deferred_plugins`) is a no-op when it finds
`script_read`/`seam_installed` already set.

The `script read '<name>' bytes=… head32=[…] encoding=<decision> nul_bytes=…
decoded_has_nul=…` line is emitted at `debug` for every storage-backed script
read through the `Scripts` natives (`execStorage`/`evalStorage`/`compileStorage`),
which includes the startup sequence's `startup.tjs`/`patch` scripts
(`compat::exec_storage` routes through the same path). `<decision>` is the
encoding path the decode took, exactly one of `UTF-32LE BOM`, `UTF-32BE BOM`,
`UTF-8 BOM`, `UTF-16LE BOM`, `UTF-16BE BOM`, `FE FE container mode <n>`,
`explicit '<name>'`, `detected <charset>`, `detected WINDOWS-1252->latin1`, or
`decode-error` when the decode failed; `<name>` is the storage name as
requested. The line is emitted before the script is executed, so a startup
script's bytes can be identified from the export even when execution fails.

If neither `info` line appears at all, the activation did not run (check that
`CompatHost::activate_deferred_plugins` is reached) or the log level filters
`info`. An execution failure of the filter script still emits
`xp3_filter: {error}` at `warn`. A failed (non-`NotFound`) `xp3filter.tjs`
read is a `CompatError::Register` from both paths: `CompatHost::install`
returns it at registration, and `CompatHost::activate_deferred_plugins`
returns it on the deferred path, so the composition root stops the open with a
typed error instead of logging it (matching the reference's rethrow at
`xp3filter.cpp:556-558`).

Interpretation:

* `loaded (...)` + still-garbled content → the script ran but does not decrypt
  this title's scheme (candidate 3).
* `not found` → no script in the app path or any mounted archive (candidate 2).
* `deferred: ... not looked for` → the storage was not attached when the
  deferred hook ran; the composition ordering in §2.4 is not in place.
* no line → the activation never ran (re-check the ordering), not a filter
  decision.

---

## 6. Internal plugins and `.tpm` auto-load

Reference: `tvpLoadPlugins` first calls `TVPLoadInternalPlugins`, which loads
`xp3filter.dll` by name (`reference/cpp/core/plugin/PluginImpl.cpp:90-93`; the
`LoadModule(TJS_W("xp3filter.dll"))` at `:92`), and then scans
`exepath`, `exepath/system` and `exepath/plugin` for `*.tpm`, sorts them by
filename, and calls `TVPLoadPlugin` on each (`:148-177`). It runs at
`Application::StartApplication` (`reference/cpp/core/environ/Application.cpp:342`),
before the startup scripts. `xp3filter.dll` being an *internal* plugin (loaded
by name, not discovered as a `.tpm`) is why its load time is fixed relative to
the project path, and therefore why the storage-attach ordering matters at all.

Port status:

* **Internal plugins: already landed.** Every `BUILTIN_PLUGINS` entry,
  including `xp3filter.dll` (`crates/compat/src/natives/plugins/mod.rs`), is
  registered unconditionally by `natives::plugins::register` from
  `crate::install`. There is no separate internal-vs-`.tpm` distinction, but
  the internal plugin is on the VM before any script runs, which is the
  reference behavior that matters here.
* **`.tpm` auto-load: landed.** The scan is the composition root's
  (`FileSystem` owns the file-system access, §2 of AGENTS):
  `discover_tpm_plugins` in `crates/app/src/game.rs` lists the exe directory,
  `<exe>/system` and `<exe>/plugin`, keeps regular files whose name ends in
  `.tpm` (case-insensitive), and sorts the combined list by file name — the
  reference's `TVPSearchPluginsAt`/`tvpLoadPlugins`
  (`reference/cpp/core/plugin/PluginImpl.cpp:76-88`, `:148-177`). A missing or
  non-directory entry contributes nothing; a real listing error is a typed
  `GameError` (D11).
* The ordered names go to `PluginRegistry::autoload`
  (`crates/compat/src/plugin.rs`), which logs the reference's
  `(info) Loading <name>` line, replaces a trailing `.tpm` with `.dll`
  (`PluginImpl.cpp:113-133`), and activates each through the same registry path
  as `Plugins.link`.
* `GameHost::open_with_preload` calls `autoload` after the compat install and
  before the startup scripts, the position of `tvpLoadPlugins`
  (`reference/cpp/core/environ/Application.cpp:342`).
* **Deliberate D14 divergence, recorded:** an unported `.tpm` gets the
  null-plugin object and one warning. The reference's `TVPLoadPlugin` tries to
  load the Windows DLL and creates no global when that fails; a port cannot load
  a Windows DLL, and D14's null object is the documented substitute for an
  unported DLL. The auto-load therefore installs the null object under the
  plugin's stem, exactly as `Plugins.link` does for an unported name.

`Plugins.link` itself is unchanged (`crates/compat/plugins_native.rs`); the
`.tpm`→`.dll` replacement lives only in the auto-load path above.
