mod listing;
use super::*;
pub use listing::*;

#[derive(serde::Serialize, Clone)]
pub struct ThreadRow {
    pub account_id: String,
    pub account_color: String,
    pub account_name: String,
    pub thread_id: String,
    pub from_name: String,
    pub subject: String,
    pub snippet: String,
    pub unread: bool,
    pub starred: bool,
    pub last_message_at: i64,
    pub labels: Vec<String>, // custom (non-system) label ids, for inline row chips
    pub has_attachments: bool,
    pub attachment_names: Vec<String>,
    pub important: bool,
    pub message_count: i64, // # messages in the thread (row "(N)" badge)
    pub participants: Vec<String>, // distinct sender display names in the thread
    pub matching_message_id: Option<String>,
    pub matching_message_excerpt: Option<String>,
    pub match_count: i64,
    /// L11: FTS5 `snippet()` output for the best-matching message, with match
    /// spans wrapped in `\u{1}`/`\u{2}` sentinel markers (never appear in
    /// real mail text) instead of HTML tags — backend-only field so the UI
    /// lane (C6) can choose its own highlight markup without re-parsing HTML.
    /// `None` when there is no FTS-anchored match (e.g. a pure operator
    /// query like `from:alice`).
    pub matching_message_snippet: Option<String>,
}

/// Sentinel "label" for the All Mail view (Gmail's archive): every thread except
/// those in Trash or Spam. Not a real Gmail label — handled specially below and
/// mirrored by the frontend's FOLDER_LABEL map.
pub const ALL_MAIL: &str = "ALL_MAIL";

/// Per-thread message aggregation. Resolves `from_name`, `has_attachments`,
/// `attachment_names` (up to 3 most recent), and the distinct participant
/// list in a single GROUP BY pass over `messages ⋈ attachments`. Replaces
/// four correlated subqueries the audit flagged as the cause of multi-second
/// `load_threads` timings against the pinned dogfood DB.
///
/// The attachment/participants `group_concat` use `char(31)` (unit separator)
/// to match the prior split-on-separator parser in `row_to_thread`, so the
/// visible chip list looks identical.
pub(super) const MSG_AGG_CTE: &str = "msg_agg AS (
    SELECT
        b.account_id,
        b.id AS thread_id,
        (SELECT from_name FROM messages
           WHERE account_id = b.account_id AND thread_id = b.id
           ORDER BY internal_date DESC LIMIT 1) AS from_name,
        MAX(COALESCE(m.has_attachments, 0)) AS has_attachments,
        (SELECT group_concat(filename, char(31)) FROM (
            SELECT att.filename AS filename
              FROM messages mm
              JOIN attachments att
                ON att.account_id = mm.account_id
               AND att.message_id  = mm.id
             WHERE mm.account_id = b.account_id
               AND mm.thread_id  = b.id
               AND att.filename IS NOT NULL
               AND att.filename <> ''
             ORDER BY mm.internal_date DESC, att.filename ASC
             LIMIT 3
        )) AS attachment_names,
        (SELECT group_concat(fn, char(31)) FROM (
            SELECT DISTINCT from_name AS fn
              FROM messages
             WHERE account_id = b.account_id
               AND thread_id  = b.id
               AND from_name IS NOT NULL
               AND from_name <> ''
        )) AS participants
    FROM base b
    LEFT JOIN messages m
      ON m.account_id = b.account_id
     AND m.thread_id = b.id
    GROUP BY b.account_id, b.id
  )";

/// Per-thread custom-label aggregation. Replaces the prior correlated
/// `group_concat(tl.label)` subquery with a single grouped projection over
/// `thread_labels`, filtering system labels in the GROUP BY step.
pub(super) const LBL_AGG_CTE: &str = "lbl_agg AS (
    SELECT b.account_id, b.id AS thread_id,
           group_concat(tl.label) AS custom_labels
      FROM base b
      LEFT JOIN thread_labels tl
        ON tl.account_id = b.account_id
       AND tl.thread_id = b.id
       AND tl.label NOT IN ('INBOX','SENT','DRAFT','SPAM','TRASH','STARRED','UNREAD',
            'IMPORTANT','CHAT','CATEGORY_PERSONAL','CATEGORY_SOCIAL','CATEGORY_PROMOTIONS',
            'CATEGORY_UPDATES','CATEGORY_FORUMS')
     GROUP BY b.account_id, b.id
  )";

