@file:OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)

package dev.krkr.rs.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Slider
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import dev.krkr.rs.RuntimeOptions
import dev.krkr.rs.RuntimeSettings
import dev.krkr.rs.RuntimeSettingsStore
import kotlin.math.roundToInt

/**
 * The settings screen: the launcher's persisted copy of the engine options.
 *
 * The screen shows the four options the engine honours — the frame-rate
 * limit, the archive preload, the debug overlay and the engine log level —
 * with the same accepted values and range as the desktop launcher's
 * `--fps`, `--preload-archives` and `--debug-overlay` options
 * (`api::config`). Nothing is offered for a feature the engine has no knob
 * for.
 *
 * Saving writes the settings file atomically; [KrkrGameActivity] reads it and
 * publishes it over JNI on the next launch, which is why the screen says the
 * change applies to the next run.
 */
@Composable
fun SettingsScreen(
    store: RuntimeSettingsStore,
    snackbarHostState: SnackbarHostState,
    onNotify: (String) -> Unit,
    onDirtyChange: (Boolean) -> Unit,
    modifier: Modifier = Modifier,
) {
    // The values as they are on disk: the baseline the edit is compared with,
    // and the values a successful save replaces.
    var baseline by remember { mutableStateOf(store.read()) }
    var frameRate by rememberSaveable { mutableStateOf(baseline.frameRate) }
    var preloadArchives by rememberSaveable { mutableStateOf(baseline.preloadArchives) }
    var debugOverlay by rememberSaveable { mutableStateOf(baseline.debugOverlay) }
    var logLevel by rememberSaveable { mutableStateOf(baseline.logLevel) }

    // Validate only the frame rate here: the other values can only be set from
    // their fixed controls, except when the file was hand-edited.
    val frameRateValue = frameRate.trim().toIntOrNull()
    val frameRateValid = frameRateValue != null &&
        frameRateValue in RuntimeOptions.MIN_FRAME_RATE..RuntimeOptions.MAX_FRAME_RATE
    val preloadKnown = preloadArchives in RuntimeOptions.PRELOAD_VALUES
    val preloadChecked = preloadArchives.toBooleanStrictOrNull() ?: false
    val debugKnown = debugOverlay in RuntimeOptions.DEBUG_OVERLAY_VALUES
    val debugChecked = debugOverlay.toBooleanStrictOrNull() ?: false
    val logLevelKnown = logLevel in RuntimeOptions.LOG_LEVELS

    // The slider shows a value inside its range, whatever the file holds, so
    // an out-of-range value is reported instead of being silently replaced.
    val sliderValue = (frameRateValue ?: RuntimeOptions.MIN_FRAME_RATE)
        .coerceIn(RuntimeOptions.MIN_FRAME_RATE, RuntimeOptions.MAX_FRAME_RATE)
        .toFloat()

    val dirty = frameRate != baseline.frameRate ||
        preloadArchives != baseline.preloadArchives ||
        debugOverlay != baseline.debugOverlay ||
        logLevel != baseline.logLevel
    LaunchedEffect(dirty) { onDirtyChange(dirty) }

    /** Fills the form with the built-in defaults, without writing them. */
    fun restoreDefaults() {
        frameRate = RuntimeOptions.DEFAULT_FRAME_RATE.toString()
        preloadArchives = RuntimeOptions.DEFAULT_PRELOAD_ARCHIVES
        debugOverlay = RuntimeOptions.DEFAULT_DEBUG_OVERLAY
        logLevel = RuntimeOptions.DEFAULT_LOG_LEVEL
        onNotify("Defaults restored; save to apply them.")
    }

    /** Writes the form to the settings file, if every value is accepted. */
    fun save() {
        val fps = frameRate.trim().toIntOrNull()
        if (fps == null || fps !in RuntimeOptions.MIN_FRAME_RATE..RuntimeOptions.MAX_FRAME_RATE) {
            onNotify(
                "Frame rate must be an integer in " +
                    "${RuntimeOptions.FRAME_RATE_RANGE} frames per second.",
            )
            return
        }
        if (preloadArchives !in RuntimeOptions.PRELOAD_VALUES) {
            onNotify("Choose one of: ${RuntimeOptions.PRELOAD_VALUES.joinToString(", ")}.")
            return
        }
        if (debugOverlay !in RuntimeOptions.DEBUG_OVERLAY_VALUES) {
            onNotify("Choose one of: ${RuntimeOptions.DEBUG_OVERLAY_VALUES.joinToString(", ")}.")
            return
        }
        if (logLevel !in RuntimeOptions.LOG_LEVELS) {
            onNotify("Choose one of: ${RuntimeOptions.LOG_LEVELS.joinToString(", ")}.")
            return
        }
        val written = RuntimeSettings(
            frameRate = fps.toString(),
            preloadArchives = preloadArchives,
            debugOverlay = debugOverlay,
            logLevel = logLevel,
        )
        if (store.write(written)) {
            baseline = written
            // Put the written values back into the form. A value that is not
            // canonical ("060", "+60", " 60") would otherwise leave the form
            // differing from the file, and the screen would report unsaved
            // changes that were in fact saved.
            frameRate = written.frameRate
            preloadArchives = written.preloadArchives
            debugOverlay = written.debugOverlay
            logLevel = written.logLevel
            onNotify("Settings saved. They apply to the next game start.")
        } else {
            onNotify("Could not save the settings; they were not changed.")
        }
    }

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text("Settings") },
                actions = {
                    TextButton(onClick = { restoreDefaults() }) { Text("Restore defaults") }
                    TextButton(onClick = { save() }) { Text("Save") }
                },
            )
        },
        snackbarHost = { SnackbarHost(snackbarHostState) },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .verticalScroll(rememberScrollState()),
        ) {
            SectionHeader("Playback")

            ListItem(
                headlineContent = { Text("Frame-rate limit") },
                supportingContent = {
                    Text(
                        "An integer in ${RuntimeOptions.FRAME_RATE_RANGE} frames per second; " +
                            "default ${RuntimeOptions.DEFAULT_FRAME_RATE}. " +
                            "Applies to the next game start.",
                    )
                },
                trailingContent = {
                    Text(
                        text = "${frameRate.trim()} fps",
                        style = MaterialTheme.typography.titleMedium,
                    )
                },
            )
            Slider(
                value = sliderValue,
                onValueChange = { frameRate = it.roundToInt().toString() },
                valueRange = RuntimeOptions.MIN_FRAME_RATE.toFloat()..
                    RuntimeOptions.MAX_FRAME_RATE.toFloat(),
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp),
            )
            if (!frameRateValid) {
                Text(
                    text = "Must be an integer in ${RuntimeOptions.FRAME_RATE_RANGE} " +
                        "frames per second.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                )
            }

            SectionHeader("Engine")

            ListItem(
                headlineContent = { Text("Archive preload") },
                supportingContent = { Text(RuntimeOptions.preloadDescription()) },
                trailingContent = {
                    Switch(
                        checked = preloadChecked,
                        onCheckedChange = { preloadArchives = it.toString() },
                        // The visible "Archive preload" label is a separate
                        // node, so name the switch itself for TalkBack.
                        modifier = Modifier.semantics {
                            contentDescription = "Archive preload"
                        },
                    )
                },
            )
            if (!preloadKnown) {
                Text(
                    text = "This saved value (\"$preloadArchives\") is not one of " +
                        "${RuntimeOptions.PRELOAD_VALUES.joinToString(", ")}; " +
                        "toggle the switch before saving.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                )
            }

            ListItem(
                headlineContent = { Text("Debug overlay") },
                supportingContent = { Text(RuntimeOptions.debugOverlayDescription()) },
                trailingContent = {
                    Switch(
                        checked = debugChecked,
                        onCheckedChange = { debugOverlay = it.toString() },
                        // The visible "Debug overlay" label is a separate node,
                        // so name the switch itself for TalkBack.
                        modifier = Modifier.semantics {
                            contentDescription = "Debug overlay"
                        },
                    )
                },
            )
            if (!debugKnown) {
                Text(
                    text = "This saved value (\"$debugOverlay\") is not one of " +
                        "${RuntimeOptions.DEBUG_OVERLAY_VALUES.joinToString(", ")}; " +
                        "toggle the switch before saving.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                )
            }

            SectionHeader("Diagnostics")

            ListItem(
                headlineContent = { Text("Log level") },
                supportingContent = {
                    Text(
                        "How much the engine writes to its log file (krkr.log); " +
                            "default: ${RuntimeOptions.DEFAULT_LOG_LEVEL}. " +
                            RuntimeOptions.logLevelDescription(logLevel),
                    )
                },
            )
            FlowRow(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp),
            ) {
                for (level in RuntimeOptions.LOG_LEVELS) {
                    FilterChip(
                        selected = logLevel == level,
                        onClick = { logLevel = level },
                        label = { Text(level.replaceFirstChar { it.uppercase() }) },
                        modifier = Modifier.padding(end = 8.dp, bottom = 8.dp),
                    )
                }
            }
            if (!logLevelKnown) {
                Text(
                    text = "This saved value (\"$logLevel\") is not one of " +
                        "${RuntimeOptions.LOG_LEVELS.joinToString(", ")}; " +
                        "choose one before saving.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                )
            }

            Text(
                text = "Settings file: ${store.settingsFile().absolutePath}",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(16.dp),
            )
        }
    }
}
