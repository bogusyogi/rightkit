//! macOS: a Secure Enclave P-256 key through Security.framework (raw FFI, like `macos.rs`).
//!
//! The key is a permanent Secure Enclave key (`kSecAttrTokenIDSecureEnclave`) tagged with the
//! key's tag, with an access control of `privateKeyUsage` plus `biometryCurrentSet` or
//! `userPresence`, accessible only while unlocked and only on this device. Secure Enclave keys
//! in the keychain need the signed app to carry a `keychain-access-groups` entitlement; an
//! unsigned or unentitled process gets `errSecMissingEntitlement` (-34018), reported as
//! `Unsupported`.
//!
//! A generic-password marker item (service = the tag, account `public-key`, value = the public
//! key bytes, device-only) is written next to the key. It holds nothing secret. A key that is
//! gone while its marker remains was invalidated (enrolment changed), which is reported as
//! `Invalidated`, never as missing.

use super::{ProtectedKeyError, Protection, PublicKey, Result, SignatureAlgorithm};
use std::ffi::c_void;
use std::ptr::{null, null_mut};

type CFTypeRef = *const c_void;
type OSStatus = i32;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFBooleanTrue: CFTypeRef;
    static kCFTypeDictionaryKeyCallBacks: u8;
    static kCFTypeDictionaryValueCallBacks: u8;
    fn CFRelease(cf: CFTypeRef);
    fn CFDictionaryCreate(
        allocator: CFTypeRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        count: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFTypeRef;
    fn CFDataCreate(allocator: CFTypeRef, bytes: *const u8, length: isize) -> CFTypeRef;
    fn CFDataGetBytePtr(data: CFTypeRef) -> *const u8;
    fn CFDataGetLength(data: CFTypeRef) -> isize;
    fn CFNumberCreate(allocator: CFTypeRef, number_type: isize, value: *const c_void) -> CFTypeRef;
    fn CFStringCreateWithBytes(
        allocator: CFTypeRef,
        bytes: *const u8,
        length: isize,
        encoding: u32,
        is_external: u8,
    ) -> CFTypeRef;
    fn CFErrorGetCode(error: CFTypeRef) -> isize;
}

#[link(name = "Security", kind = "framework")]
extern "C" {
    static kSecClass: CFTypeRef;
    static kSecClassKey: CFTypeRef;
    static kSecClassGenericPassword: CFTypeRef;
    static kSecAttrKeyType: CFTypeRef;
    static kSecAttrKeyTypeECSECPrimeRandom: CFTypeRef;
    static kSecAttrKeyClass: CFTypeRef;
    static kSecAttrKeyClassPrivate: CFTypeRef;
    static kSecAttrKeySizeInBits: CFTypeRef;
    static kSecAttrTokenID: CFTypeRef;
    static kSecAttrTokenIDSecureEnclave: CFTypeRef;
    static kSecPrivateKeyAttrs: CFTypeRef;
    static kSecAttrIsPermanent: CFTypeRef;
    static kSecAttrApplicationTag: CFTypeRef;
    static kSecAttrAccessControl: CFTypeRef;
    static kSecAttrAccessible: CFTypeRef;
    static kSecAttrAccessibleWhenUnlockedThisDeviceOnly: CFTypeRef;
    static kSecAttrService: CFTypeRef;
    static kSecAttrAccount: CFTypeRef;
    static kSecValueData: CFTypeRef;
    static kSecReturnRef: CFTypeRef;
    static kSecMatchLimit: CFTypeRef;
    static kSecMatchLimitOne: CFTypeRef;
    static kSecUseOperationPrompt: CFTypeRef;
    static kSecKeyAlgorithmECDSASignatureMessageX962SHA256: CFTypeRef;
    static kSecKeyAlgorithmECIESEncryptionCofactorX963SHA256AESGCM: CFTypeRef;
    fn SecAccessControlCreateWithFlags(
        allocator: CFTypeRef,
        protection: CFTypeRef,
        flags: usize,
        error: *mut CFTypeRef,
    ) -> CFTypeRef;
    fn SecKeyCreateRandomKey(parameters: CFTypeRef, error: *mut CFTypeRef) -> CFTypeRef;
    fn SecKeyCopyPublicKey(key: CFTypeRef) -> CFTypeRef;
    fn SecKeyCopyExternalRepresentation(key: CFTypeRef, error: *mut CFTypeRef) -> CFTypeRef;
    fn SecKeyCreateSignature(
        key: CFTypeRef,
        algorithm: CFTypeRef,
        data: CFTypeRef,
        error: *mut CFTypeRef,
    ) -> CFTypeRef;
    fn SecKeyCreateEncryptedData(
        key: CFTypeRef,
        algorithm: CFTypeRef,
        plaintext: CFTypeRef,
        error: *mut CFTypeRef,
    ) -> CFTypeRef;
    fn SecKeyCreateDecryptedData(
        key: CFTypeRef,
        algorithm: CFTypeRef,
        ciphertext: CFTypeRef,
        error: *mut CFTypeRef,
    ) -> CFTypeRef;
    fn SecItemCopyMatching(query: CFTypeRef, result: *mut CFTypeRef) -> OSStatus;
    fn SecItemAdd(attributes: CFTypeRef, result: *mut CFTypeRef) -> OSStatus;
    fn SecItemDelete(query: CFTypeRef) -> OSStatus;
}

