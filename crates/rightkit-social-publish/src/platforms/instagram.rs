//! Instagram request shapes for the publish core: a photo or a reel, written in two steps and read back.
//!
//! 1. `POST /v<version>/<ig-user-id>/media` with `media_type=REELS` and `video_url` (a reel), or `image_url` (a
//!    photo), and `caption` makes a CONTAINER and answers `{ "id": "<container id>" }`.
//! 2. `GET /v<version>/<container-id>?fields=status_code` reads the container until `status_code` is `FINISHED`
//!    (`IN_PROGRESS` while it is processed; `ERROR` and `EXPIRED` are final for that container; `PUBLISHED` means
//!    it was already published).
//! 3. `POST /v<version>/<ig-user-id>/media_publish` with `creation_id` publishes a FINISHED container and
//!    answers `{ "id": "<media id>" }`.
//! 4. `GET /v<version>/<media-id>?fields=id,permalink,caption,timestamp` reads the post back.
//!
//! The phase machine is the container family's (`create_container -> poll_status -> publish -> read_permalink ->
//! done`). This module supplies what is Instagram-specific: the request bodies and paths, the media and caption
//! rules and the answer parsing.
//!
//! No idempotency key. Neither write has one, so neither carries an `Idempotency-Key`.
//!
//! - A lost answer after step 1 can only orphan an unpublished container, which is not a post. `publish.reconcile`
//!   returns such a job to `queued` and the retry makes a NEW container (the shared `create_container` rule in
//!   machine.rs).
//! - A lost answer after step 3 is `outcome_unknown` and the publish is never repeated blind. `publish.reconcile`
//!   (machine.rs `reconcile_instagram`) reads the CONTAINER's `status_code`: `FINISHED` proves the publish did not
//!   happen (a published container reports `PUBLISHED`), so the publish is sent again once; `PUBLISHED` proves a
//!   post exists but the container read does not return the media id, so the job stays for a person
//!   (`publish.resolve_unknown`); anything else proves nothing and the job stays `outcome_unknown`.
//!
//! Carousel (#151). A job with 2 to 10 image items is a carousel: one CHILD container per image
//! (`image_url`, `is_carousel_item=true`, no caption), then a PARENT container (`media_type=CAROUSEL`, `children`,
//! `caption`) which is polled, published and read back exactly like a photo. [`Prepared::child_body`] and
//! [`Prepared::parent_body`] build the bodies; the phase machine (machine.rs) must create the children one step
//! each and the parent last, keeping the child ids until the parent exists. Child and parent field names are
//! recalled from Meta's content-publishing page and are UNVERIFIED against a live server.
//!
//! No media is fetched or uploaded here. The media URL is the job's stored media reference (`payload.media[0].url`,
//! from the enqueue input) and must be a public `https` URL that Instagram fetches itself. Instagram has no
//! text-only post, so a job without a usable media URL is refused before any request.
//!
//! The user id. The `<ig-user-id>` in the path is the job's stored `remote_account`, validated as a plain id. No
//! text from the caption or the settings reaches a path.
//!
//! The Graph version. Instagram's Graph API is versioned with Meta's Graph API, so every request names
//! `GRAPH_VERSION`, which is `facebook::GRAPH_VERSION` (one constant, held to the `facebook` entry of the version
//! registry, `api_versions.rs`, by a test in facebook.rs and another here). It has not been sent to the live API.
//!
//! Secrets: the access token is the command's `token`, sent only as the `Authorization: Bearer` header of a
//! request, never in the URL or the query string, and scrubbed from every message by `http::send_with`. Nothing in
//! this file builds a message from the token.
//!
//! Protocol facts used here come from Meta's published Instagram API (content publishing) documentation and were
//! not verified against a live server in this task; docs/PUBLISH-CORE.md lists what is left out because it is not
//! known.
use rightkit_http::percent_encode;
use serde_json::{json, Value};

use super::facebook::{self, GRAPH_VERSION};
use crate::http::Reply;
use crate::Fail;

