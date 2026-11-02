//! LinkedIn request shapes for the publish core: a member text post, written with `POST /rest/posts` and
//! read back with `GET /rest/posts/<url-encoded urn>`.
//!
//! The phase machine is unchanged (`create -> read_permalink -> done`); this module supplies what is
//! LinkedIn-specific: the request body and headers, the text rules and the answer parsing.
//!
//! No idempotency key. The Posts API has none, so a lost answer after a create can never be made safe by
//! repeating the create. The create therefore carries NO `Idempotency-Key`, an ambiguous answer is
//! `outcome_unknown`, and `publish.reconcile` (machine.rs `reconcile_linkedin`) never creates anything: it
//! may only adopt a post it can READ BY URN, and without a URN it leaves the job `outcome_unknown` for a
//! person. The new post's URN is only learned from the `x-restli-id` header of the create answer.
//!
//! The API version. Every request carries `LinkedIn-Version: <YYYYMM>`. The value is read from the
//! version registry (`crate::api_versions`, the table `doctor` warns from), never written here, and it is
//! never changed automatically: a reply saying the version is no longer supported stops the job with
//! `api_version_unsupported` and names the registry (see [`version_rejection`]).
//!
//! The author. The `author` of the post is `urn:li:person:<remote_account>`, built from the account stored
//! on the job and validated as a member id. No text from the caption reaches it.
//!
//! Company Page (organisation) author. `settings.author_organization`, a string of digits (the organisation
//! id), makes the author `urn:li:organization:<id>` instead; `remote_account` stays the administering member.
//! Whether that member administers the page is LinkedIn's decision (a 403 is a `platform_rejected` job); the
//! mock checks it against the organisations listed for the account in its token fixture. The scope
//! `w_organization_social` and the company-page role it needs are in docs/p1/capability-matrix.md (verified
//! 2026-10-16). UNVERIFIED here: that the Posts API takes exactly this author form for an organisation with the
//! same body as a member post, and that organisation posting needs partner approval beyond the Community
//! Management API (the matrix lists it as unverified).
//!
//! Images. A post with images first uploads each file (see `machine.rs` `media_upload`): `POST
//! /rest/images?action=initializeUpload` with `{"initializeUploadRequest":{"owner": <author>}}` answers
//! `{"value":{"uploadUrl", "image": "urn:li:image:..."}}`, then the file bytes are sent with a `PUT` to the
//! `uploadUrl`. The image URNs are kept on the job (`remote.container_id`, the shared media pattern) and the
//! create sends one as `content.media.id`, or several as `content.multiImage.images[].id`. Taken from LinkedIn's
//! published Images API documentation, which docs/p1/capability-matrix.md does NOT cite ("media specs:
//! unsourced"), so every detail is UNVERIFIED: the initializeUpload path and body, the PUT with a bearer token,
//! the `content.media` and `content.multiImage` shapes, the 2-to-20 image range and the file size limit. The
//! bearer token goes to the upload URL only when it is on the platform's own origin or on linkedin.com (see
//! [`upload_token_policy`]); any other https host gets the bytes without it, and any other scheme is refused.
//! Articles are NOT implemented and are refused with `media_unsupported` before any request.
//!
//! Video and documents (#135). A post carries up to 20 images, OR one video, OR one document, never a mix.
//! Sourced (docs/p1/capability-matrix.md, learn.microsoft.com Community Management shares): a video is an MP4 of 3 s
//! to 30 min and 75 KB to 500 MB; a document is up to 100 MB and 300 pages. Everything else below is recalled from
//! LinkedIn's Videos and Documents API pages and is UNVERIFIED against a live server: the paths `/rest/videos` and
//! `/rest/documents`, `?action=initializeUpload` and `?action=finalizeUpload`, the `fileSizeBytes`, `uploadCaptions`
//! and `uploadThumbnail` request fields, the answer fields `value.video`, `value.document`, `value.uploadToken`
//! and `value.uploadInstructions[].{uploadUrl, firstByte, lastByte}`, the `etag` response header of a part PUT, the
//! `finalizeUploadRequest.{video, uploadToken, uploadedPartIds}` body, the `application/octet-stream` upload type, the
//! `content.media.{id, title}` post shape (a document needs a title; `settings.media_title` or the caption's first
//! line), the file types (MP4; PDF, DOC(X), PPT(X)) and that a finalized video needs no processing wait before the
//! post. The video PUTs follow the ranges the initialise answer names, in order, and the finalize carries the part
//! ETags. The page count of a PDF is estimated by counting `/Type /Page` objects (an approximation: pages inside
//! compressed object streams are not seen, so a long compressed PDF is not refused here). The whole file is held in
//! memory and fetched under the HTTP client's 32 MiB cap, so a video or document above that cannot be sent yet.
//!
//! Secrets: the access token is the command's `token`, sent only as the bearer of a request and scrubbed
//! from every message by `http::send_with`. Nothing in this file builds a message from the token.
//!
//! Protocol facts used here come from LinkedIn's published Posts API documentation and were not verified
//! against a live server in this task; docs/PUBLISH-CORE.md lists what is left out because it is not known.
use rightkit_http::percent_encode;
use serde_json::{json, Value};

use crate::api_versions;
use crate::http::Reply;
use crate::Fail;

/// The platform name this module drives.
pub const PLATFORM: &str = "linkedin";
/// The create request path, and the prefix of the read path, under the base URL.
pub const POSTS_PATH: &str = "/rest/posts";
/// Longest `commentary` the Posts API takes, in UTF-16 code units (the stricter reading of "characters").
pub const COMMENTARY_LIMIT: usize = 3_000;
/// The response headers the create needs back: the new post's URN.
pub const ECHO: [&str; 1] = ["x-restli-id"];
/// The Rest.li protocol version the Posts API requires in `X-Restli-Protocol-Version`.
const RESTLI_PROTOCOL_VERSION: &str = "2.0.0";
/// Characters the Posts API reads as markup in `commentary` ("little text format"). Escaping them is not
/// implemented (the escape rules are not verified here), so a text that holds one is refused before any
/// request instead of being posted cut short or changed.
const RESERVED: &str = "|{}@[]()<>#\\*_~";
/// Longest member id accepted for the author URN.
const MAX_MEMBER_ID: usize = 64;
/// Longest post URN accepted from an answer.
const MAX_URN: usize = 128;
/// Longest organisation id accepted for `settings.author_organization`.
const MAX_ORG_ID: usize = 20;
/// The image upload initialisation path, under the base URL (UNVERIFIED; see the module comment).
pub const IMAGES_PATH: &str = "/rest/images";
const INIT_QUERY: &str = "action=initializeUpload";
/// The video upload path, under the base URL (UNVERIFIED; see the module comment).
pub const VIDEOS_PATH: &str = "/rest/videos";
/// The document upload path, under the base URL (UNVERIFIED).
pub const DOCUMENTS_PATH: &str = "/rest/documents";
const FINALIZE_QUERY: &str = "action=finalizeUpload";
/// Shortest and longest video, in milliseconds (sourced: 3 s to 30 min).
pub const VIDEO_MIN_MS: u64 = 3_000;
pub const VIDEO_MAX_MS: u64 = 30 * 60 * 1_000;
/// Smallest and largest video file, in bytes (sourced: 75 KB to 500 MB; read as decimal units, so nothing is
/// refused here that LinkedIn would take).
pub const VIDEO_MIN_BYTES: usize = 75_000;
pub const VIDEO_MAX_BYTES: usize = 500_000_000;
/// Largest document file and most pages (sourced: 100 MB and 300 pages).
pub const DOCUMENT_MAX_BYTES: usize = 100_000_000;
pub const DOCUMENT_MAX_PAGES: usize = 300;
/// Most upload parts accepted from an initialise answer (500 MB in 4 MiB parts is 120; UNVERIFIED part size).
const MAX_PARTS: usize = 512;
/// Longest media title sent, in characters (UNVERIFIED).
const MAX_TITLE: usize = 100;
/// Most images one post carries: `multiImage` takes 2 to 20 (UNVERIFIED).
pub const MAX_IMAGES: usize = 20;
/// The largest image file accepted here (UNVERIFIED: LinkedIn's own limit is not in a fetched source). The
/// engine's media fetch is capped at 32 MiB by its client anyway.
pub const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;

