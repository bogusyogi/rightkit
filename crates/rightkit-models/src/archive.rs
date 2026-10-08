//! Archive extraction: one named member out of tar / tar.gz / zip, or a whole
//! tar.gz with a single wrapper directory stripped. Hardened against path
//! escape (`..`, absolute, prefixes, alternate wrappers) and non-regular
//! members. Origin: HeardRight `archive_install` via ScrapeRight's model-store
//! (wrapper strip) plus GenRight's single-member extraction (tar + zip, no OS
//! `tar` subprocess).

use crate::error::{Error, ErrorCode, Result};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

fn ex(msg: impl std::fmt::Display) -> Error {
    Error::new(ErrorCode::Extract, msg.to_string())
}

fn open_tar(archive: &Path) -> Result<tar::Archive<Box<dyn Read>>> {
    let mut file =
        fs::File::open(archive).map_err(|e| ex(format!("open {}: {e}", archive.display())))?;
    let mut magic = [0u8; 2];
    let n = file.read(&mut magic).map_err(ex)?;
    let file = fs::File::open(archive).map_err(ex)?;
    let reader: Box<dyn Read> = if n == 2 && magic == [0x1f, 0x8b] {
        Box::new(flate2::read::GzDecoder::new(file))
    } else {
        Box::new(file)
    };
    let mut t = tar::Archive::new(reader);
    t.set_preserve_ownerships(false);
    t.set_preserve_permissions(false);
    t.set_unpack_xattrs(false);
    Ok(t)
}

fn is_zip(archive: &Path) -> bool {
    let mut magic = [0u8; 4];
    fs::File::open(archive)
        .and_then(|mut f| f.read_exact(&mut magic))
        .is_ok()
        && &magic == b"PK\x03\x04"
}

/// Extract the regular-file `member` of `archive` to exactly `dest` (parent
/// dirs created, existing `dest` replaced). Returns bytes written.
pub fn extract_member(archive: &Path, member: &str, dest: &Path) -> Result<u64> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| ex(format!("mkdir {}: {e}", parent.display())))?;
    }
    let tmp = dest.with_extension("extract-tmp");
    let _ = fs::remove_file(&tmp);
    let written = match write_member(archive, member, &tmp) {
        Ok(written) => written,
        Err(error) => {
            let _ = fs::remove_file(&tmp);
            return Err(error);
        }
    };
    if let Err(error) = activate(&tmp, dest) {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    Ok(written)
}

/// Copy the regular-file `member` of `archive` into `tmp`. `dest` is never touched here.
fn write_member(archive: &Path, member: &str, tmp: &Path) -> Result<u64> {
    if is_zip(archive) {
        let mut zip = zip::ZipArchive::new(fs::File::open(archive).map_err(ex)?).map_err(ex)?;
        let mut entry = zip
            .by_name(member)
            .map_err(|e| ex(format!("{}: member {member}: {e}", archive.display())))?;
        if !entry.is_file() {
            return Err(ex(format!("member {member} is not a regular file")));
        }
        let mut out = fs::File::create(tmp).map_err(ex)?;
        return std::io::copy(&mut entry, &mut out).map_err(ex);
    }
    let mut t = open_tar(archive)?;
    for entry in t.entries().map_err(ex)? {
        let mut entry = entry.map_err(ex)?;
        let path = entry.path().map_err(ex)?.into_owned();
        let same = path.to_string_lossy().trim_start_matches("./") == member;
        if same && entry.header().entry_type().is_file() {
            let mut out = fs::File::create(tmp).map_err(ex)?;
            return std::io::copy(&mut entry, &mut out).map_err(ex);
        }
    }
    Err(ex(format!(
        "{}: member {member} not found",
        archive.display()
    )))
}

/// Replace `dest` with the fully written `tmp` in a single rename, never by
/// deleting `dest` first. `std::fs::rename` replaces an existing destination on
/// Unix (rename(2)) and on Windows (MoveFileExW with MOVEFILE_REPLACE_EXISTING),
/// so `dest` is never absent: if the rename fails, the previous file stays.
fn activate(tmp: &Path, dest: &Path) -> Result<()> {
    fs::rename(tmp, dest).map_err(|e| ex(format!("activate {}: {e}", dest.display())))
}

