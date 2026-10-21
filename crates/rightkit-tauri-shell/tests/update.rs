#![cfg(feature = "update")]
//! The native update host (PK13-2/3, K13): closed DTOs, per-request authorization inside the
//! compiled allowlist, a licence binding that is never recaptured and is rechecked before the
//! download, when it ends and at install, generation-bound handles, staging identity, platform and
//! tier, installer interruption, and the caller policy in front of it all.
//!
//! The Tauri updater and the licence are fakes behind the two seams (`UpdateTransport`,
//! `UpdateAuthority`); the host, the DTOs and the policy are the real code.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rightkit_tauri_shell::caller_gate::CallerFacts;
use rightkit_tauri_shell::host::{HostError, HostServices, ServiceTrust};
use rightkit_tauri_shell::update::{
    current_platform, update_host_for, BindingToken, CancelResult, DownloadState, InstallReason,
    InstallState, ProgressThrottle, ReleaseInfo, RequestAuthorizer, RequestHeaders, StagedArtifact,
    StagedIdentity, TransportError, UpdateAuthority, UpdateErrorCode, UpdateHost, UpdateHostError,
    UpdateHostPolicy, UpdateKind, UpdateMessage, UpdatePolicy, UpdateProgress, UpdateRefusal,
    UpdateRoute, UpdateState, UpdateTransport,
};
use rightkit_tauri_shell::NavPolicy;
use serde_json::{json, Value};
use tauri::Url;

const SECRET: &str = "SENTINEL_BEARER_TOKEN";
const ARTIFACT: &str = "https://dl.example.com/downloads/app/2.0.0/app-secret-path.zip";
const EVIL: &str = "https://evil.example.net/steal";

// ---- fakes -----------------------------------------------------------------------------------

struct World {
    licensed: AtomicBool,
    revision: AtomicU64,
    authorize_calls: AtomicUsize,
    authorized_urls: Mutex<Vec<String>>,
}

impl World {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            licensed: AtomicBool::new(true),
            revision: AtomicU64::new(1),
            authorize_calls: AtomicUsize::new(0),
            authorized_urls: Mutex::new(Vec::new()),
        })
    }
}

struct FakeBinding {
    revision: u64,
    route: UpdateRoute,
    release_id: String,
}

struct FakeAuthority(Arc<World>);

impl FakeAuthority {
    fn check(&self, b: &FakeBinding) -> Result<(), UpdateRefusal> {
        if b.route == UpdateRoute::Public {
            return Ok(());
        }
        if !self.0.licensed.load(Ordering::SeqCst) {
            return Err(UpdateRefusal::Revoked);
        }
        if b.revision != self.0.revision.load(Ordering::SeqCst) {
            return Err(UpdateRefusal::NotLicensed);
        }
        Ok(())
    }
}

impl UpdateAuthority for FakeAuthority {
    fn begin(&self, route: UpdateRoute) -> Result<BindingToken, UpdateRefusal> {
        if route == UpdateRoute::Private && !self.0.licensed.load(Ordering::SeqCst) {
            return Err(UpdateRefusal::NotLicensed);
        }
        Ok(BindingToken::new(FakeBinding {
            revision: self.0.revision.load(Ordering::SeqCst),
            route,
            release_id: String::new(),
        }))
    }

    fn with_release(&self, binding: &BindingToken, release_id: &str) -> BindingToken {
        let b = binding.downcast_ref::<FakeBinding>().unwrap();
        BindingToken::new(FakeBinding {
            revision: b.revision,
            route: b.route,
            release_id: release_id.to_owned(),
        })
    }

    fn validate(&self, binding: &BindingToken) -> Result<(), UpdateRefusal> {
        self.check(binding.downcast_ref::<FakeBinding>().unwrap())
    }

    fn authorize(
        &self,
        binding: &BindingToken,
        url: &Url,
    ) -> Result<RequestHeaders, UpdateRefusal> {
        let b = binding.downcast_ref::<FakeBinding>().unwrap();
        self.0.authorize_calls.fetch_add(1, Ordering::SeqCst);
        self.0.authorized_urls.lock().unwrap().push(url.to_string());
        self.check(b)?;
        Ok(match b.route {
            UpdateRoute::Public => RequestHeaders::none(),
            UpdateRoute::Private => RequestHeaders::new(vec![(
                "Authorization".to_owned(),
                format!("Bearer {SECRET}"),
            )]),
        })
    }
}

type Hook = Box<dyn Fn() + Send + Sync>;

