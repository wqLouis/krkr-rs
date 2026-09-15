package dev.krkr.rs

import java.io.File

/**
 * A user-facing problem, rendered as a Material 3 dialog.
 *
 * [detail] is optional extra context (typically the last lines of the engine
 * log) shown in a small, scrollable block below the message.
 */
data class Issue(
    val title: String,
    val message: String,
    val detail: String? = null,
)

/**
 * Checks a resolved game directory before the engine is started.
 *
 * Every failure returns a specific [Issue] instead of letting the engine run
 * into an `EACCES`/`ENOENT` and showing a black surface. The checks are ordered
 * cheapest-first and mirror what the engine's storage layer will do:
 *
 *  1. the path exists;
 *  2. it is a directory;
 *  3. it is readable (this is where a missing "All files access" usually shows
 *     up, after the permission gate in `MainActivity` has already run);
 *  4. it looks like a KiriKiri game: `startup.tjs`, or at least one `.xp3`.
 */
object GamePreflight {

    fun check(path: String): Issue? {
        val dir = File(path)
        return when {
            !dir.exists() -> Issue(
                title = "Game folder not found",
                message = "\"$path\" no longer exists. If the game is on a removable SD card, " +
                    "reinsert it or add the game again.",
            )

            !dir.isDirectory -> Issue(
                title = "That is not a folder",
                message = "\"$path\" is not a directory, so there is nothing for the engine to mount.",
            )

            !dir.canRead() || dir.listFiles() == null -> Issue(
                title = "Game folder is not readable",
                message = "The app cannot read \"$path\".\n\nThis usually means \"All files access\" " +
                    "is off, or the folder's permission was lost. Enable All files access and try again.",
            )

            else -> notAGame(dir)
        }
    }

    private fun notAGame(dir: File): Issue? {
        val listing = dir.listFiles() ?: return null
        val hasStartup = File(dir, "startup.tjs").isFile
        val hasXp3 = listing.any { it.isFile && it.name.endsWith(".xp3", ignoreCase = true) }
        if (hasStartup || hasXp3) return null
        return Issue(
            title = "Not a KiriKiri game",
            message = "No startup.tjs and no .xp3 archives were found in \"${dir.absolutePath}\".\n\n" +
                "Pick the folder that directly contains the game's startup.tjs or .xp3 files.",
        )
    }
}
