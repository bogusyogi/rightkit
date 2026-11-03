//! `rightkit-mail-store`: the mail store over SQLCipher.
//!
//! - [`db`]: opening the encrypted database (key candidates, lifetime lock, migration gate with
//!   pre-migration backup), the versioned schema, accounts and labels, thread listing and
//!   search, the outbox and pending-operation queues, recipients, the sync apply path and the
//!   local semantic index (sqlite-vec plus a shared embedder).
//! - [`db_schema`] and [`db_schema_v21`]: the schema SQL and version.
//! - [`attach_cache`]: the shared attachment and remote-image cache set.
//! - [`host`]: what the app supplies (identity, key storage, data folder, build channel).
//!
//! Search compilation and planning are `rightkit-mail`'s; Gmail wire types are
//! `rightkit-google-apis`'s.

pub mod attach_cache;
pub mod db;
pub mod db_schema;
pub mod db_schema_v21;
pub mod host;
pub mod safety;
mod utc_stamp;

#[cfg(test)]
mod test_host;
