//! The durable, account-scoped deletion outbox.
//!
//! A local deletion is a user's intent that an account's remote zone must lose a row. The outbox
//! keeps that intent until the remote has acknowledged it, partitioned by the account scope it
//! belongs to, so that an account switch, a first bind or an engine ledger reset can never discard
//! it and it is never sent under an account it does not belong to.
//!
//! # Partitions
//! * A scoped partition holds deletions made while that account was established as the live,
//!   consented account. Only that account's passes hand them to the engine, and only while the
//!   partition's consent epoch (below) equals the durable consent epoch now.
//! * The unverified partition (`"scope": null`) holds deletions whose account was not
//!   established when they were made: every entry of a version 1 file (no account was recorded),
//!   and every deletion made while sync was off, without a positive observation of the live
//!   account made at the moment of that deletion (`TranscriptSync::enqueue_deleted_ids` observes
//!   it then and stamps that observation's account; an earlier observation, however recent, and
//!   consent state, the consented account or the consent switch alone never stamp), or while the
//!   live account was not the consented one. `delete_everywhere` stamps its captured token
//!   account (see `DeleteToken`), never a re-read one. They are
//!   retained and inert: NEVER handed to the engine and
//!   never moved by a bind. No entry leaves this partition. [`Outbox::promote`] exists, but
//!   `TranscriptSync` calls it with an empty set: an id being a remote version of an account's
//!   ledger does not prove that an unscoped deletion was made for that account, and nothing in
//!   this module has independent provenance of the account. Entries stay, are counted
//!   ([`Outbox::count_unverified`]) and are never deleted.
//!
//! The engine ledger (`pending_tombstones`) is only the in-flight copy of this queue for the
//! account that is active right now; it is bound to one scope and the engine resets it on
//! binding or switching accounts. This file is the durable owner. An entry leaves it only in
//! [`Outbox::remove`], which `TranscriptSync` calls only after the engine's ledger, bound to the
//! same scope, has durably recorded that the tombstone was written to the remote.
//!
//! # Consent epoch fence
//! Every scoped partition records the durable consent epoch (`TranscriptSyncState::epoch`, in the
//! consent file; it increases on every enable, disable and change of the authorized account) that
//! the checked stamp was made in. [`Outbox::fence`] moves every scoped partition whose epoch is
//! not exactly the current one (older, or unknown) into the unverified partition: retained,
//! inert, counted, never sent. [`Outbox::add`] does the same to a partition of an older epoch
//! before it adds, and refuses an epoch older than the partition's. [`Outbox::eligible`] and
//! [`Outbox::held_ids`] answer only for a partition whose epoch equals the one asked. So an entry
//! stamped under account A in epoch e is never handed to the engine after any consent transition,
//! including one that re-authorizes A later, and including a crash between the stamp and the save.
//!
//! # Hand-over record (`handed`)
//! The engine's ledger queues a handed-over deletion in `pending_tombstones` and may write it to
//! the remote in any later pass. The record names the account and consent epoch under which each
//! id still queued there was handed over ([`Outbox::hand_over`], saved BEFORE the ledger write).
//! At the start of every pass the host compares the ledger's queue with it
//! ([`Outbox::handed_at`]): an id handed over under the current epoch stays, any other id
//! (older epoch, other account, unknown, no record) is withdrawn from the ledger and kept in the
//! unverified partition. [`Outbox::retain_handed`] then drops every id the ledger no longer
//! queues, so the record never holds more than the ledger's queue plus one batch (a withdrawn id
//! stays in it until the ledger has really dropped it, so a crash between the outbox save and
//! the ledger write repeats the same decision).
//!
//! # Prior state of a handed-over id (`fresh`, `unproven`)
//! An id the ledger holds as buried AND confirmed (a tombstone, not queued) is never handed over:
//! the remote already holds the deletion and the engine leaves it exactly as it is. A handed-over
//! id therefore had either no tombstone (`fresh`) or was already queued. The record lists in
//! `fresh` the ids that had no tombstone when they were handed over (read from the ledger before
//! the record is saved; an id already in the record keeps its bit). Only those may lose their
//! local tombstone when their queued write is withdrawn ([`Outbox::handed_fresh`]). A queued id
//! the record does not list as fresh (a ledger or outbox from before the record, a lost record)
//! has an unknown prior state: its queued write is withdrawn, its tombstone is KEPT (the deleted
//! record does not silently come back) and it is marked `unproven` ([`Outbox::mark_unproven`]):
//! counted, never sent from the unverified partition. A later deletion of an unproven id under a
//! current consent is queued again although a tombstone exists; its acknowledgement clears the
//! mark ([`Outbox::remove`]).
//!
//! A mark is evidence about ONE ledger, so it is keyed by (ledger account scope, id) and records
//! the consent epoch it was made in (provenance only). Setting ([`Outbox::mark_unproven`]),
//! consulting ([`Outbox::unproven_among`]) and clearing ([`Outbox::remove`]) all take the same
//! scope: account A marking or acknowledging an id never changes what account B does with its own
//! copy of that id. A mark that cannot name its ledger account is not made. Marks from the
//! previous revision (a bare list of ids, no scope; it never shipped) are QUARANTINED: read, kept
//! in the file and counted ([`Outbox::count_legacy_unproven`]), but never used as evidence for
//! any account: for every account such an id has no mark.
//! A legacy mark only ever adds doubt: an id that carries one is never treated as confirmed by
//! the remote, so a new explicit deletion of it performs its write ([`Outbox::unproven_among`]).
//!
//! # Epoch high-water mark (`high`)
//! The highest consent epoch the file has carried: raised by every scoped [`Outbox::add`], every
//! [`Outbox::hand_over`], every [`Outbox::note_epoch`] (the host raises it to the consent epoch it
//! trusted when it saved), and on read to at least every epoch in the file. The consent file's
//! epoch is trusted only while it is not below it, and a consent transition always moves to
//! `max(consent epoch, high) + 1`, so a consent file that was lost or rolled back can never make
//! an epoch number carry a second meaning. [`Outbox::peek_high_water`] reads it from the head of
//! the file.
//!
//! The mark is also the revocation fence: every consent transition (enable, disable, change of
//! the authorized account) raises it to the new epoch and saves this file BEFORE the consent
//! file is written, under the outbox lock (`ConsentStore::commit_transition` in `sync.rs`). A
//! consent file later rolled back to an older valid one, enabled or not, then carries an epoch
//! below the mark and is untrusted: nothing is handed over, stamped or held back on its word.
//! The transition changes nothing in this file but the mark.
//!
//! # File formats
//! * v1: a bare JSON object `{"<logical id>": <enqueued ms>}`. No scope. Read as the unverified
//!   partition.
//! * v2: `{"v":2,"high":<n>,"partitions":[{"scope":null|"<scope>","epoch":<n>,"entries":{"<logical id>":<ms>}}],"handed":{"scope":"<scope>","epoch":<n>,"ids":["<logical id>"],"fresh":["<logical id>"]},"unproven_marks":[{"scope":"<ledger scope>","epoch":<n>,"ids":["<logical id>"]}],"unproven":["<logical id>"]}`.
//!   `high` (omitted when 0), `handed` (omitted when empty), `fresh` (omitted when empty),
//!   `unproven_marks` (omitted when empty; scoped evidence, used) and `unproven` (omitted when
//!   empty; the legacy unscoped list, quarantined, never used) are described above and below. A build
//!   without them cannot read the file (an unknown top-level key is an error there and the file
//!   is left as it is). A partition with `"scope":null` is the unverified one and carries no epoch. `epoch` is
//!   optional on read: a scoped partition without one has an UNKNOWN epoch and is fenced into the
//!   unverified partition at the first pass or enqueue (strict rule; no build that wrote v2
//!   without it shipped). A key an older reader does not know is ignored by it. (An earlier unreleased revision read
//!   it as "bound to the first account that binds"; no build that did is installed.)
//!
//! A v1 file is converted in memory on read and rewritten as v2 by the next write (the write is
//! an atomic whole-file replacement, so a crash leaves the v1 file intact). A file with any other
//! version is an error and is never overwritten.
//!
//! # Read bounds
//! One no-follow handle, at most [`MAX_OUTBOX_FILE_BYTES`] bytes (checked on the open handle and
//! again while reading), and at most [`MAX_OUTBOX_ENTRIES`] entries counted while decoding, so
//! the decoder stops at the cap instead of allocating for the rest of the file (the ids of the
//! hand-over record count toward the same cap). A file over either
//! cap, or one that cannot be read or decoded, is an `Err`; the file is left as it is.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{self, Read};
use std::path::Path;

