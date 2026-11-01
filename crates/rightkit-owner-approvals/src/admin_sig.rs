//! Signed phone approval for privileged commands.
//!
//! The computer issues a challenge (`admin.challenge`): 32 random bytes, valid 60 s, one
//! outstanding per request, persisted so a restart cannot replay. The phone signs, with a
//! Secure Enclave P-256 key that only passes Face ID, Touch ID or the passcode, the bytes built
//! by [`approval_message`]. The computer checks the signature against a key the owner enrolled
//! at this computer after comparing fingerprints (never remotely). Wire formats, shared with
//! `ios/Sources/Approvals/ApprovalKey.swift`:
//!
//! * signed bytes: for each of `pulse-admin-approve-1`, computer device id, phone device id,
//!   request id, command hash (lowercase hex text), nonce (lowercase hex text), `expiresMs`
//!   (decimal text): an 8-byte little-endian length, then the UTF-8 bytes;
//! * signature: ECDSA P-256 over those bytes with SHA-256 inside the signature
//!   (`ecdsaSignatureMessageX962SHA256`), DER, lowercase hex;
//! * public key: 65-byte X9.63 point, lowercase hex; key id: hex of the first 16 bytes of its SHA-256.
//!
//! Unix keeps the enrolled keys in a root-owned file (`key_file`) written by one sudo run;
//! the Windows service (`winadmin/`) keeps its own file (writable only by administrators, changed
//! only after one local UAC consent) and verifies there. For `admin.approve` this module only
//! checks the nonce on Windows and forwards the whole envelope; for the other signed answers
//! (`confirm.*`, `approvals.*`) it spends the nonce and asks the service to verify the signature
//! over the bytes it built itself (`op: verify`, an upstream issue), so no key bytes and no
//! enrolled-key file are read here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ring::signature::{ECDSA_P256_SHA256_ASN1, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::PhoneError;

pub const APPROVE_TAG: &str = "pulse-admin-approve-1";
/// How long one challenge can be answered.
pub const CHALLENGE_TTL_MS: u64 = 60_000;
/// A spent nonce is remembered this long (a nonce is only answerable while outstanding, so this
/// is a second lock, not the first).
const USED_KEEP_MS: u64 = 24 * 60 * 60 * 1000;
const OUTSTANDING_MAX: usize = 64;
const USED_MAX: usize = 4096;
const AUDIT_ROTATE_BYTES: u64 = 4 * 1024 * 1024;

/// The root-owned file the enrolled phone keys live in on Unix (set by the host app with
/// [`crate::set_admin_key_file`]).
#[cfg(unix)]
pub fn key_file() -> &'static str {
    crate::admin_key_file()
}

/// True when `path` (canonical) and every folder above it belong to root and no group or other
/// account can write them, so nothing short of root can change it after the owner approves.
#[cfg(unix)]
pub fn admin_root_only(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    path.is_absolute()
        && path.ancestors().all(|p| {
            std::fs::symlink_metadata(p)
                .is_ok_and(|m| m.uid() == 0 && m.mode() & 0o022 == 0 && !m.file_type().is_symlink())
        })
}

fn hex(bytes: &[u8]) -> String {
    rightkit_fs::digest::hex(bytes)
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    rightkit_fs::digest::unhex(text)
}

/// The bytes the phone signs.
pub fn approval_message(
    computer_id: &str,
    phone_id: &str,
    request_id: &str,
    command_hash: &str,
    nonce: &str,
    expires_ms: u64,
) -> Vec<u8> {
    let expires = expires_ms.to_string();
    let mut out = Vec::new();
    for part in [
        APPROVE_TAG,
        computer_id,
        phone_id,
        request_id,
        command_hash,
        nonce,
        expires.as_str(),
    ] {
        out.extend_from_slice(&(part.len() as u64).to_le_bytes());
        out.extend_from_slice(part.as_bytes());
    }
    out
}

/// A 65-byte X9.63 P-256 point (`04` then x and y) from lowercase or uppercase hex.
pub fn parse_public(text: &str) -> Option<Vec<u8>> {
    let bytes = unhex(text)?;
    (bytes.len() == 65 && bytes[0] == 4).then_some(bytes)
}

/// The id the phone and the computer use for a public key.
pub fn key_id(public: &[u8]) -> String {
    hex(&ring::digest::digest(&ring::digest::SHA256, public).as_ref()[..16])
}

/// What the owner compares on both screens: the key id in groups of four.
pub fn fingerprint(key_id: &str) -> String {
    key_id
        .as_bytes()
        .chunks(4)
        .filter_map(|c| std::str::from_utf8(c).ok())
        .collect::<Vec<_>>()
        .join(" ")
}

