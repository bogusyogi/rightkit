//! macOS: `LocalAuthentication` through the Objective-C runtime (`LAContext`).
//!
//! Policy `LAPolicyDeviceOwnerAuthentication` (2): Touch ID, Apple Watch or the account password.
//! `canEvaluatePolicy:error:` is passive and never prompts; `evaluatePolicy:localizedReason:reply:`
//! shows the system sheet and answers on a private queue, which a channel carries back so the
//! caller can bound the wait. On expiry the context is invalidated (the sheet closes) and the late
//! reply, if any, is dropped with the receiver.

use std::sync::mpsc;

use block2::RcBlock;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2_foundation::{NSError, NSString};

use super::{map_la_code, Availability, OsAuthError, Request, Verified};

#[link(name = "LocalAuthentication", kind = "framework")]
extern "C" {}

/// `LAPolicyDeviceOwnerAuthentication` (an `NSInteger`).
const POLICY_DEVICE_OWNER_AUTHENTICATION: isize = 2;
/// `LAErrorDomain`.
const LA_ERROR_DOMAIN: &str = "com.apple.LocalAuthentication";

fn new_context() -> Result<Retained<AnyObject>, OsAuthError> {
    let class = AnyClass::get(c"LAContext").ok_or(OsAuthError::Unavailable)?;
    // SAFETY: `+[LAContext new]` takes no arguments and returns a new LAContext (or nil).
    let context: Option<Retained<AnyObject>> = unsafe { msg_send![class, new] };
    context.ok_or(OsAuthError::Unavailable)
}

fn la_error(op: &'static str, error: &NSError) -> OsAuthError {
    let code = error.code() as i64;
    let domain = error.domain().to_string();
    if domain == LA_ERROR_DOMAIN {
        map_la_code(code)
    } else {
        OsAuthError::Os { op, code }
    }
}

pub fn availability() -> Availability {
    let Ok(context) = new_context() else {
        return Availability::Unavailable;
    };
    // SAFETY: `-[LAContext canEvaluatePolicy:error:]` takes an NSInteger policy and an NSError
    // out-parameter and returns BOOL; it never shows UI.
    let can: Result<(), Retained<NSError>> = unsafe {
        msg_send![&*context, canEvaluatePolicy: POLICY_DEVICE_OWNER_AUTHENTICATION, error: _]
    };
    match can {
        Ok(()) => Availability::Available,
        Err(error) => match la_error("canEvaluatePolicy", &error) {
            OsAuthError::NotEnrolled => Availability::NotEnrolled,
            OsAuthError::LockedOut => Availability::Busy,
            _ => Availability::Unavailable,
        },
    }
}

pub fn authenticate(request: &Request<'_>) -> Result<Verified, OsAuthError> {
    let context = new_context()?;

    // Fail before the prompt when the policy cannot be evaluated at all (no passcode set).
    // SAFETY: as in `availability`.
    let can: Result<(), Retained<NSError>> = unsafe {
        msg_send![&*context, canEvaluatePolicy: POLICY_DEVICE_OWNER_AUTHENTICATION, error: _]
    };
    if let Err(error) = can {
        return Err(la_error("canEvaluatePolicy", &error));
    }

    let reason = NSString::from_str(request.reason.trim());
    let (tx, rx) = mpsc::channel::<Result<(), OsAuthError>>();
    let reply = RcBlock::new(move |success: Bool, error: *mut NSError| {
        let outcome = if success.as_bool() {
            Ok(())
        } else if error.is_null() {
            Err(OsAuthError::Failed)
        } else {
            // SAFETY: a non-null error pointer is a live NSError for the duration of the reply.
            Err(la_error("evaluatePolicy", unsafe { &*error }))
        };
        let _ = tx.send(outcome);
    });

    // SAFETY: `-[LAContext evaluatePolicy:localizedReason:reply:]` takes an NSInteger policy, a
    // non-empty NSString and a `void (^)(BOOL, NSError *)` block, and returns void. The block is
    // copied by the callee; `reply` and `context` stay alive until this function returns.
    let _: () = unsafe {
        msg_send![
            &*context,
            evaluatePolicy: POLICY_DEVICE_OWNER_AUTHENTICATION,
            localizedReason: &*reason,
            reply: &*reply
        ]
    };

    match rx.recv_timeout(request.timeout) {
        Ok(Ok(())) => Ok(Verified::now()),
        Ok(Err(error)) => Err(error),
        Err(why) => {
            // SAFETY: `-[LAContext invalidate]` takes no arguments; it dismisses the sheet. The
            // receiver is dropped on return, so a reply racing this is discarded, never a pass.
            let _: () = unsafe { msg_send![&*context, invalidate] };
            Err(match why {
                mpsc::RecvTimeoutError::Timeout => OsAuthError::Timeout,
                // The block was released without replying: no answer is not a pass.
                mpsc::RecvTimeoutError::Disconnected => OsAuthError::Unavailable,
            })
        }
    }
}
