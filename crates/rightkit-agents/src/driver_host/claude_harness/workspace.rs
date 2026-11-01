//! Workspace/trust boundary for the Claude harness (PL-005).
//!
//! The Claude Code CLI is a different trust domain from the the host app
//! daemon: it must run inside one explicitly granted workspace root,
//! under a managed vendor profile, with the daemon's ambient secrets
//! stripped. The donor for these invariants is the host app's own
//! CLI-runtime lineage:
//!
//! - `cli_runtime::transports::process::managed_home` — a sandboxed
//!   vendor launch requires an absolute managed home and *never*
//!   falls back to the ambient `HOME` (donor test
//!   `managed_home_never_falls_back_to_ambient_home`).
//! - `crate::driver_host::DENIED_VENDOR_SUBPROCESS_ENV_VARS` +
//!   `cli_runtime::auth::FORBIDDEN_BYOK_ENV_VARS` — the two env-denial
//!   lists every vendor launch must strip.
//! - `cli_runtime::profile` — the managed profile layout
//!   (`~/<state-dir>/cli-profiles/<provider>/<label>/`).
//!
//! The boundary is lexical (component-wise prefix match). Canonical
//! path resolution / symlink confinement is daemon-side work layered
//! on top of this verdict, exactly like the sandbox layer in
//! `transports::process`, which grants read/write on the *declared*
//! cwd.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::driver_host::cli_runtime::auth::FORBIDDEN_BYOK_ENV_VARS;
use crate::driver_host::DENIED_VENDOR_SUBPROCESS_ENV_VARS;

use super::HarnessError;

/// The recorded trust verdict for a harness binding. Persisted on the
/// binding record so a restart can prove the workspace grant the
/// binding was created under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum WorkspaceTrustVerdict {
    Trusted {
        root: PathBuf,
        granted_at_unix_ms: u64,
    },
    Untrusted {
        reason: String,
    },
}

/// The lexical trust boundary of one harness runtime. Every launch
/// and every session cwd must resolve inside the granted root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustBoundary {
    root: PathBuf,
}

impl TrustBoundary {
    /// Grant a workspace root. Fail-closed: the root must be
    /// non-empty, absolute, and must not traverse upwards (`..`).
    pub fn grant(root: &Path, granted_at_unix_ms: u64) -> Result<Self, HarnessError> {
        if root.as_os_str().is_empty() {
            return Err(HarnessError::Trust(
                "workspace root must not be empty".to_string(),
            ));
        }
        if !root.is_absolute() {
            return Err(HarnessError::Trust(format!(
                "workspace root must be absolute: {}",
                root.display()
            )));
        }
        if traverses_parent(root) {
            return Err(HarnessError::Trust(format!(
                "workspace root must not contain '..' components: {}",
                root.display()
            )));
        }
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    /// The granted workspace root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Project the current verdict for persistence.
    pub fn verdict(&self, granted_at_unix_ms: u64) -> WorkspaceTrustVerdict {
        WorkspaceTrustVerdict::Trusted {
            root: self.root.clone(),
            granted_at_unix_ms,
        }
    }

    /// Admit a session cwd. Fail-closed: the cwd must be absolute,
    /// must not traverse upwards, and must be inside (or exactly at)
    /// the granted root. A cwd that is an *ancestor* of the root is
    /// refused because it escapes the grant.
    pub fn admit_cwd(&self, cwd: &Path) -> Result<(), HarnessError> {
        if cwd.as_os_str().is_empty() {
            return Err(HarnessError::Trust(
                "session cwd must not be empty".to_string(),
            ));
        }
        if !cwd.is_absolute() {
            return Err(HarnessError::Trust(format!(
                "session cwd must be absolute: {}",
                cwd.display()
            )));
        }
        if traverses_parent(cwd) {
            return Err(HarnessError::Trust(format!(
                "session cwd must not contain '..' components: {}",
                cwd.display()
            )));
        }
        if !path_starts_with(cwd, &self.root) {
            return Err(HarnessError::Trust(format!(
                "session cwd {} is outside the trusted workspace root {}",
                cwd.display(),
                self.root.display()
            )));
        }
        Ok(())
    }
}

fn traverses_parent(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir))
}

/// Lexical prefix containment by path components. `child` counts as
/// inside `root` when every root component appears in order at the
/// start of `child` and `child` is not shorter than `root`.
fn path_starts_with(child: &Path, root: &Path) -> bool {
    let mut child_components = child.components();
    for root_component in root.components() {
        match child_components.next() {
            Some(child_component) if child_component == root_component => continue,
            _ => return false,
        }
    }
    true
}

/// The single env-denial set every Claude harness launch must strip.
///
/// This is the union of the crate's two denial lists — the BYOK list
/// (`FORBIDDEN_BYOK_ENV_VARS`, which the legacy launch specs populate
/// `env_remove` from) and the daemon-secret list
/// (`DENIED_VENDOR_SUBPROCESS_ENV_VARS`). `lib.rs` already asserts the
/// two drivers cannot drift apart; the harness asserts the *effective*
/// launch spec strips the union so a future split cannot weaken one
/// side silently.
pub fn harness_denied_env_vars() -> BTreeSet<String> {
    FORBIDDEN_BYOK_ENV_VARS
        .iter()
        .chain(DENIED_VENDOR_SUBPROCESS_ENV_VARS.iter())
        .map(|name| (*name).to_string())
        .collect()
}

