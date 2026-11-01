use super::*;
mod sync_apply;
pub use sync_apply::*;
mod account_removal;
pub use account_removal::*;
/// The `settings(key, value)` table, accessed through the caller's borrowed
/// connection (or transaction, which derefs to it) so calls join its transaction.
fn settings() -> rightkit_app_settings::SqlSettings {
    rightkit_app_settings::SqlSettings::new(rightkit_app_settings::Schema::new(
        crate::host::host().app_slug(),
        1,
    ))
}
pub fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<(), String> {
    settings()
        .set(conn, key, value)
        .map_err(|e| format!("set setting {key}: {e}"))
}
pub fn get_setting(conn: &Connection, key: &str) -> Result<Option<String>, String> {
    settings()
        .get(conn, key)
        .map_err(|e| format!("get setting {key}: {e}"))
}
/// Delete one setting; absent keys are not an error.
pub fn remove_setting(conn: &Connection, key: &str) -> Result<(), String> {
    settings()
        .remove(conn, key)
        .map(|_| ())
        .map_err(|e| format!("remove setting {key}: {e}"))
}
/// Color palette cycled per account so the unified inbox can color-code rows.
const ACCOUNT_COLORS: &[&str] = &[
    "#C46A3D", "#6E8B6B", "#B07A84", "#7E8AA8", "#B59A52", "#8A6BA8",
];
pub fn upsert_account(conn: &Connection, email: &str) -> Result<(), String> {
    let display = email.split('@').next().unwrap_or(email).to_string();
    // Deterministic color: FNV-1a byte-fold so re-adding the same account always
    // picks the same color, regardless of how many other accounts currently exist.
    let color_idx = email
        .bytes()
        .fold(2166136261u32, |h, b| h.wrapping_mul(16777619) ^ b as u32)
        as usize
        % ACCOUNT_COLORS.len();
    let color = ACCOUNT_COLORS[color_idx];
    conn.execute(
        "INSERT INTO accounts(id, display_name, color, created_at)
         VALUES(?1, ?2, ?3, ?4)
         ON CONFLICT(id) DO UPDATE SET display_name = excluded.display_name",
        rightkit_db::rusqlite::params![email, display, color, now_ms()],
    )
    .map_err(|e| format!("upsert account: {e}"))?;
    conn.execute(
        "INSERT INTO sync_state(account_id) VALUES(?1)
         ON CONFLICT(account_id) DO NOTHING",
        [email],
    )
    .map_err(|e| format!("init sync_state: {e}"))?;
    Ok(())
}
// `remove_account_tx` (L7, transactional) lives in `accounts_labels/account_removal.rs`.
pub fn audit(conn: &Connection, kind: &str, account_id: Option<&str>, detail: &str) {
    let _ = conn.execute(
        "INSERT INTO audit_log(at, kind, account_id, detail) VALUES(?1, ?2, ?3, ?4)",
        rightkit_db::rusqlite::params![now_ms(), kind, account_id, detail],
    );
}
#[derive(serde::Serialize)]
pub struct AccountRow {
    pub id: String,
    pub display_name: String,
    pub color: String,
    pub last_error: Option<String>,
    /// Epoch-ms of the last sync attempt (success or failure), for the Settings
    /// sync-health card. None until the first sync stamps it.
    pub last_sync_at: Option<i64>,
}
pub fn list_accounts(conn: &Connection) -> Result<Vec<AccountRow>, String> {
    crate::db::query_map_collect(
        conn,
        "SELECT id, display_name, color, last_error, last_sync_at \
         FROM accounts ORDER BY created_at",
        [],
        "accounts",
        |r| {
            Ok(AccountRow {
                id: r.get(0)?,
                display_name: r.get(1)?,
                color: r.get(2)?,
                last_error: r.get(3)?,
                last_sync_at: r.get(4)?,
            })
        },
    )
}
/// Update an account's display name and accent color (Settings → account card).
/// Local-only metadata; does not touch Gmail. Empty `display_name` is rejected so
/// the row never loses its label.
pub fn set_account_display(
    conn: &Connection,
    account_id: &str,
    display_name: &str,
    color: &str,
) -> Result<(), String> {
    let name = display_name.trim();
    if name.is_empty() {
        return Err("display name cannot be empty".into());
    }
    conn.execute(
        "UPDATE accounts SET display_name = ?2, color = ?3 WHERE id = ?1",
        params![account_id, name, color],
    )
    .map(|_| ())
    .map_err(|e| format!("set account display: {e}"))
}
pub fn thread_message_ids(
    conn: &Connection,
    account_id: &str,
    thread_id: &str,
) -> Result<Vec<String>, String> {
    crate::db::query_map_collect(
        conn,
        "SELECT id FROM messages WHERE account_id = ?1 AND thread_id = ?2",
        params![account_id, thread_id],
        "msg ids",
        |r| r.get::<_, String>(0),
    )
}
/// Mark every message in a thread read (local state).
pub fn set_thread_read(conn: &Connection, account_id: &str, thread_id: &str) -> Result<(), String> {
    conn.execute(
        "UPDATE messages SET unread = 0 WHERE account_id = ?1 AND thread_id = ?2",
        params![account_id, thread_id],
    )
    .map_err(|e| format!("mark messages read: {e}"))?;
    conn.execute(
        "UPDATE threads SET unread = 0 WHERE account_id = ?1 AND id = ?2",
        params![account_id, thread_id],
    )
    .map_err(|e| format!("mark thread read: {e}"))?;
    Ok(())
}
/// Mark every message in a thread unread (local state).
pub fn set_thread_unread(
    conn: &Connection,
    account_id: &str,
    thread_id: &str,
) -> Result<(), String> {
    conn.execute(
        "UPDATE messages SET unread = 1 WHERE account_id = ?1 AND thread_id = ?2",
        params![account_id, thread_id],
    )
    .map_err(|e| format!("mark messages unread: {e}"))?;
    conn.execute(
        "UPDATE threads SET unread = 1 WHERE account_id = ?1 AND id = ?2",
        params![account_id, thread_id],
    )
    .map_err(|e| format!("mark thread unread: {e}"))?;
    Ok(())
}
/// Optimistically drop a label from a thread locally (e.g. archive removes
/// INBOX so the thread leaves the Inbox folder immediately). The next sync
/// reconciles the authoritative server state.
pub fn local_drop_label(
    conn: &Connection,
    account_id: &str,
    thread_id: &str,
    label: &str,
) -> Result<(), String> {
    conn.execute(
        "DELETE FROM thread_labels WHERE account_id = ?1 AND thread_id = ?2 AND label = ?3",
        params![account_id, thread_id, label],
    )
    .map_err(|e| format!("drop label: {e}"))?;
    // Also strip it from each message's label_ids JSON — `rebuild_rollups`
    // rebuilds thread_labels from messages.label_ids on every sync, so leaving
    // the message labels stale would resurrect the dropped label on the next
    // sync (the "deleted mail comes back" bug).
    conn.execute(
        "UPDATE messages
            SET label_ids = (
                SELECT json_group_array(value)
                FROM json_each(messages.label_ids)
                WHERE value <> ?3
            )
          WHERE account_id = ?1 AND thread_id = ?2 AND messages.label_ids IS NOT NULL",
        params![account_id, thread_id, label],
    )
    .map(|_| ())
    .map_err(|e| format!("drop label from messages: {e}"))
}
/// Optimistically add a label to a thread locally (e.g. trash adds TRASH).
pub fn local_add_label(
    conn: &Connection,
    account_id: &str,
    thread_id: &str,
    label: &str,
) -> Result<(), String> {
    conn.execute(
        "INSERT OR IGNORE INTO thread_labels(account_id, thread_id, label) VALUES(?1, ?2, ?3)",
        params![account_id, thread_id, label],
    )
    .map_err(|e| format!("add label: {e}"))?;
    // Mirror into each message's label_ids JSON (see local_drop_label) so a
    // later rebuild_rollups keeps the optimistic label instead of dropping it.
    conn.execute(
        "UPDATE messages
            SET label_ids = (
                SELECT json_group_array(value) FROM (
                    SELECT value FROM json_each(COALESCE(messages.label_ids, '[]'))
                    UNION
                    SELECT ?3
                )
            )
          WHERE account_id = ?1 AND thread_id = ?2
            AND NOT EXISTS(
                SELECT 1 FROM json_each(COALESCE(messages.label_ids, '[]')) WHERE value = ?3
            )",
        params![account_id, thread_id, label],
    )
    .map(|_| ())
    .map_err(|e| format!("add label to messages: {e}"))
}
/// Set a thread's starred flag (optimistic mirror of the STARRED label so the
/// list reflects a star toggle before the next sync).
pub fn set_thread_starred(
    conn: &Connection,
    account_id: &str,
    thread_id: &str,
    starred: bool,
) -> Result<(), String> {
    conn.execute(
        "UPDATE threads SET starred = ?3 WHERE account_id = ?1 AND id = ?2",
        params![account_id, thread_id, starred as i64],
    )
    .map(|_| ())
    .map_err(|e| format!("set thread starred: {e}"))
}
/// Count threads carrying a label (sidebar folder badges, e.g. DRAFT). When
/// `account` is Some, scope to that account so the badge matches the
/// account-scoped folder view; None counts across all accounts.
pub fn count_threads_with_label(
    conn: &Connection,
    account: Option<&str>,
    label: &str,
) -> Result<i64, String> {
    conn.query_row(
        "SELECT count(*) FROM thread_labels
         WHERE label = ?1 AND (?2 IS NULL OR account_id = ?2)",
        params![label, account],
        |r| r.get(0),
    )
    .map_err(|e| format!("count label: {e}"))
}
/// The set of labels carried by a thread (union across its messages), for the
/// reader's label chips. Caller filters out non-display labels (UNREAD, etc.).
pub fn thread_label_list(
    conn: &Connection,
    account_id: &str,
    thread_id: &str,
) -> Result<Vec<String>, String> {
    crate::db::query_map_collect(
        conn,
        "SELECT label FROM thread_labels
         WHERE account_id = ?1 AND thread_id = ?2 ORDER BY label",
        params![account_id, thread_id],
        "thread labels",
        |r| r.get::<_, String>(0),
    )
}
#[derive(serde::Serialize)]
pub struct LabelRow {
    pub id: String,
    pub name: String,
    pub label_type: String, // "system" | "user"
    pub total: i64,
    pub unread: i64,
    /// Background color hex synced from Gmail, if the user set one server-side.
    pub color: Option<String>,
}
/// The account's full label catalog with per-label thread + unread counts, for a
/// data-driven sidebar. This is what makes the user's real labels (custom +
/// system) render instead of a hardcoded folder list. Labels with zero threads
/// are still returned (the UI decides what to show).
pub fn list_label_catalog(conn: &Connection, account_id: &str) -> Result<Vec<LabelRow>, String> {
    crate::db::query_map_collect(
        conn,
        "SELECT l.id, l.name, l.type,
           (SELECT COUNT(*) FROM thread_labels tl
              WHERE tl.account_id = l.account_id AND tl.label = l.id) AS total,
           (SELECT COUNT(*) FROM thread_labels tl
              JOIN threads t ON t.account_id = tl.account_id AND t.id = tl.thread_id
              WHERE tl.account_id = l.account_id AND tl.label = l.id AND t.unread = 1) AS unread,
           l.color
         FROM labels l
         WHERE l.account_id = ?1
         ORDER BY l.name",
        [account_id],
        "label catalog",
        |r| {
            Ok(LabelRow {
                id: r.get(0)?,
                name: r.get(1)?,
                label_type: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                total: r.get(3)?,
                unread: r.get(4)?,
                color: r.get::<_, Option<String>>(5)?,
            })
        },
    )
}
/// Record (or clear) the last sync error for an account, stamping last_sync_at.
pub fn set_account_error(
    conn: &Connection,
    account_id: &str,
    err: Option<&str>,
) -> Result<(), String> {
    conn.execute(
        "UPDATE accounts SET last_error = ?2, last_sync_at = ?3 WHERE id = ?1",
        params![account_id, err, now_ms()],
    )
    .map(|_| ())
    .map_err(|e| format!("set account error: {e}"))
}
