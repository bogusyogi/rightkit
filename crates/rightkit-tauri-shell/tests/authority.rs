//! Authority tests ported from ViewRight: the path broker's known-token exchange, canonicalization,
//! stream-token revocation, site-root tokens, symlink escape and revoked-root serving (ViewRight
//! `path_broker/tests.rs`, `site_root.rs`, `binary_stream.rs`). They drive the public API of the
//! broker, the site scheme and the stream scheme with no Tauri app running.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use rightkit_tauri_shell::broker::PathBroker;
#[cfg(unix)]
use rightkit_tauri_shell::site::resolve_in_root;
use rightkit_tauri_shell::site::{serve_site_request, SitePolicy};
use rightkit_tauri_shell::stream::{serve as serve_stream, StreamPolicy};
use tauri::http::{Request, Response, StatusCode};

fn fixture(dir: &tempfile::TempDir, name: &str, body: &[u8]) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, body).unwrap();
    path
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap()
}

fn has_random_hex_suffix(token: &str, prefix: &str) -> bool {
    let Some(suffix) = token.strip_prefix(prefix) else {
        return false;
    };
    suffix.len() == 32 && suffix.chars().all(|c| c.is_ascii_hexdigit())
}

/// `serve_site_request` answers through a callback that may run on a worker thread. The channel
/// makes each request synchronous for the test and proves it answered exactly once.
fn site_sync(broker: &PathBroker, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    serve_site_request(broker, &SitePolicy::default(), request, move |response| {
        let _ = tx.send(response);
    });
    rx.recv_timeout(Duration::from_secs(5))
        .expect("serve_site_request responds exactly once")
}

fn stream_sync(broker: &PathBroker, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    serve_stream(broker, &StreamPolicy::default(), request, move |response| {
        let _ = tx.send(response);
    });
    rx.recv_timeout(Duration::from_secs(5))
        .expect("serve responds exactly once")
}

fn site_get(token: &str, rel: &str) -> Request<Vec<u8>> {
    Request::builder()
        .uri(format!("viewright-site://localhost/{token}/{rel}"))
        .body(Vec::new())
        .unwrap()
}

#[test]
fn known_token_does_not_grant_until_exchanged() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(&dir, "known-token.txt", b"ok");
    let broker = PathBroker::default();

    let token = broker.register_known_path(path.to_str().unwrap()).unwrap();
    assert!(broker.require_file(path.to_str().unwrap()).is_err());

    let granted = broker.grant_known(&token).unwrap();
    assert_eq!(
        broker.require_file(path.to_str().unwrap()).unwrap(),
        granted
    );

    // One shot: replaying the exchanged token fails.
    assert!(broker.grant_known(&token).is_err());
}

#[test]
fn stored_known_paths_are_canonicalized_before_granting() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("known-canonical-dir");
    std::fs::create_dir_all(&sub).unwrap();
    let path = sub.join("known-canonical.txt");
    std::fs::write(&path, "ok").unwrap();
    let non_canonical = sub
        .join("..")
        .join("known-canonical-dir")
        .join("known-canonical.txt");
    let broker = PathBroker::default();

    let token = broker
        .register_known_path(non_canonical.to_str().unwrap())
        .unwrap();
    let granted = broker.grant_known(&token).unwrap();

    assert_eq!(
        broker.require_file(path.to_str().unwrap()).unwrap(),
        granted
    );
}

#[test]
fn stream_token_is_revoked_with_path_and_not_revived_by_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(&dir, "stream-token.pdf", b"%PDF-1.7\n");
    let broker = PathBroker::default();

    let granted = broker.grant_file(&path).unwrap();
    let id = broker.grant_stream(&granted).unwrap();
    assert_eq!(broker.resolve_stream(&id).unwrap().path, canonical(&path));

    broker.revoke_path(&granted).unwrap();
    assert!(broker.resolve_stream(&id).is_err());

    // Reopening the same file grants it again, but the old stream id stays dead.
    broker.grant_file(&path).unwrap();
    assert!(broker.require_file(path.to_str().unwrap()).is_ok());
    assert!(broker.resolve_stream(&id).is_err());
}

#[test]
fn site_root_token_resolves_until_revoked() {
    let dir = tempfile::tempdir().unwrap();
    let broker = PathBroker::default();

    let token = broker.grant_site_root(dir.path()).unwrap();
    assert!(has_random_hex_suffix(&token, "site-"));
    assert_eq!(
        broker.resolve_site_root(&token).unwrap(),
        canonical(dir.path())
    );

    broker.revoke_site_root(&token).unwrap();
    // A closed tab's URLs must die with it: the same token never resolves again.
    assert!(broker.resolve_site_root(&token).is_err());
    broker.revoke_site_root(&token).unwrap(); // idempotent
}

#[test]
fn site_root_grant_is_not_a_file_grant() {
    let dir = tempfile::tempdir().unwrap();
    let secret = fixture(&dir, "secret.txt", b"s");
    let broker = PathBroker::default();

    broker.grant_site_root(dir.path()).unwrap();

    // A site root widens only the containment-checked, MIME-allowlisted site scheme. It must not
    // make arbitrary files in the tree readable through the document commands.
    assert!(broker.require_file(secret.to_str().unwrap()).is_err());
}

#[test]
fn two_tabs_on_one_root_get_independent_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let broker = PathBroker::default();

    let tab_a = broker.grant_site_root(dir.path()).unwrap();
    let tab_b = broker.grant_site_root(dir.path()).unwrap();
    assert_ne!(tab_a, tab_b);

    broker.revoke_site_root(&tab_a).unwrap();

    // Closing one tab must not tear down another tab open on the same project.
    assert!(broker.resolve_site_root(&tab_a).is_err());
    assert!(broker.resolve_site_root(&tab_b).is_ok());
}

#[cfg(unix)]
#[test]
fn rejects_symlink_pointing_outside_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("site");
    std::fs::create_dir_all(&root).unwrap();
    let outside = dir.path().join("outside.png");
    std::fs::write(&outside, b"x").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("link.png")).unwrap();

    let err = resolve_in_root(&root, "link.png", &SitePolicy::default()).unwrap_err();
    assert_eq!(err.code(), "broker_denied", "unexpected error: {err}");

    // Through the scheme the same escape is a policy refusal, not a 404.
    let broker = PathBroker::default();
    let token = broker.grant_site_root(&root).unwrap();
    assert_eq!(
        site_sync(&broker, &site_get(&token, "link.png")).status(),
        StatusCode::FORBIDDEN
    );
}

#[test]
fn revoked_root_stops_serving_immediately() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.png"), b"x").unwrap();
    let broker = PathBroker::default();
    let token = broker.grant_site_root(dir.path()).unwrap();
    assert_eq!(
        site_sync(&broker, &site_get(&token, "a.png")).status(),
        StatusCode::OK
    );

    broker.revoke_site_root(&token).unwrap();

    assert_eq!(
        site_sync(&broker, &site_get(&token, "a.png")).status(),
        StatusCode::FORBIDDEN
    );
}

#[test]
fn revoked_token_is_denied_even_if_path_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(&dir, "revoked.pdf", b"0123456789");
    let broker = PathBroker::default();
    let granted = broker.grant_file(&path).unwrap();
    let id = broker.grant_stream(&granted).unwrap();

    broker.revoke_path(&granted).unwrap();
    broker.grant_file(&path).unwrap();

    let request = Request::builder()
        .uri(format!("viewright-stream://localhost/{id}"))
        .body(Vec::new())
        .unwrap();
    let response = stream_sync(&broker, &request);

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(response.body().is_empty());
}
