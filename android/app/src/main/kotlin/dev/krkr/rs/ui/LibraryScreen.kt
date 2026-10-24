@file:OptIn(ExperimentalMaterial3Api::class)

package dev.krkr.rs.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import dev.krkr.rs.GameEntry

/**
 * The library screen: the games the user has added, and the actions on them.
 *
 * The games are cards in a grid that adapts to the width available, so a
 * phone in landscape shows two cards side by side and a wider screen shows
 * more. The app bar holds the selection mode; the floating action button adds
 * a game.
 */
@Composable
fun LibraryScreen(
    games: List<GameEntry>,
    manageMode: Boolean,
    selectedKeys: Set<String>,
    configPath: String,
    snackbarHostState: SnackbarHostState,
    onAddGame: () -> Unit,
    onPlay: (GameEntry) -> Unit,
    onRemove: (List<GameEntry>) -> Unit,
    onShowPath: (GameEntry) -> Unit,
    onToggleSelection: (GameEntry) -> Unit,
    onSelectAll: () -> Unit,
    onEnterManage: () -> Unit,
    onExitManage: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val selectedGames = games.filter { it.key in selectedKeys }
    val allSelected = games.isNotEmpty() && selectedGames.size == games.size

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        if (manageMode) {
                            "${selectedKeys.size} selected"
                        } else {
                            "Library"
                        },
                    )
                },
                navigationIcon = {
                    if (manageMode) {
                        IconButton(onClick = onExitManage) {
                            Icon(
                                imageVector = Icons.Default.Close,
                                contentDescription = "Leave the selection mode",
                            )
                        }
                    }
                },
                actions = {
                    if (manageMode) {
                        TextButton(onClick = onSelectAll) {
                            Text(if (allSelected) "Clear" else "Select all")
                        }
                    } else if (games.isNotEmpty()) {
                        TextButton(onClick = onEnterManage) { Text("Manage") }
                    }
                },
            )
        },
        snackbarHost = { SnackbarHost(snackbarHostState) },
        floatingActionButton = {
            if (!manageMode && games.isNotEmpty()) {
                ExtendedFloatingActionButton(
                    onClick = onAddGame,
                    icon = { Icon(Icons.Default.Add, contentDescription = null) },
                    text = { Text("Add game") },
                )
            }
        },
        bottomBar = {
            if (manageMode) {
                ManageBar(
                    selectedCount = selectedGames.size,
                    onRemoveSelected = { onRemove(selectedGames) },
                )
            }
        },
    ) { padding ->
        if (games.isEmpty()) {
            EmptyLibrary(
                configPath = configPath,
                onAddGame = onAddGame,
                modifier = Modifier
                    .fillMaxSize()
                    .padding(padding),
            )
        } else {
            LazyVerticalGrid(
                columns = GridCells.Adaptive(minSize = 280.dp),
                modifier = Modifier
                    .fillMaxSize()
                    .padding(padding),
                contentPadding = PaddingValues(
                    start = 16.dp,
                    top = 8.dp,
                    end = 16.dp,
                    bottom = 96.dp,
                ),
                horizontalArrangement = Arrangement.spacedBy(12.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                items(games, key = { it.key }) { entry ->
                    GameCard(
                        entry = entry,
                        manageMode = manageMode,
                        selected = entry.key in selectedKeys,
                        onPlay = { onPlay(entry) },
                        onRemove = { onRemove(listOf(entry)) },
                        onShowPath = { onShowPath(entry) },
                        onToggleSelection = { onToggleSelection(entry) },
                    )
                }
            }
        }
    }
}