struct FakeTransport {
    release: Mutex<Option<ReleaseInfo>>,
    staged: Mutex<StagedIdentity>,
    install_result: Mutex<Result<(), TransportError>>,
    chunks: usize,
    follow_redirect: AtomicBool,
    redirect_refused: AtomicBool,
    /// Runs after the first chunk, on the download thread.
    mid_download: Mutex<Option<Hook>>,
    installed: AtomicUsize,
}

fn release(kind: UpdateKind, platform: &str, tier: Option<&str>) -> ReleaseInfo {
    ReleaseInfo {
        release_id: "2.0.0".into(),
        version: "2.0.0".into(),
        kind,
        date: Some("2026-10-20T00:00:00Z".into()),
        notes_summary: Some("Fixes".into()),
        tier: tier.map(str::to_owned),
        platform: platform.to_owned(),
        artifact_url: Url::parse(ARTIFACT).unwrap(),
    }
}

fn identity(sha: &str) -> StagedIdentity {
    StagedIdentity {
        sha256: sha.into(),
        length: 400,
    }
}

impl FakeTransport {
    fn new(release: Option<ReleaseInfo>) -> Arc<Self> {
        Arc::new(Self {
            release: Mutex::new(release),
            staged: Mutex::new(identity("aa")),
            install_result: Mutex::new(Ok(())),
            chunks: 4,
            follow_redirect: AtomicBool::new(false),
            redirect_refused: AtomicBool::new(false),
            mid_download: Mutex::new(None),
            installed: AtomicUsize::new(0),
        })
    }
}

struct Shared(Arc<FakeTransport>);

impl UpdateTransport for Shared {
    fn check(&self, auth: &RequestAuthorizer<'_>) -> Result<Option<ReleaseInfo>, TransportError> {
        auth.authorize(&Url::parse("https://updates.example.com/latest.json").unwrap())
            .map_err(TransportError::Refused)?;
        Ok(self.0.release.lock().unwrap().clone())
    }

    fn download(
        &self,
        release: &ReleaseInfo,
        auth: &RequestAuthorizer<'_>,
        progress: &mut dyn FnMut(u64, Option<u64>) -> bool,
    ) -> Result<StagedArtifact, TransportError> {
        auth.authorize(&release.artifact_url)
            .map_err(TransportError::Refused)?;
        if self.0.follow_redirect.load(Ordering::SeqCst) {
            // A redirect hop to a host that is not on the compiled allowlist.
            let hop = auth.authorize(&Url::parse(EVIL).unwrap());
            self.0
                .redirect_refused
                .store(hop.is_err(), Ordering::SeqCst);
        }
        let total = self.0.chunks as u64 * 100;
        for i in 1..=self.0.chunks as u64 {
            if !progress(i * 100, Some(total)) {
                return Err(TransportError::Cancelled);
            }
            if i == 1 {
                if let Some(hook) = self.0.mid_download.lock().unwrap().as_ref() {
                    hook();
                }
                // The next ranged request is authorized again, like a real transfer.
                auth.authorize(&release.artifact_url)
                    .map_err(TransportError::Refused)?;
            }
        }
        Ok(StagedArtifact {
            identity: self.0.staged.lock().unwrap().clone(),
            token: Box::new(()),
        })
    }

    fn staged_identity(&self, _staged: &StagedArtifact) -> Result<StagedIdentity, TransportError> {
        Ok(self.0.staged.lock().unwrap().clone())
    }

    fn install(&self, _staged: StagedArtifact) -> Result<(), TransportError> {
        self.0.installed.fetch_add(1, Ordering::SeqCst);
        *self.0.install_result.lock().unwrap()
    }
}

struct Rig {
    host: Arc<UpdateHost>,
    world: Arc<World>,
    transport: Arc<FakeTransport>,
}

fn hosts() -> UpdateHostPolicy {
    UpdateHostPolicy::new(["updates.example.com"], ["dl.example.com"]).unwrap()
}

fn rig_with(policy: UpdatePolicy, release: Option<ReleaseInfo>) -> Rig {
    let world = World::new();
    let transport = FakeTransport::new(release);
    let host = UpdateHost::new(
        policy,
        FakeAuthority(Arc::clone(&world)),
        Shared(Arc::clone(&transport)),
    )
    .unwrap();
    Rig {
        host: Arc::new(host),
        world,
        transport,
    }
}

fn rig(kind: UpdateKind) -> Rig {
    rig_with(
        UpdatePolicy::new(hosts()),
        Some(release(kind, &current_platform(), Some("pro"))),
    )
}