/// True when `sig_der` is a valid ECDSA P-256 SHA-256 signature of `message` by `public`.
pub fn verify(public: &[u8], message: &[u8], sig_der: &[u8]) -> bool {
    UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, public)
        .verify(message, sig_der)
        .is_ok()
}

// ---- enrolled keys ----------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhoneKey {
    pub key_id: String,
    pub device_id: String,
    /// X9.63 hex.
    pub public_key: String,
    pub enrolled_ms: u64,
    #[serde(default)]
    pub revoked: bool,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyFile {
    #[serde(default)]
    pub version: u32,
    /// This computer's device id when the keys were enrolled.
    #[serde(default)]
    pub computer_id: String,
    #[serde(default)]
    pub keys: Vec<PhoneKey>,
}

/// Local revocations: a user-writable record that only ever removes trust. A key enrolled at or
/// before the device's revocation time is revoked; enrolling again at the computer renews it.
#[derive(Default, Serialize, Deserialize)]
struct Revocations {
    #[serde(default)]
    devices: BTreeMap<String, u64>,
    /// The record exists but cannot be read or parsed: nothing is trusted (fail closed).
    #[serde(skip)]
    unreadable: bool,
}

fn revoked_path(root: &Path) -> PathBuf {
    root.join("admin-revoked.json")
}

fn load_revocations(root: &Path) -> Revocations {
    match std::fs::read(revoked_path(root)) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or(Revocations {
            unreadable: true,
            ..Default::default()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Revocations::default(),
        Err(_) => Revocations {
            unreadable: true,
            ..Default::default()
        },
    }
}

/// True when the owner's local record says this phone's admin access was switched off, or the
/// record cannot be read (fail closed). Windows checks this before forwarding to the service,
/// which has no enrolment time to compare; switching the phone on again clears it.
pub fn locally_revoked(root: &Path, device_id: &str) -> bool {
    let file = load_revocations(root);
    file.unreadable || file.devices.contains_key(device_id)
}

/// The owner switched this phone's admin access on again (Windows; Unix compares enrolment times).
pub fn clear_local_revocation(root: &Path, device_id: &str) -> Result<(), PhoneError> {
    let mut file = load_revocations(root);
    if file.unreadable {
        file = Revocations::default();
    }
    if file.devices.remove(device_id).is_none() {
        return Ok(());
    }
    write_private(
        &revoked_path(root),
        &serde_json::to_vec(&file).unwrap_or_default(),
    )
}

/// The owner switched this phone's admin access off: its keys stop working until enrolled again.
pub fn revoke_device_local(root: &Path, device_id: &str, now_ms: u64) -> Result<(), PhoneError> {
    let mut file = load_revocations(root);
    file.devices.insert(device_id.to_string(), now_ms);
    write_private(
        &revoked_path(root),
        &serde_json::to_vec(&file).unwrap_or_default(),
    )
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), PhoneError> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    rightkit_fs::atomic_write_with_options(
        path,
        rightkit_fs::AtomicWriteOptions {
            privacy: rightkit_fs::Privacy::OwnerOnlyUnixInheritWindows,
            ..Default::default()
        },
        |file| std::io::Write::write_all(file, bytes),
    )
    .map_err(|e| PhoneError::retry("state_unwritable", e.to_string()))
}

/// The root-owned key file, or `Ok(None)` when none exists. A file that exists but could be
/// changed by a non-root account, or cannot be read, is an error: the approval is refused, never
/// downgraded to the password alone.
#[cfg(unix)]
fn load_trusted() -> Result<Option<KeyFile>, PhoneError> {
    let path = Path::new(key_file());
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(untrusted()),
        Ok(_) => {}
    }
    if !admin_root_only(path) {
        return Err(untrusted());
    }
    let bytes = std::fs::read(path).map_err(|_| untrusted())?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| untrusted())
}

#[cfg(unix)]
fn untrusted() -> PhoneError {
    PhoneError::new(
        "admin_keys_untrusted",
        "the enrolled phone keys cannot be trusted (not owned by root, or unreadable); nothing ran",
    )
}

#[cfg(unix)]
fn live(key: &PhoneKey, revoked: &Revocations) -> bool {
    !key.revoked
        && !revoked.unreadable
        && revoked
            .devices
            .get(&key.device_id)
            .is_none_or(|at| *at < key.enrolled_ms)
}

/// The live enrolled keys the Windows service holds, as `(key id, device id)`, minus any device
/// the owner switched off here (the service has no enrolment time to compare, so any local
/// revocation refuses). The service's answer is the record of trust: a service that is missing or
/// does not answer yields an error, never an empty list that could be mistaken for "no keys".
#[cfg(windows)]
fn windows_live_keys(root: &Path) -> Result<Vec<(String, String)>, PhoneError> {
    let keys = crate::phone_keys()?;
    Ok(keys
        .into_iter()
        .filter(|(_, device, revoked)| !revoked && !locally_revoked(root, device))
        .map(|(key, device, _)| (key, device))
        .collect())
}

