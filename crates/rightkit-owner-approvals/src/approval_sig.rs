//! Signed owner approvals (security record S1; docs/architecture/2026-10-17-open-issues/security-remote-admin.md).
//!
//! An approval is accepted on a signature, never on a caller's word. The owner's device holds a
//! biometric-gated P-256 key (the Mac owner app: a Secure Enclave key with `.biometryCurrentSet`); its
//! public key is registered here once, through an owner-only path, and the engine verifies every
//! signature itself (`ring`) before it honours an unlock. The PIN stays a separate, engine-checked
//! path (`unlock::verify_pin`); it is never an approver key and never valid for admin or remote.
//!
//! **Wire format (v1).** The owner signs these UTF-8 lines joined by `\n`, no trailing newline:
//!
//! ```text
//! pulse.approval.v1
//! kind:<approve | unlock | enroll | revoke | recover>
//! request:<engine request id | "enroll" | "revoke" | recovery challenge>
//! action:<request digest hex (approvals::request_digest) | sha256 of the public key | key id | unlock::recovery_action>
//! client:<engine's client key, e.g. chat:<id>; empty for enroll and revoke; engine:<instance> for recover>
//! device:<registered device id>
//! key:<key id: first 32 hex of SHA-256 of the public key>
//! nonce:<32 to 64 hex characters, fresh per signature>
//! issued:<Unix seconds>
//! expires:<Unix seconds>
//! ```
//!
//! The signature is ECDSA P-256 over SHA-256 of those bytes, either 64 bytes raw `r||s` (CryptoKit
//! `rawRepresentation`) or ASN.1 DER (`derRepresentation`), hex encoded. The public key is the 65
//! byte uncompressed point (CryptoKit `x963Representation`), hex encoded. `expires - issued` is at
//! most 120 seconds and `issued` may not lie more than 60 seconds in the future; a nonce is accepted
//! once (kept in engine memory until it expires, so a restart also forgets pending requests, which
//! the unlock record does too). The engine builds the `request`, `action` and `client` lines from
//! its own record of the waiting request, never from the caller, so a signature for one request
//! cannot answer another, another chat or another kind. A `recover` signature answers one
//! single-use challenge of `remote.security.recover.challenge`, for the exact repair and new PIN
//! it names, on the engine process that issued it (`unlock::authorise_recovery`).
//!
//! **Platforms.** The Mac owner app signs with a P-256 key. Windows Hello (`KeyCredentialManager`)
//! keys are RSA-2048 (RSASSA-PKCS1-v1_5, SHA-256): the key is the 270 byte PKCS#1 `RSAPublicKey`
//! (device `windows-hello`), the signature the 256 byte RSA signature over the same lines, and the
//! algorithm follows from the key's shape, which enrolment pins (`is_p256`, `is_hello_rsa`). The
//! Hello key is the FIRST owner-check key enrolled without the PIN (`enrol_owner_check_key`, hub
//! or owner app only) into `owner-check-keys.json`, an HMAC-sealed file (secret in the OS credential
//! store), and verified by `verify_owner_check`, so it survives restarts with no PIN. Adding,
//! replacing or revoking an owner-check key after that needs the PIN or a signature by an
//! enrolled owner-check key.
//!
//! **Registry.** The approver keys live in one OS credential item (`pulse.owner-approvers`,
//! account `registry-v1`), at most four, read and written like `protected_store.rs` does (macOS: pinned
//! to the login keychain and checked for the host app's team partition; HMAC seal under the PIN-derived key;
//! a blob whose seal is not proven reads as an empty registry; a marker file makes a deleted item
//! `deleted`, never a first run). Every enrolment and revocation, the first included, needs the host app
//! PIN verified in the engine (or a signature of a proven key): same-user code that impersonates the
//! owner app or writes the item cannot make an approver. A new PIN set after a lost one discards the
//! seal key, so keys are enrolled again. An unreadable registry fails closed. A key marked `dev` is refused
//! in every build, so a dev or release engine never trusts one.

use crate::{Refusal, Result};
pub use rightkit_device_link_types::KeyBinding;
use rightkit_fs::digest::hex;
use ring::signature::{
    ECDSA_P256_SHA256_ASN1, ECDSA_P256_SHA256_FIXED, RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey,
};
use serde_json::{Value, json};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const SERVICE: &str = "pulse.owner-approvers";
const ACCOUNT: &str = "registry-v1";
const MAX_KEYS: usize = 4;
const MAX_LIFETIME: u64 = 120;
const SKEW: u64 = 60;
const MAX_NONCES: usize = 1024;

/// One enrolled approver key.
#[derive(Clone)]
struct Approver {
    key_id: String,
    public: Vec<u8>,
    device: String,
    dev: bool,
}

/// A signature and the fields it commits to, as the owner's device sent them (`sig` of the params).
pub struct Signed {
    pub key_id: String,
    pub nonce: String,
    pub issued: u64,
    pub expires: u64,
    signature: Vec<u8>,
}

impl Signed {
    /// Reads `{keyId, nonce, issued, expires, signature}` from `value`.
    pub fn from_value(value: &Value) -> Result<Signed> {
        let bad = || {
            Refusal::new(
                "approval_signature_invalid",
                "the signed approval is malformed",
            )
        };
        let text = |k: &str| value[k].as_str().map(str::to_string).ok_or_else(bad);
        let number = |k: &str| value[k].as_u64().ok_or_else(bad);
        Ok(Signed {
            key_id: text("keyId")?,
            nonce: text("nonce")?,
            issued: number("issued")?,
            expires: number("expires")?,
            signature: unhex(&text("signature")?).ok_or_else(bad)?,
        })
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The longest hex text read here (a signature or public key is well under this).
const MAX_HEX_CHARS: usize = 600;

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() > MAX_HEX_CHARS {
        return None;
    }
    rightkit_fs::digest::unhex(text)
}

pub fn key_id_of(public: &[u8]) -> String {
    hex(&rightkit_crypto::sha256(public)[..16])
}

/// A device id or other label that cannot break the line format.
fn plain(text: &str, max: usize) -> bool {
    !text.is_empty()
        && text.len() <= max
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':'))
}

// ---- registry ----------------------------------------------------------------------------------

static REGISTRY_GATE: Mutex<()> = Mutex::new(());
const MARKER_FILE: &str = "owner-approvers.marker";

/// The registry as read. `state`: `proven` (sealed by the engine), `fresh` (never written),
/// `deleted` (the marker says it existed, the item is gone) or `unproven` (the seal does not check,
/// or the blob is damaged, or the PIN-wrapped seal key is not open yet). Only `proven` keys count.
struct Registry {
    keys: Vec<Approver>,
    state: &'static str,
}

fn product() -> &'static str {
    rightkit_app_host::host().product_name()
}

fn unavailable(why: impl std::fmt::Display) -> Refusal {
    Refusal::new(
        "approver_registry_unavailable",
        format!("the owner's approver keys cannot be read or written: {why}"),
    )
}

fn store() -> Result<rightkit_credentials::Store> {
    store_for(SERVICE)
}

fn store_for(service: &str) -> Result<rightkit_credentials::Store> {
    let namespace = rightkit_credentials::Namespace::new(service.to_string())
        .map_err(|e| unavailable(e.to_string()))?;
    let store = rightkit_credentials::Store::open(rightkit_credentials::Config::new(namespace));
    if !store.is_durable() {
        return Err(unavailable("the store is guarded or unavailable"));
    }
    Ok(store)
}

