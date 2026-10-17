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
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.core.content.FileProvider
import dev.krkr.rs.ui.KrkrTheme
import kotlinx.coroutines.launch
import java.io.File
import java.io.OutputStream
import java.util.Locale

/** How many lines of each log the Logs screen and crash dialogs show. */
private const val LOG_TAIL_LINES = 400

/** How many log lines to attach to an error dialog. */
private const val ISSUE_TAIL_LINES = 25

/** File name offered to the system document picker for the combined export. */
private const val EXPORT_FILE_NAME = "krkr-rs-logs.txt"

/** Cache subdirectory the full-log Share action stages its copies in. */
private const val CACHE_DIR = "share"

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
        LauncherLog.i("MainActivity.onCreate")
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

    override fun onResume() {
        super.onResume()
        val granted = SafPaths.hasAllFilesAccess()
        LauncherLog.i("onResume: all-files access = $granted")
        permissionGranted.value = granted
        reconcilePendingLaunch()
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
                startupIssue.value = Issue(
                    title = "The game could not start",
                    message = state.message
                        ?: "The engine reported a failure but did not leave a message.",
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
                startupIssue.value = Issue(
                    title = "The game stopped unexpectedly",
                    message = "\"${pending.name}\" did not shut down normally. It may have crashed, " +
                        "or Android may have killed it while it was in the background.",
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

@OptIn(ExperimentalMaterial3Api::class)
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

    var games by remember { mutableStateOf(library.all()) }
    var pendingRemoval by remember { mutableStateOf<GameEntry?>(null) }
    var gameIssue by remember { mutableStateOf<Issue?>(null) }
    // Saveable so a configuration change (rotation, dark-mode toggle, fold,
    // resize) does not close the open sub-screen or forget a dismissal.
    var showLogs by rememberSaveable { mutableStateOf(false) }
    var showSettings by rememberSaveable { mutableStateOf(false) }

    // The permission dialog is shown whenever the grant is missing, unless the
    // user explicitly dismissed it ("Not now"). Tapping a game re-opens it.
    var permissionDismissed by rememberSaveable { mutableStateOf(false) }
    LaunchedEffect(permissionGranted) { if (permissionGranted) permissionDismissed = false }
    val showPermission = !permissionGranted && !permissionDismissed

    // System back must agree with the TopAppBar arrow: while a sub-screen is
    // open it returns to the library instead of finishing the Activity.
    BackHandler(enabled = showLogs || showSettings) {
        showLogs = false
        showSettings = false
    }

    /** Reports a transient problem to the user; never fails silently. */
    fun notify(text: String) {
        scope.launch { snackbar.showSnackbar(text) }
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

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        when {
                            showLogs -> "Logs"
                            showSettings -> "Settings"
                            else -> "krkr-rs"
                        },
                    )
                },
                navigationIcon = {
                    if (showLogs || showSettings) {
                        IconButton(onClick = {
                            showLogs = false
                            showSettings = false
                        }) {
                            Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                        }
                    }
                },
                actions = {
                    if (!showLogs && !showSettings) {
                        TextButton(onClick = { showSettings = true }) { Text("Settings") }
                        TextButton(onClick = { showLogs = true }) { Text("Logs") }
                    }
                },
            )
        },
        snackbarHost = { SnackbarHost(snackbar) },
        floatingActionButton = {
            if (!showLogs && !showSettings) {
                ExtendedFloatingActionButton(
                    onClick = { pickFolder.launch(null) },
                    icon = { Icon(Icons.Default.Add, contentDescription = null) },
                    text = { Text("Add game") },
                )
            }
        },
    ) { padding ->
        if (showLogs) {
            LogsScreen(
                modifier = Modifier.fillMaxSize().padding(padding),
                onNotify = { notify(it) },
            )
        } else if (showSettings) {
            SettingsScreen(
                store = settings,
                modifier = Modifier.fillMaxSize().padding(padding),
                onNotify = { notify(it) },
                onBack = { showSettings = false },
            )
        } else if (games.isEmpty()) {
            EmptyLibrary(
                modifier = Modifier.fillMaxSize().padding(padding),
                configPath = library.configFile().absolutePath,
            )
        } else {
            LazyColumn(
                modifier = Modifier.fillMaxSize().padding(padding),
                contentPadding = PaddingValues(16.dp, 8.dp, 16.dp, 96.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                items(games, key = { it.key }) { entry ->
                    GameCard(
                        entry = entry,
                        onLaunch = {
                            if (!permissionGranted) {
                                // Re-open the gate dialog instead of silently
                                // doing nothing.
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
                        onRemove = { pendingRemoval = entry },
                    )
                }
            }
        }
    }

    // Render at most one AlertDialog at a time, by priority: the permission
    // gate first, then the startup issue, then the preflight issue, then the
    // remove confirmation. Otherwise two dialogs stack and one is obscured.
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
        startupIssue?.let { issue ->
            IssueDialog(
                issue = issue,
                onDismiss = onDismissStartupIssue,
                onViewLogs = {
                    onDismissStartupIssue()
                    showLogs = true
                },
            )
        }
    } else if (gameIssue != null) {
        gameIssue?.let { issue ->
            IssueDialog(
                issue = issue,
                onDismiss = { gameIssue = null },
                onViewLogs = {
                    gameIssue = null
                    showLogs = true
                },
            )
        }
    } else if (pendingRemoval != null) {
        pendingRemoval?.let { entry ->
            AlertDialog(
                onDismissRequest = { pendingRemoval = null },
                title = { Text("Remove from library?") },
                text = {
                    Text("\"${entry.name}\" will be removed from this list. The game's files on disk are not touched.")
                },
                confirmButton = {
                    TextButton(onClick = {
                        when (library.remove(entry.uri)) {
                            SaveResult.SAVED -> games = library.all()
                            SaveResult.FAILED -> notify(
                                "Could not remove \"${entry.name}\"; the library file is not writable.",
                            )
                            SaveResult.UNCHANGED -> Unit
                        }
                        pendingRemoval = null
                    }) { Text("Remove") }
                },
                dismissButton = {
                    TextButton(onClick = { pendingRemoval = null }) { Text("Cancel") }
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
 * The log viewer: the tail of both the launcher and the engine log for reading
 * on screen, plus the full-log Share and Export actions. The tail Share/Copy
 * stay for a quick paste; Share all sends every log file through the share
 * sheet and Export writes one combined file to a folder the user picks. This is
 * the escape hatch that lets a user report a failure without adb.
 */
@Composable
private fun LogsScreen(modifier: Modifier = Modifier, onNotify: (String) -> Unit) {
    val context = LocalContext.current
    var refreshToken by remember { mutableStateOf(0) }

    val launcherTail = remember(refreshToken) {
        LauncherLog.tail(LauncherLog.file(context), LOG_TAIL_LINES)
    }
    val engineTail = remember(refreshToken) {
        LauncherLog.tail(EngineDiagnostics.engineLogFile(context), LOG_TAIL_LINES)
    }
    val stateText = remember(refreshToken) { EngineDiagnostics.stateText(context) }
    val fullLog = remember(refreshToken) { fullLogSources(context) }
    val fileSummaries = remember(refreshToken) { fullLogSummaries(fullLog) }
    val fullLogBytes = remember(refreshToken) { totalLogBytes(fullLog) }

    // The user picks the destination with the system document picker; the
    // result is the chosen document URI, or null when they cancel.
    val exportLog = rememberLauncherForActivityResult(
        ActivityResultContracts.CreateDocument("text/plain"),
    ) { uri: Uri? ->
        if (uri == null) {
            onNotify("Export cancelled.")
            return@rememberLauncherForActivityResult
        }
        val failure = writeFullLog(context, uri)
        if (failure != null) {
            onNotify(failure)
            return@rememberLauncherForActivityResult
        }
        val missing = missingLogNames(fullLog)
        val size = formatBytes(fullLogBytes)
        onNotify(
            if (missing.isEmpty()) {
                "Exported the full log ($size)."
            } else {
                "Exported the log ($size); missing: ${missing.joinToString(", ")}."
            },
        )
    }

    Column(modifier.fillMaxSize().padding(16.dp)) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            Text(
                text = "Diagnostics",
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.weight(1f),
            )
            TextButton(onClick = { refreshToken++ }) { Text("Refresh") }
            TextButton(onClick = {
                if (!shareLogs(context, launcherTail, engineTail, stateText)) {
                    onNotify("Could not open the share sheet.")
                }
            }) { Text("Share") }
            TextButton(onClick = {
                if (copyLogs(context, launcherTail, engineTail, stateText)) {
                    onNotify("Logs copied.")
                } else {
                    onNotify("Could not copy the logs.")
                }
            }) { Text("Copy") }
        }
        Spacer(Modifier.height(4.dp))
        Text(
            text = "Last $LOG_TAIL_LINES lines, newest last. " +
                EngineDiagnostics.stateFile(context).name + " + " +
                LauncherLog.file(context).name + " + " +
                EngineDiagnostics.engineLogFile(context).name,
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(8.dp))
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            Text(
                text = "Full log",
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.weight(1f),
            )
            TextButton(onClick = { onNotify(shareFullLogs(context)) }) { Text("Share all") }
            TextButton(onClick = { exportLog.launch(EXPORT_FILE_NAME) }) { Text("Export") }
        }
        fileSummaries.forEach { summary ->
            Text(
                text = summary,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Text(
            text = "Export total: ${formatBytes(fullLogBytes)} across ${fullLog.size} files " +
                "(the export writes every file whole; nothing is truncated).",
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(8.dp))
        Column(Modifier.weight(1f).verticalScroll(rememberScrollState())) {
            LogSection("krkr_state.json", stateText)
            LogSection("krkr-launcher.log", launcherTail)
            LogSection("krkr.log", engineTail)
        }
    }
}

@Composable
private fun LogSection(title: String, body: String) {
    Text(
        text = title,
        style = MaterialTheme.typography.titleSmall,
        color = MaterialTheme.colorScheme.primary,
    )
    Text(
        text = body.ifBlank { "(empty)" },
        style = MaterialTheme.typography.bodySmall,
        fontFamily = FontFamily.Monospace,
        modifier = Modifier.fillMaxWidth().padding(top = 4.dp, bottom = 20.dp),
    )
}

private fun combineLogs(launcherTail: String, engineTail: String, stateText: String): String =
    buildString {
        appendLine("== krkr_state.json ==")
        appendLine(stateText.ifBlank { "(empty)" })
        appendLine()
        appendLine("== krkr-launcher.log ==")
        appendLine(launcherTail.ifBlank { "(empty)" })
        appendLine()
        appendLine("== krkr.log ==")
        appendLine(engineTail.ifBlank { "(empty)" })
    }

/**
 * Shares the logs as text, attaching a file through the app's FileProvider.
 * Returns false when the chooser could not be opened, so the caller can tell
 * the user instead of failing silently.
 */
private fun shareLogs(
    context: Context,
    launcherTail: String,
    engineTail: String,
    stateText: String,
): Boolean {
    val text = combineLogs(launcherTail, engineTail, stateText)
    val intent = Intent(Intent.ACTION_SEND).apply {
        type = "text/plain"
        putExtra(Intent.EXTRA_SUBJECT, "krkr-rs logs")
        putExtra(Intent.EXTRA_TEXT, text)
        // Attach the whole file as well, so a receiver that handles streams
        // gets the complete logs rather than a truncated EXTRA_TEXT. If the
        // provider is unavailable this is simply skipped.
        runCatching {
            val dir = File(context.cacheDir, "share").apply { mkdirs() }
            val file = File(dir, "krkr-logs.txt")
            file.writeText(text)
            val uri = FileProvider.getUriForFile(
                context,
                "${context.packageName}.fileprovider",
                file,
            )
            putExtra(Intent.EXTRA_STREAM, uri)
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }.onFailure { LauncherLog.w("cannot attach log file: ${it.message}") }
    }
    return runCatching { context.startActivity(Intent.createChooser(intent, "Share logs")) }
        .onFailure { LauncherLog.e("cannot share logs", it) }
        .isSuccess
}

private fun copyLogs(
    context: Context,
    launcherTail: String,
    engineTail: String,
    stateText: String,
): Boolean {
    val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager
    if (clipboard == null) {
        LauncherLog.w("clipboard service unavailable; cannot copy logs")
        return false
    }
    return runCatching {
        clipboard.setPrimaryClip(
            ClipData.newPlainText("krkr-rs logs", combineLogs(launcherTail, engineTail, stateText)),
        )
    }.onFailure { LauncherLog.e("cannot copy logs", it) }.isSuccess
}

/**
 * One diagnostics file in the full-log export: the engine state, an engine log
 * or a launcher log.
 *
 * [displayName] is the on-disk name shown to the user; [shareName] is the
 * unambiguous name used in the cache and the share sheet, where `krkr.log` and
 * `krkr.log.1` would otherwise be easy to confuse.
 */
private data class LogSource(val displayName: String, val shareName: String, val file: File)

/**
 * The complete log, in the order the files are read: the engine state first —
 * its `message` is the one line that says why a title failed — then every
 * engine log the retention policy has on disk, then the launcher's own file
 * and its rotation.
 *
 * The engine files are enumerated from the files directory rather than named,
 * so the export follows the Rust sink's rotation count without keeping a
 * second copy of it. A missing or empty file is still listed here; the
 * readers below name it as missing rather than failing.
 */
private fun fullLogSources(context: Context): List<LogSource> = buildList {
    add(LogSource("krkr_state.json", "krkr-state.json", EngineDiagnostics.stateFile(context)))
    for (file in EngineDiagnostics.engineLogFiles(context)) {
        add(LogSource(file.name, engineShareName(file.name), file))
    }
    add(LogSource("krkr-launcher.log", "krkr-launcher.log", LauncherLog.file(context)))
    add(LogSource("krkr-launcher.log.1", "krkr-launcher.log.1", LauncherLog.rotatedFile(context)))
}

/**
 * The unambiguous share/cache name for an engine log, so `krkr.log` and
 * `krkr.log.1` are not confused in the share sheet or the cache directory.
 */
private fun engineShareName(name: String): String =
    "krkr-engine" + name.removePrefix("krkr")

/** A path and size for each source, naming a file that does not exist as such. */
private fun fullLogSummaries(sources: List<LogSource>): List<String> = sources.map { source ->
    if (source.file.isFile) {
        "${source.displayName} — ${formatBytes(source.file.length())} — ${source.file.absolutePath}"
    } else {
        "${source.displayName} — not created yet"
    }
}

/** The display names of the sources that are absent or empty. */
private fun missingLogNames(sources: List<LogSource>): List<String> = sources
    .filter { !it.file.isFile || it.file.length() == 0L }
    .map { it.displayName }

/** The combined size of the files that are present, for the export notice. */
private fun totalLogBytes(sources: List<LogSource>): Long = sources.sumOf { source ->
    if (source.file.isFile) source.file.length() else 0L
}

/** Formats [bytes] as B, KiB or MiB for the Logs screen. */
private fun formatBytes(bytes: Long): String {
    val kib = bytes / 1024.0
    val mib = kib / 1024.0
    return when {
        bytes < 1024 -> "$bytes B"
        kib < 10 -> "%.1f KiB".format(Locale.US, kib)
        kib < 1024 -> "%.0f KiB".format(Locale.US, kib)
        else -> "%.1f MiB".format(Locale.US, mib)
    }
}

/**
 * Shares every present log as a file, copying each into the cache first.
 *
 * An absent or empty log is named as missing rather than failing the whole
 * share; when no log exists at all, nothing is opened and the user is told.
 * Returns the message to show, including why a share did not start.
 */
private fun shareFullLogs(context: Context): String {
    val sources = fullLogSources(context)
    val staged = mutableListOf<Uri>()
    val missing = mutableListOf<String>()
    var anyPresent = false
    for (source in sources) {
        if (!source.file.isFile || source.file.length() == 0L) {
            missing += source.displayName
            continue
        }
        anyPresent = true
        runCatching {
            val dir = File(context.cacheDir, CACHE_DIR).apply { mkdirs() }
            val copy = File(dir, source.shareName)
            source.file.copyTo(copy, overwrite = true)
            staged += FileProvider.getUriForFile(
                context,
                "${context.packageName}.fileprovider",
                copy,
            )
        }.onFailure {
            LauncherLog.w("cannot stage ${source.shareName} for sharing: ${it.message}")
            missing += source.displayName
        }
    }
    if (staged.isEmpty()) {
        return if (anyPresent) {
            "Could not prepare the log files to share."
        } else {
            "There are no log files to share yet."
        }
    }
    val intent = Intent(Intent.ACTION_SEND_MULTIPLE).apply {
        type = "text/plain"
        putExtra(Intent.EXTRA_SUBJECT, "krkr-rs logs")
        putParcelableArrayListExtra(Intent.EXTRA_STREAM, ArrayList(staged))
        addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        putExtra(
            Intent.EXTRA_TEXT,
            if (missing.isEmpty()) {
                "krkr-rs full logs attached."
            } else {
                "krkr-rs logs attached; missing: ${missing.joinToString(", ")}."
            },
        )
    }
    val opened = runCatching { context.startActivity(Intent.createChooser(intent, "Share logs")) }
        .onFailure { LauncherLog.e("cannot share the full logs", it) }
        .isSuccess
    if (!opened) {
        return "Could not open the share sheet."
    }
    return if (missing.isEmpty()) {
        "Sharing the full logs."
    } else {
        "Sharing the logs that exist; missing: ${missing.joinToString(", ")}."
    }
}

/**
 * Writes one combined log to the document at [uri].
 *
 * Returns an error message when the document could not be written, or null on
 * success. A missing or empty source is recorded in the file rather than
 * aborting the export.
 */
private fun writeFullLog(context: Context, uri: Uri): String? {
    val sources = fullLogSources(context)
    return runCatching {
        val out = context.contentResolver.openOutputStream(uri, "wt")
            ?: error("the document provider returned no write stream")
        out.use { writeCombinedLog(sources, it) }
    }.fold(
        onSuccess = { null },
        onFailure = { error ->
            LauncherLog.e("cannot export the logs", error)
            "Could not write the log file: ${error.message}"
        },
    )
}

/** Writes [sources] to [out], each preceded by a header that names it. */
private fun writeCombinedLog(sources: List<LogSource>, out: OutputStream) {
    for (source in sources) {
        out.write("===== ${source.displayName} =====\n".toByteArray(Charsets.UTF_8))
        if (!source.file.isFile || source.file.length() == 0L) {
            out.write("(no entries)\n\n".toByteArray(Charsets.UTF_8))
            continue
        }
        source.file.inputStream().use { it.copyTo(out) }
        out.write("\n\n".toByteArray(Charsets.UTF_8))
    }
}

/**
 * Starts the engine for [entry], or explains why it cannot.
 *
 * The engine needs a real path (docs/android.md §4), so every failure is turned
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

/**
 * The runtime-settings screen: the launcher's persisted copy of the engine
 * options the player can change.
 *
 * It exposes the four options the engine honours — the frame-rate limit, the
 * archive preload, the debug overlay and the engine log level — with the same
 * accepted values and range as the desktop CLI's `--fps`,
 * `--preload-archives` and `--debug-overlay` (`api::config`). Nothing is
 * offered for a feature the engine has no knob for.
 *
 * Saving writes the settings file atomically; [KrkrGameActivity] reads it and
 * publishes it over JNI on the next launch, which is why the change is described
 * as applying to the next run.
 */
@Composable
private fun SettingsScreen(
    store: RuntimeSettingsStore,
    modifier: Modifier = Modifier,
    onNotify: (String) -> Unit,
    onBack: () -> Unit,
) {
    val saved = remember { store.read() }
    // Saveable so an in-progress edit survives a configuration change rather
    // than silently reverting to the on-disk values.
    var frameRate by rememberSaveable { mutableStateOf(saved.frameRate) }
    var preloadArchives by rememberSaveable { mutableStateOf(saved.preloadArchives) }
    var debugOverlay by rememberSaveable { mutableStateOf(saved.debugOverlay) }
    var logLevel by rememberSaveable { mutableStateOf(saved.logLevel) }

    // Validate only the frame rate here: the other values can only be set from
    // their fixed controls, except when the file was hand-edited.
    val frameRateValue = frameRate.trim().toIntOrNull()
    val frameRateValid = frameRateValue != null &&
        frameRateValue in RuntimeOptions.MIN_FRAME_RATE..RuntimeOptions.MAX_FRAME_RATE
    val preloadKnown = preloadArchives in RuntimeOptions.PRELOAD_VALUES
    val preloadChecked = preloadArchives.toBooleanStrictOrNull() ?: false
    val debugKnown = debugOverlay in RuntimeOptions.DEBUG_OVERLAY_VALUES
    val debugChecked = debugOverlay.toBooleanStrictOrNull() ?: false
    val logLevelKnown = logLevel in RuntimeOptions.LOG_LEVELS

    Column(
        modifier = modifier.fillMaxSize().padding(16.dp).verticalScroll(rememberScrollState()),
    ) {
        Text("Runtime settings", style = MaterialTheme.typography.titleMedium)
        Spacer(Modifier.height(4.dp))
        Text(
            "Applied the next time a game starts.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(20.dp))

        Text("Frame-rate limit", style = MaterialTheme.typography.titleSmall)
        Spacer(Modifier.height(4.dp))
        OutlinedTextField(
            value = frameRate,
            onValueChange = { frameRate = it },
            label = { Text("Frames per second") },
            singleLine = true,
            isError = !frameRateValid,
            supportingText = {
                if (frameRateValid) {
                    Text(
                        "Integer in ${RuntimeOptions.FRAME_RATE_RANGE}; " +
                            "default ${RuntimeOptions.DEFAULT_FRAME_RATE}.",
                    )
                } else {
                    Text(
                        "Must be an integer in ${RuntimeOptions.FRAME_RATE_RANGE} frames per second.",
                    )
                }
            },
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
            modifier = Modifier.fillMaxWidth(),
        )
        Spacer(Modifier.height(24.dp))

        Text("Archive preload", style = MaterialTheme.typography.titleSmall)
        Spacer(Modifier.height(4.dp))
        Text(
            RuntimeOptions.preloadDescription(),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Switch(
                checked = preloadChecked,
                onCheckedChange = { preloadArchives = it.toString() },
                // The visible "Archive preload" label is a separate node, so
                // name the switch itself for TalkBack.
                modifier = Modifier.semantics { contentDescription = "Archive preload" },
            )
            Spacer(Modifier.width(8.dp))
            Text(
                text = if (preloadChecked) "On" else "Off",
                style = MaterialTheme.typography.bodyLarge,
            )
        }
        if (!preloadKnown) {
            Text(
                text = "This saved value (\"$preloadArchives\") is not one of " +
                    "${RuntimeOptions.PRELOAD_VALUES.joinToString(", ")}; " +
                    "toggle the switch before saving.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
            )
        }
        Spacer(Modifier.height(24.dp))

        Text("Debug overlay", style = MaterialTheme.typography.titleSmall)
        Spacer(Modifier.height(4.dp))
        Text(
            RuntimeOptions.debugOverlayDescription(),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Switch(
                checked = debugChecked,
                onCheckedChange = { debugOverlay = it.toString() },
                // The visible "Debug overlay" label is a separate node, so
                // name the switch itself for TalkBack.
                modifier = Modifier.semantics { contentDescription = "Debug overlay" },
            )
            Spacer(Modifier.width(8.dp))
            Text(
                text = if (debugChecked) "On" else "Off",
                style = MaterialTheme.typography.bodyLarge,
            )
        }
        if (!debugKnown) {
            Text(
                text = "This saved value (\"$debugOverlay\") is not one of " +
                    "${RuntimeOptions.DEBUG_OVERLAY_VALUES.joinToString(", ")}; " +
                    "toggle the switch before saving.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
            )
        }
        Spacer(Modifier.height(24.dp))

        Text("Log level", style = MaterialTheme.typography.titleSmall)
        Spacer(Modifier.height(4.dp))
        Text(
            "How much the engine writes to its log file (krkr.log); " +
                "default: ${RuntimeOptions.DEFAULT_LOG_LEVEL}. " +
                "Use debug or trace only to diagnose a problem — they log the VM's " +
                "exception dumps and per-frame detail, which slows the game and makes " +
                "the log grow and rotate much sooner.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Column(Modifier.selectableGroup()) {
            RuntimeOptions.LOG_LEVELS.forEach { level ->
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .selectable(
                            selected = logLevel == level,
                            onClick = { logLevel = level },
                            role = Role.RadioButton,
                        )
                        .padding(vertical = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    RadioButton(selected = logLevel == level, onClick = null)
                    Spacer(Modifier.width(8.dp))
                    Column {
                        Text(
                            text = level.replaceFirstChar { it.uppercase() },
                            style = MaterialTheme.typography.bodyLarge,
                        )
                        Text(
                            text = RuntimeOptions.logLevelDescription(level),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }
        }
        if (!logLevelKnown) {
            Text(
                text = "This saved value (\"$logLevel\") is not one of " +
                    "${RuntimeOptions.LOG_LEVELS.joinToString(", ")}; choose one before saving.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
            )
        }
        Spacer(Modifier.height(24.dp))

        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            TextButton(onClick = {
                frameRate = RuntimeOptions.DEFAULT_FRAME_RATE.toString()
                preloadArchives = RuntimeOptions.DEFAULT_PRELOAD_ARCHIVES
                debugOverlay = RuntimeOptions.DEFAULT_DEBUG_OVERLAY
                logLevel = RuntimeOptions.DEFAULT_LOG_LEVEL
            }) { Text("Restore defaults") }
            Button(onClick = {
                val fps = frameRate.trim().toIntOrNull()
                if (fps == null || fps !in RuntimeOptions.MIN_FRAME_RATE..RuntimeOptions.MAX_FRAME_RATE) {
                    onNotify(
                        "Frame rate must be an integer in " +
                            "${RuntimeOptions.FRAME_RATE_RANGE} frames per second.",
                    )
                    return@Button
                }
                if (preloadArchives !in RuntimeOptions.PRELOAD_VALUES) {
                    onNotify("Choose one of: ${RuntimeOptions.PRELOAD_VALUES.joinToString(", ")}.")
                    return@Button
                }
                if (debugOverlay !in RuntimeOptions.DEBUG_OVERLAY_VALUES) {
                    onNotify("Choose one of: ${RuntimeOptions.DEBUG_OVERLAY_VALUES.joinToString(", ")}.")
                    return@Button
                }
                if (logLevel !in RuntimeOptions.LOG_LEVELS) {
                    onNotify("Choose one of: ${RuntimeOptions.LOG_LEVELS.joinToString(", ")}.")
                    return@Button
                }
                if (store.write(
                        RuntimeSettings(
                            frameRate = fps.toString(),
                            preloadArchives = preloadArchives,
                            debugOverlay = debugOverlay,
                            logLevel = logLevel,
                        ),
                    )
                ) {
                    onNotify("Settings saved.")
                    onBack()
                } else {
                    onNotify("Could not save the settings; they were not changed.")
                }
            }) { Text("Save") }
        }
        Spacer(Modifier.height(8.dp))
        Text(
            text = "Settings file: ${store.settingsFile().absolutePath}",
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun EmptyLibrary(modifier: Modifier = Modifier, configPath: String) {
    Box(modifier, contentAlignment = Alignment.Center) {
        Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            modifier = Modifier.padding(32.dp),
        ) {
            Text(
                text = "No games yet",
                style = MaterialTheme.typography.headlineSmall,
            )
            Spacer(Modifier.height(8.dp))
            Text(
                text = "Add a folder containing a KiriKiri game (its .xp3 archives and startup.tjs).",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
            )
            Spacer(Modifier.height(24.dp))
            // Show where the library is stored: it is a plain JSON file the
            // user can back up or edit, so it should not be a secret.
            Text(
                text = "Library: $configPath",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
            )
        }
    }
}

@Composable
private fun GameCard(
    entry: GameEntry,
    onLaunch: () -> Unit,
    onRemove: () -> Unit,
) {
    Card(
        modifier = Modifier.fillMaxWidth().clickable(onClick = onLaunch),
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.surfaceContainerHigh,
        ),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(16.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(
                imageVector = Icons.Default.PlayArrow,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.primary,
            )
            Spacer(Modifier.width(16.dp))
            Column(Modifier.weight(1f)) {
                Text(
                    text = entry.name,
                    style = MaterialTheme.typography.titleMedium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Text(
                    text = entry.uri.lastPathSegment ?: entry.uri.toString(),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            IconButton(onClick = onRemove) {
                Icon(Icons.Default.Delete, contentDescription = "Remove ${entry.name}")
            }
        }
    }
}
