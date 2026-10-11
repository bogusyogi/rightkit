//! Blocking device client for the daemon-api v1 routes this crate serves:
//! enrolment with an enrolment code, device-signed requests, resumable
//! uploads hashed with the per-session keyed blake3, and compare-and-swap job
//! updates. Behind the `client` feature; needs no async runtime.
//!
//! Signing and hashing are the server's own functions
//! ([`crate::signing::sign_request`] / [`verify_request`](crate::signing::verify_request),
//! [`chunk_digest`](crate::signing::chunk_digest), [`object_hasher`](crate::signing::object_hasher)),
//! so client and server cannot drift. TLS trusts only the root certificates
//! the caller supplies: there is no system-root fallback.
//!
//! ```no_run
//! use rightkit_daemon::client::{Client, ClientConfig, DeviceKey};
//! # fn run(addr: std::net::SocketAddr, root_pem: &[u8], code: &str) -> Result<(), rightkit_daemon::client::Error> {
//! let client = Client::new(ClientConfig::new(addr).root_pem(root_pem)?)?;
//! let pkcs8 = DeviceKey::generate_pkcs8()?; // the caller stores these bytes
//! let device = client.enrol(code, DeviceKey::from_pkcs8(&pkcs8)?, Some("ci"))?;
//! let receipt = device.upload(b"payload", 1_048_576)?;
//! # let _ = receipt; Ok(()) }
//! ```

mod error;
mod transport;

use std::io::{Read, Seek, SeekFrom};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

pub use error::{Error, ErrorCode, Refusal};
pub use transport::Reply;
use transport::Transport;

use crate::signing::{
    chunk_digest, object_digest, object_hasher, parse_digest_key, sign_request, H_NONCE,
    H_SIGNATURE, H_TIMESTAMP,
};
use crate::util::{hex, random_bytes, Clock, SystemClock};

/// Headers `send` owns: framing and the device credentials.
const RESERVED_HEADERS: [&str; 8] = [
    "host",
    "content-length",
    "connection",
    "transfer-encoding",
    "authorization",
    H_TIMESTAMP,
    H_NONCE,
    H_SIGNATURE,
];
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Where the daemon is and whom to trust.
#[derive(Clone)]
pub struct ClientConfig {
    addr: SocketAddr,
    server_name: String,
    roots: Vec<CertificateDer<'static>>,
    timeout: Duration,
    clock: Arc<dyn Clock>,
}

impl ClientConfig {
    /// Daemon at `addr`; the TLS server name defaults to `localhost`.
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            server_name: "localhost".into(),
            roots: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
            clock: Arc::new(SystemClock),
        }
    }

    /// The name the daemon's certificate must be valid for (a DNS name or an IP literal).
    pub fn server_name(mut self, name: impl Into<String>) -> Self {
        self.server_name = name.into();
        self
    }

    /// Trust the certificate(s) in this PEM. May be called more than once.
    pub fn root_pem(mut self, pem: &[u8]) -> Result<Self, Error> {
        let before = self.roots.len();
        for cert in CertificateDer::pem_slice_iter(pem) {
            self.roots
                .push(cert.map_err(|e| Error::Config(format!("root certificate PEM: {e}")))?);
        }
        if self.roots.len() == before {
            return Err(Error::Config("no certificate found in the root PEM".into()));
        }
        Ok(self)
    }

    /// Trust this DER certificate.
    pub fn root_der(mut self, der: Vec<u8>) -> Self {
        self.roots.push(CertificateDer::from(der));
        self
    }

    /// Connect, handshake and per-read/write timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Clock used for the request timestamp (the daemon accepts its replay window either way).
    pub fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
}

/// A device Ed25519 key. Generation and storage are the caller's: keep the
/// PKCS#8 bytes from [`DeviceKey::generate_pkcs8`] and rebuild with
/// [`DeviceKey::from_pkcs8`], or pass a 32-byte seed to [`DeviceKey::from_seed`].
pub struct DeviceKey {
    pair: Ed25519KeyPair,
}

