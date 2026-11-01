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
    let segment = segments.last()?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::RetryAfter;
    use crate::wire::{PublishMedia, PublishPayload};

    const ACCOUNT: &str = "acct-alpha-instagram-0001";
    const REEL: &str = "https://media.mock.test/alpha/clip.mp4";

    fn media(kind: &str, url: Option<&str>) -> PublishMedia {
        PublishMedia {
            hash: "sha256:aaa111".to_string(),
            kind: kind.to_string(),
            duration_ms: None,
            url: url.map(str::to_string),
        }
    }

    fn payload(caption: &str, items: Vec<PublishMedia>) -> PublishPayload {
        PublishPayload {
            caption: caption.to_string(),
            format: "reel".to_string(),
            media: items,
            settings: json!({}),
        }
    }

    fn reel(caption: &str) -> PublishPayload {
        payload(caption, vec![media("video", Some(REEL))])
    }

    #[test]
    fn the_graph_version_is_the_facebook_registry_pin() {
        let pinned = crate::api_versions::pinned(facebook::PLATFORM).expect("facebook is pinned");
        assert_eq!(GRAPH_VERSION, pinned.version);
    }

    #[test]
    fn the_requests_are_the_documented_container_flow() {
        let prepared = prepare(&reel("Hello Instagram"), ACCOUNT).expect("prepared");
        assert_eq!(
            prepared.create_path(),
            format!("/{GRAPH_VERSION}/acct-alpha-instagram-0001/media")
        );
        assert_eq!(
            prepared.publish_path(),
            format!("/{GRAPH_VERSION}/acct-alpha-instagram-0001/media_publish")
        );
        assert_eq!(
            prepared.create_body(),
            json!({ "media_type": "REELS", "video_url": REEL, "caption": "Hello Instagram" })
        );
        let photo = payload(
            "A photo",
            vec![media(
                "image",
                Some("https://media.mock.test/alpha/pic.jpg"),
            )],
        );
        let prepared = prepare(&photo, ACCOUNT).expect("prepared");
        assert_eq!(
            prepared.create_body(),
            json!({ "image_url": "https://media.mock.test/alpha/pic.jpg", "caption": "A photo" })
        );
        assert_eq!(publish_body("c-1"), json!({ "creation_id": "c-1" }));
        assert_eq!(
            status_path("ig-000001").as_deref(),
            Some(format!("/{GRAPH_VERSION}/ig-000001?fields=status_code").as_str())
        );
        // A null settings value (a payload read before its JSON is restored) is accepted.
        let mut nulled = reel("x");
        nulled.settings = Value::Null;
        assert!(prepare(&nulled, ACCOUNT).is_ok());
    }

    #[test]
    fn the_user_id_comes_only_from_the_stored_account() {
        let prepared = prepare(&reel("text"), "1784-abc_X").expect("prepared");
        assert_eq!(
            prepared.create_path(),
            format!("/{GRAPH_VERSION}/1784-abc_X/media")
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
                matches!(prepare(&reel("text"), bad), Err(("bad_input", _))),
                "{bad:?} should be refused"
            );
        }
    }

    #[test]
    fn a_job_without_usable_media_is_refused_before_any_request() {
        // Instagram has no text-only post.
        assert!(matches!(
            prepare(&payload("text", Vec::new()), ACCOUNT),
            Err(("media_required", _))
        ));
        // A media item whose hash was not given a URL at enqueue, and a blank URL.
        for url in [None, Some(""), Some("   ")] {
            assert!(
                matches!(
                    prepare(&payload("text", vec![media("video", url)]), ACCOUNT),
                    Err(("media_required", _))
                ),
                "{url:?}"
            );
        }
        // Only a plain https URL is accepted; the engine never fetches it.
        for bad in [
            "http://media.mock.test/a.mp4",
            "ftp://media.mock.test/a.mp4",
            "javascript:alert(1)",
            "https://",
            "https:///a.mp4",
            "https://user:pw@media.mock.test/a.mp4",
            "https://media.mock.test/a b.mp4",
            "https://media.mock.test/a\\b.mp4",
        ] {
            assert!(
                matches!(
                    prepare(&payload("text", vec![media("video", Some(bad))]), ACCOUNT),
                    Err(("media_url_invalid", _))
                ),
                "{bad:?}"
            );
        }
        let long = format!("https://media.mock.test/{}", "a".repeat(MAX_MEDIA_URL));
        assert!(matches!(
            prepare(&payload("text", vec![media("video", Some(&long))]), ACCOUNT),
            Err(("media_url_invalid", _))
        ));
        // A carousel and an unknown kind are not implemented.
        let two = payload(
            "text",
            vec![media("video", Some(REEL)), media("image", Some(REEL))],
        );
        assert!(matches!(
            prepare(&two, ACCOUNT),
            Err(("media_unsupported", _))
        ));
        assert!(matches!(
            prepare(&payload("text", vec![media("audio", Some(REEL))]), ACCOUNT),
            Err(("media_unsupported", _))
        ));
    }

    fn image_item(n: usize) -> PublishMedia {
        media(
            "image",
            Some(&format!("https://media.mock.test/alpha/pic-{n}.jpg")),
        )
    }

    #[test]
    fn a_carousel_is_children_then_one_parent() {
        let items: Vec<PublishMedia> = (1..=3).map(image_item).collect();
        let prepared = prepare(&payload("A carousel", items), ACCOUNT).expect("prepared");
        assert_eq!(prepared.child_count(), 3);
        assert_eq!(
            prepared.child_body(1),
            Some(json!({
                "image_url": "https://media.mock.test/alpha/pic-2.jpg",
                "is_carousel_item": true
            }))
        );
        assert_eq!(prepared.child_body(3), None);
        let ids = ["ig-000001", "ig-000002", "ig-000003"].map(String::from);
        assert_eq!(
            prepared.parent_body(&ids),
            Some(json!({
                "media_type": "CAROUSEL",
                "children": "ig-000001,ig-000002,ig-000003",
                "caption": "A carousel"
            }))
        );
        // A wrong count or an odd id builds nothing.
        assert_eq!(prepared.parent_body(&ids[..2]), None);
        assert_eq!(
            prepared.parent_body(&["a".to_string(), "b/c".to_string(), "d".to_string()]),
            None
        );
        // A reel or a photo has no children.
        let single = prepare(&reel("x"), ACCOUNT).expect("prepared");
        assert_eq!(single.child_count(), 0);
        assert_eq!(single.parent_body(&[]), None);
    }

    #[test]
    fn carousel_size_and_kind_are_enforced() {
        let n = |count: usize| payload("c", (1..=count).map(image_item).collect());
        assert!(prepare(&n(CAROUSEL_MIN), ACCOUNT).is_ok());
        assert!(prepare(&n(CAROUSEL_MAX), ACCOUNT).is_ok());
        assert!(matches!(
            prepare(&n(CAROUSEL_MAX + 1), ACCOUNT),
            Err(("media_unsupported", _))
        ));
        let mut with_video = n(2);
        with_video.media[1] = media("video", Some(REEL));
        assert!(matches!(
            prepare(&with_video, ACCOUNT),
            Err(("media_unsupported", _))
        ));
        let mut no_url = n(2);
        no_url.media[0] = media("image", None);
        assert!(matches!(
            prepare(&no_url, ACCOUNT),
            Err(("media_required", _))
        ));
        let mut bad_url = n(2);
        bad_url.media[1] = media("image", Some("http://media.mock.test/a.jpg"));
        assert!(matches!(
            prepare(&bad_url, ACCOUNT),
            Err(("media_url_invalid", _))
        ));
    }

    #[test]
    fn media_urls_are_checked_for_host_and_extension() {
        for bad in [
            "https://localhost/a.mp4",
            "https://app.localhost/a.mp4",
            "https://printer.local/a.mp4",
            "https://db.internal/a.mp4",
            "https://127.0.0.1/a.mp4",
            "https://10.0.0.5:8443/a.mp4",
            "https://[::1]/a.mp4",
        ] {
            assert!(check_media_url(bad).is_err(), "{bad:?}");
            assert!(
                matches!(
                    prepare(&payload("t", vec![media("video", Some(bad))]), ACCOUNT),
                    Err(("media_url_invalid", _))
                ),
                "{bad:?}"
            );
        }
        // A named public host, a port and a query pass; so does the mock's own media host.
        for good in [
            "https://media.mock.test/a.mp4",
            "https://cdn.example.com:8443/a.mp4?sig=abc",
            "https://cdn.example.com/no-extension",
            "https://1.example.com/a.mp4",
        ] {
            assert!(check_media_url(good).is_ok(), "{good:?}");
        }
        // The extension may not contradict the kind; a missing one passes.
        for (kind, url) in [
            ("video", "https://media.mock.test/a.jpg"),
            ("video", "https://media.mock.test/a.png"),
            ("image", "https://media.mock.test/a.mp4"),
        ] {
            assert!(
                matches!(
                    prepare(&payload("t", vec![media(kind, Some(url))]), ACCOUNT),
                    Err(("media_url_invalid", _))
                ),
                "{url:?}"
            );
        }
        for (kind, url) in [
            ("video", "https://media.mock.test/a.MOV"),
            ("video", "https://media.mock.test/clip?name=a.jpg"),
            ("image", "https://media.mock.test/a.JPEG#x"),
            ("image", "https://media.mock.test/object-key"),
        ] {
            assert!(
                prepare(&payload("t", vec![media(kind, Some(url))]), ACCOUNT).is_ok(),
                "{url:?}"
            );
        }
    }

    #[test]
    fn settings_and_an_empty_caption_are_refused() {
        for settings in [json!({ "share_to_feed": true }), json!("x"), json!(1)] {
            let mut with = reel("text");
            with.settings = settings;
            assert!(matches!(prepare(&with, ACCOUNT), Err(("bad_input", _))));
        }
        assert!(matches!(
            prepare(&reel("  \n "), ACCOUNT),
            Err(("caption_empty", _))
        ));
    }

    #[test]
    fn the_caption_limit_counts_utf16_units_and_is_exact() {
        assert!(prepare(&reel(&"a".repeat(CAPTION_LIMIT)), ACCOUNT).is_ok());
        let over = CAPTION_LIMIT + 1;
        let Err(err) = prepare(&reel(&"a".repeat(over)), ACCOUNT) else {
            panic!("{over} characters must be refused");
        };
        assert_eq!(err.0, "text_too_long");
        assert!(err.1.contains(&format!("counts {over} characters")));
        assert!(err.1.contains(&format!("allows {CAPTION_LIMIT}")));
        // An astral character is two code units: half the limit of them is exactly the limit.
        assert_eq!(counted_length("\u{1f600}"), 2);
        assert_eq!(counted_length("caf\u{e9}"), 4);
        let emoji = "\u{1f600}".repeat(CAPTION_LIMIT / 2);
        assert!(prepare(&reel(&emoji), ACCOUNT).is_ok());
        let over_emoji = format!("{emoji}a");
        assert!(matches!(
            prepare(&reel(&over_emoji), ACCOUNT),
            Err(("text_too_long", _))
        ));
    }

    #[test]
    fn the_hashtag_limit_is_exact_and_bare_hashes_do_not_count() {
        let tags = |n: usize| {
            (0..n)
                .map(|i| format!("#t{i}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert!(prepare(&reel(&tags(HASHTAG_LIMIT)), ACCOUNT).is_ok());
        let Err(err) = prepare(&reel(&tags(HASHTAG_LIMIT + 1)), ACCOUNT) else {
            panic!("{} hashtags must be refused", HASHTAG_LIMIT + 1);
        };
        assert_eq!(err.0, "too_many_hashtags");
        assert_eq!(hashtag_count("# ## #! #\u{e9}t \u{1f600}#x #_y"), 3);
    }

    #[test]
    fn answers_are_matched_to_the_stored_objects() {
        assert_eq!(
            object_id(&json!({ "id": "ig-000001" })).as_deref(),
            Some("ig-000001")
        );
        assert_eq!(object_id(&json!({})), None);
        assert_eq!(object_id(&json!({ "id": 12 })), None);
        for bad in ["", "a b", "a/b", "../x", "a?b"] {
            assert_eq!(object_id(&json!({ "id": bad })), None, "{bad:?}");
        }
        // Real Instagram ids are long decimal strings; they are plain ids too.
        assert_eq!(
            object_id(&json!({ "id": "17895695668004550" })).as_deref(),
            Some("17895695668004550")
        );

        assert_eq!(
            read_path("ig-000002").as_deref(),
            Some(
                format!("/{GRAPH_VERSION}/ig-000002?fields=id,permalink,caption,timestamp")
                    .as_str()
            )
        );
        assert_eq!(read_path("a/b"), None);
        assert_eq!(read_path(""), None);
        assert_eq!(status_path("a/b"), None);

        let read = json!({
            "id": "ig-000002",
            "permalink": "https://instagram.mock.test/p/ig-000002/",
            "caption": "Hello",
            "timestamp": "2026-09-21T14:13:20Z"
        });
        assert_eq!(
            permalink_from(Some("ig-000002"), &read).as_deref(),
            Some("https://instagram.mock.test/p/ig-000002/")
        );
        assert_eq!(permalink_from(Some("ig-000009"), &read), None);
        assert_eq!(permalink_from(None, &read), None);
        assert_eq!(
            permalink_from(Some("ig-000002"), &json!({ "id": "ig-000002" })),
            None
        );
        for bad in [
            "http://x.test/p",
            "javascript:alert(1)",
            "https://x.test/a b",
            "",
        ] {
            let body = json!({ "id": "ig-000002", "permalink": bad });
            assert_eq!(permalink_from(Some("ig-000002"), &body), None, "{bad:?}");
        }
    }

    #[test]
    fn a_status_answer_must_be_this_containers() {
        let body = json!({ "id": "ig-000001", "status_code": "FINISHED" });
        assert_eq!(status_code("ig-000001", &body), Some("FINISHED"));
        // The real answer may carry only the status; another container's answer is refused.
        assert_eq!(
            status_code("ig-000001", &json!({ "status_code": "IN_PROGRESS" })),
            Some("IN_PROGRESS")
        );
        assert_eq!(status_code("ig-000009", &body), None);
        assert_eq!(
            status_code("ig-000001", &json!({ "id": "ig-000001" })),
            None
        );
        assert_eq!(status_code("ig-000001", &json!({ "status_code": 3 })), None);
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
        for code in [1, 10, 100, 200, 190_000, 400, 6130] {
            assert!(
                matches!(normalize(rejected(400, code)), Reply::Rejected { .. }),
                "code {code}"
            );
        }
        assert!(matches!(normalize(Reply::Timeout), Reply::Timeout));

        let body = |code: i64| format!("{{\"error\":{{\"message\":\"m\",\"code\":{code}}}}}");
        assert!(is_permission_error(&body(10)));
        assert!(is_permission_error(&body(200)));
        for code in [1, 100, 190, 20, 2000] {
            assert!(!is_permission_error(&body(code)), "code {code}");
        }
    }
}
