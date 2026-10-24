package dev.krkr.rs.ui

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/**
 * The tile that stands in for a game's cover.
 *
 * A game folder has no artwork the launcher can rely on, so the tile shows the
 * first character of the game's name on a tonal surface instead. The character
 * is taken from the name's first code point, so a name that starts with a
 * character outside the basic multilingual plane is not cut in half.
 */
@Composable
fun MonogramTile(
    name: String,
    modifier: Modifier = Modifier,
    size: Dp = 48.dp,
) {
    Surface(
        modifier = modifier.size(size),
        shape = KrkrMediumShape,
        color = MaterialTheme.colorScheme.primaryContainer,
        contentColor = MaterialTheme.colorScheme.onPrimaryContainer,
    ) {
        Box(contentAlignment = Alignment.Center) {
            Text(
                text = monogramOf(name),
                style = MaterialTheme.typography.titleLarge,
                maxLines = 1,
                overflow = TextOverflow.Clip,
            )
        }
    }
}

/**
 * The first character of [name], as a standalone string.
 *
 * Returns "?" for a name that is empty or only whitespace, so the tile always
 * has something to show.
 */
private fun monogramOf(name: String): String {
    val trimmed = name.trim()
    if (trimmed.isEmpty()) return "?"
    val codePoint = trimmed.codePointAt(0)
    return String(Character.toChars(codePoint))
}

/**
 * The heading of a section of a list.
 *
 * The heading uses the label colour and the small title role, which is how
 * Material 3 separates a group of list items from the items themselves.
 */
@Composable
fun SectionHeader(
    title: String,
    modifier: Modifier = Modifier,
) {
    Text(
        text = title,
        style = MaterialTheme.typography.titleSmall,
        color = MaterialTheme.colorScheme.primary,
        modifier = modifier.padding(start = 16.dp, end = 16.dp, top = 16.dp, bottom = 4.dp),
    )
}

/**
 * A file path, in the monospace face so its shape is readable.
 *
 * The path is a location, not prose: the monospace face keeps the separators
 * and the directory levels distinguishable, and the text is trimmed to a
 * single line because a path that wraps is hard to read on a phone.
 */
@Composable
fun PathLabel(
    path: String,
    modifier: Modifier = Modifier,
) {
    Text(
        text = path,
        style = MaterialTheme.typography.bodySmall,
        fontFamily = FontFamily.Monospace,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
        modifier = modifier,
    )
}
