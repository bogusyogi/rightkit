//! rightkit-vault: split-key secret sync. A loopback "entitlement service" serves the wrapping key over real HTTP to
//! `HttpServerKeySource` (rightkit-http); the vault seals a secret map into a blob file, the blob is checked for plaintext, a
//! tampered blob, a wrong client secret, a wrong id token, a revoked vault and an offline service are each observed.
//!
//! Needs: engine dependencies `rightkit-credentials` (client-secret and key-cache stores; the probe uses its in-memory backend,
//! no OS keychain item) and `rightkit-http` (the key source's client; no extra feature).
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rightkit_credentials::{Config as CredConfig, MemoryBackend, Namespace, Secret, Store};
use rightkit_http::{AddressPolicy, Client, ClientConfig};
use rightkit_vault::{
    BlobStore, Config, CredentialsClientSecret, CredentialsKeyCache, Error, HttpServerKeySource,
    SecretMap, Vault,
};
use serde_json::{json, Value};

const TOKEN: &str = "e2e-id-token";
const APP: &str = "e2eapp";
const SUBJECT: &str = "google-sub-1";

fn b64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Serve,
    Revoked,
}

struct Service {
    addr: String,
    mode: Arc<Mutex<Mode>>,
    hits: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Service {
    fn start() -> Result<Service, String> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("bind: {e}"))?;
        let addr = listener
            .local_addr()
            .map_err(|e| e.to_string())?
            .to_string();
        let mode = Arc::new(Mutex::new(Mode::Serve));
        let hits = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (m, h, s) = (mode.clone(), hits.clone(), stop.clone());
        let key = b64(&[7u8; 32]);
        let thread = std::thread::spawn(move || {
            for conn in listener.incoming() {
                if s.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut c) = conn else { continue };
                let _ = c.set_read_timeout(Some(Duration::from_secs(5)));
                let Some((line, headers)) = read_head(&mut c) else {
                    continue;
                };
                h.fetch_add(1, Ordering::SeqCst);
                let auth_ok = headers
                    .iter()
                    .any(|(k, v)| k == "authorization" && v == &format!("Bearer {TOKEN}"));
                let app_ok = headers.iter().any(|(k, v)| k == "x-right-app" && v == APP);
                let (status, body) = if !line.starts_with("GET /v1/vault/wrapping-key") {
                    (404, String::new())
                } else if !auth_ok || !app_ok {
                    (401, String::new())
                } else if *m.lock().unwrap() == Mode::Revoked {
                    (410, r#"{"error":"vault_revoked"}"#.to_string())
                } else {
                    (
                        200,
                        format!(r#"{{"key_id":"wk-1","key":"{key}","previous":[]}}"#),
                    )
                };
                let resp = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = c.write_all(resp.as_bytes());
            }
        });
        Ok(Service {
            addr,
            mode,
            hits,
            stop,
            thread: Some(thread),
        })
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(&self.addr);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn read_head(c: &mut TcpStream) -> Option<(String, Vec<(String, String)>)> {
    let mut buf = Vec::new();
    let mut b = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = c.read(&mut b).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&b[..n]);
    }
    let text = String::from_utf8_lossy(&buf).to_string();
    let mut lines = text.split("\r\n");
    let line = lines.next()?.to_string();
    let headers = lines
        .take_while(|l| !l.is_empty())
        .filter_map(|l| {
            l.split_once(':')
                .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        })
        .collect();
    Some((line, headers))
}

struct FileBlob(PathBuf);
impl BlobStore for FileBlob {
    fn read(&self) -> rightkit_vault::Result<Option<Vec<u8>>> {
        match std::fs::read(&self.0) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::Store(e.to_string())),
        }
    }
    fn write(&self, bytes: &[u8]) -> rightkit_vault::Result<()> {
        std::fs::write(&self.0, bytes).map_err(|e| Error::Store(e.to_string()))
    }
}

fn cred_store() -> Result<Store, String> {
    let ns = Namespace::new("e2e.rightkit.vault").map_err(|e| format!("namespace: {e}"))?;
    Ok(Store::open(
        CredConfig::new(ns).backend(Arc::new(MemoryBackend::new())),
    ))
}

fn client() -> Arc<Client> {
    let cfg = ClientConfig::first_party(vec!["127.0.0.1".into()])
        .address_policy(AddressPolicy::default().allow_loopback())
        .https_only(false);
    Arc::new(Client::new(cfg))
}

