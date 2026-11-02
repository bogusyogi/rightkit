//! Bluesky (AT Protocol) request shapes for the publish core: a text post, optionally with link facets,
//! written with `com.atproto.repo.createRecord` and read back with `com.atproto.repo.getRecord`.
//!
//! The phase machine is unchanged (`create -> read_permalink -> done`); this module supplies what is
//! Bluesky-specific: the request bodies, the record key, the text rules and the answer parsing.
//!
//! createRecord is NOT idempotent, so the create is made safe by the record key. The caller picks the
//! `rkey` (see [`rkey_for`]) and derives it only from the job's idempotency key, so every attempt for a
//! job targets the same record: a second create at that key cannot make a second post. After an
//! ambiguous answer the machine never repeats the create blindly; `publish.reconcile` reads that one
//! record with getRecord ([`lookup_path`], [`adoptable`]) and adopts it, or finds it missing and allows
//! one retry at the same key. No `Idempotency-Key` header is sent: the record key is the idempotency.
//!
//! Images. A post with images first uploads each file with `com.atproto.repo.uploadBlob` (see
//! `machine.rs` `media_upload` and `media.rs`): the request body is the file itself with its image type as
//! `Content-Type`, and the answer is `{ "blob": { "$type": "blob", "ref": { "$link": "<cid>" },
//! "mimeType": "<type>", "size": <bytes> } }`. The blobs are kept on the job and the record carries them as
//! `embed: { "$type": "app.bsky.embed.images", "images": [ { "alt": "", "image": <blob> } ] }`. The alt text
//! is empty because a calendar entry carries none. Taken from the published lexicons and UNVERIFIED against
//! a live server: the 1,000,000-byte image limit, the four-image cap and the empty `alt`. A job that
//! resolves its PDS through `settings.plc_directory` cannot upload (the upload would need the discovered
//! PDS): it is refused with `media_unsupported` at its first step.
//!
//! Secrets: the access JWT is the command's `token`, sent only as the bearer of a request and scrubbed
//! from every message by `http::send`. The app password never enters the engine in this slice (session
//! creation and refresh are not implemented here), so no message built in this file can carry one.
//!
//! Protocol facts used here come from the published lexicons and were not verified against a live
//! server in this task; docs/PUBLISH-CORE.md lists what is left out because it is not known.
use rightkit_http::percent_encode;
use serde_json::{json, Map, Value};

use crate::fnv1a;
use crate::http::Reply;
use crate::Fail;

/// The platform name this module drives.
pub const PLATFORM: &str = "bluesky";
/// The record collection of a post.
const COLLECTION: &str = "app.bsky.feed.post";
/// The post text limit, in graphemes.
const MAX_GRAPHEMES: usize = 300;
const CREATE_RECORD_PATH: &str = "/xrpc/com.atproto.repo.createRecord";
const GET_RECORD_PATH: &str = "/xrpc/com.atproto.repo.getRecord";
/// `uploadBlob`: the body is the file, the answer carries the blob reference (see the module comment).
pub const UPLOAD_BLOB_PATH: &str = "/xrpc/com.atproto.repo.uploadBlob";
/// `refreshSession`: the refresh JWT is the bearer, there is no body; the answer carries a new `accessJwt`.
pub const REFRESH_SESSION_PATH: &str = "/xrpc/com.atproto.server.refreshSession";
/// The base32-sortable alphabet of a TID, the record-key type of `app.bsky.feed.post`.
const TID_ALPHABET: &[u8; 32] = b"234567abcdefghijklmnopqrstuvwxyz";

/// A link facet: the byte range of a URL inside the final text, and the URL.
struct Facet {
    start: usize,
    end: usize,
    uri: String,
}

/// What a job posts, validated and ready to serialise.
pub struct Prepared {
    text: String,
    facets: Vec<Facet>,
    langs: Vec<String>,
    /// How many images the post carries; the create needs exactly this many uploaded blobs.
    images: usize,
}