fn marker_path() -> Option<std::path::PathBuf> {
    rightkit_app_host::state_directory()
        .ok()
        .map(|dir| dir.join(MARKER_FILE))
}

/// The bytes the seal covers: a version line, then every key in key-id order.
fn seal_text(keys: &[Approver]) -> String {
    let mut sorted: Vec<&Approver> = keys.iter().collect();
    sorted.sort_by(|a, b| a.key_id.cmp(&b.key_id));
    let mut out = String::from("pulse.approvers.v1\n");
    for k in sorted {
        out.push_str(&format!(
            "{}|{}|{}|{}\n",
            k.key_id,
            hex(&k.public),
            k.device,
            k.dev
        ));
    }
    out
}

fn parse(text: &str) -> Option<(Vec<Approver>, Option<String>)> {
    let value: Value = serde_json::from_str(text).ok()?;
    if value["v"] != 1 {
        return None;
    }
    let keys = value["keys"]
        .as_array()?
        .iter()
        .map(|v| {
            let public = unhex(v["publicKey"].as_str()?)?;
            let device = v["device"].as_str()?.to_string();
            let key_id = v["keyId"].as_str()?.to_string();
            (key_id_of(&public) == key_id && plain(&device, 40)).then(|| Approver {
                key_id,
                public,
                device,
                dev: v["dev"].as_bool().unwrap_or(false),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some((keys, value["mac"].as_str().map(str::to_string)))
}

/// Reads the item like `protected_store.rs` does: on macOS pinned to the login keychain and checked
/// for the host app's team partition before and after (`protected_guard::read_item`), elsewhere through
/// the credential store.
fn read_blob() -> Result<Option<String>> {
    match crate::protected_guard::read_item(SERVICE, ACCOUNT) {
        Some(read) => read.map_err(unavailable),
        None => match store()?.get(ACCOUNT) {
            Ok(Some(secret)) => Ok(Some(
                secret
                    .expose_str()
                    .map_err(|e| unavailable(e.to_string()))?
                    .to_owned(),
            )),
            Ok(None) => Ok(None),
            Err(e) => Err(unavailable(e.to_string())),
        },
    }
}

/// The registry. A store that cannot be read is an error (fail closed); a blob whose seal is not
/// proven reads as an empty registry, and a deleted item is not a first run (the marker).
fn load() -> Result<Registry> {
    let Some(text) = read_blob()? else {
        let state = if marker_path().is_some_and(|m| m.exists()) {
            "deleted"
        } else {
            "fresh"
        };
        return Ok(Registry {
            keys: Vec::new(),
            state,
        });
    };
    let unproven = Registry {
        keys: Vec::new(),
        state: "unproven",
    };
    let Some((keys, mac)) = parse(&text) else {
        return Ok(unproven);
    };
    // Like a loosened gate in `protected_store.rs::parse`, the seal counts only when its key came from
    // the PIN in this process: a clear-key seal (no PIN set) can be forged by any process of the user.
    let proven = matches!(
        crate::protected_guard::check_seal(seal_text(&keys).as_bytes(), mac.as_deref()),
        crate::protected_guard::Seal::Valid
    ) && crate::protected_guard::key_from_pin();
    Ok(if proven {
        Registry {
            keys,
            state: "proven",
        }
    } else {
        unproven
    })
}

/// Seals and writes the registry, checks the item's partition afterwards (macOS) and records the
/// marker. The seal key is PIN-wrapped once a PIN is set, so this needs the PIN to have been
/// verified since the engine started (enrolment verifies it).
fn save(keys: &[Approver]) -> Result<()> {
    let pin_set = crate::hooks().pin_is_set()?;
    let mac = crate::protected_guard::make_seal(seal_text(keys).as_bytes(), pin_set).ok_or_else(|| {
        unavailable(format!("the registry cannot be sealed until the {p} PIN has been entered since {p} started", p = product()))
    })?;
    let list: Vec<Value> = keys
        .iter()
        .map(|k| json!({"keyId": k.key_id, "publicKey": hex(&k.public), "device": k.device, "dev": k.dev}))
        .collect();
    let blob = json!({"v": 1, "keys": list, "mac": mac}).to_string();
    store()?
        .set_str(ACCOUNT, &blob)
        .map_err(|e| unavailable(e.to_string()))?;
    crate::protected_guard::check_item(SERVICE, ACCOUNT).map_err(unavailable)?;
    let marker = marker_path().ok_or_else(|| unavailable("the data folder is unknown"))?;
    std::fs::write(marker, b"1")
        .map_err(|e| unavailable(format!("the marker was not written: {e}")))
}

/// The proven keys, or a refusal that says why there are none.
fn proven_keys() -> Result<Vec<Approver>> {
    let registry = load()?;
    if registry.state == "unproven" {
        return Err(Refusal::new(
            "approver_registry_unproven",
            format!(
                "the owner's approver keys cannot be proven yet; enter your {p} PIN once after {p} starts, or enroll the key again",
                p = product()
            ),
        ));
    }
    Ok(registry.keys)
}

/// `remote.owner.key.status`: the registry's state and its enrolled keys. `enrolled` and `state`
/// are the chat-unlock registry's alone; the owner-check keys are listed in `keys` too (so a
/// caller can tell whether its own key is enrolled) and summarised in `ownerCheck`. A damaged or
/// unreadable owner-check file never fails this call: it reads as `unproven` / `unavailable`.
pub fn status() -> Result<Value> {
    let registry = load()?;
    let check = read_owner_check().unwrap_or(OwnerCheck {
        keys: Vec::new(),
        state: "unavailable",
    });
    Ok(json!({
        "enrolled": !registry.keys.is_empty(),
        "state": registry.state,
        "ownerCheck": {
            "enrolled": !check.keys.is_empty(),
            "state": check.state,
            "protection": owner_check_protection(),
            "weaker": owner_check_protection() == "file",
        },
        "keys": registry.keys.iter().chain(check.keys.iter()).map(|k| json!({"keyId": k.key_id, "device": k.device, "dev": k.dev, "bound": check_key_is_bound(k)})).collect::<Vec<_>>(),
    }))
}

// ---- verification ------------------------------------------------------------------------------

static NONCES: Mutex<Vec<(String, u64)>> = Mutex::new(Vec::new());

fn invalid() -> Refusal {
    Refusal::new(
        "approval_signature_invalid",
        "the owner's signature does not match this request",
    )
}

/// The bytes the owner signs (see the module header).
#[allow(clippy::too_many_arguments)]
pub fn message(
    kind: &str,
    request: &str,
    action: &str,
    client: &str,
    device: &str,
    key_id: &str,
    nonce: &str,
    issued: u64,
    expires: u64,
) -> Vec<u8> {
    format!(
        "pulse.approval.v1\nkind:{kind}\nrequest:{request}\naction:{action}\nclient:{client}\ndevice:{device}\nkey:{key_id}\nnonce:{nonce}\nissued:{issued}\nexpires:{expires}"
    )
    .into_bytes()
}

/// The `action` an `enroll` signature commits to: SHA-256 hex of the public key.
pub fn enroll_action(public: &[u8]) -> String {
    hex(&rightkit_crypto::sha256(public))
}

/// A fresh signature nonce: 32 hex characters from the system RNG.
pub fn fresh_nonce() -> Option<String> {
    use ring::rand::SecureRandom;
    let mut raw = [0u8; 16];
    ring::rand::SystemRandom::new().fill(&mut raw).ok()?;
    Some(hex(&raw))
}

/// Device label of an enrolled Windows Hello key.
pub const HELLO_DEVICE: &str = "windows-hello";

fn is_p256(public: &[u8]) -> bool {
    public.len() == 65 && public[0] == 4
}

/// A 2048-bit RSA key with exponent 65537 as a PKCS#1 `RSAPublicKey` (what Windows Hello's
/// `RetrievePublicKey(Pkcs1RsaPublicKey)` returns); nothing else is taken as an RSA approver.
fn is_hello_rsa(public: &[u8]) -> bool {
    public.len() == 270
        && public.starts_with(&[0x30, 0x82, 0x01, 0x0a, 0x02, 0x82, 0x01, 0x01, 0x00])
        && public[9] & 0x80 != 0
        && public.ends_with(&[0x02, 0x03, 0x01, 0x00, 0x01])
}

fn check_signature(public: &[u8], msg: &[u8], signature: &[u8]) -> bool {
    if is_hello_rsa(public) {
        return signature.len() == 256
            && UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, public)
                .verify(msg, signature)
                .is_ok();
    }
    if !is_p256(public) {
        return false;
    }
    let algorithm = if signature.len() == 64 {
        &ECDSA_P256_SHA256_FIXED
    } else {
        &ECDSA_P256_SHA256_ASN1
    };
    UnparsedPublicKey::new(algorithm, public)
        .verify(msg, signature)
        .is_ok()
}

/// Window, nonce shape and signature of one envelope; a good signature consumes its nonce.
#[allow(clippy::too_many_arguments)]
fn verify_with(
    public: &[u8],
    device: &str,
    kind: &str,
    request: &str,
    action: &str,
    client: &str,
    signed: &Signed,
) -> Result<()> {
    let at = now();
    if signed.expires <= at || signed.issued > at + SKEW || signed.expires < signed.issued {
        return Err(Refusal::new(
            "approval_expired",
            "the owner's signed approval is expired or not yet valid; approve again",
        ));
    }
    if signed.expires - signed.issued > MAX_LIFETIME {
        return Err(invalid());
    }
    let nonce_ok = (32..=64).contains(&signed.nonce.len())
        && signed.nonce.chars().all(|c| c.is_ascii_hexdigit());
    let field_ok = !request.contains('\n') && !action.contains('\n') && !client.contains('\n');
    if !nonce_ok || !field_ok {
        return Err(invalid());
    }
    let msg = message(
        kind,
        request,
        action,
        client,
        device,
        &signed.key_id,
        &signed.nonce,
        signed.issued,
        signed.expires,
    );
    if !check_signature(public, &msg, &signed.signature) {
        return Err(invalid());
    }
    let mut seen = NONCES.lock().unwrap_or_else(|e| e.into_inner());
    seen.retain(|(_, until)| *until > at);
    let nonce = signed.nonce.to_ascii_lowercase();
    if seen.iter().any(|(n, _)| *n == nonce) {
        return Err(Refusal::new(
            "approval_replayed",
            "this signed approval was already used",
        ));
    }
    if seen.len() >= MAX_NONCES {
        return Err(Refusal::new(
            "approval_busy",
            "too many signed approvals in a short time; try again shortly",
        ));
    }
    seen.push((nonce, signed.expires));
    Ok(())
}

/// Verifies that the enrolled key `signed.key_id` signed this request: `kind`, the engine's own
/// request id, its digest (`approvals::request_digest`) and client key. Unknown key, a dev key, a bad
/// or replayed or expired signature are all refusals.
pub fn verify(
    kind: &str,
    request: &str,
    action: &str,
    client: &str,
    signed: &Signed,
) -> Result<()> {
    let keys = proven_keys()?;
    let Some(key) = keys.iter().find(|k| k.key_id == signed.key_id) else {
        return Err(Refusal::new(
            "approver_unknown",
            "no enrolled owner key made this signature; enroll the owner device's key first",
        ));
    };
    if key.dev {
        return Err(Refusal::new(
            "approver_dev_key",
            "a development approver key is not trusted by this build",
        ));
    }
    verify_with(
        &key.public,
        &key.device,
        kind,
        request,
        action,
        client,
        signed,
    )
}

/// A signed approval as the core's [`rightkit_approvals::Proof`]: the enrolled key
/// `signed.key_id` signed kind `approve` for the record's own id and client and for `digest`,
/// the display digest the owner's device was shown. The core's decision rule then refuses the
/// decision unless `digest` is the waiting request's own display digest.
pub struct SignedApproval<'a> {
    pub signed: &'a Signed,
    pub digest: &'a str,
}

impl rightkit_approvals::Proof for SignedApproval<'_> {
    fn verify(&self, record: &rightkit_approvals::Record) -> Result<()> {
        let request = &record.request;
        verify(
            "approve",
            &request.id,
            self.digest,
            &request.origin.client,
            self.signed,
        )
    }
}

// ---- enrolment and revocation (owner path only) ------------------------------------------------

/// Who may change the registry (enrol or revoke), first key included: the host app PIN, verified
/// inside the engine, or a signature from an already proven enrolled key. There is no
/// trust-on-first-use: a caller that merely claims to be owner app (or writes the credential item
/// itself) cannot enrol a key, because it cannot produce the PIN or a signature of a proven key.
/// The owner app's own click is not an owner proof here (that is the forgeable word this slice
/// removes). A Mac with no PIN set has no enrolled key and unlocks by nothing but the PIN, which
/// is unset too (every gate is off then). The PIN check also opens the seal key for the write.
fn authorise(keys: &[Approver], params: &Value, kind: &str, action: &str) -> Result<()> {
    if let Some(pin) = params["pin"].as_str().filter(|p| !p.is_empty()) {
        return crate::hooks().confirm_pin(pin);
    }
    let needs = || {
        Refusal::new(
            "unlock_needs_auth",
            format!(
                "changing the owner's approver keys needs the {p} PIN (or a signature from an enrolled key); set a PIN first if none is set",
                p = product()
            ),
        )
    };
    if keys.is_empty() {
        return Err(needs());
    }
    let auth = Signed::from_value(&params["auth"]).map_err(|_| needs())?;
    let Some(key) = keys.iter().find(|k| k.key_id == auth.key_id) else {
        return Err(Refusal::new("approver_unknown", "that key is not enrolled"));
    };
    if key.dev {
        return Err(Refusal::new(
            "approver_dev_key",
            "a development approver key is not trusted by this build",
        ));
    }
    verify_with(&key.public, &key.device, kind, kind, action, "", &auth)
}

/// `remote.owner.key.register` with `publicKey` (hex, 65 bytes), `device`, `proof` (a signature
/// of the new key over kind `enroll`, which proves possession), `dev` (default false) and, when
/// `pin` (the host app PIN, always; or `auth`, a signature of a proven enrolled key). Owner-only; `from_app` is the engine's verified
/// caller label. Engine only.
pub fn register(params: &Value, from_app: bool) -> Result<Value> {
    if !from_app {
        return Err(Refusal::new(
            "unlock_needs_auth",
            format!(
                "owner keys are enrolled from the {p} app on this computer, never remotely",
                p = product()
            ),
        ));
    }
    let _gate = REGISTRY_GATE.lock().unwrap_or_else(|e| e.into_inner());
    let public = params["publicKey"]
        .as_str()
        .and_then(unhex)
        .filter(|p| is_p256(p))
        .ok_or_else(|| {
            Refusal::new(
                "invalid_request",
                "publicKey is a 65 byte P-256 point in hex",
            )
        })?;
    let device = params["device"].as_str().unwrap_or("");
    if !plain(device, 40) {
        return Err(Refusal::new(
            "invalid_request",
            "device is a short id: letters, digits, '.', '_', '-', ':'",
        ));
    }
    let dev = params["dev"].as_bool().unwrap_or(false);
    if dev {
        return Err(Refusal::new(
            "approver_dev_key",
            "a development approver key cannot be enrolled in this build",
        ));
    }
    let key_id = key_id_of(&public);
    let action = enroll_action(&public);
    let mut keys = load()?.keys;
    authorise(&keys, params, "enroll", &action)?;
    let proof = Signed::from_value(&params["proof"])?;
    if proof.key_id != key_id {
        return Err(invalid());
    }
    verify_with(&public, device, "enroll", "enroll", &action, "", &proof)?;
    keys.retain(|k| k.key_id != key_id);
    if keys.len() >= MAX_KEYS {
        return Err(Refusal::new(
            "approver_limit",
            "four owner keys are enrolled; revoke one first",
        ));
    }
    keys.push(Approver {
        key_id: key_id.clone(),
        public,
        device: device.to_string(),
        dev,
    });
    save(&keys)?;
    Ok(json!({"keyId": key_id, "device": device}))
}

/// The device ids of the owner-check keys: the Mac owner app's separate Secure Enclave key (access
/// control `.userPresence`: Touch ID or the login password) and the Windows Hello key.
pub const OWNER_DEVICE: &str = "mac-owner-check";

/// The owner-check public keys, kept OUTSIDE the PIN-sealed registry in `owner-check-keys.json` in
/// the engine's private directory (0600): `{"keys":[{"device":"mac-owner-check"|"windows-hello",
/// "alg":"p256"|"rsa2048","publicKey":"<hex>","enrolledMs":n}],"mac":"<hex>"}`, one key per device.
/// They are public keys, so the need is integrity, not secrecy, and same-user code can write the
/// file. So the file carries an HMAC-SHA256 over its keys, made with a 32 byte secret that lives in
/// the OS credential store (`rightkit-credentials`, service `pulse.owner-check-file`): on macOS the
/// login-keychain item is pinned to the engine's team partition (`protected_guard`), so another
/// process of the user cannot read the secret and cannot make a file that verifies. On Windows
/// the credential store is DPAPI, which any process of the same user can read: there the MAC only
/// catches accidental damage and writers that do not know to ask for the secret, so Windows
/// same-user protection is weaker (the enrolment policy below still holds). A file whose MAC
/// does not verify, or whose secret is gone, is refused: no key in it counts. The file is read
/// fresh for every check, so no PIN is needed after a restart. The sealed registry (chat unlock,
/// recovery, approvals) is unchanged and never read for the owner check.
///
/// Enrolment policy: the FIRST key is enrolled with only its possession proof (from owner app or
/// hub of this computer). After that, adding or replacing a key needs the host app PIN or a signature
/// by an already enrolled owner-check key over the new key (`authorise`); so does revoking one.
const OWNER_CHECK_FILE: &str = "owner-check-keys.json";
const CHECK_SECRET_SERVICE: &str = "pulse.owner-check-file";
const CHECK_SECRET_ACCOUNT: &str = "mac-key-v1";

fn owner_check_path() -> Result<std::path::PathBuf> {
    rightkit_app_host::state_directory()
        .map(|dir| dir.join(OWNER_CHECK_FILE))
        .map_err(|e| unavailable(e.to_string()))
}

/// The owner-check file as read. `state`: `fresh` (no file, no secret: the first key may enrol),
/// `ok` (file and MAC verify), `deleted` (the secret exists but the file is gone) or `unproven`
/// (the file is damaged, group- or world-writable, has no valid MAC, or its secret is missing).
/// Only `ok` keys count.
struct OwnerCheck {
    keys: Vec<Approver>,
    state: &'static str,
}

fn read_check_secret() -> Result<Option<Vec<u8>>> {
    let text = match crate::protected_guard::read_item(CHECK_SECRET_SERVICE, CHECK_SECRET_ACCOUNT) {
        Some(read) => read.map_err(unavailable)?,
        None => match store_for(CHECK_SECRET_SERVICE)?.get(CHECK_SECRET_ACCOUNT) {
            Ok(Some(secret)) => Some(
                secret
                    .expose_str()
                    .map_err(|e| unavailable(e.to_string()))?
                    .to_owned(),
            ),
            Ok(None) => None,
            Err(e) => return Err(unavailable(e.to_string())),
        },
    };
    text.map(|t| {
        unhex(t.trim())
            .filter(|k| k.len() == 32)
            .ok_or_else(|| unavailable("the owner-check seal key is damaged"))
    })
    .transpose()
}

fn create_check_secret() -> Result<Vec<u8>> {
    use ring::rand::SecureRandom;
    let mut raw = [0u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut raw)
        .map_err(|_| unavailable("the system could not make a random key"))?;
    store_for(CHECK_SECRET_SERVICE)?
        .set_str(CHECK_SECRET_ACCOUNT, &hex(&raw))
        .map_err(|e| unavailable(e.to_string()))?;
    crate::protected_guard::check_item(CHECK_SECRET_SERVICE, CHECK_SECRET_ACCOUNT)
        .map_err(unavailable)?;
    Ok(raw.to_vec())
}

/// One key line of the file, before it is trusted.
struct CheckLine {
    device: String,
    alg: String,
    public: String,
    enrolled_ms: u64,
}

/// The bytes the MAC covers: a version line, then every key in device order.
fn check_text(lines: &[CheckLine]) -> String {
    let mut sorted: Vec<&CheckLine> = lines.iter().collect();
    sorted.sort_by(|a, b| (&a.device, &a.public).cmp(&(&b.device, &b.public)));
    let mut out = String::from("pulse.owner-check.v1\n");
    for l in sorted {
        out.push_str(&format!(
            "{}|{}|{}|{}\n",
            l.device, l.alg, l.public, l.enrolled_ms
        ));
    }
    out
}

fn check_mac(secret: &[u8], text: &str) -> String {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret);
    hex(ring::hmac::sign(&key, text.as_bytes()).as_ref())
}

