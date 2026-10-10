# 09 — Game loading

Status: implemented.
Related: [`02-target-architecture.md`](02-target-architecture.md) §5 (archive
parsing in L1), [`08-tjs-vm-and-native-plugins.md`](08-tjs-vm-and-native-plugins.md).

---

## 1. Input

Higher layers do not search for the game. The composition root takes an
explicit input and hands it to L2.

CLI:

```
krkr --archive data.xp3 --archive patch.xp3 [--base-dir DIR] [GAME]
```

* `--archive` is **repeatable** and **order-significant** (§4). It stays
  authoritative and is mounted in addition to a positional archive.
* One positional argument `GAME` is accepted, mirroring the reference
  `TVPCheckStartupArg` (`reference/cpp/core/environ/win32/Platform.cpp:93`):
  * a **file** is mounted as an archive (`TVPCheckArchive`,
    `reference/cpp/core/base/impl/StorageImpl.cpp:562`);
  * a **directory** becomes the app path; its loose `startup.tjs` decides
    bootability (the reference `TVPListDir` branch).
* **No directory scan.** There is no default folder scan for `*.xp3` in the
  reference: win32 `TVPCheckStartupArg` only inspects `argv[1]`, and the
  Linux/macOS/Android implementations return `false` outright
  (`environ/linux/Platform.cpp:285`, `environ/apple/macos/platform.mm:208`,
  `environ/android/AndroidUtils.cpp:695`). We never enumerate a folder for
  `*.xp3`; a `startup.tjs` inside a mounted archive is found by the normal
  storage search (§5), not by a scan. *(Amended by D35: a directory-only
  launch — `--base-dir`/positional directory with no `--archive` — opts into a
  recursive `*.xp3` scan (`engine::storage::discover_archives`) as a documented
  ergonomic extension; explicit `--archive` entries suppress it.)*
* `key=value` arguments become command-line config
  (`TVPSetCommandLine` semantics).

Android: the same archive list arrives through Intent extras. The old
`pendingGameDir`/`pendingGameName` contract is replaced by an archive list +
base dir.

## 2. Base directory

Default: the **parent directory of the first archive** (the reference
`TVPGetAppPath` convention). `--base-dir DIR` (or a positional game
directory) overrides it, and it is resolved to an absolute path, so the app
path is the same from any working directory. This is the "app path"
(`TVPGetAppPath`) used for:

* the app-path-only scripts in §5 (`patch.tjs`, `AfterStartup.tjs`);
* loose files and assets (`plugin/`, `default.ttf`, …);
* the root every relative `--archive` path resolves against, so
  `--base-dir /path/to/game --archive data.xp3` mounts
  `/path/to/game/data.xp3`.

Disk files still win over archive entries (unchanged rule from the current
`Storage::find`).

**Directory scan (D35).** When no `--archive` is given, the base directory is
recursively scanned for `*.xp3` and those archives are mounted, so a game
folder is a complete input (`krkr --base-dir /path/to/game`, or
`krkr /path/to/game`). `engine::storage::discover_archives` orders them by
file name (case-insensitive), then shallower-first, so `patch.xp3` overrides
`data.xp3` and a root archive overrides a nested one of the same name; the
scan is followed by `Storage::mount_archives`, whose later-wins rule applies.
Explicit `--archive` entries are mounted instead of the scan (the explicit
list is authoritative). Symbolic-link directories are not followed.

## 3. Mounting

L2 mounts the resolved archive list: the explicit `--archive` entries, or the
result of `discover_archives` (D35) when none are given. Each mount:

```
L2 Storage::mount_archive(path)
  → L1 FileSystem open + read (FileState)
  → L1 parse the XP3 container (stateless)
  → L1 returns the parsed index to L2
  → L2 registers it in the VFS namespace (state)
```

L1 reads *and* parses; L2 owns the namespace/state and sees only a parsed
file system.

## 4. Precedence

Mount order is the given order, and **later mounts win** — so

```
--archive data.xp3 --archive patch.xp3
```

makes `patch.xp3` override `data.xp3`. This matches the reference
auto-path table (`tTJSHashTable::AddWithHash` *overwrites* on a duplicate
key, so the last `Add` wins). It **corrects** the current `engine`
behaviour, which scans archives in mount order and takes the first match
(data would incorrectly override patch).

