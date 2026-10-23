//! Native update host: `rightkit_update_check / _status / _release_status / _download / _install / _cancel`
//! (feature `update`, PK13-2, K13 "updates").
//!
//! `@rightkit/app-update/native` owns policy, status copy and UI scheduling; this module is the
//! native half it talks to. The renderer asks and is told; it never carries a URL, a header, a
//! token or a licence revision.
//!
//! ```ignore
//! let host = UpdateHost::new(
//!     UpdatePolicy::new(UpdateHostPolicy::new(["updates.example.com"], ["dl.example.com"])?),
//!     LicenseAuthority::new(update_auth_provider),      // feature `update-license`
//!     TauriUpdateTransport::new(app.handle().clone(), native_update_config)?,
//! )?;
//! HostServices::builder()
//!     .update(ServiceTrust::for_labels(["main", "settings"]), host)
//!     /* ... */
//!     .build()?
//!     .install(app.handle())?;
//! // invoke_handler: rightkit_tauri_shell::handler![...]  (the five commands are listed for you)
//! ```
//!
//! # Rules
//!
//! * **Closed DTOs.** Every answer is one of [`UpdateCheckView`], [`UpdateStatusView`],
//!   [`UpdateDownloadResult`], [`UpdateInstallResult`], `{ cancelled }` or an [`UpdateHostError`]:
//!   fixed fields, fixed message codes ([`UpdateMessage`]), no free text from the transport or the
//!   authority. The release's artifact URL stays in native memory ([`ReleaseInfo`] has no
//!   `Serialize`); request headers exist only as [`RequestHeaders`] (not `Clone`, not
//!   `Serialize`, redacted `Debug`, zeroed on drop), handed to the transport inside one request.
//! * **Authorization per request, inside the allowlist.** The transport asks
//!   [`RequestAuthorizer::authorize`] for the headers of EVERY request it makes, redirect hops
//!   included. The compiled [`UpdateHostPolicy`] (https only, no userinfo, exact hosts) is checked
//!   BEFORE the authority is asked, so a redirect to a host outside the allowlist gets no headers
//!   and the authority never sees it.
//! * **A binding, never recaptured.** The check captures the licence binding once
//!   ([`UpdateAuthority::begin`]); download and install use that same binding
//!   ([`UpdateAuthority::validate`]). The revision is rechecked before the download starts, when it
//!   ends, and at install admission. A licence that changed since the check refuses the update.
//! * **Opaque, generation-bound handles.** `handleId` is 128 random bits. Every `check` starts a new
//!   generation; a handle of an older generation, an expired one or an unknown one is `stale`; one
//!   that began installing is `consumed` and can never install again.
//! * **Staging identity.** The digest and length the download staged are recorded; install
//!   re-reads them and refuses (`stale`) when they changed.
//! * **Platform and tier.** A release for another platform is not offered; a release whose tier is
//!   not in [`UpdatePolicy::allowed_tiers`] is offered as `eligible: false, refusal: tierIneligible`
//!   with no handle.
//! * **The caller is a trusted app window.** Each command goes through the host caller policy
//!   (`ServiceId::UPDATE`) before it touches the host; a content webview, a foreign origin or an
//!   untrusted label is refused.
//! * **Progress** goes only to the window that started the download, as
//!   `rightkit://update-progress`, at most [`PROGRESS_MIN_INTERVAL`] apart.
//!
//! The Tauri updater itself (HTTP, signature verification, install) is behind [`UpdateTransport`],
//! the same seam [`UpdateAuthority`] is: this crate adds no updater or signing framework, and the
//! licence crate is optional (`update-license`).

pub mod tauri_transport;
pub use tauri_transport::{TauriUpdateConfig, TauriUpdateTransport};

use std::any::Any;
use std::collections::{BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{Manager, Url, Webview, Wry};

use crate::caller_gate::{self, CallerFacts};
use crate::error::ShellError;
use crate::host::{self, HostError, HostServices, ServiceId};

/// The event progress is sent as.
pub const EVENT_UPDATE_PROGRESS: &str = "rightkit://update-progress";
/// Least time between two progress events of one download (the last one always goes).
pub const PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(100);
/// Most live handles at once.
pub const MAX_HANDLES: usize = 4;
/// How long a handle lives after the check that made it.
pub const DEFAULT_HANDLE_TTL: Duration = Duration::from_secs(60 * 60);
/// Consumed handles remembered so a second install says `consumed` rather than `stale`.
const MAX_TOMBSTONES: usize = 8;
const MAX_TEXT: usize = 2048;

// ---- vocabulary ------------------------------------------------------------------------------

/// What kind of release. A patch is public; an update is entitlement-gated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateKind {
    Patch,
    Update,
}

/// Why an update (or a request) is refused. Serialises to the `UpdateRefusal` the renderer knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UpdateRefusal {
    NotLicensed,
    Expired,
    Revoked,
    TierIneligible,
    Busy,
    Unresolved,
    DisabledNoPublicPatch,
    Offline,
    None,
}

impl UpdateRefusal {
    /// The refusal for a wire name (`UpdateAuthError::refusal()` of the licence crate).
    pub fn from_wire(name: &str) -> Self {
        match name {
            "expired" => Self::Expired,
            "revoked" => Self::Revoked,
            "tierIneligible" => Self::TierIneligible,
            "busy" => Self::Busy,
            "unresolved" => Self::Unresolved,
            "disabledNoPublicPatch" => Self::DisabledNoPublicPatch,
            "offline" => Self::Offline,
            "none" => Self::None,
            _ => Self::NotLicensed,
        }
    }

    fn message(self) -> UpdateMessage {
        match self {
            Self::Busy => UpdateMessage::Busy,
            Self::Unresolved => UpdateMessage::Unresolved,
            Self::Expired => UpdateMessage::Expired,
            Self::Revoked => UpdateMessage::Revoked,
            Self::Offline => UpdateMessage::NetworkUnavailable,
            Self::None => UpdateMessage::None,
            _ => UpdateMessage::UpdateRefused,
        }
    }
}

/// The fixed message codes (a subset of `UPDATE_MESSAGES` in `@rightkit/app-update/native`). The
/// renderer owns the text; native sends only the code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UpdateMessage {
    None,
    Unconfigured,
    Busy,
    Unresolved,
    NetworkUnavailable,
    Revoked,
    Expired,
    Forbidden,
    Unsupported,
    UpdateRefused,
    UpdateFailed,
    VerifyFailed,
    StoreFailed,
    Unknown,
}

