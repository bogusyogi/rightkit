//! The one normalized finding schema for PL-007 post-edit verification
//! fan-out.
//!
//! Every verifier (LSP diagnostics, structured-config validation, external
//! typecheck/lint/format/test commands) maps its native output into
//! [`VerifiedFinding`]. The canon row fixes the schema fields:
//! `source, range, severity, category, message, content-hash binding,
//! fixability`; the `path` field is the the host app seam addition (ranges are
//! per-file, and staleness is decided per file, so the file must be typed
//! evidence on the finding itself, not re-parsed out of `message`).

use std::path::PathBuf;

/// Verifier severity, following the LSP severity integers (1=Error .. 4=Hint)
/// because the LSP verifier is the primary producer and every other producer
/// maps into its vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FindingSeverity {
    Hint,
    Information,
    Warning,
    Error,
}

impl FindingSeverity {
    /// Map an LSP severity integer. `1=Error, 2=Warning, 3=Information,
    /// 4=Hint`; anything else has no honest mapping and yields `None` (the
    /// finding is dropped rather than mis-severitied).
    pub fn from_lsp(raw: u8) -> Option<Self> {
        match raw {
            1 => Some(Self::Error),
            2 => Some(Self::Warning),
            3 => Some(Self::Information),
            4 => Some(Self::Hint),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Information => "info",
            Self::Hint => "hint",
        }
    }

    /// Actionable failures are what the originating agent is fed back
    /// pre-completion. Warnings/information/hints are surfaced but never
    /// block, mirroring the post-turn gate's treatment of warnings.
    pub fn is_actionable(&self) -> bool {
        matches!(self, Self::Error)
    }
}

/// What kind of check produced the finding. The canon row enumerates the
/// verifier families; this is that list as typed data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FindingCategory {
    /// Language-server diagnostic (layered over the CG-B01 `lsp_tool`).
    Lsp,
    /// Syntax/structure check (JSON/TOML parse).
    Syntax,
    /// External typechecker (e.g. `cargo check`, `tsc`).
    TypeCheck,
    /// External linter (e.g. `clippy`, `eslint`).
    Lint,
    /// Format check (e.g. `cargo fmt --check`).
    Format,
    /// Static/AST analysis.
    StaticAnalysis,
    /// Dependency-manifest validation (e.g. lockfile/manifest consistency).
    Dependency,
    /// Config/schema validation.
    Config,
    /// Cheap targeted test run.
    Test,
}

impl FindingCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Lsp => "lsp",
            Self::Syntax => "syntax",
            Self::TypeCheck => "typecheck",
            Self::Lint => "lint",
            Self::Format => "format",
            Self::StaticAnalysis => "static-analysis",
            Self::Dependency => "dependency",
            Self::Config => "config",
            Self::Test => "test",
        }
    }
}

/// Fixability of a finding: does the producer say a mechanical fix exists?
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindingFixability {
    /// No mechanical fix is advertised; the agent must reason about the fix.
    Manual,
    /// A mechanical fix exists; `hint` says what it is (the fix command, or
    /// the repair the agent should perform).
    FixAvailable { hint: String },
}

/// One file range in LSP wire convention: 0-based line and 0-based character,
/// end-exclusive. Stored verbatim (not humanized) so producers can copy LSP
/// positions without inverse-mapping bugs; renderers add the +1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FindingRange {
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

impl FindingRange {
    pub fn point(start_line: u32, start_column: u32) -> Self {
        Self {
            start_line,
            start_column,
            end_line: start_line,
            end_column: start_column,
        }
    }
}

