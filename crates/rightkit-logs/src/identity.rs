//! Random identifiers. Install ids are random (never derived from hardware,
//! user name or account) and live in a file the user can delete to reset.

use std::io;
use std::path::Path;

/// RFC 4122 version-4 UUID from the OS random source.
pub fn random_uuid() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        // OS RNG unavailable: fall back to time+pid entropy. Identifier only,
        // never a secret.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        bytes.copy_from_slice(&(nanos ^ ((std::process::id() as u128) << 64)).to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Load the persisted pseudonymous install id, creating it on first use.
///
/// A new id is generated and written only when the file is absent (`NotFound`) or was read
/// and holds no valid id. Any other read error (a permission error, a sharing violation from a
/// process replacing the file, an I/O error) is returned and nothing is written: replacing the
/// id because the file could not be read would silently change the install's identity.
pub fn install_id(path: &Path) -> io::Result<String> {
    match rightkit_fs::read_replaced(path) {
        Ok(bytes) => {
            if let Some(existing) = valid_id(&bytes) {
                return Ok(existing);
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let id = random_uuid();
    crate::atomic::write_atomic(path, id.as_bytes())?;
    Ok(id)
}

fn valid_id(bytes: &[u8]) -> Option<String> {
    let existing = std::str::from_utf8(bytes).ok()?.trim();
    (existing.len() == 36 && existing.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
        .then(|| existing.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn absent_file_creates_and_persists_an_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("install-id");
        let id = install_id(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), id);
        assert_eq!(install_id(&path).unwrap(), id);
    }

    #[test]
    fn invalid_or_non_utf8_content_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("install-id");
        fs::write(&path, b"not an id").unwrap();
        let id = install_id(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), id);
        fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        let again = install_id(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), again);
    }

    /// A directory at the path makes the read fail with an error that is not `NotFound`
    /// (`IsADirectory` on Unix, `PermissionDenied` on Windows) on every platform.
    #[test]
    fn unreadable_path_is_an_error_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("install-id");
        fs::create_dir(&path).unwrap();
        let error = install_id(&path).expect_err("a read error must not mint a new id");
        assert_ne!(error.kind(), io::ErrorKind::NotFound);
        assert!(path.is_dir(), "the path must be left exactly as it was");
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "no temp or replacement file: {names:?}");
    }
}
