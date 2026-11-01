//! The mailbox server's on-disk store: one directory per mailbox, one file per item.
//!
//! Layout under the data directory: `<64 hex owner>/allow.json`, `<owner>/usage.json` and
//! `<owner>/<20 digit seq>.item` (`u32 header length | JSON ItemHeader | sealed body`). Each write
//! is a temporary file, `fsync`, rename, so a crash leaves the old or the new file, never half.
//! The store never sees plaintext: bodies are sealed by the sender (`seal.rs`).
//!
//! Limits (`Limits`): a size cap per item, a queued-bytes cap and an item-count cap per mailbox,
//! a stored-bytes cap for the whole server, a 7-day TTL, a monthly deposit ceiling per mailbox and
//! a per-sender deposit rate. A full mailbox or server refuses the deposit (`queue_full`); the
//! server never drops a stored item to make room. Items are
//! deleted on `ack` and swept when they pass the TTL. Logs and events carry counts only, never ids.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use rightkit_fs::{AtomicOptions, atomic_write_with};
use serde::{Deserialize, Serialize};

use super::protocol::{DEFAULT_MAX_ITEM_BYTES, ItemHeader};
use crate::caps::month_key;
use crate::error::MeshError;
use crate::util::is_hex;

const DAY_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Clone, Debug)]
pub struct Limits {
    /// Largest sealed item. Blobs are NOT stored by default (D7: the relay never stores images).
    pub max_item_bytes: usize,
    /// Bytes one mailbox may hold at once.
    pub queue_bytes: u64,
    /// Bytes the whole server may hold at once, across every mailbox: the bound on what any set of
    /// callers can make the box keep. A deposit that would pass it is refused with `queue_full`.
    pub global_bytes: u64,
    pub max_items: usize,
    /// How long an unacknowledged item is kept.
    pub ttl_ms: u64,
    /// Bytes one mailbox may receive per calendar month (UTC): the small-traffic ceiling, 2 GB.
    pub monthly_ceiling: u64,
    /// Deposits one sender may make per minute across all mailboxes.
    pub deposits_per_minute: usize,
    /// Mailboxes the server will create. A mailbox exists only once its owner sets its allow list,
    /// so this bounds what anonymous endpoints can make the server keep (admission with proof of
    /// work is the later RK2 step).
    pub max_mailboxes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_item_bytes: DEFAULT_MAX_ITEM_BYTES,
            queue_bytes: 50 * 1024 * 1024,
            global_bytes: 2 * 1024 * 1024 * 1024,
            max_items: 10_000,
            ttl_ms: 7 * DAY_MS,
            monthly_ceiling: 2 * 1024 * 1024 * 1024,
            deposits_per_minute: 120,
            max_mailboxes: 100_000,
        }
    }
}

/// A refusal with the machine word the client sees.
#[derive(Debug)]
pub struct StoreError {
    pub code: &'static str,
    pub message: String,
}

impl StoreError {
    fn new(code: &'static str, message: &str) -> Self {
        Self {
            code,
            message: message.to_string(),
        }
    }
}

impl From<MeshError> for StoreError {
    fn from(error: MeshError) -> Self {
        StoreError::new("internal", &error.to_string())
    }
}

