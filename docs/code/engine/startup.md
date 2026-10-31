# `engine::startup` — startup-script discovery

Source: `crates/engine/src/startup.rs`

Pure storage discovery. The module **locates** the startup scripts; it never
executes them, so it is testable without the TJS VM.

The reference order is:

1. `patch.tjs` — project directory only, no search;
2. `startup.tjs` — searched through `Storage::find` (project storage first,
   then the auto paths, the remaining mounts and the loose app-dir fallback);
3. `AfterStartup.tjs` — project directory only, no search;
4. `system/Initialize.tjs` — the reference compatibility fallback when
   `startup.tjs` is not found.

---

## 1. Responsibility

* Resolve the three startup scripts against a [`Storage`](storage.md).
* Report which script the engine should execute and whether it is the
  `system/Initialize.tjs` fallback.

---

## 2. State it owns

None. `discover_startup` takes a `&Storage` and returns the resolved
locations; it keeps no cache, no global and no clock.

---

## 3. Public API

```rust
pub struct StartupScripts {
    pub patch: Option<Location>,
    pub startup: StartupScript,
    pub after_startup: Option<Location>,
}

pub enum StartupScript {
    Startup(Location),
    Initialize(Location),
}

impl StartupScript {
    pub fn location(&self) -> &Location;
    pub fn is_initialize_fallback(&self) -> bool;
}

pub enum StartupError {
    NoStartupScript,
    Fs(#[from] FsError),
    Storage(#[from] StorageReadError),
}

pub fn discover_startup(storage: &Storage) -> Result<StartupScripts, StartupError>;
```

---

## 4. Invariants

* **`patch.tjs` and `AfterStartup.tjs` are resolved at the project directory
  only** (`storage.app_dir()`), with no search and no archive lookup. A
  missing (or directory) entry is `Ok(None)`.
* **`startup.tjs` is searched through `Storage::find`**: the current project
  storage first (the project archive for an archive project, the project
  directory for a folder project), then the auto paths, the remaining mounted
  archives and finally the loose project-directory fallback.
* **`system/Initialize.tjs` is the fallback** when `startup.tjs` is absent.
  The caller decides whether to run the fallback from
  `StartupScript::is_initialize_fallback`.
* **Discovering neither `startup.tjs` nor `system/Initialize.tjs` is a typed
  error**, never a warn-and-continue.

---

## 5. Data flow

```
Storage (project dir + mounted archives)
   │
   ├─ project dir only ──► fs::stat(app_dir/"patch.tjs")        ──► Option<Location>
   ├─ project dir only ──► fs::stat(app_dir/"AfterStartup.tjs") ──► Option<Location>
   ├─ searched      ──► Storage::find("startup.tjs")          ──► Startup(Location)
   └─ searched      ──► Storage::find("system/Initialize.tjs") ──► Initialize(Location)
                                                       │
                                                       ▼
                                          StartupScripts
```

---

## 6. Failure modes

| failure | behaviour |
|---|---|
| neither `startup.tjs` nor `system/Initialize.tjs` exists | `StartupError::NoStartupScript` |
| a file-system operation failed while probing the project directory | `StartupError::Fs` |
| the storage resolution failed (a permission or I/O error) | `StartupError::Storage` |

A plain miss is `NoStartupScript`, never the storage variant: the absence of
a script and a failure to look are different outcomes.

---

## 7. Tests

* `crates/engine/tests/startup.rs` — the discovery order, the
  project-directory-only lookups, the fallback and the errors.

---

## 8. See also

| topic | document |
|---|---|
| the storage namespace this searches | [storage.md](storage.md) |
| the startup discovery in the architecture | [`arch/l2.md`](../../arch/l2.md) §5 |
| the game-loading rules | [`rules/arch_rules.md`](../../rules/arch_rules.md) §9 |
| the archived design trail | [`docs.old/migration/09-game-loading.md`](../../../docs.old/migration/09-game-loading.md) |
