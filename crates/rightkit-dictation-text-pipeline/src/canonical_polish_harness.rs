use crate::l3_cleanup::CleanupOutcome;
use rightkit_text_polish::text_pipeline::{AiTransformIntent, ControlIntent};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EvalInputRow {
    pub id: String,
    pub duration_s: f64,
    pub hypothesis: String,
    #[serde(default)]
    pub raw_hypothesis: Option<String>,
    #[serde(default)]
    pub ai_transform: Option<String>,
}

impl EvalInputRow {
    pub fn new(id: &str, duration_s: f64, hypothesis: &str) -> Self {
        Self {
            id: id.to_string(),
            duration_s,
            hypothesis: hypothesis.to_string(),
            raw_hypothesis: None,
            ai_transform: None,
        }
    }
}

pub fn pending_long_rows<'a>(
    rows: &'a [EvalInputRow],
    min_duration_s: f64,
    completed: &HashSet<String>,
) -> Vec<&'a EvalInputRow> {
    rows.iter()
        .filter(|row| row.duration_s > min_duration_s && !completed.contains(&row.id))
        .collect()
}

pub struct PreparedProductInput {
    pub text: String,
    pub control_intent: Option<&'static str>,
    pub ai_transform: Option<String>,
    pub cancelled: bool,
}

pub fn prepare_product_input(raw: &str) -> PreparedProductInput {
    let mut text = raw.trim().to_string();
    let mut control_intent = None;
    let mut cancelled = false;
    if let Some(command) = rightkit_text_polish::text_pipeline::parse_control_command(&text) {
        control_intent = Some(match command.intent {
            ControlIntent::Stop => "stop",
            ControlIntent::Send => "send",
            ControlIntent::Cancel => {
                cancelled = true;
                "cancel"
            }
        });
        text = command.clean_text;
    }
    let ai_transform =
        rightkit_text_polish::text_pipeline::parse_ai_transform_command(&text).map(|command| {
            text = command.clean_text;
            match command.intent {
                AiTransformIntent::Prompt => "prompt".to_string(),
                AiTransformIntent::Summarize => "summarize".to_string(),
            }
        });
    PreparedProductInput {
        text,
        control_intent,
        ai_transform,
        cancelled,
    }
}

pub fn select_rows<'a>(
    rows: &'a [EvalInputRow],
    min_duration_s: f64,
    completed: &HashSet<String>,
    wanted: Option<&HashSet<String>>,
    limit: Option<usize>,
) -> Vec<&'a EvalInputRow> {
    pending_long_rows(rows, min_duration_s, completed)
        .into_iter()
        .filter(|row| {
            row.ai_transform.is_none()
                && row.hypothesis.split_whitespace().count() >= 8
                && rightkit_voice_commands::command_recognition::recognize_command(&row.hypothesis)
                    .is_none()
                && rightkit_voice_commands::command_recognition::app_launch_query(&row.hypothesis)
                    .is_none()
        })
        .filter(|row| wanted.is_none_or(|ids| ids.contains(&row.id)))
        .take(limit.unwrap_or(usize::MAX))
        .collect()
}

pub struct L1Resolution {
    pub hypothesis: String,
    pub status: &'static str,
    pub reason: Option<&'static str>,
    pub circuit_open: bool,
}

pub fn resolve_cleanup_outcome(outcome: CleanupOutcome, local: &str) -> L1Resolution {
    match outcome {
        CleanupOutcome::Cleaned(text) => L1Resolution {
            hypothesis: text,
            status: "cleaned",
            reason: None,
            circuit_open: false,
        },
        CleanupOutcome::Skipped {
            reason,
            circuit_open,
        } => L1Resolution {
            hypothesis: local.to_string(),
            status: "skipped_local_fallback",
            reason: Some(reason),
            circuit_open,
        },
        CleanupOutcome::Failed {
            error_class,
            circuit_open,
        } => L1Resolution {
            hypothesis: local.to_string(),
            status: "failed_local_fallback",
            reason: Some(error_class),
            circuit_open,
        },
    }
}