#[derive(Serialize, Deserialize, Default)]
struct AllowFile {
    senders: Vec<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct UsageFile {
    month: String,
    bytes: u64,
}

struct ItemMeta {
    size: u64,
    deposited_ms: u64,
}

struct Mailbox {
    next_seq: u64,
    items: BTreeMap<u64, ItemMeta>,
    queued: u64,
    allow: HashSet<String>,
    month: String,
    month_bytes: u64,
}

struct Inner {
    boxes: HashMap<String, Mailbox>,
    recent: HashMap<String, VecDeque<u64>>,
}

pub struct Store {
    root: PathBuf,
    limits: Limits,
    inner: Mutex<Inner>,
    /// Bytes of sealed items held across all mailboxes. Counted from the files at open (a file
    /// includes its small header, so the figure starts a little high and is exact again at the
    /// next start) and kept up to date on deposit, ack and expiry.
    stored: AtomicU64,
}

fn item_path(dir: &Path, seq: u64) -> PathBuf {
    dir.join(format!("{seq:020}.item"))
}

fn encode_item(header: &ItemHeader, body: &[u8]) -> Result<Vec<u8>, MeshError> {
    let header = serde_json::to_vec(header).map_err(|e| MeshError::Store(e.to_string()))?;
    let mut out = Vec::with_capacity(4 + header.len() + body.len());
    out.extend_from_slice(&(header.len() as u32).to_be_bytes());
    out.extend_from_slice(&header);
    out.extend_from_slice(body);
    Ok(out)
}

fn decode_item(raw: &[u8]) -> Option<(ItemHeader, Vec<u8>)> {
    let len = u32::from_be_bytes(raw.get(..4)?.try_into().ok()?) as usize;
    let header: ItemHeader = serde_json::from_slice(raw.get(4..4 + len)?).ok()?;
    Some((header, raw.get(4 + len..)?.to_vec()))
}

impl Store {
    pub fn open(root: PathBuf, limits: Limits) -> Result<Self, MeshError> {
        fs::create_dir_all(&root).map_err(|e| MeshError::Store(e.to_string()))?;
        let stored = Self::scan_stored(&root);
        Ok(Self {
            root,
            limits,
            stored: AtomicU64::new(stored),
            inner: Mutex::new(Inner {
                boxes: HashMap::new(),
                recent: HashMap::new(),
            }),
        })
    }

    /// Sum of the sizes of every `.item` file under the mailboxes.
    fn scan_stored(root: &Path) -> u64 {
        let Ok(owners) = fs::read_dir(root) else {
            return 0;
        };
        let mut total = 0u64;
        for owner in owners.flatten() {
            let Ok(files) = fs::read_dir(owner.path()) else {
                continue;
            };
            for file in files.flatten() {
                if file.file_name().to_string_lossy().ends_with(".item")
                    && let Ok(meta) = file.metadata()
                {
                    total = total.saturating_add(meta.len());
                }
            }
        }
        total
    }

    /// Bytes currently held across all mailboxes.
    pub fn stored_bytes(&self) -> u64 {
        self.stored.load(Ordering::SeqCst)
    }

