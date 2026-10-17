//! rightkit#240 (for Ember): ordered mirrors with fallback and the same pins, `AllMirrorsFailed`,
//! the flat-directory adopt convenience, and the test-run loopback hook.
//!
//! Downloads run against rightkit-qa's `MockServer` on 127.0.0.1; the stores reach it through
//! `Store::with_address_policy`, with retries off so each mirror is tried once.

use rightkit_models::http::{ModelHttpOptions, ProxyMode};
use rightkit_models::*;
use rightkit_qa::mock::{MockServer, Reply, Request};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

fn sha(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

fn blob(seed: u8, n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn local(store: Store) -> Store {
    store
        .with_http_options(ModelHttpOptions {
            proxy: ProxyMode::None,
            retries: 0,
            ..Default::default()
        })
        .with_address_policy(
            rightkit_http::AddressPolicy::public_web()
                .allow_if(vec!["127.0.0.1".to_string()], |ip| ip.is_loopback()),
        )
}

fn spec(file: FileSpec) -> ModelSpec {
    ModelSpec::new(
        "demo",
        vec![RevisionSpec {
            revision: "1".into(),
            files: vec![file],
            ..Default::default()
        }],
    )
}

fn pinned(url: String, mirrors: Vec<String>, data: &[u8]) -> FileSpec {
    FileSpec {
        path: "model.bin".into(),
        url,
        bytes: Some(data.len() as u64),
        sha256: Some(sha(data)),
        ..Default::default()
    }
    .with_mirrors(mirrors)
}

fn opts() -> InstallOptions {
    InstallOptions {
        require_pinned: true,
        ..Default::default()
    }
}

fn routes(mock: &MockServer) -> Vec<String> {
    mock.seen().iter().map(|s| s.route().to_string()).collect()
}

// ---------------------------------------------------------------- mirrors

#[test]
fn urls_are_ordered_deduplicated_and_capped() {
    let f = FileSpec {
        url: "https://a/x".into(),
        mirrors: vec![
            "https://b/x".into(),
            "https://a/x".into(),
            "  ".into(),
            "https://b/x".into(),
            "https://c/x".into(),
        ],
        ..Default::default()
    };
    assert_eq!(f.urls(), vec!["https://a/x", "https://b/x", "https://c/x"]);

    let many = FileSpec {
        url: "https://h0/x".into(),
        mirrors: (1..40).map(|i| format!("https://h{i}/x")).collect(),
        ..Default::default()
    };
    let urls = many.urls();
    assert_eq!(urls.len(), MAX_MIRROR_URLS);
    assert_eq!(urls[0], "https://h0/x");
    assert_eq!(urls[15], "https://h15/x");

    // No mirrors: just the primary, as before.
    let one = FileSpec {
        url: "https://a/x".into(),
        ..Default::default()
    };
    assert_eq!(one.urls(), vec!["https://a/x"]);
}

#[test]
fn mirrors_serialize_only_when_present_and_default_when_absent() {
    let plain: FileSpec = serde_json::from_str(r#"{"path":"a","url":"https://a/x"}"#).unwrap();
    assert!(plain.mirrors.is_empty());
    assert!(!serde_json::to_string(&plain).unwrap().contains("mirrors"));
    let with = plain.with_mirrors(vec!["https://b/x".into()]);
    let text = serde_json::to_string(&with).unwrap();
    assert!(text.contains(r#""mirrors":["https://b/x"]"#), "{text}");
}

#[test]
fn a_failing_primary_falls_back_to_the_next_mirror_with_the_same_pins() {
    let data = blob(3, 200_000);
    let body = data.clone();
    let mock = MockServer::start_with(move |req: &Request, _: &str| match req.route() {
        "/primary.bin" => Some(Reply::status(500, serde_json::json!({}))),
        "/mirror.bin" => Some(Reply::bytes("application/octet-stream", body.clone())),
        _ => None,
    })
    .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let store = local(Store::new(tmp.path().join("models"), "t"));
    let file = pinned(
        mock.url("/primary.bin"),
        vec![mock.url("/mirror.bin")],
        &data,
    );
    let out = store.install(&spec(file), &opts(), |_| {}).unwrap();
    assert_eq!(fs::read(out.dir.join("model.bin")).unwrap(), data);
    assert_eq!(routes(&mock), vec!["/primary.bin", "/mirror.bin"]);
}

#[test]
fn a_mirror_serving_wrong_bytes_is_rejected_and_the_next_is_used() {
    let data = blob(5, 150_000);
    let wrong = blob(6, 150_000); // same size, different hash
    let (good, bad) = (data.clone(), wrong.clone());
    let mock = MockServer::start_with(move |req: &Request, _: &str| match req.route() {
        "/bad.bin" => Some(Reply::bytes("application/octet-stream", bad.clone())),
        "/good.bin" => Some(Reply::bytes("application/octet-stream", good.clone())),
        _ => None,
    })
    .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let store = local(Store::new(tmp.path().join("models"), "t"));
    let file = pinned(mock.url("/bad.bin"), vec![mock.url("/good.bin")], &data);
    let out = store.install(&spec(file), &opts(), |_| {}).unwrap();
    assert_eq!(fs::read(out.dir.join("model.bin")).unwrap(), data);
    assert_eq!(routes(&mock), vec!["/bad.bin", "/good.bin"]);
}

#[test]
fn when_every_mirror_fails_the_error_lists_each_one_without_query_secrets() {
    let data = blob(7, 10_000);
    let mock = MockServer::start_with(move |req: &Request, _: &str| match req.route() {
        "/a.bin" => Some(Reply::status(503, serde_json::json!({}))),
        _ => None, // 404
    })
    .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let store = local(Store::new(tmp.path().join("models"), "t"));
    let file = pinned(
        mock.url("/a.bin?token=SECRET123"),
        vec![mock.url("/b.bin")],
        &data,
    );
    let err = store.install(&spec(file), &opts(), |_| {}).unwrap_err();
    assert_eq!(err.code, ErrorCode::AllMirrorsFailed, "{err}");
    assert!(err.message.contains("mirror 1/2"), "{err}");
    assert!(err.message.contains("mirror 2/2"), "{err}");
    assert!(err.message.contains("/a.bin"), "{err}");
    assert!(err.message.contains("503"), "{err}");
    assert!(err.message.contains("404"), "{err}");
    assert!(!err.message.contains("SECRET123"), "{err}");
    assert!(store.state("demo").unwrap().active.is_none());
}

#[test]
fn a_single_url_still_reports_its_own_error_unchanged() {
    let data = blob(8, 10_000);
    let mock = MockServer::start_with(|_: &Request, _: &str| None::<Reply>).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let store = local(Store::new(tmp.path().join("models"), "t"));
    let file = pinned(mock.url("/only.bin"), vec![], &data);
    let err = store.install(&spec(file), &opts(), |_| {}).unwrap_err();
    assert_eq!(err.code, ErrorCode::Download, "{err}");
    assert!(!err.message.contains("mirror"), "{err}");
}

#[test]
fn a_cancel_stops_the_fallback_instead_of_trying_the_next_mirror() {
    let data = blob(9, 300_000);
    let body = data.clone();
    let mock = MockServer::start_with(move |req: &Request, _: &str| match req.route() {
        "/p.bin" | "/m.bin" => Some(Reply::bytes("application/octet-stream", body.clone())),
        _ => None,
    })
    .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let store = local(Store::new(tmp.path().join("models"), "t"));
    let file = pinned(mock.url("/p.bin"), vec![mock.url("/m.bin")], &data);
    let o = opts();
    let cancel = o.cancel.clone();
    let err = store
        .install(&spec(file), &o, |e| {
            if matches!(e, Event::Progress { .. }) {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        })
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Cancelled, "{err}");
    assert!(!routes(&mock).contains(&"/m.bin".to_string()));
}

// ----------------------------------------------------------- flat adopt

fn flat_dir(root: &Path, data: &[u8]) -> std::path::PathBuf {
    let dir = root.join("flat");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("model.bin"), data).unwrap();
    dir
}

fn offline_spec(data: &[u8]) -> ModelSpec {
    spec(pinned(
        "https://models.example.invalid/m.bin".into(),
        vec![],
        data,
    ))
}

#[test]
fn adopt_flat_dir_verifies_records_and_leaves_the_directory() {
    let data = blob(1, 120_000);
    let tmp = tempfile::tempdir().unwrap();
    let dir = flat_dir(tmp.path(), &data);
    let store = Store::new(tmp.path().join("models"), "t");
    let mut adopted = 0;
    let out = store
        .adopt_flat_dir_with(&offline_spec(&data), &opts(), &dir, |e| {
            if matches!(e, Event::Adopted { .. }) {
                adopted += 1;
            }
        })
        .unwrap();
    assert_eq!(adopted, 1);
    assert!(!out.already_installed);
    assert!(store.verify("demo", &out.key, VerifyMode::Full));
    assert_eq!(fs::read(out.dir.join("model.bin")).unwrap(), data);
    assert!(
        dir.join("model.bin").is_file(),
        "the flat directory is untouched"
    );
    assert!(
        store
            .adopt_flat_dir(&offline_spec(&data), &opts(), &dir)
            .unwrap()
            .already_installed
    );
}

#[test]
fn adopt_flat_dir_refuses_wrong_bytes_and_records_nothing() {
    let data = blob(1, 120_000);
    let tmp = tempfile::tempdir().unwrap();
    let dir = flat_dir(tmp.path(), &blob(2, 120_000)); // same size, other bytes
    let store = Store::new(tmp.path().join("models"), "t");
    let err = store
        .adopt_flat_dir(&offline_spec(&data), &opts(), &dir)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::HashMismatch, "{err}");
    assert!(store.state("demo").unwrap().active.is_none());

    fs::write(dir.join("model.bin"), b"short").unwrap();
    let err = store
        .adopt_flat_dir(&offline_spec(&data), &opts(), &dir)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::SizeMismatch, "{err}");
    assert!(store.state("demo").unwrap().active.is_none());
}

#[test]
fn adopt_flat_dir_reports_a_missing_file_and_a_non_directory() {
    let data = blob(1, 1_000);
    let tmp = tempfile::tempdir().unwrap();
    let empty = tmp.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let store = Store::new(tmp.path().join("models"), "t");
    let err = store
        .adopt_flat_dir(&offline_spec(&data), &opts(), &empty)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound, "{err}");

    let file = tmp.path().join("afile");
    fs::write(&file, b"x").unwrap();
    let err = store
        .adopt_flat_dir(&offline_spec(&data), &opts(), &file)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidSpec, "{err}");
    assert!(store.state("demo").unwrap().active.is_none());
}

