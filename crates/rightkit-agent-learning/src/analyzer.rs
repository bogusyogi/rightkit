//! Daily analyzer: surfaces sessions worth reviewing based on error patterns,
//! failures, frustration signals, and trajectory insights.

use crate::extractor::SessionSummary;
use serde::{Deserialize, Serialize};

/// A source of session data for the analyzer to examine.
pub trait AnalyzerSource {
    /// Return recent sessions for analysis.
    fn recent_sessions(&self) -> Vec<SessionSummary>;
}

/// Metadata about a session worth reviewing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewCandidate {
    /// Session identifier.
    pub session_id: String,
    /// Reason why this session was flagged.
    pub reason: String,
    /// Suggested proposal kind to generate from this session.
    pub suggested_proposal_kind: String,
    /// Excerpt of the session trajectory for context.
    pub trajectory_excerpt: String,
}

/// Daily report of sessions and issues found.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DailyReport {
    /// Sessions flagged for manual review.
    pub review_candidates: Vec<ReviewCandidate>,
    /// Count of tool errors across all sessions.
    pub total_errors: usize,
    /// Count of failed sessions.
    pub failed_sessions: usize,
    /// Repeated failure patterns detected.
    pub repeated_failures: Vec<(String, usize)>, // (error, count)
}

/// Analyzes recent sessions to detect patterns and surface candidates for review.
#[derive(Debug, Default, Clone)]
pub struct DailyAnalyzer;

impl DailyAnalyzer {
    /// Create a new analyzer.
    pub fn new() -> Self {
        Self
    }

    /// Analyze sessions from the provided source.
    pub fn analyze(&self, source: &dyn AnalyzerSource) -> DailyReport {
        let sessions = source.recent_sessions();

        let mut report = DailyReport {
            review_candidates: Vec::new(),
            total_errors: 0,
            failed_sessions: 0,
            repeated_failures: Vec::new(),
        };

        let mut error_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        let frustration_phrases = [
            "not working",
            "still broken",
            "wrong again",
            "failed",
            "error",
            "crash",
        ];

        for session in sessions {
            // Count errors and failures.
            report.total_errors += session.errors.len();
            if !session.succeeded {
                report.failed_sessions += 1;
            }

            // Track error frequencies.
            for error in &session.errors {
                *error_counts.entry(error.clone()).or_insert(0) += 1;
            }

            // Check for frustration signals in session summary or errors.
            let has_frustration = session
                .errors
                .iter()
                .chain(session.notable_decisions.iter())
                .any(|text| {
                    frustration_phrases
                        .iter()
                        .any(|phrase| text.to_lowercase().contains(phrase))
                });

            // Surface review candidates based on failure/error patterns.
            if !session.succeeded || has_frustration || session.errors.len() > 2 {
                let reason = if has_frustration {
                    "User frustration detected in session".to_string()
                } else if !session.succeeded && session.errors.len() > 2 {
                    format!("Failed session with {} errors", session.errors.len())
                } else if !session.succeeded {
                    "Session failed".to_string()
                } else {
                    "Potential pain point detected".to_string()
                };

                let trajectory_excerpt = format!(
                    "tool_calls={}, errors={}, decisions={}",
                    session.tool_calls,
                    session.errors.len(),
                    session.notable_decisions.len()
                );

                let suggested_proposal_kind = if session.errors.len() > 1 {
                    "HarnessRule".to_string()
                } else if !session.succeeded && session.tool_calls > 5 {
                    "Workflow".to_string()
                } else {
                    "Memory".to_string()
                };

                report.review_candidates.push(ReviewCandidate {
                    session_id: session.session_id.clone(),
                    reason,
                    suggested_proposal_kind,
                    trajectory_excerpt,
                });
            }
        }

        // Extract repeated failures (errors that appear 2+ times).
        let mut repeated: Vec<_> = error_counts
            .into_iter()
            .filter(|(_, count)| *count >= 2)
            .collect();
        repeated.sort_by_key(|&(_, count)| std::cmp::Reverse(count)); // Sort by frequency descending.
        report.repeated_failures = repeated;

        report
    }
}
