//! Bounded scans: budgets, resumable cursors, exhaustion, and bounded text reads.

use rightkit_search::page::Cursor;
use rightkit_search::scan::{read_text_prefix, scan_page, ScanBudget, ScanPage};
use rightkit_search::Param;
use std::cell::Cell;
use std::convert::Infallible;
use std::path::Path;

fn key(x: &i64) -> Cursor {
    Cursor::new(vec![Param::Int(*x)])
}

/// Fetch over a sorted source. Counts every row handed back and records the largest
/// single request, so tests can check the budget.
fn source<'a>(
    data: &'a [i64],
    fetched: &'a Cell<usize>,
    max_request: &'a Cell<usize>,
) -> impl FnMut(Option<&Cursor>, usize) -> Result<Vec<i64>, Infallible> + 'a {
    move |after, n| {
        max_request.set(max_request.get().max(n));
        let start = match after {
            None => 0,
            Some(c) => match c.values() {
                [Param::Int(v)] => data.partition_point(|x| *x <= *v),
                other => panic!("unexpected cursor {other:?}"),
            },
        };
        let out: Vec<i64> = data[start..].iter().take(n).copied().collect();
        fetched.set(fetched.get() + out.len());
        Ok(out)
    }
}

fn run(
    data: &[i64],
    after: Option<Cursor>,
    limit: usize,
    budget: ScanBudget,
    matches: impl FnMut(&i64) -> bool,
) -> (ScanPage<i64>, usize, usize) {
    let fetched = Cell::new(0);
    let max_request = Cell::new(0);
    let page = scan_page(
        after,
        limit,
        budget,
        source(data, &fetched, &max_request),
        key,
        matches,
    )
    .unwrap();
    (page, fetched.get(), max_request.get())
}

/// Drain a scan one budgeted call at a time, returning every hit in order.
fn drain(
    data: &[i64],
    limit: usize,
    max_rows: usize,
    chunk: usize,
    matches: fn(&i64) -> bool,
) -> Vec<i64> {
    let mut out = Vec::new();
    let mut after = None;
    for _ in 0..1000 {
        let (page, _, _) = run(
            data,
            after,
            limit,
            ScanBudget::new(max_rows, chunk),
            matches,
        );
        assert!(
            page.scanned <= max_rows,
            "budget exceeded: {}",
            page.scanned
        );
        out.extend(page.hits);
        match page.next {
            Some(c) => after = Some(c),
            None => return out,
        }
    }
    panic!("scan did not terminate");
}

#[test]
fn budget_bounds_rows_fetched() {
    let data: Vec<i64> = (0..100).collect();
    let budget = ScanBudget::new(10, 4);
    let (page, fetched, max_request) = run(&data, None, 5, budget, |_| false);
    assert!(page.hits.is_empty());
    assert_eq!(page.scanned, 10);
    assert_eq!(fetched, 10, "rows fetched must not exceed the budget");
    assert!(
        max_request <= 4,
        "a single fetch asked for {max_request} rows"
    );
    assert!(page.next.is_some(), "budget ended, so the scan can resume");
}

#[test]
fn budget_exhaustion_returns_resumable_cursor() {
    let data: Vec<i64> = (0..50).collect();
    let is_seven = |x: &i64| x % 7 == 0;
    let expected: Vec<i64> = data.iter().copied().filter(|x| x % 7 == 0).collect();

    // Unbudgeted reference scan, paging by limit only.
    let unbudgeted = drain(&data, 3, usize::MAX, 1000, is_seven);
    assert_eq!(unbudgeted, expected);

    // Budgeted scan: small budget, small chunk, same page limit.
    let budgeted = drain(&data, 3, 9, 4, is_seven);
    assert_eq!(budgeted, unbudgeted);

    // Budget and limit both matter: a page filled by hits ends before the budget.
    let (page, _, _) = run(&data, None, 2, ScanBudget::new(9, 4), is_seven);
    assert_eq!(page.hits, vec![0, 7]);
    assert_eq!(page.scanned, 8);
    assert!(page.next.is_some());
}