fn no_progress() -> impl Fn(&UpdateProgress) {
    |_| {}
}

fn json_of<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap()
}

/// check, then download; returns the handle.
fn downloaded(rig: &Rig) -> String {
    let view = rig.host.check().unwrap();
    let id = view.handle_id.expect("an eligible release has a handle");
    let result = rig.host.download(&id, &no_progress()).unwrap();
    assert_eq!(result.state, DownloadState::Downloaded, "{result:?}");
    id
}

// ---- the shape of what the renderer gets ------------------------------------------------------

#[test]
fn an_eligible_check_answers_exactly_the_closed_view() {
    let r = rig(UpdateKind::Update);
    let view = r.host.check().unwrap();
    let v = json_of(&view);
    let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "available",
            "checkedAt",
            "date",
            "eligible",
            "fresh",
            "handleId",
            "kind",
            "notesSummary",
            "refusal",
            "tier",
            "version"
        ]
    );
    assert_eq!(v["available"], true);
    assert_eq!(v["eligible"], true);
    assert_eq!(v["kind"], "update");
    assert_eq!(v["refusal"], "none");
    assert_eq!(v["fresh"], true);
    assert!(v["checkedAt"].as_u64().is_some());
    let handle = v["handleId"].as_str().unwrap();
    assert_eq!(handle.len(), 32);
    assert!(handle.chars().all(|c| c.is_ascii_hexdigit()), "{handle}");
}

#[test]
fn nothing_the_renderer_receives_carries_a_url_a_header_or_a_token() {
    let r = rig(UpdateKind::Update);
    r.transport.follow_redirect.store(true, Ordering::SeqCst);
    let mut seen = Vec::new();
    let view = r.host.check().unwrap();
    seen.push(json_of(&view));
    let id = view.handle_id.clone().unwrap();
    seen.push(json_of(&r.host.status()));
    let progress = Mutex::new(Vec::new());
    let result = r
        .host
        .download(&id, &|p| progress.lock().unwrap().push(json_of(p)))
        .unwrap();
    seen.push(json_of(&result));
    seen.extend(progress.into_inner().unwrap());
    seen.push(json_of(&r.host.status()));
    seen.push(json_of(&r.host.install(&id).unwrap()));
    seen.push(json_of(&r.host.cancel(&id)));
    seen.push(json_of(&r.host.status()));
    // Errors too: a refused caller and a transport failure.
    seen.push(json_of(&UpdateHostError::from(HostError::new(
        "permission_denied",
        "forbidden: the caller is a content webview /secret/path",
    ))));
    let wire = serde_json::to_string(&seen).unwrap();
    for needle in [
        SECRET,
        "Bearer",
        "dl.example.com",
        "updates.example.com",
        "evil.example.net",
        "secret-path",
        "https://",
        "Authorization",
    ] {
        assert!(
            !wire.contains(needle),
            "the renderer saw {needle:?}: {wire}"
        );
    }
    // The authority did hand the transport a secret: it just never reached an answer.
    assert!(r.world.authorize_calls.load(Ordering::SeqCst) >= 2);
}

#[test]
fn the_native_only_types_cannot_be_serialised_or_cloned() {
    // A source tripwire: these declarations must never gain `Serialize` (or `Clone` for the headers).
    // LF-normalised: a Windows checkout may hold CRLF, and the scan below finds items by blank lines.
    let src = include_str!("../src/update.rs").replace("\r\n", "\n");
    for decl in [
        "pub struct ReleaseInfo",
        "pub struct RequestHeaders",
        "pub struct BindingToken",
        "pub struct StagedArtifact",
    ] {
        let at = src.find(decl).unwrap_or_else(|| panic!("{decl} moved"));
        // The attribute lines of this item only: from the last blank line before it.
        let start = src[..at].rfind("\n\n").map_or(0, |i| i + 2);
        let attrs: String = src[start..at]
            .lines()
            .filter(|l| l.trim_start().starts_with("#["))
            .collect();
        assert!(!attrs.contains("Serialize"), "{decl} derives Serialize");
        if decl.ends_with("RequestHeaders") || decl.ends_with("StagedArtifact") {
            assert!(!attrs.contains("Clone"), "{decl} derives Clone");
        }
    }
    let headers = RequestHeaders::new(vec![("Authorization".into(), format!("Bearer {SECRET}"))]);
    assert!(!format!("{headers:?}").contains(SECRET));
    assert_eq!(headers.names(), ["Authorization"]);
    let mut got = Vec::new();
    headers.each(|n, v| got.push((n.to_owned(), v.to_owned())));
    assert!(
        got[0].1.contains(SECRET),
        "the transport does get the value"
    );
}

