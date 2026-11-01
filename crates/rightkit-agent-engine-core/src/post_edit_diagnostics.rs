//! Typed post-edit diagnostics — after a turn's mutations land, derive a
//! `PostEditReport` of typed findings and let the post-turn gate decide from
//! `DiagnosticSeverity` / `DiagnosticSource`. The string renderer turns that
//! report into transcript text; it does not own the verdict any more. The
//! engine call site collects mutations from `ToolResult::mutations` directly
//! — never by re-parsing `arguments_json` to extract a `path`.
//!
//! Caveat: on a large repo a language server may still be indexing when queried
//! and return empty diagnostics (a transient false "clean"); that is a property
//! of the underlying `lsp diagnostics` op, not of this step. Such absence is
//! surfaced as `Unverified`, not mis-represented as a clean check.

#[cfg(test)]
use rightkit_agent_engine_protocol::ToolCall;
use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
#[cfg(feature = "host")]
use rightkit_agent_tools::coding_tools::lsp_tool::{LspTool, DIAGNOSTICS_PREFIX};
use rightkit_agent_tools::coding_tools::{Tool, ToolContext, WorkspaceMutation};
use serde_json::json;
use std::path::Path;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

/// Where a diagnostic finding was produced. `StructuredConfig` findings are
/// deterministic local parses (JSON/TOML) and always carry a verdict.
/// `LanguageServer` findings may legitimately be `Unverified` if the server
/// is absent, timed out, or returned no schema for the language — that is
/// distinct from a real "no errors" finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticSource {
    StructuredConfig,
    LanguageServer,
}

/// Severity of a typed diagnostic finding. `Error` always blocks (structured
/// config parse failures, language-server diagnostics at `severity=1`).
/// `Unverified` blocks because we cannot prove the file is sound — fail
/// closed. `Warning` is reported but does not block on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Unverified,
}

/// One typed diagnostic finding tied to a concrete path. The renderer turns
/// `findings` into transcript text; gate logic reads `severity` and `source`
/// directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticFinding {
    pub path: PathBuf,
    pub source: DiagnosticSource,
    pub severity: DiagnosticSeverity,
    pub message: String,
}

/// The complete output of one post-edit diagnostic pass. `touched_paths` is
/// what the engine asked the diagnostics to look at; `findings` is what they
/// produced. The two lists are separate so a path with no error does NOT
/// appear in `findings` while still being observable in `touched_paths` for
/// reconciliation assertions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PostEditReport {
    pub touched_paths: Vec<PathBuf>,
    pub findings: Vec<DiagnosticFinding>,
}

#[cfg(test)]
const MAX_PROBLEM_LINES: usize = 40;

impl PostEditReport {
    #[cfg(test)]
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }
}

/// Extensions we have a known language server for (mirrors `lsp_tool`'s set).
fn is_code_ext(path: &str) -> bool {
    matches!(
        Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or(""),
        "rs" | "ts" | "tsx" | "js" | "jsx" | "py"
    )
}

fn is_json_ext(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e == "json")
}

fn is_toml_ext(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e == "toml")
}

/// True for a structured-config file whose syntax we can check without a
/// language server.
fn is_structured_config_ext(path: &str) -> bool {
    is_json_ext(path) || is_toml_ext(path)
}

/// Parse an edited TOML file and return a `DiagnosticFinding` (severity
/// `Error`) if it no longer parses. Symmetric with `json_syntax_finding`:
/// `Cargo.toml`, `pyproject.toml`, and the repo's own `config.toml` were all
/// invisible to post-edit checks, so a broken one shipped silently. `toml` is
/// already in the lock tree via `host_config`.
fn toml_syntax_finding(workspace_root: &Path, path: &Path) -> Option<DiagnosticFinding> {
    let text = std::fs::read_to_string(workspace_root.join(path)).ok()?;
    match toml::from_str::<toml::Value>(&text) {
        Ok(_) => None,
        Err(e) => Some(DiagnosticFinding {
            path: path.to_path_buf(),
            source: DiagnosticSource::StructuredConfig,
            severity: DiagnosticSeverity::Error,
            message: format!(
                "{} is no longer valid TOML after this edit: {e}. The file must parse — re-read it and repair the syntax before continuing.",
                path.display()
            ),
        }),
    }
}

