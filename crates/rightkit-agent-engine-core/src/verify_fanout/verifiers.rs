//! Bounded concurrent verifier fan-out (PL-007 core machinery + the
//! verifier set layered over CG-B01 `lsp_tool`).
//!
//! Donor lineage (Pi, ADAPT_TO_RUST — verified against checkout
//! `e86823096` of `repos/cli/pi`; the audit names this mechanism family
//! "Lens", and the pinned checkout carries the mechanism surfaces, not a
//! module of that name — see the lane WIRING report for the search receipt):
//!
//! * **Concurrent diagnostics** — Pi `packages/agent/src/agent-loop.ts`
//!   `executeToolCallsParallel` (line 489): collect independent units, run
//!   them all, then reassemble results deterministically in submission
//!   order (`Promise.all` + ordered finalize, lines 540-548). This module
//!   mirrors that: one `VerifierTask` per unit, a tokio `JoinSet` fan-out,
//!   ordered reassembly by submission index.
//! * **Bounded units** — every unit carries its own deadline; a unit that
//!   overruns is recorded `Unavailable`, never clean.
//!
//! Verifier units:
//! * `lsp` — the CG-B01 seam: one unit per code file, running the shared
//!   `LspTool` `diagnostics` op (the exact op `post_edit_diagnostics` uses),
//!   with content-hash staleness checking before a result is trusted.
//! * `config` — dependency/config validation: JSON/TOML parse of touched
//!   config files, bound to the hash of the exact bytes that were parsed.
//! * command units — typecheck / lint / format-check / cheap targeted tests
//!   via a configured [`CommandVerifierSpec`]; applicable only when a
//!   touched path matches its extension list; output parsed from the
//!   standard `path:line:col: severity: message` diagnostic shape.

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use futures::FutureExt;

#[cfg(feature = "host")]
use rightkit_agent_coding_tools::Tool;

use super::budget::VerifyBudgets;
use super::finding::{
    FindingCategory, FindingFixability, FindingRange, FindingSeverity, VerifiedFinding,
};
use super::staleness::{content_hash, content_hash_text, content_hashes};

// ─── Verifier identity & status ─────────────────────────────────────────────

/// Identity of one verifier kind. Per-path units (`lsp`, `config`) share the
/// kind id and are distinguished by `VerifierOutcome::subject`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VerifierId(pub String);

impl VerifierId {
    pub const LSP: &'static str = "lsp";
    pub const CONFIG: &'static str = "config";
    pub const BUDGET: &'static str = "budget";
    pub const PROFILE: &'static str = "profile";
    pub const UNKNOWN: &'static str = "unknown";

    pub fn lsp() -> Self {
        Self(Self::LSP.into())
    }

    pub fn profile() -> Self {
        Self(Self::PROFILE.into())
    }

    pub fn config() -> Self {
        Self(Self::CONFIG.into())
    }

    pub fn command(id: &str) -> Self {
        Self(format!("command:{id}"))
    }

    fn unknown() -> Self {
        Self(Self::UNKNOWN.into())
    }
}

impl fmt::Display for VerifierId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Whether a verifier unit's verdict is required for a verified-clean
/// completion, or optional fast feedback that is reported honestly but never
/// blocks the turn.
///
/// * `Required` — structured JSON/TOML parsing, configured project commands,
///   and the default Python syntax check. A `Required` unit that is
///   `Unavailable`/`Failed` prevents verified final completion (the edit is
///   still unverified) while the agent keeps working.
/// * `Optional` — LSP diagnostics and a provisioned CLI typechecker. An
///   `Optional` unit that cannot run is visibly reported as `Unavailable`
///   but must neither finish the mutation turn nor cause repeated reentry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VerifierRequirement {
    Required,
    Optional,
}

impl VerifierRequirement {
    pub fn is_required(&self) -> bool {
        matches!(self, Self::Required)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Optional => "optional",
        }
    }
}

/// Tri-state verdict of one verifier unit. The canon row requires
/// *unavailable* to be distinct from *clean* and from *failed*:
///
/// * `Clean` — the verifier ran and reported no failures (warnings are not
///   failures; they ride along as findings).
/// * `Unavailable` — the verifier produced no verdict at all: not installed,
///   not applicable, timed out, budget exhausted, content changed
///   mid-flight, or the host cannot run it. No clean claim is made.
/// * `Failed` — the verifier ran and reported failure (error-severity
///   findings, an error response, or a nonzero exit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifierStatus {
    Clean,
    Unavailable { reason: String },
    Failed { reason: String },
}

/// Result of one verifier unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifierOutcome {
    pub verifier: VerifierId,
    /// The file this unit was responsible for, when it is a per-file unit.
    pub subject: Option<PathBuf>,
    pub status: VerifierStatus,
    pub findings: Vec<VerifiedFinding>,
    /// True when `findings` was cut to the per-verifier finding cap.
    pub truncated: bool,
    /// Whether this unit's verdict is required for verified completion.
    /// Constructors fail closed to `Required`; `fan_out_bounded` restamps the
    /// authoritative value from the task so it survives timeouts, panics,
    /// and budget overflow/cancellation.
    pub requirement: VerifierRequirement,
    /// The verified paths this unit covers. Used by the required-coverage
    /// check so a clean verdict on one file can never conceal a required
    /// verifier that produced no verdict on another.
    pub covered_paths: Vec<PathBuf>,
}

impl VerifierOutcome {
    pub fn clean(verifier: VerifierId, subject: Option<PathBuf>) -> Self {
        Self {
            verifier,
            covered_paths: subject.clone().into_iter().collect(),
            subject,
            status: VerifierStatus::Clean,
            findings: Vec::new(),
            truncated: false,
            requirement: VerifierRequirement::Required,
        }
    }

    pub fn unavailable(verifier: VerifierId, subject: Option<PathBuf>, reason: String) -> Self {
        Self {
            verifier,
            covered_paths: subject.clone().into_iter().collect(),
            subject,
            status: VerifierStatus::Unavailable { reason },
            findings: Vec::new(),
            truncated: false,
            requirement: VerifierRequirement::Required,
        }
    }

    pub fn failed(
        verifier: VerifierId,
        subject: Option<PathBuf>,
        reason: String,
        findings: Vec<VerifiedFinding>,
    ) -> Self {
        Self {
            verifier,
            covered_paths: subject.clone().into_iter().collect(),
            subject,
            status: VerifierStatus::Failed { reason },
            findings,
            truncated: false,
            requirement: VerifierRequirement::Required,
        }
    }

