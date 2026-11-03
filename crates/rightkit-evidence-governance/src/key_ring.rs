use crate::error::GovernanceError;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Default environment override for the host key ring directory.
pub const KEY_DIR_ENV: &str = "RIGHTKIT_KEY_DIR";
/// Default command that provisions a missing ring.
pub const KEYS_INIT_COMMAND: &str = "keys init";
/// Key id minted by the keys-init command. Fixed so that exclusive creation of
/// the key file serialises concurrent initialisations.
pub const INIT_KEY_ID: &str = "host-primary";

/// Application-owned names for the host key ring. The consuming application
/// declares them once with [`set_key_ring_identity`]; otherwise the neutral
/// default (`RIGHTKIT_KEY_DIR`, `~/.rightkit/keys`, `keys init`) applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRingIdentity {
    /// Environment variable that overrides the key directory.
    pub env_var: String,
    /// Path segments of the canonical ring under the user's home directory.
    pub home_relative_dir: Vec<String>,
    /// The command a user runs to provision a missing ring.
    pub init_command: String,
}

impl Default for KeyRingIdentity {
    fn default() -> Self {
        Self {
            env_var: KEY_DIR_ENV.to_string(),
            home_relative_dir: vec![".rightkit".to_string(), "keys".to_string()],
            init_command: KEYS_INIT_COMMAND.to_string(),
        }
    }
}

static KEY_RING_IDENTITY: OnceLock<KeyRingIdentity> = OnceLock::new();

/// Declare the application's key ring identity. The first call wins; it
/// returns `false` when an identity was already set.
pub fn set_key_ring_identity(identity: KeyRingIdentity) -> bool {
    KEY_RING_IDENTITY.set(identity).is_ok()
}

/// The key ring identity in effect for this process.
pub fn key_ring_identity() -> &'static KeyRingIdentity {
    KEY_RING_IDENTITY.get_or_init(KeyRingIdentity::default)
}

/// The command that provisions a missing ring for this process.
pub fn keys_init_command() -> &'static str {
    &key_ring_identity().init_command
}

/// Where a resolved key directory came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyDirSource {
    Explicit,
    Environment,
    Canonical,
}

/// The canonical host ring: the identity's home-relative directory under
/// `$HOME` (`%USERPROFILE%` on Windows).
pub fn canonical_key_dir() -> Result<PathBuf, GovernanceError> {
    let home = std::env::var_os("USERPROFILE")
        .filter(|value| !value.is_empty())
        .or_else(|| std::env::var_os("HOME").filter(|value| !value.is_empty()))
        .ok_or_else(|| {
            GovernanceError::typed("ARC_AUTH_KEY_UNAVAILABLE", "host home unavailable")
        })?;
    let mut dir = PathBuf::from(home);
    for segment in &key_ring_identity().home_relative_dir {
        dir.push(segment);
    }
    Ok(dir)
}

/// The single key-directory resolution used by `keys init`, `doctor`,
/// `contract seal`, `run open` and the canonical loader. Precedence: explicit
/// path (e.g. `--key-dir`), then the identity's environment variable, then the
/// canonical ring.
pub fn resolve_key_dir(
    explicit: Option<&Path>,
) -> Result<(PathBuf, KeyDirSource), GovernanceError> {
    if let Some(path) = explicit.filter(|path| !path.as_os_str().is_empty()) {
        return Ok((path.to_path_buf(), KeyDirSource::Explicit));
    }
    if let Some(value) =
        std::env::var_os(&key_ring_identity().env_var).filter(|value| !value.is_empty())
    {
        return Ok((PathBuf::from(value), KeyDirSource::Environment));
    }
    Ok((canonical_key_dir()?, KeyDirSource::Canonical))
}

/// Exact prerequisite text for a ring that is missing, empty or unreadable.
pub fn missing_ring_message(dir: &Path, reason: &str) -> String {
    format!(
        "host key ring unavailable at {} ({reason}); initialize it with `{}`",
        dir.display(),
        keys_init_command()
    )
}

/// Result of a successful `init_ring`.
#[derive(Debug, Clone)]
pub struct InitializedRing {
    pub dir: PathBuf,
    pub key_id: String,
    pub key_path: PathBuf,
}

fn key_error(message: impl Into<String>) -> GovernanceError {
    GovernanceError::typed("ARC_AUTH_KEY_UNAVAILABLE", message)
}

#[derive(Clone)]
pub struct KeyRing {
    keys: RefCell<BTreeMap<String, KeyRecord>>,
}

#[derive(Clone)]
struct KeyRecord {
    key: Vec<u8>,
    created_at: String,
    #[allow(dead_code)]
    custody: String,
    status: String,
}

impl KeyRing {
    /// Load the single host-custody ring used when a caller does not provide
    /// an adapter-specific directory.  Keep this path identical to Node's
    /// `loadCanonicalHostKeyRing` on every supported host.
    pub fn load_canonical() -> Result<Self, GovernanceError> {
        let (dir, _) = resolve_key_dir(None)?;
        Self::load_dir(&dir)
    }

