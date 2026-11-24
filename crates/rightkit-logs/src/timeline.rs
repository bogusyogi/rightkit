//! Immutable, process-local snapshots for paging a bounded activity history.
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

/// Digest source keys before exporting them. Legacy bytes have a separate namespace.
pub fn source_id(source: &str, stored: Option<&str>, legacy: impl FnOnce() -> String) -> String {
    let digest = |s: &str| rightkit_crypto::hex_encode(&rightkit_crypto::sha256(s.as_bytes()));
    match stored.filter(|id| !id.is_empty()) {
        Some(id) => format!("{source}:{}", digest(id)),
        None => format!("{source}:legacy:{}", digest(&legacy())),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry<T> {
    /// Ordering only. Continuations use the separate cursor bound to this snapshot.
    pub position: String,
    pub cursor: String,
    pub row: T,
}

#[derive(Debug, Serialize)]
pub struct Page<T, M> {
    pub entries: Vec<Entry<T>>,
    pub next_cursor: Option<String>,
    #[serde(flatten)]
    pub metadata: M,
}

/// Caller supplies bounded, projected records and metadata. Building a new snapshot does
/// not replace another reader's rows. Each page owns its results after the store lock is released.
pub struct Snapshot<T, M> {
    key: String,
    entries: Vec<Entry<T>>,
    metadata: M,
}

impl<T, M> Snapshot<T, M> {
    pub fn new(
        rows: Vec<T>,
        metadata: M,
        at: impl Fn(&T) -> i64,
        id: impl Fn(&mut T) -> &mut String,
    ) -> std::io::Result<Self> {
        let mut entropy = [0u8; 16];
        getrandom::fill(&mut entropy).map_err(|e| std::io::Error::other(e.to_string()))?;
        let key = rightkit_crypto::hex_encode(&entropy);
        let mut rows = rows;
        let mut counts = HashMap::<String, usize>::new();
        for row in &mut rows {
            *counts.entry(id(row).clone()).or_default() += 1;
        }
        let mut occurrences = HashMap::<String, usize>::new();
        let mut entries = Vec::with_capacity(rows.len());
        for mut row in rows {
            let time = (at(&row) as u64) ^ (1 << 63);
            let row_id = id(&mut row);
            let mut position = format!("{time:016x}:{row_id}");
            if counts[row_id.as_str()] > 1 {
                let n = occurrences.entry(row_id.clone()).or_default();
                *n += 1;
                position.push_str(&format!("#{n:016x}"));
                // Identical old records have no physical identity across distinct reads.
                // Never reuse a previous snapshot's occurrence ID for a different copy.
                row_id.push_str(&format!("@{key}#{n}"));
            }
            entries.push(Entry {
                position,
                cursor: String::new(),
                row,
            });
        }
        entries.sort_unstable_by(|a, b| b.position.cmp(&a.position));
        for (index, entry) in entries.iter_mut().enumerate() {
            entry.cursor = format!("v1/{key}/{}", index + 1);
        }
        Ok(Self {
            key,
            entries,
            metadata,
        })
    }
}

impl<T: Clone, M: Clone> Snapshot<T, M> {
    fn page(&self, start: usize, limit: NonZeroUsize) -> Page<T, M> {
        let end = start.saturating_add(limit.get()).min(self.entries.len());
        let entries = self.entries[start..end].to_vec();
        let next_cursor = entries
            .last()
            .filter(|_| end < self.entries.len())
            .map(|e| e.cursor.clone());
        Page {
            entries,
            next_cursor,
            metadata: self.metadata.clone(),
        }
    }
}

/// An idle cursor expires. Capacity limits retained snapshots even when requests overlap.
pub struct Snapshots<T, M> {
    capacity: NonZeroUsize,
    idle: Duration,
    items: VecDeque<(Instant, Snapshot<T, M>)>,
}

impl<T: Clone, M: Clone> Snapshots<T, M> {
    pub fn new(capacity: NonZeroUsize, idle: Duration) -> Self {
        Self {
            capacity,
            idle,
            items: VecDeque::new(),
        }
    }

    pub fn start(&mut self, snapshot: Snapshot<T, M>, limit: NonZeroUsize) -> Page<T, M> {
        let page = snapshot.page(0, limit);
        self.prune();
        // A caller may trim a refresh and continue at any returned entry, including
        // one inside a final page. Keep every nonempty snapshot for that operation.
        if !page.entries.is_empty() {
            while self.items.len() >= self.capacity.get() {
                self.items.pop_front();
            }
            self.items.push_back((Instant::now(), snapshot));
        }
        page
    }

    /// Ok(None) means start a fresh traversal. Malformed cursors are independent errors.
    pub fn resume(
        &mut self,
        cursor: &str,
        limit: NonZeroUsize,
    ) -> Result<Option<Page<T, M>>, &'static str> {
        let (key, start) = parse_cursor(cursor).ok_or("Invalid activity cursor.")?;
        self.prune();
        let Some(index) = self.items.iter().position(|(_, s)| s.key == key) else {
            return Ok(None);
        };
        if start > self.items[index].1.entries.len() {
            return Err("Invalid activity cursor.");
        }
        let (_, snapshot) = self.items.remove(index).expect("matched snapshot");
        let page = snapshot.page(start, limit);
        self.items.push_back((Instant::now(), snapshot));
        Ok(Some(page))
    }

    fn prune(&mut self) {
        let now = Instant::now();
        self.items
            .retain(|(last, _)| now.duration_since(*last) < self.idle);
    }
}

fn parse_cursor(cursor: &str) -> Option<(&str, usize)> {
    if cursor.len() > 56 {
        return None;
    }
    let (key, ordinal) = cursor.strip_prefix("v1/")?.split_once('/')?;
    if key.len() != 32
        || !key.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        || ordinal.starts_with('0')
        || ordinal.len() > 20
        || !ordinal.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    Some((key, ordinal.parse().ok()?))
}
