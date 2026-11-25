// Inbox act planning (the `inbox.act` half of the inbox host).
//
// What `inbox.act` decides before a host submits an acting task, with nothing in it that belongs
// to one product: which action of an open item is taken, whether its ids and label are safe to
// quote, the retry, the idempotency key and the utterance id derived from it, and the digest a
// phone's signature is bound to. The host owns the task envelope, the words of the quoted request
// and where the item lives. Time is passed in; nothing here reads a clock or a store.

use serde_json::Value;

use super::inbox::{plain_id, state_at, state_word, InboxItem, InboxState};

/// Most evidence and fact ids one act carries.
pub const MAX_ACT_IDS: usize = 32;
/// Longest label an act quotes.
pub const MAX_LABEL_CHARS: usize = 80;
/// The highest `retry` an act takes.
pub const MAX_RETRY: i64 = 1000;

/// Why an act cannot be planned. `bad_input` is true for a malformed parameter, false for a
/// refusal with its own code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActRefusal {
    pub code: String,
    pub message: String,
    pub bad_input: bool,
}

impl ActRefusal {
    fn refuse(code: &str, message: impl Into<String>) -> Self {
        ActRefusal {
            code: code.to_string(),
            message: message.into(),
            bad_input: false,
        }
    }

    fn bad_input(message: impl Into<String>) -> Self {
        ActRefusal {
            code: "bad_input".to_string(),
            message: message.into(),
            bad_input: true,
        }
    }
}

/// The plain id in `params[key]`.
pub fn id_param(params: &Value, key: &str) -> Result<String, ActRefusal> {
    match params.get(key).and_then(Value::as_str) {
        Some(id) if plain_id(id) => Ok(id.to_string()),
        Some(_) => Err(ActRefusal::bad_input(format!(
            "param `{key}` is not a plain id"
        ))),
        None => Err(ActRefusal::bad_input(format!("missing param `{key}`"))),
    }
}

/// The integer in `params[key]`, `None` when absent or null.
pub fn opt_i64(params: &Value, key: &str) -> Result<Option<i64>, ActRefusal> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .map(Some)
            .ok_or_else(|| ActRefusal::bad_input(format!("param `{key}` must be an integer"))),
    }
}

/// A label as an act quotes it: letters, digits and a few marks only, whitespace collapsed, cut to
/// [`MAX_LABEL_CHARS`].
pub fn label_text(label: &str) -> String {
    let plain: String = label
        .chars()
        .map(|c| {
            if c.is_alphanumeric()
                || matches!(
                    c,
                    ' ' | '.' | ',' | ';' | ':' | '\'' | '(' | ')' | '-' | '/' | '&'
                )
            {
                c
            } else {
                ' '
            }
        })
        .collect();
    plain
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_LABEL_CHARS)
        .collect()
}

/// The ids as one line, `none` when there are none.
pub fn joined(ids: &[String]) -> String {
    if ids.is_empty() {
        "none".to_string()
    } else {
        ids.join(", ")
    }
}

/// What a plan resolved: the action taken, safe to quote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActChoice {
    pub item_id: String,
    pub action_id: String,
    /// The label, already cleaned by [`label_text`] and never empty.
    pub label: String,
    pub evidence_ids: Vec<String>,
    pub fact_ids: Vec<String>,
    /// 1 to [`MAX_RETRY`].
    pub retry: i64,
    /// `params.fromChat`, trimmed, when given and not empty.
    pub from_chat: Option<String>,
}

