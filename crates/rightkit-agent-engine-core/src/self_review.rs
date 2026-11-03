//! Restricted post-turn self-review fork plumbing.
//!
//! The engine owns the timing and restriction envelope. A daemon-side learning
//! sink owns persistence, so review output enters quarantine proposals rather
//! than live memory/instructions.

use crate::transcript::Transcript;
use rightkit_agent_engine_protocol::TranscriptMessage;
use std::sync::{Arc, Mutex, OnceLock};

pub const DEFAULT_SELF_REVIEW_TURN_INTERVAL: usize = 5;
pub const SELF_REVIEW_MODEL: &str = "haiku";
pub const SELF_REVIEW_ALLOWED_TOOLS: &[&str] = &[
    "read_memory",
    "write_memory",
    "learning_propose",
    "remember_consolidated",
];

/// A proposal drafted by the restricted review fork. The daemon learning
/// harness converts this into its canonical `Proposal` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfReviewProposalDraft {
    pub id: String,
    pub kind: SelfReviewProposalKind,
    pub summary: String,
    pub evidence: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelfReviewProposalKind {
    Memory,
    HarnessRule,
    Workflow,
    SkillTweak,
    Skill,
}

/// Immutable input passed to a restricted async fork after a visible assistant
/// turn has been committed to the transcript.
#[derive(Debug, Clone)]
pub struct SelfReviewInput {
    pub session_id: String,
    pub model: String,
    pub allowed_tools: Vec<String>,
    pub shell_allowed: bool,
    pub stable_system_prompt_bytes: Vec<u8>,
    pub stable_system_cache_key: Option<String>,
    pub transcript: Vec<TranscriptMessage>,
    pub assistant_text: String,
}

impl SelfReviewInput {
    pub fn from_transcript(
        session_id: String,
        transcript: &Transcript,
        assistant_text: String,
        stable_system_cache_key: Option<String>,
    ) -> Self {
        Self {
            session_id,
            model: SELF_REVIEW_MODEL.to_string(),
            allowed_tools: SELF_REVIEW_ALLOWED_TOOLS
                .iter()
                .map(|tool| (*tool).to_string())
                .collect(),
            shell_allowed: false,
            stable_system_prompt_bytes: stable_system_prompt_bytes(transcript.messages()),
            stable_system_cache_key,
            transcript: transcript.messages().to_vec(),
            assistant_text,
        }
    }

    /// Return whether this input carries the complete restricted review
    /// envelope. The background fork has no shell or arbitrary tool authority.
    pub fn is_governed(&self) -> bool {
        self.model == SELF_REVIEW_MODEL
            && !self.shell_allowed
            && self.allowed_tools.len() == SELF_REVIEW_ALLOWED_TOOLS.len()
            && self
                .allowed_tools
                .iter()
                .zip(SELF_REVIEW_ALLOWED_TOOLS)
                .all(|(actual, expected)| actual == expected)
    }
}

/// Sink implemented by the learning layer. It is deliberately tiny: the fork
/// may only file quarantined learning proposals through this contract.
pub trait RestrictedSelfReviewFork: Send + Sync {
    fn review_and_propose(&self, input: SelfReviewInput);
}

static GLOBAL_SELF_REVIEW_FORK: OnceLock<Mutex<Option<Arc<dyn RestrictedSelfReviewFork>>>> =
    OnceLock::new();

pub fn set_global_self_review_fork(fork: Option<Arc<dyn RestrictedSelfReviewFork>>) {
    let slot = GLOBAL_SELF_REVIEW_FORK.get_or_init(|| Mutex::new(None));
    *slot.lock().unwrap() = fork;
}

pub fn global_self_review_fork() -> Option<Arc<dyn RestrictedSelfReviewFork>> {
    GLOBAL_SELF_REVIEW_FORK
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap()
        .clone()
}

/// Stable proposal identity shared by the daemon adapter and learning sink.
pub fn stable_self_review_proposal_id(session_id: &str, summary: &str) -> String {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    session_id.hash(&mut hasher);
    summary.hash(&mut hasher);
    format!("self-review-{session_id}-{:016x}", hasher.finish())
}

pub fn stable_system_prompt_bytes(messages: &[TranscriptMessage]) -> Vec<u8> {
    let mut prefix = String::new();
    for message in messages {
        match message {
            TranscriptMessage::System {
                content, pinned, ..
            } if *pinned => {
                prefix.push_str(content);
                prefix.push('\n');
            }
            TranscriptMessage::System { .. } => {}
            _ => break,
        }
    }
    prefix.into_bytes()
}
