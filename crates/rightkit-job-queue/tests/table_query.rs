//! `JobTable::query` / `JobTable::count`: filters apply before the limit, NULL filters, an
//! uncapped count, a total order that pages without gaps or duplicates (including the
//! `COALESCE(send_at, created_ms)` order), and cursors bound to the query that issued them.

use rightkit_job_queue::rusqlite::{params, Connection};
use rightkit_job_queue::{
    ExtColumn, JobCursor, JobOrder, JobQuery, JobResult, JobTable, StatusModel, StoreConfig,
    CURSOR_MISMATCH, INVALID_CURSOR, MAX_QUERY_LIMIT,
};
use serde_json::{json, Value};

fn cfg() -> StoreConfig {
    let model = StatusModel::new("queued", ["queued", "sending", "send_failed"])
        .allow("queued", "sending")
        .allow("sending", "send_failed")
        .allow("send_failed", "queued")
        .run_status(Some("sending"));
    StoreConfig::new()
        .jobs_table("mail_jobs")
        .column(ExtColumn::text("account").not_null())
        .column(ExtColumn::integer("send_at"))
        .column(ExtColumn::integer("created_ms").not_null())
        .column(ExtColumn::text("note"))
        .column(ExtColumn::json("meta"))
        .due_column("send_at")
        .statuses(model)
}

fn open() -> (tempfile::TempDir, Connection, JobTable) {
    let dir = tempfile::tempdir().unwrap();
    let conn = Connection::open(dir.path().join("app.sqlite")).unwrap();
    let table = JobTable::new(&cfg()).unwrap();
    table.init_schema(&conn).unwrap();
    (dir, conn, table)
}

#[derive(Default)]
struct Seed<'a> {
    id: String,
    account: &'a str,
    status: &'a str,
    attempts: i64,
    send_at: Option<i64>,
    created_ms: i64,
    created_at: String,
}

fn put(conn: &Connection, s: &Seed) {
    conn.execute(
        "INSERT INTO mail_jobs (job_id, request_id, kind, request_json, status, attempts, \
         created_at, updated_at, account, send_at, created_ms) \
         VALUES (?1, ?1, 'mail', '{}', ?2, ?3, ?4, ?4, ?5, ?6, ?7)",
        params![
            s.id,
            s.status,
            s.attempts,
            s.created_at,
            s.account,
            s.send_at,
            s.created_ms
        ],
    )
    .unwrap();
}

fn code<T: std::fmt::Debug>(r: JobResult<T>) -> String {
    r.unwrap_err().code
}

/// Every page of `base`, following the cursors; the page sizes and the ids in order.
fn walk(table: &JobTable, conn: &Connection, base: &JobQuery) -> (Vec<usize>, Vec<String>) {
    let mut sizes = Vec::new();
    let mut ids = Vec::new();
    let mut q = base.clone();
    loop {
        let page = table.query(conn, &q).unwrap();
        sizes.push(page.records.len());
        ids.extend(page.records.iter().map(|r| r.job.job_id.clone()));
        match page.next {
            Some(c) => q = base.clone().after(c),
            None => return (sizes, ids),
        }
    }
}

