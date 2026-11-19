//! Model archive install. The atomic install, the single-top-level wrapper pin, the
//! AppleDouble filter, the symlink refusal and the orphaned-aside recovery are
//! `rightkit_models::suite::archive`; this module keeps the app's call names and bounds every
//! extraction: the host passes [`ExtractLimits`] (entries, bytes per file, bytes in all, see
//! [`extract_limits`]), they are validated here, and the extraction stops at the first member or
//! byte past them, before the previous install is touched.

use std::path::Path;

use rightkit_models::suite::archive;
pub use rightkit_models::suite::archive::ExtractLimits;

/// Most members an archive installed through this module may hold ([`extract_limits`]).
pub const MAX_ARCHIVE_ENTRIES: usize = 20_000;

/// Largest `max_installed_bytes` [`extract_limits`] accepts: 32 GiB.
pub const MAX_INSTALLED_BYTES_CEILING: u64 = 32 << 30;

/// True for one plain path component: 1 to 128 bytes of ASCII letters, digits, `.`, `-` and `_`,
/// not starting with `.`, with no `..`. Every model, directory and archive name a host passes in
/// is checked against this before it reaches a path.
pub fn is_plain_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.starts_with('.')
        && !value.contains("..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

/// Limits for an archive that unpacks to at most `max_installed_bytes` in all and in any one file
/// (`1..=`[`MAX_INSTALLED_BYTES_CEILING`]; anything else is refused by the install) and holds at
/// most [`MAX_ARCHIVE_ENTRIES`] members.
pub fn extract_limits(max_installed_bytes: u64) -> ExtractLimits {
    ExtractLimits {
        max_entries: MAX_ARCHIVE_ENTRIES,
        max_entry_bytes: max_installed_bytes,
        max_total_bytes: max_installed_bytes,
    }
}

/// The limits must be finite, non-zero and within this module's ceilings: no host value can
/// ask for an unbounded extraction.
fn validate_limits(limits: &ExtractLimits) -> Result<(), String> {
    let ok = (1..=MAX_ARCHIVE_ENTRIES).contains(&limits.max_entries)
        && (1..=MAX_INSTALLED_BYTES_CEILING).contains(&limits.max_total_bytes)
        && (1..=limits.max_total_bytes).contains(&limits.max_entry_bytes);
    if ok {
        Ok(())
    } else {
        Err("archive extraction limits are out of range".to_string())
    }
}

pub fn install_single_top_level_tar_gz(
    archive: &Path,
    dest_root: &Path,
    expected_top_level: &str,
    limits: &ExtractLimits,
) -> Result<(), String> {
    install_single_top_level_tar_gz_as(
        archive,
        dest_root,
        expected_top_level,
        expected_top_level,
        limits,
    )
}

/// Install an archive that retains a legacy top-level name into a versioned
/// destination directory, keeping the old and new model side by side until the
/// replacement is verified. Both names must be plain names ([`is_plain_name`]) and the
/// extraction stays within `limits`.
pub fn install_single_top_level_tar_gz_as(
    archive_path: &Path,
    dest_root: &Path,
    expected_top_level: &str,
    installed_name: &str,
    limits: &ExtractLimits,
) -> Result<(), String> {
    if !is_plain_name(expected_top_level) || !is_plain_name(installed_name) {
        return Err("archive install name is invalid".to_string());
    }
    validate_limits(limits)?;
    archive::install_tar_gz_expecting_limited(
        archive_path,
        dest_root,
        expected_top_level,
        installed_name,
        limits,
    )
}

/// Promote a previous install left at the deterministic aside path by a process death between
/// the install's two renames. Call before trusting `dest_root/<installed_name>`. A name that is
/// not a plain name ([`is_plain_name`]) is refused before any path is built.
pub fn recover_orphaned_previous_install(
    dest_root: &Path,
    installed_name: &str,
) -> Result<bool, String> {
    if !is_plain_name(installed_name) {
        return Err("archive install name is invalid".to_string());
    }
    archive::recover_orphaned_install(dest_root, installed_name)
}
