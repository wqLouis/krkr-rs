package dev.krkr.rs

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.provider.Settings
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.listSaver
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import dev.krkr.rs.ui.Destination
import dev.krkr.rs.ui.KrkrNavigationRail
import dev.krkr.rs.ui.KrkrTheme
import dev.krkr.rs.ui.LibraryScreen
import dev.krkr.rs.ui.LogsScreen
import dev.krkr.rs.ui.SettingsScreen
import kotlinx.coroutines.launch

/** How many log lines to attach to an error dialog. */
private const val ISSUE_TAIL_LINES = 25

/**
 * The saver for the game selection: the tree URIs of the selected games, as a
 * list of strings.
 *
 * The selection is a set, but a set cannot be stored in a bundle directly, so
 * it is saved as a list. The selection and the selection mode are saved
 * together, so a rotation cannot restore one without the other.
 */
private val SelectionSaver = listSaver<Set<String>, String>(
    save = { keys -> keys.toList() },
    restore = { keys -> keys.toSet() },
)

/**
 * The launcher: it owns the game library and nothing else.
 *
 * The engine is deliberately not linked into this Activity — launching a game
 * starts [KrkrGameActivity], which runs the Rust `android_main` on its own
 * surface. Keeping the two apart means a crash inside a game's script cannot
 * take the library with it, and the launcher stays a normal Compose app.
 *
 * The Activity also owns the two pieces of state that must survive a game:
 * whether "All files access" is granted, and what the engine reported the last
 * time a game was launched. Both are re-read in [onResume] because returning
 * from system Settings, or from a game, resumes an Activity that was never
 * recreated and so would otherwise keep stale values.
 */
class MainActivity : ComponentActivity() {

