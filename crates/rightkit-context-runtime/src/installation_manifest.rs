//! Runtime binding for the installation manifest. Publishes the resident
//! manifest that diagnostics and the bundle report.

use crate::installation_identity::{InstallationIdentity, StartupClaim};
use crate::release_identity;
use crate::time;
use rightkit_context_protocol::{
    canonical_data_root, ApiSchemaV1, ComponentV1, InstallationManifestV1,
};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

/// Process-wide handle to the active manifest. Set once at resident startup.
static ACTIVE_MANIFEST: OnceLock<InstallationManifestV1> = OnceLock::new();

/// Canonical API schemas bound into the manifest. Each entry pins an
/// independently-versioned schema, so a future bumping of one contract does not
/// force a release-wide manifest rewrite.
pub fn active_api_schemas() -> BTreeMap<String, ApiSchemaV1> {
    let mut schemas = BTreeMap::new();
    for (name, version, payload) in [
        (
            "ContextCandidateSetV1",
            1u32,
            include_bytes!("../data/schemas/context-candidate-set.v1.schema.json").as_slice(),
        ),
        (
            "ContextPacketV1",
            1,
            include_bytes!("../data/schemas/context-packet.v1.schema.json").as_slice(),
        ),
        (
            "ContextReceiptV1",
            1,
            include_bytes!("../data/schemas/context-receipt.v1.schema.json").as_slice(),
        ),
        (
            "KnowledgeEmissionV1",
            1,
            include_bytes!("../data/schemas/knowledge-emission.v1.schema.json").as_slice(),
        ),
        (
            "ScopeGrantV1",
            1,
            include_bytes!("../data/schemas/scope-grant.v1.schema.json").as_slice(),
        ),
    ] {
        let digest_sha256 = crate::digest::digest_bytes(payload);
        schemas.insert(
            name.to_string(),
            ApiSchemaV1 {
                version,
                digest_sha256,
            },
        );
    }
    schemas
}

/// Components the resident publishes today. Each entry pins the crate name and
/// its in-process build descriptor. The release-digest field carries the
/// resident's build identity.
pub fn active_components() -> BTreeMap<String, ComponentV1> {
    let mut components = BTreeMap::new();
    for (name, version) in [
        ("membrane-protocol", "0.1.0"),
        ("membrane-runtime", "0.1.0"),
    ] {
        components.insert(
            name.to_string(),
            ComponentV1 {
                name: name.to_string(),
                version: version.to_string(),
                digest_sha256: crate::digest::digest_bytes(name.as_bytes()),
            },
        );
    }
    components
}

/// Build the active manifest from the resident identity, claim, and the
/// workspace data root. The runtime calls this once during startup and feeds
/// the result to [`publish_active_manifest`].
pub fn build_active_manifest(
    identity: &InstallationIdentity,
    claim: &StartupClaim,
    data_root: &Path,
) -> InstallationManifestV1 {
    let canonical_root = canonical_data_root(data_root);
    InstallationManifestV1::new(
        identity.installation_id.clone(),
        claim.startup_generation,
        Path::new(&canonical_root),
        release_identity::release_generation(),
        release_identity::target_triple(),
        time::now_iso(),
        active_api_schemas(),
        active_components(),
    )
}

/// Publish the active manifest for this process. The first call wins; later
/// calls are no-ops so a misbehaving caller cannot change the active manifest
/// mid-flight.
pub fn publish_active_manifest(manifest: InstallationManifestV1) -> Result<(), ManifestError> {
    ACTIVE_MANIFEST
        .set(manifest)
        .map_err(|_| ManifestError::AlreadyPublished)
}

/// Read the active manifest, if any.
pub fn active_manifest() -> Option<&'static InstallationManifestV1> {
    ACTIVE_MANIFEST.get()
}

/// Read the active manifest alongside its canonical JSON form, the most useful
/// debugging surface because it is the exact bytes the manifest digests cover.
pub fn debug_active_manifest() -> Option<(InstallationManifestV1, String)> {
    let manifest = active_manifest()?.clone();
    let canonical = rightkit_context_protocol::canonical_json_of(&manifest);
    Some((manifest, canonical))
}

/// Errors while publishing or reading the active manifest.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ManifestError {
    /// A manifest was already published for this process. The runtime never
    /// publishes more than one; a second attempt indicates a logic bug.
    #[error("active installation manifest was already published for this process")]
    AlreadyPublished,
}
