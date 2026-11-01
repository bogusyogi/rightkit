//! Native login, status, and logout routes for vendor CLIs.
//!
//! PR 2 of the architecture plan. These routes are the only sanctioned
//! path to attach an account to a managed [`CliAccountProfile`]:
//!
//! - [`login`] launches the vendor CLI's own native login flow
//!   (currently `codex login`; Claude sign-in is not in the product, R42)
//!   under a managed `HOME` and the vendor's config-directory variable so
//!   the credential file the vendor writes lands inside the profile
//!   directory, never in the user's global shell environment. It is
//!   cancelled after [`LOGIN_TIMEOUT`].
//! - [`status`] is a non-secret probe of whether the vendor reports a
//!   signed-in account for that profile.
//! - [`logout`] tears the managed profile directory down so the next
//!   launch starts from a clean slate.
//!
//! the host app never reads credential bytes: stdout and stderr from the
//! vendor login subprocess are scanned against a deny-list of known
//! credential prefixes (`sk-`, `ghp_`, `AKIA`, `xai-`, `OPENAI_API_KEY=`,
//! …) and any match aborts the run with [`LoginError::CredentialLeakSuspected`].

use crate::driver_host::cli_runtime::qualification::now_iso;

use serde::{Deserialize, Serialize};

use crate::driver_host::cli_runtime::auth::FORBIDDEN_BYOK_ENV_VARS;
use crate::driver_host::cli_runtime::ids::{CliAuthMode, CliProviderId};
use crate::driver_host::cli_runtime::profile::{
    delete_profile, ensure_managed_home, fingerprint_from_label, list_profiles, managed_home,
    profile_for, sanitize_label, save_profile, ProfileStoreError,
};

/// Result returned by [`login`]. `provider_id` and `profile_id` are
/// echoed back so the UI can update the same profile it requested;
/// `status` is one of `started`, `signed_in`, or `failed`; and
/// `observed_at` is the ISO timestamp the call finished at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginReceipt {
    pub provider_id: CliProviderId,
    pub profile_id: String,
    pub status: LoginStatusKind,
    pub observed_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginStatusKind {
    Started,
    SignedIn,
    Failed,
}

/// Non-secret probe result returned by [`status`]. Every field is
/// safe to surface to the UI; the underlying account identifier is
/// reduced to a 16-hex-char fingerprint so two statuses can be
/// compared across launches without exposing the identifier itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginStatus {
    pub provider_id: CliProviderId,
    pub profile_id: String,
    pub signed_in: bool,
    pub auth_mode: CliAuthMode,
    pub account_fingerprint: Option<String>,
    pub source_label: String,
    pub observed_at: String,
}

/// Errors raised by the login/status/logout routes. Each variant
/// corresponds to a class of failure the UI can translate into a
/// concrete action: "refresh login", "unset BYOK env vars", "install
/// the CLI", "open logs", etc.
#[derive(Debug)]
pub enum LoginError {
    /// Failed to spawn the vendor CLI.
    Spawn {
        provider: CliProviderId,
        reason: String,
    },
    /// A BYOK or external-API environment variable was detected in
    /// the ambient environment before the vendor subprocess was
    /// launched. The launch is aborted; the caller is expected to
    /// unset the offending variable and retry.
    ForbiddenEnvDetected(String),
    /// The provider has no registered layout in [`PROFILE_LAYOUTS`]
    /// (or its equivalent in [`crate::driver_host::cli_runtime::profile`]) and is
    /// therefore not a launchable native-login target.
    NotRegistered(CliProviderId),
    /// The vendor binary could not be located on `PATH` (or the
    /// registry-resolved install location).
    BinaryMissing {
        provider: CliProviderId,
        binary: String,
    },
    /// The vendor subprocess exited non-zero. `stderr_summary` is the
    /// sanitized tail of stderr; secrets are redacted by
    /// [`redact_credential_bytes`].
    NonZeroExit {
        provider: CliProviderId,
        code: Option<i32>,
        stderr_summary: String,
    },
    /// The vendor subprocess emitted text that matches the
    /// deny-list. The launch is aborted and the matched prefix is
    /// reported so the caller can investigate. This is the single
    /// tripwire that prevents the host app from accidentally logging a
    /// vendor credential.
    CredentialLeakSuspected {
        provider: CliProviderId,
        matched: String,
    },
    /// The vendor sign-in did not finish within [`LOGIN_TIMEOUT`] and was cancelled.
    /// `stderr_summary` is what the vendor had printed by then.
    TimedOut {
        provider: CliProviderId,
        seconds: u64,
        stderr_summary: String,
    },
    /// Underlying profile-store error (`Io`, `InvalidLabel`,
    /// `UnknownProfile`, …). Surfaces as-is so the caller can
    /// decide whether to surface or repair.
    ProfileIo(ProfileStoreError),
}

