//! Framework-agnostic native host services for Right Suite Tauri applications.
//!
//! This crate owns storage ordering, atomic device identity, signed entitlement
//! authorization, paid-command guards, and typed log/diagnostic bridges. Apps
//! retain their platform keyring implementations, `#[tauri::command]` wrappers,
//! managed-state registration, command names, and product-specific side effects.

pub use rightkit_license::LicenseRequirement;
use rightkit_license::{verify_token, Entitlement, VerifyError};
use std::{
    marker::PhantomData,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

pub trait SecureStore<T> {
    fn get(&self) -> Result<Option<T>, String>;
    fn set(&self, value: &T) -> Result<(), String>;
    fn delete(&self) -> Result<(), String>;

    /// Stores `value` only when nothing is stored yet; returns whether it did.
    /// Stores with a native create-new primitive (O_EXCL, keyring add) should
    /// override this so creation is atomic across processes. The default is
    /// check-then-set and relies on the caller's cross-process lock.
    fn set_if_absent(&self, value: &T) -> Result<bool, String> {
        if self.get()?.is_some() {
            return Ok(false);
        }
        self.set(value)?;
        Ok(true)
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

pub trait LicenseRecord {
    fn signed_token(&self) -> Option<&str>;
}

#[derive(Debug, PartialEq, Eq)]
pub enum AuthorizationDenial {
    MissingLicense,
    MissingToken,
    MissingDevice,
    InvalidToken(VerifyError),
    RequirementNotMet(LicenseRequirement),
}

#[derive(Debug, PartialEq, Eq)]
pub enum AuthorizationError {
    Storage(StorageError),
    Clock(String),
    Denied(AuthorizationDenial),
}

impl From<StorageError> for AuthorizationError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl std::fmt::Display for AuthorizationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(error) => error.fmt(formatter),
            Self::Clock(error) => write!(formatter, "authorization clock failed: {error}"),
            Self::Denied(denial) => write!(formatter, "license authorization denied: {denial:?}"),
        }
    }
}

impl std::error::Error for AuthorizationError {}

pub struct AuthorizationService<'a, R, L, D> {
    licenses: &'a L,
    devices: &'a DeviceService<D>,
    public_key: &'a [u8; 32],
    expected_app: &'a str,
    record: PhantomData<fn() -> R>,
}

impl<'a, R, L, D> AuthorizationService<'a, R, L, D>
where
    R: LicenseRecord,
    L: SecureStore<R>,
    D: SecureStore<String>,
{
    pub fn new(
        licenses: &'a L,
        devices: &'a DeviceService<D>,
        public_key: &'a [u8; 32],
        expected_app: &'a str,
    ) -> Self {
        Self {
            licenses,
            devices,
            public_key,
            expected_app,
            record: PhantomData,
        }
    }

    pub fn authorize(
        &self,
        requirement: LicenseRequirement,
    ) -> Result<Entitlement, AuthorizationError> {
        self.authorize_with_clock(requirement, current_unix_time)
    }

    /// Like [`authorize`](Self::authorize) with the host's trusted time source
    /// (for example `AntiRollbackClock::trusted_now`) in place of the wall
    /// clock. A clock failure is `AuthorizationError::Clock` and denies; the
    /// clock is read once, before any store is touched.
    pub fn authorize_with_clock(
        &self,
        requirement: LicenseRequirement,
        clock: impl FnOnce() -> Result<i64, String>,
    ) -> Result<Entitlement, AuthorizationError> {
        let now_unix = clock().map_err(AuthorizationError::Clock)?;
        let license = self
            .licenses
            .get()
            .map_err(|error| StorageError::new(StorageOperation::Get, error))?
            .ok_or(AuthorizationError::Denied(
                AuthorizationDenial::MissingLicense,
            ))?;
        let token = license.signed_token().ok_or(AuthorizationError::Denied(
            AuthorizationDenial::MissingToken,
        ))?;
        let device_id = self.devices.get()?.ok_or(AuthorizationError::Denied(
            AuthorizationDenial::MissingDevice,
        ))?;
        let entitlement = verify_token(
            token,
            self.public_key,
            self.expected_app,
            &device_id,
            now_unix,
        )
        .map_err(|error| AuthorizationError::Denied(AuthorizationDenial::InvalidToken(error)))?;
        if !entitlement.satisfies(&requirement) {
            return Err(AuthorizationError::Denied(
                AuthorizationDenial::RequirementNotMet(requirement),
            ));
        }
        Ok(entitlement)
    }
}

fn current_unix_time() -> Result<i64, String> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_secs();
    i64::try_from(seconds).map_err(|error| error.to_string())
}

#[derive(Debug, PartialEq, Eq)]
pub enum PaidCommandError<E> {
    Authorization(AuthorizationError),
    Command(E),
}