#[test]
fn filters_apply_before_the_limit_over_10000_decoys() {
    let (_d, mut conn, table) = open();
    let tx = conn.transaction().unwrap();
    // The 40 rows of account "me" are the OLDEST: a limit applied before the filter would see
    // only decoys.
    for i in 0..40i64 {
        put(
            &tx,
            &Seed {
                id: format!("me-{i:03}"),
                account: "me",
                status: "queued",
                attempts: if i % 4 == 0 { 5 } else { 0 },
                created_ms: i,
                created_at: format!("2026-01-01T00:00:00.{i:06}Z"),
                ..Seed::default()
            },
        );
    }
    for i in 0..10i64 {
        put(
            &tx,
            &Seed {
                id: format!("mf-{i:03}"),
                account: "me",
                status: "send_failed",
                created_ms: i,
                created_at: format!("2026-01-02T00:00:00.{i:06}Z"),
                ..Seed::default()
            },
        );
    }
    for i in 0..10_500i64 {
        put(
            &tx,
            &Seed {
                id: format!("decoy-{i:05}"),
                account: "other",
                status: "queued",
                created_ms: i,
                created_at: format!("2026-03-01T00:00:00.{i:06}Z"),
                ..Seed::default()
            },
        );
    }
    tx.commit().unwrap();

    let base = JobQuery::new()
        .ext_eq("account", "me")
        .status_in(["queued"])
        .attempts_below(3)
        .order(JobOrder::CreatedAsc)
        .limit(7);
    let (sizes, ids) = walk(&table, &conn, &base);
    let expected: Vec<String> = (0..40)
        .filter(|i| i % 4 != 0)
        .map(|i| format!("me-{i:03}"))
        .collect();
    assert_eq!(ids, expected, "exactly the matching rows, oldest first");
    assert_eq!(sizes, vec![7, 7, 7, 7, 2]);
    assert_eq!(table.count(&conn, &base).unwrap(), 30);

    // The first page alone is full of matches, not decoys.
    let first = table.query(&conn, &base).unwrap();
    assert_eq!(first.records.len(), 7);
    assert!(first.next.is_some());
    for r in &first.records {
        assert_eq!(r.ext_str("account"), Some("me"));
        assert_eq!(r.job.status, "queued");
        assert!(r.job.attempts < 3);
    }

    // attempt >= n, and a status set with two members.
    let capped = JobQuery::new()
        .ext_eq("account", "me")
        .attempts_at_least(3)
        .order(JobOrder::CreatedAsc)
        .limit(5);
    let (_, capped_ids) = walk(&table, &conn, &capped);
    assert_eq!(capped_ids.len(), 10);
    assert!(capped_ids.iter().all(|id| id.starts_with("me-")));
    let both = JobQuery::new()
        .ext_eq("account", "me")
        .status_in(["queued"])
        .status_in(["send_failed"])
        .attempts_below(3);
    assert_eq!(table.count(&conn, &both).unwrap(), 40);

    // Default order is newest first, still filtered before the limit.
    let newest = table
        .query(&conn, &JobQuery::new().ext_eq("account", "me").limit(5))
        .unwrap();
    assert_eq!(newest.records.len(), 5);
    assert!(newest
        .records
        .iter()
        .all(|r| r.job.job_id.starts_with("mf-")));
}

#[test]
fn null_and_not_null_filters() {
    let (_d, conn, table) = open();
    for i in 0..20i64 {
        put(
            &conn,
            &Seed {
                id: format!("j-{i:02}"),
                account: "a",
                status: "queued",
                send_at: if i % 2 == 0 { Some(100 + i) } else { None },
                created_ms: i,
                created_at: format!("2026-01-01T00:00:00.{i:06}Z"),
                ..Seed::default()
            },
        );
    }
    let nulls = JobQuery::new().ext_null("send_at").limit(100);
    let page = table.query(&conn, &nulls).unwrap();
    assert_eq!(page.records.len(), 10);
    assert!(page.records.iter().all(|r| r.ext["send_at"].is_null()));
    assert!(page.next.is_none());

    let some = JobQuery::new().ext_not_null("send_at").limit(100);
    let page = table.query(&conn, &some).unwrap();
    assert_eq!(page.records.len(), 10);
    assert!(page.records.iter().all(|r| !r.ext["send_at"].is_null()));

    // NULL and NOT NULL partition the table; the unset TEXT column is NULL on every row.
    assert_eq!(
        table
            .count(&conn, &JobQuery::new().ext_null("note"))
            .unwrap(),
        20
    );
    assert_eq!(
        table
            .count(&conn, &JobQuery::new().ext_not_null("note"))
            .unwrap(),
        0
    );
    // Typed equality on an INTEGER column.
    let one = table
        .query(&conn, &JobQuery::new().ext_eq("send_at", 104))
        .unwrap();
    assert_eq!(one.records.len(), 1);
    assert_eq!(one.records[0].job.job_id, "j-04");
    // NULL never equals anything.
    assert_eq!(
        table
            .count(&conn, &JobQuery::new().ext_eq("send_at", 1))
            .unwrap(),
        0
    );
}

