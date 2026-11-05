//! Conditional-object vault sync (feature `vault`, rightkit#966).
//!
//! A password vault cannot use the record engine: last-writer-wins would silently drop a secret
//! edited on two devices, and the engine keeps plaintext in the ledger's `pending_apply`. This
//! module is separate from the engine. It syncs one whole vault through any object store that
//! documents atomic conditional writes (Cloudflare R2 and S3-compatible stores document
//! `If-Match` and `If-None-Match` on `PutObject` with strong consistency), and never through one
//! that does not (Google Drive is excluded, see the `drive` module docs).
//!
//! # Layout
//!
//! * **State objects** `<vault>/states/<seq>-<random>`: the whole vault, sealed, uploaded
//!   create-only (`If-None-Match: *`). Immutable once written.
//! * **Head** `<vault>/head`: one sealed object naming the current state object and its sequence
//!   number. It is the only object ever overwritten, and only with `If-Match: <etag it read>`
//!   (`If-None-Match: *` on the first write). Two writers racing on one head cannot both win: the
//!   store refuses the loser, which re-reads, merges and tries again.
//! * **Ledger** (device-local, through [`LedgerStorage`]): the head sequence and state key last
//!   synced and the vault as it was then, which is the *base* of the three-way merge. It is
//!   sealed with the vault key, so unlike the record engine's ledger it holds no plaintext.
//!
//! Everything the store holds is sealed with [`crate::cipher::RecordCipher`] (ChaCha20-Poly1305,
//! HKDF-SHA256) with the vault id as the zone and the object key as the record name, so an object
//! cannot be moved to another key or another vault. The store learns the vault id, object keys,
//! sequence numbers, sizes and write times. It does not learn item names or values.
//!
//! # The write protocol ([`Vault::sync`])
//!
//! 1. Read the head and its ETag. Verify it opens, and that its sequence is not below the one this
//!    device already saw (rollback) and names the same state when it is equal (fork).
//! 2. Remote unchanged and local unchanged: done. Remote changed and local unchanged: adopt the
//!    remote, write nothing. Otherwise compute the merged vault with the three-way merge
//!    ([`MergePolicy`]) of base, local and remote, upload it as a new state object (create-only),
//!    then write a head naming it with `If-Match`.
//! 3. A refused head write (`412`/precondition failed) means another writer won: start again from
//!    step 1. After an **uncertain** write (a transport error or timeout after the request may
//!    have been applied) the head is read back: if it names our state object the write landed,
//!    otherwise it did not, and the loop starts again. Retries are bounded
//!    ([`VaultConfig::max_attempts`]); after that the call returns a transient error and
//!    changes nothing that matters (an unreferenced state object may remain).
//! 4. Save the ledger. A crash between the head write and the ledger save is harmless: the next
//!    sync sees a remote that equals the local result, and the merge of identical changes is that
//!    result.
//!
//! No sleeping or thread: like [`crate::sync_once`], the call returns and the host schedules the
//! next one.
//!
//! # The merge
//!
//! Per item name, with base `b`, local `o` and remote `t` (each present or deleted): equal sides
//! take either; one side unchanged takes the other, so an edit never loses to an untouched copy and
//! a deletion wins over an untouched copy; both changed differently asks the [`MergePolicy`], which
//! decides what "the same secret edited twice" means. [`KeepBoth`] is the conflict-preserving
//! default: local keeps the name and the remote value is kept beside it under
//! `<name>.conflict-<8 hex>`; an edit against a deletion keeps the edit. Nothing is lost.
//!
//! # What it does not protect
//!
//! A store can withhold or delete objects (a missing head after one was seen is an error, never a
//! fresh vault), serve an older head to a device that never saw a newer one, and see sizes and
//! timing. State objects that lost a race stay in the bucket unreferenced; expire them with a
//! bucket lifecycle rule (they are sealed and unreachable).
//!
//! # Provider contract
//!
//! A [`ConditionalStore`] must make `put_if_absent` and `put_if_match` atomic with respect to every
//! other writer of the same key, return a different ETag after every successful write of a key,
//! and give a read that follows a successful write of that key the written value (strong
//! consistency). R2 and S3 document all three; `s3::S3ConditionalStore` (feature `vault-s3`) speaks
//! to both. A store that cannot promise them must not be used.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use ring::digest;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::cipher::{KeyProvider, OpenError, RecordCipher, ENC_FIELD};
use crate::error::{SyncError, SyncResult};
use crate::record::{FieldValue, RawRecord};
use crate::storage::LedgerStorage;
use crate::wire::{b64_decode, b64_encode};