/// The platform name this module drives.
pub const PLATFORM: &str = "instagram";
/// Longest caption the engine accepts, counted in UTF-16 code units (the stricter reading of "characters": a code
/// unit count is never below the scalar count). Meta's documentation names 2200 characters.
pub const CAPTION_LIMIT: usize = 2200;
/// Most hashtags one caption may carry. Meta's documentation names 30; the count is taken by [`hashtag_count`].
pub const HASHTAG_LIMIT: usize = 30;
/// How many times one job may be sent back to a fresh start: a new container after the old one expired or after
/// reconcile found no post, or the one repeat of a publish that the container proved was not made. The counter is
/// the job's `reconciles`, which pacing also keys a new container create on.
pub const MAX_RESTARTS: i64 = 2;
/// The fields asked for on the read-back.
const READ_FIELDS: &str = "id,permalink,caption,timestamp";
/// The field asked for on a container status read.
const STATUS_FIELDS: &str = "status_code";
/// Longest user, container or media id accepted.
const MAX_ID: usize = 64;
/// Longest permalink accepted from an answer.
const MAX_PERMALINK: usize = 512;
/// Longest media URL accepted.
const MAX_MEDIA_URL: usize = 2048;

/// Fewest and most images one carousel holds. Most: 10 (docs/p1/capability-matrix.md, "Carousel: up to 10
/// children", RESEARCH section 4). Fewest: 2 is recalled from Meta's content-publishing page and is UNVERIFIED
/// (no fetched source states a minimum).
pub const CAROUSEL_MIN: usize = 2;
/// See [`CAROUSEL_MIN`].
pub const CAROUSEL_MAX: usize = 10;

/// What the post's media is, and where Instagram fetches it from.
enum Media {
    /// A reel: `media_type=REELS` and `video_url`.
    Reel(String),
    /// A photo: `image_url`.
    Photo(String),
    /// A carousel of images: one child container per image (`is_carousel_item=true`, no caption), then a parent
    /// container (`media_type=CAROUSEL`, `children`, `caption`) that is polled and published like any other. A
    /// carousel counts as ONE post against the publish limit (capability-matrix section 10). Images only: a
    /// video child needs its own status poll before the parent, which is not implemented.
    Carousel(Vec<String>),
}

/// What a job posts, validated and ready to serialise.
pub struct Prepared {
    user: String,
    caption: String,
    media: Media,
}

impl Prepared {
    /// `POST /v<version>/<user>/media`.
    pub fn create_path(&self) -> String {
        format!("/{GRAPH_VERSION}/{}/media", percent_encode(&self.user))
    }

    /// `POST /v<version>/<user>/media_publish`.
    pub fn publish_path(&self) -> String {
        format!(
            "/{GRAPH_VERSION}/{}/media_publish",
            percent_encode(&self.user)
        )
    }

    /// The container create body. For a carousel this is only the FIRST child (see [`Prepared::child_body`]); a
    /// carousel must be driven through `child_count`, `child_body` and `parent_body`, because a child is not
    /// publishable on its own (Instagram, and the mock, refuse it).
    pub fn create_body(&self) -> Value {
        match &self.media {
            Media::Reel(url) => {
                json!({ "media_type": "REELS", "video_url": url, "caption": self.caption })
            }
            Media::Photo(url) => json!({ "image_url": url, "caption": self.caption }),
            Media::Carousel(urls) => child_body_of(&urls[0]),
        }
    }

    /// How many child containers a carousel needs; 0 for a reel or a photo.
    #[cfg_attr(not(test), allow(dead_code))] // used by the carousel step of machine.rs once it drives children
    pub fn child_count(&self) -> usize {
        match &self.media {
            Media::Carousel(urls) => urls.len(),
            _ => 0,
        }
    }

