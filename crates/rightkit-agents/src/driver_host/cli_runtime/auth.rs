//! Authentication evidence and forbidden-environment filtering.
//!
//! PR 1 of the architecture plan. Adapters call into this module to
//! obtain a normalized [`AuthEvidence`] for a profile and to obtain
//! the launch-time environment-variable strip list for their provider.
//!
//! PR 2 adds [`assert_no_forbidden_env`] — the pre-spawn tripwire
//! that aborts a launch the moment a BYOK env var is detected in the
//! ambient process environment — and promotes [`classify_auth_mode`]
//! to a fallible function that surfaces
//! [`CliAuthMode::ByokOrExternalApi`] as
//! [`CliAuthError::ByokOrExternalApiRejected`] instead of letting the
//! caller quietly accept an unsupported mode.

use std::collections::BTreeSet;

use crate::driver_host::cli_runtime::ids::{AuthEvidence, CliAuthMode};

/// Errors raised by the auth layer. The single variant that matters
/// for launches is [`CliAuthError::ByokOrExternalApiRejected`] — it
/// is the contract that lets a caller refuse a vendor CLI whose only
/// known auth mode is BYOK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliAuthError {
    /// The provider's only known auth mode is BYOK or an external
    /// API. Under this feature, that mode is forbidden — the caller
    /// must refuse to launch and surface the rejection to the user.
    ByokOrExternalApiRejected,
    /// The auth classification received some evidence that did not
    /// match any of the supported modes (for example a malformed
    /// `mode_hint` string from a vendor CLI). Surfaced verbatim so
    /// the caller can investigate.
    InvalidEvidence(String),
    /// Catch-all for unrecognized failures. Mirrors the shape of the
    /// legacy `classify_auth_mode` callers used to handle inline so
    /// the migration is mechanical.
    Unknown(String),
}

impl std::fmt::Display for CliAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CliAuthError::ByokOrExternalApiRejected => {
                write!(f, "BYOK and external-API modes are forbidden by Agent")
            }
            CliAuthError::InvalidEvidence(reason) => write!(f, "invalid auth evidence: {reason}"),
            CliAuthError::Unknown(reason) => write!(f, "auth classification failed: {reason}"),
        }
    }
}

impl std::error::Error for CliAuthError {}

/// The union of environment variables every adapter must strip from
/// a vendor subprocess before launch. This list is the union of:
///
/// - the existing `DENIED_VENDOR_SUBPROCESS_ENV_VARS` declared in
///   `driver_host::lib` (which already covers the the host app daemon
///   bootstrap and gateway tokens), and
/// - the BYOK and provider-API environment variables enumerated in
///   `docs/architecture/CLI-RUNTIME-SCOPE.md`, section 4.
///
/// A scoped lease token (`RIGHTKIT_GATEWAY_TOKEN` for one binding)
/// is deliberately re-added after the strip pass by the common
/// launch layer; this list only governs ambient inheritance.
// This is the host app's own scrub list, passed to the child as `env_remove`
// (rightkit-agents `SessionConfig.extras.env_remove` is caller-supplied).
pub const FORBIDDEN_BYOK_ENV_VARS: &[&str] = &[
    // Anthropic / Claude.
    "ANTHROPIC_API_KEY",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_PROFILE",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "GOOGLE_CLOUD_PROJECT",
    // OpenAI / Codex.
    "OPENAI_API_KEY",
    "OPENAI_BASE_URL",
    "OPENAI_ORG_ID",
    "OPENAI_PROJECT",
    // xAI / Grok.
    "XAI_API_KEY",
    "XAI_BASE_URL",
    // Cursor.
    "CURSOR_API_KEY",
    "CURSOR_API_BASE_URL",
    // OpenRouter and other generic BYOK providers.
    "OPENROUTER_API_KEY",
    "MINIMAX_API_KEY",
    "TINYFISH_API_KEY",
    // the host app's own secrets (kept in lockstep with the legacy
    // list in `crate::driver_host::lib::DENIED_VENDOR_SUBPROCESS_ENV_VARS`).
    "RIGHTKIT_DAEMON_TOKEN",
    "RIGHTKIT_GATEWAY_TOKEN",
    "RIGHTKIT_DRIVER_INSTANCE_SECRET",
];