    pub fn with_findings(
        verifier: VerifierId,
        subject: Option<PathBuf>,
        status: VerifierStatus,
        findings: Vec<VerifiedFinding>,
    ) -> Self {
        Self {
            verifier,
            covered_paths: subject.clone().into_iter().collect(),
            subject,
            status,
            findings,
            truncated: false,
            requirement: VerifierRequirement::Required,
        }
    }

    /// Override the requirement (default `Required`, fail closed). The
    /// fan-out restamps this from the task metadata; the builder exists for
    /// outcomes constructed outside the fan-out and for tests.
    pub fn with_requirement(mut self, requirement: VerifierRequirement) -> Self {
        self.requirement = requirement;
        self
    }

    /// Declare the paths this unit covers when it differs from `subject`
    /// (command units verify several files under one subject-less outcome).
    pub fn with_covered_paths(mut self, covered_paths: Vec<PathBuf>) -> Self {
        self.covered_paths = covered_paths;
        self
    }

    fn enforce_finding_cap(&mut self, cap: usize) {
        if self.findings.len() > cap {
            self.findings.truncate(cap);
            self.truncated = true;
        }
    }
}

/// Roll-up of the whole fan-out. Same tri-state discipline as
/// [`VerifierStatus`], one level up: `Failed` wins over `Clean`, and
/// "nothing ran" is `Unavailable`, never `Clean`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AggregateStatus {
    Clean,
    Unavailable { reason: String },
    Failed { reason: String },
}

// ─── Bounded concurrent fan-out ─────────────────────────────────────────────

/// One runnable verifier unit.
pub(crate) struct VerifierTask {
    pub verifier: VerifierId,
    pub subject: Option<PathBuf>,
    /// Whether the unit's verdict is required for verified completion.
    /// Stamped onto the outcome even when the unit never returns one
    /// (timeout, panic, overflow, cancellation) so requirement can never be
    /// laundered into a clean claim.
    pub requirement: VerifierRequirement,
    /// The verified paths this unit covers.
    pub covered_paths: Vec<PathBuf>,
    pub budget: Duration,
    pub run: Pin<Box<dyn Future<Output = VerifierOutcome> + Send>>,
}

/// Run verifier units concurrently under the budgets.
///
/// Pi-donor shape (see module doc): all admitted units run concurrently;
/// results are reassembled in submission order, so the report is
/// deterministic regardless of completion order. Every unit is bounded by
/// its per-unit budget, and the whole set by the aggregate budget; anything
/// that does not finish is recorded `Unavailable` with the honest reason.
/// A panicking unit degrades to `Unavailable` too — one bad verifier can
/// never take down the turn.
pub(crate) async fn fan_out_bounded(
    tasks: Vec<VerifierTask>,
    budgets: &VerifyBudgets,
) -> Vec<VerifierOutcome> {
    let mut slots: Vec<Option<VerifierOutcome>> = Vec::with_capacity(tasks.len());
    let mut identities: Vec<(
        VerifierId,
        Option<PathBuf>,
        VerifierRequirement,
        Vec<PathBuf>,
    )> = Vec::with_capacity(tasks.len());
    let mut admitted: Vec<VerifierTask> = Vec::new();
    let mut overflowed: Vec<VerifierTask> = Vec::new();

    for task in tasks {
        // Only admitted units get a result slot. A unit that was never
        // started must not also be reported as "produced no result" — it is
        // accounted for exactly once, by the per-unit budget records below.
        if admitted.len() < budgets.max_verifiers {
            slots.push(None);
            identities.push((
                task.verifier.clone(),
                task.subject.clone(),
                task.requirement,
                task.covered_paths.clone(),
            ));
            admitted.push(task);
        } else {
            overflowed.push(task);
        }
    }

    let mut set = tokio::task::JoinSet::new();
    for (index, task) in admitted.into_iter().enumerate() {
        // The unit's identity is captured by the spawned guard so a timeout
        // or panic reports the real verifier — never `unknown` — and the
        // caller's slot bookkeeping stays authoritative.
        let VerifierTask {
            verifier,
            subject,
            budget,
            run,
            ..
        } = task;
        set.spawn(async move {
            // A panicking unit must degrade to Unavailable, not poison the
            // fan-out (same poison-safe discipline as the rest of the
            // engine). Identity survives: the guard builds the degraded
            // outcome with this unit's own verifier + subject.
            let guarded = AssertUnwindSafe(run).catch_unwind();
            let outcome = match tokio::time::timeout(budget, guarded).await {
                Ok(Ok(outcome)) => outcome,
                Ok(Err(_)) => VerifierOutcome::unavailable(
                    verifier,
                    subject,
                    "verifier unit panicked; degrading cleanly".into(),
                ),
                Err(_) => VerifierOutcome::unavailable(
                    verifier,
                    subject,
                    format!("per-verifier budget exhausted after {budget:?}"),
                ),
            };
            (index, outcome)
        });
    }

    let finished_within_aggregate = tokio::time::timeout(budgets.aggregate, async {
        while let Some(joined) = set.join_next().await {
            if let Ok((index, mut outcome)) = joined {
                outcome.enforce_finding_cap(budgets.max_findings_per_verifier);
                if index < slots.len() {
                    // Restamp authoritative metadata from the admitted task:
                    // verifier identity, requirement, and covered paths come
                    // from the unit declaration, not from whatever the unit's
                    // future happened to return before it timed out, panicked,
                    // or was cancelled.
                    let (verifier, subject, requirement, covered) = identities[index].clone();
                    outcome.verifier = verifier;
                    if outcome.subject.is_none() {
                        outcome.subject = subject;
                    }
                    outcome.requirement = requirement;
                    outcome.covered_paths = covered;
                    slots[index] = Some(outcome);
                }
            }
        }
    })
    .await
    .is_ok();
    // Dropping the JoinSet aborts anything still running: bounded
    // cancellation. The units' own kill-on-drop / request timeouts clean up
    // their subprocesses.
    drop(set);

    let mut outcomes = Vec::with_capacity(slots.len() + overflowed.len() + 1);
    for (index, slot) in slots.into_iter().enumerate() {
        match slot {
            Some(outcome) => outcomes.push(outcome),
            None => {
                let (verifier, subject, requirement, covered) =
                    identities.get(index).cloned().unwrap_or_else(|| {
                        (
                            VerifierId::unknown(),
                            None,
                            VerifierRequirement::Required,
                            Vec::new(),
                        )
                    });
                let reason = if finished_within_aggregate {
                    "verifier unit produced no result".to_string()
                } else {
                    "aggregate verification budget exhausted before this verifier completed"
                        .to_string()
                };
                outcomes.push(
                    VerifierOutcome::unavailable(verifier, subject, reason)
                        .with_requirement(requirement)
                        .with_covered_paths(covered),
                );
            }
        }
    }
    // Overflowed units keep their identity and requirement: each becomes its
    // own Unavailable record under the shared `budget` marker so a required
    // unit squeezed out by the unit cap still blocks verified completion for
    // the exact paths it would have covered. `FanoutReport::budget_exhausted`
    // keys off the `budget` verifier id, which these records carry.
    for task in &overflowed {
        outcomes.push(
            VerifierOutcome::unavailable(
                task.verifier.clone(),
                task.subject.clone(),
                format!(
                    "verifier '{}' was not started: the verifier-unit budget ({}) is exhausted",
                    task.verifier, budgets.max_verifiers
                ),
            )
            .with_requirement(task.requirement)
            .with_covered_paths(task.covered_paths.clone()),
        );
    }
    if !overflowed.is_empty() {
        outcomes.push(
            VerifierOutcome::unavailable(
                VerifierId(VerifierId::BUDGET.into()),
                None,
                format!(
                    "{} verifier unit(s) exceeded admission budget",
                    overflowed.len()
                ),
            )
            .with_requirement(VerifierRequirement::Optional),
        );
    }
    outcomes
}

