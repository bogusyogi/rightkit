//! The typed LLM judge verdict.
//!
//! The model answers with one forced tool call. [`JudgeVerdict`] is the only shape that
//! answer is read into: unknown fields are rejected, every moment must name its sentence
//! ids and quote a hook, and scores must be finite and within 0..=1. Raw JSON stops at
//! this boundary; the ranker only sees typed moments.

use serde::Deserialize;
use serde_json::Value;

use crate::llm::{LlmMoment, ProviderError};

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeVerdict {
    pub moments: Vec<LlmMoment>,
}

impl JudgeVerdict {
    /// Parses and validates the forced tool call's input.
    pub fn from_tool_input(input: &Value) -> Result<Self, ProviderError> {
        let verdict: Self = serde_json::from_value(input.clone())
            .map_err(|e| ProviderError::Malformed(e.to_string()))?;
        verdict.validate()?;
        Ok(verdict)
    }

    /// Checks what the schema cannot: non-blank ids and hook, and a score in 0..=1.
    pub fn validate(&self) -> Result<(), ProviderError> {
        let bad = |m: String| -> Result<(), ProviderError> { Err(ProviderError::Malformed(m)) };
        for m in &self.moments {
            if m.start_sentence.trim().is_empty() || m.end_sentence.trim().is_empty() {
                return bad("moment without sentence ids".into());
            }
            if m.hook_text.trim().is_empty() {
                return bad(format!(
                    "moment {}..{} without a hook_text",
                    m.start_sentence, m.end_sentence
                ));
            }
            if !m.score.is_finite() || !(0.0..=1.0).contains(&m.score) {
                return bad(format!("score {} is outside 0..=1", m.score));
            }
        }
        Ok(())
    }
}