    /// `POST /<user>/media` body of child `index` (0-based): the image and `is_carousel_item`, no caption (the
    /// caption belongs to the parent). None past the last child or for a non-carousel.
    #[cfg_attr(not(test), allow(dead_code))] // see child_count
    pub fn child_body(&self, index: usize) -> Option<Value> {
        match &self.media {
            Media::Carousel(urls) => urls.get(index).map(|url| child_body_of(url)),
            _ => None,
        }
    }

    /// `POST /<user>/media` body of the carousel PARENT, once every child id is known: `media_type=CAROUSEL`,
    /// `children` as a comma-separated id list (UNVERIFIED: Meta's page words it as a list parameter; a JSON
    /// array is not confirmed) and the caption. None for a non-carousel, a count other than the number of
    /// children and an id that is not a plain id.
    #[cfg_attr(not(test), allow(dead_code))] // see child_count
    pub fn parent_body(&self, children: &[String]) -> Option<Value> {
        let Media::Carousel(urls) = &self.media else {
            return None;
        };
        (children.len() == urls.len() && children.iter().all(|id| plain_id(id))).then(|| {
            json!({
                "media_type": "CAROUSEL",
                "children": children.join(","),
                "caption": self.caption
            })
        })
    }
}

/// The create body of one carousel child.
fn child_body_of(url: &str) -> Value {
    json!({ "image_url": url, "is_carousel_item": true })
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

/// The characters Instagram counts for the caption: UTF-16 code units (see [`CAPTION_LIMIT`]).
pub fn counted_length(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Hashtags in a caption: each `#` directly followed by a letter, a digit or `_` starts one. A bare `#`, or
/// one followed by a space or punctuation, does not count.
pub fn hashtag_count(text: &str) -> usize {
    let mut count = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '#'
            && chars
                .peek()
                .is_some_and(|n| n.is_alphanumeric() || *n == '_')
        {
            count += 1;
        }
    }
    count
}

/// True for a plain `https://` URL with a host, no user info, no spaces or control characters.
fn plain_https_url(url: &str, longest: usize) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    url.len() <= longest
        && !authority.is_empty()
        && !authority.contains('@')
        && !url
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
}

/// The host of a plain `https://` URL, lowercase, without user info (already refused) or port.
fn host_of(url: &str) -> &str {
    let rest = url.strip_prefix("https://").unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.starts_with('[') {
        return authority;
    }
    authority.split(':').next().unwrap_or("")
}

/// True when `host` cannot be a public media host: `localhost` and its subdomains, `.local` and `.internal`
/// names, an IPv4 literal (public media is served by name, and a literal is how private addresses are named)
/// and an IPv6 literal. Meta must be able to fetch the URL, so these can never work; they are refused before
/// any request. `.test` is NOT refused: the mock platform's media host is `media.mock.test`.
fn private_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host.starts_with('[')
        || rightkit_http::parse_host_ip(&host).is_some()
}

/// The media URL checks that need no request (#153): a plain `https` URL with a host (no user info, spaces or
/// control characters, at most `MAX_MEDIA_URL` characters) whose host is not a private or literal address.
/// The reachability check (a HEAD or ranged GET of the URL before the container create) and the object-storage
/// upload that produces the URL are NOT done here: they need the HTTP client (machine.rs, http.rs).
pub fn check_media_url(url: &str) -> Result<(), String> {
    if !plain_https_url(url, MAX_MEDIA_URL) {
        return Err(format!(
            "media URL must be a public https:// URL with a host, no user info and no spaces (at most {MAX_MEDIA_URL} characters)"
        ));
    }
    if private_host(host_of(url)) {
        return Err(
            "media URL host is a local, internal or address-literal host that Instagram cannot fetch; use a public host name"
                .to_string(),
        );
    }
    Ok(())
}

/// The file extension of the URL path, lowercase, None when the last path segment has none.
fn extension_of(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://").unwrap_or(url);
    let path = rest.split(['?', '#']).next().unwrap_or("");
    // The last path segment after the host (the host is the first segment).
    let mut segments = path.split('/');
    segments.next();
    let segment = segments.next_back()?;
    let (_, ext) = segment.rsplit_once('.')?;
    (!ext.is_empty()).then(|| ext.to_ascii_lowercase())
}

