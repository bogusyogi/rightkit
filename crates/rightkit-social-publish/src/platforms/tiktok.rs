//! TikTok (Content Posting API, Direct Post of a video) request shapes for the publish core.
//!
//! The flow, as the container-family phases of the machine (`create_container`, `poll_status`, then
//! `read_permalink`; TikTok has no separate publish request, so the machine skips `publish`, see the
//! registration note in the lane report):
//!
//! 1. `create_container`. `POST <base>/v2/post/publish/video/init/` with `{post_info: {title, privacy_level},
//!    source_info: {...}}`. Two sources, `settings.source`:
//!    - `FILE_UPLOAD` (default): the video is fetched from its media URL (plain GET, no token), checked (size,
//!      magic bytes), the init announces `video_size`, `chunk_size` (= the size) and `total_chunk_count` 1, and
//!      the answer's `upload_url` receives the whole file in one `PUT` with `Content-Range: bytes 0-<last>/<total>`.
//!      The upload URL must start with the base URL (as the YouTube session does), so the token can never be sent
//!      to another host. The step answers `{id: <publish_id>}` once the file is up; a failed fetch or upload is
//!      `Before` (retried; a retry starts a fresh init, and the abandoned publish id never publishes anything).
//!    - `PULL_FROM_URL`: the init carries `video_url`; nothing is fetched here. UNVERIFIED: the platform
//!      requires the URL's domain or prefix to be verified for the app, which only a real run shows.
//!    The publish id is stored in `remote.container_id`.
//! 2. `poll_status`. `POST <base>/v2/post/publish/status/fetch/` with `{publish_id}`. The answer is normalised
//!    to the container shape the machine reads: `{id, status_code, tiktok_status, fail_reason, post_id}` with
//!    `status_code` `FINISHED` (`PUBLISH_COMPLETE`), `ERROR` (`FAILED`) or `IN_PROGRESS` (everything else).
//! 3. `read_permalink`. `POST <base>/v2/video/query/?fields=id,title,share_url,privacy_level` with
//!    `{filters: {video_ids: [<id>]}}`. The video must be the stored id and its `privacy_level` must equal the
//!    privacy the job asked for, so a post that is not private is never reported as published with a link.
//!
//! Privacy. SOURCED (docs/p1/capability-matrix.md row TikTok, PACK 3.3): an unaudited client can post only with
//! `SELF_ONLY`, and the platform refuses another level with 403
//! `unaudited_client_can_only_post_to_private_accounts`. `settings.privacy_level` is therefore `SELF_ONLY`
//! (the default); any other level is refused at enqueue (`privacy_unaudited`) unless `settings.audited_client` is
//! `true`, the flag an operator sets once the Content Posting audit has passed. Audited public posting is
//! itself unverified (matrix: "Audited public posting: unverified"). Other settings: `title` (else the caption)
//! and `source`. Any other setting is refused, as ignoring one silently would post the wrong thing.
//!
//! UNVERIFIED (recalled, no fetched source; the matrix says TikTok's docs were unreachable): every path and
//! its trailing slash, the `post_info` and `source_info` field names, the answer shape `{data, error: {code}}`
//! with `ok`, the four `privacy_level` names, the 2,200-character title limit, the status names, the
//! (misspelled) `publicaly_available_post_id` array, a post id being present for a SELF_ONLY post at all, the
//! video query and its `privacy_level` field, and the permalink form. The machine fails a finished post whose
//! status answer carries no post id (`bad_response`), so a real run shows at once whether the platform
//! answers one for a private post.
//!
//! Limits of this slice. The file is held in memory and fetched with the client's response cap (32 MiB), sent as
//! one chunk; video media is not yet served from R2 (docs/p1 capability matrix, blocker: R2 media upload
//! access).
//!
//! Secrets: the access token is the command's `token`, sent only as the `Authorization: Bearer` header, never in
//! a URL, and scrubbed from every message by `http::send_body`.
use rightkit_http::{Client, Method};
use serde_json::{json, Value};