#[test]
fn exhausted_source_returns_none() {
    let data: Vec<i64> = (0..5).collect();
    let (page, _, _) = run(&data, None, 100, ScanBudget::new(1000, 4), |_| true);
    assert_eq!(page.hits, data);
    assert_eq!(page.scanned, 5);
    assert_eq!(page.next, None, "fetch returned fewer rows than asked");

    // Resuming after the last row finds nothing and reports the end.
    let (page, _, _) = run(&data, Some(key(&4)), 100, ScanBudget::new(1000, 4), |_| {
        true
    });
    assert!(page.hits.is_empty());
    assert_eq!(page.scanned, 0);
    assert_eq!(page.next, None);
}

#[test]
fn full_page_on_last_row_then_empty_final_page() {
    let data: Vec<i64> = (0..3).collect();
    let (page, _, _) = run(&data, None, 3, ScanBudget::new(1000, 1000), |_| true);
    assert_eq!(page.hits, vec![0, 1, 2]);
    let next = page.next.expect("a full page returns a cursor");

    let (page, _, _) = run(&data, Some(next), 3, ScanBudget::new(1000, 1000), |_| true);
    assert!(page.hits.is_empty());
    assert_eq!(page.next, None);
}

#[test]
fn chunking_does_not_change_hits() {
    let data: Vec<i64> = (0..37).collect();
    let all_odd = |x: &i64| x % 2 == 1;
    let expected: Vec<i64> = data.iter().copied().filter(|x| x % 2 == 1).collect();
    for chunk in [1, 2, 5, 37, 100] {
        assert_eq!(
            drain(&data, 4, 6, chunk, all_odd),
            expected,
            "chunk {chunk}"
        );
    }
}

#[test]
fn zero_budget_and_limit_are_raised_to_one() {
    let data: Vec<i64> = (0..3).collect();
    let (page, _, _) = run(&data, None, 0, ScanBudget::new(0, 0), |_| true);
    assert_eq!(page.hits, vec![0]);
    assert_eq!(page.scanned, 1);
    assert!(page.next.is_some());
}

#[test]
fn fetch_error_is_returned() {
    let result: Result<ScanPage<i64>, &str> = scan_page(
        None,
        5,
        ScanBudget::new(10, 2),
        |_, _| Err("disk gone"),
        key,
        |_| true,
    );
    assert_eq!(result.unwrap_err(), "disk gone");
}

fn temp_file(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("body.txt");
    std::fs::write(&path, bytes).unwrap();
    (dir, path)
}

#[test]
fn multibyte_cut_at_cap_still_matches_earlier_text() {
    // "needle " is 7 bytes; each CJK character is 3 bytes.
    let content = "needle 東京東京".to_string();
    let (_dir, path) = temp_file(content.as_bytes());

    for cap in 0..=content.len() as u64 + 2 {
        let got = read_text_prefix(&path, cap).unwrap();
        let limit = (cap as usize).min(content.len());
        let cut = (0..=limit)
            .rev()
            .find(|&i| content.is_char_boundary(i))
            .unwrap();
        assert_eq!(got, &content[..cut], "cap {cap}");
        assert!(
            !got.contains('\u{FFFD}'),
            "cap {cap} kept a partial character"
        );
    }

    // A cut inside the first CJK character still matches the English prefix.
    let got = read_text_prefix(&path, 9).unwrap();
    assert!(got.contains("needle"));
    assert_eq!(got, "needle ");
}

#[test]
fn text_prefix_replaces_invalid_bytes_and_reads_whole_small_files() {
    let (_dir, path) = temp_file(b"a\xFFb");
    assert_eq!(read_text_prefix(&path, 100).unwrap(), "a\u{FFFD}b");

    let (_dir, path) = temp_file("plain".as_bytes());
    assert_eq!(read_text_prefix(&path, 100).unwrap(), "plain");
    assert_eq!(read_text_prefix(&path, 0).unwrap(), "");
}

#[test]
fn text_prefix_missing_file_is_an_error() {
    let missing = Path::new("/nonexistent/rightkit-search/body.txt");
    assert!(read_text_prefix(missing, 10).is_err());
}
