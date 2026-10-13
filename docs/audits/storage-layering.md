# Storage layering audit — moving TVP resolution from L2 to L3

Scope: `crates/engine/src/storage.rs` and every caller in `crates/app`,
`crates/compat`, `crates/vm`. Oracle: `reference/cpp/**`.

**Conclusion.** `engine::storage` is not a generic VFS. It is the whole TVP
storage resolver — the `media://domain/path` grammar, the `arc.xp3>path`
delimiter, ASCII lowercasing/`\`→`/`, the case-insensitive disk walk, the
project/current directory, the auto-path table, the search order and the miss
diagnostic — sitting on top of a small mount/index core. `02 §2.3` and D2 already
assign the TVP storage semantics to L3, and D6 says the *namespace* is L2 while
the *addressing/naming* is L3, but `06-file-system.md` §1–§2 (the inventory row and the "Middle (`engine`)
owns" bullets) explicitly hands `arc.xp3>path` and normalization to L2 and
`scripts/check-layering.sh` deliberately
exempts `arc.xp3>path` from the L2 vocabulary rule because of that sentence. The
code and the guard both encode the contradiction. This audit classifies every
item, describes the TVP-free L2 VFS the mount namespace actually needs, describes
the L3 resolver that should own the policy, and stages the move.

The maintainer's boundary (which overrides `06-file-system.md`) is:

* **L2 `engine`** — mount/unmount a source; open/stat/list/read by an
  **already-resolved** name within a mount; the parsed index and `FileState`
  bookkeeping. No TVP storage-name syntax and no resolution policy.
* **L3 `compat`** — the TVP storage-name grammar and policy (`media://domain/path`,
  `file://`/`file:`, `arc.xp3>path`, lowercasing/`\`→`/`, the case-insensitive
  walk, current dir/project, `addAutoPath`/`removeAutoPath`/`TVPGetPlacedPath`,
  disk-wins ordering, and the exact `TVPCannotGetLocalName`/miss strings). L3
  resolves a script-visible name to a generic L2 VFS name, then calls L2.

---

## 1. Inventory of `crates/engine/src/storage.rs`

Classification: **(a)** generic VFS primitive (stays L2), **(b)** TVP storage
resolution (moves to L3), **(c)** mixed (split). The rule that decides is D2 /
`02 §2.3` ("TVP storage semantics" → L3) and D16's two-sided test: the moment a
behaviour needs TVP vocabulary (`>`, `media://`, `startup.tjs`, `addAutoPath`,
`TVPInvalidPathName`), it is L3.

### 1.1 Public constants

| Item | Class | Why |
|---|---|---|
| `ATTR_DIRECTORY`, `ATTR_NORMAL` | (b) | Win32 `FILE_ATTRIBUTE_*` bits (D18). They are only consumed by the script member `Storages.getFileList(mask, attr)`. Generic VFS has `is_dir`, not an `i64` bitmask. |
| `STARTUP_SCRIPT`, `INITIALIZE_SCRIPT`, `PATCH_SCRIPT`, `AFTER_STARTUP_SCRIPT` | (b) | Literal names from `TVPExecuteStartupScript` (`reference/cpp/core/base/ScriptMgnIntf.cpp:898-975`; `TVPStartupScriptName` at `:371`). TVP startup policy, not a VFS concept. |

### 1.2 Public types / enums / errors

| Item | Class | Why |
|---|---|---|
| `MountOptions { preload_archives }` | (a) | Backing choice for a mount; TVP-free. |
| `Storage` | (c) | The struct is a mount namespace **and** a resolver cache (`auto_paths`, `project`, `resolution_cache`). The mount/index/`FileState` fields stay; the resolver fields move. |
| `Location { Disk(PathBuf), Archive(PathBuf, String) }` | (c) | A resolved location is a generic concept, but the archive key is the archive `PathBuf` (a resolution artifact). Change to `Archive(MountId, String)` so L2 does not re-resolve by path. |
| `StorageMetadata` | (a) | Generic stat result. |
| `MountError` | (a) | Generic mount/open/preload failures (`NotADirectory`, `Fs`, `Xp3`, `TruncatedArchive`, `ArchiveTooLarge`, `ArchiveAllocation`). |
| `StorageReadError` | (c) | `NotFound`/`Fs`/`Xp3` are generic; `InvalidPathName` is the reference `TVPInvalidPathName` (`reference/cpp/core/base/MsgIntfInc.h:28`, raised at `StorageIntf.cpp:400`) and is TVP. |
| `StartupPlan` | (b) | `TVPExecuteStartupScript`'s four-slot sequence. |

### 1.3 `Storage` methods

