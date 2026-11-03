//! Encrypted persistent browser profile (feature `profile-vault`, rightkit#1306).
//!
//! A [`ProfileVault`] keeps one Chrome profile tree as a single authenticated-encrypted file at
//! rest. At session start the tree is decrypted into a private working directory that Chrome
//! uses as its user-data directory; at session end the tree is archived again, encrypted, the
//! file is replaced atomically and the working directory is wiped. Nothing is ever encrypted in
//! place, so a crash leaves either the previous sealed file (authoritative) or a recoverable
//! working directory, never a half-written vault.
//!
//! # Layout under `root`
//!
//! * `<name>.rkvault`: the sealed profile (see "Archive format").
//! * `<name>.work/`: the plaintext working directory, present only while a session runs (or
//!   after a crash, until the next start recovers it). Created owner-only (`0700`) on Unix; on
//!   Windows it inherits the access list of `root`, so choose a per-user `root`.
//! * `<name>.work.state`: one word, written atomically: `extracting` (decrypting, the sealed
//!   file is authoritative), `open` (the tree is complete and may hold newer data than the sealed
//!   file), `sealed` (the sealed file already holds the tree; only the wipe is outstanding).
//! * `<name>.rkvault.lock`: held for the whole session; a second session gets
//!   [`VaultError::InUse`]. A lock that can be taken while a working directory exists means the
//!   previous owner died.
//! * `<name>.downloads/`: the download directory. Downloads are the person's files, not profile
//!   state, so they are neither archived nor wiped.
//!
//! # Archive format (decision record, version 1)
//!
//! One purpose-built stream instead of tar or zip: Chrome's profile needs only directories and
//! regular files, a self-defined format lets extraction refuse every other entry type and every
//! path that could leave the working directory, and it adds no dependency.
//!
//! The file is `header || chunk*`. The header is
//! `"RKPVAULT" || format version (u16 BE) || cipher id (u8, 1 = AES-256-GCM) || chunk size log2
//! (u8, 20) || profile id length (u8) || profile id || salt (32 random bytes)`. The whole header
//! is bound into every chunk as AEAD associated data together with a one-byte final flag, so a
//! changed format version, a vault renamed to another profile id, a reordered or dropped chunk and
//! a truncated file all fail authentication. Each chunk is `length (u32 BE) || AEAD ciphertext`
//! of at most 1 MiB of plaintext, nonce `0^4 || chunk counter (u64 BE)`. The file key is
//! HKDF-SHA256 over the credential-store key with the per-file salt and the profile id, so every
//! seal uses a fresh key and nonces can never repeat under one key.
//!
//! The decrypted stream is `entry* END`: directory `1 || path`, file `2 || path || mode (u32 BE)
//! || size (u64 BE) || bytes`, end `0 || entry count (u64 BE)`; a path is `length (u16 BE) ||
//! UTF-8 relative path` with `/` separators. Chrome's `Singleton*` files (a live-session symlink
//! set) and RightKit's own lock and downloads at the top level are skipped; a symlink or any
//! other special file elsewhere is refused with [`VaultError::UnsupportedEntry`] rather than
//! followed or dropped.
//!
//! # Key
//!
//! The 32-byte master key lives in the OS credential store (Keychain, Credential Manager) through
//! `rightkit-credentials`, account `rightkit-browser.profile-vault.<name>`, in the namespace of
//! the [`Store`] the caller supplies. A key that cannot be read is a typed refusal
//! ([`VaultError::KeyUnavailable`]); the key is minted only on an authoritative "absent", and a
//! vault whose key has vanished is [`VaultError::KeyLost`] and is never overwritten silently.
//! Chrome itself now also uses the real OS keychain for its own cookie and password encryption
//! (the mock keychain flags were removed in rightkit-browser 0.3.9); the vault protects the whole
//! tree, including files Chrome does not encrypt.
//!
//! # Limits
//!
//! Wiping overwrites files up to 16 MiB with zeros and then deletes the tree; on copy-on-write
//! or flash storage an overwrite does not guarantee the old blocks are gone, so full-disk
//! encryption remains the defence for a stolen powered-off disk. The master key is held in memory
//! for the session so stopping needs no second credential-store prompt.

