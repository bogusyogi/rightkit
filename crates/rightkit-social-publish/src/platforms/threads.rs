//! Threads request shapes for the publish core: a text post, written in two steps and read back.
//!
//! 1. `POST /v1.0/<threads-user-id>/threads` with `media_type=TEXT` and `text` makes a CONTAINER and answers
//!    `{ "id": "<creation id>" }`.
//! 2. `POST /v1.0/<threads-user-id>/threads_publish` with `creation_id` publishes it and answers
//!    `{ "id": "<media id>" }`.
//! 3. `GET /v1.0/<media-id>?fields=id,permalink,text,timestamp` reads the post back.
//!
//! The phase machine keeps the container family's names (`create_container -> publish -> read_permalink ->
//! done`). With `settings.wait_for_container` set, a `poll_status` phase reads the container status between
//! `create_container` and `publish` (see docs/PUBLISH-CORE.md). This module supplies what is Threads-specific: the
//! request bodies and paths, the text rules and the answer parsing.
//!
//! No idempotency key. `threads_publish` has none, so a lost answer after step 2 can never be made safe by
//! repeating it. Step 2 therefore carries NO `Idempotency-Key`, an ambiguous answer is `outcome_unknown`, and
//! `publish.reconcile` (machine.rs `reconcile_threads`) never publishes: it may only adopt a post it can READ
//! BY MEDIA ID, and without one it leaves the job `outcome_unknown` for a person. A lost answer after step 1
//! is different: it can only orphan an unpublished container, which is harmless, so reconcile returns such a
//! job to `queued` once and the retry makes a NEW container (the shared `create_container` rule in machine.rs).
//!
//! The user id. The `<threads-user-id>` in the path is the job's stored `remote_account`, validated as a plain
//! id. No text from the caption or the settings reaches a path.
//!
//! Secrets: the access token is the command's `token`, sent only as the `Authorization: Bearer` header of a
//! request, never in the URL or the query string, and scrubbed from every message by `http::send_with`.
//! Nothing in this file builds a message from the token.
//!
//! Media. Threads takes media by PUBLIC URL, so nothing is uploaded and there is no upload step: the media
//! URL (from `media_urls` at enqueue) goes into the container create. One image is `media_type=IMAGE` with
//! `image_url`, one video `media_type=VIDEO` with `video_url`. Two to twenty items are a carousel: the
//! `create_container` step makes one child container per item (`is_carousel_item=true`) and then the carousel
//! container (`media_type=CAROUSEL`, `children` = the child ids joined by commas), all inside the one step
//! (see [`create_container`]); the carousel's id is the job's `container_id`, so no phase is added. A lost
//! answer in the middle can only orphan unpublished containers, which is harmless, so the shared
//! `create_container` retry rule applies unchanged. The media URL must be `https` (Meta fetches it). From
//! docs/p1/capability-matrix.md (verified 2026-10-16): TEXT, IMAGE, VIDEO and CAROUSEL exist, a carousel takes 2
//! to 20 items, media must be publicly hosted at publish time. UNVERIFIED here: the parameter names
//! (`image_url`, `video_url`, `is_carousel_item`, `children`) and that they are accepted in a JSON body, the
//! comma-joined `children` form, that carousel children may be videos and must be FINISHED before the carousel
//! is created (children are not polled here), and the size and duration limits of video.
//!
//! Video (#139). A VIDEO container is processed by Threads after it is created, so a job with any video item
//! polls the container status (`GET /v1.0/<container id>?fields=status,error_message`) and publishes only when
//! it reports FINISHED, with no setting needed (see [`needs_poll`]); `wait_for_container` still turns the poll on
//! for an image post. A status of ERROR or EXPIRED fails the job and a container still IN_PROGRESS after the
//! machine's poll limit fails it `container_timeout` (machine.rs). Sourced (docs/p1/capability-matrix.md,
//! developers.facebook.com/docs/threads, accessed 2026-10-16): video is MOV or MP4, at most 5 minutes and 1 GB. The
//! engine never fetches the file, so the size is not checked here; the duration is checked when the entry states
//! `duration_ms` ([`VIDEO_MAX_MS`]) and a URL whose extension is not `mp4` or `mov` is refused. UNVERIFIED: the
//! status values IN_PROGRESS, FINISHED, ERROR and EXPIRED and the `error_message` field, and that the status read
//! works on a container id.
//!
//! Protocol facts used here come from Meta's published Threads API documentation and were not verified
//! against a live server in this task; docs/PUBLISH-CORE.md lists what is left out because it is not known.
use rightkit_http::percent_encode;
use serde_json::{json, Value};

use crate::http::{Reply, RetryAfter};
use crate::Fail;

