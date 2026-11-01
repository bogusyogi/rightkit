//! rightkit-models: install a two-revision model from a manifest through the real store, downloading from an
//! in-probe loopback HTTP server (strong ETag, Range, one deliberately truncated first body so the downloader must
//! resume), with SHA-256 pins, activation, upgrade, rollback, corruption detection, a wrong-digest refusal and
//! uninstall. No asset needed: the model bytes are generated here and the store lives in this run's data root.
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rightkit_models::hash::sha256_file;
use rightkit_models::http::{ModelHttpOptions, ProxyMode};
use rightkit_models::{ErrorCode, Event, InstallOptions, ModelSpec, Store, VerifyMode};
use serde_json::{json, Value};

/// `<resources>/models/<name>`: where the e2e app's bundled model assets sit, derived from the engine executable
/// (`Contents/Resources` for an executable in `Contents/MacOS` on macOS, the executable's directory on Windows).
/// Shared by the asr, vad, embed and ocr probes.
pub fn asset_dir(name: &str) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let resources = rightkit_onnx::installed_resource_dir(&exe)
        .ok_or_else(|| format!("no resources directory for {}", exe.display()))?;
    Ok(resources.join("models").join(name))
}

fn blob(seed: u8, n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

struct Server {
    base: String,
    log: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Loopback server: strong ETag and Range support; paths in `truncate_first` send half the body on their first
/// un-ranged request and then drop the socket.
fn serve(
    routes: HashMap<String, (Vec<u8>, String)>,
    truncate_first: Vec<String>,
) -> Result<Server, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("bind loopback: {e}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("nonblocking: {e}"))?;
    let base = format!(
        "http://{}",
        listener.local_addr().map_err(|e| format!("addr: {e}"))?
    );
    let log = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let truncated = Arc::new(Mutex::new(Vec::<String>::new()));
    let (log2, stop2) = (log.clone(), stop.clone());
    std::thread::spawn(move || {
        while !stop2.load(Ordering::Relaxed) {
            let stream = match listener.accept() {
                Ok((s, _)) => s,
                Err(_) => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                }
            };
            let _ = stream.set_nonblocking(false);
            let (routes, log, truncated, trunc) = (
                routes.clone(),
                log2.clone(),
                truncated.clone(),
                truncate_first.clone(),
            );
            std::thread::spawn(move || {
                let Ok(clone) = stream.try_clone() else {
                    return;
                };
                let mut reader = BufReader::new(clone);
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    return;
                }
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                let mut range = None;
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).is_err() || h.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = h.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        range = v.trim().trim_end_matches('-').parse::<usize>().ok();
                    }
                }
                if let Ok(mut l) = log.lock() {
                    l.push(format!("{path} range={range:?}"));
                }
                let mut out = stream;
                let Some((body, etag)) = routes.get(&path) else {
                    let _ = out.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
                    return;
                };
                let total = body.len();
                match range {
                    Some(start) if start < total => {
                        let _ = write!(out, "HTTP/1.1 206 Partial Content\r\nETag: \"{etag}\"\r\nContent-Range: bytes {start}-{}/{total}\r\nContent-Length: {}\r\n\r\n", total - 1, total - start);
                        let _ = out.write_all(&body[start..]);
                    }
                    _ => {
                        let _ = write!(out, "HTTP/1.1 200 OK\r\nETag: \"{etag}\"\r\nContent-Length: {total}\r\n\r\n");
                        let first = match truncated.lock() {
                            Ok(mut t) => {
                                let first = trunc.contains(&path) && !t.contains(&path);
                                if first {
                                    t.push(path.clone());
                                }
                                first
                            }
                            Err(_) => false,
                        };
                        let _ = out.write_all(if first { &body[..total / 2] } else { body });
                    }
                }
            });
        }
    });
    Ok(Server { base, log, stop })
}

