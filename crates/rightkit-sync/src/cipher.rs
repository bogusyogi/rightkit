//! End-to-end record cipher (feature `cipher`).
//!
//! [`RecordCipher`] seals a [`RawRecord`] so a storage server only ever holds ciphertext.
//! [`EncryptingBackend`] wraps any [`Backend`] with it, so the engine, the mapper and the
//! local store see plaintext and the provider sees only what ordering and compare-and-swap
//! need. The key comes from an injected [`KeyProvider`] (the key manager is another crate).
//!
//! # What stays in clear
//!
//! `record_name`, `record_type`, `change_tag`, `modified_ms`/`created_ms` (transport metadata),
//! and the two record fields `updatedAt` and `deletedAt` (when they are `Date` values). The
//! engine and the server need those for ordering, tombstone-wins and conditional saves. Every
//! other field, including `id`, `createdAt`, `text` and `tags`, moves into one sealed field,
//! [`ENC_FIELD`]. The server learns the record names, their types, versions, which records
//! are deleted, and sizes. It does not learn content.
//!
//! # Construction
//!
//! Per record: a random 16-byte salt; `HKDF-SHA256(zone key, salt, "rightkit-sync/cipher/1
//! record key+nonce")` yields 44 bytes, a one-use ChaCha20-Poly1305 key (32) and nonce (12).
//! The key is unique to the record write because the salt is, so the 96-bit nonce space is
//! never shared between messages (the same idea as XChaCha20's HChaCha subkey, built from
//! `ring` primitives that are already in the workspace lock). Sealed field `enc`:
//!
//! ```text
//! [format = 1][key version][salt: 16][ciphertext][tag: 16]
//! ```
//!
//! Associated data (length-prefixed, so fields cannot slide into each other): label, format,
//! key version, zone, record type, record name, `updatedAt` (present flag and value),
//! `deletedAt` (present flag and value). A ciphertext therefore cannot be moved to another
//! record name, type or zone, cannot be given another version, and a live record cannot be
//! turned into a tombstone (or the reverse) by the server.
//!
//! # What it does not protect
//!
//! - Rollback: a server can serve an older sealed version of a record (its AD matches its own
//!   older `updatedAt`). The engine's last-writer-wins ignores older versions it already
//!   holds, but a fresh device can be shown stale state.
//! - Withholding and deleting: a server can drop records or refuse to serve them.
//! - Sizes, counts, names, types and write times.
//! - A zone that already holds plaintext records: those fail to open (no `enc`) and are
//!   quarantined. Turn encryption on with a new zone.
//!
//! # Failure handling
//!
//! - Key unavailable (provider error, or a record sealed with a key version the provider does
//!   not have): the call fails with a retryable error and the change cursor does not advance,
//!   so nothing is skipped; the pass resumes from the same page once the key arrives.
//! - Tombstone without `enc` (a `deletedAt` date but no sealed field): quarantined like any
//!   other unauthenticated record, never passed through as a deletion. A server may drop the
//!   out-of-line body of an old tombstone when it compacts (`packages/sync-worker`) but must
//!   keep the sealed `enc`, so every honest tombstone still authenticates. A tombstone
//!   without `enc` therefore cannot come from an honest server; it could be a forged
//!   deletion, and no client acts on it. Every deletion a client applies is authenticated.
//!   Counted in [`CipherStats::unauthenticated_tombstones`].
//! - Corrupt ciphertext (authentication failure under a held key version, or broken framing,
//!   or a record without `enc` that is not a tombstone): the record is quarantined. It is returned as a shell with no
//!   fields (never a tombstone), so the mapper rejects it and the engine counts it and moves
//!   on. A mapper that accepts a record with no fields would defeat this; the shipped
//!   mappers require `id`.
//!   A wrong key under a right version number is indistinguishable from corruption. If keys
//!   were wrong and have since been fixed, call `SyncEngine::request_full_replay_once` to
//!   fetch the quarantined records again.
//!
//! # Where plaintext lands
//!
//! The engine keeps downloaded records it could not apply yet in the ledger's `pending_apply`,
//! in plaintext. The ledger file is owner-only on disk (`FileStorage`), but it is not
//! encrypted. Do not point `FileStorage` at a shared location.
//!
//! # Consent
//!
//! An app that syncs encrypted history should set
//! `SyncConfig::require_consent_on_scope_change` and make its `AuthProvider::account_scope`
//! include the key id, so a key change pauses for consent instead of publishing local history
//! into a zone sealed with other keys.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use ring::aead::{self, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::hkdf;
use ring::rand::{SecureRandom, SystemRandom};
use zeroize::{Zeroize, Zeroizing};

