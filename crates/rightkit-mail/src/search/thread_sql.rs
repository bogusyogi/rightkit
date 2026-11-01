//! SQL fragments shared by every thread-list query: the per-thread aggregation CTEs, the
//! thread projection and its joins. They assume the mail schema the predicates in
//! [`super::predicates`] assume (`threads`, `messages`, `attachments`, `thread_labels`,
//! `accounts`).

/// The mailbox scope that spans every non-trash, non-spam thread.
pub const ALL_MAIL: &str = "ALL_MAIL";

/// Gmail inbox categories other than Primary, as an SQL `IN` list body.
pub const SEARCH_OTHER_CATEGORIES: &str =
    "'CATEGORY_PROMOTIONS', 'CATEGORY_SOCIAL', 'CATEGORY_UPDATES', 'CATEGORY_FORUMS'";

/// Per-thread message aggregation. Resolves `from_name`, `has_attachments`,
/// `attachment_names` (up to 3 most recent), and the distinct participant
/// list in a single GROUP BY pass over `messages ⋈ attachments`. Replaces
/// four correlated subqueries the audit flagged as the cause of multi-second
/// `load_threads` timings against the pinned dogfood DB.
///
/// The attachment/participants `group_concat` use `char(31)` (unit separator)
/// to match the prior split-on-separator parser of the thread row, so the
/// visible chip list looks identical.
pub const MSG_AGG_CTE: &str = "msg_agg AS (
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
pub const LBL_AGG_CTE: &str = "lbl_agg AS (
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
pub const THREAD_COLS: &str = "t.account_id, a.color, a.display_name, t.id,
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
pub const THREAD_JOINS: &str = "LEFT JOIN msg_agg ma
     ON ma.account_id = t.account_id AND ma.thread_id = t.id
   LEFT JOIN lbl_agg la
     ON la.account_id = t.account_id AND la.thread_id = t.id";
