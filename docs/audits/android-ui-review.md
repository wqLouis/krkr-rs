# Audit — Android launcher Compose/Material 3 frontend

Status: read-only audit. No source was modified. HEAD at time of audit:
`a22cf9b` (branch `migration/middle-layer-ash`).

Scope: `android/app/src/main/kotlin/dev/krkr/rs/**` (MainActivity.kt, ui/Theme.kt,
GamePreflight/EngineDiagnostics/GameLibrary/SafPaths/RuntimeSettings/KrkrGameActivity/LauncherLog),
`android/app/src/main/AndroidManifest.xml`, `res/values/{themes,strings,colors}.xml`,
`res/xml/file_paths.xml`, with `android/README.md` and `docs/android.md` as the
documented intent.

Method: static reading only — no `cargo`, no `gradle` (no SDK/NDK on this host,
and AGENTS.md §0 forbids subagents from building). Anything that can only be
observed running is called out as **unverified: needs a device** rather than
guessed. User-visible strings and lifecycle claims were checked by exhaustively
reading the composition, not by sampling.

---

## 1. Findings, ranked by user impact

### F1 (high) — System back from Settings/Logs finishes the Activity instead of returning to the library

**Where:** `android/app/src/main/kotlin/dev/krkr/rs/MainActivity.kt:254-255`
(`showLogs`/`showSettings` are plain `remember` booleans),
`MainActivity.kt:333-341` (the TopAppBar arrow is the *only* back affordance),
`MainActivity.kt:362-373` (the sub-screen switch). There is **no `BackHandler`
and no `NavHost` anywhere in `android/app/src`** (grep: zero hits for
`BackHandler`, `NavHost`, `OnBackPressed`, `onBackInvoked`), and no
`android:enableOnBackInvokedCallback` anywhere in the manifest or Gradle files.

**Why it is wrong:** the launcher has two sub-screens that are shown/hidden by
in-memory booleans. With no back callback registered, a system back in
`showSettings`/`showLogs` falls through to the Activity's default handling and
**finishes `MainActivity`** — the app exits to the launcher/home instead of
popping to the library. The TopAppBar arrow (`MainActivity.kt:335-340`) works,
the gesture and the button do not: the two paths disagree.

This is worse than a plain "back exits" bug on this app because `targetSdk = 37`
(`android/app/build.gradle` `targetSdk = 37`): per the Android 16 behaviour
changes (developer.android.com/about/versions/16/behavior-changes-16, quoted
during this audit), for apps targeting API 36+ **on Android 16+ devices the
predictive back system animations are enabled by default, `onBackPressed` is not
called and `KeyEvent.KEYCODE_BACK` is not dispatched any more**. So on a current
device the back gesture from inside Settings previews leaving the app and then
kills it; on Android 15 (still in `minSdk = 35`..`targetSdk` range, legacy
dispatch) the result is the same finish, just without the animation. There is no
code path in which back returns to the library.

Note that dialogs are *not* affected: a Compose `AlertDialog`
(`MainActivity.kt:413-477`) owns its dialog window and handles back itself,
invoking `onDismissRequest`. The hole is exactly the two non-dialog
sub-screens — **unverified: needs a device** for the dialog case only.

**Fix shape:** one callback keyed on the sub-screen state, inside
`LauncherScreen` (where the booleans live):

```kotlin
BackHandler(enabled = showLogs || showSettings) {
    showLogs = false
    showSettings = false
}
```

(`androidx.activity.compose.BackHandler`; `activity-compose:1.13.0` is already a
dependency and bridges both the legacy `onBackPressed` dispatch and the
`OnBackInvokedCallback` dispatch, so **the fix works without any manifest
change**.) `android:enableOnBackInvokedCallback` is only about the *animations*:
it is default-on for targetSdk 36+ on Android 16+ (that is the behaviour change
above), and default-off on Android 15 — so if you want the predictive-back
animation on Android 15 devices too, add
`android:enableOnBackInvokedCallback="true"` **on the `<activity>` tag only, not
on `<application>`** — see F2 for why a global `true` would break the engine.

### F2 (high, device-dependent) — targetSdk 37 silently removes the engine's only back path