| Method | Class | Why |
|---|---|---|
| `mount_archives`, `mount_archives_with`, `mount_discovered_archives`, `mount_discovered_archives_with` | (a) | Construct the namespace. The skip-and-warn on a scan is the reference `TVPCheckArchive` catch-and-log (`reference/cpp/core/base/impl/StorageImpl.cpp:562-587`), a generic mount policy. |
| `file_system`, `game_dir` | (a) | Accessors to the L1 root and base. |
| `archive_count`, `archive_paths`, `for_each_archive` | (a) | Mount introspection (diagnostics/project selection). |
| `mount_archive`, `unmount_archive` | (c) | Mount state is generic; `unmount_archive`'s base-name, case-insensitive fallback (and the Docker "later wins" precedence) is resolution policy. |
| `set_project` | (b) | The composition root supplying `TVPProjectDir`; the reference decides archive-vs-folder and appends `>` (`reference/cpp/core/base/impl/SysInitImpl.cpp:160-165`). |
| `add_auto_path`, `remove_auto_path` | (b) | `TVPAddAutoPath`/`TVPRemoveAutoPath` (`reference/cpp/core/base/StorageIntf.cpp:921-957`). |
| `current_storage_path` (private) | (b) | Normalize a relative auto path against the project (`TVPNormalizeStorageName` prepends `CurrentPath`, `reference/cpp/core/base/StorageIntf.cpp:347-425`). |
| `probe_counts`, `read_bytes` | (b) | Diagnostics of the resolver's probe/caching, not of the namespace. |
| `clear_resolution_cache` | (b) | Invalidates the resolver tables (auto paths, listings, placed cache). |
| `find` / `find_recorded` | (b) | The whole search order: explicit `arc.xp3>path` → current dir → auto paths later-first → mounted archives later-first. `TVPGetPlacedPath` (`reference/cpp/core/base/StorageIntf.cpp:1061-1105`). |
| `directory_listing` / `stat_probe` | (c) | Generic `fs::list`/`fs::stat` probes wrapped in the resolver's cache. The cache is (b); the primitives are (a). |
| `auto_path_table`, `build_auto_path_table`, `build_folder_auto_path`, `build_archive_auto_path`, `archive_auto_path_entries` | (b) | `TVPRebuildAutoPathTable` (`reference/cpp/core/base/StorageIntf.cpp:959-1052`): direct children only, keyed by base name. |
| `find_in_archive` | (b) | The `arc.xp3>path` split and the on-demand archive open (`TVPIsExistentStorageNoSearchNoNormalize`, `reference/cpp/core/base/StorageIntf.cpp:737-764`). |
| `archive_disk_path` | (b) | On-demand archive resolution, including the case-insensitive name walk. |
| `relative_disk_name`, `raw_relative_disk_name`, `relative_disk_name_checked` | (b) | TVP storage-name normalization: `\`→`/`, absolute-name stripping against the mount, `.`/`..` folding, `..` escape → `TVPInvalidPathName` (`reference/cpp/core/base/StorageIntf.cpp:400`). |
| `find_disk`, `find_disk_relative`, `find_disk_relative_at`, `resolve_disk`, `find_disk_dir`, `find_disk_case_insensitive` | (b) | Disk resolution: lowercase spelling first, then the script spelling, then the component-wise case walk (`tTVPFileMedia::GetLocallyAccessibleName`, `reference/cpp/core/base/impl/StorageImpl.cpp:339-407`). |
| `local_name`, `case_correct_path`, `case_correct_component` | (b) | The reference `TVPGetLocalName`/`GetLocallyAccessibleName` (`reference/cpp/core/base/StorageIntf.cpp:544-559`, `StorageImpl.cpp:409-414`). Explicitly named in the target boundary as L3. |
| `with_archive`, `read`, `read_prefix`, `count_read_bytes`, `read_archive`, `read_archive_prefix` | (c) | Reading by a resolved name is generic; today `read`/`read_prefix` call `find` internally, so they carry resolution. |
| `stat` | (c) | Same: generic metadata, but keyed through `find`. |
| `exists` | (b) | `TVPIsExistentStorage`, which reports directories and goes through `TVPGetPlacedPath` (`reference/cpp/core/base/StorageIntf.cpp:1120-1123`). |
| `startup_plan`, `app_path_file` | (b) | `TVPExecuteStartupScript` policy; `patch.tjs`/`AfterStartup.tjs` are app-path-only. |
| `list` | (c) | Merges disk + archive children and lowercases names. The merge is generic; the lowering and `arc.xp3>path` framing are TVP. Not called by any native today. |
| `file_list` | (b) | `Storages.getFileList` semantics: Windows `attr` bits, `arcmask>inarcmask` addressing, recursive wildcard walk. Not registered on the L3 surface today (see §2). |

### 1.4 Public free functions

| Item | Class | Why |
|---|---|---|
| `discover_archives` | (a) | A TVP-free recursive `*.xp3` folder scan (D35). The natural/shallower-last sort is loader precedence; it is generic enough to stay L2 (or move to the app composition root). |

### 1.5 Private helpers

| Helper | Class | Why |
|---|---|---|
| `MountedArchive`, `ArchiveBacking` | (a) | Mount state: logical path, parsed `Xp3Archive`, L2-owned `FileState`/image. |
| `ResolutionCache`, `SearchTrace`, `AutoPathTrace`, `ShapeEntries`, `AutoPathTable`, `AutoPathHit`, `DirectoryListing`, `ArchiveChildren` | (b) | Resolver tables/caches and the miss diagnostic. |
| `strip_prefix_ignore_ascii_case` | (b) | Absolute-name resolution against the mount, case-insensitively (`StorageImpl.cpp:301-407`). |
| `fold_dot_components`, `normalize_in_archive_path` | (b) | Storage-name `..` folding; `arc.xp3>` boundary crossing. |
| `collect_archive_children`, `storage_base`, `child_name` | (b) | Auto-path direct-child indexing and base-name extraction (`TVPExtractStorageName`, `reference/cpp/core/base/StorageIntf.cpp:807-830`). |
| `archive_miss_line`, `ARCHIVE_MISS_NEAR_NAMES` | (b) | The miss diagnostic (entry counts, index sections, near-miss names). |
| `consume_number`, `natural_cmp`, `archive_name`, `discover_archives_inner` | (a) | D35 discovery ordering. |
| `system_time_from_millis`, `read_fs_file`, `read_fs_file_prefix`, `read_archive_image`, `wildcard_match` | (a) | Generic read/alloc/wildcard helpers. |
| `normalize_in_archive_name` re-export (`crates/engine/src/lib.rs:31`) | L1 | The function is L1 `fs::xp3` (the XP3 index is keyed by it). L3 depends on `fs` directly (`02 §12.1`), so the `engine` re-export should go and L3 import from `fs::xp3`. |

### 1.6 The duplicated TVP policy in L2

Two more L2 items encode the same TVP storage policy and are part of the same
conflict:

* `crates/engine/src/startup.rs` — `discover_startup` / `StartupScripts` /
  `StartupScript` / `StartupError` repeat `TVPExecuteStartupScript` (startup.tjs,
  `system/Initialize.tjs`, `patch.tjs`, `AfterStartup.tjs`). Used only by
  `crates/engine/tests/startup.rs`; the app uses `Storage::startup_plan`. This
  duplicate should be retired or moved to L3 with the rest.
* The L2 `startup_plan` and L3 `app::game` project rules (`directory_project`,
  `explicit_project`, `storage_has_boot_scripts` in `crates/app/src/game.rs`)
  are reference-cited TVP policy (`SysInitImpl.cpp:160-165`) living above L2.

---

## 2. Caller inventory

### `crates/app` — composition root (wiring, not script-facing)

| Call | Script-facing? | Note |
|---|---|---|
| `mount_archives_with`, `mount_discovered_archives_with`, `discover_archives`, `set_project` | no | Loads the game; the project rule is reference-cited (D15/D35). |
| `storage_has_boot_scripts`, `for_each_archive`, `archive_count`, `file_system`, `game_dir` | no | Startup logging and project selection. |
| `startup_plan` | no | `TVPExecuteStartupScript` sequence, then `compat::exec_storage` runs the scripts. |
| `storage.read` (`image_codec.rs`, `video.rs`), `storage.file_system()`/`game_dir()` | no | Host seams read assets; the names come from L3 (`Bitmap.load`, `LayerExMovie`). |
| `real_game.rs` tests use `archive_count`, `read`, `file_list` | no | Integration test. |

### `crates/compat` — the script surface (resolution-bearing)

| Caller | Members | Script-facing? | Note |
|---|---|---|---|
| `natives/storages.rs` | `find`, `exists`, `local_name`, `add_auto_path`, `remove_auto_path`, `clear_resolution_cache`, `game_dir`, `strip_file_media`, `normalize_storage_name` | **yes** | `Storages.getPlacedPath`/`isExistentStorage`/`getLocalName`/`addAutoPath`/`removeAutoPath`/`getFullPath`. The L3 file already owns the media prefix and the string grammar; it delegates resolution to L2. `getPlacedPath` has its own L3 `PlacedCache`. |
| `natives/plugins/fstat/mod.rs` | `find`, `stat`, `read`, `game_dir`, `relative_disk_name`, `local_name` | **yes** | `Storages.fstat`, `getTime`, `exportFile`, `copyFile`, `changeDirectory`, `dirlist`, `dirtree`, … plus its own `normalize_name`/`local_path`. |
| `resource.rs` | `find`, `stat`, `read`, `read_prefix` | **yes** | `Font`/prerendered-font resolution; `placed_path_key` re-derives a key from `Location`. |
| `natives/visual/bitmap.rs` | `read` | **yes** | `Bitmap.load`/`loadAsync` with extension probing. |
| `natives/sound.rs` | `read` | **yes** | `SoundBuffer` + `.sli` sidecar. |
| `natives/scripts.rs` (`exec_storage`) | `read` | **yes** | `Scripts.execStorage`/`compileStorage`. |
| `natives/plugins/xp3_filter/mod.rs` | `read` | **yes** (plugin) | Reads `xp3filter.tjs`; also sets `storage.xp3_extraction_filter` (L2 field). |
| `natives/plugins/{add_font,layer_ex/image,md5,scripts_ex,psd_file,…}` | `read`, `exists`, `stat`, `read_prefix` | **yes** | Asset loads. |
| `natives/plugins/dirlist/mod.rs` | none directly | **yes** | Delegates to `Storages.dirlist` (fstat), which uses L1 `fs::list`. |
| `natives/plugins/var_file/mod.rs` | none | — | Documents the missing `TVPRegisterStorageMedia` seam. |
| `natives/system.rs` | `game_dir` | **yes** | `System.appDataPath`/exe-path storage names. |
| `ctx.rs` | holds `*mut Storage` | — | The service handle; must become the resolver handle or the `Vfs` handle. |

### `crates/vm` — none

`vm/src/host.rs` mentions the storage only in comments; the VM reaches assets
through the L3 natives and `compat::exec_storage`. No direct `engine::Storage`
call.

**Script-facing summary.** Every resolution-bearing caller is in `compat`. The
app is wiring/diagnostics; the vm is absent. This is exactly the shape the target
boundary wants: the TVP resolution has one owner (L3) and L2 is the namespace.

Also note: `Storage::list` and `Storage::file_list` are **not called by any
native** today (`getFileList` is not registered; `dirlist` uses L1 `fs::list`).
They are exercised only by `crates/engine/tests/storage.rs` and
`crates/app/tests/real_game.rs`. That makes them cheap to move or delete.

---

## 3. Proposed minimal L2 VFS API

TVP-free, stateless-ish state value (D9 does not bind L2, but the API is kept
simple and cache-free so the resolution caches can live in L3). L2 may name L1
types (D33): `FileSystem`, `FileState`, `Xp3Archive`, `Entry`, `FsError`.

```rust
// engine::vfs

