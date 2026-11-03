//! Transparent chunking for secrets larger than one native credential can hold (rightkit#1126).
//!
//! Windows Credential Manager refuses a blob over 2,560 bytes (`CRED_MAX_CREDENTIAL_BLOB_SIZE`),
//! and an OAuth access plus refresh token can exceed that. [`ChunkedBackend`] wraps any
//! [`Backend`] and splits such a secret into generation-tagged chunk items behind a manifest:
//!
//! * The chunks of a new generation are written first; the manifest (the item under the caller's
//!   own account) is committed last. A reader sees the old complete value or the new complete
//!   value, never a mixture, and a crash before the manifest leaves only unreferenced chunks.
//! * A secret of at most [`INLINE_MAX`] bytes that does not begin with the manifest marker is
//!   stored as it always was, so existing items stay readable and small secrets cost nothing.
//! * A manifest whose chunk is missing or whose length disagrees is an error, never "absent":
//!   the reader must not mint a new key over a value it could not read.
//! * Chunks of a replaced or deleted generation are removed after the manifest changes; a failure
//!   to remove them leaves unreferenced chunks that no read ever sees (and a later write of the
//!   same account does not reuse their generation).
//!
//! Mutations of one credential are serialised by [`crate::Store`] on the manifest's identity, which
//! covers the whole multi-item write. Chunk accounts are `<account>.rkchunk.<generation>.<index>`.

use crate::backend::{Backend, ConditionalDelete, Interaction};
use crate::error::{Error, Result};
use crate::namespace::Namespace;
use crate::secret::Secret;
use std::sync::Arc;
use zeroize::Zeroizing;

/// The most bytes one item holds. Windows allows 2,560 bytes of blob, and the keyring-compatible
/// Windows codec stores UTF-16 (up to two bytes per byte of text), so 1,200 stays under the limit
/// in every codec.
pub const INLINE_MAX: usize = 1_200;

const MAGIC: &[u8; 8] = b"RKCHUNK1";
/// Magic, generation (u64), chunk count (u32), total length (u64).
const MANIFEST_LEN: usize = 8 + 8 + 4 + 8;
/// A secret may need at most this many chunks (1,200 bytes each: a 4.8 MB secret). Bounded so a
/// corrupt manifest cannot make a reader loop over billions of accounts.
const MAX_CHUNKS: u32 = 4_096;

struct Manifest {
    generation: u64,
    count: u32,
    total: u64,
}

impl Manifest {
    fn encode(&self) -> Secret {
        let mut out = Vec::with_capacity(MANIFEST_LEN);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&self.generation.to_be_bytes());
        out.extend_from_slice(&self.count.to_be_bytes());
        out.extend_from_slice(&self.total.to_be_bytes());
        Secret::from_bytes(out)
    }

    /// `None` when `raw` is not a manifest at all (an inline secret). A value that carries the
    /// marker but is not a well-formed manifest cannot be written by this module (such a secret
    /// is always chunked), so it is reported as corrupt.
    fn parse(raw: &[u8]) -> Result<Option<Manifest>> {
        if raw.len() < MAGIC.len() || &raw[..MAGIC.len()] != MAGIC {
            return Ok(None);
        }
        let corrupt = || Error::Invalid {
            what: "chunked credential",
            reason: "the manifest is malformed".into(),
        };
        if raw.len() != MANIFEST_LEN {
            return Err(corrupt());
        }
        let mut g = [0u8; 8];
        g.copy_from_slice(&raw[8..16]);
        let mut c = [0u8; 4];
        c.copy_from_slice(&raw[16..20]);
        let mut t = [0u8; 8];
        t.copy_from_slice(&raw[20..28]);
        let m = Manifest {
            generation: u64::from_be_bytes(g),
            count: u32::from_be_bytes(c),
            total: u64::from_be_bytes(t),
        };
        let expected = m.total.div_ceil(INLINE_MAX as u64);
        if m.count == 0 || m.count > MAX_CHUNKS || u64::from(m.count) != expected {
            return Err(corrupt());
        }
        Ok(Some(m))
    }
}

