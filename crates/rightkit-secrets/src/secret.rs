use crate::error::{Error, Result};
use zeroize::Zeroizing;

/// Secret bytes. Zeroed on drop, no `Display`, no serde, redacted `Debug`.
#[derive(Clone)]
pub struct Secret(Zeroizing<Vec<u8>>);

impl Secret {
    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Self {
        Self(Zeroizing::new(bytes.into()))
    }
    pub fn from_string(value: impl Into<String>) -> Self {
        Self::from_bytes(value.into().into_bytes())
    }
    pub fn expose_bytes(&self) -> &[u8] {
        &self.0
    }
    pub fn expose_str(&self) -> Result<&str> {
        std::str::from_utf8(&self.0).map_err(|_| Error::NotUtf8)
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    /// Comparison that does not short-circuit on the first differing byte.
    pub fn ct_eq(&self, other: &Secret) -> bool {
        let (a, b) = (&self.0, &other.0);
        // The length is not secret; compare it in full (a truncated XOR once made 32- and
        // 288-byte secrets with a shared prefix compare equal).
        if a.len() != b.len() {
            return false;
        }
        let mut diff = 0u8;
        for (x, y) in a.iter().zip(b.iter()) {
            diff |= x ^ y;
        }
        diff == 0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secret(<redacted {} bytes>)", self.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ct_eq_distinguishes_lengths_that_differ_by_a_multiple_of_256() {
        let short = Secret::from_bytes(vec![7u8; 32]);
        let long = Secret::from_bytes(vec![7u8; 288]);
        assert!(!short.ct_eq(&long));
        assert!(!long.ct_eq(&short));
        assert!(short.ct_eq(&Secret::from_bytes(vec![7u8; 32])));
        assert!(!short.ct_eq(&Secret::from_bytes(vec![8u8; 32])));
        assert!(Secret::from_bytes(Vec::new()).ct_eq(&Secret::from_bytes(Vec::new())));
    }
}
