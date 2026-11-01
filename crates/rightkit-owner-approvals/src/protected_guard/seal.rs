//! The seal shared by macOS and Windows (see `protected_guard.rs`): HMAC-SHA-256 over the stored
//! blob, a PBKDF2 key from the host app PIN, and, on macOS, the key file. Windows keeps its own key
//! storage (DPAPI, `win.rs`) and uses only the helpers at the top.

use std::num::NonZeroU32;

use ring::rand::{SecureRandom, SystemRandom};

use super::Seal;

pub const KEY_LEN: usize = 32;

/// Whether the cached seal key is one the engine itself made or opened under the owner's PIN in
/// this process. A key read from a clear (`N`) file does not count: any process of the user can
/// plant such a file with a key of its own, so a loosened gate is trusted only under a PIN key.
static PINNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_pinned(on: bool) {
    PINNED.store(on, std::sync::atomic::Ordering::SeqCst);
}

pub fn key_from_pin() -> bool {
    PINNED.load(std::sync::atomic::Ordering::SeqCst)
}
pub const PIN_ITERATIONS: u32 = 310_000;

pub fn random<const N: usize>() -> Option<[u8; N]> {
    let mut out = [0u8; N];
    SystemRandom::new().fill(&mut out).ok()?;
    Some(out)
}

/// A 32-byte key from the PIN (PBKDF2-HMAC-SHA-256, 310,000 rounds).
pub fn derive_pin(pin: &str, salt: &[u8]) -> [u8; 32] {
    let mut derived = [0u8; 32];
    if let Some(rounds) = NonZeroU32::new(PIN_ITERATIONS) {
        ring::pbkdf2::derive(
            ring::pbkdf2::PBKDF2_HMAC_SHA256,
            rounds,
            salt,
            pin.as_bytes(),
            &mut derived,
        );
    }
    derived
}

pub fn sign(key: &[u8; KEY_LEN], message: &[u8]) -> String {
    rightkit_fs::digest::hex(&rightkit_crypto::hmac_sha256(key, message))
}

pub fn verify(key: &[u8; KEY_LEN], message: &[u8], mac: Option<&str>) -> Seal {
    let Some(mac) = mac.and_then(rightkit_fs::digest::unhex) else {
        return Seal::Unproven;
    };
    if rightkit_crypto::ct_eq(&rightkit_crypto::hmac_sha256(key, message), &mac) {
        Seal::Valid
    } else {
        Seal::Unproven
    }
}

/// macOS: the seal key in `protected-settings.seal` beside the app's data. Layout: `PSEAL2`, a
/// flag byte (`N`: the key in the clear; `P`: the key sealed with AES-256-GCM under a key derived
/// from the host app PIN), a 16-byte salt, then the 32-byte key (`N`) or a 12-byte nonce and the 48
/// bytes of ciphertext and tag (`P`); the header is the associated data. Any process of the user
/// can read or replace the file; with a PIN set the key cannot be opened without the PIN, so a
/// forged blob needs a PIN-wrapped file the engine will not open (`protected_store.rs` also
/// refuses a clear-key file once a PIN is set).
#[cfg(target_os = "macos")]
pub mod file {
    use super::{KEY_LEN, Seal, derive_pin, key_from_pin, random, set_pinned, sign, verify};
    use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
    use std::path::PathBuf;
    use std::sync::Mutex;

    const FILE_NAME: &str = "protected-settings.seal";
    const MAGIC: &[u8; 6] = b"PSEAL2";
    const SALT_LEN: usize = 16;
    const NONCE_LEN: usize = 12;
    const HEADER_LEN: usize = MAGIC.len() + 1 + SALT_LEN;
    const WRAPPED_LEN: usize = NONCE_LEN + KEY_LEN + 16;

    /// The seal key, once opened or made in this process.
    static KEY: Mutex<Option<[u8; KEY_LEN]>> = Mutex::new(None);

    fn key_slot() -> std::sync::MutexGuard<'static, Option<[u8; KEY_LEN]>> {
        KEY.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn path() -> Option<PathBuf> {
        rightkit_app_host::state_directory()
            .ok()
            .map(|dir| dir.join(FILE_NAME))
    }

