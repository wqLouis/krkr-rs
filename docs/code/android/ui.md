# `ui` — the launcher's Material 3 interface

Source: `android/app/src/main/kotlin/dev/krkr/rs/ui/` · the Kotlin launcher
(the `MainActivity` module of the Android app) · **unverified until an apk CI
job or a device build** (§8)

The launcher is one Activity, `MainActivity`, that owns the game library and
nothing else; the engine runs in its own Activity (`KrkrGameActivity`). This
page documents the `ui/` package: the navigation model, the screens, and
where each piece of state lives.

---

## 1. The navigation model

The launcher has three top-level destinations, and a **navigation rail** on the
left edge is the only way between them.

| destination | screen | app bar |
|---|---|---|
| **Library** | the game cards, the empty state, the selection mode | "Library", *Manage* |
| **Settings** | the runtime options as list sections | "Settings", *Restore defaults*, *Save* |
| **Logs** | the diagnostics: the engine state, the log files, the log lines | "Logs", reload, share, overflow |

The rail is always visible: the Activity is locked to `sensorLandscape`
(`AndroidManifest.xml`), so the destinations are peers laid out side by side
rather than steps of one task, and the current one never goes off screen. The
selected rail item carries the rail's selection colour **and** its label, so
the current destination is readable without relying on the icon.

The destinations are a real navigation model, not a mode flag: the selected
destination is the only state that decides what the content area shows, and
the system back button returns to the Library (the destination the launcher
opens in) instead of finishing the Activity.

---

## 2. The files

| file | owns |
|---|---|
| `Theme.kt` | the Material 3 theme: the baseline light and dark colour schemes, the type scale, the shape scale, the shapes the components take |
| `Navigation.kt` | the `Destination` enum and the navigation rail |
| `LibraryScreen.kt` | the library screen: the app bar, the card grid, the empty state, the selection mode, the "Add game" action |
| `GameCard.kt` | one game as a card: the monogram tile, the name, the folder, the play action, the per-game overflow menu |
| `ManageBar.kt` | the bar of the selection mode and its bulk removal |
| `EmptyLibrary.kt` | the empty state |
| `SettingsScreen.kt` | the settings screen: the frame-rate slider, the two switches, the log-level chips, the save |
| `LogsScreen.kt` | the log screen: the summary card, the level filter, the log rows |
| `LogExport.kt` | the log share, copy and export actions |
| `Components.kt` | the shared pieces: the monogram tile, the section header, the path label |

### What `MainActivity.kt` keeps

The Activity itself and the state that must survive a game:

* the **permission reconciliation** — the "All files access" grant, re-read in
  `onResume`, because returning from the system settings page resumes an
  Activity that was never recreated;
* the **engine's last report** — the pending-launch marker is read and
  reconciled in `onResume`, so a crash inside a game is reported on the next
  start;
* the **dialogs** — the permission gate, the engine's issue report, the
  removal confirmation and the folder-path dialog. They belong to the launcher
  rather than to a destination, so they are shown wherever the user is;
* the **launch** — `launchGame`, the path resolution, the preflight check and
  the hand-off to the engine Activity.

The screens themselves live in `ui/`; the Activity only chooses which one is
composed and passes the state down.

---

## 3. State ownership

| state | where it lives | survives a rotation? |
|---|---|---|
| the selected destination | `rememberSaveable` in `LauncherScreen` | yes |
| the selection mode | `rememberSaveable` in `LauncherScreen` | yes |
| the selected games | `rememberSaveable` with `SelectionSaver` (a `listSaver` over the selected tree URIs) in `LauncherScreen` | yes |
| the game library | the `games.json` file, owned by `GameLibrary`; the in-memory list is Compose-local and re-read after every write | yes — it is on disk |
| the settings draft | four `rememberSaveable` strings in `SettingsScreen` | yes |
| the "All files access" grant | the Activity (`permissionGranted`), re-read in `onResume` | yes — it is a system setting |
| the engine's last report | the Activity (`startupIssue`), reconciled from the pending-launch marker in `onResume` | yes — it is a file |
| the pending launch marker | `krkr-launch-pending.json`, owned by `EngineDiagnostics` | yes — it is a file |

The selection mode and the selection are saved together, so a rotation cannot
restore one without the other.

---

## 4. The library write contract

`GameLibrary` owns `games.json`, and it is the only writer of it.

* **The schema does not change.** The file is
  `{ "version": 1, "games": [ { "name", "treeUri", "path" } ] }`, and
  `SCHEMA_VERSION` is `1`. A missing field reads as absent and an unknown
  field is ignored, so a file written by another build still loads.