const ERR_USER_CANCELED: i64 = -128;
const ERR_PARAM: i64 = -50;
const ERR_NOT_AVAILABLE: i64 = -25291;
const ERR_AUTH_FAILED: i64 = -25293;
const ERR_NOT_FOUND: i64 = -25300;
const ERR_DUPLICATE: i64 = -25299;
const ERR_INTERACTION_NOT_ALLOWED: i64 = -25308;
const ERR_MISSING_ENTITLEMENT: i64 = -34018;
const ACL_USER_PRESENCE: usize = 1 << 0;
const ACL_BIOMETRY_CURRENT_SET: usize = 1 << 3;
const ACL_PRIVATE_KEY_USAGE: usize = 1 << 30;
const CF_NUMBER_SINT32: isize = 3;
const CF_STRING_UTF8: u32 = 0x0800_0100;
const MARKER_ACCOUNT: &str = "public-key";
/// Size of the uncompressed X9.63 P-256 point.
const POINT_LEN: usize = 65;

/// An owned CoreFoundation object, released on drop.
struct Cf(CFTypeRef);

impl Cf {
    fn raw(&self) -> CFTypeRef {
        self.0
    }
}

impl Drop for Cf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `self.0` is a +1 reference obtained from a Create/Copy function.
            unsafe { CFRelease(self.0) };
        }
    }
}

fn own(raw: CFTypeRef) -> Result<Cf> {
    if raw.is_null() {
        Err(ProtectedKeyError::Os {
            op: "core foundation allocation",
            code: 0,
        })
    } else {
        Ok(Cf(raw))
    }
}

fn data(bytes: &[u8]) -> Result<Cf> {
    // SAFETY: the pointer and length describe `bytes`, which CFDataCreate copies.
    own(unsafe { CFDataCreate(null(), bytes.as_ptr(), bytes.len() as isize) })
}

fn string(text: &str) -> Result<Cf> {
    // SAFETY: the pointer and length describe `text`, which CFString copies.
    own(unsafe {
        CFStringCreateWithBytes(
            null(),
            text.as_ptr(),
            text.len() as isize,
            CF_STRING_UTF8,
            0,
        )
    })
}

fn number(value: i32) -> Result<Cf> {
    // SAFETY: `value` is a live i32 and the type tag says SInt32.
    own(unsafe { CFNumberCreate(null(), CF_NUMBER_SINT32, (&value as *const i32).cast()) })
}

/// A CFDictionary of CF-typed keys and values (copied with the standard CFType callbacks, so the
/// dictionary retains what it holds and the inputs may be released afterwards).
fn dict(pairs: &[(CFTypeRef, CFTypeRef)]) -> Result<Cf> {
    let keys: Vec<CFTypeRef> = pairs.iter().map(|p| p.0).collect();
    let values: Vec<CFTypeRef> = pairs.iter().map(|p| p.1).collect();
    // SAFETY: the arrays hold `pairs.len()` valid CF objects; the callbacks are the CF constants.
    own(unsafe {
        CFDictionaryCreate(
            null(),
            keys.as_ptr(),
            values.as_ptr(),
            pairs.len() as isize,
            (&kCFTypeDictionaryKeyCallBacks as *const u8).cast(),
            (&kCFTypeDictionaryValueCallBacks as *const u8).cast(),
        )
    })
}

