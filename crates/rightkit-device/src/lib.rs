//! Framework-agnostic native host services for Right Suite Tauri applications.
//!
//! This crate owns storage ordering and atomic device identity. Apps
//! retain their platform keyring implementations, `#[tauri::command]` wrappers,
//! managed-state registration, command names, and product-specific side effects.
//! With the opt-in `credentials` feature, `credentials::CredentialStore` supplies
//! an atomic OS-store adapter while apps retain namespace, account & access policy.

#[cfg(feature = "credentials")]
pub mod credentials;

use std::{marker::PhantomData, sync::Mutex};

pub trait SecureStore<T> {
    fn get(&self) -> Result<Option<T>, String>;
    fn set(&self, value: &T) -> Result<(), String>;
    fn delete(&self) -> Result<(), String>;

    /// Stores `value` only when nothing is stored yet; returns whether it did.
    ///
    /// Implementations must make check-and-store one atomic step against every other
    /// writer of the underlying store (a native create-new primitive such as O_EXCL or a
    /// keyring add-if-absent, or one lock every writer of that store takes). The default
    /// fails closed: a check-then-set default would let two creators both observe
    /// "absent" and the later `set` silently overwrite the winner while reporting
    /// success, so a store that cannot create atomically must say so instead. Callers
    /// such as [`DeviceService::get_or_create`] surface that refusal as an error and
    /// mint nothing.
    fn set_if_absent(&self, _value: &T) -> Result<bool, String> {
        Err(
            "set_if_absent is not implemented atomically for this store; \
             refusing a check-then-set create"
                .to_string(),
        )
    }
}

/// Post-commit notifications are deliberately infallible: storage has already
/// committed, so reporting the whole mutation as failed would be untruthful.
pub trait MutationHook<T> {
    fn after_set(&self, value: &T);
    fn after_delete(&self);
}

pub struct NoopHook;

