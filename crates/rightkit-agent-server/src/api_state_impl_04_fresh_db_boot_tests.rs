//! Repro + regression coverage for audit B-1 (fresh-DB cold-start panic).
//!
//! The original audit claim ("api_state_types.rs:320 panics on journal
//! replay against a fresh-install DB, 'Cannot block the current thread
//! from within a runtime'") pointed at a drifted line — `api_state_types.rs:320`
//! is `SharedStorage::blocking_lock`'s own definition, not a panic site, and
//! journal replay itself (`journal.rs`) is fully `Result`/`Option`-safe.
//!
//! The REAL panic path: `create_session_seeded_with_options`
//! (`api_state_impl_04.rs`) calls `tokio::task::block_in_place` to persist the
//! initial session row when storage is attached. `block_in_place` panics with
//! exactly that message ("Cannot block the current thread from within a
//! runtime") whenever it runs under a **current-thread** Tokio runtime —
//! which is exactly the flavor a plain `#[tokio::test]` (no `flavor =
//! "multi_thread"`) uses, and exactly what a headless boot/QA harness that
//! doesn't hand-pick a multi-thread runtime would use. Every existing test
//! that pairs `with_storage(...)` with `create_session*` was already opting
//! into `#[tokio::test(flavor = "multi_thread")]` to dodge this — the
//! underlying landmine was worked around, never fixed.
//!
//! This file proves the failure mode on the DEFAULT (current-thread) test
//! flavor against a genuinely fresh on-disk DB, and locks the fix: creating a
//! session with real storage attached must never panic, regardless of the
//! caller's runtime flavor.

#[cfg(test)]
mod fresh_db_boot_tests {
    use crate::api::{DaemonState, SharedStorage};

    fn unique_temp_dir(label: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir()
            .join("coderight-test-tmp")
            .join(format!("coderight-{label}-{nanos}"))
    }

    /// The exact repro: a genuinely fresh on-disk DB file (never opened
    /// before — the "fresh-install" condition), storage attached, and a
    /// session created under the DEFAULT current-thread test runtime (no
    /// `flavor = "multi_thread"`). Before the fix this panics with
    /// "Cannot block the current thread from within a runtime" the moment
    /// `create_session` tries to persist the initial session row.
    #[tokio::test]
    async fn create_session_on_fresh_disk_db_does_not_panic_under_current_thread_runtime() {
        let dir = unique_temp_dir("fresh-db-cold-start");
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("fresh.db");
        assert!(
            !db_path.exists(),
            "the DB file must not exist yet — this is the fresh-install condition"
        );

        let storage = rightkit_agent_engine_storage::Storage::open(&db_path)
            .expect("opening a brand-new on-disk DB must succeed (runs migrations cold)");
        let shared = SharedStorage::new(storage);

        let state = DaemonState::new("test-token").with_storage(shared);

        // This is the call that used to panic: create_session ->
        // create_session_with_options -> create_session_seeded_with_options
        // -> block_in_place(|| storage.blocking_lock()...) under a
        // current-thread runtime.
        let live = state.create_session(
            "cold-start-session",
            None,
            dir.to_str().unwrap(),
            "Cold start",
            None,
        );
        assert_eq!(live.id, "cold-start-session");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Same repro, but explicitly on `flavor = "multi_thread"` — the shape
    /// every pre-existing storage+create_session test already used to avoid
    /// the bug. Must keep passing after the fix (no behavior regression on
    /// the runtime flavor that already worked).
    #[tokio::test(flavor = "multi_thread")]
    async fn create_session_on_fresh_disk_db_still_works_under_multi_thread_runtime() {
        let dir = unique_temp_dir("fresh-db-cold-start-mt");
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("fresh.db");

        let storage = rightkit_agent_engine_storage::Storage::open(&db_path).expect("open fresh db");
        let shared = SharedStorage::new(storage);
        let state = DaemonState::new("test-token").with_storage(shared);

        let live = state.create_session(
            "cold-start-session-mt",
            None,
            dir.to_str().unwrap(),
            "Cold start MT",
            None,
        );
        assert_eq!(live.id, "cold-start-session-mt");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The persisted session row itself must eventually land — since the fix
    /// makes the initial persist fire-and-forget (spawned), we poll briefly
    /// rather than asserting synchronously right after `create_session`
    /// returns.
    #[tokio::test]
    async fn cold_start_session_row_is_eventually_persisted() {
        let dir = unique_temp_dir("fresh-db-row-persist");
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("fresh.db");

        let storage = rightkit_agent_engine_storage::Storage::open(&db_path).expect("open fresh db");
        let shared = SharedStorage::new(storage);
        let state = DaemonState::new("test-token").with_storage(shared.clone());

        let _live = state.create_session(
            "row-persist-session",
            None,
            dir.to_str().unwrap(),
            "Row persist",
            None,
        );

        let mut found = false;
        for _ in 0..50 {
            let exists = {
                let s = shared.lock().await;
                s.get_session("row-persist-session")
                    .ok()
                    .flatten()
                    .is_some()
            };
            if exists {
                found = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            found,
            "the initial session row must be persisted even though the write is fire-and-forget"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