#[test]
fn count_is_uncapped_and_ignores_limit_and_cursor() {
    let (_d, mut conn, table) = open();
    let tx = conn.transaction().unwrap();
    for i in 0..10_300i64 {
        put(
            &tx,
            &Seed {
                id: format!("c-{i:05}"),
                account: if i % 2 == 0 { "even" } else { "odd" },
                status: "queued",
                created_ms: i,
                created_at: format!("2026-01-01T00:00:00.{i:06}Z"),
                ..Seed::default()
            },
        );
    }
    tx.commit().unwrap();

    let all = JobQuery::new().limit(1);
    assert_eq!(table.count(&conn, &all).unwrap(), 10_300);
    let page = table.query(&conn, &all).unwrap();
    assert_eq!(page.records.len(), 1);
    let with_cursor = JobQuery::new().limit(5).after(page.next.unwrap());
    assert_eq!(table.count(&conn, &with_cursor).unwrap(), 10_300);
    assert_eq!(
        table
            .count(&conn, &JobQuery::new().ext_eq("account", "even"))
            .unwrap(),
        5_150
    );
    // The largest page holds MAX_QUERY_LIMIT rows and points at the rest.
    let big = table
        .query(&conn, &JobQuery::new().limit(MAX_QUERY_LIMIT))
        .unwrap();
    assert_eq!(big.records.len(), MAX_QUERY_LIMIT);
    assert!(big.next.is_some());
}

/// 300 rows; a quarter carry `send_at`, the rest fall back to `created_ms`; pairs of rows tie on
/// `created_ms`. Returns `(key, job_id)` per row as the store should sort them.
fn seed_coalesce(conn: &Connection) -> Vec<(i64, String)> {
    let mut keys = Vec::new();
    for i in 0..300i64 {
        let created_ms = 1_000 + (i / 2) * 7;
        let send_at = if i % 4 == 0 {
            Some(1_000 + ((i + 3) / 2) * 7)
        } else {
            None
        };
        let id = format!("job-{i:04}");
        put(
            conn,
            &Seed {
                id: id.clone(),
                account: "a",
                status: "queued",
                send_at,
                created_ms,
                created_at: format!("2026-01-01T00:00:00.{i:06}Z"),
                ..Seed::default()
            },
        );
        keys.push((send_at.unwrap_or(created_ms), id));
    }
    keys
}

#[test]
fn coalesce_desc_top_100_then_the_rest_without_gaps_or_duplicates() {
    let (_d, conn, table) = open();
    let mut expected = seed_coalesce(&conn);
    expected.sort();
    expected.reverse();
    let expected_ids: Vec<String> = expected.iter().map(|(_, id)| id.clone()).collect();

    let base = JobQuery::new()
        .order(JobOrder::coalesce_desc("send_at", "created_ms"))
        .limit(100);
    let first = table.query(&conn, &base).unwrap();
    let first_ids: Vec<String> = first.records.iter().map(|r| r.job.job_id.clone()).collect();
    assert_eq!(first_ids, expected_ids[..100], "the newest 100 by COALESCE");
    assert!(first.next.is_some());

    let (sizes, ids) = walk(&table, &conn, &base);
    assert_eq!(sizes, vec![100, 100, 100]);
    assert_eq!(
        ids, expected_ids,
        "no gap, no duplicate, ties broken by job_id"
    );

    // The page size may change between pages.
    let second = table
        .query(&conn, &base.clone().limit(30).after(first.next.unwrap()))
        .unwrap();
    let second_ids: Vec<String> = second
        .records
        .iter()
        .map(|r| r.job.job_id.clone())
        .collect();
    assert_eq!(second_ids, expected_ids[100..130]);

    // Ascending is the exact reverse.
    let asc = JobQuery::new()
        .order(JobOrder::coalesce_asc("send_at", "created_ms"))
        .limit(64);
    let (_, asc_ids) = walk(&table, &conn, &asc);
    let mut reversed = expected_ids.clone();
    reversed.reverse();
    assert_eq!(asc_ids, reversed);

    // A filter narrows the same order; the pages still add up to the count.
    let narrowed = JobQuery::new()
        .ext_not_null("send_at")
        .order(JobOrder::coalesce_desc("send_at", "created_ms"))
        .limit(20);
    let (_, narrowed_ids) = walk(&table, &conn, &narrowed);
    assert_eq!(narrowed_ids.len(), 75);
    assert_eq!(table.count(&conn, &narrowed).unwrap(), 75);
}

