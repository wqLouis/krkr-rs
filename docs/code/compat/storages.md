# `compat::natives::storages` — the `Storages` class

Source: `crates/compat/src/natives/storages.rs` · layer **L3** · class
`Storages`

`Storages` is the script's view of the **storage namespace**: the name
grammar, the auto search path, the placed-path lookup and the string helpers
that dissect a storage name.

The namespace itself is L2's; this class is the L3 adapter that applies the
TVP naming rules and marshals the results. See
[`../../code/engine/storage.md`](../../code/engine/storage.md) for the L2 side
and [`../../rules/api_rules.md`](../../rules/api_rules.md) §2.5 for the grammar.

---

## 1. Registered surface

All members are static. The base module registers thirteen; **two plugins add
more to the same class object** (see §6).

| member | behaviour |
|---|---|
| `addAutoPath(path)` | adds a directory or `archive.xp3>` prefix to the search path |
| `removeAutoPath(path)` | removes one |
| `getFullPath(path)` | the normalized storage name |
| `getPlacedPath(path)` | where a name resolves: a disk path or `archive.xp3>inner/path` |
| `isExistentStorage(name)` | whether the name resolves to a file or directory |
| `extractStorageExt(name)` | the extension, including the dot |
| `extractStorageName(name)` | the last path component |
| `extractStoragePath(name)` | the path part, including the trailing delimiter |
| `chopStorageExt(name)` | the name without its extension |
| `clearArchiveCache()` | drops the L3 placed-path cache and the L2 resolver tables |
| `getLocalName(name)` | the case-corrected local path of a disk name |
| `searchCD(label)` | returns `""` — there is no disc search |
| `selectFile(params)` | returns false — there is no file picker |

There are no properties.

---

## 2. The naming grammar

The `>` delimiter and the `file` media prefix are **L3 vocabulary**: the L2
namespace never sees them.

* `arc.xp3>inner/path` addresses a file inside an archive. The archive part
  is the disk path; the inner part is the archive-local path.
* `file://.`, `file://` and `file:` are stripped before every L2 lookup.
* `getFullPath` lowercases the disk part and folds `\` to `/`. The inner
  (in-archive) part is additionally collapsed: lowercased, `\` folded to `/`,
  runs of `/` collapsed, leading slashes dropped.

The four string helpers dissect a name on `\`, `/` **and** `>`:

| member | example |
|---|---|
| `extractStorageName("a/b/c.png")` | `c.png` |
| `extractStorageExt("a/b/c.png")` | `.png` |
| `extractStoragePath("a/b/c.png")` | `a/b/` |
| `chopStorageExt("a/b/c.png")` | `a/b/c` |

The extension scan stops at a delimiter, so `a/b.c/d` has no extension.

---

## 3. The auto path

`addAutoPath` / `removeAutoPath` require a path that ends with `/`, `\` or
`>`; anything else raises

```
Storages.addAutoPath: path must end with '/', '\' or '>'; got "<path>"
```

The path is normalized (lowercased, `\` folded) and the `file://.` prefix is
stripped before it reaches L2. An empty path raises
`Storages.<method>: empty path`.

Both members clear the L3 placed-path cache, because the answer to
`getPlacedPath` may have changed.

---

## 4. `getPlacedPath`

This is the one member with a **documented approximation**.

The reference's `TVPGetPlacedPath` returns the normalized **absolute** path it
resolved. The port returns:

* for a disk hit: the **game-directory-relative** path, normalized and
  lowercased;
* for an archive hit: `"{archive-file-name}>{in-archive-path}"` — the
  archive's real file name (not lowercased) and the L2-normalized inner path;
* for a miss: `""`.

Both forms still resolve back to the same storage through `find` / `read`, so
a script that uses the result as a storage name keeps working.

Positive results are cached per engine; `clearArchiveCache` drops the cache.

---

## 5. State ownership

| state | owner |
|---|---|
| the placed-path cache | the **per-engine dispatch state** (`CompatServices::state::<PlacedCache>()`) |
| the string scratch buffer | the **per-engine dispatch state** |
| the auto-path list, the resolution tables, the mounts | the **L2 storage** |