impl Prepared {
    /// The `record` object. `createdAt` is the step's `now`, so a replay with the same `now` is
    /// byte-identical and no wall clock is read. `blobs` are the uploaded image blobs, in entry order.
    fn record(&self, created_at: &str, blobs: &[Value]) -> Value {
        let mut record = Map::new();
        record.insert("$type".to_string(), json!(COLLECTION));
        record.insert("text".to_string(), Value::String(self.text.clone()));
        record.insert(
            "createdAt".to_string(),
            Value::String(created_at.to_string()),
        );
        if !self.facets.is_empty() {
            let facets: Vec<Value> = self
                .facets
                .iter()
                .map(|f| {
                    json!({
                        "index": { "byteStart": f.start, "byteEnd": f.end },
                        "features": [
                            { "$type": "app.bsky.richtext.facet#link", "uri": f.uri }
                        ]
                    })
                })
                .collect();
            record.insert("facets".to_string(), Value::Array(facets));
        }
        if !self.langs.is_empty() {
            let langs: Vec<Value> = self.langs.iter().map(|l| json!(l)).collect();
            record.insert("langs".to_string(), Value::Array(langs));
        }
        if !blobs.is_empty() {
            let images: Vec<Value> = blobs
                .iter()
                .map(|blob| json!({ "alt": "", "image": blob }))
                .collect();
            record.insert(
                "embed".to_string(),
                json!({ "$type": "app.bsky.embed.images", "images": images }),
            );
        }
        Value::Object(record)
    }
}

/// The record key of a job's post: a 13-character TID-shaped key derived from the idempotency key.
///
/// FNV-1a 64 of the idempotency key, top bit cleared, written as 13 base32-sortable characters
/// (`234567abcdefghijklmnopqrstuvwxyz`, five bits each, most significant first; the first character
/// carries only four bits and is therefore one of `234567ab`). That is valid record-key syntax
/// (letters and digits only) and also has the shape of a TID, the key type the post lexicon declares.
/// It is a pure function of the key, so every attempt, reconcile and restart computes the same key.
/// The value is a hash, not a timestamp.
pub fn rkey_for(idempotency_key: &str) -> String {
    let bits = fnv1a(idempotency_key) & 0x7fff_ffff_ffff_ffff;
    (0..13u32)
        .map(|i| {
            let shift = 60 - 5 * i;
            char::from(TID_ALPHABET[((bits >> shift) & 0x1f) as usize])
        })
        .collect()
}

/// The account handle in the job settings, used for the public permalink.
fn handle_of(settings: &Value) -> Result<String, Fail> {
    let handle = settings
        .get("handle")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    let ok = !handle.is_empty()
        && handle.len() <= 253
        && handle.contains('.')
        && !handle.starts_with(['.', '-'])
        && !handle.ends_with(['.', '-'])
        && handle
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.');
    if ok {
        Ok(handle.to_ascii_lowercase())
    } else {
        Err((
            "bad_input",
            "a bluesky job needs settings.handle: the account handle such as name.bsky.social (letters, digits, '-' and '.')"
                .to_string(),
        ))
    }
}

/// Link facets for every `http://` or `https://` URL in `text`. Offsets are UTF-8 BYTE offsets into
/// `text` (`str::find` and slicing are byte-based). A URL ends at whitespace; trailing `. , ; : ! ? ' "`
/// are not part of it, and neither is a closing `)` that has no opening `(` inside the URL.
fn link_facets(text: &str) -> Vec<Facet> {
    let mut facets = Vec::new();
    let mut from = 0;
    while from < text.len() {
        let Some(offset) = text[from..].find("http") else {
            break;
        };
        let start = from + offset;
        let candidate = &text[start..];
        let scheme_len = if candidate.starts_with("https://") {
            8
        } else if candidate.starts_with("http://") {
            7
        } else {
            0
        };
        let word_start = text[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric());
        if scheme_len == 0 || !word_start {
            from = start + 4;
            continue;
        }
        let stop = candidate
            .find(char::is_whitespace)
            .map_or(text.len(), |i| start + i);
        let mut end = stop;
        while let Some(last) = text[start..end].chars().next_back() {
            let url = &text[start..end];
            let trim = match last {
                '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' => true,
                ')' => url.matches(')').count() > url.matches('(').count(),
                _ => false,
            };
            if !trim {
                break;
            }
            end -= last.len_utf8();
        }
        if end - start > scheme_len {
            facets.push(Facet {
                start,
                end,
                uri: text[start..end].to_string(),
            });
        }
        from = stop;
    }
    facets
}

/// The byte ranges of the `http://` and `https://` URLs in `text`, in order and not overlapping, found
/// by the same rule as the link facets. Mastodon's character counting reuses it.
pub fn url_spans(text: &str) -> Vec<(usize, usize)> {
    link_facets(text)
        .into_iter()
        .map(|f| (f.start, f.end))
        .collect()
}

