package dev.krkr.rs

import android.os.Bundle
import com.google.androidgamesdk.GameActivity

/**
 * Hosts the engine for one game.
 *
 * This extends the AGDK [GameActivity], which is the Activity the Rust
 * `android_main` attaches to (declared via the `android.app.lib_name`
 * meta-data in the manifest). It derives from `AndroidAppCompat`, so unlike
 * `NativeActivity` it is an ordinary `AppCompatActivity` and could host a
 * Compose view hierarchy — that is why the engine uses this backend
 * (`docs/android.md` §1).
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
         * guessing a directory.
         *
         * `@JvmField` (not `@JvmStatic`) is deliberate: it puts a real static
         * **field** on this class, which is what the JNI `GetStaticField` read
         * in `crates/android/src/lib.rs` expects. A `@JvmStatic var` in a
         * companion would expose accessor *methods* instead, and the field
         * would live on the companion object.
         */
        @JvmField
        var pendingGameDir: String? = null

        /** Display name of the game being launched, for logs and the title. */
        @JvmField
        var pendingGameName: String? = null
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        // Must be set before super.onCreate(): that call loads the native
        // library and enters android_main, which reads these values.
        pendingGameDir = intent.getStringExtra(EXTRA_GAME_DIR)
        pendingGameName = intent.getStringExtra(EXTRA_GAME_NAME)
        super.onCreate(savedInstanceState)
    }

    override fun onDestroy() {
        pendingGameDir = null
        pendingGameName = null
        super.onDestroy()
    }
}
