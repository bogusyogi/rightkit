//! The environment-driven A2A adapter: the trust-root verifier, the trusted remote transport and
//! the start-up builder that reads `<PREFIX>_A2A_EXECUTOR_URL`, `<PREFIX>_A2A_TRUST_ROOT_PUBKEY`
//! and `<PREFIX>_A2A_DESCRIPTOR_JSON`. Compiled only with feature `workflow-a2a-env`; without it
//! none of the three names, the verifier or the transport exists and no A2A adapter is built.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use rightkit_http::Client as BlockingClient;

use rightkit_workflow::control_plane::a2a::{
    A2aExecutorDescriptor, A2aIdentityProof, A2aIdentityVerifier,
};
use rightkit_workflow::control_plane::context::ContextManifest;
use rightkit_workflow::control_plane::domain::NodeSpec;
use rightkit_workflow::control_plane::executors::NodeOutput;

use super::{A2aAdapter, A2aAttempt, A2aExecutionPort, AdapterError};

/// Trust-root-backed A2A identity verifier. Verifies an ed25519 signature
/// over the remote executor's signed payload hash against a daemon-configured
/// trust-root public key, and binds the descriptor's declared trust-root hash
/// to that key. Fails closed on any mismatch or missing field — it never
/// auto-grants trust.
pub struct Ed25519TrustRootVerifier {
    pubkey: VerifyingKey,
    trust_root_hash: String,
}

impl Ed25519TrustRootVerifier {
    /// Build a verifier from a hex-encoded ed25519 public key. The SHA-256 of
    /// the raw key is cached as the expected trust-root hash so a descriptor
    /// cannot advertise a trust root the daemon did not actually configure.
    pub fn new(hex_pubkey: &str) -> Result<Self, String> {
        let bytes = rightkit_fs::digest::unhex(hex_pubkey.trim())
            .ok_or_else(|| "invalid A2A trust-root pubkey hex".to_string())?;
        let array: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| "A2A trust-root pubkey must be 32 bytes".to_string())?;
        let pubkey = VerifyingKey::from_bytes(&array)
            .map_err(|_| "invalid A2A trust-root ed25519 public key".to_string())?;
        let trust_root_hash = rightkit_fs::digest::sha256_bytes(&bytes);
        Ok(Self {
            pubkey,
            trust_root_hash,
        })
    }
}

impl A2aIdentityVerifier for Ed25519TrustRootVerifier {
    fn verify(
        &self,
        descriptor: &A2aExecutorDescriptor,
        proof: &A2aIdentityProof,
    ) -> Result<(), String> {
        // Bind the descriptor's declared trust root to the configured key so a
        // descriptor cannot claim a trust root the daemon never loaded.
        if !descriptor.trust_root_pubkey_hash.trim().is_empty()
            && descriptor.trust_root_pubkey_hash.trim() != self.trust_root_hash
        {
            return Err(
                "A2A descriptor trust-root hash does not match the configured trust root".into(),
            );
        }
        if proof.key_id.trim().is_empty()
            || proof.algorithm != "ed25519"
            || proof.signed_payload_hash.trim().is_empty()
            || proof.signature.is_empty()
        {
            return Err("A2A identity proof is missing required fields".into());
        }
        let signature = Signature::from_slice(&proof.signature)
            .map_err(|_| "A2A identity proof signature must be 64 bytes".to_string())?;
        self.pubkey
            .verify(proof.signed_payload_hash.trim().as_bytes(), &signature)
            .map_err(|error| format!("A2A identity proof verification failed: {error}"))
    }
}

/// Production A2A transport: a real, daemon-owned client that reaches a
/// trusted remote executor over an HTTPS channel. Construction performs a
/// one-time trust handshake (`GET {base}/a2a/v1/identity`) and caches the
/// remote's cryptographic identity proof; a failed handshake means the
/// adapter cannot be built, so A2A stays closed (fail-closed).
pub struct TrustedRemoteTransport {
    client: BlockingClient,
    base_url: String,
    workspace_root: String,
    identity_proof: Mutex<Option<A2aIdentityProof>>,
}

impl TrustedRemoteTransport {
    /// Connect to the trusted remote executor and complete the identity
    /// handshake. The remote must present an ed25519-signed `A2aIdentityProof`
    /// at `/a2a/v1/identity`; failure here leaves A2A unconfigured.
    pub fn new(base_url: &str, workspace_root: &str, _identity_did: &str) -> Result<Self, String> {
        Self::new_with_transport_security(base_url, workspace_root, true)
    }

