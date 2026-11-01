//! Bounded skill install pipeline (ENG-105).
//!
//! Installing a skill pack means taking raw markdown fetched from a
//! provenance-checked source (see [`crate::normalize_skill_pack_github_url`]
//! for URL validation) and turning it into a manifest the host app will load
//! from disk. This module owns the acceptance gate between "bytes we fetched"
//! and "manifest the runtime will discover": every check in
//! [`validate_for_install`] must pass before a caller is allowed to write the
//! skill into a skill root, and a failure returns a specific
//! [`InstallValidationError`] variant rather than a generic string so a UI
//! (or a test) can react to *why* installation was refused.
//!
//! This module does no I/O and makes no network calls — the fetch step lives
//! with the caller (the daemon's HTTP client, gated behind the workspace's
//! usual outbound-network policy). That keeps validation pure, deterministic,
//! and testable without a live GitHub endpoint.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::manifest::{SkillError, SkillManifest};

/// Hard ceiling on installed skill markdown size. A `SKILL.md` is
/// instructions, not a data dump; this bound catches both malformed installs
/// (e.g. someone pasted a whole repo dump) and a class of resource-exhaustion
/// attempts from a hostile skill-pack source.
pub const MAX_SKILL_SOURCE_BYTES: usize = 256 * 1024;