/// Compute the managed Claude vendor home for one profile label under
/// a daemon-provided managed profile root.
///
/// Layout mirrors `cli_runtime::profile`:
/// `<managed_profile_root>/claude/<label>/`. The function composes
/// only from its inputs — it never reads the ambient `HOME`, so a
/// harness launch can never inherit a host-global `~/.claude`.
pub fn managed_claude_home(
    managed_profile_root: &Path,
    profile_label: &str,
) -> Result<PathBuf, HarnessError> {
    if !managed_profile_root.is_absolute() {
        return Err(HarnessError::Trust(format!(
            "managed profile root must be absolute: {}",
            managed_profile_root.display()
        )));
    }
    validate_path_component(profile_label)?;
    Ok(managed_profile_root.join("claude").join(profile_label))
}

/// Validate one filesystem path component for use as a profile label
/// or store filename. Rejects empty, too-long, path separators, and
/// dot-prefixed traversal tricks.
pub(crate) fn validate_path_component(component: &str) -> Result<(), HarnessError> {
    if component.is_empty() {
        return Err(HarnessError::Trust(
            "path component must not be empty".to_string(),
        ));
    }
    if component.len() > 64 {
        return Err(HarnessError::Trust(
            "path component must be at most 64 characters".to_string(),
        ));
    }
    if component == "." || component == ".." || component.starts_with('.') {
        return Err(HarnessError::Trust(format!(
            "path component must not start with '.': {component}"
        )));
    }
    if !component
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Err(HarnessError::Trust(format!(
            "path component contains disallowed characters: {component}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // The trust boundary is purely lexical and never touches the
    // filesystem, so each test simply holds the `TempDir` guard for
    // its own body and uses `dir.path()` as a plausible absolute root.

    #[test]
    fn trust_boundary_rejects_relative_and_outside_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let boundary = TrustBoundary::grant(&root, 0).unwrap();

        let relative = PathBuf::from("relative/path");
        let error = boundary.admit_cwd(&relative).unwrap_err();
        assert!(error.to_string().contains("absolute"));

        let outside = root.parent().unwrap_or(Path::new("/")).to_path_buf();
        if outside != root {
            let error = boundary.admit_cwd(&outside).unwrap_err();
            assert!(error
                .to_string()
                .contains("outside the trusted workspace root"));
        }

        // The root itself and nested paths are admitted.
        boundary.admit_cwd(&root).unwrap();
        boundary.admit_cwd(&root.join("project")).unwrap();
    }

    #[test]
    fn trust_boundary_rejects_parent_traversal_and_empty_roots() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        assert!(TrustBoundary::grant(Path::new(""), 0).is_err());
        assert!(TrustBoundary::grant(Path::new("relative"), 0).is_err());

        let boundary = TrustBoundary::grant(&root, 0).unwrap();
        let escape = root.join("..").join("elsewhere");
        let error = boundary.admit_cwd(&escape).unwrap_err();
        assert!(error.to_string().contains("'..'"));
    }

    #[test]
    fn trust_boundary_rejects_ancestor_of_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let boundary = TrustBoundary::grant(&root, 0).unwrap();
        let ancestor = root
            .ancestors()
            .nth(1)
            .expect("temp roots always have a parent")
            .to_path_buf();
        let error = boundary.admit_cwd(&ancestor).unwrap_err();
        assert!(error
            .to_string()
            .contains("outside the trusted workspace root"));
    }

    #[test]
    fn trust_verdict_records_root_and_grant_time() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let boundary = TrustBoundary::grant(&root, 1234).unwrap();
        match boundary.verdict(1234) {
            WorkspaceTrustVerdict::Trusted {
                root: recorded,
                granted_at_unix_ms,
            } => {
                assert_eq!(recorded, root);
                assert_eq!(granted_at_unix_ms, 1234);
            }
            other => panic!("expected Trusted verdict, got {other:?}"),
        }
    }

    /// Donor shape (`managed_home_never_falls_back_to_ambient_home`):
    /// the managed home is composed only from the supplied root and
    /// label and is scoped under the claude profile directory.
    #[test]
    fn managed_home_is_scoped_under_the_host_profile_root_never_ambient_home() {
        let dir = tempfile::tempdir().unwrap();
        let profile_root = dir.path().to_path_buf();
        let home = managed_claude_home(&profile_root, "default").unwrap();
        assert_eq!(home, profile_root.join("claude").join("default"));
        // The home is composed only from the supplied root: it is
        // inside the managed root, never at an ambient location.
        assert!(home.starts_with(&profile_root));

        assert!(managed_claude_home(Path::new("relative-root"), "default").is_err());
        for hostile in ["", ".", "..", ".hidden", "with/slash", "with space"] {
            assert!(
                managed_claude_home(&profile_root, hostile).is_err(),
                "label {hostile:?} must be rejected"
            );
        }
    }

    /// The harness env denial set is the union of both crate denial
    /// lists and covers the provider keys plus the daemon bootstrap
    /// token.
    #[test]
    fn harness_env_policy_superset_covers_both_denied_env_sets() {
        let denied = harness_denied_env_vars();
        for name in FORBIDDEN_BYOK_ENV_VARS {
            assert!(denied.contains(*name), "union must contain {name}");
        }
        for name in DENIED_VENDOR_SUBPROCESS_ENV_VARS {
            assert!(denied.contains(*name), "union must contain {name}");
        }
        for name in [
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "RIGHTKIT_DAEMON_TOKEN",
            "RIGHTKIT_GATEWAY_TOKEN",
        ] {
            assert!(denied.contains(name), "union must contain {name}");
        }
    }
}
