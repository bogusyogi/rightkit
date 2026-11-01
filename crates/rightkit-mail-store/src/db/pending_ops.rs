use super::job_tables::{pending_ops_table, purge_terminal, scan_due};
use super::*;
use rightkit_job_queue::{Expect, Ext, NewJob, SubmitOptions, Transition};
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct PendingOpRow {
    pub id: String,
    #[cfg(test)]
    pub account_id: String,
    pub op_class: String,
    #[cfg(test)]
    pub action: String,
    pub thread_id: String,
    pub add_labels: Vec<String>,
    pub rem_labels: Vec<String>,
}

fn labels(request: &Value, key: &str) -> Vec<String> {
    serde_json::from_value::<Vec<String>>(request.get(key).cloned().unwrap_or(Value::Null))
        .unwrap_or_default()
}

/// Enqueue a durable mutation so it can be replayed when the network is back.
/// Idempotent: a second enqueue with the same id is ignored.
pub fn pending_ops_enqueue(
    conn: &Connection,
    id: &str,
    account_id: &str,
    action: &str,
    thread_id: &str,
    add_labels: &[&str],
    rem_labels: &[&str],
) -> Result<(), String> {
    let now = now_ms();
    let mut ext = Ext::new();
    ext.insert("account_id".into(), json!(account_id));
    ext.insert("created_ms".into(), json!(now));
    ext.insert("next_attempt_at".into(), json!(now));
    let job = NewJob {
        request_id: Some(id.to_string()),
        kind: "label_modify".into(),
        request: json!({
            "action": action,
            "thread_id": thread_id,
            "add_labels": add_labels,
            "rem_labels": rem_labels,
        }),
    };
    pending_ops_table()
        .enqueue(conn, &job, &ext, &SubmitOptions::default())
        .map(|_| ())
        .map_err(|e| format!("pending_ops enqueue: {e}"))
}

/// Fetch up to `limit` due pending ops for replay under an attempt cap.
/// Only selects by account (scheduler replays per account/lease).
pub fn pending_ops_due(
    conn: &Connection,
    account_id: &str,
    max_attempts: i64,
    limit: i64,
) -> Result<Vec<PendingOpRow>, String> {
    let recs = scan_due(conn, pending_ops_table()).map_err(|e| format!("pending_ops_due: {e}"))?;
    Ok(recs
        .into_iter()
        .filter(|r| r.ext_str("account_id") == Some(account_id) && r.job.attempts < max_attempts)
        .take(usize::try_from(limit).unwrap_or(0))
        .map(|r| PendingOpRow {
            id: r
                .job
                .request_id
                .clone()
                .unwrap_or_else(|| r.job.job_id.clone()),
            #[cfg(test)]
            account_id: account_id.to_string(),
            op_class: r.job.kind.clone(),
            #[cfg(test)]
            action: r
                .job
                .request
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            thread_id: r
                .job
                .request
                .get("thread_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            add_labels: labels(&r.job.request, "add_labels"),
            rem_labels: labels(&r.job.request, "rem_labels"),
        })
        .collect())
}

/// Bump attempt count after a failed replay attempt.
pub fn pending_ops_bump_attempt(conn: &Connection, id: &str) -> Result<(), String> {
    let table = pending_ops_table();
    let Some(rec) = table
        .find_by_request(conn, id)
        .map_err(|e| format!("pending_ops bump: {e}"))?
    else {
        return Ok(());
    };
    table
        .transition(
            conn,
            &rec.job.job_id,
            Expect::new(),
            &Transition::keep().count_attempt(),
        )
        .map(|_| ())
        .map_err(|e| format!("pending_ops bump: {e}"))
}

/// Remove a pending op after a successful replay.
pub fn pending_ops_remove(conn: &Connection, id: &str) -> Result<(), String> {
    let table = pending_ops_table();
    let Some(rec) = table
        .find_by_request(conn, id)
        .map_err(|e| format!("pending_ops remove: {e}"))?
    else {
        return Ok(());
    };
    table
        .complete(conn, &rec.job.job_id, Expect::new(), &json!({}))
        .map_err(|e| format!("pending_ops remove: {e}"))?;
    purge_terminal(conn, table, &rec.job.job_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_ops_enqueue_is_idempotent() {
        let conn = test_conn();
        upsert_account(&conn, "a@x.com").unwrap();

        pending_ops_enqueue(&conn, "op1", "a@x.com", "archive", "t1", &["INBOX"], &[]).unwrap();
        // Second enqueue with the same id must be silently ignored (INSERT OR IGNORE).
        pending_ops_enqueue(&conn, "op1", "a@x.com", "archive", "t1", &["INBOX"], &[]).unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM pending_ops WHERE request_id = 'op1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "duplicate enqueue must not insert a second row");
    }

    #[test]
    fn pending_ops_due_returns_due_rows_with_op_class() {
        let conn = test_conn();
        upsert_account(&conn, "a@x.com").unwrap();
        upsert_account(&conn, "b@x.com").unwrap();

        pending_ops_enqueue(&conn, "op-due", "a@x.com", "trash", "t2", &[], &["INBOX"]).unwrap();
        // This op belongs to a different account and must not appear in a@x.com's results.
        pending_ops_enqueue(
            &conn,
            "op-other",
            "b@x.com",
            "archive",
            "t3",
            &["INBOX"],
            &[],
        )
        .unwrap();

        let rows = pending_ops_due(&conn, "a@x.com", 5, 10).unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.id, "op-due");
        assert_eq!(row.account_id, "a@x.com");
        assert_eq!(row.op_class, "label_modify");
        assert_eq!(row.action, "trash");
        assert_eq!(row.thread_id, "t2");
        assert!(row.add_labels.is_empty());
        assert_eq!(row.rem_labels, vec!["INBOX".to_string()]);
    }

    #[test]
    fn pending_ops_due_respects_max_attempts() {
        let conn = test_conn();
        upsert_account(&conn, "a@x.com").unwrap();

        pending_ops_enqueue(&conn, "op-retry", "a@x.com", "star", "t4", &[], &[]).unwrap();
        // Bump attempts to 3 — at or above the cap it should be excluded.
        for _ in 0..3 {
            pending_ops_bump_attempt(&conn, "op-retry").unwrap();
        }

        let rows = pending_ops_due(&conn, "a@x.com", 3, 10).unwrap();
        assert!(rows.is_empty(), "row at max_attempts must not be returned");

        // Still returned when cap is higher.
        let rows = pending_ops_due(&conn, "a@x.com", 4, 10).unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn pending_ops_bump_attempt_increments() {
        let conn = test_conn();
        upsert_account(&conn, "a@x.com").unwrap();

        pending_ops_enqueue(&conn, "op-bump", "a@x.com", "read", "t5", &[], &[]).unwrap();
        pending_ops_bump_attempt(&conn, "op-bump").unwrap();
        pending_ops_bump_attempt(&conn, "op-bump").unwrap();

        let attempts: i64 = conn
            .query_row(
                "SELECT attempts FROM pending_ops WHERE request_id = 'op-bump'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(attempts, 2);
    }

    #[test]
    fn pending_ops_remove_deletes_row() {
        let conn = test_conn();
        upsert_account(&conn, "a@x.com").unwrap();

        pending_ops_enqueue(&conn, "op-rm", "a@x.com", "unstar", "t6", &[], &[]).unwrap();
        pending_ops_remove(&conn, "op-rm").unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM pending_ops WHERE request_id = 'op-rm'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "row must be deleted after remove");
    }
}