    pub fn load_dir(dir: &Path) -> Result<Self, GovernanceError> {
        match std::fs::symlink_metadata(dir) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(key_error(format!(
                    "host key directory is a symlink and is refused: {}",
                    dir.display()
                )));
            }
            Ok(meta) if meta.is_dir() => {}
            _ => {
                return Err(key_error(missing_ring_message(
                    dir,
                    "host key directory not found",
                )));
            }
        }
        let ring = Self {
            keys: RefCell::new(BTreeMap::new()),
        };
        let entries =
            std::fs::read_dir(dir).map_err(|error| GovernanceError::Io(error.to_string()))?;
        let mut found = false;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("key") {
                continue;
            }
            found = true;
            check_key_file(&path)?;
            let key_id = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or_default()
                .to_owned();
            let hex = std::fs::read_to_string(&path)
                .map_err(|error| GovernanceError::Io(error.to_string()))?
                .trim()
                .to_owned();
            let key_material = hex::decode(hex).map_err(|_| {
                GovernanceError::typed(
                    "ARC_AUTH_KEY_UNAVAILABLE",
                    format!("key material unreadable for {key_id}"),
                )
            })?;
            if key_material.is_empty() {
                return Err(GovernanceError::typed(
                    "ARC_AUTH_KEY_UNAVAILABLE",
                    format!("key material unreadable for {key_id}"),
                ));
            }
            let meta_path = dir.join(format!("{key_id}.json"));
            let custody = format!("host-file:{key_id}.key");
            let (created_at, status) = if meta_path.is_file() {
                let meta = std::fs::read_to_string(&meta_path)
                    .map_err(|error| GovernanceError::Io(error.to_string()))?;
                let value: serde_json::Value = serde_json::from_str(&meta)
                    .map_err(|error| GovernanceError::Io(error.to_string()))?;
                (
                    value
                        .get("createdAt")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_owned(),
                    value
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("active")
                        .to_owned(),
                )
            } else {
                (String::new(), "active".into())
            };
            ring.add(key_id, key_material, created_at, custody, status)?;
        }
        if !found {
            return Err(key_error(missing_ring_message(
                dir,
                "no host key material found",
            )));
        }
        Ok(ring)
    }

    /// Create the ring at `dir` with one new key. Never overwrites: a ring
    /// that already holds any `*.key` file, or a symlinked directory or key
    /// path, is refused. Unix: directory 0700, files 0600. Other platforms
    /// fail closed (no owner-only ACL helper exists in this workspace).
    /// `key_material` must come from a cryptographic RNG (>= 32 bytes).
    pub fn init_ring(
        dir: &Path,
        key_material: &[u8],
        created_at: &str,
    ) -> Result<InitializedRing, GovernanceError> {
        init_ring_impl(dir, key_material, created_at)
    }

    pub fn add(
        &self,
        key_id: impl Into<String>,
        key_material: Vec<u8>,
        created_at: impl Into<String>,
        custody: impl Into<String>,
        status: impl Into<String>,
    ) -> Result<(), GovernanceError> {
        let key_id = key_id.into();
        if key_id.is_empty() {
            return Err(GovernanceError::typed(
                "ARC_AUTH_KEY_UNAVAILABLE",
                "keyId must be a non-empty string",
            ));
        }
        if key_material.is_empty() {
            return Err(GovernanceError::typed(
                "ARC_AUTH_KEY_UNAVAILABLE",
                format!("keyMaterial must be a non-empty buffer for {key_id}"),
            ));
        }
        self.keys.borrow_mut().insert(
            key_id,
            KeyRecord {
                key: key_material,
                created_at: created_at.into(),
                custody: custody.into(),
                status: status.into(),
            },
        );
        Ok(())
    }

    pub fn has(&self, key_id: &str) -> bool {
        self.keys.borrow().contains_key(key_id)
    }

    pub fn get(&self, key_id: &str) -> Result<KeyHandle, GovernanceError> {
        if !self.has(key_id) {
            if let Some((root_key_id, role, purpose)) = parse_derived_key_id(key_id) {
                let root = self.keys.borrow().get(&root_key_id).cloned();
                if let Some(root) = root {
                    if root.status != "revoked" {
                        let mac_domain = format!("arcane-authority-proof:v1:{role}:{purpose}");
                        let material = derive_key(&root.key, &mac_domain);
                        self.add(
                            key_id,
                            material,
                            root.created_at.clone(),
                            format!("derived:{root_key_id}:{mac_domain}"),
                            "active",
                        )?;
                    }
                }
            }
        }
        let record = self.keys.borrow().get(key_id).cloned().ok_or_else(|| {
            GovernanceError::typed(
                "ARC_AUTH_KEY_UNAVAILABLE",
                format!("key unavailable: {key_id}"),
            )
        })?;
        if record.status == "revoked" {
            return Err(GovernanceError::typed(
                "ARC_AUTH_KEY_UNAVAILABLE",
                format!("key unavailable: {key_id}"),
            ));
        }
        Ok(KeyHandle {
            key_id: key_id.to_owned(),
            key: record.key,
        })
    }

    pub fn active_key_id(&self) -> Result<String, GovernanceError> {
        let active = self
            .keys
            .borrow()
            .iter()
            .filter(|(_, record)| record.status != "revoked")
            .max_by(|left, right| left.1.created_at.cmp(&right.1.created_at))
            .map(|(key_id, _)| key_id.clone());
        active.ok_or_else(|| {
            GovernanceError::typed("ARC_AUTH_KEY_UNAVAILABLE", "no active key available")
        })
    }
}