/// Refuses a URL whose extension names the wrong kind of file: a video must be `mp4` or `mov`, an image `jpg` or
/// `jpeg` (UNVERIFIED: recalled from Meta's content-publishing page, which the capability matrix could not
/// confirm: "Reel duration, file size ...: unsourced"). A URL with no extension passes (signed object-storage
/// URLs often have none); only a name that contradicts the kind is refused.
fn check_media_kind(url: &str, kind: &str) -> Result<(), Fail> {
    let (allowed, label): (&[&str], &str) = match kind {
        "video" => (&["mp4", "mov"], "mp4 or mov"),
        "image" => (&["jpg", "jpeg"], "jpg or jpeg"),
        _ => return Ok(()),
    };
    match extension_of(url) {
        Some(ext) if !allowed.contains(&ext.as_str()) => Err((
            "media_url_invalid",
            format!(
                "the instagram {kind} URL ends in .{ext}; Instagram takes {label} (unverified)"
            ),
        )),
        _ => Ok(()),
    }
}

/// The URL of one media item, checked, or the typed refusal.
fn media_url_of(item: &crate::wire::PublishMedia) -> Result<&str, Fail> {
    let Some(url) = item.url.as_deref().filter(|u| !u.trim().is_empty()) else {
        return Err((
            "media_required",
            "the instagram media item has no URL: pass media_urls for its hash to publish.enqueue"
                .to_string(),
        ));
    };
    check_media_url(url)
        .map_err(|message| ("media_url_invalid", format!("the instagram {message}")))?;
    check_media_kind(url, &item.kind)?;
    Ok(url)
}

/// A carousel: [`CAROUSEL_MIN`] to [`CAROUSEL_MAX`] images, each with a checked URL.
fn carousel_of(payload: &crate::wire::PublishPayload) -> Result<Media, Fail> {
    let count = payload.media.len();
    if !(CAROUSEL_MIN..=CAROUSEL_MAX).contains(&count) {
        return Err((
            "media_unsupported",
            format!(
                "an instagram carousel holds {CAROUSEL_MIN} to {CAROUSEL_MAX} images; the entry has {count}"
            ),
        ));
    }
    let mut urls = Vec::with_capacity(count);
    for item in &payload.media {
        if item.kind != "image" {
            return Err((
                "media_unsupported",
                format!(
                    "an instagram carousel of this client holds images only (a video child is not implemented), not {:?}",
                    item.kind
                ),
            ));
        }
        urls.push(media_url_of(item)?.to_string());
    }
    Ok(Media::Carousel(urls))
}

/// The media of the job as a `Media`, or the typed refusal.
fn media_of(payload: &crate::wire::PublishPayload) -> Result<Media, Fail> {
    if payload.media.len() > 1 {
        return carousel_of(payload);
    }
    let [item] = payload.media.as_slice() else {
        return Err((
            "media_required",
            "instagram has no text-only post: the entry needs one video or image with a media URL"
                .to_string(),
        ));
    };
    let url = media_url_of(item)?;
    match item.kind.as_str() {
        "video" => Ok(Media::Reel(url.to_string())),
        "image" => Ok(Media::Photo(url.to_string())),
        other => Err((
            "media_unsupported",
            format!("the instagram client posts a video (a reel) or an image, not {other:?}"),
        )),
    }
}

