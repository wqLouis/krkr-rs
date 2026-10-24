package dev.krkr.rs

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The wording the user reads when the app's process ended without a clean
 * stop.
 *
 * These sentences are what a bug report quotes, so they are pinned here
 * instead of only on a device. They are pure text: the Android exit records
 * they are built from are read on the device, the wording is not.
 */
class ExitReasonTest {

    @Test
    fun aNativeCrashNamesTheSignal() {
        assertEquals(
            "The game crashed with a native error (signal 6).",
            ExitReason.NATIVE_CRASH.describe(6),
        )
    }

    @Test
    fun aNativeCrashWithoutASignalStillReads() {
        assertEquals(
            "The game crashed with a native error.",
            ExitReason.NATIVE_CRASH.describe(null),
        )
    }

    @Test
    fun aLowMemoryKillIsNamedAsSuch() {
        assertEquals(
            "Android killed the app because the device was running out of memory.",
            ExitReason.LOW_MEMORY.describe(null),
        )
    }

    @Test
    fun everyReasonHasASentence() {
        for (reason in ExitReason.entries) {
            assertTrue(reason.describe(null).isNotBlank())
        }
    }
}
