//! YouTube (video upload) request shapes for the publish core: `videos.insert` through the RESUMABLE upload
//! protocol, and a read-back with `videos.list`.
//!
//! The flow, as three steps of the phase machine (`create`, `create`, `read_permalink`):
//!
//! 1. Session step (phase `create`, no session stored). The video is fetched from its media URL (plain GET, no
//!    token), checked (size, magic bytes), and `POST <base>/upload/youtube/v3/videos?uploadType=resumable&part=snippet,status`
//!    starts an upload session. The body is `{ snippet: { title, description }, status: { privacyStatus, ... } }`;
//!    `X-Upload-Content-Length` and `X-Upload-Content-Type` announce the file. The answer is `200` with the
//!    session URI in its `Location` header. Only the path and query of the URI are kept, in the job's
//!    `remote.container_id` (as JSON with the file size); a URI that does not start with the base URL is refused,
//!    so the token can never be sent to another host. Nothing is created on the channel yet.
//! 2. Upload step (phase `create`, session stored). The video is fetched again, and the upload is driven in
//!    chunks of [`CHUNK_SIZE`]: first a status probe (`PUT` with `Content-Range: bytes */<total>`), which tells
//!    what the platform already holds (`308` with a `Range` header, or nothing yet), then one `PUT` per chunk
//!    (`Content-Range: bytes <first>-<last>/<total>`). A `308` answer means "chunk kept, send the next"; `200` or
//!    `201` with the video resource means the upload is complete, and its `id` is the video id. A chunk whose
//!    answer is lost (dropped connection, timeout, 5xx) is NOT repeated blind: the driver probes again and
//!    continues from the offset the platform reports, at most [`MAX_RESUMES`] times per step. When the final
//!    chunk's answer is lost, the probe answers with the finished resource, so the video id is still learned.
//!    The session survives a failed step (it is in `remote.container_id`), so the next step resumes from the
//!    platform's offset instead of starting over. This is why a lost answer here is never `outcome_unknown`: the
//!    protocol itself reports what was received.
//! 3. Read-back (phase `read_permalink`): `GET <base>/youtube/v3/videos?part=id,status,fileDetails&id=<id>`. The
//!    video must be the stored id and its `status.privacyStatus` must equal the privacy the job asked for, so a
//!    video that came out public although the job said private is never reported as published with a link.
//!
//! Privacy. `settings.privacy_status` is `private` (the default), `unlisted` or `public`. The default is
//! `private` because the API publishes PUBLIC when no `privacyStatus` is set and an unverified API project is NOT
//! restricted to private (docs/p1/capability-matrix.md section 10, source check 2026-10-16: the videos.insert
//! page). `public` must be asked for. Other settings: `title` (else the first non-empty caption line) and
//! `made_for_kids` (bool, sent as `status.selfDeclaredMadeForKids` only when the job sets it; no default is
//! invented). Any other setting is refused, as ignoring one silently would publish the wrong thing.
//!
//! Sourced (docs/p1/capability-matrix.md, accessed 2026-10-16): `videos.insert` costs 1 unit, in its own bucket
//! of 100 calls per day (the other endpoints share 10,000 units per day); scopes `youtube.upload`, `youtube`,
//! `youtubepartner` or `youtube.force-ssl`; files up to 256 GB. UNVERIFIED (recalled, no fetched source):
//! the endpoint paths and the `uploadType=resumable&part=...` query, the `X-Upload-Content-*` and `Content-Range`
//! header protocol, the `308` plus `Range` answer, the 256 KiB chunk granularity, the `status` probe answering the
//! finished resource when complete, the 100-character title and 5,000-byte description limits, the refusal of `<`
//! and `>` in a title or description, `fileDetails.container` as the media type read-back (and its availability
//! only after processing, for the owner), the `quotaExceeded` 403 shape, the permalink form
//! `https://www.youtube.com/watch?v=<id>`, and that a quota is charged when the session is opened.
//!
//! Limits of this slice. The file is held in memory and fetched with the client's response cap (32 MiB), so a
//! larger video cannot be uploaded yet ([`MAX_VIDEO_BYTES`]); video media is not yet served from R2 (docs/p1
//! capability matrix, blocker: R2 media upload access). The fetch is repeated by the upload step.
//!
//! Secrets: the access token is the command's `token`, sent only as the `Authorization: Bearer` header, never in
//! a URL, and scrubbed from every message by `http::send_body`. The session URI carries an upload id, which is a
//! capability of its own: it is kept as a relative path, redacted from messages by the HTTP layer's query
//! scrubbing, and never logged here.
use rightkit_http::{Client, Method};
use serde_json::{json, Value};