/// Refuse a key file that is a symlink, not a regular file, or (unix) readable
/// by group or others.
fn check_key_file(path: &Path) -> Result<(), GovernanceError> {
    let meta =
        std::fs::symlink_metadata(path).map_err(|error| GovernanceError::Io(error.to_string()))?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(key_error(format!(
            "key file is a symlink or not a regular file and is refused: {}",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(key_error(format!(
                "key file {} is accessible by group or others (mode {mode:03o}); refusing to load it. Fix with `chmod 600 {}` (and `chmod 700` on its directory), or re-provision with `{}`",
                path.display(),
                path.display(),
                keys_init_command()
            )));
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn init_ring_impl(
    dir: &Path,
    _key_material: &[u8],
    _created_at: &str,
) -> Result<InitializedRing, GovernanceError> {
    Err(key_error(format!(
        "{} is unsupported on this platform: owner-only ACL provisioning is not implemented, so no key was written to {}",
        keys_init_command(),
        dir.display()
    )))
}

#[cfg(unix)]
fn init_ring_impl(
    dir: &Path,
    key_material: &[u8],
    created_at: &str,
) -> Result<InitializedRing, GovernanceError> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

    if key_material.len() < 32 {
        return Err(key_error("key material must be at least 32 bytes"));
    }
    let io = |error: std::io::Error| GovernanceError::Io(error.to_string());
    if let Some(parent) = dir.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(io)?;
    }
    // Create the final component exclusively with 0700. `mkdir` fails on an
    // existing path including a symlink, so a planted link is never followed.
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let meta = std::fs::symlink_metadata(dir).map_err(io)?;
            if meta.file_type().is_symlink() {
                return Err(key_error(format!(
                    "key directory is a symlink and is refused: {}",
                    dir.display()
                )));
            }
            if !meta.is_dir() {
                return Err(key_error(format!(
                    "key directory path is not a directory: {}",
                    dir.display()
                )));
            }
            for entry in std::fs::read_dir(dir).map_err(io)?.flatten() {
                if entry.path().extension().and_then(|ext| ext.to_str()) == Some("key") {
                    return Err(key_error(format!(
                        "key ring already initialized at {}; existing keys are never overwritten",
                        dir.display()
                    )));
                }
            }
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(io)?;
        }
        Err(error) => return Err(io(error)),
    }

    let key_path = dir.join(format!("{INIT_KEY_ID}.key"));
    let meta_path = dir.join(format!("{INIT_KEY_ID}.json"));
    let exclusive = |path: &Path| {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
    };
    let describe = json_meta(created_at);
    let mut meta_file = exclusive(&meta_path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            key_error(format!(
                "key ring already initialized at {}; existing keys are never overwritten",
                dir.display()
            ))
        } else {
            io(error)
        }
    })?;
    let wrote_meta = meta_file
        .write_all(describe.as_bytes())
        .and_then(|_| meta_file.sync_all());
    let mut key_file = match wrote_meta.and_then(|_| exclusive(&key_path)) {
        Ok(file) => file,
        Err(error) => {
            let _ = std::fs::remove_file(&meta_path);
            return Err(if error.kind() == std::io::ErrorKind::AlreadyExists {
                key_error(format!(
                    "key ring already initialized at {}; existing keys are never overwritten",
                    dir.display()
                ))
            } else {
                io(error)
            });
        }
    };
    if let Err(error) = key_file
        .write_all(hex::encode(key_material).as_bytes())
        .and_then(|_| key_file.sync_all())
    {
        drop(key_file);
        let _ = std::fs::remove_file(&key_path);
        let _ = std::fs::remove_file(&meta_path);
        return Err(io(error));
    }
    Ok(InitializedRing {
        dir: dir.to_path_buf(),
        key_id: INIT_KEY_ID.to_owned(),
        key_path,
    })
}

#[cfg(unix)]
fn json_meta(created_at: &str) -> String {
    serde_json::json!({"keyId": INIT_KEY_ID, "createdAt": created_at, "status": "active"})
        .to_string()
}

pub struct KeyHandle {
    pub key_id: String,
    pub key: Vec<u8>,
}

fn parse_derived_key_id(key_id: &str) -> Option<(String, String, String)> {
    let (root_key_id, rest) = key_id.split_once(":authority-proof:")?;
    let (role, purpose) = rest.split_once(':')?;
    if root_key_id.is_empty() || role.is_empty() || purpose.is_empty() {
        return None;
    }
    Some((root_key_id.to_owned(), role.to_owned(), purpose.to_owned()))
}

fn derive_key(root: &[u8], mac_domain: &str) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(root).expect("hmac key");
    mac.update(b"arcane-key-derivation:v1\0");
    mac.update(mac_domain.as_bytes());
    mac.finalize().into_bytes().to_vec()
}
