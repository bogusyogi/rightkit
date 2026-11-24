//! `agent.engine`: acceptance of a run the ENGINE starts from a standing instruction or a schedule the owner approved
//! (`IntentSource::Engine`).
//!
//! The source carries no quote, because nothing the owner said this time started the run. What authorises it is a record: the
//! instruction `rule_id`, active at exactly `rule_revision` in the host's own store, and its occurrence `occurrence_key`. The
//! record lookup is the host's; this module holds the rest: who may submit it (only the engine's own principal, with the engine's
//! own seal over what names the task), the recipe to mode map, the task body the engine submits, and the strict reading of a
//! `brief` reply.
//!
//! * [`engine_source`], [`seal_text`], [`EngineParams`], [`verify_envelope`]: the checks both submit and run start make.
//! * [`mode_of`], [`known_recipe`]: the recipes and the mode each one runs in.
//! * [`build_envelope`], [`build_params`]: the task the engine submits for one occurrence.
//! * [`parse_brief_cards`], [`BriefCard`]: the brief's answer, read strictly.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::act_params::{AcceptError, AcceptResult, Mode};
use crate::device::{DEVICE_REF_SCHEMA_VERSION, DevicePlatform, DeviceRef};
use crate::intent::{INTENT_SCHEMA_VERSION, Intent};
use crate::principal::{PRINCIPAL_SCHEMA_VERSION, Principal};
use crate::task::{
    TASK_ENVELOPE_SCHEMA_VERSION, TaskEnvelope, TaskStatus, UTTERANCE_REF_SCHEMA_VERSION,
    UtteranceRef,
};
use crate::{EngineTrigger, IntentSource, PrincipalKind};

/// The `policy_ref` of an engine-started task.
pub const POLICY_REF: &str = "agent-engine/v1";
/// The principal subject of the engine's own submits.
pub const ENGINE_SUBJECT: &str = "engine";
/// Most cards a brief may hold.
pub const CARDS_MAX: usize = 5;
/// Longest card title, in characters.
pub const TITLE_MAX_CHARS: usize = 100;
/// Longest card detail, in characters.
pub const DETAIL_MAX_CHARS: usize = 300;
/// A brief card or a prepared-reply item expires after this long.
pub const ITEM_LIFETIME_SECS: i64 = 24 * 3600;

/// The params of an `agent.engine` task.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineParams {
    pub schema_version: u32,
    pub recipe: String,
    pub seal: String,
}

fn refused(message: &str) -> AcceptError {
    AcceptError::new("unsupported_origin", message)
}

/// The mode a recipe runs in. Unknown recipes are refused.
#[must_use]
pub fn mode_of(recipe: &str) -> Option<Mode> {
    match recipe {
        "brief" => Some(Mode::Brief),
        "prepare" => Some(Mode::Prepared),
        _ => None,
    }
}

/// Whether the engine knows `recipe` (a host refuses to propose one it could not run).
#[must_use]
pub fn known_recipe(recipe: &str) -> bool {
    mode_of(recipe).is_some()
}

/// A short stable tag made of `parts`: the first 16 hex characters of the SHA-256 of the parts joined by newlines.
#[must_use]
pub fn short(parts: &[&str]) -> String {
    let digest = rightkit_crypto::hex_encode(&rightkit_crypto::sha256(parts.join("\n").as_bytes()));
    digest.chars().take(16).collect()
}

/// The fields of an engine source: the rule id, its revision and the occurrence key.
pub fn engine_source(envelope: &TaskEnvelope) -> AcceptResult<(&str, u64, &str)> {
    match &envelope.intent.source {
        IntentSource::Engine {
            rule_id,
            rule_revision,
            occurrence_key,
            ..
        } => Ok((rule_id, *rule_revision, occurrence_key)),
        _ => Err(refused(
            "agent.engine is started only by the engine, from a rule the owner approved (intent.source engine)",
        )),
    }
}

/// What the seal covers: the rule, its revision, the occurrence, the recipe and what names the task, so a seal fits one task only.
pub fn seal_text(envelope: &TaskEnvelope, recipe: &str) -> AcceptResult<String> {
    let (rule_id, revision, occurrence) = engine_source(envelope)?;
    Ok(format!(
        "pulse.agent-engine.v1\n{rule_id}\n{revision}\n{occurrence}\n{recipe}\n{}\n{}\n{}",
        envelope.idempotency_key, envelope.utterance.utterance_id, envelope.target_device.device_id,
    ))
}

/// Parses the params of an `agent.engine` task.
pub fn parse_params(params: &Value) -> AcceptResult<EngineParams> {
    let parsed: EngineParams = serde_json::from_value(params.clone())
        .map_err(|e| AcceptError::bad_input(format!("invalid agent.engine params: {e}")))?;
    if parsed.schema_version != 1 {
        return Err(AcceptError::bad_input(
            "unsupported agent.engine params schema version",
        ));
    }
    Ok(parsed)
}

