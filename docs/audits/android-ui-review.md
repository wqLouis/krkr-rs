# Audit — Android launcher Compose/Material 3 frontend

Status: read-only audit of the launcher's user-facing surface. No source was
modified.

Scope: `android/app/src/main/kotlin/dev/krkr/rs/**` (MainActivity.kt,
ui/Theme.kt, GamePreflight/EngineDiagnostics/GameLibrary/SafPaths/
RuntimeSettings/KrkrGameActivity/LauncherLog),
`android/app/src/main/AndroidManifest.xml`,
`res/values/{themes,strings,colors}.xml`, `res/xml/file_paths.xml`, with
`android/README.md` and `docs/android.md` as the documented intent.

Method: static reading only — no `cargo`, no `gradle` (no SDK/NDK on this
host, and AGENTS.md §0 forbids subagents from building). Anything that can only
be observed running is called out as **unverified: needs a device** rather than
guessed.

---

## Findings

### F5 (low-medium) — Every user-facing string is hardcoded English

**Confirmed.** `res/values/strings.xml` contains exactly one string
(`app_name`); there is **zero** use of `stringResource` / `R.string` anywhere
in the Kotlin. All UI text lives as Kotlin literals in `MainActivity.kt`
(TopAppBar labels, the FAB, the permission dialog and its explainer, the
removal dialog, the startup-issue and launch-blocked messages, the Logs screen,
every snackbar, the whole Settings screen, `EmptyLibrary`, the `GameCard`
remove label, the share/copy subject), `RuntimeSettings.kt`
(`FRAME_RATE_RANGE`, the preload description) and `GamePreflight.kt` (all four
issue titles and messages).

**Is it a gap?** Yes, given the documented audience: the repo ships
`README.zh-CN.md`, and the games this launcher exists to run are
Japanese/Chinese titles (the doc's own example library entry is `不可视之药`,
`docs/android.md` §3.1). The one screen every user must pass — "All files
access is required", "Add a folder containing a KiriKiri game" — is
English-only, and there is no `values-zh` to fall back to because there are no
resources at all. Extraction to `strings.xml` is mechanical now and gets
harder the longer the literals accumulate.

### F7 (low) — Theme/window: fixed Light platform theme, no status-bar appearance handling in MainActivity

**Where:** `res/values/themes.xml` — `Theme.Krkr` parents
`@android:style/Theme.Material.Light.NoActionBar` unconditionally and its
window background is `@android:color/background_light`; `MainActivity` never
touches window-inset appearance (only `KrkrGameActivity.kt:140` does, and it
hides the bars). Consequences:

- The Compose UI follows dark mode (`ui/Theme.kt`, `isSystemInDarkTheme`), but
  the *window* is always the Light theme: the window background is white and
  the status/navigation-bar icon appearance follows the light theme. In dark
  mode the launcher can therefore draw dark surfaces under dark bar icons and
  flash white on start.
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

`MainActivity.kt` is ~1 370 lines and holds the library, settings and logs
screens plus every dialog. Splitting those composables into their own files is
mechanical and lowers the cost of the lifecycle work above.

---

## What is *not* broken (checked so it does not get "fixed" later)

- System back while a sub-screen is open returns to the library
  (`BackHandler`, `MainActivity.kt:279`).
- A second launch while the engine runs is refused with a log line instead of
  silently resuming the old game (`KrkrGameActivity.onNewIntent`).
- The SAF persist-permission double-try: a read+write request that fails retries
  read-only instead of losing the grant — correct, and it tells the user when a
  grant cannot be persisted.
- Library/settings file handling: atomic write + tolerant read + duplicate-key
  dedup before `LazyColumn` keys (`GameLibrary.kt:135-158`) — the duplicate-key
  crash it prevents would have been a startup crash on every launch.
- `GameCard`/dialog strings interpolate user data, no format-string hazards.
- The archive-preload `Switch` carries a merged `contentDescription`
  ("Archive preload"), so TalkBack announces it.

---

## Unverified — needs a device

- Back behaviour while a Compose `AlertDialog` is open (library-handled, not
  executed).
- F7 status/navigation-bar icon legibility in dark mode, and actual inset
  behaviour of `Scaffold` on a cutout device.
- Share-sheet grant propagation through `Intent.createChooser` to specific
  receiver apps.
- TalkBack announcements exactly as spoken (node structure analysed statically).