// ─── Path classification (mirrors the post-edit diagnostics seam) ──────────

fn extension_of(path: &Path) -> &str {
    path.extension().and_then(|e| e.to_str()).unwrap_or("")
}

pub(crate) fn is_code_ext(path: &Path) -> bool {
    matches!(
        extension_of(path),
        "rs" | "ts" | "tsx" | "js" | "jsx" | "py"
    )
}

pub(crate) fn is_structured_config_ext(path: &Path) -> bool {
    matches!(extension_of(path), "json" | "toml")
}

fn resolve_in_workspace(workspace_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    }
}

// ─── LSP diagnostics unit (CG-B01 layer) ────────────────────────────────────

/// Parse one `format_diagnostic_line` rendering into (range, severity,
/// message). Line format:
/// `{uri}:{line_1based}:{character} severity={n} {message}`. The URI is
/// ignored — findings are attributed to the queried path, exactly like the
/// existing post-edit diagnostics seam.
pub(crate) fn parse_lsp_finding_line(
    line: &str,
) -> Option<(FindingRange, FindingSeverity, String)> {
    let (head, tail) = line.split_once(" severity=")?;
    let (severity_token, message) = tail.split_once(char::is_whitespace).unwrap_or((tail, ""));
    let severity = FindingSeverity::from_lsp(severity_token.trim().parse().ok()?)?;
    // head is `uri:line:character`; parse the last two numeric components
    // from the right so URIs that contain colons (file:///C:/...) survive.
    let (without_char, character) = head.rsplit_once(':')?;
    let (_, line_1based) = without_char.rsplit_once(':')?;
    let line_1based: u32 = line_1based.trim().parse().ok()?;
    let character: u32 = character.trim().parse().ok()?;
    Some((
        FindingRange::point(line_1based.saturating_sub(1), character),
        severity,
        message.trim().to_string(),
    ))
}

#[cfg(feature = "host")]
async fn run_lsp_unit(
    path: PathBuf,
    tool_ctx: rightkit_agent_coding_tools::ToolContext,
    per_file: Duration,
) -> VerifierOutcome {
    let verifier = VerifierId::lsp();
    if !path.exists() {
        return VerifierOutcome::unavailable(
            verifier,
            Some(path.clone()),
            "path no longer exists on disk; nothing to verify".into(),
        );
    }
    let before_hash = content_hash(&path);
    let lsp = rightkit_agent_coding_tools::lsp_tool::LspTool;
    let args = serde_json::json!({
        "operation": "diagnostics",
        "path": path.to_string_lossy(),
    });
    match tokio::time::timeout(per_file, lsp.execute(args, &tool_ctx)).await {
        Ok(result) if !result.is_error => {
            let after_hash = content_hash(&path);
            if before_hash.is_some() && before_hash != after_hash {
                return VerifierOutcome::unavailable(
                    verifier,
                    Some(path.clone()),
                    "content changed during the language-server pass; the reported result is stale"
                        .into(),
                );
            }
            let message = result.content;
            if let Some(body) = message
                .strip_prefix(rightkit_agent_coding_tools::lsp_tool::DIAGNOSTICS_PREFIX)
            {
                let findings: Vec<VerifiedFinding> = body
                    .lines()
                    .filter_map(parse_lsp_finding_line)
                    .map(|(range, severity, text)| VerifiedFinding {
                        source: VerifierId::LSP.into(),
                        range: Some(range),
                        severity,
                        category: FindingCategory::Lsp,
                        message: text,
                        content_hash: after_hash.clone(),
                        fixability: FindingFixability::Manual,
                        path: path.clone(),
                    })
                    .collect();
                if findings
                    .iter()
                    .any(|f| f.severity == FindingSeverity::Error)
                {
                    VerifierOutcome::failed(
                        verifier,
                        Some(path),
                        "language-server diagnostics reported error-severity findings".into(),
                        findings,
                    )
                } else {
                    VerifierOutcome::with_findings(
                        verifier,
                        Some(path),
                        VerifierStatus::Clean,
                        findings,
                    )
                }
            } else if message.contains("No diagnostics reported.") {
                VerifierOutcome::clean(verifier, Some(path))
            } else {
                VerifierOutcome::unavailable(
                    verifier,
                    Some(path),
                    "language server not configured, produced no diagnostics schema, or an \
                     unrecognized response; no diagnostics verified"
                        .into(),
                )
            }
        }
        Ok(result) => {
            // The lsp op's infrastructure failures announce themselves with
            // this well-known prefix (`tool_bridge.rs`); a verifier that
            // could not run at all is Unavailable — distinct from a
            // verifier that ran and reported failure.
            if result.content.starts_with("LSP diagnostics unavailable") {
                VerifierOutcome::unavailable(
                    verifier,
                    Some(path),
                    format!("language server could not run: {}", result.content),
                )
            } else {
                VerifierOutcome::failed(
                    verifier,
                    Some(path),
                    "language server returned an error; not misreporting as clean".into(),
                    Vec::new(),
                )
            }
        }
        Err(_) => VerifierOutcome::unavailable(
            verifier,
            Some(path),
            "timed out waiting for language server; no diagnostics verified".into(),
        ),
    }
}

#[cfg(not(feature = "host"))]
async fn run_lsp_unit(
    path: PathBuf,
    _tool_ctx: rightkit_agent_coding_tools::ToolContext,
    _per_file: Duration,
) -> VerifierOutcome {
    VerifierOutcome::unavailable(
        VerifierId::lsp(),
        Some(path),
        "this host cannot run a language server; the edit is unverified, not clean".into(),
    )
}

// ─── Structured-config unit (dependency/config validation leg) ─────────────

