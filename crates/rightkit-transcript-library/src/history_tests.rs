use super::*;
use crate::delivery::{DeliveryId, DeliveryOutcome};

fn rec(id: &str) -> DeliveryRecord {
    DeliveryRecord::for_test(DeliveryId::new(id), "t", DeliveryOutcome::Pasted)
}

#[test]
fn memory_store_push_is_newest_first_and_delete_works() {
    let mut s = MemoryHistoryStore::default();
    s.push(rec("a")).unwrap();
    s.push(rec("b")).unwrap();
    assert_eq!(s.recent()[0].delivery_id, "b");
    assert!(s.delete("a").unwrap());
    assert!(!s.delete("a").unwrap());
    assert_eq!(s.recent().len(), 1);
}

#[test]
fn memory_store_updates_transcript_by_id() {
    let mut s = MemoryHistoryStore::default();
    s.push(rec("a")).unwrap();
    assert!(s.update_transcript("a", "fixed".into()).unwrap());
    assert_eq!(s.recent()[0].transcript, "fixed");
    assert!(!s.update_transcript("missing", "x".into()).unwrap());
}

#[test]
fn trim_caps_at_max() {
    let mut v: Vec<DeliveryRecord> = (0..MAX_HISTORY_ITEMS + 50)
        .map(|i| rec(&format!("id-{i}")))
        .collect();
    trim_records(&mut v);
    assert_eq!(v.len(), MAX_HISTORY_ITEMS);
}

#[test]
fn migration_verified_uses_distinct_ids() {
    let records = vec![rec("dup"), rec("dup"), rec("unique")];
    // 2 distinct ids → DB should have 2 rows (INSERT OR IGNORE collapsed the dup).
    assert!(migration_verified(&records, 2));
    // Fewer rows than distinct → a record failed to migrate → keep .bak.
    assert!(!migration_verified(&records, 1));
    // Negative (query failed) → never safe to shred.
    assert!(!migration_verified(&records, -1));
}

