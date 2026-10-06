# 06 — File system

Status: design sketch. Extracts raw byte IO into the bottom core and moves
the TVP storage namespace on top of it.

Goal: stateless bottom-layer file-system functions over an explicit root
that know about paths, URIs, handles, stat and listing — and nothing about
`.xp3`, `arc.xp3>path` addressing, case-insensitive search or TJS. The
middle layer keeps TVP's storage semantics (including all cursor/index
state); the natives keep their current surface.

---

## 1. Inventory being replaced

| Concern | Today | Target |
|---------|-------|--------|
| Game mount + namespace (`arc.xp3>path`, disk-over-archive, case rules, auto-path) | `engine/src/storage.rs` (concrete `PathBuf`/`fs`) | L2 `krkr-engine::storage` over the fs functions |
| XP3 container parsing + segment read | `crates/xp3/src/archive.rs` (owns `std::fs::File`, seek/read) | L2, reading a `FileHandle` from `krkr-fs` |
| Raw host path IO | `std::fs` scattered in `engine`, `tvp-storages`, `tvp-visual`, `tvp-natives`, `engine::logging`, `engine::state` | L1 `krkr-fs` (stateless fns) |
| Stream abstraction | `tvp-streams::binary::BinaryStream` (exists, unused by storage) | aligned with `FileHandle`; the sequential cursor moves to L2 |
| TJS `Array.save/load/saveStruct` | C++ `std::fopen` in `tjs2-sys/cpp/streams.cpp` | unresolved; see §6 |
| Android SAF / APK assets | SAF→path + `MANAGE_EXTERNAL_STORAGE` (option A, `docs/android.md §4`) | optional `Uri` backend |

The read path today has **no trait** (`docs/portability.md`); this is the
largest structural change of the three bottom components.

## 2. Boundaries

### Bottom (`krkr-fs`) knows

* locate/open a path or URI in read/write/append mode and return a
  `FileHandle`;
* **positional** `read_at`/`write_at`, `size`, `flush` on a handle;
* stat, exists, list a directory, create dirs, remove, copy, rename;
* the host path dialect (Linux `PathBuf`, Android `PathBuf` for app-private
  dirs and resolved external paths, Android content/asset `Uri`).

It knows **nothing** about cursors, buffering policy, caches, namespace
resolution or which archive a name belongs to. All of that is L2 state.

### Middle (`krkr-engine`) knows

* storage namespaces: game dir + mounted archives, disk-wins ordering;
* `arc.xp3>path` addressing and the specific-archive prefix;
* normalization (lowercase, `/` separators) and the recursive
  case-insensitive walk that makes Windows-authored games work on Linux;
* the auto-search-path list (`engine/src/storage.rs` global);
* archive containers (`xp3`, and optionally the unused `tvp-archive`
  zip/7z/tar readers), built over `FileHandle`s;
* `Storages` native semantics: `stat`, `getFileList`, `open`, `fstat`,
  `deleteFile`, `copyFile`, save-data writes.

### Natives (`krkr-tjs`) keep

The exact same method surface and error strings; only their implementation
calls the middle layer instead of touching `std::fs`.

## 3. Handles, not streams

The bottom layer is positional by design (D9). `open` returns an RAII
`FileHandle`; L2 owns the sequential cursor and calls `read_at(offset, buf)`.

```rust
pub fn open(root: &FsRoot, path: &FsPath, mode: OpenMode) -> Result<FileHandle, FsError>;

impl FileHandle {
    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, FsError>;
    pub fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<usize, FsError>;
    pub fn size(&self) -> Result<u64, FsError>;
    pub fn flush(&mut self) -> Result<(), FsError>;
}
```

The existing `tvp-streams::binary::BinaryStream` (`binary.rs:57`) remains the
**sequential** stream the scripts see; it becomes a thin L2 wrapper that
remembers an offset and delegates to `read_at`. There is only one vtable in
L1 (`FileHandle`), not two.

`Xp3Archive` must be reworked to parse over positional reads (or a buffered
`Vec<u8>`) instead of a live `std::fs::File`. This is the prerequisite that
also unlocks:
* concurrent reads (no shared cursor) for parallel media/image workers;
* Android `AssetManager`/SAF sources;
* in-memory test fixtures.

## 4. Backends

### 4.1 Host path root (Linux + Android app-private dirs)

