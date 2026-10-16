// Standalone build of the module (for development of the module itself). An app includes the module
// from its own settings instead:
//   include(":rightkit-android")
//   project(":rightkit-android").projectDir = file("<path to rightkit>/crates/rightkit-android/android")
// In that case the app's plugin versions apply and this file is not read.

pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
    plugins {
        // UNVERIFIED pins; confirm on the first real build (rightkit#148).
        id("com.android.library") version "8.7.3"
        id("org.jetbrains.kotlin.android") version "2.0.21"
    }
}

dependencyResolutionManagement {
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "rightkit-android"