/// Validate a payload and build the post. Refused before any request: an account that is not a plain id (it
/// becomes a path segment), any setting (none is used, and ignoring one silently would post the wrong thing), a
/// job with no usable media (Instagram has no text-only post), a carousel outside 2 to 10 images or with a
/// video, a media URL that is not a plain public `https` URL, an empty caption and a caption over
/// [`CAPTION_LIMIT`].
pub fn prepare(
    payload: &crate::wire::PublishPayload,
    remote_account: &str,
) -> Result<Prepared, Fail> {
    if !plain_id(remote_account) {
        return Err((
            "bad_input",
            format!(
                "remote_account must be an Instagram user id (letters, digits, '-' or '_', at most {MAX_ID}) to become the request path"
            ),
        ));
    }
    let unused = match &payload.settings {
        Value::Null => false,
        Value::Object(map) => !map.is_empty(),
        _ => true,
    };
    if unused {
        return Err((
            "bad_input",
            "the instagram client uses no settings; remove them from the job".to_string(),
        ));
    }
    let media = media_of(payload)?;
    let caption = payload.caption.clone();
    if caption.trim().is_empty() {
        return Err((
            "caption_empty",
            "the instagram caption is empty".to_string(),
        ));
    }
    let count = counted_length(&caption);
    if count > CAPTION_LIMIT {
        return Err((
            "text_too_long",
            format!("the caption counts {count} characters and Instagram allows {CAPTION_LIMIT}"),
        ));
    }
    let tags = hashtag_count(&caption);
    if tags > HASHTAG_LIMIT {
        return Err((
            "too_many_hashtags",
            format!("the caption has {tags} hashtags and Instagram allows {HASHTAG_LIMIT}"),
        ));
    }
    Ok(Prepared {
        user: remote_account.to_string(),
        caption,
        media,
    })
}

/// `publish.enqueue` check: the payload and account must be ones this module can post.
pub fn check(payload: &crate::wire::PublishPayload, remote_account: &str) -> Result<(), Fail> {
    prepare(payload, remote_account).map(|_| ())
}

/// The id in a create or publish answer: a plain string id. Anything else is not accepted as this job's
/// container or post.
pub fn object_id(body: &Value) -> Option<String> {
    let id = body.get("id")?.as_str()?;
    plain_id(id).then(|| id.to_string())
}

/// The `GET /v<version>/<container-id>?fields=status_code` path for a stored container id. None when the stored
/// value is not a plain id, so nothing odd is ever put in a path. The token is never part of it.
pub fn status_path(container_id: &str) -> Option<String> {
    plain_id(container_id).then(|| {
        format!(
            "/{GRAPH_VERSION}/{}?fields={STATUS_FIELDS}",
            percent_encode(container_id)
        )
    })
}

/// The container's `status_code` from a status answer. An answer that names another container (`id` present and
/// not the stored one) is not this job's, and a missing or non-text `status_code` is no status.
pub fn status_code<'a>(container_id: &str, body: &'a Value) -> Option<&'a str> {
    if body
        .get("id")
        .is_some_and(|id| id.as_str() != Some(container_id))
    {
        return None;
    }
    body.get("status_code")?.as_str()
}

/// The `GET /v<version>/<media-id>?fields=...` path that reads back a stored media id. None when the stored value
/// is not a plain id. The token is never part of it.
pub fn read_path(media_id: &str) -> Option<String> {
    plain_id(media_id).then(|| {
        format!(
            "/{GRAPH_VERSION}/{}?fields={READ_FIELDS}",
            percent_encode(media_id)
        )
    })
}

/// The permalink, once the read has confirmed the stored post: the answer's `id` must be the stored media id and
/// its `permalink` a plain `https://` URL.
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

/// True when a rejected reply's body names a missing-permission error (`error.code` 10 or 200). The Graph error
/// shape is the one the Facebook client reads, so the rule is shared.
pub fn is_permission_error(message: &str) -> bool {
    facebook::is_permission_error(message)
}

/// A Graph error can arrive as a 4xx body instead of the bare status: code 190 is an expired or invalid token
/// (the existing `token_expired` outcome), and codes 4, 17, 32 and 613 are the throttling family (a 429). A rate
/// limit names no wait here, so the machine's default backoff applies. Everything else is left as it came (a
/// missing permission is typed by the machine through [`is_permission_error`]). The rule is the Facebook
/// client's, shared.
pub fn normalize(reply: Reply) -> Reply {
    facebook::normalize(reply)
}
