package dev.krkr.rs

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.provider.Settings
import androidx.activity.ComponentActivity
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
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.core.content.FileProvider
import dev.krkr.rs.ui.KrkrTheme
import kotlinx.coroutines.launch
import java.io.File

/** How many lines of each log the Logs screen and crash dialogs show. */
private const val LOG_TAIL_LINES = 400

/** How many log lines to attach to an error dialog. */
private const val ISSUE_TAIL_LINES = 25

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
        setContent {
            KrkrTheme {
                LauncherScreen(
                    library = library,
                    permissionGranted = permissionGranted.value,
                    onOpenPermissionSettings = ::openAllFilesSettings,
                    startupIssue = startupIssue.value,
                    onDismissStartupIssue = { startupIssue.value = null },
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
     * A pending-launch marker is consumed here. It is written before
     * `startActivity` and survives the launcher's process being killed with the
     * engine, which is what lets a crash be reported on the next start. Missing
     * or unreadable state is "no information" and dismisses nothing.
     */
    private fun reconcilePendingLaunch() {
        val pending = EngineDiagnostics.readLaunch(this) ?: return
        EngineDiagnostics.clearLaunch(this)
        val state = EngineDiagnostics.readState(this)
        LauncherLog.i("reconciling launch of \"${pending.name}\": state=${state?.state ?: "none"}")

        startupIssue.value = when {
            state == null -> null // no information — carry on
            state.state == "failed" -> Issue(
                title = "The game could not start",
                message = state.message
                    ?: "The engine reported a failure but did not leave a message.",
                detail = LauncherLog.tail(
                    EngineDiagnostics.engineLogFile(this),
                    ISSUE_TAIL_LINES,
                ),
            )

            state.state == "starting" || state.state == "running" -> Issue(
                title = "The game stopped unexpectedly",
                message = "\"${pending.name}\" did not shut down normally. It may have crashed, " +
                    "or Android may have killed it while it was in the background.",
                detail = LauncherLog.tail(
                    EngineDiagnostics.engineLogFile(this),
                    ISSUE_TAIL_LINES,
                ),
            )

            else -> null // "stopped", or an unknown future state — nothing to say
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun LauncherScreen(
    library: GameLibrary,
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
    var showLogs by remember { mutableStateOf(false) }

    // The permission dialog is shown whenever the grant is missing, unless the
    // user explicitly dismissed it ("Not now"). Tapping a game re-opens it.
    var permissionDismissed by remember { mutableStateOf(false) }
    LaunchedEffect(permissionGranted) { if (permissionGranted) permissionDismissed = false }
    val showPermission = !permissionGranted && !permissionDismissed

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
        val persistable = runCatching {
            context.contentResolver.takePersistableUriPermission(
                uri,
                Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION,
            )
        }
        if (persistable.isFailure) {
            // Some providers do not offer persistable grants. The folder can
            // still be used this session, but it will not survive a restart —
            // say so rather than letting it silently disappear later.
            notify("This folder's access cannot be remembered; it may be lost on restart.")
        }

        val name = SafPaths.displayName(context, uri)
        // Resolve now so the config file records where the game actually is;
        // a folder that cannot be addressed as a path is still added (the user
        // may mount that volume later), but we say so immediately.
        val path = SafPaths.resolve(context, uri)
        if (library.add(GameEntry(uri, name, path))) {
            games = library.all()
            if (path != null) {
                notify("Added \"$name\".")
            } else {
                notify("Added \"$name\", but its folder is not on a filesystem the engine can read.")
            }
        } else {
            notify("\"$name\" is already in the library.")
        }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(if (showLogs) "Logs" else "krkr-rs") },
                navigationIcon = {
                    if (showLogs) {
                        IconButton(onClick = { showLogs = false }) {
                            Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                        }
                    }
                },
                actions = {
                    if (!showLogs) {
                        TextButton(onClick = { showLogs = true }) { Text("Logs") }
                    }
                },
            )
        },
        snackbarHost = { SnackbarHost(snackbar) },
        floatingActionButton = {
            if (!showLogs) {
                ExtendedFloatingActionButton(
                    onClick = { pickFolder.launch(null) },
                    icon = { Icon(Icons.Default.Add, contentDescription = null) },
                    text = { Text("Add game") },
                )
            }
        },
    ) { padding ->
        if (showLogs) {
            LogsScreen(Modifier.fillMaxSize().padding(padding))
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
    }

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

    pendingRemoval?.let { entry ->
        AlertDialog(
            onDismissRequest = { pendingRemoval = null },
            title = { Text("Remove from library?") },
            text = {
                Text("\"${entry.name}\" will be removed from this list. The game's files on disk are not touched.")
            },
            confirmButton = {
                TextButton(onClick = {
                    library.remove(entry.uri)
                    games = library.all()
                    pendingRemoval = null
                }) { Text("Remove") }
            },
            dismissButton = {
                TextButton(onClick = { pendingRemoval = null }) { Text("Cancel") }
            },
        )
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
 * The log viewer: the tail of both the launcher and the engine log, with Share
 * and Copy. This is the escape hatch that lets a user report a failure ("here,
 * send me this") without adb.
 */
@Composable
private fun LogsScreen(modifier: Modifier = Modifier) {
    val context = LocalContext.current
    var refreshToken by remember { mutableStateOf(0) }

    val launcherTail = remember(refreshToken) {
        LauncherLog.tail(LauncherLog.file(context), LOG_TAIL_LINES)
    }
    val engineTail = remember(refreshToken) {
        LauncherLog.tail(EngineDiagnostics.engineLogFile(context), LOG_TAIL_LINES)
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
            TextButton(onClick = { shareLogs(context, launcherTail, engineTail) }) { Text("Share") }
            TextButton(onClick = { copyLogs(context, launcherTail, engineTail) }) { Text("Copy") }
        }
        Spacer(Modifier.height(4.dp))
        Text(
            text = "Last $LOG_TAIL_LINES lines, newest last. " +
                LauncherLog.file(context).name + " + " + EngineDiagnostics.engineLogFile(context).name,
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(8.dp))
        Column(Modifier.weight(1f).verticalScroll(rememberScrollState())) {
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

private fun combineLogs(launcherTail: String, engineTail: String): String = buildString {
    appendLine("== krkr-launcher.log ==")
    appendLine(launcherTail.ifBlank { "(empty)" })
    appendLine()
    appendLine("== krkr.log ==")
    appendLine(engineTail.ifBlank { "(empty)" })
}

/** Shares the logs as text, attaching a file through the app's FileProvider. */
private fun shareLogs(context: Context, launcherTail: String, engineTail: String) {
    val text = combineLogs(launcherTail, engineTail)
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
    runCatching { context.startActivity(Intent.createChooser(intent, "Share logs")) }
        .onFailure { LauncherLog.e("cannot share logs", it) }
}

private fun copyLogs(context: Context, launcherTail: String, engineTail: String) {
    val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager ?: return
    clipboard.setPrimaryClip(ClipData.newPlainText("krkr-rs logs", combineLogs(launcherTail, engineTail)))
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

    // Prefer the path cached in `games.json`; re-resolve only when it is
    // missing, which happens for a folder added while its volume was not
    // mounted. A successful re-resolve is written back to the config.
    val path = entry.path ?: SafPaths.resolve(context, entry.uri)?.also {
        library.updatePath(entry.uri, it)
    }
    if (path == null) {
        LauncherLog.w("launch blocked: \"${entry.name}\" has no resolvable path")
        onIssue(
            Issue(
                title = "Cannot open this folder",
                message = "\"${entry.name}\" is not on a filesystem the engine can read. " +
                    "This happens with cloud storage — copy the game to internal storage or an SD card.",
            ),
        )
        return
    }

    val problem = GamePreflight.check(path)
    if (problem != null) {
        LauncherLog.w("launch blocked: \"${entry.name}\" — ${problem.title} ($path)")
        onIssue(problem)
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
                color = MaterialTheme.colorScheme.outline,
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