## 5. Startup discovery (L2)

Reference order from `TVPExecuteStartupScript`
(`reference/cpp/core/base/ScriptMgnIntf.cpp:900`):

1. **`patch.tjs`** — **app path only, no search**
   (`TVPGetAppPath() + "patch.tjs"`), executed if it exists. Games use it
   for `@set(...)` preprocessor flags.
2. **`startup.tjs`** — **searched** across the storage (disk + mounted
   archives), via `TVPSearchPlacedPath`.
3. **`AfterStartup.tjs`** — app path only, if it exists.
4. If `startup.tjs` fails and `system/Initialize.tjs` exists, execute that
   (reference compatibility fallback).

If no `startup.tjs` is found (and no fallback applies), return a **typed
error** — matching `TVPCheckArchive` returning `0` (not an archive) / `2`
(archive without `startup.tjs`). No warn-and-continue (D11).

The loader does not execute scripts; discovery is L2 (pure storage
semantics, testable without the VM) and execution is L3/L4.

## 6. Dynamic archives (script-mounted)

```
TJS script (L4)
  → native (L3)              "mount foo.xp3"
  → L2 Storage::mount(path)
  → L1 open + read (FileState)
  → L1 parse XP3 (stateless, no state held)
  → L2 registers the parsed index in the VFS namespace
  → later reads: L3 → L2 → L1
```

Parsing holds no state, so it is L1; L2 only ever sees a parsed file
system; L3 is just the script adapter.

**Multiple archives:** every `--archive` input is mounted at launch. Because
the folder scan is gone, scripts may also mount more at runtime through the
L3 native. (The reference `Storages` has no `mountArchive` — it relied on
folder enumeration — so this native is an intentional, documented
extension.)

## 7. Reference mapping

| reference | here |
|---|---|
| `TVPCheckStartupArg` (`environ/win32/Platform.cpp:93`): `argv[1]` is an archive containing `startup.tjs`, or a directory containing a loose `startup.tjs` | positional `GAME` argument; `--archive` for explicit lists |
| `TVPCheckArchive` (`base/impl/StorageImpl.cpp:562`): `1` has startup / `2` no / `0` not archive | loader validation |
| `TVPExecuteStartupScript` order | §5 |
| folder scan for `*.xp3` | **none in the reference**; explicit list plus positional `argv[1]` only |

## 8. Errors

Loading is explicit and typed (D11): a base directory that does not exist, a
missing or unreadable archive, an archive that fails to parse, and "no
`startup.tjs` found" are all errors, not warnings. The only tolerated case is
a missing native plugin DLL, handled by the null-plugin object in
[`08-tjs-vm-and-native-plugins.md`](08-tjs-vm-and-native-plugins.md) §4.2.

## 9. Implementation

* **L2** — [`Storage::mount_archives`] mounts an explicit, ordered archive
  list; [`Storage::startup_plan`] resolves §5 into a `StartupPlan`
  (`patch`/`startup`/`fallback`/`after`) with pure storage lookups. `patch.tjs`
  and `AfterStartup.tjs` are app-path only (disk, `find_disk`); `startup.tjs`
  and `system/Initialize.tjs` are searched (`exists`).
* **L3** — `compat::exec_storage(engine, storage, name)` is the core of
  `Scripts.execStorage` without the native wrapper.
* **L4** — `app::GameHost::open(host, archives, base_dir)` mounts the list,
  binds the VM, and runs §5's order: `patch.tjs` (non-fatal), `startup.tjs`
  with the `system/Initialize.tjs` fallback, then `AfterStartup.tjs`
  (non-fatal).
* **CLI** — `krkr [--archive XP3 ...] [--base-dir DIR] [GAME]`. The base
  directory defaults to the first archive's parent (else the current
  directory); a positional directory sets it, a positional file is mounted as
  an archive. **Nothing scans a folder for `*.xp3`.** The `boot_game` test
  derives an archive list from `KRKR_TEST_GAME` for convenience only; the
  product path never does.

[`Storage::mount_archives`]: ../../crates/engine/src/storage.rs
[`Storage::startup_plan`]: ../../crates/engine/src/storage.rs