fn check_mac_ok(secret: &[u8], text: &str, mac: &str) -> bool {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret);
    unhex(mac).is_some_and(|m| ring::hmac::verify(&key, text.as_bytes(), &m).is_ok())
}

/// The owner-check keys the elevated Windows service holds (`devicelink::admin_win::owner_keys`),
/// or `None` when it is not installed and the sealed file below is the store. With the service
/// installed it is the only store and any failure to reach it refuses the check.
#[cfg(windows)]
fn service_owner_keys() -> Option<Result<Vec<Approver>>> {
    let service = crate::owner_key_service()?;
    let rows = match service.owner_keys() {
        Ok(Some(rows)) => rows,
        Ok(None) => return None,
        Err(message) => return Some(Err(unavailable(message))),
    };
    let keys = rows
        .into_iter()
        .map(|(id, device, public)| {
            let public = unhex(&public)?;
            let shape_ok = match device.as_str() {
                OWNER_DEVICE => is_p256(&public),
                HELLO_DEVICE => is_hello_rsa(&public),
                _ => false,
            };
            (shape_ok && key_id_of(&public) == id).then_some(Approver {
                key_id: id,
                public,
                device,
                dev: false,
            })
        })
        .collect::<Option<Vec<_>>>();
    Some(keys.ok_or_else(|| unavailable("the service's owner-check key list is damaged")))
}

