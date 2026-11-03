fn provider_key_status_for(provider: &str) -> Result<ProviderKeyStatus, String> {
    validate_provider(provider)?;
    let path = provider_key_path(provider)?;
    let env_configured = provider_env_var(provider)
        .and_then(|name| std::env::var(name).ok())
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);

    // Report the source the loader will ACTUALLY use. This checked env first,
    // matching the old loader, so a stale exported variable made Settings show
    // "Saved securely · env" for a provider whose real stored key was never
    // consulted — and Remove appeared to do nothing because it cleared the
    // store the label was not describing. Stored keys now win in both places;
    // env is reported only when nothing is stored.
    let backend = rightkit_llm::agent_providers::OsKeyStore;
    let stored = rightkit_llm::agent_providers::status_source(&backend, provider);
    let source = match stored {
        rightkit_llm::agent_providers::KeySource::Missing if env_configured => {
            rightkit_llm::agent_providers::KeySource::Env
        }
        other => other,
    };
    let configured = !matches!(source, rightkit_llm::agent_providers::KeySource::Missing);

    Ok(ProviderKeyStatus {
        provider: provider.to_string(),
        configured,
        path: path.display().to_string(),
        source: source.as_str().to_string(),
    })
}

fn provider_key_path(provider: &str) -> Result<PathBuf, String> {
    validate_provider(provider)?;
    let root = rightkit_app_settings::agent_config::state_root()
        .ok_or_else(|| "could not locate user home directory".to_string())?;
    Ok(root.join(format!("{provider}.key")))
}

fn provider_env_var(provider: &str) -> Option<&'static str> {
    match provider {
        "openai" => Some("OPENAI_API_KEY"),
        "commandcode" => Some("COMMAND_CODE_API_KEY"),
        "openrouter" => Some("OPENROUTER_API_KEY"),
        "opencode" | "opencode-go" => Some("OPENCODE_API_KEY"),
        "minimax" => Some("MINIMAX_API_KEY"),
        "tinyfish" => Some("TINYFISH_API_KEY"),
        _ => None,
    }
}

fn validate_provider(provider: &str) -> Result<(), String> {
    if provider_env_var(provider).is_some() {
        Ok(())
    } else {
        Err(format!("unknown provider: {provider}"))
    }
}

// ── S:H13: credential resolution for connect/test/rotate ──────────────────