**Where:** `AndroidManifest.xml:51-62` (`KrkrGameActivity` declares no back
handling of any kind), the engine's back→Escape mapping at
`crates/platform/platform/src/android.rs:227-228, 299, 335, 366-374`
(`AKEYCODE_BACK` → `PhysicalKey::Escape`), and the design text that relies on
it: `docs/android.md:409-411` ("`android-activity` 0.6 does not surface
`onBackPressed`, so the only back path is the mapped `AKEYCODE_BACK` key").

**Why it is wrong:** the same Android 16 behaviour change quoted in F1 says
`KeyEvent.KEYCODE_BACK` is **not dispatched any more** to apps targeting API 36+
on Android 16+ devices. This app targets 37. The engine never receives the back
key, so the documented in-game substitute for a KAG menu ("press back → Escape")
is dead on those devices — and with no `OnBackInvokedCallback` registered, the
back invocation lands on `ComponentActivity`'s default handling, whose fallback
*finishes the activity*, i.e. back may exit the game outright instead of opening
the menu. The two-finger gestures (`docs/android.md` §5) keep working, which is
probably why this has not screamed yet.

This is exactly the migration the platform doc tells you to make or opt out of:
"temporarily opt out by setting the `android:enableOnBackInvokedCallback`
attribute to false in the `<application>` or `<activity>` tag".

**Fix shape:** `android:enableOnBackInvokedCallback="false"` on the
`<activity android:name=".KrkrGameActivity">` element (restores key dispatch for
the engine), keep the attribute off/`true` for `MainActivity` (it wants F1's
`BackHandler`, which works either way). Do **not** set `true` at
`<application>` level — that would re-break F2 globally.

**unverified: needs a device** — the *documented* half (no `KEYCODE_BACK`
dispatch, no `onBackPressed`) is certain from the platform contract; whether
back also finishes `KrkrGameActivity` depends on how `android-activity`'s
`GameActivity`/`ComponentActivity` fall back, which I could not execute here.

### F3 (medium) — Rotation (and every other config change) silently discards the launcher's in-memory state

**Where:** `AndroidManifest.xml:36-44` — `MainActivity` declares **no
`android:configChanges`** (only `KrkrGameActivity` does, at
`AndroidManifest.xml:58`) and **no `android:screenOrientation`** — so every
orientation change, dark-mode toggle, fold, split-screen resize and font-scale
change recreates the Activity and the whole composition. All UI state is
`remember`, never `rememberSaveable`:

| state | line | lost on recreation | consequence |
|---|---|---|---|
| `showLogs` / `showSettings` | `MainActivity.kt:254-255` | yes | open sub-screen closes; user is dumped back to the library |
| Settings draft (`frameRate`, `scaling`, `preloadArchives`) | `MainActivity.kt:764-767` | yes | **a half-finished settings edit reverts to the on-disk values with no warning** — worst case, a typed frame-rate limit or a radio choice is silently gone |
| `permissionDismissed` | `MainActivity.kt:259` | yes | "Not now" is forgotten; the All-files-access dialog pops again after a rotation |
| `gameIssue` | `MainActivity.kt:253` | yes | a preflight report ("Not a KiriKiri game", `GamePreflight.kt:64-70`) silently vanishes; the user must tap the game again to see why |
| `pendingRemoval` | `MainActivity.kt:252` | yes | the remove-confirmation dialog disappears |
| snackbar queue (`SnackbarHostState`) | `MainActivity.kt:249` | yes | transient messages ("Added …", save failures) lost |
| `refreshToken` | `MainActivity.kt:533` | yes | harmless (tails re-read from disk) |
| `games` | `MainActivity.kt:251` | re-read from disk | **survives** — `library.all()` runs again in the fresh composition; all mutations are persisted first |
| `permissionGranted`, `startupIssue` | `MainActivity.kt:100-101` | Activity fields, recreated | **re-derived**: `onCreate` re-reads the grant (`:108`), `onResume` re-runs `reconcilePendingLaunch` (`:131-136`) |
| scroll positions | `rememberScrollState` / `LazyColumn` state | no | `rememberSaveable`-backed internally — survives |

The documented intent makes this reachable: the app is declared
"**Landscape-locked**" (`android/README.md:32`) / "Locked to landscape"
(`docs/android.md:28`), but only `KrkrGameActivity` carries
`android:screenOrientation="sensorLandscape"` (`AndroidManifest.xml:56`); the
launcher rotates freely, so rotation is the *normal* trigger for every row in
the table above. Either lock `MainActivity` too (restoring the documented
decision) or save the state — saving is the better fix because it also covers
dark-mode/split-screen/process-death, none of which orientation locking helps.

**The pending-launch comment's claim is CORRECT — confirmed, not refuted.**
`MainActivity.kt:121-123` ("keeping the marker means a rotation (which re-runs
`reconcilePendingLaunch`) cannot swallow it") holds, by construction:
reconciliation is driven entirely by two on-disk files
(`krkr-launch-pending.json`, `krkr_state.json` — `EngineDiagnostics.kt:99-135`),
`onResume` always runs after a recreation (`MainActivity.kt:131-136`), and the
issue is only cleared by an explicit dismissal
(`onDismissStartupIssue`, `MainActivity.kt:119-126`). A dismissed issue stays
dismissed (marker gone), an undismissed one is re-derived identically after
rotation, and the still-running-engine case (`state.pid == myPid`,
`MainActivity.kt:212-217`) correctly keeps the marker instead of reporting.
Process death survives too (marker is a file).

### F4 (medium-low) — `launchMode="singleTask"` + static handoff: a second launch while the engine runs does nothing, and can file a mislabeled report

**Where:** `AndroidManifest.xml:55` (`launchMode="singleTask"`),
`KrkrGameActivity.kt:140-151` (`onNewIntent` refreshes `pendingGameDir` &
friends), the JNI read side `crates/android/src/lib.rs:81-127`, and the
launcher's `EngineDiagnostics.markLaunch(...)` at `MainActivity.kt:725`.

**Why it is wrong:** the native side reads the static fields **once**, at
`android_main` start (`crates/android/src/lib.rs:100-127`; the field doc in
`KrkrGameActivity.kt:44-51` says the same: "as soon as `android_main` starts").
When a second launch is delivered to `onNewIntent` of an already-running
instance, refreshing `pendingGameDir` cannot switch the game — nothing ever
re-reads it. The comment at `KrkrGameActivity.kt:134-139` implies the refresh is
what makes the new intent take effect; it does not. Meanwhile the launcher has
already written a **pending marker naming game B** (`MainActivity.kt:725`,
`EngineDiagnostics.kt:99`), so if the still-running engine A then dies, the
next-start report says *B* "did not shut down normally" (`MainActivity.kt:219-228`)
for a crash that happened to A. And the user sees no explanation at all for why
tapping game B just resumed game A — a silent nothing, the exact class the
project blocks elsewhere.

Reaching this path requires the game Activity to be alive while the launcher is
navigable — **unverified: needs a device** (back-out-of-game, recents, and
multi-window flows decide which of `onCreate`/`onNewIntent` fires). The inertness
of the refresh itself is certain from the code.

### F5 (low-medium) — Every user-facing string is hardcoded English

**Confirmed.** `res/values/strings.xml` contains exactly one string
(`app_name`, `strings.xml:2`); there is **zero** use of `stringResource` /
`R.string` anywhere in the Kotlin (grep: no hits). All UI text lives as Kotlin
literals. Surfaces, by file:

- **MainActivity.kt** — TopAppBar title/`Settings`/`Logs`/back label
  (`:326-346`), FAB `Add game` (`:357`), permission dialog incl. the long
  explainer (`:415-430`), removal dialog (`:456-476`), `IssueDialog`
  buttons (`:516-520`), all three startup-issue messages
  (`:192, 203, 219`), the launch-blocked messages (`:679-687, 744-748`),
  Logs screen `Diagnostics`/`Refresh`/`Share`/`Copy`/
  subtitle (`:540-578`), every snackbar message (`:295-315, 466-468, 910-930`),
  the whole Settings screen (`:781-940`), `EmptyLibrary` (`:947-967`),
  `GameCard` remove label (`:1012`), share/copy subject (`:614, 645`).
- **RuntimeSettings.kt** — `FRAME_RATE_RANGE` (`:46`), all four scaling
  descriptions (`:74-84`), the preload description (`:90-98`).
- **GamePreflight.kt** — all four issue titles/messages (`:36, 43, 52, 64-70`).

**Is it a gap?** Yes, given the documented audience: the repo ships
`README.zh-CN.md`, and the games this launcher exists to run are
Japanese/Chinese titles (the doc's own example library entry is
`不可视之药`, `docs/android.md` §3.1). The one screen every user must pass —
"All files access is required", "Add a folder containing a KiriKiri game" — is
English-only, and there is no `values-zh` to fall back to because there are no
resources at all. It is defensible for an experimental build, but it should be
tracked as a gap, not as a finished surface: extraction to `strings.xml` is
mechanical now and gets harder the longer the literals accumulate.

### F6 (low) — Accessibility: `outline`-coloured labels fail AA in light mode; the preload Switch has no label

**Contrast — confirmed defect, computed.** `labelSmall` text coloured
`colorScheme.outline` at `MainActivity.kt:936-938` ("Settings file: …") and
`MainActivity.kt:965-967` ("Library: …"). Against the Material 3 baseline
light surface (`#FFFBFE`), `outline` (`#79747E`) gives a contrast ratio of
**≈ 4.44 : 1** — below the 4.5 : 1 required for normal text, at 11 sp. (Baseline
dark is fine: `#938F99` on `#1C1B1F` ≈ 5.4 : 1.) Two aggravators: the app uses
**dynamic colour** (`ui/Theme.kt:23, 28`, `dynamicColor = true`), so with an
arbitrary wallpaper the outline/surface pair can land anywhere — this is not a
value the app controls; and M3 defines `outline` for borders/hairlines, never
for text — the codebase already uses the correct colour elsewhere
(`onSurfaceVariant`, e.g. `MainActivity.kt:572, 786`). Fix: use
`onSurfaceVariant` at both sites.

**contentDescription coverage — mostly correct:**
- covered: `Back` (`:339`), `Remove ${entry.name}` (`:1012`), all `TextButton`s
  carry visible text, FAB's `Add` icon is `null` but the FAB has a text label
  (`:355-358`), `PlayArrow` is decorative next to the title (`:991-992`).
- gap: the archive-preload `Switch` (`MainActivity.kt:865-884`) is its own
  semantics node; its control name ("Archive preload", `:862`) and the On/Off
  text (`:881-883`) are *separate* nodes outside it, so TalkBack announces an
  unlabelled switch. The radio rows are fine — `selectable(...)` (`:828-833`)
  merges its descendants, so the mode name is part of the node.

**Touch targets — essentially fine (refuted as a defect).** Every control is a
M3 component (`TextButton`, `IconButton`, `Switch`, `RadioButton`, `FAB`), and
M3 enforces `minimumInteractiveComponentSize` (48 dp) on them; the radio rows
(`:826-857`) are additionally full-width and two text lines tall. The only
nit: `Refresh`/`Share`/`Copy` sit `spacedBy(4.dp)` apart (`:546, 553-565`) —
adjacent targets closer together than the 8 dp M3 spacing guidance; not a
blocking issue.

### F7 (low) — Theme/window: fixed Light platform theme, no status-bar appearance handling in MainActivity

**Where:** `res/values/themes.xml:21` — `Theme.Krkr` parents
`@android:style/Theme.Material.Light.NoActionBar` unconditionally; `MainActivity`
never touches window-inset appearance (only `KrkrGameActivity.kt:120-127` does,
and it hides the bars). Consequences:

- The Compose UI follows dark mode (`ui/Theme.kt:22` via `isSystemInDarkTheme`),
  but the *window* is always the Light theme: the window background
  (`themes.xml:22`) is white, and status/navigation-bar icon appearance follows
  the light theme. In dark mode the launcher can therefore draw dark surfaces
  under dark bar icons / flash white on start.
- Edge-to-edge is enforced at targetSdk 35+ and `MainActivity` does not opt out
  (no `windowOptOutEdgeToEdgeEnforcement`), which is correct; the Compose
  `Scaffold` + M3 `TopAppBar` consume the system-bar insets by default, so no
  overlap is expected *by static reasoning*.

**unverified: needs a device** — actual bar-icon legibility in dark mode and
the exact inset behaviour cannot be confirmed without running it. If confirmed,
the fix is a couple of lines in `onCreate`:
`WindowCompat.getInsetsController(window, …).isAppearanceLightStatusBars = !dark`,
or a DayNight parent theme.

### F8 (lowest) — File organisation

See §6.

---

## 2. The six suspicions — verdicts

1. **SYSTEM BACK — CONFIRMED (F1).** `showLogs`/`showSettings` are plain
   `remember` booleans; no `BackHandler`/`NavHost` exists anywhere in the app;
   system back from Settings or Logs finishes `MainActivity` while the
   TopAppBar arrow returns to the library. With `targetSdk 37`, on Android 16+
   devices this happens over the predictive-back path (no manifest opt-out is
   present). Fix: `BackHandler(enabled = showLogs || showSettings)` inside
   `LauncherScreen`; `enableOnBackInvokedCallback` is **not** required for the
   fix to work (androidx.activity bridges both dispatch paths) — it only
   controls the animation, and if added must be **per-activity** because of F2.
2. **ROTATION/PROCESS DEATH — CONFIRMED for UI state, and the marker claim is
   VERIFIED AS CORRECT (F3).** Lost: open sub-screen, the entire in-progress
   Settings edit, `permissionDismissed` (permission dialog re-prompts),
   `gameIssue`, `pendingRemoval`, snackbar. Survives: `games` (disk), grants and
   startup issue (re-derived in `onCreate`/`onResume`), scroll positions
   (saveable). The `MainActivity.kt:121-123` comment's claim holds: rotation
   necessarily runs `onResume` → `reconcilePendingLaunch` (`:131-136, 176`), the
   marker is a file, and it is cleared only on explicit dismissal or a clean
   exit — so a crash report cannot be swallowed by a rotation. The one real
   loss in that area is the Settings draft, not the report.
3. **DIALOG PRIORITY — REFUTED.** The `else-if` chain (`MainActivity.kt:413-477`)
   always leaves every required dialog reachable: the permission gate has two
   dismiss paths and three re-open paths (`Not now`/outside-tap `:415, 429`;
   re-open via tapping a game `:389-393`, via `launchGame`'s
   `onNeedPermission` `:399`, and automatically when the grant arrives
   `:260`); the startup issue sits directly behind it and appears as soon as the
   gate is answered or granted; `gameIssue` waits behind `startupIssue`; the
   removal dialog is last and only ever set from a dialog-free screen. No state
   hides a dialog permanently. Two cosmetic notes: snackbar messages render
   *behind* an open dialog, and rotation resets `permissionDismissed` (F3) so
   "Not now" does not survive a rotation.
4. **LIFECYCLE OBSERVATION — REFUTED (no bug).** The reads
   `permissionGranted.value` (`MainActivity.kt:116`) and `startupIssue.value`
   (`:118`) occur **inside the `setContent { }` composable lambda** — a
   restartable composition scope. They are ordinary snapshot reads performed
   during composition, so they *are* registered as state reads of that scope.
   When `onResume` writes `permissionGranted.value` (`:135`) or
   `reconcilePendingLaunch` writes `startupIssue.value` (`:192, 203, 219`) on the
   main thread, the scope is invalidated and `LauncherScreen` is re-invoked with
   the new parameter values. Capturing them "as plain parameters" is just how
   unidirectional data flows into a composable; it does not sever observation.
   There is no unobserved-read defect here — the UI does update. (The only
   one-shot reads are `library.all()` in a `remember` initializer `:251` and
   `store.read()` `:764`, both by design with explicit refreshes.)
5. **ACCESSIBILITY/I18N — CONFIRMED gap (F5, F6).** English-only literals
   everywhere; `strings.xml` has `app_name` only. contentDescription coverage is
   good except the unlabelled preload `Switch`; touch targets are fine; the
   `outline` labels fail AA in the baseline light scheme (≈4.44:1) and are
   wallpaper-dependent because dynamic colour is on. Given `README.zh-CN.md` and
   a CJK-game audience, the missing localisation is a real gap, ranked below the
   navigation bugs because the app is explicitly experimental.
6. **FILE ORGANISATION — see §6.** Worth doing, lowest priority.

---

## 3. Manifest review

| item | verdict |
|---|---|
| `exported` flags | **correct.** `MainActivity` `exported="true"` (`AndroidManifest.xml:38`) — required for the `LAUNCHER` intent-filter; `KrkrGameActivity` `exported="false"` (`:54`) — must never be startable from outside; FileProvider `exported="false"` + `grantUriPermissions="true"` (`:74-75`) — correct. |
| `launchMode="singleTask"` vs. static handoff | **defect — F4.** `onNewIntent`'s field refresh (`KrkrGameActivity.kt:140-151`) cannot affect a running `android_main` (fields read once, `crates/android/src/lib.rs:81-127`); second game silently not started; pending marker may name the wrong game. The `onCreate`-before-`super.onCreate` ordering itself (`KrkrGameActivity.kt:110-125`) is correct and race-free for a cold start. `onDestroy` nulls `pendingGameDir`/`pendingGameName` but deliberately not `pendingStateDir` (`:63-67`) — documented, fine. |
| `screenOrientation` | **gap vs. documented intent.** `KrkrGameActivity` is `sensorLandscape` (`:56`) ✓ per `docs/android.md:28`; `MainActivity` has none, contradicting the app-level "Landscape-locked" claim (`android/README.md:32`) — and this is what makes F3's rotation losses routine. |
| `configChanges` | **asymmetry is right in principle, wrong for the launcher.** The game activity's broad `configChanges` list (`:58`) correctly avoids recreating the native host. `MainActivity`'s absence of it means full recreation on every config change — legitimate, *provided* the UI state is saveable, which it is not (F3). Prefer `rememberSaveable` over adding `configChanges` to the launcher (configChanges on an activity you don't redraw manually is how stale layouts happen). |
| theme / edge-to-edge | `Theme.Krkr`'s platform parent is fine for `ComponentActivity` (no AppCompat requirement — only the game activity needs one, `themes.xml:26` with the documented crash note). MainActivity does no bar-appearance handling → F7, **unverified: needs a device**. |
| FileProvider `<paths>` vs `shareLogs` | **matches — no defect.** `shareLogs` writes `cacheDir/share/krkr-logs.txt` (`MainActivity.kt:617-621`); `res/xml/file_paths.xml` declares `cache-path path="."` — covered; authority `${applicationId}.fileprovider` matches the code's `"${context.packageName}.fileprovider"` (`:624-627`). `files-path "."` is declared but never handed out (only the cache file is passed to `getUriForFile`), so nothing private is actually shareable today; the manifest comment's "only these two private directories are exposed" is accurate as to directories. The `EXTRA_TEXT` + `EXTRA_STREAM` + chooser pattern is standard — **unverified: needs a device** for the grant propagation on your target receivers. |

---

## 4. What is *not* broken (checked so it does not get "fixed" later)

- State observation of `permissionGranted`/`startupIssue` (§2.4).
- Pending-launch marker reconciliation across rotation *and* process death
  (§2.2/F3) — the on-disk design does what the comment claims.
- Dialog priority ordering (§2.3) — one dialog at a time, all reachable.
- The SAF persist-permission double-try (`MainActivity.kt:275-295`): a
  read+write request that fails retries read-only instead of losing the grant —
  correct, and it tells the user when a grant cannot be persisted.
- Library/settings file handling: atomic write + tolerant read + duplicate-key
  dedup before `LazyColumn` keys (`GameLibrary.kt:135-158`) — the duplicate-key
  crash it prevents would have been a startup crash on every launch.
- `GameCard`/dialog strings interpolate user data, no format-string hazards.

---

## 5. Fixes, smallest-first

1. **`BackHandler(enabled = showLogs || showSettings)` in `LauncherScreen`**
   — ~4 lines + one import (`MainActivity.kt`, near `:251`). Fixes F1.
2. **`android:enableOnBackInvokedCallback="false"` on the `KrkrGameActivity`
   element** — 1 manifest line. Restores the engine's documented back key path
   on Android 16+ (F2). Verify on device; keep it activity-scoped.
3. **Swap two colours:** `colorScheme.outline` → `colorScheme.onSurfaceVariant`
   at `MainActivity.kt:938` and `:967`. Fixes the AA failure in F6.
4. **Label the preload Switch** — merge the "Archive preload" row (or add
   `Modifier.semantics { contentDescription/stateDescription }` on the `Switch`,
   `MainActivity.kt:875`). F6.
5. **`rememberSaveable` for `showLogs`, `showSettings`, `permissionDismissed`**
   — 3 lines (F3 part 1). Optional: a `rememberSaveable` holder for the
   Settings draft (`:765-767`) so a rotation cannot discard an edit; the booleans
   are the "user is surprised" half, the draft is the "work is lost" half.
6. **Declare the documented orientation policy for `MainActivity`**
   (`android:screenOrientation`, `AndroidManifest.xml:36-44`) *or* amend
   `android/README.md:32` to say landscape applies to the game only. One line
   either way; do it alongside 5 so the two halves of F3 agree.
7. **Second-launch-while-running:** either refuse the launch with an issue
   dialog when the engine is alive (the state file already tells you: `running`
   + live pid, `EngineDiagnostics.kt:85-96`), or stop writing a fresh pending
   marker in that case (`MainActivity.kt:725`). Fixes F4's mislabeled report;
   the "silently nothing" part needs a device to reproduce first.
8. **Localisation groundwork:** move the literals of the four screens every user
   touches (permission gate, empty library, FAB/top bar, Settings labels) into
   `strings.xml` with `values-zh` translations — F5. Mechanical; do it before
   more strings accrete.
9. **Dark-mode bar appearance** in `MainActivity.onCreate` (or DayNight theme) —
   F7; confirm on device first.
10. **Split the UI out of MainActivity.kt** — §6.

---

## 6. File organisation (lowest priority)

AGENTS.md §7's genericity test is about the Rust layers and does not transfer to
Kotlin, so this is judged only on the project's *own* stated layout:
`docs/android.md:175` already documents the tree as

```
MainActivity.kt               library: game list, "Add game" (SAF), settings
ui/                           Compose screens, M3 theme
```

Today `ui/` holds only `Theme.kt`, while `MainActivity.kt` (1016 lines) holds
seven composables plus four free functions. That is a doc-vs-tree mismatch, not
a style preference.

The smallest split that helps, keeping every composable `private`/internal and
the Activity's own logic untouched:

- `ui/LibraryScreen.kt` — `LauncherScreen`, `EmptyLibrary`, `GameCard`,
  `IssueDialog` (they share the dialog-priority and library state);
- `ui/LogsScreen.kt` — `LogsScreen`, `LogSection`, `shareLogs`, `copyLogs`,
  `combineLogs` (self-contained; only `LauncherLog`/`EngineDiagnostics` deps);
- `ui/SettingsScreen.kt` — `SettingsScreen`.

`MainActivity.kt` keeps the Activity, `launchGame`, `reconcilePendingLaunch` and
the permission intent — i.e. everything that needs a `Context`/lifecycle. That
leaves ~450 lines where the lifecycle wiring (the part F1–F4 are about) is
readable on one screen. Larger refactors (per-file design system, navigation
wrapper) are not warranted until the back/saveable fixes land.

---

## 7. Unverified — needs a device

- Back behaviour while a Compose `AlertDialog` is open (library-handled, not
  executed).
- F2's concrete failure mode on an Android 16+ device (no `KEYCODE_BACK` is
  documented; the resulting "finish vs. swallow" depends on
  `android-activity`'s fallback).
- F4's reachability: which of `onCreate`/`onNewIntent` a real second launch
  takes from recents/multi-window/back-out-of-game.
- F7 status/navigation-bar icon legibility in dark mode, and actual inset
  behaviour of `Scaffold` on a cutout device.
- Share-sheet grant propagation through `Intent.createChooser` to specific
  receiver apps.
- TalkBack announcements exactly as spoken (node structure analysed statically).