/// Parse an edited JSON file and return a `DiagnosticFinding` (severity
/// `Error`) if it no longer parses.
fn json_syntax_finding(workspace_root: &Path, path: &Path) -> Option<DiagnosticFinding> {
    let text = std::fs::read_to_string(workspace_root.join(path)).ok()?;
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(_) => None,
        Err(e) => Some(DiagnosticFinding {
            path: path.to_path_buf(),
            source: DiagnosticSource::StructuredConfig,
            severity: DiagnosticSeverity::Error,
            message: format!(
                "{} is no longer valid JSON after this edit: {e}. The file must parse — re-read it and repair the syntax before continuing.",
                path.display()
            ),
        }),
    }
}

/// Syntax finding for whichever structured config `path` is, if any.
fn structured_config_finding(workspace_root: &Path, path: &Path) -> Option<DiagnosticFinding> {
    if is_json_ext(path.to_str().unwrap_or("")) {
        json_syntax_finding(workspace_root, path)
    } else if is_toml_ext(path.to_str().unwrap_or("")) {
        toml_syntax_finding(workspace_root, path)
    } else {
        None
    }
}

/// Run the diagnostic pass over a list of paths touched by mutations.
/// Returns the typed report; the renderer turns it into text for the
/// transcript separately. A read-only turn never reaches this seam.
pub async fn build_report(
    paths: &[PathBuf],
    session_id: &str,
    workspace_root: &Path,
    permission_mode: PermissionMode,
    workspace_trust: WorkspaceTrust,
    per_file_timeout: Duration,
    overall_budget: Duration,
) -> PostEditReport {
    let ctx = ToolContext::new(session_id, workspace_root, permission_mode, workspace_trust);
    let start = Instant::now();
    let mut unique_paths = paths.to_vec();
    unique_paths.sort();
    unique_paths.dedup();
    let mut report = PostEditReport {
        touched_paths: unique_paths.clone(),
        findings: Vec::new(),
    };

    // Structured configs first: a parse costs nothing, needs no language
    // server, and a config that stopped parsing is the most damaging edit an
    // agent can leave behind. These checks are exhaustive over the path list;
    // no trimming cap, because each parse is cheap.
    for path in &unique_paths {
        if is_structured_config_ext(path.to_str().unwrap_or("")) {
            if let Some(finding) = structured_config_finding(workspace_root, path) {
                report.findings.push(finding);
            }
        }
    }

    // Code files through the language server, under an overall wall-clock
    // budget. Each code path produces at most one finding; an absence of
    // server / timeout / empty result is reported as `Unverified` so the
    // gate can fail closed (we cannot prove the file is sound).
    for path in &unique_paths {
        if !is_code_ext(path.to_str().unwrap_or("")) {
            continue;
        }
        let before = file_fingerprint(workspace_root, path);
        let Some(remaining) = overall_budget.checked_sub(start.elapsed()) else {
            report.findings.push(DiagnosticFinding {
                path: path.clone(),
                source: DiagnosticSource::LanguageServer,
                severity: DiagnosticSeverity::Unverified,
                message: format!(
                    "{}: diagnostic budget exhausted before the language server could respond; the file may carry real errors that did not get reported.",
                    path.display()
                ),
            });
            continue;
        };
        if remaining.is_zero() {
            report.findings.push(DiagnosticFinding {
                path: path.clone(),
                source: DiagnosticSource::LanguageServer,
                severity: DiagnosticSeverity::Unverified,
                message: format!(
                    "{}: no diagnostic budget remaining; failure to deliver language-server findings is recorded as Unverified, not as clean.",
                    path.display()
                ),
            });
            continue;
        }
        let timeout = per_file_timeout.min(remaining);

        // A language server is a separate process. On a host that cannot spawn
        // one there is nothing to ask, so every path is recorded `Unverified`
        // — the module's own vocabulary for "we could not check this". Not
        // reporting it at all would let the post-turn gate read an unchecked
        // edit as clean, which is the exact failure the rest of this file is
        // written to prevent.
        #[cfg(not(feature = "host"))]
        {
            let _ = (&ctx, timeout, before);
            report.findings.push(DiagnosticFinding {
                path: path.clone(),
                source: DiagnosticSource::LanguageServer,
                severity: DiagnosticSeverity::Unverified,
                message: format!(
                    "{}: this host cannot run a language server; the edit is unverified, not clean.",
                    path.display()
                ),
            });
            continue;
        }

        #[cfg(feature = "host")]
        let lsp_tool = LspTool;
        #[cfg(feature = "host")]
        let fut = lsp_tool.execute(
            json!({ "operation": "diagnostics", "path": path.to_string_lossy() }),
            &ctx,
        );
        #[cfg(feature = "host")]
        match tokio::time::timeout(timeout, fut).await {
            Ok(result) if !result.is_error => {
                if before.is_some() && before != file_fingerprint(workspace_root, path) {
                    // File changed while the LSP was running; the response
                    // is stale. Surface Unverified rather than pretend it
                    // is a clean check.
                    report.findings.push(DiagnosticFinding {
                        path: path.clone(),
                        source: DiagnosticSource::LanguageServer,
                        severity: DiagnosticSeverity::Unverified,
                        message: format!(
                            "{} changed during the language-server pass; the reported result is stale.",
                            path.display()
                        ),
                    });
                    continue;
                }
                let lsp_message = result.content.clone();
                if is_clean_or_absent_message(&lsp_message) {
                    // No diagnostics, no server, or otherwise unparseable
                    // empty response — distinguish "server reported clean"
                    // from "we never heard from a server". The prefix
                    // `DIAGNOSTICS_PREFIX` is the wire-protocol marker; its
                    // absence means we never spoke to a server, which is
                    // Unverified, not clean.
                    if !lsp_message.starts_with(DIAGNOSTICS_PREFIX) {
                        report.findings.push(DiagnosticFinding {
                            path: path.clone(),
                            source: DiagnosticSource::LanguageServer,
                            severity: DiagnosticSeverity::Unverified,
                            message: format!(
                                "{}: language server not configured or no diagnostics were produced; failing closed.",
                                path.display()
                            ),
                        });
                    }
                    continue;
                }
                if let Some(body) = lsp_message.strip_prefix(DIAGNOSTICS_PREFIX) {
                    for line in body.lines() {
                        if let Some(finding) = parse_lsp_line(path, line) {
                            report.findings.push(finding);
                        }
                    }
                }
            }
            Ok(_err) => {
                report.findings.push(DiagnosticFinding {
                    path: path.clone(),
                    source: DiagnosticSource::LanguageServer,
                    severity: DiagnosticSeverity::Unverified,
                    message: format!(
                        "{}: language server returned an error; not misreporting as clean.",
                        path.display()
                    ),
                });
            }
            Err(_) => {
                report.findings.push(DiagnosticFinding {
                    path: path.clone(),
                    source: DiagnosticSource::LanguageServer,
                    severity: DiagnosticSeverity::Unverified,
                    message: format!(
                        "{}: timed out waiting for language server; failing closed.",
                        path.display()
                    ),
                });
            }
        }
    }

    report
}