/// Where the owner-check keys are protected: `service` (the elevated Windows service, a folder only
/// administrators can write), `file` (Windows without the service: the engine's own sealed file,
/// whose seal key DPAPI gives to any process of the same user: WEAKER), `keychain` (the Mac login
/// keychain, pinned to the engine).
fn owner_check_protection() -> &'static str {
    #[cfg(windows)]
    {
        if crate::owner_key_service().is_some_and(|service| service.service_installed()) {
            "service"
        } else {
            "file"
        }
    }
    #[cfg(not(windows))]
    {
        "keychain"
    }
}

/// Reads the owner-check keys. A credential store that cannot be read is an error; every problem
/// with the file itself reads as `unproven` with no keys (fail closed).
fn read_owner_check() -> Result<OwnerCheck> {
    #[cfg(windows)]
    if let Some(keys) = service_owner_keys() {
        return Ok(OwnerCheck {
            keys: keys?,
            state: "ok",
        });
    }
    let path = owner_check_path()?;
    let secret = read_check_secret()?;
    let unproven = OwnerCheck {
        keys: Vec::new(),
        state: "unproven",
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(OwnerCheck {
                keys: Vec::new(),
                state: if secret.is_some() { "deleted" } else { "fresh" },
            });
        }
        Err(_) => return Ok(unproven),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path)
            .map_err(|e| unavailable(e.to_string()))?
            .permissions()
            .mode();
        if mode & 0o022 != 0 {
            return Ok(unproven);
        }
    }
    let Some(secret) = secret else {
        return Ok(unproven);
    };
    let parse = || -> Option<(Vec<CheckLine>, String)> {
        let value: Value = serde_json::from_str(&text).ok()?;
        let lines = value["keys"]
            .as_array()?
            .iter()
            .map(|v| {
                Some(CheckLine {
                    device: v["device"].as_str()?.to_string(),
                    alg: v["alg"].as_str()?.to_string(),
                    public: v["publicKey"].as_str()?.to_string(),
                    enrolled_ms: v["enrolledMs"].as_u64()?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some((lines, value["mac"].as_str()?.to_string()))
    };
    let Some((lines, mac)) = parse() else {
        return Ok(unproven);
    };
    if !check_mac_ok(&secret, &check_text(&lines), &mac) {
        return Ok(unproven);
    }
    let keys = lines
        .iter()
        .map(|l| {
            let public = unhex(&l.public)?;
            let shape_ok = match (l.device.as_str(), l.alg.as_str()) {
                (OWNER_DEVICE, "p256") => is_p256(&public),
                (HELLO_DEVICE, "rsa2048") => is_hello_rsa(&public),
                _ => false,
            };
            shape_ok.then(|| Approver {
                key_id: key_id_of(&public),
                public,
                device: l.device.clone(),
                dev: false,
            })
        })
        .collect::<Option<Vec<_>>>();
    Ok(match keys {
        Some(keys) => OwnerCheck { keys, state: "ok" },
        None => unproven,
    })
}

/// The proven keys, or a refusal that says why there are none (verification fails closed).
fn read_owner_check_keys() -> Result<Vec<Approver>> {
    let check = read_owner_check()?;
    if check.state == "unproven" {
        return Err(unavailable(format!(
            "the owner-check key file is damaged, changed or unsealed; enrol the key again with the {p} PIN",
            p = product()
        )));
    }
    Ok(check.keys)
}

/// Windows with the service: the service's list becomes `wanted`. Each change is one elevated run
/// of the service program (one UAC consent on the secure desktop, so same-user code cannot make
/// it); then the service's list is read back until it matches.
#[cfg(windows)]
fn write_via_service(current: Vec<Approver>, wanted: &[Approver]) -> Result<()> {
    let service = crate::owner_key_service()
        .ok_or_else(|| unavailable("the elevated owner-key service is not available"))?;
    let owner_keys_change = |args: &str| service.change_owner_keys(args);
    let same = |a: &[Approver], b: &[Approver]| {
        a.len() == b.len() && a.iter().all(|k| b.iter().any(|o| o.key_id == k.key_id))
    };
    if same(&current, wanted) {
        return Ok(());
    }
    if wanted.is_empty() {
        owner_keys_change("owner-revoke --key all").map_err(unavailable)?;
    } else {
        for old in current
            .iter()
            .filter(|k| !wanted.iter().any(|w| w.device == k.device))
        {
            owner_keys_change(&format!("owner-revoke --key {}", old.key_id))
                .map_err(unavailable)?;
        }
        for new in wanted
            .iter()
            .filter(|w| !current.iter().any(|k| k.key_id == w.key_id))
        {
            owner_keys_change(&format!(
                "owner-enrol --device {} --key {}",
                new.device,
                hex(&new.public)
            ))
            .map_err(unavailable)?;
        }
    }
    for _ in 0..240 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if let Some(Ok(now)) = service_owner_keys()
            && same(&now, wanted)
        {
            return Ok(());
        }
    }
    Err(unavailable(
        "the elevated service did not record the owner-check key (the Windows confirmation was declined or timed out)",
    ))
}

/// Writes the keys with their MAC (making the secret first when the store has none). With the
/// elevated Windows service installed the service holds them instead (`write_via_service`).
fn write_owner_check_keys(keys: &[Approver]) -> Result<()> {
    #[cfg(windows)]
    if let Some(current) = service_owner_keys() {
        return write_via_service(current?, keys);
    }
    let path = owner_check_path()?;
    let secret = match read_check_secret()? {
        Some(secret) => secret,
        None => create_check_secret()?,
    };
    let enrolled_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let lines: Vec<CheckLine> = keys
        .iter()
        .map(|k| CheckLine {
            device: k.device.clone(),
            alg: if is_hello_rsa(&k.public) {
                "rsa2048"
            } else {
                "p256"
            }
            .to_string(),
            public: hex(&k.public),
            enrolled_ms,
        })
        .collect();
    let mac = check_mac(&secret, &check_text(&lines));
    let list: Vec<Value> = lines
        .iter()
        .map(|l| {
            json!({"device": l.device, "alg": l.alg, "publicKey": l.public, "enrolledMs": l.enrolled_ms})
        })
        .collect();
    let bytes = json!({"keys": list, "mac": mac}).to_string().into_bytes();
    let temp = path.with_extension("json.tmp");
    let write = || -> std::io::Result<()> {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, &path)
    };
    write().map_err(|e| unavailable(format!("the owner-check key file was not written: {e}")))
}

/// `remote.owner.key.enrol.local` with `publicKey` (hex), `device` and `proof` (a signature of the
/// new key over kind `enroll`, which proves possession): enrols the owner-check key of `device`
/// into `owner-check-keys.json` (not the sealed registry). `mac-owner-check` is a 65 byte P-256
/// key from owner app; `windows-hello` a 270 byte RSA-2048 key from owner app or the hub. Trust:
/// the engine verified the caller's executable (`from_app`, `from_companion`; a process that only
/// claims it is refused) and the key is proven by a possession signature. The FIRST key (no file
/// yet, or an empty one the engine itself sealed) needs nothing more. Once a key is enrolled,
/// adding or replacing one needs the host app PIN (`pin`) or `auth`, a signature (kind `enroll`
/// over the new key) by an already enrolled owner-check key; a file that is unproven or whose
/// secret survives without it (`deleted`) needs the PIN. Enrolling a key that is already enrolled is
/// a no-op. Keys of this file sign only kind `device` over the engine's single-use challenge,
/// which the engine still verifies. One key per device, replacing the earlier one; the
/// chat-unlock key and the sealed registry are untouched. Engine only.
pub fn enrol_owner_check_key(
    params: &Value,
    from_app: bool,
    from_companion: bool,
) -> Result<Value> {
    if !from_app && !from_companion {
        return Err(Refusal::new(
            "unlock_needs_auth",
            format!(
                "the owner-check key is enrolled by the {p} app on this computer, never remotely",
                p = product()
            ),
        ));
    }
    let _gate = REGISTRY_GATE.lock().unwrap_or_else(|e| e.into_inner());
    let public = params["publicKey"]
        .as_str()
        .and_then(unhex)
        .ok_or_else(|| Refusal::new("invalid_request", "publicKey is hex"))?;
    let device = params["device"].as_str().unwrap_or("");
    let allowed = match device {
        OWNER_DEVICE => from_app && is_p256(&public),
        HELLO_DEVICE => is_hello_rsa(&public),
        _ => false,
    };
    if !allowed {
        return Err(Refusal::new(
            "invalid_request",
            "an owner-check key is a P-256 key of `mac-owner-check` (the owner device) or an RSA-2048 key of `windows-hello`",
        ));
    }
    let key_id = key_id_of(&public);
    let action = enroll_action(&public);
    let proof = Signed::from_value(&params["proof"])?;
    if proof.key_id != key_id {
        return Err(invalid());
    }
    verify_with(&public, device, "enroll", "enroll", &action, "", &proof)?;
    let state = read_owner_check()?;
    if state
        .keys
        .iter()
        .any(|k| k.key_id == key_id && k.device == device)
    {
        store_binding_param(params, device, &public)?;
        return Ok(json!({"keyId": key_id, "device": device, "enrolled": false}));
    }
    let open_to_first = state.state == "fresh" || (state.state == "ok" && state.keys.is_empty());
    if !open_to_first {
        // `authorise` takes the PIN, or a signature by one of the keys already enrolled (none
        // when the file is `deleted` or `unproven`, so only the PIN then).
        authorise(&state.keys, params, "enroll", &action)?;
    }
    let mut keys = state.keys;
    keys.retain(|k| k.device != device);
    keys.push(Approver {
        key_id: key_id.clone(),
        public: public.clone(),
        device: device.to_string(),
        dev: false,
    });
    write_owner_check_keys(&keys)?;
    store_binding_param(params, device, &public)?;
    Ok(json!({"keyId": key_id, "device": device, "enrolled": true}))
}

/// `remote.owner.key.check.revoke` with `keyId` and `pin` or `auth` (a signature, kind `revoke`,
/// action the key id, by an enrolled owner-check key): removes one owner-check key. `keyId` `all`
/// (the PIN only) empties and reseals the file, which also repairs an unproven one. The owner app or
/// hub of this computer only. Engine only.
pub fn revoke_owner_check_key(
    params: &Value,
    from_app: bool,
    from_companion: bool,
) -> Result<Value> {
    if !from_app && !from_companion {
        return Err(Refusal::new(
            "unlock_needs_auth",
            format!(
                "owner-check keys are revoked from the {p} app on this computer, never remotely",
                p = product()
            ),
        ));
    }
    let _gate = REGISTRY_GATE.lock().unwrap_or_else(|e| e.into_inner());
    let id = params["keyId"].as_str().unwrap_or("");
    let state = read_owner_check()?;
    if id == "all" {
        authorise(&[], params, "revoke", id)?;
        write_owner_check_keys(&[])?;
        void_unlocks()?;
        return Ok(json!({"revoked": true}));
    }
    if !state.keys.iter().any(|k| k.key_id == id) {
        return Ok(json!({"revoked": false}));
    }
    authorise(&state.keys, params, "revoke", id)?;
    let keys: Vec<Approver> = state.keys.into_iter().filter(|k| k.key_id != id).collect();
    write_owner_check_keys(&keys)?;
    void_unlocks()?;
    Ok(json!({"revoked": true}))
}

/// Revoking an owner-check key voids every unlock: the same path as revoke-all (memory grants,
/// then the token generation moves on and every stored token is deleted; a failure is reported).
fn void_unlocks() -> Result<()> {
    crate::hooks().void_unlocks()
}

/// The proof an owner-check signature leaves, for the circle's admission: everything a third
/// device needs to verify the signature later with only the public key
/// ([`check_owner_proof`]); `signed.signature` stays private to this module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerCheckProof {
    /// `mac-owner-check` or `windows-hello`.
    pub device: String,
    /// The signing key's public key, hex.
    pub public_key: String,
    pub challenge: String,
    pub client: String,
    pub key_id: String,
    pub nonce: String,
    pub issued: u64,
    pub expires: u64,
    pub signature: String,
    /// The key's binding to this computer's device, when it has one (`KeyBinding`).
    pub binding: Option<KeyBinding>,
}