/// Identifies a source mounted in a `Vfs`. Stable for the mount's lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MountId(u32);

/// Where a mounted archive's bytes are read from. Owned by L2.
enum ArchiveBacking {
    Stream(FileState),
    Memory(Vec<u8>),
}

/// One mounted `.xp3`.
pub struct ArchiveMount {
    pub id: MountId,
    pub path: PathBuf,
    pub file_name: String,
    pub index: Xp3Archive,      // L1 parsed index (D6)
    backing: ArchiveBacking,
}

/// A mounted source: a host directory or an `.xp3` archive.
pub enum MountSource {
    Directory(PathBuf),
    Archive(ArchiveMount),
}

/// A name resolved by L3 into the L2 namespace.
pub enum Location {
    /// A host path already resolved by L3 (below the base dir or an absolute
    /// auto-path directory).
    Disk(PathBuf),
    /// A member of a mounted archive, by normalized in-archive name.
    Archive(MountId, String),
}

pub struct Vfs {
    fs: FileSystem,
    base_dir: PathBuf,
    mounts: Vec<MountSource>,              // mount order; later = preferred
    pub xp3_extraction_filter: Option<Box<Xp3ExtractionFilter<'static>>>,
}

impl Vfs {
    // --- construction / namespace state --------------------------------
    pub fn new(fs: FileSystem, base_dir: impl Into<PathBuf>,
               options: MountOptions) -> Result<Self, MountError>;
    pub fn file_system(&self) -> &FileSystem;
    pub fn base_dir(&self) -> &Path;

