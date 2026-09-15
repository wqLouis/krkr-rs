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
 * Only ids from known filesystem providers are trusted: the external-storage
 * provider's `primary:...` / `XXXX-XXXX:...` ids, and the Downloads provider's
 * `raw:/...` ids. Returns `null` for everything else (cloud storage, arbitrary
 * `DocumentsProvider`s, or a third-party id that merely has the right shape).
 * Callers must treat `null` as "this folder cannot be used" and say so, rather
 * than silently launching the engine with a path that will not resolve.
 */
object SafPaths {

    /** Secondary volumes are identified by a `XXXX-XXXX` UUID under `/storage`. */
    private val VOLUME_UUID = Regex("[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}")

    /**
     * The only provider whose `documentId` may be interpreted as a volume
     * filesystem path. Trusting the id shape alone would let a third-party or
     * cloud provider whose id happens to look like `primary:...` be mapped to an
     * unrelated `/storage/...` path (which, under MANAGE_EXTERNAL_STORAGE, may
     * even exist), silently opening the wrong folder.
     */
    private const val EXTERNAL_STORAGE_AUTHORITY = "com.android.externalstorage.documents"

    /** The Downloads provider, whose `raw:` document ids are real paths. */
    private const val DOWNLOADS_AUTHORITY = "com.android.providers.downloads.documents"

    /** Prefix of a Downloads document id that already is an absolute path. */
    private const val RAW_PREFIX = "raw:"

    /**
     * Whether the app currently holds "All files access"
     * (`MANAGE_EXTERNAL_STORAGE`).
     *
     * Android never grants this automatically: the user has to enable it in
     * system settings. Because the engine reads the game by filesystem path,
     * nothing may be launched until this returns true, or the engine fails with
     * `EACCES` and shows a black screen. `isExternalStorageManager` is the only
     * way to observe the grant (the permission has no runtime prompt).
     *
     * Never throws: a failure to query is treated as "not granted".
     */
    fun hasAllFilesAccess(): Boolean =
        runCatching { Environment.isExternalStorageManager() }.getOrDefault(false)

    /**
     * The absolute filesystem path for a SAF tree URI, or null if it cannot be
     * addressed as a path.
     *
     * The provider authority is checked *before* a document id is interpreted:
     * only the external-storage provider's `primary:...` / `XXXX-XXXX:...` ids
     * are turned into volume paths, and only the Downloads provider's
     * `raw:/...` ids are turned into absolute paths. An id that merely has the
     * right shape is not trusted.
     */
    fun resolve(context: Context, treeUri: Uri): String? {
        val authority = treeUri.authority ?: return null
        val documentId = runCatching {
            DocumentsContract.getTreeDocumentId(treeUri)
        }.getOrNull() ?: return null

        // Downloads emits `raw:/absolute/path` ids for files that really live on
        // disk, so returning the path is correct rather than a false negative.
        if (documentId.startsWith(RAW_PREFIX)) {
            if (authority != DOWNLOADS_AUTHORITY) return null
            val rawPath = documentId.removePrefix(RAW_PREFIX)
            return rawPath.takeIf { it.startsWith("/") }
        }

        // "<volumeId>:<relative/path>"; the relative part may be empty when the
        // user picked a volume root. Only the external-storage provider may
        // produce this shape.
        if (authority != EXTERNAL_STORAGE_AUTHORITY) return null
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
