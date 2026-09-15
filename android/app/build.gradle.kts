// The krkr-rs Android frontend: a Material 3 launcher that manages a library
// of KiriKiri game folders (picked through the system file picker / SAF) and
// hands the chosen one to the engine Activity.
//
// The engine itself is Rust (`crates/android`, a cdylib staged into
// `src/main/jniLibs/arm64-v8a/libkrkr_android.so` by
// `crates/android/build-android.sh`). This module only builds the UI + the
// hosting Activity; it deliberately contains no engine logic.

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
}

// The engine lib name must match the crate's `[lib] name` in crates/android.
val engineLibName = "krkr_android"

android {
    namespace = "dev.krkr.rs"
    // Latest available platform, and the level the Kotlin/Java side compiles
    // against. `compileSdk` lives here, not in `defaultConfig`.
    //
    // Note the SDK names its platform packages with a minor version
    // (`platforms;android-37.2`) — if AGP cannot resolve `37` on its own,
    // `compileSdkMinor` picks the QPR, and the package must be installed from
    // that same line (see .github/workflows/android.yml).
    compileSdk = 37

    defaultConfig {
        applicationId = "dev.krkr.rs"
        // Android 15+ only. The engine's native code is compiled against API 35
        // (see crates/android/build-android.sh — the NDK r27c sysroot tops out
        // there), and supporting older releases would mean carrying a lower
        // native API level and the compatibility branches that come with it.
        minSdk = 35
        // Kept equal to `compileSdk` so the app is validated against the newest
        // behaviours it can be.
        targetSdk = 37
        versionCode = 1
        versionName = "0.1.0"

        ndk {
            // arm64-v8a only, by decision (docs/android.md §1). Adding an ABI
            // here requires a matching build for it (`crates/android/
            // build-android.sh` currently hardcodes arm64-v8a), otherwise the
            // APK ships without a `.so` for it and crashes at launch.
            abiFilters += listOf("arm64-v8a")
        }
    }

    buildFeatures {
        compose = true
    }

    // AGP strips the prebuilt `.so` when packaging, which needs an NDK; the
    // build script and Gradle must agree on the revision. In CI the NDK is
    // installed into the SDK by sdkmanager, locally it is symlinked there.
    ndkVersion = "27.2.12479018"

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    // No `kotlinOptions { jvmTarget = … }`: AGP 9's built-in Kotlin support
    // removed that DSL (`Unresolved reference 'kotlinOptions'`) and derives the
    // Kotlin `jvmTarget` from `compileOptions` above, which is what keeps the
    // two in step anyway.

    // The Rust build produces the `.so`; Gradle must not try to build native
    // code itself. `build-android.sh` stages the library into jniLibs, so there
    // is nothing to configure here.
    packaging {
        jniLibs {
            useLegacyPackaging = false
        }
    }

    buildTypes {
        // Release, signed with the debug key so the APK is installable. This is
        // an experimental, side-loaded app (docs/android.md §1): a real release
        // would need its own signing key, which is deliberately out of scope.
        //
        // No minification: the UI is small, and R8 only adds a way for this to
        // break in a way that is hard to debug.
        getByName("release") {
            isMinifyEnabled = false
            signingConfig = signingConfigs.getByName("debug")
        }
    }
}

dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2026.09.00")
    implementation(composeBom)

    implementation("androidx.core:core-ktx:1.19.0")
    implementation("androidx.activity:activity-compose:1.13.0")
    implementation("androidx.lifecycle:lifecycle-runtime-ktx:2.11.0")

    // `androidx.games:games-activity`'s GameActivity derives from
    // AppCompatActivity, so appcompat has to be on the compile classpath even
    // though we never reference it directly — without it the Kotlin compiler
    // fails with "Cannot access 'androidx.appcompat.app.AppCompatActivity' which
    // is a supertype of 'dev.krkr.rs.KrkrGameActivity'".
    implementation("androidx.appcompat:appcompat:1.8.0")

    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-graphics")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-core")

    // AGDK GameActivity: the Activity base class the engine's `android_main`
    // attaches to. It derives from AndroidAppCompat, so the engine Activity can
    // host a normal view hierarchy. The artifact lives under the `androidx.games`
    // group (the `com.google.androidgamesdk` coordinate is not published to a
    // reachable repository), but the class it supplies is still
    // `com.google.androidgamesdk.GameActivity` — which is what
    // `KrkrGameActivity.kt` imports.
    implementation("androidx.games:games-activity:4.4.2")
}
