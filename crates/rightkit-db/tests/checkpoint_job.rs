//! WAL checkpoints: RESTART through the helper, and the caller-driven job's PASSIVE and RESTART
//! choices, quiet-period rule and single-flight reset. Real WAL-mode SQLite files.

use rightkit_db::{
    checkpoint_restart, journal_mode, DbOptions, WalCheckpointJob, WalCheckpointMode,
    WalCheckpointPolicy,
};
use std::time::{Duration, Instant};

/// A WAL database with one row. The writer stays open so the `-wal` file exists.
fn wal_writer(dir: &tempfile::TempDir) -> (std::path::PathBuf, rightkit_db::rusqlite::Connection) {
    let path = dir.path().canonicalize().unwrap().join("app.sqlite");
    let conn = DbOptions::new(&path).open_writer().unwrap();
    assert_eq!(journal_mode(&conn).unwrap(), "wal");
    conn.execute_batch("CREATE TABLE t(v TEXT); INSERT INTO t VALUES ('x');")
        .unwrap();
    (path, conn)
}

fn policy(passive: u64, restart: u64, quiet: Duration) -> WalCheckpointPolicy {
    WalCheckpointPolicy {
        passive_above_bytes: passive,
        restart_above_bytes: restart,
        quiet_for: quiet,
    }
}

#[test]
fn checkpoint_restart_reports_a_checkpoint_without_busy_readers() {
    let dir = tempfile::tempdir().unwrap();
    let (_path, conn) = wal_writer(&dir);
    let status = checkpoint_restart(&conn).unwrap();
    assert_eq!(status.busy, 0);
}

#[test]
fn nothing_is_due_below_both_thresholds() {
    let dir = tempfile::tempdir().unwrap();
    let (path, conn) = wal_writer(&dir);
    let job = WalCheckpointJob::new(&path, policy(u64::MAX, u64::MAX, Duration::ZERO)).unwrap();
    assert!(job.tick_at(&conn, Instant::now()).unwrap().is_none());
}

#[test]
fn a_large_log_gets_a_passive_checkpoint_while_the_database_is_busy() {
    let dir = tempfile::tempdir().unwrap();
    let (path, conn) = wal_writer(&dir);
    let job = WalCheckpointJob::new(&path, policy(1, u64::MAX, Duration::from_secs(3600))).unwrap();
    job.note_activity_at(Instant::now());
    let run = job
        .tick_at(&conn, Instant::now())
        .unwrap()
        .expect("a checkpoint is due");
    assert_eq!(run.mode, WalCheckpointMode::Passive);
    assert!(run.wal_bytes > 0);
}

#[test]
fn restart_waits_for_the_quiet_period() {
    let dir = tempfile::tempdir().unwrap();
    let (path, conn) = wal_writer(&dir);
    let job = WalCheckpointJob::new(&path, policy(1, 1, Duration::from_secs(3600))).unwrap();
    job.note_activity_at(Instant::now());
    let run = job
        .tick_at(&conn, Instant::now())
        .unwrap()
        .expect("a checkpoint is due");
    assert_eq!(
        run.mode,
        WalCheckpointMode::Passive,
        "not quiet long enough for RESTART"
    );
}

#[test]
fn restart_runs_once_the_database_has_been_quiet() {
    let dir = tempfile::tempdir().unwrap();
    let (path, conn) = wal_writer(&dir);
    let job = WalCheckpointJob::new(&path, policy(1, 1, Duration::from_secs(1))).unwrap();
    let later = Instant::now() + Duration::from_secs(5);
    let run = job
        .tick_at(&conn, later)
        .unwrap()
        .expect("a checkpoint is due");
    assert_eq!(run.mode, WalCheckpointMode::Restart);
}

#[test]
fn a_finished_tick_releases_the_single_flight_guard() {
    let dir = tempfile::tempdir().unwrap();
    let (path, conn) = wal_writer(&dir);
    let job = WalCheckpointJob::new(&path, policy(1, u64::MAX, Duration::ZERO)).unwrap();
    assert!(job.tick_at(&conn, Instant::now()).unwrap().is_some());
    assert!(job.tick_at(&conn, Instant::now()).unwrap().is_some());
}
