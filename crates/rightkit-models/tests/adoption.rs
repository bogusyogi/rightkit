//! GenRight adoption gaps (0.2.0): adopt, resolver + legacy alias, preflight
//! credit, shared runtimes + traversal guard, pause/resume vs cancel,
//! hardware requirements/overrides/aliases and remote fit classes. Every
//! assertion reads real files or the local server's request log.

use rightkit_models::*;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

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

struct Server {
    base: String,
    log: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn hits(&self, path: &str) -> Vec<String> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.starts_with(&format!("{path} ")))
            .cloned()
            .collect()
    }
}

/// Strong-ETag server; `ranges = false` ignores `Range` and always sends 200.
fn serve(routes: HashMap<String, Vec<u8>>, ranges: bool) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let log = Arc::new(Mutex::new(Vec::new()));
    let log2 = log.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (routes, log) = (routes.clone(), log2.clone());
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
                match range.filter(|_| ranges) {
                    Some(start) if start < total => {
                        let _ = write!(out, "HTTP/1.1 206 Partial Content\r\nETag: {etag}\r\nContent-Range: bytes {start}-{}/{total}\r\nContent-Length: {}\r\n\r\n", total - 1, total - start);
                        let _ = out.write_all(&body[start..]);
                    }
                    _ => {
                        let _ = write!(
                            out,
                            "HTTP/1.1 200 OK\r\nETag: {etag}\r\nContent-Length: {total}\r\n\r\n"
                        );
                        let _ = out.write_all(body);
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

fn file(path: &str, url: String, data: &[u8]) -> FileSpec {
    FileSpec {
        path: path.into(),
        url,
        bytes: Some(data.len() as u64),
        sha256: Some(sha(data)),
        ..Default::default()
    }
}

fn member(path: &str, url: String, archive: &[u8], member: &str, payload: &[u8]) -> FileSpec {
    FileSpec {
        extract: Some(Extract::Member(member.into())),
        installed_bytes: Some(payload.len() as u64),
        installed_sha256: Some(sha(payload)),
        ..file(path, url, archive)
    }
}

fn model(id: &str, revs: Vec<(&str, Vec<FileSpec>)>) -> ModelSpec {
    ModelSpec {
        id: id.into(),
        label: String::new(),
        revisions: revs
            .into_iter()
            .map(|(r, files)| RevisionSpec {
                revision: r.into(),
                quant: Some("q4".into()),
                files,
                ..Default::default()
            })
            .collect(),
    }
}

fn opts(rev: &str) -> InstallOptions {
    InstallOptions {
        revision: Some(rev.into()),
        quant: Some("q4".into()),
        ..Default::default()
    }
}

struct Fx {
    server: Server,
    spec: ModelSpec,
    v1: Vec<u8>,
    v2: Vec<u8>,
    lib: Vec<u8>,
    archive: Vec<u8>,
}

/// `demo`: revision 1 = model.bin (v1) + runtime.bin (member of pkg.tgz);
/// revision 2 = model.bin (v2) + runtime.bin.
fn fx_with(ranges: bool) -> Fx {
    let (v1, v2, lib) = (blob(1, 300_000), blob(2, 300_000), blob(9, 5_000));
    let archive = tar_gz("pkg-1.0", "lib/runtime.bin", &lib);
    let mut routes = HashMap::new();
    routes.insert("/v1.bin".to_string(), v1.clone());
    routes.insert("/v2.bin".to_string(), v2.clone());
    routes.insert("/pkg.tgz".to_string(), archive.clone());
    let server = serve(routes, ranges);
    let u = |p: &str| format!("{}{p}", server.base);
    let rt = member(
        "runtime.bin",
        u("/pkg.tgz"),
        &archive,
        "pkg-1.0/lib/runtime.bin",
        &lib,
    );
    let spec = model(
        "demo",
        vec![
            ("1", vec![file("model.bin", u("/v1.bin"), &v1), rt.clone()]),
            ("2", vec![file("model.bin", u("/v2.bin"), &v2), rt]),
        ],
    );
    Fx {
        server,
        spec,
        v1,
        v2,
        lib,
        archive,
    }
}

fn fx() -> Fx {
    fx_with(true)
}

/// A pre-store flat install of revision `rev` (model.bin + runtime.bin).
fn legacy_install(dir: &Path, model_bytes: &[u8], lib: &[u8]) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("model.bin"), model_bytes).unwrap();
    fs::write(dir.join("runtime.bin"), lib).unwrap();
    dir.to_path_buf()
}

fn from_dir(dir: &Path) -> impl Fn(&FileSpec) -> Option<PathBuf> + '_ {
    move |f| Some(dir.join(&f.path))
}

// ------------------------------------------------------------------ adopt

