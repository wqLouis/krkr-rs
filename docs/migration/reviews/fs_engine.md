# Review — `fs` (L1 fs + XP3) and `engine` (L2 VFS + startup)

Scope:

- `crates/platform/fs/` — the stateless `FileSystem` / `FileState` service, the
  host and in-memory backends, and the XP3 read+parse moved into L1.
- `crates/engine/src/storage.rs` — the L2 storage namespace / VFS.
- `crates/engine/src/startup.rs` — startup-script discovery.
- their integration tests under `tests/` (no inline test modules).

Reviewed against:

- `docs/migration/02-target-architecture.md` §5 (revised D6) and §9 (D9/D11);
- `docs/migration/09-game-loading.md` (D15: explicit loading, precedence,
  startup order, typed errors);
- `docs/migration/06-file-system.md` (the L1/L2 split).

Method: read-only, plus `cargo check -p fs --all-targets` and
`cargo check -p engine --all-targets` (both clean). No tests were run.

Severity legend: **Critical** (breaks an invariant / exploitable) · **High**
(likely bug or clear design break) · **Medium** (correctness or cost risk) ·
**Low** (quality).

> **Resolution status (current tree).** Every finding in §1 has since been
> addressed:
> * §1.1 — no chain cap; the parser tracks visited index offsets and returns
>   `Xp3Error::CorruptIndex("index chain cycle")` (`fs/src/xp3.rs:576-594`).
> * §1.2 — `parse_file_chunk` requires `info`, `segm` and `aldr` and rejects a
>   missing one (`fs/src/xp3.rs:663-746`).
> * §1.3 — `Storage::find` returns `Result<Option<Location>, StorageReadError>`;
>   only `NotFound`/`NotADirectory` map to `Ok(None)` (`engine/src/storage.rs:754`).
> * §1.4 — the `debug_assert_eq!` was replaced by a typed `CorruptIndex`.
> * §1.5 — the auto-path lookup requires a non-directory and uses the same
>   resolution helper (`engine/src/storage.rs:787-819`).
> * §1.6 — `unmount_archive` resolves against `game_dir`; bare-name matching is
>   allowed only when it identifies exactly one archive
>   (`engine/src/storage.rs:649-690`).
> * §1.7 — `find_xp3_offset` carries the overlap across every read, not only a
>   full chunk (`fs/src/xp3.rs:864-894`).
> * §1.8 — `Xp3Archive::open` maps only `UnexpectedEof` to `NotAnXp3` and keeps
>   other I/O errors (`fs/src/xp3.rs:205-216`).
> * §1.9 — `collect_disk_entries` returns `Result`, propagating non-absence
>   errors (`engine/src/storage.rs:1618-1630`).
> * §1.10 — `exists` is directory-aware (`engine/src/storage.rs:1446-1452`),
>   matching `TVPIsExistentStorage`; `crates/engine/tests/storage.rs:353-365`
>   asserts it.
> * §2 — the legacy `crates/xp3` parser is gone, and doc 06 now places the XP3
>   parse in L1.

---

## 0. Verdict

The move of the XP3 reader into L1 is done correctly for the D6 contract:
`Xp3Archive` holds only parsed index metadata (base offset + entry map), takes
the owning `FileSystem`/`FileState` for every read through positional
`read_at`, and keeps no file handle or cursor of its own. `Storage` owns the
`FileState`s and the mount list. Both crates are `#![forbid(unsafe_code)]` +
`#![warn(missing_docs)]`, contain no globals/statics/caches, keep all tests in
`tests/`, and D15 is largely right: no directory scan in the load path,
later-mounted wins, `patch.tjs`/`AfterStartup.tjs` are app-path-only,
`startup.tjs` is searched, and a missing startup is a typed error. The
size-handling (`try_reserve`, checked offset arithmetic, `usize::try_from`) is
carefully done.

The findings below were the snapshot at review time; the resolution status
above records how each was addressed. The size-handling (`try_reserve`,
checked offset arithmetic, `usize::try_from`) is carefully done.

---

## 1. Findings (ordered by severity)

