package dev.krkr.rs.ui

import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.List
import androidx.compose.material.icons.filled.Info
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationRail
import androidx.compose.material3.NavigationRailItem
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.unit.dp

/**
 * The launcher's top-level destinations, in the order the rail shows them.
 *
 * Each destination is a screen of its own with its own app bar; the rail is
 * the only way between them, so the current one is always visible.
 *
 * The icons come from the core icon set, which ships a small, fixed
 * selection: a list for the library, a gear for the settings and an
 * information mark for the diagnostics log.
 */
enum class Destination(val label: String, val icon: ImageVector) {
    LIBRARY("Library", Icons.AutoMirrored.Filled.List),
    SETTINGS("Settings", Icons.Default.Settings),
    LOGS("Logs", Icons.Default.Info),
}

/**
 * The navigation rail: the launcher's top-level destinations.
 *
 * The rail is always visible, because the launcher is a landscape app and the
 * destinations are peers rather than steps of one task. The selected item is
 * marked by the rail's own selection colour and always carries its label, so
 * the current destination is readable without relying on the icon alone.
 */
@Composable
fun KrkrNavigationRail(
    current: Destination,
    onSelect: (Destination) -> Unit,
    modifier: Modifier = Modifier,
) {
    NavigationRail(
        modifier = modifier,
        header = {
            Surface(
                shape = KrkrLargeShape,
                color = MaterialTheme.colorScheme.primaryContainer,
                contentColor = MaterialTheme.colorScheme.onPrimaryContainer,
                modifier = Modifier.padding(vertical = 8.dp),
            ) {
                Text(
                    text = "krkr",
                    style = MaterialTheme.typography.labelMedium,
                    modifier = Modifier.padding(horizontal = 10.dp, vertical = 6.dp),
                )
            }
        },
    ) {
        for (destination in Destination.entries) {
            NavigationRailItem(
                selected = destination == current,
                onClick = { onSelect(destination) },
                icon = { Icon(destination.icon, contentDescription = null) },
                label = { Text(destination.label) },
                alwaysShowLabel = true,
            )
        }
    }
}
