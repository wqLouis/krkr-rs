@file:OptIn(ExperimentalMaterial3Api::class)

package dev.krkr.rs.ui

import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Share
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import dev.krkr.rs.EngineDiagnostics
import dev.krkr.rs.LauncherLog

/** How many lines of each log the Logs screen shows. */
private const val LOG_TAIL_LINES = 400

/**
 * The log viewer: the tail of both the launcher and the engine log for reading
 * on screen, plus the full-log Share and Export actions. The tail Share/Copy
 * stay for a quick paste; Share all sends every log file through the share
 * sheet and Export writes one combined file to a document the user picks. This
 * is the escape hatch that lets a user report a failure without adb.
 */
@Composable
fun LogsScreen(
    snackbarHostState: SnackbarHostState,
    onNotify: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val context = LocalContext.current
    var refreshToken by remember { mutableStateOf(0) }
    var filter by rememberSaveable { mutableStateOf(LogFilter.ALL) }
    var menuOpen by remember { mutableStateOf(false) }

    val launcherTail = remember(refreshToken) {
        LauncherLog.tail(LauncherLog.file(context), LOG_TAIL_LINES)
    }
    val engineTail = remember(refreshToken) {
        LauncherLog.tail(EngineDiagnostics.engineLogFile(context), LOG_TAIL_LINES)
    }
    val stateText = remember(refreshToken) { EngineDiagnostics.stateText(context) }
    val sources = remember(refreshToken) { fullLogSources(context) }
    val fileSummaries = remember(refreshToken) { fullLogSummaries(sources) }
    val totalBytes = remember(refreshToken) { totalLogBytes(sources) }
    val rows = remember(refreshToken) { logRows(launcherTail, engineTail) }

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
        val missing = missingLogNames(sources)
        val size = formatBytes(totalBytes)
        onNotify(
            if (missing.isEmpty()) {
                "Exported the full log ($size)."
            } else {
                "Exported the log ($size); missing: ${missing.joinToString(", ")}."
            },
        )
    }

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text("Logs") },
                actions = {
                    IconButton(onClick = { refreshToken++ }) {
                        Icon(
                            imageVector = Icons.Default.Refresh,
                            contentDescription = "Reload the logs",
                        )
                    }
                    IconButton(onClick = {
                        if (!shareLogs(context, launcherTail, engineTail, stateText)) {
                            onNotify("Could not open the share sheet.")
                        }
                    }) {
                        Icon(
                            imageVector = Icons.Default.Share,
                            contentDescription = "Share the logs",
                        )
                    }
                    Box {
                        IconButton(onClick = { menuOpen = true }) {
                            Icon(
                                imageVector = Icons.Default.MoreVert,
                                contentDescription = "More log actions",
                            )
                        }
                        DropdownMenu(
                            expanded = menuOpen,
                            onDismissRequest = { menuOpen = false },
                        ) {
                            DropdownMenuItem(
                                text = { Text("Copy logs") },
                                onClick = {
                                    menuOpen = false
                                    if (copyLogs(context, launcherTail, engineTail, stateText)) {
                                        onNotify("Logs copied.")
                                    } else {
                                        onNotify("Could not copy the logs.")
                                    }
                                },
                            )
                            DropdownMenuItem(
                                text = { Text("Share all logs") },
                                onClick = {
                                    menuOpen = false
                                    onNotify(shareFullLogs(context))
                                },
                            )
                            DropdownMenuItem(
                                text = { Text("Export logs") },
                                onClick = {
                                    menuOpen = false
                                    exportLog.launch(EXPORT_FILE_NAME)
                                },
                            )
                        }
                    }
                },
            )
        },
        snackbarHost = { SnackbarHost(snackbarHostState) },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding),
        ) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp, vertical = 8.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                for (option in LogFilter.entries) {
                    FilterChip(
                        selected = filter == option,
                        onClick = { filter = option },
                        label = { Text(option.label) },
                    )
                }
            }

            LazyColumn(
                modifier = Modifier.fillMaxSize(),
                contentPadding = PaddingValues(start = 16.dp, end = 16.dp, bottom = 16.dp),
            ) {
                item(key = "summary") {
                    Card(
                        colors = CardDefaults.cardColors(
                            containerColor = MaterialTheme.colorScheme.surfaceContainerLow,
                        ),
                        elevation = CardDefaults.cardElevation(defaultElevation = 0.dp),
                    ) {
                        Column(Modifier.padding(16.dp)) {
                            Text(
                                text = "Engine state",
                                style = MaterialTheme.typography.titleSmall,
                                color = MaterialTheme.colorScheme.primary,
                            )
                            Text(
                                text = stateText.ifBlank { "(no state file yet)" },
                                style = MaterialTheme.typography.bodySmall,
                                fontFamily = FontFamily.Monospace,
                            )
                            Spacer(Modifier.height(12.dp))
                            Text(
                                text = "Files",
                                style = MaterialTheme.typography.titleSmall,
                                color = MaterialTheme.colorScheme.primary,
                            )
                            for (summary in fileSummaries) {
                                Text(
                                    text = summary,
                                    style = MaterialTheme.typography.labelSmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                            Text(
                                text = "Export total: ${formatBytes(totalBytes)} across " +
                                    "${sources.size} files (the export writes every file whole; " +
                                    "nothing is truncated).",
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                            Text(
                                text = "The list shows the last $LOG_TAIL_LINES lines of each " +
                                    "log, newest last.",
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                }

                rows.forEachIndexed { index, row ->
                    val visible = when (row) {
                        is LogRow.Header -> true
                        is LogRow.Line -> filter.accepts(row.level)
                    }
                    if (visible) {
                        item(key = index) {
                            LogRowView(row)
                        }
                    }
                }
            }
        }
    }
}

/**
 * One row of the log list, as it is drawn.
 */
@Composable
private fun LogRowView(row: LogRow) {
    when (row) {
        is LogRow.Header -> Text(
            text = row.title,
            style = MaterialTheme.typography.titleSmall,
            color = MaterialTheme.colorScheme.primary,
            modifier = Modifier.padding(top = 12.dp, bottom = 4.dp),
        )

        is LogRow.Line -> Text(
            text = row.text,
            style = MaterialTheme.typography.bodySmall,
            fontFamily = FontFamily.Monospace,
            color = logLineColor(row.level),
            modifier = Modifier
                .fillMaxWidth()
                .padding(vertical = 1.dp),
        )
    }
}

/**
 * The colour a log line is drawn in: the error role for an error, the tertiary
 * role for a warning, and the ordinary text colour otherwise.
 */
@Composable
private fun logLineColor(level: LogLevel?): Color = when (level) {
    LogLevel.ERROR -> MaterialTheme.colorScheme.error
    LogLevel.WARN -> MaterialTheme.colorScheme.tertiary
    else -> MaterialTheme.colorScheme.onSurface
}

/**
 * One row of the log list.
 */
private sealed interface LogRow {

    /** The name of the file the lines below it come from. */
    data class Header(val title: String) : LogRow

    /** One line of a log file, with the level it was recorded at. */
    data class Line(val text: String, val level: LogLevel?) : LogRow
}

/**
 * The severity a log line was recorded at.
 */
private enum class LogLevel { ERROR, WARN, INFO, DEBUG, TRACE }

/**
 * Which lines the list shows.
 */
private enum class LogFilter(val label: String) {
    ALL("All"),
    ERRORS("Errors"),
    WARNINGS("Warnings");

    /**
     * Whether a line recorded at [level] belongs in the list.
     *
     * A line without a level is always shown: it is either the first line of
     * a file whose format carries none, or a line that follows no recorded
     * line, and a filter must never hide a whole file.
     */
    fun accepts(level: LogLevel?): Boolean = when {
        level == null -> true
        this == ALL -> true
        this == ERRORS -> level == LogLevel.ERROR
        else -> level == LogLevel.ERROR || level == LogLevel.WARN
    }
}

/** The launcher's own log line: a timestamp, a one-letter level, a message. */
private val LAUNCHER_LEVEL = Regex("""^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3} ([DIWE]) """)

/** The engine's log line: an RFC 3339 timestamp, a level name, a message. */
private val ENGINE_LEVEL = Regex("""^\S+ (ERROR|WARN|INFO|DEBUG|TRACE)\b""")

/**
 * The level a log line was recorded at, or null when the line carries none.
 */
private fun levelOf(line: String): LogLevel? {
    val launcher = LAUNCHER_LEVEL.find(line)
    if (launcher != null) return levelFor(launcher.groupValues[1])
    val engine = ENGINE_LEVEL.find(line)
    if (engine != null) return levelFor(engine.groupValues[1])
    return null
}

/**
 * The level a level token names.
 */
private fun levelFor(token: String): LogLevel? = when (token) {
    "E", "ERROR" -> LogLevel.ERROR
    "W", "WARN" -> LogLevel.WARN
    "I", "INFO" -> LogLevel.INFO
    "D", "DEBUG" -> LogLevel.DEBUG
    "T", "TRACE" -> LogLevel.TRACE
    else -> null
}

/**
 * The log list: the launcher's log and the engine's log, each under a header.
 */
private fun logRows(launcherTail: String, engineTail: String): List<LogRow> = buildList {
    add(LogRow.Header("krkr-launcher.log"))
    addAll(levelRows(launcherTail))
    add(LogRow.Header("krkr.log"))
    addAll(levelRows(engineTail))
}

/**
 * The lines of one log file, each with the level it was recorded at.
 *
 * A line that carries no level of its own is a continuation of the line
 * before it — a stack trace, for instance — and inherits that line's level.
 */
private fun levelRows(text: String): List<LogRow> {
    if (text.isBlank()) return listOf(LogRow.Line("(no entries)", null))
    var previous: LogLevel? = null
    return text.split('\n').map { raw ->
        val line = raw.trimEnd('\r')
        val detected = levelOf(line)
        val level = detected ?: previous
        if (detected != null) previous = detected
        LogRow.Line(line, level)
    }
}
