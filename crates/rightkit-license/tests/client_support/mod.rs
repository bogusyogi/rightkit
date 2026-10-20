#![allow(dead_code)]
//! Shared fixtures of the native-client tests: a fault-injecting credential backend, a mock
//! licence server that issues sentinel-bearing tokens, and a fully wired `LicenseClient`.
//!
//! Needs the `client-testing` feature (the test origin is a loopback `http://` URL).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{
    AtomicBool, AtomicI64, AtomicU16, AtomicU64, AtomicUsize, Ordering::SeqCst,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use ed25519_dalek::Signer as _;
use rightkit_credentials::{
    Backend, ConditionalDelete, Config, Error, Interaction, Namespace, Result as CredResult,
    Secret, Store,
};
use rightkit_http::testing::{loopback_config, MockResponse, MockServer};
use rightkit_license::{
    canonical_lease_envelope, AccountClientConfig, AccountSessionClient, DeviceIdError,
    DeviceIdSource, LeaseEnvelope, LicenseClient, LicenseClientConfig, LicenseSetup,
};
use serde_json::{json, Value};
use tempfile::TempDir;

#[path = "../common/mod.rs"]
pub mod common;

pub const APP: &str = common::APP;
pub const DEVICE: &str = common::DEVICE;
pub const DEVICE_HASH: &str = "hash-of-dev-1";
pub const HOST: &str = "licence-test.example";
pub const ACCOUNT: &str = "license-record";
pub const MARKER: &str = "license-record.rotation-pending";

/// The licence key the mock server accepts.
pub const KEY: &str = "KEY-SENTINEL-0001-0002";
/// The activation token the mock server issues.
pub const ACTIVATION: &str = "SENTINEL_ACTIVATION_TOKEN";
/// The licence id inside every issued token.
pub const LICENSE_ID: &str = "lic_9f8e7d6c";
/// The Pro-bridge token the mock server issues and accepts.
pub const BRIDGE_TOKEN: &str = "BRIDGE-SENTINEL-0001";
/// The sign-in code (the token of the e-mailed link) the mock server accepts.
pub const SESSION_CODE: &str = "CODE-SENTINEL-1111";
/// The e-mail the mock account belongs to.
pub const OWNER_EMAIL: &str = "owner@example.com";
/// The device-link code the mock server issues and accepts.
pub const LINK_CODE: &str = "LINK-1234";
/// The credential account of the account session item.
pub const SESSION_ACCOUNT: &str = "account-session";

pub fn now() -> i64 {
    rightkit_license::system_now_unix().unwrap()
}

// ---- the credential backend -----------------------------------------------------------

/// An in-memory credential backend with injectable delay and failure.
#[derive(Default)]
pub struct TestBackend {
    items: Mutex<HashMap<String, Secret>>,
    /// Sleep this long inside the NEXT write of the PRIMARY item (not the marker), once.
    pub primary_set_delay_ms: AtomicU64,
    /// Fail every write of the primary item with a backend error (nothing is written).
    pub fail_primary_set: AtomicBool,
    /// Fail every conditional delete (marker removal) AFTER nothing changed.
    pub fail_marker_delete: AtomicBool,
    pub primary_sets: AtomicUsize,
    /// Every read answers "access denied" (a locked or refused credential store).
    pub deny_get: AtomicBool,
    /// Every write of a primary item answers "access denied" (nothing is written).
    pub deny_set: AtomicBool,
}

fn is_marker(account: &str) -> bool {
    account.ends_with(".rotation-pending")
}

impl TestBackend {
    pub fn text(&self, account: &str) -> Option<String> {
        self.items
            .lock()
            .unwrap()
            .get(account)
            .map(|s| s.expose_str().unwrap().to_string())
    }

    pub fn put_text(&self, account: &str, text: &str) {
        self.items
            .lock()
            .unwrap()
            .insert(account.to_string(), Secret::from_string(text));
    }

    pub fn remove(&self, account: &str) {
        self.items.lock().unwrap().remove(account);
    }

    /// The parsed envelope of the licence item.
    pub fn envelope(&self) -> Option<Value> {
        self.text(ACCOUNT)
            .map(|t| serde_json::from_str(&t).unwrap())
    }
}

