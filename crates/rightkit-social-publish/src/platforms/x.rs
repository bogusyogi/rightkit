//! X (text post) request shapes for the publish core: a post written in one step with `POST /2/tweets` and
//! read back with `GET /2/tweets/<id>?tweet.fields=created_at,text`.
//!
//! 1. `POST /2/tweets` with `{ "text": "<text>" }` and `Authorization: Bearer <user access token>` makes the
//!    post and answers `201` with `{ "data": { "id": "<id>", "text": "<text>" } }`.
//! 2. `GET /2/tweets/<id>?tweet.fields=created_at,text` reads it back (`{ "data": { ... } }`).
//!
//! The phase machine is the simple family's (`create -> read_permalink -> done`). This module supplies what is
//! X-specific: the request body and paths, the text rules and the answer parsing.
//!
//! No idempotency key. The create has none, so a lost answer after it can never be made safe by repeating it.
//! The create therefore carries NO `Idempotency-Key`, an ambiguous answer is `outcome_unknown`, and
//! `publish.reconcile` (machine.rs `reconcile_x`) never creates: it may only adopt a post it can READ BY ID, and
//! without one it leaves the job `outcome_unknown` for a person (`publish.resolve_unknown`). The API refuses a
//! text that duplicates a recent post of the same account, which would make a blind repeat partly
//! self-limiting; nothing here relies on that.
//!
//! The author. The post is made by the account the access token belongs to. The job's stored `remote_account`
//! is not part of any request: it only addresses the mock through the base URL (`<mock>/x/<account>`) and keeps
//! the job tied to its account in the store.
//!
//! The permalink. The API's read does not return a link. The form `https://x.com/i/web/status/<id>` is built
//! from the confirmed id; it is the conventional form and is UNVERIFIED against a live server.
//!
//! Images. A post with images first uploads each file with `POST /2/media/upload` (`multipart/form-data`:
//! the file in `media`, and `media_category=tweet_image`; see `machine.rs` `media_upload` and `media.rs`),
//! whose answer is `{ "data": { "id": "<media id>" } }`. The ids are kept on the job and the create sends
//! them as `media: { "media_ids": [...] }`. Sourced (docs/p1/capability-matrix.md, docs.x.com media pages):
//! the `media.write` scope exists, `tweet_image` is limited to 5 MB. UNVERIFIED here: the `/2/media/upload`
//! path (the cited page describes the chunked upload), the `media` and `media_category` fields of the simple
//! upload, the answer shape, the four-image cap, and which image types are accepted.
//!
//! The text limit. X counts weighted characters, not code units: a URL counts 23, characters in the CJK, Hangul,
//! fullwidth and emoji ranges count 2, the rest 1, and the limit is 280. The weights live in one constant
//! ([`WEIGHT_TWO_RANGES`]), marked unverified: the capability matrix cites no official source for them.
//!
//! Secrets: the access token is the command's `token`, sent only as the `Authorization: Bearer` header of a
//! request, never in the URL or the query string, and scrubbed from every message by `http::send_with`.
//! Nothing in this file builds a message from the token.
//!
//! Protocol facts used here come from X's published API v2 documentation and were not verified against a live
//! server in this task; docs/PUBLISH-CORE.md lists what is left out because it is not known.
use serde_json::{json, Value};

use crate::Fail;

