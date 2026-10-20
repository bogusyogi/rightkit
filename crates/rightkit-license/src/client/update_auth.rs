//! Update authorization (feature `update-auth`).
//!
//! The native provider the G1 updater adapter binds to. For each outgoing request the adapter
//! calls [`UpdateAuthProvider::authorize`] with the update's [`UpdateBinding`] and the exact URL
//! about to be requested, and gets back either the headers for THAT request or a refusal.
//!
//! - **One lock acquisition per request.** Inside a single hold of the licence operation lock
//!   (a bounded 250 ms try, `Busy` on timeout) it reads the record (a leftover marker is
//!   reconciled first, `Unresolved` when it cannot be), verifies the revision the update began
//!   with is still the stored one, that the entitlement is valid, unexpired and not
//!   tombstoned, and only then builds the headers. Nothing is cached.
//! - **Hosts are fixed in config**, separately for the metadata check and for artifacts, and the
//!   artifact path is checked against a normalized prefix before anything is sent.
//! - **Metadata** (the check) carries the activation token; a **private artifact** carries the
//!   signed token; the **public** route (a patch) carries nothing at all. The route alone is not
//!   the entitlement: a private request is refused unless the licence is valid right now.
//! - [`AuthorizedHeaders`] is not `Clone`, not `Serialize`, has a redacted `Debug`, zeroes its
//!   values on drop and stops yielding them after a short time-to-live.
//!
//! Redirects, the byte cap, cancellation and the final signature verification belong to the
//! adapter; this crate only decides what a request may carry.

use std::fmt;
use std::time::{Duration, Instant};

use url::Url;
use zeroize::Zeroizing;

use super::ops::Core;
use super::types::valid_host;
use super::view::LicenseError;
use super::LicenseClient;
use crate::LicenseRequirement;

/// How long [`AuthorizedHeaders`] yield their values. Long enough for one request.
pub const DEFAULT_HEADER_TTL: Duration = Duration::from_secs(30);

/// The licence revision an update began with. Opaque: equal to the stored revision or not. The
/// value is never shown (its `Debug` is fixed text) and never leaves native code.
#[derive(Clone, PartialEq, Eq)]
pub struct LicenseRevision(Option<String>);

impl fmt::Debug for LicenseRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LicenseRevision(..)")
    }
}

/// Which route an update takes. `Public` (a patch) needs no licence; `Private` (an update) is
/// entitlement-gated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateRoute {
    Public,
    Private,
}

/// The verified bytes a download produced, carried by the binding after download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedIdentity {
    /// Lowercase hex SHA-256.
    pub sha256: String,
    pub length: u64,
}

/// What one update operation is bound to. Captured when the check begins and passed unchanged to
/// every later request; it is never recaptured against a newer licence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateBinding {
    pub revision: LicenseRevision,
    pub route: UpdateRoute,
    /// The immutable release this binding is for (empty for the metadata check itself).
    pub release_id: String,
    /// Set after download. Carried for the adapter; not interpreted here.
    pub staged: Option<StagedIdentity>,
}

/// Why a request may not carry licence headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UpdateAuthError {
    /// The lock is held.
    Busy,
    /// A marker or an unfinished write stands.
    Unresolved,
    NotLicensed,
    Expired,
    Revoked,
    TierIneligible,
    /// The licence changed since this update began (a refresh, a deactivation, a new activation).
    RevisionChanged,
    /// The URL is not https, has credentials, or its host is in neither allowlist.
    HostNotAllowed,
    /// The artifact path is outside the configured prefix or is not normalized.
    PathNotAllowed,
    /// Licensing is disabled: only the public route can be authorized.
    Disabled,
    /// The licence record could not be read.
    Store,
    /// The headers outlived their time-to-live.
    HeadersExpired,
    /// The configuration is invalid.
    BadConfig,
}

