package dev.krkr.rs

import android.content.Context
import android.net.Uri
import android.util.Log
import org.json.JSONArray
import org.json.JSONObject
import java.io.File

/**
 * One game in the library.
 *
 * [uri] is the SAF tree URI — the **durable identity** of the folder, and the
 * handle the persisted URI permission is granted against. [path] is the
 * filesystem path the engine is actually handed (`docs/android.md` §4);
 * it is cached because resolving it needs a `ContentResolver` query, and is
 * nullable because a provider may not be addressable as a path at all.
 */
data class GameEntry(
    val uri: Uri,
    val name: String,
    val path: String?,
) {
    val key: String get() = uri.toString()
}

/**
 * The result of a library mutation. [FAILED] is deliberately distinct from
 * [UNCHANGED] so a failed save is never reported to the user as "already
 * there".
 */
enum class SaveResult { SAVED, UNCHANGED, FAILED }

/**
 * The game library, persisted as **JSON** in the app's private files directory.
 *
 * This is deliberately a plain file rather than `SharedPreferences`: it is
 * inspectable, portable, user-editable, and consistent with the rest of the
 * project, which stores configuration as JSON (`crates/tvp-config` deliberately
 * replaced the reference engine's XML preference files with JSON). The library
 * is the one piece of user state the launcher owns — losing it would mean
 * re-picking every game folder, which is exactly what it exists to prevent.
 *
 * File: `<app files dir>/games.json`
 *
 * ```json
 * {
 *   "version": 1,
 *   "games": [
 *     {
 *       "name": "不可视之药",
 *       "treeUri": "content://com.android.externalstorage.documents/tree/primary%3AGames",
 *       "path": "/storage/emulated/0/Games"
 *     }
 *   ]
 * }
 * ```
 *
 * Writes are atomic (temp file + rename) so a crash or a kill during a write
 * cannot leave a truncated library behind. Reads are tolerant: a missing file
 * is an empty library, and a corrupt one is reported and treated as empty
 * rather than crashing the launcher on every start.
 */
class GameLibrary(context: Context) {

    private val file = File(context.filesDir, FILE_NAME)

    /** The backing config file, exposed so the UI can show where state lives. */
    fun configFile(): File = file

    /** Every entry, in library order. Never throws. */
    fun all(): List<GameEntry> {
        if (!file.isFile) return emptyList()
        val text = runCatching { file.readText() }.getOrElse { e ->
            Log.w(TAG, "cannot read $file: ${e.message}")
            return emptyList()
        }
        return parse(text)
    }

    /**
     * Adds [entry] unless its folder is already present. [SaveResult.FAILED]
     * means the entry was not persisted, so the caller must tell the user.
     */
    fun add(entry: GameEntry): SaveResult {
        val entries = all()
        if (entries.any { it.key == entry.key }) return SaveResult.UNCHANGED
        return if (write(entries + entry)) SaveResult.SAVED else SaveResult.FAILED
    }

    /**
     * Removes [uri]. [SaveResult.FAILED] means the library file could not be
     * written, so the removal did not actually stick.
     */
    fun remove(uri: Uri): SaveResult {
        val entries = all()
        val kept = entries.filterNot { it.key == uri.toString() }
        if (kept.size == entries.size) return SaveResult.UNCHANGED
        return if (write(kept)) SaveResult.SAVED else SaveResult.FAILED
    }

    /**
     * Refreshes the cached path when a folder resolves differently than before
     * (e.g. a removable volume mounted under a new id). No-op if unchanged.
     */
    fun updatePath(uri: Uri, path: String?) {
        val entries = all()
        val updated = entries.map {
            if (it.key == uri.toString() && it.path != path) it.copy(path = path) else it
        }
        if (updated != entries) write(updated)
    }

    // -- serialization -------------------------------------------------------

    private fun parse(text: String): List<GameEntry> {
        val root = runCatching { JSONObject(text) }.getOrElse { e ->
            Log.w(TAG, "$file is not valid JSON, treating the library as empty: ${e.message}")
            return emptyList()
        }

        val version = root.optInt(FIELD_VERSION, 0)
        if (version > SCHEMA_VERSION) {
            // Written by a newer build. Read what we understand rather than
            // discarding the user's library; unknown fields are ignored.
            Log.w(TAG, "$file has version $version, newer than $SCHEMA_VERSION; reading what we can")
        }

        val games = root.optJSONArray(FIELD_GAMES) ?: return emptyList()
        val entries = ArrayList<GameEntry>(games.length())
        // The library is user-editable, so the same tree URI can appear twice.
        // Deduplicate while parsing: the list uses key = { it.key }, and a
        // duplicate key crashes the LazyColumn ("Key was already used") on
        // every start.
        val seen = HashSet<String>()
        for (i in 0 until games.length()) {
            val obj = games.optJSONObject(i) ?: continue
            val rawUri = obj.optString(FIELD_TREE_URI)
            if (rawUri.isEmpty()) {
                Log.w(TAG, "skipping library entry $i: no $FIELD_TREE_URI")
                continue
            }
            // A malformed URI must not take out the whole library.
            val uri = runCatching { Uri.parse(rawUri) }.getOrNull() ?: continue
            val path = obj.optString(FIELD_PATH).takeIf { it.isNotEmpty() }
            val name = obj.optString(FIELD_NAME).takeIf { it.isNotEmpty() }
                ?: uri.lastPathSegment
                ?: "Game"
            val entry = GameEntry(uri, name, path)
            if (!seen.add(entry.key)) {
                Log.w(TAG, "skipping duplicate library entry $i: ${entry.key}")
                continue
            }
            entries += entry
        }
        return entries
    }

    /**
     * Persists [entries]. Returns true only when the file was actually
     * replaced; a failure is logged and reported so callers cannot claim
     * success for a save that did not happen.
     */
    private fun write(entries: List<GameEntry>): Boolean {
        val games = JSONArray()
        for (entry in entries) {
            val obj = JSONObject()
                .put(FIELD_NAME, entry.name)
                .put(FIELD_TREE_URI, entry.key)
            // Omit a null path rather than writing "null": the absence is
            // meaningful ("not resolvable"), and it keeps the file clean.
            entry.path?.let { obj.put(FIELD_PATH, it) }
            games.put(obj)
        }
        val root = JSONObject()
            .put(FIELD_VERSION, SCHEMA_VERSION)
            .put(FIELD_GAMES, games)

        return runCatching {
            file.parentFile?.mkdirs()
            // Atomic replace: write a sibling temp file, then rename over the
            // target. A kill mid-write leaves the previous library intact.
            val tmp = File(file.parentFile, "$FILE_NAME.tmp")
            tmp.writeText(root.toString(2))
            if (!tmp.renameTo(file)) {
                tmp.copyTo(file, overwrite = true)
                tmp.delete()
            }
        }.onFailure { e ->
            Log.e(TAG, "cannot write $file: ${e.message}")
        }.isSuccess
    }

    private companion object {
        const val TAG = "krkr-library"

        /** Schema version written into the file; bumped on breaking changes. */
        const val SCHEMA_VERSION = 1

        const val FILE_NAME = "games.json"
        const val FIELD_VERSION = "version"
        const val FIELD_GAMES = "games"
        const val FIELD_NAME = "name"
        const val FIELD_TREE_URI = "treeUri"
        const val FIELD_PATH = "path"
    }
}
