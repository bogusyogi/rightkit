//! The process-wide Android context: the `JavaVM`, the application `Context`, and the app paths,
//! captured once by `RightKit.init` and shared through the [`ContextHandle`] trait.
//!
//! ## Who takes what
//! * Crates layered **above** this one (an app's own cdylib, HeardRight's `heardright-android`)
//!   call [`handle`] and use [`ContextHandle`] / [`ContextHandleExt::with_env`] for JNI work.
//! * Crates layered **below** it never depend on this crate. `rightkit-credentials` takes a
//!   `KeystoreBridge` and a files directory (this crate installs both, see `keystore`);
//!   `rightkit-http` initialises `rustls-platform-verifier` (feature `platform-verifier`, done by
//!   [`crate::init_platform_verifier`] during init); `rightkit-models`, `rightkit-logs` and
//!   `rightkit-app-settings` receive plain directory strings from [`crate::app_dirs`].

use crate::error::{jni_error_code, AndroidError, JNI_NO_VM};
use crate::paths::AppPaths;
use jni::objects::{GlobalRef, JObject};
use jni::{JNIEnv, JavaVM};
use std::sync::OnceLock;

/// Read-only access to the captured Android context. Object safe; `Send + Sync`.
pub trait ContextHandle: Send + Sync {
    /// The app-private paths Kotlin handed over at init.
    fn paths(&self) -> &AppPaths;

    /// Runs `f` with the calling thread attached to the JavaVM, the JNI environment and the
    /// application `Context` (an `android.content.Context`, never an `Activity`). The thread is
    /// detached again when it was attached by this call. `f` is called at most once.
    ///
    /// A Java exception left pending by `f` is cleared and reported as [`AndroidError::Jni`].
    /// Prefer [`ContextHandleExt::with_env`], which returns a value.
    fn with_env_dyn(
        &self,
        f: &mut dyn FnMut(&mut JNIEnv<'_>, &JObject<'_>) -> jni::errors::Result<()>,
    ) -> Result<(), AndroidError>;
}

/// Generic convenience over [`ContextHandle::with_env_dyn`].
pub trait ContextHandleExt: ContextHandle {
    fn with_env<R>(
        &self,
        f: impl FnOnce(&mut JNIEnv<'_>, &JObject<'_>) -> jni::errors::Result<R>,
    ) -> Result<R, AndroidError> {
        let mut f = Some(f);
        let mut out: Option<R> = None;
        self.with_env_dyn(&mut |env, ctx| {
            if let Some(f) = f.take() {
                out = Some(f(env, ctx)?);
            }
            Ok(())
        })?;
        out.ok_or(AndroidError::Jni { code: JNI_NO_VM })
    }
}

impl<T: ContextHandle + ?Sized> ContextHandleExt for T {}

/// The real context, built by `init`.
pub struct AndroidContext {
    vm: JavaVM,
    app_context: GlobalRef,
    keystore_class: GlobalRef,
    paths: AppPaths,
}

static CONTEXT: OnceLock<AndroidContext> = OnceLock::new();

impl AndroidContext {
    pub(crate) fn new(
        vm: JavaVM,
        app_context: GlobalRef,
        keystore_class: GlobalRef,
        paths: AppPaths,
    ) -> Self {
        AndroidContext {
            vm,
            app_context,
            keystore_class,
            paths,
        }
    }

    /// The initialised context, or [`AndroidError::NotInitialised`].
    pub fn get() -> Result<&'static AndroidContext, AndroidError> {
        CONTEXT.get().ok_or(AndroidError::NotInitialised)
    }

    pub fn java_vm(&self) -> &JavaVM {
        &self.vm
    }

    /// A global reference to the application `Context`.
    pub fn application_context(&self) -> &GlobalRef {
        &self.app_context
    }

    pub(crate) fn keystore_class(&self) -> &GlobalRef {
        &self.keystore_class
    }

    /// Stores `ctx` as the process-wide context. When one is already stored it is kept and the
    /// result says whether the existing paths equal the new ones (idempotent `init`).
    pub(crate) fn publish(ctx: AndroidContext) -> Result<(), AndroidError> {
        let paths = ctx.paths.clone();
        match CONTEXT.set(ctx) {
            Ok(()) => Ok(()),
            Err(_) => paths_match(Self::get()?.paths(), &paths),
        }
    }
}

/// Idempotence rule of `init`: the first paths win and later calls must repeat them.
pub(crate) fn paths_match(existing: &AppPaths, requested: &AppPaths) -> Result<(), AndroidError> {
    if existing == requested {
        Ok(())
    } else {
        Err(AndroidError::invalid(
            "init was already called with different paths",
        ))
    }
}

impl ContextHandle for AndroidContext {
    fn paths(&self) -> &AppPaths {
        &self.paths
    }

    fn with_env_dyn(
        &self,
        f: &mut dyn FnMut(&mut JNIEnv<'_>, &JObject<'_>) -> jni::errors::Result<()>,
    ) -> Result<(), AndroidError> {
        let mut guard = self
            .vm
            .attach_current_thread()
            .map_err(|_| AndroidError::Jni { code: JNI_NO_VM })?;
        match f(&mut guard, self.app_context.as_obj()) {
            Ok(()) => Ok(()),
            Err(e) => {
                if guard.exception_check().unwrap_or(false) {
                    let _ = guard.exception_clear();
                }
                Err(AndroidError::Jni {
                    code: jni_error_code(&e),
                })
            }
        }
    }
}

/// The process-wide handle for crates above this one.
pub fn handle() -> Result<&'static dyn ContextHandle, AndroidError> {
    AndroidContext::get().map(|c| c as &dyn ContextHandle)
}

/// A host-side stand-in that has paths but no JVM: `with_env_dyn` always fails with
/// `Jni { code: JNI_NO_VM }`. For unit tests of code written against [`ContextHandle`].
pub struct StaticContext {
    paths: AppPaths,
}

impl StaticContext {
    pub fn new(paths: AppPaths) -> Self {
        StaticContext { paths }
    }
}

impl ContextHandle for StaticContext {
    fn paths(&self) -> &AppPaths {
        &self.paths
    }

    fn with_env_dyn(
        &self,
        _f: &mut dyn FnMut(&mut JNIEnv<'_>, &JObject<'_>) -> jni::errors::Result<()>,
    ) -> Result<(), AndroidError> {
        Err(AndroidError::Jni { code: JNI_NO_VM })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(files: &str) -> AppPaths {
        AppPaths::new(files, "/c", "/n").unwrap()
    }

    #[test]
    fn there_is_no_context_before_init() {
        assert_eq!(
            AndroidContext::get().err(),
            Some(AndroidError::NotInitialised)
        );
        assert!(handle().is_err());
    }

    #[test]
    fn static_context_exposes_paths_and_refuses_jni() {
        let ctx = StaticContext::new(paths("/f"));
        assert_eq!(ctx.paths().files_dir, "/f");
        let dynamic: &dyn ContextHandle = &ctx;
        assert_eq!(
            dynamic.with_env(|_, _| Ok(1)),
            Err(AndroidError::Jni { code: JNI_NO_VM })
        );
    }

    #[test]
    fn repeated_init_must_repeat_the_paths() {
        assert!(paths_match(&paths("/f"), &paths("/f")).is_ok());
        assert!(matches!(
            paths_match(&paths("/f"), &paths("/g")),
            Err(AndroidError::Invalid { .. })
        ));
    }
}
