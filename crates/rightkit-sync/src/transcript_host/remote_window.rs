//! A bound on how much of the remote change feed one pass holds.
//!
//! The engine fetches every page of the change feed before it decodes, merges or applies any of
//! it (`fetch_all`), so a first sync of a large zone would hold the whole zone in memory. The
//! helper bounds one page (`MAX_RESPONSE_ENTRIES`, response bytes); nothing bounds how many pages
//! one pass accumulates. [`WindowedBackend`] sits between the engine and the real backend and ends
//! the feed early: once the pages already handed to the engine in this pass reach
//! [`MAX_PASS_RECORDS`] records, [`MAX_PASS_BYTES`] bytes or [`MAX_PASS_PAGES`] pages, the page
//! that crossed the limit is passed on with `more_coming` cleared. A page that says more is
//! coming without a cursor, or with the cursor it was requested with, also ends the feed (the
//! pass is marked stalled; the host then reports `StatusReport::FeedStalled`, keeps the recovery
//! flag, and reschedules only with backoff); inside one pass a cursor that repeats an older value
//! is bounded by the page cap. Across passes the host remembers the cursors passes ended on and
//! treats a repeat as stalled (`FeedWatch` in `sync.rs`); this module only reports the token the
//! pass ended on (`WindowTotals::end_token`). The engine applies what it has and stores that
//! page's change token as the cursor, so the next pass continues from the next page. Nothing is
//! skipped and nothing is dropped: the cursor only advances past pages that were applied.
//!
//! The first page of a pass is always delivered whole, so every pass makes progress and one pass
//! holds at most the limit plus one page. A page is bounded by the helper (100,000 entries and
//! 128 MiB per response, crate patch 0004).
//!
//! A change token is the cursor the backend itself returned for that page; cutting the feed there
//! is what `more_coming` already means to the engine between two fetches. A page without a token
//! is never cut at, because the engine would resume from the older token and repeat the page.

use std::sync::{Arc, Mutex};

use crate::{Backend, ChangePage, FieldValue, RawRecord, RecordOutcome, SavePolicy, SyncResult};

/// Records (live, tombstone and hard-deleted names) one pass may hold before the feed is cut.
pub(super) const MAX_PASS_RECORDS: usize = 20_000;
/// Approximate bytes of record content one pass may hold before the feed is cut.
pub(super) const MAX_PASS_BYTES: usize = 48 * 1024 * 1024;
/// Pages one pass may take from the feed, whatever their size. Bounds a feed of empty or tiny
/// pages, which the record and byte totals never stop.
pub(super) const MAX_PASS_PAGES: usize = 256;

#[derive(Default)]
struct WindowState {
    records: usize,
    bytes: usize,
    /// The token the next call of this pass must pass as `since` to continue the same feed:
    /// `Some(token)` is the token of the last page handed out (`Some(None)`: none yet).
    expect: Option<Option<Vec<u8>>>,
    truncated: bool,
    /// Pages taken from the current feed in this pass.
    pages: usize,
    /// The feed said more pages follow but gave no cursor, or the cursor it gave does not move:
    /// the pass stops there and no further pass is requested on its account.
    stalled: bool,
}

/// What one pass took from the feed.
pub(super) struct WindowTotals {
    /// The feed was cut: more pages remain for the next pass.
    pub(super) truncated: bool,
    /// The feed claimed more pages without a new, moving cursor; the pass stopped safely.
    pub(super) stalled: bool,
    /// The change token the pass ended on: the token of the last page handed to the engine (the
    /// cursor the engine stores), or the token the pass started from when the last page had none.
    /// `None` when the pass took no page. The host keeps digests of these across passes to spot a
    /// feed whose cursors cycle (see `FeedWatch` in `sync.rs`).
    pub(super) end_token: Option<Vec<u8>>,
}

/// Shared between the instance and the backend wrapper.
pub(super) struct RemoteWindow {
    state: Mutex<WindowState>,
}

