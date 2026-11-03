use serde::{Deserialize, Serialize};

/// Persisted (metadata-only) reference attached to a user turn. Mirrors the
/// TypeScript `ReferenceMeta` shape used by the desktop composer. **No image
/// bytes live here** — the desktop keeps the bytes in-memory only; this
/// record is the transcript-visible card (filename, size, kind, trust) that
/// the UI persists under `cr.references.v1` and re-attaches on restart.
///
/// Wire compatibility: all fields use `#[serde(default)]` and
/// `skip_serializing_if` so legacy rows (pre-references) deserialize cleanly.
/// ReferenceMeta is exposed as a standalone type — the desktop owns the
/// transcript mutation surface in the user-message variant, and Agent 4's
/// integration pass binds this type into the `PostMessage` body when the
/// composer forwards references to the engine.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceMeta {
    /// Stable client-generated id (uuid v4 on the desktop). Reused by the
    /// transcript's chip strip and the inline resolver; never recast on
    /// restart so the chip identity survives a page reload.
    pub id: String,
    /// `"external_link" | "workspace_file" | "generated_document" | "image"
    ///  | "artifact" | "change_set"`. Free-form on the wire so a future
    /// kind can be introduced without breaking older clients; the desktop
    /// renders unknown kinds as a generic chip.
    pub kind: String,
    /// Display target: URL, workspace-relative path, doc id, artifact id, or
    /// change-set id. Used by `openExternal` / `revealInFolder` / file
    /// resolver.
    pub destination: String,
    /// Optional MIME hint — `"image/png"`, `"text/markdown"`,
    /// `"application/octet-stream"`, etc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    /// Display filename shown on the chip when `kind` is `image`,
    /// `workspace_file`, or `generated_document`. Optional for
    /// `external_link` and `change_set` where the destination is the label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// Best-effort byte count for the original artifact. `None` for
    /// `external_link` and `change_set` where the size is not knowable up
    /// front.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<i64>,
    /// Provenance + trust. `source` is the producer string
    /// (`"composer.paste"`, `"workspace_resolver"`, etc.), `createdAt` is the
    /// ISO timestamp the user added the reference, `trust` is the
    /// `ContentTrust` label, `contentHash` (when known) is the SHA-256 of the
    /// original bytes — present for `image` / `workspace_file` /
    /// `generated_document` once the desktop has read the file.
    pub provenance: ReferenceProvenance,
    /// Action affordances the chip exposes. Free-form on the wire; the
    /// desktop renders the intersection with its known action set so a
    /// future `download` action sent from a newer sender doesn't break an
    /// older renderer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceProvenance {
    pub source: String,
    pub created_at: String,
    pub trust: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
}

impl ReferenceMeta {
    /// Wire-friendly default: an empty record with no actions / no MIME / no
    /// size. Useful for tests that need a placeholder.
    pub fn empty(id: &str, kind: &str, destination: &str) -> Self {
        Self {
            id: id.to_string(),
            kind: kind.to_string(),
            destination: destination.to_string(),
            mime: None,
            filename: None,
            size_bytes: None,
            provenance: ReferenceProvenance {
                source: "composer".to_string(),
                created_at: "1970-01-01T00:00:00Z".to_string(),
                trust: "user".to_string(),
                content_hash: None,
            },
            actions: Vec::new(),
        }
    }
}