#[test]
fn dictations_tab_hides_cancelled_and_cancelled_tab_shows_only_cancelled() {
    let mut store = MemoryHistoryStore::default();
    let mut cancelled = rec("cancelled-1");
    cancelled.outcome = DeliveryOutcome::Cancelled;
    store.push(cancelled).unwrap();
    store.push(rec("delivered-1")).unwrap();

    // Dictations tab (default): cancelled hidden.
    let dictations = store.query(&HistoryQuery::default()).unwrap();
    assert_eq!(
        dictations.total, 1,
        "cancelled record must be hidden by default"
    );
    assert_eq!(dictations.items[0].delivery_id, "delivered-1");

    // Cancelled tab: ONLY the cancelled record (not the delivered one) — this
    // is the fix for the "50 of 59" count bug: the count must equal exactly
    // the rows shown, so the tab is a server-side exclusive scope.
    let cancelled_tab = store
        .query(&HistoryQuery {
            filter: HistoryFilter {
                cancelled_only: true,
                ..HistoryFilter::default()
            },
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(cancelled_tab.total, 1, "cancelled tab shows only cancelled");
    assert_eq!(cancelled_tab.items[0].delivery_id, "cancelled-1");

    // Bulk delete respects the same default: an empty (match-all) filter
    // must not sweep up cancelled records.
    let deleted = store.delete_matching(&HistoryFilter::default()).unwrap();
    assert_eq!(deleted, 1, "default filtered delete must skip cancelled");
    assert_eq!(store.recent().len(), 1);
    assert_eq!(store.recent()[0].outcome, DeliveryOutcome::Cancelled);
}

#[test]
fn draft_records_are_hidden_in_both_tabs() {
    let mut store = MemoryHistoryStore::default();
    store
        .push(DeliveryRecord::new_draft("session-1", "partial so far"))
        .unwrap();
    store.push(rec("delivered-1")).unwrap();

    // Dictations tab: draft hidden.
    let dictations = store.query(&HistoryQuery::default()).unwrap();
    assert_eq!(dictations.total, 1, "draft hidden on the dictations tab");
    assert_eq!(dictations.items[0].delivery_id, "delivered-1");

    // Cancelled tab: drafts are internal crash-recovery snapshots, never a
    // user-visible outcome (a real crash sweeps them to Cancelled first), so
    // a raw draft never surfaces here either.
    let cancelled_tab = store
        .query(&HistoryQuery {
            filter: HistoryFilter {
                cancelled_only: true,
                ..HistoryFilter::default()
            },
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(
        cancelled_tab.total, 0,
        "raw draft never surfaces on the cancelled tab"
    );

    let deleted = store.delete_matching(&HistoryFilter::default()).unwrap();
    assert_eq!(deleted, 1, "default filtered delete must skip the draft");
    assert_eq!(store.recent().len(), 1);
    assert_eq!(store.recent()[0].outcome, DeliveryOutcome::Draft);
}

#[test]
fn upsert_draft_replaces_the_same_session_row_instead_of_accumulating() {
    let mut store = MemoryHistoryStore::default();
    store
        .upsert_draft("session-1", "hello".to_string())
        .unwrap();
    store
        .upsert_draft("session-1", "hello world".to_string())
        .unwrap();
    store
        .upsert_draft("session-1", "hello world done".to_string())
        .unwrap();

    // Drafts don't surface through any tab filter, so inspect raw rows.
    let rows = store.recent();
    assert_eq!(
        rows.len(),
        1,
        "repeated upsert_draft must not accumulate rows"
    );
    assert_eq!(rows[0].transcript, "hello world done");
    assert_eq!(rows[0].outcome, DeliveryOutcome::Draft);
}

#[test]
fn upsert_draft_for_two_sessions_keeps_two_separate_rows() {
    let mut store = MemoryHistoryStore::default();
    store
        .upsert_draft("session-1", "first session".to_string())
        .unwrap();
    store
        .upsert_draft("session-2", "second session".to_string())
        .unwrap();

    assert_eq!(store.recent().len(), 2);
}

#[test]
fn retention_cutoff_supports_locked_options_and_saturates() {
    const DAY_MS: u64 = 24 * 60 * 60 * 1_000;
    let now = 400 * DAY_MS;
    assert_eq!(HistoryRetention::Forever.cutoff_ms(now), None);
    assert_eq!(
        HistoryRetention::SevenDays.cutoff_ms(now),
        Some(393 * DAY_MS)
    );
    assert_eq!(
        HistoryRetention::ThirtyDays.cutoff_ms(now),
        Some(370 * DAY_MS)
    );
    assert_eq!(
        HistoryRetention::NinetyDays.cutoff_ms(now),
        Some(310 * DAY_MS)
    );
    assert_eq!(HistoryRetention::OneYear.cutoff_ms(now), Some(35 * DAY_MS));
    assert_eq!(HistoryRetention::OneYear.cutoff_ms(DAY_MS), Some(0));
}

#[test]
fn memory_query_paginates_and_bulk_delete_matches_filter() {
    let mut store = MemoryHistoryStore::default();
    for (id, text, timestamp) in [
        ("a", "alpha project", 1_000),
        ("b", "beta project", 2_000),
        ("c", "alpha follow-up", 3_000),
    ] {
        let mut record = rec(id);
        record.transcript = text.to_string();
        record.delivered_at_ms = timestamp;
        store.push(record).unwrap();
    }

    let query = HistoryQuery {
        filter: HistoryFilter {
            search: "alpha".to_string(),
            ..HistoryFilter::default()
        },
        offset: 1,
        limit: 1,
    };
    let page = store.query(&query).unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].delivery_id, "a");
    assert!(!page.has_more);

    let deleted = store.delete_matching(&query.filter).unwrap();
    assert_eq!(deleted, 2);
    assert_eq!(store.recent().len(), 1);
    assert_eq!(store.recent()[0].delivery_id, "b");
}

fn cloud_synced(id: &str, device: &str) -> DeliveryRecord {
    let mut record = rec(id);
    record.source = Some(crate::delivery::DeliverySource::CloudSync {
        device: device.to_string(),
    });
    record
}

#[test]
fn record_origin_defaults_absent_source_to_mac() {
    assert_eq!(record_origin(&rec("local")), HistoryOrigin::Mac);
}

#[test]
fn record_origin_reads_cloud_sync_device_case_insensitively() {
    assert_eq!(
        record_origin(&cloud_synced("a", "iPhone")),
        HistoryOrigin::IPhone
    );
    assert_eq!(
        record_origin(&cloud_synced("b", "WATCH")),
        HistoryOrigin::Watch
    );
    assert_eq!(record_origin(&cloud_synced("c", "Mac")), HistoryOrigin::Mac);
    // Unknown/future provenance fails closed instead of granting this Mac
    // mutation authority over a record it may not own.
    assert_eq!(
        record_origin(&cloud_synced("d", "unknown-thing")),
        HistoryOrigin::Unknown
    );
}

#[test]
fn delete_scoped_rejects_foreign_origin_and_deletes_local() {
    let mut store = MemoryHistoryStore::default();
    store.push(rec("mac-1")).unwrap();
    store.push(cloud_synced("iphone-1", "iPhone")).unwrap();

    // HR-HISTORY-006: Mac may delete/tombstone ONLY Mac-origin rows.
    let rejected = store
        .delete_scoped("iphone-1", MAC_DELETE_AUTHORITY)
        .unwrap();
    assert_eq!(rejected, ScopedDeleteOutcome::Rejected);
    assert!(
        store.find("iphone-1").is_some(),
        "a rejected scoped delete must not remove the foreign row"
    );

    let deleted = store.delete_scoped("mac-1", MAC_DELETE_AUTHORITY).unwrap();
    assert_eq!(deleted, ScopedDeleteOutcome::Deleted);
    assert!(store.find("mac-1").is_none());

    let missing = store
        .delete_scoped("does-not-exist", MAC_DELETE_AUTHORITY)
        .unwrap();
    assert_eq!(missing, ScopedDeleteOutcome::NotFound);
}

#[test]
fn update_scoped_rejects_foreign_origin_and_updates_local() {
    let mut store = MemoryHistoryStore::default();
    store.push(rec("mac-1")).unwrap();
    store.push(cloud_synced("iphone-1", "iPhone")).unwrap();

    let rejected = store
        .update_transcript_scoped("iphone-1", "must not land".into(), MAC_DELETE_AUTHORITY)
        .unwrap();
    assert_eq!(rejected, ScopedUpdateOutcome::Rejected);
    assert_ne!(store.find("iphone-1").unwrap().transcript, "must not land");

    let updated = store
        .update_transcript_scoped("mac-1", "fixed".into(), MAC_DELETE_AUTHORITY)
        .unwrap();
    assert_eq!(updated, ScopedUpdateOutcome::Updated);
    assert_eq!(store.find("mac-1").unwrap().transcript, "fixed");
}

#[test]
fn delete_matching_scoped_leaves_foreign_rows_and_removes_only_local_ones() {
    let mut store = MemoryHistoryStore::default();
    store.push(rec("mac-1")).unwrap();
    store.push(rec("mac-2")).unwrap();
    store.push(cloud_synced("iphone-1", "iPhone")).unwrap();
    store.push(cloud_synced("watch-1", "Watch")).unwrap();

    // "Delete all" scoped to this Mac's authority: matches every row
    // (empty filter) but must only actually remove the two Mac-origin
    // ones — HR-HISTORY-011 ("all locally-owned records in the current
    // source scope") + HR-HISTORY-007 ("Foreign records stay read-only").
    let deleted = store
        .delete_matching_scoped(&HistoryFilter::default(), MAC_DELETE_AUTHORITY)
        .unwrap();
    assert_eq!(deleted, 2, "only the two Mac-origin rows are deleted");
    assert_eq!(
        store.recent().len(),
        2,
        "the two foreign rows survive untouched"
    );
    assert!(store.find("iphone-1").is_some());
    assert!(store.find("watch-1").is_some());
    assert!(store.find("mac-1").is_none());
    assert!(store.find("mac-2").is_none());
}

#[test]
fn delete_matching_scoped_with_no_allowed_origins_deletes_nothing() {
    let mut store = MemoryHistoryStore::default();
    store.push(rec("mac-1")).unwrap();
    // Watch-side stores publish no tombstones at all (HR-HISTORY-006);
    // modeled here as an empty allowed-origins slice — nothing is ever
    // authorized, so nothing is ever deleted, no matter how broad the
    // filter is.
    let deleted = store
        .delete_matching_scoped(&HistoryFilter::default(), &[])
        .unwrap();
    assert_eq!(deleted, 0);
    assert_eq!(store.recent().len(), 1);
}