    private val permissionGranted = mutableStateOf(false)
    private val startupIssue = mutableStateOf<Issue?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        LauncherLog.init(this)
        LauncherLog.installUncaughtExceptionHandler()
        LauncherLog.i("MainActivity.onCreate: restored=${savedInstanceState != null}")
        permissionGranted.value = SafPaths.hasAllFilesAccess()
        val library = GameLibrary(this)
        val settings = RuntimeSettingsStore(this)
        setContent {
            KrkrTheme {
                LauncherScreen(
                    library = library,
                    settings = settings,
                    permissionGranted = permissionGranted.value,
                    onOpenPermissionSettings = ::openAllFilesSettings,
                    startupIssue = startupIssue.value,
                    onDismissStartupIssue = {
                        // Clear only when the report is actually dismissed:
                        // keeping the marker means a rotation (which re-runs
                        // reconcilePendingLaunch) cannot swallow it.
                        EngineDiagnostics.clearLaunch(this@MainActivity)
                        startupIssue.value = null
                    },
                )
            }
        }
    }

    override fun onStart() {
        super.onStart()
        LauncherLog.i("MainActivity.onStart")
    }

    override fun onResume() {
        super.onResume()
        val granted = SafPaths.hasAllFilesAccess()
        LauncherLog.i("onResume: all-files access = $granted")
        permissionGranted.value = granted
        reconcilePendingLaunch()
    }

    override fun onPause() {
        super.onPause()
        LauncherLog.i("MainActivity.onPause")
    }

    override fun onStop() {
        super.onStop()
        LauncherLog.i("MainActivity.onStop")
    }

    override fun onDestroy() {
        super.onDestroy()
        LauncherLog.i("MainActivity.onDestroy")
    }

    /**
     * Opens the per-app "All files access" settings screen.
     *
     * `ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION` with a `package:` URI is
     * the exact page for this app; some OEM builds do not implement it, so a
     * failure falls back to the generic list. Nothing here is allowed to throw.
     */
    private fun openAllFilesSettings() {
        val perApp = Intent(
            Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION,
            Uri.parse("package:$packageName"),
        )
        LauncherLog.i("opening all-files settings")
        val opened = runCatching { startActivity(perApp) }.isSuccess
        if (!opened) {
            runCatching { startActivity(Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION)) }
                .onFailure { LauncherLog.w("cannot open all-files settings: ${it.message}") }
        }
    }

    /**
     * Turns the engine's state file into a dialog if it indicates a problem.
     *
     * A pending-launch marker is written before `startActivity` and survives the
     * launcher's process being killed with the engine, which is what lets a
     * failure be reported on the next start. It is the only evidence that a
     * launch was attempted, so it is cleared according to these rules:
     *
     *  * no marker                      -> nothing to do;
     *  * clean exit ("stopped")         -> clear immediately, show nothing;
     *  * engine still alive (same pid)  -> keep the marker, show nothing (a
     *                                      later resume after a crash must still
     *                                      be able to report);
     *  * the process is gone            -> Android's exit record says how it
     *                                      ended; the report names the reason;
     *  * any Issue shown                -> keep the marker;
     *                                      [onDismissStartupIssue] clears it, so
     *                                      a rotation cannot swallow the report.
     */
    private fun reconcilePendingLaunch() {
        val pending = EngineDiagnostics.readLaunch(this) ?: return
        val state = EngineDiagnostics.readState(this)
        val stateAge = EngineDiagnostics.stateFile(this).lastModified()
        val markerAge = EngineDiagnostics.launchFile(this).lastModified()
        LauncherLog.i(
            "reconciling launch of \"${pending.name}\": state=${state?.state ?: "none"} " +
                "(state mtime=$stateAge, marker mtime=$markerAge)",
        )

        // A missing state file, or one older than the marker, means the engine
        // never reported anything for THIS launch. That is exactly the signature
        // of a failure before the native entry point (the Activity-construction
        // crash that used to be invisible), and a stale state left over from an
        // earlier run must not be mistaken for this run's result.
        if (state == null || stateAge < markerAge) {
            startupIssue.value = Issue(
                title = "The engine did not start",
                message = "\"${pending.name}\" was launched but the engine never reported " +
                    "anything. It likely failed before it could start. The log below may say why.",
                detail = LauncherLog.tail(EngineDiagnostics.engineLogFile(this), ISSUE_TAIL_LINES),
            )
            return
        }

        when (state.state) {
            "failed" -> {
                // A panic that unwound out of the engine is a crash, not a
                // problem the game reported; the two get different reports.
                val crashed = state.kind == "panic"
                val message = state.message
                    ?: "The engine reported a failure but did not leave a message."
                startupIssue.value = Issue(
                    title = if (crashed) "The game crashed" else "The game could not start",
                    message = if (crashed) {
                        "The game stopped because of an error inside the app.\n\n$message"
                    } else {
                        message
                    },
                    detail = LauncherLog.tail(EngineDiagnostics.engineLogFile(this), ISSUE_TAIL_LINES),
                )
            }

            "starting", "running" -> {
                if (state.pid != null && state.pid == android.os.Process.myPid()) {
                    // The engine is still alive in this very process: the
                    // launcher was recreated while the game keeps running. Keep
                    // the marker so a later crash is still detected.
                    LauncherLog.i("engine still alive (pid=${state.pid}); keeping pending marker")
                    return
                }
                // The process is gone and never wrote a clean stop. Android
                // knows how it ended: a crash, a kill by the system and the
                // user closing the app are three different reports, and the
                // state file alone cannot tell them apart.
                val report = ProcessExit.read(this, state.pid).report(pending.name)
                startupIssue.value = Issue(
                    title = report.title,
                    message = report.message,
                    detail = LauncherLog.tail(EngineDiagnostics.engineLogFile(this), ISSUE_TAIL_LINES),
                )
            }

            else -> {
                // "stopped", or an unknown future state: a clean exit, nothing
                // to report. Clear the marker only here and on dismissal.
                LauncherLog.i("launch of \"${pending.name}\" reconciled cleanly; clearing pending marker")
                EngineDiagnostics.clearLaunch(this)
            }
        }
    }
}

/**
 * The launcher's shell: the navigation rail, the destination it selects and the
 * dialogs the launcher owns.
 *
 * The destinations are peers, so the rail is always visible and the selected
 * one is the only state that decides what the content area shows. The dialogs
 * belong to the launcher rather than to a destination: the permission gate and
 * the engine's reports have to be shown wherever the user is.
 */
