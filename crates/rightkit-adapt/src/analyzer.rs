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

/// Test double for AnalyzerSource.
#[cfg(test)]
pub struct RecordingAnalyzerSource {
    sessions: Vec<SessionSummary>,
}

#[cfg(test)]
impl RecordingAnalyzerSource {
    /// Create a source with the given sessions.
    pub fn new(sessions: Vec<SessionSummary>) -> Self {
        Self { sessions }
    }
}

#[cfg(test)]
impl AnalyzerSource for RecordingAnalyzerSource {
    fn recent_sessions(&self) -> Vec<SessionSummary> {
        self.sessions.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str) -> SessionSummary {
        SessionSummary {
            session_id: id.to_string(),
            succeeded: true,
            tool_calls: 0,
            errors: vec![],
            notable_decisions: vec![],
        }
    }

    #[test]
    fn analyze_empty_source() {
        let source = RecordingAnalyzerSource::new(vec![]);
        let analyzer = DailyAnalyzer::new();
        let report = analyzer.analyze(&source);
        assert_eq!(report.review_candidates.len(), 0);
        assert_eq!(report.total_errors, 0);
        assert_eq!(report.failed_sessions, 0);
    }

    #[test]
    fn analyze_successful_session_no_errors() {
        let mut s = session("s1");
        s.succeeded = true;
        s.errors = vec![];
        let source = RecordingAnalyzerSource::new(vec![s]);
        let analyzer = DailyAnalyzer::new();
        let report = analyzer.analyze(&source);
        assert_eq!(report.review_candidates.len(), 0);
        assert_eq!(report.total_errors, 0);
        assert_eq!(report.failed_sessions, 0);
    }

    #[test]
    fn analyze_detects_failed_session() {
        let mut s = session("s1");
        s.succeeded = false;
        let source = RecordingAnalyzerSource::new(vec![s]);
        let analyzer = DailyAnalyzer::new();
        let report = analyzer.analyze(&source);
        assert_eq!(report.failed_sessions, 1);
        assert_eq!(report.review_candidates.len(), 1);
        assert_eq!(report.review_candidates[0].session_id, "s1");
    }

    #[test]
    fn analyze_detects_multiple_errors() {
        let mut s = session("s1");
        s.succeeded = true;
        s.errors = vec![
            "nil deref".to_string(),
            "panic".to_string(),
            "timeout".to_string(),
        ];
        let source = RecordingAnalyzerSource::new(vec![s]);
        let analyzer = DailyAnalyzer::new();
        let report = analyzer.analyze(&source);
        assert_eq!(report.total_errors, 3);
        assert_eq!(report.review_candidates.len(), 1);
        assert_eq!(
            report.review_candidates[0].reason,
            "Potential pain point detected"
        );
    }

    #[test]
    fn analyze_detects_frustration_signals() {
        let mut s = session("s1");
        s.succeeded = true;
        s.errors = vec!["still broken".to_string()];
        let source = RecordingAnalyzerSource::new(vec![s]);
        let analyzer = DailyAnalyzer::new();
        let report = analyzer.analyze(&source);
        assert_eq!(report.review_candidates.len(), 1);
        assert!(report.review_candidates[0].reason.contains("frustration"));
    }

    #[test]
    fn analyze_detects_repeated_failures() {
        let mut s1 = session("s1");
        s1.errors = vec!["boom".to_string()];
        let mut s2 = session("s2");
        s2.errors = vec!["boom".to_string(), "bang".to_string()];
        let mut s3 = session("s3");
        s3.errors = vec!["boom".to_string(), "bang".to_string()];
        let source = RecordingAnalyzerSource::new(vec![s1, s2, s3]);
        let analyzer = DailyAnalyzer::new();
        let report = analyzer.analyze(&source);
        assert_eq!(report.repeated_failures.len(), 2); // "boom" (3x) and "bang" (2x)
        assert_eq!(report.repeated_failures[0].0, "boom");
        assert_eq!(report.repeated_failures[0].1, 3);
    }

    #[test]
    fn analyze_only_reports_2plus_repeats() {
        let mut s1 = session("s1");
        s1.errors = vec!["boom".to_string()];
        let mut s2 = session("s2");
        s2.errors = vec!["bang".to_string()];
        let source = RecordingAnalyzerSource::new(vec![s1, s2]);
        let analyzer = DailyAnalyzer::new();
        let report = analyzer.analyze(&source);
        // No errors appear 2+ times, so no repeated failures.
        assert_eq!(report.repeated_failures.len(), 0);
    }

    #[test]
    fn analyze_trajectory_excerpt_preserved() {
        let mut s = session("s1");
        s.succeeded = false;
        s.tool_calls = 15;
        s.errors = vec!["err".to_string()];
        s.notable_decisions = vec!["decision1".to_string(), "decision2".to_string()];
        let source = RecordingAnalyzerSource::new(vec![s]);
        let analyzer = DailyAnalyzer::new();
        let report = analyzer.analyze(&source);
        let candidate = &report.review_candidates[0];
        assert!(candidate.trajectory_excerpt.contains("tool_calls=15"));
        assert!(candidate.trajectory_excerpt.contains("errors=1"));
        assert!(candidate.trajectory_excerpt.contains("decisions=2"));
    }

    #[test]
    fn analyze_suggests_proposal_kind() {
        let mut s1 = session("s1");
        s1.succeeded = false;
        s1.errors = vec!["e1".to_string(), "e2".to_string()];
        let source = RecordingAnalyzerSource::new(vec![s1]);
        let analyzer = DailyAnalyzer::new();
        let report = analyzer.analyze(&source);
        assert_eq!(
            report.review_candidates[0].suggested_proposal_kind,
            "HarnessRule"
        );
    }

    #[test]
    fn analyze_multiple_sessions() {
        let mut s1 = session("s1");
        s1.succeeded = true;
        s1.errors = vec![];

        let mut s2 = session("s2");
        s2.succeeded = false;

        let mut s3 = session("s3");
        s3.errors = vec!["crash".to_string()];

        let source = RecordingAnalyzerSource::new(vec![s1, s2, s3]);
        let analyzer = DailyAnalyzer::new();
        let report = analyzer.analyze(&source);
        assert_eq!(report.total_errors, 1);
        assert_eq!(report.failed_sessions, 1);
        assert_eq!(report.review_candidates.len(), 2); // s2 (failed) and s3 (frustration)
    }
}