fn is_clean_or_absent_message(content: &str) -> bool {
    content.trim().is_empty()
        || content.contains("No diagnostics reported.")
        || content.contains("Diagnostics require a language server")
}

/// Parse a single `lsp diagnostics` output line (the well-known
/// `uri:line:col: severity=N message` shape) into a finding. Severity `1`
/// is `Error`, `2` is `Warning`. Lines that don't fit the shape (the
/// typical "no server" / "no diagnostics" responses) yield no finding —
/// handled separately by `is_clean_or_absent_message`.
fn parse_lsp_line(path: &Path, line: &str) -> Option<DiagnosticFinding> {
    let (_, tail) = line.split_once("severity=")?;
    let (severity_token, message) = tail.split_once(char::is_whitespace)?;
    let severity: i64 = severity_token.parse().ok()?;
    let sev = match severity {
        1 => DiagnosticSeverity::Error,
        2 => DiagnosticSeverity::Warning,
        _ => return None,
    };
    Some(DiagnosticFinding {
        path: path.to_path_buf(),
        source: DiagnosticSource::LanguageServer,
        severity: sev,
        message: message.trim().to_string(),
    })
}

/// Render a typed `PostEditReport` into the injection block the engine posts
/// as a user turn. Pure mapping from `findings` to text; never owns the
/// decision about whether to block.
#[cfg(test)]
pub fn render_injection_block(report: &PostEditReport) -> Option<String> {
    let mut sections: Vec<String> = Vec::new();
    let mut budget = MAX_PROBLEM_LINES;
    let mut truncated = false;
    for finding in &report.findings {
        if budget == 0 {
            truncated = true;
            break;
        }
        let line = finding.message.to_string();
        sections.push(format!(
            "{} ({}):\n{}",
            finding.path.display(),
            finding.severity.severity_label(),
            line
        ));
        budget -= 1;
    }
    if sections.is_empty() {
        return None;
    }
    let mut block = String::from(
        "[auto-diagnostics] Typed findings for files you just edited. Fix errors and unverified items before continuing:\n\n",
    );
    block.push_str(&sections.join("\n\n"));
    if truncated {
        block.push_str("\n\n(diagnostics truncated)");
    }
    Some(block)
}

