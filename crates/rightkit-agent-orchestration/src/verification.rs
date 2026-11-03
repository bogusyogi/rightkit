//! Parses an agent's free-text verification output into a structured verdict.

/// A single issue raised during verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationIssue {
    pub severity: String,
    pub message: String,
}

/// The outcome of parsing verification output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationResult {
    pub passed: bool,
    pub issues: Vec<VerificationIssue>,
}

/// Deterministically parses verification text into a [`VerificationResult`].
#[derive(Debug, Default)]
pub struct VerificationParser;

impl VerificationParser {
    pub fn new() -> Self {
        Self
    }

    /// Parse line-oriented verification output.
    ///
    /// - Lines starting with `PASS` flag an explicit pass.
    /// - Lines starting with `FAIL` or `ERROR:` become error issues.
    /// - Lines starting with `WARN:` become warning issues.
    /// - `passed` is true only when there are no error issues AND
    ///   (at least one explicit PASS was seen OR there were zero issues).
    pub fn parse(&self, text: &str) -> VerificationResult {
        let mut issues = Vec::new();
        let mut saw_pass = false;

        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with("PASS") {
                saw_pass = true;
            } else if line.starts_with("FAIL") {
                issues.push(VerificationIssue {
                    severity: "error".into(),
                    message: strip_prefix(line, "FAIL"),
                });
            } else if line.starts_with("ERROR:") {
                issues.push(VerificationIssue {
                    severity: "error".into(),
                    message: strip_prefix(line, "ERROR:"),
                });
            } else if line.starts_with("WARN:") {
                issues.push(VerificationIssue {
                    severity: "warning".into(),
                    message: strip_prefix(line, "WARN:"),
                });
            }
        }

        let has_error = issues.iter().any(|i| i.severity == "error");
        let passed = !has_error && (saw_pass || issues.is_empty());

        VerificationResult { passed, issues }
    }
}

fn strip_prefix(line: &str, prefix: &str) -> String {
    line[prefix.len()..]
        .trim_start_matches(':')
        .trim()
        .to_string()
}
