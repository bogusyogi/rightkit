//! 0.2.1: partial adoption (`adopt_then_install`), per-id legacy alias
//! locations (including inside the store root) and the loopback URL-rewrite
//! test seam. Every assertion reads real files or the mock's request log.

use rightkit_models::*;
use rightkit_qa::mock::{MockServer, Reply};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

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

/// Static binary routes with strong ETags.
fn server(routes: &[(&str, &[u8])]) -> MockServer {
    MockServer::start_static(
        routes
            .iter()
            .map(|(p, b)| {
                let r = Reply::bytes("application/octet-stream", b.to_vec())
                    .with_header("ETag", &format!("\"{}\"", &sha(b)[..16]));
                (Some("GET".to_string()), p.to_string(), r)
            })
            .collect(),
    )
    .unwrap()
}

fn hits(m: &MockServer, route: &str) -> usize {
    m.seen().iter().filter(|s| s.route() == route).count()
}

fn file(path: &str, url: String, data: &[u8]) -> FileSpec {
    FileSpec {
        path: path.into(),
        url,
        bytes: Some(data.len() as u64),
        sha256: Some(sha(data)),
        ..Default::default()
    }
}

fn model(id: &str, revs: Vec<(&str, Vec<FileSpec>)>) -> ModelSpec {
    let revisions = revs
        .into_iter()
        .map(|(r, files)| RevisionSpec {
            revision: r.into(),
            quant: Some("q4".into()),
            files,
            ..Default::default()
        })
        .collect();
    ModelSpec::new(id, revisions)
}

fn opts(rev: &str) -> InstallOptions {
    InstallOptions {
        revision: Some(rev.into()),
        quant: Some("q4".into()),
        ..Default::default()
    }
}

struct Three {
    m: MockServer,
    spec: ModelSpec,
    a: Vec<u8>,
    b: Vec<u8>,
    c: Vec<u8>,
}

/// `demo` rev 1 = a.bin + sub/b.bin + c.bin.
fn three() -> Three {
    let (a, b, c) = (blob(1, 200_000), blob(2, 150_000), blob(3, 120_000));
    let m = server(&[("/a.bin", &a), ("/b.bin", &b), ("/c.bin", &c)]);
    let spec = model(
        "demo",
        vec![(
            "1",
            vec![
                file("a.bin", m.url("/a.bin"), &a),
                file("sub/b.bin", m.url("/b.bin"), &b),
                file("c.bin", m.url("/c.bin"), &c),
            ],
        )],
    );
    Three { m, spec, a, b, c }
}

/// Flat legacy dir: a.bin good, sub/b.bin corrupt (same size), c.bin missing.
fn legacy(dir: &Path, fx: &Three) -> PathBuf {
    fs::create_dir_all(dir.join("sub")).unwrap();
    fs::write(dir.join("a.bin"), &fx.a).unwrap();
    let mut bad = fx.b.clone();
    bad[1000] ^= 0xff;
    fs::write(dir.join("sub/b.bin"), &bad).unwrap();
    dir.to_path_buf()
}

fn from_dir(dir: &Path) -> impl Fn(&FileSpec) -> Option<PathBuf> + '_ {
    move |f| Some(dir.join(&f.path))
}

// ------------------------------------------------------- partial adoption