impl DeviceKey {
    pub fn generate_pkcs8() -> Result<Vec<u8>, Error> {
        Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
            .map(|d| d.as_ref().to_vec())
            .map_err(|_| Error::Key("system randomness unavailable".into()))
    }

    pub fn from_pkcs8(pkcs8: &[u8]) -> Result<Self, Error> {
        Ed25519KeyPair::from_pkcs8(pkcs8)
            .map(|pair| Self { pair })
            .map_err(|e| Error::Key(format!("PKCS#8 Ed25519 key: {e}")))
    }

    pub fn from_seed(seed: &[u8; 32]) -> Result<Self, Error> {
        Ed25519KeyPair::from_seed_unchecked(seed)
            .map(|pair| Self { pair })
            .map_err(|e| Error::Key(format!("Ed25519 seed: {e}")))
    }

    /// Raw 32-byte public key.
    pub fn public_key(&self) -> Vec<u8> {
        self.pair.public_key().as_ref().to_vec()
    }

    /// Unpadded base64url public key, the `publicKey` of the enrolment body.
    pub fn public_key_b64(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.pair.public_key().as_ref())
    }
}

/// Unauthenticated transport to one daemon; enrols devices.
#[derive(Clone)]
pub struct Client {
    transport: Transport,
    clock: Arc<dyn Clock>,
}

impl Client {
    /// Build the client. Fails with [`Error::NoRoots`] when no root certificate was supplied.
    pub fn new(cfg: ClientConfig) -> Result<Self, Error> {
        if cfg.roots.is_empty() {
            return Err(Error::NoRoots);
        }
        let mut store = rustls::RootCertStore::empty();
        for cert in cfg.roots {
            store
                .add(cert)
                .map_err(|e| Error::Config(format!("root certificate: {e}")))?;
        }
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::Config(e.to_string()))?
        .with_root_certificates(store)
        .with_no_client_auth();
        let mut tls = tls;
        tls.alpn_protocols = vec![b"http/1.1".to_vec()];
        let server_name = ServerName::try_from(cfg.server_name.clone())
            .map_err(|e| Error::Config(format!("server name {:?}: {e}", cfg.server_name)))?;
        let host_header = if cfg.addr.port() == 443 {
            cfg.server_name.clone()
        } else {
            format!("{}:{}", cfg.server_name, cfg.addr.port())
        };
        Ok(Self {
            transport: Transport {
                tls: Arc::new(tls),
                addr: cfg.addr,
                server_name,
                host_header,
                timeout: cfg.timeout,
            },
            clock: cfg.clock,
        })
    }

    /// Enrol `key` with a single-use enrolment code. The enrolment request is signed with the key
    /// being enrolled (proof of possession), as the daemon requires.
    pub fn enrol(
        &self,
        code: &str,
        key: impl Into<Arc<DeviceKey>>,
        label: Option<&str>,
    ) -> Result<DeviceSession, Error> {
        let key: Arc<DeviceKey> = key.into();
        let mut body = json!({ "publicKey": key.public_key_b64() });
        if let Some(label) = label {
            body["label"] = Value::String(label.to_string());
        }
        let body = serde_json::to_vec(&body).map_err(|e| Error::Config(e.to_string()))?;
        let headers = self.signed_headers(
            &key,
            format!("RK-Enrol {code}"),
            "POST",
            "/v1/devices",
            &body,
        );
        let reply = check(
            self.transport
                .exchange("POST", "/v1/devices", &headers, &body)?,
        )?;
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Enrolled {
            device_id: String,
            scopes: Vec<String>,
        }
        let e: Enrolled = parse(&reply)?;
        Ok(DeviceSession {
            client: self.clone(),
            id: e.device_id,
            scopes: e.scopes,
            key,
        })
    }

    /// A session for an already-enrolled device (id and key persisted by the caller).
    pub fn session(
        &self,
        device_id: impl Into<String>,
        key: impl Into<Arc<DeviceKey>>,
    ) -> DeviceSession {
        DeviceSession {
            client: self.clone(),
            id: device_id.into(),
            scopes: Vec::new(),
            key: key.into(),
        }
    }

    fn signed_headers(
        &self,
        key: &DeviceKey,
        authorization: String,
        method: &str,
        path_and_query: &str,
        body: &[u8],
    ) -> Vec<(String, String)> {
        let ts = self.clock.now_secs();
        let nonce = hex(&random_bytes::<16>());
        let sig = sign_request(&key.pair, method, path_and_query, ts, &nonce, body);
        vec![
            ("authorization".into(), authorization),
            (H_TIMESTAMP.into(), ts.to_string()),
            (H_NONCE.into(), nonce),
            (H_SIGNATURE.into(), sig),
        ]
    }
}