use crate::config::{reject_real_browser_profile, validate_profile_name};
use rightkit_credentials::Store;
use rightkit_crypto::aead::{self, Cipher, KEY_LEN, TAG_LEN};
use rightkit_crypto::Secret;
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

type VResult<T> = std::result::Result<T, VaultError>;

const MAGIC: &[u8; 8] = b"RKPVAULT";
const FORMAT_VERSION: u16 = 1;
const CIPHER_AES256GCM: u8 = 1;
const CHUNK_LOG2: u8 = 20;
const CHUNK: usize = 1 << CHUNK_LOG2;
const SALT_LEN: usize = 32;
/// Bytes of master key requested from the credential store.
const MASTER_KEY_BYTES: usize = 32;
const TAG_END: u8 = 0;
const TAG_DIR: u8 = 1;
const TAG_FILE: u8 = 2;
const MAX_PATH_BYTES: usize = 4096;
/// Top-level names never archived: Chrome's live-session links and RightKit's own files.
const SKIP_TOP: &[&str] = &[
    "SingletonLock",
    "SingletonSocket",
    "SingletonCookie",
    ".rightkit-browser.lock",
    "rightkit-downloads",
];
/// Files up to this size are overwritten with zeros before deletion.
const OVERWRITE_MAX: u64 = 16 * 1024 * 1024;
const WIPE_RETRY: Duration = Duration::from_secs(3);

const STATE_EXTRACTING: &str = "extracting";
const STATE_OPEN: &str = "open";
const STATE_SEALED: &str = "sealed";

/// Why a vault operation refused or failed. No variant carries key material.
#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    /// The OS credential store could not provide the vault key (locked, denied, unavailable,
    /// non-interactive and needing a prompt). Nothing on disk was touched.
    #[error("profile vault key unavailable: {0}")]
    KeyUnavailable(String),
    /// A sealed profile exists but its key is gone (the credential store minted a fresh one).
    /// The sealed file is untouched; decide explicitly with [`ProfileVault::discard`].
    #[error("profile vault '{0}' exists but its key is missing from the credential store")]
    KeyLost(String),
    /// The sealed file failed authentication or is malformed: tampered, truncated, written under
    /// another key or another profile id.
    #[error("profile vault is corrupt or was sealed under another key: {0}")]
    Corrupt(String),
    /// The sealed file has a format version this build does not read.
    #[error("profile vault format version {0} is not supported")]
    UnsupportedFormat(u16),
    /// Another session holds this vault.
    #[error("profile vault '{0}' is already open in another session")]
    InUse(String),
    /// An unencrypted profile with this name exists and no vault does yet; call
    /// [`ProfileVault::migrate_plaintext`] first.
    #[error("an unencrypted profile exists at {}; migrate it into the vault first", .0.display())]
    PlaintextProfilePresent(PathBuf),
    /// The profile holds an entry the archive cannot represent (symlink, device, non-UTF-8 name).
    #[error("profile entry cannot be stored in the vault: {}", .0.display())]
    UnsupportedEntry(PathBuf),
    /// The plaintext tree could not be removed; it is still on disk.
    #[error("plaintext profile still on disk at {}: {reason}", .path.display())]
    NotWiped { path: PathBuf, reason: String },
    #[error("invalid profile vault: {0}")]
    Invalid(String),
    #[error("io: {0}")]
    Io(#[from] io::Error),
}

/// An encrypted persistent profile: where it lives and which credential store keys it.
///
/// Pass it to [`crate::LaunchOptions::encrypted_profile`]. Cloning shares nothing but the
/// description; two sessions on one vault are refused by the lock.
#[derive(Clone)]
pub struct ProfileVault {
    store: Store,
    root: PathBuf,
    name: String,
}

impl std::fmt::Debug for ProfileVault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProfileVault")
            .field("root", &self.root)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl PartialEq for ProfileVault {
    fn eq(&self, other: &Self) -> bool {
        self.root == other.root
            && self.name == other.name
            && self.store.namespace() == other.store.namespace()
    }
}

impl Eq for ProfileVault {}

