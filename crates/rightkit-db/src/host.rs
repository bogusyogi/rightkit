//! Per-host database paths for rollback-journal fallback, ported from CodeRight's
//! `private_fs::journal` (`effective_db_path` / `host_discriminator`).
//!
//! A rollback journal on a network share is only safe with one writer host per file, so the
//! fallback opens `<stem>.h-<host>.<ext>` beside the requested path instead of sharing it.

use std::path::{Path, PathBuf};

/// `name.db` -> `name.h-<host>.db` (extension-less names get the tag appended). Idempotent;
/// unchanged when no host name is available.
pub fn per_host_path(db_path: &Path) -> PathBuf {
    let Some(host) = host_discriminator() else {
        return db_path.to_path_buf();
    };
    tagged(db_path, &host)
}

pub(crate) fn tagged(db_path: &Path, host: &str) -> PathBuf {
    let Some(name) = db_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
    else {
        return db_path.to_path_buf();
    };
    let tag = format!(".h-{host}");
    if name.ends_with(&tag) || name.contains(&format!("{tag}.")) {
        return db_path.to_path_buf();
    }
    // rsplit keeps dotfile names like ".hidden" on the append arm.
    let new_name = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem}{tag}.{ext}"),
        _ => format!("{name}{tag}"),
    };
    db_path.with_file_name(new_name)
}

/// Lower-case alphanumerics and `-`, at most 24 characters; `None` when unavailable.
pub(crate) fn host_discriminator() -> Option<String> {
    let raw = hostname_raw()?;
    let mut s: String = raw
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    s.truncate(24);
    let s = s.trim_matches('-');
    (!s.is_empty()).then(|| s.to_string())
}

#[cfg(unix)]
fn hostname_raw() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: valid out-buffer of the given length; position() guards a missing NUL.
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
        return None;
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8(buf[..len].to_vec()).ok()
}

#[cfg(windows)]
fn hostname_raw() -> Option<String> {
    if let Ok(host) = std::env::var("COMPUTERNAME") {
        if !host.trim().is_empty() {
            return Some(host);
        }
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetComputerNameW(buffer: *mut u16, size: *mut u32) -> i32;
    }
    let mut buffer = [0u16; 256];
    let mut size = buffer.len() as u32;
    // SAFETY: valid buffer and size out-parameter.
    if unsafe { GetComputerNameW(buffer.as_mut_ptr(), &mut size) } == 0 || size == 0 {
        return None;
    }
    String::from_utf16(&buffer[..size as usize]).ok()
}

#[cfg(not(any(unix, windows)))]
fn hostname_raw() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_host_names_are_deterministic_siblings_and_idempotent() {
        let p = Path::new("/data/coderight.db");
        let a = tagged(p, "box-1");
        assert_eq!(a, Path::new("/data/coderight.h-box-1.db"));
        assert_eq!(tagged(&a, "box-1"), a);
        assert_eq!(
            tagged(Path::new("/data/store"), "box-1"),
            Path::new("/data/store.h-box-1")
        );
        assert_eq!(
            tagged(Path::new("/data/.hidden"), "h"),
            Path::new("/data/.hidden.h-h")
        );
        assert_eq!(per_host_path(p), per_host_path(p));
    }
}
