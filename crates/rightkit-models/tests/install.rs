use rightkit_models::*;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

fn sha(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

struct Server {
    base: String,
    /// "METHOD path range?" log of every request.
    log: Arc<Mutex<Vec<String>>>,
}

/// Local HTTP server: strong ETag, Range support; `truncate_first` paths send
/// only half the body on their first un-ranged request, then drop the socket.
fn serve(routes: HashMap<String, Vec<u8>>, truncate_first: Vec<String>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let log = Arc::new(Mutex::new(Vec::new()));
    let log2 = log.clone();
    let truncated = Arc::new(Mutex::new(Vec::<String>::new()));
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (routes, log, truncated, trunc) = (
                routes.clone(),
                log2.clone(),
                truncated.clone(),
                truncate_first.clone(),
            );
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                let mut range = None;
                loop {
                    let mut h = String::new();
                    reader.read_line(&mut h).unwrap();
                    if h.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = h.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        range = v.trim().trim_end_matches('-').parse::<usize>().ok();
                    }
                }
                log.lock().unwrap().push(format!("{path} range={range:?}"));
                let mut out = stream;
                let Some(body) = routes.get(&path) else {
                    let _ = out.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
                    return;
                };
                let etag = format!("\"{}\"", &sha(body)[..16]);
                let total = body.len();
                match range {
                    Some(start) if start < total => {
                        let _ = write!(out, "HTTP/1.1 206 Partial Content\r\nETag: {etag}\r\nContent-Range: bytes {start}-{}/{total}\r\nContent-Length: {}\r\n\r\n", total - 1, total - start);
                        let _ = out.write_all(&body[start..]);
                    }
                    _ => {
                        let _ = write!(
                            out,
                            "HTTP/1.1 200 OK\r\nETag: {etag}\r\nContent-Length: {total}\r\n\r\n"
                        );
                        let first =
                            trunc.contains(&path) && !truncated.lock().unwrap().contains(&path);
                        if first {
                            truncated.lock().unwrap().push(path.clone());
                            let _ = out.write_all(&body[..total / 2]);
                        } else {
                            let _ = out.write_all(body);
                        }
                    }
                }
            });
        }
    });
    Server { base, log }
}

fn tar_gz(wrapper: &str, member: &str, data: &[u8]) -> Vec<u8> {
    let mut tar = tar::Builder::new(Vec::new());
    let mut h = tar::Header::new_gnu();
    h.set_size(data.len() as u64);
    h.set_mode(0o644);
    h.set_cksum();
    tar.append_data(&mut h, format!("{wrapper}/{member}"), data)
        .unwrap();
    let raw = tar.into_inner().unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&raw).unwrap();
    gz.finish().unwrap()
}