/// The `action` an owner-check endorsement signature commits to: SHA-256 hex over the domain
/// `pulse-circle-endorse-v1`, the number of bodies and each body's canonical `line`
/// (`devicelink::circle::EndorsementBody::line`), in order. One signature, one prompt, covers the
/// whole batch; each body verifies against it, and no body can be dropped, added or changed.
pub fn endorse_action(lines: &[String]) -> String {
    hex(&rightkit_crypto::sha256(
        format!(
            "pulse-circle-endorse-v1\ncount:{}\n{}",
            lines.len(),
            lines.concat()
        )
        .as_bytes(),
    ))
}

/// Checks an owner-check signature kept as a proof: the public key is a well formed owner-check
/// key of its device, its id matches, and the signature covers kind `device`, the request, the
/// `action` (the caller derives it from what it expects, e.g. [`endorse_action`]), the client and the
/// proof's own envelope. The window and nonce were checked by the engine that took the signature
/// live; they are not checked again here, so this proves who signed what, not when it was fresh.
pub fn check_owner_proof(proof: &OwnerCheckProof, action: &str) -> Result<()> {
    let public = unhex(&proof.public_key).ok_or_else(invalid)?;
    let shape_ok = match proof.device.as_str() {
        OWNER_DEVICE => is_p256(&public),
        HELLO_DEVICE => is_hello_rsa(&public),
        _ => false,
    };
    let nonce_ok = (32..=64).contains(&proof.nonce.len())
        && proof.nonce.chars().all(|c| c.is_ascii_hexdigit());
    let field_ok = ![proof.challenge.as_str(), action, proof.client.as_str()]
        .iter()
        .any(|f| f.contains('\n'));
    let signature = unhex(&proof.signature).ok_or_else(invalid)?;
    if !shape_ok
        || !nonce_ok
        || !field_ok
        || proof.expires < proof.issued
        || proof.expires - proof.issued > MAX_LIFETIME
        || key_id_of(&public) != proof.key_id
    {
        return Err(invalid());
    }
    let msg = message(
        "device",
        &proof.challenge,
        action,
        &proof.client,
        &proof.device,
        &proof.key_id,
        &proof.nonce,
        proof.issued,
        proof.expires,
    );
    if check_signature(&public, &msg, &signature) {
        Ok(())
    } else {
        Err(invalid())
    }
}

