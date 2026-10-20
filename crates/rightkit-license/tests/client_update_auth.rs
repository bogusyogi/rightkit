#![cfg(all(feature = "client-testing", feature = "update-auth"))]
//! Updater authorization against the native client's record: the headers of a request are decided
//! inside one lock hold, are never serialisable, never print, expire, and are refused for a record
//! that changed, was deactivated, or is not for the host and path asked for.

mod client_support;

use std::marker::PhantomData;
use std::time::Duration;

use client_support::*;
use rightkit_license::{
    AuthorizedHeaders, LicenseClient, LicenseRevision, SecretString, UpdateAuthConfig,
    UpdateAuthError, UpdateAuthProvider, UpdateBinding, UpdateRoute,
};
use url::Url;

const CHECK_URL: &str = "https://updates.example/latest.json";
const ARTIFACT_URL: &str = "https://downloads.example/downloads/heardright/app-1.0.0.msi";

fn key() -> SecretString {
    SecretString::new(KEY)
}

fn config() -> UpdateAuthConfig {
    UpdateAuthConfig::new(
        vec!["updates.example".to_string()],
        vec!["downloads.example".to_string()],
        "/downloads/heardright",
    )
    .unwrap()
}

fn provider(client: &LicenseClient, config: UpdateAuthConfig) -> UpdateAuthProvider {
    UpdateAuthProvider::new(client, config)
}

fn binding(provider: &UpdateAuthProvider) -> UpdateBinding {
    UpdateBinding {
        revision: provider.revision().expect("a valid licence"),
        route: UpdateRoute::Private,
        release_id: "rel-1".to_string(),
        staged: None,
    }
}

fn url(text: &str) -> Url {
    Url::parse(text).unwrap()
}

/// The (name, value) pairs of `headers`.
fn pairs(headers: &AuthorizedHeaders) -> Vec<(String, String)> {
    let mut out = Vec::new();
    headers
        .each(|name, value| out.push((name.to_string(), value.to_string())))
        .unwrap();
    out
}

/// Whether `T` implements `Serialize`, decided at compile time.
struct Probe<T>(PhantomData<T>);

trait NotSerialize {
    const SERIALIZE: bool = false;
}

impl<T> NotSerialize for Probe<T> {}

impl<T: serde::Serialize> Probe<T> {
    const SERIALIZE: bool = true;
}

#[test]
fn updater_headers_are_never_serialisable_and_never_print() {
    assert!(!Probe::<AuthorizedHeaders>::SERIALIZE);
    assert!(!Probe::<UpdateBinding>::SERIALIZE);
    assert!(!Probe::<LicenseRevision>::SERIALIZE);
    assert!(!Probe::<UpdateAuthProvider>::SERIALIZE);

    let f = fixture();
    let client = f.client();
    client.activate(key()).unwrap();
    let provider = provider(&client, config());
    let binding = binding(&provider);

    // The metadata check carries the activation token, an artifact the signed token.
    let check = provider.authorize(&binding, &url(CHECK_URL)).unwrap();
    assert_eq!(check.names(), vec!["Authorization"]);
    assert_eq!(
        pairs(&check),
        vec![("Authorization".to_string(), format!("Bearer {ACTIVATION}"))]
    );
    let artifact = provider.authorize(&binding, &url(ARTIFACT_URL)).unwrap();
    let signed = f.state.issued.lock().unwrap()[0].clone();
    assert_eq!(
        pairs(&artifact),
        vec![("Authorization".to_string(), format!("Bearer {signed}"))]
    );

    // Nothing that can be formatted shows a value: not the headers, the binding, the revision, the
    // provider, the client, nor the request the headers were applied to.
    let request = check.apply(rightkit_http::Request::get(CHECK_URL)).unwrap();
    let texts = [
        format!("{artifact:?}"),
        format!("{binding:?}"),
        format!("{:?}", binding.revision),
        format!("{provider:?}"),
        format!("{client:?}"),
        format!("{request:?}"),
        format!("{:?}", config()),
    ];
    for text in &texts {
        f.assert_no_secret("an updater value", text);
    }
}

#[test]
fn updater_headers_expire() {
    let f = fixture();
    let client = f.client();
    client.activate(key()).unwrap();
    let provider = provider(&client, config().with_header_ttl(Duration::from_millis(50)));
    let binding = binding(&provider);
    let headers = provider.authorize(&binding, &url(CHECK_URL)).unwrap();
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        headers.each(|_, _| {}).unwrap_err(),
        UpdateAuthError::HeadersExpired
    );
    let late = provider.authorize(&binding, &url(CHECK_URL)).unwrap();
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        late.apply(rightkit_http::Request::get(CHECK_URL))
            .unwrap_err(),
        UpdateAuthError::HeadersExpired
    );
}

#[test]
fn a_changed_or_deactivated_licence_gets_no_headers_and_a_patch_needs_none() {
    let f = fixture();
    let client = f.client();
    client.activate(key()).unwrap();
    let provider = provider(&client, config());
    let binding = binding(&provider);
    provider.authorize(&binding, &url(CHECK_URL)).unwrap();

    // A refresh moves the revision: an update that began under the old one is refused.
    client.refresh().unwrap();
    assert_eq!(
        provider.authorize(&binding, &url(CHECK_URL)).unwrap_err(),
        UpdateAuthError::RevisionChanged
    );
    assert_eq!(
        provider.validate(&binding).unwrap_err(),
        UpdateAuthError::RevisionChanged
    );
    // A binding taken now works again, until the licence is deactivated.
    let fresh = self::binding(&provider);
    provider.authorize(&fresh, &url(ARTIFACT_URL)).unwrap();
    client.deactivate().unwrap();
    assert_eq!(
        provider.authorize(&fresh, &url(ARTIFACT_URL)).unwrap_err(),
        UpdateAuthError::NotLicensed
    );
    assert_eq!(
        provider.revision().unwrap_err(),
        UpdateAuthError::NotLicensed
    );

    // A public patch carries nothing and needs no licence.
    let public = UpdateBinding {
        revision: LicenseRevision::none(),
        route: UpdateRoute::Public,
        release_id: "rel-2".to_string(),
        staged: None,
    };
    let headers = provider.authorize(&public, &url(ARTIFACT_URL)).unwrap();
    assert!(headers.is_empty() && headers.names().is_empty());
}

#[test]
fn a_request_for_another_host_or_path_gets_no_headers() {
    let f = fixture();
    let client = f.client();
    client.activate(key()).unwrap();
    let provider = provider(&client, config());
    let binding = binding(&provider);
    for (target, expected) in [
        (
            "https://evil.example/latest.json",
            UpdateAuthError::HostNotAllowed,
        ),
        (
            "http://updates.example/latest.json",
            UpdateAuthError::HostNotAllowed,
        ),
        (
            "https://downloads.example/elsewhere/app.msi",
            UpdateAuthError::PathNotAllowed,
        ),
        (
            "https://downloads.example/downloads/heardright/../secret/app.msi",
            UpdateAuthError::PathNotAllowed,
        ),
    ] {
        assert_eq!(
            provider.authorize(&binding, &url(target)).unwrap_err(),
            expected,
            "{target}"
        );
    }
}
