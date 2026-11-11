//! Where records live. A store keeps records and makes each move atomically; the rules that
//! decide whether a move is allowed are in [`crate::rules`], once, for every store.

use crate::error::Result;
use crate::model::{Change, Record, Request, State};

/// A store of approval records: in memory ([`crate::MemoryStore`]), in files
/// ([`crate::FileStore`], feature `file`), or an app's database implementing this trait.
///
/// Contract: [`Store::transition`] is the only way a record changes state, and it is a
/// compare-and-set: of two callers moving the same record from the same state, exactly one gets
/// `true`. Nothing else in a store decides anything.
pub trait Store {
    /// Adds `record` (normally pending). Its id must be new.
    fn insert(&self, record: &Record) -> Result<()>;

    /// The record with this id, whatever its state; `None` when there is none (never filed,
    /// or swept).
    fn get(&self, id: &str) -> Result<Option<Record>>;

    /// Pending records that can still be decided at `now_ms`, oldest first.
    fn pending(&self, now_ms: u64) -> Result<Vec<Record>>;

    /// Approved records whose grant still lives at `now_ms`, oldest first.
    fn approved(&self, now_ms: u64) -> Result<Vec<Record>>;

    /// Moves `id` from `from` as `change` says, only while it is in `from`. `true` only for the
    /// caller that made the move.
    fn transition(&self, id: &str, from: State, change: &Change) -> Result<bool>;

    /// Records a use of a session grant at `at_ms`. A store without idle grants may ignore it.
    fn touch(&self, id: &str, at_ms: u64) -> Result<()> {
        let _ = (id, at_ms);
        Ok(())
    }

    /// A waiting record that asks for the same thing as `like` ([`Request::same_ask`]).
    fn find_pending(&self, like: &Request, now_ms: u64) -> Result<Option<Record>> {
        Ok(self
            .pending(now_ms)?
            .into_iter()
            .find(|r| r.request.same_ask(like)))
    }

    /// How many records wait at `now_ms`: all of them, or only `client`'s.
    fn count_pending(&self, client: Option<&str>, now_ms: u64) -> Result<usize> {
        Ok(self
            .pending(now_ms)?
            .iter()
            .filter(|r| client.is_none_or(|c| r.request.origin.client == c))
            .count())
    }
}