use serde::de::{self, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::Serialize;

use super::sync::{open_state_file, read_capped, replace_file_atomically};

/// Largest outbox file read or written. A file over the cap is an error, not a truncation.
/// 2,000,000 entries of about 54 bytes each (a 36-character id and a 13-digit time) are about
/// 108 MB, so a file the entry cap allows always fits.
pub(super) const MAX_OUTBOX_FILE_BYTES: u64 = 160 * 1024 * 1024;
/// Most entries the outbox holds across all partitions. It must cover a delete-everywhere of the
/// largest supported history (1,000,000 local rows) plus the records only the remote holds.
/// Queuing more is an error the caller sees.
pub(super) const MAX_OUTBOX_ENTRIES: usize = 2_000_000;
/// Most partitions (one per account ever used on this device, plus the unverified one).
const MAX_PARTITIONS: usize = 1_024;
/// Longest logical id, in bytes.
const MAX_ID_BYTES: usize = 128;
/// Longest account scope, in bytes.
const MAX_SCOPE_BYTES: usize = 1_024;
const FORMAT_VERSION: u64 = 2;

#[derive(Serialize)]
struct PartitionV2 {
    scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    epoch: Option<u64>,
    entries: BTreeMap<String, u64>,
}

/// The hand-over record as written: the account and consent epoch under which `ids` were handed
/// to the engine's ledger (`pending_tombstones`).
#[derive(Serialize)]
struct HandedV2 {
    scope: String,
    epoch: u64,
    ids: Vec<String>,
    /// The ids (a subset of `ids`) that had no tombstone in the ledger when they were handed
    /// over: the only ids whose local tombstone a withdrawal may lift.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    fresh: Vec<String>,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// `high` is written right after `v`, so a head probe of the file finds it
/// ([`Outbox::peek_high_water`]); field order is the serialization order.
#[derive(Serialize)]
struct FileV2 {
    v: u64,
    #[serde(skip_serializing_if = "is_zero")]
    high: u64,
    partitions: Vec<PartitionV2>,
    #[serde(skip_serializing_if = "Option::is_none")]
    handed: Option<HandedV2>,
    /// Scoped unproven marks (see `MarkGroupV2`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unproven_marks: Vec<MarkGroupV2>,
    /// Legacy unscoped marks of the previous revision, written back as read: quarantined, never
    /// used as evidence for any account.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unproven: Vec<String>,
}

/// The ids one ledger account marked unproven in one consent epoch.
#[derive(Serialize)]
struct MarkGroupV2 {
    scope: String,
    epoch: u64,
    ids: Vec<String>,
}

/// Bytes of the file's head searched for the `high` field.
const HEAD_PROBE_BYTES: usize = 4096;

/// The durable proof of what was handed to the engine: ids handed under one account in one
/// consent epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Handed {
    scope: String,
    epoch: u64,
    ids: BTreeSet<String>,
    /// Subset of `ids` that had no tombstone before the hand-over created it (see `HandedV2`).
    fresh: BTreeSet<String>,
}

/// The in-memory outbox: scope (`None` = unverified) -> logical id -> time the deletion was
/// queued, milliseconds since the Unix epoch. `epochs` holds the consent epoch of each scoped
/// partition; a scoped partition with entries and no `epochs` item has an unknown epoch.
/// `high` is the highest consent epoch this file has ever carried (a partition's, the hand-over
/// record's, or the consent epoch the host saw when it saved). `handed` is the hand-over record.
#[derive(Debug, Default)]
pub(crate) struct Outbox {
    partitions: BTreeMap<Option<String>, BTreeMap<String, u64>>,
    epochs: BTreeMap<String, u64>,
    high: u64,
    handed: Option<Handed>,
    /// Ledger account scope -> id -> consent epoch the mark was made in. Evidence that the
    /// ledger bound to that account kept the id's tombstone (did not lift it) because its
    /// withdrawn queued write had no hand-over evidence of a prior state. Only that account's own
    /// pass consults or clears a mark.
    unproven: BTreeMap<String, BTreeMap<String, u64>>,
    /// Marks read from a file written without scopes: kept and counted, never used.
    legacy_unproven: BTreeSet<String>,
}

// ---------------------------------------------------------------------------
// Bounded decoding
// ---------------------------------------------------------------------------

/// Entries still allowed while one file is decoded, and the reason the decode was refused (a
/// fixed phrase, so the error text never quotes file content).
struct Limits {
    remaining: Cell<usize>,
    reason: Cell<Option<&'static str>>,
}

impl Limits {
    fn new() -> Self {
        Self {
            remaining: Cell::new(MAX_OUTBOX_ENTRIES),
            reason: Cell::new(None),
        }
    }

    fn fail<E: de::Error>(&self, reason: &'static str) -> E {
        self.reason.set(Some(reason));
        E::custom(reason)
    }

    /// Count one entry; refuse the one past the cap.
    fn take<E: de::Error>(&self) -> Result<(), E> {
        match self.remaining.get().checked_sub(1) {
            Some(left) => {
                self.remaining.set(left);
                Ok(())
            }
            None => Err(self.fail("holds more entries than the cap")),
        }
    }
}

type Entries = BTreeMap<String, u64>;
type Partition = (Option<String>, Option<u64>, Entries);

fn insert_min<E: de::Error>(
    limits: &Limits,
    entries: &mut Entries,
    id: String,
    at: u64,
) -> Result<(), E> {
    if id.is_empty() || id.len() > MAX_ID_BYTES {
        return Err(limits.fail("has an invalid id"));
    }
    let slot = entries.entry(id).or_insert(at);
    *slot = (*slot).min(at);
    Ok(())
}

struct EntriesSeed<'a>(&'a Limits);