#[test]
fn errors_are_codes_from_a_fixed_vocabulary() {
    let forbidden = UpdateHostError::from(HostError::new("permission_denied", "x /secret"));
    assert_eq!(
        json_of(&forbidden),
        json!({"code": "forbidden", "message": "forbidden"})
    );
    let unsupported = UpdateHostError::from(HostError::unsupported("no instance"));
    assert_eq!(unsupported.code, UpdateErrorCode::Unsupported);
    assert_eq!(
        UpdateHostError::from(HostError::other("anything")).message,
        UpdateMessage::Unknown
    );
}

// ---- eligibility: platform, tier, licence ----------------------------------------------------

#[test]
fn a_release_for_another_platform_is_not_offered() {
    let r = rig_with(
        UpdatePolicy::new(hosts()),
        Some(release(UpdateKind::Update, "plan9-mips", Some("pro"))),
    );
    let view = r.host.check().unwrap();
    assert!(!view.available && !view.eligible && view.handle_id.is_none());
    assert_eq!(view.refusal, UpdateRefusal::None);
    assert_eq!(r.host.status().state, UpdateState::Idle);
}

#[test]
fn a_tier_the_app_does_not_allow_is_offered_but_not_eligible() {
    let r = rig_with(
        UpdatePolicy::new(hosts()).allowed_tiers(["pro"]),
        Some(release(
            UpdateKind::Update,
            &current_platform(),
            Some("free"),
        )),
    );
    let view = r.host.check().unwrap();
    assert!(view.available && !view.eligible);
    assert_eq!(view.refusal, UpdateRefusal::TierIneligible);
    assert!(view.handle_id.is_none());
    // A release with no tier at all is not in an allow-list either.
    let r = rig_with(
        UpdatePolicy::new(hosts()).allowed_tiers(["pro"]),
        Some(release(UpdateKind::Update, &current_platform(), None)),
    );
    assert_eq!(
        r.host.check().unwrap().refusal,
        UpdateRefusal::TierIneligible
    );
    // The allowed tier is eligible.
    let r = rig_with(
        UpdatePolicy::new(hosts()).allowed_tiers(["pro"]),
        Some(release(
            UpdateKind::Update,
            &current_platform(),
            Some("pro"),
        )),
    );
    assert!(r.host.check().unwrap().eligible);
}

#[test]
fn an_update_needs_a_licence_but_a_patch_does_not() {
    let r = rig(UpdateKind::Update);
    r.world.licensed.store(false, Ordering::SeqCst);
    let view = r.host.check().unwrap();
    assert!(view.available && !view.eligible);
    assert_eq!(view.refusal, UpdateRefusal::NotLicensed);
    assert!(view.handle_id.is_none(), "no handle for a refused update");

    let r = rig(UpdateKind::Patch);
    r.world.licensed.store(false, Ordering::SeqCst);
    let view = r.host.check().unwrap();
    assert!(view.eligible && view.handle_id.is_some());
    assert_eq!(view.kind, Some(UpdateKind::Patch));
    let id = view.handle_id.unwrap();
    assert_eq!(
        r.host.download(&id, &no_progress()).unwrap().state,
        DownloadState::Downloaded
    );
    assert_eq!(r.host.install(&id).unwrap().state, InstallState::Installing);
}

#[test]
fn no_release_is_not_available_and_not_an_error() {
    let r = rig_with(UpdatePolicy::new(hosts()), None);
    let view = r.host.check().unwrap();
    assert!(!view.available && view.refusal == UpdateRefusal::None);
}

// ---- download, progress, cancel --------------------------------------------------------------

#[test]
fn progress_is_bounded_ordered_and_ends_at_the_total() {
    let r = rig(UpdateKind::Update);
    let id = r.host.check().unwrap().handle_id.unwrap();
    let events = Mutex::new(Vec::<UpdateProgress>::new());
    let result = r
        .host
        .download(&id, &|p| events.lock().unwrap().push(p.clone()))
        .unwrap();
    assert_eq!(result.state, DownloadState::Downloaded);
    let events = events.into_inner().unwrap();
    // Four instant chunks: the throttle admits the first and the last, not every one.
    assert!((2..=4).contains(&events.len()), "{events:?}");
    assert!(events.windows(2).all(|w| w[0].received <= w[1].received));
    assert!(events
        .iter()
        .all(|e| e.handle_id == id && e.received <= e.total.unwrap()));
    assert_eq!(events.last().unwrap().received, 400);
    assert_eq!(r.host.status().state, UpdateState::Downloaded);
}