impl UpdateAuthError {
    /// The wire `UpdateRefusal` value for this refusal.
    pub fn refusal(self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::Unresolved | Self::Store => "unresolved",
            Self::Expired => "expired",
            Self::Revoked => "revoked",
            Self::TierIneligible => "tierIneligible",
            Self::Disabled => "disabledNoPublicPatch",
            Self::NotLicensed
            | Self::RevisionChanged
            | Self::HostNotAllowed
            | Self::PathNotAllowed
            | Self::HeadersExpired
            | Self::BadConfig => "notLicensed",
        }
    }
}

impl fmt::Display for UpdateAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Busy => "update authorization is busy",
            Self::Unresolved => "update authorization is unresolved",
            Self::NotLicensed => "no licence authorizes this update",
            Self::Expired => "the licence has expired",
            Self::Revoked => "the licence has been revoked",
            Self::TierIneligible => "the licence tier does not include this update",
            Self::RevisionChanged => "the licence changed since this update began",
            Self::HostNotAllowed => "the update host is not allowed",
            Self::PathNotAllowed => "the update path is not allowed",
            Self::Disabled => "licensing is disabled",
            Self::Store => "the licence could not be read",
            Self::HeadersExpired => "the update headers expired",
            Self::BadConfig => "the update authorization configuration is invalid",
        })
    }
}

impl std::error::Error for UpdateAuthError {}

impl From<LicenseError> for UpdateAuthError {
    fn from(error: LicenseError) -> Self {
        match error {
            LicenseError::Busy => Self::Busy,
            LicenseError::Unresolved => Self::Unresolved,
            LicenseError::Unconfigured => Self::Disabled,
            _ => Self::Store,
        }
    }
}

/// Fixed configuration of an update authorizer.
#[derive(Debug, Clone)]
pub struct UpdateAuthConfig {
    check_hosts: Vec<String>,
    artifact_hosts: Vec<String>,
    /// Path segments of the artifact prefix, already normalized.
    artifact_prefix: Vec<String>,
    header_ttl: Duration,
    requirement: Option<LicenseRequirement>,
}

impl UpdateAuthConfig {
    /// `check_hosts` serve the update metadata, `artifact_hosts` the artifacts; both are exact
    /// lowercase DNS names (no scheme, port or path) and only `https` is ever authorized.
    /// `artifact_path_prefix` is the app's path on the artifact host, such as `/downloads/heardright`.
    pub fn new(
        check_hosts: Vec<String>,
        artifact_hosts: Vec<String>,
        artifact_path_prefix: &str,
    ) -> Result<Self, UpdateAuthError> {
        if check_hosts.is_empty()
            || artifact_hosts.is_empty()
            || !check_hosts
                .iter()
                .chain(&artifact_hosts)
                .all(|h| valid_host(h))
        {
            return Err(UpdateAuthError::BadConfig);
        }
        let prefix = path_segments(artifact_path_prefix).ok_or(UpdateAuthError::BadConfig)?;
        if prefix.is_empty() {
            return Err(UpdateAuthError::BadConfig);
        }
        Ok(Self {
            check_hosts,
            artifact_hosts,
            artifact_prefix: prefix,
            header_ttl: DEFAULT_HEADER_TTL,
            requirement: None,
        })
    }

    /// Also require this of the entitlement ([`UpdateAuthError::TierIneligible`] otherwise).
    pub fn with_requirement(mut self, requirement: LicenseRequirement) -> Self {
        self.requirement = Some(requirement);
        self
    }

    /// How long the headers it hands out stay usable (default [`DEFAULT_HEADER_TTL`]).
    pub fn with_header_ttl(mut self, ttl: Duration) -> Self {
        self.header_ttl = ttl;
        self
    }
}

/// What a request is for, decided by its host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Purpose {
    Metadata,
    Artifact,
}

