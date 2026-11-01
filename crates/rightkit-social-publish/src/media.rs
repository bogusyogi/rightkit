//! Image media for the publish core: what is checked at enqueue, what is read from a media URL, and how the
//! uploaded references are kept on the job.
//!
//! Three platforms take images here: Bluesky (a blob upload), Mastodon (a media attachment) and X (a media
//! upload). The shape is the same for all of them:
//!
//! 1. The entry's media items are `{hash, kind}`; the URL for a hash comes from the enqueue input
//!    `media_urls` and is stored in the job's payload. Enqueue refuses, before any job or request exists,
//!    an item that is not an `image` (`media_unsupported`; video upload is not implemented), an item with
//!    no URL (`media_required`), a URL that is not a plain `http(s)` URL (`media_url_invalid`) and more than
//!    [`MAX_IMAGES`] items.
//! 2. The `create` step, when the job has media and no stored upload yet, makes one UPLOAD step first: for
//!    each item it fetches the file from its URL with a plain GET (no bearer token: the media host is not
//!    the platform), checks its size and its magic bytes, and uploads it to the platform. The platform's
//!    reference for each file is kept, as a JSON array, in the job's `remote.container_id` (no schema
//!    change). The step ends without a phase change; the next `create` step posts with the references.
//!    An upload only stages a file, so it is never `outcome_unknown`: a failure is retried like a read.
//! 3. The post refers to the stored references. A job whose stored count differs from its media count is
//!    refused (`media_missing`), so media is never dropped silently.
//!
//! Nothing here is verified against a live server; each provider file marks the endpoint details that no
//! fetched source states.
use crate::wire::PublishPayload;
use serde_json::Value;

use crate::Fail;

/// Most images one post carries on any of the three platforms (Bluesky's embed holds four; Mastodon's
/// default and X's limit are also four; the Mastodon instance's own value is unverified).
pub const MAX_IMAGES: usize = 4;
/// Longest media URL accepted.
const MAX_URL: usize = 2_048;

/// The largest file one image upload may carry, per platform. Bluesky: the `app.bsky.embed.images` lexicon
/// caps an image at 1,000,000 bytes (recalled from the lexicon, UNVERIFIED here). Mastodon: instance
/// dependent; 16 MiB is the usual image limit (UNVERIFIED). X: `tweet_image` is 5 MB
/// (docs.x.com/x-api/media/quickstart/media-upload-chunked, cited in docs/p1/capability-matrix.md).
pub fn max_bytes(platform: &str) -> usize {
    match platform {
        "bluesky" => 1_000_000,
        "mastodon" => 16 * 1024 * 1024,
        "x" => 5 * 1024 * 1024,
        _ => 0,
    }
}

/// True for an `http://` or `https://` URL with a host, no user info, no whitespace or control characters
/// and at most `MAX_URL` characters.
pub fn plain_url(url: &str) -> bool {
    let rest = if let Some(rest) = url.strip_prefix("https://") {
        rest
    } else if let Some(rest) = url.strip_prefix("http://") {
        rest
    } else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    !authority.is_empty()
        && !authority.contains('@')
        && url.len() <= MAX_URL
        && !url
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
}

/// The enqueue-time rules for a job whose platform uploads images (see the module comment).
pub fn check_images(payload: &PublishPayload, platform: &str) -> Result<(), Fail> {
    if payload.media.len() > MAX_IMAGES {
        return Err((
            "media_unsupported",
            format!(
                "a {platform} post carries at most {MAX_IMAGES} images; the entry has {}",
                payload.media.len()
            ),
        ));
    }
    for item in &payload.media {
        if item.kind != "image" {
            return Err((
                "media_unsupported",
                format!(
                    "a {platform} post can carry images only; media item {} is a {}, and video upload is not implemented",
                    item.hash, item.kind
                ),
            ));
        }
        match item.url.as_deref() {
            None => {
                return Err((
                    "media_required",
                    format!(
                        "the {platform} media item {} has no URL: pass media_urls for its hash to publish.enqueue",
                        item.hash
                    ),
                ))
            }
            Some(url) if !plain_url(url) => {
                return Err((
                    "media_url_invalid",
                    format!(
                        "the media URL of item {} must be an http:// or https:// URL with a host, no user info and no spaces (at most {MAX_URL} characters)",
                        item.hash
                    ),
                ))
            }
            Some(_) => {}
        }
    }
    Ok(())
}

