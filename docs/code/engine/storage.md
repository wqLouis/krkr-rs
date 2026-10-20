# `engine::storage` — the storage namespace

Source: `crates/engine/src/storage.rs`

A `Storage` value owns everything needed to resolve a storage name:

* an immutable bottom `fs::FileSystem` object (created by L1) and the base
  directory it is mounted at;
* the parsed `fs::xp3::Xp3Archive` values explicitly mounted by the caller,
  each with its L2-owned `FileState`;
* the auto search-path list;
* the registered media table and the resolution cache.

The XP3 container read and parse itself is **L1** (`fs::xp3`); this layer owns
only the namespace.

---

## 1. Responsibility

* Mount archives, in order, with later mounts taking precedence.
* Resolve a storage name to a `Location` (a disk path or an archive entry)
  through the reference's order.
* Read bytes, stat and list entries.
* Provide the auto search-path list and the startup plan.

---

## 2. State it owns

| field | meaning |
|---|---|
| `fs: FileSystem` | the immutable L1 file-system object |
| `game_dir: PathBuf` | the app path used for disk assets |
| `archives: Vec<MountedArchive>` | the mounted archives in mount order; later entries take precedence |
| `auto_paths: Vec<String>` | the auto search-path prefixes (a plain directory or an `arc.xp3>` prefix) |
| `preload_archives: bool` | whether an archive is read into memory in full at mount |
| `project: Option<PathBuf>` | the project storage path (the reference's current directory); `None` means a folder project, `Some` an archive project |
| `xp3_extraction_filter: Option<Box<Xp3ExtractionFilter<'static>>>` | the per-segment XP3 extraction filter (the `xp3filter` seam) |
| `media: RefCell<Map<String, Vec<u8>>>` | the virtual media entries registered by L3 storage-media providers |
| `resolution_cache: RefCell<ResolutionCache>` | the lazily built resolver tables and the placed-hit cache |

Each mounted archive keeps its logical path, the backing that owns its bytes
(a `FileState` for a streamed archive, a `Vec<u8>` for a preloaded one) and
its parsed index.

There are no globals and no process-wide caches: a `Storage` is a plain value
the caller owns, and all `FileState`s live inside it.

---

## 3. Public API

### Construction and mounting

```rust
pub struct MountOptions { pub preload_archives: bool }

impl Storage {
    pub fn mount_archives(fs: FileSystem, base_dir: impl Into<PathBuf>,
                         archives: impl IntoIterator<Item = impl AsRef<Path>>) -> Result<Self, MountError>;
    pub fn mount_archives_with(fs, base_dir, archives, options) -> Result<Self, MountError>;
    pub fn mount_discovered_archives(fs, base_dir) -> Result<Self, MountError>;
    pub fn mount_discovered_archives_with(fs, base_dir, options) -> Result<Self, MountError>;
    pub fn mount_archive(&mut self, path: impl AsRef<Path>) -> Result<(), MountError>;
    pub fn unmount_archive(&mut self, path: impl AsRef<Path>) -> bool;
    pub fn set_project(&mut self, path: impl AsRef<Path>);
}
```

Loading is **explicit**: the caller supplies the archive list and the loader
never scans the folder. `mount_discovered_archives` is the one opt-in
exception — the caller asked for a directory instead of a list.

`mount_archive` and `set_project` resolve a **relative** path against the app
directory, the same way, so `--archive data.xp3` and a project path of
`data.xp3` name the same file.

`set_project` decides whether the project is an **archive project** or a
**folder project**: a path that is one of the mounted archives, or that stats
as a non-directory on disk, is an archive; anything else is a folder. `None`
in the `project` field therefore means "a folder project", and `Some` means
"an archive project" with the archive's path. Call it after the archives are
mounted and before any startup script runs.

### The namespace

```rust
impl Storage {
    pub fn add_auto_path(&mut self, path: impl Into<String>);
    pub fn remove_auto_path(&mut self, path: &str);
    pub fn find(&self, name: &str) -> Result<Option<Location>, StorageReadError>;
    pub fn read(&self, name: &str) -> Result<Vec<u8>, StorageReadError>;
    pub fn read_prefix(&self, name: &str, max_len: usize) -> Result<Vec<u8>, StorageReadError>;
    pub fn stat(&self, name: &str) -> Result<Option<StorageMetadata>, StorageReadError>;
    pub fn exists(&self, name: &str) -> Result<bool, StorageReadError>;
    pub fn list(&self, dir: &str) -> Result<Vec<String>, StorageReadError>;
    pub fn file_list(&self, mask: &str, attr: i64) -> Result<Vec<String>, StorageReadError>;
    pub fn startup_plan(&self) -> Result<StartupPlan, StorageReadError>;
    pub fn register_media(&self, name: String, bytes: Vec<u8>);
    pub fn relative_disk_name(&self, name: &str) -> Option<String>;
    pub fn find_disk(&self, name: &str) -> Result<Option<PathBuf>, StorageReadError>;
    pub fn local_name(&self, name: &str) -> Result<Option<PathBuf>, StorageReadError>;
    pub fn clear_resolution_cache(&mut self);
    pub fn probe_counts(&self) -> ProbeCounts;
    pub fn read_bytes(&self) -> u64;
    pub fn game_dir(&self) -> &Path;
    pub fn file_system(&self) -> &FileSystem;
    pub fn archive_count(&self) -> usize;
    pub fn archive_paths(&self) -> impl Iterator<Item = &Path>;
    pub fn for_each_archive(&self, f: impl FnMut(&Path, &Xp3Archive));
}
```

### Free functions

```rust
pub fn discover_archives(file_system: &FileSystem, base_dir: &Path) -> Result<Vec<PathBuf>, StorageReadError>;
pub fn local_name_in(fs: &FileSystem, game_dir: &Path, name: &str)
    -> Result<Option<PathBuf>, StorageReadError>;
```

`discover_archives` is the opt-in recursive scan for `*.xp3` under a directory.
`local_name_in` returns the **real on-disk spelling** of a storage name: it
walks the path below the game directory and replaces each existing component
with its actual name, so a script that asked for `System/Initialize.tjs` gets
the file's true casing back. A name containing the archive delimiter has no
local name.

### Types

| type | purpose |
|---|---|
| `Location` | a resolved location: `Disk(PathBuf)` or `Archive(PathBuf, String)` |
| `StorageMetadata` | size, modified, accessed and created times |
| `MountError` | the mounting failures |
| `StorageReadError` | the read/lookup failures |
| `StartupPlan` | the four startup script names, resolved |
| `ProbeCounts` | the number of `list`/`stat` probes and misses — a diagnostic |
| `ATTR_DIRECTORY`, `ATTR_NORMAL` | the Windows file attribute bits the listing understands |

---

## 4. Resolution order

`Storage::find` resolves a name in this order:

1. **Explicit archive addressing** — `foo.xp3>data/script.ks`. The archive
   is opened on demand when it exists but was not mounted.
2. **The current project storage** — the project archive when `project` is
   `Some` (an archive project), the app directory when it is `None` (a folder
   project). A folder project's app directory is checked first with the
   normalized spelling (the lowercase form wins when both spellings exist),
   then with the script's exact spelling, then a bounded component-wise
   case-insensitive walk. An archive project's project archive is searched by
   its normalized name and is **opened on demand** when it is not one of the
   mounted archives.
3. **Auto paths** — last added first. Each entry's candidate table is built
   once and cached, so the lookup is an in-memory hit rather than a
   directory probe; an archive prefix is opened on demand when its table is
   built.
4. **Remaining mounted archives** — later mounts first, so the archive mounted
   last wins. The project archive is not searched a second time.
5. **The app directory as a loose-file fallback** — archive projects only; a
   folder project already checked it as its current storage.

This is the reference's current-storage-then-auto-paths model. The launcher's
project path becomes the current directory, with the archive delimiter appended
when it names an archive and `/` when it names a folder
(`reference/cpp/core/base/impl/SysInitImpl.cpp:160-165`); a relative name
carries that current path (`reference/cpp/core/base/StorageIntf.cpp:347-425`),
and `TVPGetPlacedPath` checks the current storage before the auto-path table
(`:1061-1108`). So an archive project checks its project archive first and a
folder project checks the app directory first.

Mounted archives other than the project archive are the port's analogue of the
reference's auto-path entries: they are established before any script runs, so
a script `Storages.addAutoPath` is later and wins. Among the mounts themselves,
later wins. The final app-directory fallback for an archive project is a port
extension for loose assets (`plugin/`, `default.ttf`); the reference archive
project checks only its current archive and the auto paths.

A **hit is cached; a miss is not**, matching the reference's auto-path cache
(`reference/cpp/core/base/StorageIntf.cpp:1101`).

A `..` component that escapes the mount is the reference's invalid path name
(`reference/cpp/core/base/StorageIntf.cpp:400`), not a miss.

---

## 5. Invariants

* **Storage names are case-insensitive even when the host filesystem is
  not.** Only the storage-relative part is normalized; the mount prefix
  keeps the real filesystem spelling.
* **A `..` component that escapes the mount is an error**, not a miss:
  `StorageReadError::InvalidPathName`.
* **A directory listing is never cached.** The reference's case-insensitive
  lookup and `GetLocallyAccessibleName` walk the real directory on every
  call, so a script that creates a directory and then resolves it
  case-insensitively must see it. The auto-path candidate table is cached
  separately.
* **The project storage is checked first.** A folder project checks the app
  directory before the auto paths; an archive project checks the project
  archive before them. The project archive is opened on demand when it is not
  mounted, and a name it does not hold falls through to the auto paths and
  the remaining mounts.
* **Later mounts win among the remaining archives.** `mount_archives` mounts in
  the order given, so in a folder project `data.xp3` then `patch.xp3` lets
  `patch.xp3` override `data.xp3`. An archive project keeps its project
  archive first.
* **The discovery order is deterministic** — file name in natural order
  (embedded decimal runs compare by value, so `patch@r1071` sorts after
  `patch@r996`), then a shallower directory last (so it mounts last and
  wins), then the full path.
* **Symbolic-link directories are not followed** by the discovery scan, so a
  link cycle cannot recurse forever.
* **A preloaded archive is read in full at mount.** A short read, an
  unrepresentable size or a failed allocation is a typed `MountError`, never
  a silent fall back to the streaming path.
* **The extraction filter is L2 state.** The stateless L1 reader receives the
  closure as a borrow on every read; it is never stored in L1.
* **The media table is served first**: `read`, `read_prefix` and `exists`
  return a registered virtual entry before the disk and archive resolution, so
  a `psb://…` / `psd://…` provider wins over a file of the same name. A later
  registration for the same name replaces the earlier bytes.

---

## 6. Data flow

```
mount:
   FileSystem (L1) ──► Storage { archives: [MountedArchive { path, backing, Xp3Archive }] }

lookup:
   name ──► Storage::find
              ├─► explicit "arc.xp3>path"  ──► find_in_archive ──► Location::Archive
              ├─► project storage           ──► project archive / app directory
              ├─► auto paths (last first)   ──► Location::Disk / Location::Archive
              ├─► remaining archives (rev)  ──► Location::Archive
              └─► app-dir fallback (archive project) ──► Location::Disk

read:
   name ──► Storage::read ──► Location
              ├─► Disk(path)    ──► fs::open + read_at (L1)
              └─► Archive(path, name) ──► the archive's backing ──► fs::xp3 entry read (L1)
```

---

## 7. Failure modes

| failure | behaviour |
|---|---|
| the mount path is not a directory | `MountError::NotADirectory` |
| an archive cannot be opened or parsed | `MountError::Xp3` |
| a preloaded archive is shorter than its reported size | `MountError::TruncatedArchive` |
| a preloaded archive is too large to address in one buffer | `MountError::ArchiveTooLarge` |
| a preloaded archive's buffer cannot be allocated | `MountError::ArchiveAllocation` |
| the name does not resolve | `StorageReadError::NotFound` |
| a `..` component escapes the mount | `StorageReadError::InvalidPathName` |
| a file-system operation fails | `StorageReadError::Fs` |
| an archive read fails | `StorageReadError::Xp3` |

A genuine file-system failure while probing is **never** collapsed into
"not found". A miss logs one bounded `debug` line naming the searched
locations, and the complete list at `trace`.

---

## 8. Current limits

* **The reference has no mounted-archive set.** After the current storage it
  has only the auto-path table. The port searches the remaining mounts after
  the auto paths, with later mounts winning; a script auto path therefore
  overrides a non-project mount, as the reference's later-added auto path
  would.
* **The project path for a directory launch is the folder.** The D35 scan
  mounts the discovered archives, but a directory launch stays a folder
  project, as in the reference. The tension: a relative
  `Storages.addAutoPath("system/")` resolves against the folder, not inside a
  mounted archive; an archive-only game that relies on the archive current
  path must be launched with that archive explicitly (one `--archive`).
* **`Storages.getFullPath` / `getPlacedPath`** return a normalized relative
  name or `arc>entry`, not the reference's full `file://domain/path` — the
  script-facing shape is L3's.
* **`Storages.addAutoPath`** raises a message that does not match the
  reference's `TVPMissingPathDelimiterAtLast` text.
* **The archive discovery scan is an opt-in extension**: the reference never
  scans a folder. The explicit archive list stays authoritative when it is
  given.

---

## 9. Tests

* `crates/engine/tests/storage.rs` — the mounting order and the precedence,
  the resolution order, the case-insensitive lookup, the auto paths, the
  listing, the errors and the discovery scan.
* `crates/engine/src/storage.rs` `mod tests` — the unit-level invariants.

---

## 10. See also

| topic | document |
|---|---|
| the L3 ↔ L2 protocol for this module | [`rules/api_rules.md`](../../rules/api_rules.md) §4 |
| the storage namespace in the architecture | [`arch/l2.md`](../../arch/l2.md) §5 |
| the L1 file system and the XP3 parse | [`arch/l1.md`](../../arch/l1.md) §2 |
| the game-loading rules | [`rules/arch_rules.md`](../../rules/arch_rules.md) §9 |
| the startup discovery that uses this namespace | [startup.md](startup.md) |
| the archived design trail | [`docs.old/migration/09-game-loading.md`](../../../docs.old/migration/09-game-loading.md) |
