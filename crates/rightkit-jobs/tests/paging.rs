//! Keyset paging: `JobStore::list_page`, `Cursor`, `ListPage`.

use std::collections::{HashMap, HashSet};

use rightkit_jobs::rusqlite::params;
use rightkit_jobs::{
    Cursor, ExtColumn, JobStore, ListFilter, NewJob, StoreConfig, CURSOR_MISMATCH, INVALID_CURSOR,
};
use serde_json::json;

fn new_job(request_id: Option<&str>) -> NewJob {
    NewJob {
        request_id: request_id.map(Into::into),
        kind: "scan".into(),
        request: json!({}),
    }
}

/// A `created_at` `s` seconds after 2026-01-01T00:00:00Z (under an hour).
fn stamp(s: usize) -> String {
    format!("2026-01-01T00:{:02}:{:02}Z", s / 60, s % 60)
}

/// Insert `n` finished rows in one transaction, `per_second` of them sharing each `created_at`
/// so the tie-breaker is exercised. Row `i` is `job-{i:05}`, `req-{i:05}`; every tenth is
/// `failed`, the rest `complete`. Insertion order is oldest first, so newest first is `i` down.
fn seed(store: &JobStore, n: usize, per_second: usize) -> Vec<String> {
    store
        .write(|c| {
            let mut ids = Vec::with_capacity(n);
            for i in 0..n {
                let id = format!("job-{i:05}");
                c.execute(
                    "INSERT INTO jobs (job_id, request_id, kind, request_json, status, attempts, \
                     created_at, updated_at) VALUES (?1, ?2, 'scan', '{}', ?3, 0, ?4, ?4)",
                    params![
                        id,
                        format!("req-{i:05}"),
                        if i % 10 == 0 { "failed" } else { "complete" },
                        stamp(i / per_second)
                    ],
                )?;
                ids.push(id);
            }
            Ok(ids)
        })
        .unwrap()
}

fn insert_raw(store: &JobStore, id: &str, created_at: &str) {
    store
        .write(|c| {
            c.execute(
                "INSERT INTO jobs (job_id, request_id, kind, request_json, status, attempts, \
                 created_at, updated_at) VALUES (?1, NULL, 'scan', '{}', 'complete', 0, ?2, ?2)",
                params![id, created_at],
            )?;
            Ok(())
        })
        .unwrap();
}

fn ids_of(page: &rightkit_jobs::ListPage) -> Vec<String> {
    page.records.iter().map(|r| r.job.job_id.clone()).collect()
}

fn walk(store: &JobStore, f: &ListFilter) -> Vec<Vec<String>> {
    let mut pages = Vec::new();
    let mut after: Option<Cursor> = None;
    loop {
        let page = store.list_page(f, after.as_ref()).unwrap();
        pages.push(ids_of(&page));
        match page.next {
            Some(c) => after = Some(c),
            None => break,
        }
    }
    pages
}

