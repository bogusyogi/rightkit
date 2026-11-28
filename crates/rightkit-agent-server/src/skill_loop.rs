//! Wires the CoEvoSkills synthesis loop to the real LLM provider.
//!
//! `build_skill_synthesis_loop` constructs a `SkillSynthesisLoop` with live
//! `SynthesizeFn` and `VerifyFn` callbacks that call the configured provider,
//! replacing the deterministic stubs used by `synthesize_blocking`.

use crate::api::EngineConfig;
use rightkit_agent_learning::{SkillSynthesisLoop, SynthesisConfig, SynthesizeFn, VerifyFn};
use std::sync::Arc;

/// Build a `SkillSynthesisLoop` wired to the daemon's selected provider.
///
/// - `synthesize_fn`: sends the synthesis prompt to the LLM and returns the
///   SKILL.md-format body.
/// - `verify_fn`: sends the body to the LLM with a verification prompt and
///   parses `{"score": 0.0-1.0, "feedback": "..."}` from the response.
pub fn build_skill_synthesis_loop(config: &EngineConfig) -> SkillSynthesisLoop {
    // Both calls take the checked side-call path (`complete_projection`): egress pin, native
    // sign-in, and the Messages protocol on `api.anthropic.com`.
    let synth_config = config.clone();
    let verify_config = config.clone();

    let synthesize_fn: SynthesizeFn = Arc::new(move |prompt: String| {
        let c = synth_config.clone();
        Box::pin(async move {
            let messages = vec![
                serde_json::json!({
                    "role": "system",
                    "content": "You are a skill synthesis engine. Output only the requested \
                                SKILL.md-format content, no explanation. Start with a markdown \
                                heading and include ## Trigger, ## Body, and ## Notes sections."
                }),
                serde_json::json!({ "role": "user", "content": prompt }),
            ];
            crate::provider_client::complete_projection(&c, messages)
                .await
                .map(|t| t.text)
                .map_err(|e| e.to_string())
        })
    });

    let verify_fn: VerifyFn = Arc::new(move |body: String| {
        let c = verify_config.clone();
        Box::pin(async move {
            let prompt = format!(
                "Verify this skill definition. Is it correct and useful? \
                 Return JSON with exactly this shape: \
                 {{\"score\": <float 0.0-1.0>, \"feedback\": \"<actionable suggestions or 'looks good'>\"}}. \
                 Return only the JSON object, no other text.\n\n{body}"
            );
            let messages = vec![
                serde_json::json!({
                    "role": "system",
                    "content": "You are an impartial skill verifier. Return only the requested JSON object."
                }),
                serde_json::json!({ "role": "user", "content": prompt }),
            ];
            let text = crate::provider_client::complete_projection(&c, messages)
                .await
                .map(|t| t.text)
                .map_err(|e| e.to_string())?;

            // Strip optional markdown fences then parse.
            let json_str = strip_json_fence(text.trim());
            let parsed: serde_json::Value =
                serde_json::from_str(json_str).unwrap_or(serde_json::Value::Null);

            let score = parsed
                .get("score")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.3)
                .clamp(0.0, 1.0);

            let feedback = parsed
                .get("feedback")
                .and_then(|v| v.as_str())
                .unwrap_or("no feedback")
                .to_string();

            Ok((score, feedback))
        })
    });

    SkillSynthesisLoop::new(SynthesisConfig::default())
        .with_synthesizer(synthesize_fn)
        .with_verifier(verify_fn)
}

fn strip_json_fence(s: &str) -> &str {
    let s = s.trim();
    if let Some(inner) = s.strip_prefix("```json").or_else(|| s.strip_prefix("```")) {
        if let Some(end) = inner.rfind("```") {
            return inner[..end].trim();
        }
        return inner.trim();
    }
    s
}