/// Normalized path segments of an absolute path: no empty, `.`/`..` (also percent-encoded),
/// backslash or encoded-separator segment. `None` when the path is not that.
fn path_segments(path: &str) -> Option<Vec<String>> {
    let rest = path.strip_prefix('/')?;
    let mut out = Vec::new();
    for segment in rest.split('/') {
        if segment.is_empty() {
            // A trailing slash is allowed; an empty segment in the middle is not.
            continue;
        }
        let lower = segment.to_ascii_lowercase();
        let bad = lower == "."
            || lower == ".."
            || lower == "%2e"
            || lower == "%2e%2e"
            || lower == ".%2e"
            || lower == "%2e."
            || lower.contains('\\')
            || lower.contains("%2f")
            || lower.contains("%5c");
        if bad {
            return None;
        }
        out.push(lower);
    }
    if rest.contains("//") {
        return None;
    }
    Some(out)
}

impl UpdateAuthConfig {
    /// Decide what `url` is for, or refuse it. Pure: no lock, no licence.
    fn purpose(&self, url: &Url, _route: UpdateRoute) -> Result<Purpose, UpdateAuthError> {
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
        {
            return Err(UpdateAuthError::HostNotAllowed);
        }
        let host = url
            .host_str()
            .map(str::to_ascii_lowercase)
            .ok_or(UpdateAuthError::HostNotAllowed)?;
        // A host in both lists is read as the more restrictive purpose: a check.
        if self.check_hosts.contains(&host) {
            return Ok(Purpose::Metadata);
        }
        if !self.artifact_hosts.contains(&host) {
            return Err(UpdateAuthError::HostNotAllowed);
        }
        // Artifacts, public or private, must sit under the app's normalized path.
        let segments = path_segments(url.path()).ok_or(UpdateAuthError::PathNotAllowed)?;
        if segments.len() < self.artifact_prefix.len()
            || segments[..self.artifact_prefix.len()] != self.artifact_prefix[..]
        {
            return Err(UpdateAuthError::PathNotAllowed);
        }
        Ok(Purpose::Artifact)
    }
}

/// Headers for one request. Not `Clone`, not `Serialize`; values are zeroed on drop and only
/// yielded while fresh.
pub struct AuthorizedHeaders {
    entries: Vec<(&'static str, Zeroizing<String>)>,
    issued: Instant,
    ttl: Duration,
}

impl fmt::Debug for AuthorizedHeaders {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "AuthorizedHeaders({} header(s), <redacted>)",
            self.entries.len()
        )
    }
}

impl AuthorizedHeaders {
    fn none(ttl: Duration) -> Self {
        Self {
            entries: Vec::new(),
            issued: Instant::now(),
            ttl,
        }
    }

    /// No header at all (the public route).
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Header names, never values.
    pub fn names(&self) -> Vec<&'static str> {
        self.entries.iter().map(|(name, _)| *name).collect()
    }

    fn fresh(&self) -> Result<(), UpdateAuthError> {
        if self.issued.elapsed() > self.ttl {
            Err(UpdateAuthError::HeadersExpired)
        } else {
            Ok(())
        }
    }

    /// Call `set(name, value)` for each header, for a transport that takes headers one by one
    /// (the updater plugin's `header()`). The values are only valid inside `set`; mark them
    /// sensitive there and do not log them. `HeadersExpired` after the time-to-live.
    pub fn each(&self, mut set: impl FnMut(&str, &str)) -> Result<(), UpdateAuthError> {
        self.fresh()?;
        for (name, value) in &self.entries {
            set(name, value.as_str());
        }
        Ok(())
    }

    /// Put the headers on `request` as secret headers (never shown in its errors or `Debug`),
    /// consuming `self`.
    pub fn apply(
        self,
        request: rightkit_http::Request,
    ) -> Result<rightkit_http::Request, UpdateAuthError> {
        self.fresh()?;
        let mut request = request;
        for (name, value) in &self.entries {
            request = request.secret_header(*name, value.as_str());
        }
        Ok(request)
    }
}

/// The authorizer the G1 updater adapter consumes.
#[derive(Clone)]
pub struct UpdateAuthProvider {
    core: Option<std::sync::Arc<Core>>,
    config: UpdateAuthConfig,
}

