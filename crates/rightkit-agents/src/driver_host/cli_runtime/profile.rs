//! Managed account profiles for vendor CLIs.
//!
//! PR 2 of the architecture plan. A profile is a vendor-owned login
//! location, not a the host app secret store. the host app stores only the
//! identifier, the managed home directory, and a one-way account
//! fingerprint derived from non-secret metadata. Credentials are
//! written and consumed exclusively by the vendor CLI launched under
//! `managed_home`.
//!
//! Each adapter is responsible for:
//!
//! - mapping its own canonical home path (for example Claude's
//!   `~/.claude` and Codex's `~/.codex`) onto a profile directory under
//!   `~/<state-dir>/cli-profiles/<provider>/<label>/`, and
//! - launching the vendor CLI with that directory exposed through
//!   `HOME`/`XDG_CONFIG_HOME`/its vendor-specific home variable so the
//!   credential file the vendor creates lands inside the managed
//!   profile, not the user's global shell environment.
//!
//! The lifecycle of a profile is: `ensure_managed_home` → `begin_login`
//! → `probe_status` → either `signed_in` or `signed_out`. Logout is
//! `delete_profile_dir`, which removes the entire profile tree (the
//! vendor's own credential files included) so the next launch starts
//! from a clean slate.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::driver_host::cli_runtime::ids::{CliAccountProfile, CliAccountProfileId, CliProviderId};

/// Error type for the managed profile store. Every variant points at a
/// class of failure that the caller can recover from or surface to the
/// UI — there is no `Other` catch-all on purpose.
#[derive(Debug)]
pub enum ProfileStoreError {
    /// Filesystem I/O failure (create, read, write, remove).
    Io(std::io::Error),
    /// The profile already exists and the caller did not opt in to
    /// overwriting it.
    AlreadyExists {
        provider: CliProviderId,
        label: String,
    },
    /// The requested profile could not be located under
    /// `managed_profile_root`.
    UnknownProfile {
        provider: CliProviderId,
        label: String,
    },
    /// The label is empty, contains a path separator, or otherwise
    /// unsafe for use as a filesystem path component.
    InvalidLabel(String),
    /// A BYOK or external-API environment variable was detected in the
    /// ambient environment. Vendor CLIs must not see it; refuse the
    /// operation rather than paper over the misconfiguration.
    ForbiddenByokEnvDetected(String),
}

impl std::fmt::Display for ProfileStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProfileStoreError::Io(err) => write!(f, "profile i/o: {err}"),
            ProfileStoreError::AlreadyExists { provider, label } => write!(
                f,
                "profile {provider}/{label} already exists",
                provider = provider.as_str(),
                label = label
            ),
            ProfileStoreError::UnknownProfile { provider, label } => write!(
                f,
                "no profile for {provider}/{label}",
                provider = provider.as_str(),
                label = label
            ),
            ProfileStoreError::InvalidLabel(reason) => write!(f, "invalid profile label: {reason}"),
            ProfileStoreError::ForbiddenByokEnvDetected(name) => {
                write!(f, "forbidden BYOK env var present: {name}")
            }
        }
    }
}

impl std::error::Error for ProfileStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ProfileStoreError::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ProfileStoreError {
    fn from(err: std::io::Error) -> Self {
        ProfileStoreError::Io(err)
    }
}

/// One entry in the per-provider home layout table. Each entry
/// answers: "given a label, where does the vendor CLI expect to find
/// its config home?" — the answer is mapped by the launcher onto the
/// managed profile root, so the credential file the vendor creates
/// always lands under `~/<state-dir>/cli-profiles/`, never in the
/// user's global shell environment.
pub type ProfileHomeFn = fn(&str) -> PathBuf;

/// Canonical mapping from `(provider_id, label)` to a vendor-native
/// home directory the CLI will be told to use. Each closure takes the
/// user-supplied label (already sanitized) and returns the absolute
/// path the vendor would have read by default — e.g. Claude Code reads
/// `~/.claude/`, Codex reads `~/.codex/`. The launch layer relocates
/// this under the managed root before spawning the subprocess.
pub const PROFILE_LAYOUTS: &[(&str, ProfileHomeFn)] = &[
    ("claude", claude_home_for_label),
    ("codex", codex_home_for_label),
];