/// The platform name this module drives.
pub const PLATFORM: &str = "threads";
/// Longest `text` a Threads text post takes, counted here in UTF-16 code units (the stricter reading of
/// "characters": a code unit count is never below the scalar count).
pub const TEXT_LIMIT: usize = 500;
/// The fields asked for on the read-back.
const READ_FIELDS: &str = "id,permalink,text,timestamp";
/// Longest user, container or media id accepted.
const MAX_ID: usize = 64;
/// Longest permalink accepted from an answer.
const MAX_PERMALINK: usize = 512;
/// OAuth error code for an expired or invalid access token.
const TOKEN_CODE: i64 = 190;
/// Error codes of the documented throttling family: treated as rate limited like an HTTP 429.
const THROTTLE_CODES: [i64; 4] = [4, 17, 32, 613];

/// Fewest and most items of a carousel (docs/p1/capability-matrix.md, verified 2026-10-16).
pub const CAROUSEL_MIN: usize = 2;
pub const CAROUSEL_MAX: usize = 20;
/// Longest media URL accepted.
const MAX_MEDIA_URL: usize = 2_048;
/// Longest video, in milliseconds (sourced: 5 minutes).
pub const VIDEO_MAX_MS: u64 = 5 * 60 * 1_000;

/// The lowercase extension of the last path segment of an https URL, None when it has none.
fn url_extension(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path = rest.split(['?', '#']).next().unwrap_or("");
    let mut segments = path.split('/');
    segments.next();
    let (_, ext) = segments.next_back()?.rsplit_once('.')?;
    (!ext.is_empty()).then(|| ext.to_ascii_lowercase())
}

/// One media item to post: its kind (`image` or `video`) and its public https URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaRef {
    video: bool,
    url: String,
}

/// What a job posts, validated and ready to serialise.
pub struct Prepared {
    user: String,
    text: String,
    media: Vec<MediaRef>,
}

impl Prepared {
    /// `POST /v1.0/<user>/threads`.
    pub fn create_path(&self) -> String {
        format!("/v1.0/{}/threads", percent_encode(&self.user))
    }

    /// `POST /v1.0/<user>/threads_publish`.
    pub fn publish_path(&self) -> String {
        format!("/v1.0/{}/threads_publish", percent_encode(&self.user))
    }

    /// The container create body of a text post or of ONE media item. A carousel has no single body (its
    /// children must exist first): use [`create_container`]; `prepare` never returns a carousel.
    pub fn create_body(&self) -> Value {
        match self.media.as_slice() {
            [] => json!({ "media_type": "TEXT", "text": self.text }),
            [one] => {
                let mut body = media_body(one);
                if let Value::Object(map) = &mut body {
                    map.insert("text".to_string(), Value::String(self.text.clone()));
                }
                body
            }
            // Unreachable through `prepare`; kept total so a misuse is refused by the platform, not posted.
            _ => json!({ "media_type": "CAROUSEL", "text": self.text }),
        }
    }

    /// True for a carousel (two or more media items).
    pub fn is_carousel(&self) -> bool {
        self.media.len() >= CAROUSEL_MIN
    }
}

/// `{media_type, image_url|video_url}` of one item.
fn media_body(item: &MediaRef) -> Value {
    if item.video {
        json!({ "media_type": "VIDEO", "video_url": item.url })
    } else {
        json!({ "media_type": "IMAGE", "image_url": item.url })
    }
}

/// The create body of a carousel child: the item with `is_carousel_item` and no text.
fn child_body(item: &MediaRef) -> Value {
    let mut body = media_body(item);
    if let Value::Object(map) = &mut body {
        map.insert("is_carousel_item".to_string(), Value::Bool(true));
    }
    body
}

/// The carousel container body for the child ids the platform returned (comma-joined, UNVERIFIED form).
fn carousel_body(text: &str, children: &[String]) -> Value {
    json!({ "media_type": "CAROUSEL", "children": children.join(","), "text": text })
}