#[test]
fn walks_2500_rows_by_keyset_with_writes_between_pages() {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::open(dir.path().join("j.db")).unwrap();
    let ids = seed(&store, 2500, 7);
    // Newest first: `created_at` never decreases with `i` and `rowid` grows with it.
    let expected: Vec<String> = ids.iter().rev().cloned().collect();

    let filter = ListFilter {
        limit: Some(100),
        ..ListFilter::default()
    };
    let mut seen: Vec<String> = Vec::new();
    let mut status_seen: HashMap<String, String> = HashMap::new();
    let mut after: Option<Cursor> = None;
    let mut pages = 0;
    let mut fresh = String::new();
    loop {
        let page = store.list_page(&filter, after.as_ref()).unwrap();
        assert!(page.records.len() <= 100);
        for r in &page.records {
            seen.push(r.job.job_id.clone());
            status_seen.insert(r.job.job_id.clone(), r.job.status.clone());
        }
        pages += 1;

        // Writes between pages.
        match pages {
            1 => {
                // A new row: created now, so above everything already returned.
                fresh = store.submit(&new_job(None)).unwrap().0.job_id;
                // Behind the cursor (already returned) and ahead of it (not yet).
                store.delete(&ids[2499]).unwrap();
                store.delete(&ids[0]).unwrap();
                // Behind the cursor: changed after it was returned.
                store
                    .write(|c| {
                        c.execute(
                            "UPDATE jobs SET status='failed' WHERE job_id=?1",
                            [&ids[2451]],
                        )?;
                        Ok(())
                    })
                    .unwrap();
            }
            2 => {
                // Ahead of the cursor: changed before it is returned.
                store
                    .write(|c| {
                        c.execute(
                            "UPDATE jobs SET status='failed' WHERE job_id=?1",
                            [&ids[1201]],
                        )?;
                        Ok(())
                    })
                    .unwrap();
                // Backdated imports on either side of the cursor (it is near second 328).
                insert_raw(&store, "late-below", &stamp(120));
                insert_raw(&store, "late-above", &stamp(350));
            }
            _ => {}
        }

        match page.next {
            Some(c) => after = Some(c),
            None => break,
        }
    }

    let unique: HashSet<&String> = seen.iter().collect();
    assert_eq!(unique.len(), seen.len(), "a row was returned twice");
    assert!(
        !seen.contains(&fresh),
        "a row inserted above the cursor was returned"
    );
    assert!(!seen.contains(&"late-above".to_string()));
    assert_eq!(
        seen.iter().filter(|i| *i == "late-below").count(),
        1,
        "a row inserted below the cursor is returned once"
    );
    assert!(
        seen.contains(&ids[2499]),
        "returned before it was deleted: stays returned"
    );
    assert!(!seen.contains(&ids[0]), "deleted before it was reached");

    // Every untouched original appears once, in order, with nothing skipped.
    let originals: Vec<String> = seen
        .iter()
        .filter(|i| i.starts_with("job-"))
        .cloned()
        .collect();
    let want: Vec<String> = expected.iter().filter(|i| **i != ids[0]).cloned().collect();
    assert_eq!(originals, want);

    // A row changed behind the cursor was seen in its old state, one changed ahead in its new.
    assert_eq!(status_seen[&ids[2451]], "complete");
    assert_eq!(status_seen[&ids[1201]], "failed");

    // 2,499 originals and the one late row below the cursor, 100 per page, no empty last page.
    assert_eq!(seen.len(), 2500);
    assert_eq!(pages, 25);
}

#[test]
fn the_unpaged_call_is_the_first_page() {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::open(dir.path().join("j.db")).unwrap();
    let ids = seed(&store, 30, 4);

    // A full literal still builds: ListFilter gained no field.
    let f = ListFilter {
        limit: Some(10),
        status: None,
        request_id: None,
    };
    let (records, truncated) = store.list_records(&f).unwrap();
    let page = store.list_page(&f, None).unwrap();
    let first: Vec<String> = records.iter().map(|r| r.job.job_id.clone()).collect();
    assert_eq!(first, ids_of(&page));
    assert_eq!(
        first,
        ids.iter().rev().take(10).cloned().collect::<Vec<_>>()
    );
    assert!(truncated);
    assert!(page.next.is_some());

    // `list` is the same rows as jobs, with the same flag.
    let (jobs, t) = store.list(&f).unwrap();
    assert!(t);
    assert_eq!(
        jobs.iter().map(|j| j.job_id.clone()).collect::<Vec<_>>(),
        first
    );

    // Default limit 50 covers 30 rows: complete, no cursor.
    let (all, truncated) = store.list_records(&ListFilter::default()).unwrap();
    assert_eq!(all.len(), 30);
    assert!(!truncated);
    assert!(store
        .list_page(&ListFilter::default(), None)
        .unwrap()
        .next
        .is_none());

    // `list_records` given a position is the page after it.
    let second = store.list_page(&f, page.next.as_ref()).unwrap();
    assert_eq!(
        ids_of(&second),
        ids.iter()
            .rev()
            .skip(10)
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
    );
}

