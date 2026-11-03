//! Rename-storm coalescing.
//!
//! Editors save by writing a temp file and renaming it over the original; build tools and VCS
//! checkouts create, rename, and delete thousands of paths in milliseconds. [`ChangeCoalescer`]
//! absorbs raw `notify` events and, on [`ChangeCoalescer::drain`], reports the *net* result by
//! consulting the filesystem: a path that exists is `changed`, a path that vanished is `removed`,
//! a path created and gone again inside one batch (an atomic-save temp file) is dropped, and a
//! rename chain `a -> b -> c` collapses to a single `a -> c`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use notify::event::{ModifyKind, RenameMode};
use notify::{Event, EventKind};

/// A rename whose source existed before the batch and whose destination exists now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rename {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// One raw backend record absorbed into a batch, kept for a journaling consumer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawChange {
    pub path: PathBuf,
    /// The backend's own event kind, when the caller supplied one (`push` always does).
    pub kind: Option<EventKind>,
    /// The backend's monotonic position for this record (the FSEvents event id), when the caller
    /// supplied one. Persist the highest to resume a journal.
    pub source_clock: Option<u64>,
}

/// Cumulative figures of one coalescer since it was created. Read with
/// [`ChangeCoalescer::totals`]; batches carry the same figures for one drain.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CoalesceTotals {
    /// Raw path records absorbed.
    pub raw_events: u64,
    /// Raw records that produced no record of their own: absorbed into an earlier record of the
    /// same path inside one batch (`raw_events` minus distinct paths per batch).
    pub coalesced: u64,
    /// Overflow / dropped-event / rescan signals absorbed.
    pub overflows: u64,
    /// Raw records the journal had no room for (see [`ChangeCoalescer::journal`]).
    pub raw_dropped: u64,
    /// Highest `source_clock` seen.
    pub source_clock: Option<u64>,
}

/// Net effect of one burst. Every list is sorted and duplicate-free.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeBatch {
    /// Paths that exist now and were created or modified (includes rename destinations).
    pub changed: Vec<PathBuf>,
    /// Paths that existed before the batch and are gone now.
    pub removed: Vec<PathBuf>,
    /// Collapsed rename pairs (only reported when the backend pairs the two halves).
    pub renamed: Vec<Rename>,
    /// Raw records in arrival order, for a journaling consumer. Empty unless
    /// [`ChangeCoalescer::journal`] enabled it; bounded by its capacity.
    pub raw: Vec<RawChange>,
    /// Raw path records absorbed in this batch.
    pub raw_events: u64,
    /// Raw records of this batch that merged into another record of the same path.
    pub coalesced: u64,
    /// Overflow / dropped-event / rescan signals in this batch. When non-zero the net lists are
    /// not complete: reconcile a snapshot before trusting them.
    pub overflows: u64,
    /// Raw records of this batch the journal had no room for.
    pub raw_dropped: u64,
    /// Highest backend `source_clock` among this batch's raw records.
    pub source_clock: Option<u64>,
}

