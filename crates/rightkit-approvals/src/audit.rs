//! Audit before effect: a store wrapper that makes the app's audit row durable before each
//! filing and each move, so no approval changes state without its row.

use crate::error::Result;
use crate::model::{Change, Record, Request, State};
use crate::store::Store;

/// The app's audit log. What a row holds and where it goes (a table, a log file) is the app's.
pub trait Audit {
    /// Makes the row for `record` durable before it changes: `change` is the move about to be
    /// made, `None` when the record is about to be filed. An `Err` stops the filing or the move,
    /// and the record stays as it was.
    fn append(&self, record: &Record, change: Option<&Change>) -> Result<()>;
}

/// A [`Store`] whose every filing and move first appends its row to an [`Audit`]; a failed
/// append changes nothing. A row records an attempted move: another mover can still win the
/// compare-and-set after the row is written, so a row says the move was tried, the record's
/// state says whether it happened. Reads and [`Store::touch`] are not audited.
pub struct Audited<S, A> {
    store: S,
    audit: A,
}

impl<S: Store, A: Audit> Audited<S, A> {
    pub fn new(store: S, audit: A) -> Audited<S, A> {
        Audited { store, audit }
    }

    /// The wrapped store, for its own calls (unaudited).
    pub fn store(&self) -> &S {
        &self.store
    }

    pub fn audit(&self) -> &A {
        &self.audit
    }
}

impl<S: Store, A: Audit> Store for Audited<S, A> {
    fn insert(&self, record: &Record) -> Result<()> {
        self.audit.append(record, None)?;
        self.store.insert(record)
    }

    fn get(&self, id: &str) -> Result<Option<Record>> {
        self.store.get(id)
    }

    fn pending(&self, now_ms: u64) -> Result<Vec<Record>> {
        self.store.pending(now_ms)
    }

    fn approved(&self, now_ms: u64) -> Result<Vec<Record>> {
        self.store.approved(now_ms)
    }

    fn claimed(&self) -> Result<Vec<Record>> {
        self.store.claimed()
    }

    fn transition(&self, id: &str, from: State, change: &Change) -> Result<bool> {
        let Some(record) = self.store.get(id)? else {
            return Ok(false);
        };
        if record.state != from {
            return Ok(false);
        }
        self.audit.append(&record, Some(change))?;
        self.store.transition(id, from, change)
    }

    fn touch(&self, id: &str, at_ms: u64) -> Result<()> {
        self.store.touch(id, at_ms)
    }

    fn find_pending(&self, like: &Request, now_ms: u64) -> Result<Option<Record>> {
        self.store.find_pending(like, now_ms)
    }

    fn count_pending(&self, client: Option<&str>, now_ms: u64) -> Result<usize> {
        self.store.count_pending(client, now_ms)
    }
}