/// The thread projection shared by list_threads / search_threads. Built from
/// the `msg_agg` + `lbl_agg` CTEs plus a cheap EXISTS for IMPORTANT. The two
/// CTEs are the only thing that changes per query; the outer SELECT just does
/// a pair of LEFT JOINs against precomputed strings.
///
/// IMPORTANT stays an EXISTS because the existing `thread_labels` index makes
/// it a single index probe and a JOIN would risk losing the LIMIT 200 plan's
/// rowid ordering if the optimizer chose a different join order.
pub(super) const THREAD_COLS: &str = "t.account_id, a.color, a.display_name, t.id,
   COALESCE(ma.from_name, ''),
   t.subject, t.snippet, t.unread, t.starred, t.last_message_at,
   COALESCE(la.custom_labels, ''),
   COALESCE(ma.has_attachments, 0),
   COALESCE(ma.attachment_names, ''),
   EXISTS(SELECT 1 FROM thread_labels tl3
      WHERE tl3.account_id = t.account_id AND tl3.thread_id = t.id AND tl3.label = 'IMPORTANT'),
   t.message_count,
   COALESCE(ma.participants, '')";

/// The join clause the outer SELECT adds when the THREAD_COLS projection is
/// used, to attach the `msg_agg` and `lbl_agg` precomputed columns.
pub(super) const THREAD_JOINS: &str = "LEFT JOIN msg_agg ma
     ON ma.account_id = t.account_id AND ma.thread_id = t.id
   LEFT JOIN lbl_agg la
     ON la.account_id = t.account_id AND la.thread_id = t.id";

