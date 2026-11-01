//! Facebook Page request shapes for the publish core: a text post written in one step and read back.
//!
//! 1. `POST /v<version>/<page-id>/feed` with `message` and a Page access token makes the post and answers
//!    `{ "id": "<page id>_<post id>" }`.
//! 2. `GET /v<version>/<page-post-id>?fields=id,message,permalink_url,created_time` reads the post back.
//!
//! A post with ONE image is `POST /v<version>/<page-id>/photos` with `url`, `caption` and `published=true`, answering
//! `{ "id": "<photo id>", "post_id": "<page id>_<post id>" }`; the post is the `post_id` and is read back as above
//! (#143). Facebook fetches the image itself from a public `https` URL. The `photos` field names are UNVERIFIED.
//! Several photos are refused, not implemented.
//!
//! A post with ONE video is `POST /v<version>/<page-id>/videos` with `file_url` (a public `https` URL Facebook
//! fetches itself), `description` (the caption) and `published=true`, answering `{ "id": "<video id>" }` (#143). The
//! video id is the job's post id and is read back with `GET /v<version>/<video id>?fields=id,description,
//! permalink_url,created_time` (the Video node has `description`, not `message`; a post id holds `_` and a video id
//! does not, which is how [`read_path`] picks the fields). Everything about video is UNVERIFIED: the real video
//! edge is on `graph-video.facebook.com` (this client sends it to the one base URL it is given, so a live run must
//! set that host), the field names `file_url`, `description` and `published`, the answer shape, the Video node's
//! read fields, the file types (the URL extension must be `mp4` or `mov` when it has one; Facebook takes more
//! types) and [`VIDEO_MAX_MS`] (docs/p1/capability-matrix.md: "feed video limits: unverified"). A video this size
//! is uploaded by the URL pull only; the resumable file upload of large files is not implemented.
//!
//! The phase machine is the simple family's (`create -> read_permalink -> done`). This module supplies what is
//! Facebook-specific: the request bodies and paths, the text rules and the answer parsing.
//!
//! No idempotency key. The feed create has none, so a lost answer after it can never be made safe by repeating
//! it. The create therefore carries NO `Idempotency-Key`, an ambiguous answer is `outcome_unknown`, and
//! `publish.reconcile` (machine.rs `reconcile_facebook`) never creates: it may only adopt a post it can READ BY
//! POST ID, and without one it leaves the job `outcome_unknown` for a person.
//!
//! The Page id. The `<page-id>` in the path is the job's stored `remote_account`, validated as a plain id. No
//! text from the caption or the settings reaches a path.
//!
//! The Graph version. `GRAPH_VERSION` below is the string every request names; it must equal the `facebook`
//! entry of the version registry (`api_versions.rs`, sourced and dated, checked by `doctor`); a unit test
//! holds the two together. It has not been sent to the live API.
//!
//! Secrets: the access token is the command's `token`, sent only as the `Authorization: Bearer` header of a
//! request, never in the URL or the query string, and scrubbed from every message by `http::send_with`.
//! Nothing in this file builds a message from the token.
//!
//! Protocol facts used here come from Meta's published Graph API Pages documentation and were not verified
//! against a live server in this task; docs/PUBLISH-CORE.md lists what is left out because it is not known.
use rightkit_http::percent_encode;
use serde_json::{json, Value};

use super::threads::error_code;
use crate::http::{Reply, RetryAfter};
use crate::Fail;