use crate::http::{self, Extra, Reply};
use crate::Fail;

/// The platform name this module drives.
pub const PLATFORM: &str = "youtube";
/// The session request path under the base URL (unverified, see the module comment).
pub const INIT_PATH: &str = "/upload/youtube/v3/videos";
/// The session request query (unverified).
const INIT_QUERY: &str = "uploadType=resumable&part=snippet,status";
/// The read-back path under the base URL (unverified).
pub const LIST_PATH: &str = "/youtube/v3/videos";
/// The parts asked for on the read-back.
const READ_PARTS: &str = "id,status,fileDetails";
/// The size of one chunk. A multiple of 256 KiB (the recalled granularity; unverified).
pub const CHUNK_SIZE: usize = 262_144;
/// The largest video the engine uploads: the HTTP client's own response cap bounds the fetch.
pub const MAX_VIDEO_BYTES: usize = 32 * 1024 * 1024;
/// Longest title, in characters (unverified).
pub const TITLE_LIMIT: usize = 100;
/// Longest description, in bytes (unverified).
pub const DESCRIPTION_LIMIT: usize = 5_000;
/// How many times one upload step probes and resumes after a lost chunk answer.
pub const MAX_RESUMES: u32 = 3;
/// What `settings.privacy_status` is when the job does not say.
pub const DEFAULT_PRIVACY: &str = "private";
/// The settings the client accepts.
const SETTING_KEYS: [&str; 3] = ["privacy_status", "title", "made_for_kids"];
/// The permalink prefix (unverified, see the module comment).
const PERMALINK_PREFIX: &str = "https://www.youtube.com/watch?v=";
/// Longest video id accepted. A real id is 11 characters.
const MAX_ID: usize = 64;
/// Longest session path kept.
const MAX_SESSION_PATH: usize = 2_048;

/// What a job uploads, validated and ready to serialise.
pub struct Prepared {
    title: String,
    description: String,
    privacy: &'static str,
    made_for_kids: Option<bool>,
    url: String,
}

impl Prepared {
    /// The `videos.insert` body that starts the session.
    pub fn init_body(&self) -> Value {
        let mut status = json!({ "privacyStatus": self.privacy });
        if let Some(kids) = self.made_for_kids {
            status["selfDeclaredMadeForKids"] = json!(kids);
        }
        json!({
            "snippet": { "title": self.title, "description": self.description },
            "status": status
        })
    }
}

/// True for a plain video id: 1 to `MAX_ID` characters of `A-Z a-z 0-9 - _`.
fn plain_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The privacy a job asks for: its valid `settings.privacy_status`, else [`DEFAULT_PRIVACY`].
pub fn privacy_of(settings: &Value) -> &str {
    match settings.get("privacy_status").and_then(Value::as_str) {
        Some(value @ ("private" | "unlisted" | "public")) => value,
        _ => DEFAULT_PRIVACY,
    }
}