impl ChangeBatch {
    /// True when no path changed, was removed or was renamed. Ignores the counters: check
    /// [`ChangeBatch::has_gap`] as well when overflow matters.
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.removed.is_empty() && self.renamed.is_empty()
    }

    /// True when the batch absorbed an overflow signal, so a snapshot reconcile is owed.
    pub fn has_gap(&self) -> bool {
        self.overflows > 0
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum First {
    Created,
    Other,
}

struct Touch {
    first: First,
}

#[derive(Default)]
pub struct ChangeCoalescer {
    touched: BTreeMap<PathBuf, Touch>,
    renames: Vec<(PathBuf, PathBuf)>,
    /// Paths an earlier drain reported as existing. FSEvents re-reports an item's accumulated
    /// `ItemCreated` flag with its `ItemRemoved`, so a file created in an earlier batch can look
    /// created-and-deleted inside one window; a path in this set is still a removal (#1348).
    reported: BTreeSet<PathBuf>,
    /// Most raw records a batch keeps for a journaling consumer; 0 keeps none.
    journal_cap: usize,
    raw: Vec<RawChange>,
    batch_raw_events: u64,
    batch_raw_dropped: u64,
    batch_overflows: u64,
    batch_clock: Option<u64>,
    totals: CoalesceTotals,
}

impl ChangeCoalescer {
    /// Keep up to `capacity` raw records per batch in [`ChangeBatch::raw`] (arrival order, with
    /// the backend kind and `source_clock` the caller supplied). Records beyond it are counted in
    /// `raw_dropped`, never queued: memory stays bounded. Off by default (capacity 0), so a
    /// consumer that does not journal pays only for the counters.
    pub fn journal(mut self, capacity: usize) -> Self {
        self.journal_cap = capacity;
        self
    }

    /// Cumulative raw, coalesced, overflow and dropped counts, plus the highest `source_clock`,
    /// since this coalescer was created. Includes the batch still pending.
    pub fn totals(&self) -> CoalesceTotals {
        let mut totals = self.totals;
        totals.raw_events += self.batch_raw_events;
        totals.overflows += self.batch_overflows;
        totals.raw_dropped += self.batch_raw_dropped;
        totals.coalesced += self
            .batch_raw_events
            .saturating_sub(self.touched.len() as u64);
        totals.source_clock = max_clock(totals.source_clock, self.batch_clock);
        totals
    }

    fn note_clock(&mut self, clock: Option<u64>) {
        self.batch_clock = max_clock(self.batch_clock, clock);
    }

    fn note(&mut self, path: &Path, kind: Option<EventKind>, clock: Option<u64>) {
        self.batch_raw_events += 1;
        self.note_clock(clock);
        if self.journal_cap > 0 {
            if self.raw.len() < self.journal_cap {
                self.raw.push(RawChange {
                    path: path.to_path_buf(),
                    kind,
                    source_clock: clock,
                });
            } else {
                self.batch_raw_dropped += 1;
            }
        }
    }

    fn touch(&mut self, path: &Path, first: First, kind: Option<EventKind>, clock: Option<u64>) {
        self.note(path, kind, clock);
        self.touched
            .entry(path.to_path_buf())
            .or_insert(Touch { first });
    }

    /// Absorb a typed event without constructing backend events. Gaps return an
    /// error: the caller must reconcile its snapshot before trusting this batch.
    /// No pending changes are cleared when an error is returned.
    pub fn push_watch_event(&mut self, event: &crate::WatchEvent) -> Result<(), crate::WatchError> {
        self.push_watch_record(event, None, None)
    }

    /// As [`ChangeCoalescer::push_watch_event`], also recording the backend `raw_kind` and
    /// `source_clock` of the record for the batch's journal (issue #544). An overflow is counted
    /// in `overflows` before the error is returned.
    pub fn push_watch_record(
        &mut self,
        event: &crate::WatchEvent,
        raw_kind: Option<EventKind>,
        source_clock: Option<u64>,
    ) -> Result<(), crate::WatchError> {
        use crate::WatchEvent;
        match event {
            WatchEvent::Create(path) => self.touch(path, First::Created, raw_kind, source_clock),
            WatchEvent::Modify(path) | WatchEvent::Delete(path) => {
                self.touch(path, First::Other, raw_kind, source_clock)
            }
            WatchEvent::Rename { from, to } => {
                self.touch(from, First::Other, raw_kind, source_clock);
                self.touch(to, First::Other, raw_kind, source_clock);
                self.push_rename(from.clone(), to.clone());
            }
            WatchEvent::Error(error) => {
                self.note_clock(source_clock);
                return Err(error.clone());
            }
            WatchEvent::Overflow => {
                self.batch_overflows += 1;
                self.note_clock(source_clock);
                return Err(crate::WatchError::Overflow);
            }
        }
        Ok(())
    }

    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.touched.is_empty() && self.renames.is_empty()
    }

    /// Absorb one raw event. Access and "other" events are ignored (a rescan flag is counted as
    /// an overflow).
    pub fn push(&mut self, event: &Event) {
        self.push_with_clock(event, None);
    }

    /// As [`ChangeCoalescer::push`], recording the backend's `source_clock` for the event
    /// (issue #544). The raw kind is `event.kind`.
    pub fn push_with_clock(&mut self, event: &Event, source_clock: Option<u64>) {
        if event.need_rescan() {
            self.batch_overflows += 1;
            self.note_clock(source_clock);
        }
        let created = matches!(event.kind, EventKind::Create(_));
        let relevant = matches!(
            event.kind,
            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
        );
        if !relevant {
            return;
        }
        let first = if created {
            First::Created
        } else {
            First::Other
        };
        for path in &event.paths {
            self.touch(path, first, Some(event.kind), source_clock);
        }
        if let EventKind::Modify(ModifyKind::Name(RenameMode::Both)) = event.kind {
            if let [from, to] = event.paths.as_slice() {
                self.push_rename(from.clone(), to.clone());
            }
        }
    }

    fn push_rename(&mut self, from: PathBuf, to: PathBuf) {
        // Collapse chains: a->b then b->c becomes a->c.
        if let Some(entry) = self
            .renames
            .iter_mut()
            .find(|(_, existing_to)| *existing_to == from)
        {
            entry.1 = to;
        } else {
            self.renames.push((from, to));
        }
    }

    /// Resolve everything absorbed so far against the filesystem and reset.
    pub fn drain(&mut self) -> ChangeBatch {
        let touched = std::mem::take(&mut self.touched);
        let renames = std::mem::take(&mut self.renames);
        let mut batch = ChangeBatch {
            raw: std::mem::take(&mut self.raw),
            raw_events: std::mem::take(&mut self.batch_raw_events),
            coalesced: 0,
            overflows: std::mem::take(&mut self.batch_overflows),
            raw_dropped: std::mem::take(&mut self.batch_raw_dropped),
            source_clock: self.batch_clock.take(),
            ..ChangeBatch::default()
        };
        batch.coalesced = batch.raw_events.saturating_sub(touched.len() as u64);
        self.totals.raw_events += batch.raw_events;
        self.totals.coalesced += batch.coalesced;
        self.totals.overflows += batch.overflows;
        self.totals.raw_dropped += batch.raw_dropped;
        self.totals.source_clock = max_clock(self.totals.source_clock, batch.source_clock);
        let ephemeral = |path: &Path, touched: &BTreeMap<PathBuf, Touch>| {
            touched.get(path).is_some_and(|t| t.first == First::Created)
        };
        for (path, touch) in &touched {
            if exists(path) {
                batch.changed.push(path.clone());
                self.reported.insert(path.clone());
            } else if touch.first != First::Created || self.reported.remove(path) {
                self.reported.remove(path);
                batch.removed.push(path.clone());
            }
        }
        for (from, to) in renames {
            if from == to || ephemeral(&from, &touched) || !exists(&to) || exists(&from) {
                continue;
            }
            self.reported.remove(&from);
            self.reported.insert(to.clone());
            batch.renamed.push(Rename { from, to });
        }
        batch.renamed.sort_by(|a, b| a.from.cmp(&b.from));
        batch.renamed.dedup();
        batch
    }
}

fn max_clock(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

fn exists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}
