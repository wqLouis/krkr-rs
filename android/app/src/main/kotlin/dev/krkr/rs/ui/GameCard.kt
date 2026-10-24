package dev.krkr.rs.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Info
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.Checkbox
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.krkr.rs.GameEntry

/**
 * One game in the library, as a card.
 *
 * The card carries the game's name, where its folder lives and one primary
 * action — playing it. The overflow menu beside the name holds the actions
 * that are not the primary one: the same play action, the folder's path, and
 * removing the game from the library.
 *
 * In manage mode the card becomes a selection item: the play action and the
 * overflow menu are replaced by a checkbox, and tapping the card toggles the
 * selection instead of starting the game.
 */
@Composable
fun GameCard(
    entry: GameEntry,
    manageMode: Boolean,
    selected: Boolean,
    onPlay: () -> Unit,
    onRemove: () -> Unit,
    onShowPath: () -> Unit,
    onToggleSelection: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var menuOpen by remember { mutableStateOf(false) }

    Card(
        modifier = modifier
            .fillMaxWidth()
            .clickable(
                onClickLabel = if (manageMode) "Select" else "Play",
                role = Role.Button,
                onClick = { if (manageMode) onToggleSelection() else onPlay() },
            ),
        colors = CardDefaults.cardColors(
            containerColor = if (selected) {
                MaterialTheme.colorScheme.secondaryContainer
            } else {
                MaterialTheme.colorScheme.surfaceContainerLow
            },
        ),
        elevation = CardDefaults.cardElevation(defaultElevation = 1.dp),
        border = if (selected) {
            BorderStroke(2.dp, MaterialTheme.colorScheme.primary)
        } else {
            null
        },
    ) {
        Column(Modifier.padding(16.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                if (manageMode) {
                    Checkbox(
                        checked = selected,
                        onCheckedChange = { onToggleSelection() },
                    )
                    Spacer(Modifier.width(12.dp))
                }

                MonogramTile(name = entry.name)
                Spacer(Modifier.width(12.dp))

                Column(Modifier.weight(1f)) {
                    Text(
                        text = entry.name,
                        style = MaterialTheme.typography.titleMedium,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    PathLabel(path = folderLabel(entry))
                }

                if (!manageMode) {
                    Box {
                        IconButton(onClick = { menuOpen = true }) {
                            Icon(
                                imageVector = Icons.Default.MoreVert,
                                contentDescription = "More actions for ${entry.name}",
                            )
                        }
                        DropdownMenu(
                            expanded = menuOpen,
                            onDismissRequest = { menuOpen = false },
                        ) {
                            DropdownMenuItem(
                                text = { Text("Play") },
                                onClick = {
                                    menuOpen = false
                                    onPlay()
                                },
                                leadingIcon = {
                                    Icon(Icons.Default.PlayArrow, contentDescription = null)
                                },
                            )
                            DropdownMenuItem(
                                text = { Text("Show folder path") },
                                onClick = {
                                    menuOpen = false
                                    onShowPath()
                                },
                                leadingIcon = {
                                    Icon(Icons.Default.Info, contentDescription = null)
                                },
                            )
                            DropdownMenuItem(
                                text = { Text("Remove from library") },
                                onClick = {
                                    menuOpen = false
                                    onRemove()
                                },
                                leadingIcon = {
                                    Icon(Icons.Default.Delete, contentDescription = null)
                                },
                            )
                        }
                    }
                }
            }

            if (!manageMode) {
                Spacer(Modifier.height(12.dp))
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.End,
                ) {
                    FilledTonalButton(onClick = onPlay) {
                        Icon(Icons.Default.PlayArrow, contentDescription = null)
                        Spacer(Modifier.width(8.dp))
                        Text("Play")
                    }
                }
            }
        }
    }
}

/**
 * Where the game folder lives: the path the engine is handed when it is
 * known, and the folder's own name in the picker otherwise.
 */
private fun folderLabel(entry: GameEntry): String =
    entry.path ?: entry.uri.lastPathSegment ?: entry.uri.toString()