use crate::error::{SyncError, SyncResult};
use crate::record::{field, FieldValue, RawRecord};
use crate::traits::{Backend, ChangePage, RecordOutcome, SavePolicy, SyncObserver};

/// Name of the one field that carries the sealed payload.
pub const ENC_FIELD: &str = "enc";

const FORMAT: u8 = 1;
const SALT_LEN: usize = 16;
const TAG_LEN: usize = 16;
const HEADER_LEN: usize = 2 + SALT_LEN;
const KDF_INFO: &[u8] = b"rightkit-sync/cipher/1 record key+nonce";
const AD_LABEL: &[u8] = b"rightkit-sync/cipher/1";
const OKM_LEN: usize = 44;

/// A 32-byte zone data key. Zeroed on drop; `Debug` never prints it.
#[derive(Clone)]
pub struct CipherKey(Zeroizing<[u8; 32]>);

impl CipherKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    pub fn from_slice(bytes: &[u8]) -> SyncResult<Self> {
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|_| SyncError::Persistence("cipher key must be 32 bytes".into()))?;
        Ok(Self::from_bytes(arr))
    }

    fn bytes(&self) -> &[u8] {
        let inner: &[u8; 32] = &self.0;
        inner
    }
}

impl std::fmt::Debug for CipherKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CipherKey([redacted])")
    }
}

/// Source of zone keys. Implemented by the app's key manager.
pub trait KeyProvider: Send + Sync {
    /// Version byte and key for sealing new records. An error (keychain locked, not enrolled)
    /// fails the save and is retried by the engine.
    fn current(&self) -> SyncResult<(u8, CipherKey)>;
    /// Key for a version found on a downloaded record. `Ok(None)` = this device does not have
    /// that version (yet); the download then fails with `SyncError::Unavailable` and retries.
    fn key_for(&self, version: u8) -> SyncResult<Option<CipherKey>>;
}

/// Fixed keys: one current version plus any older versions kept for opening. For tests and
/// simple hosts.
#[derive(Clone, Debug)]
pub struct StaticKeys {
    current: u8,
    keys: BTreeMap<u8, CipherKey>,
}

impl StaticKeys {
    pub fn new(version: u8, key: CipherKey) -> Self {
        let mut keys = BTreeMap::new();
        keys.insert(version, key);
        Self {
            current: version,
            keys,
        }
    }

    /// Keep another version for opening records (not used for sealing).
    pub fn with_key(mut self, version: u8, key: CipherKey) -> Self {
        self.keys.insert(version, key);
        self
    }
}

impl KeyProvider for StaticKeys {
    fn current(&self) -> SyncResult<(u8, CipherKey)> {
        self.keys
            .get(&self.current)
            .map(|k| (self.current, k.clone()))
            .ok_or_else(|| SyncError::Unavailable("no current cipher key".into()))
    }

    fn key_for(&self, version: u8) -> SyncResult<Option<CipherKey>> {
        Ok(self.keys.get(&version).cloned())
    }
}

