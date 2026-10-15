//! `ModelStore`: the handle apps pass around. Binds a models root (the shared
//! Orthic Labs store by default, an explicit directory in tests) and the app id
//! recorded in `refs/` to the trusted catalog. Generalised from CutRight's
//! `ModelStore`, which was Parakeet-only.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::download::{FailureSlot, LastFailure};
use crate::http::ModelHttp;
use crate::suite::catalog::{parakeet_spec, ModelSpec};
use crate::suite::grant::GrantError;
use crate::suite::install::{ensure_with_grant, ensure_with_grant_via, GrantConfig};
use crate::suite::manifest::VerifyMode;
use crate::suite::store::{
    ensure_ready_recorded, status_at, status_in, EnsureOptions, EntryState, ModelStatus, Progress,
};
use crate::suite::{paths, refs};

/// Why [`ModelStore::try_ensure_entitled`] failed. Each variant says which party failed, so an
/// app can show "not entitled", back off on a rate limit, or retry a transport failure.
/// `Display` gives the same text the string-returning [`ModelStore::ensure_entitled`] reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntitlementError {
    /// Refused before any network call: the model does not use an entitlement, or the token is
    /// empty once a leading `Bearer ` (any case) and surrounding whitespace are removed, or what
    /// remains is not ASCII or contains whitespace or control characters.
    InvalidRequest(String),
    /// The server answered HTTP 401 or 403 to this token: the app is not entitled to the model,
    /// or the token is not accepted.
    NotEntitled { status: u16, message: String },
    /// The server answered another non-success HTTP status, after the download's own retries for
    /// transient statuses (for example 404 when the route is not served, or 5xx).
    Refused { status: u16, message: String },
    /// HTTP 429: the same typed value the grant API returns. [`GrantError::retry_after`] gives
    /// the server's `Retry-After` delay when it sent one.
    RateLimited(GrantError),
    /// No HTTP response: connect, TLS, timeout, body read failure, or the outbound address policy
    /// refused the destination.
    Transport(String),
    /// A failure after the server answered or without a request: local verify, disk, hash
    /// mismatch, extraction or install.
    Install(String),
}

impl fmt::Display for EntitlementError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EntitlementError::InvalidRequest(message)
            | EntitlementError::Transport(message)
            | EntitlementError::Install(message)
            | EntitlementError::NotEntitled { message, .. }
            | EntitlementError::Refused { message, .. } => f.write_str(message),
            EntitlementError::RateLimited(error) => fmt::Display::fmt(error, f),
        }
    }
}

impl std::error::Error for EntitlementError {}

/// Normalise `token` to the bare token the crate sends: the crate adds the `Bearer ` scheme
/// itself. Surrounding whitespace is trimmed, one leading `Bearer ` (any case) is stripped, and the
/// result is trimmed again. It is refused only if it is then empty, not ASCII, or contains
/// whitespace or control characters.
fn normalise_raw_token(token: &str) -> Result<&str, EntitlementError> {
    let invalid = |why: &str| EntitlementError::InvalidRequest(format!("entitlement token {why}"));
    let trimmed = token.trim();
    // Split off the first word, so a value that trims down to just the scheme (`Bearer `) is
    // seen as the scheme with nothing after it, not as a token called `Bearer`.
    let (first, rest) = match trimmed.find(char::is_whitespace) {
        Some(at) => (&trimmed[..at], trimmed[at..].trim()),
        None => (trimmed, ""),
    };
    let bare = if first.eq_ignore_ascii_case("bearer") {
        rest
    } else {
        trimmed
    };
    if bare.is_empty() {
        return Err(invalid("is empty"));
    }
    if !bare.is_ascii() || bare.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(invalid(
            "must be ASCII without whitespace or control characters",
        ));
    }
    Ok(bare)
}

fn classify_entitlement_failure(message: String, last: Option<LastFailure>) -> EntitlementError {
    match last {
        Some(LastFailure::Status {
            status,
            retry_after,
        }) => match status {
            401 | 403 => EntitlementError::NotEntitled { status, message },
            429 => EntitlementError::RateLimited(GrantError::RateLimited {
                retry_after,
                message,
            }),
            _ => EntitlementError::Refused { status, message },
        },
        Some(LastFailure::Transport(_)) => EntitlementError::Transport(message),
        None => EntitlementError::Install(message),
    }
}

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
    crate::suite::paths::app_data_base()
        .map(|b| {
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

    /// Download an entitlement-gated add-on (verified, resumable). `bearer` is the full
    /// `Authorization` header value, for example `Bearer <token>`, and is sent unchanged. A bare
    /// token without the scheme is not corrected here, so a raw token produces a 401 from the
    /// server. Use [`ModelStore::try_ensure_entitled`] for a raw token and typed errors.
    /// Grants are never requested for such a model.
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

    /// As [`ModelStore::ensure_entitled`], with a raw entitlement `token` (no `Bearer ` prefix;
    /// the crate sends `Authorization: Bearer <token>`) and typed errors. One leading `Bearer `
    /// (any case) and surrounding whitespace are stripped first, so a pasted `Authorization` value
    /// works. A value that is then empty, or has whitespace or non-ASCII characters, is refused
    /// with [`EntitlementError::InvalidRequest`] before any network call. A ready model returns
    /// without a request, and the token is still checked first.
    pub fn try_ensure_entitled(
        &self,
        spec: &ModelSpec,
        token: &str,
        cancelled: &dyn Fn() -> bool,
        progress: impl FnMut(Progress),
    ) -> Result<ModelStatus, EntitlementError> {
        if !spec.requires_entitlement() {
            return Err(EntitlementError::InvalidRequest(format!(
                "{} does not use an entitlement bearer",
                spec.label
            )));
        }
        let bare = normalise_raw_token(token)?;
        let header = format!("Bearer {bare}");
        let client = self.http.clone().unwrap_or_default();
        let slot = FailureSlot::default();
        let result = ensure_ready_recorded(
            &client,
            &self.models_root,
            spec,
            &EnsureOptions {
                seed_dir: None,
                authorization: Some(&header),
                app_id: &self.app_id,
            },
            cancelled,
            progress,
            &slot,
        );
        result.map_err(|message| classify_entitlement_failure(message, slot.into_inner()))
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