impl ProfileVault {
    /// Describe the vault `name` under `root`, keyed through `store` (its namespace is the
    /// Keychain service). `name` follows the named-profile rules (at most 64 of `A-Za-z0-9-_.`,
    /// not starting with a dot). Nothing is created or read until a session launches.
    pub fn new(
        store: Store,
        root: impl Into<PathBuf>,
        name: impl Into<String>,
    ) -> Result<Self, VaultError> {
        let name = name.into();
        validate_profile_name(&name).map_err(|e| VaultError::Invalid(e.to_string()))?;
        Ok(Self {
            store,
            root: root.into(),
            name,
        })
    }

    /// The sealed file.
    pub fn vault_file(&self) -> PathBuf {
        self.root.join(format!("{}.rkvault", self.name))
    }

    /// The plaintext working directory, present only while a session runs.
    pub fn work_dir(&self) -> PathBuf {
        self.root.join(format!("{}.work", self.name))
    }

    /// Where downloads go: outside the archive and never wiped.
    pub fn downloads_dir(&self) -> PathBuf {
        self.root.join(format!("{}.downloads", self.name))
    }

    fn state_file(&self) -> PathBuf {
        self.root.join(format!("{}.work.state", self.name))
    }

    fn lock_file(&self) -> PathBuf {
        self.root.join(format!("{}.rkvault.lock", self.name))
    }

    fn tmp_file(&self) -> PathBuf {
        self.root.join(format!("{}.rkvault.tmp", self.name))
    }

    /// The directory an earlier `ProfileSpec::Named` session used for the same name.
    fn legacy_dir(&self) -> PathBuf {
        self.root.join(&self.name)
    }

    fn account(&self) -> String {
        format!("rightkit-browser.profile-vault.{}", self.name)
    }

    fn lock(&self) -> VResult<File> {
        reject_real_browser_profile(&self.root).map_err(|e| VaultError::Invalid(e.to_string()))?;
        create_private_dir(&self.root)?;
        let lock = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(self.lock_file())?;
        match lock.try_lock() {
            Ok(()) => Ok(lock),
            Err(std::fs::TryLockError::WouldBlock) => Err(VaultError::InUse(self.name.clone())),
            Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
        }
    }

    fn master_key(&self) -> VResult<(Secret, bool)> {
        let (secret, created) = self
            .store
            .get_or_create_key(&self.account(), MASTER_KEY_BYTES)
            .map_err(|e| VaultError::KeyUnavailable(e.to_string()))?;
        Ok((Secret::new(secret.expose_bytes().to_vec()), created))
    }

    /// Take the vault for a session: lock it, fetch the key, recover a crashed session's
    /// leftover tree, then decrypt (or create) the working directory.
    pub(crate) fn open(&self) -> VResult<VaultLease> {
        let lock = self.lock()?;
        let vault = self.vault_file();
        if !vault.is_file() && self.legacy_dir().exists() {
            return Err(VaultError::PlaintextProfilePresent(self.legacy_dir()));
        }
        let (key, created) = self.master_key()?;
        let _ = std::fs::remove_file(self.tmp_file());
        let resealed = self.recover(&key)?;
        if vault.is_file() && created && !resealed {
            return Err(VaultError::KeyLost(self.name.clone()));
        }
        let work = self.work_dir();
        let state = self.state_file();
        if vault.is_file() {
            write_state(&state, STATE_EXTRACTING)?;
            if let Err(error) = open_archive(&vault, &work, &key, &self.name) {
                let _ = wipe_tree(&work);
                let _ = std::fs::remove_file(&state);
                return Err(error);
            }
        } else {
            write_state(&state, STATE_OPEN)?;
            create_private_dir(&work)?;
            return Ok(self.lease(lock, key));
        }
        write_state(&state, STATE_OPEN)?;
        Ok(self.lease(lock, key))
    }

    fn lease(&self, lock: File, key: Secret) -> VaultLease {
        VaultLease {
            _lock: lock,
            work: self.work_dir(),
            vault: self.vault_file(),
            tmp: self.tmp_file(),
            state: self.state_file(),
            key,
            name: self.name.clone(),
            finished: false,
        }
    }

