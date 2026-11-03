//! Windows: a Windows Hello key credential (`KeyCredentialManager`).
//!
//! The key is created and used through the WinRT key-credential API, which keeps the private half
//! in the TPM where there is one and asks Hello to verify the user for every signature. Every WinRT
//! call runs on a fresh multithreaded-apartment thread with a bounded wait, like `os_auth::hello`,
//! so a caller on a UI (STA) thread never joins a WinRT operation itself.
//!
//! Signing is RSASSA-PKCS1-v1_5 over SHA-256 and is deterministic. The wrapping key of
//! `seal`/`unseal` is the HKDF-SHA256 of the signature over a fixed per-key challenge, with a random
//! salt per blob: no key is stored anywhere, and opening a blob needs a Hello verification each
//! time. Sealing needs the same verification (it needs the signature). Hello offers no prompt
//! text through this API, so `reason` is not shown. Remote Desktop sessions and Hello re-enrolment
//! behaviour are device qualification items.

use super::{ProtectedKeyError, Protection, PublicKey, Result, SignatureAlgorithm};
use std::sync::mpsc;
use std::time::Duration;
use zeroize::Zeroizing;

use ::windows::core::{Array, HSTRING};
use ::windows::Security::Credentials::{
    KeyCredential, KeyCredentialCreationOption, KeyCredentialManager, KeyCredentialStatus,
};
use ::windows::Security::Cryptography::CryptographicBuffer;
use ::windows::Storage::Streams::IBuffer;
use ::windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};

/// A verification prompt may wait on a person; bound it anyway.
const WAIT: Duration = Duration::from_secs(120);
/// `HRESULT_FROM_WIN32(ERROR_CANCELLED)`.
const HR_CANCELLED: i32 = 0x800704C7_u32 as i32;
const BLOB_VERSION: u8 = 1;
const SALT_LEN: usize = 16;
const WRAP_INFO: &[u8] = b"rightkit-credentials protected-key wrap v1";

fn os(op: &'static str) -> impl Fn(::windows::core::Error) -> ProtectedKeyError {
    move |e| {
        if e.code().0 == HR_CANCELLED {
            ProtectedKeyError::Cancelled
        } else {
            ProtectedKeyError::Os {
                op,
                code: i64::from(e.code().0),
            }
        }
    }
}

/// Runs `work` on a fresh MTA thread and waits at most [`WAIT`]. A thread that cannot start or
/// that dies is `Unavailable`; a wait that expires is `Os` (the operation is abandoned, and no
/// result of it is used).
fn on_mta<T: Send + 'static>(work: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("rightkit-protected-key".into())
        .spawn(move || {
            // SAFETY: balanced with `RoUninitialize` when it succeeded; a failure means the thread
            // already has an apartment, which is left untouched.
            let initialised = unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.is_ok();
            let result = work();
            if initialised {
                // SAFETY: matches the successful `RoInitialize` on this thread.
                unsafe { RoUninitialize() };
            }
            let _ = tx.send(result);
        })
        .map_err(|_| ProtectedKeyError::Unavailable)?;
    match rx.recv_timeout(WAIT) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(ProtectedKeyError::Os {
            op: "verification wait",
            code: 0,
        }),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(ProtectedKeyError::Unavailable),
    }
}

fn buffer(bytes: &[u8]) -> Result<IBuffer> {
    CryptographicBuffer::CreateFromByteArray(bytes).map_err(os("create buffer"))
}

fn bytes_of(b: &IBuffer) -> Result<Vec<u8>> {
    let mut array = Array::<u8>::new();
    CryptographicBuffer::CopyToByteArray(b, &mut array).map_err(os("copy buffer"))?;
    Ok(array.to_vec())
}

fn status_error(op: &'static str, status: KeyCredentialStatus) -> ProtectedKeyError {
    match status {
        KeyCredentialStatus::UserCanceled => ProtectedKeyError::Cancelled,
        KeyCredentialStatus::UserPrefersPassword => ProtectedKeyError::Denied,
        KeyCredentialStatus::SecurityDeviceLocked => ProtectedKeyError::Unavailable,
        KeyCredentialStatus::NotFound => ProtectedKeyError::Missing,
        other => ProtectedKeyError::Os {
            op,
            code: i64::from(other.0),
        },
    }
}

/// The credential, or `None` when Hello has no such key.
fn open(name: &HSTRING) -> Result<Option<KeyCredential>> {
    let result = KeyCredentialManager::OpenAsync(name)
        .map_err(os("open key"))?
        .join()
        .map_err(os("open key"))?;
    let status = result.Status().map_err(os("open key"))?;
    if status == KeyCredentialStatus::Success {
        return result.Credential().map(Some).map_err(os("open key"));
    }
    if status == KeyCredentialStatus::NotFound {
        return Ok(None);
    }
    Err(status_error("open key", status))
}

fn public_of(credential: &KeyCredential) -> Result<PublicKey> {
    let key = credential
        .RetrievePublicKeyWithDefaultBlobType()
        .map_err(os("public key"))?;
    Ok(PublicKey {
        algorithm: SignatureAlgorithm::RsaPkcs1Sha256,
        bytes: bytes_of(&key)?,
    })
}