#[test]
fn the_row_cap_is_per_page_not_per_walk() {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::open(dir.path().join("j.db")).unwrap();
    seed(&store, 1100, 10);

    let big = ListFilter {
        limit: Some(5000),
        ..ListFilter::default()
    };
    let (rows, truncated) = store.list_records(&big).unwrap();
    assert_eq!(rows.len(), 1000, "the cap is unchanged");
    assert!(truncated);

    let pages = walk(&store, &big);
    assert_eq!(
        pages.iter().map(Vec::len).collect::<Vec<_>>(),
        vec![1000, 100]
    );

    let one = ListFilter {
        limit: Some(0),
        ..ListFilter::default()
    };
    assert_eq!(walk(&store, &one).len(), 1100, "a limit below 1 is 1");
}

#[test]
fn a_walk_ends_without_an_empty_page() {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::open(dir.path().join("j.db")).unwrap();
    let f = ListFilter {
        limit: Some(10),
        ..ListFilter::default()
    };
    assert_eq!(
        walk(&store, &f),
        vec![Vec::<String>::new()],
        "no rows: one empty page"
    );

    seed(&store, 20, 3);
    assert_eq!(
        walk(&store, &f).iter().map(Vec::len).collect::<Vec<_>>(),
        vec![10, 10]
    );
    insert_raw(&store, "extra", &stamp(0));
    assert_eq!(
        walk(&store, &f).iter().map(Vec::len).collect::<Vec<_>>(),
        vec![10, 10, 1]
    );
}

#[test]
fn rows_sharing_a_second_are_neither_skipped_nor_repeated() {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::open(dir.path().join("j.db")).unwrap();
    let ids = seed(&store, 50, 50); // one created_at for all fifty
    let f = ListFilter {
        limit: Some(7),
        ..ListFilter::default()
    };
    let flat: Vec<String> = walk(&store, &f).into_iter().flatten().collect();
    assert_eq!(flat, ids.iter().rev().cloned().collect::<Vec<_>>());
}