/// Map a row of THREAD_COLS into a ThreadRow.
pub(super) fn row_to_thread(
    r: &rightkit_db::rusqlite::Row,
) -> rightkit_db::rusqlite::Result<ThreadRow> {
    Ok(ThreadRow {
        account_id: r.get(0)?,
        account_color: r.get(1)?,
        account_name: r.get(2)?,
        thread_id: r.get(3)?,
        from_name: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
        subject: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
        snippet: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
        unread: r.get::<_, i64>(7)? != 0,
        starred: r.get::<_, i64>(8)? != 0,
        last_message_at: r.get(9)?,
        labels: r
            .get::<_, Option<String>>(10)?
            .map(|s| {
                s.split(',')
                    .filter(|x| !x.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default(),
        has_attachments: r.get::<_, Option<i64>>(11)?.unwrap_or(0) != 0,
        attachment_names: r
            .get::<_, Option<String>>(12)?
            .map(|s| {
                s.split('\u{1f}')
                    .filter(|x| !x.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default(),
        important: r.get::<_, i64>(13)? != 0,
        message_count: r.get::<_, Option<i64>>(14)?.unwrap_or(0),
        participants: r
            .get::<_, Option<String>>(15)?
            .map(|s| {
                s.split('\u{1f}')
                    .filter(|x| !x.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default(),
        matching_message_id: None,
        matching_message_excerpt: None,
        match_count: 0,
        matching_message_snippet: None,
    })
}

/// Of a candidate set of message ids, which already exist in the mirror for
/// this account. Lets the backfill skip re-fetching messages it already has.
pub fn existing_message_ids(
    conn: &Connection,
    account_id: &str,
    ids: &[String],
) -> Result<std::collections::HashSet<String>, String> {
    let mut out = std::collections::HashSet::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let placeholders = vec!["?"; ids.len()].join(",");
    let sql = format!("SELECT id FROM messages WHERE account_id = ?1 AND id IN ({placeholders})");
    let mut params: Vec<&dyn rightkit_db::rusqlite::ToSql> = Vec::with_capacity(ids.len() + 1);
    params.push(&account_id);
    for id in ids {
        params.push(id);
    }
    let ids =
        crate::db::query_map_collect(conn, &sql, params.as_slice(), "existing_message_ids", |r| {
            r.get::<_, String>(0)
        })?;
    out.extend(ids);
    Ok(out)
}

/// Of a candidate set of message ids, which (that exist for this account) have
/// NO stored body yet — the download-all archive fetches full bodies for these.
/// Includes ids not yet mirrored at all (they need fetching too).
pub fn messages_missing_body(
    conn: &Connection,
    account_id: &str,
    ids: &[String],
) -> Result<Vec<String>, String> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; ids.len()].join(",");
    let sql = format!(
        "SELECT id FROM messages
         WHERE account_id = ?1 AND id IN ({placeholders})
           AND id IN (SELECT message_id FROM message_bodies WHERE account_id = ?1)"
    );
    let mut params: Vec<&dyn rightkit_db::rusqlite::ToSql> = Vec::with_capacity(ids.len() + 1);
    params.push(&account_id);
    for id in ids {
        params.push(id);
    }
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("prepare missing_body: {e}"))?;
    let have_body: std::collections::HashSet<String> = stmt
        .query_map(params.as_slice(), |r| r.get::<_, String>(0))
        .map_err(|e| format!("query missing_body: {e}"))?
        .filter_map(Result::ok)
        .collect();
    Ok(ids
        .iter()
        .filter(|id| !have_body.contains(*id))
        .cloned()
        .collect())
}

/// The Gmail category labels behind the inbox tabs. Primary = INBOX minus these.
#[derive(serde::Serialize, Clone)]
pub struct MessageView {
    pub message_id: String,
    pub from_name: String,
    pub from_addr: String,
    pub to_addrs: String,
    pub cc_addrs: String,
    pub reply_to: Option<String>,
    pub list_unsubscribe: Option<String>,
    pub internal_date: i64,
    pub subject: String,
    pub body: String,
    pub is_html: bool,
    pub unread: bool,
    pub has_attachments: bool,
    /// Parsed SPF/DKIM/DMARC verdicts from Authentication-Results, if persisted.
    pub auth: Option<rightkit_mail::trust::AuthStatus>,
}

pub fn get_thread_messages(
    conn: &Connection,
    account_id: &str,
    thread_id: &str,
) -> Result<Vec<MessageView>, String> {
    crate::db::query_map_collect(
        conn,
        "SELECT m.id, m.from_name, m.from_addr, m.internal_date, m.subject, m.unread,
           m.to_addrs, m.cc_addrs, m.reply_to, m.list_unsubscribe,
           COALESCE(
             (SELECT body FROM message_bodies
                WHERE account_id=m.account_id AND message_id=m.id AND mime_type='text/html'),
             (SELECT body FROM message_bodies
                WHERE account_id=m.account_id AND message_id=m.id AND mime_type='text/plain'),
             m.snippet) AS body,
           CASE WHEN EXISTS(
             SELECT 1 FROM message_bodies
               WHERE account_id=m.account_id AND message_id=m.id AND mime_type='text/html'
           ) THEN 1 ELSE 0 END AS is_html,
           m.has_attachments, m.auth_results
         FROM messages m
         WHERE m.account_id=?1 AND m.thread_id=?2
         ORDER BY m.internal_date ASC",
        params![account_id, thread_id],
        "thread",
        |r| {
            Ok(MessageView {
                message_id: r.get::<_, String>(0)?,
                from_name: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                from_addr: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                internal_date: r.get(3)?,
                subject: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                unread: r.get::<_, i64>(5)? != 0,
                to_addrs: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                cc_addrs: r.get::<_, Option<String>>(7)?.unwrap_or_default(),
                reply_to: r.get::<_, Option<String>>(8)?,
                list_unsubscribe: r.get::<_, Option<String>>(9)?,
                body: r.get::<_, Option<String>>(10)?.unwrap_or_default(),
                is_html: r.get::<_, i64>(11)? != 0,
                has_attachments: r.get::<_, i64>(12)? != 0,
                auth: r
                    .get::<_, Option<String>>(13)?
                    .as_deref()
                    .map(rightkit_mail::trust::parse_auth_results),
            })
        },
    )
}