fn blob(seed: u8, n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn file(path: &str, url: String, data: &[u8]) -> FileSpec {
    FileSpec {
        path: path.into(),
        url,
        mirrors: vec![],
        bytes: Some(data.len() as u64),
        sha256: Some(sha(data)),
        platforms: vec![],
        extract: None,
        installed_bytes: None,
        installed_sha256: None,
        shared: None,
    }
}

struct Fixture {
    server: Server,
    spec: ModelSpec,
    v1: Vec<u8>,
    v2: Vec<u8>,
    lib: Vec<u8>,
}

fn fixture() -> Fixture {
    let v1 = blob(1, 300_000);
    let v2 = blob(2, 200_000);
    let lib = blob(9, 5_000);
    let archive = tar_gz("pkg-1.0", "lib/runtime.bin", &lib);
    let other_os = blob(7, 1_000);
    let mut routes = HashMap::new();
    routes.insert("/v1.bin".into(), v1.clone());
    routes.insert("/v2.bin".into(), v2.clone());
    routes.insert("/pkg.tgz".into(), archive.clone());
    routes.insert("/other.bin".into(), other_os.clone());
    let server = serve(routes, vec!["/v1.bin".into()]);
    let u = |p: &str| format!("{}{p}", server.base);

    let mut runtime = file("runtime.bin", u("/pkg.tgz"), &archive);
    runtime.extract = Some(Extract::Member("pkg-1.0/lib/runtime.bin".into()));
    runtime.installed_bytes = Some(lib.len() as u64);
    runtime.installed_sha256 = Some(sha(&lib));
    let mut foreign = file("other.bin", u("/other.bin"), &other_os);
    // Foreign to whichever host runs the test (unknown tokens like "plan9" never match).
    let other_host = if cfg!(windows) {
        "macos-aarch64"
    } else {
        "windows-x86_64"
    };
    foreign.platforms = vec![other_host.into(), "plan9".into()];

    let spec = ModelSpec::new(
        "demo",
        vec![
            RevisionSpec {
                revision: "1".into(),
                quant: Some("q4".into()),
                files: vec![
                    file("model.bin", u("/v1.bin"), &v1),
                    runtime.clone(),
                    foreign,
                ],
                requirements: Default::default(),
                measured: vec![],
            },
            RevisionSpec {
                revision: "2".into(),
                quant: Some("q4".into()),
                files: vec![file("model.bin", u("/v2.bin"), &v2), runtime],
                requirements: Default::default(),
                measured: vec![],
            },
        ],
    )
    .with_label("Demo");
    Fixture {
        server,
        spec,
        v1,
        v2,
        lib,
    }
}

fn opts(rev: &str) -> InstallOptions {
    InstallOptions {
        revision: Some(rev.into()),
        quant: Some("q4".into()),
        ..Default::default()
    }
}

#[test]
fn install_resume_extract_upgrade_rollback_uninstall() {
    let fx = fixture();
    let tmp = tempfile::tempdir().unwrap();
    let store = local(Store::new(tmp.path(), "test-app"));

    // v1: server truncates the first body; the downloader resumes by Range.
    let mut events = Vec::new();
    let out = store
        .install(&fx.spec, &opts("1"), |e| events.push(e))
        .unwrap();
    assert!(!out.already_installed);
    assert_eq!(std::fs::read(out.dir.join("model.bin")).unwrap(), fx.v1);
    assert_eq!(
        std::fs::read(out.dir.join("runtime.bin")).unwrap(),
        fx.lib,
        "member extracted from archive"
    );
    assert!(
        !out.dir.join("other.bin").exists(),
        "foreign-platform file skipped"
    );
    let log = fx.server.log.lock().unwrap().clone();
    assert!(
        !log.iter().any(|l| l.contains("/other.bin")),
        "foreign file never fetched: {log:?}"
    );
    assert!(
        log.iter()
            .any(|l| l.contains("/v1.bin") && l.contains("range=Some(")),
        "resumed with Range: {log:?}"
    );
    assert!(events
        .iter()
        .any(|e| matches!(e, Event::Progress { overall_total, .. } if *overall_total > 0)));
    assert!(matches!(
        events.last(),
        Some(Event::Activated { previous: None, .. })
    ));
    assert!(store.verify("demo", &out.key, VerifyMode::Full));

    // Re-install is idempotent.
    assert!(
        store
            .install(&fx.spec, &opts("1"), |_| {})
            .unwrap()
            .already_installed
    );

    // Upgrade keeps v1 for rollback.
    let up = store.install(&fx.spec, &opts("2"), |_| {}).unwrap();
    assert_eq!(up.previous.as_deref(), Some("1@q4"));
    assert_eq!(
        std::fs::read(store.active_dir("demo").unwrap().join("model.bin")).unwrap(),
        fx.v2
    );

    // Rollback flips the pointer; both revisions still verify.
    assert_eq!(store.rollback("demo", None).unwrap(), "1@q4");
    assert_eq!(
        std::fs::read(store.active_dir("demo").unwrap().join("model.bin")).unwrap(),
        fx.v1
    );
    assert!(store.verify("demo", "2@q4", VerifyMode::Full));

    // A corrupted active revision is not served.
    std::fs::write(
        store
            .revision_dir("demo", "1@q4")
            .unwrap()
            .join("model.bin"),
        b"short",
    )
    .unwrap();
    assert!(store.active_dir("demo").is_none());

    store.uninstall_revision("demo", "1@q4").unwrap();
    assert_eq!(
        store.state("demo").unwrap().active.as_deref(),
        Some("2@q4"),
        "falls back to a verified revision"
    );
    store.uninstall("demo").unwrap();
    assert!(!store.model_dir("demo").unwrap().exists());
}

#[test]
fn pause_keeps_partial_and_next_install_resumes() {
    let fx = fixture();
    let tmp = tempfile::tempdir().unwrap();
    let store = local(Store::new(tmp.path(), "test-app"));
    let o = opts("2");
    let pause = o.pause.clone();
    let err = store
        .install(&fx.spec, &o, |e| {
            if let Event::Progress { file_done, .. } = e {
                if file_done > 50_000 {
                    pause.store(true, Ordering::Relaxed);
                }
            }
        })
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Paused);
    assert!(
        store.state("demo").unwrap().active.is_none(),
        "nothing activated"
    );
    let part = store
        .model_dir("demo")
        .unwrap()
        .join(".staging/2@q4/model.bin.part");
    assert!(
        part.is_file() && part.metadata().unwrap().len() > 0,
        "partial kept"
    );

    store.install(&fx.spec, &opts("2"), |_| {}).unwrap();
    let log = fx.server.log.lock().unwrap().clone();
    assert!(
        log.iter()
            .any(|l| l.contains("/v2.bin") && l.contains("range=Some(")),
        "resumed: {log:?}"
    );
}