/// The platform name this module drives.
pub const PLATFORM: &str = "x";
/// The create request path under the base URL.
pub const CREATE_PATH: &str = "/2/tweets";
/// The media upload path (see the module comment; unverified).
pub const MEDIA_PATH: &str = "/2/media/upload";
/// Longest weighted count the engine accepts for a post's text (see [`counted_length`]).
pub const TEXT_LIMIT: usize = 280;
/// What every URL counts as, whatever its length.
pub const URL_WEIGHT: usize = 23;
/// The code point ranges (inclusive) whose characters count 2; every other character counts 1.
///
/// unverified: confirm against developer.x.com counting docs. docs/p1/capability-matrix.md cites no official
/// source for X's weights, so this table is a recollection of X's published counting configuration (CJK and
/// Hangul blocks, fullwidth forms, and the emoji and symbol blocks count double), kept in this one constant so
/// it is corrected in one place.
pub const WEIGHT_TWO_RANGES: [(u32, u32); 8] = [
    (0x1100, 0x11FF),   // Hangul Jamo
    (0x2600, 0x27BF),   // Miscellaneous Symbols and Dingbats
    (0x2E80, 0x9FFF),   // CJK radicals, kana, CJK symbols and unified ideographs
    (0xAC00, 0xD7AF),   // Hangul syllables
    (0xF900, 0xFAFF),   // CJK compatibility ideographs
    (0xFF00, 0xFFEF),   // Halfwidth and fullwidth forms
    (0x1F000, 0x1FAFF), // Emoji and pictograph blocks
    (0x20000, 0x2FFFF), // CJK extension planes
];
/// The query asked for on the read-back.
const READ_QUERY: &str = "tweet.fields=created_at,text";
/// The permalink prefix (unverified, see the module comment).
const PERMALINK_PREFIX: &str = "https://x.com/i/web/status/";
/// Longest post id accepted: a 64-bit decimal.
const MAX_ID: usize = 20;
/// What the duplicate-content refusal says, lower-cased.
const DUPLICATE_PHRASE: &str = "duplicate content";

/// What a job posts, validated and ready to serialise.
pub struct Prepared {
    text: String,
}

impl Prepared {
    /// The create body. `media_ids` are the uploaded media ids, in entry order.
    pub fn body(&self, media_ids: &[String]) -> Value {
        let mut body = json!({ "text": self.text });
        if !media_ids.is_empty() {
            body["media"] = json!({ "media_ids": media_ids });
        }
        body
    }
}

/// True for a plain post id: 1 to 20 ASCII digits.
fn plain_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID && id.bytes().all(|b| b.is_ascii_digit())
}

/// The weight one character counts: 2 inside [`WEIGHT_TWO_RANGES`], else 1.
fn char_weight(c: char) -> usize {
    let code = c as u32;
    if WEIGHT_TWO_RANGES
        .iter()
        .any(|(lo, hi)| (*lo..=*hi).contains(&code))
    {
        2
    } else {
        1
    }
}

/// The weighted count of `text`: every `http://` or `https://` URL counts [`URL_WEIGHT`] whatever its length,
/// every other character counts by `char_weight`. Only URLs with a scheme are recognised as URLs (a stricter
/// reading than X's, which also recognises some bare domains); a bare domain counts as plain text.
pub fn counted_length(text: &str) -> usize {
    let weigh = |part: &str| part.chars().map(char_weight).sum::<usize>();
    let mut total = 0;
    let mut from = 0;
    for (start, end) in super::bluesky::url_spans(text) {
        total += weigh(&text[from..start]) + URL_WEIGHT;
        from = end;
    }
    total + weigh(&text[from..])
}

/// Validate a payload and build the post. Refused before any request: media that is not one to four images
/// with URLs (see `media::check_images`), any setting (none is used, and ignoring one silently
/// would post the wrong thing), an empty text and a text whose weighted count is over [`TEXT_LIMIT`].
pub fn prepare(payload: &crate::wire::PublishPayload) -> Result<Prepared, Fail> {
    crate::media::check_images(payload, PLATFORM)?;
    let unused = match &payload.settings {
        Value::Null => false,
        Value::Object(map) => !map.is_empty(),
        _ => true,
    };
    if unused {
        return Err((
            "bad_input",
            "the x client uses no settings; remove them from the job".to_string(),
        ));
    }
    let text = payload.caption.clone();
    if text.trim().is_empty() {
        return Err(("caption_empty", "the x post text is empty".to_string()));
    }
    let count = counted_length(&text);
    if count > TEXT_LIMIT {
        return Err((
            "text_too_long",
            format!(
                "the post text counts {count} characters (each URL counts {URL_WEIGHT}, characters in the double-weight ranges count 2) and the x client allows {TEXT_LIMIT}"
            ),
        ));
    }
    Ok(Prepared { text })
}

/// `publish.enqueue` check: the payload must be one this module can post.
pub fn check(payload: &crate::wire::PublishPayload) -> Result<(), Fail> {
    prepare(payload).map(|_| ())
}

/// The id in a create answer: `data.id` as a plain decimal string. Anything else is not accepted as this
/// job's post.
pub fn created_post_id(body: &Value) -> Option<String> {
    let id = body.get("data")?.get("id")?.as_str()?;
    plain_id(id).then(|| id.to_string())
}

