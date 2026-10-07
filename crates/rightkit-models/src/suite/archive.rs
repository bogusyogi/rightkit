//! Atomic tar.gz model install — ported from HeardRight's
//! `heardright-model-store::archive_install` and adapted to the store layout:
//! archives carry a single wrapper directory (matching `tar --strip-components=1`
//! semantics in package.ps1), extracted into a staging sibling, then swapped
//! into `<entry-dir>` with the previous install moved aside first and restored
//! on failure. A deterministic aside name lets the next startup recover from a
//! kill in the narrow rename window.

use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Extract `archive` (a `.tar.gz` whose members live under one wrapper dir)
/// and install it as `dest_root/<installed_name>`. Any previous install is
/// preserved until the replacement verifies and commits.
pub fn install_tar_gz(
    archive: &Path,
    dest_root: &Path,
    installed_name: &str,
) -> Result<(), String> {
    validate_dir_name(installed_name)?;
    fs::create_dir_all(dest_root)
        .map_err(|err| format!("create {}: {err}", dest_root.display()))?;

    let staging = dest_root.join(format!(
        ".{installed_name}.installing-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging).map_err(|err| format!("create {}: {err}", staging.display()))?;

    let result = unpack_checked(archive, &staging).and_then(|()| {
        let dest = dest_root.join(installed_name);
        // Never delete `dest` before the replacement is in place: a crash or a
        // failed rename between the two would leave no model at all. Move the
        // existing install aside, commit the new one, and only then drop the
        // aside copy; restore it if the commit fails.
        let aside = dest
            .exists()
            .then(|| previous_aside_path(dest_root, installed_name));
        if let Some(aside) = aside.as_ref() {
            let _ = fs::remove_dir_all(aside);
            fs::rename(&dest, aside)
                .map_err(|err| format!("stage previous {}: {err}", dest.display()))?;
        }
        match fs::rename(&staging, &dest) {
            Ok(()) => {
                if let Some(aside) = aside.as_ref() {
                    let _ = fs::remove_dir_all(aside);
                }
                Ok(())
            }
            Err(err) => {
                if let Some(aside) = aside.as_ref() {
                    let _ = fs::rename(aside, &dest);
                }
                Err(format!("install {}: {err}", dest.display()))
            }
        }
    });

    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

/// Deterministic aside path — a fixed name (no PID or timestamp), so
/// [`recover_orphaned_install`] can find it after a crash without guessing.
/// Only one install of a given `installed_name` is ever in flight at a time
/// (the entry lock guarantees it), so a fixed name cannot collide.
fn previous_aside_path(dest_root: &Path, installed_name: &str) -> PathBuf {
    dest_root.join(format!(".{installed_name}.previous-aside"))
}

/// Recover from a process death between `rename(dest -> aside)` and
/// `rename(staging -> dest)` in [`install_tar_gz`]: a hard kill in that window
/// leaves `dest` missing and the previously-working install sitting one
/// rename away at the deterministic aside path.
///
/// Call before trusting `dest` — the store's readiness check runs it so
/// recovery happens the next time anything asks "is this model installed?".
pub fn recover_orphaned_install(dest_root: &Path, installed_name: &str) -> Result<bool, String> {
    let aside = previous_aside_path(dest_root, installed_name);
    if !aside.exists() {
        return Ok(false);
    }
    let dest = dest_root.join(installed_name);
    if dest.exists() {
        let _ = fs::remove_dir_all(&aside);
        return Ok(false);
    }
    fs::rename(&aside, &dest).map_err(|err| {
        format!(
            "recover orphaned previous install at {}: {err}",
            aside.display()
        )
    })?;
    Ok(true)
}

/// Stale staging dirs from dead installs: `<name>.installing-*` siblings in
/// `dest_root`. Cleaned up at status time — they are never valid installs.
pub fn cleanup_staging_dirs(dest_root: &Path) {
    let Ok(entries) = fs::read_dir(dest_root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with('.')
            && (name.contains(".installing-") || name.ends_with(".previous-aside"))
        {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

fn unpack_checked(archive: &Path, staging: &Path) -> Result<(), String> {
    let file =
        fs::File::open(archive).map_err(|err| format!("open {}: {err}", archive.display()))?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    archive.set_preserve_ownerships(false);
    archive.set_preserve_permissions(false);
    archive.set_unpack_xattrs(false);

    let mut saw_member = false;
    let mut wrapper: Option<std::ffi::OsString> = None;
    let entries = archive
        .entries()
        .map_err(|err| format!("read archive: {err}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|err| format!("read archive entry: {err}"))?;
        let member = entry
            .path()
            .map_err(|err| format!("read archive path: {err}"))?
            .into_owned();
        if is_ignorable_apple_metadata(&member) {
            continue;
        }
        let Some(rel) = checked_member_path(&member, &mut wrapper)? else {
            // The wrapper dir entry itself (`<wrapper>/`): nothing to unpack.
            continue;
        };
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            fs::create_dir_all(staging.join(rel))
                .map_err(|err| format!("create extracted dir: {err}"))?;
        } else if kind.is_file() {
            let out = staging.join(rel);
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent).map_err(|err| format!("create extracted dir: {err}"))?;
            }
            entry
                .unpack(&out)
                .map_err(|err| format!("extract {}: {err}", member.display()))?;
        } else {
            return Err(format!(
                "unsupported archive member type at {}",
                member.display()
            ));
        }
        saw_member = true;
    }
    if !saw_member {
        return Err("archive was empty".to_string());
    }
    Ok(())
}

/// `--strip-components=1` with hardening: every member must be `<wrapper>/<path…>`
/// where `<wrapper>` is one consistent normal dir name and `<path…>` contains
/// only normal components — `..`, roots, prefixes, and alternate wrappers all
/// reject. The wrapper entry itself maps to `None` (skipped).
fn checked_member_path(
    path: &Path,
    wrapper: &mut Option<std::ffi::OsString>,
) -> Result<Option<PathBuf>, String> {
    let mut components = path.components();
    let first = match components.next() {
        Some(Component::Normal(part)) => part,
        _ => {
            return Err(format!(
                "archive member escapes model dir: {}",
                path.display()
            ))
        }
    };
    match wrapper {
        Some(w) if w != first => {
            return Err(format!(
                "archive member escapes model dir: {}",
                path.display()
            ))
        }
        None => *wrapper = Some(first.to_os_string()),
        _ => {}
    }
    let mut out = PathBuf::new();
    for component in components {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            _ => {
                return Err(format!(
                    "archive member escapes model dir: {}",
                    path.display()
                ))
            }
        }
    }
    if out.as_os_str().is_empty() {
        return Ok(None);
    }
    Ok(Some(out))
}

fn is_ignorable_apple_metadata(path: &Path) -> bool {
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                let name = part.to_string_lossy();
                if name == "__MACOSX" || name.starts_with("._") {
                    return true;
                }
            }
            Component::CurDir => {}
            _ => return false,
        }
    }
    false
}

fn validate_dir_name(name: &str) -> Result<(), String> {
    let path = Path::new(name);
    let mut components = path.components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(part)), None) if part == OsStr::new(name) => Ok(()),
        _ => Err(format!("invalid store entry name {name:?}")),
    }
}
