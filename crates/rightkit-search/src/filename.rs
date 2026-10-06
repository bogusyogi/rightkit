//! In-memory filename index (from Cockpit `storage_browser::search_filenames`),
//! decoupled from its scan report: callers add entries, then query by
//! case-folded basename substring, extension, kind and logical size, with
//! deterministic ordering and bounded pagination.

use std::fmt;
use std::path::{Path, PathBuf};

pub const MAX_QUERY_LENGTH: usize = 256;
pub const MAX_EXTENSION_LENGTH: usize = 64;
pub const MAX_PAGE_LIMIT: usize = 1_000;
pub const MAX_PAGE_OFFSET: usize = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub kind: Kind,
    /// Unknown sizes never satisfy a size predicate.
    pub size: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Request {
    /// Empty matches every name, including hidden names.
    pub query: String,
    pub kind: Option<Kind>,
    /// With or without a leading dot; case-insensitive.
    pub extension: Option<String>,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    pub offset: usize,
    pub limit: usize,
}

impl Default for Request {
    fn default() -> Self {
        Self { query: String::new(), kind: None, extension: None, min_size: None, max_size: None, offset: 0, limit: 100 }
    }
}

impl Request {
    pub fn new(query: impl Into<String>, limit: usize) -> Self {
        Self { query: query.into(), limit, ..Self::default() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub items: Vec<Entry>,
    pub offset: usize,
    pub limit: usize,
    pub total_matches: usize,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    InvalidLimit(usize),
    InvalidOffset(usize),
    InvalidQuery(String),
    InvalidExtension(String),
    InvalidSizeRange,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimit(n) => write!(f, "limit {n} must be 1..={MAX_PAGE_LIMIT}"),
            Self::InvalidOffset(n) => write!(f, "offset {n} exceeds {MAX_PAGE_OFFSET}"),
            Self::InvalidQuery(m) | Self::InvalidExtension(m) => f.write_str(m),
            Self::InvalidSizeRange => write!(f, "min_size exceeds max_size"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Default)]
pub struct FilenameIndex {
    // (folded basename, entry), kept sorted by path.
    entries: Vec<(String, Entry)>,
    sorted: bool,
}

fn basename(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string_lossy().into_owned())
}

impl FilenameIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn insert(&mut self, entry: Entry) {
        self.entries.push((basename(&entry.path).to_lowercase(), entry));
        self.sorted = false;
    }

    /// Remove a path and everything below it; returns the count removed.
    pub fn remove_tree(&mut self, root: &Path) -> usize {
        let before = self.entries.len();
        self.entries.retain(|(_, e)| !e.path.starts_with(root));
        before - self.entries.len()
    }

    pub fn search(&mut self, req: &Request) -> Result<Page, Error> {
        validate(req)?;
        if !self.sorted {
            self.entries.sort_by(|a, b| a.1.path.cmp(&b.1.path));
            self.entries.dedup_by(|a, b| a.1.path == b.1.path);
            self.sorted = true;
        }
        let q = req.query.to_lowercase();
        let ext = req.extension.as_deref().map(|e| e.strip_prefix('.').unwrap_or(e).to_lowercase());
        let matches: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|(name, _)| name.contains(&q))
            .map(|(_, e)| e)
            .filter(|e| req.kind.is_none_or(|k| e.kind == k))
            .filter(|e| {
                ext.as_deref().is_none_or(|x| {
                    e.path.extension().is_some_and(|have| have.to_string_lossy().to_lowercase() == x)
                })
            })
            .filter(|e| req.min_size.is_none_or(|m| e.size.is_some_and(|s| s >= m)))
            .filter(|e| req.max_size.is_none_or(|m| e.size.is_some_and(|s| s <= m)))
            .collect();
        let total = matches.len();
        let end = req.offset.saturating_add(req.limit).min(total);
        let items = if req.offset >= total {
            Vec::new()
        } else {
            matches[req.offset..end].iter().map(|e| (*e).clone()).collect()
        };
        Ok(Page { items, offset: req.offset, limit: req.limit, total_matches: total, has_more: end < total })
    }
}

fn validate(req: &Request) -> Result<(), Error> {
    if req.limit == 0 || req.limit > MAX_PAGE_LIMIT {
        return Err(Error::InvalidLimit(req.limit));
    }
    if req.offset > MAX_PAGE_OFFSET {
        return Err(Error::InvalidOffset(req.offset));
    }
    if req.query.len() > MAX_QUERY_LENGTH || req.query.chars().count() > MAX_QUERY_LENGTH || req.query.contains('\0') {
        return Err(Error::InvalidQuery(format!("query must be at most {MAX_QUERY_LENGTH} chars and contain no NUL")));
    }
    if let Some(ext) = &req.extension {
        let t = ext.strip_prefix('.').unwrap_or(ext);
        if t.is_empty() || t.chars().count() > MAX_EXTENSION_LENGTH || t.contains(['/', '\\', '\0']) {
            return Err(Error::InvalidExtension("extension must be a non-empty filename suffix".into()));
        }
    }
    if let (Some(min), Some(max)) = (req.min_size, req.max_size) {
        if min > max {
            return Err(Error::InvalidSizeRange);
        }
    }
    Ok(())
}
