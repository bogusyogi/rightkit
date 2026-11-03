//! Deletion requests: what the host app erases about one account on one platform, and the record of having done it.
//! The host keeps the schema (a `deletion_requests` table) and the list of tables an erase covers, and
//! implements [`DeletionStore`] over them.
//!
//! - `deletion.request` (source `email` or `endpoint`) records the request and erases the account's data in one
//!   transaction. The public HTTP route belongs to the daemon; it calls this command with the request it accepted.
//! - `deletion.meta_callback` is the handler for Meta's data deletion callback. The daemon verifies Meta's signed
//!   request and passes the platform user id; the engine matches stored `facebook`, `instagram` and `threads`
//!   accounts whose handle is that id, erases each, and returns the confirmation code and state the daemon
//!   turns into Meta's `{ url, confirmation_code }` answer.
//! - `deletion.status` reads a request by its id or confirmation code.
//!
//! Erased for the account (scope plus platform), by the host's [`DeletionStore::erase_rows`]: publish jobs and their lineage, quota rows, analytics observations
//! and imported metric values, optimise proposals about its jobs, link clicks and tracked links, digest applications
//! and slot reservations, content occurrences, platform variants, manual drafts, calendar entries, inbox events and the
//! account row, and the account's media files on disk (see below). Hook-test openings bound to its jobs are unbound, not removed. Kept: the channel, its briefs, proofs and
//! memory, other platforms' rows, and the deletion record itself (ids, counts and the confirmation code; no content).
//! Media files. The account's media files for the platform live under `<store root>/media/<venture>/<channel>/<account>/<platform>/`
//! (`MEDIA_DIR`) under the media root the host passes. The erase removes that directory and everything in it (links are removed, never followed) BEFORE the
//! request commits, and records the number of files removed as `media_files` in the target's `deleted` counts. A file
//! that cannot be removed fails the request and nothing is erased or recorded, so a retry starts clean. The engine writes no
//! media file under this path yet: the publish core fetches media from a URL and uploads it without keeping it, and the
//! daemon's `objects/` store is content-addressed by the daemon crate with no account link. The count is 0 until a writer uses
//! the layout. Not touched here: copies on the operator's desktop and backups (they expire; see the page).
//!
//! A request id is idempotent. The same id returns the stored record (`replayed: true`) and erases nothing again.
//! Posts on the platform are not deleted. Nothing here sends an email or calls a platform.
//!
//! The deletion runs in one transaction with foreign-key checks deferred to the commit, so a request either erases
//! everything it covers or nothing.
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::calendar_time as time;
use crate::protocol::{
    DeletionMetaCallbackInput, DeletionRecord, DeletionRequestInput, DeletionStatusInput,
    DeletionTarget, Scope, DELETION_SOURCES, META_PLATFORMS, PLATFORMS,
};
use crate::util::{bad, in_txn, internal, require_channel, WriteTxn};
use crate::Fail;

/// Methods that write. A dry run reports these and writes nothing.
pub const WRITE_METHODS: &[&str] = &["deletion.request", "deletion.meta_callback"];

/// Every method this module routes.
pub const METHODS: &[&str] = &[
    "deletion.request",
    "deletion.status",
    "deletion.meta_callback",
];

/// A stored deletion request row, before its targets are decoded.
#[derive(Debug, Clone)]
pub struct StoredRequest {
    pub source: String,
    pub requester_ref: Option<String>,
    pub state: String,
    /// The targets as JSON text (an array of [`DeletionTarget`]).
    pub targets_json: String,
    pub confirmation_code: String,
    pub requested_at: String,
    pub completed_at: String,
}

/// A new deletion request row to store.
#[derive(Debug, Clone)]
pub struct NewRequest<'a> {
    pub request_id: &'a str,
    pub source: &'a str,
    pub requester_ref: Option<&'a str>,
    /// `completed` or `no_match`.
    pub state: &'a str,
    pub targets_json: &'a str,
    pub confirmation_code: &'a str,
    pub stamp: &'a str,
}