#[test]
fn cancel_discards_staging() {
    let fx = fixture();
    let tmp = tempfile::tempdir().unwrap();
    let store = local(Store::new(tmp.path(), "test-app"));
    let o = opts("2");
    let cancel = o.cancel.clone();
    let err = store
        .install(&fx.spec, &o, |e| {
            if let Event::Progress { file_done, .. } = e {
                if file_done > 50_000 {
                    cancel.store(true, Ordering::Relaxed);
                }
            }
        })
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Cancelled);
    assert!(store.state("demo").unwrap().active.is_none());
    assert!(
        !store
            .model_dir("demo")
            .unwrap()
            .join(".staging/2@q4")
            .exists(),
        "cancel discards staging and .part files"
    );
    assert!(store.paused("demo").unwrap().is_empty());
}

#[test]
fn bad_hash_and_low_disk_never_activate() {
    let mut fx = fixture();
    let tmp = tempfile::tempdir().unwrap();
    let store = local(Store::new(tmp.path(), "test-app"));

    // Disk preflight fails before any network use.
    let before = fx.server.log.lock().unwrap().len();
    let mut o = opts("2");
    o.free_bytes_override = Some(1_000);
    assert_eq!(
        store.install(&fx.spec, &o, |_| {}).unwrap_err().code,
        ErrorCode::DiskSpace
    );
    assert_eq!(fx.server.log.lock().unwrap().len(), before);

    // A pinned hash that does not match the served bytes is rejected.
    fx.spec.revisions[1].files[0].sha256 = Some("0".repeat(64));
    let err = store.install(&fx.spec, &opts("2"), |_| {}).unwrap_err();
    assert_eq!(err.code, ErrorCode::HashMismatch);
    assert!(store.state("demo").unwrap().active.is_none());
    assert!(store.active_dir("demo").is_none());
}

