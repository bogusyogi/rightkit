//! Validated import of an untrusted peer-team manifest.
//!
//! The importer accepts one strict JSON shape. It parses and validates the
//! complete document before exposing a [`TeamManifest`] to delegation code;
//! malformed values are rejected rather than trimmed, defaulted, or coerced.
//!
//! External-artifact gap: this module validates local manifest data only. It
//! does not fetch manifests from a remote registry or claim that a peer
//! endpoint is reachable.

use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_IDENTIFIER_BYTES: usize = 128;
const MAX_ROLE_BYTES: usize = 128;
const MAX_PEERS: usize = 64;
const MAX_DELEGATION_DEPTH: u8 = 32;

/// The only accepted schema version, so future manifest shapes cannot be
/// silently interpreted as this one.
pub const TEAM_MANIFEST_VERSION: u32 = 1;

/// A typed, fully validated peer-team manifest suitable for delegation.
///
/// Instances can only be created through [`import_manifest`], keeping raw
/// untrusted JSON out of the approval and delegation paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TeamManifest {
    version: u32,
    team_id: String,
    max_delegation_depth: u8,
    peers: Vec<PeerManifest>,
}

/// One named peer admitted by a validated [`TeamManifest`], giving approval
/// matching a stable identity instead of an unchecked manifest string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PeerManifest {
    peer_id: String,
    role: String,
}

/// Typed failures returned when an untrusted manifest is not admissible.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ManifestImportError {
    /// The document is not valid JSON or does not match the strict field
    /// types declared by the manifest schema.
    #[error("malformed team manifest JSON: {message}")]
    MalformedJson { message: String },
    /// The document used a schema version this importer does not understand.
    #[error("unsupported team manifest version {version}")]
    UnsupportedVersion { version: u32 },
    /// A required identifier or role was empty, oversized, or contained
    /// whitespace/control/path characters that make it unsafe as a peer id.
    #[error("invalid {field} `{value}`")]
    InvalidValue { field: String, value: String },
    /// The manifest contained too many peers for the bounded fleet model.
    #[error("team manifest contains {count} peers; maximum is {maximum}")]
    TooManyPeers { count: usize, maximum: usize },
    /// Peer ids must be unique so approval cannot alias two fleet members.
    #[error("duplicate peer id `{peer_id}`")]
    DuplicatePeer { peer_id: String },
    /// A zero depth ceiling would make the imported fleet unusable.
    #[error("team manifest delegation depth must be at least one")]
    ZeroDelegationDepth,
    /// A depth ceiling above the local bound could permit unbounded chaining.
    #[error("team manifest delegation depth {depth} exceeds maximum {maximum}")]
    DelegationDepthTooLarge { depth: u8, maximum: u8 },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UntrustedTeamManifest {
    version: u32,
    team_id: String,
    max_delegation_depth: u8,
    peers: Vec<UntrustedPeerManifest>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UntrustedPeerManifest {
    peer_id: String,
    role: String,
}

/// Parse and validate a complete JSON team manifest before returning any
/// manifest fields to callers.
pub fn import_manifest(raw: &str) -> Result<TeamManifest, ManifestImportError> {
    let candidate: UntrustedTeamManifest =
        serde_json::from_str(raw).map_err(|error| ManifestImportError::MalformedJson {
            message: error.to_string(),
        })?;

    validate_identifier("team_id", &candidate.team_id)?;
    if candidate.version != TEAM_MANIFEST_VERSION {
        return Err(ManifestImportError::UnsupportedVersion {
            version: candidate.version,
        });
    }
    if candidate.max_delegation_depth == 0 {
        return Err(ManifestImportError::ZeroDelegationDepth);
    }
    if candidate.max_delegation_depth > MAX_DELEGATION_DEPTH {
        return Err(ManifestImportError::DelegationDepthTooLarge {
            depth: candidate.max_delegation_depth,
            maximum: MAX_DELEGATION_DEPTH,
        });
    }
    if candidate.peers.is_empty() {
        return Err(ManifestImportError::InvalidValue {
            field: "peers".to_string(),
            value: "empty list".to_string(),
        });
    }
    if candidate.peers.len() > MAX_PEERS {
        return Err(ManifestImportError::TooManyPeers {
            count: candidate.peers.len(),
            maximum: MAX_PEERS,
        });
    }

    let mut peer_ids = std::collections::BTreeSet::new();
    let mut peers = Vec::with_capacity(candidate.peers.len());
    for peer in candidate.peers {
        validate_identifier("peer_id", &peer.peer_id)?;
        validate_role(&peer.role)?;
        if !peer_ids.insert(peer.peer_id.clone()) {
            return Err(ManifestImportError::DuplicatePeer {
                peer_id: peer.peer_id,
            });
        }
        peers.push(PeerManifest {
            peer_id: peer.peer_id,
            role: peer.role,
        });
    }

    Ok(TeamManifest {
        version: candidate.version,
        team_id: candidate.team_id,
        max_delegation_depth: candidate.max_delegation_depth,
        peers,
    })
}

impl TeamManifest {
    /// Return the schema version after the manifest has passed validation.
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Return the stable team id used to bind approvals and delegations.
    pub fn team_id(&self) -> &str {
        &self.team_id
    }

    /// Return the maximum allowed peer-to-peer delegation depth.
    pub fn max_delegation_depth(&self) -> u8 {
        self.max_delegation_depth
    }

    /// Return all admitted peers in manifest order for fleet display.
    pub fn peers(&self) -> &[PeerManifest] {
        &self.peers
    }

    /// Find a peer by its validated stable id without accepting a new id.
    pub(crate) fn peer(&self, peer_id: &str) -> Option<&PeerManifest> {
        self.peers.iter().find(|peer| peer.peer_id == peer_id)
    }
}

impl PeerManifest {
    /// Return the stable peer id used by approval matching.
    pub fn peer_id(&self) -> &str {
        &self.peer_id
    }

    /// Return the role imported for this peer.
    pub fn role(&self) -> &str {
        &self.role
    }
}

fn validate_identifier(field: &str, value: &str) -> Result<(), ManifestImportError> {
    if value.is_empty()
        || value.len() > MAX_IDENTIFIER_BYTES
        || value.trim() != value
        || value
            .chars()
            .any(|character| character.is_control() || matches!(character, '/' | '\\'))
    {
        return Err(ManifestImportError::InvalidValue {
            field: field.to_string(),
            value: value.to_string(),
        });
    }
    Ok(())
}

fn validate_role(value: &str) -> Result<(), ManifestImportError> {
    if value.is_empty()
        || value.len() > MAX_ROLE_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(ManifestImportError::InvalidValue {
            field: "role".to_string(),
            value: value.to_string(),
        });
    }
    Ok(())
}
