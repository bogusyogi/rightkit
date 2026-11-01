//! Standalone HTTP workspace surface over the existing per-session browser
//! store (`rightkit_agent_tools::coding_tools::browser::BrowserStore`).
//!
//! Lane discipline (D3 — shared browser workspace):
//!
//! - This file owns `pub fn browser_workspace_routes(services)` and the
//!   `browser_workspace_openapi_*` helpers. The central router in
//!   `api_router.rs` and the central OpenAPI component assembly in
//!   `api_contract/*` are out of scope; Agent 4 wires the standalone
//!   fragment in by calling
//!   `browser_workspace_routes(services).with_state(state)` and merging
//!   `browser_workspace_openapi_components()`.
//!
//! - All session-affecting state lives behind the `BrowserWorkspaceServices`
//!   trait Agent 4 implements. The fragment itself holds the per-RightKit-
//!   session annotation store and the route handlers; the trait carries the
//!   `BrowserStore`, host policy, action log, pause flag, profile kind/label,
//!   viewport, capability gate, and the dispatch seam into the live agent.
//!   Agent 4's implementation is what reaches into `LiveSession::browser_*`,
//!   not this file.
//!
//! - Security boundaries preserved by the fragment's contract (Agent 4's
//!   implementation MUST honor these when wiring the trait impls):
//!     * SSRF guard (`rightkit_agent_tools::coding_tools::browser_fetch::ssrf_guard_reject_reason`)
//!       runs before any navigation. Host policy is additive on top.
//!     * Browser capability gate runs before any state mutation.
//!     * Page text and console output are wrapped via
//!       `rightkit_agent_tools::coding_tools::browser_fetch::wrap_untrusted_web_text`.
//!     * Browser profiles are ephemeral by default; named shared profiles
//!       require the operator to flip `profile_kind` through the consent
//!       endpoint below.
//!     * Page text is bounded by `MAX_TEXT_BYTES`, screenshots by
//!       `MAX_SCREENSHOT_BYTES`, console by RightKit's own 500-line cap.
//!     * Action log, host policy, pause, viewport, profile state all
//!       live with the session and are dropped with it.
//!
//! - Tests in this file are deterministic where they exercise pure
//!   shape/decision logic and gated on `RIGHTKIT_BROWSER_WORKSPACE_TESTS=1`
//!   for anything that would require a running daemon or live Chrome.
//!
//! ## Decomposition (ra-020)
//!
//! This module is a pure structural facade over six private submodules —
//! `types`, `policy`, `services`, `routes`, `openapi`, `annotations` — split
//! out of a single 3,325-line file per the Architect decomposition map
//! (`.audit/20260901-daemon-deep-r3/architect/decomposition-plans.md`). The
//! route inventory, `BrowserWorkspaceServices` trait, JSON wire shapes,
//! OpenAPI schemas, and lease/authority semantics (SSRF guard, capability
//! gate, `WORKSPACE_USER_OWNER` lease-owner contract) are unchanged — only
//! *where the code lives* changed. Every symbol this module re-exports below
//! is byte-identical in name and signature to the pre-decomposition file;
//! the four external call sites (`api_router.rs`, `api_contract/document.rs`,
//! `api_contract/request_schemas.rs`, `api_contract/response_schemas.rs`)
//! require no changes.

mod annotations;
mod openapi;
mod policy;
mod routes;
mod services;
mod types;

#[cfg(test)]
pub use routes::browser_workspace_routes;
pub use services::daemon_browser_workspace_routes;
#[cfg(test)]
pub use services::{BrowserWorkspaceServices, BrowserWorkspaceState};
#[cfg(test)]
pub use types::{
    BrowserActionActor, BrowserActionEntry, BrowserProfileKind, CapabilityVerdict, ConsoleLineView,
    HostPolicyDecision, StoredAnnotation, ViewportInfo,
};

pub use openapi::{
    browser_workspace_openapi_components, browser_workspace_openapi_request_schema,
    browser_workspace_openapi_response_schema,
};