#[cfg(feature = "vault-s3")]
pub mod s3;

const KIND_STATE: &str = "vault.state";
const KIND_HEAD: &str = "vault.head";
const KIND_LEDGER: &str = "vault.ledger";
const LEDGER_NAME: &str = "ledger";
const FIELD_PAYLOAD: &str = "payload";

/// An object and the ETag the store gave its current version.
#[derive(Clone, PartialEq, Eq)]
pub struct StoredObject {
    pub body: Vec<u8>,
    pub etag: String,
}

impl fmt::Debug for StoredObject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredObject")
            .field("bytes", &self.body.len())
            .field("etag", &self.etag)
            .finish()
    }
}

/// The result of a conditional write that reached the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PutResult {
    /// The write was applied; `etag` is the new version.
    Stored { etag: String },
    /// The condition did not hold: the object exists (`put_if_absent`) or is not at the expected
    /// version (`put_if_match`, including an object that is gone). Nothing was written.
    PreconditionFailed,
}

/// An object store with atomic conditional writes. See the module docs for the provider contract.
///
/// An `Err` from a write means the outcome is **unknown** when it is [`SyncError::Transient`] or
/// [`SyncError::RetryAfter`] (the request may have been applied), and a refusal otherwise. The
/// vault reads back after an unknown outcome.
pub trait ConditionalStore: Send + Sync {
    /// The object and its ETag, or `None` when there is no such key.
    fn get(&self, key: &str) -> SyncResult<Option<StoredObject>>;
    /// Create `key` only if it does not exist (`If-None-Match: *`).
    fn put_if_absent(&self, key: &str, body: &[u8]) -> SyncResult<PutResult>;
    /// Replace `key` only if its ETag is still `etag` (`If-Match`).
    fn put_if_match(&self, key: &str, body: &[u8], etag: &str) -> SyncResult<PutResult>;
}

/// The items of a vault: names to opaque values. The consumer decides what a value is. Values are
/// zeroed when the state is dropped, and `Debug` shows names only.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct VaultState {
    items: BTreeMap<String, Vec<u8>>,
}

impl fmt::Debug for VaultState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VaultState")
            .field("items", &self.items.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Drop for VaultState {
    fn drop(&mut self) {
        for value in self.items.values_mut() {
            value.zeroize();
        }
    }
}

impl VaultState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.items.get(name).map(Vec::as_slice)
    }

    /// Set `name`; returns whether it replaced a value.
    pub fn insert(&mut self, name: impl Into<String>, value: impl Into<Vec<u8>>) -> bool {
        self.items.insert(name.into(), value.into()).is_some()
    }

    /// Remove `name`; returns whether it was present.
    pub fn remove(&mut self, name: &str) -> bool {
        match self.items.remove_entry(name) {
            Some((_, mut value)) => {
                value.zeroize();
                true
            }
            None => false,
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.items.iter().map(|(k, v)| (k.as_str(), v.as_slice()))
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// `u32` count, then per item (name order) `u32` name length, name, `u32` value length, value.
    fn encode(&self) -> SyncResult<Vec<u8>> {
        let mut out = Vec::new();
        put_len(&mut out, self.items.len())?;
        for (name, value) in &self.items {
            put_len(&mut out, name.len())?;
            out.extend_from_slice(name.as_bytes());
            put_len(&mut out, value.len())?;
            out.extend_from_slice(value);
        }
        Ok(out)
    }

    fn decode(buf: &[u8]) -> Result<Self, &'static str> {
        let mut reader = Reader { buf, pos: 0 };
        let count = reader.u32()?;
        let mut items = BTreeMap::new();
        for _ in 0..count {
            let name_len = reader.u32()? as usize;
            let name = String::from_utf8(reader.take(name_len)?.to_vec())
                .map_err(|_| "vault item name is not utf-8")?;
            let value_len = reader.u32()? as usize;
            let value = reader.take(value_len)?.to_vec();
            if items.insert(name, value).is_some() {
                return Err("duplicate vault item");
            }
        }
        if reader.pos != buf.len() {
            return Err("trailing bytes in vault state");
        }
        Ok(Self { items })
    }
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], &'static str> {
        let end = self.pos.checked_add(n).ok_or("length overflow")?;
        let part = self.buf.get(self.pos..end).ok_or("truncated vault state")?;
        self.pos = end;
        Ok(part)
    }

    fn u32(&mut self) -> Result<u32, &'static str> {
        let bytes: [u8; 4] = self
            .take(4)?
            .try_into()
            .map_err(|_| "truncated vault state")?;
        Ok(u32::from_be_bytes(bytes))
    }
}