/// The one video of a payload and its URL. Refused: no media, more than one item, an item that is not a
/// `video`, an item with no URL or an unusable one.
fn checked_video(payload: &crate::wire::PublishPayload) -> Result<String, Fail> {
    let item = match payload.media.as_slice() {
        [] => {
            return Err((
                "media_required",
                "a youtube upload needs one video: the entry has no media".to_string(),
            ))
        }
        [item] => item,
        many => {
            return Err((
                "media_unsupported",
                format!(
                    "a youtube upload carries exactly one video; the entry has {}",
                    many.len()
                ),
            ))
        }
    };
    if item.kind != "video" {
        return Err((
            "media_unsupported",
            format!(
                "a youtube upload is a video; media item {} is a {}",
                item.hash, item.kind
            ),
        ));
    }
    match item.url.as_deref() {
        None => Err((
            "media_required",
            format!(
                "the youtube media item {} has no URL: pass media_urls for its hash to publish.enqueue",
                item.hash
            ),
        )),
        Some(url) if !crate::media::plain_url(url) => Err((
            "media_url_invalid",
            format!(
                "the media URL of item {} must be an http:// or https:// URL with a host, no user info and no spaces",
                item.hash
            ),
        )),
        Some(url) => Ok(url.to_string()),
    }
}

/// Validate a payload and build the upload. Refused before any request: media that is not one video with a URL,
/// a setting the client does not know (or one with a bad value), an empty title, a title over [`TITLE_LIMIT`]
/// characters, a description over [`DESCRIPTION_LIMIT`] bytes, and `<` or `>` in either.
pub fn prepare(payload: &crate::wire::PublishPayload) -> Result<Prepared, Fail> {
    let url = checked_video(payload)?;
    let empty = serde_json::Map::new();
    let settings = match &payload.settings {
        Value::Null => &empty,
        Value::Object(map) => map,
        _ => {
            return Err((
                "bad_input",
                "the youtube settings must be an object".to_string(),
            ))
        }
    };
    for key in settings.keys() {
        if !SETTING_KEYS.contains(&key.as_str()) {
            return Err((
                "bad_input",
                format!(
                    "the youtube client has no setting {key}; it accepts {}",
                    SETTING_KEYS.join(", ")
                ),
            ));
        }
    }
    let privacy: &'static str = match settings.get("privacy_status") {
        None | Some(Value::Null) => DEFAULT_PRIVACY,
        Some(Value::String(text)) => match text.as_str() {
            "private" => "private",
            "unlisted" => "unlisted",
            "public" => "public",
            other => {
                return Err((
                    "bad_input",
                    format!(
                    "settings.privacy_status must be private, unlisted or public, got {other:?}"
                ),
                ))
            }
        },
        Some(_) => {
            return Err((
                "bad_input",
                "settings.privacy_status must be private, unlisted or public".to_string(),
            ))
        }
    };
    let made_for_kids = match settings.get("made_for_kids") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(flag)) => Some(*flag),
        Some(_) => {
            return Err((
                "bad_input",
                "settings.made_for_kids must be true or false".to_string(),
            ))
        }
    };
    let title = match settings.get("title") {
        None | Some(Value::Null) => payload
            .caption
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("")
            .to_string(),
        Some(Value::String(text)) => text.trim().to_string(),
        Some(_) => return Err(("bad_input", "settings.title must be text".to_string())),
    };
    if title.is_empty() {
        return Err((
            "caption_empty",
            "the youtube title is empty: give the entry a caption or settings.title".to_string(),
        ));
    }
    let count = title.chars().count();
    if count > TITLE_LIMIT {
        return Err((
            "text_too_long",
            format!(
                "the youtube title counts {count} characters and the client allows {TITLE_LIMIT}"
            ),
        ));
    }
    let description = payload.caption.trim_end().to_string();
    if description.len() > DESCRIPTION_LIMIT {
        return Err((
            "text_too_long",
            format!(
                "the youtube description is {} bytes and the client allows {DESCRIPTION_LIMIT}",
                description.len()
            ),
        ));
    }
    if title.contains(['<', '>']) || description.contains(['<', '>']) {
        return Err((
            "bad_input",
            "a youtube title or description cannot contain < or >".to_string(),
        ));
    }
    Ok(Prepared {
        title,
        description,
        privacy,
        made_for_kids,
        url,
    })
}

/// `publish.enqueue` check: the payload must be one this module can upload.
pub fn check(payload: &crate::wire::PublishPayload) -> Result<(), Fail> {
    prepare(payload).map(|_| ())
}