/// Why [`RecordCipher::open`] refused a record.
#[derive(Debug, Clone, PartialEq)]
pub enum OpenError {
    /// No sealed field, unknown format, or broken framing/payload.
    Malformed(&'static str),
    /// Sealed with a key version the provider does not have.
    UnknownKeyVersion(u8),
    /// Authentication failed: tampered, moved to another record/zone/version, or wrong key.
    Authentication,
    /// The key provider failed. Not a verdict on the record.
    Keys(SyncError),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(why) => write!(f, "malformed sealed record: {why}"),
            Self::UnknownKeyVersion(v) => write!(f, "key version {v} not available"),
            Self::Authentication => write!(f, "authentication failed"),
            Self::Keys(e) => write!(f, "key provider: {e}"),
        }
    }
}

impl std::error::Error for OpenError {}

fn crypto_failure() -> SyncError {
    SyncError::Persistence("record cipher failure".into())
}

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(&(b.len() as u64).to_be_bytes());
    out.extend_from_slice(b);
}

fn put_opt_i64(out: &mut Vec<u8>, v: Option<i64>) {
    match v {
        Some(n) => {
            out.push(1);
            out.extend_from_slice(&n.to_be_bytes());
        }
        None => out.push(0),
    }
}

struct Len(usize);
impl hkdf::KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

/// Seals and opens whole records for one zone.
pub struct RecordCipher {
    zone: String,
    keys: Arc<dyn KeyProvider>,
    rng: SystemRandom,
}

impl RecordCipher {
    pub fn new(zone: impl Into<String>, keys: Arc<dyn KeyProvider>) -> Self {
        Self {
            zone: zone.into(),
            keys,
            rng: SystemRandom::new(),
        }
    }

    fn associated_data(
        &self,
        version: u8,
        record_type: &str,
        record_name: &str,
        updated: Option<i64>,
        deleted: Option<i64>,
    ) -> Vec<u8> {
        let mut ad = Vec::new();
        put_bytes(&mut ad, AD_LABEL);
        ad.push(FORMAT);
        ad.push(version);
        put_bytes(&mut ad, self.zone.as_bytes());
        put_bytes(&mut ad, record_type.as_bytes());
        put_bytes(&mut ad, record_name.as_bytes());
        put_opt_i64(&mut ad, updated);
        put_opt_i64(&mut ad, deleted);
        ad
    }

    fn derive(key: &CipherKey, salt: &[u8]) -> SyncResult<Zeroizing<[u8; OKM_LEN]>> {
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(key.bytes());
        let info: [&[u8]; 1] = [KDF_INFO];
        let okm = prk
            .expand(&info, Len(OKM_LEN))
            .map_err(|_| crypto_failure())?;
        let mut out = Zeroizing::new([0u8; OKM_LEN]);
        okm.fill(&mut out[..]).map_err(|_| crypto_failure())?;
        Ok(out)
    }

    /// Seal `record` under the provider's current key.
    pub fn seal(&self, record: &RawRecord) -> SyncResult<RawRecord> {
        let mut salt = [0u8; SALT_LEN];
        self.rng
            .fill(&mut salt)
            .map_err(|_| SyncError::Unavailable("system random source failed".into()))?;
        self.seal_with_salt(record, salt)
    }

