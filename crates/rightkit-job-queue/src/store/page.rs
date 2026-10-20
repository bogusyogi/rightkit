//! Keyset paging for [`JobStore::list_page`].
//!
//! A listing is ordered newest first by `(created_at DESC, rowid DESC)`. Both columns are
//! written once, when the row is inserted, and `rowid` is unique, so the pair is a total order
//! that no later change to a row can move. A page ends with the key of its last row; the next
//! page is "every row strictly below that key" (`(created_at, rowid) < (?, ?)`), which SQLite
//! answers with a range seek on the `rk_jobs_created` / `rk_jobs_status_created` indexes. There
//! is no `OFFSET` anywhere.
//!
//! Because the cursor is a position and not a row, it stays valid when the row it was cut from
//! is deleted, and a row that is inserted, changed or deleted between two pages can neither be
//! returned twice nor push another row out of the walk:
//!
//! - a row that existed for the whole walk and did not change appears exactly once;
//! - a row inserted after the walk began sorts above every earlier row (or, for an imported row
//!   with an older `created_at`, lands somewhere in the order) and is returned at most once;
//! - a row whose status changes may be seen in its old or its new state, and under a `status`
//!   filter may be missed if it moves into the filter behind the cursor.
//!
//! Known limit: `rowid` is the tie-breaker the listing has always used (`jobs.job_id` is a text
//! key, so SQLite gives the table an implicit rowid). `VACUUM` is allowed to renumber such
//! rowids, so a vacuum that runs between two pages of one walk can repeat or skip rows that
//! share a `created_at` second with the cursor. Nothing in this crate vacuums.

use std::fmt;
use std::str::FromStr;

use rightkit_db::rusqlite::params_from_iter;
use rightkit_db::rusqlite::types::Value as SqlValue;
use serde::{Deserialize, Serialize};

use super::{JobRecord, JobStore, ListFilter, NCORE};
use crate::error::{JobError, JobResult, CURSOR_MISMATCH, INVALID_CURSOR};

/// Cursor wire version.
const VERSION: &str = "rk1";
/// Sort tag of the one order a listing has: newest first, `created_at DESC, rowid DESC`.
const SORT_NEWEST_FIRST: &str = "cd";
// No fixed length cap. A cursor hex-encodes the `status` and `request_id` filter values and the
// row's `created_at`, none of which has a length bound of its own, so any cap shorter than the
// longest listing the store accepts would refuse a cursor this crate issued (the walk would die
// at its first page). Same rule as the `JobTable` query cursor in `store/query.rs`: the decoder
// accepts exactly the shape `Cursor::after_row` issues, and decoding is linear in a string the
// caller already holds.

/// An opaque position in a paged listing (see [`JobStore::list_page`]).
///
/// A cursor is a string: [`Cursor::as_str`] / `Display` give it, [`Cursor::parse`] / `FromStr`
/// read it back, and with `serde` it is a plain JSON string, so an app can hand it to its UI and
/// take it back. It is bound to the filter and the sort order it was issued for; passing it to a
/// listing with a different `status` or `request_id` fails with `cursor_mismatch`
/// ([`crate::CURSOR_MISMATCH`]). It is not a secret and not signed: it only says "after this
/// row", and anyone who can list the jobs can already see every row.
///
/// The text format is not a contract; treat it as opaque. Cursors do not expire.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Cursor(String);

/// One page of a listing: the rows and where the next page starts.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ListPage {
    /// Up to `limit` rows, newest first.
    pub records: Vec<JobRecord>,
    /// Pass to [`JobStore::list_page`] with the same filter to read the next page. `None` when
    /// this page is the last one: no more row matched when the page was read.
    pub next: Option<Cursor>,
}

/// A decoded cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Position {
    pub(super) sort: String,
    pub(super) status: Option<String>,
    pub(super) request_id: Option<String>,
    pub(super) created_at: String,
    pub(super) rowid: i64,
}