@Composable
private fun LauncherScreen(
    library: GameLibrary,
    settings: RuntimeSettingsStore,
    permissionGranted: Boolean,
    onOpenPermissionSettings: () -> Unit,
    startupIssue: Issue?,
    onDismissStartupIssue: () -> Unit,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val snackbar = remember { SnackbarHostState() }

    var destination by rememberSaveable { mutableStateOf(Destination.LIBRARY) }
    var games by remember { mutableStateOf(library.all()) }
    var manageMode by rememberSaveable { mutableStateOf(false) }
    var selectedKeys by rememberSaveable(stateSaver = SelectionSaver) {
        mutableStateOf(emptySet<String>())
    }
    var pendingRemoval by remember { mutableStateOf<List<GameEntry>?>(null) }
    var gameIssue by remember { mutableStateOf<Issue?>(null) }
    var pathTarget by remember { mutableStateOf<GameEntry?>(null) }
    var settingsDirty by remember { mutableStateOf(false) }

    // The permission dialog is shown whenever the grant is missing, unless the
    // user explicitly dismissed it ("Not now"). Tapping a game re-opens it.
    var permissionDismissed by rememberSaveable { mutableStateOf(false) }
    LaunchedEffect(permissionGranted) { if (permissionGranted) permissionDismissed = false }
    val showPermission = !permissionGranted && !permissionDismissed

    /** Reports a transient problem to the user; never fails silently. */
    fun notify(text: String) {
        scope.launch { snackbar.showSnackbar(text) }
    }

    /** Shows another destination, leaving the selection mode behind. */
    fun selectDestination(next: Destination) {
        if (next == destination) return
        if (destination == Destination.SETTINGS && settingsDirty) {
            notify("Unsaved settings changes were discarded.")
        }
        settingsDirty = false
        if (next != Destination.LIBRARY) {
            manageMode = false
            selectedKeys = emptySet()
        }
        destination = next
    }

    /** Removes the given games from the library, or says why it did not. */
    fun removeGames(entries: List<GameEntry>) {
        pendingRemoval = null
        if (entries.isEmpty()) return
        when (library.removeAll(entries.map { it.uri })) {
            SaveResult.SAVED -> {
                games = library.all()
                selectedKeys = selectedKeys - entries.map { it.key }.toSet()
                notify(
                    if (entries.size == 1) {
                        "Removed \"${entries.first().name}\" from the library."
                    } else {
                        "Removed ${entries.size} games from the library."
                    },
                )
            }

            SaveResult.FAILED -> notify(
                if (entries.size == 1) {
                    "Could not remove \"${entries.first().name}\"; the library file is not writable."
                } else {
                    "Could not remove ${entries.size} games; the library file is not writable."
                },
            )

            SaveResult.UNCHANGED -> notify("Nothing was removed; the library is unchanged.")
        }
    }

    // System back returns to the library, the destination the launcher opens
    // in, instead of finishing the Activity.
    BackHandler(enabled = destination != Destination.LIBRARY) {
        selectDestination(Destination.LIBRARY)
    }

    // The system file picker. A tree (not a single file) because a game is a
    // folder containing its .xp3 archives.
    val pickFolder = rememberLauncherForActivityResult(
        ActivityResultContracts.OpenDocumentTree(),
    ) { uri: Uri? ->
        if (uri == null) return@rememberLauncherForActivityResult

        // Persist read+write: the engine reads the archives and writes saves
        // into <game>/savedata/. Without this the grant dies with the process.
        val persisted = runCatching {
            context.contentResolver.takePersistableUriPermission(
                uri,
                Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION,
            )
        }.recoverCatching {
            // A provider may have granted only one mode; asking for both then
            // throws SecurityException and would lose even the grant that was
            // available. Retry read-only before giving up.
            context.contentResolver.takePersistableUriPermission(
                uri,
                Intent.FLAG_GRANT_READ_URI_PERMISSION,
            )
        }
        if (persisted.isFailure) {
            // Some providers do not offer persistable grants. The folder can
            // still be used this session, but it will not survive a restart —
            // say so rather than letting it silently disappear later.
            notify("This folder's access cannot be remembered; it may be lost on restart.")
        }

        val name = SafPaths.displayName(context, uri)
        // Resolve now so the config file records where the game actually is;
        // a folder that cannot be addressed as a path is still added (the user
        // may mount that volume later), but we say so immediately.
        val path = SafPaths.resolve(uri)
        when (library.add(GameEntry(uri, name, path))) {
            SaveResult.SAVED -> {
                games = library.all()
                if (path != null) {
                    notify("Added \"$name\".")
                } else {
                    notify("Added \"$name\", but its folder is not on a filesystem the engine can read.")
                }
            }

            SaveResult.UNCHANGED -> notify("\"$name\" is already in the library.")

            SaveResult.FAILED -> notify(
                "Could not save \"$name\" to the library. It will disappear on restart.",
            )
        }
    }

    Row(modifier = Modifier.fillMaxSize()) {
        KrkrNavigationRail(
            current = destination,
            onSelect = { selectDestination(it) },
            modifier = Modifier.fillMaxHeight(),
        )

        when (destination) {
            Destination.LIBRARY -> LibraryScreen(
                games = games,
                manageMode = manageMode,
                selectedKeys = selectedKeys,
                configPath = library.configFile().absolutePath,
                snackbarHostState = snackbar,
                onAddGame = { pickFolder.launch(null) },
                onPlay = { entry ->
                    if (!permissionGranted) {
                        // Re-open the gate dialog instead of silently doing
                        // nothing.
                        permissionDismissed = false
                    } else {
                        launchGame(
                            context = context,
                            library = library,
                            entry = entry,
                            onIssue = { gameIssue = it },
                            onNeedPermission = { permissionDismissed = false },
                        )
                    }
                },
                onRemove = { entries -> pendingRemoval = entries },
                onShowPath = { entry -> pathTarget = entry },
                onToggleSelection = { entry ->
                    selectedKeys = if (entry.key in selectedKeys) {
                        selectedKeys - entry.key
                    } else {
                        selectedKeys + entry.key
                    }
                },
                onSelectAll = {
                    selectedKeys = if (selectedKeys.size == games.size) {
                        emptySet()
                    } else {
                        games.map { it.key }.toSet()
                    }
                },
                onEnterManage = { manageMode = true },
                onExitManage = {
                    manageMode = false
                    selectedKeys = emptySet()
                },
                modifier = Modifier.fillMaxSize(),
            )

            Destination.SETTINGS -> SettingsScreen(
                store = settings,
                snackbarHostState = snackbar,
                onNotify = { notify(it) },
                onDirtyChange = { settingsDirty = it },
                modifier = Modifier.fillMaxSize(),
            )

            Destination.LOGS -> LogsScreen(
                snackbarHostState = snackbar,
                onNotify = { notify(it) },
                modifier = Modifier.fillMaxSize(),
            )
        }
    }

    // Render at most one AlertDialog at a time, by priority: the permission
    // gate first, then the startup issue, then the preflight issue, then the
    // remove confirmation, then the folder path. Otherwise two dialogs stack
    // and one is obscured.
    if (showPermission) {
        AlertDialog(
            onDismissRequest = { permissionDismissed = true },
            title = { Text("All files access is required") },
            text = {
                Text(
                    "krkr-rs reads each game's files directly from storage, which Android only " +
                        "allows with \"All files access\".\n\n" +
                        "Turn it on for krkr-rs in Settings → Apps → Special app access → " +
                        "All files access, then come back.",
                )
            },
            confirmButton = {
                TextButton(onClick = onOpenPermissionSettings) { Text("Open settings") }
            },
            dismissButton = {
                TextButton(onClick = { permissionDismissed = true }) { Text("Not now") }
            },
        )
    } else if (startupIssue != null) {
        IssueDialog(
            issue = startupIssue,
            onDismiss = onDismissStartupIssue,
            onViewLogs = {
                onDismissStartupIssue()
                selectDestination(Destination.LOGS)
            },
        )
    } else if (gameIssue != null) {
        gameIssue?.let { issue ->
            IssueDialog(
                issue = issue,
                onDismiss = { gameIssue = null },
                onViewLogs = {
                    gameIssue = null
                    selectDestination(Destination.LOGS)
                },
            )
        }
    } else if (pendingRemoval != null) {
        pendingRemoval?.let { entries ->
            AlertDialog(
                onDismissRequest = { pendingRemoval = null },
                title = {
                    Text(
                        if (entries.size == 1) {
                            "Remove from library?"
                        } else {
                            "Remove ${entries.size} games?"
                        },
                    )
                },
                text = {
                    Text(
                        if (entries.size == 1) {
                            "\"${entries.first().name}\" will be removed from this list. " +
                                "The game's files on disk are not touched."
                        } else {
                            "${entries.size} games will be removed from this list. " +
                                "Their files on disk are not touched."
                        },
                    )
                },
                confirmButton = {
                    TextButton(onClick = { removeGames(entries) }) { Text("Remove") }
                },
                dismissButton = {
                    TextButton(onClick = { pendingRemoval = null }) { Text("Cancel") }
                },
            )
        }
    } else if (pathTarget != null) {
        pathTarget?.let { entry ->
            val path = entry.path
            AlertDialog(
                onDismissRequest = { pathTarget = null },
                title = { Text("Folder path") },
                text = {
                    Column {
                        Text(entry.name, style = MaterialTheme.typography.titleMedium)
                        Spacer(Modifier.height(8.dp))
                        if (path != null) {
                            Text(
                                text = path,
                                style = MaterialTheme.typography.bodyMedium,
                                fontFamily = FontFamily.Monospace,
                            )
                        } else {
                            Text(
                                text = "This folder is not on a filesystem the engine can open by " +
                                    "path, so it has no path to show. The game can stay in the " +
                                    "library, but it can only be started once its folder is on " +
                                    "internal storage or an SD card.",
                                style = MaterialTheme.typography.bodyMedium,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                },
                confirmButton = {
                    if (path != null) {
                        TextButton(onClick = {
                            if (copyPath(context, path)) {
                                notify("Path copied.")
                            } else {
                                notify("Could not copy the path.")
                            }
                            pathTarget = null
                        }) { Text("Copy path") }
                    } else {
                        TextButton(onClick = { pathTarget = null }) { Text("Close") }
                    }
                },
            )
        }
    }
}

/**
 * A dismissible problem dialog. [onViewLogs] is optional; when present the
 * dialog offers a shortcut into the Logs screen so the user can share the
 * reason without a debugger.
 */
@Composable
private fun IssueDialog(
    issue: Issue,
    onDismiss: () -> Unit,
    onViewLogs: (() -> Unit)?,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(issue.title) },
        text = {
            Column {
                Text(issue.message)
                issue.detail?.takeIf { it.isNotBlank() }?.let { detail ->
                    Spacer(Modifier.height(12.dp))
                    Text(
                        text = detail,
                        style = MaterialTheme.typography.bodySmall,
                        fontFamily = FontFamily.Monospace,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier
                            .fillMaxWidth()
                            .heightIn(max = 220.dp)
                            .verticalScroll(rememberScrollState()),
                    )
                }
            }
        },
        confirmButton = {
            if (onViewLogs != null) {
                TextButton(onClick = onViewLogs) { Text("View logs") }
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text("Dismiss") }
        },
    )
}

/**
 * Copies a path to the clipboard. Returns false when the clipboard is
 * unavailable or the copy failed, so the caller can say so.
 */
private fun copyPath(context: Context, path: String): Boolean {
    val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager
    if (clipboard == null) {
        LauncherLog.w("clipboard service unavailable; cannot copy the path")
        return false
    }
    return runCatching {
        clipboard.setPrimaryClip(ClipData.newPlainText("krkr-rs game path", path))
    }.onFailure { LauncherLog.e("cannot copy the path", it) }.isSuccess
}

/**
 * Starts the engine for [entry], or explains why it cannot.
 *
 * The engine needs a real path (docs.old/android.md §4), so every failure is turned
 * into a specific dialog *before* the engine Activity is started: a null
 * resolution ([SafPaths.resolve] returns null for a provider that is not a
 * filesystem), a missing/unreadable directory, or a folder that is not a
 * KiriKiri game ([GamePreflight]). A successful launch is recorded
 * ([EngineDiagnostics.markLaunch]) so a crash can be reported when the launcher
 * next resumes.
 */
private fun launchGame(
    context: Context,
    library: GameLibrary,
    entry: GameEntry,
    onIssue: (Issue) -> Unit,
    onNeedPermission: () -> Unit,
) {
    // Belt and braces: the UI gates this too, but the engine must never be
    // started by path without "All files access".
    if (!SafPaths.hasAllFilesAccess()) {
        LauncherLog.w("launch blocked: All files access is not granted")
        onNeedPermission()
        return
    }

    // Re-resolve on every launch. The mapping is pure string work (no I/O), so
    // the fresh path is authoritative: the cached path in `games.json` can name
    // a different folder after a hand edit or a restored backup, and the
    // primary-volume fallback can resolve the same URI two ways. Use the cached
    // path only when the URI no longer resolves at all.
    val cached = entry.path
    val fresh = SafPaths.resolve(entry.uri)
    var path = fresh ?: cached
    if (path == null) {
        LauncherLog.w("launch blocked: \"${entry.name}\" has no resolvable path")
        onIssue(
            Issue(
                title = "Cannot open this folder",
                message = "\"${entry.name}\" is not in a location the engine can open by path. " +
                    "Only folders on internal storage, an SD card, or Downloads can be used — " +
                    "cloud storage and other document providers cannot be mapped to a path. " +
                    "Copy the game to internal storage or an SD card.",
            ),
        )
        return
    }

    var problem = GamePreflight.check(path)
    if (problem != null && fresh != null && path == fresh && cached != null && cached != fresh) {
        // The fresh path does not work, but the cached one still does. Fall back
        // to it rather than refusing a folder the user could launch.
        LauncherLog.i(
            "resolved path \"$fresh\" failed (${problem.title}); trying cached path \"$cached\"",
        )
        val cachedProblem = GamePreflight.check(cached)
        if (cachedProblem == null) {
            path = cached
            problem = null
        } else {
            problem = cachedProblem
        }
    }
    if (problem == null && fresh != null && path == fresh && cached != fresh) {
        // The fresh resolution is what we are launching; keep the cache in step
        // with it so the file matches the path in use.
        if (library.updatePath(entry.uri, fresh) == SaveResult.FAILED) {
            LauncherLog.w("could not refresh the cached path for \"${entry.name}\"")
        }
    }
    if (problem != null) {
        LauncherLog.w("launch blocked: \"${entry.name}\" — ${problem.title} ($path)")
        onIssue(problem)
        return
    }

    // A game cannot be switched inside a live android_main: the native side reads
    // pendingGameDir once, when android_main starts, and nothing ever re-reads
    // it. Starting a second game would therefore just resume the first while the
    // pending marker written below would name the second, so a later crash would
    // be blamed on the wrong game. Only allow the launch when it is the game that
    // is already running (which merely brings it back to the front); otherwise
    // refuse and say why, and never write a marker for a game that will not run.
    val running = EngineDiagnostics.readState(context)?.takeIf {
        (it.state == "starting" || it.state == "running") &&
            it.pid != null && it.pid == android.os.Process.myPid()
    }
    if (running != null) {
        val current = EngineDiagnostics.readLaunch(context)
        if (current?.path == path) {
            LauncherLog.i("engine already running \"${entry.name}\"; bringing it to the front")
            runCatching {
                context.startActivity(
                    Intent(context, KrkrGameActivity::class.java)
                        .putExtra(KrkrGameActivity.EXTRA_GAME_DIR, path)
                        .putExtra(KrkrGameActivity.EXTRA_GAME_NAME, entry.name),
                )
            }.onFailure { LauncherLog.w("could not bring the running game to the front: ${it.message}") }
            return
        }
        LauncherLog.w("launch blocked: the engine is already running a different game")
        onIssue(
            Issue(
                title = "A game is already running",
                message = "krkr-rs can only run one game at a time. Close \"" +
                    (current?.name ?: "the running game") + "\" and try again.",
            ),
        )
        return
    }

    LauncherLog.i("launching \"${entry.name}\" from $path")
    EngineDiagnostics.markLaunch(context, entry.name, path)
    runCatching {
        context.startActivity(
            Intent(context, KrkrGameActivity::class.java)
                .putExtra(KrkrGameActivity.EXTRA_GAME_DIR, path)
                .putExtra(KrkrGameActivity.EXTRA_GAME_NAME, entry.name),
        )
    }.onFailure {
        LauncherLog.e("could not start KrkrGameActivity", it)
        EngineDiagnostics.clearLaunch(context)
        onIssue(
            Issue(
                title = "Could not start the engine",
                message = "Android refused to start the game activity: ${it.message}",
            ),
        )
    }
}
