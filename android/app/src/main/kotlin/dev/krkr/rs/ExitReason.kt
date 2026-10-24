package dev.krkr.rs

import android.app.ActivityManager
import android.app.ApplicationExitInfo
import android.content.Context

/**
 * Why the app's process ended, in the launcher's own words.
 *
 * Android keeps a short history of the ways the app's process died
 * (`ActivityManager.getHistoricalProcessExitReasons`), and every record
 * carries a reason code, the exit status and the system's own description.
 * The codes are the platform's; this enum is the launcher's vocabulary, so
 * the sentence the user reads is written in one place and can be checked
 * without a device.
 */
enum class ExitReason {
    /** The app's own native code crashed. */
    NATIVE_CRASH,

    /** The app's own Java code threw an unhandled exception. */
    CRASH,

    /** Android killed the app because the device was low on memory. */
    LOW_MEMORY,

    /** Android killed the app because it used more resources than it may. */
    RESOURCE_LIMIT,

    /** The app was killed by a system signal. */
    SIGNALED,

    /** Android killed the app because it stopped responding. */
    ANR,

    /** The app failed while it was starting. */
    START_FAILED,

    /** The app was closed by the user. */
    USER_REQUESTED,

    /** Android stopped the app for a reason the launcher does not name. */
    OTHER,

    /** Android recorded a reason the launcher does not know. */
    UNKNOWN;

    /**
     * The sentence the user reads for this reason.
     *
     * [signal] is the signal that ended the process, when Android recorded
     * one; it is the number a crash report needs to identify the fault.
     */
    fun describe(signal: Int?): String = when (this) {
        ExitReason.NATIVE_CRASH ->
            "The game crashed with a native error" + signalSuffix(signal) + "."
        ExitReason.CRASH ->
            "The game crashed."
        ExitReason.LOW_MEMORY ->
            "Android killed the app because the device was running out of memory."
        ExitReason.RESOURCE_LIMIT ->
            "Android killed the app because it was using too many resources."
        ExitReason.SIGNALED ->
            "The app was stopped by a system signal" + signalSuffix(signal) + "."
        ExitReason.ANR ->
            "Android killed the app because it stopped responding."
        ExitReason.START_FAILED ->
            "The app failed while it was starting."
        ExitReason.USER_REQUESTED ->
            "The app was closed by the user, for example swiped away from the app list."
        ExitReason.OTHER ->
            "Android stopped the app for another reason."
        ExitReason.UNKNOWN ->
            "Android did not record why the app stopped."
    }
}

/** The signal a process was ended by, when there was one. */
private fun signalSuffix(signal: Int?): String =
    if (signal != null && signal > 0) " (signal $signal)" else ""

/**
 * Reads Android's record of why the app's process ended.
 *
 * The record is the only evidence of *how* the process died: the state file
 * says that it stopped without shutting down, and Android says whether that
 * was a crash, a kill by the system, or the user closing the app.
 */
object ProcessExit {

    /**
     * What the launcher knows about the process's last ending.
     */
    sealed interface Ending {

        /** Android recorded why the process ended. */
        data class Recorded(
            val reason: ExitReason,
            val signal: Int?,
            val description: String?,
        ) : Ending

        /** Android has no record of the process ending. */
        data object None : Ending

        /** The record could not be read. */
        data class Unreadable(val message: String) : Ending
    }

    /**
     * Reads the most recent exit record of [pid].
     *
     * [pid] is the process that died — the one the state file names, which
     * after a crash is not the launcher's own process. A null [pid] asks for
     * the most recent record of the app whatever process it belonged to.
     *
     * A record that does not exist is not an error: Android keeps the records
     * in a small ring buffer, and a process that is still running has no
     * record at all. A read that fails is reported, and the caller says that
     * instead of guessing why the app stopped.
     */
    fun read(context: Context, pid: Int?): Ending {
        val manager = context.getSystemService(Context.ACTIVITY_SERVICE) as? ActivityManager
        if (manager == null) {
            LauncherLog.w("activity manager unavailable; cannot read the exit reason")
            return Ending.Unreadable("the system activity manager is not available")
        }
        val records = try {
            manager.getHistoricalProcessExitReasons(context.packageName, pid ?: 0, 1)
        } catch (error: Exception) {
            LauncherLog.e("cannot read the historical exit reasons", error)
            return Ending.Unreadable(error.message ?: error.javaClass.simpleName)
        }
        val record = records.firstOrNull()
        if (record == null) {
            LauncherLog.i("no exit information for pid=$pid")
            return Ending.None
        }
        LauncherLog.i(
            "exit record for pid=${record.pid}: reason=${record.reason} status=${record.status}",
        )
        return Ending.Recorded(
            reason = reasonOf(record.reason),
            signal = signalOf(record),
            description = record.description?.toString()?.takeIf { it.isNotBlank() },
        )
    }