fn put_len(out: &mut Vec<u8>, n: usize) -> SyncResult<()> {
    let n = u32::try_from(n).map_err(|_| SyncError::Persistence("vault item too large".into()))?;
    out.extend_from_slice(&n.to_be_bytes());
    Ok(())
}

/// What a [`MergePolicy`] decides for one item that both sides changed differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Keep the local value (or its deletion).
    Ours,
    /// Keep the remote value (or its deletion).
    Theirs,
    /// A value the policy merged itself.
    Value(Vec<u8>),
    /// The item is gone.
    Delete,
    /// Keep the local value under the name and the remote one under a conflict name
    /// (`<name>.conflict-<8 hex>`), so neither is lost. When one side deleted the item, the
    /// other side's value is kept under the name and there is nothing to duplicate.
    KeepBoth,
}

/// What "the same secret edited twice" means, decided by the consumer. Called only for an item
/// both sides changed differently from the base; `None` is "absent" (not there, or deleted).
pub trait MergePolicy: Send + Sync {
    fn resolve(
        &self,
        name: &str,
        base: Option<&[u8]>,
        ours: Option<&[u8]>,
        theirs: Option<&[u8]>,
    ) -> Resolution;
}

impl<F> MergePolicy for F
where
    F: Fn(&str, Option<&[u8]>, Option<&[u8]>, Option<&[u8]>) -> Resolution + Send + Sync,
{
    fn resolve(
        &self,
        name: &str,
        base: Option<&[u8]>,
        ours: Option<&[u8]>,
        theirs: Option<&[u8]>,
    ) -> Resolution {
        self(name, base, ours, theirs)
    }
}

/// The conflict-preserving default: [`Resolution::KeepBoth`] for every conflict.
#[derive(Debug, Clone, Copy, Default)]
pub struct KeepBoth;

impl MergePolicy for KeepBoth {
    fn resolve(&self, _: &str, _: Option<&[u8]>, _: Option<&[u8]>, _: Option<&[u8]>) -> Resolution {
        Resolution::KeepBoth
    }
}

/// An item both sides changed, and what became of the remote value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultConflict {
    pub name: String,
    /// The name the remote value was kept under, when the policy kept both.
    pub theirs_kept_as: Option<String>,
}

fn short_hex(bytes: &[u8]) -> String {
    let hash = digest::digest(&digest::SHA256, bytes);
    hex(&hash.as_ref()[..4])
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[usize::from(byte >> 4)] as char);
        out.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    out
}