impl Backend for TestBackend {
    fn name(&self) -> &'static str {
        "licence-test"
    }
    fn get(&self, _: &Namespace, account: &str, _: Interaction) -> CredResult<Option<Secret>> {
        if self.deny_get.load(SeqCst) {
            return Err(Error::Denied);
        }
        Ok(self.items.lock().unwrap().get(account).cloned())
    }
    fn exists(&self, _: &Namespace, account: &str, _: Interaction) -> CredResult<bool> {
        Ok(self.items.lock().unwrap().contains_key(account))
    }
    fn set(&self, _: &Namespace, account: &str, secret: &Secret, _: Interaction) -> CredResult<()> {
        if !is_marker(account) {
            self.primary_sets.fetch_add(1, SeqCst);
            // One-shot: the first write of the primary item after it is set is slow.
            let delay = self.primary_set_delay_ms.swap(0, SeqCst);
            if delay > 0 {
                std::thread::sleep(Duration::from_millis(delay));
            }
            if self.fail_primary_set.load(SeqCst) {
                return Err(Error::Backend { op: "set", code: 5 });
            }
            if self.deny_set.load(SeqCst) {
                return Err(Error::Denied);
            }
        }
        self.items
            .lock()
            .unwrap()
            .insert(account.to_string(), secret.clone());
        Ok(())
    }
    fn add_if_absent(
        &self,
        _: &Namespace,
        account: &str,
        secret: &Secret,
        _: Interaction,
    ) -> CredResult<bool> {
        let mut items = self.items.lock().unwrap();
        if items.contains_key(account) {
            return Ok(false);
        }
        items.insert(account.to_string(), secret.clone());
        Ok(true)
    }
    fn delete(&self, _: &Namespace, account: &str, _: Interaction) -> CredResult<bool> {
        Ok(self.items.lock().unwrap().remove(account).is_some())
    }
    fn delete_if_equals(
        &self,
        _: &Namespace,
        account: &str,
        expected: &Secret,
        _: Interaction,
    ) -> CredResult<ConditionalDelete> {
        if self.fail_marker_delete.load(SeqCst) {
            return Err(Error::Backend {
                op: "delete",
                code: 5,
            });
        }
        let mut items = self.items.lock().unwrap();
        match items.get(account) {
            None => Ok(ConditionalDelete::Absent),
            Some(current) if current.ct_eq(expected) => {
                items.remove(account);
                Ok(ConditionalDelete::Deleted)
            }
            Some(_) => Ok(ConditionalDelete::Changed),
        }
    }
}

// ---- the device ---------------------------------------------------------------------

pub struct FixedDevice;

impl DeviceIdSource for FixedDevice {
    fn device_id(&self) -> Result<String, DeviceIdError> {
        Ok(DEVICE.to_string())
    }
    fn device_id_hash(&self) -> Result<String, DeviceIdError> {
        Ok(DEVICE_HASH.to_string())
    }
}

// ---- the mock licence server ------------------------------------------------------------

/// How the mock answers a refresh.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RefreshMode {
    /// A new signed token.
    NewToken,
    /// Active, no token.
    Omitted,
    /// `status: revoked`.
    Revoked,
    /// HTTP 500.
    ServerError,
}