impl<'de> DeserializeSeed<'de> for EntriesSeed<'_> {
    type Value = Entries;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Entries, D::Error> {
        deserializer.deserialize_map(EntriesVisitor(self.0))
    }
}

struct EntriesVisitor<'a>(&'a Limits);

impl<'de> Visitor<'de> for EntriesVisitor<'_> {
    type Value = Entries;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a map of logical id to queued time")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Entries, A::Error> {
        let mut entries = Entries::new();
        while let Some((id, at)) = map.next_entry::<String, u64>()? {
            self.0.take::<A::Error>()?;
            insert_min::<A::Error>(self.0, &mut entries, id, at)?;
        }
        Ok(entries)
    }
}

struct PartitionSeed<'a>(&'a Limits);

impl<'de> DeserializeSeed<'de> for PartitionSeed<'_> {
    type Value = Partition;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Partition, D::Error> {
        deserializer.deserialize_map(PartitionVisitor(self.0))
    }
}

struct PartitionVisitor<'a>(&'a Limits);

impl<'de> Visitor<'de> for PartitionVisitor<'_> {
    type Value = Partition;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an outbox partition")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Partition, A::Error> {
        let mut scope: Option<String> = None;
        let mut entries: Option<Entries> = None;
        let mut epoch: Option<u64> = None;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "epoch" => epoch = map.next_value::<Option<u64>>()?,
                "scope" => {
                    let value = map.next_value::<Option<String>>()?;
                    if value
                        .as_ref()
                        .is_some_and(|s| s.is_empty() || s.len() > MAX_SCOPE_BYTES)
                    {
                        return Err(self.0.fail("has an invalid scope"));
                    }
                    scope = value;
                }
                "entries" => entries = Some(map.next_value_seed(EntriesSeed(self.0))?),
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        match entries {
            Some(entries) => Ok((scope, epoch, entries)),
            None => Err(self.0.fail("has a partition without entries")),
        }
    }
}

struct PartitionsSeed<'a>(&'a Limits);

impl<'de> DeserializeSeed<'de> for PartitionsSeed<'_> {
    type Value = Vec<Partition>;
    fn deserialize<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Vec<Partition>, D::Error> {
        deserializer.deserialize_seq(PartitionsVisitor(self.0))
    }
}