/// Machine states of the update flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateState {
    Idle,
    Checking,
    Available,
    Downloading,
    Downloaded,
    Installing,
    Failed,
}

/// Error codes a command rejects with (`UpdateErrorCode` in the renderer package).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateErrorCode {
    Unconfigured,
    Busy,
    Unresolved,
    Network,
    Store,
    Verify,
    Forbidden,
    Unsupported,
    Unknown,
}

/// What a command rejects with: `{ code, message }` from the fixed vocabularies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct UpdateHostError {
    pub code: UpdateErrorCode,
    pub message: UpdateMessage,
}

impl UpdateHostError {
    pub fn new(code: UpdateErrorCode, message: UpdateMessage) -> Self {
        Self { code, message }
    }

    fn busy() -> Self {
        Self::new(UpdateErrorCode::Busy, UpdateMessage::Busy)
    }
}

impl std::fmt::Display for UpdateHostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "update error: {:?}", self.code)
    }
}

impl std::error::Error for UpdateHostError {}

impl From<HostError> for UpdateHostError {
    /// A refused caller is `forbidden`, a missing service `unsupported`; nothing else leaks.
    fn from(e: HostError) -> Self {
        match e.code {
            "permission_denied" => Self::new(UpdateErrorCode::Forbidden, UpdateMessage::Forbidden),
            "unsupported" => Self::new(UpdateErrorCode::Unsupported, UpdateMessage::Unsupported),
            _ => Self::new(UpdateErrorCode::Unknown, UpdateMessage::Unknown),
        }
    }
}

impl From<crate::caller_gate::CallerError> for UpdateHostError {
    fn from(_: crate::caller_gate::CallerError) -> Self {
        Self::new(UpdateErrorCode::Forbidden, UpdateMessage::Forbidden)
    }
}

/// The answer to `rightkit_update_check`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheckView {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<UpdateKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    pub eligible: bool,
    pub refusal: UpdateRefusal,
    /// Present only for an available, eligible release.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handle_id: Option<String>,
    /// Unix milliseconds of this check.
    pub checked_at: u64,
    /// Always `true`: the view was read from native state just now.
    pub fresh: bool,
}

/// The answer to `rightkit_update_status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatusView {
    pub state: UpdateState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handle_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<UpdateKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub received: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    pub message: UpdateMessage,
}

/// One `rightkit://update-progress` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProgress {
    pub handle_id: String,
    pub received: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
}

impl UpdateProgress {
    /// `received` never exceeds `total` (the renderer refuses a payload that does).
    pub fn new(handle_id: &str, received: u64, total: Option<u64>) -> Self {
        Self {
            handle_id: handle_id.to_owned(),
            received: total.map_or(received, |t| received.min(t)),
            total,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadState {
    Downloaded,
    Cancelled,
    Failed,
    Refused,
}

/// The answer to `rightkit_update_download`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateDownloadResult {
    pub handle_id: String,
    pub state: DownloadState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<UpdateRefusal>,
    pub message: UpdateMessage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InstallState {
    Installing,
    Refused,
    Failed,
}

/// Why an install was refused: a refusal, or the handle's own state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum InstallReason {
    NotLicensed,
    Expired,
    Revoked,
    TierIneligible,
    Busy,
    Unresolved,
    DisabledNoPublicPatch,
    Offline,
    None,
    /// The handle is from an older check, expired, unknown, not downloaded, or its staged bytes changed.
    Stale,
    /// The handle already began installing.
    Consumed,
}

impl From<UpdateRefusal> for InstallReason {
    fn from(r: UpdateRefusal) -> Self {
        match r {
            UpdateRefusal::NotLicensed => Self::NotLicensed,
            UpdateRefusal::Expired => Self::Expired,
            UpdateRefusal::Revoked => Self::Revoked,
            UpdateRefusal::TierIneligible => Self::TierIneligible,
            UpdateRefusal::Busy => Self::Busy,
            UpdateRefusal::Unresolved => Self::Unresolved,
            UpdateRefusal::DisabledNoPublicPatch => Self::DisabledNoPublicPatch,
            UpdateRefusal::Offline => Self::Offline,
            UpdateRefusal::None => Self::None,
        }
    }
}

/// The answer to `rightkit_update_install`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInstallResult {
    pub state: InstallState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<InstallReason>,
    pub message: UpdateMessage,
}

/// The answer to `rightkit_update_cancel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CancelResult {
    pub cancelled: bool,
}

// ---- seams -----------------------------------------------------------------------------------

/// Headers for ONE request. Not `Clone`, not `Serialize`, redacted `Debug`; values are zeroed on
/// drop. They exist only between the authority and the transport.
pub struct RequestHeaders {
    entries: Vec<(String, String)>,
}

impl RequestHeaders {
    pub fn new(entries: Vec<(String, String)>) -> Self {
        Self { entries }
    }

    /// No header at all (the public route).
    pub fn none() -> Self {
        Self::new(Vec::new())
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Header names, never values.
    pub fn names(&self) -> Vec<&str> {
        self.entries.iter().map(|(n, _)| n.as_str()).collect()
    }

    /// Call `set(name, value)` for each header, for a transport that sets headers one by one. Mark
    /// the values sensitive there and do not log them.
    pub fn each(&self, mut set: impl FnMut(&str, &str)) {
        for (name, value) in &self.entries {
            set(name, value);
        }
    }
}

impl std::fmt::Debug for RequestHeaders {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "RequestHeaders({} header(s), <redacted>)",
            self.entries.len()
        )
    }
}

impl Drop for RequestHeaders {
    fn drop(&mut self) {
        for (_, value) in &mut self.entries {
            let mut bytes = std::mem::take(value).into_bytes();
            bytes.fill(0);
            std::hint::black_box(&bytes);
        }
    }
}

/// An opaque binding an [`UpdateAuthority`] made at the start of an update. The host only carries it.
#[derive(Clone)]
pub struct BindingToken(Arc<dyn Any + Send + Sync>);

impl BindingToken {
    pub fn new<T: Any + Send + Sync>(binding: T) -> Self {
        Self(Arc::new(binding))
    }

    pub fn downcast_ref<T: Any>(&self) -> Option<&T> {
        self.0.downcast_ref::<T>()
    }
}