impl Cursor {
    /// The cursor that starts after the row `(created_at, rowid)` of a listing with filter `f`.
    fn after_row(f: &ListFilter, created_at: &str, rowid: i64) -> Self {
        Cursor(format!(
            "{VERSION}.{SORT_NEWEST_FIRST}.{}.{}.{}.{rowid}",
            enc_opt(&f.status),
            enc_opt(&f.request_id),
            to_hex(created_at)
        ))
    }

    /// The cursor as a string (stable for the life of the position; opaque).
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Read a cursor back from the string [`Cursor::as_str`] gave. Checks the shape only; the
    /// filter and sort are checked when the cursor is used. A malformed string fails with
    /// `invalid_cursor` ([`crate::INVALID_CURSOR`]).
    pub fn parse(s: &str) -> JobResult<Cursor> {
        let c = Cursor(s.to_string());
        c.decode()?;
        Ok(c)
    }

    fn decode(&self) -> JobResult<Position> {
        let bad = |why: &str| JobError::new(INVALID_CURSOR, format!("not a paging cursor: {why}"));
        let s = self.0.as_str();
        let parts: Vec<&str> = s.split('.').collect();
        let &[version, sort, status, request_id, created_at, rowid] = parts.as_slice() else {
            return Err(bad("wrong number of fields"));
        };
        if version != VERSION {
            return Err(bad("unknown version"));
        }
        if sort.is_empty() || sort.len() > 8 || !sort.bytes().all(|b| b.is_ascii_lowercase()) {
            return Err(bad("bad sort tag"));
        }
        Ok(Position {
            sort: sort.to_string(),
            status: dec_opt(status).ok_or_else(|| bad("bad status field"))?,
            request_id: dec_opt(request_id).ok_or_else(|| bad("bad request_id field"))?,
            created_at: from_hex(created_at).ok_or_else(|| bad("bad created_at field"))?,
            rowid: rowid.parse().map_err(|_| bad("bad row key"))?,
        })
    }

    /// The position this cursor names, checked against the listing it is passed to.
    pub(super) fn position_for(&self, f: &ListFilter) -> JobResult<Position> {
        let p = self.decode()?;
        let mismatch = |what: &str| {
            JobError::new(
                CURSOR_MISMATCH,
                format!("the cursor was issued for a different {what} than this listing"),
            )
        };
        if p.sort != SORT_NEWEST_FIRST {
            return Err(mismatch("sort order"));
        }
        if p.status != f.status {
            return Err(mismatch("status filter"));
        }
        if p.request_id != f.request_id {
            return Err(mismatch("request_id filter"));
        }
        Ok(p)
    }
}

impl fmt::Display for Cursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Cursor {
    type Err = JobError;
    fn from_str(s: &str) -> JobResult<Cursor> {
        Cursor::parse(s)
    }
}

