//! The delayed follow-up comment (`first_comment`, #1497): the `comment` phase that runs after a post is live.
//!
//! The text is `PublishPayload::first_comment`, frozen at enqueue from the approved calendar entry. When a job
//! reaches `done` (published, with or without a permalink) and has such a text and no stored comment id, the
//! machine moves it to phase `comment` with `next_attempt_at` set to `settings.first_comment_delay_secs` (default
//! [`DEFAULT_DELAY_SECS`]) after the publish. The job keeps state `published` throughout: the post is live and
//! a comment problem never changes that. A step then sends the one comment request; its result is stored as
//! `remote.comment_id` and the phase becomes `done`. A refusal or a failure that is certain (a rejection, an
//! expired token, retries exhausted) ends the phase with the post untouched and the reason in `last_error_code`
//! (`comment_rejected`, `comment_failed`, `token_expired`, ...), outcome `comment_failed`.
//!
//! A lost answer to the comment write is `outcome_unknown` at phase `comment`, and `publish.reconcile` settles it
//! without a blind repeat (see `machine::reconcile_comment`):
//!
//! | Platform | Request (verified from the platform's docs on 2026-11-03) | Lost answer |
//! |---|---|---|
//! | Mastodon | `POST /api/v1/statuses` with `in_reply_to_id`, `Idempotency-Key: <job key>:comment` (stored 1 hour) | Repeated with the same key inside the window: returns the first status. |
//! | Facebook | `POST /v<ver>/<post id>/comments`, `message` (Page token with `pages_manage_engagement`, or the MODERATE task) | `GET /<post id>/comments` and adopt the one comment whose text is the job's. |
//! | Instagram | `POST /v<ver>/<media id>/comments`, `message` (`instagram_business_manage_comments` / `instagram_manage_comments`); not for live video | `GET /<media id>/comments?fields=id,text`, same match. |
//! | LinkedIn | `POST /rest/socialActions/<post urn>/comments`, body `{actor, object, message:{text}}`, version headers; the new id is the `x-restli-id` header | `GET /rest/socialActions/<post urn>/comments`, match on the actor and the text. |
//! | X | `POST /2/tweets` with `reply.in_reply_to_tweet_id` | No read by reply is built (it needs the paid search endpoint): nothing is repeated; a person records what they saw with `publish.resolve_unknown`. |
//!
//! Not supported, so a `first_comment` on these platforms is refused at `check` and skipped by the machine with
//! `comment_unsupported`: Pinterest and TikTok (no create-comment endpoint in the documented API), YouTube
//! (`commentThreads.insert` needs the channel id and `youtube.force-ssl`; the channel binding is not verified),
//! Threads (reply creation is a two-request container flow whose docs were not found), Bluesky (a reply needs the
//! parent's CID and root, which the job does not store). Every request here is "code-complete, unverified"
//! against the live platform; the endpoints and field names are the documented ones.
use rightkit_http::{percent_encode, Method};
use serde_json::{json, Value};

use crate::http::{self, Reply};
use crate::machine::{current_token, Drive};
use crate::platforms::{facebook, instagram, linkedin, mastodon, x};
use crate::protocol::PublishJob;
use crate::wire::PublishPayload;
use crate::Fail;

/// The phase name.
pub const PHASE: &str = "comment";
/// Seconds after the publish before the comment is sent when the job names no delay.
pub const DEFAULT_DELAY_SECS: i64 = 60;
/// The longest delay a job may name: one day.
pub const MAX_DELAY_SECS: i64 = 86_400;

/// True when the engine can post a first comment on `platform`.
pub fn supports(platform: &str) -> bool {
    matches!(
        platform,
        x::PLATFORM
            | facebook::PLATFORM
            | instagram::PLATFORM
            | mastodon::PLATFORM
            | linkedin::PLATFORM
    )
}