use crate::http::{self, Extra, Reply};
use crate::Fail;

/// The platform name this module drives.
pub const PLATFORM: &str = "tiktok";
/// The init path under the base URL (unverified).
pub const INIT_PATH: &str = "/v2/post/publish/video/init/";
/// The status path under the base URL (unverified).
pub const STATUS_PATH: &str = "/v2/post/publish/status/fetch/";
/// The read-back path under the base URL (unverified).
pub const QUERY_PATH: &str = "/v2/video/query/?fields=id,title,share_url,privacy_level";
/// The largest video the engine uploads: the HTTP client's own response cap bounds the fetch.
pub const MAX_VIDEO_BYTES: usize = 32 * 1024 * 1024;
/// Longest title, in characters (unverified).
pub const TITLE_LIMIT: usize = 2_200;
/// The privacy a job gets when it does not say, and the only one an unaudited client may use (sourced).
pub const DEFAULT_PRIVACY: &str = "SELF_ONLY";
/// The settings the client accepts.
const SETTING_KEYS: [&str; 4] = ["privacy_level", "title", "source", "audited_client"];
/// The permalink prefix (unverified).
const PERMALINK_PREFIX: &str = "https://www.tiktok.com/video/";
const MAX_ID: usize = 128;
const MAX_UPLOAD_PATH: usize = 2_048;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    FileUpload,
    PullFromUrl,
}

/// What a job posts, validated and ready to serialise.
pub struct Prepared {
    title: String,
    privacy: &'static str,
    source: Source,
    url: String,
}

impl Prepared {
    /// The init body. `size` is the file size, used by FILE_UPLOAD (one chunk).
    fn init_body(&self, size: usize) -> Value {
        let source_info = match self.source {
            Source::FileUpload => json!({
                "source": "FILE_UPLOAD",
                "video_size": size,
                "chunk_size": size,
                "total_chunk_count": 1
            }),
            Source::PullFromUrl => json!({ "source": "PULL_FROM_URL", "video_url": self.url }),
        };
        json!({
            "post_info": { "title": self.title, "privacy_level": self.privacy },
            "source_info": source_info
        })
    }
}

fn level_name(text: &str) -> Option<&'static str> {
    match text {
        "SELF_ONLY" => Some("SELF_ONLY"),
        "MUTUAL_FOLLOW_FRIENDS" => Some("MUTUAL_FOLLOW_FRIENDS"),
        "FOLLOWER_OF_CREATOR" => Some("FOLLOWER_OF_CREATOR"),
        "PUBLIC_TO_EVERYONE" => Some("PUBLIC_TO_EVERYONE"),
        _ => None,
    }
}

/// The privacy a job asks for: its valid `settings.privacy_level`, else [`DEFAULT_PRIVACY`].
pub fn privacy_of(settings: &Value) -> &'static str {
    settings
        .get("privacy_level")
        .and_then(Value::as_str)
        .and_then(level_name)
        .unwrap_or(DEFAULT_PRIVACY)
}

/// True for a plain id: 1 to `MAX_ID` characters of `A-Z a-z 0-9 _ - ~ .`.
fn plain_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'~' | b'.'))
}

