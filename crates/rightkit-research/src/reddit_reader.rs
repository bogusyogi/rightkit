//! The Reddit thread reader, schema v12 of the channel store (one migration list, in channels.rs).
//!
//! OFF by default. Only the exact value `on` of the `RIGHTKIT_REDDIT_READER` environment variable enables
//! it. While it is off, `reddit_reader.fetch` is refused with `reader_disabled` before any network call or
//! store write. The reader never logs in, never posts, never votes and sends no credential: its one request
//! is an anonymous GET of a public listing, to the loopback base URL the publish core validates. Reddit is
//! draft-only in the product; the owner posts by hand.
//!
//! Rules this module keeps:
//! - `reddit_reader.fetch` is daemon-only (the command table applies the publish core's role gate, as it
//!   does for `analytics.pull`). `status` and `list` run in any role and make no network call.
//! - Time comes from the input `now`. A fetch without it is `bad_input`; the system clock is never read.
//! - A thread is stored once per scope and thread id. A later fetch that sees the same id stores nothing
//!   and keeps the first row, so a stored row never changes.
//! - A 429 stops the fetch with `rate_limited` and the retry-after value. A 5xx, a transport failure or a
//!   listing with one bad thread stores nothing: every field is checked before the one transaction.
//! - Every stored permalink names the fixed host https://www.reddit.com, whatever base URL was used.
use std::path::Path;

use crate::reddit_protocol::{
    RedditFetchInput, RedditFetchOutput, RedditListInput, RedditListOutput, RedditReaderStatus,
    RedditThread,
};
use rightkit_db::rusqlite::{params, Connection, Row};
use rightkit_db::Migration;
use rightkit_http::{HttpError, Method, Request, RetryPolicy};
use rightkit_social_publish::protocol::Scope;
use serde_json::Value;

use rightkit_social_publish::calendar_time as time;
use rightkit_social_publish::http::{self, RetryAfter};
use rightkit_social_publish::util as publish;

use crate::host::{host, Fail};

/// The environment variable that turns the reader on.
pub const ENV: &str = "RIGHTKIT_REDDIT_READER";
/// The one value that enables the reader.
const ENABLED_VALUE: &str = "on";
/// The name the `platform_endpoints` setting and the mock's listing route use.
const PLATFORM: &str = "reddit";
/// The host every stored permalink names.
const PERMALINK_HOST: &str = "https://www.reddit.com";
/// Sent on the one anonymous request. It names the reader; it is not a credential.
const USER_AGENT: &str = "rightkit-reddit-reader/0.1 (draft-only, anonymous, read-only)";
/// Longest subreddit name Reddit allows.
const SUBREDDIT_MAX: usize = 21;
/// Longest thread id accepted. Reddit ids are short base-36 strings.
const THREAD_ID_MAX: usize = 20;
/// Largest epoch second a created time may name (9999-12-31T23:59:59Z).
const EPOCH_MAX: f64 = 253_402_300_799.0;

/// Methods that write. A dry run reports these and changes nothing.
pub const WRITE_METHODS: &[&str] = &["reddit_reader.fetch"];

/// Every method this module routes.
pub const METHODS: &[&str] = &[
    "reddit_reader.status",
    "reddit_reader.fetch",
    "reddit_reader.list",
];

const SCHEMA_V12: &str = "
CREATE TABLE reddit_threads (
    venture TEXT NOT NULL,
    channel TEXT NOT NULL,
    account TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    subreddit TEXT NOT NULL,
    title TEXT NOT NULL,
    author TEXT NOT NULL,
    permalink TEXT NOT NULL,
    created_at TEXT NOT NULL,
    score INTEGER NOT NULL,
    comment_count INTEGER NOT NULL CHECK (comment_count >= 0),
    fetched_at TEXT NOT NULL,
    PRIMARY KEY (venture, channel, account, thread_id),
    FOREIGN KEY (venture, channel, account) REFERENCES channels (venture, channel, account)
);
CREATE INDEX reddit_threads_by_created ON reddit_threads (venture, channel, account, created_at);
";

