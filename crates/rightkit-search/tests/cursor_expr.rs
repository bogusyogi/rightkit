//! Cursor-side expressions: a sort column whose cursor value resolves through a
//! lookup with its own bound parameters. Runs against a real in-memory SQLite database.

use rightkit_search::page::{Cursor, CursorError, Keyset};
use rightkit_search::{Compiled, Param, Query, Schema};
use rusqlite::{params_from_iter, Connection};

type Row = (String, i64); // (ext, ts)

const ACCOUNT: i64 = 1;

/// Resolves the cursor's `ext` to the row's rowid within one account. The fallback
/// for a row that no longer exists is `i64::MAX`, which the deleted-row test uses.
const ROWID_OF_EXT: &str =
    "COALESCE((SELECT rowid FROM evts WHERE account_id = ? AND ext = ?), 9223372036854775807)";

/// Oldest first by timestamp, then by rowid. The rowid is not the `ext` order, so a
/// cursor that compared `ext` instead of the resolved rowid would page incorrectly.
fn keyset() -> Keyset {
    Keyset::new()
        .asc("ts")
        .asc("rowid")
        .cursor_expr(1, ROWID_OF_EXT, vec![Param::Int(ACCOUNT)])
}

fn evts_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE evts(account_id INTEGER NOT NULL, ts INTEGER NOT NULL, ext TEXT NOT NULL, \
         UNIQUE(account_id, ext));",
    )
    .unwrap();
    conn
}

fn insert(conn: &Connection, ts: i64, ext: &str) {
    conn.execute(
        "INSERT INTO evts(account_id, ts, ext) VALUES (?1, ?2, ?3)",
        rusqlite::params![ACCOUNT, ts, ext],
    )
    .unwrap();
}

/// The cursor for a row: its sort values in keyset order, `ts` then `ext`.
fn cursor(ts: i64, ext: &str) -> Cursor {
    Cursor::new(vec![Param::Int(ts), Param::Text(ext.to_string())])
}

fn no_filter() -> Compiled {
    Schema::new().compile(&Query::default()).unwrap()
}

/// One page of up to `limit` rows, plus the cursor for the next page when one exists.
fn page(
    conn: &Connection,
    keyset: &Keyset,
    after: Option<&Cursor>,
    limit: usize,
) -> (Vec<Row>, Option<Cursor>) {
    let p = no_filter()
        .page("SELECT ext, ts FROM evts", keyset, after, limit)
        .unwrap();
    let mut stmt = conn.prepare(&p.sql).unwrap();
    let mut rows: Vec<Row> = stmt
        .query_map(params_from_iter(p.params.iter()), |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next = if more {
        rows.last().map(|(ext, ts)| cursor(*ts, ext))
    } else {
        None
    };
    (rows, next)
}

/// Every row in the keyset order, read without paging.
fn all_rows(conn: &Connection) -> Vec<Row> {
    let mut stmt = conn
        .prepare("SELECT ext, ts FROM evts ORDER BY ts ASC, rowid ASC")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

#[test]
fn duplicate_timestamps_page_without_skips_or_repeats() {
    let conn = evts_db();
    // Insert in reverse so each timestamp's rowid order differs from its ext order.
    // Timestamps: m00..m03 at 100, m04..m07 at 90, m08..m11 at 80.
    for i in (0..12).rev() {
        insert(&conn, 100 - (i / 4) * 10, &format!("m{i:02}"));
    }
    let expected = all_rows(&conn);
    assert_eq!(expected.len(), 12);
    assert_eq!(expected[0].0, "m11", "oldest tie group runs in rowid order");

    let ks = keyset();
    let mut got: Vec<Row> = Vec::new();
    let mut after: Option<Cursor> = None;
    loop {
        let (rows, next) = page(&conn, &ks, after.as_ref(), 5);
        got.extend(rows);
        match next {
            Some(c) => after = Some(c),
            None => break,
        }
    }
    assert_eq!(got, expected);
}

#[test]
fn deleted_cursor_row_resolves_through_the_expression_fallback() {
    let conn = evts_db();
    for (i, ts) in [10, 10, 10, 20, 20, 30].into_iter().enumerate() {
        insert(&conn, ts, &format!("d{i}"));
    }
    let ks = keyset();

    let (p1, next1) = page(&conn, &ks, None, 2);
    assert_eq!(
        p1.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
        vec!["d0", "d1"]
    );
    let next1 = next1.expect("first page is full");

    // The cursor row d1 is gone. Its rowid lookup misses, so the fallback (i64::MAX)
    // applies: no row in its tie group compares as after it, and the page resumes
    // with the next timestamp. d2 is skipped, which is the documented trade-off of
    // this fallback.
    conn.execute("DELETE FROM evts WHERE ext = 'd1'", [])
        .unwrap();
    let (p2, _) = page(&conn, &ks, Some(&next1), 10);
    let expected: Vec<Row> = all_rows(&conn).into_iter().filter(|r| r.1 > 10).collect();
    assert_eq!(p2, expected);
    assert_eq!(
        p2.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
        vec!["d3", "d4", "d5"]
    );
}

#[test]
fn cursor_params_bind_in_placeholder_order() {
    let q = no_filter();
    let ks = keyset();

    // No cursor: the expression adds no parameters.
    let first = q.page_clauses(&ks, None).unwrap();
    assert!(first.params.is_empty());
    assert_eq!(first.order_by, "ts ASC, rowid ASC");

    // With a cursor, the tie-group term binds ts, then the expression's account, then
    // the cursor's ext for its `?`. The order matches the SQL text left to right.
    let after = cursor(10, "d1");
    let clauses = q.page_clauses(&ks, Some(&after)).unwrap();
    assert_eq!(
        clauses.params,
        vec![
            Param::Int(10),
            Param::Int(10),
            Param::Int(ACCOUNT),
            Param::Text("d1".to_string()),
        ]
    );
    assert_eq!(
        clauses.order_by, "ts ASC, rowid ASC",
        "sort order is unchanged"
    );
}

#[test]
fn bad_cursor_expressions_are_typed_errors() {
    let q = no_filter();

    let out_of_range =
        Keyset::new()
            .asc("ts")
            .cursor_expr(3, ROWID_OF_EXT, vec![Param::Int(ACCOUNT)]);
    assert_eq!(
        q.page_clauses(&out_of_range, None).unwrap_err(),
        CursorError::ColumnOutOfRange {
            column: 3,
            columns: 1,
        }
    );

    // Two placeholders but no params: one is the cursor value, so one param is missing.
    let missing_param = Keyset::new()
        .asc("ts")
        .asc("rowid")
        .cursor_expr(1, ROWID_OF_EXT, vec![]);
    assert_eq!(
        q.page_clauses(&missing_param, None).unwrap_err(),
        CursorError::ExprBindCount {
            column: 1,
            expected: 1,
            got: 2,
        }
    );
}