/// A successful (2xx) reply.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    /// Lower-case names.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn json<T: DeserializeOwned>(&self) -> Result<T, Error> {
        serde_json::from_slice(&self.body).map_err(|e| Error::BadReply(e.to_string()))
    }
}

fn check(reply: Reply) -> Result<Reply, Error> {
    if (200..300).contains(&reply.status) {
        return Ok(reply);
    }
    let body: Value = serde_json::from_slice(&reply.body).unwrap_or(Value::Null);
    let code = ErrorCode::from_code(body.get("error").and_then(Value::as_str).unwrap_or(""));
    Err(Error::Refused(Refusal {
        status: reply.status,
        code,
        body,
    }))
}

fn parse<T: DeserializeOwned>(reply: &Reply) -> Result<T, Error> {
    serde_json::from_slice(&reply.body).map_err(|e| Error::BadReply(e.to_string()))
}

/// Daemon-issued ids reach the path unescaped (the daemon matches the raw path), so only
/// unreserved characters are allowed.
fn segment(s: &str) -> Result<&str, Error> {
    if !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~'))
    {
        Ok(s)
    } else {
        Err(Error::Config(format!(
            "id {s:?} is not a plain path segment"
        )))
    }
}

/// An enrolled device: signs every request with its key.
#[derive(Clone)]
pub struct DeviceSession {
    client: Client,
    id: String,
    scopes: Vec<String>,
    key: Arc<DeviceKey>,
}

impl std::fmt::Debug for DeviceSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceSession")
            .field("id", &self.id)
            .field("scopes", &self.scopes)
            .finish_non_exhaustive()
    }
}

