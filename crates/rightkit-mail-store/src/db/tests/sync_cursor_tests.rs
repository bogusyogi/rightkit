use super::*;
use rightkit_google_apis::gmail::{SyncOutcome, SyncResult, SyncWork};

fn full(history_id: Option<&str>) -> SyncWork {
    SyncWork::Full(SyncResult {
        outcome: SyncOutcome {
            messages: vec![msg("t1", &["INBOX"], true, 1)],
            skipped: Vec::new(),
        },
        labels: Vec::new(),
        history_id: history_id.map(str::to_string),
    })
}

// sync_folders captures the profile historyId before listing/fetching and
// carries it in SyncResult.history_id; the store must persist exactly that
// value (not any later one) once the pass applies.
#[test]
fn full_pass_stores_the_pre_fetch_cursor() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    apply_sync_work(&conn, "a@x.com", full(Some("100"))).unwrap();
    assert_eq!(
        get_history_id(&conn, "a@x.com").unwrap().as_deref(),
        Some("100")
    );
}

// A pass that produced no cursor (profile call failed) or never applied
// leaves the stored cursor unchanged.
#[test]
fn pass_without_a_cursor_leaves_stored_cursor_unchanged() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    set_history_id(&conn, "a@x.com", "50").unwrap();
    apply_sync_work(&conn, "a@x.com", full(None)).unwrap();
    assert_eq!(
        get_history_id(&conn, "a@x.com").unwrap().as_deref(),
        Some("50")
    );
}

#[test]
fn cursor_never_moves_backwards() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    set_history_id(&conn, "a@x.com", "900").unwrap();
    set_history_id(&conn, "a@x.com", "100").unwrap();
    assert_eq!(
        get_history_id(&conn, "a@x.com").unwrap().as_deref(),
        Some("900")
    );
    // Numeric, not lexicographic: "1000" > "900".
    set_history_id(&conn, "a@x.com", "1000").unwrap();
    assert_eq!(
        get_history_id(&conn, "a@x.com").unwrap().as_deref(),
        Some("1000")
    );
}