pub fn guard_paid_command<R, L, D, T, E>(
    authorization: &AuthorizationService<'_, R, L, D>,
    requirement: LicenseRequirement,
    command: impl FnOnce(&Entitlement) -> Result<T, E>,
) -> Result<T, PaidCommandError<E>>
where
    R: LicenseRecord,
    L: SecureStore<R>,
    D: SecureStore<String>,
{
    let entitlement = authorization
        .authorize(requirement)
        .map_err(PaidCommandError::Authorization)?;
    command(&entitlement).map_err(PaidCommandError::Command)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostOperation {
    WriteLog,
    CollectLogs,
    ClearLogs,
    CollectDiagnostics,
    ClearDiagnostics,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostOperationError {
    pub operation: HostOperation,
    pub message: String,
}

impl HostOperationError {
    fn new(operation: HostOperation, message: impl Into<String>) -> Self {
        Self {
            operation,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for HostOperationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "host {:?} failed: {}",
            self.operation, self.message
        )
    }
}

impl std::error::Error for HostOperationError {}

pub trait LogHost<Entry, Bundle> {
    fn write(&self, entry: &Entry) -> Result<(), String>;
    fn collect(&self) -> Result<Bundle, String>;
    fn clear(&self) -> Result<(), String>;
}

pub struct LogService<'a, H, Entry, Bundle> {
    host: &'a H,
    types: PhantomData<fn(Entry) -> Bundle>,
}

impl<'a, H, Entry, Bundle> LogService<'a, H, Entry, Bundle>
where
    H: LogHost<Entry, Bundle>,
{
    pub fn new(host: &'a H) -> Self {
        Self {
            host,
            types: PhantomData,
        }
    }
    pub fn write(&self, entry: &Entry) -> Result<(), HostOperationError> {
        self.host
            .write(entry)
            .map_err(|error| HostOperationError::new(HostOperation::WriteLog, error))
    }
    pub fn collect(&self) -> Result<Bundle, HostOperationError> {
        self.host
            .collect()
            .map_err(|error| HostOperationError::new(HostOperation::CollectLogs, error))
    }
    pub fn clear(&self) -> Result<(), HostOperationError> {
        self.host
            .clear()
            .map_err(|error| HostOperationError::new(HostOperation::ClearLogs, error))
    }
}

pub trait DiagnosticHost<Bundle> {
    fn collect(&self) -> Result<Bundle, String>;
    fn clear(&self) -> Result<(), String>;
}

pub struct DiagnosticService<'a, H, Bundle> {
    host: &'a H,
    bundle: PhantomData<fn() -> Bundle>,
}

impl<'a, H, Bundle> DiagnosticService<'a, H, Bundle>
where
    H: DiagnosticHost<Bundle>,
{
    pub fn new(host: &'a H) -> Self {
        Self {
            host,
            bundle: PhantomData,
        }
    }
    pub fn collect(&self) -> Result<Bundle, HostOperationError> {
        self.host
            .collect()
            .map_err(|error| HostOperationError::new(HostOperation::CollectDiagnostics, error))
    }
    pub fn clear(&self) -> Result<(), HostOperationError> {
        self.host
            .clear()
            .map_err(|error| HostOperationError::new(HostOperation::ClearDiagnostics, error))
    }
}

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
    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
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

#[cfg(test)]
mod device_id_hash_tests {
    use super::device_id_hash;

