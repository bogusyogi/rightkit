//! Daemon wiring + per-router state for the workflow API surface
//! (WP-09 / ADR-0021).
//!
//! The router is built once per daemon process: the daemon constructs a
//! [`WorkflowApiState`], wraps it in `Arc`, and hands it to
//! [`build_router`]. From there the [`crate::api_workflows::routes`]
//! module owns the actual endpoint definitions; this module is just the
//! glue between the daemon and axum.
//!
//! The struct is intentionally cheap to clone-free (every field is already
//! `Arc`-wrapped or `Copy`) so handlers can hold a single `State<Arc<Self>>`
//! without re-locking behind a mutex.

#![forbid(unsafe_code)]
#![allow(clippy::items_after_test_module)]

use std::sync::Arc;

use ed25519_dalek::{Signature, Verifier, VerifyingKey};

use axum::Router;

use crate::api::{DaemonState, SharedStorage};

use rightkit_workflow::control_plane::prelude::FeatureGate;

/// Concrete verifier built at daemon startup from configured public trust
/// roots. Invalid config entries are ignored; an empty valid set rejects all.
pub struct Ed25519PackageVerifier {
    keys: std::collections::BTreeMap<String, VerifyingKey>,
}

impl Ed25519PackageVerifier {
    pub fn from_config(
        keys: &[rightkit_app_settings::agent_config::WorkflowImportTrustedPublicKey],
    ) -> Self {
        let keys = keys
            .iter()
            .filter_map(|entry| {
                let bytes = rightkit_crypto::base64_decode(&entry.public_key_base64).ok()?;
                let bytes: [u8; 32] = bytes.try_into().ok()?;
                let key = VerifyingKey::from_bytes(&bytes).ok()?;
                (!entry.key_id.trim().is_empty()).then(|| (entry.key_id.clone(), key))
            })
            .collect();
        Self { keys }
    }
}

impl rightkit_workflow::control_plane::import_export::PackageSignatureVerifier
    for Ed25519PackageVerifier
{
    fn verify(
        &self,
        manifest: &rightkit_workflow::control_plane::import_export::PackageManifest,
        payload: &[u8],
        signature: &[u8],
    ) -> Result<(), String> {
        let key = self
            .keys
            .get(&manifest.signing_key_id)
            .ok_or_else(|| "untrusted workflow import signing key".to_string())?;
        let signature = Signature::from_slice(signature)
            .map_err(|_| "invalid workflow import signature".to_string())?;
        let manifest = serde_json::to_vec(manifest)
            .map_err(|_| "unable to canonicalize workflow import manifest".to_string())?;
        let mut signed = manifest;
        signed.extend_from_slice(payload);
        key.verify(&signed, &signature)
            .map_err(|_| "workflow import signature verification failed".to_string())
    }
}

/// Per-router state for `/workflows`, `/runs`, and `/openapi.json`.
///
/// Paths are deliberately unversioned at the surface (the OpenAPI document
/// still advertises v1, but the router itself uses the bare paths so the
/// desktop UI does not have to know about the prefix). The
/// [`crate::api_workflows::openapi`] module is the single source of truth for
/// the wire contract.
///
/// The struct is intentionally cheap to clone-free (every field is already
/// `Arc`-wrapped or `Copy`) so handlers can hold a single `State<Arc<Self>>`
/// without re-locking behind a mutex.
///
/// The supervisor is optional because the feature gate is V1-default
/// `false` and the daemon is supposed to defer building the supervisor
/// until it knows the gate is on. WP-03 leaves it to the harness to wire
/// the supervisor in; this struct simply carries whatever the daemon
/// decides to hand over.
pub struct WorkflowApiState {
    /// Durable SQLite-backed storage. Used by every read endpoint through
    /// `WorkflowRepository`.
    pub storage: SharedStorage,
    /// Resolved workflow feature gate (mirrors
    /// `rightkit_app_settings::agent_config::FeatureConfig::workflows_enabled`). Every handler
    /// calls `feature.is_enabled()` before doing any work.
    pub feature: FeatureGate,
    /// Injected cryptographic package verifier. Default rejects imports until
    /// a daemon trust provider supplies a real verifier.
    pub import_verifier:
        Arc<dyn rightkit_workflow::control_plane::import_export::PackageSignatureVerifier>,
}