/// Compute the Claude-managed home for one label. The actual directory
/// lives under [`managed_profile_root`]; this function exists so the
/// legacy vendor CLI receives the path it expects to read.
fn claude_home_for_label(_label: &str) -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".claude");
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        return PathBuf::from(profile).join(".claude");
    }
    PathBuf::from(".claude")
}

/// Compute the Codex-managed home for one label.
fn codex_home_for_label(_label: &str) -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".codex");
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        return PathBuf::from(profile).join(".codex");
    }
    PathBuf::from(".codex")
}

/// Resolve the per-provider home layout function for a provider id.
/// Returns `None` if the provider has no registered layout (for
/// example a future vendor with a non-standard home convention).
pub fn home_layout(provider: &CliProviderId) -> Option<ProfileHomeFn> {
    PROFILE_LAYOUTS
        .iter()
        .find(|(id, _)| *id == provider.as_str())
        .map(|(_, f)| *f)
}

/// Sanitize a profile label. Labels must be lowercase ASCII alphanumerics
/// plus `-`, `_`, and `.`; everything else is rejected so a label can
/// never escape its `<provider>/<label>` parent directory.
pub fn sanitize_label(label: &str) -> Result<&str, ProfileStoreError> {
    if label.is_empty() {
        return Err(ProfileStoreError::InvalidLabel(
            "label must not be empty".into(),
        ));
    }
    if label.len() > 64 {
        return Err(ProfileStoreError::InvalidLabel(format!(
            "label must be <= 64 chars, got {}",
            label.len()
        )));
    }
    if !label
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
    {
        return Err(ProfileStoreError::InvalidLabel(format!(
            "label {label:?} must be lowercase a-z, 0-9, '-', '_', '.' only"
        )));
    }
    Ok(label)
}

/// The canonical root for managed profiles. Each provider gets its
/// own subdirectory; per-profile directories live beneath that.
pub fn managed_profile_root() -> PathBuf {
    rightkit_app_settings::agent_config::state_root()
        .unwrap_or_else(|| PathBuf::from(".").join(".rightkit"))
        .join("cli-profiles")
}

/// Path to the default profile home for one provider.
pub fn default_profile_home(provider: &CliProviderId) -> PathBuf {
    managed_profile_root()
        .join(provider.as_str())
        .join("default")
}

/// Default profile identifier for one provider. Profiles are
/// keyed by `<provider>/<label>`. `default` is always present
/// after the first native login completes.
pub fn default_profile_id(provider: &CliProviderId) -> CliAccountProfileId {
    CliAccountProfileId::new(format!("{}/default", provider.as_str()))
}

/// Construct a fresh profile record. `managed_home` is the directory
/// the vendor CLI will be launched with; the host app creates it on
/// first login if it does not exist.
pub fn profile_record(
    provider: &CliProviderId,
    label: impl Into<String>,
    managed_home: PathBuf,
) -> CliAccountProfile {
    let label = label.into();
    CliAccountProfile {
        id: CliAccountProfileId::new(format!("{}/{}", provider.as_str(), label)),
        provider_id: provider.clone(),
        label,
        managed_home,
        auth_mode: crate::driver_host::cli_runtime::ids::CliAuthMode::Unknown,
        signed_in: false,
        account_fingerprint: None,
        last_verified_at: None,
    }
}

/// Ensure the managed home directory exists. Adapters call this
/// before launching the vendor CLI so first-time login does not
/// fail because of a missing directory. Returns the canonical home.
pub fn ensure_managed_home(home: &Path) -> std::io::Result<PathBuf> {
    rightkit_fs::ensure_private_dir(home).map_err(std::io::Error::other)?;
    Ok(home.to_path_buf())
}