/// True when this computer holds a live enrolled key for `device_id` (Windows: the service holds
/// the keys and says which are live).
pub fn enrolled_for(root: &Path, device_id: &str) -> Result<bool, PhoneError> {
    #[cfg(unix)]
    {
        let revoked = load_revocations(root);
        Ok(load_trusted()?.is_some_and(|f| {
            f.keys
                .iter()
                .any(|k| k.device_id == device_id && live(k, &revoked))
        }))
    }
    #[cfg(windows)]
    {
        Ok(windows_live_keys(root)?.iter().any(|(_, d)| d == device_id))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (root, device_id);
        Ok(false)
    }
}

/// Revalidate the exact key that signed a still-unused confirmation.
pub fn enrolled_key(root: &Path, device_id: &str, key_id: &str) -> bool {
    #[cfg(unix)]
    {
        let revoked = load_revocations(root);
        load_trusted().ok().flatten().is_some_and(|file| {
            file.keys.iter().any(|key| {
                key.device_id == device_id && key.key_id == key_id && live(key, &revoked)
            })
        })
    }
    #[cfg(windows)]
    {
        windows_live_keys(root)
            .is_ok_and(|keys| keys.iter().any(|(k, d)| k == key_id && d == device_id))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (root, device_id, key_id);
        false
    }
}

/// True once this computer holds any approval key record (even a revoked one): from then on no
/// phone may approve with the password alone (Windows always requires the signature).
pub fn keys_recorded() -> Result<bool, PhoneError> {
    #[cfg(unix)]
    {
        Ok(load_trusted()?.is_some_and(|f| !f.keys.is_empty()))
    }
    #[cfg(windows)]
    {
        Ok(!crate::phone_keys()?.is_empty())
    }
    #[cfg(not(any(unix, windows)))]
    {
        Ok(false)
    }
}

/// Devices the owner switched off since they were enrolled: the root-owned key file still has to
/// be told (`pulse admin revoke`), because a user-writable file alone is not the record of trust.
pub fn local_revoked_devices(root: &Path) -> Vec<String> {
    load_revocations(root).devices.into_keys().collect()
}

/// True when any phone has a live enrolled key here.
pub fn any_enrolled(root: &Path) -> bool {
    #[cfg(unix)]
    {
        let revoked = load_revocations(root);
        load_trusted()
            .ok()
            .flatten()
            .is_some_and(|f| f.keys.iter().any(|k| live(k, &revoked)))
    }
    #[cfg(windows)]
    {
        windows_live_keys(root).is_ok_and(|keys| !keys.is_empty())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = root;
        false
    }
}

/// The enrolled keys for the owner's `pulse admin keys` (Unix).
#[cfg(unix)]
pub fn enrolled_keys() -> Result<Vec<PhoneKey>, PhoneError> {
    Ok(load_trusted()?.map(|f| f.keys).unwrap_or_default())
}

#[cfg(unix)]
fn lookup(root: &Path, device_id: &str, key_id: &str) -> Result<PhoneKey, PhoneError> {
    let revoked = load_revocations(root);
    load_trusted()?
        .and_then(|f| {
            f.keys
                .into_iter()
                .find(|k| k.key_id == key_id && k.device_id == device_id && live(k, &revoked))
        })
        .ok_or_else(|| {
            PhoneError::new(
                "key_not_enrolled",
                "this phone's approval key is not enrolled at this computer (or was revoked); run the admin enrol command here",
            )
        })
}

/// The key file as the owner's own command edits it (Unix): the current root-owned file, or an
/// empty one when none exists. Anything that is not root-owned and root-only is refused. The
/// write itself is done by root's own system tools (`/usr/bin/install` under sudo), never by
/// this program running as root.
#[cfg(unix)]
pub fn load_for_edit() -> Result<KeyFile, String> {
    match load_trusted() {
        Ok(file) => Ok(file.unwrap_or_default()),
        Err(e) => Err(e.message),
    }
}

/// `file` with one key enrolled (replacing any record of the same key). Returns its id, computed
/// from the bytes that will be written.
pub fn edit_enrol(
    file: &mut KeyFile,
    computer_id: &str,
    device_id: &str,
    public_hex: &str,
    now_ms: u64,
) -> Result<String, String> {
    let public = parse_public(public_hex).ok_or("the key is not a P-256 public key")?;
    let id = key_id(&public);
    file.version = 1;
    file.computer_id = computer_id.to_string();
    file.keys.retain(|k| k.key_id != id);
    file.keys.push(PhoneKey {
        key_id: id.clone(),
        device_id: device_id.to_string(),
        public_key: hex(&public),
        enrolled_ms: now_ms,
        revoked: false,
    });
    Ok(id)
}

