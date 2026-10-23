#![cfg(feature = "update-license")]
//! The Right Suite configuration the shell builds from RightKit's API contract
//! (`rightkit_license::suite_api`): host allowlist per app, the updater feed, the release-status
//! read and the legacy-host refusals ported from CodeRight's `update_host.rs`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rightkit_tauri_shell::update::{
    parse_release_status, BindingToken, ReleaseInfo, ReleaseStatusView, RequestAuthorizer,
    RequestHeaders, StagedArtifact, StagedIdentity, TauriUpdateConfig, TransportError,
    UpdateAuthority, UpdateErrorCode, UpdateHost, UpdateHostError, UpdateHostPolicy, UpdateMessage,
    UpdatePolicy, UpdateRefusal, UpdateRoute, UpdateTransport,
};
use tauri::Url;

const GATED: &str = "rightapps-license-gate.adrdsouza.workers.dev";
const PUBLIC: &str = "pub-6c73208d46c245a9b4881d5e02f6b618.r2.dev";

fn url(text: &str) -> Url {
    Url::parse(text).unwrap()
}

#[test]
fn the_check_host_and_the_artifact_hosts_are_allowed_for_the_app() {
    let hosts = UpdateHostPolicy::right_suite("coderight").unwrap();
    for allowed in [
        "https://api.orthiclabs.com/v1/apps/coderight/releases/latest.json",
        "https://api.orthiclabs.com/v1/apps/coderight/releases/status.json",
        &format!("https://{GATED}/coderight/updates/windows/current/CodeRight_x64-setup.exe"),
        &format!("https://{PUBLIC}/coderight/installers/windows/current/CodeRight_x64-setup.exe"),
    ] {
        assert!(hosts.allows(&url(allowed)), "{allowed}");
    }
}

#[test]
fn the_legacy_host_lookalikes_and_unsafe_urls_are_refused() {
    let hosts = UpdateHostPolicy::right_suite("coderight").unwrap();
    for refused in [
        "http://api.orthiclabs.com/latest.json",
        "https://api.orthiclabs.com:8443/latest.json",
        "https://user@api.orthiclabs.com/latest.json",
        // The retired host (sellright#331): its 301s are never followed, so it is not allowlisted.
        "https://api.spoares.com/v1/apps/coderight/releases/latest.json",
        "https://api.orthiclabs.com.evil.com/v1/apps/coderight/releases/latest.json",
        "https://evilapi.orthiclabs.com/v1/apps/coderight/releases/latest.json",
        "https://api.coderight.cc/releases/status.json",
        "https://evil.example/coderight/updates/windows/current/CodeRight_x64-setup.exe",
        &format!("https://{PUBLIC}.evil.com/coderight/x"),
    ] {
        assert!(!hosts.allows(&url(refused)), "{refused}");
    }
}

#[test]
fn viewright_never_receives_the_gated_host() {
    let hosts = UpdateHostPolicy::right_suite("viewright").unwrap();
    assert!(!hosts.allows(&url(&format!("https://{GATED}/viewright/updates/x"))));
    assert!(hosts.allows(&url(&format!("https://{PUBLIC}/viewright/installers/x"))));
    let mail = UpdateHostPolicy::right_suite("mailright").unwrap();
    assert!(mail.allows(&url(&format!("https://{GATED}/mailright/updates/x"))));
}

#[test]
fn an_unlisted_or_malformed_app_builds_no_policy() {
    for bad in ["", "Mail", "a/b", "../x", "nosuchapp"] {
        assert!(UpdateHostPolicy::right_suite(bad).is_err(), "{bad:?}");
        assert!(UpdatePolicy::right_suite(bad).is_err(), "{bad:?}");
        assert!(
            TauriUpdateConfig::right_suite(bad, "key", Duration::from_secs(1)).is_err(),
            "{bad:?}"
        );
    }
}

#[test]
fn the_updater_config_is_the_contracts_feed_with_the_runtime_placeholder() {
    let config =
        TauriUpdateConfig::right_suite("mailright", "PUBKEY", Duration::from_secs(60)).unwrap();
    // `Url` keeps braces in a query, so Tauri's updater still sees `{{target}}` to fill.
    assert_eq!(
        config.endpoint.as_str(),
        "https://api.orthiclabs.com/v1/apps/mailright/releases/latest.json?platform={{target}}"
    );
    assert_eq!(config.public_key, "PUBKEY");
    for (key, timeout) in [("", Duration::from_secs(1)), (" ", Duration::from_secs(1))] {
        assert!(TauriUpdateConfig::right_suite("mailright", key, timeout).is_err());
    }
    assert!(TauriUpdateConfig::right_suite("mailright", "k", Duration::ZERO).is_err());
}

#[test]
fn the_policy_carries_the_contracts_status_url() {
    let policy = UpdatePolicy::right_suite("viewright").unwrap();
    assert_eq!(
        policy.release_status_url.as_ref().map(Url::as_str),
        Some("https://api.orthiclabs.com/v1/apps/viewright/releases/status.json")
    );
    assert!(policy
        .hosts
        .allows(policy.release_status_url.as_ref().unwrap()));
}

// ---- the release-status command's host side ----------------------------------------------------

struct Seen(Arc<Mutex<Vec<String>>>);