#[test]
fn adopt_verifies_hard_links_and_records_without_download() {
    let fx = fx();
    let tmp = tempfile::tempdir().unwrap();
    let store = local(Store::new(tmp.path().join("models"), "test-app"));
    let legacy = legacy_install(&tmp.path().join("flat/demo"), &fx.v1, &fx.lib);

    let mut events = Vec::new();
    let out = store
        .adopt_with(&fx.spec, &opts("1"), from_dir(&legacy), |e| events.push(e))
        .unwrap();
    assert!(!out.already_installed);
    assert_eq!(out.key, "1@q4");
    assert!(
        fx.server.log.lock().unwrap().is_empty(),
        "adopt never touches the network"
    );
    assert!(store.verify("demo", "1@q4", VerifyMode::Full));
    assert_eq!(store.state("demo").unwrap().active.as_deref(), Some("1@q4"));
    assert_eq!(fs::read(out.dir.join("model.bin")).unwrap(), fx.v1);
    assert_eq!(fs::read(out.dir.join("runtime.bin")).unwrap(), fx.lib);
    let linked: Vec<bool> = events
        .iter()
        .filter_map(|e| match e {
            Event::Adopted { linked, .. } => Some(*linked),
            _ => None,
        })
        .collect();
    assert_eq!(linked, vec![true, true], "same volume prefers hard links");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            fs::metadata(out.dir.join("model.bin")).unwrap().ino(),
            fs::metadata(legacy.join("model.bin")).unwrap().ino(),
            "hard link, not a copy"
        );
    }
    assert!(
        legacy.join("model.bin").is_file(),
        "the legacy install is left in place"
    );
    assert!(!store.model_dir("demo").unwrap().join(".adopt").exists());

    // Adopting an intact installed revision is idempotent.
    assert!(
        store
            .adopt(&fx.spec, &opts("1"), from_dir(&legacy))
            .unwrap()
            .already_installed
    );
}

#[test]
fn adopt_fails_closed_and_never_records_a_partial_adoption() {
    let fx = fx();
    let tmp = tempfile::tempdir().unwrap();
    let store = local(Store::new(tmp.path().join("models"), "test-app"));
    // A good, active revision 1 that every failed adoption must leave alone.
    store.install(&fx.spec, &opts("1"), |_| {}).unwrap();
    let before = store.state("demo").unwrap();

    let check_untouched = |label: &str| {
        let st = store.state("demo").unwrap();
        assert_eq!(st.active.as_deref(), Some("1@q4"), "{label}");
        assert_eq!(
            st.revisions.keys().collect::<Vec<_>>(),
            before.revisions.keys().collect::<Vec<_>>(),
            "{label}: nothing recorded"
        );
        assert!(
            !store.revision_dir("demo", "2@q4").unwrap().exists(),
            "{label}: no revision dir"
        );
        assert!(
            !store.model_dir("demo").unwrap().join(".adopt").exists(),
            "{label}: adoption tree discarded"
        );
        assert!(store.verify("demo", "1@q4", VerifyMode::Full), "{label}");
    };

    // Second file has the right size but a flipped byte.
    let mut bad_lib = fx.lib.clone();
    bad_lib[7] ^= 0xff;
    let d = legacy_install(&tmp.path().join("hash"), &fx.v2, &bad_lib);
    let e = store.adopt(&fx.spec, &opts("2"), from_dir(&d)).unwrap_err();
    assert_eq!(e.code, ErrorCode::HashMismatch, "{e}");
    check_untouched("hash mismatch");
    assert_eq!(
        fs::read(d.join("runtime.bin")).unwrap(),
        bad_lib,
        "the source is never modified"
    );

    // Wrong size.
    let d = legacy_install(&tmp.path().join("size"), &fx.v2[..1000], &fx.lib);
    let e = store.adopt(&fx.spec, &opts("2"), from_dir(&d)).unwrap_err();
    assert_eq!(e.code, ErrorCode::SizeMismatch, "{e}");
    check_untouched("size mismatch");

    // One file not supplied at all.
    let d = legacy_install(&tmp.path().join("missing"), &fx.v2, &fx.lib);
    let e = store
        .adopt(&fx.spec, &opts("2"), |f: &FileSpec| {
            (f.path == "model.bin").then(|| d.join("model.bin"))
        })
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound, "{e}");
    check_untouched("missing file");

    // Supplied path that does not exist.
    let e = store
        .adopt(&fx.spec, &opts("2"), |f: &FileSpec| {
            Some(tmp.path().join("nowhere").join(&f.path))
        })
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound, "{e}");
    check_untouched("nonexistent source");

    // Unpinned files cannot be verified, so they cannot be adopted.
    let mut unpinned = fx.spec.clone();
    unpinned.revisions[1].files[0].sha256 = None;
    let e = store
        .adopt(&unpinned, &opts("2"), from_dir(&d))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidSpec, "{e}");
    // An unpacked TarGz tree has no per-file pins.
    let mut tree = fx.spec.clone();
    tree.revisions[1].files[1].extract = Some(Extract::TarGz);
    let e = store.adopt(&tree, &opts("2"), from_dir(&d)).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidSpec, "{e}");
    check_untouched("invalid specs");
    assert!(fx.server.hits("/v2.bin").is_empty());

    // The good copy adopts and upgrades.
    let out = store.adopt(&fx.spec, &opts("2"), from_dir(&d)).unwrap();
    assert_eq!(out.previous.as_deref(), Some("1@q4"));
    assert!(store.verify("demo", "2@q4", VerifyMode::Full));
}