#[cfg(unix)]
#[test]
fn adopt_flat_dir_refuses_a_symlinked_file() {
    let data = blob(1, 5_000);
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real.bin");
    fs::write(&real, &data).unwrap();
    let dir = tmp.path().join("flat");
    fs::create_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink(&real, dir.join("model.bin")).unwrap();
    let store = Store::new(tmp.path().join("models"), "t");
    let err = store
        .adopt_flat_dir(&offline_spec(&data), &opts(), &dir)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidSpec, "{err}");
}

// ------------------------------------------------------- loopback hook

#[test]
fn the_loopback_test_hook_needs_a_test_run() {
    let release = rightkit_daemon::mode::Mode::Release;
    assert!(ModelHttpOptions::default()
        .allow_loopback_for_tests(&release)
        .is_err());
    assert!(
        rightkit_models::http::ModelHttp::for_loopback_tests(Default::default(), &release)
            .is_err()
    );
    let tmp = tempfile::tempdir().unwrap();
    let err = Store::new(tmp.path().join("models"), "t")
        .with_loopback_for_tests(&release)
        .expect_err("a release run is refused");
    assert_eq!(err.code, ErrorCode::InvalidSpec);

    // Without the hook, the default store still refuses loopback.
    let mock = MockServer::start_with(|_: &Request, _: &str| Some(Reply::text("x"))).unwrap();
    let data = blob(4, 100);
    let store = Store::new(tmp.path().join("models"), "t").with_http_options(ModelHttpOptions {
        proxy: ProxyMode::None,
        retries: 0,
        ..Default::default()
    });
    let err = store
        .install(
            &spec(pinned(mock.url("/m.bin"), vec![], &data)),
            &opts(),
            |_| {},
        )
        .unwrap_err();
    assert!(err.message.contains("address denied"), "{err}");
    assert!(mock.seen().is_empty());
}

