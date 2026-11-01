//! User-operated cloud agent endpoint connection (ENG-62) and target-scoped
//! credential custody (ENG-65).
//!
//! ENG-62: a "cloud endpoint" is a base URL + credential a user points the
//! runtime at (an agent backend they run/rent themselves — not Cloudflare,
//! which has its own OAuth-based `cloudflare.rs`). This module verifies
//! reachability and holds the session-scoped selection.
//!
//! ENG-65: credentials must never cross execution-target boundaries — a
//! `Local` target's secret must never reach a `Cloud` or `Computer` target's
//! tool calls, and vice versa. Rather than a single shared cell (as
//! `context::cloudflare_credential_registry` uses for the single-target
//! Cloudflare case), this registry keys each cell by `(session_id, target)`
//! so a lookup for the wrong target simply returns `None` — there is no
//! shared value a mis-scoped read could observe.
//!
//! Secret values are never included in `Debug`/`Display` output or logged by
//! this module; only target/session identifiers and boolean/status outcomes
//! are ever formatted.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

/// Execution target a credential is scoped to. Mirrors the targets the
/// mobile runtime and desktop execution backends can dispatch tool calls to;
/// see `ToolContext::execution_backend` for the desktop-side backend id this
/// corresponds to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionTarget {
    /// The device/host running the tool call directly.
    Local,
    /// A user-operated cloud agent endpoint (this module's connection).
    Cloud,
    /// A remote "computer" (e.g. a paired desktop/daemon) driven over the
    /// network.
    Computer,
}

impl ExecutionTarget {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExecutionTarget::Local => "local",
            ExecutionTarget::Cloud => "cloud",
            ExecutionTarget::Computer => "computer",
        }
    }
}

/// Base URL + bearer credential for a user-connected cloud agent endpoint.
/// `Debug` is derived but the credential field is redacted explicitly below
/// so accidental `{:?}` logging never leaks the secret.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudEndpointConfig {
    pub base_url: String,
    pub credential: String,
}

impl std::fmt::Debug for CloudEndpointConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudEndpointConfig")
            .field("base_url", &self.base_url)
            .field("credential", &"<redacted>")
            .finish()
    }
}

/// Outcome of a reachability check against a candidate cloud endpoint,
/// performed before the selection is persisted.
#[derive(Debug, Clone, Serialize)]
pub struct CloudEndpointVerification {
    pub reachable: bool,
    pub status_code: Option<u16>,
    pub error: Option<String>,
}

/// Verify a candidate cloud endpoint is reachable and accepts the supplied
/// credential, without persisting anything. Callers persist the selection
/// (e.g. via `update_target_credential(session_id, ExecutionTarget::Cloud, ..)`)
/// only after a successful verification.
pub async fn verify_endpoint(config: &CloudEndpointConfig) -> CloudEndpointVerification {
    let url = match url::Url::parse(&config.base_url) {
        Ok(url) => url,
        Err(err) => {
            return CloudEndpointVerification {
                reachable: false,
                status_code: None,
                error: Some(format!("invalid base URL: {err}")),
            }
        }
    };
    if url.scheme() != "https" {
        return CloudEndpointVerification {
            reachable: false,
            status_code: None,
            error: Some("cloud endpoint base URL must use https".to_string()),
        };
    }

    let client =
        match crate::coding_tools::http_client::client_for(url.as_str(), Duration::from_secs(10)) {
            Ok(client) => client,
            Err(err) => {
                return CloudEndpointVerification {
                    reachable: false,
                    status_code: None,
                    error: Some(format!("could not build HTTP client: {err}")),
                }
            }
        };

    let request = rightkit_http::Request::get(url.as_str()).bearer(&config.credential);
    match client.send_any(&request.headers_only()).await {
        Ok(response) => {
            let status = response.status;
            let success = (200..300).contains(&status);
            CloudEndpointVerification {
                reachable: success || status == 401 || status == 403,
                status_code: Some(status),
                error: if success {
                    None
                } else {
                    Some(format!("endpoint responded with HTTP {status}"))
                },
            }
        }
        Err(err) => CloudEndpointVerification {
            reachable: false,
            status_code: None,
            error: Some(err.to_string()),
        },
    }
}

/// Key into the target-scoped registry: a session paired with the execution
/// target the credential is bound to. Two different targets in the same
/// session get two independent cells, so a credential stored for `Cloud`
/// cannot be returned by a lookup for `Local` or `Computer`.
type RegistryKey = (String, ExecutionTarget);
type CredentialCell = Arc<RwLock<CloudEndpointConfig>>;

