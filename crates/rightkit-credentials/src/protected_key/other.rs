//! No OS-bound key on this platform: every call is `Unsupported`.

use super::{ProtectedKeyError, Protection, PublicKey, Result};

pub(super) fn public_key(_tag: &str) -> Result<Option<PublicKey>> {
    Err(ProtectedKeyError::Unsupported)
}
pub(super) fn create(_tag: &str, _protection: Protection) -> Result<PublicKey> {
    Err(ProtectedKeyError::Unsupported)
}
pub(super) fn sign(_tag: &str, _message: &[u8], _reason: &str) -> Result<Vec<u8>> {
    Err(ProtectedKeyError::Unsupported)
}
pub(super) fn delete(_tag: &str) -> Result<bool> {
    Err(ProtectedKeyError::Unsupported)
}
pub(super) fn seal(_tag: &str, _plaintext: &[u8], _aad: &[u8]) -> Result<Vec<u8>> {
    Err(ProtectedKeyError::Unsupported)
}
pub(super) fn unseal(_tag: &str, _blob: &[u8], _aad: &[u8], _reason: &str) -> Result<Vec<u8>> {
    Err(ProtectedKeyError::Unsupported)
}
