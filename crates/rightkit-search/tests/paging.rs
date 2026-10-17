//! Keyset paging: page boundaries, rows changing between pages, FTS filters, and
//! cursor encoding. Runs against a real in-memory SQLite database.

use rightkit_search::page::{Cursor, CursorError, Keyset};
use rightkit_search::{Binding, Compiled, Param, Query, Schema};
use rusqlite::{params_from_iter, Connection};

type Row = (i64, i64); // (id, ts)

fn key(id: i64, ts: i64) -> Cursor {
    Cursor::new(vec![Param::Int(ts), Param::Int(id)])
}

fn msgs_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE msgs(id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, subject TEXT NOT NULL);",
    )
    .unwrap();
    conn
}

fn insert(conn: &Connection, id: i64, ts: i64) {
    conn.execute(
        "INSERT INTO msgs(id, ts, subject) VALUES (?1, ?2, ?3)",
        rusqlite::params![id, ts, format!("subject {id}")],
    )
    .unwrap();
}

/// Newest first by timestamp, then by id (the id column is unique, so it is last).
fn newest_first() -> Keyset {
    Keyset::new().desc("ts").desc("id")
}

fn no_filter() -> Compiled {
    Schema::new().compile(&Query::default()).unwrap()
}

/// Run one page and return up to `limit` rows, plus the cursor for the next page when
/// one exists. The statement binds `limit + 1`, so a row past `limit` means more rows.
fn page(
    conn: &Connection,
    compiled: &Compiled,
    keyset: &Keyset,
    after: Option<&Cursor>,
    limit: usize,
) -> (Vec<Row>, Option<Cursor>) {
    let p = compiled
        .page("SELECT id, ts FROM msgs", keyset, after, limit)
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
        rows.last().map(|&(id, ts)| key(id, ts))
    } else {
        None
    };
    (rows, next)
}

fn ids(rows: &[Row]) -> Vec<i64> {
    rows.iter().map(|r| r.0).collect()
}

#[test]
fn page_two_starts_strictly_after_page_one_with_equal_sort_keys() {
    let conn = msgs_db();
    // Ids 1..=4 share ts 100; ids 5 and 6 share ts 90.
    for (id, ts) in [(1, 100), (2, 100), (3, 100), (4, 100), (5, 90), (6, 90)] {
        insert(&conn, id, ts);
    }
    let ks = newest_first();
    let q = no_filter();

    let (p1, next1) = page(&conn, &q, &ks, None, 2);
    assert_eq!(ids(&p1), vec![4, 3]);
    let next1 = next1.expect("first page is full, so a cursor is returned");

    let (p2, next2) = page(&conn, &q, &ks, Some(&next1), 2);
    assert_eq!(ids(&p2), vec![2, 1]);
    // Strictly after page one's last row (100, 3) in the sort order.
    assert!(p2.iter().all(|&(id, ts)| (ts, id) < (100, 3)));
    let next2 = next2.expect("second page is full");

    let (p3, next3) = page(&conn, &q, &ks, Some(&next2), 2);
    assert_eq!(ids(&p3), vec![6, 5]);
    assert_eq!(next3, None, "last rows fit the page, so no cursor");
}

