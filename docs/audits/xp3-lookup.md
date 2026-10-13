# XP3 in-archive lookup audit

Oracle: `reference/cpp/**`. Scope: the XP3 index reader, in-archive name
handling, and the storage lookup that resolves a bare `Config.tjs`.

**Conclusion.** The XP3 reader was already correct: it reads every
`INDEX_CONTINUE` block, decodes the name field the way
`TVPXP3Archive::Init` does, and normalizes names the way
`NormalizeInArchiveStorageName` does. The `Config.tjs が見つかりません` false
negative was **not** an index or name defect. It was the auto-path model:
`Storages.addAutoPath("system/")` was stored as a disk-relative folder, while
the reference resolves a relative auto path against the current directory —
the project archive — so it indexes `data.xp3>system/` and matches a request by
its base name. With disk-only resolution, `system/Config.tjs` inside the
archive was invisible.

Reproduced on the host with `/mnt/DATA/Games/Others/test2/data.xp3` (1384
entries, one zlib index block containing `system/config.tjs`) and the repo's
own reader: the engine logged `opened …/data.xp3 (1384 entries)` and then
`storage miss "Config.tjs"` with the auto path `system/` probed as a disk
folder. The reference reaches `system/config.tjs` through the `system/` auto
path; the port did not.

---

## 1. The reference index reader (`XP3Archive.cpp`)

`tTVPXP3Archive::Init` (`:340-518`):

1. Seek to `11 + offset` and read an `i64` index offset
   (`:340-343`, `:352`).
2. Loop (`while(true)`, `:348`): seek to `index_ofs + offset` (`:353`), read a
   flag byte (`:355`), then read the block:
   * `TVP_XP3_INDEX_ENCODE_ZLIB` — an `i64` compressed size, an `i64`
     uncompressed size, then the zlib stream (`:360-390`).
   * `TVP_XP3_INDEX_ENCODE_RAW` — an `i64` size then the bytes (`:391-399`).
3. Walk the block's `File` chunks (`FindChunk` `:521-548`), each with `info` /
   `segm` / `adlr` sub-chunks, and **append** each item to one `ItemVector`
   (`:424-496`); `Count` is the total across all blocks (`:495`).
4. Read the next index offset from the stream position right after the block's
   data and repeat while `TVP_XP3_INDEX_CONTINUE` (`0x80`) is set
   (`:500-505`).
5. Sort the merged vector by name when `normalizeName` (`:507-509`).

The chain is the merge: block 2's `File` chunks are appended after block 1's,
and the later names win in the hash table the archive builds
(`tTVPArchive::AddToHash`, `StorageIntf.cpp:608-616`).

The port reads the same blocks in the same order and appends to one map
(`crates/platform/fs/src/xp3.rs` `read_index` / `parse_index`); it also records
per-block stats (encode, `File` chunks, new names) for the miss diagnostic.

## 2. The reference name decode (`XP3Archive.cpp`)

Per `File` → `info` sub-chunk (`:424-436`):

```
flags  : u32  at info + 0
org    : i64  at info + 4
arc    : i64  at info + 12
namelen: i16  at info + 20          (:431)
name   : namelen UTF-16LE units at info + 22
         TVPStringFromBMPUnicode((const tjs_uint16 *)(info + 22), len)  (:432-433)
```

* `namelen` is a **signed 16-bit count of UTF-16 code units**, not bytes.
* `TVPStringFromBMPUnicode` (`StorageIntf.cpp:48-78`) reads exactly `namelen`
  units (a negative `maxlen` other than `-1` is passed through unguarded; the
  port rejects a negative length as a corrupt index).
* `TVP_XP3_FILE_PROTECTED` (bit 31, `XP3Archive.h:76`) gates extraction, not
  name decoding (`:425-427`).
* `NormalizeInArchiveStorageName` (`StorageIntf.cpp:566-606`) lowercases ASCII
  `A-Z`, maps `\` to `/`, collapses runs of `/`, and **drops leading slashes**
  (the `ptr != org_ptr` guard). It does not strip a trailing space, a NUL, or
  fold non-ASCII / full-width characters.
* Requests are normalized again before the hash lookup
  (`TVPIsExistentStorageNoSearchNoNormalize`, `:737-764`;
  `tTVPArchive::IsExistent` / `CreateStream`, `:630-645`).

The port's decode and normalizer match field for field
(`crates/platform/fs/src/xp3.rs` `parse_file_chunk`,
`normalize_in_archive_name`); the archive fixture tests below pin that.

## 3. The reference auto-path table (the actual defect)

* `TVPBeforeSystemInit` sets the current directory to the project, appending
  `>` when the project is an archive
  (`reference/cpp/core/base/impl/SysInitImpl.cpp:160-165`).
* `TVPAddAutoPath` (`StorageIntf.cpp:921-937`) normalizes the path with
  `TVPNormalizeStorageName`, which prepends `CurrentPath` to a relative path
  (`:366-367`). So `Storages.addAutoPath("system/")` for an archive project
  becomes `file://./…/data.xp3>system/`.
* `TVPRebuildAutoPathTable` (`:959-1052`) indexes the auto paths **once**:
  * archive form (`>` present, `:975-1025`): open the archive, use
    `GetFirstIndexStartsWith` (`:648-680`) on the sorted names, and register
    only **direct children** — a name with another `/` after the prefix is
    skipped (`:1001-1003`); the key is the **base name**
    (`TVPExtractStorageName`, `:997-998`).
  * folder form (`:1026-1044`): list the folder and key each file by its name.
