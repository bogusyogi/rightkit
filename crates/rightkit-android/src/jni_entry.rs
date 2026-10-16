//! The JNI entry point Kotlin calls once: `RightKitNative.nativeInit`.
//!
//! There is deliberately no `JNI_OnLoad`: the `JavaVM` is read from the `JNIEnv` of the init call,
//! and the application classes this crate needs are looked up on that (Java) thread, where the app
//! class loader is visible. A native thread could not find them later.
//!
//! ## Linking into an app's own cdylib
//! An app crate that depends on this crate as an rlib must re-export this module so the linker
//! keeps the symbol: `pub use rightkit_android::jni_entry::*;` (or call
//! [`crate::link_anchor`]). UNVERIFIED on a device build.

use crate::context::AndroidContext;
use crate::error::{AndroidError, InitCode};
use crate::keystore::{JniKeystoreBridge, KEYSTORE_CLASS};
use crate::paths::AppPaths;
use jni::objects::{JClass, JObject, JString};
use jni::sys::{jboolean, jint};
use jni::JNIEnv;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

/// `external fun nativeInit(context: Context, filesDir: String, cacheDir: String,
/// nativeLibraryDir: String, debugLogging: Boolean): Int` of
/// `com.orthiclabs.rightkit.android.RightKitNative` (a Kotlin `object` with `@JvmStatic`).
/// Returns an [`InitCode`] as an int; never throws.
#[no_mangle]
pub extern "system" fn Java_com_orthiclabs_rightkit_android_RightKitNative_nativeInit<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    context: JObject<'local>,
    files_dir: JString<'local>,
    cache_dir: JString<'local>,
    native_library_dir: JString<'local>,
    debug_logging: jboolean,
) -> jint {
    let run = catch_unwind(AssertUnwindSafe(|| {
        let read = |env: &mut JNIEnv<'local>, s: &JString<'local>| -> Option<String> {
            env.get_string(s).ok().map(String::from)
        };
        let (Some(files), Some(cache), Some(native)) = (
            read(&mut env, &files_dir),
            read(&mut env, &cache_dir),
            read(&mut env, &native_library_dir),
        ) else {
            return InitCode::Jni;
        };
        match initialise(&mut env, &context, &files, &cache, &native, debug_logging != 0) {
            Ok(()) => InitCode::Ok,
            Err(code) => code,
        }
    }));
    run.unwrap_or(InitCode::Panic).as_i32()
}

/// Captures the context and installs the Keystore store. Idempotent when repeated with the same
/// paths.
pub fn initialise(
    env: &mut JNIEnv<'_>,
    app_context: &JObject<'_>,
    files_dir: &str,
    cache_dir: &str,
    native_library_dir: &str,
    debug_logging: bool,
) -> Result<(), InitCode> {
    let paths = AppPaths::new(files_dir, cache_dir, native_library_dir)
        .map_err(|_| InitCode::InvalidArgument)?;
    if app_context.is_null() {
        return Err(InitCode::InvalidArgument);
    }
    crate::logcat::set_debug_logging(debug_logging);

    let files = paths.files_dir.clone();
    if let Ok(existing) = AndroidContext::get() {
        // A repeated init repeats the paths; it still re-installs the Keystore store below, so a
        // failed first attempt can be retried.
        crate::context::paths_match(crate::context::ContextHandle::paths(existing), &paths)
            .map_err(|_| InitCode::PathsChanged)?;
    } else {
        let vm = env.get_java_vm().map_err(|_| InitCode::Jni)?;
        let app_ref = env.new_global_ref(app_context).map_err(|_| InitCode::Jni)?;
        let class = match env.find_class(KEYSTORE_CLASS) {
            Ok(class) => class,
            Err(_) => {
                if env.exception_check().unwrap_or(false) {
                    let _ = env.exception_clear();
                }
                return Err(InitCode::Jni);
            }
        };
        let class_ref = env.new_global_ref(&class).map_err(|_| InitCode::Jni)?;
        AndroidContext::publish(AndroidContext::new(vm, app_ref, class_ref, paths)).map_err(
            |e| match e {
                AndroidError::Invalid { .. } => InitCode::PathsChanged,
                _ => InitCode::Jni,
            },
        )?;
    }

    rightkit_credentials::android::install(Arc::new(JniKeystoreBridge), &files)
        .map_err(|_| InitCode::KeystoreInstall)?;

    #[cfg(all(feature = "platform-verifier", target_os = "android"))]
    crate::init_platform_verifier(env, app_context).map_err(|_| InitCode::Jni)?;

    crate::logcat::write(
        crate::logcat::LogLevel::Debug,
        "rightkit-android",
        "initialised",
    );
    Ok(())
}