#[test]
fn adopt_survives_a_kill_at_every_durable_step() {
    let fx = fx();
    // Fresh key: the swap's only durable steps are the rename and the ledger.
    for point in [CrashPoint::AfterSwap, CrashPoint::AfterState] {
        let tmp = tempfile::tempdir().unwrap();
        let store = local(Store::new(tmp.path().join("models"), "test-app"));
        let d = legacy_install(&tmp.path().join("flat"), &fx.v1, &fx.lib);
        let mut o = opts("1");
        o.crash_after = Some(point);
        let e = store.adopt(&fx.spec, &o, from_dir(&d)).unwrap_err();
        assert!(e.message.contains("injected crash"), "{e}");
        store.recover("demo").unwrap();
        let st = store.state("demo").unwrap();
        match point {
            CrashPoint::AfterSwap => {
                assert!(st.active.is_none(), "{point:?}: not recorded");
                assert!(st.revisions.is_empty(), "{point:?}: not recorded");
                assert!(store.active_dir("demo").is_none());
            }
            _ => {
                assert_eq!(st.active.as_deref(), Some("1@q4"));
                assert!(store.verify("demo", "1@q4", VerifyMode::Full));
            }
        }
        // Retrying converges.
        store.adopt(&fx.spec, &opts("1"), from_dir(&d)).unwrap();
        assert!(store.verify("demo", "1@q4", VerifyMode::Full));
    }

    // Same-key replacement killed mid-swap: recovery restores the previous tree.
    for point in [
        CrashPoint::AfterAside,
        CrashPoint::AfterSwap,
        CrashPoint::AfterState,
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let store = local(Store::new(tmp.path().join("models"), "test-app"));
        store.install(&fx.spec, &opts("1"), |_| {}).unwrap();
        let dir = store.revision_dir("demo", "1@q4").unwrap();
        // Corrupt the installed copy (same size) so adopt replaces it.
        let mut bad = fx.v1.clone();
        bad[3] ^= 0xff;
        fs::write(dir.join("model.bin"), &bad).unwrap();
        let d = legacy_install(&tmp.path().join("flat"), &fx.v1, &fx.lib);
        let mut o = opts("1");
        o.crash_after = Some(point);
        store.adopt(&fx.spec, &o, from_dir(&d)).unwrap_err();
        store.recover("demo").unwrap();
        assert!(dir.is_dir(), "{point:?}: revision dir present");
        let body = fs::read(dir.join("model.bin")).unwrap();
        if point == CrashPoint::AfterAside {
            assert_eq!(
                body, bad,
                "{point:?}: previous tree put back, never missing"
            );
        } else {
            // The swapped-in tree matches the ledger (same pins), so it is kept.
            assert_eq!(body, fx.v1, "{point:?}: adopted tree kept");
            assert!(store.verify("demo", "1@q4", VerifyMode::Full));
        }
        assert!(!store.model_dir("demo").unwrap().join(".aside").exists());
        store.adopt(&fx.spec, &opts("1"), from_dir(&d)).unwrap();
        assert!(store.verify("demo", "1@q4", VerifyMode::Full));
    }

    // A half-built adoption tree left by a killed process is discarded.
    let tmp = tempfile::tempdir().unwrap();
    let store = local(Store::new(tmp.path().join("models"), "test-app"));
    let junk = store.model_dir("demo").unwrap().join(".adopt/1@q4");
    fs::create_dir_all(&junk).unwrap();
    fs::write(junk.join("model.bin"), b"half").unwrap();
    store.recover("demo").unwrap();
    assert!(!junk.exists());
    assert!(store.state("demo").unwrap().revisions.is_empty());
}

// ------------------------------------------------ resolver + legacy alias

