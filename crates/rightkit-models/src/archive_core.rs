//! Internal archive layer shared by the root stack ([`crate::archive`]) and the
//! suite store ([`crate::suite::archive`]). Not public API.
//!
//! One copy of the tar opener, the hardened `--strip-components=1` member
//! check, the Apple-metadata filter, and the wrapped unpack loop. Both callers
//! keep their own error type and their own install policy (single-file
//! activation in the root stack, staged directory swap in the suite store).

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// How the archive's outer compression is determined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Compression {
    /// Always gzip. A plain tar fails to read.
    Gzip,
    /// gzip when the magic bytes say so, otherwise a plain tar.
    Sniff,
}

/// Open `archive` as a tar stream, decompressing per `compression`.
pub(crate) fn open_tar(
    archive: &Path,
    compression: Compression,
) -> Result<tar::Archive<Box<dyn Read>>, String> {
    let mut file =
        fs::File::open(archive).map_err(|err| format!("open {}: {err}", archive.display()))?;
    let gzip = match compression {
        Compression::Gzip => true,
        Compression::Sniff => {
            let mut magic = [0u8; 2];
            let n = file
                .read(&mut magic)
                .map_err(|err| format!("read {}: {err}", archive.display()))?;
            n == 2 && magic == [0x1f, 0x8b]
        }
    };
    let file =
        fs::File::open(archive).map_err(|err| format!("open {}: {err}", archive.display()))?;
    let reader: Box<dyn Read> = if gzip {
        Box::new(flate2::read::GzDecoder::new(file))
    } else {
        Box::new(file)
    };
    let mut tar = tar::Archive::new(reader);
    tar.set_preserve_ownerships(false);
    tar.set_preserve_permissions(false);
    tar.set_unpack_xattrs(false);
    Ok(tar)
}

/// Unpack `archive` into `dest_dir` with its single wrapper directory stripped.
/// Every member must sit under the same wrapper, no member may escape
/// `dest_dir`, Apple metadata is skipped, and only regular files and
/// directories are accepted. Creates `dest_dir` if needed.
///
/// When `expected_wrapper` is `Some`, the wrapper must be exactly that name;
/// any other top-level name (including a second top-level entry) fails.
pub(crate) fn unpack_wrapped(
    archive: &Path,
    compression: Compression,
    dest_dir: &Path,
    expected_wrapper: Option<&str>,
) -> Result<(), String> {
    fs::create_dir_all(dest_dir).map_err(|err| format!("create {}: {err}", dest_dir.display()))?;
    let mut tar = open_tar(archive, compression)?;

    let mut saw_member = false;
    let mut wrapper: Option<OsString> = expected_wrapper.map(OsString::from);
    let entries = tar
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
        if let (Some(expected), Some(Component::Normal(found))) =
            (expected_wrapper, member.components().next())
        {
            if found != OsStr::new(expected) {
                return Err(format!(
                    "archive top-level entry `{}` does not match expected wrapper `{expected}`",
                    found.to_string_lossy()
                ));
            }
        }
        let Some(rel) = checked_member_path(&member, &mut wrapper)? else {
            // The wrapper dir entry itself (`<wrapper>/`): nothing to unpack.
            continue;
        };
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            fs::create_dir_all(dest_dir.join(&rel))
                .map_err(|err| format!("create extracted dir: {err}"))?;
        } else if kind.is_file() {
            let out = dest_dir.join(&rel);
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
pub(crate) fn checked_member_path(
    path: &Path,
    wrapper: &mut Option<OsString>,
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

/// `__MACOSX/` and `._*` AppleDouble entries carry no model data.
pub(crate) fn is_ignorable_apple_metadata(path: &Path) -> bool {
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
