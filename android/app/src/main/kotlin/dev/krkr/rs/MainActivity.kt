package dev.krkr.rs

import android.content.Intent
import android.net.Uri
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.krkr.rs.ui.KrkrTheme
import kotlinx.coroutines.launch

/**
 * The launcher: it owns the game library and nothing else.
 *
 * The engine is deliberately not linked into this Activity — launching a game
 * starts [KrkrGameActivity], which runs the Rust `android_main` on its own
 * surface. Keeping the two apart means a crash inside a game's script cannot
 * take the library with it, and the launcher stays a normal Compose app.
 */
class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val library = GameLibrary(this)
        setContent {
            KrkrTheme {
                LauncherScreen(library)
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun LauncherScreen(library: GameLibrary) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val snackbar = remember { SnackbarHostState() }

    var games by remember { mutableStateOf(library.all()) }
    var pendingRemoval by remember { mutableStateOf<GameEntry?>(null) }

    /** Reports a problem to the user; never fails silently. */
    fun notify(text: String) {
        scope.launch { snackbar.showSnackbar(text) }
    }

    // The system file picker. A tree (not a single file) because a game is a
    // folder containing its .xp3 archives.
    val pickFolder = rememberLauncherForActivityResult(
        ActivityResultContracts.OpenDocumentTree(),
    ) { uri: Uri? ->
        if (uri == null) return@rememberLauncherForActivityResult

        // Persist read+write: the engine reads the archives and writes saves
        // into <game>/savedata/. Without this the grant dies with the process.
        val persistable = runCatching {
            context.contentResolver.takePersistableUriPermission(
                uri,
                Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION,
            )
        }
        if (persistable.isFailure) {
            // Some providers do not offer persistable grants. The folder can
            // still be used this session, but it will not survive a restart —
            // say so rather than letting it silently disappear later.
            notify("This folder's access cannot be remembered; it may be lost on restart.")
        }

        val name = SafPaths.displayName(context, uri)
        // Resolve now so the config file records where the game actually is;
        // a folder that cannot be addressed as a path is still added (the user
        // may mount that volume later), but we say so immediately.
        val path = SafPaths.resolve(context, uri)
        if (library.add(GameEntry(uri, name, path))) {
            games = library.all()
            if (path != null) {
                notify("Added \"$name\".")
            } else {
                notify("Added \"$name\", but its folder is not on a filesystem the engine can read.")
            }
        } else {
            notify("\"$name\" is already in the library.")
        }
    }

    Scaffold(
        topBar = { TopAppBar(title = { Text("krkr-rs") }) },
        snackbarHost = { SnackbarHost(snackbar) },
        floatingActionButton = {
            ExtendedFloatingActionButton(
                onClick = { pickFolder.launch(null) },
                icon = { Icon(Icons.Default.Add, contentDescription = null) },
                text = { Text("Add game") },
            )
        },
    ) { padding ->
        if (games.isEmpty()) {
            EmptyLibrary(
                modifier = Modifier.fillMaxSize().padding(padding),
                configPath = library.configFile().absolutePath,
            )
        } else {
            LazyColumn(
                modifier = Modifier.fillMaxSize().padding(padding),
                contentPadding = PaddingValues(16.dp, 8.dp, 16.dp, 96.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                items(games, key = { it.key }) { entry ->
                    GameCard(
                        entry = entry,
                        onLaunch = { launchGame(context, library, entry, ::notify) },
                        onRemove = { pendingRemoval = entry },
                    )
                }
            }
        }
    }

    pendingRemoval?.let { entry ->
        AlertDialog(
            onDismissRequest = { pendingRemoval = null },
            title = { Text("Remove from library?") },
            text = {
                Text("\"${entry.name}\" will be removed from this list. The game's files on disk are not touched.")
            },
            confirmButton = {
                TextButton(onClick = {
                    library.remove(entry.uri)
                    games = library.all()
                    pendingRemoval = null
                }) { Text("Remove") }
            },
            dismissButton = {
                TextButton(onClick = { pendingRemoval = null }) { Text("Cancel") }
            },
        )
    }
}

/**
 * Starts the engine for [entry], or explains why it cannot.
 *
 * The engine needs a real path (docs/android.md §4), so a folder that cannot be
 * resolved — a cloud provider, or a documents provider that is not a
 * filesystem — is reported instead of being launched into a failure.
 */
private fun launchGame(
    context: android.content.Context,
    library: GameLibrary,
    entry: GameEntry,
    notify: (String) -> Unit,
) {
    // Prefer the path cached in `games.json`; re-resolve only when it is
    // missing, which happens for a folder added while its volume was not
    // mounted. A successful re-resolve is written back to the config.
    val path = entry.path ?: SafPaths.resolve(context, entry.uri)?.also {
        library.updatePath(entry.uri, it)
    }
    if (path == null) {
        notify("\"${entry.name}\" is not on a filesystem the engine can read.")
        return
    }
    context.startActivity(
        Intent(context, KrkrGameActivity::class.java)
            .putExtra(KrkrGameActivity.EXTRA_GAME_DIR, path)
            .putExtra(KrkrGameActivity.EXTRA_GAME_NAME, entry.name),
    )
}

@Composable
private fun EmptyLibrary(modifier: Modifier = Modifier, configPath: String) {
    Box(modifier, contentAlignment = Alignment.Center) {
        Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            modifier = Modifier.padding(32.dp),
        ) {
            Text(
                text = "No games yet",
                style = MaterialTheme.typography.headlineSmall,
            )
            Spacer(Modifier.height(8.dp))
            Text(
                text = "Add a folder containing a KiriKiri game (its .xp3 archives and startup.tjs).",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
            )
            Spacer(Modifier.height(24.dp))
            // Show where the library is stored: it is a plain JSON file the
            // user can back up or edit, so it should not be a secret.
            Text(
                text = "Library: $configPath",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.outline,
                textAlign = TextAlign.Center,
            )
        }
    }
}

@Composable
private fun GameCard(
    entry: GameEntry,
    onLaunch: () -> Unit,
    onRemove: () -> Unit,
) {
    Card(
        modifier = Modifier.fillMaxWidth().clickable(onClick = onLaunch),
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.surfaceContainerHigh,
        ),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(16.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(
                imageVector = Icons.Default.PlayArrow,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.primary,
            )
            Spacer(Modifier.width(16.dp))
            Column(Modifier.weight(1f)) {
                Text(
                    text = entry.name,
                    style = MaterialTheme.typography.titleMedium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Text(
                    text = entry.uri.lastPathSegment ?: entry.uri.toString(),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            IconButton(onClick = onRemove) {
                Icon(Icons.Default.Delete, contentDescription = "Remove ${entry.name}")
            }
        }
    }
}