impl std::fmt::Debug for BindingToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BindingToken(..)")
    }
}

/// Which route a release takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateRoute {
    /// A patch: no licence, no headers.
    Public,
    /// An update: entitlement-gated.
    Private,
}

/// Decides what an update request may carry. The licence adapter is [`LicenseAuthority`]
/// (feature `update-license`); a host with no licensing implements a public-only authority.
pub trait UpdateAuthority: Send + Sync + 'static {
    /// Capture the binding for a new update, once. `Private` is refused unless a valid entitlement
    /// exists now.
    fn begin(&self, route: UpdateRoute) -> Result<BindingToken, UpdateRefusal>;
    /// The same binding, now for release `release_id`. Never recaptures the licence revision.
    fn with_release(&self, binding: &BindingToken, release_id: &str) -> BindingToken;
    /// Revalidate the binding without a request: unchanged revision, valid entitlement.
    fn validate(&self, binding: &BindingToken) -> Result<(), UpdateRefusal>;
    /// The headers `url` may carry for `binding`.
    fn authorize(&self, binding: &BindingToken, url: &Url)
        -> Result<RequestHeaders, UpdateRefusal>;
}

/// The hosts the updater may talk to, compiled into the app. Exact lowercase DNS names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateHostPolicy {
    check_hosts: BTreeSet<String>,
    artifact_hosts: BTreeSet<String>,
}

fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host == host.to_ascii_lowercase()
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

impl UpdateHostPolicy {
    /// `check_hosts` serve the metadata, `artifact_hosts` the artifacts. Each is a lowercase DNS
    /// name with no scheme, port or path; both lists need an entry.
    pub fn new<C, A>(check_hosts: C, artifact_hosts: A) -> Result<Self, ShellError>
    where
        C: IntoIterator,
        C::Item: Into<String>,
        A: IntoIterator,
        A::Item: Into<String>,
    {
        let check_hosts: BTreeSet<String> = check_hosts.into_iter().map(Into::into).collect();
        let artifact_hosts: BTreeSet<String> = artifact_hosts.into_iter().map(Into::into).collect();
        if check_hosts.is_empty()
            || artifact_hosts.is_empty()
            || !check_hosts
                .iter()
                .chain(&artifact_hosts)
                .all(|h| valid_host(h))
        {
            return Err(ShellError::Invalid(
                "update hosts must be non-empty lowercase DNS names (no scheme, port or path)"
                    .into(),
            ));
        }
        Ok(Self {
            check_hosts,
            artifact_hosts,
        })
    }

    /// Whether a request to `url` may be authorized at all: https, no userinfo, no explicit port, a
    /// listed host.
    pub fn allows(&self, url: &Url) -> bool {
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
        {
            return false;
        }
        url.host_str()
            .map(str::to_ascii_lowercase)
            .is_some_and(|h| self.check_hosts.contains(&h) || self.artifact_hosts.contains(&h))
    }

    /// The Right Suite policy for `app_key`, from the RightKit-owned API contract
    /// (`rightkit_license::suite_api`): the current API host is the only check host and the
    /// app's own artifact hosts the only artifact hosts. The retired host is never allowed, and an
    /// app does not receive another app's hosts. An unlisted or malformed key is refused.
    #[cfg(feature = "update-license")]
    pub fn right_suite(app_key: &str) -> Result<Self, ShellError> {
        use rightkit_license::suite_api;
        let artifact_hosts = suite_api::artifact_hosts(app_key)
            .map_err(|e| ShellError::Invalid(format!("Right Suite update hosts: {e}")))?;
        Self::new(
            suite_api::update_check_hosts().iter().copied(),
            artifact_hosts,
        )
    }
}

/// What the transport asks to authorize each request. Checks the compiled allowlist first.
pub struct RequestAuthorizer<'a> {
    hosts: &'a UpdateHostPolicy,
    authority: &'a dyn UpdateAuthority,
    binding: &'a BindingToken,
}

impl RequestAuthorizer<'_> {
    /// The headers `url` may carry. A URL outside the allowlist is refused without asking the
    /// authority, so a redirect cannot carry credentials to another host.
    pub fn authorize(&self, url: &Url) -> Result<RequestHeaders, UpdateRefusal> {
        if !self.hosts.allows(url) {
            return Err(UpdateRefusal::NotLicensed);
        }
        self.authority.authorize(self.binding, url)
    }
}

/// A release the transport found. Native-only: it has no `Serialize`, and the artifact URL never
/// reaches a DTO.
#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    /// The immutable release this is (a version or id the authority binds to).
    pub release_id: String,
    pub version: String,
    pub kind: UpdateKind,
    pub date: Option<String>,
    pub notes_summary: Option<String>,
    pub tier: Option<String>,
    /// The platform this artifact is for (see [`current_platform`]).
    pub platform: String,
    pub artifact_url: Url,
}

/// The digest and length of the bytes a download staged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedIdentity {
    /// Lowercase hex SHA-256.
    pub sha256: String,
    pub length: u64,
}

/// A staged, verified artifact. Not `Clone`: install consumes it.
pub struct StagedArtifact {
    pub identity: StagedIdentity,
    /// The transport's own handle to the staged bytes (a path, an in-memory update object).
    pub token: Box<dyn Any + Send + Sync>,
}

impl std::fmt::Debug for StagedArtifact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StagedArtifact")
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

/// Why a transport step failed. Fixed set: the host never forwards transport text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportError {
    Network,
    /// A request was refused by the authorizer (the licence changed, a host was not allowed).
    Refused(UpdateRefusal),
    Cancelled,
    /// The signature or digest did not verify.
    Verify,
    /// The installer did not run to completion.
    Interrupted,
    Failed,
}

