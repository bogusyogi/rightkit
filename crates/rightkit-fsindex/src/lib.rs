//! Live whole-disk file index: fuzzy name search plus folder sizes that are
//! correct for a storage app (64-bit, allocated vs logical, APFS clone and
//! hard-link aware), kept live by the platform change feed.
//!
//! ```no_run
//! use rightkit_fsindex::{Config, Index};
//! let index = Index::open(Config::new(vec!["/".into()])).unwrap();
//! let hits = index.search("invoice 2024").unwrap();
//! let size = index.folder_size("/Users/me/Library").unwrap();
//! println!("{} {}", hits.len(), size.allocated_unique);
//! ```
//!
//! Third-party code: see THIRD-PARTY-NOTICES (fsearch, MIT).

pub mod backend;
mod col;
mod config;
#[cfg(feature = "content")]
mod content;
mod error;
mod index;
mod names;
mod query;
mod store;

pub use config::*;
pub use error::{Error, Result};
pub use index::{
    Change, ChangeBatch, ChangeKind, EntryKind, Hit, Index, Item, RollupUpdate, RootStatus,
    SearchOptions, SizeInfo, SortKey, Status,
};
pub use query::Query;