#[test]
fn created_orders_break_ties_by_job_id() {
    let (_d, conn, table) = open();
    for i in 0..25i64 {
        put(
            &conn,
            &Seed {
                id: format!("t-{i:02}"),
                account: "a",
                status: "queued",
                created_ms: i,
                // Five distinct seconds, five rows each.
                created_at: format!("2026-01-01T00:00:0{}Z", i / 5),
                ..Seed::default()
            },
        );
    }
    let desc = JobQuery::new().order(JobOrder::CreatedDesc).limit(4);
    let (sizes, ids) = walk(&table, &conn, &desc);
    // 25 rows in pages of 4: six full pages and one of 1.
    assert_eq!(sizes, vec![4, 4, 4, 4, 4, 4, 1]);
    let mut want: Vec<(String, String)> = (0..25)
        .map(|i| (format!("2026-01-01T00:00:0{}Z", i / 5), format!("t-{i:02}")))
        .collect();
    want.sort();
    want.reverse();
    let want_ids: Vec<String> = want.into_iter().map(|(_, id)| id).collect();
    assert_eq!(ids, want_ids);

    let asc = JobQuery::new().order(JobOrder::CreatedAsc).limit(6);
    let (_, asc_ids) = walk(&table, &conn, &asc);
    let mut rev = want_ids.clone();
    rev.reverse();
    assert_eq!(asc_ids, rev);
}

#[test]
fn due_order_leaves_out_rows_without_a_due_time() {
    let (_d, conn, table) = open();
    for i in 0..30i64 {
        put(
            &conn,
            &Seed {
                id: format!("d-{i:02}"),
                account: "a",
                status: "queued",
                send_at: if i % 3 == 0 {
                    None
                } else {
                    Some(500 - (i % 5))
                },
                created_ms: i,
                created_at: format!("2026-01-01T00:00:00.{i:06}Z"),
                ..Seed::default()
            },
        );
    }
    let base = JobQuery::new().order(JobOrder::DueAsc).limit(8);
    let (_, ids) = walk(&table, &conn, &base);
    assert_eq!(ids.len(), 20);
    assert_eq!(table.count(&conn, &base).unwrap(), 20);
    let mut want: Vec<(i64, String)> = (0..30i64)
        .filter(|i| i % 3 != 0)
        .map(|i| (500 - (i % 5), format!("d-{i:02}")))
        .collect();
    want.sort();
    let want_ids: Vec<String> = want.into_iter().map(|(_, id)| id).collect();
    assert_eq!(ids, want_ids);

    // No due column configured: refused before any SQL runs.
    let bare = JobTable::new(&StoreConfig::new()).unwrap();
    let mem = Connection::open_in_memory().unwrap();
    assert_eq!(
        code(bare.query(&mem, &JobQuery::new().order(JobOrder::DueAsc))),
        "not_configured"
    );
}