    struct SealFile {
        pinned: bool,
        salt: [u8; SALT_LEN],
        body: Vec<u8>,
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
            body: bytes[HEADER_LEN..].to_vec(),
        })
    }

    fn header(pinned: bool, salt: &[u8; SALT_LEN]) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN);
        out.extend_from_slice(MAGIC);
        out.push(if pinned { b'P' } else { b'N' });
        out.extend_from_slice(salt);
        out
    }

    fn cipher(pin: &str, salt: &[u8]) -> Option<LessSafeKey> {
        let derived = derive_pin(pin, salt);
        Some(LessSafeKey::new(
            UnboundKey::new(&AES_256_GCM, &derived).ok()?,
        ))
    }

    fn write_file(key: &[u8; KEY_LEN], pin: Option<&str>) -> bool {
        let (Some(path), Some(salt)) = (path(), random::<SALT_LEN>()) else {
            return false;
        };
        let head = header(pin.is_some(), &salt);
        let body = match pin {
            None => key.to_vec(),
            Some(pin) => {
                let (Some(cipher), Some(nonce)) = (cipher(pin, &salt), random::<NONCE_LEN>())
                else {
                    return false;
                };
                let mut sealed = key.to_vec();
                if cipher
                    .seal_in_place_append_tag(
                        Nonce::assume_unique_for_key(nonce),
                        Aad::from(&head),
                        &mut sealed,
                    )
                    .is_err()
                {
                    return false;
                }
                let mut out = nonce.to_vec();
                out.extend_from_slice(&sealed);
                out
            }
        };
        let mut bytes = head;
        bytes.extend_from_slice(&body);
        rightkit_fs::atomic_write_private(&path, &bytes).is_ok()
    }

    fn open_pinned(file: &SealFile, pin: &str) -> Option<[u8; KEY_LEN]> {
        if file.body.len() != WRAPPED_LEN {
            return None;
        }
        let nonce: [u8; NONCE_LEN] = file.body[..NONCE_LEN].try_into().ok()?;
        let mut buffer = file.body[NONCE_LEN..].to_vec();
        let head = header(true, &file.salt);
        let plain: &[u8] = cipher(pin, &file.salt)?
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(&head),
                &mut buffer,
            )
            .ok()?;
        plain.try_into().ok()
    }

    /// The cached key, else the key of an `N` file (cached). A `P` file needs `pin_entered`.
    fn key_without_pin() -> Option<[u8; KEY_LEN]> {
        let mut slot = key_slot();
        if slot.is_some() {
            return *slot;
        }
        let file = read_file()?;
        if file.pinned {
            return None;
        }
        let key: [u8; KEY_LEN] = file.body.as_slice().try_into().ok()?;
        *slot = Some(key);
        Some(key)
    }

    pub fn check_seal(message: &[u8], mac: Option<&str>) -> Seal {
        match key_without_pin() {
            Some(key) => verify(&key, message, mac),
            None => Seal::Unproven,
        }
    }

    pub fn make_seal(message: &[u8], pin_set: bool) -> Option<String> {
        let key = match key_without_pin() {
            Some(key) => key,
            // No file and no PIN yet: the first run makes the key. Anything else (a PIN-bound file
            // not yet opened, or a damaged file) leaves the blob unsealed.
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
        Some(sign(&key, message))
    }

    /// The PIN was verified: opens a PIN-bound file. With no file or a clear-key (`N`) file the
    /// old key is not adopted (a planted file's key is not the engine's): a fresh key is made and
    /// wrapped under the PIN, so blobs sealed under the old key read as closed defaults and
    /// loosened gates are set again with the PIN.
    pub fn pin_entered(pin: &str) {
        if key_from_pin() {
            return;
        }
        // A `P` file that will not open under the PIN just verified (planted, damaged) is replaced.
        let opened = match read_file() {
            Some(file) if file.pinned => open_pinned(&file, pin),
            _ => None,
        };
        let key = opened.or_else(|| random::<KEY_LEN>().filter(|key| write_file(key, Some(pin))));
        if let Some(key) = key {
            *key_slot() = Some(key);
            set_pinned(true);
        }
    }

    /// The PIN was set or changed: the key is wrapped under the new PIN (a fresh key unless the
    /// cached one already came from a PIN).
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
}