#[cfg(unix)]
#[test]
fn legacy_alias_follows_install_switch_and_remove() {
    let fx = fx();
    let tmp = tempfile::tempdir().unwrap();
    let flat = tmp.path().join("flat");
    let store = local(
        Store::new(tmp.path().join("models"), "test-app")
            .with_legacy_alias_root(&flat)
            .unwrap(),
    );
    assert!(
        Store::new(tmp.path().join("models"), "x")
            .with_legacy_alias_root(tmp.path().join("models"))
            .is_err(),
        "alias root may not be the store root"
    );
    let alias = flat.join("demo");
    assert_eq!(
        store.legacy_alias_path("demo").unwrap(),
        Some(alias.clone())
    );
    let target = || fs::read_link(&alias).ok();
    let rev = |k: &str| store.revision_dir("demo", k).unwrap();

    assert!(store.resolve("demo").is_none());
    assert_eq!(
        store.resolve_or_legacy("demo", &tmp.path().join("nope")),
        None
    );

    let mut events = Vec::new();
    store
        .install(&fx.spec, &opts("1"), |e| events.push(e))
        .unwrap();
    assert!(events.iter().any(|e| matches!(
        e,
        Event::LegacyAlias {
            status: AliasStatus::Linked,
            ..
        }
    )));
    assert_eq!(target(), Some(rev("1@q4")));
    assert_eq!(fs::read(alias.join("model.bin")).unwrap(), fx.v1);
    let r = store.resolve("demo").unwrap();
    assert_eq!((r.key.as_str(), r.dir.clone()), ("1@q4", rev("1@q4")));

    store.install(&fx.spec, &opts("2"), |_| {}).unwrap();
    assert_eq!(target(), Some(rev("2@q4")), "install re-points");
    assert_eq!(fs::read(alias.join("model.bin")).unwrap(), fx.v2);

    store.rollback("demo", None).unwrap();
    assert_eq!(target(), Some(rev("1@q4")), "rollback re-points");

    store.uninstall_revision("demo", "1@q4").unwrap();
    assert_eq!(target(), Some(rev("2@q4")), "removal falls back");
    assert_eq!(store.resolve("demo").unwrap().key, "2@q4");

    store.uninstall_revision("demo", "2@q4").unwrap();
    assert!(
        fs::symlink_metadata(&alias).is_err(),
        "no active revision: alias removed"
    );

    store.install(&fx.spec, &opts("2"), |_| {}).unwrap();
    assert_eq!(target(), Some(rev("2@q4")));
    assert_eq!(
        store.sync_legacy_alias("demo").unwrap().status,
        AliasStatus::Unchanged
    );
    store.uninstall("demo").unwrap();
    assert!(
        fs::symlink_metadata(&alias).is_err(),
        "uninstall removes it"
    );

    // A real legacy directory at the alias path is never replaced.
    let legacy = legacy_install(&alias, &fx.v1, &fx.lib);
    let out = store
        .adopt(&fx.spec, &opts("1"), from_dir(&legacy))
        .unwrap();
    assert!(fs::symlink_metadata(&alias).unwrap().is_dir());
    assert_eq!(
        store.sync_legacy_alias("demo").unwrap().status,
        AliasStatus::SkippedRealPath
    );
    assert_eq!(fs::read(alias.join("model.bin")).unwrap(), fx.v1);
    assert_eq!(
        store.resolve_or_legacy("demo", &alias),
        Some(out.dir.clone()),
        "store revision wins over the flat dir"
    );
    // Once the app moves the adopted flat dir away, the alias takes over.
    fs::remove_dir_all(&alias).unwrap();
    assert_eq!(
        store.sync_legacy_alias("demo").unwrap().status,
        AliasStatus::Linked
    );
    assert_eq!(fs::read(alias.join("model.bin")).unwrap(), fx.v1);

    // A store without the alias opt-in never creates one.
    let plain = local(Store::new(tmp.path().join("models2"), "test-app"));
    plain.install(&fx.spec, &opts("1"), |_| {}).unwrap();
    assert_eq!(
        plain.sync_legacy_alias("demo").unwrap().status,
        AliasStatus::Disabled
    );
}

#[test]
fn resolve_or_legacy_falls_back_to_the_flat_dir() {
    let fx = fx();
    let tmp = tempfile::tempdir().unwrap();
    let store = local(Store::new(tmp.path().join("models"), "test-app"));
    let legacy = legacy_install(&tmp.path().join("flat/demo"), &fx.v1, &fx.lib);
    assert_eq!(
        store.resolve_or_legacy("demo", &legacy),
        Some(legacy.clone())
    );
    store.install(&fx.spec, &opts("1"), |_| {}).unwrap();
    assert_eq!(
        store.resolve_or_legacy("demo", &legacy),
        Some(store.revision_dir("demo", "1@q4").unwrap())
    );
}

// --------------------------------------------------------- preflight credit

#[test]
fn preflight_credits_staged_and_installed_files_with_matching_pins() {
    let fx = fx();
    let tmp = tempfile::tempdir().unwrap();
    let store = local(Store::new(tmp.path().join("models"), "test-app"));
    let total = fx.spec.revisions[1].required_disk_bytes(&Platform::current());

    // A matching model.bin already staged (e.g. linked in by the app).
    let staged = store.model_dir("demo").unwrap().join(".staging/2@q4");
    fs::create_dir_all(&staged).unwrap();
    fs::write(staged.join("model.bin"), &fx.v2).unwrap();
    let remaining = total - fx.v2.len() as u64;
    let mut o = opts("2");
    // Enough for what is missing, not for the whole revision.
    o.free_bytes_override = Some(remaining * 2);
    assert!(remaining * 2 < total);
    let mut events = Vec::new();
    store.install(&fx.spec, &o, |e| events.push(e)).unwrap();
    let credited = events.iter().find_map(|e| match e {
        Event::PreflightCredit { credited_bytes } => Some(*credited_bytes),
        _ => None,
    });
    let needed = events.iter().find_map(|e| match e {
        Event::Preflight { needed_bytes, .. } => Some(*needed_bytes),
        _ => None,
    });
    assert_eq!(credited, Some(fx.v2.len() as u64));
    assert_eq!(needed, Some(remaining));
    assert!(fx.server.hits("/v2.bin").is_empty(), "staged file reused");
    assert!(store.verify("demo", "2@q4", VerifyMode::Full));

    // A staged file with the right size but wrong bytes earns no credit.
    let tmp2 = tempfile::tempdir().unwrap();
    let store2 = local(Store::new(tmp2.path().join("models"), "test-app"));
    let staged = store2.model_dir("demo").unwrap().join(".staging/2@q4");
    fs::create_dir_all(&staged).unwrap();
    let mut bad = fx.v2.clone();
    bad[0] ^= 1;
    fs::write(staged.join("model.bin"), &bad).unwrap();
    let mut o = opts("2");
    o.free_bytes_override = Some(remaining * 2);
    assert_eq!(
        store2.install(&fx.spec, &o, |_| {}).unwrap_err().code,
        ErrorCode::DiskSpace
    );

    // Repair: intact files of the installed same-key revision are linked into
    // staging, credited, and not downloaded again.
    let dir = store.revision_dir("demo", "2@q4").unwrap();
    fs::write(dir.join("runtime.bin"), b"corrupt").unwrap();
    assert!(!store.verify("demo", "2@q4", VerifyMode::Full));
    let pkg_before = fx.server.hits("/pkg.tgz").len();
    let mut o = opts("2");
    o.free_bytes_override = Some(remaining * 2);
    let mut events = Vec::new();
    store.install(&fx.spec, &o, |e| events.push(e)).unwrap();
    assert!(events.iter().any(|e| matches!(
        e,
        Event::PreflightCredit { credited_bytes } if *credited_bytes == fx.v2.len() as u64
    )));
    assert!(
        fx.server.hits("/v2.bin").is_empty(),
        "intact file not refetched"
    );
    assert_eq!(fx.server.hits("/pkg.tgz").len(), pkg_before + 1);
    assert!(store.verify("demo", "2@q4", VerifyMode::Full));
    assert_eq!(fs::read(dir.join("runtime.bin")).unwrap(), fx.lib);
}

