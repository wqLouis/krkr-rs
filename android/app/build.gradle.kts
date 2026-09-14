// The krkr-rs Android frontend: a Material 3 launcher that manages a library
// of KiriKiri game folders (picked through the system file picker / SAF) and
// hands the chosen one to the engine Activity.
//
// The engine itself is Rust (`crates/android`, a cdylib dropped into
// `src/main/jniLibs/arm64-v8a/libkrkr_android.so` by `cargo ndk`). This module
// only builds the UI + the hosting Activity; it deliberately contains no
// engine logic.

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

// The engine lib name must match the crate's `[lib] name` in crates/android.
val engineLibName = "krkr_android"

android {
    namespace = "dev.krkr.rs"
    compileSdk = 35

    defaultConfig {
        applicationId = "dev.krkr.rs"
        // API 24 is where the AGDK GameActivity + modern MediaCodec APIs the
        // engine targets are all available. `minSdk` may need raising if a
        // dependency demands it; document any change in docs/android.md.
        minSdk = 24
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"

        ndk {
            // arm64-v8a only, by decision (docs/android.md §1). Adding an ABI
            // here requires a matching `cargo ndk -t <abi>` build, otherwise
            // the APK ships without a `.so` for it and crashes at launch.
            abiFilters += listOf("arm64-v8a")
        }
    }

    buildFeatures {
        compose = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }

    // The Rust build produces the `.so`; Gradle must not try to build native
    // code itself. `cargo-ndk` writes into jniLibs, so nothing to configure.
    packaging {
        jniLibs {
            useLegacyPackaging = false
        }
    }

    buildTypes {
        // Debug only, matching the project's dev-binary rule. A release build
        // would also need signing config, which is deliberately out of scope.
        getByName("debug") {
            isMinifyEnabled = false
        }
    }
}

dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2024.12.01")
    implementation(composeBom)

    implementation("androidx.core:core-ktx:1.15.0")
    implementation("androidx.activity:activity-compose:1.9.3")
    implementation("androidx.lifecycle:lifecycle-runtime-ktx:2.8.7")

    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-graphics")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-core")

    // AGDK GameActivity: the Activity base class the engine's `android_main`
    // attaches to. It derives from AndroidAppCompat, so the engine Activity
    // can host a normal view hierarchy.
    implementation("com.google.androidgamesdk:games-activity:4.2.0")
}