    #[test]
    fn matches_fips_vectors() {
        assert_eq!(
            device_id_hash(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            device_id_hash("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            device_id_hash("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn is_lowercase_hex_and_input_exact() {
        let hash = device_id_hash("dev-1");
        assert_eq!(hash.len(), 64);
        assert!(hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        assert_ne!(hash, device_id_hash("Dev-1"));
        assert_ne!(hash, device_id_hash("dev-1 "));
        assert_ne!(hash, "dev-1");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    const PUBKEY: [u8; 32] = [
        108, 142, 220, 5, 154, 87, 124, 180, 244, 31, 25, 204, 0, 155, 84, 165, 16, 227, 134, 87,
        61, 66, 199, 238, 168, 219, 128, 145, 39, 235, 80, 211,
    ];
    const TOKEN: &str = "eyJ2IjoyLCJpZCI6ImxpY18xIiwiYXBwIjoiaGVhcmRyaWdodCIsInRpZXIiOiJwcm8iLCJmZWF0dXJlcyI6WyJ3YWtlX3dvcmQiLCJ2b2ljZV9jb21tYW5kcyJdLCJkZXZpY2VfaWQiOiJkZXYtMSIsImlhdCI6MTAwMCwiZXhwIjo5OTk5OTk5OTk5fQ.JNmWEwG16VehpwJz9SooQIia0k1Ht0c150jca86h7sAnFj9tKuPO8yZhfyR48vx3Yd_03RQyYYtbKm08TCgsBg";

    #[derive(Clone)]
    struct TestLicense;

    impl LicenseRecord for TestLicense {
        fn signed_token(&self) -> Option<&str> {
            Some(TOKEN)
        }
    }

    struct LicenseStore;

    impl SecureStore<TestLicense> for LicenseStore {
        fn get(&self) -> Result<Option<TestLicense>, String> {
            Ok(Some(TestLicense))
        }

        fn set(&self, _value: &TestLicense) -> Result<(), String> {
            Ok(())
        }

        fn delete(&self) -> Result<(), String> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct TestStore(Mutex<Option<String>>);

    impl SecureStore<String> for TestStore {
        fn get(&self) -> Result<Option<String>, String> {
            Ok(self.0.lock().unwrap().clone())
        }

        fn set(&self, value: &String) -> Result<(), String> {
            *self.0.lock().unwrap() = Some(value.clone());
            Ok(())
        }

        fn delete(&self) -> Result<(), String> {
            *self.0.lock().unwrap() = None;
            Ok(())
        }
    }

    #[test]
    fn device_creation_fails_closed_with_typed_rng_error() {
        let service = DeviceService::new(TestStore::default());

        let error = service
            .get_or_create_with_rng(|_bytes| Err("rng unavailable"))
            .unwrap_err();

        assert_eq!(error.operation, StorageOperation::GenerateDeviceId);
        assert_eq!(error.message, "rng unavailable");
        assert_eq!(service.get().unwrap(), None);
    }

    #[test]
    fn device_creation_fails_closed_when_creation_lock_is_poisoned() {
        let service = DeviceService::new(TestStore::default());
        let poisoned = catch_unwind(AssertUnwindSafe(|| {
            let _guard = service.creation.lock().unwrap();
            panic!("poison creation lock");
        }));
        assert!(poisoned.is_err());

        let error = service.get_or_create().unwrap_err();

        assert_eq!(error.operation, StorageOperation::DeviceLock);
        assert_eq!(error.message, "device lock poisoned");
        assert_eq!(service.get().unwrap(), None);
    }

    #[test]
    fn private_clock_seam_supports_deterministic_expiry_tests() {
        let licenses = LicenseStore;
        let devices = DeviceService::new(TestStore(Mutex::new(Some("dev-1".into()))));
        let authorization = AuthorizationService::new(&licenses, &devices, &PUBKEY, "heardright");

        let error = authorization
            .authorize_with_clock(LicenseRequirement::pro(), || Ok(10_000_000_000))
            .unwrap_err();

        assert!(matches!(
            error,
            AuthorizationError::Denied(AuthorizationDenial::InvalidToken(
                VerifyError::Expired { .. }
            ))
        ));
    }

    struct FileStore(std::path::PathBuf);

    impl SecureStore<String> for FileStore {
        fn get(&self) -> Result<Option<String>, String> {
            match std::fs::read_to_string(&self.0) {
                Ok(text) if text.is_empty() => Ok(None),
                Ok(text) => Ok(Some(text)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error.to_string()),
            }
        }

        fn set(&self, value: &String) -> Result<(), String> {
            // Widen the check-then-set window so an unlocked race is visible.
            std::thread::sleep(std::time::Duration::from_millis(20));
            std::fs::write(&self.0, value).map_err(|error| error.to_string())
        }

        fn delete(&self) -> Result<(), String> {
            std::fs::remove_file(&self.0).map_err(|error| error.to_string())
        }
    }

    #[test]
    fn concurrent_creators_across_service_instances_agree_on_one_device_id() {
        let dir = std::env::temp_dir().join(
            format!(
                "rightkit-tauri-device-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
            )
            .replace([' ', ':', '{', '}', '"'], "_"),
        );
        std::fs::create_dir_all(&dir).unwrap();
        let (id_path, lock_path) = (dir.join("device-id"), dir.join("device-id.lock"));

        // Separate services stand in for separate processes: each has its own
        // in-process mutex, so only the file lock can serialize them.
        let ids: Vec<String> = (0..8)
            .map(|_| {
                let (id_path, lock_path) = (id_path.clone(), lock_path.clone());
                std::thread::spawn(move || {
                    DeviceService::with_lock_file(FileStore(id_path), lock_path)
                        .get_or_create()
                        .unwrap()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();

        assert!(ids.windows(2).all(|pair| pair[0] == pair[1]), "{ids:?}");
        assert_eq!(std::fs::read_to_string(&id_path).unwrap(), ids[0]);
        std::fs::remove_dir_all(&dir).ok();
    }
}