#[test]
fn the_throttle_is_time_bounded_never_regresses_and_always_sends_the_last() {
    let mut t = ProgressThrottle::new(Duration::from_millis(100));
    let t0 = std::time::Instant::now();
    assert!(t.admit(t0, 10, Some(1000)), "the first reading goes");
    assert!(!t.admit(t0 + Duration::from_millis(10), 20, Some(1000)));
    assert!(t.admit(t0 + Duration::from_millis(150), 300, Some(1000)));
    assert!(
        !t.admit(t0 + Duration::from_millis(400), 100, Some(1000)),
        "never backwards"
    );
    assert!(
        t.admit(t0 + Duration::from_millis(401), 1000, Some(1000)),
        "the last reading always goes"
    );
    // Unknown totals are throttled by time alone.
    let mut u = ProgressThrottle::new(Duration::from_millis(100));
    assert!(u.admit(t0, 1, None));
    assert!(!u.admit(t0 + Duration::from_millis(1), 2, None));
    // The payload clamps received to the total.
    assert_eq!(UpdateProgress::new("h", 900, Some(500)).received, 500);
}

#[test]
fn a_download_can_be_cancelled_and_started_again() {
    let r = rig(UpdateKind::Update);
    let id = r.host.check().unwrap().handle_id.unwrap();
    let hook_host = Arc::clone(&r.host);
    let hook_id = id.clone();
    let seen = Arc::new(Mutex::new(None::<CancelResult>));
    let sink = Arc::clone(&seen);
    *r.transport.mid_download.lock().unwrap() = Some(Box::new(move || {
        *sink.lock().unwrap() = Some(hook_host.cancel(&hook_id));
    }));
    let result = r.host.download(&id, &no_progress()).unwrap();
    assert_eq!(result.state, DownloadState::Cancelled);
    assert_eq!(
        *seen.lock().unwrap(),
        Some(CancelResult { cancelled: true })
    );
    assert_eq!(
        r.host.status().state,
        UpdateState::Available,
        "cancelled is not failed"
    );
    // Nothing is downloading now, so a cancel is a no-op.
    assert!(!r.host.cancel(&id).cancelled);
    // And the same handle downloads again.
    *r.transport.mid_download.lock().unwrap() = None;
    assert_eq!(
        r.host.download(&id, &no_progress()).unwrap().state,
        DownloadState::Downloaded
    );
}

#[test]
fn the_flow_is_serialised_a_second_operation_is_busy() {
    let r = rig(UpdateKind::Update);
    let id = r.host.check().unwrap().handle_id.unwrap();
    let busy = Arc::new(Mutex::new(Vec::<UpdateErrorCode>::new()));
    let (host, sink, hid) = (Arc::clone(&r.host), Arc::clone(&busy), id.clone());
    *r.transport.mid_download.lock().unwrap() = Some(Box::new(move || {
        let mut codes = sink.lock().unwrap();
        codes.push(host.check().unwrap_err().code);
        codes.push(host.download(&hid, &|_| {}).unwrap_err().code);
        codes.push(host.install(&hid).unwrap_err().code);
        // Status stays answerable and says what is happening.
        assert_eq!(host.status().state, UpdateState::Downloading);
    }));
    r.host.download(&id, &no_progress()).unwrap();
    assert_eq!(*busy.lock().unwrap(), [UpdateErrorCode::Busy; 3]);
}

// ---- the licence binding ---------------------------------------------------------------------

#[test]
fn a_deactivation_during_the_download_refuses_it_and_the_bytes_are_not_kept() {
    let r = rig(UpdateKind::Update);
    let id = r.host.check().unwrap().handle_id.unwrap();
    let world = Arc::clone(&r.world);
    *r.transport.mid_download.lock().unwrap() = Some(Box::new(move || {
        world.licensed.store(false, Ordering::SeqCst);
    }));
    let result = r.host.download(&id, &no_progress()).unwrap();
    assert_eq!(result.state, DownloadState::Refused);
    assert_eq!(result.reason, Some(UpdateRefusal::Revoked));
    assert_eq!(result.message, UpdateMessage::Revoked);
    assert_eq!(r.host.status().state, UpdateState::Failed);
    let install = r.host.install(&id).unwrap();
    assert_eq!(install.state, InstallState::Refused);
    assert_eq!(install.reason, Some(InstallReason::Stale));
    assert_eq!(r.transport.installed.load(Ordering::SeqCst), 0);
}