    fn release(&self, bytes: u64) {
        let _ = self
            .stored
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
                Some(v.saturating_sub(bytes))
            });
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    fn load_mailbox(&self, owner: &str, now_ms: u64) -> Result<Mailbox, MeshError> {
        let dir = self.root.join(owner);
        let mut mailbox = Mailbox {
            next_seq: 1,
            items: BTreeMap::new(),
            queued: 0,
            allow: HashSet::new(),
            month: month_key(now_ms),
            month_bytes: 0,
        };
        if let Ok(raw) = fs::read(dir.join("allow.json"))
            && let Ok(file) = serde_json::from_slice::<AllowFile>(&raw)
        {
            mailbox.allow = file.senders.into_iter().collect();
        }
        if let Ok(raw) = fs::read(dir.join("usage.json"))
            && let Ok(file) = serde_json::from_slice::<UsageFile>(&raw)
            && file.month == mailbox.month
        {
            mailbox.month_bytes = file.bytes;
        }
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                let Some(stem) = name.strip_suffix(".item") else {
                    continue;
                };
                let Ok(seq) = stem.parse::<u64>() else {
                    continue;
                };
                let Ok(raw) = fs::read(entry.path()) else {
                    continue;
                };
                let Some((header, body)) = decode_item(&raw) else {
                    continue;
                };
                mailbox.queued += body.len() as u64;
                mailbox.next_seq = mailbox.next_seq.max(seq + 1);
                mailbox.items.insert(
                    seq,
                    ItemMeta {
                        size: body.len() as u64,
                        deposited_ms: header.deposited_ms,
                    },
                );
            }
        }
        Ok(mailbox)
    }

    fn with_box<R>(
        &self,
        owner: &str,
        now_ms: u64,
        create: bool,
        f: impl FnOnce(&mut Mailbox, &Path, &Limits) -> Result<R, StoreError>,
    ) -> Result<R, StoreError> {
        if !is_hex(owner, 64) {
            return Err(StoreError::new("malformed", "mailbox id"));
        }
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if !inner.boxes.contains_key(owner) {
            if !self.root.join(owner).is_dir() {
                if !create {
                    return Err(StoreError::new("no_mailbox", "no such mailbox"));
                }
                let existing = fs::read_dir(&self.root).map_or(0, |entries| entries.count());
                if existing >= self.limits.max_mailboxes {
                    return Err(StoreError::new(
                        "quota",
                        "the server holds as many mailboxes as it allows",
                    ));
                }
            }
            let loaded = self.load_mailbox(owner, now_ms)?;
            inner.boxes.insert(owner.to_string(), loaded);
        }
        let mailbox = inner.boxes.get_mut(owner).expect("just inserted");
        let month = month_key(now_ms);
        if mailbox.month != month {
            mailbox.month = month;
            mailbox.month_bytes = 0;
        }
        f(mailbox, &self.root.join(owner), &self.limits)
    }

    fn expire_locked(&self, mailbox: &mut Mailbox, dir: &Path, ttl_ms: u64, now_ms: u64) -> u32 {
        let stale: Vec<u64> = mailbox
            .items
            .iter()
            .filter(|(_, meta)| meta.deposited_ms.saturating_add(ttl_ms) < now_ms)
            .map(|(seq, _)| *seq)
            .collect();
        for seq in &stale {
            let _ = fs::remove_file(item_path(dir, *seq));
            if let Some(meta) = mailbox.items.remove(seq) {
                mailbox.queued = mailbox.queued.saturating_sub(meta.size);
                self.release(meta.size);
            }
        }
        stale.len() as u32
    }

    /// Put one sealed item in `to`'s mailbox on behalf of `from`. Returns the server sequence.
    pub fn deposit(
        &self,
        from: &str,
        to: &str,
        record_id: &str,
        body: &[u8],
        now_ms: u64,
    ) -> Result<u64, StoreError> {
        if !is_hex(from, 64) || !is_hex(record_id, 32) {
            return Err(StoreError::new("malformed", "ids"));
        }
        if body.is_empty() || body.len() > self.limits.max_item_bytes {
            return Err(StoreError::new(
                "too_large",
                "the item is over the mailbox size limit",
            ));
        }
        {
            let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            let recent = inner.recent.entry(from.to_string()).or_default();
            while recent.front().is_some_and(|t| t + 60_000 < now_ms) {
                recent.pop_front();
            }
            if recent.len() >= self.limits.deposits_per_minute {
                return Err(StoreError::new("rate_limited", "too many deposits"));
            }
            recent.push_back(now_ms);
        }
        let result = self.with_box(to, now_ms, false, |mailbox, dir, limits| {
            if !mailbox.allow.contains(from) {
                return Err(StoreError::new(
                    "not_allowed",
                    "this device may not deposit here",
                ));
            }
            self.expire_locked(mailbox, dir, limits.ttl_ms, now_ms);
            let size = body.len() as u64;
            if mailbox.items.len() >= limits.max_items
                || mailbox.queued.saturating_add(size) > limits.queue_bytes
            {
                return Err(StoreError::new("queue_full", "the mailbox is full"));
            }
            // Reserve the bytes before writing, so two deposits cannot both pass the cap.
            let reserved = self
                .stored
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
                    v.checked_add(size)
                        .filter(|total| *total <= limits.global_bytes)
                });
            if reserved.is_err() {
                return Err(StoreError::new(
                    "queue_full",
                    "the server is full; try again later",
                ));
            }
            if mailbox.month_bytes.saturating_add(size) > limits.monthly_ceiling {
                return Err(StoreError::new("quota", "monthly mailbox ceiling reached"));
            }
            let seq = mailbox.next_seq;
            let header = ItemHeader {
                seq,
                from: from.to_string(),
                record_id: record_id.to_string(),
                deposited_ms: now_ms,
            };
            let item = match encode_item(&header, body) {
                Ok(item) => item,
                Err(e) => {
                    self.release(size);
                    return Err(e.into());
                }
            };
            if let Err(e) = atomic_write_with(
                &item_path(dir, seq),
                AtomicOptions {
                    create_parents: true,
                    ..AtomicOptions::default()
                },
                |f| f.write_all(&item),
            ) {
                self.release(size);
                return Err(MeshError::Store(e.to_string()).into());
            }
            mailbox.next_seq = seq + 1;
            mailbox.queued += size;
            mailbox.items.insert(
                seq,
                ItemMeta {
                    size,
                    deposited_ms: now_ms,
                },
            );
            mailbox.month_bytes += size;
            let usage = serde_json::to_vec(&UsageFile {
                month: mailbox.month.clone(),
                bytes: mailbox.month_bytes,
            })
            .map_err(|e| MeshError::Store(e.to_string()))?;
            atomic_write_with(
                &dir.join("usage.json"),
                AtomicOptions {
                    create_parents: true,
                    ..AtomicOptions::default()
                },
                |f| f.write_all(&usage),
            )
            .map_err(|e| MeshError::Store(e.to_string()))?;
            Ok(seq)
        });
        match result {
            Err(e) if e.code == "no_mailbox" => Err(StoreError::new(
                "not_allowed",
                "this device may not deposit here",
            )),
            other => other,
        }
    }

    /// Items in `owner`'s mailbox after `after`, oldest first. Expired items are deleted first.
    pub fn collect(
        &self,
        owner: &str,
        after: u64,
        limit: u32,
        now_ms: u64,
    ) -> Result<Vec<(ItemHeader, Vec<u8>)>, StoreError> {
        let result = self.with_box(owner, now_ms, false, |mailbox, dir, limits| {
            self.expire_locked(mailbox, dir, limits.ttl_ms, now_ms);
            let seqs: Vec<u64> = mailbox
                .items
                .range(after.saturating_add(1)..)
                .map(|(seq, _)| *seq)
                .take(limit as usize)
                .collect();
            let mut out = Vec::new();
            for seq in seqs {
                let Ok(raw) = fs::read(item_path(dir, seq)) else {
                    continue;
                };
                if let Some(item) = decode_item(&raw) {
                    out.push(item);
                }
            }
            Ok(out)
        });
        match result {
            Err(e) if e.code == "no_mailbox" => Ok(Vec::new()),
            other => other,
        }
    }

    /// Delete the acknowledged items. Returns how many existed.
    pub fn ack(&self, owner: &str, seqs: &[u64], now_ms: u64) -> Result<u32, StoreError> {
        let result = self.with_box(owner, now_ms, false, |mailbox, dir, _| {
            let mut deleted = 0;
            for seq in seqs {
                if let Some(meta) = mailbox.items.remove(seq) {
                    let _ = fs::remove_file(item_path(dir, *seq));
                    mailbox.queued = mailbox.queued.saturating_sub(meta.size);
                    self.release(meta.size);
                    deleted += 1;
                }
            }
            Ok(deleted)
        });
        match result {
            Err(e) if e.code == "no_mailbox" => Ok(0),
            other => other,
        }
    }

    /// Replace the list of endpoints allowed to deposit into `owner`'s mailbox.
    pub fn set_allow(
        &self,
        owner: &str,
        senders: &[String],
        now_ms: u64,
    ) -> Result<(), StoreError> {
        if senders.len() > 256 || senders.iter().any(|s| !is_hex(s, 64)) {
            return Err(StoreError::new("malformed", "sender list"));
        }
        self.with_box(owner, now_ms, true, |mailbox, dir, _| {
            mailbox.allow = senders.iter().cloned().collect();
            let raw = serde_json::to_vec(&AllowFile {
                senders: senders.to_vec(),
            })
            .map_err(|e| MeshError::Store(e.to_string()))?;
            atomic_write_with(
                &dir.join("allow.json"),
                AtomicOptions {
                    create_parents: true,
                    ..AtomicOptions::default()
                },
                |f| f.write_all(&raw),
            )
            .map_err(|e| MeshError::Store(e.to_string()))?;
            Ok(())
        })
    }

    /// Delete every expired item in every mailbox on disk. Returns the count (counts only, no ids).
    pub fn sweep(&self, now_ms: u64) -> u32 {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return 0;
        };
        let owners: Vec<String> = entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| is_hex(name, 64))
            .collect();
        let mut expired = 0;
        for owner in owners {
            let ttl = self.limits.ttl_ms;
            let was_cached = self
                .inner
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .boxes
                .contains_key(&owner);
            if let Ok(count) = self.with_box(&owner, now_ms, false, |mailbox, dir, _| {
                Ok(self.expire_locked(mailbox, dir, ttl, now_ms))
            }) {
                expired += count;
            }
            if !was_cached {
                // Loaded only for the sweep: drop it so the sweep does not retain every owner.
                self.inner
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .boxes
                    .remove(&owner);
            }
        }
        expired
    }
}
