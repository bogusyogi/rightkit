use super::*;

pub const MAX_SEND_ATTEMPTS: i64 = 5;

pub fn current_sync_lease_owner(conn: &Connection) -> Result<Option<String>, String> {
    match conn.query_row(
        "SELECT lease_owner FROM sync_state
         WHERE lease_owner IS NOT NULL
         ORDER BY COALESCE(lease_expires_at, 0) DESC LIMIT 1",
        [],
        |r| r.get::<_, Option<String>>(0),
    ) {
        Ok(v) => Ok(v),
        Err(rightkit_db::rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(format!("current sync lease owner: {e}")),
    }
}

pub fn current_sync_lease_age_ms(conn: &Connection) -> Result<Option<i64>, String> {
    match conn.query_row(
        "SELECT CAST((?1 - lease_expires_at) AS INTEGER) FROM sync_state
         WHERE lease_owner IS NOT NULL
         ORDER BY COALESCE(lease_expires_at, 0) DESC LIMIT 1",
        [now_ms()],
        |r| r.get::<_, Option<i64>>(0),
    ) {
        Ok(v) => Ok(v),
        Err(rightkit_db::rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(format!("current sync lease age: {e}")),
    }
}

pub fn last_account_error_summary(conn: &Connection) -> Result<Option<String>, String> {
    match conn.query_row(
        "SELECT last_error FROM accounts
         WHERE last_error IS NOT NULL AND last_error != ''
         ORDER BY COALESCE(last_sync_at, 0) DESC LIMIT 1",
        [],
        |r| r.get::<_, Option<String>>(0),
    ) {
        Ok(v) => Ok(v),
        Err(rightkit_db::rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(format!("last account error: {e}")),
    }
}

pub fn set_history_id(conn: &Connection, account_id: &str, history_id: &str) -> Result<(), String> {
    conn.execute(
        // Never move the cursor backwards (historyIds are numeric strings).
        "UPDATE sync_state SET last_history_id = ?2 WHERE account_id = ?1
           AND (last_history_id IS NULL
                OR CAST(?2 AS INTEGER) >= CAST(last_history_id AS INTEGER))",
        params![account_id, history_id],
    )
    .map(|_| ())
    .map_err(|e| format!("set history_id: {e}"))
}
