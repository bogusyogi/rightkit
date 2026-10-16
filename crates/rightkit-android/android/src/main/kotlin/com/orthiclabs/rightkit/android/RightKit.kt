package com.orthiclabs.rightkit.android

import android.content.Context

/**
 * One-call setup of the Rust bridge. Call [init] from `Application.onCreate` (and from nothing that
 * runs earlier), before any generated UniFFI function or any Right Suite Rust code is used.
 *
 * `init` loads the native library, hands the application [Context], `filesDir`, `cacheDir` and
 * `nativeLibraryDir` to Rust, and installs the Keystore-backed credential store. It is idempotent
 * for the same paths and safe to call from several processes (a service process calls it
 * in its own process; each process has its own native library instance).
 */
object RightKit {
    /** Default native library name (`librightkit_android.so`). */
    const val DEFAULT_LIBRARY = "rightkit_android"

    private val lock = Any()
    private var initialised = false

    class InitException(val code: Int) :
        RuntimeException("rightkit-android init failed (code $code): ${describe(code)}")

    /**
     * @param libraryName the cdylib to load. An app with its own Rust cdylib that links
     *   rightkit-android passes its name (for example `heardright_android`); that one library then
     *   carries both the bridge and the app's UniFFI components.
     * @param debugLogging enables the logcat sink. Leave it off in release builds.
     * @throws InitException when the native side reports a non-zero code.
     */
    @JvmStatic
    @JvmOverloads
    fun init(
        context: Context,
        debugLogging: Boolean = false,
        libraryName: String = DEFAULT_LIBRARY,
    ) {
        synchronized(lock) {
            val app = context.applicationContext
            if (!initialised) {
                System.loadLibrary(libraryName)
            }
            val code = RightKitNative.nativeInit(
                app,
                app.filesDir.absolutePath,
                app.cacheDir.absolutePath,
                app.applicationInfo.nativeLibraryDir,
                debugLogging,
            )
            if (code != 0) throw InitException(code)
            initialised = true
        }
    }

    /** Text for an init code (the Rust `InitCode`). */
    @JvmStatic
    fun describe(code: Int): String = when (code) {
        0 -> "ok"
        1 -> "a path was rejected (empty, relative or containing `..`)"
        2 -> "a JNI call failed (is RightKitKeystore kept by ProGuard/R8?)"
        3 -> "init was already called with different paths"
        4 -> "the Keystore credential store could not be installed"
        5 -> "a panic was caught in the native library"
        else -> "unknown"
    }
}