fn target_credential_registry() -> &'static Mutex<HashMap<RegistryKey, CredentialCell>> {
    static REGISTRY: OnceLock<Mutex<HashMap<RegistryKey, CredentialCell>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Store (or replace) the credential scoped to `(session_id, target)`. This
/// is the only write path into the registry, and it never overwrites a cell
/// belonging to a different target.
pub fn update_target_credential(
    session_id: &str,
    target: ExecutionTarget,
    config: CloudEndpointConfig,
) {
    let key = (session_id.to_string(), target);
    let mut registry = target_credential_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match registry.get(&key) {
        Some(cell) => {
            let mut guard = cell
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *guard = config;
        }
        None => {
            registry.insert(key, Arc::new(RwLock::new(config)));
        }
    }
}

/// Read the credential scoped to `(session_id, target)`. Returns `None` when
/// no credential has ever been stored for that exact target — a lookup for
/// `ExecutionTarget::Local` never returns a `Cloud`- or `Computer`-scoped
/// value, by construction of the registry key.
pub fn target_credential(session_id: &str, target: ExecutionTarget) -> Option<CloudEndpointConfig> {
    let key = (session_id.to_string(), target);
    let registry = target_credential_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    registry.get(&key).map(|cell| {
        cell.read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    })
}

/// Drop every credential cell for `session_id` across all targets. Callers
/// should invoke this alongside other per-session cleanup (see
/// `retire_cloudflare_credential`) so a long-running daemon does not
/// accumulate entries forever.
pub fn retire_target_credentials(session_id: &str) {
    let mut registry = target_credential_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    registry.retain(|(sid, _), _| sid != session_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(url: &str, credential: &str) -> CloudEndpointConfig {
        CloudEndpointConfig {
            base_url: url.to_string(),
            credential: credential.to_string(),
        }
    }

    #[test]
    fn credential_scoped_to_one_target_is_invisible_to_another() {
        let session_id = "scope-test-cross-target";
        update_target_credential(
            session_id,
            ExecutionTarget::Local,
            config("https://local.example", "local-secret"),
        );
        update_target_credential(
            session_id,
            ExecutionTarget::Cloud,
            config("https://cloud.example", "cloud-secret"),
        );

        let local = target_credential(session_id, ExecutionTarget::Local)
            .expect("local credential present");
        assert_eq!(local.credential, "local-secret");

        let cloud = target_credential(session_id, ExecutionTarget::Cloud)
            .expect("cloud credential present");
        assert_eq!(cloud.credential, "cloud-secret");

        // A target that was never written (Computer) must not see either.
        assert!(target_credential(session_id, ExecutionTarget::Computer).is_none());

        retire_target_credentials(session_id);
    }

    #[test]
    fn retire_clears_every_target_for_session_only() {
        let session_id = "scope-test-retire";
        let other_session = "scope-test-retire-other";
        update_target_credential(
            session_id,
            ExecutionTarget::Cloud,
            config("https://cloud.example", "a"),
        );
        update_target_credential(
            session_id,
            ExecutionTarget::Computer,
            config("https://computer.example", "b"),
        );
        update_target_credential(
            other_session,
            ExecutionTarget::Cloud,
            config("https://cloud.example", "c"),
        );

        retire_target_credentials(session_id);

        assert!(target_credential(session_id, ExecutionTarget::Cloud).is_none());
        assert!(target_credential(session_id, ExecutionTarget::Computer).is_none());
        assert!(target_credential(other_session, ExecutionTarget::Cloud).is_some());

        retire_target_credentials(other_session);
    }

    #[test]
    fn update_then_read_reflects_new_credential() {
        let session_id = "scope-test-swap";
        update_target_credential(
            session_id,
            ExecutionTarget::Cloud,
            config("https://cloud.example", "old"),
        );
        update_target_credential(
            session_id,
            ExecutionTarget::Cloud,
            config("https://cloud.example", "new"),
        );
        let read =
            target_credential(session_id, ExecutionTarget::Cloud).expect("credential present");
        assert_eq!(read.credential, "new");
        retire_target_credentials(session_id);
    }

    #[test]
    fn debug_format_redacts_credential() {
        let cfg = config("https://cloud.example", "super-secret-value");
        let formatted = format!("{cfg:?}");
        assert!(!formatted.contains("super-secret-value"));
        assert!(formatted.contains("<redacted>"));
    }
}
