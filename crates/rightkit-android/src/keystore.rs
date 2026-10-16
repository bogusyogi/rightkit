//! The JNI implementation of `rightkit_credentials::android::KeystoreBridge`.
//!
//! The AES-GCM work happens in Kotlin (`RightKitKeystore`, inside `AndroidKeyStore`); this module
//! only calls its two static methods and maps failures to `BridgeError`. The Kotlin contract is
//! the one documented in `rightkit-credentials/src/android.rs`.
//!
//! ## Kotlin side
//! ```text
//! object RightKitKeystore {
//!   @JvmStatic fun encrypt(alias: String, plaintext: ByteArray, aad: ByteArray): ByteArray
//!   @JvmStatic fun decrypt(alias: String, blob: ByteArray, aad: ByteArray): ByteArray
//! }
//! class RightKitBridgeException(val code: Int) : RuntimeException()   // getCode()
//! ```
//! Codes: [`CODE_NO_CONTEXT`], [`CODE_KEY_MISSING`], [`CODE_AUTH_FAILED`]; any other value is
//! carried as `BridgeError::Failed(code)` (the Kotlin side uses 100 and above).
//!
//! ## Limits
//! The plaintext and the decrypted secret pass through Java byte arrays that this crate cannot
//! zero; they live until the Java GC reclaims them. The class is found once at init (a native
//! thread cannot look up an app class later) and held as a global reference.

use crate::context::{AndroidContext, ContextHandleExt};
use jni::objects::{GlobalRef, JByteArray, JClass, JObject, JValue};
use jni::JNIEnv;
use rightkit_credentials::android::{BridgeError, KeystoreBridge};

/// JNI name of the Kotlin class (slash form, as `FindClass` takes it).
pub const KEYSTORE_CLASS: &str = "com/orthiclabs/rightkit/android/RightKitKeystore";

pub const CODE_NO_CONTEXT: i32 = 1;
pub const CODE_KEY_MISSING: i32 = 2;
pub const CODE_AUTH_FAILED: i32 = 3;
/// A JNI problem on the Rust side (string or array creation failed, or a null result).
pub const CODE_JNI: i32 = 90;

/// Maps the code of a `RightKitBridgeException` to a `BridgeError`.
pub fn code_to_bridge_error(code: i32) -> BridgeError {
    match code {
        CODE_NO_CONTEXT => BridgeError::NoContext,
        CODE_KEY_MISSING => BridgeError::KeyMissing,
        CODE_AUTH_FAILED => BridgeError::AuthenticationFailed,
        other => BridgeError::Failed(other),
    }
}

/// The bridge installed into `rightkit-credentials` by `init`. Stateless: it reaches the process
/// context on every call, so a call before init is `BridgeError::NoContext`.
pub struct JniKeystoreBridge;

impl KeystoreBridge for JniKeystoreBridge {
    fn encrypt(&self, alias: &str, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, BridgeError> {
        call("encrypt", alias, plaintext, aad)
    }

    fn decrypt(&self, alias: &str, blob: &[u8], aad: &[u8]) -> Result<Vec<u8>, BridgeError> {
        call("decrypt", alias, blob, aad)
    }
}

fn call(method: &str, alias: &str, data: &[u8], aad: &[u8]) -> Result<Vec<u8>, BridgeError> {
    let ctx = AndroidContext::get().map_err(|_| BridgeError::NoContext)?;
    let mut outcome: Result<Vec<u8>, BridgeError> = Err(BridgeError::NoContext);
    ctx.with_env(|env, _app| {
        outcome = invoke(env, ctx.keystore_class(), method, alias, data, aad);
        Ok(())
    })
    .map_err(|_| BridgeError::NoContext)?;
    outcome
}

fn jni_failed() -> BridgeError {
    BridgeError::Failed(CODE_JNI)
}

fn invoke(
    env: &mut JNIEnv<'_>,
    class: &GlobalRef,
    method: &str,
    alias: &str,
    data: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, BridgeError> {
    // SAFETY: the raw reference belongs to a live `GlobalRef`; `JObject`/`JClass` do not delete it.
    let class = JClass::from(unsafe { JObject::from_raw(class.as_obj().as_raw()) });
    let alias = env.new_string(alias).map_err(|_| jni_failed())?;
    let data = env.byte_array_from_slice(data).map_err(|_| jni_failed())?;
    let aad = env.byte_array_from_slice(aad).map_err(|_| jni_failed())?;
    let result = env.call_static_method(
        &class,
        method,
        "(Ljava/lang/String;[B[B)[B",
        &[
            JValue::Object(&*alias),
            JValue::Object(&*data),
            JValue::Object(&*aad),
        ],
    );
    match result {
        Ok(value) => {
            let obj = value.l().map_err(|_| jni_failed())?;
            if obj.is_null() {
                return Err(jni_failed());
            }
            let array = JByteArray::from(obj);
            env.convert_byte_array(&array).map_err(|_| jni_failed())
        }
        Err(jni::errors::Error::JavaException) => Err(read_exception(env)),
        Err(_) => Err(jni_failed()),
    }
}

/// Reads `RightKitBridgeException.getCode()` from the pending exception and clears it. Never
/// reads the message (it could carry provider text).
fn read_exception(env: &mut JNIEnv<'_>) -> BridgeError {
    let Ok(exception) = env.exception_occurred() else {
        return jni_failed();
    };
    let _ = env.exception_clear();
    if exception.is_null() {
        return jni_failed();
    }
    let code = env
        .call_method(&exception, "getCode", "()I", &[])
        .and_then(|v| v.i());
    match code {
        Ok(code) => code_to_bridge_error(code),
        Err(_) => {
            if env.exception_check().unwrap_or(false) {
                let _ = env.exception_clear();
            }
            jni_failed()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exception_codes_map_to_bridge_errors() {
        assert_eq!(code_to_bridge_error(1), BridgeError::NoContext);
        assert_eq!(code_to_bridge_error(2), BridgeError::KeyMissing);
        assert_eq!(code_to_bridge_error(3), BridgeError::AuthenticationFailed);
        assert_eq!(code_to_bridge_error(100), BridgeError::Failed(100));
        assert_eq!(code_to_bridge_error(0), BridgeError::Failed(0));
    }

    #[test]
    fn a_call_before_init_is_no_context() {
        let bridge = JniKeystoreBridge;
        assert_eq!(
            bridge.encrypt("alias", b"secret", b"aad"),
            Err(BridgeError::NoContext)
        );
        assert_eq!(
            bridge.decrypt("alias", b"blob", b"aad"),
            Err(BridgeError::NoContext)
        );
    }

    #[test]
    fn the_class_name_matches_the_kotlin_package() {
        assert_eq!(
            KEYSTORE_CLASS,
            "com/orthiclabs/rightkit/android/RightKitKeystore"
        );
        let kt = include_str!(
            "../android/src/main/kotlin/com/orthiclabs/rightkit/android/RightKitKeystore.kt"
        );
        assert!(kt.contains("package com.orthiclabs.rightkit.android"));
        assert!(kt.contains("object RightKitKeystore"));
        assert!(kt.contains(&format!("CODE_KEY_MISSING = {CODE_KEY_MISSING}")));
        assert!(kt.contains(&format!("CODE_AUTH_FAILED = {CODE_AUTH_FAILED}")));
        assert!(kt.contains(&format!("CODE_NO_CONTEXT = {CODE_NO_CONTEXT}")));
    }
}
