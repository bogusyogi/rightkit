//! Cooperative cancellation for requests that have no persisted job row, plus a process-local
//! activity registry: the same registrations can carry a label, a progress fraction and a message,
//! and can be listed, so a UI shows what is running and cancels it by id.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::{JobError, JobResult, JOB_EXISTS};

struct Entry {
    /// Unique per registration, so a stale guard never removes a later registration of its id.
    token: u64,
    flag: Arc<AtomicBool>,
    label: Option<String>,
    message: Option<String>,
    progress: Option<f64>,
    counts: Option<Counts>,
    kind: Option<String>,
    cancellable: bool,
    started: Instant,
}

/// The unit a [`Counts`] pair is worded in (`3 of 25`, `12 of 340 MB`, `40 %`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CountUnit {
    /// Plain items.
    Count,
    /// Bytes; a listing may show them scaled.
    Bytes,
    /// Percent of the total; `done` is read against 100 when `total` is 0.
    Percent,
}

/// Work done out of a total, in one unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Counts {
    /// Units completed so far.
    pub done: u64,
    /// Units in the whole task.
    pub total: u64,
    /// The unit `done` and `total` are counted in.
    pub unit: CountUnit,
}

#[derive(Default)]
struct State {
    next_token: u64,
    map: HashMap<String, Entry>,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    revision: AtomicU64,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn bump(&self) {
        self.revision.fetch_add(1, Ordering::SeqCst);
    }
}

/// How to register an activity ([`InlineCancelRegistry::register_with`]). Every field is optional;
/// the default behaves like [`InlineCancelRegistry::register`].
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct ActivityOptions {
    /// Human-readable name shown in a listing.
    pub label: Option<String>,
    /// Initial status message.
    pub message: Option<String>,
    /// Initial progress fraction, `0.0..=1.0` (clamped; NaN is ignored).
    pub progress: Option<f64>,
    /// Use this flag as the cancellation flag instead of a fresh one, so work that already holds
    /// one (a UI cancel token) is cancelled through the registry. It is used as it stands: a flag
    /// that is already set reads as cancelled from the first listing.
    pub flag: Option<Arc<AtomicBool>>,
    /// Done and total counts in a unit, shown next to the label.
    pub counts: Option<Counts>,
    /// What kind of work this is (for example `import`, `export`), for grouping in a listing.
    pub kind: Option<String>,
    /// Whether a cancel control is offered for this activity. Defaults to true; it is a
    /// listing attribute and does not gate [`InlineCancelRegistry::cancel`].
    pub cancellable: Option<bool>,
}

impl ActivityOptions {
    /// Options with nothing set.
    pub fn new() -> Self {
        Self::default()
    }
    /// Sets the listing label.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
    /// Sets the initial status message.
    pub fn message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }
    /// Sets the initial progress fraction.
    pub fn progress(mut self, fraction: f64) -> Self {
        self.progress = Some(fraction);
        self
    }
    /// Registers `flag` as the cancellation flag.
    pub fn cancel_flag(mut self, flag: Arc<AtomicBool>) -> Self {
        self.flag = Some(flag);
        self
    }
    /// Sets the done and total counts and their unit.
    pub fn counts(mut self, done: u64, total: u64, unit: CountUnit) -> Self {
        self.counts = Some(Counts { done, total, unit });
        self
    }
    /// Sets the kind of work.
    pub fn kind(mut self, kind: impl Into<String>) -> Self {
        self.kind = Some(kind.into());
        self
    }
    /// Sets whether a cancel control is offered.
    pub fn cancellable(mut self, yes: bool) -> Self {
        self.cancellable = Some(yes);
        self
    }
}