#[test]
fn a_licence_lost_after_the_last_byte_still_refuses_the_download() {
    // The transport finished without another request, so only the end-of-download check sees it.
    let r = rig(UpdateKind::Update);
    let id = r.host.check().unwrap().handle_id.unwrap();
    let world = Arc::clone(&r.world);
    // Flip the licence from the progress callback of the LAST chunk: the transport makes no
    // request after it, so only the end-of-download check can notice.
    let result = r
        .host
        .download(&id, &move |p| {
            if p.received == p.total.unwrap() {
                world.licensed.store(false, Ordering::SeqCst);
            }
        })
        .unwrap();
    assert_eq!(result.state, DownloadState::Refused, "{result:?}");
    assert_eq!(result.reason, Some(UpdateRefusal::Revoked));
}

#[test]
fn a_changed_licence_revision_at_install_refuses_it_and_a_restored_one_installs() {
    let r = rig(UpdateKind::Update);
    let id = downloaded(&r);
    // Deactivated, then re-activated under a NEW revision: the update began with the old one.
    r.world.licensed.store(false, Ordering::SeqCst);
    let refused = r.host.install(&id).unwrap();
    assert_eq!(refused.state, InstallState::Refused);
    assert_eq!(refused.reason, Some(InstallReason::Revoked));
    r.world.licensed.store(true, Ordering::SeqCst);
    r.world.revision.fetch_add(1, Ordering::SeqCst);
    let refused = r.host.install(&id).unwrap();
    assert_eq!(refused.reason, Some(InstallReason::NotLicensed));
    assert_eq!(r.transport.installed.load(Ordering::SeqCst), 0);
    // Back to the revision the update began with: the staged bytes were kept, and it installs.
    r.world.revision.fetch_sub(1, Ordering::SeqCst);
    let ok = r.host.install(&id).unwrap();
    assert_eq!(ok.state, InstallState::Installing, "{ok:?}");
    assert_eq!(r.transport.installed.load(Ordering::SeqCst), 1);
}

#[test]
fn a_licence_that_changes_before_the_download_starts_refuses_it() {
    let r = rig(UpdateKind::Update);
    let id = r.host.check().unwrap().handle_id.unwrap();
    r.world.revision.fetch_add(1, Ordering::SeqCst);
    let result = r.host.download(&id, &no_progress()).unwrap();
    assert_eq!(result.state, DownloadState::Refused);
    assert_eq!(result.reason, Some(UpdateRefusal::NotLicensed));
}

// ---- redirects -------------------------------------------------------------------------------

#[test]
fn a_redirect_outside_the_compiled_allowlist_gets_no_headers_and_never_reaches_the_authority() {
    let r = rig(UpdateKind::Update);
    r.transport.follow_redirect.store(true, Ordering::SeqCst);
    downloaded(&r);
    assert!(r.transport.redirect_refused.load(Ordering::SeqCst));
    let urls = r.world.authorized_urls.lock().unwrap().clone();
    assert!(!urls.is_empty());
    assert!(
        urls.iter().all(|u| !u.contains("evil.example.net")),
        "the authority was asked about the foreign host: {urls:?}"
    );
}

#[test]
fn the_host_allowlist_is_https_only_exact_and_free_of_credentials() {
    let p = hosts();
    let ok = |u: &str| p.allows(&Url::parse(u).unwrap());
    assert!(ok("https://updates.example.com/latest.json"));
    assert!(ok("https://dl.example.com/downloads/app/x.zip"));
    assert!(
        ok("https://DL.example.com/x"),
        "the URL parser folds the host case"
    );
    for bad in [
        "http://dl.example.com/x",
        "https://user@dl.example.com/x",
        "https://user:pw@dl.example.com/x",
        "https://dl.example.com:8443/x",
        "https://dl.example.com.evil.net/x",
        "https://evil.dl.example.com/x",
        "https://example.com/x",
        "ftp://dl.example.com/x",
    ] {
        assert!(!ok(bad), "{bad}");
    }
    for bad in [
        vec!["Updates.Example.com"],
        vec!["https://a.example.com"],
        vec![""],
        vec!["a b.com"],
    ] {
        assert!(UpdateHostPolicy::new(bad, ["dl.example.com"]).is_err());
    }
    assert!(UpdateHostPolicy::new(Vec::<String>::new(), ["dl.example.com"]).is_err());
    assert!(UpdateHostPolicy::new(["a.example.com"], Vec::<String>::new()).is_err());
}

