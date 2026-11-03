//! Android Keystore credential store.
//!
//! Android has no keychain-style item store for an app's secrets, so the store is built from the
//! two things the platform does provide: a hardware-backed, non-exportable AES-GCM key held in the
//! `AndroidKeyStore` provider, and app-private storage (`Context.getFilesDir()`). A secret is
//! encrypted inside the Keystore and only the ciphertext is written to disk.
//!
//! ## What lives where
//! * **Keystore (one AES-256-GCM key per [`Namespace`] service).** The alias is
//!   `rightkit-credentials/<service>`. The key never leaves the Keystore: this crate holds no key
//!   bytes and does no AES itself. It asks a [`KeystoreBridge`] to encrypt and decrypt.
//! * **App-private file `<files_dir>/rightkit-credentials/credentials-v1.json`.** A JSON list of
//!   `{service, account, blob}` entries. `blob` is the hex of whatever [`KeystoreBridge::encrypt`]
//!   returned (for the reference bridge: 12-byte IV, ciphertext, 16-byte tag). Service and account
//!   names are stored in the clear (they are identifiers, not secrets); the secret is only ever
//!   present as ciphertext. No plaintext secret is written to any file, log or error.
//! * **Additional authenticated data.** Every blob is bound to its `(service, account)` through the
//!   GCM AAD, so moving a blob from one entry to another makes it fail to decrypt rather than
//!   return another credential's secret.
//!
//! ## The injected bridge (JNI is not in this crate)
//! This crate does not link JNI and does not find the `JavaVM` or the `Context` itself. The
//! `rightkit-android` bridge crate (tracked as rightkit#149) owns that and implements
//! [`KeystoreBridge`] over it, then calls [`install`] once at startup with the app's files
//! directory (`Context.getFilesDir()`, an absolute path). Until [`install`] has run, the store
//! returned by [`crate::system_backend`] fails every call with [`Error::Unavailable`]. There is no
//! fallback to a file, to memory or to a key held by this crate.
//!
//! The bridge must implement the following contract (the `KeyGenParameterSpec` details are what
//! the reference Kotlin implementation uses):
//! * `encrypt(alias, plaintext, aad)`: get the `AndroidKeyStore` key `alias`, creating it if it is
//!   absent (`KeyProperties.KEY_ALGORITHM_AES`, 256 bits, `BLOCK_MODE_GCM`,
//!   `ENCRYPTION_PADDING_NONE`, `setRandomizedEncryptionRequired(true)`, purposes encrypt and
//!   decrypt, no user-authentication requirement, so the store works for a background caller and
//!   never prompts). Encrypt with `AES/GCM/NoPadding`, `updateAAD(aad)`, and return the
//!   Keystore-chosen IV followed by the ciphertext and tag as one opaque `Vec<u8>`. The IV must
//!   come from the Keystore, never be supplied by the caller (a caller-chosen IV is refused by
//!   `setRandomizedEncryptionRequired(true)`, and nonce reuse breaks GCM).
//! * `decrypt(alias, blob, aad)`: the inverse. It must NOT create a key: a missing key is
//!   [`BridgeError::KeyMissing`]. A GCM tag failure (tampered blob, different AAD, replaced key) is
//!   [`BridgeError::AuthenticationFailed`].
//! * A missing or detached `JavaVM`/`Context` is [`BridgeError::NoContext`].
//! * Calls arrive on arbitrary threads ([`crate::Store`] runs each OS call on its own worker
//!   thread), so the bridge must attach the calling thread to the VM (`AttachCurrentThread`) and
//!   detach it afterwards, and must be `Send + Sync`.
//! * No error may carry a secret, a key or a provider message: [`BridgeError`] holds a small code
//!   only.
//!
//! ## Behaviour
//! * `exists` reads the entry list only: it never calls the bridge, so it never touches the
//!   Keystore and cannot fail on a missing key.
//! * [`Interaction`] has no effect: the key requires no user authentication, so nothing can prompt.
//! * Mutations (`set`, `add_if_absent`, `delete`, `delete_if_equals`) hold a process-wide mutex and
//!   an exclusive `flock` on `<files_dir>/rightkit-credentials/credentials.lock` for their whole
//!   read-modify-write, so threads and processes (an IME or service in a second process) exclude
//!   each other. If `File::lock` reports `Unsupported` on Android (unconfirmed), the `flock` is
//!   skipped and only threads of one process exclude each other. The file is
//!   replaced by writing `credentials-v1.json.tmp` and renaming it over the old one, so a reader
//!   sees the old or the new file, never a torn one. Reads take no lock.
//! * A store file that cannot be parsed or has an unknown version fails every call, mutations
//!   included, and is never overwritten.
//! * `get` of an entry whose key is gone (see below) is an error, never `Ok(None)`: the store does
//!   not say "absent" about a credential it holds but cannot read. `set` over such an entry
//!   succeeds (the bridge creates a fresh key), which is how a user re-enters a lost secret.
//!
//! ## Error codes ([`Error::Backend`])
//! `-4` the Keystore key is missing, `-5` authentication failed, `-6` the store file cannot be
//! read, `-7` it cannot be parsed, `-8` an entry blob is not hex, `-9` it cannot be written, `-1`
//! lock failure, a positive or other value is the bridge's own [`BridgeError::Failed`] code.
//!
//! ## Backup (integrator action, not enforced here)
//! Android Auto Backup restores app files on a new device but cannot restore Keystore keys, so a
//! restored `credentials-v1.json` would hold entries nothing can decrypt (reported as code `-4`).
//! The app should exclude `files/rightkit-credentials/` from backup and device transfer
//! (`android:fullBackupContent` and `android:dataExtractionRules`).
//!
//! ## Verification status
//! The file format, locking, error mapping and AAD binding are unit-tested on the host with a fake
//! bridge. The Keystore behaviour itself (key generation, IV handling, StrongBox availability, key
//! invalidation, multi-process locking on Android) has not been run on a device or emulator.