/// The media id of an upload answer: `data.id` as a plain decimal string. Anything else is not accepted, so
/// a post is never made with a made-up attachment.
pub fn media_id(body: &Value) -> Option<String> {
    let id = body.get("data")?.get("id")?.as_str()?;
    plain_id(id).then(|| id.to_string())
}

/// The `GET /2/tweets/<id>?tweet.fields=...` path that reads back a stored post id. None when the stored
/// value is not a plain id, so nothing odd is ever put in a path. The token is never part of it.
pub fn read_path(post_id: &str) -> Option<String> {
    plain_id(post_id).then(|| format!("{CREATE_PATH}/{post_id}?{READ_QUERY}"))
}

/// The read-back `text`, once the read has confirmed the stored post: the answer's `data.id` must be the
/// stored post id.
pub fn confirmed_text<'a>(post_id: &str, body: &'a Value) -> Option<&'a str> {
    let data = body.get("data")?;
    if data.get("id").and_then(Value::as_str) != Some(post_id) {
        return None;
    }
    data.get("text").and_then(Value::as_str)
}

/// The permalink, once the read has confirmed the stored post: the answer's `data.id` must be the stored
/// post id, a plain id. The link is built from it (see the module comment).
pub fn permalink_from(post_id: Option<&str>, body: &Value) -> Option<String> {
    let id = post_id?;
    if !plain_id(id) || body.get("data")?.get("id").and_then(Value::as_str) != Some(id) {
        return None;
    }
    Some(format!("{PERMALINK_PREFIX}{id}"))
}