/// What a job posts, validated and ready to serialise.
pub struct Prepared {
    author: String,
    text: String,
    /// The uploaded media URNs (images, or one video, or one document), in entry order (empty for a text post).
    images: Vec<String>,
    /// The `content.media.title` of a video or document (a document always has one).
    title: Option<String>,
}

impl Prepared {
    /// The author URN: the member, or the organisation named by `settings.author_organization`.
    #[cfg(test)]
    pub fn author(&self) -> &str {
        &self.author
    }

    /// The create request body. `content` is present only when the post has images.
    pub fn body(&self) -> Value {
        let mut body = json!({
            "author": self.author,
            "commentary": self.text,
            "visibility": "PUBLIC",
            "distribution": {
                "feedDistribution": "MAIN_FEED",
                "targetEntities": [],
                "thirdPartyDistributionChannels": []
            },
            "lifecycleState": "PUBLISHED",
            "isReshareDisabledByAuthor": false
        });
        if let (Some(content), Value::Object(map)) =
            (content_of(&self.images, self.title.as_deref()), &mut body)
        {
            map.insert("content".to_string(), content);
        }
        body
    }
}

/// The `content` member for uploaded media: one image, video or document is `media` (a video or document may
/// carry a `title`), several images are `multiImage` (UNVERIFIED shapes).
fn content_of(urns: &[String], title: Option<&str>) -> Option<Value> {
    match urns {
        [] => None,
        [one] => {
            let mut media = json!({ "id": one });
            let titled = one.starts_with("urn:li:video:") || one.starts_with("urn:li:document:");
            if let (true, Some(title), Value::Object(map)) = (titled, title, &mut media) {
                map.insert("title".to_string(), Value::String(title.to_string()));
            }
            Some(json!({ "media": media }))
        }
        many => Some(json!({
            "multiImage": {
                "images": many.iter().map(|id| json!({ "id": id })).collect::<Vec<_>>()
            }
        })),
    }
}

/// The `LinkedIn-Version` value: the registry's pinned version for LinkedIn. `api_version_missing` when
/// the registry has none or holds something that is not `YYYYMM`; nothing is guessed or defaulted.
pub fn api_version() -> Result<&'static str, Fail> {
    let Some(pinned) = api_versions::pinned(PLATFORM) else {
        return Err((
            "api_version_missing",
            "no LinkedIn API version is pinned in the version registry (rightkit-social-publish src/api_versions.rs); nothing was posted"
                .to_string(),
        ));
    };
    let version = pinned.version;
    if version.len() != 6 || !version.bytes().all(|b| b.is_ascii_digit()) {
        return Err((
            "api_version_missing",
            format!(
                "the pinned LinkedIn API version {version:?} in the version registry (rightkit-social-publish src/api_versions.rs) is not YYYYMM; nothing was posted"
            ),
        ));
    }
    Ok(version)
}

/// The two headers every versioned Posts API request carries.
pub fn request_headers(version: &'static str) -> [(&'static str, &'static str); 2] {
    [
        ("LinkedIn-Version", version),
        ("X-Restli-Protocol-Version", RESTLI_PROTOCOL_VERSION),
    ]
}

/// `urn:li:person:<id>` for a stored account id, which must look like a member id: letters, digits, `-`
/// and `_`, at most 64. The account is the only input.
fn author_urn(remote_account: &str) -> Result<String, Fail> {
    let plain = !remote_account.is_empty()
        && remote_account.len() <= MAX_MEMBER_ID
        && remote_account
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !plain {
        return Err((
            "bad_input",
            format!(
                "remote_account must be a LinkedIn member id (letters, digits, '-' or '_', at most {MAX_MEMBER_ID}) to become the post's author URN"
            ),
        ));
    }
    Ok(format!("urn:li:person:{remote_account}"))
}

/// The characters LinkedIn counts for `text`: UTF-16 code units, the stricter reading of "characters"
/// (a code unit count is never below the scalar count, so nothing is accepted here that a scalar
/// reading would refuse).
pub fn counted_length(text: &str) -> usize {
    text.encode_utf16().count()
}

/// How `build` treats the payload's media.
enum Mode<'a> {
    /// The text-only builder: any media is refused (the job has no upload step on this path).
    #[cfg(test)]
    NoMedia,
    /// The enqueue check: the media must be postable, but nothing is uploaded yet.
    Check,
    /// The create step: the media has been uploaded and these are its URNs, one per item.
    Images(&'a [String]),
}

/// The author URN: `urn:li:organization:<id>` when `settings.author_organization` names an organisation,
/// else the member. The member account is validated either way (it is the administering member).
fn author_of(remote_account: &str, settings: &Value) -> Result<String, Fail> {
    let member = author_urn(remote_account)?;
    match settings.get("author_organization") {
        None | Some(Value::Null) => Ok(member),
        Some(Value::String(id))
            if !id.is_empty()
                && id.len() <= MAX_ORG_ID
                && id.bytes().all(|b| b.is_ascii_digit()) =>
        {
            Ok(format!("urn:li:organization:{id}"))
        }
        Some(_) => Err((
            "bad_input",
            format!(
                "settings.author_organization must be an organisation id: digits only, at most {MAX_ORG_ID}"
            ),
        )),
    }
}

/// The lowercase extension of the last path segment of an http(s) URL, None when it has none.
fn url_extension(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path = rest.split(['?', '#']).next().unwrap_or("");
    let mut segments = path.split('/');
    segments.next();
    let (_, ext) = segments.last()?.rsplit_once('.')?;
    (!ext.is_empty()).then(|| ext.to_ascii_lowercase())
}

/// The enqueue-time rules for media: up to [`MAX_IMAGES`] images, OR one video, OR one document (never a mix),
/// each with a plain http(s) URL. A video's `duration_ms`, when the entry states one, must be 3 s to 30 min. A
/// URL whose extension names the wrong type of file is refused (a URL with no extension passes: signed storage
/// URLs often have none, and the bytes are sniffed at upload).
fn check_media(payload: &crate::wire::PublishPayload) -> Result<(), Fail> {
    if payload.media.len() > MAX_IMAGES {
        return Err((
            "media_unsupported",
            format!(
                "a linkedin post carries at most {MAX_IMAGES} images; the entry has {}",
                payload.media.len()
            ),
        ));
    }
    for item in &payload.media {
        if !matches!(item.kind.as_str(), "image" | "video" | "document") {
            return Err((
                "media_unsupported",
                format!(
                    "a linkedin post can carry images, one video or one document; media item {} is a {}, and articles are not implemented",
                    item.hash, item.kind
                ),
            ));
        }
    }
    let single = payload.media.iter().any(|item| item.kind != "image");
    if single && payload.media.len() > 1 {
        return Err((
            "media_unsupported",
            "a linkedin post carries up to 20 images, or ONE video, or ONE document; they cannot be mixed or repeated"
                .to_string(),
        ));
    }
    for item in &payload.media {
        match item.url.as_deref() {
            None => {
                return Err((
                    "media_required",
                    format!(
                        "the linkedin media item {} has no URL: pass media_urls for its hash to publish.enqueue",
                        item.hash
                    ),
                ))
            }
            Some(url) if !crate::media::plain_url(url) => {
                return Err((
                    "media_url_invalid",
                    format!(
                        "the media URL of item {} must be an http:// or https:// URL with a host, no user info and no spaces",
                        item.hash
                    ),
                ))
            }
            Some(url) => check_kind_rules(item, url)?,
        }
    }
    Ok(())
}

/// The video and document rules of one item with a usable URL (see `check_media`).
fn check_kind_rules(item: &crate::wire::PublishMedia, url: &str) -> Result<(), Fail> {
    match item.kind.as_str() {
        "video" => {
            if let Some(ms) = item.duration_ms {
                if ms > VIDEO_MAX_MS {
                    return Err((
                        "media_too_long",
                        format!(
                            "the linkedin video {} lasts {ms} ms and LinkedIn takes {VIDEO_MIN_MS} to {VIDEO_MAX_MS} ms (3 s to 30 min); nothing was posted",
                            item.hash
                        ),
                    ));
                }
                if ms < VIDEO_MIN_MS {
                    return Err((
                        "media_too_short",
                        format!(
                            "the linkedin video {} lasts {ms} ms and LinkedIn takes {VIDEO_MIN_MS} to {VIDEO_MAX_MS} ms (3 s to 30 min); nothing was posted",
                            item.hash
                        ),
                    ));
                }
            }
            match url_extension(url) {
                Some(ext) if ext != "mp4" => Err((
                    "media_url_invalid",
                    format!(
                        "the linkedin video URL of item {} ends in .{ext}; LinkedIn takes MP4",
                        item.hash
                    ),
                )),
                _ => Ok(()),
            }
        }
        "document" => match url_extension(url) {
            Some(ext) if !["pdf", "doc", "docx", "ppt", "pptx"].contains(&ext.as_str()) => Err((
                "media_url_invalid",
                format!(
                    "the linkedin document URL of item {} ends in .{ext}; LinkedIn takes PDF, DOC, DOCX, PPT or PPTX (unverified list)",
                    item.hash
                ),
            )),
            _ => Ok(()),
        },
        _ => Ok(()),
    }
}