// =============================================================================
// Tests (deterministic + gated-live). Not run in this lane — Agent 4 owns
// test execution. The `#[cfg(test)] mod tests` block here documents the
// deterministic unit checks; the gated-live block exercises the full
// workspace surface against a real Chrome process.
// =============================================================================

#[cfg(test)]
mod tests {
    use super::annotations::render_annotation_message;
    use super::policy::{browser_capability_verdict, workspace_ssrf_check};
    use super::routes::probe_loopback;
    use super::types::{png_dimensions, WorkspaceErrorBody, WORKSPACE_USER_OWNER};
    use super::*;
    use axum::http::StatusCode;
    use rightkit_agent_policy::PermissionMode;
    use serde_json::{json, Value};
    use tower::ServiceExt;

    fn make_entry(kind: &str, actor: BrowserActionActor) -> BrowserActionEntry {
        BrowserActionEntry {
            at_ms: 1234,
            actor,
            kind: kind.into(),
            detail: "detail".into(),
        }
    }

    fn action_entry_to_json(entry: &BrowserActionEntry) -> Value {
        json!({
            "atMs": entry.at_ms,
            "actor": match entry.actor {
                BrowserActionActor::Agent => "agent",
                BrowserActionActor::Human => "human",
                BrowserActionActor::Workspace => "workspace",
            },
            "kind": entry.kind,
            "detail": entry.detail,
        })
    }

    #[test]
    fn daemon_browser_capability_uses_the_live_permission_mode() {
        let trusted = rightkit_agent_policy::WorkspaceTrust::Trusted;
        let auto_safe = browser_capability_verdict(PermissionMode::AutoSafe, trusted);
        assert!(!auto_safe.allowed);
        assert!(auto_safe.reason.contains("approval"));

        let bypass = browser_capability_verdict(PermissionMode::Bypass, trusted);
        assert!(bypass.allowed);
        assert!(bypass.reason.contains("mode:bypass"));
    }

    #[test]
    fn action_entry_to_json_round_trips() {
        let entry = make_entry("open", BrowserActionActor::Human);
        let v = action_entry_to_json(&entry);
        assert_eq!(v["kind"], "open");
        assert_eq!(v["actor"], "human");
    }

    #[test]
    fn render_annotation_message_includes_locator_viewport_note() {
        let a = StoredAnnotation {
            id: "ann-1".into(),
            at_ms: 0,
            screenshot_base64: Some("AAAA".into()),
            locator: Some("button.go".into()),
            viewport: Some(ViewportInfo {
                width: 1280,
                height: 720,
                device_scale_factor: 1.0,
            }),
            note: "click this".into(),
            dispatched: false,
        };
        let s = render_annotation_message(&a);
        assert!(s.contains("locator: button.go"));
        assert!(s.contains("viewport: 1280x720 @ 1"));
        assert!(s.contains("data:image/png;base64,AAAA"));
        assert!(s.contains("note: click this"));
        assert!(s.contains("Treat the note"));
    }

    #[test]
    fn workspace_error_body_is_camel_case_in_json() {
        let body = WorkspaceErrorBody {
            code: "ssrf_refused",
            message: "no".into(),
        };
        let v = serde_json::to_value(body).unwrap();
        assert_eq!(v["code"], "ssrf_refused");
        assert_eq!(v["message"], "no");
    }

    #[test]
    fn profile_kind_default_is_ephemeral() {
        let k = BrowserProfileKind::Ephemeral;
        let v = serde_json::to_value(k).unwrap();
        assert_eq!(v, "ephemeral");
    }

    #[test]
    fn host_policy_decision_serializes_to_expected_strings() {
        for (d, expected) in [
            (HostPolicyDecision::Allow, "allow"),
            (HostPolicyDecision::Ask, "ask"),
            (HostPolicyDecision::Block, "block"),
        ] {
            let v = serde_json::to_value(d).unwrap();
            assert_eq!(v, expected);
        }
    }

