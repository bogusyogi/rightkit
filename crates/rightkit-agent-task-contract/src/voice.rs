//! Voice hand-off: the typed record a voice-addressed CONTROL utterance
//! arrives as, and its mapping onto a [`TaskEnvelope`].
//!
//! Only addressed CONTROL can be expressed here: [`VoiceMode`] has no other
//! variant, so dictation cannot be represented and cannot reach the agent. A
//! submission is accepted only when R0 found no exact local catalog match and
//! R1 left the request unresolved as an agent request. Anything R1 resolved
//! locally stays with the voice front end.
//!
//! The wake id, confidence and check references are kept on the hand-off for
//! audit. The envelope has no field for them, so they are not carried over.
//! Transcript text appears only as the intent quote, never in other fields.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::device::DeviceRef;
use super::intent::{INTENT_SCHEMA_VERSION, Intent, IntentSource};
use super::principal::{PRINCIPAL_SCHEMA_VERSION, Principal, PrincipalKind};
use super::task::{
    TASK_ENVELOPE_SCHEMA_VERSION, TaskEnvelope, TaskStatus, UTTERANCE_REF_SCHEMA_VERSION,
    UtteranceRef,
};

/// Version of the [`VoiceHandoff`] record.
pub const VOICE_HANDOFF_SCHEMA_VERSION: u32 = 1;
/// Vocabulary version for [`VoiceMode`].
pub const VOICE_MODE_SCHEMA_VERSION: u32 = 1;
/// Version of the [`VoiceChecks`] record.
pub const VOICE_CHECKS_SCHEMA_VERSION: u32 = 1;
/// Vocabulary version for [`R0Outcome`] and [`R1Outcome`].
pub const VOICE_CHECK_OUTCOME_SCHEMA_VERSION: u32 = 1;
/// Largest accepted transcript, in bytes.
pub const MAX_TRANSCRIPT_BYTES: usize = 4096;

/// The labels a hand-off is mapped with. They name a product and its policy, so the caller
/// supplies them; the contract holds no product's names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceLabels {
    /// Chat label that `intent.source` records for the voice front end's control path, such as
    /// `<voice-app>:control`.
    pub chat: String,
    /// Policy reference applied to voice-submitted tasks, such as `voice-control/v1`.
    pub policy_ref: String,
}

/// What the voice front end sends for one final addressed CONTROL utterance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoiceHandoff {
    pub schema_version: u32,
    /// Stable utterance id and generation, used for dedupe and cancel.
    pub utterance: UtteranceRef,
    /// Conversation session the wake opened.
    pub session_id: String,
    /// Wake event that opened the session, when one did. The voice front end's push-to-talk control opens a
    /// session with no wake word, so it is optional (the voice front end, 2026-10-16). A wake alone submits nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_id: Option<String>,
    /// Final ASR text of the utterance.
    pub transcript: String,
    /// ASR confidence, from 0 to 1. Recorded for audit, not used as a gate.
    pub confidence: f32,
    /// Device the voice came from. The task targets it.
    pub device: DeviceRef,
    /// The owner, who speaks through this device.
    pub owner_id: String,
    pub mode: VoiceMode,
    pub checks: VoiceChecks,
    /// Least-privilege context references the voice front end chose to share. Empty by default.
    #[serde(default)]
    pub context_refs: Vec<String>,
}

/// The only mode that can reach the agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VoiceMode {
    /// Addressed CONTROL: the utterance was addressed to the agent in control mode.
    AddressedControl,
}

/// Results of the voice front end's deterministic checks on the utterance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoiceChecks {
    pub schema_version: u32,
    pub r0: R0Check,
    pub r1: R1Check,
}

/// R0: exact catalog match, run first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct R0Check {
    pub outcome: R0Outcome,
    /// Version of the exact catalog that was checked.
    pub catalog_version: String,
}

/// R1: grammar and registry resolution, run after R0.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct R1Check {
    pub outcome: R1Outcome,
    /// Version of the destination and action registry that was checked.
    pub registry_version: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum R0Outcome {
    /// An exact catalog entry matched. The voice front end runs it locally; it is not sent to the agent.
    ExactCatalogMatch,
    /// No exact match. R1 decides what happens next.
    NoExactMatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum R1Outcome {
    /// Resolved to a local action. The voice front end runs it; it is not sent to the agent.
    ResolvedLocal,
    /// Unresolved agent request. This is the only outcome that submits.
    UnresolvedAgentRequest,
}

/// Why a hand-off was refused before it reached the engine.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum VoiceError {
    #[error("unsupported voice hand-off schema version {0}")]
    UnsupportedSchema(u32),
    #[error("voice hand-off field {0} is empty")]
    Empty(&'static str),
    #[error("confidence must be a finite number from 0 to 1")]
    Confidence,
    #[error("transcript exceeds 4096 bytes")]
    TranscriptTooLong,
    #[error(
        "not an agent request (R0 {r0:?}, R1 {r1:?}); only an unresolved agent request is submitted to the agent"
    )]
    NotAgentRequest { r0: R0Outcome, r1: R1Outcome },
}

