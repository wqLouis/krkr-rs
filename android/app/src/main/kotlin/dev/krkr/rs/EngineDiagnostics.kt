package dev.krkr.rs

import android.content.Context
import org.json.JSONObject
import java.io.File

/**
 * The engine's own state, as published by the Rust side.
 *
 * The contract (implemented on the Rust side in `crates/android`) is a single
 * JSON object at `<filesDir>/krkr_state.json`:
 *
 * ```json
 * { "state": "starting", "message": null, "pid": 12345, "at": "2025-..." }
 * ```
 *
 * `state` is one of `starting`, `running`, `failed`, `stopped`. The launcher
 * reads it when it resumes after a game, which is how it tells the three
 * interesting cases apart:
 *
 *  * `failed`             — the engine reported a specific error ([message]);
 *  * `starting`/`running` — a crash or a kill: the process never got to write
 *                           `stopped`, so the file is left mid-flight;
 *  * `stopped`            — a normal exit, nothing to report.
 *
 * The directory is `<filesDir>` because
 * [KrkrGameActivity.pendingStateDir] is set to `filesDir.absolutePath` before
 * `super.onCreate` — the write that makes the native side's read race-free.
 *
 * Reading is deliberately forgiving: a missing, empty or corrupt file yields
 * `null` ("no information"), never an exception. Losing the state file must not
 * take the launcher with it.
 */
data class EngineState(
    val state: String,
    val message: String?,
    val pid: Int?,
    val at: String?,
)

/**
 * A game launch the launcher has started and not yet reconciled.
 *
 * Written just before `startActivity`, read (and cleared) the next time the
 * launcher resumes. It exists because the launcher and the engine share one
 * process: if the engine dies, so does the launcher, so an in-memory flag
 * cannot survive to report the failure. This marker can.
 */
data class PendingLaunch(
    val name: String,
    val path: String?,
    val at: Long,
)

/**
 * File locations and tolerant readers for the engine state, the engine log and
 * the pending-launch marker. See [EngineState] for the JSON contract.
 */
object EngineDiagnostics {

    const val STATE_FILE = "krkr_state.json"
    const val ENGINE_LOG = "krkr.log"
    const val LAUNCH_FILE = "krkr-launch-pending.json"

    fun stateFile(context: Context): File = File(context.filesDir, STATE_FILE)

    fun engineLogFile(context: Context): File = File(context.filesDir, ENGINE_LOG)

    /**
     * Every engine log file, current first (`krkr.log`), then its rotations in
     * order (`krkr.log.1`, `krkr.log.2`, …).
     *
     * The list is discovered from the files directory rather than a fixed pair
     * or a Kotlin copy of the retention count, so it cannot drift from the Rust
     * sink's policy: a new rotation simply appears, and one that is absent is
     * simply not listed. The launcher's own `krkr-launcher.log` does not match —
     * it starts with `krkr-`, not `krkr.log`.
     */
    fun engineLogFiles(context: Context): List<File> =
        context.filesDir.listFiles().orEmpty()
            .filter { it.isFile && isEngineLogName(it.name) }
            .sortedBy { engineLogOrder(it.name) }

    /**
     * The raw state file for display, or "" when it is missing or unreadable.
     * Never throws: the state file's `message` is the most useful line for a
     * failed title, so the Logs screen shows it as written, without parsing it.
     */
    fun stateText(context: Context): String {
        val file = stateFile(context)
        if (!file.isFile) return ""
        return runCatching { file.readText() }.getOrDefault("")
    }

    /**
     * The pending-launch marker file. Exposed so reconciliation can compare its
     * modification time against the engine state file's (same filesystem, same
     * clock) and tell this run's state apart from a stale leftover.
     */
    fun launchFile(context: Context): File = File(context.filesDir, LAUNCH_FILE)

    /**
     * The engine's last published state, or null when there is none we can
     * understand. Never throws.
     */
    fun readState(context: Context): EngineState? {
        val file = stateFile(context)
        if (!file.isFile) return null
        return runCatching {
            val obj = JSONObject(file.readText())
            val state = obj.optString("state")
            if (state.isBlank() || state == "null") return null
            EngineState(
                state = state,
                message = if (obj.isNull("message")) null
                else obj.optString("message").takeIf { it.isNotBlank() && it != "null" },
                pid = if (obj.isNull("pid")) null else obj.optInt("pid"),
                at = if (obj.isNull("at")) null
                else obj.optString("at").takeIf { it.isNotBlank() && it != "null" },
            )
        }.getOrNull()
    }

    /** Records that a launch is in flight. Failure here is logged, never fatal. */
    fun markLaunch(context: Context, name: String, path: String?) {
        runCatching {
            val obj = JSONObject()
                .put("name", name)
                .put("at", System.currentTimeMillis())
            path?.let { obj.put("path", it) }

            DurableWrite.text(launchFile(context), obj.toString())
        }.onFailure { LauncherLog.w("cannot record pending launch: ${it.message}") }
    }

    /** The in-flight launch, or null. Never throws. */
    fun readLaunch(context: Context): PendingLaunch? {
        val file = launchFile(context)
        if (!file.isFile) return null
        return runCatching {
            val obj = JSONObject(file.readText())
            PendingLaunch(
                name = obj.optString("name").takeIf { it.isNotBlank() } ?: "The game",
                path = obj.optString("path").takeIf { it.isNotBlank() },
                at = obj.optLong("at"),
            )
        }.getOrNull()
    }

    /** Forgets the in-flight launch (after it has been reported or reconciled). */
    fun clearLaunch(context: Context) {
        runCatching { launchFile(context).delete() }
    }
}

/** One engine log name: `krkr.log` or `krkr.log.<digits>`. */
private fun isEngineLogName(name: String): Boolean {
    if (name == EngineDiagnostics.ENGINE_LOG) return true
    val suffix = name.removePrefix("${EngineDiagnostics.ENGINE_LOG}.")
    return suffix != name && suffix.isNotEmpty() && suffix.all { it.isDigit() }
}

/** Sort key: the current file first, then rotation slot order. */
private fun engineLogOrder(name: String): Int {
    if (name == EngineDiagnostics.ENGINE_LOG) return 0
    return name.removePrefix("${EngineDiagnostics.ENGINE_LOG}.").toIntOrNull() ?: Int.MAX_VALUE
}
