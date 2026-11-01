//! Mastodon request shapes for the publish core: a text status, written with `POST /api/v1/statuses` and
//! read back with `GET /api/v1/statuses/:id`.
//!
//! The phase machine is unchanged (`create -> read_permalink -> done`); this module supplies what is
//! Mastodon-specific: the request body, the text rules and the answer parsing.
//!
//! Idempotency. The create carries `Idempotency-Key: <the job's idempotency key>`. The API's contract is
//! that a repeated create with the same key returns the FIRST status instead of making another, so no id
//! has to be derived (compare Bluesky's record key). After an ambiguous answer the machine never repeats
//! the create blindly; `publish.reconcile` repeats it WITH THE SAME KEY (machine.rs `reconcile_mastodon`),
//! which is safe by that contract and is how the status id is learned when the answer was lost.
//!
//! The text limit is a property of the instance (`configuration.statuses.max_characters`), not a
//! constant: 500 is only the default. The machine reads `GET /api/v2/instance` before each create and
//! refuses locally, before any create, when the text is over what the instance reports. A job may carry
//! `settings.max_characters` instead, which skips that read (and is also enforced at enqueue).
//!
//! Images. A status with images first uploads each file with `POST /api/v2/media` (`multipart/form-data`,
//! the file in the `file` field; see `machine.rs` `media_upload` and `media.rs`), whose answer is a
//! MediaAttachment carrying the attachment's `id`. The ids are kept on the job and the create sends them as
//! `media_ids`. Taken from the published Mastodon API documentation, of which docs/p1/capability-matrix.md
//! cites only the statuses page (`media_ids[]`, `write:media` scope); UNVERIFIED here: the `/api/v2/media`
//! path and the `file` field name, the 16 MiB size limit and the four-attachment cap (an instance's own
//! `configuration.statuses.max_media_attachments` is not read), and that an upload answered `202` (still
//! processing) can be attached at once. No description (alt text) is sent: a calendar entry carries none.
//!
//! Secrets: the access token is the command's `token`, sent only as the bearer of a request and scrubbed
//! from every message by `http::send`. Nothing in this file builds a message from the token.
//!
//! Protocol facts used here come from the published Mastodon API documentation and were not verified
//! against a live server in this task; docs/PUBLISH-CORE.md lists what is left out because it is not known.
use rightkit_http::percent_encode;
use serde_json::{Map, Value};

use super::bluesky;
use crate::Fail;

/// The platform name this module drives.
pub const PLATFORM: &str = "mastodon";
/// The create request path, under the instance base URL.
pub const STATUSES_PATH: &str = "/api/v1/statuses";
/// The instance configuration path (the v2 instance document carries `configuration.statuses`).
pub const INSTANCE_PATH: &str = "/api/v2/instance";
/// The media upload path (see the module comment; unverified).
pub const MEDIA_PATH: &str = "/api/v2/media";
/// The account check path: the account the token belongs to.
pub const VERIFY_PATH: &str = "/api/v1/accounts/verify_credentials";

