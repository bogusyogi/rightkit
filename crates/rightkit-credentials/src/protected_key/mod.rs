//! OS-bound keys (feature `protected-key`; rightkit#964, #944, #410): a private key that never
//! leaves the secure hardware and is used only after the OS has verified the user.
//!
//! * macOS: a Secure Enclave P-256 key (`kSecAttrTokenIDSecureEnclave`) whose access control is
//!   `biometryCurrentSet` (Touch ID; enrolling or removing a finger invalidates the key by design)
//!   or, as the weaker opt-in, `userPresence` (Touch ID or the account password), always with
//!   `privateKeyUsage`. Signing is ECDSA P-256 SHA-256 (X9.62 DER); wrapping is ECIES
//!   (`ECIESEncryptionCofactorX963SHA256AESGCM`) to the key's public half, so sealing needs no
//!   prompt and unsealing needs the Enclave and the user.
//! * Windows: a Windows Hello key credential (`KeyCredentialManager`, TPM-backed where present).
//!   Signing is RSA PKCS#1 v1.5 SHA-256, deterministic; a wrapping key is the HKDF-SHA256 of the
//!   signature over a fixed per-key challenge, so unsealing needs Hello verification every time
//!   and no key is ever stored. Remote Desktop sessions and Hello re-enrolment are device
//!   qualification items (see the CHANGELOG).
//! * Everything else: every call is [`ProtectedKeyError::Unsupported`]. There is no fallback to a
//!   file, a passphrase or memory; a caller that wants one offers its own slot (see
//!   [`seal_with_pin`]).
//!
//! A plaintext key never reaches the caller before the OS operation: [`unseal`] returns it only
//! after the Enclave or Hello operation succeeded. Nothing here stores a secret; the only item
//! written is a public-key marker (macOS) that lets a lost key be reported as
//! [`ProtectedKeyError::Invalidated`] rather than as missing.

use crate::namespace::Namespace;
use crate::secret::Secret;
use zeroize::Zeroizing;

#[cfg(target_os = "macos")]
mod apple;
#[cfg(target_os = "macos")]
use apple as sys;
#[cfg(windows)]
mod win;
#[cfg(windows)]
use win as sys;
#[cfg(not(any(target_os = "macos", windows)))]
mod other;
#[cfg(not(any(target_os = "macos", windows)))]
use other as sys;

/// Why an OS-bound key operation did not happen. Never carries a key, a message or a secret.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ProtectedKeyError {
    /// This platform, or this build (macOS without the keychain-access-groups entitlement that
    /// Secure Enclave keys need), has no OS-bound key.
    #[error("OS-bound keys are not supported here")]
    Unsupported,
    /// The device has no biometry, Hello or passcode set up for this user.
    #[error("no user verification method is enrolled")]
    NotEnrolled,
    /// The user failed verification, or policy denied the operation.
    #[error("the user verification was denied")]
    Denied,
    /// The user dismissed the prompt.
    #[error("the user cancelled the verification")]
    Cancelled,
    /// The key existed and is gone because the enrolment changed (a finger added, Hello reset).
    /// Whatever it protected is unrecoverable; the caller falls back to its own slot.
    #[error("the OS-bound key was invalidated")]
    Invalidated,
    /// The key does not exist (never created, or deleted).
    #[error("the OS-bound key does not exist")]
    Missing,
    /// The OS security service is busy, locked or absent.
    #[error("the OS security service is unavailable")]
    Unavailable,
    /// Any other OS failure: operation name and status code only.
    #[error("OS key operation {op} failed (code {code})")]
    Os { op: &'static str, code: i64 },
    /// The name is empty, too long, or has a control character.
    #[error("invalid key name")]
    InvalidName,
    /// A sealed blob is malformed, or the wrapping key did not open it.
    #[error("the sealed blob is invalid")]
    BadBlob,
}

pub type Result<T> = std::result::Result<T, ProtectedKeyError>;

/// How strongly the OS binds use of the key to the user (macOS; Windows Hello is always the
/// verification and ignores this).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protection {
    /// Touch ID of the CURRENTLY enrolled fingers; a change invalidates the key. The default.
    BiometryCurrentSet,
    /// Touch ID or the account password; survives re-enrolment. The weaker, explicit opt-in.
    UserPresence,
}