impl AsRef<str> for Cursor {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<Cursor> for String {
    fn from(c: Cursor) -> String {
        c.0
    }
}

impl JobStore {
    /// One page of the newest-first listing, starting after `after` (or at the newest row when
    /// `after` is `None`). `f.limit` is the page size (default 50, clamped to 1..=1000); `status`
    /// and `request_id` filter as in [`JobStore::list_records`].
    ///
    /// Keyset paging, never `OFFSET`: the cost of a page does not grow with its depth, and the
    /// walk is stable under concurrent writes (see the module notes in `store/page.rs`): a row
    /// that exists for the whole walk and does not change is returned exactly once, a row that is
    /// inserted, changed or deleted meanwhile is returned at most once, and a cursor stays valid
    /// after the row it was cut from is deleted.
    ///
    /// ```text
    /// let mut after = None;
    /// loop {
    ///     let page = store.list_page(&filter, after.as_ref())?;
    ///     show(&page.records);
    ///     match page.next {
    ///         Some(c) => after = Some(c),
    ///         None => break,
    ///     }
    /// }
    /// ```
    ///
    /// Errors: `invalid_cursor` for a malformed cursor, `cursor_mismatch` for a cursor issued
    /// for a different filter or sort order. The filter's `limit` may change between pages.
    pub fn list_page(&self, f: &ListFilter, after: Option<&Cursor>) -> JobResult<ListPage> {
        let limit = f.limit.unwrap_or(50).clamp(1, 1000);
        let pos = match after {
            Some(c) => Some(c.position_for(f)?),
            None => None,
        };
        let (sql, args) = self.page_query(f, pos.as_ref(), limit);
        let key_col = NCORE + self.cfg.columns.len();
        let conn = self.c();
        let mut st = conn.prepare(&sql)?;
        let mut rows = st
            .query_map(params_from_iter(args), |r| {
                Ok((self.record_row(r)?, r.get::<_, i64>(key_col)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let more = rows.len() as i64 > limit;
        rows.truncate(limit as usize);
        let next = match (more, rows.last()) {
            (true, Some((rec, rowid))) => Some(Cursor::after_row(f, &rec.job.created_at, *rowid)),
            _ => None,
        };
        Ok(ListPage {
            records: rows.into_iter().map(|(rec, _)| rec).collect(),
            next,
        })
    }

    /// The SQL of one page and its bound values: `limit + 1` rows so the caller can tell whether
    /// another page exists, each row followed by its `rowid`. Only the filters that are set
    /// appear in the `WHERE`, so SQLite can seek on `request_id` or `status` instead of scanning.
    pub(super) fn page_query(
        &self,
        f: &ListFilter,
        pos: Option<&Position>,
        limit: i64,
    ) -> (String, Vec<SqlValue>) {
        let mut args: Vec<SqlValue> = Vec::new();
        let mut conds: Vec<String> = Vec::new();
        if let Some(r) = &f.request_id {
            args.push(r.clone().into());
            conds.push(format!("request_id=?{}", args.len()));
        }
        if let Some(s) = &f.status {
            args.push(s.clone().into());
            conds.push(format!("status=?{}", args.len()));
        }
        if let Some(p) = pos {
            args.push(p.created_at.clone().into());
            args.push(p.rowid.into());
            conds.push(format!(
                "(created_at, rowid) < (?{}, ?{})",
                args.len() - 1,
                args.len()
            ));
        }
        let mut sql = format!("SELECT {}, rowid FROM {}", self.table.select(), self.jt());
        if !conds.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conds.join(" AND "));
        }
        args.push((limit + 1).into());
        sql.push_str(&format!(
            " ORDER BY created_at DESC, rowid DESC LIMIT ?{}",
            args.len()
        ));
        (sql, args)
    }
}

fn to_hex(s: &str) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0x0f) as usize] as char);
    }
    out
}

fn from_hex(h: &str) -> Option<String> {
    fn nibble(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            _ => None,
        }
    }
    let bytes = h.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.as_chunks::<2>().0 {
        out.push((nibble(pair[0])? << 4) | nibble(pair[1])?);
    }
    String::from_utf8(out).ok()
}

/// `-` for no value; `x` plus hex for a value (so an empty string differs from none).
fn enc_opt(v: &Option<String>) -> String {
    match v {
        None => "-".to_string(),
        Some(s) => format!("x{}", to_hex(s)),
    }
}

