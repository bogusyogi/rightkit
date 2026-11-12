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
//!
//! ## Keyring-compatible namespaces
//! In a [`WindowsTarget::KeyringCompat`](crate::WindowsTarget::KeyringCompat) namespace every
//! stored blob is UTF-16LE text, so every item this module writes there is text:
//!
//! * A text secret of at most [`KEYRING_INLINE_UNITS`] UTF-16 code units (the 2,560-byte blob
//!   limit, which is also the `keyring` crate's own limit) is stored inline, exactly as `keyring`
//!   and every earlier version store it.
//! * A longer one is cut on character boundaries into chunks of at most that many units, behind
//!   the ASCII manifest `RKCHUNK2 <generation> <count> <total bytes>`. No character is split and
//!   nothing is re-encoded.
//! * The binary manifest and its 1,200-byte chunks are still read there (a secret whose chunks and
//!   random generation happened to be text could be stored that way before), and a secret that is
//!   not text takes the binary form as before, which the Windows codec refuses with
//!   `Error::NotUtf8`.

use crate::backend::{Backend, ConditionalDelete, Interaction};
use crate::error::{Error, Result};
use crate::namespace::{BlobCodec, Namespace};
use crate::secret::Secret;
use std::sync::Arc;
use zeroize::Zeroizing;

/// The most bytes one item holds outside a keyring-compatible namespace (see
/// [`KEYRING_INLINE_UNITS`] for those), and the size of a binary chunk. Windows allows 2,560
/// bytes of blob.
pub const INLINE_MAX: usize = 1_200;

/// The most UTF-16 code units one item holds in a keyring-compatible namespace: the 2,560-byte
/// Windows blob limit at two bytes per unit. It is the limit of the `keyring` crate as well, so a
/// text secret up to this size stays one item that `keyring` reads.
pub const KEYRING_INLINE_UNITS: usize = 1_280;

const MAGIC: &[u8; 8] = b"RKCHUNK1";
/// Marker of the text manifest (keyring-compatible namespaces only).
const TEXT_MAGIC: &str = "RKCHUNK2";
/// Magic, generation (u64), chunk count (u32), total length (u64).
const MANIFEST_LEN: usize = 8 + 8 + 4 + 8;
/// A secret may need at most this many chunks (1,200 bytes each: a 4.8 MB secret). Bounded so a
/// corrupt manifest cannot make a reader loop over billions of accounts.
const MAX_CHUNKS: u32 = 4_096;

/// How a chunked value is laid out.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Form {
    /// Binary manifest, chunks of exactly [`INLINE_MAX`] bytes (the last one shorter).
    Binary,
    /// ASCII manifest, text chunks cut on character boundaries at [`KEYRING_INLINE_UNITS`].
    Text,
}

struct Manifest {
    form: Form,
    generation: u64,
    count: u32,
    /// Length of the logical value in bytes.
    total: u64,
}

fn corrupt_manifest() -> Error {
    Error::Invalid {
        what: "chunked credential",
        reason: "the manifest is malformed".into(),
    }
}

impl Manifest {
    fn encode(&self) -> Secret {
        match self.form {
            Form::Binary => {
                let mut out = Vec::with_capacity(MANIFEST_LEN);
                out.extend_from_slice(MAGIC);
                out.extend_from_slice(&self.generation.to_be_bytes());
                out.extend_from_slice(&self.count.to_be_bytes());
                out.extend_from_slice(&self.total.to_be_bytes());
                Secret::from_bytes(out)
            }
            Form::Text => Secret::from_string(format!(
                "{TEXT_MAGIC} {:016x} {} {}",
                self.generation, self.count, self.total
            )),
        }
    }

    /// `None` when `raw` is not a manifest at all (an inline secret). A value that carries a
    /// marker but is not a well-formed manifest cannot be written by this module (such a secret
    /// is always chunked), so it is reported as corrupt. The text manifest exists only in a
    /// keyring-compatible namespace; everywhere else a value that begins with its marker is an
    /// ordinary inline secret.
    fn parse(ns: &Namespace, raw: &[u8]) -> Result<Option<Manifest>> {
        if raw.starts_with(MAGIC) {
            return Self::parse_binary(raw).map(Some);
        }
        if ns.blob_codec() == BlobCodec::KeyringUtf16 && raw.starts_with(TEXT_MAGIC.as_bytes()) {
            return Self::parse_text(raw).map(Some);
        }
        Ok(None)
    }

    fn parse_binary(raw: &[u8]) -> Result<Manifest> {
        if raw.len() != MANIFEST_LEN {
            return Err(corrupt_manifest());
        }
        let mut g = [0u8; 8];
        g.copy_from_slice(&raw[8..16]);
        let mut c = [0u8; 4];
        c.copy_from_slice(&raw[16..20]);
        let mut t = [0u8; 8];
        t.copy_from_slice(&raw[20..28]);
        let m = Manifest {
            form: Form::Binary,
            generation: u64::from_be_bytes(g),
            count: u32::from_be_bytes(c),
            total: u64::from_be_bytes(t),
        };
        let expected = m.total.div_ceil(INLINE_MAX as u64);
        if m.count == 0 || m.count > MAX_CHUNKS || u64::from(m.count) != expected {
            return Err(corrupt_manifest());
        }
        Ok(m)
    }