* `TVPGetPlacedPath` (`:1061-1105`) then: cache lookup, normalize, try the name
  as-is (`:1081`), else look up the request's **base name** in the table
  (`:1088-1091`) and return `auto_path + basename` (`:1093`). A hit is cached;
  a miss is not (`:1101`).

The port stored `system/` verbatim and resolved it as a disk folder, so the
archive's `system/Config.tjs` was never a candidate. That is the bug.

### Fix

* `Storage::set_project` takes the project path as an explicit input from the
  composition root and applies the reference's decision: a path that exists as
  a storage (a mounted archive, or a regular file) is an archive project;
  otherwise it is a folder project (`SysInitImpl.cpp:160-165`). It is never
  inferred from mount order.
* The composition root resolves the path:
  * explicit `--archive` list: the archive holding the boot scripts
    (`startup.tjs` or `system/Initialize.tjs`), else the first named archive;
  * directory launch (D35): the conventional boot archive `data.xp3` if
    present, else the boot-script archive, else the directory itself.
* `Storage::add_auto_path` / `remove_auto_path` normalize a relative path
  against an archive project (`current_storage_path`), producing the
  reference's `data.xp3>system/` form; a folder project, an absolute path, or
  a path that already names an archive is used as given.
* `Storage::find` caches a resolved hit and not a miss (the reference's
  `TVPAutoPathCache` semantics).
* The archive form registers **direct children only**
  (`collect_archive_children`), keyed by base name.

## 4. The failure diagnostic

Every archive miss now logs the archive's entry count, the index sections read
and the names each contributed, and a bounded set of near misses (path match,
base-name match, stem match), capped at 8 with the total shown.

For the failing archive the pre-fix line would have been:

```
archive /mnt/DATA/Games/Others/test2/data.xp3 entry "config.tjs": 1384 entries,
1 index section(s) [zlib 1384 names (+1384)]; nearest 2/2: "system/config.tjs",
"system/updateconfig.tjs"
```

which names the answer directly.

## 5. Tests (reference citations)

`crates/platform/fs/tests/xp3.rs` (fixtures built from the reader's layout,
not from our writer):

* `plain_index_resolves_a_known_name` — raw index, a known name resolves
  (`XP3Archive.cpp:392-399`, `:424-436`).
* `zlib_index_resolves_a_known_name` — zlib index, a known name resolves
  (`:360-390`).
* `two_index_sections_resolve_names_from_both` — `INDEX_CONTINUE` chain, a name
  unique to the second block resolves and both blocks are reported
  (`:348-505`).
* `stored_case_and_backslash_resolve`, `stored_leading_slash_resolves` — the
  normalizer (`StorageIntf.cpp:566-606`).
* `near_misses_report_path_and_base_name_matches` — the diagnostic's rules.

`crates/engine/tests/storage.rs`:

* `relative_auto_path_resolves_inside_a_mounted_archive` — the regression:
  `system/` resolves the bare `Config.tjs` to `system/config.tjs`
  (`SysInitImpl.cpp:160-165`, `StorageIntf.cpp:921-937`).
* `project_archive_is_not_the_first_mounted` — with `bgimage.xp3` sorted before
  `data.xp3`, the explicit project still wins (`SysInitImpl.cpp:160-165`).
* `no_project_leaves_a_relative_auto_path_disk_relative` — a re-introduced
  mount-order inference fails this test.
* `folder_project_resolves_a_relative_auto_path_on_disk` — the folder branch.
* `both_auto_path_forms_resolve_a_bare_name` — plain `system/` and
  `data.xp3>system/` (`StorageIntf.cpp:979-1013`, `:1088-1093`).
* `auto_path_registers_direct_children_only` — a deeper name does not resolve
  by base name (`:1001-1003`).
* `a_resolved_hit_is_cached_and_a_miss_is_not` (`:1101`).
* `archive_miss_reports_near_misses` — the diagnostic is covered.

Existing tests whose relative folder auto path had relied on disk resolution
while an archive was mounted now use an absolute path, which is what the
reference does for a disk folder (`resolver_cache_lists_each_auto_path_once`,
`later_added_auto_path_wins_over_a_mounted_archive`).

`crates/app/src/game.rs` (the composition root's project rule):

* `directory_project_prefers_data_xp3` — `data.xp3` wins over the
  case-insensitively sorted `bgimage.xp3`.
* `directory_project_falls_back_to_the_boot_script_archive`,
  `directory_project_falls_back_to_the_folder` — the remaining order.
* `explicit_project_uses_the_named_archive`.
All cite `SysInitImpl.cpp:160-165`.

## 6. What remains unverified

* The reworked binary has not been run on this host; the fix is proven by the
  tests and the reference citations above. A device (or host) run of
  `/mnt/DATA/Games/Others/test2` must confirm the startup proceeds past
  `Config.tjs` and that `system/Initialize.tjs` loads `Config.tjs`.
* The project path is explicit. A directory launch prefers `data.xp3`; the
  reference is only known to exercise the launcher-passed archive, so a game
  whose boot archive is named otherwise relies on the boot-script fallback or
  on an explicit `--archive`.
* The failed device run had no copy of every archive, so whether some other
  title's missing file is a genuinely absent member or another resolution gap
  is not decided here; the near-miss line makes the next occurrence decidable.
* A stored name containing an unpaired UTF-16 surrogate is decoded with U+FFFD
  by the port, while the Windows reference keeps the raw unit; this is not
  exercised by any archive inspected here.
