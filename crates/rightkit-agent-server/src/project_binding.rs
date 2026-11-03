//! CG-A06 — durable project/cwd binding with trust-aware session navigation.
//!
//! RightKit sessions are durably bound to a `workspace_root` at creation
//! time (see `api_session_bindings.rs::session_workspace_root`,
//! `main_runtime_daemon_sessions.rs::same_cli_path`, and
//! `api_session_utils_tasks.rs::same_path`) but that binding logic is
//! scattered across CLI resume/fork path matching, the HTTP session-binding
//! row builder, and spec-completion workspace checks — each with its own
//! ad-hoc path-normalization routine and no single trust-aware navigation
//! type. This module consolidates that mechanism into one `ProjectBinding`
//! that:
//!
//!   - durably records the canonical root a session is bound to;
//!   - resolves/canonicalizes an incoming cwd against platform path rules
//!     (case-insensitive, separator-normalized — matching the existing
//!     `same_cli_path` behavior so migration is a drop-in);
//!   - classifies navigation attempts as `Inside`, `Outside`, or
//!     `Unresolvable`, and pairs that with the existing
//!     [`rightkit_agent_engine_protocol::trust::ContentTrust`] vocabulary so a caller can
//!     decide whether content flowing from a given cwd should be treated as
//!     `WorkspaceTrusted` or something lower;
//!   - is a plain, sync, dependency-light type (no daemon state, no I/O
//!     beyond `std::fs::canonicalize`) so it can be reused identically by the
//!     HTTP session layer, the CLI resume/fork commands, and the ACP bridge's
//!     `session/new` / `session/load` / `session/resume` cwd handling.
//!
//! The daemon session-admission seam binds the incoming workspace before it
//! resolves persisted trust, so unresolvable roots fail closed rather than
//! inheriting a trust grant through path spelling differences.

use rightkit_agent_engine_protocol::trust::ContentTrust;
use std::path::{Path, PathBuf};

/// Outcome of checking a candidate path against a bound project root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigationOutcome {
    /// The candidate path canonicalizes to a location inside (or equal to)
    /// the bound root.
    Inside,
    /// The candidate path canonicalizes successfully but resolves outside
    /// the bound root — a trust-boundary crossing.
    Outside,
    /// The candidate path could not be canonicalized (does not exist, is
    /// not accessible, or is malformed). Callers should treat this the same
    /// as `Outside` for any effect-granting decision — fail closed.
    Unresolvable,
}

impl NavigationOutcome {
    /// Whether this outcome permits treating navigation as staying inside
    /// the trusted project boundary. Only `Inside` is trusted; both
    /// `Outside` and `Unresolvable` fail closed.
    pub fn is_trusted(self) -> bool {
        matches!(self, NavigationOutcome::Inside)
    }
}

/// A durable binding between a session (or ACP session/CLI invocation) and
/// its project root, plus the logic to evaluate further navigation against
/// that root.
///
/// `ProjectBinding` itself holds no session id — it is the pure
/// path-trust primitive. Callers (HTTP session layer, ACP bridge, CLI
/// resume/fork) attach it to whatever session-identifying context they
/// already carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectBinding {
    /// The raw root string exactly as it was bound (e.g. as stored in the
    /// session row's `workspaceRoot` field). Preserved verbatim for display
    /// and for round-tripping into storage.
    raw_root: String,
    /// Best-effort canonicalized form of `raw_root`, computed once at bind
    /// time. `None` when the root does not exist / cannot be resolved at
    /// bind time (still a legal binding — canonicalization failures do not
    /// block binding, only later navigation checks against them).
    canonical_root: Option<PathBuf>,
}

impl ProjectBinding {
    /// Binds a session to `root`. Canonicalizes eagerly so repeated
    /// navigation checks don't re-touch the filesystem for the root side of
    /// the comparison.
    pub fn bind(root: impl AsRef<str>) -> Self {
        let raw_root = root.as_ref().to_string();
        let canonical_root = canonicalize_best_effort(&raw_root);
        Self {
            raw_root,
            canonical_root,
        }
    }