/// Parse one touched JSON/TOML config file; binds the finding to the hash of
/// the exact bytes that were parsed. Message wording mirrors the
/// structured-config findings in `post_edit_diagnostics`.
pub(crate) fn run_config_unit(workspace_root: &Path, path: &Path) -> VerifierOutcome {
    let verifier = VerifierId::config();
    let absolute = resolve_in_workspace(workspace_root, path);
    let text = match std::fs::read_to_string(&absolute) {
        Ok(text) => text,
        Err(error) => {
            return VerifierOutcome::unavailable(
                verifier,
                Some(path.to_path_buf()),
                format!("could not read file for verification: {error}"),
            )
        }
    };
    let binding = content_hash_text(&text);
    let format_name = match extension_of(path) {
        "json" => "JSON",
        "toml" => "TOML",
        other => {
            return VerifierOutcome::unavailable(
                verifier,
                Some(path.to_path_buf()),
                format!("no structured-config check for extension '{other}'"),
            )
        }
    };
    let parse_result: Result<(), (String, Option<(u32, u32)>)> = match format_name {
        "JSON" => serde_json::from_str::<serde_json::Value>(&text)
            .map(|_| ())
            .map_err(|e| (e.to_string(), Some((e.line() as u32, e.column() as u32)))),
        _ => toml::from_str::<toml::Value>(&text)
            .map(|_| ())
            .map_err(|e| (e.to_string(), None)),
    };
    match parse_result {
        Ok(()) => VerifierOutcome::clean(verifier, Some(path.to_path_buf())),
        Err((error, position)) => {
            let range = position.map(|(line_1based, column_1based)| {
                FindingRange::point(
                    line_1based.saturating_sub(1),
                    column_1based.saturating_sub(1),
                )
            });
            let finding = VerifiedFinding {
                source: VerifierId::CONFIG.into(),
                range,
                severity: FindingSeverity::Error,
                category: FindingCategory::Config,
                message: format!(
                    "{} is no longer valid {format_name} after this edit: {error}. The file must parse — re-read it and repair the syntax before continuing.",
                    path.display()
                ),
                content_hash: Some(binding),
                fixability: FindingFixability::FixAvailable {
                    hint: "re-read the file and repair the syntax before continuing".into(),
                },
                path: path.to_path_buf(),
            };
            VerifierOutcome::failed(
                verifier,
                Some(path.to_path_buf()),
                "config no longer parses".into(),
                vec![finding],
            )
        }
    }
}

// ─── Command verifier units (typecheck / lint / format / tests) ────────────

/// The `{paths}` argv placeholder: expands to one argument per applicable
/// path. An argv without the placeholder keeps its literal shape, so
/// workspace-level commands are never force-fed file arguments.
pub(crate) const PATHS_PLACEHOLDER: &str = "{paths}";

/// A configured external verifier: typecheck, lint, format-check,
/// static/AST, or a cheap targeted test. One spec = one fan-out unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandVerifierSpec {
    /// Verifier name; its findings' `source` is `command:<id>`.
    pub id: String,
    pub category: FindingCategory,
    /// Program + arguments. An argument exactly equal to `"{paths}"` expands
    /// to one argument per applicable path. Without that explicit placeholder,
    /// argv stays unchanged (workspace commands do not accept source paths).
    pub argv: Vec<String>,
    /// Extensions this verifier applies to. Empty = applies to every touched
    /// path (workspace-level verifiers such as `cargo check`).
    pub applicable_extensions: Vec<String>,
    /// Workspace-relative directory prefixes this verifier applies to
    /// (`"src/"`, `"tests/"`). Empty = no prefix narrowing. Combined with
    /// `applicable_extensions`: both must match when both are non-empty.
    pub path_prefixes: Vec<String>,
    /// Whether a verdict from this verifier is required for verified
    /// completion. Configured project commands and the built-in structural
    /// checks are `Required`; set `false` for advisory checks whose absence
    /// must be visible but never blocks the turn.
    pub required: bool,
    /// True when the verifier advertises a mechanical fix.
    pub fixable: bool,
}

/// Native workspace verifier set used by the live post-edit gate when a host
/// does not provide an override. Every command is still bounded by fan-out
/// budgets; missing manifests are represented by an empty set, so absence is
/// reported by existing LSP/config units rather than claimed clean.
pub fn default_command_verifiers(workspace_root: &Path) -> Vec<CommandVerifierSpec> {
    if !workspace_root.join("Cargo.toml").is_file() {
        return Vec::new();
    }
    let manifest = workspace_root
        .join("Cargo.toml")
        .to_string_lossy()
        .to_string();
    let cargo = |id: &str, category: FindingCategory, args: &[&str], fixable: bool| {
        let mut argv = vec!["cargo".to_string()];
        let mut manifest_added = false;
        for arg in args {
            if *arg == "--" && !manifest_added {
                argv.extend(["--manifest-path".to_string(), manifest.clone()]);
                manifest_added = true;
            }
            argv.push((*arg).to_string());
        }
        if !manifest_added {
            argv.extend(["--manifest-path".to_string(), manifest.clone()]);
        }
        CommandVerifierSpec {
            id: id.to_string(),
            category,
            argv,
            applicable_extensions: vec!["rs".into(), "toml".into(), "lock".into()],
            path_prefixes: Vec::new(),
            required: true,
            fixable,
        }
    };
    vec![
        cargo(
            "typecheck",
            FindingCategory::TypeCheck,
            &["check", "--workspace"],
            false,
        ),
        cargo(
            "lint-static",
            FindingCategory::StaticAnalysis,
            &["clippy", "--workspace", "--all-targets"],
            false,
        ),
        cargo(
            "format",
            FindingCategory::Format,
            &["fmt", "--all", "--", "--check"],
            true,
        ),
        cargo(
            "dependencies",
            FindingCategory::Dependency,
            &["metadata", "--no-deps", "--format-version", "1"],
            false,
        ),
        cargo(
            "targeted-tests",
            FindingCategory::Test,
            &["test", "--workspace", "--lib"],
            false,
        ),
    ]
}

impl CommandVerifierSpec {
    /// Extension-only applicability (kept for callers/tests that have no
    /// workspace root; `build_units` uses [`Self::applies_to_in`]).
    pub fn applies_to(&self, path: &Path) -> bool {
        self.applicable_extensions.is_empty()
            || self
                .applicable_extensions
                .iter()
                .any(|ext| ext == extension_of(path))
    }