use crate::backend::{Backend, ConditionalDelete, Interaction};
use crate::error::{Error, Result};
use crate::locked::{self, CredentialOps};
use crate::namespace::{check_target, Namespace};
use crate::secret::Secret;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const DIR_NAME: &str = "rightkit-credentials";
const STORE_FILE: &str = "credentials-v1.json";
const TMP_FILE: &str = "credentials-v1.json.tmp";
const LOCK_FILE: &str = "credentials.lock";
const FORMAT_VERSION: u32 = 1;

/// Why a [`KeystoreBridge`] call failed. A code only: never a message, key or secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeError {
    /// No `JavaVM` or `Context` is available to the bridge (not initialised, or detached).
    NoContext,
    /// `decrypt` found no key under the alias (never created, deleted, or lost on a restore).
    KeyMissing,
    /// The GCM tag did not verify: the blob was altered, the AAD differs, or the key was replaced.
    AuthenticationFailed,
    /// Any other Keystore or JNI failure, as a small bridge-defined code.
    Failed(i32),
}

/// The Android side of the store: AES-GCM with a key that stays inside `AndroidKeyStore`. See the
/// module documentation for the exact contract an implementation must meet.
pub trait KeystoreBridge: Send + Sync + 'static {
    /// Encrypt `plaintext` under the key `alias`, creating the key if it does not exist, with
    /// `aad` as GCM additional authenticated data. Returns one opaque blob (IV, ciphertext, tag).
    fn encrypt(
        &self,
        alias: &str,
        plaintext: &[u8],
        aad: &[u8],
    ) -> std::result::Result<Vec<u8>, BridgeError>;

    /// Decrypt a blob this bridge produced, with the same `alias` and `aad`. Never creates a key.
    fn decrypt(
        &self,
        alias: &str,
        blob: &[u8],
        aad: &[u8],
    ) -> std::result::Result<Vec<u8>, BridgeError>;
}

fn unavailable(why: &str) -> Error {
    Error::Unavailable(why.into())
}

