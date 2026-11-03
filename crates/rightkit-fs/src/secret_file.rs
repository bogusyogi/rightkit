//! An owner-only secret file whose path is named by an environment variable.
//!
//! [`read_secret_file`] is the reader for an operator-supplied secret (an app secret, a token)
//! that lives in its own file: the variable names the path, the file must pass
//! [`crate::private::verify_owner_only`], and its text must be one line of printable characters.
//! The bytes are read through a bounded reader and wiped on drop.

use std::io::Read;
use std::path::PathBuf;

use zeroize::Zeroizing;

use crate::error::FsError;
use crate::private::verify_owner_only;

/// Why [`read_secret_file`] refused the secret.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SecretFileError {
    /// The environment variable is unset or empty. The name is the variable's, never a value.
    #[error("environment variable {0} is not set or is empty")]
    NotConfigured(String),
    /// The named file is not usable as a secret file (not owner-only, a symlink, unreadable).
    #[error("secret file is not usable: {0}")]
    Access(FsError),
    /// The file holds no text once its final line break is removed.
    #[error("secret file is empty")]
    Empty,
    /// The file is larger than the caller's bound.
    #[error("secret file is larger than {max_bytes} bytes")]
    TooLarge {
        /// The bound the caller passed.
        max_bytes: usize,
    },
    /// The file is not valid UTF-8.
    #[error("secret file is not valid UTF-8")]
    NotUtf8,
    /// The secret spans more than one line.
    #[error("secret file has more than one line")]
    MultiLine,
    /// The secret holds a control character (other than the one final line break).
    #[error("secret file holds a control character")]
    ControlCharacter,
}

/// Reads the secret file whose path is the value of `env_name`.
///
/// Refused, with a typed error and no value in it: the variable unset or empty, the file not
/// owner-only or a symlink ([`SecretFileError::Access`]), empty after one final `\n` or `\r\n` is
/// removed, larger than `max_bytes`, not UTF-8, more than one line, or holding a control
/// character. The returned text is wiped when dropped.
pub fn read_secret_file(
    env_name: &str,
    max_bytes: usize,
) -> Result<Zeroizing<String>, SecretFileError> {
    let path = match std::env::var_os(env_name) {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => return Err(SecretFileError::NotConfigured(env_name.to_string())),
    };
    verify_owner_only(&path).map_err(SecretFileError::Access)?;
    let length = std::fs::metadata(&path)
        .map_err(|e| SecretFileError::Access(FsError::io(&path, e)))?
        .len();
    if length > max_bytes as u64 {
        return Err(SecretFileError::TooLarge { max_bytes });
    }
    let file =
        std::fs::File::open(&path).map_err(|e| SecretFileError::Access(FsError::io(&path, e)))?;
    // One byte past the bound, so a file that grew after the length check is still refused.
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| SecretFileError::Access(FsError::io(&path, e)))?;
    if bytes.len() > max_bytes {
        return Err(SecretFileError::TooLarge { max_bytes });
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| SecretFileError::NotUtf8)?;
    let text = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(text);
    if text.is_empty() {
        return Err(SecretFileError::Empty);
    }
    if text.contains(['\n', '\r']) {
        return Err(SecretFileError::MultiLine);
    }
    if text.chars().any(char::is_control) {
        return Err(SecretFileError::ControlCharacter);
    }
    Ok(Zeroizing::new(text.to_string()))
}