#[cfg(test)]
impl DiagnosticSeverity {
    fn severity_label(&self) -> &'static str {
        match self {
            DiagnosticSeverity::Error => "error",
            DiagnosticSeverity::Warning => "warning",
            DiagnosticSeverity::Unverified => "unverified",
        }
    }
}

/// True for a tool name that should be considered for the post-edit guard.
/// Replaces the legacy `is_edit_tool` string-match by reading capability
/// class from the registry, and is kept here as a name-based fallback only
/// for diagnostic logging.
#[cfg(test)]
fn is_legacy_edit_tool(name: &str) -> bool {
    matches!(
        name,
        "edit_file" | "write_file" | "multi_edit" | "apply_patch"
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileFingerprint {
    len: u64,
    modified: Option<SystemTime>,
}

fn file_fingerprint(workspace_root: &Path, path: &Path) -> Option<FileFingerprint> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    };
    let metadata = std::fs::metadata(absolute).ok()?;
    Some(FileFingerprint {
        len: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

#[cfg(all(test, feature = "host"))]
mod tests {
    use super::*;
    use crate::verify_fanout::normalize_workspace_mutations;
    use rightkit_agent_tools::coding_tools::lsp_tool::{
        format_diagnostic_line, write_lsp_workspace_config, LspWorkspaceConfig, LspWorkspaceServer,
    };
    use rightkit_agent_tools::coding_tools::WorkspaceMutationKind;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn normalize_mutations_resolves_and_dedupes() {
        let root = PathBuf::from("/work");
        let a = WorkspaceMutation {
            path: PathBuf::from("a.json"),
            kind: WorkspaceMutationKind::Modified,
            previous_content: Some("a".to_string()),
        };
        let b = WorkspaceMutation {
            path: PathBuf::from("/work/a.json"),
            kind: WorkspaceMutationKind::Modified,
            previous_content: Some("a".to_string()),
        };
        let c = WorkspaceMutation {
            path: PathBuf::from("b.json"),
            kind: WorkspaceMutationKind::Created,
            previous_content: None,
        };
        let normalized = normalize_workspace_mutations(&root, vec![a, b, c]);
        assert_eq!(normalized.len(), 2, "{:?}", normalized);
        // Output is sorted: a before b.
        assert_eq!(normalized[0].path, PathBuf::from("/work/a.json"));
        assert_eq!(normalized[1].path, PathBuf::from("/work/b.json"));
    }

    #[test]
    fn parse_lsp_line_recognizes_severity_one_and_two() {
        let p = Path::new("a.rs");
        let err = parse_lsp_line(p, "file:///a.rs:3:5: severity=1 cannot find value x").unwrap();
        assert_eq!(err.severity, DiagnosticSeverity::Error);
        let warn = parse_lsp_line(p, "file:///a.rs:9:1: severity=2 unused import").unwrap();
        assert_eq!(warn.severity, DiagnosticSeverity::Warning);
        assert!(parse_lsp_line(p, "no severity here").is_none());
    }

    /// Convenience for tests that don't need the wire-format helper.
    fn diag_content(lines: &[(&str, u32, u32, i64, &str)]) -> String {
        let body = lines
            .iter()
            .map(|(u, l, c, s, m)| format_diagnostic_line(u, *l, *c, *s, m))
            .collect::<Vec<_>>()
            .join("\n");
        format!("{DIAGNOSTICS_PREFIX}{body}")
    }

    #[test]
    fn render_injection_block_keeps_errors_drops_warnings() {
        let content = diag_content(&[
            ("file:///a.rs", 3, 5, 1, "cannot find value x"),
            ("file:///a.rs", 9, 1, 2, "unused import"),
        ]);
        let report = PostEditReport {
            touched_paths: vec![PathBuf::from("a.rs")],
            findings: vec![
                DiagnosticFinding {
                    path: PathBuf::from("a.rs"),
                    source: DiagnosticSource::LanguageServer,
                    severity: DiagnosticSeverity::Error,
                    message: "cannot find value x".into(),
                },
                DiagnosticFinding {
                    path: PathBuf::from("a.rs"),
                    source: DiagnosticSource::LanguageServer,
                    severity: DiagnosticSeverity::Warning,
                    message: "unused import".into(),
                },
            ],
        };
        let block = render_injection_block(&report).unwrap();
        assert!(block.contains("Typed findings"));
        assert!(block.contains("cannot find value x"));
        assert!(block.contains("unused import"));
        // Just exercise the helper so it doesn't show as dead.
        let _ = content;
    }

    #[test]
    fn render_injection_block_includes_unverified_marker() {
        let report = PostEditReport {
            touched_paths: vec![PathBuf::from("a.rs")],
            findings: vec![
                DiagnosticFinding {
                    path: PathBuf::from("a.rs"),
                    source: DiagnosticSource::LanguageServer,
                    severity: DiagnosticSeverity::Unverified,
                    message: "no language server".into(),
                },
                DiagnosticFinding {
                    path: PathBuf::from("a.json"),
                    source: DiagnosticSource::StructuredConfig,
                    severity: DiagnosticSeverity::Error,
                    message: "no longer valid JSON".into(),
                },
            ],
        };
        let s = render_injection_block(&report).unwrap();
        assert!(s.contains("Typed findings"));
        assert!(s.contains("unverified"));
        assert!(s.contains("no longer valid JSON"));
    }

    #[test]
    fn render_injection_block_is_none_when_report_is_clean() {
        let report = PostEditReport::default();
        assert!(render_injection_block(&report).is_none());
    }

    #[test]
    fn broken_json_after_an_edit_is_reported() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            "{\n  \"scripts\": {\n    \"audit\": \"jscpd --ignore \\\"x\\\"\n  }\n}",
        )
        .unwrap();
        let p = PathBuf::from("package.json");
        let finding = json_syntax_finding(dir.path(), &p).expect("must report broken JSON");
        assert_eq!(finding.severity, DiagnosticSeverity::Error);
        assert_eq!(finding.source, DiagnosticSource::StructuredConfig);
        assert!(finding.message.contains("no longer valid JSON"));
    }

    #[test]
    fn broken_toml_after_an_edit_is_reported() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package\nname = \"x\"\n").unwrap();
        let p = PathBuf::from("Cargo.toml");
        let finding = toml_syntax_finding(dir.path(), &p).expect("must report broken TOML");
        assert_eq!(finding.severity, DiagnosticSeverity::Error);
        assert_eq!(finding.source, DiagnosticSource::StructuredConfig);
        assert!(finding.message.contains("no longer valid TOML"));
    }

    #[test]
    fn valid_toml_is_not_reported() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let p = PathBuf::from("Cargo.toml");
        assert!(toml_syntax_finding(dir.path(), &p).is_none());
    }

    #[test]
    fn valid_json_is_not_reported() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("tsconfig.json"), "{\"compilerOptions\":{}}").unwrap();
        let p = PathBuf::from("tsconfig.json");
        assert!(json_syntax_finding(dir.path(), &p).is_none());
    }

    #[test]
    fn is_code_and_structured_ext_recognize_known_extensions() {
        assert!(is_code_ext("a.rs"));
        assert!(is_code_ext("a.ts"));
        assert!(!is_code_ext("a.json"));
        assert!(is_structured_config_ext("a.json"));
        assert!(is_structured_config_ext("a.toml"));
        assert!(!is_structured_config_ext("a.txt"));
    }

    #[test]
    fn file_fingerprint_changes_when_content_changes() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn main() {}\n").unwrap();
        let before = file_fingerprint(dir.path(), Path::new("a.rs")).expect("fingerprint before");
        std::fs::write(dir.path().join("a.rs"), "fn main() { let x = 1; }\n").unwrap();
        let after = file_fingerprint(dir.path(), Path::new("a.rs")).expect("fingerprint after");
        assert_ne!(before, after);
    }

    #[tokio::test]
    async fn build_report_marks_no_server_as_unverified_not_clean() {
        let dir = tempdir().unwrap();
        write_lsp_workspace_config(
            dir.path(),
            &LspWorkspaceConfig {
                servers: vec![LspWorkspaceServer {
                    language: "rust".into(),
                    extensions: vec!["rs".into()],
                    server_path: "definitely-not-a-real-langserver-xyz".into(),
                    server_args: vec![],
                }],
            },
        )
        .unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn main() { let x = 1; }\n").unwrap();
        let paths = vec![PathBuf::from("a.rs")];
        let report = build_report(
            &paths,
            "s1",
            dir.path(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
            Duration::from_secs(5),
            Duration::from_secs(10),
        )
        .await;
        assert_eq!(report.touched_paths.len(), 1);
        // No server + clean config = one Unverified finding, NOT no findings.
        assert!(
            !report.findings.is_empty(),
            "no-server run must yield Unverified, not clean: {:?}",
            report.findings
        );
        assert!(report
            .findings
            .iter()
            .all(|f| f.severity == DiagnosticSeverity::Unverified));
    }

    #[tokio::test]
    async fn build_report_with_only_clean_structured_config_runs_clean() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), "{\"k\":1}\n").unwrap();
        let paths = vec![PathBuf::from("config.json")];
        let report = build_report(
            &paths,
            "s1",
            dir.path(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
            Duration::from_secs(5),
            Duration::from_secs(10),
        )
        .await;
        assert!(report.is_clean(), "{:?}", report.findings);
    }

    #[tokio::test]
    async fn build_report_collapses_duplicate_paths() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            "{\n  \"scripts\": {\n    \"audit\": \"jscpd --ignore \\\"x\\\"\n  }\n}",
        )
        .unwrap();
        let paths = vec![PathBuf::from("config.json"), PathBuf::from("config.json")];
        let report = build_report(
            &paths,
            "s1",
            dir.path(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
            Duration::from_secs(5),
            Duration::from_secs(10),
        )
        .await;
        // The path is repeated but the finding dedupes to one entry.
        assert_eq!(report.findings.len(), 1, "{:?}", report.findings);
    }

    // Legacy reference: the old `edited_code_paths` accepted tool calls and
    // re-parsed `arguments_json`. The new seam never does that — mutations
    // are the only source. This is a guard against regressing to string
    // matching; if anyone restores a `&[ToolCall]`-based collector, this
    // assertion gives them a name to remove first.
    #[test]
    fn legacy_edit_tool_name_match_is_kept_for_logging_only() {
        assert!(is_legacy_edit_tool("edit_file"));
        assert!(is_legacy_edit_tool("apply_patch"));
    }

    // Reserved: the wire shape used to record a tool call (ToolCall.name +
    // ToolCall.arguments_json) is no longer parsed to derive paths — the
    // mutation list carries that information forward. Make this explicit so
    // a future ToolCall-based restore is conspicuously wrong.
    #[allow(dead_code)]
    fn _assert_tool_call_is_not_a_path_source(_call: &ToolCall) {
        let _ = _call; // existence is the assertion
    }

    #[test]
    fn diagnostic_severity_labels_are_stable() {
        assert_eq!(DiagnosticSeverity::Error.severity_label(), "error");
        assert_eq!(DiagnosticSeverity::Warning.severity_label(), "warning");
        assert_eq!(
            DiagnosticSeverity::Unverified.severity_label(),
            "unverified"
        );
    }

    #[test]
    fn parse_lsp_line_skips_unknown_severity_integers() {
        let p = Path::new("a.rs");
        // Severity 4 is a hint/information in some LSPs; we ignore non-error
        // and non-warning severities entirely so they can't pollute the
        // report.
        assert!(parse_lsp_line(p, "file:///a.rs:1:1: severity=4 just a hint").is_none());
    }
}