impl<T> MutationHook<T> for NoopHook {
    fn after_set(&self, _value: &T) {}
    fn after_delete(&self) {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageOperation {
    Get,
    Set,
    Delete,
    DeviceLock,
    GenerateDeviceId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageError {
    pub operation: StorageOperation,
    pub message: String,
}

impl StorageError {
    fn new(operation: StorageOperation, message: impl Into<String>) -> Self {
        Self {
            operation,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "secure {:?} failed: {}",
            self.operation, self.message
        )
    }
}

impl std::error::Error for StorageError {}

pub struct SecureService<'a, T, S, H> {
    store: &'a S,
    hook: &'a H,
    value: PhantomData<fn() -> T>,
}

impl<'a, T, S, H> SecureService<'a, T, S, H>
where
    S: SecureStore<T>,
    H: MutationHook<T>,
{
    pub fn new(store: &'a S, hook: &'a H) -> Self {
        Self {
            store,
            hook,
            value: PhantomData,
        }
    }

    pub fn get(&self) -> Result<Option<T>, StorageError> {
        self.store
            .get()
            .map_err(|error| StorageError::new(StorageOperation::Get, error))
    }

    pub fn set(&self, value: &T) -> Result<(), StorageError> {
        self.store
            .set(value)
            .map_err(|error| StorageError::new(StorageOperation::Set, error))?;
        self.hook.after_set(value);
        Ok(())
    }

    pub fn delete(&self) -> Result<(), StorageError> {
        self.store
            .delete()
            .map_err(|error| StorageError::new(StorageOperation::Delete, error))?;
        self.hook.after_delete();
        Ok(())
    }
}

/// One instance belongs in each app's managed native state. Its mutex makes the
/// read/create/write transaction atomic for every command using that state.
pub struct DeviceService<S> {
    store: S,
    creation: Mutex<()>,
    lock_path: Option<std::path::PathBuf>,
}

impl<S> DeviceService<S> {
    pub fn new(store: S) -> Self {
        Self {
            store,
            creation: Mutex::new(()),
            lock_path: None,
        }
    }

    /// Like `new`, and also serializes creation across processes (window +
    /// engine, repeated launches) with an exclusive advisory lock on `lock_path`.
    pub fn with_lock_file(store: S, lock_path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            lock_path: Some(lock_path.into()),
            ..Self::new(store)
        }
    }
}

impl<S> DeviceService<S>
where
    S: SecureStore<String>,
{
    pub fn get(&self) -> Result<Option<String>, StorageError> {
        self.store
            .get()
            .map_err(|error| StorageError::new(StorageOperation::Get, error))
    }

    pub fn get_or_create(&self) -> Result<String, StorageError> {
        self.get_or_create_with_rng(getrandom::fill)
    }

    fn get_or_create_with_rng<E>(
        &self,
        fill_random: impl FnOnce(&mut [u8]) -> Result<(), E>,
    ) -> Result<String, StorageError>
    where
        E: std::fmt::Display,
    {
        let _guard = self
            .creation
            .lock()
            .map_err(|_| StorageError::new(StorageOperation::DeviceLock, "device lock poisoned"))?;
        let _file_lock = match &self.lock_path {
            Some(path) => Some(
                std::fs::OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .write(true)
                    .open(path)
                    .and_then(|file| file.lock().map(|()| file))
                    .map_err(|error| {
                        StorageError::new(StorageOperation::DeviceLock, error.to_string())
                    })?,
            ),
            None => None,
        };
        if let Some(existing) = self.get()? {
            return Ok(existing);
        }
        let created = generate_device_id(fill_random)?;
        let stored = self
            .store
            .set_if_absent(&created)
            .map_err(|error| StorageError::new(StorageOperation::Set, error))?;
        if stored {
            return Ok(created);
        }
        // Another creator won: adopt its ID rather than overwrite it.
        self.get()?.ok_or_else(|| {
            StorageError::new(
                StorageOperation::Get,
                "device id vanished after create race",
            )
        })
    }
}

fn generate_device_id<E>(
    fill_random: impl FnOnce(&mut [u8]) -> Result<(), E>,
) -> Result<String, StorageError>
where
    E: std::fmt::Display,
{
    let mut bytes = [0_u8; 16];
    fill_random(&mut bytes).map_err(|error| {
        StorageError::new(StorageOperation::GenerateDeviceId, error.to_string())
    })?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    ))
}

// Deprecated since 0.4.2: the log and diagnostic bridges moved to `rightkit-logs`
// (`rightkit_logs::{LogHost, LogService, DiagnosticHost, DiagnosticService, HostOperation,
// HostOperationError}`). rustc ignores `#[deprecated]` on a re-export, so this is a compatibility
// re-export only; it goes in the next release.
pub use rightkit_logs::{
    DiagnosticHost, DiagnosticService, HostOperation, HostOperationError, LogHost, LogService,
};

/// Lowercase hex SHA-256 of the raw device id.
///
/// This is the only form of the device id that leaves the machine for lease, link and Pro-bridge
/// endpoints (`LeaseEnvelope::device_id_hash` carries it). The raw id never goes on the wire. The
/// digest is computed over the UTF-8 bytes of `raw` exactly as given (no trimming or case
/// folding), so the signer and the client agree by construction.
pub fn device_id_hash(raw: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = sha256(raw.as_bytes());
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push(HEX[usize::from(byte >> 4)] as char);
        out.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    out
}

/// FIPS 180-4 SHA-256, kept local so the device-id definition needs no extra dependency.
fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64).wrapping_mul(8)).to_be_bytes());
    for chunk in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, word) in chunk.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v[7] = v[6];
            v[6] = v[5];
            v[5] = v[4];
            v[4] = v[3].wrapping_add(t1);
            v[3] = v[2];
            v[2] = v[1];
            v[1] = v[0];
            v[0] = t1.wrapping_add(t2);
        }
        for (slot, add) in h.iter_mut().zip(v) {
            *slot = slot.wrapping_add(add);
        }
    }
    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}
