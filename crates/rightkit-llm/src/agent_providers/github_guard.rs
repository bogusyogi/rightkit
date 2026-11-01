//! Egress guard for [`crate::agent_providers::github::GitHubClient`]'s caller-supplied `base_url`.
//!
//! Enforces https-only (localhost may use http) and, unless explicitly
//! approved, rejects a `base_url` that resolves to a private/reserved network
//! address — the fix for a `base_url` override sending the GitHub bearer token
//! to an attacker-controlled or unintended internal host.

use crate::agent_providers::transport::ProviderError;
use tokio::sync::OnceCell;

/// Memoized target validator: DNS resolution only runs once per client
/// instance regardless of how many requests it serves.
pub(crate) struct TargetGuard {
    base_url: String,
    approved_private: bool,
    check: OnceCell<Result<(), ProviderError>>,
}

impl TargetGuard {
    pub(crate) fn new(base_url: String, approved_private: bool) -> Self {
        Self {
            base_url,
            approved_private,
            check: OnceCell::new(),
        }
    }

    /// Validate `base_url` is https (or http to localhost) and, unless
    /// explicitly approved, does not resolve to a private/reserved address.
    /// Result is memoized after the first call.
    pub(crate) async fn ensure_allowed(&self) -> Result<(), ProviderError> {
        self.check
            .get_or_init(|| async { self.check_once().await })
            .await
            .clone()
    }

    async fn check_once(&self) -> Result<(), ProviderError> {
        let url = url::Url::parse(&self.base_url).map_err(|e| {
            ProviderError::Provider(format!("invalid base_url '{}': {e}", self.base_url))
        })?;

        let host = url
            .host_str()
            .ok_or_else(|| {
                ProviderError::Provider(format!("base_url '{}' has no host", self.base_url))
            })?
            .to_string();
        let is_loopback_host = host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());

        if url.scheme() != "https" && !(url.scheme() == "http" && is_loopback_host) {
            return Err(ProviderError::Provider(format!(
                "base_url '{}' must use https (http is only allowed for localhost)",
                self.base_url
            )));
        }

        if self.approved_private {
            return Ok(());
        }

        // The outbound address policy of `rightkit-http` (the same one every
        // request through the client passes): resolve once, classify every address.
        let port = url.port_or_known_default().unwrap_or(443);
        let rejected = |detail: String| {
            ProviderError::Provider(format!(
                "base_url '{}' rejected by network egress guard: {detail}",
                self.base_url
            ))
        };
        tokio::task::spawn_blocking(move || {
            rightkit_http::AddressPolicy::public_web().resolve_vetted(&host, port)
        })
        .await
        .map_err(|e| rejected(e.to_string()))?
        .map(|_| ())
        .map_err(|e| rejected(e.to_string()))
    }
}

/// Hard cap on how much of a non-2xx response body is kept — a
/// malicious/misbehaving endpoint must not be able to balloon an error message.
pub(crate) const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

/// Text of up to [`MAX_ERROR_BODY_BYTES`] of an error body, noting truncation.
pub(crate) fn capped_error_text(body: &[u8]) -> String {
    let truncated = body.len() >= MAX_ERROR_BODY_BYTES;
    let kept = &body[..body.len().min(MAX_ERROR_BODY_BYTES)];
    let mut text = String::from_utf8_lossy(kept).into_owned();
    if truncated {
        text.push_str(&format!(
            "... [truncated, body exceeded {MAX_ERROR_BODY_BYTES} bytes]"
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn default_github_base_url_is_allowed() {
        let guard = TargetGuard::new("https://api.github.com".to_string(), false);
        assert!(guard.ensure_allowed().await.is_ok());
    }

    #[tokio::test]
    async fn plain_http_to_remote_host_rejected() {
        let guard = TargetGuard::new("http://github.example.com".to_string(), false);
        let err = guard.ensure_allowed().await.unwrap_err();
        assert!(matches!(err, ProviderError::Provider(_)));
        assert!(err.to_string().contains("https"));
    }

    #[tokio::test]
    async fn http_to_localhost_passes_scheme_gate_but_fails_egress_guard() {
        // Loopback IS explicitly permitted for the http scheme check;
        // resolve_then_reject_private has no allow-loopback fast path here, so
        // it still rejects — proving the failure comes from the egress guard,
        // not the scheme gate.
        let guard = TargetGuard::new("http://localhost:8080".to_string(), false);
        let err = guard.ensure_allowed().await.unwrap_err();
        assert!(err.to_string().contains("egress guard"));
    }

    #[tokio::test]
    async fn http_to_internal_host_rejected_without_approval() {
        let guard = TargetGuard::new("http://internal-host.local:8080".to_string(), false);
        let err = guard.ensure_allowed().await.unwrap_err();
        assert!(matches!(err, ProviderError::Provider(_)));
        assert!(err.to_string().contains("https"));
    }

    #[tokio::test]
    async fn private_ip_https_rejected_without_approval() {
        let guard = TargetGuard::new("https://192.168.1.50:8443".to_string(), false);
        let err = guard.ensure_allowed().await.unwrap_err();
        assert!(err.to_string().contains("egress guard"));
    }

    #[tokio::test]
    async fn private_ip_allowed_with_explicit_approval() {
        let guard = TargetGuard::new("https://192.168.1.50:8443".to_string(), true);
        assert!(guard.ensure_allowed().await.is_ok());
    }

    #[tokio::test]
    async fn target_check_is_memoized() {
        let guard = TargetGuard::new("https://api.github.com".to_string(), false);
        let first = guard.ensure_allowed().await;
        let second = guard.ensure_allowed().await;
        assert!(first.is_ok());
        assert!(second.is_ok());
    }
}