/// Build a `BTreeSet<String>` of forbidden env-var names for fast
/// containment checks during launch prep.
pub fn forbidden_env_var_set() -> BTreeSet<String> {
    FORBIDDEN_BYOK_ENV_VARS
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

/// Strip every entry in [`FORBIDDEN_BYOK_ENV_VARS`] from a process
/// environment. Returns the filtered environment, leaving the input
/// untouched.
pub fn strip_forbidden_env<I, K, V>(env: I) -> Vec<(String, String)>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<String>,
    V: Into<String>,
{
    let forbidden = forbidden_env_var_set();
    let mut out = Vec::new();
    for (k, v) in env {
        let key = k.into();
        if !forbidden.contains(&key) {
            out.push((key, v.into()));
        }
    }
    out
}

/// Reduce an observed auth source to one of the three enum variants.
/// `NativeCliAccount` is the only launchable outcome under this
/// feature; the other two are tripwires that the registry must
/// surface as `CliProviderEligibility::Unqualified`.
///
/// PR 2 promotes this function to `Result` so a BYOK observation is
/// surfaced as [`CliAuthError::ByokOrExternalApiRejected`] instead of
/// being silently passed back to the caller — the caller is then
/// forced to refuse the launch explicitly. The result type is
/// `Result<CliAuthMode, CliAuthError>` for callers that need to
/// distinguish a forbidden mode from a transient classification
/// failure.
pub fn classify_auth_mode(
    mode_hint: Option<CliAuthMode>,
    byok_flag_present: bool,
    unknown_evidence: bool,
) -> Result<CliAuthMode, CliAuthError> {
    if let Some(mode) = mode_hint {
        if matches!(mode, CliAuthMode::ByokOrExternalApi) {
            return Err(CliAuthError::ByokOrExternalApiRejected);
        }
        return Ok(mode);
    }
    if byok_flag_present {
        return Err(CliAuthError::ByokOrExternalApiRejected);
    }
    if unknown_evidence {
        return Ok(CliAuthMode::Unknown);
    }
    // Without any positive evidence we do **not** assume
    // `NativeCliAccount`. The unknown path forces the registry to
    // prompt the user through a native login.
    Ok(CliAuthMode::Unknown)
}

/// Pre-spawn tripwire: inspect the ambient process environment for
/// any entry in [`FORBIDDEN_BYOK_ENV_VARS`]. Returns `Err` with the
/// offending variable name the moment one is detected so the caller
/// can refuse to spawn the vendor subprocess rather than accidentally
/// re-introduce a BYOK surface.
///
/// This is intentionally a hard pre-check, not a logged warning —
/// the vendor CLI launched with `ANTHROPIC_API_KEY` already in scope
/// would defeat every other defense in this module.
pub fn assert_no_forbidden_env() -> Result<(), CliAuthError> {
    for name in FORBIDDEN_BYOK_ENV_VARS {
        if std::env::var(name).is_ok() {
            return Err(CliAuthError::InvalidEvidence(format!(
                "forbidden BYOK env var present: {name}"
            )));
        }
    }
    Ok(())
}

/// Build a typed `AuthEvidence` from raw inputs. Used by adapters
/// when they have non-secret evidence from initialization events.
pub fn auth_evidence(
    mode: CliAuthMode,
    source_label: impl Into<String>,
    account_fingerprint: Option<String>,
    observed_at: impl Into<String>,
) -> AuthEvidence {
    AuthEvidence {
        mode,
        source_label: source_label.into(),
        account_fingerprint,
        observed_at: observed_at.into(),
    }
}