/// Schema v12, appended to the channel store's migration list in channels.rs.
pub const MIGRATION_V12: Migration = Migration::sql(12, "reddit-reader", SCHEMA_V12);

const INSERT_THREAD: &str = "
INSERT INTO reddit_threads (venture, channel, account, thread_id, subreddit, title, author, permalink,
    created_at, score, comment_count, fetched_at)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
ON CONFLICT (venture, channel, account, thread_id) DO NOTHING";

const LIST_SQL: &str = "
SELECT venture, channel, account, thread_id, subreddit, title, author, permalink, created_at, score,
    comment_count, fetched_at
FROM reddit_threads
WHERE venture = ?1 AND channel = ?2 AND account = ?3
ORDER BY created_at DESC, thread_id DESC";

fn bad_response(msg: impl Into<String>) -> Fail {
    ("bad_response", msg.into())
}

fn to_value<T: serde::Serialize>(value: &T) -> Result<Value, Fail> {
    serde_json::to_value(value).map_err(publish::internal)
}

fn open_store(root: &Path) -> Result<Connection, Fail> {
    host()?.open_store(root)
}

/// Whether the reader is on. Only the exact value `on` enables it; absence and every other value are off.
pub fn enabled() -> bool {
    std::env::var(ENV).is_ok_and(|value| value == ENABLED_VALUE)
}

/// `reddit_reader.status`. The reason names the state and never echoes what the variable holds.
fn status() -> RedditReaderStatus {
    match std::env::var(ENV) {
        Ok(value) if value == ENABLED_VALUE => RedditReaderStatus {
            enabled: true,
            reason: format!("enabled: {ENV} is on"),
        },
        Ok(_) => RedditReaderStatus {
            enabled: false,
            reason: format!("off: {ENV} is set to something other than on"),
        },
        Err(_) => RedditReaderStatus {
            enabled: false,
            reason: format!("off: {ENV} is not set"),
        },
    }
}

/// Route one method of this module. Unknown methods are `unknown_method`.
pub fn dispatch(root: &Path, method: &str, input: Value) -> Result<Value, Fail> {
    match method {
        "reddit_reader.status" => to_value(&status()),
        "reddit_reader.fetch" => fetch(root, input),
        "reddit_reader.list" => list(root, input),
        other => Err(("unknown_method", format!("unknown method: {other}"))),
    }
}

/// The bare subreddit name: 1 to 21 letters, digits or underscores. It becomes a URL path segment, so a
/// slash, a dot, a space or an `r/` prefix is refused.
fn subreddit_ok(name: &str) -> Result<(), Fail> {
    let ok = !name.is_empty()
        && name.len() <= SUBREDDIT_MAX
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(publish::bad(format!(
            "subreddit must be 1 to {SUBREDDIT_MAX} letters, digits or underscores, without the r/ prefix, got {name:?}"
        )))
    }
}

/// `reddit_reader.fetch`: one anonymous GET of a subreddit listing, then one transaction that stores the
/// threads not yet stored in scope. Checks run in this order, and the first failure writes nothing: reader
/// on, input, subreddit, `now`, channel in scope, base URL (loopback), the listing read, every thread parsed.
fn fetch(root: &Path, input: Value) -> Result<Value, Fail> {
    if !enabled() {
        return Err((
            "reader_disabled",
            format!(
                "the Reddit reader is off: set {ENV}=on to enable it (development builds only)"
            ),
        ));
    }
    let req: RedditFetchInput = serde_json::from_value(input)
        .map_err(|e| publish::bad(format!("reddit_reader.fetch input: {e}")))?;
    subreddit_ok(&req.subreddit)?;
    let raw_now = req.now.as_deref().ok_or_else(|| {
        publish::bad("now is required: the reader takes its clock from the input")
    })?;
    let fetched_at = time::format_instant(time::parse_instant(raw_now, "now")?);

    let mut conn = open_store(root)?;
    require_channel(&conn, &req.scope)?;
    let base = http::base_url(
        &host()?.endpoint_settings(),
        PLATFORM,
        req.base_url.as_deref(),
    )?;
    let url = format!("{base}/reddit/r/{}/new.json", req.subreddit);
    let listing = get_listing(&url)?;
    let threads = parse_listing(&listing, &req.scope, &req.subreddit, &fetched_at)?;
    let (stored, already_stored) = store(&mut conn, &req.scope, &threads)?;

    to_value(&RedditFetchOutput {
        scope: req.scope,
        subreddit: req.subreddit,
        fetched_at,
        seen: threads.len() as i64,
        stored,
        already_stored,
    })
}

