//! Read-only evidence over the isolated `host app.db` an InlineHost TUI
//! writes under the fixture suite root (`RIGHTKIT_SUITE_ROOT`).
//!
//! These helpers answer "did the runtime actually consume this input" — the
//! distinction between a slash command the interactive surface handled and
//! text that leaked to the provider as an ordinary prompt. Every read opens
//! a fresh read-only connection so evidence collected mid-run never holds a
//! stale snapshot, and WAL readers never block the owning process.

use rightkit_db::rusqlite;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Database file the isolated home's session store writes.
pub fn database(home: &str) -> PathBuf {
    app_root(home).join("rightkit-agent.db")
}

/// The the host app app root (`<home>/apps/the host app`) of the suite rooted at `home`.
pub fn app_root(home: &str) -> PathBuf {
    rightkit_app_settings::agent_config::test_isolation::app_root_in(Path::new(home))
}

fn connect(database: &Path) -> Option<rusqlite::Connection> {
    rusqlite::Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()
}

/// `seq` watermark of the newest durable event; `0` when the store or the
/// events table does not exist yet. Use it as a baseline: anything recorded
/// after it happened because of input the case just typed.
pub fn latest_seq(database: &Path) -> i64 {
    connect(database)
        .and_then(|connection| {
            connection
                .query_row("SELECT COALESCE(MAX(seq), 0) FROM events", [], |row| {
                    row.get(0)
                })
                .ok()
        })
        .unwrap_or(0)
}

pub fn latest_session_seq(database: &Path, session_id: &str) -> i64 {
    connect(database)
        .and_then(|connection| {
            connection
                .query_row(
                    "SELECT COALESCE(MAX(seq), 0) FROM events WHERE session_id = ?1",
                    [session_id],
                    |row| row.get(0),
                )
                .ok()
        })
        .unwrap_or(0)
}

/// `(seq, text)` of every `userMessageSubmitted` event recorded after
/// `after_seq`. A `/repo`, `/effort`, or `/model` line that lands here was
/// submitted to the provider as a prompt — proof the command surface failed
/// to consume it.
pub fn submissions_after(database: &Path, after_seq: i64) -> Vec<(i64, String)> {
    let Some(connection) = connect(database) else {
        return Vec::new();
    };
    let Ok(mut statement) = connection.prepare(
        "SELECT seq, json_extract(payload_json, '$.text') FROM events \
         WHERE seq > ?1 \
           AND json_extract(payload_json, '$.\"$kind\"') = 'userMessageSubmitted' \
         ORDER BY seq",
    ) else {
        return Vec::new();
    };
    statement
        .query_map([after_seq], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1).unwrap_or_default(),
            ))
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

