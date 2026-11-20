//! The one resolver for an app's per-user data root, handed in and never looked up behind the
//! caller's back.
//!
//! [`mode::roots`] resolves the directory according to the supplied [`AppId`]. Each binary's
//! `main` passes [`AppDataRoot::production`] to [`AppDataRoot::set`]
//! before anything reads it, and a test passes a temp directory. A process that was never handed a
//! root (every test that does not set one) gets a per-process temp root, so nothing can reach the
//! user's real folder unless a `main` asked for it.
//!
//! The app's identity (`AppId`), the temp folder prefix and the display label in the panic text are
//! inputs: no product name lives here.

use std::path::PathBuf;
use std::sync::RwLock;

use crate::mode::{self, AppId};

/// An app's data root: one value per app, usually a `static`.
pub struct AppDataRoot {
    app: AppId,
    temp_prefix: &'static str,
    label: &'static str,
    injected: RwLock<Option<PathBuf>>,
}

impl AppDataRoot {
    /// `temp_prefix` names the per-process temp folder (`<temp>/<temp_prefix>-<pid>/<dir name>`);
    /// `label` names the app in the panic text of [`Self::production`].
    pub const fn new(app: AppId, temp_prefix: &'static str, label: &'static str) -> Self {
        Self {
            app,
            temp_prefix,
            label,
            injected: RwLock::new(None),
        }
    }

    /// The app's data folder name.
    pub const fn dir_name(&self) -> &'static str {
        self.app.dir_name()
    }

    /// Hand this process its app-data root: a binary's `main` passes [`Self::production`], a test
    /// passes a temp directory. A later call replaces the root (tests that share a process do so
    /// in turn).
    pub fn set(&self, root: PathBuf) {
        *self.injected.write().unwrap_or_else(|e| e.into_inner()) = Some(root);
    }

    /// The root a process gets when nothing handed it one: a per-process temp directory.
    pub fn uninjected(&self) -> PathBuf {
        std::env::temp_dir()
            .join(format!("{}-{}", self.temp_prefix, std::process::id()))
            .join(self.app.dir_name())
    }

    /// Resolve the data directory using the app's configured run mode and root policy.
    /// When the OS gives no usable base folder this panics with
    /// the reason: app data must never land in the current directory.
    ///
    /// The Windows per-user directory resolver caches its result for the process.
    pub fn production(&self) -> PathBuf {
        let run_mode = mode::init_or_exit(&self.app);
        mode::roots(&self.app, run_mode)
            .map(|roots| roots.app_dir)
            .unwrap_or_else(|error| {
                panic!(
                    "{} cannot resolve its per-user app data folder ({error}); refusing to use the current directory",
                    self.label
                )
            })
    }

    /// The root handed to this process by [`Self::set`]; [`Self::uninjected`] until then.
    pub fn get(&self) -> PathBuf {
        self.injected
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_else(|| self.uninjected())
    }
}