/// This computer's own owner-check public key for the circle: `(device, public key hex, binding)`,
/// or `None` when none is enrolled or the file is not proven. The binding is the key's proof that
/// it belongs to this computer's device (`KeyBinding`); it is `None` for a key enrolled before
/// bindings existed, which then counts as unbound everywhere until the next owner check makes one.
pub fn own_owner_check_key() -> Option<(String, String, Option<KeyBinding>)> {
    let check = read_owner_check().ok()?;
    if check.state != "ok" {
        return None;
    }
    check
        .keys
        .iter()
        .find(|k| k.device == OWNER_DEVICE)
        .or_else(|| check.keys.first())
        .map(|k| {
            let public = hex(&k.public);
            let binding = stored_binding(&k.device, &public);
            (k.device.clone(), public, binding)
        })
}

/// [`own_owner_check_key`] in the engine process only (a joiner tells the host the owner-check key
/// its owner approves with; any other process, such as a test or the CLI, reads no key file).
pub fn engine_owner_check_key() -> Option<(String, String, Option<KeyBinding>)> {
    if !crate::hooks().is_engine_process() {
        return None;
    }
    own_owner_check_key()
}

// ---- key bindings ------------------------------------------------------------------------------

const BIND_DOMAIN: &str = "pulse-check-bind-v1";
const BINDINGS_FILE: &str = "owner-check-bindings.json";

