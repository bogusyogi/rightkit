//! `agent.relay`: the owner's phone asked, with its signed owner check, for a request to run on ANOTHER of the owner's computers
//! than the one the phone talks to (the duty host). This kind lives on the duty host and carries that request there.
//!
//! **Authority.** The owner's signature, twice. The duty host checks the phone's signature when it takes the request; the target
//! checks the SAME signature again against the key the owner enrolled there, so a request signed for one computer cannot run on
//! another and a duty host that was compromised cannot make one up. Nothing here carries an effect between computers: the target
//! starts its own act task, and every effect that task asks for waits for its own approval there.
//!
//! **Parking.** A relay task is `accepted`; the host moves it to `waiting-device` while the target has no fresh presence report and
//! leases it again when it has. A task whose signature expired while it waited fails `relay_expired` and is never run late.
//!
//! **Who may submit it.** Only the engine: `params.seal` is its own MAC over what names the task. A client's submit of this kind
//! carries no seal and is refused.
//!
//! This module holds the acceptance rules, the task body, the follow limits and the reading of the target's answers. The device
//! link, the presence report, the seal's MAC and the poll loop are the host's.

use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::act_params::{AcceptError, AcceptResult};
use crate::device::{DEVICE_REF_SCHEMA_VERSION, DevicePlatform, DeviceRef};
use crate::intent::{INTENT_SCHEMA_VERSION, Intent};
use crate::principal::{PRINCIPAL_SCHEMA_VERSION, Principal};
use crate::task::{
    TASK_ENVELOPE_SCHEMA_VERSION, TaskEnvelope, TaskStatus, UTTERANCE_REF_SCHEMA_VERSION,
    UtteranceRef,
};
use crate::{IntentSource, PrincipalKind};

/// The job kind.
pub const RELAY_KIND: &str = "agent.relay";
/// The `policy_ref` of a relay task.
pub const POLICY_REF: &str = "agent-relay/v1";
/// The principal subject of the engine's own relay submits.
pub const RELAY_SUBJECT: &str = "phone-relay";
/// Longest request, in bytes (what `agent.act` accepts).
pub const REQUEST_MAX: usize = 4096;
/// How often the target's task is asked about.
pub const POLL: Duration = Duration::from_secs(4);
/// A target that has not answered for this long is lost.
pub const LOST_AFTER: Duration = Duration::from_secs(90);
/// The most a relayed run is followed, from forward to result.
pub const FOLLOW_MAX: Duration = Duration::from_secs(35 * 60);
/// How often the heartbeat is written (a task's note).
pub const BEAT_EVERY: Duration = Duration::from_secs(15);

/// The params of an `agent.relay` task.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayParams {
    pub schema_version: u32,
    /// The phone whose signature this is, and its client message id.
    pub phone: String,
    pub client: String,
    pub nonce: String,
    pub key_id: String,
    pub sig: String,
    pub expires_ms: u64,
    pub seal: String,
}

fn refused(message: &str) -> AcceptError {
    AcceptError::new("unsupported_origin", message)
}

/// What the seal covers: the task's names, the SHA-256 of the quote and the phone.
#[must_use]
pub fn seal_text(envelope: &TaskEnvelope, phone: &str) -> String {
    let quote = match &envelope.intent.source {
        IntentSource::Direct { quote, .. } => quote.as_str(),
        _ => "",
    };
    format!(
        "pulse.agent-relay.v1\n{}\n{}\n{}\n{}\n{phone}",
        envelope.idempotency_key,
        envelope.utterance.utterance_id,
        envelope.target_device.device_id,
        rightkit_crypto::hex_encode(&rightkit_crypto::sha256(quote.as_bytes())),
    )
}

/// Parses the params of an `agent.relay` task.
pub fn parse_params(params: &Value) -> AcceptResult<RelayParams> {
    let parsed: RelayParams = serde_json::from_value(params.clone())
        .map_err(|e| AcceptError::bad_input(format!("invalid agent.relay params: {e}")))?;
    if parsed.schema_version != 1 {
        return Err(AcceptError::bad_input(
            "unsupported agent.relay params schema version",
        ));
    }
    Ok(parsed)
}

/// The owner's own request, quoted in `intent.source` (direct), 1 to [`REQUEST_MAX`] bytes.
pub fn quote_of(envelope: &TaskEnvelope) -> AcceptResult<&str> {
    match &envelope.intent.source {
        IntentSource::Direct { quote, .. }
            if !quote.trim().is_empty() && quote.len() <= REQUEST_MAX =>
        {
            Ok(quote)
        }
        _ => Err(refused(
            "agent.relay needs the owner's own request quoted in intent.source (direct)",
        )),
    }
}

/// The checks both submit and run make: the shape, the quote, the engine's own principal, no context references, the seal (only
/// this engine makes one) and the target (never this computer). `sealed(text, mac)` is the host's check that `mac` is this engine
/// process's seal of `text`; `local_device` is this computer's device id, when known.
pub fn verify(
    params: &Value,
    envelope: &TaskEnvelope,
    local_device: Option<&str>,
    sealed: impl Fn(&str, &str) -> bool,
) -> AcceptResult<RelayParams> {
    let parsed = parse_params(params)?;
    quote_of(envelope)?;
    if envelope.origin.kind != PrincipalKind::LocalClient
        || envelope.origin.subject != RELAY_SUBJECT
    {
        return Err(refused(
            "agent.relay is accepted only from this engine's own principal",
        ));
    }
    if !envelope.context_refs.is_empty() {
        return Err(AcceptError::new(
            "unsupported_context",
            "agent.relay takes no context references",
        ));
    }
    if !sealed(&seal_text(envelope, &parsed.phone), &parsed.seal) {
        return Err(refused(
            "agent.relay needs this engine's own seal (none, or one from before a restart); the phone asks again",
        ));
    }
    if local_device == Some(envelope.target_device.device_id.as_str()) {
        return Err(AcceptError::new(
            "not_a_relay",
            "the request names this computer; it runs here as an ordinary act",
        ));
    }
    Ok(parsed)
}