/// Needs a build with the `test-mode` feature: a real `Mode::Test` is decided from a
/// runner-bound isolated root.
#[cfg(feature = "test-mode")]
#[test]
fn the_loopback_test_hook_works_in_a_test_run_and_reaches_a_local_server() {
    use rightkit_daemon::mode::{decide, AppId, ModeInput};

    struct Input(String);
    impl ModeInput for Input {
        fn env(&self, name: &str) -> Option<String> {
            match name {
                "RIGHTKIT_UNDER_TEST" => Some("1".into()),
                "RIGHTKIT_QA_DATA_DIR" => Some(self.0.clone()),
                _ => None,
            }
        }
    }

    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join(".rightkit-app.json"),
        r#"{"app":"models-hook-test"}"#,
    )
    .unwrap();
    let app = AppId::new("models-hook-test");
    let mode = decide(&app, &Input(root.path().to_string_lossy().into_owned())).unwrap();
    assert!(mode.is_test());

    let data = blob(4, 50_000);
    let body = data.clone();
    let mock = MockServer::start_with(move |_: &Request, _: &str| {
        Some(Reply::bytes("application/octet-stream", body.clone()))
    })
    .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::new(tmp.path().join("models"), "t")
        .with_loopback_for_tests(&mode)
        .unwrap();
    let out = store
        .install(
            &spec(pinned(mock.url("/m.bin"), vec![], &data)),
            &opts(),
            |_| {},
        )
        .unwrap();
    assert_eq!(fs::read(out.dir.join("model.bin")).unwrap(), data);
}
