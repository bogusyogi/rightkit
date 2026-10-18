//! Install in place at ONE fixed path, with NO retained copy of the replaced build (D01).
//!
//! macOS ties privacy grants to the path an app runs from and to its code identity, so a dev or QA
//! binary, bundle or helper must never be launched from a per-build, per-run or per-generation
//! folder (the build broker's `ws/<hash>/g/<gen>/...` target tree changes on every generation).
//! These helpers copy a build to its fixed path by rename: the new copy is written to a staging
//! sibling (nothing runs from it), the old copy is renamed to a transient aside, and the staging
//! copy is renamed into place. A failure renames the aside back (roll-forward recovery); success
//! deletes the aside and any legacy `<name>.previous` left by older installers, so no `.previous`
//! application copy is ever retained.

use crate::util::{Error, Result};
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Suffix of the rollback copy older installers kept. Retained only so a successful install can
/// delete a legacy `<name>.previous`; nothing writes one any more (D01).
pub const PREVIOUS_SUFFIX: &str = ".previous";

/// `<path>.previous`, beside `path`.
pub fn previous_path(path: &Path) -> PathBuf {
    suffixed(path, PREVIOUS_SUFFIX)
}

/// `<path>.staging-<pid>`: where a copy is written before the rename. The pid keeps two concurrent
/// runs from writing one staging path; nothing is ever run from it.
pub fn staging_path(path: &Path) -> PathBuf {
    suffixed(path, &format!(".staging-{}", std::process::id()))
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name: OsString = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn remove_any(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
    }
}

/// Clean only the exact legacy sibling with the installed object's kind. Never follow a
/// symlink/reparse point or remove a differently shaped object at a reserved name.
fn cleanup_previous(dest: &Path) -> Result<()> {
    let previous = previous_path(dest);
    let metadata = match std::fs::symlink_metadata(&previous) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        result => {
            result.map_err(|e| Error(format!("cannot inspect {}: {e}", previous.display())))?
        }
    };
    let installed = std::fs::symlink_metadata(dest)?;
    #[cfg(windows)]
    let reparse = {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0 || installed.file_attributes() & 0x400 != 0
    };
    #[cfg(not(windows))]
    let reparse = false;
    if reparse
        || metadata.file_type().is_symlink()
        || installed.file_type().is_symlink()
        || !(metadata.is_file() && installed.is_file() || metadata.is_dir() && installed.is_dir())
    {
        return Err(Error(format!(
            "refusing unvalidated legacy backup {}",
            previous.display()
        )));
    }
    remove_any(&previous).map_err(|e| {
        Error(format!(
            "cannot remove legacy backup {}: {e}",
            previous.display()
        ))
    })
}

/// `<path>.aside-<pid>`: transient home of the replaced copy during one swap. Deleted on success,
/// renamed back on failure; nothing ever runs from it and it is never retained.
fn aside_path(path: &Path) -> PathBuf {
    suffixed(path, &format!(".aside-{}", std::process::id()))
}

/// Rename `staging` into `dest`, moving an existing `dest` to a transient aside first. Any failure
/// puts the aside back and removes `staging`. Success deletes the aside and any legacy
/// `<dest>.previous`, retaining no copy of the replaced build (D01).
pub fn swap_in_place(staging: &Path, dest: &Path) -> Result<()> {
    let aside = aside_path(dest);
    let moved_aside = std::fs::symlink_metadata(dest).is_ok();
    let swapped = (|| -> std::io::Result<()> {
        if moved_aside {
            remove_any(&aside)?;
            std::fs::rename(dest, &aside)?;
        }
        std::fs::rename(staging, dest)
    })();
    if let Err(e) = swapped {
        if moved_aside && std::fs::symlink_metadata(dest).is_err() {
            let _ = std::fs::rename(&aside, dest);
        }
        let _ = remove_any(staging);
        return Err(Error(format!(
            "cannot install {} in place: {e}",
            dest.display()
        )));
    }
    if moved_aside {
        remove_any(&aside).map_err(|e| {
            Error(format!(
                "installed {} in place but cannot remove the replaced copy at {}: {e}",
                dest.display(),
                aside.display()
            ))
        })?;
    }
    cleanup_previous(dest)
}

/// True when both are regular files with identical bytes.
pub fn same_bytes(a: &Path, b: &Path) -> bool {
    let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    if !ma.is_file() || !mb.is_file() || ma.len() != mb.len() {
        return false;
    }
    let (Ok(mut fa), Ok(mut fb)) = (std::fs::File::open(a), std::fs::File::open(b)) else {
        return false;
    };
    let (mut ba, mut bb) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    loop {
        let (Ok(na), Ok(nb)) = (fa.read(&mut ba), fb.read(&mut bb)) else {
            return false;
        };
        if na != nb || ba[..na] != bb[..nb] {
            return false;
        }
        if na == 0 {
            return true;
        }
    }
}