/// What the host app provides: the request ledger and the erase of one account's rows.
pub trait DeletionStore: WriteTxn {
    /// The stored request with this id, if any.
    fn load_request(&self, request_id: &str) -> Result<Option<StoredRequest>, Fail>;
    /// The id of the request with this confirmation code, if any.
    fn request_id_by_code(&self, code: &str) -> Result<Option<String>, Fail>;
    /// Stores one request, with `requested_at` and `completed_at` both set to `stamp`.
    fn insert_request(&self, request: &NewRequest<'_>) -> Result<(), Fail>;
    /// The stored accounts whose handle is `handle` on one of `platforms`, ordered by venture, channel,
    /// account, platform.
    fn accounts_by_handle(
        &self,
        handle: &str,
        platforms: &[&str],
    ) -> Result<Vec<(Scope, String)>, Fail>;
    /// Erases the rows the host keeps about one account on one platform, inside the caller's transaction
    /// (foreign-key checks deferred to the commit), in an order that satisfies the foreign keys, and returns
    /// the row count per label. Does not touch media files: this module removes those.
    fn erase_rows(&self, scope: &Scope, platform: &str) -> Result<BTreeMap<String, u64>, Fail>;
}

/// Request ids are 1 to this many bytes: letters, digits, `-` and `_`.
const MAX_ID: usize = 64;
/// The longest platform user id a Meta callback may carry. The request id is `meta-` plus the id.
const MAX_USER_ID: usize = 48;
/// The longest `requester_ref`, in characters.
const MAX_REF: usize = 200;

fn encoded(value: serde_json::Result<Value>) -> Result<Value, Fail> {
    value.map_err(internal)
}

fn parse<T: DeserializeOwned>(input: Value, method: &str) -> Result<T, Fail> {
    serde_json::from_value(input).map_err(|e| bad(format!("{method} input: {e}")))
}

fn id_ok(raw: &str, max: usize) -> bool {
    !raw.is_empty()
        && raw.len() <= max
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn confirmation_code_of(request_id: &str) -> String {
    format!("del-{request_id}")
}

/// The directory under the media root that holds account media files, one subdirectory per scope and platform.
const MEDIA_DIR: &str = "media";

/// True for a stored scope or platform name that is safe as one path segment: no separators, no dot-leading names
/// (so no `..`), no drive or stream colon, no control characters.
fn segment_ok(raw: &str) -> bool {
    !raw.is_empty()
        && !raw.starts_with('.')
        && !raw
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
}

/// `<root>/media/<venture>/<channel>/<account>/<platform>`, or None when a name could not have been a path segment
/// (nothing can have been written there under this layout).
fn media_dir(root: &Path, scope: &Scope, platform: &str) -> Option<PathBuf> {
    [&scope.venture, &scope.channel, &scope.account]
        .into_iter()
        .map(String::as_str)
        .chain(std::iter::once(platform))
        .all(segment_ok)
        .then(|| {
            root.join(MEDIA_DIR)
                .join(&scope.venture)
                .join(&scope.channel)
                .join(&scope.account)
                .join(platform)
        })
}

fn media_io(path: &Path, e: impl std::fmt::Display) -> Fail {
    ("media_delete_failed", format!("{}: {e}", path.display()))
}

/// Most entries read from one media directory.
const MEDIA_LIST_LIMIT: usize = 1_000_000;

/// Removes everything below the pinned `dir` and returns the number of files (every non-directory entry, links
/// included) removed. Every step is relative to a pinned directory (rightkit-fs `StateDir`), so a path swapped or
/// symlinked meanwhile cannot redirect it, and a link is removed as a link, never followed.
fn empty_dir(dir: &rightkit_fs::StateDir) -> Result<u64, Fail> {
    use rightkit_fs::EntryKind;
    let mut removed = 0;
    for entry in dir
        .list_entries(MEDIA_LIST_LIMIT)
        .map_err(|e| media_io(dir.path(), e))?
    {
        let path = dir.path().join(&entry.name);
        match entry.kind {
            EntryKind::Dir => {
                let child = dir.child(&entry.name).map_err(|e| media_io(&path, e))?;
                removed += empty_dir(&child)?;
                // Windows cannot remove a directory while a handle to it is open.
                drop(child);
                dir.remove_dir(&entry.name)
                    .map_err(|e| media_io(&path, e))?;
            }
            EntryKind::Symlink => {
                dir.remove_link(&entry.name)
                    .map_err(|e| media_io(&path, e))?;
                removed += 1;
            }
            EntryKind::File | EntryKind::Other => {
                dir.remove(&entry.name).map_err(|e| media_io(&path, e))?;
                removed += 1;
            }
        }
    }
    Ok(removed)
}

/// Removes `dir` and everything below it (see [`empty_dir`]) and returns the number of files removed.
fn remove_tree(dir: &Path) -> Result<u64, Fail> {
    let pinned = rightkit_fs::StateDir::open_shared(dir).map_err(|e| media_io(dir, e))?;
    let removed = empty_dir(&pinned)?;
    drop(pinned);
    let (Some(parent), Some(name)) = (dir.parent(), dir.file_name().and_then(|n| n.to_str()))
    else {
        return Err(media_io(dir, "the media directory has no parent"));
    };
    rightkit_fs::StateDir::open_shared(parent)
        .and_then(|p| p.remove_dir(name))
        .map_err(|e| media_io(dir, e))?;
    Ok(removed)
}

/// Removes the account's media files for the platform and returns how many were removed (0 when there is no media
/// directory). A media directory that is itself a link or a plain file is refused (`media_unsafe`), never followed.
fn remove_media(root: &Path, scope: &Scope, platform: &str) -> Result<u64, Fail> {
    let Some(dir) = media_dir(root, scope, platform) else {
        return Ok(0);
    };
    match fs::symlink_metadata(&dir) {
        Ok(meta) if meta.is_dir() => remove_tree(&dir),
        Ok(_) => Err((
            "media_unsafe",
            format!(
                "{} is not a plain directory; it was not followed or removed",
                dir.display()
            ),
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(media_io(&dir, e)),
    }
}

/// Erases one account's data on one platform inside the caller's transaction and returns the count per label: the
/// host's row counts, then `media_files` (files removed from disk under `root`, after every statement succeeded).
fn erase(
    store: &dyn DeletionStore,
    root: &Path,
    scope: &Scope,
    platform: &str,
) -> Result<BTreeMap<String, u64>, Fail> {
    let mut counts = store.erase_rows(scope, platform)?;
    counts.insert(
        "media_files".to_string(),
        remove_media(root, scope, platform)?,
    );
    Ok(counts)
}

fn ref_of(raw: Option<&str>) -> Result<Option<String>, Fail> {
    let Some(raw) = raw else { return Ok(None) };
    let text = raw.trim();
    if text.is_empty() {
        return Ok(None);
    }
    if text.chars().count() > MAX_REF || text.chars().any(char::is_control) {
        return Err(bad(format!(
            "requester_ref must be at most {MAX_REF} characters with no control characters"
        )));
    }
    Ok(Some(text.to_string()))
}

fn load_request(
    store: &dyn DeletionStore,
    request_id: &str,
) -> Result<Option<DeletionRecord>, Fail> {
    store
        .load_request(request_id)?
        .map(|row| {
            Ok::<DeletionRecord, Fail>(DeletionRecord {
                request_id: request_id.to_string(),
                source: row.source,
                requester_ref: row.requester_ref,
                state: row.state,
                confirmation_code: row.confirmation_code,
                requested_at: row.requested_at,
                completed_at: row.completed_at,
                targets: serde_json::from_str(&row.targets_json).map_err(internal)?,
                replayed: false,
            })
        })
        .transpose()
}

/// Records one request that erased `targets`, inside the caller's transaction, and returns the stored record.
fn record_request(
    store: &dyn DeletionStore,
    request_id: &str,
    source: &str,
    requester_ref: Option<String>,
    targets: Vec<DeletionTarget>,
    stamp: &str,
) -> Result<DeletionRecord, Fail> {
    let any = targets.iter().any(|t| t.deleted.values().any(|n| *n > 0));
    let state = if any { "completed" } else { "no_match" };
    let confirmation_code = confirmation_code_of(request_id);
    let targets_json = serde_json::to_string(&targets).map_err(internal)?;
    store.insert_request(&NewRequest {
        request_id,
        source,
        requester_ref: requester_ref.as_deref(),
        state,
        targets_json: &targets_json,
        confirmation_code: &confirmation_code,
        stamp,
    })?;
    Ok(DeletionRecord {
        request_id: request_id.to_string(),
        source: source.to_string(),
        requester_ref,
        state: state.to_string(),
        confirmation_code,
        requested_at: stamp.to_string(),
        completed_at: stamp.to_string(),
        targets,
        replayed: false,
    })
}

/// The answer for a stored or newly written record; the flag says it was found stored (a replay).
fn finish((record, was_stored): (DeletionRecord, bool)) -> Result<Value, Fail> {
    if was_stored {
        replayed(record)
    } else {
        encoded(serde_json::to_value(record))
    }
}

fn replayed(mut record: DeletionRecord) -> Result<Value, Fail> {
    record.replayed = true;
    encoded(serde_json::to_value(record))
}

/// Route one method of this module. `root` is the media root. Unknown methods are `unknown_method`.
pub fn dispatch(
    store: &dyn DeletionStore,
    root: &Path,
    method: &str,
    input: Value,
) -> Result<Value, Fail> {
    match method {
        "deletion.request" => request(store, root, input),
        "deletion.status" => status(store, input),
        "deletion.meta_callback" => meta_callback(store, root, input),
        other => Err(("unknown_method", format!("unknown method: {other}"))),
    }
}

/// `deletion.request`: records the request and erases the account's data on the platform. The same `request_id`
/// again returns the stored record; the same id for a different scope, platform or source is `request_mismatch`.
pub fn request(store: &dyn DeletionStore, root: &Path, input: Value) -> Result<Value, Fail> {
    let req: DeletionRequestInput = parse(input, "deletion.request")?;
    if !id_ok(&req.request_id, MAX_ID) {
        return Err(bad(format!(
            "request_id must be 1 to {MAX_ID} letters, digits, '-' or '_', got {:?}",
            req.request_id
        )));
    }
    if !PLATFORMS.contains(&req.platform.as_str()) {
        return Err(bad(format!(
            "platform must be one of {}, got {}",
            PLATFORMS.join(", "),
            req.platform
        )));
    }
    if !DELETION_SOURCES.contains(&req.source.as_str()) || req.source == "meta_callback" {
        return Err(bad(format!(
            "source must be email or endpoint, got {}",
            req.source
        )));
    }
    let requester_ref = ref_of(req.requester_ref.as_deref())?;
    let stamp = time::format_instant(time::now_secs(req.now.as_deref())?);

    let outcome = in_txn(store, || {
        if let Some(existing) = load_request(store, &req.request_id)? {
            let same = existing.source == req.source
                && existing.targets.len() == 1
                && existing.targets[0].scope == req.scope
                && existing.targets[0].platform == req.platform;
            if !same {
                return Err((
                    "request_mismatch",
                    format!(
                        "request {} is already stored for a different scope, platform or source",
                        req.request_id
                    ),
                ));
            }
            return Ok((existing, true));
        }
        require_channel(store, &req.scope)?;
        let deleted = erase(store, root, &req.scope, &req.platform)?;
        let record = record_request(
            store,
            &req.request_id,
            &req.source,
            requester_ref.clone(),
            vec![DeletionTarget {
                scope: req.scope.clone(),
                platform: req.platform.clone(),
                deleted,
            }],
            &stamp,
        )?;
        Ok((record, false))
    })?;
    finish(outcome)
}

/// `deletion.meta_callback`: the handler for Meta's data deletion callback. `user_id` is the platform user id from
/// Meta's verified signed request. Every stored Meta-platform account whose handle is that id is erased. The request
/// id is `meta-` plus the user id, so Meta's retry returns the stored record. An id that matches no account is
/// recorded as `no_match` and still gets a confirmation code, so Meta always receives an answer.
pub fn meta_callback(store: &dyn DeletionStore, root: &Path, input: Value) -> Result<Value, Fail> {
    let req: DeletionMetaCallbackInput = parse(input, "deletion.meta_callback")?;
    if !id_ok(&req.user_id, MAX_USER_ID) {
        return Err(bad(format!(
            "user_id must be 1 to {MAX_USER_ID} letters, digits, '-' or '_', got {:?}",
            req.user_id
        )));
    }
    let request_id = format!("meta-{}", req.user_id);
    let stamp = time::format_instant(time::now_secs(req.now.as_deref())?);

    let outcome = in_txn(store, || {
        if let Some(existing) = load_request(store, &request_id)? {
            return Ok((existing, true));
        }
        let matched = store.accounts_by_handle(&req.user_id, &META_PLATFORMS)?;
        let mut targets = Vec::with_capacity(matched.len());
        for (scope, platform) in matched {
            let deleted = erase(store, root, &scope, &platform)?;
            targets.push(DeletionTarget {
                scope,
                platform,
                deleted,
            });
        }
        let record = record_request(store, &request_id, "meta_callback", None, targets, &stamp)?;
        Ok((record, false))
    })?;
    finish(outcome)
}

/// `deletion.status`: one request by `request_id` or `confirmation_code`. Exactly one of them is given.
fn status(store: &dyn DeletionStore, input: Value) -> Result<Value, Fail> {
    let req: DeletionStatusInput = parse(input, "deletion.status")?;
    let request_id = match (req.request_id, req.confirmation_code) {
        (Some(id), None) => id,
        (None, Some(code)) => store.request_id_by_code(&code)?.ok_or_else(|| {
            (
                "not_found",
                format!("no deletion request with confirmation code {code}"),
            )
        })?,
        _ => return Err(bad("give exactly one of request_id or confirmation_code")),
    };
    let record = load_request(store, &request_id)?
        .ok_or_else(|| ("not_found", format!("no deletion request {request_id}")))?;
    encoded(serde_json::to_value(record))
}