### 1.1 `read_index` has a hardcoded `chain > 256` cap and no cycle detection — High

`crates/platform/fs/src/xp3.rs:299-305`

```rust
let mut chain = 0usize;
loop {
    chain += 1;
    if chain > 256 {
        return Err(Xp3Error::IndexChainTooLong(chain));
    }
```

Why it is wrong:

- **D11 (no hardcoded limits):** `256` is arbitrary. A legitimate archive with
  more than 256 chained index blocks (`INDEX_CONTINUE`) is rejected even though
  the reference loop has no bound (`XP3Archive.cpp:348`, `while(true)`).
- **Correctness:** the loop has no cycle detection. A corrupt index whose
  `CONTINUE` block points back at an earlier offset is only stopped by the
  arbitrary cap and then reported as "chain too long", which is the wrong
  diagnosis. Without the cap (the principled fix) such an archive would loop
  forever, so the cap is masking, not fixing, the bug.

Fix: delete `IndexChainTooLong`/`chain > 256`. Track the set of index offsets
already visited (`BTreeSet<u64>` keyed by the resolved `base + index_ofs`) and
return a typed `CorruptIndex("index chain cycle")` on a repeat. A real chain is
finite, so this is both unbounded and terminating, matching D11 and the
reference.

### 1.2 `parse_file_chunk` silently defaults missing required sub-chunks — High

`crates/platform/fs/src/xp3.rs:377-452`

`parse_file_chunk` initializes `flags/org_size/arc_size/raw_name/segments/
file_hash` to defaults and only fills them when it happens to see the
`info`/`segm`/`aldr` tags. A `File` chunk with no `segm` (or no `info`, or no
`aldr`) is inserted as a real entry. The reference requires all three and
throws `TVPReadError` when any is missing (`XP3Archive.cpp:418, 447, 485`).

Consequences:

- An entry can have `org_size > 0` from `info` but **zero segments**;
  `Xp3Archive::read` then returns an empty `Vec` while `Storage::stat` reports
  a nonzero size. That is a silent fallback (D11) and a data-integrity bug.
- A `File` chunk with no `info` inserts an entry whose normalized name is `""`
  (and `Storage::find("")` can even resolve it through the archive branch),
  which is corruption made addressable.

Fix: after the sub-chunk loop, require that `info` and `segm` (and `aldr`, to
match the reference) were each seen exactly once; otherwise
`return Err(Xp3Error::CorruptIndex("File chunk missing info/segm/aldr"))`.
Reject an empty `segm` payload as corrupt rather than producing a zero-segment
entry.

### 1.3 `Storage::find` swallows every `FsError` and returns `Option` — High

`crates/engine/src/storage.rs:277-340` (disk probes at `:391`,
`:397`, `:411-434`), consumed by `read` `:447`, `stat` `:473`, `exists`,
and `discover_startup` at `crates/engine/src/startup.rs:84-89`.

`find_disk_relative` uses `fs::stat(...).is_ok_and(|m| !m.is_dir)` and
`find_disk_case_insensitive` uses `fs::list(...).ok()?`. Every error —
`PermissionDenied`, `Io`, an unrepresentable path — is collapsed into `None`.
`Storage::read` then maps the result to `StorageReadError::NotFound`, and
`discover_startup` turns a permission failure reading `startup.tjs` into
`StartupError::NoStartupScript`.

Why it is wrong: doc 09 §8 and D11 require missing/unreadable archives and
failing probes to be typed errors. Treating an inaccessible file as absent is
exactly the warn-and-continue fallback D11 forbids, and it produces misleading
"not found" errors on the load path.

Fix: make resolution fallible, e.g.
`fn find(&self, name: &str) -> Result<Option<Location>, StorageReadError>`
(or a small `ResolveError`). Map `FsError::NotFound`/`NotADirectory` to
`Ok(None)` and propagate everything else. Thread that through `read`/`stat`/
`exists` (the latter may keep `exists -> bool` by treating errors as an error
surface, or gain a `try_exists`), and make `discover_startup` propagate the
error instead of returning `NoStartupScript`.