/// Copy file `from` to its fixed path `to` in place, retaining no copy of the replaced build
/// (D01). A copy whose bytes already match is left alone.
pub fn replace_file_in_place(from: &Path, to: &Path) -> Result<()> {
    if same_bytes(from, to) {
        return cleanup_previous(to);
    }
    let staging = staging_path(to);
    let _ = remove_any(&staging);
    std::fs::copy(from, &staging).map_err(|e| {
        let _ = remove_any(&staging);
        Error(format!(
            "cannot copy {} to {}: {e}",
            from.display(),
            staging.display()
        ))
    })?;
    swap_in_place(&staging, to)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rkqa-inplace-{tag}-{}", crate::util::new_id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn paths_are_beside_the_fixed_path() {
        assert_eq!(
            previous_path(Path::new("/a/b/app.exe")),
            Path::new("/a/b/app.exe.previous")
        );
        assert!(staging_path(Path::new("/a/b/app"))
            .to_string_lossy()
            .starts_with("/a/b/app.staging-"));
    }

    #[test]
    fn replace_keeps_no_previous_and_the_same_path() {
        let base = tmp("replace");
        let to = base.join("app");
        let v1 = base.join("v1");
        let v2 = base.join("v2");
        let v3 = base.join("v3");
        std::fs::write(&v1, b"one").unwrap();
        std::fs::write(&v2, b"two").unwrap();
        std::fs::write(&v3, b"three").unwrap();
        replace_file_in_place(&v1, &to).unwrap();
        assert!(
            !previous_path(&to).exists(),
            "first install has no previous"
        );
        // A legacy `.previous` from an older installer is removed by the next successful install.
        std::fs::write(previous_path(&to), b"legacy").unwrap();
        replace_file_in_place(&v2, &to).unwrap();
        replace_file_in_place(&v2, &to).unwrap(); // identical bytes: left alone
        assert_eq!(std::fs::read(&to).unwrap(), b"two");
        assert!(
            !previous_path(&to).exists(),
            "no retained .previous copy (D01)"
        );
        replace_file_in_place(&v3, &to).unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"three");
        assert!(!previous_path(&to).exists());
        let mut names: Vec<_> = std::fs::read_dir(&base)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["app", "v1", "v2", "v3"]);
    }

    #[test]
    fn swap_in_place_swaps_directories_and_restores_on_failure() {
        let base = tmp("dirs");
        let dest = base.join("App.app");
        std::fs::create_dir_all(dest.join("Contents")).unwrap();
        std::fs::write(dest.join("Contents/marker"), b"old").unwrap();
        let staging = base.join("App.app.staging-1");
        std::fs::create_dir_all(staging.join("Contents")).unwrap();
        std::fs::write(staging.join("Contents/marker"), b"new").unwrap();
        swap_in_place(&staging, &dest).unwrap();
        assert_eq!(std::fs::read(dest.join("Contents/marker")).unwrap(), b"new");
        assert!(
            !previous_path(&dest).exists(),
            "no retained .previous bundle (D01)"
        );
        // A missing staging dir fails and puts the installed copy back.
        let missing = base.join("App.app.staging-2");
        assert!(swap_in_place(&missing, &dest).is_err());
        assert_eq!(std::fs::read(dest.join("Contents/marker")).unwrap(), b"new");
    }

    #[test]
    fn unchanged_write_cleans_only_matching_legacy_sibling() {
        let base = tmp("unchanged");
        let from = base.join("source");
        let to = base.join("app");
        let unrelated = base.join("unrelated.previous");
        std::fs::write(&from, b"same").unwrap();
        std::fs::write(&to, b"same").unwrap();
        std::fs::write(previous_path(&to), b"old").unwrap();
        std::fs::write(&unrelated, b"keep").unwrap();
        replace_file_in_place(&from, &to).unwrap();
        assert!(!previous_path(&to).exists());
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"keep");
        std::fs::create_dir(previous_path(&to)).unwrap();
        assert!(replace_file_in_place(&from, &to).is_err());
        assert!(previous_path(&to).is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn unchanged_write_refuses_symlink_backup_without_touching_target() {
        let base = tmp("unchanged-link");
        let from = base.join("source");
        let to = base.join("app");
        let unrelated = base.join("unrelated");
        std::fs::write(&from, b"same").unwrap();
        std::fs::write(&to, b"same").unwrap();
        std::fs::write(&unrelated, b"keep").unwrap();
        std::os::unix::fs::symlink(&unrelated, previous_path(&to)).unwrap();
        assert!(replace_file_in_place(&from, &to).is_err());
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"keep");
    }
}