// ---- handles ---------------------------------------------------------------------------------

#[test]
fn a_new_check_makes_every_older_handle_stale() {
    let r = rig(UpdateKind::Update);
    let old = r.host.check().unwrap().handle_id.unwrap();
    let new = r.host.check().unwrap().handle_id.unwrap();
    assert_ne!(old, new);
    let download = r.host.download(&old, &no_progress()).unwrap();
    assert_eq!(download.state, DownloadState::Failed);
    let install = r.host.install(&old).unwrap();
    assert_eq!(
        (install.state, install.reason),
        (InstallState::Refused, Some(InstallReason::Stale))
    );
    assert_eq!(
        r.host.download(&new, &no_progress()).unwrap().state,
        DownloadState::Downloaded
    );
    // A made-up handle is the same as a stale one.
    assert_eq!(
        r.host
            .install("0123456789abcdef0123456789abcdef")
            .unwrap()
            .reason,
        Some(InstallReason::Stale)
    );
}

#[test]
fn a_handle_expires() {
    let r = rig_with(
        UpdatePolicy::new(hosts()).handle_ttl(Duration::from_millis(40)),
        Some(release(
            UpdateKind::Update,
            &current_platform(),
            Some("pro"),
        )),
    );
    let id = r.host.check().unwrap().handle_id.unwrap();
    std::thread::sleep(Duration::from_millis(90));
    assert_eq!(
        r.host.download(&id, &no_progress()).unwrap().state,
        DownloadState::Failed
    );
    assert_eq!(r.host.status().state, UpdateState::Idle);
}

#[test]
fn handles_are_bounded() {
    let r = rig(UpdateKind::Update);
    // Each check clears the table and makes one handle, so the table never grows past the bound.
    for _ in 0..10 {
        r.host.check().unwrap();
    }
    assert!(UpdateHost::new(
        UpdatePolicy {
            max_handles: 0,
            ..UpdatePolicy::new(hosts())
        },
        FakeAuthority(World::new()),
        Shared(FakeTransport::new(None)),
    )
    .is_err());
}

// ---- staging identity and install ------------------------------------------------------------

#[test]
fn a_changed_staging_identity_refuses_the_install() {
    let r = rig(UpdateKind::Update);
    let id = downloaded(&r);
    // Someone replaced the staged file after it was verified.
    *r.transport.staged.lock().unwrap() = identity("bb");
    let install = r.host.install(&id).unwrap();
    assert_eq!(install.state, InstallState::Refused);
    assert_eq!(install.reason, Some(InstallReason::Stale));
    assert_eq!(install.message, UpdateMessage::VerifyFailed);
    assert_eq!(r.transport.installed.load(Ordering::SeqCst), 0);
    assert_eq!(r.host.status().state, UpdateState::Failed);
    // The tampered bytes are gone: restoring the old digest does not bring the handle back.
    *r.transport.staged.lock().unwrap() = identity("aa");
    assert_eq!(
        r.host.install(&id).unwrap().reason,
        Some(InstallReason::Stale)
    );
}

#[test]
fn an_installer_that_is_interrupted_fails_and_the_handle_is_spent() {
    let r = rig(UpdateKind::Update);
    let id = downloaded(&r);
    *r.transport.install_result.lock().unwrap() = Err(TransportError::Interrupted);
    let install = r.host.install(&id).unwrap();
    assert_eq!(install.state, InstallState::Failed);
    assert_eq!(install.message, UpdateMessage::UpdateFailed);
    let status = r.host.status();
    assert_eq!(status.state, UpdateState::Failed);
    assert_eq!(status.message, UpdateMessage::UpdateFailed);
    // It cannot be retried blindly: the next try must be a fresh check.
    let again = r.host.install(&id).unwrap();
    assert_eq!(
        (again.state, again.reason),
        (InstallState::Refused, Some(InstallReason::Consumed))
    );
    assert_eq!(r.transport.installed.load(Ordering::SeqCst), 1);
    // And a fresh check starts clean.
    assert!(r.host.check().unwrap().eligible);
}