#[test]
fn a_cursor_is_refused_by_any_other_query() {
    let (_d, conn, table) = open();
    seed_coalesce(&conn);
    let base = JobQuery::new()
        .ext_eq("account", "a")
        .ext_not_null("send_at")
        .order(JobOrder::coalesce_desc("send_at", "created_ms"))
        .limit(10);
    let page = table.query(&conn, &base).unwrap();
    let cursor = page.next.unwrap();

    let refused = [
        base.clone().ext_eq("account", "b"),
        JobQuery::new()
            .ext_eq("account", "a")
            .order(JobOrder::coalesce_desc("send_at", "created_ms")),
        base.clone().status_in(["queued"]),
        base.clone().attempts_below(9),
        base.clone().attempts_at_least(0),
        base.clone().ext_null("note"),
        base.clone()
            .order(JobOrder::coalesce_asc("send_at", "created_ms")),
        base.clone().order(JobOrder::CreatedDesc),
    ];
    for q in refused {
        assert_eq!(
            code(table.query(&conn, &q.after(cursor.clone()))),
            CURSOR_MISMATCH
        );
    }

    // The same query is accepted: another limit, and filters added in another order.
    let same = JobQuery::new()
        .ext_not_null("send_at")
        .ext_eq("account", "a")
        .order(JobOrder::coalesce_desc("send_at", "created_ms"))
        .limit(3)
        .after(cursor.clone());
    assert_eq!(table.query(&conn, &same).unwrap().records.len(), 3);

    // A cursor survives a round trip through its string, and a bad string is invalid.
    let again = JobCursor::parse(cursor.as_str()).unwrap();
    assert_eq!(again, cursor);
    assert_eq!(cursor.to_string().parse::<JobCursor>().unwrap(), cursor);
    for bad in ["", "garbage", "rq1.zz.i1.00", "rq9.00.i1.00"] {
        assert_eq!(code(JobCursor::parse(bad)), INVALID_CURSOR, "{bad:?}");
    }
    // A cursor of the default order (sort key of another type) is a mismatch, not a wrong page.
    let default_order_cursor = table
        .query(&conn, &JobQuery::new().limit(1))
        .unwrap()
        .next
        .unwrap();
    assert_eq!(
        code(table.query(&conn, &base.clone().after(default_order_cursor))),
        CURSOR_MISMATCH
    );
}

#[test]
fn identifiers_are_declared_columns_and_values_are_bound() {
    let (_d, conn, table) = open();
    seed_coalesce(&conn);
    let bad_input = "bad_input";

    // Unknown or injected column names never reach SQL.
    for q in [
        JobQuery::new().ext_eq("nope", 1),
        JobQuery::new().ext_null("account; DROP TABLE mail_jobs"),
        JobQuery::new().ext_not_null("job_id"),
        JobQuery::new().order(JobOrder::coalesce_desc("nope", "created_ms")),
        JobQuery::new().order(JobOrder::coalesce_desc("send_at", "status")),
    ] {
        assert_eq!(code(table.query(&conn, &q)), bad_input);
        assert_eq!(code(table.count(&conn, &q)), bad_input);
    }
    // A hostile value is just a value.
    let hostile = JobQuery::new().ext_eq("account", "x'; DROP TABLE mail_jobs; --");
    assert_eq!(table.count(&conn, &hostile).unwrap(), 0);
    assert!(table.query(&conn, &hostile).unwrap().records.is_empty());
    assert_eq!(table.count(&conn, &JobQuery::new()).unwrap(), 300);

    // Values must fit the column's type; NULL and JSON equality are refused.
    for q in [
        JobQuery::new().ext_eq("send_at", "soon"),
        JobQuery::new().ext_eq("account", 5),
        JobQuery::new().ext_eq("account", Value::Null),
        JobQuery::new().ext_eq("meta", json!({"a": 1})),
        JobQuery::new().status_in(["bogus"]),
    ] {
        assert_eq!(code(table.query(&conn, &q)), bad_input);
    }

    // COALESCE needs same-typed columns and a NOT NULL fallback, so its key is never NULL.
    for q in [
        JobQuery::new().order(JobOrder::coalesce_desc("send_at", "created_at")),
        JobQuery::new().order(JobOrder::coalesce_desc("send_at", "note")),
        JobQuery::new().order(JobOrder::coalesce_desc("send_at", "send_at")),
        JobQuery::new().order(JobOrder::coalesce_desc("meta", "created_ms")),
    ] {
        assert_eq!(code(table.query(&conn, &q)), bad_input);
    }
    // TEXT with the core created_at, and a NOT NULL integer fallback, are fine.
    table
        .query(
            &conn,
            &JobQuery::new().order(JobOrder::coalesce_asc("account", "created_at")),
        )
        .unwrap();

    // The limit is refused when out of range, never clamped.
    assert_eq!(
        code(table.query(&conn, &JobQuery::new().limit(0))),
        bad_input
    );
    assert_eq!(
        code(table.query(&conn, &JobQuery::new().limit(MAX_QUERY_LIMIT + 1))),
        bad_input
    );
    assert_eq!(
        table
            .query(&conn, &JobQuery::new().limit(MAX_QUERY_LIMIT))
            .unwrap()
            .records
            .len(),
        300
    );

    // An empty status set matches nothing (it is not a dropped filter).
    let none = JobQuery::new().status_in(Vec::<String>::new());
    assert_eq!(table.count(&conn, &none).unwrap(), 0);
    assert!(table.query(&conn, &none).unwrap().records.is_empty());
}