/// One anonymous GET of the listing: no Authorization header, no cookie, no body. A 429 stops the fetch
/// with `rate_limited` and the retry-after value. Nothing is parsed or stored on any failure.
fn get_listing(url: &str) -> Result<Value, Fail> {
    let client = http::client();
    let request = Request::new(Method::Get, url)
        .header("Accept", "application/json")
        .header("User-Agent", USER_AGENT)
        .retry(RetryPolicy::none());
    match client.send_any(&request) {
        Ok(response) => {
            let status = response.status;
            match status {
                200..=299 => serde_json::from_slice::<Value>(&response.body)
                    .map_err(|e| bad_response(format!("the listing is not JSON: {e}"))),
                429 => {
                    let retry = response
                        .header("retry-after")
                        .map_or(RetryAfter::Unspecified, http::parse_retry_after);
                    Err((
                        "rate_limited",
                        format!(
                            "the platform answered 429; nothing was stored; retry_after={}",
                            retry_text(retry)
                        ),
                    ))
                }
                500..=599 => Err(("server_error", format!("HTTP {status}; nothing was stored"))),
                _ => Err((
                    "platform_rejected",
                    format!("HTTP {status}; nothing was stored"),
                )),
            }
        }
        Err(HttpError::Transport { message, .. }) => Err(("connection_failed", message)),
        Err(HttpError::Timeout { .. }) => Err((
            "timeout",
            "the listing request timed out; nothing was stored".to_string(),
        )),
        Err(HttpError::InvalidRequest(message)) => Err(("invalid_request", message)),
        Err(other) => Err(("connection_failed", other.to_string())),
    }
}

/// The retry-after value as the platform gave it, for the error message.
fn retry_text(retry: RetryAfter) -> String {
    match retry {
        RetryAfter::Seconds(secs) => format!("{secs}s"),
        RetryAfter::At(at) => format!("at {}", time::format_instant(at)),
        RetryAfter::Unspecified => "unspecified".to_string(),
    }
}

/// Every thread of the listing, each field checked. One bad child refuses the whole listing, so a partial
/// listing is never stored.
fn parse_listing(
    body: &Value,
    scope: &Scope,
    subreddit: &str,
    fetched_at: &str,
) -> Result<Vec<RedditThread>, Fail> {
    let children = body
        .get("data")
        .and_then(|data| data.get("children"))
        .and_then(Value::as_array)
        .ok_or_else(|| bad_response("the listing has no data.children array"))?;
    children
        .iter()
        .map(|child| thread_of(child, scope, subreddit, fetched_at))
        .collect()
}

/// One listing child as a thread. The permalink must be a `/r/` path with no whitespace or control
/// characters; it is stored under the fixed reddit.com host.
fn thread_of(
    child: &Value,
    scope: &Scope,
    fallback_subreddit: &str,
    fetched_at: &str,
) -> Result<RedditThread, Fail> {
    let data = child
        .get("data")
        .filter(|data| data.is_object())
        .ok_or_else(|| bad_response("a listing child has no data object"))?;
    let text = |key: &str| -> Result<String, Fail> {
        data.get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| bad_response(format!("thread field {key} is not a string")))
    };

    let thread_id = text("id")?;
    if thread_id.is_empty()
        || thread_id.len() > THREAD_ID_MAX
        || !thread_id.bytes().all(|b| b.is_ascii_alphanumeric())
    {
        return Err(bad_response(format!(
            "thread id {thread_id:?} is not a short alphanumeric id"
        )));
    }
    let path = text("permalink")?;
    if !path.starts_with("/r/") || path.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(bad_response(format!(
            "thread {thread_id} has a permalink that is not a reddit /r/ path"
        )));
    }
    let created = data
        .get("created_utc")
        .and_then(Value::as_f64)
        .filter(|secs| (0.0..=EPOCH_MAX).contains(secs))
        .ok_or_else(|| bad_response(format!("thread {thread_id} has no valid created_utc")))?;
    let score = data
        .get("score")
        .and_then(Value::as_i64)
        .ok_or_else(|| bad_response(format!("thread {thread_id} has no integer score")))?;
    let comment_count = data
        .get("num_comments")
        .and_then(Value::as_i64)
        .filter(|n| *n >= 0)
        .ok_or_else(|| bad_response(format!("thread {thread_id} has no valid num_comments")))?;

    Ok(RedditThread {
        scope: scope.clone(),
        thread_id,
        subreddit: data
            .get("subreddit")
            .and_then(Value::as_str)
            .unwrap_or(fallback_subreddit)
            .to_string(),
        title: text("title")?,
        author: text("author")?,
        permalink: format!("{PERMALINK_HOST}{path}"),
        created_at: time::format_instant(created.trunc() as i64),
        score,
        comment_count,
        fetched_at: fetched_at.to_string(),
    })
}

