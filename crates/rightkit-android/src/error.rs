//! The error type crossing the UniFFI boundary and the JNI init return codes.

/// Errors of the Android bridge. Messages never contain a path, a secret or a provider message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum AndroidError {
    /// `RightKit.init` has not run (or failed).
    #[error("rightkit-android is not initialised")]
    NotInitialised,
    /// An argument was rejected; `reason` is a fixed description.
    #[error("invalid argument: {reason}")]
    Invalid { reason: String },
    /// A JNI call failed; `code` is one of the `JNI_*` constants.
    #[error("JNI failure (code {code})")]
    Jni { code: i32 },
}

impl AndroidError {
    pub(crate) fn invalid(reason: &str) -> Self {
        AndroidError::Invalid {
            reason: reason.to_string(),
        }
    }
}

/// The JavaVM could not be reached (no VM captured, or the thread could not be attached).
pub const JNI_NO_VM: i32 = 1;
/// A Java exception was pending after a call.
pub const JNI_EXCEPTION: i32 = 10;
/// A null reference where an object was required.
pub const JNI_NULL: i32 = 11;
/// Any other `jni` crate error.
pub const JNI_OTHER: i32 = 19;

/// Maps a `jni` crate error to a small stable code.
pub fn jni_error_code(e: &jni::errors::Error) -> i32 {
    match e {
        jni::errors::Error::JavaException | jni::errors::Error::CaughtJavaException { .. } => {
            JNI_EXCEPTION
        }
        jni::errors::Error::NullPtr(_) => JNI_NULL,
        _ => JNI_OTHER,
    }
}

/// What `RightKitNative.nativeInit` returns to Kotlin. `0` is success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitCode {
    Ok = 0,
    /// A path argument was rejected.
    InvalidArgument = 1,
    /// A JNI call failed while capturing the context.
    Jni = 2,
    /// `init` ran before with different paths; the first call wins and paths cannot change.
    PathsChanged = 3,
    /// Installing the Keystore store into rightkit-credentials failed.
    KeystoreInstall = 4,
    /// A panic was caught at the JNI boundary.
    Panic = 5,
}

impl InitCode {
    pub fn as_i32(self) -> i32 {
        self as i32
    }
}