/// The checks both submit and run start make, short of the host's record lookup: the shape, the source, the origin, no context
/// references, the seal and the recipe. `sealed(text, mac)` is the host's check that `mac` is this engine process's seal of
/// `text`. Returns the mode and the params. The host then checks the record that authorises the run (`engine_source` names it).
pub fn verify_envelope(
    params: &Value,
    envelope: &TaskEnvelope,
    sealed: impl Fn(&str, &str) -> bool,
) -> AcceptResult<(Mode, EngineParams)> {
    let parsed = parse_params(params)?;
    engine_source(envelope)?;
    // The contract's own check of an engine source: ids present, at most 256 bytes, no control character, a revision of 1 or more.
    envelope
        .intent
        .source
        .validate()
        .map_err(|_| AcceptError::bad_input("the engine source is malformed"))?;
    if envelope.origin.kind != PrincipalKind::LocalClient
        || envelope.origin.subject != ENGINE_SUBJECT
    {
        return Err(refused(
            "agent.engine is accepted only from this engine's own principal",
        ));
    }
    if !envelope.context_refs.is_empty() {
        return Err(AcceptError::new(
            "unsupported_context",
            "agent.engine takes no context references",
        ));
    }
    let text = seal_text(envelope, &parsed.recipe)?;
    if !sealed(&text, &parsed.seal) {
        return Err(refused(
            "agent.engine needs this engine's own seal (none, or one from before a restart); the engine submits it again as a new occurrence",
        ));
    }
    let mode = mode_of(&parsed.recipe)
        .ok_or_else(|| AcceptError::new("unknown_recipe", "no such recipe"))?;
    Ok((mode, parsed))
}

/// The platform this build runs on.
#[must_use]
pub fn local_platform() -> DevicePlatform {
    if cfg!(target_os = "macos") {
        DevicePlatform::Mac
    } else if cfg!(target_os = "windows") {
        DevicePlatform::Windows
    } else {
        DevicePlatform::Linux
    }
}

/// The task envelope the engine submits for one occurrence: the engine source, the engine's own principal, this computer as the
/// target. The idempotency key is `engine:<occurrence>` and the utterance id is its digest, so one occurrence is one task.
#[must_use]
pub fn build_envelope(
    trigger: EngineTrigger,
    rule_id: &str,
    revision: u64,
    occurrence: &str,
    device_id: String,
    platform: DevicePlatform,
) -> TaskEnvelope {
    let idempotency_key = format!("engine:{occurrence}");
    let digest = rightkit_crypto::hex_encode(&rightkit_crypto::sha256(idempotency_key.as_bytes()));
    let utterance_id = format!(
        "{}-{}-{}-{}-{}",
        &digest[0..8],
        &digest[8..12],
        &digest[12..16],
        &digest[16..20],
        &digest[20..32]
    );
    TaskEnvelope {
        schema_version: TASK_ENVELOPE_SCHEMA_VERSION,
        task_id: String::new(),
        kind: crate::act_params::ENGINE_KIND.to_string(),
        conversation_id: utterance_id.clone(),
        utterance: UtteranceRef {
            schema_version: UTTERANCE_REF_SCHEMA_VERSION,
            utterance_id,
            generation: 1,
        },
        origin: Principal {
            schema_version: PRINCIPAL_SCHEMA_VERSION,
            kind: PrincipalKind::LocalClient,
            subject: ENGINE_SUBJECT.to_string(),
            connection: Some(device_id.clone()),
        },
        context_refs: Vec::new(),
        authority: None,
        intent: Intent {
            schema_version: INTENT_SCHEMA_VERSION,
            source: IntentSource::Engine {
                trigger,
                rule_id: rule_id.to_string(),
                rule_revision: revision,
                occurrence_key: occurrence.to_string(),
            },
        },
        target_device: DeviceRef {
            schema_version: DEVICE_REF_SCHEMA_VERSION,
            device_id,
            platform,
            display_name: None,
        },
        idempotency_key,
        deadline_ms: None,
        policy_ref: POLICY_REF.to_string(),
        owner_epoch: 0,
        status: TaskStatus::Accepted,
        checkpoint_ref: None,
        output_refs: Vec::new(),
    }
}

/// The task body: the envelope as JSON with `params` set to the recipe and the host-made `seal` (of [`seal_text`]).
pub fn build_params(envelope: &TaskEnvelope, recipe: &str, seal: &str) -> AcceptResult<Value> {
    let mut body =
        serde_json::to_value(envelope).map_err(|e| AcceptError::bad_input(e.to_string()))?;
    if let Value::Object(map) = &mut body {
        map.insert(
            "params".into(),
            json!({ "schema_version": 1, "recipe": recipe, "seal": seal }),
        );
    }
    Ok(body)
}

/// One card of a brief, cleaned and capped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BriefCard {
    /// Its position in the reply, for a stable id.
    pub index: usize,
    /// `title`, or `title: detail`.
    pub finding: String,
}

/// Plain text: control characters become spaces, runs of whitespace one space, cut to `max` characters.
#[must_use]
pub fn clean(text: &str, max: usize) -> String {
    let plain: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    plain
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max)
        .collect()
}

/// The brief's answer: `{"cards": [...]}`, read strictly (an optional code fence is allowed; more than [`CARDS_MAX`] cards, or
/// anything else, is unreadable). A card with no title is skipped. `Err(())` is `brief_unreadable`.
#[allow(clippy::result_unit_err)]
pub fn parse_brief_cards(text: &str) -> Result<Vec<BriefCard>, ()> {
    let body = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let value: Value = serde_json::from_str(body).map_err(|_| ())?;
    let list = value.get("cards").and_then(Value::as_array).ok_or(())?;
    if list.len() > CARDS_MAX {
        return Err(());
    }
    let mut cards = Vec::new();
    for (index, card) in list.iter().enumerate() {
        let title = clean(
            card.get("title").and_then(Value::as_str).unwrap_or(""),
            TITLE_MAX_CHARS,
        );
        if title.is_empty() {
            continue;
        }
        let detail = clean(
            card.get("detail").and_then(Value::as_str).unwrap_or(""),
            DETAIL_MAX_CHARS,
        );
        let finding = if detail.is_empty() {
            title
        } else {
            format!("{title}: {detail}")
        };
        cards.push(BriefCard { index, finding });
    }
    Ok(cards)
}