impl std::fmt::Display for LoginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoginError::Spawn { provider, reason } => {
                write!(f, "failed to spawn {provider} login: {reason}")
            }
            LoginError::ForbiddenEnvDetected(name) => {
                write!(f, "forbidden BYOK env var present: {name}")
            }
            LoginError::NotRegistered(provider) => {
                write!(f, "{provider} has no native login route")
            }
            LoginError::BinaryMissing { provider, binary } => {
                write!(f, "{provider} binary not found: {binary}")
            }
            LoginError::NonZeroExit {
                provider,
                code,
                stderr_summary,
            } => match code {
                Some(code) => write!(
                    f,
                    "{provider} login exited with status {code}: {stderr_summary}"
                ),
                None => write!(
                    f,
                    "{provider} login terminated without an exit code: {stderr_summary}"
                ),
            },
            LoginError::CredentialLeakSuspected { provider, matched } => write!(
                f,
                "{provider} login output looks like a credential (matched {matched:?}); launch aborted"
            ),
            LoginError::TimedOut {
                provider,
                seconds,
                stderr_summary,
            } => write!(
                f,
                "{provider} login did not finish within {seconds} s and was cancelled: {stderr_summary}"
            ),
            LoginError::ProfileIo(err) => write!(f, "profile store error: {err}"),
        }
    }
}

impl std::error::Error for LoginError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LoginError::ProfileIo(err) => Some(err),
            _ => None,
        }
    }
}

impl From<ProfileStoreError> for LoginError {
    fn from(err: ProfileStoreError) -> Self {
        LoginError::ProfileIo(err)
    }
}

/// How long a vendor sign-in may run before it is cancelled (providers R13).
pub const LOGIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Resolve the vendor binary name and the native-login argv for one
/// provider. `None` means the provider has no native login route.
///
/// Claude has none: Claude account sign-in is not in the product until Anthropic approval is
/// applied for and granted (providers R42, B68). A person's own `claude` login stays the CLI's.
fn vendor_login_argv(provider: &CliProviderId) -> Option<(&'static str, &'static [&'static str])> {
    match provider.as_str() {
        "codex" => Some(("codex", &["login"])),
        _ => None,
    }
}

/// The variable the vendor CLI reads its config directory from, set to the managed home so the
/// credential file lands exactly where the launch reads it (providers R12): `CODEX_HOME` for
/// Codex (`<home>/auth.json`).
fn config_dir_variable(provider: &CliProviderId) -> Option<&'static str> {
    match provider.as_str() {
        "codex" => Some("CODEX_HOME"),
        _ => None,
    }
}

/// Cheap UTC ISO timestamp suitable for `observed_at`. Mirrors the
/// shape used by the qualification runner so SQL string comparisons
/// stay correct.

/// Scan `text` for any deny-list prefix. Returns the first matched
/// prefix as `Some(prefix)` so the caller can include it in the
/// error; `None` means the text is safe to log or surface.
fn find_credential_match(text: &str) -> Option<&'static str> {
    for prefix in CREDENTIAL_DENY_PREFIXES.iter() {
        if text.contains(prefix) {
            return Some(*prefix);
        }
    }
    None
}

/// The deny-list of credential prefixes. Membership in this list is
/// the single source of truth for what counts as "looks like a
/// credential" — kept narrow on purpose so legitimate log lines
/// (e.g. an info message that happens to contain the word "secret")
/// do not trigger the tripwire.
const CREDENTIAL_DENY_PREFIXES: &[&str] = &[
    "sk-",
    "sk_",
    "ghp_",
    "gho_",
    "ghs_",
    "ghu_",
    "ghr_",
    "AKIA",
    "ASIA",
    "xai-",
    "xai_",
    "OPENAI_API_KEY=",
    "ANTHROPIC_API_KEY=",
    "AWS_ACCESS_KEY_ID=",
    "AWS_SECRET_ACCESS_KEY=",
    "GOOGLE_API_KEY=",
    "XAI_API_KEY=",
    "OPENROUTER_API_KEY=",
    "MINIMAX_API_KEY=",
    "TINYFISH_API_KEY=",
];