    /// [`RecordCipher::seal`] with a caller-chosen salt, for deterministic tests. Reusing a
    /// salt under one key reuses the record key and nonce: never do it outside tests.
    #[doc(hidden)]
    pub fn seal_with_salt(
        &self,
        record: &RawRecord,
        salt: [u8; SALT_LEN],
    ) -> SyncResult<RawRecord> {
        let (version, key) = self.keys.current()?;
        let mut clear: BTreeMap<String, FieldValue> = BTreeMap::new();
        let mut secret: Vec<(&String, &FieldValue)> = Vec::new();
        for (k, v) in &record.fields {
            match (k.as_str(), v) {
                (field::UPDATED_AT, FieldValue::Date(_))
                | (field::DELETED_AT, FieldValue::Date(_)) => {
                    clear.insert(k.clone(), v.clone());
                }
                _ => secret.push((k, v)),
            }
        }
        let updated = clear
            .get(field::UPDATED_AT)
            .and_then(FieldValue::as_date_ms);
        let deleted = clear
            .get(field::DELETED_AT)
            .and_then(FieldValue::as_date_ms);
        let ad = self.associated_data(
            version,
            &record.record_type,
            &record.record_name,
            updated,
            deleted,
        );
        let mut buf = encode_fields(&secret)?;
        buf.reserve_exact(TAG_LEN);
        let okm = Self::derive(&key, &salt)?;
        let sealing = LessSafeKey::new(
            UnboundKey::new(&aead::CHACHA20_POLY1305, &okm[..32]).map_err(|_| crypto_failure())?,
        );
        let nonce =
            Nonce::try_assume_unique_for_key(&okm[32..OKM_LEN]).map_err(|_| crypto_failure())?;
        sealing
            .seal_in_place_append_tag(nonce, Aad::from(&ad), &mut buf)
            .map_err(|_| crypto_failure())?;
        let mut envelope = Vec::with_capacity(HEADER_LEN + buf.len());
        envelope.push(FORMAT);
        envelope.push(version);
        envelope.extend_from_slice(&salt);
        envelope.extend_from_slice(&buf);
        clear.insert(ENC_FIELD.to_string(), FieldValue::Bytes(envelope));
        Ok(RawRecord {
            record_type: record.record_type.clone(),
            record_name: record.record_name.clone(),
            fields: clear,
            modified_ms: record.modified_ms,
            created_ms: record.created_ms,
            change_tag: record.change_tag.clone(),
        })
    }

    /// Open a sealed record. Only authenticated data is returned: fields the server added
    /// beside `enc` are dropped.
    pub fn open(&self, record: &RawRecord) -> Result<RawRecord, OpenError> {
        let Some(FieldValue::Bytes(envelope)) = record.fields.get(ENC_FIELD) else {
            return Err(OpenError::Malformed("no sealed payload"));
        };
        if envelope.len() < HEADER_LEN + TAG_LEN {
            return Err(OpenError::Malformed("sealed payload too short"));
        }
        if envelope[0] != FORMAT {
            return Err(OpenError::Malformed("unknown sealed format"));
        }
        let version = envelope[1];
        let salt = &envelope[2..HEADER_LEN];
        let updated = clear_date(record, field::UPDATED_AT)?;
        let deleted = clear_date(record, field::DELETED_AT)?;
        let key = self
            .keys
            .key_for(version)
            .map_err(OpenError::Keys)?
            .ok_or(OpenError::UnknownKeyVersion(version))?;
        let ad = self.associated_data(
            version,
            &record.record_type,
            &record.record_name,
            updated,
            deleted,
        );
        let okm = Self::derive(&key, salt).map_err(OpenError::Keys)?;
        let opening = LessSafeKey::new(
            UnboundKey::new(&aead::CHACHA20_POLY1305, &okm[..32])
                .map_err(|_| OpenError::Malformed("cipher setup"))?,
        );
        let nonce = Nonce::try_assume_unique_for_key(&okm[32..OKM_LEN])
            .map_err(|_| OpenError::Malformed("cipher setup"))?;
        let mut buf = envelope[HEADER_LEN..].to_vec();
        let decoded = match opening.open_in_place(nonce, Aad::from(&ad), &mut buf) {
            Ok(plain) => decode_fields(plain),
            Err(_) => {
                return Err(OpenError::Authentication);
            }
        };
        buf.zeroize();
        let mut fields = decoded.map_err(OpenError::Malformed)?;
        for (name, date) in [(field::UPDATED_AT, updated), (field::DELETED_AT, deleted)] {
            if let Some(ms) = date {
                if fields
                    .insert(name.to_string(), FieldValue::Date(ms))
                    .is_some()
                {
                    return Err(OpenError::Malformed("duplicate clear field"));
                }
            }
        }
        Ok(RawRecord {
            record_type: record.record_type.clone(),
            record_name: record.record_name.clone(),
            fields,
            modified_ms: record.modified_ms,
            created_ms: record.created_ms,
            change_tag: record.change_tag.clone(),
        })
    }
}