/// The image type of `bytes` by its magic bytes, as `(mime type, file extension)`. The type the media host
/// claims is never trusted. PNG, JPEG, GIF and WebP only.
pub fn sniff_image(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(("image/png", "png"))
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(("image/jpeg", "jpg"))
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(("image/gif", "gif"))
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(("image/webp", "webp"))
    } else {
        None
    }
}

/// A `multipart/form-data` body with the text `fields` first and then one file part. Returns the
/// `Content-Type` header value (with the boundary) and the body. The boundary is a fixed prefix plus a
/// counter, raised until it does not occur in the file, so the same input always gives the same bytes.
/// `file_name` and `mime` are written by the engine from the sniffed type, never from the media host, so
/// they hold no quote or line break.
pub fn multipart(
    fields: &[(&str, &str)],
    file_field: &str,
    file_name: &str,
    mime: &str,
    bytes: &[u8],
) -> (String, Vec<u8>) {
    let mut counter = 0u32;
    let boundary = loop {
        let candidate = format!("rightkit-boundary-{counter:08x}");
        if !bytes
            .windows(candidate.len())
            .any(|window| window == candidate.as_bytes())
        {
            break candidate;
        }
        counter += 1;
    };
    let mut form = rightkit_http::Multipart::with_boundary(boundary);
    for (name, value) in fields {
        form = form.text(name, value);
    }
    form = form.file_bytes(file_field, file_name, mime, bytes);
    let content_type = form.content_type();
    // The boundary is chosen above to be absent from the file and cannot be invalid, so encoding holds.
    let body = form
        .into_body()
        .expect("multipart boundary is valid and absent from every part");
    (content_type, body)
}

/// The stored upload references as the text kept in `remote.container_id`: a JSON array.
pub fn encode(items: &[Value]) -> String {
    Value::Array(items.to_vec()).to_string()
}

/// The stored upload references of a job whose payload has `expected` media items. No stored value and
/// `expected == 0` is an empty list (a text-only job). Any other count is `media_missing`, so a post is never
/// made with fewer attachments than the entry has.
pub fn stored(container_id: Option<&str>, expected: usize) -> Result<Vec<Value>, Fail> {
    let items: Vec<Value> = match container_id {
        None => Vec::new(),
        Some(text) => serde_json::from_str(text).map_err(|_| {
            (
                "media_missing",
                "the stored media references are not readable; nothing was posted".to_string(),
            )
        })?,
    };
    if items.len() == expected {
        Ok(items)
    } else {
        Err((
            "media_missing",
            format!(
                "the job has {expected} media items but {} stored uploads; nothing was posted",
                items.len()
            ),
        ))
    }
}

