//! The advisory judge interface. An implementation may call a model, but this
//! crate does not. Its scores are recorded on the verdict and cannot change
//! the disposition, so a judge cannot launder a deterministic failure.

use serde::{Deserialize, Serialize};

use crate::Subject;

/// A taste or intent score in `0.0..=1.0` for one named dimension.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JudgeScore {
    pub dimension: String,
    pub score: f32,
    pub rationale: String,
}

/// The advisory record of one judge run on the final candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JudgeRecord {
    pub judge_id: String,
    /// Always true. Kept in the record so readers can see the judge is not a gate.
    pub advisory: bool,
    pub scores: Vec<JudgeScore>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub trait Judge {
    fn id(&self) -> &str;
    /// Returns advisory scores, or a reason the judge could not score.
    fn score(&self, subject: &Subject) -> Result<Vec<JudgeScore>, String>;
}

/// A judge that never runs. Used when no judge is configured.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoJudge;

impl Judge for NoJudge {
    fn id(&self) -> &str {
        "none"
    }

    fn score(&self, _subject: &Subject) -> Result<Vec<JudgeScore>, String> {
        Ok(Vec::new())
    }
}

/// Runs a judge and validates its output into a record. Out-of-range scores
/// are dropped and reported, never clamped into a verdict.
pub(crate) fn run_judge(judge: &dyn Judge, subject: &Subject) -> JudgeRecord {
    match judge.score(subject) {
        Ok(scores) => {
            let bad = scores
                .iter()
                .find(|score| !score.score.is_finite() || !(0.0..=1.0).contains(&score.score));
            match bad {
                Some(score) => JudgeRecord {
                    judge_id: judge.id().to_string(),
                    advisory: true,
                    scores: Vec::new(),
                    error: Some(format!(
                        "score for `{}` is outside 0.0..=1.0; all scores dropped",
                        score.dimension
                    )),
                },
                None => JudgeRecord {
                    judge_id: judge.id().to_string(),
                    advisory: true,
                    scores,
                    error: None,
                },
            }
        }
        Err(reason) => JudgeRecord {
            judge_id: judge.id().to_string(),
            advisory: true,
            scores: Vec::new(),
            error: Some(reason),
        },
    }
}
