//! Complexity dispatch routing — decides whether a user task should run as the
//! direct single-agent loop or be handed to the conductor for decomposition.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

const CLASSIFIER_THRESHOLD: f32 = 0.75;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionStrategy {
    Direct,
    Conduct { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JudgeSource {
    Heuristic,
    Classifier,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DispatchDecision {
    pub strategy: ExecutionStrategy,
    pub confidence: f32,
    pub rationale: String,
    pub judged_by: JudgeSource,
}

impl DispatchDecision {
    pub fn direct(confidence: f32, rationale: impl Into<String>, judged_by: JudgeSource) -> Self {
        Self {
            strategy: ExecutionStrategy::Direct,
            confidence,
            rationale: rationale.into(),
            judged_by,
        }
    }

    pub fn conduct(confidence: f32, rationale: impl Into<String>, judged_by: JudgeSource) -> Self {
        let rationale = rationale.into();
        Self {
            strategy: ExecutionStrategy::Conduct {
                reason: rationale.clone(),
            },
            confidence,
            rationale,
            judged_by,
        }
    }

    pub fn strategy_label(&self) -> String {
        match self.strategy {
            ExecutionStrategy::Direct => "direct".to_string(),
            ExecutionStrategy::Conduct { .. } => "multi-agent".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassifierComplexity {
    Simple,
    Complex,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifierOutcome {
    pub complexity: ClassifierComplexity,
    pub reason: String,
}

#[async_trait]
pub trait DispatchClassifier: Send + Sync {
    async fn classify(&self, task: &str) -> Result<ClassifierOutcome, String>;
}

#[derive(Debug, Clone)]
pub struct DispatchRouter {
    classifier_threshold: f32,
}

impl Default for DispatchRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl DispatchRouter {
    pub fn new() -> Self {
        Self {
            classifier_threshold: CLASSIFIER_THRESHOLD,
        }
    }

    pub async fn decide(
        &self,
        task: &str,
        classifier: Option<&dyn DispatchClassifier>,
    ) -> DispatchDecision {
        let heuristic = self.decide_heuristically(task);
        if heuristic.confidence >= self.classifier_threshold {
            return heuristic;
        }

        let Some(classifier) = classifier else {
            return heuristic;
        };

        match classifier.classify(task).await {
            Ok(outcome) => match outcome.complexity {
                ClassifierComplexity::Simple => DispatchDecision::direct(
                    0.80,
                    clean_reason(
                        outcome.reason,
                        "Classifier judged this as a simple direct task.",
                    ),
                    JudgeSource::Classifier,
                ),
                ClassifierComplexity::Complex => DispatchDecision::conduct(
                    0.80,
                    clean_reason(
                        outcome.reason,
                        "Classifier judged this as complex enough to decompose.",
                    ),
                    JudgeSource::Classifier,
                ),
            },
            Err(err) => {
                let rationale = format!(
                    "{} Classifier unavailable: {err}; using the heuristic fallback.",
                    heuristic.rationale
                );
                DispatchDecision {
                    rationale,
                    ..heuristic
                }
            }
        }
    }

    fn decide_heuristically(&self, task: &str) -> DispatchDecision {
        let text = task.trim();
        if text.is_empty() {
            return DispatchDecision::direct(
                1.0,
                "Empty task text defaults to the direct loop.",
                JudgeSource::Heuristic,
            );
        }

        let lower = text.to_ascii_lowercase();
        let words = word_count(&lower);
        let paths = path_reference_count(text);
        let sentences = sentence_count(text);
        let question = is_question(&lower);
        let broad_scope = contains_any(
            &lower,
            &[
                "across the repo",
                "across the codebase",
                "across the project",
                "repo-wide",
                "entire repo",
                "entire codebase",
                "whole repo",
                "all call sites",
                "all usages",
                "all files",
                "everything",
                "every command",
                "every handler",
            ],
        );
        let explicit_decomposition = contains_any(
            &lower,
            &[
                "end-to-end",
                "decompose",
                "multi-agent",
                "conductor",
                "task graph",
                "audit and fix",
                "migrate",
                "migration",
            ],
        );
        let multi_step = contains_any(
            &lower,
            &[
                " and then ",
                " then ",
                ";",
                " and add ",
                " and add tests",
                " and update ",
                " and fix ",
                " and verify ",
            ],
        ) || sentences >= 2;

        if question && words <= 28 && !broad_scope {
            return DispatchDecision::direct(
                0.90,
                "Short explanatory question; a single direct agent turn is enough.",
                JudgeSource::Heuristic,
            );
        }

        if paths == 1 && !broad_scope && !explicit_decomposition && words <= 20 {
            return DispatchDecision::direct(
                0.86,
                "Single-file scoped request; direct execution avoids unnecessary decomposition.",
                JudgeSource::Heuristic,
            );
        }

        if contains_any(&lower, &["fix this", "typo", "rename ", "change "])
            && words <= 16
            && !broad_scope
        {
            return DispatchDecision::direct(
                0.84,
                "Small local edit signal; direct execution is the shortest path.",
                JudgeSource::Heuristic,
            );
        }

        let mut conduct_score = 0;
        if broad_scope {
            conduct_score += 3;
        }
        if explicit_decomposition {
            conduct_score += 2;
        }
        if lower.contains("refactor") && (broad_scope || multi_step) {
            conduct_score += 2;
        }
        if (lower.contains("build") || lower.contains("implement"))
            && (multi_step || lower.contains("feature"))
        {
            conduct_score += 1;
        }
        if multi_step {
            conduct_score += 1;
        }
        if paths >= 3 {
            conduct_score += 2;
        }
        if words >= 24 {
            conduct_score += 1;
        }

        if conduct_score >= 3 {
            let confidence = (0.78 + (conduct_score as f32 * 0.04)).min(0.96);
            return DispatchDecision::conduct(
                confidence,
                "Broad or multi-step scope detected; conductor decomposition should coordinate the work.",
                JudgeSource::Heuristic,
            );
        }

        if words <= 8 && !multi_step {
            return DispatchDecision::direct(
                0.80,
                "Short single-ask task; direct execution is appropriate.",
                JudgeSource::Heuristic,
            );
        }

        DispatchDecision::direct(
            0.55,
            "Mixed complexity signals; classifier can decide whether decomposition is worth it.",
            JudgeSource::Heuristic,
        )
    }
}

fn clean_reason(reason: String, fallback: &'static str) -> String {
    let trimmed = reason.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

fn sentence_count(text: &str) -> usize {
    text.matches('.').count() + text.matches('?').count() + text.matches('!').count()
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn is_question(text: &str) -> bool {
    text.contains('?')
        || text.starts_with("what ")
        || text.starts_with("why ")
        || text.starts_with("how ")
        || text.starts_with("where ")
        || text.starts_with("when ")
        || text.starts_with("who ")
        || text.starts_with("explain ")
}

fn path_reference_count(text: &str) -> usize {
    text.split_whitespace()
        .map(|token| {
            token.trim_matches(|c: char| {
                matches!(c, ',' | ';' | ':' | '"' | '\'' | '(' | ')' | '[' | ']')
            })
        })
        .filter(|token| {
            token.contains('/')
                || token.contains('\\')
                || COMMON_EXTENSIONS.iter().any(|ext| token.ends_with(ext))
        })
        .count()
}

const COMMON_EXTENSIONS: [&str; 18] = [
    ".rs", ".ts", ".tsx", ".js", ".jsx", ".py", ".go", ".java", ".cs", ".md", ".toml", ".json",
    ".yaml", ".yml", ".css", ".scss", ".html", ".sql",
];