pub struct Server {
    pub activate_calls: AtomicUsize,
    pub refresh_calls: AtomicUsize,
    pub deactivate_calls: AtomicUsize,
    pub trial_calls: AtomicUsize,
    /// Delay before answering an activate, in milliseconds.
    pub activate_delay_ms: AtomicU64,
    pub refresh_mode: Mutex<RefreshMode>,
    /// Issued-at of the next token; each issuance adds one.
    pub next_iat: AtomicI64,
    /// Every token the server has issued, to prove none of them leaks.
    pub issued: Mutex<Vec<String>>,
    /// The trial answer's `ok`.
    pub trial_ok: AtomicBool,
    pub lease_calls: AtomicUsize,
    pub renew_calls: AtomicUsize,
    pub remove_calls: AtomicUsize,
    /// Generation of the lease the next issue answer carries.
    pub lease_generation: AtomicU64,
    /// Generation of the lease the next renew answer carries.
    pub renew_generation: AtomicU64,
    /// HTTP status of the renew answer (200, or 410 for a removed device).
    pub renew_status: AtomicU16,
    /// Sign the lease envelope with the wrong key.
    pub bad_signature: AtomicBool,
    /// Put another device hash in the lease.
    pub wrong_hash: AtomicBool,
    /// Seconds until the lease expires (negative: already expired).
    pub lease_ttl: AtomicI64,
    /// Issue tokens minted for another app.
    pub wrong_app: AtomicBool,
    /// Issue tokens bound to another device.
    pub wrong_device: AtomicBool,
    // ---- the Pro bridge and the link codes ----
    pub bridge_calls: AtomicUsize,
    pub exchange_calls: AtomicUsize,
    pub exchange_delay_ms: AtomicU64,
    /// How the exchange answers: 200 active, 201 says `revoked` in a 200, 500 a server error.
    pub exchange_mode: AtomicU16,
    pub link_request_calls: AtomicUsize,
    pub link_redeem_calls: AtomicUsize,
    /// HTTP status of the link request answer (200, or 404).
    pub link_request_status: AtomicU16,
    /// HTTP status of the link redeem answer (200, 404 or 409).
    pub link_redeem_status: AtomicU16,
    // ---- the account service ----
    pub signin_request_calls: AtomicUsize,
    pub signin_request_status: AtomicU16,
    pub consume_calls: AtomicUsize,
    pub consume_delay_ms: AtomicU64,
    pub me_calls: AtomicUsize,
    pub me_delay_ms: AtomicU64,
    /// HTTP status of `/auth/me` (200 or 401).
    pub me_status: AtomicU16,
    pub logout_calls: AtomicUsize,
    pub devices_calls: AtomicUsize,
    pub delete_calls: AtomicUsize,
    pub delete_delay_ms: AtomicU64,
    /// HTTP status of the account deletion answer (200, 401, 409, 429, 500).
    pub delete_status: AtomicU16,
    /// Every session token issued.
    pub sessions: Mutex<Vec<String>>,
    /// The session tokens that were logged out, in order.
    pub logged_out: Mutex<Vec<String>>,
}