/// The whole `create_container` step: one request for a text or single-media post, and for a carousel one
/// request per item and then the carousel's. `send(path, body)` performs one POST. Returns the answer the
/// machine handles as the step's answer (the container's `{id}`) and how many requests were made. The first
/// request that is not a 2xx answer with an id ends the step with that answer (an item answering no id
/// ends it with `Reply::Invalid`), so the job is retried or failed exactly as a single create would be.
/// Children made before a failure stay unpublished and unused, which is harmless.
pub fn create_container(
    prepared: &Prepared,
    mut send: impl FnMut(&str, &Value) -> Reply,
) -> (Reply, i64) {
    // A request that could not be built (`Reply::Invalid`) sent nothing, so it is not counted.
    fn counted(reply: &Reply) -> i64 {
        if matches!(reply, Reply::Invalid(_)) {
            0
        } else {
            1
        }
    }
    let path = prepared.create_path();
    if !prepared.is_carousel() {
        let reply = send(&path, &prepared.create_body());
        let calls = counted(&reply);
        return (reply, calls);
    }
    let mut calls = 0;
    let mut children: Vec<String> = Vec::new();
    for item in &prepared.media {
        let reply = send(&path, &child_body(item));
        calls += counted(&reply);
        match reply {
            Reply::Ok(body) => match object_id(&body) {
                Some(id) => children.push(id),
                None => return (
                    Reply::Invalid(
                        "a carousel item container answered with no usable id; nothing was posted"
                            .to_string(),
                    ),
                    calls,
                ),
            },
            other => return (other, calls),
        }
    }
    let reply = send(&path, &carousel_body(&prepared.text, &children));
    calls += counted(&reply);
    (reply, calls)
}

/// True for an `https://` URL with a host, no user info, no whitespace or control characters, at most
/// `MAX_MEDIA_URL` characters (Meta fetches it, so it must be public https).
fn public_https_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    !authority.is_empty()
        && !authority.contains('@')
        && url.len() <= MAX_MEDIA_URL
        && !url
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
}

/// The media items of a payload as [`MediaRef`]s: image or video only, each with a public https URL, none or
/// 1 to [`CAROUSEL_MAX`] of them.
fn media_of(payload: &crate::wire::PublishPayload) -> Result<Vec<MediaRef>, Fail> {
    if payload.media.len() > CAROUSEL_MAX {
        return Err((
            "media_unsupported",
            format!(
                "a threads carousel holds at most {CAROUSEL_MAX} items; the entry has {}",
                payload.media.len()
            ),
        ));
    }
    let mut out = Vec::new();
    for item in &payload.media {
        let video = match item.kind.as_str() {
            "image" => false,
            "video" => true,
            other => {
                return Err((
                    "media_unsupported",
                    format!("a threads post carries images or videos, not {other:?}"),
                ))
            }
        };
        let Some(url) = item.url.as_deref().filter(|u| !u.trim().is_empty()) else {
            return Err((
                "media_required",
                format!(
                    "the threads media item {} has no URL: pass media_urls for its hash to publish.enqueue",
                    item.hash
                ),
            ));
        };
        if !public_https_url(url) {
            return Err((
                "media_url_invalid",
                format!(
                    "the threads media URL of item {} must be a public https:// URL with a host, no user info and no spaces (at most {MAX_MEDIA_URL} characters)",
                    item.hash
                ),
            ));
        }
        if video {
            if let Some(ms) = item.duration_ms.filter(|ms| *ms > VIDEO_MAX_MS) {
                return Err((
                    "media_too_long",
                    format!(
                        "the threads video {} lasts {ms} ms and Threads takes at most {VIDEO_MAX_MS} ms (5 minutes); nothing was posted",
                        item.hash
                    ),
                ));
            }
            if let Some(ext) = url_extension(url).filter(|e| e != "mp4" && e != "mov") {
                return Err((
                    "media_url_invalid",
                    format!(
                        "the threads video URL of item {} ends in .{ext}; Threads takes MOV or MP4",
                        item.hash
                    ),
                ));
            }
        }
        out.push(MediaRef {
            video,
            url: url.to_string(),
        });
    }
    Ok(out)
}

/// True when the job asks to wait for the container to be FINISHED before publishing.
pub fn wants_poll(settings: &Value) -> bool {
    settings
        .get("wait_for_container")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// True when the job must wait for its container to be FINISHED before publishing: the opt-in
/// `wait_for_container` setting, or any video item (Threads processes a video after the create).
pub fn needs_poll(payload: &crate::wire::PublishPayload) -> bool {
    wants_poll(&payload.settings) || payload.media.iter().any(|item| item.kind == "video")
}

/// `GET /v1.0/<container id>?fields=status,error_message`. None when the stored id is not a plain id.
pub fn status_path(container_id: &str) -> Option<String> {
    plain_id(container_id).then(|| {
        format!(
            "/v1.0/{}?fields=status,error_message",
            percent_encode(container_id)
        )
    })
}

/// The container `status` of a status answer; when the answer names an `id` it must be this container.
pub fn status_of<'a>(container_id: &str, body: &'a Value) -> Option<&'a str> {
    if let Some(id) = body.get("id").and_then(Value::as_str) {
        if id != container_id {
            return None;
        }
    }
    body.get("status").and_then(Value::as_str)
}

/// The publish request body for a container id the platform returned.
pub fn publish_body(container_id: &str) -> Value {
    json!({ "creation_id": container_id })
}