impl fmt::Debug for UpdateAuthProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UpdateAuthProvider")
            .field("configured", &self.core.is_some())
            .finish_non_exhaustive()
    }
}

impl UpdateAuthProvider {
    /// A provider over `client`. With a disabled client only the public route can be
    /// authorized (Disabled + PublicPatch).
    pub fn new(client: &LicenseClient, config: UpdateAuthConfig) -> Self {
        Self {
            core: client.core().cloned(),
            config,
        }
    }

    /// The revision to bind a new update to, read inside one lock hold. Refused unless a valid
    /// licence is stored now (a public patch needs no binding revision: use
    /// [`LicenseRevision::none`]).
    pub fn revision(&self) -> Result<LicenseRevision, UpdateAuthError> {
        let core = self.core.as_ref().ok_or(UpdateAuthError::Disabled)?;
        core.authorized(|evaluation| {
            if evaluation.grants && evaluation.entitlement.is_some() {
                Ok(LicenseRevision(evaluation.revision.clone()))
            } else {
                Err(refusal_of(evaluation.view.status))
            }
        })?
    }

    /// Revalidate `binding` without any request (after a check, after a download, before
    /// install admission): the revision is unchanged and the entitlement valid, unexpired and
    /// not tombstoned. A public binding is always valid. One lock acquisition.
    pub fn validate(&self, binding: &UpdateBinding) -> Result<(), UpdateAuthError> {
        if binding.route == UpdateRoute::Public {
            return Ok(());
        }
        self.guarded(binding, |_| ())
    }

    /// The headers `url` may carry for `binding`, decided inside one lock acquisition.
    pub fn authorize(
        &self,
        binding: &UpdateBinding,
        url: &Url,
    ) -> Result<AuthorizedHeaders, UpdateAuthError> {
        let purpose = self.config.purpose(url, binding.route)?;
        let ttl = self.config.header_ttl;
        if binding.route == UpdateRoute::Public {
            // A patch is public: zero licence headers, whatever is stored.
            return Ok(AuthorizedHeaders::none(ttl));
        }
        let token = self.guarded(binding, |evaluation| {
            let record = evaluation.record.as_ref()?;
            let token = match purpose {
                Purpose::Metadata => record.activation_token(),
                Purpose::Artifact => record.signed_token(),
            }?;
            Some(Zeroizing::new(format!("Bearer {token}")))
        })?;
        let value = token.ok_or(UpdateAuthError::NotLicensed)?;
        Ok(AuthorizedHeaders {
            entries: vec![("Authorization", value)],
            issued: Instant::now(),
            ttl,
        })
    }

    /// Hold the lock once, check the binding against what is stored, and build `f`'s value.
    fn guarded<R>(
        &self,
        binding: &UpdateBinding,
        f: impl FnOnce(&super::ops::Evaluation) -> R,
    ) -> Result<R, UpdateAuthError> {
        let core = self.core.as_ref().ok_or(UpdateAuthError::Disabled)?;
        core.authorized(|evaluation| {
            let entitlement = match (&evaluation.entitlement, evaluation.grants) {
                (Some(entitlement), true) => entitlement,
                _ => return Err(refusal_of(evaluation.view.status)),
            };
            if evaluation.revision != binding.revision.0 {
                return Err(UpdateAuthError::RevisionChanged);
            }
            if let Some(requirement) = &self.config.requirement {
                if !entitlement.satisfies(requirement) {
                    return Err(UpdateAuthError::TierIneligible);
                }
            }
            Ok(f(evaluation))
        })?
    }
}

impl LicenseRevision {
    /// The binding revision of a public patch, which has no licence to bind to.
    pub fn none() -> Self {
        Self(None)
    }
}