fn chunk_account(account: &str, generation: u64, index: u32) -> String {
    format!("{account}.rkchunk.{generation:016x}.{index}")
}

/// [`Backend`] wrapper that stores a secret over [`INLINE_MAX`] bytes as chunks. See the module
/// docs. Every other behaviour (identity, durability, pinning, interaction) is the inner backend's.
pub struct ChunkedBackend {
    inner: Arc<dyn Backend>,
}

impl ChunkedBackend {
    pub fn new(inner: Arc<dyn Backend>) -> Self {
        Self { inner }
    }

    fn needs_chunks(secret: &Secret) -> bool {
        secret.len() > INLINE_MAX || secret.expose_bytes().starts_with(MAGIC)
    }

    /// Write the chunk items of a new generation. On any failure the chunks already written are
    /// removed (best effort) and the error is returned; the manifest has not been touched.
    fn write_chunks(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        mode: Interaction,
    ) -> Result<Manifest> {
        let mut rnd = [0u8; 8];
        getrandom::fill(&mut rnd)
            .map_err(|_| Error::Unavailable("the system random source is unavailable".into()))?;
        let bytes = secret.expose_bytes();
        let count = u32::try_from(bytes.len().div_ceil(INLINE_MAX).max(1))
            .ok()
            .filter(|c| *c <= MAX_CHUNKS)
            .ok_or_else(|| Error::Invalid {
                what: "secret",
                reason: "too large to store".into(),
            })?;
        let manifest = Manifest {
            generation: u64::from_be_bytes(rnd),
            count,
            total: bytes.len() as u64,
        };
        for (i, part) in bytes.chunks(INLINE_MAX).enumerate() {
            let name = chunk_account(account, manifest.generation, i as u32);
            let piece = Secret::from_bytes(part.to_vec());
            if let Err(e) = self.inner.set(ns, &name, &piece, mode) {
                self.remove_chunks(ns, account, &manifest, mode);
                return Err(e);
            }
        }
        Ok(manifest)
    }

    /// Best effort: an unreferenced chunk is never read, so a failure here loses nothing.
    fn remove_chunks(&self, ns: &Namespace, account: &str, m: &Manifest, mode: Interaction) {
        for i in 0..m.count {
            let _ = self
                .inner
                .delete(ns, &chunk_account(account, m.generation, i), mode);
        }
    }

    /// The logical value a manifest points at; a missing or short chunk is an error.
    fn assemble(
        &self,
        ns: &Namespace,
        account: &str,
        m: &Manifest,
        mode: Interaction,
    ) -> Result<Secret> {
        let missing = || Error::Invalid {
            what: "chunked credential",
            reason: "a chunk is missing or has the wrong length".into(),
        };
        let mut out: Zeroizing<Vec<u8>> = Zeroizing::new(Vec::with_capacity(m.total as usize));
        for i in 0..m.count {
            let part = self
                .inner
                .get(ns, &chunk_account(account, m.generation, i), mode)?
                .ok_or_else(missing)?;
            let last = i + 1 == m.count;
            let want = if last {
                (m.total - u64::from(i) * INLINE_MAX as u64) as usize
            } else {
                INLINE_MAX
            };
            if part.len() != want {
                return Err(missing());
            }
            out.extend_from_slice(part.expose_bytes());
        }
        Ok(Secret::from_bytes(out.to_vec()))
    }

    /// Read the item under `account` and resolve it to its logical value, keeping the raw item
    /// (needed to delete it only if it is unchanged).
    fn read_raw(
        &self,
        ns: &Namespace,
        account: &str,
        mode: Interaction,
    ) -> Result<Option<(Secret, Option<Manifest>)>> {
        match self.inner.get(ns, account, mode)? {
            None => Ok(None),
            Some(raw) => {
                let manifest = Manifest::parse(raw.expose_bytes())?;
                Ok(Some((raw, manifest)))
            }
        }
    }
}