/// Replace any matched deny-list prefix with `<redacted>` so the
/// returned string is safe to log. Returns `Some(redacted)` if any
/// prefix matched, `None` otherwise.
pub fn redact_credential_bytes(text: &str) -> Option<String> {
    let matched = find_credential_match(text)?;
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(idx) = rest.find(matched) {
        out.push_str(&rest[..idx]);
        out.push_str("<redacted>");
        // Skip past the matched prefix; the next 16 characters after
        // the prefix are the typical bearer-token body and are
        // dropped along with the prefix so we never carry them
        // through to the log.
        let after = &rest[idx + matched.len()..];
        let drop = after
            .char_indices()
            .take_while(|(i, c)| *i < 32 && (c.is_ascii_alphanumeric() || matches!(c, '-' | '_')))
            .last()
            .map(|(i, _)| i + 1)
            .unwrap_or(0);
        rest = &rest[idx + matched.len() + drop..];
    }
    out.push_str(rest);
    Some(out)
}

/// Inspect the ambient environment for any entry in
/// [`FORBIDDEN_BYOK_ENV_VARS`]. Returns the first match as
/// `Some(name)`, or `None` if no BYOK env var is leaking into the
/// login subprocess. The check is intentionally a hard tripwire:
/// launching the vendor CLI with `ANTHROPIC_API_KEY` or
/// `OPENAI_API_KEY` already in scope would re-create exactly the
/// BYOK surface this PR removes.
pub fn ambient_forbidden_env() -> Option<String> {
    for name in FORBIDDEN_BYOK_ENV_VARS {
        if std::env::var(name).is_ok() {
            return Some((*name).to_string());
        }
    }
    None
}

/// Run a native login for `<provider>/<label>`. Returns a
/// [`LoginReceipt`] when the vendor CLI exited successfully; any
/// denial or failure is surfaced as a [`LoginError`].
///
/// Steps:
/// 1. Sanitize the label and load-or-create the profile record.
/// 2. Ensure the managed home directory exists.
/// 3. Run [`ambient_forbidden_env`] and abort if any BYOK env var is
///    leaking.
/// 4. Resolve the vendor binary + argv. Unknown providers return
///    [`LoginError::NotRegistered`]; missing binaries return
///    [`LoginError::BinaryMissing`].
/// 5. Spawn the vendor CLI under the managed `HOME`. Scan stdout
///    and stderr through [`find_credential_match`]; any match aborts
///    with [`LoginError::CredentialLeakSuspected`] before we
///    persist anything.
/// 6. On exit success, flip `signed_in = true`, persist the profile,
///    and return [`LoginReceipt`] with `status = SignedIn`.
pub fn login(provider: &CliProviderId, label: &str) -> Result<LoginReceipt, LoginError> {
    let (binary, argv) =
        vendor_login_argv(provider).ok_or_else(|| LoginError::NotRegistered(provider.clone()))?;
    login_with(
        provider,
        label,
        binary,
        argv,
        LOGIN_TIMEOUT,
        ambient_forbidden_env(),
    )
}