/// The updater: the Tauri updater / install path behind a narrow seam. Every request it makes asks
/// `auth.authorize(url)` for its headers, including each redirect hop.
pub trait UpdateTransport: Send + Sync + 'static {
    /// Fetch the release metadata. `None`: no newer release.
    fn check(&self, auth: &RequestAuthorizer<'_>) -> Result<Option<ReleaseInfo>, TransportError>;
    /// Download and verify the artifact. `progress(received, total)` returns `false` when the
    /// download was cancelled: stop and answer [`TransportError::Cancelled`].
    fn download(
        &self,
        release: &ReleaseInfo,
        auth: &RequestAuthorizer<'_>,
        progress: &mut dyn FnMut(u64, Option<u64>) -> bool,
    ) -> Result<StagedArtifact, TransportError>;
    /// Re-read the identity of the staged bytes (so a swap after the download is seen).
    fn staged_identity(&self, staged: &StagedArtifact) -> Result<StagedIdentity, TransportError>;
    /// Run the installer on the staged artifact.
    fn install(&self, staged: StagedArtifact) -> Result<(), TransportError>;
    /// Read the release status at `url` (already checked against the host allowlist): an
    /// unauthenticated GET with the transport's https-only, no-redirect client, whose body must
    /// pass [`parse_release_status`]. A transport without this answers `Failed`.
    fn release_status(&self, _url: &Url) -> Result<ReleaseStatusView, TransportError> {
        Err(TransportError::Failed)
    }
}

/// The answer to `rightkit_update_release_status`: the closed shape of
/// `ReleaseStatus` in `@rightkit/app-update` (`isReleaseStatus`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseStatusView {
    /// Always `1`.
    pub schema: u8,
    pub app_key: String,
    pub platform: Option<String>,
    pub patch_version: Option<String>,
    pub update_version: Option<String>,
}

/// Longest value a release-status field may carry.
const MAX_STATUS_TEXT: usize = 256;

/// Validate a release-status body against the shape `isReleaseStatus` accepts: a JSON object with
/// exactly the keys `schema` (the number 1), `appKey` (a non-empty string) and `platform`,
/// `patchVersion`, `updateVersion` (each a string or null, none missing). Anything else, and any
/// string over 256 characters or holding a control character, is `None`.
pub fn parse_release_status(body: &[u8]) -> Option<ReleaseStatusView> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let record = value.as_object()?;
    const KEYS: [&str; 5] = [
        "schema",
        "appKey",
        "platform",
        "patchVersion",
        "updateVersion",
    ];
    if record.len() != KEYS.len() || !KEYS.iter().all(|k| record.contains_key(*k)) {
        return None;
    }
    let text = |v: &serde_json::Value| -> Option<String> {
        let t = v.as_str()?;
        (t.chars().count() <= MAX_STATUS_TEXT && !t.chars().any(char::is_control))
            .then(|| t.to_owned())
    };
    let nullable = |v: &serde_json::Value| -> Option<Option<String>> {
        if v.is_null() {
            Some(None)
        } else {
            text(v).map(Some)
        }
    };
    if record["schema"].as_f64() != Some(1.0) {
        return None;
    }
    let app_key = text(&record["appKey"]).filter(|k| !k.is_empty())?;
    Some(ReleaseStatusView {
        schema: 1,
        app_key,
        platform: nullable(&record["platform"])?,
        patch_version: nullable(&record["patchVersion"])?,
        update_version: nullable(&record["updateVersion"])?,
    })
}

// ---- policy ----------------------------------------------------------------------------------

/// The platform string releases are matched against: `<os>-<arch>` with the updater's names
/// (`windows`, `darwin`, `linux`; `x86_64`, `aarch64`, ...).
pub fn current_platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    format!("{os}-{}", std::env::consts::ARCH)
}

/// Fixed configuration of an update host.
#[derive(Debug, Clone)]
pub struct UpdatePolicy {
    pub hosts: UpdateHostPolicy,
    /// Releases for any other platform are not offered. Default [`current_platform`].
    pub platform: String,
    /// When set, only a release whose tier is listed is eligible.
    pub allowed_tiers: Option<BTreeSet<String>>,
    pub max_handles: usize,
    pub handle_ttl: Duration,
    /// Where `release_status` reads from; none means the host answers `unsupported`. Its host
    /// must pass `hosts` (checked again on every read).
    pub release_status_url: Option<Url>,
}

impl UpdatePolicy {
    pub fn new(hosts: UpdateHostPolicy) -> Self {
        Self {
            hosts,
            platform: current_platform(),
            allowed_tiers: None,
            max_handles: MAX_HANDLES,
            handle_ttl: DEFAULT_HANDLE_TTL,
            release_status_url: None,
        }
    }

    /// The Right Suite policy for `app_key`: [`UpdateHostPolicy::right_suite`] hosts plus the
    /// contract's release-status URL for the app.
    #[cfg(feature = "update-license")]
    pub fn right_suite(app_key: &str) -> Result<Self, ShellError> {
        let hosts = UpdateHostPolicy::right_suite(app_key)?;
        let status = rightkit_license::suite_api::release_status_url(app_key)
            .map_err(|e| ShellError::Invalid(format!("Right Suite release status: {e}")))?;
        let status = Url::parse(&status)
            .map_err(|_| ShellError::Invalid("Right Suite release status URL".into()))?;
        Ok(Self::new(hosts).release_status_url(status))
    }

    /// Read the release status from `url`.
    pub fn release_status_url(mut self, url: Url) -> Self {
        self.release_status_url = Some(url);
        self
    }

    pub fn platform(mut self, platform: &str) -> Self {
        self.platform = platform.to_owned();
        self
    }

    pub fn allowed_tiers<I, S>(mut self, tiers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allowed_tiers = Some(tiers.into_iter().map(Into::into).collect());
        self
    }

    pub fn handle_ttl(mut self, ttl: Duration) -> Self {
        self.handle_ttl = ttl;
        self
    }

    fn tier_allowed(&self, tier: Option<&str>) -> bool {
        match (&self.allowed_tiers, tier) {
            (None, _) => true,
            (Some(allowed), Some(t)) => allowed.contains(t),
            (Some(_), None) => false,
        }
    }
}

// ---- progress --------------------------------------------------------------------------------

/// Bounds the progress events of one download: at most one per [`PROGRESS_MIN_INTERVAL`], never a
/// regression, the last one always.
#[derive(Debug)]
pub struct ProgressThrottle {
    min_interval: Duration,
    last_emit: Option<Instant>,
    last_received: u64,
}

impl ProgressThrottle {
    pub fn new(min_interval: Duration) -> Self {
        Self {
            min_interval,
            last_emit: None,
            last_received: 0,
        }
    }

    /// Whether to send this reading now.
    pub fn admit(&mut self, now: Instant, received: u64, total: Option<u64>) -> bool {
        if received < self.last_received {
            return false;
        }
        let done = total.is_some_and(|t| received >= t);
        let due = self
            .last_emit
            .is_none_or(|t| now.saturating_duration_since(t) >= self.min_interval);
        if done || due {
            self.last_emit = Some(now);
            self.last_received = received;
            true
        } else {
            false
        }
    }
}

