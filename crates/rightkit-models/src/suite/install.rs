//! Grant-gated download: ask the model worker for a device-scoped,
//! route-and-hash-bound grant, run the verified resumable `ensure_ready`, and
//! refresh the grant (expiry, or a 401/403 mid-flight) without losing the
//! resumable `.part`. Bytes reach the shared store only through `ensure_ready`
//! (resumable `.incoming`, SHA-256 pinned, atomic swap).

use std::path::{Path, PathBuf};

use crate::http::{http_client, Client, ModelHttp};
use crate::suite::catalog::{ModelSpec, SpecSource};
use crate::suite::grant::{
    acquire_grant_pinned, acquire_grant_with_client, ModelGrant, ModelGrantRequest,
};
use crate::suite::manifest::VerifyMode;
use crate::suite::store::{
    ensure_ready_cancellable, status_at, EnsureOptions, EntryState, ModelStatus, Progress,
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
    client: Option<Client>,
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
            client: None,
            acquisitions: 0,
        })
    }

    /// Send grant requests through `client` instead of one pinned to the worker
    /// host over HTTPS. The caller owns that client's address policy.
    pub fn with_client(mut self, client: Client) -> Self {
        self.client = Some(client);
        self
    }

    fn fetch(&mut self) -> Result<String, String> {
        let request = ModelGrantRequest {
            worker_base: &self.config.worker_base,
            route: &self.route,
            model: &self.config.model,
            platform: &self.config.platform,
            sha256: &self.sha256,
            device_id: &self.config.device_id,
        };
        let grant = match &self.client {
            Some(client) => acquire_grant_with_client(client, request)?,
            None => acquire_grant_pinned(request)?,
        };
        let bearer = grant.bearer();
        self.current = Some(grant);
        self.acquisitions += 1;
        Ok(bearer)
    }

    /// Current `Authorization` value, acquiring or renewing as needed. Requests
    /// are pinned to the worker host over HTTPS (see [`GrantSession::with_client`]).
    pub fn authorization_pinned(&mut self) -> Result<String, String> {
        match &self.current {
            Some(grant) if !grant.expires_within(REFRESH_SKEW) => Ok(grant.bearer()),
            _ => self.fetch(),
        }
    }

    /// Discard the cached grant and request a fresh one.
    pub fn refresh_pinned(&mut self) -> Result<String, String> {
        self.current = None;
        self.fetch()
    }

    /// Retired: `agent` is ignored; see [`GrantSession::authorization_pinned`].
    #[deprecated(since = "0.2.9", note = "use `authorization_pinned`")]
    pub fn authorization(&mut self, _agent: &ureq::Agent) -> Result<String, String> {
        self.authorization_pinned()
    }

    /// Retired: `agent` is ignored; see [`GrantSession::refresh_pinned`].
    #[deprecated(since = "0.2.9", note = "use `refresh_pinned`")]
    pub fn refresh(&mut self, _agent: &ureq::Agent) -> Result<String, String> {
        self.refresh_pinned()
    }
}

/// Stable random per-install device id for model grants, kept in the calling
/// app's own data folder (`<data dir>/<app_dir>/model-device-id`, never in the
/// shared store). The data dir is [`crate::suite::paths::app_data_base`], so a `test-mode`
/// build with a suite-root override writes under that root, never the real per-user folder.
pub fn device_id_get_or_create(app_dir: &str) -> Result<String, String> {
    let dir: PathBuf = crate::suite::paths::app_data_base()
        .ok_or("no app data dir")?
        .join(app_dir);
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
///
/// Downloads use the public-web [`http_client`]; grant requests are pinned to the
/// worker host over HTTPS. Use [`ensure_with_grant_via`] to supply the transport.
pub fn ensure_with_grant(
    root: &Path,
    spec: &ModelSpec,
    app_id: &str,
    grant: Option<&GrantConfig>,
    cancelled: &dyn Fn() -> bool,
    progress: impl FnMut(Progress),
) -> Result<ModelStatus, String> {
    ensure_with_grant_inner(None, root, spec, app_id, grant, cancelled, progress)
}

/// As [`ensure_with_grant`] through `client` for both the model download and the
/// grant requests. The caller owns its address policy, for example to reach a
/// private mirror or worker that the app lists explicitly.
pub fn ensure_with_grant_via(
    client: &ModelHttp,
    root: &Path,
    spec: &ModelSpec,
    app_id: &str,
    grant: Option<&GrantConfig>,
    cancelled: &dyn Fn() -> bool,
    progress: impl FnMut(Progress),
) -> Result<ModelStatus, String> {
    ensure_with_grant_inner(Some(client), root, spec, app_id, grant, cancelled, progress)
}

fn ensure_with_grant_inner(
    custom: Option<&ModelHttp>,
    root: &Path,
    spec: &ModelSpec,
    app_id: &str,
    grant: Option<&GrantConfig>,
    cancelled: &dyn Fn() -> bool,
    mut progress: impl FnMut(Progress),
) -> Result<ModelStatus, String> {
    let client = custom.cloned().unwrap_or_else(http_client);
    let missing = status_at(root, spec, VerifyMode::Size).state != EntryState::Ready;
    if spec.requires_entitlement() && missing {
        // No grant exists for an entitlement-gated model; asking the worker for one would
        // only fail. The caller holds the signed entitlement and supplies it as the bearer.
        return Err(format!(
            "{} is an entitlement-gated add-on: download it with the app's signed Pro entitlement as the bearer (ensure_entitled)",
            spec.label
        ));
    }
    let needs_grant = spec.requires_grant() && missing;
    let mut session = match (needs_grant, grant) {
        (true, Some(config)) => {
            let session = GrantSession::for_spec(config, spec)?;
            Some(match custom {
                // `client_for` picks the development loopback client when the worker is the
                // one allowed host and port (feature `dev-worker-override`).
                Some(client) => session.with_client(client.client_for(&config.worker_base).clone()),
                None => session,
            })
        }
        (true, None) => return Err("a device id is required for model download".into()),
        (false, _) => None,
    };
    let mut authorization = match session.as_mut() {
        Some(session) => Some(session.authorization_pinned()?),
        None => None,
    };
    let mut attempts = 0;
    loop {
        let result = ensure_ready_cancellable(
            &client,
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
                authorization = Some(session.refresh_pinned()?);
            }
            (other, _) => return other,
        }
    }
}