/// One registered activity as of a listing ([`InlineCancelRegistry::list`] / `get`).
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ActivitySnapshot {
    /// The registered job id.
    pub id: String,
    /// Registration order within the registry (first registered is lowest); unique per
    /// registration, so a reused id gets a new value.
    pub seq: u64,
    pub label: Option<String>,
    pub message: Option<String>,
    /// Progress fraction `0.0..=1.0`, when the work reported one.
    pub progress: Option<f64>,
    /// True once cancellation was requested (the flag reads set).
    pub cancelled: bool,
    /// Time since registration.
    pub elapsed: Duration,
    /// Done and total counts with their unit, when the work reported them.
    pub counts: Option<Counts>,
    /// The kind of work, when one was given.
    pub kind: Option<String>,
    /// Whether a cancel control is offered (see [`ActivityOptions::cancellable`]).
    pub cancellable: bool,
}

fn clean_progress(p: Option<f64>) -> Option<f64> {
    p.filter(|v| !v.is_nan()).map(|v| v.clamp(0.0, 1.0))
}

/// Cloneable, process-local registry for inline requests. Clones share registrations.
///
/// Keep each returned [`InlineJobGuard`] alive until work finishes. Cancellation sets
/// its `Arc<AtomicBool>`; work must observe that flag cooperatively. Nothing is persisted.
///
/// It doubles as an activity registry: [`InlineCancelRegistry::register_with`] attaches a label,
/// progress and message, the guard updates them, [`InlineCancelRegistry::list`] reads them, and
/// [`InlineCancelRegistry::revision`] changes whenever anything listed does, so a UI can poll it
/// cheaply and re-list only on change.
#[derive(Clone, Default)]
pub struct InlineCancelRegistry {
    shared: Arc<Shared>,
}

/// Same registry under the name an app uses when it shows the entries (see
/// [`InlineCancelRegistry::list`]).
pub type ActivityRegistry = InlineCancelRegistry;

impl InlineCancelRegistry {
    /// Creates an independent registry. Share it (or clones) across request handlers.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `job_id`, rejecting an active duplicate with [`JOB_EXISTS`].
    /// Empty IDs return `bad_input`. Dropping the guard releases the ID, including on unwind.
    pub fn register(&self, job_id: &str) -> JobResult<(InlineJobGuard, Arc<AtomicBool>)> {
        self.register_with(job_id, ActivityOptions::default())
    }

    /// As [`InlineCancelRegistry::register`], with a label, initial progress and message, and
    /// optionally a caller-supplied cancellation flag. The returned flag is the one registered.
    pub fn register_with(
        &self,
        job_id: &str,
        options: ActivityOptions,
    ) -> JobResult<(InlineJobGuard, Arc<AtomicBool>)> {
        if job_id.is_empty() {
            return Err(JobError::new(
                "bad_input",
                "inline job id must not be empty",
            ));
        }
        let mut state = self.shared.lock();
        if state.map.contains_key(job_id) {
            return Err(JobError::new(
                JOB_EXISTS,
                format!("job {job_id} is already running"),
            ));
        }
        let flag = options
            .flag
            .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
        state.next_token += 1;
        let token = state.next_token;
        state.map.insert(
            job_id.to_owned(),
            Entry {
                token,
                flag: Arc::clone(&flag),
                label: options.label,
                message: options.message,
                progress: clean_progress(options.progress),
                counts: options.counts,
                kind: options.kind,
                cancellable: options.cancellable.unwrap_or(true),
                started: Instant::now(),
            },
        );
        drop(state);
        self.shared.bump();
        Ok((
            InlineJobGuard {
                shared: Arc::clone(&self.shared),
                job_id: job_id.to_owned(),
                token,
            },
            flag,
        ))
    }

    /// Requests cancellation; false when no request with this ID is registered.
    /// Repeated cancellation returns true until its guard drops.
    pub fn cancel(&self, job_id: &str) -> bool {
        let state = self.shared.lock();
        if let Some(entry) = state.map.get(job_id) {
            entry.flag.store(true, Ordering::SeqCst);
            drop(state);
            self.shared.bump();
            true
        } else {
            false
        }
    }

    /// Cancels all current registrations; returns their count. Future requests start uncancelled.
    pub fn cancel_all(&self) -> usize {
        let state = self.shared.lock();
        for entry in state.map.values() {
            entry.flag.store(true, Ordering::SeqCst);
        }
        let n = state.map.len();
        drop(state);
        if n > 0 {
            self.shared.bump();
        }
        n
    }