impl Backend for ChunkedBackend {
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn exclusion_identity(&self) -> Option<String> {
        self.inner.exclusion_identity()
    }
    fn pinned(&self) -> Result<Option<Arc<dyn Backend>>> {
        Ok(self
            .inner
            .pinned()?
            .map(|b| Arc::new(ChunkedBackend::new(b)) as Arc<dyn Backend>))
    }
    fn credential_identity(&self, ns: &Namespace, account: &str) -> Result<String> {
        self.inner.credential_identity(ns, account)
    }
    fn durable(&self) -> bool {
        self.inner.durable()
    }
    fn get(&self, ns: &Namespace, account: &str, mode: Interaction) -> Result<Option<Secret>> {
        match self.read_raw(ns, account, mode)? {
            None => Ok(None),
            Some((raw, None)) => Ok(Some(raw)),
            Some((_, Some(m))) => self.assemble(ns, account, &m, mode).map(Some),
        }
    }
    fn exists(&self, ns: &Namespace, account: &str, mode: Interaction) -> Result<bool> {
        self.inner.exists(ns, account, mode)
    }
    fn set(&self, ns: &Namespace, account: &str, secret: &Secret, mode: Interaction) -> Result<()> {
        // The previous manifest, to remove its chunks after the new value is committed. An
        // unreadable previous item is not a reason to refuse the write: the new value replaces it.
        let previous = match self.inner.get(ns, account, mode) {
            Ok(Some(raw)) => Manifest::parse(raw.expose_bytes()).ok().flatten(),
            _ => None,
        };
        if Self::needs_chunks(secret) {
            let manifest = self.write_chunks(ns, account, secret, mode)?;
            if let Err(e) = self.inner.set(ns, account, &manifest.encode(), mode) {
                self.remove_chunks(ns, account, &manifest, mode);
                return Err(e);
            }
        } else {
            self.inner.set(ns, account, secret, mode)?;
        }
        if let Some(old) = previous {
            self.remove_chunks(ns, account, &old, mode);
        }
        Ok(())
    }
    fn add_if_absent(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        mode: Interaction,
    ) -> Result<bool> {
        if !Self::needs_chunks(secret) {
            return self.inner.add_if_absent(ns, account, secret, mode);
        }
        let manifest = self.write_chunks(ns, account, secret, mode)?;
        match self
            .inner
            .add_if_absent(ns, account, &manifest.encode(), mode)
        {
            Ok(true) => Ok(true),
            Ok(false) => {
                self.remove_chunks(ns, account, &manifest, mode);
                Ok(false)
            }
            Err(e) => {
                self.remove_chunks(ns, account, &manifest, mode);
                Err(e)
            }
        }
    }
    fn delete(&self, ns: &Namespace, account: &str, mode: Interaction) -> Result<bool> {
        let previous = match self.inner.get(ns, account, mode) {
            Ok(Some(raw)) => Manifest::parse(raw.expose_bytes()).ok().flatten(),
            _ => None,
        };
        let removed = self.inner.delete(ns, account, mode)?;
        if let Some(old) = previous {
            self.remove_chunks(ns, account, &old, mode);
        }
        Ok(removed)
    }
    fn delete_if_equals(
        &self,
        ns: &Namespace,
        account: &str,
        expected: &Secret,
        mode: Interaction,
    ) -> Result<ConditionalDelete> {
        match self.read_raw(ns, account, mode)? {
            None => Ok(ConditionalDelete::Absent),
            Some((_, None)) => self.inner.delete_if_equals(ns, account, expected, mode),
            Some((raw, Some(m))) => {
                let logical = self.assemble(ns, account, &m, mode)?;
                if !logical.ct_eq(expected) {
                    return Ok(ConditionalDelete::Changed);
                }
                // The raw manifest pins the generation: a writer that replaced the value in the
                // meantime changed the manifest, so the delete reports Changed and removes nothing.
                let outcome = self.inner.delete_if_equals(ns, account, &raw, mode)?;
                if outcome == ConditionalDelete::Deleted {
                    self.remove_chunks(ns, account, &m, mode);
                }
                Ok(outcome)
            }
        }
    }
}