/// `file` with every key whose id (spaces ignored) or device id is `target` marked revoked.
/// Returns how many.
pub fn edit_revoke(file: &mut KeyFile, target: &str) -> Result<usize, String> {
    let wanted: String = target.chars().filter(|c| !c.is_whitespace()).collect();
    let mut count = 0;
    for k in file
        .keys
        .iter_mut()
        .filter(|k| k.key_id == wanted || k.device_id == wanted)
    {
        k.revoked = true;
        count += 1;
    }
    if count == 0 {
        return Err("no such key: no enrolled key has that id".to_string());
    }
    Ok(count)
}

/// The bytes to install.
pub fn render(file: &KeyFile) -> Result<Vec<u8>, String> {
    serde_json::to_vec_pretty(file).map_err(|e| e.to_string())
}

/// A key file read back from disk.
pub fn parse_file(bytes: &[u8]) -> Option<KeyFile> {
    serde_json::from_slice(bytes).ok()
}

/// True when `file` holds a live key for `device_id` whose id, recomputed from the stored public
/// key bytes, is `key_id`.
pub fn holds_key(file: &KeyFile, device_id: &str, key_id_wanted: &str) -> bool {
    file.keys.iter().any(|k| {
        !k.revoked
            && k.device_id == device_id
            && k.key_id == key_id_wanted
            && parse_public(&k.public_key).is_some_and(|p| key_id(&p) == key_id_wanted)
    })
}

/// True when every key matching `target` (id or device) in `file` is revoked, and there is one.
pub fn all_revoked(file: &KeyFile, target: &str) -> bool {
    let matching: Vec<&PhoneKey> = file
        .keys
        .iter()
        .filter(|k| k.key_id == target || k.device_id == target)
        .collect();
    !matching.is_empty() && matching.iter().all(|k| k.revoked)
}

// ---- nonces -----------------------------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Outstanding {
    nonce: String,
    phone_id: String,
    expires_ms: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct NonceFile {
    #[serde(default)]
    outstanding: BTreeMap<String, Outstanding>,
    #[serde(default)]
    used: BTreeMap<String, u64>,
}

/// Challenges the computer issued and nonces it spent, on disk. A challenge is answerable only
/// while it is outstanding, and spending it removes it before anything else happens, so a
/// restart can neither revive nor replay one.
pub struct Nonces {
    path: PathBuf,
    guard: Mutex<()>,
}

impl Nonces {
    pub fn new(path: PathBuf) -> Nonces {
        Nonces {
            path,
            guard: Mutex::new(()),
        }
    }

    fn load(&self) -> NonceFile {
        std::fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn store(&self, file: &NonceFile) -> Result<(), PhoneError> {
        write_private(&self.path, &serde_json::to_vec(file).unwrap_or_default())
    }

    fn prune(file: &mut NonceFile, now: u64) {
        file.outstanding.retain(|_, o| o.expires_ms >= now);
        while file.outstanding.len() > OUTSTANDING_MAX {
            let Some(oldest) = file
                .outstanding
                .iter()
                .min_by_key(|(_, o)| o.expires_ms)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            file.outstanding.remove(&oldest);
        }
        file.used
            .retain(|_, at| now.saturating_sub(*at) < USED_KEEP_MS);
        while file.used.len() > USED_MAX {
            let Some(oldest) = file
                .used
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            file.used.remove(&oldest);
        }
    }

    /// A fresh nonce for `request_id`, replacing any outstanding one. Returns it and its expiry.
    pub fn issue(
        &self,
        request_id: &str,
        phone_id: &str,
        now: u64,
    ) -> Result<(String, u64), PhoneError> {
        let _g = self.guard.lock().unwrap_or_else(|e| e.into_inner());
        let mut raw = [0u8; 32];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut raw)
            .map_err(|_| PhoneError::new("rng_failed", "no random bytes for a challenge"))?;
        let nonce = hex(&raw);
        let expires = now + CHALLENGE_TTL_MS;
        let mut file = self.load();
        Self::prune(&mut file, now);
        file.outstanding.insert(
            request_id.to_string(),
            Outstanding {
                nonce: nonce.clone(),
                phone_id: phone_id.to_string(),
                expires_ms: expires,
            },
        );
        self.store(&file)?;
        Ok((nonce, expires))
    }