/// The media type of a video by its magic bytes: MP4 (`ftyp` at byte 4), QuickTime (`ftyp` brand `qt  `) or
/// WebM (an EBML header). The type the media host claims is never trusted.
pub fn sniff_video(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        if &bytes[8..12] == b"qt  " {
            Some("video/quicktime")
        } else {
            Some("video/mp4")
        }
    } else if bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        Some("video/webm")
    } else {
        None
    }
}

/// The id in a finished upload's answer: the video resource's `id`, when it is a plain id (and `kind`, when
/// present, says `youtube#video`). Anything else is not accepted as this job's video.
pub fn created_video_id(body: &Value) -> Option<String> {
    if let Some(kind) = body.get("kind").and_then(Value::as_str) {
        if kind != "youtube#video" {
            return None;
        }
    }
    let id = body.get("id")?.as_str()?;
    plain_id(id).then(|| id.to_string())
}

/// The path (with its query) that reads back a stored video id. None when the stored value is not a plain id,
/// so nothing odd is ever put in a URL. The token is never part of it.
pub fn read_path(video_id: &str) -> Option<String> {
    plain_id(video_id).then(|| format!("{LIST_PATH}?part={READ_PARTS}&id={video_id}"))
}

/// What a read-back confirmed about a video. `media_type` is read back for the record and the tests; the job
/// stores only the permalink.
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq)]
pub struct Readback {
    pub id: String,
    /// `status.uploadStatus` (`uploaded`, `processed`, `failed`, `rejected`, `deleted`).
    pub upload_status: String,
    /// `status.privacyStatus`.
    pub privacy: String,
    /// The media type read from `fileDetails.container`, when the answer carries it (unverified: the owner
    /// sees it, and perhaps only once the video is processed).
    pub media_type: Option<String>,
}

fn container_mime(container: &str) -> Option<String> {
    match container {
        "mp4" => Some("video/mp4".to_string()),
        "mov" => Some("video/quicktime".to_string()),
        "webm" => Some("video/webm".to_string()),
        _ => None,
    }
}

/// The read-back of `video_id` from a `videos.list` answer: the item whose `id` is the stored id, with its
/// status. None when the list is empty (the video was not found, or is not this account's) or the item has no
/// status.
pub fn readback(video_id: &str, body: &Value) -> Option<Readback> {
    let item = body
        .get("items")?
        .as_array()?
        .iter()
        .find(|item| item.get("id").and_then(Value::as_str) == Some(video_id))?;
    let status = item.get("status")?;
    Some(Readback {
        id: video_id.to_string(),
        upload_status: status.get("uploadStatus")?.as_str()?.to_string(),
        privacy: status.get("privacyStatus")?.as_str()?.to_string(),
        media_type: item
            .get("fileDetails")
            .and_then(|d| d.get("container"))
            .and_then(Value::as_str)
            .and_then(container_mime),
    })
}

/// The permalink, once the read has confirmed the stored video: the id matches, the video's privacy is the one
/// the job asked for, and it has not failed, been rejected or been deleted. The link is built from the id (see
/// the module comment).
pub fn permalink_from(settings: &Value, video_id: Option<&str>, body: &Value) -> Option<String> {
    let id = video_id?;
    let confirmed = readback(id, body)?;
    if confirmed.privacy != privacy_of(settings) {
        return None;
    }
    if matches!(
        confirmed.upload_status.as_str(),
        "failed" | "rejected" | "deleted"
    ) {
        return None;
    }
    Some(format!("{PERMALINK_PREFIX}{id}"))
}

/// True when a 403 body says the daily upload quota is used up. The body is cut at 200 characters by the HTTP
/// layer, and the documented message is long, so the reason (`quotaExceeded`, domain `youtube.quota`) may be cut
/// off; the message's own words are matched too. A quota refusal is final for the day: it is typed
/// `quota_exceeded` and never retried; any other 403 is `permission_denied`.
pub fn is_quota_exceeded(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("quotaexceeded")
        || lower.contains("youtube.quota")
        || (lower.contains("exceeded your") && lower.contains("quota"))
}