fn refusal_of(status: super::view::ViewStatus) -> UpdateAuthError {
    use super::view::ViewStatus;
    match status {
        ViewStatus::Expired => UpdateAuthError::Expired,
        ViewStatus::Revoked => UpdateAuthError::Revoked,
        _ => UpdateAuthError::NotLicensed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{DeviceIdError, DeviceIdSource, LicenseSetup};
    use std::sync::Arc;

    struct NoDevice;
    impl DeviceIdSource for NoDevice {
        fn device_id(&self) -> Result<String, DeviceIdError> {
            Err(DeviceIdError)
        }
        fn device_id_hash(&self) -> Result<String, DeviceIdError> {
            Err(DeviceIdError)
        }
    }

    fn config() -> UpdateAuthConfig {
        UpdateAuthConfig::new(
            vec!["updates.example.com".into()],
            vec!["dl.example.com".into()],
            "/downloads/app",
        )
        .unwrap()
    }

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    #[test]
    fn hosts_decide_the_purpose_and_everything_else_is_refused() {
        let c = config();
        let route = UpdateRoute::Private;
        assert_eq!(
            c.purpose(&url("https://updates.example.com/latest.json"), route),
            Ok(Purpose::Metadata)
        );
        assert_eq!(
            c.purpose(
                &url("https://dl.example.com/downloads/app/1.2/app.zip"),
                route
            ),
            Ok(Purpose::Artifact)
        );
        for bad in [
            "https://evil.example.com/downloads/app/x",
            "http://dl.example.com/downloads/app/x",
            "https://user@dl.example.com/downloads/app/x",
            "https://user:pw@updates.example.com/latest.json",
            "https://dl.example.com:8443/downloads/app/x",
            "https://dl.example.com.evil.com/downloads/app/x",
            "https://UPDATES.example.com.evil.com/x",
        ] {
            assert_eq!(
                c.purpose(&url(bad), route),
                Err(UpdateAuthError::HostNotAllowed),
                "{bad}"
            );
        }
        // Host case is folded by the URL parser, so the exact host still matches.
        assert_eq!(
            c.purpose(&url("https://DL.example.com/downloads/app/x"), route),
            Ok(Purpose::Artifact)
        );
    }

    #[test]
    fn artifact_paths_must_sit_under_the_normalized_prefix() {
        let c = config();
        let route = UpdateRoute::Private;
        for good in [
            "https://dl.example.com/downloads/app",
            "https://dl.example.com/downloads/app/",
            "https://dl.example.com/downloads/app/1.2/app.zip",
        ] {
            assert_eq!(
                c.purpose(&url(good), route),
                Ok(Purpose::Artifact),
                "{good}"
            );
        }
        for bad in [
            "https://dl.example.com/downloads/other/x",
            "https://dl.example.com/downloads/apple/x",
            "https://dl.example.com/downloads/app/../other/x",
            "https://dl.example.com/downloads/app/%2e%2e/other/x",
            "https://dl.example.com/downloads//app/x",
            "https://dl.example.com/downloads/app%2fx",
            "https://dl.example.com/downloads/app/a%5cb",
            "https://dl.example.com/",
        ] {
            assert_eq!(
                c.purpose(&url(bad), route),
                Err(UpdateAuthError::PathNotAllowed),
                "{bad}"
            );
        }
    }

    #[test]
    fn configuration_is_validated() {
        let ok = |checks: &[&str], artifacts: &[&str], prefix: &str| {
            UpdateAuthConfig::new(
                checks.iter().map(|s| s.to_string()).collect(),
                artifacts.iter().map(|s| s.to_string()).collect(),
                prefix,
            )
        };
        assert!(ok(&["a.example.com"], &["b.example.com"], "/x").is_ok());
        assert!(ok(&[], &["b.example.com"], "/x").is_err());
        assert!(ok(&["a.example.com"], &[], "/x").is_err());
        assert!(ok(&["https://a.example.com"], &["b.example.com"], "/x").is_err());
        assert!(ok(&["A.example.com"], &["b.example.com"], "/x").is_err());
        assert!(ok(&["a.example.com"], &["b.example.com"], "x").is_err());
        assert!(ok(&["a.example.com"], &["b.example.com"], "/").is_err());
        assert!(ok(&["a.example.com"], &["b.example.com"], "/a/../b").is_err());
    }

    #[test]
    fn the_public_route_carries_no_headers_even_without_licensing() {
        let client = LicenseClient::new(LicenseSetup::Disabled, Arc::new(NoDevice)).unwrap();
        let provider = UpdateAuthProvider::new(&client, config());
        let binding = UpdateBinding {
            revision: LicenseRevision::none(),
            route: UpdateRoute::Public,
            release_id: "1.2.3".into(),
            staged: None,
        };
        let headers = provider
            .authorize(
                &binding,
                &url("https://dl.example.com/downloads/app/1.2.3/app.zip"),
            )
            .unwrap();
        assert!(headers.is_empty());
        assert!(headers.names().is_empty());
        // The host allowlist still applies to a public request.
        assert_eq!(
            provider
                .authorize(&binding, &url("https://evil.example.com/downloads/app/x"))
                .unwrap_err(),
            UpdateAuthError::HostNotAllowed
        );
        assert!(provider.validate(&binding).is_ok());
    }

    #[test]
    fn a_private_request_without_licensing_is_refused() {
        let client = LicenseClient::new(LicenseSetup::Disabled, Arc::new(NoDevice)).unwrap();
        let provider = UpdateAuthProvider::new(&client, config());
        let binding = UpdateBinding {
            revision: LicenseRevision::none(),
            route: UpdateRoute::Private,
            release_id: String::new(),
            staged: None,
        };
        let error = provider
            .authorize(&binding, &url("https://updates.example.com/latest.json"))
            .unwrap_err();
        assert_eq!(error, UpdateAuthError::Disabled);
        assert_eq!(error.refusal(), "disabledNoPublicPatch");
        assert_eq!(provider.revision().unwrap_err(), UpdateAuthError::Disabled);
    }

    #[test]
    fn headers_are_redacted_short_lived_and_secret_on_the_request() {
        let live = AuthorizedHeaders {
            entries: vec![(
                "Authorization",
                Zeroizing::new("Bearer SENTINEL_SIGNED".into()),
            )],
            issued: Instant::now(),
            ttl: Duration::from_secs(30),
        };
        assert!(!format!("{live:?}").contains("SENTINEL"));
        assert_eq!(live.names(), vec!["Authorization"]);
        let mut seen = Vec::new();
        live.each(|name, value| seen.push((name.to_string(), value.to_string())))
            .unwrap();
        assert_eq!(seen[0].1, "Bearer SENTINEL_SIGNED");
        let request = live
            .apply(rightkit_http::Request::get("https://dl.example.com/x"))
            .unwrap();
        assert!(request
            .headers
            .iter()
            .any(|(k, v)| k == "Authorization" && v == "Bearer SENTINEL_SIGNED"));
        // Secret headers never print.
        assert!(!format!("{request:?}").contains("SENTINEL"));

        let stale = AuthorizedHeaders {
            entries: vec![("Authorization", Zeroizing::new("Bearer X".into()))],
            issued: Instant::now() - Duration::from_secs(5),
            ttl: Duration::from_secs(1),
        };
        assert_eq!(
            stale.each(|_, _| ()).unwrap_err(),
            UpdateAuthError::HeadersExpired
        );
        assert!(stale
            .apply(rightkit_http::Request::get("https://dl.example.com/x"))
            .is_err());
    }

    #[test]
    fn refusals_map_to_the_wire_names() {
        assert_eq!(UpdateAuthError::Busy.refusal(), "busy");
        assert_eq!(UpdateAuthError::Unresolved.refusal(), "unresolved");
        assert_eq!(UpdateAuthError::Expired.refusal(), "expired");
        assert_eq!(UpdateAuthError::Revoked.refusal(), "revoked");
        assert_eq!(UpdateAuthError::TierIneligible.refusal(), "tierIneligible");
        assert_eq!(UpdateAuthError::NotLicensed.refusal(), "notLicensed");
        assert_eq!(
            UpdateAuthError::from(LicenseError::Busy),
            UpdateAuthError::Busy
        );
    }
}
