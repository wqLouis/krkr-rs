package dev.krkr.rs

import android.content.Context
import android.util.Log
import java.io.File
import java.io.RandomAccessFile
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/**
 * The launcher's own log.
 *
 * The launcher used to be silent: a problem either showed a one-line snackbar
 * that vanished, or (for a JVM exception) killed the process and left a tombstone
 * in logcat that nobody would see on a phone. This writes the same lines to both
 * logcat (tag [TAG]) and `<filesDir>/krkr-launcher.log`, so whatever the user
 * sees on screen can be sent to us as a file.
 *
 * It is deliberately tiny and allocation-light: a log call must never throw into
 * the code path it is describing, so every file operation is wrapped in
 * `runCatching`. The engine's log (`krkr.log`) is written by the Rust side and is
 * read — not written — here.
 *
 * Files:
 *   * launcher log: `<filesDir>/krkr-launcher.log`, rotated once at [MAX_BYTES];
 *   * engine log:   `<filesDir>/krkr.log` (Rust-owned, rotated by Rust).
 */
object LauncherLog {

    /** Logcat tag shared by every launcher-side message. */
    const val TAG = "krkr-launcher"

    private const val FILE_NAME = "krkr-launcher.log"
    private const val ROTATED_NAME = "krkr-launcher.log.1"

    /** Rotate the launcher log above this size, keeping one previous file. */
    private const val MAX_BYTES = 512L * 1024L

    private val lock = Any()
    private val timeFormat = SimpleDateFormat("yyyy-MM-dd HH:mm:ss.SSS", Locale.US)

    @Volatile
    private var logFile: File? = null

    @Volatile
    private var handlerInstalled = false

    /**
     * Points the log at [context]'s private files directory. Safe to call more
     * than once (e.g. after an Activity recreation); the first call wins.
     */
    fun init(context: Context) {
        if (logFile == null) logFile = File(context.filesDir, FILE_NAME)
    }

    /** The launcher log file, even before [init] has run. */
    fun file(context: Context): File = File(context.filesDir, FILE_NAME)

    fun d(message: String) = record(Log.DEBUG, "D", message, null)
    fun i(message: String) = record(Log.INFO, "I", message, null)
    fun w(message: String) = record(Log.WARN, "W", message, null)
    fun e(message: String, throwable: Throwable? = null) = record(Log.ERROR, "E", message, throwable)

    private fun record(priority: Int, level: String, message: String, throwable: Throwable?) {
        when (priority) {
            Log.DEBUG -> Log.d(TAG, message, throwable)
            Log.INFO -> Log.i(TAG, message, throwable)
            Log.WARN -> Log.w(TAG, message, throwable)
            else -> Log.e(TAG, message, throwable)
        }
        append(level, message, throwable)
    }

    private fun append(level: String, message: String, throwable: Throwable?) {
        val file = logFile ?: return
        val stack = throwable?.let { Log.getStackTraceString(it) }
        synchronized(lock) {
            runCatching {
                file.parentFile?.mkdirs()
                if (file.length() > MAX_BYTES) {
                    val rotated = File(file.parentFile, ROTATED_NAME)
                    rotated.delete()
                    file.renameTo(rotated)
                }
                val stamp = timeFormat.format(Date())
                file.appendText("$stamp $level $message\n")
                if (stack != null) {
                    file.appendText(stack)
                    if (!stack.endsWith("\n")) file.appendText("\n")
                }
            }
        }
    }

    /**
     * Logs an uncaught JVM exception, then hands it to the previously-installed
     * handler so Android's normal "app has stopped" behaviour is preserved.
     *
     * This cannot catch a native crash (the engine is Rust): those leave the
     * `krkr_state.json` file in `starting`/`running`, which is what the launch
     * bookkeeping in [EngineDiagnostics] detects instead.
     */
    fun installUncaughtExceptionHandler() {
        if (handlerInstalled) return
        handlerInstalled = true
        val previous = Thread.getDefaultUncaughtExceptionHandler()
        Thread.setDefaultUncaughtExceptionHandler { thread, throwable ->
            e("Uncaught exception on thread \"${thread.name}\"", throwable)
            previous?.uncaughtException(thread, throwable)
        }
    }

    /**
     * The last [maxLines] lines of [file], oldest first, or "" if it is missing
     * or unreadable.
     *
     * Reads at most [maxBytes] from the end, so a multi-megabyte log costs a
     * bounded read. If the read starts mid-file the first (partial) line is
     * dropped. Never throws: a diagnostics screen that can itself crash is worse
     * than no screen.
     */
    fun tail(file: File, maxLines: Int, maxBytes: Int = 256 * 1024): String {
        if (!file.isFile) return ""
        return runCatching {
            RandomAccessFile(file, "r").use { raf ->
                val length = raf.length()
                if (length == 0L) return ""
                val start = (length - maxBytes).coerceAtLeast(0L)
                raf.seek(start)
                val bytes = ByteArray((length - start).toInt())
                raf.readFully(bytes)
                val text = String(bytes, Charsets.UTF_8)
                val lines = text.split('\n')
                val usable = if (start > 0L) lines.drop(1) else lines
                usable.takeLast(maxLines).joinToString("\n").trimEnd('\n')
            }
        }.getOrDefault("")
    }
}
