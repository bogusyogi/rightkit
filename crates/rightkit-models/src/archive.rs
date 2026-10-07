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
    let mut file = fs::File::open(archive).map_err(|e| ex(format!("open {}: {e}", archive.display())))?;
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
    fs::File::open(archive).and_then(|mut f| f.read_exact(&mut magic)).is_ok() && &magic == b"PK\x03\x04"
}

/// Extract the regular-file `member` of `archive` to exactly `dest` (parent
/// dirs created, existing `dest` replaced). Returns bytes written.
pub fn extract_member(archive: &Path, member: &str, dest: &Path) -> Result<u64> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| ex(format!("mkdir {}: {e}", parent.display())))?;
    }
    let tmp = dest.with_extension("extract-tmp");
    let _ = fs::remove_file(&tmp);
    let written = if is_zip(archive) {
        let mut zip = zip::ZipArchive::new(fs::File::open(archive).map_err(ex)?).map_err(ex)?;
        let mut entry = zip
            .by_name(member)
            .map_err(|e| ex(format!("{}: member {member}: {e}", archive.display())))?;
        if !entry.is_file() {
            return Err(ex(format!("member {member} is not a regular file")));
        }
        let mut out = fs::File::create(&tmp).map_err(ex)?;
        std::io::copy(&mut entry, &mut out).map_err(ex)?
    } else {
        let mut t = open_tar(archive)?;
        let mut found = None;
        for entry in t.entries().map_err(ex)? {
            let mut entry = entry.map_err(ex)?;
            let path = entry.path().map_err(ex)?.into_owned();
            let same = path.to_string_lossy().trim_start_matches("./") == member;
            if same && entry.header().entry_type().is_file() {
                let mut out = fs::File::create(&tmp).map_err(ex)?;
                found = Some(std::io::copy(&mut entry, &mut out).map_err(ex)?);
                break;
            }
        }
        found.ok_or_else(|| ex(format!("{}: member {member} not found", archive.display())))?
    };
    if dest.exists() {
        fs::remove_file(dest).map_err(|e| ex(format!("replace {}: {e}", dest.display())))?;
    }
    fs::rename(&tmp, dest).map_err(|e| ex(format!("activate {}: {e}", dest.display())))?;
    Ok(written)
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
        let Some(rel) = checked_member_path(&member, &mut wrapper).map_err(ex)? else { continue };
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            fs::create_dir_all(dest_dir.join(rel)).map_err(ex)?;
        } else if kind.is_file() {
            let out = dest_dir.join(rel);
            if let Some(p) = out.parent() {
                fs::create_dir_all(p).map_err(ex)?;
            }
            entry.unpack(&out).map_err(|e| ex(format!("extract {}: {e}", member.display())))?;
        } else {
            return Err(ex(format!("unsupported archive member type at {}", member.display())));
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