#[test]
fn partial_adoption_links_the_good_file_and_downloads_only_the_rest() {
    let fx = three();
    let tmp = tempfile::tempdir().unwrap();
    let flat = legacy(&tmp.path().join("flat/demo"), &fx);
    let corrupt_before = fs::read(flat.join("sub/b.bin")).unwrap();
    let store = local(Store::new(tmp.path().join("models"), "test-app"));
    let need = (fx.b.len() + fx.c.len()) as u64;
    let mut o = opts("1");
    // Enough for b + c with headroom, not for all three: proves preflight
    // counts only bytes still needed.
    o.free_bytes_override = Some((need as f64 * 1.1) as u64 + 1);
    assert!((o.free_bytes_override.unwrap() as f64) < (need + fx.a.len() as u64) as f64 * 1.1);
    let mut events = Vec::new();
    let out = store
        .adopt_then_install(&fx.spec, &o, from_dir(&flat), |e| events.push(e))
        .unwrap();
    assert_eq!(out.key, "1@q4");
    assert!(!out.already_installed);

    let adopted: Vec<(&str, bool)> = events
        .iter()
        .filter_map(|e| match e {
            Event::Adopted { path, linked } => Some((path.as_str(), *linked)),
            _ => None,
        })
        .collect();
    assert_eq!(adopted, [("a.bin", true)], "only the good file is adopted");
    assert!(events.iter().any(|e| matches!(
        e,
        Event::Preflight { needed_bytes, .. } if *needed_bytes == need
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        Event::PreflightCredit { credited_bytes } if *credited_bytes == fx.a.len() as u64
    )));
    assert_eq!(hits(&fx.m, "/a.bin"), 0, "the good file is not downloaded");
    assert_eq!(
        hits(&fx.m, "/b.bin"),
        1,
        "the corrupt file is re-downloaded"
    );
    assert_eq!(hits(&fx.m, "/c.bin"), 1, "the missing file is downloaded");

    let dir = store.active_dir("demo").unwrap();
    assert_eq!(fs::read(dir.join("a.bin")).unwrap(), fx.a);
    assert_eq!(fs::read(dir.join("sub/b.bin")).unwrap(), fx.b);
    assert_eq!(fs::read(dir.join("c.bin")).unwrap(), fx.c);
    assert!(store.verify("demo", "1@q4", VerifyMode::Full));
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            fs::metadata(dir.join("a.bin")).unwrap().ino(),
            fs::metadata(flat.join("a.bin")).unwrap().ino(),
            "adopted by hard link, no copy"
        );
    }
    assert_eq!(
        fs::read(flat.join("sub/b.bin")).unwrap(),
        corrupt_before,
        "the legacy corrupt file is never written through"
    );
    assert!(!flat.join("c.bin").exists());

    // Idempotent: a second call reactivates without touching the network.
    let before = fx.m.seen().len();
    let again = store
        .adopt_then_install(&fx.spec, &o, from_dir(&flat), |_| {})
        .unwrap();
    assert!(again.already_installed);
    assert_eq!(fx.m.seen().len(), before);
}

#[test]
fn partial_adoption_fails_closed_and_never_half_records() {
    let fx = three();
    let tmp = tempfile::tempdir().unwrap();
    let flat = legacy(&tmp.path().join("flat/demo"), &fx);
    let store = local(Store::new(tmp.path().join("models"), "test-app"));

    // A download failure (404 for c) records nothing.
    let mut broken = fx.spec.clone();
    broken.revisions[0].files[2].url = fx.m.url("/missing.bin");
    let err = store
        .adopt_then_install(&broken, &opts("1"), from_dir(&flat), |_| {})
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Download, "{err}");
    let st = store.state("demo").unwrap();
    assert!(st.active.is_none() && st.revisions.is_empty());
    assert!(store.resolve("demo").is_none());

    // Unpinned files are refused (require_pinned is forced).
    let mut unpinned = fx.spec.clone();
    unpinned.revisions[0].files[2].sha256 = None;
    let err = store
        .adopt_then_install(&unpinned, &opts("1"), from_dir(&flat), |_| {})
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidSpec);

    // A kill after the swap but before the ledger write records nothing...
    let mut o = opts("1");
    o.crash_after = Some(CrashPoint::AfterSwap);
    assert!(store
        .adopt_then_install(&fx.spec, &o, from_dir(&flat), |_| {})
        .is_err());
    let st = store.state("demo").unwrap();
    assert!(st.active.is_none() && st.revisions.is_empty());
    assert!(store.resolve("demo").is_none());

    // ...and a pause mid-download records nothing either.
    let o = opts("1");
    let pause = o.pause.clone();
    let err = store
        .adopt_then_install(&fx.spec, &o, from_dir(&flat), |e| {
            if matches!(e, Event::FileStart { .. }) {
                pause.store(true, Ordering::Relaxed);
            }
        })
        .unwrap_err();
    assert!(matches!(err.code, ErrorCode::Paused), "{err}");
    assert!(store.state("demo").unwrap().active.is_none());

    // The next call re-verifies everything and completes.
    store
        .adopt_then_install(&fx.spec, &opts("1"), from_dir(&flat), |_| {})
        .unwrap();
    assert!(store.verify("demo", "1@q4", VerifyMode::Full));
    let dir = store.active_dir("demo").unwrap();
    assert_eq!(fs::read(dir.join("sub/b.bin")).unwrap(), fx.b);
}

// ---------------------------------------------------- per-id alias paths