fn clear_date(record: &RawRecord, key: &str) -> Result<Option<i64>, OpenError> {
    match record.fields.get(key) {
        None => Ok(None),
        Some(FieldValue::Date(ms)) => Ok(Some(*ms)),
        Some(_) => Err(OpenError::Malformed("clear field is not a date")),
    }
}

const TAG_TEXT: u8 = 1;
const TAG_I64: u8 = 2;
const TAG_F64: u8 = 3;
const TAG_DATE: u8 = 4;
const TAG_BYTES: u8 = 5;
const TAG_STRINGS: u8 = 6;

fn put_len(out: &mut Vec<u8>, n: usize) -> SyncResult<()> {
    let n =
        u32::try_from(n).map_err(|_| SyncError::Persistence("field too large to seal".into()))?;
    out.extend_from_slice(&n.to_be_bytes());
    Ok(())
}

fn put_str(out: &mut Vec<u8>, s: &str) -> SyncResult<()> {
    put_len(out, s.len())?;
    out.extend_from_slice(s.as_bytes());
    Ok(())
}

/// Deterministic binary payload: `u32 count`, then per field (key order) `str key`, `u8 tag`,
/// value. Integers are big-endian; `f64` keeps its exact bits.
fn encode_fields(fields: &[(&String, &FieldValue)]) -> SyncResult<Vec<u8>> {
    let mut out = Vec::new();
    put_len(&mut out, fields.len())?;
    for (key, value) in fields {
        put_str(&mut out, key)?;
        match value {
            FieldValue::Text(s) => {
                out.push(TAG_TEXT);
                put_str(&mut out, s)?;
            }
            FieldValue::I64(n) => {
                out.push(TAG_I64);
                out.extend_from_slice(&n.to_be_bytes());
            }
            FieldValue::F64(n) => {
                out.push(TAG_F64);
                out.extend_from_slice(&n.to_bits().to_be_bytes());
            }
            FieldValue::Date(n) => {
                out.push(TAG_DATE);
                out.extend_from_slice(&n.to_be_bytes());
            }
            FieldValue::Bytes(b) => {
                out.push(TAG_BYTES);
                put_len(&mut out, b.len())?;
                out.extend_from_slice(b);
            }
            FieldValue::Strings(list) => {
                out.push(TAG_STRINGS);
                put_len(&mut out, list.len())?;
                for s in list {
                    put_str(&mut out, s)?;
                }
            }
        }
    }
    Ok(out)
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], &'static str> {
        let end = self.pos.checked_add(n).ok_or("length overflow")?;
        if end > self.buf.len() {
            return Err("truncated payload");
        }
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, &'static str> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, &'static str> {
        let b: [u8; 4] = self.take(4)?.try_into().map_err(|_| "truncated payload")?;
        Ok(u32::from_be_bytes(b))
    }
    fn i64(&mut self) -> Result<i64, &'static str> {
        Ok(i64::from_be_bytes(self.u64_bytes()?))
    }
    fn u64_bytes(&mut self) -> Result<[u8; 8], &'static str> {
        self.take(8)?.try_into().map_err(|_| "truncated payload")
    }
    fn string(&mut self) -> Result<String, &'static str> {
        let n = self.u32()? as usize;
        let b = self.take(n)?;
        String::from_utf8(b.to_vec()).map_err(|_| "invalid utf-8")
    }
}