    /// Full applicability: extension match AND `pathPrefixes` narrowing.
    /// Prefixes are workspace-relative directories compared on normalized
    /// `/` separators with a path-segment boundary.
    pub fn applies_to_in(&self, workspace_root: &Path, path: &Path) -> bool {
        if !self.applies_to(path) {
            return false;
        }
        if self.path_prefixes.is_empty() {
            return true;
        }
        let relative = path.strip_prefix(workspace_root).unwrap_or(path);
        let normalized = relative.to_string_lossy().replace('\\', "/");
        self.path_prefixes.iter().any(|prefix| {
            let prefix = prefix.trim_matches('/');
            normalized == prefix || normalized.starts_with(&format!("{prefix}/"))
        })
    }
}

pub(crate) fn expand_command_args(argv: &[String], paths: &[PathBuf]) -> Vec<String> {
    let path_args: Vec<String> = paths
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    let mut expanded: Vec<String> = Vec::new();
    for arg in argv {
        if arg == PATHS_PLACEHOLDER {
            expanded.extend(path_args.iter().cloned());
        } else {
            expanded.push(arg.clone());
        }
    }
    expanded
}

/// One parsed diagnostic line from a command verifier's output.
pub(crate) struct ParsedCompilerLine {
    pub path: String,
    /// 1-based, exactly as emitted by the producer.
    pub line: u32,
    pub column: Option<u32>,
    pub severity: FindingSeverity,
    pub message: String,
}

/// Split `path:line[:col]` out of the left-of-`": "` head. Handles Windows
/// drive-letter paths by re-joining the single-letter first component.
pub(crate) fn split_location_head(head: &str) -> Option<(String, u32, Option<u32>)> {
    let mut components: Vec<String> = head.split(':').map(str::to_string).collect();
    if components.len() >= 3 && components[0].chars().count() == 1 {
        // Windows drive letter: "C" + "\src\a.rs" belong together.
        let second = components.remove(1);
        components[0] = format!("{}:{}", components[0], second);
    }
    if components.len() < 2 {
        return None;
    }
    let last_index = components.len() - 1;
    let (line_part, column_part) = if last_index >= 2 {
        (
            components[last_index - 1].as_str(),
            Some(components[last_index].as_str()),
        )
    } else {
        (components[last_index].as_str(), None)
    };
    let line: u32 = line_part.trim().parse().ok()?;
    let column: Option<u32> = match column_part {
        Some(text) => text.trim().parse().ok(),
        None => None,
    };
    let path_length = if column_part.is_some() {
        last_index - 1
    } else {
        last_index
    };
    components.truncate(path_length);
    let path = components.join(":");
    if path.is_empty() {
        return None;
    }
    Some((path, line, column))
}

fn classify_severity(rest: &str) -> (FindingSeverity, String) {
    let lowered = rest.to_ascii_lowercase();
    let (severity, consumed) = if lowered.starts_with("error") {
        (FindingSeverity::Error, "error".len())
    } else if lowered.starts_with("warning") {
        (FindingSeverity::Warning, "warning".len())
    } else if lowered.starts_with("note") {
        (FindingSeverity::Information, "note".len())
    } else if lowered.starts_with("info") {
        (FindingSeverity::Information, "info".len())
    } else if lowered.starts_with("hint") {
        (FindingSeverity::Hint, "hint".len())
    } else {
        // No recognized severity word: keep the line as a warning so nothing
        // actionable is lost, but nothing spuriously blocks either.
        return (FindingSeverity::Warning, rest.to_string());
    };
    let mut tail = &rest[consumed..];
    // Skip an optional `[E0308]`-style code tag.
    if let Some(rest_of_tag) = tail.strip_prefix('[') {
        if let Some(close) = rest_of_tag.find(']') {
            tail = &rest_of_tag[close + 1..];
        }
    }
    let message = tail
        .strip_prefix(':')
        .map(|m| m.strip_prefix(' ').unwrap_or(m))
        .unwrap_or(tail)
        .trim()
        .to_string();
    (severity, message)
}

/// Parse one diagnostic line in the standard compiler shape:
/// `path:line:col: severity: message` (column and severity word optional).
pub(crate) fn parse_compiler_line(line: &str) -> Option<ParsedCompilerLine> {
    let (head, rest) = line.split_once(": ")?;
    let (path, line_no, column) = split_location_head(head)?;
    let (severity, message) = classify_severity(rest);
    Some(ParsedCompilerLine {
        path,
        line: line_no,
        column,
        severity,
        message,
    })
}

/// Kill-on-drop guard for a managed verifier child: when the fan-out budget
/// aborts the unit's future mid-await, dropping the bare `ManagedChild` box
/// would leak the OS process (tokio children are not kill-on-drop). The
/// adapter's `start_kill` is the best-effort terminate used here — safe after
/// exit and requiring no async cleanup inside `Drop`.
#[cfg(feature = "host")]
struct KillOnDrop(Option<Box<dyn rightkit_sandbox::execution::ManagedChild>>);

#[cfg(feature = "host")]
impl KillOnDrop {
    fn child(&mut self) -> &mut dyn rightkit_sandbox::execution::ManagedChild {
        self.0.as_mut().expect("managed child present").as_mut()
    }
}

#[cfg(feature = "host")]
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            child.start_kill();
        }
    }
}

/// Spawn one verifier process through the existing native launch adapter.
/// Project trust is checked when assembling units; child output & lifetime
/// and collect bounded output + exit status. Spawn/pipe failure degrades to
/// `Unavailable`; a run that never completed is `Failed`. stdout/stderr are
/// drained through the shared `capture_stream_bounded` contract so a loud
/// verifier can neither deadlock the child on a full pipe nor flood the
/// report; truncation is reported honestly on the captured stderr tail.
/// Shared by command verifiers and the provisioned Pyright CLI unit.
/// Launch environment requesting read/execute access to `read_roots` from the
/// sandbox (consumed by the AppContainer backend; empty roots add nothing).
#[cfg(feature = "host")]
pub(crate) fn launch_read_roots_env(read_roots: &[PathBuf]) -> Vec<(String, String)> {
    rightkit_agent_coding_tools::sandbox::sandbox_read_roots_env(read_roots)
}