/// Stores the threads in one transaction. Returns how many rows were new and how many were already stored
/// under the same scope and thread id (those keep their first row).
fn store(
    conn: &mut Connection,
    scope: &Scope,
    threads: &[RedditThread],
) -> Result<(i64, i64), Fail> {
    let tx = rightkit_db::write_txn(conn).map_err(publish::db_err)?;
    let mut stored: i64 = 0;
    for thread in threads {
        let written = tx
            .execute(
                INSERT_THREAD,
                params![
                    scope.venture,
                    scope.channel,
                    scope.account,
                    thread.thread_id,
                    thread.subreddit,
                    thread.title,
                    thread.author,
                    thread.permalink,
                    thread.created_at,
                    thread.score,
                    thread.comment_count,
                    thread.fetched_at
                ],
            )
            .map_err(publish::db_err)?;
        stored += written as i64;
    }
    tx.commit().map_err(publish::db_err)?;
    Ok((stored, threads.len() as i64 - stored))
}

fn read_thread(r: &Row<'_>) -> rightkit_db::rusqlite::Result<RedditThread> {
    Ok(RedditThread {
        scope: Scope {
            venture: r.get(0)?,
            channel: r.get(1)?,
            account: r.get(2)?,
        },
        thread_id: r.get(3)?,
        subreddit: r.get(4)?,
        title: r.get(5)?,
        author: r.get(6)?,
        permalink: r.get(7)?,
        created_at: r.get(8)?,
        score: r.get(9)?,
        comment_count: r.get(10)?,
        fetched_at: r.get(11)?,
    })
}

/// `reddit_reader.list`: every stored thread in scope, newest first. Reads in any role and never calls out.
fn list(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: RedditListInput = serde_json::from_value(input)
        .map_err(|e| publish::bad(format!("reddit_reader.list input: {e}")))?;
    let conn = open_store(root)?;
    require_channel(&conn, &req.scope)?;
    let mut stmt = conn.prepare(LIST_SQL).map_err(publish::db_err)?;
    let rows = stmt
        .query_map(
            params![req.scope.venture, req.scope.channel, req.scope.account],
            read_thread,
        )
        .map_err(publish::db_err)?;
    let mut threads = Vec::new();
    for row in rows {
        threads.push(row.map_err(publish::db_err)?);
    }
    to_value(&RedditListOutput {
        scope: req.scope,
        threads,
    })
}

/// `foreign_scope` unless the scope exactly matches a stored channel.
fn require_channel(conn: &Connection, scope: &Scope) -> Result<(), Fail> {
    let known: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM channels WHERE venture = ?1 AND channel = ?2 AND account = ?3",
            params![scope.venture, scope.channel, scope.account],
            |r| r.get(0),
        )
        .map_err(publish::db_err)?;
    if known == 0 {
        return Err((
            "foreign_scope",
            format!(
                "no stored channel for venture={} channel={} account={}",
                scope.venture, scope.channel, scope.account
            ),
        ));
    }
    Ok(())
}
