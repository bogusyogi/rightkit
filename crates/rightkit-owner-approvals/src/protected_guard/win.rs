//! Windows: the seal over the protected blob (see `protected_guard.rs`).
//!
//! `protected-settings.seal`, beside owner app's data, holds a 32-byte HMAC key under DPAPI
//! (`CryptProtectData`, user scope). Layout: `PSEAL1`, a flag byte (`N`: wrapped with the fixed
//! entropy only; `P`: the entropy also carries a key derived from the host app PIN), a 16-byte salt,
//! then the DPAPI blob. With `P` the key cannot be unwrapped until the owner's PIN has been
//! verified since the engine started (`pin_entered`), so until then a blob reads as closed
//! defaults. DPAPI protects against other users and other machines, not against code running as
//! this user: without a PIN that code can unwrap the key; with one it must guess the PIN offline.
//! Written against the `windows` crate's documented signatures (as `windows/src/clip_seal.rs`
//! uses them); not compiled on Windows yet.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::Mutex;

use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CryptProtectData, CryptUnprotectData,
};
use windows::core::PCWSTR;

use super::Seal;
use super::seal::{self, KEY_LEN, derive_pin, key_from_pin, random, set_pinned};

const FILE_NAME: &str = "protected-settings.seal";
const MAGIC: &[u8; 6] = b"PSEAL1";
const BASE_ENTROPY: &[u8] = b"pulse.protected-settings.seal.v1";
const SALT_LEN: usize = 16;
const HEADER_LEN: usize = MAGIC.len() + 1 + SALT_LEN;
/// `CRYPTPROTECT_UI_FORBIDDEN`: DPAPI never shows a prompt.
const UI_FORBIDDEN: u32 = 0x1;

/// The seal key, once unwrapped or made in this process.
static KEY: Mutex<Option<[u8; KEY_LEN]>> = Mutex::new(None);

fn key_slot() -> std::sync::MutexGuard<'static, Option<[u8; KEY_LEN]>> {
    KEY.lock().unwrap_or_else(|e| e.into_inner())
}

fn path() -> Option<PathBuf> {
    rightkit_app_host::state_directory()
        .ok()
        .map(|dir| dir.join(FILE_NAME))
}

fn copy_and_free(blob: &CRYPT_INTEGER_BLOB) -> Vec<u8> {
    // SAFETY: DPAPI returned `cbData` bytes at `pbData`; they are copied before the buffer is freed.
    let bytes = unsafe { std::slice::from_raw_parts(blob.pbData, blob.cbData as usize) }.to_vec();
    // SAFETY: the buffer came from CryptProtectData/CryptUnprotectData and is freed exactly once.
    unsafe {
        let _ = LocalFree(Some(HLOCAL(blob.pbData as *mut c_void)));
    }
    bytes
}

fn protect(plain: &[u8], entropy: &[u8]) -> Option<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(plain.len()).ok()?,
        pbData: plain.as_ptr() as *mut u8,
    };
    let extra = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(entropy.len()).ok()?,
        pbData: entropy.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: `input` and `extra` point at live slices for the call; the output buffer is owned by
    // DPAPI and released by `copy_and_free`. No prompt is allowed.
    unsafe {
        CryptProtectData(
            &input,
            PCWSTR::null(),
            Some(&extra as *const CRYPT_INTEGER_BLOB),
            None,
            None,
            UI_FORBIDDEN,
            &mut output,
        )
    }
    .ok()?;
    Some(copy_and_free(&output))
}

fn unprotect(blob: &[u8], entropy: &[u8]) -> Option<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(blob.len()).ok()?,
        pbData: blob.as_ptr() as *mut u8,
    };
    let extra = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(entropy.len()).ok()?,
        pbData: entropy.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: as in `protect`; DPAPI only reads `input` and `extra` for the call.
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            Some(&extra as *const CRYPT_INTEGER_BLOB),
            None,
            None,
            UI_FORBIDDEN,
            &mut output,
        )
    }
    .ok()?;
    Some(copy_and_free(&output))
}

/// The entropy for one wrap: the fixed part, plus a PBKDF2 key from the PIN when there is one.
fn entropy(pin: Option<&str>, salt: &[u8]) -> Vec<u8> {
    let mut out = BASE_ENTROPY.to_vec();
    if let Some(pin) = pin {
        out.extend_from_slice(&derive_pin(pin, salt));
    }
    out
}

