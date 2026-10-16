//! End-to-end tests of the shared suite layout against real files, real file
//! locks, real symlinks and a real local HTTP server (tiny payloads). Every
//! assertion reads bytes back from disk or from the server's request log.

use rightkit_models::suite::catalog::{ModelSpec, RemoteFile, SpecFile, SpecSource};
use rightkit_models::suite::lease;
use rightkit_models::suite::manifest::{self, VerifyMode};
use rightkit_models::suite::migration::{self, JournalPhase, MigrationJournal};
use rightkit_models::suite::refs;
use rightkit_models::suite::store::{self, EnsureOptions, EntryState};
use rightkit_models::suite::{paths, GrantConfig, ModelStore};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Loopback-capable client for the local mock server. Production clients refuse
/// loopback; this test client allows exactly 127.0.0.1.
fn http() -> rightkit_models::http::ModelHttp {
    rightkit_models::http::http_client_with_options(rightkit_models::http::ModelHttpOptions {
        proxy: rightkit_models::http::ProxyMode::None,
        address_policy: rightkit_http::AddressPolicy::public_web()
            .allow_if(vec!["127.0.0.1".to_string()], |ip| ip.is_loopback()),
        ..Default::default()
    })
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

fn leak(s: impl Into<String>) -> &'static str {
    Box::leak(s.into().into_boxed_str())
}

// --------------------------------------------------------------- server

#[derive(Clone, Copy, PartialEq)]
enum GrantMode {
    Normal,
    WrongRoute,
    Expired,
}

struct Config {
    routes: HashMap<String, Vec<u8>>,
    /// Gated routes need `Authorization: Bearer <token it issued>`.
    gated: bool,
    /// The first issued token is rejected (401) as if revoked/expired mid-flight.
    revoke_first_token: bool,
    /// Paths whose first un-ranged response sends half the body then drops.
    truncate_first: Vec<String>,
    grant_mode: GrantMode,
}

impl Config {
    fn new(routes: Vec<(&str, Vec<u8>)>) -> Self {
        Self {
            routes: routes
                .into_iter()
                .map(|(p, b)| (p.to_string(), b))
                .collect(),
            gated: false,
            revoke_first_token: false,
            truncate_first: vec![],
            grant_mode: GrantMode::Normal,
        }
    }
}

struct Server {
    base: String,
    log: Arc<Mutex<Vec<String>>>,
    grants: Arc<AtomicUsize>,
}

impl Server {
    fn requests(&self, prefix: &str) -> Vec<String> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.starts_with(prefix))
            .cloned()
            .collect()
    }
}

fn serve(cfg: Config) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let log = Arc::new(Mutex::new(Vec::new()));
    let grants = Arc::new(AtomicUsize::new(0));
    let cfg = Arc::new(cfg);
    let truncated = Arc::new(Mutex::new(HashSet::<String>::new()));
    let (log2, grants2) = (log.clone(), grants.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (cfg, log, grants, truncated) = (
                cfg.clone(),
                log2.clone(),
                grants2.clone(),
                truncated.clone(),
            );
            std::thread::spawn(move || handle(stream, &cfg, &log, &grants, &truncated));
        }
    });
    Server { base, log, grants }
}

fn handle(
    stream: std::net::TcpStream,
    cfg: &Config,
    log: &Mutex<Vec<String>>,
    grants: &AtomicUsize,
    truncated: &Mutex<HashSet<String>>,
) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let (mut range, mut auth, mut length) = (None::<usize>, String::new(), 0usize);
    loop {
        let mut h = String::new();
        reader.read_line(&mut h).unwrap();
        if h.trim().is_empty() {
            break;
        }
        let lower = h.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("range: bytes=") {
            range = v.trim().trim_end_matches('-').parse().ok();
        } else if let Some(v) = lower.strip_prefix("content-length:") {
            length = v.trim().parse().unwrap_or(0);
        } else if lower.starts_with("authorization:") {
            auth = h["authorization:".len()..].trim().to_string();
        }
    }
    let mut out = stream;
    if method == "POST" && path == "/v1/model-grants" {
        let mut body = vec![0u8; length];
        reader.read_exact(&mut body).unwrap();
        let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let n = grants.fetch_add(1, Ordering::SeqCst) + 1;
        log.lock().unwrap().push(format!(
            "POST grant device={} model={} platform={}",
            req["deviceId"].as_str().unwrap_or(""),
            req["model"].as_str().unwrap_or(""),
            req["platform"].as_str().unwrap_or("")
        ));
        let route = if cfg.grant_mode == GrantMode::WrongRoute {
            "/wrong".into()
        } else {
            req["route"].as_str().unwrap().to_string()
        };
        let expires: u64 = if cfg.grant_mode == GrantMode::Expired {
            1
        } else {
            4_000_000_000
        };
        let json = format!(
            r#"{{"token":"tok{n}","expiresAt":{expires},"route":"{route}","sha256":"{}"}}"#,
            req["sha256"].as_str().unwrap()
        );
        let _ = write!(
            out,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
            json.len()
        );
        return;
    }
    log.lock()
        .unwrap()
        .push(format!("{method} {path} range={range:?} auth={auth}"));
    if cfg.gated {
        let revoked = cfg.revoke_first_token && auth == "Bearer tok1";
        if !auth.starts_with("Bearer tok") || revoked {
            let _ = out.write_all(
                b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            return;
        }
    }
    let Some(body) = cfg.routes.get(&path) else {
        let _ = out
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    };
    let etag = format!("\"{}\"", &sha(body)[..16]);
    let total = body.len();
    match range {
        Some(start) if start < total => {
            let _ = write!(
                out,
                "HTTP/1.1 206 Partial Content\r\nETag: {etag}\r\nContent-Range: bytes {start}-{}/{total}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                total - 1,
                total - start
            );
            let _ = out.write_all(&body[start..]);
        }
        _ => {
            let first = cfg.truncate_first.contains(&path)
                && truncated.lock().unwrap().insert(path.clone());
            let _ = write!(out, "HTTP/1.1 200 OK\r\nETag: {etag}\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n");
            if first {
                let _ = out.write_all(&body[..total / 2]);
            } else {
                let _ = out.write_all(body);
            }
        }
    }
}

// ------------------------------------------------------------- fixtures