/// The signature scheme of a key, which depends on the OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureAlgorithm {
    /// ECDSA P-256 over SHA-256, X9.62 DER signature, public key as the 65-byte uncompressed point.
    EcdsaP256Sha256,
    /// RSASSA-PKCS1-v1_5 over SHA-256, public key as X.509 SubjectPublicKeyInfo DER.
    RsaPkcs1Sha256,
}

/// The public half of an OS-bound key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKey {
    pub algorithm: SignatureAlgorithm,
    pub bytes: Vec<u8>,
}

/// A handle on one named OS-bound key of one app namespace. Constructing it touches nothing.
#[derive(Debug, Clone)]
pub struct ProtectedKey {
    tag: String,
}

const MAX_NAME: usize = 128;

impl ProtectedKey {
    /// The key `name` of `ns`. `name` is 1..=128 characters with no control character.
    pub fn new(ns: &Namespace, name: &str) -> Result<Self> {
        if name.is_empty() || name.chars().count() > MAX_NAME || name.chars().any(char::is_control)
        {
            return Err(ProtectedKeyError::InvalidName);
        }
        Ok(Self {
            tag: format!("{}.protected-key.{}", ns.service(), name),
        })
    }

    /// Create the key. An existing key is never replaced: its public key is returned instead, so
    /// a caller can call this at every start. Creation may prompt on Windows (Hello enrolment)
    /// and never on macOS.
    pub fn create(&self, protection: Protection) -> Result<PublicKey> {
        if let Some(existing) = sys::public_key(&self.tag)? {
            return Ok(existing);
        }
        sys::create(&self.tag, protection)
    }

    /// The public key, or `None` when there is none. Never prompts. A key lost to a changed
    /// enrolment is [`ProtectedKeyError::Invalidated`], never `None`.
    pub fn public_key(&self) -> Result<Option<PublicKey>> {
        sys::public_key(&self.tag)
    }

    /// Sign `message` with the private key after the OS verified the user (`reason` is the text
    /// of the prompt where the OS shows one).
    pub fn sign(&self, message: &[u8], reason: &str) -> Result<Vec<u8>> {
        sys::sign(&self.tag, message, reason)
    }

    /// Remove the key. `Ok(false)` when there was none.
    pub fn delete(&self) -> Result<bool> {
        sys::delete(&self.tag)
    }

    /// Wrap `plaintext` (a key slot) so that only this key can open it. `aad` binds the blob to
    /// its purpose (a vault id, say) and must be given again to [`ProtectedKey::unseal`].
    pub fn seal(&self, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
        sys::seal(&self.tag, plaintext, aad)
    }

    /// Open a blob made by [`ProtectedKey::seal`]. The OS verifies the user first; the plaintext
    /// exists in this process only after that succeeded.
    pub fn unseal(&self, blob: &[u8], aad: &[u8], reason: &str) -> Result<Secret> {
        sys::unseal(&self.tag, blob, aad, reason).map(Secret::from_bytes)
    }
}

/// PBKDF2-HMAC-SHA256 iteration count for [`seal_with_pin`]. A PIN has little entropy: the OS
/// key above is the strong slot, and this is the caller's fallback, so the cost is high.
pub const PIN_ITERATIONS: u32 = 600_000;
const PIN_SALT_LEN: usize = 16;
const PIN_BLOB_VERSION: u8 = 1;

/// Wrap `plaintext` under a key derived from `pin` (PBKDF2-HMAC-SHA256, random salt, AES-256-GCM
/// bound to `aad`). Blob: version byte, salt, nonce, ciphertext, tag. The caller owns attempt
/// limits; this offers no protection against guessing a short PIN offline beyond the iteration
/// count.
pub fn seal_with_pin(pin: &[u8], plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    let mut salt = [0u8; PIN_SALT_LEN];
    rightkit_crypto::random_fill(&mut salt).map_err(|_| ProtectedKeyError::Unavailable)?;
    let key = pin_key(pin, &salt)?;
    let sealed = rightkit_crypto::aead::seal(
        rightkit_crypto::aead::Cipher::Aes256Gcm,
        &key,
        aad,
        plaintext,
    )
    .map_err(|_| ProtectedKeyError::Unavailable)?;
    let mut out = Vec::with_capacity(1 + PIN_SALT_LEN + sealed.len());
    out.push(PIN_BLOB_VERSION);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&sealed);
    Ok(out)
}