/// The session path kept from a `Location` header: what follows the base URL, which must start with `/`. A URI
/// that is not under the base URL (another host, or a sibling path) is not accepted, so the token is never
/// sent anywhere else.
pub fn session_path(base: &str, location: Option<&str>) -> Option<String> {
    let rest = location?.trim().strip_prefix(base)?;
    safe_path(rest).then(|| rest.to_string())
}

fn safe_path(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= MAX_SESSION_PATH
        && !path
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
}

/// The text kept in `remote.container_id` for a session.
fn session_text(path: &str, size: usize) -> String {
    json!({ "v": 1, "path": path, "size": size }).to_string()
}

/// The session path and file size from the stored text.
fn parse_session(text: &str) -> Option<(String, usize)> {
    let value: Value = serde_json::from_str(text).ok()?;
    if value.get("v")?.as_u64()? != 1 {
        return None;
    }
    let path = value.get("path")?.as_str()?;
    if !safe_path(path) {
        return None;
    }
    let size = usize::try_from(value.get("size")?.as_u64()?).ok()?;
    Some((path.to_string(), size))
}

/// The bytes the platform holds, from a `Range` header: `bytes=0-<last>` means `last + 1` bytes. No header, or
/// one that cannot be read, means none (the next chunk is then sent from the start, and a platform that
/// disagrees refuses it).
pub fn received_from_range(header: Option<&str>) -> u64 {
    let Some(text) = header else { return 0 };
    let Some(span) = text.trim().strip_prefix("bytes=") else {
        return 0;
    };
    let Some((first, last)) = span.split_once('-') else {
        return 0;
    };
    if first.trim() != "0" {
        return 0;
    }
    last.trim()
        .parse::<u64>()
        .map_or(0, |n| n.saturating_add(1))
}

/// What the platform answered to a chunk or a probe.
enum Probe {
    /// `200` or `201`: the upload is complete; this is the video resource.
    Done(Value),
    /// `308`: this many bytes are kept.
    Incomplete(u64),
}

/// Classify one answer of the upload protocol. The `308` arrives as `Reply::Ok` with `resume_incomplete` set and
/// the `range` header echoed (see the registration note for `http.rs`).
fn classify(reply: Reply) -> Result<Probe, Reply> {
    match reply {
        Reply::Ok(body) => {
            if body.get("resume_incomplete").and_then(Value::as_bool) == Some(true) {
                let range = body
                    .get("headers")
                    .and_then(|h| h.get("range"))
                    .and_then(Value::as_str);
                Ok(Probe::Incomplete(received_from_range(range)))
            } else {
                Ok(Probe::Done(body))
            }
        }
        other => Err(other),
    }
}

/// `PUT` with `Content-Range: bytes */<total>` and no body: what does the platform hold?
fn probe(client: &Client, url: &str, token: &str, total: usize) -> Result<Probe, Reply> {
    let range = format!("bytes */{total}");
    let headers = [("Content-Range", range.as_str())];
    let reply = http::send_body(
        client,
        Method::Put,
        url,
        token,
        None,
        Some(("application/octet-stream", Vec::new())),
        &Extra {
            headers: &headers,
            echo: &["range"],
        },
    );
    classify(reply)
}

/// True for an answer that leaves it unknown whether a chunk arrived: the connection failed after the request
/// was sent, a timeout, or a 5xx.
fn ambiguous(reply: &Reply) -> bool {
    matches!(
        reply,
        Reply::Transport {
            connect_phase: false,
            ..
        } | Reply::Timeout
            | Reply::ServerError(_)
    )
}

fn stalled(why: &str) -> Reply {
    Reply::Transport {
        connect_phase: false,
        message: why.to_string(),
    }
}

/// An offset reported by the platform, checked against the file: it can neither exceed the file nor be all of
/// it (an incomplete answer with every byte kept would loop).
fn checked_offset(received: u64, total: usize) -> Result<usize, Reply> {
    match usize::try_from(received) {
        Ok(n) if n < total => Ok(n),
        _ => Err(stalled(
            "the platform reports an offset that is not inside the file; the upload was not finished",
        )),
    }
}

