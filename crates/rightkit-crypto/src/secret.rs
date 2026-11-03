//! A byte secret that is wiped when dropped and never printed.

/// Owned secret bytes. The buffer is overwritten with zeros (volatile writes the optimizer may not
/// remove) on drop. `Debug` prints only the length, and `Clone` is not implemented so a copy is
/// always an explicit `Secret::new(secret.expose().to_vec())`.
pub struct Secret(Vec<u8>);

impl Secret {
    /// Take ownership of `bytes`.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// `len` fresh bytes from the system random generator.
    pub fn random(len: usize) -> Result<Self, crate::CryptoError> {
        let mut bytes = vec![0u8; len];
        crate::random_fill(&mut bytes)?;
        Ok(Self(bytes))
    }

    /// The secret bytes. Keep the borrow short; do not format or log it.
    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    /// Length in bytes.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True when the secret holds no bytes.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Constant-time equality with `other` (unequal lengths are not equal).
    pub fn ct_eq(&self, other: &[u8]) -> bool {
        crate::ct_eq(&self.0, other)
    }
}

/// Overwrite `bytes` with zeros in a way the compiler may not elide.
pub fn wipe(bytes: &mut [u8]) {
    for b in bytes.iter_mut() {
        // SAFETY: `b` is a valid, aligned, exclusive reference to one initialized byte.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

impl Drop for Secret {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secret(<{} bytes>)", self.0.len())
    }
}