/// Open a blob made by [`seal_with_pin`]. A wrong PIN and a damaged blob are both
/// [`ProtectedKeyError::BadBlob`].
pub fn unseal_with_pin(pin: &[u8], blob: &[u8], aad: &[u8]) -> Result<Secret> {
    if blob.len() < 1 + PIN_SALT_LEN || blob[0] != PIN_BLOB_VERSION {
        return Err(ProtectedKeyError::BadBlob);
    }
    let (salt, sealed) = blob[1..].split_at(PIN_SALT_LEN);
    let key = pin_key(pin, salt)?;
    rightkit_crypto::aead::open(rightkit_crypto::aead::Cipher::Aes256Gcm, &key, aad, sealed)
        .map(Secret::from_bytes)
        .map_err(|_| ProtectedKeyError::BadBlob)
}

fn pin_key(pin: &[u8], salt: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    rightkit_crypto::pbkdf2_derive(
        rightkit_crypto::Pbkdf2Hash::Sha256,
        PIN_ITERATIONS,
        salt,
        pin,
        rightkit_crypto::aead::KEY_LEN,
    )
    .map(Zeroizing::new)
    .map_err(|_| ProtectedKeyError::Unavailable)
}

const STORE_BLOB_VERSION: u8 = 1;
const STORE_WRAP_INFO: &[u8] = b"rightkit-credentials store-wrapped seal v1";

fn store_error(e: crate::Error) -> ProtectedKeyError {
    match e {
        crate::Error::Denied => ProtectedKeyError::Denied,
        crate::Error::InteractionRequired => ProtectedKeyError::Unavailable,
        crate::Error::Backend { op, code } => ProtectedKeyError::Os { op, code },
        _ => ProtectedKeyError::Unavailable,
    }
}

/// The wrapping key kept in the OS credential store under `account`, derived with HKDF so the
/// stored bytes are never the AES key itself.
fn store_key(store: &crate::Store, account: &str, aad: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    let (stored, _) = store.get_or_create_key(account, 32).map_err(store_error)?;
    let mut info = Vec::with_capacity(STORE_WRAP_INFO.len() + aad.len());
    info.extend_from_slice(STORE_WRAP_INFO);
    info.extend_from_slice(aad);
    rightkit_crypto::hkdf_sha256(
        stored.expose_bytes(),
        &[],
        &info,
        rightkit_crypto::aead::KEY_LEN,
    )
    .map(Zeroizing::new)
    .map_err(|_| ProtectedKeyError::Unavailable)
}

/// Seal `plaintext` under an AES-256-GCM key that lives in the OS credential store (Keychain,
/// Credential Manager which is DPAPI-protected, Secret Service) under `account`, minted on first
/// use. This is the OS-store-wrapped blob for secrets that must be non-interactive: it needs no
/// Secure Enclave and no user verification, so any process of the same user that can read the
/// store item can open the blob. Use [`ProtectedKey`] where the user must be present.
pub fn seal_with_store(
    store: &crate::Store,
    account: &str,
    plaintext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>> {
    let key = store_key(store, account, aad)?;
    let sealed = rightkit_crypto::aead::seal(
        rightkit_crypto::aead::Cipher::Aes256Gcm,
        &key,
        aad,
        plaintext,
    )
    .map_err(|_| ProtectedKeyError::Unavailable)?;
    let mut out = Vec::with_capacity(1 + sealed.len());
    out.push(STORE_BLOB_VERSION);
    out.extend_from_slice(&sealed);
    Ok(out)
}

/// Open a blob made by [`seal_with_store`]. The store item must still exist: it is never minted
/// here, so a lost item is [`ProtectedKeyError::Missing`] rather than a new key that opens nothing.
pub fn unseal_with_store(
    store: &crate::Store,
    account: &str,
    blob: &[u8],
    aad: &[u8],
) -> Result<Secret> {
    if blob.len() < 1 || blob[0] != STORE_BLOB_VERSION {
        return Err(ProtectedKeyError::BadBlob);
    }
    if !store.exists(account).map_err(store_error)? {
        return Err(ProtectedKeyError::Missing);
    }
    let key = store_key(store, account, aad)?;
    rightkit_crypto::aead::open(
        rightkit_crypto::aead::Cipher::Aes256Gcm,
        &key,
        aad,
        &blob[1..],
    )
    .map(Secret::from_bytes)
    .map_err(|_| ProtectedKeyError::BadBlob)
}
