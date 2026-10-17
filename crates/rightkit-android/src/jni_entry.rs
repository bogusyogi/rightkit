//! The JNI entry point Kotlin calls once: `RightKitNative.nativeInit`.
//!
//! There is deliberately no `JNI_OnLoad`: the `JavaVM` is read from the `Env` of the init call,
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
use jni::objects::{JClass, JObject, JString, Reference};
use jni::strings::JNIString;
use jni::sys::{jboolean, jint};
use jni::{Env, EnvUnowned, Outcome};
use std::sync::Arc;

/// `external fun nativeInit(context: Context, filesDir: String, cacheDir: String,
/// nativeLibraryDir: String, debugLogging: Boolean): Int` of
/// `com.orthiclabs.rightkit.android.RightKitNative` (a Kotlin `object` with `@JvmStatic`).
/// Returns an [`InitCode`] as an int; never throws.
#[no_mangle]
pub extern "system" fn Java_com_orthiclabs_rightkit_android_RightKitNative_nativeInit<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    context: JObject<'local>,
    files_dir: JString<'local>,
    cache_dir: JString<'local>,
    native_library_dir: JString<'local>,
    debug_logging: jboolean,
) -> jint {
    // `with_env` catches a panic in the closure and reports it as `Outcome::Panic`, so nothing
    // unwinds across the native-method boundary.
    let outcome = env
        .with_env(|env| -> jni::errors::Result<InitCode> {
            let code = run_init(
                env,
                &context,
                &files_dir,
                &cache_dir,
                &native_library_dir,
                debug_logging,
            );
            // Never hand a pending exception back to Kotlin: nativeInit reports through its code.
            if env.exception_check() {
                env.exception_clear();
            }
            Ok(code)
        })
        .into_outcome();
    let code = match outcome {
        Outcome::Ok(code) => code,
        Outcome::Err(_) => InitCode::Jni,
        Outcome::Panic(_) => InitCode::Panic,
    };
    code.as_i32()
}

fn run_init(
    env: &mut Env<'_>,
    context: &JObject<'_>,
    files_dir: &JString<'_>,
    cache_dir: &JString<'_>,
    native_library_dir: &JString<'_>,
    debug_logging: bool,
) -> InitCode {
    let (Ok(files), Ok(cache), Ok(native)) = (
        files_dir.try_to_string(env),
        cache_dir.try_to_string(env),
        native_library_dir.try_to_string(env),
    ) else {
        return InitCode::Jni;
    };
    match initialise(env, context, &files, &cache, &native, debug_logging) {
        Ok(()) => InitCode::Ok,
        Err(code) => code,
    }
}

/// Captures the context and installs the Keystore store. Idempotent when repeated with the same
/// paths.
pub fn initialise(
    env: &mut Env<'_>,
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
        let class = match env.find_class(JNIString::new(KEYSTORE_CLASS)) {
            Ok(class) => class,
            Err(_) => {
                if env.exception_check() {
                    env.exception_clear();
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