/// The platform name this module drives.
pub const PLATFORM: &str = "facebook";
/// The Graph API version every request names: the registry's `facebook` pin (see the module comment).
pub const GRAPH_VERSION: &str = "v25.0";
/// Longest `message` the engine accepts, counted in UTF-16 code units. It is the development limit the mock
/// fixture (`tests/mock-platform/fixtures/platforms.json`) configures for `facebook`, NOT a sourced Facebook
/// limit: the real limit is unverified and this constant must be replaced by a sourced one before live use.
pub const TEXT_LIMIT: usize = 200;
/// The fields asked for on the read-back of a post.
const READ_FIELDS: &str = "id,message,permalink_url,created_time";
/// The fields asked for on the read-back of a video (the Video node: `description`, UNVERIFIED).
const VIDEO_READ_FIELDS: &str = "id,description,permalink_url,created_time";
/// Longest video, in milliseconds: 240 minutes. UNVERIFIED (recalled; no fetched source states it) and a
/// development ceiling like [`TEXT_LIMIT`], to be replaced by a sourced value before live use.
pub const VIDEO_MAX_MS: u64 = 240 * 60 * 1_000;
/// Longest page or post id accepted.
const MAX_ID: usize = 64;
/// Longest permalink accepted from an answer.
const MAX_PERMALINK: usize = 512;
/// OAuth error code for an expired or invalid access token.
const TOKEN_CODE: i64 = 190;
/// Error codes of the documented throttling family: treated as rate limited like an HTTP 429.
const THROTTLE_CODES: [i64; 4] = [4, 17, 32, 613];
/// Error codes for a missing permission: typed and never retried.
const PERMISSION_CODES: [i64; 2] = [10, 200];

/// What a job posts, validated and ready to serialise.
pub struct Prepared {
    page: String,
    message: String,
    /// The public `https` URL of the one photo, when this is a photo post (Facebook fetches it itself).
    photo: Option<String>,
    /// The public `https` URL of the one video, when this is a video post (Facebook fetches it itself).
    video: Option<String>,
}

impl Prepared {
    /// `POST /v<version>/<page>/feed` for text, `POST /v<version>/<page>/photos` for a photo.
    pub fn create_path(&self) -> String {
        let edge = if self.video.is_some() {
            "videos"
        } else if self.photo.is_some() {
            "photos"
        } else {
            "feed"
        };
        format!("/{GRAPH_VERSION}/{}/{edge}", percent_encode(&self.page))
    }

    /// The create body. A photo post sends `url`, `caption` and `published: true` (the `/photos` edge of a
    /// Page publishes to the Page's feed when `published` is true). UNVERIFIED against a live server: the
    /// parameter names `url`, `caption` and `published` are recalled from Meta's Page photos reference, which
    /// no fetched source in docs/p1/capability-matrix.md states ("Facebook Page media specs: unverified").
    pub fn create_body(&self) -> Value {
        if let Some(url) = &self.video {
            // UNVERIFIED field names (see the module comment).
            return json!({ "file_url": url, "description": self.message, "published": true });
        }
        match &self.photo {
            Some(url) => json!({ "url": url, "caption": self.message, "published": true }),
            None => json!({ "message": self.message }),
        }
    }
}

