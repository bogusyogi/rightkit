use super::*;

pub fn try_acquire_sync_lease(
    conn: &Connection,
    account_id: &str,
    owner: &str,
    ttl_ms: i64,
) -> Result<bool, String> {
    let now = now_ms();
    let n = conn
        .execute(
            "UPDATE sync_state SET lease_owner = ?2, lease_expires_at = ?3
             WHERE account_id = ?1 AND (lease_expires_at IS NULL OR lease_expires_at < ?4)",
            params![account_id, owner, now + ttl_ms, now],
        )
        .map_err(|e| format!("acquire sync lease: {e}"))?;
    Ok(n == 1)
}

/// Release the per-account sync lease.
pub fn release_sync_lease(conn: &Connection, account_id: &str) -> Result<(), String> {
    conn.execute(
        "UPDATE sync_state SET lease_owner = NULL, lease_expires_at = NULL WHERE account_id = ?1",
        [account_id],
    )
    .map(|_| ())
    .map_err(|e| format!("release sync lease: {e}"))
}

/// Upsert the account's label catalog (system + custom). Custom labels are
/// stored even though their folder UI is deferred, so the data isn't thrown
/// away — a synced message's custom labels also land in thread_labels.
pub fn apply_labels(
    conn: &Connection,
    account_id: &str,
    labels: &[LabelInfo],
) -> Result<(), String> {
    for l in labels {
        conn.execute(
            "INSERT INTO labels(account_id, id, name, type, color) VALUES(?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(account_id, id) DO UPDATE SET
                 name = excluded.name, type = excluded.type, color = excluded.color",
            params![account_id, l.id, l.name, l.label_type, l.color],
        )
        .map_err(|e| format!("upsert label: {e}"))?;
    }
    Ok(())
}