/// True when a 403 body says the text duplicates a recent post. The refusal is final for this text, so it is
/// typed `duplicate_content` and never retried; any other 403 is `permission_denied`.
pub fn is_duplicate_content(message: &str) -> bool {
    message.to_ascii_lowercase().contains(DUPLICATE_PHRASE)
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

    #[test]
    fn the_request_is_the_documented_create() {
        let prepared = prepare(&payload("Hello X", json!({}))).expect("prepared");
        assert_eq!(prepared.body(&[]), json!({ "text": "Hello X" }));
        assert_eq!(
            prepared.body(&["1810000000000000001".to_string()]),
            json!({ "text": "Hello X", "media": { "media_ids": ["1810000000000000001"] } })
        );
        assert_eq!(
            media_id(&json!({ "data": { "id": "1810000000000000001" } })).as_deref(),
            Some("1810000000000000001")
        );
        assert_eq!(media_id(&json!({ "data": { "id": "abc" } })), None);
        assert_eq!(media_id(&json!({})), None);
        assert_eq!(CREATE_PATH, "/2/tweets");
        // A null settings value (a payload read before its JSON is restored) is accepted.
        assert!(prepare(&payload("x", Value::Null)).is_ok());
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
        for settings in [json!({ "reply": "1" }), json!("x"), json!(1)] {
            assert!(
                matches!(prepare(&payload("text", settings)), Err(("bad_input", _))),
                "should be refused"
            );
        }
        assert!(matches!(
            prepare(&payload("  \n ", json!({}))),
            Err(("caption_empty", _))
        ));
    }

    #[test]
    fn the_limit_counts_weighted_characters_and_is_exact() {
        assert!(prepare(&payload(&"a".repeat(TEXT_LIMIT), json!({}))).is_ok());
        let over = TEXT_LIMIT + 1;
        let Err(err) = prepare(&payload(&"a".repeat(over), json!({}))) else {
            panic!("{over} characters must be refused");
        };
        assert_eq!(err.0, "text_too_long");
        assert!(err.1.contains(&format!("counts {over} characters")));
        assert!(err.1.contains(&format!("allows {TEXT_LIMIT}")));
        // An emoji and a CJK character count 2: half the limit of them is exactly the limit.
        assert_eq!(counted_length("\u{1f600}"), 2);
        assert_eq!(counted_length("\u{65e5}"), 2);
        assert_eq!(counted_length("\u{e9}"), 1);
        let emoji = "\u{1f600}".repeat(TEXT_LIMIT / 2);
        assert!(prepare(&payload(&emoji, json!({}))).is_ok());
        let over_emoji = format!("{emoji}a");
        assert!(matches!(
            prepare(&payload(&over_emoji, json!({}))),
            Err(("text_too_long", _))
        ));
        let cjk = "\u{65e5}".repeat(TEXT_LIMIT / 2);
        assert!(prepare(&payload(&cjk, json!({}))).is_ok());
        assert!(matches!(
            prepare(&payload(&format!("{cjk}\u{65e5}"), json!({}))),
            Err(("text_too_long", _))
        ));
    }

    #[test]
    fn a_url_counts_23_whatever_its_length() {
        let url = "https://example.test/a/very/long/path/that/is/much/longer/than/twenty-three/characters";
        assert!(url.len() > URL_WEIGHT);
        assert_eq!(counted_length(url), URL_WEIGHT);
        assert_eq!(counted_length(&format!("hi {url}")), 3 + URL_WEIGHT);
        let fits = format!("{} {url}", "a".repeat(TEXT_LIMIT - URL_WEIGHT - 1));
        assert_eq!(counted_length(&fits), TEXT_LIMIT);
        assert!(prepare(&payload(&fits, json!({}))).is_ok());
        let over = format!("{} {url}", "a".repeat(TEXT_LIMIT - URL_WEIGHT));
        assert!(matches!(
            prepare(&payload(&over, json!({}))),
            Err(("text_too_long", _))
        ));
    }

    #[test]
    fn the_weight_table_is_sorted_and_disjoint() {
        for pair in WEIGHT_TWO_RANGES.windows(2) {
            assert!(pair[0].0 <= pair[0].1 && pair[0].1 < pair[1].0, "{pair:?}");
        }
    }

    #[test]
    fn answers_are_matched_to_the_stored_post() {
        let post = "1790000000000000001";
        assert_eq!(
            created_post_id(&json!({ "data": { "id": post, "text": "t" } })).as_deref(),
            Some(post)
        );
        assert_eq!(created_post_id(&json!({})), None);
        assert_eq!(created_post_id(&json!({ "data": {} })), None);
        // The id is a decimal string: a number, a path or an empty value is not accepted.
        assert_eq!(created_post_id(&json!({ "data": { "id": 12 } })), None);
        for bad in ["", "a1", "1/2", "../1", "1 2", "12345678901234567890123"] {
            assert_eq!(
                created_post_id(&json!({ "data": { "id": bad } })),
                None,
                "{bad:?}"
            );
        }

        assert_eq!(
            read_path(post).as_deref(),
            Some(format!("/2/tweets/{post}?tweet.fields=created_at,text").as_str())
        );
        assert_eq!(read_path("a/b"), None);
        assert_eq!(read_path(""), None);

        let read = json!({ "data": { "id": post, "text": "Hello", "created_at": "2026-09-21T14:13:20Z" } });
        assert_eq!(
            permalink_from(Some(post), &read).as_deref(),
            Some("https://x.com/i/web/status/1790000000000000001")
        );
        assert_eq!(permalink_from(Some("1790000000000000009"), &read), None);
        assert_eq!(permalink_from(None, &read), None);
        assert_eq!(
            permalink_from(Some("a/b"), &json!({ "data": { "id": "a/b" } })),
            None
        );
        // A read that found nothing carries no data.
        assert_eq!(
            permalink_from(
                Some(post),
                &json!({ "errors": [{ "title": "Not Found Error" }] })
            ),
            None
        );
        assert_eq!(confirmed_text(post, &read), Some("Hello"));
        assert_eq!(confirmed_text("1790000000000000009", &read), None);
        assert_eq!(
            confirmed_text(post, &json!({ "data": { "id": post } })),
            None
        );
    }

    #[test]
    fn a_duplicate_content_refusal_is_told_from_other_403s() {
        let duplicate = "{\"detail\":\"You are not allowed to create a Tweet with duplicate content.\",\"type\":\"about:blank\",\"title\":\"Forbidden\",\"status\":403}";
        assert!(is_duplicate_content(duplicate));
        // A body cut at 200 characters, and a different case, are still read.
        assert!(is_duplicate_content(
            "...to create a Tweet with Duplicate Content.\",\"ty"
        ));
        assert!(!is_duplicate_content(
            "{\"title\":\"Forbidden\",\"detail\":\"Forbidden\",\"status\":403}"
        ));
        assert!(!is_duplicate_content(""));
    }
}