    /// Crash recovery. A leftover working directory whose state is `open` holds the newest
    /// data and is sealed into the vault; any other state means the sealed file is already
    /// authoritative (or already current) and the tree is only wiped. Returns whether a tree was
    /// sealed. The caller holds the lock, so no live session owns the directory.
    fn recover(&self, key: &Secret) -> VResult<bool> {
        let work = self.work_dir();
        let state = self.state_file();
        if std::fs::symlink_metadata(&work).is_err() {
            let _ = std::fs::remove_file(&state);
            return Ok(false);
        }
        let resealed = read_state(&state).as_deref() == Some(STATE_OPEN);
        if resealed {
            seal_tree(&work, &self.vault_file(), &self.tmp_file(), key, &self.name)?;
            write_state(&state, STATE_SEALED)?;
        }
        wipe_tree(&work)?;
        let _ = std::fs::remove_file(&state);
        Ok(resealed)
    }

    /// Move an unencrypted profile that an earlier `ProfileSpec::Named` session left at
    /// `<root>/<name>` into the vault: seal it, re-read the sealed file to authenticate every
    /// chunk, move its `rightkit-downloads` to [`Self::downloads_dir`], then wipe the plaintext
    /// tree. Refuses when a vault already exists or a session holds the lock.
    pub fn migrate_plaintext(&self) -> Result<(), VaultError> {
        let _lock = self.lock()?;
        let legacy = self.legacy_dir();
        let vault = self.vault_file();
        if vault.exists() {
            return Err(VaultError::Invalid(format!(
                "vault '{}' already exists; nothing to migrate",
                self.name
            )));
        }
        if !legacy.is_dir() {
            return Err(VaultError::Invalid(format!(
                "no unencrypted profile at {}",
                legacy.display()
            )));
        }
        let (key, _) = self.master_key()?;
        seal_tree(&legacy, &vault, &self.tmp_file(), &key, &self.name)?;
        if let Err(error) = verify_archive(&vault, &key, &self.name) {
            let _ = std::fs::remove_file(&vault);
            return Err(error);
        }
        let old_downloads = legacy.join("rightkit-downloads");
        if old_downloads.is_dir() && !self.downloads_dir().exists() {
            std::fs::rename(&old_downloads, self.downloads_dir())?;
        }
        wipe_tree(&legacy)
    }

    /// Delete the sealed file, any leftover working tree and its state: the explicit way out of
    /// [`VaultError::KeyLost`] or [`VaultError::Corrupt`]. The caller owns the decision to lose
    /// the profile. The credential-store key is left alone.
    pub fn discard(&self) -> Result<(), VaultError> {
        let _lock = self.lock()?;
        let _ = std::fs::remove_file(self.tmp_file());
        wipe_tree(&self.work_dir())?;
        let _ = std::fs::remove_file(self.state_file());
        match std::fs::remove_file(self.vault_file()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// An open vault for one session. Dropping it without [`Self::seal_and_wipe`] (a launch that
/// failed, a dropped session) still seals and wipes, best effort; a failure there leaves the
/// tree for the next start's recovery.
pub(crate) struct VaultLease {
    _lock: File,
    work: PathBuf,
    vault: PathBuf,
    tmp: PathBuf,
    state: PathBuf,
    key: Secret,
    name: String,
    finished: bool,
}

impl VaultLease {
    /// The plaintext directory Chrome uses as its user-data directory.
    pub(crate) fn work_dir(&self) -> &Path {
        &self.work
    }

    /// Encrypt the working tree into the vault, then wipe it. On a seal failure the tree and its
    /// `open` state stay, so the next start recovers them. Chrome must have exited.
    pub(crate) fn seal_and_wipe(&mut self) -> VResult<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        seal_tree(&self.work, &self.vault, &self.tmp, &self.key, &self.name)?;
        write_state(&self.state, STATE_SEALED)?;
        wipe_tree(&self.work)?;
        let _ = std::fs::remove_file(&self.state);
        Ok(())
    }
}

impl Drop for VaultLease {
    fn drop(&mut self) {
        let _ = self.seal_and_wipe();
    }
}

// ---------------------------------------------------------------------------------------------
// Filesystem helpers

fn create_private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
    }
}

fn private_file_options(mode: u32) -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
    }
    options
}

fn write_state(path: &Path, value: &str) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut file = private_file_options(0o600)
        .create(true)
        .truncate(true)
        .open(&tmp)?;
    file.write_all(value.as_bytes())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)
}