/// A one-way account fingerprint derived from non-secret metadata
/// the vendor CLI exposes (typically an account label or stable
/// opaque identifier returned by a status probe). the host app stores
/// only this fingerprint, not the underlying identifier.
pub fn fingerprint_from_label(label: &str) -> String {
    let mut h = rightkit_fs::digest::Sha256Hasher::new();
    h.update(label.as_bytes());
    let digest = h.finalize_bytes();
    let hex = rightkit_crypto::hex_encode(&digest);
    hex.chars().take(16).collect()
}

/// Path to the managed profile directory for one provider+label.
/// Returned path is the directory the vendor CLI will see as its
/// `HOME` (or vendor-specific equivalent) on launch; the directory
/// itself may not exist yet — callers should call
/// [`ensure_managed_home`] before spawning the subprocess.
pub fn managed_home(provider: &CliProviderId, label: &str) -> PathBuf {
    managed_profile_root().join(provider.as_str()).join(label)
}

/// Path to the per-profile metadata file (managed record, not the
/// vendor's own credential store).
pub fn profile_record_path(provider: &CliProviderId, label: &str) -> PathBuf {
    managed_home(provider, label).join("profile.json")
}

/// Load the persisted [`CliAccountProfile`] for one provider+label.
/// Returns `Ok(None)` when the profile does not exist (this is the
/// expected first-login state), and an `Err(UnknownProfile)` for
/// malformed records — the caller decides whether to repair or
/// recreate in that case.
pub fn load_profile(
    provider: &CliProviderId,
    label: &str,
) -> Result<Option<CliAccountProfile>, ProfileStoreError> {
    let path = profile_record_path(provider, label);
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path)?;
    let record: CliAccountProfile = serde_json::from_str(&raw).map_err(|err| {
        ProfileStoreError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("profile.json at {} is malformed: {err}", path.display()),
        ))
    })?;
    Ok(Some(record))
}

/// Persist a [`CliAccountProfile`] record to disk as `profile.json`
/// inside the managed home. The directory must already exist — callers
/// obtain it via [`ensure_managed_home`] or [`profile_for`]. Records
/// are written atomically & owner-only through `rightkit-fs`.
pub fn save_profile(record: &CliAccountProfile) -> Result<(), ProfileStoreError> {
    let path = profile_record_path(&record.provider_id, &record.label);
    if let Some(parent) = path.parent() {
        ensure_managed_home(parent)?;
    }
    let serialized = serde_json::to_string_pretty(record).map_err(|err| {
        ProfileStoreError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("failed to serialize profile record: {err}"),
        ))
    })?;
    rightkit_fs::write_private_file(&path, serialized.as_bytes()).map_err(std::io::Error::other)?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn with_isolated_profile_home<T>(f: impl FnOnce() -> T) -> T {
    use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
    use std::sync::{Mutex, OnceLock};

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let previous = std::env::var_os("RIGHTKIT_SUITE_ROOT");
    let isolated = tempfile::tempdir().expect("isolated profile home");
    rightkit_app_settings::agent_config::test_isolation::set_process_suite_root(isolated.path());
    let result = catch_unwind(AssertUnwindSafe(f));
    match previous {
        Some(value) => std::env::set_var("RIGHTKIT_SUITE_ROOT", value),
        None => std::env::remove_var("RIGHTKIT_SUITE_ROOT"),
    }
    match result {
        Ok(value) => value,
        Err(payload) => resume_unwind(payload),
    }
}

/// Look up the profile for `<provider>/<label>`. If no record exists
/// yet, create the managed home, build a fresh profile record, persist
/// it, and return it. The created record is **not yet signed in** —
/// `signed_in` is `false` and `auth_mode` is `Unknown` until the
/// vendor CLI completes its own native flow.
pub fn profile_for(
    provider: &CliProviderId,
    label: &str,
) -> Result<CliAccountProfile, ProfileStoreError> {
    let sanitized = sanitize_label(label)?;
    if let Some(existing) = load_profile(provider, sanitized)? {
        return Ok(existing);
    }
    let home = ensure_managed_home(&managed_home(provider, sanitized))?;
    let record = profile_record(provider, sanitized, home);
    save_profile(&record)?;
    Ok(record)
}