fn non_empty(field: &'static str, value: &str) -> Result<(), VoiceError> {
    if value.trim().is_empty() {
        Err(VoiceError::Empty(field))
    } else {
        Ok(())
    }
}

impl VoiceHandoff {
    /// Checks the hand-off before it is mapped or sent. Pure; no runtime state.
    pub fn validate(&self) -> Result<(), VoiceError> {
        if self.schema_version != VOICE_HANDOFF_SCHEMA_VERSION {
            return Err(VoiceError::UnsupportedSchema(self.schema_version));
        }
        if self.utterance.schema_version != UTTERANCE_REF_SCHEMA_VERSION {
            return Err(VoiceError::UnsupportedSchema(self.utterance.schema_version));
        }
        if self.checks.schema_version != VOICE_CHECKS_SCHEMA_VERSION {
            return Err(VoiceError::UnsupportedSchema(self.checks.schema_version));
        }
        non_empty("utterance_id", &self.utterance.utterance_id)?;
        non_empty("session_id", &self.session_id)?;
        if let Some(wake) = &self.wake_id {
            non_empty("wake_id", wake)?;
        }
        non_empty("transcript", &self.transcript)?;
        non_empty("owner_id", &self.owner_id)?;
        non_empty("device_id", &self.device.device_id)?;
        if !self.confidence.is_finite() || !(0.0..=1.0).contains(&self.confidence) {
            return Err(VoiceError::Confidence);
        }
        if self.transcript.len() > MAX_TRANSCRIPT_BYTES {
            return Err(VoiceError::TranscriptTooLong);
        }
        if self.checks.r0.outcome != R0Outcome::NoExactMatch
            || self.checks.r1.outcome != R1Outcome::UnresolvedAgentRequest
        {
            return Err(VoiceError::NotAgentRequest {
                r0: self.checks.r0.outcome,
                r1: self.checks.r1.outcome,
            });
        }
        Ok(())
    }

    /// Maps a validated hand-off onto a [`TaskEnvelope`], labelled with `labels`.
    ///
    /// * `task_id` is empty: the engine mints it at ACCEPTANCE.
    /// * `origin` is the owner, connected through the voice device.
    /// * `intent.source` is `direct`, quoting the transcript with a pointer to
    ///   the utterance and generation. the owner's own words are the authority.
    /// * `authority` is `None`: the intent alone authorizes the act.
    /// * `status` is a placeholder. The engine sets the real status at ACCEPTANCE.
    /// * `idempotency_key` is derived from the utterance, so a resend after an
    ///   unknown outcome resolves to the same task.
    pub fn to_task_envelope(&self, labels: &VoiceLabels) -> Result<TaskEnvelope, VoiceError> {
        self.validate()?;
        non_empty("chat label", &labels.chat)?;
        non_empty("policy reference", &labels.policy_ref)?;
        let utterance_id = &self.utterance.utterance_id;
        let generation = self.utterance.generation;
        let device_id = &self.device.device_id;
        Ok(TaskEnvelope {
            schema_version: TASK_ENVELOPE_SCHEMA_VERSION,
            task_id: String::new(),
            // Empty: a hand-off names no job kind. the task submit command fills in `agent.request` when
            // no `--kind` is given, and `--kind` still wins. The engine refuses an empty kind at submit.
            kind: String::new(),
            conversation_id: self.session_id.clone(),
            utterance: self.utterance.clone(),
            origin: Principal {
                schema_version: PRINCIPAL_SCHEMA_VERSION,
                kind: PrincipalKind::Owner,
                subject: self.owner_id.clone(),
                connection: Some(device_id.clone()),
            },
            context_refs: self.context_refs.clone(),
            authority: None,
            intent: Intent {
                schema_version: INTENT_SCHEMA_VERSION,
                source: IntentSource::Direct {
                    chat: labels.chat.clone(),
                    device: device_id.clone(),
                    message_id: format!("{utterance_id}#{generation}"),
                    quote: self.transcript.clone(),
                },
            },
            target_device: self.device.clone(),
            idempotency_key: format!("voice:{utterance_id}:{generation}"),
            deadline_ms: None,
            policy_ref: labels.policy_ref.clone(),
            owner_epoch: 0,
            status: TaskStatus::Accepted,
            checkpoint_ref: None,
            output_refs: Vec::new(),
        })
    }
}