    pub fn mount_archive(&mut self, path: impl AsRef<Path>) -> Result<MountId, MountError>;
    pub fn mount_directory(&mut self, path: impl AsRef<Path>) -> Result<MountId, MountError>;
    pub fn unmount(&mut self, id: MountId) -> bool;
    pub fn mounts(&self) -> &[MountSource];
    pub fn archive_count(&self) -> usize;

    // --- parsed index / mount navigation (no resolution) ----------------
    pub fn archive(&self, id: MountId) -> Option<&Xp3Archive>;
    /// Case-insensitive archive lookup by file name (the reference opens an
    /// archive by its storage name).
    pub fn archive_by_name(&self, file_name: &str) -> Option<MountId>;
    pub fn archive_entries(&self, id: MountId) -> impl Iterator<Item = &xp3::Entry>;

    // --- primitive IO by an already-resolved name -----------------------
    pub fn read(&self, location: &Location) -> Result<Vec<u8>, VfsError>;
    pub fn read_prefix(&self, location: &Location, max_len: usize) -> Result<Vec<u8>, VfsError>;
    pub fn stat(&self, location: &Location) -> Result<StorageMetadata, VfsError>;
    /// Open a resolved entry as a sequential L2 stream (the `Storages.open`
    /// seam in `06 §3`). `EntryStream` is an L2 value carrying the source
    /// `FileState` (or archive mount) plus a cursor.
    pub fn open(&self, location: &Location) -> Result<EntryStream, VfsError>;