#[test]
fn fit_is_measured_only_with_evidence_for_the_profile() {
    let hw = Hardware {
        os: "macos".into(),
        arch: "aarch64".into(),
        memory_gb: Some(24.0),
        gpus: vec![Gpu {
            name: "Apple GPU".into(),
            vram_gb: None,
            unified: true,
            backends: vec!["metal".into()],
        }],
        profile: String::new(),
        source: "override".into(),
        profile_aliases: vec![],
    };
    let hw = Hardware {
        profile: hw.profile_id(),
        ..hw
    };
    assert_eq!(hw.profile, "macos-24gb-unified");
    let small = Requirements {
        unified_memory_gb: Some(8.0),
        ..Default::default()
    };
    let huge = Requirements {
        unified_memory_gb: Some(40.0),
        ..Default::default()
    };
    let m: Measurement =
        serde_json::from_str(r#"{"profile":"macos-24gb","peak_rss_gb":2.3}"#).unwrap();

    assert_eq!(hw.estimate_fit(&small, &[]).kind, FitKind::Estimated);
    let measured = hw.estimate_fit(&small, std::slice::from_ref(&m));
    assert_eq!(
        measured.kind,
        FitKind::Measured,
        "recorded macos-24gb applies to macos-24gb-unified"
    );
    assert_eq!(measured.evidence.unwrap().extra["peak_rss_gb"], 2.3);
    assert_eq!(hw.estimate_fit(&huge, &[]).kind, FitKind::Unsupported);
    assert_eq!(
        hw.estimate_fit(&Requirements::default(), &[]).kind,
        FitKind::Unqualified
    );
    assert!(
        Hardware::probe().memory_gb.is_some_and(|g| g > 0.0),
        "real probe returns RAM"
    );
}

#[test]
fn hostile_ids_and_keys_cannot_leave_the_store_root() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("models");
    let victim = tmp.path().join("other");
    std::fs::create_dir_all(&victim).unwrap();
    std::fs::write(victim.join("precious.txt"), b"keep").unwrap();
    let store = local(Store::new(&root, "test-app"));

    for bad in [
        "../other", "..", ".", "", "a/b", "a\\b", "/abs", "C:", "x.lock", ".hidden", "trail.",
        "CON", "a\0b",
    ] {
        assert!(
            store.uninstall(bad).is_err(),
            "uninstall({bad:?}) must be rejected"
        );
        assert!(
            store.model_dir(bad).is_err(),
            "model_dir({bad:?}) must be rejected"
        );
        assert_eq!(store.state(bad).unwrap_err().code, ErrorCode::InvalidSpec);
    }
    for bad in ["../x", "..", "a/b", "", "UPPER", "a@b@c", "_zz", ".x"] {
        assert!(
            store.revision_dir("demo", bad).is_err(),
            "key {bad:?} must be rejected"
        );
        assert!(store.uninstall_revision("demo", bad).is_err());
        assert!(!store.verify("demo", bad, VerifyMode::Presence));
    }
    // A hostile manifest id never reaches the filesystem.
    let mut fx = fixture();
    fx.spec.id = "../other".into();
    assert_eq!(
        store
            .install(&fx.spec, &opts("1"), |_| {})
            .unwrap_err()
            .code,
        ErrorCode::InvalidSpec
    );
    assert_eq!(std::fs::read(victim.join("precious.txt")).unwrap(), b"keep");
}

#[test]
fn revision_keys_are_injective() {
    let key = |r: &str, q: Option<&str>| {
        RevisionSpec {
            revision: r.into(),
            quant: q.map(String::from),
            files: vec![],
            requirements: Default::default(),
            measured: vec![],
        }
        .checked_key()
    };
    assert_eq!(key("1", Some("q4")).unwrap().as_str(), "1@q4");
    // The old sanitiser mapped all of these to the same directory.
    let inputs = [
        "a/b", "a_b", "a b", "a:b", "A", "a", "a@b", "1.0", "1_0", "..", "é",
    ];
    let keys: Vec<String> = inputs
        .iter()
        .map(|r| key(r, None).unwrap().as_str().to_string())
        .collect();
    let unique: std::collections::BTreeSet<_> = keys.iter().collect();
    assert_eq!(unique.len(), inputs.len(), "collision among {keys:?}");
    assert_ne!(key("a", Some("b")).unwrap(), key("a@b", None).unwrap());
    assert!(key("", None).is_err());
    for k in &keys {
        assert!(RevisionKey::parse(k).is_ok(), "encoded key {k} must parse");
        assert!(!k.contains('/') && !k.contains("..") && !k.starts_with('.'));
    }
}

