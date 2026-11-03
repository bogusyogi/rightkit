//! Session recap generation (ENG-83).
//!
//! This module only coordinates the shared small-model seam: it bounds the
//! transcript, requests the typed `Recap` task, and treats a blank response as
//! absent. An unavailable provider is never represented as a successful recap;
//! the client's typed error is returned to the caller instead.
//!
//! External-artifact gap: this module does not provision a small-model route.
//! `SmallModelError::NoRouteAvailable` (or another client error) remains
//! visible when the configured provider is unavailable.

use crate::coding::small_model_utility::{
    SmallModelBudget, SmallModelClient, SmallModelError, SmallModelInput, SmallModelOutput,
    SmallModelTask, DEFAULT_CALL_TOKEN_ESTIMATE, DEFAULT_INPUT_BYTE_CAP,
};
use thiserror::Error;

/// The typed recap returned after a non-empty session has been summarized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecap {
    /// Short model-generated summary suitable for display or memory storage.
    pub summary: String,
}

/// Complete ENG-83 assist payload. Knowledge briefing is source material,
/// never model-invented; recap & next instruction are typed small-model output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionBriefing {
    pub recap: String,
    pub suggested_follow_up: String,
    pub knowledge_briefing: String,
}

/// Failures while requesting or decoding a session recap.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SessionRecapError {
    /// The configured small-model client could not complete the request.
    #[error("small-model recap failed: {0}")]
    SmallModel(#[from] SmallModelError),
    /// The client returned a different typed result than the requested recap.
    #[error("small-model client returned a non-recap output")]
    UnexpectedOutput,
}

/// Produce recap plus exactly one suggested next user instruction. Pinned
/// project knowledge remains verbatim in `knowledge_briefing` so it can be
/// injected without granting it authority or letting a model silently rewrite
/// it. No provider failure is converted into a fabricated suggestion.
pub async fn brief_session<C: SmallModelClient + ?Sized>(
    client: &C,
    session_content: &str,
    knowledge_briefing: &str,
    budget: &mut SmallModelBudget,
) -> Result<Option<SessionBriefing>, SessionRecapError> {
    let Some(recap) = summarize_session(client, session_content, budget).await? else {
        return Ok(None);
    };
    budget.spend(DEFAULT_CALL_TOKEN_ESTIMATE);
    let follow_up_input = SmallModelInput::bounded(
        &format!(
            "Session recap:\n{}\n\nPinned project knowledge:\n{}",
            recap.summary, knowledge_briefing
        ),
        DEFAULT_INPUT_BYTE_CAP,
    );
    let output = client
        .run(SmallModelTask::SessionFollowUp, &follow_up_input, budget)
        .await?;
    let SmallModelOutput::SessionFollowUp { suggestion } = output else {
        return Err(SessionRecapError::UnexpectedOutput);
    };
    let suggested_follow_up = suggestion.trim().to_string();
    if suggested_follow_up.is_empty() {
        return Ok(None);
    }
    budget.spend(DEFAULT_CALL_TOKEN_ESTIMATE);
    Ok(Some(SessionBriefing {
        recap: recap.summary,
        suggested_follow_up,
        knowledge_briefing: knowledge_briefing.trim().to_string(),
    }))
}

/// Summarize a session through the shared bounded small-model helper.
///
/// Empty or whitespace-only sessions return `Ok(None)` and do not spend a
/// client call. Non-empty input is passed through `SmallModelInput::bounded`
/// before the `SmallModelTask::Recap` request. A blank typed summary is also
/// treated as absent rather than surfaced as an empty banner or memory row.
pub async fn summarize_session<C: SmallModelClient + ?Sized>(
    client: &C,
    session_content: &str,
    budget: &SmallModelBudget,
) -> Result<Option<SessionRecap>, SessionRecapError> {
    if session_content.trim().is_empty() {
        return Ok(None);
    }

    let input = SmallModelInput::bounded(session_content, DEFAULT_INPUT_BYTE_CAP);
    let output = client.run(SmallModelTask::Recap, &input, budget).await?;
    match output {
        SmallModelOutput::Recap { summary } => {
            let summary = summary.trim().to_string();
            if summary.is_empty() {
                Ok(None)
            } else {
                Ok(Some(SessionRecap { summary }))
            }
        }
        SmallModelOutput::ProgressAudit { .. }
        | SmallModelOutput::DiffNarration { .. }
        | SmallModelOutput::SessionFollowUp { .. }
        | SmallModelOutput::Classify { .. } => Err(SessionRecapError::UnexpectedOutput),
    }
}