    // --- raw directory primitives L3 resolution needs -------------------
    pub fn stat_path(&self, path: &Path) -> Result<Option<bool /* is_dir */>, FsError>;
    pub fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>, FsError>;
}
```

Design notes:

* **No name policy.** `Vfs` never lowercases, never splits on `>`, never folds
  `..`, never walks cases, never knows about auto paths or a project. It accepts
  a `Location` or a directory path.
* **No resolution cache.** `read`/`stat` take `&self` and do not memoize;
  `Location` is a plain value. The auto-path/placed/directory caches move to L3.
  This removes the current `RefCell<ResolutionCache>` from L2.
* **`Location::Archive` is mount-keyed**, so reading never re-resolves a path.
  `MountId` must be invalidated by unmount; L3 caches keyed by `MountId` must be
  cleared when mounts change (the app calls that once at startup today).
* **Backing selection** (`Stream` vs `Memory`, `preload_archives`) stays L2: it
  is mount mechanism, not resolution.
* **`discover_archives`** stays as `engine::vfs::discover_archives` (generic) or
  moves to `app`.
* `VfsError` replaces `StorageReadError`; `NotFound`/`Fs`/`Xp3` remain. The
  `InvalidPathName` variant moves to the L3 error type (it is raised by L3
  grammar, not by L2 IO).

`MountError`, `StorageMetadata`, `MountOptions` move unchanged.

---

## 4. Proposed L3 resolution layer

Location: a new module tree `crates/compat/src/storage/` — for example
`storage/mod.rs` (pipeline), `storage/name.rs` (grammar), `storage/resolver.rs`
(state + search), `storage/auto_path.rs` (table), `storage/trace.rs` (miss
diagnostic). `compat` already depends on `fs` and `engine` (`02 §12.1`), so it
imports `fs::xp3::normalize_in_archive_name` directly and drops the `engine`
re-export.

### 4.1 Ownership

The resolver is per-VM dispatch state (the same pattern as `PlacedCache` and the
fstat `FstatState`), stored in `CompatServices::state::<StorageResolver>()` or
owned by `CompatHost` and attached alongside the `Vfs` pointer. It owns:

* `auto_paths: Vec<String>` — the `TVPAutoPathList`;
* `project: Option<PathBuf>` — the reference current directory;
* the caches today in `ResolutionCache`: auto-path tables, directory listings,
  placed hits, probe counts, read bytes;
* the `SearchTrace`/`AutoPathTrace` diagnostic.

The `Vfs` (mounts, index, backing) stays owned by the composition root / host and
is borrowed by the resolver. The pointer-to-`Vfs` service handle replaces the
current pointer-to-`Storage`.

### 4.2 Pipeline: parse → normalize → resolve → call L2

```
script name (Storages.*, System.*, Scripts, plugins)
  │
  ├─ 1. parse       media://domain/path | file:// | file: ; split on '>'
  │                 (TVPStorageMediaManager::NormalizeStorageName,
  │                  reference/cpp/core/base/StorageIntf.cpp:240-430;
  │                  TVPArchiveDelimiter split, :271-284)
  ├─ 2. normalize   lowercase ASCII path, '\' → '/', in-archive part via
  │                 fs::xp3::normalize_in_archive_name
  │                 (StorageImpl.cpp:95-117; StorageIntf.cpp:566-606);
  │                 fold './..' and raise TVPInvalidPathName on escape
  │                 (StorageIntf.cpp:400)
  ├─ 3. resolve     current dir / project first, then auto paths
  │                 later-added-first, then mounted archives later-first;
  │                 disk wins over archive
  │                 (TVPGetPlacedPath, StorageIntf.cpp:1061-1105;
  │                  CheckExistentStorage, StorageImpl.cpp:118+;
  │                  TVPRebuildAutoPathTable, :959-1052)
  │                 via Vfs::{stat_path,list_dir,archive,archive_by_name,
  │                 archive_entries}
  └─ 4. call L2     Vfs::read / stat / read_prefix / open(&Location)