/// Drive the upload of `bytes` to the session at `url`, resuming after a lost answer. Returns the final result
/// (the finished video resource, or the reply that stopped the upload) and the number of requests made.
fn upload_chunks(
    client: &Client,
    url: &str,
    token: &str,
    mime: &str,
    bytes: &[u8],
) -> (Result<Value, Reply>, i64) {
    let total = bytes.len();
    let mut calls: i64 = 1;
    let mut offset = match probe(client, url, token, total) {
        Ok(Probe::Done(body)) => return (Ok(body), calls),
        Ok(Probe::Incomplete(received)) => match checked_offset(received, total) {
            Ok(n) => n,
            Err(reply) => return (Err(reply), calls),
        },
        Err(reply) => return (Err(reply), calls),
    };
    let mut resumes: u32 = 0;
    let mut rounds: usize = 0;
    let max_rounds = total / CHUNK_SIZE + 4 + (MAX_RESUMES as usize) * 2;
    loop {
        rounds += 1;
        if rounds > max_rounds {
            return (Err(stalled("the upload made no progress")), calls);
        }
        let end = (offset + CHUNK_SIZE).min(total);
        let range = format!("bytes {}-{}/{}", offset, end - 1, total);
        let headers = [("Content-Range", range.as_str())];
        calls += 1;
        let reply = http::send_body(
            client,
            Method::Put,
            url,
            token,
            None,
            Some((mime, bytes[offset..end].to_vec())),
            &Extra {
                headers: &headers,
                echo: &["range"],
            },
        );
        let answered: Result<u64, Reply> = match classify(reply) {
            Ok(Probe::Done(body)) => return (Ok(body), calls),
            Ok(Probe::Incomplete(received)) => Ok(received),
            Err(reply) if ambiguous(&reply) => Err(reply),
            Err(reply) => return (Err(reply), calls),
        };
        let received = match answered {
            Ok(received) => received,
            Err(reply) => {
                if resumes >= MAX_RESUMES {
                    return (Err(reply), calls);
                }
                resumes += 1;
                calls += 1;
                match probe(client, url, token, total) {
                    Ok(Probe::Done(body)) => return (Ok(body), calls),
                    Ok(Probe::Incomplete(received)) => received,
                    Err(reply) => return (Err(reply), calls),
                }
            }
        };
        offset = match checked_offset(received, total) {
            Ok(n) => n,
            Err(reply) => return (Err(reply), calls),
        };
    }
}

/// What one step of a YouTube job needs from the job.
pub struct Job<'a> {
    pub phase: &'a str,
    pub payload: &'a crate::wire::PublishPayload,
    /// `remote.container_id`: the stored session, when the session step has run.
    pub session: Option<&'a str>,
    /// `remote.post_id`: the video id, once the upload finished.
    pub post_id: Option<&'a str>,
}

/// What a step produced. The variants are the machine's `Sent` variants, so the machine converts one to one.
pub enum Step {
    /// The step's own request was sent; its answer is the phase's outcome.
    Reply { reply: Reply, calls: i64 },
    /// A request that precedes the video's creation failed (the fetch, the session, a chunk). Nothing is
    /// ambiguous: the machine handles it as a read (retried, never `outcome_unknown`).
    Before {
        reply: Reply,
        calls: i64,
        what: &'static str,
    },
    /// The session step succeeded; `media` is the text for `remote.container_id`. The job stays at `create`.
    Uploaded { media: String, calls: i64 },
    /// The step refused locally. Nothing was created.
    Refused {
        code: &'static str,
        message: String,
        calls: i64,
    },
}

fn refused(code: &'static str, message: String, calls: i64) -> Step {
    Step::Refused {
        code,
        message,
        calls,
    }
}

