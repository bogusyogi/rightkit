//! Readers for a file another process replaces.
//!
//! A replace (`atomic_write`, `publish_file`, `StateDir::write`) never shows a reader a truncated
//! file or a mix of two versions. On Windows it can, for a moment, refuse a reader's *open*: the
//! open fails with `ERROR_ACCESS_DENIED` (5) or `ERROR_SHARING_VIOLATION` (32) while the other
//! process is moving a new file over the name. The refusal passes by itself, so a reader that must
//! not mistake it for "no file" or "not allowed" tries again.
//!
//! - [`open_replaced`]: `File::open` that retries those two refusals on Windows.
//! - [`read_replaced`] / [`read_to_string_replaced`]: the whole file, through [`open_replaced`].
//!
//! **Windows:** up to [`MAX_OPEN_TRIES`] opens, 1 ms growing by 1 ms per try to a 20 ms ceiling
//! between them, so the worst case waits a little over a second. A refusal that is a real
//! permission failure (error 5 that never passes) is returned after the last try, unchanged.
//! Every other error, `NotFound` included, is returned at once. **Other platforms:** one plain
//! open; no retry exists because `rename(2)` never refuses an opening reader.
//!
//! Only the open is retried. Bytes read from the returned handle are one version of the file: the
//! one that was named when the open succeeded.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::time::Duration;

/// The most opens a Windows reader makes: the first, and up to 63 retries.
pub const MAX_OPEN_TRIES: u32 = 64;

/// Longest pause between two opens.
const MAX_PAUSE_MS: u64 = 20;

/// `ERROR_ACCESS_DENIED`.
const ERROR_ACCESS_DENIED: i32 = 5;
/// `ERROR_SHARING_VIOLATION`.
const ERROR_SHARING_VIOLATION: i32 = 32;

/// Whether a failed open carries a Win32 code that a concurrent replace produces: access denied
/// or a sharing violation. Pure; it looks only at the code.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn is_replace_refusal(code: Option<i32>) -> bool {
    matches!(
        code,
        Some(ERROR_ACCESS_DENIED) | Some(ERROR_SHARING_VIOLATION)
    )
}

/// The pause after failed try number `try_index` (0 for the first): 1 ms, plus 1 ms per earlier
/// failure, never more than 20 ms.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn pause_after(try_index: u32) -> Duration {
    Duration::from_millis((u64::from(try_index) + 1).min(MAX_PAUSE_MS))
}

/// Run `open` up to `tries` times, calling `pause` with the wait before each retry, and retry
/// only a refusal that [`is_replace_refusal`] accepts. Any other result ends the loop.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn open_with_retry<T>(
    tries: u32,
    mut open: impl FnMut() -> io::Result<T>,
    mut pause: impl FnMut(Duration),
) -> io::Result<T> {
    let mut attempt = 0u32;
    loop {
        match open() {
            Err(error)
                if attempt + 1 < tries.max(1) && is_replace_refusal(error.raw_os_error()) =>
            {
                pause(pause_after(attempt));
                attempt += 1;
            }
            other => return other,
        }
    }
}

/// Open `path` for reading, retrying on Windows an open that a concurrent replace refused (see the
/// module docs). Elsewhere this is `File::open`.
pub fn open_replaced(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    {
        open_with_retry(MAX_OPEN_TRIES, || File::open(path), std::thread::sleep)
    }
    #[cfg(not(windows))]
    {
        File::open(path)
    }
}

/// Read the whole file at `path` through [`open_replaced`].
pub fn read_replaced(path: &Path) -> io::Result<Vec<u8>> {
    let mut file = open_replaced(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Read the whole file at `path` as UTF-8 through [`open_replaced`]. Invalid UTF-8 is
/// `InvalidData`, as for `std::fs::read_to_string`.
pub fn read_to_string_replaced(path: &Path) -> io::Result<String> {
    let mut file = open_replaced(path)?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok(text)
}
