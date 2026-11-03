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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::path::PathBuf;

    fn held() -> io::Error {
        io::Error::new(io::ErrorKind::WouldBlock, "held")
    }

    fn is_held(error: &io::Error) -> bool {
        error.kind() == io::ErrorKind::WouldBlock
    }

    fn quick(attempts: u32) -> RetryPolicy {
        RetryPolicy::new(attempts, Duration::ZERO)
    }

    #[test]
    fn only_codes_five_thirty_two_and_thirty_three_on_windows_are_transient() {
        for code in [5, 32, 33] {
            assert!(transient_code(true, Some(code)), "{code}");
            assert!(!transient_code(false, Some(code)), "{code} off Windows");
        }
        for code in [0, 2, 3, 80, 145, 183] {
            assert!(!transient_code(true, Some(code)), "{code}");
        }
        assert!(!transient_code(true, None));
        assert!(!is_transient_refusal(&io::Error::from(
            io::ErrorKind::NotFound
        )));
        assert!(!is_transient_refusal(&io::Error::from(
            io::ErrorKind::PermissionDenied
        )));
        assert_eq!(
            is_transient_refusal(&io::Error::from_raw_os_error(5)),
            cfg!(windows),
            "error 5 is transient on Windows only"
        );
    }

    #[test]
    fn the_default_bound_is_twenty_retries_a_hundred_milliseconds_apart() {
        assert_eq!(TRANSIENT_RETRY.attempts, 20);
        assert_eq!(TRANSIENT_RETRY.delay, Duration::from_millis(100));
        assert!(
            TRANSIENT_RETRY.delay * TRANSIENT_RETRY.attempts <= Duration::from_secs(2),
            "a persistent refusal must cost at most two seconds"
        );
    }

    #[test]
    fn a_transient_failure_is_retried_until_it_clears() {
        let calls = Cell::new(0u32);
        let result = retry_by(quick(5), is_held, || {
            calls.set(calls.get() + 1);
            if calls.get() <= 3 {
                Err(held())
            } else {
                Ok(calls.get())
            }
        });
        assert_eq!(result.unwrap(), 4);
        assert_eq!(calls.get(), 4);
    }

    #[test]
    fn the_retry_is_bounded_and_returns_the_original_error() {
        let calls = Cell::new(0u32);
        let result: io::Result<()> = retry_by(quick(3), is_held, || {
            calls.set(calls.get() + 1);
            Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!("attempt {}", calls.get()),
            ))
        });
        let error = result.unwrap_err();
        assert!(is_held(&error), "{error:?}");
        assert_eq!(error.to_string(), "attempt 4", "the last error is returned");
        assert_eq!(calls.get(), 4, "one attempt plus three retries");
    }

    #[test]
    fn zero_attempts_runs_once() {
        let calls = Cell::new(0u32);
        let result: io::Result<()> = retry_by(quick(0), is_held, || {
            calls.set(calls.get() + 1);
            Err(held())
        });
        assert!(result.is_err());
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn other_errors_and_not_found_return_at_once() {
        for kind in [io::ErrorKind::NotFound, io::ErrorKind::PermissionDenied] {
            let calls = Cell::new(0u32);
            let result: io::Result<()> = retry_by(quick(5), is_held, || {
                calls.set(calls.get() + 1);
                Err(io::Error::from(kind))
            });
            assert_eq!(result.unwrap_err().kind(), kind);
            assert_eq!(calls.get(), 1, "{kind:?} is not transient");
        }
    }

    #[test]
    fn success_runs_once() {
        let calls = Cell::new(0u32);
        let result = retry_transient(|| {
            calls.set(calls.get() + 1);
            Ok(7)
        });
        assert_eq!(result.unwrap(), 7);
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn the_pause_between_retries_is_taken() {
        let started = std::time::Instant::now();
        let result: io::Result<()> = retry_by(
            RetryPolicy::new(2, Duration::from_millis(20)),
            is_held,
            || Err(held()),
        );
        assert!(result.is_err());
        assert!(
            started.elapsed() >= Duration::from_millis(40),
            "two retries pause twice: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn an_fs_error_is_transient_only_when_its_io_source_is() {
        let path = PathBuf::from("x");
        let calls = Cell::new(0u32);
        // The platform-independent half: an FsError that is not an Io error is never retried.
        let result: Result<()> = retry_transient_fs(|| {
            calls.set(calls.get() + 1);
            Err(FsError::Symlink(path.clone()))
        });
        assert!(matches!(result, Err(FsError::Symlink(_))));
        assert_eq!(calls.get(), 1);
        // A missing path is returned at once on every platform.
        calls.set(0);
        let result: Result<()> = retry_transient_fs(|| {
            calls.set(calls.get() + 1);
            Err(FsError::io(&path, io::Error::from(io::ErrorKind::NotFound)))
        });
        assert!(result.unwrap_err().is_not_found());
        assert_eq!(calls.get(), 1);
    }

    #[cfg(not(windows))]
    #[test]
    fn off_windows_the_windows_codes_run_once() {
        for code in [5, 32, 33] {
            let calls = Cell::new(0u32);
            let result: io::Result<()> = retry_transient(|| {
                calls.set(calls.get() + 1);
                Err(io::Error::from_raw_os_error(code))
            });
            assert_eq!(result.unwrap_err().raw_os_error(), Some(code));
            assert_eq!(calls.get(), 1, "code {code} is not a refusal here");
        }
    }

    #[cfg(windows)]
    #[test]
    fn on_windows_the_three_codes_are_retried_and_then_returned_unchanged() {
        for code in [5, 32, 33] {
            let calls = Cell::new(0u32);
            let result = retry_transient_with(quick(5), || {
                calls.set(calls.get() + 1);
                if calls.get() <= 2 {
                    Err(io::Error::from_raw_os_error(code))
                } else {
                    Ok(calls.get())
                }
            });
            assert_eq!(result.unwrap(), 3, "code {code} clears after two retries");

            calls.set(0);
            let result: io::Result<()> = retry_transient_with(quick(2), || {
                calls.set(calls.get() + 1);
                Err(io::Error::from_raw_os_error(code))
            });
            assert_eq!(result.unwrap_err().raw_os_error(), Some(code));
            assert_eq!(calls.get(), 3, "code {code}: one attempt plus two retries");
        }
        let calls = Cell::new(0u32);
        let result: Result<()> = retry_transient_fs(|| {
            calls.set(calls.get() + 1);
            Err(FsError::io(
                "x",
                io::Error::from_raw_os_error(if calls.get() <= 2 { 32 } else { 2 }),
            ))
        });
        assert!(result.unwrap_err().is_not_found());
        assert_eq!(calls.get(), 3, "retried while 32, returned at once on 2");
    }
}