fn bytes_of(d: &Cf) -> Vec<u8> {
    // SAFETY: `d` is a CFData; the slice is copied before `d` is released.
    unsafe {
        let len = CFDataGetLength(d.raw());
        if len <= 0 {
            return Vec::new();
        }
        std::slice::from_raw_parts(CFDataGetBytePtr(d.raw()), len as usize).to_vec()
    }
}

/// The OSStatus code of a failed Security call's `CFError`, which is released here.
fn error_code(error: CFTypeRef) -> i64 {
    if error.is_null() {
        return 0;
    }
    let err = Cf(error);
    // SAFETY: `err` is a CFError.
    unsafe { CFErrorGetCode(err.raw()) as i64 }
}

fn map_code(op: &'static str, code: i64) -> ProtectedKeyError {
    match code {
        ERR_USER_CANCELED => ProtectedKeyError::Cancelled,
        ERR_AUTH_FAILED => ProtectedKeyError::Denied,
        ERR_INTERACTION_NOT_ALLOWED => ProtectedKeyError::Unavailable,
        ERR_MISSING_ENTITLEMENT | ERR_NOT_AVAILABLE => ProtectedKeyError::Unsupported,
        _ => ProtectedKeyError::Os { op, code },
    }
}

fn tag_data(tag: &str) -> Result<Cf> {
    data(tag.as_bytes())
}

/// The private-key query. `prompt` is the text of the OS verification sheet.
fn key_query(tag: &str, prompt: Option<&str>) -> Result<Cf> {
    let tag = tag_data(tag)?;
    let prompt = prompt.map(string).transpose()?;
    // SAFETY: the statics are constant CF objects exported by Security.framework.
    unsafe {
        let mut pairs = vec![
            (kSecClass, kSecClassKey),
            (kSecAttrKeyClass, kSecAttrKeyClassPrivate),
            (kSecAttrKeyType, kSecAttrKeyTypeECSECPrimeRandom),
            (kSecAttrApplicationTag, tag.raw()),
            (kSecReturnRef, kCFBooleanTrue),
            (kSecMatchLimit, kSecMatchLimitOne),
        ];
        if let Some(p) = &prompt {
            pairs.push((kSecUseOperationPrompt, p.raw()));
        }
        dict(&pairs)
    }
}

fn marker_query(tag: &str) -> Result<Cf> {
    let service = string(tag)?;
    let account = string(MARKER_ACCOUNT)?;
    // SAFETY: constant CF objects.
    unsafe {
        dict(&[
            (kSecClass, kSecClassGenericPassword),
            (kSecAttrService, service.raw()),
            (kSecAttrAccount, account.raw()),
        ])
    }
}

fn marker_exists(tag: &str) -> Result<bool> {
    let q = marker_query(tag)?;
    // SAFETY: `q` is a valid query; no result is requested.
    let status = unsafe { SecItemCopyMatching(q.raw(), null_mut()) } as i64;
    match status {
        0 => Ok(true),
        ERR_NOT_FOUND => Ok(false),
        other => Err(map_code("marker lookup", other)),
    }
}

fn write_marker(tag: &str, public: &[u8]) -> Result<()> {
    let service = string(tag)?;
    let account = string(MARKER_ACCOUNT)?;
    let value = data(public)?;
    // SAFETY: constant CF objects and live owned values.
    let attrs = unsafe {
        dict(&[
            (kSecClass, kSecClassGenericPassword),
            (kSecAttrService, service.raw()),
            (kSecAttrAccount, account.raw()),
            (kSecValueData, value.raw()),
            (
                kSecAttrAccessible,
                kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
            ),
        ])?
    };
    // SAFETY: `attrs` is a valid attribute dictionary.
    let status = unsafe { SecItemAdd(attrs.raw(), null_mut()) } as i64;
    match status {
        0 | ERR_DUPLICATE => Ok(()),
        other => Err(map_code("marker write", other)),
    }
}

/// The private key reference, or `None` when there is none.
fn find_private(tag: &str, prompt: Option<&str>) -> Result<Option<Cf>> {
    let q = key_query(tag, prompt)?;
    let mut out: CFTypeRef = null();
    // SAFETY: `q` is a valid query and `out` receives a +1 reference on success.
    let status = unsafe { SecItemCopyMatching(q.raw(), &mut out) } as i64;
    match status {
        0 => own(out).map(Some),
        ERR_NOT_FOUND => Ok(None),
        other => Err(map_code("key lookup", other)),
    }
}

