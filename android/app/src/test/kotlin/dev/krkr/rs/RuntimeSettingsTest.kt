package dev.krkr.rs

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Unit tests for the settings JSON. They exercise the pure encode/decode pair
 * [settingsToJson]/[settingsFromJson] on the JVM, with the upstream `org.json`
 * on the test classpath in place of Android's stubbed one.
 */
class RuntimeSettingsTest {

    @Test
    fun settingsRoundTripThroughJson() {
        val settings = RuntimeSettings(
            frameRate = "120",
            preloadArchives = "true",
            debugOverlay = "true",
            logLevel = "debug",
        )
        val decoded = settingsFromJson(JSONObject(settingsToJson(settings).toString()))
        assertEquals(settings, decoded)
    }

    @Test
    fun debugOverlayDefaultsToOffWhenTheKeyIsAbsent() {
        // A settings.json written before the overlay existed has no
        // `debug_overlay` key; the shared built-in default is `false`.
        val root = JSONObject().put("version", 1).put("frame_rate", "60")
        assertEquals("false", settingsFromJson(root).debugOverlay)
    }

    @Test
    fun aPresentDebugOverlayValueIsPreservedForTheEngineToValidate() {
        // An invalid value is kept verbatim; the engine rejects it with the
        // option name rather than the launcher silently defaulting it.
        val root = settingsToJson(RuntimeOptions.DEFAULTS).put("debug_overlay", "yes")
        assertEquals("yes", settingsFromJson(root).debugOverlay)
    }

    @Test
    fun everyOptionRoundTripsIncludingAHandEditedDebugOverlay() {
        val root = settingsToJson(RuntimeOptions.DEFAULTS).put("debug_overlay", "true")
        val decoded = settingsFromJson(root)
        assertEquals(RuntimeOptions.DEFAULTS.frameRate, decoded.frameRate)
        assertEquals(RuntimeOptions.DEFAULTS.preloadArchives, decoded.preloadArchives)
        assertEquals("true", decoded.debugOverlay)
        assertEquals(RuntimeOptions.DEFAULTS.logLevel, decoded.logLevel)
        assertTrue(decoded.debugOverlay in RuntimeOptions.DEBUG_OVERLAY_VALUES)
    }
}
