package dev.krkr.rs

import android.content.Intent
import android.os.Bundle

import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import com.google.androidgamesdk.GameActivity

/**
 * Hosts the engine for one game.
 *
 * This extends the AGDK [GameActivity], which is the Activity the Rust
 * `android_main` attaches to (declared via the `android.app.lib_name`
 * meta-data in the manifest). It derives from `AndroidAppCompat`, so unlike
 * `NativeActivity` it is an ordinary `AppCompatActivity` and could host a
 * Compose view hierarchy — that is why the engine uses this backend
 * (`docs.old/android.md` §1).
 *
 * The engine needs to know which folder to mount before its `android_main`
 * runs, and the only channel that exists at that point is a JNI read of this
 * class. A static field is the pragmatic mechanism; it is written in
 * [onCreate] *before* `super.onCreate`, because `super` is what loads the
 * native library and starts `android_main`.
 */
class KrkrGameActivity : GameActivity() {

    companion object {
        const val EXTRA_GAME_DIR = "dev.krkr.rs.GAME_DIR"
        const val EXTRA_GAME_NAME = "dev.krkr.rs.GAME_NAME"

        /**
         * The folder the engine should mount, as a filesystem path.
         *
         * Read from native code (`crates/android`) through JNI as soon as
         * `android_main` starts. Null until [onCreate] has run, so the native
         * side must treat null as a fatal configuration error rather than
         * guessing a directory. It is not cleared when the Activity is
         * destroyed: a new instance republishes it in [onCreate] before that
         * instance's `android_main` reads, and clearing it early would race a
         * slow first read.
         *
         * `@JvmField` (not `@JvmStatic`) is deliberate: it puts a real static
         * **field** on this class, which is what the JNI `GetStaticField` read
         * in `crates/android/src/jni_fields.rs` expects. A `@JvmStatic var` in a
         * companion would expose accessor *methods* instead, and the field
         * would live on the companion object.
         */
        @JvmField
        var pendingGameDir: String? = null

        /** Display name of the game being launched, for logs and the title. */
        @JvmField
        var pendingGameName: String? = null

        /**
         * Where the engine writes its state file and log, as a filesystem path.
         *
         * The native side reads this through JNI exactly like [pendingGameDir],
         * and writes `<this dir>/krkr_state.json` + `<this dir>/krkr.log`. It is
         * set to `filesDir.absolutePath` in [onCreate] **before** `super.onCreate`
         * for the same reason: `super` enters `android_main`, so the field is
         * always populated before native code can observe it.
         *
         * Native code copies the directory once at `android_main` startup and
         * never reads the field again, so nothing clears it afterwards; the
         * terminal `stopped`/`failed` write uses the copy, not this field.
         */
        @JvmField
        var pendingStateDir: String? = null

        /**
         * The frame-rate limit the user chose, as a decimal string.
         *
         * Read from native code (`crates/android`) through JNI exactly like
         * [pendingGameDir], and resolved there into the `RuntimeConfig` the
         * engine receives. Null means the launcher had no persisted choice, so
         * the engine's default applies.
         *
         * `@JvmField` is deliberate: JNI `GetStaticField` reads a real static
         * *field*, not a companion accessor.
         */
        @JvmField
        var pendingFrameRate: String? = null

        /**
         * Whether every mounted archive is read into memory at mount (`true`
         * or `false`), read through JNI like [pendingFrameRate].
         *
         * Null means the launcher had no persisted choice, so the engine's
         * `false` default applies.
         */
        @JvmField
        var pendingPreloadArchives: String? = null

        /**
         * Whether the debug overlay (frame rate and resident memory) is drawn
         * (`true` or `false`), read through JNI like [pendingFrameRate].
         *
         * Null means the launcher had no persisted choice, so the engine's
         * `false` default applies.
         */
        @JvmField
        var pendingDebugOverlay: String? = null

        /**
         * The engine log level the user chose, one of `off`, `error`, `warn`,
         * `info`, `debug`, `trace`, read through JNI like [pendingFrameRate].
         *
         * Null or empty means the engine's `info` default applies. `debug`
         * and `trace` record far more per frame, so the log fills and rotates
         * sooner.
         */
        @JvmField
        var pendingLogLevel: String? = null
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        // The launcher normally initialises logging first, but this Activity can
        // be restored on its own after a process death; make sure the handler and
        // the log file are set up either way.
        LauncherLog.init(this)
        LauncherLog.installUncaughtExceptionHandler()

        // Must be set before super.onCreate(): that call loads the native
        // library and enters android_main, which reads these values.
        pendingGameDir = intent.getStringExtra(EXTRA_GAME_DIR)
        pendingGameName = intent.getStringExtra(EXTRA_GAME_NAME)
        pendingStateDir = filesDir.absolutePath
        publishRuntimeSettings()
        LauncherLog.i(
            "KrkrGameActivity.onCreate: game=${pendingGameName ?: "?"} dir=${pendingGameDir ?: "?"} stateDir=${pendingStateDir ?: "?"} fps=${pendingFrameRate ?: "?"} preload=${pendingPreloadArchives ?: "?"} overlay=${pendingDebugOverlay ?: "?"} log=${pendingLogLevel ?: "?"}",
        )
        super.onCreate(savedInstanceState)

        // Immersive: the surface should be the whole screen. The theme requests
        // fullscreen, but from targetSdk 35 Android enforces edge-to-edge and
        // ignores that, so the system bars have to be hidden explicitly.
        WindowCompat.setDecorFitsSystemWindows(window, false)
        WindowInsetsControllerCompat(window, window.decorView).apply {
            hide(WindowInsetsCompat.Type.systemBars())
            systemBarsBehavior =
                WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        }
    }