/// Reads `{deviceId, deviceKey, nonce, issued, expires, signature}` from `value`.
fn key_binding_from_value(value: &Value) -> Result<KeyBinding> {
    let bad = || Refusal::new("invalid_request", "the key binding is malformed");
    let text = |k: &str| value[k].as_str().map(str::to_string).ok_or_else(bad);
    let number = |k: &str| value[k].as_u64().ok_or_else(bad);
    Ok(KeyBinding {
        device_id: text("deviceId")?,
        device_key: text("deviceKey")?,
        nonce: text("nonce")?,
        issued: number("issued")?,
        expires: number("expires")?,
        signature: text("signature")?,
    })
}

/// The `action` a binding signature commits to: SHA-256 hex over `pulse-check-bind-v1`, the device
/// id, the device's identity key and the check key (`device:public`).
pub fn bind_action(
    device_id: &str,
    device_key: &str,
    check_device: &str,
    check_public: &str,
) -> String {
    hex(&rightkit_crypto::sha256(
        format!(
            "{BIND_DOMAIN}\ndevice:{device_id}\nkey:{device_key}\ncheck:{check_device}:{check_public}\n"
        )
        .as_bytes(),
    ))
}

/// The hub offers the owner-key confirmation on its own at most once per this long (a day), unless
/// the owner clicks "Confirm owner key" (which does not depend on the offer).
const BIND_OFFER_EVERY_MS: u64 = 24 * 60 * 60 * 1000;
const BIND_OFFER_FILE: &str = "owner-key-bind-offer";

/// Adds `bindDue` to `status` (`remote.owner.key.status`). Only a caller that sends `offer: true`
/// (the hub page that would prompt) can consume an offer: `bindDue` is true when this computer's
/// owner key is unbound and the last offer is more than a day old (or never made). The time is stamped here, in the engine's store, so a
/// recreated web view, a dismissed prompt or a restart asks again only the next day.
pub fn add_bind_offer(status: &mut Value, offer: bool) {
    let needed = status["keys"].as_array().is_some_and(|keys| {
        keys.iter().any(|k| {
            matches!(k["device"].as_str(), Some(OWNER_DEVICE | HELLO_DEVICE)) && k["bound"] == false
        })
    });
    let mut due = false;
    if offer && needed {
        let at = now().saturating_mul(1000);
        if let Ok(path) = rightkit_app_host::state_directory().map(|d| d.join(BIND_OFFER_FILE)) {
            let last = std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| t.trim().parse::<u64>().ok());
            if last.is_none_or(|l| at >= l.saturating_add(BIND_OFFER_EVERY_MS) || l > at) {
                due = std::fs::write(&path, at.to_string()).is_ok();
            }
        }
    }
    status["bindDue"] = Value::Bool(due);
}