// ---- host ------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Checking,
    Downloading,
    Installing,
}

enum Stage {
    Available,
    Downloading,
    Downloaded {
        staged: Option<StagedArtifact>,
        identity: StagedIdentity,
    },
    Failed,
}

struct Entry {
    id: String,
    generation: u64,
    release: ReleaseInfo,
    binding: BindingToken,
    stage: Stage,
    created: Instant,
    cancel: Arc<AtomicBool>,
    received: u64,
    total: Option<u64>,
}

#[derive(Default)]
struct HostState {
    generation: u64,
    phase: Option<Phase>,
    handles: Vec<Entry>,
    consumed: VecDeque<String>,
    last_message: Option<UpdateMessage>,
    failed: bool,
}

impl HostState {
    fn phase(&self) -> Phase {
        self.phase.unwrap_or(Phase::Idle)
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn clip(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(MAX_TEXT)
        .collect()
}

fn random_handle() -> Result<String, UpdateHostError> {
    crate::preview::random_hex(16)
        .map_err(|_| UpdateHostError::new(UpdateErrorCode::Unknown, UpdateMessage::Unknown))
}

fn transport_message(e: TransportError) -> (UpdateErrorCode, UpdateMessage) {
    match e {
        TransportError::Network => (UpdateErrorCode::Network, UpdateMessage::NetworkUnavailable),
        TransportError::Verify => (UpdateErrorCode::Verify, UpdateMessage::VerifyFailed),
        TransportError::Refused(r) => (UpdateErrorCode::Unknown, r.message()),
        TransportError::Cancelled | TransportError::Interrupted | TransportError::Failed => {
            (UpdateErrorCode::Unknown, UpdateMessage::UpdateFailed)
        }
    }
}

/// The update host: the state machine behind the five commands. Register it with
/// [`crate::host::HostServicesBuilder::update`].
pub struct UpdateHost {
    authority: Arc<dyn UpdateAuthority>,
    transport: Arc<dyn UpdateTransport>,
    policy: UpdatePolicy,
    state: Mutex<HostState>,
}

impl UpdateHost {
    pub fn new(
        policy: UpdatePolicy,
        authority: impl UpdateAuthority,
        transport: impl UpdateTransport,
    ) -> Result<Self, ShellError> {
        if policy.max_handles == 0 || policy.handle_ttl.is_zero() {
            return Err(ShellError::Invalid(
                "an update policy needs at least one handle and a non-zero handle lifetime".into(),
            ));
        }
        Ok(Self {
            authority: Arc::new(authority),
            transport: Arc::new(transport),
            policy,
            state: Mutex::new(HostState::default()),
        })
    }

    pub fn policy(&self) -> &UpdatePolicy {
        &self.policy
    }

    fn authorizer<'a>(&'a self, binding: &'a BindingToken) -> RequestAuthorizer<'a> {
        RequestAuthorizer {
            hosts: &self.policy.hosts,
            authority: self.authority.as_ref(),
            binding,
        }
    }

    fn expire(&self, state: &mut HostState, now: Instant) {
        let ttl = self.policy.handle_ttl;
        let phase = state.phase();
        state.handles.retain(|h| {
            let live = matches!(h.stage, Stage::Downloading) && phase == Phase::Downloading;
            live || now.saturating_duration_since(h.created) < ttl
        });
    }

    fn finish(&self, outcome: impl FnOnce(&mut HostState)) {
        let mut state = lock(&self.state);
        state.phase = Some(Phase::Idle);
        outcome(&mut state);
    }

    /// Check for an update. Starts a new generation: every earlier handle becomes stale.
    pub fn check(&self) -> Result<UpdateCheckView, UpdateHostError> {
        {
            let mut state = lock(&self.state);
            if state.phase() != Phase::Idle {
                return Err(UpdateHostError::busy());
            }
            state.phase = Some(Phase::Checking);
            state.generation += 1;
            state.failed = false;
            state.last_message = None;
            // Handles of an older generation can never act again; drop their staged bytes.
            state.handles.clear();
        }
        let result = self.check_inner();
        match &result {
            Ok(_) => self.finish(|_| {}),
            Err(e) => {
                let message = e.message;
                self.finish(|s| {
                    s.failed = true;
                    s.last_message = Some(message);
                });
            }
        }
        result
    }

    fn check_inner(&self) -> Result<UpdateCheckView, UpdateHostError> {
        // The binding is captured once, here, before any request. An update needs a private one;
        // when there is none (no licence, licensing off) the metadata check still runs on the
        // public route so a patch can be found.
        let private = self.authority.begin(UpdateRoute::Private);
        let metadata_binding = match &private {
            Ok(b) => b.clone(),
            Err(_) => self
                .authority
                .begin(UpdateRoute::Public)
                .map_err(|r| UpdateHostError::new(UpdateErrorCode::Unknown, r.message()))?,
        };
        let found = self
            .transport
            .check(&self.authorizer(&metadata_binding))
            .map_err(|e| {
                let (code, message) = transport_message(e);
                UpdateHostError::new(code, message)
            })?;
        let checked_at = now_epoch_ms();
        let none = |refusal: UpdateRefusal| UpdateCheckView {
            available: false,
            kind: None,
            version: None,
            date: None,
            notes_summary: None,
            tier: None,
            eligible: false,
            refusal,
            handle_id: None,
            checked_at,
            fresh: true,
        };
        let Some(release) = found else {
            return Ok(none(UpdateRefusal::None));
        };
        if release.platform != self.policy.platform {
            // Another platform's release is not an update for this install.
            return Ok(none(UpdateRefusal::None));
        }
        let mut view = UpdateCheckView {
            available: true,
            kind: Some(release.kind),
            version: Some(clip(&release.version)),
            date: release.date.as_deref().map(clip),
            notes_summary: release.notes_summary.as_deref().map(clip),
            tier: release.tier.as_deref().map(clip),
            eligible: false,
            refusal: UpdateRefusal::None,
            handle_id: None,
            checked_at,
            fresh: true,
        };
        let binding = match (release.kind, &private) {
            (UpdateKind::Patch, _) => self
                .authority
                .begin(UpdateRoute::Public)
                .map(|b| self.authority.with_release(&b, &release.release_id)),
            (UpdateKind::Update, Ok(b)) => Ok(self.authority.with_release(b, &release.release_id)),
            (UpdateKind::Update, Err(refusal)) => Err(*refusal),
        };
        let binding = match binding {
            Ok(b) if self.policy.tier_allowed(release.tier.as_deref()) => b,
            Ok(_) => {
                view.refusal = UpdateRefusal::TierIneligible;
                return Ok(view);
            }
            Err(refusal) => {
                view.refusal = refusal;
                return Ok(view);
            }
        };
        let id = random_handle()?;
        let mut state = lock(&self.state);
        let now = Instant::now();
        self.expire(&mut state, now);
        while state.handles.len() >= self.policy.max_handles {
            state.handles.remove(0);
        }
        let generation = state.generation;
        state.handles.push(Entry {
            id: id.clone(),
            generation,
            release,
            binding,
            stage: Stage::Available,
            created: now,
            cancel: Arc::new(AtomicBool::new(false)),
            received: 0,
            total: None,
        });
        view.eligible = true;
        view.handle_id = Some(id);
        Ok(view)
    }

    /// The release status for the configured URL: the closed [`ReleaseStatusView`] or a fixed
    /// error. Unconfigured, or a URL the host policy does not allow, is `unsupported`. It reads
    /// no licence and holds no flow state, so it is not serialised with check or download.
    pub fn release_status(&self) -> Result<ReleaseStatusView, UpdateHostError> {
        let unsupported =
            || UpdateHostError::new(UpdateErrorCode::Unsupported, UpdateMessage::Unsupported);
        let url = self
            .policy
            .release_status_url
            .as_ref()
            .ok_or_else(unsupported)?;
        if !self.policy.hosts.allows(url) {
            return Err(unsupported());
        }
        self.transport.release_status(url).map_err(|e| {
            let (code, message) = transport_message(e);
            UpdateHostError::new(code, message)
        })
    }

    /// The state, read now.
    pub fn status(&self) -> UpdateStatusView {
        let mut state = lock(&self.state);
        self.expire(&mut state, Instant::now());
        let phase = state.phase();
        let current = state
            .handles
            .iter()
            .rev()
            .find(|h| h.generation == state.generation);
        let view_of = |s: UpdateState, e: Option<&Entry>, message| UpdateStatusView {
            state: s,
            handle_id: e.map(|h| h.id.clone()),
            version: e.map(|h| clip(&h.release.version)),
            kind: e.map(|h| h.release.kind),
            received: e
                .filter(|_| s == UpdateState::Downloading)
                .map(|h| h.received),
            total: e
                .filter(|_| s == UpdateState::Downloading)
                .and_then(|h| h.total),
            message,
        };
        let message = state.last_message.unwrap_or(UpdateMessage::None);
        match (phase, current) {
            (Phase::Checking, e) => view_of(UpdateState::Checking, e, message),
            (Phase::Downloading, e) => view_of(UpdateState::Downloading, e, message),
            (Phase::Installing, e) => view_of(UpdateState::Installing, e, message),
            (Phase::Idle, Some(e)) => match e.stage {
                Stage::Available => view_of(UpdateState::Available, Some(e), message),
                Stage::Downloading => view_of(UpdateState::Downloading, Some(e), message),
                Stage::Downloaded { .. } => view_of(UpdateState::Downloaded, Some(e), message),
                Stage::Failed => view_of(UpdateState::Failed, Some(e), message),
            },
            (Phase::Idle, None) if state.failed => view_of(UpdateState::Failed, None, message),
            (Phase::Idle, None) => view_of(UpdateState::Idle, None, UpdateMessage::None),
        }
    }

    /// Cancel the download of `handle_id`. `cancelled: false` when nothing was downloading it.
    pub fn cancel(&self, handle_id: &str) -> CancelResult {
        let state = lock(&self.state);
        let active = state.phase() == Phase::Downloading
            && state
                .handles
                .iter()
                .find(|h| h.id == handle_id && h.generation == state.generation)
                .is_some_and(|h| matches!(h.stage, Stage::Downloading));
        if active {
            if let Some(h) = state.handles.iter().find(|h| h.id == handle_id) {
                h.cancel.store(true, Ordering::Release);
            }
        }
        CancelResult { cancelled: active }
    }

    /// Download the update `handle_id` names. `progress` is called, throttled, on this thread.
    pub fn download(
        &self,
        handle_id: &str,
        progress: &dyn Fn(&UpdateProgress),
    ) -> Result<UpdateDownloadResult, UpdateHostError> {
        let result_for = |state, reason: Option<UpdateRefusal>, message| UpdateDownloadResult {
            handle_id: handle_id.to_owned(),
            state,
            reason,
            message,
        };
        let (release, binding, cancel, generation) = {
            let mut state = lock(&self.state);
            if state.phase() != Phase::Idle {
                return Err(UpdateHostError::busy());
            }
            self.expire(&mut state, Instant::now());
            let current = state.generation;
            let Some(entry) = state
                .handles
                .iter_mut()
                .find(|h| h.id == handle_id && h.generation == current)
            else {
                return Ok(result_for(
                    DownloadState::Failed,
                    None,
                    UpdateMessage::UpdateFailed,
                ));
            };
            if !matches!(entry.stage, Stage::Available) {
                return Ok(result_for(
                    DownloadState::Failed,
                    None,
                    UpdateMessage::UpdateFailed,
                ));
            }
            if let Err(refusal) = self.authority.validate(&entry.binding) {
                return Ok(result_for(
                    DownloadState::Refused,
                    Some(refusal),
                    refusal.message(),
                ));
            }
            entry.stage = Stage::Downloading;
            entry.cancel = Arc::new(AtomicBool::new(false));
            let parts = (
                entry.release.clone(),
                entry.binding.clone(),
                Arc::clone(&entry.cancel),
                entry.generation,
            );
            state.phase = Some(Phase::Downloading);
            parts
        };

        let mut throttle = ProgressThrottle::new(PROGRESS_MIN_INTERVAL);
        let mut on_progress = |received: u64, total: Option<u64>| -> bool {
            if cancel.load(Ordering::Acquire) {
                return false;
            }
            {
                let mut state = lock(&self.state);
                if let Some(e) = state.handles.iter_mut().find(|h| h.id == handle_id) {
                    e.received = total.map_or(received, |t| received.min(t));
                    e.total = total;
                }
            }
            if throttle.admit(Instant::now(), received, total) {
                progress(&UpdateProgress::new(handle_id, received, total));
            }
            true
        };
        let outcome =
            self.transport
                .download(&release, &self.authorizer(&binding), &mut on_progress);
        // The licence may have changed while the bytes came down: check again before the result
        // counts, whatever the transport saw.
        let revalidated = self.authority.validate(&binding);
        let mut state = lock(&self.state);
        state.phase = Some(Phase::Idle);
        let Some(entry) = state
            .handles
            .iter_mut()
            .find(|h| h.id == handle_id && h.generation == generation)
        else {
            // A newer check cleared the table while this ran.
            return Ok(result_for(
                DownloadState::Failed,
                None,
                UpdateMessage::UpdateFailed,
            ));
        };
        match outcome {
            Ok(staged) => match revalidated {
                Ok(()) => {
                    let identity = staged.identity.clone();
                    entry.stage = Stage::Downloaded {
                        staged: Some(staged),
                        identity,
                    };
                    state.last_message = None;
                    Ok(result_for(
                        DownloadState::Downloaded,
                        None,
                        UpdateMessage::None,
                    ))
                }
                Err(refusal) => {
                    // The bytes are dropped with the staged artifact.
                    entry.stage = Stage::Failed;
                    state.last_message = Some(refusal.message());
                    Ok(result_for(
                        DownloadState::Refused,
                        Some(refusal),
                        refusal.message(),
                    ))
                }
            },
            Err(TransportError::Cancelled) => {
                entry.stage = Stage::Available;
                Ok(result_for(
                    DownloadState::Cancelled,
                    None,
                    UpdateMessage::None,
                ))
            }
            Err(TransportError::Refused(refusal)) => {
                entry.stage = Stage::Failed;
                state.last_message = Some(refusal.message());
                Ok(result_for(
                    DownloadState::Refused,
                    Some(refusal),
                    refusal.message(),
                ))
            }
            Err(other) => {
                let (_, message) = transport_message(other);
                entry.stage = Stage::Failed;
                state.last_message = Some(message);
                Ok(result_for(DownloadState::Failed, None, message))
            }
        }
    }

    /// Install the downloaded update `handle_id` names. Admission rechecks the licence binding and
    /// the staged identity; a handle installs at most once.
    pub fn install(&self, handle_id: &str) -> Result<UpdateInstallResult, UpdateHostError> {
        let refused = |reason: InstallReason, message| UpdateInstallResult {
            state: InstallState::Refused,
            reason: Some(reason),
            message,
        };
        // Phase 1, under the lock: find the handle and take its staged bytes. The slow checks
        // below run without the lock, so `status` stays answerable.
        let (staged, recorded, binding, generation) = {
            let mut state = lock(&self.state);
            if state.phase() != Phase::Idle {
                return Err(UpdateHostError::busy());
            }
            self.expire(&mut state, Instant::now());
            if state.consumed.iter().any(|c| c == handle_id) {
                return Ok(refused(
                    InstallReason::Consumed,
                    UpdateMessage::UpdateRefused,
                ));
            }
            let current = state.generation;
            let Some(entry) = state
                .handles
                .iter_mut()
                .find(|h| h.id == handle_id && h.generation == current)
            else {
                return Ok(refused(InstallReason::Stale, UpdateMessage::UpdateRefused));
            };
            if entry.release.platform != self.policy.platform
                || !self.policy.tier_allowed(entry.release.tier.as_deref())
            {
                return Ok(refused(InstallReason::Stale, UpdateMessage::UpdateRefused));
            }
            let binding = entry.binding.clone();
            let (staged, recorded) = match &mut entry.stage {
                Stage::Downloaded { staged, identity } => (staged.take(), identity.clone()),
                _ => return Ok(refused(InstallReason::Stale, UpdateMessage::UpdateRefused)),
            };
            let Some(staged) = staged else {
                return Ok(refused(InstallReason::Stale, UpdateMessage::UpdateRefused));
            };
            state.phase = Some(Phase::Installing);
            (staged, recorded, binding, current)
        };

        // Phase 2, no lock: the licence the update began with must still hold, right now, and the
        // staged bytes must still be the ones that were downloaded.
        let licence = self.authority.validate(&binding);
        let identity = match licence {
            Ok(()) => Some(self.transport.staged_identity(&staged)),
            Err(_) => None,
        };

        // Phase 3, under the lock: put the bytes back on a refusal, or spend the handle.
        let restore = |state: &mut HostState, staged: StagedArtifact, keep: bool| {
            if let Some(entry) = state
                .handles
                .iter_mut()
                .find(|h| h.id == handle_id && h.generation == generation)
            {
                entry.stage = if keep {
                    Stage::Downloaded {
                        identity: recorded.clone(),
                        staged: Some(staged),
                    }
                } else {
                    Stage::Failed
                };
            }
        };
        if let Err(refusal) = licence {
            let mut state = lock(&self.state);
            state.phase = Some(Phase::Idle);
            restore(&mut state, staged, true);
            return Ok(refused(refusal.into(), refusal.message()));
        }
        if identity.as_ref().and_then(|r| r.as_ref().ok()) != Some(&recorded) {
            let mut state = lock(&self.state);
            state.phase = Some(Phase::Idle);
            restore(&mut state, staged, false);
            return Ok(refused(InstallReason::Stale, UpdateMessage::VerifyFailed));
        }
        {
            // Past this point the handle is spent, whatever the installer does.
            let mut state = lock(&self.state);
            state.consumed.push_back(handle_id.to_owned());
            while state.consumed.len() > MAX_TOMBSTONES {
                state.consumed.pop_front();
            }
        }
        match self.transport.install(staged) {
            // The installer is running (the app will exit and relaunch): the phase stays
            // `Installing` until the process ends.
            Ok(()) => Ok(UpdateInstallResult {
                state: InstallState::Installing,
                reason: None,
                message: UpdateMessage::None,
            }),
            Err(e) => {
                let (_, message) = transport_message(e);
                self.finish(|s| {
                    s.failed = true;
                    s.last_message = Some(message);
                    for h in s.handles.iter_mut().filter(|h| h.id == handle_id) {
                        h.stage = Stage::Failed;
                    }
                });
                Ok(UpdateInstallResult {
                    state: InstallState::Failed,
                    reason: None,
                    message,
                })
            }
        }
    }
}

// ---- licence adapter -------------------------------------------------------------------------

/// [`UpdateAuthority`] over `rightkit-license`'s `UpdateAuthProvider` (feature `update-license`).
#[cfg(feature = "update-license")]
pub struct LicenseAuthority {
    provider: rightkit_license::UpdateAuthProvider,
}

#[cfg(feature = "update-license")]
impl LicenseAuthority {
    pub fn new(provider: rightkit_license::UpdateAuthProvider) -> Self {
        Self { provider }
    }
}

#[cfg(feature = "update-license")]
fn license_refusal(error: rightkit_license::UpdateAuthError) -> UpdateRefusal {
    UpdateRefusal::from_wire(error.refusal())
}

#[cfg(feature = "update-license")]
impl UpdateAuthority for LicenseAuthority {
    fn begin(&self, route: UpdateRoute) -> Result<BindingToken, UpdateRefusal> {
        let (revision, route) = match route {
            UpdateRoute::Public => (
                rightkit_license::LicenseRevision::none(),
                rightkit_license::UpdateRoute::Public,
            ),
            UpdateRoute::Private => (
                self.provider.revision().map_err(license_refusal)?,
                rightkit_license::UpdateRoute::Private,
            ),
        };
        Ok(BindingToken::new(rightkit_license::UpdateBinding {
            revision,
            route,
            release_id: String::new(),
            staged: None,
        }))
    }

    fn with_release(&self, binding: &BindingToken, release_id: &str) -> BindingToken {
        match binding.downcast_ref::<rightkit_license::UpdateBinding>() {
            Some(b) => {
                let mut next = b.clone();
                next.release_id = release_id.to_owned();
                BindingToken::new(next)
            }
            None => binding.clone(),
        }
    }

    fn validate(&self, binding: &BindingToken) -> Result<(), UpdateRefusal> {
        let b = binding
            .downcast_ref::<rightkit_license::UpdateBinding>()
            .ok_or(UpdateRefusal::NotLicensed)?;
        self.provider.validate(b).map_err(license_refusal)
    }

    fn authorize(
        &self,
        binding: &BindingToken,
        url: &Url,
    ) -> Result<RequestHeaders, UpdateRefusal> {
        let b = binding
            .downcast_ref::<rightkit_license::UpdateBinding>()
            .ok_or(UpdateRefusal::NotLicensed)?;
        // The licence crate takes the `url` crate's type; this one is the same type re-exported
        // by Tauri, parsed again so a mismatched `url` major cannot break the build.
        let url = url_for_license(url)?;
        let headers = self.provider.authorize(b, &url).map_err(license_refusal)?;
        let mut entries = Vec::new();
        headers
            .each(|name, value| entries.push((name.to_owned(), value.to_owned())))
            .map_err(license_refusal)?;
        Ok(RequestHeaders::new(entries))
    }
}

#[cfg(feature = "update-license")]
fn url_for_license(url: &Url) -> Result<url::Url, UpdateRefusal> {
    url::Url::parse(url.as_str()).map_err(|_| UpdateRefusal::NotLicensed)
}

// ---- commands --------------------------------------------------------------------------------

/// The update host `facts` may use for `action`; the first call of every update command.
pub fn update_host_for(
    host: &HostServices,
    action: &str,
    facts: &CallerFacts<'_>,
) -> Result<Arc<UpdateHost>, HostError> {
    host.service_for::<UpdateHost>(ServiceId::UPDATE, action, facts)
}

fn update_host(webview: &Webview<Wry>, action: &str) -> Result<Arc<UpdateHost>, UpdateHostError> {
    let app = webview.app_handle();
    let services = host::managed(app)?;
    let owned = caller_gate::gather(webview)?;
    Ok(update_host_for(&services, action, &owned.facts())?)
}

async fn off_thread<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, UpdateHostError> + Send + 'static,
) -> Result<T, UpdateHostError> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|_| UpdateHostError::new(UpdateErrorCode::Unknown, UpdateMessage::Unknown))?
}

