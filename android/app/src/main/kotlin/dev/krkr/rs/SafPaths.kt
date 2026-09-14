package dev.krkr.rs

import android.content.Context
import android.net.Uri
import android.os.Environment
import android.provider.DocumentsContract

/**
 * Maps a SAF tree URI to the filesystem path the engine is given.
 *
 * The engine's storage layer is path-based (`Storage::mount` walks a directory
 * and `Xp3Archive::open` opens files), while SAF hands out `content://` URIs.
 * For the primary shared volume and for removable volumes the tree document id
 * encodes exactly the information needed to reconstruct the path
 * (`primary:Games/MyGame` → `/storage/emulated/0/Games/MyGame`,
 * `1A2B-3C4D:Games` → `/storage/1A2B-3C4D/Games`), so we resolve rather than
 * stream.
 *
 * This is option A in `docs/android.md` §4: `MANAGE_EXTERNAL_STORAGE` is
 * required for the resolved path to be readable on Android 11+. The
 * policy-clean alternative (a SAF-backed storage backend that streams through
 * `ContentResolver`) is the documented follow-up.
 *
 * Returns `null` when the volume is not one we can address by path (e.g. a
 * cloud provider, or a `DocumentsProvider` that is not a real filesystem).
 * Callers must treat `null` as "this folder cannot be used" and say so, rather
 * than silently launching the engine with a path that will not resolve.
 */
object SafPaths {

    /** Secondary volumes are identified by a `XXXX-XXXX` UUID under `/storage`. */
    private val VOLUME_UUID = Regex("[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}")

    /**
     * The absolute filesystem path for a SAF tree URI, or null if it cannot be
     * addressed as a path.
     */
    fun resolve(context: Context, treeUri: Uri): String? {
        val documentId = runCatching {
            DocumentsContract.getTreeDocumentId(treeUri)
        }.getOrNull() ?: return null

        // "<volumeId>:<relative/path>"; the relative part may be empty when the
        // user picked a volume root.
        val separator = documentId.indexOf(':')
        if (separator < 0) return null
        val volumeId = documentId.substring(0, separator)
        val relative = documentId.substring(separator + 1)

        val root = volumeRoot(context, volumeId) ?: return null
        return if (relative.isEmpty()) root else "$root/$relative"
    }

    private fun volumeRoot(context: Context, volumeId: String): String? = when {
        volumeId.equals("primary", ignoreCase = true) -> primaryRoot(context)
        VOLUME_UUID.matches(volumeId) -> "/storage/$volumeId"
        else -> null
    }

    /**
     * A human-readable name for a picked folder: the folder's own name.
     *
     * Prefers the provider's display name (which is localized and correct for
     * non-filesystem providers) and falls back to the URI's last segment.
     */
    fun displayName(context: Context, treeUri: Uri): String {
        runCatching {
            val documentUri = DocumentsContract.buildDocumentUriUsingTree(
                treeUri,
                DocumentsContract.getTreeDocumentId(treeUri),
            )
            context.contentResolver.query(
                documentUri,
                arrayOf(DocumentsContract.Document.COLUMN_DISPLAY_NAME),
                null,
                null,
                null,
            )?.use { cursor ->
                if (cursor.moveToFirst()) {
                    val name = cursor.getString(0)
                    if (!name.isNullOrBlank()) return name
                }
            }
        }
        return treeUri.lastPathSegment?.substringAfterLast('/')?.substringAfterLast(':')
            ?.takeIf { it.isNotBlank() }
            ?: "Game"
    }

    /**
     * The primary shared volume. `Environment.getExternalStorageDirectory()`
     * is deprecated but remains the only way to obtain the path that
     * `DocumentsContract`'s `primary` volume id refers to; it is correct on
     * every API level we target.
     */
    @Suppress("DEPRECATION")
    private fun primaryRoot(context: Context): String? =
        runCatching { Environment.getExternalStorageDirectory().absolutePath }
            .getOrNull()
            ?: context.filesDir.parentFile?.absolutePath
}
