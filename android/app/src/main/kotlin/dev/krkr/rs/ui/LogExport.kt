package dev.krkr.rs.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.core.content.FileProvider
import dev.krkr.rs.EngineDiagnostics
import dev.krkr.rs.LauncherLog
import java.io.File
import java.io.OutputStream
import java.util.Locale

/** File name offered to the system document picker for the combined export. */
internal const val EXPORT_FILE_NAME = "krkr-rs-logs.txt"

/** Cache subdirectory the full-log Share action stages its copies in. */
private const val CACHE_DIR = "share"

/**
 * One diagnostics file in the full-log export: the engine state, an engine log
 * or a launcher log.
 *
 * [displayName] is the on-disk name shown to the user; [shareName] is the
 * unambiguous name used in the cache and the share sheet, where `krkr.log` and
 * `krkr.log.1` would otherwise be easy to confuse.
 */
internal data class LogSource(
    val displayName: String,
    val shareName: String,
    val file: File,
)

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
internal fun fullLogSources(context: Context): List<LogSource> = buildList {
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
internal fun fullLogSummaries(sources: List<LogSource>): List<String> = sources.map { source ->
    if (source.file.isFile) {
        "${source.displayName} — ${formatBytes(source.file.length())} — ${source.file.absolutePath}"
    } else {
        "${source.displayName} — not created yet"
    }
}

/** The display names of the sources that are absent or empty. */
internal fun missingLogNames(sources: List<LogSource>): List<String> =
    sources
        .filter { !it.file.isFile || it.file.length() == 0L }
        .map { it.displayName }

/** The combined size of the files that are present, for the export notice. */
internal fun totalLogBytes(sources: List<LogSource>): Long = sources.sumOf { source ->
    if (source.file.isFile) source.file.length() else 0L
}

/** Formats [bytes] as B, KiB or MiB for the Logs screen. */
internal fun formatBytes(bytes: Long): String {
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
 * The tail of both logs and the engine state, as the text the Share and Copy
 * actions send.
 */
internal fun combineLogs(
    launcherTail: String,
    engineTail: String,
    stateText: String,
): String =
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
internal fun shareLogs(
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
            val dir = File(context.cacheDir, CACHE_DIR).apply { mkdirs() }
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

/**
 * Copies the logs to the clipboard. Returns false when the clipboard is
 * unavailable or the copy failed, so the caller can say so.
 */
internal fun copyLogs(
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
 * Shares every present log as a file, copying each into the cache first.
 *
 * An absent or empty log is named as missing rather than failing the whole
 * share; when no log exists at all, nothing is opened and the user is told.
 * Returns the message to show, including why a share did not start.
 */
internal fun shareFullLogs(context: Context): String {
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
internal fun writeFullLog(context: Context, uri: Uri): String? {
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