`std::fs`-based. Used for:
* the game directory (desktop, and Android's resolved external path under
  option A);
* the app data dir (`filesDir` on Android, `temp_dir()` elsewhere);
* savedata (`savedata/` under the game dir or app data dir).

### 4.2 In-memory root (tests + wasm-style future)

An in-memory map. This is what L2 unit tests use and what makes the storage
namespace testable without a game tree. It is also the natural base if a
web target is ever revisited (`docs/portability.md`).

### 4.3 Android asset / SAF roots (optional, later)

* `AssetManager` for APK-bundled game data (`AAssetManager_open`), if the
  project ever ships data in the APK (currently it does not).
* SAF/`ContentResolver` streaming for permission-free external storage
  (option B in `docs/android.md §4`, explicitly deferred). The `FsPath::Uri`
  variant in the API exists so this can be added without an API change.

The plan keeps the current Android decision (SAF for picking, resolved path
for reading, `MANAGE_EXTERNAL_STORAGE`) as the default, because it already
works and `option B` is a large JNI-per-read undertaking. The stateless fs
surface makes option B *possible* without a second rewrite.

## 5. Save data, logging and state files

These are not game-storage reads but still OS IO:

* logging sink (`engine/src/logging.rs`, rotating `krkr.log`) → fs
  functions or a dedicated `LogSink` writer;
* state file (`engine/src/state.rs`, `krkr_state.json`) → fs functions;
* savedata (`savedata/`) → fs functions;
* `Debug.startLogToFile` → fs functions.

All of them become testable with the in-memory root.

## 6. The C++ `fopen` bypass

`tjs2-sys/cpp/streams.cpp` implements TJS `Array.save/load/saveStruct` with
`std::fopen` directly, bypassing Rust storage. This is the one IO path the
stateless fs surface cannot reach. Options:

| Option | Description | Cost |
|--------|-------------|------|
| A | Add an IO hook in `cpp/streams.cpp` that calls back into Rust (function-pointer table set at init). | C ABI addition + threading care; keeps TJS file semantics. |
| B | Reimplement the save/load natives in Rust (`krkr-tjs`) and stop using the C++ stream. | Touches the TJS ABI; must match serialization exactly. |
| C | Leave as-is; document `std::fopen` as a platform escape hatch. | Degrades on Android/scoped storage; keeps `std::fs` reachable outside L1. |

Plan: **A** if save files must go through the fs surface; otherwise **C** is
acceptable for the first migration milestones and revisited before Android
savedata hardening. This is listed as an open question in
`07-roadmap.md §7`.

## 7. Case-insensitivity and archives

The recursive case-insensitive disk walk (`engine/src/storage.rs:47-70`) and
the archive entry map are performance-sensitive on large games (23,572
entries measured in `docs/optimization.md`). The middle layer should:

* index the storage namespace once at mount (name → source) instead of
  doing `Path::is_file` per lookup;
* use positional reads (`pread`/`read_at`) so archive reads can be
  parallelized across media/image workers without a shared cursor;
* support an in-memory-root-backed pre-loader for tests.

None of this changes the stateless fs surface; it is L2 optimization and
L2-owned index state.

## 8. Platform notes

* **Linux**: host path root, `std::fs`. `xdg-open` etc. stay in `System`.
* **Android**: host path root for app-private dirs and the resolved game
  path; state/log paths come from the JNI `pendingStateDir`; `System.appDataPath`
  falls back to the state dir (`runner.rs app_data_dir`).
* **wasm** (not planned): a `fetch`-backed in-memory mount plus OPFS saves
  would implement the same functions; the in-memory `FsRoot` is the
  stepping stone.

## 9. Migration steps specific to FS

1. Define the stateless fs functions + `FsRoot`/`FileHandle` in `krkr-api`
   (`03-middle-layer-api.md §1`) and align `tvp-streams` with `FileHandle`.
2. Implement the host path root + the in-memory root.
3. Rework `xp3::Xp3Archive` to read positionally; L2 owns any buffering.
4. Move `engine::Storage` into L2 `krkr-engine` on top of the fs functions;
   keep the current public API as a shim for natives.
5. Route logging/state/savedata/native file IO through the fs functions.
6. Add Android/SAF roots only if/when APK assets or SAF streaming are needed.
7. Decide the `cpp/streams.cpp` hook.

Each step is independently testable with the in-memory root and preserves
the existing `Storages`/`Scripts` behavior.
