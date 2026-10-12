package dev.krkr.rs

import android.content.Context
import android.util.Log
import org.json.JSONObject
import java.io.File

/**
 * The runtime options the launcher persists and publishes to the engine.
 *
 * The values are kept as the exact strings the user's choice is stored as, not
 * as parsed numbers or enums. Validation belongs to the read side
 * (`crates/android`), exactly like the desktop launcher's `key=value` file:
 * keeping the raw text means a hand-edited or corrupted value is reported by
 * the engine with the option name and its accepted values instead of being
 * silently replaced by a default (`AGENTS.md` D11).
 */
data class RuntimeSettings(
    val frameRate: String,
    val scaling: String,
    val preloadArchives: String,
)

/**
 * The accepted runtime options, their ranges and their defaults.
 *
 * These mirror `api::config::{FrameRateLimit, PresentationScaling}` and the
 * boolean `preload_archives` option: every default and accepted value comes
 * from there, while the Android default presentation is
 * [DEFAULT_SCALING] (`fit`) rather than the desktop `game`
 * (`docs/android.md` §2.2). Only options the engine can actually honour are
 * listed — there is no control for texture resolution or audio buffering,
 * because the engine exposes no knob for either.
 */
object RuntimeOptions {
    /** Lowest accepted frame-rate limit; mirrors `MIN_FRAME_RATE`. */
    const val MIN_FRAME_RATE = 1

    /** Highest accepted frame-rate limit; mirrors `MAX_FRAME_RATE`. */
    const val MAX_FRAME_RATE = 1000

    /** Built-in default frame-rate limit; mirrors `DEFAULT_FRAME_RATE`. */
    const val DEFAULT_FRAME_RATE = 60

    /** Accepted frame-rate range as written in the UI and in errors. */
    const val FRAME_RATE_RANGE = "1..=1000"

    /** The four presentation modes; mirrors `PresentationScaling`. */
    val SCALING = listOf("game", "fit", "fill", "expand")

    /**
     * Android's presentation default.
     *
     * A phone must never present the game at its raw logical size, so the
     * picture is scaled uniformly to fit the screen and the remainder is
     * letterboxed. The user's saved choice overrides it.
     */
    const val DEFAULT_SCALING = "fit"

    /** The two accepted archive-preload values, as stored in the file. */
    val PRELOAD_VALUES = listOf("true", "false")

    /** The built-in archive-preload default; mirrors `RuntimeConfig`. */
    const val DEFAULT_PRELOAD_ARCHIVES = "false"

    /** The stored defaults, used when the settings file is missing or empty. */
    val DEFAULTS = RuntimeSettings(
        frameRate = DEFAULT_FRAME_RATE.toString(),
        scaling = DEFAULT_SCALING,
        preloadArchives = DEFAULT_PRELOAD_ARCHIVES,
    )

    /** A one-line description of [mode], shown under its radio button. */
    fun scalingDescription(mode: String): String = when (mode) {
        "game" -> "One logical pixel per screen pixel (the reference Win32 presentation)."
        "fit" -> "Scale uniformly to fit the screen; letterbox the remainder. Recommended."
        "fill" -> "Scale to fill the screen, which may distort the aspect."
        "expand" -> "Grow the logical area to the screen aspect, then scale uniformly."
        else -> "Unknown mode."
    }

    /**
     * The archive-preload trade, shown under its switch.
     *
     * Preloading is not a debug mode: it reads the archive into the same
     * storage as before, so every entry returns the same bytes. The cost is
     * the archive's size in resident memory, which on a phone is worth weighing
     * before turning it on.
     */
    const val PRELOAD_DESCRIPTION =
        "Read every mounted archive into memory when it mounts. On, the game uses " +
            "memory equal to each archive's size but never reads it back from storage; " +
            "off, it reads entries from storage as they are needed."

    /** A sentence naming the default and the trade, shown above the switch. */
    fun preloadDescription(): String =
        "Default: ${if (DEFAULT_PRELOAD_ARCHIVES == "true") "on" else "off"}. " +
            PRELOAD_DESCRIPTION
}

/**
 * Persists the runtime settings, as **JSON** in the app's private files
 * directory.
 *
 * This follows [GameLibrary]: the app keeps its launcher state in inspectable
 * JSON files rather than `SharedPreferences`, so it stays portable and
 * user-editable. Writes are atomic and flushed to disk before the replace, so a
 * failed write leaves the previous file intact, and reads are tolerant — a
 * missing or corrupt file falls back to [RuntimeOptions.DEFAULTS] rather than
 * crashing the launcher. An individual value that is present but invalid is
 * preserved and handed to the engine, which rejects it with the option name and
 * its accepted values.
 *
 * File: `<app files dir>/settings.json`
 *
 * ```json
 * { "version": 1, "frame_rate": "60", "scaling": "fit",
 *   "preload_archives": "false" }
 * ```
 */
class RuntimeSettingsStore(context: Context) {

    private val file = File(context.filesDir, FILE_NAME)

    /** The backing settings file, exposed so the UI can show where it lives. */
    fun settingsFile(): File = file

    /** The stored settings; a missing or unreadable file yields the defaults. */
    fun read(): RuntimeSettings {
        if (!file.isFile) return RuntimeOptions.DEFAULTS
        val text = runCatching { file.readText() }.getOrElse { error ->
            Log.w(TAG, "cannot read $file: ${error.message}")
            return RuntimeOptions.DEFAULTS
        }
        val root = runCatching { JSONObject(text) }.getOrElse { error ->
            Log.w(TAG, "$file is not valid JSON, using the defaults: ${error.message}")
            return RuntimeOptions.DEFAULTS
        }
        return RuntimeSettings(
            frameRate = stringField(root, FIELD_FRAME_RATE, RuntimeOptions.DEFAULTS.frameRate),
            scaling = stringField(root, FIELD_SCALING, RuntimeOptions.DEFAULTS.scaling),
            preloadArchives = stringField(
                root,
                FIELD_PRELOAD_ARCHIVES,
                RuntimeOptions.DEFAULTS.preloadArchives,
            ),
        )
    }

    /**
     * Persists [settings]. Returns false only when the file could not actually
     * be replaced, so the caller can tell the user instead of claiming success.
     */
    fun write(settings: RuntimeSettings): Boolean {
        val root = JSONObject()
            .put(FIELD_VERSION, SCHEMA_VERSION)
            .put(FIELD_FRAME_RATE, settings.frameRate)
            .put(FIELD_SCALING, settings.scaling)
            .put(FIELD_PRELOAD_ARCHIVES, settings.preloadArchives)
        return runCatching {
            DurableWrite.text(file, root.toString(2))
        }.onFailure { error ->
            Log.e(TAG, "cannot write $file: ${error.message}")
        }.isSuccess
    }

    /**
     * A raw string field, preserving a present value verbatim (including one
     * that is not a valid option) and falling back only when the key is absent,
     * null or blank.
     */
    private fun stringField(root: JSONObject, key: String, fallback: String): String {
        if (!root.has(key) || root.isNull(key)) return fallback
        val text = root.get(key).toString().trim()
        return text.ifEmpty { fallback }
    }

    private companion object {
        const val TAG = "krkr-settings"

        /** Schema version written into the file; bumped on breaking changes. */
        const val SCHEMA_VERSION = 1

        const val FILE_NAME = "settings.json"
        const val FIELD_VERSION = "version"
        const val FIELD_FRAME_RATE = "frame_rate"
        const val FIELD_SCALING = "scaling"
        const val FIELD_PRELOAD_ARCHIVES = "preload_archives"
    }
}