/// A declared TEXT sort key and a job id have no length bound, so a cursor over them is longer
/// than any fixed cap (the key and the id are hex-encoded). The cursor is issued and accepted in
/// the same domain: every page of a walk over long keys is readable, the walk has no gap and no
/// duplicate, and an issued cursor survives a round trip through its string form.
#[test]
fn page_walk_over_long_text_keys_and_long_job_ids() {
    let (_d, conn, table) = open();
    let mut expected: Vec<(String, String)> = Vec::new();
    for i in 0..14usize {
        // Shared long prefix, so the order is decided by the tail and by the ids.
        let note: Option<String> = match i % 7 {
            0 => None,
            1 => Some(format!("{}{:03}", "k".repeat(2_500), i)),
            2 => Some(format!("{}{:03}", "k".repeat(4_000), i)),
            3 => Some(format!("{}{:03}", "k".repeat(100_000), i)),
            _ => Some(format!("short-{i:03}")),
        };
        let id = if i % 2 == 0 {
            format!("job-{i:04}-{}", "j".repeat(3_000))
        } else {
            format!("job-{i:04}")
        };
        let created_at = format!("2026-01-01T00:00:00.{i:06}Z");
        put(
            &conn,
            &Seed {
                id: id.clone(),
                account: "a",
                status: "queued",
                created_ms: i as i64,
                created_at: created_at.clone(),
                ..Seed::default()
            },
        );
        if let Some(n) = &note {
            conn.execute(
                "UPDATE mail_jobs SET note = ?1 WHERE job_id = ?2",
                params![n, id],
            )
            .unwrap();
        }
        expected.push((note.unwrap_or(created_at), id));
    }
    expected.sort();
    let asc_ids: Vec<String> = expected.iter().map(|(_, id)| id.clone()).collect();
    let mut desc_ids = asc_ids.clone();
    desc_ids.reverse();

    for (order, want) in [
        (JobOrder::coalesce_asc("note", "created_at"), asc_ids),
        (JobOrder::coalesce_desc("note", "created_at"), desc_ids),
    ] {
        let base = JobQuery::new().order(order).limit(3);
        let mut q = base.clone();
        let mut ids = Vec::new();
        let mut longest = 0usize;
        loop {
            let page = table.query(&conn, &q).unwrap();
            ids.extend(page.records.iter().map(|r| r.job.job_id.clone()));
            match page.next {
                Some(c) => {
                    longest = longest.max(c.as_str().len());
                    // The string form of an issued cursor is accepted again, unchanged.
                    let again = JobCursor::parse(c.as_str()).unwrap();
                    assert_eq!(again, c);
                    q = base.clone().after(again);
                }
                None => break,
            }
        }
        assert_eq!(ids, want, "no gap, no duplicate, over long keys and ids");
        assert!(
            longest > 4096,
            "the walk must issue a cursor past the old 4096-byte cap, longest was {longest}"
        );
    }
}