    fn parse_text(raw: &[u8]) -> Result<Manifest> {
        let text = std::str::from_utf8(raw).map_err(|_| corrupt_manifest())?;
        let mut fields = text.split(' ');
        let (Some(TEXT_MAGIC), Some(generation), Some(count), Some(total), None) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            return Err(corrupt_manifest());
        };
        let m = Manifest {
            form: Form::Text,
            generation: u64::from_str_radix(generation, 16).map_err(|_| corrupt_manifest())?,
            count: count.parse().map_err(|_| corrupt_manifest())?,
            total: total.parse().map_err(|_| corrupt_manifest())?,
        };
        // Only the exact text `encode` writes is a manifest (no sign, no padding, lowercase hex).
        if m.encode().expose_bytes() != raw {
            return Err(corrupt_manifest());
        }
        // Every chunk but the last holds KEYRING_INLINE_UNITS units, or one fewer when the next
        // character needs two; a unit is one to three bytes of UTF-8.
        let units = KEYRING_INLINE_UNITS as u64;
        let earlier = u64::from(m.count.saturating_sub(1));
        if m.count == 0
            || m.count > MAX_CHUNKS
            || m.total <= earlier * (units - 1)
            || m.total > u64::from(m.count) * units * 3
        {
            return Err(corrupt_manifest());
        }
        Ok(m)
    }
}

/// `text` cut on character boundaries into pieces of at most [`KEYRING_INLINE_UNITS`] UTF-16
/// code units each, so every piece is text the keyring-compatible codec stores in one blob.
fn split_text(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut start, mut units) = (0usize, 0usize);
    for (at, c) in text.char_indices() {
        let n = c.len_utf16();
        if units + n > KEYRING_INLINE_UNITS {
            parts.push(&text[start..at]);
            start = at;
            units = 0;
        }
        units += n;
    }
    if start < text.len() {
        parts.push(&text[start..]);
    }
    parts
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

    /// How `secret` is stored in `ns`: `None` is inline (one item, the secret itself).
    fn layout(ns: &Namespace, secret: &Secret) -> Option<Form> {
        let bytes = secret.expose_bytes();
        let keyring = ns.blob_codec() == BlobCodec::KeyringUtf16;
        // A secret that begins with a marker a reader of this namespace would take for a
        // manifest is always chunked.
        let marked =
            bytes.starts_with(MAGIC) || (keyring && bytes.starts_with(TEXT_MAGIC.as_bytes()));
        if keyring {
            if let Ok(text) = std::str::from_utf8(bytes) {
                let fits = text.encode_utf16().count() <= KEYRING_INLINE_UNITS;
                return (!fits || marked).then_some(Form::Text);
            }
        }
        (bytes.len() > INLINE_MAX || marked).then_some(Form::Binary)
    }

    /// Write the chunk items of a new generation. On any failure the chunks already written are
    /// removed (best effort) and the error is returned; the manifest has not been touched.
    fn write_chunks(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        form: Form,
        mode: Interaction,
    ) -> Result<Manifest> {
        let mut rnd = [0u8; 8];
        getrandom::fill(&mut rnd)
            .map_err(|_| Error::Unavailable("the system random source is unavailable".into()))?;
        let bytes = secret.expose_bytes();
        let parts: Vec<&[u8]> = match form {
            Form::Binary => bytes.chunks(INLINE_MAX).collect(),
            Form::Text => split_text(secret.expose_str()?)
                .into_iter()
                .map(str::as_bytes)
                .collect(),
        };
        let count = u32::try_from(parts.len())
            .ok()
            .filter(|c| (1..=MAX_CHUNKS).contains(c))
            .ok_or_else(|| Error::Invalid {
                what: "secret",
                reason: "too large to store".into(),
            })?;
        let manifest = Manifest {
            form,
            generation: u64::from_be_bytes(rnd),
            count,
            total: bytes.len() as u64,
        };
        for (i, part) in parts.into_iter().enumerate() {
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
        // Text form: whether the previous chunk stopped one unit short of the limit, which only
        // happens when this chunk begins with a character of two units.
        let mut short_before = false;
        for i in 0..m.count {
            let part = self
                .inner
                .get(ns, &chunk_account(account, m.generation, i), mode)?
                .ok_or_else(missing)?;
            let last = i + 1 == m.count;
            match m.form {
                Form::Binary => {
                    let want = if last {
                        (m.total - u64::from(i) * INLINE_MAX as u64) as usize
                    } else {
                        INLINE_MAX
                    };
                    if part.len() != want {
                        return Err(missing());
                    }
                }
                Form::Text => {
                    let text = part.expose_str().map_err(|_| missing())?;
                    let units = text.encode_utf16().count();
                    let wide_first = text.chars().next().is_some_and(|c| c.len_utf16() == 2);
                    let short = units + 1 == KEYRING_INLINE_UNITS;
                    let length_ok = if last {
                        (1..=KEYRING_INLINE_UNITS).contains(&units)
                    } else {
                        short || units == KEYRING_INLINE_UNITS
                    };
                    if !length_ok || (short_before && !wide_first) {
                        return Err(missing());
                    }
                    short_before = !last && short;
                }
            }
            out.extend_from_slice(part.expose_bytes());
        }
        if out.len() as u64 != m.total {
            return Err(missing());
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
                let manifest = Manifest::parse(ns, raw.expose_bytes())?;
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
            Ok(Some(raw)) => Manifest::parse(ns, raw.expose_bytes()).ok().flatten(),
            _ => None,
        };
        if let Some(form) = Self::layout(ns, secret) {
            let manifest = self.write_chunks(ns, account, secret, form, mode)?;
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
        let Some(form) = Self::layout(ns, secret) else {
            return self.inner.add_if_absent(ns, account, secret, mode);
        };
        let manifest = self.write_chunks(ns, account, secret, form, mode)?;
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
            Ok(Some(raw)) => Manifest::parse(ns, raw.expose_bytes()).ok().flatten(),
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