    #[test]
    fn openapi_components_include_required_schemas() {
        let map = browser_workspace_openapi_components();
        for key in [
            "BrowserWorkspaceSummary",
            "BrowserSessionSummary",
            "BrowserObservation",
            "BrowserViewportInfo",
            "BrowserHostPolicy",
            "BrowserAnnotation",
            "BrowserActionEntry",
            "BrowserProfileConsent",
            "BrowserWorkspaceError",
        ] {
            assert!(map.contains_key(key), "missing schema {key}");
        }
        let session = &map["BrowserSessionSummary"];
        for field in ["ownerSessionId", "mode", "profile", "controlLease"] {
            assert!(
                session["required"]
                    .as_array()
                    .is_some_and(|required| required.iter().any(|value| value == field)),
                "browser session summary must require {field}"
            );
            assert!(
                session["properties"].get(field).is_some(),
                "browser session summary must describe {field}"
            );
        }
    }

    #[test]
    fn request_schema_lookup_returns_correct_refs() {
        assert!(browser_workspace_openapi_request_schema(
            "post",
            "/sessions/{id}/browser/workspace/open"
        )
        .is_some());
        assert!(browser_workspace_openapi_request_schema(
            "post",
            "/sessions/{id}/browser/workspace/zoom"
        )
        .is_some());
        assert!(browser_workspace_openapi_request_schema(
            "post",
            "/sessions/{id}/browser/workspace/close"
        )
        .is_some());
    }

    #[test]
    fn response_schema_lookup_returns_correct_refs() {
        let s =
            browser_workspace_openapi_response_schema("get", "/sessions/{id}/browser/workspace");
        assert!(s.is_some());
        let navigate = browser_workspace_openapi_response_schema(
            "post",
            "/sessions/{id}/browser/workspace/navigate",
        )
        .unwrap();
        assert_eq!(navigate["required"], json!(["navigated", "observation"]));
    }