/// The one video of a payload and its URL. Refused: no media, more than one item, an item that is not a
/// `video`, an item with no URL or an unusable one.
fn checked_video(payload: &crate::wire::PublishPayload) -> Result<String, Fail> {
    let item = match payload.media.as_slice() {
        [] => {
            return Err((
                "media_required",
                "a tiktok post needs one video: the entry has no media".to_string(),
            ))
        }
        [item] => item,
        many => {
            return Err((
                "media_unsupported",
                format!(
                    "a tiktok post carries exactly one video; the entry has {}",
                    many.len()
                ),
            ))
        }
    };
    if item.kind != "video" {
        return Err((
            "media_unsupported",
            format!(
                "a tiktok post is a video; media item {} is a {}",
                item.hash, item.kind
            ),
        ));
    }
    match item.url.as_deref() {
        None => Err((
            "media_required",
            format!(
                "the tiktok media item {} has no URL: pass media_urls for its hash to publish.enqueue",
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

/// Validate a payload and build the post. Refused before any request: media that is not one video with a URL,
/// a setting the client does not know (or one with a bad value), a privacy other than SELF_ONLY while
/// `settings.audited_client` is not true, an empty title and a title over [`TITLE_LIMIT`] characters.
pub fn prepare(payload: &crate::wire::PublishPayload) -> Result<Prepared, Fail> {
    let url = checked_video(payload)?;
    let empty = serde_json::Map::new();
    let settings = match &payload.settings {
        Value::Null => &empty,
        Value::Object(map) => map,
        _ => {
            return Err((
                "bad_input",
                "the tiktok settings must be an object".to_string(),
            ))
        }
    };
    for key in settings.keys() {
        if !SETTING_KEYS.contains(&key.as_str()) {
            return Err((
                "bad_input",
                format!(
                    "the tiktok client has no setting {key}; it accepts {}",
                    SETTING_KEYS.join(", ")
                ),
            ));
        }
    }
    let audited = match settings.get("audited_client") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(_) => {
            return Err((
                "bad_input",
                "settings.audited_client must be true or false".to_string(),
            ))
        }
    };
    let privacy: &'static str = match settings.get("privacy_level") {
        None | Some(Value::Null) => DEFAULT_PRIVACY,
        Some(Value::String(text)) => match level_name(text.as_str()) {
            Some(level) => level,
            None => {
                return Err((
                    "bad_input",
                    format!(
                        "settings.privacy_level must be SELF_ONLY, MUTUAL_FOLLOW_FRIENDS, FOLLOWER_OF_CREATOR or PUBLIC_TO_EVERYONE, got {text:?}"
                    ),
                ))
            }
        },
        Some(_) => {
            return Err((
                "bad_input",
                "settings.privacy_level must be text".to_string(),
            ))
        }
    };
    if privacy != DEFAULT_PRIVACY && !audited {
        return Err((
            "privacy_unaudited",
            format!(
                "privacy {privacy} is refused: an unaudited tiktok client can post only SELF_ONLY. Set settings.audited_client to true only after the Content Posting audit has passed"
            ),
        ));
    }
    let source = match settings.get("source") {
        None | Some(Value::Null) => Source::FileUpload,
        Some(Value::String(text)) => match text.as_str() {
            "FILE_UPLOAD" => Source::FileUpload,
            "PULL_FROM_URL" => Source::PullFromUrl,
            other => {
                return Err((
                    "bad_input",
                    format!("settings.source must be FILE_UPLOAD or PULL_FROM_URL, got {other:?}"),
                ))
            }
        },
        Some(_) => {
            return Err((
                "bad_input",
                "settings.source must be FILE_UPLOAD or PULL_FROM_URL".to_string(),
            ))
        }
    };
    let title = match settings.get("title") {
        None | Some(Value::Null) => payload.caption.trim().to_string(),
        Some(Value::String(text)) => text.trim().to_string(),
        Some(_) => return Err(("bad_input", "settings.title must be text".to_string())),
    };
    if title.is_empty() {
        return Err((
            "caption_empty",
            "the tiktok title is empty: give the entry a caption or settings.title".to_string(),
        ));
    }
    let count = title.chars().count();
    if count > TITLE_LIMIT {
        return Err((
            "text_too_long",
            format!(
                "the tiktok title counts {count} characters and the client allows {TITLE_LIMIT}"
            ),
        ));
    }
    Ok(Prepared {
        title,
        privacy,
        source,
        url,
    })
}

/// `publish.enqueue` check: the payload must be one this module can post.
pub fn check(payload: &crate::wire::PublishPayload) -> Result<(), Fail> {
    prepare(payload).map(|_| ())
}

/// The media type of a video by its magic bytes: MP4, QuickTime or WebM. The type the media host claims is
/// never trusted.
fn sniff_video(bytes: &[u8]) -> Option<&'static str> {
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

/// A refusal carried in an answer that is HTTP 2xx: `error.code` present and not `ok`.
fn body_error(body: &Value) -> Option<Reply> {
    let error = body.get("error")?;
    let code = error.get("code").and_then(Value::as_str)?;
    if code == "ok" || code.is_empty() {
        return None;
    }
    let message = error.get("message").and_then(Value::as_str).unwrap_or("");
    Some(Reply::Rejected {
        status: 400,
        message: format!("tiktok error {code}: {message}"),
    })
}

/// The publish id and the upload URL of an init answer.
fn init_answer(body: &Value) -> Result<(String, Option<String>), Reply> {
    if let Some(reply) = body_error(body) {
        return Err(reply);
    }
    let data = body.get("data");
    let id = data
        .and_then(|d| d.get("publish_id"))
        .and_then(Value::as_str)
        .filter(|id| plain_id(id));
    match id {
        Some(id) => Ok((
            id.to_string(),
            data.and_then(|d| d.get("upload_url"))
                .and_then(Value::as_str)
                .map(str::to_string),
        )),
        None => Err(Reply::Invalid(
            "the tiktok init answer carried no usable publish_id".to_string(),
        )),
    }
}

/// The upload path kept from an `upload_url`: what follows the base URL, which must start with `/`. A URL that
/// is not under the base URL is not accepted, so the token is never sent anywhere else.
pub fn upload_path(base: &str, url: Option<&str>) -> Option<String> {
    let rest = url?.trim().strip_prefix(base)?;
    (rest.starts_with('/')
        && rest.len() <= MAX_UPLOAD_PATH
        && !rest
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\'))
    .then(|| rest.to_string())
}

/// The first post id of a status answer's `publicaly_available_post_id` (or the correctly spelled name), as
/// text; the platform may send numbers (unverified).
fn status_post_id(data: &Value) -> Option<String> {
    ["publicaly_available_post_id", "publicly_available_post_id"]
        .iter()
        .find_map(|name| data.get(*name)?.as_array()?.first().cloned())
        .and_then(|first| match first {
            Value::String(text) => Some(text),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        })
        .filter(|id| plain_id(id))
}

/// A status answer in the container shape the machine reads (see the module comment). A non-2xx or refused
/// answer is passed on unchanged.
pub fn normalize_status(reply: Reply, publish_id: &str) -> Reply {
    match reply {
        Reply::Ok(body) => {
            if let Some(refusal) = body_error(&body) {
                return refusal;
            }
            let data = body.get("data").cloned().unwrap_or(Value::Null);
            let status = data.get("status").and_then(Value::as_str).unwrap_or("");
            let code = match status {
                "PUBLISH_COMPLETE" => "FINISHED",
                "FAILED" => "ERROR",
                _ => "IN_PROGRESS",
            };
            Reply::Ok(json!({
                "id": publish_id,
                "status_code": code,
                "tiktok_status": status,
                "fail_reason": data.get("fail_reason").cloned().unwrap_or(Value::Null),
                "post_id": status_post_id(&data)
            }))
        }
        other => other,
    }
}

/// The post id of a finished, normalised status answer, when it carries a plain one.
pub fn finished_post_id(body: &Value) -> Option<String> {
    body.get("post_id")?
        .as_str()
        .filter(|id| plain_id(id))
        .map(str::to_string)
}

/// The permalink, once the read-back has confirmed the stored video: the id matches and its `privacy_level` is
/// the one the job asked for (a missing level is not a confirmation). Built from the id (unverified form).
pub fn permalink_from(settings: &Value, post_id: Option<&str>, body: &Value) -> Option<String> {
    let id = post_id?;
    if !plain_id(id) || body_error(body).is_some() {
        return None;
    }
    let item = body
        .get("data")?
        .get("videos")?
        .as_array()?
        .iter()
        .find(|item| item.get("id").and_then(Value::as_str) == Some(id))?;
    if item.get("privacy_level").and_then(Value::as_str)? != privacy_of(settings) {
        return None;
    }
    Some(format!("{PERMALINK_PREFIX}{id}"))
}

/// What one step of a TikTok job needs from the job.
pub struct Job<'a> {
    pub phase: &'a str,
    pub payload: &'a crate::wire::PublishPayload,
    /// `remote.container_id`: the publish id, once the init step has run.
    pub publish_id: Option<&'a str>,
    /// `remote.post_id`: the video id, once the status step finished.
    pub post_id: Option<&'a str>,
}

/// What a step produced. The variants are the machine's `Sent` variants, so the machine converts one to one.
pub enum Step {
    /// The step's own request was sent; its answer is the phase's outcome.
    Reply { reply: Reply, calls: i64 },
    /// A request that precedes the post's creation failed (the fetch, the upload). Handled as a read.
    Before {
        reply: Reply,
        calls: i64,
        what: &'static str,
    },
    /// The step refused locally. Nothing was published.
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

/// The one step of a TikTok job (see the module comment). `base` is the validated base URL; `token` goes only
/// in the `Authorization` header.
pub fn send(job: &Job<'_>, client: &Client, base: &str, token: &str) -> Step {
    match job.phase {
        "create_container" => init(job, client, base, token),
        "poll_status" => {
            let Some(id) = job.publish_id.filter(|id| plain_id(id)) else {
                return Step::Reply {
                    reply: Reply::Invalid("poll_status has no usable publish id".to_string()),
                    calls: 0,
                };
            };
            let reply = http::send(
                client,
                Method::Post,
                &format!("{base}{STATUS_PATH}"),
                token,
                None,
                Some(&json!({ "publish_id": id })),
            );
            Step::Reply {
                reply: normalize_status(reply, id),
                calls: 1,
            }
        }
        "read_permalink" => {
            let Some(id) = job.post_id.filter(|id| plain_id(id)) else {
                return Step::Reply {
                    reply: Reply::Invalid("read_permalink has no usable post id".to_string()),
                    calls: 0,
                };
            };
            let reply = http::send(
                client,
                Method::Post,
                &format!("{base}{QUERY_PATH}"),
                token,
                None,
                Some(&json!({ "filters": { "video_ids": [id] } })),
            );
            Step::Reply { reply, calls: 1 }
        }
        other => refused(
            "internal",
            format!("no tiktok request exists for phase {other}"),
            0,
        ),
    }
}

/// `create_container`: the init and, for FILE_UPLOAD, the upload of the file.
fn init(job: &Job<'_>, client: &Client, base: &str, token: &str) -> Step {
    let prepared = match prepare(job.payload) {
        Ok(prepared) => prepared,
        Err((code, message)) => return refused(code, message, 0),
    };
    let init_url = format!("{base}{INIT_PATH}");
    if prepared.source == Source::PullFromUrl {
        let reply = http::send(
            client,
            Method::Post,
            &init_url,
            token,
            None,
            Some(&prepared.init_body(0)),
        );
        let reply = match reply {
            Reply::Ok(body) => match init_answer(&body) {
                Ok((id, _)) => Reply::Ok(json!({ "id": id })),
                Err(reply) => reply,
            },
            other => other,
        };
        return Step::Reply { reply, calls: 1 };
    }
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
                "the video is {} bytes and the tiktok client uploads 1 to {MAX_VIDEO_BYTES}; nothing was uploaded",
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
    calls += 1;
    let reply = http::send(
        client,
        Method::Post,
        &init_url,
        token,
        None,
        Some(&prepared.init_body(bytes.len())),
    );
    let (publish_id, upload_url) = match reply {
        Reply::Ok(body) => match init_answer(&body) {
            Ok(answer) => answer,
            Err(reply) => return Step::Reply { reply, calls },
        },
        other => {
            return Step::Reply {
                reply: other,
                calls,
            }
        }
    };
    let Some(path) = upload_path(base, upload_url.as_deref()) else {
        return refused(
            "bad_response",
            "the tiktok init answer carried no usable upload URL under the base URL; nothing was uploaded"
                .to_string(),
            calls,
        );
    };
    calls += 1;
    let range = format!("bytes 0-{}/{}", bytes.len() - 1, bytes.len());
    let headers = [("Content-Range", range.as_str())];
    let reply = http::send_body(
        client,
        Method::Put,
        &format!("{base}{path}"),
        token,
        None,
        Some((mime, bytes)),
        &Extra {
            headers: &headers,
            echo: &[],
        },
    );
    match reply {
        Reply::Ok(_) => Step::Reply {
            reply: Reply::Ok(json!({ "id": publish_id })),
            calls,
        },
        other => Step::Before {
            reply: other,
            calls,
            what: "the video upload",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{PublishMedia, PublishPayload};

    fn payload(caption: &str, settings: Value) -> PublishPayload {
        PublishPayload {
            caption: caption.to_string(),
            format: "video".to_string(),
            media: vec![PublishMedia {
                hash: "sha256:vid1".to_string(),
                kind: "video".to_string(),
                duration_ms: None,
                url: Some("http://127.0.0.1:4010/media/video-small.mp4".to_string()),
            }],
            settings,
        }
    }

    #[test]
    fn privacy_is_self_only_until_the_audit_flag_is_set() {
        let prepared = prepare(&payload("Hello", json!({}))).expect("prepared");
        assert_eq!(prepared.privacy, "SELF_ONLY");
        assert_eq!(
            prepared.init_body(10),
            json!({
                "post_info": { "title": "Hello", "privacy_level": "SELF_ONLY" },
                "source_info": { "source": "FILE_UPLOAD", "video_size": 10, "chunk_size": 10, "total_chunk_count": 1 }
            })
        );
        for level in [
            "PUBLIC_TO_EVERYONE",
            "MUTUAL_FOLLOW_FRIENDS",
            "FOLLOWER_OF_CREATOR",
        ] {
            assert!(matches!(
                prepare(&payload("Hi", json!({ "privacy_level": level }))),
                Err(("privacy_unaudited", _))
            ));
            assert!(matches!(
                prepare(&payload(
                    "Hi",
                    json!({ "privacy_level": level, "audited_client": false })
                )),
                Err(("privacy_unaudited", _))
            ));
            assert!(prepare(&payload(
                "Hi",
                json!({ "privacy_level": level, "audited_client": true })
            ))
            .is_ok());
        }
        assert!(matches!(
            prepare(&payload("Hi", json!({ "privacy_level": "EVERYONE" }))),
            Err(("bad_input", _))
        ));
        assert_eq!(privacy_of(&json!({})), "SELF_ONLY");
        assert_eq!(
            privacy_of(&json!({ "privacy_level": "PUBLIC_TO_EVERYONE" })),
            "PUBLIC_TO_EVERYONE"
        );
    }

    #[test]
    fn settings_and_media_rules_refuse_before_any_request() {
        assert!(matches!(
            prepare(&payload("  ", json!({}))),
            Err(("caption_empty", _))
        ));
        assert!(matches!(
            prepare(&payload(&"a".repeat(TITLE_LIMIT + 1), json!({}))),
            Err(("text_too_long", _))
        ));
        assert!(matches!(
            prepare(&payload("x", json!({ "duet": true }))),
            Err(("bad_input", _))
        ));
        assert!(matches!(
            prepare(&payload("x", json!({ "source": "UPLOAD" }))),
            Err(("bad_input", _))
        ));
        let pulled = prepare(&payload("x", json!({ "source": "PULL_FROM_URL" }))).expect("pull");
        assert_eq!(
            pulled.init_body(0)["source_info"],
            json!({ "source": "PULL_FROM_URL", "video_url": "http://127.0.0.1:4010/media/video-small.mp4" })
        );
        let mut none = payload("x", json!({}));
        none.media.clear();
        assert!(matches!(prepare(&none), Err(("media_required", _))));
        let mut image = payload("x", json!({}));
        image.media[0].kind = "image".to_string();
        assert!(matches!(prepare(&image), Err(("media_unsupported", _))));
    }

    #[test]
    fn answers_are_normalised_and_matched() {
        let base = "http://127.0.0.1:4010/tiktok/acct-alpha-tiktok-0001";
        let good = format!("{base}/upload?publish_id=v_pub_file~v2.0000000001");
        assert_eq!(
            upload_path(base, Some(good.as_str())).as_deref(),
            Some("/upload?publish_id=v_pub_file~v2.0000000001")
        );
        assert_eq!(upload_path(base, Some("http://evil.example/upload")), None);
        assert_eq!(upload_path(base, None), None);

        let init = json!({ "data": { "publish_id": "v_pub_file~v2.1", "upload_url": "u" }, "error": { "code": "ok" } });
        assert_eq!(
            init_answer(&init).map(|(id, _)| id).ok().as_deref(),
            Some("v_pub_file~v2.1")
        );
        let refused_init = json!({ "error": { "code": "invalid_param", "message": "bad" } });
        assert!(matches!(
            init_answer(&refused_init),
            Err(Reply::Rejected { status: 400, .. })
        ));

        let done = Reply::Ok(json!({
            "data": { "status": "PUBLISH_COMPLETE", "publicaly_available_post_id": ["7000000000000000001"] },
            "error": { "code": "ok" }
        }));
        match normalize_status(done, "p1") {
            Reply::Ok(body) => {
                assert_eq!(body["status_code"], "FINISHED");
                assert_eq!(
                    finished_post_id(&body).as_deref(),
                    Some("7000000000000000001")
                );
            }
            _ => panic!("expected Ok"),
        }
        let failed = Reply::Ok(
            json!({ "data": { "status": "FAILED", "fail_reason": "x" }, "error": { "code": "ok" } }),
        );
        match normalize_status(failed, "p1") {
            Reply::Ok(body) => assert_eq!(body["status_code"], "ERROR"),
            _ => panic!("expected Ok"),
        }
        let busy = Reply::Ok(
            json!({ "data": { "status": "PROCESSING_DOWNLOAD" }, "error": { "code": "ok" } }),
        );
        match normalize_status(busy, "p1") {
            Reply::Ok(body) => assert_eq!(body["status_code"], "IN_PROGRESS"),
            _ => panic!("expected Ok"),
        }

        let read = json!({ "data": { "videos": [{ "id": "7001", "privacy_level": "SELF_ONLY" }] }, "error": { "code": "ok" } });
        assert_eq!(
            permalink_from(&json!({}), Some("7001"), &read).as_deref(),
            Some("https://www.tiktok.com/video/7001")
        );
        // Another privacy than the job asked for, a missing level, another id: no link.
        assert_eq!(
            permalink_from(
                &json!({ "privacy_level": "PUBLIC_TO_EVERYONE" }),
                Some("7001"),
                &read
            ),
            None
        );
        assert_eq!(permalink_from(&json!({}), Some("7002"), &read), None);
        let bare = json!({ "data": { "videos": [{ "id": "7001" }] } });
        assert_eq!(permalink_from(&json!({}), Some("7001"), &bare), None);
        assert_eq!(permalink_from(&json!({}), None, &read), None);
    }
}
