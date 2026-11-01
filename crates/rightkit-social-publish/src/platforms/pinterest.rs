//! Pinterest (API v5) request shapes for the publish core: a pin created on a board in one step and read back.
//!
//! 1. `POST /v5/pins` with `{ board_id, title?, description, link?, media_source: { source_type: "image_url",
//!    url } }` and `Authorization: Bearer <user access token>` makes the pin and answers with the pin
//!    (`{ "id": "<id>", "link": ..., "board_id": ... }`).
//! 2. `GET /v5/pins/<id>` reads it back; the answer's `id` must be the stored pin id.
//!
//! The phase machine is the simple family's (`create -> read_permalink -> done`). This module supplies what is
//! Pinterest-specific: the request body and paths, the settings and text rules and the answer parsing.
//!
//! The board. `settings.board_id` is REQUIRED (a plain id): a pin always lives on a board and the engine never
//! guesses one, so a job without it is refused at enqueue (`board_required`) before any job or request exists.
//! Creating a board is not part of this module (`boards:write` is a separate request; not implemented).
//!
//! The image. Exactly one image with a public `https` URL (`media_source.source_type = "image_url"`):
//! Pinterest fetches it itself, so the engine never fetches or uploads a byte. Refused at enqueue: no media
//! (`media_required`), more than one item or a video (`media_unsupported`) and a URL that is not a public
//! `https` URL (`media_url_invalid`, the same host rules as Instagram's `check_media_url`).
//!
//! No idempotency key. The pin create has none that this module relies on, so a lost answer after it can never
//! be made safe by repeating it. The create carries NO `Idempotency-Key`, an ambiguous answer is
//! `outcome_unknown`, and a reconcile may only adopt a pin it can READ BY ID.
//!
//! Scopes and tokens (docs/p1/capability-matrix.md, sourced): publish uses `pins:write` and `boards:write`,
//! reads use `pins:read`, `boards:read` and `user_accounts:read`; analytics need no separate scope, they use
//! `pins:read` and `user_accounts:read`. The refresh token is continuous and lasts 60 days.
//!
//! Trial access. Under Trial access a pin (and a board) is visible only to its creator, so a pin made by a
//! Trial app is NOT public; a public pin needs Standard access. The matrix sources this to the Pinterest
//! developer pack (PACK section 6.3), not to a live test. [`TRIAL_VISIBILITY_NOTE`] is the text a job result
//! carries so no one reads a Trial pin's permalink as a public link.
//!
//! The permalink. The v5 pin object's `link` is the pin's DESTINATION link (the page the pin points to), not
//! its address on Pinterest. The pin's own address, `https://www.pinterest.com/pin/<id>/`, is built from the
//! confirmed id; that form is the conventional one and is UNVERIFIED against a live server. The destination
//! link read back is available through [`destination_link`].
//!
//! UNVERIFIED (the capability matrix says: "Pin spec: unsourced, reference not fetched"): every numeric pin
//! limit here (the 100-character title, the 500-character description, the 2,048-character link), the field
//! names of the create body and of the answer, the accepted image types and sizes, the answer status and the
//! rate-limit shape. Each constant below is marked; replace them with sourced values before live use.
//!
//! Secrets: the access token is the command's `token`, sent only as the `Authorization: Bearer` header of a
//! request, never in the URL or the query string, and scrubbed from every message by `http::send_with`.
use rightkit_http::percent_encode;
use serde_json::{json, Map, Value};

use crate::Fail;

/// The platform name this module drives.
pub const PLATFORM: &str = "pinterest";
/// The create request path under the base URL.
pub const CREATE_PATH: &str = "/v5/pins";
/// Longest description the engine accepts, in characters. UNVERIFIED development limit (the mock fixture
/// `platforms.json` configures the same value): the numeric pin specs are unsourced.
pub const DESCRIPTION_LIMIT: usize = 500;
/// Longest title the engine accepts, in characters. UNVERIFIED.
pub const TITLE_LIMIT: usize = 100;
/// Longest destination link accepted. UNVERIFIED.
pub const LINK_LIMIT: usize = 2_048;
/// Longest board or pin id accepted.
const MAX_ID: usize = 64;
/// The permalink prefix (UNVERIFIED, see the module comment).
const PERMALINK_PREFIX: &str = "https://www.pinterest.com/pin/";
/// What a Trial-access job result says about visibility (see the module comment).
pub const TRIAL_VISIBILITY_NOTE: &str = "Pinterest Trial access: this pin is visible only to the app owner's own account, not public; a public pin needs Standard access (capability matrix, PACK section 6.3; not verified live)";
/// The settings this client reads.
const KNOWN_SETTINGS: [&str; 3] = ["board_id", "title", "link"];