/// The three-way merge. Pure: no I/O.
fn merge(
    base: &VaultState,
    ours: &VaultState,
    theirs: &VaultState,
    policy: &dyn MergePolicy,
) -> (VaultState, Vec<VaultConflict>) {
    let names: BTreeSet<&String> = base
        .items
        .keys()
        .chain(ours.items.keys())
        .chain(theirs.items.keys())
        .collect();
    let mut merged = VaultState::new();
    let mut conflicts = Vec::new();
    let mut extra: Vec<(String, Vec<u8>)> = Vec::new();
    for name in names {
        let b = base.get(name);
        let o = ours.get(name);
        let t = theirs.get(name);
        let chosen: Option<Vec<u8>> = if o == t || t == b {
            o.map(<[u8]>::to_vec)
        } else if o == b {
            t.map(<[u8]>::to_vec)
        } else {
            match policy.resolve(name, b, o, t) {
                Resolution::Ours => {
                    conflicts.push(VaultConflict {
                        name: name.clone(),
                        theirs_kept_as: None,
                    });
                    o.map(<[u8]>::to_vec)
                }
                Resolution::Theirs => {
                    conflicts.push(VaultConflict {
                        name: name.clone(),
                        theirs_kept_as: None,
                    });
                    t.map(<[u8]>::to_vec)
                }
                Resolution::Value(value) => {
                    conflicts.push(VaultConflict {
                        name: name.clone(),
                        theirs_kept_as: None,
                    });
                    Some(value)
                }
                Resolution::Delete => {
                    conflicts.push(VaultConflict {
                        name: name.clone(),
                        theirs_kept_as: None,
                    });
                    None
                }
                Resolution::KeepBoth => match (o, t) {
                    (Some(local), Some(remote)) => {
                        let kept_as = format!("{name}.conflict-{}", short_hex(remote));
                        extra.push((kept_as.clone(), remote.to_vec()));
                        conflicts.push(VaultConflict {
                            name: name.clone(),
                            theirs_kept_as: Some(kept_as),
                        });
                        Some(local.to_vec())
                    }
                    // One side deleted: the edit survives, nothing to duplicate.
                    (Some(value), None) | (None, Some(value)) => {
                        conflicts.push(VaultConflict {
                            name: name.clone(),
                            theirs_kept_as: None,
                        });
                        Some(value.to_vec())
                    }
                    (None, None) => None,
                },
            }
        };
        if let Some(value) = chosen {
            merged.items.insert(name.clone(), value);
        }
    }
    // Conflict copies go in after every real name is placed, so one never displaces a real item;
    // a name that is already taken gets a counter.
    for (name, value) in extra {
        let mut candidate = name.clone();
        let mut counter = 2u32;
        while merged.items.contains_key(&candidate) {
            if merged.items.get(&candidate).map(Vec::as_slice) == Some(value.as_slice()) {
                break;
            }
            candidate = format!("{name}-{counter}");
            counter += 1;
        }
        merged.items.insert(candidate, value);
    }
    (merged, conflicts)
}

/// Tuning of [`Vault`].
#[derive(Debug, Clone)]
pub struct VaultConfig {
    /// Most read-merge-write rounds of one [`Vault::sync`] before it gives up with a transient
    /// error. Default 8.
    pub max_attempts: u32,
    /// Largest sealed state accepted, either way. Default 8 MiB.
    pub max_state_bytes: usize,
}

impl Default for VaultConfig {
    fn default() -> Self {
        Self {
            max_attempts: 8,
            max_state_bytes: 8 * 1024 * 1024,
        }
    }
}

/// How a [`Vault::sync`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultOutcome {
    /// Nothing differed; nothing was written.
    Unchanged,
    /// Local changes were published over an unchanged remote.
    Pushed,
    /// The remote changed and local did not (or local changes equalled the remote): adopt it.
    Pulled,
    /// Both changed: the merged vault was published.
    Merged,
}

/// The result of one [`Vault::sync`]. Replace the local vault with `state`.
#[derive(Debug)]
pub struct VaultSync {
    pub state: VaultState,
    pub outcome: VaultOutcome,
    /// Items both sides changed, in name order.
    pub conflicts: Vec<VaultConflict>,
    /// Read-merge-write rounds it took (1 when nobody raced).
    pub attempts: u32,
}

#[derive(Serialize, Deserialize)]
struct Head {
    seq: u64,
    state: String,
    prev: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct LedgerRecord {
    seq: u64,
    state_key: String,
    base: String,
}

struct Ledger {
    seq: u64,
    state_key: String,
    base: VaultState,
}

enum Attempt {
    Done(VaultSync),
    Lost,
}

/// One vault in one conditional-object store. See the module docs.
pub struct Vault {
    store: Arc<dyn ConditionalStore>,
    vault_id: String,
    cipher: RecordCipher,
    ledger: Arc<dyn LedgerStorage>,
    config: VaultConfig,
    rng: SystemRandom,
    /// One sync at a time per handle.
    serial: Mutex<()>,
}

fn valid_vault_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn is_unknown_outcome(error: &SyncError) -> bool {
    matches!(
        error,
        SyncError::Transient(_) | SyncError::RetryAfter { .. }
    )
}

impl Vault {
    /// `vault_id` is `[A-Za-z0-9._-]{1,64}` and is part of every object key and of the sealing.
    /// `keys` supplies the vault key (the key id travels in each sealed object, so rotating keeps
    /// old objects readable while the provider still holds the old version). `ledger` is the
    /// device-local base record.
    pub fn new(
        store: Arc<dyn ConditionalStore>,
        vault_id: &str,
        keys: Arc<dyn KeyProvider>,
        ledger: Arc<dyn LedgerStorage>,
        config: VaultConfig,
    ) -> SyncResult<Self> {
        if !valid_vault_id(vault_id) {
            return Err(SyncError::Persistence(
                "vault id must be 1 to 64 of A-Z a-z 0-9 . _ -".into(),
            ));
        }
        if config.max_attempts == 0 {
            return Err(SyncError::Persistence(
                "vault max_attempts must be at least 1".into(),
            ));
        }
        Ok(Self {
            store,
            vault_id: vault_id.to_string(),
            cipher: RecordCipher::new(vault_id, keys),
            ledger,
            config,
            rng: SystemRandom::new(),
            serial: Mutex::new(()),
        })
    }

