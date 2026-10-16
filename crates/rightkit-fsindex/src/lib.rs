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
mod gate;
mod index;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod limits;
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

/// macOS typed FSEvents watcher (0.2.0). See [`FsEventsWatcher`].
#[cfg(target_os = "macos")]
pub use backend::macos::events::{FsEvent, FsEventsWatcher, ItemFlags, WatchFlags, WatchOptions};
/// macOS bulk directory listing (0.2.0). Data-fork sizes, optional creation time,
/// and a typed refusal for dataless files. See [`list_dir_fd`].
#[cfg(target_os = "macos")]
pub use backend::macos::listing::{list_dir_fd, Entry, EntrySize, ListOptions, Listing};

/// Windows Search (SystemIndex) queries, status, and the `Backend` over them (0.3.0). The
/// pure parts are available on every platform; COM calls report `Unsupported` off Windows.
pub use backend::windows_search::{
    escape_sql_literal, folder_rollup, like_pattern, search_names, windows_search_status,
    ServiceState, WindowsRollup, WindowsSearchBackend, WindowsSearchHit, WindowsSearchStatus,
};
/// NTFS USN journal reader (0.3.0): record parsing everywhere, `UsnJournal` on Windows.
pub use backend::windows_usn::{
    parse_usn_records, UsnError, UsnJournalInfo, UsnRecord, UsnRecords,
};
#[cfg(windows)]
pub use backend::windows_usn::UsnJournal;