/// Whether `binding` is the check key's own signature binding it to the device it names: a well
/// formed owner-check key, the signature over [`bind_action`]. The caller still compares
/// `binding.device_id` and `binding.device_key` with the device the key is attached to. The window
/// is not checked (a binding does not expire; it only proves which device the key belongs to).
pub fn check_key_binding(
    check_device: &str,
    check_public: &str,
    binding: &KeyBinding,
) -> Result<()> {
    let public = unhex(check_public).ok_or_else(invalid)?;
    let shape_ok = match check_device {
        OWNER_DEVICE => is_p256(&public),
        HELLO_DEVICE => is_hello_rsa(&public),
        _ => false,
    };
    let nonce_ok = (32..=64).contains(&binding.nonce.len())
        && binding.nonce.chars().all(|c| c.is_ascii_hexdigit());
    let ids_ok = binding.device_id.len() <= 64
        && binding
            .device_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && binding.device_key.len() == 64
        && binding.device_key.bytes().all(|b| b.is_ascii_hexdigit());
    let signature = unhex(&binding.signature).ok_or_else(invalid)?;
    if !shape_ok
        || !nonce_ok
        || !ids_ok
        || binding.expires < binding.issued
        || binding.expires - binding.issued > MAX_LIFETIME
    {
        return Err(invalid());
    }
    let key_id = key_id_of(&public);
    let action = bind_action(
        &binding.device_id,
        &binding.device_key,
        check_device,
        check_public,
    );
    let msg = message(
        "bind",
        "bind",
        &action,
        "",
        check_device,
        &key_id,
        &binding.nonce,
        binding.issued,
        binding.expires,
    );
    if check_signature(&public, &msg, &signature) {
        Ok(())
    } else {
        Err(invalid())
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredBinding {
    device: String,
    public_key: String,
    binding: KeyBinding,
}

fn bindings_path() -> Result<std::path::PathBuf> {
    rightkit_app_host::state_directory()
        .map(|dir| dir.join(BINDINGS_FILE))
        .map_err(|e| unavailable(e.to_string()))
}

fn read_bindings() -> Vec<StoredBinding> {
    bindings_path()
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// The stored binding of this computer's owner-check key `device` / `public_key`, only when it
/// verifies. The file holds no secret and needs no seal: a binding is self-authenticating, and
/// deleting or damaging it only leaves the key unbound.
pub fn stored_binding(device: &str, public_key: &str) -> Option<KeyBinding> {
    read_bindings()
        .into_iter()
        .find(|s| s.device == device && s.public_key == public_key)
        .map(|s| s.binding)
        .filter(|b| check_key_binding(device, public_key, b).is_ok())
}

fn check_key_is_bound(key: &Approver) -> bool {
    matches!(key.device.as_str(), OWNER_DEVICE | HELLO_DEVICE)
        && stored_binding(&key.device, &hex(&key.public)).is_some()
}

fn write_binding(device: &str, public_key: &str, binding: &KeyBinding) -> Result<()> {
    let path = bindings_path()?;
    let mut all = read_bindings();
    all.retain(|s| s.device != device);
    all.push(StoredBinding {
        device: device.to_string(),
        public_key: public_key.to_string(),
        binding: binding.clone(),
    });
    let bytes = serde_json::to_vec(&all).map_err(|e| unavailable(e.to_string()))?;
    let temp = path.with_extension("json.tmp");
    let write = || -> std::io::Result<()> {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, &path)
    };
    write().map_err(|e| unavailable(format!("the owner-check binding was not written: {e}")))
}

/// Keeps the `binding` that came with an enrolment or a bind call, once it verifies for `device`
/// and `public`. No `binding` param is a no-op (the key stays unbound).
fn store_binding_param(params: &Value, device: &str, public: &[u8]) -> Result<()> {
    if params["binding"].is_null() {
        return Ok(());
    }
    let binding = key_binding_from_value(&params["binding"])?;
    let public_hex = hex(public);
    check_key_binding(device, &public_hex, &binding)?;
    write_binding(device, &public_hex, &binding)
}

/// `remote.owner.key.bind.local` with `device`, `publicKey` (hex) and `binding`: gives an already
/// enrolled owner-check key its binding to this computer's device (a key enrolled before bindings
/// existed). The key must be enrolled and the binding must be its own signature
/// ([`check_key_binding`]), which needs the owner's presence (Touch ID, Windows Hello). The owner app
/// or hub of this computer only. Engine only.
pub fn bind_owner_check_key(params: &Value, from_app: bool, from_companion: bool) -> Result<Value> {
    if !from_app && !from_companion {
        return Err(Refusal::new(
            "unlock_needs_auth",
            format!(
                "the owner-check key is bound by the {p} app on this computer, never remotely",
                p = product()
            ),
        ));
    }
    let _gate = REGISTRY_GATE.lock().unwrap_or_else(|e| e.into_inner());
    let public = params["publicKey"]
        .as_str()
        .and_then(unhex)
        .ok_or_else(|| Refusal::new("invalid_request", "publicKey is hex"))?;
    let device = params["device"].as_str().unwrap_or("");
    let enrolled = read_owner_check_keys()?
        .iter()
        .any(|k| k.device == device && k.public == public);
    if !enrolled {
        return Err(Refusal::new(
            "approver_unknown",
            "that owner-check key is not enrolled here",
        ));
    }
    store_binding_param(params, device, &public)?;
    Ok(json!({"device": device, "bound": true}))
}

/// Verifies a `device` (owner check) signature against `owner-check-keys.json`: an unknown key is
/// `approver_unknown`; the window, nonce and signature are checked and the nonce used up as in
/// `verify`. Needs no PIN, before or after a restart. Returns the proof the signature leaves.
pub fn verify_owner_check_proof(
    request: &str,
    action: &str,
    client: &str,
    signed: &Signed,
) -> Result<OwnerCheckProof> {
    let keys = read_owner_check_keys()?;
    let Some(key) = keys.iter().find(|k| k.key_id == signed.key_id) else {
        return Err(Refusal::new(
            "approver_unknown",
            "no enrolled owner-check key made this signature",
        ));
    };
    verify_with(
        &key.public,
        &key.device,
        "device",
        request,
        action,
        client,
        signed,
    )?;
    Ok(OwnerCheckProof {
        device: key.device.clone(),
        public_key: hex(&key.public),
        challenge: request.to_string(),
        client: client.to_string(),
        key_id: key.key_id.clone(),
        nonce: signed.nonce.clone(),
        issued: signed.issued,
        expires: signed.expires,
        signature: hex(&signed.signature),
        binding: stored_binding(&key.device, &hex(&key.public)),
    })
}

/// Verifies, with an enrolled owner-check key (Touch ID or password key on the Mac, Windows Hello
/// key on Windows; no PIN, and no PIN needed after a restart), a signature of `kind` over the
/// engine's own `request`, `action` and `client`. The kind is part of the signed bytes, so a
/// `device` signature never answers an unlock and the reverse. `approver_unknown` when no
/// owner-check key made it.
pub fn verify_owner_check_kind(
    kind: &str,
    request: &str,
    action: &str,
    client: &str,
    signed: &Signed,
) -> Result<()> {
    let keys = read_owner_check_keys()?;
    let Some(key) = keys.iter().find(|k| k.key_id == signed.key_id) else {
        return Err(Refusal::new(
            "approver_unknown",
            "no enrolled owner-check key made this signature",
        ));
    };
    verify_with(
        &key.public,
        &key.device,
        kind,
        request,
        action,
        client,
        signed,
    )
}

/// The device label (`mac-owner-check` or `windows-hello`) of the enrolled, proven owner-check key
/// `key_id`, which is the device the signed text names; `None` when no such key is enrolled.
pub fn owner_check_key_device(key_id: &str) -> Result<Option<String>> {
    Ok(read_owner_check_keys()?
        .iter()
        .find(|k| k.key_id == key_id)
        .map(|k| k.device.clone()))
}

/// Whether any owner-check key is enrolled. A read or parse error is an error (never "no").
pub fn owner_check_enrolled() -> Result<bool> {
    let check = read_owner_check()?;
    // A seal secret with no key file ("deleted") or a damaged file is enrolled-but-unreadable.
    if matches!(check.state, "deleted" | "unproven") {
        return Err(unavailable(
            "the owner-check keys are missing, damaged or unsealed; enrol the key again",
        ));
    }
    Ok(!check.keys.is_empty())
}

/// HMAC-SHA-256 (hex) of `text` under the owner-check seal secret, which lives in the OS
/// credential store and is created at the first owner-check enrolment. Engine-side seal of a
/// stored unlock token: an item another process wrote cannot carry a valid seal without the
/// secret. A missing or unreadable secret is a refusal (fail closed).
pub fn owner_check_seal(text: &str) -> Result<String> {
    match read_check_secret()? {
        Some(secret) => Ok(check_mac(&secret, text)),
        None => Err(unavailable("no owner-check key has been enrolled yet")),
    }
}

/// Whether `mac` is [`owner_check_seal`] of `text`. A missing or unreadable secret is `false`.
pub fn owner_check_seal_ok(text: &str, mac: &str) -> bool {
    matches!(read_check_secret(), Ok(Some(secret)) if check_mac_ok(&secret, text, mac))
}

/// [`verify_owner_check_proof`] without the proof.
pub fn verify_owner_check(
    request: &str,
    action: &str,
    client: &str,
    signed: &Signed,
) -> Result<()> {
    verify_owner_check_proof(request, action, client, signed).map(|_| ())
}

/// `remote.owner.key.revoke` with `keyId` and `auth` (kind `revoke`, action the key id) or `pin`.
/// Owner-only. Engine only.
pub fn revoke(params: &Value, from_app: bool) -> Result<Value> {
    if !from_app {
        return Err(Refusal::new(
            "unlock_needs_auth",
            format!(
                "owner keys are revoked from the {p} app on this computer, never remotely",
                p = product()
            ),
        ));
    }
    let _gate = REGISTRY_GATE.lock().unwrap_or_else(|e| e.into_inner());
    let id = params["keyId"].as_str().unwrap_or("");
    let mut keys = load()?.keys;
    if !keys.iter().any(|k| k.key_id == id) {
        return Ok(json!({"revoked": false}));
    }
    authorise(&keys, params, "revoke", id)?;
    keys.retain(|k| k.key_id != id);
    save(&keys)?;
    Ok(json!({"revoked": true}))
}