struct SealFile {
    pinned: bool,
    salt: [u8; SALT_LEN],
    blob: Vec<u8>,
}

fn read_file() -> Option<SealFile> {
    let bytes = std::fs::read(path()?).ok()?;
    if bytes.len() <= HEADER_LEN || &bytes[..MAGIC.len()] != MAGIC {
        return None;
    }
    let pinned = match bytes[MAGIC.len()] {
        b'P' => true,
        b'N' => false,
        _ => return None,
    };
    let mut salt = [0u8; SALT_LEN];
    salt.copy_from_slice(&bytes[MAGIC.len() + 1..HEADER_LEN]);
    Some(SealFile {
        pinned,
        salt,
        blob: bytes[HEADER_LEN..].to_vec(),
    })
}

/// Writes the seal file for `key`, wrapped with the PIN's entropy when `pin` is given.
fn write_file(key: &[u8; KEY_LEN], pin: Option<&str>) -> bool {
    let (Some(path), Some(salt)) = (path(), random::<SALT_LEN>()) else {
        return false;
    };
    let Some(blob) = protect(key, &entropy(pin, &salt)) else {
        return false;
    };
    let mut bytes = Vec::with_capacity(HEADER_LEN + blob.len());
    bytes.extend_from_slice(MAGIC);
    bytes.push(if pin.is_some() { b'P' } else { b'N' });
    bytes.extend_from_slice(&salt);
    bytes.extend_from_slice(&blob);
    rightkit_fs::atomic_write_private(&path, &bytes).is_ok()
}

fn as_key(bytes: Vec<u8>) -> Option<[u8; KEY_LEN]> {
    bytes.try_into().ok()
}

/// The cached key, else the key of an `N` seal file (cached). A `P` file needs `pin_entered`.
fn key_without_pin() -> Option<[u8; KEY_LEN]> {
    let mut slot = key_slot();
    if slot.is_some() {
        return *slot;
    }
    let file = read_file()?;
    if file.pinned {
        return None;
    }
    let key = as_key(unprotect(&file.blob, &entropy(None, &file.salt))?)?;
    *slot = Some(key);
    Some(key)
}

pub fn check_seal(message: &[u8], mac: Option<&str>) -> Seal {
    match key_without_pin() {
        Some(key) => seal::verify(&key, message, mac),
        None => Seal::Unproven,
    }
}

pub fn make_seal(message: &[u8], pin_set: bool) -> Option<String> {
    let key = match key_without_pin() {
        Some(key) => key,
        // No seal file and no PIN yet: the first run makes the key. Anything else (a PIN-bound
        // file not yet opened, or a damaged file) leaves the blob unsealed.
        None if !pin_set && read_file().is_none() => {
            let key = random::<KEY_LEN>()?;
            if !write_file(&key, None) {
                return None;
            }
            *key_slot() = Some(key);
            key
        }
        None => return None,
    };
    Some(seal::sign(&key, message))
}

/// With no file or a clear (`N`) file the old key is not adopted (any process of the user can plant
/// one with `BASE_ENTROPY`): a fresh key is wrapped under the PIN, so blobs sealed under the old
/// key read as closed defaults and loosened gates are set again with the PIN.
pub fn pin_entered(pin: &str) {
    if key_from_pin() {
        return;
    }
    // A `P` file that will not open under the PIN just verified (damaged, DPAPI key changed) is replaced.
    let opened = match read_file() {
        Some(file) if file.pinned => {
            unprotect(&file.blob, &entropy(Some(pin), &file.salt)).and_then(as_key)
        }
        _ => None,
    };
    let key = opened.or_else(|| random::<KEY_LEN>().filter(|key| write_file(key, Some(pin))));
    if let Some(key) = key {
        *key_slot() = Some(key);
        set_pinned(true);
    }
}

pub fn pin_changed(pin: &str) {
    let key = if key_from_pin() { *key_slot() } else { None };
    let Some(key) = key.or_else(random::<KEY_LEN>) else {
        return;
    };
    if write_file(&key, Some(pin)) {
        *key_slot() = Some(key);
        set_pinned(true);
    }
}

pub fn pin_bound() -> bool {
    read_file().is_some_and(|file| file.pinned)
}

pub fn reset_seal() {
    *key_slot() = None;
    set_pinned(false);
    if let Some(path) = path() {
        let _ = std::fs::remove_file(path);
    }
}