#[cfg(feature = "host")]
// VerifierOutcome is the verifier result contract shared by every verifier; boxing it would change them all.
#[allow(clippy::result_large_err)]
async fn run_process_capture(
    verifier: &VerifierId,
    spec_id: &str,
    program: String,
    argv: Vec<String>,
    workspace_root: PathBuf,
    read_roots: &[PathBuf],
) -> Result<(Vec<u8>, Vec<u8>, i32), Box<VerifierOutcome>> {
    use rightkit_sandbox::execution::{
        capture_stream_bounded, CommandLaunchSpec, OutputLimits, ProcessLaunchAdapter,
        ProcessLaunchContext,
    };
    let adapter = rightkit_agent_coding_tools::shell::NativeCommandSandboxAdapter;
    let mut child = KillOnDrop(Some(
        adapter
            .spawn(CommandLaunchSpec {
                program,
                argv,
                cwd: workspace_root.to_string_lossy().into_owned(),
                context: ProcessLaunchContext {
                    workspace_root: workspace_root.to_string_lossy().into_owned(),
                    sandbox_profile: Some("workspace".to_string()),
                    sandbox_required: true,
                    network_required: false,
                },
                env: launch_read_roots_env(read_roots),
            })
            .await
            .map_err(|error| {
                Box::new(VerifierOutcome::unavailable(
                    verifier.clone(),
                    None,
                    format!("verifier '{spec_id}' could not start: {error:?}"),
                ))
            })?,
    ));
    // Verifiers never read stdin; drop it so a child that waits on EOF can
    // exit instead of pinning the unit's budget.
    drop(child.child().take_stdin());
    let limits = OutputLimits::default();
    let stdout = match child.child().take_stdout() {
        Some(stdout) => stdout,
        None => {
            let _ = child.child().cancel().await;
            let _ = child.child().wait().await;
            return Err(Box::new(VerifierOutcome::unavailable(
                verifier.clone(),
                None,
                "verifier stdout unavailable".into(),
            )));
        }
    };
    let stderr = match child.child().take_stderr() {
        Some(stderr) => stderr,
        None => {
            let _ = child.child().cancel().await;
            let _ = child.child().wait().await;
            return Err(Box::new(VerifierOutcome::unavailable(
                verifier.clone(),
                None,
                "verifier stderr unavailable".into(),
            )));
        }
    };
    let (stdout, stderr) = tokio::join!(
        capture_stream_bounded(stdout, limits.stdout_bytes),
        capture_stream_bounded(stderr, limits.stderr_bytes),
    );
    match (stdout, stderr, child.child().wait().await) {
        (Ok((stdout, out_truncated)), Ok((mut stderr, err_truncated)), Ok(code)) => {
            if out_truncated || err_truncated {
                stderr.extend_from_slice(
                    b"\n(verifier output truncated at the bounded capture limit)",
                );
            }
            Ok((stdout, stderr, code))
        }
        (stdout, stderr, wait) => {
            let _ = child.child().cancel().await;
            Err(Box::new(VerifierOutcome::failed(
                verifier.clone(),
                None,
                format!(
                    "verifier '{spec_id}' did not complete: {:?} {:?} {:?}",
                    stdout.err(),
                    stderr.err(),
                    wait.err()
                ),
                Vec::new(),
            )))
        }
    }
}

/// Run one command verifier unit. Spawn failure degrades to `Unavailable`
/// (the verifier is not installed / not runnable — distinct from clean).
/// A completed run with error-severity findings or a nonzero exit is
/// `Failed`; a completed run with only warnings/hints is `Clean` with the
/// findings attached. A `Failed` verdict is carried even when no line parsed
/// as a diagnostic — the outcome's `reason` is visible evidence, so an
/// unparseable failure is never dropped because `findings` is empty.
pub(crate) async fn run_command_unit(
    spec: CommandVerifierSpec,
    workspace_root: PathBuf,
    paths: Vec<PathBuf>,
) -> VerifierOutcome {
    let verifier = VerifierId::command(&spec.id);
    let expanded = expand_command_args(&spec.argv, &paths);
    let Some(program) = expanded.first().cloned() else {
        return VerifierOutcome::unavailable(verifier, None, "verifier argv is empty".into());
    };
    // A command verifier needs a process. The portable profile has none, so
    // it reports `unavailable` -- which the fan-out already treats as
    // distinct from both clean and failed.
    #[cfg(not(feature = "host"))]
    {
        let _ = (program, workspace_root, spec);
        return VerifierOutcome::unavailable(
            verifier,
            None,
            "command verifiers require the host feature; process launch is unavailable in the portable profile".into(),
        );
    }
    #[cfg(feature = "host")]
    {
        let Some(program) = super::profile::resolve_program(&program, &workspace_root) else {
            return VerifierOutcome::unavailable(
                verifier,
                None,
                format!("verifier '{}' executable could not be resolved", spec.id),
            );
        };
        let code = match run_process_capture(
            &verifier,
            &spec.id,
            program.to_string_lossy().into_owned(),
            expanded[1..].to_vec(),
            workspace_root.clone(),
            &super::profile::interpreter_read_roots(&program),
        )
        .await
        {
            Ok(code) => code,
            Err(outcome) => return *outcome,
        };
        let mut text = String::from_utf8_lossy(&code.0).into_owned();
        text.push('\n');
        text.push_str(&String::from_utf8_lossy(&code.1));

        // Findings bind to post-run content: a verifier that rewrote files (a
        // formatter, a codegen step) leaves findings bound to what exists now,
        // and anything describing pre-run content will be discarded as stale.
        let post_hashes: HashMap<PathBuf, String> = content_hashes(&paths);
        let exit_ok = code.2 == 0;
        let findings: Vec<VerifiedFinding> = text
            .lines()
            .filter_map(parse_compiler_line)
            .map(|parsed| {
                let absolute = resolve_in_workspace(&workspace_root, Path::new(&parsed.path));
                let binding = post_hashes
                    .get(&absolute)
                    .cloned()
                    .or_else(|| content_hash(&absolute));
                let range = FindingRange::point(
                    parsed.line.saturating_sub(1),
                    parsed.column.map(|c| c.saturating_sub(1)).unwrap_or(0),
                );
                let fixability = if spec.fixable {
                    FindingFixability::FixAvailable {
                        hint: format!("'{}' advertises a mechanical fix for this finding", spec.id),
                    }
                } else {
                    FindingFixability::Manual
                };
                VerifiedFinding {
                    source: verifier.0.clone(),
                    range: Some(range),
                    severity: parsed.severity,
                    category: spec.category,
                    message: parsed.message,
                    content_hash: binding,
                    fixability,
                    path: absolute,
                }
            })
            .collect();

        let has_error = findings
            .iter()
            .any(|f| f.severity == FindingSeverity::Error);
        let status = if exit_ok && !has_error {
            VerifierStatus::Clean
        } else if has_error {
            VerifierStatus::Failed {
                reason: format!("verifier '{}' reported error-severity findings", spec.id),
            }
        } else {
            VerifierStatus::Failed {
                reason: format!(
                    "verifier '{}' exited unsuccessfully ({}): {}",
                    spec.id,
                    code.2,
                    text.chars().take(2000).collect::<String>()
                ),
            }
        };
        VerifierOutcome::with_findings(verifier, None, status, findings)
    }
}

// ─── Python verification (required syntax + optional provisioned typecheck) ─