fn read_state(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_string())
}

/// Overwrite small files with zeros, then delete the tree with bounded retries (a just-exited
/// Windows helper can hold a handle briefly). Symlinks are deleted, never followed.
fn wipe_tree(path: &Path) -> VResult<()> {
    if std::fs::symlink_metadata(path).is_err() {
        return Ok(());
    }
    overwrite_files(path);
    let deadline = Instant::now() + WIPE_RETRY;
    loop {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                if Instant::now() >= deadline {
                    return Err(VaultError::NotWiped {
                        path: path.to_path_buf(),
                        reason: e.to_string(),
                    });
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn overwrite_files(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            overwrite_files(&path);
        } else if meta.is_file() && meta.len() <= OVERWRITE_MAX {
            zero_file(&path, meta.len());
        }
    }
}

fn zero_file(path: &Path, len: u64) {
    let Ok(mut file) = OpenOptions::new().write(true).open(path) else {
        return;
    };
    let zeros = [0u8; 65536];
    let mut left = len;
    while left > 0 {
        let n = left.min(zeros.len() as u64) as usize;
        if file.write_all(&zeros[..n]).is_err() {
            break;
        }
        left -= n as u64;
    }
    let _ = file.sync_all();
}

// ---------------------------------------------------------------------------------------------
// Chunked AEAD stream

fn chunk_nonce(counter: u64) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[4..].copy_from_slice(&counter.to_be_bytes());
    nonce
}

fn chunk_aad(header: &[u8], last: bool) -> Vec<u8> {
    let mut aad = Vec::with_capacity(header.len() + 1);
    aad.extend_from_slice(header);
    aad.push(u8::from(last));
    aad
}

fn build_header(name: &str, salt: &[u8; SALT_LEN]) -> Vec<u8> {
    let mut header = Vec::with_capacity(13 + name.len() + SALT_LEN);
    header.extend_from_slice(MAGIC);
    header.extend_from_slice(&FORMAT_VERSION.to_be_bytes());
    header.push(CIPHER_AES256GCM);
    header.push(CHUNK_LOG2);
    header.push(name.len() as u8);
    header.extend_from_slice(name.as_bytes());
    header.extend_from_slice(salt);
    header
}

/// Read and check the header; returns its exact bytes (the associated data) and the salt.
fn read_header<R: Read>(reader: &mut R, expected_name: &str) -> VResult<(Vec<u8>, [u8; SALT_LEN])> {
    let corrupt = |what: &str| VaultError::Corrupt(what.to_string());
    let mut fixed = [0u8; 13];
    reader
        .read_exact(&mut fixed)
        .map_err(|_| corrupt("header is truncated"))?;
    if &fixed[..8] != MAGIC {
        return Err(corrupt("not a profile vault"));
    }
    let version = u16::from_be_bytes([fixed[8], fixed[9]]);
    if version != FORMAT_VERSION {
        return Err(VaultError::UnsupportedFormat(version));
    }
    if fixed[10] != CIPHER_AES256GCM || fixed[11] != CHUNK_LOG2 {
        return Err(corrupt("unknown cipher or chunk size"));
    }
    let name_len = usize::from(fixed[12]);
    let mut name = vec![0u8; name_len];
    reader
        .read_exact(&mut name)
        .map_err(|_| corrupt("header is truncated"))?;
    if name != expected_name.as_bytes() {
        return Err(corrupt("sealed for a different profile id"));
    }
    let mut salt = [0u8; SALT_LEN];
    reader
        .read_exact(&mut salt)
        .map_err(|_| corrupt("header is truncated"))?;
    let mut header = fixed.to_vec();
    header.extend_from_slice(&name);
    header.extend_from_slice(&salt);
    Ok((header, salt))
}

fn derive_key(master: &Secret, salt: &[u8], name: &str) -> VResult<Secret> {
    let mut info = b"rightkit-browser profile vault v1 key\0".to_vec();
    info.extend_from_slice(name.as_bytes());
    rightkit_crypto::hkdf_sha256(master.expose(), salt, &info, KEY_LEN)
        .map(Secret::new)
        .map_err(|_| VaultError::Corrupt("key derivation failed".into()))
}

