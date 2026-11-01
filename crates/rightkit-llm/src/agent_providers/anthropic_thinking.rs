//! Anthropic `thinking` / `output_config.effort` request shaping.
//!
//! Sources (fetched 2026-10-17):
//! - <https://platform.claude.com/docs/en/build-with-claude/extended-thinking>
//!   (manual `{type:"enabled", budget_tokens}`: min 1024, `budget_tokens <
//!   max_tokens`; deprecated on 4.6, rejected with 400 on 4.7 and later)
//! - <https://platform.claude.com/docs/en/build-with-claude/thinking-steering-and-cost>
//!   (adaptive thinking; effort lives at `output_config.effort`, not inside
//!   `thinking`; thinking tokens count toward `max_tokens`)
//! - <https://platform.claude.com/docs/en/build-with-claude/thinking-troubleshooting>
//!   (per-model table of accepted and rejected `thinking.type` values)
//! - <https://platform.claude.com/docs/en/build-with-claude/effort>
//!   (per-model effort levels: low/medium/high/xhigh/max)
//! - <https://platform.claude.com/docs/en/build-with-claude/thinking>
//!   (`display`: newer models default to `"omitted"`, which returns empty
//!   thinking text; signed-block replay here is keyed by that text, so
//!   adaptive requests ask for `"summarized"`)

use serde_json::{json, Value};

/// Minimum accepted `budget_tokens`.
const MIN_BUDGET_TOKENS: u32 = 1024;
/// Docs advise batch processing above 32k; never ask for more interactively.
const MAX_BUDGET_TOKENS: u32 = 32_000;
/// Budget used for manual-thinking models when the operator sets none.
const DEFAULT_MANUAL_BUDGET_TOKENS: u32 = 8192;
/// Output room reserved for the visible answer beyond a manual budget.
const MANUAL_ANSWER_HEADROOM: u32 = 4096;
/// Default `max_tokens` for adaptive requests when the caller sets none.
/// Thinking counts toward `max_tokens`; the docs' own examples use 16000.
const ADAPTIVE_DEFAULT_MAX_TOKENS: u32 = 16_000;
/// Default `max_tokens` for requests that do not think.
const PLAIN_DEFAULT_MAX_TOKENS: u32 = 4096;

/// How a model family accepts thinking configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThinkingSupport {
    /// No thinking parameter; send nothing.
    None,
    /// Legacy `{type:"enabled", budget_tokens}` only (4.5 and earlier).
    Manual,
    /// `{type:"adaptive"}` (4.6 and later; 4.7+ reject `enabled`).
    Adaptive,
}

