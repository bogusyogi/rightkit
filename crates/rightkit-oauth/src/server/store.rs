//! Linux/macOS reference persistence: AES-256-GCM, authenticated account/provider binding,
//! atomic replacement and OS file locks. Key provisioning belongs to the daemon.
use super::{digest, ServerAuthError};
use crate::TokenSet;
use ring::aead::{self, Aad, LessSafeKey, Nonce, UnboundKey};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

/// Encrypt tokens at rest, separate encryption keys from data/backups, and never log values.
/// All methods isolate `(account, provider)`. Implementations MUST serialize get/refresh/put
/// across processes via `lock`, atomically replacing the entire token set (rotation included).
/// A database adapter can use a row/advisory lock; a distributed adapter needs a fenced
/// lease spanning the exchange, not merely compare-and-swap after spending a refresh token.
pub trait TokenStore: Send + Sync {
    fn lock(
        &self,
        account: &str,
        provider: &str,
    ) -> Result<Box<dyn TokenStoreGuard + '_>, ServerAuthError>;
    fn get(&self, account: &str, provider: &str) -> Result<Option<TokenSet>, ServerAuthError> {
        self.lock(account, provider)?.get()
    }
    fn put(&self, account: &str, provider: &str, tokens: &TokenSet) -> Result<(), ServerAuthError> {
        self.lock(account, provider)?.put(tokens)
    }
    fn delete(&self, account: &str, provider: &str) -> Result<(), ServerAuthError> {
        self.lock(account, provider)?.delete()
    }
}

/// Dropping this guard releases the lock. Methods operate on its bound account/provider.
pub trait TokenStoreGuard {
    fn get(&mut self) -> Result<Option<TokenSet>, ServerAuthError>;
    fn put(&mut self, tokens: &TokenSet) -> Result<(), ServerAuthError>;
    fn delete(&mut self) -> Result<(), ServerAuthError>;
}

/// Reference store for Unix daemons. Requires a trusted, host-controlled parent directory,
/// local filesystem with reliable advisory locks/atomic rename, and a private 0700 root.
/// Provision a 32-byte random key via a secret manager/systemd credential, never beside
/// ciphertext. No implicit key generation, plaintext fallback, or desktop keychain.
/// Windows construction fails closed: use an adapter with protected ACLs/credential storage.
/// Processes sharing this directory must use the same key. No distributed/NFS guarantees.
pub struct SealedFileTokenStore {
    root: PathBuf,
    key: LessSafeKey,
    lock_timeout: Duration,
}
impl std::fmt::Debug for SealedFileTokenStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SealedFileTokenStore { [redacted] }")
    }
}
const MAX_SEALED_BYTES: u64 = 1024 * 1024;
const MAGIC: &[u8] = b"RKOT1";

impl SealedFileTokenStore {
    pub fn new(root: impl AsRef<Path>, key: [u8; 32]) -> Result<Self, ServerAuthError> {
        let key = Zeroizing::new(key);
        private_dir(root.as_ref())?;
        let root = fs::canonicalize(root).map_err(|_| ServerAuthError::Storage)?;
        let key = LessSafeKey::new(
            UnboundKey::new(&aead::AES_256_GCM, &*key).map_err(|_| ServerAuthError::Storage)?,
        );
        Ok(Self {
            root,
            key,
            lock_timeout: Duration::from_secs(65),
        })
    }
    pub fn with_lock_timeout(mut self, timeout: Duration) -> Self {
        self.lock_timeout = timeout;
        self
    }
}

struct FileGuard<'a> {
    store: &'a SealedFileTokenStore,
    _lock: File,
    key: String,
}

impl TokenStore for SealedFileTokenStore {
    fn lock(
        &self,
        account: &str,
        provider: &str,
    ) -> Result<Box<dyn TokenStoreGuard + '_>, ServerAuthError> {
        if account.is_empty() || provider.is_empty() {
            return Err(ServerAuthError::InvalidConfig);
        }
        // Recheck privacy after construction, before any token/lock access.
        check_dir(&self.root)?;
        // JSON tuple encoding prevents delimiter collisions; hash prevents path traversal.
        let key = digest(&serde_json::json!([account, provider]).to_string());
        let lock = private_file(&self.root.join(format!("{key}.lock")), false)?;
        let start = Instant::now();
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if start.elapsed() < self.lock_timeout => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => return Err(ServerAuthError::Storage),
            }
        }
        Ok(Box::new(FileGuard {
            store: self,
            _lock: lock,
            key,
        }))
    }
}