fn decode_fields(buf: &[u8]) -> Result<BTreeMap<String, FieldValue>, &'static str> {
    let mut r = Reader { buf, pos: 0 };
    let count = r.u32()?;
    let mut fields = BTreeMap::new();
    for _ in 0..count {
        let key = r.string()?;
        let value = match r.u8()? {
            TAG_TEXT => FieldValue::Text(r.string()?),
            TAG_I64 => FieldValue::I64(r.i64()?),
            TAG_F64 => FieldValue::F64(f64::from_bits(u64::from_be_bytes(r.u64_bytes()?))),
            TAG_DATE => FieldValue::Date(r.i64()?),
            TAG_BYTES => {
                let n = r.u32()? as usize;
                FieldValue::Bytes(r.take(n)?.to_vec())
            }
            TAG_STRINGS => {
                let n = r.u32()?;
                let mut list = Vec::new();
                for _ in 0..n {
                    list.push(r.string()?);
                }
                FieldValue::Strings(list)
            }
            _ => return Err("unknown field tag"),
        };
        if fields.insert(key, value).is_some() {
            return Err("duplicate field");
        }
    }
    if r.pos != buf.len() {
        return Err("trailing bytes");
    }
    Ok(fields)
}

/// Counters of an [`EncryptingBackend`], from [`EncryptingBackend::stats`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CipherStats {
    pub sealed: u64,
    pub opened: u64,
    /// Records quarantined because the ciphertext failed authentication under a held key.
    pub authentication_failed: u64,
    /// Records quarantined for broken framing, or because they carried no sealed field.
    pub malformed: u64,
    /// Downloads that failed because a key or key version was not available.
    pub key_unavailable: u64,
    /// Tombstones (a `deletedAt` date) that carried no sealed field, quarantined because a
    /// deletion without `enc` cannot be authenticated. Not also counted in `malformed`.
    pub unauthenticated_tombstones: u64,
}

#[derive(Default)]
struct Counters {
    sealed: AtomicU64,
    opened: AtomicU64,
    authentication_failed: AtomicU64,
    malformed: AtomicU64,
    key_unavailable: AtomicU64,
    unauthenticated_tombstones: AtomicU64,
}

struct NullEvents;
impl SyncObserver for NullEvents {
    fn event(&self, _name: &'static str, _detail: &str) {}
}

/// A [`Backend`] that seals every record on the way out and opens it on the way in.
///
/// Wraps `Arc<dyn Backend>` so it composes with any provider. `max_batch` passes through.
/// Observer events carry record names and reasons, never content.
pub struct EncryptingBackend {
    inner: Arc<dyn Backend>,
    cipher: RecordCipher,
    counters: Counters,
    observer: Arc<dyn SyncObserver>,
}

impl EncryptingBackend {
    pub fn new(
        inner: Arc<dyn Backend>,
        zone: impl Into<String>,
        keys: Arc<dyn KeyProvider>,
    ) -> Self {
        Self {
            inner,
            cipher: RecordCipher::new(zone, keys),
            counters: Counters::default(),
            observer: Arc::new(NullEvents),
        }
    }

    pub fn observer(mut self, observer: Arc<dyn SyncObserver>) -> Self {
        self.observer = observer;
        self
    }

    pub fn stats(&self) -> CipherStats {
        let c = &self.counters;
        CipherStats {
            sealed: c.sealed.load(Ordering::Relaxed),
            opened: c.opened.load(Ordering::Relaxed),
            authentication_failed: c.authentication_failed.load(Ordering::Relaxed),
            malformed: c.malformed.load(Ordering::Relaxed),
            key_unavailable: c.key_unavailable.load(Ordering::Relaxed),
            unauthenticated_tombstones: c.unauthenticated_tombstones.load(Ordering::Relaxed),
        }
    }