struct PartitionsVisitor<'a>(&'a Limits);

impl<'de> Visitor<'de> for PartitionsVisitor<'_> {
    type Value = Vec<Partition>;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a list of outbox partitions")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<Partition>, A::Error> {
        let mut partitions = Vec::new();
        while let Some(partition) = seq.next_element_seed(PartitionSeed(self.0))? {
            if partitions.len() >= MAX_PARTITIONS {
                return Err(self.0.fail("has more partitions than the cap"));
            }
            partitions.push(partition);
        }
        Ok(partitions)
    }
}

struct IdsSeed<'a>(&'a Limits);

impl<'de> DeserializeSeed<'de> for IdsSeed<'_> {
    type Value = BTreeSet<String>;
    fn deserialize<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<BTreeSet<String>, D::Error> {
        deserializer.deserialize_seq(IdsVisitor(self.0))
    }
}

struct IdsVisitor<'a>(&'a Limits);

impl<'de> Visitor<'de> for IdsVisitor<'_> {
    type Value = BTreeSet<String>;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a list of logical ids")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<BTreeSet<String>, A::Error> {
        let mut ids = BTreeSet::new();
        while let Some(id) = seq.next_element::<String>()? {
            self.0.take::<A::Error>()?;
            if id.is_empty() || id.len() > MAX_ID_BYTES {
                return Err(self.0.fail("has an invalid id"));
            }
            ids.insert(id);
        }
        Ok(ids)
    }
}

struct HandedSeed<'a>(&'a Limits);

impl<'de> DeserializeSeed<'de> for HandedSeed<'_> {
    type Value = Handed;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Handed, D::Error> {
        deserializer.deserialize_map(HandedVisitor(self.0))
    }
}

struct HandedVisitor<'a>(&'a Limits);

impl<'de> Visitor<'de> for HandedVisitor<'_> {
    type Value = Handed;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a hand-over record")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Handed, A::Error> {
        let mut scope: Option<String> = None;
        let mut epoch: Option<u64> = None;
        let mut ids: Option<BTreeSet<String>> = None;
        let mut fresh: BTreeSet<String> = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "scope" => scope = Some(map.next_value::<String>()?),
                "epoch" => epoch = Some(map.next_value::<u64>()?),
                "ids" => ids = Some(map.next_value_seed(IdsSeed(self.0))?),
                "fresh" => fresh = map.next_value_seed(IdsSeed(self.0))?,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        match (scope, epoch, ids) {
            (Some(scope), Some(epoch), Some(ids))
                if !scope.is_empty() && scope.len() <= MAX_SCOPE_BYTES =>
            {
                // A `fresh` id that is not handed over proves nothing: it is dropped.
                fresh.retain(|id| ids.contains(id));
                Ok(Handed {
                    scope,
                    epoch,
                    ids,
                    fresh,
                })
            }
            _ => Err(self.0.fail("has an invalid hand-over record")),
        }
    }
}

type MarkGroup = (String, u64, BTreeSet<String>);

struct MarksSeed<'a>(&'a Limits);

impl<'de> DeserializeSeed<'de> for MarksSeed<'_> {
    type Value = Vec<MarkGroup>;
    fn deserialize<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Vec<MarkGroup>, D::Error> {
        deserializer.deserialize_seq(MarksVisitor(self.0))
    }
}

struct MarksVisitor<'a>(&'a Limits);

impl<'de> Visitor<'de> for MarksVisitor<'_> {
    type Value = Vec<MarkGroup>;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a list of unproven mark groups")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<MarkGroup>, A::Error> {
        let mut groups = Vec::new();
        while let Some(group) = seq.next_element_seed(MarkGroupSeed(self.0))? {
            if groups.len() >= MAX_PARTITIONS {
                return Err(self.0.fail("has more mark groups than the cap"));
            }
            groups.push(group);
        }
        Ok(groups)
    }
}

struct MarkGroupSeed<'a>(&'a Limits);

impl<'de> DeserializeSeed<'de> for MarkGroupSeed<'_> {
    type Value = MarkGroup;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<MarkGroup, D::Error> {
        deserializer.deserialize_map(MarkGroupVisitor(self.0))
    }
}

struct MarkGroupVisitor<'a>(&'a Limits);

impl<'de> Visitor<'de> for MarkGroupVisitor<'_> {
    type Value = MarkGroup;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an unproven mark group")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<MarkGroup, A::Error> {
        let mut scope: Option<String> = None;
        let mut epoch: Option<u64> = None;
        let mut ids: Option<BTreeSet<String>> = None;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "scope" => scope = Some(map.next_value::<String>()?),
                "epoch" => epoch = Some(map.next_value::<u64>()?),
                "ids" => ids = Some(map.next_value_seed(IdsSeed(self.0))?),
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        match (scope, epoch, ids) {
            (Some(scope), Some(epoch), Some(ids))
                if !scope.is_empty() && scope.len() <= MAX_SCOPE_BYTES =>
            {
                Ok((scope, epoch, ids))
            }
            _ => Err(self.0.fail("has an invalid unproven mark group")),
        }
    }
}

/// Finds `high` in the head of a file. Stops (with an error the caller ignores) at the field,
/// or before skipping a value that is not the field's own (`partitions`, `handed`).
struct HighProbe<'a>(&'a Cell<Option<u64>>);