/// One normalized finding bound to the content it was produced against.
///
/// `content_hash` is the `sha256:<hex>` of the verified file content captured
/// at finding time (see `staleness`). A finding whose hash no longer matches
/// the file's current hash is stale and is discarded before the report
/// reaches the agent — a diagnostic about a version of the file that no
/// longer exists must not be presented as a current error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedFinding {
    /// Which verifier produced this finding (its `VerifierId` string).
    pub source: String,
    /// Range the finding attaches to, when the producer could locate one.
    pub range: Option<FindingRange>,
    pub severity: FindingSeverity,
    pub category: FindingCategory,
    pub message: String,
    /// Content-hash binding at finding time: `sha256:<hex>`, or `None` when
    /// the content could not be hashed (file already gone, or the finding is
    /// workspace-level rather than file-level).
    pub content_hash: Option<String>,
    pub fixability: FindingFixability,
    /// File this finding is about. the host app seam addition (see module doc).
    pub path: PathBuf,
}

impl VerifiedFinding {
    /// Dedupe identity: two findings describing the same problem at the same
    /// location in the same category collapse to one. `source` is
    /// deliberately not part of the key (two verifiers reporting the
    /// identical problem is duplication for the agent, not corroboration);
    /// severity is not part of the key either — dedupe keeps the most
    /// severe occurrence. Message normalization collapses whitespace so a
    /// wrapped message line does not defeat the match.
    pub fn dedupe_key(&self) -> (PathBuf, Option<FindingRange>, FindingCategory, String) {
        (
            self.path.clone(),
            self.range,
            self.category,
            normalize_message(&self.message),
        )
    }
}

pub(crate) fn normalize_message(message: &str) -> String {
    message.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_from_lsp_maps_the_lsp_vocabulary_and_rejects_unknowns() {
        assert_eq!(FindingSeverity::from_lsp(1), Some(FindingSeverity::Error));
        assert_eq!(FindingSeverity::from_lsp(2), Some(FindingSeverity::Warning));
        assert_eq!(
            FindingSeverity::from_lsp(3),
            Some(FindingSeverity::Information)
        );
        assert_eq!(FindingSeverity::from_lsp(4), Some(FindingSeverity::Hint));
        assert_eq!(FindingSeverity::from_lsp(0), None);
        assert_eq!(FindingSeverity::from_lsp(5), None);
    }

    #[test]
    fn only_errors_are_actionable() {
        assert!(FindingSeverity::Error.is_actionable());
        assert!(!FindingSeverity::Warning.is_actionable());
        assert!(!FindingSeverity::Information.is_actionable());
        assert!(!FindingSeverity::Hint.is_actionable());
    }

    #[test]
    fn severity_orders_error_above_warning_for_dedupe_keep_most_severe() {
        assert!(FindingSeverity::Error > FindingSeverity::Warning);
        assert!(FindingSeverity::Warning > FindingSeverity::Information);
        assert!(FindingSeverity::Information > FindingSeverity::Hint);
    }

    #[test]
    fn category_labels_are_stable() {
        assert_eq!(FindingCategory::Lsp.as_str(), "lsp");
        assert_eq!(FindingCategory::Syntax.as_str(), "syntax");
        assert_eq!(FindingCategory::TypeCheck.as_str(), "typecheck");
        assert_eq!(FindingCategory::Lint.as_str(), "lint");
        assert_eq!(FindingCategory::Format.as_str(), "format");
        assert_eq!(FindingCategory::StaticAnalysis.as_str(), "static-analysis");
        assert_eq!(FindingCategory::Dependency.as_str(), "dependency");
        assert_eq!(FindingCategory::Config.as_str(), "config");
        assert_eq!(FindingCategory::Test.as_str(), "test");
    }

    #[test]
    fn dedupe_key_ignores_source_and_severity_and_normalizes_whitespace() {
        let base = VerifiedFinding {
            source: "lsp".into(),
            range: Some(FindingRange::point(2, 4)),
            severity: FindingSeverity::Warning,
            category: FindingCategory::Lsp,
            message: "cannot  find\nvalue x".into(),
            content_hash: Some("sha256:aa".into()),
            fixability: FindingFixability::Manual,
            path: PathBuf::from("a.rs"),
        };
        let mut other = base.clone();
        other.source = "clippy".into();
        other.severity = FindingSeverity::Error;
        other.message = "cannot find value x".into();
        assert_eq!(base.dedupe_key(), other.dedupe_key());
    }
}