    /// Spend the outstanding challenge for `request_id`: it must be this phone's, equal to
    /// `nonce`, unused and unexpired. It is removed and recorded as used, on disk, whatever the
    /// outcome, so a wrong answer needs a new challenge. Returns its expiry (what was signed).
    pub fn take(
        &self,
        request_id: &str,
        phone_id: &str,
        nonce: &str,
        now: u64,
    ) -> Result<u64, PhoneError> {
        let _g = self.guard.lock().unwrap_or_else(|e| e.into_inner());
        let mut file = self.load();
        let Some(out) = file.outstanding.remove(request_id) else {
            return Err(PhoneError::new(
                "challenge_missing",
                "there is no challenge for this request; ask for a new one",
            ));
        };
        let spent_before = file.used.contains_key(&out.nonce);
        file.used.insert(out.nonce.clone(), now);
        Self::prune(&mut file, now);
        self.store(&file)?;
        let bad = |code: &str, message: &str| Err(PhoneError::new(code, message));
        if spent_before {
            return bad("nonce_used", "that challenge was already used");
        }
        if out.phone_id != phone_id
            || !rightkit_crypto::ct_eq(out.nonce.as_bytes(), nonce.as_bytes())
        {
            return bad(
                "nonce_mismatch",
                "that is not the challenge issued to this phone",
            );
        }
        if now > out.expires_ms {
            return bad(
                "challenge_expired",
                "the challenge expired; ask for a new one",
            );
        }
        Ok(out.expires_ms)
    }
}

// ---- one signed approval ----------------------------------------------------------------------

/// What the phone sent and the computer accepted; carried whole into the executor.
#[derive(Clone, Debug)]
pub struct SignedApproval {
    pub key_id: String,
    pub phone_id: String,
    pub computer_id: String,
    pub nonce: String,
    pub expires_ms: u64,
    /// DER signature, lowercase hex.
    pub sig: String,
}

fn hex_param<'a>(params: &'a Value, key: &str, max: usize) -> Option<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= max && s.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Non-Unix: any local revocation (or an unreadable record) refuses. Unix: `lookup` compares times.
fn locally_revoked_now(root: &Path, device_id: &str) -> bool {
    #[cfg(unix)]
    {
        load_revocations(root).unreadable && !device_id.is_empty()
    }
    #[cfg(not(unix))]
    {
        locally_revoked(root, device_id)
    }
}

/// Check the `sig`, `nonce` and `keyId` of an `admin.approve`. `verify_here` is true where this
/// process holds the enrolled keys (Unix); on Windows the service verifies the forwarded envelope
/// and this only spends the nonce. Order: key enrolled for this phone, nonce spent, signature.
#[allow(clippy::too_many_arguments)]
pub fn check(
    root: &Path,
    nonces: &Nonces,
    phone_id: &str,
    computer_id: &str,
    request_id: &str,
    command_hash: &str,
    params: &Value,
    verify_here: bool,
    now: u64,
) -> Result<SignedApproval, PhoneError> {
    let (Some(sig), Some(nonce), Some(key)) = (
        hex_param(params, "sig", 160),
        hex_param(params, "nonce", 64),
        hex_param(params, "keyId", 64),
    ) else {
        return Err(PhoneError::new(
            "signature_required",
            "this approval needs the phone's signature (sig, nonce, keyId)",
        ));
    };
    if locally_revoked_now(root, phone_id) {
        return Err(PhoneError::new(
            "key_not_enrolled",
            "this phone's admin access was switched off here; turn it on again in the app and enrol",
        ));
    }
    let (sig, nonce, key) = (
        sig.to_ascii_lowercase(),
        nonce.to_ascii_lowercase(),
        key.to_ascii_lowercase(),
    );
    #[cfg(unix)]
    let enrolled = if verify_here {
        Some(lookup(root, phone_id, &key)?)
    } else {
        None
    };
    #[cfg(not(unix))]
    let _ = (root, verify_here, computer_id, command_hash);
    let expires = nonces.take(request_id, phone_id, &nonce, now)?;
    #[cfg(unix)]
    if let Some(enrolled) = enrolled {
        let public = parse_public(&enrolled.public_key).ok_or_else(|| {
            PhoneError::new(
                "key_not_enrolled",
                "the enrolled key is damaged; enrol again",
            )
        })?;
        let message = approval_message(
            computer_id,
            phone_id,
            request_id,
            command_hash,
            &nonce,
            expires,
        );
        let der = unhex(&sig).unwrap_or_default();
        if !verify(&public, &message, &der) {
            return Err(PhoneError::new(
                "sig_invalid",
                "the phone's signature does not match this approval; nothing ran",
            ));
        }
    }
    Ok(SignedApproval {
        key_id: key,
        phone_id: phone_id.to_string(),
        computer_id: computer_id.to_string(),
        nonce,
        expires_ms: expires,
        sig,
    })
}

