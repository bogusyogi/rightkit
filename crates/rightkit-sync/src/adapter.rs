//! One generic [`Adapter`] that turns a family's rows into the engine's [`LocalStore`] over [`FamilyItem`] (feature `cipher`, with
//! [`crate::families`]).
//!
//! A host writes a small [`Rows`] implementation over the store it already has (memories, profile entries, the Inbox, facts,
//! standing instructions, cursors, schedules). The wire form is [`FamilyItem`]: an id every computer derives the same way
//! ([`crate::families::item_id`]), the two times last-writer-wins orders by, and an opaque body that travels byte for byte.
//!
//! The host decides what a row's body is and which rules apply when a remote row lands (`Rows::put`); this module only moves rows
//! to and from the engine. No row content is logged here.

use crate::families::FamilyItem;
use crate::{ApplyOutcome, LocalStore, SyncError, SyncResult};

/// One row of a family, before it is a [`FamilyItem`].
pub struct Row {
    pub id: String,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub body: Vec<u8>,
}

/// A family's local store, in its own terms.
pub trait Rows: Send + Sync {
    /// The number of this family's body layout.
    fn schema(&self) -> u32;
    fn all(&self) -> Result<Vec<Row>, String>;
    /// Lands rows that came from another computer. Last-writer-wins and the family's own rules are applied here.
    fn put(&self, rows: &[Row]) -> Result<(), String>;
    /// Removes the row with sync item id `id` (a tombstone arrived). Idempotent.
    fn remove(&self, id: &str) -> Result<(), String>;
}

/// The engine's `LocalStore` over any family's rows.
pub struct Adapter<R: Rows> {
    rows: R,
    device: String,
}

impl<R: Rows> Adapter<R> {
    pub fn new(rows: R, device: &str) -> Self {
        Adapter {
            rows,
            device: device.to_owned(),
        }
    }
}

fn persistence(message: String) -> SyncError {
    SyncError::Persistence(message)
}

impl<R: Rows> LocalStore<FamilyItem> for Adapter<R> {
    fn all(&self) -> SyncResult<Vec<FamilyItem>> {
        self.rows
            .all()
            .map_err(persistence)?
            .into_iter()
            .map(|row| {
                FamilyItem::new(
                    &row.id,
                    row.created_ms,
                    row.updated_ms,
                    Some(self.device.clone()),
                    self.rows.schema(),
                    row.body,
                )
                .map_err(persistence)
            })
            .collect()
    }

    fn remove(&self, id: &str) -> SyncResult<()> {
        self.rows.remove(id).map_err(persistence)
    }

    fn apply_remote(&self, items: &[FamilyItem]) -> SyncResult<Vec<ApplyOutcome>> {
        let rows: Vec<Row> = items
            .iter()
            .map(|item| Row {
                id: item.id().to_owned(),
                created_ms: item.created_at_ms,
                updated_ms: item.updated_at_ms,
                body: item.body.clone(),
            })
            .collect();
        self.rows.put(&rows).map_err(persistence)?;
        Ok(items.iter().map(|_| ApplyOutcome::Applied).collect())
    }
}