/// True for a plain id: letters, digits, `-` and `_`, 1 to 64 of them.
fn plain_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The characters counted for `message`: UTF-16 code units (see [`TEXT_LIMIT`]).
pub fn counted_length(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Validate a payload and build the post. Refused before any request: an account that is not a plain id (it
/// becomes a path segment), media other than one image with a public `https` URL (see `media_of`; dropping
/// media silently would post the wrong thing), any setting (none is used, and ignoring one silently would post the wrong thing), an empty text
/// and a text over [`TEXT_LIMIT`].
pub fn prepare(
    payload: &crate::wire::PublishPayload,
    remote_account: &str,
) -> Result<Prepared, Fail> {
    if !plain_id(remote_account) {
        return Err((
            "bad_input",
            format!(
                "remote_account must be a Facebook Page id (letters, digits, '-' or '_', at most {MAX_ID}) to become the request path"
            ),
        ));
    }
    let (photo, video) = media_of(payload)?;
    let unused = match &payload.settings {
        Value::Null => false,
        Value::Object(map) => !map.is_empty(),
        _ => true,
    };
    if unused {
        return Err((
            "bad_input",
            "the facebook client uses no settings; remove them from the job".to_string(),
        ));
    }
    let message = payload.caption.clone();
    if message.trim().is_empty() {
        return Err((
            "caption_empty",
            "the facebook post text is empty".to_string(),
        ));
    }
    let count = counted_length(&message);
    if count > TEXT_LIMIT {
        return Err((
            "text_too_long",
            format!("the post text counts {count} characters and the facebook client allows {TEXT_LIMIT}"),
        ));
    }
    Ok(Prepared {
        page: remote_account.to_string(),
        message,
        photo,
        video,
    })
}

/// The lowercase extension of the last path segment of an https URL, None when it has none.
fn url_extension(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path = rest.split(['?', '#']).next().unwrap_or("");
    let mut segments = path.split('/');
    segments.next();
    let (_, ext) = segments.last()?.rsplit_once('.')?;
    (!ext.is_empty()).then(|| ext.to_ascii_lowercase())
}

/// True when the payload posts one video (the create then uses the video edge and its answer is a video id).
pub fn has_video(payload: &crate::wire::PublishPayload) -> bool {
    payload.media.iter().any(|item| item.kind == "video")
}

/// The one photo URL or the one video URL of a job (`(None, None)` for a text post), or the typed refusal.
/// Facebook fetches the file itself from a public `https` URL (the engine never fetches or uploads it, so no byte
/// of it passes here). Refused: more than one item (several photos need one unpublished upload each and
/// `attached_media`, not implemented), any other kind than `image` or `video`, an item with no URL, a URL that is
/// not a plain public `https` URL, a video longer than [`VIDEO_MAX_MS`] when the entry states `duration_ms`, and
/// a video URL whose extension is not `mp4` or `mov`.
fn media_of(
    payload: &crate::wire::PublishPayload,
) -> Result<(Option<String>, Option<String>), Fail> {
    let [item] = payload.media.as_slice() else {
        return if payload.media.is_empty() {
            Ok((None, None))
        } else {
            Err((
                "media_unsupported",
                "a facebook post carries one photo or one video; several items are not implemented, keep one".to_string(),
            ))
        };
    };
    let video = match item.kind.as_str() {
        "image" => false,
        "video" => true,
        other => {
            return Err((
                "media_unsupported",
                format!("the facebook client posts one image or one video, not {other:?}"),
            ))
        }
    };
    let Some(url) = item.url.as_deref().filter(|u| !u.trim().is_empty()) else {
        return Err((
            "media_required",
            "the facebook media item has no URL: pass media_urls for its hash to publish.enqueue"
                .to_string(),
        ));
    };
    super::instagram::check_media_url(url)
        .map_err(|message| ("media_url_invalid", format!("the facebook {message}")))?;
    if video {
        if let Some(ms) = item.duration_ms.filter(|ms| *ms > VIDEO_MAX_MS) {
            return Err((
                "media_too_long",
                format!(
                    "the facebook video {} lasts {ms} ms and the facebook client takes at most {VIDEO_MAX_MS} ms (unverified ceiling); nothing was posted",
                    item.hash
                ),
            ));
        }
        if let Some(ext) = url_extension(url).filter(|e| e != "mp4" && e != "mov") {
            return Err((
                "media_url_invalid",
                format!(
                    "the facebook video URL of item {} ends in .{ext}; the facebook client takes mp4 or mov",
                    item.hash
                ),
            ));
        }
        Ok((None, Some(url.to_string())))
    } else {
        Ok((Some(url.to_string()), None))
    }
}

/// `publish.enqueue` check: the payload and account must be ones this module can post.
pub fn check(payload: &crate::wire::PublishPayload, remote_account: &str) -> Result<(), Fail> {
    prepare(payload, remote_account).map(|_| ())
}

/// The id in a create answer: a plain string id of the form `<page id>_<post id>` whose page part is the
/// Page this job posted to. A photo create answers `{ "id": "<photo id>", "post_id": "<page id>_<post id>" }`
/// (field names UNVERIFIED, recalled from Meta's reference): the post is the `post_id`, never the photo's own
/// `id`, because the read-back and the permalink are of the post. A video create (`video` true) answers
/// `{ "id": "<video id>" }` and the video id is the post id (UNVERIFIED); it is accepted only when it is a plain
/// id holding no `_`, so a post id is never taken for a video. Anything else is not accepted as this job's post.
pub fn created_post_id(page: &str, body: &Value, video: bool) -> Option<String> {
    if video {
        let id = body.get("id")?.as_str()?;
        return (plain_id(id) && !id.contains('_')).then(|| id.to_string());
    }
    let id = body.get("post_id").or_else(|| body.get("id"))?.as_str()?;
    let post_part = id.strip_prefix(page)?.strip_prefix('_')?;
    (plain_id(id) && !post_part.is_empty()).then(|| id.to_string())
}

/// The `GET /v<version>/<post-id>?fields=...` path that reads back a stored post id. None when the stored
/// value is not a plain id, so nothing odd is ever put in a path. The token is never part of it.
pub fn read_path(post_id: &str) -> Option<String> {
    // A post id is `<page>_<post>`; a video id (#143) has no `_` and is read as a Video node.
    let fields = if post_id.contains('_') {
        READ_FIELDS
    } else {
        VIDEO_READ_FIELDS
    };
    plain_id(post_id).then(|| {
        format!(
            "/{GRAPH_VERSION}/{}?fields={fields}",
            percent_encode(post_id)
        )
    })
}

/// The read-back `message`, once the read has confirmed the stored post: the answer's `id` must be the stored
/// post id.
pub fn confirmed_message<'a>(post_id: &str, body: &'a Value) -> Option<&'a str> {
    if body.get("id").and_then(Value::as_str) != Some(post_id) {
        return None;
    }
    // A video node answers `description`, a post `message` (UNVERIFIED for the video node).
    body.get("message")
        .or_else(|| body.get("description"))
        .and_then(Value::as_str)
}