/// How a model turns thinking off when the operator asks for that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OffMode {
    /// Omit `thinking` (model is off by default, or cannot be turned off).
    Omit,
    /// `{type:"disabled"}`; accepted only at effort `high` or below.
    Disabled,
    /// `{type:"between_tools"}` (Sonnet 5.5); accepted only at `high` or below.
    BetweenTools,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Level {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Level {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "low" | "minimal" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::Xhigh),
            "max" => Some(Self::Max),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Family {
    Opus,
    Sonnet,
    Haiku,
    Fable,
    Mythos,
}

#[derive(Clone, Copy, Debug)]
struct Parsed {
    family: Family,
    major: u32,
    minor: u32,
    /// `mythos-preview`: accepts both modes; adaptive is preferred.
    preview: bool,
}

/// Parse ids such as `claude-opus-4-5-20251101`, `claude-sonnet-4-20250514`,
/// `claude-3-7-sonnet-20250219`, `anthropic/claude-sonnet-4.5`,
/// `us.anthropic.claude-opus-4-6-v1:0` and `claude-fable-5-1`.
fn parse_model(model_id: &str) -> Option<Parsed> {
    let lower = model_id.to_ascii_lowercase();
    let tokens: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    let pos = tokens
        .iter()
        .position(|t| matches!(*t, "opus" | "sonnet" | "haiku" | "fable" | "mythos"))?;
    let family = match tokens[pos] {
        "opus" => Family::Opus,
        "sonnet" => Family::Sonnet,
        "haiku" => Family::Haiku,
        "fable" => Family::Fable,
        _ => Family::Mythos,
    };
    // A version token is 1-2 digits; dates (8 digits) and `v1` are not.
    let version = |t: &str| (t.len() <= 2).then(|| t.parse::<u32>().ok()).flatten();
    let mut after = Vec::new();
    for token in &tokens[pos + 1..] {
        match version(token) {
            Some(v) if after.len() < 2 => after.push(v),
            _ => break,
        }
    }
    let (major, minor) = if !after.is_empty() {
        (after[0], after.get(1).copied().unwrap_or(0))
    } else {
        // `claude-3-7-sonnet`: version precedes the family.
        let mut before = Vec::new();
        for token in tokens[..pos].iter().rev() {
            match version(token) {
                Some(v) if before.len() < 2 => before.push(v),
                _ => break,
            }
        }
        before.reverse();
        match before.as_slice() {
            [major, minor] => (*major, *minor),
            [major] => (*major, 0),
            // `mythos-preview` carries no version; both families are 5+.
            _ if matches!(family, Family::Fable | Family::Mythos) => (5, 0),
            _ => return None,
        }
    };
    Some(Parsed {
        family,
        major,
        minor,
        preview: tokens.contains(&"preview"),
    })
}

/// Which thinking configuration the model accepts. Unknown ids get nothing,
/// so non-Anthropic models behind compatible gateways are untouched.
pub(crate) fn thinking_support(model_id: &str) -> ThinkingSupport {
    let Some(m) = parse_model(model_id) else {
        return ThinkingSupport::None;
    };
    match m.family {
        Family::Fable | Family::Mythos => ThinkingSupport::Adaptive,
        Family::Opus | Family::Sonnet | Family::Haiku => {
            if m.major >= 5 || (m.major == 4 && m.minor >= 6) {
                ThinkingSupport::Adaptive
            } else if m.major == 4 || (m.major == 3 && m.minor == 7) {
                ThinkingSupport::Manual
            } else {
                ThinkingSupport::None
            }
        }
    }
}

fn off_mode(m: &Parsed) -> OffMode {
    match (m.family, m.major, m.minor) {
        (Family::Opus, 5, 0) | (Family::Sonnet, 5, 0) | (Family::Haiku, 5, _) => OffMode::Disabled,
        (Family::Sonnet, 5, _) => OffMode::BetweenTools,
        _ => OffMode::Omit,
    }
}

/// Highest effort level the model accepts, or `None` when it has no effort
/// parameter. Opus 4.5 stops at `high`; `xhigh` is absent from Mythos Preview,
/// Opus 4.6 and Sonnet 4.6.
fn max_effort(m: &Parsed) -> Option<Level> {
    match m.family {
        Family::Fable | Family::Mythos => Some(Level::Max),
        Family::Opus => match (m.major, m.minor) {
            (4, 5) => Some(Level::High),
            (4, 6) => Some(Level::Max),
            (4, minor) if minor >= 7 => Some(Level::Max),
            (major, _) if major >= 5 => Some(Level::Max),
            _ => None,
        },
        Family::Sonnet => match (m.major, m.minor) {
            (4, 6) => Some(Level::Max),
            (major, _) if major >= 5 => Some(Level::Max),
            _ => None,
        },
        Family::Haiku if m.major >= 5 => Some(Level::Max),
        Family::Haiku => None,
    }
}

fn supports_xhigh(m: &Parsed) -> bool {
    match m.family {
        Family::Fable => true,
        Family::Mythos => !m.preview,
        Family::Opus => (m.major == 4 && m.minor >= 7) || m.major >= 5,
        Family::Sonnet => m.major >= 5,
        Family::Haiku => m.major >= 5,
    }
}

/// Clamp a requested level to what the model accepts (xhigh falls to high
/// where unsupported; Opus 4.5 caps at high).
fn clamp_level(m: &Parsed, requested: Level) -> Option<Level> {
    let cap = max_effort(m)?;
    let mut level = requested.min(cap);
    if level == Level::Xhigh && !supports_xhigh(m) {
        level = Level::High;
    }
    Some(level)
}

/// Operator-facing thinking settings carried by the client.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ThinkingSettings {
    pub effort: Option<String>,
    pub budget_tokens: Option<u32>,
    pub disabled: bool,
}

