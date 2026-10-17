//! Popup and protocol decisions for OAuth popups and non-web schemes (CodeRight port, #638 and #639).

use std::collections::BTreeMap;

use rightkit_tauri_shell::nav::{
    popup_origins_from_json, strip_coop_headers, NavPolicy, WindowOpenDecision, OAUTH_POPUP_ORIGINS,
};
use tauri::Url;

fn url(s: &str) -> Url {
    Url::parse(s).expect("test url")
}

#[test]
fn atlassian_and_salesforce_sign_in_origins_are_popup_origins() {
    for origin in [
        "https://mcp.atlassian.com",
        "https://auth.atlassian.com",
        "https://login.salesforce.com",
        "https://test.salesforce.com",
    ] {
        assert!(OAUTH_POPUP_ORIGINS.contains(&origin), "{origin} missing");
    }
}

#[test]
fn non_web_schemes_are_denied_and_only_unserved_ones_ask_for_consent() {
    let p = NavPolicy::default();
    let app = url("tauri://localhost/");
    assert_eq!(
        p.decide_window_open(&url("zoom:meeting?id=1"), true, Some(&app)),
        WindowOpenDecision::Deny
    );
    assert_eq!(
        p.protocol_consent_scheme(&url("zoom:meeting?id=1"))
            .as_deref(),
        Some("zoom")
    );
    for denied in [
        "file:///etc/hosts",
        "javascript:alert(1)",
        "data:text/html,x",
        "blob:x",
    ] {
        assert_eq!(
            p.decide_window_open(&url(denied), true, Some(&app)),
            WindowOpenDecision::Deny,
            "{denied}"
        );
        assert_eq!(p.protocol_consent_scheme(&url(denied)), None, "{denied}");
    }
    assert_eq!(p.protocol_consent_scheme(&url("https://x.example/")), None);
}

#[test]
fn a_pinned_origin_admits_only_its_own_opener() {
    let github = url("https://github.com/login/oauth/authorize?client_id=x");
    let policy = NavPolicy::default();
    let pin = Some("https://app.example");
    assert_eq!(
        policy.decide_window_open_pinned(&github, true, Some(&url("https://app.example/")), pin),
        WindowOpenDecision::Popup
    );
    assert!(matches!(
        policy.decide_window_open_pinned(&github, true, Some(&url("tauri://localhost/")), pin),
        WindowOpenDecision::External { .. }
    ));
    assert!(matches!(
        policy.decide_window_open_pinned(&github, true, None, pin),
        WindowOpenDecision::External { .. }
    ));
}

#[test]
fn strip_coop_headers_removes_both_opener_policies_case_insensitively() {
    let mut headers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    headers.insert(
        "Cross-Origin-Opener-Policy".into(),
        vec!["same-origin".into()],
    );
    headers.insert(
        "cross-origin-opener-policy-report-only".into(),
        vec!["same-origin".into()],
    );
    headers.insert("content-type".into(), vec!["text/html".into()]);
    let kept = strip_coop_headers(Some(&headers)).expect("some in, some out");
    assert_eq!(kept.len(), 1);
    assert!(kept.contains_key("content-type"));
    assert_eq!(strip_coop_headers(None), None);
}

#[test]
fn popup_origins_from_json_ignores_invalid_entries() {
    let raw = r#"["https://idp.example.com", "http://plain.example", "https://idp.example.com",
        "https://x.example/path", "not a url", 7, "https://idp.example.com:8443"]"#;
    assert_eq!(
        popup_origins_from_json(raw),
        vec![
            "https://idp.example.com".to_string(),
            "https://idp.example.com:8443".to_string()
        ]
    );
    assert!(popup_origins_from_json("{\"a\":1}").is_empty());
    assert!(popup_origins_from_json("not json").is_empty());
}

#[test]
fn without_a_pin_the_unpinned_decision_is_unchanged() {
    let github = url("https://github.com/login/oauth/authorize?client_id=x");
    let policy = NavPolicy::default();
    assert_eq!(
        policy.decide_window_open(&github, true, None),
        policy.decide_window_open_pinned(&github, true, None, None)
    );
    assert_eq!(
        policy.decide_window_open(&github, true, Some(&url("tauri://localhost/"))),
        WindowOpenDecision::Popup
    );
}
