//! `ModelStore`: the handle apps pass around. Binds a models root (the shared
//! Orthic Labs store by default, an explicit directory in tests) and the app id
//! recorded in `refs/` to the trusted catalog. Generalised from CutRight's
//! `ModelStore`, which was Parakeet-only.

use std::path::{Path, PathBuf};

use crate::http::ModelHttp;
use crate::suite::catalog::{parakeet_spec, ModelSpec};
use crate::suite::install::{ensure_with_grant, ensure_with_grant_via, GrantConfig};
use crate::suite::manifest::VerifyMode;
use crate::suite::store::{status_at, status_in, EntryState, ModelStatus, Progress};
use crate::suite::{paths, refs};

/// A model that passed the trusted-identity check and is ready to load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModel {
    /// Entry directory holding the model files.
    pub dir: PathBuf,
    /// Trusted variant id (the entry directory name).
    pub id: String,
    /// Trusted revision (immutable archive SHA-256).
    pub revision: String,
}

#[derive(Clone)]
pub struct ModelStore {
    models_root: PathBuf,
    app_id: String,
    http: Option<ModelHttp>,
}

impl std::fmt::Debug for ModelStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelStore")
            .field("models_root", &self.models_root)
            .field("app_id", &self.app_id)
            .field("custom_http", &self.http.is_some())
            .finish()
    }
}

/// Parakeet copies installed by sibling Orthic apps in their own stores.
pub fn sibling_parakeet_dirs() -> Vec<PathBuf> {
    let base = if cfg!(windows) {
        dirs::data_local_dir()
    } else {
        dirs::data_dir()
    };
    base.map(|b| {
        ["HeardRightNext", "HeardRight"]
            .iter()
            .map(|app| b.join(app).join("models").join("parakeet-tdt-v3"))
            .collect()
    })
    .unwrap_or_default()
}

impl ModelStore {
    /// The machine-wide shared store (`ORTHIC_SHARED_DIR` overrides the root).
    pub fn shared(app_id: impl Into<String>) -> Self {
        Self {
            models_root: paths::models_root(),
            app_id: app_id.into(),
            http: None,
        }
    }

    /// A store rooted at an explicit `models` directory.
    pub fn at_models_root(root: impl Into<PathBuf>, app_id: impl Into<String>) -> Self {
        Self {
            models_root: root.into(),
            app_id: app_id.into(),
            http: None,
        }
    }

    /// Send this store's downloads and grant requests through `client` instead of
    /// the default vetted ones (public-web downloads, grants pinned to the worker
    /// host over HTTPS). The caller owns that client's address policy.
    pub fn with_http_client(mut self, client: impl Into<ModelHttp>) -> Self {
        self.http = Some(client.into());
        self
    }

    pub fn models_root(&self) -> &Path {
        &self.models_root
    }

    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    pub fn parakeet_spec(&self) -> ModelSpec {
        parakeet_spec()
    }

    /// Live readiness (never cached): identity matches the trusted catalog
    /// manifest AND every pinned file is present with the pinned size. `Full`
    /// additionally re-hashes every file.
    pub fn status(&self, spec: &ModelSpec, mode: VerifyMode) -> ModelStatus {
        status_at(&self.models_root, spec, mode)
    }

    pub fn parakeet_status(&self, mode: VerifyMode) -> ModelStatus {
        self.status(&parakeet_spec(), mode)
    }

    /// Resolve `spec` for loading. `siblings` are other apps' entry dirs that
    /// may hold the identical trusted variant; each is accepted only if it
    /// verifies against the same trusted manifest, and only when this store is
    /// the machine-wide one. On success registers this app's ref in
    /// `refs/<app>.json` (atomic, registry-locked) so GC cannot delete the model
    /// under a running app.
    pub fn resolve(&self, spec: &ModelSpec, siblings: &[PathBuf]) -> Result<ResolvedModel, String> {
        let status = status_at(&self.models_root, spec, VerifyMode::Size);
        if status.state == EntryState::Missing && self.models_root == paths::models_root() {
            if let Some(found) = siblings
                .iter()
                .map(|d| status_in(d.clone(), spec, VerifyMode::Size))
                .find(|s| s.state == EntryState::Ready)
            {
                return Ok(ResolvedModel {
                    dir: found.dir,
                    id: spec.id.to_string(),
                    revision: spec.version.to_string(),
                });
            }
        }
        match status.state {
            EntryState::Ready => {}
            EntryState::Missing => {
                return Err(format!(
                    "model {} is not installed in {}; install it with ensure",
                    spec.id,
                    self.models_root.display()
                ))
            }
            EntryState::InProgress => {
                return Err(format!(
                    "model {} is being installed by another process",
                    spec.id
                ))
            }
            EntryState::Partial => {
                return Err(format!(
                    "model {} does not match trusted revision {} (files to repair: {})",
                    spec.id,
                    spec.version,
                    status.needed.join(", ")
                ))
            }
        }
        refs::register(
            &paths::model_refs_dir_at(&self.models_root),
            &self.app_id,
            spec.id,
        )?;
        Ok(ResolvedModel {
            dir: status.dir,
            id: spec.id.to_string(),
            revision: spec.version.to_string(),
        })
    }

    pub fn resolve_parakeet(&self) -> Result<ResolvedModel, String> {
        self.resolve(&parakeet_spec(), &sibling_parakeet_dirs())
    }

    /// Download an entitlement-gated add-on (verified, resumable) with `bearer`, the app's
    /// signed Pro entitlement. Grants are never requested for such a model.
    pub fn ensure_entitled(
        &self,
        spec: &ModelSpec,
        bearer: &str,
        cancelled: &dyn Fn() -> bool,
        progress: impl FnMut(Progress),
    ) -> Result<ModelStatus, String> {
        if !spec.requires_entitlement() {
            return Err(format!("{} does not use an entitlement bearer", spec.label));
        }
        let client = self.http.clone().unwrap_or_default();
        crate::suite::store::ensure_ready_cancellable(
            &client,
            &self.models_root,
            spec,
            &crate::suite::store::EnsureOptions {
                seed_dir: None,
                authorization: Some(bearer),
                app_id: &self.app_id,
            },
            cancelled,
            progress,
        )
    }

    /// Download (verified, resumable, grant-refreshing) and register `spec` if
    /// the store has no verified copy.
    pub fn ensure(
        &self,
        spec: &ModelSpec,
        grant: Option<&GrantConfig>,
        cancelled: &dyn Fn() -> bool,
        progress: impl FnMut(Progress),
    ) -> Result<ModelStatus, String> {
        match &self.http {
            Some(client) => ensure_with_grant_via(
                client,
                &self.models_root,
                spec,
                &self.app_id,
                grant,
                cancelled,
                progress,
            ),
            None => ensure_with_grant(
                &self.models_root,
                spec,
                &self.app_id,
                grant,
                cancelled,
                progress,
            ),
        }
    }
}
