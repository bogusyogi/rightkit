//! Bounded scans over a source that cannot be filtered in SQL (for example body text
//! read from files). Each call examines at most a fixed number of rows and returns a
//! resumable cursor, so a large corpus never loads in one go.

use crate::page::Cursor;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

/// Limits for one [`scan_page`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanBudget {
    max_rows: usize,
    chunk: usize,
}

impl ScanBudget {
    /// `max_rows` bounds the rows examined per call; `chunk` bounds the rows requested
    /// per `fetch`. Both are raised to at least 1 so every call makes progress.
    pub fn new(max_rows: usize, chunk: usize) -> Self {
        Self {
            max_rows: max_rows.max(1),
            chunk: chunk.max(1),
        }
    }
}

/// Result of one [`scan_page`] call.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ScanPage<T> {
    /// Matching rows, at most `limit`, in source order.
    pub hits: Vec<T>,
    /// Resume point: the key of the last row examined when the page filled or the
    /// budget ended. `None` only when the source returned fewer rows than requested.
    pub next: Option<Cursor>,
    /// Rows examined by this call, matching or not.
    pub scanned: usize,
}

/// Scan one page of a source in key order.
///
/// `fetch(after, n)` returns up to `n` rows strictly after `after` (or from the start
/// when `None`). `key` gives a row's position in that order. `matches` decides whether
/// a row is a hit. The call returns when `limit` hits are found, when `budget` rows
/// have been examined, or when `fetch` returns fewer rows than requested. A `fetch`
/// error is returned as-is and discards the hits gathered so far.
///
/// `limit` is raised to at least 1.
pub fn scan_page<T, E>(
    after: Option<Cursor>,
    limit: usize,
    budget: ScanBudget,
    mut fetch: impl FnMut(Option<&Cursor>, usize) -> Result<Vec<T>, E>,
    key: impl Fn(&T) -> Cursor,
    mut matches: impl FnMut(&T) -> bool,
) -> Result<ScanPage<T>, E> {
    let limit = limit.max(1);
    let mut cursor = after;
    let mut hits = Vec::new();
    let mut scanned = 0usize;
    loop {
        // Invariant: `scanned < budget.max_rows`, so `want >= 1`.
        let want = budget.chunk.min(budget.max_rows - scanned);
        let rows = fetch(cursor.as_ref(), want)?;
        let short = rows.len() < want;
        for row in rows {
            scanned += 1;
            let k = key(&row);
            let is_hit = matches(&row);
            if is_hit {
                hits.push(row);
            }
            if (is_hit && hits.len() == limit) || scanned >= budget.max_rows {
                return Ok(ScanPage {
                    hits,
                    next: Some(k),
                    scanned,
                });
            }
            cursor = Some(k);
        }
        if short {
            return Ok(ScanPage {
                hits,
                next: None,
                scanned,
            });
        }
    }
}

/// Read at most `max_bytes` from the start of `path` as text.
///
/// An incomplete UTF-8 sequence at the cut is dropped, so the result never ends in a
/// partial character. Invalid bytes elsewhere are replaced with U+FFFD.
pub fn read_text_prefix(path: &Path, max_bytes: u64) -> io::Result<String> {
    let mut buf = Vec::new();
    File::open(path)?.take(max_bytes).read_to_end(&mut buf)?;
    trim_incomplete_tail(&mut buf);
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Remove a trailing lead byte whose sequence is cut off by the end of `buf`.
fn trim_incomplete_tail(buf: &mut Vec<u8>) {
    let n = buf.len();
    for back in 1..=n.min(3) {
        let b = buf[n - back];
        if b & 0b1100_0000 == 0b1000_0000 {
            // Continuation byte: keep looking for the lead byte.
            continue;
        }
        let need = if b < 0x80 {
            1
        } else if b >> 5 == 0b110 {
            2
        } else if b >> 4 == 0b1110 {
            3
        } else if b >> 3 == 0b11110 {
            4
        } else {
            1
        };
        if need > back {
            buf.truncate(n - back);
        }
        return;
    }
}
