//! Bounded retry for the Windows file refusals that clear by themselves.
//!
//! An indexer, a virus scanner or a backup agent can hold a file this process wrote a moment ago
//! open without `FILE_SHARE_DELETE`. Windows then refuses a delete of that file, a rename over it,
//! or an open of it with `ERROR_ACCESS_DENIED` (5), `ERROR_SHARING_VIOLATION` (32) or
//! `ERROR_LOCK_VIOLATION` (33), although nothing about the caller's rights is wrong and the hold
//! ends by itself within moments. This module is the one place that knows which errors those are
//! and how long to wait for them, so a plain delete, rename or open that is the last step of a
//! longer, already-verified procedure can ride out a passing hold instead of failing the whole
//! procedure or leaving recovery material behind.
//!
//! What it guarantees:
//!
//! * **Windows only.** The three codes are Windows codes. On any other platform
//!   [`is_transient_refusal`] is always `false`, so every operation runs exactly once.
//! * **Bounded.** An operation is attempted once and then retried at most
//!   [`TRANSIENT_RETRY`]`.attempts` times (20), pausing `delay` (100 ms) before each retry, so a
//!   persistent refusal costs at most two seconds. [`retry_transient_with`] takes a different
//!   bound from the caller.
//! * **The original error comes back.** After the bound the last error is returned unchanged
//!   (same kind, same OS code), so a true permission failure still fails, only later. Nothing is
//!   wrapped, summarised or turned into success.
//! * **Only the operation is repeated.** The helper repeats the closure it is given and nothing
//!   else; it changes no ACL, attribute or sharing mode. A caller that deletes or renames a file
//!   it identified earlier proves that identity again inside the closure, so a name that changed
//!   hands during the pause is never touched.
//!
//! Error 5 is also what a real ACL denial returns. The publish step of an atomic write therefore
//! retries it only when the caller opts in with [`crate::atomic::AccessDeniedRetry`]; this module
//! is for the calls that are not an atomic write (a plain `remove_file`, `remove_dir`, `rename` or
//! `open`), where retrying error 5 for a bounded time is the same trade.

use crate::atomic::RetryPolicy;
use crate::error::{FsError, Result};
use std::io;
use std::time::Duration;

/// The bound [`retry_transient`] applies: at most 20 retries after the first attempt, 100 ms
/// before each.
pub const TRANSIENT_RETRY: RetryPolicy = RetryPolicy {
    attempts: 20,
    delay: Duration::from_millis(100),
};

/// True when `error` is a Windows refusal that usually clears by itself: `ERROR_ACCESS_DENIED`
/// (5), `ERROR_SHARING_VIOLATION` (32) or `ERROR_LOCK_VIOLATION` (33). Always `false` off Windows.
pub fn is_transient_refusal(error: &io::Error) -> bool {
    transient_code(cfg!(windows), error.raw_os_error())
}

/// The classifier behind [`is_transient_refusal`], with the platform explicit so it is testable
/// everywhere.
fn transient_code(windows: bool, raw: Option<i32>) -> bool {
    windows && matches!(raw, Some(5 | 32 | 33))
}

/// Run `operation`; while it fails with a [transient refusal](is_transient_refusal), pause and run
/// it again, at most [`TRANSIENT_RETRY`]`.attempts` more times. The last result is returned as it
/// is.
pub fn retry_transient<T>(operation: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    retry_transient_with(TRANSIENT_RETRY, operation)
}

/// [`retry_transient`] with the caller's bound: `policy.attempts` retries after the first
/// attempt, `policy.delay` before each. `attempts: 0` runs the operation once.
pub fn retry_transient_with<T>(
    policy: RetryPolicy,
    operation: impl FnMut() -> io::Result<T>,
) -> io::Result<T> {
    retry_by(policy, is_transient_refusal, operation)
}

/// [`retry_transient`] for an operation that reports [`FsError`] (for example
/// [`crate::FileKey::of_path`]). An error is transient when it is [`FsError::Io`] whose source is.
/// Any other error, including a path that does not exist, is returned at once.
pub fn retry_transient_fs<T>(operation: impl FnMut() -> Result<T>) -> Result<T> {
    retry_by(
        TRANSIENT_RETRY,
        |error: &FsError| matches!(error, FsError::Io { source, .. } if is_transient_refusal(source)),
        operation,
    )
}

/// The loop behind the public entry points, with the classifier explicit so tests can drive it
/// with errors that are transient on every platform.
fn retry_by<T, E>(
    policy: RetryPolicy,
    transient: impl Fn(&E) -> bool,
    mut operation: impl FnMut() -> std::result::Result<T, E>,
) -> std::result::Result<T, E> {
    let mut retries_left = policy.attempts;
    loop {
        match operation() {
            Err(error) if retries_left > 0 && transient(&error) => {
                retries_left -= 1;
                std::thread::sleep(policy.delay);
            }
            other => return other,
        }
    }
}