// ------------------------------------------- shared runtimes + traversal

fn ort_spec(fx: &Fx, id: &str, lib_path: &str) -> ModelSpec {
    let mut rt = member(
        lib_path,
        format!("{}/pkg.tgz", fx.server.base),
        &fx.archive,
        "pkg-1.0/lib/runtime.bin",
        &fx.lib,
    );
    rt.shared = Some("onnxruntime".into());
    model(
        id,
        vec![(
            "1",
            vec![
                file("model.bin", format!("{}/v1.bin", fx.server.base), &fx.v1),
                rt,
            ],
        )],
    )
}

#[test]
fn shared_runtime_files_live_outside_the_revision_and_are_reused() {
    let fx = fx();
    let tmp = tempfile::tempdir().unwrap();
    let shared = tmp.path().join("runtimes/ort");
    let store = local(
        Store::new(tmp.path().join("models"), "test-app")
            .with_shared_root("onnxruntime", &shared)
            .unwrap(),
    );
    let lib_at = shared.join("1.22/libonnxruntime.dylib");

    let a = ort_spec(&fx, "tts-a", "1.22/libonnxruntime.dylib");
    let out = store.install(&a, &opts("1"), |_| {}).unwrap();
    assert_eq!(fs::read(&lib_at).unwrap(), fx.lib);
    assert!(!out.dir.join("1.22").exists(), "not inside the revision");
    let rec = &store.state("tts-a").unwrap().revisions["1@q4"];
    let shared_rec = rec.files.iter().find(|f| f.shared.is_some()).unwrap();
    assert_eq!(shared_rec.path, "1.22/libonnxruntime.dylib");
    assert!(store.verify("tts-a", "1@q4", VerifyMode::Full));
    assert!(store.active_dir("tts-a").is_some());
    assert_eq!(fx.server.hits("/pkg.tgz").len(), 1);

    // A second model sharing the runtime: no download, full credit.
    let b = ort_spec(&fx, "tts-b", "1.22/libonnxruntime.dylib");
    let mut events = Vec::new();
    store.install(&b, &opts("1"), |e| events.push(e)).unwrap();
    assert_eq!(fx.server.hits("/pkg.tgz").len(), 1, "shared file reused");
    let want = fx.archive.len() as u64 + fx.lib.len() as u64;
    assert!(events.iter().any(|e| matches!(
        e,
        Event::PreflightCredit { credited_bytes } if *credited_bytes == want
    )));

    // Uninstall leaves the shared runtime for other models.
    store.uninstall("tts-a").unwrap();
    assert_eq!(fs::read(&lib_at).unwrap(), fx.lib);
    assert!(store.verify("tts-b", "1@q4", VerifyMode::Full));

    // Shared bytes are verified: a corrupted runtime fails the model.
    fs::write(&lib_at, b"tampered").unwrap();
    assert!(!store.verify("tts-b", "1@q4", VerifyMode::Size));
    // ...and are never overwritten: a different file at the path is a conflict.
    let c = ort_spec(&fx, "tts-c", "1.22/libonnxruntime.dylib");
    let e = store.install(&c, &opts("1"), |_| {}).unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict, "{e}");
    assert_eq!(fs::read(&lib_at).unwrap(), b"tampered");
    assert!(store.state("tts-c").unwrap().active.is_none());

    // A store that does not declare the kind fails closed.
    let bare = local(Store::new(tmp.path().join("models"), "test-app"));
    assert!(!bare.verify("tts-b", "1@q4", VerifyMode::Presence));
    let e = bare
        .install(&ort_spec(&fx, "tts-d", "x.dylib"), &opts("1"), |_| {})
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidSpec, "{e}");
}

#[test]
fn adopt_places_a_shared_runtime_from_an_existing_install() {
    let fx = fx();
    let tmp = tempfile::tempdir().unwrap();
    let shared = tmp.path().join("runtimes/ort");
    let store = local(
        Store::new(tmp.path().join("models"), "test-app")
            .with_shared_root("onnxruntime", &shared)
            .unwrap(),
    );
    let legacy = tmp.path().join("flat");
    fs::create_dir_all(legacy.join("runtime")).unwrap();
    fs::write(legacy.join("model.bin"), &fx.v1).unwrap();
    fs::write(legacy.join("runtime/libort.dylib"), &fx.lib).unwrap();
    let spec = ort_spec(&fx, "tts", "libort.dylib");
    store
        .adopt(&spec, &opts("1"), |f: &FileSpec| {
            Some(if f.shared.is_some() {
                legacy.join("runtime/libort.dylib")
            } else {
                legacy.join("model.bin")
            })
        })
        .unwrap();
    assert_eq!(fs::read(shared.join("libort.dylib")).unwrap(), fx.lib);
    assert!(store.verify("tts", "1@q4", VerifyMode::Full));
    assert!(fx.server.log.lock().unwrap().is_empty());
}