// ---- agent browser approvals ----------------------

/// The first line of a browser approval's signed text. A different domain from
/// [`APPROVE_TAG`]: a signature for one can never answer the other.
pub const BROWSER_TAG: &str = "pulse.browser.v1";

/// The text a phone signs to approve one agent browser request:
/// `pulse.browser.v1\n<request id>\n<sites joined by ",">\n<chat>\n<nonce>`, UTF-8, no trailing
/// newline. The computer builds it from its own record of the request, never from the caller.
pub fn browser_message(request_id: &str, sites: &[String], chat: &str, nonce: &str) -> String {
    format!(
        "{BROWSER_TAG}\n{request_id}\n{}\n{chat}\n{nonce}",
        sites.join(",")
    )
}

/// The first line of the bound form of a browser approval (BD13, #577): a different domain from
/// [`BROWSER_TAG`], so a v1 signature never answers a v2 request and the reverse.
pub const BROWSER_TAG_V2: &str = "pulse.browser.v2";

/// What a v2 approval binds besides the request id, sites and chat: the reason the owner was
/// shown, the computer's name, the session length in minutes and whether page content (text and
/// a screenshot of the approved sites' pages) may go to the agent's model provider (BD14).
#[derive(Clone, Copy, Debug)]
pub struct BrowserBind<'a> {
    pub reason: &'a str,
    pub device: &'a str,
    pub minutes: u64,
    pub share_content: bool,
}

/// `pulse.browser.v2\n<id>\n<sites joined by ",">\n<chat>\n<reason>\n<device>\n<minutes>\n<share 1|0>\n<nonce>`,
/// UTF-8, no trailing newline. Every field is one line (the computer strips control characters
/// from agent text), so no field can carry the separator.
pub fn browser_message_v2(
    request_id: &str,
    sites: &[String],
    chat: &str,
    bind: &BrowserBind<'_>,
    nonce: &str,
) -> String {
    format!(
        "{BROWSER_TAG_V2}\n{request_id}\n{}\n{chat}\n{}\n{}\n{}\n{}\n{nonce}",
        sites.join(","),
        bind.reason,
        bind.device,
        bind.minutes,
        if bind.share_content { "1" } else { "0" },
    )
}

/// The owner check's `action` for a v2 request: SHA-256 hex of [`browser_message_v2`].
pub fn browser_action_v2(
    request_id: &str,
    sites: &[String],
    chat: &str,
    bind: &BrowserBind<'_>,
    nonce: &str,
) -> String {
    hex(ring::digest::digest(
        &ring::digest::SHA256,
        browser_message_v2(request_id, sites, chat, bind, nonce).as_bytes(),
    )
    .as_ref())
}

/// What a paired computer's owner check signs instead (`approval_sig::message` with kind
/// `browser`): SHA-256 hex of [`browser_message`], the request's `action`.
pub fn browser_action(request_id: &str, sites: &[String], chat: &str, nonce: &str) -> String {
    hex(ring::digest::digest(
        &ring::digest::SHA256,
        browser_message(request_id, sites, chat, nonce).as_bytes(),
    )
    .as_ref())
}

/// ECDSA P-256 over SHA-256 as DER (a phone, Swift `derRepresentation`) or 64 raw bytes `r||s`
/// (the Mac's CryptoKit `rawRepresentation`).
#[cfg(unix)]
fn verify_der_or_raw(public: &[u8], message: &[u8], sig: &[u8]) -> bool {
    let algorithm = if sig.len() == 64 {
        &ring::signature::ECDSA_P256_SHA256_FIXED
    } else {
        &ECDSA_P256_SHA256_ASN1
    };
    UnparsedPublicKey::new(algorithm, public)
        .verify(message, sig)
        .is_ok()
}

