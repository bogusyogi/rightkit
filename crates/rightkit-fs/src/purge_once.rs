//! Remove a known set of retired files before recording that the migration completed.

use std::path::Path;

use crate::names::is_unsafe_windows_component;
use crate::state::{EntryKind, StateDir};

/// Purge direct children of `root` once. An existing marker skips the migration.
/// Missing files are fine. If removal fails, leave the marker absent for a retry.
pub fn purge_files_once(
    root: &Path,
    marker_name: &str,
    marker_contents: &[u8],
    names: &[&str],
) -> Result<(), String> {
    if is_unsafe_windows_component(marker_name)
        || names.iter().any(|name| is_unsafe_windows_component(name))
    {
        return Err("purge names must be single portable path components".to_owned());
    }
    let dir = match StateDir::create_in_resolved_parent(root) {
        Err(error) if error.is_not_found() => StateDir::create(root),
        result => result,
    }
    .map_err(|error| format!("open purge directory: {error}"))?;
    if let Some(marker) = dir
        .stat(marker_name)
        .map_err(|error| format!("read purge marker: {error}"))?
    {
        return match marker.kind {
            EntryKind::File => Ok(()),
            _ => Err("purge marker is not a regular file".to_owned()),
        };
    }
    for name in names {
        dir.remove(name)
            .map_err(|error| format!("purge {name}: {error}"))?;
    }
    dir.write(marker_name, marker_contents)
        .map_err(|error| format!("write purge marker: {error}"))
}