/// The job's follow-up text when one is wanted: non-blank and not yet posted.
pub fn text_of(payload: &PublishPayload) -> Option<&str> {
    payload
        .first_comment
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

/// The seconds to wait after the publish: `settings.first_comment_delay_secs`, else the default.
pub fn delay_secs(payload: &PublishPayload) -> i64 {
    payload
        .settings
        .get("first_comment_delay_secs")
        .and_then(Value::as_i64)
        .unwrap_or(DEFAULT_DELAY_SECS)
        .clamp(0, MAX_DELAY_SECS)
}

/// The enqueue-time check of a payload's `first_comment`: a platform that can comment, a text with no control
/// characters (a newline is fine), X's 280 weighted characters, and a delay within bounds. `Ok` for a payload
/// with no follow-up.
pub fn check(platform: &str, payload: &PublishPayload) -> Result<(), Fail> {
    let Some(text) = text_of(payload) else {
        if payload.first_comment.is_some() {
            return Err((
                "first_comment_empty",
                "first_comment is blank; leave it out or give the text".to_string(),
            ));
        }
        return Ok(());
    };
    if !supports(platform) {
        return Err((
            "comment_unsupported",
            format!("the publish engine cannot post a first comment on {platform}"),
        ));
    }
    if text.chars().any(|c| c.is_control() && c != '\n') {
        return Err((
            "first_comment_invalid",
            "first_comment holds a control character".to_string(),
        ));
    }
    if platform == x::PLATFORM && x::counted_length(text) > x::TEXT_LIMIT {
        return Err((
            "text_too_long",
            format!(
                "first_comment is {} characters by X's counting; the limit is {}",
                x::counted_length(text),
                x::TEXT_LIMIT
            ),
        ));
    }
    if let Some(raw) = payload.settings.get("first_comment_delay_secs") {
        let in_range = raw
            .as_i64()
            .is_some_and(|secs| (0..=MAX_DELAY_SECS).contains(&secs));
        if !in_range {
            return Err((
                "first_comment_delay_invalid",
                format!("settings.first_comment_delay_secs must be 0 to {MAX_DELAY_SECS}"),
            ));
        }
    }
    Ok(())
}

/// A stored id that may be put in a path: plain printable characters only.
fn path_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 512 && id.bytes().all(|b| b.is_ascii_graphic())
}