    fn new_with_transport_security(
        base_url: &str,
        workspace_root: &str,
        require_https: bool,
    ) -> Result<Self, String> {
        let base_url = base_url.trim().trim_end_matches('/').to_string();
        if base_url.is_empty() {
            return Err("A2A executor base URL is empty".into());
        }
        if require_https
            && base_url
                .get(..8)
                .map(|scheme| scheme.eq_ignore_ascii_case("https://"))
                != Some(true)
        {
            return Err("A2A executor base URL must use HTTPS".into());
        }
        // The configured executor host alone may resolve privately; no redirects.
        let mut policy = rightkit_http::AddressPolicy::public_web();
        if let Some(host) = rightkit_agent_daemon::http_client::host_of(&base_url) {
            policy = policy.allow_if(vec![host], |_| true);
        }
        let client = BlockingClient::try_new(rightkit_http::ClientConfig {
            request_timeout: Duration::from_secs(30),
            response_timeout: Some(Duration::from_secs(30)),
            retry: rightkit_http::RetryPolicy::none(),
            address_policy: policy,
            proxy: rightkit_http::ProxyMode::Environment,
            ..rightkit_http::ClientConfig::default()
        })
        .map_err(|error| format!("failed to build A2A transport client: {error}"))?;
        let proof = fetch_remote_identity(&client, &base_url)?;
        Ok(Self {
            client,
            base_url,
            workspace_root: workspace_root.to_string(),
            identity_proof: Mutex::new(Some(proof)),
        })
    }
}

/// Perform the A2A trust handshake against the remote executor and return its
/// cryptographic identity proof. Any transport or schema failure is surfaced
/// so the caller keeps A2A closed.
fn fetch_remote_identity(
    client: &BlockingClient,
    base_url: &str,
) -> Result<A2aIdentityProof, String> {
    let url = format!("{base_url}/a2a/v1/identity");
    let response = client
        .send_any(&rightkit_http::Request::get(url))
        .map_err(|error| format!("A2A identity handshake request failed: {error}"))?;
    if !(200..300).contains(&response.status) {
        return Err(format!(
            "A2A identity handshake returned status {}",
            response.status
        ));
    }
    let proof: A2aIdentityProof = response
        .json()
        .map_err(|error| format!("A2A identity handshake returned invalid JSON: {error}"))?;
    if proof.key_id.trim().is_empty() || proof.algorithm != "ed25519" {
        return Err("A2A identity handshake returned an unsupported proof".into());
    }
    Ok(proof)
}

impl A2aExecutionPort for TrustedRemoteTransport {
    fn local_workspace_root(&self) -> &str {
        &self.workspace_root
    }

    fn identity_proof(&self) -> A2aIdentityProof {
        self.identity_proof
            .lock()
            .expect("a2a proof lock")
            .clone()
            .unwrap_or_else(|| A2aIdentityProof {
                key_id: String::new(),
                algorithm: String::new(),
                signed_payload_hash: String::new(),
                signature: Vec::new(),
            })
    }

    fn execute(
        &self,
        spec: &NodeSpec,
        manifest: &ContextManifest,
        attempt: &A2aAttempt,
    ) -> Result<NodeOutput, AdapterError> {
        let url = format!("{}/a2a/v1/execute", self.base_url);
        let body = serde_json::json!({
            "attempt_id": attempt.id(),
            "node_spec": spec,
            "manifest": manifest,
        });
        let request = rightkit_http::Request::post(url)
            .json(&body)
            .map_err(|error| AdapterError::ProjectionFailed {
                adapter: "a2a-transport",
                reason: format!("remote A2A execute request failed: {error}"),
            })?;
        let response =
            self.client
                .send_any(&request)
                .map_err(|error| AdapterError::ProjectionFailed {
                    adapter: "a2a-transport",
                    reason: format!("remote A2A execute request failed: {error}"),
                })?;
        if !(200..300).contains(&response.status) {
            return Err(AdapterError::ProjectionFailed {
                adapter: "a2a-transport",
                reason: format!("remote A2A execute returned status {}", response.status),
            });
        }
        let output: NodeOutput =
            response
                .json()
                .map_err(|error| AdapterError::ProjectionFailed {
                    adapter: "a2a-transport",
                    reason: format!("remote A2A execute returned invalid NodeOutput: {error}"),
                })?;
        Ok(output)
    }