#[test]
fn filters_page_too_and_bind_their_cursors() {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::open(dir.path().join("j.db")).unwrap();
    let ids = seed(&store, 100, 5);

    let failed = ListFilter {
        limit: Some(3),
        status: Some("failed".into()),
        request_id: None,
    };
    let pages = walk(&store, &failed);
    assert_eq!(
        pages.iter().map(Vec::len).collect::<Vec<_>>(),
        vec![3, 3, 3, 1]
    );
    let want: Vec<String> = (0..100)
        .rev()
        .filter(|i| i % 10 == 0)
        .map(|i| ids[i].clone())
        .collect();
    assert_eq!(pages.into_iter().flatten().collect::<Vec<_>>(), want);

    let one = ListFilter {
        request_id: Some("req-00042".into()),
        ..ListFilter::default()
    };
    let page = store.list_page(&one, None).unwrap();
    assert_eq!(ids_of(&page), vec![ids[42].clone()]);
    assert!(page.next.is_none());

    // A cursor belongs to the filter it was issued for.
    let everyone = ListFilter {
        limit: Some(5),
        ..ListFilter::default()
    };
    let from_all = store.list_page(&everyone, None).unwrap().next.unwrap();
    let from_failed = store.list_page(&failed, None).unwrap().next.unwrap();
    for (cursor, wrong) in [
        (&from_all, &failed),
        (&from_failed, &everyone),
        (
            &from_all,
            &ListFilter {
                request_id: Some("req-00042".into()),
                ..everyone.clone()
            },
        ),
        (
            &from_failed,
            &ListFilter {
                status: Some("complete".into()),
                ..failed.clone()
            },
        ),
    ] {
        let e = store.list_page(wrong, Some(cursor)).unwrap_err();
        assert_eq!(e.code, CURSOR_MISMATCH, "{e}");
    }

    // The page size is not part of the position.
    let bigger = ListFilter {
        limit: Some(40),
        ..everyone.clone()
    };
    let next = store.list_page(&bigger, Some(&from_all)).unwrap();
    assert_eq!(
        ids_of(&next),
        ids.iter()
            .rev()
            .skip(5)
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_cursor_is_a_string_and_a_bad_one_is_a_typed_error() {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::open(dir.path().join("j.db")).unwrap();
    seed(&store, 30, 3);
    let f = ListFilter {
        limit: Some(10),
        ..ListFilter::default()
    };
    let cursor = store.list_page(&f, None).unwrap().next.unwrap();

    // Hand it to a UI as JSON (or text) and take it back.
    let json = serde_json::to_string(&cursor).unwrap();
    let back: Cursor = serde_json::from_str(&json).unwrap();
    assert_eq!(back, cursor);
    let parsed: Cursor = cursor.to_string().parse().unwrap();
    assert_eq!(parsed, cursor);
    assert_eq!(Cursor::parse(cursor.as_str()).unwrap(), cursor);
    assert_eq!(
        ids_of(&store.list_page(&f, Some(&back)).unwrap()),
        ids_of(&store.list_page(&f, Some(&cursor)).unwrap())
    );

    for bad in ["", "nonsense", "rk1.cd.-.-.zz.1", "rk9.cd.-.-.00.1"] {
        assert_eq!(
            Cursor::parse(bad).unwrap_err().code,
            INVALID_CURSOR,
            "{bad:?}"
        );
        let from_ui: Cursor = serde_json::from_str(&serde_json::to_string(bad).unwrap()).unwrap();
        assert_eq!(
            store.list_page(&f, Some(&from_ui)).unwrap_err().code,
            INVALID_CURSOR,
            "{bad:?}"
        );
    }
    // Cut short in transit.
    let cut = &cursor.as_str()[..cursor.as_str().len() - 3];
    assert_eq!(Cursor::parse(cut).unwrap_err().code, INVALID_CURSOR);
}

#[test]
fn a_cursor_outlives_the_row_it_was_cut_from() {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::open(dir.path().join("j.db")).unwrap();
    let ids = seed(&store, 30, 3);
    let f = ListFilter {
        limit: Some(10),
        ..ListFilter::default()
    };
    let first = store.list_page(&f, None).unwrap();
    let last = first.records.last().unwrap().job.job_id.clone();
    assert_eq!(last, ids[20]);
    store.delete(&last).unwrap();
    assert!(store.get(&last).is_err());

    let second = store.list_page(&f, first.next.as_ref()).unwrap();
    assert_eq!(
        ids_of(&second),
        ids.iter()
            .rev()
            .skip(10)
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
    );
}

#[test]
fn extension_columns_ride_along() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = StoreConfig::new()
        .column(ExtColumn::text("owner"))
        .column(ExtColumn::integer("n"));
    let store = JobStore::open_configured(dir.path().join("j.db"), cfg).unwrap();
    let mut made = Vec::new();
    for n in 0..12 {
        let mut ext = rightkit_jobs::Ext::new();
        ext.insert("owner".into(), json!(format!("o{n}")));
        ext.insert("n".into(), json!(n));
        made.push(store.submit_ext(&new_job(None), &ext).unwrap().0.job.job_id);
    }
    let f = ListFilter {
        limit: Some(5),
        ..ListFilter::default()
    };
    let mut seen = Vec::new();
    let mut after = None;
    loop {
        let page = store.list_page(&f, after.as_ref()).unwrap();
        for r in &page.records {
            let n = r.ext["n"].as_i64().unwrap();
            assert_eq!(r.ext_str("owner"), Some(format!("o{n}").as_str()));
            seen.push(r.job.job_id.clone());
        }
        match page.next {
            Some(c) => after = Some(c),
            None => break,
        }
    }
    // Same-second submits tie on `created_at`; `rowid` puts the newest first.
    assert_eq!(seen, made.into_iter().rev().collect::<Vec<_>>());
}
