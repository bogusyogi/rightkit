use super::*;

const OTHER_CATEGORIES: &str =
    "'CATEGORY_PROMOTIONS', 'CATEGORY_SOCIAL', 'CATEGORY_UPDATES', 'CATEGORY_FORUMS'";

/// Threads in a scope (unwrapped of category). `account` None = all accounts.
pub fn list_threads(
    conn: &Connection,
    account: Option<&str>,
    label: &str,
) -> Result<Vec<ThreadRow>, String> {
    list_threads_cat(conn, account, label, None)
}

/// Threads in a scope, optionally narrowed to an inbox category tab.
/// `label` = ALL_MAIL → every thread not in Trash/Spam (the archive view);
/// otherwise threads carrying that Gmail label. When `label` == INBOX and
/// `category` is set, also filter by the tab: "PRIMARY" = INBOX minus the other
/// categories; "CATEGORY_*" = INBOX threads in that category.
///
/// The query declares two pre-aggregated CTEs (`msg_agg`, `lbl_agg`) once and
/// joins them into the limited 200-row base. This replaces the four per-row
/// correlated subqueries the audit flagged as the cause of multi-second
/// `load_threads` timings against the pinned dogfood DB.
pub fn list_threads_cat(
    conn: &Connection,
    account: Option<&str>,
    label: &str,
    category: Option<&str>,
) -> Result<Vec<ThreadRow>, String> {
    let now = now_ms();

    // Optional inbox-category narrowing. cat_param carries the bound value when
    // we filter by a specific category; PRIMARY uses a literal NOT EXISTS.
    let (extra, cat_param): (String, Option<String>) = match (label, category) {
        ("INBOX", Some("PRIMARY")) => (
            format!(
                " AND NOT EXISTS(SELECT 1 FROM thread_labels tl2
                   WHERE tl2.account_id = t.account_id AND tl2.thread_id = t.id
                     AND tl2.label IN ({OTHER_CATEGORIES}))"
            ),
            None,
        ),
        ("INBOX", Some(cat)) => (
            " AND EXISTS(SELECT 1 FROM thread_labels tl2
               WHERE tl2.account_id = t.account_id AND tl2.thread_id = t.id
                 AND tl2.label = ?3)"
                .to_string(),
            Some(cat.to_string()),
        ),
        _ => (String::new(), None),
    };

    // The MailRight-managed Snoozed view: threads with an unexpired snooze,
    // soonest-due first. (Gmail has no Snooze API — this is local-only.)
    if label == "SNOOZED" {
        let sql = format!(
            "WITH base AS (
               SELECT t.*, s.until_ms AS snooze_until_ms
               FROM threads t
               JOIN snoozes s ON s.account_id = t.account_id AND s.thread_id = t.id
               WHERE (?1 IS NULL OR t.account_id = ?1)
                 AND s.until_ms > {now}
               ORDER BY s.until_ms ASC
               LIMIT 200
             ), {MSG_AGG_CTE}, {LBL_AGG_CTE}
             SELECT {THREAD_COLS}
             FROM base t
             JOIN accounts a ON a.id = t.account_id
             {THREAD_JOINS}
             ORDER BY t.snooze_until_ms ASC"
        );
        return crate::db::query_map_collect(
            conn,
            &sql,
            params![account],
            "snoozed",
            row_to_thread,
        );
    }

    // Threads with an unexpired snooze are hidden from every other view.
    let snooze_excl = format!(
        " AND NOT EXISTS(SELECT 1 FROM snoozes s WHERE s.account_id = t.account_id
           AND s.thread_id = t.id AND s.until_ms > {now})"
    );

    if label == ALL_MAIL {
        let sql = format!(
            "WITH base AS (
               SELECT t.*
               FROM threads t
               WHERE (?1 IS NULL OR t.account_id = ?1)
                 AND NOT EXISTS(
                   SELECT 1 FROM thread_labels tl
                   WHERE tl.account_id = t.account_id AND tl.thread_id = t.id
                     AND tl.label IN ('TRASH', 'SPAM')
                 ){snooze_excl}
               ORDER BY t.last_message_at DESC
               LIMIT 200
             ), {MSG_AGG_CTE}, {LBL_AGG_CTE}
             SELECT {THREAD_COLS}
             FROM base t
             JOIN accounts a ON a.id = t.account_id
             {THREAD_JOINS}
             ORDER BY t.last_message_at DESC"
        );
        return crate::db::query_map_collect(
            conn,
            &sql,
            params![account],
            "all-mail",
            row_to_thread,
        );
    }

    // Base thread set for an arbitrary label. INBOX matches most threads, so
    // driving from `threads` (date-ordered) reaches LIMIT 200 almost immediately.
    // But a RARE label (DRAFT, a custom Label_*) matches few threads, so that same
    // threads-driven scan walks the ENTIRE table chasing 200 rows it never finds —
    // the 9-11s "Not Responding" freezes logged for label=DRAFT (2 rows) and custom
    // labels (0 rows). For non-INBOX labels, drive from thread_labels instead
    // (idx_thread_labels_label on account_id,label) so we touch only that label's
    // own rows, then join to threads. INBOX keeps its fast existing plan.
    let base = if label == "INBOX" {
        format!(
            "base AS (
               SELECT t.*
               FROM threads t
               WHERE (?1 IS NULL OR t.account_id = ?1)
                 AND EXISTS(
                   SELECT 1 FROM thread_labels tl
                   WHERE tl.account_id = t.account_id AND tl.thread_id = t.id
                     AND tl.label = ?2
                 ){extra}{snooze_excl}
               ORDER BY t.last_message_at DESC
               LIMIT 200
             )"
        )
    } else {
        format!(
            "base AS (
               SELECT t.*
               FROM thread_labels tl
               JOIN threads t
                 ON t.account_id = tl.account_id AND t.id = tl.thread_id
               WHERE tl.label = ?2 AND (?1 IS NULL OR tl.account_id = ?1){snooze_excl}
               ORDER BY t.last_message_at DESC
               LIMIT 200
             )"
        )
    };
    let sql = format!(
        "WITH {base}, {MSG_AGG_CTE}, {LBL_AGG_CTE}
         SELECT {THREAD_COLS}
         FROM base t
         JOIN accounts a ON a.id = t.account_id
         {THREAD_JOINS}
         ORDER BY t.last_message_at DESC"
    );
    if let Some(cat) = cat_param {
        crate::db::query_map_collect(
            conn,
            &sql,
            params![account, label, cat],
            "threads",
            row_to_thread,
        )
    } else {
        crate::db::query_map_collect(
            conn,
            &sql,
            params![account, label],
            "threads",
            row_to_thread,
        )
    }
}

#[derive(serde::Serialize)]
pub struct AccountCount {
    pub account_id: String,
    pub count: i64,
}

#[derive(serde::Serialize)]
pub struct UnreadCounts {
    pub total: i64,
    pub per_account: Vec<AccountCount>,
}

/// Unread INBOX thread counts, total + per account, for sidebar badges.
pub fn unread_counts(conn: &Connection) -> Result<UnreadCounts, String> {
    let per_account = crate::db::query_map_collect(
        conn,
        "SELECT t.account_id, COUNT(*)
         FROM threads t
         WHERE t.unread = 1
           AND EXISTS(
             SELECT 1 FROM thread_labels tl
             WHERE tl.account_id = t.account_id AND tl.thread_id = t.id
               AND tl.label = 'INBOX'
           )
         GROUP BY t.account_id",
        [],
        "unread",
        |r| {
            Ok(AccountCount {
                account_id: r.get(0)?,
                count: r.get(1)?,
            })
        },
    )?;
    let total = per_account.iter().map(|ac| ac.count).sum();
    Ok(UnreadCounts { total, per_account })
}