impl RemoteWindow {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(WindowState::default()),
        }
    }

    pub(super) fn begin_pass(&self) {
        if let Ok(mut state) = self.state.lock() {
            *state = WindowState::default();
        }
    }

    pub(super) fn finish_pass(&self) -> WindowTotals {
        // A poisoned lock cannot be read as "the feed ended cleanly": report it as stalled, so
        // the pass is never shown as complete on evidence that could not be read.
        self.state.lock().map_or(
            WindowTotals {
                truncated: false,
                stalled: true,
                end_token: None,
            },
            |state| WindowTotals {
                truncated: state.truncated,
                stalled: state.stalled,
                end_token: state.expect.clone().flatten(),
            },
        )
    }

    fn account(&self, since: Option<&[u8]>, page: &mut ChangePage) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        // A call that does not continue the last page's token starts a new feed (the engine
        // restarted after an expired token or a recreated zone): what was counted belongs to a
        // feed the engine dropped.
        let continuing = matches!(&state.expect, Some(previous) if previous.as_deref() == since);
        if !continuing {
            state.records = 0;
            state.bytes = 0;
            state.truncated = false;
            state.pages = 0;
            state.stalled = false;
        }
        state.pages = state.pages.saturating_add(1);
        let content_bytes = page
            .records
            .iter()
            .map(record_bytes)
            .fold(0usize, usize::saturating_add);
        let name_bytes = page
            .deleted_record_names
            .iter()
            .map(String::len)
            .fold(0usize, usize::saturating_add);
        state.records = state
            .records
            .saturating_add(page.records.len())
            .saturating_add(page.deleted_record_names.len());
        state.bytes = state
            .bytes
            .saturating_add(content_bytes)
            .saturating_add(name_bytes);
        state.expect = Some(page.new_token.clone().or_else(|| since.map(<[u8]>::to_vec)));
        if page.more_coming {
            // "More coming" is accepted only with a cursor that exists and differs from the one
            // this page was asked with. Without one the next request would repeat this page for
            // ever: stop here. The engine applies what it has; the cursor it stores is this
            // page's own (or none), so nothing is skipped, and no further pass is requested.
            let progressing = match (page.new_token.as_deref(), since) {
                (None, _) => false,
                (Some(new), Some(previous)) => new != previous,
                (Some(_), None) => true,
            };
            if !progressing {
                page.more_coming = false;
                state.stalled = true;
                state.truncated = false;
                return;
            }
            let full = state.records >= MAX_PASS_RECORDS
                || state.bytes >= MAX_PASS_BYTES
                || state.pages >= MAX_PASS_PAGES;
            if full {
                page.more_coming = false;
                state.truncated = true;
            }
        }
    }
}

/// Approximate bytes of one record: names, field keys and field values.
fn record_bytes(record: &RawRecord) -> usize {
    let fields = record.fields.iter().fold(0usize, |total, (key, value)| {
        let value_bytes = match value {
            FieldValue::Text(text) => text.len(),
            FieldValue::Bytes(bytes) => bytes.len(),
            FieldValue::Strings(list) => {
                list.iter().map(String::len).fold(0, usize::saturating_add)
            }
            FieldValue::I64(_) | FieldValue::F64(_) | FieldValue::Date(_) => 8,
        };
        total.saturating_add(key.len()).saturating_add(value_bytes)
    });
    fields
        .saturating_add(record.record_name.len())
        .saturating_add(record.record_type.len())
        .saturating_add(64)
}

/// The engine's backend with the feed bounded; every other call is passed through.
pub(super) struct WindowedBackend {
    inner: Arc<dyn Backend>,
    window: Arc<RemoteWindow>,
}

impl WindowedBackend {
    pub(super) fn new(inner: Arc<dyn Backend>, window: Arc<RemoteWindow>) -> Self {
        Self { inner, window }
    }
}

impl Backend for WindowedBackend {
    fn prepare(&self) -> SyncResult<()> {
        self.inner.prepare()
    }

    fn fetch_changes(&self, since: Option<&[u8]>) -> SyncResult<ChangePage> {
        let mut page = self.inner.fetch_changes(since)?;
        self.window.account(since, &mut page);
        Ok(page)
    }

    fn fetch_record(&self, record_name: &str) -> SyncResult<Option<RawRecord>> {
        self.inner.fetch_record(record_name)
    }

    fn save_records(
        &self,
        records: &[RawRecord],
        policy: SavePolicy,
    ) -> SyncResult<Vec<RecordOutcome>> {
        self.inner.save_records(records, policy)
    }

    fn max_batch(&self) -> usize {
        self.inner.max_batch()
    }
}