/// Check the `{id, nonce, key_id, sig}` of a `browser.approve`. Order: this device's key is
/// enrolled here and live, the nonce is spent (single use, 60 s, removed whatever the outcome),
/// the signature. `phone` true: `sig` signs [`browser_message`] as it stands. A paired computer
/// signs with its owner-check key: the envelope of `approval_sig::message` with kind `browser`,
/// `request` the request id, `action` [`browser_action`], an empty client and the extra
/// `issued` and `expires` (Unix seconds, at most 120 apart, as `approval_sig` takes them).
/// Returns the key id that signed. Unix only: Windows keeps its keys in its own service.
#[allow(clippy::too_many_arguments)]
pub fn check_browser(
    root: &Path,
    nonces: &Nonces,
    device_id: &str,
    phone: bool,
    request_id: &str,
    sites: &[String],
    chat: &str,
    bind: &BrowserBind<'_>,
    params: &Value,
    now: u64,
) -> Result<(String, bool), PhoneError> {
    // v2 (`"v": 2`) signs the bound text; v1 stays accepted so an older phone or the Mac owner
    // check through a paired computer keeps working, but a v1 approval never grants page content
    // (BD13, BD14). The grant is the signed flag, never a bare param.
    let v2 = params.get("v").and_then(Value::as_u64) == Some(2);
    let bind = BrowserBind {
        share_content: v2 && params.get("share_content").and_then(Value::as_bool) == Some(true),
        ..*bind
    };
    let (Some(sig), Some(nonce), Some(key)) = (
        hex_param(params, "sig", 160),
        hex_param(params, "nonce", 64),
        hex_param(params, "key_id", 64),
    ) else {
        return Err(PhoneError::new(
            "signature_required",
            "this approval needs the owner's signature (sig, nonce, key_id)",
        ));
    };
    if locally_revoked_now(root, device_id) {
        return Err(PhoneError::new(
            "key_not_enrolled",
            "this device's approval access was switched off here; turn it on again in the app and enrol",
        ));
    }
    let (sig, nonce, key) = (
        sig.to_ascii_lowercase(),
        nonce.to_ascii_lowercase(),
        key.to_ascii_lowercase(),
    );
    #[cfg(unix)]
    {
        let enrolled = lookup(root, device_id, &key)?;
        let public = parse_public(&enrolled.public_key).ok_or_else(|| {
            PhoneError::new(
                "key_not_enrolled",
                "the enrolled key is damaged; enrol again",
            )
        })?;
        nonces.take(request_id, device_id, &nonce, now)?;
        let der = unhex(&sig).unwrap_or_default();
        let ok = if phone {
            let text = if v2 {
                browser_message_v2(request_id, sites, chat, &bind, &nonce)
            } else {
                browser_message(request_id, sites, chat, &nonce)
            };
            verify(&public, text.as_bytes(), &der)
        } else {
            let (Some(issued), Some(expires)) = (
                params.get("issued").and_then(Value::as_u64),
                params.get("expires").and_then(Value::as_u64),
            ) else {
                return Err(PhoneError::new(
                    "signature_required",
                    "a computer's approval carries issued and expires",
                ));
            };
            let at = now / 1000;
            if expires <= at || issued > at + 60 || expires < issued || expires - issued > 120 {
                return Err(PhoneError::new(
                    "approval_expired",
                    "the signed approval is expired or not yet valid; approve again",
                ));
            }
            let message = crate::approval_sig::message(
                "browser",
                request_id,
                &if v2 {
                    browser_action_v2(request_id, sites, chat, &bind, &nonce)
                } else {
                    browser_action(request_id, sites, chat, &nonce)
                },
                "",
                crate::approval_sig::OWNER_DEVICE,
                &key,
                &nonce,
                issued,
                expires,
            );
            verify_der_or_raw(&public, &message, &der)
        };
        if !ok {
            return Err(PhoneError::new(
                "sig_invalid",
                "the signature does not match this request; nothing was approved",
            ));
        }
        Ok((key, bind.share_content))
    }
    #[cfg(not(unix))]
    {
        let _ = (
            nonces, phone, request_id, sites, chat, now, sig, nonce, key, bind, v2,
        );
        Err(PhoneError::new(
            "unsupported",
            "agent browser approvals are not available on this computer",
        ))
    }
}

// ---- chat confirmation ---------------------------------------------------

/// The first field of a chat confirmation's signed bytes. A different domain from [`APPROVE_TAG`]
/// and [`BROWSER_TAG`]: a signature for one can never answer another.
pub const CONFIRM_TAG: &str = "pulse-chatconfirm-approve-1";

/// The bytes the phone signs to confirm one reserved action a server or guest asked for through
/// chat: [`CONFIRM_TAG`], this computer's device id, the phone's device id, the request id, the
/// payload hash (`chat_confirm::payload_hash`, lowercase hex), the nonce (lowercase hex) and the
/// expiry in ms (decimal text), each as an 8-byte little-endian length then its UTF-8 bytes (the
/// framing of [`approval_message`]). The computer builds them from its own record of the request;
/// the phone signs ECDSA P-256 SHA-256 (DER) over exactly these bytes.
pub fn confirm_message(
    computer_id: &str,
    phone_id: &str,
    request_id: &str,
    payload_hash: &str,
    nonce: &str,
    expires_ms: u64,
) -> Vec<u8> {
    let expires = expires_ms.to_string();
    let mut out = Vec::new();
    for part in [
        CONFIRM_TAG,
        computer_id,
        phone_id,
        request_id,
        payload_hash,
        nonce,
        expires.as_str(),
    ] {
        out.extend_from_slice(&(part.len() as u64).to_le_bytes());
        out.extend_from_slice(part.as_bytes());
    }
    out
}