#[test]
fn path_traversal_is_rejected_for_revision_and_shared_files() {
    let fx = fx();
    let tmp = tempfile::tempdir().unwrap();
    let shared = tmp.path().join("shared");
    let store = local(
        Store::new(tmp.path().join("models"), "test-app")
            .with_shared_root("onnxruntime", &shared)
            .unwrap(),
    );
    for bad_kind in ["../x", "a/b", "", ".hidden", "C:"] {
        assert!(
            Store::new(tmp.path(), "t")
                .with_shared_root(bad_kind, &shared)
                .is_err(),
            "kind {bad_kind:?}"
        );
    }
    for bad in ["../evil", "/abs", "a/../b", "a//b", "a\\b", "C:x", "."] {
        // Shared file.
        let spec = ort_spec(&fx, "tts", bad);
        let e = store.install(&spec, &opts("1"), |_| {}).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidSpec, "shared {bad:?}");
        let e = store.adopt(&spec, &opts("1"), |_| None).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidSpec, "adopt shared {bad:?}");
        assert!(store.shared_path("onnxruntime", bad).is_err());
        // Revision file.
        let mut spec = fx.spec.clone();
        spec.revisions[0].files[0].path = bad.into();
        let e = store.install(&spec, &opts("1"), |_| {}).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidSpec, "revision {bad:?}");
    }
    // A kind the store does not declare can never name a location.
    let mut spec = ort_spec(&fx, "tts", "lib.dylib");
    spec.revisions[0].files[1].shared = Some("../outside".into());
    assert_eq!(
        store.install(&spec, &opts("1"), |_| {}).unwrap_err().code,
        ErrorCode::InvalidSpec
    );
    // Shared files must be pinned.
    let mut spec = ort_spec(&fx, "tts", "lib.dylib");
    spec.revisions[0].files[1].installed_sha256 = None;
    assert_eq!(
        store.install(&spec, &opts("1"), |_| {}).unwrap_err().code,
        ErrorCode::InvalidSpec
    );
    assert!(fx.server.log.lock().unwrap().is_empty(), "nothing fetched");
    assert!(!tmp.path().join("evil").exists());
    assert!(!shared.exists() || fs::read_dir(&shared).unwrap().next().is_none());
}

#[cfg(unix)]
#[test]
fn shared_placement_never_writes_through_a_symlink() {
    let fx = fx();
    let tmp = tempfile::tempdir().unwrap();
    let shared = tmp.path().join("shared");
    fs::create_dir_all(&shared).unwrap();
    let victim = tmp.path().join("victim.bin");
    fs::write(&victim, b"keep").unwrap();
    std::os::unix::fs::symlink(&victim, shared.join("lib.dylib")).unwrap();
    let store = local(
        Store::new(tmp.path().join("models"), "test-app")
            .with_shared_root("onnxruntime", &shared)
            .unwrap(),
    );
    assert!(store
        .install(&ort_spec(&fx, "tts", "lib.dylib"), &opts("1"), |_| {})
        .is_err());
    assert_eq!(fs::read(&victim).unwrap(), b"keep");
    assert!(store.state("tts").unwrap().active.is_none());
}

// ----------------------------------------------------------- pause/resume

fn pause_midway(store: &Store, spec: &ModelSpec, rev: &str) -> (Error, Vec<Event>) {
    let o = opts(rev);
    let pause = o.pause.clone();
    let mut events = Vec::new();
    let err = store
        .install(spec, &o, |e| {
            if let Event::Progress { file_done, .. } = &e {
                if *file_done > 50_000 {
                    pause.store(true, Ordering::Relaxed);
                }
            }
            events.push(e);
        })
        .unwrap_err();
    (err, events)
}

#[test]
fn pause_keeps_staging_and_resumes_by_range() {
    let fx = fx();
    let tmp = tempfile::tempdir().unwrap();
    let store = local(Store::new(tmp.path().join("models"), "test-app"));
    let (err, events) = pause_midway(&store, &fx.spec, "2");
    assert_eq!(err.code, ErrorCode::Paused, "{err}");
    let paused = store.paused("demo").unwrap();
    assert_eq!(paused.len(), 1);
    assert_eq!(paused[0].key, "2@q4");
    assert_eq!(paused[0].revision, "2");
    assert!(paused[0].staged_bytes > 50_000);
    assert!(events.iter().any(|e| matches!(
        e,
        Event::Paused { key, staged_bytes } if key == "2@q4" && *staged_bytes == paused[0].staged_bytes
    )));
    let part = store
        .model_dir("demo")
        .unwrap()
        .join(".staging/2@q4/model.bin.part");
    let kept = part.metadata().unwrap().len();
    assert!(kept > 0 && kept < fx.v2.len() as u64);
    assert!(store.state("demo").unwrap().active.is_none());

    let mut events = Vec::new();
    store
        .install(&fx.spec, &opts("2"), |e| events.push(e))
        .unwrap();
    assert!(events
        .iter()
        .any(|e| matches!(e, Event::Resumed { key, .. } if key == "2@q4")));
    let hits = fx.server.hits("/v2.bin");
    assert_eq!(hits.len(), 2, "{hits:?}");
    assert_eq!(
        hits[1],
        format!("/v2.bin range=Some({kept})"),
        "resumed at the kept offset"
    );
    assert!(store.verify("demo", "2@q4", VerifyMode::Full));
    assert!(store.paused("demo").unwrap().is_empty());
}

