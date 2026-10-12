# Android launcher — storage, persistence & lifetime audit

Read-only audit of the launcher's SAF resolution, path caching, JSON persistence,
pending-launch protocol, static-field lifetime and FileProvider wiring.

**Pinned against:**

* Kotlin/manifest/resources: commit `b443935` (the audited files were last
  touched by `fc2b268`, "back handling, launch refusal, and state that
  survives a change" — working tree clean).
* Rust engine side (`crates/android/**`): **HEAD plus the uncommitted
  working-tree diff** (`git diff HEAD`). The tree was being modified *during*
  this audit; two of the defects below (R1, R2) are already fixed by those
  uncommitted changes. The fixes were verified by reading the diff; they are
  **not committed and not gated**, and they need the device check described at
  the end.

Method: static code reading only. No `cargo`, no Gradle, no device. Everything
that can only be confirmed on a phone is listed explicitly under "Needs a
device" and is not counted as verified.

---

## Findings ranked by user impact

| # | Finding | Where | Status |
|---|---|---|---|
| R1 | Every second game launch in one process session fails — the global logger is installed once per process and the second `android_main` treats `AlreadyExists` as fatal | `crates/android/src/lib.rs` (`init_logging`), HEAD | **Fixed in uncommitted working tree; verify + commit** |
| R2 | A mid-run `Failed` state is overwritten by an unconditional `Stopped`, so game-open/frame failures are reported to the launcher as a clean exit and shown to nobody | `crates/android/src/lib.rs` + `host.rs`, HEAD | **Fixed in uncommitted working tree; verify + commit** |
| K1 | JSON writes are not fsync'ed and the `copyTo` fallback truncates the target first — the documented "a crash or a kill during a write cannot leave a truncated library" is not actually guaranteed | `GameLibrary.kt:182-188`, `RuntimeSettings.kt:162-168`, `EngineDiagnostics.kt:107-113` | Real |
| K2 | Auto-backup is not disabled: `games.json` (paths + game names), `settings.json`, logs and `krkr_state.json` are restorable onto a *different* device, where the cached paths mean nothing | `AndroidManifest.xml:20-33` | Real |
| K3 | The cached path is preferred over the fresh resolution; when the two diverge and the cached one preflights OK, the wrong game is launched and the cache is never corrected | `MainActivity.kt:692-695`, `GameLibrary.kt:106-112` | Real (narrow triggers — see below) |
| K4 | Reconcile classifies launches by comparing wall-clock mtimes; a backward clock step between runs can flip the verdict | `MainActivity.kt:183-199` | Real (narrow) |
| K5 | `onDestroy` nulls `pendingGameDir`/`pendingGameName` while a slow-starting `android_main` may not have read them yet; the nulling also buys nothing, and the `pendingStateDir` comment justifying it is factually wrong | `KrkrGameActivity.kt:175-177`, `:61-64` | Real (narrow race + doc defect) |
| K6 | Fixed sibling temp names (`<name>.tmp`) shared by every instance of a store — not reachable today, latent for any future background writer | `GameLibrary.kt:183`, `RuntimeSettings.kt:163`, `EngineDiagnostics.kt:108` | Latent |
| K7 | `updatePath` silently discards a failed write | `GameLibrary.kt:111` | Real (minor) |
| K8 | `SafPaths` hygiene: `..` accepted unnormalized; `primaryRoot`'s fallback returns `/data/user/<u>` as if it were shared storage | `SafPaths.kt:94-99`, `:149` | Real (latent/low) |

---

## R1 — second launch in one process always fails at the logger (HIGH)

`android-activity` invokes `android_main` **once per `KrkrGameActivity`
instance**, and the launcher and the engine share one process (the manifest
declares no `android:process` — verified). So the sequence "launch game →
back out → launch another (or the same) game" runs `android_main` a second
time in a live process. At HEAD, `init_logging` (`lib.rs:245-268`) calls
`log::set_boxed_logger` and converts the inevitable `AlreadyExists` into a
startup failure:

```rust
if log::set_boxed_logger(Box::new(logger)).is_err() {
    return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, ...));
}
```

`android_main` then writes `Failed` ("cannot start the engine log: a global
logger is already installed") and returns without starting the loop
(`lib.rs:94-103`). The launcher will show "The game could not start" for every
relaunch within one uninterrupted process session — i.e. the most common
usage pattern.

**Status: fixed in the uncommitted working tree** — the `Err(_)` branch now
logs at `debug` and returns `Ok(())`, which is correct: the existing logger
already fans into the same `krkr.log`, and the one genuinely-fallible setup
(opening the log file) is done before the claim. Must be committed and
confirmed on a device.

## R2 — `Stopped` clobbers `Failed` for in-loop failures (HIGH)

At HEAD, `AndroidHost::resumed` writes `Failed` and returns `Flow::Exit` when
the game cannot be opened (`host.rs:176-184`), and `frame` does the same on a
step error (`host.rs:224-231`). But `android_main` unconditionally writes
`Stopped` after `run_loop` returns (`lib.rs:166-171`). A folder that passes
`GamePreflight` (has `startup.tjs`/`.xp3`) yet fails to open in the engine —
exactly the class the preflight cannot catch — is therefore recorded as a
**clean exit**: `reconcilePendingLaunch` takes the `else` branch
(`MainActivity.kt:230-235`), clears the marker and shows nothing. The user
sees a black flash back to the launcher with no diagnosis, and `krkr.log`'s
explanation is never surfaced.

**Status: fixed in the uncommitted working tree** — `run_loop` now returns a
`RunOutcome` carrying the first fatal error, and the terminal write reports
that failure (or `Stopped` only when no failure was recorded). The diff also
publishes `Running` after a successful `InitWindow`/`resumed`, which tightens
the reconcile window. Looks correct by inspection; must be committed and
confirmed on a device.

## K1 — persistence atomicity: no fsync, and a truncating fallback (MEDIUM)

All three Kotlin writers share one pattern (`GameLibrary.kt:180-188`,
`RuntimeSettings.kt:160-168`, `EngineDiagnostics.kt:105-113`):

```kotlin
val tmp = File(file.parentFile, "$FILE_NAME.tmp")
tmp.writeText(root.toString(2))
if (!tmp.renameTo(file)) {
    tmp.copyTo(file, overwrite = true)
    tmp.delete()
}
```

* **renameTo over an existing target is fine on Android/Linux.** `File.renameTo`
  maps to POSIX `rename(2)`, which atomically replaces an existing
  same-directory target; the "may fail if the target exists" caveat in the
  Java docs is about other platforms. For a sibling file it returns `false`
  only on genuine errors (EACCES, target-is-a-directory; EXDEV is impossible
  for siblings). So the fallback is nearly dead code — but see next point.
* **The fallback breaks the guarantee it exists for.** `copyTo(overwrite =
  true)` opens the target with truncate *first*; a kill between truncation and
  the end of the copy leaves a partial or empty `games.json`. On that path the
  comment "A kill mid-write leaves the previous library intact"
  (`GameLibrary.kt:179`) is false. Correct behaviour: if the rename fails, do
  **not** copy — delete the temp and return `false` so the caller reports
  FAILED and the *old* file survives.
* **No fsync anywhere.** `writeText` + `rename` without `FileDescriptor.sync()`
  (and without a directory fsync) survives a *process* kill (the data is in
  page cache) but not a *power* loss: the rename metadata can be committed
  while the file data is not, leaving an empty or garbage `games.json`. The
  tolerant read then reports an empty library (`GameLibrary.kt:80-98`), and
  the next `add` **persists the loss**. The project's own Rust twin already
  does this correctly — `crates/android/src/state.rs` `write_state_at` writes,
  `sync_all()`s, then renames — so the Kotlin side is the asymmetry. This
  matters: `docs/android.md` §3.1 states that losing this file "means
  re-picking every game folder, which is precisely what it exists to prevent".

## K2 — auto-backup is enabled by default (MEDIUM)

`AndroidManifest.xml:20-33` sets no `android:allowBackup="false"` (and no
backup rules), so `allowBackup` defaults to **true** and `filesDir` participates
in Android auto-backup: `games.json` (per-game display names and paths),
`settings.json`, `krkr-launcher.log`, `krkr.log` and `krkr_state.json` are
uploaded and can be restored onto a *different device*. On the new device both
`treeUri` and `path` are meaningless, and — combined with K3 — a restored
`path` that coincidentally exists there and looks like a KiriKiri game will be
launched. Fix: `android:allowBackup="false"` (side-loaded experimental app —
nothing of value to back up), or data-extraction rules excluding these files.

## K3 — cached path preferred over fresh resolution (MEDIUM-LOW)

`launchGame` (`MainActivity.kt:692-695`):

```kotlin
val cached = entry.path
val resolved = SafPaths.resolve(context, entry.uri)
var path = cached ?: resolved
```

If the cached path passes `GamePreflight`, `resolved` is never consulted and
`updatePath` never runs. The fresh resolution is only tried when the cached
path *fails* preflight (`MainActivity.kt:714-727`).

**Which divergences are actually possible.** The motivating comment ("a volume
may be remounted under a new id, or the folder renamed or moved") is wrong
about this provider set: for `com.android.externalstorage.documents` and the
Downloads `raw:` ids, `SafPaths.resolve` is a **pure function of the URI
string** (plus the per-user primary volume root). The cached path was written
from `resolve` of the same URI, so remounts, card swaps and folder moves
cannot make `cached != resolved` — both change identically or not at all.
Real divergence sources are:

1. **A hand-edited `games.json`** — the file is advertised as user-editable
   and its path is shown in the UI (`MainActivity.kt:1033`); editing `path`
   without touching `treeUri` is exactly the divergence.
2. **Backup/restore onto another device** (K2).
3. **The `primaryRoot` fallback firing on one occasion but not another** (K8).

In each case a stale-but-valid `path` wins, preflight cannot tell it is a
*different* game (it only checks "is *a* KiriKiri game"), and the wrong game
launches — the failure mode the suspicion predicted, minus the guessed
triggers.

**Minimal fix:** prefer the fresh resolution — `val path = resolved ?: cached`.
It is strictly better here: `resolve` does no I/O (the "cached because
resolving it needs a `ContentResolver` query" rationale in
`GameLibrary.kt:24-26` is factually false — only `displayName` queries), so
cached-first buys nothing but the hazard. Keep `updatePath` to refresh the
file, and surface its failure (K7).

## K4 — reconcile verdicts rest on wall-clock mtimes (LOW)

`reconcilePendingLaunch` (`MainActivity.kt:180-199`):

```kotlin
if (state == null || stateAge < markerAge) { /* "did not start" */ }
```

* **The strict `<` is correct** for the suspected immediate-failure case: an
  engine that fails at once writes `krkr_state.json` *after* the marker, in
  the same timestamp granule if anywhere. Equal mtimes therefore fall through
  to `when (state.state)` and the real `failed` message is shown. Using `<=`
  would misclassify that case as "did not start". **Suspicion refuted.**
* **A clock change can flip the verdict.** Both mtimes are wall-clock. If the
  clock steps *backwards* (NTP correction, manual change) between the previous
  run's state write and the new marker, the stale state appears *newer* than
  the marker and is misread as this run's verdict: a stale `stopped` silently
  swallows a launch that never started; a stale `failed` shows the old
  message. Narrow, but exactly the "future-dated file" hazard asked about.
* **Minimal fix:** delete (or truncate) `krkr_state.json` inside
  `EngineDiagnostics.markLaunch` (`EngineDiagnostics.kt:99-115`) before
  writing the marker. The engine writes its own state right after
  `super.onCreate` starts `android_main`, so afterwards *presence* of the
  state file alone proves the engine ran for this launch — the mtime
  comparison becomes redundant defence instead of load-bearing logic.

**Stale marker after a successful run — mostly refuted.** On a clean exit the
engine writes `stopped` right after `run_loop` returns; if reconcile runs
before that write, it sees `running` with a matching pid, keeps the marker and
shows nothing, and the *next* reconcile clears it against the now-newer
`stopped`. The only path where a successful run leaves `running`+dead-pid is a
process death in the milliseconds between activity destroy and the terminal
state write (or a lost terminal write after power loss) — after which the
"stopped unexpectedly" dialog is at worst a fair description ("Android may
have killed it"). The background-kill (LMK) case is the *intended* report.
Interaction note: the second-launch guard added in `fc2b268`
(`MainActivity.kt:737-771`) keys off the same state file, so a stale `running`
state whose terminal write was lost would also make the guard refuse all new
in-process launches until the process restarts — acceptable, but worth knowing.

**The pid check is meaningful exactly as suspected — confirmed.** There is no
`android:process` in the manifest, launcher and engine share one process, and
`state.rs` writes `std::process::id()`. So `state.pid == myPid()` is true
whenever the writing process is *still* the current one; the check distinguishes
"engine alive" from "crashed/killed" only because a crash or kill restarts the
process with a new pid. An in-process engine death without process death is
effectively ruled out: a Rust panic in `android_main` unwinds into the C
glue's `extern "C"` boundary and aborts the process. The check is sound, but
it means "the engine thread last reported alive *in this process*" — not a
liveness probe.

## K5 — static-field lifetime (LOW, plus a wrong comment)

* **Stale statics surviving into a new Activity instance: refuted.** A new
  `KrkrGameActivity` instance always runs `onCreate` (which republishes all
  statics *before* `super.onCreate` starts `android_main`); an existing
  instance gets `onNewIntent`, and since `fc2b268` a cross-game intent is
  ignored rather than silently mislabelling the running engine
  (`KrkrGameActivity.kt:137-156`). After process death all statics are null
  anyway and the native side treats a null `pendingGameDir` as a typed fatal
  error (`lib.rs` startup), which is surfaced. There is no path that starts
  `android_main` without `onCreate`/`onNewIntent` having run.
* **Nulling vs. native shutdown: refuted for `pendingStateDir`.** The native
  side reads the statics exactly once, at `android_main` startup, and copies
  the state directory into a Rust `OnceLock` (`state.rs` `set_state_dir`);
  the terminal `Stopped`/failure write uses that copy, never the static. So
  the comment at `KrkrGameActivity.kt:61-64` ("the native shutdown path may
  still want the directory") describes a mechanism that does not exist — the
  static *could* be cleared safely. Doc defect (D31-flavoured): fix the
  comment either way.
* **The real race is the opposite one (K5a):** `onDestroy`
  (`KrkrGameActivity.kt:175-177`) nulls `pendingGameDir`/`pendingGameName`
  *before* `super.onDestroy()`, while the `android_main` thread may not yet
  have performed its single JNI read (a back-out within milliseconds of
  launch). The read then sees null and records `Failed` ("pendingGameDir is
  unset") for a run the user deliberately cancelled — shown as a launch
  failure. Since the nulling protects nothing (a fresh instance always
  republishes before its own `android_main` reads; nothing re-reads the
  statics), the fix is to **stop nulling** — or at most null after
  `super.onDestroy()`, accepting the residual window.
* Process-global statics while no launcher-relevant instance exists: benign —
  nothing reads them outside `android_main` startup.

## K6 — fixed temp names (LATENT)

`games.json.tmp`, `settings.json.tmp`, `krkr-launch-pending.json.tmp` are
shared by every instance of their store. The suspected concurrent collision is
**not reachable today**: every writer runs on the main thread of the single
process; `GameLibrary` and the writing `RuntimeSettingsStore` exist only in
`MainActivity`, and `KrkrGameActivity`'s store instance
(`KrkrGameActivity.kt:169`) is **read-only**. It stays a hazard for any future
background writer or second Activity writing the same store — the Rust side
already avoids it with a pid-suffixed temp (`krkr_state.json.tmp.<pid>`,
`state.rs`). Cheap hardening: `File.createTempFile("games", null, dir)`.

## K7 — `updatePath` swallows write failure (MINOR)

`GameLibrary.kt:111`: `if (updated != entries) write(updated)` discards the
`Boolean` that `write`'s own doc says callers must not ignore
(`GameLibrary.kt:170-173`). A failed refresh leaves a stale cache silently —
inconsistent with `add`/`remove`, which report `FAILED`. Return the result (or
log and propagate) so `launchGame` can tell the user.

## K8 — SafPaths hygiene (LOW/LATENT)

* **`primaryRoot` fallback is a wrong-root silent fallback** —
  `SafPaths.kt:146-150`: if `Environment.getExternalStorageDirectory()` throws,
  the fallback returns `context.filesDir.parentFile` = `/data/user/<u>`, so
  `primary:Games` silently resolves to `/data/user/<u>/Games`. Reachable only
  if the deprecated call ever throws (it normally does not), but when it does
  the user gets a misleading "Game folder not found … reinsert the SD card"
  for a path that was never shared storage. Return `null` instead — callers
  already handle "cannot be addressed as a path" properly.
* **`..` is accepted unnormalized** — `SafPaths.kt:94-99` joins
  `"$root/$relative"` verbatim. Only reachable from a hand-edited
  `games.json` (the picker never emits `..` ids), and it crosses no boundary:
  the same file's `path` field is already used verbatim, and the engine runs
  with `MANAGE_EXTERNAL_STORAGE`. Hygiene only — normalize or reject `..`
  segments.

---

## Suspicion verdicts

1. **SAF resolution — mostly SAFE.**
   * `indexOf(':')` with further colons: **safe** — first colon is the
     separator; colons inside the relative part (legal on the emulated
     volume) are preserved. Using `lastIndexOf` would have been the bug.
   * Case sensitivity: `equals("primary", ignoreCase = true)` and the
     case-permissive UUID regex are harmless over-acceptance — the authority
     is verified first, and a lowercase UUID id would only yield a
     nonexistent path that preflight reports cleanly.
   * UUID-shaped unmounted volume: **safe** — `resolve` is deliberately I/O-free,
     returns `/storage/XXXX-XXXX/...`, and `GamePreflight` turns the
     nonexistent directory into the correct "reinsert the SD card" dialog.
   * `primaryRoot` vs. the provider's `primary` id: **agree** — both denote the
     *current user's* primary volume, and `games.json` lives in the per-user
     `filesDir`, so there is no cross-user divergence. The one real defect is
     the private-data-dir fallback (K8).
   * Document (not tree) URI: **safe** — `DocumentsContract.getTreeDocumentId`
     throws `IllegalArgumentException` on a document URI; `resolve` wraps the
     call in `runCatching` (`SafPaths.kt:78-80`) and returns `null`, which
     callers turn into an explicit refusal.
   * Path traversal via `..`: accepted but not a security boundary (K8).
2. **Dual path identity — CONFIRMED in mechanism, REFUTED in the guessed
   triggers.** Cached-first + write-back-only-on-failure is real
   (`MainActivity.kt:695`), but remount/move/card-swap cannot diverge cached
   from resolved for these providers (pure function of the URI). Divergence
   needs a hand-edited file, backup restore, or the K8 fallback; then the
   wrong game launches. Minimal fix: `resolved ?: cached`.
3. **Persistence atomicity — concurrency REFUTED; fallback + fsync CONFIRMED.**
   Two instances writing the same store concurrently are not reachable today
   (main-thread-only, `KrkrGameActivity`'s store is read-only), so the shared
   temp name is latent (K6). `renameTo` atomically replaces an existing target
   on Android/Linux, so the fallback runs only on genuine rename failure — and
   there it truncates the target before copying, a partial-file window the
   comment denies (K1). No fsync: process-crash safe, power-loss unsafe, and
   the tolerant read converts corruption into a silently wiped library (K1).
4. **Pending-launch protocol — strict `<` REFUTED (it is correct); clock-flip
   CONFIRMED (narrow); stale-marker-after-success mostly REFUTED (self-heals;
   LMK report is intended); pid check CONFIRMED as believed** (works only
   because a crash restarts the process with a new pid; manifest has no
   `android:process`; in-process engine death aborts the process). Fix
   direction: delete the state file in `markLaunch` so freshness stops
   depending on mtimes (K4). *(The separate R2 — `Stopped` clobbering
   `Failed` — is the case where the protocol genuinely loses a verdict.)*
5. **Static-field lifetime — reverse hazard REFUTED** (a new instance always
   runs `onCreate` before native reads; `onNewIntent` covers `singleTask`;
   cross-game intents ignored since `fc2b268`). **Shutdown-vs-nulling race
   REFUTED** for `pendingStateDir` (native copies it at startup, never
   re-reads) — but the comment claiming that rationale is wrong, and a real
   narrow race exists in the *other* direction (K5a). Statics with no
   instance: benign.
6. **File provider — REFUTED (no defect).** `res/xml/file_paths.xml` declares
   `cache-path path="."`, which covers `cacheDir/share/krkr-logs.txt`
   (FileProvider serves everything under a declared root, subdirectories
   included), and `files-path` covers the logs. The manifest authority
   `${applicationId}.fileprovider` always equals
   `"${context.packageName}.fileprovider"` at runtime (`packageName` *is* the
   final applicationId, suffix or not; none is configured in
   `build.gradle.kts` anyway). Optional tightening: `files-path path="."`
   exposes all of `filesDir` to any future grant URI — declaring just the log
   files would be narrower.

---

## Fixes, smallest first

1. `SafPaths.kt:149` — drop the `filesDir.parentFile` fallback; return `null`.
2. `GameLibrary.kt:111` — surface the `write` result from `updatePath`.
3. `MainActivity.kt:695` — `val path = resolved ?: cached` (and fix the
   "costs a ContentResolver query" rationale in `GameLibrary.kt:24-26`).
4. `EngineDiagnostics.markLaunch` — delete/truncate `krkr_state.json` before
   writing the marker; the mtime comparison in reconcile becomes belt-and-braces.
5. `AndroidManifest.xml` — `android:allowBackup="false"`.
6. `KrkrGameActivity.kt:175-177` — stop nulling the statics in `onDestroy`;
   correct the `pendingStateDir` comment (`:61-64`).
7. All three Kotlin writers — write via `FileOutputStream`, `fd.sync()`,
   then rename; on rename failure delete the temp and return `false` (no
   `copyTo` fallback); unique temp names via `File.createTempFile`.
8. **Commit the in-flight Rust fixes (R1/R2)** — highest user impact; the
   working tree already contains them (`git diff HEAD` on
   `crates/android/src/{lib,host,state,jni_fields}.rs`); they must go through
   the main agent's gate.
9. `SafPaths.resolve` — reject or normalize `..` segments (hygiene).

## Needs a device (not verifiable from here)

* R1 end-to-end: launch → back out → relaunch in one session, with the
  working-tree fix, showing the second game actually starts and keeps logging
  to the same `krkr.log`.
* R2 end-to-end: a folder that passes preflight but fails
  `GameHost::open_with_preload` must surface "The game could not start" with
  the engine's message, not a silent clean exit.
* Actual `File.lastModified` granularity on the device's `/data` filesystem
  and how the reconcile logs behave around same-millisecond writes (analysis
  assumes ext4/f2fs ns timestamps truncated to ms by the API).
* FileProvider grant propagation through `Intent.createchooser` on the target
  SDK, and the LMK "stopped unexpectedly" dialog flow after a background kill.
* The K5a race (instant back-out between `markLaunch` and the first JNI read)
  and the stale-`running`-state guard interaction described under K4.