/// An open upload session. The `digest_key` is returned by the daemon only at init: persist
/// the whole struct (it is serde-serializable) to resume after a crash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadSession {
    pub upload_id: String,
    /// Hex per-session blake3 key.
    pub digest_key: String,
    pub size: u64,
    pub chunk_bytes: u64,
    pub chunk_count: u64,
    pub expires_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadStatus {
    pub upload_id: String,
    pub size: u64,
    pub chunk_bytes: u64,
    pub chunk_count: u64,
    /// Indexes of chunks the daemon holds.
    pub received: Vec<u64>,
    /// Byte offset of the first missing chunk (`size` when none is missing).
    pub next_offset: u64,
    pub complete: bool,
    pub digest: Option<String>,
    pub expires_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadReceipt {
    pub upload_id: String,
    /// The keyed blake3 the daemon computed over the committed object.
    pub digest: String,
    pub size: u64,
}

/// A fresh or replayed `job.update`.
#[derive(Debug, Clone, PartialEq)]
pub struct JobUpdate {
    pub job_id: String,
    pub revision: u64,
    pub fields: Map<String, Value>,
    /// The daemon answered from its idempotency record (`Idempotency-Replayed: true`).
    pub replayed: bool,
}

impl DeviceSession {
    pub fn device_id(&self) -> &str {
        &self.id
    }

    /// Scopes granted at enrolment; empty for a session rebuilt with [`Client::session`].
    pub fn scopes(&self) -> &[String] {
        &self.scopes
    }

    /// One device-signed request. `path_and_query` is signed exactly as sent. Any non-2xx answer is
    /// an [`Error::Refused`] with the daemon's code.
    pub fn send(
        &self,
        method: &str,
        path_and_query: &str,
        extra_headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<Response, Error> {
        let method = method.to_ascii_uppercase();
        for (k, _) in extra_headers {
            let k = k.to_ascii_lowercase();
            if RESERVED_HEADERS.contains(&k.as_str()) {
                return Err(Error::Config(format!(
                    "header {k} is set by the client and cannot be overridden"
                )));
            }
        }
        let mut headers = self.client.signed_headers(
            &self.key,
            format!("RK-Device {}", self.id),
            &method,
            path_and_query,
            body,
        );
        for (k, v) in extra_headers {
            headers.push((k.to_ascii_lowercase(), v.to_string()));
        }
        let reply = check(self.client.transport.exchange(
            &method,
            path_and_query,
            &headers,
            body,
        )?)?;
        Ok(Response {
            status: reply.status,
            headers: reply.headers,
            body: reply.body,
        })
    }

    /// Compare-and-swap patch of a publish job. `if_match` is the revision you read (0 for a new
    /// job); a stale one is [`ErrorCode::RevisionConflict`] with `currentRevision` in the body.
    pub fn update_job(
        &self,
        job_id: &str,
        patch: &Map<String, Value>,
        if_match: u64,
        idempotency_key: &str,
    ) -> Result<JobUpdate, Error> {
        let path = format!("/v1/publish-jobs/{}", segment(job_id)?);
        let body = serde_json::to_vec(patch).map_err(|e| Error::Config(e.to_string()))?;
        let if_match = format!("\"{if_match}\"");
        let r = self.send(
            "PATCH",
            &path,
            &[
                ("if-match", &if_match),
                ("idempotency-key", idempotency_key),
            ],
            &body,
        )?;
        let v: Value = r.json()?;
        Ok(JobUpdate {
            job_id: v["jobId"].as_str().unwrap_or(job_id).to_string(),
            revision: v["revision"]
                .as_u64()
                .ok_or_else(|| Error::BadReply("job reply has no revision".into()))?,
            fields: v["fields"].as_object().cloned().unwrap_or_default(),
            replayed: r.header("idempotency-replayed") == Some("true"),
        })
    }

    /// Open an upload session for `size` bytes in `chunk_bytes` chunks.
    pub fn upload_init(&self, size: u64, chunk_bytes: u64) -> Result<UploadSession, Error> {
        let body = serde_json::to_vec(&json!({ "size": size, "chunkBytes": chunk_bytes }))
            .map_err(|e| Error::Config(e.to_string()))?;
        let r = self.send("POST", "/v1/uploads", &[], &body)?;
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Init {
            upload_id: String,
            digest_key: String,
            chunk_bytes: u64,
            chunk_count: u64,
            expires_at: u64,
        }
        let i: Init = r.json()?;
        if parse_digest_key(&i.digest_key).is_none() {
            return Err(Error::BadReply("digestKey is not 32 hex bytes".into()));
        }
        Ok(UploadSession {
            upload_id: i.upload_id,
            digest_key: i.digest_key,
            size,
            chunk_bytes: i.chunk_bytes,
            chunk_count: i.chunk_count,
            expires_at: i.expires_at,
        })
    }

    pub fn upload_status(&self, upload_id: &str) -> Result<UploadStatus, Error> {
        let path = format!("/v1/uploads/{}", segment(upload_id)?);
        self.send("GET", &path, &[], &[])?.json()
    }

    /// Send one chunk with its keyed blake3 in `X-Chunk-Digest`. Re-sending the same bytes is
    /// accepted (idempotent); different bytes at a stored index are [`ErrorCode::ChunkConflict`].
    pub fn upload_chunk(
        &self,
        session: &UploadSession,
        index: u64,
        data: &[u8],
    ) -> Result<(), Error> {
        let key = parse_digest_key(&session.digest_key)
            .ok_or_else(|| Error::Source("digest key is not 32 hex bytes".into()))?;
        let path = format!(
            "/v1/uploads/{}/chunks/{index}",
            segment(&session.upload_id)?
        );
        let digest = chunk_digest(&key, data);
        self.send("PUT", &path, &[("x-chunk-digest", &digest)], data)?;
        Ok(())
    }

    /// Commit the upload with the whole-object keyed blake3 (hex). A wrong digest is
    /// [`ErrorCode::HashMismatch`] and the daemon discards the session.
    pub fn upload_complete(&self, upload_id: &str, digest: &str) -> Result<UploadReceipt, Error> {
        let path = format!("/v1/uploads/{}/complete", segment(upload_id)?);
        let body = serde_json::to_vec(&json!({ "digest": digest }))
            .map_err(|e| Error::Config(e.to_string()))?;
        self.send("POST", &path, &[], &body)?.json()
    }

    /// Upload `data` end to end in a new session. If this fails part way the session is lost to
    /// the caller; to resume, use [`upload_init`](Self::upload_init) and keep the
    /// [`UploadSession`], then call [`upload_bytes`](Self::upload_bytes) again.
    pub fn upload(&self, data: &[u8], chunk_bytes: u64) -> Result<UploadReceipt, Error> {
        let session = self.upload_init(data.len() as u64, chunk_bytes)?;
        self.upload_bytes(&session, data)
    }

    /// [`upload_reader`](Self::upload_reader) over a byte slice.
    pub fn upload_bytes(
        &self,
        session: &UploadSession,
        data: &[u8],
    ) -> Result<UploadReceipt, Error> {
        self.upload_reader(session, &mut std::io::Cursor::new(data))
    }

    /// Send every chunk the daemon does not yet hold (per `upload.status`, so a resumed session
    /// skips what already arrived), then the whole-object digest, and commit. Safe to call again
    /// after any failure: completed work is not repeated, and an already-committed upload is
    /// re-acknowledged. `source` must hold exactly `session.size` bytes.
    pub fn upload_reader<R: Read + Seek>(
        &self,
        session: &UploadSession,
        source: &mut R,
    ) -> Result<UploadReceipt, Error> {
        let key = parse_digest_key(&session.digest_key)
            .ok_or_else(|| Error::Source("digest key is not 32 hex bytes".into()))?;
        let src = |e: std::io::Error| Error::Source(e.to_string());
        let len = source.seek(SeekFrom::End(0)).map_err(src)?;
        if len != session.size {
            return Err(Error::Source(format!(
                "source holds {len} bytes, session expects {}",
                session.size
            )));
        }
        let status = self.upload_status(&session.upload_id)?;
        let have: std::collections::BTreeSet<u64> = status.received.iter().copied().collect();
        let mut buf = Vec::new();
        for index in 0..session.chunk_count {
            if have.contains(&index) {
                continue;
            }
            let offset = index * session.chunk_bytes;
            let want = session.chunk_bytes.min(session.size - offset) as usize;
            buf.resize(want, 0);
            source.seek(SeekFrom::Start(offset)).map_err(src)?;
            source.read_exact(&mut buf).map_err(src)?;
            self.upload_chunk(session, index, &buf)?;
        }
        source.seek(SeekFrom::Start(0)).map_err(src)?;
        let mut hasher = object_hasher(&key);
        let mut block = vec![0u8; 1 << 20];
        loop {
            let n = source.read(&mut block).map_err(src)?;
            if n == 0 {
                break;
            }
            hasher.update(&block[..n]);
        }
        let digest = object_digest(&hasher);
        let receipt = self.upload_complete(&session.upload_id, &digest)?;
        if receipt.digest != digest {
            return Err(Error::BadReply(
                "daemon committed a digest different from the one sent".into(),
            ));
        }
        Ok(receipt)
    }
}