/// Buffers plaintext into 1 MiB chunks and writes each as an authenticated ciphertext chunk.
struct SealWriter<W: Write> {
    out: Option<W>,
    key: Secret,
    header: Vec<u8>,
    buf: Vec<u8>,
    counter: u64,
}

impl<W: Write> SealWriter<W> {
    fn new(out: W, key: Secret, header: Vec<u8>) -> Self {
        Self {
            out: Some(out),
            key,
            header,
            buf: Vec::with_capacity(CHUNK),
            counter: 0,
        }
    }

    fn seal_chunk(&mut self, last: bool) -> io::Result<()> {
        let sealed = aead::seal_with_nonce(
            Cipher::Aes256Gcm,
            self.key.expose(),
            &chunk_nonce(self.counter),
            &chunk_aad(&self.header, last),
            &self.buf,
        )
        .map_err(|_| io::Error::other("chunk encryption failed"))?;
        let out = self
            .out
            .as_mut()
            .ok_or_else(|| io::Error::other("writer already finished"))?;
        out.write_all(&(sealed.len() as u32).to_be_bytes())?;
        out.write_all(&sealed)?;
        self.counter += 1;
        rightkit_crypto::wipe(&mut self.buf);
        self.buf.clear();
        Ok(())
    }

    fn finish(mut self) -> io::Result<W> {
        self.seal_chunk(true)?;
        let mut out = self
            .out
            .take()
            .ok_or_else(|| io::Error::other("writer already finished"))?;
        out.flush()?;
        Ok(out)
    }
}

impl<W: Write> Write for SealWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut rest = data;
        while !rest.is_empty() {
            if self.buf.len() == CHUNK {
                self.seal_chunk(false)?;
            }
            let take = (CHUNK - self.buf.len()).min(rest.len());
            self.buf.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<W: Write> Drop for SealWriter<W> {
    fn drop(&mut self) {
        rightkit_crypto::wipe(&mut self.buf);
    }
}

/// Reads ciphertext chunks, authenticating each (with its final flag decided by lookahead), and
/// serves the plaintext as a byte stream.
struct OpenReader<R: Read> {
    inp: R,
    key: Secret,
    header: Vec<u8>,
    buf: Vec<u8>,
    pos: usize,
    counter: u64,
    pending: Option<Vec<u8>>,
}

impl<R: Read> OpenReader<R> {
    fn new(mut inp: R, key: Secret, header: Vec<u8>) -> VResult<Self> {
        let pending = read_sealed(&mut inp).map_err(map_io)?;
        if pending.is_none() {
            return Err(VaultError::Corrupt("vault holds no data".into()));
        }
        Ok(Self {
            inp,
            key,
            header,
            buf: Vec::new(),
            pos: 0,
            counter: 0,
            pending,
        })
    }

    fn next_chunk(&mut self) -> io::Result<bool> {
        let Some(current) = self.pending.take() else {
            return Ok(false);
        };
        let next = read_sealed(&mut self.inp)?;
        let last = next.is_none();
        let plain = aead::open_with_nonce(
            Cipher::Aes256Gcm,
            self.key.expose(),
            &chunk_nonce(self.counter),
            &chunk_aad(&self.header, last),
            &current,
        )
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "chunk authentication failed"))?;
        self.counter += 1;
        self.pending = next;
        rightkit_crypto::wipe(&mut self.buf);
        self.buf = plain;
        self.pos = 0;
        Ok(true)
    }
}

impl<R: Read> Read for OpenReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            if self.pos < self.buf.len() {
                let n = (self.buf.len() - self.pos).min(out.len());
                out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
                self.pos += n;
                return Ok(n);
            }
            if !self.next_chunk()? {
                return Ok(0);
            }
        }
    }
}

impl<R: Read> Drop for OpenReader<R> {
    fn drop(&mut self) {
        rightkit_crypto::wipe(&mut self.buf);
    }
}