/// Unpack a tar.gz whose members all live under one wrapper directory into
/// `dest_dir` with the wrapper stripped (`--strip-components=1`).
pub fn unpack_wrapped(archive: &Path, dest_dir: &Path) -> Result<()> {
    fs::create_dir_all(dest_dir).map_err(ex)?;
    let mut t = open_tar(archive)?;
    let mut saw = false;
    let mut wrapper: Option<std::ffi::OsString> = None;
    for entry in t.entries().map_err(ex)? {
        let mut entry = entry.map_err(ex)?;
        let member = entry.path().map_err(ex)?.into_owned();
        if is_ignorable_apple_metadata(&member) {
            continue;
        }
        let Some(rel) = checked_member_path(&member, &mut wrapper).map_err(ex)? else {
            continue;
        };
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            fs::create_dir_all(dest_dir.join(rel)).map_err(ex)?;
        } else if kind.is_file() {
            let out = dest_dir.join(rel);
            if let Some(p) = out.parent() {
                fs::create_dir_all(p).map_err(ex)?;
            }
            entry
                .unpack(&out)
                .map_err(|e| ex(format!("extract {}: {e}", member.display())))?;
        } else {
            return Err(ex(format!(
                "unsupported archive member type at {}",
                member.display()
            )));
        }
        saw = true;
    }
    if !saw {
        return Err(ex("archive was empty"));
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
) -> std::result::Result<Option<PathBuf>, String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tar_with(dir: &Path, member: &str, data: &[u8]) -> PathBuf {
        let path = dir.join("model.tar");
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        builder.append_data(&mut header, member, data).unwrap();
        fs::write(&path, builder.into_inner().unwrap()).unwrap();
        path
    }

    #[test]
    fn extract_replaces_existing_destination_without_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        let archive = tar_with(dir.path(), "weights.bin", b"new-weights");
        let dest = dir.path().join("weights.bin");
        fs::write(&dest, b"old-weights").unwrap();

        let written = extract_member(&archive, "weights.bin", &dest).unwrap();

        assert_eq!(written, 11);
        assert_eq!(fs::read(&dest).unwrap(), b"new-weights");
        assert!(!dir.path().join("weights.extract-tmp").exists());
    }

    #[test]
    fn missing_member_keeps_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let archive = tar_with(dir.path(), "weights.bin", b"new-weights");
        let dest = dir.path().join("weights.bin");
        fs::write(&dest, b"old-weights").unwrap();

        assert!(extract_member(&archive, "absent.bin", &dest).is_err());

        assert_eq!(fs::read(&dest).unwrap(), b"old-weights");
        assert!(!dir.path().join("weights.extract-tmp").exists());
    }

    #[test]
    fn failed_activation_never_removes_existing_destination() {
        // Injected failure between the write and the activation: the staged
        // file is missing, so the rename must fail. The old file must survive.
        // The previous implementation deleted `dest` first and lost it here.
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("weights.bin");
        fs::write(&dest, b"old-weights").unwrap();
        let missing_tmp = dir.path().join("weights.extract-tmp");

        assert!(activate(&missing_tmp, &dest).is_err());

        assert_eq!(fs::read(&dest).unwrap(), b"old-weights");
    }

    #[test]
    fn stale_temp_from_interrupted_run_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let archive = tar_with(dir.path(), "weights.bin", b"fresh");
        let dest = dir.path().join("weights.bin");
        fs::write(&dest, b"old-weights").unwrap();
        fs::write(dir.path().join("weights.extract-tmp"), b"partial-junk").unwrap();

        extract_member(&archive, "weights.bin", &dest).unwrap();

        assert_eq!(fs::read(&dest).unwrap(), b"fresh");
        assert!(!dir.path().join("weights.extract-tmp").exists());
    }
}