fn dec_opt(s: &str) -> Option<Option<String>> {
    if s == "-" {
        return Some(None);
    }
    s.strip_prefix('x').and_then(from_hex).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(status: Option<&str>, request_id: Option<&str>) -> ListFilter {
        ListFilter {
            limit: None,
            status: status.map(Into::into),
            request_id: request_id.map(Into::into),
        }
    }

    #[test]
    fn a_cursor_round_trips_and_keeps_its_filter() {
        let f = filter(Some("failed"), Some("req.1"));
        let c = Cursor::after_row(&f, "2026-01-02T03:04:05Z", 42);
        let p = c.position_for(&f).unwrap();
        assert_eq!(p.created_at, "2026-01-02T03:04:05Z");
        assert_eq!(p.rowid, 42);
        assert_eq!(Cursor::parse(c.as_str()).unwrap(), c);
        assert_eq!(c.to_string(), c.as_str());
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<Cursor>(&json).unwrap(), c);
    }

    #[test]
    fn a_cursor_is_bound_to_its_filter_and_sort() {
        let f = filter(Some("failed"), None);
        let c = Cursor::after_row(&f, "2026-01-02T03:04:05Z", 7);
        for other in [
            filter(None, None),
            filter(Some("complete"), None),
            filter(Some("failed"), Some("r")),
            filter(Some(""), None),
        ] {
            assert_eq!(c.position_for(&other).unwrap_err().code, CURSOR_MISMATCH);
        }
        // A well-formed cursor of a sort order this build does not have.
        let other_sort = Cursor(c.as_str().replacen(".cd.", ".xx.", 1));
        assert!(Cursor::parse(other_sort.as_str()).is_ok());
        assert_eq!(
            other_sort.position_for(&f).unwrap_err().code,
            CURSOR_MISMATCH
        );
    }

    #[test]
    fn malformed_cursors_are_invalid() {
        let good = Cursor::after_row(&filter(None, None), "2026-01-02T03:04:05Z", 1);
        let truncated = &good.as_str()[..good.as_str().len() - 2];
        for bad in [
            "",
            "nonsense",
            "rk2.cd.-.-.00.1",
            "rk1.cd.-.-.0.1",
            "rk1.cd.-.-.zz.1",
            "rk1.cd.q.-.00.1",
            "rk1.CD.-.-.00.1",
            "rk1.cd.-.-.00.x",
            "rk1.cd.-.-.00.1.extra",
            truncated,
        ] {
            let e = Cursor::parse(bad).unwrap_err();
            assert_eq!(e.code, INVALID_CURSOR, "{bad:?}");
        }
        // Length alone never makes a cursor bad; a long string of the wrong shape still is.
        assert!(Cursor::parse(&"a".repeat(100_000)).is_err());
    }

    #[test]
    fn every_issued_cursor_decodes_whatever_its_length() {
        // Filter values and `created_at` have no length bound, so a cursor well past any fixed
        // cap must round-trip and still bind to the filter it was issued for.
        let status = "s".repeat(40_000);
        let request_id = "r".repeat(70_000);
        let created_at = format!("2026-01-01T00:00:00Z{}", "p".repeat(50_000));
        let f = filter(Some(&status), Some(&request_id));
        let c = Cursor::after_row(&f, &created_at, 42);
        assert!(c.as_str().len() > 300_000);
        let parsed = Cursor::parse(c.as_str()).unwrap();
        assert_eq!(parsed, c);
        let p = parsed.position_for(&f).unwrap();
        assert_eq!(p.status.as_deref(), Some(status.as_str()));
        assert_eq!(p.request_id.as_deref(), Some(request_id.as_str()));
        assert_eq!(p.created_at, created_at);
        assert_eq!(p.rowid, 42);
        assert_eq!(
            parsed
                .position_for(&filter(Some(&status), Some("other")))
                .unwrap_err()
                .code,
            CURSOR_MISMATCH
        );
    }

    #[test]
    fn a_page_is_a_range_seek_on_the_created_index() {
        let dir = tempfile::tempdir().unwrap();
        let store = JobStore::open(dir.path().join("j.db")).unwrap();
        let at = Position {
            sort: SORT_NEWEST_FIRST.into(),
            status: None,
            request_id: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            rowid: 5,
        };
        for (f, pos) in [
            (filter(None, None), None),
            (filter(None, None), Some(&at)),
            (filter(Some("failed"), None), None),
            (filter(Some("failed"), None), Some(&at)),
        ] {
            let (sql, args) = store.page_query(&f, pos, 50);
            let plan = store
                .read(|c| {
                    let mut st = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
                    let rows = st
                        .query_map(params_from_iter(args), |r| r.get::<_, String>(3))?
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(rows.join(" | "))
                })
                .unwrap();
            assert!(plan.contains("rk_jobs_"), "no index in: {plan}\n{sql}");
            assert!(!plan.contains("TEMP B-TREE"), "a sort in: {plan}\n{sql}");
        }
    }
}