### 1.4 `read_entry` trusts `entry.org_size`; mismatch is only a `debug_assert` — Medium

`crates/platform/fs/src/xp3.rs:242-283`

`expected` is computed from the segment sizes (raw → `arc_size`, zlib →
`org_size`) and `out.len()` is guaranteed to equal `expected`, but the code
only checks it against the header `entry.org_size` with
`debug_assert_eq!(out.len() as u64, entry.org_size)` at `:283`. In release a
structurally inconsistent archive silently returns a `Vec` whose length
disagrees with the `org_size` that `Storage::stat` reports from the same
entry.

Fix: replace the `debug_assert_eq!` with a real comparison and
`Xp3Error::CorruptIndex("segment sizes disagree with info.org_size")`. (This
also makes the `org_size`/`arc_size` fields on `Entry` trustworthy.)

### 1.5 Auto-path disk lookup ignores `is_dir` and case-insensitive resolution — Medium

`crates/engine/src/storage.rs:329-334`

```rust
let candidate = self.game_dir.join(dir).join(base);
if fs::stat(&self.fs, &FsPath::host(&candidate)).is_ok() {
    return Some(Location::Disk(candidate));
}
```

`is_ok()` accepts a directory as a resolved file, so `find` can return a
`Location::Disk` for a directory and a later `read` fails with an opaque I/O
error. It also bypasses the normalized/exact/case-insensitive walk that every
other disk hit goes through, so an auto-path hit is case-sensitive while the
rest of the namespace is not.

Fix: require `is_ok_and(|m| !m.is_dir)` and route the auto-path candidate
through the same resolution helper (`find_disk_relative` on the joined
relative name) so behaviour is uniform.

### 1.6 `unmount_archive` falls back to base-name matching and can remove the wrong archive(s) — Medium

`crates/engine/src/storage.rs:227-247`

When the supplied path has a `file_name()`, the `retain` predicate removes
**every** mounted archive whose `file_name` matches case-insensitively, even if
the supplied path is in a different directory. Two archives named
`data.xp3` in `a/` and `b/` cannot be unmounted independently, and
`unmount_archive("b/data.xp3")` also removes `a/data.xp3`.

Fix: resolve the argument against `game_dir` (as `mount_archive` does) and
retain only on exact path equality. If base-name matching is genuinely wanted
for the native surface, do it explicitly (return an error on ambiguity) rather
than silently removing all matches.

### 1.7 `find_xp3_offset` only overlaps full-chunk reads — Medium

`crates/platform/fs/src/xp3.rs:542-566`

The 16-byte rewind that catches a magic straddling a read boundary runs only
when `n == CHUNK` (`:561`). `read_at` on the host backend may legitimately
return a short read before EOF, and the memory backend returns short reads at
EOF; an 11-byte magic that straddles such a boundary is then missed entirely.

Fix: always carry the last 10 bytes into the next iteration (or rewind by 10
bytes after *every* non-empty read), not just when the buffer is full. A
simple `Vec<u8>` carry buffer is clearer than seek arithmetic.

### 1.8 `Xp3Archive::open` erases non-EOF errors while reading the magic — Medium

`crates/platform/fs/src/xp3.rs:177-179`

```rust
reader
    .read_exact(&mut magic)
    .map_err(|_| Xp3Error::NotAnXp3)?;
```

`read_exact` returns `UnexpectedEof` for a file shorter than 11 bytes (fine to
call "not an XP3"), but it also carries the `FsError`/`Io` variants produced by
the adapter for a genuine read failure. Those are erased into `NotAnXp3`,
which is the same class of error-loss the crate otherwise works hard to avoid
(see the `Xp3Error::Fs` recovery test).

Fix: inspect the `io::ErrorKind`; map only `UnexpectedEof` to `NotAnXp3`, and
run the rest through the existing `Xp3Error::from(io::Error)` conversion so
`FsError`/`Io` survive. Report a short-but-nonempty file distinctly if desired.

### 1.9 `collect_disk_entries` silently swallows all list errors — Medium