#[test]
fn a_handle_installs_at_most_once() {
    let r = rig(UpdateKind::Update);
    let id = downloaded(&r);
    let first = r.host.install(&id).unwrap();
    assert_eq!(
        (first.state, first.reason),
        (InstallState::Installing, None)
    );
    assert_eq!(r.host.status().state, UpdateState::Installing);
    // The installer is running: nothing else may start.
    assert_eq!(r.host.check().unwrap_err().code, UpdateErrorCode::Busy);
    let second = r.host.install(&id).unwrap_err();
    assert_eq!(second.code, UpdateErrorCode::Busy);
    assert_eq!(r.transport.installed.load(Ordering::SeqCst), 1);
}

#[test]
fn install_before_download_is_refused() {
    let r = rig(UpdateKind::Update);
    let id = r.host.check().unwrap().handle_id.unwrap();
    let install = r.host.install(&id).unwrap();
    assert_eq!(
        (install.state, install.reason),
        (InstallState::Refused, Some(InstallReason::Stale))
    );
    assert_eq!(r.transport.installed.load(Ordering::SeqCst), 0);
}

#[test]
fn the_status_follows_the_flow() {
    let r = rig(UpdateKind::Update);
    assert_eq!(r.host.status().state, UpdateState::Idle);
    let view = r.host.check().unwrap();
    let id = view.handle_id.unwrap();
    let s = r.host.status();
    assert_eq!(
        (s.state, s.handle_id.as_deref(), s.version.as_deref()),
        (UpdateState::Available, Some(id.as_str()), Some("2.0.0"))
    );
    assert_eq!(s.kind, Some(UpdateKind::Update));
    r.host.download(&id, &no_progress()).unwrap();
    assert_eq!(r.host.status().state, UpdateState::Downloaded);
}

// ---- the caller -----------------------------------------------------------------------------

fn facts<'a>(
    label: &'a str,
    content: &'a [String],
    url: &'a Url,
    nav: &'a NavPolicy,
) -> CallerFacts<'a> {
    CallerFacts {
        label,
        is_app_window: true,
        content_labels: content,
        url: Some(url),
        nav,
    }
}

#[test]
fn only_a_trusted_app_window_reaches_the_update_host() {
    let r = rig(UpdateKind::Update);
    let services = HostServices::builder()
        .update(
            ServiceTrust::for_labels(["main"]),
            Arc::try_unwrap(r.host).ok().unwrap(),
        )
        .build()
        .unwrap();
    let nav = NavPolicy::default();
    let app = Url::parse("tauri://localhost/").unwrap();
    let foreign = Url::parse("https://evil.example.net/").unwrap();
    let content = vec!["viewer".to_owned()];
    for action in ["check", "status", "download", "install", "cancel"] {
        assert!(update_host_for(&services, action, &facts("main", &[], &app, &nav)).is_ok());
        for (label, url, content) in [
            ("settings", &app, &[][..]), // an app window the service does not trust
            ("main", &foreign, &[][..]), // a foreign page in the trusted window
            ("viewer", &app, &content[..]), // a registered content webview
        ] {
            let denied = update_host_for(&services, action, &facts(label, content, url, &nav))
                .err()
                .unwrap_or_else(|| panic!("{label} reached {action}"));
            assert_eq!(denied.code, "permission_denied");
            assert_eq!(
                UpdateHostError::from(denied),
                UpdateHostError::new(UpdateErrorCode::Forbidden, UpdateMessage::Forbidden)
            );
        }
    }
    // A caller that is not an app window at all.
    let mut not_window = facts("main", &[], &app, &nav);
    not_window.is_app_window = false;
    assert!(update_host_for(&services, "check", &not_window).is_err());
    // A host with no update service registered refuses everyone.
    let empty = HostServices::builder().build().unwrap();
    assert!(update_host_for(&empty, "check", &facts("main", &[], &app, &nav)).is_err());
}

#[test]
fn only_the_enabled_actions_run() {
    let r = rig(UpdateKind::Update);
    let services = HostServices::builder()
        .update(
            ServiceTrust::for_labels(["main"]).with_actions(["status"]),
            Arc::try_unwrap(r.host).ok().unwrap(),
        )
        .build()
        .unwrap();
    let nav = NavPolicy::default();
    let app = Url::parse("tauri://localhost/").unwrap();
    assert!(update_host_for(&services, "status", &facts("main", &[], &app, &nav)).is_ok());
    assert!(update_host_for(&services, "install", &facts("main", &[], &app, &nav)).is_err());
}

#[test]
fn the_current_platform_names_an_os_and_an_architecture() {
    let p = current_platform();
    let (os, arch) = p.split_once('-').expect("os-arch");
    assert!(["windows", "darwin", "linux"].contains(&os) || !os.is_empty());
    assert!(!arch.is_empty());
}