/// Validate a payload and build the final text, facets and languages.
///
/// The final text is the caption, plus `settings.link` on a new paragraph when the caption does not
/// already contain it. Facets are computed from that final text. Refused before any request:
/// media that is not one to four images with URLs (see `media::check_images`), a
/// missing or malformed `settings.handle`, a bad `settings.link` or `settings.langs`, an empty text, and
/// a text over the limit.
///
/// The limit counts Unicode scalar values, because exact extended-grapheme counting needs the Unicode
/// segmentation tables and the engine takes no new dependency. Every grapheme cluster contains at least
/// one scalar value, so graphemes <= scalars always holds. A text with at most 300 scalar values
/// therefore has at most 300 graphemes and can never exceed the real limit. The cost is a false
/// refusal: a text of combining marks or emoji sequences with fewer than 300 graphemes but more than 300
/// scalars is refused here although Bluesky would accept it. The gap is a stricter limit, never a looser
/// one. (The lexicon also caps the text at 3000 bytes; 300 scalars are at most 1200 bytes.)
pub fn prepare(payload: &crate::wire::PublishPayload) -> Result<Prepared, Fail> {
    let settings = &payload.settings;
    crate::media::check_images(payload, PLATFORM)?;
    handle_of(settings)?;
    let mut text = payload.caption.clone();
    match settings.get("link") {
        None | Some(Value::Null) => {}
        Some(Value::String(link)) => {
            let scheme_len = if link.starts_with("https://") {
                8
            } else if link.starts_with("http://") {
                7
            } else {
                0
            };
            let clean = scheme_len > 0
                && link.len() > scheme_len
                && !link.chars().any(|c| c.is_whitespace() || c.is_control());
            if !clean {
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
    let mut langs = Vec::new();
    match settings.get("langs") {
        None | Some(Value::Null) => {}
        Some(Value::Array(items)) => {
            for item in items {
                match item.as_str() {
                    Some(tag)
                        if !tag.is_empty()
                            && tag.len() <= 35
                            && tag.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') =>
                    {
                        langs.push(tag.to_string())
                    }
                    _ => {
                        return Err((
                            "bad_input",
                            "settings.langs must be an array of language tags such as \"en\""
                                .to_string(),
                        ))
                    }
                }
            }
        }
        Some(_) => {
            return Err((
                "bad_input",
                "settings.langs must be an array of language tags".to_string(),
            ))
        }
    }
    match settings.get("plc_directory") {
        None | Some(Value::Null) => {}
        Some(Value::String(url)) => {
            let ok = (url.starts_with("http://") || url.starts_with("https://"))
                && !url.chars().any(|c| c.is_whitespace() || c.is_control());
            if !ok {
                return Err((
                    "bad_input",
                    "settings.plc_directory must be an http:// or https:// URL with no spaces"
                        .to_string(),
                ));
            }
        }
        Some(_) => {
            return Err((
                "bad_input",
                "settings.plc_directory must be a string".to_string(),
            ))
        }
    }
    if text.trim().is_empty() {
        return Err((
            "caption_empty",
            "the bluesky post text is empty".to_string(),
        ));
    }
    let scalars = text.chars().count();
    if scalars > MAX_GRAPHEMES {
        return Err((
            "text_too_long",
            format!(
                "the post text has {scalars} Unicode scalar values; Bluesky allows {MAX_GRAPHEMES} graphemes and this client counts scalar values (never fewer than graphemes), so it refuses more than {MAX_GRAPHEMES}"
            ),
        ));
    }
    let facets = link_facets(&text);
    Ok(Prepared {
        text,
        facets,
        langs,
        images: payload.media.len(),
    })
}

/// `publish.enqueue` check: the payload must be one this module can post.
pub fn check(payload: &crate::wire::PublishPayload) -> Result<(), Fail> {
    prepare(payload).map(|_| ())
}

/// The blob reference of an `uploadBlob` answer: its `blob` object, when it is a blob with a `ref.$link`
/// string and a `mimeType` string. Anything else is not accepted, so a record is never built from it.
pub fn blob_of(body: &Value) -> Option<Value> {
    let blob = body.get("blob")?;
    let usable = blob.get("$type").and_then(Value::as_str) == Some("blob")
        && blob
            .get("ref")
            .and_then(|r| r.get("$link"))
            .and_then(Value::as_str)
            .is_some_and(|cid| !cid.is_empty() && cid.len() <= 256)
        && blob.get("mimeType").and_then(Value::as_str).is_some();
    usable.then(|| blob.clone())
}

/// The createRecord request: path and body. `blobs` are the uploaded image blobs of the job (empty for a
/// text post); their count must equal the post's image count, so an image is never dropped silently.
pub fn create_request(
    repo: &str,
    idempotency_key: &str,
    payload: &crate::wire::PublishPayload,
    created_at: &str,
    blobs: &[Value],
) -> Result<(String, Value), String> {
    let prepared = prepare(payload).map_err(|(_, message)| message)?;
    if blobs.len() != prepared.images {
        return Err(format!(
            "the post has {} images but {} uploaded blobs",
            prepared.images,
            blobs.len()
        ));
    }
    let body = json!({
        "repo": repo,
        "collection": COLLECTION,
        "rkey": rkey_for(idempotency_key),
        "record": prepared.record(created_at, blobs)
    });
    Ok((CREATE_RECORD_PATH.to_string(), body))
}

/// True when `uri` is an at:// URI of this job's record.
fn is_own_uri(idempotency_key: &str, uri: &str) -> bool {
    uri.starts_with("at://")
        && uri.ends_with(&format!("/{COLLECTION}/{}", rkey_for(idempotency_key)))
}

/// The post id from a createRecord answer: its `uri`, when it names this job's record. Anything else
/// (no uri, another record) is not accepted as this job's post.
pub fn created_post_id(idempotency_key: &str, body: &Value) -> Option<String> {
    let uri = body.get("uri").and_then(Value::as_str)?;
    is_own_uri(idempotency_key, uri).then(|| uri.to_string())
}

fn record_path(repo: &str, rkey: &str) -> String {
    format!(
        "{GET_RECORD_PATH}?repo={}&collection={}&rkey={}",
        percent_encode(repo),
        percent_encode(COLLECTION),
        percent_encode(rkey)
    )
}

/// The getRecord path that reads back a stored post id (an at:// uri).
pub fn read_path(repo: &str, post_id: &str) -> Option<String> {
    let rkey = post_id.rsplit('/').next().filter(|k| !k.is_empty())?;
    Some(record_path(repo, rkey))
}

/// The getRecord path for the record key this job would have used, for reconcile.
pub fn lookup_path(repo: &str, idempotency_key: &str) -> String {
    record_path(repo, &rkey_for(idempotency_key))
}

/// The public permalink, once getRecord has confirmed the stored post: the answer's `uri` must be the
/// stored post id. Form: `https://bsky.app/profile/<handle>/post/<rkey>`.
pub fn permalink_from(settings: &Value, post_id: Option<&str>, body: &Value) -> Option<String> {
    let uri = body.get("uri").and_then(Value::as_str)?;
    if post_id != Some(uri) {
        return None;
    }
    let handle = handle_of(settings).ok()?;
    let rkey = uri.rsplit('/').next().filter(|k| !k.is_empty())?;
    Some(format!("https://bsky.app/profile/{handle}/post/{rkey}"))
}

fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// For reconcile: the at:// uri of the record at this job's key when it is this job's post: the uri
/// names the job's record key and the stored text equals the text the job would post (whitespace
/// normalised). A record at the key with other text is not adopted.
pub fn adoptable(
    idempotency_key: &str,
    payload: &crate::wire::PublishPayload,
    body: &Value,
) -> Option<String> {
    let prepared = prepare(payload).ok()?;
    let uri = body.get("uri").and_then(Value::as_str)?;
    if !is_own_uri(idempotency_key, uri) {
        return None;
    }
    let stored = body.get("value")?.get("text").and_then(Value::as_str)?;
    (squash(stored) == squash(&prepared.text)).then(|| uri.to_string())
}

/// A getRecord answer that says the record does not exist: the lexicon's `RecordNotFound` error name
/// in the (scrubbed, capped) body. The HTTP status is not relied on, because it is not verified.
pub fn is_not_found(message: &str) -> bool {
    message.contains("RecordNotFound")
}

/// The new access JWT of a `refreshSession` answer: a non-empty string with no whitespace or control
/// characters (it becomes a header value).
pub fn access_jwt(body: &Value) -> Option<String> {
    let jwt = body.get("accessJwt")?.as_str()?;
    let clean = !jwt.is_empty()
        && jwt.len() <= 4096
        && !jwt.chars().any(|c| c.is_whitespace() || c.is_control());
    clean.then(|| jwt.to_string())
}

/// `settings.plc_directory`: the base URL of a DID directory. Present means the job resolves its account's
/// PDS and verifies its handle through DID resolution (opt-in, so existing jobs make exactly the requests
/// they did; the base URL is then the entryway that answers `resolveHandle`).
pub fn plc_directory(settings: &Value) -> Option<String> {
    settings
        .get("plc_directory")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The account handle of the job settings, lower-cased, when well formed.
pub fn job_handle(settings: &Value) -> Option<String> {
    handle_of(settings).ok()
}

/// `GET /xrpc/com.atproto.identity.resolveHandle?handle=<handle>`.
pub fn resolve_handle_path(handle: &str) -> String {
    format!(
        "/xrpc/com.atproto.identity.resolveHandle?handle={}",
        percent_encode(handle)
    )
}

/// True for a DID that is safe to put in a path: `did:<method>:<id>` of letters, digits and `._:%-`.
pub fn plain_did(did: &str) -> bool {
    did.starts_with("did:")
        && did.len() <= 512
        && did
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'%' | b'-'))
}

/// The DID of a `resolveHandle` answer.
pub fn resolved_did(body: &Value) -> Option<String> {
    let did = body.get("did")?.as_str()?;
    plain_did(did).then(|| did.to_string())
}

/// `GET <directory>/<did>`: the DID document path (the PLC directory's shape).
pub fn did_doc_path(did: &str) -> Option<String> {
    plain_did(did).then(|| format!("/{}", percent_encode(did)))
}

/// What a job needs from a DID document: the handles it claims (`alsoKnownAs` entries `at://<handle>`) and
/// its PDS (`service` entry whose id ends `#atproto_pds`, its `serviceEndpoint`).
pub struct DidDoc {
    pub handles: Vec<String>,
    pub pds: Option<String>,
}

/// Parses a DID document; its `id` must be the DID asked for.
pub fn parse_did_doc(did: &str, body: &Value) -> Option<DidDoc> {
    if body.get("id").and_then(Value::as_str) != Some(did) {
        return None;
    }
    let handles = body
        .get("alsoKnownAs")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .filter_map(|s| s.strip_prefix("at://"))
                .map(str::to_ascii_lowercase)
                .collect()
        })
        .unwrap_or_default();
    let pds = body
        .get("service")
        .and_then(Value::as_array)
        .and_then(|list| {
            list.iter().find(|s| {
                s.get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| id.ends_with("#atproto_pds"))
            })
        })
        .and_then(|s| s.get("serviceEndpoint")?.as_str())
        .map(str::to_string);
    Some(DidDoc { handles, pds })
}