    /// The root exactly as bound.
    pub fn raw_root(&self) -> &str {
        &self.raw_root
    }

    /// The canonicalized root, if resolvable.
    pub fn canonical_root(&self) -> Option<&Path> {
        self.canonical_root.as_deref()
    }

    /// Evaluates whether `candidate` (a cwd a client is asking to navigate
    /// to, or that a resume/fork/ACP `session/new` call supplied) stays
    /// inside this binding's root.
    ///
    /// Equality is intentionally lenient in the same way the existing
    /// `same_cli_path`/`same_path` helpers were: canonicalized paths are
    /// compared case-insensitively with separators normalized, because
    /// Windows paths in particular vary in case and separator style across
    /// callers (shell cwd vs. stored `workspaceRoot` vs. ACP client cwd).
    pub fn evaluate(&self, candidate: impl AsRef<str>) -> NavigationOutcome {
        let candidate_raw = candidate.as_ref();
        if candidate_raw.trim().is_empty() {
            return NavigationOutcome::Unresolvable;
        }
        let Some(candidate_canonical) = canonicalize_best_effort(candidate_raw) else {
            return NavigationOutcome::Unresolvable;
        };
        let Some(root_canonical) = &self.canonical_root else {
            return NavigationOutcome::Unresolvable;
        };
        if path_equal_or_within(root_canonical, &candidate_canonical) {
            NavigationOutcome::Inside
        } else {
            NavigationOutcome::Outside
        }
    }

    /// Whether `candidate` is the exact bound root (not merely inside it).
    /// Mirrors the strict-equality behavior CLI resume/fork use today via
    /// `same_cli_path` to filter "sessions in this exact cwd".
    pub fn is_exact_root(&self, candidate: impl AsRef<str>) -> bool {
        let candidate_raw = candidate.as_ref();
        if candidate_raw.trim().is_empty() {
            return false;
        }
        match (
            &self.canonical_root,
            canonicalize_best_effort(candidate_raw),
        ) {
            (Some(root), Some(candidate)) => {
                normalize_for_compare(root) == normalize_for_compare(&candidate)
            }
            _ => normalize_raw(&self.raw_root) == normalize_raw(candidate_raw),
        }
    }

    /// Maps a navigation outcome onto the trust vocabulary content flowing
    /// from that location should carry. Content originating from inside the
    /// bound root is workspace-trusted; anything else is deliberately
    /// downgraded rather than silently trusted, matching RightKit's
    /// fail-closed posture on unresolved/outside navigation.
    pub fn trust_for(&self, candidate: impl AsRef<str>) -> ContentTrust {
        match self.evaluate(candidate) {
            NavigationOutcome::Inside => ContentTrust::WorkspaceTrusted,
            NavigationOutcome::Outside | NavigationOutcome::Unresolvable => {
                ContentTrust::ToolUntrusted
            }
        }
    }
}

fn canonicalize_best_effort(raw: &str) -> Option<PathBuf> {
    let trimmed = raw.trim().trim_end_matches(['\\', '/']);
    if trimmed.is_empty() {
        return None;
    }
    std::fs::canonicalize(trimmed).ok()
}

fn normalize_for_compare(path: &Path) -> String {
    path.display()
        .to_string()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

fn normalize_raw(raw: &str) -> String {
    raw.trim()
        .trim_end_matches(['\\', '/'])
        .replace('\\', "/")
        .to_ascii_lowercase()
}

/// True when `candidate` equals `root`, or is a filesystem descendant of it.
fn path_equal_or_within(root: &Path, candidate: &Path) -> bool {
    let root_norm = normalize_for_compare(root);
    let candidate_norm = normalize_for_compare(candidate);
    if root_norm == candidate_norm {
        return true;
    }
    let root_with_sep = format!("{root_norm}/");
    candidate_norm.starts_with(&root_with_sep)
}