/// Check the `sig`, `nonce` and `keyId` of a `confirm.approve`. Order: this phone's key is
/// enrolled here and live, the nonce is spent (single use, removed whatever the outcome), the
/// signature over [`confirm_message`]. Returns the key id that signed. Windows asks the elevated
/// service to verify (it holds the keys); with no service installed the confirmation is refused.
#[allow(clippy::too_many_arguments)]
pub fn check_confirm(
    root: &Path,
    nonces: &Nonces,
    phone_id: &str,
    computer_id: &str,
    request_id: &str,
    payload_hash: &str,
    params: &Value,
    now: u64,
) -> Result<String, PhoneError> {
    let (Some(sig), Some(nonce), Some(key)) = (
        hex_param(params, "sig", 160),
        hex_param(params, "nonce", 64),
        hex_param(params, "keyId", 64),
    ) else {
        return Err(PhoneError::new(
            "signature_required",
            "this confirmation needs the phone's signature (sig, nonce, keyId)",
        ));
    };
    if locally_revoked_now(root, phone_id) {
        return Err(PhoneError::new(
            "key_not_enrolled",
            "this phone's admin access was switched off here; turn it on again in the app and enrol",
        ));
    }
    let (sig, nonce, key) = (
        sig.to_ascii_lowercase(),
        nonce.to_ascii_lowercase(),
        key.to_ascii_lowercase(),
    );
    #[cfg(unix)]
    {
        let enrolled = lookup(root, phone_id, &key)?;
        let public = parse_public(&enrolled.public_key).ok_or_else(|| {
            PhoneError::new(
                "key_not_enrolled",
                "the enrolled key is damaged; enrol again",
            )
        })?;
        let expires = nonces.take(request_id, phone_id, &nonce, now)?;
        let message = confirm_message(
            computer_id,
            phone_id,
            request_id,
            payload_hash,
            &nonce,
            expires,
        );
        let der = unhex(&sig).unwrap_or_default();
        if !verify(&public, &message, &der) {
            return Err(PhoneError::new(
                "sig_invalid",
                "the phone's signature does not match this confirmation; nothing was approved",
            ));
        }
        Ok(key)
    }
    #[cfg(windows)]
    {
        // The service holds the keys: this phone must have a live one with this id, the nonce is
        // spent here (single use, removed whatever the outcome), then the service verifies the
        // signature over the bytes built from this computer's own record of the request.
        if !windows_live_keys(root)?
            .iter()
            .any(|(k, d)| *k == key && d == phone_id)
        {
            return Err(PhoneError::new(
                "key_not_enrolled",
                "this phone's approval key is not enrolled at this computer (or was revoked); run the admin enrol command here",
            ));
        }
        let expires = nonces.take(request_id, phone_id, &nonce, now)?;
        let message = confirm_message(
            computer_id,
            phone_id,
            request_id,
            payload_hash,
            &nonce,
            expires,
        );
        crate::verify_phone_signature(phone_id, computer_id, &key, &message, &sig)?;
        Ok(key)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (
            nonces,
            computer_id,
            request_id,
            payload_hash,
            now,
            sig,
            nonce,
            key,
        );
        Err(PhoneError::new(
            "unsupported",
            "phone confirmation of chat requests is not available on this computer",
        ))
    }
}

// ---- audit ------------------------------------------------------------------------------------

/// Append one JSON line. Callers pass ids, digests, the display command, the outcome and exit
/// code only: never the password, never a command's output. Best effort; an unwritable log does
/// not stop a run the owner approved (the Windows service keeps its own log).
pub fn audit(path: &Path, line: &Value) {
    use std::io::Write;
    if path.as_os_str().is_empty() {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() > AUDIT_ROTATE_BYTES) {
        let _ = std::fs::rename(path, path.with_extension("jsonl.1"));
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    if let Ok(mut file) = options.open(path) {
        let _ = writeln!(file, "{line}");
    }
}

/// An audit line's fixed shape.
#[allow(clippy::too_many_arguments)]
pub fn audit_line(
    now: u64,
    request_id: &str,
    machine: &str,
    phone_id: &str,
    key_fingerprint: &str,
    command_hash: &str,
    command: &str,
    agent: &str,
    reason: &str,
    outcome: &str,
    exit_code: Option<i32>,
    code: Option<&str>,
) -> Value {
    json!({
        "ts": now, "requestId": request_id, "machine": machine, "phoneDeviceId": phone_id,
        "keyFingerprint": key_fingerprint, "commandHash": command_hash, "command": command,
        "agent": agent, "reason": reason, "outcome": outcome, "exitCode": exit_code, "code": code,
    })
}