impl<'de> Visitor<'de> for HighProbe<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a deletion outbox")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "high" => {
                    self.0.set(Some(map.next_value::<u64>()?));
                    return Err(<A::Error as de::Error>::custom("high found"));
                }
                "partitions" | "handed" => {
                    return Err(<A::Error as de::Error>::custom("high absent"));
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(())
    }
}

fn high_water_of_head(head: &[u8]) -> Option<u64> {
    let found: Cell<Option<u64>> = Cell::new(None);
    let mut deserializer = serde_json::Deserializer::from_slice(head);
    let _ = (&mut deserializer).deserialize_map(HighProbe(&found));
    found.get()
}

struct FileSeed<'a>(&'a Limits);

impl<'de> DeserializeSeed<'de> for FileSeed<'_> {
    type Value = Outbox;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Outbox, D::Error> {
        deserializer.deserialize_map(FileVisitor(self.0))
    }
}

/// Reads either format. Keys `v` and `partitions` mark v2; any other key is a v1 entry. A file
/// that mixes the two, has a `v` other than 2 or lacks `partitions` is refused.
struct FileVisitor<'a>(&'a Limits);

impl<'de> Visitor<'de> for FileVisitor<'_> {
    type Value = Outbox;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a deletion outbox")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Outbox, A::Error> {
        let mut version: Option<u64> = None;
        let mut partitions: Option<Vec<Partition>> = None;
        let mut high: Option<u64> = None;
        let mut handed: Option<Handed> = None;
        let mut legacy_unproven: BTreeSet<String> = BTreeSet::new();
        let mut marks: Vec<MarkGroup> = Vec::new();
        let mut legacy = Entries::new();
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "v" => version = Some(map.next_value::<u64>()?),
                "high" => high = Some(map.next_value::<u64>()?),
                "partitions" => partitions = Some(map.next_value_seed(PartitionsSeed(self.0))?),
                "handed" => handed = Some(map.next_value_seed(HandedSeed(self.0))?),
                "unproven" => legacy_unproven = map.next_value_seed(IdsSeed(self.0))?,
                "unproven_marks" => marks = map.next_value_seed(MarksSeed(self.0))?,
                _ => {
                    let at = map.next_value::<u64>()?;
                    self.0.take::<A::Error>()?;
                    insert_min::<A::Error>(self.0, &mut legacy, key, at)?;
                }
            }
        }
        let mut outbox = Outbox::default();
        if version.is_none() && partitions.is_none() {
            if high.is_some()
                || handed.is_some()
                || !legacy_unproven.is_empty()
                || !marks.is_empty()
            {
                return Err(self.0.fail("has an unsupported version"));
            }
            outbox.merge(None, None, legacy);
            return Ok(outbox);
        }
        match (version, partitions) {
            (Some(FORMAT_VERSION), Some(partitions)) if legacy.is_empty() => {
                // The high-water mark is at least every epoch the file carries, whatever `high`
                // says (a file written before the field existed has none).
                let mut seen = high.unwrap_or(0);
                if let Some(held) = &handed {
                    seen = seen.max(held.epoch);
                }
                for (scope, epoch, entries) in partitions {
                    if let Some(epoch) = epoch {
                        seen = seen.max(epoch);
                    }
                    outbox.merge(scope, epoch, entries);
                }
                outbox.high = seen;
                outbox.handed = handed;
                outbox.legacy_unproven = legacy_unproven;
                for (scope, epoch, ids) in marks {
                    let slot = outbox.unproven.entry(scope).or_default();
                    for id in ids {
                        let at = slot.entry(id).or_insert(epoch);
                        *at = (*at).min(epoch);
                    }
                }
                Ok(outbox)
            }
            _ => Err(self.0.fail("has an unsupported version")),
        }
    }
}