pub(super) fn public_key(tag: &str) -> Result<Option<PublicKey>> {
    let name = HSTRING::from(tag);
    on_mta(move || match open(&name)? {
        Some(credential) => public_of(&credential).map(Some),
        None => Ok(None),
    })
}

pub(super) fn create(tag: &str, _protection: Protection) -> Result<PublicKey> {
    let name = HSTRING::from(tag);
    on_mta(move || {
        let supported = KeyCredentialManager::IsSupportedAsync()
            .map_err(os("hello support"))?
            .join()
            .map_err(os("hello support"))?;
        if !supported {
            return Err(ProtectedKeyError::NotEnrolled);
        }
        let result = KeyCredentialManager::RequestCreateAsync(
            &name,
            KeyCredentialCreationOption::FailIfExists,
        )
        .map_err(os("create key"))?
        .join()
        .map_err(os("create key"))?;
        let status = result.Status().map_err(os("create key"))?;
        if status == KeyCredentialStatus::Success {
            let credential = result.Credential().map_err(os("create key"))?;
            return public_of(&credential);
        }
        if status == KeyCredentialStatus::CredentialAlreadyExists {
            return match open(&name)? {
                Some(credential) => public_of(&credential),
                None => Err(ProtectedKeyError::Missing),
            };
        }
        Err(status_error("create key", status))
    })
}

/// One Hello-verified signature over `message`. `missing` is the error for a key that is not there.
fn sign_with(name: &HSTRING, message: &[u8], missing: ProtectedKeyError) -> Result<Vec<u8>> {
    let credential = open(name)?.ok_or(missing)?;
    let data = buffer(message)?;
    let result = credential
        .RequestSignAsync(&data)
        .map_err(os("sign"))?
        .join()
        .map_err(os("sign"))?;
    let status = result.Status().map_err(os("sign"))?;
    if status != KeyCredentialStatus::Success {
        return Err(status_error("sign", status));
    }
    bytes_of(&result.Result().map_err(os("sign"))?)
}

pub(super) fn sign(tag: &str, message: &[u8], _reason: &str) -> Result<Vec<u8>> {
    let name = HSTRING::from(tag);
    let message = message.to_vec();
    on_mta(move || sign_with(&name, &message, ProtectedKeyError::Missing))
}

pub(super) fn delete(tag: &str) -> Result<bool> {
    let name = HSTRING::from(tag);
    on_mta(move || {
        let existed = open(&name)?.is_some();
        if existed {
            KeyCredentialManager::DeleteAsync(&name)
                .map_err(os("delete key"))?
                .join()
                .map_err(os("delete key"))?;
        }
        Ok(existed)
    })
}

/// The fixed per-key challenge whose signature seeds the wrapping key.
fn challenge(tag: &str) -> Vec<u8> {
    let mut input = Vec::with_capacity(WRAP_INFO.len() + tag.len());
    input.extend_from_slice(WRAP_INFO);
    input.extend_from_slice(tag.as_bytes());
    rightkit_crypto::sha256(&input).to_vec()
}

fn wrapping_key(signature: &[u8], salt: &[u8], aad: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    let mut info = Vec::with_capacity(WRAP_INFO.len() + aad.len());
    info.extend_from_slice(WRAP_INFO);
    info.extend_from_slice(aad);
    rightkit_crypto::hkdf_sha256(signature, salt, &info, rightkit_crypto::aead::KEY_LEN)
        .map(Zeroizing::new)
        .map_err(|_| ProtectedKeyError::Unavailable)
}

pub(super) fn seal(tag: &str, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    let name = HSTRING::from(tag);
    let challenge = challenge(tag);
    let signature = on_mta(move || sign_with(&name, &challenge, ProtectedKeyError::Missing))?;
    let signature = Zeroizing::new(signature);
    let mut salt = [0u8; SALT_LEN];
    rightkit_crypto::random_fill(&mut salt).map_err(|_| ProtectedKeyError::Unavailable)?;
    let key = wrapping_key(&signature, &salt, aad)?;
    let sealed = rightkit_crypto::aead::seal(
        rightkit_crypto::aead::Cipher::Aes256Gcm,
        &key,
        aad,
        plaintext,
    )
    .map_err(|_| ProtectedKeyError::Unavailable)?;
    let mut out = Vec::with_capacity(1 + SALT_LEN + sealed.len());
    out.push(BLOB_VERSION);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&sealed);
    Ok(out)
}

pub(super) fn unseal(tag: &str, blob: &[u8], aad: &[u8], _reason: &str) -> Result<Vec<u8>> {
    if blob.len() < 1 + SALT_LEN || blob[0] != BLOB_VERSION {
        return Err(ProtectedKeyError::BadBlob);
    }
    let name = HSTRING::from(tag);
    let challenge = challenge(tag);
    // A blob exists only because its key did: a key Hello no longer has was invalidated.
    let signature = on_mta(move || sign_with(&name, &challenge, ProtectedKeyError::Invalidated))?;
    let signature = Zeroizing::new(signature);
    let (salt, sealed) = blob[1..].split_at(SALT_LEN);
    let key = wrapping_key(&signature, salt, aad)?;
    rightkit_crypto::aead::open(rightkit_crypto::aead::Cipher::Aes256Gcm, &key, aad, sealed)
        .map_err(|_| ProtectedKeyError::BadBlob)
}