/// Verifier id for the default Python syntax check (required).
pub(crate) const PYTHON_SYNTAX_ID: &str = "python-syntax";

/// Verifier id for the optional Pyright CLI typecheck.
pub(crate) const PYRIGHT_ID: &str = "pyright";

/// Byte-level `compile()` syntax check run through the project's resolved
/// interpreter. `compile()` never writes `__pycache__` (only `import`
/// machinery does), so verification mutates nothing. Output lines use the
/// standard `path:line:col: severity: message` shape the command-verifier
/// parser already reads.
pub(crate) const PYTHON_SYNTAX_HELPER: &str = r#"import sys
failures = 0
for path in sys.argv[1:]:
    try:
        with open(path, "rb") as handle:
            source = handle.read()
        compile(source, path, "exec")
    except SyntaxError as error:
        print("{0}:{1}:{2}: error: {3}".format(path, error.lineno or 1, error.offset or 1, error.msg))
        failures += 1
    except Exception as error:
        print("{0}:1:1: error: could not compile source: {1}".format(path, error))
        failures += 1
sys.exit(1 if failures else 0)
"#;

/// Build the required Python syntax spec bound to one resolved interpreter.
fn python_syntax_spec(interpreter: &Path) -> CommandVerifierSpec {
    CommandVerifierSpec {
        id: PYTHON_SYNTAX_ID.into(),
        category: FindingCategory::Syntax,
        argv: vec![
            interpreter.to_string_lossy().into_owned(),
            "-c".into(),
            PYTHON_SYNTAX_HELPER.into(),
            PATHS_PLACEHOLDER.into(),
        ],
        applicable_extensions: vec!["py".into()],
        path_prefixes: Vec::new(),
        required: true,
        fixable: false,
    }
}

/// Optional Pyright CLI typecheck (`pyright --outputjson <paths>`). Only
/// assembled when the executable was provisioned during profile discovery;
/// absence is reported by `build_units` as an explicit optional
/// `Unavailable`, so the verifier is honestly "not provisioned" rather than
/// silently absent or a fake clean.
#[cfg(feature = "host")]
async fn run_pyright_unit(
    executable: PathBuf,
    workspace_root: PathBuf,
    paths: Vec<PathBuf>,
) -> VerifierOutcome {
    let verifier = VerifierId::command(PYRIGHT_ID);
    let mut argv = vec!["--outputjson".to_string()];
    argv.extend(paths.iter().map(|p| p.to_string_lossy().into_owned()));
    let (stdout, _stderr, code) = match run_process_capture(
        &verifier,
        PYRIGHT_ID,
        executable.to_string_lossy().into_owned(),
        argv,
        workspace_root.clone(),
        &[],
    )
    .await
    {
        Ok(captured) => captured,
        Err(outcome) => return *outcome,
    };
    let parsed: Result<serde_json::Value, _> = serde_json::from_slice(&stdout);
    let value = match parsed {
        Ok(value) => value,
        Err(_) if code != 0 => {
            return VerifierOutcome::failed(
                verifier,
                None,
                format!("pyright exited unsuccessfully ({code}) with unparseable output"),
                Vec::new(),
            );
        }
        Err(_) => {
            return VerifierOutcome::unavailable(
                verifier,
                None,
                "pyright produced output that could not be parsed; no verdict is claimed".into(),
            );
        }
    };
    let post_hashes: HashMap<PathBuf, String> = content_hashes(&paths);
    let mut findings: Vec<VerifiedFinding> = Vec::new();
    if let Some(diagnostics) = value.get("generalDiagnostics").and_then(|d| d.as_array()) {
        for diagnostic in diagnostics {
            let file = diagnostic
                .get("file")
                .and_then(|f| f.as_str())
                .unwrap_or_default();
            let severity = match diagnostic.get("severity").and_then(|s| s.as_str()) {
                Some("error") => FindingSeverity::Error,
                Some("information") => FindingSeverity::Information,
                // Warnings and unknown severities stay visible but never
                // block by themselves.
                _ => FindingSeverity::Warning,
            };
            // Pyright JSON ranges are 0-based LSP convention already.
            let line = diagnostic
                .pointer("/range/start/line")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let character = diagnostic
                .pointer("/range/start/character")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let message = diagnostic
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or_default()
                .to_string();
            let path = resolve_in_workspace(&workspace_root, Path::new(file));
            let binding = post_hashes
                .get(&path)
                .cloned()
                .or_else(|| content_hash(&path));
            findings.push(VerifiedFinding {
                source: verifier.0.clone(),
                range: Some(FindingRange::point(line, character)),
                severity,
                category: FindingCategory::TypeCheck,
                message,
                content_hash: binding,
                fixability: FindingFixability::Manual,
                path,
            });
        }
    }
    let error_count = value
        .pointer("/summary/errorCount")
        .and_then(|v| v.as_u64())
        .unwrap_or_else(|| {
            findings
                .iter()
                .filter(|f| f.severity == FindingSeverity::Error)
                .count() as u64
        });
    if error_count > 0 {
        VerifierOutcome::with_findings(
            verifier,
            None,
            VerifierStatus::Failed {
                reason: format!("pyright reported {error_count} error(s)"),
            },
            findings,
        )
    } else if code != 0 {
        VerifierOutcome::with_findings(
            verifier,
            None,
            VerifierStatus::Failed {
                reason: format!("pyright exited unsuccessfully ({code})"),
            },
            findings,
        )
    } else {
        VerifierOutcome::with_findings(verifier, None, VerifierStatus::Clean, findings)
    }
}

#[cfg(not(feature = "host"))]
async fn run_pyright_unit(
    _executable: PathBuf,
    _workspace_root: PathBuf,
    _paths: Vec<PathBuf>,
) -> VerifierOutcome {
    VerifierOutcome::unavailable(
        VerifierId::command(PYRIGHT_ID),
        None,
        "command verifiers require the host feature; process launch is unavailable in the portable profile".into(),
    )
}

/// A task whose unit cannot produce a verdict — the process is not
/// provisioned or the interpreter could not be resolved. Carried as a real
/// task (not a synthetic post-hoc outcome) so the budget machinery, the
/// deterministic ordering, and the requirement/coverage stamping all apply
/// to it exactly like a unit that ran.
fn unavailable_task(
    verifier: VerifierId,
    subject: Option<PathBuf>,
    requirement: VerifierRequirement,
    covered_paths: Vec<PathBuf>,
    budget: Duration,
    reason: String,
) -> VerifierTask {
    VerifierTask {
        verifier: verifier.clone(),
        subject,
        requirement,
        covered_paths,
        budget,
        run: Box::pin(async move { VerifierOutcome::unavailable(verifier, None, reason) }),
    }
}