/// The one step of a YouTube job (see the module comment). `base` is the validated base URL; `token` goes only
/// in the `Authorization` header.
pub fn send(job: &Job<'_>, client: &Client, base: &str, token: &str) -> Step {
    if job.phase == "read_permalink" {
        return read(job, client, base, token);
    }
    if job.phase != "create" {
        return refused(
            "internal",
            format!("no youtube request exists for phase {}", job.phase),
            0,
        );
    }
    let prepared = match prepare(job.payload) {
        Ok(prepared) => prepared,
        Err((code, message)) => return refused(code, message, 0),
    };
    let mut calls: i64 = 1;
    let bytes = match http::fetch(client, &prepared.url) {
        Ok(bytes) => bytes,
        Err(reply) => {
            return Step::Before {
                reply,
                calls,
                what: "the video fetch",
            }
        }
    };
    if bytes.is_empty() || bytes.len() > MAX_VIDEO_BYTES {
        return refused(
            "media_too_large",
            format!(
                "the video is {} bytes and the youtube client uploads 1 to {MAX_VIDEO_BYTES}; nothing was uploaded",
                bytes.len()
            ),
            calls,
        );
    }
    let Some(mime) = sniff_video(&bytes) else {
        return refused(
            "media_invalid",
            "the media is not an MP4, QuickTime or WebM video; nothing was uploaded".to_string(),
            calls,
        );
    };
    match job.session {
        None => {
            let url = format!("{base}{INIT_PATH}?{INIT_QUERY}");
            let length = bytes.len().to_string();
            let headers = [
                ("X-Upload-Content-Length", length.as_str()),
                ("X-Upload-Content-Type", mime),
            ];
            let body = prepared.init_body().to_string().into_bytes();
            calls += 1;
            let reply = http::send_body(
                client,
                Method::Post,
                &url,
                token,
                None,
                Some(("application/json; charset=UTF-8", body)),
                &Extra {
                    headers: &headers,
                    echo: &["location"],
                },
            );
            match reply {
                Reply::Ok(answer) => {
                    let location = answer
                        .get("headers")
                        .and_then(|h| h.get("location"))
                        .and_then(Value::as_str);
                    match session_path(base, location) {
                        Some(path) => Step::Uploaded {
                            media: session_text(&path, bytes.len()),
                            calls,
                        },
                        None => refused(
                            "bad_response",
                            "the upload session answer carried no usable session URI under the base URL; nothing was uploaded"
                                .to_string(),
                            calls,
                        ),
                    }
                }
                other => Step::Before {
                    reply: other,
                    calls,
                    what: "the upload session request",
                },
            }
        }
        Some(text) => {
            let Some((path, size)) = parse_session(text) else {
                return refused(
                    "upload_session_invalid",
                    "the stored upload session cannot be read; enqueue the entry again".to_string(),
                    calls,
                );
            };
            if size != bytes.len() {
                return refused(
                    "media_changed",
                    format!(
                        "the video is {} bytes now and the upload session was opened for {size}; nothing more was uploaded",
                        bytes.len()
                    ),
                    calls,
                );
            }
            let url = format!("{base}{path}");
            let (result, used) = upload_chunks(client, &url, token, mime, &bytes);
            let calls = calls + used;
            match result {
                Ok(body) => Step::Reply {
                    reply: Reply::Ok(body),
                    calls,
                },
                Err(Reply::Rejected { status, .. }) if status == 404 || status == 410 => refused(
                    "upload_session_expired",
                    format!(
                        "the platform no longer knows this upload session (HTTP {status}); enqueue the entry again for a new upload"
                    ),
                    calls,
                ),
                Err(reply) => Step::Before {
                    reply,
                    calls,
                    what: "the resumable upload",
                },
            }
        }
    }
}

/// The read-back: `GET <base>/youtube/v3/videos?part=...&id=<id>` for the stored video id.
fn read(job: &Job<'_>, client: &Client, base: &str, token: &str) -> Step {
    let Some(video_id) = job.post_id else {
        return Step::Reply {
            reply: Reply::Invalid("read_permalink has no post id".to_string()),
            calls: 0,
        };
    };
    let Some(path) = read_path(video_id) else {
        return Step::Reply {
            reply: Reply::Invalid("the stored youtube video id is not a plain id".to_string()),
            calls: 0,
        };
    };
    let reply = http::send(
        client,
        Method::Get,
        &format!("{base}{path}"),
        token,
        None,
        None,
    );
    Step::Reply { reply, calls: 1 }
}
