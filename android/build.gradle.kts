// Top-level Gradle build. See docs/android.md.
//
// Versions are the latest stable as published (AGP 9.4.0, Kotlin 2.4.20), which
// is what supports `compileSdk = 37` in app/build.gradle.kts. They are pinned
// rather than ranged so a CI run is reproducible; bump them together, and check
// the AGP release notes for the `compileSdk` each one accepts.
plugins {
    id("com.android.application") version "9.4.0" apply false
    id("org.jetbrains.kotlin.android") version "2.4.20" apply false
    id("org.jetbrains.kotlin.plugin.compose") version "2.4.20" apply false
}