#[test]
fn mapped_alias_inside_the_store_root_follows_install_switch_and_remove() {
    let tmp = tempfile::tempdir().unwrap();
    let (v1, v2) = (blob(5, 50_000), blob(6, 50_000));
    let m = server(&[("/v1", &v1), ("/v2", &v2)]);
    let spec = model(
        "demo",
        vec![
            ("1", vec![file("m.bin", m.url("/v1"), &v1)]),
            ("2", vec![file("m.bin", m.url("/v2"), &v2)]),
        ],
    );
    let root = tmp.path().join("models");
    let alias = root.join("demo").join("r1");
    let a2 = alias.clone();
    let store = local(
        Store::new(&root, "test-app")
            .with_legacy_alias_map(move |id| (id == "demo").then(|| a2.clone())),
    );
    assert_eq!(
        store.legacy_alias_path("demo").unwrap(),
        Some(alias.clone())
    );
    assert_eq!(store.legacy_alias_path("other").unwrap(), None);

    let mut events = Vec::new();
    store
        .install(&spec, &opts("1"), |e| events.push(e))
        .unwrap();
    assert!(events.iter().any(|e| matches!(
        e,
        Event::LegacyAlias {
            status: AliasStatus::Linked,
            ..
        }
    )));
    let rev1 = store.revision_dir("demo", "1@q4").unwrap();
    assert!(fs::symlink_metadata(&alias)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read(alias.join("m.bin")).unwrap(), v1);

    // The alias is not revision storage: the ledger, the revisions dir and
    // verification ignore it, and it is a link, never a directory the store
    // treats as its own.
    let names = |p: &Path| -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(p)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    };
    assert_eq!(names(&root.join("demo/revisions")), ["1@q4"]);
    let visible: Vec<String> = names(&root.join("demo"))
        .into_iter()
        .filter(|n| !n.starts_with('.'))
        .collect();
    assert_eq!(visible, ["r1", "revisions", "state.json"]);
    let st = store.state("demo").unwrap();
    assert_eq!(st.revisions.keys().collect::<Vec<_>>(), ["1@q4"]);
    assert!(store.paused("demo").unwrap().is_empty());
    assert_eq!(store.resolve("demo").unwrap().dir, rev1);

    // Upgrade re-points; rollback re-points back.
    store.install(&spec, &opts("2"), |_| {}).unwrap();
    assert_eq!(fs::read(alias.join("m.bin")).unwrap(), v2);
    assert_eq!(names(&root.join("demo/revisions")), ["1@q4", "2@q4"]);
    store.rollback("demo", None).unwrap();
    assert_eq!(fs::read(alias.join("m.bin")).unwrap(), v1);
    // Removing the active revision falls back and re-points.
    store.uninstall_revision("demo", "1@q4").unwrap();
    assert_eq!(fs::read(alias.join("m.bin")).unwrap(), v2);
    // The target dir was deleted through its real path, not the alias.
    assert!(!rev1.exists());
    store.uninstall("demo").unwrap();
    assert!(fs::symlink_metadata(&alias).is_err());
    assert!(!root.join("demo").exists());
}

#[test]
fn mapped_alias_paths_cannot_collide_with_store_storage_or_escape() {
    let tmp = tempfile::tempdir().unwrap();
    let v1 = blob(5, 50_000);
    let m = server(&[("/v1", &v1)]);
    let spec = model("demo", vec![("1", vec![file("m.bin", m.url("/v1"), &v1)])]);
    let root = tmp.path().join("models");
    local(Store::new(&root, "test-app"))
        .install(&spec, &opts("1"), |_| {})
        .unwrap();
    let elsewhere = tmp.path().join("elsewhere");
    let refused = [
        root.join("demo"),
        root.join("demo").join("revisions"),
        root.join("demo").join("Revisions"),
        root.join("demo").join("revisions").join("1@q4"),
        root.join("demo").join("revisions").join("r1"),
        root.join("demo").join(".staging").join("r1"),
        root.join("demo").join(".aside"),
        root.join("demo").join(".adopt"),
        root.join("demo").join("state.json"),
        root.join("demo").join("STATE.JSON"),
        root.join("demo").join("state.tmp-1"),
        root.join("demo").join("x").join("..").join("revisions"),
        root.join("other").join("r1"),
        root.join("r1"),
        root.join("refs").join("demo"),
        elsewhere.join("demo"),
    ];
    for p in refused {
        let p2 = p.clone();
        let store =
            local(Store::new(&root, "test-app").with_legacy_alias_map(move |_| Some(p2.clone())));
        let err = store.legacy_alias_path("demo").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidSpec, "{}", p.display());
        let sync = store.sync_legacy_alias("demo").unwrap();
        assert_eq!(sync.status, AliasStatus::Failed, "{}", p.display());
    }
    // Storage is untouched by every refusal.
    let store = local(Store::new(&root, "test-app"));
    assert!(store.verify("demo", "1@q4", VerifyMode::Full));
    assert!(fs::metadata(root.join("demo/revisions/1@q4"))
        .unwrap()
        .is_dir());
    assert!(!is_link(&root.join("demo/revisions")));

    // A symlinked parent cannot smuggle an in-store alias elsewhere.
    #[cfg(unix)]
    {
        fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("demo").join("sneaky")).unwrap();
        let p = root.join("demo").join("sneaky").join("r1");
        let store =
            local(Store::new(&root, "test-app").with_legacy_alias_map(move |_| Some(p.clone())));
        assert!(store.legacy_alias_path("demo").is_err());
        fs::remove_file(root.join("demo").join("sneaky")).unwrap();
    }

    // A flat runtime dir outside the store needs the explicit opt-in.
    let qts = tmp.path().join("qts");
    let alias = qts.join("demo");
    let a2 = alias.clone();
    let store = Store::new(&root, "test-app")
        .with_legacy_alias_map(move |_| Some(a2.clone()))
        .with_legacy_alias_allowed_root(&qts);
    assert_eq!(
        store.sync_legacy_alias("demo").unwrap().status,
        AliasStatus::Linked
    );
    assert_eq!(fs::read(alias.join("m.bin")).unwrap(), v1);
    // The table form, and the allowed root itself is not an alias location.
    let mut table = BTreeMap::new();
    table.insert("demo".to_string(), qts.clone());
    let store = Store::new(&root, "test-app")
        .with_legacy_alias_paths(table)
        .with_legacy_alias_allowed_root(&qts);
    assert!(store.legacy_alias_path("demo").is_err());

    // A real directory at a mapped path is never replaced.
    let real = root.join("demo").join("r2");
    fs::create_dir_all(&real).unwrap();
    fs::write(real.join("keep"), b"keep").unwrap();
    let r2 = real.clone();
    let store =
        local(Store::new(&root, "test-app").with_legacy_alias_map(move |_| Some(r2.clone())));
    assert_eq!(
        store.sync_legacy_alias("demo").unwrap().status,
        AliasStatus::SkippedRealPath
    );
    assert_eq!(fs::read(real.join("keep")).unwrap(), b"keep");

    // `None` from the map falls back to the alias root.
    let flat = tmp.path().join("flat");
    let store = Store::new(&root, "test-app")
        .with_legacy_alias_root(&flat)
        .unwrap()
        .with_legacy_alias_map(|_| None);
    assert_eq!(
        store.legacy_alias_path("demo").unwrap(),
        Some(flat.join("demo"))
    );
}