/// True when the job asks for the token's account to be checked against `remote_account` before the
/// create (`settings.verify_account`, a boolean). Opt-in so existing jobs make exactly the requests they did.
pub fn wants_verify(settings: &Value) -> bool {
    settings
        .get("verify_account")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// The account `id` of a `verify_credentials` answer: a plain id, else None.
pub fn account_id(body: &Value) -> Option<String> {
    let id = body.get("id").and_then(Value::as_str)?;
    plain_id(id).then(|| id.to_string())
}

/// Characters every URL counts as in Mastodon's counting, whatever its length.
pub const URL_WEIGHT: usize = 23;
/// A reported or configured limit above this is not believed.
const MAX_LIMIT: u64 = 1_000_000;
/// Seconds after the ambiguous answer within which a repeat of the create is still trusted to hit the
/// stored `Idempotency-Key`. The API documentation says keys are kept for about an hour (recalled, not
/// verified); this is a deliberately shorter window.
pub const KEY_WINDOW_SECS: i64 = 3_000;
const VISIBILITIES: [&str; 4] = ["public", "unlisted", "private", "direct"];

/// What a job posts, validated and ready to serialise.
pub struct Prepared {
    text: String,
    visibility: Option<String>,
    language: Option<String>,
    limit_override: Option<usize>,
}

impl Prepared {
    /// The characters this text counts as under Mastodon's rules (see [`counted_length`]).
    pub fn count(&self) -> usize {
        counted_length(&self.text)
    }

    /// The limit the job itself names (`settings.max_characters`), if any.
    pub fn limit_override(&self) -> Option<usize> {
        self.limit_override
    }

    /// `text_too_long` when the text counts more than `limit`; `source` says where the limit came from.
    pub fn within(&self, limit: usize, source: &str) -> Result<(), Fail> {
        let count = self.count();
        if count > limit {
            return Err((
                "text_too_long",
                format!(
                    "the status counts {count} characters (each URL counts {URL_WEIGHT}) and {source} allows {limit}"
                ),
            ));
        }
        Ok(())
    }

    /// The create request body. `media_ids` are the uploaded attachment ids, in entry order.
    pub fn body(&self, media_ids: &[String]) -> Value {
        let mut body = Map::new();
        body.insert("status".to_string(), Value::String(self.text.clone()));
        if !media_ids.is_empty() {
            body.insert(
                "media_ids".to_string(),
                Value::Array(
                    media_ids
                        .iter()
                        .map(|id| Value::String(id.clone()))
                        .collect(),
                ),
            );
        }
        if let Some(visibility) = &self.visibility {
            body.insert("visibility".to_string(), Value::String(visibility.clone()));
        }
        if let Some(language) = &self.language {
            body.insert("language".to_string(), Value::String(language.clone()));
        }
        Value::Object(body)
    }
}

/// The length Mastodon counts for `text`: Unicode scalar values, with every `http://` or `https://` URL
/// counted as [`URL_WEIGHT`] characters whatever its length (shorter URLs count MORE than they are).
///
/// Stricter readings, chosen where the documentation is not certain (listed in docs/PUBLISH-CORE.md):
/// scalar values, never fewer than grapheme clusters; the whole `@user@domain` of a mention counts
/// although Mastodon counts only `@user`; a URL ends at whitespace and trailing punctuation is counted
/// as text; a URL glued to a word, or without a scheme, is counted as plain text.
pub fn counted_length(text: &str) -> usize {
    let mut total = 0;
    let mut from = 0;
    for (start, end) in bluesky::url_spans(text) {
        total += text[from..start].chars().count() + URL_WEIGHT;
        from = end;
    }
    total + text[from..].chars().count()
}

fn clean_link(link: &str) -> bool {
    let scheme_len = if link.starts_with("https://") {
        8
    } else if link.starts_with("http://") {
        7
    } else {
        0
    };
    scheme_len > 0
        && link.len() > scheme_len
        && !link.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Validate a payload and build the status. Refused before any request: media that is not one to four
/// images with URLs (see `media::check_images`), a bad `settings.link`,
/// `settings.visibility`, `settings.language` or `settings.max_characters`, an empty text, and, when
/// `settings.max_characters` is present, a text over it.
pub fn prepare(payload: &crate::wire::PublishPayload) -> Result<Prepared, Fail> {
    let settings = &payload.settings;
    crate::media::check_images(payload, PLATFORM)?;
    let mut text = payload.caption.clone();
    match settings.get("link") {
        None | Some(Value::Null) => {}
        Some(Value::String(link)) => {
            if !clean_link(link) {
                return Err((
                    "bad_input",
                    "settings.link must be an http:// or https:// URL with no spaces".to_string(),
                ));
            }
            if !text.contains(link.as_str()) {
                text = format!("{text}\n\n{link}");
            }
        }
        Some(_) => return Err(("bad_input", "settings.link must be a string".to_string())),
    }
    match settings.get("verify_account") {
        None | Some(Value::Null | Value::Bool(_)) => {}
        Some(_) => {
            return Err((
                "bad_input",
                "settings.verify_account must be true or false".to_string(),
            ))
        }
    }
    let visibility = match settings.get("visibility") {
        None | Some(Value::Null) => None,
        Some(Value::String(v)) if VISIBILITIES.contains(&v.as_str()) => Some(v.clone()),
        Some(_) => {
            return Err((
                "bad_input",
                format!(
                    "settings.visibility must be one of {}",
                    VISIBILITIES.join(", ")
                ),
            ))
        }
    };
    let language = match settings.get("language") {
        None | Some(Value::Null) => None,
        Some(Value::String(l)) if l.len() == 2 && l.bytes().all(|b| b.is_ascii_lowercase()) => {
            Some(l.clone())
        }
        Some(_) => {
            return Err((
                "bad_input",
                "settings.language must be a two-letter ISO 639-1 code such as \"en\"".to_string(),
            ))
        }
    };
    let limit_override = match settings.get("max_characters") {
        None | Some(Value::Null) => None,
        Some(value) => match value.as_u64() {
            Some(n) if (1..=MAX_LIMIT).contains(&n) => Some(n as usize),
            _ => {
                return Err((
                    "bad_input",
                    format!("settings.max_characters must be a whole number from 1 to {MAX_LIMIT}"),
                ))
            }
        },
    };
    if text.trim().is_empty() {
        return Err((
            "caption_empty",
            "the mastodon status text is empty".to_string(),
        ));
    }
    let prepared = Prepared {
        text,
        visibility,
        language,
        limit_override,
    };
    if let Some(limit) = prepared.limit_override {
        prepared.within(limit, "settings.max_characters")?;
    }
    Ok(prepared)
}

/// `publish.enqueue` check: the payload must be one this module can post.
pub fn check(payload: &crate::wire::PublishPayload) -> Result<(), Fail> {
    prepare(payload).map(|_| ())
}

/// The instance's status limit from a `GET /api/v2/instance` answer:
/// `configuration.statuses.max_characters`. None when absent or not a believable whole number.
pub fn instance_limit(body: &Value) -> Option<usize> {
    let n = body
        .get("configuration")?
        .get("statuses")?
        .get("max_characters")?
        .as_u64()?;
    (1..=MAX_LIMIT).contains(&n).then_some(n as usize)
}

/// A status id is used as a URL path segment, so only plain characters are accepted.
fn plain_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The attachment id of a media upload answer (a MediaAttachment): its `id`, a non-empty string of plain
/// characters. Anything else is not accepted, so a status is never posted with a made-up attachment.
pub fn media_id(body: &Value) -> Option<String> {
    let id = body.get("id").and_then(Value::as_str)?;
    plain_id(id).then(|| id.to_string())
}

/// The status id from a create answer (a Status object): its `id`, a non-empty string of plain
/// characters. Anything else is not accepted as this job's status.
pub fn created_post_id(body: &Value) -> Option<String> {
    let id = body.get("id").and_then(Value::as_str)?;
    plain_id(id).then(|| id.to_string())
}

/// The `GET /api/v1/statuses/:id` path that reads back a stored status id.
pub fn read_path(post_id: &str) -> String {
    format!("{STATUSES_PATH}/{}", percent_encode(post_id))
}

/// The public permalink, once the read has confirmed the stored status: the answer's `id` must be the
/// stored post id, and its `url` must be an http(s) URL with no spaces.
pub fn permalink_from(post_id: Option<&str>, body: &Value) -> Option<String> {
    let id = body.get("id").and_then(Value::as_str)?;
    if post_id != Some(id) {
        return None;
    }
    let url = body.get("url").and_then(Value::as_str)?;
    (clean_link(url) && url.len() <= 2_048).then(|| url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::PublishPayload;
    use serde_json::json;

    fn payload(caption: &str, settings: Value) -> PublishPayload {
        PublishPayload {
            caption: caption.to_string(),
            format: "post".to_string(),
            media: Vec::new(),
            settings,
        }
    }

    #[test]
    fn urls_count_as_23_whatever_their_length() {
        assert_eq!(counted_length("hello"), 5);
        // 5 + 23: the URL is 11 characters long but counts 23.
        assert_eq!(counted_length("see: http://a.co"), 5 + 23);
        let long = format!("Read https://example.com/{}", "a".repeat(200));
        assert_eq!(counted_length(&long), 5 + 23);
        // Two URLs, text between and after.
        assert_eq!(
            counted_length("a https://x.example/1 b http://y.example/2 c"),
            2 + 23 + 3 + 23 + 2
        );
        // Trailing punctuation is counted as text (the stricter reading), and so is a glued scheme.
        assert_eq!(counted_length("https://x.example/1."), 23 + 1);
        assert_eq!(counted_length("xhttps://x.example"), 18);
        // Scalar values, not bytes.
        assert_eq!(counted_length("caf\u{e9}"), 4);
    }

    #[test]
    fn the_job_limit_is_enforced_at_prepare_and_the_instance_limit_by_within() {
        let over = payload("0123456789", json!({ "max_characters": 9 }));
        assert!(matches!(prepare(&over), Err(("text_too_long", _))));
        let fits = payload("0123456789", json!({ "max_characters": 10 }));
        let prepared = prepare(&fits).expect("fits");
        assert_eq!(prepared.limit_override(), Some(10));
        assert!(prepared.within(10, "x").is_ok());
        let err = prepared.within(9, "the instance").expect_err("over");
        assert_eq!(err.0, "text_too_long");
        assert!(err.1.contains("counts 10"));
        assert!(err.1.contains("the instance allows 9"));
        // Without a job limit the text is accepted here; the instance limit is applied at run time.
        let free = prepare(&payload(&"a".repeat(5_000), json!({}))).expect("no limit yet");
        assert_eq!(free.limit_override(), None);
        assert!(free.within(500, "the instance").is_err());
    }

    #[test]
    fn unsupported_or_malformed_input_is_refused_before_any_request() {
        let mut with_media = payload("text", json!({}));
        with_media.media.push(crate::wire::PublishMedia {
            hash: "blake3:abc".to_string(),
            kind: "video".to_string(),
            duration_ms: None,
            url: None,
        });
        assert!(matches!(
            prepare(&with_media),
            Err(("media_unsupported", _))
        ));
        for settings in [
            json!({ "link": "javascript:alert(1)" }),
            json!({ "link": 7 }),
            json!({ "visibility": "everyone" }),
            json!({ "language": "english" }),
            json!({ "language": "EN" }),
            json!({ "max_characters": 0 }),
            json!({ "max_characters": "500" }),
        ] {
            assert!(
                matches!(prepare(&payload("text", settings)), Err(("bad_input", _))),
                "should be refused"
            );
        }
        assert!(matches!(
            prepare(&payload("   ", json!({}))),
            Err(("caption_empty", _))
        ));
    }

    #[test]
    fn the_body_carries_the_status_and_only_the_settings_given() {
        let plain = prepare(&payload("hello", json!({}))).expect("plain");
        assert_eq!(plain.body(&[]), json!({ "status": "hello" }));
        assert_eq!(
            plain.body(&["m1".to_string(), "m2".to_string()]),
            json!({ "status": "hello", "media_ids": ["m1", "m2"] })
        );
        assert_eq!(media_id(&json!({ "id": "108" })).as_deref(), Some("108"));
        assert_eq!(media_id(&json!({ "id": "../x" })), None);
        assert_eq!(media_id(&json!({})), None);
        let full = prepare(&payload(
            "hello",
            json!({ "visibility": "unlisted", "language": "en", "link": "https://example.com/p/1" }),
        ))
        .expect("full");
        assert_eq!(
            full.body(&[]),
            json!({
                "status": "hello\n\nhttps://example.com/p/1",
                "visibility": "unlisted",
                "language": "en"
            })
        );
        let present = prepare(&payload(
            "Read https://example.com/p/1 now",
            json!({ "link": "https://example.com/p/1" }),
        ))
        .expect("link already in the caption");
        assert_eq!(
            present.body(&[])["status"],
            "Read https://example.com/p/1 now"
        );
    }

    #[test]
    fn answers_are_matched_to_the_stored_status() {
        let created = json!({ "id": "110345", "url": "https://m.example/@a/110345", "uri": "u" });
        assert_eq!(created_post_id(&created).as_deref(), Some("110345"));
        assert_eq!(created_post_id(&json!({})), None);
        assert_eq!(created_post_id(&json!({ "id": 110345 })), None);
        assert_eq!(created_post_id(&json!({ "id": "../x" })), None);
        assert_eq!(created_post_id(&json!({ "id": "" })), None);
        assert_eq!(read_path("110345"), "/api/v1/statuses/110345");

        assert_eq!(
            permalink_from(Some("110345"), &created).as_deref(),
            Some("https://m.example/@a/110345")
        );
        assert_eq!(permalink_from(Some("999"), &created), None);
        assert_eq!(permalink_from(None, &created), None);
        let no_url = json!({ "id": "110345", "url": null });
        assert_eq!(permalink_from(Some("110345"), &no_url), None);
        let bad_url = json!({ "id": "110345", "url": "javascript:alert(1)" });
        assert_eq!(permalink_from(Some("110345"), &bad_url), None);
    }

    #[test]
    fn the_instance_limit_is_read_from_the_configuration() {
        let body = json!({ "configuration": { "statuses": { "max_characters": 120 } } });
        assert_eq!(instance_limit(&body), Some(120));
        assert_eq!(instance_limit(&json!({})), None);
        assert_eq!(
            instance_limit(&json!({ "configuration": { "statuses": { "max_characters": 0 } } })),
            None
        );
        assert_eq!(
            instance_limit(
                &json!({ "configuration": { "statuses": { "max_characters": "500" } } })
            ),
            None
        );
    }
}
