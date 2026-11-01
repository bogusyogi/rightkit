use crate::references::ReferenceMeta;
use crate::refs::OutputRef;
use crate::tool::ToolCall;
use crate::trust::TrustEnvelope;
use serde::{Deserialize, Serialize};

/// A transcript entry. Discriminated union over the conversation roles plus the compaction boundary
/// marker. Reasoning content is a normalized channel on `Assistant` — never folded into ordinary
/// `content`, because per-provider re-feed rules differ (DeepSeek forbids re-feeding CoT; Kimi
/// requires echoing it in tool-call turns).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "$kind")]
pub enum TranscriptMessage {
    #[serde(rename = "system", rename_all = "camelCase")]
    System {
        id: String,
        created_at: String,
        content: String,
        pinned: bool,
    },

    #[serde(rename = "user", rename_all = "camelCase")]
    User {
        id: String,
        created_at: String,
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trust: Option<TrustEnvelope>,
        pinned: bool,
        /// D2 — typed, persisted references attached to this user turn.
        /// Metadata-only on the wire (no image bytes — those continue to
        /// travel through the legacy `Attachment` channel). `serde-default`
        /// + skip-when-empty keeps every pre-D2 transcript row wire-
        ///   compatible: rows written before D2 deserialize into an empty
        ///   vec, rows written after D2 skip the key when no references are
        ///   attached (so an empty user turn still serializes to the legacy
        ///   5-field shape).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<ReferenceMeta>,
    },

    #[serde(rename = "assistant", rename_all = "camelCase")]
    Assistant {
        id: String,
        created_at: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning_content: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
        pinned: bool,
    },

    /// The result of a tool call. `summary` is the compact transcript line; the full (possibly
    /// large) payload is referenced by `output` and kept out of context.
    #[serde(rename = "tool", rename_all = "camelCase")]
    Tool {
        id: String,
        created_at: String,
        tool_call_id: String,
        summary: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        output: Option<OutputRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trust: Option<TrustEnvelope>,
        is_error: bool,
        pinned: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        process_id: Option<String>,
    },

    /// A compaction/tombstone boundary (G7). Marks where older turns were summarized and dropped.
    /// Visible in the transcript — compaction is never silent.
    #[serde(rename = "compact", rename_all = "camelCase")]
    Compact {
        id: String,
        created_at: String,
        summary: String,
        dropped_count: i32,
        pinned: bool,
    },

    /// A context-fold boundary. Marks where a subtask's sub-trajectory was folded into
    /// a summary. The full sub-trajectory is archived to disk and retrievable via `archive_ref`.
    /// Structural (per-subtask), complementary to the 70% emergency compactor.
    #[serde(rename = "fold", rename_all = "camelCase")]
    Fold {
        id: String,
        created_at: String,
        subtask_id: String,
        summary: String,
        dropped_count: i32,
        /// Reference to the archived sub-trajectory on disk.
        archive_ref: String,
        pinned: bool,
    },
}

impl TranscriptMessage {
    /// Rewrite the message id. Used at the storage boundary to migrate legacy
    /// constant IDs (`tr`, `final`, etc.) to the row-derived canonical form
    /// and by the live engine when a re-emitted assistant completion
    /// disagrees with the persisted row sequence.
    pub fn set_id(&mut self, value: String) {
        match self {
            Self::System { id, .. }
            | Self::User { id, .. }
            | Self::Assistant { id, .. }
            | Self::Tool { id, .. }
            | Self::Compact { id, .. }
            | Self::Fold { id, .. } => *id = value,
        }
    }

    /// Read the message id without disturbing the variant.
    pub fn id(&self) -> &str {
        match self {
            Self::System { id, .. }
            | Self::User { id, .. }
            | Self::Assistant { id, .. }
            | Self::Tool { id, .. }
            | Self::Compact { id, .. }
            | Self::Fold { id, .. } => id.as_str(),
        }
    }

    /// Created-at timestamp; same regardless of variant.
    pub fn created_at(&self) -> &str {
        match self {
            Self::System { created_at, .. }
            | Self::User { created_at, .. }
            | Self::Assistant { created_at, .. }
            | Self::Tool { created_at, .. }
            | Self::Compact { created_at, .. }
            | Self::Fold { created_at, .. } => created_at.as_str(),
        }
    }

    /// Canonical constructor for a `User` transcript row. Defaults
    /// `attachments` to empty so the many call sites that don't carry
    /// references don't have to spell out the field. Callers that DO
    /// attach references should use [`TranscriptMessage::user_with_attachments`].
    pub fn user(
        id: impl Into<String>,
        created_at: impl Into<String>,
        content: impl Into<String>,
        trust: Option<TrustEnvelope>,
        pinned: bool,
    ) -> Self {
        Self::User {
            id: id.into(),
            created_at: created_at.into(),
            content: content.into(),
            trust,
            pinned,
            attachments: Vec::new(),
        }
    }

    /// Canonical constructor for a `User` transcript row that carries typed
    /// D2 references. Use this at the canonical write path (`transcript.rs`)
    /// and the message-submission entry point (`api_sessions.rs`); all other
    /// call sites should use [`TranscriptMessage::user`].
    pub fn user_with_attachments(
        id: impl Into<String>,
        created_at: impl Into<String>,
        content: impl Into<String>,
        trust: Option<TrustEnvelope>,
        pinned: bool,
        attachments: Vec<ReferenceMeta>,
    ) -> Self {
        Self::User {
            id: id.into(),
            created_at: created_at.into(),
            content: content.into(),
            trust,
            pinned,
            attachments,
        }
    }

    pub fn is_provider_visible(&self) -> bool {
        match self {
            Self::Tool { output, .. } => output
                .as_ref()
                .map(|output| output.kind != crate::refs::TOOL_OUTPUT_INTERNAL_KIND)
                .unwrap_or(true),
            _ => true,
        }
    }

    /// Borrow the user-row attachments (empty vec for any non-user variant).
    /// Used by the persistence layer to round-trip references across
    /// write / read boundaries without coupling to `..` destructure shape.
    pub fn user_attachments(&self) -> &[ReferenceMeta] {
        match self {
            Self::User { attachments, .. } => attachments.as_slice(),
            _ => &[],
        }
    }

    /// Owned copy of the user-row attachments (empty vec for any non-user
    /// variant). Useful at submission time when the row needs to forward
    /// attachments to a downstream consumer.
    pub fn into_user_attachments(self) -> Vec<ReferenceMeta> {
        match self {
            Self::User { attachments, .. } => attachments,
            _ => Vec::new(),
        }
    }
}