impl WorkflowApiState {
    /// Build a fresh state object already wrapped in `Arc` for axum. The
    /// daemon wiring in `main_runtime` calls this once at startup; handlers
    /// receive the `Arc` via the `State` extractor.
    pub fn new(storage: SharedStorage, feature: FeatureGate) -> Arc<Self> {
        Self::new_with_import_verifier(
            storage,
            feature,
            Arc::new(rightkit_workflow::control_plane::import_export::RejectUnsignedPackage),
        )
    }

    pub fn new_with_import_verifier(
        storage: SharedStorage,
        feature: FeatureGate,
        import_verifier: Arc<
            dyn rightkit_workflow::control_plane::import_export::PackageSignatureVerifier,
        >,
    ) -> Arc<Self> {
        Arc::new(Self {
            storage,
            feature,
            import_verifier,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use rightkit_workflow::control_plane::import_export::PackageSignatureVerifier;

    #[test]
    fn empty_trust_roots_fail_closed() {
        let verifier = Ed25519PackageVerifier::from_config(&[]);
        let manifest = rightkit_workflow::control_plane::import_export::PackageManifest {
            name: "n".into(),
            version: "1".into(),
            content_hash: "sha256:x".into(),
            intended_state: rightkit_workflow::control_plane::domain::PublicationState::Draft,
            declared_capabilities: rightkit_workflow::control_plane::domain::CapabilitySet::empty(),
            schema_version: 1,
            declared_effects: Vec::new(),
            signing_key_id: "missing".into(),
        };
        assert!(verifier.verify(&manifest, b"payload", &[0; 64]).is_err());
    }

    #[test]
    fn configured_ed25519_key_verifies_bound_manifest_and_payload() {
        let signing = SigningKey::from_bytes(&[7; 32]);
        let config = rightkit_app_settings::agent_config::WorkflowImportTrustedPublicKey {
            key_id: "test".into(),
            public_key_base64: rightkit_crypto::base64_encode(signing.verifying_key().as_bytes()),
        };
        let verifier = Ed25519PackageVerifier::from_config(&[config]);
        let manifest = rightkit_workflow::control_plane::import_export::PackageManifest {
            name: "n".into(),
            version: "1".into(),
            content_hash: "sha256:x".into(),
            intended_state: rightkit_workflow::control_plane::domain::PublicationState::Draft,
            declared_capabilities: rightkit_workflow::control_plane::domain::CapabilitySet::empty(),
            schema_version: 1,
            declared_effects: Vec::new(),
            signing_key_id: "test".into(),
        };
        let mut signed = serde_json::to_vec(&manifest).expect("manifest");
        signed.extend_from_slice(b"payload");
        let signature = signing.sign(&signed).to_bytes();
        assert!(verifier.verify(&manifest, b"payload", &signature).is_ok());
    }
}

/// Build the full v1 axum router for the workflow control plane.
///
/// The returned `Router` is typed with [`DaemonState`] (the daemon's shared
/// state). Handlers reach the workflow per-router state via
/// `state.workflow_api_state`, and the daemon's [`crate::api_router`]
/// mounts the router through the same `merge` seam as every other surface.
///
/// The router exposes:
///
/// * `GET    /workflows`
/// * `GET    /workflows/{id}`
/// * `GET    /runs`
/// * `GET    /runs/{id}`
/// * `POST   /runs/{id}/cancel`
/// * `GET    /openapi.json`
///
/// This is the single seam the daemon uses to mount the surface into the
/// main router; the handler bodies live in [`crate::api_workflows::routes`].
pub fn build_router() -> Router<DaemonState> {
    crate::api_workflows::routes::router()
}
