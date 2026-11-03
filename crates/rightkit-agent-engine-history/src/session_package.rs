//! Cross-host session package format (ENG-101).
//!
//! A "session package" is a versioned, self-describing bundle capturing
//! everything needed to resume a session on a different host or execution
//! target: the durable journal transcript, its replay cursor, the workspace
//! identity the session was bound to, and target metadata (see
//! `host_tools::cloud_endpoint::ExecutionTarget`, not duplicated here —
//! this crate does not depend on `host_tools`, so the target is carried
//! as its `as_str()` string form to avoid a new taxonomy).
//!
//! # Credential safety
//!
//! [`SessionPackage`] can only ever be built from `JournalEvent` (from the host mobile runtime journal)
//! records, which already exclude raw secrets by construction (see that
//! module's doc comment). This module additionally never adds a field wide
//! enough to carry a credential — no free-form maps, no provider tokens, no
//! bearer values. A reviewer only needs to confirm this file has no such
//! field to know an exported package cannot leak a secret.
//!
//! # Versioning
//!
//! [`SESSION_PACKAGE_FORMAT_VERSION`] is bumped only for a
//! backward-incompatible schema change. [`SessionPackage::decode`] refuses to
//! import a package whose `schema_version` is newer than this crate
//! understands, rather than guessing at an unknown shape.

use serde::{Deserialize, Serialize};

/// Current schema version this crate writes and the newest it can read.
/// Bump only on a backward-incompatible change to [`SessionPackage`]'s
/// fields.
pub const SESSION_PACKAGE_FORMAT_VERSION: u32 = 1;

/// One journal record as carried in a package. Mirrors
/// `host_mobile_runtime::journal::JournalEvent` field-for-field; this
/// crate does not depend on `mobile_runtime`, so the shape is duplicated
/// rather than imported. Keep the two in sync by hand if the journal record
/// shape changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackagedJournalEvent {
    pub sequence: u64,
    pub nonce: String,
    /// Opaque, pre-serialized `JournalEventKind` JSON. Kept as a raw value
    /// (rather than re-declaring the enum here) so this crate has nothing
    /// new to keep credential-safe: whatever redaction the journal already
    /// applied is preserved verbatim, and this module cannot widen it.
    pub kind: serde_json::Value,
}

/// Workspace identity a packaged session was bound to. Enough to let the
/// importing host verify (or prompt to re-bind) the workspace before
/// resuming — never a credential, never an absolute host-specific path
/// beyond what the source host already exposed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceIdentity {
    /// Human-readable workspace name/label, as shown in host UI.
    pub name: String,
    /// Best-effort stable identifier for the workspace (e.g. a git remote
    /// URL or repository slug) used to detect an obvious mismatch on
    /// import. Not a filesystem path: paths are host-specific and carry no
    /// portable meaning across a handoff.
    pub identity_key: Option<String>,
}

/// Target metadata describing where a session was running before export.
/// `target` is the string form of `ExecutionTarget::as_str()` from
/// `host_tools`; kept as a string here (rather than importing that
/// enum) to avoid a `history -> tools` dependency and a second target
/// taxonomy — see module doc comment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetMetadata {
    /// One of `"local"`, `"cloud"`, `"computer"` (mirrors
    /// `ExecutionTarget::as_str()`). Not validated against a closed set here
    /// so this crate does not need to track every target the tools crate
    /// ever adds; an unrecognized value is preserved and surfaced to the
    /// caller rather than rejected.
    pub target: String,
    /// Opaque host label (hostname, daemon id, etc.) for display only.
    pub host_label: Option<String>,
}

/// A versioned, self-describing session package.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPackage {
    /// Schema version this package was written under. Checked by
    /// [`SessionPackage::decode`] before any other field is trusted.
    pub schema_version: u32,
    pub session_id: String,
    pub workspace: WorkspaceIdentity,
    pub source_target: TargetMetadata,
    /// Full transcript as journal records, in ascending `sequence` order.
    pub events: Vec<PackagedJournalEvent>,
    /// Replay cursor at export time: the sequence number the importing host
    /// should resume replay *after*. Equal to the last event's `sequence`
    /// for a fully-flushed export.
    pub cursor: u64,
    /// Milliseconds since Unix epoch when this package was produced, for
    /// display and staleness checks. Not used for ordering — `cursor` and
    /// `sequence` own that.
    pub exported_at_ms: u64,
}