impl FileGuard<'_> {
    fn path(&self) -> PathBuf {
        self.store.root.join(format!("{}.sealed", self.key))
    }
    fn sync_dir(&self) -> Result<(), ServerAuthError> {
        File::open(&self.store.root)
            .and_then(|f| f.sync_all())
            .map_err(|_| ServerAuthError::Storage)
    }
}
impl TokenStoreGuard for FileGuard<'_> {
    fn get(&mut self) -> Result<Option<TokenSet>, ServerAuthError> {
        check_dir(&self.store.root)?;
        let path = self.path();
        let file = match open_existing_private_file(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(ServerAuthError::Storage),
        };
        let meta = file.metadata().map_err(|_| ServerAuthError::Storage)?;
        check_private_file(&meta)?;
        if meta.len() > MAX_SEALED_BYTES {
            return Err(ServerAuthError::Storage);
        }
        let mut bytes = Zeroizing::new(Vec::with_capacity(meta.len() as usize));
        file.take(MAX_SEALED_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ServerAuthError::Storage)?;
        if bytes.len() as u64 > MAX_SEALED_BYTES
            || bytes.len() < MAGIC.len() + 12 + 16
            || !bytes.starts_with(MAGIC)
        {
            return Err(ServerAuthError::Storage);
        }
        let nonce: [u8; 12] = bytes[MAGIC.len()..MAGIC.len() + 12]
            .try_into()
            .map_err(|_| ServerAuthError::Storage)?;
        let plaintext = self
            .store
            .key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(self.key.as_bytes()),
                &mut bytes[MAGIC.len() + 12..],
            )
            .map_err(|_| ServerAuthError::Storage)?;
        let text = std::str::from_utf8(plaintext).map_err(|_| ServerAuthError::Storage)?;
        crate::token_codec::decode(text)
            .map(Some)
            .map_err(|_| ServerAuthError::Storage)
    }
    fn put(&mut self, tokens: &TokenSet) -> Result<(), ServerAuthError> {
        check_dir(&self.store.root)?;
        let mut text = Zeroizing::new(crate::token_codec::encode(tokens));
        // Move the allocation: avoid an extra plaintext copy before sealing.
        let mut bytes = Zeroizing::new(std::mem::take(&mut *text).into_bytes());
        if bytes.len() as u64 + 33 > MAX_SEALED_BYTES {
            return Err(ServerAuthError::Storage);
        }
        let mut nonce = [0u8; 12];
        getrandom::fill(&mut nonce).map_err(|_| ServerAuthError::Storage)?;
        self.store
            .key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(self.key.as_bytes()),
                &mut *bytes,
            )
            .map_err(|_| ServerAuthError::Storage)?;
        let temp = self
            .store
            .root
            .join(format!("{}.{}.tmp", self.key, super::random(16)?));
        let result = (|| {
            let mut file = private_file(&temp, true)?;
            file.write_all(MAGIC)
                .and_then(|_| file.write_all(&nonce))
                .and_then(|_| file.write_all(&bytes))
                .and_then(|_| file.sync_all())
                .map_err(|_| ServerAuthError::Storage)?;
            fs::rename(&temp, self.path()).map_err(|_| ServerAuthError::Storage)?;
            self.sync_dir()
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result
    }
    fn delete(&mut self) -> Result<(), ServerAuthError> {
        check_dir(&self.store.root)?;
        match fs::remove_file(self.path()) {
            Ok(()) => self.sync_dir(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(ServerAuthError::Storage),
        }
    }
}

pub(super) fn private_dir(path: &Path) -> Result<(), ServerAuthError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        match fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(ServerAuthError::Storage),
        }
        check_dir(path)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(ServerAuthError::InvalidConfig)
    }
}
fn check_permissions(meta: &fs::Metadata) -> Result<(), ServerAuthError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        // SAFETY: geteuid has no arguments or memory-safety preconditions.
        let uid = unsafe { libc::geteuid() };
        if meta.permissions().mode() & 0o077 != 0 || meta.uid() != uid {
            return Err(ServerAuthError::Storage);
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        Err(ServerAuthError::InvalidConfig)
    }
}
pub(super) fn private_file(path: &Path, exclusive: bool) -> Result<File, ServerAuthError> {
    if let Ok(meta) = fs::symlink_metadata(path) {
        if !meta.is_file() {
            return Err(ServerAuthError::Storage);
        }
        check_permissions(&meta)?;
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(!exclusive)
        .create_new(exclusive);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // NOFOLLOW closes the lstat/open race; NONBLOCK prevents a swapped FIFO
        // from hanging before descriptor metadata can reject it.
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options.open(path).map_err(|_| ServerAuthError::Storage)?;
    check_private_file(&file.metadata().map_err(|_| ServerAuthError::Storage)?)?;
    Ok(file)
}

pub(super) fn check_dir(path: &Path) -> Result<(), ServerAuthError> {
    let meta = fs::symlink_metadata(path).map_err(|_| ServerAuthError::Storage)?;
    if !meta.is_dir() {
        return Err(ServerAuthError::Storage);
    }
    check_permissions(&meta)
}
pub(super) fn check_private_file(meta: &fs::Metadata) -> Result<(), ServerAuthError> {
    if !meta.is_file() {
        return Err(ServerAuthError::Storage);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() != 1 {
            return Err(ServerAuthError::Storage);
        }
    }
    check_permissions(meta)
}
pub(super) fn open_existing_private_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    options.open(path)
}