/// Enumerate every profile currently on disk for one provider. The
/// result is sorted by label so callers can render a stable list.
/// Profiles whose `profile.json` is malformed are silently skipped
/// (they will surface as errors the next time the caller tries to
/// load them by label).
pub fn list_profiles(provider: &CliProviderId) -> Vec<CliAccountProfile> {
    let root = managed_profile_root().join(provider.as_str());
    let Ok(entries) = fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out: Vec<CliAccountProfile> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            let label = entry.file_name().to_string_lossy().into_owned();
            // `default` is the canonical first-profile label; arbitrary
            // user labels are also accepted. Skip hidden directories
            // (rightkit-fs temp names are dot-prefixed).
            if label.starts_with('.') {
                return None;
            }
            match load_profile(provider, &label) {
                Ok(Some(record)) => Some(record),
                _ => None,
            }
        })
        .collect();
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

/// Remove the managed profile directory for `<provider>/<label>`
/// wholesale — vendor credential files and the host app's own record go
/// together so the next launch starts from a clean slate. Returns
/// `Err(UnknownProfile)` if no directory exists.
pub fn delete_profile(provider: &CliProviderId, label: &str) -> Result<(), ProfileStoreError> {
    let sanitized = sanitize_label(label)?;
    let home = managed_home(provider, sanitized);
    if !home.exists() {
        return Err(ProfileStoreError::UnknownProfile {
            provider: provider.clone(),
            label: sanitized.to_string(),
        });
    }
    fs::remove_dir_all(&home)?;
    Ok(())
}

/// Snapshot every provider whose managed profile root exists on disk.
/// Used by the readiness gate so the registry can list profiles
/// without knowing the provider ids in advance.
pub fn known_providers() -> Vec<CliProviderId> {
    let root = managed_profile_root();
    let Ok(entries) = fs::read_dir(&root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            let raw = entry.file_name().to_string_lossy().into_owned();
            // Skip hidden directories (including rightkit-fs temp names).
            if raw.starts_with('.') {
                return None;
            }
            Some(CliProviderId::new(raw))
        })
        .collect()
}

/// Build a fresh fingerprint for non-secret account metadata. Returns
/// the 16-hex-char prefix used everywhere in this module; called by
/// adapters that observe a stable vendor identifier and want to
/// record it without ever touching the underlying credential.
pub fn fingerprint_of(account_label: &str) -> String {
    fingerprint_from_label(account_label)
}