#[test]
fn pause_without_server_ranges_restarts_cleanly() {
    let fx = fx_with(false);
    let tmp = tempfile::tempdir().unwrap();
    let store = local(Store::new(tmp.path().join("models"), "test-app"));
    let (err, _) = pause_midway(&store, &fx.spec, "2");
    assert_eq!(err.code, ErrorCode::Paused);
    store.install(&fx.spec, &opts("2"), |_| {}).unwrap();
    let hits = fx.server.hits("/v2.bin");
    assert_eq!(hits.len(), 2, "{hits:?}");
    assert!(hits[1].contains("range=Some("), "asked to resume: {hits:?}");
    assert_eq!(
        fs::read(store.active_dir("demo").unwrap().join("model.bin")).unwrap(),
        fx.v2,
        "a 200 reply restarts from zero; no spliced bytes"
    );
    assert!(store.verify("demo", "2@q4", VerifyMode::Full));
}

#[test]
fn a_paused_install_can_be_cancelled_and_cancel_wins_over_pause() {
    let fx = fx();
    let tmp = tempfile::tempdir().unwrap();
    let store = local(Store::new(tmp.path().join("models"), "test-app"));
    let (err, _) = pause_midway(&store, &fx.spec, "2");
    assert_eq!(err.code, ErrorCode::Paused);
    store.discard_staging("demo", "2@q4").unwrap();
    assert!(store.paused("demo").unwrap().is_empty());
    assert!(!store
        .model_dir("demo")
        .unwrap()
        .join(".staging/2@q4")
        .exists());

    let o = opts("2");
    o.pause.store(true, Ordering::Relaxed);
    o.cancel.store(true, Ordering::Relaxed);
    assert_eq!(
        store.install(&fx.spec, &o, |_| {}).unwrap_err().code,
        ErrorCode::Cancelled
    );
    assert!(!store
        .model_dir("demo")
        .unwrap()
        .join(".staging/2@q4")
        .exists());
    // Pause before the first byte still records a resumable marker.
    let o = opts("1");
    o.pause.store(true, Ordering::Relaxed);
    assert_eq!(
        store.install(&fx.spec, &o, |_| {}).unwrap_err().code,
        ErrorCode::Paused
    );
    assert_eq!(store.paused("demo").unwrap()[0].key, "1@q4");
    assert!(fx.server.hits("/v1.bin").is_empty());
    store.install(&fx.spec, &opts("1"), |_| {}).unwrap();
    assert_eq!(
        fs::read(store.active_dir("demo").unwrap().join("model.bin")).unwrap(),
        fx.v1
    );
}

// ------------------------------------------------------------- hardware

fn hw(json: &str) -> Hardware {
    Hardware::from_override_json(json).unwrap()
}