#[derive(Debug)]
pub enum SessionPackageError {
    Serialize(serde_json::Error),
    /// The package's `schema_version` is newer than
    /// [`SESSION_PACKAGE_FORMAT_VERSION`]; this crate does not know how to
    /// interpret it and refuses to guess.
    UnsupportedSchemaVersion {
        found: u32,
        max_supported: u32,
    },
    /// `events` is not sorted by ascending, contiguous-from-source
    /// `sequence`, or contains a `sequence` greater than `cursor`.
    InconsistentCursor,
}

impl std::fmt::Display for SessionPackageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Serialize(error) => write!(f, "session package serialize error: {error}"),
            Self::UnsupportedSchemaVersion {
                found,
                max_supported,
            } => write!(
                f,
                "session package schema version {found} is newer than the highest \
                 supported version {max_supported}; refusing to import"
            ),
            Self::InconsistentCursor => write!(
                f,
                "session package events are not consistent with its cursor"
            ),
        }
    }
}

impl std::error::Error for SessionPackageError {}

impl From<serde_json::Error> for SessionPackageError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

impl SessionPackage {
    /// Build a package from the caller's already-collected journal replay,
    /// stamping the current [`SESSION_PACKAGE_FORMAT_VERSION`].
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: String,
        workspace: WorkspaceIdentity,
        source_target: TargetMetadata,
        events: Vec<PackagedJournalEvent>,
        cursor: u64,
        exported_at_ms: u64,
    ) -> Self {
        Self {
            schema_version: SESSION_PACKAGE_FORMAT_VERSION,
            session_id,
            workspace,
            source_target,
            events,
            cursor,
            exported_at_ms,
        }
    }

    /// Serialize to a portable byte form (pretty JSON) for export/transfer.
    pub fn encode(&self) -> Result<Vec<u8>, SessionPackageError> {
        Ok(serde_json::to_vec_pretty(self)?)
    }

    /// Parse and validate a package. Refuses:
    /// - a `schema_version` newer than this crate understands
    ///   ([`SessionPackageError::UnsupportedSchemaVersion`]);
    /// - an events/cursor combination that could silently drop or replay
    ///   the transcript incorrectly on import
    ///   ([`SessionPackageError::InconsistentCursor`]).
    ///
    /// This is the sole entry point for turning bytes from another host
    /// into a trusted [`SessionPackage`]; callers must not
    /// `serde_json::from_slice` a package directly.
    pub fn decode(bytes: &[u8]) -> Result<Self, SessionPackageError> {
        // Peek the schema version first via a minimal shape, so a future,
        // wider schema_version+1 payload that would otherwise fail to
        // deserialize into today's struct still reports the precise
        // "too new" error instead of a generic parse failure.
        // Must carry the same rename rule as `SessionPackage`: the wire field
        // is `schemaVersion`, so a probe without this attribute fails to
        // deserialize every well-formed package and turns the precise version
        // check into a generic parse error.
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct VersionProbe {
            schema_version: u32,
        }
        let probe: VersionProbe = serde_json::from_slice(bytes)?;
        if probe.schema_version > SESSION_PACKAGE_FORMAT_VERSION {
            return Err(SessionPackageError::UnsupportedSchemaVersion {
                found: probe.schema_version,
                max_supported: SESSION_PACKAGE_FORMAT_VERSION,
            });
        }

        let package: SessionPackage = serde_json::from_slice(bytes)?;

        let mut last_sequence = 0u64;
        for event in &package.events {
            if event.sequence <= last_sequence && last_sequence != 0 {
                return Err(SessionPackageError::InconsistentCursor);
            }
            if event.sequence == 0 {
                return Err(SessionPackageError::InconsistentCursor);
            }
            last_sequence = event.sequence;
        }
        if let Some(last) = package.events.last() {
            if last.sequence > package.cursor {
                return Err(SessionPackageError::InconsistentCursor);
            }
        }

        Ok(package)
    }
}