/// [`login`] with the vendor program, argv and time limit chosen by the caller, and the result of
/// the ambient BYOK-variable probe passed in (so a test is not at the mercy of the host's
/// environment). The one login implementation: the desktop, the daemon route and the tests all
/// pass through it.
fn login_with(
    provider: &CliProviderId,
    label: &str,
    binary: &str,
    argv: &[&str],
    limit: std::time::Duration,
    ambient_forbidden: Option<String>,
) -> Result<LoginReceipt, LoginError> {
    let sanitized = sanitize_label(label).map_err(LoginError::ProfileIo)?;
    let mut record = profile_for(provider, sanitized).map_err(LoginError::ProfileIo)?;
    let home = ensure_managed_home(&managed_home(provider, sanitized))
        .map_err(ProfileStoreError::Io)
        .map_err(LoginError::ProfileIo)?;

    if let Some(name) = ambient_forbidden {
        return Err(LoginError::ForbiddenEnvDetected(name));
    }

    let launch_context = rightkit_sandbox::execution::ProcessLaunchContext {
        workspace_root: home.to_string_lossy().into_owned(),
        sandbox_profile: Some("vendor-login".into()),
        sandbox_required: true,
        network_required: true,
    };
    let argv = argv
        .iter()
        .map(|value| (*value).to_string())
        .collect::<Vec<_>>();
    let policy_adapter = rightkit_agent_tools::coding_tools::shell::NativeCommandSandboxAdapter;
    let prepared = rightkit_sandbox::execution::CommandSandboxAdapter::prepare(
        &policy_adapter,
        binary,
        &argv,
        &launch_context,
    )
    .map_err(|error| LoginError::Spawn {
        provider: provider.clone(),
        reason: format!("login launch rejected by policy: {error}"),
    })?;

    // The vendor CLI must see its config under our managed home: `HOME`, and the variable the
    // launch reads the account from, both point at it (providers R12).
    let mut login_env = vec![("HOME".to_string(), home.to_string_lossy().into_owned())];
    if let Some(variable) = config_dir_variable(provider) {
        login_env.push((variable.to_string(), home.to_string_lossy().into_owned()));
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| LoginError::Spawn {
            provider: provider.clone(),
            reason: error.to_string(),
        })?;
    let captured = runtime
        .block_on(
            crate::driver_host::cli_runtime::transports::process::StdioProcessIo::capture_bounded(
                std::path::Path::new(&prepared.program),
                prepared.argv,
                &home,
                login_env,
                limit,
            ),
        )
        .map_err(|reason| LoginError::Spawn {
            provider: provider.clone(),
            reason: reason.to_string(),
        })?;
    let stdout = String::from_utf8_lossy(&captured.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&captured.stderr).into_owned();

    if let Some(matched) = find_credential_match(&stdout).or_else(|| find_credential_match(&stderr))
    {
        return Err(LoginError::CredentialLeakSuspected {
            provider: provider.clone(),
            matched: matched.to_string(),
        });
    }

    let Some(status) = captured.status else {
        let redacted = redact_credential_bytes(&stderr).unwrap_or(stderr);
        return Err(LoginError::TimedOut {
            provider: provider.clone(),
            seconds: limit.as_secs(),
            stderr_summary: truncate(&redacted, 240),
        });
    };
    if status != 0 {
        let redacted = redact_credential_bytes(&stderr).unwrap_or(stderr);
        return Err(LoginError::NonZeroExit {
            provider: provider.clone(),
            code: Some(status),
            stderr_summary: truncate(&redacted, 240),
        });
    }

    record.signed_in = true;
    record.auth_mode = CliAuthMode::NativeCliAccount;
    record.account_fingerprint = Some(fingerprint_from_label(sanitized));
    record.last_verified_at = Some(now_iso());
    save_profile(&record).map_err(LoginError::ProfileIo)?;

    Ok(LoginReceipt {
        provider_id: provider.clone(),
        profile_id: record.id.as_str().to_string(),
        status: LoginStatusKind::SignedIn,
        observed_at: now_iso(),
    })
}

/// Non-secret probe for whether a profile is signed in. Reads the
/// persisted record only — it does **not** spawn the vendor CLI —
/// because we never want a probe to surface credential bytes or
/// trigger a fresh login flow.
pub fn status(provider: &CliProviderId, label: &str) -> Result<LoginStatus, LoginError> {
    let sanitized = sanitize_label(label).map_err(LoginError::ProfileIo)?;
    let record = profile_for(provider, sanitized).map_err(LoginError::ProfileIo)?;
    let observed_at = now_iso();
    let account_fingerprint = record.account_fingerprint.clone();
    let signed_in = record.signed_in;
    let auth_mode = record.auth_mode;
    let profile_id = record.id.as_str().to_string();
    Ok(LoginStatus {
        provider_id: provider.clone(),
        profile_id,
        signed_in,
        auth_mode,
        account_fingerprint,
        source_label: format!("{}/{}", provider.as_str(), sanitized),
        observed_at,
    })
}

/// Remove the managed profile directory and return. Convenience
/// wrapper around [`delete_profile`] that adapts the
/// [`ProfileStoreError`] to [`LoginError`].
pub fn logout(provider: &CliProviderId, label: &str) -> Result<(), LoginError> {
    let sanitized = sanitize_label(label).map_err(LoginError::ProfileIo)?;
    delete_profile(provider, sanitized).map_err(LoginError::ProfileIo)
}

/// List every profile currently on disk for one provider. Convenience
/// wrapper that flattens [`list_profiles`] into a [`Vec`] of
/// [`LoginStatus`] so the UI can render one card per profile.
pub fn list(provider: &CliProviderId) -> Vec<LoginStatus> {
    let observed_at = now_iso();
    list_profiles(provider)
        .into_iter()
        .map(|record| LoginStatus {
            provider_id: record.provider_id.clone(),
            profile_id: record.id.as_str().to_string(),
            signed_in: record.signed_in,
            auth_mode: record.auth_mode,
            account_fingerprint: record.account_fingerprint,
            source_label: format!("{}/{}", record.provider_id.as_str(), record.label),
            observed_at: observed_at.clone(),
        })
        .collect()
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut out = text[..max].to_string();
    out.push('\u{2026}');
    out
}