/// The permalink, once the read has confirmed the stored post: the answer's `id` must be the stored post id
/// and its `permalink_url` a plain `https://` URL.
pub fn permalink_from(post_id: Option<&str>, body: &Value) -> Option<String> {
    let id = post_id?;
    if !plain_id(id) || body.get("id").and_then(Value::as_str) != Some(id) {
        return None;
    }
    let link = body.get("permalink_url")?.as_str()?;
    let plain = link.len() <= MAX_PERMALINK
        && link.starts_with("https://")
        && !link.chars().any(|c| c.is_whitespace() || c.is_control());
    plain.then(|| link.to_string())
}

/// True when a rejected reply's body names a missing-permission error (`error.code` 10 or 200). The request
/// was understood and refused, so repeating it cannot help.
pub fn is_permission_error(message: &str) -> bool {
    error_code(message).is_some_and(|code| PERMISSION_CODES.contains(&code))
}

/// A Graph error can arrive as a 4xx body instead of the bare status: code 190 is an expired or invalid
/// token (the existing `token_expired` outcome), and codes 4, 17, 32 and 613 are the throttling family (a
/// 429). A rate limit names no wait here, so the machine's default backoff applies. Everything else is left
/// as it came (a missing permission is typed by the machine through [`is_permission_error`]).
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::PublishPayload;

    #[test]
    fn the_graph_version_is_the_registry_pin() {
        let pinned = crate::api_versions::pinned(PLATFORM).expect("facebook is pinned");
        assert_eq!(GRAPH_VERSION, pinned.version);
    }

    fn payload(caption: &str, settings: Value) -> PublishPayload {
        PublishPayload {
            caption: caption.to_string(),
            format: "post".to_string(),
            media: Vec::new(),
            settings,
        }
    }

    const ACCOUNT: &str = "acct-alpha-facebook-0001";

    #[test]
    fn the_requests_are_the_documented_feed_post() {
        let prepared = prepare(&payload("Hello Facebook", json!({})), ACCOUNT).expect("prepared");
        assert_eq!(
            prepared.create_path(),
            format!("/{GRAPH_VERSION}/acct-alpha-facebook-0001/feed")
        );
        assert_eq!(
            prepared.create_body(),
            json!({ "message": "Hello Facebook" })
        );
        // A null settings value (a payload read before its JSON is restored) is accepted.
        assert!(prepare(&payload("x", Value::Null), ACCOUNT).is_ok());
    }

    #[test]
    fn the_page_id_comes_only_from_the_stored_account() {
        let prepared = prepare(&payload("text", json!({})), "1784-abc_X").expect("prepared");
        assert_eq!(
            prepared.create_path(),
            format!("/{GRAPH_VERSION}/1784-abc_X/feed")
        );
        let too_long = "a".repeat(65);
        for bad in [
            "",
            "a b",
            "a/b",
            "a:b",
            "a.b",
            "a?b",
            "..",
            too_long.as_str(),
        ] {
            assert!(
                matches!(
                    prepare(&payload("text", json!({})), bad),
                    Err(("bad_input", _))
                ),
                "{bad:?} should be refused"
            );
        }
    }

    #[test]
    fn unsupported_or_malformed_input_is_refused_before_any_request() {
        let mut with_media = payload("text", json!({}));
        with_media.media.push(crate::wire::PublishMedia {
            hash: "blake3:abc".to_string(),
            kind: "image".to_string(),
            duration_ms: None,
            url: None,
        });
        // A media item with no URL is refused with its own code.
        assert!(matches!(
            prepare(&with_media, ACCOUNT),
            Err(("media_required", _))
        ));
        for settings in [json!({ "privacy": "friends" }), json!("x"), json!(1)] {
            assert!(
                matches!(
                    prepare(&payload("text", settings), ACCOUNT),
                    Err(("bad_input", _))
                ),
                "should be refused"
            );
        }
        assert!(matches!(
            prepare(&payload("  \n ", json!({})), ACCOUNT),
            Err(("caption_empty", _))
        ));
    }

    fn photo(kind: &str, url: Option<&str>) -> PublishPayload {
        let mut p = payload("A photo", json!({}));
        p.media.push(crate::wire::PublishMedia {
            hash: "sha256:aaa111".to_string(),
            kind: kind.to_string(),
            duration_ms: None,
            url: url.map(str::to_string),
        });
        p
    }

    #[test]
    fn a_photo_post_uses_the_photos_edge() {
        let url = "https://media.mock.test/alpha/pic.jpg";
        let prepared = prepare(&photo("image", Some(url)), ACCOUNT).expect("prepared");
        assert_eq!(
            prepared.create_path(),
            format!("/{GRAPH_VERSION}/acct-alpha-facebook-0001/photos")
        );
        assert_eq!(
            prepared.create_body(),
            json!({ "url": url, "caption": "A photo", "published": true })
        );
        // The post is the answer's post_id, not the photo id.
        let page = "acct-alpha-facebook-0001";
        let body = json!({ "id": "ph-000000001", "post_id": "acct-alpha-facebook-0001_000000001" });
        assert_eq!(
            created_post_id(page, &body, false).as_deref(),
            Some("acct-alpha-facebook-0001_000000001")
        );
        assert_eq!(
            created_post_id(page, &json!({ "id": "ph-000000001" }), false),
            None
        );
    }

    #[test]
    fn unusable_photos_are_refused_before_any_request() {
        let url = "https://media.mock.test/alpha/pic.jpg";
        assert!(matches!(
            prepare(&photo("audio", Some(url)), ACCOUNT),
            Err(("media_unsupported", _))
        ));
        for none in [None, Some(""), Some("  ")] {
            assert!(matches!(
                prepare(&photo("image", none), ACCOUNT),
                Err(("media_required", _))
            ));
        }
        for bad in [
            "http://media.mock.test/a.jpg",
            "https://localhost/a.jpg",
            "https://u:p@media.mock.test/a.jpg",
        ] {
            assert!(
                matches!(
                    prepare(&photo("image", Some(bad)), ACCOUNT),
                    Err(("media_url_invalid", _))
                ),
                "{bad:?}"
            );
        }
        let mut two = photo("image", Some(url));
        two.media.push(two.media[0].clone());
        assert!(matches!(
            prepare(&two, ACCOUNT),
            Err(("media_unsupported", _))
        ));
    }

    #[test]
    fn the_limit_counts_utf16_units_and_is_exact() {
        assert!(prepare(&payload(&"a".repeat(TEXT_LIMIT), json!({})), ACCOUNT).is_ok());
        let over = TEXT_LIMIT + 1;
        let Err(err) = prepare(&payload(&"a".repeat(over), json!({})), ACCOUNT) else {
            panic!("{over} characters must be refused");
        };
        assert_eq!(err.0, "text_too_long");
        assert!(err.1.contains(&format!("counts {over} characters")));
        assert!(err.1.contains(&format!("allows {TEXT_LIMIT}")));
        // An astral character is two code units: half the limit of them is exactly the limit.
        assert_eq!(counted_length("\u{1f600}"), 2);
        assert_eq!(counted_length("caf\u{e9}"), 4);
        let emoji = "\u{1f600}".repeat(TEXT_LIMIT / 2);
        assert!(prepare(&payload(&emoji, json!({})), ACCOUNT).is_ok());
        let over_emoji = format!("{emoji}a");
        assert!(matches!(
            prepare(&payload(&over_emoji, json!({})), ACCOUNT),
            Err(("text_too_long", _))
        ));
    }

    #[test]
    fn answers_are_matched_to_the_stored_post() {
        let page = "acct-alpha-facebook-0001";
        let post = "acct-alpha-facebook-0001_000000001";
        assert_eq!(
            created_post_id(page, &json!({ "id": post }), false).as_deref(),
            Some(post)
        );
        assert_eq!(created_post_id(page, &json!({}), false), None);
        assert_eq!(created_post_id(page, &json!({ "id": 12 }), false), None);
        // The page part must be this job's Page, and the post part must not be empty.
        assert_eq!(
            created_post_id(page, &json!({ "id": "other_000000001" }), false),
            None
        );
        assert_eq!(
            created_post_id(page, &json!({ "id": format!("{page}_") }), false),
            None
        );
        assert_eq!(created_post_id(page, &json!({ "id": page }), false), None);
        for bad in ["", "a b", "a/b", "../x", "a?b"] {
            assert_eq!(
                created_post_id(page, &json!({ "id": bad }), false),
                None,
                "{bad:?}"
            );
        }
        assert_eq!(
            created_post_id("a/b", &json!({ "id": "a/b_1" }), false),
            None
        );

        assert_eq!(
            read_path(post).as_deref(),
            Some(
                format!("/{GRAPH_VERSION}/{post}?fields=id,message,permalink_url,created_time")
                    .as_str()
            )
        );
        assert_eq!(read_path("a/b"), None);
        assert_eq!(read_path(""), None);

        let read = json!({
            "id": post,
            "permalink_url": "https://facebook.mock.test/acct-alpha-facebook-0001/posts/000000001",
            "message": "Hello",
            "created_time": "2026-09-21T14:13:20Z"
        });
        assert_eq!(
            permalink_from(Some(post), &read).as_deref(),
            Some("https://facebook.mock.test/acct-alpha-facebook-0001/posts/000000001")
        );
        assert_eq!(
            permalink_from(Some("acct-alpha-facebook-0001_000000009"), &read),
            None
        );
        assert_eq!(permalink_from(None, &read), None);
        assert_eq!(permalink_from(Some(post), &json!({ "id": post })), None);
        for bad in [
            "http://x.test/p",
            "javascript:alert(1)",
            "https://x.test/a b",
            "",
        ] {
            let body = json!({ "id": post, "permalink_url": bad });
            assert_eq!(permalink_from(Some(post), &body), None, "{bad:?}");
        }
        assert_eq!(confirmed_message(post, &read), Some("Hello"));
        assert_eq!(
            confirmed_message("acct-alpha-facebook-0001_000000009", &read),
            None
        );
    }

    #[test]
    fn token_throttle_and_permission_codes_are_told_apart() {
        let rejected = |status: u16, code: i64| Reply::Rejected {
            status,
            message: format!("{{\"error\":{{\"message\":\"m\",\"code\":{code}}}}}"),
        };
        for status in [400, 401, 403] {
            assert!(matches!(
                normalize(rejected(status, 190)),
                Reply::Unauthorized
            ));
        }
        for code in [4, 17, 32, 613] {
            assert!(
                matches!(
                    normalize(rejected(400, code)),
                    Reply::RateLimited(RetryAfter::Unspecified)
                ),
                "code {code}"
            );
        }
        // Other codes, and codes that merely start with a throttle digit, are left alone.
        for code in [1, 10, 100, 200, 190_000, 400, 6130] {
            assert!(
                matches!(normalize(rejected(400, code)), Reply::Rejected { .. }),
                "code {code}"
            );
        }
        assert!(matches!(normalize(Reply::Timeout), Reply::Timeout));
        assert!(matches!(
            normalize(Reply::Ok(json!({ "id": "x" }))),
            Reply::Ok(_)
        ));

        let body = |code: i64| format!("{{\"error\":{{\"message\":\"m\",\"code\":{code}}}}}");
        assert!(is_permission_error(&body(10)));
        assert!(is_permission_error(&body(200)));
        for code in [1, 100, 190, 20, 2000] {
            assert!(!is_permission_error(&body(code)), "code {code}");
        }
        assert!(!is_permission_error("not json at all"));
        assert!(!is_permission_error("{\"error\":{\"code\":\"200\"}}"));
    }

    #[test]
    fn a_video_post_uses_the_videos_edge_and_is_read_as_a_video() {
        let url = "https://media.mock.test/alpha/clip.mp4";
        let prepared = prepare(&photo("video", Some(url)), ACCOUNT).expect("prepared");
        assert_eq!(
            prepared.create_path(),
            format!("/{GRAPH_VERSION}/acct-alpha-facebook-0001/videos")
        );
        assert_eq!(
            prepared.create_body(),
            json!({ "file_url": url, "description": "A photo", "published": true })
        );
        let page = "acct-alpha-facebook-0001";
        assert_eq!(
            created_post_id(page, &json!({ "id": "vid-000000001" }), true).as_deref(),
            Some("vid-000000001")
        );
        // A post id is never taken for a video, nor a video id for a post.
        assert_eq!(
            created_post_id(
                page,
                &json!({ "id": "acct-alpha-facebook-0001_000000001" }),
                true
            ),
            None
        );
        assert_eq!(
            created_post_id(page, &json!({ "id": "vid-000000001" }), false),
            None
        );
        assert!(read_path("vid-000000001")
            .expect("path")
            .contains("fields=id,description,"));
        assert!(read_path("acct-alpha-facebook-0001_000000001")
            .expect("path")
            .contains("fields=id,message,"));
        let video_read = json!({ "id": "vid-000000001", "description": "A photo" });
        assert_eq!(
            confirmed_message("vid-000000001", &video_read),
            Some("A photo")
        );
        let mut long = photo("video", Some(url));
        long.media[0].duration_ms = Some(VIDEO_MAX_MS + 1);
        assert!(matches!(
            prepare(&long, ACCOUNT),
            Err(("media_too_long", _))
        ));
        long.media[0].duration_ms = Some(VIDEO_MAX_MS);
        assert!(prepare(&long, ACCOUNT).is_ok());
        let wrong = photo("video", Some("https://media.mock.test/alpha/clip.png"));
        assert!(matches!(
            prepare(&wrong, ACCOUNT),
            Err(("media_url_invalid", _))
        ));
    }
}