/// Maps a bridge failure without carrying any text.
fn map(op: &'static str, e: BridgeError) -> Error {
    match e {
        BridgeError::NoContext => unavailable("the Android Keystore bridge has no JNI context"),
        BridgeError::KeyMissing => Error::Backend { op, code: -4 },
        BridgeError::AuthenticationFailed => Error::Backend { op, code: -5 },
        BridgeError::Failed(code) => Error::Backend {
            op,
            code: i64::from(code),
        },
    }
}

fn key_alias(ns: &Namespace) -> String {
    format!("rightkit-credentials/{}", ns.service())
}

/// The GCM additional authenticated data binding a blob to its entry.
fn aad(ns: &Namespace, account: &str) -> Vec<u8> {
    format!(
        "rightkit-credentials/v1\u{1f}{}\u{1f}{account}",
        ns.service()
    )
    .into_bytes()
}

fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(DIGITS[usize::from(b >> 4)] as char);
        out.push(DIGITS[usize::from(b & 0x0f)] as char);
    }
    out
}

fn nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    if !b.len().is_multiple_of(2) {
        return None;
    }
    b.chunks(2)
        .map(|pair| Some((nibble(pair[0])? << 4) | nibble(pair[1])?))
        .collect()
}

#[derive(Serialize, Deserialize, Clone)]
struct Item {
    service: String,
    account: String,
    /// Hex of the bridge's encrypted blob.
    blob: String,
}

#[derive(Serialize, Deserialize)]
struct StoreFile {
    version: u32,
    items: Vec<Item>,
}

impl StoreFile {
    fn empty() -> Self {
        Self {
            version: FORMAT_VERSION,
            items: Vec::new(),
        }
    }

    fn find(&self, service: &str, account: &str) -> Option<&Item> {
        self.items
            .iter()
            .find(|i| i.service == service && i.account == account)
    }
}

