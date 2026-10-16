//! Durable, cross-process pending-authorization store for CLI/engine flows where `begin` and
//! `complete` run in different processes. Each record is one AES-256-GCM sealed file named by
//! the hex SHA-256 of the (already hashed) state key, so a stolen directory listing reveals
//! nothing usable and the PKCE verifier is never plaintext at rest. All mutation runs under a
//! directory lock shared by every process using the same directory. `take` deletes the record
//! before it decrypts it, so a state can be consumed at most once even if the file is corrupt.
use super::lock;
use super::{PendingAuthorization, PendingStateStore, ServerAuthError};
use ring::aead::{self, Aad, LessSafeKey, Nonce, UnboundKey};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use zeroize::Zeroizing;

const MAGIC: &[u8] = b"RKOP1";
const HEADER: usize = 5 + 8 + 12;
const TAG: usize = 16;
const MAX_RECORD_BYTES: u64 = 64 * 1024;
const LOCK_NAME: &str = "pending.lock";
const EXT: &str = ".pending";

/// File-backed [`PendingStateStore`]. Share one directory (and one key) between the process
/// that calls `begin` and the one that calls `complete`. Use a per-user directory: on Unix it
/// must be (or is created as) mode 0700 and owned by the effective user; on Windows it inherits
/// its parent's ACL, so place it under `%LOCALAPPDATA%`. Records are encrypted regardless; the
/// 32-byte key must not live beside them. [`Self::with_keychain_key`] keeps it in the OS
/// credential store.
pub struct FilePendingStateStore {
    root: PathBuf,
    key: LessSafeKey,
    capacity: usize,
    lock_timeout: Duration,
}

impl std::fmt::Debug for FilePendingStateStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FilePendingStateStore { [redacted] }")
    }
}

impl FilePendingStateStore {
    pub fn new(root: impl AsRef<Path>, key: [u8; 32]) -> Result<Self, ServerAuthError> {
        let key = Zeroizing::new(key);
        let root = lock::prepare_dir(root.as_ref())?;
        let key = LessSafeKey::new(
            UnboundKey::new(&aead::AES_256_GCM, &*key).map_err(|_| ServerAuthError::Storage)?,
        );
        Ok(Self {
            root,
            key,
            capacity: 1024,
            lock_timeout: Duration::from_secs(10),
        })
    }

    /// Like [`Self::new`], with the sealing key held in the OS credential store under
    /// `key_account`: minted once (32 CSPRNG bytes) with add-if-absent, so concurrent first
    /// runs adopt one key. A stored value that is unreadable or malformed is an error, never
    /// replaced (replacing it would strand every pending record).
    #[cfg(feature = "secrets")]
    pub fn with_keychain_key(
        root: impl AsRef<Path>,
        credentials: &rightkit_credentials::Store,
        key_account: &str,
    ) -> Result<Self, ServerAuthError> {
        Self::new(root, keychain_key(credentials, key_account)?)
    }

    pub fn with_capacity(mut self, capacity: usize) -> Self {
        self.capacity = capacity;
        self
    }
    pub fn with_lock_timeout(mut self, timeout: Duration) -> Self {
        self.lock_timeout = timeout;
        self
    }

    fn path(&self, stem: &str) -> PathBuf {
        self.root.join(format!("{stem}{EXT}"))
    }
}

#[cfg(feature = "secrets")]
fn keychain_key(
    credentials: &rightkit_credentials::Store,
    account: &str,
) -> Result<[u8; 32], ServerAuthError> {
    use rightkit_credentials::Secret as CredSecret;
    let parse = |secret: &CredSecret| -> Result<[u8; 32], ServerAuthError> {
        let text = secret.expose_str().map_err(|_| ServerAuthError::Storage)?;
        let bytes = Zeroizing::new(lock::unhex(text).ok_or(ServerAuthError::Storage)?);
        <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| ServerAuthError::Storage)
    };
    if let Some(existing) = credentials
        .get(account)
        .map_err(|_| ServerAuthError::Storage)?
    {
        return parse(&existing);
    }
    let mut raw = Zeroizing::new([0u8; 32]);
    getrandom::fill(&mut *raw).map_err(|_| ServerAuthError::Storage)?;
    let fresh = CredSecret::from_string(lock::hex(&*raw));
    if credentials
        .add_if_absent(account, &fresh)
        .map_err(|_| ServerAuthError::Storage)?
    {
        return Ok(*raw);
    }
    let winner = credentials
        .get(account)
        .map_err(|_| ServerAuthError::Storage)?
        .ok_or(ServerAuthError::Storage)?;
    parse(&winner)
}

fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 128
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn aad(stem: &str, expires: &[u8]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(stem.len() + expires.len());
    aad.extend_from_slice(stem.as_bytes());
    aad.extend_from_slice(expires);
    aad
}

fn read_record(path: &Path) -> Result<Option<Zeroizing<Vec<u8>>>, ServerAuthError> {
    #[cfg(unix)]
    let opened = super::store::open_existing_private_file(path);
    #[cfg(not(unix))]
    let opened = match fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_file() => return Err(ServerAuthError::Storage),
        Ok(_) => File::open(path),
        Err(e) => Err(e),
    };
    let file = match opened {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ServerAuthError::Storage),
    };
    let meta = file.metadata().map_err(|_| ServerAuthError::Storage)?;
    #[cfg(unix)]
    super::store::check_private_file(&meta)?;
    if !meta.is_file() || meta.len() > MAX_RECORD_BYTES {
        return Err(ServerAuthError::Storage);
    }
    let mut bytes = Zeroizing::new(Vec::with_capacity(meta.len() as usize));
    file.take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ServerAuthError::Storage)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(ServerAuthError::Storage);
    }
    Ok(Some(bytes))
}