/// [`stored`] for the platforms whose reference is a plain string id (Mastodon, X).
pub fn stored_ids(container_id: Option<&str>, expected: usize) -> Result<Vec<String>, Fail> {
    let items = stored(container_id, expected)?;
    let ids: Vec<String> = items
        .iter()
        .filter_map(|item| item.as_str().map(str::to_string))
        .collect();
    if ids.len() == items.len() {
        Ok(ids)
    } else {
        Err((
            "media_missing",
            "a stored media reference is not an id; nothing was posted".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::PublishMedia;
    use serde_json::json;

    fn payload(media: Vec<PublishMedia>) -> PublishPayload {
        PublishPayload {
            caption: "text".to_string(),
            format: "post".to_string(),
            media,
            settings: json!({}),
        }
    }

    fn item(kind: &str, url: Option<&str>) -> PublishMedia {
        PublishMedia {
            hash: "sha256:abc".to_string(),
            kind: kind.to_string(),
            duration_ms: None,
            url: url.map(str::to_string),
        }
    }

    #[test]
    fn enqueue_rules() {
        let ok = payload(vec![item("image", Some("https://media.example/a.png"))]);
        assert!(check_images(&ok, "x").is_ok());
        assert!(matches!(
            check_images(
                &payload(vec![item("video", Some("https://m.example/a.mp4"))]),
                "x"
            ),
            Err(("media_unsupported", _))
        ));
        assert!(matches!(
            check_images(&payload(vec![item("image", None)]), "x"),
            Err(("media_required", _))
        ));
        assert!(matches!(
            check_images(
                &payload(vec![item("image", Some("ftp://m.example/a.png"))]),
                "x"
            ),
            Err(("media_url_invalid", _))
        ));
        assert!(matches!(
            check_images(
                &payload(vec![item("image", Some("https://u@m.example/a.png"))]),
                "x"
            ),
            Err(("media_url_invalid", _))
        ));
        let five = payload(
            (0..5)
                .map(|_| item("image", Some("https://m.example/a.png")))
                .collect(),
        );
        assert!(matches!(
            check_images(&five, "x"),
            Err(("media_unsupported", _))
        ));
    }

    #[test]
    fn sniffing_reads_magic_bytes_only() {
        assert_eq!(
            sniff_image(b"\x89PNG\r\n\x1a\n rest"),
            Some(("image/png", "png"))
        );
        assert_eq!(
            sniff_image(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some(("image/jpeg", "jpg"))
        );
        assert_eq!(sniff_image(b"GIF89a.."), Some(("image/gif", "gif")));
        assert_eq!(
            sniff_image(b"RIFF\0\0\0\0WEBPVP8 "),
            Some(("image/webp", "webp"))
        );
        assert_eq!(sniff_image(b"<html>"), None);
        assert_eq!(sniff_image(b""), None);
    }

    #[test]
    fn multipart_framing_is_exact() {
        let (content_type, body) = multipart(
            &[("media_category", "tweet_image")],
            "media",
            "media.png",
            "image/png",
            b"PNGDATA",
        );
        let boundary = content_type
            .strip_prefix("multipart/form-data; boundary=")
            .expect("boundary");
        let text = String::from_utf8(body).expect("utf8");
        let expected = format!(
            "--{b}\r\nContent-Disposition: form-data; name=\"media_category\"\r\n\r\ntweet_image\r\n--{b}\r\nContent-Disposition: form-data; name=\"media\"; filename=\"media.png\"\r\nContent-Type: image/png\r\n\r\nPNGDATA\r\n--{b}--\r\n",
            b = boundary
        );
        assert_eq!(text, expected);
    }

    #[test]
    fn stored_references_round_trip_and_count() {
        let items = vec![json!("a1"), json!("a2")];
        let text = encode(&items);
        assert_eq!(stored(Some(text.as_str()), 2).expect("two"), items);
        assert_eq!(
            stored_ids(Some(text.as_str()), 2).expect("ids"),
            vec!["a1".to_string(), "a2".to_string()]
        );
        assert!(matches!(
            stored(Some(text.as_str()), 1),
            Err(("media_missing", _))
        ));
        assert!(matches!(stored(None, 1), Err(("media_missing", _))));
        assert!(stored(None, 0).expect("text only").is_empty());
        assert!(matches!(
            stored(Some("not json"), 1),
            Err(("media_missing", _))
        ));
        assert!(matches!(
            stored_ids(Some(encode(&[json!({"a": 1})]).as_str()), 1),
            Err(("media_missing", _))
        ));
    }
}