#[test]
fn rows_inserted_between_pages() {
    let conn = msgs_db();
    // Ids 1..=10 with ts = id * 10, so ids and timestamps order the same way.
    for id in 1..=10 {
        insert(&conn, id, id * 10);
    }
    let ks = newest_first();
    let q = no_filter();

    let (p1, next1) = page(&conn, &q, &ks, None, 4);
    assert_eq!(ids(&p1), vec![10, 9, 8, 7]);
    let next1 = next1.unwrap();

    // Rows added after page one: one newer than everything, one with the cursor's
    // timestamp but a higher id, and one inside page two's range.
    insert(&conn, 11, 200);
    insert(&conn, 12, 70);
    insert(&conn, 13, 55);
    // The row the cursor points at is deleted; the cursor values still hold.
    conn.execute("DELETE FROM msgs WHERE id = 7", []).unwrap();

    let (p2, next2) = page(&conn, &q, &ks, Some(&next1), 4);
    assert_eq!(ids(&p2), vec![6, 13, 5, 4]);
    assert!(!ids(&p2).contains(&11), "newer row must not appear");
    assert!(
        !ids(&p2).contains(&12),
        "equal-timestamp higher-id row must not appear"
    );
    let next2 = next2.unwrap();

    let (p3, next3) = page(&conn, &q, &ks, Some(&next2), 4);
    assert_eq!(ids(&p3), vec![3, 2, 1]);
    assert_eq!(next3, None);

    // Across all pages: no duplicate, no skipped pre-existing row, and the new in-range
    // row is included exactly once.
    let mut all = ids(&p1);
    all.extend(ids(&p2));
    all.extend(ids(&p3));
    assert_eq!(all, vec![10, 9, 8, 7, 6, 13, 5, 4, 3, 2, 1]);
}

#[test]
fn cursor_roundtrip_and_arity_error() {
    let samples = vec![
        Cursor::new(vec![]),
        Cursor::new(vec![Param::Int(0)]),
        Cursor::new(vec![
            Param::Int(-42),
            Param::Int(i64::MIN),
            Param::Int(i64::MAX),
        ]),
        Cursor::new(vec![Param::Text(String::new())]),
        Cursor::new(vec![Param::Text("a;b:c t9:x i1;".to_string())]),
        Cursor::new(vec![Param::Text("東京 café".to_string()), Param::Int(7)]),
    ];
    for c in &samples {
        let s = c.encode();
        assert!(s.starts_with("k1"));
        assert_eq!(&Cursor::decode(&s).unwrap(), c, "roundtrip of {s:?}");
    }
    assert_eq!(Cursor::new(vec![Param::Int(3)]).encode(), "k1i3;");
    assert_eq!(
        Cursor::new(vec![Param::Text("ab".into())]).encode(),
        "k1t2:ab"
    );

    for bad in [
        "",
        "k2i1;",
        "i1;",
        "k1i1",
        "k1i;",
        "k1ix;",
        "k1i-;",
        "k1x1;",
        "k1t5:abc",
        "k1t:abc",
        "k1t1:東",
        "k1t99999999999999999999999:a",
    ] {
        assert_eq!(
            Cursor::decode(bad),
            Err(CursorError::Malformed),
            "input {bad:?}"
        );
    }

    let q = no_filter();
    let err = q
        .page(
            "SELECT id, ts FROM msgs",
            &newest_first(),
            Some(&Cursor::new(vec![Param::Int(1)])),
            5,
        )
        .unwrap_err();
    assert_eq!(
        err,
        CursorError::Arity {
            expected: 2,
            got: 1
        }
    );
}

#[test]
fn fts_filter_pages_newest_first() {
    let conn = msgs_db();
    conn.execute_batch("CREATE VIRTUAL TABLE fts USING fts5(subject);")
        .unwrap();
    let rows = [
        (1, 100, "invoice march"),
        (2, 110, "receipt"),
        (3, 120, "invoice april"),
        (4, 130, "lunch"),
        (5, 140, "invoice may"),
        (6, 150, "invoice june"),
        (7, 160, "invoice july"),
    ];
    for (id, ts, subject) in rows {
        conn.execute(
            "INSERT INTO msgs(id, ts, subject) VALUES (?1, ?2, ?3)",
            rusqlite::params![id, ts, subject],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO fts(rowid, subject) VALUES (?1, ?2)",
            rusqlite::params![id, subject],
        )
        .unwrap();
    }
    let schema = Schema::new().fts("fts", "msgs.rowid");
    let q = schema.parse("invoice").unwrap();
    let compiled = schema.compile(&q).unwrap();
    assert!(compiled.fts_match.is_some());

    let ks = newest_first();
    let (p1, n1) = page(&conn, &compiled, &ks, None, 2);
    assert_eq!(ids(&p1), vec![7, 6]);
    let (p2, n2) = page(&conn, &compiled, &ks, n1.as_ref(), 2);
    assert_eq!(ids(&p2), vec![5, 3]);
    let (p3, n3) = page(&conn, &compiled, &ks, n2.as_ref(), 2);
    assert_eq!(ids(&p3), vec![1]);
    assert_eq!(n3, None);
}