#[cfg(unix)]
#[test]
fn uninstall_never_follows_symlinks() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("models");
    std::fs::create_dir_all(&root).unwrap();
    let victim = tmp.path().join("victim");
    std::fs::create_dir_all(victim.join("revisions/1@q4")).unwrap();
    std::fs::write(victim.join("precious.txt"), b"keep").unwrap();
    std::fs::write(victim.join("revisions/1@q4/f"), b"keep").unwrap();
    let store = local(Store::new(&root, "test-app"));

    // <root>/demo -> victim: uninstall removes the link only.
    std::os::unix::fs::symlink(&victim, root.join("demo")).unwrap();
    store.uninstall("demo").unwrap();
    assert!(!root.join("demo").exists() && std::fs::symlink_metadata(root.join("demo")).is_err());
    assert_eq!(std::fs::read(victim.join("precious.txt")).unwrap(), b"keep");

    // <root>/demo/revisions -> victim/revisions: mutations refuse to run through it.
    std::fs::create_dir_all(root.join("demo")).unwrap();
    std::os::unix::fs::symlink(victim.join("revisions"), root.join("demo/revisions")).unwrap();
    std::fs::write(
        root.join("demo/state.json"),
        br#"{"id":"demo","active":null,"history":[],"revisions":{"1@q4":{"revision":"1","quant":"q4","installed_at_unix":0,"files":[{"path":"f","bytes":4,"sha256":"x"}]}}}"#,
    )
    .unwrap();
    assert!(store.uninstall_revision("demo", "1@q4").is_err());
    assert!(store.rollback("demo", Some("1@q4")).is_err());
    assert_eq!(
        std::fs::read(victim.join("revisions/1@q4/f")).unwrap(),
        b"keep"
    );
    assert!(store.active_dir("demo").is_none());
}

/// Same-key repair where the repaired payload differs in size from the
/// ledger. Kill the process after each durable step of the swap; the next
/// lock holder must leave the model either fully old or fully new, never missing.
#[test]
fn repair_swap_survives_a_kill_at_every_durable_step() {
    for point in [
        CrashPoint::AfterAside,
        CrashPoint::AfterSwap,
        CrashPoint::AfterState,
    ] {
        let fx = fixture();
        let tmp = tempfile::tempdir().unwrap();
        let store = local(Store::new(tmp.path(), "test-app"));
        let out = store.install(&fx.spec, &opts("2"), |_| {}).unwrap();
        let model = out.dir.join("model.bin");
        assert_eq!(std::fs::read(&model).unwrap(), fx.v2);

        // Corrupt one byte (size unchanged), and re-pin the manifest to v1's payload.
        let mut bad = fx.v2.clone();
        bad[10] ^= 0xff;
        std::fs::write(&model, &bad).unwrap();
        let mut spec = fx.spec.clone();
        spec.revisions[1].files[0] = {
            let mut f = spec.revisions[0].files[0].clone();
            f.path = "model.bin".into();
            f
        };

        let mut o = opts("2");
        o.crash_after = Some(point);
        let err = store.install(&spec, &o, |_| {}).unwrap_err();
        assert!(err.message.contains("injected crash"), "{err}");

        // "Restart": the next operation recovers before anything else.
        let restored = store.recover("demo").unwrap();
        let dir = store.revision_dir("demo", "2@q4").unwrap();
        assert!(
            dir.is_dir(),
            "{point:?}: revision directory must exist after recovery"
        );
        assert!(
            !store.model_dir("demo").unwrap().join(".aside").exists(),
            "{point:?}: aside cleaned up"
        );
        let body = std::fs::read(dir.join("model.bin")).unwrap();
        match point {
            CrashPoint::AfterAside | CrashPoint::AfterSwap => {
                assert!(restored, "{point:?}: previous revision restored");
                assert_eq!(
                    body, bad,
                    "{point:?}: ledger-consistent previous content back in place"
                );
                assert!(store.active_dir("demo").is_some());
            }
            CrashPoint::AfterState => {
                assert!(!restored);
                assert_eq!(body, fx.v1, "{point:?}: committed replacement kept");
                assert!(store.verify("demo", "2@q4", VerifyMode::Full));
            }
        }
        // Retrying always converges on the new payload.
        store.install(&spec, &opts("2"), |_| {}).unwrap();
        assert_eq!(std::fs::read(dir.join("model.bin")).unwrap(), fx.v1);
        assert!(store.verify("demo", "2@q4", VerifyMode::Full));
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
