//! Model archive install. The atomic install, the single-top-level wrapper pin, the
//! AppleDouble filter, the symlink refusal and the orphaned-aside recovery are
//! `rightkit_models::suite::archive`; this module keeps the app's call names.

use std::path::Path;

use rightkit_models::suite::archive;

pub fn install_single_top_level_tar_gz(
    archive: &Path,
    dest_root: &Path,
    expected_top_level: &str,
) -> Result<(), String> {
    install_single_top_level_tar_gz_as(archive, dest_root, expected_top_level, expected_top_level)
}

/// Install an archive that retains a legacy top-level name into a versioned
/// destination directory, keeping the old and new model side by side until the
/// replacement is verified.
pub fn install_single_top_level_tar_gz_as(
    archive_path: &Path,
    dest_root: &Path,
    expected_top_level: &str,
    installed_name: &str,
) -> Result<(), String> {
    archive::install_tar_gz_expecting(archive_path, dest_root, expected_top_level, installed_name)
}

/// Promote a previous install left at the deterministic aside path by a process death between
/// the install's two renames. Call before trusting `dest_root/<installed_name>`.
pub fn recover_orphaned_previous_install(
    dest_root: &Path,
    installed_name: &str,
) -> Result<bool, String> {
    archive::recover_orphaned_install(dest_root, installed_name)
}