#[test]
fn compiled_predicate_params_come_before_cursor_and_limit() {
    let schema = Schema::new().bind("label", Binding::Eq("label"));
    let q = schema.parse("label:inbox").unwrap();
    let compiled = schema.compile(&q).unwrap();
    let ks = Keyset::new().asc("ts").asc("id");
    let cur = Cursor::new(vec![Param::Int(5), Param::Int(9)]);
    let p = compiled
        .page("SELECT id, ts FROM msgs", &ks, Some(&cur), 3)
        .unwrap();
    assert_eq!(
        p.params,
        vec![
            Param::Text("inbox".into()),
            Param::Int(5),
            Param::Int(5),
            Param::Int(9),
            Param::Int(4),
        ]
    );
    assert!(p.sql.starts_with("SELECT id, ts FROM msgs WHERE ("));
    assert!(p.sql.ends_with("ORDER BY ts ASC, id ASC LIMIT ?"));
}

#[test]
fn empty_keyset_is_a_typed_error_not_a_panic() {
    let compiled = no_filter();
    let empty = Keyset::new();
    assert!(matches!(
        compiled.page("SELECT id, ts FROM msgs", &empty, None, 2),
        Err(CursorError::EmptyKeyset)
    ));
    let cur = Cursor::new(vec![Param::Int(1)]);
    assert!(matches!(
        compiled.page("SELECT id, ts FROM msgs", &empty, Some(&cur), 2),
        Err(CursorError::EmptyKeyset)
    ));
    assert_eq!(
        CursorError::EmptyKeyset.to_string(),
        "keyset has no sort columns"
    );
}

#[test]
fn range_bound_keeps_pages_identical_and_adds_one_bind() {
    let conn = msgs_db();
    for id in 1..=10 {
        insert(&conn, id, id * 10);
    }
    let plain = newest_first();
    let bounded = newest_first().range_bound();
    let q = no_filter();

    let (p1, next1) = page(&conn, &q, &plain, None, 4);
    let next1 = next1.unwrap();
    // With no cursor the range bound adds nothing to the statement.
    let none = q
        .page("SELECT id, ts FROM msgs", &bounded, None, 4)
        .unwrap();
    assert_eq!(none.params.len(), 1, "only the limit is bound");

    let (b2, _) = page(&conn, &q, &bounded, Some(&next1), 4);
    let (p2, _) = page(&conn, &q, &plain, Some(&next1), 4);
    assert_eq!(ids(&b2), ids(&p2));
    assert_eq!(ids(&p1), vec![10, 9, 8, 7]);
}

#[test]
fn page_clauses_supports_a_caller_built_statement_with_its_own_limit() {
    let conn = msgs_db();
    for id in 1..=5 {
        insert(&conn, id, id * 10);
    }
    let ks = newest_first().range_bound();
    let q = no_filter();
    let c = q.page_clauses(&ks, Some(&key(4, 40))).unwrap();
    // Range bound (1) + group one (1) + group two (2): no limit in the clauses.
    assert_eq!(c.params.len(), 4);
    let sql = format!(
        "WITH base AS (SELECT id, ts FROM msgs) SELECT id, ts FROM base WHERE {} ORDER BY {} LIMIT ?",
        c.where_sql, c.order_by
    );
    let mut params = c.params.clone();
    params.push(Param::Int(10));
    let mut stmt = conn.prepare(&sql).unwrap();
    let got: Vec<i64> = stmt
        .query_map(params_from_iter(params.iter()), |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(got, vec![3, 2, 1]);
}
