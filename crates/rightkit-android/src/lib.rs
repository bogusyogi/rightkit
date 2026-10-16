//! Android bridge for Right Suite apps (rightkit#149).
//!
//! * **UniFFI surface** (proc-macro style, UniFFI 0.29, library mode): [`AppDirs`], [`AndroidError`],
//!   [`LogLevel`] and the functions at the bottom of this file. Generated Kotlin lands in the
//!   package `com.orthiclabs.rightkit.android.core` (`uniffi.toml`).
//! * **JNI init** ([`jni_entry`], [`context`]): captures the `JavaVM`, the application `Context`
//!   and the app paths once, and shares them through [`ContextHandle`].
//! * **Keystore bridge** ([`keystore`]): installs the `rightkit-credentials` Android store.
//! * **Gradle module** under `android/`: Kotlin `RightKit.init`, the Keystore implementation, and
//!   packaging of `jniLibs/<abi>/*.so` plus the generated bindings.
//!
//! App-specific UniFFI functions (HeardRight's `load_model`, `push_pcm`, ...) belong in the app's
//! own cdylib, which depends on this crate and re-exports [`jni_entry`]; see the README.
//!
//! Everything pure (paths, ABI table, pin file, logcat formatting, error mapping) is unit-tested on
//! the host. The JNI and Keystore paths need a device or emulator and are unverified.

uniffi::setup_scaffolding!();

pub mod abi;
pub mod context;
pub mod error;
pub mod jni_entry;
pub mod keystore;
pub mod logcat;
pub mod paths;
pub mod toolchain;

pub use abi::{Abi, COMPILE_SDK, MIN_SDK, NDK_VERSION, SUPPORTED_ABIS};
pub use context::{handle, AndroidContext, ContextHandle, ContextHandleExt, StaticContext};
pub use error::{AndroidError, InitCode};
pub use logcat::LogLevel;
pub use paths::{AppDirs, AppPaths};

/// References the JNI entry point so a dependent cdylib keeps the symbol. Call it once from the
/// app crate (for example from an exported function), or `pub use` [`jni_entry`] instead.
#[doc(hidden)]
pub fn link_anchor() -> usize {
    jni_entry::Java_com_orthiclabs_rightkit_android_RightKitNative_nativeInit as *const () as usize
}

/// Initialises `rustls-platform-verifier` (the root store `rightkit-http` uses on Android) from the
/// captured context. Called by `init` when the `platform-verifier` feature is on.
///
/// UNVERIFIED on a device: written against `rustls_platform_verifier::android::init_with_env(&mut
/// Env, JObject)` of the 0.7 line and the `jni` 0.22 types (the same `jni` major the verifier
/// depends on, so the `Env` and `JObject` passed here are the verifier's own types). The
/// verifier's own Kotlin component must also be on the app's classpath (see README).
#[cfg(all(feature = "platform-verifier", target_os = "android"))]
pub fn init_platform_verifier(
    env: &mut jni::Env<'_>,
    context: &jni::objects::JObject<'_>,
) -> Result<(), AndroidError> {
    // The verifier takes the `JObject` by value, so hand it its own local reference and leave the
    // caller's untouched.
    let to_error = |e: &jni::errors::Error| AndroidError::Jni {
        code: error::jni_error_code(e),
    };
    let context = env.new_local_ref(context).map_err(|e| to_error(&e))?;
    rustls_platform_verifier::android::init_with_env(env, context).map_err(|e| to_error(&e))
}

/// Version of this bridge (the crate version).
#[uniffi::export]
pub fn bridge_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// `true` once `RightKit.init` has succeeded in this process.
#[uniffi::export]
pub fn is_initialised() -> bool {
    AndroidContext::get().is_ok()
}

/// The injected app paths and the conventional subdirectories under `files_dir`.
#[uniffi::export]
pub fn app_dirs() -> Result<AppDirs, AndroidError> {
    Ok(AndroidContext::get()?.paths().dirs())
}

/// Android ABI names this build packages (`arm64-v8a`, `x86_64`).
#[uniffi::export]
pub fn supported_abis() -> Vec<String> {
    SUPPORTED_ABIS
        .iter()
        .map(|a| a.android_name().to_string())
        .collect()
}

/// The module's `minSdk`.
#[uniffi::export]
pub fn min_sdk() -> u32 {
    MIN_SDK
}

/// Writes one line to logcat when `RightKit.init(debugLogging = true)` was used; otherwise drops
/// it. Never pass user text, transcripts or secrets.
#[uniffi::export]
pub fn log_debug(level: LogLevel, tag: String, message: String) {
    logcat::write(level, &tag, &message);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exported_helpers_work_before_init() {
        assert!(!is_initialised());
        assert_eq!(app_dirs(), Err(AndroidError::NotInitialised));
        assert_eq!(min_sdk(), MIN_SDK);
        assert_eq!(supported_abis(), vec!["arm64-v8a", "x86_64"]);
        assert_eq!(bridge_version(), env!("CARGO_PKG_VERSION"));
        log_debug(LogLevel::Info, "t".into(), "m".into());
    }
}