/// The title sent with a video or document: `settings.media_title` when given (a string of 1 to 100 characters
/// with no control character), else for a document the first line of the caption cut to 60 characters (a
/// document needs a title, UNVERIFIED), else none.
fn media_title(payload: &crate::wire::PublishPayload) -> Result<Option<String>, Fail> {
    match payload.settings.get("media_title") {
        None | Some(Value::Null) => {}
        Some(Value::String(t))
            if !t.trim().is_empty()
                && t.chars().count() <= MAX_TITLE
                && !t.chars().any(char::is_control) =>
        {
            return Ok(Some(t.clone()));
        }
        Some(_) => {
            return Err((
                "bad_input",
                format!(
                    "settings.media_title must be text of 1 to {MAX_TITLE} characters with no line break"
                ),
            ))
        }
    }
    let document = payload.media.iter().any(|item| item.kind == "document");
    if !document {
        return Ok(None);
    }
    let line = payload.caption.lines().next().unwrap_or("").trim();
    let derived: String = line.chars().take(60).collect();
    Ok(Some(if derived.is_empty() {
        "Document".to_string()
    } else {
        derived
    }))
}

fn build(
    payload: &crate::wire::PublishPayload,
    remote_account: &str,
    mode: Mode<'_>,
) -> Result<Prepared, Fail> {
    let author = author_of(remote_account, &payload.settings)?;
    let mut images: Vec<String> = Vec::new();
    match mode {
        #[cfg(test)]
        Mode::NoMedia => {
            if !payload.media.is_empty() {
                return Err((
                    "media_unsupported",
                    "a linkedin post with media needs the image upload step; remove the media from the entry"
                        .to_string(),
                ));
            }
        }
        Mode::Check => check_media(payload)?,
        Mode::Images(urns) => {
            check_media(payload)?;
            let usable = urns.len() == payload.media.len()
                && urns
                    .iter()
                    .zip(&payload.media)
                    .all(|(urn, item)| valid_media_urn(&item.kind, urn));
            if !usable {
                return Err((
                    "media_missing",
                    format!(
                        "the job has {} media items but {} usable media URNs of the right kind; nothing was posted",
                        payload.media.len(),
                        urns.len()
                    ),
                ));
            }
            images = urns.to_vec();
        }
    }
    match payload.settings.get("visibility") {
        None | Some(Value::Null) => {}
        Some(Value::String(v)) if v == "PUBLIC" => {}
        Some(_) => return Err((
            "bad_input",
            "settings.visibility must be \"PUBLIC\": it is the only visibility this client sends"
                .to_string(),
        )),
    }
    let text = payload.caption.clone();
    if text.trim().is_empty() {
        return Err((
            "caption_empty",
            "the linkedin post text is empty".to_string(),
        ));
    }
    if let Some(found) = text.chars().find(|c| RESERVED.contains(*c)) {
        return Err((
            "text_unsupported",
            format!(
                "the post text holds {found:?}, which LinkedIn reads as markup; escaping is not implemented, so nothing was posted. Remove it from the text (reserved: {RESERVED})"
            ),
        ));
    }
    let count = counted_length(&text);
    if count > COMMENTARY_LIMIT {
        return Err((
            "text_too_long",
            format!(
                "the post text counts {count} characters and LinkedIn allows {COMMENTARY_LIMIT}"
            ),
        ));
    }
    let title = media_title(payload)?;
    Ok(Prepared {
        author,
        text,
        images,
        title,
    })
}

/// Validate a text-only payload and build the post. Refused before any request: media (use [`prepare_with`]
/// once the images are uploaded), a `settings.visibility` other than `PUBLIC` (the only visibility sent), a bad
/// `settings.author_organization`, an empty text, a text holding a markup character (see `RESERVED`), a text
/// over [`COMMENTARY_LIMIT`], and an account that is not a member id.
#[cfg(test)]
pub fn prepare(
    payload: &crate::wire::PublishPayload,
    remote_account: &str,
) -> Result<Prepared, Fail> {
    build(payload, remote_account, Mode::NoMedia)
}

/// [`prepare`] for a payload whose images are uploaded: `images` holds one image URN per media item, in entry
/// order. A count that differs from the payload's media is `media_missing`, so an image is never dropped.
pub fn prepare_with(
    payload: &crate::wire::PublishPayload,
    remote_account: &str,
    images: &[String],
) -> Result<Prepared, Fail> {
    build(payload, remote_account, Mode::Images(images))
}

/// `publish.enqueue` check: the payload and account must be ones this module can post (images are checked for
/// kind and URL; nothing is uploaded yet).
pub fn check(payload: &crate::wire::PublishPayload, remote_account: &str) -> Result<(), Fail> {
    build(payload, remote_account, Mode::Check).map(|_| ())
}

/// The owner URN an image upload is initialised for: the same author the post will have.
pub fn owner(payload: &crate::wire::PublishPayload, remote_account: &str) -> Result<String, Fail> {
    author_of(remote_account, &payload.settings)
}