/// Unix seconds as an RFC 3339 UTC time.
pub fn rfc3339(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

impl Server {
    fn token(&self) -> String {
        let device = if self.wrong_device.load(SeqCst) {
            "another-device"
        } else {
            DEVICE
        };
        self.token_for(device)
    }

    /// The live (issued and not logged out) session token `request` carries, if any.
    fn session_of(&self, request: &rightkit_http::testing::RecordedRequest) -> Option<String> {
        let token = request
            .header("authorization")?
            .strip_prefix("Bearer ")?
            .to_string();
        let live = self.sessions.lock().unwrap().contains(&token)
            && !self.logged_out.lock().unwrap().contains(&token);
        live.then_some(token)
    }

    /// A signed token bound to `device`, recorded as issued.
    pub fn token_for(&self, device: &'static str) -> String {
        let iat = self.next_iat.fetch_add(1, SeqCst);
        let mut claims = common::Claims::term(iat, now() + 86_400);
        claims.id = LICENSE_ID;
        claims.device = device;
        if self.wrong_app.load(SeqCst) {
            claims.app = "otherapp";
        }
        let token = claims.token();
        self.issued.lock().unwrap().push(token.clone());
        token
    }

    /// A signed lease envelope of `generation`, carrying a token bound to the device hash.
    fn lease(&self, generation: u64) -> Value {
        let at = now();
        let hash = if self.wrong_hash.load(SeqCst) {
            "another-device-hash"
        } else {
            DEVICE_HASH
        };
        let envelope = LeaseEnvelope {
            lease_id: "lease-1".to_string(),
            device_id_hash: hash.to_string(),
            pool: "computer".to_string(),
            issued_at: rfc3339(at),
            expires_at: rfc3339(at + self.lease_ttl.load(SeqCst)),
            grace_seconds: 60,
            generation,
            unverified_entitlement: None,
        };
        let canonical = canonical_lease_envelope(&envelope).unwrap();
        let key = if self.bad_signature.load(SeqCst) {
            common::other_signing_key()
        } else {
            common::signing_key()
        };
        let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(key.sign(canonical.as_bytes()).to_bytes());
        json!({
            "leaseId": envelope.lease_id,
            "deviceIdHash": envelope.device_id_hash,
            "pool": envelope.pool,
            "entitlement": self.token_for(DEVICE_HASH),
            "issuedAt": envelope.issued_at,
            "expiresAt": envelope.expires_at,
            "graceSeconds": envelope.grace_seconds,
            "generation": generation,
            "signature": signature,
        })
    }
}

pub struct Fixture {
    pub dir: TempDir,
    pub backend: Arc<TestBackend>,
    /// A store over the same backend, with the rotation lock directory of the client set.
    pub store: Store,
    pub server: MockServer,
    pub state: Arc<Server>,
    pub config: LicenseClientConfig,
}

impl Fixture {
    pub fn license_dir(&self) -> PathBuf {
        self.dir.path().join("license")
    }

    pub fn clock_path(&self) -> PathBuf {
        self.license_dir().join("clock.json")
    }

    /// A new client over the same directory, store and server (another "process").
    pub fn client(&self) -> LicenseClient {
        LicenseClient::new(
            LicenseSetup::configured(self.config.clone()),
            Arc::new(FixedDevice),
        )
        .unwrap()
    }

    /// The account session client over the same store, server and directory.
    pub fn account_client(&self) -> AccountSessionClient {
        self.account_client_with(|config| config)
    }

    /// An account client built from a modified copy of its configuration.
    pub fn account_client_with(
        &self,
        change: impl FnOnce(AccountClientConfig) -> AccountClientConfig,
    ) -> AccountSessionClient {
        AccountSessionClient::new(
            change(AccountClientConfig::new(&self.config, self.store.clone())),
            self.client(),
        )
        .unwrap()
    }

    /// The account lock file.
    pub fn account_lock_path(&self) -> PathBuf {
        self.license_dir().join("account").join("account.lock")
    }

    /// The parsed session item.
    pub fn session_item(&self) -> Option<Value> {
        self.backend
            .text(SESSION_ACCOUNT)
            .map(|t| serde_json::from_str(&t).unwrap())
    }

    /// A client built from a modified copy of the configuration.
    pub fn client_with(
        &self,
        change: impl FnOnce(LicenseClientConfig) -> LicenseClientConfig,
    ) -> LicenseClient {
        LicenseClient::new(
            LicenseSetup::configured(change(self.config.clone())),
            Arc::new(FixedDevice),
        )
        .unwrap()
    }

    /// Every text a server-issued secret could leak into, for a sentinel scan.
    pub fn secrets(&self) -> Vec<String> {
        let mut secrets = self.state.issued.lock().unwrap().clone();
        secrets.push(KEY.to_string());
        secrets.push(ACTIVATION.to_string());
        secrets.push(BRIDGE_TOKEN.to_string());
        secrets.push(SESSION_CODE.to_string());
        secrets.extend(self.state.sessions.lock().unwrap().iter().cloned());
        secrets
    }

    pub fn assert_no_secret(&self, what: &str, text: &str) {
        for secret in self.secrets() {
            assert!(!text.contains(&secret), "{what} leaks a secret: {text}");
        }
    }

    /// The raw bodies the server has received, as JSON.
    pub fn requests(&self, path: &str) -> Vec<Value> {
        self.server
            .requests()
            .iter()
            .filter(|r| r.path() == path)
            .map(|r| r.body_json())
            .collect()
    }

    /// A marker as a crashed writer would leave it.
    pub fn leave_marker(&self, text: &str) {
        let store = self
            .store
            .with_rotation_lock_dir(self.license_dir().join("rotation"));
        assert!(store
            .begin_rotation(ACCOUNT, &Secret::from_string(text))
            .unwrap());
    }
}

pub fn fixture() -> Fixture {
    fixture_with(Duration::from_secs(5))
}

/// `store_timeout` is the credential store's per-call timeout.
pub fn fixture_with(store_timeout: Duration) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(TestBackend::default());
    let store = Store::open(
        Config::new(Namespace::new("rk-license-test").unwrap())
            .backend(backend.clone())
            .timeout(store_timeout),
    );
    assert!(
        !store.guard_active(),
        "a credential guard variable replaced the test store"
    );
    let state = Arc::new(Server {
        activate_calls: AtomicUsize::new(0),
        refresh_calls: AtomicUsize::new(0),
        deactivate_calls: AtomicUsize::new(0),
        trial_calls: AtomicUsize::new(0),
        activate_delay_ms: AtomicU64::new(0),
        refresh_mode: Mutex::new(RefreshMode::NewToken),
        next_iat: AtomicI64::new(now() - 120),
        issued: Mutex::new(Vec::new()),
        trial_ok: AtomicBool::new(true),
        lease_calls: AtomicUsize::new(0),
        renew_calls: AtomicUsize::new(0),
        remove_calls: AtomicUsize::new(0),
        lease_generation: AtomicU64::new(1),
        renew_generation: AtomicU64::new(2),
        renew_status: AtomicU16::new(200),
        bad_signature: AtomicBool::new(false),
        wrong_hash: AtomicBool::new(false),
        lease_ttl: AtomicI64::new(3600),
        wrong_app: AtomicBool::new(false),
        wrong_device: AtomicBool::new(false),
        bridge_calls: AtomicUsize::new(0),
        exchange_calls: AtomicUsize::new(0),
        exchange_delay_ms: AtomicU64::new(0),
        exchange_mode: AtomicU16::new(200),
        link_request_calls: AtomicUsize::new(0),
        link_redeem_calls: AtomicUsize::new(0),
        link_request_status: AtomicU16::new(200),
        link_redeem_status: AtomicU16::new(200),
        signin_request_calls: AtomicUsize::new(0),
        signin_request_status: AtomicU16::new(200),
        consume_calls: AtomicUsize::new(0),
        consume_delay_ms: AtomicU64::new(0),
        me_calls: AtomicUsize::new(0),
        me_delay_ms: AtomicU64::new(0),
        me_status: AtomicU16::new(200),
        logout_calls: AtomicUsize::new(0),
        devices_calls: AtomicUsize::new(0),
        delete_calls: AtomicUsize::new(0),
        delete_delay_ms: AtomicU64::new(0),
        delete_status: AtomicU16::new(200),
        sessions: Mutex::new(Vec::new()),
        logged_out: Mutex::new(Vec::new()),
    });
    let handler_state = state.clone();
    let lease_base = format!("/v1/pro/{APP}/devices/lease");
    let link_base = format!("/v1/pro/{APP}/devices/windows-link");
    let server = MockServer::start(move |request, _| {
        let state = &handler_state;
        // A GET or DELETE carries no body: Null then, which indexes to Null.
        let body = serde_json::from_slice::<Value>(&request.body).unwrap_or(Value::Null);
        match request.path() {
            "/v1/licenses/activate" => {
                state.activate_calls.fetch_add(1, SeqCst);
                let delay = Duration::from_millis(state.activate_delay_ms.load(SeqCst));
                if body["licenseKey"] != KEY {
                    return MockResponse::json(
                        400,
                        &json!({"ok": false, "status": "invalid", "message": "SERVER TEXT"}),
                    )
                    .delay(delay);
                }
                MockResponse::json(
                    200,
                    &json!({
                        "ok": true,
                        "status": "active",
                        "activationToken": ACTIVATION,
                        "licenseId": LICENSE_ID,
                        "signedToken": state.token(),
                        "message": "SERVER TEXT"
                    }),
                )
                .delay(delay)
            }
            "/v1/licenses/refresh" => {
                state.refresh_calls.fetch_add(1, SeqCst);
                if body["activationToken"] != ACTIVATION {
                    return MockResponse::json(
                        400,
                        &json!({"ok": false, "status": "invalid", "message": "SERVER TEXT"}),
                    );
                }
                match *state.refresh_mode.lock().unwrap() {
                    RefreshMode::NewToken => MockResponse::json(
                        200,
                        &json!({"ok": true, "status": "active", "signedToken": state.token()}),
                    ),
                    RefreshMode::Omitted => {
                        MockResponse::json(200, &json!({"ok": true, "status": "active"}))
                    }
                    RefreshMode::Revoked => MockResponse::json(
                        200,
                        &json!({"ok": false, "status": "revoked", "message": "SERVER TEXT"}),
                    ),
                    RefreshMode::ServerError => MockResponse::text(500, "SERVER TEXT"),
                }
            }
            "/v1/licenses/deactivate" => {
                state.deactivate_calls.fetch_add(1, SeqCst);
                MockResponse::json(200, &json!({"ok": true}))
            }
            "/v1/licenses/trial" => {
                state.trial_calls.fetch_add(1, SeqCst);
                MockResponse::json(200, &json!({"ok": state.trial_ok.load(SeqCst)}))
            }
            path if path == lease_base => {
                state.lease_calls.fetch_add(1, SeqCst);
                if body["licenseKey"] != KEY {
                    return MockResponse::json(404, &json!({"error": "SERVER TEXT"}));
                }
                MockResponse::json(
                    200,
                    &json!({
                        "ok": true,
                        "activationToken": ACTIVATION,
                        "lease": state.lease(state.lease_generation.load(SeqCst)),
                    }),
                )
            }
            path if path == format!("{lease_base}/renew") => {
                state.renew_calls.fetch_add(1, SeqCst);
                match state.renew_status.load(SeqCst) {
                    200 => MockResponse::json(
                        200,
                        &json!({"ok": true, "lease": state.lease(state.renew_generation.load(SeqCst))}),
                    ),
                    other => MockResponse::json(other, &json!({"error": "SERVER TEXT"})),
                }
            }
            path if path == format!("{lease_base}/remove") => {
                state.remove_calls.fetch_add(1, SeqCst);
                MockResponse::json(200, &json!({"ok": true}))
            }
            "/v1/licenses/bridge/from-activation" => {
                state.bridge_calls.fetch_add(1, SeqCst);
                if body["activationToken"] != ACTIVATION {
                    return MockResponse::json(
                        400,
                        &json!({"ok": false, "status": "invalid", "message": "SERVER TEXT"}),
                    );
                }
                MockResponse::json(
                    200,
                    &json!({"ok": true, "status": "active", "bridgeToken": BRIDGE_TOKEN}),
                )
            }
            "/v1/licenses/bridge/exchange" => {
                state.exchange_calls.fetch_add(1, SeqCst);
                let delay = Duration::from_millis(state.exchange_delay_ms.load(SeqCst));
                if body["bridgeToken"] != BRIDGE_TOKEN {
                    return MockResponse::json(
                        400,
                        &json!({"ok": false, "status": "invalid", "message": "SERVER TEXT"}),
                    )
                    .delay(delay);
                }
                match state.exchange_mode.load(SeqCst) {
                    201 => MockResponse::json(
                        200,
                        &json!({"ok": false, "status": "revoked", "message": "SERVER TEXT"}),
                    )
                    .delay(delay),
                    500 => MockResponse::text(500, "SERVER TEXT").delay(delay),
                    _ => MockResponse::json(
                        200,
                        &json!({
                            "ok": true,
                            "status": "active",
                            "activationToken": ACTIVATION,
                            "licenseId": LICENSE_ID,
                            "lease": state.lease(state.lease_generation.load(SeqCst)),
                        }),
                    )
                    .delay(delay),
                }
            }
            path if path == format!("{link_base}/request") => {
                state.link_request_calls.fetch_add(1, SeqCst);
                if body["activationToken"] != ACTIVATION {
                    return MockResponse::json(404, &json!({"error": "SERVER TEXT"}));
                }
                match state.link_request_status.load(SeqCst) {
                    200 => MockResponse::json(
                        200,
                        &json!({"code": LINK_CODE, "expiresAt": rfc3339(now() + 600)}),
                    ),
                    other => MockResponse::json(other, &json!({"error": "SERVER TEXT"})),
                }
            }
            path if path == format!("{link_base}/redeem") => {
                state.link_redeem_calls.fetch_add(1, SeqCst);
                if body["code"] != LINK_CODE {
                    return MockResponse::json(404, &json!({"error": "SERVER TEXT"}));
                }
                match state.link_redeem_status.load(SeqCst) {
                    200 => MockResponse::json(
                        200,
                        &json!({
                            "ok": true,
                            "activationToken": ACTIVATION,
                            "lease": state.lease(state.lease_generation.load(SeqCst)),
                        }),
                    ),
                    other => MockResponse::json(other, &json!({"error": "SERVER TEXT"})),
                }
            }
            "/v1/shop/auth/magic-link/request" => {
                state.signin_request_calls.fetch_add(1, SeqCst);
                match state.signin_request_status.load(SeqCst) {
                    200 => MockResponse::json(200, &json!({"ok": true})),
                    other => MockResponse::json(other, &json!({"error": "SERVER TEXT"})),
                }
            }
            "/v1/shop/auth/magic-link/consume" => {
                state.consume_calls.fetch_add(1, SeqCst);
                let delay = Duration::from_millis(state.consume_delay_ms.load(SeqCst));
                if body["token"] != SESSION_CODE {
                    return MockResponse::json(409, &json!({"error": "SERVER TEXT"})).delay(delay);
                }
                let token = {
                    let mut sessions = state.sessions.lock().unwrap();
                    let token = format!("SESSION-SENTINEL-{}", sessions.len() + 1);
                    sessions.push(token.clone());
                    token
                };
                MockResponse::json(
                    200,
                    &json!({"token": token, "customer": {"email": OWNER_EMAIL, "id": "cust_1"}}),
                )
                .delay(delay)
            }
            "/v1/shop/auth/me" => {
                state.me_calls.fetch_add(1, SeqCst);
                let delay = Duration::from_millis(state.me_delay_ms.load(SeqCst));
                if state.me_status.load(SeqCst) == 401 || state.session_of(request).is_none() {
                    return MockResponse::json(401, &json!({"error": "SERVER TEXT"})).delay(delay);
                }
                MockResponse::json(200, &json!({"email": OWNER_EMAIL, "id": "cust_1"})).delay(delay)
            }
            "/v1/shop/auth/logout" => {
                state.logout_calls.fetch_add(1, SeqCst);
                if let Some(token) = state.session_of(request) {
                    state.logged_out.lock().unwrap().push(token);
                }
                MockResponse::json(200, &json!({"ok": true}))
            }
            "/v1/shop/account/devices" => {
                state.devices_calls.fetch_add(1, SeqCst);
                if state.session_of(request).is_none() {
                    return MockResponse::json(401, &json!({"error": "SERVER TEXT"}));
                }
                MockResponse::json(
                    200,
                    &json!({"items": [{
                        "id": "row-1",
                        "licenseKey": KEY,
                        "appKey": APP,
                        "platform": "macos",
                        "pool": "computer",
                        "label": "Work Mac",
                        "state": "active",
                        "lastSeenAt": rfc3339(now() - 60),
                        "activatedAt": rfc3339(now() - 3600),
                        "generation": 2
                    }]}),
                )
            }
            path if path.starts_with("/v1/shop/account/devices/") => {
                if state.session_of(request).is_none() {
                    return MockResponse::json(401, &json!({"error": "SERVER TEXT"}));
                }
                if path.ends_with("/gone") {
                    return MockResponse::json(404, &json!({"error": "SERVER TEXT"}));
                }
                MockResponse::json(200, &json!({"ok": true}))
            }
            "/v1/shop/account" => {
                state.delete_calls.fetch_add(1, SeqCst);
                let delay = Duration::from_millis(state.delete_delay_ms.load(SeqCst));
                if state.session_of(request).is_none() {
                    return MockResponse::json(401, &json!({"error": "SERVER TEXT"})).delay(delay);
                }
                match state.delete_status.load(SeqCst) {
                    200 => MockResponse::json(200, &json!({"ok": true})).delay(delay),
                    500 => MockResponse::text(500, "SERVER TEXT").delay(delay),
                    other => {
                        MockResponse::json(other, &json!({"error": "SERVER TEXT"})).delay(delay)
                    }
                }
            }
            _ => MockResponse::text(404, "SERVER TEXT"),
        }
    });
    let config = LicenseClientConfig::new(
        APP,
        HOST,
        "1.0.0",
        common::pubkey(),
        store.clone(),
        dir.path().join("license"),
    )
    .unwrap()
    .with_test_origin(server.base_url(), loopback_config());
    Fixture {
        dir,
        backend,
        store,
        server,
        state,
        config,
    }
}