/// A provider credential resolved the way the runtime loader resolves it,
/// without ever logging, echoing, or serializing the value outside the
/// caller's explicit control.
#[derive(Debug, PartialEq)]
enum ResolvedProviderCredential {
    /// No credential anywhere — the account is not connected.
    Missing,
    /// A credential exists but the OS refused to hand it back (the macOS ACL
    /// story: the keychain item predates this build). `remedy` carries the
    /// user-facing guidance from [`rightkit_llm::agent_providers::KeySource`].
    Unreadable { remedy: Option<&'static str> },
    /// A usable credential plus the non-secret source label the status view
    /// already reports (`keychain` | `env`).
    Present {
        secret: String,
        source: &'static str,
    },
}

/// Backend-injectable resolution so tests exercise the policy without
/// touching a real OS credential store. Production callers pass
/// `&rightkit_llm::agent_providers::OsKeyStore`.
///
/// Layering mirrors the status view and the runtime loader: a stored key wins
/// (keychain), and the provider's environment variable is consulted only when nothing is
/// stored. `status_source` is deliberately used instead of
/// `read_key` because probing is read-only and must not record a keychain failure.
fn resolve_provider_credential_with<B: rightkit_llm::agent_providers::KeyStoreBackend>(
    backend: &B,
    provider: &str,
) -> Result<ResolvedProviderCredential, String> {
    validate_provider(provider)?;
    let status = rightkit_llm::agent_providers::status_source(backend, provider);
    let resolved = match status {
        rightkit_llm::agent_providers::KeySource::Keychain => {
            match backend.get(
                rightkit_llm::agent_providers::key_migration::SERVICE,
                provider,
            ) {
                Ok(value) if !value.trim().is_empty() => ResolvedProviderCredential::Present {
                    secret: value.trim().to_string(),
                    source: "keychain",
                },
                // Status said keychain but the value vanished or emptied
                // between the two reads; report it as unreadable rather than
                // silently claiming the account is connected.
                _ => ResolvedProviderCredential::Unreadable {
                    remedy: rightkit_llm::agent_providers::KeySource::Unreadable.remedy(),
                },
            }
        }
        rightkit_llm::agent_providers::KeySource::Unreadable => {
            ResolvedProviderCredential::Unreadable {
                remedy: status.remedy(),
            }
        }
        // Missing (or the env variant status_source never returns): stored
        // sources came up empty, so fall back to the environment variable.
        _ => match provider_env_var(provider).and_then(|name| std::env::var(name).ok()) {
            Some(value) if !value.trim().is_empty() => ResolvedProviderCredential::Present {
                secret: value.trim().to_string(),
                source: "env",
            },
            _ => ResolvedProviderCredential::Missing,
        },
    };
    Ok(resolved)
}

// ── S:H13: live credential probe ────────────────────────────────────────

/// Outcome of probing a provider with the resolved credential.
#[derive(Debug, PartialEq)]
enum ProviderCredentialProbe {
    /// The provider accepted the credential.
    Accepted,
    /// The provider explicitly rejected the credential (401/403).
    Rejected,
    /// The provider does not offer the probe route (404/405/501) — the
    /// credential could not be verified either way. Deliberately distinct
    /// from `Rejected`: a missing route must never be reported as a bad key.
    ProbeUnavailable,
    /// Any other provider response, carrying the status code.
    ProviderError(u16),
    /// No probe route is defined for this provider (no registry base URL).
    Unsupported,
    /// The request never reached the provider, carrying a secret-free reason.
    Unreachable(String),
}

/// Probe the provider with the resolved credential over the generic
/// OpenAI-compatible `GET {base_url}/models` route taken from the provider
/// registry — the same base URL the real runtime client uses. No redirects:
/// a credential-bearing probe must never follow a cross-host redirect.
/// The secret is sent only as the bearer header and is never logged.
async fn probe_provider_credential(provider: &str, secret: &str) -> ProviderCredentialProbe {
    let Some(info) = rightkit_llm::agent_providers::find_provider(provider) else {
        return ProviderCredentialProbe::Unsupported;
    };
    if info.base_url.trim().is_empty() {
        return ProviderCredentialProbe::Unsupported;
    }
    let url = format!("{}/models", info.base_url.trim_end_matches('/'));
    // Redirects are never followed (`endpoint_client`), and the bearer secret is
    // registered with the request so no error text can echo it.
    let client =
        match rightkit_http::http_client::endpoint_client(&url, std::time::Duration::from_secs(10))
        {
            Ok(client) => client,
            Err(error) => {
                return ProviderCredentialProbe::Unreachable(format!(
                    "probe client unavailable: {error}"
                ))
            }
        };
    let response = match client
        .send_any(&rightkit_http::Request::get(url).bearer(secret))
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return ProviderCredentialProbe::Unreachable(format!("probe request failed: {error}"))
        }
    };
    classify_provider_probe_status(response.status)
}

fn classify_provider_probe_status(status: u16) -> ProviderCredentialProbe {
    match status {
        200..=299 => ProviderCredentialProbe::Accepted,
        401 | 403 => ProviderCredentialProbe::Rejected,
        404 | 405 | 501 => ProviderCredentialProbe::ProbeUnavailable,
        other => ProviderCredentialProbe::ProviderError(other),
    }
}

/// Wire projection of a probe outcome: `(state, valid, detail, providerStatus)`.
/// Details are static secret-free strings or transport reasons; the caller's
/// leakage gate still checks the serialized body.
fn probe_outcome_wire(
    outcome: &ProviderCredentialProbe,
) -> (&'static str, bool, Option<String>, Option<u16>) {
    match outcome {
        ProviderCredentialProbe::Accepted => ("accepted", true, None, None),
        ProviderCredentialProbe::Rejected => (
            "rejected",
            false,
            Some("the provider rejected this credential".to_string()),
            None,
        ),
        ProviderCredentialProbe::ProbeUnavailable => (
            "unverified",
            false,
            Some(
                "the provider does not offer the models probe route; the credential was neither accepted nor rejected"
                    .to_string(),
            ),
            None,
        ),
        ProviderCredentialProbe::ProviderError(status) => (
            "provider_error",
            false,
            Some("the provider answered the probe with an unexpected status".to_string()),
            Some(*status),
        ),
        ProviderCredentialProbe::Unsupported => (
            "unsupported",
            false,
            Some("no probe route is defined for this provider".to_string()),
            None,
        ),
        ProviderCredentialProbe::Unreachable(reason) => {
            ("unreachable", false, Some(reason.clone()), None)
        }
    }
}