/// The comment's id from the create answer. LinkedIn returns it in the `x-restli-id` header (echoed under
/// `headers`) and in the body `id`; the others return `id`, X as `data.id`.
pub fn created_id(platform: &str, body: &Value) -> Option<String> {
    if platform == x::PLATFORM {
        return x::created_post_id(body);
    }
    if platform == mastodon::PLATFORM {
        return mastodon::created_post_id(body);
    }
    let from_body = body.get("id").and_then(|id| match id {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    });
    let id = if platform == linkedin::PLATFORM {
        body.get("headers")
            .and_then(|h| h.get("x-restli-id"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or(from_body)
    } else {
        from_body
    };
    id.filter(|id| path_id(id))
}

fn graph_path(version: &str, id: &str, edge: &str) -> Option<String> {
    (path_id(id) && !id.contains('/')).then(|| format!("/{version}/{}/{edge}", percent_encode(id)))
}

fn linkedin_path(post_urn: &str) -> Option<String> {
    linkedin::read_path(post_urn)?;
    Some(format!(
        "/rest/socialActions/{}/comments",
        percent_encode(post_urn)
    ))
}

/// Sends the comment write of the job's current step. The post id must be stored. A request that cannot be
/// built is `Reply::Invalid` and nothing is sent. The answer is already normalised for the platform.
pub(crate) fn send(job: &PublishJob, d: &Drive) -> Reply {
    let Some(text) = text_of(&job.payload) else {
        return Reply::Invalid("the job has no first_comment to post".to_string());
    };
    let Some(post_id) = job.remote.post_id.as_deref() else {
        return Reply::Invalid("the comment step has no post id".to_string());
    };
    let token = current_token(d);
    let platform = job.platform.as_str();
    if platform == x::PLATFORM {
        if x::read_path(post_id).is_none() {
            return Reply::Invalid("the stored x post id is not a plain id".to_string());
        }
        return http::send(
            &d.client,
            Method::Post,
            &format!("{}{}", d.base, x::CREATE_PATH),
            &token,
            None,
            Some(&json!({ "text": text, "reply": { "in_reply_to_tweet_id": post_id } })),
        );
    }
    if platform == mastodon::PLATFORM {
        if post_id.is_empty() || !post_id.bytes().all(|b| b.is_ascii_digit()) {
            return Reply::Invalid("the stored mastodon status id is not a plain id".to_string());
        }
        let key = format!("{}:comment", job.idempotency_key);
        return http::send(
            &d.client,
            Method::Post,
            &format!("{}{}", d.base, mastodon::STATUSES_PATH),
            &token,
            Some(key.as_str()),
            Some(&json!({ "status": text, "in_reply_to_id": post_id })),
        );
    }
    if platform == facebook::PLATFORM || platform == instagram::PLATFORM {
        // Instagram's pinned Graph version is `facebook::GRAPH_VERSION` (one constant).
        let Some(path) = graph_path(facebook::GRAPH_VERSION, post_id, "comments") else {
            return Reply::Invalid(format!("the stored {platform} post id is not a plain id"));
        };
        let reply = http::send(
            &d.client,
            Method::Post,
            &format!("{}{}", d.base, path),
            &token,
            None,
            Some(&json!({ "message": text })),
        );
        return if platform == facebook::PLATFORM {
            facebook::normalize(reply)
        } else {
            instagram::normalize(reply)
        };
    }
    if platform == linkedin::PLATFORM {
        let Some(path) = linkedin_path(post_id) else {
            return Reply::Invalid("the stored linkedin post id is not a post URN".to_string());
        };
        let actor = match linkedin::owner(&job.payload, &job.remote_account) {
            Ok(actor) => actor,
            Err((_, message)) => return Reply::Invalid(message),
        };
        let version = match linkedin::api_version() {
            Ok(version) => version,
            Err((_, message)) => return Reply::Invalid(message),
        };
        let headers = linkedin::request_headers(version);
        return http::send_with(
            &d.client,
            Method::Post,
            &format!("{}{}", d.base, path),
            &token,
            None,
            Some(&json!({
                "actor": actor,
                "object": post_id,
                "message": { "text": text },
            })),
            &http::Extra {
                headers: &headers,
                echo: &linkedin::ECHO,
            },
        );
    }
    Reply::Invalid(format!("no comment request exists for {platform}"))
}

/// Reads the comments on the live post, for a reconcile. `None` for a platform that has no read built (X,
/// Mastodon: Mastodon repeats with its key instead).
pub(crate) fn list(job: &PublishJob, d: &Drive) -> Option<Reply> {
    let post_id = job.remote.post_id.as_deref()?;
    let token = current_token(d);
    let platform = job.platform.as_str();
    if platform == facebook::PLATFORM || platform == instagram::PLATFORM {
        let fields = if platform == facebook::PLATFORM {
            "id,message"
        } else {
            "id,text"
        };
        let Some(path) = graph_path(facebook::GRAPH_VERSION, post_id, "comments") else {
            return Some(Reply::Invalid(format!(
                "the stored {platform} post id is not a plain id"
            )));
        };
        let reply = http::send(
            &d.client,
            Method::Get,
            &format!("{}{}?fields={fields}&limit=100", d.base, path),
            &token,
            None,
            None,
        );
        return Some(if platform == facebook::PLATFORM {
            facebook::normalize(reply)
        } else {
            instagram::normalize(reply)
        });
    }
    if platform == linkedin::PLATFORM {
        let Some(path) = linkedin_path(post_id) else {
            return Some(Reply::Invalid(
                "the stored linkedin post id is not a post URN".to_string(),
            ));
        };
        let version = match linkedin::api_version() {
            Ok(version) => version,
            Err((_, message)) => return Some(Reply::Invalid(message)),
        };
        let headers = linkedin::request_headers(version);
        return Some(http::send_with(
            &d.client,
            Method::Get,
            &format!("{}{}", d.base, path),
            &token,
            None,
            None,
            &http::Extra {
                headers: &headers,
                echo: &[],
            },
        ));
    }
    None
}

fn same_text(a: &str, b: &str) -> bool {
    let norm = |t: &str| t.split_whitespace().collect::<Vec<_>>().join(" ");
    norm(a) == norm(b)
}

/// The ids of the comments in a list answer whose text is the job's first comment (and, on LinkedIn, whose
/// actor is the job's author). One is this job's comment; several are left for a person.
pub(crate) fn matching(job: &PublishJob, body: &Value) -> Vec<String> {
    let Some(want) = text_of(&job.payload) else {
        return Vec::new();
    };
    let platform = job.platform.as_str();
    let (items, text_key) = if platform == linkedin::PLATFORM {
        (body.get("elements"), "message")
    } else if platform == instagram::PLATFORM {
        (body.get("data"), "text")
    } else {
        (body.get("data"), "message")
    };
    let actor = if platform == linkedin::PLATFORM {
        linkedin::owner(&job.payload, &job.remote_account).ok()
    } else {
        None
    };
    items
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let id = item.get("id").and_then(|id| match id {
                        Value::String(s) => Some(s.clone()),
                        Value::Number(n) => Some(n.to_string()),
                        _ => None,
                    })?;
                    let text = if platform == linkedin::PLATFORM {
                        item.get(text_key)?.get("text")?.as_str()?
                    } else {
                        item.get(text_key)?.as_str()?
                    };
                    if let Some(actor) = actor.as_deref() {
                        if item.get("actor").and_then(Value::as_str) != Some(actor) {
                            return None;
                        }
                    }
                    (same_text(text, want) && path_id(&id)).then_some(id)
                })
                .collect()
        })
        .unwrap_or_default()
}