/// Check for an update.
#[tauri::command]
pub async fn rightkit_update_check(
    webview: Webview<Wry>,
) -> Result<UpdateCheckView, UpdateHostError> {
    let host = update_host(&webview, "check")?;
    off_thread(move || host.check()).await
}

/// The state of the update flow, read now.
#[tauri::command]
pub async fn rightkit_update_status(
    webview: Webview<Wry>,
) -> Result<UpdateStatusView, UpdateHostError> {
    let host = update_host(&webview, "status")?;
    Ok(host.status())
}

/// The release status (`ReleaseStatus` of `@rightkit/app-update`), read natively.
#[tauri::command]
pub async fn rightkit_update_release_status(
    webview: Webview<Wry>,
) -> Result<ReleaseStatusView, UpdateHostError> {
    let host = update_host(&webview, "release_status")?;
    off_thread(move || host.release_status()).await
}

/// Download the update `handle_id` names. Progress goes to the invoking window only.
#[tauri::command]
pub async fn rightkit_update_download(
    webview: Webview<Wry>,
    handle_id: String,
) -> Result<UpdateDownloadResult, UpdateHostError> {
    use tauri::Emitter;
    let host = update_host(&webview, "download")?;
    let app = webview.app_handle().clone();
    let label = webview.label().to_owned();
    off_thread(move || {
        host.download(&handle_id, &|p| {
            let Some(webview) = app.get_webview(&label) else {
                return;
            };
            if host::authorize(&webview, ServiceId::UPDATE, "download").is_err() {
                return;
            }
            let _ = app.emit_to(label.clone(), EVENT_UPDATE_PROGRESS, p.clone());
        })
    })
    .await
}

/// Install the downloaded update `handle_id` names.
#[tauri::command]
pub async fn rightkit_update_install(
    webview: Webview<Wry>,
    handle_id: String,
) -> Result<UpdateInstallResult, UpdateHostError> {
    let host = update_host(&webview, "install")?;
    off_thread(move || host.install(&handle_id)).await
}

/// Cancel the running download of `handle_id`.
#[tauri::command]
pub async fn rightkit_update_cancel(
    webview: Webview<Wry>,
    handle_id: String,
) -> Result<CancelResult, UpdateHostError> {
    let host = update_host(&webview, "cancel")?;
    Ok(host.cancel(&handle_id))
}