fn vault(svc: &Service, blob: &PathBuf, keystore: &Store, secretstore: &Store) -> Vault {
    Vault::new(
        Config::new(APP, SUBJECT),
        Arc::new(HttpServerKeySource::new(client(), svc.url(), APP)),
        Arc::new(CredentialsKeyCache::new(keystore.clone(), APP, SUBJECT)),
        Arc::new(CredentialsClientSecret::new(
            secretstore.clone(),
            APP,
            SUBJECT,
        )),
        Arc::new(FileBlob(blob.clone())),
    )
}

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let dir = ctx.dir("vault")?;
    let blob_path = dir.join("vault.blob");
    let mut svc = Service::start()?;
    let (keys, secrets_store) = (cred_store()?, cred_store()?);
    let good = || -> rightkit_vault::Result<String> { Ok(TOKEN.to_string()) };

    let mut map: SecretMap = BTreeMap::new();
    map.insert(
        "openai-api-key".into(),
        Secret::from_string("sk-e2e-PLAINTEXT-MARKER-123"),
    );
    map.insert(
        "anthropic-api-key".into(),
        Secret::from_string("sk-ant-e2e-OTHER-MARKER-456"),
    );

    let v = vault(&svc, &blob_path, &keys, &secrets_store);
    if v.load(&good)
        .map_err(|e| format!("empty load: {e}"))?
        .is_some()
    {
        return Err("an empty vault loaded a map".into());
    }
    v.save(&good, &map).map_err(|e| format!("save: {e}"))?;
    if svc.hits.load(Ordering::SeqCst) == 0 {
        return Err("save never reached the loopback entitlement service".into());
    }
    let raw = std::fs::read(&blob_path).map_err(|e| format!("read blob: {e}"))?;
    let raw_text = String::from_utf8_lossy(&raw).to_string();
    for needle in [
        "PLAINTEXT-MARKER",
        "OTHER-MARKER",
        "openai-api-key",
        "anthropic-api-key",
    ] {
        if raw_text.contains(needle) {
            return Err(format!("the sealed blob contains `{needle}`"));
        }
    }
    let back = v
        .load(&good)
        .map_err(|e| format!("load: {e}"))?
        .ok_or("saved vault loads as empty")?;
    if back.len() != 2
        || !back["openai-api-key"].ct_eq(&map["openai-api-key"])
        || !back["anthropic-api-key"].ct_eq(&map["anthropic-api-key"])
    {
        return Err("the loaded map differs from the saved one".into());
    }

    // Tamper: change one character inside the sealed payload; AEAD must refuse.
    let mut json: Value =
        serde_json::from_slice(&raw).map_err(|e| format!("blob is not json: {e}"))?;
    let data = json["data"]
        .as_str()
        .ok_or("blob has no data field")?
        .to_string();
    let mid = data.len() / 2;
    let mut chars: Vec<char> = data.chars().collect();
    chars[mid] = if chars[mid] == 'A' { 'B' } else { 'A' };
    json["data"] = Value::String(chars.into_iter().collect());
    std::fs::write(
        &blob_path,
        serde_json::to_vec(&json).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    match v.load(&good) {
        Err(Error::Decrypt) => {}
        other => {
            return Err(format!(
                "tampered blob did not fail authentication: {:?}",
                other.map(|m| m.map(|m| m.len()))
            ))
        }
    }
    // A vault must refuse to overwrite a blob it cannot open.
    if v.save(&good, &map).is_ok() {
        return Err("save replaced a blob it could not authenticate".into());
    }
    std::fs::write(&blob_path, &raw).map_err(|e| e.to_string())?;
    v.load(&good)
        .map_err(|e| format!("restored blob: {e}"))?
        .ok_or("restored blob empty")?;

    // A device with no client secret cannot open it; one with a different client secret fails authentication.
    let new_device_secrets = cred_store()?;
    let v_new = vault(&svc, &blob_path, &keys, &new_device_secrets);
    if !matches!(v_new.load(&good), Err(Error::ClientSecretMissing)) {
        return Err("a device without the client secret did not get ClientSecretMissing".into());
    }
    CredentialsClientSecret::new(new_device_secrets.clone(), APP, SUBJECT)
        .install_if_absent(&rightkit_vault::ClientSecret::generate().map_err(|e| e.to_string())?)
        .map_err(|e| format!("install client secret: {e}"))?;
    if !matches!(v_new.load(&good), Err(Error::Decrypt)) {
        return Err("a wrong client secret did not fail authentication".into());
    }

    // The service rejects a bad id token (fresh key cache so the service is actually asked).
    let v_badtok = vault(&svc, &blob_path, &cred_store()?, &secrets_store);
    let bad = || -> rightkit_vault::Result<String> { Ok("not-the-token".to_string()) };
    if !matches!(v_badtok.load(&bad), Err(Error::Unauthorized)) {
        return Err("a bad id token was not Unauthorized".into());
    }

    // Rotation re-wraps without losing data.
    v.rotate(&good).map_err(|e| format!("rotate: {e}"))?;
    if v.load(&good).map_err(|e| e.to_string())?.map(|m| m.len()) != Some(2) {
        return Err("load after rotate lost data".into());
    }

    // Offline within the grace window: the cached server half still opens it with the service gone.
    let url_hits = svc.hits.load(Ordering::SeqCst);
    let mut svc_for_offline = svc;
    let svc_addr_unused = svc_for_offline.url();
    svc_for_offline.shutdown();
    let _ = svc_addr_unused;
    if v.load(&good)
        .map_err(|e| format!("offline load from cache: {e}"))?
        .map(|m| m.len())
        != Some(2)
    {
        return Err("offline cached load lost data".into());
    }
    // ...and with no cache and no service, the vault says Offline.
    let v_cold = vault(&svc_for_offline, &blob_path, &cred_store()?, &secrets_store);
    if !matches!(v_cold.load(&good), Err(Error::Offline)) {
        return Err("a cold vault with the service down was not Offline".into());
    }

    // Revocation, on a second service instance: refresh sees 410 and the cache is dropped.
    let mut svc2 = Service::start()?;
    let v2 = vault(&svc2, &blob_path, &cred_store()?, &secrets_store);
    v2.load(&good)
        .map_err(|e| format!("load via second service: {e}"))?
        .ok_or("empty via second service")?;
    *svc2.mode.lock().unwrap() = Mode::Revoked;
    if !matches!(v2.refresh(&good), Err(Error::Revoked)) {
        return Err("refresh against an erased vault was not Revoked".into());
    }
    svc2.shutdown();

    Ok(json!({
        "service_hits_before_offline": url_hits,
        "sealed_blob_bytes": raw.len(),
        "checked": ["roundtrip", "no plaintext/names in blob", "tamper->Decrypt", "no overwrite of unopenable blob", "client secret missing/wrong", "unauthorized token", "rotate", "offline from cache", "cold offline", "revoked"],
    }))
}