/// An expired access JWT can arrive as a 4xx body naming `ExpiredToken` (the lexicon's error name)
/// instead of a bare 401. Treat it as the existing expired-token outcome.
pub fn normalize(reply: Reply) -> Reply {
    if let Reply::Rejected { message, .. } = &reply {
        if message.contains("ExpiredToken") {
            return Reply::Unauthorized;
        }
    }
    reply
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

    fn with_handle(caption: &str) -> PublishPayload {
        payload(caption, json!({ "handle": "alpha.mock.test" }))
    }

    #[test]
    fn rkey_is_a_pure_tid_shaped_function_of_the_key() {
        let key = "postright-pj-0123456789abcdef";
        let rkey = rkey_for(key);
        assert_eq!(rkey, rkey_for(key));
        assert_eq!(rkey, "24l4torpocyrj");
        assert_eq!(rkey.len(), 13);
        assert!(rkey.bytes().all(|b| TID_ALPHABET.contains(&b)));
        assert!(rkey.starts_with(['2', '3', '4', '5', '6', '7', 'a', 'b']));
        assert_ne!(rkey, rkey_for("postright-pj-0123456789abcdee"));
    }

    #[test]
    fn link_facets_use_utf8_byte_offsets() {
        let text = "Caf\u{e9} \u{2615} \u{2014} lire la suite: https://example.com/a?x=1";
        let facets = link_facets(text);
        assert_eq!(facets.len(), 1);
        let f = &facets[0];
        assert_eq!(&text[f.start..f.end], "https://example.com/a?x=1");
        assert_eq!(f.uri, "https://example.com/a?x=1");
        // 24 characters precede the link but 29 bytes do; the facet carries bytes.
        assert_eq!(text[..f.start].chars().count(), 24);
        assert_eq!(f.start, 29);
        assert_eq!(f.end, text.len());
        assert_eq!(f.end, 54);
    }

    #[test]
    fn link_facets_trim_punctuation_and_find_several() {
        let text = "See https://a.example/x. Then (https://b.example/y) and http://c.example/z!";
        let uris: Vec<String> = link_facets(text).into_iter().map(|f| f.uri).collect();
        assert_eq!(
            uris,
            [
                "https://a.example/x",
                "https://b.example/y",
                "http://c.example/z"
            ]
        );
        assert!(link_facets("no link here, not even xhttps://glued.example").is_empty());
        assert!(link_facets("scheme only https:// and http://").is_empty());
    }

    #[test]
    fn the_final_text_gets_the_link_and_its_facet() {
        let settings = json!({ "handle": "alpha.mock.test", "link": "https://example.com/p/1" });
        let prepared = prepare(&payload(
            "Cr\u{e8}me br\u{fb}l\u{e9}e \u{1f642} \u{e0} lire",
            settings,
        ))
        .expect("valid payload");
        assert_eq!(
            prepared.text,
            "Cr\u{e8}me br\u{fb}l\u{e9}e \u{1f642} \u{e0} lire\n\nhttps://example.com/p/1"
        );
        assert_eq!(prepared.facets.len(), 1);
        assert_eq!(prepared.facets[0].start, 30);
        assert_eq!(prepared.facets[0].end, 53);
        let record = prepared.record("2026-09-21T14:13:20Z", &[]);
        assert_eq!(record["facets"][0]["index"]["byteStart"], 30);
        assert_eq!(record["facets"][0]["index"]["byteEnd"], 53);
        assert_eq!(
            record["facets"][0]["features"][0]["$type"],
            "app.bsky.richtext.facet#link"
        );
        assert_eq!(record["$type"], COLLECTION);
        assert_eq!(record["createdAt"], "2026-09-21T14:13:20Z");
    }

    #[test]
    fn a_link_already_in_the_caption_is_not_repeated() {
        let settings = json!({ "handle": "alpha.mock.test", "link": "https://example.com/p/1" });
        let prepared =
            prepare(&payload("Read https://example.com/p/1 now", settings)).expect("valid payload");
        assert_eq!(prepared.text, "Read https://example.com/p/1 now");
        assert_eq!(prepared.facets.len(), 1);
    }

    #[test]
    fn the_scalar_count_bound_never_lets_through_more_than_300_graphemes() {
        assert!(prepare(&with_handle(&"a".repeat(300))).is_ok());
        let over = prepare(&with_handle(&"a".repeat(301)));
        assert!(matches!(over, Err(("text_too_long", _))));
        // 200 graphemes ("e" plus a combining acute accent) are 400 scalars: refused, the known false
        // refusal of a bound that can only be stricter than the real limit.
        let combining = "e\u{301}".repeat(200);
        assert!(matches!(
            prepare(&with_handle(&combining)),
            Err(("text_too_long", _))
        ));
    }

    #[test]
    fn unsupported_or_malformed_input_is_refused_before_any_request() {
        let mut with_media = with_handle("text");
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
        assert!(matches!(
            prepare(&payload("text", json!({}))),
            Err(("bad_input", _))
        ));
        assert!(matches!(
            prepare(&payload("text", json!({ "handle": "no-dot" }))),
            Err(("bad_input", _))
        ));
        assert!(matches!(
            prepare(&payload(
                "text",
                json!({ "handle": "a.example", "link": "javascript:alert(1)" })
            )),
            Err(("bad_input", _))
        ));
        assert!(matches!(
            prepare(&payload("   ", json!({ "handle": "a.example" }))),
            Err(("caption_empty", _))
        ));
    }

    #[test]
    fn create_request_carries_the_derived_key_and_the_repo() {
        let key = "postright-pj-0123456789abcdef";
        let (path, body) = create_request(
            "did:plc:abc",
            key,
            &with_handle("hello"),
            "2026-09-21T14:13:20Z",
            &[],
        )
        .expect("request");
        assert_eq!(path, "/xrpc/com.atproto.repo.createRecord");
        assert_eq!(body["repo"], "did:plc:abc");
        assert_eq!(body["collection"], COLLECTION);
        assert_eq!(body["rkey"], rkey_for(key));
        assert_eq!(body["record"]["text"], "hello");
        assert!(body["record"].get("facets").is_none());
    }

    #[test]
    fn images_are_embedded_from_uploaded_blobs() {
        let mut job = with_handle("with a picture");
        job.media.push(crate::wire::PublishMedia {
            hash: "sha256:abc".to_string(),
            kind: "image".to_string(),
            duration_ms: None,
            url: Some("https://media.example/a.png".to_string()),
        });
        let blob = json!({
            "$type": "blob",
            "ref": { "$link": "bafkreiexample" },
            "mimeType": "image/png",
            "size": 70
        });
        let key = "postright-pj-0123456789abcdef";
        let (_, body) = create_request(
            "did:plc:abc",
            key,
            &job,
            "2026-09-21T14:13:20Z",
            std::slice::from_ref(&blob),
        )
        .expect("request");
        let embed = &body["record"]["embed"];
        assert_eq!(embed["$type"], "app.bsky.embed.images");
        assert_eq!(embed["images"][0]["alt"], "");
        assert_eq!(embed["images"][0]["image"], blob);
        // The blob count must equal the image count: nothing is dropped silently.
        assert!(create_request("did:plc:abc", key, &job, "2026-09-21T14:13:20Z", &[]).is_err());
        // An uploadBlob answer is accepted only when it is a blob with a link and a type.
        assert_eq!(blob_of(&json!({ "blob": blob.clone() })), Some(blob));
        assert_eq!(blob_of(&json!({ "blob": { "$type": "blob" } })), None);
        assert_eq!(blob_of(&json!({})), None);
    }

    #[test]
    fn answers_are_matched_to_this_jobs_record() {
        let key = "postright-pj-0123456789abcdef";
        let rkey = rkey_for(key);
        let uri = format!("at://did:plc:abc/{COLLECTION}/{rkey}");
        let own = json!({ "uri": uri, "cid": "x" });
        assert_eq!(created_post_id(key, &own).as_deref(), Some(uri.as_str()));
        let other = json!({ "uri": format!("at://did:plc:abc/{COLLECTION}/2222222222222") });
        assert_eq!(created_post_id(key, &other), None);
        assert_eq!(created_post_id(key, &json!({})), None);

        let stored = json!({ "uri": uri, "value": { "text": "hello   world" } });
        let job = with_handle("hello world");
        assert_eq!(adoptable(key, &job, &stored).as_deref(), Some(uri.as_str()));
        let different = json!({ "uri": uri, "value": { "text": "something else" } });
        assert_eq!(adoptable(key, &job, &different), None);

        let settings = json!({ "handle": "Alpha.Mock.Test" });
        assert_eq!(
            permalink_from(&settings, Some(uri.as_str()), &stored),
            Some(format!(
                "https://bsky.app/profile/alpha.mock.test/post/{rkey}"
            ))
        );
        assert_eq!(permalink_from(&settings, Some("at://other"), &stored), None);
        assert_eq!(permalink_from(&settings, None, &stored), None);
    }

    #[test]
    fn read_paths_encode_the_repo() {
        let path = lookup_path("did:plc:abc", "postright-pj-0123456789abcdef");
        assert!(path.starts_with("/xrpc/com.atproto.repo.getRecord?repo="));
        assert!(path.contains("&collection="));
        assert!(path.ends_with("&rkey=24l4torpocyrj"));
        let stored = read_path(
            "did:plc:abc",
            "at://did:plc:abc/app.bsky.feed.post/24l4torpocyrj",
        );
        assert_eq!(stored, Some(path));
    }

    #[test]
    fn an_expired_token_body_is_the_expired_token_outcome() {
        let rejected = Reply::Rejected {
            status: 400,
            message: "{\"error\":\"ExpiredToken\",\"message\":\"Token has expired\"}".to_string(),
        };
        assert!(matches!(normalize(rejected), Reply::Unauthorized));
        let other = Reply::Rejected {
            status: 400,
            message: "{\"error\":\"InvalidRequest\"}".to_string(),
        };
        assert!(matches!(normalize(other), Reply::Rejected { .. }));
        assert!(is_not_found("{\"error\":\"RecordNotFound\"}"));
        assert!(!is_not_found("{\"error\":\"InvalidRequest\"}"));
    }
}