/// Whether a stored relay task's signature has expired (`params.expires_ms` at or before `now_ms`), so it must fail rather than
/// wait or run.
#[must_use]
pub fn expired(params: &Value, now_ms: u64) -> bool {
    params
        .get("expires_ms")
        .and_then(Value::as_u64)
        .is_some_and(|at| at <= now_ms)
}

/// The text a phone's signed values come in as, from the `signed` block (`nonce`, `keyId`, `sig`, `expiresMs`).
fn signed_text(signed: &Value, name: &str) -> String {
    signed
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// The task envelope the duty host submits once the phone's signature has been verified there: the relay of `request` to
/// `target`, whose platform the host found in its pairing registry. `local_device` is this computer's device id.
#[must_use]
pub fn build_envelope(
    phone: &str,
    client: &str,
    request: &str,
    target: &str,
    key: &str,
    local_device: String,
    target_platform: DevicePlatform,
) -> TaskEnvelope {
    TaskEnvelope {
        schema_version: TASK_ENVELOPE_SCHEMA_VERSION,
        task_id: String::new(),
        kind: RELAY_KIND.to_string(),
        conversation_id: format!("phone-thread:{phone}"),
        utterance: UtteranceRef {
            schema_version: UTTERANCE_REF_SCHEMA_VERSION,
            utterance_id: key.to_string(),
            generation: 1,
        },
        origin: Principal {
            schema_version: PRINCIPAL_SCHEMA_VERSION,
            kind: PrincipalKind::LocalClient,
            subject: RELAY_SUBJECT.to_string(),
            connection: Some(local_device),
        },
        context_refs: Vec::new(),
        authority: None,
        intent: Intent {
            schema_version: INTENT_SCHEMA_VERSION,
            source: IntentSource::Direct {
                chat: RELAY_SUBJECT.to_string(),
                device: phone.to_string(),
                message_id: client.to_string(),
                quote: request.to_string(),
            },
        },
        target_device: DeviceRef {
            schema_version: DEVICE_REF_SCHEMA_VERSION,
            device_id: target.to_string(),
            platform: target_platform,
            display_name: None,
        },
        idempotency_key: key.to_string(),
        deadline_ms: None,
        policy_ref: POLICY_REF.to_string(),
        owner_epoch: 0,
        status: TaskStatus::Accepted,
        checkpoint_ref: None,
        output_refs: Vec::new(),
    }
}

/// The task body: the envelope as JSON with `params` made of the phone's signed values (`signed` carries `nonce`, `keyId`, `sig`
/// and the challenge's `expiresMs`) and the host-made `seal` (of [`seal_text`]).
pub fn build_params(
    envelope: &TaskEnvelope,
    phone: &str,
    client: &str,
    signed: &Value,
    seal: &str,
) -> AcceptResult<Value> {
    let expires_ms = signed.get("expiresMs").and_then(Value::as_u64).unwrap_or(0);
    let mut body =
        serde_json::to_value(envelope).map_err(|e| AcceptError::bad_input(e.to_string()))?;
    body["params"] = json!({
        "schema_version": 1,
        "phone": phone,
        "client": client,
        "nonce": signed_text(signed, "nonce"),
        "key_id": signed_text(signed, "keyId"),
        "sig": signed_text(signed, "sig"),
        "expires_ms": expires_ms,
        "seal": seal,
    });
    Ok(body)
}

/// Errors from the target that will not get better by waiting: the request itself was refused there.
#[must_use]
pub fn permanent(code: &str) -> bool {
    matches!(
        code,
        "not_granted"
            | "forbidden"
            | "sig_invalid"
            | "signature_required"
            | "key_not_enrolled"
            | "challenge_missing"
            | "challenge_expired"
            | "nonce_used"
            | "nonce_mismatch"
            | "phone_not_paired_here"
            | "agent_actions_off"
            | "invalid_params"
            | "unsupported"
    )
}

/// The body forwarded to the target (`task.relay`) from a stored task's params and its quoted request.
#[must_use]
pub fn forward_body(parsed: &RelayParams, request: &str) -> Value {
    json!({
        "phone": parsed.phone,
        "client": parsed.client,
        "request": request,
        "nonce": parsed.nonce,
        "keyId": parsed.key_id,
        "sig": parsed.sig,
        "expiresMs": parsed.expires_ms,
    })
}

/// The question put to the target about its own task (`task.relay.result`, `task.relay.cancel`).
#[must_use]
pub fn follow_query(parsed: &RelayParams, remote_id: &str) -> Value {
    json!({ "taskId": remote_id, "phone": parsed.phone, "client": parsed.client })
}

/// The target's finished task as this task's end: its text on success, its own code on failure (a code of letters, digits and
/// `_` only; anything else is `relay_failed`). `Err` is `(code, message)`.
pub fn finish(answer: &Value) -> Result<Value, (String, String)> {
    match answer.get("status").and_then(Value::as_str) {
        Some("succeeded") => Ok(json!({
            "schema_version": 1,
            "kind": RELAY_KIND,
            "text": answer.get("text").cloned().unwrap_or(Value::Null),
        })),
        _ => {
            let code = answer
                .pointer("/error/code")
                .and_then(Value::as_str)
                .filter(|c| c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_'))
                .unwrap_or("relay_failed");
            Err((
                code.to_string(),
                "the run on the other computer did not succeed".to_string(),
            ))
        }
    }
}