    override fun onStart() {
        super.onStart()
        LauncherLog.i("KrkrGameActivity.onStart")
    }

    override fun onResume() {
        super.onResume()
        LauncherLog.i("KrkrGameActivity.onResume")
    }

    override fun onPause() {
        super.onPause()
        LauncherLog.i("KrkrGameActivity.onPause")
    }

    override fun onStop() {
        super.onStop()
        LauncherLog.i("KrkrGameActivity.onStop")
    }

    /**
     * The end of the Activity.
     *
     * A finished Activity logs this line; a process that died does not, so
     * the launcher log tells the two apart.
     */
    override fun onDestroy() {
        LauncherLog.i("KrkrGameActivity.onDestroy: finishing=$isFinishing")
        super.onDestroy()
    }

    /**
     * `launchMode="singleTask"`: a second launch while an instance is alive is
     * delivered here rather than to [onCreate]. The native side reads
     * [pendingGameDir] once, when `android_main` starts, so these fields must
     * keep describing the game the engine is actually running. A different game
     * cannot take effect, so such an intent is ignored; the launcher refuses it
     * before starting anything, which keeps the pending-launch marker naming the
     * running game.
     */
    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        val gameDir = intent.getStringExtra(EXTRA_GAME_DIR)
        val currentDir = pendingGameDir
        if (currentDir != null && gameDir != currentDir) {
            LauncherLog.w(
                "onNewIntent ignored: the engine is already running \"${pendingGameName ?: "?"}\", " +
                    "not \"${intent.getStringExtra(EXTRA_GAME_NAME) ?: "?"}\"",
            )
            return
        }
        pendingGameDir = gameDir
        pendingGameName = intent.getStringExtra(EXTRA_GAME_NAME)
        pendingStateDir = filesDir.absolutePath
        publishRuntimeSettings()
        LauncherLog.i(
            "KrkrGameActivity.onNewIntent: engine already running; game=${pendingGameName ?: "?"} dir=${pendingGameDir ?: "?"} fps=${pendingFrameRate ?: "?"} preload=${pendingPreloadArchives ?: "?"} overlay=${pendingDebugOverlay ?: "?"} log=${pendingLogLevel ?: "?"}",
        )
    }

    /**
     * Copies the launcher's saved runtime settings onto the static fields the
     * native side reads. Must run before `super.onCreate` enters `android_main`.
     */
    private fun publishRuntimeSettings() {
        val settings = RuntimeSettingsStore(this).read()
        pendingFrameRate = settings.frameRate
        pendingPreloadArchives = settings.preloadArchives
        pendingDebugOverlay = settings.debugOverlay
        pendingLogLevel = settings.logLevel
    }
}
