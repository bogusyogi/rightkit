//! Windows: Windows Hello through `Windows.Security.Credentials.UI.UserConsentVerifier`.
//!
//! The WinRT calls run on a dedicated thread in the multithreaded apartment, so a caller on a UI
//! (STA) thread never joins a WinRT operation itself and the wait can be bounded. With an owner
//! window the prompt is requested through `IUserConsentVerifierInterop::
//! RequestVerificationForWindowAsync`, which an unpackaged desktop app needs for the Hello dialog
//! to appear in front of its window; without one the plain `RequestVerificationAsync` is used
//! (packaged apps). On expiry the operation is cancelled and a late result is discarded.
//!
//! Hello only: there is no CredUI or password fallback. A user with no Hello method set up
//! (`NotConfiguredForUser`) gets `OsAuthError::NotEnrolled` and `Availability::NotEnrolled`, and
//! nothing is shown; the caller must offer its own fallback or ask the person to set up Hello.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use ::windows::core::{factory, HSTRING};
use ::windows::Security::Credentials::UI::{UserConsentVerifier, UserConsentVerifierAvailability};
use ::windows::Win32::Foundation::HWND;
use ::windows::Win32::System::WinRT::{
    IUserConsentVerifierInterop, RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED,
};

use super::{
    map_hello_availability, map_hello_result, Availability, OsAuthError, PromptLease, Request,
    Verified,
};

/// `HRESULT_FROM_WIN32(ERROR_CANCELLED)`, the code WinRT async objects conventionally report for a
/// cancelled operation (C++/WinRT's `hresult_canceled`). This is not verified against the Hello
/// implementation itself, and the mapping is fail closed either way: any other code is an `Os`
/// error, never a pass. A prompt the person dismisses is the `Canceled` result value (mapped to
/// `OsAuthError::Cancelled`), not this error. Only the timeout path of this module cancels an
/// operation, and there the caller has already answered `Timeout`; an operation that reports this
/// code without that was cancelled by the system, hence `SystemCancelled`.
const HR_CANCELLED: i32 = 0x800704C7_u32 as i32;
/// The availability probe is passive and quick; bound it anyway.
const AVAILABILITY_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the timeout path waits for the short cancelling thread (`Cancel` is one quick call).
const CANCEL_TIMEOUT: Duration = Duration::from_secs(2);

fn os_error(op: &'static str, error: &::windows::core::Error) -> OsAuthError {
    if error.code().0 == HR_CANCELLED {
        OsAuthError::SystemCancelled
    } else {
        OsAuthError::Os {
            op,
            code: i64::from(error.code().0),
        }
    }
}

/// Runs `work` on a fresh thread initialised for the multithreaded apartment and waits at most
/// `timeout`. A panic or a thread that cannot start is `Unavailable`, never a pass.
fn on_mta_thread<T: Send + 'static>(
    timeout: Duration,
    work: impl FnOnce() -> Result<T, OsAuthError> + Send + 'static,
) -> Result<T, OsAuthError> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("rightkit-os-auth".into())
        .spawn(move || {
            // SAFETY: balanced with `RoUninitialize` below when it succeeded; a failure (the
            // thread already has an apartment) leaves the existing one untouched.
            let initialised = unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.is_ok();
            let result = work();
            if initialised {
                // SAFETY: matches the successful `RoInitialize` on this thread.
                unsafe { RoUninitialize() };
            }
            let _ = tx.send(result);
        })
        .map_err(|_| OsAuthError::Unavailable)?;
    match rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(OsAuthError::Timeout),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(OsAuthError::Unavailable),
    }
}

pub fn availability() -> Availability {
    let probed = on_mta_thread(AVAILABILITY_TIMEOUT, || {
        let operation = UserConsentVerifier::CheckAvailabilityAsync()
            .map_err(|e| os_error("CheckAvailabilityAsync", &e))?;
        let value: UserConsentVerifierAvailability = operation
            .join()
            .map_err(|e| os_error("CheckAvailabilityAsync", &e))?;
        Ok(map_hello_availability(value.0))
    });
    match probed {
        Ok(availability) => availability,
        Err(OsAuthError::Timeout) => Availability::Busy,
        Err(_) => Availability::Unavailable,
    }
}

/// Cancels the in-flight WinRT operation. It is built from the operation itself, so this module
/// never names the `IAsyncOperation` type (in windows 0.62 it lives in the separate
/// `windows-future` crate, not under `windows::Foundation`) and needs no dependency on it.
type CancelOperation = Box<dyn FnOnce() + Send>;

/// The in-flight operation's cancel handle, shared so the waiting caller can cancel it on timeout.
struct InFlight {
    cancelled: AtomicBool,
    launch: AtomicU8,
    cancel: Mutex<Option<CancelOperation>>,
}

const WAITING: u8 = 0;
const STARTED: u8 = 1;
const CANCELLED: u8 = 2;