fn read_sealed<R: Read>(reader: &mut R) -> io::Result<Option<Vec<u8>>> {
    let bad = |what: &'static str| io::Error::new(io::ErrorKind::InvalidData, what);
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < len.len() {
        let n = reader.read(&mut len[got..])?;
        if n == 0 {
            if got == 0 {
                return Ok(None);
            }
            return Err(bad("truncated chunk header"));
        }
        got += n;
    }
    let len = u32::from_be_bytes(len) as usize;
    if !(TAG_LEN..=CHUNK + TAG_LEN).contains(&len) {
        return Err(bad("chunk length out of range"));
    }
    let mut sealed = vec![0u8; len];
    reader
        .read_exact(&mut sealed)
        .map_err(|_| bad("truncated chunk"))?;
    Ok(Some(sealed))
}

fn map_io(error: io::Error) -> VaultError {
    match error.kind() {
        io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof => {
            VaultError::Corrupt(error.to_string())
        }
        _ => VaultError::Io(error),
    }
}

// ---------------------------------------------------------------------------------------------
// Archive

fn seal_tree(work: &Path, vault: &Path, tmp: &Path, master: &Secret, name: &str) -> VResult<()> {
    let result = seal_to_tmp(work, tmp, master, name);
    if result.is_err() {
        let _ = std::fs::remove_file(tmp);
        return result;
    }
    std::fs::rename(tmp, vault).map_err(|e| {
        let _ = std::fs::remove_file(tmp);
        VaultError::Io(e)
    })
}

fn seal_to_tmp(work: &Path, tmp: &Path, master: &Secret, name: &str) -> VResult<()> {
    let mut salt = [0u8; SALT_LEN];
    rightkit_crypto::random_fill(&mut salt)
        .map_err(|_| VaultError::Corrupt("system random generator failed".into()))?;
    let header = build_header(name, &salt);
    let key = derive_key(master, &salt, name)?;
    let file = private_file_options(0o600)
        .create(true)
        .truncate(true)
        .open(tmp)?;
    let mut out = BufWriter::new(file);
    out.write_all(&header)?;
    let mut writer = SealWriter::new(out, key, header);
    write_tree(&mut writer, work)?;
    let out = writer.finish()?;
    let file = out
        .into_inner()
        .map_err(|e| VaultError::Io(e.into_error()))?;
    file.sync_all()?;
    Ok(())
}

fn write_tree<W: Write>(writer: &mut W, root: &Path) -> VResult<()> {
    let mut count = 0u64;
    walk_tree(writer, root, Path::new(""), &mut count)?;
    writer.write_all(&[TAG_END])?;
    writer.write_all(&count.to_be_bytes())?;
    Ok(())
}

fn relative_text(rel: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in rel.components() {
        let text = component.as_os_str().to_str()?;
        if text.is_empty() || text.contains(['/', '\\', '\0']) {
            return None;
        }
        parts.push(text);
    }
    let joined = parts.join("/");
    (!joined.is_empty() && joined.len() <= MAX_PATH_BYTES).then_some(joined)
}

fn write_path<W: Write>(writer: &mut W, text: &str) -> io::Result<()> {
    writer.write_all(&(text.len() as u16).to_be_bytes())?;
    writer.write_all(text.as_bytes())
}

fn walk_tree<W: Write>(writer: &mut W, base: &Path, rel: &Path, count: &mut u64) -> VResult<()> {
    let dir = base.join(rel);
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        names.push(entry?.file_name());
    }
    names.sort();
    for name in names {
        let top_level = rel.as_os_str().is_empty();
        let child_rel = rel.join(&name);
        let path = base.join(&child_rel);
        if top_level && name.to_str().is_some_and(|n| SKIP_TOP.contains(&n)) {
            continue;
        }
        let text =
            relative_text(&child_rel).ok_or_else(|| VaultError::UnsupportedEntry(path.clone()))?;
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            writer.write_all(&[TAG_DIR])?;
            write_path(writer, &text)?;
            *count += 1;
            walk_tree(writer, base, &child_rel, count)?;
        } else if meta.is_file() {
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                meta.permissions().mode() & 0o777
            };
            #[cfg(not(unix))]
            let mode = 0u32;
            let size = meta.len();
            writer.write_all(&[TAG_FILE])?;
            write_path(writer, &text)?;
            writer.write_all(&mode.to_be_bytes())?;
            writer.write_all(&size.to_be_bytes())?;
            let mut file = File::open(&path)?;
            let copied = io::copy(&mut (&mut file).take(size), &mut *writer)?;
            if copied != size {
                return Err(VaultError::Io(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("{} changed while it was archived", path.display()),
                )));
            }
            *count += 1;
        } else {
            return Err(VaultError::UnsupportedEntry(path));
        }
    }
    Ok(())
}