impl UpdateTransport for Seen {
    fn check(&self, _: &RequestAuthorizer<'_>) -> Result<Option<ReleaseInfo>, TransportError> {
        Err(TransportError::Failed)
    }
    fn download(
        &self,
        _: &ReleaseInfo,
        _: &RequestAuthorizer<'_>,
        _: &mut dyn FnMut(u64, Option<u64>) -> bool,
    ) -> Result<StagedArtifact, TransportError> {
        Err(TransportError::Failed)
    }
    fn staged_identity(&self, _: &StagedArtifact) -> Result<StagedIdentity, TransportError> {
        Err(TransportError::Failed)
    }
    fn install(&self, _: StagedArtifact) -> Result<(), TransportError> {
        Err(TransportError::Failed)
    }
    fn release_status(&self, url: &Url) -> Result<ReleaseStatusView, TransportError> {
        self.0.lock().unwrap().push(url.to_string());
        Ok(ReleaseStatusView {
            schema: 1,
            app_key: "viewright".into(),
            platform: None,
            patch_version: Some("1.0.0".into()),
            update_version: None,
        })
    }
}

struct NoAuthority;

impl UpdateAuthority for NoAuthority {
    fn begin(&self, _: UpdateRoute) -> Result<BindingToken, UpdateRefusal> {
        Err(UpdateRefusal::NotLicensed)
    }
    fn with_release(&self, binding: &BindingToken, _: &str) -> BindingToken {
        binding.clone()
    }
    fn validate(&self, _: &BindingToken) -> Result<(), UpdateRefusal> {
        Err(UpdateRefusal::NotLicensed)
    }
    fn authorize(&self, _: &BindingToken, _: &Url) -> Result<RequestHeaders, UpdateRefusal> {
        Err(UpdateRefusal::NotLicensed)
    }
}

fn host(policy: UpdatePolicy) -> (UpdateHost, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let host = UpdateHost::new(policy, NoAuthority, Seen(Arc::clone(&seen))).unwrap();
    (host, seen)
}

#[test]
fn release_status_reads_the_contracts_url_through_the_transport() {
    let (host, seen) = host(UpdatePolicy::right_suite("viewright").unwrap());
    let view = host.release_status().unwrap();
    assert_eq!(view.patch_version.as_deref(), Some("1.0.0"));
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        ["https://api.orthiclabs.com/v1/apps/viewright/releases/status.json"]
    );
    // The answer is closed: exactly the fields `isReleaseStatus` accepts.
    assert_eq!(
        serde_json::to_value(&view).unwrap(),
        serde_json::json!({
            "schema": 1, "appKey": "viewright", "platform": null,
            "patchVersion": "1.0.0", "updateVersion": null
        })
    );
}

#[test]
fn release_status_without_a_url_or_with_a_disallowed_host_is_unsupported_and_never_read() {
    let unsupported =
        UpdateHostError::new(UpdateErrorCode::Unsupported, UpdateMessage::Unsupported);
    let hosts = UpdateHostPolicy::right_suite("viewright").unwrap();
    let (none, seen) = host(UpdatePolicy::new(hosts.clone()));
    assert_eq!(none.release_status().unwrap_err(), unsupported);
    for refused in [
        "https://api.spoares.com/v1/apps/viewright/releases/status.json",
        "http://api.orthiclabs.com/v1/apps/viewright/releases/status.json",
        "https://evil.example/status.json",
    ] {
        let (h, seen2) = host(UpdatePolicy::new(hosts.clone()).release_status_url(url(refused)));
        assert_eq!(h.release_status().unwrap_err(), unsupported, "{refused}");
        assert!(seen2.lock().unwrap().is_empty(), "{refused} was read");
    }
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn a_transport_without_release_status_fails_closed() {
    struct Plain;
    impl UpdateTransport for Plain {
        fn check(&self, _: &RequestAuthorizer<'_>) -> Result<Option<ReleaseInfo>, TransportError> {
            Ok(None)
        }
        fn download(
            &self,
            _: &ReleaseInfo,
            _: &RequestAuthorizer<'_>,
            _: &mut dyn FnMut(u64, Option<u64>) -> bool,
        ) -> Result<StagedArtifact, TransportError> {
            Err(TransportError::Failed)
        }
        fn staged_identity(&self, _: &StagedArtifact) -> Result<StagedIdentity, TransportError> {
            Err(TransportError::Failed)
        }
        fn install(&self, _: StagedArtifact) -> Result<(), TransportError> {
            Ok(())
        }
    }
    let host = UpdateHost::new(
        UpdatePolicy::right_suite("viewright").unwrap(),
        NoAuthority,
        Plain,
    )
    .unwrap();
    assert_eq!(
        host.release_status().unwrap_err(),
        UpdateHostError::new(UpdateErrorCode::Unknown, UpdateMessage::UpdateFailed)
    );
}

#[test]
fn the_release_status_shape_matches_the_typescript_guard() {
    let ok = |body: &str| parse_release_status(body.as_bytes());
    assert!(ok(r#"{"schema":1,"appKey":"a","platform":"darwin-aarch64","patchVersion":"1","updateVersion":null}"#).is_some());
    for bad in [
        r#"{"schema":1,"appKey":"a","platform":null,"patchVersion":null}"#,
        r#"{"schema":"1","appKey":"a","platform":null,"patchVersion":null,"updateVersion":null}"#,
        r#"{"schema":1,"appKey":7,"platform":null,"patchVersion":null,"updateVersion":null}"#,
        r#"{"schema":1,"appKey":"a","platform":null,"patchVersion":null,"updateVersion":null,"x":0}"#,
        r#"{"schema":1,"appKey":"a\u0000","platform":null,"patchVersion":null,"updateVersion":null}"#,
        "null",
        "",
    ] {
        assert!(ok(bad).is_none(), "{bad}");
    }
}