/// Convenience: deny-list set used by the credential-leak assertion
/// in [`crate::driver_host::cli_runtime::login_routes`]. Re-exported here so the
/// profile store can document what it considers sensitive without
/// reaching into the login layer.
pub fn credential_deny_list() -> &'static BTreeSet<&'static str> {
    use std::sync::OnceLock;
    static DENY: OnceLock<BTreeSet<&'static str>> = OnceLock::new();
    DENY.get_or_init(|| {
        BTreeSet::from([
            // Vendor prefixes that always indicate a bearer credential.
            "sk-",
            "sk_",
            "ghp_",
            "gho_",
            "ghs_",
            "ghu_",
            "ghr_",
            "AKIA",
            "ASIA",
            "xai-",
            "xai_",
            // Common BYOK env vars whose value is the literal key.
            "OPENAI_API_KEY=",
            "ANTHROPIC_API_KEY=",
            "AWS_ACCESS_KEY_ID=",
            "AWS_SECRET_ACCESS_KEY=",
            "GOOGLE_API_KEY=",
            "XAI_API_KEY=",
            "OPENROUTER_API_KEY=",
            "MINIMAX_API_KEY=",
            "TINYFISH_API_KEY=",
        ])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profile_home_is_under_managed_root() {
        let provider = CliProviderId::new("claude");
        let home = default_profile_home(&provider);
        let s = home.to_string_lossy();
        assert!(
            s.contains("cli-profiles"),
            "managed home must live under cli-profiles, got {s}"
        );
        assert!(
            s.contains("claude"),
            "managed home must include provider id, got {s}"
        );
    }

    #[test]
    fn default_profile_id_includes_provider_and_label() {
        let provider = CliProviderId::new("grok");
        let id = default_profile_id(&provider);
        assert_eq!(id.as_str(), "grok/default");
    }

    #[test]
    fn fingerprint_is_deterministic_and_bounded() {
        let a = fingerprint_from_label("adrian@example.com");
        let b = fingerprint_from_label("adrian@example.com");
        let c = fingerprint_from_label("someone-else@example.com");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 16, "fingerprint is a 16-hex-char prefix");
    }

    #[test]
    fn sanitize_label_rejects_path_traversal_and_uppercase() {
        assert!(sanitize_label("default").is_ok());
        assert!(sanitize_label("work-1").is_ok());
        assert!(sanitize_label("work_account").is_ok());
        assert!(sanitize_label("").is_err(), "empty is not a label");
        assert!(
            sanitize_label("../escape").is_err(),
            "path traversal must be rejected"
        );
        assert!(
            sanitize_label("Work-1").is_err(),
            "uppercase is rejected so labels are predictable"
        );
        assert!(
            sanitize_label("with space").is_err(),
            "whitespace is rejected so shell escaping is never needed"
        );
        assert!(
            sanitize_label(&"a".repeat(65)).is_err(),
            "labels are bounded to 64 chars"
        );
    }

    #[test]
    fn profile_layouts_cover_claude_and_codex() {
        let names: Vec<&str> = PROFILE_LAYOUTS.iter().map(|(id, _)| *id).collect();
        assert!(names.contains(&"claude"), "claude layout missing");
        assert!(names.contains(&"codex"), "codex layout missing");
        let provider = CliProviderId::new("claude");
        let home = home_layout(&provider).expect("claude has a layout");
        let home = home("default");
        let s = home.to_string_lossy();
        assert!(
            s.contains(".claude"),
            "claude home must live under ~/.claude, got {s}"
        );
    }

    #[test]
    fn profile_round_trips_through_disk() {
        with_isolated_profile_home(|| {
            let provider = CliProviderId::new("claude");
            let label = "roundtrip";
            let record = profile_for(&provider, label).expect("first load creates profile");
            assert_eq!(record.provider_id.as_str(), "claude");
            assert_eq!(record.label, label);
            assert!(
                !record.signed_in,
                "fresh profile is unsigned-in until the vendor CLI completes"
            );
            // Re-load returns the same record (idempotent).
            let again = profile_for(&provider, label).expect("second load is idempotent");
            assert_eq!(again.id.as_str(), record.id.as_str());
            // Listing surfaces it.
            let listed = list_profiles(&provider);
            assert!(
                listed.iter().any(|row| row.label == label),
                "list_profiles must include the new row; got {:?}",
                listed.iter().map(|row| &row.label).collect::<Vec<_>>()
            );
            delete_profile(&provider, label).expect("delete_profile removes the tree");
            assert!(load_profile(&provider, label).unwrap().is_none());
        });
    }

    #[test]
    fn delete_profile_rejects_unknown_label() {
        let provider = CliProviderId::new("claude");
        let err = delete_profile(&provider, "ghost").unwrap_err();
        assert!(matches!(err, ProfileStoreError::UnknownProfile { .. }));
    }

    #[test]
    fn credential_deny_list_catches_synthetic_secrets() {
        let deny = credential_deny_list();
        for needle in [
            concat!("sk", "-abcdefghijklmnop"),
            concat!("gh", "p_abcdefghijklmnop"),
            concat!("AK", "IAABCDEFGHIJKLMNOP"),
            "xai-secret-value",
        ] {
            assert!(
                deny.iter().any(|prefix| needle.starts_with(prefix)),
                "deny-list must catch synthetic secret {needle:?}"
            );
        }
        assert!(
            deny.iter()
                .any(|prefix| prefix.starts_with("OPENAI_API_KEY=")),
            "deny-list must include env-var-prefixed secrets"
        );
    }

    #[test]
    fn profile_layout_returns_none_for_unknown_provider() {
        let provider = CliProviderId::new("mystery-cli");
        assert!(home_layout(&provider).is_none());
    }
}
