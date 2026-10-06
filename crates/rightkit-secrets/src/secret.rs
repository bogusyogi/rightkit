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
        let mut diff = (a.len() ^ b.len()) as u8;
        for i in 0..a.len().min(b.len()) {
            diff |= a[i] ^ b[i];
        }
        diff == 0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secret(<redacted {} bytes>)", self.len())
    }
}