    fn head_key(&self) -> String {
        format!("{}/head", self.vault_id)
    }

    fn states_prefix(&self) -> String {
        format!("{}/states/", self.vault_id)
    }

    fn seal(&self, kind: &str, name: &str, mut payload: Vec<u8>) -> SyncResult<Vec<u8>> {
        let mut record = RawRecord::new(kind, name);
        record.fields.insert(
            FIELD_PAYLOAD.to_string(),
            FieldValue::Bytes(payload.clone()),
        );
        payload.zeroize();
        let sealed = self.cipher.seal(&record)?;
        match sealed.fields.get(ENC_FIELD) {
            Some(FieldValue::Bytes(body)) => Ok(body.clone()),
            _ => Err(SyncError::Persistence(
                "vault seal produced no payload".into(),
            )),
        }
    }

    fn open(&self, kind: &str, name: &str, body: &[u8]) -> SyncResult<Vec<u8>> {
        if body.len() > self.config.max_state_bytes.saturating_add(1024) {
            return Err(SyncError::Unavailable("vault object is too large".into()));
        }
        let mut record = RawRecord::new(kind, name);
        record
            .fields
            .insert(ENC_FIELD.to_string(), FieldValue::Bytes(body.to_vec()));
        match self.cipher.open(&record) {
            Ok(opened) => match opened.fields.get(FIELD_PAYLOAD) {
                Some(FieldValue::Bytes(payload)) => Ok(payload.clone()),
                _ => Err(SyncError::Unavailable("vault object has no payload".into())),
            },
            Err(OpenError::Keys(error)) => Err(error),
            Err(OpenError::UnknownKeyVersion(version)) => Err(SyncError::Unavailable(format!(
                "vault key version {version} is not available on this device"
            ))),
            Err(OpenError::Authentication) => Err(SyncError::Unavailable(
                "vault object failed authentication (tampered, moved, or the wrong key)".into(),
            )),
            Err(OpenError::Malformed(why)) => Err(SyncError::Unavailable(format!(
                "vault object is malformed: {why}"
            ))),
        }
    }

    fn open_head(&self, body: &[u8]) -> SyncResult<Head> {
        let payload = self.open(KIND_HEAD, &self.head_key(), body)?;
        let head: Head = serde_json::from_slice(&payload)
            .map_err(|_| SyncError::Unavailable("vault head is malformed".into()))?;
        if !head.state.starts_with(&self.states_prefix()) {
            return Err(SyncError::Unavailable(
                "vault head names an object outside this vault".into(),
            ));
        }
        Ok(head)
    }

    fn load_ledger(&self) -> SyncResult<Option<Ledger>> {
        let Some(bytes) = self.ledger.load().map_err(SyncError::Persistence)? else {
            return Ok(None);
        };
        let payload = self.open(KIND_LEDGER, LEDGER_NAME, &bytes)?;
        let record: LedgerRecord = serde_json::from_slice(&payload)
            .map_err(|_| SyncError::Persistence("vault ledger is malformed".into()))?;
        let base_bytes = b64_decode(&record.base)
            .map_err(|_| SyncError::Persistence("vault ledger base is malformed".into()))?;
        let base = VaultState::decode(&base_bytes)
            .map_err(|why| SyncError::Persistence(format!("vault ledger base: {why}")))?;
        Ok(Some(Ledger {
            seq: record.seq,
            state_key: record.state_key,
            base,
        }))
    }