/// A media URN is used in a request body and compared as text: the given `urn:li:<kind>:` prefix and plain
/// characters only.
fn valid_urn_with(prefix: &str, urn: &str) -> bool {
    urn.len() <= MAX_URN
        && urn.strip_prefix(prefix).is_some_and(|id| {
            !id.is_empty()
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
}

fn valid_image_urn(urn: &str) -> bool {
    valid_urn_with("urn:li:image:", urn)
}

/// True when `urn` is a usable URN for a media item of this kind (`image`, `video` or `document`).
fn valid_media_urn(kind: &str, urn: &str) -> bool {
    match kind {
        "image" => valid_image_urn(urn),
        "video" => valid_urn_with("urn:li:video:", urn),
        "document" => valid_urn_with("urn:li:document:", urn),
        _ => false,
    }
}

/// `POST /rest/images?action=initializeUpload` (UNVERIFIED path).
pub fn init_upload_path() -> String {
    format!("{IMAGES_PATH}?{INIT_QUERY}")
}

/// The initialise body for an owner URN (UNVERIFIED shape).
pub fn init_upload_body(owner: &str) -> Value {
    json!({ "initializeUploadRequest": { "owner": owner } })
}

/// `(uploadUrl, urn)` of an initialise answer whose URN is in `field` with the given prefix, or None when either
/// is missing or odd.
fn parse_init_field(body: &Value, field: &str, prefix: &str) -> Option<(String, String)> {
    let value = body.get("value")?;
    let url = value.get("uploadUrl")?.as_str()?;
    let urn = value.get(field)?.as_str()?;
    (crate::media::plain_url(url) && valid_urn_with(prefix, urn))
        .then(|| (url.to_string(), urn.to_string()))
}

/// `(uploadUrl, image URN)` of an initialise answer, or None when either is missing or odd.
#[cfg(test)]
pub fn parse_init(body: &Value) -> Option<(String, String)> {
    parse_init_field(body, "image", "urn:li:image:")
}

/// The image URN of an [`upload_image`] answer (`{"image": urn}`).
pub fn image_urn(body: &Value) -> Option<String> {
    let urn = body.get("image")?.as_str()?;
    valid_image_urn(urn).then(|| urn.to_string())
}

/// The document URN of an [`upload_document`] answer (`{"document": urn}`).
pub fn document_urn(body: &Value) -> Option<String> {
    let urn = body.get("document")?.as_str()?;
    valid_media_urn("document", urn).then(|| urn.to_string())
}

/// The video URN of an [`upload_video`] answer (`{"video": urn}`).
pub fn video_urn(body: &Value) -> Option<String> {
    let urn = body.get("video")?.as_str()?;
    valid_media_urn("video", urn).then(|| urn.to_string())
}

/// The URN of an upload answer for a media kind (`image`, `video` or `document`).
pub fn media_urn(kind: &str, body: &Value) -> Option<String> {
    match kind {
        "image" => image_urn(body),
        "video" => video_urn(body),
        "document" => document_urn(body),
        _ => None,
    }
}

/// `scheme://authority`, lowercased, of an http(s) URL.
fn origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    (!authority.is_empty()).then(|| {
        format!(
            "{}://{}",
            scheme.to_ascii_lowercase(),
            authority.to_ascii_lowercase()
        )
    })
}

/// Whether the bearer token may go to `upload_url`: `Some(true)` on the platform's own origin (`base`) or on
/// https linkedin.com or a subdomain, `Some(false)` for another https host (the bytes are sent without the
/// token), `None` (refused) for any other scheme on another origin. The upload URL is named by the answer of
/// the platform, so the token never follows it to a host that is not LinkedIn's.
pub fn upload_token_policy(base: &str, upload_url: &str) -> Option<bool> {
    let target = origin(upload_url)?;
    if origin(base).is_some_and(|own| own == target) {
        return Some(true);
    }
    let rest = target.strip_prefix("https://")?;
    let host = rest.split(':').next().unwrap_or(rest);
    Some(host == "linkedin.com" || host.ends_with(".linkedin.com"))
}

/// Upload one image: initialise, then PUT the bytes. `init` sends the initialise request (path and JSON body)
/// and `put` sends the file to the URL, with `true` when the bearer token may be sent (see
/// [`upload_token_policy`]). Returns the final answer and the number of requests made BEYOND the first (the
/// caller counts the first itself): `Reply::Ok({"image": urn})` on success, the failing reply when a request is
/// not 2xx, and `Reply::Ok({})` (an answer with no usable reference, which the caller refuses as
/// `bad_response`) when the initialise answer is unusable or names an upload URL the token may not follow.
pub fn upload_image(
    owner: &str,
    base: &str,
    init: impl FnOnce(&str, &Value) -> Reply,
    put: impl FnOnce(&str, bool) -> Reply,
) -> (Reply, i64) {
    upload_single(
        (init_upload_path().as_str(), "image", "urn:li:image:"),
        owner,
        base,
        init,
        put,
    )
}

/// Upload one document: `POST /rest/documents?action=initializeUpload` (UNVERIFIED), then one PUT of the file.
/// Same contract as [`upload_image`], answering `{"document": urn}`.
pub fn upload_document(
    owner: &str,
    base: &str,
    init: impl FnOnce(&str, &Value) -> Reply,
    put: impl FnOnce(&str, bool) -> Reply,
) -> (Reply, i64) {
    upload_single(
        (
            format!("{DOCUMENTS_PATH}?{INIT_QUERY}").as_str(),
            "document",
            "urn:li:document:",
        ),
        owner,
        base,
        init,
        put,
    )
}

/// The shared initialise-then-one-PUT upload of an image or a document. `kind` is
/// `(initialise path, answer field, URN prefix)`.
fn upload_single(
    kind: (&str, &str, &str),
    owner: &str,
    base: &str,
    init: impl FnOnce(&str, &Value) -> Reply,
    put: impl FnOnce(&str, bool) -> Reply,
) -> (Reply, i64) {
    let (path, field, prefix) = kind;
    let body = match init(path, &init_upload_body(owner)) {
        Reply::Ok(body) => body,
        other => return (other, 0),
    };
    let Some((url, urn)) = parse_init_field(&body, field, prefix) else {
        return (Reply::Ok(json!({})), 0);
    };
    let Some(send_token) = upload_token_policy(base, &url) else {
        return (Reply::Ok(json!({})), 0);
    };
    match put(&url, send_token) {
        Reply::Ok(_) => {
            let mut answer = serde_json::Map::new();
            answer.insert(field.to_string(), Value::String(urn));
            (Reply::Ok(Value::Object(answer)), 1)
        }
        other => (other, 1),
    }
}

/// One part of a video upload: the URL to PUT and the inclusive byte range it takes.
#[derive(Debug, PartialEq, Eq)]
struct Part {
    url: String,
    first: usize,
    last: usize,
}

/// `POST /rest/videos?action=initializeUpload` (UNVERIFIED path).
pub fn video_init_path() -> String {
    format!("{VIDEOS_PATH}?{INIT_QUERY}")
}

/// The video initialise body: the owner and the exact file size (UNVERIFIED field names).
pub fn video_init_body(owner: &str, size: usize) -> Value {
    json!({ "initializeUploadRequest": {
        "owner": owner,
        "fileSizeBytes": size,
        "uploadCaptions": false,
        "uploadThumbnail": false
    } })
}

/// `POST /rest/videos?action=finalizeUpload` (UNVERIFIED path).
fn video_finalize_path() -> String {
    format!("{VIDEOS_PATH}?{FINALIZE_QUERY}")
}

/// The finalize body: the video URN, the upload token of the initialise answer and the part ETags in order
/// (UNVERIFIED field names).
fn video_finalize_body(video: &str, token: &str, etags: &[String]) -> Value {
    json!({ "finalizeUploadRequest": {
        "video": video,
        "uploadToken": token,
        "uploadedPartIds": etags
    } })
}

/// `(video URN, upload token, parts)` of a video initialise answer for a file of `size` bytes. The parts must
/// be 1 to `MAX_PARTS`, each with a plain URL, and cover bytes `0..size` in order without a gap or overlap;
/// anything else is None (no usable answer), so a malformed range is never followed.
fn parse_video_init(body: &Value, size: usize) -> Option<(String, String, Vec<Part>)> {
    let value = body.get("value")?;
    let video = value.get("video")?.as_str()?;
    if !valid_media_urn("video", video) {
        return None;
    }
    let token = match value.get("uploadToken") {
        None | Some(Value::Null) => "",
        Some(Value::String(t)) if t.len() <= 4_096 && !t.chars().any(char::is_control) => {
            t.as_str()
        }
        Some(_) => return None,
    };
    let list = value.get("uploadInstructions")?.as_array()?;
    if list.is_empty() || list.len() > MAX_PARTS {
        return None;
    }
    let mut parts = Vec::with_capacity(list.len());
    let mut next = 0usize;
    for entry in list {
        let url = entry.get("uploadUrl")?.as_str()?;
        let first = usize::try_from(entry.get("firstByte")?.as_u64()?).ok()?;
        let last = usize::try_from(entry.get("lastByte")?.as_u64()?).ok()?;
        if !crate::media::plain_url(url) || first != next || last < first || last >= size {
            return None;
        }
        next = last + 1;
        parts.push(Part {
            url: url.to_string(),
            first,
            last,
        });
    }
    (next == size).then(|| (video.to_string(), token.to_string(), parts))
}

/// A part ETag read from the PUT answer's echoed `etag` header: 1 to 256 characters, no control character.
fn part_etag(body: &Value) -> Option<String> {
    let etag = body.get("headers")?.get("etag")?.as_str()?;
    (!etag.is_empty() && etag.len() <= 256 && !etag.chars().any(char::is_control))
        .then(|| etag.to_string())
}

/// Upload one video of `size` bytes: initialise (UNVERIFIED shapes), PUT each part named by the answer in order
/// (`put(url, with_token, first, last)` sends bytes `first..=last` and must echo the `etag` response header),
/// then finalize with the video URN, the upload token and the ETags. `init` sends a JSON POST (path and body) and
/// is used for the initialise and the finalize. Returns the final answer and the number of requests made BEYOND
/// the first: `Reply::Ok({"video": urn})` on success, the failing reply when a request is not 2xx, and
/// `Reply::Ok({})` (no usable reference, refused by the caller as `bad_response`) when the initialise answer is
/// unusable, names an upload URL the token may not follow, or a part answers without an ETag. A failure after
/// the initialise leaves an unfinished upload on LinkedIn's side, which is harmless; the retry starts a new one.
pub fn upload_video(
    owner: &str,
    size: usize,
    base: &str,
    mut init: impl FnMut(&str, &Value) -> Reply,
    mut put: impl FnMut(&str, bool, usize, usize) -> Reply,
) -> (Reply, i64) {
    let body = match init(&video_init_path(), &video_init_body(owner, size)) {
        Reply::Ok(body) => body,
        other => return (other, 0),
    };
    let Some((video, token, parts)) = parse_video_init(&body, size) else {
        return (Reply::Ok(json!({})), 0);
    };
    let mut policies = Vec::with_capacity(parts.len());
    for part in &parts {
        match upload_token_policy(base, &part.url) {
            Some(send_token) => policies.push(send_token),
            None => return (Reply::Ok(json!({})), 0),
        }
    }
    let mut extra = 0;
    let mut etags: Vec<String> = Vec::with_capacity(parts.len());
    for (part, send_token) in parts.iter().zip(policies) {
        extra += 1;
        match put(&part.url, send_token, part.first, part.last) {
            Reply::Ok(answer) => match part_etag(&answer) {
                Some(etag) => etags.push(etag),
                None => return (Reply::Ok(json!({})), extra),
            },
            other => return (other, extra),
        }
    }
    extra += 1;
    match init(
        &video_finalize_path(),
        &video_finalize_body(&video, &token, &etags),
    ) {
        Reply::Ok(_) => (Reply::Ok(json!({ "video": video })), extra),
        other => (other, extra),
    }
}

/// Sniff and size-check a video or document file before it is uploaded. `kind` is `video` or `document`. The
/// refusals are typed: `media_too_small`, `media_too_large`, `media_invalid` (not an MP4, or not a PDF, DOC(X) or
/// PPT(X) by its magic bytes) and `media_too_many_pages` (a PDF with more than [`DOCUMENT_MAX_PAGES`] pages by
/// the estimate in the module comment).
pub fn check_file(kind: &str, bytes: &[u8]) -> Result<(), Fail> {
    match kind {
        "video" => {
            if bytes.len() < VIDEO_MIN_BYTES {
                return Err((
                    "media_too_small",
                    format!(
                        "the video is {} bytes and LinkedIn takes {VIDEO_MIN_BYTES} to {VIDEO_MAX_BYTES} bytes; nothing was posted",
                        bytes.len()
                    ),
                ));
            }
            if bytes.len() > VIDEO_MAX_BYTES {
                return Err((
                    "media_too_large",
                    format!(
                        "the video is {} bytes and LinkedIn takes {VIDEO_MIN_BYTES} to {VIDEO_MAX_BYTES} bytes; nothing was posted",
                        bytes.len()
                    ),
                ));
            }
            if super::youtube::sniff_video(bytes) != Some("video/mp4") {
                return Err((
                    "media_invalid",
                    "the file is not an MP4 video by its magic bytes; nothing was posted"
                        .to_string(),
                ));
            }
            Ok(())
        }
        "document" => {
            if bytes.len() > DOCUMENT_MAX_BYTES {
                return Err((
                    "media_too_large",
                    format!(
                        "the document is {} bytes and LinkedIn takes at most {DOCUMENT_MAX_BYTES}; nothing was posted",
                        bytes.len()
                    ),
                ));
            }
            if bytes.starts_with(b"%PDF-") {
                let pages = pdf_page_count(bytes);
                if pages > DOCUMENT_MAX_PAGES {
                    return Err((
                        "media_too_many_pages",
                        format!(
                            "the PDF has about {pages} pages and LinkedIn takes at most {DOCUMENT_MAX_PAGES}; nothing was posted"
                        ),
                    ));
                }
                return Ok(());
            }
            // OOXML (DOCX, PPTX) is a ZIP; legacy DOC and PPT are an OLE compound file.
            let zip = bytes.starts_with(b"PK\x03\x04");
            let ole = bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]);
            if zip || ole {
                Ok(())
            } else {
                Err((
                    "media_invalid",
                    "the file is not a PDF, DOC, DOCX, PPT or PPTX document by its magic bytes; nothing was posted"
                        .to_string(),
                ))
            }
        }
        other => Err((
            "media_unsupported",
            format!("a {other} is uploaded as an image, not as a file"),
        )),
    }
}