fn opts(rev: &str) -> InstallOptions {
    InstallOptions {
        revision: Some(rev.into()),
        quant: Some("q4".into()),
        require_pinned: true,
        ..Default::default()
    }
}

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let src = ctx.dir("models-src")?;
    let mut routes = HashMap::new();
    let mut digests = HashMap::new();
    for (name, seed, n) in [
        ("v1.bin", 1u8, 300_000usize),
        ("v2.bin", 2, 200_000),
        ("tok.json", 3, 4_000),
    ] {
        let bytes = blob(seed, n);
        let path = src.join(name);
        std::fs::write(&path, &bytes).map_err(|e| format!("write {}: {e}", path.display()))?;
        let sha = sha256_file(&path)?;
        routes.insert(
            format!("/org/demo/resolve/main/{name}"),
            (bytes.clone(), sha[..16].to_string()),
        );
        digests.insert(name, (sha, bytes.len() as u64));
    }
    let server = serve(routes, vec!["/org/demo/resolve/main/v1.bin".into()])?;

    // A manifest as an app ships it: public https URLs; the store's URL rewrite aims them at the loopback server.
    let file = |path: &str, src: &str| {
        let (sha, bytes) = &digests[src];
        json!({ "path": path, "url": format!("https://models.example.test/org/demo/resolve/main/{src}"), "bytes": bytes, "sha256": sha })
    };
    let manifest = json!({
        "id": "e2e-demo",
        "label": "E2E demo",
        "revisions": [
            { "revision": "1", "quant": "q4", "files": [file("model.bin", "v1.bin"), file("tokenizer.json", "tok.json")] },
            { "revision": "2", "quant": "q4", "files": [file("model.bin", "v2.bin"), file("tokenizer.json", "tok.json")] },
        ]
    });
    let spec: ModelSpec = serde_json::from_value(manifest.clone())
        .map_err(|e| format!("manifest does not parse: {e}"))?;
    if serde_json::to_value(&spec).map_err(|e| format!("manifest serialise: {e}"))?["revisions"]
        .as_array()
        .map(Vec::len)
        != Some(2)
    {
        return Err("manifest lost a revision on round trip".into());
    }

    let root = ctx.dir("models-store")?;
    let store = Store::new(&root, "rke2e")
        .with_http_options(ModelHttpOptions {
            proxy: ProxyMode::None,
            ..Default::default()
        })
        .with_url_rewrite(&server.base)
        .map_err(|e| format!("url rewrite: {e}"))?;

    let mut events = Vec::new();
    let out = store
        .install(&spec, &opts("1"), |e| events.push(e))
        .map_err(|e| format!("install v1: {e:?}"))?;
    if out.already_installed {
        return Err("a fresh store reported already_installed".into());
    }
    let model_path = out.dir.join("model.bin");
    if sha256_file(&model_path)? != digests["v1.bin"].0 {
        return Err("installed model.bin does not match its pinned digest".into());
    }
    if !events.iter().any(|e| matches!(e, Event::Progress { .. }))
        || !matches!(events.last(), Some(Event::Activated { .. }))
    {
        return Err("install did not report progress and activation".into());
    }
    let log = server
        .log
        .lock()
        .map_err(|_| "server log poisoned")?
        .clone();
    if !log
        .iter()
        .any(|l| l.contains("v1.bin") && l.contains("range=Some("))
    {
        return Err(format!(
            "the truncated download was not resumed with a Range request: {log:?}"
        ));
    }
    if !store.verify("e2e-demo", &out.key, VerifyMode::Full) {
        return Err("Full verification of the fresh install failed".into());
    }
    if !store
        .install(&spec, &opts("1"), |_| {})
        .map_err(|e| format!("reinstall: {e:?}"))?
        .already_installed
    {
        return Err("re-installing the same revision was not idempotent".into());
    }

    // Upgrade, rollback.
    let up = store
        .install(&spec, &opts("2"), |_| {})
        .map_err(|e| format!("install v2: {e:?}"))?;
    if up.previous.as_deref() != Some("1@q4") {
        return Err(format!("upgrade previous was {:?}", up.previous));
    }
    let active = store
        .active_dir("e2e-demo")
        .ok_or("no active dir after upgrade")?;
    if sha256_file(&active.join("model.bin"))? != digests["v2.bin"].0 {
        return Err("active revision is not v2".into());
    }
    if store
        .rollback("e2e-demo", None)
        .map_err(|e| format!("rollback: {e:?}"))?
        != "1@q4"
    {
        return Err("rollback did not return 1@q4".into());
    }
    let active = store
        .active_dir("e2e-demo")
        .ok_or("no active dir after rollback")?;
    if sha256_file(&active.join("model.bin"))? != digests["v1.bin"].0 {
        return Err("rollback did not restore v1".into());
    }

    // Corruption is detected and the corrupt revision is not served.
    let rev1 = store
        .revision_dir("e2e-demo", "1@q4")
        .map_err(|e| format!("revision_dir: {e:?}"))?;
    std::fs::write(rev1.join("model.bin"), b"short").map_err(|e| format!("corrupt: {e}"))?;
    if store.verify("e2e-demo", "1@q4", VerifyMode::Full) || store.active_dir("e2e-demo").is_some()
    {
        return Err("a corrupted revision is still served".into());
    }
    store
        .uninstall_revision("e2e-demo", "1@q4")
        .map_err(|e| format!("uninstall revision: {e:?}"))?;
    if store
        .state("e2e-demo")
        .map_err(|e| format!("state: {e:?}"))?
        .active
        .as_deref()
        != Some("2@q4")
    {
        return Err("did not fall back to the verified v2 revision".into());
    }

    // A manifest whose pin is wrong is refused and installs nothing.
    let mut bad = manifest.clone();
    bad["id"] = json!("e2e-bad");
    bad["revisions"][0]["files"][0]["sha256"] = json!("0".repeat(64));
    let bad: ModelSpec = serde_json::from_value(bad).map_err(|e| format!("bad manifest: {e}"))?;
    match store.install(&bad, &opts("1"), |_| {}) {
        Err(e) if matches!(e.code, ErrorCode::HashMismatch | ErrorCode::Download) => {}
        other => {
            return Err(format!(
                "a wrong digest pin must fail with HashMismatch, got {other:?}"
            ))
        }
    }
    if store.active_dir("e2e-bad").is_some() {
        return Err("a refused install became active".into());
    }

    store
        .uninstall("e2e-demo")
        .map_err(|e| format!("uninstall: {e:?}"))?;
    if store
        .model_dir("e2e-demo")
        .map_err(|e| format!("model_dir: {e:?}"))?
        .exists()
    {
        return Err("uninstall left the model directory".into());
    }

    Ok(json!({
        "revisions_installed": 2,
        "resumed_by_range": true,
        "requests": log.len(),
        "wrong_digest_refused": true,
        "store_root": root.display().to_string(),
    }))
}