#[test]
fn cuda_and_mps_requirements_apply_to_their_hardware_class() {
    let req = Requirements {
        memory_gb: Some(32.0),
        vram_gb: Some(20.0),
        cuda_vram_gb: Some(7.5),
        mps_memory_gb: Some(9.0),
        unified_memory_gb: Some(30.0),
        ..Default::default()
    };
    let nvidia = hw(
        r#"{"os":"windows","memory_gb":32,"gpus":[{"name":"NVIDIA GeForce RTX 4070 Laptop GPU","vram_gb":8,"backends":["cuda","vulkan"]}]}"#,
    );
    assert_eq!(nvidia.required_gb(&req), Some(7.5));
    assert_eq!(nvidia.estimate_fit(&req, &[]).kind, FitKind::Estimated);
    let amd = hw(
        r#"{"os":"windows","memory_gb":32,"gpus":[{"name":"AMD Radeon","vram_gb":8,"backends":["vulkan"]}]}"#,
    );
    assert_eq!(
        amd.required_gb(&req),
        Some(20.0),
        "CUDA figure ignored off CUDA"
    );
    assert_eq!(amd.estimate_fit(&req, &[]).kind, FitKind::Unsupported);
    let mac = hw(
        r#"{"os":"macos","arch":"aarch64","memory_gb":24,"gpus":[{"name":"Apple M4","unified":true,"backends":["metal"]}]}"#,
    );
    assert_eq!(mac.required_gb(&req), Some(9.0));
    assert_eq!(mac.estimate_fit(&req, &[]).kind, FitKind::Estimated);
    // Older manifests without the new keys keep their meaning.
    let legacy = Requirements {
        vram_gb: Some(6.0),
        ..Default::default()
    };
    assert_eq!(nvidia.required_gb(&legacy), Some(6.0));
    let parsed: Requirements =
        serde_json::from_str(r#"{"memory_gb":4,"cuda_vram_gb":7.5,"mps_memory_gb":9}"#).unwrap();
    assert_eq!(
        (parsed.cuda_vram_gb, parsed.mps_memory_gb),
        (Some(7.5), Some(9.0))
    );
}

#[test]
fn hardware_override_is_lenient_and_app_supplied() {
    let h = hw(r#"{"memory_gb":16,"gpus":[{"name":"ok","vram_gb":4},{"vram_gb":"bad"},7]}"#);
    assert_eq!(h.os, std::env::consts::OS, "missing os takes the host's");
    assert_eq!(h.arch, std::env::consts::ARCH);
    assert_eq!(h.gpus.len(), 1, "unreadable gpu entries dropped");
    assert_eq!(h.source, "override");
    assert_eq!(h.profile, h.profile_id());
    let no_gpus = hw(r#"{"os":"linux","gpus":"nope"}"#);
    assert!(no_gpus.gpus.is_empty() && no_gpus.memory_gb.is_none());
    assert_eq!(no_gpus.profile, "linux-0gb");

    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("hw.json");
    fs::write(
        &path,
        r#"{"os":"macos","memory_gb":24,"gpus":[{"name":"Apple","unified":true}]}"#,
    )
    .unwrap();
    let h = Hardware::probe_with(Some(HardwareOverride::Path(&path))).unwrap();
    assert_eq!(h.profile, "macos-24gb-unified");
    let h = Hardware::probe_with(Some(HardwareOverride::Json(
        r#"{"os":"linux","memory_gb":8}"#,
    )))
    .unwrap();
    assert_eq!(h.profile, "linux-8gb");
    assert!(Hardware::probe_with(Some(HardwareOverride::Json("[1,2]"))).is_err());
    assert!(Hardware::probe_with(Some(HardwareOverride::Json(r#"{"memory_gb":"x"}"#))).is_err());
    assert!(Hardware::probe_with(Some(HardwareOverride::Path(&tmp.path().join("none")))).is_err());
    assert!(Hardware::probe_with(None).unwrap().memory_gb.is_some());
}

#[test]
fn profile_aliases_file_measurements_under_app_profiles() {
    let mut h = hw(
        r#"{"os":"windows","memory_gb":32,"gpus":[{"name":"NVIDIA GeForce RTX 4070 Laptop GPU","vram_gb":8,"backends":["cuda"]}]}"#,
    );
    assert_eq!(h.profile, "windows-gpu-8gb");
    let aliases = vec![
        ProfileAlias {
            alias: "rtx4070-8gb".into(),
            gpu_name_contains: Some("4070".into()),
            primary: true,
            ..Default::default()
        },
        ProfileAlias {
            alias: "lab-box".into(),
            profile: Some("windows-gpu-8gb".into()),
            ..Default::default()
        },
        ProfileAlias {
            alias: "never".into(),
            ..Default::default()
        },
        ProfileAlias {
            alias: "mac-only".into(),
            gpu_name_contains: Some("apple".into()),
            ..Default::default()
        },
    ];
    h.apply_profile_aliases(&aliases);
    h.apply_profile_aliases(&aliases); // idempotent
    assert_eq!(h.profile, "rtx4070-8gb");
    assert_eq!(h.profile_aliases, vec!["windows-gpu-8gb", "lab-box"]);
    assert!(h.answers_to("rtx4070-8gb") && h.answers_to("lab-box"));
    assert!(!h.answers_to("never") && !h.answers_to("mac-only"));

    let m: Measurement =
        serde_json::from_str(r#"{"profile":"rtx4070-8gb","peak_gb":7.8}"#).unwrap();
    let req = Requirements {
        cuda_vram_gb: Some(7.8),
        ..Default::default()
    };
    let fit = h.estimate_fit(&req, &[m]);
    assert_eq!(fit.kind, FitKind::Measured);
    assert_eq!(fit.profile, "rtx4070-8gb");
    let lab: Measurement = serde_json::from_str(r#"{"profile":"lab-box"}"#).unwrap();
    assert_eq!(h.estimate_fit(&req, &[lab]).kind, FitKind::Measured);
    let json = serde_json::to_value(&h).unwrap();
    assert_eq!(json["profile"], "rtx4070-8gb");
}

#[test]
fn models_without_a_local_backend_are_rental_only_or_api() {
    let h = hw(r#"{"os":"macos","memory_gb":24,"gpus":[{"name":"Apple","unified":true}]}"#);
    let rental = Requirements {
        remote_only: Some(RemoteRoute::Rental),
        memory_gb: Some(1.0),
        ..Default::default()
    };
    let fit = h.estimate_fit(&rental, &[]);
    assert_eq!(fit.kind, FitKind::RentalOnly);
    assert!(fit.required_gb.is_none());
    let m: Measurement = serde_json::from_str(r#"{"profile":"macos-24gb"}"#).unwrap();
    assert_eq!(
        h.estimate_fit(&rental, std::slice::from_ref(&m)).kind,
        FitKind::Measured,
        "a recorded run on this profile still wins"
    );
    let api: Requirements = serde_json::from_str(r#"{"remote_only":"api"}"#).unwrap();
    assert_eq!(h.estimate_fit(&api, &[m]).kind, FitKind::Api);
    assert_eq!(
        serde_json::to_value(FitKind::RentalOnly).unwrap(),
        "rental_only"
    );
    assert_eq!(serde_json::to_value(FitKind::Api).unwrap(), "api");
    assert_eq!(serde_json::to_value(ErrorCode::Paused).unwrap(), "paused");
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