fn tar_gz(wrapper: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    for (name, data) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        builder
            .append_data(&mut header, format!("{wrapper}/{name}"), *data)
            .unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

fn spec_files(files: &[(&str, &[u8])]) -> &'static [SpecFile] {
    Box::leak(
        files
            .iter()
            .map(|(p, b)| SpecFile {
                path: leak(*p),
                sha256: Some(leak(sha(b))),
                size: Some(b.len() as u64),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    )
}

struct Layout {
    _tmp: tempfile::TempDir,
    suite: PathBuf,
    models: PathBuf,
}

fn layout() -> Layout {
    let tmp = tempfile::tempdir().unwrap();
    let suite = tmp.path().join("shared");
    let models = paths::models_root_at(&suite);
    fs::create_dir_all(&models).unwrap();
    Layout {
        _tmp: tmp,
        suite,
        models,
    }
}

const WEIGHTS: &[u8] = b"weights-bytes";
const VOCAB: &[u8] = b"vocab-bytes";

fn archive_spec(
    id: &'static str,
    url: Option<&'static str>,
    archive: &[u8],
    auth: bool,
) -> ModelSpec {
    ModelSpec {
        tier: rightkit_models::suite::catalog::ModelTier::Base,
        entitlement: false,
        id,
        label: "Archive model",
        version: leak(sha(archive)),
        size_hint_bytes: (WEIGHTS.len() + VOCAB.len()) as u64,
        files: spec_files(&[("weights.bin", WEIGHTS), ("sub/vocab.txt", VOCAB)]),
        source: SpecSource::Archive {
            url,
            sha256: Some(leak(sha(archive))),
            size: None,
            requires_auth: auth,
        },
        legacy_dirs: &["asr/archive-model"],
        retired_legacy_paths: &[],
        retired_store_ids: &[],
    }
}

fn opts(app: &'static str) -> EnsureOptions<'static> {
    EnsureOptions {
        seed_dir: None,
        authorization: None,
        app_id: app,
    }
}

fn read(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap()
}

// ===================================================================
// Download grants + refresh + trusted archive install
// ===================================================================

#[test]
fn gated_archive_install_uses_grant_writes_manifest_and_registers_ref() {
    let lay = layout();
    let archive = tar_gz(
        "wrap",
        &[("weights.bin", WEIGHTS), ("sub/vocab.txt", VOCAB)],
    );
    let mut cfg = Config::new(vec![("/m/model.tar.gz", archive.clone())]);
    cfg.gated = true;
    let server = serve(cfg);
    let url = leak(format!("{}/m/model.tar.gz", server.base));
    let spec = archive_spec("arch-model", Some(url), &archive, true);

    let grant = GrantConfig {
        worker_base: server.base.clone(),
        model: "demo".into(),
        platform: "macos".into(),
        device_id: "device-0123456789abcdef".into(),
    };
    let store = ModelStore::at_models_root(&lay.models, "heardright").with_http_client(http());
    let mut events = Vec::new();
    let status = store
        .ensure(&spec, Some(&grant), &|| false, |p| {
            events.push(format!("{p:?}"))
        })
        .unwrap();

    assert_eq!(status.state, EntryState::Ready);
    let dir = lay.models.join("arch-model");
    assert_eq!(read(&dir.join("weights.bin")), WEIGHTS);
    assert_eq!(read(&dir.join("sub/vocab.txt")), VOCAB);
    // Shared on-disk contract: manifest.json carries the TRUSTED pins.
    let m = manifest::read_manifest(&dir).unwrap().unwrap();
    assert_eq!(m.id, "arch-model");
    assert_eq!(m.version, spec.version);
    assert_eq!(
        m.files
            .iter()
            .find(|f| f.path == "weights.bin")
            .unwrap()
            .sha256
            .as_deref(),
        Some(sha(WEIGHTS).as_str())
    );
    // refs/<app>.json written in the contract's JSON shape.
    let ref_json: serde_json::Value =
        serde_json::from_slice(&read(&lay.models.join("refs/heardright.json"))).unwrap();
    assert_eq!(ref_json["app"], "heardright");
    assert_eq!(ref_json["models"], serde_json::json!(["arch-model"]));
    // The grant request carried the device/model/platform; the bytes request carried the bearer.
    assert_eq!(
        server.requests("POST grant"),
        vec!["POST grant device=device-0123456789abcdef model=demo platform=macos"]
    );
    assert!(server.requests("GET /m/model.tar.gz")[0].ends_with("auth=Bearer tok1"));
    assert!(
        events.iter().any(|e| e.starts_with("Download")),
        "{events:?}"
    );
    // Scratch is gone.
    assert!(!lay.models.join(".incoming").exists());

    // Second ensure is verify-only: no new network traffic at all.
    let before = server.log.lock().unwrap().len();
    store
        .ensure(&spec, Some(&grant), &|| false, |_| {})
        .unwrap();
    assert_eq!(server.log.lock().unwrap().len(), before);
}

#[test]
fn rejected_grant_is_refreshed_once_and_download_proceeds() {
    let lay = layout();
    let archive = tar_gz(
        "wrap",
        &[("weights.bin", WEIGHTS), ("sub/vocab.txt", VOCAB)],
    );
    let mut cfg = Config::new(vec![("/m/r.tar.gz", archive.clone())]);
    cfg.gated = true;
    cfg.revoke_first_token = true;
    let server = serve(cfg);
    let url = leak(format!("{}/m/r.tar.gz", server.base));
    let spec = archive_spec("refresh-model", Some(url), &archive, true);
    let grant = GrantConfig {
        worker_base: server.base.clone(),
        model: "demo".into(),
        platform: "windows".into(),
        device_id: "device-0123456789abcdef".into(),
    };

    let store = ModelStore::at_models_root(&lay.models, "cutright").with_http_client(http());
    store
        .ensure(&spec, Some(&grant), &|| false, |_| {})
        .unwrap();

    assert_eq!(
        server.grants.load(Ordering::SeqCst),
        2,
        "one initial grant + one refresh"
    );
    let gets = server.requests("GET /m/r.tar.gz");
    assert!(gets[0].ends_with("auth=Bearer tok1"), "{gets:?}");
    assert!(
        gets.last().unwrap().ends_with("auth=Bearer tok2"),
        "{gets:?}"
    );
    assert_eq!(read(&lay.models.join("refresh-model/weights.bin")), WEIGHTS);
}

#[test]
fn grant_scope_mismatch_and_expired_grants_never_download() {
    for (mode, needle) in [
        (GrantMode::WrongRoute, "scope"),
        (GrantMode::Expired, "incomplete"),
    ] {
        let lay = layout();
        let archive = tar_gz(
            "wrap",
            &[("weights.bin", WEIGHTS), ("sub/vocab.txt", VOCAB)],
        );
        let mut cfg = Config::new(vec![("/m/x.tar.gz", archive.clone())]);
        cfg.gated = true;
        cfg.grant_mode = mode;
        let server = serve(cfg);
        let url = leak(format!("{}/m/x.tar.gz", server.base));
        let spec = archive_spec("scope-model", Some(url), &archive, true);
        let grant = GrantConfig {
            worker_base: server.base.clone(),
            model: "demo".into(),
            platform: "macos".into(),
            device_id: "device-0123456789abcdef".into(),
        };
        let err = ModelStore::at_models_root(&lay.models, "a")
            .with_http_client(http())
            .ensure(&spec, Some(&grant), &|| false, |_| {})
            .unwrap_err();
        assert!(err.contains(needle), "{err}");
        assert!(
            server.requests("GET").is_empty(),
            "no archive request without a valid grant"
        );
        assert!(!lay.models.join("scope-model").exists());
    }
}

#[test]
fn gated_download_without_device_id_or_token_fails_before_network() {
    let lay = layout();
    let archive = tar_gz(
        "wrap",
        &[("weights.bin", WEIGHTS), ("sub/vocab.txt", VOCAB)],
    );
    let server = serve(Config::new(vec![("/m/g.tar.gz", archive.clone())]));
    let spec = archive_spec(
        "gated-model",
        Some(leak(format!("{}/m/g.tar.gz", server.base))),
        &archive,
        true,
    );
    let err = ModelStore::at_models_root(&lay.models, "a")
        .with_http_client(http())
        .ensure(&spec, None, &|| false, |_| {})
        .unwrap_err();
    assert!(err.contains("device id"), "{err}");
    let err = store::ensure_ready(&http(), &lay.models, &spec, &opts("a"), |_| {}).unwrap_err();
    assert!(err.contains("download token"), "{err}");
    assert!(server.requests("GET").is_empty());
}

#[test]
fn corrupted_archive_fails_integrity_and_leaves_no_entry() {
    let lay = layout();
    let good = tar_gz(
        "wrap",
        &[("weights.bin", WEIGHTS), ("sub/vocab.txt", VOCAB)],
    );
    let evil = tar_gz(
        "wrap",
        &[("weights.bin", b"EVIL"), ("sub/vocab.txt", VOCAB)],
    );
    let server = serve(Config::new(vec![("/m/e.tar.gz", evil)]));
    let spec = archive_spec(
        "evil-model",
        Some(leak(format!("{}/m/e.tar.gz", server.base))),
        &good,
        false,
    );
    let err = store::ensure_ready(&http(), &lay.models, &spec, &opts("a"), |_| {}).unwrap_err();
    assert!(err.contains("integrity check"), "{err}");
    assert!(!lay.models.join("evil-model").exists());
    assert!(
        !lay.models.join("refs/a.json").exists(),
        "failed install must not register a ref"
    );
}

#[test]
fn interrupted_download_resumes_with_range_and_verifies() {
    let lay = layout();
    let archive = tar_gz(
        "wrap",
        &[("weights.bin", &[7u8; 4096]), ("sub/vocab.txt", VOCAB)],
    );
    let mut cfg = Config::new(vec![("/m/t.tar.gz", archive.clone())]);
    cfg.truncate_first = vec!["/m/t.tar.gz".into()];
    let server = serve(cfg);
    let mut spec = archive_spec(
        "resume-model",
        Some(leak(format!("{}/m/t.tar.gz", server.base))),
        &archive,
        false,
    );
    spec.files = spec_files(&[("weights.bin", &[7u8; 4096]), ("sub/vocab.txt", VOCAB)]);
    store::ensure_ready(&http(), &lay.models, &spec, &opts("a"), |_| {}).unwrap();
    let gets = server.requests("GET /m/t.tar.gz");
    assert!(
        gets.len() >= 2 && gets[0].contains("range=None") && gets[1].contains("range=Some("),
        "{gets:?}"
    );
    assert_eq!(
        read(&lay.models.join("resume-model/weights.bin")),
        vec![7u8; 4096]
    );
}

#[test]
fn cancelled_download_keeps_part_file_and_next_call_resumes() {
    let lay = layout();
    let big = vec![9u8; 300_000];
    let archive = tar_gz("wrap", &[("weights.bin", &big), ("sub/vocab.txt", VOCAB)]);
    let server = serve(Config::new(vec![("/m/c.tar.gz", archive.clone())]));
    let mut spec = archive_spec(
        "cancel-model",
        Some(leak(format!("{}/m/c.tar.gz", server.base))),
        &archive,
        false,
    );
    spec.files = spec_files(&[("weights.bin", &big), ("sub/vocab.txt", VOCAB)]);
    let err =
        store::ensure_ready_cancellable(&http(), &lay.models, &spec, &opts("a"), &|| true, |_| {})
            .unwrap_err();
    assert!(err.contains("cancelled"), "{err}");
    assert!(!lay.models.join("cancel-model").exists());
    store::ensure_ready(&http(), &lay.models, &spec, &opts("a"), |_| {}).unwrap();
    assert_eq!(read(&lay.models.join("cancel-model/weights.bin")), big);
}

// ===================================================================
// Files source: repair only what drifted; bundled seeds are pin-checked
// ===================================================================

fn files_spec(base: &str, seed_pin: &[u8]) -> ModelSpec {
    let remote: &'static [RemoteFile] = Box::leak(
        vec![
            RemoteFile {
                path: "weights.bin",
                url: Some(leak(format!("{base}/f/weights.bin"))),
                sha256: Some(leak(sha(WEIGHTS))),
                size: Some(WEIGHTS.len() as u64),
            },
            RemoteFile {
                path: "vocab.txt",
                url: Some(leak(format!("{base}/f/vocab.txt"))),
                sha256: Some(leak(sha(VOCAB))),
                size: Some(VOCAB.len() as u64),
            },
            RemoteFile {
                path: "config.json",
                url: None,
                sha256: Some(leak(sha(seed_pin))),
                size: Some(seed_pin.len() as u64),
            },
        ]
        .into_boxed_slice(),
    );
    ModelSpec {
        tier: rightkit_models::suite::catalog::ModelTier::Base,
        entitlement: false,
        id: "files-model",
        label: "Files model",
        version: "files-v1",
        size_hint_bytes: 0,
        files: spec_files(&[
            ("weights.bin", WEIGHTS),
            ("vocab.txt", VOCAB),
            ("config.json", seed_pin),
        ]),
        source: SpecSource::Files(remote),
        legacy_dirs: &[],
        retired_legacy_paths: &[],
        retired_store_ids: &[],
    }
}

#[test]
fn files_source_downloads_seeds_and_repairs_only_drifted_files() {
    let lay = layout();
    let server = serve(Config::new(vec![
        ("/f/weights.bin", WEIGHTS.to_vec()),
        ("/f/vocab.txt", VOCAB.to_vec()),
    ]));
    let cfg_bytes: &[u8] = br#"{"k":1}"#;
    let spec = files_spec(&server.base, cfg_bytes);
    let seeds = tempfile::tempdir().unwrap();
    fs::write(seeds.path().join("config.json"), cfg_bytes).unwrap();
    let o = EnsureOptions {
        seed_dir: Some(seeds.path()),
        authorization: None,
        app_id: "scraperight",
    };

    store::ensure_ready(&http(), &lay.models, &spec, &o, |_| {}).unwrap();
    let dir = lay.models.join("files-model");
    assert_eq!(read(&dir.join("config.json")), cfg_bytes);
    assert_eq!(server.requests("GET").len(), 2);

    // Same-size corruption of one file: Size passes, Full catches; repair refetches only it.
    fs::write(dir.join("vocab.txt"), vec![b'X'; VOCAB.len()]).unwrap();
    assert_eq!(
        store::status_at(&lay.models, &spec, VerifyMode::Size).state,
        EntryState::Ready
    );
    let full = store::status_at(&lay.models, &spec, VerifyMode::Full);
    assert_eq!(full.state, EntryState::Partial);
    assert_eq!(full.needed, vec!["vocab.txt"]);
    store::ensure_ready(&http(), &lay.models, &spec, &o, |_| {}).unwrap();
    assert_eq!(read(&dir.join("vocab.txt")), VOCAB);
    let gets = server.requests("GET");
    assert_eq!(gets.len(), 3, "{gets:?}");
    assert!(gets[2].starts_with("GET /f/vocab.txt"));
}

#[test]
fn stale_bundled_seed_fails_its_pin_and_is_not_installed() {
    let lay = layout();
    let server = serve(Config::new(vec![
        ("/f/weights.bin", WEIGHTS.to_vec()),
        ("/f/vocab.txt", VOCAB.to_vec()),
    ]));
    let spec = files_spec(&server.base, br#"{"k":1}"#);
    let seeds = tempfile::tempdir().unwrap();
    fs::write(seeds.path().join("config.json"), br#"{"k":2}"#).unwrap();
    let o = EnsureOptions {
        seed_dir: Some(seeds.path()),
        authorization: None,
        app_id: "a",
    };
    let err = store::ensure_ready(&http(), &lay.models, &spec, &o, |_| {}).unwrap_err();
    assert!(err.contains("failed integrity check"), "{err}");
    assert!(!lay.models.join("files-model/config.json").exists());
    assert!(!lay.models.join("refs/a.json").exists());
}

// ===================================================================
// Trusted catalog identity
// ===================================================================

#[test]
fn self_described_manifest_never_defines_identity_but_matching_bytes_are_adopted() {
    let lay = layout();
    let spec = ModelSpec {
        tier: rightkit_models::suite::catalog::ModelTier::Base,
        entitlement: false,
        id: "trust-model",
        label: "t",
        version: "pinned-rev",
        size_hint_bytes: 0,
        files: spec_files(&[("weights.bin", WEIGHTS)]),
        source: SpecSource::Files(&[]),
        legacy_dirs: &[],
        retired_legacy_paths: &[],
        retired_store_ids: &[],
    };
    let dir = lay.models.join("trust-model");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("weights.bin"), WEIGHTS).unwrap();

    // No manifest at all: hand-placed copy is not trusted.
    assert_eq!(
        store::status_at(&lay.models, &spec, VerifyMode::Full).state,
        EntryState::Partial
    );
    // A manifest claiming another revision: not trusted either.
    let mut forged = spec.expected_manifest();
    forged.version = "attacker-rev".into();
    manifest::write_manifest(&dir, &forged).unwrap();
    assert_eq!(
        store::status_at(&lay.models, &spec, VerifyMode::Full).state,
        EntryState::Partial
    );
    // A manifest that weakens the pins (no hash): not trusted.
    let mut weak = spec.expected_manifest();
    weak.files[0].sha256 = None;
    manifest::write_manifest(&dir, &weak).unwrap();
    assert_eq!(
        store::status_at(&lay.models, &spec, VerifyMode::Full).state,
        EntryState::Partial
    );

    // ensure with no download endpoint adopts bytes that verify against the catalog pins.
    let status = store::ensure_ready(&http(), &lay.models, &spec, &opts("a"), |_| {}).unwrap();
    assert_eq!(status.state, EntryState::Ready);
    assert_eq!(
        manifest::read_manifest(&dir).unwrap().unwrap().version,
        "pinned-rev"
    );

    // Different bytes + no endpoint: honest "unavailable", and the bad file is untouched.
    fs::write(dir.join("weights.bin"), b"weights-byteZ").unwrap();
    let err = store::ensure_ready(&http(), &lay.models, &spec, &opts("a"), |_| {}).unwrap_err();
    assert!(err.contains("no download endpoint"), "{err}");
}

#[test]
fn builtin_catalog_pins_every_file_and_variant_descriptor_matches() {
    use rightkit_models::suite::catalog::{all_models, parakeet_spec};
    for spec in all_models() {
        assert!(!spec.files.is_empty());
        for f in spec.files {
            assert!(
                f.size.is_some_and(|s| s > 0),
                "{}:{} unpinned size",
                spec.id,
                f.path
            );
            let h = f
                .sha256
                .unwrap_or_else(|| panic!("{}:{} unpinned hash", spec.id, f.path));
            assert!(
                h.len() == 64
                    && h.bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            );
        }
        assert!(manifest::safe_rel_path(spec.files[0].path).is_some());
    }
    match rightkit_models::suite::parakeet_variant_descriptor() {
        Some(d) => {
            let spec = parakeet_spec();
            assert_eq!(d.id, spec.id);
            assert_eq!(d.revision, spec.version);
            assert!(spec.can_download() && spec.requires_auth());
        }
        None => assert!(!parakeet_spec().can_download()),
    }
    // Traversal in a manifest path is rejected by verification.
    assert!(
        manifest::safe_rel_path("../escape").is_none() && manifest::safe_rel_path("a\\b").is_none()
    );
}

// ===================================================================
// Refs registry, GC protection, runtime-family leases
// ===================================================================

fn installed_entry(lay: &Layout, id: &str) -> PathBuf {
    let dir = lay.models.join(id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("weights.bin"), id.as_bytes()).unwrap();
    dir
}

#[test]
fn refs_are_per_app_gc_removes_only_unreferenced_unlocked_and_unprotected() {
    let lay = layout();
    for id in ["shared-a", "only-hr", "orphan", "kept", "locked"] {
        installed_entry(&lay, id);
    }
    store::register_ref(&lay.models, "scraperight", "shared-a").unwrap();
    store::register_ref(&lay.models, "heardright", "shared-a").unwrap();
    store::register_ref(&lay.models, "heardright", "only-hr").unwrap();

    let held = rightkit_models::lock::try_lock(&lay.models.join("locked"), "installer")
        .unwrap()
        .unwrap();
    let removed = store::prune_unref_entries(&lay.models, &["kept"]).unwrap();
    let removed: BTreeSet<_> = removed.into_iter().collect();
    assert_eq!(removed, BTreeSet::from(["orphan".to_string()]));
    assert!(lay.models.join("kept").exists() && lay.models.join("locked").exists());
    drop(held);

    // Unregistering one app frees only models nobody else uses.
    let freed = refs::unregister(&lay.models.join("refs"), "heardright").unwrap();
    assert_eq!(freed, vec!["only-hr".to_string()]);
    let removed = store::prune_unref_entries(&lay.models, &["kept"]).unwrap();
    assert_eq!(
        removed.into_iter().collect::<BTreeSet<_>>(),
        BTreeSet::from(["locked".to_string(), "only-hr".to_string()])
    );
    assert!(
        lay.models.join("shared-a/weights.bin").exists(),
        "still referenced by scraperight"
    );
}

#[test]
fn corrupt_ref_file_blocks_deletion_and_unregister() {
    let lay = layout();
    installed_entry(&lay, "orphan");
    store::register_ref(&lay.models, "good", "something").unwrap();
    fs::write(lay.models.join("refs/bad.json"), b"{not json").unwrap();
    assert!(store::prune_unref_entries(&lay.models, &[]).is_err());
    assert!(lay.models.join("orphan").exists());
    assert!(refs::unregister(&lay.models.join("refs"), "good").is_err());
    assert!(
        lay.models.join("refs/good.json").exists(),
        "failed unregister must not change registry"
    );
    // A ref naming a path-escaping model id is rejected, not trusted.
    fs::write(
        lay.models.join("refs/bad.json"),
        br#"{"app":"bad","models":["../x"]}"#,
    )
    .unwrap();
    assert!(refs::referenced_ids_checked(&lay.models.join("refs")).is_err());
}

#[test]
fn concurrent_ref_writers_lose_no_registration() {
    let lay = layout();
    let refs_dir = lay.models.join("refs");
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let dir = refs_dir.clone();
            std::thread::spawn(move || {
                for j in 0..5 {
                    refs::register(&dir, "app", &format!("m{i}-{j}")).unwrap();
                }
            })
        })
        .collect();
    handles.into_iter().for_each(|h| h.join().unwrap());
    let ids = refs::referenced_ids_checked(&refs_dir).unwrap();
    assert_eq!(ids.len(), 40);
}

#[test]
fn runtime_lease_is_exclusive_defers_gc_and_signals_yield_request() {
    let lay = layout();
    installed_entry(&lay, "orphan");
    let family = "family-x";
    let held = lease::try_acquire_at_root(&lay.suite, family, "heardright")
        .unwrap()
        .expect("free");
    assert_eq!(
        lease::current_holder_at_root(&lay.suite, family)
            .unwrap()
            .app,
        "heardright"
    );
    assert!(
        lease::try_acquire_at_root(&lay.suite, family, "scraperight")
            .unwrap()
            .is_none()
    );

    // GC is conservative while ANY runtime family (even unknown) is held.
    assert!(store::prune_unref_entries(&lay.models, &[])
        .unwrap()
        .is_empty());
    assert!(lay.models.join("orphan").exists());

    // A waiter posts a yield request beside the lock so an idle holder can unload.
    let lock_file = lay.suite.join("runtime/model-use/family-x.lock");
    let yield_file = lease::yield_request_path(&lock_file);
    let suite = lay.suite.clone();
    let waiter = std::thread::spawn(move || {
        lease::acquire_at_root(&suite, family, "cutright", &|_| {}, &|| false)
            .map(drop)
            .unwrap();
    });
    let mut seen = None;
    for _ in 0..100 {
        if let Ok(body) = fs::read_to_string(&yield_file) {
            seen = Some(body);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let body: serde_json::Value =
        serde_json::from_str(&seen.expect("yield request posted")).unwrap();
    assert_eq!(body["app"], "cutright");
    drop(held); // the holder yields
    waiter.join().unwrap();
    assert!(
        !yield_file.exists(),
        "request removed once the waiter acquired"
    );

    // Released: GC proceeds.
    assert_eq!(
        store::prune_unref_entries(&lay.models, &[]).unwrap(),
        vec!["orphan".to_string()]
    );
}

#[test]
fn leases_across_roots_are_all_or_nothing() {
    let a = layout();
    let b = layout();
    let blocker = lease::try_acquire_at_root(&b.suite, "fam", "x")
        .unwrap()
        .unwrap();
    assert!(
        lease::try_acquire_at_roots(&[&a.suite, &b.suite], "fam", "y")
            .unwrap()
            .is_none()
    );
    assert!(
        lease::try_acquire_at_root(&a.suite, "fam", "z")
            .unwrap()
            .is_some(),
        "partial hold released"
    );
    drop(blocker);
    assert!(
        lease::try_acquire_at_roots(&[&a.suite, &b.suite], "fam", "y")
            .unwrap()
            .is_some()
    );
}

// ===================================================================
// Migration journal: move, alias, rollback, crash recovery, ref migration
// ===================================================================

fn mig_spec() -> ModelSpec {
    ModelSpec {
        tier: rightkit_models::suite::catalog::ModelTier::Base,
        entitlement: false,
        id: "current",
        label: "migrating",
        version: "v1",
        size_hint_bytes: 0,
        files: spec_files(&[("weights.bin", b"data"), ("vocab.txt", b"test")]),
        source: SpecSource::Files(&[]),
        legacy_dirs: &["asr/current"],
        retired_legacy_paths: &[],
        retired_store_ids: &[],
    }
}

struct Mig {
    _tmp: tempfile::TempDir,
    old_models: PathBuf,
    new_models: PathBuf,
    source: PathBuf,
    destination: PathBuf,
}

fn mig() -> Mig {
    let tmp = tempfile::tempdir().unwrap();
    let old_models = tmp.path().join("old/models");
    let new_models = tmp.path().join("new/models");
    let source = old_models.join("asr/current");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("weights.bin"), b"data").unwrap();
    fs::write(source.join("vocab.txt"), b"test").unwrap();
    let destination = new_models.join("current");
    Mig {
        _tmp: tmp,
        old_models,
        new_models,
        source,
        destination,
    }
}

fn is_link(p: &Path) -> bool {
    fs::symlink_metadata(p)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

#[test]
fn migration_moves_verified_copy_keeps_old_path_working_and_archives_journal() {
    let m = mig();
    let loaded = std::cell::RefCell::new(Vec::<PathBuf>::new());
    let out = store::migrate_legacy_checked(
        &m.new_models,
        &mig_spec(),
        &m.old_models,
        "heardright",
        &|d| {
            loaded.borrow_mut().push(d.to_path_buf());
            Ok(())
        },
    );
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_eq!(out.migrated.len(), 1);
    // Native load check ran against the PUBLISHED destination path.
    assert_eq!(
        loaded.borrow().as_slice(),
        std::slice::from_ref(&m.destination)
    );
    assert_eq!(
        store::status_at(&m.new_models, &mig_spec(), VerifyMode::Full).state,
        EntryState::Ready
    );
    // Old path is an alias to the new payload, still readable by un-migrated app versions.
    assert!(is_link(&m.source));
    assert_eq!(read(&m.source.join("weights.bin")), b"data");
    assert_eq!(
        m.source.canonicalize().unwrap(),
        m.destination.canonicalize().unwrap()
    );
    // Ref registered; journal archived under .migration/completed, none active.
    assert!(m.new_models.join("refs/heardright.json").is_file());
    assert!(!migration::journal_path(&m.new_models, "current").exists());
    let completed: Vec<_> = fs::read_dir(m.new_models.join(".migration/completed"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(completed.len(), 1);
    let j: MigrationJournal = serde_json::from_slice(&read(&completed[0].path())).unwrap();
    assert_eq!(j.phase, JournalPhase::Complete);

    // Running again is idempotent (alias resolves to destination; ref re-registered).
    let again = store::migrate_legacy_checked(
        &m.new_models,
        &mig_spec(),
        &m.old_models,
        "heardright",
        &|_| Ok(()),
    );
    assert!(again.errors.is_empty(), "{:?}", again.errors);
}

#[test]
fn failed_native_load_restores_source_and_removes_destination() {
    let m = mig();
    let out = store::migrate_legacy_checked(
        &m.new_models,
        &mig_spec(),
        &m.old_models,
        "heardright",
        &|_| Err("cannot load".into()),
    );
    assert!(
        out.errors.iter().any(|e| e.contains("load check failed")),
        "{:?}",
        out.errors
    );
    assert!(!is_link(&m.source) && read(&m.source.join("weights.bin")) == b"data");
    assert!(store::resolve_ready_dir(&m.new_models, &mig_spec()).is_none());
    assert!(!m.new_models.join("refs/heardright.json").exists());
    assert!(
        !migration::journal_path(&m.new_models, "current").exists(),
        "rollback archived its journal"
    );
}

#[test]
fn unverified_or_wrong_variant_source_is_left_untouched() {
    let m = mig();
    fs::write(m.source.join("weights.bin"), b"DATA").unwrap(); // same size, wrong bytes
    let out =
        store::migrate_legacy_checked(&m.new_models, &mig_spec(), &m.old_models, "a", &|_| {
            panic!("must not load")
        });
    assert!(out.migrated.is_empty() && out.errors.is_empty());
    assert_eq!(out.skipped.len(), 1);
    assert_eq!(read(&m.source.join("weights.bin")), b"DATA");
    assert!(!m.destination.exists());
}

#[test]
fn migration_defers_while_runtime_lease_is_held_by_another_app() {
    let m = mig();
    let suite = m.new_models.parent().unwrap().to_path_buf();
    let held = lease::try_acquire_at_root(&suite, "parakeet-tdt-v3", "scraperight")
        .unwrap()
        .unwrap();
    let out = store::migrate_legacy_checked(
        &m.new_models,
        &mig_spec(),
        &m.old_models,
        "heardright",
        &|_| panic!("deferred"),
    );
    assert!(out.deferred && out.migrated.is_empty());
    assert!(!is_link(&m.source) && !m.destination.exists());
    drop(held);
    let out = store::migrate_legacy_checked(
        &m.new_models,
        &mig_spec(),
        &m.old_models,
        "heardright",
        &|_| Ok(()),
    );
    assert_eq!(out.migrated.len(), 1, "{:?}", out.errors);
}

#[test]
fn crash_recovery_restarts_early_phases_and_rolls_back_failed_late_phases() {
    fn journal(m: &Mig, phase: JournalPhase) -> MigrationJournal {
        MigrationJournal {
            version: migration::JOURNAL_VERSION,
            model_id: "current".into(),
            source: m.source.clone(),
            destination: m.destination.clone(),
            staging: Some(m.new_models.join(".current.migration-stage")),
            aside: Some(m.old_models.join("asr/.current.migration-aside")),
            backup: None,
            destination_created: false,
            source_moved: false,
            phase,
        }
    }
    // Planned / SourceVerified / Staged: abandoned staging is discarded, migration completes.
    for phase in [
        JournalPhase::Planned,
        JournalPhase::SourceVerified,
        JournalPhase::Staged,
    ] {
        let m = mig();
        let stage = m.new_models.join(".current.migration-stage");
        fs::create_dir_all(&stage).unwrap();
        fs::write(stage.join("partial.bin"), b"part").unwrap();
        migration::write_journal(
            &migration::journal_path(&m.new_models, "current"),
            &journal(&m, phase),
        )
        .unwrap();
        let out =
            store::migrate_source_checked(&m.new_models, &mig_spec(), &m.source, "a", &|_| Ok(()));
        assert!(out.errors.is_empty(), "{phase:?}: {:?}", out.errors);
        assert!(
            is_link(&m.source) && m.destination.join("weights.bin").exists(),
            "{phase:?}"
        );
        assert!(!stage.exists(), "{phase:?}: stale staging removed");
        assert!(!migration::journal_path(&m.new_models, "current").exists());
    }
    // DestinationVerified with a destination that no longer loads: source restored, journal archived.
    let m = mig();
    fs::create_dir_all(&m.destination).unwrap();
    fs::write(m.destination.join("weights.bin"), b"data").unwrap();
    fs::write(m.destination.join("vocab.txt"), b"test").unwrap();
    manifest::write_manifest(&m.destination, &mig_spec().expected_manifest()).unwrap();
    let mut j = journal(&m, JournalPhase::DestinationVerified);
    j.destination_created = true;
    migration::write_journal(&migration::journal_path(&m.new_models, "current"), &j).unwrap();
    let out = store::migrate_source_checked(&m.new_models, &mig_spec(), &m.source, "a", &|_| {
        Err("gpu says no".into())
    });
    assert!(
        out.errors.iter().any(|e| e.contains("load check failed")),
        "{:?}",
        out.errors
    );
    assert_eq!(read(&m.source.join("weights.bin")), b"data");
    assert!(!is_link(&m.source));
    assert!(!migration::journal_path(&m.new_models, "current").exists());

    // A journal whose identity does not match is refused rather than followed.
    let m = mig();
    let mut bad = journal(&m, JournalPhase::Planned);
    bad.model_id = "someone-else".into();
    migration::write_journal(&migration::journal_path(&m.new_models, "current"), &bad).unwrap();
    let out =
        store::migrate_source_checked(&m.new_models, &mig_spec(), &m.source, "a", &|_| Ok(()));
    assert!(!out.errors.is_empty());
    assert!(!is_link(&m.source) && read(&m.source.join("weights.bin")) == b"data");
}

#[test]
fn distinct_extra_files_are_preserved_through_migration() {
    let m = mig();
    fs::write(m.source.join("user-notes.txt"), b"mine").unwrap();
    let out =
        store::migrate_legacy_checked(&m.new_models, &mig_spec(), &m.old_models, "a", &|_| Ok(()));
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_eq!(read(&m.destination.join("user-notes.txt")), b"mine");
    assert_eq!(
        read(&m.source.join("user-notes.txt")),
        b"mine",
        "alias still shows the extra file"
    );
}

#[test]
fn retired_legacy_dirs_are_removed_only_when_byte_identical() {
    let m = mig();
    let mut spec = mig_spec();
    spec.retired_legacy_paths = &["asr/retired-same", "asr/retired-diff"];
    for (name, extra) in [("retired-same", false), ("retired-diff", true)] {
        let d = m.old_models.join("asr").join(name);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("weights.bin"), b"data").unwrap();
        fs::write(d.join("vocab.txt"), b"test").unwrap();
        if extra {
            fs::write(d.join("extra"), b"x").unwrap();
        }
    }
    let out = store::migrate_legacy_checked(&m.new_models, &spec, &m.old_models, "a", &|_| Ok(()));
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    // Identical retired copy folded into the store (alias); distinct one preserved verbatim.
    assert!(is_link(&m.old_models.join("asr/retired-same")));
    let diff = m.old_models.join("asr/retired-diff");
    assert!(!is_link(&diff) && diff.join("extra").exists());
}

#[test]
fn root_ref_migration_merges_notes_maps_ids_and_unregister_spans_both_roots() {
    let m = mig();
    let (old_refs, new_refs) = (m.old_models.join("refs"), m.new_models.join("refs"));
    fs::create_dir_all(&old_refs).unwrap();
    fs::write(old_refs.join("heardright.json"), br#"{"app":"heardright","models":["parakeet-tdt-v3","unknown-id"],"note":"first used 0.1.1"}"#).unwrap();
    refs::register(&new_refs, "heardright", "already-here").unwrap();
    refs::register(
        &new_refs,
        "scraperight",
        "parakeet-tdt-v3-coreml-compiled-x",
    )
    .unwrap();

    refs::migrate_root_refs_with_map(
        &m.old_models,
        &m.new_models,
        &[("parakeet-tdt-v3", "parakeet-tdt-v3-coreml-compiled-x")],
    )
    .unwrap();
    let merged: serde_json::Value =
        serde_json::from_slice(&read(&new_refs.join("heardright.json"))).unwrap();
    let models: BTreeSet<String> = merged["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        models,
        [
            "already-here",
            "parakeet-tdt-v3",
            "parakeet-tdt-v3-coreml-compiled-x",
            "unknown-id"
        ]
        .map(String::from)
        .into_iter()
        .collect()
    );
    assert_eq!(merged["note"], "first used 0.1.1");

    // Uninstall removes the app from BOTH registries; ids still used elsewhere stay protected.
    let freed = refs::unregister_across_roots(&m.old_models, &m.new_models, "heardright").unwrap();
    assert!(
        !old_refs.join("heardright.json").exists() && !new_refs.join("heardright.json").exists()
    );
    let freed: BTreeSet<_> = freed.into_iter().collect();
    assert_eq!(
        freed,
        ["already-here", "parakeet-tdt-v3", "unknown-id"]
            .map(String::from)
            .into_iter()
            .collect()
    );
    assert!(new_refs.join("scraperight.json").exists());

    // A corrupt registry on either side aborts a migration before any change.
    fs::write(old_refs.join("broken.json"), b"nope").unwrap();
    assert!(refs::migrate_root_refs(&m.old_models, &m.new_models).is_err());
    assert!(refs::unregister_across_roots(&m.old_models, &m.new_models, "scraperight").is_err());
    assert!(new_refs.join("scraperight.json").exists());
}

#[test]
fn ensure_prunes_retired_store_ids_only_when_unreferenced_and_identical() {
    let lay = layout();
    let archive = tar_gz(
        "wrap",
        &[("weights.bin", WEIGHTS), ("sub/vocab.txt", VOCAB)],
    );
    let server = serve(Config::new(vec![("/m/new.tar.gz", archive.clone())]));
    let mut spec = archive_spec(
        "new-id",
        Some(leak(format!("{}/m/new.tar.gz", server.base))),
        &archive,
        false,
    );
    spec.retired_store_ids = &["old-same", "old-diff", "old-ref"];
    for (id, bytes) in [
        ("old-same", WEIGHTS),
        ("old-diff", b"different-bytes".as_slice()),
        ("old-ref", WEIGHTS),
    ] {
        let d = lay.models.join(id);
        fs::create_dir_all(d.join("sub")).unwrap();
        fs::write(d.join("weights.bin"), bytes).unwrap();
        fs::write(d.join("sub/vocab.txt"), VOCAB).unwrap();
    }
    store::register_ref(&lay.models, "scraperight", "old-ref").unwrap();
    store::ensure_ready(&http(), &lay.models, &spec, &opts("heardright"), |_| {}).unwrap();
    assert!(
        !lay.models.join("old-same").exists(),
        "identical + unreferenced: reclaimed"
    );
    assert!(
        lay.models.join("old-diff").exists(),
        "distinct payload preserved"
    );
    assert!(
        lay.models.join("old-ref").exists(),
        "still referenced by another app"
    );
}

// ===================================================================
// Discovery, handle, device id
// ===================================================================

#[test]
fn discover_finds_peer_copy_only_when_every_pin_matches() {
    use rightkit_models::suite::discover;
    let spec = ModelSpec {
        tier: rightkit_models::suite::catalog::ModelTier::Base,
        entitlement: false,
        id: "parakeet-tdt-v3-coreml-compiled-5f8888117f47",
        label: "d",
        version: "v",
        size_hint_bytes: 0,
        files: spec_files(&[("weights.bin", WEIGHTS)]),
        source: SpecSource::Files(&[]),
        legacy_dirs: &[],
        retired_legacy_paths: &[],
        retired_store_ids: &[],
    };
    let data = tempfile::tempdir().unwrap();
    let sub = if cfg!(target_os = "macos") {
        "parakeet-tdt-v3"
    } else {
        "parakeet_tdt_v3_static1500_qint8_20260722"
    };
    let peer = data.path().join("HeardRightNext/models").join(sub);
    fs::create_dir_all(&peer).unwrap();
    fs::write(peer.join("weights.bin"), b"weights-byteZ").unwrap(); // same size, wrong bytes
    assert!(discover::find_existing_under(data.path(), &spec, true).is_none());
    fs::write(peer.join("weights.bin"), WEIGHTS).unwrap();
    let found = discover::find_existing_under(data.path(), &spec, true).unwrap();
    assert_eq!(
        (found.app, found.dir.as_path()),
        ("HeardRight", peer.as_path())
    );

    // The previous RightSuite layout is honoured only inside the legacy window.
    fs::remove_dir_all(data.path().join("HeardRightNext")).unwrap();
    let old = data.path().join("RightSuite/models").join(sub);
    fs::create_dir_all(&old).unwrap();
    fs::write(old.join("weights.bin"), WEIGHTS).unwrap();
    assert!(discover::find_existing_under(data.path(), &spec, true).is_some());
    assert!(discover::find_existing_under(data.path(), &spec, false).is_none());
    assert!(
        paths::legacy_locations_supported_for("0.1.12")
            && !paths::legacy_locations_supported_for("0.1.13")
    );
}

#[test]
fn model_store_handle_resolves_registers_ref_and_accepts_verified_sibling() {
    let lay = layout();
    let spec = ModelSpec {
        tier: rightkit_models::suite::catalog::ModelTier::Base,
        entitlement: false,
        id: "handle-model",
        label: "h",
        version: "h1",
        size_hint_bytes: 0,
        files: spec_files(&[("weights.bin", WEIGHTS)]),
        source: SpecSource::Files(&[]),
        legacy_dirs: &[],
        retired_legacy_paths: &[],
        retired_store_ids: &[],
    };
    let store = ModelStore::at_models_root(&lay.models, "cutright").with_http_client(http());
    assert!(store
        .resolve(&spec, &[])
        .unwrap_err()
        .contains("not installed"));
    let dir = installed_entry(&lay, "handle-model");
    fs::write(dir.join("weights.bin"), WEIGHTS).unwrap();
    assert!(
        store
            .resolve(&spec, &[])
            .unwrap_err()
            .contains("trusted revision"),
        "no manifest = not trusted"
    );
    manifest::write_manifest(&dir, &spec.expected_manifest()).unwrap();
    let resolved = store.resolve(&spec, &[]).unwrap();
    assert_eq!(
        (
            resolved.id.as_str(),
            resolved.revision.as_str(),
            resolved.dir.as_path()
        ),
        ("handle-model", "h1", dir.as_path())
    );
    assert!(
        lay.models.join("refs/cutright.json").is_file(),
        "resolve registers the ref so GC cannot reap it"
    );
    assert!(store::prune_unref_entries(&lay.models, &[])
        .unwrap()
        .is_empty());
}

#[test]
fn device_id_is_random_stable_and_private_to_the_app_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let a = rightkit_models::suite::install::device_id_in(&tmp.path().join("app-a")).unwrap();
    let again = rightkit_models::suite::install::device_id_in(&tmp.path().join("app-a")).unwrap();
    let b = rightkit_models::suite::install::device_id_in(&tmp.path().join("app-b")).unwrap();
    assert_eq!(a, again);
    assert_eq!(a.len(), 32);
    assert_ne!(a, b);
}

// ===================================================================
// Coexistence with the revisioned layout (today's additions)
// ===================================================================

#[test]
fn revisioned_store_shares_refs_gc_and_pin_enforcement_with_suite_layout() {
    use rightkit_models::{FileSpec, InstallOptions, ModelSpec as RevSpec, RevisionSpec, Store};
    let lay = layout();
    let server = serve(Config::new(vec![("/r/w.bin", WEIGHTS.to_vec())]));
    let file = |pinned: bool| FileSpec {
        path: "w.bin".into(),
        url: format!("{}/r/w.bin", server.base),
        bytes: pinned.then_some(WEIGHTS.len() as u64),
        sha256: pinned.then(|| sha(WEIGHTS)),
        platforms: vec![],
        extract: None,
        installed_bytes: None,
        installed_sha256: None,
        shared: None,
        mirrors: vec![],
    };
    let spec = |pinned| {
        RevSpec::new(
            "rev-model",
            vec![RevisionSpec {
                revision: "1".into(),
                quant: None,
                files: vec![file(pinned)],
                requirements: Default::default(),
                measured: vec![],
            }],
        )
    };
    let st = Store::new(&lay.models, "cutright")
        .with_http_options(rightkit_models::http::ModelHttpOptions {
            proxy: rightkit_models::http::ProxyMode::None,
            ..Default::default()
        })
        .with_address_policy(
            rightkit_http::AddressPolicy::public_web()
                .allow_if(vec!["127.0.0.1".to_string()], |ip| ip.is_loopback()),
        );

    // Trusted-catalog mode refuses unpinned files before any network traffic.
    let strict = InstallOptions {
        require_pinned: true,
        ..Default::default()
    };
    let err = st.install(&spec(false), &strict, |_| {}).unwrap_err();
    assert!(err.message.contains("pinned"), "{err}");
    assert!(server.requests("GET").is_empty());

    st.install(&spec(true), &strict, |_| {}).unwrap();
    assert_eq!(
        read(&st.active_dir("rev-model").unwrap().join("w.bin")),
        WEIGHTS
    );
    // The id "refs" can never be a model (it is the registry directory).
    assert!(st.model_dir("refs").is_err());

    st.register_app("heardright", "rev-model").unwrap();
    installed_entry(&lay, "suite-orphan"); // flat-layout orphan next to it
    assert!(
        st.uninstall_unreferenced("rev-model").is_err(),
        "referenced by heardright"
    );
    assert_eq!(
        st.gc(&[]).unwrap(),
        vec!["suite-orphan".to_string()],
        "GC reaps the orphan, keeps the referenced revisioned model"
    );
    assert!(st.active_dir("rev-model").is_some());

    assert_eq!(
        st.unregister_app("heardright").unwrap(),
        vec!["rev-model".to_string()]
    );
    st.uninstall_unreferenced("rev-model").unwrap();
    assert!(!lay.models.join("rev-model").exists());
}
