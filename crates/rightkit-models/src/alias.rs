//! Legacy flat-directory alias links (see [`crate::store`] docs). Only ever
//! creates, replaces or removes a *link*; a real file or directory at the
//! alias path is reported and left alone.

use crate::store::{is_link, AliasStatus};
use std::fs;
use std::path::{Path, PathBuf};

/// Absolute form of `p` without resolving links (alias targets must not
/// depend on the reader's working directory).
pub(crate) fn absolute(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|c| c.join(p))
            .unwrap_or_else(|_| p.to_path_buf())
    }
}

/// Make `alias` a link to the directory `target`.
pub(crate) fn point(alias: &Path, target: &Path) -> Result<AliasStatus, String> {
    match fs::symlink_metadata(alias) {
        Ok(m) if !m.file_type().is_symlink() => return Ok(AliasStatus::SkippedRealPath),
        Ok(_) => {
            if fs::read_link(alias).is_ok_and(|t| t == target) {
                return Ok(AliasStatus::Unchanged);
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("inspect {}: {e}", alias.display())),
    }
    if let Some(parent) = alias.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    replace_link(alias, target)?;
    Ok(AliasStatus::Linked)
}

/// Remove `alias` if it is a link; a real path is left alone.
pub(crate) fn remove(alias: &Path) -> Result<AliasStatus, String> {
    match fs::symlink_metadata(alias) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AliasStatus::Removed),
        Err(e) => Err(format!("inspect {}: {e}", alias.display())),
        Ok(_) if !is_link(alias) => Ok(AliasStatus::SkippedRealPath),
        Ok(_) => {
            unlink(alias)?;
            Ok(AliasStatus::Removed)
        }
    }
}

fn unlink(alias: &Path) -> Result<(), String> {
    fs::remove_file(alias)
        .or_else(|_| fs::remove_dir(alias))
        .map_err(|e| format!("remove link {}: {e}", alias.display()))
}

/// Unix: build the new link beside the alias, then rename it over (atomic;
/// rename replaces the link itself, never its target).
#[cfg(unix)]
fn replace_link(alias: &Path, target: &Path) -> Result<(), String> {
    let name = alias
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = alias.with_file_name(format!(".{name}.alias-{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp)
        .map_err(|e| format!("link {} -> {}: {e}", tmp.display(), target.display()))?;
    fs::rename(&tmp, alias).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("replace {}: {e}", alias.display())
    })
}

/// Windows: a directory link cannot be renamed over atomically; remove, then
/// create a directory symlink, falling back to a junction (no privilege
/// needed) through `mklink /J` with no console window.
#[cfg(windows)]
fn replace_link(alias: &Path, target: &Path) -> Result<(), String> {
    if fs::symlink_metadata(alias).is_ok() {
        unlink(alias)?;
    }
    if std::os::windows::fs::symlink_dir(target, alias).is_ok() {
        return Ok(());
    }
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(alias)
        .arg(target)
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output()
        .map_err(|e| format!("mklink /J {}: {e}", alias.display()))?;
    if out.status.success() && is_link(alias) {
        Ok(())
    } else {
        Err(format!(
            "mklink /J {} failed: {}",
            alias.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

#[cfg(not(any(unix, windows)))]
fn replace_link(alias: &Path, _target: &Path) -> Result<(), String> {
    Err(format!("links unsupported here: {}", alias.display()))
}