fn create_exclusive(path: &Path) -> Result<File, ServerAuthError> {
    #[cfg(unix)]
    {
        super::store::private_file(path, true)
    }
    #[cfg(not(unix))]
    {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|_| ServerAuthError::Storage)
    }
}

fn sync_dir(root: &Path) -> Result<(), ServerAuthError> {
    #[cfg(unix)]
    {
        File::open(root)
            .and_then(|f| f.sync_all())
            .map_err(|_| ServerAuthError::Storage)
    }
    #[cfg(not(unix))]
    {
        // Directories cannot be opened for sync on Windows; rename is durable enough there.
        let _ = root;
        Ok(())
    }
}

impl PendingStateStore for FilePendingStateStore {
    fn insert(
        &self,
        key: &str,
        pending: PendingAuthorization,
        now: SystemTime,
    ) -> Result<(), ServerAuthError> {
        if !valid_key(key) {
            return Err(ServerAuthError::InvalidConfig);
        }
        let _lock = lock::acquire(&self.root, LOCK_NAME, self.lock_timeout)?;
        // Purge expired and unreadable records and any stranded temp files (writers hold the
        // lock, so no temp file belongs to a live writer), then count what remains.
        let mut live = 0usize;
        let now_secs = unix_secs(now);
        for entry in fs::read_dir(&self.root).map_err(|_| ServerAuthError::Storage)? {
            let entry = entry.map_err(|_| ServerAuthError::Storage)?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let path = entry.path();
            if name.ends_with(".tmp") {
                let _ = fs::remove_file(&path);
                continue;
            }
            if !name.ends_with(EXT) {
                continue;
            }
            let keep = match read_record(&path) {
                Ok(Some(bytes)) => {
                    bytes.len() >= HEADER + TAG
                        && bytes.starts_with(MAGIC)
                        && u64::from_be_bytes(bytes[5..13].try_into().unwrap_or([0; 8])) > now_secs
                }
                _ => false,
            };
            if keep {
                live += 1;
            } else {
                fs::remove_file(&path).map_err(|_| ServerAuthError::Storage)?;
            }
        }
        if live >= self.capacity {
            return Err(ServerAuthError::PendingCapacity);
        }
        let stem = lock::hex_digest(key);
        let target = self.path(&stem);
        if fs::symlink_metadata(&target).is_ok() {
            return Err(ServerAuthError::StateMismatch);
        }
        let expires = unix_secs(pending.expires_at).to_be_bytes();
        let mut body =
            Zeroizing::new(serde_json::to_vec(&pending).map_err(|_| ServerAuthError::Storage)?);
        let mut nonce = [0u8; 12];
        getrandom::fill(&mut nonce).map_err(|_| ServerAuthError::Storage)?;
        self.key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad(&stem, &expires)),
                &mut *body,
            )
            .map_err(|_| ServerAuthError::Storage)?;
        if (HEADER + body.len()) as u64 > MAX_RECORD_BYTES {
            return Err(ServerAuthError::Storage);
        }
        let temp = self.root.join(format!("{stem}.{}.tmp", super::random(16)?));
        let result = (|| {
            let mut file = create_exclusive(&temp)?;
            file.write_all(MAGIC)
                .and_then(|_| file.write_all(&expires))
                .and_then(|_| file.write_all(&nonce))
                .and_then(|_| file.write_all(&body))
                .and_then(|_| file.sync_all())
                .map_err(|_| ServerAuthError::Storage)?;
            drop(file);
            fs::rename(&temp, &target).map_err(|_| ServerAuthError::Storage)?;
            sync_dir(&self.root)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }

    fn take(&self, key: &str) -> Result<Option<PendingAuthorization>, ServerAuthError> {
        if !valid_key(key) {
            return Ok(None);
        }
        let _lock = lock::acquire(&self.root, LOCK_NAME, self.lock_timeout)?;
        let stem = lock::hex_digest(key);
        let path = self.path(&stem);
        let Some(mut bytes) = read_record(&path)? else {
            return Ok(None);
        };
        // Consume first: a failed decrypt must not leave the state replayable, and a delete
        // that fails must not hand the record out.
        fs::remove_file(&path).map_err(|_| ServerAuthError::Storage)?;
        sync_dir(&self.root)?;
        if bytes.len() < HEADER + TAG || !bytes.starts_with(MAGIC) {
            return Err(ServerAuthError::Storage);
        }
        let expires: [u8; 8] = bytes[5..13]
            .try_into()
            .map_err(|_| ServerAuthError::Storage)?;
        let nonce: [u8; 12] = bytes[13..HEADER]
            .try_into()
            .map_err(|_| ServerAuthError::Storage)?;
        let plaintext = self
            .key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad(&stem, &expires)),
                &mut bytes[HEADER..],
            )
            .map_err(|_| ServerAuthError::Storage)?;
        let record: PendingAuthorization =
            serde_json::from_slice(plaintext).map_err(|_| ServerAuthError::Storage)?;
        Ok(Some(record))
    }
}
