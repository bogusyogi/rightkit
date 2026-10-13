#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine as _;
use rightkit_daemon::remote::signing::{
    canonical_request, webhook_signature, H_NONCE, H_PROVIDER_SIGNATURE, H_PROVIDER_TIMESTAMP,
    H_SIGNATURE, H_TIMESTAMP,
};
use rightkit_daemon::remote::{
    Clock, Daemon, DaemonConfig, OAuthCallbackHandler, Request, Response,
};
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{json, Value};

pub const T0: u64 = 1_800_000_000;
pub const ALL_SCOPES: &[&str] = &["venture", "channel", "account"];

pub struct TestClock(pub AtomicU64);
impl Clock for TestClock {
    fn now_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

pub struct Harness {
    pub dir: tempfile::TempDir,
    pub clock: Arc<TestClock>,
    pub daemon: Arc<Daemon>,
    pub cfg: DaemonConfig,
    pub oauth: Option<Arc<dyn OAuthCallbackHandler>>,
}

impl Harness {
    pub fn new() -> Self {
        Self::with(|_| {})
    }

    pub fn with(tweak: impl FnOnce(&mut DaemonConfig)) -> Self {
        Self::with_oauth(tweak, None)
    }

    pub fn with_oauth(
        tweak: impl FnOnce(&mut DaemonConfig),
        oauth: Option<Arc<dyn OAuthCallbackHandler>>,
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = DaemonConfig::new(dir.path());
        // Small bounds keep the tests fast; production defaults come from the contract.
        cfg.limits.chunk_min = 4;
        cfg.limits.chunk_max = 64;
        cfg.limits.max_object_bytes = 1000;
        cfg.limits.session_ttl_secs = 100;
        cfg.webhook_secrets
            .insert("meta".into(), b"meta-secret".to_vec());
        tweak(&mut cfg);
        let clock = Arc::new(TestClock(AtomicU64::new(T0)));
        let daemon = Arc::new(Daemon::open(cfg.clone(), clock.clone(), oauth.clone()).unwrap());
        Self {
            dir,
            clock,
            daemon,
            cfg,
            oauth,
        }
    }

    /// Drop the daemon and open a new one on the same state directory.
    pub fn restart(&mut self) {
        self.daemon = Arc::new(
            Daemon::open(self.cfg.clone(), self.clock.clone(), self.oauth.clone()).unwrap(),
        );
    }

    pub fn now(&self) -> u64 {
        self.clock.0.load(Ordering::SeqCst)
    }

    pub fn advance(&self, secs: u64) {
        self.clock.0.fetch_add(secs, Ordering::SeqCst);
    }

    pub fn call(&self, req: Request) -> Response {
        self.daemon.handle(req)
    }

    pub fn enrol(&self) -> Dev {
        self.enrol_scopes(ALL_SCOPES)
    }

    pub fn enrol_scopes(&self, scopes: &[&str]) -> Dev {
        let code = self.daemon.issue_enrolment_code(scopes, 600).unwrap();
        let (dev, resp) = self.try_enrol(&code);
        assert_eq!(resp.status, 201, "{:?}", resp.body);
        dev
    }

    /// Enrol a fresh key with the given code; returns the response unchecked.
    pub fn try_enrol(&self, code: &str) -> (Dev, Response) {
        let dev = Dev::generate();
        let resp = self.call(dev.enrol_request(code, self.now(), &next_nonce()));
        let id = resp
            .body
            .get("deviceId")
            .and_then(Value::as_str)
            .map(str::to_string);
        (
            Dev {
                id: id.unwrap_or_else(|| "none".into()),
                ..dev
            },
            resp,
        )
    }

    pub fn events(&self, name: &str) -> Vec<Value> {
        self.daemon
            .events()
            .into_iter()
            .filter(|e| e["event"] == name)
            .collect()
    }

    pub fn rejections(&self) -> Vec<String> {
        self.events("AUTH_REJECTED")
            .iter()
            .map(|e| e["reason"].as_str().unwrap().to_string())
            .collect()
    }

    pub fn webhook(&self, provider: &str, secret: &[u8], ts: u64, body: &[u8]) -> Request {
        let mut headers = BTreeMap::new();
        headers.insert(H_PROVIDER_TIMESTAMP.to_string(), ts.to_string());
        headers.insert(
            H_PROVIDER_SIGNATURE.to_string(),
            webhook_signature(secret, ts, body),
        );
        Request {
            method: "POST".into(),
            path_and_query: format!("/v1/webhooks/{provider}"),
            headers,
            body: body.to_vec(),
        }
    }
}

static NONCE: AtomicU64 = AtomicU64::new(1);
pub fn next_nonce() -> String {
    format!("n-{}", NONCE.fetch_add(1, Ordering::SeqCst))
}

pub struct Dev {
    pub id: String,
    pub kp: Arc<Ed25519KeyPair>,
}

impl Dev {
    pub fn generate() -> Self {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let kp = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        Self {
            id: "none".into(),
            kp: Arc::new(kp),
        }
    }