impl InFlight {
    fn begin(&self, deadline: Instant) -> Result<(), OsAuthError> {
        if Instant::now() >= deadline {
            return Err(OsAuthError::Timeout);
        }
        self.launch
            .compare_exchange(WAITING, STARTED, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| OsAuthError::Timeout)?;
        if Instant::now() >= deadline || self.cancelled.load(Ordering::SeqCst) {
            return Err(OsAuthError::Timeout);
        }
        Ok(())
    }
}

pub(super) fn authenticate(
    request: &Request<'_>,
    prompt: Arc<PromptLease>,
) -> Result<Verified, OsAuthError> {
    let deadline = Instant::now() + request.timeout;
    let message = HSTRING::from(request.reason.trim());
    let owner = request.owner_window;
    let in_flight = Arc::new(InFlight {
        cancelled: AtomicBool::new(false),
        launch: AtomicU8::new(WAITING),
        cancel: Mutex::new(None),
    });

    let worker = Arc::clone(&in_flight);
    let outcome = on_mta_thread(request.timeout, move || {
        // Keep ownership until the actual OS operation settles, even after caller timeout.
        let _prompt = prompt;
        let operation = match owner {
            Some(handle) => {
                let interop: IUserConsentVerifierInterop =
                    factory::<UserConsentVerifier, IUserConsentVerifierInterop>()
                        .map_err(|e| os_error("UserConsentVerifier factory", &e))?;
                let window = HWND(handle as *mut core::ffi::c_void);
                // The result type is inferred: it unifies with the `RequestVerificationAsync`
                // arm below, which returns `IAsyncOperation<UserConsentVerificationResult>`.
                // SAFETY: `window` is the caller-supplied native window handle (non-zero,
                // validated); the interop call only reads it and `message` is a live HSTRING.
                worker.begin(deadline)?;
                let started =
                    unsafe { interop.RequestVerificationForWindowAsync(window, &message) };
                started.map_err(|e| os_error("RequestVerificationForWindowAsync", &e))?
            }
            None => {
                worker.begin(deadline)?;
                UserConsentVerifier::RequestVerificationAsync(&message)
                    .map_err(|e| os_error("RequestVerificationAsync", &e))?
            }
        };

        // Publish the cancel handle, then re-check `cancelled`: the caller sets `cancelled` and
        // then takes the handle, so one side or the other always cancels.
        let handle = operation.clone();
        let cancel: CancelOperation = Box::new(move || {
            let _ = handle.Cancel();
        });
        *worker.cancel.lock().unwrap_or_else(PoisonError::into_inner) = Some(cancel);
        if worker.cancelled.load(Ordering::SeqCst) {
            let _ = operation.Cancel();
        }
        let joined = operation.join();
        // The operation is finished: drop the stored handle here, on the MTA thread, instead of on
        // whichever thread releases the last `Arc` after the apartment is gone.
        let finished = worker
            .cancel
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        drop(finished);
        let result = joined.map_err(|e| os_error("RequestVerificationAsync", &e))?;
        map_hello_result(result.0)
    });

    let outcome = outcome.and_then(|()| {
        if Instant::now() >= deadline {
            Err(OsAuthError::Timeout)
        } else {
            Ok(())
        }
    });
    match outcome {
        Ok(()) => Ok(Verified::now()),
        Err(error) => {
            // Wins against a worker not yet admitted to initiation. A worker that already won
            // retains the prompt lease until its operation actually completes.
            in_flight.launch.swap(CANCELLED, Ordering::SeqCst);
            // Whatever the failure (a timeout, or a worker that died with the prompt still open),
            // make sure no operation is left running: tell a worker that has not stored its
            // handle yet to cancel, and cancel one that has. A handle the worker already took
            // (the operation finished) leaves nothing to do.
            in_flight.cancelled.store(true, Ordering::SeqCst);
            let pending = in_flight
                .cancel
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            if let Some(cancel) = pending {
                // Run the cancel, and drop the captured operation, on a short MTA thread rather
                // than on this caller (often an STA thread with no multithreaded apartment).
                let _ = on_mta_thread(CANCEL_TIMEOUT, move || {
                    cancel();
                    Ok(())
                });
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timed_out_worker_cannot_begin_a_prompt() {
        let state = InFlight {
            cancelled: AtomicBool::new(true),
            launch: AtomicU8::new(CANCELLED),
            cancel: Mutex::new(None),
        };
        assert_eq!(
            state.begin(Instant::now() + Duration::from_secs(1)),
            Err(OsAuthError::Timeout)
        );
        let state = InFlight {
            cancelled: AtomicBool::new(false),
            launch: AtomicU8::new(WAITING),
            cancel: Mutex::new(None),
        };
        assert_eq!(state.begin(Instant::now()), Err(OsAuthError::Timeout));
        assert_eq!(state.launch.load(Ordering::SeqCst), WAITING);
    }
}
