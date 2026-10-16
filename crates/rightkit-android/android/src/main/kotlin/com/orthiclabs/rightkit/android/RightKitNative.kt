package com.orthiclabs.rightkit.android

import android.content.Context

/**
 * The JNI entry point of the Rust bridge. The native symbol is
 * `Java_com_orthiclabs_rightkit_android_RightKitNative_nativeInit` (crates/rightkit-android
 * src/jni_entry.rs); do not rename this object, its package or the method.
 */
internal object RightKitNative {
    /** Returns 0 on success, otherwise an `InitCode` of the Rust crate. Never throws. */
    @JvmStatic
    external fun nativeInit(
        context: Context,
        filesDir: String,
        cacheDir: String,
        nativeLibraryDir: String,
        debugLogging: Boolean,
    ): Int
}