// ─── Unit assembly ──────────────────────────────────────────────────────────

/// Assemble the verifier units for one fan-out run, in deterministic
/// submission order: structured-config parses (cheapest, exhaustive over
/// config paths), then per-file LSP diagnostics, then the Python legs
/// (required syntax, optional provisioned typecheck), then command
/// verifiers. A command verifier with no applicable path does not become a
/// unit; a command verifier whose program is not provisioned becomes an
/// explicit `Unavailable` unit (its requirement is honored) rather than a
/// spawn that can only fail.
pub(crate) fn build_units(
    paths: &[PathBuf],
    tool_ctx: &super::FanoutToolContext,
    specs: &[CommandVerifierSpec],
    profile: &super::profile::VerificationProfile,
    budgets: &VerifyBudgets,
) -> Vec<VerifierTask> {
    let mut tasks: Vec<VerifierTask> = Vec::new();
    for path in paths {
        if is_structured_config_ext(path) {
            let path = path.clone();
            let root = tool_ctx.workspace_root.clone();
            tasks.push(VerifierTask {
                verifier: VerifierId::config(),
                subject: Some(path.clone()),
                requirement: VerifierRequirement::Required,
                covered_paths: vec![path.clone()],
                budget: budgets.per_verifier,
                run: Box::pin(async move { run_config_unit(&root, &path) }),
            });
        }
    }
    for path in paths {
        if is_code_ext(path) {
            let path = path.clone();
            let tool_ctx = tool_ctx.clone();
            let per_file = budgets.per_verifier_file;
            let requirement = if profile.lsp_required {
                VerifierRequirement::Required
            } else {
                VerifierRequirement::Optional
            };
            tasks.push(VerifierTask {
                verifier: VerifierId::lsp(),
                subject: Some(path.clone()),
                requirement,
                covered_paths: vec![path.clone()],
                budget: budgets.per_verifier,
                run: Box::pin(async move {
                    run_lsp_unit(path, tool_ctx.tool_context(), per_file).await
                }),
            });
        }
    }
    let trusted = tool_ctx.workspace_trust == rightkit_agent_policy::WorkspaceTrust::Trusted;
    let mut python_groups = std::collections::BTreeMap::new();
    for file in &profile.python_files {
        python_groups
            .entry((file.project_root.clone(), file.interpreter.clone()))
            .or_insert_with(Vec::new)
            .push(file.path.clone());
    }
    for ((project_root, interpreter), covered) in python_groups {
        if !trusted {
            tasks.push(unavailable_task(
                VerifierId::command(PYTHON_SYNTAX_ID),
                None,
                VerifierRequirement::Required,
                covered,
                budgets.per_verifier,
                "project interpreter execution requires a trusted workspace".into(),
            ));
            continue;
        }
        match &interpreter {
            Some(interpreter) => {
                let spec = python_syntax_spec(interpreter);
                let root = tool_ctx.workspace_root.clone();
                let unit_paths = covered.clone();
                tasks.push(VerifierTask {
                    verifier: VerifierId::command(PYTHON_SYNTAX_ID),
                    subject: None,
                    requirement: VerifierRequirement::Required,
                    covered_paths: covered,
                    budget: budgets.per_verifier,
                    run: Box::pin(
                        async move { run_command_unit(spec, root, unit_paths).await },
                    ),
                });
            }
            None => tasks.push(unavailable_task(
                VerifierId::command(PYTHON_SYNTAX_ID),
                None,
                VerifierRequirement::Required,
                covered,
                budgets.per_verifier,
                format!(
                    "no Python interpreter resolved for project root {}; checked .venv/Scripts/python.exe, venv equivalents, and the provisioned PATH (python/py)",
                    project_root.display()
                ),
            )),
        }
    }
    let py_paths: Vec<PathBuf> = profile
        .python_files
        .iter()
        .map(|file| file.path.clone())
        .collect();
    if !py_paths.is_empty() {
        match &profile.pyright {
            Some(_) if !trusted => tasks.push(unavailable_task(
                VerifierId::command(PYRIGHT_ID),
                None,
                VerifierRequirement::Optional,
                py_paths,
                budgets.per_verifier,
                "project typechecker execution requires a trusted workspace".into(),
            )),
            Some(executable) => {
                let executable = executable.clone();
                let root = tool_ctx.workspace_root.clone();
                let unit_paths = py_paths.clone();
                tasks.push(VerifierTask {
                    verifier: VerifierId::command(PYRIGHT_ID),
                    subject: None,
                    requirement: VerifierRequirement::Optional,
                    covered_paths: py_paths,
                    budget: budgets.per_verifier,
                    run: Box::pin(
                        async move { run_pyright_unit(executable, root, unit_paths).await },
                    ),
                });
            }
            None => tasks.push(unavailable_task(
                VerifierId::command(PYRIGHT_ID),
                None,
                VerifierRequirement::Optional,
                py_paths,
                budgets.per_verifier,
                "optional Python typechecker 'pyright' is not provisioned on the search path; \
                 no typecheck verdict is claimed"
                    .into(),
            )),
        }
    }
    for spec in specs {
        let applicable: Vec<PathBuf> = paths
            .iter()
            .filter(|p| spec.applies_to_in(&tool_ctx.workspace_root, p))
            .cloned()
            .collect();
        if applicable.is_empty() {
            continue;
        }
        let requirement = if spec.required {
            VerifierRequirement::Required
        } else {
            VerifierRequirement::Optional
        };
        if !trusted {
            tasks.push(unavailable_task(
                VerifierId::command(&spec.id),
                None,
                requirement,
                applicable,
                budgets.per_verifier,
                "project verifier commands require a trusted workspace".into(),
            ));
            continue;
        }
        let program = spec.argv.first().map(String::as_str).unwrap_or("");
        if super::profile::resolve_program(program, &tool_ctx.workspace_root).is_none() {
            tasks.push(unavailable_task(
                VerifierId::command(&spec.id),
                None,
                requirement,
                applicable,
                budgets.per_verifier,
                format!(
                    "verifier '{}' program '{}' is not provisioned (checked the workspace and the search path); it was not run",
                    spec.id, program
                ),
            ));
            continue;
        }
        let spec = spec.clone();
        let root = tool_ctx.workspace_root.clone();
        tasks.push(VerifierTask {
            verifier: VerifierId::command(&spec.id),
            subject: None,
            requirement,
            covered_paths: applicable.clone(),
            budget: budgets.per_verifier,
            run: Box::pin(async move { run_command_unit(spec, root, applicable).await }),
        });
    }
    tasks.sort_by_key(|task| !task.requirement.is_required());
    tasks
}