    /// Open one record for the engine. Key problems fail the call (retryable, cursor stays);
    /// corrupt records become a field-less shell the mapper rejects.
    fn open_record(&self, record: RawRecord) -> SyncResult<RawRecord> {
        if is_unsealed_tombstone(&record) {
            self.counters
                .unauthenticated_tombstones
                .fetch_add(1, Ordering::Relaxed);
            self.observer
                .event("cipher_tombstone_unauthenticated", &record.record_name);
            return Ok(shell(record));
        }
        match self.cipher.open(&record) {
            Ok(opened) => {
                self.counters.opened.fetch_add(1, Ordering::Relaxed);
                Ok(opened)
            }
            Err(OpenError::Keys(e)) => {
                self.counters
                    .key_unavailable
                    .fetch_add(1, Ordering::Relaxed);
                self.observer
                    .event("cipher_key_unavailable", &record.record_name);
                Err(match e {
                    SyncError::Unavailable(_) | SyncError::RetryAfter { .. } => e,
                    other => SyncError::Unavailable(format!("cipher key: {other}")),
                })
            }
            Err(OpenError::UnknownKeyVersion(v)) => {
                self.counters
                    .key_unavailable
                    .fetch_add(1, Ordering::Relaxed);
                self.observer
                    .event("cipher_key_unavailable", &record.record_name);
                Err(SyncError::Unavailable(format!(
                    "records are sealed with key version {v}, which this device does not hold yet"
                )))
            }
            Err(OpenError::Authentication) => {
                self.counters
                    .authentication_failed
                    .fetch_add(1, Ordering::Relaxed);
                self.observer
                    .event("cipher_record_corrupt", &record.record_name);
                Ok(shell(record))
            }
            Err(OpenError::Malformed(why)) => {
                self.counters.malformed.fetch_add(1, Ordering::Relaxed);
                self.observer.event(
                    "cipher_record_malformed",
                    &format!("{}: {why}", record.record_name),
                );
                Ok(shell(record))
            }
        }
    }
}

/// A record with no sealed field and a `deletedAt` date: a deletion nothing authenticates.
/// It is quarantined, never applied.
fn is_unsealed_tombstone(record: &RawRecord) -> bool {
    !record.fields.contains_key(ENC_FIELD)
        && record
            .fields
            .get(field::DELETED_AT)
            .and_then(FieldValue::as_date_ms)
            .is_some()
}

/// A record the mapper must reject: identity and change tag only, no fields (so no
/// `deletedAt` and never a tombstone), no server dates.
fn shell(record: RawRecord) -> RawRecord {
    RawRecord {
        record_type: record.record_type,
        record_name: record.record_name,
        fields: BTreeMap::new(),
        modified_ms: None,
        created_ms: None,
        change_tag: record.change_tag,
    }
}

impl Backend for EncryptingBackend {
    fn prepare(&self) -> SyncResult<()> {
        self.inner.prepare()
    }

    fn fetch_changes(&self, since: Option<&[u8]>) -> SyncResult<ChangePage> {
        let ChangePage {
            records: raw,
            deleted_record_names,
            new_token,
            more_coming,
        } = self.inner.fetch_changes(since)?;
        let mut records = Vec::with_capacity(raw.len());
        for r in raw {
            records.push(self.open_record(r)?);
        }
        Ok(ChangePage {
            records,
            deleted_record_names,
            new_token,
            more_coming,
        })
    }

    fn fetch_record(&self, record_name: &str) -> SyncResult<Option<RawRecord>> {
        match self.inner.fetch_record(record_name)? {
            Some(r) => Ok(Some(self.open_record(r)?)),
            None => Ok(None),
        }
    }

    fn save_records(
        &self,
        records: &[RawRecord],
        policy: SavePolicy,
    ) -> SyncResult<Vec<RecordOutcome>> {
        let mut sealed = Vec::with_capacity(records.len());
        for r in records {
            sealed.push(self.cipher.seal(r)?);
        }
        self.counters
            .sealed
            .fetch_add(sealed.len() as u64, Ordering::Relaxed);
        let outcomes = self.inner.save_records(&sealed, policy)?;
        let mut out = Vec::with_capacity(outcomes.len());
        for o in outcomes {
            let server_record = match o.server_record {
                Some(r) => Some(self.open_record(r)?),
                None => None,
            };
            out.push(RecordOutcome {
                record_name: o.record_name,
                result: o.result,
                server_record,
            });
        }
        Ok(out)
    }

    fn max_batch(&self) -> usize {
        self.inner.max_batch()
    }
}