    fn cancel(&self, attempt: &A2aAttempt) -> Result<(), AdapterError> {
        let url = format!("{}/a2a/v1/cancel/{}", self.base_url, attempt.id());
        let response = self
            .client
            .send_any(&rightkit_http::Request::post(url))
            .map_err(|error| AdapterError::ProjectionFailed {
                adapter: "a2a-transport",
                reason: format!("remote A2A cancel request failed: {error}"),
            })?;
        if !(200..300).contains(&response.status) {
            return Err(AdapterError::ProjectionFailed {
                adapter: "a2a-transport",
                reason: format!("remote A2A cancel returned status {}", response.status),
            });
        }
        Ok(())
    }
}

/// Build the daemon-owned A2A adapter if a trusted remote executor is
/// configured through the environment. Returns `None` (A2A stays closed)
/// whenever the executor URL, trust-root public key, or descriptor is missing
/// or invalid, or the trust handshake fails. Fail-closed by construction:
/// nothing here auto-grants trust, and a misconfigured or unreachable
/// executor never silently degrades to unverified admission.
///
/// Environment:
/// * `<PREFIX>_A2A_EXECUTOR_URL` — base URL of the trusted remote executor.
/// * `<PREFIX>_A2A_TRUST_ROOT_PUBKEY` — hex ed25519 public key of the trust root.
/// * `<PREFIX>_A2A_DESCRIPTOR_JSON` — JSON `A2aExecutorDescriptor` bounding the executor.
///
/// With workflows off for this process the three variables are never read and this returns
/// `None`.
pub fn try_build_daemon_a2a_adapter(
    workflows_feature: rightkit_workflow::control_plane::prelude::FeatureGate,
) -> Option<Arc<A2aAdapter>> {
    if !workflows_feature.is_enabled() {
        return None;
    }
    let base_url = match std::env::var(rightkit_agent_config::env_name("A2A_EXECUTOR_URL")) {
        Ok(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => {
            tracing::info!(
                target: "agent_server.workflow.a2a",
                "A2A executor URL not configured; bounded A2A stays closed"
            );
            return None;
        }
    };
    let trust_root_hex =
        match std::env::var(rightkit_agent_config::env_name("A2A_TRUST_ROOT_PUBKEY")) {
            Ok(value) if !value.trim().is_empty() => value.trim().to_string(),
            _ => {
                tracing::warn!(
                    target: "agent_server.workflow.a2a",
                    "A2A trust-root public key not configured; bounded A2A stays closed"
                );
                return None;
            }
        };
    let descriptor_json =
        match std::env::var(rightkit_agent_config::env_name("A2A_DESCRIPTOR_JSON")) {
            Ok(value) if !value.trim().is_empty() => value,
            _ => {
                tracing::warn!(
                    target: "agent_server.workflow.a2a",
                    "A2A descriptor not configured; bounded A2A stays closed"
                );
                return None;
            }
        };
    let descriptor: A2aExecutorDescriptor = match serde_json::from_str(&descriptor_json) {
        Ok(descriptor) => descriptor,
        Err(error) => {
            tracing::warn!(
                target: "agent_server.workflow.a2a",
                %error,
                "A2A descriptor JSON is invalid; bounded A2A stays closed"
            );
            return None;
        }
    };
    let verifier = match Ed25519TrustRootVerifier::new(&trust_root_hex) {
        Ok(verifier) => Arc::new(verifier),
        Err(error) => {
            tracing::warn!(
                target: "agent_server.workflow.a2a",
                %error,
                "A2A trust-root verifier could not be built; bounded A2A stays closed"
            );
            return None;
        }
    };
    let transport = match TrustedRemoteTransport::new(
        &base_url,
        &descriptor.workspace_root,
        &descriptor.identity_did,
    ) {
        Ok(transport) => Arc::new(transport),
        Err(error) => {
            tracing::warn!(
                target: "agent_server.workflow.a2a",
                %error,
                "A2A trust handshake failed; bounded A2A stays closed"
            );
            return None;
        }
    };
    match A2aAdapter::with_transport(descriptor, verifier, transport) {
        Ok(adapter) => Some(Arc::new(adapter)),
        Err(error) => {
            tracing::warn!(
                target: "agent_server.workflow.a2a",
                %error,
                "A2A descriptor failed admission validation; bounded A2A stays closed"
            );
            None
        }
    }
}