fn open_archive(vault: &Path, work: &Path, master: &Secret, name: &str) -> VResult<()> {
    let mut reader = BufReader::new(File::open(vault)?);
    let (header, salt) = read_header(&mut reader, name)?;
    let key = derive_key(master, &salt, name)?;
    let mut opened = OpenReader::new(reader, key, header)?;
    create_private_dir(work)?;
    read_tree(&mut opened, work)
}

/// Authenticate every chunk of the sealed file and the archive framing without writing anything.
fn verify_archive(vault: &Path, master: &Secret, name: &str) -> VResult<()> {
    let mut reader = BufReader::new(File::open(vault)?);
    let (header, salt) = read_header(&mut reader, name)?;
    let key = derive_key(master, &salt, name)?;
    let mut opened = OpenReader::new(reader, key, header)?;
    io::copy(&mut opened, &mut io::sink()).map_err(map_io)?;
    Ok(())
}

fn read_exact_corrupt<R: Read>(reader: &mut R, out: &mut [u8]) -> VResult<()> {
    reader.read_exact(out).map_err(map_io)
}

fn read_path<R: Read>(reader: &mut R) -> VResult<String> {
    let mut len = [0u8; 2];
    read_exact_corrupt(reader, &mut len)?;
    let len = usize::from(u16::from_be_bytes(len));
    if len == 0 || len > MAX_PATH_BYTES {
        return Err(VaultError::Corrupt("entry path length out of range".into()));
    }
    let mut text = vec![0u8; len];
    read_exact_corrupt(reader, &mut text)?;
    String::from_utf8(text).map_err(|_| VaultError::Corrupt("entry path is not UTF-8".into()))
}

/// Join an archive path under `dest`, refusing anything that could leave it.
fn safe_join(dest: &Path, text: &str) -> VResult<PathBuf> {
    let mut path = dest.to_path_buf();
    for part in text.split('/') {
        let bad = part.is_empty()
            || part == "."
            || part == ".."
            || part.contains(['\\', '\0'])
            || (cfg!(windows) && part.contains(':'));
        if bad {
            return Err(VaultError::Corrupt("entry path escapes the profile".into()));
        }
        path.push(part);
    }
    Ok(path)
}

fn read_tree<R: Read>(reader: &mut R, dest: &Path) -> VResult<()> {
    let mut count = 0u64;
    loop {
        let mut tag = [0u8; 1];
        read_exact_corrupt(reader, &mut tag)?;
        match tag[0] {
            TAG_END => {
                let mut total = [0u8; 8];
                read_exact_corrupt(reader, &mut total)?;
                if u64::from_be_bytes(total) != count {
                    return Err(VaultError::Corrupt("entry count does not match".into()));
                }
                let mut extra = [0u8; 1];
                if reader.read(&mut extra).map_err(map_io)? != 0 {
                    return Err(VaultError::Corrupt("data after the end entry".into()));
                }
                return Ok(());
            }
            TAG_DIR => {
                let target = safe_join(dest, &read_path(reader)?)?;
                create_private_dir(&target)?;
                count += 1;
            }
            TAG_FILE => {
                let target = safe_join(dest, &read_path(reader)?)?;
                let mut mode = [0u8; 4];
                read_exact_corrupt(reader, &mut mode)?;
                let mode = u32::from_be_bytes(mode);
                let mut size = [0u8; 8];
                read_exact_corrupt(reader, &mut size)?;
                let size = u64::from_be_bytes(size);
                if let Some(parent) = target.parent() {
                    create_private_dir(parent)?;
                }
                let mut file = private_file_options((mode & 0o700) | 0o600)
                    .create_new(true)
                    .open(&target)?;
                let copied = io::copy(&mut (&mut *reader).take(size), &mut file).map_err(map_io)?;
                if copied != size {
                    return Err(VaultError::Corrupt("file entry is truncated".into()));
                }
                file.flush()?;
                count += 1;
            }
            _ => return Err(VaultError::Corrupt("unknown archive entry".into())),
        }
    }
}
