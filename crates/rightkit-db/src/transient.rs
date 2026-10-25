//! Bounded retry for the Windows file errors that clear on their own.
//!
//! An indexer, a virus scanner or a backup agent can hold a file this crate wrote a moment ago
//! open without delete sharing. Windows then refuses a rename over it or a delete of it with
//! `ERROR_ACCESS_DENIED` (5), `ERROR_SHARING_VIOLATION` (32) or `ERROR_LOCK_VIOLATION` (33)
//! although nothing about the caller's rights is wrong, and the hold ends by itself within
//! moments. A file operation that is the last step of a longer, already-verified procedure (the
//! backup publish, a relocation journal write, the cleanup after a committed relocation) retries
//! these codes a bounded number of times instead of turning a passing hold into a failed
//! operation or a retained journal.
//!
//! Only Windows reports these codes in this meaning, so off Windows nothing is classified as
//! transient and every operation runs exactly once. After the bound the original error is
//! returned unchanged, so a true permission failure still fails, only later by
//! `ATTEMPTS * DELAY`.

use std::io;
use std::time::Duration;

/// Retries after the first attempt.
pub(crate) const ATTEMPTS: u32 = 20;
/// Pause before each retry. With [`ATTEMPTS`] the worst case adds two seconds to one operation.
pub(crate) const DELAY: Duration = Duration::from_millis(100);

/// Windows `ERROR_ACCESS_DENIED` (5), `ERROR_SHARING_VIOLATION` (32) and `ERROR_LOCK_VIOLATION`
/// (33). Never true off Windows.
pub(crate) fn is_transient(error: &io::Error) -> bool {
    transient_code(cfg!(windows), error.raw_os_error())
}

/// The classifier behind [`is_transient`], with the platform explicit so it is testable everywhere.
fn transient_code(windows: bool, raw: Option<i32>) -> bool {
    windows && matches!(raw, Some(5 | 32 | 33))
}

/// Run `operation`; while it fails with an error `transient` accepts, pause `delay` and run it
/// again, at most `attempts` more times. The last result is returned as it is.
pub(crate) fn retry<T>(
    attempts: u32,
    delay: Duration,
    transient: impl Fn(&io::Error) -> bool,
    mut operation: impl FnMut() -> io::Result<T>,
) -> io::Result<T> {
    let mut retries_left = attempts;
    loop {
        match operation() {
            Err(error) if retries_left > 0 && transient(&error) => {
                retries_left -= 1;
                std::thread::sleep(delay);
            }
            other => return other,
        }
    }
}

/// [`retry`] with this crate's bound and the Windows classifier.
pub(crate) fn retry_transient<T>(operation: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    retry(ATTEMPTS, DELAY, is_transient, operation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn busy() -> io::Error {
        io::Error::new(io::ErrorKind::WouldBlock, "held")
    }

    fn is_busy(error: &io::Error) -> bool {
        error.kind() == io::ErrorKind::WouldBlock
    }

    #[test]
    fn only_windows_codes_five_thirty_two_and_thirty_three_are_transient() {
        for code in [5, 32, 33] {
            assert!(transient_code(true, Some(code)), "{code}");
            assert!(!transient_code(false, Some(code)), "{code} off Windows");
        }
        for code in [0, 2, 3, 80, 145, 183] {
            assert!(!transient_code(true, Some(code)), "{code}");
        }
        assert!(!transient_code(true, None));
        assert!(!is_transient(&io::Error::from(io::ErrorKind::NotFound)));
        assert_eq!(
            is_transient(&io::Error::from_raw_os_error(5)),
            cfg!(windows),
            "error 5 is transient on Windows only"
        );
    }

    #[test]
    fn a_transient_failure_is_retried_until_it_clears() {
        let calls = Cell::new(0u32);
        let result = retry(5, Duration::ZERO, is_busy, || {
            calls.set(calls.get() + 1);
            if calls.get() <= 3 {
                Err(busy())
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
        let result: io::Result<()> = retry(3, Duration::ZERO, is_busy, || {
            calls.set(calls.get() + 1);
            Err(busy())
        });
        let error = result.unwrap_err();
        assert!(is_busy(&error), "{error:?}");
        assert_eq!(calls.get(), 4, "one attempt plus three retries");
    }

    #[test]
    fn zero_attempts_runs_once() {
        let calls = Cell::new(0u32);
        let result: io::Result<()> = retry(0, Duration::ZERO, is_busy, || {
            calls.set(calls.get() + 1);
            Err(busy())
        });
        assert!(result.is_err());
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn other_errors_and_not_found_return_at_once() {
        for kind in [io::ErrorKind::NotFound, io::ErrorKind::PermissionDenied] {
            let calls = Cell::new(0u32);
            let result: io::Result<()> = retry(5, Duration::ZERO, is_busy, || {
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
}