/// True for a plain id: letters, digits, `-` and `_`, 1 to 64 of them.
fn plain_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The characters Threads counts for `text`: UTF-16 code units (see [`TEXT_LIMIT`]).
pub fn counted_length(text: &str) -> usize {
    text.encode_utf16().count()
}

/// `prepare` that also accepts a carousel (two to twenty media items).
pub fn prepare_any(
    payload: &crate::wire::PublishPayload,
    remote_account: &str,
) -> Result<Prepared, Fail> {
    if !plain_id(remote_account) {
        return Err((
            "bad_input",
            format!(
                "remote_account must be a Threads user id (letters, digits, '-' or '_', at most {MAX_ID}) to become the request path"
            ),
        ));
    }
    let media = media_of(payload)?;
    // The one setting is `wait_for_container` (a boolean): poll the container status until FINISHED before
    // publishing. Any other setting is refused, because ignoring it silently would post the wrong thing.
    let unused = match &payload.settings {
        Value::Null => false,
        Value::Object(map) => map
            .iter()
            .any(|(k, v)| k != "wait_for_container" || !v.is_boolean()),
        _ => true,
    };
    if unused {
        return Err((
            "bad_input",
            "the threads client uses only the boolean setting wait_for_container; remove the others from the job"
                .to_string(),
        ));
    }
    let text = payload.caption.clone();
    if text.trim().is_empty() {
        return Err((
            "caption_empty",
            "the threads post text is empty".to_string(),
        ));
    }
    let count = counted_length(&text);
    if count > TEXT_LIMIT {
        return Err((
            "text_too_long",
            format!("the post text counts {count} characters and Threads allows {TEXT_LIMIT}"),
        ));
    }
    Ok(Prepared {
        user: remote_account.to_string(),
        text,
        media,
    })
}

/// `publish.enqueue` check: the payload and account must be ones this module can post (a carousel included).
pub fn check(payload: &crate::wire::PublishPayload, remote_account: &str) -> Result<(), Fail> {
    prepare_any(payload, remote_account).map(|_| ())
}

/// The id in a create or publish answer: a plain string id. Anything else is not accepted as this job's
/// container or post.
pub fn object_id(body: &Value) -> Option<String> {
    let id = body.get("id")?.as_str()?;
    plain_id(id).then(|| id.to_string())
}

/// The `GET /v1.0/<media-id>?fields=...` path that reads back a stored media id. None when the stored value
/// is not a plain id, so nothing odd is ever put in a path. The token is never part of it.
pub fn read_path(media_id: &str) -> Option<String> {
    plain_id(media_id).then(|| format!("/v1.0/{}?fields={READ_FIELDS}", percent_encode(media_id)))
}

/// The read-back `text`, once the read has confirmed the stored post: the answer's `id` must be the stored
/// media id.
pub fn confirmed_text<'a>(media_id: &str, body: &'a Value) -> Option<&'a str> {
    if body.get("id").and_then(Value::as_str) != Some(media_id) {
        return None;
    }
    body.get("text").and_then(Value::as_str)
}

/// The permalink, once the read has confirmed the stored post: the answer's `id` must be the stored media
/// id and its `permalink` a plain `https://` URL.
pub fn permalink_from(media_id: Option<&str>, body: &Value) -> Option<String> {
    let id = media_id?;
    if !plain_id(id) || body.get("id").and_then(Value::as_str) != Some(id) {
        return None;
    }
    let link = body.get("permalink")?.as_str()?;
    let plain = link.len() <= MAX_PERMALINK
        && link.starts_with("https://")
        && !link.chars().any(|c| c.is_whitespace() || c.is_control());
    plain.then(|| link.to_string())
}

/// The `error.code` of a Graph-style error body. The message a rejected reply carries is the body text cut
/// to 200 characters, so when it no longer parses as JSON the first `"code"` member is read from the text.
pub fn error_code(message: &str) -> Option<i64> {
    if let Ok(value) = serde_json::from_str::<Value>(message) {
        return value.get("error")?.get("code")?.as_i64();
    }
    let at = message.find("\"code\"")?;
    let rest = message[at + "\"code\"".len()..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// A Threads error can arrive as a 4xx body instead of the bare status: code 190 is an expired or invalid
/// token (the existing `token_expired` outcome), and codes 4, 17, 32 and 613 are the throttling family (a
/// 429). A rate limit names no wait here, so the machine's default backoff applies. Everything else is left
/// as it came.
pub fn normalize(reply: Reply) -> Reply {
    if let Reply::Rejected { message, .. } = &reply {
        if let Some(code) = error_code(message) {
            if code == TOKEN_CODE {
                return Reply::Unauthorized;
            }
            if THROTTLE_CODES.contains(&code) {
                return Reply::RateLimited(RetryAfter::Unspecified);
            }
        }
    }
    reply
}
