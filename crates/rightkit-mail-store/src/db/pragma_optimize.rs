//! `PRAGMA optimize` boundary hook (L9, 2026-07-04).
//!
//! Plan stats refresh — connection open uses `PRAGMA optimize=0x10002`, while
//! periodic boundaries use bare `PRAGMA optimize`. Called
//! at three boundaries:
//!   1. after every schema migration (see `migrations::migrate`)
//!   2. on app quit next to the quit checkpoint (see `desktop::quit_app`)
//!   3. after a download-all completes (see `sync::backfill::run_download_all_step`)
//!
//! There is intentionally NO scheduled ANALYZE cron — the three call sites
//! are the boundaries where the row distribution shifts enough to invalidate
//! planner stats. `PRAGMA optimize` itself is self-limiting (no-op when
//! sqlite_stat1 is fresh), so re-calling at every boundary is free.

use rightkit_db::rusqlite::Connection;

/// Refresh planner statistics at a sync/migration/quit boundary. Best-effort
/// — the operation never fails the caller (errors are swallowed; the planner
/// just runs with stale stats, identical to having no `sqlite_stat1`).
pub fn pragma_optimize(conn: &Connection) {
    let _ = rightkit_db::optimize_boundary(conn);
}