/// Resolves `{action?, retry?, fromChat?}` against `item` at `now`. Refuses an item that is not
/// open now, an action that is done or unknown, an id or label that cannot be quoted, too many
/// ids, and a `retry` outside 1 to 1000. Nothing else in `params` is read.
pub fn choose_act(item: &InboxItem, now: i64, params: &Value) -> Result<ActChoice, ActRefusal> {
    let state = state_at(item, now);
    if state != InboxState::Open {
        return Err(ActRefusal::refuse(
            "not_open",
            format!(
                "that item is {}; only an open item can be acted on",
                state_word(state)
            ),
        ));
    }
    let action = match params.get("action") {
        None | Some(Value::Null) => {
            let mut left = item.proposed_actions.iter().filter(|a| !a.done);
            match (left.next(), left.next()) {
                (Some(only), None) => only,
                (None, _) => {
                    return Err(ActRefusal::refuse(
                        "no_action",
                        "the item has no action left to take",
                    ));
                }
                _ => {
                    return Err(ActRefusal::refuse(
                        "action_required",
                        "the item proposes more than one action; name one with `action`",
                    ));
                }
            }
        }
        Some(_) => {
            let wanted = id_param(params, "action")?;
            item.proposed_actions
                .iter()
                .find(|a| a.id == wanted)
                .ok_or_else(|| {
                    ActRefusal::refuse("unknown_action", "the item proposes no action with that id")
                })?
        }
    };
    if action.done {
        return Err(ActRefusal::refuse(
            "already_done",
            "that action is already done",
        ));
    }
    if !plain_id(&action.id) {
        return Err(ActRefusal::refuse(
            "action_unusable",
            "the action's id is not a plain id",
        ));
    }
    let label = label_text(&action.label);
    if label.is_empty() {
        return Err(ActRefusal::refuse(
            "action_unusable",
            "the action has no usable label",
        ));
    }
    if item.evidence_ids.len() > MAX_ACT_IDS || item.fact_ids.len() > MAX_ACT_IDS {
        return Err(ActRefusal::refuse(
            "too_much_evidence",
            "the item carries too many ids for one task",
        ));
    }
    if item
        .evidence_ids
        .iter()
        .chain(&item.fact_ids)
        .any(|i| !plain_id(i))
    {
        return Err(ActRefusal::refuse(
            "evidence_unusable",
            "an evidence or fact id of the item is not a plain id, so nothing was submitted",
        ));
    }
    let retry = opt_i64(params, "retry")?.unwrap_or(1);
    if !(1..=MAX_RETRY).contains(&retry) {
        return Err(ActRefusal::bad_input("param `retry` must be 1 to 1000"));
    }
    let from_chat = params
        .get("fromChat")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|chat| !chat.is_empty())
        .map(str::to_string);
    Ok(ActChoice {
        item_id: item.id.clone(),
        action_id: action.id.clone(),
        label,
        evidence_ids: item.evidence_ids.clone(),
        fact_ids: item.fact_ids.clone(),
        retry,
        from_chat,
    })
}

/// The idempotency key of an act: it names the item, the action and `retry`, so a second click
/// resolves to the first task.
pub fn act_idempotency_key(item_id: &str, action_id: &str, retry: i64) -> String {
    format!("inbox.act:{item_id}:{action_id}:{retry}")
}

/// The utterance id, derived from the key and not from chance (UUID-shaped, 8-4-4-4-12 hex): a
/// resubmit with the same key is the same work only when the whole request matches, so a second
/// click must build the same request.
pub fn act_utterance_id(idempotency_key: &str) -> String {
    let digest = rightkit_crypto::hex_encode(&rightkit_crypto::sha256(idempotency_key.as_bytes()));
    format!(
        "{}-{}-{}-{}-{}",
        &digest[0..8],
        &digest[8..12],
        &digest[12..16],
        &digest[16..20],
        &digest[20..32]
    )
}

/// What a phone's signature is bound to, built from the stored item as it is now: SHA-256 hex of
/// `<scheme>`, the item id, the action id, the retry, the finding, the action's label and the
/// number of evidence ids, one to a line (`item:`, `action:`, `retry:`, `finding:`, `label:`,
/// `evidence:`). A changed finding or label gives another digest, so a signature made for the old
/// one is refused. `scheme` is the host's first line (a versioned tag such as `app.inbox-act.v1`); the phone
/// builds the same text.
pub fn act_digest(
    scheme: &str,
    item: &InboxItem,
    action_id: &str,
    retry: i64,
) -> Result<String, ActRefusal> {
    let action = item
        .proposed_actions
        .iter()
        .find(|a| a.id == action_id)
        .ok_or_else(|| {
            ActRefusal::refuse("unknown_action", "the item proposes no action with that id")
        })?;
    let text = format!(
        "{scheme}\nitem:{}\naction:{}\nretry:{}\nfinding:{}\nlabel:{}\nevidence:{}",
        item.id,
        action.id,
        retry,
        item.finding,
        action.label,
        item.evidence_ids.len()
    );
    Ok(rightkit_crypto::hex_encode(&rightkit_crypto::sha256(
        text.as_bytes(),
    )))
}

/// The name a signed challenge is kept under: `<prefix>:<item>:<action>:<retry>`.
pub fn signed_request_id(prefix: &str, item_id: &str, action_id: &str, retry: i64) -> String {
    format!("{prefix}:{item_id}:{action_id}:{retry}")
}

/// A phone's act params cut to `id`, `action` and `retry`: a `fromChat` or anything else it sent
/// is dropped, so a phone cannot name a chat.
pub fn clean_signed_params(params: &Value) -> Value {
    let mut clean = serde_json::Map::new();
    for key in ["id", "action", "retry"] {
        if let Some(value) = params.get(key) {
            clean.insert(key.to_string(), value.clone());
        }
    }
    Value::Object(clean)
}