    #[test]
    fn ssrf_check_rejects_metadata_endpoint() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = runtime.block_on(workspace_ssrf_check(
            "http://169.254.169.254/latest/meta-data/",
            &rightkit_agent_tools::coding_tools::browser::BrowserNavigationTrustV1::default(),
            false,
        ));
        assert!(matches!(err, Err((StatusCode::FORBIDDEN, _))));
    }

    #[test]
    fn ssrf_check_rejects_empty_url() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = runtime.block_on(workspace_ssrf_check(
            "",
            &rightkit_agent_tools::coding_tools::browser::BrowserNavigationTrustV1::default(),
            false,
        ));
        assert!(matches!(err, Err((StatusCode::BAD_REQUEST, _))));
    }

    #[test]
    fn ssrf_check_allows_loopback() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let res = runtime.block_on(workspace_ssrf_check(
            "http://127.0.0.1:5173/",
            &rightkit_agent_tools::coding_tools::browser::BrowserNavigationTrustV1::default(),
            false,
        ));
        assert!(res.is_ok());
    }

    #[test]
    fn fragment_state_starts_empty() {
        let _s = BrowserWorkspaceState::new();
        // The fragment owns only the annotation store; the action log lives
        // on `LiveSession` behind the `BrowserWorkspaceServices` trait. This
        // test asserts the annotation store starts empty, which is what the
        // fragment's contract guarantees.
    }

    #[tokio::test]
    async fn control_transition_lock_is_shared_across_route_state_clones() {
        let state = BrowserWorkspaceState::new();
        let clone = state.clone();
        let first = state.control_transition_lock();
        let second = clone.control_transition_lock();
        assert!(std::sync::Arc::ptr_eq(&first, &second));

        let guard = first.lock().await;
        let (started, mut started_rx) = tokio::sync::oneshot::channel();
        let waiter = tokio::spawn(async move {
            let _guard = second.lock().await;
            started.send(()).unwrap();
        });
        tokio::task::yield_now().await;
        assert!(started_rx.try_recv().is_err());
        drop(guard);
        started_rx.await.unwrap();
        waiter.await.unwrap();
    }

    #[tokio::test]
    async fn annotation_store_round_trip() {
        let s = BrowserWorkspaceState::new();
        let stored = StoredAnnotation {
            id: "ann-1".into(),
            at_ms: 0,
            screenshot_base64: None,
            locator: Some("button".into()),
            viewport: None,
            note: "x".into(),
            dispatched: false,
        };
        {
            let store = s.annotations();
            let mut map = store.lock().await;
            map.entry("s1".into()).or_default().push(stored.clone());
        }
        let store = s.annotations();
        let map = store.lock().await;
        assert_eq!(map.get("s1").unwrap().len(), 1);
        assert_eq!(map.get("s1").unwrap()[0].locator.as_deref(), Some("button"));
    }

    // ── Deterministic stub of BrowserWorkspaceServices for routing tests ───

    use rightkit_agent_tools::coding_tools::browser::{BrowserObservationV1, BrowserStore, SemanticBoundsV1};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn stub_observation() -> BrowserObservationV1 {
        BrowserObservationV1 {
            schema_version: 1,
            browser_session_id: "bs-stub".into(),
            owner_session_id: "s1".into(),
            tab_id: "tab-stub".into(),
            frame_ids: vec!["main".into()],
            epoch: 7,
            url: "http://127.0.0.1:5173/".into(),
            title: "stub".into(),
            viewport: SemanticBoundsV1 {
                x: 0.0,
                y: 0.0,
                width: 1280.0,
                height: 720.0,
            },
            ax_nodes: Vec::new(),
            dom_fallback: Vec::new(),
            diff: Default::default(),
            screenshot_artifact: None,
            console_errors: 0,
        }
    }

    struct StubServices {
        paused: AtomicUsize,
        profile_kind: BrowserProfileKind,
        block_url: Option<String>,
        ask_url: Option<String>,
        action_log_len: AtomicUsize,
        capability_allowed: bool,
        viewport: Option<ViewportInfo>,
    }

    impl StubServices {
        fn new() -> Self {
            Self {
                paused: AtomicUsize::new(0),
                profile_kind: BrowserProfileKind::Ephemeral,
                block_url: None,
                ask_url: None,
                action_log_len: AtomicUsize::new(0),
                capability_allowed: true,
                viewport: None,
            }
        }
    }

    impl BrowserWorkspaceServices for StubServices {
        fn capability_gate(&self, _session_id: &str) -> CapabilityVerdict {
            CapabilityVerdict {
                allowed: self.capability_allowed,
                reason: if self.capability_allowed {
                    "ok".into()
                } else {
                    "denied".into()
                },
            }
        }

        fn browser_store(&self, _session_id: &str) -> Option<BrowserStore> {
            Some(Arc::new(Mutex::new(HashMap::new())))
        }

        fn host_policy_decision(&self, _session_id: &str, url: &str) -> Option<HostPolicyDecision> {
            self.block_url
                .as_deref()
                .filter(|blocked| url.contains(blocked))
                .map(|_| HostPolicyDecision::Block)
                .or_else(|| {
                    self.ask_url
                        .as_deref()
                        .filter(|asked| url.contains(asked))
                        .map(|_| HostPolicyDecision::Ask)
                })
        }

        fn action_log_len(&self, _session_id: &str) -> usize {
            self.action_log_len.load(AtomicOrdering::Relaxed)
        }

        fn is_paused(&self, _session_id: &str) -> bool {
            self.paused.load(AtomicOrdering::Relaxed) != 0
        }

        async fn set_paused(&self, _session_id: &str, paused: bool) {
            self.paused
                .store(usize::from(paused), AtomicOrdering::Relaxed);
        }

        fn profile_kind(&self, _session_id: &str) -> BrowserProfileKind {
            self.profile_kind
        }

        fn profile_label(&self, _session_id: &str) -> Option<String> {
            None
        }

        fn viewport(&self, _session_id: &str) -> Option<ViewportInfo> {
            self.viewport.clone()
        }

        fn push_action(&self, _session_id: &str, _entry: BrowserActionEntry) {
            self.action_log_len.fetch_add(1, AtomicOrdering::Relaxed);
        }

        async fn open_browser_session(
            &self,
            _session_id: &str,
            _url: &str,
        ) -> Result<String, String> {
            Ok("bs-stub".into())
        }

        async fn close_browser_session(
            &self,
            _session_id: &str,
            _browser_session_id: &str,
        ) -> Result<(), String> {
            Ok(())
        }

        async fn navigate_browser_session_with_observation(
            &self,
            _session_id: &str,
            _browser_session_id: &str,
            _url: &str,
        ) -> Result<BrowserObservationV1, String> {
            Ok(stub_observation())
        }

        async fn reload_browser_session_with_observation(
            &self,
            _session_id: &str,
            _browser_session_id: &str,
        ) -> Result<BrowserObservationV1, String> {
            Ok(stub_observation())
        }

        async fn navigate_browser_history_with_observation(
            &self,
            _session_id: &str,
            _browser_session_id: &str,
            _forward: bool,
        ) -> Result<BrowserObservationV1, String> {
            Ok(stub_observation())
        }

        async fn apply_viewport(
            &self,
            _session_id: &str,
            _viewport: &ViewportInfo,
        ) -> Result<(), String> {
            Ok(())
        }

        async fn apply_zoom(&self, _session_id: &str, _zoom: f64) -> Result<(), String> {
            Ok(())
        }

        async fn browser_session_url(
            &self,
            _session_id: &str,
            _browser_session_id: &str,
        ) -> Result<String, String> {
            Ok("http://127.0.0.1:5173/".to_string())
        }

        async fn screenshot_browser_session(
            &self,
            _session_id: &str,
            _browser_session_id: &str,
        ) -> Result<Vec<u8>, String> {
            Ok(vec![0u8; 4])
        }

        async fn console_lines(
            &self,
            _session_id: &str,
            _browser_session_id: &str,
        ) -> Result<Vec<ConsoleLineView>, String> {
            Ok(vec![ConsoleLineView {
                level: "error".into(),
                text: "boom".into(),
            }])
        }

        async fn dispatch_annotation(
            &self,
            _session_id: &str,
            _annotation: StoredAnnotation,
        ) -> Result<u64, String> {
            Ok(42)
        }
    }

    #[tokio::test]
    async fn stub_router_serves_summary() {
        let services = Arc::new(StubServices::new());
        let state = BrowserWorkspaceState::new();
        let router = browser_workspace_routes(services, state);
        let resp = axum::http::Request::builder()
            .method("GET")
            .uri("/sessions/s1/browser/workspace")
            .body(axum::body::Body::empty())
            .unwrap();
        let res = router.oneshot(resp).await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK);
    }

    #[tokio::test]
    async fn action_route_returns_post_action_observation() {
        let services = Arc::new(StubServices::new());
        let state = BrowserWorkspaceState::new();
        let router = browser_workspace_routes(services, state);
        let body = serde_json::to_string(&serde_json::json!({
            "browserSessionId": "bs-stub",
            "url": "http://127.0.0.1:5173/"
        }))
        .unwrap();
        let resp = axum::http::Request::builder()
            .method("POST")
            .uri("/sessions/s1/browser/workspace/navigate")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body))
            .unwrap();
        let res = router.oneshot(resp).await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK);
        let payload: Value = serde_json::from_slice(
            &axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(payload["observation"]["browserSessionId"], "bs-stub");
        assert_eq!(payload["observation"]["epoch"], 7);
    }

    #[tokio::test]
    async fn stub_router_blocks_metadata_url() {
        let services = Arc::new(StubServices::new());
        let state = BrowserWorkspaceState::new();
        let router = browser_workspace_routes(services, state);
        let body = serde_json::to_string(&serde_json::json!({
            "url": "http://169.254.169.254/latest/meta-data/"
        }))
        .unwrap();
        let resp = axum::http::Request::builder()
            .method("POST")
            .uri("/sessions/s1/browser/workspace/open")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body))
            .unwrap();
        let res = router.oneshot(resp).await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn stub_router_blocks_when_capability_denied() {
        let services = Arc::new(StubServices {
            capability_allowed: false,
            ..StubServices::new()
        });
        let state = BrowserWorkspaceState::new();
        let router = browser_workspace_routes(services, state);
        let resp = axum::http::Request::builder()
            .method("GET")
            .uri("/sessions/s1/browser/workspace")
            .body(axum::body::Body::empty())
            .unwrap();
        let res = router.oneshot(resp).await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn stub_router_refuses_unsupported_named_shared_profile() {
        let services = Arc::new(StubServices {
            profile_kind: BrowserProfileKind::Ephemeral,
            ..StubServices::new()
        });
        let state = BrowserWorkspaceState::new();
        let router = browser_workspace_routes(services, state);
        let body = serde_json::to_string(&serde_json::json!({
            "url": "http://127.0.0.1:5173/",
            "name": "shared"
        }))
        .unwrap();
        let resp = axum::http::Request::builder()
            .method("POST")
            .uri("/sessions/s1/browser/workspace/open")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body))
            .unwrap();
        let res = router.oneshot(resp).await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::NOT_IMPLEMENTED);
    }

    #[tokio::test]
    async fn stub_router_fails_closed_when_host_policy_requires_approval() {
        let services = Arc::new(StubServices {
            ask_url: Some("127.0.0.1".into()),
            ..StubServices::new()
        });
        let state = BrowserWorkspaceState::new();
        let router = browser_workspace_routes(services, state);
        let body = serde_json::to_string(&serde_json::json!({
            "url": "http://127.0.0.1:5173/"
        }))
        .unwrap();
        let resp = axum::http::Request::builder()
            .method("POST")
            .uri("/sessions/s1/browser/workspace/open")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body))
            .unwrap();
        let res = router.oneshot(resp).await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn loopback_probe_accepts_redirect_without_following_it() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let responder = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = socket.read(&mut request).await;
            socket
                .write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/\r\nContent-Length: 0\r\n\r\n",
                )
                .await
                .unwrap();
        });

        assert!(probe_loopback(&format!("http://{address}/")).await);
        responder.await.unwrap();
    }

    #[test]
    fn png_dimensions_reads_ihdr_dimensions() {
        let mut bytes = vec![0_u8; 24];
        bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        bytes[16..20].copy_from_slice(&1280_u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&720_u32.to_be_bytes());
        assert_eq!(png_dimensions(&bytes), Some((1280, 720)));
        assert_eq!(png_dimensions(b"not a png"), None);
    }

    /// The desktop lease strip (`BrowserPane`) only treats a session as
    /// human-controlled when `controlLease.owner === "workspace-user"`. If
    /// this constant drifts, the operator loses every human-lease affordance
    /// while the daemon keeps taking leases that no surface recognizes.
    #[test]
    fn workspace_user_owner_matches_desktop_lease_strip_contract() {
        assert_eq!(WORKSPACE_USER_OWNER, "workspace-user");
    }

    #[tokio::test]
    async fn take_and_release_control_report_human_control_transitions() {
        let services = Arc::new(StubServices::new());
        let state = BrowserWorkspaceState::new();
        let router = browser_workspace_routes(services, state);

        let resp = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/sessions/s1/browser/workspace/take-control")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let payload: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(payload["paused"], true);
        assert_eq!(payload["control"], "human");

        let resp = router
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/sessions/s1/browser/workspace/release-control")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let payload: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(payload["paused"], false);
        assert_eq!(payload["control"], "agent");
    }

    /// `pause`/`resume` must round-trip through the async `set_paused` seam —
    /// this is the transition the daemon impl also pushes into every live
    /// browser session, so a broken await here would silently strand the
    /// agent-side gate.
    #[tokio::test]
    async fn pause_and_resume_round_trip_through_set_paused() {
        let services = Arc::new(StubServices::new());
        let state = BrowserWorkspaceState::new();
        let router = browser_workspace_routes(services, state);

        let resp = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/sessions/s1/browser/workspace/pause")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let payload: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(payload["paused"], true);

        let resp = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri("/sessions/s1/browser/workspace")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let payload: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(payload["paused"], true);

        let resp = router
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/sessions/s1/browser/workspace/resume")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let payload: Value = serde_json::from_slice(
            &axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(payload["paused"], false);
    }
}