/// The number of `/Type /Page` objects in a PDF (not `/Type /Pages`), with or without the space. An estimate:
/// page objects inside compressed object streams are not seen.
fn pdf_page_count(bytes: &[u8]) -> usize {
    const KEY: &[u8] = b"/Type";
    const PAGE: &[u8] = b"/Page";
    let mut count = 0;
    let mut at = 0;
    while let Some(found) = bytes[at..].windows(KEY.len()).position(|w| w == KEY) {
        let mut i = at + found + KEY.len();
        while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        if bytes[i.min(bytes.len())..].starts_with(PAGE) {
            let after = bytes.get(i + PAGE.len()).copied().unwrap_or(b' ');
            if !after.is_ascii_alphabetic() {
                count += 1;
            }
        }
        at += found + KEY.len();
    }
    count
}

/// A post URN is used as a URL path segment and compared as text, so only `urn:li:share:` and
/// `urn:li:ugcPost:` followed by plain characters are accepted.
fn valid_post_urn(urn: &str) -> bool {
    let id = urn
        .strip_prefix("urn:li:share:")
        .or_else(|| urn.strip_prefix("urn:li:ugcPost:"));
    urn.len() <= MAX_URN
        && id.is_some_and(|id| {
            !id.is_empty()
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
}

/// The post URN from a create answer: the `x-restli-id` response header (`http::send_with` puts it under
/// `headers`). Anything that is not a share or ugcPost URN of plain characters is not accepted as this
/// job's post.
pub fn created_post_id(body: &Value) -> Option<String> {
    let urn = body.get("headers")?.get("x-restli-id")?.as_str()?;
    valid_post_urn(urn).then(|| urn.to_string())
}

/// The `GET /rest/posts/<urn>` path that reads back a stored post URN (percent-encoded). None when the
/// stored value is not a post URN, so nothing odd is ever put in a path.
pub fn read_path(post_id: &str) -> Option<String> {
    valid_post_urn(post_id).then(|| format!("{POSTS_PATH}/{}", percent_encode(post_id)))
}

/// The read-back `commentary`, once the read has confirmed the stored post: the answer's `id` must be
/// the stored URN.
pub fn confirmed_commentary<'a>(post_id: &str, body: &'a Value) -> Option<&'a str> {
    if body.get("id").and_then(Value::as_str) != Some(post_id) {
        return None;
    }
    body.get("commentary").and_then(Value::as_str)
}

/// The permalink, once the read has confirmed the stored post: the answer's `id` must be the stored
/// URN, and its `lifecycleState`, when present, `PUBLISHED`. The form
/// `https://www.linkedin.com/feed/update/<urn>/` is conventional and not verified.
pub fn permalink_from(post_id: Option<&str>, body: &Value) -> Option<String> {
    let id = post_id?;
    if !valid_post_urn(id) || body.get("id").and_then(Value::as_str) != Some(id) {
        return None;
    }
    match body.get("lifecycleState").and_then(Value::as_str) {
        None | Some("PUBLISHED") => {}
        Some(_) => return None,
    }
    Some(format!("https://www.linkedin.com/feed/update/{id}/"))
}

/// True when a refusal says the `LinkedIn-Version` is no longer supported: HTTP 426, or another 4xx
/// whose message names the version as not active, unsupported or sunset. A false positive only changes
/// the error code of a job that would fail `platform_rejected` anyway: both are final.
pub fn is_version_rejection(status: u16, message: &str) -> bool {
    if status == 426 {
        return true;
    }
    if !(400..=499).contains(&status) {
        return false;
    }
    let lower = message.to_ascii_lowercase();
    lower.contains("nonexistent_version")
        || (lower.contains("version")
            && [
                "not active",
                "not supported",
                "unsupported",
                "no longer",
                "sunset",
                "deprecated",
            ]
            .iter()
            .any(|w| lower.contains(w)))
}