impl Outbox {
    /// A missing file is an empty outbox. A file that cannot be read, is over a cap, does not
    /// parse or has an unknown version is an `Err`: the caller must not write over it.
    pub(crate) fn load(path: &Path) -> Result<Self, String> {
        let bytes = match read_capped(path, MAX_OUTBOX_FILE_BYTES) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(format!("read deletion outbox: {error}")),
        };
        let limits = Limits::new();
        let refused = |error: serde_json::Error| match limits.reason.get() {
            Some(reason) => format!("deletion outbox {reason}"),
            None => format!("parse deletion outbox: {:?}", error.classify()),
        };
        let mut deserializer = serde_json::Deserializer::from_slice(&bytes);
        let outbox = FileSeed(&limits)
            .deserialize(&mut deserializer)
            .map_err(&refused)?;
        deserializer.end().map_err(&refused)?;
        Ok(outbox)
    }

    /// Atomic whole-file replacement (temp file, fsync, rename). Empty partitions are dropped.
    pub(crate) fn save(&self, path: &Path) -> Result<(), String> {
        // The same limits the decoder enforces, checked BEFORE anything is written: a state the
        // loader would refuse is never saved, and the file already on disk stays as it is and
        // readable. The decoder counts every partition entry, every handed-over id, every
        // `fresh` id, every scoped mark and every legacy `unproven` id against one budget; so does
        // this.
        if self.counted_entries() > MAX_OUTBOX_ENTRIES {
            return Err("deletion outbox holds more entries than the cap; not saved".to_string());
        }
        if self
            .partitions
            .values()
            .filter(|entries| !entries.is_empty())
            .count()
            > MAX_PARTITIONS
        {
            return Err(
                "deletion outbox holds more partitions than the cap; not saved".to_string(),
            );
        }
        let file = FileV2 {
            v: FORMAT_VERSION,
            high: self.high,
            handed: self.handed.as_ref().map(|held| HandedV2 {
                scope: held.scope.clone(),
                epoch: held.epoch,
                ids: held.ids.iter().cloned().collect(),
                fresh: held.fresh.iter().cloned().collect(),
            }),
            unproven_marks: self.mark_groups(),
            unproven: self.legacy_unproven.iter().cloned().collect(),
            partitions: self
                .partitions
                .iter()
                .filter(|(_, entries)| !entries.is_empty())
                .map(|(scope, entries)| PartitionV2 {
                    scope: scope.clone(),
                    epoch: scope.as_ref().and_then(|s| self.epochs.get(s).copied()),
                    entries: entries.clone(),
                })
                .collect(),
        };
        let bytes = serde_json::to_vec(&file)
            .map_err(|error| format!("serialize deletion outbox: {error}"))?;
        if bytes.len() as u64 > MAX_OUTBOX_FILE_BYTES {
            return Err("deletion outbox is larger than its cap".to_string());
        }
        replace_file_atomically(path, &bytes)
            .map_err(|error| format!("write deletion outbox: {error}"))
    }

    pub(crate) fn len(&self) -> usize {
        self.partitions.values().map(BTreeMap::len).sum()
    }

    /// Everything the decoder counts against `MAX_OUTBOX_ENTRIES`: partition entries, handed-over
    /// ids, their `fresh` subset, scoped marks and legacy `unproven` ids (saturating; used by
    /// `save`).
    fn counted_entries(&self) -> usize {
        let handed = self
            .handed
            .as_ref()
            .map_or(0, |held| held.ids.len().saturating_add(held.fresh.len()));
        self.len()
            .saturating_add(handed)
            .saturating_add(self.count_unproven())
            .saturating_add(self.legacy_unproven.len())
    }

    /// The scoped marks grouped by (scope, epoch), for `save`.
    fn mark_groups(&self) -> Vec<MarkGroupV2> {
        let mut groups: Vec<MarkGroupV2> = Vec::new();
        for (scope, ids) in &self.unproven {
            let mut by_epoch: BTreeMap<u64, Vec<String>> = BTreeMap::new();
            for (id, epoch) in ids {
                by_epoch.entry(*epoch).or_default().push(id.clone());
            }
            for (epoch, ids) in by_epoch {
                groups.push(MarkGroupV2 {
                    scope: scope.clone(),
                    epoch,
                    ids,
                });
            }
        }
        groups
    }

    /// The highest consent epoch this file has carried. The consent file's epoch is trusted only
    /// while it is not below this.
    pub(crate) fn high_water(&self) -> u64 {
        self.high
    }

    /// Raise the high-water mark to `epoch`. Returns whether it moved (the caller saves).
    pub(crate) fn note_epoch(&mut self, epoch: u64) -> bool {
        if epoch > self.high {
            self.high = epoch;
            true
        } else {
            false
        }
    }

    /// Whether `id` was handed to the engine's ledger under `scope` in consent epoch `epoch`.
    pub(crate) fn handed_at(&self, scope: &str, epoch: u64, id: &str) -> bool {
        self.handed
            .as_ref()
            .is_some_and(|held| held.scope == scope && held.epoch == epoch && held.ids.contains(id))
    }

    /// Keep only the handed ids `keep` accepts; the record goes when none is left. Returns
    /// whether anything changed (the caller saves).
    pub(crate) fn retain_handed(&mut self, keep: impl Fn(&str) -> bool) -> bool {
        let Some(held) = self.handed.as_mut() else {
            return false;
        };
        let before = held.ids.len();
        held.ids.retain(|id| keep(id));
        let ids = &held.ids;
        held.fresh.retain(|id| ids.contains(id));
        let changed = held.ids.len() != before;
        if held.ids.is_empty() {
            self.handed = None;
            return true;
        }
        changed
    }

    /// Whether the record proves that handing `id` over to the ledger bound to `scope` CREATED its
    /// tombstone (none existed before), in any consent epoch. Only such an id may lose its local
    /// tombstone when its queued write is withdrawn; every other queued id keeps it.
    pub(crate) fn handed_fresh(&self, scope: &str, id: &str) -> bool {
        self.handed
            .as_ref()
            .is_some_and(|held| held.scope == scope && held.fresh.contains(id))
    }

    /// Mark `ids` (all in the unverified partition) as holding a tombstone kept without proof in
    /// the ledger bound to `scope`, made in consent epoch `epoch`. An id already marked for
    /// `scope` keeps its first epoch. Returns whether anything was added (the caller saves).
    pub(crate) fn mark_unproven(
        &mut self,
        scope: &str,
        epoch: u64,
        ids: &BTreeSet<String>,
    ) -> bool {
        if ids.is_empty() || scope.is_empty() || scope.len() > MAX_SCOPE_BYTES {
            return false;
        }
        let slot = self.unproven.entry(scope.to_string()).or_default();
        let before = slot.len();
        for id in ids {
            slot.entry(id.clone()).or_insert(epoch);
        }
        slot.len() != before
    }

    /// The members of `ids` whose ledger tombstone must NOT be taken as a confirmed remote
    /// deletion for `scope`: those marked unproven for `scope`, and those carrying a legacy
    /// unscoped mark. Another account's scoped marks are never consulted. A legacy mark is used
    /// here only as doubt, never as evidence of anything: it makes a new explicit deletion of
    /// that id perform its remote write instead of being acknowledged without one. The extra
    /// write of a deletion the remote may already hold is harmless; a skipped one is not.
    pub(crate) fn unproven_among(&self, scope: &str, ids: &BTreeSet<String>) -> BTreeSet<String> {
        let scoped = self.unproven.get(scope);
        ids.iter()
            .filter(|id| {
                scoped.is_some_and(|marks| marks.contains_key(*id))
                    || self.legacy_unproven.contains(*id)
            })
            .cloned()
            .collect()
    }

    /// How many scoped marks (all accounts) keep a ledger tombstone without proof.
    pub(crate) fn count_unproven(&self) -> usize {
        self.unproven.values().map(BTreeMap::len).sum()
    }

    /// How many quarantined legacy (unscoped) marks the file carries: kept, never used.
    #[allow(dead_code)]
    pub(crate) fn count_legacy_unproven(&self) -> usize {
        self.legacy_unproven.len()
    }

    /// Record that `ids` are being handed to the engine under `scope` in consent epoch `epoch`.
    /// The caller checked under the outbox lock that this is the durable epoch and saves BEFORE it
    /// writes the ids into the ledger. A record of another scope or epoch is replaced: the host
    /// withdrew every id it did not vouch for before it hands anything new over.
    ///
    /// `fresh` (a subset of `ids`) names the ids the ledger held NO tombstone for when the caller
    /// read it, under the engine lock: the hand-over creates their tombstone. An id already in
    /// the record keeps the bit it was recorded with (a repeat after a crash sees its own
    /// tombstone and must not read it as an earlier one).
    pub(crate) fn hand_over(
        &mut self,
        scope: &str,
        epoch: u64,
        ids: &BTreeSet<String>,
        fresh: &BTreeSet<String>,
    ) -> Result<(), String> {
        if epoch < self.high {
            return Err("deletion outbox holds a newer consent epoch".to_string());
        }
        if scope.is_empty() || scope.len() > MAX_SCOPE_BYTES {
            return Err("deletion outbox: invalid scope".to_string());
        }
        let same = self
            .handed
            .as_ref()
            .is_some_and(|held| held.scope == scope && held.epoch == epoch);
        if !same {
            self.handed = None;
        }
        let held = self.handed.get_or_insert_with(|| Handed {
            scope: scope.to_string(),
            epoch,
            ids: BTreeSet::new(),
            fresh: BTreeSet::new(),
        });
        for id in ids {
            if id.is_empty() || id.len() > MAX_ID_BYTES {
                return Err("deletion outbox: invalid id".to_string());
            }
            if held.ids.len() >= MAX_OUTBOX_ENTRIES && !held.ids.contains(id) {
                return Err("deletion hand-over record is full".to_string());
            }
            if held.ids.insert(id.clone()) && fresh.contains(id) {
                held.fresh.insert(id.clone());
            }
        }
        self.high = self.high.max(epoch);
        Ok(())
    }

    /// The high-water mark of the file at `path` without reading all of it: the head is probed
    /// for `high` (written right after `v`); a file whose head does not show it (a file written
    /// before the field existed, or one whose mark is 0) is read in full. A missing file is 0; an
    /// unreadable one is an `Err`. No lock is taken: the file is replaced atomically.
    pub(crate) fn peek_high_water(path: &Path) -> Result<u64, String> {
        let file = match open_state_file(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(format!("read deletion outbox: {error}")),
        };
        let mut head: Vec<u8> = Vec::with_capacity(HEAD_PROBE_BYTES);
        file.take(HEAD_PROBE_BYTES as u64)
            .read_to_end(&mut head)
            .map_err(|error| format!("read deletion outbox: {error}"))?;
        if let Some(high) = high_water_of_head(&head) {
            return Ok(high);
        }
        Self::load(path).map(|outbox| outbox.high_water())
    }

    /// Add decoded entries to `scope`'s partition, keeping the earlier time of a duplicate.
    /// A scoped partition keeps its epoch only when every merged part names the same one; any
    /// disagreement or unknown part makes it unknown (fenced at the next check).
    fn merge(&mut self, scope: Option<String>, epoch: Option<u64>, entries: Entries) {
        if entries.is_empty() {
            return;
        }
        if let Some(name) = &scope {
            let occupied = self.partitions.get(&scope).is_some_and(|e| !e.is_empty());
            match (occupied, self.epochs.get(name).copied(), epoch) {
                (false, _, Some(e)) => {
                    self.epochs.insert(name.clone(), e);
                }
                (true, Some(a), Some(b)) if a == b => {}
                _ => {
                    self.epochs.remove(name);
                }
            }
        }
        let slot = self.partitions.entry(scope).or_default();
        for (id, at) in entries {
            let entry = slot.entry(id).or_insert(at);
            *entry = (*entry).min(at);
        }
    }

    /// Queue `id` under `scope` (`None` = unverified). Queuing an id that is already queued under
    /// the same scope keeps the earlier time.
    ///
    /// A scoped add is stamped with `epoch`, the durable consent epoch the caller checked under
    /// the outbox lock. A partition of an older or unknown epoch is first moved to the unverified
    /// partition (it can never be sent again); an `epoch` older than the partition's is refused.
    pub(crate) fn add(
        &mut self,
        scope: Option<&str>,
        epoch: u64,
        id: &str,
        at: u64,
    ) -> Result<(), String> {
        if let Some(name) = scope {
            // An epoch below the file's high-water mark was already used by an older stamp (the
            // consent file lost or rolled back its counter): it can never stamp again.
            if epoch < self.high {
                return Err("deletion outbox holds a newer consent epoch".to_string());
            }
            match self.epochs.get(name).copied() {
                Some(held) if held == epoch => {}
                Some(held) if held > epoch => {
                    return Err("deletion outbox holds a newer consent epoch".to_string());
                }
                _ => {
                    self.demote(name);
                    self.epochs.insert(name.to_string(), epoch);
                }
            }
            self.high = self.high.max(epoch);
        }
        let exists = self
            .partitions
            .get(&scope.map(str::to_string))
            .is_some_and(|entries| entries.contains_key(id));
        if !exists && self.len() >= MAX_OUTBOX_ENTRIES {
            return Err("deletion outbox is full".to_string());
        }
        let entry = self
            .partitions
            .entry(scope.map(str::to_string))
            .or_default()
            .entry(id.to_string())
            .or_insert(at);
        *entry = (*entry).min(at);
        Ok(())
    }

    /// Move `scope`'s partition into the unverified one (earlier time of a duplicate kept) and
    /// forget its epoch. Returns how many entries moved.
    fn demote(&mut self, scope: &str) -> usize {
        self.epochs.remove(scope);
        let Some(entries) = self.partitions.remove(&Some(scope.to_string())) else {
            return 0;
        };
        let count = entries.len();
        self.merge(None, None, entries);
        count
    }

    /// Move every scoped partition whose epoch is not exactly `current` (older or unknown) into
    /// the unverified partition. Returns how many entries moved; the caller saves when non-zero.
    pub(crate) fn fence(&mut self, current: u64) -> usize {
        let stale: Vec<String> = self
            .partitions
            .iter()
            .filter_map(|(key, entries)| {
                let name = key.as_ref()?;
                (!entries.is_empty() && self.epochs.get(name).copied() != Some(current))
                    .then(|| name.clone())
            })
            .collect();
        stale.iter().map(|name| self.demote(name)).sum()
    }

    /// `scope`'s partition, only when its epoch is exactly `epoch`.
    fn scoped(&self, scope: &str, epoch: u64) -> Option<&Entries> {
        if self.epochs.get(scope).copied() != Some(epoch) {
            return None;
        }
        self.partitions.get(&Some(scope.to_string()))
    }

    /// At most `limit` deletions, in id order, that may be handed to the engine while `scope` is
    /// the active account in consent epoch `epoch`: that scope's own partition, and only if it
    /// was stamped in exactly that epoch. The unverified partition, every other scope's partition
    /// and a partition of any other epoch are never returned.
    pub(crate) fn eligible(&self, scope: &str, epoch: u64, limit: usize) -> BTreeSet<String> {
        self.scoped(scope, epoch)
            .map(|entries| entries.keys().take(limit).cloned().collect())
            .unwrap_or_default()
    }

    /// The ids of `scope`'s own partition: the ids whose pulled copies must not be re-inserted
    /// into history while a pass for `scope` runs. The unverified partition is NOT included: an
    /// id being present in an account's zone says nothing about which account an unscoped
    /// deletion was meant for, so an unverified deletion neither suppresses a pulled copy nor is
    /// sent.
    pub(crate) fn held_ids(&self, scope: &str, epoch: u64) -> BTreeSet<String> {
        self.scoped(scope, epoch)
            .map(|entries| entries.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// How many deletions wait for `scope` (its own partition).
    pub(crate) fn count_for(&self, scope: &str) -> usize {
        self.partitions
            .get(&Some(scope.to_string()))
            .map_or(0, BTreeMap::len)
    }

    /// How many deletions are unverified (owner account not established).
    pub(crate) fn count_unverified(&self) -> usize {
        self.partitions.get(&None).map_or(0, BTreeMap::len)
    }

    /// How many deletions belong to accounts other than `scope`.
    pub(crate) fn count_other_accounts(&self, scope: Option<&str>) -> usize {
        self.partitions
            .iter()
            .filter(|(key, _)| key.is_some() && key.as_deref() != scope)
            .map(|(_, entries)| entries.len())
            .sum()
    }

    /// Move the ids in `proven` that are unverified into `scope`'s partition. Returns how many
    /// moved. Ids that are not unverified are left alone. The only caller passes an empty set
    /// today: nothing in this module can establish which account an unscoped deletion was made
    /// for, so nothing is promoted. Kept for the day an account-provenance source exists.
    pub(crate) fn promote(&mut self, scope: &str, epoch: u64, proven: &BTreeSet<String>) -> usize {
        let Some(unverified) = self.partitions.get_mut(&None) else {
            return 0;
        };
        let mut moved: Entries = Entries::new();
        for id in proven {
            if let Some(at) = unverified.remove(id) {
                moved.insert(id.clone(), at);
            }
        }
        let count = moved.len();
        self.merge(Some(scope.to_string()), Some(epoch), moved);
        count
    }

    /// Remove acknowledged ids from `scope`'s partition only. Returns whether anything left.
    pub(crate) fn remove(&mut self, scope: &str, ids: &BTreeSet<String>) -> bool {
        let mut changed = false;
        if let Some(entries) = self.partitions.get_mut(&Some(scope.to_string())) {
            let before = entries.len();
            entries.retain(|id, _| !ids.contains(id));
            changed = entries.len() != before;
        }
        // An acknowledged id has its tombstone confirmed by the remote of THIS account: only this
        // account's doubt is gone. Other accounts' marks and the legacy ones stay as they are.
        if let Some(marks) = self.unproven.get_mut(scope) {
            let before = marks.len();
            marks.retain(|id, _| !ids.contains(id));
            changed |= marks.len() != before;
            if marks.is_empty() {
                self.unproven.remove(scope);
            }
        }
        changed
    }
}