/// What a job posts, validated and ready to serialise.
pub struct Prepared {
    board_id: String,
    title: Option<String>,
    link: Option<String>,
    description: String,
    image_url: String,
}

impl Prepared {
    /// The create body.
    pub fn create_body(&self) -> Value {
        let mut body = Map::new();
        body.insert("board_id".to_string(), json!(self.board_id));
        if let Some(title) = &self.title {
            body.insert("title".to_string(), json!(title));
        }
        body.insert("description".to_string(), json!(self.description));
        if let Some(link) = &self.link {
            body.insert("link".to_string(), json!(link));
        }
        body.insert(
            "media_source".to_string(),
            json!({ "source_type": "image_url", "url": self.image_url }),
        );
        Value::Object(body)
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

/// The one image URL of a job, or the typed refusal (see the module comment).
fn image_of(payload: &crate::wire::PublishPayload) -> Result<String, Fail> {
    let [item] = payload.media.as_slice() else {
        return Err(if payload.media.is_empty() {
            (
                "media_required",
                "a pin needs one image: add one image to the entry and pass media_urls for its hash to publish.enqueue".to_string(),
            )
        } else {
            (
                "media_unsupported",
                "a pinterest pin carries one image; several are not implemented, keep one"
                    .to_string(),
            )
        });
    };
    if item.kind != "image" {
        return Err((
            "media_unsupported",
            format!(
                "the pinterest client posts one image, not a {}; video pins are not implemented",
                item.kind
            ),
        ));
    }
    let Some(url) = item.url.as_deref().filter(|u| !u.trim().is_empty()) else {
        return Err((
            "media_required",
            "the pinterest media item has no URL: pass media_urls for its hash to publish.enqueue"
                .to_string(),
        ));
    };
    super::instagram::check_media_url(url)
        .map_err(|message| ("media_url_invalid", format!("the pinterest {message}")))?;
    Ok(url.to_string())
}

/// A string setting, trimmed, None when absent; a non-string or an over-limit value is refused.
fn text_setting(
    map: &Map<String, Value>,
    name: &str,
    limit: usize,
) -> Result<Option<String>, Fail> {
    match map.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => {
            let count = s.chars().count();
            if count > limit {
                Err((
                    "bad_input",
                    format!("settings.{name} counts {count} characters and the pinterest client allows {limit}"),
                ))
            } else {
                Ok(Some(s.clone()))
            }
        }
        Some(_) => Err(("bad_input", format!("settings.{name} must be a string"))),
    }
}

/// Validate a payload and build the pin. Refused before any request: a missing or malformed
/// `settings.board_id` (`board_required` / `bad_input`), a setting this client does not know (ignoring one
/// silently would post the wrong thing), anything but one image with a public `https` URL (see `image_of`),
/// a link that is not an `https` URL and a description over [`DESCRIPTION_LIMIT`]. The description is the
/// entry's caption and may be empty.
pub fn prepare(payload: &crate::wire::PublishPayload) -> Result<Prepared, Fail> {
    let empty = Map::new();
    let map = match &payload.settings {
        Value::Null => &empty,
        Value::Object(map) => map,
        _ => {
            return Err((
                "bad_input",
                "the pinterest settings must be an object".to_string(),
            ))
        }
    };
    if let Some(unknown) = map.keys().find(|k| !KNOWN_SETTINGS.contains(&k.as_str())) {
        return Err((
            "bad_input",
            format!(
                "the pinterest client does not use the setting {unknown:?}; it reads board_id, title and link"
            ),
        ));
    }
    let board_id = match map.get("board_id") {
        None | Some(Value::Null) => {
            return Err((
                "board_required",
                "a pin is made on a board: pass settings.board_id to publish.enqueue".to_string(),
            ))
        }
        Some(Value::String(id)) if plain_id(id) => id.clone(),
        Some(_) => {
            return Err((
                "bad_input",
                format!(
                    "settings.board_id must be a board id (letters, digits, '-' or '_', at most {MAX_ID})"
                ),
            ))
        }
    };
    let image_url = image_of(payload)?;
    let title = text_setting(map, "title", TITLE_LIMIT)?;
    let link = text_setting(map, "link", LINK_LIMIT)?;
    if let Some(link) = &link {
        if !crate::media::plain_url(link) || !link.starts_with("https://") {
            return Err((
                "bad_input",
                "settings.link must be an https:// URL with a host, no user info and no spaces"
                    .to_string(),
            ));
        }
    }
    let description = payload.caption.clone();
    let count = description.chars().count();
    if count > DESCRIPTION_LIMIT {
        return Err((
            "text_too_long",
            format!(
                "the pin description counts {count} characters and the pinterest client allows {DESCRIPTION_LIMIT} (unverified limit)"
            ),
        ));
    }
    Ok(Prepared {
        board_id,
        title,
        link,
        description,
        image_url,
    })
}

/// `publish.enqueue` check: the payload must be one this module can post.
pub fn check(payload: &crate::wire::PublishPayload) -> Result<(), Fail> {
    prepare(payload).map(|_| ())
}

/// The id in a create answer: `id` as a plain string id. Anything else is not accepted as this job's pin.
pub fn created_pin_id(body: &Value) -> Option<String> {
    let id = body.get("id")?.as_str()?;
    plain_id(id).then(|| id.to_string())
}

/// The `GET /v5/pins/<id>` path that reads back a stored pin id. None when the stored value is not a plain
/// id, so nothing odd is ever put in a path. The token is never part of it.
pub fn read_path(pin_id: &str) -> Option<String> {
    plain_id(pin_id).then(|| format!("{CREATE_PATH}/{}", percent_encode(pin_id)))
}

/// The permalink, once the read has confirmed the stored pin: the answer's `id` must be the stored pin id.
/// The link is built from it (see the module comment).
pub fn permalink_from(pin_id: Option<&str>, body: &Value) -> Option<String> {
    let id = pin_id?;
    if !plain_id(id) || body.get("id").and_then(Value::as_str) != Some(id) {
        return None;
    }
    Some(format!("{PERMALINK_PREFIX}{id}/"))
}

/// The destination link the read-back reports for the confirmed pin (the page the pin points to), None when
/// the pin has none or the answer is not the stored pin.
#[cfg(test)]
pub fn destination_link(pin_id: &str, body: &Value) -> Option<String> {
    if body.get("id").and_then(Value::as_str) != Some(pin_id) {
        return None;
    }
    let link = body.get("link")?.as_str()?;
    (link.len() <= LINK_LIMIT && link.starts_with("https://")).then(|| link.to_string())
}

/// The board the read-back reports for the confirmed pin.
#[cfg(test)]
pub fn confirmed_board<'a>(pin_id: &str, body: &'a Value) -> Option<&'a str> {
    if body.get("id").and_then(Value::as_str) != Some(pin_id) {
        return None;
    }
    body.get("board_id").and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{PublishMedia, PublishPayload};

    fn image(url: Option<&str>) -> PublishMedia {
        PublishMedia {
            hash: "sha256:pin1".to_string(),
            kind: "image".to_string(),
            duration_ms: None,
            url: url.map(str::to_string),
        }
    }

    fn payload(caption: &str, settings: Value, url: Option<&str>) -> PublishPayload {
        PublishPayload {
            caption: caption.to_string(),
            format: "pin".to_string(),
            media: vec![image(url)],
            settings,
        }
    }

    const URL: &str = "https://media.example.com/a/pin.jpg";

    #[test]
    fn the_request_is_the_pin_create_on_a_board() {
        let prepared = prepare(&payload(
            "A pin",
            json!({ "board_id": "board-1", "title": "T", "link": "https://example.com/p" }),
            Some(URL),
        ))
        .expect("prepared");
        assert_eq!(
            prepared.create_body(),
            json!({
                "board_id": "board-1",
                "title": "T",
                "description": "A pin",
                "link": "https://example.com/p",
                "media_source": { "source_type": "image_url", "url": URL }
            })
        );
        assert_eq!(CREATE_PATH, "/v5/pins");
        let bare = prepare(&payload("", json!({ "board_id": "b1" }), Some(URL))).expect("prepared");
        assert!(bare.create_body().get("title").is_none());
        assert!(bare.create_body().get("link").is_none());
    }

    #[test]
    fn a_missing_board_is_refused_before_any_request() {
        for settings in [json!({}), Value::Null, json!({ "board_id": null })] {
            assert!(
                matches!(
                    prepare(&payload("x", settings, Some(URL))),
                    Err(("board_required", _))
                ),
                "board is required"
            );
        }
        for bad in [
            json!({ "board_id": "a/b" }),
            json!({ "board_id": 12 }),
            json!({ "board_id": "" }),
        ] {
            assert!(matches!(
                prepare(&payload("x", bad, Some(URL))),
                Err(("bad_input", _))
            ));
        }
        assert!(matches!(
            prepare(&payload(
                "x",
                json!({ "board_id": "b", "privacy": "secret" }),
                Some(URL)
            )),
            Err(("bad_input", _))
        ));
        assert!(matches!(
            prepare(&payload("x", json!("b"), Some(URL))),
            Err(("bad_input", _))
        ));
    }

    #[test]
    fn the_image_must_be_one_public_https_url() {
        let ok = json!({ "board_id": "b" });
        assert!(matches!(
            prepare(&payload("x", ok.clone(), None)),
            Err(("media_required", _))
        ));
        for url in [
            "http://media.example.com/a.jpg",
            "https://localhost/a.jpg",
            "https://10.0.0.5/a.jpg",
            "https://u@media.example.com/a.jpg",
        ] {
            assert!(
                matches!(
                    prepare(&payload("x", ok.clone(), Some(url))),
                    Err(("media_url_invalid", _))
                ),
                "{url}"
            );
        }
        let mut none = payload("x", ok.clone(), Some(URL));
        none.media.clear();
        assert!(matches!(prepare(&none), Err(("media_required", _))));
        let mut two = payload("x", ok.clone(), Some(URL));
        two.media.push(image(Some(URL)));
        assert!(matches!(prepare(&two), Err(("media_unsupported", _))));
        let mut video = payload("x", ok, Some(URL));
        video.media[0].kind = "video".to_string();
        assert!(matches!(prepare(&video), Err(("media_unsupported", _))));
    }

    #[test]
    fn text_and_link_limits_are_exact() {
        let s = json!({ "board_id": "b" });
        assert!(prepare(&payload(
            &"a".repeat(DESCRIPTION_LIMIT),
            s.clone(),
            Some(URL)
        ))
        .is_ok());
        assert!(matches!(
            prepare(&payload(&"a".repeat(DESCRIPTION_LIMIT + 1), s, Some(URL))),
            Err(("text_too_long", _))
        ));
        let long_title = json!({ "board_id": "b", "title": "t".repeat(TITLE_LIMIT + 1) });
        assert!(matches!(
            prepare(&payload("x", long_title, Some(URL))),
            Err(("bad_input", _))
        ));
        let http_link = json!({ "board_id": "b", "link": "http://example.com" });
        assert!(matches!(
            prepare(&payload("x", http_link, Some(URL))),
            Err(("bad_input", _))
        ));
    }

    #[test]
    fn answers_are_matched_to_the_stored_pin() {
        let pin = "880000000000000001";
        assert_eq!(created_pin_id(&json!({ "id": pin })).as_deref(), Some(pin));
        assert_eq!(created_pin_id(&json!({})), None);
        assert_eq!(created_pin_id(&json!({ "id": 12 })), None);
        for bad in ["", "a/b", "../1", "1 2"] {
            assert_eq!(created_pin_id(&json!({ "id": bad })), None, "{bad:?}");
        }
        assert_eq!(
            read_path(pin).as_deref(),
            Some("/v5/pins/880000000000000001")
        );
        assert_eq!(read_path("a/b"), None);
        let read = json!({ "id": pin, "link": "https://example.com/p", "board_id": "board-1" });
        assert_eq!(
            permalink_from(Some(pin), &read).as_deref(),
            Some("https://www.pinterest.com/pin/880000000000000001/")
        );
        assert_eq!(permalink_from(Some("9"), &read), None);
        assert_eq!(permalink_from(None, &read), None);
        assert_eq!(
            destination_link(pin, &read).as_deref(),
            Some("https://example.com/p")
        );
        assert_eq!(
            destination_link(pin, &json!({ "id": pin, "link": null })),
            None
        );
        assert_eq!(destination_link("9", &read), None);
        assert_eq!(confirmed_board(pin, &read), Some("board-1"));
        assert_eq!(confirmed_board("9", &read), None);
    }
}