    fn save_ledger(&self, seq: u64, state_key: &str, base: &VaultState) -> SyncResult<()> {
        let mut encoded = base.encode()?;
        let record = LedgerRecord {
            seq,
            state_key: state_key.to_string(),
            base: b64_encode(&encoded),
        };
        encoded.zeroize();
        let payload = serde_json::to_vec(&record)
            .map_err(|_| SyncError::Persistence("vault ledger could not be encoded".into()))?;
        let sealed = self.seal(KIND_LEDGER, LEDGER_NAME, payload)?;
        self.ledger.save(&sealed).map_err(SyncError::Persistence)
    }

    fn new_state_key(&self, seq: u64) -> SyncResult<String> {
        let mut random = [0u8; 8];
        self.rng
            .fill(&mut random)
            .map_err(|_| SyncError::Unavailable("system random source failed".into()))?;
        Ok(format!(
            "{}{seq:020}-{}",
            self.states_prefix(),
            hex(&random)
        ))
    }

    fn fetch_state(&self, key: &str) -> SyncResult<VaultState> {
        let object = self.store.get(key)?.ok_or_else(|| {
            SyncError::Unavailable("a vault state object the head names is missing".into())
        })?;
        let mut payload = self.open(KIND_STATE, key, &object.body)?;
        let state = VaultState::decode(&payload)
            .map_err(|why| SyncError::Unavailable(format!("vault state: {why}")));
        payload.zeroize();
        state
    }

    /// Upload `state` create-only. A key that already holds exactly these bytes is our own earlier
    /// upload whose answer was lost, and counts as done.
    fn put_state(&self, key: &str, state: &VaultState) -> SyncResult<()> {
        let mut encoded = state.encode()?;
        if encoded.len() > self.config.max_state_bytes {
            encoded.zeroize();
            return Err(SyncError::Persistence(
                "vault is larger than max_state_bytes".into(),
            ));
        }
        let body = self.seal(KIND_STATE, key, encoded)?;
        match self.store.put_if_absent(key, &body)? {
            PutResult::Stored { .. } => Ok(()),
            PutResult::PreconditionFailed => match self.store.get(key)? {
                Some(existing) if existing.body == body => Ok(()),
                _ => Err(SyncError::Persistence(
                    "a vault state object key is already taken by other content".into(),
                )),
            },
        }
    }

    /// Whether the head now names `state_key` (after a write of unknown outcome).
    fn head_names(&self, state_key: &str) -> SyncResult<bool> {
        match self.store.get(&self.head_key())? {
            Some(object) => Ok(self.open_head(&object.body)?.state == state_key),
            None => Ok(false),
        }
    }

    /// Bring this device and the store to one vault. `local` is the vault as the app has it now;
    /// replace it with the returned `state`. See the module docs for the protocol.
    pub fn sync(&self, local: &VaultState, policy: &dyn MergePolicy) -> SyncResult<VaultSync> {
        let _serial = self.serial.lock().unwrap_or_else(PoisonError::into_inner);
        let ledger = self.load_ledger()?;
        let empty = VaultState::new();
        let base = ledger.as_ref().map_or(&empty, |l| &l.base);
        for attempt in 1..=self.config.max_attempts {
            match self.attempt(local, policy, ledger.as_ref(), base)? {
                Attempt::Done(mut done) => {
                    done.attempts = attempt;
                    return Ok(done);
                }
                Attempt::Lost => {}
            }
        }
        Err(SyncError::Transient(format!(
            "the vault head changed under every one of {} attempts",
            self.config.max_attempts
        )))
    }