* **Every mutation is one atomic write** (`DurableWrite.text`: write, flush,
  then replace). A failed write leaves the previous file intact and is
  reported as `SaveResult.FAILED`; the caller must tell the user, and the
  launcher does.
* **`removeAll(uris)` is the single removal entry point.** It drops every
  entry whose folder is one of the given tree URIs and writes the file **once**,
  so a set of games is removed as a whole or not at all: a removal written one
  entry at a time could leave the library half-updated when a write fails.
  `SaveResult.UNCHANGED` means none of the folders were in the library, and
  `SaveResult.FAILED` means the file could not be replaced, so nothing was
  removed.
* **The library is user-editable**, so the parser skips a malformed URI and
  de-duplicates the tree URIs: the list is keyed by the tree URI, and a
  duplicate key would crash the card list on every start.

---

## 5. The log level contract

The Logs screen colours and filters the lines of two files, written by two
different writers with two different line formats:

| file | writer | line format |
|---|---|---|
| `krkr-launcher.log` | `LauncherLog` (Kotlin) | `$stamp $level $message`, where `$stamp` is `yyyy-MM-dd HH:mm:ss.SSS` and `$level` is one of `D`, `I`, `W`, `E` |
| `krkr.log` | the Rust sink, `crates/android/src/log_file.rs` | `{rfc3339} {LEVEL} {message}`, where `{LEVEL}` is `ERROR`, `WARN`, `INFO`, `DEBUG` or `TRACE` |

The screen recognises both, and only these two:

* a **launcher line** matches `^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3} ([DIWE]) `;
* an **engine line** matches `^\S+ (ERROR|WARN|INFO|DEBUG|TRACE)\b`.

A line that matches neither carries **no level**. Such a line is a
continuation — a stack trace, for instance — and inherits the level of the
line above it, so the error it belongs to keeps its continuations. A line
with no level and no preceding line is always shown, so a change in either
writer's format degrades to "no filtering" and never to an empty list.

The filter chips (All / Errors / Warnings) narrow the list by that level; the
file order and the line content are untouched.

---

## 6. The theme

`Theme.kt` holds the Material 3 tokens:

* the **baseline light and dark colour schemes**, written out role by role
  (the primary/secondary/tertiary triples, the error pair, the surface
  family, the outline pair, the inverse pair), so a device without dynamic
  colour still shows a proper Material 3 palette;
* **Material You dynamic colour** on Android 12 and above, which replaces the
  baseline schemes at runtime;
* the **type scale** (the Material 3 scale, used unchanged) and the **shape
  scale** (4/8/12/16/28 dp).

No screen names a colour: every surface, text and outline colour comes from a
`MaterialTheme.colorScheme` role.

---

## 7. Accessibility

* Every icon-only control carries a content description, and an icon that
  sits next to a visible label passes `null` so it is not announced twice.
* The navigation rail always shows its label.
* The two switches are named for TalkBack, because the visible label is a
  separate node.
* The game card is clickable with the click label "Play" (or "Select" in the
  selection mode) and the `Button` role.
* A log line's level is carried by the text and by the colour role, so it does
  not depend on colour alone.

---

## 8. What is unverified

**The Android UI is unverified until an apk CI job or a device build.** The
development host has no Android SDK and no NDK, and neither may be installed
there, so the launcher has never been compiled by the Android toolchain on the
host. The CI workflow builds the APK; only that build, or a run on a device,
shows the interface.

What the host can check is the source: the launcher was type-checked against
the published Compose artifacts that `compose-bom:2026.09.00` resolves to
(material3 1.4.0, foundation 1.12.1, ui 1.12.1, runtime 1.12.1,
material-icons-core 1.7.8), and every signature it uses was read from those
artifacts' sources. That is a source-level check: it says the code calls the
API correctly, not that the result looks right.

Still unverified, and only a device or the CI APK can show it:

* the layout of the rail, the card grid and the settings list on a real
  screen, and the dialogs' stacking;
* the log level filter against a captured log file;
* the dynamic colour path on a device that has it;
* the share sheet, the document picker and the SAF picker flows;
* the TalkBack announcements.

The engine-side contracts the launcher depends on are documented in
[host.md](host.md) (§5, the state file; §6, the log file) and in
[config.md](config.md) (the settings the launcher persists).

---

## 9. Related documents

| topic | document |
|---|---|
| the Android composition root overview | [README.md](README.md) |
| the engine state file the launcher reports | [host.md](host.md) |
| the engine log file the Logs screen reads | [host.md](host.md) |
| the settings the launcher persists | [config.md](config.md) |
| the cross-compile and the packaging | [build.md](build.md) |
| the Android design, the launcher, the storage decision | [`android.md`](../../../docs.old/android.md) |