    /**
     * The launcher's reason for an Android reason code.
     *
     * The codes this launcher does not name are reported as [ExitReason.OTHER]:
     * Android stopped the app for a reason it alone knows.
     */
    private fun reasonOf(code: Int): ExitReason = when (code) {
        ApplicationExitInfo.REASON_CRASH_NATIVE -> ExitReason.NATIVE_CRASH
        ApplicationExitInfo.REASON_CRASH -> ExitReason.CRASH
        ApplicationExitInfo.REASON_LOW_MEMORY -> ExitReason.LOW_MEMORY
        ApplicationExitInfo.REASON_EXCESSIVE_RESOURCE_USAGE -> ExitReason.RESOURCE_LIMIT
        ApplicationExitInfo.REASON_MEMORY_LIMITER -> ExitReason.RESOURCE_LIMIT
        ApplicationExitInfo.REASON_ANOMALY -> ExitReason.RESOURCE_LIMIT
        ApplicationExitInfo.REASON_SIGNALED -> ExitReason.SIGNALED
        ApplicationExitInfo.REASON_ANR -> ExitReason.ANR
        ApplicationExitInfo.REASON_INITIALIZATION_FAILURE -> ExitReason.START_FAILED
        ApplicationExitInfo.REASON_USER_REQUESTED -> ExitReason.USER_REQUESTED
        ApplicationExitInfo.REASON_UNKNOWN -> ExitReason.UNKNOWN
        else -> ExitReason.OTHER
    }

    /**
     * The signal that ended the process, for the reasons that carry one.
     *
     * Android reports the terminating signal in the exit status for a
     * process that was signalled and for a native crash; for the other
     * reasons the status is an exit code and means nothing here.
     */
    private fun signalOf(record: ApplicationExitInfo): Int? {
        val status = record.status
        if (status <= 0) return null
        return when (record.reason) {
            ApplicationExitInfo.REASON_SIGNALED, ApplicationExitInfo.REASON_CRASH_NATIVE -> status
            else -> null
        }
    }
}

/**
 * The report the launcher shows when the process ended without a clean stop.
 */
data class ExitReport(
    /** The dialog's title. */
    val title: String,
    /** The dialog's message: what happened, in plain English. */
    val message: String,
)

/**
 * Builds the report for [gameName], the game that was running.
 *
 * A recorded reason names the fault; a missing record says so instead of
 * claiming a crash, and a record that could not be read says that too.
 */
fun ProcessExit.Ending.report(gameName: String): ExitReport = when (this) {
    is ProcessExit.Ending.Recorded -> {
        val closed = reason == ExitReason.USER_REQUESTED
        ExitReport(
            title = if (closed) "The game was closed" else "The game stopped unexpectedly",
            message = buildString {
                append(
                    if (closed) {
                        "\"$gameName\" was closed."
                    } else {
                        "\"$gameName\" stopped unexpectedly."
                    },
                )
                append("\n\n")
                append(reason.describe(signal))
                description?.let { append("\n\n").append(it) }
            },
        )
    }

    ProcessExit.Ending.None -> ExitReport(
        title = "The game stopped unexpectedly",
        message = "\"$gameName\" stopped unexpectedly.\n\n" +
            "Android has no record of why the app stopped.",
    )

    is ProcessExit.Ending.Unreadable -> ExitReport(
        title = "The game stopped unexpectedly",
        message = "\"$gameName\" stopped unexpectedly.\n\n" +
            "Android's record of why the app stopped could not be read: $message",
    )
}