/// Reasons an install candidate can be refused. Distinct from [`SkillError`]
/// (which covers parse/frontmatter/trigger-compile failures) so an install
/// caller can distinguish "this isn't a valid skill at all" from
/// "this is a valid skill but fails our install-specific policy."
#[derive(Debug, Error)]
pub enum InstallValidationError {
    /// The manifest itself failed to parse or its trigger failed to compile.
    #[error(transparent)]
    Manifest(#[from] SkillError),
    /// The raw source text exceeded [`MAX_SKILL_SOURCE_BYTES`].
    #[error("skill source is {actual} bytes, exceeding the {limit}-byte install limit")]
    TooLarge { actual: usize, limit: usize },
    /// The manifest's `name` is empty, or contains characters that would be
    /// unsafe to use as a filesystem path component (path separators, `..`,
    /// NUL, or leading `.`).
    #[error("skill name '{0}' is not a safe install slug")]
    UnsafeName(String),
    /// The manifest declared a required environment variable with an empty
    /// or otherwise malformed name.
    #[error("skill declares an invalid required environment variable: '{0}'")]
    InvalidRequiredEnvVar(String),
    /// A behavioral skill must declare a non-empty trigger; without one it
    /// can never fire and installing it would be silently inert.
    #[error("behavioral skill '{0}' has no trigger expression and would never fire")]
    BehavioralSkillMissingTrigger(String),
    /// The destination path the caller intends to write to escapes the
    /// caller-supplied install root (would only happen if the caller passed
    /// an already-malicious `install_root`/name combination; checked
    /// defensively since this is the last gate before a filesystem write).
    #[error("resolved install path escapes the skill root")]
    PathEscapesRoot,
}

/// A skill manifest that has passed every install-time check and is safe to
/// write to disk. Carries the normalized destination path a caller should
/// write `source` to, so the "where do the bytes go" decision is made once,
/// here, rather than re-derived ad hoc at each call site.
#[derive(Debug, Clone)]
pub struct InstallableSkill {
    pub manifest: SkillManifest,
    pub destination: PathBuf,
}

/// Validate raw skill-pack markdown for installation into `install_root`.
///
/// Runs, in order:
/// 1. Size bound ([`MAX_SKILL_SOURCE_BYTES`]).
/// 2. Manifest parse (`SkillManifest::from_markdown`) — auto-detects
///    Anthropic `SKILL.md` (YAML) vs. the host app native (TOML) frontmatter.
/// 3. Manifest validation (`SkillManifest::validate`) — compiles the trigger
///    DSL expression, if present.
/// 4. Install-specific policy: safe name, valid required-env-var names, and
///    (for behavioral skills, which compose into the instruction layer purely
///    by triggering) a non-empty trigger.
/// 5. Destination path containment: the resolved `install_root/<name>.md`
///    (or `install_root/<name>/SKILL.md` when `as_skill_md` is set) must stay
///    under `install_root`.
///
/// Returns the parsed manifest plus its intended destination path on success;
/// the first failing check on error. Nothing is written to disk here.
pub fn validate_for_install(
    source: &str,
    install_root: &Path,
    as_skill_md: bool,
) -> Result<InstallableSkill, InstallValidationError> {
    if source.len() > MAX_SKILL_SOURCE_BYTES {
        return Err(InstallValidationError::TooLarge {
            actual: source.len(),
            limit: MAX_SKILL_SOURCE_BYTES,
        });
    }

    let manifest = SkillManifest::from_markdown(source)?;
    manifest.validate()?;

    validate_safe_slug(&manifest.name)
        .then_some(())
        .ok_or_else(|| InstallValidationError::UnsafeName(manifest.name.clone()))?;

    for var in &manifest.required_environment_variables {
        if !is_valid_env_var_name(var) {
            return Err(InstallValidationError::InvalidRequiredEnvVar(var.clone()));
        }
    }

    if manifest.category == crate::manifest::SkillCategory::Behavioral
        && manifest.trigger.trim().is_empty()
    {
        return Err(InstallValidationError::BehavioralSkillMissingTrigger(
            manifest.name.clone(),
        ));
    }

    let destination = if as_skill_md {
        install_root.join(&manifest.name).join("SKILL.md")
    } else {
        install_root.join(format!("{}.md", manifest.name))
    };

    let canonical_root_prefix = install_root.to_path_buf();
    if !destination.starts_with(&canonical_root_prefix) {
        return Err(InstallValidationError::PathEscapesRoot);
    }

    Ok(InstallableSkill {
        manifest,
        destination,
    })
}

/// A safe install slug: non-empty, ASCII alphanumeric plus `-`/`_`, no
/// leading `.`, no path separators, no `..`. Deliberately conservative —
/// this becomes a filesystem path component.
fn validate_safe_slug(name: &str) -> bool {
    if name.is_empty() || name == "." || name == ".." {
        return false;
    }
    if name.starts_with('.') {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A conservative POSIX-ish env var name check: non-empty, starts with a
/// letter or underscore, and contains only alphanumerics/underscore.
fn is_valid_env_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn root() -> PathBuf {
        PathBuf::from("/skills/user")
    }

    const VALID_SKILL_MD: &str =
        "---\nname: rust-expert\ndescription: helps with Rust code\n---\n\nBody text.\n";

    #[test]
    fn accepts_valid_skill_md() {
        let result = validate_for_install(VALID_SKILL_MD, &root(), false).unwrap();
        assert_eq!(result.manifest.name, "rust-expert");
        assert_eq!(result.destination, root().join("rust-expert.md"));
    }

    #[test]
    fn accepts_valid_skill_md_as_skill_md_layout() {
        let result = validate_for_install(VALID_SKILL_MD, &root(), true).unwrap();
        assert_eq!(
            result.destination,
            root().join("rust-expert").join("SKILL.md")
        );
    }

    #[test]
    fn rejects_oversized_source() {
        let huge = "x".repeat(MAX_SKILL_SOURCE_BYTES + 1);
        let err = validate_for_install(&huge, &root(), false).unwrap_err();
        assert!(matches!(err, InstallValidationError::TooLarge { .. }));
    }

    #[test]
    fn rejects_unparseable_manifest() {
        let err = validate_for_install("no frontmatter here", &root(), false).unwrap_err();
        assert!(matches!(err, InstallValidationError::Manifest(_)));
    }

    #[test]
    fn rejects_missing_required_fields() {
        let missing_description = "---\nname: broken\n---\nbody\n";
        let err = validate_for_install(missing_description, &root(), false).unwrap_err();
        assert!(matches!(err, InstallValidationError::Manifest(_)));
    }

    #[test]
    fn rejects_path_traversal_name() {
        let malicious = "---\nname: ../../etc/passwd\ndescription: evil\n---\nbody\n";
        let err = validate_for_install(malicious, &root(), false).unwrap_err();
        assert!(matches!(err, InstallValidationError::UnsafeName(_)));
    }

    #[test]
    fn rejects_slash_in_name() {
        let malicious = "---\nname: foo/bar\ndescription: evil\n---\nbody\n";
        let err = validate_for_install(malicious, &root(), false).unwrap_err();
        assert!(matches!(err, InstallValidationError::UnsafeName(_)));
    }

    #[test]
    fn rejects_leading_dot_name() {
        let malicious = "---\nname: .hidden\ndescription: evil\n---\nbody\n";
        let err = validate_for_install(malicious, &root(), false).unwrap_err();
        assert!(matches!(err, InstallValidationError::UnsafeName(_)));
    }

    #[test]
    fn rejects_invalid_required_env_var() {
        let bad_env = "+++\nname = \"foo\"\ndescription = \"d\"\nrequired_environment_variables = [\"1BAD\"]\n+++\nbody\n";
        let err = validate_for_install(bad_env, &root(), false).unwrap_err();
        assert!(matches!(
            err,
            InstallValidationError::InvalidRequiredEnvVar(_)
        ));
    }

    #[test]
    fn accepts_valid_required_env_var() {
        let good_env = "+++\nname = \"foo\"\ndescription = \"d\"\nrequired_environment_variables = [\"OPENAI_API_KEY\"]\n+++\nbody\n";
        let result = validate_for_install(good_env, &root(), false).unwrap();
        assert_eq!(
            result.manifest.required_environment_variables,
            vec!["OPENAI_API_KEY".to_string()]
        );
    }

    #[test]
    fn rejects_behavioral_skill_without_trigger() {
        let behavioral_no_trigger =
            "+++\nname = \"foo\"\ndescription = \"d\"\ncategory = \"behavioral\"\n+++\nbody\n";
        let err = validate_for_install(behavioral_no_trigger, &root(), false).unwrap_err();
        assert!(matches!(
            err,
            InstallValidationError::BehavioralSkillMissingTrigger(_)
        ));
    }

    #[test]
    fn rejects_invalid_trigger_expression() {
        let bad_trigger =
            "+++\nname = \"foo\"\ndescription = \"d\"\ntrigger = \"(((\"\n+++\nbody\n";
        let err = validate_for_install(bad_trigger, &root(), false).unwrap_err();
        assert!(matches!(err, InstallValidationError::Manifest(_)));
    }
}
