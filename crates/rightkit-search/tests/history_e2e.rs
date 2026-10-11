//! End to end, scan strategy: a SQLite history table plus transcript files in a temporary
//! directory. The table filters by operator in SQL; the free text is matched in Rust, with
//! the title checked eagerly and the body read from disk only when needed. Pages must
//! concatenate to the full hit set, and a body hit must yield a snippet.

use rightkit_search::fold::fold;
use rightkit_search::page::{Cursor, Keyset};
use rightkit_search::scan::{read_text_prefix, scan_page, ScanBudget, ScanPage};
use rightkit_search::{Binding, Compiled, Param, Schema, TextMatcher};
use rusqlite::{params, params_from_iter, Connection};
use std::error::Error;
use std::fs;
use std::path::Path;

/// Body read cap for each transcript file.
const BODY_CAP: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
struct Entry {
    id: i64,
    created_at: i64,
    title: String,
    file: String,
}

/// (id, created_at, kind, title, transcript body). `None` writes no file, so the body is
/// unreadable and counts as empty.
const ROWS: &[(i64, i64, &str, &str, Option<&str>)] = &[
    (1, 500, "note", "Standup", Some("nothing relevant")),
    (2, 490, "note", "Café planning", Some("agenda")),
    (3, 480, "chat", "Lunch", Some("meet at the CAFÉ")),
    (
        4,
        470,
        "note",
        "Budget",
        Some("Ledger from Café Ledger review"),
    ),
    (5, 460, "note", "Random", Some("empty")),
    (6, 450, "note", "Notes", Some("a café chain expansion")),
    (7, 440, "note", "Ledger", Some("ledger only")),
    (8, 430, "note", "café", Some("x")),
    (9, 420, "note", "Quiet", None),
    (10, 400, "note", "Café tie A", Some("x")),
    (11, 400, "note", "Café tie B", Some("x")),
];

fn setup(dir: &Path) -> Connection {
    let conn = Connection::open(dir.join("history.db")).unwrap();
    conn.execute_batch(
        "CREATE TABLE history(
            id INTEGER PRIMARY KEY,
            created_at INTEGER NOT NULL,
            kind TEXT NOT NULL,
            title TEXT NOT NULL,
            file TEXT NOT NULL
        );",
    )
    .unwrap();
    for &(id, created_at, kind, title, body) in ROWS {
        let file = format!("t{id}.txt");
        conn.execute(
            "INSERT INTO history(id, created_at, kind, title, file) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, created_at, kind, title, file],
        )
        .unwrap();
        if let Some(body) = body {
            fs::write(dir.join(&file), body).unwrap();
        }
    }
    conn
}

fn newest_first() -> Keyset {
    Keyset::new().desc("created_at").desc("id")
}

fn key(e: &Entry) -> Cursor {
    Cursor::new(vec![Param::Int(e.created_at), Param::Int(e.id)])
}

fn body(dir: &Path, e: &Entry) -> String {
    read_text_prefix(&dir.join(&e.file), BODY_CAP).unwrap_or_default()
}

/// One keyset page of at most `n` rows, run against SQLite.
fn fetch_rows(
    conn: &Connection,
    filters: &Compiled,
    ks: &Keyset,
    after: Option<&Cursor>,
    n: usize,
) -> Result<Vec<Entry>, Box<dyn Error>> {
    let p = filters.page(
        "SELECT id, created_at, title, file FROM history",
        ks,
        after,
        n,
    )?;
    let mut stmt = conn.prepare(&p.sql)?;
    let mut rows = stmt
        .query_map(params_from_iter(p.params.iter()), |r| {
            Ok(Entry {
                id: r.get(0)?,
                created_at: r.get(1)?,
                title: r.get(2)?,
                file: r.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    // `page` binds n + 1 to detect a following page; the scan wants at most n.
    rows.truncate(n);
    Ok(rows)
}

/// One bounded scan page: the title is checked first, the body only when undecided.
fn run_scan(
    conn: &Connection,
    filters: &Compiled,
    ks: &Keyset,
    matcher: &TextMatcher,
    dir: &Path,
    after: Option<Cursor>,
    limit: usize,
    budget: ScanBudget,
) -> Result<ScanPage<Entry>, Box<dyn Error>> {
    scan_page(
        after,
        limit,
        budget,
        |cursor, n| fetch_rows(conn, filters, ks, cursor, n),
        key,
        |e| matcher.matches_with(&[e.title.as_str()], || vec![body(dir, e)]),
    )
}

/// Drain every page, following cursors until one is absent.
fn all_pages(
    conn: &Connection,
    filters: &Compiled,
    ks: &Keyset,
    matcher: &TextMatcher,
    dir: &Path,
    limit: usize,
    budget: ScanBudget,
) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut cursor = None;
    let mut calls = 0;
    loop {
        calls += 1;
        assert!(calls < 100, "scan did not terminate");
        let page = run_scan(
            conn,
            filters,
            ks,
            matcher,
            dir,
            cursor.take(),
            limit,
            budget,
        )
        .expect("scan page");
        assert!(page.hits.len() <= limit);
        out.extend(page.hits);
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    out
}

#[test]
fn pages_concatenate_to_full_hit_set_and_body_hit_has_snippet() {
    let dir = tempfile::tempdir().unwrap();
    let conn = setup(dir.path());
    let schema = Schema::new().bind("kind", Binding::Eq("kind"));
    let q = schema.parse("café -budget kind:note").unwrap();

    // Build the matcher from the full query first, then the operator filters.
    let matcher = TextMatcher::new(&q).expect("no mixed OR groups");
    let filters = schema.compile(&q.filters_only()).unwrap();
    let ks = newest_first();

    let full = run_scan(
        &conn,
        &filters,
        &ks,
        &matcher,
        dir.path(),
        None,
        1000,
        ScanBudget::new(1000, 1000),
    )
    .unwrap();
    assert!(full.next.is_none(), "source exhausted in one page");
    let full_ids: Vec<i64> = full.hits.iter().map(|e| e.id).collect();
    // Row 3 is a chat, row 4 has "budget" in its title, row 9 has no body but its title
    // does not match. Equal created_at (10, 11) is ordered by id, newest first.
    assert_eq!(full_ids, vec![2, 6, 8, 11, 10]);

    // Budget of 3 rows per call and pages of 2 force several resumptions.
    let paged = all_pages(
        &conn,
        &filters,
        &ks,
        &matcher,
        dir.path(),
        2,
        ScanBudget::new(3, 2),
    );
    let paged_ids: Vec<i64> = paged.iter().map(|e| e.id).collect();
    assert_eq!(
        paged_ids, full_ids,
        "pages must concatenate to the full set"
    );

    // Row 6 matches in the body only, so its snippet must come from the transcript.
    let body_hit = full.hits.iter().find(|e| e.id == 6).expect("row 6 hit");
    assert!(!matcher.matches_with(&[body_hit.title.as_str()], Vec::new));
    let text = body(dir.path(), body_hit);
    let snip = matcher
        .snippet(&text, 40)
        .expect("body hit yields a snippet");
    assert!(snip
        .spans
        .iter()
        .any(|s| s.matched && fold(&s.text) == fold("café")));
    let rebuilt: String = snip.spans.iter().map(|s| s.text.as_str()).collect();
    assert!(text.contains(&rebuilt));
}