fn private_open_options() -> OpenOptions {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

/// Serialises the mutations of this process, whatever directory they address (a finer lock would
/// buy nothing: there is one credential directory per app).
static PROCESS_LOCK: Mutex<()> = Mutex::new(());

/// What a mutation holds: the in-process mutex guard and the open lock file (the `flock` is
/// released when the file closes). Fields drop in order: the file closes before the mutex frees.
type Held = (File, std::sync::MutexGuard<'static, ()>);

/// One bridge and one private directory.
struct Config {
    bridge: Arc<dyn KeystoreBridge>,
    dir: PathBuf,
}

impl Config {
    fn open(bridge: Arc<dyn KeystoreBridge>, files_dir: &Path) -> Result<Config> {
        if !files_dir.is_absolute() {
            return Err(Error::Invalid {
                what: "android files dir",
                reason: "not an absolute path".into(),
            });
        }
        let dir = files_dir.join(DIR_NAME);
        std::fs::create_dir_all(&dir)
            .map_err(|_| unavailable("cannot create the credential directory"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Best effort: the app sandbox already makes the directory private to the app.
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        Ok(Config { bridge, dir })
    }

    fn store_path(&self) -> PathBuf {
        self.dir.join(STORE_FILE)
    }

    fn load(&self) -> Result<StoreFile> {
        let bytes = match std::fs::read(self.store_path()) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(StoreFile::empty()),
            Err(_) => {
                return Err(Error::Backend {
                    op: "read store",
                    code: -6,
                })
            }
        };
        let file: StoreFile = serde_json::from_slice(&bytes).map_err(|_| Error::Backend {
            op: "parse store",
            code: -7,
        })?;
        if file.version != FORMAT_VERSION {
            return Err(Error::Backend {
                op: "store version",
                code: i64::from(file.version),
            });
        }
        Ok(file)
    }

    /// Write the whole file to a temporary name and rename it over the store, so a crash leaves
    /// the old file or the new one.
    fn save(&self, file: &StoreFile) -> Result<()> {
        let fail = |code: i64| Error::Backend {
            op: "write store",
            code,
        };
        let json = serde_json::to_vec(file).map_err(|_| fail(-9))?;
        let tmp = self.dir.join(TMP_FILE);
        let mut out = private_open_options()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|_| fail(-9))?;
        out.write_all(&json).map_err(|_| fail(-9))?;
        out.sync_all().map_err(|_| fail(-9))?;
        drop(out);
        std::fs::rename(&tmp, self.store_path()).map_err(|_| fail(-9))?;
        Ok(())
    }

    /// The exclusive per-directory lock: this process's mutex, then an `flock` for other
    /// processes. Both are released when the returned guard drops.
    fn lock(&self) -> Result<Held> {
        let in_process = PROCESS_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let file = private_open_options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.dir.join(LOCK_FILE))
            .map_err(|_| unavailable("cannot open the credential lock file"))?;
        match file.lock() {
            Ok(()) => {}
            // `File::lock` reports `Unsupported` on a target whose std has no `flock`. Whether
            // that is true of `target_os = "android"` has not been confirmed on a device; if it
            // is, exclusion between threads still holds (the mutex above) and a second process
            // writing the same directory is not excluded. Never taken on another target.
            Err(e)
                if cfg!(target_os = "android") && e.kind() == std::io::ErrorKind::Unsupported => {}
            Err(_) => {
                return Err(Error::Backend {
                    op: "lock",
                    code: -1,
                })
            }
        }
        Ok((file, in_process))
    }
}

/// The configuration [`install`] sets and [`AndroidKeystoreBackend::system`] reads.
static INSTALLED: Mutex<Option<Arc<Config>>> = Mutex::new(None);

fn installed() -> Option<Arc<Config>> {
    INSTALLED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Wire the process-wide Android store: the bridge that reaches the Keystore, and the app's
/// private files directory (`Context.getFilesDir()`, absolute). Call once at startup, before any
/// [`crate::Store`] is used. A second call replaces the first (stores already opened with
/// [`crate::system_backend`] follow the new configuration on their next call).
///
/// A relative path is [`Error::Invalid`]; a directory that cannot be created is
/// [`Error::Unavailable`]. Neither changes the installed configuration.
pub fn install(bridge: Arc<dyn KeystoreBridge>, files_dir: impl AsRef<Path>) -> Result<()> {
    let config = Config::open(bridge, files_dir.as_ref())?;
    *INSTALLED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::new(config));
    Ok(())
}

enum Source {
    Fixed(Arc<Config>),
    Installed,
}

/// The Android Keystore store. Compiled on every platform so its logic is testable on a host with
/// a fake [`KeystoreBridge`]; only an Android build has a real bridge, and
/// [`crate::system_backend`] returns it on `target_os = "android"` only.
pub struct AndroidKeystoreBackend {
    source: Source,
}

impl AndroidKeystoreBackend {
    /// A store over an explicit bridge and files directory (absolute).
    pub fn new(bridge: Arc<dyn KeystoreBridge>, files_dir: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            source: Source::Fixed(Arc::new(Config::open(bridge, files_dir.as_ref())?)),
        })
    }

    /// The store configured by [`install`], looked up on every call. Before [`install`] each call
    /// fails with [`Error::Unavailable`].
    pub fn system() -> Self {
        Self {
            source: Source::Installed,
        }
    }

    fn config(&self) -> Result<Arc<Config>> {
        match &self.source {
            Source::Fixed(config) => Ok(config.clone()),
            Source::Installed => installed().ok_or_else(|| {
                unavailable(
                    "the Android Keystore store is not installed (call rightkit_credentials::android::install)",
                )
            }),
        }
    }
}

struct Ops<'a> {
    cfg: &'a Config,
}

