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

impl PostEditReport {}

/// Sort, dedupe, and absolute-resolve a list of mutations the engine just
/// collected. This is the single seam where `path` strings become typed
/// `WorkspaceMutation`s the rest of the diagnostic pipeline reads. Read-only
/// turns never call this.
pub fn normalize_workspace_mutations(
    workspace_root: &Path,
    mutations: Vec<WorkspaceMutation>,
) -> Vec<WorkspaceMutation> {
    let mut normalized: Vec<WorkspaceMutation> = Vec::with_capacity(mutations.len());
    for m in mutations {
        let absolute = if m.path.is_absolute() {
            m.path.clone()
        } else {
            workspace_root.join(&m.path)
        };
        let canonical = absolute;
        if !normalized
            .iter()
            .any(|existing| existing.path == canonical && existing.kind == m.kind)
        {
            normalized.push(WorkspaceMutation {
                path: canonical,
                kind: m.kind,
                previous_content: m.previous_content,
            });
        }
    }
    normalized.sort_by(|a, b| a.path.cmp(&b.path));
    normalized
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