```

Grammar rules and where they live:

| Rule | Reference | L3 entry point |
|---|---|---|
| media/domain/path split, `file://`/`file:` | `StorageIntf.cpp:240-430` | `storage::name::parse` |
| `arc.xp3>path` split | `StorageIntf.cpp:271-284`, `:737-764` | `storage::name::split_archive` |
| lowercase + `\`→`/` | `StorageImpl.cpp:95-117` | `storage::name::normalize` |
| in-archive normalize | `StorageIntf.cpp:566-606` | `fs::xp3::normalize_in_archive_name` |
| case-insensitive walk | `StorageImpl.cpp:339-407` | `storage::resolver::resolve_disk` |
| project/current dir | `SysInitImpl.cpp:160-165` | resolver `project` |
| auto path table | `StorageIntf.cpp:921-1052` | `storage::auto_path` |
| placed/hit cache | `StorageIntf.cpp:1101` | resolver `placed` |
| disk-wins ordering | `StorageIntf.cpp:1061-1105` | `storage::resolver::resolve` |
| miss diagnostic | (port instrument) | `storage::trace` |

### 4.3 How the natives use it

* `natives/storages.rs` — drop `attached_storage` / `storage.find` /
  `storage.local_name` / `storage.add_auto_path`; take the resolver from
  `CompatServices::state` and call `resolver.find(name)` /
  `resolver.add_auto_path(name)` / `resolver.local_name(name)`; read through
  `vfs.read(&location)`. `normalize_storage_name`, `strip_file_media` and
  `extract_*`/`chop_*` move from the native module into `storage::name` so the
  grammar has one home (fstat's private `normalize_name` is deleted and imports
  the same function).
* `natives/plugins/fstat/mod.rs` — `local_path`, `placed_local`,
  `normalized_local`, `raw_local`, `local_name_no_search`, `change_directory`,
  `validate_local_name` call the resolver's disk/local-name methods.
* `natives/scripts.rs`, `resource.rs`, `natives/visual/bitmap.rs`,
  `natives/sound.rs`, `natives/plugins/{xp3_filter,layer_ex,psd_file,…}` — call
  `resolver.find` then `vfs.read`; the `StorageReadError::NotFound` matches
  become the L3 error's `NotFound`.
* The `xp3_extraction_filter` field and its install path stay on `Vfs` (L2),
  because the filter is a read transform, not resolution.

### 4.4 Exact error strings (preserved)

| Reference | String | Where |
|---|---|---|
| `TVPInvalidPathName` | `Invalid path name %1` | `MsgIntfInc.h:28`; raised `StorageIntf.cpp:400`; already reproduced by `storage_error_message`. The L3 error must keep the verbatim `"Invalid path name {name}"`. |
| `TVPCannotGetLocalName` | `Cannot get local name from %1` | `MsgIntfInc.h:35`; `StorageIntf.cpp:546-550`. |
| `TVPCannotFindStorage` | `Cannot find storage %1` | `MsgIntfInc.h:23`; `TVPSearchPlacedPath` `StorageIntf.cpp:1109-1115`. |
| missing auto-path delimiter | `Use path delimiter '>' or '/'` | `MsgIntfInc.h:32`; `TVPAddAutoPath` `StorageIntf.cpp:921-937`. |
| our own misses | `Storages.<member>: ...` | Unchanged; the L3 `storage_error_message` helper keeps the prefix for messages with no reference counterpart. |

The L3 error type (`StorageError`) owns `InvalidPathName` +
`CannotGetLocalName` + a generic `NotFound`; the native mapping decides which
reference string to raise per member.

---

## 5. Staged migration plan

The resolution and the namespace are entangled, so the smallest **coherent**
first code slice is the mechanical extraction of the resolver; splitting smaller
would duplicate resolution logic in both layers. The plan keeps the game
runnable at every step (D7).

### Slice 0 — docs and guard (no code)

1. Apply the doc edits in §6.
2. Add `l2-storage-resolution` to `scripts/check-layering.sh` (and its own
   allowlist entries) forbidding in `crates/engine/**`:
   * `split_once('>')` / `split_once(">")` / `find('>')` / `contains('>')` /
     `TVPArchiveDelimiter`;
   * `add_auto_path`/`remove_auto_path`/`auto_path`/`placed`/`set_project`;
   * `find_in_archive`/`local_name`/`case_correct`/`find_disk`/`resolve_disk`;
   * the literals `"Invalid path name "`, `media://`, `file://`, `file:`;
   * `startup.tjs`, `system/Initialize.tjs`, `patch.tjs`,
     `AfterStartup.tjs`, `normalize_in_archive_name`;
   * remove the `arc.xp3>path` exemption note from the script.
   Until the code moves, add reviewed allowlist entries for the current sites so
   the gate stays green; the allowlist shrinks as each slice lands.

### Slice 1 — extract the L3 resolver (mechanical, no behavior change)

* Add `engine::vfs` with the API of §3 (`Vfs`, `MountId`, `Location`,
  `ArchiveMount`, `MountOptions`, `MountError`, `StorageMetadata`, `VfsError`);
  move the mount/index/backing/read/stat code verbatim from `storage.rs`.
  `read`/`stat` take a `Location`; `read` no longer calls `find`.
* Add `compat::storage` and move `find`/`find_disk`/`find_in_archive`/
  `resolve_disk`/`case_correct`/`auto_path`/`current_storage_path`/`set_project`/
  `relative_disk_name`/`local_name`/`startup_plan`/the trace and caches verbatim.
  The resolver accesses `Vfs` via `stat_path`/`list_dir`/`archive`/
  `archive_by_name`/`archive_entries`.
* Rewire the L3 natives (and `CompatibilityServices`) to the resolver + `Vfs`.
* Preserve the exact error strings (§4.4); no behavior assertion changes.
* Move `normalize_storage_name`, `strip_file_media`, `extract_*`/`chop_*` into
  `compat::storage::name` and delete the copies in `natives/storages.rs` and
  fstat's `normalize_name`.
* Tests: `crates/engine/tests/storage.rs` reference-cited resolution tests move
  to `crates/compat/tests/storage_resolution.rs`; generic mount/read/index tests
  stay as `crates/engine/tests/vfs.rs`. `crates/engine/tests/startup.rs` moves
  with `discover_startup`.
* `engine/src/lib.rs`: drop the `normalize_in_archive_name` re-export.

### Slice 2 — retire the L2 startup module

Move/merge `engine::startup::{discover_startup, StartupScripts, StartupScript,
StartupError}` into `compat::storage`; `Storage::startup_plan` becomes the single
resolver entry point. `crates/app` calls the resolver.

### Slice 3 — collapse the tail

* Delete `Storage::list`/`file_list` (unregistered script members) or move them
  to L3; update `real_game.rs`.
* Decide `discover_archives`: keep in `engine::vfs` or move to `app`.
* Remove the `l2-storage-resolution` allowlist entries; the guard is now real.

### What must stay green

* `scripts/surface_lock.py` — the registered `Storages` member set must not
  change (`Storages` methods are unchanged; only their implementation moves).
* `scripts/native_parity.py` / `plugin_parity.py --no-doc` — the plugin surface
  (`Storages.fstat`, `getFileList`, …) is unchanged.
* Every `reference/cpp/...` citation currently in `crates/engine/tests/storage.rs`
  and `startup.rs` moves with its test, not deleted.

### Risk areas

* **D30 reference tests live in L2 today.** `crates/engine/tests/storage.rs`
  (≈40 tests) and `startup.rs` are the evidence for TVP resolution but sit in
  the L2 crate. The move must keep the citations and the assertions; the generic
  subset stays in L2.
* **`MountId` lifetime vs L3 caches.** Mounts change at startup; any later mount
  change must clear the resolver caches. The app owns that call today; document
  the invariant on `Vfs` and the resolver.
* **Two `PlacedCache`s.** `natives/storages.rs` keeps an L3 `PlacedCache` while
  the L2 `find` caches hits too. Slice 1 must pick one place for the placed
  cache (the resolver), and `clearArchiveCache` must clear exactly that.
* **`placed_path_key` / `compute_placed_path`.** L3 currently re-derives a key
  from `Location`. With `Location::Archive(MountId, …)` the key should use the
  archive file name (the reference returns a storage name), so
  `archive_by_name`/`archive(id).file_name` is needed.
* **`refCell` removal is not free.** The current `find(&self)` mutates through
  `RefCell`. If the resolver is `CompatServices::state`, the borrow discipline
  is explicit; if it is `&mut`, the `Storages` callbacks (which hold `&*storage`)
  need re-plumbing.
* **`on-demand` archive open.** `find_in_archive`/`archive_auto_path_entries`
  open an archive that exists on disk but is not mounted. That is resolution
  policy (L3) but it calls `fs::open`+`Xp3Archive::open`. L3 may open and parse
  directly via L1 `fs`+`fs::xp3` (allowed) or ask `Vfs` to mount it; either way
  the parsed index is L2-owned. Decide explicitly and keep the `MountError`
  path typed.

---

## 6. Doc edits

### `docs/migration/06-file-system.md`

1. **Status line (1).** Replace
   *"Raw byte IO lives in the bottom `fs` crate and the TVP storage namespace sits
   on top of it in L2 `engine::storage`."*
   with
   *"Raw byte IO and XP3 parsing live in the bottom `fs` crate, the generic mount
   namespace in L2 `engine::vfs`, and the TVP storage resolution in L3
   `compat::storage`."*

2. **§1 Inventory, first row.** Replace
   *"Game mount + namespace (`arc.xp3>path`, disk-over-archive, case rules,
   auto-path) | L2 `engine::storage` over the `FileSystem` object"*
   with two rows:
   *"Mount namespace + parsed index + `FileState` + read/list primitives | L2
   `engine::vfs` over the `FileSystem` object"* and
   *"TVP storage resolution: `arc.xp3>path`, media grammar, case rules,
   disk-wins ordering, auto-path, project, error strings | L3 `compat::storage`"*.

3. **§2 `### Middle (engine) owns`.** Delete the bullets *"`arc.xp3>path`
   addressing and the specific-archive prefix"*, *"normalization (lowercase, `/`
   separators) and the recursive case-insensitive walk"*, and *"the
   auto-search-path list …"*. Keep the mount namespace/bases/disk-wins ordering
   **of mounts** (later mount wins), the parsed index and the `FileState`s. Add:
   *"resolution (the name grammar, the case-insensitive disk walk, the
   auto-search path and disk-wins ordering) is L3 (`compat::storage`); L2 accepts
   an already-resolved `Location`."*

4. **§2 `### L3 (compat) uses`.** Replace *"The exact same method surface and
   error strings; its implementation calls the middle layer"* with
   *"The TVP storage semantics: the `media://domain/path` / `file://` / `file:`
   grammar, the `arc.xp3>path` delimiter, lowercasing/`\`→`/`, the
   case-insensitive disk walk, the project/current directory, the
   `Storages.addAutoPath`/`removeAutoPath` list, the `TVPGetPlacedPath` search
   order and the exact `TVPCannotGetLocalName`/`TVPInvalidPathName` strings. It
   resolves a script-visible name to a generic L2 VFS `Location` and then calls
   L2. Its implementation never touches `std::fs` (`l3-platform`)."*

5. **§3 last paragraph.** Keep the `Xp3Archive`/`FileState` statement; change
   *"L2 owns the `FileState`s and the parsed index"* to *"L2 owns the `FileState`s
   and the parsed index; L3 owns the resolved `Location` and the search
   policy."*

6. **§7 `Case-insensitivity and archives`.** Change *"The recursive
   case-insensitive disk walk (`engine/src/storage.rs:47-70`) and the archive
   entry map are … The middle layer should …"* to attribute the index to L2 and
   the walk to L3, and update the `engine/src/storage.rs` path to the new
   modules.

7. **§7.1 Layering paragraph.** Unchanged (the `Vec<u8>` image is still L2).

8. **§9 `Implementation`.** Replace *"`engine::Storage` sits on top of the fs
   functions and owns the namespace, the mount index and the auto-path table."*
   with *"`engine::vfs` sits on top of the fs functions and owns the mount
   namespace, the archive index and the read/stat primitives. `compat::storage`
   owns resolution (`arc.xp3>path`, case rules, the auto-path table, the
   project)."* and *"L3 file IO calls the L1 `fs` abstraction"* with *"L3
   resolves through its storage resolver and reads through the L2 `Vfs` (or the
   L1 `fs` object for pure probes)."*

9. Add a short note in §2 next to the **D6 note** stating explicitly: *"This
   corrects the earlier wording in this section: `arc.xp3>path` and storage-name
   normalization are **not** L2; they are the TVP semantics of `02 §2.3` and live
   in L3."*

### `docs/migration/02-target-architecture.md`

1. **§2.2 L2 bullet.** Change *"a **virtual file system** over the parsed
   archives that L1 hands back (`mount`/`read`/`list`), with mount points and an
   index"* to add *"TVP-free: it accepts an already-resolved name; the storage
   grammar and search policy are L3 (see §2.3)."*

2. **§2.3 L3 bullet.** Expand *"The **TVP storage semantics**: `arc.xp3>path`
   addressing, case rules, `addAutoPath`."* to the full list (media grammar,
   `arc.xp3>path`, lowercasing/`\`→`/`, case-insensitive walk, project/current
   dir, auto-path/`TVPGetPlacedPath`, disk-wins ordering, exact error strings)
   and add *"L3 resolves a script-visible name to a generic L2 VFS name and then
   calls L2."*

3. **§5 flow.** Change the last line *"L2 registers it in the VFS namespace
   (state) and resolves names"* to *"L2 registers it in the VFS namespace
   (state); L3 resolves a script name to a VFS name and calls L2."*

4. **§7 table.** Change the L2 row comment *"Layer system (generic scene graph),
   mixer, timers, VFS namespace, caches"* to add *"(mounts + parsed index +
   read/list; TVP resolution is L3)"*, and the L3 row to name the resolver.

5. **§12.3 `storage` row.** Replace
   *"`Storage` + `mount_archives`/`mount_archive`/`find`/`read`/`stat`/`exists`/`get_file_list`/`add_auto_path`/`extract…`; `discover_startup` | The VFS namespace + TVP addressing."*
   with *"`Vfs` + `MountId`/`Location`/`mount_archive`/`mount_directory`/`unmount`/`read`/`stat`/`list_dir`/`stat_path`/archive index; `discover_archives` | The generic mount namespace; TVP storage resolution is L3 (`compat::storage`)."*

6. **§12.4.** Change *"XP3 decompress cache / mmap → `fs` + `engine::storage`
   internals"* to the new module path.

### `docs/migration/03-middle-layer-api.md`

1. **§1 bullet.** Replace *"The storage namespace, `arc.xp3>path` addressing,
   case-insensitive walk and mount index are L2 state; L1 has no namespace."*
   with *"The mount namespace and parsed index are L2 state; `arc.xp3>path`
   addressing, the case-insensitive walk and the auto-path search are L3
   resolution. L1 has no namespace."*

2. **§8 `Engine` field.** Change `storage_index: StorageIndex, // namespace,
   arc.xp3>path` to `vfs: Vfs, // mount namespace; TVP resolution is L3`.

3. **§10.** Replace *"Storage namespaces / `.xp3` / auto-path search (L2)."* with
   *"`.xp3` and the mount namespace (L2); the storage-name grammar and auto-path
   search (L3)."*

### `docs/migration/README.md`

1. **D6** (the one live contradiction): replace *"the **VFS namespace** and TVP
   `arc.xp3>path` addressing are L2/L3."* with *"the **VFS mount namespace** is
   L2; the TVP storage-name grammar, `arc.xp3>path` addressing and resolution
   policy (case walk, auto paths, project, disk-wins ordering, error strings) are
   L3."*

D2 already says *"storage naming … are L3"* and D16's two-sided test already
decides the boundary, so no new decision is strictly required. If the guard needs
a named anchor, add a short **D39 — storage resolution is L3** entry beside D6
restating the two-sided test for storage; the audit does not depend on it.