    pub fn public_key_b64(&self) -> String {
        B64.encode(self.kp.public_key().as_ref())
    }

    fn sign(&self, method: &str, pq: &str, ts: u64, nonce: &str, body: &[u8]) -> String {
        B64.encode(
            self.kp
                .sign(canonical_request(method, pq, ts, nonce, body).as_bytes())
                .as_ref(),
        )
    }

    pub fn enrol_request(&self, code: &str, ts: u64, nonce: &str) -> Request {
        let body = serde_json::to_vec(&json!({ "publicKey": self.public_key_b64() })).unwrap();
        let mut headers = BTreeMap::new();
        headers.insert("authorization".into(), format!("RK-Enrol {code}"));
        headers.insert(H_TIMESTAMP.into(), ts.to_string());
        headers.insert(H_NONCE.into(), nonce.into());
        headers.insert(
            H_SIGNATURE.into(),
            self.sign("POST", "/v1/devices", ts, nonce, &body),
        );
        Request {
            method: "POST".into(),
            path_and_query: "/v1/devices".into(),
            headers,
            body,
        }
    }

    pub fn request_at(
        &self,
        method: &str,
        pq: &str,
        body: &[u8],
        ts: u64,
        nonce: &str,
        extra: &[(&str, &str)],
    ) -> Request {
        let mut headers = BTreeMap::new();
        headers.insert("authorization".into(), format!("RK-Device {}", self.id));
        headers.insert(H_TIMESTAMP.into(), ts.to_string());
        headers.insert(H_NONCE.into(), nonce.into());
        headers.insert(H_SIGNATURE.into(), self.sign(method, pq, ts, nonce, body));
        for (k, v) in extra {
            headers.insert(k.to_string(), v.to_string());
        }
        Request {
            method: method.into(),
            path_and_query: pq.into(),
            headers,
            body: body.to_vec(),
        }
    }

    pub fn request(&self, h: &Harness, method: &str, pq: &str, body: &[u8]) -> Request {
        self.request_at(method, pq, body, h.now(), &next_nonce(), &[])
    }

    pub fn call(&self, h: &Harness, method: &str, pq: &str, body: &[u8]) -> Response {
        h.call(self.request(h, method, pq, body))
    }

    pub fn json(&self, h: &Harness, method: &str, pq: &str, body: &Value) -> Response {
        self.call(h, method, pq, &serde_json::to_vec(body).unwrap())
    }

    pub fn patch_job(
        &self,
        h: &Harness,
        job: &str,
        if_match: u64,
        idem: &str,
        body: &Value,
    ) -> Response {
        let raw = serde_json::to_vec(body).unwrap();
        let req = self.request_at(
            "PATCH",
            &format!("/v1/publish-jobs/{job}"),
            &raw,
            h.now(),
            &next_nonce(),
            &[
                ("if-match", &if_match.to_string()),
                ("idempotency-key", idem),
            ],
        );
        h.call(req)
    }

    /// Start an upload; returns `(upload id, digest key bytes)`.
    pub fn upload_init(&self, h: &Harness, size: u64, chunk: u64) -> (String, [u8; 32]) {
        let r = self.json(
            h,
            "POST",
            "/v1/uploads",
            &json!({ "size": size, "chunkBytes": chunk }),
        );
        assert_eq!(r.status, 201, "{:?}", r.body);
        let key = rightkit_daemon::remote::unhex(r.body["digestKey"].as_str().unwrap()).unwrap();
        (
            r.body["uploadId"].as_str().unwrap().to_string(),
            key.try_into().unwrap(),
        )
    }

    pub fn put_chunk(&self, h: &Harness, id: &str, index: u64, data: &[u8]) -> Response {
        self.call(h, "PUT", &format!("/v1/uploads/{id}/chunks/{index}"), data)
    }

    pub fn complete(&self, h: &Harness, id: &str, digest: &str) -> Response {
        self.json(
            h,
            "POST",
            &format!("/v1/uploads/{id}/complete"),
            &json!({ "digest": digest }),
        )
    }
}

pub fn keyed_digest(key: &[u8; 32], data: &[u8]) -> String {
    blake3::keyed_hash(key, data).to_hex().to_string()
}

pub fn pattern_bytes(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 % 251) as u8).collect()
}