fn is_link(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

// ------------------------------------------------------------ url rewrite

#[test]
fn url_rewrite_sends_downloads_to_a_loopback_mock_only() {
    let tmp = tempfile::tempdir().unwrap();
    let v1 = blob(7, 40_000);
    let m = server(&[("/org/m/resolve/main/m.bin", &v1)]);
    let spec = model(
        "demo",
        vec![(
            "1",
            vec![file(
                "m.bin",
                "https://models.example.invalid/org/m/resolve/main/m.bin".into(),
                &v1,
            )],
        )],
    );
    let store = Store::new(tmp.path().join("models"), "test-app")
        .with_url_rewrite(&m.base)
        .unwrap();
    assert_eq!(
        store.download_url("https://h.example/a/b.bin?x=1"),
        format!("{}/a/b.bin?x=1", m.base)
    );
    store.install(&spec, &opts("1"), |_| {}).unwrap();
    assert_eq!(hits(&m, "/org/m/resolve/main/m.bin"), 1);
    assert!(store.verify("demo", "1@q4", VerifyMode::Full));

    let plain = local(Store::new(tmp.path().join("p"), "a"));
    assert_eq!(plain.download_url("https://h/x"), "https://h/x");
    let prefixed = Store::new(tmp.path().join("p"), "a")
        .with_url_rewrite("http://localhost:9/mirror/")
        .unwrap();
    assert_eq!(
        prefixed.download_url("https://h/x/y.bin"),
        "http://localhost:9/mirror/x/y.bin"
    );
    assert_eq!(
        prefixed.download_url("https://h"),
        "http://localhost:9/mirror/"
    );
    for ok in [
        "http://127.0.0.1:1",
        "http://127.0.0.2",
        "http://[::1]:8080",
        "http://LOCALHOST:3",
    ] {
        assert!(Store::new("r", "a").with_url_rewrite(ok).is_ok(), "{ok}");
    }
    for bad in [
        "https://127.0.0.1:1",
        "http://example.com",
        "http://10.0.0.1:80",
        "http://127.0.0.1.evil.example",
        "http://localhost.evil.example:80",
        "http://user@127.0.0.1:1",
        "http://127.0.0.1:1?x=1",
        "ftp://127.0.0.1",
        "127.0.0.1:80",
        "",
    ] {
        let err = Store::new("r", "a").with_url_rewrite(bad).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidSpec, "{bad}");
    }
}

/// Test stores may reach the local mock server on 127.0.0.1 and nothing else.
fn local(store: Store) -> Store {
    store
        .with_http_options(rightkit_models::http::ModelHttpOptions {
            proxy: rightkit_models::http::ProxyMode::None,
            ..Default::default()
        })
        .with_address_policy(
            rightkit_http::AddressPolicy::public_web()
                .allow_if(vec!["127.0.0.1".to_string()], |ip| ip.is_loopback()),
        )
}