    /// The registered activities in registration order (`seq`). A snapshot: it does not follow
    /// later changes.
    pub fn list(&self) -> Vec<ActivitySnapshot> {
        let state = self.shared.lock();
        let mut all: Vec<ActivitySnapshot> = state
            .map
            .iter()
            .map(|(id, entry)| snapshot(id, entry))
            .collect();
        drop(state);
        all.sort_by_key(|a| a.seq);
        all
    }

    /// One registered activity, or `None` when `job_id` is not registered.
    pub fn get(&self, job_id: &str) -> Option<ActivitySnapshot> {
        let state = self.shared.lock();
        state.map.get(job_id).map(|entry| snapshot(job_id, entry))
    }

    /// Number of registered activities.
    pub fn len(&self) -> usize {
        self.shared.lock().map.len()
    }

    /// True when nothing is registered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// A counter that rises on every register, release, cancel and label/progress/message
    /// change. Equal values at two reads mean the listing did not change between them, except
    /// that a flag set directly through a held `Arc<AtomicBool>` (not through
    /// [`InlineCancelRegistry::cancel`]) and the `elapsed` field are not tracked.
    pub fn revision(&self) -> u64 {
        self.shared.revision.load(Ordering::SeqCst)
    }
}

fn snapshot(id: &str, entry: &Entry) -> ActivitySnapshot {
    ActivitySnapshot {
        id: id.to_owned(),
        seq: entry.token,
        label: entry.label.clone(),
        message: entry.message.clone(),
        progress: entry.progress,
        cancelled: entry.flag.load(Ordering::SeqCst),
        elapsed: entry.started.elapsed(),
        counts: entry.counts,
        kind: entry.kind.clone(),
        cancellable: entry.cancellable,
    }
}

/// Registration lifetime. Drop removes its entry without resetting cancellation flags
/// already held by work. This guard is intentionally not cloneable.
#[must_use = "keep this guard alive until inline work finishes"]
pub struct InlineJobGuard {
    shared: Arc<Shared>,
    job_id: String,
    token: u64,
}

impl InlineJobGuard {
    /// Registered ID.
    pub fn job_id(&self) -> &str {
        &self.job_id
    }

    /// Sets or clears the listing label.
    pub fn set_label(&self, label: Option<&str>) {
        self.update(|e| e.label = label.map(str::to_owned));
    }

    /// Sets or clears the status message.
    pub fn set_message(&self, message: Option<&str>) {
        self.update(|e| e.message = message.map(str::to_owned));
    }

    /// Sets or clears the progress fraction (clamped to `0.0..=1.0`; NaN clears it).
    pub fn set_progress(&self, fraction: Option<f64>) {
        self.update(|e| e.progress = clean_progress(fraction));
    }

    /// Sets the done and total counts in a unit; `None` clears them.
    pub fn set_counts(&self, counts: Option<Counts>) {
        self.update(|e| e.counts = counts);
    }

    /// Switches the cancel control for this activity on or off (listing attribute only; see
    /// [`ActivityOptions::cancellable`]).
    pub fn set_cancellable(&self, yes: bool) {
        self.update(|e| e.cancellable = yes);
    }

    /// Sets progress and message together, as one change.
    pub fn report(&self, fraction: Option<f64>, message: Option<&str>) {
        self.update(|e| {
            e.progress = clean_progress(fraction);
            e.message = message.map(str::to_owned);
        });
    }

    fn update(&self, change: impl FnOnce(&mut Entry)) {
        let mut state = self.shared.lock();
        if let Some(entry) = state
            .map
            .get_mut(&self.job_id)
            .filter(|e| e.token == self.token)
        {
            change(entry);
            drop(state);
            self.shared.bump();
        }
    }
}

impl Drop for InlineJobGuard {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        if state
            .map
            .get(&self.job_id)
            .is_some_and(|e| e.token == self.token)
        {
            state.map.remove(&self.job_id);
            drop(state);
            self.shared.bump();
        }
    }
}