The L3 grammar — `strip_file_media`, the `>` delimiter, the normalization —
is L3's own; the L2 namespace is generic.

---

## 6. The plugin members

Two plugins attach to the **same** `Storages` class object:

* the `fstat` plugin adds thirty members: `fstat`, `getTime`, `setTime`,
  `getLastModifiedFileTime`, `setLastModifiedFileTime`, `exportFile`,
  `deleteFile`, `truncateFile`, `moveFile`, `dirlist`, `dirlistEx`,
  `dirtree`, `removeDirectory`, `createDirectory`,
  `createDirectoryNoNormalize`, `changeDirectory`, `setFileAttributes`,
  `resetFileAttributes`, `getFileAttributes`, `selectDirectory`,
  `isExistentDirectory`, `copyFile`, `copyFileNoNormalize`,
  `isExistentStorageNoSearchNoNormalize`, `getDisplayName`,
  `getMD5HashString`, `searchPath`, `getTemporaryName`, `currentPath`,
  `clearStorageCaches`;
* the `xp3_filter` plugin adds `setXP3ArchiveExtractionFilter` and
  `setXP3ArchiveContentFilter`.

`Storages.open` (the binary stream object) is **not** registered.

---

## 7. Errors

| shape | example |
|---|---|
| `"Storages.<member> requires N argument(s)"` | `Storages.getFullPath requires 1 argument` |
| `"Storages.<member>: compat context is not bound"` | the engine binding is missing |
| `"Storages.<member>: no storage attached"` | no L2 storage |
| `"Storages.<member>: empty path"` | an empty auto path |
| `"Storages.<member>: path must end with '/', '\' or '>'; got \"<path>\""` | an auto path with no delimiter |
| `"Storages.<member>: \"<name>\" is inside an archive and has no local name"` | `getLocalName` on an archive name |
| `"Storages.<member>: {error}"` | an inner L2 failure |

One error is raised **verbatim** from the reference: an invalid path name
raises `Invalid path name <path>` (the reference's `TVPInvalidPathName`
message), because a script may match on it. Every other L2 error keeps the
member prefix: `Storages.isExistentStorage: storage not found: x`.

---

## 8. Tests

`crates/compat/tests/storages.rs` covers the string helpers (including the `>`
delimiter), the in-archive normalization, `clearArchiveCache` without a
storage, the empty-name short circuit, the auto-path delimiter check (raised
**before** the storage check), the "no storage" errors for the storage-backed
members, the arity errors, and the inert `searchCD` / `selectFile` results.

`crates/compat/tests/storages_io.rs` covers the plugin members and asserts
that the over-scoped members (`Storages.getDirList`) are **not** registered.

The unit test inside the module asserts that the reference's
`Invalid path name` message is raised verbatim while an L2 error keeps the
member prefix.

---

## 9. Known gaps

1. **`getPlacedPath` returns a relative path** where the reference returns an
   absolute one — see §4.
2. **The archive file name in `getPlacedPath` is not lowercased**, while the
   inner path is.
3. **`searchCD` and `selectFile` are inert**: they match the reference's
   values and log a warning, but they never raise.
4. **`getLocalName` returns the script spelling unchanged** on a miss rather
   than a normalized form.
5. **`Storages.open` is not registered.**

---

## 10. Related documents

| topic | document |
|---|---|
| the L2 storage namespace | [`../../code/engine/storage.md`](../../code/engine/storage.md) |
| the storage-name grammar and the surface rules | [`../../rules/api_rules.md`](../../rules/api_rules.md) §2.5 |
| game loading, mounts and precedence | [`../../../docs.old/migration/09-game-loading.md`](../../../docs.old/migration/09-game-loading.md) |
| the L3 ↔ L2 protocol | [`../../../docs.old/migration/14-l3-l2-protocol.md`](../../../docs.old/migration/14-l3-l2-protocol.md) |
