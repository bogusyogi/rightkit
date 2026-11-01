use super::*;

#[test]
fn sync_lease_acquire_and_contention() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    assert!(
        try_acquire_sync_lease(&conn, "a@x.com", "s1", 60_000).unwrap(),
        "first claim acquires"
    );
    assert!(
        !try_acquire_sync_lease(&conn, "a@x.com", "s2", 60_000).unwrap(),
        "second claim blocked by the live lease"
    );
    release_sync_lease(&conn, "a@x.com").unwrap();
    assert!(
        try_acquire_sync_lease(&conn, "a@x.com", "s2", 60_000).unwrap(),
        "claim succeeds once released"
    );
}

#[test]
fn expired_sync_lease_is_reclaimable() {
    let conn = test_conn();
    upsert_account(&conn, "a@x.com").unwrap();
    // A ttl in the past means the lease is already expired the instant it is set.
    assert!(try_acquire_sync_lease(&conn, "a@x.com", "s1", -1).unwrap());
    assert!(
        try_acquire_sync_lease(&conn, "a@x.com", "s2", 60_000).unwrap(),
        "an expired lease can be reclaimed by another owner"
    );
}

// Debug helper: print the newest audit_log rows from a COPY of the live DB —
// used to confirm the background scheduler actually ran ("bg synced …").
// Never touches the live file.
// Run: cargo test --lib --features live-tests -- --ignored --nocapture dump_recent_audit