    fn attempt(
        &self,
        local: &VaultState,
        policy: &dyn MergePolicy,
        ledger: Option<&Ledger>,
        base: &VaultState,
    ) -> SyncResult<Attempt> {
        let head_key = self.head_key();
        let Some(object) = self.store.get(&head_key)? else {
            return self.first_write(local, ledger);
        };
        let head = self.open_head(&object.body)?;
        if let Some(seen) = ledger {
            if head.seq < seen.seq {
                return Err(SyncError::Unavailable(
                    "the vault head is older than one this device already synced (rolled back)"
                        .into(),
                ));
            }
            if head.seq == seen.seq && head.state != seen.state_key {
                return Err(SyncError::Unavailable(
                    "the vault head forks from the one this device synced".into(),
                ));
            }
        }
        let remote_changed = ledger.is_none_or(|l| l.state_key != head.state);
        let local_changed = local != base;
        if !remote_changed && !local_changed {
            return Ok(Attempt::Done(VaultSync {
                state: local.clone(),
                outcome: VaultOutcome::Unchanged,
                conflicts: Vec::new(),
                attempts: 1,
            }));
        }
        let theirs = if remote_changed {
            Some(self.fetch_state(&head.state)?)
        } else {
            None
        };
        if let (Some(theirs), false) = (&theirs, local_changed) {
            self.save_ledger(head.seq, &head.state, theirs)?;
            return Ok(Attempt::Done(VaultSync {
                state: theirs.clone(),
                outcome: VaultOutcome::Pulled,
                conflicts: Vec::new(),
                attempts: 1,
            }));
        }
        let (merged, conflicts) = match &theirs {
            Some(theirs) => merge(base, local, theirs, policy),
            None => (local.clone(), Vec::new()),
        };
        if let Some(theirs) = &theirs {
            if &merged == theirs {
                self.save_ledger(head.seq, &head.state, theirs)?;
                return Ok(Attempt::Done(VaultSync {
                    state: merged,
                    outcome: VaultOutcome::Pulled,
                    conflicts,
                    attempts: 1,
                }));
            }
        }
        let seq = head
            .seq
            .checked_add(1)
            .ok_or_else(|| SyncError::Persistence("vault sequence overflow".into()))?;
        let key = self.new_state_key(seq)?;
        self.put_state(&key, &merged)?;
        let sealed = self.seal_head(seq, &key, Some(head.state.clone()))?;
        let landed = match self.store.put_if_match(&head_key, &sealed, &object.etag) {
            Ok(PutResult::Stored { .. }) => true,
            Ok(PutResult::PreconditionFailed) => false,
            Err(error) if is_unknown_outcome(&error) => match self.head_names(&key) {
                Ok(landed) => landed,
                Err(_) => return Err(error),
            },
            Err(error) => return Err(error),
        };
        if !landed {
            return Ok(Attempt::Lost);
        }
        self.save_ledger(seq, &key, &merged)?;
        Ok(Attempt::Done(VaultSync {
            state: merged,
            outcome: if theirs.is_some() {
                VaultOutcome::Merged
            } else {
                VaultOutcome::Pushed
            },
            conflicts,
            attempts: 1,
        }))
    }

    fn seal_head(&self, seq: u64, state_key: &str, prev: Option<String>) -> SyncResult<Vec<u8>> {
        let payload = serde_json::to_vec(&Head {
            seq,
            state: state_key.to_string(),
            prev,
        })
        .map_err(|_| SyncError::Persistence("vault head could not be encoded".into()))?;
        self.seal(KIND_HEAD, &self.head_key(), payload)
    }

    /// No head in the store. A device that has synced before never treats that as a fresh vault.
    fn first_write(&self, local: &VaultState, ledger: Option<&Ledger>) -> SyncResult<Attempt> {
        if ledger.is_some() {
            return Err(SyncError::Unavailable(
                "the vault head is gone from the store after this device synced it".into(),
            ));
        }
        if local.is_empty() {
            return Ok(Attempt::Done(VaultSync {
                state: local.clone(),
                outcome: VaultOutcome::Unchanged,
                conflicts: Vec::new(),
                attempts: 1,
            }));
        }
        let seq = 1;
        let key = self.new_state_key(seq)?;
        self.put_state(&key, local)?;
        let sealed = self.seal_head(seq, &key, None)?;
        let landed = match self.store.put_if_absent(&self.head_key(), &sealed) {
            Ok(PutResult::Stored { .. }) => true,
            Ok(PutResult::PreconditionFailed) => false,
            Err(error) if is_unknown_outcome(&error) => match self.head_names(&key) {
                Ok(landed) => landed,
                Err(_) => return Err(error),
            },
            Err(error) => return Err(error),
        };
        if !landed {
            return Ok(Attempt::Lost);
        }
        self.save_ledger(seq, &key, local)?;
        Ok(Attempt::Done(VaultSync {
            state: local.clone(),
            outcome: VaultOutcome::Pushed,
            conflicts: Vec::new(),
            attempts: 1,
        }))
    }
}