/// Request-body fragments chosen for one model and settings pair.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct ThinkingPlan {
    pub thinking: Option<Value>,
    pub effort: Option<&'static str>,
    pub max_tokens: u32,
}

/// Decide `thinking`, `output_config.effort` and `max_tokens`.
///
/// `caller_max_tokens` is the engine's explicit ceiling, never raised past
/// what the caller set; the client's own default is raised so thinking has
/// room. `tool_loop_lacks_thinking` is true when the trailing assistant turn
/// of a tool loop has no thinking block: manual mode requires the final
/// assistant turn to begin with one, so thinking is skipped for that request
/// rather than risking a 400 (adaptive has no such requirement).
pub(crate) fn plan(
    model_id: &str,
    settings: &ThinkingSettings,
    caller_max_tokens: Option<u32>,
    tool_loop_lacks_thinking: bool,
) -> ThinkingPlan {
    let default_max = caller_max_tokens.unwrap_or(PLAIN_DEFAULT_MAX_TOKENS);
    let plain = ThinkingPlan {
        thinking: None,
        effort: None,
        max_tokens: default_max,
    };
    let Some(m) = parse_model(model_id) else {
        return plain;
    };
    // `none` is the OpenAI-path spelling of "no reasoning".
    let wants_off = settings.disabled
        || settings
            .effort
            .as_deref()
            .is_some_and(|e| e.trim().eq_ignore_ascii_case("none"));
    let requested = settings.effort.as_deref().and_then(Level::parse);
    match thinking_support(model_id) {
        ThinkingSupport::None => plain,
        ThinkingSupport::Manual => {
            if wants_off || tool_loop_lacks_thinking {
                return plain;
            }
            let mut budget = settings
                .budget_tokens
                .unwrap_or(DEFAULT_MANUAL_BUDGET_TOKENS)
                .clamp(MIN_BUDGET_TOKENS, MAX_BUDGET_TOKENS);
            let max_tokens = match caller_max_tokens {
                // Leave room for an answer below the caller's ceiling.
                Some(cap) => {
                    let room = cap.saturating_sub(MIN_BUDGET_TOKENS);
                    if room < MIN_BUDGET_TOKENS {
                        return plain;
                    }
                    budget = budget.min(room);
                    cap
                }
                None => budget + MANUAL_ANSWER_HEADROOM,
            };
            ThinkingPlan {
                thinking: Some(json!({"type": "enabled", "budget_tokens": budget})),
                // Opus 4.5 composes effort with the budget; others have none.
                effort: requested
                    .and_then(|l| clamp_level(&m, l))
                    .map(Level::as_str),
                max_tokens,
            }
        }
        ThinkingSupport::Adaptive => {
            let max_tokens = caller_max_tokens.unwrap_or(ADAPTIVE_DEFAULT_MAX_TOKENS);
            let mut level = requested.and_then(|l| clamp_level(&m, l));
            let thinking = if wants_off {
                match off_mode(&m) {
                    OffMode::Omit => None,
                    OffMode::Disabled => {
                        level = level.map(|l| l.min(Level::High));
                        Some(json!({"type": "disabled"}))
                    }
                    OffMode::BetweenTools => {
                        level = level.map(|l| l.min(Level::High));
                        Some(json!({"type": "between_tools"}))
                    }
                }
            } else {
                Some(json!({"type": "adaptive", "display": "summarized"}))
            };
            ThinkingPlan {
                thinking,
                effort: level.map(Level::as_str),
                max_tokens,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(effort: Option<&str>, budget: Option<u32>, disabled: bool) -> ThinkingSettings {
        ThinkingSettings {
            effort: effort.map(str::to_string),
            budget_tokens: budget,
            disabled,
        }
    }

    #[test]
    fn model_families_classify_by_thinking_mode() {
        for adaptive in [
            "claude-opus-5-5",
            "claude-opus-5",
            "claude-opus-4-8",
            "claude-opus-4-7",
            "claude-opus-4-6",
            "claude-sonnet-5-5",
            "claude-sonnet-5",
            "claude-sonnet-4-6",
            "claude-haiku-5-5",
            "claude-fable-5-1",
            "claude-mythos-5",
            "claude-mythos-preview",
            "anthropic/claude-sonnet-4.6",
            "us.anthropic.claude-opus-4-6-v1:0",
        ] {
            assert_eq!(
                thinking_support(adaptive),
                ThinkingSupport::Adaptive,
                "{adaptive}"
            );
        }
        for manual in [
            "claude-opus-4-5-20251101",
            "claude-haiku-4-5-20251001",
            "claude-sonnet-4-5-20250929",
            "claude-opus-4-1-20250805",
            "claude-sonnet-4-20250514",
            "claude-opus-4-20250514",
            "claude-3-7-sonnet-20250219",
            "anthropic/claude-sonnet-4.5",
        ] {
            assert_eq!(
                thinking_support(manual),
                ThinkingSupport::Manual,
                "{manual}"
            );
        }
        for none in [
            "claude-3-5-sonnet-20241022",
            "claude-3-5-haiku-20241022",
            "claude-3-haiku-20240307",
            "gpt-5",
            "model",
            "",
        ] {
            assert_eq!(thinking_support(none), ThinkingSupport::None, "{none}");
        }
    }

    #[test]
    fn adaptive_models_get_adaptive_with_summarized_display_and_roomy_max_tokens() {
        let p = plan("claude-opus-4-8", &ThinkingSettings::default(), None, false);
        assert_eq!(
            p.thinking,
            Some(json!({"type": "adaptive", "display": "summarized"}))
        );
        assert_eq!(p.effort, None);
        assert_eq!(p.max_tokens, 16_000);
        // An explicit caller ceiling is never raised.
        let p = plan(
            "claude-sonnet-5-5",
            &ThinkingSettings::default(),
            Some(2048),
            false,
        );
        assert_eq!(p.max_tokens, 2048);
    }

    #[test]
    fn effort_is_clamped_to_the_model_level_set() {
        let high = settings(Some("max"), None, false);
        assert_eq!(
            plan("claude-opus-4-6", &high, None, false).effort,
            Some("max")
        );
        let xhigh = settings(Some("xhigh"), None, false);
        assert_eq!(
            plan("claude-opus-4-6", &xhigh, None, false).effort,
            Some("high")
        );
        assert_eq!(
            plan("claude-sonnet-4-6", &xhigh, None, false).effort,
            Some("high")
        );
        assert_eq!(
            plan("claude-opus-4-8", &xhigh, None, false).effort,
            Some("xhigh")
        );
        assert_eq!(
            plan("claude-haiku-5-5", &xhigh, None, false).effort,
            Some("xhigh")
        );
        assert_eq!(
            plan(
                "claude-opus-4-5",
                &settings(Some("max"), None, false),
                None,
                false
            )
            .effort,
            Some("high")
        );
        assert_eq!(
            plan(
                "claude-sonnet-5-5",
                &settings(Some("minimal"), None, false),
                None,
                false
            )
            .effort,
            Some("low")
        );
        // Garbage effort is dropped, not forwarded.
        assert_eq!(
            plan(
                "claude-opus-5",
                &settings(Some("turbo"), None, false),
                None,
                false
            )
            .effort,
            None
        );
    }

    #[test]
    fn manual_models_get_enabled_with_budget_below_max_tokens() {
        let p = plan(
            "claude-sonnet-4-5-20250929",
            &ThinkingSettings::default(),
            None,
            false,
        );
        assert_eq!(
            p.thinking,
            Some(json!({"type": "enabled", "budget_tokens": 8192}))
        );
        assert_eq!(p.max_tokens, 8192 + 4096);
        assert!(
            p.thinking.as_ref().unwrap()["budget_tokens"]
                .as_u64()
                .unwrap()
                < p.max_tokens as u64
        );
        assert_eq!(p.effort, None);

        // Below the floor is raised to 1024; above the ceiling is capped.
        let p = plan(
            "claude-haiku-4-5",
            &settings(None, Some(10), false),
            None,
            false,
        );
        assert_eq!(p.thinking.unwrap()["budget_tokens"], 1024);
        let p = plan(
            "claude-haiku-4-5",
            &settings(None, Some(500_000), false),
            None,
            false,
        );
        assert_eq!(p.thinking.unwrap()["budget_tokens"], 32_000);
    }

    #[test]
    fn manual_budget_is_clamped_under_an_explicit_caller_ceiling() {
        let p = plan(
            "claude-opus-4-5-20251101",
            &settings(None, Some(8000), false),
            Some(4096),
            false,
        );
        assert_eq!(p.max_tokens, 4096);
        let budget = p.thinking.unwrap()["budget_tokens"].as_u64().unwrap();
        assert!((1024..4096).contains(&budget), "{budget}");
        // No room for a 1024 budget plus an answer: thinking is skipped.
        let p = plan(
            "claude-opus-4-5-20251101",
            &settings(None, Some(8000), false),
            Some(1500),
            false,
        );
        assert_eq!(p.thinking, None);
        assert_eq!(p.max_tokens, 1500);
    }

    #[test]
    fn manual_thinking_is_skipped_when_tool_loop_lacks_a_thinking_block() {
        let p = plan(
            "claude-sonnet-4-5",
            &ThinkingSettings::default(),
            None,
            true,
        );
        assert_eq!(p.thinking, None);
        assert_eq!(p.max_tokens, 4096);
        // Adaptive has no such requirement.
        let p = plan(
            "claude-sonnet-4-6",
            &ThinkingSettings::default(),
            None,
            true,
        );
        assert!(p.thinking.is_some());
    }

    #[test]
    fn models_without_thinking_get_nothing() {
        for model in [
            "claude-3-5-sonnet-20241022",
            "claude-3-haiku-20240307",
            "gpt-5",
            "model",
        ] {
            let p = plan(
                model,
                &settings(Some("high"), Some(4096), false),
                None,
                false,
            );
            assert_eq!(p.thinking, None, "{model}");
            assert_eq!(p.effort, None, "{model}");
            assert_eq!(p.max_tokens, 4096, "{model}");
        }
    }

    #[test]
    fn disabling_follows_each_models_accepted_off_switch() {
        let off = settings(Some("xhigh"), None, true);
        // Opus 5 / Haiku 5.5 accept "disabled" only at effort high or below.
        let p = plan("claude-opus-5", &off, None, false);
        assert_eq!(p.thinking, Some(json!({"type": "disabled"})));
        assert_eq!(p.effort, Some("high"));
        let p = plan("claude-haiku-5-5", &off, None, false);
        assert_eq!(p.thinking, Some(json!({"type": "disabled"})));
        assert_eq!(p.effort, Some("high"));
        // Sonnet 5.5 rejects "disabled"; it takes between_tools, no other field.
        let p = plan("claude-sonnet-5-5", &off, None, false);
        assert_eq!(p.thinking, Some(json!({"type": "between_tools"})));
        assert_eq!(p.effort, Some("high"));
        // Always-on and default-off models: omit.
        assert_eq!(plan("claude-opus-5-5", &off, None, false).thinking, None);
        assert_eq!(plan("claude-fable-5-1", &off, None, false).thinking, None);
        assert_eq!(plan("claude-opus-4-8", &off, None, false).thinking, None);
        // Manual models: omit.
        assert_eq!(plan("claude-haiku-4-5", &off, None, false).thinking, None);
        // The OpenAI-path spelling `effort = none` also turns thinking off.
        let none = settings(Some("none"), None, false);
        assert_eq!(
            plan("claude-opus-5", &none, None, false).thinking,
            Some(json!({"type": "disabled"}))
        );
        assert_eq!(plan("claude-opus-5", &none, None, false).effort, None);
    }
}
