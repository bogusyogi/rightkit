//! Stable per-installation identity and append-only startup claims.
//!
//! The JSON schema and mirror paths intentionally match
//! `tools/lib/memory/installation_identity.py` so the resident Rust service and batch Python
//! synchronizer share one N-machine identity contract.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const IDENTITY_SCHEMA_VERSION: u32 = 2;
const CLAIM_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallationIdentity {
    pub schema_version: u32,
    pub installation_id: String,
    pub created_at: String,
    pub startup_generation: u64,
    pub lineage: Vec<String>,
    pub current_service_instance_id: Option<String>,
    pub current_claimed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupClaim {
    pub schema_version: u32,
    pub installation_id: String,
    pub startup_generation: u64,
    pub service_instance_id: String,
    pub claimed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallationPaths {
    pub identity: PathBuf,
    pub mirror: PathBuf,
}

impl InstallationPaths {
    pub fn defaults_for_workspace(workspace_root: &Path) -> Self {
        Self {
            identity: workspace_root.join("tools/.cache/memory/installation.json"),
            mirror: workspace_root.join("memory-mirror"),
        }
    }

    pub fn for_workspace(workspace_root: &Path) -> Self {
        let defaults = Self::defaults_for_workspace(workspace_root);
        Self {
            identity: crate::qa_env::installation_file()
                .map(PathBuf::from)
                .unwrap_or(defaults.identity),
            mirror: crate::qa_env::mirror()
                .map(PathBuf::from)
                .unwrap_or(defaults.mirror),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InstallationIdentityError {
    #[error("{operation} {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid installation identity: {0}")]
    InvalidIdentity(PathBuf),
    #[error("invalid startup claim: {0}")]
    InvalidClaim(PathBuf),
    #[error("{0}")]
    Invalid(String),
    #[error(
        "clone/snapshot collision quarantines installation {installation_id}; startup_generation={generations}; rotate identity with reason clone"
    )]
    Quarantined {
        installation_id: String,
        generations: String,
    },
}

fn io_error(
    operation: &'static str,
    path: &Path,
    source: std::io::Error,
) -> InstallationIdentityError {
    InstallationIdentityError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

fn valid_uuid4(value: &str) -> bool {
    let bytes = value.as_bytes();
    value.len() == 36
        && [8, 13, 18, 23].iter().all(|index| bytes[*index] == b'-')
        && bytes[14] == b'4'
        && matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
        && bytes.iter().enumerate().all(|(index, byte)| {
            [8, 13, 18, 23].contains(&index) || byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')
        })
}

fn require_uuid4(value: &str, field: &str) -> Result<(), InstallationIdentityError> {
    if valid_uuid4(value) {
        Ok(())
    } else {
        Err(InstallationIdentityError::Invalid(format!(
            "{field} must be a lowercase UUIDv4"
        )))
    }
}

fn valid_timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 20
        || !bytes.ends_with(b"Z")
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return false;
    }
    let decimal = |slice: &[u8]| -> Option<u32> {
        if slice.iter().all(u8::is_ascii_digit) {
            std::str::from_utf8(slice).ok()?.parse().ok()
        } else {
            None
        }
    };
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
        decimal(&bytes[0..4]),
        decimal(&bytes[5..7]),
        decimal(&bytes[8..10]),
        decimal(&bytes[11..13]),
        decimal(&bytes[14..16]),
        decimal(&bytes[17..19]),
    ) else {
        return false;
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    if year == 0 || day == 0 || day > max_day || hour > 23 || minute > 59 || second > 59 {
        return false;
    }
    bytes.len() == 20
        || (bytes.len() > 21
            && bytes.get(19) == Some(&b'.')
            && bytes[20..bytes.len() - 1].iter().all(u8::is_ascii_digit))
}

fn validate_identity(identity: &InstallationIdentity) -> Result<(), InstallationIdentityError> {
    if identity.schema_version != IDENTITY_SCHEMA_VERSION {
        return Err(InstallationIdentityError::Invalid(format!(
            "installation schema_version must be {IDENTITY_SCHEMA_VERSION}"
        )));
    }
    require_uuid4(&identity.installation_id, "installation_id")?;
    if !valid_timestamp(&identity.created_at) {
        return Err(InstallationIdentityError::Invalid(
            "created_at must be an ISO-8601 UTC timestamp".to_string(),
        ));
    }
    for lineage_id in &identity.lineage {
        require_uuid4(lineage_id, "lineage")?;
    }
    match (
        identity.current_service_instance_id.as_deref(),
        identity.current_claimed_at.as_deref(),
    ) {
        (None, None) => {}
        (Some(service_id), Some(claimed_at)) => {
            require_uuid4(service_id, "current_service_instance_id")?;
            if !valid_timestamp(claimed_at) {
                return Err(InstallationIdentityError::Invalid(
                    "current_claimed_at must be an ISO-8601 UTC timestamp".to_string(),
                ));
            }
        }
        _ => {
            return Err(InstallationIdentityError::Invalid(
                "current startup claim is partial".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_claim(claim: &StartupClaim) -> Result<(), InstallationIdentityError> {
    if claim.schema_version != CLAIM_SCHEMA_VERSION {
        return Err(InstallationIdentityError::Invalid(format!(
            "startup claim schema_version must be {CLAIM_SCHEMA_VERSION}"
        )));
    }
    require_uuid4(&claim.installation_id, "installation_id")?;
    require_uuid4(&claim.service_instance_id, "service_instance_id")?;
    if claim.startup_generation == 0 {
        return Err(InstallationIdentityError::Invalid(
            "startup_generation must be positive".to_string(),
        ));
    }
    if !valid_timestamp(&claim.claimed_at) {
        return Err(InstallationIdentityError::Invalid(
            "claimed_at must be an ISO-8601 UTC timestamp".to_string(),
        ));
    }
    Ok(())
}

/// Validate that a startup claim is the one currently held by this installation identity.
/// A structurally valid historical claim is not active after the identity advances to a new
/// startup generation.
pub fn validate_active_startup_claim(
    identity: &InstallationIdentity,
    claim: &StartupClaim,
) -> Result<(), InstallationIdentityError> {
    validate_identity(identity)?;
    validate_claim(claim)?;
    if claim.installation_id != identity.installation_id
        || claim.startup_generation != identity.startup_generation
        || identity.current_service_instance_id.as_deref()
            != Some(claim.service_instance_id.as_str())
        || identity.current_claimed_at.as_deref() != Some(claim.claimed_at.as_str())
    {
        return Err(InstallationIdentityError::Invalid(
            "startup claim is not the installation's active lease".to_string(),
        ));
    }
    Ok(())
}

fn encoded<T: Serialize>(value: &T) -> Result<Vec<u8>, InstallationIdentityError> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| InstallationIdentityError::Invalid(format!("encode JSON: {error}")))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn read_identity(path: &Path) -> Result<InstallationIdentity, InstallationIdentityError> {
    let bytes = fs::read(path).map_err(|error| io_error("read", path, error))?;
    let identity: InstallationIdentity = serde_json::from_slice(&bytes)
        .map_err(|_| InstallationIdentityError::InvalidIdentity(path.to_path_buf()))?;
    validate_identity(&identity)
        .map_err(|_| InstallationIdentityError::InvalidIdentity(path.to_path_buf()))?;
    Ok(identity)
}

/// The options of a durable publish: missing folders are created, the temp file and (on Unix) the
/// folder are synced, and on Windows the move is write-through. `replace` says whether an existing
/// file is replaced or refused.
fn publish_options(replace: rightkit_fs::ReplaceMode) -> rightkit_fs::AtomicWriteOptions {
    rightkit_fs::AtomicWriteOptions {
        create_parents: true,
        replace,
        // A destination another handle briefly holds (an antivirus scan, an indexer, a reader whose
        // close is not yet observed) fails a Windows move with a sharing or lock violation; the
        // published write retries those 19 times, 5 ms apart. Elsewhere the policy is inert.
        retry_sharing_violation: Some(rightkit_fs::RetryPolicy::new(
            19,
            std::time::Duration::from_millis(5),
        )),
        ..Default::default()
    }
}

/// Publishes `data` at `path` only if nothing is there: `Ok(true)` when this call wrote it,
/// `Ok(false)` when the file already existed (it is never written over).
fn write_exclusive(path: &Path, data: &[u8]) -> Result<bool, InstallationIdentityError> {
    match rightkit_fs::atomic_write_with_options(
        path,
        publish_options(rightkit_fs::ReplaceMode::NoReplace),
        |file| file.write_all(data),
    ) {
        Ok(()) => Ok(true),
        Err(rightkit_fs::FsError::AlreadyExists(_)) => Ok(false),
        Err(error) => Err(io_error(
            "publish immutable file",
            path,
            std::io::Error::from(error),
        )),
    }
}

/// Durable publish of `data` at `path` through `rightkit-fs`: temp file, sync, replace (on Windows
/// `MoveFileExW` with write-through, retried while a scanner or reader briefly holds the destination:
/// 19 retries, 5 ms apart, sharing violations and access denied), directory sync.
/// `pub(crate)` so other durable-write call sites in this crate (see
/// `context_telemetry::persist_ingress_cursor`) share the one publish.
pub(crate) fn atomic_replace(path: &Path, data: &[u8]) -> Result<(), InstallationIdentityError> {
    rightkit_fs::atomic_write_with_access_denied_retry(
        path,
        replace_options(),
        replace_retry(),
        |file| file.write_all(data),
    )
    .map_err(|error| io_error("atomically replace", path, std::io::Error::from(error)))
}

fn replace_options() -> rightkit_fs::AtomicWriteOptions {
    rightkit_fs::AtomicWriteOptions {
        create_parents: true,
        replace: rightkit_fs::ReplaceMode::MoveWriteThrough,
        ..Default::default()
    }
}

fn replace_retry() -> rightkit_fs::AccessDeniedRetry {
    rightkit_fs::AccessDeniedRetry::new(rightkit_fs::RetryPolicy::new(
        19,
        std::time::Duration::from_millis(5),
    ))
}

/// Execute a credential/identity migration while holding the same OS lock used
/// by startup claims. The lock path is derived from `path`, so every owner
/// shares one cross-process primitive.
pub fn with_identity_lock<T>(
    path: &Path,
    operation: impl FnOnce() -> Result<T, InstallationIdentityError>,
) -> Result<T, InstallationIdentityError> {
    let _lock = IdentityLock::acquire(path)?;
    operation()
}

/// Compare the current file bytes by SHA-256, then publish atomically only if
/// they are unchanged. Caller must hold `with_identity_lock` for the full
/// read/modify/write sequence.
pub fn atomic_replace_prepared_if_fingerprint(
    path: &Path,
    expected_sha256: &str,
    candidate: &Path,
) -> Result<bool, InstallationIdentityError> {
    let current =
        fs::read(path).map_err(|error| io_error("read compare-and-replace target", path, error))?;
    let actual = rightkit_fs::digest::sha256_bytes(&current);
    if actual != expected_sha256 {
        return Ok(false);
    }
    rightkit_fs::publish_file_with_access_denied_retry(
        candidate,
        path,
        replace_options(),
        replace_retry(),
    )
    .map_err(|error| io_error("atomically replace", path, std::io::Error::from(error)))?;
    Ok(true)
}

struct IdentityLock {
    _lock: rightkit_fs::lock::FileLock,
}

impl IdentityLock {
    /// Exclusive, blocking cross-process lock owned by `rightkit-fs`; released when the guard drops
    /// (or the process dies). The wait is unbounded, as the migration callers expect.
    fn acquire(identity_path: &Path) -> Result<Self, InstallationIdentityError> {
        let lock_path = identity_path.with_extension(format!(
            "{}lock",
            identity_path
                .extension()
                .and_then(|value| value.to_str())
                .map(|value| format!("{value}."))
                .unwrap_or_default()
        ));
        let parent = lock_path.parent().ok_or_else(|| {
            InstallationIdentityError::Invalid(format!(
                "lock path has no parent: {}",
                lock_path.display()
            ))
        })?;
        fs::create_dir_all(parent)
            .map_err(|error| io_error("create lock directory", parent, error))?;
        let options = rightkit_fs::lock::LockOptions::wait(std::time::Duration::MAX)
            .labeled("installation-identity");
        let lock = rightkit_fs::lock::FileLock::acquire(&lock_path, &options)
            .map_err(|error| io_error("lock identity", &lock_path, error.into()))?;
        Ok(Self { _lock: lock })
    }
}

fn new_identity(lineage: Vec<String>) -> Result<InstallationIdentity, InstallationIdentityError> {
    let identity = InstallationIdentity {
        schema_version: IDENTITY_SCHEMA_VERSION,
        installation_id: rightkit_logs::identity::random_uuid(),
        created_at: crate::time::now_iso(),
        startup_generation: 0,
        lineage,
        current_service_instance_id: None,
        current_claimed_at: None,
    };
    validate_identity(&identity)?;
    Ok(identity)
}

pub fn load_or_create_installation(
    path: &Path,
) -> Result<InstallationIdentity, InstallationIdentityError> {
    if !path.exists() {
        let candidate = new_identity(Vec::new())?;
        if write_exclusive(path, &encoded(&candidate)?)? {
            return Ok(candidate);
        }
    }
    read_identity(path)
}

fn claim_path(mirror_root: &Path, claim: &StartupClaim) -> PathBuf {
    mirror_root
        .join("_installation_claims")
        .join(&claim.installation_id)
        .join(format!("{:020}", claim.startup_generation))
        .join(format!("{}.json", claim.service_instance_id))
}

fn publish_claim(
    mirror_root: &Path,
    claim: &StartupClaim,
) -> Result<PathBuf, InstallationIdentityError> {
    validate_claim(claim)?;
    let path = claim_path(mirror_root, claim);
    let data = encoded(claim)?;
    if !write_exclusive(&path, &data)? {
        let existing =
            fs::read(&path).map_err(|error| io_error("read startup claim", &path, error))?;
        if existing != data {
            return Err(InstallationIdentityError::Invalid(format!(
                "immutable startup claim collision: {}",
                path.display()
            )));
        }
    }
    Ok(path)
}

fn read_claim(path: &Path) -> Result<StartupClaim, InstallationIdentityError> {
    let bytes = fs::read(path).map_err(|error| io_error("read startup claim", path, error))?;
    let claim: StartupClaim = serde_json::from_slice(&bytes)
        .map_err(|_| InstallationIdentityError::InvalidClaim(path.to_path_buf()))?;
    validate_claim(&claim)
        .map_err(|_| InstallationIdentityError::InvalidClaim(path.to_path_buf()))?;
    Ok(claim)
}

pub fn assert_installation_not_quarantined(
    mirror_root: &Path,
    installation_id: &str,
) -> Result<(), InstallationIdentityError> {
    require_uuid4(installation_id, "installation_id")?;
    let root = mirror_root
        .join("_installation_claims")
        .join(installation_id);
    if !root.exists() {
        return Ok(());
    }
    let mut collisions = Vec::new();
    for generation_entry in
        fs::read_dir(&root).map_err(|error| io_error("read claim generations", &root, error))?
    {
        let generation_entry =
            generation_entry.map_err(|error| io_error("read claim generation", &root, error))?;
        if !generation_entry
            .file_type()
            .map_err(|error| io_error("inspect claim generation", &generation_entry.path(), error))?
            .is_dir()
        {
            continue;
        }
        let generation_name = generation_entry.file_name().to_string_lossy().to_string();
        if generation_name.len() != 20 || !generation_name.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(InstallationIdentityError::InvalidClaim(
                generation_entry.path(),
            ));
        }
        let generation: u64 = generation_name
            .parse()
            .map_err(|_| InstallationIdentityError::InvalidClaim(generation_entry.path()))?;
        let generation_path = generation_entry.path();
        let mut service_ids = BTreeSet::new();
        for claim_entry in fs::read_dir(&generation_path)
            .map_err(|error| io_error("read startup claims", &generation_path, error))?
        {
            let claim_entry = claim_entry
                .map_err(|error| io_error("read startup claim", &generation_path, error))?;
            let path = claim_entry.path();
            if !claim_entry
                .file_type()
                .map_err(|error| io_error("inspect startup claim", &path, error))?
                .is_file()
                || path.extension().and_then(|value| value.to_str()) != Some("json")
            {
                continue;
            }
            let claim = read_claim(&path)?;
            if claim.installation_id != installation_id
                || claim.startup_generation != generation
                || path.file_stem().and_then(|value| value.to_str())
                    != Some(claim.service_instance_id.as_str())
                || claim_path(mirror_root, &claim) != path
            {
                return Err(InstallationIdentityError::InvalidClaim(path));
            }
            service_ids.insert(claim.service_instance_id);
        }
        if service_ids.len() > 1 {
            collisions.push(generation);
        }
    }
    if collisions.is_empty() {
        Ok(())
    } else {
        collisions.sort_unstable();
        Err(InstallationIdentityError::Quarantined {
            installation_id: installation_id.to_string(),
            generations: collisions
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(","),
        })
    }
}

pub fn start_and_publish(
    identity_path: &Path,
    mirror_root: &Path,
    service_instance_id: Option<&str>,
) -> Result<(InstallationIdentity, StartupClaim), InstallationIdentityError> {
    let service_instance_id = match service_instance_id {
        Some(value) => {
            require_uuid4(value, "service_instance_id")?;
            value.to_string()
        }
        None => rightkit_logs::identity::random_uuid(),
    };
    let _lock = IdentityLock::acquire(identity_path)?;
    let identity = load_or_create_installation(identity_path)?;
    assert_installation_not_quarantined(mirror_root, &identity.installation_id)?;
    let startup_generation = identity.startup_generation.checked_add(1).ok_or_else(|| {
        InstallationIdentityError::Invalid("startup_generation overflow".to_string())
    })?;
    let claimed_at = crate::time::now_iso();
    let advanced = InstallationIdentity {
        startup_generation,
        current_service_instance_id: Some(service_instance_id.clone()),
        current_claimed_at: Some(claimed_at.clone()),
        ..identity
    };
    atomic_replace(identity_path, &encoded(&advanced)?)?;
    let claim = StartupClaim {
        schema_version: CLAIM_SCHEMA_VERSION,
        installation_id: advanced.installation_id.clone(),
        startup_generation,
        service_instance_id,
        claimed_at,
    };
    publish_claim(mirror_root, &claim)?;
    assert_installation_not_quarantined(mirror_root, &advanced.installation_id)?;
    Ok((advanced, claim))
}

pub fn prepare_service_start(
    workspace_root: &Path,
) -> Result<(InstallationIdentity, StartupClaim), InstallationIdentityError> {
    let paths = InstallationPaths::for_workspace(workspace_root);
    start_and_publish(&paths.identity, &paths.mirror, None)
}

pub fn rotate_installation(
    path: &Path,
    reason: &str,
) -> Result<InstallationIdentity, InstallationIdentityError> {
    if reason != "clone" {
        return Err(InstallationIdentityError::Invalid(
            "rotation reason must be exactly 'clone'".to_string(),
        ));
    }
    let _lock = IdentityLock::acquire(path)?;
    let old = read_identity(path)?;
    let archive = path
        .parent()
        .ok_or_else(|| {
            InstallationIdentityError::Invalid(format!("path has no parent: {}", path.display()))
        })?
        .join("installation-archive")
        .join(format!("{}.json", old.installation_id));
    let mut archive_value = serde_json::to_value(&old)
        .map_err(|error| InstallationIdentityError::Invalid(format!("encode archive: {error}")))?;
    let object = archive_value.as_object_mut().ok_or_else(|| {
        InstallationIdentityError::Invalid("installation archive must be an object".to_string())
    })?;
    object.insert(
        "rotation_reason".to_string(),
        serde_json::Value::String("clone".to_string()),
    );
    object.insert(
        "rotated_at".to_string(),
        serde_json::Value::String(crate::time::now_iso()),
    );
    let archive_data = encoded(&archive_value)?;
    if !write_exclusive(&archive, &archive_data)? {
        let existing: serde_json::Value = serde_json::from_slice(
            &fs::read(&archive).map_err(|error| io_error("read archive", &archive, error))?,
        )
        .map_err(|_| InstallationIdentityError::InvalidIdentity(archive.clone()))?;
        if existing
            .get("installation_id")
            .and_then(|value| value.as_str())
            != Some(old.installation_id.as_str())
            || existing
                .get("rotation_reason")
                .and_then(|value| value.as_str())
                != Some("clone")
        {
            return Err(InstallationIdentityError::InvalidIdentity(archive));
        }
    }
    let mut lineage = old.lineage.clone();
    if !lineage.contains(&old.installation_id) {
        lineage.push(old.installation_id);
    }
    let rotated = new_identity(lineage)?;
    atomic_replace(path, &encoded(&rotated)?)?;
    Ok(rotated)
}