/// The message of a version rejection: it names the version that was sent and points at the registry.
/// The version is never changed automatically.
pub fn version_rejection(status: u16, message: &str) -> String {
    let version = api_version().unwrap_or("(none pinned)");
    format!(
        "LinkedIn no longer supports LinkedIn-Version {version} (HTTP {status}: {message}); nothing was posted and the job is not retried. The version is never changed automatically: read LinkedIn's versioning page, then update the LinkedIn entry in the sourced platform-rule catalog (rightkit-social-publish api_versions)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::PublishPayload;

    fn payload(caption: &str, settings: Value) -> PublishPayload {
        PublishPayload {
            caption: caption.to_string(),
            format: "post".to_string(),
            media: Vec::new(),
            settings,
        }
    }

    const ACCOUNT: &str = "acct-alpha-linkedin-0001";

    #[test]
    fn the_body_is_the_documented_member_post() {
        let prepared = prepare(&payload("Hello LinkedIn", json!({})), ACCOUNT).expect("prepared");
        assert_eq!(
            prepared.body(),
            json!({
                "author": "urn:li:person:acct-alpha-linkedin-0001",
                "commentary": "Hello LinkedIn",
                "visibility": "PUBLIC",
                "distribution": {
                    "feedDistribution": "MAIN_FEED",
                    "targetEntities": [],
                    "thirdPartyDistributionChannels": []
                },
                "lifecycleState": "PUBLISHED",
                "isReshareDisabledByAuthor": false
            })
        );
        // A null settings value (a payload read before its JSON is restored) and PUBLIC are accepted.
        assert!(prepare(&payload("x", Value::Null), ACCOUNT).is_ok());
        assert!(prepare(&payload("x", json!({ "visibility": "PUBLIC" })), ACCOUNT).is_ok());
    }

    #[test]
    fn the_author_comes_only_from_the_stored_account() {
        let prepared = prepare(
            &payload("text", json!({ "author": "urn:li:person:evil" })),
            "m-1_X",
        )
        .expect("prepared");
        assert_eq!(prepared.body()["author"], "urn:li:person:m-1_X");
        let too_long = "a".repeat(65);
        for bad in [
            "",
            "a b",
            "urn:li:person:x",
            "a/b",
            "a:b",
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
        assert!(matches!(
            prepare(&with_media, ACCOUNT),
            Err(("media_unsupported", _))
        ));
        for settings in [
            json!({ "visibility": "CONNECTIONS" }),
            json!({ "visibility": "public" }),
            json!({ "visibility": 1 }),
        ] {
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
        for text in [
            "see (below)",
            "#launch",
            "a_b",
            "50% * 2",
            "x ~ y",
            "a|b",
            "<b>",
            "{x}",
            "@me",
            "[x]",
            "back\\slash",
        ] {
            assert!(
                matches!(
                    prepare(&payload(text, json!({})), ACCOUNT),
                    Err(("text_unsupported", _))
                ),
                "{text:?} should be refused"
            );
        }
    }

    #[test]
    fn the_limit_counts_utf16_units_and_is_exact() {
        assert!(prepare(&payload(&"a".repeat(3_000), json!({})), ACCOUNT).is_ok());
        let Err(err) = prepare(&payload(&"a".repeat(3_001), json!({})), ACCOUNT) else {
            panic!("3001 characters must be refused");
        };
        assert_eq!(err.0, "text_too_long");
        assert!(err.1.contains("counts 3001 characters"));
        assert!(err.1.contains("allows 3000"));
        // An astral character is two code units: 1500 of them are exactly 3000, one more unit is over.
        assert_eq!(counted_length("\u{1f600}"), 2);
        assert_eq!(counted_length("caf\u{e9}"), 4);
        let emoji = "\u{1f600}".repeat(1_500);
        assert!(prepare(&payload(&emoji, json!({})), ACCOUNT).is_ok());
        let over = format!("{emoji}a");
        assert!(matches!(
            prepare(&payload(&over, json!({})), ACCOUNT),
            Err(("text_too_long", _))
        ));
    }

    #[test]
    fn the_version_comes_from_the_registry() {
        let pinned = api_versions::pinned(PLATFORM).expect("linkedin is pinned");
        assert_eq!(api_version(), Ok(pinned.version));
        let headers = request_headers(pinned.version);
        assert_eq!(headers[0], ("LinkedIn-Version", pinned.version));
        assert_eq!(headers[1], ("X-Restli-Protocol-Version", "2.0.0"));
    }

    #[test]
    fn answers_are_matched_to_the_stored_post() {
        let created = json!({ "headers": { "x-restli-id": "urn:li:share:7000000001" } });
        assert_eq!(
            created_post_id(&created).as_deref(),
            Some("urn:li:share:7000000001")
        );
        let ugc = json!({ "headers": { "x-restli-id": "urn:li:ugcPost:6844785523593134080" } });
        assert_eq!(
            created_post_id(&ugc).as_deref(),
            Some("urn:li:ugcPost:6844785523593134080")
        );
        assert_eq!(created_post_id(&json!({})), None);
        assert_eq!(created_post_id(&json!({ "id": "urn:li:share:1" })), None);
        for bad in [
            "",
            "7000000001",
            "urn:li:person:1",
            "urn:li:share:",
            "urn:li:share:../x",
            "urn:li:share:a b",
        ] {
            assert_eq!(
                created_post_id(&json!({ "headers": { "x-restli-id": bad } })),
                None,
                "{bad:?}"
            );
        }
        assert_eq!(
            read_path("urn:li:share:7000000001").as_deref(),
            Some("/rest/posts/urn%3Ali%3Ashare%3A7000000001")
        );
        assert_eq!(read_path("urn:li:person:1"), None);

        let read = json!({
            "id": "urn:li:share:7000000001",
            "commentary": "Hello",
            "lifecycleState": "PUBLISHED"
        });
        assert_eq!(
            permalink_from(Some("urn:li:share:7000000001"), &read).as_deref(),
            Some("https://www.linkedin.com/feed/update/urn:li:share:7000000001/")
        );
        assert_eq!(permalink_from(Some("urn:li:share:9"), &read), None);
        assert_eq!(permalink_from(None, &read), None);
        let draft = json!({ "id": "urn:li:share:7000000001", "lifecycleState": "DRAFT" });
        assert_eq!(
            permalink_from(Some("urn:li:share:7000000001"), &draft),
            None
        );
        // No lifecycleState in the answer is accepted (the id match is the proof).
        let bare = json!({ "id": "urn:li:share:7000000001" });
        assert!(permalink_from(Some("urn:li:share:7000000001"), &bare).is_some());

        assert_eq!(
            confirmed_commentary("urn:li:share:7000000001", &read),
            Some("Hello")
        );
        assert_eq!(confirmed_commentary("urn:li:share:9", &read), None);
    }

    #[test]
    fn a_version_rejection_is_recognised_and_names_the_version() {
        assert!(is_version_rejection(426, ""));
        assert!(is_version_rejection(
            400,
            "{\"code\":\"NONEXISTENT_VERSION\",\"message\":\"Requested version 202301 is not active\"}"
        ));
        assert!(is_version_rejection(
            404,
            "This version is no longer supported"
        ));
        assert!(!is_version_rejection(422, "commentary is required"));
        assert!(!is_version_rejection(
            403,
            "Not enough permissions to access this resource"
        ));
        assert!(!is_version_rejection(500, "version not active"));
        let message = version_rejection(426, "gone");
        let pinned = api_versions::pinned(PLATFORM).expect("pinned");
        assert!(message.contains(pinned.version));
        assert!(message.contains("rightkit-social-publish api_versions"));
        assert!(message.contains("never changed automatically"));
    }

    fn image_item(hash: &str, url: Option<&str>) -> crate::wire::PublishMedia {
        crate::wire::PublishMedia {
            hash: hash.to_string(),
            kind: "image".to_string(),
            duration_ms: None,
            url: url.map(str::to_string),
        }
    }

    #[test]
    fn an_organisation_author_comes_from_the_setting_and_only_digits() {
        let org = prepare(
            &payload("text", json!({ "author_organization": "1000001" })),
            ACCOUNT,
        )
        .expect("prepared");
        assert_eq!(org.author(), "urn:li:organization:1000001");
        assert_eq!(org.body()["author"], "urn:li:organization:1000001");
        // The member account is still validated, and the setting never reaches the URN as text.
        assert!(matches!(
            prepare(
                &payload("text", json!({ "author_organization": "1000001" })),
                "a b"
            ),
            Err(("bad_input", _))
        ));
        let too_long = "1".repeat(21);
        for bad in [
            json!("urn:li:organization:1"),
            json!("12a"),
            json!(""),
            json!(12),
            json!(too_long),
        ] {
            assert!(
                matches!(
                    prepare(
                        &payload("text", json!({ "author_organization": bad.clone() })),
                        ACCOUNT
                    ),
                    Err(("bad_input", _))
                ),
                "{bad:?} should be refused"
            );
        }
        assert_eq!(
            owner(
                &payload("t", json!({ "author_organization": "7" })),
                ACCOUNT
            )
            .as_deref(),
            Ok("urn:li:organization:7")
        );
        assert_eq!(
            owner(&payload("t", json!({})), ACCOUNT).as_deref(),
            Ok("urn:li:person:acct-alpha-linkedin-0001")
        );
    }

    #[test]
    fn images_are_checked_at_enqueue_and_sent_as_content() {
        let mut one = payload("text", json!({}));
        one.media
            .push(image_item("sha256:a", Some("https://m.example/a.png")));
        assert!(check(&one, ACCOUNT).is_ok());
        // The text-only builder still refuses media; the media builder needs one URN per item.
        assert!(matches!(
            prepare(&one, ACCOUNT),
            Err(("media_unsupported", _))
        ));
        assert!(matches!(
            prepare_with(&one, ACCOUNT, &[]),
            Err(("media_missing", _))
        ));
        assert!(matches!(
            prepare_with(&one, ACCOUNT, &["urn:li:person:x".to_string()]),
            Err(("media_missing", _))
        ));
        let single = prepare_with(&one, ACCOUNT, &["urn:li:image:C1".to_string()]).expect("one");
        assert_eq!(
            single.body()["content"],
            json!({ "media": { "id": "urn:li:image:C1" } })
        );
        let mut two = payload("text", json!({}));
        two.media
            .push(image_item("sha256:a", Some("https://m.example/a.png")));
        two.media
            .push(image_item("sha256:b", Some("https://m.example/b.png")));
        let many = prepare_with(
            &two,
            ACCOUNT,
            &["urn:li:image:C1".to_string(), "urn:li:image:C2".to_string()],
        )
        .expect("two");
        assert_eq!(
            many.body()["content"],
            json!({ "multiImage": { "images": [{ "id": "urn:li:image:C1" }, { "id": "urn:li:image:C2" }] } })
        );
        // A text post has no content member.
        assert!(prepare(&payload("text", json!({})), ACCOUNT)
            .expect("text")
            .body()
            .get("content")
            .is_none());

        let mut video = payload("text", json!({}));
        let mut item = image_item("sha256:v", Some("https://m.example/v.mp4"));
        item.kind = "video".to_string();
        video.media.push(item);
        assert!(check(&video, ACCOUNT).is_ok());
        let mut no_url = payload("text", json!({}));
        no_url.media.push(image_item("sha256:a", None));
        assert!(matches!(
            check(&no_url, ACCOUNT),
            Err(("media_required", _))
        ));
        let mut bad_url = payload("text", json!({}));
        bad_url
            .media
            .push(image_item("sha256:a", Some("ftp://m.example/a.png")));
        assert!(matches!(
            check(&bad_url, ACCOUNT),
            Err(("media_url_invalid", _))
        ));
        let mut many_items = payload("text", json!({}));
        for _ in 0..21 {
            many_items
                .media
                .push(image_item("sha256:a", Some("https://m.example/a.png")));
        }
        assert!(matches!(
            check(&many_items, ACCOUNT),
            Err(("media_unsupported", _))
        ));
    }

    #[test]
    fn the_upload_answers_are_read_strictly() {
        let init = json!({ "value": {
            "uploadUrl": "https://www.linkedin.com/dms-uploads/abc",
            "image": "urn:li:image:C4E10AQ"
        } });
        assert_eq!(
            parse_init(&init),
            Some((
                "https://www.linkedin.com/dms-uploads/abc".to_string(),
                "urn:li:image:C4E10AQ".to_string()
            ))
        );
        assert_eq!(parse_init(&json!({})), None);
        assert_eq!(
            parse_init(
                &json!({ "value": { "uploadUrl": "javascript:x", "image": "urn:li:image:C1" } })
            ),
            None
        );
        assert_eq!(
            parse_init(
                &json!({ "value": { "uploadUrl": "https://a.test/u", "image": "urn:li:share:1" } })
            ),
            None
        );
        assert_eq!(
            image_urn(&json!({ "image": "urn:li:image:C1" })).as_deref(),
            Some("urn:li:image:C1")
        );
        assert_eq!(image_urn(&json!({ "image": "urn:li:image:" })), None);
        assert_eq!(image_urn(&json!({})), None);
        assert_eq!(init_upload_path(), "/rest/images?action=initializeUpload");
        assert_eq!(
            init_upload_body("urn:li:person:x"),
            json!({ "initializeUploadRequest": { "owner": "urn:li:person:x" } })
        );
    }

    #[test]
    fn the_token_follows_an_upload_url_only_to_the_platform() {
        let base = "http://127.0.0.1:9000/linkedin/acct";
        assert_eq!(
            upload_token_policy(base, "http://127.0.0.1:9000/linkedin/acct/upload/1"),
            Some(true)
        );
        assert_eq!(
            upload_token_policy(base, "https://www.linkedin.com/dms/x"),
            Some(true)
        );
        assert_eq!(
            upload_token_policy(base, "https://linkedin.com/dms/x"),
            Some(true)
        );
        assert_eq!(
            upload_token_policy(base, "https://evil-linkedin.com/x"),
            Some(false)
        );
        assert_eq!(
            upload_token_policy(base, "https://linkedin.com.evil.test/x"),
            Some(false)
        );
        assert_eq!(
            upload_token_policy(base, "https://blob.example/x"),
            Some(false)
        );
        assert_eq!(upload_token_policy(base, "http://other.example/x"), None);
        assert_eq!(upload_token_policy(base, "http://127.0.0.1:9001/x"), None);
        assert_eq!(upload_token_policy(base, "nonsense"), None);
    }

    #[test]
    fn upload_image_initialises_then_puts_and_counts_the_extra_request() {
        let init_ok = || {
            Reply::Ok(json!({ "value": {
                "uploadUrl": "http://mock.test/linkedin/a/upload/1",
                "image": "urn:li:image:C1"
            } }))
        };
        let (reply, extra) = upload_image(
            "urn:li:person:x",
            "http://mock.test/linkedin/a",
            |path, body| {
                assert_eq!(path, "/rest/images?action=initializeUpload");
                assert_eq!(body["initializeUploadRequest"]["owner"], "urn:li:person:x");
                init_ok()
            },
            |url, token| {
                assert_eq!(url, "http://mock.test/linkedin/a/upload/1");
                assert!(token);
                Reply::Ok(Value::Null)
            },
        );
        assert_eq!(extra, 1);
        match reply {
            Reply::Ok(body) => assert_eq!(image_urn(&body).as_deref(), Some("urn:li:image:C1")),
            other => panic!("{other:?}"),
        }
        // A failed initialise stops before the PUT; a failed PUT is returned as it came.
        let (reply, extra) = upload_image(
            "urn:li:person:x",
            "http://mock.test/linkedin/a",
            |_, _| Reply::ServerError(503),
            |_, _| panic!("no put after a failed initialise"),
        );
        assert!(matches!(reply, Reply::ServerError(503)));
        assert_eq!(extra, 0);
        let (reply, extra) = upload_image(
            "urn:li:person:x",
            "http://mock.test/linkedin/a",
            |_, _| init_ok(),
            |_, _| Reply::Rejected {
                status: 400,
                message: "m".to_string(),
            },
        );
        assert!(matches!(reply, Reply::Rejected { .. }));
        assert_eq!(extra, 1);
        // An upload URL the token may not follow is no usable reference, and nothing is put.
        let (reply, extra) = upload_image(
            "urn:li:person:x",
            "http://mock.test/linkedin/a",
            |_, _| {
                Reply::Ok(
                    json!({ "value": { "uploadUrl": "http://other.example/u", "image": "urn:li:image:C1" } }),
                )
            },
            |_, _| panic!("no put to a foreign http host"),
        );
        assert_eq!(extra, 0);
        match reply {
            Reply::Ok(body) => assert_eq!(image_urn(&body), None),
            other => panic!("{other:?}"),
        }
    }

    fn file_item(kind: &str, url: &str, duration_ms: Option<u64>) -> crate::wire::PublishMedia {
        crate::wire::PublishMedia {
            hash: "sha256:f".to_string(),
            kind: kind.to_string(),
            duration_ms,
            url: Some(url.to_string()),
        }
    }

    #[test]
    fn a_video_or_document_is_alone_and_within_its_limits() {
        let one = |item| {
            let mut p = payload("text", json!({}));
            p.media.push(item);
            p
        };
        assert!(check(
            &one(file_item("video", "https://m.example/v.mp4", Some(15_000))),
            ACCOUNT
        )
        .is_ok());
        assert!(check(
            &one(file_item("video", "https://m.example/v", None)),
            ACCOUNT
        )
        .is_ok());
        assert!(matches!(
            check(
                &one(file_item(
                    "video",
                    "https://m.example/v.mp4",
                    Some(1_800_001)
                )),
                ACCOUNT
            ),
            Err(("media_too_long", _))
        ));
        assert!(check(
            &one(file_item(
                "video",
                "https://m.example/v.mp4",
                Some(1_800_000)
            )),
            ACCOUNT
        )
        .is_ok());
        assert!(matches!(
            check(
                &one(file_item("video", "https://m.example/v.mp4", Some(2_999))),
                ACCOUNT
            ),
            Err(("media_too_short", _))
        ));
        assert!(matches!(
            check(
                &one(file_item("video", "https://m.example/v.mov", None)),
                ACCOUNT
            ),
            Err(("media_url_invalid", _))
        ));
        assert!(check(
            &one(file_item("document", "https://m.example/d.pdf", None)),
            ACCOUNT
        )
        .is_ok());
        assert!(matches!(
            check(
                &one(file_item("document", "https://m.example/d.png", None)),
                ACCOUNT
            ),
            Err(("media_url_invalid", _))
        ));
        assert!(matches!(
            check(
                &one(file_item("audio", "https://m.example/a.mp3", None)),
                ACCOUNT
            ),
            Err(("media_unsupported", _))
        ));
        let mut mixed = one(file_item("video", "https://m.example/v.mp4", None));
        mixed
            .media
            .push(image_item("sha256:a", Some("https://m.example/a.png")));
        assert!(matches!(
            check(&mixed, ACCOUNT),
            Err(("media_unsupported", _))
        ));
        let mut two_videos = one(file_item("video", "https://m.example/v.mp4", None));
        two_videos
            .media
            .push(file_item("video", "https://m.example/w.mp4", None));
        assert!(matches!(
            check(&two_videos, ACCOUNT),
            Err(("media_unsupported", _))
        ));
    }

    #[test]
    fn a_video_post_carries_the_video_urn_and_a_document_a_title() {
        let mut video = payload("text", json!({}));
        video
            .media
            .push(file_item("video", "https://m.example/v.mp4", None));
        // The wrong kind of URN is not accepted for a video.
        assert!(matches!(
            prepare_with(&video, ACCOUNT, &["urn:li:image:C1".to_string()]),
            Err(("media_missing", _))
        ));
        let posted =
            prepare_with(&video, ACCOUNT, &["urn:li:video:D1".to_string()]).expect("video");
        assert_eq!(
            posted.body()["content"],
            json!({ "media": { "id": "urn:li:video:D1" } })
        );
        let mut doc = payload("Quarterly deck\nmore", json!({}));
        doc.media
            .push(file_item("document", "https://m.example/d.pdf", None));
        let posted = prepare_with(&doc, ACCOUNT, &["urn:li:document:D1".to_string()]).expect("doc");
        assert_eq!(
            posted.body()["content"],
            json!({ "media": { "id": "urn:li:document:D1", "title": "Quarterly deck" } })
        );
        let mut titled = payload("text", json!({ "media_title": "Deck" }));
        titled
            .media
            .push(file_item("document", "https://m.example/d.pdf", None));
        let posted =
            prepare_with(&titled, ACCOUNT, &["urn:li:document:D1".to_string()]).expect("doc");
        assert_eq!(posted.body()["content"]["media"]["title"], "Deck");
        assert!(matches!(
            prepare(&payload("t", json!({ "media_title": 3 })), ACCOUNT),
            Err(("bad_input", _))
        ));
    }

    #[test]
    fn files_are_sniffed_and_sized_before_upload() {
        let mut mp4 = vec![0, 0, 0, 0x20];
        mp4.extend_from_slice(b"ftypisom");
        mp4.resize(VIDEO_MIN_BYTES, 1);
        assert!(check_file("video", &mp4).is_ok());
        assert!(matches!(
            check_file("video", &mp4[..1000]),
            Err(("media_too_small", _))
        ));
        let mut not_mp4 = b"\x89PNG\r\n\x1a\n".to_vec();
        not_mp4.resize(VIDEO_MIN_BYTES, 1);
        assert!(matches!(
            check_file("video", &not_mp4),
            Err(("media_invalid", _))
        ));
        let pdf = |pages: usize| {
            let mut bytes = b"%PDF-1.4\n1 0 obj << /Type /Pages >> endobj\n".to_vec();
            for _ in 0..pages {
                bytes.extend_from_slice(b"<< /Type /Page /Parent 1 0 R >>\n");
            }
            bytes
        };
        assert_eq!(pdf_page_count(&pdf(3)), 3);
        assert!(check_file("document", &pdf(300)).is_ok());
        assert!(matches!(
            check_file("document", &pdf(301)),
            Err(("media_too_many_pages", _))
        ));
        assert!(check_file("document", b"PK\x03\x04rest").is_ok());
        assert!(matches!(
            check_file("document", b"\x89PNG\r\n\x1a\n"),
            Err(("media_invalid", _))
        ));
    }

    #[test]
    fn upload_video_follows_the_named_ranges_and_finalizes_with_the_etags() {
        let init = json!({ "value": {
            "video": "urn:li:video:D1",
            "uploadToken": "tok",
            "uploadInstructions": [
                { "uploadUrl": "http://mock.test/linkedin/a/upload/V1-1", "firstByte": 0, "lastByte": 5 },
                { "uploadUrl": "http://mock.test/linkedin/a/upload/V1-2", "firstByte": 6, "lastByte": 9 }
            ]
        } });
        let mut posted: Vec<(String, Value)> = Vec::new();
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        let (reply, extra) = upload_video(
            "urn:li:person:x",
            10,
            "http://mock.test/linkedin/a",
            |path, body| {
                posted.push((path.to_string(), body.clone()));
                if path.contains("initializeUpload") {
                    Reply::Ok(init.clone())
                } else {
                    Reply::Ok(Value::Null)
                }
            },
            |_, token, first, last| {
                assert!(token);
                ranges.push((first, last));
                Reply::Ok(json!({ "headers": { "etag": format!("e{first}") } }))
            },
        );
        assert_eq!(extra, 3);
        assert_eq!(ranges, vec![(0, 5), (6, 9)]);
        match reply {
            Reply::Ok(body) => assert_eq!(video_urn(&body).as_deref(), Some("urn:li:video:D1")),
            other => panic!("{other:?}"),
        }
        assert_eq!(posted[0].0, "/rest/videos?action=initializeUpload");
        assert_eq!(posted[0].1["initializeUploadRequest"]["fileSizeBytes"], 10);
        assert_eq!(posted[1].0, "/rest/videos?action=finalizeUpload");
        assert_eq!(
            posted[1].1,
            json!({ "finalizeUploadRequest": { "video": "urn:li:video:D1", "uploadToken": "tok", "uploadedPartIds": ["e0", "e6"] } })
        );
        // A gap in the ranges, or a range past the file, is no usable answer.
        for bad in [
            json!([{ "uploadUrl": "http://a.test/u", "firstByte": 1, "lastByte": 9 }]),
            json!([{ "uploadUrl": "http://a.test/u", "firstByte": 0, "lastByte": 10 }]),
            json!([{ "uploadUrl": "http://a.test/u", "firstByte": 0, "lastByte": 4 }]),
            json!([]),
        ] {
            let body =
                json!({ "value": { "video": "urn:li:video:D1", "uploadInstructions": bad } });
            assert_eq!(parse_video_init(&body, 10), None);
        }
        // A part with no ETag stops the upload before the finalize.
        let (reply, extra) = upload_video(
            "urn:li:person:x",
            10,
            "http://mock.test/linkedin/a",
            |path, _| {
                assert!(
                    path.contains("initializeUpload"),
                    "no finalize without every ETag"
                );
                Reply::Ok(init.clone())
            },
            |_, _, _, _| Reply::Ok(json!({})),
        );
        assert_eq!(extra, 1);
        match reply {
            Reply::Ok(body) => assert_eq!(video_urn(&body), None),
            other => panic!("{other:?}"),
        }
    }
}
