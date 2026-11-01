//! One file-search result row, mirroring the donor `codex_file_search::FileMatch`
//! shape so the lifted `file_search_popup` state machine is unchanged. The
//! parent supplies instances from the canonical repository file-search source.

use std::path::PathBuf;

/// A fuzzy file match, verbatim from `codex_file_search` @ 444da310.
#[derive(Clone, Debug, PartialEq)]
pub struct FileMatch {
    pub score: u32,
    /// Path relative to the search root; this is the insertion payload.
    pub path: PathBuf,
    pub match_type: MatchType,
    pub root: PathBuf,
    /// Sorted & deduplicated display-string char indices that matched, when present.
    pub indices: Option<Vec<u32>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchType {
    File,
    Directory,
}