/// Session-scoped submission fence: event sequence numbers are per session.
pub fn session_submissions_after(
    database: &Path,
    session_id: &str,
    after_seq: i64,
) -> Vec<(i64, String)> {
    let Some(connection) = connect(database) else {
        return Vec::new();
    };
    let Ok(mut statement) = connection.prepare(
        "SELECT seq, json_extract(payload_json, '$.text') FROM events \
         WHERE session_id = ?1 AND seq > ?2 \
           AND json_extract(payload_json, '$.\"$kind\"') = 'userMessageSubmitted' \
         ORDER BY seq",
    ) else {
        return Vec::new();
    };
    statement
        .query_map(rusqlite::params![session_id, after_seq], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

/// The one durable session whose transcript carries a `userMessageSubmitted`
/// event containing `marker`. Polls: the event ledger is journaled by the
/// live process and may flush a beat after the screen settles.
pub async fn session_for_marker(
    database: &Path,
    marker: &str,
    timeout: Duration,
) -> Result<String, String> {
    let deadline = std::time::Instant::now() + timeout;
    let pattern = format!("%{marker}%");
    loop {
        if let Some(connection) = connect(database) {
            if let Ok(mut statement) = connection.prepare(
                "SELECT DISTINCT session_id FROM events \
                 WHERE payload_json LIKE ?1 \
                   AND json_extract(payload_json, '$.\"$kind\"') = 'userMessageSubmitted'",
            ) {
                let sessions: Vec<String> = statement
                    .query_map([&pattern], |row| row.get(0))
                    .map(|rows| rows.flatten().collect())
                    .unwrap_or_default();
                if sessions.len() == 1 {
                    return Ok(sessions[0].clone());
                }
                if sessions.len() > 1 {
                    return Err(format!(
                        "marker {marker:?} maps to {count} durable sessions — the id must be unique per case",
                        count = sessions.len()
                    ));
                }
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "no durable userMessageSubmitted carries {marker:?} within {timeout:?}"
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// `(workspace_root, config_json)` for an exact session id, when its row
/// exists. `config_json` carries the session's committed model, effort, and
/// inference selection — the committed route, not a rendered label.
pub fn session_row(database: &Path, session_id: &str) -> Option<(String, Value)> {
    let connection = connect(database)?;
    let (root, raw): (String, String) = connection
        .query_row(
            "SELECT workspace_root, config_json FROM sessions WHERE id = ?1",
            [session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok()?;
    Some((root, serde_json::from_str(&raw).unwrap_or(Value::Null)))
}

/// Poll until the session's serialized `config_json` contains every needle.
/// On failure the error carries the last observed document so the receipt
/// records what the route actually committed.
pub async fn session_config_contains(
    database: &Path,
    session_id: &str,
    needles: &[&str],
    timeout: Duration,
) -> Result<String, String> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last = String::new();
    loop {
        if let Some((_root, config)) = session_row(database, session_id) {
            let serialized = config.to_string();
            if needles.iter().all(|needle| serialized.contains(needle)) {
                return Ok(serialized);
            }
            last = serialized;
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "session {session_id} config never carried {needles:?} within {timeout:?}; \
                 last observed: {last}"
            ));
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

/// Poll until a `sessionTurnFinished` event for `session_id` records
/// `status`. Used to prove a cancelled turn settled to a canonical terminal
/// status rather than being silently dropped from the ledger.
pub async fn finished_has_status(
    database: &Path,
    session_id: &str,
    statuses: &[&str],
    timeout: Duration,
) -> Result<(), String> {
    finished_has_status_after(database, session_id, 0, statuses, timeout).await
}

pub async fn finished_has_status_after(
    database: &Path,
    session_id: &str,
    after_seq: i64,
    statuses: &[&str],
    timeout: Duration,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + timeout;
    let mut observed: Vec<String> = Vec::new();
    loop {
        if let Some(connection) = connect(database) {
            if let Ok(mut statement) = connection.prepare(
                "SELECT payload_json FROM events WHERE session_id = ?1 AND seq > ?2 \
                   AND json_extract(payload_json, '$.\"$kind\"') = 'sessionTurnFinished' \
                 ORDER BY seq",
            ) {
                observed = statement
                    .query_map(rusqlite::params![session_id, after_seq], |row| {
                        row.get::<_, String>(0)
                    })
                    .map(|rows| {
                        rows.flatten()
                            .filter_map(|raw| serde_json::from_str::<Value>(&raw).ok())
                            .filter_map(|event| event["status"].as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                if observed
                    .iter()
                    .any(|status| statuses.iter().any(|want| status == want))
                {
                    return Ok(());
                }
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "session {session_id} recorded no sessionTurnFinished in {statuses:?} \
                 within {timeout:?}; observed statuses: {observed:?}"
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Windows path equality that survives canonicalization/8.3/casing drift:
/// canonicalize both sides and compare case-insensitively.
pub fn same_root(a: &str, b: &str) -> bool {
    let canonical = |path: &str| -> String {
        std::fs::canonicalize(path)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string())
            .to_lowercase()
    };
    canonical(a) == canonical(b)
}