`crates/engine/src/storage.rs:630-641` (via `file_list`)

```rust
let Ok(entries) = fs::list(&self.fs, &FsPath::host(dir)) else {
    return;
};
```

`Storage::list` is careful to ignore only `NotFound`/`NotADirectory`
(`:526`) and to propagate anything else, but the recursive walk used by
`file_list` discards every failure, so a permission or I/O error yields a
silently truncated file list. That is a D11 fallback and a wrong result
presented as a complete one.

Fix: make `collect_disk_entries` return `Result<_, StorageReadError>` and let
`file_list` propagate it (only `NotFound`/`NotADirectory` may be treated as
an empty subtree); or document the truncation explicitly and return it as an
error.

### 1.10 `find`/`exists` cannot resolve directories — Medium

`crates/engine/src/storage.rs:363-364`, `:370-371`, `:411-434`

Both the direct and the case-insensitive disk probes require `!is_dir`, so
`Storage::find("savedata")`/`Storage::exists("savedata")` are `false` even when
the directory exists. The reference `TVPIsExistentStorage` reports directories
as existing, and scripts commonly probe directory existence.

Fix: decide explicitly. If `find` is file-only, add a separate
directory-aware existence probe used by `exists`/`Storages.isExistentStorage`
(e.g. `stat`-based rather than `find`-based), or carry an `is_dir` flag on the
resolved location. Do not leave `exists` implying the reference semantics while
`find` cannot express them.

---

## 2. Smaller observations (not ranked)

- `crates/platform/fs/src/xp3.rs:352-373` and `:377-395`: the chunk loops stop on
  `data.len().saturating_sub(pos) >= 12`, so trailing bytes (a truncated chunk
  header or payload) are silently dropped. The reference also stops searching,
  but D11 argues for a `CorruptIndex` when `pos != data.len()` after the loop.
- `crates/platform/fs/src/lib.rs:209-213`: `lock()` recovers from a poisoned mutex
  and keeps using the `MemoryStore`. This is documented, but it can observe a
  half-applied mutation after a panic. A typed error (or `expect`) would be
  more honest for a test/wasm backend.
- `crates/platform/fs/src/xp3.rs:283` compared with `Storage::stat`
  (`storage.rs:488`) is the only place the two size notions can diverge; see
  1.4.
- Tests: there is no test for a chained (`INDEX_CONTINUE`) index, a `File`
  chunk missing `segm`/`info`/`aldr`, or an `org_size`/segment-sum mismatch —
  the exact cases behind 1.1, 1.2 and 1.4. The `xp3_builder` helper is also
  duplicated verbatim in `crates/platform/fs/tests/common/` and
  `crates/engine/tests/common/`.
- `docs/migration/06-file-system.md:44-52` said the middle layer owns the
  archive containers (`xp3`); that section now matches the revised D6 and the
  code (L1 owns the parse).
- The legacy `crates/xp3` crate (with a near-identical parser) is gone.

---

## 3. What is correct (spot checks)

- D6: `Xp3Archive` (`fs/src/xp3.rs:159-165`) stores only `base` + the
  `BTreeMap`; reads take `&FileState` and use positional `read_at`; no handle
  or cursor is hidden in L1. L2 (`MountedArchive`) owns every `FileState`.
- D15 precedence: `find` iterates `archives.iter().rev()` (`storage.rs:312`),
  so later mounts win, and `mount_archive` never scans the directory. Tests
  `later_mounted_archive_wins` and `mounting_does_not_scan_for_archives` cover
  both.
- Startup order (`startup.rs:80-99`): `patch.tjs` app-path-only,
  `startup.tjs` searched, `AfterStartup.tjs` app-path-only,
  `system/Initialize.tjs` fallback, `NoStartupScript` typed error — matching
  doc 09 §5 and the reference `TVPExecuteStartupScript`.
- Overflow/allocator discipline: checked offset math, `try_reserve(_exact)`,
  `usize::try_from`, and typed `CorruptIndex` errors throughout the parser are
  the right D11 shape.
