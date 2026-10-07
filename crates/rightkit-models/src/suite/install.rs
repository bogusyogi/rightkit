//! Grant-gated download: ask the model worker for a device-scoped,
//! route-and-hash-bound grant, run the verified resumable `ensure_ready`, and
//! refresh the grant (expiry, or a 401/403 mid-flight) without losing the
//! resumable `.part`. Bytes reach the shared store only through `ensure_ready`
//! (resumable `.incoming`, SHA-256 pinned, atomic swap).

use std::path::{Path, PathBuf};

use crate::suite::catalog::{ModelSpec, SpecSource};
use crate::suite::grant::{acquire_grant, ModelGrant, ModelGrantRequest};
use crate::suite::manifest::VerifyMode;
use crate::suite::store::{
    ensure_ready_cancellable, http_agent, status_at, EnsureOptions, EntryState, ModelStatus,
    Progress,
};

/// Seconds before expiry at which a cached grant is replaced.
const REFRESH_SKEW: u64 = 60;

/// Where and for whom grants are requested.
#[derive(Debug, Clone)]
pub struct GrantConfig {
    /// Worker origin (route prefix is stripped from the spec's archive URL).
    pub worker_base: String,
    /// Worker model name (`parakeet-tdt`).
    pub model: String,
    /// `macos` / `windows`.
    pub platform: String,
    /// Stable per-install device id (see [`device_id_get_or_create`]).
    pub device_id: String,
}

/// A grant that refreshes itself: handed out until it is about to expire, then
/// re-acquired; [`GrantSession::refresh`] forces a new one after a rejection.
pub struct GrantSession<'a> {
    pub config: &'a GrantConfig,
    route: String,
    sha256: String,
    current: Option<ModelGrant>,
    pub acquisitions: u32,
}

impl<'a> GrantSession<'a> {
    /// Derive route and pinned archive hash from `spec`'s gated archive source.
    pub fn for_spec(config: &'a GrantConfig, spec: &ModelSpec) -> Result<Self, String> {
        let SpecSource::Archive {
            url: Some(url),
            sha256,
            ..
        } = &spec.source
        else {
            return Err(format!("{} has no gated archive route", spec.label));
        };
        let route = url
            .strip_prefix(config.worker_base.trim_end_matches('/'))
            .ok_or("model archive is outside configured worker")?
            .to_string();
        let sha256 = sha256
            .ok_or("model archive has no pinned SHA-256")?
            .to_string();
        Ok(Self {
            config,
            route,
            sha256,
            current: None,
            acquisitions: 0,
        })
    }

    fn fetch(&mut self, agent: &ureq::Agent) -> Result<String, String> {
        let grant = acquire_grant(
            agent,
            ModelGrantRequest {
                worker_base: &self.config.worker_base,
                route: &self.route,
                model: &self.config.model,
                platform: &self.config.platform,
                sha256: &self.sha256,
                device_id: &self.config.device_id,
            },
        )?;
        let bearer = grant.bearer();
        self.current = Some(grant);
        self.acquisitions += 1;
        Ok(bearer)
    }

    /// Current `Authorization` value, acquiring or renewing as needed.
    pub fn authorization(&mut self, agent: &ureq::Agent) -> Result<String, String> {
        match &self.current {
            Some(grant) if !grant.expires_within(REFRESH_SKEW) => Ok(grant.bearer()),
            _ => self.fetch(agent),
        }
    }

    /// Discard the cached grant and request a fresh one.
    pub fn refresh(&mut self, agent: &ureq::Agent) -> Result<String, String> {
        self.current = None;
        self.fetch(agent)
    }
}

/// Stable random per-install device id for model grants, kept in the calling
/// app's own data folder (`<data dir>/<app_dir>/model-device-id`, never in the
/// shared store).
pub fn device_id_get_or_create(app_dir: &str) -> Result<String, String> {
    let dir: PathBuf = dirs::data_dir().ok_or("no app data dir")?.join(app_dir);
    device_id_in(&dir)
}

/// As [`device_id_get_or_create`] under an explicit directory.
pub fn device_id_in(dir: &Path) -> Result<String, String> {
    let path = dir.join("model-device-id");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let existing = existing.trim().to_string();
        if existing.len() >= 16 {
            return Ok(existing);
        }
    }
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| format!("random device id: {e}"))?;
    let id: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    std::fs::write(&path, &id).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(id)
}

/// Make `spec` ready in the store at `root` (downloading only when there is no
/// verified copy) and register `app_id`'s ref. For a gated archive, a grant is
/// acquired up front and refreshed once if the worker rejects it.
pub fn ensure_with_grant(
    root: &Path,
    spec: &ModelSpec,
    app_id: &str,
    grant: Option<&GrantConfig>,
    cancelled: &dyn Fn() -> bool,
    mut progress: impl FnMut(Progress),
) -> Result<ModelStatus, String> {
    let agent = http_agent();
    let needs_grant =
        spec.requires_auth() && status_at(root, spec, VerifyMode::Size).state != EntryState::Ready;
    let mut session = match (needs_grant, grant) {
        (true, Some(config)) => Some(GrantSession::for_spec(config, spec)?),
        (true, None) => return Err("a device id is required for model download".into()),
        (false, _) => None,
    };
    let mut authorization = match session.as_mut() {
        Some(session) => Some(session.authorization(&agent)?),
        None => None,
    };
    let mut attempts = 0;
    loop {
        let result = ensure_ready_cancellable(
            &agent,
            root,
            spec,
            &EnsureOptions {
                seed_dir: None,
                authorization: authorization.as_deref(),
                app_id,
            },
            cancelled,
            &mut progress,
        );
        match (result, session.as_mut()) {
            (Err(error), Some(session))
                if attempts == 0 && error.contains("download authorization rejected") =>
            {
                attempts += 1;
                authorization = Some(session.refresh(&agent)?);
            }
            (other, _) => return other,
        }
    }
}
