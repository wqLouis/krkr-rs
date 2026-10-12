package dev.krkr.rs

import java.io.File
import java.io.FileOutputStream

/**
 * Replaces a small JSON store durably, without ever truncating the old file.
 *
 * The content is written to a unique sibling temp file, flushed to the backing
 * store with `FileDescriptor.sync()`, and then renamed over the target. On
 * Android/Linux `rename(2)` atomically replaces an existing same-directory
 * target, so a reader sees either the old file or the complete new one; the
 * `File.renameTo` "may fail if the target exists" caveat applies to other
 * platforms, not here. Because the target is never opened for writing, a failed
 * replace leaves the previous file exactly as it was.
 *
 * A `renameTo` that returns `false` is therefore a genuine error (the target
 * became a directory, the directory is not writable) and this throws. There is
 * deliberately no `copyTo(overwrite = true)` fallback: that truncates the
 * target before copying, which would turn a failed replace into data loss.
 *
 * The synced temp plus rename matches the engine's own state writer
 * (`crates/android/src/state.rs`): the data survives a process kill (it is in
 * the page cache) and a power loss cannot expose a truncated file. The rename
 * itself is not directory-synced, so the worst a power loss can do is leave the
 * previous version in place.
 */
internal object DurableWrite {
    /** Writes [content] to [file], replacing it atomically. Throws on failure. */
    fun text(file: File, content: String) {
        val dir = file.parentFile ?: error("no parent directory for $file")
        dir.mkdirs()
        val tmp = File.createTempFile("${file.name}.", ".tmp", dir)
        try {
            FileOutputStream(tmp).use { out ->
                out.write(content.toByteArray(Charsets.UTF_8))
                out.flush()
                out.fd.sync()
            }
            if (!tmp.renameTo(file)) {
                error("rename ${tmp.name} -> ${file.name} failed")
            }
        } finally {
            // No-op after a successful rename; on any failure it removes the
            // orphaned temp so no stale store copy is left behind.
            tmp.delete()
        }
    }
}