impl CredentialOps for Ops<'_> {
    type Guard = Held;

    fn lock(&self, _normalized_target: &str, _ns: &Namespace, _account: &str) -> Result<Held> {
        self.cfg.lock()
    }

    fn read(&self, ns: &Namespace, account: &str, want_blob: bool) -> Result<Option<Secret>> {
        let file = self.cfg.load()?;
        let Some(item) = file.find(ns.service(), account) else {
            return Ok(None);
        };
        if !want_blob {
            // Presence only: no bridge call, no secret bytes.
            return Ok(Some(Secret::from_bytes(Vec::new())));
        }
        let blob = hex_decode(&item.blob).ok_or(Error::Backend {
            op: "decode item",
            code: -8,
        })?;
        let plain = self
            .cfg
            .bridge
            .decrypt(&key_alias(ns), &blob, &aad(ns, account))
            .map_err(|e| map("decrypt", e))?;
        Ok(Some(Secret::from_bytes(plain)))
    }

    fn write(&self, ns: &Namespace, account: &str, secret: &Secret) -> Result<()> {
        let mut file = self.cfg.load()?;
        // Encrypt before touching the file: a missing context leaves nothing behind.
        let blob = self
            .cfg
            .bridge
            .encrypt(&key_alias(ns), secret.expose_bytes(), &aad(ns, account))
            .map_err(|e| map("encrypt", e))?;
        let blob = hex_encode(&blob);
        match file
            .items
            .iter_mut()
            .find(|i| i.service == ns.service() && i.account == account)
        {
            Some(item) => item.blob = blob,
            None => file.items.push(Item {
                service: ns.service().to_string(),
                account: account.to_string(),
                blob,
            }),
        }
        self.cfg.save(&file)
    }

    fn remove(&self, ns: &Namespace, account: &str) -> Result<bool> {
        let mut file = self.cfg.load()?;
        let before = file.items.len();
        file.items
            .retain(|i| !(i.service == ns.service() && i.account == account));
        if file.items.len() == before {
            return Ok(false);
        }
        self.cfg.save(&file)?;
        Ok(true)
    }
}

impl Backend for AndroidKeystoreBackend {
    fn name(&self) -> &'static str {
        "android-keystore"
    }
    fn exclusion_identity(&self) -> Option<String> {
        match &self.source {
            Source::Fixed(config) => Some(format!("android-keystore:{}", config.dir.display())),
            // `install` is process-wide: every such instance addresses the one installed store.
            Source::Installed => Some("android-keystore".to_string()),
        }
    }
    fn credential_identity(&self, ns: &Namespace, account: &str) -> Result<String> {
        check_target(ns, account)?;
        // Entries match exactly (case-sensitive): no folding.
        Ok(format!("android:{}\u{1f}{account}", ns.service()))
    }
    fn durable(&self) -> bool {
        true
    }
    fn get(&self, ns: &Namespace, account: &str, _: Interaction) -> Result<Option<Secret>> {
        check_target(ns, account)?;
        let cfg = self.config()?;
        Ops { cfg: &cfg }.read(ns, account, true)
    }
    fn exists(&self, ns: &Namespace, account: &str, _: Interaction) -> Result<bool> {
        check_target(ns, account)?;
        let cfg = self.config()?;
        Ok(Ops { cfg: &cfg }.read(ns, account, false)?.is_some())
    }
    fn set(&self, ns: &Namespace, account: &str, secret: &Secret, _: Interaction) -> Result<()> {
        check_target(ns, account)?;
        let cfg = self.config()?;
        locked::set(&Ops { cfg: &cfg }, ns, account, secret)
    }
    fn add_if_absent(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        _: Interaction,
    ) -> Result<bool> {
        check_target(ns, account)?;
        let cfg = self.config()?;
        locked::add_if_absent(&Ops { cfg: &cfg }, ns, account, secret)
    }
    fn delete(&self, ns: &Namespace, account: &str, _: Interaction) -> Result<bool> {
        check_target(ns, account)?;
        let cfg = self.config()?;
        locked::delete(&Ops { cfg: &cfg }, ns, account)
    }
    fn delete_if_equals(
        &self,
        ns: &Namespace,
        account: &str,
        expected: &Secret,
        _: Interaction,
    ) -> Result<ConditionalDelete> {
        check_target(ns, account)?;
        let cfg = self.config()?;
        locked::delete_if_equals(&Ops { cfg: &cfg }, ns, account, expected)
    }
}