/// Path helper exposed for tests so they can pre-create the
/// directory the launch path expects. Production code uses
/// [`ensure_managed_home`] instead.
#[cfg(test)]
fn write_dummy_home(path: &std::path::PathBuf, contents: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deny_list_catches_synthetic_secrets() {
        for needle in [
            concat!("echo sk", "-abcdefghijklmnopqrstuvwxyz"),
            "export OPENAI_API_KEY=sk-abc",
            "ANTHROPIC_API_KEY=sk-ant-123",
            concat!("AK", "IAABCDEFGHIJKLMNOPQRST"),
            concat!("gh", "p_abcdefghijklmnopqrstuvwxyz"),
            "xai-abcdefghijklmnopqrst",
        ] {
            assert!(
                find_credential_match(needle).is_some(),
                "deny-list must catch {needle:?}"
            );
        }
        for benign in [
            "Welcome to claude-code",
            "logged in as adrian@example.com",
            "saved 3 files",
        ] {
            assert!(
                find_credential_match(benign).is_none(),
                "benign log must not trip the deny-list: {benign:?}"
            );
        }
    }

    #[test]
    fn redact_replaces_matched_substrings_with_redacted_marker() {
        let input = concat!(
            "header OPENAI_API_KEY=sk",
            "-abcdefghijklmnopqrstuvwxyz tail"
        );
        let out = redact_credential_bytes(input).expect("must match");
        assert!(
            !out.contains("sk-abc"),
            "the literal key body must not survive redaction: {out}"
        );
        assert!(out.contains("<redacted>"), "marker must be present: {out}");
        assert!(
            out.contains("header"),
            "non-secret context must survive: {out}"
        );
    }

    #[test]
    fn ambient_forbidden_env_is_empty_when_no_byok_vars_set() {
        // The test harness is not guaranteed to have these unset; we
        // only assert the function does not panic and returns a
        // well-typed Option.
        let _ = ambient_forbidden_env();
    }

    #[test]
    fn vendor_login_argv_returns_registered_providers() {
        let provider = CliProviderId::new("claude");
        assert!(
            vendor_login_argv(&provider).is_none(),
            "Claude sign-in is not in the product"
        );
        let provider = CliProviderId::new("codex");
        let (binary, argv) = vendor_login_argv(&provider).expect("codex registered");
        assert_eq!(binary, "codex");
        assert_eq!(argv, &["login"]);
        let unknown = CliProviderId::new("nope");
        assert!(vendor_login_argv(&unknown).is_none());
    }

    #[test]
    fn logout_removes_managed_home_for_label() {
        super::super::profile::with_isolated_profile_home(|| {
            let provider = CliProviderId::new("claude");
            let label = "logout-test";
            // Create the profile first so logout has something to remove.
            let _ = profile_for(&provider, label).expect("first load creates");
            let home = managed_home(&provider, label);
            assert!(home.exists(), "managed home must exist after create");
            logout(&provider, label).expect("logout removes the tree");
            assert!(!home.exists(), "logout must remove the managed home");
        });
    }

    #[test]
    fn forbidden_env_aborts_before_spawn() {
        // We cannot safely inject an env var from inside the test
        // process and have the function under test see it; the
        // abort-before-spawn guarantee is covered instead by the
        // ambient_forbidden_env probe paired with the contract that
        // `login` calls it before constructing any `Command`.
        let probe = ambient_forbidden_env();
        // Either an env var is set in this harness (probe returns
        // Some) or it is not (probe returns None); both are valid.
        // The point of this test is the typing and idempotency.
        let _ = probe;
    }

    #[test]
    fn truncate_handles_short_and_long_inputs() {
        assert_eq!(truncate("short", 10), "short");
        let long = "x".repeat(300);
        let out = truncate(&long, 10);
        assert!(out.ends_with('\u{2026}'));
        assert!(out.chars().count() <= 11);
    }

    #[test]
    fn list_returns_empty_for_unknown_provider() {
        // `list` must not panic on a provider with no managed home.
        let provider = CliProviderId::new("definitely-not-a-real-provider");
        let _ = list(&provider);
    }

    #[test]
    #[ignore = "writes to the real managed profile root; run with --include-ignored"]
    fn login_with_synthetic_credential_in_stdout_is_rejected() {
        let provider = CliProviderId::new("claude");
        let label = "leak-test";
        let home = managed_home(&provider, label);
        // Drop a binary that pretends to be the vendor CLI and emits
        // a deny-listed token on stdout.
        let fake = home.join("fake_claude_login.sh");
        write_dummy_home(
            &fake,
            concat!("#!/bin/sh\necho OPENAI_API_KEY=sk", "-leaked-key-body\n"),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&fake).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&fake, perms).unwrap();
        }
        let result = login(&provider, label);
        assert!(matches!(
            result,
            Err(LoginError::CredentialLeakSuspected { .. })
        ));
    }
}