/// The private key, or the reason there is none: `Invalidated` when the marker proves it existed.
fn require_private(tag: &str, prompt: Option<&str>) -> Result<Cf> {
    match find_private(tag, prompt)? {
        Some(key) => Ok(key),
        None => {
            if marker_exists(tag)? {
                Err(ProtectedKeyError::Invalidated)
            } else {
                Err(ProtectedKeyError::Missing)
            }
        }
    }
}

fn public_bytes(private: &Cf) -> Result<Vec<u8>> {
    // SAFETY: `private` is a SecKey; the copy is released by `Cf`.
    let public = own(unsafe { SecKeyCopyPublicKey(private.raw()) })?;
    let mut error: CFTypeRef = null();
    // SAFETY: `public` is a SecKey.
    let rep = unsafe { SecKeyCopyExternalRepresentation(public.raw(), &mut error) };
    if rep.is_null() {
        return Err(map_code("public key export", error_code(error)));
    }
    let rep = Cf(rep);
    let bytes = bytes_of(&rep);
    if bytes.len() != POINT_LEN || bytes[0] != 0x04 {
        return Err(ProtectedKeyError::Os {
            op: "public key export",
            code: 0,
        });
    }
    Ok(bytes)
}

fn as_public(bytes: Vec<u8>) -> PublicKey {
    PublicKey {
        algorithm: SignatureAlgorithm::EcdsaP256Sha256,
        bytes,
    }
}

pub(super) fn public_key(tag: &str) -> Result<Option<PublicKey>> {
    match find_private(tag, None)? {
        Some(private) => public_bytes(&private).map(as_public).map(Some),
        None => {
            if marker_exists(tag)? {
                Err(ProtectedKeyError::Invalidated)
            } else {
                Ok(None)
            }
        }
    }
}

pub(super) fn create(tag: &str, protection: Protection) -> Result<PublicKey> {
    let flags = ACL_PRIVATE_KEY_USAGE
        | match protection {
            Protection::BiometryCurrentSet => ACL_BIOMETRY_CURRENT_SET,
            Protection::UserPresence => ACL_USER_PRESENCE,
        };
    let mut error: CFTypeRef = null();
    // SAFETY: constant protection class and valid flags.
    let access = unsafe {
        SecAccessControlCreateWithFlags(
            null(),
            kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
            flags,
            &mut error,
        )
    };
    if access.is_null() {
        return Err(map_code("access control", error_code(error)));
    }
    let access = Cf(access);
    let tag_cf = tag_data(tag)?;
    let bits = number(256)?;
    // SAFETY: constant CF objects and live owned values.
    let private_attrs = unsafe {
        dict(&[
            (kSecAttrIsPermanent, kCFBooleanTrue),
            (kSecAttrApplicationTag, tag_cf.raw()),
            (kSecAttrAccessControl, access.raw()),
        ])?
    };
    // SAFETY: as above.
    let params = unsafe {
        dict(&[
            (kSecAttrKeyType, kSecAttrKeyTypeECSECPrimeRandom),
            (kSecAttrKeySizeInBits, bits.raw()),
            (kSecAttrTokenID, kSecAttrTokenIDSecureEnclave),
            (kSecPrivateKeyAttrs, private_attrs.raw()),
        ])?
    };
    let mut error: CFTypeRef = null();
    // SAFETY: `params` is a valid key-generation dictionary.
    let key = unsafe { SecKeyCreateRandomKey(params.raw(), &mut error) };
    if key.is_null() {
        let code = error_code(error);
        return Err(match code {
            // Biometry-bound creation with nothing enrolled, or a Mac without a Secure Enclave.
            ERR_AUTH_FAILED => ProtectedKeyError::NotEnrolled,
            ERR_PARAM => ProtectedKeyError::Unsupported,
            other => map_code("secure enclave key creation", other),
        });
    }
    let key = Cf(key);
    let public = public_bytes(&key)?;
    if let Err(e) = write_marker(tag, &public) {
        // A key whose marker cannot be written could not later be told from a missing one.
        let _ = delete_private(tag);
        return Err(e);
    }
    Ok(as_public(public))
}

pub(super) fn sign(tag: &str, message: &[u8], reason: &str) -> Result<Vec<u8>> {
    let private = require_private(tag, Some(reason))?;
    let payload = data(message)?;
    let mut error: CFTypeRef = null();
    // SAFETY: `private` is a SecKey, the algorithm a constant, `payload` a CFData.
    let sig = unsafe {
        SecKeyCreateSignature(
            private.raw(),
            kSecKeyAlgorithmECDSASignatureMessageX962SHA256,
            payload.raw(),
            &mut error,
        )
    };
    if sig.is_null() {
        return Err(map_code("sign", error_code(error)));
    }
    Ok(bytes_of(&Cf(sig)))
}

fn delete_private(tag: &str) -> Result<bool> {
    let tag_cf = tag_data(tag)?;
    // SAFETY: constant CF objects and a live tag.
    let q = unsafe {
        dict(&[
            (kSecClass, kSecClassKey),
            (kSecAttrKeyClass, kSecAttrKeyClassPrivate),
            (kSecAttrApplicationTag, tag_cf.raw()),
        ])?
    };
    // SAFETY: `q` is a valid query.
    match unsafe { SecItemDelete(q.raw()) } as i64 {
        0 => Ok(true),
        ERR_NOT_FOUND => Ok(false),
        other => Err(map_code("key delete", other)),
    }
}

pub(super) fn delete(tag: &str) -> Result<bool> {
    let removed = delete_private(tag)?;
    let q = marker_query(tag)?;
    // SAFETY: `q` is a valid query.
    let marker = match unsafe { SecItemDelete(q.raw()) } as i64 {
        0 => true,
        ERR_NOT_FOUND => false,
        other => return Err(map_code("marker delete", other)),
    };
    Ok(removed || marker)
}

/// The bytes that go through ECIES: the SHA-256 of `aad`, then the plaintext. ECIES here has no
/// associated-data input, so the binding is carried inside the encrypted payload and checked
/// after decryption.
fn bound(aad: &[u8], plaintext: &[u8]) -> zeroize::Zeroizing<Vec<u8>> {
    let mut out = Vec::with_capacity(32 + plaintext.len());
    out.extend_from_slice(&rightkit_crypto::sha256(aad));
    out.extend_from_slice(plaintext);
    zeroize::Zeroizing::new(out)
}

pub(super) fn seal(tag: &str, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    // Looking the key up and copying its public half never prompts: only use of the private
    // half does.
    let private = require_private(tag, None)?;
    // SAFETY: `private` is a SecKey.
    let public = own(unsafe { SecKeyCopyPublicKey(private.raw()) })?;
    let payload = bound(aad, plaintext);
    let input = data(&payload)?;
    let mut error: CFTypeRef = null();
    // SAFETY: `public` is a SecKey, the algorithm a constant, `input` a CFData.
    let out = unsafe {
        SecKeyCreateEncryptedData(
            public.raw(),
            kSecKeyAlgorithmECIESEncryptionCofactorX963SHA256AESGCM,
            input.raw(),
            &mut error,
        )
    };
    if out.is_null() {
        return Err(map_code("seal", error_code(error)));
    }
    Ok(bytes_of(&Cf(out)))
}

pub(super) fn unseal(tag: &str, blob: &[u8], aad: &[u8], reason: &str) -> Result<Vec<u8>> {
    let private = require_private(tag, Some(reason))?;
    let input = data(blob)?;
    let mut error: CFTypeRef = null();
    // SAFETY: `private` is a SecKey, the algorithm a constant, `input` a CFData.
    let out = unsafe {
        SecKeyCreateDecryptedData(
            private.raw(),
            kSecKeyAlgorithmECIESEncryptionCofactorX963SHA256AESGCM,
            input.raw(),
            &mut error,
        )
    };
    if out.is_null() {
        let code = error_code(error);
        // A blob this key cannot decrypt is a bad blob, not an OS failure; the OS errors that
        // carry the user's decision keep their meaning.
        return Err(match code {
            ERR_USER_CANCELED | ERR_AUTH_FAILED | ERR_INTERACTION_NOT_ALLOWED => {
                map_code("unseal", code)
            }
            _ => ProtectedKeyError::BadBlob,
        });
    }
    let plain = zeroize::Zeroizing::new(bytes_of(&Cf(out)));
    if plain.len() < 32 || !rightkit_crypto::ct_eq(&plain[..32], &rightkit_crypto::sha256(aad)) {
        return Err(ProtectedKeyError::BadBlob);
    }
    Ok(plain[32..].to_vec())
}
